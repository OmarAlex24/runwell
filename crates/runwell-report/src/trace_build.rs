//! GitHub JSON and workflow YAML to portable trace records.
use crate::Error;
use jiff::Timestamp;
use runwell_trace::{SCHEMA_VERSION, TraceJob, TraceStep};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct Run {
    pub id: u64,
    #[serde(default)]
    pub name: Option<String>,
    pub created_at: Timestamp,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub event: Option<String>,
    #[serde(default)]
    pub head_branch: Option<String>,
    #[serde(default)]
    pub head_sha: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default = "attempt_one")]
    pub run_attempt: u32,
}
fn attempt_one() -> u32 {
    1
}

pub fn job(repo: &str, run: &Run, value: Value) -> Result<TraceJob, Error> {
    #[derive(Deserialize)]
    struct Job {
        id: u64,
        name: String,
        #[serde(default = "attempt_one")]
        run_attempt: u32,
        #[serde(default)]
        labels: Vec<String>,
        #[serde(default)]
        runner_name: Option<String>,
        #[serde(default)]
        created_at: Option<Timestamp>,
        #[serde(default)]
        started_at: Option<Timestamp>,
        #[serde(default)]
        completed_at: Option<Timestamp>,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        conclusion: Option<String>,
        #[serde(default)]
        steps: Vec<TraceStep>,
    }
    let j: Job = serde_json::from_value(value)?;
    Ok(TraceJob {
        schema_version: SCHEMA_VERSION,
        repo: repo.into(),
        workflow: run.name.clone(),
        run_id: run.id,
        run_attempt: j.run_attempt,
        event: run.event.clone(),
        branch: run.head_branch.clone(),
        head_sha: run.head_sha.clone(),
        run_created_at: Some(run.created_at),
        run_conclusion: run.conclusion.clone(),
        job_id: Some(j.id),
        job_name: j.name,
        labels: j.labels,
        runner_name: j.runner_name,
        created_at: j.created_at,
        started_at: j.started_at,
        completed_at: j.completed_at,
        status: j.status,
        conclusion: j.conclusion,
        needs: None,
        steps: j.steps,
        annotations: Vec::new(),
        log_excerpt: None,
        timeout_minutes: None,
        workflow_job_id: None,
        max_parallel: None,
        dispatch_delay_seconds: None,
        cancel_group: None,
    })
}

pub use crate::workflow::apply_workflow;
