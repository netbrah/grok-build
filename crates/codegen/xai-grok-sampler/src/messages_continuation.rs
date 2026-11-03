//! Ordered, request-local Messages capture; the public stream remains fail-closed.

use indexmap::IndexMap;
use serde_json::Value;
use xai_grok_sampling_types::messages::{ContentBlock, MessageStreamEvent, StreamDelta};
use xai_grok_sampling_types::request_validation::{
    MAX_ENCODED_MESSAGES_REQUEST_BYTES, MAX_MESSAGES_REQUEST_ITEMS,
};
use xai_grok_sampling_types::{ConversationResponse, SamplingError};

use crate::metrics::InferenceLatencyStats;

use xai_grok_sampling_types::conversation::native_search_history::{
    NativeMessagesFinalized, NativeMessagesObservation, NativeUnavailable,
};

pub(crate) struct MessagesStreamFrame {
    pub(crate) event: MessageStreamEvent,
    raw: Option<Value>,
    wire_bytes: usize,
}

impl MessagesStreamFrame {
    pub(crate) fn decoded(event: MessageStreamEvent, data: &str) -> Result<Self, SamplingError> {
        Ok(Self {
            event,
            raw: Some(serde_json::from_str(data)?),
            wire_bytes: data.len(),
        })
    }
}

impl From<MessageStreamEvent> for MessagesStreamFrame {
    fn from(event: MessageStreamEvent) -> Self {
        Self {
            event,
            raw: None,
            wire_bytes: 0,
        }
    }
}

#[derive(Debug)]
pub(crate) struct CompletedAttempt {
    pub(crate) content: Vec<Value>,
    pub(crate) response: ConversationResponse,
    pub(crate) metrics: InferenceLatencyStats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureError {
    IncompleteStream,
    UnfaithfulStart,
    UnfaithfulDelta,
    UnknownEvent,
    InvalidStream,
    InvalidInput,
    LimitExceeded,
    CompletedMismatch,
    WaitingClientTool,
}

impl CaptureError {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::IncompleteStream => "Messages attempt did not complete faithfully",
            Self::UnfaithfulStart => "Messages content start cannot be retained faithfully",
            Self::UnfaithfulDelta => "Messages content delta cannot be retained faithfully",
            Self::UnknownEvent => "Messages event type cannot be retained faithfully",
            Self::InvalidStream => "Messages capture received inconsistent stream grammar",
            Self::InvalidInput => "Messages capture received invalid tool input",
            Self::LimitExceeded => "Messages capture exceeded the request retention limit",
            Self::CompletedMismatch => "Completed Messages content differs from its typed assembly",
            Self::WaitingClientTool => {
                "Paused Messages content contains a waiting client tool call"
            }
        }
    }
}

struct CapturedBlock {
    raw: Value,
    typed: ContentBlock,
    raw_json: String,
    typed_json: String,
    closed: bool,
}

fn start_matches(raw: &Value, typed: &ContentBlock) -> Result<bool, CaptureError> {
    let mut serialized = serde_json::to_value(typed).map_err(|_| CaptureError::UnfaithfulStart)?;
    if raw == &serialized {
        return Ok(true);
    }
    // Closed start-only equivalence: the decoder defaults omitted thinking.signature to "".
    if matches!(typed, ContentBlock::Thinking { signature, .. } if signature.is_empty())
        && raw.get("signature").is_none()
    {
        serialized
            .as_object_mut()
            .ok_or(CaptureError::UnfaithfulStart)?
            .remove("signature");
        return Ok(raw == &serialized);
    }
    Ok(false)
}

impl CapturedBlock {
    fn append_raw_string(&mut self, field: &str, value: &str) -> Result<(), CaptureError> {
        let object = self
            .raw
            .as_object_mut()
            .ok_or(CaptureError::UnfaithfulDelta)?;
        if field == "signature" && !object.contains_key(field) {
            object.insert(field.to_owned(), Value::String(String::new()));
        }
        let Some(Value::String(current)) = object.get_mut(field) else {
            return Err(CaptureError::UnfaithfulDelta);
        };
        current.push_str(value);
        Ok(())
    }

    fn native_delta_growth(&self, delta: &StreamDelta) -> Result<u64, CaptureError> {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        let (fragment, extra) = match delta {
            StreamDelta::CitationsDelta { citation } => {
                let bytes = native_encoded_len(citation, NATIVE_BLOCK_BYTES)
                    .map_err(|_| CaptureError::LimitExceeded)?;
                let (extra, removed) = match self.raw.get("citations") {
                    None => (b",\"citations\":[]".len() as u64, 0),
                    Some(Value::Null) => (2, 4),
                    Some(Value::Array(values)) => {
                        if values.len() >= MAX_MESSAGES_REQUEST_ITEMS {
                            return Err(CaptureError::LimitExceeded);
                        }
                        (u64::from(!values.is_empty()), 0)
                    }
                    Some(_) => return Err(CaptureError::InvalidStream),
                };
                return bytes
                    .checked_add(extra)
                    .and_then(|size| size.checked_sub(removed))
                    .ok_or(CaptureError::LimitExceeded);
            }
            StreamDelta::TextDelta { text } => (text.as_str(), 0),
            StreamDelta::ThinkingDelta { thinking } => (thinking.as_str(), 0),
            StreamDelta::SignatureDelta { signature } => (
                signature.as_str(),
                if self.raw.get("signature").is_none() {
                    b",\"signature\":\"\"".len() as u64
                } else {
                    0
                },
            ),
            StreamDelta::InputJsonDelta { partial_json } => (
                partial_json.as_str(),
                u64::from(self.raw_json.is_empty() && !partial_json.is_empty()) * 2,
            ),
        };
        native_encoded_len(&fragment, NATIVE_BLOCK_BYTES)
            .map_err(|_| CaptureError::LimitExceeded)?
            .checked_sub(2)
            .and_then(|n| n.checked_add(extra))
            .ok_or(CaptureError::LimitExceeded)
    }

    fn native_retained_bytes(&self) -> Result<u64, CaptureError> {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        let raw = native_encoded_len(&self.raw, NATIVE_BLOCK_BYTES)
            .map_err(|_| CaptureError::LimitExceeded)?;
        let partial = if self.raw_json.is_empty() {
            0
        } else {
            native_encoded_len(&self.raw_json, NATIVE_BLOCK_BYTES)
                .map_err(|_| CaptureError::LimitExceeded)?
        };
        raw.checked_add(partial)
            .filter(|bytes| *bytes <= NATIVE_BLOCK_BYTES)
            .ok_or(CaptureError::LimitExceeded)
    }

    fn native_materialized_input(&self) -> Result<Option<u64>, CaptureError> {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        if matches!(
            self.raw["type"].as_str(),
            Some("tool_use" | "server_tool_use")
        ) && self.raw["input"]
            .as_object()
            .is_some_and(|object| object.is_empty())
            && !self.raw_json.is_empty()
        {
            let bytes =
                native_json_encoded_len(&self.raw_json, NATIVE_BLOCK_BYTES).map_err(|reason| {
                    if reason == NativeUnavailable::Capacity {
                        CaptureError::LimitExceeded
                    } else {
                        CaptureError::InvalidInput
                    }
                })?;
            Ok(Some(bytes))
        } else {
            Ok(None)
        }
    }

    fn native_retained_sizes(&self) -> Result<NativeCaptureSizes, CaptureError> {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        let raw = native_encoded_json_string_len(&self.raw, NATIVE_RESPONSE_BYTES)
            .map_err(|_| CaptureError::LimitExceeded)?;
        let partial = if self.raw_json.is_empty() {
            4
        } else {
            native_encoded_len(&self.raw_json, NATIVE_RESPONSE_BYTES)
                .map_err(|_| CaptureError::LimitExceeded)?
        };
        Ok(NativeCaptureSizes {
            stored: self.native_retained_bytes()?,
            envelope: raw
                .checked_add(partial)
                .ok_or(CaptureError::LimitExceeded)?,
        })
    }

    fn native_delta_sizes(&self, delta: &StreamDelta) -> Result<NativeCaptureSizes, CaptureError> {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        let envelope = match delta {
            StreamDelta::CitationsDelta { citation } => {
                let bytes = native_encoded_json_string_len(citation, NATIVE_RESPONSE_BYTES)
                    .map_err(|_| CaptureError::LimitExceeded)?;
                let (extra, removed) = match self.raw.get("citations") {
                    None => (
                        native_encoded_len(&",\"citations\":[]", NATIVE_RESPONSE_BYTES)
                            .map_err(|_| CaptureError::LimitExceeded)?
                            - 2,
                        0,
                    ),
                    Some(Value::Null) => (2, 4),
                    Some(Value::Array(values)) => (u64::from(!values.is_empty()), 0),
                    Some(_) => return Err(CaptureError::InvalidStream),
                };
                bytes
                    .checked_sub(2)
                    .and_then(|size| size.checked_add(extra))
                    .and_then(|size| size.checked_sub(removed))
                    .ok_or(CaptureError::LimitExceeded)?
            }
            StreamDelta::InputJsonDelta { partial_json } => {
                let bytes = native_encoded_len(partial_json, NATIVE_RESPONSE_BYTES)
                    .map_err(|_| CaptureError::LimitExceeded)?;
                bytes.saturating_sub(if self.raw_json.is_empty() { 4 } else { 2 })
            }
            StreamDelta::TextDelta { text }
            | StreamDelta::ThinkingDelta { thinking: text }
            | StreamDelta::SignatureDelta { signature: text } => {
                let extra = if matches!(delta, StreamDelta::SignatureDelta { .. })
                    && self.raw.get("signature").is_none()
                {
                    native_encoded_len(&",\"signature\":\"\"", NATIVE_RESPONSE_BYTES)
                        .map_err(|_| CaptureError::LimitExceeded)?
                        - 2
                } else {
                    0
                };
                native_encoded_json_string_len(text, NATIVE_RESPONSE_BYTES)
                    .map_err(|_| CaptureError::LimitExceeded)?
                    .checked_sub(6)
                    .and_then(|bytes| bytes.checked_add(extra))
                    .ok_or(CaptureError::LimitExceeded)?
            }
        };
        Ok(NativeCaptureSizes {
            stored: self.native_delta_growth(delta)?,
            envelope,
        })
    }

    fn native_closed_sizes(&self) -> Result<NativeCaptureSizes, CaptureError> {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        let raw = native_encoded_len(&self.raw, NATIVE_BLOCK_BYTES)
            .map_err(|_| CaptureError::LimitExceeded)?;
        let encoded = native_encoded_json_string_len(&self.raw, NATIVE_RESPONSE_BYTES)
            .map_err(|_| CaptureError::LimitExceeded)?;
        let input = self.native_materialized_input()?;
        let input_encoded = if input.is_some() {
            native_json_string_encoded_len(&self.raw_json, NATIVE_RESPONSE_BYTES)
                .map_err(|reason| {
                    if reason == NativeUnavailable::Capacity {
                        CaptureError::LimitExceeded
                    } else {
                        CaptureError::InvalidInput
                    }
                })?
                .saturating_sub(4)
        } else {
            0
        };
        Ok(NativeCaptureSizes {
            stored: raw
                .checked_add(input.map_or(0, |bytes| bytes.saturating_sub(2)))
                .ok_or(CaptureError::LimitExceeded)?,
            envelope: encoded
                .checked_add(input_encoded)
                .and_then(|bytes| bytes.checked_add(4))
                .ok_or(CaptureError::LimitExceeded)?,
        })
    }

    fn reserve_native_delta(
        &self,
        delta: &StreamDelta,
        lease: &xai_grok_sampling_types::conversation::native_search_history::NativeCaptureLease,
    ) -> Result<(), CaptureError> {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        let bytes = self.native_retained_bytes()?;
        if bytes
            .checked_add(self.native_delta_growth(delta)?)
            .filter(|n| *n <= NATIVE_BLOCK_BYTES)
            .is_none()
        {
            return Err(CaptureError::LimitExceeded);
        }
        let growth = match delta {
            StreamDelta::CitationsDelta { citation } => usize::try_from(
                native_encoded_len(citation, NATIVE_BLOCK_BYTES)
                    .map_err(|_| CaptureError::LimitExceeded)?,
            )
            .map_err(|_| CaptureError::LimitExceeded)?,
            StreamDelta::TextDelta { text } => text.len(),
            StreamDelta::ThinkingDelta { thinking } => thinking.len(),
            StreamDelta::SignatureDelta { signature } => signature.len(),
            StreamDelta::InputJsonDelta { partial_json } => {
                if self
                    .raw_json
                    .len()
                    .checked_add(partial_json.len())
                    .filter(|n| *n as u64 <= MAX_ENCODED_MESSAGES_REQUEST_BYTES)
                    .is_none()
                {
                    return Err(CaptureError::LimitExceeded);
                }
                partial_json.len()
            }
        };
        lease
            .reserve(
                (growth as u64)
                    .checked_mul(2)
                    .ok_or(CaptureError::LimitExceeded)?,
            )
            .map_err(|_| CaptureError::LimitExceeded)
    }

    fn reserve_native_close(
        &self,
        lease: &xai_grok_sampling_types::conversation::native_search_history::NativeCaptureLease,
    ) -> Result<(), CaptureError> {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        let bytes = native_encoded_len(&self.raw, NATIVE_BLOCK_BYTES)
            .map_err(|_| CaptureError::LimitExceeded)?;
        let input = self.native_materialized_input()?;
        if bytes
            .checked_add(input.map_or(0, |n| n.saturating_sub(2)))
            .filter(|n| *n <= NATIVE_BLOCK_BYTES)
            .is_none()
        {
            return Err(CaptureError::LimitExceeded);
        }
        if let Some(input) = input {
            lease
                .reserve(input.checked_mul(2).ok_or(CaptureError::LimitExceeded)?)
                .map_err(|_| CaptureError::LimitExceeded)?;
        }
        Ok(())
    }

    fn apply_delta(
        &mut self,
        delta: &StreamDelta,
        raw_delta: &Value,
        native: Option<
            &xai_grok_sampling_types::conversation::native_search_history::NativeCaptureLease,
        >,
    ) -> Result<(), CaptureError> {
        if let Some(lease) = native {
            self.reserve_native_delta(delta, lease)?;
        }
        match (&mut self.typed, delta) {
            (ContentBlock::Text { citations, .. }, StreamDelta::CitationsDelta { citation }) => {
                if citations
                    .as_ref()
                    .is_some_and(|values| values.len() >= MAX_MESSAGES_REQUEST_ITEMS)
                {
                    return Err(CaptureError::LimitExceeded);
                }
                let raw_citation = raw_delta
                    .get("citation")
                    .ok_or(CaptureError::UnfaithfulDelta)?;
                let mut values = std::mem::take(citations).into_value().unwrap_or_default();
                values.push(citation.clone());
                *citations = xai_grok_sampling_types::presence::WirePresence::value(values);
                let object = self
                    .raw
                    .as_object_mut()
                    .ok_or(CaptureError::UnfaithfulDelta)?;
                let raw_citations = object
                    .entry("citations")
                    .or_insert_with(|| serde_json::json!([]));
                if raw_citations.is_null() {
                    *raw_citations = serde_json::json!([]);
                }
                raw_citations
                    .as_array_mut()
                    .ok_or(CaptureError::UnfaithfulDelta)?
                    .push(raw_citation.clone());
            }
            (ContentBlock::Text { text, .. }, StreamDelta::TextDelta { text: fragment }) => {
                text.push_str(fragment);
                self.append_raw_string(
                    "text",
                    raw_delta["text"]
                        .as_str()
                        .ok_or(CaptureError::UnfaithfulDelta)?,
                )?;
            }
            (
                ContentBlock::Thinking { thinking, .. },
                StreamDelta::ThinkingDelta { thinking: fragment },
            ) => {
                thinking.push_str(fragment);
                self.append_raw_string(
                    "thinking",
                    raw_delta["thinking"]
                        .as_str()
                        .ok_or(CaptureError::UnfaithfulDelta)?,
                )?;
            }
            (
                ContentBlock::Thinking { signature, .. },
                StreamDelta::SignatureDelta {
                    signature: fragment,
                },
            ) => {
                signature.push_str(fragment);
                self.append_raw_string(
                    "signature",
                    raw_delta["signature"]
                        .as_str()
                        .ok_or(CaptureError::UnfaithfulDelta)?,
                )?;
            }
            (
                ContentBlock::ToolUse { .. } | ContentBlock::ServerToolUse { .. },
                StreamDelta::InputJsonDelta { partial_json },
            ) => {
                self.typed_json.push_str(partial_json);
                self.raw_json.push_str(
                    raw_delta["partial_json"]
                        .as_str()
                        .ok_or(CaptureError::UnfaithfulDelta)?,
                );
            }
            _ => return Err(CaptureError::InvalidStream),
        }
        Ok(())
    }

    fn close(
        &mut self,
        native: Option<
            &xai_grok_sampling_types::conversation::native_search_history::NativeCaptureLease,
        >,
    ) -> Result<(), CaptureError> {
        if let Some(lease) = native {
            self.reserve_native_close(lease)?;
        }
        if let ContentBlock::ToolUse { input, .. } | ContentBlock::ServerToolUse { input, .. } =
            &mut self.typed
            && input.as_object().is_some_and(|object| object.is_empty())
            && !self.typed_json.is_empty()
        {
            *input =
                serde_json::from_str(&self.typed_json).map_err(|_| CaptureError::InvalidInput)?;
        }
        // Non-empty start input stays authoritative, matching the existing stream accumulator.
        if matches!(
            self.raw["type"].as_str(),
            Some("tool_use" | "server_tool_use")
        ) && self.raw["input"]
            .as_object()
            .is_some_and(|object| object.is_empty())
            && !self.raw_json.is_empty()
        {
            self.raw["input"] =
                serde_json::from_str(&self.raw_json).map_err(|_| CaptureError::InvalidInput)?;
        }
        self.raw_json.clear();
        self.typed_json.clear();
        self.closed = true;
        Ok(())
    }
}

#[derive(Default, PartialEq, Eq)]
enum CapturePhase {
    #[default]
    BeforeStart,
    Content,
    Terminal,
    Stopped,
}

#[derive(Default)]
pub(crate) struct AttemptCapture {
    blocks: IndexMap<u32, CapturedBlock>,
    phase: CapturePhase,
    retained_wire_bytes: u64,
    pub(crate) completed: Option<CompletedAttempt>,
    pub(crate) failure: Option<CaptureError>,
    native:
        Option<xai_grok_sampling_types::conversation::native_search_history::NativeCaptureLease>,
    native_admitted: bool,
    native_started: bool,
    native_id: Option<Value>,
    native_stop: Option<String>,
    native_paused: bool,
    native_usage: std::collections::BTreeMap<String, Value>,
    messages_observations: Vec<NativeMessagesObservation>,
    messages_finalized: NativeMessagesFinalized,
    messages_observation_bytes: u64,
    messages_reserved_bytes: u64,
    messages_failure: Option<NativeUnavailable>,
    native_coordinates: Vec<u32>,
    native_failure_block: Option<u32>,
    native_starts: u64,
    native_reasoning: Option<u32>,
    native_reserved_response_bytes: u64,
    native_generation_funded: bool,
}

#[derive(Clone, Copy)]
struct NativeCaptureSizes {
    stored: u64,
    envelope: u64,
}

impl NativeCaptureSizes {
    fn add(self, growth: Self) -> Result<Self, CaptureError> {
        Ok(Self {
            stored: self
                .stored
                .checked_add(growth.stored)
                .ok_or(CaptureError::LimitExceeded)?,
            envelope: self
                .envelope
                .checked_add(growth.envelope)
                .ok_or(CaptureError::LimitExceeded)?,
        })
    }
}

#[derive(Clone, Copy)]
struct NativeMetadataSizes {
    id: u64,
    stop: u64,
    usage: u64,
    messages: u64,
}

impl AttemptCapture {
    pub(crate) fn passive(
        lease: xai_grok_sampling_types::conversation::native_search_history::NativeCaptureLease,
    ) -> Self {
        let native_generation_funded = match lease.reserve_generation() {
            Ok(()) => true,
            Err(reason) => {
                lease.unavailable(reason);
                false
            }
        };
        let mut capture = Self {
            native: Some(lease),
            native_generation_funded,
            ..Self::default()
        };
        if !native_generation_funded {
            capture.failure = Some(CaptureError::LimitExceeded);
            return capture;
        }
        if capture
            .native_metadata_sizes()
            .and_then(|metadata| capture.reserve_native_response(None, metadata))
            .is_err()
        {
            capture.failure = Some(CaptureError::LimitExceeded);
        }
        capture
    }

    pub(crate) fn is_passive(&self) -> bool {
        self.native.is_some()
    }

    pub(crate) fn passive_exhausted(&self) -> bool {
        self.is_passive() && self.failure == Some(CaptureError::LimitExceeded)
    }

    pub(crate) fn observed_pause(&self) -> bool {
        self.native_paused
    }

    fn native_reserve(&mut self, bytes: u64) -> Result<(), CaptureError> {
        if let Some(lease) = &self.native {
            lease
                .reserve(bytes)
                .map_err(|_| CaptureError::LimitExceeded)?;
        }
        Ok(())
    }

    fn messages_capture_len(
        count: u64,
        bytes: u64,
        finalized: u64,
        limit: u64,
    ) -> Result<u64, CaptureError> {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        let empty = NativeMessagesFinalized::default();
        let capture = NativeMessagesCapture::Observed {
            observations: Vec::new(),
            finalized: empty.clone().into(),
            coverage: NativeMessagesCoverage::Partial {
                reason: NativeUnavailable::ConflictingObservation,
            },
        };
        native_encoded_len(&capture, limit)
            .map_err(|_| CaptureError::LimitExceeded)?
            .checked_sub(
                native_encoded_len(&empty, limit).map_err(|_| CaptureError::LimitExceeded)?,
            )
            .and_then(|size| size.checked_add(finalized))
            .and_then(|size| size.checked_add(bytes))
            .and_then(|size| size.checked_add(count.saturating_sub(1)))
            .filter(|size| *size <= limit)
            .ok_or(CaptureError::LimitExceeded)
    }

    fn messages_sizes(
        &self,
        kind: xai_grok_sampling_types::conversation::native_search_history::NativeMessagesObservationKind,
        envelope: &Value,
    ) -> Result<(NativeMetadataSizes, u64, u64, u64), CaptureError> {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        let limit = if self.is_passive() {
            NATIVE_RESPONSE_BYTES
        } else {
            MAX_ENCODED_MESSAGES_REQUEST_BYTES
        };
        let ordinal = self.messages_observations.len() as u64;
        if ordinal >= NATIVE_MESSAGES_METADATA_OBSERVATIONS {
            return Err(CaptureError::LimitExceeded);
        }
        let observation =
            NativeMessagesObservation::envelope_encoded_len(ordinal, kind, envelope, limit)
                .map_err(|_| CaptureError::LimitExceeded)?;
        let finalized = self
            .messages_finalized
            .preview_encoded_len(ordinal, kind, envelope, limit)
            .map_err(|reason| {
                if reason == NativeUnavailable::Capacity {
                    CaptureError::LimitExceeded
                } else {
                    CaptureError::InvalidStream
                }
            })?;
        let usage = self
            .messages_finalized
            .preview_usage_encoded_len(kind, envelope, limit)
            .map_err(|_| CaptureError::LimitExceeded)?;
        let series = self
            .messages_observation_bytes
            .checked_add(observation)
            .ok_or(CaptureError::LimitExceeded)?;
        let actual = Self::messages_capture_len(ordinal + 1, series, finalized, limit)?;
        let terminal = serde_json::json!({"stop_reason":"model_context_window_exceeded","stop_sequence":"",
            "stop_details":null,"usage":{"input_tokens":u64::MAX,"output_tokens":u64::MAX,
                "thinking_tokens":u64::MAX,"cache_read_input_tokens":u64::MAX,"cache_creation_input_tokens":u64::MAX,
                "output_tokens_details":{"thinking_tokens":u64::MAX}}});
        let terminal_observation = NativeMessagesObservation::envelope_encoded_len(
            u64::MAX,
            NativeMessagesObservationKind::MessageDelta,
            &terminal,
            limit,
        )
        .map_err(|_| CaptureError::LimitExceeded)?;
        let terminal_finalized = self
            .messages_finalized
            .preview_encoded_len(
                u64::MAX,
                NativeMessagesObservationKind::MessageDelta,
                &terminal,
                limit,
            )
            .map_err(|_| CaptureError::LimitExceeded)?;
        let future = if self.is_passive() {
            Self::messages_capture_len(
                ordinal + 2,
                series
                    .checked_add(terminal_observation)
                    .ok_or(CaptureError::LimitExceeded)?,
                finalized.max(terminal_finalized),
                limit,
            )?
        } else {
            actual
        };
        let mut metadata = self.native_metadata_sizes()?;
        metadata.usage = usage;
        metadata.messages = future;
        if kind != NativeMessagesObservationKind::MessageDelta {
            metadata.id = native_encoded_len(&envelope.get("id"), limit)
                .map_err(|_| CaptureError::LimitExceeded)?;
        }
        let stops = if kind == NativeMessagesObservationKind::MessageDelta {
            envelope.get("delta").unwrap_or(envelope)
        } else {
            envelope
        };
        if let Some(stop) = stops.get("stop_reason").filter(|value| value.is_string()) {
            metadata.stop =
                native_encoded_len(stop, limit).map_err(|_| CaptureError::LimitExceeded)?;
        }
        let retained = self
            .retained_wire_bytes
            .checked_add(actual)
            .and_then(|bytes| bytes.checked_add(usage))
            .and_then(|bytes| bytes.checked_add(metadata.id))
            .and_then(|bytes| bytes.checked_add(metadata.stop))
            .ok_or(CaptureError::LimitExceeded)?;
        if retained > MAX_ENCODED_MESSAGES_REQUEST_BYTES {
            return Err(CaptureError::LimitExceeded);
        }
        Ok((metadata, observation, finalized, actual))
    }

    fn observe_native_metadata(&mut self, frame: &MessagesStreamFrame) -> Result<(), CaptureError> {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        if let MessageStreamEvent::MessageDelta { delta, .. } = &frame.event {
            self.native_paused |= matches!(
                delta.stop_reason.as_ref(),
                Some(xai_grok_sampling_types::messages::StopReason::PauseTurn)
            );
        }
        if self.native.is_some() && !self.native_generation_funded {
            return Ok(());
        }
        if let MessageStreamEvent::ContentBlockStart { index, .. } = &frame.event {
            self.native_starts = self
                .native_starts
                .checked_add(1)
                .ok_or(CaptureError::LimitExceeded)?;
            if self.failure.is_none() {
                self.native_failure_block = Some(*index);
            }
        }
        let Some(raw) = &frame.raw else {
            return Ok(());
        };
        if let MessageStreamEvent::ContentBlockStart { .. } = &frame.event {
            self.native_admitted |= raw
                .get("content_block")
                .and_then(|block| block.get("type"))
                .and_then(Value::as_str)
                .and_then(xai_grok_sampling_types::tool_search::ToolSearchKind::from_item_type)
                .is_some();
        }
        let (kind, envelope) = match &frame.event {
            MessageStreamEvent::MessageStart { .. } => {
                self.native_started = true;
                (
                    NativeMessagesObservationKind::MessageStart,
                    raw.get("message").ok_or(CaptureError::InvalidStream)?,
                )
            }
            MessageStreamEvent::MessageDelta { .. } => {
                (NativeMessagesObservationKind::MessageDelta, raw)
            }
            _ => return Ok(()),
        };
        if self.messages_failure.is_some() {
            return Ok(());
        }
        let measured = self.messages_sizes(kind, envelope);
        let (metadata, observation_bytes, finalized_bytes, actual) = match measured {
            Ok(measured) => measured,
            Err(error) => {
                self.messages_failure = Some(if error == CaptureError::LimitExceeded {
                    NativeUnavailable::Capacity
                } else {
                    NativeUnavailable::Fidelity
                });
                return Err(error);
            }
        };
        let physical = observation_bytes
            .checked_add(finalized_bytes)
            .and_then(|bytes| bytes.checked_add(metadata.usage))
            .and_then(|bytes| bytes.checked_mul(2))
            .ok_or(CaptureError::LimitExceeded)?;
        let funded = !self.passive_exhausted()
            && self.reserve_native_response(None, metadata).is_ok()
            && self.native_reserve(physical).is_ok();
        if !funded {
            let mut current = metadata;
            current.messages = actual;
            let required = self
                .native_response_reservation(None, current)?
                .checked_add(physical)
                .ok_or(CaptureError::LimitExceeded)?;
            let capacity_only = self
                .failure
                .is_none_or(|error| error == CaptureError::LimitExceeded)
                && self.native.as_ref().is_some_and(|lease| {
                    matches!(lease.reserve(0), Ok(()) | Err(NativeUnavailable::Capacity))
                });
            if !capacity_only || required > self.native_reserved_response_bytes {
                self.messages_failure = Some(NativeUnavailable::Capacity);
                return Err(CaptureError::LimitExceeded);
            }
            // Keep the current envelope only within the prepaid terminal footprint.
            self.failure = Some(CaptureError::LimitExceeded);
            self.messages_failure = Some(NativeUnavailable::Capacity);
        }
        if funded {
            self.messages_reserved_bytes = self.messages_reserved_bytes.max(metadata.messages);
        }
        let ordinal = self.messages_observations.len() as u64;
        let observation = NativeMessagesObservation::from_envelope(ordinal, kind, envelope);
        self.messages_finalized
            .apply(&observation)
            .map_err(|_| CaptureError::InvalidStream)?;
        self.messages_observation_bytes = self
            .messages_observation_bytes
            .checked_add(observation_bytes)
            .ok_or(CaptureError::LimitExceeded)?;
        self.messages_observations.push(observation);
        if kind == NativeMessagesObservationKind::MessageStart {
            self.native_id = envelope.get("id").cloned();
        }
        if let Some(stop) = self
            .messages_finalized
            .stop_reason
            .value()
            .and_then(Value::as_str)
        {
            self.native_stop = Some(stop.to_owned());
        }
        self.native_usage = match self.messages_finalized.usage.value() {
            Some(Value::Object(usage)) => usage.clone().into_iter().collect(),
            None => Default::default(),
            Some(_) => return Err(CaptureError::InvalidStream),
        };
        Ok(())
    }

    fn native_metadata_sizes(&self) -> Result<NativeMetadataSizes, CaptureError> {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        let limit = if self.is_passive() {
            NATIVE_RESPONSE_BYTES
        } else {
            MAX_ENCODED_MESSAGES_REQUEST_BYTES
        };
        let finalized = native_encoded_len(&self.messages_finalized, limit)
            .map_err(|_| CaptureError::LimitExceeded)?;
        Ok(NativeMetadataSizes {
            id: native_encoded_len(&self.native_id, limit)
                .map_err(|_| CaptureError::LimitExceeded)?,
            stop: native_encoded_len(&self.native_stop, limit)
                .map_err(|_| CaptureError::LimitExceeded)?,
            usage: native_encoded_len(&self.native_usage, limit)
                .map_err(|_| CaptureError::LimitExceeded)?,
            messages: Self::messages_capture_len(
                self.messages_observations.len() as u64,
                self.messages_observation_bytes,
                finalized,
                limit,
            )?
            .max(self.messages_reserved_bytes),
        })
    }

    fn reserve_native_response(
        &mut self,
        replacement: Option<(u32, NativeCaptureSizes)>,
        metadata: NativeMetadataSizes,
    ) -> Result<(), CaptureError> {
        let Some(lease) = &self.native else {
            return Ok(());
        };
        let reserved = self.native_response_reservation(replacement, metadata)?;
        if reserved > self.native_reserved_response_bytes {
            lease
                .reserve(reserved - self.native_reserved_response_bytes)
                .map_err(|_| CaptureError::LimitExceeded)?;
            self.native_reserved_response_bytes = reserved;
        }
        Ok(())
    }

    fn native_response_reservation(
        &self,
        replacement: Option<(u32, NativeCaptureSizes)>,
        metadata: NativeMetadataSizes,
    ) -> Result<u64, CaptureError> {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        let Some(lease) = &self.native else {
            return Ok(0);
        };
        fn len<T: serde::Serialize>(value: &T) -> Result<u64, CaptureError> {
            xai_grok_sampling_types::conversation::native_search_history::native_encoded_len(
                value,
                xai_grok_sampling_types::conversation::native_search_history::NATIVE_RESPONSE_BYTES,
            )
            .map_err(|_| CaptureError::LimitExceeded)
        }
        // Reserve terminal forms and maximum-width coordinates before retaining content.
        let status =
            NativeResponseStatus::PartialTerminal(NativeUnavailable::ConflictingObservation);
        let stop = metadata.stop.max(len(
            &xai_grok_sampling_types::messages::StopReason::ModelContextWindowExceeded,
        )?);
        let usage = metadata.usage.max(len(&serde_json::json!({
            "input_tokens":u64::MAX,"output_tokens":u64::MAX,"thinking_tokens":u64::MAX,
            "cache_read_input_tokens":u64::MAX,"cache_creation_input_tokens":u64::MAX,
            "cache_creation":{"ephemeral_5m_input_tokens":u64::MAX,"ephemeral_1h_input_tokens":u64::MAX},
            "output_tokens_details":{"thinking_tokens":u64::MAX}
        }))?);
        let header = NativeAttemptEvidence {
            attempt: u64::MAX,
            admitted: false,
            response_started: false,
            provider_response_id: None,
            source_stop_reason: None,
            stream_complete: false,
            status,
            blocks: Vec::new(),
            usage: Default::default(),
            messages_metadata: NativeMessagesCapture::Observed {
                observations: Vec::new(),
                finalized: NativeMessagesFinalized::default().into(),
                coverage: NativeMessagesCoverage::Partial {
                    reason: NativeUnavailable::ConflictingObservation,
                },
            },
            latency_ms: Some(u64::MAX),
            http_dispatches: u64::MAX,
            observed_bytes: u64::MAX,
            failed_block: Some(u32::MAX),
            view_roles: Vec::new(),
            view_digests: Vec::new(),
        };
        let mut envelope = len(&header)? + metadata.id.max(4) - 4 + stop - 4 + usage - 2
            + metadata
                .messages
                .checked_sub(len(&header.messages_metadata)?)
                .ok_or(CaptureError::LimitExceeded)?;
        let response = NativeResponseBoundary {
            occurrence: u64::MAX,
            request: lease.request().into(),
            lease: lease.ordinal(),
            attempt: u64::MAX,
            continuation: 0,
            provider_response_id: Some(Value::Null),
            source_stop_reason: None,
            block_start: u64::MAX,
            block_end: u64::MAX,
            stream_complete: false,
            status,
        };
        let mut stored = len(&response)? + metadata.id.max(4) - 4 + stop - 4 + metadata.messages;
        let digest = "0".repeat(64);
        let view_bytes = len(&Some((u64::MAX, "Discovery")))? + len(&digest)?;
        let reference = |kind: &str| NativeBlobRef {
            kind: kind.into(),
            byte_len: NATIVE_BLOCK_BYTES,
            digest: digest.clone(),
        };
        let mut count = 0u64;
        let mut add = |index: u32, sizes: NativeCaptureSizes| -> Result<(), CaptureError> {
            let block = NativeRawBlock {
                coordinates: NativeSourceCoordinate {
                    lineage: lease.lineage().into(),
                    producing_binding: lease.request().into(),
                    turn: lease.turn(),
                    item: u64::MAX,
                    response: u64::MAX,
                    block: u64::MAX,
                    submember: None,
                    occurrence: u64::MAX,
                },
                wire_index: index,
                arrival_ordinal: u64::MAX,
                block: reference("raw_block"),
                partial: Some(reference("partial_input_json")),
                state: NativeBlockState::Unavailable(NativeUnavailable::Incomplete),
                eligible_ref: None,
                eligible_unavailable: Some(NativeUnavailable::UnknownIdentity),
            };
            let bytes = NativeResource::BlockBytes
                .admit(sizes.stored, len(&block)?)
                .map_err(|_| CaptureError::LimitExceeded)?;
            stored = NativeResource::ResponseBytes
                .admit(stored, bytes)
                .map_err(|_| CaptureError::LimitExceeded)?;
            let captured = NativeCapturedBlock {
                wire_index: index,
                arrival_ordinal: u64::MAX,
                raw: String::new(),
                partial_json: None,
                state: NativeBlockState::Unavailable(NativeUnavailable::Incomplete),
            };
            let extra = len(&captured)? - 6 + view_bytes + u64::from(count > 0) * 3;
            envelope = envelope
                .checked_add(sizes.envelope)
                .and_then(|bytes| bytes.checked_add(extra))
                .filter(|bytes| *bytes <= NATIVE_RESPONSE_BYTES)
                .ok_or(CaptureError::LimitExceeded)?;
            count = NativeResource::ResponseBlocks
                .admit(count, 1)
                .map_err(|_| CaptureError::LimitExceeded)?;
            Ok(())
        };
        for (index, block) in &self.blocks {
            let sizes = if let Some((changed, sizes)) = replacement
                && changed == *index
            {
                sizes
            } else {
                block.native_retained_sizes()?
            };
            add(*index, sizes)?;
        }
        if let Some((index, sizes)) = replacement
            && !self.blocks.contains_key(&index)
        {
            add(index, sizes)?;
        }
        let accounting = NativeResponseAccounting {
            request: lease.request().into(),
            lease: lease.ordinal(),
            attempt: u64::MAX,
            response_occurrence: Some(u64::MAX),
            continuation: 0,
            observed: Default::default(),
            source_stop_reason: None,
            outcome: status,
            latency_ms: Some(u64::MAX),
            http_dispatches: 1,
        };
        let terminal = NativeTerminalMarker {
            outcome: NativeUnavailable::Capacity,
            last_accepted_response: Some(u64::MAX),
            observed_bytes: u64::MAX,
            failed_block: Some(u32::MAX),
        };
        let reserved = stored
            .checked_add(envelope)
            .and_then(|bytes| bytes.checked_add(len(&accounting).ok()? + usage - 2 + stop - 4))
            .and_then(|bytes| bytes.checked_add(len(&terminal).ok()?))
            .and_then(|bytes| bytes.checked_add(metadata.messages.checked_mul(3)?))
            .ok_or(CaptureError::LimitExceeded)?;
        if stored > NATIVE_RESPONSE_BYTES || envelope > NATIVE_RESPONSE_BYTES {
            return Err(CaptureError::LimitExceeded);
        }
        Ok(reserved)
    }

    pub(crate) fn seal_native(
        &mut self,
        attempt: u64,
        outcome: xai_grok_sampling_types::conversation::native_search_history::NativeResponseStatus,
        dispatched: bool,
        latency_ms: u64,
        mint_tag: Option<&str>,
    ) -> Option<xai_grok_sampling_types::conversation::native_search_history::NativeAttemptEvidence>
    {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        self.native.as_ref()?;
        if !self.native_generation_funded || self.observed_pause() {
            return None;
        }
        let mut blocks = Vec::new();
        let mut view_roles = Vec::new();
        let mut view_digests = Vec::new();
        let mut stream_complete = self.phase == CapturePhase::Stopped;
        let mut status = if self.failure == Some(CaptureError::LimitExceeded) {
            NativeResponseStatus::PartialTerminal(NativeUnavailable::Capacity)
        } else if self.failure.is_some() {
            NativeResponseStatus::PartialTerminal(NativeUnavailable::Fidelity)
        } else {
            outcome
        };
        if let Some(mut completed) = self.completed.take() {
            use sha2::Digest;
            struct ViewHasher(sha2::Sha256);
            impl std::io::Write for ViewHasher {
                fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                    self.0.update(bytes);
                    Ok(bytes.len())
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            let digest_bytes = (completed.response.items.len() as u64)
                .checked_mul(67)?
                .checked_add(2)?;
            if self.native_reserve(digest_bytes).is_ok() {
                // Match the mint stamp on the ordinary committed response.
                xai_grok_sampling_types::stamp_reasoning_mint_tag(
                    &mut completed.response.items,
                    mint_tag,
                );
                for item in &completed.response.items {
                    let mut writer = ViewHasher(sha2::Sha256::new());
                    serde_json::to_writer(&mut writer, std::slice::from_ref(item)).ok()?;
                    view_digests.push(format!("{:x}", writer.0.finalize()));
                }
            } else {
                self.failure = Some(CaptureError::LimitExceeded);
                status = NativeResponseStatus::PartialTerminal(NativeUnavailable::Capacity);
                self.native
                    .as_ref()?
                    .unavailable(NativeUnavailable::Capacity);
            }
            let reasoning = completed.response.items.iter().position(|item| {
                matches!(
                    item,
                    xai_grok_sampling_types::ConversationItem::Reasoning(_)
                )
            });
            let assistant = completed.response.items.iter().position(|item| {
                matches!(
                    item,
                    xai_grok_sampling_types::ConversationItem::Assistant(_)
                )
            });
            let mut discovery =
                completed
                    .response
                    .items
                    .iter()
                    .enumerate()
                    .filter_map(|(index, item)| {
                        matches!(
                            item,
                            xai_grok_sampling_types::ConversationItem::Discovery { .. }
                        )
                        .then_some(index)
                    });
            for (ordinal, raw) in completed.content.into_iter().enumerate() {
                let role = match raw.get("type").and_then(Value::as_str) {
                    Some("thinking")
                        if self.native_coordinates.get(ordinal).copied()
                            == self.native_reasoning =>
                    {
                        reasoning.map(|item| (item as u64, "Reasoning"))
                    }
                    Some("thinking" | "redacted_thinking") => None,
                    Some("server_tool_use" | "tool_search_tool_result") => {
                        discovery.next().map(|item| (item as u64, "Discovery"))
                    }
                    Some("tool_use") => assistant.map(|item| (item as u64, "Client")),
                    Some("text")
                        if raw
                            .get("text")
                            .and_then(Value::as_str)
                            .is_some_and(|text| !text.is_empty()) =>
                    {
                        assistant.map(|item| (item as u64, "Text"))
                    }
                    _ => None,
                };
                let bytes = encode_native(&raw, NATIVE_BLOCK_BYTES).ok()?;
                blocks.push(NativeCapturedBlock {
                    wire_index: *self.native_coordinates.get(ordinal)?,
                    arrival_ordinal: ordinal as u64,
                    raw: String::from_utf8(bytes).ok()?,
                    partial_json: None,
                    state: NativeBlockState::ClosedFaithful,
                });
                view_roles.push(role.map(|(item, role)| (item, role.into())));
            }
        } else {
            stream_complete = false;
            for (ordinal, (wire_index, block)) in
                std::mem::take(&mut self.blocks).into_iter().enumerate()
            {
                let faithful = block.closed
                    && serde_json::to_value(&block.typed).ok().as_ref() == Some(&block.raw);
                let bytes = encode_native(&block.raw, NATIVE_BLOCK_BYTES).ok()?;
                blocks.push(NativeCapturedBlock {
                    wire_index,
                    arrival_ordinal: ordinal as u64,
                    raw: String::from_utf8(bytes).ok()?,
                    partial_json: (!block.raw_json.is_empty()).then_some(block.raw_json),
                    state: if faithful {
                        NativeBlockState::ClosedFaithful
                    } else {
                        NativeBlockState::Unavailable(
                            if self.failure == Some(CaptureError::LimitExceeded) {
                                NativeUnavailable::Capacity
                            } else {
                                NativeUnavailable::Incomplete
                            },
                        )
                    },
                });
            }
        }
        Some(NativeAttemptEvidence {
            attempt,
            admitted: self.native_admitted,
            response_started: self.native_started,
            provider_response_id: self.native_id.take(),
            source_stop_reason: self.native_stop.take(),
            stream_complete,
            status,
            blocks,
            usage: std::mem::take(&mut self.native_usage),
            messages_metadata: NativeMessagesCapture::Observed {
                observations: std::mem::take(&mut self.messages_observations),
                finalized: std::mem::take(&mut self.messages_finalized).into(),
                coverage: if stream_complete
                    && status == NativeResponseStatus::Accepted
                    && self.messages_failure.is_none()
                {
                    NativeMessagesCoverage::Complete
                } else {
                    NativeMessagesCoverage::Partial {
                        reason: match status {
                            NativeResponseStatus::PartialTerminal(reason) => reason,
                            NativeResponseStatus::Collecting => NativeUnavailable::Incomplete,
                            NativeResponseStatus::Accepted => {
                                self.messages_failure.unwrap_or(NativeUnavailable::Failed)
                            }
                        },
                    }
                },
            },
            latency_ms: Some(latency_ms),
            http_dispatches: u64::from(dispatched),
            observed_bytes: self.retained_wire_bytes,
            failed_block: self.failure.and(self.native_failure_block),
            view_roles,
            view_digests,
        })
    }

    pub(crate) fn observe(&mut self, frame: &MessagesStreamFrame) -> Result<(), CaptureError> {
        self.retained_wire_bytes = self
            .retained_wire_bytes
            .checked_add(frame.wire_bytes as u64)
            .unwrap_or(u64::MAX);
        self.observe_native_metadata(frame)?;
        if self.is_passive()
            && let Some(error) = self.failure
            && error != CaptureError::LimitExceeded
        {
            return Err(error);
        }
        if self.passive_exhausted() {
            return Err(CaptureError::LimitExceeded);
        }
        if self.retained_wire_bytes > MAX_ENCODED_MESSAGES_REQUEST_BYTES {
            return Err(CaptureError::LimitExceeded);
        }
        let raw = frame.raw.as_ref().ok_or(CaptureError::IncompleteStream)?;
        match &frame.event {
            MessageStreamEvent::MessageStart { message } => {
                if self.phase != CapturePhase::BeforeStart
                    || !message.content.is_empty()
                    || message.stop_reason.as_ref().is_some()
                    || message.container.as_ref().is_some()
                    || message.role != "assistant"
                    || message.r#type != "message"
                {
                    return Err(CaptureError::InvalidStream);
                }
                self.phase = CapturePhase::Content;
            }
            MessageStreamEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                if self.phase != CapturePhase::Content || self.blocks.contains_key(index) {
                    return Err(CaptureError::InvalidStream);
                }
                if self.blocks.len() >= MAX_MESSAGES_REQUEST_ITEMS {
                    return Err(CaptureError::LimitExceeded);
                }
                let raw_block = raw
                    .get("content_block")
                    .ok_or(CaptureError::UnfaithfulStart)?;
                if matches!(content_block, ContentBlock::Unknown { .. })
                    || !start_matches(raw_block, content_block)?
                {
                    return Err(CaptureError::UnfaithfulStart);
                }
                if let ContentBlock::ToolUse { input, .. }
                | ContentBlock::ServerToolUse { input, .. } = content_block
                    && !input.is_object()
                {
                    return Err(CaptureError::InvalidInput);
                }
                if self.is_passive() {
                    use xai_grok_sampling_types::conversation::native_search_history::*;
                    if self.blocks.len() as u64 >= NATIVE_RESPONSE_BLOCKS {
                        return Err(CaptureError::LimitExceeded);
                    }
                    let bytes = native_encoded_len(raw_block, NATIVE_BLOCK_BYTES)
                        .map_err(|_| CaptureError::LimitExceeded)?;
                    let sizes = NativeCaptureSizes {
                        stored: bytes,
                        envelope: native_encoded_json_string_len(raw_block, NATIVE_RESPONSE_BYTES)
                            .map_err(|_| CaptureError::LimitExceeded)?
                            + 4,
                    };
                    self.reserve_native_response(
                        Some((*index, sizes)),
                        self.native_metadata_sizes()?,
                    )?;
                    self.native_reserve(bytes.checked_mul(2).ok_or(CaptureError::LimitExceeded)?)?;
                }
                self.blocks.insert(
                    *index,
                    CapturedBlock {
                        raw: raw_block.clone(),
                        typed: content_block.clone(),
                        raw_json: String::new(),
                        typed_json: String::new(),
                        closed: false,
                    },
                );
            }
            MessageStreamEvent::ContentBlockDelta { index, delta } => {
                if self.phase != CapturePhase::Content {
                    return Err(CaptureError::InvalidStream);
                }
                let raw_delta = raw.get("delta").ok_or(CaptureError::UnfaithfulDelta)?;
                let serialized =
                    serde_json::to_value(delta).map_err(|_| CaptureError::UnfaithfulDelta)?;
                if raw_delta != &serialized {
                    return Err(CaptureError::UnfaithfulDelta);
                }
                if matches!(delta, StreamDelta::CitationsDelta { .. })
                    && !self.blocks.get(index).is_some_and(|block| {
                        !block.closed && matches!(&block.typed, ContentBlock::Text { .. })
                    })
                {
                    return Err(CaptureError::InvalidStream);
                }
                if self.is_passive() {
                    self.native_failure_block = Some(*index);
                    let block = self.blocks.get(index).ok_or(CaptureError::InvalidStream)?;
                    let sizes = block
                        .native_retained_sizes()?
                        .add(block.native_delta_sizes(delta)?)?;
                    self.reserve_native_response(
                        Some((*index, sizes)),
                        self.native_metadata_sizes()?,
                    )?;
                }
                let block = self
                    .blocks
                    .get_mut(index)
                    .filter(|block| !block.closed)
                    .ok_or(CaptureError::InvalidStream)?;
                block.apply_delta(delta, raw_delta, self.native.as_ref())?;
            }
            MessageStreamEvent::ContentBlockStop { index } => {
                if self.phase != CapturePhase::Content {
                    return Err(CaptureError::InvalidStream);
                }
                if self.is_passive() {
                    self.native_failure_block = Some(*index);
                    let sizes = self
                        .blocks
                        .get(index)
                        .ok_or(CaptureError::InvalidStream)?
                        .native_closed_sizes()?;
                    self.reserve_native_response(
                        Some((*index, sizes)),
                        self.native_metadata_sizes()?,
                    )?;
                }
                let block = self
                    .blocks
                    .get_mut(index)
                    .filter(|block| !block.closed)
                    .ok_or(CaptureError::InvalidStream)?;
                block.close(self.native.as_ref())?;
                if matches!(&block.typed, ContentBlock::Thinking { thinking, signature }
                    if !thinking.is_empty() || !signature.is_empty())
                {
                    self.native_reasoning = Some(*index);
                }
            }
            MessageStreamEvent::MessageDelta { delta, .. } => {
                if self.phase != CapturePhase::Content
                    || delta.container.as_ref().is_some()
                    || self.blocks.values().any(|block| !block.closed)
                {
                    return Err(CaptureError::InvalidStream);
                }
                if let Some(reason) = delta.stop_reason.as_ref() {
                    if matches!(
                        reason,
                        xai_grok_sampling_types::messages::StopReason::Unknown(_)
                    ) {
                        return Err(CaptureError::InvalidStream);
                    }
                    self.phase = CapturePhase::Terminal;
                }
            }
            MessageStreamEvent::MessageStop => {
                if self.phase != CapturePhase::Terminal
                    || self.blocks.values().any(|block| !block.closed)
                {
                    return Err(CaptureError::InvalidStream);
                }
                self.phase = CapturePhase::Stopped;
            }
            MessageStreamEvent::Ping => match raw["type"].as_str() {
                Some("ping") => {}
                Some("content_block_delta") => return Err(CaptureError::UnfaithfulDelta),
                _ => return Err(CaptureError::UnknownEvent),
            },
            MessageStreamEvent::Error { .. } if self.is_passive() => {}
            MessageStreamEvent::Error { .. } => return Err(CaptureError::IncompleteStream),
        }
        Ok(())
    }

    #[cfg(test)]
    fn raw_start(&self, index: u32) -> Option<&Value> {
        self.blocks.get(&index).map(|block| &block.raw)
    }

    pub(crate) fn finish(
        &mut self,
        response: &ConversationResponse,
    ) -> Result<Vec<Value>, CaptureError> {
        if self.phase != CapturePhase::Stopped || self.blocks.values().any(|block| !block.closed) {
            return Err(CaptureError::IncompleteStream);
        }
        let paused = response.raw_stop_reason.as_deref() == Some("pause_turn");
        for block in self.blocks.values() {
            let serialized =
                serde_json::to_value(&block.typed).map_err(|_| CaptureError::CompletedMismatch)?;
            // Completed blocks have no decoder-default equivalence exceptions.
            if block.raw != serialized {
                return Err(CaptureError::CompletedMismatch);
            }
            if paused && matches!(block.typed, ContentBlock::ToolUse { .. }) {
                return Err(CaptureError::WaitingClientTool);
            }
        }
        if self.is_passive() {
            self.native_coordinates = self.blocks.keys().copied().collect();
        }
        Ok(std::mem::take(&mut self.blocks)
            .into_values()
            .map(|block| block.raw)
            .collect())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ContinuationBody {
    encoded: xai_grok_sampling_types::request_validation::EncodedMessagesRequest,
    paused_message: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ContinuationError {
    InvalidBody,
    UnfaithfulBody,
    Validation(xai_grok_sampling_types::request_validation::RequestValidationError),
}

impl ContinuationBody {
    pub(crate) fn new(
        encoded: xai_grok_sampling_types::request_validation::EncodedMessagesRequest,
    ) -> Self {
        Self {
            encoded,
            paused_message: None,
        }
    }

    pub(crate) fn encoded(
        &self,
    ) -> &xai_grok_sampling_types::request_validation::EncodedMessagesRequest {
        &self.encoded
    }

    pub(crate) fn append(&self, content: &[Value]) -> Result<Self, ContinuationError> {
        if content.is_empty() {
            return Ok(self.clone());
        }
        let mut body: Value = serde_json::from_slice(self.encoded.as_bytes())
            .map_err(|_| ContinuationError::InvalidBody)?;
        let messages = body
            .get_mut("messages")
            .and_then(Value::as_array_mut)
            .ok_or(ContinuationError::InvalidBody)?;
        // The suffix is owned by this logical request, never inferred from the last role.
        let paused_message = match self.paused_message {
            Some(index) => {
                if index + 1 != messages.len() {
                    return Err(ContinuationError::InvalidBody);
                }
                let message = messages
                    .get_mut(index)
                    .ok_or(ContinuationError::InvalidBody)?;
                if message.get("role").and_then(Value::as_str) != Some("assistant") {
                    return Err(ContinuationError::InvalidBody);
                }
                message
                    .get_mut("content")
                    .and_then(Value::as_array_mut)
                    .ok_or(ContinuationError::InvalidBody)?
                    .extend_from_slice(content);
                index
            }
            None => {
                let index = messages.len();
                messages.push(serde_json::json!({"role":"assistant","content":content}));
                index
            }
        };
        let request =
            serde_json::from_value(body.clone()).map_err(|_| ContinuationError::InvalidBody)?;
        let encoded =
            xai_grok_sampling_types::request_validation::validate_and_encode_messages_request(
                &request,
            )
            .map_err(ContinuationError::Validation)?;
        let encoded_body: Value = serde_json::from_slice(encoded.as_bytes())
            .map_err(|_| ContinuationError::InvalidBody)?;
        // Validation must cover the concrete body, not a lossy decoded surrogate.
        if encoded_body != body {
            return Err(ContinuationError::UnfaithfulBody);
        }
        Ok(Self {
            encoded,
            paused_message: Some(paused_message),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{StreamExt, stream};
    use serde_json::json;
    use std::time::Duration;

    use crate::events::SamplingEvent;
    use crate::stream::messages::stream_messages_tracked;
    use crate::types::RequestId;

    async fn collect_raw(
        rows: Vec<String>,
    ) -> (Result<CompletedAttempt, CaptureError>, Vec<SamplingEvent>) {
        let mut capture = AttemptCapture::default();
        let events = {
            let frames = stream::iter(rows)
                .map(|row| crate::client::parse_messages_stream_event(&row))
                .boxed();
            stream_messages_tracked(
                frames,
                None,
                RequestId::from("pause-capture"),
                Duration::from_secs(1),
                Some(&mut capture),
            )
            .collect::<Vec<_>>()
            .await
        };
        (
            capture
                .completed
                .ok_or(capture.failure.unwrap_or(CaptureError::IncompleteStream)),
            events,
        )
    }

    fn start() -> Value {
        json!({"type":"message_start","message":{
            "id":"msg_capture","type":"message","role":"assistant","model":"capture-model",
            "content":[],"stop_reason":null,"container":null,"stop_details":null,
            "usage":{"input_tokens":10,"output_tokens":0,"cache_read_input_tokens":2,
                "cache_creation_input_tokens":3,"cache_creation":{"ephemeral_5m_input_tokens":1,"ephemeral_1h_input_tokens":2},
                "output_tokens_details":{"thinking_tokens":1}}}})
    }

    fn terminal(reason: &str) -> Value {
        json!({"type":"message_delta","delta":{"stop_reason":reason,"stop_details":null},
            "usage":{"input_tokens":11,"output_tokens":7,"cache_read_input_tokens":4,
                "cache_creation_input_tokens":5,"output_tokens_details":{"thinking_tokens":2}}})
    }

    fn block(index: u32, content: Value) -> Value {
        json!({"type":"content_block_start","index":index,"content_block":content})
    }

    fn delta(index: u32, delta: Value) -> Value {
        json!({"type":"content_block_delta","index":index,"delta":delta})
    }

    fn stop(index: u32) -> Value {
        json!({"type":"content_block_stop","index":index})
    }

    fn rows(events: Vec<Value>) -> Vec<String> {
        events.into_iter().map(|event| event.to_string()).collect()
    }

    fn text_stream(reason: &str) -> Vec<Value> {
        vec![
            start(),
            block(0, json!({"type":"text","text":""})),
            delta(0, json!({"type":"text_delta","text":"working"})),
            stop(0),
            terminal(reason),
            json!({"type":"message_stop"}),
        ]
    }

    fn body_json(body: &ContinuationBody) -> Value {
        serde_json::from_slice(body.encoded().as_bytes()).unwrap()
    }

    fn initial_body(body: Value) -> ContinuationBody {
        let request = serde_json::from_value(body.clone()).unwrap();
        let encoded =
            xai_grok_sampling_types::request_validation::validate_and_encode_messages_request(
                &request,
            )
            .unwrap();
        let result = ContinuationBody::new(encoded);
        assert_eq!(
            body_json(&result),
            body,
            "initial fixture must be the actual validated outgoing body"
        );
        result
    }

    fn minimal_body(messages: Vec<Value>) -> Value {
        json!({"model":"continuation-model","messages":messages,"max_tokens":8192,"stream":true})
    }

    fn assistant_message(content: &[Value]) -> Value {
        json!({"role":"assistant","content":content})
    }

    async fn captured_segment(content: &[Value]) -> Vec<Value> {
        let mut events = vec![start()];
        for (index, content) in content.iter().enumerate() {
            events.extend([block(index as u32, content.clone()), stop(index as u32)]);
        }
        events.extend([terminal("pause_turn"), json!({"type":"message_stop"})]);
        collect_raw(rows(events)).await.0.unwrap().content
    }

    fn rich_body() -> Value {
        json!({
            "model":"bound-model-A",
            "messages":[
                {"role":"user","content":[{"type":"text","text":"original request","cache_control":{"type":"ephemeral"}}]},
                {"role":"assistant","content":[{"type":"tool_use","id":"client_prior","name":"fs__read_file","input":{"path":"a"}}]},
                {"role":"user","content":[{"type":"tool_result","tool_use_id":"client_prior","content":[{"type":"text","text":"prior result"}]}]}
            ],
            "max_tokens":8192,
            "system":[{"type":"text","text":"unchanged system"},{"type":"text","text":"unchanged tail","cache_control":{"type":"ephemeral","ttl":"1h"}}],
            "tools":[
                {"type":"tool_search_tool_bm25_20251119","name":"tool_search_tool_bm25"},
                {"name":"fs__read_file","description":"read a file","input_schema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]},"defer_loading":true},
                {"name":"eager","input_schema":{"type":"object"},"defer_loading":false}
            ],
            "tool_choice":{"type":"auto","disable_parallel_tool_use":false},
            "temperature":0.7,"top_p":0.9,"top_k":42,
            "stream":true,"stop_sequences":["\nSTOP","α\""],
            "thinking":{"type":"disabled"},
            "output_config":{"effort":null,"format":{"type":"json_schema","schema":{"type":"object","properties":{"answer":{"type":"string"}}}}},
            "metadata":{"user_id":null}
        })
    }

    #[tokio::test]
    async fn encoder_appends_exact_suffix_and_preserves_tools_options() {
        let first = captured_segment(&[
            json!({"type":"text","text":"lead α"}),
            json!({"type":"thinking","thinking":"reasoning","signature":"opaque-signature"}),
            json!({"type":"text","text":"separate boundary"}),
            json!({"type":"redacted_thinking","data":"opaque-redacted"}),
            json!({"type":"server_tool_use","id":"srvtoolu_pending","name":"tool_search_tool_bm25","input":{"query":"read","nested":{"n":1}}}),
        ]).await;
        let second = captured_segment(&[
            json!({"type":"tool_search_tool_result","tool_use_id":"srvtoolu_pending","content":{"type":"tool_search_tool_search_result","tool_references":[{"type":"tool_reference","tool_name":"fs__read_file"}],"nested":{"empty":[],"opaque":"value"}}}),
            json!({"type":"tool_search_tool_result","tool_use_id":"srvtoolu_empty","content":{"type":"tool_search_tool_search_result","tool_references":[]}}),
            json!({"type":"tool_search_tool_result","tool_use_id":"srvtoolu_error","content":{"type":"tool_search_tool_result_error","error_code":"unavailable"}}),
            json!({"type":"text","text":"tail"}),
        ]).await;
        let empty = captured_segment(&[]).await;
        let original = rich_body();
        let initial = initial_body(original.clone());
        let leading_empty = initial.append(&empty).unwrap();
        assert_eq!(
            leading_empty.encoded().as_bytes(),
            initial.encoded().as_bytes(),
            "a leading zero-block segment resends the same body"
        );
        let continued = leading_empty.append(&first).unwrap();
        let mut expected = original.clone();
        expected["messages"]
            .as_array_mut()
            .unwrap()
            .push(assistant_message(&first));
        assert_eq!(
            body_json(&continued),
            expected,
            "continued body must preserve the exact suffix and every original field"
        );
        assert_eq!(
            body_json(&initial),
            original,
            "construction must not mutate the prior outgoing body"
        );
        assert_eq!(
            continued.append(&empty).unwrap().encoded().as_bytes(),
            continued.encoded().as_bytes(),
            "a later zero-block segment must leave the body unchanged"
        );
        let twice = continued.append(&second).unwrap();
        let mut combined = first.clone();
        combined.extend_from_slice(&second);
        *expected["messages"]
            .as_array_mut()
            .unwrap()
            .last_mut()
            .unwrap() = assistant_message(&combined);
        assert_eq!(
            body_json(&twice),
            expected,
            "repeated pauses must extend exactly one assistant suffix"
        );
        assert_eq!(
            twice.append(&empty).unwrap().encoded().as_bytes(),
            twice.encoded().as_bytes()
        );

        let mut enabled = original.clone();
        enabled.as_object_mut().unwrap().remove("top_p");
        enabled.as_object_mut().unwrap().remove("top_k");
        enabled["thinking"] = json!({"type":"enabled","budget_tokens":1024});
        let enabled_initial = initial_body(enabled.clone());
        enabled["messages"]
            .as_array_mut()
            .unwrap()
            .push(assistant_message(&first));
        assert_eq!(body_json(&enabled_initial.append(&first).unwrap()), enabled);

        let mut prefilled = original;
        prefilled["messages"]
            .as_array_mut()
            .unwrap()
            .push(assistant_message(&[
                json!({"type":"text","text":"old assistant"}),
            ]));
        let prefilled_initial = initial_body(prefilled.clone());
        prefilled["messages"]
            .as_array_mut()
            .unwrap()
            .push(assistant_message(&first));
        assert_eq!(
            body_json(
                &prefilled_initial
                    .append(&empty)
                    .unwrap()
                    .append(&first)
                    .unwrap()
            ),
            prefilled,
            "suffix state must not be inferred from the initial trailing role"
        );

        assert_eq!(
            initial
                .append(&[json!({"type":"text","text":"keep","future_field":"opaque"})])
                .err(),
            Some(ContinuationError::UnfaithfulBody),
            "encoding must refuse a lossy typed body"
        );
    }

    fn initial_for_final_bytes(bytes: u64, content: &[Value]) -> ContinuationBody {
        let mut body = minimal_body(vec![
            json!({"role":"user","content":"a".repeat(31_200)});
            1024
        ]);
        let mut probe = body.clone();
        probe["messages"]
            .as_array_mut()
            .unwrap()
            .push(assistant_message(content));
        let padding = bytes as usize - serde_json::to_vec(&probe).unwrap().len();
        let last = body["messages"].as_array_mut().unwrap().last_mut().unwrap();
        last["content"] = json!("a".repeat(31_200 + padding));
        assert!(
            serde_json::to_vec(last).unwrap().len() / 4 < 100_000,
            "N2 fixture must not trip N3 first"
        );
        let mut expected = body.clone();
        expected["messages"]
            .as_array_mut()
            .unwrap()
            .push(assistant_message(content));
        assert_eq!(serde_json::to_vec(&expected).unwrap().len() as u64, bytes);
        initial_body(body)
    }

    #[test]
    fn encoder_validates_concrete_limits_and_client_pairing() {
        use xai_grok_sampling_types::request_validation::{
            ItemRef, MAX_MODEL_CONTEXT_ITEM_TOKENS, RequestValidationError,
        };

        let content = [json!({"type":"text","text":"next"})];
        for count in [
            MAX_MESSAGES_REQUEST_ITEMS - 2,
            MAX_MESSAGES_REQUEST_ITEMS - 1,
            MAX_MESSAGES_REQUEST_ITEMS,
        ] {
            let initial = initial_body(minimal_body(vec![
                json!({"role":"user","content":"u"});
                count
            ]));
            let expected =
                (count == MAX_MESSAGES_REQUEST_ITEMS).then_some(ContinuationError::Validation(
                    RequestValidationError::TooManyMessages { count: count + 1 },
                ));
            assert_eq!(
                initial.append(&content).err(),
                expected,
                "N1 must validate the continued message count"
            );
            assert_eq!(
                body_json(&initial)["messages"].as_array().unwrap().len(),
                count
            );
        }

        let first = [json!({"type":"text","text":"first segment"})];
        let initial = initial_body(minimal_body(vec![json!({"role":"user","content":"u"})]));
        let once = initial.append(&first).unwrap();
        let empty_second = json!({"type":"text","text":""});
        let overhead = serde_json::to_vec(&assistant_message(&[first[0].clone(), empty_second]))
            .unwrap()
            .len();
        for bytes in [
            MAX_MODEL_CONTEXT_ITEM_TOKENS * 4 - 1,
            MAX_MODEL_CONTEXT_ITEM_TOKENS * 4,
            (MAX_MODEL_CONTEXT_ITEM_TOKENS + 1) * 4,
        ] {
            let second = [json!({"type":"text","text":"a".repeat(bytes as usize - overhead)})];
            let expected = (bytes / 4 > MAX_MODEL_CONTEXT_ITEM_TOKENS).then_some(
                ContinuationError::Validation(RequestValidationError::ItemTokenLimitExceeded {
                    item: ItemRef::MessageItem(1),
                    estimated_tokens: bytes / 4,
                }),
            );
            assert_eq!(
                once.append(&second).err(),
                expected,
                "N3 must price the whole merged assistant item, not separate segments"
            );
        }
        assert_eq!(
            body_json(&once)["messages"][1],
            assistant_message(&first),
            "failed validation must not change the authoritative body"
        );

        for bytes in [
            MAX_ENCODED_MESSAGES_REQUEST_BYTES - 1,
            MAX_ENCODED_MESSAGES_REQUEST_BYTES,
            MAX_ENCODED_MESSAGES_REQUEST_BYTES + 1,
        ] {
            let initial = initial_for_final_bytes(bytes, &content);
            let result = initial.append(&content);
            if bytes <= MAX_ENCODED_MESSAGES_REQUEST_BYTES {
                assert_eq!(
                    result.unwrap().encoded().len() as u64,
                    bytes,
                    "N2 must encode the actual continued body"
                );
            } else {
                assert_eq!(
                    result.err(),
                    Some(ContinuationError::Validation(
                        RequestValidationError::EncodedBodyTooLarge { bytes }
                    )),
                    "N2 must reject the concrete body one byte over its cap"
                );
            }
        }

        let pending = [
            json!({"type":"server_tool_use","id":"srvtoolu_pending","name":"tool_search_tool_bm25","input":{"query":"read"}}),
        ];
        assert!(
            initial.append(&pending).is_ok(),
            "pending server calls retain the existing inert allowance"
        );
        let client = [
            json!({"type":"tool_use","id":"client_pending","name":"fs__read_file","input":{"path":"a"}}),
        ];
        assert_eq!(
            initial.append(&client).err(),
            Some(ContinuationError::Validation(
                RequestValidationError::UnansweredToolUse {
                    id: "client_pending".to_owned()
                }
            )),
            "unmatched client calls must not receive a pairing waiver"
        );
        let orphan = [
            json!({"type":"tool_result","tool_use_id":"client_missing","content":"not an answer"}),
        ];
        assert_eq!(
            initial.append(&orphan).err(),
            Some(ContinuationError::Validation(
                RequestValidationError::UnpairedToolResult {
                    id: "client_missing".to_owned()
                }
            )),
            "unpaired client results must be rejected"
        );
        assert!(
            initial_body(rich_body()).append(&pending).is_ok(),
            "prior paired client calls must remain valid"
        );

        let misplaced = [
            json!({"type":"text","text":"marked","cache_control":{"type":"ephemeral"}}),
            json!({"type":"text","text":"later"}),
        ];
        assert_eq!(
            initial.append(&misplaced).err(),
            Some(ContinuationError::Validation(
                RequestValidationError::CacheMarkerMisplaced { at: 0 }
            )),
            "continued content must pass the unchanged cache invariant"
        );
    }

    #[tokio::test]
    async fn ordered_capture_preserves_interleaved_blocks() {
        let expected = json!([
            {"type":"text","text":"lead α"},
            {"type":"server_tool_use","id":"srvtoolu_search","name":"tool_search_tool_bm25","input":{"query":"shipping"}},
            {"type":"thinking","thinking":"reasoning","signature":"opaque-signature"},
            {"type":"tool_search_tool_result","tool_use_id":"srvtoolu_search","content":{"type":"tool_search_tool_search_result","tool_references":[{"type":"tool_reference","tool_name":"lookup"}],"nested":{"empty":[],"opaque":"opaque-value"}}},
            {"type":"text","text":"middle"},
            {"type":"redacted_thinking","data":"opaque-redacted"},
            {"type":"tool_search_tool_result","tool_use_id":"srvtoolu_empty","content":{"type":"tool_search_tool_search_result","tool_references":[]}},
            {"type":"tool_search_tool_result","tool_use_id":"srvtoolu_error","content":{"type":"tool_search_tool_result_error","error_code":"unavailable"}},
            {"type":"server_tool_use","id":"srvtoolu_pending","name":"tool_search_tool_regex","input":{"query":"^next$","nested":{"n":1}}}
        ]);
        let blocks = expected.as_array().unwrap();
        let mut events = vec![
            start(),
            block(0, json!({"type":"text","text":"lead "})),
            delta(0, json!({"type":"text_delta","text":"α"})),
            stop(0),
            block(
                1,
                json!({"type":"server_tool_use","id":"srvtoolu_search","name":"tool_search_tool_bm25","input":{}}),
            ),
            delta(
                1,
                json!({"type":"input_json_delta","partial_json":"{\"query\":"}),
            ),
            delta(
                1,
                json!({"type":"input_json_delta","partial_json":"\"shipping\"}"}),
            ),
            stop(1),
            block(2, json!({"type":"thinking","thinking":"","signature":""})),
            delta(2, json!({"type":"thinking_delta","thinking":"reasoning"})),
            delta(
                2,
                json!({"type":"signature_delta","signature":"opaque-signature"}),
            ),
            stop(2),
        ];
        for (index, content) in blocks.iter().enumerate().skip(3) {
            events.extend([block(index as u32, content.clone()), stop(index as u32)]);
        }
        events.extend([terminal("end_turn"), json!({"type":"message_stop"})]);
        let (attempt, _) = collect_raw(rows(events.clone())).await;
        let attempt = attempt.expect("complete faithful stream must be captured");
        assert_eq!(
            json!(attempt.content),
            expected,
            "ordered provider blocks must survive capture"
        );

        events
            .iter_mut()
            .find(|event| event["type"] == "message_delta")
            .unwrap()["delta"]["stop_reason"] = json!("pause_turn");
        let (paused, emitted) = collect_raw(rows(events)).await;
        assert_eq!(
            json!(
                paused
                    .expect("complete faithful pause must be captured")
                    .content
            ),
            expected
        );
        assert!(
            !emitted
                .iter()
                .any(|event| matches!(event, SamplingEvent::Completed { .. })),
            "private pause is not logical success"
        );

        let raw: Vec<String> = serde_json::from_str(include_str!(
            "../testdata/stream_equiv/eq-14-redacted-thinking/messages_sse.json"
        ))
        .unwrap();
        let data = raw
            .iter()
            .filter_map(|line| line.strip_prefix("data: ").map(str::to_owned))
            .collect();
        let (redacted, _) = collect_raw(data).await;
        assert_eq!(
            json!(redacted.unwrap().content),
            json!([
                {"type":"redacted_thinking","data":"opaque_encrypted_data_xyz"}, {"type":"text","text":"done"}
            ])
        );
    }

    #[tokio::test]
    async fn start_and_delta_unknown_fields_are_incomplete() {
        let mut unknown_start = text_stream("end_turn");
        unknown_start[1]["content_block"]["future_field"] = json!({"opaque":"keep-or-refuse"});
        let (attempt, _) = collect_raw(rows(unknown_start)).await;
        assert!(
            attempt.is_err(),
            "unmodelled provider fields must refuse capture"
        );

        let cases = [
            (
                json!({"type":"text","text":""}),
                json!({"type":"text_delta","text":"text"}),
            ),
            (
                json!({"type":"server_tool_use","id":"srvtoolu_x","name":"tool_search_tool_bm25","input":{}}),
                json!({"type":"input_json_delta","partial_json":"{}"}),
            ),
            (
                json!({"type":"thinking","thinking":"","signature":""}),
                json!({"type":"thinking_delta","thinking":"reason"}),
            ),
            (
                json!({"type":"thinking","thinking":"reason","signature":""}),
                json!({"type":"signature_delta","signature":"opaque"}),
            ),
        ];
        for (content, mut update) in cases {
            update["future_field"] = json!("must-not-disappear");
            let (attempt, _) = collect_raw(rows(vec![
                start(),
                block(0, content),
                delta(0, update),
                stop(0),
                terminal("end_turn"),
                json!({"type":"message_stop"}),
            ]))
            .await;
            assert!(
                attempt.is_err(),
                "unknown field in a known delta must refuse capture"
            );
        }
        for events in [
            vec![
                start(),
                block(0, json!({"type":"future_block","opaque":"value"})),
                stop(0),
                terminal("end_turn"),
                json!({"type":"message_stop"}),
            ],
            vec![
                start(),
                block(0, json!({"type":"text","text":""})),
                delta(0, json!({"type":"future_delta","text":"lost"})),
                stop(0),
                terminal("end_turn"),
                json!({"type":"message_stop"}),
            ],
        ] {
            let (attempt, _) = collect_raw(rows(events)).await;
            assert!(
                attempt.is_err(),
                "unmodelled content types must refuse capture"
            );
        }
        let (valid, _) = collect_raw(rows(text_stream("end_turn"))).await;
        assert!(valid.is_ok(), "refusal cannot replace all capture outcomes");
    }

    #[tokio::test]
    async fn signatureless_thinking_deltas_pass_closed_default_rule() {
        let skeleton = json!({"type":"thinking","thinking":"prefix "});
        let mut capture = AttemptCapture::default();
        capture
            .observe(&crate::client::parse_messages_stream_event(&start().to_string()).unwrap())
            .unwrap();
        let frame =
            crate::client::parse_messages_stream_event(&block(0, skeleton.clone()).to_string())
                .unwrap();
        capture
            .observe(&frame)
            .expect("signature-less thinking starts must be supported");
        assert_eq!(
            capture.raw_start(0),
            Some(&skeleton),
            "signature-less start must preserve field absence"
        );
        let events = vec![
            start(),
            block(0, skeleton),
            delta(0, json!({"type":"thinking_delta","thinking":"reason"})),
            delta(
                0,
                json!({"type":"signature_delta","signature":"opaque-signature"}),
            ),
            stop(0),
            terminal("pause_turn"),
            json!({"type":"message_stop"}),
        ];
        let (attempt, _) = collect_raw(rows(events.clone())).await;
        assert_eq!(
            json!(
                attempt
                    .expect("signature-less thinking with signature deltas must be captured")
                    .content
            ),
            json!([{"type":"thinking","thinking":"prefix reason","signature":"opaque-signature"}])
        );

        let mut no_signature = events.clone();
        no_signature.remove(3);
        let (attempt, _) = collect_raw(rows(no_signature)).await;
        assert!(
            attempt.is_err(),
            "completed comparison must not apply the start-only default exception"
        );
        let mut unknown = events;
        unknown[1]["content_block"]["future_default"] = json!("");
        let (attempt, _) = collect_raw(rows(unknown)).await;
        assert!(
            attempt.is_err(),
            "the start default equivalence list is closed"
        );
        let mut null_cache = text_stream("end_turn");
        null_cache[1]["content_block"]["cache_control"] = Value::Null;
        let (attempt, _) = collect_raw(rows(null_cache)).await;
        assert!(
            attempt.is_err(),
            "no other defaulted field may be normalized"
        );
    }

    #[tokio::test]
    async fn resp001_skeletons_assemble_without_false_mismatch() {
        let raw: Vec<String> = include_str!("../tests/fixtures/resp-001.raw")
            .lines()
            .filter_map(|line| {
                line.strip_prefix("data:")
                    .map(|data| data.trim_start().to_owned())
            })
            .collect();
        let expected = json!([
            {"type":"text","text":"I'll search for the shipping ETA tool first."},
            {"type":"server_tool_use","id":"srvtoolu_vrtx_01Hh2DfGwhjyYDSnUWDFveu3","name":"tool_search_tool_bm25","input":{"query":"shipping ETA estimated delivery date order"}},
            {"type":"tool_search_tool_result","tool_use_id":"srvtoolu_vrtx_01Hh2DfGwhjyYDSnUWDFveu3","content":{"type":"tool_search_tool_search_result","tool_references":[{"type":"tool_reference","tool_name":"lookup_shipping_eta"}]}},
            {"type":"text","text":"I found the tool. Now calling it once with the specified order ID."},
            {"type":"tool_use","id":"toolu_vrtx_01AZWFeGUbgxTKFbK9236Fhw","name":"lookup_shipping_eta","input":{"order_id":"order_probe_42"}}
        ]);
        let (attempt, _) = collect_raw(raw.clone()).await;
        assert_eq!(
            json!(
                attempt
                    .expect("delta-bearing resp-001 capture must complete")
                    .content
            ),
            expected,
            "resp-001 must retain assembled content rather than its start skeletons"
        );
        let mut events: Vec<Value> = raw
            .iter()
            .map(|data| serde_json::from_str(data).unwrap())
            .collect();
        assert_eq!(
            events
                .iter()
                .filter(|event| event["index"] == 4 && event["type"] == "content_block_delta")
                .count(),
            6
        );
        events
            .iter_mut()
            .find(|event| event["type"] == "message_delta")
            .unwrap()["delta"]["stop_reason"] = json!("pause_turn");
        let (client_pause, _) = collect_raw(rows(events.clone())).await;
        assert!(
            client_pause.is_err(),
            "a waiting client call is not a valid pause"
        );
        events.retain(|event| event.get("index") != Some(&json!(4)));
        let (paused, emitted) = collect_raw(rows(events)).await;
        let paused = paused.expect("client-free resp-001 pause must complete faithfully");
        assert_eq!(paused.content, expected.as_array().unwrap()[..4]);
        assert_eq!(
            (
                paused.response.raw_stop_reason.as_deref(),
                paused.response.message_id.as_deref()
            ),
            (
                Some("pause_turn"),
                Some("msg_vrtx_011Cfe5vLgP2239uDfbgcRi8")
            )
        );
        assert_eq!(
            paused.response.usage,
            Some(xai_grok_sampling_types::TokenUsage {
                prompt_tokens: 1589,
                completion_tokens: 167,
                total_tokens: 1756,
                ..Default::default()
            })
        );
        assert!(
            !emitted
                .iter()
                .any(|event| matches!(event, SamplingEvent::Completed { .. }))
        );
        let (sh4, _) = collect_raw(rows(text_stream("pause_turn"))).await;
        assert_eq!(
            json!(sh4.unwrap().content),
            json!([{"type":"text","text":"working"}])
        );
    }

    #[tokio::test]
    async fn pause_capture_requires_complete_valid_stream() {
        let valid = text_stream("pause_turn");
        let (attempt, emitted) = collect_raw(rows(valid.clone())).await;
        assert!(
            attempt.is_ok(),
            "a complete pause must include finalized usage"
        );
        let attempt = attempt.unwrap();
        assert_eq!(
            json!({"content":attempt.content,"id":attempt.response.message_id,
            "wire_stop":attempt.response.raw_stop_reason,"stop":attempt.response.stop_reason,
            "usage":attempt.response.usage,"chunks":attempt.response.message_chunks_emitted}),
            json!({"content":[{"type":"text","text":"working"}],"id":"msg_capture","wire_stop":"pause_turn",
                "stop":null,"usage":{"prompt_tokens":20,"completion_tokens":7,"total_tokens":27,"reasoning_tokens":2,
                    "cached_prompt_tokens":4,"cache_creation_prompt_tokens":5,"cache_creation_5m_input_tokens":1,
                    "cache_creation_1h_input_tokens":2},"chunks":1}),
            "pause capture must contain terminal usage exactly once"
        );
        assert_eq!(attempt.metrics.chunk_count, 1);
        assert!(
            !emitted.iter().any(|event| matches!(
                event,
                SamplingEvent::Completed { .. } | SamplingEvent::Failed { .. }
            )),
            "private capture must not emit a logical terminal"
        );

        for end in 0..valid.len() {
            let (truncated, _) = collect_raw(rows(valid[..end].to_vec())).await;
            assert!(
                truncated.is_err(),
                "missing message_stop must not accept pause at prefix {end}"
            );
        }
        let mut unclosed = valid.clone();
        unclosed.remove(3);
        let mut no_start = valid.clone();
        no_start.remove(0);
        let mut after_terminal = valid.clone();
        after_terminal.insert(5, block(1, json!({"type":"text","text":"late"})));
        after_terminal.insert(6, stop(1));
        let mut duplicate_terminal = valid.clone();
        duplicate_terminal.insert(5, terminal("pause_turn"));
        let mut mismatched_delta = valid.clone();
        mismatched_delta[2] = delta(0, json!({"type":"thinking_delta","thinking":"wrong block"}));
        let mut no_terminal = valid.clone();
        no_terminal.remove(4);
        for events in [
            unclosed,
            no_start,
            after_terminal,
            duplicate_terminal,
            mismatched_delta,
            no_terminal,
        ] {
            let (attempt, _) = collect_raw(rows(events)).await;
            assert!(
                attempt.is_err(),
                "invalid stream grammar must not accept pause"
            );
        }
        let mut malformed = rows(valid[..4].to_vec());
        malformed.push("{".to_owned());
        let (attempt, _) = collect_raw(malformed).await;
        assert!(
            attempt.is_err(),
            "malformed raw data cannot complete capture"
        );
        let (empty, _) = collect_raw(rows(vec![
            start(),
            terminal("pause_turn"),
            json!({"type":"message_stop"}),
        ]))
        .await;
        assert!(
            empty.unwrap().content.is_empty(),
            "zero-block pause still has a completed attempt"
        );
    }

    fn mss_u1_server_fixture(caller: Option<Value>, fragmented: bool) -> (Vec<Value>, Vec<Value>) {
        let input = json!({"query":"read α", "opaque":{"parts":["a", "b"]}});
        let mut search = json!({"type":"server_tool_use","id":"srvtoolu_mss_u1",
            "name":"tool_search_tool_bm25","input":input});
        if let Some(caller) = caller {
            search["caller"] = caller;
        }
        let result = json!({"type":"tool_search_tool_result","tool_use_id":"srvtoolu_mss_u1",
            "content":{"type":"tool_search_tool_search_result",
                "tool_references":[{"type":"tool_reference","tool_name":"fs__read_file"}]}});
        let expected = vec![search.clone(), result.clone()];
        if fragmented {
            search["input"] = json!({});
        }
        let mut wire = vec![start(), block(3, search)];
        if fragmented {
            wire.push(delta(
                3,
                json!({"type":"input_json_delta","partial_json":"{\"query\":\"read "}),
            ));
            wire.push(delta(
                3,
                json!({"type":"input_json_delta",
                "partial_json":"α\",\"opaque\":{\"parts\":[\"a\",\"b\"]}}"}),
            ));
        }
        wire.extend([
            stop(3),
            block(9, result),
            stop(9),
            terminal("end_turn"),
            json!({"type":"message_stop"}),
        ]);
        (wire, expected)
    }

    #[tokio::test]
    async fn mss_u1_caller_survives_decode_capture_encode() {
        let callers = [
            Some(json!({"type":"direct","opaque":{"exact":"α"}})),
            Some(
                json!({"type":"code_execution_20250825","tool_id":"srvtoolu_origin_α",
                "opaque":[1, false]}),
            ),
            Some(
                json!({"type":"code_execution_20260120","tool_id":"srvtoolu_origin_2026",
                "opaque":{"null":null}}),
            ),
            None,
        ];
        for caller in callers {
            for fragmented in [false, true] {
                let (wire, expected) = mss_u1_server_fixture(caller.clone(), fragmented);
                let (attempt, output) = collect_raw(rows(wire)).await;
                let attempt = attempt.expect("MSS_U1_SERVER_CALLER_CAPTURE");
                assert_eq!(attempt.content, expected, "MSS_U1_ORDERED_CALLER_CONTENT");
                let typed: Vec<ContentBlock> =
                    serde_json::from_value(json!(attempt.content)).unwrap();
                assert_eq!(
                    serde_json::to_value(typed).unwrap(),
                    json!(expected),
                    "MSS_U1_TYPED_CALLER_ROUNDTRIP"
                );
                let discovery: Vec<Value> = output
                    .iter()
                    .filter_map(|event| match event {
                        SamplingEvent::MessagesToolSearchDisplay { item, .. } => {
                            Some(item.raw().clone())
                        }
                        _ => None,
                    })
                    .collect();
                assert_eq!(discovery, expected, "MSS_U1_DISCOVERY_CALLER_FIDELITY");
                assert!(
                    !output
                        .iter()
                        .any(|event| matches!(event, SamplingEvent::ToolCallDelta { .. })),
                    "MSS_U1_SERVER_CALL_MUST_NOT_DISPATCH_CLIENT"
                );
                let original = rich_body();
                let initial = initial_body(original.clone());
                let appended = initial.append(&attempt.content).unwrap();
                let mut expected_body = original.clone();
                expected_body["messages"]
                    .as_array_mut()
                    .unwrap()
                    .push(assistant_message(&expected));
                assert_eq!(
                    body_json(&appended),
                    expected_body,
                    "MSS_U1_CONCRETE_CALLER_APPEND"
                );
                assert_eq!(body_json(&initial), original);
            }
        }
        for caller in [
            json!(null),
            json!("direct"),
            json!([]),
            json!({}),
            json!({"type":"future_caller"}),
            json!({"type":"code_execution_20250825"}),
            json!({"type":"code_execution_20260120","tool_id":null}),
            json!({"type":"code_execution_20260120","tool_id":""}),
            json!({"type":"code_execution_20260120","tool_id":7}),
        ] {
            let (wire, _) = mss_u1_server_fixture(Some(caller), false);
            let (attempt, output) = collect_raw(rows(wire)).await;
            assert!(attempt.is_err(), "MSS_U1_INVALID_CALLER_NOT_CAPTURED");
            assert!(
                !output
                    .iter()
                    .any(|event| matches!(event, SamplingEvent::Completed { .. })),
                "MSS_U1_INVALID_CALLER_NOT_COMPLETED"
            );
        }
    }

    #[tokio::test]
    async fn mss_u1_client_caller_capture_preserves_pairing_rejection() {
        let (mut wire, mut expected) = mss_u1_server_fixture(Some(json!({"type":"direct"})), true);
        wire.truncate(wire.len() - 2);
        let client = json!({"type":"tool_use","id":"client_mss_actual","name":"fs__read_file",
            "input":{"path":"a"},"caller":{"type":"direct","opaque":"client-exact"}});
        wire.extend([
            block(12, client.clone()),
            stop(12),
            terminal("tool_use"),
            json!({"type":"message_stop"}),
        ]);
        expected.push(client);
        let (attempt, output) = collect_raw(rows(wire)).await;
        let attempt = attempt.expect("MSS_U1_CLIENT_CALLER_CAPTURE");
        assert_eq!(attempt.content, expected, "MSS_U1_CLIENT_CALLER_FIDELITY");
        let typed: Vec<ContentBlock> = serde_json::from_value(json!(attempt.content)).unwrap();
        assert_eq!(serde_json::to_value(typed).unwrap(), json!(expected));
        let client_ids: Vec<_> = output
            .iter()
            .filter_map(|event| match event {
                SamplingEvent::ToolCallDelta { id: Some(id), .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(client_ids, vec!["client_mss_actual"]);
        assert_eq!(initial_body(rich_body()).append(&attempt.content).err(),
            Some(ContinuationError::Validation(
                xai_grok_sampling_types::request_validation::RequestValidationError::UnansweredToolUse {
                    id: "client_mss_actual".to_owned(),
                })), "MSS_U1_ACTUAL_CLIENT_PAIRING_REJECTION");
    }

    fn mss_u2_citation_objects() -> Vec<Value> {
        vec![
            json!({"type":"char_location","cited_text":"char α","document_index":0,
                "document_title":null,"start_char_index":0,"end_char_index":4,
                "file_id":"file_opaque_α","extra":{"keep":[null,1]}}),
            json!({"type":"page_location","cited_text":"page","document_index":2,
                "document_title":"PDF","start_page_number":1,"end_page_number":2,"file_id":null}),
            json!({"type":"content_block_location","cited_text":"blocks","document_index":1,
                "document_title":null,"start_block_index":2,"end_block_index":4,"file_id":"file_blocks"}),
            json!({"type":"web_search_result_location","cited_text":"web","encrypted_index":"opaque_α\\\"",
                "title":null,"url":"https://example.invalid/web","extra":false}),
            json!({"type":"search_result_location","cited_text":"search","search_result_index":3,
                "source":"https://example.invalid/source","title":null,"start_block_index":1,"end_block_index":3}),
        ]
    }

    fn mss_u2_citation_fixture(
        presence: Option<Value>,
        with_deltas: bool,
    ) -> (Vec<Value>, Vec<Value>) {
        let mut first = json!({"type":"text","text":"first α"});
        let mut second = json!({"type":"text","text":"second"});
        if let Some(presence) = presence {
            first["citations"] = presence.clone();
            second["citations"] = presence;
        }
        let thinking = json!({"type":"thinking","thinking":"why","signature":"sig_exact"});
        let server = json!({"type":"server_tool_use","id":"srvtoolu_u2","name":"tool_search_tool_bm25",
            "input":{"query":"read"},"caller":{"type":"direct","opaque":"keep"}});
        let result = json!({"type":"tool_search_tool_result","tool_use_id":"srvtoolu_u2",
            "content":{"type":"tool_search_tool_search_result","tool_references":[]}});
        let mut wire = vec![
            start(),
            block(3, first.clone()),
            block(4, json!({"type":"thinking","thinking":"","signature":""})),
            delta(4, json!({"type":"thinking_delta","thinking":"why"})),
            delta(4, json!({"type":"signature_delta","signature":"sig_exact"})),
            stop(4),
            block(11, second.clone()),
        ];
        if with_deltas {
            let citations = mss_u2_citation_objects();
            wire.push(delta(3, json!({"type":"text_delta","text":" + one"})));
            wire.push(delta(11, json!({"type":"text_delta","text":" + two"})));
            first["text"] = json!("first α + one");
            second["text"] = json!("second + two");
            for (ordinal, index) in [(0, 3), (3, 11), (1, 3), (2, 11), (4, 3)] {
                wire.push(delta(
                    index,
                    json!({"type":"citations_delta","citation":citations[ordinal]}),
                ));
            }
            let initial_first = first
                .get("citations")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let initial_second = second
                .get("citations")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut expected_first = initial_first;
            expected_first.extend([
                citations[0].clone(),
                citations[1].clone(),
                citations[4].clone(),
            ]);
            let mut expected_second = initial_second;
            expected_second.extend([citations[3].clone(), citations[2].clone()]);
            first["citations"] = json!(expected_first);
            second["citations"] = json!(expected_second);
        }
        wire.extend([
            stop(11),
            stop(3),
            block(20, server.clone()),
            stop(20),
            block(21, result.clone()),
            stop(21),
            terminal("end_turn"),
            json!({"type":"message_stop"}),
        ]);
        (wire, vec![first, thinking, second, server, result])
    }

    #[tokio::test]
    async fn mss_u2_citations_start_and_delta_roundtrip() {
        for presence in [
            None,
            Some(Value::Null),
            Some(json!([])),
            Some(json!([mss_u2_citation_objects()[0].clone()])),
        ] {
            for with_deltas in [false, true] {
                let (wire, expected) = mss_u2_citation_fixture(presence.clone(), with_deltas);
                let (attempt, output) = collect_raw(rows(wire)).await;
                let attempt = attempt.expect("MSS_U2_EXACT_ORDERED_CAPTURE");
                assert_eq!(attempt.content, expected, "MSS_U2_INDEXED_CITATION_ORDER");
                let typed: Vec<ContentBlock> =
                    serde_json::from_value(json!(attempt.content)).unwrap();
                assert_eq!(
                    serde_json::to_value(typed).unwrap(),
                    json!(expected),
                    "MSS_U2_TYPED_RAW_EQUALITY"
                );
                let original = rich_body();
                let appended = initial_body(original.clone())
                    .append(&attempt.content)
                    .unwrap();
                let mut expected_body = original;
                expected_body["messages"]
                    .as_array_mut()
                    .unwrap()
                    .push(assistant_message(&expected));
                assert_eq!(
                    body_json(&appended),
                    expected_body,
                    "MSS_U2_EXACT_CITATION_REENCODE"
                );
                assert!(
                    !output
                        .iter()
                        .any(|event| matches!(event, SamplingEvent::ToolCallDelta { .. }))
                );
            }
        }
    }

    #[tokio::test]
    async fn mss_u2_citations_reject_wrong_index_or_kind() {
        let (valid, _) = mss_u2_citation_fixture(None, true);
        collect_raw(rows(valid))
            .await
            .0
            .expect("MSS_U2_VALID_CITATION_SEQUENCE");
        let citation = mss_u2_citation_objects()[0].clone();
        let text = json!({"type":"text","text":"safe"});
        let terminal_frames = || [terminal("end_turn"), json!({"type":"message_stop"})];
        let mut invalid = vec![
            vec![
                delta(3, json!({"type":"citations_delta","citation":citation})),
                start(),
                block(3, text.clone()),
                stop(3),
            ],
            vec![
                start(),
                block(3, text.clone()),
                delta(55, json!({"type":"citations_delta","citation":citation})),
                stop(3),
            ],
            vec![
                start(),
                block(3, text.clone()),
                stop(3),
                delta(3, json!({"type":"citations_delta","citation":citation})),
            ],
            vec![
                start(),
                block(3, text.clone()),
                stop(3),
                terminal("end_turn"),
                delta(3, json!({"type":"citations_delta","citation":citation})),
            ],
            vec![
                start(),
                block(3, text.clone()),
                json!({"type":"content_block_delta","delta":{"type":"citations_delta","citation":citation}}),
                stop(3),
            ],
        ];
        for content in [
            json!({"type":"thinking","thinking":"why","signature":"sig"}),
            json!({"type":"tool_use","id":"client_wrong_kind","name":"read","input":{}}),
            json!({"type":"server_tool_use","id":"srvtoolu_wrong_kind","name":"tool_search_tool_bm25","input":{}}),
            json!({"type":"image","source":{"type":"url","url":"https://example.invalid/image"}}),
            json!({"type":"redacted_thinking","data":"opaque"}),
            json!({"type":"tool_result","tool_use_id":"client_result","content":"data"}),
            json!({"type":"tool_search_tool_result","tool_use_id":"srvtoolu_result","content":{"type":"tool_search_tool_search_result","tool_references":[]}}),
        ] {
            let mut wire = vec![
                start(),
                block(3, content),
                delta(3, json!({"type":"citations_delta","citation":citation})),
                stop(3),
            ];
            wire.extend(terminal_frames());
            let public = stream::iter(rows(wire.clone()))
                .map(|row| {
                    crate::client::parse_messages_stream_event(&row).map(|frame| frame.event)
                })
                .boxed();
            let output = crate::stream::messages::stream_messages(
                public,
                None,
                RequestId::from("mss-u2-kind"),
                Duration::from_secs(1),
            )
            .collect::<Vec<_>>()
            .await;
            assert!(
                output
                    .iter()
                    .any(|event| matches!(event, SamplingEvent::Failed { .. })),
                "MSS_U2_PUBLIC_NON_TEXT_REJECTED"
            );
            assert!(
                !output
                    .iter()
                    .any(|event| matches!(event, SamplingEvent::Completed { .. }))
            );
            invalid.push(wire);
        }
        for malformed in [
            json!(null),
            json!(7),
            json!([]),
            json!({}),
            json!({"type":"future_citation"}),
            json!({"type":"web_search_result_location","url":"https://example.invalid/evidence"}),
        ] {
            invalid.push(vec![
                start(),
                block(3, text.clone()),
                delta(3, json!({"type":"citations_delta","citation":malformed})),
                stop(3),
            ]);
        }
        for (field, value) in [
            ("document_index", json!(-1)),
            ("start_char_index", json!(1.5)),
            ("file_id", json!(false)),
            ("end_char_index", json!(0)),
        ] {
            let mut bad = citation.clone();
            if field == "end_char_index" {
                bad["start_char_index"] = json!(2);
            }
            bad[field] = value;
            invalid.push(vec![
                start(),
                block(3, text.clone()),
                delta(3, json!({"type":"citations_delta","citation":bad})),
                stop(3),
            ]);
        }
        let mut page = mss_u2_citation_objects()[1].clone();
        page["start_page_number"] = json!(0);
        invalid.push(vec![
            start(),
            block(3, text.clone()),
            delta(3, json!({"type":"citations_delta","citation":page})),
            stop(3),
        ]);
        invalid.push(vec![
            start(),
            block(3, text),
            delta(3, json!({"type":"citation_delta","citation":citation})),
            stop(3),
        ]);
        for mut wire in invalid {
            wire.extend(terminal_frames());
            let (attempt, _) = collect_raw(rows(wire)).await;
            assert!(
                attempt.is_err(),
                "MSS_U2_INVALID_INDEX_KIND_OR_SHAPE_NOT_CAPTURED"
            );
        }
    }

    fn mss_u3_usage_fixture() -> Vec<Value> {
        let (mut wire, _) = mss_u1_server_fixture(None, false);
        wire[0]["message"]["usage"] = json!({"input_tokens":10,"output_tokens":0,
            "server_tool_use":{"web_search_requests":1,"web_fetch_requests":2},
            "output_tokens_details":{"thinking_tokens":0},
            "cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":null},
            "service_tier":"priority","inference_geo":null,
            "iterations":[{"iteration":1}],"speed":"standard","opaque":{"exact":"α"}});
        wire.truncate(wire.len() - 2);
        wire.extend([
            json!({"type":"message_delta","delta":{},"usage":{"output_tokens":2,
                "server_tool_use":{"web_search_requests":3},"output_tokens_details":{"thinking_tokens":null}}}),
            json!({"type":"message_delta","delta":{},"usage":{"output_tokens":4,
                "server_tool_use":{"web_fetch_requests":4},"iterations":[{"iteration":2}]}}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_details":null},
                "usage":{"output_tokens":7,"server_tool_use":null,"output_tokens_details":{},
                    "speed":"fast","fallback_credit":{"outcome":"not_used"}}}),
            json!({"type":"message_stop"}),
        ]);
        wire
    }

    async fn mss_u3_capture_evidence(wire: Vec<Value>) -> (Value, Vec<SamplingEvent>) {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        let lease = NativeCaptureLease::issue(
            "mss-u3-lineage".into(),
            "mss-u3-request".into(),
            1,
            1,
            NATIVE_SOURCE_BYTES,
        );
        let mut capture = AttemptCapture::passive(lease);
        let frames = stream::iter(rows(wire))
            .map(|row| crate::client::parse_messages_stream_event(&row))
            .boxed();
        let output = stream_messages_tracked(
            frames,
            None,
            RequestId::from("mss-u3-request"),
            Duration::from_secs(1),
            Some(&mut capture),
        )
        .collect::<Vec<_>>()
        .await;
        let status = if output
            .iter()
            .any(|event| matches!(event, SamplingEvent::Completed { .. }))
        {
            NativeResponseStatus::Accepted
        } else {
            NativeResponseStatus::PartialTerminal(NativeUnavailable::Failed)
        };
        let evidence = capture
            .seal_native(1, status, true, 1, None)
            .expect("MSS_U3_REAL_CAPTURE_SEAL");
        (serde_json::to_value(evidence).unwrap(), output)
    }

    #[test]
    fn mss_u3_usage_decode_keeps_partial_server_counts() {
        let raw = json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},
            "usage":{"output_tokens":0,"server_tool_use":{"web_search_requests":0}}});
        let frame = crate::client::parse_messages_stream_event(&raw.to_string()).unwrap();
        let typed = serde_json::to_value(frame.event).unwrap();
        assert_eq!(
            typed["usage"], raw["usage"],
            "MSS_U3_TYPED_PARTIAL_SERVER_COUNTS"
        );
        for value in [json!(-1), json!(1.5), json!("1"), json!(false)] {
            let mut bad = raw.clone();
            bad["usage"]["server_tool_use"]["web_search_requests"] = value;
            assert!(crate::client::parse_messages_stream_event(&bad.to_string()).is_err());
        }
    }

    #[tokio::test]
    async fn mss_u3_usage_reduces_cumulative_fields_once() {
        let wire = mss_u3_usage_fixture();
        let (evidence, output) = mss_u3_capture_evidence(wire.clone()).await;
        let observed = &evidence["messages_metadata"]["Observed"];
        assert!(observed.is_object(), "MSS_U3_ORDERED_METADATA_HANDOFF");
        let expected_usage = json!({"input_tokens":10,"output_tokens":7,
            "server_tool_use":{"web_search_requests":3,"web_fetch_requests":4},
            "output_tokens_details":{"thinking_tokens":0},
            "cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":null},
            "service_tier":"priority","inference_geo":null,"iterations":[{"iteration":2}],
            "speed":"fast","opaque":{"exact":"α"},"fallback_credit":{"outcome":"not_used"}});
        assert_eq!(
            observed["finalized"],
            json!({"usage":{"Value":expected_usage},
            "stop_reason":{"Value":"end_turn"},"stop_sequence":"Missing","stop_details":"Null",
            "terminal_observation":3}),
            "MSS_U3_PER_FIELD_CUMULATIVE_REDUCTION"
        );
        assert_eq!(evidence["usage"], expected_usage);
        let observations = observed["observations"].as_array().unwrap();
        assert_eq!(observations.len(), 4);
        let envelopes: Vec<_> = wire
            .iter()
            .filter(|row| {
                matches!(
                    row["type"].as_str(),
                    Some("message_start" | "message_delta")
                )
            })
            .collect();
        for (ordinal, (actual, raw)) in observations.iter().zip(envelopes).enumerate() {
            let envelope = if raw["type"] == "message_start" {
                &raw["message"]
            } else {
                raw
            };
            assert_eq!(actual["ordinal"], ordinal);
            assert_eq!(
                actual["usage"],
                json!({"Value":envelope["usage"]}),
                "MSS_U3_RAW_USAGE_PRESENCE"
            );
        }
        assert_eq!(observed["coverage"], "Complete");
        assert_eq!(
            output
                .iter()
                .filter(|event| matches!(event, SamplingEvent::Completed { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn mss_u3_metadata_limits_are_inclusive() {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        let start = json!({"type":"message_start","message":{"id":"mss-limits","type":"message",
            "role":"assistant","model":"test","content":[],"stop_reason":null,
            "usage":{"input_tokens":0,"output_tokens":0}}});
        let update =
            json!({"type":"message_delta","delta":{},"usage":{"output_tokens":0}}).to_string();
        for passive in [false, true] {
            let mut capture = if passive {
                AttemptCapture::passive(NativeCaptureLease::issue(
                    "limits".into(),
                    "limits".into(),
                    1,
                    1,
                    NATIVE_SOURCE_BYTES,
                ))
            } else {
                AttemptCapture::default()
            };
            capture
                .observe(&crate::client::parse_messages_stream_event(&start.to_string()).unwrap())
                .unwrap();
            let mut refused = None;
            for count in 2..=100_001 {
                let result =
                    capture.observe(&crate::client::parse_messages_stream_event(&update).unwrap());
                if result == Err(CaptureError::LimitExceeded) {
                    refused = Some(count);
                    break;
                }
                assert_eq!(result, Ok(()));
            }
            assert!(refused.is_some(), "MSS_U3_OBSERVATION_CAP_ENFORCED");
            if !passive {
                assert_eq!(refused, Some(100_001), "MSS_U3_COUNT_CAP_INCLUSIVE");
            } else {
                assert!(
                    refused.unwrap() <= 100_001,
                    "MSS_U3_TIGHTER_NATIVE_LIMIT_PREVAILS"
                );
            }
        }
    }

    fn mss_u3_byte_start(payload: usize) -> Value {
        json!({"type":"message_start","message":{"id":"mss-bytes","type":"message","role":"assistant",
            "model":"test","content":[],"stop_reason":null,"stop_details":null,
            "usage":{"input_tokens":1,"output_tokens":0,"opaque":"x".repeat(payload)}}})
    }

    fn mss_u3_independent_start_bytes(raw: &Value, wire_bytes: usize) -> u64 {
        let usage = &raw["message"]["usage"];
        let observation = json!({"ordinal":0,"kind":"MessageStart","terminal":false,
            "usage":{"Value":usage},"stop_reason":"Null","stop_sequence":"Missing","stop_details":"Null"});
        let captured = json!({"Observed":{"observations":[observation],
            "finalized":{"usage":{"Value":usage},"stop_reason":"Null","stop_sequence":"Missing","stop_details":"Null",
                "terminal_observation":null},"coverage":{"Partial":{"reason":"ConflictingObservation"}}}});
        wire_bytes as u64
            + serde_json::to_vec(&captured).unwrap().len() as u64
            + serde_json::to_vec(usage).unwrap().len() as u64
            + serde_json::to_vec(&raw["message"]["id"]).unwrap().len() as u64
            + 4
    }

    #[test]
    fn mss_u3_metadata_byte_limits_reserve_before_growth() {
        use xai_grok_sampling_types::conversation::native_search_history::NativeMessagesPresence;
        let empty = mss_u3_byte_start(0);
        let overhead = mss_u3_independent_start_bytes(&empty, empty.to_string().len());
        let payload = ((MAX_ENCODED_MESSAGES_REQUEST_BYTES - overhead) / 4) as usize;
        let raw = mss_u3_byte_start(payload);
        let row = raw.to_string();
        let used = mss_u3_independent_start_bytes(&raw, row.len());
        let row = row + &" ".repeat((MAX_ENCODED_MESSAGES_REQUEST_BYTES - used) as usize);
        assert_eq!(
            mss_u3_independent_start_bytes(&raw, row.len()),
            MAX_ENCODED_MESSAGES_REQUEST_BYTES
        );
        let mut fit = AttemptCapture::default();
        assert_eq!(
            fit.observe(&crate::client::parse_messages_stream_event(&row).unwrap()),
            Ok(()),
            "MSS_U3_BYTES_EXACT_FIT"
        );
        assert_eq!(fit.messages_observations.len(), 1);
        let mut too_large = AttemptCapture::default();
        assert_eq!(
            too_large.observe(&crate::client::parse_messages_stream_event(&(row + " ")).unwrap()),
            Err(CaptureError::LimitExceeded),
            "MSS_U3_BYTES_PLUS_ONE_REFUSES"
        );
        assert!(
            too_large.messages_observations.is_empty(),
            "MSS_U3_BYTES_RESERVE_BEFORE_GROWTH"
        );
        assert!(matches!(
            too_large.messages_finalized.usage,
            NativeMessagesPresence::Missing
        ));
    }

    fn mss_u3_ordinary_projection(events: &[SamplingEvent]) -> Value {
        let complete: Vec<_> = events.iter().filter_map(|event| match event {
            SamplingEvent::Completed { response, .. } => Some(json!({
                "items":response.items,"usage":response.usage,"stop_reason":response.stop_reason,
                "message_id":response.message_id,"raw_stop_reason":response.raw_stop_reason,
                "stop_message":response.stop_message,"stop_sequence":response.stop_sequence,
                "cost_usd_ticks":response.cost_usd_ticks,"message_chunks_emitted":response.message_chunks_emitted,
            })),
            _ => None,
        }).collect();
        assert_eq!(complete.len(), 1);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, SamplingEvent::Failed { .. }))
        );
        json!(complete)
    }

    #[tokio::test]
    async fn mss_u3_metadata_passive_byte_exhaustion_preserves_ordinary() {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        let raw = vec![
            mss_u3_byte_start(NATIVE_RESPONSE_BYTES as usize),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"ordinary intact"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
            json!({"type":"message_stop"}),
        ];
        let frames = || {
            stream::iter(rows(raw.clone()))
                .map(|row| crate::client::parse_messages_stream_event(&row))
                .boxed()
        };
        let ordinary = stream_messages_tracked(
            frames(),
            None,
            RequestId::from("mss-u3-bytes"),
            Duration::from_secs(1),
            None,
        )
        .collect::<Vec<_>>()
        .await;
        let mut capture = AttemptCapture::passive(NativeCaptureLease::issue(
            "mss-u3-bytes".into(),
            "mss-u3-bytes".into(),
            1,
            1,
            NATIVE_SOURCE_BYTES,
        ));
        let passive = stream_messages_tracked(
            frames(),
            None,
            RequestId::from("mss-u3-bytes"),
            Duration::from_secs(1),
            Some(&mut capture),
        )
        .collect::<Vec<_>>()
        .await;
        assert_eq!(
            mss_u3_ordinary_projection(&passive),
            mss_u3_ordinary_projection(&ordinary),
            "MSS_U3_PASSIVE_BYTES_ORDINARY_EQUAL"
        );
        assert!(
            capture.passive_exhausted(),
            "MSS_U3_PASSIVE_METADATA_BYTES_CHARGED"
        );
        assert!(capture.messages_observations.is_empty());
        assert!(matches!(
            capture.messages_failure,
            Some(NativeUnavailable::Capacity)
        ));
    }

    #[test]
    fn mss_u3_usage_missing_server_counts_are_not_zero() {
        for server in [
            None,
            Some(json!(null)),
            Some(json!({})),
            Some(json!({"web_search_requests":0})),
        ] {
            let mut usage = json!({"input_tokens":0,"output_tokens":0});
            if let Some(server) = server {
                usage["server_tool_use"] = server;
            }
            let start = json!({"type":"message_start","message":{"id":"mss-u3-absence","type":"message","role":"assistant",
                "model":"test","content":[],"stop_reason":null,"usage":usage}});
            let delta =
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":usage});
            for raw in [start, delta] {
                let frame = crate::client::parse_messages_stream_event(&raw.to_string()).unwrap();
                let typed = serde_json::to_value(frame.event).unwrap();
                let actual = if raw["type"] == "message_start" {
                    &typed["message"]["usage"]
                } else {
                    &typed["usage"]
                };
                assert_eq!(*actual, usage, "MSS_U3_NO_SYNTHETIC_SERVER_USAGE");
            }
        }
    }

    #[test]
    fn mss_u3_metadata_reserves_every_physical_copy_before_growth() {
        use xai_grok_sampling_types::conversation::native_search_history::*;
        let lease = NativeCaptureLease::issue(
            "mss-u3-copy".into(),
            "mss-u3-copy".into(),
            1,
            1,
            NATIVE_SOURCE_BYTES,
        );
        let mut capture = AttemptCapture::passive(lease.clone());
        let before = lease.charged().unwrap();
        let before_reserved = capture.native_reserved_response_bytes;
        let raw = mss_u3_byte_start(16_000).to_string();
        capture
            .observe(&crate::client::parse_messages_stream_event(&raw).unwrap())
            .unwrap();
        let observation = serde_json::to_vec(&capture.messages_observations[0])
            .unwrap()
            .len() as u64;
        let finalized = serde_json::to_vec(&capture.messages_finalized)
            .unwrap()
            .len() as u64;
        let usage = serde_json::to_vec(&capture.native_usage).unwrap().len() as u64;
        let copies = 2 * (observation + finalized + usage);
        assert_eq!(
            lease.charged().unwrap() - before,
            capture.native_reserved_response_bytes - before_reserved + copies,
            "MSS_U3_CAPTURE_OBSERVATION_COPIES_CHARGED"
        );
        let metadata = NativeMessagesCapture::Observed {
            observations: capture.messages_observations.clone(),
            finalized: capture.messages_finalized.clone().into(),
            coverage: NativeMessagesCoverage::Partial {
                reason: NativeUnavailable::Incomplete,
            },
        };
        let metadata_bytes = serde_json::to_vec(&metadata).unwrap().len() as u64;
        assert!(
            capture.native_reserved_response_bytes >= 5 * metadata_bytes,
            "MSS_U3_CAPTURE_LEDGER_COPIES_RESERVED"
        );
    }

    fn mss_u4_stop_frames(details: Option<Value>, sequence: Option<Value>) -> Vec<Value> {
        let mut wire = vec![
            json!({"type":"message_start","message":{"id":"mss-u4-capture",
            "type":"message","role":"assistant","model":"test","content":[],"stop_reason":null,
            "stop_details":{"type":"start","explanation":"start-only","opaque":[1,2]},
            "stop_sequence":"start-only","usage":{"input_tokens":1,"output_tokens":0}}}),
            json!({"type":"content_block_start","index":2,"content_block":{"type":"server_tool_use",
                "id":"srvtoolu_u4","name":"tool_search_tool_bm25","input":{}}}),
            json!({"type":"content_block_stop","index":2}),
            json!({"type":"content_block_start","index":9,"content_block":{"type":"tool_search_tool_result",
                "tool_use_id":"srvtoolu_u4","content":{"type":"tool_search_tool_search_result","tool_references":[]}}}),
            json!({"type":"content_block_stop","index":9}),
            json!({"type":"message_delta","delta":{"stop_details":{"explanation":"nonterminal-only"},
                "stop_sequence":"nonterminal-only"},"usage":{"output_tokens":2}}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}),
            json!({"type":"message_stop"}),
        ];
        if let Some(details) = details {
            wire[6]["delta"]["stop_details"] = details;
        }
        if let Some(sequence) = sequence {
            wire[6]["delta"]["stop_sequence"] = sequence;
        }
        wire
    }

    #[tokio::test]
    async fn mss_u4_stream_stop_details_are_not_explanation_only() {
        let details = json!({"type":null,"category":"terminal","explanation":"terminal-only",
            "resumable":false,"fallback_has_prefill_claim":true,"opaque":{"empty":[],"null":null}});
        let raw = json!({"type":"message_delta","delta":{"stop_reason":"end_turn",
            "stop_sequence":null,"stop_details":details},"usage":{"output_tokens":7}});
        let frame = crate::client::parse_messages_stream_event(&raw.to_string()).unwrap();
        let typed = serde_json::to_value(frame.event).unwrap();
        assert_eq!(
            typed["delta"], raw["delta"],
            "MSS_U4_TYPED_WHOLE_STOP_DETAILS"
        );
        for value in [None, Some(json!(null)), Some(json!({})), Some(details)] {
            for sequence in [None, Some(json!(null)), Some(json!("matched-α\""))] {
                let wire = mss_u4_stop_frames(value.clone(), sequence.clone());
                let terminal = &wire[6]["delta"];
                let frame =
                    crate::client::parse_messages_stream_event(&wire[6].to_string()).unwrap();
                assert_eq!(
                    serde_json::to_value(frame.event).unwrap()["delta"],
                    *terminal,
                    "MSS_U4_TERMINAL_TYPED_PRESENCE"
                );
                let (evidence, output) = mss_u3_capture_evidence(wire).await;
                let observed = &evidence["messages_metadata"]["Observed"];
                let presence = |value: &Option<Value>| match value {
                    None => json!("Missing"),
                    Some(Value::Null) => json!("Null"),
                    Some(value) => json!({"Value":value}),
                };
                assert_eq!(
                    observed["finalized"]["stop_details"],
                    presence(&value),
                    "MSS_U4_TERMINAL_DETAILS_REPLACE"
                );
                assert_eq!(
                    observed["finalized"]["stop_sequence"],
                    presence(&sequence),
                    "MSS_U4_TERMINAL_SEQUENCE_REPLACE"
                );
                assert_eq!(observed["observations"].as_array().unwrap().len(), 3);
                let responses: Vec<_> = output
                    .iter()
                    .filter_map(|event| match event {
                        SamplingEvent::Completed { response, .. } => Some(response),
                        _ => None,
                    })
                    .collect();
                assert_eq!(responses.len(), 1);
                assert_eq!(
                    responses[0].stop_sequence.as_deref(),
                    sequence.as_ref().and_then(Value::as_str),
                    "MSS_U4_ORDINARY_TERMINAL_SEQUENCE"
                );
                assert_eq!(
                    responses[0].stop_message.as_deref(),
                    value
                        .as_ref()
                        .and_then(|value| value.get("explanation"))
                        .and_then(Value::as_str)
                );
            }
        }
    }

    #[tokio::test]
    async fn mss_u5_container_and_eof_never_qualify_a_suffix() {
        for at_start in [true, false] {
            let (mut wire, _) = mss_u1_server_fixture(None, false);
            if at_start {
                wire[0]["message"]["container"] = json!({"id":"unsupported-container"});
            } else {
                let terminal = wire.len() - 2;
                wire[terminal]["delta"]["container"] = json!({"id":"unsupported-container"});
            }
            let (attempt, output) = collect_raw(rows(wire)).await;
            assert!(attempt.is_err(), "MSS_U5_CONTAINER_NO_FAITHFUL_SUFFIX");
            assert!(
                !output
                    .iter()
                    .any(|event| matches!(event, SamplingEvent::Completed { .. })),
                "MSS_U5_REQUIRED_CONTAINER_FAILURE"
            );
        }
        let (mut wire, _) = mss_u1_server_fixture(None, false);
        wire.pop();
        let (attempt, output) = collect_raw(rows(wire)).await;
        assert!(attempt.is_err(), "MSS_U5_EOF_NO_FAITHFUL_SUFFIX");
        assert_eq!(
            output
                .iter()
                .filter(|event| matches!(event, SamplingEvent::Failed { .. }))
                .count(),
            1
        );
        assert!(
            !output
                .iter()
                .any(|event| matches!(event, SamplingEvent::Completed { .. }))
        );
    }
}
