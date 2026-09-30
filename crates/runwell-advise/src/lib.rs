//! Workflow advice and agent-facing rules for runwell.
//!
//! Advice identifies shardable steps, serial hops, and missing concurrency without
//! executing workflow code or changing workflow semantics.

#![deny(missing_docs)]

/// One actionable workflow finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Stable rule identifier.
    pub rule: String,
    /// Explanation and suggested improvement in English.
    pub message: String,
}

/// Analyze workflow text without running it; currently unimplemented.
pub fn analyze(_workflow: &str) -> Result<Vec<Finding>, Error> {
    Err(Error::Unimplemented)
}

/// An operation that has not been implemented in this milestone.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The public interface is reserved for a later milestone.
    #[error("this operation is not implemented in the M0 bootstrap")]
    Unimplemented,
}
