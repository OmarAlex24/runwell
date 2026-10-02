//! TOML configuration. All durations are seconds and memory sizes are GiB.
use crate::Error;
use runwell_admission::{ReservationAdmission, Resources};
use serde::{Deserialize, Serialize};

/// Replay assumptions and one or more host scenarios.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// All hosts; scenarios use successive prefixes of this list.
    pub hosts: Vec<Host>,
    /// Default demand when a job name has no override.
    pub default_demand: Demand,
    /// Exact job-name overrides, optionally scoped by repository.
    pub jobs: Vec<JobDemand>,
    /// Fixed baseline runner pools, optionally selected by labels.
    pub pools: Vec<Pool>,
    /// Only jobs with this label consume simulated host resources; others stay external.
    pub contention_label: Option<String>,
    /// Step-name fragments identifying semaphore waits to subtract from observed work.
    pub semaphore_steps: Vec<String>,
    /// Baseline semaphore slots per host; absent disables the semaphore.
    pub heavy_slots: Option<usize>,
    /// Optional per-host limits, in host order; the global limit remains the fallback.
    pub heavy_slots_per_host: Vec<usize>,
    /// Baseline semaphore timeout before fail-open.
    pub semaphore_timeout_seconds: f64,
    /// CPU reservation multiplier.
    pub cpu_overcommit: f64,
    /// Memory reservation multiplier.
    pub memory_overcommit: f64,
    /// Strict FIFO promotion threshold for all priority variants.
    pub aging_seconds: f64,
    /// Maximum mean observed concurrency considered uncontended.
    pub low_concurrency: f64,
    /// Concurrency above which heavy failure rates are fitted separately.
    pub high_concurrency: f64,
    /// Memory demand / physical memory above which a penalty applies.
    pub memory_threshold: f64,
    /// Inflation multiplier at twice the memory threshold.
    pub memory_penalty: f64,
    /// Random seed, stable across scenarios.
    pub seed: u64,
    /// Compare successful first-attempt runs, as in the reference PR baseline.
    pub successful_first_attempt: bool,
    /// Replace repository names with stable letters in reports.
    pub anonymize: bool,
    /// Fit slowdown separately for each repository/workflow job class.
    pub fit_by_class: bool,
    /// Derive CPU reservations from relative class contention sensitivity.
    pub derive_cpu_demand: bool,
    /// Refit slowdown on aggregate proxy CPU load when deriving reservations.
    /// Disabling this retains the raw job-count axis for diagnostic ablations.
    pub fit_proxy_load: bool,
    /// Apply imported workflow cancellation groups.
    pub cancel_in_progress: bool,
    /// Use first job creation as workflow-concurrency admission time. Run creation
    /// still starts end-to-end latency; GitHub can dispatch runs out of that order.
    pub cancel_on_dispatch: bool,
    /// Delay between superseding arrival and forced termination.
    pub cancel_grace_seconds: f64,
    /// Diagnostic only: retain measured net work and disable modeled slowdown.
    pub observed_work: bool,
    /// Preserve per-execution variation by integrating fitted speed over observed load.
    /// False retains the legacy low-load median work estimate for diagnostics.
    pub preserve_work_variation: bool,
    /// Baseline applies the semaphore only where recorded steps show it existed.
    pub semaphore_history: bool,
    /// Exclude recorded semaphore wait intervals from CPU contention fitting.
    pub exclude_semaphore_waits_from_contention: bool,
    /// Limit reported cohorts to these workflows; background replay is unchanged.
    pub report_workflows: Vec<WorkflowScope>,
    /// CPU and memory multipliers evaluated for every resource-admission variant.
    pub overcommit_sweep: Vec<f64>,
    /// Optional measured runner availability changes for baseline reconstruction.
    pub runner_history: Vec<RunnerCapacity>,
    /// Optional runner/host outages and installed pool sizes.
    pub availability: Vec<crate::availability::Record>,
    /// Sensitivity only: resource policies additionally require a legacy runner slot.
    pub runwell_runner_availability: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hosts: vec![Host {
                class: "big".into(),
                cores: 12,
                memory_gib: 31.0,
            }],
            default_demand: Demand::default(),
            jobs: Vec::new(),
            pools: Vec::new(),
            contention_label: None,
            semaphore_steps: Vec::new(),
            heavy_slots: None,
            heavy_slots_per_host: Vec::new(),
            semaphore_timeout_seconds: 900.0,
            cpu_overcommit: 1.0,
            memory_overcommit: 1.0,
            aging_seconds: 300.0,
            low_concurrency: 3.0,
            high_concurrency: 7.0,
            memory_threshold: 1.0,
            memory_penalty: 1.5,
            seed: 1,
            successful_first_attempt: true,
            anonymize: true,
            fit_by_class: true,
            derive_cpu_demand: false,
            fit_proxy_load: true,
            cancel_in_progress: true,
            cancel_on_dispatch: true,
            cancel_grace_seconds: 0.0,
            observed_work: false,
            preserve_work_variation: true,
            semaphore_history: true,
            exclude_semaphore_waits_from_contention: true,
            report_workflows: Vec::new(),
            overcommit_sweep: Vec::new(),
            runner_history: Vec::new(),
            availability: Vec::new(),
            runwell_runner_availability: false,
        }
    }
}

/// Physical host capacity and placement class.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    /// Class used for placement pins, not a machine identity.
    pub class: String,
    /// Physical CPU cores.
    pub cores: u32,
    /// Physical RAM in GiB.
    pub memory_gib: f64,
}
impl Host {
    /// Physical resources.
    pub fn resources(&self) -> Resources {
        Resources {
            cpu_slots: self.cores,
            memory_bytes: gib(self.memory_gib),
        }
    }
}

/// Resource reservation and job class.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Demand {
    /// CPU cores required.
    pub cores: u32,
    /// RAM in GiB required.
    pub memory_gib: f64,
    /// Participate in the baseline semaphore and heavy failure fit.
    pub heavy: bool,
    /// Restrict placement to this host class, when set.
    pub host_class: Option<String>,
}
impl Default for Demand {
    fn default() -> Self {
        Self {
            cores: 1,
            memory_gib: 1.0,
            heavy: false,
            host_class: None,
        }
    }
}
impl Demand {
    /// Reservation used unchanged by the shared scheduling policy.
    pub fn resources(&self) -> Resources {
        Resources {
            cpu_slots: self.cores,
            memory_bytes: gib(self.memory_gib),
        }
    }
}

/// Exact job-name resource override.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobDemand {
    /// Job name, matched exactly.
    pub name: String,
    /// Optional repository scope.
    pub repo: Option<String>,
    /// Resource estimate.
    pub demand: Demand,
}

/// Fixed baseline runner pool replicated on each configured host.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pool {
    /// Repository, matched exactly.
    pub repo: String,
    /// All of these labels must be present.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Optional exact job-name routing; empty matches every name.
    #[serde(default)]
    pub job_names: Vec<String>,
    /// Slots per host. A missing pool uses six runners per repository.
    pub runners: usize,
}

pub(crate) fn gib(value: f64) -> u64 {
    (value * 1_073_741_824.0).floor() as u64
}

impl Config {
    /// Validate physical capacities and model parameters before replay.
    pub fn validate(&self) -> Result<(), Error> {
        self.admission()?;
        let positive = [
            self.aging_seconds,
            self.low_concurrency,
            self.high_concurrency,
            self.memory_threshold,
            self.semaphore_timeout_seconds,
        ];
        if self
            .overcommit_sweep
            .iter()
            .any(|f| !f.is_finite() || *f <= 0.0)
            || !self.cancel_grace_seconds.is_finite()
            || self.cancel_grace_seconds < 0.0
            || self.hosts.is_empty()
            || positive.iter().any(|x| !x.is_finite() || *x <= 0.0)
            || !self.memory_penalty.is_finite()
            || self.memory_penalty < 1.0
            || self.high_concurrency <= self.low_concurrency
            || self.heavy_slots == Some(0)
            || self.heavy_slots_per_host.len() > self.hosts.len()
            || self.heavy_slots_per_host.contains(&0)
            || self.pools.iter().any(|p| p.runners == 0)
        {
            return Err(Error::Invalid(
                "invalid hosts, thresholds, pools or semaphore".into(),
            ));
        }
        for (cores, ram) in self
            .hosts
            .iter()
            .map(|h| (h.cores, h.memory_gib))
            .chain(std::iter::once((
                self.default_demand.cores,
                self.default_demand.memory_gib,
            )))
            .chain(
                self.jobs
                    .iter()
                    .map(|j| (j.demand.cores, j.demand.memory_gib)),
            )
        {
            if cores == 0
                || !ram.is_finite()
                || ram <= 0.0
                || ram >= (u64::MAX as f64 / 1_073_741_824.0)
            {
                return Err(Error::Invalid(
                    "CPU and memory must be positive and representable".into(),
                ));
            }
        }
        Ok(())
    }
    pub(crate) fn admission(&self) -> Result<ReservationAdmission, Error> {
        Ok(ReservationAdmission::new(
            self.cpu_overcommit,
            self.memory_overcommit,
        )?)
    }
    pub(crate) fn demand(&self, repo: &str, name: &str) -> Demand {
        self.jobs
            .iter()
            .filter(|j| j.name == name && j.repo.as_deref().is_none_or(|r| r == repo))
            .max_by_key(|j| j.repo.is_some())
            .map(|j| j.demand.clone())
            .unwrap_or_else(|| self.default_demand.clone())
    }
    pub(crate) fn heavy_slots_on(&self, host: usize) -> Option<usize> {
        self.heavy_slots_per_host
            .get(host)
            .copied()
            .or(self.heavy_slots)
    }
}

/// A workflow selected for reporting and calibration, not for load filtering.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowScope {
    /// Exact repository identity.
    pub repo: String,
    /// Exact workflow display name.
    pub name: String,
}

/// Persistent runner capacity change; counterfactual allocation search ignores it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerCapacity {
    /// Repository whose pool changes.
    pub repo: String,
    /// Host index in the configured list.
    pub host: usize,
    /// Effective UTC time.
    pub at: jiff::Timestamp,
    /// Available runners from this time onward.
    pub runners: usize,
}
