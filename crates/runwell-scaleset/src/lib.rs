//! GitHub runner scale-set protocol client, based on `actions/scaleset` e6daac7.
//!
//! [`ActionsClient`] handles PAT/App authentication, scale sets and runner
//! registration. [`Session`] owns queue credentials. [`Listener`] implements an
//! async stream of [`Message`] batches with explicit ack-last delivery: acquire
//! chosen jobs, persist/process the entire batch, then call [`Listener::ack`].
//! Initial and recreated-session statistics have no message ID and need no ack.
//! Always await `close` on graceful exit; Drop cleanup is only best effort.
//!
//! Retry policy: GET/DELETE, absolute scale-set PATCH, session renewal and
//! idempotent acquirejobs may retry 429, 5xx except 501, and transient transport
//! failures. Creation/JIT/token-minting POSTs never retry ambiguous failures.
//! Registration exchange alone retries propagation 401/403. Admin/queue 401s
//! trigger single-flight refresh and one replay, including otherwise unsafe POSTs
//! because explicit authentication rejection indicates no successful mutation.
//!
//! Differences from Go HEAD: admin 401 recovery; unknown envelopes are skipped
//! but still require explicit ack; every poll includes lastMessageId (including
//! zero); every request adds api-version; bounded jittered retries preserve final
//! statuses; session 409 backoff and 404 recreation; ownership-checked JIT recovery.
//! No wire-schema disagreements with the research spec were found.
//!
//! Live-service verification is still pending for all endpoints, GHES routing and
//! label behavior, token propagation/expiry/revocation, session conflict lifetime,
//! 50-second polling, server redelivery, 404 session recreation, acquisition
//! idempotency, JIT collisions, busy-runner removal, and Retry-After behavior.
//! Contract tests use synthetic fixtures derived from Go structs/tests, not live
//! service captures. The historical acquirablejobs endpoint is not implemented.
//!
//! Process supervision, local flock/registry, runner health checks, upgrades and
//! host resource calibration belong to the daemon, not this HTTP client. Demand
//! derives from absolute assigned-job statistics, never the at-most-50 events.
//! Runner execution belongs to runwell-runner. The daemon's durable registry
//! handles startup reconciliation and DELETE-first scale-down.
#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod api;
pub mod auth;
mod config;
mod error;
pub mod events;
pub mod listener;
pub mod retry;
mod secret;
pub mod session;
mod transport;
pub mod types;

pub use api::ActionsClient;
pub use auth::Credentials;
pub use config::Config;
pub use error::Error;
pub use events::{Event, Job, JobAssigned, JobAvailable, JobCompleted, JobStarted, Message};
pub use listener::Listener;
pub use secret::Secret;
pub use session::Session;
pub use types::*;

/// Desired runners from absolute assigned jobs, with overflow-safe saturation.
/// If min exceeds max, the configured maximum still wins.
pub fn desired_runners(min: u32, max: u32, total_assigned: u32) -> u32 {
    min.saturating_add(total_assigned).min(max)
}

#[cfg(test)]
mod tests {
    #[test]
    fn desired_runners_uses_absolute_assigned_jobs_and_saturates() {
        assert_eq!(super::desired_runners(2, 10, 3), 5);
        assert_eq!(super::desired_runners(2, 10, 50), 10);
        assert_eq!(super::desired_runners(5, 2, 0), 2);
        assert_eq!(super::desired_runners(u32::MAX, u32::MAX, 1), u32::MAX);
    }
    #[test]
    fn secret_formatting_redacts() {
        let secret = super::Secret::new("test-credential");
        assert_eq!(format!("{secret} {secret:?}"), "[REDACTED] [REDACTED]");
    }
}
