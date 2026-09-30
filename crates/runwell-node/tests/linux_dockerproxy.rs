#![cfg(target_os = "linux")]
use runwell_node::{linux::LinuxBackend, *};
use serde_json::Value;
use std::{os::unix::fs::PermissionsExt, path::Path, process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command};

async fn docker(socket: &Path, args: &[&str], input: Option<&[u8]>) -> std::process::Output {
    let mut child = Command::new("docker")
        .env("DOCKER_HOST", format!("unix://{}", socket.display()))
        .env("DOCKER_BUILDKIT", "1")
        .env_remove("DOCKER_CONTEXT")
        .env_remove("BUILDX_BUILDER")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input).await.unwrap();
    }
    drop(child.stdin.take());
    tokio::time::timeout(Duration::from_secs(180), child.wait_with_output())
        .await
        .unwrap()
        .unwrap()
}
async fn success(socket: &Path, args: &[&str], input: Option<&[u8]>) -> String {
    let output = docker(socket, args, input).await;
    assert!(
        output.status.success(),
        "docker {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}
#[tokio::test]
#[ignore = "requires root Linux, systemd, cgroup v2, Docker systemd driver and Buildx"]
async fn docker_cli_attribution_exec_socket_build_and_label_scoped_teardown() {
    linux::require_root().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let config =
        runwell_config::Config::from_toml(include_str!("../../../examples/runwell.toml")).unwrap();
    let mut settings = config.standalone.unwrap();
    settings.runner_user = "nobody".into();
    settings.runners_dir = dir.path().join("runners");
    settings.templates_dir = dir.path().join("templates");
    settings.docker_proxy.stop_seconds = 1;
    // Do not modify shared ci.slice limits: M3 tests may run concurrently.
    // A distinct slice ID keeps all mutable job resources independent.
    let id = 8_100_000 + u64::from(std::process::id());
    let socket = settings
        .docker_proxy
        .run_dir
        .join(format!("jobs/{id}/docker.sock"));
    let upstream = settings.docker_proxy.upstream_socket.clone();
    assert_eq!(
        success(&upstream, &["info", "--format", "{{.CgroupDriver}}"], None).await,
        "systemd"
    );
    let backend = LinuxBackend::connect(settings.clone())
        .await
        .unwrap()
        .with_docker_proxy("m4a-test".into());
    let slice = SliceSpec {
        job_id: id,
        memory_high: 512 * 1024 * 1024,
        memory_max: 768 * 1024 * 1024,
        cpu_weight: 100,
        tasks_max: 1024,
    };
    backend.create_slice(&slice).await.unwrap();
    let plan = JobPlan {
        slice,
        directory: settings.runners_dir.join(format!("j{id}")),
        template_version: "unused".into(),
    };
    backend.recover(&plan).await.unwrap();
    // Exercise the real service environment and filesystem permissions as the
    // unprivileged runner. In particular /run/runwell also holds JIT files.
    std::fs::create_dir_all(plan.directory.join("bin")).unwrap();
    for directory in [
        settings.runners_dir.clone(),
        plan.directory.clone(),
        plan.directory.join("bin"),
    ] {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let executable = plan.directory.join("bin/Runner.Listener");
    std::fs::write(&executable, format!(
        "#!/bin/sh\ntest \"$DOCKER_HOST\" = \"unix://{}\" || exit 21\ntest \"$TESTCONTAINERS_DOCKER_SOCKET_OVERRIDE\" = \"{}\" || exit 22\nexec docker version --format '{{{{.Server.Version}}}}'\n",
        socket.display(), socket.display()
    )).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
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
            let state = backend.inspect(id).await.unwrap();
            if let ProcessState::Exited(code) = state {
                assert_eq!(
                    code,
                    Some(0),
                    "runner could not use its injected Docker socket"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();

    assert_eq!(
        std::fs::metadata(format!("/run/runwell/j{id}.env"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let before = backend.measure(id).await.unwrap();
    let network = format!("rw-m4a-net-{id}");
    let volume = format!("rw-m4a-vol-{id}");
    let name = format!("rw-m4a-job-{id}");
    let control_name = format!("rw-m4a-control-{id}");
    let image = format!("rw-m4a-build:{id}");
    success(&socket, &["network", "create", &network], None).await;
    success(&socket, &["volume", "create", &volume], None).await;
    let control = success(
        &upstream,
        &[
            "run",
            "-d",
            "--name",
            &control_name,
            "busybox:1.37",
            "sleep",
            "600",
        ],
        None,
    )
    .await;
    let container = success(
        &socket,
        &[
            "run",
            "-d",
            "--name",
            &name,
            "--network",
            &network,
            "-v",
            &format!("{volume}:/data"),
            "-v",
            "/var/run/docker.sock:/var/run/docker.sock",
            "busybox:1.37",
            "sh",
            "-c",
            "dd if=/dev/zero of=/dev/null bs=1M count=128; sleep 600",
        ],
        None,
    )
    .await;
    let inspect: Value =
        serde_json::from_str(&success(&upstream, &["inspect", &container], None).await).unwrap();
    assert_eq!(
        inspect[0]["Config"]["Labels"]["io.runwell.job"],
        id.to_string()
    );
    assert_eq!(
        inspect[0]["Config"]["Labels"]["io.runwell.node"],
        "m4a-test"
    );
    assert_eq!(inspect[0]["HostConfig"]["Memory"], slice.memory_max);
    let pid = inspect[0]["State"]["Pid"].as_u64().unwrap();
    let membership = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap();
    assert!(
        membership.contains(&format!("/ci.slice/ci-rw.slice/{}/", slice.unit())),
        "{membership}"
    );
    let binds = inspect[0]["HostConfig"]["Binds"].as_array().unwrap();
    assert!(binds.contains(&Value::String(format!(
        "{}:/var/run/docker.sock",
        socket.display()
    ))));
    assert_eq!(
        success(
            &socket,
            &["exec", "-i", &container, "cat"],
            Some(b"stdin through upgrade\n")
        )
        .await,
        "stdin through upgrade"
    );
    // Host source remapping applies to the structured --mount API too. A nested
    // client can use the mounted socket to reach the same attribution policy.
    let nested = success(
        &socket,
        &[
            "create",
            "--mount",
            "type=bind,src=/run/docker.sock,dst=/nested.sock",
            "busybox:1.37",
        ],
        None,
    )
    .await;
    let mounts: Value =
        serde_json::from_str(&success(&upstream, &["inspect", &nested], None).await).unwrap();
    assert_eq!(mounts[0]["Mounts"][0]["Source"], socket.to_str().unwrap());
    let context = dir.path().join("context");
    std::fs::create_dir(&context).unwrap();
    std::fs::write(
        context.join("Dockerfile"),
        "FROM busybox:1.37\nRUN echo buildkit-works > /proof\n",
    )
    .unwrap();
    success(
        &socket,
        &[
            "build",
            "--progress=plain",
            "--no-cache",
            "-t",
            &image,
            context.to_str().unwrap(),
        ],
        None,
    )
    .await;
    // Buildx /grpc is opaque: this checks compatibility, not BuildKit executor
    // attribution. Ordinary containers from the image still use create rewrites.
    assert_eq!(
        success(&socket, &["run", "--rm", &image, "cat", "/proof"], None).await,
        "buildkit-works"
    );
    backend.stop_runner(id).await.unwrap(); // cleanup precedes final slice counters
    let sample = backend.measure(id).await.unwrap();
    assert!(sample.cpu_usec > before.cpu_usec);
    assert!(sample.memory_peak > before.memory_peak);
    assert!(!socket.exists());
    for args in [
        vec!["inspect", container.as_str()],
        vec!["inspect", nested.as_str()],
        vec!["network", "inspect", network.as_str()],
        vec!["volume", "inspect", volume.as_str()],
    ] {
        assert!(!docker(&upstream, &args, None).await.status.success());
    }
    assert_eq!(
        success(
            &upstream,
            &["inspect", "--format", "{{.State.Running}}", &control],
            None
        )
        .await,
        "true"
    );
    backend.cleanup(id).await.unwrap();
    backend.cleanup(id).await.unwrap();
    // Remove only the explicitly created test controls and output image.
    success(&upstream, &["rm", "-f", &control], None).await;
    success(&upstream, &["image", "rm", "-f", &image], None).await;
    assert!(
        !Path::new(&format!(
            "/sys/fs/cgroup/ci.slice/ci-rw.slice/{}",
            slice.unit()
        ))
        .exists()
    );
}
