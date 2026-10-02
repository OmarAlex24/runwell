use crate::{
    Config, Policy, PreparedTrace,
    search::{SearchOptions, search_allocations},
    tests::job,
};

#[test]
fn bounded_search_is_exhaustive_ranked_and_deterministic_across_worker_counts() {
    let mut a = job(1, "a", 0, 0, 10, &[]);
    let mut b = job(2, "b", 0, 30, 40, &[]);
    a.repo = "example/a".into();
    b.repo = "example/b".into();
    let mut config = Config::default();
    config.hosts.push(config.hosts[0].clone());
    let p = PreparedTrace::new(&[a, b], &config).unwrap();
    let mut options = SearchOptions {
        max_runners_per_host: vec![2, 2],
        workers: 1,
        ..Default::default()
    };
    let single = search_allocations(&p, &options).unwrap();
    options.workers = 2;
    let parallel = search_allocations(&p, &options).unwrap();
    assert_eq!(single.evaluated, 38);
    assert_eq!(
        serde_json::to_string(&single).unwrap(),
        serde_json::to_string(&parallel).unwrap()
    );
    assert_eq!(single.allocations.len(), 10);
    for mode in single.allocations.chunks(5) {
        assert!(mode.windows(2).all(|p| p[0].score <= p[1].score));
    }
}

#[test]
fn median_target_prevents_a_p90_only_search_from_claiming_the_latency_goal() {
    let config = Config::default();
    let trace = PreparedTrace::new(&[job(1, "work", 0, 0, 60, &[])], &config).unwrap();
    let mut options = SearchOptions {
        max_runners_per_host: vec![1],
        target_p90_minutes: vec![2.0],
        ..Default::default()
    };
    let p90_only = search_allocations(&trace, &options).unwrap();
    assert_eq!(p90_only.allocations[0].score, 0.5);
    options.target_p50_minutes = vec![0.5];
    let both = search_allocations(&trace, &options).unwrap();
    assert_eq!(both.allocations[0].score, 2.0);
    options.target_p50_minutes = vec![0.0];
    assert!(search_allocations(&trace, &options).is_err());
}
#[test]
fn capacity_increase_wakes_queued_work_and_equivalence_includes_history() {
    let trace = vec![job(1, "a", 0, 0, 20, &[]), job(2, "b", 0, 30, 50, &[])];
    let mut config = Config::default();
    config.runner_history = vec![
        crate::config::RunnerCapacity {
            repo: "example/app".into(),
            host: 0,
            at: jiff::Timestamp::from_second(0).unwrap(),
            runners: 1,
        },
        crate::config::RunnerCapacity {
            repo: "example/app".into(),
            host: 0,
            at: jiff::Timestamp::from_second(5).unwrap(),
            runners: 2,
        },
    ];
    let p = PreparedTrace::new(&trace, &config).unwrap();
    let o = crate::engine::replay(&p, &config, Policy::Baseline, 1).unwrap();
    assert_eq!(o.timings[1].start, 5.0);
    assert!(crate::experiments::verify_equivalent(&p, 1).unwrap().passed);
}
