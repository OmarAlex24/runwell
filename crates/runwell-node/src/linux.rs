//! Linux systemd/cgroup-v2 backend. Root is checked before any side effect.
mod bootstrap;
mod credentials;
mod systemd;
mod workspace_unit;
mod workspaces;
use crate::*;
pub use bootstrap::standalone;
use runwell_admission::Pressure;
use runwell_config::StandaloneConfig;
use runwell_runner::{LaunchSpec, ReleaseClient, ReleaseStatus, RunnerUser, Template};
use runwell_store::JobMeasurement;
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    time::{Duration, Instant},
};
use systemd::{ProcessCommand, Systemd};
use tokio::sync::Mutex;

/// Fail fast rather than partially configuring a host without privileges.
pub fn require_root() -> Result<(), Error> {
    if rustix::process::geteuid().is_root() {
        Ok(())
    } else {
        Err(Error::RootRequired)
    }
}
struct Templates {
    current: Option<Template>,
    checked: Option<Instant>,
    force: bool,
}
/// Production Linux host backend, with a retained D-Bus connection and templates.
pub struct LinuxBackend {
    settings: StandaloneConfig,
    systemd: Systemd,
    user: RunnerUser,
    releases: ReleaseClient,
    templates: Mutex<Templates>,
    workspaces: workspaces::Workspaces,
}
impl LinuxBackend {
    /// Connect to systemd; require cgroup v2 and the configured local runner user.
    pub async fn connect(settings: StandaloneConfig) -> Result<Self, Error> {
        Self::connect_with_github(settings, None).await
    }
    pub(super) async fn connect_with_github(
        settings: StandaloneConfig,
        github: Option<&runwell_config::GithubConfig>,
    ) -> Result<Self, Error> {
        require_root()?;
        if !cfg!(target_arch = "x86_64")
            || !std::path::Path::new("/sys/fs/cgroup/cgroup.controllers").is_file()
        {
            return Err(Error::Unsupported);
        }
        let user = RunnerUser::resolve(&settings.runner_user)?;
        let systemd = Systemd::connect(settings.stop_seconds).await?;
        let workspaces = workspaces::Workspaces::new(
            &settings,
            runwell_workspace::Owner {
                uid: user.uid,
                gid: user.gid,
            },
            github,
        )?;
        Ok(Self {
            settings,
            workspaces,
            systemd,
            user,
            releases: ReleaseClient::new()?,
            templates: Mutex::new(Templates {
                current: None,
                checked: None,
                force: false,
            }),
        })
    }
    fn workspace_home(&self, plan: &JobPlan) -> Result<std::path::PathBuf, Error> {
        let home = self.workspaces.home(plan.slice.job_id)?;
        Ok(if home.exists() {
            home
        } else {
            plan.directory.join("home")
        })
    }
    async fn current(&self) -> Result<Template, Error> {
        let mut state = self.templates.lock().await;
        if state.current.is_none() {
            // Active selection survives restart, so a refreshed template never
            // silently rolls back to the original pinned version in TOML.
            let active = self.settings.templates_dir.join("active-version");
            let version = match fs::read_to_string(active) {
                Ok(version) => version.trim().to_owned(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    self.settings.runner.version.clone()
                }
                Err(_) => return Err(Error::Io),
            };
            let pin = if version == self.settings.runner.version {
                self.settings.runner.sha256.as_deref()
            } else {
                None
            };
            state.current = Some(
                runwell_runner::stage_template(
                    &self.releases,
                    &self.settings.templates_dir,
                    &version,
                    pin,
                    &self.user,
                )
                .await?,
            );
        }
        if state.force
            || state
                .checked
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(86400))
        {
            let releases = self.releases.releases().await?;
            let current = state.current.as_ref().ok_or(Error::Config)?;
            let status = runwell_runner::release_status(
                &current.version,
                &releases,
                jiff::Timestamp::now(),
            )?;
            if status == ReleaseStatus::Warn {
                tracing::warn!("runner template is at least 21 days behind; refresh due at day 25");
            }
            if state.force || matches!(status, ReleaseStatus::Refresh | ReleaseStatus::Expired) {
                let latest = releases
                    .iter()
                    .filter(|r| !r.draft && !r.prerelease)
                    .max_by_key(|r| r.published_at)
                    .ok_or(Error::Config)?;
                let template = runwell_runner::stage_template(
                    &self.releases,
                    &self.settings.templates_dir,
                    latest.version()?,
                    None,
                    &self.user,
                )
                .await?;
                let selection = self.settings.templates_dir.join(".active-version");
                fs::write(&selection, &template.version)?;
                fs::File::open(&selection)?.sync_all()?;
                fs::rename(
                    selection,
                    self.settings.templates_dir.join("active-version"),
                )?;
                fs::File::open(&self.settings.templates_dir)?.sync_all()?;
                state.current = Some(template);
                state.force = false;
            }
            state.checked = Some(Instant::now());
        }
        state.current.clone().ok_or(Error::Config)
    }
    /// Force an immediate release check after a deprecated-version exit.
    pub async fn refresh(&self) -> Result<String, Error> {
        self.templates.lock().await.force = true;
        Ok(self.current().await?.version)
    }
    /// Create a limited slice without launching a process. Callers must already
    /// hold a reservation; this also supports privileged backend verification.
    pub async fn create_slice(&self, spec: &SliceSpec) -> Result<(), Error> {
        self.systemd.slice(spec).await
    }
    /// Start a limited test service, using the same unit properties as the runner.
    /// This is intended for privileged host verification, never workflow routing.
    pub async fn start_probe(
        &self,
        spec: SliceSpec,
        executable: &str,
        arguments: Vec<String>,
    ) -> Result<(), Error> {
        self.systemd.slice(&spec).await?;
        self.systemd
            .start_command(
                spec.job_id,
                std::path::Path::new("/tmp"),
                &self.user.name,
                ProcessCommand {
                    executable,
                    arguments,
                    environment: vec![],
                    environment_files: vec![],
                    workspace_home: None,
                },
            )
            .await
    }
}
impl NodeBackend for LinuxBackend {
    fn initialize(&self) -> NodeFuture<'_, ()> {
        Box::pin(async {
            require_root()?;
            let total = cgroup::counter(&fs::read_to_string("/proc/meminfo")?, "MemTotal:")?
                .checked_mul(1024)
                .ok_or(Error::Config)?;
            let headroom = (total / 20).max(256 * 1024 * 1024);
            if self.settings.ci.memory_max_bytes > total.saturating_sub(headroom) {
                return Err(Error::HostBudget);
            }
            for root in [&self.settings.templates_dir, &self.settings.runners_dir] {
                fs::create_dir_all(root)?;
                if fs::symlink_metadata(root)?.is_symlink() || fs::metadata(root)?.uid() != 0 {
                    return Err(Error::Config);
                }
                fs::set_permissions(root, fs::Permissions::from_mode(0o711))?;
            }
            self.systemd.parent(&self.settings.ci).await
        })
    }
    fn pressure(&self) -> NodeFuture<'_, Pressure> {
        Box::pin(async { cgroup::host_pressure() })
    }
    fn template_version(&self) -> NodeFuture<'_, String> {
        Box::pin(async { Ok(self.current().await?.version) })
    }
    fn refresh_template(&self) -> NodeFuture<'_, String> {
        Box::pin(self.refresh())
    }
    fn prepare<'a>(&'a self, plan: &'a JobPlan) -> NodeFuture<'a, ()> {
        Box::pin(async move {
            if plan.directory
                != self
                    .settings
                    .runners_dir
                    .join(format!("j{}", plan.slice.job_id))
            {
                return Err(Error::Config);
            }
            self.systemd.slice(&plan.slice).await?;
            if self.systemd.inspect(plan.slice.job_id).await? == ProcessState::Running {
                return Ok(());
            }
            let ready = plan.directory.join(".runwell-prepared");
            if fs::read_to_string(&ready).is_ok_and(|v| v == plan.template_version) {
                return Ok(());
            }
            runwell_runner::remove_install(&self.settings.runners_dir, &plan.directory)?;
            let template = runwell_runner::stage_template(
                &self.releases,
                &self.settings.templates_dir,
                &plan.template_version,
                if plan.template_version == self.settings.runner.version {
                    self.settings.runner.sha256.as_deref()
                } else {
                    None
                },
                &self.user,
            )
            .await?;
            runwell_runner::clone_install(&template, &plan.directory)?;
            self.user.own_install(&plan.directory)?;
            fs::write(ready, &plan.template_version)?;
            Ok(())
        })
    }
    fn prepare_workspace<'a>(&'a self, job: &'a runwell_store::Job) -> NodeFuture<'a, ()> {
        Box::pin(self.workspaces.prepare(job))
    }
    fn bind_workspace(
        &self,
        id: u64,
        execution: runwell_workspace::Execution,
    ) -> NodeFuture<'_, ()> {
        Box::pin(self.workspaces.bind(id, execution))
    }
    fn reconcile_workspaces<'a>(
        &'a self,
        retained: &'a std::collections::HashSet<u64>,
    ) -> NodeFuture<'a, ()> {
        Box::pin(self.workspaces.reconcile(retained.clone()))
    }
    fn harvest_workspace<'a>(&'a self, job: &'a runwell_store::Job) -> NodeFuture<'a, ()> {
        Box::pin(async move {
            // Stop all descendants before reading cache files. Docker cleanup
            // must also have completed before invoking this lifecycle hook.
            self.systemd.stop(&service_unit(job.id as u64)).await?;
            self.systemd.stop(&slice_unit(job.id as u64)).await?;
            self.workspaces.harvest(job).await;
            Ok(())
        })
    }
    fn start<'a>(&'a self, plan: &'a JobPlan, launch: &'a LaunchSpec) -> NodeFuture<'a, ()> {
        Box::pin(async move {
            if launch.install_dir != plan.directory
                || plan.directory
                    != self
                        .settings
                        .runners_dir
                        .join(format!("j{}", plan.slice.job_id))
            {
                return Err(Error::Config);
            }
            self.systemd
                .start(
                    plan.slice.job_id,
                    &plan.directory,
                    &self.user.name,
                    &launch.jit_config,
                    &self.workspace_home(plan)?,
                )
                .await
        })
    }
    fn inspect(&self, id: u64) -> NodeFuture<'_, ProcessState> {
        Box::pin(self.systemd.inspect(id))
    }
    fn measure(&self, id: u64) -> NodeFuture<'_, JobMeasurement> {
        Box::pin(async move {
            let mut measurement = cgroup::measure(id)?;
            measurement.duration_ms = self.systemd.duration_ms(id).await?;
            Ok(measurement)
        })
    }
    fn stop_runner(&self, id: u64) -> NodeFuture<'_, ()> {
        Box::pin(async move { self.systemd.stop(&service_unit(id)).await })
    }
    fn inventory(&self) -> NodeFuture<'_, Vec<LocalJob>> {
        Box::pin(async {
            let mut ids: BTreeSet<_> = self.systemd.inventory().await?.into_iter().collect();
            for entry in fs::read_dir(&self.settings.runners_dir)? {
                let entry = entry?;
                if let Some(id) = entry
                    .file_name()
                    .to_str()
                    .and_then(|v| v.strip_prefix('j'))
                    .and_then(|v| v.parse::<u64>().ok())
                {
                    ids.insert(id);
                }
            }
            Ok(ids.into_iter().map(|id| LocalJob { id }).collect())
        })
    }
    fn cleanup(&self, id: u64) -> NodeFuture<'_, ()> {
        Box::pin(async move {
            self.systemd.stop(&service_unit(id)).await?;
            self.systemd.stop(&slice_unit(id)).await?;
            credentials::remove(id)?;
            self.workspaces.teardown(id).await?;
            runwell_runner::remove_install(
                &self.settings.runners_dir,
                &self.settings.runners_dir.join(format!("j{id}")),
            )?;
            Ok(())
        })
    }
}
