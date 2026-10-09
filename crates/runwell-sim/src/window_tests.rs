use crate::{
    Config, Policy, PreparedTrace,
    config::{Demand, Host},
    tests::job,
};
use jiff::Timestamp;
use runwell_trace::TraceJob;

fn at(second: i64) -> Option<Timestamp> {
    Some(Timestamp::from_second(second).unwrap())
}
fn host(class: &str, pattern: &str) -> Host {
    Host {
        class: class.into(),
        cores: 4,
        memory_gib: 16.0,
        runners: vec![pattern.into()],
    }
}
fn two_hosts() -> Config {
    Config {
        hosts: vec![host("a", "busy-\\d+"), host("b", "calm-\\d+")],
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

/// Before t = 10000, db takes twice as long on b and nothing overlaps. Afterwards
/// db takes four times as long on b, six builds overlap at three times their
/// low-load duration, and one of them fails.
fn shifting() -> Vec<TraceJob> {
    let mut trace = Vec::new();
    let mut id = 0;
    let mut add = |runner: &str, name: &str, start: i64, seconds: i64| {
        id += 1;
        trace.push(on(runner, id, name, start, seconds));
    };
    for k in 0..3 {
        add("busy-1", "db", 1000 * k, 10);
        add("calm-1", "db", 1000 * k + 100, 20);
        add("busy-1", "build", 1000 * k + 200, 10);
    }
    for k in 0..3 {
        add("busy-1", "db", 20_000 + 1000 * k, 10);
        add("calm-1", "db", 20_000 + 1000 * k + 100, 40);
    }
    for f in 1..=6 {
        add(&format!("busy-{f}"), "build", 30_000, 30);
    }
    trace.last_mut().unwrap().conclusion = Some("failure".into());
    trace
}

#[test]
fn held_out_jobs_fit_no_factor_curve_failure_rate_or_reservation() {
    let trace = shifting();
    let mut c = two_hosts();
    c.default_demand = Demand {
        heavy: true,
        ..Demand::default()
    };
    // Classes sort by name: build, db.
    let all = PreparedTrace::new(&trace, &c).unwrap();
    c.fit_until = at(10_000);
    let fit = PreparedTrace::new(&trace, &c).unwrap();
    assert!(close(fit.classes[1].host_factors[1].factor, 2.0));
    assert!(all.classes[1].host_factors[1].factor > 2.0);
    assert_eq!(fit.host_fit.as_ref().unwrap().anchor_samples, 9);
    assert_eq!(fit.classes[0].fit.inflation(6.0), 1.0);
    assert!(all.classes[0].fit.inflation(6.0) > 1.0);
    assert_eq!(fit.classes[0].fit.failure_samples, [3, 0]);
    assert_eq!(fit.classes[0].fit.low_failure_rate, 0.0);
    assert!(all.classes[0].fit.low_failure_rate > 0.0);
    // Held-out jobs still load their host and are replayed with the frozen model.
    assert_eq!(fit.jobs.len(), trace.len());
    assert!(fit.jobs.iter().all(|j| j.work > 0.0));
    // Reservations are sized from the peak while fit-window jobs ran.
    c.derive_cpu_demand = true;
    let fit = PreparedTrace::new(&trace, &c).unwrap();
    c.fit_until = None;
    let all = PreparedTrace::new(&trace, &c).unwrap();
    assert!(fit.classes.iter().all(|class| class.cpu_cores == 4));
    assert!(all.classes[0].cpu_cores < 4);
    // A window starting later fits only what starts inside it.
    c.fit_since = at(20_000);
    let late = PreparedTrace::new(&trace, &c).unwrap();
    assert!(close(late.classes[1].host_factors[1].raw.unwrap(), 4.0));
}

#[test]
fn report_window_selects_cohorts_and_calibration_uses_the_configured_hosts() {
    let trace: Vec<_> = (0..10)
        .map(|k| on("busy-1", k + 1, "work", 100 * k as i64, 10))
        .collect();
    let mut c = two_hosts();
    c.report_since = at(300);
    c.report_until = at(700);
    let p = PreparedTrace::new(&trace, &c).unwrap();
    assert_eq!(p.diagnostics.calibration_runs, 4);
    let report = crate::compare(&p, &[Policy::Baseline]).unwrap();
    assert!(report.rows.iter().all(|r| r.metrics.runs == 4));
    assert_eq!(report.calibration.len(), 1);
    assert!(report.markdown().contains("(one host, baseline)"));
    c.calibration_hosts = 2;
    let report = crate::simulate(&trace, &c, &[Policy::Baseline]).unwrap();
    assert_eq!(report.calibration.len(), 1);
    assert_eq!(report.assumptions["calibration_hosts"], 2);
    assert_eq!(report.assumptions["report_since"], "1970-01-01T00:05:00Z");
    assert!(report.markdown().contains("(2 hosts, baseline)"));
    c.calibration_hosts = 3;
    assert!(c.validate().is_err());
    c.calibration_hosts = 1;
    c.report_until = at(300);
    assert!(c.validate().is_err());
}
