//! Trace matching, observed percentiles, and dependency critical paths.
use crate::{context::Job, model::Savings};
use runwell_trace::{TraceJob, TraceStep};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Timing {
    pub samples: usize,
    pub work_p50: f64,
    pub work_p90: f64,
    pub queue_p50: f64,
    pub queue_p90: f64,
    pub failure_rate: f64,
    pub timeout_rate: f64,
    pub critical_samples: usize,
    pub critical_hop_p50: f64,
    pub critical_hop_p90: f64,
}
impl Timing {
    pub fn hop(&self) -> Savings {
        Savings {
            p50: self.critical_hop_p50,
            p90: self.critical_hop_p90,
        }
    }
}
pub(crate) struct Trace {
    pub rows: Vec<TraceJob>,
    critical: BTreeSet<usize>,
}
impl Trace {
    pub fn new(rows: Vec<TraceJob>) -> Self {
        let mut groups: BTreeMap<_, Vec<usize>> = BTreeMap::new();
        for (i, r) in rows.iter().enumerate().filter(|(_, r)| {
            r.run_conclusion.as_deref() == Some("success")
                && r.run_attempt == 1
                && r.event.as_deref() == Some("pull_request")
        }) {
            groups
                .entry((&r.repo, r.run_id, r.run_attempt))
                .or_default()
                .push(i);
        }
        let mut critical = BTreeSet::new();
        for indices in groups.values() {
            let mut current = indices
                .iter()
                .copied()
                .filter(|i| rows[*i].completed_at.is_some())
                .max_by_key(|i| rows[*i].completed_at);
            let mut visited = BTreeSet::new();
            while let Some(i) = current {
                if !visited.insert(i) {
                    break;
                }
                critical.insert(i);
                let row = &rows[i];
                current = if let Some(needs) = &row.needs {
                    indices
                        .iter()
                        .copied()
                        .filter(|j| {
                            *j != i
                                && needs.iter().any(|n| {
                                    rows[*j].job_name == *n
                                        || rows[*j].workflow_job_id.as_ref() == Some(n)
                                })
                        })
                        .max_by_key(|j| rows[*j].completed_at)
                } else {
                    indices
                        .iter()
                        .copied()
                        .filter(|j| {
                            *j != i
                                && rows[*j].completed_at.zip(row.created_at).is_some_and(
                                    |(end, created)| end.as_second() <= created.as_second() + 3,
                                )
                                && rows[*j].started_at < row.started_at
                        })
                        .max_by_key(|j| rows[*j].completed_at)
                };
            }
        }
        Self { rows, critical }
    }
    pub fn matches(&self, job: &Job<'_>) -> Vec<usize> {
        let name = job.node.str("name");
        let workflow = job.workflow.root.str("name");
        let candidates: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                let workflow_matches = r.workflow.as_deref().is_some_and(|w| {
                    w == workflow || w == job.workflow.file || job.workflow.file.ends_with(w)
                });
                let name_matches = r.workflow_job_id.as_deref().map_or_else(
                    || {
                        r.job_name == job.id
                            || (!name.is_empty() && r.job_name == name)
                            || ((!name.is_empty() && name.contains("${{"))
                                && r.job_name
                                    .starts_with(name.split("${{").next().unwrap_or("").trim_end()))
                            || r.job_name.starts_with(&format!(
                                "{} (",
                                if name.is_empty() { job.id } else { name }
                            ))
                    },
                    |id| id == job.id,
                );
                workflow_matches
                    && name_matches
                    && (!job.workflow.triggered("pull_request")
                        || r.event.as_deref() == Some("pull_request"))
            })
            .map(|(i, _)| i)
            .collect();
        let repos: BTreeSet<_> = candidates.iter().map(|i| &self.rows[*i].repo).collect();
        if repos.len() > 1 {
            return vec![];
        }
        let duplicate_name = !name.is_empty()
            && job
                .workflow
                .jobs()
                .iter()
                .filter(|j| j.node.str("name") == name)
                .count()
                > 1;
        candidates
            .into_iter()
            .filter(|i| !duplicate_name || self.rows[*i].workflow_job_id.as_deref() == Some(job.id))
            .collect()
    }
    pub fn timing(&self, job: &Job<'_>) -> Option<Timing> {
        let indices = self.matches(job);
        let executed: Vec<_> = indices
            .iter()
            .copied()
            .filter(|i| self.rows[*i].started_at.is_some())
            .collect();
        let successful: Vec<_> = executed
            .iter()
            .copied()
            .filter(|i| {
                self.rows[*i].conclusion.as_deref() == Some("success")
                    && self.rows[*i].run_attempt == 1
            })
            .collect();
        let work: Vec<_> = successful
            .iter()
            .filter_map(|i| duration(&self.rows[*i]))
            .collect();
        if work.is_empty() {
            return None;
        }
        let queue: Vec<_> = successful
            .iter()
            .filter_map(|i| {
                self.rows[*i]
                    .created_at
                    .zip(self.rows[*i].started_at)
                    .map(|(a, b)| minutes(a.as_second(), b.as_second()))
            })
            .collect();
        let critical_hops: Vec<_> = successful
            .iter()
            .filter(|i| self.critical.contains(i))
            .filter_map(|i| {
                let r = &self.rows[*i];
                r.created_at
                    .zip(r.completed_at)
                    .map(|(a, b)| minutes(a.as_second(), b.as_second()))
            })
            .collect();
        let denom = executed.len().max(1) as f64;
        Some(Timing {
            critical_hop_p50: percentile(&critical_hops, 0.5),
            critical_hop_p90: percentile(&critical_hops, 0.9),
            samples: work.len(),
            work_p50: percentile(&work, 0.5),
            work_p90: percentile(&work, 0.9),
            queue_p50: percentile(&queue, 0.5),
            queue_p90: percentile(&queue, 0.9),
            failure_rate: executed
                .iter()
                .filter(|i| self.rows[**i].conclusion.as_deref() == Some("failure"))
                .count() as f64
                / denom,
            timeout_rate: executed.iter().filter(|i| timeout(&self.rows[**i])).count() as f64
                / denom,
            critical_samples: successful
                .iter()
                .filter(|i| self.critical.contains(i))
                .count(),
        })
    }
    pub fn step_minutes(&self, job: &Job<'_>, name: &str) -> Vec<f64> {
        self.step_observations(job, name, true)
    }
    pub fn waiting_minutes(&self, job: &Job<'_>, name: &str) -> Vec<f64> {
        self.step_observations(job, name, false)
    }
    fn step_observations(&self, job: &Job<'_>, name: &str, successful: bool) -> Vec<f64> {
        self.matches(job)
            .iter()
            .filter(|i| {
                !successful
                    || (self.rows[**i].conclusion.as_deref() == Some("success")
                        && self.rows[**i].run_attempt == 1)
            })
            .flat_map(|i| {
                self.rows[*i]
                    .steps
                    .iter()
                    .filter(|s| s.name == name)
                    .filter_map(step_duration)
            })
            .collect()
    }
}
fn timeout(row: &TraceJob) -> bool {
    row.conclusion.as_deref() == Some("timed_out")
        || (matches!(row.conclusion.as_deref(), Some("cancelled" | "failure"))
            && row
                .annotations
                .iter()
                .chain(row.log_excerpt.iter())
                .any(|s| {
                    let s = s.to_lowercase();
                    s.contains("maximum execution time")
                        || s.contains("job timed out")
                        || s.contains("job exceeded")
                }))
}
pub(crate) fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    v[((v.len() as f64 * p).ceil() as usize)
        .saturating_sub(1)
        .min(v.len() - 1)]
}
fn minutes(a: i64, b: i64) -> f64 {
    (b - a).max(0) as f64 / 60.0
}
fn duration(row: &TraceJob) -> Option<f64> {
    row.started_at
        .zip(row.completed_at)
        .map(|(a, b)| minutes(a.as_second(), b.as_second()))
}
fn step_duration(row: &TraceStep) -> Option<f64> {
    row.started_at
        .zip(row.completed_at)
        .map(|(a, b)| minutes(a.as_second(), b.as_second()))
}
