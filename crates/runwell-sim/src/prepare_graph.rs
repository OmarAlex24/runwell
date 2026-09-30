use crate::{
    Diagnostics, Error,
    observation::Observation,
    trace::{Job, Run},
};
use std::collections::BTreeMap;

pub(crate) fn wire(
    jobs: &mut [Job],
    runs: &mut [Run],
    observations: &[Observation<'_>],
    diagnostics: &mut Diagnostics,
) -> Result<(), Error> {
    let mut matrix_ids = BTreeMap::new();
    for (r, run) in runs.iter_mut().enumerate() {
        for &i in &run.jobs {
            let o = &observations[i];
            if let Some(original) = o.reuse {
                jobs[i].needs = vec![original];
                jobs[i].work = 0.0;
                continue;
            }
            if o.raw.max_parallel.is_some() {
                let key = (
                    r,
                    o.raw
                        .workflow_job_id
                        .clone()
                        .unwrap_or_else(|| o.raw.job_name.clone()),
                );
                let next = matrix_ids.len();
                jobs[i].matrix_group = Some(*matrix_ids.entry(key).or_insert(next));
            }
            let needs = if let Some(names) = &o.raw.needs {
                diagnostics.workflow_jobs += 1;
                let mut parents = Vec::new();
                for name in names {
                    let matches: Vec<_> = run
                        .jobs
                        .iter()
                        .copied()
                        .filter(|&j| observations[j].raw.job_name == *name)
                        .collect();
                    // Excluded malformed/skipped records may have been named by the
                    // importer. Zero-work skipped ancestors do not need a runner.
                    if matches.is_empty() {
                        return Err(Error::Invalid(
                            "dependency name absent from usable run records".into(),
                        ));
                    }
                    parents.extend(matches);
                }
                parents.sort_unstable();
                parents.dedup();
                parents
            } else {
                run.jobs
                    .iter()
                    .copied()
                    .filter(|&j| {
                        j != i && observations[j].net > 0.0 && observations[j].end <= o.created
                    })
                    .max_by(|&a, &b| {
                        observations[a]
                            .end
                            .total_cmp(&observations[b].end)
                            .then(a.cmp(&b))
                    })
                    .into_iter()
                    .collect()
            };
            let preceding = needs
                .iter()
                .map(|&j| observations[j].end)
                .fold(run.arrival, f64::max);
            jobs[i].delay = if o.unstarted {
                (o.end - preceding).max(0.0)
            } else if let Some(delay) = o.raw.dispatch_delay_seconds {
                delay
            } else if o.raw.needs.is_some() {
                0.0
            } else {
                (o.created - preceding).max(0.0)
            };
            if !jobs[i].delay.is_finite() || jobs[i].delay < 0.0 {
                return Err(Error::Invalid("invalid dispatch delay".into()));
            }
            jobs[i].observed_queue = if o.net > 0.0 {
                (o.start - preceding - jobs[i].delay).max(0.0) + o.slot_wait
            } else {
                0.0
            };
            jobs[i].needs = needs;
        }
        let mut cursor = run.jobs.iter().copied().max_by(|&a, &b| {
            observations[a]
                .end
                .total_cmp(&observations[b].end)
                .then(a.cmp(&b))
        });
        // Graph validation happens next. Bound this walk so malformed explicit
        // cycles produce an error instead of hanging while calculating metrics.
        let mut traversed = 0;
        while let Some(i) = cursor {
            traversed += 1;
            if traversed > jobs.len() {
                return Err(Error::Invalid("cyclic dependencies".into()));
            }
            run.observed_queue += jobs[i].observed_queue;
            cursor = jobs[i].needs.iter().copied().max_by(|&a, &b| {
                observations[a]
                    .end
                    .total_cmp(&observations[b].end)
                    .then(a.cmp(&b))
            });
        }
    }
    Ok(())
}

pub(crate) fn cancellations(
    runs: &mut [Run],
    observations: &[Observation<'_>],
    config: &crate::Config,
    diagnostics: &mut Diagnostics,
) {
    let mut groups = BTreeMap::new();
    let requests: Vec<_> = runs
        .iter()
        .map(|r| {
            let raw = observations[r.jobs[0]].raw;
            let group = raw.cancel_group.as_ref().map(|g| {
                let next = groups.len();
                *groups.entry(g).or_insert(next)
            });
            // Run creation does not order GitHub concurrency admission. First job
            // creation is the available dispatch proxy, also needed on reruns where
            // run_created_at retains the original attempt's creation timestamp.
            let arrival = if config.cancel_on_dispatch || raw.run_attempt > 1 {
                r.jobs
                    .iter()
                    .filter(|&&i| observations[i].reuse.is_none())
                    .map(|&i| observations[i].created)
                    .fold(f64::INFINITY, f64::min)
            } else {
                r.arrival
            };
            runwell_scheduler::RunArrival {
                run_id: raw.run_id,
                group,
                time: if arrival.is_finite() {
                    arrival
                } else {
                    r.arrival
                },
            }
        })
        .collect();
    if config.cancel_in_progress {
        for (r, deadline) in runs
            .iter_mut()
            .zip(runwell_scheduler::cancel_deadlines(&requests))
        {
            r.cancel_at = deadline.map(|t| t + config.cancel_grace_seconds);
            diagnostics.superseded_runs += usize::from(r.cancel_at.is_some());
        }
    }
}
