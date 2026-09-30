use super::{job, step, time};
use crate::{
    metrics::{self, bestcase, concurrency, contention, critical, jobs, runs},
    trace_build,
};
use regex::RegexSet;
use std::collections::BTreeMap;

fn waits() -> RegexSet {
    RegexSet::new(["(?i)(wait|slot|semaphore|lock)"]).unwrap()
}

#[test]
fn needs_path_and_inferred_path_split_time() {
    let mut first = job("build", 1, 0, 10, 60);
    first.needs = Some(Vec::new());
    let mut last = job("test", 1, 65, 80, 180);
    last.needs = Some(vec!["build".into()]);
    last.steps = vec![step("Wait for slot", 80, 100), step("Test", 100, 180)];
    let refs = vec![&first, &last];
    let graph = critical::graph(&refs);
    assert!(graph.explicit);
    assert_eq!(critical::path(&refs, &graph), vec![0, 1]);
    let comp = critical::composition(&refs, &graph, &waits());
    assert_eq!(
        (comp.queue, comp.gap, comp.wait, comp.work),
        (25.0, 5.0, 20.0, 130.0)
    );
    first.needs = None;
    last.needs = None;
    let refs = vec![&first, &last];
    let graph = critical::graph(&refs);
    assert!(!graph.explicit);
    assert_eq!(critical::path(&refs, &graph), vec![0, 1]);
}

#[test]
fn ambiguous_and_cyclic_needs_fall_back() {
    let mut a = job("a", 1, 0, 0, 10);
    a.needs = Some(vec!["b".into()]);
    let mut b = job("b", 1, 10, 10, 20);
    b.needs = Some(vec!["a".into()]);
    assert!(!critical::graph(&[&a, &b]).explicit);
    b.needs = Some(vec!["missing".into()]);
    assert!(!critical::graph(&[&a, &b]).explicit);
}

#[test]
fn overlapping_waits_are_unioned_and_clipped() {
    let mut a = job("tests", 1, 0, 10, 100);
    a.steps = vec![
        step("Wait", 0, 50),
        step("slot", 40, 80),
        step("lock", 90, 120),
    ];
    assert_eq!(critical::wait_seconds(&a, &waits()), 80.0);
}

#[test]
fn successful_first_attempts_only_and_percentile_bands() {
    let mut all = Vec::new();
    for n in 1..=10 {
        all.push(job("test", n, 0, 0, n as i64 * 100));
    }
    let mut failed = job("test", 11, 0, 0, 5000);
    failed.run_conclusion = Some("failure".into());
    all.push(failed);
    let mut rerun = job("test", 12, 0, 0, 5000);
    rerun.run_attempt = 2;
    all.push(rerun);
    let r = runs::summarize(&all, &waits(), &BTreeMap::new());
    assert_eq!(r[0].end_to_end_seconds.count, 10);
    assert_eq!(r[0].end_to_end_seconds.p50, Some(550.0));
    assert_eq!(r[0].end_to_end_seconds.p90, Some(910.0));
    assert_eq!(
        r[0].bands
            .iter()
            .find(|b| b.band == "p40-p60")
            .unwrap()
            .count,
        2
    );
}

#[test]
fn per_job_excludes_skipped_and_not_started_from_duration() {
    let mut all = vec![
        job("test", 1, 0, 20, 80),
        job("test", 2, 0, 40, 160),
        job("test", 3, 0, 0, 100),
    ];
    all[1].conclusion = Some("failure".into());
    all[2].conclusion = Some("skipped".into());
    let mut cancelled = job("test", 4, 0, 0, 100);
    cancelled.conclusion = Some("cancelled".into());
    cancelled.runner_name = None;
    all.push(cancelled);
    let r = jobs::summarize(&all);
    assert_eq!(r[0].ran, 2);
    assert_eq!(r[0].duration_seconds.p50, Some(90.0));
    assert_eq!(r[0].queue_seconds.p50, Some(30.0));
    assert!((r[0].fail_percent - 100.0 / 3.0).abs() < 1e-6);
    assert_eq!(r[0].runner_minutes, 3.0);
}

#[test]
fn concurrency_clips_runner_overlap_and_honors_host_scope_and_boundaries() {
    let mut second = job("test", 2, 0, 5, 15);
    second.runner_name = Some("runner-build".into());
    let mut outside = job("other", 3, 0, 0, 20);
    outside.labels = vec!["host-b".into()];
    let jobs = vec![
        job("build", 1, 0, 0, 10),
        second,
        job("next", 4, 15, 15, 20),
        outside,
    ];
    let capacities = BTreeMap::from([("host-a".into(), 1)]);
    let tl = concurrency::timeline(&jobs, time(0), time(30), &["host-a".into()], &capacities);
    assert_eq!(tl.report.peak, 1);
    assert_eq!(tl.report.clipped_runner_overlaps, 1);
    assert!((tl.report.idle_fraction - 1.0 / 3.0).abs() < 1e-6);
    assert_eq!(tl.average(time(5), time(25)), 0.75);
    assert_eq!(
        tl.report
            .labels
            .iter()
            .find(|l| l.label == "host-a")
            .unwrap()
            .saturation_hours,
        20.0 / 3600.0
    );
}

#[test]
fn contention_compares_job_net_and_step_durations() {
    let mut all = vec![job("test", 1, 0, 0, 30)];
    all[0].steps = vec![step("wait", 0, 10), step("Run tests", 10, 30)];
    for n in 0..7 {
        let mut j = job(if n == 0 { "test" } else { "load" }, n + 2, 100, 100, 160);
        j.runner_name = Some(format!("parallel-{n}"));
        j.steps = vec![step("Run tests", 100, 160)];
        all.push(j);
    }
    let tl = concurrency::timeline(&all, time(0), time(200), &[], &BTreeMap::new());
    let rows = contention::summarize(&all, &tl, &[], 3, 7, &waits());
    let r = rows
        .iter()
        .find(|r| r.job == "test" && r.step.is_none())
        .unwrap();
    assert_eq!(
        (r.low.p50, r.high.p50, r.high_to_low_ratio),
        (Some(20.0), Some(60.0), Some(3.0))
    );
    assert_eq!(
        rows.iter()
            .find(|r| r.job == "test" && r.step.as_deref() == Some("Run tests"))
            .unwrap()
            .high_to_low_ratio,
        Some(3.0)
    );
}

#[test]
fn best_case_recomputes_the_full_dag_and_removes_queue_and_waits() {
    let mut a = job("a", 1, 0, 10, 110);
    a.needs = Some(Vec::new());
    a.steps = vec![step("Wait", 10, 30), step("Test", 30, 110)];
    let mut b = job("b", 1, 0, 10, 90);
    b.needs = Some(Vec::new());
    let mut c = job("c", 1, 115, 125, 155);
    c.needs = Some(vec!["a".into(), "b".into()]);
    let factors = BTreeMap::from([(("acme/app".into(), "a".into()), 0.5)]);
    let refs = vec![&a, &b, &c];
    let g = critical::graph(&refs);
    // b becomes the simulated bottleneck: max(a=40,b=80)+gap5+c30.
    assert_eq!(bestcase::estimate(&refs, &g, &factors, &waits()), 115.0);
}

#[test]
fn workflow_maps_yaml_ids_to_display_names_and_detects_ambiguity() {
    let mut jobs = vec![job("Build", 1, 0, 0, 20), job("Tests", 1, 20, 20, 40)];
    let yaml = "jobs:\n  build:\n    name: Build\n  test:\n    name: Tests\n    needs: build\n    timeout-minutes: 15\n";
    assert!(trace_build::apply_workflow(&mut jobs, yaml).unwrap());
    assert_eq!(jobs[1].needs, Some(vec!["Build".into()]));
    assert_eq!(jobs[1].timeout_minutes, Some(15.0));
    let ambiguous = "jobs:\n  build:\n    strategy: {matrix: {os: [linux, windows]}}\n";
    let mut jobs = vec![job("build", 1, 0, 0, 20)];
    assert!(!trace_build::apply_workflow(&mut jobs, ambiguous).unwrap());
    assert_eq!(jobs[0].needs, None);
}

#[test]
fn negative_and_never_started_intervals_are_not_work() {
    assert_eq!(metrics::duration(&job("test", 1, 0, 100, 90)), None);
    let mut j = job("test", 1, 0, 0, 10);
    j.started_at = None;
    assert_eq!(metrics::duration(&j), None);
}

#[test]
fn end_to_end_excludes_runs_that_were_rerun() {
    let first = job("tests", 1, 0, 0, 100);
    let mut second = first.clone();
    second.run_attempt = 2;
    assert!(runs::summarize(&[first, second], &waits(), &BTreeMap::new()).is_empty());
}

#[test]
fn low_concurrency_factor_matches_reference_ratio_and_does_not_inflate() {
    let mut all = vec![job("tests", 1, 0, 0, 20)];
    for n in 0..7 {
        let mut j = job(if n == 0 { "tests" } else { "load" }, n + 2, 100, 100, 160);
        j.runner_name = Some(format!("parallel-{n}"));
        all.push(j);
    }
    let timeline = concurrency::timeline(&all, time(0), time(200), &[], &BTreeMap::new());
    let factors = bestcase::factors(&all, &timeline, &[], 3, &waits());
    assert_eq!(factors[&("acme/app".into(), "tests".into())], 0.5);
    assert!(!factors.contains_key(&("acme/app".into(), "load".into())));
}
