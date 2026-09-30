//! Session model and interfaces for later simulation and installation milestones.
use crate::{Error, facts::HostFacts};
use serde::{Deserialize, Serialize};
use std::{io::Write, str::FromStr};

/// A validated SSH destination. IPv6 literals must be bracketed in user input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HostTarget {
    user: String,
    host: String,
    port: Option<u16>,
}

impl HostTarget {
    pub fn destination(&self) -> String {
        format!("{}@{}", self.user, self.host)
    }
    pub fn port(&self) -> Option<u16> {
        self.port
    }
}

impl FromStr for HostTarget {
    type Err = Error;
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let (user, address) = input.split_once('@').ok_or(Error::InvalidHost)?;
        if user.is_empty()
            || !user
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
            || user.starts_with('-')
        {
            return Err(Error::InvalidHost);
        }
        let (host, port) = if let Some(ipv6) = address.strip_prefix('[') {
            let (host, suffix) = ipv6.split_once(']').ok_or(Error::InvalidHost)?;
            if host.parse::<std::net::Ipv6Addr>().is_err() {
                return Err(Error::InvalidHost);
            }
            (
                host,
                if suffix.is_empty() {
                    None
                } else {
                    Some(suffix.strip_prefix(':').ok_or(Error::InvalidHost)?)
                },
            )
        } else {
            let (host, port) = match address.split_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (address, None),
            };
            if host.is_empty()
                || host.starts_with('-')
                || !host
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
            {
                return Err(Error::InvalidHost);
            }
            (host, port)
        };
        let port = port
            .map(|p| p.parse::<u16>().map_err(|_| Error::InvalidHost))
            .transpose()?;
        if port == Some(0) {
            return Err(Error::InvalidHost);
        }
        Ok(Self {
            user: user.into(),
            host: host.into(),
            port,
        })
    }
}
impl TryFrom<String> for HostTarget {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Error> {
        value.parse()
    }
}
impl From<HostTarget> for String {
    fn from(target: HostTarget) -> Self {
        target.to_string()
    }
}
impl std::fmt::Display for HostTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@", self.user)?;
        if self.host.contains(':') {
            write!(f, "[{}]", self.host)?;
        } else {
            write!(f, "{}", self.host)?;
        }
        if let Some(port) = self.port {
            write!(f, ":{port}")?;
        }
        Ok(())
    }
}

/// Repository input; workload traces are supplied separately to the recommender.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RepoTarget(pub String);
impl FromStr for RepoTarget {
    type Err = Error;
    fn from_str(input: &str) -> Result<Self, Error> {
        let parts: Vec<_> = input.split('/').collect();
        if parts.len() != 2
            || parts.iter().any(|p| {
                p.is_empty()
                    || *p == "."
                    || *p == ".."
                    || !p
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
            })
        {
            return Err(Error::InvalidRepo);
        }
        Ok(Self(input.into()))
    }
}
impl TryFrom<String> for RepoTarget {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Error> {
        value.parse()
    }
}
impl From<RepoTarget> for String {
    fn from(repo: RepoTarget) -> Self {
        repo.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbedHost {
    pub target: HostTarget,
    pub facts: HostFacts,
}

/// Local resumable wizard state. No credentials or passwords are accepted.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SetupSession {
    pub hosts: Vec<ProbedHost>,
    pub repos: Vec<RepoTarget>,
    pub recommendation: Option<Vec<RankedArchitecture>>,
    pub plan: Option<InstallPlan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Architecture {
    pub name: String,
    pub description: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankedArchitecture {
    pub architecture: Architecture,
    pub predicted_p50_ms: f64,
    pub predicted_p90_ms: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoTrace {
    pub repo: RepoTarget,
    pub jobs: Vec<TraceJob>,
    pub uses_service_containers: Option<bool>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceJob {
    pub arrival_ms: u64,
    pub duration_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallPlan {
    pub architecture: Architecture,
    pub steps: Vec<InstallStep>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallStep {
    pub description: String,
    pub command: Vec<String>,
}

/// Future adapters can translate traces to runwell-sim without coupling discovery.
pub trait Recommender {
    fn recommend(
        &self,
        hosts: &[ProbedHost],
        traces: &[RepoTrace],
    ) -> Result<Vec<RankedArchitecture>, Error>;
}
pub struct StubRecommender;
impl Recommender for StubRecommender {
    fn recommend(
        &self,
        _hosts: &[ProbedHost],
        _traces: &[RepoTrace],
    ) -> Result<Vec<RankedArchitecture>, Error> {
        Err(Error::Unimplemented)
    }
}

/// Separate plan generation from execution; implementations must check confirmation.
pub trait Applier {
    fn plan(&self, architecture: &Architecture) -> Result<InstallPlan, Error>;
    fn execute(
        &self,
        plan: &InstallPlan,
        confirmation: &str,
        output: &mut dyn Write,
    ) -> Result<(), Error>;
}
/// Prints explicit steps and never runs any command, even after confirmation.
pub struct DryRunApplier;
impl Applier for DryRunApplier {
    fn plan(&self, architecture: &Architecture) -> Result<InstallPlan, Error> {
        Ok(InstallPlan {
            architecture: architecture.clone(),
            steps: vec![InstallStep {
                description:
                    "Installation is reserved for a later milestone; no changes will be made".into(),
                command: vec![],
            }],
        })
    }
    fn execute(
        &self,
        plan: &InstallPlan,
        confirmation: &str,
        output: &mut dyn Write,
    ) -> Result<(), Error> {
        if confirmation != "APPLY" {
            return Err(Error::ConfirmationRequired);
        }
        writeln!(output, "Dry run: {}", plan.architecture.name)?;
        for step in &plan.steps {
            writeln!(output, "{}: {:?}", step.description, step.command)?;
        }
        Ok(())
    }
}
