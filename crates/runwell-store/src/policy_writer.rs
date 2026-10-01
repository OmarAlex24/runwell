use crate::{Error, policy::PolicyMutation};
use sqlx::{Acquire, Sqlite, pool::PoolConnection};

pub(crate) async fn apply(
    c: &mut PoolConnection<Sqlite>,
    mutation: PolicyMutation,
) -> Result<i64, Error> {
    match mutation {
        PolicyMutation::Context(id, context) => {
            let json = serde_json::to_string(&context).map_err(|_| Error::Corrupt)?;
            sqlx::query("INSERT INTO scheduling_context VALUES(?,?) ON CONFLICT(job_id) DO UPDATE SET payload=excluded.payload")
                .bind(id).bind(json).execute(&mut **c).await?;
        }
        PolicyMutation::Propose(p, state) => {
            let json = serde_json::to_string(&state).map_err(|_| Error::Corrupt)?;
            let mut tx = c.begin().await?;
            sqlx::query(
                "INSERT INTO placements(job_id,attempt,node_id,assigned_at,lost) VALUES(?,?,?,?,0)",
            )
            .bind(p.job_id)
            .bind(p.attempt)
            .bind(p.node_id)
            .bind(p.assigned_at)
            .execute(&mut *tx)
            .await?;
            sqlx::query("INSERT INTO dispatch_proposals(job_id,fairness) VALUES(?,?)")
                .bind(p.job_id)
                .bind(json)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }
        PolicyMutation::Accept(id) => {
            let mut tx = c.begin().await?;
            let next: Option<String> = sqlx::query_scalar(
                "SELECT fairness FROM dispatch_proposals WHERE job_id=? AND accepted=0",
            )
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(next) = next else {
                return Ok(0);
            };
            sqlx::query("UPDATE scheduler_fairness SET payload=? WHERE singleton=1")
                .bind(next)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE dispatch_proposals SET accepted=1 WHERE job_id=?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            return Ok(1);
        }
        PolicyMutation::Reject(id) => {
            let mut tx = c.begin().await?;
            let removed =
                sqlx::query("DELETE FROM dispatch_proposals WHERE job_id=? AND accepted=0")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?
                    .rows_affected();
            if removed > 0 {
                sqlx::query("DELETE FROM placements WHERE job_id=?")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
            }
            tx.commit().await?;
        }
        PolicyMutation::Abandon(id) => {
            sqlx::query("DELETE FROM dispatch_proposals WHERE job_id=? AND accepted=0")
                .bind(id)
                .execute(&mut **c)
                .await?;
        }
        PolicyMutation::Count(id) => {
            return Ok(sqlx::query(
                "UPDATE controller_observations SET counted=1 WHERE job_id=? AND counted=0",
            )
            .bind(id)
            .execute(&mut **c)
            .await?
            .rows_affected() as i64);
        }
        PolicyMutation::Observe(id, reason, completed, at) => {
            sqlx::query("INSERT INTO controller_observations(job_id,reason,completed,observed_at) VALUES(?,?,?,?) ON CONFLICT(job_id) DO UPDATE SET processed=CASE WHEN excluded.completed>controller_observations.completed OR (controller_observations.reason IS NULL AND excluded.reason IS NOT NULL) THEN 0 ELSE controller_observations.processed END,reason=COALESCE(controller_observations.reason,excluded.reason),completed=MAX(controller_observations.completed,excluded.completed)")
                .bind(id).bind(reason).bind(completed).bind(at).execute(&mut **c).await?;
        }
        PolicyMutation::Processed(id, infra, processed) => {
            sqlx::query("UPDATE controller_observations SET processed=?,infra=? WHERE job_id=?")
                .bind(processed)
                .bind(infra)
                .bind(id)
                .execute(&mut **c)
                .await?;
        }
        PolicyMutation::Expiry(version, at) => {
            sqlx::query("INSERT INTO template_expiry VALUES(?,?) ON CONFLICT(version) DO UPDATE SET expires_at=MIN(expires_at,excluded.expires_at)")
                .bind(version).bind(at).execute(&mut **c).await?;
        }
    }
    Ok(0)
}
