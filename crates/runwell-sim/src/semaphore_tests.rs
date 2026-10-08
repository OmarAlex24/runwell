use crate::{Config, Policy, PreparedTrace, engine, tests::job};
use runwell_trace::{TraceJob, TraceStep};

fn step(name: &str, start: i64, end: i64) -> TraceStep {
    TraceStep {
        name: name.into(),
        started_at: Some(jiff::Timestamp::from_second(start).unwrap()),
        completed_at: Some(jiff::Timestamp::from_second(end).unwrap()),
        conclusion: Some("success".into()),
    }
}
fn config() -> Config {
    let mut c = Config {
        observed_work: true,
        heavy_slots: Some(1),
        semaphore_steps: vec!["Acquire capacity".into()],
        semaphore_release_steps: vec!["Release capacity".into()],
        ..Config::default()
    };
    c.default_demand.heavy = true;
    c
}
fn phased(id: u64, end: i64, acquire: (i64, i64), release: i64) -> TraceJob {
    let mut j = job(id, "build", 0, 0, end, &[]);
    j.steps = vec![
        step("Acquire capacity", acquire.0, acquire.1),
        step("Release capacity", release, release),
    ];
    j
}

#[test]
fn setup_precedes_acquisition_and_cleanup_follows_release() {
    let trace = vec![phased(1, 30, (2, 2), 12), phased(2, 25, (5, 12), 22)];
    let c = config();
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = engine::replay(&p, &c, Policy::Baseline, 1).unwrap();
    assert_eq!(p.jobs[1].work, 18.); // The observed wait is not service work.
    assert_eq!(o.timings[0].end, 30.);
    assert_eq!(o.timings[1].end, 25.); // Starts protected work during first cleanup.
    let o = engine::replay(&p, &c, Policy::Equivalent, 1).unwrap();
    assert_eq!(o.timings[1].end, 18.); // Removing the gate also removes its wait.
}

#[test]
fn missing_release_holds_until_death_and_history_ignores_skipped_acquire() {
    let mut first = phased(1, 20, (2, 2), 8);
    first.steps.pop();
    let mut historical = phased(3, 9, (3, 3), 8);
    historical.steps[0].conclusion = Some("skipped".into());
    let trace = vec![first, phased(2, 30, (4, 20), 28), historical];
    let c = config();
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = engine::replay(&p, &c, Policy::Baseline, 1).unwrap();
    assert_eq!(o.timings[1].semaphore_wait, 16.);
    assert_eq!(o.timings[1].end, 30.);
    assert_eq!(o.timings[2].end, 9.);
    assert_eq!(o.timings[2].semaphore_wait, 0.);
}

#[test]
fn fail_open_clock_begins_at_acquire_and_does_not_release_another_token() {
    let trace = vec![
        phased(1, 100, (2, 2), 100),
        phased(2, 24, (10, 17), 22),
        phased(3, 36, (23, 30), 34),
    ];
    let mut c = config();
    c.semaphore_timeout_seconds = 7.;
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = engine::replay(&p, &c, Policy::Baseline, 1).unwrap();
    assert_eq!(o.timings[1].end, 24.);
    assert_eq!(o.timings[2].end, 36.);
    assert_eq!(o.timings[1].semaphore_wait, 7.);
    assert_eq!(o.timings[2].semaphore_wait, 7.);
    assert_eq!(o.fail_opens, 2);
}

#[test]
fn step_wait_holds_runner_and_counts_toward_critical_queue() {
    let trace = vec![phased(1, 20, (2, 2), 12), phased(2, 25, (5, 12), 22)];
    let c = config();
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = engine::replay(&p, &c, Policy::Baseline, 1).unwrap();
    assert_eq!(o.timings[1].start, 0.);
    assert_eq!(o.timings[1].semaphore_wait, 7.);
    let report = crate::simulate(&trace, &c, &[Policy::Baseline]).unwrap();
    assert!((report.rows[0].metrics.queue_share - 7. / 45.).abs() < 1e-9);
    let o = engine::replay_allocation(&p, &c, Policy::Baseline, 1, Some(&[vec![1]])).unwrap();
    assert_eq!(o.timings[1].start, 20.); // Cleanup still occupies the only runner.
    assert_eq!(o.timings[1].end, 38.);
}

#[test]
fn phase_integrals_use_local_speed_and_preserve_total_work() {
    let trace = vec![phased(1, 40, (10, 20), 30)];
    let c = config();
    let obs = crate::observation::read(&trace, &c, &mut crate::Diagnostics::default()).unwrap();
    let work = obs[0].phase_work(|a, b| (b - a) * if a < 25. { 0.5 } else { 1. });
    assert_eq!(work, [5., 5., 10.]);
}

#[test]
fn cancellation_in_protected_work_releases_the_slot() {
    let mut first = phased(1, 100, (2, 2), 100);
    first.cancel_group = Some("replace".into());
    let mut replacement = job(3, "replacement", 6, 6, 7, &[]);
    replacement.cancel_group = first.cancel_group.clone();
    replacement.steps = vec![step("ordinary", 6, 7)];
    let trace = vec![first, phased(2, 12, (4, 6), 10), replacement];
    let c = config();
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = engine::replay(&p, &c, Policy::Baseline, 1).unwrap();
    assert_eq!(o.timings[0].end, 6.);
    assert_eq!(o.timings[1].semaphore_wait, 2.);
    assert_eq!(o.timings[1].end, 12.);
}

#[test]
fn polling_allows_barging_without_fitting_wait_durations() {
    let trace = vec![
        phased(1, 9, (1, 1), 9),
        phased(2, 23, (3, 18), 23),
        phased(3, 14, (7, 12), 14),
    ];
    let mut c = config();
    c.semaphore_poll_seconds = 5.;
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = engine::replay(&p, &c, Policy::Baseline, 1).unwrap();
    assert_eq!(o.timings[1].semaphore_wait, 15.);
    assert_eq!(o.timings[2].semaphore_wait, 5.);
    assert_eq!(o.timings[1].end, 23.);
    assert_eq!(o.timings[2].end, 14.);
}

#[test]
fn polling_caps_sleep_at_timeout_and_acquisition_wins_at_deadline() {
    for release in [8, 20] {
        let trace = vec![
            phased(1, release, (0, 0), release),
            phased(2, 10, (1, 8), 10),
        ];
        let mut c = config();
        c.semaphore_poll_seconds = 5.;
        c.semaphore_timeout_seconds = 7.;
        let p = PreparedTrace::new(&trace, &c).unwrap();
        let o = engine::replay(&p, &c, Policy::Baseline, 1).unwrap();
        assert_eq!(o.timings[1].semaphore_wait, 7.);
        assert_eq!(o.timings[1].end, 10.);
        assert_eq!(o.fail_opens, usize::from(release > 8));
    }
}

#[test]
fn default_deadline_polls_on_the_interval_and_fails_open_after_900_seconds() {
    // Polls at 1, 8, ..., 897, then a shortened final attempt at the 901 deadline.
    for (release, wait, fail_opens) in [(450, 455., 0), (899, 900., 0), (950, 900., 1)] {
        let trace = vec![
            phased(1, release, (0, 0), release),
            phased(2, 910, (1, 901), 905),
        ];
        let mut c = config();
        assert_eq!(c.semaphore_timeout_seconds, 900.);
        c.semaphore_poll_seconds = 7.; // Does not divide the timeout.
        let p = PreparedTrace::new(&trace, &c).unwrap();
        let o = engine::replay(&p, &c, Policy::Baseline, 1).unwrap();
        assert_eq!(o.timings[1].semaphore_wait, wait); // A release at 450 waits for the 456 poll.
        assert_eq!(o.timings[1].end, 1. + wait + 9.);
        assert_eq!(o.fail_opens, fail_opens);
    }
}

#[test]
fn skipped_steps_do_not_subtract_work_and_repeated_acquisitions_are_explicit_errors() {
    let mut j = phased(1, 20, (2, 12), 18);
    j.steps[0].conclusion = Some("skipped".into());
    let c = config();
    let p = PreparedTrace::new(&[j.clone()], &c).unwrap();
    assert_eq!(p.jobs[0].work, 20.);
    j.steps[0].conclusion = Some("success".into());
    j.steps.push(step("Acquire capacity", 19, 19));
    assert!(PreparedTrace::new(&[j], &c).is_err());
    for interval in [-1., f64::NAN, f64::INFINITY] {
        let mut c = config();
        c.semaphore_poll_seconds = interval;
        assert!(c.validate().is_err());
    }
}

#[test]
fn cancellation_during_wait_preserves_start_and_frees_runner() {
    let mut waiting = phased(2, 12, (4, 6), 10);
    waiting.cancel_group = Some("replace".into());
    let mut replacement = job(3, "replacement", 6, 6, 7, &[]);
    replacement.cancel_group = waiting.cancel_group.clone();
    replacement.steps = vec![step("ordinary", 6, 7)];
    let trace = vec![phased(1, 100, (2, 2), 100), waiting, replacement];
    let c = config();
    let p = PreparedTrace::new(&trace, &c).unwrap();
    let o = engine::replay_allocation(&p, &c, Policy::Baseline, 1, Some(&[vec![2]])).unwrap();
    assert_eq!(o.timings[1].start, 0.);
    assert_eq!(o.timings[1].end, 6.);
    assert_eq!(o.timings[1].semaphore_wait, 2.);
    assert_eq!(o.timings[2].end, 7.);
}
