use runwell_node::{JobPlan, LocalJob, ProcessState};
use runwell_store::{Job, JobMeasurement};
use serde::{Deserialize, Serialize};

/// Every lifecycle RPC is scoped to a durable job and attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    pub job_id: i64,
    pub attempt: i64,
}
/// Request bodies intentionally have no Debug implementation (Start carries JIT).
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    Register(Report),
    Report,
    Drain,
    Admit {
        key: Key,
        job: Job,
    },
    Prepare {
        key: Key,
        plan: JobPlan,
    },
    Workspace {
        key: Key,
        job: Job,
    },
    Bind {
        key: Key,
        execution: runwell_workspace::Execution,
    },
    Harvest {
        key: Key,
        job: Job,
    },
    Start {
        key: Key,
        agent_id: i64,
        jit: String,
    },
    Inspect(Key),
    Measure(Key),
    Stop(Key),
    Cleanup(Key),
    Orphan(Key),
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "result", content = "value", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Admitted(bool),
    Report(Report),
    Process(ProcessState),
    Measurement(JobMeasurement),
}
/// Full state snapshots also repair a missed event stream after reconnect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub node_id: String,
    pub sequence: i64,
    #[serde(default)]
    pub observed_at_ms: i64,
    pub draining: bool,
    pub cpu_slots: u32,
    pub memory_bytes: u64,
    pub reserved_cpu: u32,
    pub reserved_memory: u64,
    pub free_slots: u32,
    pub pressure: [f64; 4],
    pub template_version: String,
    pub jobs: Vec<JobStatus>,
    pub inventory: Vec<LocalJob>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobStatus {
    pub key: Key,
    pub phase: u8,
    pub process: ProcessState,
    #[serde(default)]
    pub started_at_ms: Option<i64>,
    #[serde(default)]
    pub heartbeat_at_ms: Option<i64>,
}
pub const ADMITTED: u8 = 0;
pub const PREPARED: u8 = 1;
pub const WORKSPACE_READY: u8 = 2;
pub const STARTING: u8 = 3;
pub const STARTED: u8 = 4;
pub const STOPPED: u8 = 5;
pub const CLEANED: u8 = 6;

impl Request {
    pub(crate) fn key(&self) -> Option<Key> {
        match self {
            Self::Admit { key, .. }
            | Self::Prepare { key, .. }
            | Self::Workspace { key, .. }
            | Self::Bind { key, .. }
            | Self::Harvest { key, .. }
            | Self::Start { key, .. }
            | Self::Inspect(key)
            | Self::Measure(key)
            | Self::Stop(key)
            | Self::Cleanup(key)
            | Self::Orphan(key) => Some(*key),
            _ => None,
        }
    }
}
