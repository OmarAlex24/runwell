//! Per-host intrinsic speed factors, anchored at low load so that a busy host is
//! not mistaken for a slow one. The shared contention curve is fitted afterwards
//! on what these factors leave unexplained.
use crate::{contention::quantile, observation::Observation};
use serde::Serialize;
use std::collections::BTreeMap;

/// Median polish converges within a few rounds; a fixed count keeps it deterministic.
const POLISH_ROUNDS: usize = 20;
/// Normal-consistent scale of the median absolute deviation.
const MAD_SCALE: f64 = 1.4826;

/// One class's duration multiple on one host, relative to the class's fastest host.
#[derive(Debug, Clone, Serialize)]
pub struct HostFactor {
    /// Host index in the configured list.
    pub host: usize,
    /// Multiple used in replay: a configured override, else `fitted`.
    pub factor: f64,
    /// Estimate after shrinkage toward no host difference.
    pub fitted: f64,
    /// Unshrunk low-load ratio to the fastest host with samples; absent without samples.
    pub raw: Option<f64>,
    /// Successful low-load samples of this class on this host.
    pub samples: usize,
    /// Share of the raw log ratio kept by shrinkage: one keeps it, zero removes it.
    pub weight: f64,
}

/// Evidence behind the per-host factors.
#[derive(Debug, Clone, Serialize)]
pub struct HostFit {
    /// Equivalent concurrency at or below which a sample anchors the factors.
    pub anchor_concurrency: f64,
    /// Anchor samples across all classes and hosts.
    pub anchor_samples: usize,
    /// Robust scale of log durations around the anchor fit; absent without replicates.
    pub log_sigma: Option<f64>,
    /// Method-of-moments variance of true log ratios between hosts, across classes.
    pub log_tau_squared: f64,
}

pub(crate) struct Fitted {
    /// Low-load duration on the class's fastest host, per observation.
    pub baselines: Vec<Option<f64>>,
    /// Duration multiple by class and host.
    pub factors: Vec<Vec<f64>>,
    pub details: Vec<Vec<HostFactor>>,
    pub summary: HostFit,
}

/// Fit `ln(duration) = name baseline + class-host effect` on successful anchor
/// samples by median polish. Each class's effects are taken relative to its
/// best-supported host and shrunk toward zero by an empirical-Bayes weight
/// `tau² / (tau² + se²)`, where `se² = (pi/2) sigma² (1/n_host + 1/n_reference)`
/// is the variance of a difference of medians and `tau²` is estimated from all
/// classes. A host without samples keeps the reference host's effect. Factors are
/// finally expressed relative to each class's fastest host.
pub(crate) fn fit(
    obs: &[Observation<'_>],
    class_ids: &[usize],
    classes: usize,
    hosts: usize,
    anchor: f64,
) -> Fitted {
    let mut names = BTreeMap::new();
    let name_of: Vec<usize> = obs
        .iter()
        .zip(class_ids)
        .map(|(o, &c)| {
            let next = names.len();
            *names.entry((c, o.raw.job_name.as_str())).or_insert(next)
        })
        .collect();
    let samples: Vec<(usize, usize)> = obs
        .iter()
        .enumerate()
        .filter(|(_, o)| {
            o.local
                && o.net > 0.0
                && o.concurrency <= anchor
                && o.raw.conclusion.as_deref() == Some("success")
        })
        .filter_map(|(i, o)| Some((i, o.host.filter(|&h| h < hosts)?)))
        .collect();
    let y = |i: usize| obs[i].net.ln();
    let mut by_name = vec![Vec::new(); names.len()];
    let mut by_cell = vec![vec![Vec::new(); hosts]; classes];
    for &(i, h) in &samples {
        by_name[name_of[i]].push((i, h));
        by_cell[class_ids[i]][h].push(i);
    }
    let reference: Vec<usize> = by_cell
        .iter()
        .map(|row| {
            (0..hosts)
                .max_by_key(|&h| (row[h].len(), std::cmp::Reverse(h)))
                .unwrap_or(0)
        })
        .collect();
    let mut effect = vec![vec![0.0; hosts]; classes];
    let mut base = vec![0.0; names.len()];
    let polish_names = |base: &mut [f64], effect: &[Vec<f64>]| {
        for (n, members) in by_name.iter().enumerate() {
            let values: Vec<_> = members
                .iter()
                .map(|&(i, h)| y(i) - effect[class_ids[i]][h])
                .collect();
            base[n] = quantile(&values, 0.5);
        }
    };
    for _ in 0..POLISH_ROUNDS {
        polish_names(&mut base, &effect);
        for (c, row) in by_cell.iter().enumerate() {
            for (h, members) in row.iter().enumerate().filter(|(_, m)| !m.is_empty()) {
                let values: Vec<_> = members.iter().map(|&i| y(i) - base[name_of[i]]).collect();
                effect[c][h] = quantile(&values, 0.5);
            }
            let offset = effect[c][reference[c]];
            for (h, e) in effect[c].iter_mut().enumerate() {
                *e = if row[h].is_empty() { 0.0 } else { *e - offset };
            }
        }
    }
    polish_names(&mut base, &effect);
    let residuals: Vec<_> = samples
        .iter()
        .filter(|&&(i, _)| by_name[name_of[i]].len() > 1)
        .map(|&(i, h)| y(i) - base[name_of[i]] - effect[class_ids[i]][h])
        .collect();
    let sigma = (residuals.len() > 1).then(|| {
        let center = quantile(&residuals, 0.5);
        let deviations: Vec<_> = residuals.iter().map(|r| (r - center).abs()).collect();
        MAD_SCALE * quantile(&deviations, 0.5)
    });
    let se2 = |c: usize, h: usize| {
        sigma.map_or(f64::INFINITY, |s| {
            std::f64::consts::FRAC_PI_2
                * s
                * s
                * (1.0 / by_cell[c][h].len() as f64 + 1.0 / by_cell[c][reference[c]].len() as f64)
        })
    };
    let pairs: Vec<(usize, usize)> = (0..classes)
        .flat_map(|c| (0..hosts).map(move |h| (c, h)))
        .filter(|&(c, h)| h != reference[c] && !by_cell[c][h].is_empty())
        .collect();
    let tau2 = if pairs.is_empty() || sigma.is_none() {
        0.0
    } else {
        let n = pairs.len() as f64;
        (pairs
            .iter()
            .map(|&(c, h)| effect[c][h].powi(2))
            .sum::<f64>()
            / n
            - pairs.iter().map(|&(c, h)| se2(c, h)).sum::<f64>() / n)
            .max(0.0)
    };
    let mut weights = vec![vec![0.0; hosts]; classes];
    let mut shrunk = vec![vec![0.0; hosts]; classes];
    for &(c, h) in &pairs {
        weights[c][h] = if tau2 > 0.0 {
            tau2 / (tau2 + se2(c, h))
        } else {
            0.0
        };
        shrunk[c][h] = weights[c][h] * effect[c][h];
    }
    for (c, r) in reference.iter().enumerate() {
        weights[c][*r] = f64::from(u8::from(!by_cell[c][*r].is_empty()));
    }
    polish_names(&mut base, &shrunk);
    let fastest: Vec<f64> = shrunk
        .iter()
        .map(|row| row.iter().copied().fold(f64::INFINITY, f64::min))
        .collect();
    let mut details = Vec::with_capacity(classes);
    let mut factors = Vec::with_capacity(classes);
    for c in 0..classes {
        let observed_fastest = (0..hosts)
            .filter(|&h| !by_cell[c][h].is_empty())
            .map(|h| effect[c][h])
            .fold(f64::INFINITY, f64::min);
        let row: Vec<f64> = shrunk[c].iter().map(|e| (e - fastest[c]).exp()).collect();
        details.push(
            (0..hosts)
                .map(|h| HostFactor {
                    host: h,
                    factor: row[h],
                    fitted: row[h],
                    raw: (!by_cell[c][h].is_empty())
                        .then(|| (effect[c][h] - observed_fastest).exp()),
                    samples: by_cell[c][h].len(),
                    weight: weights[c][h],
                })
                .collect(),
        );
        factors.push(row);
    }
    let baselines = obs
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let n = name_of[i];
            (!by_name[n].is_empty()).then(|| (base[n] + fastest[class_ids[i]]).exp())
        })
        .collect();
    Fitted {
        baselines,
        factors,
        details,
        summary: HostFit {
            anchor_concurrency: anchor,
            anchor_samples: samples.len(),
            log_sigma: sigma,
            log_tau_squared: tau2,
        },
    }
}
