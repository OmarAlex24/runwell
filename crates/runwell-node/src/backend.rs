use crate::{Error, NodeFuture};
use runwell_admission::Pressure;
use runwell_runner::LaunchSpec;
use runwell_store::JobMeasurement;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Resource constraints for a job subtree. Swap is always disabled.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SliceSpec {
    /// Local durable identity.
    pub job_id: u64,
    /// Soft memory throttle in bytes.
    pub memory_high: u64,
    /// Hard memory limit in bytes.
    pub memory_max: u64,
    /// CPU fairness weight.
    pub cpu_weight: u32,
    /// Maximum concurrent tasks across this job.
    pub tasks_max: u64,
}
impl SliceSpec {
    /// Actual dash ancestry: ci.slice / ci-rw.slice / ci-rw-jN.slice.
    pub fn unit(&self) -> String {
        slice_unit(self.job_id)
    }
    /// Fail closed before a systemd start with invalid limits.
    pub fn validate(&self) -> Result<(), Error> {
        if self.job_id == 0
            || self.memory_high == 0
            || self.memory_max < self.memory_high
            || !(1..=10000).contains(&self.cpu_weight)
            || self.tasks_max == 0
        {
            return Err(Error::Config);
        }
        Ok(())
    }
}
/// Deterministic systemd slice name whose ancestry is rooted in ci.slice.
pub fn slice_unit(id: u64) -> String {
    format!("ci-rw-j{id}.slice")
}
/// Deterministic runner service name.
pub fn service_unit(id: u64) -> String {
    format!("rw-j{id}.service")
}
/// Preparation plan containing no credentials; safe for an authenticated RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobPlan {
    /// Job slice limits.
    pub slice: SliceSpec,
    /// Independent install root, derived from the local durable ID.
    pub directory: PathBuf,
    /// Immutable runner generation.
    pub template_version: String,
}
/// Observed process state, independent of GitHub workflow result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessState {
    /// Running, activating, or stopping; must be preserved on restart.
    Running,
    /// Main process exited; slice still exists for measurement.
    Exited(Option<i32>),
    /// Unit no longer exists.
    Absent,
}
/// Inventory identity, combining surviving units and installation directories.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalJob {
    /// Durable local job identity inferred from the name.
    pub id: u64,
}
/// Local-node boundary. Each operation is idempotent by durable job identity.
/// M5 can implement these same methods as authenticated network calls.
pub trait NodeBackend: Send + Sync {
    /// Fleet adapters own admission and scheduling; local backends retain the
    /// standalone controller's in-process reservation implementation.
    fn manages_admission(&self) -> bool {
        false
    }
    /// Reconcile reports and deliver durable failure/metrics hooks.
    fn maintenance(&self) -> NodeFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    /// Schedule using a pure policy; return only jobs that can make progress.
    fn schedule<'a>(
        &'a self,
        jobs: &'a [runwell_store::Job],
        _now: u64,
    ) -> NodeFuture<'a, Vec<i64>> {
        Box::pin(async move { Ok(jobs.iter().map(|j| j.id).collect()) })
    }
    /// Authoritative node acceptance, before acquisition or JIT generation.
    fn admit<'a>(&'a self, _job: &'a runwell_store::Job) -> NodeFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
    /// Optional fleet-wide realizable class capacities.
    fn fleet_capacities<'a>(
        &'a self,
        _classes: &'a std::collections::BTreeMap<i64, runwell_config::JobClass>,
    ) -> NodeFuture<'a, Option<std::collections::BTreeMap<i64, u32>>> {
        Box::pin(async { Ok(None) })
    }

    /// Ensure parent limits before any admission.
    fn initialize(&self) -> NodeFuture<'_, ()>;
    /// Host and ci.slice PSI combined conservatively.
    fn pressure(&self) -> NodeFuture<'_, Pressure>;
    /// Current verified template; handles the release-manager update policy.
    fn template_version(&self) -> NodeFuture<'_, String>;
    /// Request an immediate update after an outdated-runner signal.
    fn refresh_template(&self) -> NodeFuture<'_, String> {
        self.template_version()
    }
    /// Create the limited slice and private installation before registration.
    fn prepare<'a>(&'a self, plan: &'a JobPlan) -> NodeFuture<'a, ()>;
    /// Prepare warm HOME after installation and before JIT registration.
    fn prepare_workspace<'a>(&'a self, _job: &'a runwell_store::Job) -> NodeFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    /// Persist actual assignment metadata before the queue delivery is acked.
    fn bind_workspace(
        &self,
        _id: u64,
        _execution: runwell_workspace::Execution,
    ) -> NodeFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    /// Reconcile mounts using durable jobs retained for adoption or cleanup.
    fn reconcile_workspaces<'a>(
        &'a self,
        _retained: &'a std::collections::HashSet<u64>,
    ) -> NodeFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    /// Harvest after success is journaled and all job writers are stopped.
    fn harvest_workspace<'a>(&'a self, _job: &'a runwell_store::Job) -> NodeFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    /// Start Runner.Listener directly; duplicate starts must not replace a unit.
    fn start<'a>(&'a self, plan: &'a JobPlan, launch: &'a LaunchSpec) -> NodeFuture<'a, ()>;
    /// Restore in-process resources for a surviving runner during reconcile.
    fn recover<'a>(&'a self, _plan: &'a JobPlan) -> NodeFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    /// Timestamp of the last runner job-renewal heartbeat, independent of host
    /// liveness or process state. None means no renewal evidence is available.
    fn runner_heartbeat(&self, _id: u64) -> NodeFuture<'_, Option<i64>> {
        Box::pin(async { Ok(None) })
    }
    /// Observe the runner process without treating liveness as a heartbeat.
    fn inspect(&self, id: u64) -> NodeFuture<'_, ProcessState>;
    /// Sample final counters while the slice still exists.
    fn measure(&self, id: u64) -> NodeFuture<'_, JobMeasurement>;
    /// Stop the service and job-owned Docker resources, keeping the slice for final sampling.
    /// The controller must journal a successful DELETE before calling this.
    fn stop_runner(&self, id: u64) -> NodeFuture<'_, ()>;
    /// Durable terminal-result hook for classification, learned durations and metrics.
    /// May be redelivered after a crash; implementations must deduplicate by job.
    fn finished<'a>(
        &'a self,
        _job: &'a runwell_store::Job,
        _sample: &'a JobMeasurement,
    ) -> NodeFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    /// List owned services/slices and independent installation directories.
    fn inventory(&self) -> NodeFuture<'_, Vec<LocalJob>>;
    /// Stop and await units, then remove the installation. Called only after
    /// remote DELETE permitted it and final statistics were durably recorded.
    fn cleanup(&self, id: u64) -> NodeFuture<'_, ()>;
}
