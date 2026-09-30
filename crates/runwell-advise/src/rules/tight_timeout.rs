//! A timeout near observed tails causes cancellations; missing timeouts can hold
//! self-hosted runners for six hours. With trace, detect <1.5*p90 or >1% explicit
//! job timeouts. Guard expression timeouts and test-level timeout messages. Missing
//! self-hosted job timeouts are also static warnings. Suggest a reviewed whole-job cap.
use super::*;
use crate::timing::Trace;
pub(super) fn check(job: &Job<'_>, trace: Option<&Trace>, out: &mut Vec<Finding>) {
    let timing = trace.and_then(|t| t.timing(job));
    let timeout = job.node.get("timeout-minutes");
    let value = timeout.and_then(|n| n.text().parse::<f64>().ok());
    let absent = timeout.is_none() && job.hosted();
    let tight = timing
        .as_ref()
        .is_some_and(|t| value.is_some_and(|v| v < 1.5 * t.work_p90) || t.timeout_rate > 0.01);
    if !absent && !tight {
        return;
    }
    let suggested = timing
        .as_ref()
        .map(|t| (1.5 * t.work_p90).ceil().max(value.unwrap_or(0.0) + 1.0));
    let snippet = suggested.map_or("timeout-minutes: <reviewed-whole-job-budget>".into(), |v| {
        format!("timeout-minutes: {v:.0}")
    });
    out.push(job_finding(job,timeout.unwrap_or(job.node),"tight-timeout",if absent { "No job timeout on a self-hosted runner: a hung job can hold capacity for the default 360 minutes." } else { "The job timeout is too close to the observed duration tail or explicit timeout cancellations exceed 1%. Include in-job admission waits in its budget." },&snippet,json!({"timeoutMinutes":value,"defaultMinutes":360,"timing":timing,"suggestedMinutes":suggested})));
}
