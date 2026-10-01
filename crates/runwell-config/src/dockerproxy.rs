use crate::Error;
use serde::Deserialize;
use std::path::{Component, PathBuf};

/// In-process Docker proxy configuration, shared by every job on this node.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DockerProxyConfig {
    /// Unix socket of the host daemon.
    pub upstream_socket: PathBuf,
    /// Runtime root; sockets live at jobs/<job_id>/docker.sock below it.
    pub run_dir: PathBuf,
    /// Maximum buffered JSON request size. Streaming bodies are not buffered.
    pub max_json_bytes: usize,
    /// Deadline for reading an entire buffered JSON request body.
    pub body_read_seconds: u32,
    /// Cap missing or excessive container Memory at the job MemoryMax.
    pub cap_memory: bool,
    /// Reject privileged containers and host PID/network namespaces.
    pub deny_host_access: bool,
    /// Grace period before force-removing job containers.
    pub stop_seconds: u32,
}
impl Default for DockerProxyConfig {
    fn default() -> Self {
        Self {
            upstream_socket: "/var/run/docker.sock".into(),
            run_dir: "/run/runwell".into(),
            max_json_bytes: 2 * 1024 * 1024,
            body_read_seconds: 30,
            cap_memory: true,
            deny_host_access: false,
            stop_seconds: 10,
        }
    }
}
impl DockerProxyConfig {
    /// Reject ambiguous paths, empty limits and unbounded stop timeouts.
    pub fn validate(&self) -> Result<(), Error> {
        for path in [&self.run_dir, &self.upstream_socket] {
            if !path.is_absolute()
                || path.parent().is_none()
                || path.components().any(|v| matches!(v, Component::ParentDir))
                || path
                    .to_str()
                    .is_none_or(|s| s.chars().any(char::is_control))
            {
                return Err(Error::Validation("invalid Docker proxy path".into()));
            }
        }
        if self.max_json_bytes == 0
            || !(1..=3600).contains(&self.stop_seconds)
            || !(1..=3600).contains(&self.body_read_seconds)
        {
            return Err(Error::Validation("invalid Docker proxy limits".into()));
        }
        Ok(())
    }
}
