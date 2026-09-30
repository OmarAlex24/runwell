use crate::{
    Config, Policy, PreparedTrace,
    availability::{Cause, Record},
    config::Pool,
    tests::job,
};
fn ts(s: i64) -> jiff::Timestamp {
    jiff::Timestamp::from_second(s).unwrap()
}
fn config() -> Config {
    Config {
        pools: vec![Pool {
            repo: "example/app".into(),
            labels: vec![],
            job_names: vec![],
            runners: 2,
        }],
        ..Config::default()
    }
}
fn offline(runner: usize, start: i64, end: i64) -> Record {
    Record::RunnerOffline {
        host: 0,
        pool: 0,
        runner,
        start: ts(start),
        end: ts(end),
        cause: Cause::Broker,
    }
}
#[test]
fn busy_offline_runner_does_not_remove_another_free_slot() {
    let mut c = config();
    c.availability = vec![offline(0, 2, 20)];
    let trace = vec![
        job(1, "a", 0, 0, 10, &[]),
        job(2, "b", 3, 30, 40, &[]),
        job(3, "c", 11, 50, 60, &[]),
    ];
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = crate::engine::replay(&p, &c, Policy::Baseline, 1).unwrap();
    assert_eq!(o.timings[0].end, 10.);
    assert_eq!(o.timings[1].start, 3.);
    assert_eq!(o.timings[2].start, 13.);
    assert!(crate::experiments::verify_equivalent(&p, 1).unwrap().passed);
}
#[test]
fn runner_intervals_overlap_recover_at_endpoint_and_do_not_apply_to_runwell_by_default() {
    let mut c = config();
    c.availability = vec![offline(0, -5, 20), offline(0, 0, 30), offline(1, 0, 30)];
    let trace = vec![job(1, "a", 0, 0, 10, &[])];
    let p = PreparedTrace::new(&trace, &c).unwrap();
    assert_eq!(
        crate::engine::replay(&p, &c, Policy::Baseline, 1)
            .unwrap()
            .timings[0]
            .start,
        30.
    );
    assert_eq!(
        crate::engine::replay(&p, &c, Policy::Fifo, 1)
            .unwrap()
            .timings[0]
            .start,
        0.
    );
    c.runwell_runner_availability = true;
    assert_eq!(
        crate::engine::replay(&p, &c, Policy::Fifo, 1)
            .unwrap()
            .timings[0]
            .start,
        30.
    );
}
#[test]
fn whole_host_outages_pause_work_for_all_policies_and_block_held_jobs() {
    let mut c = config();
    c.default_demand.heavy = true;
    c.heavy_slots = Some(1);
    c.semaphore_timeout_seconds = 3.;
    c.availability = vec![
        Record::HostOffline {
            host: 0,
            start: ts(2),
            end: ts(20),
        },
        Record::HostOffline {
            host: 0,
            start: ts(10),
            end: ts(25),
        },
    ];
    let trace = vec![job(1, "a", 0, 0, 10, &[]), job(2, "b", 1, 30, 40, &[])];
    let p = PreparedTrace::new(&trace, &c).unwrap();
    for policy in Policy::ALL {
        let o = crate::engine::replay(&p, &c, policy, 1).unwrap();
        assert_eq!(o.timings[0].end, 33.);
        assert!(o.timings[1].end >= 34.);
    }
}
#[test]
fn jsonl_and_toml_support_size_history_and_reject_bad_intervals_and_indices() {
    let json = r#"{"kind":"pool_size","host":0,"pool":0,"at":"1970-01-01T00:00:00Z","runners":0}
{"kind":"pool_size","host":0,"pool":0,"at":"1970-01-01T00:00:05Z","runners":1}"#;
    let mut c = config();
    c.availability = crate::availability::parse_jsonl(json).unwrap();
    let trace = vec![job(1, "a", 0, 0, 10, &[])];
    let p = PreparedTrace::new(&trace, &c).unwrap();
    assert_eq!(
        crate::engine::replay(&p, &c, Policy::Baseline, 1)
            .unwrap()
            .timings[0]
            .start,
        5.
    );
    let toml = "[[events]]\nkind='runner_offline'\nhost=0\npool=0\nrunner=1\nstart='1970-01-01T00:00:00Z'\nend='1970-01-01T00:00:05Z'\ncause='service'";
    assert_eq!(crate::availability::parse_toml(toml).unwrap().len(), 1);
    c.availability = vec![offline(0, 5, 5)];
    assert!(PreparedTrace::new(&trace, &c).is_err());
    c.availability = vec![offline(2, 0, 5)];
    assert!(PreparedTrace::new(&trace, &c).is_err());
    c.availability = vec![Record::HostOffline {
        host: 9,
        start: ts(0),
        end: ts(1),
    }];
    assert!(PreparedTrace::new(&trace, &c).is_err());
    assert!(crate::availability::parse_jsonl("{bad}").is_err());
}

#[test]
fn outage_routes_work_to_another_host_and_restores_capacity_at_completion_tie() {
    let mut c = config();
    c.hosts.push(c.hosts[0].clone());
    c.availability = vec![Record::HostOffline {
        host: 0,
        start: ts(0),
        end: ts(10),
    }];
    let trace = vec![job(1, "a", 0, 0, 10, &[]), job(2, "b", 10, 20, 30, &[])];
    let p = PreparedTrace::new(&trace, &c).unwrap();
    for policy in Policy::ALL {
        let o = crate::engine::replay(&p, &c, policy, 2).unwrap();
        assert_eq!(o.timings[0].host, Some(1));
        assert_eq!(o.timings[0].end, 10.);
        assert_eq!(o.timings[1].host, Some(0));
        assert_eq!(o.timings[1].start, 10.);
    }
}
#[test]
fn static_allocations_keep_counts_and_cycle_recorded_runner_outage_patterns() {
    let mut c = config();
    c.availability = vec![
        offline(0, 0, 20),
        Record::PoolSize {
            host: 0,
            pool: 0,
            at: ts(0),
            runners: 0,
        },
    ];
    let trace: Vec<_> = (0..4)
        .map(|i| job(i + 1, "a", 0, i as i64 * 20, i as i64 * 20 + 10, &[]))
        .collect();
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o =
        crate::engine::replay_allocation(&p, &c, Policy::Baseline, 1, Some(&[vec![4]])).unwrap();
    assert_eq!(o.timings.iter().filter(|t| t.start == 0.).count(), 2);
    assert_eq!(o.timings.iter().filter(|t| t.start == 10.).count(), 2);
}
