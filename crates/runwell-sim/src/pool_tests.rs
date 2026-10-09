use crate::{
    Config, Policy, PreparedTrace,
    config::{Demand, Host, JobDemand, Semaphore, SlotChange},
    engine,
    semaphore_tests::{config, step},
    tests::job,
};
use runwell_trace::TraceJob;

fn host(class: &str) -> Host {
    Host {
        class: class.into(),
        cores: 4,
        memory_gib: 16.0,
        runners: Vec::new(),
    }
}
/// Heavy pool from `semaphore_tests::config` plus a `db` pool on two hosts.
fn pools(db_slots: Vec<usize>) -> Config {
    let mut c = config();
    c.hosts = vec![host("a"), host("b")];
    c.semaphores = vec![Semaphore {
        name: "db".into(),
        acquire_steps: vec!["Acquire db".into()],
        release_steps: vec!["Release db".into()],
        slots: None,
        slots_per_host: db_slots,
        slot_history: Vec::new(),
        timeout_seconds: None,
        poll_seconds: None,
    }];
    c.jobs = vec![JobDemand {
        name: "db".into(),
        repo: None,
        demand: Demand {
            semaphore: Some("db".into()),
            ..Demand::default()
        },
    }];
    c
}
fn db(id: u64, end: i64) -> TraceJob {
    let mut j = job(id, "db", 0, 0, end, &[]);
    j.steps = vec![step("Acquire db", 0, 0), step("Release db", end, end)];
    j
}
fn heavy(id: u64, end: i64) -> TraceJob {
    let mut j = job(id, "build", 0, 0, end, &[]);
    j.steps = vec![
        step("Acquire capacity", 0, 0),
        step("Release capacity", end, end),
    ];
    j
}

#[test]
fn named_pool_has_per_host_slots_and_leaves_heavy_unchanged() {
    // Three runners per host: jobs 0-2 land on host 0, jobs 3-5 on host 1.
    let trace = vec![
        db(1, 10),
        db(2, 10),
        db(3, 10),
        db(4, 10),
        db(5, 10),
        heavy(6, 10),
    ];
    let c = pools(vec![1, 2]);
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let runners = [vec![3], vec![3]];
    let o = engine::replay_allocation(&p, &c, Policy::Baseline, 2, Some(&runners)).unwrap();
    let ends: Vec<_> = o.timings.iter().map(|t| t.end).collect();
    assert_eq!(ends, [10., 20., 30., 10., 10., 10.]);
    assert_eq!(o.timings[2].semaphore_wait, 20.);
    // The heavy job shares host 1 with two db holders but uses its own pool.
    assert_eq!(o.timings[5].semaphore_wait, 0.);
    let o = engine::replay_allocation(&p, &c, Policy::Equivalent, 2, Some(&runners)).unwrap();
    assert!(o.timings.iter().all(|t| t.end == 10.));
    assert!(crate::experiments::verify_equivalent(&p, 2).unwrap().passed);
    // A second heavy job on the same host still waits on heavy's single slot.
    let trace = vec![heavy(1, 10), heavy(2, 10), db(3, 10)];
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = engine::replay(&p, &c, Policy::Baseline, 1).unwrap();
    let ends: Vec<_> = o.timings.iter().map(|t| t.end).collect();
    assert_eq!(ends, [10., 20., 10.]);
}

#[test]
fn slot_history_changes_a_host_limit_and_wakes_waiters() {
    let mut c = pools(vec![1]);
    c.semaphores[0].slot_history = vec![SlotChange {
        host: 0,
        at: jiff::Timestamp::from_second(5).unwrap(),
        slots: 2,
    }];
    let trace = vec![db(1, 20), db(2, 10), db(3, 10)];
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = engine::replay(&p, &c, Policy::Baseline, 1).unwrap();
    assert_eq!(o.timings[1].semaphore_wait, 5.);
    assert_eq!(o.timings[1].end, 15.);
    assert_eq!(o.timings[2].end, 25.); // Two slots: waits for the first release.
    c.semaphores[0].poll_seconds = Some(4.);
    let o = engine::replay(&p, &c, Policy::Baseline, 1).unwrap();
    assert_eq!(o.timings[1].end, 18.); // The increase is seen at the next poll.
}

#[test]
fn history_uses_each_pool_own_acquire_step() {
    let mut ungated = job(2, "db", 0, 0, 10, &[]);
    ungated.steps = vec![step("ordinary", 0, 10)];
    let trace = vec![db(1, 10), ungated];
    let mut c = pools(vec![1]);
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = engine::replay(&p, &c, Policy::Baseline, 1).unwrap();
    assert_eq!(o.timings[1].end, 10.);
    c.semaphore_history = false;
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = engine::replay(&p, &c, Policy::Baseline, 1).unwrap();
    assert_eq!(o.timings[1].end, 20.);
}

#[test]
fn pool_configuration_errors_are_explicit() {
    let mut c = pools(vec![1]);
    c.jobs[0].demand.semaphore = Some("missing".into());
    assert!(c.validate().is_err());
    let mut c = pools(vec![1]);
    c.jobs[0].demand.heavy = true;
    assert!(c.validate().is_err());
    for name in ["heavy", ""] {
        let mut c = pools(vec![1]);
        c.semaphores[0].name = name.into();
        assert!(c.validate().is_err());
    }
    let mut c = pools(vec![0]);
    assert!(c.validate().is_err());
    c.semaphores[0].slots_per_host = vec![1, 1, 1];
    assert!(c.validate().is_err());
    // One step matching two pools' fragments is ambiguous, not silently assigned.
    let mut c = pools(vec![1]);
    c.semaphores[0].acquire_steps = vec!["Acquire".into()];
    assert!(PreparedTrace::new(&[heavy(1, 10)], &c).is_err());
}
