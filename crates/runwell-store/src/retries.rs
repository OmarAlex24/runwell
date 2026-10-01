use crate::{Error, Store, writer::Mutation};
use sqlx::{Acquire, Row, Sqlite, pool::PoolConnection};

/// Atomic budget and idempotency request. All failed jobs in the run must be infra.
#[derive(Debug, Clone)]
pub struct RetryClaim {
    /// Canonical lowercase owner/repository.
    pub repo: String,
    /// GitHub run identity.
    pub run_id: i64,
    /// Failed run attempt.
    pub attempt: u32,
    /// Complete, unique failed job IDs; cap is charged per job, not per POST.
    pub job_ids: Vec<i64>,
    /// UTC days since UNIX epoch, derived from controller time.
    pub utc_day: i64,
    /// Maximum automatically retried jobs per repository/day; zero disables.
    pub daily_cap: u32,
}
/// Durable send outcome. Every state consumes the claim and daily budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryStatus {
    /// Persisted before network I/O; recovery must never blindly resend it.
    Claimed,
    /// GitHub returned 201.
    Accepted,
    /// GitHub explicitly rejected the operation.
    Rejected,
    /// Timeout, transport loss or server error: remote outcome is uncertain.
    Ambiguous,
}
impl RetryStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Claimed => "claimed",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Ambiguous => "ambiguous",
        }
    }
}
/// Durable retry audit entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryRecord {
    /// Number of failed jobs charged to the daily budget.
    pub job_count: u32,
    /// Day the claim was made, unaffected by restart or outcome.
    pub utc_day: i64,
    /// Send outcome.
    pub status: RetryStatus,
}
impl Store {
    /// Serialize deduplication and the daily budget check with the write-ahead
    /// claim. A claimed preceding attempt blocks an automatic retry chain.
    pub async fn claim_retry(&self, claim: RetryClaim) -> Result<bool, Error> {
        Ok(self.write(Mutation::RetryClaim(claim)).await? == 1)
    }
    /// Record the outcome without releasing the claim, even on an HTTP failure.
    pub async fn finish_retry(
        &self,
        repo: &str,
        run: i64,
        attempt: u32,
        status: RetryStatus,
    ) -> Result<(), Error> {
        self.write(Mutation::RetryFinish(repo.into(), run, attempt, status))
            .await
            .map(|_| ())
    }
    /// Inspect a claim after restart; claimed/ambiguous entries require reconciliation.
    pub async fn retry_record(
        &self,
        repo: &str,
        run: i64,
        attempt: u32,
    ) -> Result<Option<RetryRecord>, Error> {
        let row = sqlx::query("SELECT job_count,utc_day,status FROM retry_claims WHERE repo=? AND run_id=? AND attempt=?")
            .bind(repo).bind(run).bind(attempt).fetch_optional(&self.pool).await?;
        row.map(|r| {
            Ok(RetryRecord {
                job_count: r.try_get("job_count")?,
                utc_day: r.try_get("utc_day")?,
                status: match r.try_get::<&str, _>("status")? {
                    "claimed" => RetryStatus::Claimed,
                    "accepted" => RetryStatus::Accepted,
                    "rejected" => RetryStatus::Rejected,
                    "ambiguous" => RetryStatus::Ambiguous,
                    _ => return Err(Error::Corrupt),
                },
            })
        })
        .transpose()
    }
}
pub(crate) async fn claim(c: &mut PoolConnection<Sqlite>, j: RetryClaim) -> Result<i64, Error> {
    if j.repo.is_empty()
        || j.repo != j.repo.to_lowercase()
        || j.run_id <= 0
        || j.attempt == 0
        || j.job_ids.is_empty()
        || j.job_ids.iter().any(|&id| id <= 0)
        || j.job_ids
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != j.job_ids.len()
    {
        return Err(Error::Corrupt);
    }
    let count = i64::try_from(j.job_ids.len()).map_err(|_| Error::Corrupt)?;
    let mut tx = c.begin().await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM retry_claims WHERE repo=? AND run_id=? AND attempt IN (?,?))",
    )
    .bind(&j.repo)
    .bind(j.run_id)
    .bind(j.attempt)
    .bind(j.attempt - 1)
    .fetch_one(&mut *tx)
    .await?;
    let used: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(job_count),0) FROM retry_claims WHERE repo=? AND utc_day=?",
    )
    .bind(&j.repo)
    .bind(j.utc_day)
    .fetch_one(&mut *tx)
    .await?;
    if exists || used.saturating_add(count) > i64::from(j.daily_cap) {
        return Ok(0);
    }
    sqlx::query("INSERT INTO retry_claims VALUES(?,?,?,?,?,'claimed')")
        .bind(&j.repo)
        .bind(j.run_id)
        .bind(j.attempt)
        .bind(j.utc_day)
        .bind(count)
        .execute(&mut *tx)
        .await?;
    for id in j.job_ids {
        sqlx::query("INSERT INTO retried_jobs VALUES(?,?,?,?)")
            .bind(&j.repo)
            .bind(j.run_id)
            .bind(j.attempt)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(1)
}
pub(crate) async fn finish(
    c: &mut PoolConnection<Sqlite>,
    repo: &str,
    run: i64,
    attempt: u32,
    status: RetryStatus,
) -> Result<i64, Error> {
    let changed = sqlx::query("UPDATE retry_claims SET status=? WHERE repo=? AND run_id=? AND attempt=? AND (status='claimed' OR status=?)")
        .bind(status.as_str()).bind(repo).bind(run).bind(attempt).bind(status.as_str()).execute(&mut **c).await?.rows_affected();
    if changed == 0 {
        return Err(Error::Transition);
    }
    Ok(0)
}
