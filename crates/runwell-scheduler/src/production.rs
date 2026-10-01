use crate::{
    Criticality, FairState, HistoryKey, HistorySnapshot, NodeHeadroom, PendingJob, Placement,
    SchedulingPolicy, fairness, headroom,
};
use runwell_admission::Resources;
use std::collections::{BTreeMap, BTreeSet};

/// Production controls. Fairness weights count dispatches, not CPU seconds.
#[derive(Debug, Clone)]
pub struct ProductionConfig {
    /// FIFO protection starts at this age; capacity must eventually become free.
    pub aging_seconds: f64,
    /// Below this remaining capacity fraction, spread a run across nodes.
    pub tight_headroom: f64,
    /// Positive per-repository weights; unspecified repositories receive one.
    pub repository_weights: BTreeMap<String, u32>,
    /// Positive PR weights keyed by (repository, PR identity); default one.
    pub pr_weights: BTreeMap<(String, String), u32>,
    /// Simulator sensitivity: also require a free legacy runner for the pool.
    pub require_runner: bool,
}
impl Default for ProductionConfig {
    fn default() -> Self {
        Self {
            aging_seconds: 300.0,
            tight_headroom: 0.25,
            repository_weights: BTreeMap::new(),
            pr_weights: BTreeMap::new(),
            require_runner: false,
        }
    }
}

/// Metadata kept separate so the original simulator policy API stays compatible.
#[derive(Debug, Clone)]
pub struct JobIdentity {
    /// History and repository identity.
    pub key: HistoryKey,
    /// PR number, or a stable branch/run bucket for non-PR work.
    pub pull_request: String,
    /// Repository-scoped run identity including its attempt.
    pub run: String,
    /// Known run DAG shape; `None` uses this workflow job's historical shape.
    pub criticality: Option<Criticality>,
}
/// Admission-reported constraints in addition to resource reservations.
#[derive(Debug, Clone)]
pub struct NodeStatus {
    /// False for paused, stale, offline, or draining nodes.
    pub admission_open: bool,
    /// Additional reservations allowed by admission's job-count limit.
    pub remaining_jobs: u32,
    /// Accepted job classes. Empty means no classes are accepted.
    pub classes: BTreeSet<String>,
    /// Repository/run pairs already reserved on this host.
    pub active_runs: BTreeSet<(String, String)>,
}
/// Complete caller-owned production metadata. Missing job/node metadata fails closed.
#[derive(Debug, Clone, Default)]
pub struct ProductionSnapshot {
    /// Pending request identities.
    pub jobs: BTreeMap<usize, JobIdentity>,
    /// Current admission and run occupancy, keyed by node ID.
    pub nodes: BTreeMap<usize, NodeStatus>,
    /// Learned duration snapshot, with class defaults for cold starts.
    pub history: HistorySnapshot,
}
/// Proposed placement and fairness accounting; commit together after admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// One proposed reservation, subject to an authoritative node recheck.
    pub placement: Placement,
    /// Next accounting state; discard if the reservation is rejected.
    pub fairness: FairState,
}
/// Deterministic production scheduling over immutable inputs; no I/O or clock.
/// `NodeHeadroom.capacity` MUST be admission's already-scaled allocatable limit,
/// after OS headroom. This policy never applies another overcommit multiplier.
pub struct Production<'a> {
    config: &'a ProductionConfig,
    snapshot: &'a ProductionSnapshot,
    fairness: &'a FairState,
}
/// Invalid production settings.
#[derive(Debug, thiserror::Error)]
#[error("invalid production scheduler settings")]
pub struct ProductionError;

impl<'a> Production<'a> {
    /// Validate settings and borrow a decision snapshot.
    pub fn new(
        config: &'a ProductionConfig,
        snapshot: &'a ProductionSnapshot,
        fairness: &'a FairState,
    ) -> Result<Self, ProductionError> {
        if !config.aging_seconds.is_finite()
            || config.aging_seconds < 0.0
            || !config.tight_headroom.is_finite()
            || !(0.0..=1.0).contains(&config.tight_headroom)
            || config
                .repository_weights
                .values()
                .chain(config.pr_weights.values())
                .any(|&w| w == 0)
        {
            return Err(ProductionError);
        }
        Ok(Self {
            config,
            snapshot,
            fairness,
        })
    }
    fn identity(&self, job: &PendingJob) -> Option<&JobIdentity> {
        self.snapshot.jobs.get(&job.request_id)
    }
    fn eligible(&self, job: &PendingJob, node: &NodeHeadroom) -> bool {
        let Some(meta) = self.identity(job) else {
            return false;
        };
        self.snapshot
            .nodes
            .get(&node.node_id)
            .is_some_and(|status| {
                status.admission_open
                    && status.classes.contains(&meta.key.class)
                    && crate::compatible(job, node)
                    && (!self.config.require_runner
                        || node.free_runners.get(job.pool).is_some_and(|&n| n > 0))
            })
    }
    fn fits(&self, job: &PendingJob, node: &NodeHeadroom) -> bool {
        self.eligible(job, node)
            && self
                .snapshot
                .nodes
                .get(&node.node_id)
                .is_some_and(|s| s.remaining_jobs > 0)
            && fits(node.capacity, node.reserved, job.reservation)
    }
    fn collocated(&self, job: &PendingJob, node: &NodeHeadroom) -> bool {
        self.identity(job).is_some_and(|j| {
            self.snapshot
                .nodes
                .get(&node.node_id)
                .is_some_and(|n| n.active_runs.contains(&(j.key.repo.clone(), j.run.clone())))
        }) && headroom(node, job) <= self.config.tight_headroom
    }
    fn node<'b>(
        &self,
        job: &PendingJob,
        nodes: &'b [NodeHeadroom],
        drain: Option<usize>,
    ) -> Option<&'b NodeHeadroom> {
        nodes
            .iter()
            .filter(|n| Some(n.node_id) != drain && self.fits(job, n))
            .max_by(|a, b| {
                self.collocated(job, b)
                    .cmp(&self.collocated(job, a))
                    .then_with(|| headroom(a, job).total_cmp(&headroom(b, job)))
                    .then(b.node_id.cmp(&a.node_id))
            })
    }
    fn aged(&self, job: &PendingJob, now: f64) -> bool {
        now - job.ready_at >= self.config.aging_seconds
    }
    fn ready(&self, job: &PendingJob, now: f64) -> bool {
        job.ready_at.is_finite()
            && job.ready_at <= now
            && self.identity(job).is_some()
            && job.reservation.cpu_slots > 0
            && job.reservation.memory_bytes > 0
    }
    // Protect one host even if the oldest job cannot fit yet. New small jobs may
    // use other nodes; they cannot continuously refill the protected host.
    fn drain(&self, jobs: &[PendingJob], nodes: &[NodeHeadroom], now: f64) -> Option<usize> {
        let oldest = jobs
            .iter()
            .filter(|j| {
                self.ready(j, now)
                    && self.aged(j, now)
                    && nodes.iter().any(|n| {
                        self.eligible(j, n) && fits(n.capacity, Resources::default(), j.reservation)
                    })
            })
            .min_by(|a, b| fifo(a, b))?;
        if self.node(oldest, nodes, None).is_some() {
            return None;
        }
        nodes
            .iter()
            .filter(|n| {
                self.eligible(oldest, n)
                    && fits(n.capacity, Resources::default(), oldest.reservation)
            })
            .max_by(|a, b| {
                headroom(a, oldest)
                    .total_cmp(&headroom(b, oldest))
                    .then(b.node_id.cmp(&a.node_id))
            })
            .map(|n| n.node_id)
    }
    fn priority(&self, job: &PendingJob) -> (Criticality, f64, f64) {
        let estimate = self
            .identity(job)
            .and_then(|j| self.snapshot.history.estimate(&j.key));
        let criticality = self
            .identity(job)
            .and_then(|j| j.criticality)
            .or_else(|| estimate.map(|e| e.criticality))
            .unwrap_or_default();
        let p50 = estimate.map_or(job.expected_seconds, |e| e.p50_seconds);
        let p90 = estimate.map_or(p50, |e| e.p90_seconds);
        (criticality, valid_duration(p50), valid_duration(p90))
    }
    /// Select a placement and return updated hierarchical round-robin deficits.
    /// Aged jobs precede fairness and criticality, FIFO. With finite work ahead,
    /// bounded execution time and eventual compatible capacity they cannot starve.
    pub fn decide(
        &self,
        jobs: &[PendingJob],
        nodes: &[NodeHeadroom],
        now: f64,
    ) -> Option<Decision> {
        if !now.is_finite() {
            return None;
        }
        let drain = self.drain(jobs, nodes, now);
        let candidates: Vec<_> = jobs
            .iter()
            .filter(|j| self.ready(j, now))
            .filter_map(|j| Some((j, self.identity(j)?, self.node(j, nodes, drain)?)))
            .collect();
        let repos: BTreeMap<_, _> = candidates
            .iter()
            .map(|(_, m, _)| {
                (
                    m.key.repo.clone(),
                    self.config
                        .repository_weights
                        .get(&m.key.repo)
                        .copied()
                        .unwrap_or(1),
                )
            })
            .collect();
        let mut next = self.fairness.clone();
        let fair_repo = fairness::accrue(&mut next.repositories, &repos)?;
        next.pull_requests
            .retain(|repo, _| repos.contains_key(repo));
        let oldest = candidates
            .iter()
            .filter(|(j, _, _)| self.aged(j, now))
            .min_by(|a, b| fifo(a.0, b.0));
        let repo = oldest.map_or(fair_repo.as_str(), |(_, m, _)| &m.key.repo);
        let prs: BTreeMap<_, _> = candidates
            .iter()
            .filter(|(_, m, _)| m.key.repo == repo)
            .map(|(_, m, _)| {
                (
                    m.pull_request.clone(),
                    self.config
                        .pr_weights
                        .get(&(repo.into(), m.pull_request.clone()))
                        .copied()
                        .unwrap_or(1),
                )
            })
            .collect();
        let pr_state = next.pull_requests.entry(repo.into()).or_default();
        let fair_pr = fairness::accrue(pr_state, &prs)?;
        let pr = oldest.map_or(fair_pr.as_str(), |(_, m, _)| &m.pull_request);
        let selected = oldest.or_else(|| {
            candidates
                .iter()
                .filter(|(_, m, _)| m.key.repo == repo && m.pull_request == pr)
                .min_by(|a, b| {
                    let (ca, sa, ta) = self.priority(a.0);
                    let (cb, sb, tb) = self.priority(b.0);
                    cb.cmp(&ca)
                        .then_with(|| sa.total_cmp(&sb))
                        .then_with(|| ta.total_cmp(&tb))
                        .then_with(|| fifo(a.0, b.0))
                })
        })?;
        fairness::charge(pr_state, &prs, pr);
        fairness::charge(&mut next.repositories, &repos, repo);
        Some(Decision {
            placement: Placement {
                request_id: selected.0.request_id,
                node_id: selected.2.node_id,
            },
            fairness: next,
        })
    }
}
impl SchedulingPolicy for Production<'_> {
    fn select(&self, jobs: &[PendingJob], nodes: &[NodeHeadroom], now: f64) -> Option<Placement> {
        self.decide(jobs, nodes, now).map(|d| d.placement)
    }
}
fn fifo(a: &PendingJob, b: &PendingJob) -> std::cmp::Ordering {
    a.ready_at
        .total_cmp(&b.ready_at)
        .then(a.request_id.cmp(&b.request_id))
}
fn valid_duration(seconds: f64) -> f64 {
    if seconds.is_finite() && seconds > 0.0 {
        seconds
    } else {
        f64::MAX
    }
}
fn fits(capacity: Resources, used: Resources, request: Resources) -> bool {
    used.cpu_slots
        .checked_add(request.cpu_slots)
        .is_some_and(|n| n <= capacity.cpu_slots)
        && used
            .memory_bytes
            .checked_add(request.memory_bytes)
            .is_some_and(|n| n <= capacity.memory_bytes)
}
