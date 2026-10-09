use crate::config::Demand;
use crate::{Config, Error, contention::ContentionFit};
use runwell_trace::TraceJob;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub(crate) struct Job {
    pub workflow_job: String,
    pub pull_request: String,
    pub run: usize,
    pub repo: usize,
    pub pool: usize,
    pub demand: Demand,
    pub local: bool,
    pub needs: Vec<usize>,
    pub delay: f64,
    pub work: f64,
    pub semaphore_work: [f64; 3],
    pub path: f64,
    pub external_queue: f64,
    pub matrix_group: Option<usize>,
    pub max_parallel: Option<usize>,
    pub observed_queue: f64,
    pub class: usize,
    /// Semaphore pool gating this job in baseline replay.
    pub gate: Option<usize>,
    pub observed_semaphore: Option<bool>,
}
#[derive(Debug, Clone)]
pub(crate) struct Run {
    pub repo: usize,
    pub event: String,
    pub arrival: f64,
    pub observed: f64,
    pub observed_queue: f64,
    pub cancel_at: Option<f64>,
    pub report: bool,
    pub jobs: Vec<usize>,
}
/// Evidence quality and exclusions; these travel with every report.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Diagnostics {
    /// Input records, including skipped jobs.
    pub input_jobs: usize,
    /// Records missing usable run/start/end timestamps.
    pub excluded_jobs: usize,
    /// Cancellation records with neither a runner nor steps, treated as zero work.
    pub unstarted_cancellations: usize,
    /// Identical executions repeated across attempt snapshots, reused without work.
    pub reused_executions: usize,
    /// Jobs executed outside the modeled host fleet.
    pub external_jobs: usize,
    /// Positive-work jobs lacking explicit dependency metadata.
    pub inferred_jobs: usize,
    /// Positive-work jobs without a successful low-concurrency name sample.
    pub unsupported_intrinsic_jobs: usize,
    /// Successful first-attempt PR runs used for calibration.
    pub calibration_runs: usize,
    /// Total modeled positive-work local jobs.
    pub local_jobs: usize,
    /// Jobs using explicit workflow metadata.
    pub workflow_jobs: usize,
    /// Runs superseded by a later arrival in their cancellation group.
    pub superseded_runs: usize,
    /// Modeling caveats, without private identifiers.
    pub warnings: Vec<String>,
}
/// A validated trace with fitted work estimates and an indexed dependency graph.
#[derive(Debug, Clone)]
pub struct PreparedTrace {
    pub(crate) config: Config,
    pub(crate) jobs: Vec<Job>,
    pub(crate) runs: Vec<Run>,
    pub(crate) repos: Vec<String>,
    pub(crate) pool_limits: Vec<usize>,
    pub(crate) pool_repos: Vec<usize>,
    pub(crate) availability: Vec<crate::availability::Change>,
    pub(crate) runner_history: Vec<(f64, usize, usize, usize)>,
    /// Earliest run creation, in epoch seconds; replay times are relative to it.
    pub(crate) origin: f64,
    /// Fitted contention and heavy failure proxies.
    pub fit: ContentionFit,
    /// Independently fitted slowdown and inferred CPU demand by job class.
    pub classes: Vec<crate::ClassModel>,
    /// Input quality and cohort diagnostics.
    pub diagnostics: Diagnostics,
}
impl PreparedTrace {
    /// Prepare once for all scenarios. Unknown DAGs are inferred from job creation
    /// and the latest preceding completion within the same run attempt.
    pub fn new(trace: &[TraceJob], config: &Config) -> Result<Self, Error> {
        config.validate()?;
        let mut diagnostics = Diagnostics {
            input_jobs: trace.len(),
            ..Diagnostics::default()
        };
        let mut observations = crate::observation::read(trace, config, &mut diagnostics)?;
        let model = crate::model::fit(&mut observations, config);
        let intrinsic = if config.preserve_work_variation && !config.observed_work {
            crate::intrinsic::estimate(&observations, &model, config)
        } else {
            Vec::new()
        };
        let repo_names: Vec<_> = observations
            .iter()
            .map(|o| o.raw.repo.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut pool_limits: Vec<_> = config.pools.iter().map(|p| p.runners).collect();
        let default_pool_start = pool_limits.len();
        pool_limits.extend(vec![6; repo_names.len()]);
        let mut latest_attempt = BTreeMap::new();
        for j in trace {
            latest_attempt
                .entry((&j.repo, j.run_id))
                .and_modify(|a: &mut u32| *a = (*a).max(j.run_attempt))
                .or_insert(j.run_attempt);
        }
        let mut run_index = BTreeMap::new();
        let mut runs: Vec<Run> = Vec::new();
        let mut jobs = Vec::new();
        for (i, o) in observations.iter().enumerate() {
            let repo = repo_names
                .binary_search(&o.raw.repo)
                .map_err(|_| Error::Invalid("missing repo index".into()))?;
            let key = (repo, o.raw.run_id, o.raw.run_attempt);
            let run = *run_index.entry(key).or_insert_with(|| {
                let id = runs.len();
                runs.push(Run {
                    repo,
                    event: o.raw.event.clone().unwrap_or_else(|| "unknown".into()),
                    arrival: o.arrival,
                    observed: 0.0,
                    observed_queue: 0.0,
                    cancel_at: None,
                    report: (config.report_workflows.is_empty()
                        || config.report_workflows.iter().any(|w| {
                            w.repo == o.raw.repo && Some(&*w.name) == o.raw.workflow.as_deref()
                        }))
                        && (!config.successful_first_attempt
                            || (latest_attempt.get(&(&o.raw.repo, o.raw.run_id)) == Some(&1)
                                && o.raw.run_conclusion.as_deref() == Some("success"))),
                    jobs: Vec::new(),
                });
                id
            });
            runs[run].observed = runs[run].observed.max(o.end - runs[run].arrival);
            runs[run].jobs.push(i);
            let pool = config
                .pools
                .iter()
                .position(|p| {
                    p.repo == o.raw.repo
                        && p.labels.iter().all(|l| o.raw.labels.contains(l))
                        && (p.job_names.is_empty() || p.job_names.contains(&o.raw.job_name))
                })
                .unwrap_or(default_pool_start + repo);
            let median = model
                .medians
                .get(&(o.raw.repo.clone(), o.raw.job_name.clone()));
            let work = if config.preserve_work_variation && !config.observed_work {
                intrinsic[i].iter().sum()
            } else if config.observed_work
                || o.net == 0.0
                || !o.local
                || o.concurrency <= config.low_concurrency
                || o.raw.conclusion.as_deref() != Some("success")
            {
                o.net
            } else {
                median.copied().unwrap_or(o.net)
            };
            let semaphore_work = if !o.local {
                [0.0, work, 0.0]
            } else if config.preserve_work_variation && !config.observed_work {
                intrinsic[i]
            } else {
                o.phase_work(|a, b| (b - a) * work / o.net)
            };
            let gate = config.gate_of(&o.demand);
            let acquire = config
                .gate(gate.unwrap_or(crate::config::HEAVY_GATE))
                .acquire;
            if o.local && o.net > 0.0 {
                diagnostics.local_jobs += 1;
                diagnostics.unsupported_intrinsic_jobs += usize::from(median.is_none());
                diagnostics.inferred_jobs += usize::from(o.raw.needs.is_none());
            } else if !o.local && o.net > 0.0 {
                diagnostics.external_jobs += 1;
            }
            jobs.push(Job {
                workflow_job: format!(
                    "{}/{}",
                    o.raw.workflow.as_deref().unwrap_or("unknown"),
                    o.raw.workflow_job_id.as_deref().unwrap_or(&o.raw.job_name)
                ),
                pull_request: o
                    .raw
                    .branch
                    .clone()
                    .unwrap_or_else(|| format!("run-{}", o.raw.run_id)),
                run,
                repo,
                pool,
                demand: o.demand.clone(),
                local: o.local,
                needs: Vec::new(),
                delay: 0.0,
                matrix_group: None,
                max_parallel: o.raw.max_parallel,
                observed_queue: 0.0,
                class: model.class_ids[i],
                gate,
                observed_semaphore: if o.raw.steps.is_empty() || acquire.is_empty() {
                    None
                } else {
                    Some(o.raw.steps.iter().any(|s| {
                        s.conclusion.as_deref() != Some("skipped")
                            && acquire.iter().any(|p| s.name.contains(p))
                    }))
                },
                work,
                semaphore_work,
                path: 0.0,
                external_queue: if o.local || o.net == 0.0 {
                    0.0
                } else {
                    o.start - o.created
                },
            });
        }
        let used: std::collections::BTreeSet<_> = jobs
            .iter()
            .filter(|j| j.local && j.work > 0.0)
            .map(|j| j.pool)
            .collect();
        let mapping: BTreeMap<_, _> = used.iter().enumerate().map(|(i, &old)| (old, i)).collect();
        let pool_repos: Vec<_> = used
            .iter()
            .map(|&p| jobs.iter().find(|j| j.pool == p).map_or(0, |j| j.repo))
            .collect();
        pool_limits = used.iter().map(|&p| pool_limits[p]).collect();
        for j in &mut jobs {
            j.pool = mapping.get(&j.pool).copied().unwrap_or_default();
        }
        let origin = trace
            .iter()
            .filter_map(|j| j.run_created_at)
            .min()
            .map_or(0.0, |t| t.as_millisecond() as f64 / 1000.0);
        let availability = crate::availability::prepare(config, &mapping, origin)?;
        let mut runner_history = Vec::new();
        for change in &config.runner_history {
            let repo = repo_names
                .iter()
                .position(|r| *r == change.repo)
                .ok_or_else(|| Error::Invalid("runner history repository is absent".into()))?;
            let pools: Vec<_> = pool_repos
                .iter()
                .enumerate()
                .filter_map(|(i, &r)| (r == repo).then_some(i))
                .collect();
            if pools.len() != 1 || change.host >= config.hosts.len() {
                return Err(Error::Invalid(
                    "runner history needs an unambiguous pool and valid host".into(),
                ));
            }
            runner_history.push((
                change.at.as_millisecond() as f64 / 1000.0 - origin,
                change.host,
                pools[0],
                change.runners,
            ));
        }
        crate::prepare_graph::cancellations(&mut runs, &observations, config, &mut diagnostics);
        crate::prepare_graph::wire(&mut jobs, &mut runs, &observations, &mut diagnostics)?;
        let work: Vec<_> = jobs.iter().map(|j| j.work).collect();
        let needs: Vec<_> = jobs.iter().map(|j| j.needs.clone()).collect();
        let paths = runwell_scheduler::critical_paths(&work, &needs)?;
        for (j, path) in jobs.iter_mut().zip(paths) {
            j.path = path;
        }
        diagnostics.calibration_runs = runs
            .iter()
            .filter(|r| r.report && r.event == "pull_request")
            .count();
        if diagnostics.reused_executions > 0 || diagnostics.unstarted_cancellations > 0 {
            diagnostics.warnings.push("Repeated executions across rerun snapshots are reused; cancellations with no runner and no steps consume no host work.".into());
        }
        if diagnostics.inferred_jobs > 0 {
            diagnostics.warnings.push("Dependencies inferred from timestamps: parallel branches, dispatch delays, matrix limits and workflow revisions are not fully identifiable.".into());
        }
        if diagnostics.unsupported_intrinsic_jobs > 0 {
            diagnostics.warnings.push("Some job names lack low-concurrency successes; CPU decontending is unsupported for these classes, so their work may retain contention.".into());
        }
        diagnostics.warnings.push("Heavy failure rates are all-cause proxies, not identified infrastructure failures; simulated failures do not trigger retries or change the recorded DAG.".into());
        diagnostics.warnings.push("Execution intervals alone do not identify listener availability or GitHub dispatch eligibility/order. Optional availability input constrains dispatch but cannot recover upstream eligibility. Older workflow revisions retain inferred dependencies; cancellation groups use branches when PR IDs are absent.".into());
        let repos = repo_names
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                if config.anonymize {
                    format!("repo {}", alias(i))
                } else {
                    r
                }
            })
            .collect();
        Ok(Self {
            config: config.clone(),
            jobs,
            runs,
            repos,
            pool_limits,
            pool_repos,
            runner_history,
            origin,
            availability,
            fit: model.pooled,
            classes: model.classes,
            diagnostics,
        })
    }
}
fn alias(index: usize) -> String {
    if index < 26 {
        ((b'A' + index as u8) as char).to_string()
    } else {
        (index + 1).to_string()
    }
}
