//! TOML configuration and validation for runwell.
//!
//! Unknown keys are rejected, credentials are file references, and resource
//! reservations must fit the node budget. Validation performs no host or network I/O.

#![deny(missing_docs)]

use serde::Deserialize;
use std::{collections::HashSet, path::PathBuf};

mod dockerproxy;
mod network;
mod production;
pub use network::{NetworkConfig, PeerNode, valid_identity};
pub use production::ProductionConfig;
mod standalone;
mod validation;
mod workspace;
pub use dockerproxy::DockerProxyConfig;
pub use standalone::{CiLimits, Overcommit, RunnerConfig, StandaloneConfig};
pub use workspace::{WorkspaceConfig, valid_repository};

/// Complete configuration; call validate before using manually constructed values.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Optional authenticated multi-host topology.
    pub network: Option<NetworkConfig>,
    /// Configuration schema version, currently one.
    pub schema_version: u32,
    /// Controller scale-set classes and database location.
    pub controller: ControllerConfig,
    /// Node identity and resource budget.
    pub node: NodeConfig,
    /// GitHub scope and authentication files.
    pub github: GithubConfig,
    /// Mutual TLS identity and trust roots.
    pub transport: Option<TransportConfig>,
    /// Local controller/node wiring; required with --standalone.
    pub standalone: Option<StandaloneConfig>,
}

/// Controller configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControllerConfig {
    /// Production scheduling, retry, metrics and alert settings.
    #[serde(default)]
    pub production: ProductionConfig,
    /// Durable SQLite database path.
    pub database: PathBuf,
    /// Nonempty list of resource classes, each owning one scale-set session.
    pub classes: Vec<JobClass>,
}

/// A class reservation and hard resource ceiling.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobClass {
    /// Unique scale-set name such as runwell-small.
    pub name: String,
    /// Reserved CPU slots.
    pub cpu_slots: u32,
    /// Reserved RAM and MemoryHigh threshold in bytes.
    pub memory_high_bytes: u64,
    /// MemoryMax ceiling in bytes.
    pub memory_max_bytes: u64,
    /// Systemd CPUWeight in the range 1 through 10000.
    pub cpu_weight: u32,
    /// Additional routing labels (the class name is always included).
    #[serde(default)]
    pub labels: Vec<String>,
    /// Task ceiling for the entire job cgroup.
    #[serde(default = "default_tasks")]
    pub tasks_max: u64,
}

/// Per-host capacity after reserving headroom for the OS and daemon.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    /// Stable node identity, authenticated by mTLS.
    pub id: String,
    /// State, overlay, and template root.
    pub state_dir: PathBuf,
    /// CPU slots available to jobs.
    pub cpu_slots: u32,
    /// RAM available to jobs in bytes.
    pub memory_bytes: u64,
    /// Pressure thresholds for admission hysteresis.
    pub psi: PsiConfig,
}

/// Memory PSI some average thresholds expressed as percentages.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PsiConfig {
    /// Pause admission at or above this threshold.
    pub pause_percent: f64,
    /// Resume below this strictly lower threshold.
    pub resume_percent: f64,
    /// CPU high/low overrides; defaults to the shared thresholds.
    pub cpu: Option<PressureThreshold>,
    /// I/O high/low overrides; defaults to the shared thresholds.
    pub io: Option<PressureThreshold>,
    /// Minimum time in each brake state and sustained recovery time.
    #[serde(default = "default_dwell")]
    pub dwell_seconds: u64,
}

/// GitHub scope and authentication.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubConfig {
    /// HTTPS organization, repository, or enterprise URL.
    pub config_url: String,
    /// Credential file references, never inline secrets.
    pub auth: AuthConfig,
}

/// Secret file references; their contents must never be logged.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthConfig {
    /// GitHub App authentication.
    App {
        /// Nonzero App identity.
        app_id: u64,
        /// Nonzero installation identity.
        installation_id: u64,
        /// PEM private key, preferably delivered through systemd credentials.
        private_key_file: PathBuf,
    },
    /// App key read from an environment variable.
    AppEnv {
        /// App identity.
        app_id: u64,
        /// Installation identity.
        installation_id: u64,
        /// Environment variable containing the PEM.
        private_key_env: String,
    },
    /// PAT read from an environment variable.
    PatEnv {
        /// Environment variable containing the PAT.
        token_env: String,
    },
    /// PAT authentication.
    Pat {
        /// File containing the PAT.
        token_file: PathBuf,
    },
}

/// Mutual TLS is mandatory between controller and node.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportConfig {
    /// Trusted peer certificate authority bundle.
    pub ca_file: PathBuf,
    /// Local certificate chain.
    pub certificate_file: PathBuf,
    /// Local private key.
    pub private_key_file: PathBuf,
}

/// Configuration parse or validation failure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid TOML syntax or schema.
    #[error("invalid TOML configuration (source redacted)")]
    Parse,
    /// Invalid configuration values.
    #[error("invalid configuration: {0}")]
    Validation(String),
}

/// Per-resource pressure thresholds, in percent.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PressureThreshold {
    /// Pause at or above this value.
    pub high: f64,
    /// Resume below this value.
    pub low: f64,
}
fn default_tasks() -> u64 {
    4096
}
fn default_dwell() -> u64 {
    30
}
