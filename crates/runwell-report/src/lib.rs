//! GitHub API reports explaining CI queueing and critical paths for runwell.
//!
//! Reports distinguish queue wait from execution time, cite observed history, and
//! never change runner registration or retry workflow jobs.

#![deny(missing_docs)]

/// Repository workflow-run selector.
#[derive(Debug, Clone)]
pub struct ReportRequest {
    /// Repository owner.
    pub owner: String,
    /// Repository name.
    pub repository: String,
    /// Workflow run identity.
    pub run_id: u64,
}

/// Human-readable CI performance report.
#[derive(Debug, Clone)]
pub struct Report {
    /// English report content.
    pub text: String,
}

/// Build a report from GitHub history; currently unimplemented and makes no requests.
pub async fn build(
    _client: &runwell_github::RestClient,
    _request: &ReportRequest,
) -> Result<Report, Error> {
    Err(Error::Unimplemented)
}

/// An operation that has not been implemented in this milestone.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The public interface is reserved for a later milestone.
    #[error("this operation is not implemented in the M0 bootstrap")]
    Unimplemented,
}
