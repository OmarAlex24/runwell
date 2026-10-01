//! Standalone controller logic and a local-node port, ready for an M5 transport.
//! Admission precedes acquisition; durable intents precede JIT; cgroup samples
//! precede teardown. Linux services survive daemon restarts and drain expiry.
#![deny(missing_docs)]
mod backend;
pub mod cgroup;
mod cleanup;
mod controller;
pub mod fake;
mod gateway;
mod lifecycle;
#[cfg(target_os = "linux")]
pub mod linux;
mod runtime;
#[cfg(any(target_os = "linux", test))]
mod teardown;
#[cfg(any(target_os = "linux", test))]
mod workspace_trust;
pub use backend::*;
pub use controller::Controller;
pub use gateway::{GithubGateway, Registration, RunnerApi};
pub use runtime::{Drain, run_loop};
use std::{future::Future, pin::Pin};

/// Object-safe asynchronous port result, suitable for local or network adapters.
pub type NodeFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send + 'a>>;
/// Sanitized node failures. Foreign errors cannot expose request bodies or env.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Docker proxy setup or label-scoped cleanup failed.
    #[cfg(target_os = "linux")]
    #[error(transparent)]
    DockerProxy(#[from] runwell_dockerproxy::Error),
    /// Node daemon requires root before any host or network mutation.
    #[error("runwell node requires root to manage systemd slices; see SECURITY.md")]
    RootRequired,
    /// Linux systemd and cgroup v2 are required for host execution.
    #[error("runwell node --standalone requires Linux x86-64 with systemd and cgroup v2")]
    Unsupported,
    /// Invalid runtime configuration.
    #[error("invalid standalone node configuration")]
    Config,
    /// Physical RAM must retain OS headroom beyond the CI hard ceiling.
    #[error(
        "ci.slice MemoryMax must leave at least 5% of physical RAM (minimum 256 MiB) for the host"
    )]
    HostBudget,
    /// A restart may not lower the parent memory ceiling beneath live units.
    #[error("cannot reduce ci.slice MemoryMax while job units exist; drain them first")]
    ParentLimit,
    /// Sanitized persistent store error.
    #[error(transparent)]
    Store(#[from] runwell_store::Error),
    /// Workspace preparation or mount cleanup failed.
    #[error(transparent)]
    Workspace(#[from] runwell_workspace::Error),
    /// Sanitized runner template error.
    #[error(transparent)]
    Runner(#[from] runwell_runner::Error),
    /// Systemd operation failed; raw D-Bus payloads are deliberately omitted.
    #[error("systemd operation failed")]
    Systemd,
    /// A systemd operation did not complete within its configured deadline.
    #[error("systemd operation timed out")]
    Timeout,
    /// GitHub call failed, leaving the durable intent for recovery.
    #[error("GitHub scale-set operation failed; durable work retained")]
    Github,
    /// Queue generation changed; retain durable work and consume fresh statistics.
    #[error("scale-set session recreated; awaiting current statistics and redelivery")]
    SessionReset,
    /// Filesystem/cgroup operation failed; admission fails closed.
    #[error("node filesystem or cgroup operation failed")]
    Io,
    /// Malformed cgroup counter or PSI data.
    #[error("invalid cgroup statistics")]
    Stats,
    /// Another standalone controller owns the local journal.
    #[error("another runwell node owns this state directory")]
    Locked,
    /// Unknown busy runner must remain alive, and capacity cannot be trusted.
    #[error("unaccounted runner is still busy; admission remains stopped")]
    OrphanBusy,
}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::Io
    }
}
impl From<runwell_scaleset::Error> for Error {
    fn from(error: runwell_scaleset::Error) -> Self {
        if matches!(error, runwell_scaleset::Error::SessionRecreated) {
            Self::SessionReset
        } else {
            Self::Github
        }
    }
}
