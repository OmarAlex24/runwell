//! Pure scheduling, placement, dependency priority and semaphore decisions.
#![deny(missing_docs)]

mod concurrency;
mod graph;
pub use concurrency::{
    RunArrival, cancel_deadlines, parallel_slot, requires_heavy_slot, runner_headroom,
};
pub use graph::{GraphError, critical_paths};
use runwell_admission::{ReservationAdmission, Resources};

/// Priority among jobs that can currently be placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// Oldest ready job first.
    Fifo,
    /// Least estimated work first.
    Shortest,
    /// Longest remaining downstream path first.
    CriticalPath,
    /// Least CPU service received by a repository first.
    FairShare,
}

/// Scheduling metadata and mutable accounting supplied by the caller.
#[derive(Debug, Clone)]
pub struct PendingJob {
    /// Stable identity and final deterministic tie breaker.
    pub request_id: usize,
    /// Runner pool index for the baseline.
    pub pool: usize,
    /// Optional host class pin.
    pub host_class: Option<String>,
    /// Time dependencies were satisfied, in seconds on the caller's clock.
    pub ready_at: f64,
    /// Estimated intrinsic work in seconds.
    pub expected_seconds: f64,
    /// Estimated longest remaining downstream path in seconds.
    pub critical_path_seconds: f64,
    /// CPU core-seconds already received by this repository.
    pub fair_service: f64,
    /// Required reservation.
    pub reservation: Resources,
}

/// An immutable host snapshot.
#[derive(Debug, Clone)]
pub struct NodeHeadroom {
    /// Stable host index.
    pub node_id: usize,
    /// Host class used for pinning.
    pub class: String,
    /// Physical capacity.
    pub capacity: Resources,
    /// Resources already reserved.
    pub reserved: Resources,
    /// Free runner slots by pool; baseline only.
    pub free_runners: Vec<usize>,
}

/// Selected job and host; the caller commits the reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Job identity.
    pub request_id: usize,
    /// Host identity.
    pub node_id: usize,
}

/// A deterministic policy with no clock, network, or storage access.
pub trait SchedulingPolicy {
    /// Select one placement from a snapshot. `now` uses the caller's clock.
    fn select(&self, jobs: &[PendingJob], nodes: &[NodeHeadroom], now: f64) -> Option<Placement>;
}

/// Fixed runner pools with FIFO within each pool.
#[derive(Debug, Clone, Copy)]
pub struct Baseline;

impl SchedulingPolicy for Baseline {
    fn select(&self, jobs: &[PendingJob], nodes: &[NodeHeadroom], _now: f64) -> Option<Placement> {
        jobs.iter()
            .filter_map(|job| {
                let node = nodes.iter().find(|n| {
                    compatible(job, n)
                        && n.free_runners.get(job.pool).copied().unwrap_or_default() > 0
                })?;
                Some((job, node))
            })
            .min_by(|(a, _), (b, _)| {
                a.ready_at
                    .total_cmp(&b.ready_at)
                    .then(a.request_id.cmp(&b.request_id))
            })
            .map(|(j, n)| Placement {
                request_id: j.request_id,
                node_id: n.node_id,
            })
    }
}

/// Resource admission plus priority and headroom placement.
#[derive(Debug, Clone, Copy)]
pub struct Runwell {
    /// Job ordering below the aging threshold.
    pub priority: Priority,
    /// Jobs older than this threshold precede all younger jobs, in FIFO order.
    pub aging_seconds: f64,
    /// Shared resource admission check.
    pub admission: ReservationAdmission,
}

fn compatible(job: &PendingJob, node: &NodeHeadroom) -> bool {
    job.host_class
        .as_ref()
        .is_none_or(|class| class == &node.class)
}

impl Runwell {
    fn compare(&self, a: &PendingJob, b: &PendingJob, now: f64) -> std::cmp::Ordering {
        let aged_a = now - a.ready_at >= self.aging_seconds;
        let aged_b = now - b.ready_at >= self.aging_seconds;
        aged_b
            .cmp(&aged_a)
            .then_with(|| {
                if aged_a && aged_b {
                    return a.ready_at.total_cmp(&b.ready_at);
                }
                match self.priority {
                    Priority::Fifo => a.ready_at.total_cmp(&b.ready_at),
                    Priority::Shortest => a.expected_seconds.total_cmp(&b.expected_seconds),
                    Priority::CriticalPath => {
                        b.critical_path_seconds.total_cmp(&a.critical_path_seconds)
                    }
                    Priority::FairShare => a.fair_service.total_cmp(&b.fair_service),
                }
            })
            .then(a.ready_at.total_cmp(&b.ready_at))
            .then(a.request_id.cmp(&b.request_id))
    }
}

impl Runwell {
    // An aged large job must be able to drain one eligible host. Merely sorting
    // fitting jobs by age lets an endless stream of small jobs starve it.
    fn draining_host(
        &self,
        jobs: &[PendingJob],
        nodes: &[NodeHeadroom],
        now: f64,
    ) -> Option<usize> {
        let oldest = jobs
            .iter()
            .filter(|j| now - j.ready_at >= self.aging_seconds)
            .min_by(|a, b| {
                a.ready_at
                    .total_cmp(&b.ready_at)
                    .then(a.request_id.cmp(&b.request_id))
            })?;
        if nodes.iter().any(|n| {
            compatible(oldest, n)
                && self
                    .admission
                    .fits(n.capacity, n.reserved, oldest.reservation)
        }) {
            return None;
        }
        nodes
            .iter()
            .filter(|n| {
                compatible(oldest, n)
                    && self
                        .admission
                        .fits(n.capacity, Resources::default(), oldest.reservation)
            })
            .max_by(|a, b| {
                headroom(a, oldest)
                    .total_cmp(&headroom(b, oldest))
                    .then(b.node_id.cmp(&a.node_id))
            })
            .map(|n| n.node_id)
    }
}

impl SchedulingPolicy for Runwell {
    fn select(&self, jobs: &[PendingJob], nodes: &[NodeHeadroom], now: f64) -> Option<Placement> {
        let draining = self.draining_host(jobs, nodes, now);
        jobs.iter()
            .filter_map(|job| {
                let node = nodes
                    .iter()
                    .filter(|n| {
                        Some(n.node_id) != draining
                            && compatible(job, n)
                            && self.admission.fits(n.capacity, n.reserved, job.reservation)
                    })
                    .max_by(|a, b| {
                        headroom(a, job)
                            .total_cmp(&headroom(b, job))
                            .then(b.node_id.cmp(&a.node_id))
                    })?;
                Some((job, node))
            })
            .min_by(|(a, _), (b, _)| self.compare(a, b, now))
            .map(|(j, n)| Placement {
                request_id: j.request_id,
                node_id: n.node_id,
            })
    }
}

fn headroom(node: &NodeHeadroom, job: &PendingJob) -> f64 {
    let cpu = (f64::from(node.capacity.cpu_slots)
        - f64::from(node.reserved.cpu_slots)
        - f64::from(job.reservation.cpu_slots))
        / f64::from(node.capacity.cpu_slots.max(1));
    let ram = (node.capacity.memory_bytes as f64
        - node.reserved.memory_bytes as f64
        - job.reservation.memory_bytes as f64)
        / node.capacity.memory_bytes.max(1) as f64;
    cpu.min(ram)
}

/// Whether a runner holding a heavy job may start work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemaphoreDecision {
    /// Keep the runner occupied while waiting.
    Wait,
    /// Acquire a slot until the job finishes.
    Acquire,
    /// Work without a slot after the configured timeout.
    FailOpen,
}

/// Pure host-local semaphore rule. Fail-open jobs do not own a semaphore token.
pub fn heavy_slot(
    active: usize,
    limit: usize,
    held_seconds: f64,
    timeout: f64,
) -> SemaphoreDecision {
    if active < limit {
        SemaphoreDecision::Acquire
    } else if held_seconds >= timeout {
        SemaphoreDecision::FailOpen
    } else {
        SemaphoreDecision::Wait
    }
}
