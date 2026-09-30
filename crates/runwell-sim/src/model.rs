use crate::{
    Config,
    contention::{ContentionFit, Occupancy, Sample, quantile},
    observation::Observation,
};
use serde::Serialize;
use std::collections::BTreeMap;

/// Independently fitted job-class slowdown and a relative CPU reservation proxy.
#[derive(Debug, Clone, Serialize)]
pub struct ClassModel {
    /// Stable anonymized class identity; no private job names are emitted.
    pub class: String,
    /// CPU reservation inferred from sensitivity, or the configured estimate.
    pub cpu_cores: u32,
    /// RAM prior from configuration; the trace does not identify memory usage.
    pub memory_gib: f64,
    /// Class-specific slowdown and failure support.
    pub fit: ContentionFit,
}
pub(crate) struct Model {
    pub pooled: ContentionFit,
    pub classes: Vec<ClassModel>,
    pub class_ids: Vec<usize>,
    pub medians: BTreeMap<(String, String), f64>,
}

pub(crate) fn fit(observations: &mut [Observation<'_>], config: &Config) -> Model {
    let intervals: Vec<_> = observations
        .iter()
        .filter(|o| o.local && o.net > 0.0)
        .flat_map(|o| {
            if config.exclude_semaphore_waits_from_contention {
                o.active.clone()
            } else {
                vec![(o.start, o.end)]
            }
        })
        .collect();
    let occupancy = Occupancy::new(&intervals);
    for o in observations.iter_mut() {
        o.concurrency = if config.exclude_semaphore_waits_from_contention && o.net > 0.0 {
            o.active
                .iter()
                .map(|&(a, b)| occupancy.mean(a, b) * (b - a))
                .sum::<f64>()
                / o.net
        } else {
            occupancy.mean(o.start, o.end)
        };
    }
    let mut low: BTreeMap<(String, String), Vec<f64>> = BTreeMap::new();
    for o in observations.iter().filter(|o| {
        o.local
            && o.net > 0.0
            && o.concurrency <= config.low_concurrency
            && o.raw.conclusion.as_deref() == Some("success")
    }) {
        low.entry((o.raw.repo.clone(), o.raw.job_name.clone()))
            .or_default()
            .push(o.net);
    }
    let medians: BTreeMap<_, _> = low
        .into_iter()
        .map(|(k, v)| (k, quantile(&v, 0.5)))
        .collect();
    let keys: std::collections::BTreeSet<_> = observations
        .iter()
        .map(|o| {
            (
                o.raw.repo.clone(),
                o.raw
                    .workflow_job_id
                    .clone()
                    .unwrap_or_else(|| o.raw.job_name.clone()),
            )
        })
        .collect();
    let class_index: BTreeMap<_, _> = keys.into_iter().enumerate().map(|(i, k)| (k, i)).collect();
    let mut class_ids = Vec::new();
    let mut class_samples = vec![Vec::new(); class_index.len()];
    let mut all = Vec::new();
    for o in observations.iter() {
        let key = (
            o.raw.repo.clone(),
            o.raw
                .workflow_job_id
                .clone()
                .unwrap_or_else(|| o.raw.job_name.clone()),
        );
        let id = class_index[&key];
        class_ids.push(id);
        if o.local
            && o.net > 0.0
            && o.raw.conclusion.as_deref() == Some("success")
            && let Some(&median) = medians.get(&(o.raw.repo.clone(), o.raw.job_name.clone()))
        {
            let sample = Sample {
                concurrency: o.concurrency,
                inflation: o.net / median,
            };
            class_samples[id].push(sample);
            all.push(sample);
        }
    }
    let reference = f64::from(config.hosts[0].cores);
    let anchor = reference / occupancy.peak().max(1.0);
    let mut classes: Vec<_> = class_samples
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let fit = ContentionFit::fit(s, config.low_concurrency, 1.0, reference);
            ClassModel {
                class: format!("class {}", i + 1),
                cpu_cores: (anchor * fit.inflation(config.high_concurrency))
                    .round()
                    .clamp(1.0, reference) as u32,
                memory_gib: config.default_demand.memory_gib,
                fit,
            }
        })
        .collect();
    for (o, &id) in observations.iter_mut().zip(&class_ids) {
        if config.derive_cpu_demand && o.local {
            o.demand.cores = classes[id].cpu_cores;
        }
        classes[id].memory_gib = o.demand.memory_gib;
        if !config.derive_cpu_demand {
            classes[id].cpu_cores = o.demand.cores;
        }
    }
    let work = observations
        .iter()
        .filter(|o| o.local)
        .map(|o| o.net)
        .sum::<f64>();
    let mean_cores = observations
        .iter()
        .filter(|o| o.local)
        .map(|o| o.net * f64::from(o.demand.cores))
        .sum::<f64>()
        / work.max(1.0);
    if config.derive_cpu_demand && config.fit_proxy_load {
        // Fit on the same aggregate proxy-demand axis used during replay. Raw
        // job counts alone would apply class sensitivity twice when the job mix
        // changes. The reservations above are inferred once, never target-tuned.
        let intervals: Vec<_> = observations
            .iter()
            .filter(|o| o.local && o.net > 0.0)
            .flat_map(|o| {
                let active = if config.exclude_semaphore_waits_from_contention {
                    o.active.clone()
                } else {
                    vec![(o.start, o.end)]
                };
                active
                    .into_iter()
                    .map(|(a, b)| (a, b, f64::from(o.demand.cores)))
                    .collect::<Vec<_>>()
            })
            .collect();
        let cpu = Occupancy::weighted(&intervals);
        all.clear();
        class_samples.iter_mut().for_each(Vec::clear);
        for (o, &id) in observations.iter().zip(&class_ids) {
            if o.local
                && o.net > 0.0
                && o.raw.conclusion.as_deref() == Some("success")
                && let Some(&median) = medians.get(&(o.raw.repo.clone(), o.raw.job_name.clone()))
            {
                let cores = if config.exclude_semaphore_waits_from_contention {
                    o.active
                        .iter()
                        .map(|&(a, b)| cpu.mean(a, b) * (b - a))
                        .sum::<f64>()
                        / o.net
                } else {
                    cpu.mean(o.start, o.end)
                };
                let sample = Sample {
                    concurrency: cores / mean_cores.max(0.001),
                    inflation: o.net / median,
                };
                all.push(sample);
                class_samples[id].push(sample);
            }
        }
        for (class, samples) in classes.iter_mut().zip(&class_samples) {
            class.fit = ContentionFit::fit(
                samples,
                config.low_concurrency,
                mean_cores.max(0.001),
                reference,
            );
        }
    }
    let mut pooled = ContentionFit::fit(
        &all,
        config.low_concurrency,
        mean_cores.max(0.001),
        reference,
    );
    let mut failures = vec![[0_usize; 2]; classes.len()];
    let mut totals = [0_usize; 2];
    for (o, &id) in observations.iter().zip(&class_ids) {
        if o.local
            && o.net > 0.0
            && o.demand.heavy
            && matches!(o.raw.conclusion.as_deref(), Some("success" | "failure"))
        {
            let bin = usize::from(o.concurrency >= config.high_concurrency);
            classes[id].fit.failure_samples[bin] += 1;
            pooled.failure_samples[bin] += 1;
            let failed = usize::from(o.raw.conclusion.as_deref() == Some("failure"));
            failures[id][bin] += failed;
            totals[bin] += failed;
        }
    }
    pooled.low_failure_rate = totals[0] as f64 / pooled.failure_samples[0].max(1) as f64;
    pooled.high_failure_rate = totals[1] as f64 / pooled.failure_samples[1].max(1) as f64;
    for (c, failed) in classes.iter_mut().zip(failures) {
        c.fit.cores_per_job = mean_cores.max(0.001);
        c.fit.low_failure_rate = failed[0] as f64 / c.fit.failure_samples[0].max(1) as f64;
        c.fit.high_failure_rate = failed[1] as f64 / c.fit.failure_samples[1].max(1) as f64;
    }
    Model {
        pooled,
        classes,
        class_ids,
        medians,
    }
}
