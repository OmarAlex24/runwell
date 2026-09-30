//! Prometheus live metrics for runwell.
//!
//! Live gauges and histograms complement durable per-job SQLite records. Secrets
//! and unbounded job or repository labels must never be exposed in metrics.

#![deny(missing_docs)]

/// OpenMetrics registry owned by the daemon.
#[derive(Debug, Default)]
pub struct Metrics {
    registry: prometheus_client::registry::Registry,
}

impl Metrics {
    /// Access the registry for bounded-cardinality metric registration.
    pub fn registry(&mut self) -> &mut prometheus_client::registry::Registry {
        &mut self.registry
    }

    /// Encode the registered metrics; export wiring is currently unimplemented.
    pub fn encode(&self) -> Result<String, Error> {
        Err(Error::Unimplemented)
    }
}

/// An operation that has not been implemented in this milestone.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The public interface is reserved for a later milestone.
    #[error("this operation is not implemented in the M0 bootstrap")]
    Unimplemented,
}
