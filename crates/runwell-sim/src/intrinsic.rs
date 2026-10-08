//! Recover service work using the same pressure curve as forward replay.
use crate::{Config, model::Model, observation::Observation};

struct Segment {
    at: f64,
    cpu: f64,
    memory: f64,
}

/// Integrating speed over observed execution retains cache/input variation and
/// decontends interrupted work too. No queue latency or target quantile enters
/// this estimate. The reference host and RAM priors remain model assumptions.
pub(crate) fn estimate(obs: &[Observation<'_>], model: &Model, config: &Config) -> Vec<[f64; 3]> {
    let mut events = Vec::new();
    for o in obs.iter().filter(|o| o.local && o.net > 0.0) {
        for &(a, b) in &o.active {
            events.push((a, f64::from(o.demand.cores), o.demand.memory_gib));
            events.push((b, -f64::from(o.demand.cores), -o.demand.memory_gib));
        }
    }
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
    let mut timelines = Vec::new();
    for class in &model.classes {
        let fit = if config.fit_by_class {
            &class.fit
        } else {
            &model.pooled
        };
        let mut timeline: Vec<(f64, f64, f64)> = Vec::new();
        let mut area = 0.0;
        for segment in &segments {
            if let Some(&(at, _, speed)) = timeline.last() {
                area += (segment.at - at) * speed;
            }
            let speed = fit.speed(
                segment.cpu.max(0.0),
                f64::from(config.hosts[0].cores),
                segment.memory.max(0.0) / config.hosts[0].memory_gib,
                config.memory_threshold,
                config.memory_penalty,
            );
            timeline.push((segment.at, area, speed));
        }
        timelines.push(timeline);
    }
    obs.iter()
        .zip(&model.class_ids)
        .map(|(o, &class)| {
            if !o.local || o.net == 0.0 {
                return [0.0, o.net, 0.0];
            }
            let timeline = &timelines[class];
            o.phase_work(|a, b| integral(timeline, b) - integral(timeline, a))
        })
        .collect()
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
