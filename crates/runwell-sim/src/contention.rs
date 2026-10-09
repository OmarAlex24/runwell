//! Trace-derived monotone piecewise-linear contention and failure models.
use serde::Serialize;
use std::collections::BTreeMap;

/// One successful observed duration relative to its job-name low-load median.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    /// Time-weighted mean host concurrency during this job's work intervals.
    pub concurrency: f64,
    /// Duration / uncontended job-name duration.
    pub inflation: f64,
}

/// Monotone least-squares fit of per-concurrency median inflation.
#[derive(Debug, Clone, Serialize)]
pub struct ContentionFit {
    /// Concurrency / fitted inflation points. Endpoints extrapolate constantly.
    pub points: Vec<(f64, f64)>,
    /// Number of successful observations supporting the fit.
    pub samples: usize,
    /// Mean configured CPU demand per observed running job.
    pub cores_per_job: f64,
    /// Reference host core count used to convert CPU pressure to concurrency.
    pub reference_cores: f64,
    /// Observed low-concurrency heavy job failure fraction (not proven infra failures).
    pub low_failure_rate: f64,
    /// Observed high-concurrency heavy job failure fraction.
    pub high_failure_rate: f64,
    /// Low/high heavy observations; zero indicates no empirical support.
    pub failure_samples: [usize; 2],
}

impl ContentionFit {
    /// Fit medians by integer concurrency, then weighted isotonic regression.
    pub fn fit(samples: &[Sample], low: f64, cores_per_job: f64, reference_cores: f64) -> Self {
        let mut bins: BTreeMap<u32, Vec<f64>> = BTreeMap::new();
        for s in samples.iter().filter(|s| {
            s.concurrency.is_finite()
                && s.inflation.is_finite()
                && s.inflation > 0.0
                && s.concurrency > low
        }) {
            bins.entry(s.concurrency.round() as u32)
                .or_default()
                .push(s.inflation);
        }
        let mut blocks: Vec<(Vec<f64>, f64, usize)> = Vec::new();
        for (n, values) in bins {
            let median = quantile(&values, 0.5).max(1.0);
            blocks.push((
                vec![f64::from(n)],
                median * values.len() as f64,
                values.len(),
            ));
            while blocks.len() >= 2 {
                let end = blocks.len() - 1;
                if blocks[end - 1].1 / blocks[end - 1].2 as f64
                    <= blocks[end].1 / blocks[end].2 as f64
                {
                    break;
                }
                if let Some((xs, sum, count)) = blocks.pop() {
                    let last = &mut blocks[end - 1];
                    last.0.extend(xs);
                    last.1 += sum;
                    last.2 += count;
                }
            }
        }
        let mut points = vec![(0.0, 1.0), (low, 1.0)];
        for (xs, sum, count) in blocks {
            for x in xs {
                if x > low {
                    points.push((x, sum / count as f64));
                }
            }
        }
        Self {
            points,
            samples: samples.len(),
            cores_per_job,
            reference_cores,
            low_failure_rate: 0.0,
            high_failure_rate: 0.0,
            failure_samples: [0; 2],
        }
    }
    /// Interpolate the fitted inflation curve, clamping beyond measured support.
    pub fn inflation(&self, concurrency: f64) -> f64 {
        for pair in self.points.windows(2) {
            let [(x0, y0), (x1, y1)] = [pair[0], pair[1]];
            if concurrency <= x1 {
                return y0 + (y1 - y0) * ((concurrency - x0) / (x1 - x0)).clamp(0.0, 1.0);
            }
        }
        self.points.last().map_or(1.0, |p| p.1)
    }
    /// Effective speed from aggregate CPU pressure, transferred to other host sizes.
    pub fn speed(
        &self,
        demand_cores: f64,
        host_cores: f64,
        memory_ratio: f64,
        memory_threshold: f64,
        memory_penalty: f64,
    ) -> f64 {
        let equivalent =
            demand_cores / host_cores * self.reference_cores / self.cores_per_job.max(0.001);
        let memory =
            1.0 + (memory_ratio / memory_threshold - 1.0).max(0.0) * (memory_penalty - 1.0);
        1.0 / (self.inflation(equivalent) * memory)
    }
}

/// Linearly interpolated quantile (R type 7), with zero for an empty sample.
pub fn quantile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let at = p.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let lo = at.floor() as usize;
    let hi = at.ceil() as usize;
    sorted[lo] + (sorted[hi] - sorted[lo]) * at.fract()
}

/// Integral of observed running-job count, queried in logarithmic time.
pub(crate) struct Occupancy {
    times: Vec<f64>,
    areas: Vec<f64>,
    counts: Vec<f64>,
}
impl Occupancy {
    #[cfg(test)]
    pub fn new(intervals: &[(f64, f64)]) -> Self {
        Self::weighted(
            &intervals
                .iter()
                .map(|&(a, b)| (a, b, 1.0))
                .collect::<Vec<_>>(),
        )
    }
    pub fn weighted(intervals: &[(f64, f64, f64)]) -> Self {
        let mut events: Vec<_> = intervals
            .iter()
            .filter(|(a, b, _)| b > a)
            .flat_map(|&(a, b, weight)| [(a, weight), (b, -weight)])
            .collect();
        events.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (mut times, mut areas, mut counts) = (Vec::new(), Vec::new(), Vec::new());
        let (mut area, mut count, mut previous) = (0.0, 0.0, events.first().map_or(0.0, |e| e.0));
        for (t, delta) in events {
            area += (t - previous) * count;
            count += delta;
            previous = t;
            if times.last() == Some(&t) {
                if let Some(last) = counts.last_mut() {
                    *last = count;
                }
            } else {
                times.push(t);
                areas.push(area);
                counts.push(count);
            }
        }
        Self {
            times,
            areas,
            counts,
        }
    }
    fn integral(&self, t: f64) -> f64 {
        let i = self.times.partition_point(|x| *x <= t);
        if i == 0 {
            0.0
        } else {
            self.areas[i - 1] + (t - self.times[i - 1]) * self.counts[i - 1]
        }
    }
    #[cfg(test)]
    pub fn peak(&self) -> f64 {
        self.peak_between(f64::NEG_INFINITY, f64::INFINITY)
    }
    /// Highest count at any instant of `[a, b]`.
    pub fn peak_between(&self, a: f64, b: f64) -> f64 {
        let first = self.times.partition_point(|x| *x <= a).saturating_sub(1);
        let last = self.times.partition_point(|x| *x <= b);
        self.counts
            .get(first..last)
            .map_or(0.0, |c| c.iter().copied().fold(0.0, f64::max))
    }
    pub fn mean(&self, a: f64, b: f64) -> f64 {
        if b <= a {
            0.0
        } else {
            (self.integral(b) - self.integral(a)) / (b - a)
        }
    }
}
