use super::systemd::{ProcessCommand, Systemd};
use crate::{ProcessState, SliceSpec, service_unit, slice_unit};
use runwell_dockerproxy::{CgroupDriver, DockerProxyConfig, Proxy, ProxySpec, Rewriter};
use runwell_workspace::{Cache, Mode, Owner};
use std::{fs, os::unix::fs::PermissionsExt, time::Duration};
use tokio::{io::AsyncWriteExt, net::UnixListener, process::Command};

#[tokio::test]
#[ignore = "requires Linux root, systemd PID 1, cgroup v2, overlayfs and python3"]
async fn masked_overlay_home_keeps_job_proxy_socket_reachable() {
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o711)).unwrap();
    let workspace = temp.path().join("workspaces");
    let mut cache = Cache::open(
        runwell_config::WorkspaceConfig {
            cache_root: Some(temp.path().join("cache")),
            ..Default::default()
        },
        &workspace,
        Owner {
            uid: 65534,
            gid: 65534,
        },
    )
    .unwrap();
    assert_eq!(cache.mode(), Mode::Overlay);
    let home = cache.prepare(1, None).unwrap();
    let other = cache.prepare(2, None).unwrap();
    fs::write(other.join("private"), b"private").unwrap();
    let settings = DockerProxyConfig {
        run_dir: temp.path().join("proxy"),
        upstream_socket: temp.path().join("up.sock"),
        ..Default::default()
    };
    let listener = UnixListener::bind(&settings.upstream_socket).unwrap();
    let upstream = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(stream.read_u8().await.unwrap());
            assert!(request.len() < 4096);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
            .await
            .unwrap();
    });
    let spec = ProxySpec {
        job_id: 1,
        node: "socket-probe".into(),
        cgroup_parent: "ci-rw-j1.slice".into(),
        memory_max: Some(1024 * 1024),
        uid: 65534,
        gid: 65534,
    };
    let socket = spec.socket(&settings);
    let proxy = Proxy::start(Rewriter::new(spec, settings, CgroupDriver::Systemd).unwrap())
        .await
        .unwrap();
    let id = 8_300_000 + u64::from(std::process::id());
    let systemd = Systemd::connect(5).await.unwrap();
    systemd
        .slice(&SliceSpec {
            job_id: id,
            memory_high: 64 * 1024 * 1024,
            memory_max: 128 * 1024 * 1024,
            cpu_weight: 100,
            tasks_max: 64,
        })
        .await
        .unwrap();
    let logs = temp.path().join("probe");
    fs::create_dir(&logs).unwrap();
    runwell_runner::RunnerUser::resolve("nobody")
        .unwrap()
        .own_install(&logs)
        .unwrap();
    fs::write(home.join("data"), b"own").unwrap();
    let mut peer = Command::new("/bin/sleep")
        .arg("120")
        .uid(65534)
        .gid(65534)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let peer_file = format!(
        "/proc/{}/root{}/private",
        peer.id().unwrap(),
        other.display()
    );
    let script = r#"
import os, socket, sys
home, proxy, *blocked = sys.argv[1:]
print("checking HOME and workspace isolation", flush=True)
assert os.getuid() == 65534
assert os.environ["HOME"] == home
with open(home + "/data") as f: assert f.read() == "own"
with open(home + "/written", "w") as f: f.write("own")
for path in blocked:
    for mode in ("r", "w"):
        try:
            with open(path, mode): pass
        except (FileNotFoundError, PermissionError):
            pass
        else:
            raise AssertionError(("peer path accessible", path, mode))
print("connecting to own proxy socket", flush=True)
s = socket.socket(socket.AF_UNIX)
s.settimeout(10)
s.connect(proxy)
s.sendall(b"GET /_ping HTTP/1.1\r\nHost: docker\r\nConnection: close\r\n\r\n")
response = b""
while True:
    part = s.recv(4096)
    if not part: break
    response += part
assert response.endswith(b"OK"), response
print("HOME, isolation and proxy socket verified", flush=True)
"#;
    // Use the production launcher, including PrivateUsers, socket restrictions
    // and workspace mounts. unshare --map-users uses newuidmap on some hosts,
    // which needs subordinate-ID grants even for root and does not model systemd.
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        systemd
            .start_command(
                id,
                temp.path(),
                "nobody",
                ProcessCommand {
                    executable: "/bin/sh",
                    arguments: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        r#"exec >"$1/stdout" 2>"$1/stderr"; shift; exec /usr/bin/python3 -u -c "$@""#.into(),
                        "probe".into(),
                        logs.display().to_string(),
                        script.into(),
                        home.display().to_string(),
                        socket.display().to_string(),
                        other.join("private").display().to_string(),
                        workspace.join("upper/2/upper/home/private").display().to_string(),
                        peer_file,
                    ],
                    environment: vec![format!("HOME={}", home.display())],
                    environment_files: vec![],
                    workspace_home: Some(&home),
                },
            )
            .await?;
        loop {
            let state = systemd.inspect(id).await?;
            if matches!(state, ProcessState::Exited(_)) {
                return Ok::<_, crate::Error>(state);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    let success = matches!(result, Ok(Ok(ProcessState::Exited(Some(0)))));
    let diagnostics = if success {
        String::new()
    } else {
        unit_diagnostics(&service_unit(id)).await
    };
    let stdout =
        fs::read_to_string(logs.join("stdout")).unwrap_or_else(|e| format!("unavailable: {e}"));
    let stderr =
        fs::read_to_string(logs.join("stderr")).unwrap_or_else(|e| format!("unavailable: {e}"));
    let stopped = systemd.stop(&service_unit(id)).await;
    let removed = systemd.stop(&slice_unit(id)).await;
    let peer_stopped = peer.kill().await;
    let proxy_stopped = proxy.shutdown().await;
    if !success {
        upstream.abort();
    }
    let written = fs::read(home.join("written"));
    let private = fs::read(other.join("private"));
    let unmounted = (cache.teardown(1), cache.teardown(2));
    assert!(
        success,
        "workspace child: {result:?}\nstdout:\n{stdout}\nstderr:\n{stderr}\n{diagnostics}\ncleanup: {stopped:?}, {removed:?}, {peer_stopped:?}, {proxy_stopped:?}, {unmounted:?}"
    );
    stopped.unwrap();
    removed.unwrap();
    peer_stopped.unwrap();
    proxy_stopped.unwrap();
    unmounted.0.unwrap();
    unmounted.1.unwrap();
    upstream.await.unwrap();
    assert_eq!(written.unwrap(), b"own");
    assert_eq!(private.unwrap(), b"private");
}

async fn unit_diagnostics(unit: &str) -> String {
    let mut diagnostics = String::new();
    for (command, args) in [
        ("systemctl", vec!["show", "--no-pager", unit]),
        (
            "journalctl",
            vec!["--no-pager", "--boot", "-n", "80", "--unit", unit],
        ),
    ] {
        let output = tokio::time::timeout(
            Duration::from_secs(5),
            Command::new(command).args(args).kill_on_drop(true).output(),
        )
        .await;
        match output {
            Ok(Ok(output)) => diagnostics.push_str(&format!(
                "{command} ({}):\n{}\n{}\n",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )),
            result => diagnostics.push_str(&format!("{command}: {result:?}\n")),
        }
    }
    diagnostics
}
