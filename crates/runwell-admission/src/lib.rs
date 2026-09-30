//! Pure reservation admission shared by the daemon and simulator.
#![deny(missing_docs)]

/// Reserved host resources; RAM is measured in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Resources {
    /// Reserved CPU cores.
    pub cpu_slots: u32,
    /// Reserved RAM in bytes.
    pub memory_bytes: u64,
}

/// Pressure brake state carried between decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Brake {
    /// Admission may proceed if reservations fit.
    Open,
    /// Admission is paused.
    Paused,
}

/// A pure reservation and pressure policy.
pub trait AdmissionPolicy {
    /// Decide whether a reservation fits in already scaled headroom.
    fn admit(
        &self,
        available: Resources,
        requested: Resources,
        brake: Brake,
    ) -> Result<bool, Error>;
}

/// Capacity multiplier, applied independently to CPU and RAM.
#[derive(Debug, Clone, Copy)]
pub struct ReservationAdmission {
    cpu_factor: f64,
    memory_factor: f64,
}

impl ReservationAdmission {
    /// Reject nonpositive or nonfinite multipliers.
    pub fn new(cpu_factor: f64, memory_factor: f64) -> Result<Self, Error> {
        if [cpu_factor, memory_factor]
            .iter()
            .any(|v| !v.is_finite() || *v <= 0.0)
        {
            return Err(Error::InvalidFactor);
        }
        Ok(Self {
            cpu_factor,
            memory_factor,
        })
    }

    /// Round down fractional capacity so a reservation never exceeds the limit.
    pub fn limit(&self, capacity: Resources) -> Resources {
        Resources {
            cpu_slots: (f64::from(capacity.cpu_slots) * self.cpu_factor).floor() as u32,
            memory_bytes: (capacity.memory_bytes as f64 * self.memory_factor).floor() as u64,
        }
    }

    /// Check the full reservation, including an already overcommitted snapshot.
    pub fn fits(&self, capacity: Resources, used: Resources, request: Resources) -> bool {
        let limit = self.limit(capacity);
        used.cpu_slots
            .checked_add(request.cpu_slots)
            .is_some_and(|v| v <= limit.cpu_slots)
            && used
                .memory_bytes
                .checked_add(request.memory_bytes)
                .is_some_and(|v| v <= limit.memory_bytes)
    }
}

impl AdmissionPolicy for ReservationAdmission {
    fn admit(
        &self,
        available: Resources,
        requested: Resources,
        brake: Brake,
    ) -> Result<bool, Error> {
        Ok(brake == Brake::Open
            && requested.cpu_slots <= available.cpu_slots
            && requested.memory_bytes <= available.memory_bytes)
    }
}

/// Invalid admission configuration.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Multipliers must be finite and positive.
    #[error("overcommit factors must be finite and positive")]
    InvalidFactor,
}
