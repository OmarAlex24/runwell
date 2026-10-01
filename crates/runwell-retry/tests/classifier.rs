use runwell_retry::*;
fn evidence(text: &str, signals: &[Signal]) -> FailureEvidence {
    FailureEvidence {
        conclusion: "failure".into(),
        signals: signals.iter().copied().collect(),
        annotations_and_tail: text.into(),
    }
}
#[test]
fn every_shipped_rule_has_positive_and_veto_coverage() {
    let classifier = Classifier::builtin().unwrap();
    let cases = [
        ("oom", evidence("", &[Signal::OomKill])),
        ("runner-crash", evidence("", &[Signal::RunnerCrash])),
        ("runner-crash", evidence("", &[Signal::RunnerLost])),
        ("runner-crash", evidence("", &[Signal::NodeLost])),
        (
            "runner-crash",
            evidence("The runner has received a shutdown signal", &[]),
        ),
        (
            "runner-crash",
            evidence("lost communication with the server", &[]),
        ),
        (
            "docker-daemon",
            evidence("Cannot connect to the Docker daemon", &[]),
        ),
        ("docker-daemon", evidence("", &[Signal::DockerDaemonError])),
        (
            "pressure-timeout",
            evidence("", &[Signal::TimeoutAbovePsiBrake]),
        ),
        ("never-picked-up", evidence("", &[Signal::NeverPickedUp])),
        ("disk-full", evidence("No space left on device", &[])),
        (
            "registry-network",
            evidence("net/http: TLS handshake timeout", &[]),
        ),
    ];
    for (rule, mut input) in cases {
        assert_eq!(
            classifier.classify(&input),
            Classification {
                class: FailureClass::Infra,
                rule: rule.into()
            }
        );
        input
            .annotations_and_tail
            .push_str("\nAssertionError: values differ");
        assert_eq!(classifier.classify(&input).class, FailureClass::TestCode);
        input.annotations_and_tail.clear();
        input.signals.insert(Signal::CodeFailure);
        assert_eq!(classifier.classify(&input).class, FailureClass::TestCode);
    }
}
#[test]
fn red_tests_lint_generic_failures_and_timeouts_never_become_infra() {
    let c = Classifier::builtin().unwrap();
    for text in [
        "test result: FAILED",
        "Tests failed",
        "Lint failed",
        "ESLint found 2 errors",
        "failed to compile crate",
    ] {
        assert_eq!(
            c.classify(&evidence(text, &[Signal::OomKill])).class,
            FailureClass::TestCode
        );
    }
    for text in [
        "Process completed with exit code 1",
        "exit code 137",
        "timeout",
        "Error response from daemon: invalid image name",
        "",
    ] {
        assert_eq!(
            c.classify(&evidence(text, &[])).class,
            FailureClass::Unknown
        );
    }
    let mut timed_out = evidence("", &[]);
    timed_out.conclusion = "timed_out".into();
    assert_eq!(c.classify(&timed_out).class, FailureClass::Unknown);
    timed_out.signals.insert(Signal::TimeoutAbovePsiBrake);
    assert_eq!(c.classify(&timed_out).class, FailureClass::Infra);
    for conclusion in ["success", "cancelled", "skipped", "neutral", ""] {
        timed_out.conclusion = conclusion.into();
        assert_eq!(c.classify(&timed_out).class, FailureClass::Unknown);
    }
}
#[test]
fn custom_rules_cannot_remove_code_guards_and_malformed_rules_are_rejected() {
    let custom = "[[rule]]\nid='custom'\nclass='infra'\npatterns=['special fault']";
    let c = Classifier::from_toml(custom).unwrap();
    assert_eq!(
        c.classify(&evidence("special fault", &[])).class,
        FailureClass::Infra
    );
    assert_eq!(
        c.classify(&evidence("special fault; tests failed", &[]))
            .class,
        FailureClass::TestCode
    );
    assert!(Classifier::from_toml("[[rule]]\nid='x'\nclass='infra'\npatterns=['']").is_err());
    assert!(
        Classifier::from_toml("[[rule]]\nid='x'\nclass='infra'\nsignals=['exit_code']").is_err()
    );
}
