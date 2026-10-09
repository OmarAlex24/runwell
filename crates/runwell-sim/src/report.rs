use crate::{
    Config, Diagnostics, Policy, PreparedTrace,
    contention::{ContentionFit, quantile},
    engine::Outcome,
};
use serde::Serialize;
use std::collections::BTreeMap;
mod render;

/// Run latency and critical-path queue accounting for a single cohort.
#[derive(Debug, Clone, Serialize)]
pub struct Metrics {
    /// Number of runs in the cohort.
    pub runs: usize,
    /// Median end-to-end run latency, minutes.
    pub p50_minutes: f64,
    /// 90th percentile end-to-end run latency, minutes.
    pub p90_minutes: f64,
    /// 99th percentile end-to-end run latency, minutes.
    pub p99_minutes: f64,
    /// Critical-path runner/semaphore wait divided by total end-to-end run time.
    pub queue_share: f64,
    /// Selected runs cancelled in replay; excluded from completed latency samples.
    pub cancelled_runs: usize,
}
impl Metrics {
    fn new(values: &[f64], queue: f64) -> Self {
        Self {
            runs: values.len(),
            cancelled_runs: 0,
            p50_minutes: quantile(values, 0.5) / 60.0,
            p90_minutes: quantile(values, 0.9) / 60.0,
            p99_minutes: quantile(values, 0.99) / 60.0,
            queue_share: queue / values.iter().sum::<f64>().max(f64::EPSILON),
        }
    }
}
/// A row of the side-by-side policy/scenario table.
#[derive(Debug, Clone, Serialize)]
pub struct Row {
    /// Number of hosts in this scenario.
    pub hosts: usize,
    /// Scheduling variant.
    pub policy: Policy,
    /// Resource overcommit multiplier; absent for runner-count policies.
    pub overcommit: Option<f64>,
    /// Repository alias (or name if anonymization is disabled).
    pub repo: String,
    /// Workflow event.
    pub event: String,
    /// Simulated latency and queue metrics.
    pub metrics: Metrics,
    /// Physical CPU utilization, integrated over the entire replay window and fleet.
    pub mean_cpu_utilization: f64,
    /// Physical memory utilization, integrated over the entire replay window and fleet.
    pub mean_memory_utilization: f64,
    /// Simulated infra-failure proxy in reported runs, using all-cause heavy failure rates.
    pub infra_failures: usize,
    /// Simulated infra-failure proxy across all replayed jobs, including background load.
    pub total_infra_failures: usize,
    /// Host semaphore fail-open events across the entire replay.
    pub semaphore_fail_opens: usize,
}
/// Observed cohort latencies and queue composition along the resolved graph.
#[derive(Debug, Clone, Serialize)]
pub struct ObservedMetrics {
    /// Number of observed runs.
    pub runs: usize,
    /// Median latency, minutes.
    pub p50_minutes: f64,
    /// 90th percentile latency, minutes.
    pub p90_minutes: f64,
    /// 99th percentile latency, minutes.
    pub p99_minutes: f64,
    /// Observed critical-path queue share, including semaphore and matrix waits.
    pub queue_share: f64,
}
impl From<Metrics> for ObservedMetrics {
    fn from(m: Metrics) -> Self {
        Self {
            runs: m.runs,
            p50_minutes: m.p50_minutes,
            p90_minutes: m.p90_minutes,
            p99_minutes: m.p99_minutes,
            queue_share: m.queue_share,
        }
    }
}

/// Baseline, one-host comparison against the matching observed cohort.
#[derive(Debug, Clone, Serialize)]
pub struct Calibration {
    /// Repository alias or name.
    pub repo: String,
    /// Workflow event.
    pub event: String,
    /// Observed run latencies.
    pub observed: ObservedMetrics,
    /// Signed simulated / observed minus one, for p50.
    pub p50_error: f64,
    /// Signed simulated / observed minus one, for p90.
    pub p90_error: f64,
    /// Signed simulated minus observed queue share, percentage points.
    pub queue_error_pp: f64,
    /// Both absolute relative errors are at most 10%, with no censored successes.
    pub within_ten_percent: bool,
}
/// Complete JSON-serializable comparison, including evidence quality.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// Schema version for the simulator output.
    pub schema_version: u32,
    /// Reproducible failure seed.
    pub seed: u64,
    /// Applied demand, admission and semaphore assumptions; names omitted.
    pub assumptions: serde_json::Value,
    /// Fitted contention model and failure-rate support.
    pub contention: ContentionFit,
    /// Per-class slowdown fits and CPU demand proxies.
    pub classes: Vec<crate::ClassModel>,
    /// Data quality diagnostics and caveats.
    pub diagnostics: Diagnostics,
    /// All repository/event rows in policy/scenario order.
    pub rows: Vec<Row>,
    /// One-host baseline calibration, if baseline was requested.
    pub calibration: Vec<Calibration>,
    /// Exact equivalence against baseline with its semaphore disabled.
    pub equivalence: Vec<crate::experiments::Equivalence>,
    /// Optional bounded exhaustive persistent-runner allocation search.
    pub classic: Option<crate::search::SearchReport>,
}
impl Report {
    pub(crate) fn new(trace: &PreparedTrace, config: &Config) -> Self {
        let mut assumptions = serde_json::json!({"hosts": config.hosts, "cpu_overcommit": config.cpu_overcommit,
                "memory_overcommit": config.memory_overcommit, "heavy_slots": config.heavy_slots,
                "heavy_slots_per_host": config.heavy_slots_per_host,
                "semaphore_timeout_seconds": config.semaphore_timeout_seconds, "aging_seconds": config.aging_seconds,
                "memory_threshold": config.memory_threshold, "memory_penalty": config.memory_penalty,
                "default_demand": config.default_demand, "job_demand_overrides": config.jobs.len(),
                "runner_limits": trace.pool_limits, "successful_first_attempt": config.successful_first_attempt,
                "external_host_filter": config.contention_label.is_some(),
                "fit_by_class": config.fit_by_class, "derive_cpu_demand": config.derive_cpu_demand,
                "fit_proxy_load": config.fit_proxy_load,
                "cancel_in_progress": config.cancel_in_progress, "cancel_grace_seconds": config.cancel_grace_seconds,
                "cancel_on_dispatch": config.cancel_on_dispatch,
                "semaphore_history": config.semaphore_history, "observed_work": config.observed_work,
                "semaphore_release_steps_configured": !config.semaphore_release_steps.is_empty(),
                "semaphore_poll_seconds": config.semaphore_poll_seconds,
                "preserve_work_variation": config.preserve_work_variation,
                "exclude_semaphore_waits_from_contention": config.exclude_semaphore_waits_from_contention,
                "overcommit_sweep": config.overcommit_sweep, "runner_history_changes": config.runner_history.len(),
                "availability_records": config.availability.len(), "runwell_runner_availability": config.runwell_runner_availability,
                "host_outage_semantics": "pause work and dispatch; retain reservations",
                "classic_availability": "cycle observed runner patterns on recorded hosts; chosen counts replace capacity history"});
        if !config.semaphores.is_empty() {
            // Pool names and limits only; step names can identify private workflows.
            assumptions["semaphore_pools"] = config
                .semaphores
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let gate = config.gate(i + 1);
                    serde_json::json!({"name": s.name, "slots": s.slots,
                        "slots_per_host": s.slots_per_host, "slot_changes": s.slot_history.len(),
                        "timeout_seconds": gate.timeout, "poll_seconds": gate.poll,
                        "release_steps_configured": !s.release_steps.is_empty()})
                })
                .collect();
        }
        Self {
            schema_version: 1,
            seed: config.seed,
            assumptions,
            contention: trace.fit.clone(),
            classes: trace.classes.clone(),
            diagnostics: trace.diagnostics.clone(),
            rows: Vec::new(),
            calibration: Vec::new(),
            equivalence: Vec::new(),
            classic: None,
        }
    }
    pub(crate) fn add(
        &mut self,
        trace: &PreparedTrace,
        policy: Policy,
        hosts: usize,
        outcome: &Outcome,
        overcommit: Option<f64>,
    ) {
        let mut groups: BTreeMap<(usize, &str), Vec<usize>> = BTreeMap::new();
        for (i, r) in trace.runs.iter().enumerate().filter(|(_, r)| r.report) {
            groups.entry((r.repo, &r.event)).or_default().push(i);
        }
        for ((repo, event), runs) in groups {
            let mut values = Vec::new();
            let mut observed = Vec::new();
            let mut queue = 0.0;
            let mut failures = 0;
            for &r in &runs {
                let run = &trace.runs[r];
                observed.push(run.observed);
                if outcome.cancelled_runs[r] {
                    continue;
                }
                let last = run.jobs.iter().copied().max_by(|&a, &b| {
                    outcome.timings[a]
                        .end
                        .total_cmp(&outcome.timings[b].end)
                        .then(a.cmp(&b))
                });
                if let Some(last) = last {
                    values.push(outcome.timings[last].end - run.arrival);
                    let mut cursor = Some(last);
                    while let Some(i) = cursor {
                        let t = &outcome.timings[i];
                        queue += (t.start - t.ready.max(run.arrival)).max(0.0) + t.semaphore_wait;
                        cursor = trace.jobs[i].needs.iter().copied().max_by(|&a, &b| {
                            outcome.timings[a]
                                .end
                                .total_cmp(&outcome.timings[b].end)
                                .then(a.cmp(&b))
                        });
                    }
                }
                failures += run
                    .jobs
                    .iter()
                    .filter(|&&i| outcome.timings[i].failure)
                    .count();
            }
            let mut metrics = Metrics::new(&values, queue);
            metrics.cancelled_runs = runs.iter().filter(|&&r| outcome.cancelled_runs[r]).count();
            if hosts == 1 && policy == Policy::Baseline {
                let observed_queue = runs.iter().map(|&r| trace.runs[r].observed_queue).sum();
                let actual = Metrics::new(&observed, observed_queue);
                let p50_error = relative_error(metrics.p50_minutes, actual.p50_minutes);
                let p90_error = relative_error(metrics.p90_minutes, actual.p90_minutes);
                self.calibration.push(Calibration {
                    repo: trace.repos[repo].clone(),
                    event: event.into(),
                    queue_error_pp: 100.0 * (metrics.queue_share - actual.queue_share),
                    observed: actual.into(),
                    p50_error,
                    p90_error,
                    within_ten_percent: p50_error.abs() <= 0.1
                        && p90_error.abs() <= 0.1
                        && metrics.cancelled_runs == 0,
                });
            }
            self.rows.push(Row {
                hosts,
                policy,
                overcommit,
                repo: trace.repos[repo].clone(),
                event: event.into(),
                metrics,
                mean_cpu_utilization: outcome.cpu_utilization,
                mean_memory_utilization: outcome.memory_utilization,
                infra_failures: failures,
                total_infra_failures: outcome.timings.iter().filter(|t| t.failure).count(),
                semaphore_fail_opens: outcome.fail_opens,
            });
        }
    }
}
fn relative_error(simulated: f64, observed: f64) -> f64 {
    if observed > 0.0 {
        simulated / observed - 1.0
    } else if simulated == 0.0 {
        0.0
    } else {
        1.0
    }
}
