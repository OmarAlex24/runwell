//! Synthetic table-driven positive, negative, and false-positive cases per rule.
use runwell_advise::{
    analyze,
    cli::{AdviseArgs, Format, SelfHosted, execute},
};
use std::{
    fs,
    path::{Path, PathBuf},
};
const RULES: &[&str] = &[
    "missing-concurrency",
    "push-and-pr-duplicate",
    "serial-hop",
    "shardable-tests",
    "lint-in-test-job",
    "build-behind-quality",
    "host-semaphore-holds-runner",
    "shared-home-cache",
    "tight-timeout",
    "fixed-service-ports",
    "missing-self-hosted-cleanup",
];
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}
#[test]
fn every_rule_has_positive_negative_and_guard_cases() {
    for rule in RULES {
        for (case, expected) in [("positive", true), ("negative", false), ("guard", false)] {
            let path = fixture(&format!("{rule}-{case}.yml"));
            let ids: Vec<String> = if *rule == "tight-timeout" {
                let args = AdviseArgs {
                    workflows: path,
                    trace: Some(fixture("timing.jsonl")),
                    format: Format::Json,
                    fix: false,
                    rule: vec![],
                    self_hosted: SelfHosted::Auto,
                };
                let (text, _) = execute(&args).unwrap();
                let v: serde_json::Value = serde_json::from_str(&text).unwrap();
                v["findings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|f| f["rule"].as_str().unwrap().into())
                    .collect()
            } else {
                analyze(&fs::read_to_string(path).unwrap())
                    .unwrap()
                    .iter()
                    .map(|f| f.rule.clone())
                    .collect()
            };
            assert_eq!(
                ids.iter().any(|id| id == rule),
                expected,
                "{rule} {case}: {ids:?}"
            );
        }
    }
}
#[test]
fn anchors_merges_flow_documents_and_reusable_jobs() {
    let source = "name: checks\non: [push, pull_request]\nbase: &base\n  runs-on: self-hosted\n  steps:\n    - run: pytest\njobs:\n  tests:\n    <<: *base\n  reusable:\n    uses: example/project/.github/workflows/check.yml@main\n---\nname: other\non: workflow_dispatch\njobs: {simple: {runs-on: ubuntu-latest, steps: [{run: 'echo ok'}]}}\n";
    let findings = analyze(source).unwrap();
    assert!(
        findings
            .iter()
            .any(|f| f.rule == "shardable-tests" && f.job == "tests")
    );
    assert!(findings.iter().all(|f| f.job != "reusable"));
    assert!(analyze("on: [\n").is_err());
    assert!(analyze("jobs: {}\njobs: {}\n").is_err());
}
#[test]
fn tools_have_native_or_explicit_manual_partition_suggestions() {
    for command in [
        "go test ./...",
        "gotestsum -- ./...",
        "pytest",
        "jest",
        "vitest run",
        "cargo test",
        "cargo nextest run",
        "playwright test",
        "rspec",
        "phpunit",
    ] {
        let source = format!(
            "on: pull_request\njobs:\n  tests:\n    runs-on: ubuntu-latest\n    steps:\n      - run: {command}\n"
        );
        let f = analyze(&source)
            .unwrap()
            .into_iter()
            .find(|f| f.rule == "shardable-tests")
            .unwrap_or_else(|| panic!("missing test detection: {command}"));
        assert!(f.fix.snippet.contains("shard") || f.fix.snippet.contains("SHARDS"));
        assert_eq!(f.fix.kind, "manual");
    }
}
#[test]
fn self_hosted_overrides_and_dynamic_labels() {
    for (labels, expected) in [
        ("ubuntu-latest", false),
        ("ubuntu-26.04", false),
        ("windows-2025-vs2026", false),
        ("[self-hosted, linux]", true),
        ("custom-pool", true),
        ("${{ matrix.runner }}", false),
    ] {
        let text = format!(
            "on: workflow_dispatch\njobs:\n  check:\n    runs-on: {labels}\n    steps:\n      - run: echo ok\n"
        );
        assert_eq!(
            analyze(&text)
                .unwrap()
                .iter()
                .any(|f| f.rule == "tight-timeout"),
            expected,
            "{labels}"
        );
    }
}

#[test]
fn image_builds_for_pr_and_push_are_still_parallel_candidates() {
    let source = "on: [pull_request, push]\njobs:\n  image:\n    if: github.event_name == 'pull_request' || github.event_name == 'push'\n    needs: quality\n    runs-on: self-hosted\n    steps:\n      - uses: docker/bake-action@v6\n  quality:\n    runs-on: self-hosted\n    steps:\n      - run: cargo test\n";
    assert!(
        analyze(source)
            .unwrap()
            .iter()
            .any(|f| f.rule == "build-behind-quality")
    );
    let excluded = source.replace(
        "github.event_name == 'pull_request' || github.event_name == 'push'",
        "github.event_name == 'push'",
    );
    assert!(
        !analyze(&excluded)
            .unwrap()
            .iter()
            .any(|f| f.rule == "build-behind-quality")
    );
}

#[test]
fn pnpm_env_supports_current_and_legacy_versions() {
    for (version, key) in [
        ("11", "pnpm_config_store_dir"),
        ("10", "npm_config_store_dir"),
    ] {
        let source = format!(
            "on: pull_request\njobs:\n  check:\n    runs-on: self-hosted\n    env:\n      {key}: ${{{{ runner.temp }}}}/pnpm\n    steps:\n      - uses: pnpm/action-setup@v4\n        with:\n          version: '{version}'\n      - run: pnpm install\n"
        );
        assert!(
            !analyze(&source)
                .unwrap()
                .iter()
                .any(|f| f.rule == "shared-home-cache"),
            "{version}"
        );
    }
}
