use crate::{Error, Excludes, disk};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

/// Overlay operations, abstracted so portable tests need no device nodes/xattrs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayerEntry {
    /// Remove the corresponding lower name.
    pub whiteout: bool,
    /// Discard the existing destination before applying a redirect or children.
    pub opaque: bool,
    /// Absolute from the lower root, or relative to the lower-origin parent.
    pub redirect: Option<PathBuf>,
}
/// Interpret overlay metadata; test implementations can use synthetic markers.
pub trait LayerMetadata {
    /// Classify this upper entry (including the upper root).
    fn entry(&self, upper: &Path, relative: &Path) -> Result<LayerEntry, Error>;
}
/// Apply an upper to an already hardlink-seeded candidate. Both upper and lower
/// must be quiescent; the destination must be daemon-private and disposable.
/// Whiteouts, opacity, and redirects are consumed, never copied as cache data.
pub fn apply_upper(
    lower: &Path,
    upper: &Path,
    candidate: &Path,
    excludes: &Excludes,
    metadata: &dyn LayerMetadata,
) -> Result<(), Error> {
    apply(
        lower,
        upper,
        candidate,
        Path::new(""),
        Path::new(""),
        excludes,
        metadata,
    )?;
    disk::prune(candidate, Path::new(""), excludes)
}
#[allow(clippy::too_many_arguments)]
fn apply(
    lower: &Path,
    upper: &Path,
    target: &Path,
    relative: &Path,
    origin: &Path,
    excludes: &Excludes,
    metadata: &dyn LayerMetadata,
) -> Result<(), Error> {
    if excludes.contains(relative) {
        disk::remove(target)?;
        return Ok(());
    }
    let m = fs::symlink_metadata(upper)?;
    let entry = metadata.entry(upper, relative)?;
    if entry.whiteout {
        return disk::remove(target);
    }
    if entry.opaque {
        disk::remove(target)?;
    }
    let mut origin = origin.to_owned();
    if let Some(redirect) = entry.redirect {
        // A child of a renamed parent resolves relative to its original lower
        // parent, not its new name in the upper tree. Opacity is independent.
        origin = if redirect.is_absolute() {
            redirect
                .strip_prefix("/")
                .map_err(|_| Error::Invalid)?
                .to_owned()
        } else {
            origin.parent().unwrap_or(Path::new("")).join(redirect)
        };
        if !m.is_dir()
            || !origin
                .components()
                .all(|c| matches!(c, Component::Normal(_)))
        {
            return Err(Error::Invalid);
        }
        disk::remove(target)?;
        disk::directory(target, 0o700)?;
        if !excludes.contains(&origin) {
            let source = lower.canonicalize()?.join(&origin);
            if source.canonicalize()? != source || !fs::symlink_metadata(&source)?.is_dir() {
                return Err(Error::Invalid);
            }
            disk::copy_at(&source, target, &origin, excludes, true)?;
        }
    }
    if m.is_dir() {
        if fs::symlink_metadata(target).is_ok_and(|m| !m.is_dir()) {
            disk::remove(target)?;
        }
        disk::directory(target, 0o700)?;
        for e in fs::read_dir(upper)? {
            let e = e?;
            apply(
                lower,
                &e.path(),
                &target.join(e.file_name()),
                &relative.join(e.file_name()),
                &origin.join(e.file_name()),
                excludes,
                metadata,
            )?;
        }
    } else if m.is_file() && !multiple_links(&m) {
        disk::copy_file(upper, target)?;
    } else {
        // Dropping aliases also prevents a hardlink to an excluded secret from
        // laundering that secret under an innocent filename.
        disk::remove(target)?;
    }
    Ok(())
}
pub(crate) fn multiple_links(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.nlink() > 1
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}

// The root-owned overlay wrapper contains only a user-owned `home`. Kernel
// absolute redirects include that wrapper component; never allow escape from it.
pub(crate) struct NativeMetadata;
impl LayerMetadata for NativeMetadata {
    fn entry(&self, path: &Path, _relative: &Path) -> Result<LayerEntry, Error> {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::{FileTypeExt, MetadataExt};
            let m = fs::symlink_metadata(path)?;
            if (m.file_type().is_char_device() && m.rdev() == 0)
                || (m.is_file() && m.len() == 0 && attribute(path, "whiteout")?.is_some())
            {
                return Ok(LayerEntry {
                    whiteout: true,
                    ..Default::default()
                });
            }
            if attribute(path, "metacopy")?.is_some() {
                return Err(Error::Invalid);
            }
            let opaque = attribute(path, "opaque")?.as_deref() == Some(b"y");
            let redirect = attribute(path, "redirect")?
                .map(|origin| -> Result<PathBuf, Error> {
                    let origin = String::from_utf8(origin).map_err(|_| Error::Invalid)?;
                    if origin.starts_with('/') {
                        let rest = Path::new(&origin)
                            .strip_prefix("/home")
                            .map_err(|_| Error::Invalid)?;
                        Ok(Path::new("/").join(rest))
                    } else {
                        Ok(PathBuf::from(origin))
                    }
                })
                .transpose()?;
            Ok(LayerEntry {
                opaque,
                redirect,
                whiteout: false,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = path;
            Ok(LayerEntry::default())
        }
    }
}
#[cfg(target_os = "linux")]
fn attribute(path: &Path, name: &str) -> Result<Option<Vec<u8>>, Error> {
    let mut buffer = [0_u8; 4096];
    match rustix::fs::lgetxattr(path, format!("trusted.overlay.{name}"), &mut buffer[..]) {
        Ok(n) => Ok(Some(buffer[..n].to_vec())),
        Err(rustix::io::Errno::NODATA | rustix::io::Errno::OPNOTSUPP) => Ok(None),
        Err(e) => Err(std::io::Error::from(e).into()),
    }
}
