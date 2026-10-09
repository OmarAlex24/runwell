use crate::{
    Config, Policy, PreparedTrace,
    config::{Host, RunnerChoice, SpeedFactor},
    contention::quantile,
    engine,
    tests::job,
};
use runwell_trace::TraceJob;

fn host(class: &str, patterns: &[&str]) -> Host {
    Host {
        class: class.into(),
        cores: 4,
        memory_gib: 16.0,
        runners: patterns.iter().map(|p| (*p).into()).collect(),
    }
}
fn two_hosts() -> Config {
    Config {
        hosts: vec![host("a", &["busy-\\d+"]), host("b", &["calm-\\d+"])],
        ..Config::default()
    }
}
fn on(runner: &str, id: u64, name: &str, start: i64, seconds: i64) -> TraceJob {
    let mut j = job(id, name, start, start, start + seconds, &[]);
    j.runner_name = Some(runner.into());
    j
}
fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn runners_attribute_to_one_host_and_the_rest_are_reported() {
    let mut c = two_hosts();
    c.hosts[0].runners.push("shared-\\d+".into());
    c.hosts[1].runners.push("shared-.+".into());
    c.anonymize = false;
    let mut skipped = on("other-1", 7, "work", 60, 0);
    skipped.conclusion = Some("skipped".into());
    let mut no_runner = job(8, "work", 70, 70, 75, &[]);
    no_runner.runner_name = None;
    let trace = vec![
        on("busy-10", 1, "work", 0, 5),
        on("calm-2", 2, "work", 10, 5),
        on("shared-1", 3, "work", 20, 5), // matches both hosts
        on("gamma-1", 4, "work", 30, 5),  // matches neither
        on("busy-1x", 5, "work", 40, 5),  // patterns match whole names only
        skipped,
        no_runner,
    ];
    let p = PreparedTrace::new(&trace, &c).unwrap();
    assert_eq!(p.diagnostics.unattributed_jobs, 4);
    assert_eq!(
        p.diagnostics.unattributed_runners,
        ["busy-1x", "gamma-1", "shared-1"]
    );
    c.anonymize = true;
    let p = PreparedTrace::new(&trace, &c).unwrap();
    assert_eq!(p.diagnostics.unattributed_jobs, 4);
    assert!(p.diagnostics.unattributed_runners.is_empty());
    // Without patterns every job stays on the aggregate host, with no diagnostic.
    let p = PreparedTrace::new(&trace, &Config::default()).unwrap();
    assert_eq!(p.diagnostics.unattributed_jobs, 0);
    assert!(p.host_fit.is_none() && p.classes[0].host_factors.is_empty());
    c.hosts[1].runners = vec!["calm-(".into()];
    assert!(c.validate().is_err());
}

/// Host a is intrinsically as fast as b for `build` but busy, so its median build
/// is twice b's. Host b is twice as slow for `db` even when idle.
fn confounded() -> Vec<TraceJob> {
    let mut trace = Vec::new();
    let mut id = 0;
    let mut add = |runner: &str, name: &str, start: i64, seconds: i64| {
        id += 1;
        trace.push(on(runner, id, name, start, seconds));
    };
    for f in 1..=5 {
        add(&format!("busy-{f}"), "filler", 0, 1000);
    }
    for k in 0..6 {
        add("busy-6", "build", 100 + 50 * k, 20); // load 6: twice as long
    }
    for k in 0..2 {
        add("busy-1", "build", 2000 + 50 * k, 10);
        add("calm-1", "build", 2000 + 50 * k, 10);
        add("busy-1", "db", 3000 + 50 * k, 10);
        add("calm-1", "db", 3000 + 50 * k, 20);
    }
    trace
}

#[test]
fn low_load_factors_separate_a_busy_host_from_a_slow_one() {
    let trace = confounded();
    let median = |runner: &str, name: &str| {
        let v: Vec<_> = trace
            .iter()
            .filter(|j| j.job_name == name && j.runner_name.as_deref().unwrap().starts_with(runner))
            .map(|j| (j.completed_at.unwrap() - j.started_at.unwrap()).get_seconds() as f64)
            .collect();
        quantile(&v, 0.5)
    };
    // The naive ratio of host medians calls the busy host twice as slow.
    assert_eq!(median("busy", "build") / median("calm", "build"), 2.0);
    let p = PreparedTrace::new(&trace, &two_hosts()).unwrap();
    // Classes sort by name: build, db, filler.
    let factor = |class: usize, host: usize| p.classes[class].host_factors[host].factor;
    assert!(close(factor(0, 0), 1.0) && close(factor(0, 1), 1.0));
    assert!(close(factor(1, 0), 1.0) && close(factor(1, 1), 2.0));
    assert_eq!(p.classes[1].host_factors[1].samples, 2);
    // Load is left to the shared curve, and not counted twice.
    assert!(close(p.classes[0].fit.inflation(6.0), 2.0));
    assert_eq!(p.classes[1].fit.inflation(6.0), 1.0);
    // Intrinsic work is in fastest-host seconds.
    assert!(
        p.jobs
            .iter()
            .filter(|j| j.class == 1)
            .all(|j| close(j.work, 10.0))
    );
}

#[test]
fn replay_applies_the_host_factor_and_overrides_only_replay() {
    let trace: Vec<_> = (0..3)
        .flat_map(|k| {
            [
                on("busy-1", 2 * k + 1, "db", 100 * k as i64, 10),
                on("calm-1", 2 * k + 2, "db", 100 * k as i64 + 50, 20),
            ]
        })
        .collect();
    let mut c = two_hosts();
    let ends = |c: &Config, runners: [usize; 2]| {
        let p = PreparedTrace::new(&trace, c).unwrap();
        let alloc = [vec![runners[0]], vec![runners[1]]];
        let o = engine::replay_allocation(&p, c, Policy::Baseline, 2, Some(&alloc)).unwrap();
        o.timings
            .iter()
            .map(|t| t.end - t.start)
            .collect::<Vec<_>>()
    };
    assert!(ends(&c, [1, 0]).iter().all(|&d| close(d, 10.0)));
    assert!(ends(&c, [0, 1]).iter().all(|&d| close(d, 20.0)));
    c.speed_factors = vec![SpeedFactor {
        host: 1,
        job: "db".into(),
        repo: None,
        factor: 1.0,
    }];
    assert!(ends(&c, [0, 1]).iter().all(|&d| close(d, 10.0)));
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let detail = &p.classes[0].host_factors[1];
    assert!(detail.factor == 1.0 && close(detail.fitted, 2.0));
    c.speed_factors[0].job = "missing".into();
    assert!(PreparedTrace::new(&trace, &c).is_err());
    c.speed_factors[0].host = 2;
    assert!(c.validate().is_err());
}

#[test]
fn sparse_classes_shrink_toward_no_host_difference() {
    // Every class runs 1.5x longer on b. Two classes vary within a name, so the
    // noise scale is positive; `sparse` has one sample on b, `local` none.
    let mut trace = Vec::new();
    let mut id = 0;
    let mut t = 0;
    let mut add = |runner: &str, name: &str, seconds: i64| {
        id += 1;
        t += 100;
        trace.push(on(runner, id, name, t, seconds));
    };
    for k in 0..12 {
        let jitter = [0, 2, 4, 6][k % 4];
        for name in ["dense", "other"] {
            add("busy-1", name, 20 + jitter);
            add("calm-1", name, 30 + jitter * 3 / 2);
        }
        add("busy-1", "sparse", 20);
        add("busy-1", "local", 20);
    }
    add("calm-1", "sparse", 30);
    let p = PreparedTrace::new(&trace, &two_hosts()).unwrap();
    let fit = p.host_fit.as_ref().unwrap();
    assert!(fit.log_sigma.is_some_and(|s| s > 0.0) && fit.log_tau_squared > 0.0);
    // Classes sort by name: dense, local, other, sparse.
    let dense = &p.classes[0].host_factors[1];
    let local = &p.classes[1].host_factors[1];
    let sparse = &p.classes[3].host_factors[1];
    assert!(close(dense.raw.unwrap(), 1.5) && close(sparse.raw.unwrap(), 1.5));
    assert!(sparse.weight < dense.weight && dense.weight < 1.0);
    assert!(1.0 < sparse.fitted && sparse.fitted < dense.fitted && dense.fitted < 1.5);
    assert_eq!(local.samples, 0);
    assert!(local.raw.is_none() && local.factor == 1.0 && local.weight == 0.0);
}

#[test]
fn uniform_runner_choice_spreads_jobs_by_idle_runners() {
    let trace: Vec<_> = (0..400)
        .map(|k| on("busy-1", k + 1, "work", 100 * k as i64, 10))
        .collect();
    let mut c = two_hosts();
    let share = |c: &Config| {
        let p = PreparedTrace::new(&trace, c).unwrap();
        let alloc = [vec![3], vec![1]];
        let o = engine::replay_allocation(&p, c, Policy::Baseline, 2, Some(&alloc)).unwrap();
        o.timings.iter().filter(|t| t.host == Some(1)).count() as f64 / trace.len() as f64
    };
    assert_eq!(share(&c), 0.0);
    c.runner_choice = RunnerChoice::Uniform;
    let b = share(&c);
    assert!((b - 0.25).abs() < 0.06, "host b share {b}");
    let p = PreparedTrace::new(&trace, &c).unwrap();
    assert!(crate::experiments::verify_equivalent(&p, 2).unwrap().passed);
}
