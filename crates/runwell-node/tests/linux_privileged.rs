#![cfg(target_os = "linux")]
use runwell_node::{linux::LinuxBackend, *};
use runwell_scaleset::RunnerReference;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
static HOST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct NoRegistrations;
impl RunnerApi for NoRegistrations {
    fn acquire(&self, _: i64, _: i64) -> NodeFuture<'_, bool> {
        Box::pin(async { Err(Error::Github) })
    }
    fn create<'a>(&'a self, _: i64, _: &'a str) -> NodeFuture<'a, Registration> {
        Box::pin(async { Err(Error::Github) })
    }
    fn lookup<'a>(&'a self, _: &'a str) -> NodeFuture<'a, Option<RunnerReference>> {
        Box::pin(async { Ok(None) })
    }
    fn delete(&self, _: i64) -> NodeFuture<'_, bool> {
        Box::pin(async { Ok(true) })
    }
}
async fn backend() -> (tempfile::TempDir, runwell_config::Config, Arc<LinuxBackend>) {
    linux::require_root().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut config =
        runwell_config::Config::from_toml(include_str!("../../../examples/runwell.toml")).unwrap();
    config.node.state_dir = dir.path().into();
    config.controller.database = dir.path().join("state.sqlite");
    config.node.cpu_slots = 1;
    config.node.memory_bytes = 128 * 1024 * 1024;
    config.controller.classes.truncate(1);
    config.controller.classes[0].memory_high_bytes = 16 * 1024 * 1024;
    config.controller.classes[0].memory_max_bytes = 64 * 1024 * 1024;
    let settings = config.standalone.as_mut().unwrap();
    settings.ci.memory_max_bytes = 256 * 1024 * 1024;
    settings.runner_user = "nobody".into();
    settings.templates_dir = dir.path().join("templates");
    settings.runners_dir = dir.path().join("runners");
    settings.stop_seconds = 5;
    let backend = Arc::new(LinuxBackend::connect(settings.clone()).await.unwrap());
    backend.initialize().await.unwrap();
    (dir, config, backend)
}
fn limits(id: u64, max: u64) -> SliceSpec {
    SliceSpec {
        job_id: id,
        memory_high: max / 2,
        memory_max: max,
        cpu_weight: 100,
        tasks_max: 64,
    }
}
async fn exited(backend: &LinuxBackend, id: u64) -> ProcessState {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let state = backend.inspect(id).await.unwrap();
            if matches!(state, ProcessState::Exited(_)) {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
#[ignore = "requires Linux root, systemd PID 1, unified cgroup v2 and python3"]
async fn systemd_slice_stats_teardown_and_orphan_reconciliation() {
    let _guard = HOST.lock().await;
    let (_dir, config, backend) = backend().await;
    let id = 8_000_001;
    let source = "import time\nend=time.monotonic()+2\nwhile time.monotonic()<end: pass";
    backend
        .start_probe(
            limits(id, 128 * 1024 * 1024),
            "/usr/bin/python3",
            vec!["/usr/bin/python3".into(), "-c".into(), source.into()],
        )
        .await
        .unwrap();
    assert_eq!(exited(&backend, id).await, ProcessState::Exited(Some(0)));
    let sample = backend.measure(id).await.unwrap();
    assert!(sample.cpu_usec > 0);
    assert!(sample.memory_peak > 0);
    let cgroup = format!("/sys/fs/cgroup/ci.slice/ci-rw.slice/{}", slice_unit(id));
    assert_eq!(
        std::fs::read_to_string(format!("{cgroup}/memory.swap.max"))
            .unwrap()
            .trim(),
        "0"
    );
    backend.cleanup(id).await.unwrap();
    backend.cleanup(id).await.unwrap();
    assert_eq!(backend.inspect(id).await.unwrap(), ProcessState::Absent);
    assert!(!std::path::Path::new(&cgroup).exists());
    // A process with no durable registry entry is an orphan, not an adopted job.
    backend
        .start_probe(
            limits(id, 128 * 1024 * 1024),
            "/bin/sleep",
            vec!["/bin/sleep".into(), "30".into()],
        )
        .await
        .unwrap();
    let orphan_dir = config
        .standalone
        .as_ref()
        .unwrap()
        .runners_dir
        .join(format!("j{id}"));
    std::fs::create_dir(&orphan_dir).unwrap();
    let store = runwell_store::Store::open(config.controller.database.to_str().unwrap())
        .await
        .unwrap();
    let mut controller = Controller::new(
        &config,
        BTreeMap::from([(42, config.controller.classes[0].clone())]),
        store,
        backend.clone(),
        Arc::new(NoRegistrations),
    )
    .unwrap();
    controller.reconcile().await.unwrap();
    controller.reconcile().await.unwrap();
    assert!(!orphan_dir.exists());
    assert_eq!(backend.inspect(id).await.unwrap(), ProcessState::Absent);
}
#[tokio::test]
#[ignore = "requires Linux root, systemd PID 1, unified cgroup v2 and python3"]
async fn memory_max_kills_oversized_process_and_records_oom() {
    let _guard = HOST.lock().await;
    let (_dir, _config, backend) = backend().await;
    let id = 8_000_002;
    backend
        .start_probe(
            // MemoryHigh below MemoryMax throttles the allocation instead of letting it
            // reach the OOM killer within the test window.
            SliceSpec {
                memory_high: 32 * 1024 * 1024,
                ..limits(id, 32 * 1024 * 1024)
            },
            "/usr/bin/python3",
            vec![
                "/usr/bin/python3".into(),
                "-c".into(),
                "a=bytearray(256*1024*1024); import time; time.sleep(2)".into(),
            ],
        )
        .await
        .unwrap();
    let state = exited(&backend, id).await;
    let sample = backend.measure(id).await.unwrap();
    backend.cleanup(id).await.unwrap();
    assert_ne!(state, ProcessState::Exited(Some(0)));
    assert!(sample.oom_kills > 0);
    assert!(sample.infra_signal);
    assert!(sample.memory_current <= 32 * 1024 * 1024);
}
#[test]
#[ignore = "requires a root Linux test harness to spawn an unprivileged child"]
fn nonroot_node_fails_fast() {
    use std::os::unix::process::CommandExt;
    if std::env::var_os("RUNWELL_NONROOT_PROBE").is_some() {
        assert!(matches!(linux::require_root(), Err(Error::RootRequired)));
        return;
    }
    linux::require_root().unwrap();
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let executable = directory.path().join("nonroot-probe");
    std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let status = std::process::Command::new(executable)
        .args(["--ignored", "--exact", "nonroot_node_fails_fast"])
        .env("RUNWELL_NONROOT_PROBE", "1")
        .uid(65534)
        .gid(65534)
        .status()
        .unwrap();
    assert!(status.success());
}

#[tokio::test]
#[ignore = "requires Linux root and systemd; uses a local listener stub"]
async fn jit_environment_is_private_and_home_is_per_job() {
    use std::os::unix::fs::PermissionsExt;
    use zbus_systemd::systemd1::{ManagerProxy, ServiceProxy};
    let _guard = HOST.lock().await;
    let (temp, config, backend) = backend().await;
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o711)).unwrap();
    let settings = config.standalone.as_ref().unwrap();
    let id = 8_000_003;
    let directory = settings.runners_dir.join(format!("j{id}"));
    std::fs::create_dir_all(directory.join("bin")).unwrap();
    std::fs::create_dir(directory.join("home")).unwrap();
    let executable = directory.join("bin/Runner.Listener");
    std::fs::write(&executable,b"#!/bin/sh\ntest \"$ACTIONS_RUNNER_INPUT_JITCONFIG\" = synthetic-jit && test \"$HOME\" = \"$PWD/home\"\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o555)).unwrap();
    runwell_runner::RunnerUser::resolve("nobody")
        .unwrap()
        .own_install(&directory)
        .unwrap();
    let plan = JobPlan {
        slice: limits(id, 64 * 1024 * 1024),
        directory: directory.clone(),
        template_version: "probe".into(),
    };
    backend.create_slice(&plan.slice).await.unwrap();
    backend
        .start(
            &plan,
            &runwell_runner::LaunchSpec {
                agent_id: 42,
                install_dir: directory,
                jit_config: secrecy::SecretString::from("synthetic-jit"),
            },
        )
        .await
        .unwrap();
    assert_eq!(exited(&backend, id).await, ProcessState::Exited(Some(0)));
    let connection = zbus::Connection::system().await.unwrap();
    let path = ManagerProxy::new(&connection)
        .await
        .unwrap()
        .get_unit(service_unit(id))
        .await
        .unwrap();
    let service = ServiceProxy::builder(&connection)
        .path(path)
        .unwrap()
        .build()
        .await
        .unwrap();
    assert!(
        service
            .environment()
            .await
            .unwrap()
            .iter()
            .all(|v| !v.contains("synthetic-jit"))
    );
    let credential = format!("/run/runwell/j{id}.env");
    assert_eq!(
        std::fs::metadata(&credential).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata("/run/runwell")
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    backend.cleanup(id).await.unwrap();
    assert!(!std::path::Path::new(&credential).exists());
}
