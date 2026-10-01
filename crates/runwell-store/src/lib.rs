//! WAL SQLite journal with migrations and one serialized writer task.
//! Write-ahead runner identities and durable cleanup markers make restart and
//! redelivery safe. No API accepts JIT credentials or authentication material.
#![deny(missing_docs)]
mod history;
mod model;
mod network;
mod network_writer;
mod spool;
mod spool_writer;
pub use network::{FailureEvent, NodeLease, Placement};
pub use spool::LeaseRecord;
mod read;
mod retries;
mod writer;
pub use history::CompletedJob;
pub use model::*;
pub use retries::{RetryClaim, RetryRecord, RetryStatus};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use std::{str::FromStr, time::Duration};
use tokio::sync::{mpsc, oneshot};
use writer::{Command, Mutation};

/// Cloneable persistence boundary. Dropping all handles drains the writer queue.
#[derive(Clone)]
pub struct Store {
    pool: sqlx::SqlitePool,
    writer: mpsc::Sender<Command>,
}
impl Store {
    /// Open/migrate a database in WAL mode with FULL crash durability.
    pub async fn open(database_url: &str) -> Result<Self, Error> {
        let options = SqliteConnectOptions::from_str(database_url)
            .map_err(|_| Error::Database)?
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .busy_timeout(Duration::from_secs(10));
        // One held writer plus read connections. A single-connection in-memory DB
        // would deadlock; tests use a temporary file, exercising actual WAL.
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        sqlx::migrate!()
            .run(&pool)
            .await
            .map_err(|_| Error::Database)?;
        let connection = pool.acquire().await?;
        let (writer, receiver) = mpsc::channel(64);
        tokio::spawn(writer::run(connection, receiver));
        Ok(Self { pool, writer })
    }
    async fn write(&self, mutation: Mutation) -> Result<i64, Error> {
        let (reply, receive) = oneshot::channel();
        self.writer
            .send(Command { mutation, reply })
            .await
            .map_err(|_| Error::Closed)?;
        receive.await.map_err(|_| Error::Closed)?
    }
    /// Insert demand once and return its stable local identity.
    pub async fn queue(&self, job: NewJob) -> Result<i64, Error> {
        self.write(Mutation::Queue(job)).await
    }
    /// Apply an explicit legal transition; repeating the current state is a no-op.
    pub async fn transition(&self, id: i64, state: State) -> Result<(), Error> {
        self.write(Mutation::Transition(id, state))
            .await
            .map(|_| ())
    }
    /// Persist unique identity before creating a remote runner.
    pub async fn runner_intent(&self, runner: Runner) -> Result<(), Error> {
        self.write(Mutation::Intent(runner)).await.map(|_| ())
    }
    /// Update an unlaunched reservation to the current immutable template.
    /// Agent identity and execution states make this illegal after registration.
    pub async fn retarget_template(&self, id: i64, version: String) -> Result<(), Error> {
        self.write(Mutation::Retarget(id, version))
            .await
            .map(|_| ())
    }
    /// Journal acquisition before generating JIT credentials.
    pub async fn acquired(&self, id: i64) -> Result<(), Error> {
        self.write(Mutation::Acquired(id)).await.map(|_| ())
    }
    /// Persist the agent ID and RunnerCreated state atomically.
    pub async fn registered(&self, id: i64, agent: i64) -> Result<(), Error> {
        self.write(Mutation::Registered(id, agent))
            .await
            .map(|_| ())
    }
    /// Bind execution to the actual GitHub request, which can differ from acquisition.
    pub async fn bind(
        &self,
        id: i64,
        execution: Execution,
        outcome: Option<String>,
    ) -> Result<(), Error> {
        self.write(Mutation::Bind(id, execution, outcome))
            .await
            .map(|_| ())
    }
    /// Persist the main process exit before remote deletion or service collection.
    pub async fn exited(&self, id: i64, code: Option<i32>) -> Result<(), Error> {
        self.write(Mutation::Exited(id, code)).await.map(|_| ())
    }
    /// Remember successful remote removal before local destructive cleanup.
    pub async fn remote_deleted(&self, id: i64) -> Result<(), Error> {
        self.write(Mutation::Deleted(id)).await.map(|_| ())
    }
    /// Mark local cleanup complete; requires a terminal state and remote removal.
    pub async fn cleaned(&self, id: i64) -> Result<(), Error> {
        self.write(Mutation::Cleaned(id)).await.map(|_| ())
    }
    /// Commit the final sample once, before stopping its cgroup.
    pub async fn record(&self, measurement: &JobMeasurement) -> Result<(), Error> {
        self.write(Mutation::Measure(measurement.clone()))
            .await
            .map(|_| ())
    }
    /// Record the last successfully acknowledged ID. Call after remote ack.
    pub async fn acked(&self, set: i64, message: i64) -> Result<(), Error> {
        self.write(Mutation::Ack(set, message)).await.map(|_| ())
    }
}
/// Sanitized journal errors; SQL and bound data are never formatted.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// SQLite or migration error.
    #[error("journal operation failed")]
    Database,
    /// Illegal state transition or missing lifecycle prerequisite.
    #[error("illegal journal state transition")]
    Transition,
    /// Writer task unavailable.
    #[error("journal writer is closed")]
    Closed,
    /// Invalid durable data.
    #[error("invalid journal data")]
    Corrupt,
}
impl From<sqlx::Error> for Error {
    fn from(error: sqlx::Error) -> Self {
        if let sqlx::Error::Database(ref e) = error
            && e.message() == "illegal job state transition"
        {
            return Self::Transition;
        }
        Self::Database
    }
}

#[cfg(test)]
mod spool_tests;
