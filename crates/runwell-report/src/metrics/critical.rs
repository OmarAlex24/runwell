//! Critical-path reconstruction with explicit dependencies or timestamp inference.
use super::{duration, seconds};
use regex::RegexSet;
use runwell_trace::TraceJob;
use std::collections::{BTreeMap, BTreeSet};

pub struct Graph {
    pub predecessors: Vec<Vec<usize>>,
    pub explicit: bool,
}

#[derive(Default, Debug)]
pub struct Composition {
    pub queue: f64,
    pub work: f64,
    pub gap: f64,
    pub wait: f64,
}

pub fn wait_seconds(job: &TraceJob, patterns: &RegexSet) -> f64 {
    let (Some(start), Some(end)) = (job.started_at, job.completed_at) else {
        return 0.0;
    };
    let mut intervals: Vec<_> = job
        .steps
        .iter()
        .filter(|s| patterns.is_match(&s.name))
        .filter_map(|s| s.started_at.zip(s.completed_at))
        .map(|(s, e)| (s.max(start), e.min(end)))
        .filter(|(s, e)| e > s)
        .collect();
    intervals.sort();
    let mut last = start;
    let mut total = 0.0;
    for (s, e) in intervals {
        total += seconds(s.max(last), e);
        last = last.max(e);
    }
    total.min(duration(job).unwrap_or(0.0))
}

pub fn graph(jobs: &[&TraceJob]) -> Graph {
    let names: BTreeMap<_, _> = jobs
        .iter()
        .enumerate()
        .map(|(i, j)| (&j.job_name, i))
        .collect();
    if names.len() == jobs.len() && jobs.iter().all(|j| j.needs.is_some()) {
        let mut predecessors = Vec::new();
        let mut valid = true;
        for job in jobs {
            let mut deps = Vec::new();
            for name in job.needs.iter().flatten() {
                if let Some(i) = names.get(name) {
                    deps.push(*i);
                } else {
                    valid = false;
                }
            }
            predecessors.push(deps);
        }
        let result = Graph {
            predecessors,
            explicit: true,
        };
        if valid && acyclic(&result) {
            return result;
        }
    }
    let predecessors = jobs
        .iter()
        .enumerate()
        .map(|(i, current)| {
            let Some(created) = current.created_at.or(current.started_at) else {
                return Vec::new();
            };
            jobs.iter()
                .enumerate()
                .filter(|(k, j)| {
                    *k != i
                        && j.completed_at.is_some_and(|e| {
                            e.as_nanosecond() <= created.as_nanosecond() + 3_000_000_000
                                && current.started_at.is_some_and(|s| e <= s)
                                && current.completed_at.is_some_and(|end| e < end)
                                && current.run_created_at.is_none_or(|c| e >= c)
                        })
                })
                .max_by_key(|(_, j)| j.completed_at)
                .map(|(k, _)| vec![k])
                .unwrap_or_default()
        })
        .collect();
    Graph {
        predecessors,
        explicit: false,
    }
}

fn acyclic(graph: &Graph) -> bool {
    let mut remaining: BTreeSet<_> = (0..graph.predecessors.len()).collect();
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
            return false;
        }
        for i in ready {
            remaining.remove(&i);
        }
    }
    true
}

pub fn path(jobs: &[&TraceJob], graph: &Graph) -> Vec<usize> {
    let mut current = jobs
        .iter()
        .enumerate()
        .max_by_key(|(_, j)| j.completed_at)
        .map(|(i, _)| i);
    let mut path = Vec::new();
    let mut seen = BTreeSet::new();
    while let Some(i) = current {
        if !seen.insert(i) {
            break;
        }
        path.push(i);
        current = graph.predecessors[i]
            .iter()
            .copied()
            .max_by_key(|p| jobs[*p].completed_at);
    }
    path.reverse();
    path
}

pub fn composition(jobs: &[&TraceJob], graph: &Graph, waits: &RegexSet) -> Composition {
    let mut result = Composition::default();
    let mut previous = jobs.iter().filter_map(|j| j.run_created_at).min();
    for i in path(jobs, graph) {
        let j = jobs[i];
        let (Some(s), Some(e), Some(prev)) = (j.started_at, j.completed_at, previous) else {
            continue;
        };
        let created = j.created_at.unwrap_or(s).max(prev).min(s);
        result.gap += seconds(prev, created);
        result.queue += seconds(created, s);
        let wait = wait_seconds(j, waits).min(seconds(s.max(prev), e));
        result.wait += wait;
        result.work += seconds(s.max(prev), e) - wait;
        previous = Some(e);
    }
    result
}
