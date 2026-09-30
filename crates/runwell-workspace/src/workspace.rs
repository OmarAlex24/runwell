use crate::{Error, Excludes, Owner, disk};
use std::path::{Path, PathBuf};

/// Paths derived only from a daemon-owned journal, never from job contents.
#[derive(Debug, Clone)]
pub struct WorkspacePaths {
    /// Fixed immutable generation, resolved before mounting.
    pub lower: PathBuf,
    /// Private upper directory.
    pub upper: PathBuf,
    /// Overlay work directory on the same filesystem as upper.
    pub work: PathBuf,
    /// Job's HOME in the host namespace.
    pub home: PathBuf,
}
/// Portable filesystem boundary. Callers stop all processes/containers before
/// reading an upper, removing a workspace, or releasing a generation reference.
pub trait Workspace {
    /// Populate/mount HOME and apply ownership for the unprivileged job account.
    fn prepare(
        &self,
        paths: &WorkspacePaths,
        owner: Owner,
        excludes: &Excludes,
    ) -> Result<(), Error>;
    /// Unmount, returning Detached if lazy fallback leaves uncertain references.
    fn unmount(&self, home: &Path) -> Result<(), Error>;
}
/// Plain-directory backend. Writable files cannot safely share hardlinks: Unix
/// has no copy-on-write for hardlinks. Private byte copies preserve isolation on
/// ext4 and macOS; hardlinks are reserved for immutable generation construction.
pub struct CopyWorkspace;
impl Workspace for CopyWorkspace {
    fn prepare(
        &self,
        paths: &WorkspacePaths,
        owner: Owner,
        excludes: &Excludes,
    ) -> Result<(), Error> {
        disk::copy_tree(&paths.lower, &paths.home, excludes, false)?;
        disk::own_tree(&paths.home, owner)
    }
    fn unmount(&self, _home: &Path) -> Result<(), Error> {
        Ok(())
    }
}
