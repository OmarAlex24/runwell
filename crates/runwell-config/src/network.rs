//! Multi-host addresses, peer authorization and resilience deadlines.
use crate::Error;
use serde::Deserialize;
use std::{collections::HashSet, net::SocketAddr};

/// Networked controller/node configuration, independent of standalone operation.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    /// Stable controller certificate identity.
    pub controller_id: String,
    /// Controller report listener.
    pub controller_listen: SocketAddr,
    /// Address used by nodes to register with the controller.
    pub controller_address: String,
    /// This node's command listener.
    pub node_listen: SocketAddr,
    /// Authorized nodes; certificates alone do not grant membership.
    pub nodes: Vec<PeerNode>,
    /// Per-attempt RPC deadline in seconds.
    pub rpc_seconds: u64,
    /// Bounded retries for ambiguous transport failures.
    pub rpc_attempts: u32,
    /// Periodic node reporting interval.
    pub report_seconds: u64,
    /// Report silence beyond this marks a node lost.
    pub lost_seconds: u64,
    /// Deadline for admission and preparation, independent of execution duration.
    #[serde(default = "preparation_seconds")]
    pub preparation_seconds: u64,
    /// Expected job duration until learned estimates are integrated.
    pub expected_seconds: u64,
    /// Watchdog threshold as a multiple of expected duration.
    pub watchdog_multiple: u32,
    /// No runner job-renewal heartbeat beyond this interval triggers the watchdog.
    pub heartbeat_seconds: u64,
}
/// A configured member and its command address.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerNode {
    /// Stable node SAN identity.
    pub id: String,
    /// Host:port, resolved when connecting; TLS name is derived from the identity.
    pub address: String,
    /// Optional scheduler host class.
    #[serde(default)]
    pub class: String,
}
/// Strict identity grammar, also safe as a certificate filename and DNS label.
pub fn valid_identity(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 63
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !id.starts_with('-')
        && !id.ends_with('-')
}
impl NetworkConfig {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        let mut ids = HashSet::new();
        if !valid_identity(&self.controller_id)
            || self.controller_address.is_empty()
            || self.nodes.is_empty()
            || self
                .nodes
                .iter()
                .any(|n| !valid_identity(&n.id) || n.address.is_empty() || !ids.insert(&n.id))
            || !(1..=300).contains(&self.rpc_seconds)
            || !(1..=8).contains(&self.rpc_attempts)
            || !(1..=3600).contains(&self.report_seconds)
            || !(1..=604800).contains(&self.lost_seconds)
            || self.lost_seconds <= self.report_seconds
            || !(1..=604800).contains(&self.heartbeat_seconds)
            || self.heartbeat_seconds <= self.report_seconds
            || !(1..=604800).contains(&self.preparation_seconds)
            || !(1..=604800).contains(&self.expected_seconds)
            || !(1..=1000).contains(&self.watchdog_multiple)
        {
            return Err(Error::Validation(
                "invalid network identities or resilience deadlines".into(),
            ));
        }
        Ok(())
    }
}

fn preparation_seconds() -> u64 {
    3600
}
