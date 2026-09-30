//! Linux host execution, systemd, cgroup v2, and PSI integration for runwell.
//!
//! Systemd owns cgroup configuration. Reservations precede runner creation; final
//! measurements precede teardown. Restart reconciliation must preserve live jobs.
//! Linux operations are unavailable on other targets.

#![deny(missing_docs)]

/// Limits for a systemd transient job slice under ci.slice.
#[derive(Debug, Clone, Copy)]
pub struct SliceSpec {
    /// Numeric job identity used in rw-j<id>.slice.
    pub job_id: u64,
    /// Memory throttling threshold in bytes.
    pub memory_high: u64,
    /// Hard memory ceiling in bytes.
    pub memory_max: u64,
    /// CPU scheduling weight.
    pub cpu_weight: u32,
}

/// Cgroup counters sampled before the slice disappears.
#[derive(Debug, Clone, Copy, Default)]
pub struct JobStats {
    /// Cumulative CPU usage in microseconds.
    pub cpu_usec: u64,
    /// Peak resident memory in bytes.
    pub memory_peak: u64,
    /// Number of kernel OOM kills.
    pub oom_kills: u64,
}

/// Host lifecycle interface; implementations must reconcile before admission.
pub trait NodeBackend {
    /// Reconcile surviving units, mounts, and containers with the durable registry.
    fn reconcile(&self) -> Result<(), Error>;
    /// Stop admission while allowing running jobs to finish.
    fn drain(&self) -> Result<(), Error>;
}

#[cfg(target_os = "linux")]
pub mod linux;

/// An operation that has not been implemented in this milestone.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The public interface is reserved for a later milestone.
    #[error("this operation is not implemented in the M0 bootstrap")]
    Unimplemented,
}
