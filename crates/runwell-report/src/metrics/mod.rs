//! Pure computations over trace records.
pub mod bestcase;
pub mod concurrency;
pub mod contention;
pub mod critical;
pub mod jobs;
pub mod runs;

use crate::model::Percentiles;
use jiff::Timestamp;
use runwell_trace::TraceJob;

pub fn seconds(start: Timestamp, end: Timestamp) -> f64 {
    (end.as_nanosecond() - start.as_nanosecond()).max(0) as f64 / 1e9
}

pub fn duration(job: &TraceJob) -> Option<f64> {
    let (s, e) = (job.started_at?, job.completed_at?);
    (ran(job) && e >= s).then(|| seconds(s, e))
}

pub fn executed(job: &TraceJob) -> bool {
    job.started_at.is_some()
        && job.runner_name.as_ref().is_some_and(|r| !r.is_empty())
        && job.conclusion.as_deref() != Some("skipped")
}

pub fn ran(job: &TraceJob) -> bool {
    job.started_at.is_some()
        && job.runner_name.as_ref().is_some_and(|r| !r.is_empty())
        && !matches!(job.conclusion.as_deref(), Some("skipped"))
        && job
            .created_at
            .zip(job.started_at)
            .is_none_or(|(c, s)| s >= c)
}

/// Linear interpolation, matching the Python baseline's percentile definition.
pub fn percentile(values: &[f64], fraction: f64) -> Option<f64> {
    let mut values: Vec<_> = values.iter().copied().filter(|v| v.is_finite()).collect();
    values.sort_by(f64::total_cmp);
    if values.is_empty() {
        return None;
    }
    let k = (values.len() - 1) as f64 * fraction.clamp(0.0, 1.0);
    let lo = k.floor() as usize;
    let hi = (lo + 1).min(values.len() - 1);
    Some(values[lo] + (values[hi] - values[lo]) * (k - lo as f64))
}

pub fn percentiles(values: &[f64]) -> Percentiles {
    Percentiles {
        count: values.len(),
        p50: percentile(values, 0.5),
        p90: percentile(values, 0.9),
        p99: percentile(values, 0.99),
    }
}

pub fn percent(n: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        100.0 * n as f64 / total as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interpolates_percentiles_and_handles_empty_samples() {
        assert_eq!(percentile(&[0.0, 10.0], 0.9), Some(9.0));
        assert_eq!(percentile(&[], 0.5), None);
    }
}
