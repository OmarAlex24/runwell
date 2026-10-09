//! Per-host speed: runner attribution, factor overrides and runner choice.
use super::Config;
use crate::Error;
use serde::{Deserialize, Serialize};

/// Replay-only duration multiple of one job class on one host, relative to the
/// class's fastest host. Fitting still reads the trace and reports its values.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeedFactor {
    /// Host index in the configured list.
    pub host: usize,
    /// Workflow job id, or the job name when the trace records no id.
    pub job: String,
    /// Optional repository scope; absent matches the job in every repository.
    #[serde(default)]
    pub repo: Option<String>,
    /// Duration multiple; 1.0 runs as fast as the fastest host.
    pub factor: f64,
}

/// Host choice among hosts with an idle runner in runner-limited replay.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunnerChoice {
    /// The first such host in configured order.
    #[default]
    First,
    /// Any idle runner with equal probability, by a seeded per-job draw.
    Uniform,
}

impl Config {
    /// Whether any host lists runner-name patterns.
    pub fn attributes_hosts(&self) -> bool {
        self.hosts.iter().any(|h| !h.runners.is_empty())
    }
    /// Full-match runner patterns, one list per host.
    pub(crate) fn runner_patterns(&self) -> Result<Vec<Vec<regex::Regex>>, Error> {
        self.hosts
            .iter()
            .enumerate()
            .map(|(i, h)| {
                h.runners
                    .iter()
                    .map(|p| {
                        regex::Regex::new(&format!("^(?:{p})$")).map_err(|_| {
                            Error::Invalid(format!("invalid runner pattern on host {i}"))
                        })
                    })
                    .collect()
            })
            .collect()
    }
    pub(super) fn validate_speed(&self) -> Result<(), Error> {
        self.runner_patterns()?;
        if self
            .factor_low_concurrency
            .is_some_and(|x| !x.is_finite() || x <= 0.0)
            || self.speed_factors.iter().any(|f| {
                f.host >= self.hosts.len()
                    || f.job.is_empty()
                    || !f.factor.is_finite()
                    || f.factor <= 0.0
            })
        {
            return Err(Error::Invalid(
                "speed factors need a configured host, a job and a positive factor".into(),
            ));
        }
        Ok(())
    }
}
