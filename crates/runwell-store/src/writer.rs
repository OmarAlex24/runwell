use crate::{Error, Execution, JobMeasurement, NewJob, Runner, State};
use sqlx::{Acquire, Sqlite, pool::PoolConnection};
use tokio::sync::{mpsc, oneshot};
pub(crate) struct Command {
    pub mutation: Mutation,
    pub reply: oneshot::Sender<Result<i64, Error>>,
}
pub(crate) enum Mutation {
    Queue(NewJob),
    Transition(i64, State),
    Intent(Runner),
    Acquired(i64),
    Retarget(i64, String),
    Registered(i64, i64),
    Bind(i64, Execution, Option<String>),
    Deleted(i64),
    Exited(i64, Option<i32>),
    Cleaned(i64),
    Measure(JobMeasurement),
    Ack(i64, i64),
}
pub(crate) async fn run(mut connection: PoolConnection<Sqlite>, mut rx: mpsc::Receiver<Command>) {
    while let Some(command) = rx.recv().await {
        let result = apply(&mut connection, command.mutation).await;
        let _ = command.reply.send(result);
    }
}
async fn apply(c: &mut PoolConnection<Sqlite>, mutation: Mutation) -> Result<i64, Error> {
    let changed = match mutation {
        Mutation::Queue(j) => {
            let memory = i64::try_from(j.reserved_memory).map_err(|_| Error::Corrupt)?;
            return Ok(sqlx::query_scalar("INSERT INTO jobs (scale_set_id,request_id,github_job_id,workflow_run_id,repo,name,class,reserved_cpu,reserved_memory) VALUES (?,?,?,?,?,?,?,?,?) ON CONFLICT(scale_set_id,request_id) DO UPDATE SET id=id RETURNING id")
                .bind(j.scale_set_id).bind(j.request_id).bind(j.github_job_id).bind(j.workflow_run_id)
                .bind(j.repo).bind(j.name).bind(j.class).bind(j.reserved_cpu).bind(memory).fetch_one(&mut **c).await?);
        }
        Mutation::Transition(id, state) => sqlx::query("UPDATE jobs SET state=?,updated_at=unixepoch(),started_at=CASE WHEN ?='running' THEN COALESCE(started_at,unixepoch()) ELSE started_at END,finished_at=CASE WHEN ? THEN COALESCE(finished_at,unixepoch()) ELSE finished_at END WHERE id=?")
            .bind(state.as_str()).bind(state.as_str()).bind(state.terminal()).bind(id).execute(&mut **c).await?.rows_affected(),
        Mutation::Intent(r) => {
            let state: String = sqlx::query_scalar("SELECT state FROM jobs WHERE id=?").bind(r.job_id).fetch_one(&mut **c).await?;
            if state != "admitted" { return Err(Error::Transition); }
            sqlx::query("INSERT INTO runners(job_id,name,dir,unit,template_version) VALUES(?,?,?,?,?) ON CONFLICT(job_id) DO NOTHING")
                .bind(r.job_id).bind(&r.name).bind(&r.dir).bind(&r.unit).bind(&r.template_version).execute(&mut **c).await?;
            let same: bool = sqlx::query_scalar("SELECT name=? AND dir=? AND unit=? AND template_version=? FROM runners WHERE job_id=?")
                .bind(r.name).bind(r.dir).bind(r.unit).bind(r.template_version).bind(r.job_id).fetch_one(&mut **c).await?;
            u64::from(same)
        }
        Mutation::Retarget(id, version) => sqlx::query("UPDATE runners SET template_version=? WHERE job_id=? AND agent_id IS NULL AND EXISTS(SELECT 1 FROM jobs WHERE id=? AND state='admitted')")
            .bind(version).bind(id).bind(id).execute(&mut **c).await?.rows_affected(),
        Mutation::Acquired(id) => sqlx::query("UPDATE jobs SET acquired=1 WHERE id=? AND state='admitted'").bind(id).execute(&mut **c).await?.rows_affected(),
        Mutation::Registered(id, agent) => {
            let mut tx = c.begin().await?;
            if agent <= 0 { return Err(Error::Corrupt); }
            let n = sqlx::query("UPDATE runners SET agent_id=? WHERE job_id=? AND (agent_id IS NULL OR agent_id=?)")
                .bind(agent).bind(id).bind(agent).execute(&mut *tx).await?.rows_affected();
            if n != 1 { return Err(Error::Transition); }
            sqlx::query("UPDATE jobs SET state='runner_created',updated_at=unixepoch() WHERE id=? AND acquired=1")
                .bind(id).execute(&mut *tx).await?;
            let state: String = sqlx::query_scalar("SELECT state FROM jobs WHERE id=?").bind(id).fetch_one(&mut *tx).await?;
            if state != "runner_created" { return Err(Error::Transition); }
            tx.commit().await?;
            1
        }
        Mutation::Bind(id, execution, outcome) => sqlx::query("UPDATE jobs SET github_job_id=COALESCE(NULLIF(?,''),github_job_id),workflow_run_id=COALESCE(NULLIF(?,0),workflow_run_id),repo=COALESCE(NULLIF(?,''),repo),name=COALESCE(NULLIF(?,''),name),actual_request_id=?,outcome_at=CASE WHEN ? IS NOT NULL THEN COALESCE(outcome_at,unixepoch()) ELSE outcome_at END,outcome=COALESCE(?,outcome),updated_at=unixepoch() WHERE id=? AND (actual_request_id IS NULL OR actual_request_id=?)")
            .bind(execution.github_job_id).bind(execution.workflow_run_id).bind(execution.repo).bind(execution.name).bind(execution.request_id).bind(&outcome).bind(outcome).bind(id).bind(execution.request_id).execute(&mut **c).await?.rows_affected(),
        Mutation::Exited(id, code) => sqlx::query("UPDATE runners SET exit_code=COALESCE(exit_code,?) WHERE job_id=?")
            .bind(code).bind(id).execute(&mut **c).await?.rows_affected(),
        Mutation::Deleted(id) => sqlx::query("UPDATE runners SET remote_deleted=1 WHERE job_id=?").bind(id).execute(&mut **c).await?.rows_affected(),
        Mutation::Cleaned(id) => sqlx::query("UPDATE runners SET cleaned=1 WHERE job_id=? AND remote_deleted=1 AND EXISTS(SELECT 1 FROM jobs WHERE id=? AND state IN ('completed','failed','orphaned')) AND EXISTS(SELECT 1 FROM measurements WHERE job_id=runners.job_id)")
            .bind(id).bind(id).execute(&mut **c).await?.rows_affected(),
        Mutation::Measure(m) => {
            let psi = serde_json::to_string(&m.psi).map_err(|_| Error::Corrupt)?;
            sqlx::query("INSERT INTO measurements(job_id,cpu_usec,memory_current,memory_peak,io_read_bytes,io_write_bytes,psi,oom_kills,duration_ms,exit_code,infra_signal) VALUES(?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(job_id) DO NOTHING")
                .bind(m.job_id).bind(number(m.cpu_usec)?).bind(number(m.memory_current)?).bind(number(m.memory_peak)?)
                .bind(number(m.io_read_bytes)?).bind(number(m.io_write_bytes)?).bind(psi).bind(number(m.oom_kills)?)
                .bind(number(m.duration_ms)?).bind(m.exit_code).bind(m.infra_signal || m.oom_kills > 0).execute(&mut **c).await?;
            1
        }
        Mutation::Ack(set, id) => sqlx::query("INSERT INTO messages VALUES(?,?) ON CONFLICT(scale_set_id) DO UPDATE SET last_acked_id=excluded.last_acked_id")
            .bind(set).bind(id).execute(&mut **c).await?.rows_affected(),
    };
    if changed == 0 {
        Err(Error::Transition)
    } else {
        Ok(0)
    }
}
fn number(value: u64) -> Result<i64, Error> {
    i64::try_from(value).map_err(|_| Error::Corrupt)
}
