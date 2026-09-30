//! Timing thresholds, population matching, and critical-path evidence.
use runwell_advise::cli::{AdviseArgs, Format, SelfHosted, execute};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
fn row() -> Value {
    serde_json::from_str(include_str!("fixtures/timing.jsonl")).unwrap()
}
fn analyze(source: &str, rows: &[Value], mode: SelfHosted) -> Value {
    let dir = std::env::temp_dir().join(format!(
        "runwell-timing-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("checks.yml");
    let trace = dir.join("trace.jsonl");
    fs::write(&file, source).unwrap();
    fs::write(
        &trace,
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let (text, _) = execute(&AdviseArgs {
        workflows: file,
        trace: Some(trace),
        format: Format::Json,
        fix: false,
        rule: vec![],
        self_hosted: mode,
    })
    .unwrap();
    let result = serde_json::from_str(&text).unwrap();
    fs::remove_dir_all(dir).unwrap();
    result
}
const TESTS: &str = "name: checks\non: pull_request\njobs:\n  check:\n    runs-on: self-hosted\n    timeout-minutes: 5\n    steps:\n      - name: Run tests\n        run: pytest\n";
fn find<'a>(result: &'a Value, rule: &str) -> Option<&'a Value> {
    result["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["rule"] == rule)
}
#[test]
fn measured_shards_and_timeout_budget() {
    let r = analyze(TESTS, &[row()], SelfHosted::Auto);
    let shard = find(&r, "shardable-tests").unwrap();
    assert_eq!(shard["estimatedSavings"], json!({"p50":4.5,"p90":4.5}));
    assert_eq!(
        shard["evidence"]["observations"]["timing"]["criticalSamples"],
        1
    );
    let timeout = find(&r, "tight-timeout").unwrap();
    assert_eq!(
        timeout["evidence"]["observations"]["suggestedMinutes"],
        15.0
    );
}
#[test]
fn unrelated_workflow_timing_is_not_attached() {
    let mut other = row();
    other["workflow"] = json!("different");
    let r = analyze(TESTS, &[other], SelfHosted::Auto);
    assert!(find(&r, "shardable-tests").is_none());
    assert!(find(&r, "tight-timeout").is_none());
}
#[test]
fn timeout_cancellations_are_distinct_from_test_timeouts() {
    let mut failed = row();
    failed["run_id"] = json!(2);
    failed["conclusion"] = json!("cancelled");
    failed["annotations"] = json!(["The job has exceeded the maximum execution time"]);
    let r = analyze(
        &TESTS.replace("timeout-minutes: 5", "timeout-minutes: 30"),
        &[row(), failed.clone()],
        SelfHosted::Auto,
    );
    assert_eq!(
        find(&r, "tight-timeout").unwrap()["evidence"]["observations"]["timing"]["timeoutRate"],
        0.5
    );
    failed["annotations"] = json!(["Test timeout exceeded"]);
    let r = analyze(
        &TESTS.replace("timeout-minutes: 5", "timeout-minutes: 30"),
        &[row(), failed],
        SelfHosted::Auto,
    );
    assert!(find(&r, "tight-timeout").is_none());
}
#[test]
fn queue_cost_and_observed_wait_have_units() {
    let mut r = row();
    r["steps"] = json!([{"name":"Acquire slot","started_at":"2026-01-01T00:02:00Z","completed_at":"2026-01-01T00:03:00Z","conclusion":"success"}]);
    let source = "name: checks\non: pull_request\njobs:\n  check:\n    needs: upstream\n    runs-on: self-hosted\n    steps:\n      - name: Acquire slot\n        run: ci-slot acquire heavy\n";
    r["completed_at"] = json!("2026-01-01T00:03:00Z");
    let result = analyze(source, &[r], SelfHosted::Auto);
    assert_eq!(
        find(&result, "serial-hop").unwrap()["estimatedSavings"]["p50"],
        3.0
    );
    assert_eq!(
        find(&result, "host-semaphore-holds-runner").unwrap()["evidence"]["observations"]["runnerMinutesWaiting"],
        1.0
    );
    assert!(find(&analyze(source, &[row()], SelfHosted::No), "serial-hop").is_none());
}
#[test]
fn unsupported_trace_schema_and_rule_usage_are_rejected() {
    // Clap integration covers flags; the library also rejects future trace records.
    let dir = std::env::temp_dir().join(format!("runwell-schema-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let mut r = row();
    r["schema_version"] = json!(99);
    fs::write(dir.join("trace.jsonl"), r.to_string()).unwrap();
    let args = AdviseArgs {
        workflows: PathBuf::from("missing"),
        trace: Some(dir.join("trace.jsonl")),
        format: Format::Json,
        fix: false,
        rule: vec![],
        self_hosted: SelfHosted::Auto,
    };
    assert!(
        execute(&args)
            .unwrap_err()
            .to_string()
            .contains("unsupported")
    );
    fs::remove_dir_all(dir).unwrap();
}
