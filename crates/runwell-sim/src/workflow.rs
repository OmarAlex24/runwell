//! Import workflow dependencies without executing expressions or workflow steps.
use crate::Error;
use runwell_trace::TraceJob;
use serde_yaml_ng::Value;
use std::collections::BTreeMap;

/// A parsed, repository-scoped workflow graph. No file I/O is performed here.
#[derive(Debug, Clone)]
pub struct WorkflowNeeds {
    repo: String,
    name: String,
    since: Option<jiff::Timestamp>,
    jobs: BTreeMap<String, Definition>,
    cancellation: Cancellation,
}
#[derive(Debug, Clone)]
struct Definition {
    name: String,
    needs: Vec<String>,
    max_parallel: Option<usize>,
}
#[derive(Debug, Clone, Copy)]
enum Cancellation {
    Disabled,
    PullRequest,
    Branch,
}

/// Counts describing which observations a workflow file actually covers.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ImportSummary {
    /// Runs whose full graph could be resolved.
    pub graph_runs: usize,
    /// Older or structurally incompatible runs retaining timestamp inference.
    pub inferred_runs: usize,
    /// Job records assigned explicit workflow dependencies.
    pub graph_jobs: usize,
    /// Runs assigned a branch-based cancellation group.
    pub cancellation_runs: usize,
}
impl WorkflowNeeds {
    /// Workflow display name used to scope calibration without filtering load.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Parse YAML, preserving job IDs, display names, matrix limits and the common
    /// boolean / pull-request-only cancellation forms. Unknown expressions fail.
    pub fn parse(repo: String, yaml: &str, since: Option<jiff::Timestamp>) -> Result<Self, Error> {
        let document: Value = serde_yaml_ng::from_str(yaml)
            .map_err(|_| Error::Invalid("invalid workflow YAML".into()))?;
        let name = document["name"]
            .as_str()
            .ok_or_else(|| Error::Invalid("workflow needs a name".into()))?
            .to_owned();
        let mapping = document["jobs"]
            .as_mapping()
            .ok_or_else(|| Error::Invalid("workflow needs a jobs map".into()))?;
        let mut jobs = BTreeMap::new();
        for (id, value) in mapping {
            let id = id
                .as_str()
                .ok_or_else(|| Error::Invalid("workflow job ID must be a string".into()))?;
            let needs = match &value["needs"] {
                Value::Null => Vec::new(),
                Value::String(s) => vec![s.clone()],
                Value::Sequence(xs) => xs
                    .iter()
                    .map(|x| {
                        x.as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| Error::Invalid("needs must contain job IDs".into()))
                    })
                    .collect::<Result<_, _>>()?,
                _ => return Err(Error::Invalid("needs must be a string or list".into())),
            };
            let max_parallel = match &value["strategy"]["max-parallel"] {
                Value::Null => None,
                v => Some(
                    v.as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .filter(|&n| n > 0)
                        .ok_or_else(|| {
                            Error::Invalid("max-parallel must be a positive integer".into())
                        })?,
                ),
            };
            jobs.insert(
                id.to_owned(),
                Definition {
                    name: value["name"].as_str().unwrap_or(id).into(),
                    needs,
                    max_parallel,
                },
            );
        }
        let cancel = &document["concurrency"]["cancel-in-progress"];
        let cancellation = if cancel.is_null() || cancel.as_bool() == Some(false) {
            Cancellation::Disabled
        } else if cancel.as_bool() == Some(true) {
            Cancellation::Branch
        } else if cancel
            .as_str()
            .is_some_and(|s| s.replace(' ', "") == "${{github.event_name=='pull_request'}}")
        {
            Cancellation::PullRequest
        } else {
            return Err(Error::Invalid(
                "unsupported cancel-in-progress expression".into(),
            ));
        };
        if !matches!(cancellation, Cancellation::Disabled) {
            let group = document["concurrency"]["group"].as_str().unwrap_or("");
            // A group with additional dynamic dimensions needs explicit trace
            // metadata. Never silently reinterpret an arbitrary workflow expression.
            let mut rest = group;
            let mut branch_scoped = false;
            while let Some((_, expression)) = rest.split_once("${{") {
                let (expression, suffix) = expression
                    .split_once("}}")
                    .ok_or_else(|| Error::Invalid("invalid concurrency group expression".into()))?;
                let expression = expression.replace(' ', "");
                match expression.as_str() {
                    "github.workflow" => {},
                    "github.ref" | "github.head_ref" | "github.event.pull_request.number||github.ref" => branch_scoped = true,
                    "github.event.pull_request.number||github.run_id" if matches!(cancellation, Cancellation::PullRequest) => branch_scoped = true,
                    _ => return Err(Error::Invalid("unsupported concurrency group expression; supply cancel_group in the trace".into())),
                }
                rest = suffix;
            }
            if !branch_scoped {
                return Err(Error::Invalid(
                    "concurrency group must identify a PR or branch".into(),
                ));
            }
        }
        for d in jobs.values() {
            if d.needs.iter().any(|p| !jobs.contains_key(p)) {
                return Err(Error::Invalid(
                    "workflow references an unknown job ID".into(),
                ));
            }
        }
        Ok(Self {
            repo,
            name,
            since,
            jobs,
            cancellation,
        })
    }
    /// Apply only matching workflow runs. A validity cutoff affects the DAG, not
    /// the stable workflow cancellation rule. Missing-version jobs retain inference.
    pub fn apply(&self, trace: &mut [TraceJob]) -> Result<ImportSummary, Error> {
        let mut groups = BTreeMap::<(u64, u32), Vec<usize>>::new();
        for (i, j) in trace
            .iter()
            .enumerate()
            .filter(|(_, j)| j.repo == self.repo && j.workflow.as_deref() == Some(&self.name))
        {
            groups.entry((j.run_id, j.run_attempt)).or_default().push(i);
        }
        let mut summary = ImportSummary::default();
        for indices in groups.values() {
            let first = &trace[indices[0]];
            let cancel = match self.cancellation {
                Cancellation::Disabled => false,
                Cancellation::PullRequest => first.event.as_deref() == Some("pull_request"),
                Cancellation::Branch => true,
            };
            let cancel_group = if cancel {
                first.branch.as_ref().map(|b| {
                    format!(
                        "{}:{}:{}:{}",
                        self.repo,
                        self.name,
                        if first.event.as_deref() == Some("pull_request") {
                            "pr"
                        } else {
                            "ref"
                        },
                        b
                    )
                })
            } else {
                None
            };
            let valid_date = self
                .since
                .is_none_or(|s| first.run_created_at.is_some_and(|t| t >= s));
            let arrival = first.run_created_at;
            if let Some(group) = cancel_group {
                summary.cancellation_runs += 1;
                for &i in indices {
                    trace[i].cancel_group = Some(group.clone());
                }
            }
            let mut matched: BTreeMap<String, Vec<usize>> = BTreeMap::new();
            for &i in indices {
                if let Some((id, _)) = self.jobs.iter().find(|(id, d)| {
                    trace[i].job_name == **id || template_matches(&d.name, &trace[i].job_name)
                }) {
                    matched.entry(id.clone()).or_default().push(i);
                }
            }
            for (id, children) in &matched {
                for &i in children {
                    trace[i].workflow_job_id = Some(id.clone());
                    trace[i].max_parallel = self.jobs[id].max_parallel;
                }
            }
            let covered = matched.values().map(Vec::len).sum::<usize>() == indices.len();
            let complete = matched
                .keys()
                .all(|id| self.jobs[id].needs.iter().all(|p| matched.contains_key(p)));
            if !valid_date || !covered || !complete {
                summary.inferred_runs += 1;
                continue;
            }
            for (id, children) in &matched {
                let d = &self.jobs[id];
                let parents: Vec<_> = d
                    .needs
                    .iter()
                    .flat_map(|p| matched[p].iter().copied())
                    .collect();
                let parent_end = parents
                    .iter()
                    .filter_map(|&p| trace[p].completed_at)
                    .chain(arrival)
                    .max();
                // Matrix throttling can postpone later children. Use the earliest
                // child creation gap so max-parallel is not paid a second time.
                let gap = children
                    .iter()
                    .filter_map(|&i| {
                        Some(
                            (trace[i].created_at?.as_millisecond() - parent_end?.as_millisecond())
                                .max(0) as f64
                                / 1000.0,
                        )
                    })
                    .fold(f64::INFINITY, f64::min);
                let names: Vec<_> = parents.iter().map(|&i| trace[i].job_name.clone()).collect();
                for &i in children {
                    trace[i].needs = Some(names.clone());
                    trace[i].workflow_job_id = Some(id.clone());
                    trace[i].max_parallel = d.max_parallel;
                    trace[i].dispatch_delay_seconds = Some(if gap.is_finite() { gap } else { 0.0 });
                    summary.graph_jobs += 1;
                }
            }
            summary.graph_runs += 1;
        }
        Ok(summary)
    }
}
fn template_matches(template: &str, value: &str) -> bool {
    let Some((prefix, rest)) = template.split_once("${{") else {
        return template == value;
    };
    let Some((_, suffix)) = rest.split_once("}}") else {
        return false;
    };
    value
        .strip_prefix(prefix)
        .is_some_and(|v| v.len() >= suffix.len() && v.ends_with(suffix))
}
