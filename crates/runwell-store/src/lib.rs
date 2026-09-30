//! SQLite journal and final job measurements for runwell.
//!
//! SQLite is the source of truth for durable runner identities and transition
//! records. Use WAL with one writer connection; never mix SQLite binding crates.

#![deny(missing_docs)]

/// Durable job measurement record, distinct from live Prometheus samples.
#[derive(Debug, Clone, Copy)]
pub struct JobMeasurement {
    /// GitHub runner request identity.
    pub request_id: i64,
    /// Cumulative CPU time in microseconds.
    pub cpu_usec: u64,
    /// Peak memory use in bytes.
    pub memory_peak: u64,
}

/// Persistence boundary using a sqlx SQLite pool.
pub struct Store {
    pool: sqlx::SqlitePool,
}

impl Store {
    /// Open and migrate the SQLite journal; currently unimplemented.
    pub async fn open(_database_url: &str) -> Result<Self, Error> {
        Err(Error::Unimplemented)
    }

    /// Access the pool for future focused repositories.
    pub fn pool(&self) -> &sqlx::SqlitePool {
        &self.pool
    }

    /// Persist final measurements idempotently; currently unimplemented.
    pub async fn record(&self, _measurement: &JobMeasurement) -> Result<(), Error> {
        Err(Error::Unimplemented)
    }
}

/// An operation that has not been implemented in this milestone.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The public interface is reserved for a later milestone.
    #[error("this operation is not implemented in the M0 bootstrap")]
    Unimplemented,
}
