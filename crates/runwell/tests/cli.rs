use std::process::Command;

#[test]
fn help_lists_all_subcommands() {
    let output = Command::new(env!("CARGO_BIN_EXE_runwell"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for command in [
        "controller",
        "node",
        "report",
        "simulate",
        "advise",
        "setup",
        "version",
    ] {
        assert!(help.contains(command));
    }
}

#[test]
fn daemon_commands_require_explicit_configuration() {
    for command in ["controller", "node"] {
        let output = Command::new(env!("CARGO_BIN_EXE_runwell"))
            .arg(command)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        let message = String::from_utf8(output.stderr).unwrap();
        assert!(message.contains(command) && message.contains("--config"));
    }
}

#[test]
fn version_subcommand_reports_package_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_runwell"))
        .arg("version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        concat!("runwell ", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn report_help_documents_live_and_offline_modes() {
    let output = Command::new(env!("CARGO_BIN_EXE_runwell"))
        .args(["report", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for flag in [
        "--repo",
        "--since",
        "--from-trace",
        "--export-trace",
        "--cache-dir",
        "--fetch-logs",
        "--workflow",
    ] {
        assert!(help.contains(flag));
    }
    assert!(help.contains("fully offline") && help.contains("GH_TOKEN"));
}

#[test]
fn report_rejects_missing_input_without_authentication() {
    let output = Command::new(env!("CARGO_BIN_EXE_runwell"))
        .arg("report")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--repo"));
}

#[test]
fn simulate_requires_trace_and_hosts() {
    let output = Command::new(env!("CARGO_BIN_EXE_runwell"))
        .arg("simulate")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("--trace") && error.contains("--hosts"));
}

#[test]
fn simulate_emits_parseable_json_and_markdown() {
    let directory = std::env::temp_dir().join(format!("runwell-cli-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let trace = directory.join("trace.jsonl");
    let hosts = directory.join("hosts.toml");
    std::fs::write(&trace,r#"{"schema_version":1,"repo":"example/app","run_id":1,"event":"pull_request","run_conclusion":"success","run_created_at":"2026-01-01T00:00:00Z","job_name":"unit","started_at":"2026-01-01T00:00:00Z","completed_at":"2026-01-01T00:01:00Z","conclusion":"success","needs":[]}"#).unwrap();
    std::fs::write(
        &hosts,
        "[[hosts]]\nclass = 'big'\ncores = 12\nmemory_gib = 31.0\n",
    )
    .unwrap();
    for format in ["json", "md"] {
        let output = Command::new(env!("CARGO_BIN_EXE_runwell"))
            .arg("simulate")
            .arg("--trace")
            .arg(&trace)
            .arg("--hosts")
            .arg(&hosts)
            .args(["--policy", "all", "--format", format])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if format == "json" {
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["rows"].as_array().unwrap().len(), 7);
            assert!(
                report["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|row| row["policy"] == "production")
            );
            assert_eq!(report["calibration"][0]["within_ten_percent"], true);
        } else {
            assert!(
                String::from_utf8(output.stdout)
                    .unwrap()
                    .contains("Calibration against observed")
            );
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn advise_help_and_usage_errors() {
    let output = Command::new(env!("CARGO_BIN_EXE_runwell"))
        .args(["advise", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    for flag in [
        "--workflows",
        "--trace",
        "--format",
        "--fix",
        "--rule",
        "--self-hosted",
    ] {
        assert!(help.contains(flag));
    }
    for args in [
        vec!["advise", "--rule", "missing-concurrency"],
        vec!["advise", "--fix", "--rule", "unknown"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_runwell"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
}
