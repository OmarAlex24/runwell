use crate::{Error, ProcessState, SliceSpec, service_unit, slice_unit};
use runwell_config::CiLimits;
use secrecy::SecretString;
use std::{path::Path, time::Duration};
use zbus::{
    Connection,
    zvariant::{OwnedValue, Value},
};
use zbus_systemd::systemd1::{ManagerProxy, ServiceProxy, UnitProxy};

pub(super) struct Systemd {
    pub connection: Connection,
    pub stop_seconds: u64,
}
pub(super) struct ProcessCommand<'a> {
    pub executable: &'a str,
    pub arguments: Vec<String>,
    pub environment: Vec<String>,
    pub environment_files: Vec<(String, bool)>,
    pub workspace_home: Option<&'a Path>,
}
pub(super) type Properties = Vec<(String, OwnedValue)>;
pub(super) fn property<'a>(
    name: &str,
    value: impl Into<Value<'a>>,
) -> Result<(String, OwnedValue), Error> {
    Ok((
        name.into(),
        OwnedValue::try_from(value.into()).map_err(|_| Error::Systemd)?,
    ))
}
fn missing(error: &zbus::Error) -> bool {
    matches!(error, zbus::Error::MethodError(name,_,_) if name.as_str() == "org.freedesktop.systemd1.NoSuchUnit")
}
impl Systemd {
    pub async fn connect(stop_seconds: u64) -> Result<Self, Error> {
        Ok(Self {
            connection: Connection::system().await.map_err(|_| Error::Systemd)?,
            stop_seconds,
        })
    }
    async fn manager(&self) -> Result<ManagerProxy<'_>, Error> {
        ManagerProxy::new(&self.connection)
            .await
            .map_err(|_| Error::Systemd)
    }
    pub async fn parent(&self, limits: &CiLimits) -> Result<(), Error> {
        use std::io::Write;
        let ceiling = Path::new("/sys/fs/cgroup/ci.slice/memory.max");
        if ceiling.try_exists()? && !self.inventory().await?.is_empty() {
            let previous = std::fs::read_to_string(ceiling)?;
            let previous = previous.trim().parse::<u64>().unwrap_or(u64::MAX);
            if limits.memory_max_bytes < previous {
                return Err(Error::ParentLimit);
            }
        }
        let unit_path = Path::new("/etc/systemd/system/ci.slice");
        if !unit_path.exists() {
            let mut unit = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(unit_path)?;
            unit.write_all(b"[Unit]\nDescription=Runwell CI resource boundary\n[Slice]\n")?;
            unit.sync_all()?;
        }
        let manager = self.manager().await?;
        manager.reload().await.map_err(|_| Error::Systemd)?;
        manager
            .load_unit("ci.slice".into())
            .await
            .map_err(|_| Error::Systemd)?;
        manager
            .set_unit_properties(
                "ci.slice".into(),
                false,
                vec![
                    property("CPUWeight", u64::from(limits.cpu_weight))?,
                    property("MemoryMax", limits.memory_max_bytes)?,
                    property("CPUAccounting", true)?,
                    property("MemoryAccounting", true)?,
                    property("IOAccounting", true)?,
                ],
            )
            .await
            .map_err(|_| Error::Systemd)?;
        manager
            .start_unit("ci.slice".into(), "replace".into())
            .await
            .map_err(|_| Error::Systemd)?;
        self.wait_active("ci.slice").await?;
        if self.unit_path("ci-rw.slice").await?.is_none() {
            self.transient(
                "ci-rw.slice",
                vec![
                    property("Description", "Runwell job slices")?,
                    property("IOAccounting", true)?,
                ],
            )
            .await?;
        }
        self.wait_active("ci-rw.slice").await
    }
    pub async fn slice(&self, spec: &SliceSpec) -> Result<(), Error> {
        spec.validate()?;
        let name = spec.unit();
        if self.unit_path(&name).await?.is_none() {
            self.transient(
                &name,
                vec![
                    property("Description", "Runwell job")?,
                    property("MemoryHigh", spec.memory_high)?,
                    property("MemoryMax", spec.memory_max)?,
                    property("MemorySwapMax", 0_u64)?,
                    property("CPUWeight", u64::from(spec.cpu_weight))?,
                    property("TasksMax", spec.tasks_max)?,
                    property("CPUAccounting", true)?,
                    property("MemoryAccounting", true)?,
                    property("IOAccounting", true)?,
                ],
            )
            .await?;
        }
        self.wait_active(&name).await
    }
    async fn transient(&self, name: &str, properties: Properties) -> Result<(), Error> {
        self.manager()
            .await?
            .start_transient_unit(name.into(), "fail".into(), properties, vec![])
            .await
            .map_err(|_| Error::Systemd)?;
        Ok(())
    }
    pub async fn start(
        &self,
        id: u64,
        directory: &Path,
        user: &str,
        secret: &SecretString,
        home: &Path,
        environment: Vec<String>,
    ) -> Result<(), Error> {
        let executable = directory.join("bin/Runner.Listener");
        let exe = executable.to_str().ok_or(Error::Config)?;
        let mut env = vec![
            format!("HOME={}", home.display()),
            "ACTIONS_RUNNER_RETURN_VERSION_DEPRECATED_EXIT_CODE=1".into(),
            "PATH=/usr/local/bin:/usr/bin:/bin".into(),
        ];
        env.extend(environment);
        let environment_file = super::credentials::write(id, secret)?;
        self.start_command(
            id,
            directory,
            user,
            ProcessCommand {
                executable: exe,
                arguments: vec![exe.into(), "run".into()],
                environment: env,
                environment_files: vec![(environment_file, false)],
                workspace_home: (home != directory.join("home")).then_some(home),
            },
        )
        .await
    }
    pub async fn start_command(
        &self,
        id: u64,
        directory: &Path,
        user: &str,
        command: ProcessCommand<'_>,
    ) -> Result<(), Error> {
        if self.unit_path(&slice_unit(id)).await?.is_none() {
            return Err(Error::Config);
        }
        if self.unit_path(&service_unit(id)).await?.is_some() {
            return Ok(());
        }
        let exec = Value::new(vec![(
            command.executable.to_owned(),
            command.arguments,
            false,
        )]);
        let mut properties = vec![
            property("Description", "Runwell ephemeral runner")?,
            property("Slice", slice_unit(id))?,
            property("User", user)?,
            property("WorkingDirectory", directory.to_str().ok_or(Error::Config)?)?,
            property("ExecStart", exec)?,
            property("Environment", Value::new(command.environment))?,
            property("EnvironmentFiles", Value::new(command.environment_files))?,
            property("KillMode", "control-group")?,
            property(
                "TimeoutStopUSec",
                self.stop_seconds.saturating_mul(1_000_000),
            )?,
            property("RemainAfterExit", true)?,
            property("Type", "exec")?,
            property("Restart", "no")?,
            property("UMask", 0o077_u32)?,
            property("NoNewPrivileges", true)?,
            property("ProtectControlGroups", true)?,
            // Direct privileged sockets remain inaccessible to runners.
            property(
                "InaccessiblePaths",
                Value::new(vec![
                    "-/run/docker.sock".to_owned(),
                    "-/run/containerd/containerd.sock".to_owned(),
                ]),
            )?,
            property("StandardOutput", "null")?,
            property("StandardError", "null")?,
        ];
        properties.extend(super::workspace_unit::properties(command.workspace_home)?);
        self.transient(&service_unit(id), properties).await
    }
    async fn unit_path(
        &self,
        name: &str,
    ) -> Result<Option<zbus::zvariant::OwnedObjectPath>, Error> {
        match self.manager().await?.get_unit(name.into()).await {
            Ok(path) => Ok(Some(path)),
            Err(e) if missing(&e) => Ok(None),
            Err(_) => Err(Error::Systemd),
        }
    }
    async fn unit<'a>(
        &'a self,
        path: zbus::zvariant::OwnedObjectPath,
    ) -> Result<UnitProxy<'a>, Error> {
        UnitProxy::builder(&self.connection)
            .path(path)
            .map_err(|_| Error::Systemd)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .map_err(|_| Error::Systemd)
    }
    pub async fn inspect(&self, id: u64) -> Result<ProcessState, Error> {
        let Some(path) = self.unit_path(&service_unit(id)).await? else {
            return Ok(ProcessState::Absent);
        };
        let unit = self.unit(path.clone()).await?;
        let service = ServiceProxy::builder(&self.connection)
            .path(path)
            .map_err(|_| Error::Systemd)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .map_err(|_| Error::Systemd)?;
        if service.slice().await.map_err(|_| Error::Systemd)? != slice_unit(id) {
            return Err(Error::Config);
        }
        let state = unit.active_state().await.map_err(|_| Error::Systemd)?;
        let sub = unit.sub_state().await.map_err(|_| Error::Systemd)?;
        if matches!(state.as_str(), "activating" | "deactivating" | "reloading")
            || (state == "active" && sub != "exited")
        {
            return Ok(ProcessState::Running);
        }
        let code = service.exec_main_code().await.map_err(|_| Error::Systemd)?;
        let status = service
            .exec_main_status()
            .await
            .map_err(|_| Error::Systemd)?;
        Ok(ProcessState::Exited(if code == 1 {
            Some(status)
        } else {
            None
        }))
    }
    pub async fn duration_ms(&self, id: u64) -> Result<u64, Error> {
        let Some(path) = self.unit_path(&service_unit(id)).await? else {
            return Ok(0);
        };
        let service = ServiceProxy::builder(&self.connection)
            .path(path)
            .map_err(|_| Error::Systemd)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .map_err(|_| Error::Systemd)?;
        let start = service
            .exec_main_start_timestamp_monotonic()
            .await
            .map_err(|_| Error::Systemd)?;
        let end = service
            .exec_main_exit_timestamp_monotonic()
            .await
            .map_err(|_| Error::Systemd)?;
        Ok(end.saturating_sub(start) / 1000)
    }
    async fn wait_active(&self, name: &str) -> Result<(), Error> {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Some(path) = self.unit_path(name).await? {
                    let state = self
                        .unit(path)
                        .await?
                        .active_state()
                        .await
                        .map_err(|_| Error::Systemd)?;
                    if state == "active" {
                        return Ok(());
                    }
                    if state == "failed" {
                        return Err(Error::Systemd);
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .map_err(|_| Error::Timeout)?
    }
    pub async fn stop(&self, name: &str) -> Result<(), Error> {
        let manager = self.manager().await?;
        if self.unit_path(name).await?.is_none() {
            return Ok(());
        }
        match manager.stop_unit(name.into(), "replace".into()).await {
            Ok(_) => {}
            Err(e) if missing(&e) => return Ok(()),
            Err(_) => return Err(Error::Systemd),
        }
        tokio::time::timeout(
            Duration::from_secs(self.stop_seconds.saturating_add(30)),
            async {
                loop {
                    let Some(path) = self.unit_path(name).await? else {
                        return Ok(());
                    };
                    let state = self
                        .unit(path)
                        .await?
                        .active_state()
                        .await
                        .map_err(|_| Error::Systemd)?;
                    if state == "failed" {
                        manager
                            .reset_failed_unit(name.into())
                            .await
                            .map_err(|_| Error::Systemd)?;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
        )
        .await
        .map_err(|_| Error::Timeout)?
    }
    pub async fn inventory(&self) -> Result<Vec<u64>, Error> {
        let units = self
            .manager()
            .await?
            .list_units_by_patterns(
                vec![],
                vec!["ci-rw-j*.slice".into(), "rw-j*.service".into()],
            )
            .await
            .map_err(|_| Error::Systemd)?;
        Ok(units.into_iter().filter_map(|u| parse_id(&u.0)).collect())
    }
}
fn parse_id(name: &str) -> Option<u64> {
    name.strip_prefix("ci-rw-j")
        .and_then(|v| v.strip_suffix(".slice"))
        .or_else(|| {
            name.strip_prefix("rw-j")
                .and_then(|v| v.strip_suffix(".service"))
        })
        .and_then(|v| v.parse().ok())
}
