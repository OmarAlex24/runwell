//! Golden edits and atomic writer regression coverage.
use runwell_advise::{
    atomic_write,
    cli::{AdviseArgs, Format, SelfHosted, execute},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}
fn temp() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "runwell-advise-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}
fn args(path: PathBuf, rule: &str) -> AdviseArgs {
    AdviseArgs {
        workflows: path,
        trace: None,
        format: Format::Json,
        fix: true,
        rule: vec![rule.into()],
        self_hosted: SelfHosted::Auto,
    }
}
#[test]
fn golden_fixes_preserve_every_unrelated_byte_and_are_idempotent() {
    for (fixture_id, rule) in [
        ("concurrency", "missing-concurrency"),
        ("cache", "shared-home-cache"),
        ("ports", "fixed-service-ports"),
    ] {
        let dir = temp();
        let path = dir.join("checks.yml");
        fs::copy(fixture(&format!("{fixture_id}-input.yml")), &path).unwrap();
        let (json, _) = execute(&args(path.clone(), rule)).unwrap();
        let report: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(report["schemaVersion"], 1);
        assert_eq!(
            fs::read(&path).unwrap(),
            fs::read(fixture(&format!("{fixture_id}-expected.yml"))).unwrap(),
            "{fixture_id}"
        );
        assert!(
            report["changes"][0]["diff"]
                .as_str()
                .unwrap()
                .contains("@@")
        );
        let (again, _) = execute(&args(path, rule)).unwrap();
        assert!(
            serde_json::from_str::<serde_json::Value>(&again).unwrap()["changes"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
#[test]
fn refuses_ambiguous_insertions_and_literal_port_clients() {
    let cases = [
        ("missing-concurrency", "on: pull_request\njobs: {}\n", false),
        (
            "missing-concurrency",
            "{on: pull_request, jobs: {}}\n",
            true,
        ),
        (
            "shared-home-cache",
            "on: pull_request\njobs:\n  check: &job\n    runs-on: self-hosted\n    steps:\n      - run: golangci-lint run\n",
            true,
        ),
        (
            "fixed-service-ports",
            "on: pull_request\njobs:\n  check:\n    runs-on: self-hosted\n    services:\n      db:\n        image: postgres:16\n        ports: ['5432:5432']\n    steps:\n      - run: psql -p 5432\n",
            true,
        ),
    ];
    for (rule, source, refused) in cases {
        let dir = temp();
        let path = dir.join("checks.yml");
        fs::write(&path, source).unwrap();
        let (json, _) = execute(&args(path.clone(), rule)).unwrap();
        let report: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            !report["refused"].as_array().unwrap().is_empty(),
            refused,
            "{rule}"
        );
        if refused {
            assert_eq!(fs::read_to_string(&path).unwrap(), source);
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
#[test]
fn atomic_writer_aborts_on_stale_file_and_removes_temp() {
    let dir = temp();
    let path = dir.join("checks.yml");
    fs::write(&path, b"newer content").unwrap();
    let error = atomic_write(&path, b"analyzed content", b"replacement").unwrap_err();
    assert!(error.to_string().contains("changed since"));
    assert_eq!(fs::read(&path).unwrap(), b"newer content");
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    atomic_write(&path, b"newer content", b"replacement").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"replacement");
    fs::remove_dir_all(dir).unwrap();
}
#[test]
fn parses_every_file_before_editing_any() {
    let dir = temp();
    let valid = fs::read(fixture("concurrency-input.yml")).unwrap();
    fs::write(dir.join("a.yml"), &valid).unwrap();
    fs::write(dir.join("b.yml"), "jobs: [").unwrap();
    assert!(execute(&args(dir.clone(), "missing-concurrency")).is_err());
    assert_eq!(fs::read(dir.join("a.yml")).unwrap(), valid);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn unicode_multiline_scalars_do_not_shift_later_edits() {
    let dir = temp();
    let path = dir.join("checks.yml");
    let source = "name: checks\non: pull_request\njobs:\n  first:\n    runs-on: ubuntu-latest\n    steps:\n      - run: |\n          echo 'λ 🚀 café'\n          echo '\\u00e9'\n  check:\n    name: Later job\n    runs-on: self-hosted\n    steps:\n      - run: golangci-lint run\n";
    fs::write(&path, source).unwrap();
    execute(&args(path.clone(), "shared-home-cache")).unwrap();
    let expected=source.replace("    name: Later job", "    env:\n      GOLANGCI_LINT_CACHE: ${{ github.workspace }}/../.runwell-cache/${{ github.run_id }}-${{ github.run_attempt }}-${{ github.job }}-${{ strategy.job-index || 0 }}/golangci-lint\n    name: Later job");
    assert_eq!(fs::read_to_string(&path).unwrap(), expected);
    fs::remove_dir_all(dir).unwrap();
}
