//! Stateful reservation accounting, without host I/O.
use crate::{Brake, Error, ReservationAdmission, Resources};
use std::collections::BTreeMap;

/// Authoritative per-host reservation ledger. Recovered jobs retain reservations.
pub struct HostAdmission {
    capacity: Resources,
    policy: ReservationAdmission,
    max_jobs: u32,
    reservations: BTreeMap<u64, Resources>,
}
impl HostAdmission {
    /// Build a ledger with an explicit concurrency ceiling.
    pub fn new(capacity: Resources, policy: ReservationAdmission, max_jobs: u32) -> Self {
        Self {
            capacity,
            policy,
            max_jobs,
            reservations: BTreeMap::new(),
        }
    }
    /// Sum all reservations, saturating conservatively on corrupt/oversized recovery.
    pub fn used(&self) -> Resources {
        self.reservations
            .values()
            .fold(Resources::default(), |a, b| Resources {
                cpu_slots: a.cpu_slots.saturating_add(b.cpu_slots),
                memory_bytes: a.memory_bytes.saturating_add(b.memory_bytes),
            })
    }
    /// Whether one more job can fit. Zero-sized reservations are rejected.
    pub fn can_admit(&self, request: Resources, brake: Brake) -> bool {
        brake == Brake::Open
            && request.cpu_slots > 0
            && request.memory_bytes > 0
            && self.reservations.len() < self.max_jobs as usize
            && self.fits(request)
    }
    fn fits(&self, request: Resources) -> bool {
        let limit = self.policy.conservative_limit(self.capacity);
        let used = self.used();
        used.cpu_slots
            .checked_add(request.cpu_slots)
            .is_some_and(|v| v <= limit.cpu_slots)
            && used
                .memory_bytes
                .checked_add(request.memory_bytes)
                .is_some_and(|v| v <= limit.memory_bytes)
    }
    /// Reserve once; repeated identical reservations are harmless.
    pub fn reserve(&mut self, id: u64, request: Resources, brake: Brake) -> bool {
        if let Some(previous) = self.reservations.get(&id) {
            return *previous == request;
        }
        if !self.can_admit(request, brake) {
            return false;
        }
        self.reservations.insert(id, request);
        true
    }
    /// Restore a durable reservation even if a changed host budget is now smaller.
    pub fn restore(&mut self, id: u64, resources: Resources) {
        self.reservations.insert(id, resources);
    }
    /// Release only after local cleanup and remote deletion have succeeded.
    pub fn release(&mut self, id: u64) {
        self.reservations.remove(&id);
    }
    /// Realizable additional class capacity, bounded by the job ceiling.
    pub fn available_jobs(&self, request: Resources, brake: Brake) -> u32 {
        if !self.can_admit(request, brake) {
            return 0;
        }
        let used = self.used();
        let limit = self.policy.conservative_limit(self.capacity);
        let cpu = (limit.cpu_slots - used.cpu_slots) / request.cpu_slots;
        let mem = (limit.memory_bytes - used.memory_bytes) / request.memory_bytes;
        cpu.min(mem.min(u64::from(u32::MAX)) as u32)
            .min(self.max_jobs.saturating_sub(self.reservations.len() as u32))
    }
}

/// Per-resource PSI some avg10 values, and memory full avg10, in percent.
#[derive(Debug, Clone, Copy, Default)]
pub struct Pressure {
    /// CPU some avg10.
    pub cpu: f64,
    /// Memory some avg10.
    pub memory: f64,
    /// I/O some avg10.
    pub io: f64,
    /// Memory full avg10: any positive value is an immediate admission veto.
    pub memory_full: f64,
}
/// Independent high/low thresholds for a PSI resource.
#[derive(Debug, Clone, Copy)]
pub struct Threshold {
    /// Pause threshold (inclusive).
    pub high: f64,
    /// Recovery threshold (exclusive).
    pub low: f64,
}
/// Hysteresis state with minimum dwell in both states and sustained recovery.
/// High pressure, memory-full and invalid samples veto admission immediately without
/// changing the dwell-limited state; the pending pause is latched until the state can transition.
pub struct PsiBrake {
    thresholds: [Threshold; 3],
    dwell_ms: u64,
    state: Brake,
    changed_at: u64,
    recovering_since: Option<u64>,
    hard: bool,
    pending_pause: bool,
}
impl PsiBrake {
    /// Validate thresholds. Time inputs are monotonic milliseconds from one epoch.
    pub fn new(thresholds: [Threshold; 3], dwell_ms: u64) -> Result<Self, Error> {
        if dwell_ms == 0
            || thresholds.iter().any(|t| {
                !t.low.is_finite()
                    || !t.high.is_finite()
                    || t.low < 0.0
                    || t.low >= t.high
                    || t.high > 100.0
            })
        {
            return Err(Error::InvalidPressure);
        }
        Ok(Self {
            thresholds,
            dwell_ms,
            state: Brake::Open,
            changed_at: 0,
            recovering_since: None,
            hard: false,
            pending_pause: false,
        })
    }
    /// Advance the hysteresis state. A backwards clock fails closed.
    pub fn update(&mut self, now_ms: u64, sample: Pressure) -> Brake {
        let values = [sample.cpu, sample.memory, sample.io];
        self.hard = sample.memory_full > 0.0
            || !sample.memory_full.is_finite()
            || sample.memory_full < 0.0
            || now_ms < self.changed_at
            || values
                .iter()
                .any(|v| !v.is_finite() || !(0.0..=100.0).contains(v));
        let high = self.hard
            || values
                .iter()
                .zip(self.thresholds)
                .any(|(v, t)| *v >= t.high);
        self.pending_pause |= high;
        let low = !self.hard && values.iter().zip(self.thresholds).all(|(v, t)| *v < t.low);
        if low {
            self.recovering_since.get_or_insert(now_ms);
        } else {
            self.recovering_since = None;
        }
        if now_ms.saturating_sub(self.changed_at) >= self.dwell_ms {
            let recover = self
                .recovering_since
                .is_some_and(|t| now_ms.saturating_sub(t) >= self.dwell_ms);
            let next = match self.state {
                Brake::Open if self.pending_pause => Brake::Paused,
                Brake::Paused if recover => Brake::Open,
                state => state,
            };
            if next != self.state {
                self.changed_at = now_ms;
                self.state = next;
                self.pending_pause = false;
            }
        }
        self.decision()
    }
    /// Dwell-limited state, excluding the immediate memory-full veto.
    pub fn state(&self) -> Brake {
        self.state
    }
    /// Effective admission decision, including the immediate hard brake.
    pub fn decision(&self) -> Brake {
        if self.hard || self.pending_pause {
            Brake::Paused
        } else {
            self.state
        }
    }
}

impl ReservationAdmission {
    /// Exact integer scaling for the host ledger, avoiding large-u64 conversion
    /// rounding. The original simulator limit/fits API retains its behavior.
    pub fn conservative_limit(&self, capacity: Resources) -> Resources {
        Resources {
            cpu_slots: scaled_floor(u64::from(capacity.cpu_slots), self.cpu_factor)
                .min(u64::from(u32::MAX)) as u32,
            memory_bytes: scaled_floor(capacity.memory_bytes, self.memory_factor),
        }
    }
}
// Multiply by the exact binary value of the validated f64 using integers. Casting
// a large u64 capacity to f64 first could round UP even at overcommit = 1.0.
fn scaled_floor(capacity: u64, factor: f64) -> u64 {
    let bits = factor.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let mantissa = (bits & ((1_u64 << 52) - 1)) | if exponent == 0 { 0 } else { 1_u64 << 52 };
    let product = u128::from(capacity) * u128::from(mantissa);
    if product == 0 {
        return 0;
    }
    let shift = if exponent == 0 {
        -1074
    } else {
        exponent - 1023 - 52
    };
    let value = if shift < 0 {
        product.checked_shr((-shift) as u32).unwrap_or(0)
    } else if shift >= 128 || product > (u128::from(u64::MAX) >> shift) {
        return u64::MAX;
    } else {
        product << shift
    };
    value.min(u128::from(u64::MAX)) as u64
}
