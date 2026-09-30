use crate::{
    Cache, CacheKey, Completion, Error, Generation, Lease, Mode, Promotion, disk, harvest, mounts,
};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

impl Cache {
    /// Resolve the selected immutable generation, creating an empty gen-0 on
    /// first use. `current` is always a relative symlink switched by rename.
    pub fn current(&mut self, key: &CacheKey) -> Result<PathBuf, Error> {
        let root = self.root.join(key.directory());
        disk::directory(&root, 0o700)?;
        if fs::symlink_metadata(root.join("current"))
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            let initial = root.join("gen-0");
            disk::directory(&initial, 0o700)?;
            disk::own(&initial, self.owner)?;
            disk::write(
                &root.join("gen-0.json"),
                &Generation {
                    promoted_at: None,
                    job: None,
                },
            )?;
            switch(&root, "gen-0")?;
        }
        resolve(&root)
    }
    /// Build and atomically publish an eligible stopped job's changes on the
    /// latest generation. `now` is UNIX seconds; a clock rollback delays harvest.
    pub fn promote(
        &mut self,
        id: u64,
        completion: &Completion,
        now: u64,
    ) -> Result<Promotion, Error> {
        let lease: Lease = disk::read(&self.lease_path(id))?;
        let Some(key) = &lease.key else {
            return Ok(Promotion::Untrusted);
        };
        if !lease.ready
            || lease.detached_boot.is_some()
            || !completion.trusted(key, self.config.allow_pull_requests)
        {
            return Ok(Promotion::Untrusted);
        }
        let current = self.current(key)?;
        let root = current.parent().ok_or(Error::Invalid)?;
        let record: Generation = disk::read(&current.with_extension("json"))?;
        if record.job == Some(id)
            || record
                .promoted_at
                .is_some_and(|t| now < t || now - t < self.config.promotion_interval_seconds)
        {
            return Ok(Promotion::Interval);
        }
        let next = generations(root)?
            .iter()
            .map(|(n, _)| *n)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(Error::Invalid)?;
        let stage = root.join(".stage");
        disk::remove(&stage)?;
        disk::directory(&stage, 0o700)?;
        let build = (|| {
            let source = if lease.mode == Mode::Overlay {
                self.paths(id, lease.lower.clone()).upper
            } else {
                self.home(id)
            };
            disk::eligible_size(
                &source,
                Path::new(""),
                &self.excludes,
                self.config.max_generation_bytes,
            )?;
            disk::copy_tree(&current, &stage, &self.excludes, true)?;
            let paths = self.paths(id, lease.lower.clone());
            match lease.mode {
                Mode::Overlay => harvest::apply_upper(
                    &lease.lower,
                    &paths.upper,
                    &stage,
                    &self.excludes,
                    &harvest::NativeMetadata,
                )?,
                Mode::Copy => {
                    crate::copy_delta::apply(&lease.lower, &paths.home, &stage, &self.excludes)?
                }
            }
            disk::size(&stage, self.config.max_generation_bytes)?;
            disk::own_tree(&stage, self.owner)?;
            sync_tree(&stage)?;
            Ok::<_, Error>(())
        })();
        if let Err(error) = build {
            disk::remove(&stage)?;
            if matches!(error, Error::SizeCap) {
                tracing::warn!(
                    job_id = id,
                    "cache promotion skipped: generation exceeds size cap"
                );
                return Ok(Promotion::TooLarge);
            }
            return Err(error);
        }
        let name = format!("gen-{next}");
        let destination = root.join(&name);
        fs::rename(&stage, &destination)?;
        disk::write(
            &destination.with_extension("json"),
            &Generation {
                promoted_at: Some(now),
                job: Some(id),
            },
        )?;
        switch(root, &name)?;
        self.gc()?;
        Ok(Promotion::Published(destination))
    }
    /// Keep the newest N plus current and every journal/mount-table reference.
    /// The mount scan also protects jobs whose journal write was interrupted.
    pub fn gc(&mut self) -> Result<(), Error> {
        let mut pinned: HashSet<_> = self.leases()?.into_iter().map(|l| l.lower).collect();
        pinned.extend(mounts::scan()?.into_iter().map(|m| m.lower));
        for e in fs::read_dir(&self.root)? {
            let e = e?;
            if !e.file_type()?.is_dir() || !e.path().join("current").exists() {
                continue;
            }
            let root = e.path();
            pinned.insert(resolve(&root)?);
            let mut generations = generations(&root)?;
            generations.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
            for (_, path) in generations.into_iter().skip(self.config.keep_generations) {
                if !pinned.iter().any(|reference| reference.starts_with(&path)) {
                    disk::remove(&path)?;
                    disk::remove(&path.with_extension("json"))?;
                }
            }
        }
        Ok(())
    }
}
fn generations(root: &Path) -> Result<Vec<(u64, PathBuf)>, Error> {
    let mut result = Vec::new();
    for e in fs::read_dir(root)? {
        let e = e?;
        if e.file_type()?.is_dir()
            && let Some(n) = e
                .file_name()
                .to_str()
                .and_then(|s| s.strip_prefix("gen-"))
                .and_then(|s| s.parse().ok())
        {
            result.push((n, e.path()));
        }
    }
    Ok(result)
}
fn resolve(root: &Path) -> Result<PathBuf, Error> {
    let target = fs::read_link(root.join("current"))?;
    let name = target.to_str().ok_or(Error::Invalid)?;
    if !name
        .strip_prefix("gen-")
        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(Error::Invalid);
    }
    let path = root.join(target);
    if !fs::symlink_metadata(&path)?.is_dir() {
        return Err(Error::Invalid);
    }
    Ok(path)
}
fn switch(root: &Path, name: &str) -> Result<(), Error> {
    let temp = root.join(".current");
    disk::remove(&temp)?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(name, &temp)?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(name, &temp)?;
    fs::rename(temp, root.join("current"))?;
    disk::sync(root)
}
fn sync_tree(path: &Path) -> Result<(), Error> {
    if fs::symlink_metadata(path)?.is_dir() {
        for e in fs::read_dir(path)? {
            sync_tree(&e?.path())?;
        }
    }
    disk::sync(path)
}
