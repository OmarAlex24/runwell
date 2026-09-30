//! Reusable policy sanity checks.
use crate::{Error, Policy, PreparedTrace, engine};
use serde::Serialize;

/// Standard admission sweep. Each value scales CPU and RAM reservation limits.
pub const OVERCOMMIT_SWEEP: [f64; 6] = [1.0, 1.25, 1.5, 2.0, 2.5, 3.0];

/// Job-by-job equivalence result; aggregate percentiles alone can hide bugs.
#[derive(Debug, Clone, Serialize)]
pub struct Equivalence {
    /// Host prefix used for both replays.
    pub hosts: usize,
    /// Largest absolute job start/end difference, in seconds.
    pub max_timing_error_seconds: f64,
    /// Timings, placements, cancellations and failure draws all match.
    pub passed: bool,
}
/// Verify that count-limited runwell is exactly baseline without semaphore waste.
pub fn verify_equivalent(trace: &PreparedTrace, hosts: usize) -> Result<Equivalence, Error> {
    if hosts == 0 || hosts > trace.config.hosts.len() {
        return Err(Error::Invalid("invalid host count".into()));
    }
    let mut reference = trace.config.clone();
    reference.heavy_slots = None;
    let baseline = engine::replay(trace, &reference, Policy::Baseline, hosts)?;
    let equivalent = engine::replay(trace, &trace.config, Policy::Equivalent, hosts)?;
    let mut error = 0.0_f64;
    let mut passed = baseline.cancelled_runs == equivalent.cancelled_runs;
    for (a, b) in baseline.timings.iter().zip(&equivalent.timings) {
        error = error
            .max((a.start - b.start).abs())
            .max((a.end - b.end).abs());
        passed &= a.host == b.host && a.failure == b.failure;
    }
    Ok(Equivalence {
        hosts,
        max_timing_error_seconds: error,
        passed: passed && error <= 1e-7,
    })
}
