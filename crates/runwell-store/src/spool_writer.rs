use crate::{Error, NodeLease};
use sqlx::{Sqlite, pool::PoolConnection};

pub(crate) async fn lease(c: &mut PoolConnection<Sqlite>, lease: NodeLease) -> Result<i64, Error> {
    let payload = if lease.phase < 6 {
        Some(serde_json::to_string(&lease).map_err(|_| Error::Corrupt)?)
    } else {
        None
    };
    let measurement = lease
        .measurement
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|_| Error::Corrupt)?;
    let changed = sqlx::query(
        "INSERT INTO node_leases(job_id,attempt,phase,payload,started_at_ms,heartbeat_at_ms,measurement) VALUES(?,?,?,?,?,?,?)
         ON CONFLICT(job_id) DO UPDATE SET phase=excluded.phase,payload=excluded.payload,
         started_at_ms=COALESCE(node_leases.started_at_ms,excluded.started_at_ms),
         heartbeat_at_ms=COALESCE(MAX(node_leases.heartbeat_at_ms,excluded.heartbeat_at_ms),node_leases.heartbeat_at_ms,excluded.heartbeat_at_ms),
         measurement=COALESCE(node_leases.measurement,excluded.measurement)
         WHERE node_leases.attempt=excluded.attempt AND excluded.phase>=node_leases.phase"
    ).bind(lease.job.id).bind(lease.attempt).bind(lease.phase).bind(payload)
        .bind(lease.started_at_ms).bind(lease.heartbeat_at_ms).bind(measurement)
        .execute(&mut **c).await?.rows_affected();
    if changed == 0 {
        return Err(Error::Transition);
    }
    Ok(changed as i64)
}
