use std::fmt;

use serde::{Deserialize, Serialize};
use xai_grok_sampling_types::rs;

use crate::events::SamplingEvent;
use crate::terminal::{NamedEvent, OutputPresence, TerminalFacts, TerminalReason};

/// Terminal fields captured from the raw Responses JSON before compatibility
/// normalization can fill omitted values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawTerminalEnvelope {
    pub(crate) dialect: Option<String>,
    pub(crate) named_event: NamedEvent,
    pub(crate) status: Option<String>,
    pub(crate) reason: Option<TerminalReason>,
    pub(crate) raw_output_presence: OutputPresence,
    pub(crate) response_id: Option<String>,
    pub(crate) response_model: Option<String>,
}

/// Internal decoder carrier. It is public only because it is an opaque variant
/// payload of the public [`ResponsesStreamItem`]; its terminal metadata remains
/// crate-private.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct DecodedResponsesFrame {
    event: rs::ResponseStreamEvent,
    raw_terminal: Option<RawTerminalEnvelope>,
}

impl DecodedResponsesFrame {
    pub(crate) fn new(
        event: rs::ResponseStreamEvent,
        raw_terminal: Option<RawTerminalEnvelope>,
    ) -> Self {
        Self {
            event,
            raw_terminal,
        }
    }

    pub fn event(&self) -> &rs::ResponseStreamEvent {
        &self.event
    }

    pub fn into_event(self) -> rs::ResponseStreamEvent {
        self.event
    }

    pub(crate) fn into_parts(self) -> (rs::ResponseStreamEvent, Option<RawTerminalEnvelope>) {
        (self.event, self.raw_terminal)
    }
}

/// One item received from a Responses API SSE stream.
///
/// Auxiliary transport frames carry liveness but no semantic model output.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum ResponsesStreamItem {
    Event(rs::ResponseStreamEvent),
    Heartbeat,
    #[doc(hidden)]
    Decoded(DecodedResponsesFrame),
}

impl From<rs::ResponseStreamEvent> for ResponsesStreamItem {
    fn from(event: rs::ResponseStreamEvent) -> Self {
        Self::Event(event)
    }
}

impl ResponsesStreamItem {
    /// Borrow the typed event carried by this item, if it is not a heartbeat.
    pub fn event(&self) -> Option<&rs::ResponseStreamEvent> {
        match self {
            Self::Event(event) => Some(event),
            Self::Decoded(frame) => Some(frame.event()),
            Self::Heartbeat => None,
        }
    }

    /// Consume this item and return its typed event, if it is not a heartbeat.
    pub fn into_event(self) -> Option<rs::ResponseStreamEvent> {
        match self {
            Self::Event(event) => Some(event),
            Self::Decoded(frame) => Some(frame.into_event()),
            Self::Heartbeat => None,
        }
    }
}

/// Sampler-private L2 item. Terminal facts travel immediately before their
/// matching public terminal event and never escape on the public event rail.
#[derive(Debug)]
pub(crate) enum AttemptStreamItem {
    Forward(SamplingEvent),
    TerminalFacts(TerminalFacts),
}

impl AttemptStreamItem {
    pub(crate) fn into_forward(self) -> Option<SamplingEvent> {
        match self {
            Self::Forward(event) => Some(event),
            Self::TerminalFacts(_) => None,
        }
    }
}

impl From<SamplingEvent> for AttemptStreamItem {
    fn from(event: SamplingEvent) -> Self {
        Self::Forward(event)
    }
}

/// Wraps a `String` so callers can pass an externally-assigned ID (e.g., a session-assigned UUID) or generate a fresh one via [`RequestId::random`].
#[derive(Clone, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct RequestId(String);

impl RequestId {
    /// Generate a fresh random request ID backed by a UUIDv4.
    pub fn random() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for RequestId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for RequestId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_produces_unique_values() {
        let a = RequestId::random();
        let b = RequestId::random();
        assert_ne!(a, b, "two random IDs must differ");
        // UUIDv4 strings are 36 characters (8-4-4-4-12 hex with hyphens).
        assert_eq!(a.as_str().len(), 36);
    }
}
