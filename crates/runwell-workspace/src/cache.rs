use crate::{Error, Excludes, Mode, Owner, Workspace, WorkspacePaths, disk};
use runwell_config::WorkspaceConfig;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Serialized host cache manager. Filesystem locks exclude another manager;
/// root-private journals pin lowers across daemon restarts and partial prepares.
pub struct Cache {
    pub(crate) config: WorkspaceConfig,
    pub(crate) root: PathBuf,
    pub(crate) run: PathBuf,
    pub(crate) owner: Owner,
    pub(crate) excludes: Excludes,
    pub(crate) mode: Mode,
    _locks: Vec<fs::File>,
}
impl Cache {
    /// Probe overlay once and report the selected backend. Cache and run roots
    /// must be disjoint, administrator-controlled absolute directories.
    pub fn open(config: WorkspaceConfig, run: &Path, owner: Owner) -> Result<Self, Error> {
        Self::open_inner(config, run, owner, false)
    }
    /// Explicit portable backend, also useful for deterministic host tests.
    pub fn copy(config: WorkspaceConfig, run: &Path, owner: Owner) -> Result<Self, Error> {
        Self::open_inner(config, run, owner, true)
    }
    fn open_inner(
        config: WorkspaceConfig,
        run: &Path,
        owner: Owner,
        copy: bool,
    ) -> Result<Self, Error> {
        config.validate().map_err(|_| Error::Invalid)?;
        let root = config.cache_root.clone().ok_or(Error::Invalid)?;
        if !run.is_absolute()
            || run.parent().is_none()
            || root.starts_with(run)
            || run.starts_with(&root)
        {
            return Err(Error::Invalid);
        }
        disk::directory(&root, 0o700)?;
        disk::directory(run, 0o711)?;
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt;
            let uid = rustix::process::geteuid().as_raw();
            if fs::metadata(&root)?.uid() != uid || fs::metadata(run)?.uid() != uid {
                return Err(Error::Invalid);
            }
        }
        let root = root.canonicalize()?;
        let run = run.canonicalize()?;
        if root.starts_with(&run) || run.starts_with(&root) {
            return Err(Error::Invalid);
        }
        let mut locks = Vec::new();
        for path in [&root, &run] {
            let lock = fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(path.join(".lock"))?;
            lock.try_lock().map_err(|_| Error::Locked)?;
            locks.push(lock);
        }
        disk::directory(&run.join("state"), 0o700)?;
        disk::directory(&run.join("jobs"), 0o711)?;
        let empty = root.join("empty");
        if !empty.exists() {
            disk::directory(&empty, 0o700)?;
            disk::own(&empty, owner)?;
        } else {
            let metadata = fs::symlink_metadata(&empty)?;
            if !metadata.is_dir() {
                return Err(Error::Invalid);
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.uid() != owner.uid || metadata.gid() != owner.gid {
                    return Err(Error::Invalid);
                }
            }
        }
        let mut cache = Self {
            excludes: Excludes::new(&config.excludes),
            config,
            root,
            run,
            owner,
            mode: Mode::Copy,
            _locks: locks,
        };
        if !copy {
            cache.probe()?;
        }
        match cache.mode {
            Mode::Overlay => tracing::info!("workspace backend: Linux overlayfs"),
            Mode::Copy => tracing::warn!(
                "workspace backend: private copies; overlay unavailable, warm seeding uses more I/O"
            ),
        }
        Ok(cache)
    }
    /// Backend selected for new jobs; existing jobs retain their journaled mode.
    pub fn mode(&self) -> Mode {
        self.mode
    }
    /// Host-namespace HOME path for a durable job identity.
    pub fn home(&self, id: u64) -> PathBuf {
        self.run.join("jobs").join(id.to_string()).join("home")
    }
    pub(crate) fn paths(&self, id: u64, lower: PathBuf) -> WorkspacePaths {
        let root = self.run.join("jobs").join(id.to_string());
        WorkspacePaths {
            lower,
            upper: root.join("upper"),
            work: root.join("work"),
            home: root.join("home"),
        }
    }
    pub(crate) fn backend(mode: Mode) -> Result<Box<dyn Workspace>, Error> {
        match mode {
            Mode::Copy => Ok(Box::new(crate::CopyWorkspace)),
            #[cfg(target_os = "linux")]
            Mode::Overlay => Ok(Box::new(crate::OverlayWorkspace)),
            #[cfg(not(target_os = "linux"))]
            Mode::Overlay => Err(Error::Invalid),
        }
    }
    fn probe(&mut self) -> Result<(), Error> {
        #[cfg(target_os = "linux")]
        {
            let probe = self.run.join("probe");
            disk::directory(&probe, 0o700)?;
            let paths = WorkspacePaths {
                lower: self.root.join("empty"),
                upper: probe.join("upper"),
                work: probe.join("work"),
                home: probe.join("home"),
            };
            // Also recover a daemon killed during the previous startup probe.
            crate::OverlayWorkspace.unmount(&paths.home)?;
            disk::remove(&probe)?;
            disk::directory(&probe, 0o700)?;
            match crate::OverlayWorkspace.prepare(&paths, self.owner, &self.excludes) {
                Ok(()) => {
                    crate::OverlayWorkspace.unmount(&paths.home)?;
                    self.mode = Mode::Overlay;
                }
                Err(error) => {
                    tracing::warn!(%error, "overlay startup probe failed; using private-copy workspaces")
                }
            }
            disk::remove(&probe)?;
        }
        Ok(())
    }
}
