//! Deterministic job-trace replay against scheduling policies for runwell.
//!
//! Simulation uses recorded arrivals and durations, with no wall-clock or network
//! access. All policies must see the same trace and capacity assumptions.

#![deny(missing_docs)]

use runwell_scheduler::SchedulingPolicy;

/// One observed job in a replay trace.
#[derive(Debug, Clone, Copy)]
pub struct TraceJob {
    /// Original request identity.
    pub request_id: i64,
    /// Arrival relative to trace start in milliseconds.
    pub arrival_ms: u64,
    /// Observed run duration in milliseconds.
    pub duration_ms: u64,
}

/// Comparable replay outcome.
#[derive(Debug, Clone, Copy)]
pub struct SimulationResult {
    /// Total trace makespan in milliseconds.
    pub makespan_ms: u64,
    /// Aggregate queue wait in milliseconds.
    pub queue_wait_ms: u64,
}

/// Replay a recorded trace using a pure policy; currently unimplemented.
pub fn replay(
    _trace: &[TraceJob],
    _policy: &impl SchedulingPolicy,
) -> Result<SimulationResult, Error> {
    Err(Error::Unimplemented)
}

/// An operation that has not been implemented in this milestone.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The public interface is reserved for a later milestone.
    #[error("this operation is not implemented in the M0 bootstrap")]
    Unimplemented,
}
