//! Per-job Docker attribution, bounded JSON rewriting and streaming Unix HTTP.
//! Docker access remains root-equivalent; this proxy is not an isolation boundary.
#![deny(missing_docs)]

mod json;
mod rewrite;
mod routes;
pub use rewrite::{CgroupDriver, Rewrite, Rewriter};
pub use routes::route;
#[cfg(unix)]
mod accept;
#[cfg(unix)]
mod cleanup;
#[cfg(unix)]
mod manager;
#[cfg(unix)]
mod server;
#[cfg(unix)]
mod socket;
#[cfg(unix)]
pub use cleanup::DockerResources;
#[cfg(unix)]
pub use manager::ProxyManager;
pub use runwell_config::DockerProxyConfig;
#[cfg(unix)]
pub use server::Proxy;
use std::path::PathBuf;

/// Identity, ownership and limits for a single existing job slice.
#[derive(Debug, Clone)]
pub struct ProxySpec {
    /// Durable numeric identity, unique within this node.
    pub job_id: u64,
    /// Stable node name, attached to every attributed object.
    pub node: String,
    /// Existing, limited systemd slice (ci-rw-j<id>.slice).
    pub cgroup_parent: String,
    /// Job slice MemoryMax in bytes; None means memory.max = max (unlimited).
    pub memory_max: Option<u64>,
    /// Socket owner's numeric user ID.
    pub uid: u32,
    /// Socket owner's numeric group ID.
    pub gid: u32,
}
impl ProxySpec {
    /// Parse the cgroup-v2 memory.max file, including its unlimited sentinel.
    pub fn parse_memory_max(value: &str) -> Result<Option<u64>, Error> {
        match value.trim() {
            "max" => Ok(None),
            value => value
                .parse::<u64>()
                .ok()
                .filter(|v| *v > 0 && *v <= i64::MAX as u64)
                .map(Some)
                .ok_or(Error::Config),
        }
    }
    /// Deterministic runtime socket path, also used when remapping bind sources.
    pub fn socket(&self, settings: &DockerProxyConfig) -> PathBuf {
        settings
            .run_dir
            .join("jobs")
            .join(self.job_id.to_string())
            .join("docker.sock")
    }
    /// Environment entries for the runner and its descendants.
    /// Host override is unnecessary for a local Unix daemon on the runner host.
    pub fn environment(&self, settings: &DockerProxyConfig) -> Vec<String> {
        let socket = self.socket(settings);
        vec![
            format!("DOCKER_HOST=unix://{}", socket.display()),
            format!("TESTCONTAINERS_DOCKER_SOCKET_OVERRIDE={}", socket.display()),
        ]
    }
}
/// Sanitized failures; request bodies, credentials and upstream errors are omitted.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid socket, limits, identity or cgroup driver.
    #[error("invalid Docker proxy configuration or unsupported cgroup driver")]
    Config,
    /// Malformed JSON or build labels.
    #[error("invalid Docker proxy rewrite payload")]
    Payload,
    /// Ambiguous or unsafe path encoding cannot bypass attribution.
    #[error("Docker proxy rejects encoded slashes, empty, dot or parent path segments")]
    Path,
    /// Go's JSON field folding must not disagree with the proxy.
    #[error("Docker proxy rejects non-ASCII object keys and duplicate attribution fields")]
    JsonKeys,
    /// Only Docker's known streaming upgrade endpoints may hijack a connection.
    #[error("Docker proxy upgrades are allowed only for attach, exec start, session and grpc")]
    Upgrade,
    /// Buffered requests must finish within the configured time budget.
    #[error("Docker proxy JSON body read timed out")]
    BodyTimeout,
    /// JSON requests have a configurable bounded size.
    #[error("Docker proxy JSON body exceeds configured limit of {0} bytes")]
    BodyTooLarge(usize),
    /// Cgroup reassignment is forbidden on container updates.
    #[error("Docker proxy does not allow CgroupParent in container updates")]
    CgroupUpdate,
    /// Optional compatibility policy denies these host-level settings.
    #[error("Docker proxy policy denies privileged or host PID/network access")]
    HostAccess,
    /// Teardown rejects new work while accepted mutations drain.
    #[error("Docker job proxy is stopping")]
    Stopping,
    /// Filesystem or Unix socket failure.
    #[error("Docker proxy socket operation failed")]
    Io,
    /// Upstream daemon request failed.
    #[error("Docker proxy upstream request failed")]
    Upstream,
}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::Io
    }
}
