use crate::{ReportArgs, analyze};
use clap::Parser;
use jiff::Timestamp;
use runwell_trace::{TraceJob, TraceStep};

mod classification;
mod fetching;
mod metric;

#[derive(Parser)]
struct Cli {
    #[command(flatten)]
    args: ReportArgs,
}
pub fn args(extra: &[&str]) -> ReportArgs {
    let mut values = vec!["report", "--from-trace", "synthetic.jsonl"];
    values.extend_from_slice(extra);
    Cli::parse_from(values).args
}
pub fn time(seconds: i64) -> Timestamp {
    Timestamp::from_second(1_767_225_600 + seconds).unwrap()
}
pub fn job(name: &str, run: u64, created: i64, start: i64, end: i64) -> TraceJob {
    serde_json::from_value(serde_json::json!({
        "schema_version":1,"repo":"acme/app","workflow":"CI","run_id":run,
        "event":"pull_request","branch":"feature","head_sha":format!("commit-{run}"),
        "run_created_at":time(0),"run_conclusion":"success","job_name":name,
        "runner_name":format!("runner-{name}"),"labels":["host-a","linux"],
        "created_at":time(created),"started_at":time(start),"completed_at":time(end),
        "status":"completed","conclusion":"success","steps":[{
            "name":"Run tests","started_at":time(start),"completed_at":time(end),"conclusion":"success"
        }]
    })).unwrap()
}
pub fn step(name: &str, start: i64, end: i64) -> TraceStep {
    TraceStep {
        name: name.into(),
        started_at: Some(time(start)),
        completed_at: Some(time(end)),
        conclusion: Some("success".into()),
    }
}
pub fn rules() -> &'static str {
    include_str!("../default-rules.toml")
}

#[test]
fn markdown_matches_synthetic_golden() {
    let jobs = vec![job("tests", 1, 10, 20, 300)];
    let report = analyze(&jobs, &args(&[]), rules(), Vec::new()).unwrap();
    assert_eq!(
        crate::render::markdown(&report),
        include_str!("../tests/golden.md")
    );
}

#[test]
fn report_schema_and_offline_filters_are_stable() {
    let mut other = job("push", 2, 0, 0, 100);
    other.event = Some("push".into());
    let jobs = vec![job("tests", 1, 10, 20, 300), other];
    let report = analyze(
        &jobs,
        &args(&["--event", "pull_request"]),
        rules(),
        Vec::new(),
    )
    .unwrap();
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["window"]["jobs"], 1);
    assert_eq!(report.concurrency.peak, 2);
}

#[test]
fn validates_windows_thresholds_and_regexes() {
    assert!(
        args(&["--since", "2026-01-02", "--until", "2026-01-01"])
            .window(None)
            .is_err()
    );
    assert!(args(&["--low-concurrency", "7"]).validate().is_err());
    assert!(analyze(&[], &args(&["--wait-regex", "["]), rules(), Vec::new()).is_err());
    assert_eq!(
        args(&["--since", "1d", "--until", "2026-01-02"])
            .window(None)
            .unwrap(),
        (time(0), time(86400))
    );
}

#[test]
fn workflow_and_window_filters_preserve_overlapping_host_work() {
    let mut overlap = job("old", 1, 0, 0, 100);
    overlap.workflow = Some("Other".into());
    let mut selected = job("tests", 2, 50, 50, 150);
    selected.run_created_at = Some(time(50));
    let args = args(&[
        "--since",
        "2026-01-01T00:00:50Z",
        "--until",
        "2026-01-01T00:03:20Z",
        "--workflow",
        "CI",
    ]);
    let r = analyze(&[overlap, selected], &args, rules(), Vec::new()).unwrap();
    assert_eq!(r.window.jobs, 1);
    assert_eq!(r.concurrency.peak, 2);
    assert_eq!(r.runs[0].end_to_end_seconds.p50, Some(100.0));
}

#[tokio::test]
async fn offline_execution_exports_unfiltered_trace_and_renders_selected_json() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.jsonl");
    let export = dir.path().join("export.jsonl");
    let mut push = job("push", 2, 0, 0, 20);
    push.event = Some("push".into());
    let jobs = vec![job("tests", 1, 10, 20, 100), push];
    runwell_trace::write_jsonl(std::fs::File::create(&input).unwrap(), &jobs).unwrap();
    let mut options = args(&["--event", "pull_request", "--format", "json"]);
    options.from_trace = Some(input);
    options.export_trace = Some(export.clone());
    let output = crate::execute(&options).await.unwrap();
    let report: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(report["window"]["jobs"], 1);
    let exported = runwell_trace::read_jsonl(std::io::BufReader::new(
        std::fs::File::open(export).unwrap(),
    ))
    .unwrap();
    assert_eq!(exported, jobs);
}
