#![cfg(target_os = "linux")]
use runwell_node::{linux::LinuxBackend, *};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
};

// A host-local control daemon, so the outage test never stops the host's Docker.
async fn daemon(path: &Path) -> tokio::task::JoinHandle<()> {
    let listener = UnixListener::bind(path).unwrap();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                if stream.read(&mut byte).await.unwrap() == 0 {
                    break;
                }
                header.push(byte[0]);
            }
            if header.is_empty() {
                continue;
            }
            let header = String::from_utf8(header).unwrap();
            let body = if header.contains("/info ") {
                r#"{"CgroupDriver":"systemd"}"#
            } else {
                r#"{"ApiVersion":"1.47"}"#
            };
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    })
}

#[tokio::test]
#[ignore = "requires root Linux, systemd and cgroup v2"]
async fn unavailable_docker_does_not_block_runner_or_local_teardown() {
    linux::require_root().unwrap();
    for initially_available in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let config =
            runwell_config::Config::from_toml(include_str!("../../../examples/runwell.toml"))
                .unwrap();
        let mut settings = config.standalone.unwrap();
        settings.runner_user = "nobody".into();
        settings.runners_dir = dir.path().join("runners");
        settings.docker_proxy.run_dir = dir.path().join("run");
        settings.docker_proxy.upstream_socket = dir.path().join("up.sock");
        settings.docker_proxy.stop_seconds = 1;
        let daemon = if initially_available {
            Some(daemon(&settings.docker_proxy.upstream_socket).await)
        } else {
            None
        };
        let backend = LinuxBackend::connect(settings.clone())
            .await
            .unwrap()
            .with_docker_proxy("outage-test".into());
        let id = 8_200_000 + 2 * u64::from(std::process::id()) + u64::from(initially_available);
        let plan = JobPlan {
            slice: SliceSpec {
                job_id: id,
                memory_high: 128 * 1024 * 1024,
                memory_max: 256 * 1024 * 1024,
                cpu_weight: 100,
                tasks_max: 128,
            },
            directory: settings.runners_dir.join(format!("j{id}")),
            template_version: "prepared-test".into(),
        };
        fs::create_dir_all(plan.directory.join("bin")).unwrap();
        fs::write(
            plan.directory.join(".runwell-prepared"),
            &plan.template_version,
        )
        .unwrap();
        let executable = plan.directory.join("bin/Runner.Listener");
        let check = if initially_available { "-n" } else { "-z" };
        fs::write(&executable, format!("#!/bin/sh\ntest {check} \"$DOCKER_HOST\" || exit 21\ntest {check} \"$TESTCONTAINERS_DOCKER_SOCKET_OVERRIDE\" || exit 22\n")).unwrap();
        fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
        backend.prepare(&plan).await.unwrap();
        let cgroup = format!("/sys/fs/cgroup/ci.slice/ci-rw.slice/{}", plan.slice.unit());
        fs::write(format!("{cgroup}/memory.max"), "max").unwrap();
        backend.recover(&plan).await.unwrap();
        backend
            .start(
                &plan,
                &runwell_runner::LaunchSpec {
                    agent_id: 1,
                    install_dir: plan.directory.clone(),
                    jit_config: secrecy::SecretString::new("dGVzdA==".into()),
                },
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let ProcessState::Exited(code) = backend.inspect(id).await.unwrap() {
                    assert_eq!(code, Some(0));
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        let credential = format!("/run/runwell/j{id}.env");
        assert!(Path::new(&credential).exists());
        if let Some(daemon) = daemon {
            daemon.abort();
            let _ = daemon.await;
        }
        let stopped = backend.stop_runner(id).await;
        if initially_available {
            assert!(matches!(stopped, Err(Error::DockerProxy(_))));
            assert!(!Path::new(&cgroup).exists());
            assert!(!Path::new(&credential).exists());
            assert!(!plan.directory.exists());
        } else {
            stopped.unwrap();
        }
        let cleaned = backend.cleanup(id).await;
        assert_eq!(cleaned.is_err(), initially_available);
        assert!(!Path::new(&cgroup).exists());
        assert!(!Path::new(&credential).exists());
        assert!(!plan.directory.exists());
        let runtime = settings.docker_proxy.run_dir.join(format!("jobs/{id}"));
        assert_eq!(
            runtime.exists(),
            initially_available,
            "retain only pending Docker cleanup"
        );
    }
}
