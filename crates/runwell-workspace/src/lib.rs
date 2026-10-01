//! Private writable HOME trees over immutable cache generations.
//!
//! The node owns one Cache per host, stops all job writers before harvest, and
//! passes only authenticated workflow metadata to promotion. See README.md.
#![deny(missing_docs)]
mod budget;
mod cache;
mod copy_delta;
mod disk;
#[cfg(test)]
mod disk_tests;
mod excludes;
mod gc;
mod generations;
mod harvest;
mod jobs;
mod model;
mod mounts;
#[cfg(target_os = "linux")]
mod overlay;
mod workspace;
pub use cache::Cache;
pub use excludes::{DEFAULT_EXCLUDES, Excludes};
pub use harvest::{LayerEntry, LayerMetadata, apply_upper};
pub use model::*;
#[cfg(target_os = "linux")]
pub use overlay::OverlayWorkspace;
pub use workspace::{CopyWorkspace, Workspace, WorkspacePaths};

/// Filesystem, configuration, or lifecycle failure; never includes job contents.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A host filesystem operation failed.
    #[error("workspace filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    /// Invalid cache identity, path, or on-disk journal.
    #[error("invalid workspace configuration or journal")]
    Invalid,
    /// Another manager owns this cache/run directory.
    #[error("workspace is already managed by another node")]
    Locked,
    /// The candidate exceeds its logical byte budget.
    #[error("workspace generation exceeds the size cap")]
    SizeCap,
    /// A candidate contains too many entries or an excessively deep path.
    #[error("workspace generation exceeds the entry or depth cap")]
    TreeCap,
    /// Reconciliation attempted every job but could not clean these identities.
    #[error("workspace reconciliation failed for jobs: {0:?}")]
    Reconcile(Vec<u64>),
    /// Detached mounts may still have users. Preserve the generation and upper.
    #[error("workspace lazily detached; retained until a host reboot")]
    Detached,
}
