use crate::{Error, Excludes, Owner, Workspace, WorkspacePaths, disk};
use rustix::mount::{MountFlags, UnmountFlags, mount, mount_bind, unmount};
use std::path::Path;

/// Host-namespace Linux overlay. `redirect_dir=on` lets tools rename cached
/// directories without EXDEV; harvest resolves the resulting redirects.
/// `index=off` avoids NFS export/origin handles binding an upper to an old lower.
/// `metacopy=off` ensures copied-up regular files contain data, not metadata-only
/// placeholders. No lower is ever changed while referenced by a job.
///
/// Overlay root ownership follows its upper. Keep that root owned by the daemon,
/// and expose only its user-owned `home` subdirectory through a bind mount.
/// All backing mounts, upper and work live behind a root-only parent.
pub struct OverlayWorkspace;
impl Workspace for OverlayWorkspace {
    fn prepare(
        &self,
        paths: &WorkspacePaths,
        owner: Owner,
        excludes: &Excludes,
    ) -> Result<(), Error> {
        for path in [&paths.lower, &paths.upper, &paths.work, &paths.home] {
            if path
                .to_str()
                .is_none_or(|s| s.contains([',', ':', '\\', '\n']))
            {
                return Err(Error::Invalid);
            }
        }
        let private = paths.private()?;
        let base = private.join("base");
        let merged = private.join("merged");
        for path in [
            private,
            &paths.upper,
            &paths.work,
            &paths.home,
            &base,
            &merged,
        ] {
            disk::directory(path, 0o700)?;
        }
        let lower_home = base.join("home");
        disk::directory(&lower_home, 0o700)?;
        // Mount-table recovery can resolve the generation even without a lease.
        let reference = base.join(".generation");
        disk::remove(&reference)?;
        std::os::unix::fs::symlink(&paths.lower, reference)?;
        // Overlayfs does not cross bind mounts nested in a lower tree. Build
        // the wrapper with immutable hardlinks instead; only directory entries
        // are copied, and the daemon-only parent prevents writes to those links.
        disk::copy_tree(&paths.lower, &lower_home, excludes, true)?;
        disk::own_tree(&lower_home, owner)?;
        let upper_home = paths.upper.join("home");
        disk::directory(&upper_home, 0o700)?;
        disk::own(&upper_home, owner)?;
        let options = format!(
            "lowerdir={},upperdir={},workdir={},redirect_dir=on,index=off,metacopy=off",
            base.display(),
            paths.upper.display(),
            paths.work.display()
        );
        mount(
            "runwell",
            &merged,
            "overlay",
            MountFlags::NOSUID | MountFlags::NODEV,
            std::ffi::CString::new(options)
                .map_err(|_| Error::Invalid)?
                .as_c_str(),
        )
        .map_err(std::io::Error::from)?;
        mount_bind(merged.join("home"), &paths.home).map_err(std::io::Error::from)?;
        Ok(())
    }
    fn unmount(&self, paths: &WorkspacePaths) -> Result<(), Error> {
        // Even after a lazy detach, remove all reachable mounts. Keep the lease
        // and every backing directory until reboot if any mount stayed busy.
        let private = paths.private()?;
        let mut detached = false;
        for path in [&paths.home, &private.join("merged")] {
            match release(path) {
                Ok(()) => {}
                Err(Error::Detached) => detached = true,
                Err(error) => return Err(error),
            }
        }
        if detached {
            Err(Error::Detached)
        } else {
            Ok(())
        }
    }
}
fn release(path: &Path) -> Result<(), Error> {
    match unmount(path, UnmountFlags::empty()) {
        Ok(()) | Err(rustix::io::Errno::INVAL | rustix::io::Errno::NOENT) => Ok(()),
        Err(rustix::io::Errno::BUSY) => {
            tracing::warn!(
                "workspace mount busy; lazy detach preserves generation and upper until reboot"
            );
            unmount(path, UnmountFlags::DETACH).map_err(std::io::Error::from)?;
            Err(Error::Detached)
        }
        Err(e) => Err(std::io::Error::from(e).into()),
    }
}
