//! Read-only host discovery and a resumable CI setup wizard.
//!
//! SSH always uses batch key authentication. The remote probe only reads host
//! state; recommendation and installation are reserved behind explicit traits.

pub mod cli;
pub mod facts;
pub mod model;
pub mod probe;
mod releases;
pub mod session;
pub mod ssh;
pub mod warnings;
mod wizard;

pub use cli::{SetupArgs, run};
pub use facts::HostFacts;
pub use model::{Applier, Recommender, SetupSession};

/// Failures from discovery, local persistence, or unfinished setup stages.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid host target; use user@host[:port] (bracket IPv6 addresses)")]
    InvalidHost,
    #[error("invalid repository; use owner/repository")]
    InvalidRepo,
    #[error("SSH key authentication failed for {0}; install a public key and retry")]
    Authentication(String),
    #[error("SSH operation failed for {0}")]
    Ssh(String),
    #[error("SSH operation timed out for {0}")]
    Timeout(String),
    #[error("invalid probe JSON: {0}")]
    ProbeJson(serde_json::Error),
    #[error("invalid local session JSON: {0}")]
    SessionJson(#[from] serde_json::Error),
    #[error("local I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("interactive prompt failed: {0}")]
    Prompt(#[from] dialoguer::Error),
    #[error("{0}")]
    Usage(String),
    #[error("recommendation is unimplemented in this milestone")]
    Unimplemented,
    #[error("execution requires the exact confirmation: APPLY")]
    ConfirmationRequired,
}
