use crate::{Error, network::NetworkMutation};
use sqlx::{Acquire, Sqlite, pool::PoolConnection};

pub(crate) async fn apply(
    c: &mut PoolConnection<Sqlite>,
    mutation: NetworkMutation,
) -> Result<i64, Error> {
    let n = match mutation {
        NetworkMutation::LostBefore(before) => sqlx::query("UPDATE node_health SET lost=1 WHERE last_seen<=?").bind(before).execute(&mut **c).await?.rows_affected(),
        NetworkMutation::Release(version, checked) => sqlx::query("INSERT INTO controller_release VALUES(1,?,?) ON CONFLICT(singleton) DO UPDATE SET version=excluded.version,checked_at=excluded.checked_at").bind(version).bind(checked).execute(&mut **c).await?.rows_affected(),
        NetworkMutation::Place(p) => {
            sqlx::query("INSERT INTO placements(job_id,attempt,node_id,assigned_at) VALUES(?,?,?,?) ON CONFLICT(job_id) DO NOTHING")
                .bind(p.job_id).bind(p.attempt).bind(&p.node_id).bind(p.assigned_at).execute(&mut **c).await?;
            let same: bool = sqlx::query_scalar("SELECT attempt=? AND node_id=? FROM placements WHERE job_id=?")
                .bind(p.attempt).bind(p.node_id).bind(p.job_id).fetch_one(&mut **c).await?;
            if !same { return Err(Error::Transition); }
            1
        }
        NetworkMutation::Snapshot(id, sequence, now, payload) => {
            let mut tx = c.begin().await?;
            let n = sqlx::query("INSERT INTO node_snapshots VALUES(?,?,?,?) ON CONFLICT(node_id) DO UPDATE SET sequence=excluded.sequence,received_at=excluded.received_at,payload=excluded.payload WHERE excluded.sequence>node_snapshots.sequence")
                .bind(&id).bind(sequence).bind(now).bind(payload).execute(&mut *tx).await?.rows_affected();
            if n > 0 {
                sqlx::query("INSERT INTO node_health VALUES(?,?,0) ON CONFLICT(node_id) DO UPDATE SET last_seen=excluded.last_seen,lost=0").bind(id).bind(now).execute(&mut *tx).await?;
            }
            tx.commit().await?;
            n
        }
        NetworkMutation::Lease(lease) => return crate::spool_writer::lease(c, *lease).await,
        NetworkMutation::Heartbeat(job, attempt, at) => sqlx::query("UPDATE node_leases SET heartbeat_at_ms=? WHERE job_id=? AND attempt=? AND phase<6 AND started_at_ms<=? AND (heartbeat_at_ms IS NULL OR heartbeat_at_ms<?)")
            .bind(at).bind(job).bind(attempt).bind(at).bind(at).execute(&mut **c).await?.rows_affected(),
        NetworkMutation::ExecutionStarted(job, at) => sqlx::query("UPDATE placements SET execution_started_at=? WHERE job_id=? AND execution_started_at IS NULL")
            .bind(at).bind(job).execute(&mut **c).await?.rows_affected(),
        NetworkMutation::Drain(draining) => sqlx::query("UPDATE node_control SET draining=? WHERE singleton=1").bind(draining).execute(&mut **c).await?.rows_affected(),
        NetworkMutation::Sequence => return Ok(sqlx::query_scalar("UPDATE node_control SET sequence=sequence+1 WHERE singleton=1 RETURNING sequence").fetch_one(&mut **c).await?),
        NetworkMutation::Failure(e) => {
            let mut tx = c.begin().await?;
            sqlx::query("INSERT INTO failure_outbox(job_id,attempt,reason) VALUES(?,?,?) ON CONFLICT DO NOTHING")
                .bind(e.job_id).bind(e.attempt).bind(e.reason).execute(&mut *tx).await?;
            sqlx::query("UPDATE placements SET lost=1 WHERE job_id=? AND attempt=?")
                .bind(e.job_id).bind(e.attempt).execute(&mut *tx).await?;
            tx.commit().await?;
            1
        }
        NetworkMutation::Delivered(job, attempt) => sqlx::query("UPDATE failure_outbox SET delivered=1 WHERE job_id=? AND attempt=?").bind(job).bind(attempt).execute(&mut **c).await?.rows_affected(),
        NetworkMutation::Jit(job) => sqlx::query("INSERT INTO jit_intents VALUES(?) ON CONFLICT DO NOTHING").bind(job).execute(&mut **c).await?.rows_affected(),
    };
    Ok(n as i64)
}
