use crate::{Alert, Error};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
/// Bounded webhook delivery controls, in caller-clock seconds.
#[derive(Debug, Clone)]
pub struct WebhookConfig {
    /// Minimum interval between successful notifications/reminders for one key.
    pub cooldown_seconds: u64,
    /// First retry delay; subsequent delays double up to max_backoff_seconds.
    pub initial_backoff_seconds: u64,
    /// Maximum retry delay.
    pub max_backoff_seconds: u64,
    /// Maximum sends per notification, including its first send.
    pub max_attempts: u32,
    /// Maximum distinct active/cooling alert keys.
    pub max_alerts: usize,
    /// Timeout for one HTTP request.
    pub timeout_seconds: u64,
}
impl Default for WebhookConfig {
    fn default() -> Self {
        Self {
            cooldown_seconds: 600,
            initial_backoff_seconds: 5,
            max_backoff_seconds: 300,
            max_attempts: 5,
            max_alerts: 4096,
            timeout_seconds: 10,
        }
    }
}
struct Pending {
    alert: Alert,
    attempts: u32,
    due: u64,
}
/// Progress from one non-sleeping delivery tick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeliveryReport {
    /// Accepted by webhook (any 2xx).
    pub sent: usize,
    /// Temporary failures scheduled for later.
    pub deferred: usize,
    /// Permanent failures or exhausted attempts (cooldown applies).
    pub exhausted: usize,
}
/// Single-owner in-process outbox. Call `sync` with the complete current alert set,
/// then `deliver_due` on controller ticks. No sleeps hold the controller loop.
/// The receiver should dedup `Idempotency-Key`: a lost response can cause redelivery.
/// State is in-process; restart can resend, so receivers must retain their keys.
pub struct Webhook {
    config: WebhookConfig,
    url: reqwest::Url,
    http: reqwest::Client,
    pending: BTreeMap<String, Pending>,
    cooldown: BTreeMap<String, u64>,
}
impl Webhook {
    /// HTTPS endpoint (loopback HTTP for tests), with no redirects or implicit retries.
    pub fn new(url: reqwest::Url, config: WebhookConfig) -> Result<Self, Error> {
        let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if !(url.scheme() == "https" || (url.scheme() == "http" && local))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || config.cooldown_seconds == 0
            || config.initial_backoff_seconds == 0
            || config.max_backoff_seconds < config.initial_backoff_seconds
            || !(1..=8).contains(&config.max_attempts)
            || config.max_alerts == 0
            || config.timeout_seconds == 0
            || config.timeout_seconds > 60
        {
            return Err(Error::Invalid);
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_secs(config.timeout_seconds))
            .build()
            .map_err(|_| Error::Invalid)?;
        Ok(Self {
            config,
            url,
            http,
            pending: BTreeMap::new(),
            cooldown: BTreeMap::new(),
        })
    }
    /// Deduplicate active alerts, cancel resolved retries, and apply cooldown even
    /// to conditions that flap. Refuses overflow before modifying the outbox.
    pub fn sync(&mut self, alerts: &[Alert], now: u64) -> Result<(), Error> {
        let keys: BTreeSet<_> = alerts.iter().map(|a| a.key.clone()).collect();
        let cooling: BTreeSet<_> = self
            .cooldown
            .iter()
            .filter(|(_, until)| **until > now)
            .map(|(k, _)| k.clone())
            .collect();
        if keys.union(&cooling).count() > self.config.max_alerts
            || alerts
                .iter()
                .any(|a| a.key.is_empty() || !a.value.is_finite() || !a.threshold.is_finite())
        {
            return Err(Error::Invalid);
        }
        self.cooldown.retain(|_, until| *until > now);
        self.pending.retain(|key, _| keys.contains(key));
        for alert in alerts {
            if !self.cooldown.contains_key(&alert.key) {
                self.pending
                    .entry(alert.key.clone())
                    .or_insert_with(|| Pending {
                        alert: alert.clone(),
                        attempts: 0,
                        due: now,
                    });
            }
        }
        Ok(())
    }
    /// Send at most `limit` due alerts, in key order. Retries only transport errors,
    /// 408, 429 and 5xx; honors numeric Retry-After up to the configured ceiling.
    pub async fn deliver_due(&mut self, now: u64, limit: usize) -> DeliveryReport {
        let keys: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, p)| p.due <= now)
            .take(limit)
            .map(|(k, _)| k.clone())
            .collect();
        let mut report = DeliveryReport::default();
        for key in keys {
            let Some(pending) = self.pending.get_mut(&key) else {
                continue;
            };
            pending.attempts += 1;
            // The observation timestamp stays fixed over HTTP retries but changes
            // for a later cooldown reminder; opaque encoding is safe in headers.
            let id = idempotency_key(&pending.alert);
            let result = self
                .http
                .post(self.url.clone())
                .header("Idempotency-Key", id)
                .json(&pending.alert)
                .send()
                .await;
            let success = result.as_ref().is_ok_and(|r| r.status().is_success());
            let temporary = result.as_ref().map_or(true, |r| {
                r.status().is_server_error() || matches!(r.status().as_u16(), 408 | 429)
            });
            if success || !temporary || pending.attempts >= self.config.max_attempts {
                if success {
                    report.sent += 1;
                } else {
                    report.exhausted += 1;
                }
                self.pending.remove(&key);
                self.cooldown
                    .insert(key, now.saturating_add(self.config.cooldown_seconds));
            } else {
                let backoff = self
                    .config
                    .initial_backoff_seconds
                    .saturating_mul(1_u64 << (pending.attempts - 1))
                    .min(self.config.max_backoff_seconds);
                let retry_after = result
                    .as_ref()
                    .ok()
                    .and_then(|r| r.headers().get("retry-after"))
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0);
                pending.due = now.saturating_add(
                    backoff
                        .max(retry_after)
                        .min(self.config.max_backoff_seconds),
                );
                report.deferred += 1;
            }
        }
        report
    }
    /// Number of queued notifications, useful for shutdown and diagnostics.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}
fn idempotency_key(alert: &Alert) -> String {
    let hex: String = alert.key.bytes().map(|b| format!("{b:02x}")).collect();
    format!("rw-{hex}-{}", alert.observed_at)
}
