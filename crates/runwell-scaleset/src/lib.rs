//! GitHub Actions scale-set protocol boundaries for runwell.
//!
//! One message session belongs to each class. Handling is idempotent and finishes
//! before acknowledgment; queue and admin tokens are distinct. Modules mirror the
//! proposal in docs/research/SCALESET_PROTOCOL.md and contain no protocol logic yet.

#![deny(missing_docs)]

pub mod api;
pub mod auth;
pub mod listener;
pub mod runner;
pub mod supervisor;

/// Scale-set protocol client boundary, reserved for the protocol port.
pub trait ProtocolClient {
    /// Open the single session for a scale set.
    fn open_session(&self, scale_set_id: i64) -> Result<(), Error>;
}

/// An operation that has not been implemented in this milestone.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The public interface is reserved for a later milestone.
    #[error("this operation is not implemented in the M0 bootstrap")]
    Unimplemented,
}
