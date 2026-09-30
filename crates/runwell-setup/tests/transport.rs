#![cfg(unix)]
use runwell_setup::{
    Error,
    model::HostTarget,
    probe,
    ssh::{SshClient, copy_key_command, local_public_key, ssh_args},
};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command, time::Duration};

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("runwell-ssh-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn ssh_arguments_only_use_batch_key_authentication() {
    let host: HostTarget = "builder@example.invalid:2222".parse().unwrap();
    let args = ssh_args(&host, &["sh", "-s"]);
    let args: Vec<_> = args.iter().map(|s| s.to_str().unwrap()).collect();
    assert_eq!(
        args,
        [
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-p",
            "2222",
            "--",
            "builder@example.invalid",
            "sh",
            "-s"
        ]
    );
    for arg in args {
        assert!(!arg.to_lowercase().contains("password"));
    }
}

#[test]
fn targets_reject_option_shell_and_password_injection() {
    for bad in [
        "example.invalid",
        "-o@example.invalid",
        "user@-x",
        "user@host;id",
        "user@host:0",
        "user@host:65536",
        "user:secret@host",
        "user@host\ntrue",
        "user@host$(id)",
        "user@[not-ipv6]",
        "user@::1",
    ] {
        assert!(bad.parse::<HostTarget>().is_err(), "{bad}");
    }
    let host: HostTarget = "user@[2001:db8::1]:2222".parse().unwrap();
    assert_eq!(host.to_string(), "user@[2001:db8::1]:2222");
}

#[test]
fn public_key_help_preserves_port_and_shell_quotes_path() {
    let temp = TempDir::new();
    assert!(local_public_key(&temp.0).is_none());
    fs::create_dir(temp.0.join(".ssh")).unwrap();
    fs::write(temp.0.join(".ssh/id_ed25519.pub"), "public fixture").unwrap();
    assert!(local_public_key(&temp.0).is_some());
    let host = "user@example.invalid:2222".parse().unwrap();
    assert_eq!(
        copy_key_command(&host, std::path::Path::new("/tmp/key's file.pub")),
        "ssh-copy-id -i '/tmp/key'\\''s file.pub' -p 2222 'user@example.invalid'"
    );
}

#[test]
fn fake_ssh_on_path_simulates_success_auth_failure_and_timeout() {
    let temp = TempDir::new();
    let ssh = temp.0.join("ssh");
    fs::write(
        &ssh,
        r#"#!/bin/sh
printf '%s\n' "$@" >> "$RUNWELL_SSH_ARGS"
case "$RUNWELL_SSH_MODE" in
  auth) exit 255 ;;
  timeout) exec sleep 5 ;;
esac
for last do :; done
if [ "$last" = true ]; then exit 0; fi
cat >/dev/null
printf '%s\n' '{"vcpus":{"value":4},"docker_present":{"value":false}}'
"#,
    )
    .unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(temp.0.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    for mode in ["success", "auth", "timeout"] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "ssh_path_helper", "--nocapture"])
            .env("PATH", &path)
            .env("RUNWELL_SSH_MODE", mode)
            .env("RUNWELL_SSH_ARGS", temp.0.join("args"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let args = fs::read_to_string(temp.0.join("args")).unwrap();
    assert!(args.contains("BatchMode=yes"));
    assert!(!args.to_lowercase().contains("password"));
}

// A subprocess isolates PATH changes from concurrent tests without unsafe env mutation.
#[tokio::test]
async fn ssh_path_helper() {
    let Ok(mode) = std::env::var("RUNWELL_SSH_MODE") else {
        return;
    };
    let host = "user@example.invalid".parse().unwrap();
    let deadline = if mode == "timeout" {
        Duration::from_millis(150)
    } else {
        Duration::from_secs(5)
    };
    let client = SshClient::new("ssh", deadline);
    match mode.as_str() {
        "success" => assert_eq!(
            probe::collect(&client, &host).await.unwrap().vcpus.value,
            Some(4)
        ),
        "auth" => assert!(matches!(
            client.test_auth(&host).await,
            Err(Error::Authentication(_))
        )),
        "timeout" => assert!(matches!(
            client.test_auth(&host).await,
            Err(Error::Timeout(_))
        )),
        _ => panic!("unknown fixture mode"),
    }
}

#[tokio::test]
async fn local_shell_probe_is_valid_and_tolerates_missing_linux_features() {
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new("sh")
            .arg("-c")
            .arg(probe::SCRIPT)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(result.status.success());
    let facts = probe::parse_output(&result.stdout).unwrap();
    assert!(facts.kernel.value.is_some());
    if cfg!(target_os = "macos") {
        assert_eq!(facts.cgroup_v2.unknown_reason.as_deref(), Some("not Linux"));
        assert!(facts.ram_bytes.value.is_none());
    }
}
