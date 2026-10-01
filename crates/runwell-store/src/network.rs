//! Fleet placement, execution spool and idempotent failure delivery journal.
use crate::{Error, Store, writer::Mutation};
use serde::{Deserialize, Serialize};

/// Controller's immutable placement for one attempt.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Placement {
    /// Journal job identity.
    pub job_id: i64,
    /// Attempt number; retries allocate a new job identity.
    pub attempt: i64,
    /// Authenticated node identity.
    pub node_id: String,
    /// Controller clock milliseconds.
    pub assigned_at: i64,
    /// Partition/watchdog fencing remains set after reconnect.
    pub lost: bool,
    /// Controller clock at the first acknowledged or observed execution start.
    pub execution_started_at: Option<i64>,
}
/// Secret-free, locally durable execution reservation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeLease {
    /// Original job metadata, before actual assignment.
    pub job: crate::Job,
    /// Monotonically allocated attempt.
    pub attempt: i64,
    /// Monotonic execution stage.
    pub phase: u8,
    /// Locally validated preparation plan encoded as JSON, never launch credentials.
    pub plan: Option<String>,
    /// Final sample retained across duplicate requests and cleanup.
    pub measurement: Option<crate::JobMeasurement>,
    /// Node clock at the durable start intent, excluding admission/preparation.
    #[serde(default)]
    pub started_at_ms: Option<i64>,
    /// Most recent independently observed runner renewal.
    #[serde(default)]
    pub heartbeat_at_ms: Option<i64>,
}
/// Durable logical failure event. Consumers deduplicate by (job_id, attempt).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct FailureEvent {
    /// Durable job identity.
    pub job_id: i64,
    /// Attempt identity.
    pub attempt: i64,
    /// Stable infrastructure/watchdog reason.
    pub reason: String,
}
/// Mutations share the existing single writer connection.
pub(crate) enum NetworkMutation {
    Place(Placement),
    LostBefore(i64),
    Snapshot(String, i64, i64, String),
    Lease(Box<NodeLease>),
    Heartbeat(i64, i64, i64),
    ExecutionStarted(i64, i64),
    Drain(bool),
    Sequence,
    Failure(FailureEvent),
    Delivered(i64, i64),
    Jit(i64),
    Release(String, i64),
}
impl Store {
    /// Persist lost-node status using the controller clock, even for idle nodes.
    pub async fn mark_lost_nodes(&self, before: i64) -> Result<(), Error> {
        self.write(Mutation::Network(NetworkMutation::LostBefore(before)))
            .await
            .map(|_| ())
    }
    /// Durable node identity and lost status; fresh accepted reports clear this.
    pub async fn node_health(&self) -> Result<Vec<(String, bool)>, Error> {
        Ok(
            sqlx::query_as("SELECT node_id,lost FROM node_health ORDER BY node_id")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    /// Last controller-selected runner release, retained across restart.
    pub async fn release(&self) -> Result<Option<(String, i64)>, Error> {
        Ok(
            sqlx::query_as("SELECT version,checked_at FROM controller_release WHERE singleton=1")
                .fetch_optional(&self.pool)
                .await?,
        )
    }
    /// Persist release policy without changing existing runner installations.
    pub async fn select_release(&self, version: String, checked: i64) -> Result<(), Error> {
        self.write(Mutation::Network(NetworkMutation::Release(
            version, checked,
        )))
        .await
        .map(|_| ())
    }

    /// Pin placement before admission; duplicate calls cannot move it.
    pub async fn place(&self, placement: Placement) -> Result<(), Error> {
        self.write(Mutation::Network(NetworkMutation::Place(placement)))
            .await
            .map(|_| ())
    }
    /// Read all placements, including fenced and cleaned attempts.
    pub async fn placements(&self) -> Result<Vec<Placement>, Error> {
        Ok(sqlx::query_as("SELECT * FROM placements ORDER BY job_id")
            .fetch_all(&self.pool)
            .await?)
    }
    /// Persist only strictly newer reports; stale reports cannot refresh liveness.
    pub async fn snapshot(
        &self,
        id: String,
        sequence: i64,
        now: i64,
        payload: String,
    ) -> Result<bool, Error> {
        Ok(self
            .write(Mutation::Network(NetworkMutation::Snapshot(
                id, sequence, now, payload,
            )))
            .await?
            > 0)
    }
    /// Last accepted report and controller receipt time.
    pub async fn snapshots(&self) -> Result<Vec<(String, i64, String)>, Error> {
        Ok(sqlx::query_as(
            "SELECT node_id,received_at,payload FROM node_snapshots ORDER BY node_id",
        )
        .fetch_all(&self.pool)
        .await?)
    }
    /// Persist a node lease before host side effects.
    pub async fn lease(&self, lease: NodeLease) -> Result<(), Error> {
        self.write(Mutation::Network(NetworkMutation::Lease(Box::new(lease))))
            .await
            .map(|_| ())
    }
    /// Persistently stop admissions until an operator explicitly removes the drain.
    pub async fn drain_node(&self) -> Result<(), Error> {
        self.write(Mutation::Network(NetworkMutation::Drain(true)))
            .await
            .map(|_| ())
    }
    /// Explicit operator action to resume admissions after a drain/upgrade.
    pub async fn resume_node(&self) -> Result<(), Error> {
        self.write(Mutation::Network(NetworkMutation::Drain(false)))
            .await
            .map(|_| ())
    }
    /// Whether this node is draining.
    pub async fn node_draining(&self) -> Result<bool, Error> {
        Ok(
            sqlx::query_scalar("SELECT draining FROM node_control WHERE singleton=1")
                .fetch_one(&self.pool)
                .await?,
        )
    }
    /// Crash-durable monotonically increasing report sequence.
    pub async fn next_sequence(&self) -> Result<i64, Error> {
        self.write(Mutation::Network(NetworkMutation::Sequence))
            .await
    }
    /// Fence placement and enqueue one logical failure atomically.
    pub async fn fail_attempt(&self, event: FailureEvent) -> Result<(), Error> {
        self.write(Mutation::Network(NetworkMutation::Failure(event)))
            .await
            .map(|_| ())
    }
    /// Pending M5a failure callbacks.
    pub async fn failures(&self) -> Result<Vec<FailureEvent>, Error> {
        Ok(sqlx::query_as(
            "SELECT job_id,attempt,reason FROM failure_outbox WHERE delivered=0 ORDER BY job_id",
        )
        .fetch_all(&self.pool)
        .await?)
    }
    /// Acknowledge an idempotently processed callback.
    pub async fn failure_delivered(&self, job: i64, attempt: i64) -> Result<(), Error> {
        self.write(Mutation::Network(NetworkMutation::Delivered(job, attempt)))
            .await
            .map(|_| ())
    }
    /// Claim a JIT POST exactly once. Ambiguous outcomes require name lookup, never another POST.
    pub async fn claim_jit(&self, job: i64) -> Result<bool, Error> {
        Ok(self
            .write(Mutation::Network(NetworkMutation::Jit(job)))
            .await?
            == 1)
    }
}
