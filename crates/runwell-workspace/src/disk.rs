use crate::{Error, Excludes, Owner};
use serde::{Serialize, de::DeserializeOwned};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::{fs, path::Path};

pub(crate) fn directory(path: &Path, mode: u32) -> Result<(), Error> {
    fs::create_dir_all(path)?;
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(Error::Invalid);
    }
    permissions(path, mode)
}
pub(crate) fn permissions(path: &Path, mode: u32) -> Result<(), Error> {
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}
pub(crate) fn own(path: &Path, owner: Owner) -> Result<(), Error> {
    #[cfg(unix)]
    {
        let m = fs::symlink_metadata(path)?;
        if m.uid() != owner.uid || m.gid() != owner.gid {
            if m.is_file() && m.nlink() > 1 {
                return Err(Error::Invalid);
            }
            std::os::unix::fs::chown(path, Some(owner.uid), Some(owner.gid))?;
        }
    }
    #[cfg(not(unix))]
    let _ = (path, owner);
    Ok(())
}
pub(crate) fn remove(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(path)?,
        Ok(_) => fs::remove_file(path)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
pub(crate) fn read<T: DeserializeOwned>(path: &Path) -> Result<T, Error> {
    serde_json::from_slice(&fs::read(path)?).map_err(|_| Error::Invalid)
}
pub(crate) fn write(path: &Path, value: &impl Serialize) -> Result<(), Error> {
    let temp = path.with_extension("tmp");
    remove(&temp)?;
    fs::write(
        &temp,
        serde_json::to_vec(value).map_err(|_| Error::Invalid)?,
    )?;
    fs::File::open(&temp)?.sync_all()?;
    fs::rename(temp, path)?;
    sync(path.parent().ok_or(Error::Invalid)?)
}
pub(crate) fn sync(path: &Path) -> Result<(), Error> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}
/// Build only regular files/directories. Never follow a job-provided symlink or
/// carry xattrs (including capabilities/ACLs). Hardlinks are exclusively between
/// daemon-owned immutable trees; writable copies always get independent inodes.
pub(crate) fn copy_tree(
    source: &Path,
    target: &Path,
    excludes: &Excludes,
    links: bool,
) -> Result<(), Error> {
    copy_at(source, target, Path::new(""), excludes, links)
}
pub(crate) fn copy_at(
    source: &Path,
    target: &Path,
    relative: &Path,
    excludes: &Excludes,
    links: bool,
) -> Result<(), Error> {
    directory(target, 0o700)?;
    for e in fs::read_dir(source)? {
        let e = e?;
        let rel = relative.join(e.file_name());
        if excludes.contains(&rel) {
            continue;
        }
        let to = target.join(e.file_name());
        let kind = e.file_type()?;
        if kind.is_dir() {
            copy_at(&e.path(), &to, &rel, excludes, links)?;
        } else if kind.is_file() && (!links || fs::hard_link(e.path(), &to).is_err()) {
            copy_file(&e.path(), &to)?;
        }
    }
    Ok(())
}
pub(crate) fn copy_file(source: &Path, target: &Path) -> Result<(), Error> {
    // Callers remove the destination first; fs::copy must never truncate a
    // hardlink belonging to an older mounted generation.
    remove(target)?;
    std::io::copy(&mut fs::File::open(source)?, &mut fs::File::create(target)?)?;
    fs::File::options()
        .write(true)
        .open(target)?
        .set_times(fs::FileTimes::new().set_modified(fs::metadata(source)?.modified()?))?;
    #[cfg(unix)]
    permissions(
        target,
        if fs::metadata(source)?.permissions().mode() & 0o111 != 0 {
            0o700
        } else {
            0o600
        },
    )?;
    Ok(())
}
pub(crate) fn own_tree(path: &Path, owner: Owner) -> Result<(), Error> {
    let m = fs::symlink_metadata(path)?;
    if m.is_dir() {
        for e in fs::read_dir(path)? {
            own_tree(&e?.path(), owner)?;
        }
    }
    if !m.is_symlink() {
        own(path, owner)?;
    }
    Ok(())
}
pub(crate) fn size(path: &Path, cap: u64) -> Result<u64, Error> {
    let m = fs::symlink_metadata(path)?;
    let mut bytes = if m.is_file() { m.len() } else { 0 };
    if m.is_dir() {
        for e in fs::read_dir(path)? {
            bytes = bytes
                .checked_add(size(&e?.path(), cap)?)
                .ok_or(Error::SizeCap)?;
            if bytes > cap {
                return Err(Error::SizeCap);
            }
        }
    }
    if bytes > cap {
        return Err(Error::SizeCap);
    }
    Ok(bytes)
}

pub(crate) fn prune(path: &Path, relative: &Path, excludes: &Excludes) -> Result<(), Error> {
    if excludes.contains(relative) {
        return remove(path);
    }
    if fs::symlink_metadata(path)?.is_dir() {
        for e in fs::read_dir(path)? {
            let e = e?;
            prune(&e.path(), &relative.join(e.file_name()), excludes)?;
        }
    }
    Ok(())
}

// Reject oversized job input before copying bytes; the final merged size is
// checked separately because it also includes unchanged and redirected lowers.
pub(crate) fn eligible_size(
    path: &Path,
    relative: &Path,
    excludes: &Excludes,
    cap: u64,
) -> Result<u64, Error> {
    if excludes.contains(relative) {
        return Ok(0);
    }
    let m = fs::symlink_metadata(path)?;
    let mut size = if m.is_file() && !crate::harvest::multiple_links(&m) {
        m.len()
    } else {
        0
    };
    if m.is_dir() {
        for e in fs::read_dir(path)? {
            let e = e?;
            size = size
                .checked_add(eligible_size(
                    &e.path(),
                    &relative.join(e.file_name()),
                    excludes,
                    cap,
                )?)
                .ok_or(Error::SizeCap)?;
            if size > cap {
                return Err(Error::SizeCap);
            }
        }
    }
    if size > cap {
        return Err(Error::SizeCap);
    }
    Ok(size)
}
