use crate::{
    Cache, Error, disk,
    generations::{generations, resolve},
    mounts,
};
use std::{collections::HashSet, fs, path::Path};

impl Cache {
    /// Keep newest N, current and every valid journal/mount reference. Private
    /// lower-wrapper markers also pin generations, including detached mounts
    /// whose lease is corrupt. A damaged entry never blocks other keys' GC.
    pub fn gc(&mut self) -> Result<(), Error> {
        let mut pinned: HashSet<_> = self.leases()?.into_iter().map(|l| l.lower).collect();
        for entry in fs::read_dir(self.run.join("upper"))? {
            let reference = entry.and_then(|e| fs::read_link(e.path().join("base/.generation")));
            match reference {
                Ok(lower) => {
                    pinned.insert(lower);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => tracing::warn!(%error, "skipping unreadable private lower reference"),
            }
        }
        pinned.extend(mounts::scan()?.into_iter().map(|m| m.lower));
        for entry in fs::read_dir(&self.root)? {
            let result = (|| {
                let entry = entry?;
                if !entry.file_type()?.is_dir()
                    || fs::symlink_metadata(entry.path().join("current")).is_err()
                {
                    return Ok(());
                }
                collect(&entry.path(), &pinned, self.config.keep_generations)
            })();
            if let Err(error) = result {
                tracing::warn!(%error, "skipping damaged cache key during collection");
            }
        }
        Ok(())
    }
}
fn collect(root: &Path, pinned: &HashSet<std::path::PathBuf>, keep: usize) -> Result<(), Error> {
    let current = resolve(root)?;
    let mut generations = generations(root)?;
    generations.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
    for (_, path) in generations.into_iter().skip(keep) {
        if current != path && !pinned.iter().any(|reference| reference.starts_with(&path)) {
            disk::remove(&path)?;
            disk::remove(&path.with_extension("json"))?;
        }
    }
    Ok(())
}
