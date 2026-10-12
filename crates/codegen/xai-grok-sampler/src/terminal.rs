//! Structural terminal observations captured before a provider response is
//! collapsed into the sampler's public completion/error events.

use xai_grok_sampling_types::{ApiBackend, TokenUsage};

/// Named terminal event observed on the provider wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedEvent {
    Completed,
    Incomplete,
    Failed,
}

/// Normalized provider reason attached to a terminal event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalReason {
    Length,
    Refusal,
    ContentFilter,
    ContextWindow,
    Other(String),
}

/// Shape of the raw Responses `response.output` field before compatibility
/// normalization can insert an empty array.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputPresence {
    Missing,
    Null,
    Array(usize),
}

/// Stable structural facts for one terminal provider observation.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalFacts {
    pub wire: ApiBackend,
    pub dialect: Option<String>,
    pub named_event: Option<NamedEvent>,
    pub status: Option<String>,
    pub reason: Option<TerminalReason>,
    pub raw_output_presence: OutputPresence,
    pub message_items: usize,
    pub reasoning_items: usize,
    pub raw_text_bytes: usize,
    pub trimmed_text_bytes: usize,
    pub tool_activity: bool,
    pub tool_items_complete: bool,
    pub refusal: bool,
    pub response_id: Option<String>,
    pub response_model: Option<String>,
    pub usage: Option<TokenUsage>,
    pub cost_usd_ticks: Option<i64>,
}

/// Mutually exclusive terminal class for one provider attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalClass {
    ValidText,
    ValidTool,
    ExactEmpty,
    WhitespaceEmpty,
    ReasoningOnly,
    ZeroOutput,
    Length,
    ContentFilter,
    ContextWindow,
    ProviderIncomplete,
    TransportEof,
    ProviderFailed,
    Cancelled,
    Refusal,
    Malformed,
}

/// Retry-policy lane associated with a structural terminal class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryLane {
    None,
    Semantic,
    Length,
    Transport,
}

/// Classify terminal facts once, before response conversion can erase their
/// named cause or raw output shape.
pub fn classify_terminal(facts: &TerminalFacts) -> (TerminalClass, RetryLane) {
    if facts.refusal {
        return (TerminalClass::Refusal, RetryLane::None);
    }

    match (&facts.named_event, &facts.status, &facts.reason) {
        (Some(NamedEvent::Failed), _, _) => {
            return (TerminalClass::ProviderFailed, RetryLane::Transport);
        }
        (_, _, Some(TerminalReason::Refusal)) => {
            return (TerminalClass::Refusal, RetryLane::None);
        }
        (_, _, Some(TerminalReason::ContentFilter)) => {
            return (TerminalClass::ContentFilter, RetryLane::None);
        }
        (_, _, Some(TerminalReason::ContextWindow)) => {
            return (TerminalClass::ContextWindow, RetryLane::None);
        }
        (Some(NamedEvent::Incomplete), _, Some(TerminalReason::Length))
        | (_, _, Some(TerminalReason::Length)) => {
            return (TerminalClass::Length, RetryLane::Length);
        }
        (Some(NamedEvent::Incomplete), _, _) => {
            return (TerminalClass::ProviderIncomplete, RetryLane::None);
        }
        _ => {}
    }

    match facts.status.as_deref() {
        Some("cancelled") => return (TerminalClass::Cancelled, RetryLane::None),
        Some("transport_eof") => return (TerminalClass::TransportEof, RetryLane::Transport),
        _ => {}
    }

    if facts.named_event != Some(NamedEvent::Completed) {
        return (TerminalClass::Malformed, RetryLane::None);
    }

    if facts.trimmed_text_bytes > 0 {
        return (TerminalClass::ValidText, RetryLane::None);
    }
    if facts.tool_activity && facts.tool_items_complete {
        return (TerminalClass::ValidTool, RetryLane::None);
    }

    if facts.wire == ApiBackend::Responses
        && matches!(
            facts.raw_output_presence,
            OutputPresence::Missing | OutputPresence::Null
        )
    {
        return (TerminalClass::Malformed, RetryLane::None);
    }
    if facts.tool_activity {
        return (TerminalClass::Malformed, RetryLane::None);
    }
    if facts.message_items > 0 && facts.raw_text_bytes == 0 {
        return (TerminalClass::ExactEmpty, RetryLane::Semantic);
    }
    if facts.message_items > 0 && facts.raw_text_bytes > 0 && facts.trimmed_text_bytes == 0 {
        return (TerminalClass::WhitespaceEmpty, RetryLane::Semantic);
    }
    if facts.reasoning_items > 0 && facts.message_items == 0 {
        return (TerminalClass::ReasoningOnly, RetryLane::Semantic);
    }
    if facts.raw_output_presence == OutputPresence::Array(0) {
        return (TerminalClass::ZeroOutput, RetryLane::Semantic);
    }

    (TerminalClass::Malformed, RetryLane::None)
}
