//! Production controller policy, telemetry and automatic retry controls.
use crate::Error;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, SocketAddr},
};

/// Optional settings under `[controller.production]`; old configurations retain safe defaults.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProductionConfig {
    /// Plain HTTP Prometheus listener; loopback by default.
    pub metrics_listen: SocketAddr,
    /// Automatic retry is explicitly opt-in.
    pub retry_enabled: bool,
    /// Maximum failed jobs automatically retried per repository per UTC day.
    pub retry_daily_cap: u32,
    /// Aging protection bound in seconds.
    pub aging_seconds: u64,
    /// Repository dispatch weights, canonical owner/repository keys.
    pub repository_weights: BTreeMap<String, u32>,
    /// Optional HTTPS JSON alert receiver.
    pub alert_webhook: Option<String>,
    /// Remote observation and alert evaluation cadence.
    pub tick_seconds: u64,
    /// REST API base; override for GitHub Enterprise Server.
    pub github_api_base: String,
}
impl Default for ProductionConfig {
    fn default() -> Self {
        Self {
            metrics_listen: (Ipv4Addr::LOCALHOST, 9090).into(),
            retry_enabled: false,
            retry_daily_cap: 100,
            aging_seconds: 300,
            repository_weights: BTreeMap::new(),
            alert_webhook: None,
            tick_seconds: 5,
            github_api_base: "https://api.github.com/".into(),
        }
    }
}
impl ProductionConfig {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.aging_seconds == 0
            || !(1..=300).contains(&self.tick_seconds)
            || self.repository_weights.iter().any(|(repo, weight)| {
                *weight == 0 || !crate::valid_repository(repo) || repo.to_lowercase() != *repo
            })
        {
            return Err(Error::Validation(
                "invalid production scheduling settings".into(),
            ));
        }
        Ok(())
    }
}
