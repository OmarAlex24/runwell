use crate::{
    Config,
    contention::{ContentionFit, Occupancy, Sample, quantile},
    host_fit::{HostFactor, HostFit},
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
    /// Per-host intrinsic duration multiples; empty without runner attribution.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub host_factors: Vec<HostFactor>,
}
pub(crate) struct Model {
    pub pooled: ContentionFit,
    pub classes: Vec<ClassModel>,
    pub class_ids: Vec<usize>,
    /// Repository and job id (or name) of each class, for configured overrides.
    pub class_keys: Vec<(String, String)>,
    /// Low-load duration per observation; on the class's fastest host when fitted per host.
    pub baselines: Vec<Option<f64>>,
    /// Intrinsic duration multiple by class and host; empty without runner attribution.
    pub factors: Vec<Vec<f64>>,
    pub host_fit: Option<HostFit>,
}

/// Time-weighted load seen by one observation on its host.
fn mean_load(o: &Observation<'_>, occupancy: &Occupancy, config: &Config) -> f64 {
    if config.exclude_semaphore_waits_from_contention && o.net > 0.0 {
        o.active
            .iter()
            .map(|&(a, b)| occupancy.mean(a, b) * (b - a))
            .sum::<f64>()
            / o.net
    } else {
        occupancy.mean(o.start, o.end)
    }
}

/// Occupancy of each fitted host, counting each running job with `weight`.
fn host_occupancy(
    observations: &[Observation<'_>],
    config: &Config,
    hosts: usize,
    weight: impl Fn(&Observation<'_>) -> f64,
) -> Vec<Occupancy> {
    (0..hosts)
        .map(|h| {
            let intervals: Vec<_> = observations
                .iter()
                .filter(|o| o.local && o.net > 0.0 && o.host == Some(h))
                .flat_map(|o| {
                    let active = if config.exclude_semaphore_waits_from_contention {
                        o.active.clone()
                    } else {
                        vec![(o.start, o.end)]
                    };
                    let w = weight(o);
                    active.into_iter().map(move |(a, b)| (a, b, w))
                })
                .collect();
            Occupancy::weighted(&intervals)
        })
        .collect()
}

pub(crate) fn fit(observations: &mut [Observation<'_>], config: &Config) -> Model {
    let per_host = config.attributes_hosts();
    let hosts = if per_host { config.hosts.len() } else { 1 };
    let reference = f64::from(config.hosts[0].cores);
    // N jobs on host h load it like N * scale[h] jobs on the reference host.
    let scale: Vec<f64> = (0..hosts)
        .map(|h| {
            if per_host {
                reference / f64::from(config.hosts[h].cores)
            } else {
                1.0
            }
        })
        .collect();
    let occupancy = host_occupancy(observations, config, hosts, |_| 1.0);
    for o in observations.iter_mut() {
        o.concurrency = match o.host.filter(|&h| h < hosts) {
            Some(h) => mean_load(o, &occupancy[h], config) * scale[h],
            None => 0.0,
        };
    }
    let class_key = |o: &Observation<'_>| {
        (
            o.raw.repo.clone(),
            o.raw
                .workflow_job_id
                .clone()
                .unwrap_or_else(|| o.raw.job_name.clone()),
        )
    };
    let keys: std::collections::BTreeSet<_> = observations.iter().map(class_key).collect();
    let class_index: BTreeMap<_, _> = keys.into_iter().enumerate().map(|(i, k)| (k, i)).collect();
    let class_ids: Vec<usize> = observations
        .iter()
        .map(|o| class_index[&class_key(o)])
        .collect();
    let (baselines, factors, mut details, host_fit) = if per_host {
        let fitted = crate::host_fit::fit(
            observations,
            &class_ids,
            class_index.len(),
            hosts,
            config
                .factor_low_concurrency
                .unwrap_or(config.low_concurrency),
        );
        (
            fitted.baselines,
            fitted.factors,
            fitted.details,
            Some(fitted.summary),
        )
    } else {
        (
            legacy_medians(observations, config),
            Vec::new(),
            Vec::new(),
            None,
        )
    };
    // Expected low-load duration on the observation's own host.
    let expected = |i: usize, o: &Observation<'_>| {
        let base = baselines[i]?;
        Some(match (factors.get(class_ids[i]), o.host) {
            (Some(row), Some(h)) => base * row[h],
            _ => base,
        })
    };
    let fitted = |o: &Observation<'_>| {
        o.local && o.net > 0.0 && o.host.is_some() && o.raw.conclusion.as_deref() == Some("success")
    };
    let mut class_samples = vec![Vec::new(); class_index.len()];
    let mut all = Vec::new();
    for (i, o) in observations.iter().enumerate() {
        if fitted(o)
            && let Some(base) = expected(i, o)
        {
            let sample = Sample {
                concurrency: o.concurrency,
                inflation: o.net / base,
            };
            class_samples[class_ids[i]].push(sample);
            all.push(sample);
        }
    }
    let peak = occupancy
        .iter()
        .zip(&scale)
        .map(|(o, s)| o.peak() * s)
        .fold(0.0, f64::max);
    let anchor = reference / peak.max(1.0);
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
                host_factors: details.get_mut(i).map(std::mem::take).unwrap_or_default(),
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
        // Fit on the same per-host proxy-demand axis used during replay. Raw
        // job counts alone would apply class sensitivity twice when the job mix
        // changes. The reservations above are inferred once, never target-tuned.
        let cpu = host_occupancy(observations, config, hosts, |o| f64::from(o.demand.cores));
        all.clear();
        class_samples.iter_mut().for_each(Vec::clear);
        for (i, o) in observations.iter().enumerate() {
            if fitted(o)
                && let Some(h) = o.host
                && let Some(base) = expected(i, o)
            {
                let cores = mean_load(o, &cpu[h], config) * scale[h];
                let sample = Sample {
                    concurrency: cores / mean_cores.max(0.001),
                    inflation: o.net / base,
                };
                all.push(sample);
                class_samples[class_ids[i]].push(sample);
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
            && o.host.is_some()
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
        class_keys: class_index.into_keys().collect(),
        baselines,
        factors,
        host_fit,
    }
}

/// Aggregate model: median low-load duration of each repository/job name.
fn legacy_medians(observations: &[Observation<'_>], config: &Config) -> Vec<Option<f64>> {
    let mut low: BTreeMap<(&str, &str), Vec<f64>> = BTreeMap::new();
    for o in observations.iter().filter(|o| {
        o.local
            && o.net > 0.0
            && o.concurrency <= config.low_concurrency
            && o.raw.conclusion.as_deref() == Some("success")
    }) {
        low.entry((&o.raw.repo, &o.raw.job_name))
            .or_default()
            .push(o.net);
    }
    let medians: BTreeMap<_, _> = low
        .into_iter()
        .map(|(k, v)| (k, quantile(&v, 0.5)))
        .collect();
    observations
        .iter()
        .map(|o| medians.get(&(&*o.raw.repo, &*o.raw.job_name)).copied())
        .collect()
}
