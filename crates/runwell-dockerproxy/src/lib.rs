//! Per-job Docker API attribution for runwell.
//!
//! The proxy rewrites cgroup parents, job labels, and Docker socket binds, and
//! preserves upgrade streams. It attributes resources; it is not a security boundary.

#![deny(missing_docs)]

use std::path::PathBuf;

/// Host paths and labels owned by one job proxy.
#[derive(Debug, Clone)]
pub struct ProxySpec {
    /// Durable numeric job identity.
    pub job_id: u64,
    /// Per-job Unix socket exposed through DOCKER_HOST.
    pub socket: PathBuf,
    /// Existing, limited systemd slice receiving Docker scopes.
    pub cgroup_parent: String,
}

/// Docker request rewriting boundary.
pub trait RequestRewriter {
    /// Force the job's cgroup, labels, and socket source in a container create body.
    fn container_create(&self, body: &[u8], spec: &ProxySpec) -> Result<Vec<u8>, Error>;
}

#[cfg(target_os = "linux")]
pub mod linux;

/// An operation that has not been implemented in this milestone.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The public interface is reserved for a later milestone.
    #[error("this operation is not implemented in the M0 bootstrap")]
    Unimplemented,
}
