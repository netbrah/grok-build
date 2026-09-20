//! Integration tests that run the real turn loop against a scripted Chat Completions backend whose responses end with `finish_reason: "length"`.
//!
//! There is deliberately no test of the default agent with the env gate ON: env mutation is process-global and racy in the parallel test binary.
//! That case gains coverage when the RemoteSettings gate makes the budget injectable.
use super::support::*;
use super::*;
use std::sync::Arc;
use std::time::Duration;
use xai_grok_test_support::sse::chat_completion_script_exact;
use xai_grok_test_support::{MockInferenceServer, ScriptedResponse, SseEvent};
/// Distinctive fragment of the continue reminder.
const REMINDER_MARKER: &str = "exceeded the output token limit";
/// `SessionActor` turn futures overflow the default test thread stack.
fn block_on_session(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(f)
        .expect("spawn large-stack test thread")
        .join()
        .expect("test thread");
}
fn current_thread_local<F>(f: F)
where
    F: std::future::Future<Output = ()> + 'static,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    tokio::task::LocalSet::new().block_on(&rt, f);
}
/// SSE stream ending `finish_reason: "length"`.
/// Its reasoning delta pins that report aggregation spans a synthesized `Reasoning` sibling.
fn length_sse(text: &str) -> ScriptedResponse {
    ScriptedResponse::sse(vec![
        SseEvent::data(
            serde_json::json!({
                "id": "chatcmpl-len",
                "object": "chat.completion.chunk",
                "created": 1234567890,
                "model": "test",
                "choices": [{
                    "index": 0,
                    "delta": {
                        "role": "assistant",
                        "reasoning_content": "thinking about the cut",
                        "content": text
                    },
                    "finish_reason": null
                }]
            })
            .to_string(),
        ),
        SseEvent::data(
            serde_json::json!({
                "id": "chatcmpl-len",
                "object": "chat.completion.chunk",
                "created": 1234567890,
                "model": "test",
                "choices": [{ "index": 0, "delta": {}, "finish_reason": "length" }],
                "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
            })
            .to_string(),
        ),
        SseEvent::data("[DONE]".to_string()),
    ])
}
fn stop_sse(text: &str) -> ScriptedResponse {
    ScriptedResponse::sse(chat_completion_script_exact(text, "test"))
}
/// Build an actor wired to the mock server on the Chat Completions backend.
/// `is_subagent` sets the subagent startup hint before the actor is Arc'd
/// (ANTHROPIC-WIRE-2 cut 4 tests the subagent salvage tier on a real actor).
async fn salvage_test_actor(server: &MockInferenceServer, is_subagent: bool) -> Arc<SessionActor> {
    salvage_test_actor_with_context(server, 0, 256_000, is_subagent).await
}
/// [`salvage_test_actor`] with a seeded token total and context window, for the tests where salvage interacts with compaction.
async fn salvage_test_actor_with_context(
    server: &MockInferenceServer,
    total_tokens: u64,
    context_window: u64,
    is_subagent: bool,
) -> Arc<SessionActor> {
    salvage_test_actor_on_backend(
        server,
        total_tokens,
        context_window,
        xai_grok_sampling_types::ApiBackend::ChatCompletions,
        is_subagent,
    )
    .await
}
/// [`salvage_test_actor`] on an explicit backend.
/// The Messages test exists because only that stream layer delivers `Length` with completed tool calls.
/// Chat Completions rewrites the stop reason to `ToolCalls`.
async fn salvage_test_actor_on_backend(
    server: &MockInferenceServer,
    total_tokens: u64,
    context_window: u64,
    backend: xai_grok_sampling_types::ApiBackend,
    is_subagent: bool,
) -> Arc<SessionActor> {
    let sampling_cfg = xai_grok_sampler::SamplerConfig {
        api_key: Some("test-key".to_string()),
        base_url: server.url(),
        model: "test".to_string(),
        api_backend: backend.clone(),
        context_window,
        max_retries: Some(0),
        idle_timeout_secs: Some(30),
        ..Default::default()
    };
    let (sampler_event_tx, sampler_event_rx) =
        tokio::sync::mpsc::unbounded_channel::<xai_grok_sampler::SamplingEvent>();
    let sampler_handle = xai_grok_sampler::SamplerActor::spawn(
        sampling_cfg,
        xai_grok_sampler::RetryPolicy {
            max_retries: 0,
            ..Default::default()
        },
        sampler_event_tx,
    );
    let (gateway_tx, gateway_rx) =
        tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
    drain_gateway(gateway_rx);
    let (persistence_tx, persistence_rx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
    drain_persistence(persistence_rx);
    let mut actor =
        create_test_actor(total_tokens, context_window, 85, gateway_tx, persistence_tx).await;
    actor.sampler_handle = sampler_handle;
    let mut cfg = actor
        .chat_state_handle
        .get_sampling_config()
        .await
        .expect("test actor has sampling config");
    cfg.base_url = server.url();
    cfg.api_backend = backend;
    cfg.model = "test".to_string();
    actor.chat_state_handle.update_sampling_config(cfg);
    let mut creds = actor.chat_state_handle.get_credentials().await;
    creds.api_key = Some("test-key".to_string());
    actor.chat_state_handle.update_credentials(creds);
    actor
        .workspace_ops
        .bind_local_session(
            &actor.session_id_string(),
            actor.tool_context.cwd.as_path().to_path_buf(),
            actor.tool_context.hunk_tracker_handle.clone(),
            actor.agent.borrow().tool_bridge().toolset(),
            None,
        )
        .expect("bind_local_session");
    if is_subagent {
        actor.startup_hints = crate::session::StartupHints {
            is_subagent: true,
            ..Default::default()
        };
    }
    let actor = Arc::new(actor);
    {
        let drainer = actor.clone();
        let mut sampler_event_rx = sampler_event_rx;
        tokio::task::spawn_local(async move {
            while let Some(event) = sampler_event_rx.recv().await {
                drainer.handle_sampling_event(event).await;
            }
        });
    }
    actor
}
async fn run_prompt(actor: &Arc<SessionActor>, prompt_id: &str) -> PromptTurnResult {
    let prompt_blocks = vec![acp::ContentBlock::Text(acp::TextContent::new(
        "write out the numbers".to_string(),
    ))];
    tokio::time::timeout(
        Duration::from_secs(60),
        actor.handle_prompt(
            prompt_id,
            prompt_blocks,
            PromptMode::Agent,
            None,
            None,
            None,
            None,
            true,
            false,
            None,
            None,
            None,
        ),
    )
    .await
    .expect("turn must finish within timeout")
}
/// The remote tier: a spawn-resolved `length_salvage_budget` reaches the
/// resolver on a real actor (the previously env-blocked default-agent ON
/// cell, now injectable).
#[test]
fn remote_budget_wires_into_the_resolver() {
    if xai_grok_config::env_bool("GROK_LENGTH_SALVAGE") == Some(true) {
        panic!("ambient GROK_LENGTH_SALVAGE=1 would mask the remote tier under test");
    }
    block_on_session(|| {
        current_thread_local(async {
            let (gateway_tx, gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            drain_gateway(gateway_rx);
            let (persistence_tx, persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            drain_persistence(persistence_rx);
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            assert_eq!(actor.length_salvage_budget(), None, "off by default");
            actor.length_salvage_remote_budget = Some(9);
            assert_eq!(
                actor.length_salvage_budget(),
                Some(9),
                "the remote tier feeds the resolver"
            );
            actor.length_salvage_remote_budget = Some(0);
            assert_eq!(
                actor.length_salvage_budget(),
                None,
                "the remote kill zero turns salvage off outright"
            );
        });
    });
}
/// `RemoteSettings` wire contract: legacy payloads without the key and
/// explicit null both mean absent; set values (including the kill zero)
/// round-trip.
#[test]
fn remote_settings_length_salvage_budget_serde_cells() {
    use crate::util::config::RemoteSettings;
    let legacy: RemoteSettings = serde_json::from_str("{}").expect("legacy payload");
    assert_eq!(legacy.length_salvage_budget, None);
    let null: RemoteSettings =
        serde_json::from_value(serde_json::json!({ "length_salvage_budget": null }))
            .expect("explicit null");
    assert_eq!(null.length_salvage_budget, None);
    let set: RemoteSettings =
        serde_json::from_value(serde_json::json!({ "length_salvage_budget": 0 }))
            .expect("kill value");
    assert_eq!(set.length_salvage_budget, Some(0));
}
/// Default agent with the gate off (no env var): a Length response is the legacy non-retryable hard failure.
/// This is the safety property that nothing changes for production default agents until the rollout flag lands.
#[test]
fn default_agent_gate_off_hard_fails_on_length() {
    if xai_grok_config::env_bool("GROK_LENGTH_SALVAGE") == Some(true) {
        panic!("ambient GROK_LENGTH_SALVAGE=1 would flip the gate under test");
    }
    block_on_session(|| {
        current_thread_local(async {
            let server = MockInferenceServer::start().await.expect("mock server");
            server.enqueue_response("/v1/chat/completions", length_sse("one, two, three,"));
            let actor = salvage_test_actor(&server, false).await;
            let outcome = run_prompt(&actor, "length-gate-off").await;
            let err = outcome.expect_err("gate off: Length must hard-fail the turn");
            let err_str = format!("{err:?}");
            assert!(
                err_str.contains("max_tokens") || err_str.contains("max tokens"),
                "the failure must be the max-tokens truncation error: {err_str}"
            );
            let conv = actor.chat_state_handle.get_conversation().await;
            let all_text: Vec<String> = conv.iter().map(|i| i.text_content()).collect();
            assert!(
                !all_text.iter().any(|t| t.contains(REMINDER_MARKER)),
                "no continue reminder without the gate: {all_text:#?}"
            );
        });
    });
}
/// Terminal error whose metadata reports a tiny context window.
/// The overflow heuristic compares the token estimate to that window, so it fires for any nonempty history.
fn error_with_tiny_window(
    kind: xai_grok_sampler::SamplingErrorKind,
    status_code: u16,
) -> xai_grok_sampler::SamplingErrorInfo {
    xai_grok_sampler::SamplingErrorInfo {
        kind,
        status_code: Some(status_code),
        message: "terminal failure".to_string(),
        should_retry: None,
        error_code: None,
        is_retryable: false,
        retry_after_secs: None,
        model_metadata: Some(xai_grok_sampling_types::ResponseModelMetadata {
            context_window: Some(1),
            max_completion_tokens: None,
            models_etag: None,
        }),
        empty_response_context: None,
        doom_loop_triggers: None,
        doom_loop_aborted_at_chunk: None,
        credential: xai_grok_sampling_types::SentCredential::Unknown,
    }
}
/// A rate-limited terminal error mid-continuation keeps its terminal arm even when the estimate exceeds the reported window.
/// The quiet truncated-complete arm is only for overflows and empty caps, never for kinds naming a non-overflow cause.
#[test]
fn rate_limit_mid_continuation_stays_terminal() {
    block_on_session(|| {
        current_thread_local(async {
            let server = MockInferenceServer::start().await.expect("mock server");
            let actor = salvage_test_actor(&server, false).await;
            actor.chat_state_handle.push_user_message(
                xai_grok_sampling_types::ConversationItem::user(
                    "enough history that the token estimate clears the tiny window",
                ),
            );
            let error =
                error_with_tiny_window(xai_grok_sampler::SamplingErrorKind::RateLimited, 429);
            let Err(err) = actor
                .handle_sampling_failure(
                    error,
                    0,
                    transient_state(0, true),
                    true,
                    TurnParkState::Fresh,
                )
                .await
            else {
                panic!("a mid-salvage rate limit is still terminal");
            };
            assert_eq!(
                i32::from(err.code),
                crate::sampling::error::RATE_LIMITED_ERROR_CODE,
                "must take the rate-limited arm, not the quiet truncated-complete arm: {err:?}"
            );
        });
    });
}
/// A genuine context overflow mid-continuation completes the turn truncated even while auto-compaction is sticky-suppressed.
/// The overflow signal must not depend on the compaction gate.
#[test]
fn suppressed_overflow_mid_continuation_still_completes_truncated() {
    block_on_session(|| {
        current_thread_local(async {
            let server = MockInferenceServer::start().await.expect("mock server");
            let actor = salvage_test_actor(&server, false).await;
            actor.chat_state_handle.push_user_message(
                xai_grok_sampling_types::ConversationItem::user(
                    "enough history that the token estimate clears the tiny window",
                ),
            );
            actor.compaction.auto_compact_suppressed.store(
                crate::session::compaction_config::SUPPRESS_STICKY,
                std::sync::atomic::Ordering::Relaxed,
            );
            let error = error_with_tiny_window(xai_grok_sampler::SamplingErrorKind::Api, 500);
            let Err(err) = actor
                .handle_sampling_failure(
                    error,
                    0,
                    transient_state(0, true),
                    true,
                    TurnParkState::Fresh,
                )
                .await
            else {
                panic!("the quiet arm returns the typed error");
            };
            assert!(
                crate::sampling::error::is_max_tokens_turn_error(&err),
                "the turn loop needs the max-tokens marker to complete truncated: {err:?}"
            );
            let cause = err
                .data
                .as_ref()
                .and_then(|d| d.get(crate::sampling::error::SALVAGE_CAUSE_KEY))
                .and_then(|v| v.as_str());
            assert_eq!(
                cause,
                Some(crate::sampling::error::SALVAGE_CAUSE_OVERFLOW),
                "an over-window failure is the overflow population"
            );
        });
    });
}
/// Messages-backend SSE: text and a completed `tool_use` block, terminated `stop_reason: "max_tokens"`.
/// Only this backend delivers `Length` with tool calls to the turn loop.
/// Chat Completions rewrites the stop reason to `ToolCalls` at the stream layer.
fn messages_length_with_tool_call_sse(
    call_id: &str,
    name: &str,
    arguments: &str,
) -> ScriptedResponse {
    let events = vec![
        serde_json::json!({
            "type": "message_start",
            "message": {
                "id": "msg_len_tools", "type": "message", "role": "assistant",
                "content": [], "model": "test", "stop_reason": null,
                "usage": {
                    "input_tokens": 10, "output_tokens": 0,
                    "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0
                }
            }
        }),
        serde_json::json!({
            "type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}
        }),
        serde_json::json!({
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "recording the todo"}
        }),
        serde_json::json!({"type": "content_block_stop", "index": 0}),
        serde_json::json!({
            "type": "content_block_start", "index": 1,
            "content_block": {"type": "tool_use", "id": call_id, "name": name, "input": {}}
        }),
        serde_json::json!({
            "type": "content_block_delta", "index": 1,
            "delta": {"type": "input_json_delta", "partial_json": arguments}
        }),
        serde_json::json!({"type": "content_block_stop", "index": 1}),
        serde_json::json!({
            "type": "message_delta",
            "delta": {"stop_reason": "max_tokens"},
            "usage": {"output_tokens": 5, "input_tokens": 10}
        }),
        serde_json::json!({"type": "message_stop"}),
    ];
    ScriptedResponse::sse(
        events
            .into_iter()
            .map(|e| SseEvent::data(e.to_string()))
            .collect(),
    )
}
/// Messages SSE: plain text terminated `stop_reason: "end_turn"`.
fn messages_stop_sse(text: &str) -> ScriptedResponse {
    let events = vec![
        serde_json::json!({
            "type": "message_start",
            "message": {
                "id": "msg_stop", "type": "message", "role": "assistant",
                "content": [], "model": "test", "stop_reason": null,
                "usage": {
                    "input_tokens": 10, "output_tokens": 0,
                    "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0
                }
            }
        }),
        serde_json::json!({
            "type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}
        }),
        serde_json::json!({
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": text}
        }),
        serde_json::json!({"type": "content_block_stop", "index": 0}),
        serde_json::json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 5, "input_tokens": 10}
        }),
        serde_json::json!({"type": "message_stop"}),
    ];
    ScriptedResponse::sse(
        events
            .into_iter()
            .map(|e| SseEvent::data(e.to_string()))
            .collect(),
    )
}
/// ANTHROPIC-WIRE-2 (cut 4): subagent turns default salvage on (budget 3) with
/// no env gate and no remote budget — the .41 root-cause cell (hyphen claude
/// subagent on a small cap, salvage off, hard-fail max_tokens_truncation) is
/// closed: the turn completes through the continue.
#[test]
fn subagent_turns_default_salvage_on_without_env_or_remote() {
    if xai_grok_config::env_bool("GROK_LENGTH_SALVAGE") == Some(true) {
        panic!("ambient GROK_LENGTH_SALVAGE=1 would mask the subagent tier under test");
    }
    block_on_session(|| {
        current_thread_local(async {
            let server = MockInferenceServer::start().await.expect("mock server");
            server.enqueue_response("/v1/chat/completions", length_sse("one, two,"));
            server.enqueue_response("/v1/chat/completions", stop_sse("three, four."));
            let actor = salvage_test_actor(&server, true).await;
            assert_eq!(
                actor.length_salvage_budget(),
                Some(3),
                "the subagent tier defaults on with the 3-attempt budget"
            );
            let outcome = run_prompt(&actor, "subagent-salvage-default").await;
            outcome.expect("subagent salvage must complete the turn, not hard-fail");
            let conv = actor.chat_state_handle.get_conversation().await;
            let all_text: Vec<String> = conv.iter().map(|i| i.text_content()).collect();
            assert!(
                all_text.iter().any(|t| t.contains(REMINDER_MARKER)),
                "the continue reminder marks the salvage retry: {all_text:#?}"
            );
        });
    });
}
/// ANTHROPIC-WIRE-2 (cut 4): the first Length stop re-samples at the escalated
/// cap — max(64K, the alias-aware R5 table row), here the 64K floor for the
/// row-less "test" slug — OUTSIDE the continue budget; the initial sample kept
/// the small configured cap, and one escalation continue is enough to finish
/// the turn. (The remote tier outranking the subagent default is covered at
/// unit level: `subagent_tier_loses_to_every_explicit_tier_and_kill`.)
#[test]
fn escalation_bumps_the_continuation_cap_outside_the_budget() {
    if xai_grok_config::env_bool("GROK_LENGTH_SALVAGE") == Some(true) {
        panic!("ambient GROK_LENGTH_SALVAGE=1 would mask the remote tier under test");
    }
    block_on_session(|| {
        current_thread_local(async {
            let server = MockInferenceServer::start().await.expect("mock server");
            server.enqueue_response("/v1/chat/completions", length_sse("one, two,"));
            server.enqueue_response("/v1/chat/completions", stop_sse("three, four."));
            let actor = salvage_test_actor(&server, true).await;
            assert_eq!(
                actor.length_salvage_budget(),
                Some(3),
                "the subagent tier defaults on with the 3-attempt budget"
            );
            let mut cfg = actor
                .chat_state_handle
                .get_sampling_config()
                .await
                .expect("test actor has sampling config");
            cfg.max_completion_tokens = Some(64);
            actor.chat_state_handle.update_sampling_config(cfg);
            let outcome = run_prompt(&actor, "escalation-cap-bump").await;
            outcome.expect("the escalated continuation must complete the turn");
            let caps: Vec<u64> = server
                .request_bodies()
                .iter()
                .filter_map(|b| b.get("max_tokens").and_then(|v| v.as_u64()))
                .collect();
            assert_eq!(
                caps.first(),
                Some(&64),
                "the initial sample keeps the configured cap: {caps:?}"
            );
            assert!(
                caps.iter().filter(|c| **c == 64_000).count() == 1,
                "exactly one continuation carries the escalated 64K cap: {caps:?}"
            );
            assert!(
                caps.windows(2).any(|w| w[0] == 64 && w[1] == 64_000),
                "the escalation resample immediately follows the capped sample: {caps:?}"
            );
        });
    });
}

// ============================================================================
// STOPREASON-TYPED-1 (apex-ayl.49): stop-reason typed terminals — shell turn loop
// Real turn loop against a scripted Messages backend. Reuses the salvage harness above.
// Provenance: frozen-spec@2cbc222c §6.3 L4271-4286 (+ L1976-1978 / L1980-1981).
// ============================================================================

/// Messages SSE: a text-only turn terminated `stop_reason: "model_context_window_exceeded"`.
fn messages_context_full_sse(text: &str) -> ScriptedResponse {
    let events = vec![
        serde_json::json!({
            "type": "message_start",
            "message": {
                "id": "msg_ctx_full", "type": "message", "role": "assistant",
                "content": [], "model": "test", "stop_reason": null,
                "usage": {
                    "input_tokens": 10, "output_tokens": 0,
                    "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0
                }
            }
        }),
        serde_json::json!({
            "type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}
        }),
        serde_json::json!({
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": text}
        }),
        serde_json::json!({"type": "content_block_stop", "index": 0}),
        serde_json::json!({
            "type": "message_delta",
            "delta": {"stop_reason": "model_context_window_exceeded"},
            "usage": {"output_tokens": 5, "input_tokens": 10}
        }),
        serde_json::json!({"type": "message_stop"}),
    ];
    ScriptedResponse::sse(
        events.into_iter().map(|e| SseEvent::data(e.to_string())).collect(),
    )
}

/// Messages SSE: text + a CLOSED tool_use block, terminated
/// `stop_reason: "model_context_window_exceeded"`.
fn messages_context_full_with_tools_sse(
    call_id: &str,
    name: &str,
    arguments: &str,
) -> ScriptedResponse {
    let events = vec![
        serde_json::json!({
            "type": "message_start",
            "message": {
                "id": "msg_ctx_tools", "type": "message", "role": "assistant",
                "content": [], "model": "test", "stop_reason": null,
                "usage": {
                    "input_tokens": 10, "output_tokens": 0,
                    "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0
                }
            }
        }),
        serde_json::json!({
            "type": "content_block_start", "index": 0,
            "content_block": {"type": "tool_use", "id": call_id, "name": name, "input": {}}
        }),
        serde_json::json!({
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "input_json_delta", "partial_json": arguments}
        }),
        serde_json::json!({"type": "content_block_stop", "index": 0}),
        serde_json::json!({
            "type": "message_delta",
            "delta": {"stop_reason": "model_context_window_exceeded"},
            "usage": {"output_tokens": 5, "input_tokens": 10}
        }),
        serde_json::json!({"type": "message_stop"}),
    ];
    ScriptedResponse::sse(
        events.into_iter().map(|e| SseEvent::data(e.to_string())).collect(),
    )
}

/// Messages SSE: a CLOSED tool_use block, terminated `stop_reason: "refusal"` with a
/// provider `stop_details` explanation (the Anthropic ToS auto-refusal shape).
fn messages_refusal_with_tools_sse(
    call_id: &str,
    name: &str,
    arguments: &str,
    explanation: &str,
) -> ScriptedResponse {
    let events = vec![
        serde_json::json!({
            "type": "message_start",
            "message": {
                "id": "msg_refusal_tools", "type": "message", "role": "assistant",
                "content": [], "model": "test", "stop_reason": null,
                "usage": {
                    "input_tokens": 10, "output_tokens": 0,
                    "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0
                }
            }
        }),
        serde_json::json!({
            "type": "content_block_start", "index": 0,
            "content_block": {"type": "tool_use", "id": call_id, "name": name, "input": {}}
        }),
        serde_json::json!({
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "input_json_delta", "partial_json": arguments}
        }),
        serde_json::json!({"type": "content_block_stop", "index": 0}),
        serde_json::json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": "refusal",
                "stop_details": {"type": "refusal", "category": "frontier_llm", "explanation": explanation}
            },
            "usage": {"output_tokens": 5, "input_tokens": 10}
        }),
        serde_json::json!({"type": "message_stop"}),
    ];
    ScriptedResponse::sse(
        events.into_iter().map(|e| SseEvent::data(e.to_string())).collect(),
    )
}

/// Messages SSE: partial text streamed, then a terminal `stop_reason: "pause_turn"`.
fn messages_pause_turn_sse(text: &str) -> ScriptedResponse {
    let events = vec![
        serde_json::json!({
            "type": "message_start",
            "message": {
                "id": "msg_pause", "type": "message", "role": "assistant",
                "content": [], "model": "test", "stop_reason": null,
                "usage": {
                    "input_tokens": 10, "output_tokens": 0,
                    "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0
                }
            }
        }),
        serde_json::json!({
            "type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}
        }),
        serde_json::json!({
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": text}
        }),
        serde_json::json!({"type": "content_block_stop", "index": 0}),
        serde_json::json!({
            "type": "message_delta",
            "delta": {"stop_reason": "pause_turn"},
            "usage": {"output_tokens": 5, "input_tokens": 10}
        }),
        serde_json::json!({"type": "message_stop"}),
    ];
    ScriptedResponse::sse(
        events.into_iter().map(|e| SseEvent::data(e.to_string())).collect(),
    )
}

/// Messages SSE: ZERO visible content (no blocks), a bare terminal
/// `stop_reason: "model_context_window_exceeded"` (the m-1 empty-resample shape).
fn messages_context_full_empty_sse() -> ScriptedResponse {
    let events = vec![
        serde_json::json!({
            "type": "message_start",
            "message": {
                "id": "msg_ctx_empty", "type": "message", "role": "assistant",
                "content": [], "model": "test", "stop_reason": null,
                "usage": {
                    "input_tokens": 10, "output_tokens": 0,
                    "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0
                }
            }
        }),
        serde_json::json!({
            "type": "message_delta",
            "delta": {"stop_reason": "model_context_window_exceeded"},
            "usage": {"output_tokens": 0, "input_tokens": 10}
        }),
        serde_json::json!({"type": "message_stop"}),
    ];
    ScriptedResponse::sse(
        events.into_iter().map(|e| SseEvent::data(e.to_string())).collect(),
    )
}

/// SH1 (apex-ayl.49 item 1): a context-full turn EXITS the salvage loop.
/// A salvage-enabled (subagent-tier) actor sees exactly ONE request (no free-escalation retry,
/// no budgeted continue — pre-fix the Length-mapped context-full resampled >=2 times), the turn
/// completes Ok with the typed terminal (projected to ACP MaxTokens), and force_compact is armed.
#[test]
fn context_full_exits_the_salvage_loop() {
    block_on_session(|| {
        current_thread_local(async {
            let server = MockInferenceServer::start().await.expect("mock server");
            // Enqueue enough identical context-full responses that a pre-fix salvage loop
            // (Length-mapped) can never hang on a missing response; post-fix exactly one is used.
            for _ in 0..8 {
                server.enqueue_response(
                    "/v1/messages",
                    messages_context_full_sse("the answer was cut short"),
                );
            }
            let actor = salvage_test_actor_on_backend(
                &server,
                0,
                256_000,
                xai_grok_sampling_types::ApiBackend::Messages,
                true,
            )
            .await;
            let outcome = run_prompt(&actor, "ctx-full-exit").await;
            let ok = outcome.expect(
                "a context-full turn completes Ok (typed terminal), it is never a turn error",
            );
            assert_eq!(
                server.messages_request_count(),
                1,
                "context-window completion is never retried: exactly one request (L4286)"
            );
            assert_eq!(
                ok.stop_reason,
                acp::StopReason::MaxTokens,
                "the context-full terminal projects to ACP MaxTokens"
            );
            assert!(
                actor
                    .compaction
                    .force_compact
                    .load(std::sync::atomic::Ordering::Relaxed),
                "the context-full turn arms force_compact for the next pre-turn compaction (L4286)"
            );
        });
    });
}

/// SH2 (apex-ayl.49 item 1 + item 3): a context-full turn carrying a completed tool_use block is
/// REJECTED before commit — exactly one request (no tool-execution round), the tool is NOT executed,
/// the context-full assistant item is NOT committed to history, and the turn completes Ok with the
/// typed terminal. Pre-fix (Length-mapped) it committed the item and `LengthPolicy` executed the call.
#[test]
fn context_full_rejects_tools_before_commit() {
    block_on_session(|| {
        current_thread_local(async {
            let server = MockInferenceServer::start().await.expect("mock server");
            for _ in 0..8 {
                server.enqueue_response(
                    "/v1/messages",
                    messages_context_full_with_tools_sse("call_full", "do_thing", "{\"x\": 1}"),
                );
            }
            let actor = salvage_test_actor_on_backend(
                &server,
                0,
                256_000,
                xai_grok_sampling_types::ApiBackend::Messages,
                true,
            )
            .await;
            let outcome = run_prompt(&actor, "ctx-full-reject-tools").await;
            let ok = outcome.expect("a rejected context-full turn completes Ok (typed terminal)");
            assert_eq!(
                server.messages_request_count(),
                1,
                "reject-before-commit: no tool-execution round (a dispatch would resample)"
            );
            assert_eq!(
                ok.stop_reason,
                acp::StopReason::MaxTokens,
                "the context-full terminal projects to ACP MaxTokens"
            );
            let conv = actor.chat_state_handle.get_conversation().await;
            assert!(
                !conv
                    .iter()
                    .any(|i| matches!(i, ConversationItem::Assistant(a) if !a.tool_calls.is_empty())),
                "the rejected context-full response is NOT committed to history: {conv:#?}"
            );
        });
    });
}

/// SH3 (apex-ayl.49 item 3): a refusal turn carrying a completed tool_use block is REJECTED before
/// commit — no tool execution, the refusal assistant item is NOT committed, and the turn completes
/// Ok with the Refusal terminal (the goal auto-pause path stays intact). Pre-fix the Q3 override
/// turned refusal+tools into `ToolCalls`, committed the item, and dispatched the call.
#[test]
fn refusal_with_tools_rejected_before_commit() {
    block_on_session(|| {
        current_thread_local(async {
            let server = MockInferenceServer::start().await.expect("mock server");
            for _ in 0..8 {
                server.enqueue_response(
                    "/v1/messages",
                    messages_refusal_with_tools_sse(
                        "call_refused",
                        "do_thing",
                        "{\"x\": 1}",
                        "This request was blocked.",
                    ),
                );
            }
            let actor = salvage_test_actor_on_backend(
                &server,
                0,
                256_000,
                xai_grok_sampling_types::ApiBackend::Messages,
                true,
            )
            .await;
            let outcome = run_prompt(&actor, "refusal-reject-tools").await;
            let ok = outcome.expect("a rejected refusal turn completes Ok (Refusal terminal)");
            assert_eq!(
                server.messages_request_count(),
                1,
                "reject-before-commit: no tool-execution round"
            );
            assert_eq!(
                ok.stop_reason,
                acp::StopReason::Refusal,
                "a rejected refusal keeps its Refusal terminal (goal auto-pause intact)"
            );
            let conv = actor.chat_state_handle.get_conversation().await;
            assert!(
                !conv
                    .iter()
                    .any(|i| matches!(i, ConversationItem::Assistant(a) if !a.tool_calls.is_empty())),
                "the rejected refusal response is NOT committed to history: {conv:#?}"
            );
        });
    });
}

/// SH4 (apex-ayl.49 item 2): a pause_turn turn FAILS with the dedicated error and commits nothing.
/// The partial streamed text is NOT committed, no tool is dispatched, and the error is NOT classified
/// as a max-tokens turn error (the mid-salvage empty-continuation arm cannot swallow it).
/// Pre-fix pause_turn completed as a normal `Stop` (EndTurn) and committed the partial response.
#[test]
fn pause_turn_turn_fails_with_dedicated_error_no_commit() {
    block_on_session(|| {
        current_thread_local(async {
            let server = MockInferenceServer::start().await.expect("mock server");
            server.enqueue_response(
                "/v1/messages",
                messages_pause_turn_sse("partial answer before the pause"),
            );
            let actor = salvage_test_actor_on_backend(
                &server,
                0,
                256_000,
                xai_grok_sampling_types::ApiBackend::Messages,
                true,
            )
            .await;
            let outcome = run_prompt(&actor, "pause-turn-fail").await;
            let err = outcome.expect_err(
                "a pause_turn turn must fail with the dedicated error, not complete",
            );
            let err_str = format!("{err}");
            assert!(
                err_str.contains("pause_turn") && err_str.contains("unsupported control"),
                "the dedicated error names the unsupported control, got: {err_str}"
            );
            // (d) NOT classified as a max-tokens turn error (M1: the mid-salvage arm cannot swallow it).
            assert!(
                !crate::sampling::error::is_max_tokens_turn_error(&err),
                "pause_turn is not a max-tokens turn error (the mid-salvage arm must not fire)"
            );
            let conv = actor.chat_state_handle.get_conversation().await;
            assert!(
                !conv
                    .iter()
                    .any(|i| matches!(i, ConversationItem::Assistant(_))),
                "the partial pause_turn response is NOT committed to history: {conv:#?}"
            );
        });
    });
}

/// SH5 (apex-ayl.49 m-1): a context-full response with ZERO visible content completes TYPED with no
/// empty-resample. Exactly ONE request (without the D1 exclusion extension the zero-content context-full
/// would invert into an `AttemptOutcome::Empty` retry storm, >=2 requests), the turn is Ok with the typed
/// terminal, and force_compact is armed.
#[test]
fn context_full_empty_response_completes_typed_no_resample() {
    block_on_session(|| {
        current_thread_local(async {
            let server = MockInferenceServer::start().await.expect("mock server");
            for _ in 0..8 {
                server.enqueue_response("/v1/messages", messages_context_full_empty_sse());
            }
            let actor = salvage_test_actor_on_backend(
                &server,
                0,
                256_000,
                xai_grok_sampling_types::ApiBackend::Messages,
                true,
            )
            .await;
            let outcome = run_prompt(&actor, "ctx-full-empty-no-resample").await;
            let ok = outcome.expect(
                "a zero-content context-full turn completes Ok (typed terminal), not an empty-resample storm",
            );
            assert_eq!(
                server.messages_request_count(),
                1,
                "no empty-resample: the D1 exclusion extension keeps the typed terminal flowing (m-1)"
            );
            assert_eq!(
                ok.stop_reason,
                acp::StopReason::MaxTokens,
                "the context-full terminal projects to ACP MaxTokens"
            );
            assert!(
                actor
                    .compaction
                    .force_compact
                    .load(std::sync::atomic::Ordering::Relaxed),
                "the context-full turn arms force_compact for the next pre-turn compaction"
            );
        });
    });
}
