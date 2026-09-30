use crate::{Error, Job, JobMeasurement, NewJob, Runner, Store};
use sqlx::{Row, sqlite::SqliteRow};
impl Store {
    /// UNIX timestamp of the first final sample, for completion-event grace time.
    pub async fn measured_at(&self, id: i64) -> Result<Option<i64>, Error> {
        Ok(
            sqlx::query_scalar("SELECT recorded_at FROM measurements WHERE job_id=?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    /// Snapshot all demand, including terminal history, in stable arrival order.
    pub async fn jobs(&self) -> Result<Vec<Job>, Error> {
        sqlx::query("SELECT * FROM jobs ORDER BY id")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(job)
            .collect()
    }
    /// Load one durable job.
    pub async fn job(&self, id: i64) -> Result<Job, Error> {
        job(&sqlx::query("SELECT * FROM jobs WHERE id=?")
            .bind(id)
            .fetch_one(&self.pool)
            .await?)
    }
    /// Snapshot all runner identities, including completed cleanup history.
    pub async fn runners(&self) -> Result<Vec<Runner>, Error> {
        Ok(sqlx::query_as("SELECT * FROM runners ORDER BY job_id")
            .fetch_all(&self.pool)
            .await?)
    }
    /// Load a runner by local job ID.
    pub async fn runner(&self, id: i64) -> Result<Option<Runner>, Error> {
        Ok(sqlx::query_as("SELECT * FROM runners WHERE job_id=?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?)
    }
    /// Last remote acknowledgment, for audit; session generations can reset IDs.
    pub async fn last_acked(&self, set: i64) -> Result<Option<i64>, Error> {
        Ok(
            sqlx::query_scalar("SELECT last_acked_id FROM messages WHERE scale_set_id=?")
                .bind(set)
                .fetch_optional(&self.pool)
                .await?,
        )
    }
    /// Read a final sample to avoid overwriting it after the cgroup disappeared.
    pub async fn measurement(&self, id: i64) -> Result<Option<JobMeasurement>, Error> {
        let Some(r) = sqlx::query("SELECT * FROM measurements WHERE job_id=?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(JobMeasurement {
            job_id: id,
            cpu_usec: unsigned(&r, "cpu_usec")?,
            memory_current: unsigned(&r, "memory_current")?,
            memory_peak: unsigned(&r, "memory_peak")?,
            io_read_bytes: unsigned(&r, "io_read_bytes")?,
            io_write_bytes: unsigned(&r, "io_write_bytes")?,
            oom_kills: unsigned(&r, "oom_kills")?,
            duration_ms: unsigned(&r, "duration_ms")?,
            exit_code: r.try_get("exit_code")?,
            infra_signal: r.try_get("infra_signal")?,
            psi: serde_json::from_str(r.try_get::<&str, _>("psi")?).map_err(|_| Error::Corrupt)?,
        }))
    }
}
fn job(r: &SqliteRow) -> Result<Job, Error> {
    Ok(Job {
        id: r.try_get("id")?,
        state: r.try_get::<&str, _>("state")?.parse()?,
        acquired: r.try_get("acquired")?,
        actual_request_id: r.try_get("actual_request_id")?,
        outcome: r.try_get("outcome")?,
        outcome_at: r.try_get("outcome_at")?,
        started_at: r.try_get("started_at")?,
        metadata: NewJob {
            scale_set_id: r.try_get("scale_set_id")?,
            request_id: r.try_get("request_id")?,
            github_job_id: r.try_get("github_job_id")?,
            workflow_run_id: r.try_get("workflow_run_id")?,
            repo: r.try_get("repo")?,
            name: r.try_get("name")?,
            class: r.try_get("class")?,
            reserved_cpu: r.try_get("reserved_cpu")?,
            reserved_memory: unsigned(r, "reserved_memory")?,
        },
    })
}
fn unsigned(r: &SqliteRow, key: &str) -> Result<u64, Error> {
    u64::try_from(r.try_get::<i64, _>(key)?).map_err(|_| Error::Corrupt)
}
