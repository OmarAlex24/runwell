//! Lint serialized with tests lengthens critical jobs on a finite runner pool.
//! Detect lint/typecheck and test commands in one job; guard shard detection by
//! classifying test steps independently of strategy. Suggest a separate parallel
//! job while retaining every quality check and publish/deploy dependency.
use super::*;
use crate::context::{lint, test_tool};
pub(super) fn check(job: &Job<'_>, out: &mut Vec<Finding>) {
    if job.gate() || !job.steps().iter().any(|s| test_tool(s, job).is_some()) {
        return;
    }
    let lints: Vec<_> = job
        .steps()
        .iter()
        .enumerate()
        .filter(|(_, s)| lint(s))
        .map(|(i, s)| step_name(s, i))
        .collect();
    if lints.is_empty() {
        return;
    }
    out.push(job_finding(job,job.node,"lint-in-test-job","Lint/typecheck work shares a test job and extends its serial execution. Move quality checks into a separate parallel job.","jobs:\n  tests:\n    # Retain all existing tests and coverage.\n  lint:\n    runs-on: <same-runner-labels>\n    steps:\n      # Copy required setup and every lint/typecheck step.\n  publish:\n    needs: [tests, lint] # preserve existing release gates",json!({"lintSteps":lints})));
}
