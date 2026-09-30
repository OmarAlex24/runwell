//! Runner-clipped sweep-line concurrency and time-weighted interval queries.
use super::{ran, seconds};
use crate::model::{Concurrency, ConcurrencyLevel, LabelSaturation, TimelinePoint};
use jiff::Timestamp;
use runwell_trace::TraceJob;
use std::collections::{BTreeMap, BTreeSet};

pub struct Timeline {
    pub points: Vec<(Timestamp, usize)>,
    pub report: Concurrency,
}

pub fn host_matches(job: &TraceJob, labels: &[String]) -> bool {
    labels.is_empty() || labels.iter().all(|l| job.labels.contains(l))
}

pub fn timeline(
    jobs: &[TraceJob],
    start: Timestamp,
    end: Timestamp,
    labels: &[String],
    capacities: &BTreeMap<String, usize>,
) -> Timeline {
    let mut runners = BTreeMap::<_, Vec<_>>::new();
    for j in jobs.iter().filter(|j| ran(j) && host_matches(j, labels)) {
        if let (Some(s), Some(e), Some(r)) = (j.started_at, j.completed_at, j.runner_name.as_ref())
            && e > s
            && e > start
            && s < end
        {
            runners
                .entry(r)
                .or_default()
                .push((s.max(start), e.min(end), j));
        }
    }
    let mut report = Concurrency::default();
    let mut events = BTreeMap::<Timestamp, Vec<(i64, &TraceJob)>>::new();
    let mut label_runners = BTreeMap::<&str, BTreeSet<&str>>::new();
    for (runner, mut intervals) in runners {
        intervals.sort_by_key(|(s, e, _)| (*s, *e));
        let mut last = start;
        for (mut s, e, j) in intervals {
            if s < last {
                report.clipped_runner_overlaps += 1;
                s = last;
            }
            last = last.max(e);
            if e <= s {
                continue;
            }
            events.entry(s).or_default().push((1, j));
            events.entry(e).or_default().push((-1, j));
            for label in &j.labels {
                label_runners.entry(label).or_default().insert(runner);
            }
        }
    }
    events.entry(start).or_default();
    events.entry(end).or_default();
    let caps: BTreeMap<_, _> = label_runners
        .iter()
        .map(|(l, r)| (*l, capacities.get(*l).copied().unwrap_or(r.len())))
        .collect();
    let mut current = 0i64;
    let mut active = BTreeMap::<&str, i64>::new();
    let mut peaks = BTreeMap::<&str, usize>::new();
    let mut saturation = BTreeMap::<&str, f64>::new();
    let mut histogram = BTreeMap::<usize, f64>::new();
    let mut prev = start;
    let mut points = Vec::new();
    for (time, deltas) in events {
        let dt = seconds(prev, time);
        *histogram.entry(current.max(0) as usize).or_default() += dt;
        for (label, n) in &active {
            if *n >= caps.get(label).copied().unwrap_or(usize::MAX) as i64 {
                *saturation.entry(label).or_default() += dt;
            }
        }
        for (delta, j) in deltas {
            current += delta;
            for label in &j.labels {
                *active.entry(label).or_default() += delta;
            }
        }
        for (label, n) in &active {
            let p = peaks.entry(label).or_default();
            *p = (*p).max((*n).max(0) as usize);
        }
        let level = current.max(0) as usize;
        report.peak = report.peak.max(level);
        points.push((time, level));
        prev = time;
    }
    let span = seconds(start, end);
    let idle = histogram.get(&0).copied().unwrap_or(0.0);
    report.busy_seconds = (span - idle).max(0.0);
    report.idle_fraction = fraction(idle, span);
    report.distribution = histogram
        .into_iter()
        .map(|(level, s)| ConcurrencyLevel {
            level,
            seconds: s,
            wall_fraction: fraction(s, span),
            busy_fraction: if level == 0 {
                0.0
            } else {
                fraction(s, report.busy_seconds)
            },
        })
        .collect();
    report.labels = caps
        .into_iter()
        .map(|(label, capacity)| LabelSaturation {
            label: label.into(),
            capacity,
            capacity_inferred: !capacities.contains_key(label),
            peak: peaks.get(label).copied().unwrap_or(0),
            saturation_hours: saturation.get(label).copied().unwrap_or(0.0) / 3600.0,
        })
        .collect();
    report.timeline = points
        .iter()
        .map(|(t, c)| TimelinePoint {
            timestamp: t.to_string(),
            concurrency: *c,
        })
        .collect();
    Timeline { points, report }
}

fn fraction(n: f64, d: f64) -> f64 {
    if d > 0.0 { n / d } else { 0.0 }
}

impl Timeline {
    pub fn average(&self, start: Timestamp, end: Timestamp) -> f64 {
        if end <= start {
            return 0.0;
        }
        let mut i = self.points.partition_point(|(t, _)| *t <= start);
        let mut level = if i == 0 { 0 } else { self.points[i - 1].1 };
        let mut cur = start;
        let mut acc = 0.0;
        while let Some(&(time, next)) = self.points.get(i) {
            if time >= end {
                break;
            }
            acc += level as f64 * seconds(cur, time);
            cur = time;
            level = next;
            i += 1;
        }
        acc += level as f64 * seconds(cur, end);
        acc / seconds(start, end)
    }
}
