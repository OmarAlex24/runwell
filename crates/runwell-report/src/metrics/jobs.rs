use super::{duration, percent, percentiles, ran, seconds};
use crate::model::JobSummary;
use runwell_trace::TraceJob;
use std::collections::BTreeMap;

pub fn summarize(jobs: &[TraceJob]) -> Vec<JobSummary> {
    let mut groups = BTreeMap::<_, Vec<_>>::new();
    for job in jobs {
        groups
            .entry((
                &job.repo,
                job.workflow.as_deref().unwrap_or("unknown"),
                &job.job_name,
            ))
            .or_default()
            .push(job);
    }
    groups
        .into_iter()
        .map(|((repo, workflow, name), jobs)| {
            let d: Vec<_> = jobs.iter().filter_map(|j| duration(j)).collect();
            let q: Vec<_> = jobs
                .iter()
                .filter(|j| ran(j))
                .filter_map(|j| j.created_at.zip(j.started_at).map(|(c, s)| seconds(c, s)))
                .collect();
            let total = jobs
                .iter()
                .filter(|j| j.conclusion.as_deref() != Some("skipped"))
                .count();
            JobSummary {
                repo: repo.clone(),
                workflow: workflow.into(),
                job: name.clone(),
                count: jobs.len(),
                ran: d.len(),
                duration_seconds: percentiles(&d),
                queue_seconds: percentiles(&q),
                fail_percent: percent(
                    jobs.iter()
                        .filter(|j| j.conclusion.as_deref() == Some("failure"))
                        .count(),
                    total,
                ),
                cancel_percent: percent(
                    jobs.iter()
                        .filter(|j| j.conclusion.as_deref() == Some("cancelled"))
                        .count(),
                    total,
                ),
                runner_minutes: d.iter().sum::<f64>() / 60.0,
            }
        })
        .collect()
}
