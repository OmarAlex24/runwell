use crate::{
    Config, Policy, PreparedTrace,
    config::{Demand, Host},
    engine,
    tests::job,
};

#[test]
fn production_keeps_a_wakeup_consumed_within_event_tolerance() {
    let trace = [
        job(1, "first", 0, 0, 1, &[]),
        job(2, "second", 2, 2, 3, &[]),
    ];
    let config = Config {
        hosts: vec![Host {
            class: "small".into(),
            cores: 2,
            memory_gib: 1.0,
        }],
        default_demand: Demand {
            cores: 1,
            memory_gib: 1.0,
            ..Default::default()
        },
        memory_threshold: 0.5,
        memory_penalty: 1.99999999,
        // This regression injects a forward penalty into fixed synthetic work.
        preserve_work_variation: false,
        ..Default::default()
    };
    let trace = PreparedTrace::new(&trace, &config).unwrap();
    let fifo = engine::replay(&trace, &config, Policy::Fifo, 1).unwrap();
    assert!(fifo.timings[1].start < 2.0 && fifo.timings[1].start > 2.0 - 1e-7);
    let production = engine::replay(&trace, &config, Policy::Production, 1).unwrap();
    for (expected, actual) in fifo.timings.iter().zip(&production.timings) {
        assert_eq!(actual.start, expected.start);
        assert_eq!(actual.end, expected.end);
    }
}
