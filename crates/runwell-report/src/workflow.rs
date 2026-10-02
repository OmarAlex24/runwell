//! Resolve observed workflow jobs without evaluating GitHub expressions.
use crate::Error;
use runwell_trace::TraceJob;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

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

/// Map a complete, unambiguous observed graph, including named matrix children.
/// Dynamic matrix values come from observed display names, never evaluation.
pub fn apply_workflow(jobs: &mut [TraceJob], source: &str) -> Result<bool, Error> {
    let workflow: Workflow = serde_yaml::from_str(source)?;
    let unique: BTreeSet<_> = jobs.iter().map(|j| &j.job_name).collect();
    if unique.len() != jobs.len() || workflow.jobs.values().any(|j| j.uses.is_some()) {
        return Ok(false);
    }
    let mut observed: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, job) in jobs.iter().enumerate() {
        let matches: Vec<_> = workflow
            .jobs
            .iter()
            .filter(|(id, definition)| matches_job(id, definition, job))
            .collect();
        let [(id, _)] = matches.as_slice() else {
            return Ok(false);
        };
        observed.entry(id.as_str()).or_default().push(i);
    }
    if observed.len() != workflow.jobs.len()
        || workflow.jobs.iter().any(|(id, j)| {
            (j.strategy.is_none() && observed[id.as_str()].len() != 1)
                || j.needs.names().iter().any(|n| !observed.contains_key(n))
        })
    {
        return Ok(false);
    }
    // Validate all metadata before changing any trace record.
    let mut limits = BTreeMap::new();
    for (id, j) in &workflow.jobs {
        let limit = j.strategy.as_ref().and_then(|v| v.get("max-parallel"));
        let limit = match limit {
            None => None,
            Some(v) => match v
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|&n| n > 0)
            {
                Some(n) => Some(n),
                None => return Ok(false),
            },
        };
        limits.insert(id.as_str(), limit);
    }
    for (id, definition) in &workflow.jobs {
        let children = &observed[id.as_str()];
        let parents: Vec<_> = definition
            .needs
            .names()
            .iter()
            .flat_map(|name| observed[name].iter().copied())
            .collect();
        let parent_end = parents
            .iter()
            .filter_map(|&i| jobs[i].completed_at)
            .chain(jobs.iter().filter_map(|j| j.run_created_at))
            .max();
        let delay = children
            .iter()
            .filter_map(|&i| {
                Some(
                    (jobs[i].created_at?.as_millisecond() - parent_end?.as_millisecond()).max(0)
                        as f64
                        / 1000.0,
                )
            })
            .fold(f64::INFINITY, f64::min);
        let names: Vec<_> = parents.iter().map(|&i| jobs[i].job_name.clone()).collect();
        for &i in children {
            jobs[i].workflow_job_id = Some(id.clone());
            jobs[i].needs = Some(names.clone());
            jobs[i].max_parallel = limits[id.as_str()];
            jobs[i].timeout_minutes = definition
                .timeout
                .as_ref()
                .and_then(serde_yaml::Value::as_f64);
            jobs[i].dispatch_delay_seconds = Some(if delay.is_finite() { delay } else { 0.0 });
        }
    }
    Ok(true)
}

fn matches_job(id: &str, definition: &YamlJob, job: &TraceJob) -> bool {
    let name = definition.name.as_deref().unwrap_or(id);
    if definition.strategy.is_none() {
        return !name.contains("${{") && name == job.job_name;
    }
    if job.conclusion.as_deref() == Some("skipped") && (job.job_name == id || job.job_name == name)
    {
        return true;
    }
    if !name.contains("${{") {
        // GitHub appends matrix values to a static/default display name.
        return job.job_name.starts_with(&format!("{name} (")) && job.job_name.ends_with(')');
    }
    let mut rest = name;
    let mut pattern = String::from("^");
    while let Some((prefix, expression)) = rest.split_once("${{") {
        let Some((expression, suffix)) = expression.split_once("}}") else {
            return false;
        };
        if !expression.trim().starts_with("matrix.") {
            return false;
        }
        pattern.push_str(&regex::escape(prefix));
        pattern.push_str(".+");
        rest = suffix;
    }
    pattern.push_str(&regex::escape(rest));
    pattern.push('$');
    regex::Regex::new(&pattern).is_ok_and(|re| re.is_match(&job.job_name))
}
