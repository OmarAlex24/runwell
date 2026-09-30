//! Ephemeral overlay workspaces and warm cache generations for runwell.
//!
//! Mounted lower generations are immutable and retained until no jobs reference
//! them. Each job gets its own workdir and HOME; recovery removes only orphan mounts.

#![deny(missing_docs)]

use std::path::PathBuf;

/// Per-job writable paths over an immutable generation.
#[derive(Debug, Clone)]
pub struct WorkspaceSpec {
    /// Durable job identity.
    pub job_id: u64,
    /// Immutable lower generation directories.
    pub lowers: Vec<PathBuf>,
    /// Parent directory for upper, work, merged, and HOME paths.
    pub root: PathBuf,
}

/// Filesystem boundary for preparation, teardown, and unused-generation collection.
pub trait WorkspaceBackend {
    /// Prepare independent writable workdir and HOME overlays.
    fn prepare(&self, spec: &WorkspaceSpec) -> Result<(), Error>;
    /// Unmount a job only after its processes and containers have stopped.
    fn teardown(&self, job_id: u64) -> Result<(), Error>;
    /// Collect generations with no live references.
    fn gc(&self) -> Result<(), Error>;
}

#[cfg(target_os = "linux")]
pub mod overlay;

/// An operation that has not been implemented in this milestone.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The public interface is reserved for a later milestone.
    #[error("this operation is not implemented in the M0 bootstrap")]
    Unimplemented,
}
