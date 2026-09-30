//! Command-line arguments and window resolution.
use crate::Error;
use clap::{Args, ValueEnum};
use jiff::Timestamp;
use runwell_trace::TraceJob;
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Format {
    Md,
    Json,
}

/// Explain CI latency and infrastructure failures from GitHub or an offline trace.
#[derive(Debug, Clone, Args)]
#[command(
    after_help = "Examples:\n  runwell report --repo acme/app --since 7d --event pull_request\n  runwell report --from-trace history.jsonl --format json\n\nOffline input defaults to its complete observed window. Live input defaults to 7d.\nDurations in JSON are seconds; Markdown tables use minutes.\nAuth: GH_TOKEN, then GITHUB_TOKEN, then `gh auth token`.\nRaw API responses are cached privately for 5 minutes; YAML at a SHA is immutable."
)]
pub struct ReportArgs {
    /// Repository owner/name; repeat for multiple repositories.
    #[arg(long, required_unless_present = "from_trace")]
    pub repo: Vec<String>,
    /// Start of window: relative duration (7d), UTC date, or RFC 3339 timestamp.
    #[arg(long)]
    pub since: Option<String>,
    /// Exclusive end of window: UTC date or RFC 3339 timestamp (default: now).
    #[arg(long)]
    pub until: Option<String>,
    /// Restrict reported runs/jobs to an event; host concurrency still uses all events.
    #[arg(long)]
    pub event: Option<String>,
    /// Restrict reported metrics to workflow display names; repeat (host sweep uses all).
    #[arg(long)]
    pub workflow: Vec<String>,
    /// Output Markdown or versioned JSON.
    #[arg(long, value_enum, default_value = "md")]
    pub format: Format,
    /// Write the collected, unfiltered portable JSONL trace.
    #[arg(long)]
    pub export_trace: Option<PathBuf>,
    /// Read a portable JSONL trace fully offline, without authentication.
    #[arg(long, conflicts_with = "fetch_logs")]
    pub from_trace: Option<PathBuf>,
    /// Cache raw API responses here (default: platform cache directory/runwell/report).
    #[arg(long)]
    pub cache_dir: Option<PathBuf>,
    /// Download logs for at most N failed/cancelled jobs to improve classification.
    #[arg(long, default_value_t = 0)]
    pub fetch_logs: usize,
    /// TOML classification rules; replaces defaults.
    #[arg(long)]
    pub rules: Option<PathBuf>,
    /// In-job wait step regex; repeat to replace the default list.
    #[arg(long)]
    pub wait_regex: Vec<String>,
    /// Aggregator job-name regex excluded from the failure-rate denominator; repeat.
    #[arg(long)]
    pub aggregator_regex: Vec<String>,
    /// Host label required for concurrency/contention analysis; repeat requires all labels.
    #[arg(long)]
    pub host_label: Vec<String>,
    /// Scope every reported metric to jobs with this label; repeat requires all labels.
    #[arg(long)]
    pub label: Vec<String>,
    /// Maximum time-weighted concurrency for low-contention samples (inclusive).
    #[arg(long, default_value_t = 3)]
    pub low_concurrency: usize,
    /// Minimum time-weighted concurrency for high-contention samples (inclusive).
    #[arg(long, default_value_t = 7)]
    pub high_concurrency: usize,
    /// Runner-label saturation capacity, LABEL=N; repeat. Otherwise inferred from runners.
    #[arg(long)]
    pub label_capacity: Vec<String>,
}

impl ReportArgs {
    pub fn validate(&self) -> Result<(), Error> {
        if self.low_concurrency == 0 || self.high_concurrency <= self.low_concurrency {
            return Err(Error::Invalid(
                "require 1 <= low-concurrency < high-concurrency".into(),
            ));
        }
        for repo in &self.repo {
            let parts: Vec<_> = repo.split('/').collect();
            if parts.len() != 2
                || parts.iter().any(|p| {
                    p.is_empty()
                        || !p
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
                })
            {
                return Err(Error::Invalid(format!(
                    "invalid repository owner/name: {repo}"
                )));
            }
        }
        Ok(())
    }
    pub fn capacities(&self) -> Result<BTreeMap<String, usize>, Error> {
        let mut result = BTreeMap::new();
        for value in &self.label_capacity {
            let (label, n) = value
                .split_once('=')
                .ok_or_else(|| Error::Invalid("label capacity must be LABEL=N".into()))?;
            let n = n
                .parse::<usize>()
                .map_err(|_| Error::Invalid("invalid label capacity".into()))?;
            if n == 0 || label.is_empty() {
                return Err(Error::Invalid("label capacity must be positive".into()));
            }
            result.insert(label.into(), n);
        }
        Ok(result)
    }
    pub fn window(&self, jobs: Option<&[TraceJob]>) -> Result<(Timestamp, Timestamp), Error> {
        let end = match &self.until {
            Some(s) => timestamp(s)?,
            None => jobs
                .and_then(|jobs| {
                    jobs.iter()
                        .filter_map(|j| j.completed_at.or(j.started_at).or(j.run_created_at))
                        .max()
                })
                .map(|t| {
                    Timestamp::from_nanosecond(t.as_nanosecond() + 1_000_000_000)
                        .map_err(|e| Error::Invalid(e.to_string()))
                })
                .transpose()?
                .unwrap_or_else(Timestamp::now),
        };
        let start = match &self.since {
            Some(s) => since(s, end)?,
            None => jobs
                .and_then(|jobs| {
                    jobs.iter()
                        .filter_map(|j| j.run_created_at.or(j.created_at))
                        .min()
                })
                .map(Ok)
                .unwrap_or_else(|| since("7d", end))?,
        };
        if start >= end {
            return Err(Error::Invalid("since must precede until".into()));
        }
        Ok((start, end))
    }
    pub fn cache(&self) -> PathBuf {
        self.cache_dir.clone().unwrap_or_else(|| {
            let root = std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
                .unwrap_or_else(std::env::temp_dir);
            root.join("runwell/report")
        })
    }
}

fn timestamp(s: &str) -> Result<Timestamp, Error> {
    let s = if s.len() == 10 {
        format!("{s}T00:00:00Z")
    } else {
        s.into()
    };
    s.parse()
        .map_err(|e| Error::Invalid(format!("invalid UTC date/timestamp: {e}")))
}
fn since(s: &str, end: Timestamp) -> Result<Timestamp, Error> {
    if let Ok(duration) = humantime::parse_duration(s) {
        let ns = i128::try_from(duration.as_nanos())
            .map_err(|_| Error::Invalid("duration too large".into()))?;
        return Timestamp::from_nanosecond(end.as_nanosecond() - ns)
            .map_err(|e| Error::Invalid(e.to_string()));
    }
    timestamp(s)
}
