use crate::{Config, Policy, PreparedTrace, tests::job};

#[test]
fn peak_load_has_no_spike_at_simultaneous_departures_and_arrivals() {
    let occupancy = crate::contention::Occupancy::weighted(&[(5., 10., 3.), (0., 5., 4.)]);
    assert_eq!(occupancy.peak(), 4.);
    assert_eq!(occupancy.mean(0., 10.), 3.5);
}

#[test]
fn a_held_runner_does_not_count_as_cpu_contention() {
    let mut held = job(1, "held", 0, 0, 100, &[]);
    held.steps = vec![runwell_trace::TraceStep {
        name: "acquire".into(),
        started_at: Some(jiff::Timestamp::from_second(0).unwrap()),
        completed_at: Some(jiff::Timestamp::from_second(90).unwrap()),
        conclusion: Some("success".into()),
    }];
    let trace = vec![held, job(2, "work", 0, 0, 80, &[])];
    let config = Config {
        semaphore_steps: vec!["acquire".into()],
        ..Config::default()
    };
    let mut obs =
        crate::observation::read(&trace, &config, &mut crate::Diagnostics::default()).unwrap();
    crate::model::fit(&mut obs, &config);
    assert_eq!(obs[0].net, 10.);
    assert_eq!(obs[0].concurrency, 1.);
    assert_eq!(obs[1].concurrency, 1.);
}

#[test]
fn sensitivity_is_fitted_per_class_and_cpu_proxy_reflects_it() {
    let mut trace = vec![
        job(1, "sensitive", 0, 0, 10, &[]),
        job(2, "steady", 20, 20, 30, &[]),
    ];
    for i in 0..8 {
        let (name, end) = if i < 4 {
            ("sensitive", 140)
        } else {
            ("steady", 110)
        };
        trace.push(job(3 + i, name, 100, 100, end, &[]));
    }
    let config = Config {
        derive_cpu_demand: true,
        ..Config::default()
    };
    let p = PreparedTrace::new(&trace, &config).unwrap();
    assert_eq!(p.classes.len(), 2);
    assert!(p.classes[0].fit.inflation(7.) > p.classes[1].fit.inflation(7.));
    assert!(p.classes[0].cpu_cores > p.classes[1].cpu_cores);
    assert_eq!(p.classes[1].fit.inflation(7.), 1.);
}

#[test]
fn sweep_has_all_factors_and_equivalent_is_not_resource_scaled() {
    let config = Config {
        overcommit_sweep: crate::experiments::OVERCOMMIT_SWEEP.to_vec(),
        ..Config::default()
    };
    let report = crate::simulate(&[job(1, "a", 0, 0, 10, &[])], &config, &Policy::ALL).unwrap();
    assert_eq!(report.rows.len(), 26);
    assert_eq!(
        report
            .rows
            .iter()
            .filter(|r| r.overcommit.is_none())
            .count(),
        2
    );
    assert!(report.equivalence[0].passed);
}

#[test]
fn named_pools_route_jobs_before_repository_fallback() {
    let config = Config {
        pools: vec![crate::config::Pool {
            repo: "example/app".into(),
            labels: vec!["local".into()],
            job_names: vec!["special".into()],
            runners: 1,
        }],
        ..Config::default()
    };
    let p = PreparedTrace::new(
        &[
            job(1, "special", 0, 0, 10, &[]),
            job(2, "other", 0, 20, 30, &[]),
        ],
        &config,
    )
    .unwrap();
    assert_ne!(p.jobs[0].pool, p.jobs[1].pool);
    assert_eq!(p.pool_limits, [1, 6]);
}
