use crate::{Error, Store, writer::Mutation};
use runwell_scheduler::{Criticality, DurationEstimate, HistoryKey, HistorySnapshot};
use sqlx::{Acquire, Row, Sqlite, pool::PoolConnection};
use std::collections::BTreeMap;

/// Authoritative completed execution. Idempotent by repository/job/attempt.
#[derive(Debug, Clone)]
pub struct CompletedJob {
    /// Stable workflow/class key.
    pub key: HistoryKey,
    /// REST job identity (not the scale-set request ID).
    pub github_job_id: i64,
    /// REST workflow run identity.
    pub run_id: i64,
    /// GitHub run attempt, starting at one.
    pub attempt: u32,
    /// UTC UNIX completion seconds, used to order the sample window.
    pub completed_at: i64,
    /// Observed execution time, excluding queue time.
    pub duration_ms: u64,
    /// Time ready but waiting for admission/dispatch.
    pub queue_ms: u64,
    /// Authoritative GitHub conclusion; process exit alone is insufficient.
    pub conclusion: String,
    /// Known downstream graph shape.
    pub criticality: Criticality,
}
impl Store {
    /// Store completion and update the affected key's 128-success window in one
    /// writer transaction. Failed durations remain auditable but do not bias work.
    pub async fn record_completion(&self, job: CompletedJob) -> Result<(), Error> {
        self.write(Mutation::Completion(job)).await.map(|_| ())
    }
    /// Refresh only the changed key in a cached scheduling snapshot (indexed read).
    pub async fn duration_estimate(
        &self,
        key: &HistoryKey,
    ) -> Result<Option<DurationEstimate>, Error> {
        let row = sqlx::query(
            "SELECT * FROM duration_estimates WHERE repo=? AND workflow_job=? AND class=?",
        )
        .bind(&key.repo)
        .bind(&key.workflow_job)
        .bind(&key.class)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(estimate).transpose()
    }
    /// Load materialized estimates, never scanning job history. Cache this snapshot
    /// between completions; scheduling itself needs no database calls.
    pub async fn duration_history(
        &self,
        class_defaults: BTreeMap<String, DurationEstimate>,
    ) -> Result<HistorySnapshot, Error> {
        let rows = sqlx::query("SELECT * FROM duration_estimates")
            .fetch_all(&self.pool)
            .await?;
        let mut jobs = BTreeMap::new();
        for row in rows {
            jobs.insert(
                HistoryKey {
                    repo: row.try_get("repo")?,
                    workflow_job: row.try_get("workflow_job")?,
                    class: row.try_get("class")?,
                },
                estimate(&row)?,
            );
        }
        Ok(HistorySnapshot {
            jobs,
            class_defaults,
        })
    }
}
fn estimate(row: &sqlx::sqlite::SqliteRow) -> Result<DurationEstimate, Error> {
    Ok(DurationEstimate {
        p50_seconds: row.try_get("p50_seconds")?,
        p90_seconds: row.try_get("p90_seconds")?,
        samples: row.try_get("samples")?,
        criticality: Criticality {
            depth: row.try_get("depth")?,
            fan_out: row.try_get("fan_out")?,
        },
    })
}
pub(crate) async fn record(c: &mut PoolConnection<Sqlite>, j: CompletedJob) -> Result<i64, Error> {
    if j.key.repo.is_empty()
        || j.key.repo != j.key.repo.to_lowercase()
        || j.key.workflow_job.is_empty()
        || j.key.class.is_empty()
        || j.conclusion.is_empty()
    {
        return Err(Error::Corrupt);
    }
    let duration = i64::try_from(j.duration_ms).map_err(|_| Error::Corrupt)?;
    let queue = i64::try_from(j.queue_ms).map_err(|_| Error::Corrupt)?;
    let mut tx = c.begin().await?;
    let inserted = sqlx::query("INSERT INTO completed_jobs VALUES(?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(repo,github_job_id,attempt) DO NOTHING")
        .bind(&j.key.repo).bind(j.github_job_id).bind(j.run_id).bind(j.attempt).bind(&j.key.workflow_job).bind(&j.key.class)
        .bind(j.completed_at).bind(duration).bind(queue).bind(&j.conclusion).bind(j.criticality.depth).bind(j.criticality.fan_out)
        .execute(&mut *tx).await?.rows_affected();
    if inserted == 1 && j.conclusion == "success" && j.duration_ms > 0 {
        let rows = sqlx::query("SELECT duration_ms,depth,fan_out FROM completed_jobs WHERE repo=? AND workflow_job=? AND class=? AND conclusion='success' AND duration_ms>0 ORDER BY completed_at DESC,github_job_id DESC,attempt DESC LIMIT 128")
            .bind(&j.key.repo).bind(&j.key.workflow_job).bind(&j.key.class).fetch_all(&mut *tx).await?;
        let latest = rows.first().ok_or(Error::Corrupt)?;
        let depth: u32 = latest.try_get("depth")?;
        let fan_out: u32 = latest.try_get("fan_out")?;
        let mut samples: Vec<i64> = rows
            .iter()
            .map(|r| r.try_get("duration_ms"))
            .collect::<Result<_, _>>()?;
        samples.sort_unstable();
        let quantile =
            |pct: usize| samples[(samples.len() * pct).div_ceil(100) - 1] as f64 / 1000.0;
        sqlx::query("INSERT INTO duration_estimates VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(repo,workflow_job,class) DO UPDATE SET p50_seconds=excluded.p50_seconds,p90_seconds=excluded.p90_seconds,samples=excluded.samples,depth=excluded.depth,fan_out=excluded.fan_out")
            .bind(&j.key.repo).bind(&j.key.workflow_job).bind(&j.key.class).bind(quantile(50)).bind(quantile(90))
            .bind(samples.len() as i64).bind(depth).bind(fan_out).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(0)
}
