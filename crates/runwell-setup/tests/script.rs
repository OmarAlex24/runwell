#![cfg(unix)]
use runwell_setup::probe;
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("runwell-probe-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(path.join("bin")).unwrap();
        Self(path)
    }
    fn command(&self, name: &str, body: &str) {
        let path = self.0.join("bin").join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    async fn run(&self, script: &str) -> runwell_setup::HostFacts {
        let path = std::env::join_paths(
            std::iter::once(self.0.join("bin"))
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let output = tokio::time::timeout(
            Duration::from_secs(20),
            tokio::process::Command::new("sh")
                .args(["-c", script])
                .env("PATH", path)
                .env("FIXTURE_ROOT", &self.0)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        probe::parse_output(&output.stdout)
            .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&output.stdout)))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn shell_probe_extracts_runner_metadata_process_tree_and_cpu_percentiles() {
    let fixture = Fixture::new();
    let runner = fixture.0.join("runner with spaces");
    fs::create_dir_all(runner.join("bin")).unwrap();
    fs::write(
        runner.join(".runner"),
        r#"{
      "agentName": "test-\"runner\"",
      "gitHubUrl": "https://github.com/example/project",
      "labels": ["self-hosted", "linux", "custom"],
      "workFolder": "_work"
    }"#,
    )
    .unwrap();
    fs::write(
        runner.join("bin/Runner.Listener.deps.json"),
        "{\"libraries\": {\"Runner.Listener/2.325.0\": {}}}",
    )
    .unwrap();
    fixture.command(
        "uname",
        "case \"$1\" in -s) echo Linux;; -r) echo 6.12.0;; -m) echo x86_64;; esac",
    );
    fixture.command("id", "echo 1000");
    fixture.command("sudo", "exit 1");
    fixture.command(
        "systemctl",
        r#"case "$1" in
      --version) echo 'systemd 257' ;;
      list-units) echo 'actions.runner.example.test.service loaded active running Runner' ;;
      show) case "$4" in
        WorkingDirectory) printf '%s\n' "$FIXTURE_ROOT/runner with spaces" ;;
        User) echo runner ;;
        MainPID) echo 10 ;;
      esac ;;
    esac"#,
    );
    fixture.command("getent", "echo 'runner:x:1000:1000::/home/runner:/bin/sh'");
    fixture.command(
        "ps",
        "printf '10 1 Runner.Listener\n11 10 node\n12 11 Runner.Worker\n'",
    );
    fixture.command(
        "ss",
        "printf '%s\n' 'tcp LISTEN 0 128 *:5432 *:* users:((\"postgres\",pid=123,fd=1))'",
    );
    fixture.command("docker", "exit 127");
    fixture.command("date", "printf '2026-09-%02d\n' \"$((30 - ${2%% *}))\"");
    fixture.command(
        "sar",
        r#"printf '%s\n' \
      'Linux 6.12.0 (example) 09/29/2026 _x86_64_ (8 CPU)' \
      '00:00:00 CPU %user %nice %system %iowait %steal %idle' \
      '00:01:00 all 0 0 0 0 0 90.00' \
      '00:02:00 all 0 0 0 0 0 80.00' \
      '00:03:00 all 0 0 0 0 0 70.00' \
      '00:04:00 all 0 0 0 0 0 60.00' \
      'Average: all 0 0 0 0 0 75.00'"#,
    );
    let sa = fixture.0.join("sa");
    fs::create_dir(&sa).unwrap();
    fs::write(sa.join("sa20260929"), "fixture").unwrap();
    let script = probe::SCRIPT.replace("/var/log/sa", &sa.to_string_lossy());
    let facts = fixture.run(&script).await;
    let runners = facts.runners.value.unwrap();
    assert_eq!(runners.len(), 1);
    assert_eq!(runners[0].name.value.as_deref(), Some("test-\"runner\""));
    assert_eq!(runners[0].labels.value.as_ref().unwrap().len(), 3);
    assert_eq!(runners[0].ephemeral.value, Some(false));
    assert_eq!(runners[0].version.value.as_deref(), Some("2.325.0"));
    assert_eq!(runners[0].active_job.value, Some(true));
    assert_eq!(facts.sudo_available.value, Some(false));
    assert_eq!(
        facts.listening_services.value.unwrap()[0]
            .name
            .value
            .as_deref(),
        Some("postgres")
    );
    let days = facts.daily_cpu.value.unwrap();
    assert_eq!(days.len(), 1);
    assert_eq!(days[0].p50.value, Some(20.0));
    assert_eq!(days[0].p95.value, Some(40.0));
    assert_eq!(days[0].samples.value, Some(4));
}

#[tokio::test]
async fn unreadable_or_absent_runner_metadata_stays_unknown() {
    let fixture = Fixture::new();
    fixture.command(
        "uname",
        "case \"$1\" in -s) echo Linux;; -r) echo 6.12.0;; -m) echo x86_64;; esac",
    );
    fixture.command("id", "echo 1000");
    fixture.command("sudo", "exit 1");
    fixture.command("docker", "exit 127");
    fixture.command("systemctl", r#"case "$1" in
      --version) echo 'systemd 257' ;;
      list-units) echo 'actions.runner.example.test.service loaded inactive dead Runner' ;;
      show) case "$4" in WorkingDirectory) echo /nonexistent-runwell-fixture;; User) echo runner;; MainPID) echo 0;; esac ;;
    esac"#);
    fixture.command("sar", "exit 1");
    let facts = fixture.run(probe::SCRIPT).await;
    let runners = facts.runners.value.unwrap();
    assert!(runners[0].ephemeral.value.is_none());
    assert!(runners[0].name.unknown_reason.is_some());
    assert_eq!(runners[0].active_job.value, Some(false));
}
