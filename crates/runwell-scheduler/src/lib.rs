//! Pure scheduling and placement policy for runwell.
//!
//! Critical path and short-job estimates guide priority, with fair share across
//! repositories and pull requests. Decisions consume snapshots and perform no I/O.

#![deny(missing_docs)]

use runwell_admission::Resources;

/// Scheduling metadata learned from job history.
#[derive(Debug, Clone)]
pub struct PendingJob {
    /// GitHub runner request identity, used for idempotency.
    pub request_id: i64,
    /// Size class matching a scale set.
    pub class: String,
    /// Repository and pull-request fairness key.
    pub fair_share_key: String,
    /// Estimated duration in milliseconds.
    pub estimated_duration_ms: u64,
    /// Estimated remaining critical path in milliseconds.
    pub critical_path_ms: u64,
    /// Required reservation.
    pub reservation: Resources,
}

/// A node snapshot eligible for placement.
#[derive(Debug, Clone)]
pub struct NodeHeadroom {
    /// Stable authenticated node identity.
    pub node_id: String,
    /// Classes supported by the node.
    pub classes: Vec<String>,
    /// Unreserved capacity after host headroom.
    pub available: Resources,
}

/// A selected request and host; no reservation has been committed yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// Selected request identity.
    pub request_id: i64,
    /// Selected node identity.
    pub node_id: String,
}

/// A deterministic policy with no clock, network, or storage access.
pub trait SchedulingPolicy {
    /// Select the next placement from immutable snapshots.
    fn select(
        &self,
        jobs: &[PendingJob],
        nodes: &[NodeHeadroom],
    ) -> Result<Option<Placement>, Error>;
}

/// An operation that has not been implemented in this milestone.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The public interface is reserved for a later milestone.
    #[error("this operation is not implemented in the M0 bootstrap")]
    Unimplemented,
}
