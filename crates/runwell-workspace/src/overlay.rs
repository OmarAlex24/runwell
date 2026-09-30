use crate::{Error, Excludes, Owner, Workspace, WorkspacePaths, disk};
use rustix::mount::{MountFlags, UnmountFlags, mount, unmount};
use std::path::Path;

/// Host-namespace Linux overlay. `redirect_dir=on` lets tools rename cached
/// directories without EXDEV; harvest resolves the resulting redirects.
/// `index=off` avoids NFS export/origin handles binding an upper to an old lower.
/// `metacopy=off` ensures copied-up regular files contain data, not metadata-only
/// placeholders. No lower is ever changed while referenced by a job.
pub struct OverlayWorkspace;
impl Workspace for OverlayWorkspace {
    fn prepare(
        &self,
        paths: &WorkspacePaths,
        owner: Owner,
        _excludes: &Excludes,
    ) -> Result<(), Error> {
        for path in [&paths.lower, &paths.upper, &paths.work, &paths.home] {
            if path
                .to_str()
                .is_none_or(|s| s.contains([',', ':', '\\', '\n']))
            {
                return Err(Error::Invalid);
            }
        }
        for path in [&paths.upper, &paths.work, &paths.home] {
            disk::directory(path, 0o700)?;
            disk::own(path, owner)?;
        }
        let options = format!(
            "lowerdir={},upperdir={},workdir={},redirect_dir=on,index=off,metacopy=off",
            paths.lower.display(),
            paths.upper.display(),
            paths.work.display()
        );
        mount(
            "runwell",
            &paths.home,
            "overlay",
            MountFlags::NOSUID | MountFlags::NODEV,
            std::ffi::CString::new(options)
                .map_err(|_| Error::Invalid)?
                .as_c_str(),
        )
        .map_err(std::io::Error::from)?;
        Ok(())
    }
    fn unmount(&self, home: &Path) -> Result<(), Error> {
        match unmount(home, UnmountFlags::empty()) {
            Ok(()) | Err(rustix::io::Errno::INVAL | rustix::io::Errno::NOENT) => Ok(()),
            Err(rustix::io::Errno::BUSY) => {
                tracing::warn!(
                    "workspace mount busy; lazy detach preserves generation and upper until reboot"
                );
                unmount(home, UnmountFlags::DETACH).map_err(std::io::Error::from)?;
                Err(Error::Detached)
            }
            Err(e) => Err(std::io::Error::from(e).into()),
        }
    }
}
