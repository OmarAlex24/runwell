use std::process::Command;

#[test]
fn availability_formats_and_legacy_sensitivity_reach_replay() {
    let directory =
        std::env::temp_dir().join(format!("runwell-availability-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let trace = directory.join("trace.jsonl");
    let hosts = directory.join("hosts.toml");
    std::fs::write(&trace, r#"{"schema_version":1,"repo":"example/app","run_id":1,"event":"pull_request","run_conclusion":"success","run_created_at":"2026-01-01T00:00:00Z","job_name":"unit","started_at":"2026-01-01T00:00:00Z","completed_at":"2026-01-01T00:01:00Z","conclusion":"success","needs":[]}"#).unwrap();
    std::fs::write(&hosts, "[[hosts]]\nclass='big'\ncores=12\nmemory_gib=31.0\n[[pools]]\nrepo='example/app'\nrunners=1\n").unwrap();
    for (extension, input) in [
        (
            "jsonl",
            r#"{"kind":"runner_offline","host":0,"pool":0,"runner":0,"start":"2026-01-01T00:00:00Z","end":"2026-01-01T00:01:00Z","cause":"broker"}"#,
        ),
        (
            "toml",
            "[[events]]\nkind='runner_offline'\nhost=0\npool=0\nrunner=0\nstart='2026-01-01T00:00:00Z'\nend='2026-01-01T00:01:00Z'\ncause='broker'",
        ),
    ] {
        let availability = directory.join(format!("availability.{extension}"));
        std::fs::write(&availability, input).unwrap();
        for legacy in [false, true] {
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_runwell"));
            cmd.arg("simulate")
                .arg("--trace")
                .arg(&trace)
                .arg("--hosts")
                .arg(&hosts)
                .arg("--availability")
                .arg(&availability)
                .args(["--policy", "all", "--format", "json"]);
            if legacy {
                cmd.arg("--runwell-runner-availability");
            }
            let output = cmd.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            for row in report["rows"].as_array().unwrap() {
                let limited = legacy
                    || ["baseline", "runwell-equivalent"]
                        .contains(&row["policy"].as_str().unwrap());
                assert_eq!(row["metrics"]["p50_minutes"], if limited { 2. } else { 1. });
            }
            assert_eq!(report["equivalence"][0]["passed"], true);
        }
    }
    std::fs::remove_dir_all(directory).unwrap();
}
