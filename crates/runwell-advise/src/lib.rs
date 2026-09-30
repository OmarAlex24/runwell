//! Static GitHub Actions advice with trace evidence and byte-preserving local fixes.
#![deny(missing_docs)]
pub mod cli;
mod context;
mod local;
mod model;
mod render;
mod rules;
mod spans;
mod timing;
mod writer;
mod yaml;
pub use model::{Change, Finding, Fix, Report, Savings, Severity};
pub use writer::atomic_write;
/// Analyze workflow text statically with automatic runner-label classification.
pub fn analyze(workflow: &str) -> Result<Vec<Finding>, Error> {
    cli::analyze_source(workflow, "workflow.yml", cli::SelfHosted::Auto, None)
}
/// Analysis, trace, or safe-edit failure (CLI exit code 2).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Filesystem error.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// Invalid YAML or workflow shape.
    #[error("{0}")]
    Parse(String),
    /// Invalid trace.
    #[error("{0}")]
    Trace(#[from] runwell_trace::TraceError),
    /// JSON encoding error.
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    /// Edit safety failure.
    #[error("{0}")]
    Fix(String),
}
