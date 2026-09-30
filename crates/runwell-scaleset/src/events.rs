//! Typed job events and double-encoded queue envelopes.
use crate::{Error, types::Statistics};
use serde::{Deserialize, Deserializer};

/// Shared job identity and scheduling metadata.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Job {
    /// Acquisition/idempotency key.
    pub runner_request_id: i64,
    /// Repository name.
    pub repository_name: String,
    /// Organization or repository owner.
    pub owner_name: String,
    /// Job UUID.
    pub job_id: String,
    /// Workflow reference.
    pub job_workflow_ref: String,
    /// Human-readable job name.
    pub job_display_name: String,
    /// Workflow run identity.
    pub workflow_run_id: i64,
    /// Trigger event name.
    pub event_name: String,
    /// Requested runner labels.
    pub request_labels: Vec<String>,
    /// Queue time; empty and Go zero times become `None`.
    #[serde(deserialize_with = "timestamp")]
    pub queue_time: Option<jiff::Timestamp>,
    /// Scale-set assignment time.
    #[serde(deserialize_with = "timestamp")]
    pub scale_set_assign_time: Option<jiff::Timestamp>,
    /// Runner assignment time.
    #[serde(deserialize_with = "timestamp")]
    pub runner_assign_time: Option<jiff::Timestamp>,
    /// Job completion time.
    #[serde(deserialize_with = "timestamp")]
    pub finish_time: Option<jiff::Timestamp>,
}
fn timestamp<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<jiff::Timestamp>, D::Error> {
    let value = Option::<String>::deserialize(deserializer)?;
    match value.as_deref() {
        None | Some("") => Ok(None),
        Some(s) if s.starts_with("0001-01-01T00:00:00") => Ok(None),
        Some(s) => s
            .parse()
            .map(Some)
            .map_err(|_| serde::de::Error::custom("invalid job timestamp")),
    }
}

/// A job which must be explicitly acquired before acknowledgment.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobAvailable {
    /// Shared identity.
    #[serde(flatten)]
    pub job: Job,
    /// Advisory URL; the client uses the authenticated scale-set acquisition endpoint.
    #[serde(default)]
    pub acquire_job_url: String,
}
/// Informational assignment to the scale set.
#[derive(Clone, Debug, Deserialize)]
pub struct JobAssigned {
    /// Shared identity.
    #[serde(flatten)]
    pub job: Job,
}
/// A runner started executing a job.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobStarted {
    /// Shared identity.
    #[serde(flatten)]
    pub job: Job,
    /// Runner identity.
    #[serde(default)]
    pub runner_id: i64,
    /// Runner name.
    #[serde(default)]
    pub runner_name: String,
}
/// Completion, potentially before a runner was assigned (zero ID/empty name).
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobCompleted {
    /// Shared identity.
    #[serde(flatten)]
    pub job: Job,
    /// Result such as succeeded, failed or canceled.
    #[serde(default)]
    pub result: String,
    /// Runner ID, or zero when never assigned.
    #[serde(default)]
    pub runner_id: i64,
    /// Runner name, or empty when never assigned.
    #[serde(default)]
    pub runner_name: String,
}
/// Known queue events, in their original wire order.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "messageType")]
pub enum Event {
    /// Explicit acquisition required.
    JobAvailable(JobAvailable),
    /// Informational assignment.
    JobAssigned(JobAssigned),
    /// Runner became busy.
    JobStarted(JobStarted),
    /// Job finished.
    JobCompleted(JobCompleted),
}

/// One atomic delivery. Process the entire batch, then explicitly acknowledge its
/// ID. `None` identifies startup/recreation statistics and needs no acknowledgment.
#[derive(Clone, Debug)]
pub struct Message {
    /// Queue identity, including zero (which is a valid ID).
    pub message_id: Option<i64>,
    /// Absolute demand, authoritative even when event arrays are truncated.
    pub statistics: Option<Statistics>,
    /// Decoded known events; unknown types are logged and skipped.
    pub events: Vec<Event>,
    pub(crate) generation: u64,
}
impl Message {
    pub(crate) fn initial(statistics: Statistics, generation: u64) -> Self {
        Self {
            message_id: None,
            statistics: Some(statistics),
            events: Vec::new(),
            generation,
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Envelope {
    message_id: i64,
    message_type: String,
    #[serde(default)]
    body: String,
    statistics: Option<Statistics>,
}
impl Envelope {
    pub fn decode(self, endpoint: &str, generation: u64) -> Result<Message, Error> {
        let mut result = Message {
            message_id: Some(self.message_id),
            statistics: self.statistics,
            events: Vec::new(),
            generation,
        };
        // Unlike Go HEAD (hard error), unknown envelopes remain deliverable for explicit ack.
        if self.message_type != "RunnerScaleSetJobMessages" {
            tracing::warn!(message_type = %self.message_type, "skipping unknown envelope type");
            return Ok(result);
        }
        if self.body.is_empty() {
            return Ok(result);
        }
        let messages: Vec<serde_json::Value> = serde_json::from_slice(crate::transport::strip_bom(
            self.body.as_bytes(),
        ))
        .map_err(|_| Error::protocol(endpoint, "body must be a JSON string containing an array"))?;
        for message in messages {
            match message.get("messageType").and_then(|v| v.as_str()) {
                Some("JobAvailable" | "JobAssigned" | "JobStarted" | "JobCompleted") => {
                    result.events.push(
                        serde_json::from_value(message)
                            .map_err(|_| Error::protocol(endpoint, "invalid known job event"))?,
                    );
                }
                unknown => tracing::warn!(message_type = ?unknown, "skipping unknown job type"),
            }
        }
        Ok(result)
    }
}
