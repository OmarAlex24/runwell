//! PR image builds behind quality add avoidable serialization. Detect image builds
//! and transitive quality dependencies (including aggregator jobs). Exclude
//! environments/default-ref/deploy gates and jobs restricted to push. Manual fix
//! builds in parallel while preserving publish/deploy gates and artifact inputs.
use super::*;
use crate::{
    context::{build, lint, test_tool},
    timing::Trace,
};
use std::collections::BTreeSet;
fn quality(job: &Job<'_>, id: &str, seen: &mut BTreeSet<String>) -> bool {
    if !seen.insert(id.into()) {
        return false;
    }
    job.workflow
        .jobs()
        .iter()
        .find(|j| j.id == id)
        .is_some_and(|j| {
            j.steps()
                .iter()
                .any(|s| lint(s) || test_tool(s, j).is_some())
                || j.needs().iter().any(|n| quality(job, n, seen))
        })
}
pub(super) fn check(job: &Job<'_>, trace: Option<&Trace>, out: &mut Vec<Finding>) {
    if !job.workflow.triggered("pull_request")
        || job.gate()
        || excludes_pr(job.node.str("if"))
        || !job.steps().iter().any(build)
    {
        return;
    }
    let dependencies: Vec<_> = job
        .needs()
        .into_iter()
        .filter(|n| quality(job, n, &mut BTreeSet::new()))
        .collect();
    if dependencies.is_empty() {
        return;
    }
    let timing = trace.and_then(|t| t.timing(job));
    let mut f = job_finding(
        job,
        job.node.key("needs").unwrap_or(job.node),
        "build-behind-quality",
        "PR image/artifact builds wait behind quality jobs. Build in parallel and gate only publishing/deployment on quality.",
        "jobs:\n  build:\n    # Keep routing/output dependencies; remove only reviewed quality dependencies.\n    steps:\n      - uses: docker/build-push-action@<pinned-version>\n        with:\n          push: false\n  publish:\n    needs: [build, tests, lint]\n    # Preserve all existing event, environment, and release gates.",
        json!({"qualityDependencies":dependencies,"timing":timing,"savingsAssumption":"sufficient capacity to overlap builds and quality; observed critical hop cost is an upper bound"}),
    );
    f.estimated_savings = timing.filter(|t| t.critical_samples > 0).map(|t| t.hop());
    out.push(f);
}

fn excludes_pr(condition: &str) -> bool {
    condition.contains("github.event_name != 'pull_request'")
        || condition.contains("github.event_name != \"pull_request\"")
        || ((condition.contains("github.event_name == 'push'")
            || condition.contains("github.event_name == \"push\""))
            && !condition.contains("pull_request")
            && !condition.contains("||"))
}
