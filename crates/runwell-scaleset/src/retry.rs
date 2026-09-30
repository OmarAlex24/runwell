//! Explicit retry limits and a pluggable clock for deterministic tests.
use std::{
    future::Future,
    pin::Pin,
    time::{Duration, SystemTime},
};

/// Injectable wall clock and asynchronous sleeper. Fake implementations can advance
/// virtual time instead of waiting. Dropping the returned future cancels a wait.
pub trait Clock: Send + Sync {
    /// Current wall clock time, used for JWT expiry and HTTP-date Retry-After.
    fn now(&self) -> SystemTime;
    /// Sleep without blocking the executor.
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

/// Production clock using Tokio timers.
#[derive(Debug, Default)]
pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep(duration))
    }
}

/// Bounded exponential backoff with equal jitter. Applies only to calls explicitly
/// marked idempotent, or registration propagation failures (401/403).
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Number of additional attempts (default four).
    pub max_retries: u32,
    /// Initial exponential delay (default one second).
    pub initial_delay: Duration,
    /// Maximum exponential delay (default 30 seconds).
    pub max_delay: Duration,
    /// Upper bound for Retry-After (default five minutes).
    pub max_retry_after: Duration,
}
impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 4,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            max_retry_after: Duration::from_secs(300),
        }
    }
}
impl RetryConfig {
    pub(crate) fn delay(&self, attempt: u32) -> Duration {
        let cap = self
            .initial_delay
            .saturating_mul(2_u32.saturating_pow(attempt))
            .min(self.max_delay);
        let bytes = uuid::Uuid::new_v4().into_bytes();
        let fraction = f64::from(u16::from_be_bytes([bytes[0], bytes[1]])) / f64::from(u16::MAX);
        cap.mul_f64(0.5 + fraction * 0.5)
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Policy {
    Never,
    Idempotent,
    RegistrationPropagation,
}
impl Policy {
    pub(crate) fn retries(self, status: reqwest::StatusCode) -> bool {
        match self {
            Self::Never => false,
            Self::Idempotent => {
                status.as_u16() == 429 || (status.is_server_error() && status.as_u16() != 501)
            }
            Self::RegistrationPropagation => matches!(status.as_u16(), 401 | 403),
        }
    }
}
