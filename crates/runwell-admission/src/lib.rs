//! Pure host admission policy for runwell.
//!
//! Reservations are authoritative; PSI can only reduce admissions. Hysteresis
//! prevents rapid pause/resume cycles. This crate performs no I/O.

#![deny(missing_docs)]

/// Reserved host resources; RAM is measured in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resources {
    /// Reserved CPU slots.
    pub cpu_slots: u32,
    /// Reserved RAM in bytes.
    pub memory_bytes: u64,
}

/// Pressure brake state carried between decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Brake {
    /// Admission may proceed if reservations fit.
    Open,
    /// Admission is paused until the lower recovery threshold is reached.
    Paused,
}

/// A pure reservation and pressure policy.
pub trait AdmissionPolicy {
    /// Decide whether a reservation fits without changing host state.
    fn admit(
        &self,
        available: Resources,
        requested: Resources,
        brake: Brake,
    ) -> Result<bool, Error>;
}

/// An operation that has not been implemented in this milestone.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The public interface is reserved for a later milestone.
    #[error("this operation is not implemented in the M0 bootstrap")]
    Unimplemented,
}
