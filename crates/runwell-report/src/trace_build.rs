//! GitHub JSON and workflow YAML to portable trace records.
use crate::Error;
use jiff::Timestamp;
use runwell_trace::{SCHEMA_VERSION, TraceJob, TraceStep};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

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

#[derive(Debug, Deserialize)]
struct Workflow {
    jobs: BTreeMap<String, YamlJob>,
}
#[derive(Debug, Deserialize)]
struct YamlJob {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    needs: Needs,
    #[serde(default)]
    strategy: Option<serde_yaml::Value>,
    #[serde(default)]
    uses: Option<String>,
    #[serde(default, rename = "timeout-minutes")]
    timeout: Option<serde_yaml::Value>,
}
#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
enum Needs {
    One(String),
    Many(Vec<String>),
    #[default]
    Missing,
}
impl Needs {
    fn names(&self) -> Vec<&str> {
        match self {
            Self::One(s) => vec![s],
            Self::Many(v) => v.iter().map(String::as_str).collect(),
            Self::Missing => Vec::new(),
        }
    }
}

/// Only map a complete, one-to-one graph. Matrix, expressions, and reusable jobs
/// deliberately leave `needs` unknown so metrics can label timestamp inference.
pub fn apply_workflow(jobs: &mut [TraceJob], source: &str) -> Result<bool, Error> {
    let workflow: Workflow = serde_yaml::from_str(source)?;
    if workflow.jobs.values().any(|j| {
        j.strategy.is_some()
            || j.uses.is_some()
            || j.name.as_ref().is_some_and(|n| n.contains("${{"))
    }) {
        return Ok(false);
    }
    let mut observed = BTreeMap::new();
    for (key, j) in &workflow.jobs {
        let name = j.name.as_deref().unwrap_or(key);
        if jobs.iter().filter(|o| o.job_name == name).count() != 1 {
            return Ok(false);
        }
        if observed.insert(key.as_str(), name).is_some() {
            return Ok(false);
        }
    }
    let unique: BTreeSet<_> = observed.values().collect();
    if unique.len() != jobs.len() || observed.len() != jobs.len() {
        return Ok(false);
    }
    if workflow
        .jobs
        .values()
        .any(|j| j.needs.names().iter().any(|n| !observed.contains_key(n)))
    {
        return Ok(false);
    }
    for (key, j) in &workflow.jobs {
        if let Some(o) = jobs
            .iter_mut()
            .find(|o| o.job_name == *observed[key.as_str()])
        {
            o.workflow_job_id = Some(key.clone());
            o.timeout_minutes = j.timeout.as_ref().and_then(serde_yaml::Value::as_f64);
            o.needs = Some(
                j.needs
                    .names()
                    .iter()
                    .filter_map(|n| observed.get(n).map(|s| s.to_string()))
                    .collect(),
            );
        }
    }
    Ok(true)
}
