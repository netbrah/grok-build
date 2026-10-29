//! Layer-2 stream transform for the Google Gemini `streamGenerateContent` wire.
//!
//! Consumes a raw stream of [`GenerateContentResponse`] chunks (one per SSE
//! `data:` line — the wire has no event types and no `[DONE]` sentinel; the
//! stream simply ends after the chunk carrying `finishReason`) and produces
//! [`SamplingEvent`]s. Pure: no I/O, no shell coupling.
//!
//! Chunk shape: `candidates[0].content.parts[]` carries incremental parts.
//! Text parts are deltas (concatenate); `thought: true` text parts are
//! thought-summary deltas (the reasoning channel); `functionCall` parts are
//! complete (the wire never splits one call's `args` across chunks). A
//! `thoughtSignature` rides on whichever part the model attached it to —
//! normally the first `functionCall` of a step, else the last text part.
//! `usageMetadata` is cumulative (last wins); `modelVersion` / `responseId`
//! are repeated on every chunk.
//!
//! Opaque-reasoning contract (the point of the native wire): the signature
//! is surfaced as `encrypted_content` on one synthesized
//! `ConversationItem::Reasoning` sibling emitted ahead of the Assistant item,
//! with the thought summary as its `summary` text — the exact storage shape
//! the Messages transform uses, so compaction, switch-time projection
//! (`project_reasoning`) and the Gemini request projector
//! (`build_generate_content_request`) need no dialect-specific storage.
//! A step with parallel function calls carries ONE signature (on the first
//! call); the projector re-attaches it there on replay.
//!
//! Function-call ids: `functionCall.id` is optional on the wire (both the
//! public API and Vertex define it; Gemini 3+ populates it). A non-empty id
//! that is unique within the response is kept as the stored call id, so the
//! projector can echo it back on the matching `functionResponse` as Google
//! asks; an absent or duplicated id gets a locally minted `call_<uuid>`
//! instead. Either way the tool-result pairing machinery (orphan cleanup,
//! `tool_call_id` lookup) works unchanged.

use std::time::{Duration, Instant};

use futures_util::StreamExt;
use futures_util::stream::{BoxStream, Stream};

use xai_grok_sampling_types::gemini::{FinishReason, GenerateContentResponse};
use xai_grok_sampling_types::{
    AssistantItem, ConversationItem, ConversationResponse, ResponseModelMetadata, SamplingError,
    StopReason, TokenUsage, ToolCall, rs,
};

use crate::events::{SamplingChannel, SamplingErrorInfo, SamplingEvent};
use crate::metrics::InferenceLatencyStats;
use crate::types::RequestId;

/// Locally minted tool-call id for a wire `functionCall` without a usable id.
fn mint_call_id() -> String {
    format!("call_{}", uuid::Uuid::new_v4().simple())
}

/// The call id to store: the wire's own when it is non-empty and not already
/// taken by an earlier call of this response, else a minted one.
fn resolve_call_id(wire_id: Option<String>, taken: &[ToolCall]) -> String {
    match wire_id {
        Some(id) if !id.is_empty() && !taken.iter().any(|tc| *tc.id == *id) => id,
        _ => mint_call_id(),
    }
}

/// Transform a raw Gemini `streamGenerateContent` chunk stream into a stream of
/// [`SamplingEvent`]s. Yields exactly one terminal event
/// ([`SamplingEvent::Completed`] or [`SamplingEvent::Failed`]) per request.
pub fn stream_generate_content<'a>(
    raw_stream: BoxStream<'a, Result<GenerateContentResponse, SamplingError>>,
    model_metadata: Option<ResponseModelMetadata>,
    request_id: RequestId,
    idle_timeout: Duration,
) -> impl Stream<Item = SamplingEvent> + Send + 'a {
    async_stream::stream! {
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

        // Response-level accumulators.
        let mut assistant_text = String::new();
        let mut thought_text = String::new();
        let mut assistant_tool_calls: Vec<ToolCall> = Vec::new();
        // The step's opaque signature. The first `functionCall`'s signature
        // is authoritative (parallel calls carry exactly one, on the first);
        // without calls the last signature seen (the last text part's) wins.
        let mut call_signature: Option<String> = None;
        let mut last_signature: Option<String> = None;

        let mut final_finish: Option<FinishReason> = None;
        let mut final_finish_message: Option<String> = None;
        let mut block_reason: Option<String> = None;
        let mut block_message: Option<String> = None;
        let mut final_model: Option<String> = None;
        let mut final_response_id: Option<String> = None;
        let mut final_usage: Option<xai_grok_sampling_types::gemini::UsageMetadata> = None;

        let mut chunk_index: u64 = 0;
        let mut message_chunk_count: u64 = 0;
        let mut first_token_emitted = false;
        let mut any_chunk_seen = false;

        let mut stream = raw_stream;
        loop {
            let chunk_result = match tokio::time::timeout(idle_timeout, stream.next()).await {
                Ok(Some(chunk_result)) => chunk_result,
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

            let chunk = match chunk_result {
                Ok(chunk) => chunk,
                Err(err) => {
                    yield SamplingEvent::Failed {
                        request_id: request_id.clone(),
                        error: SamplingErrorInfo::from(&err),
                    };
                    return;
                }
            };
            any_chunk_seen = true;

            if let Some(model) = chunk.model_version.filter(|m| !m.is_empty()) {
                final_model = Some(model);
            }
            if let Some(id) = chunk.response_id.filter(|id| !id.is_empty()) {
                final_response_id = Some(id);
            }
            if let Some(usage) = chunk.usage_metadata {
                // Cumulative on the wire: last wins.
                final_usage = Some(usage);
            }
            if let Some(pf) = chunk.prompt_feedback
                && let Some(reason) = pf.block_reason.filter(|r| !r.is_empty())
            {
                block_reason = Some(reason);
                if pf.block_reason_message.is_some() {
                    block_message = pf.block_reason_message;
                }
            }

            // Only the first candidate is modelled (`candidateCount` is never
            // requested above 1).
            let Some(candidate) = chunk.candidates.into_iter().next() else {
                continue;
            };
            if let Some(reason) = candidate.finish_reason {
                final_finish = Some(reason);
            }
            if candidate.finish_message.is_some() {
                final_finish_message = candidate.finish_message;
            }
            let Some(content) = candidate.content else {
                continue;
            };

            for part in content.parts {
                let signature = part.thought_signature.filter(|s| !s.is_empty());
                if let Some(call) = part.function_call {
                    if call_signature.is_none() {
                        call_signature = signature.clone();
                    }
                    let id = resolve_call_id(call.id, &assistant_tool_calls);
                    let arguments = match call.args {
                        Some(serde_json::Value::Null) | None => "{}".to_owned(),
                        Some(args) => serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_owned()),
                    };
                    let tool_index = u32::try_from(assistant_tool_calls.len()).unwrap_or(u32::MAX);
                    assistant_tool_calls.push(ToolCall {
                        id: std::sync::Arc::<str>::from(id.as_str()),
                        name: call.name.clone(),
                        arguments: std::sync::Arc::<str>::from(arguments.as_str()),
                    });
                    yield SamplingEvent::ToolCallDelta {
                        request_id: request_id.clone(),
                        tool_index,
                        id: Some(id),
                        name: Some(call.name),
                        arguments_delta: Some(arguments),
                    };
                } else if let Some(text) = part.text {
                    if part.thought == Some(true) {
                        if !text.is_empty() {
                            thought_text.push_str(&text);
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
                                text,
                                chunk_index,
                            };
                        }
                    } else if !text.is_empty() {
                        assistant_text.push_str(&text);
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
                // Other part kinds (inlineData / fileData / executableCode …)
                // are not modelled on the assistant side and are skipped.
                if signature.is_some() {
                    last_signature = signature;
                }
            }
        }

        // The wire ends after the terminal chunk; an empty stream (or one
        // that only carried heartbeats) means the connection was cut.
        if !any_chunk_seen {
            let err = SamplingError::StreamError {
                error_type: "stream_truncated".to_owned(),
                message: "stream ended without any generateContent chunk".to_owned(),
                code: None,
            };
            yield SamplingEvent::Failed {
                request_id: request_id.clone(),
                error: SamplingErrorInfo::from(&err),
            };
            return;
        }

        // ── Terminal classification ──────────────────────────────────
        // A prompt-level block arrives with no candidates at all; it is the
        // refusal terminal, never a stream error.
        let raw_stop_reason: Option<String> = match (&final_finish, &block_reason) {
            (Some(reason), _) => Some(reason.wire_str()),
            (None, Some(block)) => Some(format!("BLOCKED_{block}")),
            (None, None) => None,
        };

        let mut stop_reason = match &final_finish {
            Some(FinishReason::MaxTokens) => Some(StopReason::Length),
            Some(
                FinishReason::MalformedFunctionCall
                | FinishReason::UnexpectedToolCall
                | FinishReason::TooManyToolCalls
                | FinishReason::MalformedResponse,
            ) => {
                let reason = final_finish.as_ref().map(FinishReason::wire_str).unwrap_or_default();
                let message = final_finish_message
                    .clone()
                    .map(|m| format!("{reason}: {m}"))
                    .unwrap_or_else(|| format!("the model's output was rejected by the service ({reason})"));
                yield SamplingEvent::Failed {
                    request_id: request_id.clone(),
                    error: SamplingErrorInfo::from(&SamplingError::StreamError {
                        error_type: "malformed_model_output".to_owned(),
                        message,
                        code: None,
                    }),
                };
                return;
            }
            Some(FinishReason::Stop) => Some(StopReason::Stop),
            Some(FinishReason::Unknown(wire)) => {
                tracing::warn!(finish_reason = %wire, "gemini-stream: unrecognized finishReason; completing as Stop");
                Some(StopReason::Stop)
            }
            // Policy reasons (SAFETY, RECITATION, …) are the typed refusal;
            // OTHER / UNSPECIFIED / NO_IMAGE complete for a reason the
            // transform does not model — the raw value is carried for display
            // and the turn completes as a plain stop.
            Some(reason) => Some(if reason.is_content_filter() {
                StopReason::ContentFilter
            } else {
                StopReason::Stop
            }),
            None if block_reason.is_some() => Some(StopReason::ContentFilter),
            None => {
                // A stream that ends without a terminal finishReason was cut
                // mid-generation (the wire always closes a candidate).
                let err = SamplingError::StreamError {
                    error_type: "stream_truncated".to_owned(),
                    message: "stream ended without a finishReason".to_owned(),
                    code: None,
                };
                yield SamplingEvent::Failed {
                    request_id: request_id.clone(),
                    error: SamplingErrorInfo::from(&err),
                };
                return;
            }
        };

        if stop_reason == Some(StopReason::Stop) && !assistant_tool_calls.is_empty() {
            // Completed function calls win over a plain stop: the calls are
            // real model output the agent loop must resolve. Length keeps its
            // terminal (fail-vs-salvage belongs to the LengthPolicy gate) and
            // ContentFilter keeps its typed refusal.
            stop_reason = Some(StopReason::ToolCalls);
        }

        // At a completed (non-Length) terminal every call's arguments must be
        // valid JSON — the service asserted the call is complete.
        if !matches!(stop_reason, Some(StopReason::Length)) {
            for tc in &assistant_tool_calls {
                if !tc.arguments.is_empty()
                    && serde_json::from_str::<serde_json::Value>(&tc.arguments).is_err()
                {
                    let message = format!("tool call `{}` arguments are not valid JSON", tc.name);
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

        // ── Build the final response ─────────────────────────────────
        let usage = final_usage.map(|u| {
            // `candidatesTokenCount` excludes thoughts; the port's
            // completion_tokens is the full generated count (Responses parity).
            let completion = u.candidates_token_count.saturating_add(u.thoughts_token_count);
            // A proxy that drops `promptTokenCount` still reports the total:
            // recover the prompt from it. Cached tokens are a subset of the
            // prompt by definition; a count above it is a reporting quirk.
            let prompt = if u.prompt_token_count > 0 {
                u.prompt_token_count
            } else {
                u.total_token_count.saturating_sub(completion)
            };
            let total = if u.total_token_count > 0 {
                u.total_token_count
            } else {
                prompt.saturating_add(completion)
            };
            TokenUsage {
                prompt_tokens: prompt,
                completion_tokens: completion,
                total_tokens: total,
                reasoning_tokens: u.thoughts_token_count,
                cached_prompt_tokens: u.cached_content_token_count.min(prompt),
                cache_creation_prompt_tokens: 0,
                cache_creation_5m_input_tokens: 0,
                cache_creation_1h_input_tokens: 0,
            }
        });

        let signature = if assistant_tool_calls.is_empty() {
            last_signature
        } else {
            call_signature.or(last_signature)
        };
        if let Some(sig) = signature.as_ref() {
            yield SamplingEvent::ReasoningCompleted {
                request_id: request_id.clone(),
                signature: sig.clone(),
            };
        }

        let mut items: Vec<ConversationItem> = Vec::new();
        if !thought_text.is_empty() || signature.is_some() {
            let summary = if thought_text.is_empty() {
                Vec::new()
            } else {
                vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                    text: thought_text,
                })]
            };
            items.push(ConversationItem::Reasoning(
                rs::ReasoningItem {
                    // Same synthesized shape as the Messages transform: a
                    // present-but-empty id (the wire mints none).
                    id: Some(String::new()),
                    summary,
                    content: None,
                    encrypted_content: signature,
                    status: None,
                }
                .into(),
            ));
        }
        items.push(ConversationItem::Assistant(AssistantItem {
            content: std::sync::Arc::<str>::from(assistant_text),
            tool_calls: assistant_tool_calls,
            // `modelVersion` is the resolved concrete version (e.g.
            // `gemini-2.5-pro-preview-06-05`); it keys the projector's R8
            // identity check against the request slug, so a missing value
            // must stay `None` rather than inventing one.
            model_id: final_model,
            model_fingerprint: None,
            reasoning_effort: None,
        }));

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

        let stop_message = match (&final_finish_message, &block_message) {
            (Some(m), _) => Some(m.clone()),
            (None, Some(m)) => Some(m.clone()),
            (None, None) => block_reason.as_ref().map(|b| format!("prompt blocked: {b}")),
        };

        let response = ConversationResponse {
            items,
            stop_reason,
            usage,
            cost_usd_ticks: None,
            message_chunks_emitted: message_chunk_count,
            doom_loop_signals: Vec::new(),
            stop_message,
            message_id: final_response_id,
            raw_stop_reason,
            stop_sequence: None,
        };

        yield SamplingEvent::Completed {
            request_id: request_id.clone(),
            response: Box::new(response),
            metrics,
        };
    }
}

#[cfg(test)]
#[path = "gemini_tests.rs"]
mod tests;
