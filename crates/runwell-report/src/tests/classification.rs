use super::{job, rules, step, time};
use crate::classify::{self, Rules};
fn defaults() -> Rules {
    Rules::parse(rules(), &[]).unwrap()
}

#[test]
fn classifies_reruns_on_same_sha_and_excludes_aggregators_and_unstarted() {
    let mut fail = job("tests", 1, 0, 0, 100);
    fail.conclusion = Some("failure".into());
    fail.steps[0].conclusion = Some("failure".into());
    let mut success = fail.clone();
    success.run_attempt = 2;
    success.conclusion = Some("success".into());
    let mut agg = job("summary", 1, 0, 0, 10);
    agg.steps = vec![step("Set up job", 0, 1), step("Complete job", 9, 10)];
    agg.conclusion = Some("failure".into());
    let mut unstarted = job("queued", 2, 0, 0, 100);
    unstarted.runner_name = None;
    unstarted.conclusion = Some("cancelled".into());
    let result = classify::summarize(&[fail, success, agg, unstarted], &defaults());
    assert_eq!(result.eligible_jobs, 2);
    assert_eq!(result.flaky, 1);
    assert_eq!(result.flaky_percent, 50.0);
    assert_eq!(result.aggregators, 1);
    assert_eq!(result.cancelled_before_start, 1);
}

#[test]
fn infrastructure_evidence_overrides_rerun_and_timeout_cancellations() {
    let mut failure = job("lint", 1, 0, 0, 100);
    failure.conclusion = Some("failure".into());
    failure.log_excerpt = Some("Error: parallel golangci-lint is running".into());
    let mut timeout = job("tests", 2, 0, 0, 900);
    timeout.conclusion = Some("cancelled".into());
    timeout.timeout_minutes = Some(15.0);
    let mut shutdown = job("build", 3, 0, 0, 100);
    shutdown.conclusion = Some("failure".into());
    shutdown.annotations = vec!["The runner has received a shutdown signal".into()];
    let result = classify::summarize(&[failure, timeout, shutdown], &defaults());
    assert_eq!(result.infra, 3);
}

#[test]
fn superseded_cancellations_and_code_failures_are_distinct() {
    let mut old = job("tests", 1, 0, 0, 100);
    old.conclusion = Some("cancelled".into());
    let mut newer = job("tests", 2, 50, 50, 150);
    newer.run_created_at = Some(time(50));
    let mut code = job("lint", 3, 0, 0, 100);
    code.conclusion = Some("failure".into());
    code.steps[0].conclusion = Some("failure".into());
    let result = classify::summarize(&[old, newer, code], &defaults());
    assert_eq!(result.superseded, 1);
    assert_eq!(result.code, 1);
}

#[test]
fn separate_run_same_sha_pass_is_flaky_but_different_sha_is_not() {
    let mut bad = job("tests", 1, 0, 0, 100);
    bad.conclusion = Some("failure".into());
    bad.steps[0].conclusion = Some("failure".into());
    let mut later = job("tests", 2, 200, 200, 300);
    later.run_created_at = Some(time(200));
    later.head_sha = bad.head_sha.clone();
    assert_eq!(
        classify::summarize(&[bad.clone(), later.clone()], &defaults()).flaky,
        1
    );
    later.head_sha = Some("different-commit".into());
    assert_eq!(classify::summarize(&[bad, later], &defaults()).flaky, 0);
}

#[test]
fn old_trace_falls_back_to_same_run_attempt_and_unknown_is_not_infra() {
    let mut bad = job("tests", 1, 0, 0, 100);
    bad.head_sha = None;
    bad.conclusion = Some("failure".into());
    let mut later = bad.clone();
    later.conclusion = Some("success".into());
    later.run_attempt = 2;
    assert_eq!(classify::summarize(&[bad, later], &defaults()).flaky, 1);
    let mut cancelled = job("tests", 2, 0, 0, 100);
    cancelled.conclusion = Some("cancelled".into());
    assert_eq!(classify::summarize(&[cancelled], &defaults()).unknown, 1);
}

#[test]
fn custom_rules_and_aggregator_patterns_are_data_driven() {
    let rules = Rules::parse(
        "[[rules]]\nname='synthetic signal'\nclass='infra'\npattern='broken widget'",
        &["^gate$".into()],
    )
    .unwrap();
    let mut bad = job("test", 1, 0, 0, 100);
    bad.conclusion = Some("failure".into());
    bad.annotations = vec!["broken widget".into()];
    assert_eq!(
        classify::summarize(&[bad, job("gate", 1, 0, 0, 1)], &rules).infra,
        1
    );
}

#[test]
fn failure_denominator_includes_executed_jobs_with_skewed_creation_timestamps() {
    let mut skewed = job("tests", 1, 10, 0, 100);
    skewed.conclusion = Some("failure".into());
    skewed.annotations = vec!["No space left on device".into()];
    let report = classify::summarize(&[skewed], &defaults());
    assert_eq!(report.eligible_jobs, 1);
    assert_eq!(report.infra, 1);
}

#[test]
fn pending_outcomes_are_visible_in_the_executed_job_denominator() {
    let mut pending = job("tests", 1, 0, 0, 100);
    pending.completed_at = None;
    pending.conclusion = None;
    let report = classify::summarize(&[pending], &defaults());
    assert_eq!(report.eligible_jobs, 1);
    assert_eq!(report.in_progress_jobs, 1);
}
