//! Layer-2 stream transform for the Anthropic Messages API.
//!
//! Consumes a raw `MessageStreamEvent` stream and produces [`SamplingEvent`]s.
//! Pure: no I/O, no shell coupling.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use futures_util::stream::{BoxStream, Stream};

use xai_grok_sampling_types::messages::{self, MessageStreamEvent};
use xai_grok_sampling_types::{
    AssistantItem, ConversationItem, ConversationResponse, ResponseModelMetadata, SamplingError,
    StopReason, TokenUsage, ToolCall, rs,
};

use crate::events::{SamplingChannel, SamplingErrorInfo, SamplingEvent};
use crate::metrics::InferenceLatencyStats;
use crate::types::RequestId;

/// The wire `type` string of a delta (input to the R2 unopened-index guard).
fn stream_delta_type(delta: &messages::StreamDelta) -> &'static str {
    match delta {
        messages::StreamDelta::TextDelta { .. } => "text_delta",
        messages::StreamDelta::InputJsonDelta { .. } => "input_json_delta",
        messages::StreamDelta::ThinkingDelta { .. } => "thinking_delta",
        messages::StreamDelta::SignatureDelta { .. } => "signature_delta",
    }
}

/// Returns whether a Messages API event reflects real model progress rather than a liveness-only heartbeat (Ping).
pub(crate) fn messages_event_has_meaningful_content(event: &MessageStreamEvent) -> bool {
    match event {
        MessageStreamEvent::Ping => false,
        MessageStreamEvent::MessageStart { .. }
        | MessageStreamEvent::MessageDelta { .. }
        | MessageStreamEvent::MessageStop
        | MessageStreamEvent::ContentBlockStart { .. }
        | MessageStreamEvent::ContentBlockDelta { .. }
        | MessageStreamEvent::ContentBlockStop { .. }
        | MessageStreamEvent::Error { .. } => true,
    }
}

/// The Anthropic Messages API reports content as a sequence of indexed blocks (text / thinking / tool_use), each with start / delta / stop events.
/// We accumulate per-index and finalize each block on `ContentBlockStop`.
struct BlockState {
    block_type: BlockType,
    text_acc: String,
    tool_name: String,
    tool_id: String,
    args_acc: String,
    /// R7 (row 17): the block's own non-empty `input` object, compact
    /// serialized — the wire's authoritative tool-call payload. Wins over
    /// `args_acc` at the block's stop; `None` for the normal wire shape
    /// (`input: {}` at start, arguments via `input_json_delta`).
    authoritative_args: Option<String>,
    thinking_acc: String,
    signature: String,
    /// A `signature_delta` already arrived for this block (R2
    /// `DuplicateSignatureDelta` guard input; xli `signature_seen`).
    signature_seen: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockType {
    Text,
    ToolUse,
    Thinking,
    /// D3 phantom (spec G3 ruling): an unknown wire `type` opened a block the
    /// transform does not model — index recorded, deltas swallowed, stop
    /// no-op. A forward-compat stream never hits the fatal unopened-index
    /// classes because of this.
    Unknown,
    /// A KNOWN wire kind this transform does not model (redacted_thinking /
    /// image / tool_result in an assistant stream): recorded so its stop is
    /// not misclassified as the fatal never-started-index class, but inert
    /// otherwise (deltas swallowed, stop no-op — pre-MW-3 observable behavior).
    Inert,
}

/// Transform a raw Anthropic Messages API stream into a stream of [`SamplingEvent`]s.
/// Yields exactly one terminal event ([`SamplingEvent::Completed`] or [`SamplingEvent::Failed`]) per request.
/// The actor's retry loop treats them as retryable transport-level errors.
pub fn stream_messages<'a>(
    raw_stream: BoxStream<'a, Result<MessageStreamEvent, SamplingError>>,
    model_metadata: Option<ResponseModelMetadata>,
    request_id: RequestId,
    idle_timeout: Duration,
) -> impl Stream<Item = SamplingEvent> + Send + 'a {
    async_stream::stream! {
        use messages::{ContentBlock, StreamDelta};

        let decode_region = crate::span_timing::Region::from_span(tracing::info_span!(
            "sampling.stream_decode",
            ttft_ms = tracing::field::Empty,
            ttlb_ms = tracing::field::Empty,
            output_tokens = tracing::field::Empty,
            chunk_count = tracing::field::Empty,
        ));
        let stream_start = Instant::now();
        let mut chunk_timestamps: Vec<Instant> = Vec::new();

        yield SamplingEvent::StreamStarted {
            request_id: request_id.clone(),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        };

        if let Some(metadata) = model_metadata {
            yield SamplingEvent::ModelMetadata {
                request_id: request_id.clone(),
                metadata,
            };
        }

        // Per-block accumulators keyed by content block index.
        let mut blocks: BTreeMap<u32, BlockState> = BTreeMap::new();

        // Final-message-level accumulators
        let mut final_model: Option<String> = None;
        // Anthropic Messages API `input_tokens` is the uncached portion
        // Cache hits and writes are reported in separate buckets and must be summed for the true total prompt size
        let mut final_input_tokens: u32 = 0;
        let mut final_cache_read_input_tokens: u32 = 0;
        let mut final_cache_creation_input_tokens: u32 = 0;
        let mut final_output_tokens: u32 = 0;
        // R4: the messages route's thinking decomposition (thinking_tokens).
        // Mapped into TokenUsage.reasoning_tokens so the messages wire displays
        // real reasoning tokens like the responses wire (stream/responses.rs:635).
        let mut final_reasoning_tokens: u32 = 0;
        let mut final_stop_reason: Option<StopReason> = None;
        let mut final_stop_message: Option<String> = None;
        let mut final_message_id: Option<String> = None;
        let mut final_raw_stop_reason: Option<String> = None;
        // The provider sends the matched stop sequence in `message_delta.stop_sequence` on a `stop_sequence`-terminated turn
        // It is carried through so the headless `streaming-messages-json` consumer can echo it
        let mut final_stop_sequence: Option<String> = None;

        // Assistant-response accumulators (built up as ContentBlockStop events fire)
        // Reasoning is collected into a synthesized `rs::ReasoningItem`
        // It is emitted as a sibling `ConversationItem::Reasoning` before the trailing Assistant
        let mut assistant_text = String::new();
        let mut assistant_tool_calls: Vec<ToolCall> = Vec::new();
        let mut assistant_reasoning: Option<rs::ReasoningItem> = None;

        // Index counters
        let mut chunk_index: u64 = 0;
        let mut message_chunk_count: u64 = 0;
        let mut first_token_emitted = false;
        let mut last_content_chunk_at = Instant::now();

        // Tool-call index counter for per-tool deltas (separate from the block index, which can be interleaved with text/thinking blocks)
        let mut next_tool_index: u32 = 0;
        let mut block_to_tool_index: BTreeMap<u32, u32> = BTreeMap::new();

        // R2 usage-monotonicity guard state (xli `UsageSnapshot` subset:
        // input + output wire fields; zero/absent incoming skips the check).
        let mut prev_input_tokens: u32 = 0;
        let mut prev_output_tokens: u32 = 0;
        // R3: `message_stop` observed — the stream is only complete with it.
        let mut message_stop_seen = false;

        let mut stream = raw_stream;
        loop {
            let event_result = match tokio::time::timeout(idle_timeout, stream.next()).await {
                Ok(Some(event_result)) => event_result,
                Ok(None) => break,
                Err(_elapsed) => {
                    let err = SamplingError::IdleTimeout {
                        elapsed_secs: idle_timeout.as_secs(),
                    };
                    yield SamplingEvent::Failed {
                        request_id: request_id.clone(),
                        error: SamplingErrorInfo::from(&err),
                    };
                    return;
                }
            };

            let event = match event_result {
                Ok(event) => event,
                Err(err) => {
                    yield SamplingEvent::Failed {
                        request_id: request_id.clone(),
                        error: SamplingErrorInfo::from(&err),
                    };
                    return;
                }
            };

            let event_has_content = messages_event_has_meaningful_content(&event);

            match event {
                MessageStreamEvent::MessageStart { message } => {
                    final_message_id = Some(message.id.clone());
                    final_model = Some(message.model.clone());
                    final_input_tokens = message.usage.input_tokens;
                    prev_input_tokens = message.usage.input_tokens;
                    prev_output_tokens = message.usage.output_tokens;
                    final_cache_read_input_tokens = message.usage.cache_read_input_tokens;
                    final_cache_creation_input_tokens = message.usage.cache_creation_input_tokens;
                    final_reasoning_tokens = message
                        .usage
                        .output_tokens_details
                        .map(|d| d.thinking_tokens)
                        .unwrap_or(0);
                    // Yield the real id, model, and input usage before any content
                    // Partial-mode framing then emits them on the real `message_start` instead of a synthesized placeholder
                    yield SamplingEvent::ResponseStarted {
                        request_id: request_id.clone(),
                        message_id: message.id,
                        model: message.model,
                        input_tokens: u64::from(message.usage.input_tokens),
                        cache_read_input_tokens: u64::from(
                            message.usage.cache_read_input_tokens,
                        ),
                        cache_creation_input_tokens: u64::from(
                            message.usage.cache_creation_input_tokens,
                        ),
                    };
                }

                MessageStreamEvent::ContentBlockStart {
                    index,
                    content_block,
                } => {
                    if blocks.contains_key(&index) {
                        // R7 row 18: duplicate open index — recoverable
                        // (xli overwrites silently; grok warns and keeps the
                        // first block, so its deltas/stop stay coherent).
                        let v = super::messages_invariants::StreamInvariantViolation::DuplicateToolCallIndex {
                            block_index: index,
                        };
                        tracing::warn!(violation = ?v, "messages-stream:invariant");
                    } else {
                        match content_block {
                    ContentBlock::Thinking {
                        thinking,
                        signature,
                    } => {
                        blocks.insert(
                            index,
                            BlockState {
                                block_type: BlockType::Thinking,
                                text_acc: String::new(),
                                tool_name: String::new(),
                                tool_id: String::new(),
                                args_acc: String::new(),
                                authoritative_args: None,
                                thinking_acc: thinking.clone(),
                                signature: signature.clone(),
                                signature_seen: !signature.is_empty(),
                            },
                        );
                        if !first_token_emitted {
                            first_token_emitted = true;
                            yield SamplingEvent::FirstToken {
                                request_id: request_id.clone(),
                            };
                        }
                    }
                    ContentBlock::Text { text, .. } => {
                        blocks.insert(
                            index,
                            BlockState {
                                block_type: BlockType::Text,
                                text_acc: text.clone(),
                                tool_name: String::new(),
                                tool_id: String::new(),
                                args_acc: String::new(),
                                authoritative_args: None,
                                thinking_acc: String::new(),
                                signature: String::new(),
                                signature_seen: false,
                            },
                        );
                        if !first_token_emitted {
                            first_token_emitted = true;
                            yield SamplingEvent::FirstToken {
                                request_id: request_id.clone(),
                            };
                        }
                    }
                    ContentBlock::ToolUse { id, name, input, .. } => {
                        let tool_index = next_tool_index;
                        next_tool_index += 1;
                        block_to_tool_index.insert(index, tool_index);

                        blocks.insert(
                            index,
                            BlockState {
                                block_type: BlockType::ToolUse,
                                text_acc: String::new(),
                                tool_name: name.clone(),
                                tool_id: id.clone(),
                                // Anthropic Messages API streams arguments via InputJsonDelta events
                                // Starting from "{}" then appending fragments would produce invalid JSON
                                args_acc: String::new(),
                                // R7 (row 17): a NON-EMPTY start `input` object
                                // is the wire's authoritative tool-call payload
                                // and wins over the streamed deltas at stop.
                                // The normal shape is `{}` (no authority).
                                authoritative_args: if input.is_object()
                                    && !input.as_object().unwrap().is_empty()
                                {
                                    Some(input.to_string())
                                } else {
                                    None
                                },
                                thinking_acc: String::new(),
                                signature: String::new(),
                                signature_seen: false,
                            },
                        );

                        // Emit the initial id and name so subscribers can pre-allocate UI for the tool call before arguments stream in
                        yield SamplingEvent::ToolCallDelta {
                            request_id: request_id.clone(),
                            tool_index,
                            id: Some(id),
                            name: Some(name),
                            arguments_delta: None,
                        };
                    }
                        // Encrypted reasoning the model chose to redact; not
                        // forwarded (no consumer claims redacted_thinking
                        // support), but RECORDED as inert so its stop is not
                        // misclassified as the fatal never-started-index class.
                        ContentBlock::RedactedThinking { .. }
                        | ContentBlock::Image { .. }
                        | ContentBlock::ToolResult { .. }
                        | ContentBlock::Unknown { .. } => {
                            let block_type = match content_block {
                                ContentBlock::Unknown { .. } => BlockType::Unknown,
                                _ => BlockType::Inert,
                            };
                            blocks.insert(
                                index,
                                BlockState {
                                    block_type,
                                    text_acc: String::new(),
                                    tool_name: String::new(),
                                    tool_id: String::new(),
                                    args_acc: String::new(),
                                    authoritative_args: None,
                                    thinking_acc: String::new(),
                                    signature: String::new(),
                                    signature_seen: false,
                                },
                            );
                        }
                    }
                    // end duplicate-check branch
                }
                }

                MessageStreamEvent::ContentBlockDelta { index, delta } => {
                    let delta_type = stream_delta_type(&delta);
                    if let Some(v) =
                        super::messages_invariants::check_content_block_delta(
                            index,
                            delta_type,
                            blocks.contains_key(&index),
                        )
                    {
                        if !v.is_fatal() {
                            tracing::warn!(violation = ?v, "messages-stream:invariant");
                        } else {
                            let message = format!(
                                "content_block_delta for unopened index {index} (delta {delta_type})"
                            );
                            yield SamplingEvent::Failed {
                                request_id: request_id.clone(),
                                error: SamplingErrorInfo::from(&SamplingError::StreamError {
                                    error_type: "unopened_index".to_owned(),
                                    message,
                                    code: None,
                                }),
                            };
                            return;
                        }
                    }
                    if let Some(state) = blocks.get_mut(&index) {
                        if matches!(
                            state.block_type,
                            BlockType::Unknown | BlockType::Inert
                        ) {
                            // D3 phantom / inert known-unmodeled kind: deltas
                            // are swallowed, never fatal.
                        } else {
                        match delta {
                            StreamDelta::ThinkingDelta { thinking } => {
                                if !thinking.is_empty() {
                                    state.thinking_acc.push_str(&thinking);
                                    if !first_token_emitted {
                                        first_token_emitted = true;
                                        yield SamplingEvent::FirstToken {
                                            request_id: request_id.clone(),
                                        };
                                    }
                                    chunk_index += 1;
                                    yield SamplingEvent::ChannelToken {
                                        request_id: request_id.clone(),
                                        channel: SamplingChannel::Reasoning,
                                        text: thinking,
                                        chunk_index,
                                    };
                                }
                            }
                            StreamDelta::SignatureDelta { signature } => {
                                for v in super::messages_invariants::check_signature_delta(
                                    index,
                                    state.block_type == BlockType::Thinking,
                                    state.thinking_acc.is_empty(),
                                    state.signature_seen,
                                ) {
                                    tracing::warn!(violation = ?v, "messages-stream:invariant");
                                }
                                // Last-wins (xli concatenates — documented
                                // divergence in messages_invariants.rs).
                                state.signature = signature;
                                state.signature_seen = true;
                            }
                            StreamDelta::TextDelta { text } => {
                                if !text.is_empty() {
                                    state.text_acc.push_str(&text);
                                    if !first_token_emitted {
                                        first_token_emitted = true;
                                        yield SamplingEvent::FirstToken {
                                            request_id: request_id.clone(),
                                        };
                                    }
                                    chunk_timestamps.push(Instant::now());
                                    chunk_index += 1;
                                    message_chunk_count += 1;
                                    yield SamplingEvent::ChannelToken {
                                        request_id: request_id.clone(),
                                        channel: SamplingChannel::Text,
                                        text,
                                        chunk_index,
                                    };
                                }
                            }
                            StreamDelta::InputJsonDelta { partial_json } => {
                                state.args_acc.push_str(&partial_json);
                                if let Some(&tool_index) = block_to_tool_index.get(&index) {
                                    yield SamplingEvent::ToolCallDelta {
                                        request_id: request_id.clone(),
                                        tool_index,
                                        id: None,
                                        name: None,
                                        arguments_delta: Some(partial_json),
                                    };
                                }
                            }
                        }
                        }
                    }
                }

                MessageStreamEvent::ContentBlockStop { index } => {
                    if let Some(v) =
                        super::messages_invariants::check_content_block_stop(
                            index,
                            blocks.contains_key(&index),
                        )
                    {
                        // xli StopForUnknownIndex: true corruption (the index
                        // never received a start — every start, including
                        // unknown-kind phantoms, records its index).
                        let message = format!("content_block_stop for unknown index {index}");
                        yield SamplingEvent::Failed {
                            request_id: request_id.clone(),
                            error: SamplingErrorInfo::from(&SamplingError::StreamError {
                                error_type: "unopened_index".to_owned(),
                                message,
                                code: None,
                            }),
                        };
                        return;
                    }
                    if let Some(state) = blocks.remove(&index) {
                        match state.block_type {
                            BlockType::Text => {
                                if !state.text_acc.is_empty() {
                                    if !assistant_text.is_empty() {
                                        assistant_text.push('\n');
                                    }
                                    assistant_text.push_str(&state.text_acc);
                                }
                            }
                            BlockType::Thinking => {
                                // Yield the encrypted signature at the thinking block's stop
                                // Partial-mode framing can then emit `signature_delta` before its `content_block_stop`
                                if !state.signature.is_empty() {
                                    yield SamplingEvent::ReasoningCompleted {
                                        request_id: request_id.clone(),
                                        signature: state.signature.clone(),
                                    };
                                }
                                if !state.thinking_acc.is_empty() || !state.signature.is_empty() {
                                    // Anthropic Messages API `Thinking` blocks uniquely carry an encrypted `signature` distinct from the text
                                    // Either field may be empty
                                    // Build directly rather than via `synthesized_reasoning_item` since the helper assumes a non-empty summary
                                    let summary = if state.thinking_acc.is_empty() {
                                        vec![]
                                    } else {
                                        vec![rs::SummaryPart::SummaryText(
                                            rs::SummaryTextContent {
                                                text: state.thinking_acc,
                                            },
                                        )]
                                    };
                                    let encrypted_content = if state.signature.is_empty() {
                                        None
                                    } else {
                                        Some(state.signature)
                                    };
                                    assistant_reasoning = Some(rs::ReasoningItem {
                                        id: String::new(),
                                        summary,
                                        content: None,
                                        encrypted_content,
                                        status: None,
                                    });
                                }
                            }
                            BlockType::ToolUse => {
                                assistant_tool_calls.push(ToolCall {
                                    id: std::sync::Arc::<str>::from(state.tool_id),
                                    name: state.tool_name,
                                    // R7: the authoritative `input` wins over
                                    // the accumulated deltas when present.
                                    arguments: std::sync::Arc::<str>::from(
                                        state
                                            .authoritative_args
                                            .unwrap_or_else(|| state.args_acc),
                                    ),
                                });
                            }
                            // D3 phantom / inert: the stop is a no-op.
                            BlockType::Unknown | BlockType::Inert => {}
                        }
                    }
                }

                MessageStreamEvent::MessageDelta { delta, usage } => {
                    // Normalize the provider's stop detail to a plain message; the shell logs it when it shows a refusal
                    if let Some(details) = delta.stop_details {
                        final_stop_message = details.explanation;
                    }
                    // Keep the exact wire string so consumers can echo it.
                    final_raw_stop_reason = delta
                        .stop_reason
                        .as_ref()
                        .map(messages::StopReason::wire_str);
                    // The matched stop sequence arrives on the same terminal delta (present only on a `stop_sequence` stop); carry it verbatim
                    if delta.stop_sequence.is_some() {
                        final_stop_sequence = delta.stop_sequence.clone();
                    }
                    final_stop_reason = delta.stop_reason.map(|sr| match sr {
                        messages::StopReason::EndTurn => StopReason::Stop,
                        messages::StopReason::MaxTokens => StopReason::Length,
                        messages::StopReason::StopSequence => StopReason::Stop,
                        messages::StopReason::ToolUse => StopReason::ToolCalls,
                        // The model declined to continue; whatever streamed is the complete response, so end the turn cleanly
                        messages::StopReason::Refusal => StopReason::ContentFilter,
                        messages::StopReason::PauseTurn => {
                            // Anthropic Messages API expects the client to resend to continue; we end the turn instead
                            tracing::warn!(
                                wire_stop_reason = "pause_turn",
                                "pause_turn ended the turn like stop (no auto-continue)"
                            );
                            StopReason::Stop
                        }
                        messages::StopReason::ModelContextWindowExceeded => {
                            // Output-side overflow on a successful stream maps to the Length stop class
                            // Compact-on-error recovery needs an Api error carrying model metadata and a prompt-side overflow; neither exists here
                            tracing::warn!(
                                wire_stop_reason = "model_context_window_exceeded",
                                "context window hit mid-generation; mapping to the Length stop class"
                            );
                            StopReason::Length
                        }
                        messages::StopReason::Unknown(wire) => {
                            tracing::warn!(
                                wire_stop_reason = %wire,
                                "unrecognized stop_reason in messages stream; treating as stop"
                            );
                            StopReason::Stop
                        }
                    });
                    let output_incoming = usage.output_tokens;
                    let input_incoming = usage.input_tokens;
                    // R2: usage counters are monotonic across
                    // message_start → message_delta; a regressed counter is
                    // warned and SKIPPED (previous kept) — recoverable,
                    // never fatal (R-risk-4: live audit at A6 finalizes the
                    // recoverable-vs-fatal call against L2 wire logs).
                    if let Some(v) =
                        super::messages_invariants::check_usage_counter(
                            "input_tokens",
                            input_incoming,
                            prev_input_tokens,
                        )
                    {
                        tracing::warn!(violation = ?v, "messages-stream:invariant");
                    } else if let Some(input) = input_incoming {
                        prev_input_tokens = input;
                        final_input_tokens = input;
                    }
                    if let Some(v) = super::messages_invariants::check_usage_counter(
                        "output_tokens",
                        Some(output_incoming),
                        prev_output_tokens,
                    ) {
                        tracing::warn!(violation = ?v, "messages-stream:invariant");
                    } else {
                        prev_output_tokens = output_incoming;
                        final_output_tokens = output_incoming;
                    }
                    // Optional on the delta; preserve message_start values when omitted.
                    if let Some(cache_read) = usage.cache_read_input_tokens {
                        final_cache_read_input_tokens = cache_read;
                    }
                    if let Some(cache_creation) = usage.cache_creation_input_tokens {
                        final_cache_creation_input_tokens = cache_creation;
                    }
                    // Override the message_start value when the terminal delta
                    // carries its own decomposition (preserve otherwise).
                    if let Some(details) = usage.output_tokens_details {
                        final_reasoning_tokens = details.thinking_tokens;
                    }
                }

                MessageStreamEvent::MessageStop => {
                    // R3: `message_stop` while any block is still open is a
                    // truncation — the provider never closed the block it
                    // (claiming) finished (xli eq-11/eq-19 shape).
                    if !blocks.is_empty() {
                        let open: Vec<u32> = blocks.keys().copied().collect();
                        let message = format!(
                            "message_stop before all blocks ended (open indices: {open:?})"
                        );
                        yield SamplingEvent::Failed {
                            request_id: request_id.clone(),
                            error: SamplingErrorInfo::from(&SamplingError::StreamError {
                                error_type: "unclosed_blocks".to_owned(),
                                message,
                                code: None,
                            }),
                        };
                        return;
                    }
                    // Final message complete; the loop exits naturally when the underlying stream ends
                    message_stop_seen = true;
                }

                MessageStreamEvent::Ping => {
                    // Liveness only, no action; the inner timeout was already reset above by the successful `next()`
                }

                MessageStreamEvent::Error { error } => {
                    let error_message = format!("{}: {}", error.r#type, error.message);
                    let err = SamplingError::Api {
                        status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                        message: error_message,
                        model_metadata: None,
                        retry_after_secs: None,
                        should_retry: None,
                        // Messages-style error events carry no code slot.
                        error_code: None,
                    };
                    yield SamplingEvent::Failed {
                        request_id: request_id.clone(),
                        error: SamplingErrorInfo::from(&err),
                    };
                    return;
                }
            }

            if event_has_content {
                last_content_chunk_at = Instant::now();
            } else if last_content_chunk_at.elapsed() > idle_timeout {
                let err = SamplingError::IdleTimeout {
                    elapsed_secs: idle_timeout.as_secs(),
                };
                yield SamplingEvent::Failed {
                    request_id: request_id.clone(),
                    error: SamplingErrorInfo::from(&err),
                };
                return;
            }
        }

        // R3: the raw stream ended without a `message_stop` — truncated
        // ("without done" shape). Pre-MW-3 this was a silent `Completed`;
        // spec G6: NEW hard failure, live-verified at A6.
        if !message_stop_seen {
            let err = SamplingError::StreamError {
                error_type: "stream_truncated".to_owned(),
                message: "stream ended without done (no message_stop)".to_owned(),
                code: None,
            };
            yield SamplingEvent::Failed {
                request_id: request_id.clone(),
                error: SamplingErrorInfo::from(&err),
            };
            return;
        }

        // A `Length` stop is NOT failed here
        // The transform completes with `stop_reason=Length` and `drive_l2` decides fail-vs-salvage per the request's `LengthPolicy`

        // ── Build the final response ─────────────────────────────────
        let model_id = final_model.unwrap_or_default();
        // Match the OAI Responses convention: prompt_tokens holds the full prompt, cached_prompt_tokens counts cache hits only
        let total_prompt_tokens = final_input_tokens
            .saturating_add(final_cache_read_input_tokens)
            .saturating_add(final_cache_creation_input_tokens);
        let usage = if total_prompt_tokens > 0 || final_output_tokens > 0 {
            Some(TokenUsage {
                prompt_tokens: total_prompt_tokens,
                completion_tokens: final_output_tokens,
                total_tokens: total_prompt_tokens.saturating_add(final_output_tokens),
                reasoning_tokens: final_reasoning_tokens,
                cached_prompt_tokens: final_cache_read_input_tokens,
                cache_creation_prompt_tokens: final_cache_creation_input_tokens,
            })
        } else {
            None
        };

        let stop_reason = if final_stop_reason == Some(StopReason::Length) {
            // Length wins even over completed tool_use blocks
            // The provider closes a block it cut mid-stream, so the trailing call's arguments may be silently truncated
            // Fail-vs-salvage belongs to the `LengthPolicy` gate
            final_stop_reason
        } else if !assistant_tool_calls.is_empty() {
            // Completed tool_use blocks win even over Refusal: the calls are real model output the agent loop must resolve
            Some(StopReason::ToolCalls)
        } else {
            final_stop_reason
        };

        // R3: at a completed (non-Length) terminal, every tool call's
        // arguments must be empty (G5: the proven `""` shape) or valid JSON —
        // the provider asserted the call is complete, so garbage args are a
        // deterministic failure (the eq-10 tool_args_state class). `Length`
        // terminals are EXCLUDED: the provider cut the stream, the
        // intentionally-truncated prefix is the 09-09 proven salvage shape
        // for `drive_l2`'s LengthPolicy (pinned by
        // max_tokens_with_tool_use_keeps_length_stop).
        if stop_reason != Some(StopReason::Length) {
            for tc in &assistant_tool_calls {
                if !tc.arguments.is_empty()
                    && serde_json::from_str::<serde_json::Value>(&tc.arguments).is_err()
                {
                    let message = format!(
                        "tool call `{}` arguments are not valid JSON",
                        tc.name
                    );
                    yield SamplingEvent::Failed {
                        request_id: request_id.clone(),
                        error: SamplingErrorInfo::from(&SamplingError::StreamError {
                            error_type: "invalid_tool_args".to_owned(),
                            message,
                            code: None,
                        }),
                    };
                    return;
                }
            }
        }

        let assistant_item = ConversationItem::Assistant(AssistantItem {
            content: std::sync::Arc::<str>::from(assistant_text),
            tool_calls: assistant_tool_calls,
            model_id: Some(model_id),
            model_fingerprint: None,
            // The Messages API does not echo the applied reasoning effort.
            reasoning_effort: None,
        });

        let mut items: Vec<ConversationItem> = Vec::new();
        if let Some(r) = assistant_reasoning {
            items.push(ConversationItem::Reasoning(r));
        }
        items.push(assistant_item);

        let stream_end = Instant::now();
        let metrics =
            InferenceLatencyStats::from_timestamps(stream_start, &chunk_timestamps, stream_end);

        decode_region
            .span()
            .record("ttlb_ms", metrics.time_to_last_byte_ms as i64);
        decode_region
            .span()
            .record("chunk_count", metrics.chunk_count as i64);
        if let Some(ttft) = metrics.time_to_first_token_ms {
            decode_region.span().record("ttft_ms", ttft as i64);
        }
        if let Some(u) = usage.as_ref() {
            decode_region
                .span()
                .record("output_tokens", u.completion_tokens as i64);
        }
        drop(decode_region);

        let response = ConversationResponse {
            items,
            stop_reason,
            usage,
            // Anthropic Messages API carries no cost on the wire.
            cost_usd_ticks: None,
            message_chunks_emitted: message_chunk_count,
            doom_loop_signals: Vec::new(),
            stop_message: final_stop_message,
            message_id: final_message_id,
            raw_stop_reason: final_raw_stop_reason,
            stop_sequence: final_stop_sequence,
        };

        yield SamplingEvent::Completed {
            request_id: request_id.clone(),
            response: Box::new(response),
            metrics,
        };
    }
}

#[cfg(test)]
#[path = "messages_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "messages_equiv_tests.rs"]
mod equiv_tests;
