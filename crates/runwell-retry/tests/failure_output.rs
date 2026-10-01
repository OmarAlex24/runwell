use runwell_retry::{Classifier, FailureClass, FailureEvidence, Signal};

const PYTEST_RED: &str =
    "FAILED tests/test_network.py::test_connection - OSError: [Errno 101] Network is unreachable";
fn evidence(text: &str, signals: &[Signal]) -> FailureEvidence {
    FailureEvidence {
        conclusion: "failure".into(),
        signals: signals.iter().copied().collect(),
        annotations_and_tail: text.into(),
    }
}

#[test]
fn framework_failure_summaries_veto_network_infra_patterns() {
    let classifier = Classifier::builtin().unwrap();
    for output in [
        PYTEST_RED,
        "\x1b[31mFAILED\x1b[0m tests/test_network.py::test_connection - net/http: TLS handshake timeout",
        "2026-10-01T00:00:01.123Z FAILED tests/test_network.py::test_connection - No space left on device",
        "ERROR tests/test_network.py::test_connection - network is unreachable",
        "FAIL: test_connection (tests.test_network.ClientTests)\nnetwork is unreachable",
        "--- FAIL: TestConnection (0.01s)\nnet/http: TLS handshake timeout",
        "test client::tests::connect ... FAILED\nregistry connection timed out",
        "FAIL tests/network.test.ts\nnet/http: TLS handshake timeout",
        "Test Suites: 1 failed, 2 passed, 3 total\nnet/http: TLS handshake timeout",
        "Tests  1 failed | 2 passed (3)\nregistry connection timed out",
        "1 failing\nNo space left on device",
        "not ok 1 - connects\nNo space left on device",
        "Tests run: 3, Failures: 1, Errors: 0, Skipped: 0\nregistry connection timed out",
        "================ 1 failed, 2 passed in 0.05s ================\nNo space left on device",
        "1 failed, 2 passed in 0.05s\nNo space left on device",
    ] {
        for signals in [&[][..], &[Signal::OomKill][..]] {
            assert_eq!(
                classifier.classify(&evidence(output, signals)).class,
                FailureClass::TestCode,
                "{output}"
            );
        }
    }
    let custom = Classifier::from_toml(
        "[[rule]]\nid='custom-network'\nclass='infra'\npatterns=['network is unreachable']",
    )
    .unwrap();
    assert_eq!(
        custom.classify(&evidence(PYTEST_RED, &[])).class,
        FailureClass::TestCode
    );
}

#[test]
fn ambiguous_application_network_errors_stay_unknown() {
    let classifier = Classifier::builtin().unwrap();
    for output in [
        "OSError: [Errno 101] Network is unreachable",
        "Temporary failure in name resolution",
        "dial tcp: i/o timeout",
        "Traceback (most recent call last):\n  File 'client.py', line 10\nOSError: No space left on device",
    ] {
        assert_eq!(
            classifier.classify(&evidence(output, &[])).class,
            FailureClass::Unknown,
            "{output}"
        );
    }
}

#[test]
fn lint_command_names_and_successful_summaries_do_not_veto_infra() {
    let classifier = Classifier::builtin().unwrap();
    for output in [
        "Run cargo clippy --workspace",
        "Run npx eslint src --max-warnings=0",
        "Run cargo clippy --workspace\nRun eslint .\nESLint found 0 errors",
        "Tests run: 3, Failures: 0, Errors: 0, Skipped: 0",
        "Tests: 0 failed, 3 passed, 3 total",
        "registry: 2 failed pulls",
    ] {
        assert_eq!(
            classifier
                .classify(&evidence(output, &[Signal::OomKill]))
                .class,
            FailureClass::Infra,
            "{output}"
        );
        let shutdown = format!("{output}\nThe runner has received a shutdown signal");
        assert_eq!(
            classifier.classify(&evidence(&shutdown, &[])).class,
            FailureClass::Infra,
            "{output}"
        );
        assert_eq!(
            classifier.classify(&evidence(output, &[])).class,
            FailureClass::Unknown,
            "{output}"
        );
    }
}

#[test]
fn actual_lint_diagnostics_still_veto_infra() {
    let classifier = Classifier::builtin().unwrap();
    for output in [
        "ESLint found 2 errors",
        "  2:3  error  'unused' is assigned a value but never used  no-unused-vars",
        "✖ 2 problems (2 errors, 0 warnings)",
        "error: this let-binding has unit value\n --> src/main.rs:1:1\n  = help: for further information visit https://rust-lang.github.io/rust-clippy/master/index.html#let_unit_value",
        "cargo clippy failed",
    ] {
        assert_eq!(
            classifier
                .classify(&evidence(output, &[Signal::OomKill]))
                .class,
            FailureClass::TestCode,
            "{output}"
        );
    }
}

#[test]
fn malformed_diagnostic_regex_is_rejected_without_echoing_its_content() {
    let result =
        Classifier::from_toml("[[rule]]\nid='bad'\nclass='test_code'\nregexes=['[private']");
    assert!(result.is_err());
    assert_eq!(
        result.err().unwrap().to_string(),
        "invalid infrastructure classifier rule table"
    );
}
