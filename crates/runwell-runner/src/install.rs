use crate::{Error, Template};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{fs, path::Path};

/// Clone one immutable template. Only bin/ and externals/ are hardlinked; all
/// other files are private copies. Mutable runner state and HOME are independent.
/// A failed/partial installation is never reused: callers remove it before retry.
pub fn clone_install(template: &Template, destination: &Path) -> Result<(), Error> {
    if !template.directory.join("bin/Runner.Listener").is_file() {
        return Err(Error::Release);
    }
    fs::create_dir(destination)?;
    #[cfg(unix)]
    fs::set_permissions(destination, fs::Permissions::from_mode(0o700))?;
    clone_tree(&template.directory, destination, false)?;
    for name in ["home", "_work", "_diag"] {
        fs::create_dir_all(destination.join(name))?;
    }
    Ok(())
}
fn clone_tree(source: &Path, target: &Path, immutable: bool) -> Result<(), Error> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let path = entry.path();
        let destination = target.join(&name);
        let kind = entry.file_type()?;
        // A verified archive still must not create cross-job writable state or
        // symlinks escaping the installation. Upstream archives use regular files.
        if kind.is_symlink() {
            return Err(Error::Release);
        }
        let immutable = immutable || name == "bin" || name == "externals";
        if kind.is_dir() {
            fs::create_dir(&destination)?;
            clone_tree(&path, &destination, immutable)?;
        } else if kind.is_file() {
            if name == ".runwell-verified" {
                continue;
            }
            if immutable {
                fs::hard_link(path, destination)?;
            } else {
                fs::copy(&path, &destination)?;
                #[cfg(unix)]
                fs::set_permissions(
                    &destination,
                    fs::Permissions::from_mode(
                        if entry.metadata()?.permissions().mode() & 0o111 != 0 {
                            0o700
                        } else {
                            0o600
                        },
                    ),
                )?;
            }
        } else {
            return Err(Error::Release);
        }
    }
    Ok(())
}
/// Idempotent removal confined to a direct child of the configured runners root.
pub fn remove_install(root: &Path, path: &Path) -> Result<(), Error> {
    if path.parent() != Some(root)
        || !path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("j"))
    {
        return Err(Error::Io);
    }
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_symlink() => fs::remove_file(path)?,
        Ok(_) => fs::remove_dir_all(path)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
