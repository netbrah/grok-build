//! These tests live outside `messages.rs` so the implementation reads top-to-bottom.
//! `#[path = "messages_tests.rs"] mod tests;` in messages.rs wires them in.

use super::*;
use futures_util::stream;
use std::pin::pin;
use xai_grok_sampling_types::messages::{
    ContentBlock, MessageDeltaBody, MessageDeltaUsage, MessagesResponse, MessagesUsage,
    OutputTokensDetails, StreamDelta, StreamError,
};

fn rid() -> RequestId {
    RequestId::from("msg-test")
}

fn message_start() -> MessageStreamEvent {
    MessageStreamEvent::MessageStart {
        message: MessagesResponse {
            id: "msg_1".into(),
            r#type: "message".into(),
            role: "assistant".into(),
            content: vec![],
            model: "messages-compatible-model".into(),
            stop_reason: None,
            usage: MessagesUsage {
                input_tokens: 10,
                output_tokens: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                output_tokens_details: None,
                cache_creation: None,
            },
        },
    }
}

fn text_block_start(index: u32) -> MessageStreamEvent {
    MessageStreamEvent::ContentBlockStart {
        index,
        content_block: ContentBlock::Text {
            text: String::new(),
            cache_control: None,
        },
    }
}

fn text_delta(index: u32, text: &str) -> MessageStreamEvent {
    MessageStreamEvent::ContentBlockDelta {
        index,
        delta: StreamDelta::TextDelta { text: text.into() },
    }
}

fn block_stop(index: u32) -> MessageStreamEvent {
    MessageStreamEvent::ContentBlockStop { index }
}

fn message_delta_with_stop(stop: messages::StopReason) -> MessageStreamEvent {
    MessageStreamEvent::MessageDelta {
        delta: MessageDeltaBody {
            stop_reason: Some(stop),
            stop_sequence: None,
            stop_details: None,
        },
        usage: MessageDeltaUsage {
            output_tokens: 5,
            input_tokens: Some(10),
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            output_tokens_details: None,
        },
    }
}

/// A refusal `message_delta` carrying a provider `stop_details.explanation`, mirroring the Anthropic Messages API ToS auto-refusal wire shape.
fn message_delta_refusal_with_explanation(explanation: &str) -> MessageStreamEvent {
    MessageStreamEvent::MessageDelta {
        delta: MessageDeltaBody {
            stop_reason: Some(messages::StopReason::Refusal),
            stop_sequence: None,
            stop_details: Some(messages::StopDetails {
                r#type: Some("refusal".to_string()),
                category: Some("frontier_llm".to_string()),
                explanation: Some(explanation.to_string()),
            }),
        },
        usage: MessageDeltaUsage {
            output_tokens: 0,
            input_tokens: Some(10),
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            output_tokens_details: None,
        },
    }
}

async fn collect(s: impl Stream<Item = SamplingEvent>) -> Vec<SamplingEvent> {
    let mut out = Vec::new();
    let mut s = pin!(s);
    while let Some(ev) = s.next().await {
        out.push(ev);
    }
    out
}

/// R3: a stream that ends without `message_stop` is truncated — the pre-MW-3
/// behavior was a silent `Completed`, which is now a hard failure (spec G6:
/// NEW hard failure; live verification at A6 records the observed terminal
/// event of every live turn).
///
/// Provenance: hyper-grok-build@45e984f3 — packages/ai/xai-grok-sampler/src/pi_messages.rs :: stream_fails_truncated_and_unended_blocks (re-expressed, "without done" half; the pre-MW-3 test of this file pinned the old silent-complete behavior and is amended here, disclosed in the MW-3 R2/R3 commit body)
#[tokio::test]
async fn empty_stream_fails_truncated_without_done() {
    let raw = stream::iter(Vec::<Result<MessageStreamEvent, SamplingError>>::new()).boxed();
    let events = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;
    assert_eq!(events.len(), 2);
    assert!(matches!(events[0], SamplingEvent::StreamStarted { .. }));
    match &events[1] {
        SamplingEvent::Failed { error, .. } => {
            assert_eq!(error.kind, crate::events::SamplingErrorKind::Api);
            assert!(
                error.message.contains("stream error (stream_truncated)"),
                "got {}",
                error.message
            );
            assert!(error.message.contains("without done"));
            assert!(
                error.is_retryable,
                "stream errors keep the actor retry path"
            );
        }
        other => panic!("expected Failed(stream_truncated), got {other:?}"),
    }
}

#[tokio::test]
async fn text_block_assembles_into_completed_response() {
    let events: Vec<Result<MessageStreamEvent, SamplingError>> = vec![
        Ok(message_start()),
        Ok(text_block_start(0)),
        Ok(text_delta(0, "Hello, ")),
        Ok(text_delta(0, "world!")),
        Ok(block_stop(0)),
        Ok(message_delta_with_stop(messages::StopReason::EndTurn)),
        Ok(MessageStreamEvent::MessageStop),
    ];
    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    let text_tokens: Vec<&str> = evs
        .iter()
        .filter_map(|e| match e {
            SamplingEvent::ChannelToken {
                channel: SamplingChannel::Text,
                text,
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text_tokens, vec!["Hello, ", "world!"]);

    match evs.last().unwrap() {
        SamplingEvent::Completed { response, .. } => {
            let a = response.assistant().expect("assistant item present");
            assert_eq!(a.content.as_ref(), "Hello, world!");
            assert_eq!(a.model_id.as_deref(), Some("messages-compatible-model"));
            assert_eq!(response.stop_reason, Some(StopReason::Stop));
            // Provider message id and the verbatim wire stop reason survive onto the response (collapsed `stop_reason` loses the string)
            assert_eq!(response.message_id.as_deref(), Some("msg_1"));
            assert_eq!(response.raw_stop_reason.as_deref(), Some("end_turn"));
            let u = response.usage.as_ref().expect("usage extracted");
            assert_eq!(u.prompt_tokens, 10);
            assert_eq!(u.completion_tokens, 5);
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

#[tokio::test]
async fn thinking_block_emits_reasoning_channel_and_preserved_in_response() {
    let thinking_start = MessageStreamEvent::ContentBlockStart {
        index: 0,
        content_block: ContentBlock::Thinking {
            thinking: String::new(),
            signature: String::new(),
        },
    };
    let thinking_delta = MessageStreamEvent::ContentBlockDelta {
        index: 0,
        delta: StreamDelta::ThinkingDelta {
            thinking: "let me think...".into(),
        },
    };
    let sig_delta = MessageStreamEvent::ContentBlockDelta {
        index: 0,
        delta: StreamDelta::SignatureDelta {
            signature: "abc123".into(),
        },
    };
    let events: Vec<Result<MessageStreamEvent, SamplingError>> = vec![
        Ok(message_start()),
        Ok(thinking_start),
        Ok(thinking_delta),
        Ok(sig_delta),
        Ok(block_stop(0)),
        Ok(MessageStreamEvent::MessageStop),
    ];
    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    let reasoning_tokens: Vec<&str> = evs
        .iter()
        .filter_map(|e| match e {
            SamplingEvent::ChannelToken {
                channel: SamplingChannel::Reasoning,
                text,
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(reasoning_tokens, vec!["let me think..."]);

    match evs.last().unwrap() {
        SamplingEvent::Completed { response, .. } => {
            let r = response
                .reasoning_items()
                .next()
                .expect("reasoning sibling preserved");
            let rs::SummaryPart::SummaryText(t) = &r.summary[0];
            assert_eq!(t.text, "let me think...");
            assert_eq!(r.encrypted_content.as_deref(), Some("abc123"));
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

/// `thinking(sig1) → text → thinking(sig2)` must emit each thinking block's own signature, in order, on its own `ReasoningCompleted`.
/// The event fires at the block's stop, so per-index signatures reach the headless reducer instead of collapsing to one.
#[tokio::test]
async fn multiple_thinking_blocks_emit_per_block_signatures_in_order() {
    let thinking_block = |index: u32, text: &str, sig: &str| {
        vec![
            Ok(MessageStreamEvent::ContentBlockStart {
                index,
                content_block: ContentBlock::Thinking {
                    thinking: String::new(),
                    signature: String::new(),
                },
            }),
            Ok(MessageStreamEvent::ContentBlockDelta {
                index,
                delta: StreamDelta::ThinkingDelta {
                    thinking: text.into(),
                },
            }),
            Ok(MessageStreamEvent::ContentBlockDelta {
                index,
                delta: StreamDelta::SignatureDelta {
                    signature: sig.into(),
                },
            }),
            Ok(block_stop(index)),
        ]
    };
    let mut events: Vec<Result<MessageStreamEvent, SamplingError>> = vec![Ok(message_start())];
    events.extend(thinking_block(0, "first", "sig-1"));
    events.push(Ok(text_block_start(1)));
    events.push(Ok(text_delta(1, "interlude")));
    events.push(Ok(block_stop(1)));
    events.extend(thinking_block(2, "second", "sig-2"));
    events.push(Ok(MessageStreamEvent::MessageStop));

    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    let sigs: Vec<&str> = evs
        .iter()
        .filter_map(|e| match e {
            SamplingEvent::ReasoningCompleted { signature, .. } => Some(signature.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        sigs,
        vec!["sig-1", "sig-2"],
        "each thinking block emits its own signature in order"
    );
}

#[tokio::test]
async fn tool_use_block_assembles_into_tool_call() {
    let tool_start = MessageStreamEvent::ContentBlockStart {
        index: 0,
        content_block: ContentBlock::ToolUse {
            id: "call_xyz".into(),
            name: "do_thing".into(),
            input: serde_json::json!({}),
            // Set: a parser matching only the absent case must fail here.
            cache_control: Some(xai_grok_sampling_types::messages::CacheControl::ephemeral()),
        },
    };
    let arg_delta_1 = MessageStreamEvent::ContentBlockDelta {
        index: 0,
        delta: StreamDelta::InputJsonDelta {
            partial_json: "{\"x\":".into(),
        },
    };
    let arg_delta_2 = MessageStreamEvent::ContentBlockDelta {
        index: 0,
        delta: StreamDelta::InputJsonDelta {
            partial_json: "1}".into(),
        },
    };
    let events: Vec<Result<MessageStreamEvent, SamplingError>> = vec![
        Ok(message_start()),
        Ok(tool_start),
        Ok(arg_delta_1),
        Ok(arg_delta_2),
        Ok(block_stop(0)),
        Ok(message_delta_with_stop(messages::StopReason::ToolUse)),
        Ok(MessageStreamEvent::MessageStop),
    ];
    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    let deltas: Vec<_> = evs
        .iter()
        .filter_map(|e| match e {
            SamplingEvent::ToolCallDelta {
                tool_index,
                id,
                name,
                arguments_delta,
                ..
            } => Some((
                *tool_index,
                id.clone(),
                name.clone(),
                arguments_delta.clone(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(deltas.len(), 3);
    assert_eq!(deltas[0].0, 0);
    assert_eq!(deltas[0].1.as_deref(), Some("call_xyz"));
    assert_eq!(deltas[0].2.as_deref(), Some("do_thing"));
    assert_eq!(deltas[0].3, None);
    assert_eq!(deltas[1].3.as_deref(), Some("{\"x\":"));
    assert_eq!(deltas[2].3.as_deref(), Some("1}"));

    match evs.last().unwrap() {
        SamplingEvent::Completed { response, .. } => {
            let calls = response.tool_calls();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].id.as_ref(), "call_xyz");
            assert_eq!(calls[0].name, "do_thing");
            assert_eq!(calls[0].arguments.as_ref(), "{\"x\":1}");
            assert_eq!(response.stop_reason, Some(StopReason::ToolCalls));
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

/// Regression: a stream whose terminal `message_delta` carries `stop_reason: "refusal"` must complete cleanly.
/// Erroring out would discard the already-streamed response.
#[tokio::test]
async fn refusal_stop_reason_completes_stream() {
    let events: Vec<Result<MessageStreamEvent, SamplingError>> = vec![
        Ok(message_start()),
        Ok(text_block_start(0)),
        Ok(text_delta(0, "I can't help with that.")),
        Ok(block_stop(0)),
        Ok(message_delta_with_stop(messages::StopReason::Refusal)),
        Ok(MessageStreamEvent::MessageStop),
    ];
    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    assert!(
        !evs.iter()
            .any(|e| matches!(e, SamplingEvent::Failed { .. })),
        "refusal stream must not yield Failed: {evs:?}"
    );
    match evs.last().unwrap() {
        SamplingEvent::Completed { response, .. } => {
            let a = response.assistant().expect("assistant item present");
            assert_eq!(a.content.as_ref(), "I can't help with that.");
            assert_eq!(response.stop_reason, Some(StopReason::ContentFilter));
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

/// A refusal `stop_details.explanation` on the terminal delta must land on the completed `ConversationResponse.stop_message`.
/// The agent loop shows the provider's reason from there; otherwise the turn ends empty and silent.
#[tokio::test]
async fn refusal_stop_message_flows_to_response() {
    let explanation = "This request was blocked by the provider's content policy.";
    let events: Vec<Result<MessageStreamEvent, SamplingError>> = vec![
        Ok(message_start()),
        Ok(message_delta_refusal_with_explanation(explanation)),
        Ok(MessageStreamEvent::MessageStop),
    ];
    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    match evs.last().unwrap() {
        SamplingEvent::Completed { response, .. } => {
            assert_eq!(response.stop_reason, Some(StopReason::ContentFilter));
            assert_eq!(
                response.stop_message.as_deref(),
                Some(explanation),
                "provider explanation normalized onto stop_message"
            );
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

#[tokio::test]
async fn pause_turn_and_unknown_stop_reasons_complete_as_stop() {
    for stop in [
        messages::StopReason::PauseTurn,
        messages::StopReason::Unknown("mystery_reason".to_string()),
    ] {
        let label = format!("{stop:?}");
        let events: Vec<Result<MessageStreamEvent, SamplingError>> = vec![
            Ok(message_start()),
            Ok(text_block_start(0)),
            Ok(text_delta(0, "partial answer")),
            Ok(block_stop(0)),
            Ok(message_delta_with_stop(stop)),
            Ok(MessageStreamEvent::MessageStop),
        ];
        let raw = stream::iter(events).boxed();
        let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;
        match evs.last().unwrap() {
            SamplingEvent::Completed { response, .. } => {
                assert_eq!(
                    response.stop_reason,
                    Some(StopReason::Stop),
                    "{label} must end the turn like stop"
                );
            }
            other => panic!("{label}: expected Completed, got {other:?}"),
        }
    }
}

/// A plain `max_tokens` stop with only text completes with `stop_reason=Length` and keeps the partial text.
#[tokio::test]
async fn max_tokens_text_only_completes_with_length_stop() {
    let events: Vec<Result<MessageStreamEvent, SamplingError>> = vec![
        Ok(message_start()),
        Ok(text_block_start(0)),
        Ok(text_delta(0, "cut answ")),
        Ok(block_stop(0)),
        Ok(message_delta_with_stop(messages::StopReason::MaxTokens)),
        Ok(MessageStreamEvent::MessageStop),
    ];
    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    match evs.last().unwrap() {
        SamplingEvent::Completed { response, .. } => {
            assert_eq!(response.stop_reason, Some(StopReason::Length));
            assert_eq!(response.assistant_text(), "cut answ");
        }
        other => panic!("expected Completed(Length), got {other:?}"),
    }
}

/// A max_tokens stop carrying a completed tool_use block keeps `stop_reason=Length`.
/// The ToolCalls override must not mask the truncation: the block's arguments may be a silently-truncated prefix.
#[tokio::test]
async fn max_tokens_with_tool_use_keeps_length_stop() {
    let tool_start = MessageStreamEvent::ContentBlockStart {
        index: 0,
        content_block: ContentBlock::ToolUse {
            id: "call_cut".into(),
            name: "do_thing".into(),
            input: serde_json::json!({}),
            cache_control: None,
        },
    };
    let arg_delta = MessageStreamEvent::ContentBlockDelta {
        index: 0,
        delta: StreamDelta::InputJsonDelta {
            partial_json: "{\"x\": \"trunc".into(),
        },
    };
    let events: Vec<Result<MessageStreamEvent, SamplingError>> = vec![
        Ok(message_start()),
        Ok(tool_start),
        Ok(arg_delta),
        Ok(block_stop(0)),
        Ok(message_delta_with_stop(messages::StopReason::MaxTokens)),
        Ok(MessageStreamEvent::MessageStop),
    ];
    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    match evs.last().unwrap() {
        SamplingEvent::Completed { response, .. } => {
            assert_eq!(response.stop_reason, Some(StopReason::Length));
            assert_eq!(response.tool_calls().len(), 1, "tool call still carried");
        }
        other => panic!("expected Completed(Length), got {other:?}"),
    }
}

/// A tool_use block closed with zero argument deltas collects as an empty-arguments tool call.
/// That is the shape `LengthPolicy::verdict` salvages.
#[tokio::test]
async fn max_tokens_tool_use_without_arg_deltas_collects_empty_arguments() {
    let tool_start = MessageStreamEvent::ContentBlockStart {
        index: 0,
        content_block: ContentBlock::ToolUse {
            id: "call_no_args".into(),
            name: "do_thing".into(),
            input: serde_json::json!({}),
            cache_control: None,
        },
    };
    let events: Vec<Result<MessageStreamEvent, SamplingError>> = vec![
        Ok(message_start()),
        Ok(tool_start),
        Ok(block_stop(0)),
        Ok(message_delta_with_stop(messages::StopReason::MaxTokens)),
        Ok(MessageStreamEvent::MessageStop),
    ];
    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    match evs.last().unwrap() {
        SamplingEvent::Completed { response, .. } => {
            assert_eq!(response.stop_reason, Some(StopReason::Length));
            assert_eq!(response.tool_calls().len(), 1);
            assert_eq!(response.tool_calls()[0].arguments.as_ref(), "");
        }
        other => panic!("expected Completed(Length), got {other:?}"),
    }
}

/// Pins the model_context_window_exceeded decision: it maps to the Length stop class and COMPLETES with the partial preserved.
/// Fail-vs-salvage belongs to `drive_l2`, not this transform.
#[tokio::test]
async fn model_context_window_exceeded_completes_with_length_stop() {
    let events: Vec<Result<MessageStreamEvent, SamplingError>> = vec![
        Ok(message_start()),
        Ok(text_block_start(0)),
        Ok(text_delta(0, "truncated answ")),
        Ok(block_stop(0)),
        Ok(message_delta_with_stop(
            messages::StopReason::ModelContextWindowExceeded,
        )),
        Ok(MessageStreamEvent::MessageStop),
    ];
    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    match evs.last().unwrap() {
        SamplingEvent::Completed { response, .. } => {
            assert_eq!(response.stop_reason, Some(StopReason::Length));
            assert_eq!(
                response.assistant_text(),
                "truncated answ",
                "partial content must be preserved"
            );
        }
        other => panic!("expected Completed(Length), got {other:?}"),
    }
}

/// Pins the override: completed tool_use blocks beat a terminal Refusal, so the agent loop still resolves the calls.
#[tokio::test]
async fn refusal_after_tool_use_blocks_keeps_tool_calls_stop_reason() {
    let tool_start = MessageStreamEvent::ContentBlockStart {
        index: 0,
        content_block: ContentBlock::ToolUse {
            id: "call_refused".into(),
            name: "do_thing".into(),
            input: serde_json::json!({}),
            cache_control: None,
        },
    };
    let arg_delta = MessageStreamEvent::ContentBlockDelta {
        index: 0,
        delta: StreamDelta::InputJsonDelta {
            partial_json: "{}".into(),
        },
    };
    let events: Vec<Result<MessageStreamEvent, SamplingError>> = vec![
        Ok(message_start()),
        Ok(tool_start),
        Ok(arg_delta),
        Ok(block_stop(0)),
        Ok(message_delta_with_stop(messages::StopReason::Refusal)),
        Ok(MessageStreamEvent::MessageStop),
    ];
    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    match evs.last().unwrap() {
        SamplingEvent::Completed { response, .. } => {
            assert_eq!(response.tool_calls().len(), 1);
            assert_eq!(
                response.stop_reason,
                Some(StopReason::ToolCalls),
                "tool_use blocks must win over the refusal stop_reason"
            );
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/tests/stream_invariants.rs :: error_event_propagates_typed (re-expressed; grok surfaces the in-stream `error` event as a `Failed` terminal — already satisfied pre-MW-3, so this is the pinning re-expression, HI-C5-006)
#[tokio::test]
async fn server_error_event_yields_failed_500() {
    let err_event = MessageStreamEvent::Error {
        error: StreamError {
            r#type: "overloaded_error".into(),
            message: "rate limit hit".into(),
        },
    };
    let raw = stream::iter(vec![Ok(message_start()), Ok(err_event)]).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    match evs.last().unwrap() {
        SamplingEvent::Failed { error, .. } => {
            assert_eq!(error.kind, crate::events::SamplingErrorKind::Api);
            assert_eq!(error.status_code, Some(500));
            assert!(error.message.contains("overloaded_error"));
            // Messages error events have no code slot; a code appearing here would make typed events eligible for a destructive image strip
            assert_eq!(error.error_code, None);
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[tokio::test]
async fn mid_stream_transport_error_yields_failed() {
    let raw = stream::iter(vec![
        Ok(message_start()),
        Err(SamplingError::EventStreamError("conn reset".into())),
    ])
    .boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;
    assert!(
        evs.iter()
            .any(|e| matches!(e, SamplingEvent::Failed { .. }))
    );
    assert!(
        !evs.iter()
            .any(|e| matches!(e, SamplingEvent::Completed { .. }))
    );
}

#[tokio::test(start_paused = true)]
async fn idle_timeout_when_stream_stalls() {
    let raw = stream::iter(vec![Ok(message_start())])
        .chain(stream::pending())
        .boxed();
    let evs = collect(stream_messages(
        raw,
        None,
        rid(),
        Duration::from_millis(100),
    ))
    .await;

    match evs.last().unwrap() {
        SamplingEvent::Failed { error, .. } => {
            assert_eq!(error.kind, crate::events::SamplingErrorKind::IdleTimeout);
        }
        other => panic!("expected Failed(IdleTimeout), got {other:?}"),
    }
}

#[tokio::test]
async fn model_metadata_yielded_after_stream_started() {
    let raw = stream::iter(vec![Ok(MessageStreamEvent::MessageStop)]).boxed();
    let metadata = ResponseModelMetadata {
        context_window: Some(200_000),
        ..Default::default()
    };
    let evs = collect(stream_messages(
        raw,
        Some(metadata),
        rid(),
        Duration::from_secs(60),
    ))
    .await;

    assert!(matches!(evs[0], SamplingEvent::StreamStarted { .. }));
    assert!(matches!(evs[1], SamplingEvent::ModelMetadata { .. }));
}

#[test]
fn meaningful_content_classifier_treats_ping_as_keepalive() {
    assert!(!messages_event_has_meaningful_content(
        &MessageStreamEvent::Ping
    ));
    assert!(messages_event_has_meaningful_content(
        &MessageStreamEvent::MessageStop
    ));
}

// ── Token usage: Anthropic Messages API cache-bucket accounting ────────────

fn message_start_with_cache(
    input: u32,
    cache_read: u32,
    cache_creation: u32,
) -> MessageStreamEvent {
    MessageStreamEvent::MessageStart {
        message: MessagesResponse {
            id: "msg_cache".into(),
            r#type: "message".into(),
            role: "assistant".into(),
            content: vec![],
            model: "messages-compatible-model".into(),
            stop_reason: None,
            usage: MessagesUsage {
                input_tokens: input,
                output_tokens: 0,
                cache_creation_input_tokens: cache_creation,
                cache_read_input_tokens: cache_read,
                output_tokens_details: None,
                cache_creation: None,
            },
        },
    }
}

fn message_delta_with_cache(
    output: u32,
    input: Option<u32>,
    cache_read: Option<u32>,
    cache_creation: Option<u32>,
) -> MessageStreamEvent {
    MessageStreamEvent::MessageDelta {
        delta: MessageDeltaBody {
            stop_reason: Some(messages::StopReason::EndTurn),
            stop_sequence: None,
            stop_details: None,
        },
        usage: MessageDeltaUsage {
            output_tokens: output,
            input_tokens: input,
            cache_read_input_tokens: cache_read,
            cache_creation_input_tokens: cache_creation,
            output_tokens_details: None,
        },
    }
}

/// Drive a minimal stream with the supplied usage events and pluck the `TokenUsage` out of the terminal `Completed` event.
async fn usage_from_stream(events: Vec<MessageStreamEvent>) -> TokenUsage {
    let raw = stream::iter(
        events
            .into_iter()
            .map(Ok::<_, SamplingError>)
            .collect::<Vec<_>>(),
    )
    .boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;
    match evs.last().expect("at least one event") {
        SamplingEvent::Completed { response, .. } => response
            .usage
            .clone()
            .expect("usage should be emitted when prompt or output tokens > 0"),
        other => panic!("expected Completed, got {other:?}"),
    }
}

#[tokio::test]
async fn prompt_tokens_sums_all_three_anthropic_buckets() {
    // cached_prompt_tokens counts cache_read only (writes aren't a hit)
    let usage = usage_from_stream(vec![
        message_start_with_cache(100, 5000, 200),
        text_block_start(0),
        text_delta(0, "ok"),
        block_stop(0),
        message_delta_with_cache(7, None, None, None),
        MessageStreamEvent::MessageStop,
    ])
    .await;

    assert_eq!(usage.prompt_tokens, 100 + 5000 + 200);
    assert_eq!(usage.cached_prompt_tokens, 5000);
    assert_eq!(usage.cache_creation_prompt_tokens, 200);
    assert_eq!(usage.completion_tokens, 7);
    assert_eq!(usage.total_tokens, 100 + 5000 + 200 + 7);
}

#[tokio::test]
async fn message_delta_cache_fields_override_message_start() {
    // Providers can report zero cache at message_start and emit the real values on the final delta; honor the delta when present
    let usage = usage_from_stream(vec![
        message_start_with_cache(10, 0, 0),
        message_delta_with_cache(4, Some(10), Some(900), Some(50)),
        MessageStreamEvent::MessageStop,
    ])
    .await;

    assert_eq!(usage.prompt_tokens, 10 + 900 + 50);
    assert_eq!(usage.cached_prompt_tokens, 900);
    assert_eq!(usage.cache_creation_prompt_tokens, 50);
    assert_eq!(usage.completion_tokens, 4);
}

#[tokio::test]
async fn pure_cache_hit_with_zero_uncached_still_emits_usage() {
    // 100% cache hit: Anthropic Messages API reports input_tokens=0 with cache_read>0.
    // Usage must still be emitted so callers see the cached cost
    let usage = usage_from_stream(vec![
        message_start_with_cache(0, 2500, 0),
        message_delta_with_cache(1, None, None, None),
        MessageStreamEvent::MessageStop,
    ])
    .await;

    assert_eq!(usage.prompt_tokens, 2500);
    assert_eq!(usage.cached_prompt_tokens, 2500);
    assert_eq!(usage.total_tokens, 2501);
}

// ── MW-2 R4 — thinking tokens → TokenUsage.reasoning_tokens ───────────────

fn message_start_with_thinking(input: u32, thinking_tokens: u32) -> MessageStreamEvent {
    MessageStreamEvent::MessageStart {
        message: MessagesResponse {
            id: "msg_r4".into(),
            r#type: "message".into(),
            role: "assistant".into(),
            content: vec![],
            model: "messages-compatible-model".into(),
            stop_reason: None,
            usage: MessagesUsage {
                input_tokens: input,
                output_tokens: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                output_tokens_details: Some(OutputTokensDetails { thinking_tokens }),
                cache_creation: None,
            },
        },
    }
}

fn message_delta_with_thinking(output: u32, thinking_tokens: u32) -> MessageStreamEvent {
    MessageStreamEvent::MessageDelta {
        delta: MessageDeltaBody {
            stop_reason: Some(messages::StopReason::EndTurn),
            stop_sequence: None,
            stop_details: None,
        },
        usage: MessageDeltaUsage {
            output_tokens: output,
            input_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            output_tokens_details: Some(OutputTokensDetails { thinking_tokens }),
        },
    }
}

/// Fresh-written: R4 mapping (spec R4 pre-resolved plumbing). The responses wire already
/// maps its `output_tokens_details.reasoning_tokens` into `TokenUsage.reasoning_tokens`
/// (stream/responses.rs:635); the messages route must map `thinking_tokens` the same way
/// instead of hardcoding 0. Field per wirejig/refs/anthropic@d3d5028 messages.ts:2423.
#[tokio::test]
async fn thinking_tokens_from_message_start_map_to_reasoning_tokens() {
    let usage = usage_from_stream(vec![
        message_start_with_thinking(10, 25),
        text_block_start(0),
        text_delta(0, "ok"),
        block_stop(0),
        // No detail object on the delta: the message_start value is preserved,
        // matching the existing cache-bucket preserve pattern.
        message_delta_with_cache(7, None, None, None),
        MessageStreamEvent::MessageStop,
    ])
    .await;

    assert_eq!(usage.reasoning_tokens, 25);
    assert_eq!(usage.completion_tokens, 7);
}

/// Fresh-written: a `message_delta` carrying its own `output_tokens_details`
/// overrides the `message_start` value (same preserve/override pattern as the
/// existing cache buckets).
#[tokio::test]
async fn thinking_tokens_delta_overrides_message_start() {
    let usage = usage_from_stream(vec![
        message_start_with_thinking(10, 25),
        text_block_start(0),
        text_delta(0, "ok"),
        block_stop(0),
        message_delta_with_thinking(7, 40),
        MessageStreamEvent::MessageStop,
    ])
    .await;

    assert_eq!(usage.reasoning_tokens, 40);
    assert_eq!(usage.completion_tokens, 7);
}

/// Fresh-written: when neither event carries the detail object, reasoning tokens
/// stay 0 (the pre-R4 value) — absence must not fail the stream nor fabricate.
#[tokio::test]
async fn missing_thinking_details_keep_reasoning_tokens_zero() {
    let usage = usage_from_stream(vec![
        message_start_with_cache(10, 0, 0),
        text_block_start(0),
        text_delta(0, "ok"),
        block_stop(0),
        message_delta_with_cache(7, None, None, None),
        MessageStreamEvent::MessageStop,
    ])
    .await;

    assert_eq!(usage.reasoning_tokens, 0);
}

// ── MW-3 R2 — stream invariants (transform level; xli suite re-expression) ──

fn thinking_block_start(index: u32) -> MessageStreamEvent {
    MessageStreamEvent::ContentBlockStart {
        index,
        content_block: ContentBlock::Thinking {
            thinking: String::new(),
            signature: String::new(),
        },
    }
}

fn thinking_delta(index: u32, thinking: &str) -> MessageStreamEvent {
    MessageStreamEvent::ContentBlockDelta {
        index,
        delta: StreamDelta::ThinkingDelta {
            thinking: thinking.into(),
        },
    }
}

fn signature_delta(index: u32, signature: &str) -> MessageStreamEvent {
    MessageStreamEvent::ContentBlockDelta {
        index,
        delta: StreamDelta::SignatureDelta {
            signature: signature.into(),
        },
    }
}

fn tool_use_start(index: u32, id: &str, name: &str) -> MessageStreamEvent {
    MessageStreamEvent::ContentBlockStart {
        index,
        content_block: ContentBlock::ToolUse {
            id: id.into(),
            name: name.into(),
            input: serde_json::json!({}),
            cache_control: None,
        },
    }
}

fn input_delta(index: u32, partial_json: &str) -> MessageStreamEvent {
    MessageStreamEvent::ContentBlockDelta {
        index,
        delta: StreamDelta::InputJsonDelta {
            partial_json: partial_json.into(),
        },
    }
}

/// Assert the terminal is a `Failed` stream error of `error_type` whose
/// message contains `phrase`; returns the error info for extra assertions.
fn assert_failed_stream_error(evs: &[SamplingEvent], error_type: &str, phrase: &str) {
    match evs.last().unwrap() {
        SamplingEvent::Failed { error, .. } => {
            assert_eq!(
                error.kind,
                crate::events::SamplingErrorKind::Api,
                "stream errors surface as the Api kind, got {}",
                error.message
            );
            assert!(
                error.is_retryable,
                "stream errors keep the actor retry path"
            );
            assert!(
                error
                    .message
                    .contains(&format!("stream error ({error_type})")),
                "expected error_type {error_type}, got {}",
                error.message
            );
            assert!(
                error.message.contains(phrase),
                "expected {phrase:?} in {}",
                error.message
            );
        }
        other => panic!("expected Failed({error_type}), got {other:?}"),
    }
}

/// D3 phantom-open (spec G3 ruling, binding): an unknown-kind
/// `content_block_start` opens a swallowed phantom block — the index is
/// recorded, its deltas are swallowed, and its stop is a no-op, so a
/// forward-compat stream COMPLETES instead of hitting the fatal
/// unopened-index classes. Green-from-start pin: this held pre-guard (the
/// transform ignored unknown starts) and must hold with the guard in place.
///
/// Provenance: hyper-grok-build@45e984f3 — packages/ai/xai-grok-sampler/src/client.rs :: decode_messages_sse_frame_skips_unknown_content_block_kinds (re-expressed, transform half; the serde half mapping the unknown kind to the phantom variant is the sampling-types R1 test unknown_content_block_kind_opens_phantom_block; grok deviates from HY's Ping mapping per the D3 ruling)
#[tokio::test]
async fn unknown_kind_phantom_block_stream_completes() {
    let phantom_start = MessageStreamEvent::ContentBlockStart {
        index: 1,
        content_block: ContentBlock::Unknown {
            kind: "brand_new_block".into(),
        },
    };
    let events: Vec<Result<MessageStreamEvent, SamplingError>> = vec![
        Ok(message_start()),
        Ok(text_block_start(0)),
        Ok(text_delta(0, "real")),
        Ok(block_stop(0)),
        Ok(phantom_start),
        Ok(text_delta(1, "phantom")),
        Ok(block_stop(1)),
        Ok(message_delta_with_stop(messages::StopReason::EndTurn)),
        Ok(MessageStreamEvent::MessageStop),
    ];
    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    match evs.last().unwrap() {
        SamplingEvent::Completed { response, .. } => {
            assert_eq!(
                response.assistant().map(|a| a.content.as_ref()),
                Some("real"),
                "phantom deltas are swallowed; only the text block survives"
            );
        }
        other => panic!("expected Completed (D3 phantom-open), got {other:?}"),
    }
}

/// A delta on an index that never received a start is true wire corruption:
/// FATAL (xli `DeltaForUnopenedIndex`). RED pre-guard: the transform silently
/// dropped such deltas and completed.
///
/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/tests/stream_invariants.rs :: accumulator_delta_for_unopened_index_is_fatal (re-expressed on the grok transform, HI-C5-002)
#[tokio::test]
async fn never_started_index_delta_fails_unopened() {
    let events: Vec<Result<MessageStreamEvent, SamplingError>> =
        vec![Ok(message_start()), Ok(text_delta(0, "hi"))];
    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    assert_failed_stream_error(
        &evs,
        "unopened_index",
        "content_block_delta for unopened index 0",
    );
    assert!(
        !evs.iter()
            .any(|e| matches!(e, SamplingEvent::Completed { .. }))
    );
}

/// A stop on an index that never received a start is true wire corruption:
/// FATAL (xli `StopForUnknownIndex`). RED pre-guard: the transform silently
/// ignored such stops and completed.
///
/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/tests/stream_invariants.rs :: accumulator_stop_for_unknown_index_is_fatal (re-expressed on the grok transform, HI-C5-001)
#[tokio::test]
async fn never_started_index_stop_fails_unopened() {
    let events: Vec<Result<MessageStreamEvent, SamplingError>> =
        vec![Ok(message_start()), Ok(block_stop(0))];
    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    assert_failed_stream_error(
        &evs,
        "unopened_index",
        "content_block_stop for unknown index 0",
    );
    assert!(
        !evs.iter()
            .any(|e| matches!(e, SamplingEvent::Completed { .. }))
    );
}

/// A `content_block_start` for an index that already holds an open block is
/// the NEW `DuplicateToolCallIndex` recoverable violation (R7 row 18; xli
/// overwrites silently, grok warns and keeps the FIRST block).
///
/// Fresh-written: spec R2 note + R7 row-18 duplicate-index subcase (no xli home)
#[tokio::test]
async fn duplicate_open_index_warns_and_keeps_first_block() {
    let events: Vec<Result<MessageStreamEvent, SamplingError>> = vec![
        Ok(message_start()),
        Ok(tool_use_start(0, "call_first", "first_tool")),
        Ok(tool_use_start(0, "call_second", "second_tool")),
        Ok(input_delta(0, r#"{"k":1}"#)),
        Ok(block_stop(0)),
        Ok(message_delta_with_stop(messages::StopReason::ToolUse)),
        Ok(MessageStreamEvent::MessageStop),
    ];
    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    let id_deltas: Vec<Option<String>> = evs
        .iter()
        .filter_map(|e| match e {
            SamplingEvent::ToolCallDelta { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        id_deltas.iter().filter(|id| id.is_some()).count(),
        1,
        "the duplicate start must not re-emit a tool call id"
    );
    match evs.last().unwrap() {
        SamplingEvent::Completed { response, .. } => {
            let calls = response.tool_calls();
            assert_eq!(calls.len(), 1, "the first block wins; no second call");
            assert_eq!(calls[0].id.as_ref(), "call_first");
            assert_eq!(calls[0].name, "first_tool");
            assert_eq!(calls[0].arguments.as_ref(), r#"{"k":1}"#);
        }
        other => panic!("expected Completed (recoverable duplicate), got {other:?}"),
    }
}

/// Usage counters are monotonic across `message_start` → `message_delta`; a
/// regressed counter is skipped (previous kept) — recoverable, never fatal
/// (xli `UsageMonotonicityViolation`, R-risk-4 live audit pending at A6).
/// RED pre-guard: the transform overwrote unconditionally.
///
/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/tests/stream_invariants.rs :: accumulator_usage_non_monotonic_clamps (re-expressed on the grok transform, HI-C1-012; grok skips the regressed FIELD, xli drops the whole snapshot — documented divergence)
#[tokio::test]
async fn usage_non_monotonic_deltas_keep_previous_values() {
    let usage = usage_from_stream(vec![
        message_start_with_cache(100, 0, 0),
        text_block_start(0),
        text_delta(0, "ok"),
        block_stop(0),
        // input regresses 100 -> 90 (skipped); output 0 -> 30 (applied)
        message_delta_with_cache(30, Some(90), None, None),
        // output regresses 30 -> 20 (skipped); input 100 -> 110 (applied)
        message_delta_with_cache(20, Some(110), None, None),
        MessageStreamEvent::MessageStop,
    ])
    .await;

    assert_eq!(
        usage.completion_tokens, 30,
        "regressed output must be skipped"
    );
    assert_eq!(usage.prompt_tokens, 110, "monotonic input must still apply");
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/tests/stream_invariants.rs :: ping_event_is_noop (re-expressed on the grok transform, HI-C5-005: pings are liveness-only and never touch accumulation)
#[tokio::test]
async fn ping_events_are_liveness_noop() {
    let events: Vec<Result<MessageStreamEvent, SamplingError>> = vec![
        Ok(message_start()),
        Ok(MessageStreamEvent::Ping),
        Ok(text_block_start(0)),
        Ok(MessageStreamEvent::Ping),
        Ok(text_delta(0, "hi")),
        Ok(MessageStreamEvent::Ping),
        Ok(block_stop(0)),
        Ok(MessageStreamEvent::Ping),
        Ok(message_delta_with_stop(messages::StopReason::EndTurn)),
        Ok(MessageStreamEvent::Ping),
        Ok(MessageStreamEvent::MessageStop),
    ];
    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    match evs.last().unwrap() {
        SamplingEvent::Completed { response, .. } => {
            assert_eq!(response.assistant().map(|a| a.content.as_ref()), Some("hi"));
        }
        other => panic!("expected Completed (pings are no-ops), got {other:?}"),
    }
}

/// A `signature_delta` that arrives before any thinking text is
/// recoverable (xli `SignatureBeforeThinking`): the stream continues and the
/// signature still lands. (xli asserts the log line; grok asserts the
/// non-fatal continuation — the is_fatal=false classification is pinned in
/// the guard module's unit suite.)
///
/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/tests/stream_invariants.rs :: signature_before_thinking_warns (re-expressed on the grok transform, HI-C5-004)
#[tokio::test]
async fn signature_before_thinking_keeps_stream_alive() {
    let events: Vec<Result<MessageStreamEvent, SamplingError>> = vec![
        Ok(message_start()),
        Ok(thinking_block_start(0)),
        Ok(signature_delta(0, "sig-early")),
        Ok(thinking_delta(0, "why")),
        Ok(block_stop(0)),
        Ok(message_delta_with_stop(messages::StopReason::EndTurn)),
        Ok(MessageStreamEvent::MessageStop),
    ];
    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    match evs.last().unwrap() {
        SamplingEvent::Completed { response, .. } => {
            let r = response
                .reasoning_items()
                .next()
                .expect("reasoning preserved");
            assert_eq!(r.encrypted_content.as_deref(), Some("sig-early"));
        }
        other => panic!("expected Completed (recoverable), got {other:?}"),
    }
}

/// A second `signature_delta` for the same block is recoverable (xli
/// `DuplicateSignatureDelta`). grok accumulation divergence from xli (documented):
/// the transform is LAST-WINS (xli concatenates "sig1sig2").
///
/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/tests/stream_invariants.rs :: duplicate_signature_delta_warns_and_concats (re-expressed on the grok transform, HI-C5-005-adjacent; grok last-wins, xli concat — divergence documented in the guard module)
#[tokio::test]
async fn duplicate_signature_delta_keeps_last_signature() {
    let events: Vec<Result<MessageStreamEvent, SamplingError>> = vec![
        Ok(message_start()),
        Ok(thinking_block_start(0)),
        Ok(thinking_delta(0, "why")),
        Ok(signature_delta(0, "sig-1")),
        Ok(signature_delta(0, "sig-2")),
        Ok(block_stop(0)),
        Ok(message_delta_with_stop(messages::StopReason::EndTurn)),
        Ok(MessageStreamEvent::MessageStop),
    ];
    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    let sigs: Vec<&str> = evs
        .iter()
        .filter_map(|e| match e {
            SamplingEvent::ReasoningCompleted { signature, .. } => Some(signature.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(sigs, vec!["sig-2"], "grok is last-wins (xli concats)");
    match evs.last().unwrap() {
        SamplingEvent::Completed { response, .. } => {
            let r = response
                .reasoning_items()
                .next()
                .expect("reasoning preserved");
            assert_eq!(r.encrypted_content.as_deref(), Some("sig-2"));
        }
        other => panic!("expected Completed (recoverable), got {other:?}"),
    }
}

// ── MW-3 R3 — truncation / unclosed-block failures ─────────────────────────

/// R3: the stream ends (raw `None`) without a `message_stop` →
/// `Failed(stream_truncated)`, the "without done" shape. RED pre-MW-3:
/// silent `Completed` (spec G6: NEW hard failure, live-verified at A6).
///
/// Provenance: hyper-grok-build@45e984f3 — packages/ai/xai-grok-sampler/src/pi_messages.rs :: stream_fails_truncated_and_unended_blocks (re-expressed, "without done" half, audit row 15)
#[tokio::test]
async fn stream_ends_without_done_fails_truncated() {
    let events: Vec<Result<MessageStreamEvent, SamplingError>> = vec![
        Ok(message_start()),
        Ok(text_block_start(0)),
        Ok(text_delta(0, "hi")),
        Ok(block_stop(0)),
        Ok(message_delta_with_stop(messages::StopReason::EndTurn)),
        // no MessageStop: the raw stream ends
    ];
    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    assert_failed_stream_error(&evs, "stream_truncated", "without done");
}

/// R3: `message_stop` while any block is still open →
/// `Failed(unclosed_blocks)`, the "before all blocks ended" shape
/// (xli eq-11/eq-19 shape). RED pre-MW-3: silent `Completed`.
///
/// Provenance: hyper-grok-build@45e984f3 — packages/ai/xai-grok-sampler/src/pi_messages.rs :: stream_fails_truncated_and_unended_blocks (re-expressed, "before all blocks ended" half, audit row 15; fixture shape xli codex-api/tests/fixtures/stream_equiv/eq-11-tool-no-stop)
#[tokio::test]
async fn message_stop_with_open_blocks_fails_unclosed() {
    let events: Vec<Result<MessageStreamEvent, SamplingError>> = vec![
        Ok(message_start()),
        Ok(text_block_start(0)),
        Ok(text_delta(0, "hi")),
        // block 0 never stops
        Ok(message_delta_with_stop(messages::StopReason::EndTurn)),
        Ok(MessageStreamEvent::MessageStop),
    ];
    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    assert_failed_stream_error(&evs, "unclosed_blocks", "before all blocks ended");
}

/// R3: a tool call whose accumulated arguments are non-empty and not valid
/// JSON at a completed (non-Length) terminal → `Failed(invalid_tool_args)`
/// (the eq-10 `tool_args_state` class). RED pre-MW-3: `Completed` with the
/// garbage args. Scoped out: `Length` terminals keep the 09-09 proven
/// salvage surface (`max_tokens_with_tool_use_keeps_length_stop`).
///
/// Provenance: hyper-grok-build@45e984f3 — packages/ai/xai-grok-sampler/src/pi_messages.rs :: stream_fails_truncated_and_unended_blocks (re-expressed, tool-args class; fixture shape xli codex-api/tests/fixtures/stream_equiv/eq-10-tool-truncated-invalid-json)
#[tokio::test]
async fn tool_args_invalid_json_fails() {
    let events: Vec<Result<MessageStreamEvent, SamplingError>> = vec![
        Ok(message_start()),
        Ok(tool_use_start(0, "call_x", "do_thing")),
        Ok(input_delta(0, r#"{"a":"#)),
        Ok(block_stop(0)),
        Ok(message_delta_with_stop(messages::StopReason::ToolUse)),
        Ok(MessageStreamEvent::MessageStop),
    ];
    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    assert_failed_stream_error(&evs, "invalid_tool_args", "not valid JSON");
}

/// G5: the proven zero-args `""` finalization is EXCLUDED from the
/// invalid_tool_args class — an empty-arguments tool call at a completed
/// terminal still completes. Green-from-start pin.
///
/// Fresh-written: spec G5 ruling (`""` kept; xref the 09-09 pin max_tokens_tool_use_without_arg_deltas_collects_empty_arguments, which covers the Length terminal)
#[tokio::test]
async fn zero_arg_tool_call_empty_string_is_not_invalid() {
    let events: Vec<Result<MessageStreamEvent, SamplingError>> = vec![
        Ok(message_start()),
        Ok(tool_use_start(0, "call_e", "do_thing")),
        Ok(block_stop(0)),
        Ok(message_delta_with_stop(messages::StopReason::EndTurn)),
        Ok(MessageStreamEvent::MessageStop),
    ];
    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    match evs.last().unwrap() {
        SamplingEvent::Completed { response, .. } => {
            let calls = response.tool_calls();
            assert_eq!(calls.len(), 1);
            assert_eq!(
                calls[0].arguments.as_ref(),
                "",
                "the proven zero-args shape must not trip invalid_tool_args"
            );
        }
        other => panic!("expected Completed (empty args excluded), got {other:?}"),
    }
}

/// R6 (row 16): tool-call ORDER is pinned to wire arrival order on the
/// Anthropic messages wire — both the streaming `ToolCallDelta` id
/// emissions and the final `tool_calls` list. The transform does NOT
/// sort by content index: HY's index sort is a Pi-wire artifact (Pi
/// blocks may open out of order); on the Anthropic wire index order IS
/// arrival order, so arrival order is the pinned behavior
/// (DECISION-1: keep wire-index/arrival order, reject the Pi-wire
/// specific sort).
///
/// Zero-args half (G5): a `tool_use` block with no input deltas
/// finalizes with the proven `""` arguments at a completed terminal.
/// (The Length-terminal half is pinned by
/// `max_tokens_tool_use_without_arg_deltas_collects_empty_arguments`.)
/// Green-from-start pin — re-expresses no HY/xli source.
///
/// Provenance: hyper-grok-build@45e984f3 — packages/ai/xai-grok-sampler/src/pi_messages.rs:1232 :: stream_emits_toolcall_start_for_zero_args_and_sorts_tool_calls (pinning; DECISION-1 — the HY index-sort assertion is deliberately not adopted: Pi-wire-specific, out-of-order block opens do not occur on the Anthropic wire)
#[tokio::test]
async fn tool_call_order_follows_wire_arrival_and_zero_args_complete() {
    let events: Vec<Result<MessageStreamEvent, SamplingError>> = vec![
        Ok(message_start()),
        // Tool A: two argument deltas (split so neither raw literal ends
        // in a quote, which would merge with the `r#"` closer)
        Ok(tool_use_start(0, "call_a", "tool_a")),
        Ok(input_delta(0, r#"{"a":"1"#)),
        Ok(input_delta(0, r#""}"#)),
        Ok(block_stop(0)),
        // Tool B: zero arguments (no input deltas)
        Ok(tool_use_start(1, "call_b", "tool_b")),
        Ok(block_stop(1)),
        Ok(message_delta_with_stop(messages::StopReason::ToolUse)),
        Ok(MessageStreamEvent::MessageStop),
    ];
    let raw = stream::iter(events).boxed();
    let evs = collect(stream_messages(raw, None, rid(), Duration::from_secs(60))).await;

    // Streaming side: ToolCallDelta id emissions in arrival order.
    let id_emissions: Vec<&str> = evs
        .iter()
        .filter_map(|ev| match ev {
            SamplingEvent::ToolCallDelta { id: Some(id), .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        id_emissions,
        vec!["call_a", "call_b"],
        "ToolCallDelta id emissions must follow wire arrival order"
    );

    // Terminal side: final tool_calls in the same arrival order (no
    // index sort), and the zero-args tool keeps the proven `""`.
    match evs.last().unwrap() {
        SamplingEvent::Completed { response, .. } => {
            let calls = response.tool_calls();
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[0].id.as_ref(), "call_a");
            assert_eq!(calls[0].name, "tool_a");
            assert_eq!(calls[0].arguments.as_ref(), r#"{"a":"1"}"#);
            assert_eq!(calls[1].id.as_ref(), "call_b");
            assert_eq!(calls[1].name, "tool_b");
            assert_eq!(
                calls[1].arguments.as_ref(),
                "",
                "zero-args tool at a completed terminal keeps the proven empty shape (G5)"
            );
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}
