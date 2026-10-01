//! Fail-closed infrastructure classification and write-ahead automatic retries.
#![deny(missing_docs)]
mod classifier;
mod policy;
mod ports;
pub use classifier::{
    Classification, Classifier, FailureClass, FailureEvidence, RuleError, Signal,
};
pub use policy::{Outcome, RetryPolicy, RetryRequest, retry};
pub use ports::{RetryApi, RetryFuture, RetryJournal};
/// Shipped data-driven rules, also available to operators for review/customization.
pub const DEFAULT_RULES: &str = include_str!("rules.toml");
/// Retry operation failed without releasing its durable send claim.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// GitHub REST failure (sanitized).
    #[error(transparent)]
    Github(#[from] runwell_github::Error),
    /// Journal failure; sending is prohibited before a successful claim.
    #[error(transparent)]
    Store(#[from] runwell_store::Error),
}
