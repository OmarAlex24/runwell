use crate::{Error, Excludes};
use runwell_config::WorkspaceConfig;
use std::{fs, path::Path};

/// Bound traversal before any recursive copy, including empty directory forests.
/// The iterative walk uses at most depth+1 directory handles, not a list of all
/// entries. Excluded subtrees are never read or charged to the cache budget.
pub(crate) fn check(
    path: &Path,
    excludes: &Excludes,
    limits: &WorkspaceConfig,
) -> Result<(), Error> {
    let mut bytes = 0_u64;
    let mut entries = 0_u64;
    let mut stack = vec![(fs::read_dir(path)?, std::path::PathBuf::new())];
    while let Some((directory, relative)) = stack.last_mut() {
        let Some(entry) = directory.next() else {
            stack.pop();
            continue;
        };
        let entry = entry?;
        let relative = relative.join(entry.file_name());
        if excludes.contains(&relative) {
            continue;
        }
        entries = entries.checked_add(1).ok_or(Error::TreeCap)?;
        if entries > limits.max_generation_entries
            || relative.components().count() > limits.max_generation_depth
        {
            return Err(Error::TreeCap);
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_file() {
            bytes = bytes.checked_add(metadata.len()).ok_or(Error::SizeCap)?;
            if bytes > limits.max_generation_bytes {
                return Err(Error::SizeCap);
            }
        } else if metadata.is_dir() {
            stack.push((fs::read_dir(entry.path())?, relative));
        }
    }
    Ok(())
}
