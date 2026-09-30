use super::{Config, Error};
use serde::Deserialize;
use std::path::PathBuf;

/// In-process controller and local node settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StandaloneConfig {
    /// Maximum simultaneous reservations on this host.
    pub max_jobs: u32,
    /// Reservation capacity multipliers.
    pub overcommit: Overcommit,
    /// Persistent parent slice resource ceilings.
    pub ci: CiLimits,
    /// Dedicated unprivileged local account.
    pub runner_user: String,
    /// Immutable template generations directory.
    pub templates_dir: PathBuf,
    /// Independent per-job installation directories.
    pub runners_dir: PathBuf,
    /// Runner binary release policy.
    pub runner: RunnerConfig,
    /// Maximum drain wait; on expiry preserve running units for the next daemon.
    pub drain_seconds: u64,
    /// Service graceful termination timeout.
    pub stop_seconds: u64,
    /// Unbound runner timeout; DELETE must confirm idle before stopping it.
    #[serde(default = "idle_seconds")]
    pub idle_seconds: u64,
    /// Timer for reconsideration and cleanup retries.
    pub reconcile_seconds: u64,
    /// Scale-set runner group identity.
    pub runner_group_id: i64,
}
/// Independent CPU/RAM overcommit factors.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overcommit {
    /// CPU reservation multiplier.
    pub cpu: f64,
    /// RAM reservation multiplier.
    pub memory: f64,
}
/// Limits applied to the persistent ci.slice.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CiLimits {
    /// Fair CPU weight against host services.
    pub cpu_weight: u32,
    /// Aggregate hard RAM ceiling in bytes.
    pub memory_max_bytes: u64,
}
/// Pinned initial release, refreshed before the upstream update deadline.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerConfig {
    /// Numeric linux-x64 runner release (without a leading v).
    pub version: String,
    /// Optional SHA-256 hex digest; otherwise require the release body's checksum.
    pub sha256: Option<String>,
}
impl StandaloneConfig {
    pub(super) fn validate(&self, config: &Config) -> Result<(), Error> {
        let require = |ok, message: &str| {
            if ok {
                Ok(())
            } else {
                Err(Error::Validation(message.into()))
            }
        };
        require(self.max_jobs > 0, "standalone.max_jobs must be positive")?;
        require(
            [self.overcommit.cpu, self.overcommit.memory]
                .iter()
                .all(|v| v.is_finite() && *v > 0.0),
            "overcommit must be finite and positive",
        )?;
        require(
            (1..=10000).contains(&self.ci.cpu_weight)
                && self.ci.memory_max_bytes > 0
                && self.ci.memory_max_bytes <= i64::MAX as u64,
            "invalid ci.slice limits",
        )?;
        require(
            self.ci.memory_max_bytes >= config.node.memory_bytes,
            "ci.slice memory ceiling must cover the host reservation budget",
        )?;
        require(
            self.runner_user != "root"
                && !self.runner_user.is_empty()
                && self
                    .runner_user
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "runner_user must name an unprivileged local account",
        )?;
        for path in [
            &self.templates_dir,
            &self.runners_dir,
            &config.node.state_dir,
        ] {
            require(
                path.is_absolute()
                    && !path
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
                    && path.parent().is_some(),
                "node directories must be absolute non-root paths without ..",
            )?;
        }
        require(
            !self.templates_dir.starts_with(&self.runners_dir)
                && !self.runners_dir.starts_with(&self.templates_dir),
            "template and runner directories must not overlap",
        )?;
        require(
            !config.controller.database.starts_with(&self.runners_dir)
                && config
                    .controller
                    .database
                    .starts_with(&config.node.state_dir),
            "database must be inside state_dir and outside runners_dir",
        )?;
        require(
            (1..=604800).contains(&self.drain_seconds)
                && (1..=3600).contains(&self.stop_seconds)
                && (1..=86400).contains(&self.idle_seconds)
                && (1..=3600).contains(&self.reconcile_seconds)
                && self.runner_group_id > 0,
            "timeouts must be bounded (drain <= 7 days, idle <= 1 day, stop/reconcile <= 1 hour); runner_group_id must be positive",
        )?;
        require(
            self.runner.version.split('.').count() == 3
                && self
                    .runner
                    .version
                    .split('.')
                    .all(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())),
            "runner version must be numeric major.minor.patch",
        )?;
        if let Some(hash) = &self.runner.sha256 {
            require(
                hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                "runner.sha256 must have 64 hex digits",
            )?;
        }
        require(
            config.node.psi.dwell_seconds > 0,
            "PSI dwell time must be positive",
        )?;
        for threshold in [&config.node.psi.cpu, &config.node.psi.io]
            .into_iter()
            .flatten()
        {
            require(
                threshold.low.is_finite()
                    && threshold.high.is_finite()
                    && threshold.low >= 0.0
                    && threshold.low < threshold.high
                    && threshold.high <= 100.0,
                "invalid per-resource PSI thresholds",
            )?;
        }
        for class in &config.controller.classes {
            require(
                class.tasks_max > 0
                    && class
                        .labels
                        .iter()
                        .all(|l| !l.is_empty() && !l.chars().any(char::is_control)),
                "invalid class task limit or label",
            )?;
        }
        Ok(())
    }
}

fn idle_seconds() -> u64 {
    180
}
