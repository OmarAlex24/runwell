//! Host-wide semaphore waits hold scarce runners idle. Detect blocking acquisition
//! scripts or explicitly named waits for slots/locks. Guard release/unlock steps,
//! background waits, lockfile operations, and nonblocking flock. Suggest scheduler
//! admission or concurrency; trace evidence measures waiting runner-minutes.
use super::*;
use crate::{
    context::commands,
    model::Savings,
    timing::{Trace, percentile},
};
pub(super) fn check(job: &Job<'_>, trace: Option<&Trace>, out: &mut Vec<Finding>) {
    if !job.hosted() {
        return;
    }
    for (i, s) in job.steps().iter().enumerate() {
        let script = commands(s).to_lowercase();
        let name = s.str("name").to_lowercase();
        if script.contains("release")
            || script.contains("unlock")
            || script.contains("flock -n")
            || script.contains("flock --nonblock")
        {
            continue;
        }
        let acquisition = script.contains("flock ")
            || ((script.contains("slot")
                || script.contains("semaphore")
                || script.contains("lock"))
                && (script.contains("acquire")
                    || script.contains("while ")
                    || script.contains("sleep ")));
        let named = name.contains("wait")
            && ["slot", "semaphore", "lock"]
                .iter()
                .any(|x| name.contains(x));
        if !acquisition && !named {
            continue;
        }
        let values = trace.map_or(vec![], |t| t.waiting_minutes(job, s.str("name")));
        let mut f = job_finding(
            job,
            s,
            "host-semaphore-holds-runner",
            "A host-wide admission wait occupies a runner while idle. Use runwell scheduler-level admission or a workflow concurrency group instead.",
            "concurrency:\n  group: <capacity-pool-or-resource>\n  cancel-in-progress: false\n# Prefer CPU-aware scheduler admission for multi-slot resources; concurrency permits one running job per group.",
            json!({"step":step_name(s,i),"waitSamples":values.len(),"runnerMinutesWaiting":values.iter().sum::<f64>(),"waitP50Minutes":percentile(&values,0.5),"waitP90Minutes":percentile(&values,0.9)}),
        );
        f.step = Some(step_name(s, i));
        if !values.is_empty() {
            f.estimated_savings = Some(Savings {
                p50: percentile(&values, 0.5),
                p90: percentile(&values, 0.9),
            });
        }
        out.push(f);
    }
}
