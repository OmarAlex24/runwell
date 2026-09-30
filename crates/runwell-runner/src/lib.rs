//! Verified runner templates, direct launch, and release management for runwell.
//!
//! Every runner owns a separate install directory from a SHA-256-verified immutable
//! template. JIT configuration is secret, listeners run one job, and DELETE agent is
//! attempted after every exit, including zero. Updates must meet the 30-day window.

#![deny(missing_docs)]

use secrecy::SecretString;
use std::path::PathBuf;

/// Immutable template identity; verification is required before launch.
#[derive(Debug, Clone)]
pub struct Template {
    /// Runner release version.
    pub version: String,
    /// Extracted template generation directory.
    pub directory: PathBuf,
    /// Expected release archive SHA-256 digest.
    pub sha256: [u8; 32],
}

/// One runner's private launch material; no Debug implementation exposes JIT data.
pub struct LaunchSpec {
    /// Registered agent identity, persisted before process startup.
    pub agent_id: i64,
    /// Independent install root for this runner.
    pub install_dir: PathBuf,
    /// ACTIONS_RUNNER_INPUT_JITCONFIG environment value.
    pub jit_config: SecretString,
}

/// Process and deregistration lifecycle boundary.
pub trait RunnerLifecycle {
    /// Spawn bin/Runner.Listener run directly with JIT configuration in the environment.
    fn launch(&self, spec: &LaunchSpec) -> Result<(), Error>;
    /// Always DELETE agent; 404 succeeds, 409 retains a busy runner for later retry.
    fn unregister(&self, agent_id: i64) -> Result<(), Error>;
}

/// Template release freshness and verification boundary.
pub trait ReleaseManager {
    /// Stage a SHA-256-verified release without changing running installations.
    fn stage(&self, template: &Template) -> Result<(), Error>;
}

/// An operation that has not been implemented in this milestone.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The public interface is reserved for a later milestone.
    #[error("this operation is not implemented in the M0 bootstrap")]
    Unimplemented,
}
