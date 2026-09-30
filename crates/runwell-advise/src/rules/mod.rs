//! Independent workflow rules and shared diagnostic construction.
mod build_behind_quality;
mod fixed_service_ports;
mod host_semaphore_holds_runner;
mod lint_in_test_job;
mod missing_concurrency;
mod missing_self_hosted_cleanup;
mod push_and_pr_duplicate;
mod serial_hop;
mod shardable_tests;
mod shared_home_cache;
mod tight_timeout;
use crate::{
    context::{Job, Workflow},
    model::{Finding, Fix, Severity},
    timing::Trace,
    yaml::Node,
};
use serde_json::{Value, json};

pub(crate) const IDS: &[&str] = &[
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
pub(crate) fn analyze(w: &Workflow<'_>, trace: Option<&Trace>) -> Vec<Finding> {
    let mut out = Vec::new();
    missing_concurrency::check(w, &mut out);
    push_and_pr_duplicate::check(w, &mut out);
    for job in w.jobs() {
        serial_hop::check(&job, trace, &mut out);
        shardable_tests::check(&job, trace, &mut out);
        lint_in_test_job::check(&job, &mut out);
        build_behind_quality::check(&job, trace, &mut out);
        host_semaphore_holds_runner::check(&job, trace, &mut out);
        shared_home_cache::check(&job, &mut out);
        tight_timeout::check(&job, trace, &mut out);
        fixed_service_ports::check(&job, &mut out);
        missing_self_hosted_cleanup::check(&job, &mut out);
    }
    out
}
pub(crate) fn finding(
    w: &Workflow<'_>,
    job: &str,
    node: &Node,
    rule: &str,
    message: &str,
    snippet: &str,
    evidence: Value,
) -> Finding {
    Finding {
        rule: rule.into(),
        severity: Severity::Warn,
        file: w.file.into(),
        line: node.span.line,
        job: job.into(),
        step: None,
        message: message.into(),
        evidence: json!({"source": {"line":node.span.line,"column":node.span.column + 1,"startByte":node.span.start,"endByte":node.span.end}, "observations":evidence}),
        fix: Fix {
            kind: "manual".into(),
            snippet: snippet.into(),
            reason: None,
        },
        estimated_savings: None,
        edit: None,
    }
}
pub(crate) fn job_finding(
    job: &Job<'_>,
    node: &Node,
    rule: &str,
    message: &str,
    snippet: &str,
    evidence: Value,
) -> Finding {
    finding(job.workflow, job.id, node, rule, message, snippet, evidence)
}
pub(crate) fn step_name(step: &Node, index: usize) -> String {
    if step.str("name").is_empty() {
        format!("step {}", index + 1)
    } else {
        step.str("name").into()
    }
}
pub(crate) fn auto(f: &mut Finding, edit: Result<crate::model::Edit, String>) {
    match edit {
        Ok(e) => {
            f.fix.kind = "auto".into();
            f.edit = Some(e);
        }
        Err(reason) => f.fix.reason = Some(reason),
    }
}
