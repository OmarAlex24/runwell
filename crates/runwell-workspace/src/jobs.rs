use crate::{Cache, CacheKey, Error, Lease, disk, mounts};
use std::{collections::HashSet, fs, path::PathBuf};

impl Cache {
    pub(crate) fn lease_path(&self, id: u64) -> PathBuf {
        self.run.join("state").join(format!("{id}.json"))
    }
    pub(crate) fn leases(&self) -> Result<Vec<Lease>, Error> {
        let mut leases = Vec::new();
        for entry in fs::read_dir(self.run.join("state"))? {
            let result = (|| {
                let entry = entry?;
                if entry.path().extension().is_none_or(|s| s != "json") {
                    return Ok(None);
                }
                let lease: Lease = disk::read(&entry.path())?;
                if entry.path() != self.lease_path(lease.id) {
                    return Err(Error::Invalid);
                }
                Ok(Some(lease))
            })();
            match result {
                Ok(Some(lease)) => leases.push(lease),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(%error, "skipping unreadable workspace lease");
                }
            }
        }
        Ok(leases)
    }
    /// Prepare HOME before launching a runner. None means unknown repository:
    /// use a cold private tree and never publish it under a guessed identity.
    pub fn prepare(&mut self, id: u64, key: Option<CacheKey>) -> Result<PathBuf, Error> {
        if id == 0 {
            return Err(Error::Invalid);
        }
        if self.lease_path(id).exists() {
            let lease: Lease = disk::read(&self.lease_path(id))?;
            if lease.key != key || lease.detached_boot.is_some() {
                return Err(Error::Invalid);
            }
            if lease.ready
                && (lease.mode == crate::Mode::Copy
                    || mounts::scan()?
                        .iter()
                        .any(|m| m.home == self.home(id) && m.lower == lease.lower))
            {
                return Ok(self.home(id));
            }
            // A failed prepare is not reused; callers may only prepare stopped jobs.
            self.teardown(id)?;
        }
        let lower = if let Some(key) = &key {
            self.current(key)?
        } else {
            self.root.join("empty")
        };
        let root = self.home(id).parent().ok_or(Error::Invalid)?.to_owned();
        disk::directory(&root, 0o711)?;
        let mut lease = Lease {
            id,
            key,
            lower,
            mode: self.mode,
            detached_boot: None,
            ready: false,
            execution: None,
        };
        disk::write(&self.lease_path(id), &lease)?;
        Self::backend(lease.mode)?.prepare(
            &self.paths(id, lease.lower.clone()),
            self.owner,
            &self.excludes,
        )?;
        lease.ready = true;
        disk::write(&self.lease_path(id), &lease)?;
        Ok(self.home(id))
    }
    /// Persist complete actual-assignment evidence before acknowledging the
    /// controller event. Partial events cannot overwrite complete evidence.
    pub fn bind(&mut self, id: u64, execution: crate::Execution) -> Result<(), Error> {
        if execution.request_id <= 0
            || execution.workflow_run_id <= 0
            || !runwell_config::valid_repository(&execution.repository)
        {
            return Err(Error::Invalid);
        }
        let mut lease: Lease = disk::read(&self.lease_path(id))?;
        if lease
            .execution
            .as_ref()
            .is_some_and(|old| old != &execution)
        {
            return Err(Error::Invalid);
        }
        lease.execution = Some(execution);
        disk::write(&self.lease_path(id), &lease)
    }
    /// Read actual assignment evidence independently of provisioned demand.
    pub fn execution(&self, id: u64) -> Result<Option<crate::Execution>, Error> {
        Ok(disk::read::<Lease>(&self.lease_path(id))?.execution)
    }
    /// Release a stopped job. Lazy detach retains a durable lease until reboot;
    /// callers may finish runner cleanup but must not remove these retained paths.
    pub fn teardown(&mut self, id: u64) -> Result<(), Error> {
        let lease = match disk::read::<Lease>(&self.lease_path(id)) {
            Ok(lease) => Some(lease),
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        if let Some(mut lease) = lease {
            if let Some(boot) = &lease.detached_boot {
                if *boot == boot_id()? {
                    return Err(Error::Detached);
                }
            } else if lease.mode == crate::Mode::Overlay {
                // Journal before unmount: a crash between detach and reference
                // release must never permit GC of a still-used lower.
                lease.detached_boot = Some(boot_id()?);
                disk::write(&self.lease_path(id), &lease)?;
                match Self::backend(lease.mode)?.unmount(&self.paths(id, lease.lower.clone())) {
                    Ok(()) => {}
                    Err(Error::Detached) => return Err(Error::Detached),
                    Err(e) => {
                        lease.detached_boot = None;
                        disk::write(&self.lease_path(id), &lease)?;
                        return Err(e);
                    }
                }
            }
        } else {
            for mount in mounts::scan()?
                .iter()
                .filter(|m| mounts::job_id(&self.run, &m.home) == Some(id))
            {
                let lease = Lease {
                    id,
                    key: None,
                    lower: mount.lower.clone(),
                    mode: crate::Mode::Overlay,
                    detached_boot: Some(boot_id()?),
                    ready: false,
                    execution: None,
                };
                disk::write(&self.lease_path(id), &lease)?;
                Self::backend(crate::Mode::Overlay)?
                    .unmount(&self.paths(id, mount.lower.clone()))?;
            }
        }
        disk::remove(self.home(id).parent().ok_or(Error::Invalid)?)?;
        disk::remove(&self.run.join("upper").join(id.to_string()))?;
        disk::remove(&self.lease_path(id))?;
        Ok(())
    }
    /// Reconcile after consulting the durable store and inspecting live units.
    /// Preserve every supplied job; stale mounts and orphan upper/work dirs go.
    /// A store entry with unfinished cleanup can be retained to harvest it later.
    pub fn reconcile(&mut self, retained_jobs: &HashSet<u64>) -> Result<(), Error> {
        let mut ids: HashSet<_> = self.leases()?.iter().map(|l| l.id).collect();
        for mount in mounts::scan()? {
            if let Some(id) = mounts::job_id(&self.run, &mount.home) {
                ids.insert(id);
            }
        }
        for directory in ["jobs", "upper", "state"] {
            for entry in fs::read_dir(self.run.join(directory))? {
                match entry {
                    Ok(entry) => {
                        let name = entry.file_name();
                        let id = name
                            .to_str()
                            .and_then(|s| s.strip_suffix(".json").unwrap_or(s).parse::<u64>().ok());
                        if let Some(id) = id {
                            ids.insert(id);
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "skipping unreadable workspace directory entry")
                    }
                }
            }
        }
        let mut failures = Vec::new();
        for id in ids.difference(retained_jobs) {
            match self.teardown(*id) {
                Ok(()) | Err(Error::Detached) => {}
                Err(error) => {
                    tracing::warn!(job_id = id, %error, "workspace teardown deferred; continuing reconciliation");
                    failures.push(*id);
                }
            }
        }
        if let Err(error) = self.gc() {
            tracing::warn!(%error, "workspace reconciliation garbage collection deferred");
        }
        failures.sort_unstable();
        if failures.is_empty() {
            Ok(())
        } else {
            Err(Error::Reconcile(failures))
        }
    }
}
fn boot_id() -> Result<String, Error> {
    #[cfg(target_os = "linux")]
    {
        Ok(fs::read_to_string("/proc/sys/kernel/random/boot_id")?
            .trim()
            .into())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok("portable".into())
    }
}
