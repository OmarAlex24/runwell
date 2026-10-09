//! Bounded exhaustive allocation search for the future setup recommender.
use crate::{Error, Policy, PreparedTrace, Report, Row, contention::quantile, engine};
use serde::Serialize;

/// Search bounds and objective, independent of CLI or filesystem access.
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// Total persistent runner limit per host. Empty uses physical core counts.
    pub max_runners_per_host: Vec<usize>,
    /// p90 target minutes in report repository order. Empty uses observed p90 / 2.
    pub target_p90_minutes: Vec<f64>,
    /// Optional p50 targets in the same repository order; empty ranks p90 only.
    pub target_p50_minutes: Vec<f64>,
    /// Number retained for each semaphore mode.
    pub top_per_mode: usize,
    /// Heavy slot count for the enabled mode; disabled mode is always searched too.
    pub heavy_slots: usize,
    /// Worker count; deterministic ranking is independent of this value.
    pub workers: usize,
}
impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            max_runners_per_host: Vec::new(),
            target_p90_minutes: Vec::new(),
            target_p50_minutes: Vec::new(),
            top_per_mode: 5,
            heavy_slots: 3,
            workers: 4,
        }
    }
}
/// One evaluated persistent-runner allocation.
#[derive(Debug, Clone, Serialize)]
pub struct Allocation {
    /// Host-major matrix of runner counts in report repository order.
    pub runners: Vec<Vec<usize>>,
    /// Host-local heavy slots; absent disables the semaphore.
    pub heavy_slots: Option<usize>,
    /// Per-host overrides for the enabled semaphore mode.
    pub heavy_slots_per_host: Vec<usize>,
    /// Maximum latency/target ratio over repositories and requested percentiles.
    pub score: f64,
    /// Full repository/event metrics for this allocation.
    pub rows: Vec<Row>,
}
/// Search coverage and top allocations, with deterministic tie breaking.
#[derive(Debug, Clone, Serialize)]
pub struct SearchReport {
    /// Number of allocation/semaphore combinations evaluated.
    pub evaluated: usize,
    /// Infeasible combinations or those censoring a selected successful run.
    pub rejected: usize,
    /// Explicit host bounds defining the finite search domain.
    pub max_runners_per_host: Vec<usize>,
    /// Objective denominators, in report repository order.
    pub target_p90_minutes: Vec<f64>,
    /// Optional median targets included in the ranking objective.
    pub target_p50_minutes: Vec<f64>,
    /// Retained count per semaphore mode.
    pub top_per_mode: usize,
    /// Disabled-sem top results, then enabled-sem top results.
    pub allocations: Vec<Allocation>,
}
fn rank(a: &Allocation, b: &Allocation) -> std::cmp::Ordering {
    a.score
        .total_cmp(&b.score)
        .then(
            a.runners
                .iter()
                .flatten()
                .sum::<usize>()
                .cmp(&b.runners.iter().flatten().sum::<usize>()),
        )
        .then(a.runners.cmp(&b.runners))
}

/// Search every integer allocation inside the bounds, with and without a heavy
/// semaphore. Background jobs, cancellations and contention remain identical.
/// This initial API requires one runner pool per repository; ambiguous label
/// partitions must be resolved explicitly before offering setup advice.
pub fn search_allocations(
    trace: &PreparedTrace,
    options: &SearchOptions,
) -> Result<SearchReport, Error> {
    let repos = trace.repos.len();
    if trace.pool_repos.len() != repos
        || (0..repos).any(|r| trace.pool_repos.iter().filter(|&&p| p == r).count() != 1)
    {
        return Err(Error::Invalid(
            "allocation search requires one runner pool per repository".into(),
        ));
    }
    let bounds = if options.max_runners_per_host.is_empty() {
        trace
            .config
            .hosts
            .iter()
            .map(|h| h.cores as usize)
            .collect()
    } else {
        options.max_runners_per_host.clone()
    };
    if bounds.is_empty()
        || bounds.len() > trace.config.hosts.len()
        || bounds.iter().any(|&n| n == 0 || n > 64)
        || options.top_per_mode == 0
        || options.workers == 0
        || options.heavy_slots == 0
    {
        return Err(Error::Invalid("invalid allocation search bounds".into()));
    }
    let targets = if options.target_p90_minutes.is_empty() {
        (0..repos)
            .map(|repo| {
                let values: Vec<_> = trace
                    .runs
                    .iter()
                    .filter(|r| r.report && r.repo == repo && r.event == "pull_request")
                    .map(|r| r.observed / 60.0)
                    .collect();
                quantile(&values, 0.9) / 2.0
            })
            .collect()
    } else {
        options.target_p90_minutes.clone()
    };
    if targets.len() != repos || targets.iter().any(|t| !t.is_finite() || *t <= 0.0) {
        return Err(Error::Invalid(
            "positive PR p90 targets are required for every repository".into(),
        ));
    }
    if !options.target_p50_minutes.is_empty()
        && (options.target_p50_minutes.len() != repos
            || options
                .target_p50_minutes
                .iter()
                .any(|t| !t.is_finite() || *t <= 0.0))
    {
        return Err(Error::Invalid(
            "positive PR p50 targets are required for every repository".into(),
        ));
    }
    let candidates = allocations(&bounds, repos)?;
    let workers = options.workers.min(candidates.len()).max(1);
    let partials = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for worker in 0..workers {
            let candidates = &candidates;
            let targets = &targets;
            handles.push(scope.spawn(move || {
                let mut best = [Vec::new(), Vec::new()];
                let mut rejected = 0;
                for runners in candidates.iter().skip(worker).step_by(workers) {
                    let pools: Vec<Vec<_>> = runners
                        .iter()
                        .map(|row| trace.pool_repos.iter().map(|&r| row[r]).collect())
                        .collect();
                    for (mode, slots) in [None, Some(options.heavy_slots)].into_iter().enumerate() {
                        let mut config = trace.config.clone();
                        if slots.is_none() {
                            // The disabled mode removes every pool, not only heavy.
                            config.disable_gates();
                        }
                        config.heavy_slots = slots;
                        config.semaphore_history = false;
                        let Ok(outcome) = engine::replay_allocation(
                            trace,
                            &config,
                            Policy::Baseline,
                            runners.len(),
                            Some(&pools),
                        ) else {
                            rejected += 1;
                            continue;
                        };
                        let mut report = Report::new(trace, &config);
                        report.add(trace, Policy::Baseline, runners.len(), &outcome, None);
                        let pr: Vec<_> = report
                            .rows
                            .iter()
                            .filter(|r| r.event == "pull_request")
                            .collect();
                        if pr.len() != repos
                            || pr
                                .iter()
                                .any(|r| r.metrics.runs == 0 || r.metrics.cancelled_runs > 0)
                        {
                            rejected += 1;
                            continue;
                        }
                        let score =
                            pr.iter()
                                .enumerate()
                                .map(|(r, row)| {
                                    let p90 = row.metrics.p90_minutes / targets[r];
                                    options.target_p50_minutes.get(r).map_or(p90, |target| {
                                        p90.max(row.metrics.p50_minutes / target)
                                    })
                                })
                                .fold(0.0, f64::max);
                        best[mode].push(Allocation {
                            runners: runners.clone(),
                            heavy_slots: slots,
                            heavy_slots_per_host: config.heavy_slots_per_host.clone(),
                            score,
                            rows: report.rows,
                        });
                        best[mode].sort_by(rank);
                        best[mode].truncate(options.top_per_mode);
                    }
                }
                (best, rejected)
            }));
        }
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .map_err(|_| Error::Invalid("allocation search worker failed".into()))
            })
            .collect::<Result<Vec<_>, _>>()
    })?;
    let mut best = [Vec::new(), Vec::new()];
    let mut rejected = 0;
    for (results, count) in partials {
        rejected += count;
        for (mode, values) in results.into_iter().enumerate() {
            best[mode].extend(values);
        }
    }
    let mut winners = Vec::new();
    for values in &mut best {
        values.sort_by(rank);
        values.truncate(options.top_per_mode);
        winners.append(values);
    }
    Ok(SearchReport {
        evaluated: candidates.len() * 2,
        rejected,
        max_runners_per_host: bounds,
        target_p90_minutes: targets,
        target_p50_minutes: options.target_p50_minutes.clone(),
        top_per_mode: options.top_per_mode,
        allocations: winners,
    })
}
fn allocations(bounds: &[usize], repos: usize) -> Result<Vec<Vec<Vec<usize>>>, Error> {
    let mut grid = vec![Vec::new()];
    for &bound in bounds {
        let mut choices = Vec::new();
        partitions(bound, repos, &mut Vec::new(), &mut choices)?;
        let mut next = Vec::new();
        for previous in &grid {
            for row in &choices {
                let mut candidate = previous.clone();
                candidate.push(row.clone());
                next.push(candidate);
                if next.len() > 100_000 {
                    return Err(Error::Invalid(
                        "allocation grid exceeds 100,000 candidates; tighten bounds".into(),
                    ));
                }
            }
        }
        grid = next;
    }
    grid.retain(|rows| (0..repos).all(|r| rows.iter().any(|row| row[r] > 0)));
    Ok(grid)
}
fn partitions(
    remaining: usize,
    repos: usize,
    prefix: &mut Vec<usize>,
    out: &mut Vec<Vec<usize>>,
) -> Result<(), Error> {
    if repos == 0 {
        if out.len() >= 100_000 {
            return Err(Error::Invalid(
                "allocation grid exceeds 100,000 candidates; tighten bounds".into(),
            ));
        }
        out.push(prefix.clone());
        return Ok(());
    }
    for n in 0..=remaining {
        prefix.push(n);
        partitions(remaining - n, repos - 1, prefix, out)?;
        prefix.pop();
    }
    Ok(())
}
