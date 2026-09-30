use super::*;
use crate::config::{Demand, Host, Pool};
use crate::contention::{ContentionFit, Occupancy, Sample};
use runwell_trace::TraceJob;

pub(super) fn job(
    id: u64,
    name: &str,
    arrival: i64,
    start: i64,
    end: i64,
    needs: &[&str],
) -> TraceJob {
    let ts = |s| jiff::Timestamp::from_second(s).unwrap().to_string();
    serde_json::from_value(serde_json::json!({
        "schema_version":1,"repo":"example/app","run_id":id,"run_attempt":1,
        "event":"pull_request","run_conclusion":"success","job_name":name,
        "run_created_at":ts(arrival),"created_at":ts(arrival),"started_at":ts(start),
        "completed_at":ts(end),"conclusion":"success","needs":needs,"labels":["local"]
    }))
    .unwrap()
}
fn config(cores: u32) -> Config {
    Config {
        hosts: vec![Host {
            class: "big".into(),
            cores,
            memory_gib: 32.0,
        }],
        pools: vec![Pool {
            repo: "example/app".into(),
            labels: vec![],
            job_names: vec![],
            runners: 1,
        }],
        fit_by_class: false,
        ..Config::default()
    }
}

#[test]
fn contention_fit_interpolates_and_pools_nonmonotone_bins() {
    let fit = ContentionFit::fit(
        &[
            Sample {
                concurrency: 4.0,
                inflation: 2.0,
            },
            Sample {
                concurrency: 5.0,
                inflation: 1.5,
            },
            Sample {
                concurrency: 7.0,
                inflation: 3.0,
            },
        ],
        3.0,
        2.0,
        12.0,
    );
    assert_eq!(fit.inflation(3.0), 1.0);
    assert_eq!(fit.inflation(4.0), 1.75);
    assert_eq!(fit.inflation(4.5), 1.75);
    assert_eq!(fit.inflation(6.0), 2.375);
    assert_eq!(fit.inflation(20.0), 3.0);
    assert_eq!(fit.speed(8.0, 12.0, 0.5, 1.0, 2.0), 1.0 / 1.75);
    assert_eq!(fit.speed(8.0, 24.0, 0.5, 1.0, 2.0), 1.0);
    assert_eq!(fit.speed(8.0, 24.0, 2.0, 1.0, 2.0), 0.5);
}
#[test]
fn occupancy_is_time_weighted_and_endpoints_do_not_overlap() {
    let occupancy = Occupancy::new(&[(0.0, 10.0), (5.0, 15.0), (15.0, 20.0)]);
    assert_eq!(occupancy.mean(0.0, 10.0), 1.5);
    assert_eq!(occupancy.mean(15.0, 20.0), 1.0);
}
#[test]
fn completion_precedes_simultaneous_arrival_and_releases_dependencies() {
    let trace = vec![
        job(1, "a", 0, 0, 10, &[]),
        job(1, "b", 0, 10, 15, &["a"]),
        job(2, "c", 10, 20, 21, &[]),
    ];
    let c = config(1);
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = engine::replay(&p, &c, Policy::Fifo, 1).unwrap();
    assert_eq!((o.timings[0].start, o.timings[0].end), (0.0, 10.0));
    assert_eq!((o.timings[1].start, o.timings[1].end), (10.0, 15.0));
    assert_eq!((o.timings[2].start, o.timings[2].end), (15.0, 16.0));
}
#[test]
fn external_host_jobs_do_not_affect_local_work_or_admission() {
    let mut remote = job(2, "remote", 0, 0, 100, &[]);
    remote.labels = vec!["elsewhere".into()];
    let trace = vec![job(1, "local", 0, 0, 10, &[]), remote];
    let mut c = config(1);
    c.contention_label = Some("local".into());
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = engine::replay(&p, &c, Policy::Fifo, 1).unwrap();
    assert_eq!(o.timings[0].end, 10.0);
    assert_eq!(o.timings[1].end, 100.0);
    assert_eq!(o.timings[1].host, None);
    assert_eq!(p.diagnostics.external_jobs, 1);
}
#[test]
fn semaphore_holds_runner_and_fail_open_does_not_take_a_slot() {
    let trace = vec![
        job(1, "long", 0, 0, 100, &[]),
        job(2, "wait", 0, 100, 120, &[]),
        job(3, "last", 0, 120, 121, &[]),
    ];
    let mut c = config(12);
    c.default_demand.heavy = true;
    c.heavy_slots = Some(1);
    c.pools[0].runners = 2;
    c.semaphore_timeout_seconds = 30.0;
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = engine::replay(&p, &c, Policy::Baseline, 1).unwrap();
    assert_eq!(o.timings[1].start, 30.0);
    assert_eq!(o.timings[2].start, 80.0);
    assert_eq!(o.fail_opens, 2);
}
#[test]
fn semaphore_completion_wins_over_timeout_at_the_same_instant() {
    let trace = vec![
        job(1, "long", 0, 0, 30, &[]),
        job(2, "wait", 0, 30, 40, &[]),
    ];
    let mut c = config(12);
    c.default_demand.heavy = true;
    c.heavy_slots = Some(1);
    c.pools[0].runners = 2;
    c.semaphore_timeout_seconds = 30.0;
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = engine::replay(&p, &c, Policy::Baseline, 1).unwrap();
    assert_eq!(o.timings[1].start, 30.0);
    assert_eq!(o.fail_opens, 0);
}
#[test]
fn invalid_graph_and_impossible_reservations_are_errors() {
    let trace = vec![
        job(1, "a", 0, 0, 10, &["b"]),
        job(1, "b", 0, 10, 20, &["a"]),
    ];
    assert!(PreparedTrace::new(&trace, &config(1)).is_err());
    let mut c = config(1);
    c.default_demand.cores = 2;
    assert!(simulate(&[job(1, "a", 0, 0, 10, &[])], &c, &[Policy::Fifo]).is_err());
}
#[test]
fn explicit_empty_needs_ignores_observed_creation_queue() {
    let mut trace = vec![job(1, "a", 0, 50, 60, &[])];
    trace[0].created_at = trace[0].started_at;
    let c = config(1);
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = engine::replay(&p, &c, Policy::Fifo, 1).unwrap();
    assert_eq!(o.timings[0].start, 0.0);
}
#[test]
fn inferred_graph_keeps_dispatch_gap_but_discards_runner_queue() {
    let mut trace = vec![job(1, "a", 0, 5, 15, &[]), job(1, "b", 0, 30, 40, &[])];
    trace[0].needs = None;
    trace[1].needs = None;
    trace[1].created_at = Some(jiff::Timestamp::from_second(17).unwrap());
    let c = config(1);
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = engine::replay(&p, &c, Policy::Fifo, 1).unwrap();
    assert_eq!(o.timings[1].start, 12.0);
}
#[test]
fn seeded_failures_and_reports_are_reproducible() {
    let trace: Vec<_> = (0..100)
        .map(|i| {
            let mut j = job(i, "a", i as i64 * 10, i as i64 * 10, i as i64 * 10 + 5, &[]);
            if i % 2 == 0 {
                j.conclusion = Some("failure".into());
            }
            j
        })
        .collect();
    let mut c = config(2);
    c.default_demand = Demand {
        heavy: true,
        ..Demand::default()
    };
    let a = simulate(&trace, &c, &Policy::ALL).unwrap();
    let b = simulate(&trace, &c, &Policy::ALL).unwrap();
    assert_eq!(
        serde_json::to_string(&a).unwrap(),
        serde_json::to_string(&b).unwrap()
    );
    c.seed = 44;
    let different = simulate(&trace, &c, &Policy::ALL).unwrap();
    assert_ne!(a.rows[0].infra_failures, different.rows[0].infra_failures);
}
#[test]
fn priorities_and_class_pinning_use_shared_scheduler() {
    let trace = vec![
        job(1, "long", 0, 0, 40, &[]),
        job(2, "short", 0, 100, 110, &[]),
        job(3, "medium", 0, 200, 220, &[]),
    ];
    let c = config(1);
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let shortest = engine::replay(&p, &c, Policy::Shortest, 1).unwrap();
    let critical = engine::replay(&p, &c, Policy::CriticalPath, 1).unwrap();
    assert_eq!(shortest.timings[1].start, 0.0);
    assert_eq!(critical.timings[0].start, 0.0);
    let mut c = config(1);
    c.hosts.push(Host {
        class: "small".into(),
        cores: 2,
        memory_gib: 8.0,
    });
    c.default_demand.host_class = Some("big".into());
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = engine::replay(&p, &c, Policy::Fifo, 2).unwrap();
    assert!(o.timings.iter().all(|t| t.host == Some(0)));
}

#[test]
fn zero_work_dependency_closure_is_ready_before_priority_selection() {
    let trace = vec![
        job(1, "gate", 0, 0, 0, &[]),
        job(1, "long", 0, 100, 200, &["gate"]),
        job(2, "short", 0, 200, 210, &[]),
    ];
    let c = config(1);
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = engine::replay(&p, &c, Policy::CriticalPath, 1).unwrap();
    assert_eq!(o.timings[1].start, 0.0);
}

#[test]
fn rerun_snapshots_reuse_identical_executions_without_extra_host_load() {
    let first = job(1, "a", 0, 0, 10, &[]);
    let mut duplicate = first.clone();
    duplicate.run_attempt = 2;
    let trace = vec![duplicate, first]; // Normalization is independent of input attempt order.
    let c = config(1);
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = engine::replay(&p, &c, Policy::Fifo, 1).unwrap();
    assert_eq!(p.diagnostics.reused_executions, 1);
    assert_eq!(o.timings[0].end, 10.0);
    assert_eq!(o.timings[1].end, 10.0);
    assert_eq!(o.timings[0].host, None);
}

#[test]
fn unassigned_cancelled_job_is_not_cpu_work() {
    let mut cancelled = job(1, "cancelled", 0, 0, 100, &[]);
    cancelled.conclusion = Some("cancelled".into());
    let trace = vec![cancelled, job(2, "real", 0, 0, 10, &[])];
    let c = config(1);
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = engine::replay(&p, &c, Policy::Fifo, 1).unwrap();
    assert_eq!(p.diagnostics.unstarted_cancellations, 1);
    assert_eq!(o.timings[1].end, 10.0);
    assert_eq!(o.timings[0].host, None);
}

#[test]
fn arrivals_and_completions_change_speed_of_already_running_work() {
    let trace = vec![job(1, "a", 0, 0, 10, &[]), job(2, "b", 5, 20, 30, &[])];
    let mut c = config(1);
    c.pools[0].runners = 2;
    let mut p = PreparedTrace::new(&trace, &c).unwrap();
    p.fit.points = vec![(0.0, 1.0), (1.0, 1.0), (2.0, 2.0)];
    p.fit.cores_per_job = 1.0;
    p.fit.reference_cores = 1.0;
    let o = engine::replay(&p, &c, Policy::Baseline, 1).unwrap();
    assert_eq!(o.timings[0].end, 15.0);
    assert_eq!(o.timings[1].end, 20.0);
}

#[test]
fn memory_overcommit_adds_work_penalty_and_failure_exposure() {
    let trace = vec![job(1, "heavy", 0, 0, 10, &[])];
    let mut c = config(1);
    c.hosts[0].memory_gib = 1.0;
    c.default_demand.memory_gib = 2.0;
    c.default_demand.heavy = true;
    let mut p = PreparedTrace::new(&trace, &c).unwrap();
    p.fit.low_failure_rate = 0.0;
    p.fit.high_failure_rate = 1.0;
    let o = engine::replay(&p, &c, Policy::Baseline, 1).unwrap();
    assert_eq!(o.timings[0].end, 15.0);
    assert!(o.timings[0].failure);
}

#[test]
fn example_configuration_is_valid_and_typos_are_rejected() {
    let example: Config = toml::from_str(include_str!("../examples/hosts.toml")).unwrap();
    example.validate().unwrap();
    assert!(toml::from_str::<Config>("cpu_overcomit = 1.0").is_err());
}
