use crate::Error;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
/// Thresholds for pure in-process alert evaluation. Times are UNIX seconds.
#[derive(Debug, Clone)]
pub struct AlertConfig {
    /// Rolling completion window.
    pub window_seconds: u64,
    /// Infra fraction; default 0.01, alerting strictly above it.
    pub infra_threshold: f64,
    /// Minimum completions before evaluating the fraction.
    pub minimum_completions: usize,
    /// Queued or running job age divided by its learned p90.
    pub stuck_p90_multiple: f64,
    /// Maximum age of a node report.
    pub heartbeat_seconds: u64,
    /// Alert this long before a runner template's enforced expiry.
    pub template_lead_seconds: u64,
}
impl Default for AlertConfig {
    fn default() -> Self {
        Self {
            window_seconds: 3600,
            infra_threshold: 0.01,
            minimum_completions: 1,
            stuck_p90_multiple: 3.0,
            heartbeat_seconds: 90,
            template_lead_seconds: 3 * 86_400,
        }
    }
}
/// One completion; stable identity prevents duplicate deliveries inflating rates.
#[derive(Debug, Clone)]
pub struct Completion {
    /// Repository/job/attempt identity for deduplication, never a metric label.
    pub id: String,
    /// Authoritative completion timestamp.
    pub at: u64,
    /// Confirmed infrastructure classification.
    pub infra: bool,
}
/// Pending or running job watch (use a separate phase identity if watching both).
#[derive(Debug, Clone)]
pub struct JobWatch {
    /// Stable job/attempt/phase identifier.
    pub id: String,
    /// Ready time for queue monitoring, start time for execution monitoring.
    pub since: u64,
    /// Learned or class-default p90 seconds.
    pub p90_seconds: f64,
}
/// Configured node, including nodes that have never reported.
#[derive(Debug, Clone)]
pub struct NodeWatch {
    /// Configured stable node identity.
    pub id: String,
    /// Last heartbeat, or registration time if no report has arrived.
    pub last_report: u64,
}
/// Runner template validity; version identity allows replacement to resolve alerts.
#[derive(Debug, Clone)]
pub struct TemplateWatch {
    /// Stable template version.
    pub version: String,
    /// Absolute enforced expiry, not just a download timestamp.
    pub expires_at: u64,
}
/// Immutable alert inputs; callers can load them from their durable event journal.
#[derive(Debug, Clone, Default)]
pub struct AlertSnapshot {
    /// Completion history (filtered to the configured rolling window).
    pub completions: Vec<Completion>,
    /// Jobs still queued/running.
    pub jobs: Vec<JobWatch>,
    /// Expected nodes, including offline nodes.
    pub nodes: Vec<NodeWatch>,
    /// Templates available for new work.
    pub templates: Vec<TemplateWatch>,
}
impl AlertSnapshot {
    /// Rolling infra fraction, deduplicated by execution identity. Future events
    /// and events at/before the lower window boundary do not contribute.
    pub fn infra_ratio(&self, now: u64, window_seconds: u64) -> (usize, f64) {
        let mut samples = BTreeMap::new();
        for c in &self.completions {
            if c.at <= now && (now < window_seconds || c.at > now - window_seconds) {
                samples
                    .entry(&c.id)
                    .and_modify(|infra: &mut bool| *infra |= c.infra)
                    .or_insert(c.infra);
            }
        }
        let total = samples.len();
        (
            total,
            if total == 0 {
                0.0
            } else {
                samples.values().filter(|&&v| v).count() as f64 / total as f64
            },
        )
    }
}
/// Finite webhook rule names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    /// Rolling infra failure fraction exceeded its threshold.
    InfraFailureRate,
    /// Queue/execution age exceeded a multiple of the job's p90.
    StuckJob,
    /// Expected node is not reporting.
    NodeMissing,
    /// Runner template is near expiry or already expired.
    TemplateExpiry,
}
/// Generic JSON webhook payload; contains no logs, credentials or annotations.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Alert {
    /// Versioned payload contract.
    pub schema_version: u32,
    /// Stable deduplication key, unchanged across retries/cooldown reminders.
    pub key: String,
    /// Evaluated rule.
    pub kind: AlertKind,
    /// Node/job/template identity or `fleet`.
    pub subject: String,
    /// Time this evaluation observed the condition.
    pub observed_at: u64,
    /// Current measured value (fraction or seconds).
    pub value: f64,
    /// Configured threshold in the same units.
    pub threshold: f64,
}
/// Evaluate rules without I/O. Delivery deduplication/cooldown belongs to Webhook.
pub fn evaluate(
    config: &AlertConfig,
    snapshot: &AlertSnapshot,
    now: u64,
) -> Result<Vec<Alert>, Error> {
    if config.window_seconds == 0
        || config.minimum_completions == 0
        || config.heartbeat_seconds == 0
        || !config.infra_threshold.is_finite()
        || !(0.0..=1.0).contains(&config.infra_threshold)
        || !config.stuck_p90_multiple.is_finite()
        || config.stuck_p90_multiple <= 0.0
    {
        return Err(Error::Invalid);
    }
    let mut alerts = Vec::new();
    let mut push = |kind, subject: &str, value, threshold| {
        alerts.push(Alert {
            schema_version: 1,
            key: format!("{kind:?}:{subject}"),
            kind,
            subject: subject.into(),
            observed_at: now,
            value,
            threshold,
        })
    };
    let (total, ratio) = snapshot.infra_ratio(now, config.window_seconds);
    if total >= config.minimum_completions && ratio > config.infra_threshold {
        push(
            AlertKind::InfraFailureRate,
            "fleet",
            ratio,
            config.infra_threshold,
        );
    }
    for job in &snapshot.jobs {
        if !job.p90_seconds.is_finite() || job.p90_seconds <= 0.0 {
            return Err(Error::Invalid);
        }
        let threshold = job.p90_seconds * config.stuck_p90_multiple;
        if !threshold.is_finite() {
            return Err(Error::Invalid);
        }
        let age = now.saturating_sub(job.since) as f64;
        if age > threshold {
            push(AlertKind::StuckJob, &job.id, age, threshold);
        }
    }
    for node in &snapshot.nodes {
        let age = now.saturating_sub(node.last_report);
        if age > config.heartbeat_seconds {
            push(
                AlertKind::NodeMissing,
                &node.id,
                age as f64,
                config.heartbeat_seconds as f64,
            );
        }
    }
    for template in &snapshot.templates {
        if template.expires_at <= now.saturating_add(config.template_lead_seconds) {
            push(
                AlertKind::TemplateExpiry,
                &template.version,
                template.expires_at.saturating_sub(now) as f64,
                config.template_lead_seconds as f64,
            );
        }
    }
    let mut seen = BTreeSet::new();
    alerts.retain(|a| seen.insert(a.key.clone()));
    alerts.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(alerts)
}
