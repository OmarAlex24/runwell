//! Persistent runners retain manually started containers after failures. Detect
//! docker run/compose up with no always() stop/down/rm cleanup, excluding GitHub
//! services (managed automatically). Info-only; suggest teardown preserving gates.
use super::*;
use crate::context::commands;
pub(super) fn check(job: &Job<'_>, out: &mut Vec<Finding>) {
    if !job.hosted() {
        return;
    }
    let starts = job.steps().iter().any(|s| {
        let c = commands(s);
        c.contains("docker run")
            || (c.contains("docker compose") && c.contains(" up"))
            || (c.contains("docker-compose") && c.contains(" up"))
    });
    let cleanup = job.steps().iter().any(|s| {
        let c = commands(s);
        s.str("if").contains("always()")
            && (c.contains("docker rm")
                || c.contains("docker stop")
                || ((c.contains("docker compose") || c.contains("docker-compose"))
                    && c.contains(" down")))
    });
    if starts && !cleanup {
        let mut f = job_finding(
            job,
            job.node,
            "missing-self-hosted-cleanup",
            "Manually started containers can survive failed jobs on persistent runners. Add unconditional cleanup.",
            "- name: Clean up containers\n  if: always()\n  run: docker compose down --remove-orphans\n# Use explicit job-owned container names for docker rm/stop.",
            json!({"startsContainers":true,"alwaysCleanup":false}),
        );
        f.severity = Severity::Info;
        out.push(f);
    }
}
