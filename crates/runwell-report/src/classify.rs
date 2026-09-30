//! Evidence-based classification. Unknown cancellations remain visible.
use crate::{
    Error,
    metrics::{duration, executed, percent},
    model::{FailureDetail, Failures},
};
use regex::{Regex, RegexSet};
use runwell_trace::TraceJob;
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
pub struct RulesFile {
    #[serde(default)]
    pub aggregator_patterns: Vec<String>,
    pub rules: Vec<Rule>,
}

#[derive(Debug, Deserialize)]
pub struct Rule {
    pub name: String,
    pub class: String,
    pub pattern: String,
}

pub struct Rules {
    aggregators: RegexSet,
    rules: Vec<(Rule, Regex)>,
}

impl Rules {
    pub fn parse(text: &str, extra_aggregators: &[String]) -> Result<Self, Error> {
        let mut file: RulesFile = toml::from_str(text)?;
        file.aggregator_patterns
            .extend_from_slice(extra_aggregators);
        let aggregators = RegexSet::new(file.aggregator_patterns)?;
        let mut rules = Vec::new();
        for rule in file.rules {
            if !matches!(
                rule.class.as_str(),
                "infra" | "code" | "flaky" | "superseded" | "unknown"
            ) {
                return Err(Error::Invalid(format!(
                    "invalid rule class: {}",
                    rule.class
                )));
            }
            let regex = Regex::new(&rule.pattern)?;
            rules.push((rule, regex));
        }
        Ok(Self { aggregators, rules })
    }

    pub fn aggregator(&self, job: &TraceJob) -> bool {
        self.aggregators.is_match(&job.job_name)
            || (!job.steps.is_empty()
                && job.steps.iter().all(|s| {
                    matches!(
                        s.name.to_ascii_lowercase().as_str(),
                        "set up job" | "complete job"
                    )
                }))
    }
}

/// Rerun detection prefers the same SHA; old traces fall back to the same run ID.
pub fn summarize(jobs: &[TraceJob], rules: &Rules) -> Failures {
    let mut successes = BTreeMap::<_, Vec<_>>::new();
    let mut branches = BTreeMap::<_, Vec<_>>::new();
    for j in jobs {
        if j.conclusion.as_deref() == Some("success") {
            successes.entry((&j.repo, &j.job_name)).or_default().push(j);
        }
        if let (Some(branch), Some(created)) = (&j.branch, j.run_created_at) {
            branches
                .entry((&j.repo, &j.workflow, branch))
                .or_default()
                .push((created, j.run_id));
        }
    }
    let mut result = Failures::default();
    for j in jobs {
        let aggregator = rules.aggregator(j);
        if aggregator {
            result.aggregators += 1;
        }
        let has_run = executed(j);
        if has_run && !aggregator {
            result.eligible_jobs += 1;
            if j.completed_at.is_none() {
                result.in_progress_jobs += 1;
            }
        }
        let conclusion = j.conclusion.as_deref().unwrap_or("");
        if !matches!(
            conclusion,
            "failure" | "cancelled" | "timed_out" | "startup_failure"
        ) {
            continue;
        }
        if aggregator {
            continue;
        }
        if !has_run && conclusion == "cancelled" {
            result.cancelled_before_start += 1;
            continue;
        }
        // A failed provisioning job that never ran is excluded from the job-rate denominator.
        if !has_run {
            continue;
        }
        let failed_steps: Vec<_> = j
            .steps
            .iter()
            .filter(|s| s.conclusion.as_deref() == Some("failure"))
            .collect();
        let mut evidence = failed_steps
            .iter()
            .map(|s| format!("failed step: {}", s.name))
            .collect::<Vec<_>>();
        evidence.extend(j.annotations.iter().cloned());
        if let Some(log) = &j.log_excerpt {
            evidence.push(log.clone());
        }
        if j.annotations.is_empty() && j.log_excerpt.is_none() {
            result.missing_evidence += 1;
        }
        let matched = rules
            .rules
            .iter()
            .find(|(_, regex)| evidence.iter().any(|s| regex.is_match(s)));
        let rerun = successes.get(&(&j.repo, &j.job_name)).is_some_and(|list| {
            list.iter().any(|s| {
                let same_commit = match (&j.head_sha, &s.head_sha) {
                    (Some(a), Some(b)) => a == b,
                    _ => j.run_id == s.run_id,
                };
                same_commit
                    && ((s.run_id == j.run_id && s.run_attempt > j.run_attempt)
                        || (s.run_id != j.run_id && s.run_created_at > j.run_created_at))
            })
        });
        let newer = j
            .branch
            .as_ref()
            .and_then(|branch| branches.get(&(&j.repo, &j.workflow, branch)))
            .is_some_and(|list| {
                list.iter().any(|(created, id)| {
                    *id != j.run_id
                        && j.run_created_at.is_some_and(|c| *created > c)
                        && j.completed_at.or(j.started_at).is_some_and(|e| {
                            created.as_nanosecond() <= e.as_nanosecond() + 20_000_000_000
                        })
                })
            });
        let timeout = conclusion == "timed_out"
            || (conclusion == "cancelled"
                && j.timeout_minutes
                    .zip(duration(j))
                    .is_some_and(|(minutes, d)| (d - minutes * 60.0).abs() <= 10.0));
        let (class, reason) = if let Some((rule, _)) = matched {
            (rule.class.as_str(), rule.name.clone())
        } else if timeout {
            ("infra", "workflow timeout".into())
        } else if conclusion == "cancelled" && newer {
            (
                "superseded",
                "newer run on the same workflow and branch".into(),
            )
        } else if conclusion == "failure" && rerun {
            ("flaky", "later rerun of the same commit succeeded".into())
        } else if conclusion == "failure" && !failed_steps.is_empty() {
            ("code", format!("failed step: {}", failed_steps[0].name))
        } else {
            ("unknown", "insufficient failure evidence".into())
        };
        match class {
            "infra" => result.infra += 1,
            "flaky" => result.flaky += 1,
            "code" => result.code += 1,
            "superseded" => result.superseded += 1,
            _ => result.unknown += 1,
        }
        result.details.push(FailureDetail {
            repo: j.repo.clone(),
            run_id: j.run_id,
            attempt: j.run_attempt,
            job: j.job_name.clone(),
            class: class.into(),
            reason,
        });
    }
    result.infra_percent = percent(result.infra, result.eligible_jobs);
    result.flaky_percent = percent(result.flaky, result.eligible_jobs);
    result.infra_and_flaky_percent = percent(result.infra + result.flaky, result.eligible_jobs);
    result.under_one_percent = result.eligible_jobs > 0 && result.infra_and_flaky_percent < 1.0;
    result
}
