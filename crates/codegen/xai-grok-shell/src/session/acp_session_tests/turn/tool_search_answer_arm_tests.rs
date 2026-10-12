//! Coverage of the H2 answer arm (apex-waj.57, D17): the turn loop answers the
//! client-executed `tool_search_call` the provider minted and stopped on.
//! R7b — the loop re-enters the model (a second request is issued) — and the
//! production e2e — that second request carries the answered pair on the
//! Codex-dialect wire bytes. Plus the R17-a per-turn budget and the R17-b
//! per-`call_id` idempotence, both observed on the wire rather than on state.

use super::rate_limit_backoff_tests::{SessionKind, actor_under_test, pump_local_tasks};
use super::support::test_grok_build_agent_with_todo;
use super::*;
use std::time::Duration;
use xai_grok_test_support::{
    MockInferenceServer, MockModelEntry, ScriptedResponse,
    sse::{responses_api_completed_only_events, with_terminal_output_items},
};

/// The turn future needs a session-sized stack (spawn.rs: 8 MiB); default test
/// stacks overflow.
fn on_session_stack(test: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(test)
        .expect("spawn test thread")
        .join()
        .expect("test thread panicked");
}

fn run_paused<F: std::future::Future>(fut: impl FnOnce() -> F) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .expect("test runtime");
    let local = tokio::task::LocalSet::new();
    rt.block_on(local.run_until(async move {
        fut().await;
    }));
}

/// No sampler-internal retries: request counts map 1:1 to submissions, so a
/// delta of exactly two is exactly "the arm continued once".
fn sampler_no_retries() -> xai_grok_sampler::RetryPolicy {
    xai_grok_sampler::RetryPolicy {
        max_retries: 0,
        ..Default::default()
    }
}

/// The banked client-executed call (`probe2-client-full.raw`, the only
/// client-executed call in the corpus): the provider stops after minting the
/// call; the answer is the harness's to author.
fn lone_client_call_item(call_id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": format!("tsc_{call_id}"),
        "arguments": { "query": "shipping ETA lookup by order ID", "limit": 5 },
        "call_id": call_id,
        "execution": "client",
        "status": "completed",
        "type": "tool_search_call"
    })
}

/// A canned discovery-only response: `response.completed` whose `output`
/// carries only the given client-executed call(s). The terminal `output`
/// array is what the stream decoder hands to the decode seam
/// (`xai-grok-sampler/src/stream/responses.rs:821`).
fn discovery_only_response(items: &[serde_json::Value]) -> ScriptedResponse {
    let events =
        with_terminal_output_items(responses_api_completed_only_events("test"), items.to_vec());
    ScriptedResponse::sse(events)
}

/// Seed the catalog so the per-turn config reconstruction resolves the live
/// model to the Codex family (`sampler_turn.rs:682`), which is what makes the
/// per-request client splice discovery items raw into the input (R5).
async fn seed_codex_family(actor: &Arc<SessionActor>) {
    let mut entry = crate::agent::config::ModelEntry::fallback(
        "test",
        &crate::agent::config::EndpointsConfig::default(),
    );
    entry.info.model_family = Some("codex".to_string());
    entry.info.api_backend = crate::sampling::ApiBackend::Responses;
    actor
        .models_manager
        .insert_test_entry("codex-test-catalog", entry);
    actor
        .models_manager
        .set_current_model_id(acp::ModelId::new("codex-test-catalog"));
}

async fn run_discovery_turn(
    server: &MockInferenceServer,
    actor: &Arc<SessionActor>,
    request_id: &str,
) -> Result<TurnOutcome, agent_client_protocol::Error> {
    tokio::time::timeout(
        Duration::from_secs(300),
        actor.process_conversation_turn_with_recovery(
            request_id,
            None,
            None,
            None,
            &mut length_salvage::LengthSalvage::new(None),
            &mut Default::default(),
        ),
    )
    .await
    .expect("turn must finish within timeout")
}

/// The second `/v1/responses` body's `input` array — the wire bytes the next
/// model call actually reads.
fn second_request_input(server: &MockInferenceServer) -> Vec<serde_json::Value> {
    assert_eq!(
        server.request_count_for("/v1/responses"),
        2,
        "exactly one re-entry: the discovery-only response and the arm's follow-up"
    );
    let bodies = server
        .request_bodies()
        .into_iter()
        .filter(|body| body.get("input").is_some())
        .collect::<Vec<_>>();
    let second = bodies.last().expect("both requests carry an input array");
    second["input"]
        .as_array()
        .expect("the Responses request input is an array")
        .clone()
}

fn shipping_tool_definition() -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "name": "shipping__lookup_eta",
        "description": "shipping ETA lookup by order ID",
        "parameters": {
            "type": "object",
            "properties": {"order_id": {"type": "string"}},
            "required": ["order_id"],
            "additionalProperties": false
        }
    })
}

fn seed_searchable_mcp(actor: &SessionActor) {
    use crate::session::tool_index::{ServerMetadata, ToolMetadata};
    let definition = shipping_tool_definition();
    let mut snapshot = actor.tool_metadata_snapshot.lock().unwrap();
    snapshot.tools = vec![ToolMetadata {
        qualified_name: "shipping__lookup_eta".to_string(),
        server_name: "shipping".to_string(),
        tool_name: "lookup_eta".to_string(),
        description: definition["description"].as_str().unwrap().to_string(),
        parameters: vec!["order_id".to_string()],
        input_schema: definition["parameters"].clone(),
    }];
    snapshot.servers = vec![ServerMetadata {
        name: "shipping".to_string(),
        description: Some("Shipping ETA lookup".to_string()),
    }];
    snapshot.mcp_initialized = true;
}

async fn install_ordinary_tool(actor: &SessionActor) {
    let agent = test_grok_build_agent_with_todo().await;
    *actor.agent.borrow_mut() = agent;
    actor
        .workspace_ops
        .bind_local_session(
            &actor.session_id_string(),
            actor.tool_context.cwd.as_path().to_path_buf(),
            actor.tool_context.hunk_tracker_handle.clone(),
            actor.agent.borrow().tool_bridge().toolset(),
            None,
        )
        .expect("bind the ordinary built-in tool");
}

fn ordinary_call_item(call_id: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "function_call",
        "id": format!("fc_{call_id}"),
        "call_id": call_id,
        "name": "todo_write",
        "arguments": serde_json::json!({
            "merge": false,
            "todos": [{
                "id": call_id,
                "content": "paired ordinary dispatch",
                "status": "completed"
            }]
        }).to_string(),
        "status": "completed"
    })
}

fn provider_answer_item(call_id: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "tool_search_output",
        "id": format!("tso_{call_id}"),
        "call_id": call_id,
        "execution": "client",
        "status": "completed",
        "tools": [shipping_tool_definition()]
    })
}

fn incomplete_search_response(items: &[serde_json::Value]) -> ScriptedResponse {
    let mut events =
        with_terminal_output_items(responses_api_completed_only_events("test"), items.to_vec());
    for event in &mut events {
        if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&event.data)
            && value["type"] == "response.completed"
        {
            value["type"] = serde_json::json!("response.incomplete");
            value["response"]["status"] = serde_json::json!("incomplete");
            value["response"]["incomplete_details"] =
                serde_json::json!({"reason": "max_output_tokens"});
            event.data = value.to_string();
        }
    }
    ScriptedResponse::sse(events)
}

fn request_inputs(server: &MockInferenceServer) -> Vec<Vec<serde_json::Value>> {
    let inputs: Vec<_> = server
        .request_bodies()
        .into_iter()
        .filter_map(|body| {
            body.get("input")
                .and_then(serde_json::Value::as_array)
                .cloned()
        })
        .collect();
    assert_eq!(inputs.len(), server.request_count_for("/v1/responses"));
    inputs
}

async fn transcript_discovery_items(actor: &SessionActor) -> Vec<serde_json::Value> {
    actor
        .chat_state_handle
        .get_conversation()
        .await
        .iter()
        .filter_map(|item| match item {
            ConversationItem::Discovery { item } => Some(item.raw().clone()),
            _ => None,
        })
        .collect()
}

fn assert_native_pair(input: &[serde_json::Value], call_id: &str, tools: Vec<serde_json::Value>) {
    let call_at = input
        .iter()
        .position(|item| item["type"] == "tool_search_call" && item["call_id"] == call_id)
        .expect("the accepted call must survive");
    assert_eq!(input[call_at]["execution"], "client");
    let outputs: Vec<_> = input
        .iter()
        .enumerate()
        .filter(|(_, item)| item["type"] == "tool_search_output" && item["call_id"] == call_id)
        .collect();
    assert_eq!(
        outputs.len(),
        1,
        "every accepted client call must have exactly one paired output: {call_id}"
    );
    let (output_at, output) = outputs[0];
    assert!(
        call_at < output_at,
        "the call must precede its paired output"
    );
    assert_eq!(
        output,
        &serde_json::json!({
            "type": "tool_search_output",
            "call_id": call_id,
            "execution": "client",
            "status": "completed",
            "tools": tools
        })
    );
    assert!(output["tools"].as_array().unwrap().len() <= 8);
    assert!(
        output.to_string().len()
            <= xai_grok_sampling_types::conversation::tool_search::MAX_DISCOVERY_ANSWER_BYTES
    );
}

fn assert_ordinary_result(input: &[serde_json::Value], call_id: &str) {
    let calls: Vec<_> = input
        .iter()
        .filter(|item| item["type"] == "function_call" && item["call_id"] == call_id)
        .collect();
    let results: Vec<_> = input
        .iter()
        .filter(|item| item["type"] == "function_call_output" && item["call_id"] == call_id)
        .collect();
    assert_eq!(
        calls.len(),
        1,
        "ordinary call dispatch remains exactly once"
    );
    assert_eq!(
        results.len(),
        1,
        "ordinary dispatch produces one matching result"
    );
    assert!(
        results[0]["output"]
            .to_string()
            .contains("paired ordinary dispatch"),
        "ordinary tool must execute successfully: {:?}",
        results[0]
    );
}

/// R7b (D17): drive the real turn loop over a canned discovery-only response.
/// The call is invisible to `tool_calls()`, so without the answer arm the
/// loop would end the turn after one request; the arm must re-enter the
/// model — the second request is the continuation itself.
#[test]
fn the_turn_loop_reenters_the_model_to_answer_a_client_call() {
    on_session_stack(|| {
        run_paused(|| async {
            let server = MockInferenceServer::start_with_models(vec![MockModelEntry::new("test")])
                .await
                .expect("mock inference server");
            server.enqueue_response(
                "/v1/responses",
                discovery_only_response(&[lone_client_call_item("call_KGrhHQ8F7MeagVDbKnGlq6vv")]),
            );
            // Once the queue drains, the mock serves its default completed text
            // response: the arm's re-entry ends the turn.
            let (actor, _retries) =
                actor_under_test(&server, SessionKind::Main, sampler_no_retries(), false).await;
            let outcome = run_discovery_turn(&server, &actor, "req-waj57-r7b").await;
            pump_local_tasks().await;
            assert!(
                outcome.is_ok(),
                "the answer arm's re-entry must complete the turn: {:?}",
                outcome.as_ref().err()
            );
            assert_eq!(
                server.request_count_for("/v1/responses"),
                2,
                "the call is invisible to tool_calls(), so only the answer arm issues \
                 the second request"
            );
        });
    });
}

/// The production e2e (D16's offline proof): real decode of a canned stream,
/// the real turn loop, the real H2 answer, and the answered pair on the wire —
/// the second request's Codex-dialect input carries the provider's call AND
/// the harness's answer, joined on the call's own `call_id` (R6), with no
/// invented id on the answer (policy 3).
#[test]
fn the_answered_pair_rides_the_codex_wire_on_the_second_request() {
    on_session_stack(|| {
        run_paused(|| async {
            let server = MockInferenceServer::start_with_models(vec![MockModelEntry::new("test")])
                .await
                .expect("mock inference server");
            server.enqueue_response(
                "/v1/responses",
                discovery_only_response(&[lone_client_call_item("call_KGrhHQ8F7MeagVDbKnGlq6vv")]),
            );
            let (actor, _retries) =
                actor_under_test(&server, SessionKind::Main, sampler_no_retries(), false).await;
            seed_codex_family(&actor).await;
            let outcome = run_discovery_turn(&server, &actor, "req-waj57-e2e").await;
            pump_local_tasks().await;
            assert!(
                outcome.is_ok(),
                "the answer arm's re-entry must complete the turn: {:?}",
                outcome.as_ref().err()
            );

            let input = second_request_input(&server);
            let call = input
                .iter()
                .find(|item| item["type"] == serde_json::json!("tool_search_call"))
                .expect("the provider-minted call rides the second request's input");
            assert_eq!(
                call["call_id"],
                serde_json::json!("call_KGrhHQ8F7MeagVDbKnGlq6vv"),
                "the splice replays the banked call's own bytes"
            );
            assert_eq!(call["execution"], serde_json::json!("client"));
            let answer = input
                .iter()
                .position(|item| item["type"] == serde_json::json!("tool_search_output"))
                .and_then(|index| input.get(index))
                .expect("the harness's answer rides the second request's input");
            assert!(
                input
                    .iter()
                    .position(|item| item["type"] == serde_json::json!("tool_search_call"))
                    .unwrap()
                    < input
                        .iter()
                        .position(|item| item["type"] == serde_json::json!("tool_search_output"))
                        .unwrap(),
                "the call precedes its answer in the wire order"
            );
            assert_eq!(
                answer["call_id"],
                serde_json::json!("call_KGrhHQ8F7MeagVDbKnGlq6vv"),
                "the answer echoes the call's call_id verbatim (R6)"
            );
            assert_eq!(answer["execution"], serde_json::json!("client"));
            assert_eq!(answer["status"], serde_json::json!("completed"));
            assert!(
                answer.get("id").is_none(),
                "the client-authored answer carries no invented id (policy 3): {answer}"
            );
            assert!(
                answer["tools"].is_array(),
                "the answer carries a tools array — empty is the honest zero for a \
                 session with no MCP snapshot: {answer}"
            );
        });
    });
}

/// The search budget limits populated answers, never accepted-call pairing.
#[test]
fn the_answer_arm_caps_searches_but_pairs_all_five_calls() {
    on_session_stack(|| {
        run_paused(|| async {
            let server = MockInferenceServer::start_with_models(vec![MockModelEntry::new("test")])
                .await
                .expect("mock inference server");
            let calls: Vec<serde_json::Value> = (0..5)
                .map(|index| lone_client_call_item(&format!("call_WAJ57BUDGET{index}")))
                .collect();
            server.enqueue_response("/v1/responses", discovery_only_response(&calls));
            let (actor, _retries) =
                actor_under_test(&server, SessionKind::Main, sampler_no_retries(), false).await;
            seed_codex_family(&actor).await;
            seed_searchable_mcp(&actor);
            let outcome = run_discovery_turn(&server, &actor, "req-waj57-budget").await;
            pump_local_tasks().await;
            assert!(
                outcome.is_ok(),
                "a budget-capped answer must still complete the turn: {:?}",
                outcome.as_ref().err()
            );
            let input = second_request_input(&server);
            let calls_on_wire = input
                .iter()
                .filter(|item| item["type"] == serde_json::json!("tool_search_call"))
                .count();
            let answers_on_wire = input
                .iter()
                .filter(|item| item["type"] == serde_json::json!("tool_search_output"))
                .count();
            assert_eq!(
                calls_on_wire, 5,
                "every minted call rides the wire: {input:?}"
            );
            assert_eq!(
                answers_on_wire, 5,
                "all five accepted calls must be paired; the search cap cannot orphan the fifth"
            );
            for index in 0..5 {
                let tools = if index < 4 {
                    vec![shipping_tool_definition()]
                } else {
                    vec![]
                };
                assert_native_pair(&input, &format!("call_WAJ57BUDGET{index}"), tools);
            }
            let output_ids: Vec<_> = input
                .iter()
                .filter(|item| item["type"] == "tool_search_output")
                .map(|item| item["call_id"].clone())
                .collect();
            assert_eq!(
                output_ids,
                (0..5)
                    .map(|index| serde_json::json!(format!("call_WAJ57BUDGET{index}")))
                    .collect::<Vec<_>>()
            );
        });
    });
}

/// R17-b (D18(b)): the in-arm `call_id` guard is the ONLY release-build
/// defence against a duplicate answer — `group_is_closed` is existential and
/// `outbound_lint_gate` panics only under debug. A frame that re-mints an
/// already-answered `call_id` (distinct item id, same key) therefore appends
/// exactly ONE output, observed on the second request's wire bytes.
#[test]
fn a_re_delivered_call_id_appends_no_second_output() {
    on_session_stack(|| {
        run_paused(|| async {
            let server = MockInferenceServer::start_with_models(vec![MockModelEntry::new("test")])
                .await
                .expect("mock inference server");
            let mut second = lone_client_call_item("call_WAJ57REDIV");
            // Distinct item id, same call_id: the re-delivered frame shape.
            second["id"] = serde_json::json!("tsc_WAJ57REDIV_dup");
            server.enqueue_response(
                "/v1/responses",
                discovery_only_response(&[lone_client_call_item("call_WAJ57REDIV"), second]),
            );
            let (actor, _retries) =
                actor_under_test(&server, SessionKind::Main, sampler_no_retries(), false).await;
            seed_codex_family(&actor).await;
            let outcome = run_discovery_turn(&server, &actor, "req-waj57-redelivered").await;
            pump_local_tasks().await;
            assert!(
                outcome.is_ok(),
                "a re-delivered call_id must not wedge the turn: {:?}",
                outcome.as_ref().err()
            );
            let input = second_request_input(&server);
            let calls_on_wire = input
                .iter()
                .filter(|item| item["type"] == serde_json::json!("tool_search_call"))
                .count();
            let answers: Vec<&serde_json::Value> = input
                .iter()
                .filter(|item| item["type"] == serde_json::json!("tool_search_output"))
                .collect();
            assert_eq!(
                calls_on_wire, 2,
                "both minted calls ride the wire (the duplicate is the provider's \
                 shape, not ours to drop): {input:?}"
            );
            assert_eq!(
                answers.len(),
                1,
                "one call_id answers once per turn — the in-arm guard is the only \
                 release-build duplicate defence: {input:?}"
            );
            assert_eq!(answers[0]["call_id"], serde_json::json!("call_WAJ57REDIV"));
        });
    });
}

#[test]
fn mixed_response_pairs_search_and_dispatches_the_ordinary_tool() {
    on_session_stack(|| {
        run_paused(|| async {
            let server = MockInferenceServer::start_with_models(vec![MockModelEntry::new("test")])
                .await
                .expect("mock inference server");
            server.enqueue_response(
                "/v1/responses",
                discovery_only_response(&[
                    lone_client_call_item("call_J086MIXED"),
                    ordinary_call_item("ordinary_J086MIXED"),
                ]),
            );
            let (actor, _retries) =
                actor_under_test(&server, SessionKind::Main, sampler_no_retries(), false).await;
            seed_codex_family(&actor).await;
            seed_searchable_mcp(&actor);
            install_ordinary_tool(&actor).await;
            let outcome = run_discovery_turn(&server, &actor, "req-j086-mixed").await;
            pump_local_tasks().await;
            assert!(
                matches!(outcome, Ok(TurnOutcome::Completed { ref tools_called, .. }) if tools_called == &["todo_write"]),
                "mixed response must execute its ordinary tool once"
            );
            let input = second_request_input(&server);
            assert_ordinary_result(&input, "ordinary_J086MIXED");
            assert_native_pair(&input, "call_J086MIXED", vec![shipping_tool_definition()]);
        });
    });
}

#[test]
fn length_retry_carries_the_accepted_search_pair() {
    on_session_stack(|| {
        run_paused(|| async {
            let server = MockInferenceServer::start_with_models(vec![MockModelEntry::new("test")])
                .await
                .expect("mock inference server");
            server.enqueue_response(
                "/v1/responses",
                incomplete_search_response(&[lone_client_call_item("call_J086LENGTH")]),
            );
            let (actor, _retries) =
                actor_under_test(&server, SessionKind::Main, sampler_no_retries(), false).await;
            seed_codex_family(&actor).await;
            seed_searchable_mcp(&actor);
            let mut salvage = length_salvage::LengthSalvage::new(Some(1));
            let outcome = tokio::time::timeout(
                Duration::from_secs(300),
                actor.process_conversation_turn_with_recovery(
                    "req-j086-length",
                    None,
                    None,
                    None,
                    &mut salvage,
                    &mut Default::default(),
                ),
            )
            .await
            .expect("length retry must finish within timeout");
            pump_local_tasks().await;
            assert!(
                outcome.is_ok(),
                "enabled salvage must accept the Length response"
            );
            assert_eq!(
                salvage.continues(),
                1,
                "the second request is the Length salvage retry"
            );
            let input = second_request_input(&server);
            assert!(
                input
                    .iter()
                    .any(|item| item.to_string().contains("exceeded the output token limit")),
                "the actual salvage reminder must ride the retry"
            );
            assert_native_pair(&input, "call_J086LENGTH", vec![shipping_tool_definition()]);
        });
    });
}

#[test]
fn a_committed_provider_pair_seeds_cross_response_idempotence_without_spending_budgets() {
    on_session_stack(|| {
        run_paused(|| async {
            let server = MockInferenceServer::start_with_models(vec![MockModelEntry::new("test")])
                .await
                .expect("mock inference server");
            let original_answer = provider_answer_item("call_J086PAIRED");
            server.enqueue_response(
                "/v1/responses",
                discovery_only_response(&[
                    lone_client_call_item("call_J086PAIRED"),
                    original_answer.clone(),
                    ordinary_call_item("ordinary_J086PAIRED1"),
                ]),
            );
            let mut redelivered = lone_client_call_item("call_J086PAIRED");
            redelivered["id"] = serde_json::json!("tsc_J086PAIRED_redelivered");
            let mut mixed = vec![
                redelivered.clone(),
                ordinary_call_item("ordinary_J086PAIRED2"),
            ];
            mixed.extend(
                (0..4).map(|index| lone_client_call_item(&format!("call_J086FRESH{index}"))),
            );
            server.enqueue_response("/v1/responses", discovery_only_response(&mixed));
            redelivered["id"] = serde_json::json!("tsc_J086PAIRED_duplicate_only");
            server.enqueue_response("/v1/responses", discovery_only_response(&[redelivered]));
            let (actor, _retries) =
                actor_under_test(&server, SessionKind::Main, sampler_no_retries(), false).await;
            seed_codex_family(&actor).await;
            seed_searchable_mcp(&actor);
            install_ordinary_tool(&actor).await;
            let outcome = run_discovery_turn(&server, &actor, "req-j086-provider-pair").await;
            pump_local_tasks().await;
            assert!(
                matches!(outcome, Ok(TurnOutcome::Completed { ref tools_called, .. }) if tools_called == &["todo_write", "todo_write"]),
                "both ordinary tools must dispatch"
            );
            assert_eq!(
                server.request_count_for("/v1/responses"),
                3,
                "redelivery alone must not spend a discovery continuation"
            );
            let inputs = request_inputs(&server);
            for input in &inputs[1..] {
                let answers: Vec<_> = input
                    .iter()
                    .filter(|item| {
                        item["type"] == "tool_search_output" && item["call_id"] == "call_J086PAIRED"
                    })
                    .cloned()
                    .collect();
                assert_eq!(
                    answers,
                    vec![original_answer.clone()],
                    "the committed provider pair survives without an extra harness answer"
                );
            }
            let third = &inputs[2];
            assert_ordinary_result(third, "ordinary_J086PAIRED1");
            assert_ordinary_result(third, "ordinary_J086PAIRED2");
            for index in 0..4 {
                assert_native_pair(
                    third,
                    &format!("call_J086FRESH{index}"),
                    vec![shipping_tool_definition()],
                );
            }
            let final_items = transcript_discovery_items(&actor).await;
            let answers: Vec<_> = final_items
                .iter()
                .filter(|item| {
                    item["type"] == "tool_search_output" && item["call_id"] == "call_J086PAIRED"
                })
                .cloned()
                .collect();
            assert_eq!(
                answers,
                vec![original_answer],
                "later redelivery cannot append a second output to the final transcript"
            );
        });
    });
}

#[test]
fn five_discovery_responses_allow_exactly_four_followups_and_persist_the_fifth_empty_pair() {
    on_session_stack(|| {
        run_paused(|| async {
            let server = MockInferenceServer::start_with_models(vec![MockModelEntry::new("test")])
                .await
                .expect("mock inference server");
            for index in 0..6 {
                server.enqueue_response(
                    "/v1/responses",
                    discovery_only_response(&[lone_client_call_item(&format!(
                        "call_J086FOLLOWUP{index}"
                    ))]),
                );
            }
            let (actor, _retries) =
                actor_under_test(&server, SessionKind::Main, sampler_no_retries(), false).await;
            seed_codex_family(&actor).await;
            seed_searchable_mcp(&actor);
            let outcome = run_discovery_turn(&server, &actor, "req-j086-followups").await;
            pump_local_tasks().await;
            assert!(outcome.is_ok(), "bounded discovery must complete the turn");
            assert_eq!(
                server.request_count_for("/v1/responses"),
                5,
                "four discovery continuations permit five requests, never a sixth"
            );
            let inputs = request_inputs(&server);
            for (request_index, input) in inputs.iter().enumerate().skip(1) {
                assert_eq!(
                    input
                        .iter()
                        .filter(|item| item["type"] == "tool_search_output")
                        .count(),
                    request_index,
                    "each follow-up carries every earlier paired output"
                );
                for call_index in 0..request_index {
                    assert_native_pair(
                        input,
                        &format!("call_J086FOLLOWUP{call_index}"),
                        vec![shipping_tool_definition()],
                    );
                }
            }
            let final_items = transcript_discovery_items(&actor).await;
            assert_eq!(
                final_items
                    .iter()
                    .filter(|item| item["type"] == "tool_search_call")
                    .count(),
                5
            );
            assert_eq!(
                final_items
                    .iter()
                    .filter(|item| item["type"] == "tool_search_output")
                    .count(),
                5,
                "the fifth accepted discovery call must persist its empty paired output"
            );
            for index in 0..5 {
                let tools = if index < 4 {
                    vec![shipping_tool_definition()]
                } else {
                    vec![]
                };
                assert_native_pair(&final_items, &format!("call_J086FOLLOWUP{index}"), tools);
            }
        });
    });
}

#[test]
fn exhausted_discovery_budgets_still_pair_and_dispatch_a_mixed_fifth_response() {
    on_session_stack(|| {
        run_paused(|| async {
            let server = MockInferenceServer::start_with_models(vec![MockModelEntry::new("test")])
                .await
                .expect("mock inference server");
            for index in 0..4 {
                server.enqueue_response(
                    "/v1/responses",
                    discovery_only_response(&[lone_client_call_item(&format!(
                        "call_J086EXHAUST{index}"
                    ))]),
                );
            }
            server.enqueue_response(
                "/v1/responses",
                discovery_only_response(&[
                    lone_client_call_item("call_J086EXHAUST4"),
                    ordinary_call_item("ordinary_J086EXHAUST"),
                ]),
            );
            let (actor, _retries) =
                actor_under_test(&server, SessionKind::Main, sampler_no_retries(), false).await;
            seed_codex_family(&actor).await;
            seed_searchable_mcp(&actor);
            install_ordinary_tool(&actor).await;
            let outcome = run_discovery_turn(&server, &actor, "req-j086-exhausted-mixed").await;
            pump_local_tasks().await;
            assert!(
                matches!(outcome, Ok(TurnOutcome::Completed { ref tools_called, .. }) if tools_called == &["todo_write"]),
                "both exhausted discovery budgets must leave ordinary dispatch intact"
            );
            assert_eq!(
                server.request_count_for("/v1/responses"),
                6,
                "ordinary dispatch, not discovery, permits the sixth request"
            );
            let inputs = request_inputs(&server);
            for (request_index, input) in inputs.iter().enumerate().take(5).skip(1) {
                for call_index in 0..request_index {
                    assert_native_pair(
                        input,
                        &format!("call_J086EXHAUST{call_index}"),
                        vec![shipping_tool_definition()],
                    );
                }
            }
            let final_input = &inputs[5];
            assert_ordinary_result(final_input, "ordinary_J086EXHAUST");
            for index in 0..5 {
                let tools = if index < 4 {
                    vec![shipping_tool_definition()]
                } else {
                    vec![]
                };
                assert_native_pair(final_input, &format!("call_J086EXHAUST{index}"), tools);
            }
        });
    });
}

#[test]
fn a_server_executed_search_is_never_answered_by_the_harness() {
    on_session_stack(|| {
        run_paused(|| async {
            let server = MockInferenceServer::start_with_models(vec![MockModelEntry::new("test")])
                .await
                .expect("mock inference server");
            let mut server_call = lone_client_call_item("call_J086SERVER");
            server_call["execution"] = serde_json::json!("server");
            let mut server_answer = provider_answer_item("call_J086SERVER");
            server_answer["execution"] = serde_json::json!("server");
            let text = serde_json::json!({
                "type": "message", "id": "msg_J086SERVER", "role": "assistant", "status": "completed",
                "content": [{"type": "output_text", "text": "Provider search completed.", "annotations": []}]
            });
            server.enqueue_response(
                "/v1/responses",
                discovery_only_response(&[server_call, server_answer.clone(), text]),
            );
            let (actor, _retries) =
                actor_under_test(&server, SessionKind::Main, sampler_no_retries(), false).await;
            seed_codex_family(&actor).await;
            seed_searchable_mcp(&actor);
            let outcome = run_discovery_turn(&server, &actor, "req-j086-server").await;
            pump_local_tasks().await;
            assert!(outcome.is_ok());
            assert_eq!(server.request_count_for("/v1/responses"), 1);
            let items = transcript_discovery_items(&actor).await;
            let outputs: Vec<_> = items
                .iter()
                .filter(|item| item["type"] == "tool_search_output")
                .cloned()
                .collect();
            assert_eq!(
                outputs,
                vec![server_answer],
                "server execution keeps only its provider-authored output"
            );
        });
    });
}
