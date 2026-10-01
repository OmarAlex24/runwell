//! Bounded Prometheus metrics, in-process alert evaluation and webhook delivery.
#![deny(missing_docs)]
mod alerts;
mod instruments;
mod webhook;
pub use alerts::{
    Alert, AlertConfig, AlertKind, AlertSnapshot, Completion, JobWatch, NodeWatch, TemplateWatch,
    evaluate,
};
pub use instruments::{AdmissionDecision, CompletionKind, Metrics, PressureResource};
pub use webhook::{DeliveryReport, Webhook, WebhookConfig};
/// Sanitized observability errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid bounds, thresholds, endpoint, or labels.
    #[error("invalid observability configuration or sample")]
    Invalid,
    /// Registry text encoding failed.
    #[error("metrics encoding failed")]
    Encoding,
}
