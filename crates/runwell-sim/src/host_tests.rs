use crate::{Config, Policy, PreparedTrace, config::Host, engine, tests::job};

#[test]
fn host_semaphores_have_independent_limits() {
    let config = Config {
        hosts: vec![
            Host {
                class: "a".into(),
                cores: 4,
                memory_gib: 16.0,
            },
            Host {
                class: "b".into(),
                cores: 4,
                memory_gib: 16.0,
            },
        ],
        heavy_slots: Some(1),
        heavy_slots_per_host: vec![2, 1],
        default_demand: crate::config::Demand {
            heavy: true,
            ..Default::default()
        },
        observed_work: true,
        ..Default::default()
    };
    let trace: Vec<_> = (0..12).map(|i| job(i, "work", 0, 0, 10, &[])).collect();
    let prepared = PreparedTrace::new(&trace, &config).unwrap();
    let outcome = engine::replay(&prepared, &config, Policy::Baseline, 2).unwrap();
    for (host, expected) in [(0, 2), (1, 1)] {
        let active = outcome
            .timings
            .iter()
            .filter(|t| t.host == Some(host) && t.start + t.semaphore_wait == 0.0)
            .count();
        assert_eq!(active, expected);
    }
    assert!(
        crate::experiments::verify_equivalent(&prepared, 2)
            .unwrap()
            .passed
    );
}

#[test]
fn invalid_per_host_semaphore_limits_are_rejected() {
    for limits in [vec![0], vec![1, 1]] {
        let config = Config {
            heavy_slots_per_host: limits,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }
}
