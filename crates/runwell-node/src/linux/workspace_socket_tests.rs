use super::workspace_unit;
use runwell_dockerproxy::{CgroupDriver, DockerProxyConfig, Proxy, ProxySpec, Rewriter};
use runwell_workspace::{Cache, Mode, Owner};
use std::{fs, os::unix::fs::PermissionsExt};
use tokio::{io::AsyncWriteExt, net::UnixListener, process::Command};

#[tokio::test]
#[ignore = "requires Linux root, overlayfs, unshare, mount, setpriv and python3"]
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
    // Read the production unit's mask/bind paths so this exercises the same
    // filesystem layout without requiring systemd as PID 1 in the container.
    let properties = workspace_unit::properties(Some(&home)).unwrap();
    let masks: Vec<(String, String)> = properties[0].1.try_clone().unwrap().try_into().unwrap();
    let binds: Vec<(String, String, bool, u64)> =
        properties[1].1.try_clone().unwrap().try_into().unwrap();
    assert!(!socket.starts_with(&masks[0].0));
    let exposed = temp.path().join("exposed");
    fs::create_dir(&exposed).unwrap();
    let script = r#"
set -eu
mount --make-rprivate /
mount --bind "$1" "$2"
mount -t tmpfs -o mode=0711,nodev,nosuid tmpfs "$3"
mkdir -p "$1"
mount --bind "$2" "$1"
umount "$2"
mount -o remount,ro "$3"
exec setpriv --reuid=65534 --regid=65534 --clear-groups --no-new-privs python3 -c '
import os, socket, sys
home, proxy, other = sys.argv[1:]
assert not os.path.exists(other)
with open(home + "/written", "w") as f: f.write("own")
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
' "$1" "$4" "$5"
"#;
    let status = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        Command::new("unshare")
            .args([
                "--user",
                "--map-users=0:0:65535",
                "--map-groups=0:0:65535",
                "--mount",
                "/bin/sh",
                "-c",
                script,
                "probe",
            ])
            .arg(&binds[0].0)
            .arg(exposed)
            .arg(&masks[0].0)
            .arg(socket)
            .arg(other)
            .kill_on_drop(true)
            .status(),
    )
    .await
    .unwrap()
    .unwrap();
    proxy.shutdown().await.unwrap();
    if !status.success() {
        upstream.abort();
    }
    assert!(status.success());
    upstream.await.unwrap();
    assert_eq!(fs::read(home.join("written")).unwrap(), b"own");
    cache.teardown(1).unwrap();
    cache.teardown(2).unwrap();
}
