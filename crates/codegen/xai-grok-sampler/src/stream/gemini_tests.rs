//! Tests for the Gemini `streamGenerateContent` Layer-2 transform.
//! `#[path = "gemini_tests.rs"] mod tests;` in gemini.rs wires them in.

use super::*;
use futures_util::stream;
use std::pin::pin;
use xai_grok_sampling_types::gemini::{
    Candidate, Content, FunctionCall, Part, PromptFeedback, UsageMetadata,
};

fn rid() -> RequestId {
    RequestId::from("gemini-test")
}

async fn collect(s: impl Stream<Item = SamplingEvent>) -> Vec<SamplingEvent> {
    let mut out = Vec::new();
    let mut s = pin!(s);
    while let Some(ev) = s.next().await {
        out.push(ev);
    }
    out
}

async fn run(chunks: Vec<GenerateContentResponse>) -> Vec<SamplingEvent> {
    let raw = stream::iter(chunks.into_iter().map(Ok::<_, SamplingError>)).boxed();
    collect(stream_generate_content(
        raw,
        None,
        rid(),
        Duration::from_secs(60),
    ))
    .await
}

fn chunk(parts: Vec<Part>, finish: Option<FinishReason>) -> GenerateContentResponse {
    GenerateContentResponse {
        candidates: vec![Candidate {
            content: Some(Content::model(parts)),
            finish_reason: finish,
            finish_message: None,
            index: Some(0),
        }],
        prompt_feedback: None,
        usage_metadata: None,
        model_version: Some("gemini-2.5-pro-preview-06-05".into()),
        response_id: Some("resp_1".into()),
    }
}

fn usage(prompt: u32, candidates: u32, thoughts: u32, cached: u32) -> UsageMetadata {
    UsageMetadata {
        prompt_token_count: prompt,
        candidates_token_count: candidates,
        total_token_count: prompt + candidates + thoughts,
        thoughts_token_count: thoughts,
        cached_content_token_count: cached,
        tool_use_prompt_token_count: 0,
    }
}

fn call(name: &str, args: serde_json::Value) -> Part {
    Part::function_call(FunctionCall {
        id: None,
        name: name.into(),
        args: Some(args),
    })
}

fn completed(events: &[SamplingEvent]) -> &ConversationResponse {
    match events.last() {
        Some(SamplingEvent::Completed { response, .. }) => response,
        other => panic!("expected Completed terminal, got {other:?}"),
    }
}

fn failed_type(events: &[SamplingEvent]) -> String {
    match events.last() {
        Some(SamplingEvent::Failed { error, .. }) => error.message.clone(),
        other => panic!("expected Failed terminal, got {other:?}"),
    }
}

fn reasoning_of(resp: &ConversationResponse) -> Option<&rs::ReasoningItem> {
    resp.items.iter().find_map(|i| match i {
        ConversationItem::Reasoning(r) => Some(&r.item),
        _ => None,
    })
}

#[tokio::test]
async fn text_stream_concatenates_deltas_and_completes_with_stop() {
    let mut last = chunk(vec![Part::text(" world")], Some(FinishReason::Stop));
    last.usage_metadata = Some(usage(10, 5, 0, 0));
    let events = run(vec![chunk(vec![Part::text("Hello")], None), last]).await;

    assert!(matches!(events[0], SamplingEvent::StreamStarted { .. }));
    assert!(matches!(events[1], SamplingEvent::FirstToken { .. }));
    let texts: Vec<&str> = events
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
    assert_eq!(texts, ["Hello", " world"]);

    let resp = completed(&events);
    assert_eq!(resp.stop_reason, Some(StopReason::Stop));
    assert_eq!(resp.raw_stop_reason.as_deref(), Some("STOP"));
    assert_eq!(resp.message_id.as_deref(), Some("resp_1"));
    assert_eq!(resp.message_chunks_emitted, 2);
    let a = resp.assistant().expect("assistant item");
    assert_eq!(&*a.content, "Hello world");
    assert_eq!(a.model_id.as_deref(), Some("gemini-2.5-pro-preview-06-05"));
    assert!(reasoning_of(resp).is_none(), "no thoughts, no signature ⇒ no reasoning item");
    let u = resp.usage.as_ref().unwrap();
    assert_eq!((u.prompt_tokens, u.completion_tokens, u.total_tokens), (10, 5, 15));
}

#[tokio::test]
async fn thoughts_stream_on_reasoning_channel_and_signature_lands_on_reasoning_item() {
    let events = run(vec![
        chunk(vec![Part::thought("Let me think")], None),
        chunk(vec![Part::thought(" harder.")], None),
        chunk(
            vec![Part::text("Answer.").with_thought_signature("SIG_TEXT")],
            Some(FinishReason::Stop),
        ),
    ])
    .await;

    let reasoning: Vec<&str> = events
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
    assert_eq!(reasoning, ["Let me think", " harder."]);
    assert!(
        events.iter().any(|e| matches!(
            e,
            SamplingEvent::ReasoningCompleted { signature, .. } if signature == "SIG_TEXT"
        )),
        "signature surfaced as ReasoningCompleted"
    );

    let resp = completed(&events);
    assert_eq!(resp.items.len(), 2);
    assert!(matches!(resp.items[0], ConversationItem::Reasoning(_)));
    assert!(matches!(resp.items[1], ConversationItem::Assistant(_)));
    let r = reasoning_of(resp).unwrap();
    assert_eq!(r.id.as_deref(), Some(""));
    assert_eq!(r.encrypted_content.as_deref(), Some("SIG_TEXT"));
    let rs::SummaryPart::SummaryText(t) = &r.summary[0];
    assert_eq!(t.text, "Let me think harder.");
    assert_eq!(resp.message_chunks_emitted, 1, "thoughts are not message chunks");
}

#[tokio::test]
async fn function_call_mints_local_id_and_carries_first_call_signature() {
    let events = run(vec![
        chunk(vec![Part::thought("plan")], None),
        chunk(
            vec![
                call("read_file", serde_json::json!({"path": "a.rs"}))
                    .with_thought_signature("SIG_CALL"),
                call("read_file", serde_json::json!({"path": "b.rs"})),
            ],
            Some(FinishReason::Stop),
        ),
    ])
    .await;

    let deltas: Vec<(u32, Option<String>, Option<String>)> = events
        .iter()
        .filter_map(|e| match e {
            SamplingEvent::ToolCallDelta {
                tool_index,
                id,
                name,
                arguments_delta,
                ..
            } => {
                assert_eq!(name.as_deref(), Some("read_file"));
                Some((*tool_index, id.clone(), arguments_delta.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(deltas.len(), 2);
    assert_eq!(deltas[0].0, 0);
    assert_eq!(deltas[1].0, 1);
    assert!(deltas[0].1.as_deref().unwrap().starts_with("call_"));
    assert_ne!(deltas[0].1, deltas[1].1, "ids are unique per call");
    assert_eq!(deltas[0].2.as_deref(), Some(r#"{"path":"a.rs"}"#));

    let resp = completed(&events);
    assert_eq!(resp.stop_reason, Some(StopReason::ToolCalls));
    let a = resp.assistant().unwrap();
    assert_eq!(a.tool_calls.len(), 2);
    assert_eq!(&*a.tool_calls[1].arguments, r#"{"path":"b.rs"}"#);
    assert_eq!(
        reasoning_of(resp).unwrap().encrypted_content.as_deref(),
        Some("SIG_CALL")
    );
}

#[tokio::test]
async fn first_call_signature_wins_over_a_later_text_signature() {
    let events = run(vec![chunk(
        vec![
            Part::text("ok").with_thought_signature("SIG_TEXT"),
            call("a", serde_json::json!({})).with_thought_signature("SIG_A"),
            call("b", serde_json::json!({})).with_thought_signature("SIG_B"),
        ],
        Some(FinishReason::Stop),
    )])
    .await;
    let resp = completed(&events);
    assert_eq!(
        reasoning_of(resp).unwrap().encrypted_content.as_deref(),
        Some("SIG_A")
    );
}

#[tokio::test]
async fn function_call_without_args_serializes_empty_object() {
    let events = run(vec![chunk(
        vec![Part::function_call(FunctionCall {
            id: None,
            name: "noop".into(),
            args: None,
        })],
        Some(FinishReason::Stop),
    )])
    .await;
    let resp = completed(&events);
    assert_eq!(&*resp.assistant().unwrap().tool_calls[0].arguments, "{}");
    assert!(reasoning_of(resp).is_none());
}

#[tokio::test]
async fn max_tokens_maps_to_length_even_with_calls() {
    let events = run(vec![chunk(
        vec![Part::text("partial"), call("a", serde_json::json!({}))],
        Some(FinishReason::MaxTokens),
    )])
    .await;
    let resp = completed(&events);
    assert_eq!(resp.stop_reason, Some(StopReason::Length));
    assert_eq!(resp.raw_stop_reason.as_deref(), Some("MAX_TOKENS"));
}

#[tokio::test]
async fn safety_finish_maps_to_content_filter_with_message() {
    let mut c = chunk(vec![], Some(FinishReason::Safety));
    c.candidates[0].finish_message = Some("harm category".into());
    let events = run(vec![c]).await;
    let resp = completed(&events);
    assert_eq!(resp.stop_reason, Some(StopReason::ContentFilter));
    assert_eq!(resp.stop_message.as_deref(), Some("harm category"));
    assert_eq!(resp.raw_stop_reason.as_deref(), Some("SAFETY"));
}

#[tokio::test]
async fn prompt_block_without_candidates_is_a_content_filter_terminal() {
    let events = run(vec![GenerateContentResponse {
        candidates: vec![],
        prompt_feedback: Some(PromptFeedback {
            block_reason: Some("PROHIBITED_CONTENT".into()),
            block_reason_message: None,
        }),
        usage_metadata: Some(usage(7, 0, 0, 0)),
        model_version: None,
        response_id: None,
    }])
    .await;
    let resp = completed(&events);
    assert_eq!(resp.stop_reason, Some(StopReason::ContentFilter));
    assert_eq!(
        resp.raw_stop_reason.as_deref(),
        Some("BLOCKED_PROHIBITED_CONTENT")
    );
    assert_eq!(
        resp.stop_message.as_deref(),
        Some("prompt blocked: PROHIBITED_CONTENT")
    );
    let a = resp.assistant().unwrap();
    assert!(a.content.is_empty());
    assert_eq!(a.model_id, None, "no modelVersion ⇒ no invented model id");
}

#[tokio::test]
async fn malformed_function_call_fails_the_stream() {
    let events = run(vec![chunk(
        vec![Part::text("x")],
        Some(FinishReason::MalformedFunctionCall),
    )])
    .await;
    let msg = failed_type(&events);
    assert!(msg.contains("malformed_model_output"), "{msg}");
}

#[tokio::test]
async fn unknown_finish_reason_completes_as_stop_with_raw_value() {
    let events = run(vec![chunk(
        vec![Part::text("x")],
        Some(FinishReason::Unknown("SOME_NEW_REASON".into())),
    )])
    .await;
    let resp = completed(&events);
    assert_eq!(resp.stop_reason, Some(StopReason::Stop));
    assert_eq!(resp.raw_stop_reason.as_deref(), Some("SOME_NEW_REASON"));
}

#[tokio::test]
async fn empty_stream_fails_truncated() {
    let events = run(vec![]).await;
    assert_eq!(events.len(), 2);
    let msg = failed_type(&events);
    assert!(msg.contains("stream_truncated"), "{msg}");
}

#[tokio::test]
async fn stream_without_finish_reason_fails_truncated() {
    let events = run(vec![chunk(vec![Part::text("cut")], None)]).await;
    let msg = failed_type(&events);
    assert!(msg.contains("stream_truncated"), "{msg}");
}

#[tokio::test]
async fn transport_error_mid_stream_fails() {
    let raw = stream::iter(vec![
        Ok(chunk(vec![Part::text("a")], None)),
        Err(SamplingError::EventStreamError("boom".into())),
    ])
    .boxed();
    let events = collect(stream_generate_content(
        raw,
        None,
        rid(),
        Duration::from_secs(60),
    ))
    .await;
    assert!(matches!(events.last(), Some(SamplingEvent::Failed { .. })));
}

#[tokio::test]
async fn idle_timeout_fails_the_stream() {
    let raw = stream::pending::<Result<GenerateContentResponse, SamplingError>>().boxed();
    let events = collect(stream_generate_content(
        raw,
        None,
        rid(),
        Duration::from_millis(20),
    ))
    .await;
    match events.last() {
        Some(SamplingEvent::Failed { error, .. }) => {
            assert!(error.message.to_lowercase().contains("idle"), "{}", error.message);
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[tokio::test]
async fn usage_is_cumulative_last_wins_and_counts_thoughts_in_completion() {
    let mut c1 = chunk(vec![Part::thought("t")], None);
    c1.usage_metadata = Some(usage(100, 1, 1, 0));
    let mut c2 = chunk(vec![Part::text("x")], Some(FinishReason::Stop));
    c2.usage_metadata = Some(usage(100, 20, 30, 40));
    let events = run(vec![c1, c2]).await;
    let u = completed(&events).usage.clone().unwrap();
    assert_eq!(u.prompt_tokens, 100);
    assert_eq!(u.completion_tokens, 50);
    assert_eq!(u.total_tokens, 150);
    assert_eq!(u.reasoning_tokens, 30);
    assert_eq!(u.cached_prompt_tokens, 40);
}

#[tokio::test]
async fn model_metadata_is_forwarded_after_stream_started() {
    let raw = stream::iter(vec![Ok(chunk(vec![Part::text("a")], Some(FinishReason::Stop)))]).boxed();
    let md = ResponseModelMetadata {
        context_window: Some(1_000_000),
        ..Default::default()
    };
    let events = collect(stream_generate_content(
        raw,
        Some(md),
        rid(),
        Duration::from_secs(60),
    ))
    .await;
    assert!(matches!(events[0], SamplingEvent::StreamStarted { .. }));
    assert!(matches!(events[1], SamplingEvent::ModelMetadata { .. }));
}

#[test]
fn heartbeat_classification() {
    use crate::span_timing::ItemClass;
    use crate::stream_classify::generate_content_chunk_class;

    // Liveness-only frames must not release the first-content hold.
    assert!(matches!(
        generate_content_chunk_class(&GenerateContentResponse::default()),
        ItemClass::Other
    ));
    assert!(matches!(
        generate_content_chunk_class(&chunk(vec![], None)),
        ItemClass::Other
    ));
    assert!(matches!(
        generate_content_chunk_class(&chunk(vec![Part::text("a")], None)),
        ItemClass::Content
    ));
    assert!(matches!(
        generate_content_chunk_class(&chunk(vec![], Some(FinishReason::Stop))),
        ItemClass::End
    ));
    let blocked = GenerateContentResponse {
        prompt_feedback: Some(PromptFeedback {
            block_reason: Some("SAFETY".into()),
            block_reason_message: None,
        }),
        ..Default::default()
    };
    assert!(matches!(generate_content_chunk_class(&blocked), ItemClass::End));
}
