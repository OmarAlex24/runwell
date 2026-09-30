use serde::{Deserialize, Serialize};

/// Durable lifecycle states. Terminal states never transition back to active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Observed demand, with no local reservation.
    Queued,
    /// Durable local reservation; acquisition may be retried.
    Admitted,
    /// Agent identity persisted before launch.
    RunnerCreated,
    /// The runner service has been started or re-adopted.
    Running,
    /// GitHub reported successful job completion.
    Completed,
    /// GitHub reported failure or the process failed.
    Failed,
    /// Execution disappeared or exited without an authoritative job result.
    Orphaned,
}
impl State {
    /// Stable journal spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Admitted => "admitted",
            Self::RunnerCreated => "runner_created",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Orphaned => "orphaned",
        }
    }
    /// Whether this job has reached its final state (cleanup can still be pending).
    pub fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Orphaned)
    }
}
impl std::str::FromStr for State {
    type Err = crate::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "queued" => Ok(Self::Queued),
            "admitted" => Ok(Self::Admitted),
            "runner_created" => Ok(Self::RunnerCreated),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "orphaned" => Ok(Self::Orphaned),
            _ => Err(crate::Error::Corrupt),
        }
    }
}
/// Metadata for durable, deduplicated demand. No credential fields are accepted.
#[derive(Debug, Clone)]
pub struct NewJob {
    /// Owning GitHub scale set.
    pub scale_set_id: i64,
    /// GitHub runner request identity; negative values identify statistical demand.
    pub request_id: i64,
    /// GitHub job UUID, if known.
    pub github_job_id: String,
    /// GitHub workflow run identity.
    pub workflow_run_id: i64,
    /// Repository owner/name.
    pub repo: String,
    /// Display name.
    pub name: String,
    /// Configured class name.
    pub class: String,
    /// Reserved CPU slots, retained across configuration changes.
    pub reserved_cpu: u32,
    /// Reserved RAM in bytes.
    pub reserved_memory: u64,
}
/// Actual workflow execution bound from a runner event, independent of the
/// request originally acquired to provision its capacity.
#[derive(Debug, Clone)]
pub struct Execution {
    /// Actual GitHub request identity.
    pub request_id: i64,
    /// Actual GitHub job UUID; empty means not supplied by this event.
    pub github_job_id: String,
    /// Actual workflow run; zero means not supplied.
    pub workflow_run_id: i64,
    /// Actual repository owner/name; empty means not supplied.
    pub repo: String,
    /// Actual display name; empty means not supplied.
    pub name: String,
}
/// Persisted job snapshot.
#[derive(Debug, Clone)]
pub struct Job {
    /// Local monotonically allocated identity, used for directories and units.
    pub id: i64,
    /// Durable input metadata.
    pub metadata: NewJob,
    /// Current lifecycle state.
    pub state: State,
    /// Whether acquirejobs has succeeded (or statistics proved assignment).
    pub acquired: bool,
    /// Actual request assigned to this runner, from JobStarted/Completed.
    pub actual_request_id: Option<i64>,
    /// Authoritative GitHub result, never inferred from exit zero.
    pub outcome: Option<String>,
    /// UNIX seconds when the authoritative completion event arrived.
    pub outcome_at: Option<i64>,
    /// UNIX seconds when the service started.
    pub started_at: Option<i64>,
}
/// Write-ahead runner identity, persisted before the JIT POST.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Runner {
    /// Owning local job/reservation.
    pub job_id: i64,
    /// Globally unique deterministic registration name.
    pub name: String,
    /// Unique installation directory.
    pub dir: String,
    /// Unique systemd service name.
    pub unit: String,
    /// Immutable runner template generation.
    pub template_version: String,
    /// GitHub agent ID, persisted before spawning.
    pub agent_id: Option<i64>,
    /// Observed listener exit status, retained after service collection.
    pub exit_code: Option<i32>,
    /// Remote DELETE succeeded or runner lookup proved absence.
    pub remote_deleted: bool,
    /// Local teardown succeeded.
    pub cleaned: bool,
}
/// Cumulative PSI totals (microseconds) and final avg10 snapshots.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct PsiMeasurement {
    /// CPU some avg10.
    pub cpu: f64,
    /// Memory some avg10.
    pub memory: f64,
    /// I/O some avg10.
    pub io: f64,
    /// Memory full avg10.
    pub memory_full: f64,
    /// CPU some total microseconds.
    pub cpu_total: u64,
    /// Memory some total microseconds.
    pub memory_total: u64,
    /// I/O some total microseconds.
    pub io_total: u64,
    /// Memory full total microseconds.
    pub memory_full_total: u64,
}
/// Final durable cgroup sample, written before teardown.
#[derive(Debug, Clone, Default)]
pub struct JobMeasurement {
    /// Local job/reservation identity.
    pub job_id: i64,
    /// Cumulative CPU usage.
    pub cpu_usec: u64,
    /// Current memory consumption.
    pub memory_current: u64,
    /// Peak memory consumption.
    pub memory_peak: u64,
    /// Aggregate block read bytes.
    pub io_read_bytes: u64,
    /// Aggregate block write bytes.
    pub io_write_bytes: u64,
    /// Pressure statistics.
    pub psi: PsiMeasurement,
    /// Cgroup OOM kills.
    pub oom_kills: u64,
    /// Elapsed service duration.
    pub duration_ms: u64,
    /// Listener exit code, absent for a vanished process.
    pub exit_code: Option<i32>,
    /// Confirmed infrastructure signal (currently OOM).
    pub infra_signal: bool,
}
