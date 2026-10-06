//! Coverage of the H2 answer arm (apex-waj.57, D17): the turn loop answers the
//! client-executed `tool_search_call` the provider minted and stopped on.
//! R7b — the loop re-enters the model (a second request is issued) — and the
//! production e2e — that second request carries the answered pair on the
//! Codex-dialect wire bytes. Plus the R17-a per-turn budget and the R17-b
//! per-`call_id` idempotence, both observed on the wire rather than on state.

use super::rate_limit_backoff_tests::{SessionKind, actor_under_test, pump_local_tasks};
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

/// R17-a: the answer-and-`continue` path is budgeted per turn
/// (`MAX_DISCOVERY_ANSWERS_PER_TURN`). Five client calls in one response
/// therefore yield exactly four harness answers — the fifth stays in-flight
/// (the documented residual: the client still holds the call, and the
/// compaction orphan policy bounds it) — and the arm still re-enters the
/// model once.
#[test]
fn the_answer_arm_budget_caps_harness_answers_at_four_per_turn() {
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
                answers_on_wire, 4,
                "the per-turn budget caps harness answers at four; the fifth stays \
                 in-flight by design: {input:?}"
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
