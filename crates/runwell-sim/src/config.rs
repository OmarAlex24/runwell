//! TOML configuration. All durations are seconds and memory sizes are GiB.
use crate::Error;
use runwell_admission::{ReservationAdmission, Resources};
use serde::{Deserialize, Serialize};
mod speed;
mod window;
pub use speed::{RunnerChoice, SpeedFactor};

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
    /// Step-name fragments whose completion releases the semaphore; missing means job end.
    pub semaphore_release_steps: Vec<String>,
    /// Baseline semaphore slots per host; absent disables the semaphore.
    pub heavy_slots: Option<usize>,
    /// Optional per-host limits, in host order; the global limit remains the fallback.
    pub heavy_slots_per_host: Vec<usize>,
    /// Baseline semaphore timeout before fail-open.
    pub semaphore_timeout_seconds: f64,
    /// Retry interval for a polling semaphore; zero means immediate FIFO wakeups.
    pub semaphore_poll_seconds: f64,
    /// Further named semaphore pools. The legacy keys above remain the `heavy` pool.
    pub semaphores: Vec<Semaphore>,
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
    /// Equivalent concurrency at or below which samples anchor per-host speed
    /// factors; defaults to `low_concurrency`.
    pub factor_low_concurrency: Option<f64>,
    /// Replay-only per-host speed factor overrides; reports keep the fitted values.
    pub speed_factors: Vec<SpeedFactor>,
    /// How runner-limited replay picks among hosts with an idle runner.
    pub runner_choice: RunnerChoice,
    /// Only jobs starting in `[fit_since, fit_until)` fit curves, factors, failure
    /// rates and CPU reservations. Every job still loads its host and is replayed.
    pub fit_since: Option<jiff::Timestamp>,
    /// End of the fit window, exclusive.
    pub fit_until: Option<jiff::Timestamp>,
    /// Only runs created in `[report_since, report_until)` form reported cohorts.
    pub report_since: Option<jiff::Timestamp>,
    /// End of the report window, exclusive.
    pub report_until: Option<jiff::Timestamp>,
    /// Host-count scenario whose baseline is compared with the observed cohorts.
    pub calibration_hosts: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hosts: vec![Host {
                class: "big".into(),
                cores: 12,
                memory_gib: 31.0,
                runners: Vec::new(),
            }],
            default_demand: Demand::default(),
            jobs: Vec::new(),
            pools: Vec::new(),
            contention_label: None,
            semaphore_steps: Vec::new(),
            semaphore_release_steps: Vec::new(),
            heavy_slots: None,
            heavy_slots_per_host: Vec::new(),
            semaphore_timeout_seconds: 900.0,
            semaphore_poll_seconds: 0.0,
            semaphores: Vec::new(),
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
            factor_low_concurrency: None,
            speed_factors: Vec::new(),
            runner_choice: RunnerChoice::First,
            fit_since: None,
            fit_until: None,
            report_since: None,
            report_until: None,
            calibration_hosts: 1,
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
    /// Full-match regular expressions for runner names on this host. When any
    /// host lists patterns, observed jobs are fitted per host by runner name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runners: Vec<String>,
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
    /// Named semaphore pool gating this job; `heavy` already selects the heavy pool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semaphore: Option<String>,
}
impl Default for Demand {
    fn default() -> Self {
        Self {
            cores: 1,
            memory_gib: 1.0,
            heavy: false,
            host_class: None,
            semaphore: None,
        }
    }
}

/// Additional host-local semaphore with the heavy pool's wait, poll and fail-open rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Semaphore {
    /// Pool name referenced by `Demand::semaphore`; `heavy` is reserved.
    pub name: String,
    /// Step-name fragments of the recorded acquire (wait) step.
    pub acquire_steps: Vec<String>,
    /// Step-name fragments whose completion releases a slot; missing means job end.
    #[serde(default)]
    pub release_steps: Vec<String>,
    /// Slots on hosts without a per-host entry; absent leaves those hosts ungated.
    #[serde(default)]
    pub slots: Option<usize>,
    /// Per-host slots in host order.
    #[serde(default)]
    pub slots_per_host: Vec<usize>,
    /// Timestamped per-host slot changes for baseline replay.
    #[serde(default)]
    pub slot_history: Vec<SlotChange>,
    /// Fail-open timeout; defaults to `semaphore_timeout_seconds`.
    #[serde(default)]
    pub timeout_seconds: Option<f64>,
    /// Polling interval; defaults to `semaphore_poll_seconds`.
    #[serde(default)]
    pub poll_seconds: Option<f64>,
}

/// A semaphore pool's slot count on one host from `at` onward.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotChange {
    /// Host index in the configured list.
    pub host: usize,
    /// Effective UTC time.
    pub at: jiff::Timestamp,
    /// Slots from this time onward.
    pub slots: usize,
}

/// Index of the heavy pool among `Config::gate`.
pub(crate) const HEAVY_GATE: usize = 0;

/// One semaphore pool, resolved from either the legacy heavy keys or `semaphores`.
pub(crate) struct Gate<'a> {
    pub acquire: &'a [String],
    pub release: &'a [String],
    pub timeout: f64,
    pub poll: f64,
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
            || !self.semaphore_poll_seconds.is_finite()
            || self.semaphore_poll_seconds < 0.0
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
        self.validate_semaphores()?;
        self.validate_speed()?;
        self.validate_windows()?;
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
    fn validate_semaphores(&self) -> Result<(), Error> {
        let invalid = || Error::Invalid("invalid named semaphore pool".into());
        for (i, s) in self.semaphores.iter().enumerate() {
            if s.name.is_empty()
                || s.name == "heavy"
                || self.semaphores[..i].iter().any(|o| o.name == s.name)
                || s.acquire_steps.is_empty()
                || s.acquire_steps
                    .iter()
                    .chain(&s.release_steps)
                    .any(String::is_empty)
                || s.slots == Some(0)
                || s.slots_per_host.len() > self.hosts.len()
                || s.slots_per_host.contains(&0)
                || s.slot_history
                    .iter()
                    .any(|c| c.host >= self.hosts.len() || c.slots == 0)
                || s.timeout_seconds
                    .is_some_and(|t| !t.is_finite() || t <= 0.0)
                || s.poll_seconds.is_some_and(|p| !p.is_finite() || p < 0.0)
            {
                return Err(invalid());
            }
        }
        for demand in
            std::iter::once(&self.default_demand).chain(self.jobs.iter().map(|j| &j.demand))
        {
            if let Some(name) = &demand.semaphore
                && (demand.heavy || !self.semaphores.iter().any(|s| &s.name == name))
            {
                return Err(Error::Invalid(
                    "a job names an unknown semaphore pool or two pools".into(),
                ));
            }
        }
        Ok(())
    }
    /// Pools in index order: the heavy pool from the legacy keys, then `semaphores`.
    pub(crate) fn gate_count(&self) -> usize {
        1 + self.semaphores.len()
    }
    pub(crate) fn gate(&self, gate: usize) -> Gate<'_> {
        match gate.checked_sub(1).and_then(|i| self.semaphores.get(i)) {
            Some(s) => Gate {
                acquire: &s.acquire_steps,
                release: &s.release_steps,
                timeout: s.timeout_seconds.unwrap_or(self.semaphore_timeout_seconds),
                poll: s.poll_seconds.unwrap_or(self.semaphore_poll_seconds),
            },
            None => Gate {
                acquire: &self.semaphore_steps,
                release: &self.semaphore_release_steps,
                timeout: self.semaphore_timeout_seconds,
                poll: self.semaphore_poll_seconds,
            },
        }
    }
    /// Configured slots before any history change; `None` leaves the host ungated.
    pub(crate) fn gate_slots(&self, gate: usize, host: usize) -> Option<usize> {
        match gate.checked_sub(1).and_then(|i| self.semaphores.get(i)) {
            Some(s) => s.slots_per_host.get(host).copied().or(s.slots),
            None => self.heavy_slots_on(host),
        }
    }
    /// The pool gating a job with this demand, if any.
    pub(crate) fn gate_of(&self, demand: &Demand) -> Option<usize> {
        if demand.heavy {
            return Some(HEAVY_GATE);
        }
        let name = demand.semaphore.as_deref()?;
        self.semaphores
            .iter()
            .position(|s| s.name == name)
            .map(|i| i + 1)
    }
    /// Counterfactual without any semaphore pool.
    pub(crate) fn disable_gates(&mut self) {
        self.heavy_slots = None;
        self.heavy_slots_per_host.clear();
        for s in &mut self.semaphores {
            s.slots = None;
            s.slots_per_host.clear();
            s.slot_history.clear();
        }
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
