use crate::{Config, Policy, PreparedTrace, tests::job, workflow::WorkflowNeeds};

const YAML: &str = "name: checks\nconcurrency:\n  group: '${{ github.workflow }}-${{ github.ref }}'\n  cancel-in-progress: true\njobs:\n  prepare:\n    name: Prepare\n  build:\n    name: 'Build (${{ matrix.target }})'\n    needs: prepare\n    strategy:\n      max-parallel: 2\n  finish:\n    needs: [build]\n";

#[test]
fn workflow_resolves_display_names_matrix_needs_and_date_cutoff() {
    let mut trace = vec![
        job(1, "Prepare", 0, 0, 1, &[]),
        job(1, "Build (x)", 0, 2, 12, &[]),
        job(1, "Build (y)", 0, 2, 12, &[]),
        job(1, "finish", 0, 13, 14, &[]),
    ];
    for j in &mut trace {
        j.workflow = Some("checks".into());
        j.branch = Some("feature".into());
    }
    let workflow = WorkflowNeeds::parse("example/app".into(), YAML, None).unwrap();
    let result = workflow.apply(&mut trace).unwrap();
    assert_eq!(result.graph_runs, 1);
    assert_eq!(trace[1].needs, Some(vec!["Prepare".into()]));
    assert_eq!(trace[1].max_parallel, Some(2));
    assert_eq!(
        trace[3].needs,
        Some(vec!["Build (x)".into(), "Build (y)".into()])
    );
    assert!(trace[0].cancel_group.is_some());
    let mut old = vec![job(2, "Prepare", 0, 0, 1, &[])];
    old[0].workflow = Some("checks".into());
    old[0].needs = None;
    let workflow = WorkflowNeeds::parse(
        "example/app".into(),
        YAML,
        Some(jiff::Timestamp::from_second(1).unwrap()),
    )
    .unwrap();
    assert_eq!(workflow.apply(&mut old).unwrap().inferred_runs, 1);
    assert_eq!(old[0].needs, None);
    old[0].job_name = "Build (older)".into();
    workflow.apply(&mut old).unwrap();
    assert_eq!(old[0].needs, None);
    assert_eq!(old[0].max_parallel, Some(2));
}
#[test]
fn matrix_limit_is_shared_across_hosts() {
    let mut trace: Vec<_> = (0..3)
        .map(|i| job(1, &format!("build-{i}"), 0, i * 20, i * 20 + 10, &[]))
        .collect();
    for j in &mut trace {
        j.workflow_job_id = Some("build".into());
        j.max_parallel = Some(2);
    }
    let mut c = Config::default();
    c.hosts.push(c.hosts[0].clone());
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let outcome = crate::engine::replay(&p, &c, Policy::Baseline, 2).unwrap();
    assert_eq!(outcome.timings[2].start, 10.0);
}
#[test]
fn calibration_excludes_rerun_success_snapshots_and_other_workflows() {
    let mut trace = vec![
        job(1, "a", 0, 0, 10, &[]),
        job(1, "a", 0, 20, 30, &[]),
        job(2, "a", 0, 0, 10, &[]),
        job(3, "a", 0, 0, 10, &[]),
    ];
    trace[1].run_attempt = 2;
    for j in &mut trace {
        j.workflow = Some("checks".into());
    }
    trace[3].workflow = Some("different".into());
    let c = Config {
        report_workflows: vec![crate::config::WorkflowScope {
            repo: "example/app".into(),
            name: "checks".into(),
        }],
        ..Config::default()
    };
    let report = crate::simulate(&trace, &c, &[Policy::Baseline]).unwrap();
    assert_eq!(report.rows[0].metrics.runs, 1);
}

#[test]
fn cancellation_releases_running_held_and_pending_jobs() {
    let mut trace = vec![
        job(1, "active", 0, 0, 100, &[]),
        job(1, "held", 0, 100, 200, &[]),
        job(1, "pending", 0, 200, 300, &[]),
        job(2, "replacement", 5, 300, 310, &[]),
    ];
    for j in &mut trace {
        j.cancel_group = Some("workflow/branch".into());
    }
    let mut c = Config::default();
    c.default_demand.heavy = true;
    c.heavy_slots = Some(1);
    c.pools = vec![crate::config::Pool {
        repo: "example/app".into(),
        labels: vec![],
        job_names: vec![],
        runners: 2,
    }];
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = crate::engine::replay(&p, &c, Policy::Baseline, 1).unwrap();
    assert!(o.cancelled_runs[0]);
    assert_eq!(o.timings[3].start, 5.0);
    assert_eq!(o.timings[3].end, 15.0);
    assert!(o.timings[..3].iter().all(|t| t.end == 5.0));
}
#[test]
fn equivalent_matches_baseline_without_semaphore_exactly() {
    let trace = vec![
        job(1, "a", 0, 0, 40, &[]),
        job(2, "b", 0, 100, 130, &[]),
        job(3, "c", 1, 200, 210, &[]),
    ];
    let mut c = Config::default();
    c.default_demand.heavy = true;
    c.heavy_slots = Some(1);
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let equivalent = crate::engine::replay(&p, &c, Policy::Equivalent, 1).unwrap();
    c.heavy_slots = None;
    let reference = crate::engine::replay(&p, &c, Policy::Baseline, 1).unwrap();
    for (a, b) in equivalent.timings.iter().zip(reference.timings) {
        assert_eq!(a.start, b.start);
        assert_eq!(a.end, b.end);
    }
}

#[test]
fn concurrency_order_uses_dispatch_while_latency_keeps_run_creation() {
    let mut older = job(1, "a", 0, 4, 14, &[]);
    let mut newer = job(2, "b", 1, 2, 22, &[]);
    older.created_at = Some(jiff::Timestamp::from_second(4).unwrap());
    newer.created_at = Some(jiff::Timestamp::from_second(2).unwrap());
    older.cancel_group = Some("group".into());
    newer.cancel_group = Some("group".into());
    older.dispatch_delay_seconds = Some(4.0);
    newer.dispatch_delay_seconds = Some(1.0);
    let c = Config::default();
    let p = PreparedTrace::new(&[older, newer], &c).unwrap();
    assert_eq!(p.runs[0].arrival, 0.0);
    assert_eq!(p.runs[0].cancel_at, None);
    assert_eq!(p.runs[1].cancel_at, Some(4.0));
    let o = crate::engine::replay(&p, &c, Policy::Baseline, 1).unwrap();
    assert!(!o.cancelled_runs[0]);
    assert!(o.cancelled_runs[1]);
    assert_eq!(o.timings[0].end, 14.0);
}

#[test]
fn workflow_rejects_unmodeled_expressions_and_separates_event_groups() {
    assert!(
        WorkflowNeeds::parse(
            "example/app".into(),
            &YAML.replace("max-parallel: 2", "max-parallel: '${{ inputs.limit }}'"),
            None
        )
        .is_err()
    );
    assert!(
        WorkflowNeeds::parse(
            "example/app".into(),
            &YAML.replace("github.ref", "inputs.arbitrary"),
            None
        )
        .is_err()
    );
    let mut pr = job(1, "Prepare", 0, 0, 1, &[]);
    pr.workflow = Some("checks".into());
    pr.branch = Some("feature".into());
    let mut push = pr.clone();
    push.run_id = 2;
    push.event = Some("push".into());
    let mut trace = vec![pr, push];
    WorkflowNeeds::parse("example/app".into(), YAML, None)
        .unwrap()
        .apply(&mut trace)
        .unwrap();
    assert_ne!(trace[0].cancel_group, trace[1].cancel_group);
}
