//! Recover service work using the same pressure curve as forward replay.
use crate::{Config, model::Model, observation::Observation};
use std::collections::BTreeMap;

struct Segment {
    at: f64,
    cpu: f64,
    memory: f64,
}

/// Integrating speed over observed execution retains cache/input variation and
/// decontends interrupted work too. No queue latency or target quantile enters
/// this estimate. The reference host and RAM priors remain model assumptions.
/// With runner attribution each job integrates its own host's load and divides
/// by its class's intrinsic factor there, so work is in fastest-host seconds.
pub(crate) fn estimate(obs: &[Observation<'_>], model: &Model, config: &Config) -> Vec<[f64; 3]> {
    let hosts = if model.factors.is_empty() {
        1
    } else {
        config.hosts.len()
    };
    let mut events: Vec<Vec<(f64, f64, f64)>> = vec![Vec::new(); hosts];
    for o in obs.iter().filter(|o| o.local && o.net > 0.0) {
        let Some(h) = o.host.filter(|&h| h < hosts) else {
            continue;
        };
        for &(a, b) in &o.active {
            events[h].push((a, f64::from(o.demand.cores), o.demand.memory_gib));
            events[h].push((b, -f64::from(o.demand.cores), -o.demand.memory_gib));
        }
    }
    let segments: Vec<Vec<Segment>> = events.into_iter().map(segments).collect();
    let mut timelines = BTreeMap::new();
    for (o, &class) in obs.iter().zip(&model.class_ids) {
        if let Some(h) = o.host.filter(|&h| h < hosts && o.local && o.net > 0.0) {
            timelines
                .entry((h, class))
                .or_insert_with(|| timeline(&segments[h], h, class, model, config));
        }
    }
    obs.iter()
        .zip(&model.class_ids)
        .map(|(o, &class)| {
            if !o.local || o.net == 0.0 {
                return [0.0, o.net, 0.0];
            }
            match o.host.and_then(|h| timelines.get(&(h, class))) {
                Some(timeline) => {
                    o.phase_work(|a, b| integral(timeline, b) - integral(timeline, a))
                }
                // Unattributed: no host load is known, so keep the observed time.
                None => o.phase_work(|a, b| b - a),
            }
        })
        .collect()
}

fn segments(mut events: Vec<(f64, f64, f64)>) -> Vec<Segment> {
    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut segments: Vec<Segment> = Vec::new();
    let (mut cpu, mut memory) = (0.0, 0.0);
    for (at, dc, dm) in events {
        cpu += dc;
        memory += dm;
        if let Some(last) = segments.last_mut().filter(|last| last.at == at) {
            last.cpu = cpu;
            last.memory = memory;
        } else {
            segments.push(Segment { at, cpu, memory });
        }
    }
    segments
}

/// Cumulative work `(time, area, speed)` of one class on one host.
fn timeline(
    segments: &[Segment],
    host: usize,
    class: usize,
    model: &Model,
    config: &Config,
) -> Vec<(f64, f64, f64)> {
    let fit = if config.fit_by_class {
        &model.classes[class].fit
    } else {
        &model.pooled
    };
    let factor = model.factors.get(class).map(|row| row[host]);
    let mut timeline: Vec<(f64, f64, f64)> = Vec::new();
    let mut area = 0.0;
    for segment in segments {
        if let Some(&(at, _, speed)) = timeline.last() {
            area += (segment.at - at) * speed;
        }
        let speed = fit.speed(
            segment.cpu.max(0.0),
            f64::from(config.hosts[host].cores),
            segment.memory.max(0.0) / config.hosts[host].memory_gib,
            config.memory_threshold,
            config.memory_penalty,
        );
        timeline.push((segment.at, area, factor.map_or(speed, |f| speed / f)));
    }
    timeline
}

fn integral(timeline: &[(f64, f64, f64)], time: f64) -> f64 {
    let i = timeline.partition_point(|&(at, _, _)| at <= time);
    if i == 0 {
        0.0
    } else {
        let (at, area, speed) = timeline[i - 1];
        area + (time - at) * speed
    }
}
