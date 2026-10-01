//! Verified immutable templates, private installations, and runner lifecycle rules.
#![deny(missing_docs)]
mod install;
mod release;
pub use install::{clone_install, remove_install};
pub use release::{
    Release, ReleaseClient, ReleaseStatus, checksum, release_deadline, release_status,
};
#[cfg(target_os = "linux")]
mod template;
use secrecy::SecretString;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
pub use template::{RunnerUser, stage_template};

/// Verified runner template generation. Templates are immutable after promotion.
#[derive(Debug, Clone)]
pub struct Template {
    /// Numeric upstream version.
    pub version: String,
    /// Extracted template directory.
    pub directory: PathBuf,
    /// Verified archive digest.
    pub sha256: [u8; 32],
}
/// Private process material; intentionally has no Debug or serialization.
pub struct LaunchSpec {
    /// Persisted agent identity.
    pub agent_id: i64,
    /// Independent installation directory.
    pub install_dir: PathBuf,
    /// Secret passed only through the service environment.
    pub jit_config: SecretString,
}
/// Listener process disposition; this does not describe the workflow result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitDisposition {
    /// Exit zero; still requires DELETE and an authoritative completion event.
    Exited,
    /// Invalid configuration/session; discard these credentials.
    Discard,
    /// Transient failure; cleanup remains mandatory.
    Transient,
    /// Disable admission until the template is refreshed.
    Outdated,
}
/// Map the direct Runner.Listener exit status (never a wrapper script's status).
pub fn classify_exit(code: Option<i32>) -> ExitDisposition {
    match code {
        Some(0) => ExitDisposition::Exited,
        Some(1 | 5) => ExitDisposition::Discard,
        Some(7) => ExitDisposition::Outdated,
        _ => ExitDisposition::Transient,
    }
}
/// Always-run remote teardown. Busy responses retain the durable cleanup entry.
pub async fn unregister(
    client: &runwell_scaleset::ActionsClient,
    agent_id: i64,
) -> Result<runwell_scaleset::RemoveRunnerResult, Error> {
    client
        .remove_runner(agent_id)
        .await
        .map_err(|_| Error::Remote)
}
/// Sanitized errors: no remote body, URL, environment, or command output is logged.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Missing, malformed, ambiguous, or mismatching checksum.
    #[error("runner checksum verification failed")]
    Checksum,
    /// Invalid upstream metadata or release identity.
    #[error("invalid runner release metadata")]
    Release,
    /// Network operation failed.
    #[error("runner release request failed")]
    Network,
    /// Template or installation filesystem failure.
    #[error("runner filesystem operation failed")]
    Io,
    /// Archive extraction as the dedicated user failed.
    #[error("runner extraction as the configured user failed")]
    Extract,
    /// Unknown/root runner account.
    #[error("runner_user must be an existing unprivileged local account")]
    User,
    /// Remote cleanup failed and must be retried.
    #[error("runner deregistration failed; cleanup retained")]
    Remote,
}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::Io
    }
}
