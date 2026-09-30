use reqwest::StatusCode;

/// Protocol, authentication, and transport failures. Response bodies are deliberately
/// excluded: upstream errors can echo credentials. Endpoint paths omit query strings.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An HTTP failure, including the final status after exhausted retries.
    #[error("{endpoint}: HTTP {status} ({error_type:?})")]
    Http {
        /// Final HTTP status.
        status: StatusCode,
        /// Endpoint path without potentially sensitive query parameters.
        endpoint: String,
        /// GitHub's exception type, when present.
        error_type: Option<String>,
        /// Actions service correlation ID.
        activity_id: Option<String>,
        /// GitHub REST correlation ID.
        request_id: Option<String>,
    },
    /// A request failed before a complete response arrived.
    #[error("transport failure at {endpoint}: {source}")]
    Transport {
        /// Endpoint path.
        endpoint: String,
        /// Original transport error with URL removed.
        source: reqwest::Error,
    },
    /// A malformed or incomplete response; raw payloads are never retained.
    #[error("invalid response at {endpoint}: {detail}")]
    Protocol {
        /// Endpoint path.
        endpoint: String,
        /// Static explanation safe to log.
        detail: &'static str,
    },
    /// Invalid local configuration.
    #[error("invalid configuration: {0}")]
    Config(&'static str),
    /// Session changed; reprocess initial statistics and await redelivery.
    #[error("message session was recreated; previous message IDs are no longer valid")]
    SessionRecreated,
    /// A caller polled for another batch before acknowledging the current delivery.
    #[error("acknowledge the pending delivery before polling for another batch")]
    AckRequired,
    /// A caller tried to acknowledge a message other than its pending delivery.
    #[error("message is not the pending delivery")]
    InvalidAck,
}

impl Error {
    /// HTTP status, preserved even when retries are exhausted.
    pub fn status(&self) -> Option<StatusCode> {
        match self {
            Self::Http { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// Whether the server supplied this exception (assembly suffixes are permitted).
    pub fn is_type(&self, name: &str) -> bool {
        matches!(self, Self::Http { error_type: Some(kind), .. } if kind.contains(name))
    }

    pub(crate) fn protocol(endpoint: impl Into<String>, detail: &'static str) -> Self {
        Self::Protocol {
            endpoint: endpoint.into(),
            detail,
        }
    }
}
