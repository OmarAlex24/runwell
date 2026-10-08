//! Deterministic discrete-event CI replay using shared scheduler/admission rules.
#![deny(missing_docs)]

pub mod availability;
pub mod config;
pub mod contention;
mod engine;
mod intrinsic;
mod model;
mod observation;
pub use model::ClassModel;
pub mod experiments;
mod prepare_graph;
mod report;
pub mod search;
mod trace;
pub mod workflow;

pub use config::Config;
pub use report::{Calibration, Metrics, ObservedMetrics, Report, Row};
use runwell_scheduler::Priority;
use serde::{Deserialize, Serialize};
pub use trace::{Diagnostics, PreparedTrace};

/// Named scheduling variants exposed by the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Policy {
    /// Fixed per-repository runner pools with optional heavy semaphore.
    Baseline,
    /// Same per-pool capacity and FIFO as baseline, without semaphore occupancy.
    #[serde(rename = "runwell-equivalent")]
    Equivalent,
    /// Resource admission and FIFO.
    Fifo,
    /// Resource admission and shortest expected job first.
    Shortest,
    /// Resource admission and longest remaining path first.
    CriticalPath,
    /// Resource admission and repository CPU service fairness.
    FairShare,
    /// Hierarchical weighted fairness, criticality, learned durations and admission headroom.
    Production,
}
impl Policy {
    /// Complete comparison set, in stable output order.
    pub const ALL: [Self; 7] = [
        Self::Baseline,
        Self::Equivalent,
        Self::Fifo,
        Self::Shortest,
        Self::CriticalPath,
        Self::FairShare,
        Self::Production,
    ];
    /// CLI/report name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Equivalent => "runwell-equivalent",
            Self::Fifo => "fifo",
            Self::Shortest => "shortest",
            Self::CriticalPath => "critical-path",
            Self::FairShare => "fair-share",
            Self::Production => "production",
        }
    }
    /// Parse a CLI policy name. `runwell` aliases critical-path.
    pub fn parse(value: &str) -> Result<Self, Error> {
        if value == "runwell" {
            return Ok(Self::CriticalPath);
        }
        Self::ALL
            .into_iter()
            .find(|p| p.name() == value)
            .ok_or_else(|| Error::Invalid(format!("unknown policy: {value}")))
    }
    pub(crate) fn runner_limited(self) -> bool {
        matches!(self, Self::Baseline | Self::Equivalent)
    }
    pub(crate) fn priority(self) -> Option<Priority> {
        match self {
            Self::Baseline | Self::Equivalent | Self::Production => None,
            Self::Fifo => Some(Priority::Fifo),
            Self::Shortest => Some(Priority::Shortest),
            Self::CriticalPath => Some(Priority::CriticalPath),
            Self::FairShare => Some(Priority::FairShare),
        }
    }
}

/// Run requested policies for each prefix of the host list (one host, then two,
/// and so on). Preparation and fitting are performed only once.
pub fn simulate(
    trace: &[runwell_trace::TraceJob],
    config: &Config,
    policies: &[Policy],
) -> Result<Report, Error> {
    let prepared = PreparedTrace::new(trace, config)?;
    compare(&prepared, policies)
}

/// Compare scenarios against an already prepared trace using the same configuration.
pub fn compare(trace: &PreparedTrace, policies: &[Policy]) -> Result<Report, Error> {
    let config = &trace.config;
    config.validate()?;
    if policies.is_empty() {
        return Err(Error::Invalid("at least one policy is required".into()));
    }
    let mut report = Report::new(trace, config);
    for hosts in 1..=config.hosts.len() {
        for &policy in policies {
            let factors = if policy.runner_limited() || config.overcommit_sweep.is_empty() {
                vec![config.cpu_overcommit]
            } else {
                config.overcommit_sweep.clone()
            };
            for factor in factors {
                let mut scenario = config.clone();
                scenario.cpu_overcommit = factor;
                if !config.overcommit_sweep.is_empty() {
                    scenario.memory_overcommit = factor;
                }
                let outcome = engine::replay(trace, &scenario, policy, hosts)?;
                report.add(
                    trace,
                    policy,
                    hosts,
                    &outcome,
                    (!policy.runner_limited()).then_some(factor),
                );
            }
        }
        if policies.contains(&Policy::Equivalent) {
            report
                .equivalence
                .push(experiments::verify_equivalent(trace, hosts)?);
        }
    }
    Ok(report)
}

/// Invalid configuration, trace or dependency graph.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid input with a privacy-preserving explanation.
    #[error("{0}")]
    Invalid(String),
    /// Invalid resource multipliers.
    #[error(transparent)]
    Admission(#[from] runwell_admission::Error),
    /// Invalid dependencies.
    #[error(transparent)]
    Graph(#[from] runwell_scheduler::GraphError),
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod workflow_tests;

#[cfg(test)]
mod search_tests;

#[cfg(test)]
mod model_tests;

#[cfg(test)]
mod availability_tests;

#[cfg(test)]
mod production_tests;

#[cfg(test)]
mod host_tests;

#[cfg(test)]
mod semaphore_tests;
