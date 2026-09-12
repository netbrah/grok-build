//! L2: ACP-driven live acceptance against the real LLM proxy.
//!
//! Layer 2 of `grok/plans/smoke-harness-spec.md` (v1 FINAL): the L1 scenarios from
//! `smoke/run-smoke.sh` (prompts/expectations mirrored 1:1) driven through ACP
//! against the live proxy, so the catalog/hydration/auth/model-seam surface is
//! guarded at the protocol level instead of CLI stdout parsing.
//!
//! All scenarios are `#[ignore = "live proxy acceptance"]`: the hermetic canonical
//! gate never touches the network. Live run (`CODEX_LLM_PROXY_KEY` must be in the
//! environment — env-only, never on disk):
//!
//! ```sh
//! RUST_MIN_STACK=67108864 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=4 \
//!   cargo test -p xai-grok-shell --features xai-grok-shell/test-support \
//!   --test responses_acceptance -- --ignored
//! ```

mod acp_harness;

use std::time::Duration;

use acp_harness::{RecordingClient, connect_and_auth_live, run_agent_test_live_proxy};
use agent_client_protocol::{self as acp, Agent as _};
use serde_json::json;
use xai_grok_shell::agent::config::AgentDefinition;

/// Live turns can exceed the 60s `RPC_TIMEOUT` the mock path is tuned for:
/// the spec sets 180s, overridable via `GROK_L2_TIMEOUT_SECS`.
fn live_rpc_timeout() -> Duration {
    std::env::var("GROK_L2_TIMEOUT_SECS")
        .ok()
        .and_then(|secs| secs.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(180))
}

/// `session/new` with `_meta.modelId` (the harness `new_session` helper is pinned
/// to the mock catalog's `test-model`).
async fn live_new_session(
    conn: &acp::ClientSideConnection,
    cwd: &std::path::Path,
    model_id: &str,
) -> acp::SessionId {
    tokio::time::timeout(
        live_rpc_timeout(),
        conn.new_session(
            acp::NewSessionRequest::new(cwd.to_path_buf())
                .meta(json!({ "modelId": model_id }).as_object().cloned()),
        ),
    )
    .await
    .expect("session/new timed out")
    .expect("session/new failed")
    .session_id
}

async fn live_prompt_turn(
    conn: &acp::ClientSideConnection,
    session_id: &acp::SessionId,
    text: &str,
) {
    let resp = tokio::time::timeout(
        live_rpc_timeout(),
        conn.prompt(acp::PromptRequest::new(
            session_id.clone(),
            vec![acp::ContentBlock::Text(acp::TextContent::new(
                text.to_owned(),
            ))],
        )),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "prompt on {} timed out after {}s",
            session_id.0,
            live_rpc_timeout().as_secs()
        )
    })
    .unwrap_or_else(|e| panic!("prompt on {} failed: {e}", session_id.0));
    assert!(
        matches!(resp.stop_reason, acp::StopReason::EndTurn),
        "expected EndTurn on {}, got {:?}",
        session_id.0,
        resp.stop_reason
    );
}

/// Protocol-level auth-failure signatures (the 024ef68/403 class): wire artifacts
/// (ext-notification keys, the ACP auth-required error name/code, the proxy's 403
/// body), not natural-language model output — any of them in the recorded stream
/// means the turn hit an auth failure.
const AUTH_ERROR_SIGNATURES: &[&str] = &[
    "auth_error",
    "authError",
    "auth_required",
    "AuthRequired",
    "authorization credentials",
    "-32000",
];

/// No `auth_error`-class update anywhere in the recorded stream or streamed text.
fn assert_no_auth_error(client: &RecordingClient, session_id: &acp::SessionId) {
    let mut evidence = Vec::new();
    for (idx, update) in client.recorded_updates().iter().enumerate() {
        let raw = update.to_string();
        if AUTH_ERROR_SIGNATURES.iter().any(|sig| raw.contains(sig)) {
            evidence.push(format!("update[{idx}]: {raw}"));
        }
    }
    let text = client.recorded_text();
    if AUTH_ERROR_SIGNATURES.iter().any(|sig| text.contains(sig)) {
        evidence.push(format!("agent text: {text}"));
    }
    assert!(
        evidence.is_empty(),
        "auth_error-class update in the ACP stream for session {} (024ef68/403 class regression):\n{}",
        session_id.0,
        evidence.join("\n")
    );
}

/// L1 `a-hydrated`: a hydrated row (no `[model.*]` section) on the responses wire with
/// the endpoint-default env key — the exact path the P1 403 (024ef68) broke.
#[test]
#[ignore = "live proxy acceptance"]
fn l2_hydrated_no_auth_error() {
    run_agent_test_live_proxy(|cwd, cfg| async move {
        let client = RecordingClient::default();
        let (conn, _init) = connect_and_auth_live(client.clone(), "l2-hydrated", cfg).await;
        let session_id = live_new_session(&conn, &cwd, "qwen3.8-27b").await;
        live_prompt_turn(&conn, &session_id, "Reply with exactly: P1-OK").await;
        let text = client.recorded_text();
        assert!(
            text.contains("P1-OK"),
            "P1-OK missing from the streamed reply: {text:?}"
        );
        assert_no_auth_error(&client, &session_id);
    });
}

/// L1 `control`, mid-session: `session/set_session_config_option` (config_id "model")
/// is the surface where the P1 403 bit — the set_model re-resolution path.
#[test]
#[ignore = "live proxy acceptance"]
fn l2_set_model_mid_session() {
    run_agent_test_live_proxy(|cwd, cfg| async move {
        let client = RecordingClient::default();
        let (conn, _init) = connect_and_auth_live(client.clone(), "l2-set-model", cfg).await;
        let session_id = live_new_session(&conn, &cwd, "qwen3.8-27b").await;
        let options = tokio::time::timeout(
            live_rpc_timeout(),
            conn.set_session_config_option(acp::SetSessionConfigOptionRequest::new(
                session_id.clone(),
                "model",
                acp::SessionConfigOptionValue::ValueId {
                    value: acp::SessionConfigValueId::new("gpt-5.6-sol"),
                },
            )),
        )
        .await
        .expect("set_session_config_option timed out")
        .expect("set_session_config_option failed (P1 403 class: set_model re-resolution)");
        let model_option = options
            .config_options
            .iter()
            .find(|o| o.id.0.as_ref() == "model")
            .expect("model config option missing from set_session_config_option response");
        let acp::SessionConfigKind::Select(select) = &model_option.kind else {
            panic!("model config option is not a select: {model_option:?}");
        };
        assert_eq!(
            select.current_value.0.as_ref(),
            "gpt-5.6-sol",
            "set_model did not take effect: {model_option:?}"
        );
        live_prompt_turn(&conn, &session_id, "Reply with exactly: CONTROL-OK").await;
        let text = client.recorded_text();
        assert!(
            text.contains("CONTROL-OK"),
            "CONTROL-OK missing from the streamed reply: {text:?}"
        );
        assert_no_auth_error(&client, &session_id);
    });
}

/// L1 `b-messages`: the messages transport (the injected config pins
/// `[model.claude-sonnet-5] api_backend = "messages"` per the live-config mirror).
/// Doubles as a regression guard for the MW-1+ messages work.
#[test]
#[ignore = "live proxy acceptance"]
fn l2_messages_wire() {
    run_agent_test_live_proxy(|cwd, cfg| async move {
        let client = RecordingClient::default();
        let (conn, _init) = connect_and_auth_live(client.clone(), "l2-messages", cfg).await;
        let session_id = live_new_session(&conn, &cwd, "claude-sonnet-5").await;
        live_prompt_turn(&conn, &session_id, "Reply with exactly: MSG-OK").await;
        let text = client.recorded_text();
        assert!(
            text.contains("MSG-OK"),
            "MSG-OK missing from the streamed reply: {text:?}"
        );
        assert_no_auth_error(&client, &session_id);
    });
}

/// The L1 `c-subagent` `--agents` row, parsed the same way the pager parses it
/// (headless/cli.rs `parse_cli_agents`: `prompt` → `promptBody`, name/description
/// defaulted from the key, then `AgentDefinition::from_json`).
fn echo_agent_definition() -> AgentDefinition {
    let mut value = json!({
        "model": "qwen3.8-27b",
        "prompt": "You are an echo agent. Reply with exactly what the parent asks for, nothing else.",
    });
    if let serde_json::Value::Object(ref mut obj) = value {
        if !obj.contains_key("promptBody")
            && let Some(prompt) = obj.remove("prompt")
        {
            obj.insert("promptBody".to_owned(), prompt);
        }
        obj.entry("name".to_owned())
            .or_insert_with(|| json!("echo"));
        obj.entry("description".to_owned())
            .or_insert_with(|| json!("echo"));
    }
    let mut def = AgentDefinition::from_json(&value).expect("echo AgentDefinition");
    def.name = "echo".to_owned();
    def
}

/// L1 `c-subagent`: the parent spawns the `echo` subagent (model qwen3.8-27b, echo
/// prompt) and the child's reply must surface in the parent's stream. Guards c88936e
/// (the subagent override credential guard) plus the subagent transport.
#[test]
#[ignore = "live proxy acceptance"]
fn l2_subagent() {
    run_agent_test_live_proxy(|cwd, mut cfg| async move {
        // The same field the pager's `--agents` JSON feeds (headless.rs:915).
        cfg.cli_agents = vec![echo_agent_definition()];
        let client = RecordingClient::default();
        let (conn, _init) = connect_and_auth_live(client.clone(), "l2-subagent", cfg).await;
        let session_id = live_new_session(&conn, &cwd, "qwen3.8-27b").await;
        live_prompt_turn(
            &conn,
            &session_id,
            "Spawn the echo subagent and ask it to reply with exactly: CHILD-OK",
        )
        .await;
        let finished = client
            .wait_for_subagent_finished_any(live_rpc_timeout())
            .await;
        assert!(
            !finished.is_empty(),
            "no subagent_finished in the ACP stream (child never finished)"
        );
        let text = client.recorded_text();
        assert!(
            text.contains("CHILD-OK"),
            "CHILD-OK missing from the streamed reply: {text:?}"
        );
        assert_no_auth_error(&client, &session_id);
    });
}

/// L1 `c-subagent` re-run with the parent on the messages wire: parent =
/// claude-sonnet-5 (the same `[model.claude-sonnet-5] api_backend = "messages"`
/// config row `l2_messages_wire` uses) + the same `echo` child spec as
/// `l2_subagent`. Roadmap item 6 requires subagents working ON the messages
/// wire; `l2_subagent` only proved the subagent path on the responses wire.
///
/// CHILD-WIRE PIN (live-observed 2026-09-12, de-risking run, log
/// /tmp/mw4-l2-live.log): the child rides the RESPONSES wire, not the
/// parent's messages wire. The wire is a per-model property —
/// `[model.<slug>] api_backend` else the `[endpoints] default_api_backend`
/// hydration — never inherited from the parent session:
///   - parent claude-sonnet-5: explicit `api_backend = "messages"` row →
///     POST .../v1/messages (the parent's own UUIDv7 session id).
///   - child qwen3.8-27b: no `[model.qwen3.8-27b]` row → hydrated off the
///     endpoint defaults (`default_api_backend = "responses"`) →
///     POST .../v1/responses, with `x-grok-model-override: qwen3.8-27b` on a
///     fresh UUIDv7 child session id distinct from the parent's.
/// The c88936e fail-closed guard did NOT fire: the hydrated child entry
/// carries the endpoint-default `env_key = CODEX_LLM_PROXY_KEY` (env-only),
/// so `has_own_credentials()` holds, the `AgentDefinition` pin is accepted,
/// and the parent-route fall-through (which would have run the child as
/// claude-sonnet-5 on messages) was never reached.
#[test]
#[ignore = "live proxy acceptance"]
fn l2_subagent_messages_wire() {
    run_agent_test_live_proxy(|cwd, mut cfg| async move {
        cfg.cli_agents = vec![echo_agent_definition()];
        let client = RecordingClient::default();
        let (conn, _init) = connect_and_auth_live(client.clone(), "l2-subagent-msgs", cfg).await;
        let session_id = live_new_session(&conn, &cwd, "claude-sonnet-5").await;
        live_prompt_turn(
            &conn,
            &session_id,
            "Spawn the echo subagent and ask it to reply with exactly: CHILD-OK",
        )
        .await;
        let finished = client
            .wait_for_subagent_finished_any(live_rpc_timeout())
            .await;
        assert!(
            !finished.is_empty(),
            "no subagent_finished in the ACP stream (child never finished)"
        );
        let text = client.recorded_text();
        assert!(
            text.contains("CHILD-OK"),
            "CHILD-OK missing from the streamed reply: {text:?}"
        );
        // The child's wire is not visible in the ACP stream; it is pinned by
        // the CHILD-WIRE PIN above (sampler per-request send lines —
        // model_id + endpoint URL — from the de-risking run's log).
        assert_no_auth_error(&client, &session_id);
    });
}
