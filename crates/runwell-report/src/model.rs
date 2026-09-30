//! Stable, versioned report data. All durations are seconds unless named otherwise.
use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Percentiles {
    pub count: usize,
    pub p50: Option<f64>,
    pub p90: Option<f64>,
    pub p99: Option<f64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub schema_version: u32,
    pub window: Window,
    pub configuration: Configuration,
    pub runs: Vec<RunSummary>,
    pub jobs: Vec<JobSummary>,
    pub concurrency: Concurrency,
    pub contention: Vec<Contention>,
    pub failures: Failures,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    pub since: String,
    pub until: String,
    pub jobs: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Configuration {
    pub host_labels: Vec<String>,
    pub low_concurrency: usize,
    pub high_concurrency: usize,
    pub wait_patterns: Vec<String>,
    pub infra_target_percent: f64,
    pub latency_target_ratio: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    pub repo: String,
    pub event: String,
    pub end_to_end_seconds: Percentiles,
    pub best_case_seconds: Percentiles,
    pub half_baseline_p50_seconds: Option<f64>,
    pub half_baseline_p90_seconds: Option<f64>,
    pub needs_runs: usize,
    pub inferred_runs: usize,
    pub bands: Vec<Band>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Band {
    pub band: String,
    pub count: usize,
    pub end_to_end_seconds: f64,
    pub queue_seconds: f64,
    pub work_seconds: f64,
    pub dispatch_gap_seconds: f64,
    pub in_job_wait_seconds: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSummary {
    pub repo: String,
    pub workflow: String,
    pub job: String,
    pub count: usize,
    pub ran: usize,
    pub duration_seconds: Percentiles,
    pub queue_seconds: Percentiles,
    pub fail_percent: f64,
    pub cancel_percent: f64,
    pub runner_minutes: f64,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Concurrency {
    pub peak: usize,
    pub idle_fraction: f64,
    pub busy_seconds: f64,
    pub clipped_runner_overlaps: usize,
    pub timeline: Vec<TimelinePoint>,
    pub distribution: Vec<ConcurrencyLevel>,
    pub labels: Vec<LabelSaturation>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelinePoint {
    pub timestamp: String,
    pub concurrency: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConcurrencyLevel {
    pub level: usize,
    pub seconds: f64,
    pub wall_fraction: f64,
    pub busy_fraction: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelSaturation {
    pub label: String,
    pub capacity: usize,
    pub capacity_inferred: bool,
    pub peak: usize,
    pub saturation_hours: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Contention {
    pub repo: String,
    pub job: String,
    pub step: Option<String>,
    pub low: Percentiles,
    pub high: Percentiles,
    pub high_to_low_ratio: Option<f64>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Failures {
    pub eligible_jobs: usize,
    pub in_progress_jobs: usize,
    pub infra: usize,
    pub flaky: usize,
    pub code: usize,
    pub superseded: usize,
    pub unknown: usize,
    pub aggregators: usize,
    pub cancelled_before_start: usize,
    pub infra_percent: f64,
    pub flaky_percent: f64,
    pub infra_and_flaky_percent: f64,
    pub under_one_percent: bool,
    pub missing_evidence: usize,
    pub details: Vec<FailureDetail>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailureDetail {
    pub repo: String,
    pub run_id: u64,
    pub attempt: u32,
    pub job: String,
    pub class: String,
    pub reason: String,
}
