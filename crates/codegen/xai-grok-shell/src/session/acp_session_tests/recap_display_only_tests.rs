//! Regression tests for the session-recap display-only invariant.
//!
//! A recap must NEVER mutate the model conversation: it is generated from a read-only snapshot and sent to the client as a notification only.
//! These tests lock that contract: after `handle_recap` returns, `get_conversation()` must be byte-identical to what it was before.

use super::support::*;
use super::*;
use xai_grok_sampling_types::ConversationItem;

/// Serializes `items` the way a main turn would, so auxiliary calls can be compared against the real wire shape.
///
/// apex-ayl.69 (XW-EMPTYID-1): the egress path patches the outgoing body with
/// `patch_reasoning_empty_ids` (sampler client.rs send sites, cell = session
/// model), so the expected main-turn shape must ride the same patch — otherwise
/// the prefix comparison pits post-patch actual against pre-patch expected.
/// The xw_ id is content-addressed (cell|ord|content|summary), so the patched
/// expected stays in lockstep with egress for any fixture content.
fn main_turn_input(items: Vec<ConversationItem>, cell: &str) -> Vec<serde_json::Value> {
    let request = xai_grok_sampling_types::ConversationRequest {
        items: xai_chat_state::compaction_utils::ModelRequestHistory::from_raw(items).into_items(),
        model: Some("test-model".to_string()),
        ..Default::default()
    };
    let mapped = async_openai::types::responses::CreateResponse::from(&request);
    let mut body = serde_json::to_value(&mapped).expect("request serializes");
    xai_grok_sampling_types::patch_reasoning_empty_ids(&mut body, cell);
    j(&body, "input")
        .as_array()
        .expect("input is an array")
        .clone()
}

/// Checks that an auxiliary call replays the parent conversation verbatim and appends one instruction. A prefix that shifts cannot hit the cache.
fn assert_rides_parent_prefix(
    body: &serde_json::Value,
    parent: Vec<ConversationItem>,
    label: &str,
    cell: &str,
) {
    let expected = main_turn_input(parent, cell);
    let raw = j(body, "input")
        .as_array()
        .expect("input must be present")
        .clone();
    // apex-ayl.86 (R-UNIFIED-ITEM v2, census-fix-3 R-B): the auxiliary call
    // egresses through the PATCHED responses path, so its parent prefix
    // carries exactly one injected `<multi_agent_mode>` developer item that
    // the unpatched `main_turn_input` expected value does not (post-cut
    // contract: auxiliary body = parent main-turn input INCLUDING the unified
    // item + exactly one appended instruction turn). Strip the single
    // injected item from `actual` so the prefix-equality and
    // one-instruction-turn intents below compare the parent conversation
    // itself, as before the cut.
    let mut seen_unified = false;
    let actual: Vec<serde_json::Value> = raw
        .into_iter()
        .filter(|item| {
            let is_unified = item
                .get("role")
                .and_then(|role| role.as_str())
                == Some("developer")
                && item
                    .get("content")
                    .and_then(|content| content.as_array())
                    .is_some_and(|parts| {
                        parts.len() == 1
                            && parts[0].get("type").and_then(|t| t.as_str()) == Some("input_text")
                            && parts[0]
                                .get("text")
                                .and_then(|text| text.as_str())
                                .is_some_and(|text| text.contains("<multi_agent_mode>"))
                    });
            if is_unified {
                assert!(
                    !seen_unified,
                    "{label}: more than one injected <multi_agent_mode> item"
                );
                seen_unified = true;
                return false;
            }
            true
        })
        .collect();
    assert!(
        seen_unified,
        "{label}: the injected <multi_agent_mode> item is missing from the auxiliary input"
    );
    assert!(
        actual.len() > expected.len(),
        "{label}: auxiliary input ({}) must extend the parent ({})",
        actual.len(),
        expected.len()
    );
    let Some(prefix) = actual.get(..expected.len()) else {
        panic!("{label}: actual shorter than parent prefix: {actual:?}");
    };
    assert_eq!(
        prefix,
        expected.as_slice(),
        "{label}: prefix diverges from the main turn"
    );
    assert_eq!(
        actual.len(),
        expected.len() + 1,
        "{label}: exactly one appended instruction turn"
    );
}

fn without_cache_control(mut value: serde_json::Value) -> serde_json::Value {
    match &mut value {
        serde_json::Value::Object(fields) => {
            fields.remove("cache_control");
            for value in fields.values_mut() {
                *value = without_cache_control(value.take());
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                *value = without_cache_control(value.take());
            }
        }
        _ => {}
    }
    value
}

fn assert_messages_rides_parent_prefix(
    body: &serde_json::Value,
    parent: Vec<ConversationItem>,
    label: &str,
) {
    let request = xai_grok_sampling_types::ConversationRequest {
        items: xai_chat_state::compaction_utils::ModelRequestHistory::from_raw(parent).into_items(),
        model: Some("test".to_string()),
        reasoning_effort: Some(xai_grok_sampling_types::ReasoningEffort::High),
        ..Default::default()
    };
    let expected = serde_json::to_value(xai_grok_sampling_types::build_messages_request(&request))
        .expect("main Messages request serializes");
    let expected_messages = without_cache_control(j(&expected, "messages").clone());
    let actual_messages = without_cache_control(j(body, "messages").clone());
    let mut expected = expected_messages
        .as_array()
        .expect("main Messages request has messages")
        .clone();
    let actual = actual_messages
        .as_array()
        .expect("side-call Messages request has messages");

    // MW-1 (trailing-assistant repair): a parent-only build that ends on an
    // assistant message gets a synthetic sentinel user appended, but the real
    // side call replaces that trailing position with its instruction, so drop
    // the sentinel from the expected prefix before comparing.
    if let Some(last) = expected.last()
        && expected.len() >= 2
        && last.get("role").and_then(|r| r.as_str()) == Some("user")
        && expected[expected.len() - 2]
            .get("role")
            .and_then(|r| r.as_str())
            == Some("assistant")
    {
        let is_sentinel = last
            .get("content")
            .and_then(|c| c.as_array())
            .is_some_and(|blocks| {
                blocks.len() == 1
                    && matches!(
                        blocks[0].get("text").and_then(|t| t.as_str()),
                        Some("[Continue]") | Some("[Awaiting tool result]")
                    )
            });
        if is_sentinel {
            expected.pop();
        }
    }

    assert!(
        actual.len() > expected.len(),
        "{label}: side-call Messages request must extend the parent"
    );
    let Some(prefix) = actual.get(..expected.len()) else {
        panic!("{label}: actual shorter than parent prefix: {actual:?}");
    };
    assert_eq!(
        prefix,
        expected.as_slice(),
        "{label}: Messages prefix diverges from the main turn"
    );
    assert_eq!(
        actual.len(),
        expected.len() + 1,
        "{label}: exactly one instruction message must be appended"
    );
    assert_eq!(
        j(j(body, "thinking"), "type"),
        "adaptive",
        "{label}: {body:#}"
    );
    assert_eq!(
        j(j(body, "output_config"), "effort"),
        "high",
        "{label}: {body:#}"
    );
}

fn assert_messages_reasoning_stripped(body: &serde_json::Value, label: &str) {
    assert!(
        body.get("thinking").is_none() || j(body, "thinking").is_null(),
        "{label}: top-level thinking must be absent: {body:#}"
    );
    for message in j(body, "messages")
        .as_array()
        .expect("Messages request has messages")
    {
        let Some(blocks) = j(message, "content").as_array() else {
            continue;
        };
        assert!(
            blocks.iter().all(|block| {
                !matches!(
                    j(block, "type").as_str(),
                    Some("thinking" | "redacted_thinking")
                )
            }),
            "{label}: replayed thinking must be absent: {body:#}"
        );
    }
}

/// Reasoning effort sits ahead of the conversation in the prompt, so an auxiliary call that drops it diverges from the main turn right away.
#[tokio::test(flavor = "current_thread")]
async fn side_question_projects_agent_messages_without_mutating_history() {
    use xai_grok_test_support::MockInferenceServer;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            *actor.agent.borrow_mut() = test_agent_with_goal_tool().await;

            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("an answer");
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = server.url();
            cfg.api_backend = xai_grok_sampling_types::ApiBackend::Responses;
            actor.chat_state_handle.update_sampling_config(cfg);

            let raw = vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::agent_message("agent context"),
            ];
            let raw_bytes = serde_json::to_vec(&raw).unwrap();
            actor.chat_state_handle.replace_conversation(raw);

            actor
                .handle_side_question("what context matters?", Vec::new())
                .await
                .expect("side question must succeed");

            let requests = server.requests();
            let body = requests
                .iter()
                .rev()
                .find(|request| request.path.contains("responses"))
                .and_then(|request| request.body.as_ref())
                .expect("btw body must be JSON")
                .to_string();
            assert!(body.contains(xai_chat_state::compaction_utils::AGENT_MESSAGE_MODEL_LABEL));
            assert_eq!(
                serde_json::to_vec(&actor.chat_state_handle.get_conversation().await).unwrap(),
                raw_bytes
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn auxiliary_calls_send_the_session_reasoning_effort() {
    use xai_grok_test_support::MockInferenceServer;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            *actor.agent.borrow_mut() = test_agent_with_goal_tool().await;

            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("an answer");
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = server.url();
            cfg.api_backend = xai_grok_sampling_types::ApiBackend::Responses;
            // Low is not the model default, so a fallback would show up in the assert below.
            cfg.reasoning_effort = Some(xai_grok_sampling_types::ReasoningEffort::Low);
            actor.chat_state_handle.update_sampling_config(cfg);

            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
            ]);

            actor
                .handle_side_question("what does xor mean here?", Vec::new())
                .await
                .expect("side question must succeed");

            let requests = server.requests();
            let body = requests
                .iter()
                .rev()
                .find(|r| r.path.contains("responses"))
                .and_then(|r| r.body.as_ref())
                .expect("btw body must be JSON");
            assert_eq!(
                j(j(body, "reasoning"), "effort").as_str(),
                Some("low"),
                "side question must send the session's effort, not the model default: {}",
                j(body, "reasoning")
            );
        })
        .await;
}

/// When a backend drops `prompt_cache_key`, the conv id is all that ties the call to its conversation, so it must be the parent session id.
#[tokio::test(flavor = "current_thread")]
async fn side_question_routes_on_the_session_id_when_the_key_is_not_forwarded() {
    use xai_grok_test_support::MockInferenceServer;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            *actor.agent.borrow_mut() = test_agent_with_goal_tool().await;

            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("an answer");
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = server.url();
            cfg.api_backend = xai_grok_sampling_types::ApiBackend::ChatCompletions;
            actor.chat_state_handle.update_sampling_config(cfg);

            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
            ]);

            actor
                .handle_side_question("what does xor mean here?", Vec::new())
                .await
                .expect("side question must succeed");

            let requests = server.requests();
            let req = requests.last().expect("a request must be recorded");
            let session_id = actor.session_info.id.to_string();
            assert_eq!(
                req.header("x-grok-conv-id"),
                Some(session_id.as_str()),
                "on a backend that drops the cache key the conv id must be the parent session id"
            );
            let req_id = req
                .header("x-grok-req-id")
                .expect("req id must still be sent");
            assert!(
                req_id.starts_with("xai-btw-"),
                "the btw label moves to the req id: {req_id}"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn new_prompt_cancels_in_flight_recap_epoch() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            let epoch0 = actor.recap_epoch.get();
            assert!(!actor.recap_was_cancelled(epoch0));

            actor.invalidate_side_calls_for_new_prompt();
            assert!(
                actor.recap_was_cancelled(epoch0),
                "bumping epoch cancels a recap that captured the prior value"
            );
            let epoch1 = actor.recap_epoch.get();
            assert_eq!(epoch1, epoch0.wrapping_add(1));
            assert!(
                !actor.recap_was_cancelled(epoch1),
                "a recap that captures after the bump is still live"
            );
        })
        .await;
}

/// `queue_input` for a real user prompt bumps the epoch before any await.
/// A LocalSet recap therefore cannot commit after Prompt accept but before handle_prompt.
#[tokio::test(flavor = "current_thread")]
async fn queue_input_user_prompt_bumps_recap_epoch() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            let epoch0 = actor.recap_epoch.get();
            let (respond_to, _rx) = tokio::sync::oneshot::channel();
            let _ = actor
                .queue_input(queue_input_request(vec![], "user-next", respond_to))
                .await;
            assert!(
                actor.recap_was_cancelled(epoch0),
                "user queue_input must invalidate in-flight recap epoch"
            );
        })
        .await;
}

/// A synthetic `queue_input`, sent when a background task completes, must not cancel an in-flight recap.
#[tokio::test(flavor = "current_thread")]
async fn queue_input_synthetic_does_not_bump_recap_epoch() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            let epoch0 = actor.recap_epoch.get();
            let (respond_to, _rx) = tokio::sync::oneshot::channel();
            let _ = actor
                .queue_input(queue_input_request(
                    vec![],
                    "task-completed-bg-1",
                    respond_to,
                ))
                .await;
            assert_eq!(
                actor.recap_epoch.get(),
                epoch0,
                "synthetic queue_input must leave recap epoch alone"
            );
            assert!(!actor.recap_was_cancelled(epoch0));
        })
        .await;
}

/// A second auto recap must not clear another recap's in-flight claim.
#[tokio::test(flavor = "current_thread")]
async fn skipped_auto_recap_leaves_in_flight_claim() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            actor.recap_in_flight.set(true);
            actor.handle_recap(true).await;
            assert!(
                actor.recap_in_flight.get(),
                "skipped auto recap must not clear another recap's in-flight claim"
            );
        })
        .await;
}

/// When the epoch bumps mid-flight, `try_commit_recap` advances no watermark and still clears `recap_in_flight`.
#[tokio::test(flavor = "current_thread")]
async fn try_commit_recap_cancelled_clears_in_flight_without_watermark() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            actor.last_recap_main_turn.set(2);
            actor.recap_in_flight.set(true);
            let epoch = actor.recap_epoch.get();
            actor.invalidate_side_calls_for_new_prompt();

            assert!(
                !actor.try_commit_recap(epoch, 7),
                "stale epoch must not commit"
            );
            assert_eq!(
                actor.last_recap_main_turn.get(),
                2,
                "cancelled recap must not advance watermark"
            );
            assert!(
                !actor.recap_in_flight.get(),
                "cancelled recap must clear recap_in_flight"
            );
        })
        .await;
}

/// A live epoch commits the watermark and clears `recap_in_flight`, so the emit path may proceed.
#[tokio::test(flavor = "current_thread")]
async fn try_commit_recap_live_advances_watermark() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            actor.last_recap_main_turn.set(2);
            actor.recap_in_flight.set(true);
            let epoch = actor.recap_epoch.get();

            assert!(actor.try_commit_recap(epoch, 7));
            assert_eq!(actor.last_recap_main_turn.get(), 7);
            assert!(!actor.recap_in_flight.get());
        })
        .await;
}

/// Auto cancel is silent; manual cancel emits SessionRecapUnavailable.
#[tokio::test(flavor = "current_thread")]
async fn drop_recap_after_cancel_auto_silent_manual_unavailable() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            actor.recap_in_flight.set(true);
            actor.drop_recap_after_cancel(true).await;
            assert!(!actor.recap_in_flight.get());
            assert!(
                !drained_recap_unavailable(&mut persistence_rx),
                "auto cancel must not emit SessionRecapUnavailable"
            );
            assert!(
                !drained_session_recap(&mut persistence_rx),
                "auto cancel must not emit SessionRecap"
            );

            actor.recap_in_flight.set(true);
            actor.drop_recap_after_cancel(false).await;
            assert!(!actor.recap_in_flight.get());
            assert!(
                drained_recap_unavailable(&mut persistence_rx),
                "manual cancel must emit SessionRecapUnavailable"
            );
            assert!(
                !drained_session_recap(&mut persistence_rx),
                "manual cancel must not emit SessionRecap"
            );
        })
        .await;
}

/// Drain the persistence channel and report whether a `SessionRecap` update was emitted.
fn drained_session_recap(rx: &mut tokio::sync::mpsc::UnboundedReceiver<PersistenceMsg>) -> bool {
    let mut saw = false;
    while let Ok(msg) = rx.try_recv() {
        if let PersistenceMsg::Update(crate::session::storage::SessionUpdate::Xai(n)) = msg
            && matches!(
                n.update,
                crate::extensions::notification::SessionUpdate::SessionRecap { .. }
            )
        {
            saw = true;
        }
    }
    saw
}

/// Auto recap below `MIN_TURNS_FOR_AUTO_RECAP` is a no-op and display-only.
#[tokio::test(flavor = "current_thread")]
async fn auto_recap_below_min_turns_is_noop_and_display_only() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
            ]);
            let before = actor.chat_state_handle.get_conversation().await;
            assert_eq!(
                before.len(),
                2,
                "seed must be applied before the recap call"
            );

            actor.handle_recap(true).await;

            let after = actor.chat_state_handle.get_conversation().await;
            assert_eq!(
                serde_json::to_string(&before).unwrap(),
                serde_json::to_string(&after).unwrap(),
                "a gated auto recap must not mutate the conversation"
            );
            assert!(
                persistence_rx.try_recv().is_err(),
                "a gated auto recap must emit no notification"
            );
        })
        .await;
}

/// A manual `/recap` passes the gate (when a new main turn exists) and attempts generation.
/// The test's base_url is unreachable so the model call fails.
/// Either way the conversation must be byte-identical afterwards: display-only.
#[tokio::test(flavor = "current_thread")]
async fn manual_recap_never_mutates_conversation() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
            ]);
            let before = actor.chat_state_handle.get_conversation().await;
            assert_eq!(
                before.len(),
                3,
                "seed must be applied before the recap call"
            );

            actor.handle_recap(false).await;

            let after = actor.chat_state_handle.get_conversation().await;
            assert_eq!(
                serde_json::to_string(&before).unwrap(),
                serde_json::to_string(&after).unwrap(),
                "manual recap must be display-only"
            );
        })
        .await;
}

/// Drain the persistence channel and report whether a `SessionRecapUnavailable` xAI update was emitted.
fn drained_recap_unavailable(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<PersistenceMsg>,
) -> bool {
    let mut saw = false;
    while let Ok(msg) = rx.try_recv() {
        if let PersistenceMsg::Update(crate::session::storage::SessionUpdate::Xai(n)) = msg
            && matches!(
                n.update,
                crate::extensions::notification::SessionUpdate::SessionRecapUnavailable
            )
        {
            saw = true;
        }
    }
    saw
}

/// A manual `/recap` on a brand-new session (no main turns yet) must NOT strand the client's loading spinner.
/// The gate skips before any model call, but the shell emits `SessionRecapUnavailable` instead of dropping silently, so the client can clear it.
/// This test reproduces, with no network, the bug where the spinner never cleared.
#[tokio::test(flavor = "current_thread")]
async fn manual_recap_with_no_turns_emits_unavailable() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            // No main (user) turns: the gate skips before any model call
            actor.chat_state_handle.replace_conversation(vec![]);

            actor.handle_recap(false).await;

            assert!(
                drained_recap_unavailable(&mut persistence_rx),
                "manual recap with no turns must emit SessionRecapUnavailable"
            );
        })
        .await;
}

/// A manual `/recap` whose generation fails must also emit `SessionRecapUnavailable` rather than leaving the spinner running.
/// The test's base_url is unreachable, so the prepare/model call errors.
#[tokio::test(flavor = "current_thread")]
async fn manual_recap_generation_failure_emits_unavailable() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            // One main (user) turn clears the recap gate, so the failure comes from the (unreachable) prepare/model call rather than the gate
            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
            ]);

            actor.handle_recap(false).await;

            assert!(
                drained_recap_unavailable(&mut persistence_rx),
                "a failed manual recap must emit SessionRecapUnavailable"
            );
        })
        .await;
}

/// When the recap model call is attempted (gate passes) but fails, we still persist a `RecapRequest` artifact (with `error` set) for offline replay.
/// Compaction persists its request artifacts on failure the same way.
#[tokio::test(flavor = "current_thread")]
async fn manual_recap_generation_failure_persists_request_artifact() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
            ]);

            actor.handle_recap(false).await;

            let mut saw_recap_request = false;
            while let Ok(msg) = persistence_rx.try_recv() {
                if let PersistenceMsg::RecapRequest(artifact) = msg {
                    assert_eq!(artifact.trigger, "manual");
                    assert!(
                        artifact.error.is_some(),
                        "failed recap must record error on the artifact"
                    );
                    assert!(
                        artifact.summary.is_none(),
                        "failed recap must not invent a summary"
                    );
                    assert!(
                        !artifact.chat_history.is_empty(),
                        "artifact must include the recap request items"
                    );
                    assert!(
                        artifact.x_grok_req_id.starts_with("xai-recap-"),
                        "req id: {}",
                        artifact.x_grok_req_id
                    );
                    saw_recap_request = true;
                }
            }
            assert!(
                saw_recap_request,
                "failed recap must enqueue PersistenceMsg::RecapRequest"
            );
        })
        .await;
}

/// An automatic recap below the turn gate stays silent: it shows no spinner, so it must NOT emit `SessionRecapUnavailable`.
/// That emit would be wasted wire traffic and could clear an unrelated manual spinner on another client.
#[tokio::test(flavor = "current_thread")]
async fn auto_recap_gated_does_not_emit_unavailable() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            // One main turn (below the auto min-turns gate): the auto path is gated and shows no spinner, so it must stay silent
            actor
                .chat_state_handle
                .replace_conversation(vec![ConversationItem::user("hi, nothing yet")]);

            actor.handle_recap(true).await;

            assert!(
                !drained_recap_unavailable(&mut persistence_rx),
                "a gated auto recap must not emit SessionRecapUnavailable"
            );
        })
        .await;
}

/// A recap over a huge session sends a capped transcript and leaves the conversation unmutated (display-only).
#[tokio::test(flavor = "current_thread")]
async fn manual_recap_caps_transcript_and_is_display_only() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, mut persistence_rx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;

            let mut conv = vec![ConversationItem::system("you are a coding agent")];
            for i in 0..100 {
                conv.push(ConversationItem::user(format!("question {i}")));
                conv.push(ConversationItem::assistant("x".repeat(10_000)));
            }
            actor.chat_state_handle.replace_conversation(conv);
            let before = actor.chat_state_handle.get_conversation().await;

            actor.handle_recap(false).await;

            let after = actor.chat_state_handle.get_conversation().await;
            assert_eq!(
                serde_json::to_string(&before).unwrap(),
                serde_json::to_string(&after).unwrap(),
                "a recap must not mutate the conversation"
            );

            // The model call fails (unreachable base_url), so the error arm persists the request artifact
            let mut saw_recap_request = false;
            while let Ok(msg) = persistence_rx.try_recv() {
                if let PersistenceMsg::RecapRequest(artifact) = msg {
                    let [_, transcript] = artifact.chat_history.as_slice() else {
                        panic!("recap request must be system + transcript");
                    };
                    let transcript = transcript.text_content();
                    assert!(transcript.len() < 60_000, "{}", transcript.len());
                    assert!(transcript.contains("question 99"));
                    assert!(!transcript.contains("question 0\n"));
                    saw_recap_request = true;
                }
            }
            assert!(
                saw_recap_request,
                "recap must enqueue a RecapRequest artifact"
            );
        })
        .await;
}

/// The recap sends a compact transcript to the default recap model at low effort, with no tools.
#[tokio::test(flavor = "current_thread")]
async fn recap_request_uses_small_model_and_compact_transcript() {
    use xai_grok_test_support::MockInferenceServer;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            *actor.agent.borrow_mut() = test_agent_with_goal_tool().await;

            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("You asked about the borrow checker.");
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = server.url();
            cfg.api_backend = xai_grok_sampling_types::ApiBackend::Responses;
            let session_model = cfg.model.clone();
            actor.chat_state_handle.update_sampling_config(cfg);

            let recap_slug = crate::util::config::SessionRecapConfig::default()
                .settings()
                .model;
            let recap_model = |base_url: String| {
                let mut entry = crate::agent::config::ModelEntry::fallback(
                    &recap_slug,
                    &crate::agent::config::EndpointsConfig::default(),
                );
                entry.info.supports_reasoning_effort = true;
                entry.info.api_backend = xai_grok_sampling_types::ApiBackend::Responses;
                entry.info.base_url = base_url;
                actor.models_manager.insert_test_entry(&recap_slug, entry);
            };
            let last_model = || {
                let body = server
                    .requests()
                    .into_iter()
                    .rev()
                    .find(|r| r.path.contains("responses"))
                    .and_then(|r| r.body)
                    .expect("recap body must be JSON");
                j(&body, "model").as_str().map(str::to_owned)
            };

            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::tool_result("call-1", "raw tool output"),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
            ]);

            // Listed on another endpoint: the session model writes the recap.
            recap_model("https://elsewhere.example/v1".to_owned());
            actor.handle_recap(false).await;
            assert_eq!(last_model(), Some(session_model));

            recap_model(server.url());
            actor.handle_recap(false).await;

            let requests = server.requests();
            let recap_req = requests
                .iter()
                .rev()
                .find(|r| r.path.contains("responses"))
                .expect("a responses request must be recorded");
            let conv_id = recap_req
                .header("x-grok-conv-id")
                .expect("recap must send x-grok-conv-id");
            assert!(conv_id.starts_with("recap-"), "{conv_id}");

            let body = recap_req.body.as_ref().expect("recap body must be JSON");
            assert_eq!(j(body, "model").as_str(), Some("grok-4.5"));
            assert_eq!(j(j(body, "reasoning"), "effort").as_str(), Some("low"));
            assert!(
                j(body, "tools").as_array().is_none_or(|t| t.is_empty()),
                "recap must send no tools"
            );
            let text = body.to_string();
            assert!(text.contains("User: explain the borrow checker"));
            assert!(text.contains("Agent: it enforces shared-xor-mutable"));
            assert!(!text.contains("you are a coding agent") && !text.contains("raw tool output"));
        })
        .await;
}

// ── Turn-summary task lifecycle (bail / abort-and-respawn) ──────────────

/// A queued follow-up promoted before the post-turn respawn fires is already running; a snapshot taken now would contain its user message.
/// The entry gate on `current_prompt_id` bails; that turn's completion re-fires.
/// The gate also stays inert when the feature is off.
#[tokio::test(flavor = "current_thread")]
async fn turn_summary_bails_when_newer_turn_already_running() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let disabled = std::sync::Arc::new(
                create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await,
            );
            disabled.restart_turn_summary("pid-off".into());
            assert!(
                disabled.turn_summary_task.borrow().is_none(),
                "feature off: no task spawned"
            );

            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, mut prx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.turn_summary_enabled = true;
            let actor = std::sync::Arc::new(actor);
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("pid-next".into());

            actor.restart_turn_summary("pid-done".into());

            assert!(
                actor.turn_summary_task.borrow().is_none(),
                "bailed before spawning: the running turn's completion re-fires"
            );
            assert!(
                prx.try_recv().is_err(),
                "no persistence write for a bailed generation"
            );
        })
        .await;
}

/// A real user prompt aborts an in-flight summary generation: its result would describe a conversation the prompt is about to extend.
/// (A newer completion aborts via `restart_turn_summary` the same way.)
#[tokio::test(flavor = "current_thread")]
async fn new_prompt_aborts_in_flight_turn_summary() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.turn_summary_enabled = true;

            // Stand-in for an in-flight generation: a task parked forever.
            let task = tokio::task::spawn_local(std::future::pending::<()>());
            *actor.turn_summary_task.borrow_mut() = Some(task);

            actor.invalidate_side_calls_for_new_prompt();

            assert!(
                actor.turn_summary_task.borrow().is_none(),
                "new prompt must abort the in-flight generation"
            );
        })
        .await;
}

/// Happy path: a successful side-call persists the summary and broadcasts it transiently, then clears the task slot.
#[tokio::test(flavor = "current_thread")]
async fn turn_summary_generate_persists_and_broadcasts() {
    use xai_grok_test_support::MockInferenceServer;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, mut grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, mut prx) =
                tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.turn_summary_enabled = true;
            let actor = std::sync::Arc::new(actor);

            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("Fixed the parser race; suite green");
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = server.url();
            cfg.api_backend = xai_grok_sampling_types::ApiBackend::Responses;
            actor.chat_state_handle.update_sampling_config(cfg);

            let mut summary_model = crate::agent::config::ModelEntry::fallback(
                crate::util::config::TurnSummaryConfig::default()
                    .settings()
                    .model
                    .as_str(),
                &crate::agent::config::EndpointsConfig::default(),
            );
            summary_model.info.supports_reasoning_effort = true;
            summary_model.info.base_url = server.url();
            summary_model.info.api_backend = xai_grok_sampling_types::ApiBackend::Responses;
            summary_model.api_key = Some("summary-key".into());
            actor.models_manager.insert_test_entry(
                crate::util::config::TurnSummaryConfig::default()
                    .settings()
                    .model
                    .as_str(),
                summary_model,
            );

            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("an earlier question"),
                ConversationItem::assistant("an earlier answer"),
                ConversationItem::user("fix the flaky parser test"),
                ConversationItem::tool_result("call-1", "raw tool output"),
                ConversationItem::assistant("patched the race and re-ran the suite"),
            ]);

            actor.restart_turn_summary("pid-happy".into());
            assert!(
                actor.turn_summary_task.borrow().is_some(),
                "generation task must be registered"
            );

            // Drive the LocalSet until the task finishes and clears its slot.
            for _ in 0..200 {
                if actor.turn_summary_task.borrow().is_none() {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert!(
                actor.turn_summary_task.borrow().is_none(),
                "slot must clear when generation finishes"
            );

            let mut found_persist = false;
            while let Ok(msg) = prx.try_recv() {
                if let PersistenceMsg::LastTurnSummary(Some((text, prompt_id))) = msg {
                    assert_eq!(prompt_id, "pid-happy");
                    assert!(
                        text.contains("parser") || text.contains("suite") || !text.is_empty(),
                        "summary text must be non-empty cleaned model output: {text:?}"
                    );
                    found_persist = true;
                }
            }
            assert!(found_persist, "must persist LastTurnSummary with prompt_id");

            let body = server
                .request_bodies()
                .into_iter()
                .find(|b| b.get("model").is_some())
                .expect("turn summary request");
            assert_eq!(
                body.pointer("/model").and_then(|v| v.as_str()),
                Some("grok-4.5")
            );
            assert_eq!(
                body.pointer("/reasoning/effort").and_then(|v| v.as_str()),
                Some("low")
            );
            let text = body.to_string();
            assert!(
                text.contains("fix the flaky parser test") && text.contains("patched the race")
            );
            assert!(!text.contains("an earlier") && !text.contains("raw tool output"));

            let mut found_broadcast = false;
            while let Ok(msg) = grx.try_recv() {
                let xai_acp_lib::AcpClientMessage::ExtNotification(args) = msg else {
                    continue;
                };
                if args.request.method.as_ref() != "x.ai/session_notification" {
                    continue;
                }
                let value: serde_json::Value =
                    serde_json::from_str(args.request.params.get()).expect("params json");
                let update = value.get("update").expect("update object");
                if update.get("sessionUpdate").and_then(|v| v.as_str()) != Some("last_turn_summary")
                {
                    continue;
                }
                assert_eq!(
                    update.get("prompt_id").and_then(|v| v.as_str()),
                    Some("pid-happy")
                );
                let summary = update.get("summary").and_then(|v| v.as_str()).unwrap_or("");
                assert!(!summary.is_empty(), "broadcast summary must be non-empty");
                // The transient path must not stamp eventId, the cursor a reconnect resumes from
                let meta = value.get("meta");
                assert!(
                    meta.and_then(|m| m.get("eventId")).is_none(),
                    "transient summary must omit eventId: {meta:?}"
                );
                found_broadcast = true;
            }
            assert!(found_broadcast, "must broadcast LastTurnSummary to gateway");
        })
        .await;
}

/// A `/btw` call sends the main turn's tools and the session id as `prompt_cache_key`, so it reuses the parent's cached prefix.
#[tokio::test(flavor = "current_thread")]
async fn side_question_request_rides_parent_prompt_cache() {
    use xai_grok_test_support::MockInferenceServer;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            *actor.agent.borrow_mut() = test_agent_with_goal_tool().await;

            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("The borrow checker enforces shared-xor-mutable.");
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = server.url();
            cfg.api_backend = xai_grok_sampling_types::ApiBackend::Responses;
            actor.chat_state_handle.update_sampling_config(cfg);

            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
            ]);

            let answer = actor
                .handle_side_question("what does xor mean here?", Vec::new())
                .await
                .expect("side question must succeed against the mock server");
            assert!(!answer.is_empty());

            assert!(
                server.has_responses_request(),
                "side question must hit /v1/responses"
            );
            let requests = server.requests();
            let btw_req = requests
                .iter()
                .rev()
                .find(|r| r.path.contains("responses"))
                .expect("a responses request must be recorded");

            let conv_id = btw_req
                .header("x-grok-conv-id")
                .expect("side question must send x-grok-conv-id");
            assert!(
                conv_id.starts_with("btw-"),
                "conv id keeps the btw-* label: {conv_id}"
            );

            let body = btw_req.body.as_ref().expect("btw body must be JSON");
            assert_eq!(
                j(body, "prompt_cache_key").as_str(),
                Some(actor.session_info.id.to_string().as_str()),
                "prompt_cache_key must be the parent session id for sticky routing"
            );
            let tools = j(body, "tools").as_array().expect("tools must be present");

            // The fixture registers `update_goal`, so an empty or unrelated tool list cannot pass.
            let sent: Vec<&str> = tools
                .iter()
                .filter(|t| j(t, "type") == "function")
                .map(|t| j(t, "name").as_str().unwrap_or_default())
                .collect();
            assert_eq!(
                sent,
                vec!["update_goal"],
                "side question must send the fixture's main-turn tool"
            );

            // Compare the whole array: name, description, and schema must match the main turn, in order.
            let main_turn_specs =
                actor.turn_base_tool_specs(&actor.prepare_tool_definitions().await);
            let expected: Vec<serde_json::Value> = main_turn_specs
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "type": "function",
                        "name": s.name,
                        "description": s.description,
                        "parameters": s.parameters,
                    })
                })
                .collect();
            assert_eq!(
                tools, &expected,
                "side question tools must equal the main turn's specs verbatim"
            );

            // The main turn sends no hosted search here, so the side question must not add one.
            assert!(
                actor.hosted_tools_for_turn().is_empty(),
                "fixture must have backend search off"
            );
            assert!(
                !tools.iter().any(|t| j(t, "type") != "function"),
                "no hosted tools may be added to a side question the main turn would not send: {tools:?}"
            );
        })
        .await;
}

/// `/btw` must replay the parent conversation verbatim and append one instruction. The cache key buys nothing if the prefix moved.
#[tokio::test(flavor = "current_thread")]
async fn auxiliary_calls_keep_the_main_turn_prefix() {
    use xai_grok_test_support::MockInferenceServer;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            *actor.agent.borrow_mut() = test_agent_with_goal_tool().await;

            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("a summary");
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            // apex-ayl.69: the egress cell is the session model; capture before the config moves.
            let session_model = cfg.model.clone();
            cfg.base_url = server.url();
            cfg.api_backend = xai_grok_sampling_types::ApiBackend::Responses;
            actor.chat_state_handle.update_sampling_config(cfg);

            // The Responses backend keeps reasoning, so it belongs in the prefix both calls have to reproduce.
            let parent = vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::Reasoning(xai_grok_sampling_types::synthesized_reasoning_item(
                    "recalling the aliasing rules",
                ).into()),
                ConversationItem::assistant("it enforces shared-xor-mutable"),
            ];
            actor.chat_state_handle.replace_conversation(parent.clone());

            actor
                .handle_side_question("what does xor mean here?", Vec::new())
                .await
                .expect("side question must succeed");
            let requests = server.requests();
            let btw_body = requests
                .iter()
                .rev()
                .find(|r| r.path.contains("responses"))
                .and_then(|r| r.body.as_ref())
                .expect("btw body must be JSON");
            assert_rides_parent_prefix(btw_body, parent, "/btw", &session_model);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn messages_side_calls_preserve_completed_reasoning() {
    use xai_grok_sampling_types::{ReasoningEffort, rs};
    use xai_grok_test_support::MockInferenceServer;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            actor.title_refresh_enabled = true;
            *actor.agent.borrow_mut() = test_agent_with_goal_tool().await;
            let actor = std::sync::Arc::new(actor);

            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("a short summary");
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = server.url();
            cfg.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
            cfg.reasoning_effort = Some(ReasoningEffort::High);
            actor.chat_state_handle.update_sampling_config(cfg);

            let reasoning = |turn: usize| {
                ConversationItem::Reasoning(rs::ReasoningItem {
                    // `Some("")` matches what the persist seam stores (`conversation.rs:2399-2403`);
                    // `None` would serialize `"id": null` where the fork wrote `"id": ""`.
                    id: Some(String::new()),
                    summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                        text: format!("thinking for turn {turn}"),
                    })],
                    content: None,
                    encrypted_content: Some(format!("signature-{turn}")),
                    status: None,
                }.into())
            };
            let parent = vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("first question"),
                reasoning(1),
                ConversationItem::assistant("first answer"),
                ConversationItem::user("second question"),
                reasoning(2),
                ConversationItem::assistant("second answer"),
                ConversationItem::user("third question"),
                reasoning(3),
                ConversationItem::assistant("third answer"),
            ];
            actor.chat_state_handle.replace_conversation(parent.clone());

            actor
                .handle_side_question("what matters most?", Vec::new())
                .await
                .expect("side question must succeed");
            let body = server
                .requests()
                .into_iter()
                .rev()
                .find(|request| request.path == "/v1/messages")
                .and_then(|request| request.body)
                .expect("/btw Messages body");
            assert_messages_rides_parent_prefix(&body, parent.clone(), "/btw");

            actor.maybe_refresh_title();
            for _ in 0..200 {
                if actor.title_refresh_task.borrow().is_none() {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert!(
                actor.title_refresh_task.borrow().is_none(),
                "title refresh must finish"
            );
            let body = server
                .requests()
                .into_iter()
                .rev()
                .find(|request| request.path == "/v1/messages")
                .and_then(|request| request.body)
                .expect("title-refresh Messages body");
            assert_messages_rides_parent_prefix(&body, parent, "title refresh");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn messages_side_calls_strip_reasoning_without_supported_thinking_effort() {
    use xai_grok_sampling_types::{ReasoningEffort, synthesized_reasoning_item};
    use xai_grok_test_support::MockInferenceServer;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            for reasoning_effort in [
                None,
                Some(ReasoningEffort::None),
                Some(ReasoningEffort::Minimal),
            ] {
                let (gateway_tx, _grx) =
                    tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
                let (persistence_tx, _prx) =
                    tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
                let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
                actor.title_refresh_enabled = true;
                *actor.agent.borrow_mut() = test_agent_with_goal_tool().await;
                let actor = std::sync::Arc::new(actor);

                let server = MockInferenceServer::start().await.unwrap();
                server.set_response("a short summary");
                let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
                cfg.base_url = server.url();
                cfg.api_backend = xai_grok_sampling_types::ApiBackend::Messages;
                cfg.reasoning_effort = reasoning_effort;
                actor.chat_state_handle.update_sampling_config(cfg);

                let parent = vec![
                    ConversationItem::system("you are a coding agent"),
                    ConversationItem::user("first question"),
                    ConversationItem::Reasoning(synthesized_reasoning_item("signed thinking").into()),
                    ConversationItem::assistant("first answer"),
                    ConversationItem::user("second question"),
                    ConversationItem::Reasoning(synthesized_reasoning_item("more signed thinking").into()),
                    ConversationItem::assistant("second answer"),
                    ConversationItem::user("third question"),
                    ConversationItem::assistant("third answer"),
                ];
                actor.chat_state_handle.replace_conversation(parent);

                actor
                    .handle_side_question("what matters most?", Vec::new())
                    .await
                    .expect("side question must succeed");
                let body = server
                    .requests()
                    .into_iter()
                    .rev()
                    .find(|request| request.path == "/v1/messages")
                    .and_then(|request| request.body)
                    .expect("/btw Messages body");
                assert_messages_reasoning_stripped(&body, "/btw");

                actor.handle_recap(false).await;
                let body = server
                    .requests()
                    .into_iter()
                    .rev()
                    .find(|request| request.path == "/v1/messages")
                    .and_then(|request| request.body)
                    .expect("recap Messages body");
                assert_messages_reasoning_stripped(&body, "recap");

                actor.maybe_refresh_title();
                for _ in 0..200 {
                    if actor.title_refresh_task.borrow().is_none() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                assert!(
                    actor.title_refresh_task.borrow().is_none(),
                    "title refresh must finish"
                );
                let body = server
                    .requests()
                    .into_iter()
                    .rev()
                    .find(|request| request.path == "/v1/messages")
                    .and_then(|request| request.body)
                    .expect("title-refresh Messages body");
                assert_messages_reasoning_stripped(&body, "title refresh");
            }
        })
        .await;
}

/// A mid-turn `/btw` must not send a reasoning item whose assistant the trim removed, or the request goes out with an unpaired prefix.
#[tokio::test(flavor = "current_thread")]
async fn side_question_trims_reasoning_orphaned_by_mid_turn_truncation() {
    use xai_grok_sampling_types::conversation::{AssistantItem, ToolCall};
    use xai_grok_test_support::MockInferenceServer;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _grx) =
                tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
            *actor.agent.borrow_mut() = test_agent_with_goal_tool().await;

            let server = MockInferenceServer::start().await.unwrap();
            server.set_response("xor means one or the other, not both.");
            let mut cfg = actor.chat_state_handle.get_sampling_config().await.unwrap();
            cfg.base_url = server.url();
            // Responses backend keeps reasoning, which is what creates the orphan.
            cfg.api_backend = xai_grok_sampling_types::ApiBackend::Responses;
            actor.chat_state_handle.update_sampling_config(cfg);

            // Mid-turn shape: the tool call is still in flight, so the reasoning before it has no result behind it.
            actor.chat_state_handle.replace_conversation(vec![
                ConversationItem::system("you are a coding agent"),
                ConversationItem::user("explain the borrow checker"),
                ConversationItem::Reasoning(xai_grok_sampling_types::synthesized_reasoning_item(
                    "planning the file read",
                ).into()),
                ConversationItem::Assistant(AssistantItem {
                    content: String::new().into(),
                    tool_calls: vec![ToolCall {
                        id: "tc1".into(),
                        name: "read_file".into(),
                        arguments: "{}".into(),
                    }],
                    model_id: None,
                    model_fingerprint: None,
                    reasoning_effort: None,
                }),
            ]);

            actor
                .handle_side_question("what does xor mean here?", Vec::new())
                .await
                .expect("side question must succeed against the mock server");

            let requests = server.requests();
            let btw_req = requests
                .iter()
                .rev()
                .find(|r| r.path.contains("responses"))
                .expect("a responses request must be recorded");
            let body = btw_req.body.as_ref().expect("btw body must be JSON");
            let input = j(body, "input").as_array().expect("input must be present");

            let kinds: Vec<&str> = input
                .iter()
                .map(|i| j(i, "type").as_str().unwrap_or("message"))
                .collect();
            assert!(
                !kinds.contains(&"reasoning"),
                "reasoning orphaned by the mid-turn trim must not be sent: {kinds:?}"
            );
            assert!(
                !kinds.contains(&"function_call"),
                "the in-flight tool call must be trimmed: {kinds:?}"
            );
        })
        .await;
}

/// Side calls persist text without executing tools, so the shared builder must pin `Fail` rather than inherit the default that salvages tool calls.
#[tokio::test]
async fn parent_cached_request_pins_fail_length_policy() {
    let local = tokio::task::LocalSet::new();
    let (actor, _gateway_rx) = local.run_until(build_actor()).await;
    let request = actor.parent_cached_request(super::side_call::AuxCall {
        items: Vec::new(),
        tools: Vec::new(),
        hosted_tools: Vec::new(),
        model: "test-model".to_string(),
        supports_search_tool: false,
        reasoning_effort: None,
        ultra_wire_effort: None,
        backend: crate::sampling::ApiBackend::Messages,
        conv_id: "conv".to_string(),
        req_id: "req".to_string(),
    });
    assert_eq!(
        request.length_policy,
        xai_grok_sampling_types::LengthPolicy::Fail
    );
}

// Effective configuration caches its home; isolate the suggestion fixture before that cache is read.
fn auxiliary_recap_isolated(name: &str) -> bool {
    const CHILD: &str = "GROK_TEST_U6_AUXILIARY_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(name) {
        return false;
    }
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[ui]\nprompt_suggestions = true\n[prompt_suggestions]\nreasoning_effort = \"high\"\nmax_output_tokens = 72\n").unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .env(CHILD, name)
        .env("GROK_HOME", home.path())
        .env_remove("GROK_CONFIG")
        .env_remove("GROK_CONFIG_PATH")
        .env_remove("GROK_CAMPAIGNS")
        .env_remove("GROK_CAMPAIGNS_OVERRIDE")
        .env_remove("XAI_API_KEY")
        .env_remove("APEX_LLM_API_KEY")
        .args(["--exact", name, "--nocapture"])
        .stdin(std::process::Stdio::null());
    xai_tty_utils::detach_std_command(&mut command);
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("running 1 test"),
        "the isolated selector must select one test"
    );
    assert!(
        output.status.success(),
        "isolated auxiliary behavioral assertion failed: {stderr}"
    );
    assert!(
        stdout.contains(&format!("test {name} ... ok")),
        "the isolated selector must pass its named test"
    );
    true
}

struct AuxiliaryRefreshFailure(std::sync::Arc<std::sync::atomic::AtomicUsize>);

#[async_trait::async_trait]
impl xai_grok_login::refresh::TokenRefresher for AuxiliaryRefreshFailure {
    async fn refresh(
        &self,
        reason: xai_grok_login::refresh::RefreshReason,
    ) -> xai_grok_login::refresh::RefreshOutcome {
        assert_eq!(reason, xai_grok_login::refresh::RefreshReason::PreRequest);
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        xai_grok_login::refresh::RefreshOutcome::TransientFailure {
            message: "u6 synthetic refresh unavailable".into(),
        }
    }
}

fn auxiliary_suggest_manager(home: &std::path::Path) -> crate::agent::remote_config::ModelsManager {
    let auth = std::sync::Arc::new(xai_grok_login::AuthManager::new(
        home,
        xai_grok_login::GrokComConfig::default(),
    ));
    let mut config = crate::agent::config::Config::default();
    config.prompt_suggest_model_pin =
        crate::config::PromptSuggestModelPin::Pinned("u6-suggest-pinned".into());
    crate::agent::remote_config::ModelsManager::new(
        None,
        indexmap::indexmap! {
            "u6-row-a".into() => entry_with_search_flag("u6-row-a", true),
            "u6-row-b".into() => entry_with_search_flag("u6-row-b", false),
            "u6-suggest-pinned".into() => entry_with_search_flag("u6-suggest-pinned", false),
        },
        agent_client_protocol::ModelId::new("u6-row-a"),
        auth,
        config,
    )
}

#[tokio::test(flavor = "current_thread")]
async fn recap_client_and_request_keep_captured_row() {
    const NAME: &str = "session::acp_session::recap_display_only_tests::recap_client_and_request_keep_captured_row";
    if auxiliary_recap_isolated(NAME) {
        return;
    }
    use super::side_call::tests::{
        AuxiliaryGuard, assert_auxiliary_binding, assert_auxiliary_wire, auxiliary_fixture,
    };
    use xai_grok_sampling_types::{ApiBackend, ReasoningEffort, SearchAdmission};
    tokio::task::LocalSet::new().run_until(async {
        for call in ["recap", "btw", "ai-default", "ai-override", "prompt-api", "prompt-session"] {
            let mut fixture = auxiliary_fixture(ApiBackend::Responses, true).await;
            let before = fixture.actor.chat_state_handle.get_conversation().await;
            let specs = fixture.actor.turn_base_tool_specs(&fixture.actor.prepare_tool_definitions().await);
            let hosted = fixture.actor.hosted_tools_for_turn();
            let is_prompt = call.starts_with("prompt-");
            let refresh_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            if is_prompt {
                fixture.actor.models_manager = auxiliary_suggest_manager(fixture.home.path());
                let mut pinned = entry_with_search_flag("u6-suggest-pinned", false);
                pinned.info.supports_reasoning_effort = true;
                fixture.actor.models_manager.insert_test_entry("u6-suggest-pinned", pinned);
            }
            if call == "prompt-session" {
                let manager = std::sync::Arc::new(xai_grok_login::AuthManager::new(fixture.home.path(), xai_grok_login::GrokComConfig::default()));
                manager.hot_swap(xai_grok_login::GrokAuth {
                    key: "u6-expired-bearer".into(), auth_mode: xai_grok_login::AuthMode::Oidc,
                    refresh_token: Some("u6-refresh-token".into()),
                    expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
                    ..xai_grok_login::GrokAuth::test_default()
                });
                manager.set_refresher(std::sync::Arc::new(AuxiliaryRefreshFailure(refresh_count.clone())));
                fixture.actor.auth_manager = Some(manager);
                fixture.actor.auth_method_id = test_auth_method_id("cached_token");
                fixture.actor.model_auth_memo.replace(Some(ModelAuthMemo {
                    model_id: fixture.a.config.model.clone(),
                    facts: crate::agent::config::ModelAuthFacts {
                        byok: crate::agent::auth_method::ModelByok::NotByok,
                        auth_scheme: xai_grok_sampler::AuthScheme::Bearer,
                    }, provider: None,
                }));
            }
            let expected_suggest = if is_prompt {
                let mut config = fixture.actor.assemble_sampler_config_without_refresh(&fixture.a).await;
                config.model = "u6-suggest-pinned".into();
                config.reasoning_effort = None;
                fixture.actor.models_manager.apply_supported_effort(&mut config, Some(ReasoningEffort::High), &fixture.actor.session_info.id, crate::sampling::EffortTarget::SummaryClient);
                Some(config)
            } else { None };
            fixture.actor.models_manager.insert_test_entry("u6-row-a", entry_with_search_flag("u6-row-a", false));
            let guard = AuxiliaryGuard::new(fixture.b.clone());
            guard.count_refreshes(refresh_count.clone());
            let expected_model = match call {
                "recap" => {
                    fixture.actor.handle_recap(false).await;
                    "u6-row-a"
                }
                "btw" => {
                    assert!(fixture.actor.handle_side_question("What matters?").await.is_ok());
                    "u6-row-a"
                }
                "ai-default" => {
                    assert!(fixture.actor.handle_ai_suggest("git st", "/u6", None).await.is_some());
                    "grok-4.6"
                }
                "ai-override" => {
                    assert!(fixture.actor.handle_ai_suggest("git st", "/u6", Some("u6-ai-override")).await.is_some());
                    "u6-ai-override"
                }
                "prompt-api" | "prompt-session" => {
                    assert!(fixture.actor.handle_suggest_prompt(Some("u6-row-b")).await.is_some());
                    "u6-suggest-pinned"
                }
                _ => unreachable!(),
            };
            let sends = guard.sends();
            assert_eq!(sends.len(), 1, "one actual auxiliary request per caller");
            let request = &sends[0].1;
            assert_eq!(request.model.as_deref(), Some(expected_model), "recap and suggestions must retain captured request authority");
            if matches!(call, "recap" | "btw") {
                assert_eq!((serde_json::to_value(&request.tools).unwrap(), &request.hosted_tools), (serde_json::to_value(&specs).unwrap(), &hosted), "complete prepared tool surfaces must remain unchanged");
                assert_eq!(request.search_admission, Some(SearchAdmission::for_row(true, &specs)));
                assert_eq!((request.reasoning_effort, request.ultra_wire_effort), (fixture.a.config.reasoning_effort, fixture.a.config.ultra_wire_effort));
                assert_rides_parent_prefix(fixture.server_a.requests()[0].body.as_ref().unwrap(), before.clone(), call, &fixture.a.config.model);
                assert_eq!(request.length_policy, xai_grok_sampling_types::LengthPolicy::Fail);
            } else {
                assert_eq!((serde_json::to_value(&request.tools).unwrap(), &request.hosted_tools), (serde_json::json!([]), &Vec::new()));
            }
            if let Some(expected) = expected_suggest {
                let configs = guard.suggest_configs();
                assert_eq!(configs.len(), 2);
                assert_eq!(serde_json::to_value(&configs[1]).unwrap(), serde_json::to_value(&expected).unwrap(), "suggest config keeps captured transport before deliberate model and effort overrides");
                assert!(std::sync::Arc::ptr_eq(configs[0].header_injector.as_ref().unwrap(), configs[1].header_injector.as_ref().unwrap()), "suggest edits retain the assembled header injector");
                assert!(std::sync::Arc::ptr_eq(configs[0].attribution_callback.as_ref().unwrap(), configs[1].attribution_callback.as_ref().unwrap()), "suggest edits retain the attribution callback");
                assert!(std::sync::Arc::ptr_eq(configs[1].attribution_callback.as_ref().unwrap(), fixture.actor.attribution_callback.as_ref().unwrap()));
                match (&configs[0].bearer_resolver, &configs[1].bearer_resolver) {
                    (Some(before), Some(after)) => assert!(std::sync::Arc::ptr_eq(before, after), "suggest edits retain the live resolver identity"),
                    (None, None) => {}
                    (Some(_), None) | (None, Some(_)) => panic!("suggest edits changed resolver presence"),
                }
                assert_eq!(configs[1].bearer_resolver.is_some(), call == "prompt-session");
                assert_eq!((request.reasoning_effort, request.ultra_wire_effort), (expected.reasoning_effort, expected.ultra_wire_effort));
                assert_eq!(request.max_output_tokens, Some(328));
                assert_eq!(guard.refreshes_before_send(), vec![if call == "prompt-session" { 2 } else { 0 }], "suggest preserves one preflight plus one gated reconstruction without extra preparation");
                assert_eq!(refresh_count.load(std::sync::atomic::Ordering::SeqCst), if call == "prompt-session" { 2 } else { 0 }, "a hard-expired bearer must not drive another per-send refresh");
            }
            if call == "prompt-session" {
                assert!(fixture.server_b.requests().is_empty());
                let requests = fixture.server_a.requests();
                assert_eq!(requests.len(), 1);
                assert_eq!(requests[0].path, "/v1/responses");
                assert!(requests[0].authorization.is_none(), "expired bearer must not be seeded");
                assert_eq!(requests[0].body.as_ref().unwrap()["model"], expected_model);
            } else {
                assert_auxiliary_wire(&fixture, expected_model);
            }
            assert_eq!(serde_json::to_vec(&fixture.actor.chat_state_handle.get_conversation().await).unwrap(), serde_json::to_vec(&before).unwrap(), "auxiliary calls remain display-only");
            assert_auxiliary_binding(&fixture.actor.chat_state_handle.capture_transport_binding().await.unwrap(), &fixture.b);
        }
    }).await;
}

#[derive(Debug, Clone)]
struct RecapMcpTool {
    name: &'static str,
    description: &'static str,
}

impl xai_grok_tools::types::tool_metadata::ToolMetadata for RecapMcpTool {
    fn kind(&self) -> xai_grok_tools::types::tool::ToolKind {
        xai_grok_tools::types::tool::ToolKind::Other
    }
    fn tool_namespace(&self) -> xai_grok_tools::types::tool::ToolNamespace {
        xai_grok_tools::types::tool::ToolNamespace::MCP
    }
    fn description_template(&self) -> &str {
        self.description
    }
}

impl xai_tool_runtime::Tool for RecapMcpTool {
    type Args = serde_json::Value;
    type Output = xai_grok_tools::types::output::ToolOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        xai_tool_protocol::ToolId::new(self.name).unwrap()
    }
    fn description(
        &self,
        _: &xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(self.name, self.description)
    }
    async fn run(
        &self,
        _: xai_tool_runtime::ToolCallContext,
        _: Self::Args,
    ) -> Result<Self::Output, xai_tool_runtime::ToolError> {
        panic!("recap declaration fixtures never execute tools")
    }
}

fn register_recap_mcp(actor: &SessionActor) -> Vec<xai_grok_sampling_types::ToolSpec> {
    let mut specs = Vec::new();
    for (name, description, schema) in [
        (
            "zeta__fetch",
            "Recap URL lookup",
            serde_json::json!({"type":"object","properties":{"url":{"type":"string"},"limit":{"type":"integer"}},"required":["url"],"additionalProperties":false}),
        ),
        (
            "alpha__lookup",
            "Recap query lookup",
            serde_json::json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false}),
        ),
    ] {
        actor
            .agent
            .borrow()
            .tool_bridge()
            .register_mcp_tools(
                name.into(),
                RecapMcpTool { name, description },
                Some(schema.clone()),
            )
            .unwrap();
        specs.push(xai_grok_sampling_types::ToolSpec {
            name: name.into(),
            description: Some(description.into()),
            parameters: schema,
            exposure: xai_grok_sampling_types::ToolExposure::Deferred,
        });
    }
    specs.reverse();
    specs
}

#[tokio::test(flavor = "current_thread")]
async fn auxiliary_mcp_selection_keeps_builtin_snapshot_and_captured_row() {
    use super::side_call::tests::{
        AuxiliaryGuard, assert_auxiliary_binding, assert_auxiliary_wire, auxiliary_fixture,
    };
    use xai_grok_sampling_types::{ApiBackend, SearchAdmission, ToolExposure};
    tokio::task::LocalSet::new().run_until(async {
        for call in ["btw", "recap"] {
            for (backend, flag, next_backend, next_flag) in [
                (ApiBackend::Messages, true, ApiBackend::Messages, false),
                (ApiBackend::Messages, true, ApiBackend::Responses, false),
                (ApiBackend::Messages, false, ApiBackend::Messages, true),
                (ApiBackend::Responses, true, ApiBackend::Messages, false),
                (ApiBackend::Responses, false, ApiBackend::Messages, true),
                (ApiBackend::ChatCompletions, true, ApiBackend::Messages, false),
                (ApiBackend::ChatCompletions, false, ApiBackend::Messages, true),
            ] {
                for backend_search in [false, true] {
                    let mut fixture = auxiliary_fixture(backend.clone(), flag).await;
                    let mut control = auxiliary_fixture(backend.clone(), flag).await;
                    for actor in [&fixture.actor, &control.actor] {
                        use xai_grok_tools::implementations::grok_build::{update_goal::UpdateGoalTool, web_search::WebSearchTool};
                        use xai_grok_tools::registry::types::ToolConfig;
                        let agent = support::test_agent_with_tools(vec![
                            ToolConfig::for_tool::<UpdateGoalTool>(),
                            ToolConfig::for_tool::<WebSearchTool>(),
                        ]).await;
                        *actor.agent.borrow_mut() = xai_grok_agent::Agent::new(
                            agent.definition().clone(), agent.prompt_context().clone(),
                            agent.system_prompt().to_string(), std::sync::Arc::clone(agent.tool_bridge()),
                            agent.reminder_policy().clone(), agent.compaction_policy().clone(),
                            vec![xai_grok_sampling_types::HostedTool::WebSearch { options: None }], true,
                        );
                    }
                    fixture.b.config.api_backend = next_backend.clone();
                    fixture.b.config.supports_search_tool = next_flag;
                    fixture.actor.supports_backend_search.set(backend_search);
                    control.actor.supports_backend_search.set(backend_search);
                    control.actor.session_info.id = fixture.actor.session_info.id.clone();
                    let mcp = register_recap_mcp(&fixture.actor);
                    let history = fixture.actor.chat_state_handle.get_conversation().await;
                    let baseline_guard = AuxiliaryGuard::new(control.b.clone());
                    match call {
                        "btw" => { control.actor.handle_side_question("What matters?").await.unwrap(); }
                        "recap" => control.actor.handle_recap(false).await,
                        _ => unreachable!(),
                    }
                    let baseline_sends = baseline_guard.sends();
                    assert_eq!(baseline_sends.len(), 1);
                    assert_eq!(baseline_sends[0].0, call);
                    let baseline = baseline_sends[0].1.clone();
                    drop(baseline_guard);
                    assert!(baseline.tools.iter().any(|tool| tool.exposure == ToolExposure::Deferred));
                    assert_eq!(baseline.tools.iter().any(|tool| tool.name == "web_search"), !backend_search);
                    assert_eq!(!baseline.hosted_tools.is_empty(), backend_search);
                    let guard = AuxiliaryGuard::new(fixture.b.clone());
                    match call {
                        "btw" => { fixture.actor.handle_side_question("What matters?").await.unwrap(); }
                        "recap" => fixture.actor.handle_recap(false).await,
                        _ => unreachable!(),
                    }
                    let sends = guard.sends();
                    assert_eq!(sends.len(), 1);
                    assert_eq!(sends[0].0, call);
                    let request = &sends[0].1;
                    let eligible = backend == ApiBackend::Messages && flag;
                    let expected_tools: Vec<_> = baseline.tools.iter().cloned()
                        .chain(if eligible { mcp.clone() } else { Vec::new() }).collect();
                    let non_mcp: Vec<_> = request.tools.iter().filter(|tool| !mcp.iter().any(|m| m.name == tool.name)).cloned().collect();
                    assert_eq!((serde_json::to_value(&non_mcp).unwrap(), &request.hosted_tools),
                        (serde_json::to_value(&baseline.tools).unwrap(), &baseline.hosted_tools),
                        "U6 actual recap and side-question sends preserve the non-MCP preparation snapshot");
                    assert_eq!(serde_json::to_value(&request.tools).unwrap(), serde_json::to_value(&expected_tools).unwrap(),
                        "U6 actual auxiliary sends must select exact qualified MCP schemas using captured A");
                    assert_eq!(request.search_admission, Some(SearchAdmission::for_row(flag, &expected_tools)),
                        "U6 actual auxiliary admission must use captured A, not installed B or the catalog");
                    let mut expected_request = baseline.clone();
                    expected_request.tools = expected_tools;
                    expected_request.x_grok_conv_id = request.x_grok_conv_id.clone();
                    expected_request.x_grok_req_id = request.x_grok_req_id.clone();
                    assert_eq!(format!("{request:?}"), format!("{expected_request:?}"),
                        "U6 complete auxiliary request differs only by selected MCP and fresh request IDs");
                    assert_auxiliary_wire(&fixture, &fixture.a.config.model);
                    assert_auxiliary_wire(&control, &control.a.config.model);
                    let body = fixture.server_a.requests()[0].body.clone().unwrap();
                    let baseline_body = control.server_a.requests()[0].body.clone().unwrap();
                    if eligible {
                        let baseline_tools = baseline_body["tools"].as_array().unwrap();
                        let mut expected_wire = baseline_tools.clone();
                        expected_wire.splice(baseline.tools.len()..baseline.tools.len(), mcp.iter().map(|spec| serde_json::json!({
                            "name": spec.name, "description": spec.description,
                            "input_schema": spec.parameters, "defer_loading": true,
                        })));
                        assert_eq!(body["tools"], serde_json::to_value(expected_wire).unwrap(),
                            "U6 actual auxiliary Messages wire preserves the builtin prefix, MCP schemas and native tail");
                    } else {
                        assert_eq!(body["tools"], baseline_body["tools"], "U6 auxiliary excluded rows preserve the isolated builtin wire surface");
                    }
                    if backend == ApiBackend::Responses {
                        assert_rides_parent_prefix(&body, history.clone(), call, &fixture.a.config.model);
                    }
                    assert_eq!(request.length_policy, xai_grok_sampling_types::LengthPolicy::Fail);
                    assert_eq!(request.prompt_cache_key.as_deref(), Some(fixture.actor.session_info.id.0.as_ref()));
                    assert_eq!(serde_json::to_value(fixture.actor.chat_state_handle.get_conversation().await).unwrap(), serde_json::to_value(&history).unwrap(),
                        "U6 actual auxiliary sends remain display-only");
                    assert_auxiliary_binding(&fixture.actor.chat_state_handle.capture_transport_binding().await.unwrap(), &fixture.b);
                }
            }
        }
    }).await;
}
