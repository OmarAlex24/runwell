//! Generic CI performance and infrastructure-failure reports over portable traces.
pub mod cache;
pub mod classify;
pub mod fetch;
pub mod metrics;
pub mod model;
pub mod options;
pub mod render;
pub mod trace_build;
mod workflow;

pub use model::Report;
pub use options::{Format, ReportArgs};
use regex::RegexSet;
use runwell_trace::TraceJob;
use std::{fs::File, io::BufReader};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("TOML rules: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("workflow YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("regex: {0}")]
    Regex(#[from] regex::Error),
    #[error("trace: {0}")]
    Trace(#[from] runwell_trace::TraceError),
    #[error("GitHub request: {0}")]
    Request(#[from] reqwest::Error),
    #[error("GitHub returned HTTP {0}")]
    Http(u16),
}

/// Fetch or read a trace, export it if requested, and produce the selected format.
pub async fn execute(args: &ReportArgs) -> Result<String, Error> {
    args.validate()?;
    let mut resolved = args.clone();
    args.capacities()?;
    let waits = wait_patterns(args);
    RegexSet::new(&waits)?;
    let rules_text = match &args.rules {
        Some(p) => std::fs::read_to_string(p)?,
        None => include_str!("../default-rules.toml").into(),
    };
    classify::Rules::parse(&rules_text, &args.aggregator_regex)?;
    let (jobs, warnings) = if let Some(path) = &args.from_trace {
        (
            runwell_trace::read_jsonl(BufReader::new(File::open(path)?))?,
            Vec::new(),
        )
    } else {
        let (start, end) = args.window(None)?;
        resolved.since = Some(start.to_string());
        resolved.until = Some(end.to_string());
        let mut client = fetch::Client::new(
            "https://api.github.com".into(),
            if args.offline_cache {
                String::new()
            } else {
                fetch::token().await?
            },
            args.cache(),
        )?;
        if args.offline_cache {
            client.offline_cache();
        }
        let collected =
            fetch::collect(&mut client, &args.repo, start, end, args.fetch_logs).await?;
        let mut warnings = collected.warnings;
        if args.offline_cache {
            warnings.push("Replayed cached API snapshot; responses were not refreshed.".into());
        }
        (collected.jobs, warnings)
    };
    if let Some(path) = &args.export_trace {
        if args.from_trace.as_ref().is_some_and(|p| p == path) {
            return Err(Error::Invalid(
                "export-trace must differ from from-trace".into(),
            ));
        }
        let mut bytes = Vec::new();
        runwell_trace::write_jsonl(&mut bytes, &jobs)?;
        cache::write_private(path, &bytes)?;
    }
    let report = analyze(&jobs, &resolved, &rules_text, warnings)?;
    match args.format {
        Format::Md => Ok(render::markdown(&report)),
        Format::Json => Ok(serde_json::to_string_pretty(&report)?),
    }
}

fn wait_patterns(args: &ReportArgs) -> Vec<String> {
    if args.wait_regex.is_empty() {
        vec!["(?i)(wait|slot|semaphore|lock)".into()]
    } else {
        args.wait_regex.clone()
    }
}

/// Pure report builder: no filesystem, credentials, or network access.
pub fn analyze(
    jobs: &[TraceJob],
    args: &ReportArgs,
    rules_text: &str,
    mut warnings: Vec<String>,
) -> Result<Report, Error> {
    args.validate()?;
    let capacities = args.capacities()?;
    let (start, end) = args.window(Some(jobs))?;
    let waits = wait_patterns(args);
    let wait_set = RegexSet::new(&waits)?;
    let rules = classify::Rules::parse(rules_text, &args.aggregator_regex)?;
    let in_repo = |j: &&TraceJob| args.repo.is_empty() || args.repo.contains(&j.repo);
    // Keep every event in the host sweep; run-level selection is applied below.
    let within_window = |j: &TraceJob| {
        j.run_created_at
            .or(j.created_at)
            .is_some_and(|c| c >= start && c < end)
    };
    let host_jobs: Vec<_> = jobs
        .iter()
        .filter(in_repo)
        .filter(|j| {
            within_window(j)
                || j.started_at
                    .zip(j.completed_at)
                    .is_some_and(|(s, e)| s < end && e > start)
        })
        .cloned()
        .collect();
    let timeline =
        metrics::concurrency::timeline(&host_jobs, start, end, &args.host_label, &capacities);
    let selected: Vec<_> = host_jobs
        .iter()
        .filter(|j| {
            within_window(j)
                && args
                    .event
                    .as_ref()
                    .is_none_or(|event| j.event.as_ref() == Some(event))
                && (args.workflow.is_empty()
                    || j.workflow
                        .as_ref()
                        .is_some_and(|w| args.workflow.contains(w)))
                && metrics::concurrency::host_matches(j, &args.label)
        })
        .cloned()
        .collect();
    let factors = metrics::bestcase::factors(
        &selected,
        &timeline,
        &args.host_label,
        args.low_concurrency,
        &wait_set,
    );
    let runs = metrics::runs::summarize(&selected, &wait_set, &factors);
    let failures = classify::summarize(&selected, &rules);
    if runs.iter().any(|r| r.inferred_runs > 0) {
        warnings.push("Timestamp-inferred critical paths are approximate (3-second dispatch tolerance); matrix limits and reusable workflows are not reconstructed.".into());
    }
    if failures.in_progress_jobs > 0 {
        warnings.push(format!("{} executed jobs are still in progress and count in the reliability denominator; their outcomes are pending.", failures.in_progress_jobs));
    }
    if failures.missing_evidence > 0 {
        warnings.push(format!("{} failed/cancelled jobs lack annotations and logs; infra classification is a lower bound.",failures.missing_evidence));
    }
    if selected.iter().any(|j| j.head_sha.is_none()) {
        warnings.push("Some commit SHAs are missing; flaky detection falls back to later attempts of the same run ID.".into());
    }
    if selected.iter().any(|j| {
        metrics::duration(j).is_some()
            && !factors.contains_key(&(j.repo.clone(), j.job_name.clone()))
    }) {
        warnings.push("Some jobs have no low-concurrency samples; their best-case estimate retains measured execution duration.".into());
    }
    warnings.push("Best-case estimates remove queue and matching wait steps, retain dispatch gaps, and scale each job by min(1, low-concurrency median / overall median). They use the observed graph, not a hypothetical workflow redesign.".into());
    warnings.sort();
    warnings.dedup();
    Ok(Report {
        schema_version: 1,
        window: model::Window {
            since: start.to_string(),
            until: end.to_string(),
            jobs: selected.len(),
        },
        configuration: model::Configuration {
            host_labels: args.host_label.clone(),
            low_concurrency: args.low_concurrency,
            high_concurrency: args.high_concurrency,
            wait_patterns: waits,
            infra_target_percent: 1.0,
            latency_target_ratio: 0.5,
        },
        runs,
        jobs: metrics::jobs::summarize(&selected),
        contention: metrics::contention::summarize(
            &selected,
            &timeline,
            &args.host_label,
            args.low_concurrency,
            args.high_concurrency,
            &wait_set,
        ),
        concurrency: timeline.report,
        failures,
        warnings,
    })
}

#[cfg(test)]
mod tests;
