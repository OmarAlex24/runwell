//! Atomic local editing with stale-file detection and unified diffs.
use crate::{
    Error,
    model::{Change, Edit},
    yaml,
};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);

pub(crate) fn apply(source: &str, mut edits: Vec<Edit>) -> Result<String, Error> {
    edits.sort_by_key(|e| (e.start, e.end));
    for pair in edits.windows(2) {
        if pair[0].end > pair[1].start {
            return Err(Error::Fix("overlapping edits".into()));
        }
    }
    let mut result = source.to_string();
    for e in edits.into_iter().rev() {
        if e.end < e.start || !source.is_char_boundary(e.start) || !source.is_char_boundary(e.end) {
            return Err(Error::Fix("invalid edit span".into()));
        }
        result.replace_range(e.start..e.end, &e.replacement);
    }
    yaml::parse(&result)?;
    Ok(result)
}
/// Replace a file only if its bytes still match the analyzed snapshot.
pub fn atomic_write(path: &Path, original: &[u8], replacement: &[u8]) -> Result<(), Error> {
    atomic_write_checked(path, original, replacement, &stamp(path)?)
}
pub(crate) fn atomic_write_checked(
    path: &Path,
    original: &[u8],
    replacement: &[u8],
    expected: &Stamp,
) -> Result<(), Error> {
    let parent = path.parent().unwrap_or(Path::new("."));
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(Error::Fix("refusing to replace a symlink".into()));
    }
    let temp = parent.join(format!(
        ".runwell-advise-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.set_permissions(fs::metadata(path)?.permissions())?;
        file.write_all(replacement)?;
        file.sync_all()?;
        if fs::read(path)? != original || &stamp(path)? != expected {
            return Err(Error::Fix(format!(
                "{} changed since it was read; aborting",
                path.display()
            )));
        }
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if temp.exists() {
        let _ = fs::remove_file(&temp);
    }
    result
}
pub(crate) fn change(file: &str, old: &str, new: &str) -> Change {
    let diff = similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(file, file)
        .to_string();
    Change {
        file: file.into(),
        diff,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Stamp {
    modified: Option<std::time::SystemTime>,
    len: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    device: u64,
}
fn stamp(path: &Path) -> Result<Stamp, Error> {
    let m = fs::symlink_metadata(path)?;
    if m.file_type().is_symlink() {
        return Err(Error::Fix("refusing to replace a symlink".into()));
    }
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Ok(Stamp {
        modified: m.modified().ok(),
        len: m.len(),
        #[cfg(unix)]
        inode: m.ino(),
        #[cfg(unix)]
        device: m.dev(),
    })
}
pub(crate) fn read(path: &Path) -> Result<(String, Stamp), Error> {
    let before = stamp(path)?;
    let source = fs::read_to_string(path)?;
    verify(path, source.as_bytes(), &before)?;
    Ok((source, before))
}
pub(crate) fn verify(path: &Path, original: &[u8], expected: &Stamp) -> Result<(), Error> {
    if &stamp(path)? != expected || fs::read(path)? != original {
        return Err(Error::Fix(format!(
            "{} changed since it was read; aborting",
            path.display()
        )));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn replacement_with_identical_bytes_is_stale() {
        let dir = std::env::temp_dir().join(format!("runwell-stamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("checks.yml");
        std::fs::write(&path, "jobs: {}\n").unwrap();
        let (source, stamp) = super::read(&path).unwrap();
        let newer = dir.join("new.yml");
        std::fs::write(&newer, &source).unwrap();
        std::fs::rename(newer, &path).unwrap();
        assert!(
            super::atomic_write_checked(&path, source.as_bytes(), b"replacement", &stamp).is_err()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
