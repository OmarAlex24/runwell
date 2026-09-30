//! TOML configuration and validation for runwell.
//!
//! Unknown keys are rejected, credentials are file references, and resource
//! reservations must fit the node budget. Validation performs no host or network I/O.

#![deny(missing_docs)]

use serde::Deserialize;
use std::{collections::HashSet, path::PathBuf};

/// Complete configuration; call validate before using manually constructed values.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Configuration schema version, currently one.
    pub schema_version: u32,
    /// Controller scale-set classes and database location.
    pub controller: ControllerConfig,
    /// Node identity and resource budget.
    pub node: NodeConfig,
    /// GitHub scope and authentication files.
    pub github: GithubConfig,
    /// Mutual TLS identity and trust roots.
    pub transport: TransportConfig,
}

/// Controller configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControllerConfig {
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
    #[error("invalid TOML configuration: {0}")]
    Parse(#[from] toml::de::Error),
    /// Invalid configuration values.
    #[error("invalid configuration: {0}")]
    Validation(String),
}

impl Config {
    /// Parse TOML and validate its values without reading credential files.
    pub fn from_toml(source: &str) -> Result<Self, Error> {
        let config: Self = toml::from_str(source)?;
        config.validate()?;
        Ok(config)
    }

    /// Reject invalid reservations, ambiguous classes, paths, and PSI thresholds.
    pub fn validate(&self) -> Result<(), Error> {
        require(self.schema_version == 1, "schema_version must be 1")?;
        require(!self.node.id.trim().is_empty(), "node.id must not be empty")?;
        require(self.node.cpu_slots > 0, "node.cpu_slots must be positive")?;
        require(
            self.node.memory_bytes > 0,
            "node.memory_bytes must be positive",
        )?;
        let psi = &self.node.psi;
        require(
            psi.resume_percent.is_finite()
                && psi.pause_percent.is_finite()
                && psi.resume_percent >= 0.0
                && psi.resume_percent < psi.pause_percent
                && psi.pause_percent <= 100.0,
            "PSI thresholds must satisfy 0 <= resume_percent < pause_percent <= 100",
        )?;
        require(
            self.github.config_url.starts_with("https://")
                && self.github.config_url.len() > "https://".len(),
            "github.config_url must be a nonempty HTTPS URL",
        )?;
        require(
            !self.controller.classes.is_empty(),
            "controller.classes must not be empty",
        )?;
        let mut names = HashSet::new();
        for class in &self.controller.classes {
            require(
                !class.name.is_empty()
                    && class
                        .name
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-')
                    && names.insert(&class.name),
                "class names must be unique, nonempty, and contain only letters, digits, or hyphens",
            )?;
            require(
                class.cpu_slots > 0 && class.cpu_slots <= self.node.cpu_slots,
                "class CPU reservation must be positive and fit the node budget",
            )?;
            require(
                class.memory_high_bytes > 0
                    && class.memory_high_bytes <= class.memory_max_bytes
                    && class.memory_max_bytes <= self.node.memory_bytes,
                "class memory must satisfy 0 < memory_high_bytes <= memory_max_bytes <= node budget",
            )?;
            require(
                (1..=10000).contains(&class.cpu_weight),
                "cpu_weight must be between 1 and 10000",
            )?;
        }
        for path in [
            &self.controller.database,
            &self.node.state_dir,
            &self.transport.ca_file,
            &self.transport.certificate_file,
            &self.transport.private_key_file,
        ] {
            require(path.is_absolute(), "state and TLS paths must be absolute")?;
        }
        match &self.github.auth {
            AuthConfig::App {
                app_id,
                installation_id,
                private_key_file,
            } => {
                require(
                    *app_id > 0 && *installation_id > 0,
                    "App and installation IDs must be positive",
                )?;
                require(
                    private_key_file.is_absolute(),
                    "App private key path must be absolute",
                )?;
            }
            AuthConfig::Pat { token_file } => {
                require(token_file.is_absolute(), "PAT file path must be absolute")?;
            }
        }
        Ok(())
    }
}

fn require(valid: bool, message: &str) -> Result<(), Error> {
    if valid {
        Ok(())
    } else {
        Err(Error::Validation(message.to_owned()))
    }
}
