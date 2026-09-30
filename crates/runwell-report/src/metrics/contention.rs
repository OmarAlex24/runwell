use super::{
    concurrency::{Timeline, host_matches},
    critical::wait_seconds,
    duration, percentiles, seconds,
};
use crate::model::Contention;
use regex::RegexSet;
use runwell_trace::TraceJob;
use std::collections::BTreeMap;

pub fn summarize(
    jobs: &[TraceJob],
    timeline: &Timeline,
    host_labels: &[String],
    low: usize,
    high: usize,
    waits: &RegexSet,
) -> Vec<Contention> {
    let mut groups = BTreeMap::<_, (Vec<f64>, Vec<f64>)>::new();
    for j in jobs
        .iter()
        .filter(|j| host_matches(j, host_labels) && j.conclusion.as_deref() == Some("success"))
    {
        let (Some(s), Some(e), Some(d)) = (j.started_at, j.completed_at, duration(j)) else {
            continue;
        };
        let conc = timeline.average(s, e);
        let bucket = if conc >= 1.0 && conc <= low as f64 {
            0
        } else if conc >= high as f64 {
            1
        } else {
            continue;
        };
        let mut add = |step: Option<&str>, value: f64| {
            let entry = groups
                .entry((
                    j.repo.as_str(),
                    j.job_name.as_str(),
                    step.map(str::to_owned),
                ))
                .or_default();
            if bucket == 0 {
                entry.0.push(value);
            } else {
                entry.1.push(value);
            }
        };
        add(None, (d - wait_seconds(j, waits)).max(0.0));
        for step in &j.steps {
            if step.conclusion.as_deref() != Some("success") {
                continue;
            }
            if let (Some(s), Some(e)) = (step.started_at, step.completed_at)
                && e >= s
            {
                add(Some(&step.name), seconds(s, e));
            }
        }
    }
    groups
        .into_iter()
        .map(|((repo, job, step), (l, h))| {
            let low = percentiles(&l);
            let high = percentiles(&h);
            let ratio = low
                .p50
                .zip(high.p50)
                .and_then(|(l, h)| (l > 0.0).then_some(h / l));
            Contention {
                repo: repo.into(),
                job: job.into(),
                step,
                low,
                high,
                high_to_low_ratio: ratio,
            }
        })
        .collect()
}
