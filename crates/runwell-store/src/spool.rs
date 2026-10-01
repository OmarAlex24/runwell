use crate::{Error, NodeLease, Store, network::NetworkMutation, writer::Mutation};

/// Compact idempotency record, including cleaned attempts without job/plan payloads.
#[derive(Debug, sqlx::FromRow)]
pub struct LeaseRecord {
    /// Durable attempt identity.
    pub attempt: i64,
    /// Monotonic execution phase.
    pub phase: u8,
    /// Durable start intent in node clock milliseconds.
    pub started_at_ms: Option<i64>,
    /// Last independently observed runner renewal.
    pub heartbeat_at_ms: Option<i64>,
    measurement: Option<String>,
}
impl LeaseRecord {
    /// Retained result for idempotent measurement after cleanup.
    pub fn measurement(&self) -> Result<Option<crate::JobMeasurement>, Error> {
        self.measurement
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|_| Error::Corrupt)
    }
}
#[derive(sqlx::FromRow)]
struct ActiveLease {
    payload: String,
    started_at_ms: Option<i64>,
    heartbeat_at_ms: Option<i64>,
}
impl ActiveLease {
    fn decode(self) -> Result<NodeLease, Error> {
        let mut lease: NodeLease =
            serde_json::from_str(&self.payload).map_err(|_| Error::Corrupt)?;
        lease.started_at_ms = self.started_at_ms;
        lease.heartbeat_at_ms = self.heartbeat_at_ms;
        Ok(lease)
    }
}
impl Store {
    /// Indexed lookup of one immutable placement.
    pub async fn placement(&self, job: i64) -> Result<Option<crate::Placement>, Error> {
        Ok(sqlx::query_as("SELECT * FROM placements WHERE job_id=?")
            .bind(job)
            .fetch_optional(&self.pool)
            .await?)
    }

    /// Indexed active-only spool read; never loads historical job or plan payloads.
    pub async fn active_leases(&self) -> Result<Vec<NodeLease>, Error> {
        sqlx::query_as::<_, ActiveLease>("SELECT payload,started_at_ms,heartbeat_at_ms FROM node_leases WHERE phase<6 ORDER BY job_id")
            .fetch_all(&self.pool).await?.into_iter().map(ActiveLease::decode).collect()
    }
    /// Indexed lookup of a single active attempt.
    pub async fn node_lease(&self, job: i64, attempt: i64) -> Result<Option<NodeLease>, Error> {
        sqlx::query_as::<_, ActiveLease>("SELECT payload,started_at_ms,heartbeat_at_ms FROM node_leases WHERE job_id=? AND attempt=? AND phase<6")
            .bind(job).bind(attempt).fetch_optional(&self.pool).await?.map(ActiveLease::decode).transpose()
    }
    /// Indexed compact lookup, including tombstones and conflicting attempts.
    pub async fn lease_record(&self, job: i64) -> Result<Option<LeaseRecord>, Error> {
        Ok(sqlx::query_as("SELECT attempt,phase,started_at_ms,heartbeat_at_ms,measurement FROM node_leases WHERE job_id=?")
            .bind(job).fetch_optional(&self.pool).await?)
    }
    /// Record runner evidence monotonically without racing lifecycle phase updates.
    pub async fn runner_heartbeat(&self, job: i64, attempt: i64, at: i64) -> Result<(), Error> {
        self.write(Mutation::Network(NetworkMutation::Heartbeat(
            job, attempt, at,
        )))
        .await
        .map(|_| ())
    }
    /// Record execution independently of assignment and preparation, once.
    pub async fn execution_started(&self, job: i64, at: i64) -> Result<(), Error> {
        self.write(Mutation::Network(NetworkMutation::ExecutionStarted(
            job, at,
        )))
        .await
        .map(|_| ())
    }
}
