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
fn unfinished_commands_exit_two_and_explain_status() {
    for command in ["controller", "node", "simulate", "advise"] {
        let output = Command::new(env!("CARGO_BIN_EXE_runwell"))
            .arg(command)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        let message = String::from_utf8(output.stderr).unwrap();
        assert!(message.contains(command) && message.contains("not implemented"));
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
