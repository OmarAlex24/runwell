//! Re-simulate the entire dependency graph: a shorter job can become critical.
use super::concurrency::{Timeline, host_matches};
use super::{
    critical::{Graph, wait_seconds},
    duration, percentile, seconds,
};
use regex::RegexSet;
use runwell_trace::TraceJob;
use std::collections::{BTreeMap, BTreeSet};

pub type Factors = BTreeMap<(String, String), f64>;

pub fn factors(
    jobs: &[TraceJob],
    timeline: &Timeline,
    labels: &[String],
    low: usize,
    waits: &RegexSet,
) -> Factors {
    let mut groups = BTreeMap::<_, (Vec<f64>, Vec<f64>)>::new();
    for j in jobs
        .iter()
        .filter(|j| host_matches(j, labels) && j.conclusion.as_deref() == Some("success"))
    {
        let (Some(s), Some(e), Some(d)) = (j.started_at, j.completed_at, duration(j)) else {
            continue;
        };
        let net = (d - wait_seconds(j, waits)).max(0.0);
        let entry = groups
            .entry((j.repo.clone(), j.job_name.clone()))
            .or_default();
        entry.1.push(net);
        let conc = timeline.average(s, e);
        if conc >= 1.0 && conc <= low as f64 {
            entry.0.push(net);
        }
    }
    groups
        .into_iter()
        .filter_map(|(k, (l, a))| {
            percentile(&l, 0.5)
                .zip(percentile(&a, 0.5))
                .map(|(l, a)| (k, if a > 0.0 { (l / a).min(1.0) } else { 1.0 }))
        })
        .collect()
}

pub fn estimate(jobs: &[&TraceJob], graph: &Graph, factors: &Factors, waits: &RegexSet) -> f64 {
    let mut remaining: BTreeSet<_> = (0..jobs.len()).collect();
    let mut finishes = vec![0.0f64; jobs.len()];
    while !remaining.is_empty() {
        let ready: Vec<_> = remaining
            .iter()
            .copied()
            .filter(|i| {
                graph.predecessors[*i]
                    .iter()
                    .all(|p| !remaining.contains(p))
            })
            .collect();
        if ready.is_empty() {
            return 0.0;
        }
        for i in ready {
            let j = jobs[i];
            let dependency = graph.predecessors[i]
                .iter()
                .map(|p| finishes[*p])
                .fold(0.0, f64::max);
            let observed_dependency = graph.predecessors[i]
                .iter()
                .filter_map(|p| jobs[*p].completed_at)
                .max()
                .or(j.run_created_at);
            let gap = observed_dependency
                .zip(j.created_at)
                .map(|(p, c)| seconds(p, c))
                .unwrap_or(0.0);
            let net = (duration(j).unwrap_or(0.0) - wait_seconds(j, waits)).max(0.0);
            let factor = factors
                .get(&(j.repo.clone(), j.job_name.clone()))
                .copied()
                .unwrap_or(1.0);
            finishes[i] = dependency + gap + net * factor;
            remaining.remove(&i);
        }
    }
    finishes.into_iter().fold(0.0, f64::max)
}
