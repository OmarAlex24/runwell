#![cfg(unix)]
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "runwell-cli-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(path.join("bin")).unwrap();
        let ssh = path.join("bin/ssh");
        fs::write(&ssh, r#"#!/bin/sh
printf '%s\n' "$@" >> "$SSH_ARG_LOG"
for arg do
  case "$arg" in *denied.invalid) exit 255;; esac
  last=$arg
done
if [ "$last" = true ]; then exit 0; fi
cat >/dev/null
printf '%s\n' '{"os":{"value":"Example Linux"},"vcpus":{"value":4},"docker_present":{"value":false}}'
"#).unwrap();
        fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
    fn run(&self, args: &[&str]) -> Output {
        let path = std::env::join_paths(
            std::iter::once(self.0.join("bin"))
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        Command::new(env!("CARGO_BIN_EXE_runwell"))
            .args(args)
            .env("PATH", path)
            .env("SSH_ARG_LOG", self.0.join("ssh-args"))
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn setup_probe_help_explains_read_only_key_auth_and_json() {
    let output = Command::new(env!("CARGO_BIN_EXE_runwell"))
        .args(["setup", "probe", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for text in [
        "BatchMode=yes",
        "No remote files are written",
        "--host",
        "--json",
        "never prompts",
    ] {
        assert!(help.contains(text), "{text}: {help}");
    }
}

#[test]
fn agent_setup_saves_multiple_hosts_repos_and_resumes_without_prompts() {
    let fixture = Fixture::new();
    let state_file = fixture.0.join("state.json");
    let state_path = state_file.to_str().unwrap();
    let output = fixture.run(&[
        "setup",
        "--host",
        "builder@first.invalid:2222",
        "--host",
        "builder@second.invalid",
        "--repo",
        "example/a",
        "--repo",
        "example/b",
        "--state-file",
        state_path,
        "--json",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let state: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(state["hosts"].as_array().unwrap().len(), 2);
    assert_eq!(state["repos"].as_array().unwrap().len(), 2);
    assert!(state["recommendation"].is_null());
    assert!(state["plan"].is_null());
    assert_eq!(state["hosts"][0]["facts"]["vcpus"]["value"], 4);
    let saved: Value = serde_json::from_slice(&fs::read(&state_file).unwrap()).unwrap();
    assert_eq!(state, saved);
    let log = fs::read_to_string(fixture.0.join("ssh-args")).unwrap();
    assert!(!log.to_lowercase().contains("password"));
    assert!(log.contains("BatchMode=yes"));
    let resumed = fixture.run(&["setup", "--state-file", state_path, "--json"]);
    assert!(resumed.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&resumed.stdout).unwrap(),
        state
    );
}

#[test]
fn probe_outputs_only_facts_and_auth_failure_keeps_completed_progress() {
    let fixture = Fixture::new();
    let probe = fixture.run(&[
        "setup",
        "probe",
        "--host",
        "builder@first.invalid",
        "--json",
    ]);
    assert!(probe.status.success());
    let facts: Value = serde_json::from_slice(&probe.stdout).unwrap();
    assert_eq!(facts["vcpus"]["value"], 4);
    assert!(facts.get("hosts").is_none());
    let state_file = fixture.0.join("state.json");
    let failed = fixture.run(&[
        "setup",
        "--host",
        "builder@first.invalid",
        "--host",
        "builder@denied.invalid",
        "--state-file",
        state_file.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(failed.status.code(), Some(2));
    assert!(failed.stdout.is_empty());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("SSH key authentication failed"));
    let saved: Value = serde_json::from_slice(&fs::read(state_file).unwrap()).unwrap();
    assert_eq!(saved["hosts"].as_array().unwrap().len(), 1);
}
