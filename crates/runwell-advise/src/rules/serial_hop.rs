//! Each needs hop re-enters a busy self-hosted queue. Detect short downstream work
//! or queue longer than work; static mode uses small non-test/non-build jobs.
//! Exclude environment/ref/deploy/release gates and retain output/artifact edges.
//! Manual suggestions preserve required checks; measured hop cost is an upper bound.
use super::*;
use crate::{
    context::{build, lint, test_tool},
    timing::Trace,
};
pub(super) fn check(job: &Job<'_>, trace: Option<&Trace>, out: &mut Vec<Finding>) {
    if !job.hosted() || job.gate() || job.needs().is_empty() {
        return;
    }
    let timing = trace.and_then(|t| t.timing(job));
    let short = timing.as_ref().map_or_else(
        || {
            job.steps().len() <= 3
                && !job
                    .steps()
                    .iter()
                    .any(|s| test_tool(s, job).is_some() || build(s) || lint(s))
        },
        |t| t.work_p50 < 2.0 || t.queue_p50 > t.work_p50,
    );
    if !short {
        return;
    }
    let consumes = job
        .node
        .strings()
        .iter()
        .any(|s| s.contains("needs.") && s.contains(".outputs"))
        || job
            .steps()
            .iter()
            .any(|s| s.str("uses").contains("download-artifact"));
    let suggestion = if consumes {
        "Merge the short work into an upstream job while retaining outputs, artifacts, and required-check semantics."
    } else {
        "Merge into an upstream job, or drop needs only after verifying it is not a required status gate and consumes no upstream outputs or artifacts."
    };
    let mut f = job_finding(
        job,
        job.node.key("needs").unwrap_or(job.node),
        "serial-hop",
        "This dependency hop pays another runner queue wait for little work and can extend the critical path.",
        suggestion,
        json!({"needs":job.needs(),"consumesOutputsOrArtifacts":consumes,"timing":timing,"costInterpretation":"work plus queue for observed critical-path instances; upper bound, not additive"}),
    );
    f.estimated_savings = timing.filter(|t| t.critical_samples > 0).map(|t| t.hop());
    out.push(f);
}
