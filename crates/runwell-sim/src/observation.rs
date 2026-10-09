use crate::{Config, Diagnostics, Error, config::Demand};
use runwell_trace::TraceJob;
use std::collections::BTreeMap;

pub(crate) struct Observation<'a> {
    pub raw: &'a TraceJob,
    pub arrival: f64,
    pub created: f64,
    pub start: f64,
    pub end: f64,
    pub net: f64,
    pub slot_wait: f64,
    pub active: Vec<(f64, f64)>,
    pub semaphore: Option<(f64, f64)>,
    pub concurrency: f64,
    pub local: bool,
    /// Fitted host; every job is on host 0 unless runner patterns attribute it.
    pub host: Option<usize>,
    /// Whether the job starts inside the fit window.
    pub fit: bool,
    pub demand: Demand,
    pub reuse: Option<usize>,
    pub unstarted: bool,
}
fn seconds(t: jiff::Timestamp) -> f64 {
    t.as_millisecond() as f64 / 1000.0
}

pub(crate) fn read<'a>(
    trace: &'a [TraceJob],
    config: &Config,
    diagnostics: &mut Diagnostics,
) -> Result<Vec<Observation<'a>>, Error> {
    let mut observations = Vec::new();
    let gates: Vec<_> = (0..config.gate_count()).map(|g| config.gate(g)).collect();
    let mut first_attempts = BTreeMap::new();
    for raw in trace {
        first_attempts
            .entry(execution_key(raw))
            .and_modify(|attempt: &mut u32| {
                *attempt = (*attempt).min(raw.run_attempt);
            })
            .or_insert(raw.run_attempt);
    }
    for raw in trace {
        let unstarted = raw.conclusion.as_deref() == Some("cancelled")
            && raw.runner_name.as_deref().is_none_or(str::is_empty)
            && raw.steps.is_empty();
        let (Some(arrival), Some(start), Some(end)) = (
            raw.run_created_at,
            raw.started_at.or_else(|| {
                if unstarted {
                    raw.created_at.or(raw.run_created_at)
                } else {
                    None
                }
            }),
            raw.completed_at.or_else(|| {
                if unstarted {
                    raw.created_at.or(raw.run_created_at)
                } else {
                    None
                }
            }),
        ) else {
            diagnostics.excluded_jobs += 1;
            continue;
        };
        let (arrival, mut start, end) = (seconds(arrival), seconds(start), seconds(end));
        let skipped = raw.conclusion.as_deref() == Some("skipped");
        if unstarted {
            start = start.min(end).max(arrival);
        }
        let repeated = first_attempts
            .get(&execution_key(raw))
            .is_some_and(|&attempt| attempt < raw.run_attempt);
        if !skipped && !unstarted && (end < start || (start < arrival && !repeated)) {
            diagnostics.excluded_jobs += 1;
            continue;
        }
        diagnostics.unstarted_cancellations += usize::from(unstarted);
        let created = raw
            .created_at
            .map(seconds)
            .unwrap_or(arrival)
            .max(arrival)
            .min(start.max(arrival));
        let acquires: Vec<_> = raw
            .steps
            .iter()
            .filter(|s| s.conclusion.as_deref() != Some("skipped"))
            .filter_map(|s| {
                let mut pools = gates
                    .iter()
                    .enumerate()
                    .filter(|(_, g)| g.acquire.iter().any(|p| s.name.contains(p)))
                    .map(|(i, _)| i);
                let pool = pools.next()?;
                Some(if pools.next().is_some() {
                    Err(Error::Invalid(
                        "a step matches more than one semaphore pool".into(),
                    ))
                } else {
                    Ok((s, pool))
                })
            })
            .collect::<Result<_, _>>()?;
        let waits: Vec<_> = acquires
            .iter()
            .map(|(s, _)| s)
            .filter_map(|s| {
                Some((
                    seconds(s.started_at?).max(start),
                    seconds(s.completed_at?).min(end),
                ))
            })
            .filter(|(a, b)| b > a)
            .collect();
        let active = active_intervals(start, end, &waits);
        if acquires.len() > 1 {
            return Err(Error::Invalid(
                "multiple semaphore acquisitions per job are unsupported".into(),
            ));
        }
        let semaphore = acquires.first().and_then(|&(s, pool)| {
            let acquire = seconds(s.started_at?).clamp(start, end.max(start));
            let release = raw
                .steps
                .iter()
                .filter(|s| {
                    s.conclusion.as_deref() != Some("skipped")
                        && gates[pool].release.iter().any(|p| s.name.contains(p))
                })
                .filter_map(|s| s.completed_at.map(seconds))
                .filter(|&at| at >= acquire)
                .min_by(f64::total_cmp)
                .unwrap_or(end);
            Some((acquire, release.clamp(acquire, end.max(acquire))))
        });
        let net = active.iter().map(|(a, b)| b - a).sum::<f64>();
        let waits = (end - start - net).max(0.0);
        let local = config
            .contention_label
            .as_ref()
            .is_none_or(|l| raw.labels.contains(l));
        observations.push(Observation {
            raw,
            arrival,
            created,
            start: start.max(arrival),
            end: end.max(start).max(arrival),
            net: if skipped || unstarted {
                0.0
            } else if local {
                (end - start - waits).max(0.0)
            } else {
                (end - start).max(0.0)
            },
            slot_wait: waits.min((end - start).max(0.0)),
            active,
            semaphore,
            concurrency: 0.0,
            local,
            host: Some(0),
            fit: config.in_fit_window(raw.started_at),
            demand: config.demand(&raw.repo, &raw.job_name),
            reuse: None,
            unstarted,
        });
    }
    if observations.is_empty() {
        return Err(Error::Invalid("trace has no usable job timestamps".into()));
    }
    // GitHub rerun snapshots can include earlier successful executions. An exact
    // execution/timestamp match across attempts is reused, never counted twice.
    // GitHub can assign a new job ID to the snapshot of an old execution.
    let mut identities = BTreeMap::new();
    let mut order: Vec<_> = (0..observations.len()).collect();
    order.sort_by_key(|&i| observations[i].raw.run_attempt);
    for i in order {
        let o = &observations[i];
        if o.net <= 0.0 {
            continue;
        }
        let raw = o.raw;
        let key = execution_key(raw);
        if let Some(&(attempt, original)) = identities.get(&key) {
            if attempt < raw.run_attempt {
                observations[i].reuse = Some(original);
                observations[i].net = 0.0;
                diagnostics.reused_executions += 1;
            }
        } else {
            identities.insert(key, (raw.run_attempt, i));
        }
    }
    let origin = observations
        .iter()
        .map(|o| o.arrival)
        .fold(f64::INFINITY, f64::min);
    for o in &mut observations {
        o.arrival -= origin;
        o.created -= origin;
        o.start -= origin;
        o.end -= origin;
        if let Some((a, b)) = &mut o.semaphore {
            *a -= origin;
            *b -= origin;
        }
        for (a, b) in &mut o.active {
            *a -= origin;
            *b -= origin;
        }
    }
    Ok(observations)
}

impl Observation<'_> {
    /// Work before acquisition, inside the slot, and after release. Integrate
    /// each phase against the same speed curve; observed waits are already absent.
    pub fn phase_work(&self, integral: impl Fn(f64, f64) -> f64) -> [f64; 3] {
        if self.net == 0.0 {
            return [0.0; 3];
        }
        let (acquire, release) = self.semaphore.unwrap_or((self.start, self.end));
        let mut work = [0.0; 3];
        for &(a, b) in &self.active {
            for (i, (start, end)) in [
                (a, b.min(acquire)),
                (a.max(acquire), b.min(release)),
                (a.max(release), b),
            ]
            .into_iter()
            .enumerate()
            {
                if end > start {
                    work[i] += integral(start, end).max(0.0);
                }
            }
        }
        work
    }
}

type ExecutionKey<'a> = (
    &'a str,
    u64,
    &'a str,
    Option<jiff::Timestamp>,
    Option<jiff::Timestamp>,
    Option<&'a str>,
);

fn execution_key(raw: &TraceJob) -> ExecutionKey<'_> {
    (
        &raw.repo,
        raw.run_id,
        &raw.job_name,
        raw.started_at,
        raw.completed_at,
        raw.runner_name.as_deref(),
    )
}

fn active_intervals(start: f64, end: f64, waits: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut waits = waits.to_vec();
    waits.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut result = Vec::new();
    let mut cursor = start;
    for (a, b) in waits {
        if a > cursor {
            result.push((cursor, a));
        }
        cursor = cursor.max(b);
    }
    if end > cursor {
        result.push((cursor, end));
    }
    result
}
