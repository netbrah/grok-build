//! I-37 TITLE-SIDE-REQUEST: the first-title side call observed at the real wire boundary.
//!
//! The title call is built by `build_title_request` and sent by `SummaryGenerator::update` →
//! `generate_session_summary` → `SamplingClient::conversation_collect`, so it rides the row's own
//! backend through the real lowerer and transport. A Messages row must ask with
//! `tool_choice: {"type":"auto"}`: the captured deployment bodies below carried the forced
//! `{"type":"tool","name":"session_title"}` shape and the provider answered
//! `tool_choice: type "tool" and "any" are not supported for this model`, so the first title was
//! lost to a local fallback. Chat keeps `Function`, Responses keeps `Required` with `store=false`.
//!
//! Everything here is offline: a bounded, choice-sensitive loopback receiver on `127.0.0.1:0`
//! answers the catalog, the main turn and the title call. No provider, auth or live call.

mod acp_harness;

use std::cell::RefCell;
use std::convert::Infallible;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use acp_harness::{RPC_TIMEOUT, connect_and_auth_live};
use agent_client_protocol::{self as acp, Agent as _};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt as _;
use futures_util::stream;
use serde_json::{Value, json};
use xai_grok_shell::agent::config::Config as AgentConfig;
use xai_grok_shell::session::info::Info;
use xai_grok_shell::session::storage::{JsonlStorageAdapter, StorageAdapter};
use xai_grok_test_support::MockModelEntry;
use xai_grok_test_support::sse;

/// The title call runs on a spawned task; this is how long the test waits for it to land.
const TITLE_WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(20);

/// The receiver refuses anything beyond this many inference calls, so a runaway fixture fails
/// loudly instead of spinning.
const MAX_INFERENCE_REQUESTS: usize = 24;

/// Sentinel the fake model returns for the title. Deliberately unlike anything the local fallback
/// could build from the prompt text, so a fallback can never satisfy a "generated title" assert.
const TITLE_SENTINEL: &str = "Zephyr Archive Manifest Probe Title";

/// Agent text the fake model streams for the main turn.
const MAIN_TEXT: &str = "offline main turn complete";

/// A legal unforced Messages answer that carries the title as plain text (I-37 T2). It is unlike
/// [`TITLE_SENTINEL`] and unlike anything the prompt-derived fallback could build, so a discarded
/// or overwritten title cannot be confused with a passing case.
const TEXT_TITLE: &str = "Zephyr Archive Checksum Verification Report";

/// [`TEXT_TITLE`] split across stream deltas: the stream collector has to reassemble it.
const TEXT_FRAGMENTS: [&str; 4] = ["Zephyr ", "Archive Checksum ", "Verification ", "Report"];

/// Prose that arrives *before* a valid tool title inside one answer (mixed-answer precedence).
const MIXED_PROSE: &str = "Archive Manifest Checksum Work In Progress Notes";

/// The structured title in that same mixed answer; only this one may be adopted.
const MIXED_TOOL_TITLE: &str = "Zephyr Structured Title Wins Over Prose";

/// An answer with no usable characters in it.
const BLANK_TITLE_TEXT: &str = "   \n\t  ";

/// A `session_title` tool call whose arguments never parse.
const MALFORMED_TITLE_ARGS: &str = "{\"session_title\": ";

/// A tool call for a different tool: its arguments are not a title.
const UNKNOWN_TOOL_NAME: &str = "session_summary";
const UNKNOWN_TOOL_ARGS: &str = "{\"session_summary\": \"Zephyr Wrong Tool Answer\"}";
/// A call for a differently named tool that still carries a perfectly valid `session_title`
/// argument object; only the name distinguishes it (L1I37-R4 R-4 control input).
const WRONG_NAME_TITLE: &str = "Zephyr Wrong Name Must Not Win";
const WRONG_NAME_TITLE_ARGS: &str = r#"{"session_title": "Zephyr Wrong Name Must Not Win"}"#;

/// The stock post-turn feature pair, both ON: the profile a normal deployment resolves to.
const NORMAL_PROFILE: &str = r#"[features]
turn_summary = true
title_refresh = true
"#;

/// POSTTURN_OFF: exactly the two approved feature values differ from [`NORMAL_PROFILE`].
const OFF_PROFILE: &str = r#"[features]
turn_summary = false
title_refresh = false
"#;

/// A first prompt with no usable characters: the producer must stay idle, not spend its one attempt.
const BLANK_USER_PROMPT: &str = "   \n\t  ";

/// A manual title set through the real `x.ai/session/rename` boundary.
const MANUAL_TITLE: &str = "Renamed By The User On Purpose";

/// Later ordinary turns, used to reach the real `TITLE_REFRESH_TURNS` checkpoints (3 and 6).
fn follow_up_prompt(turn: usize) -> String {
    format!("Follow-up {turn}: report the archive checksum result again, briefly.")
}

/// How long to wait for a call that a profile is expected to make, and equally how long an
/// expected-absence window stays open. The NORMAL leg asserts its own observed latency is below
/// this, so the OFF absence window is provably at least as wide as the real arrival latency.
const SETTLE_ABSENCE: Duration = Duration::from_secs(20);

/// Upper bound on how long the receiver holds a stalled title response open.
const GATE_WAIT: Duration = Duration::from_secs(30);

/// Distinctive instruction fragments: the only honest way to tell the two post-turn side calls
/// apart from a main turn, since both are deliberately tool-free requests.
const TITLE_REFRESH_MARKER: &str = "Generate a session title for the conversation above";
const TURN_SUMMARY_MARKER: &str = "Write an ultra-short dashboard line";

/// The body a rejected title request answers with. It names the deployment failure and must never
/// be adopted as a model-generated title.
const TITLE_ERROR_BODY: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"litellm.BadRequestError: Zephyr Error Body Must Never Become The Title"}}"#;

/// The first user prompt, byte-for-byte the one whose side call is captured below.
const FIRST_PROMPT: &str = "Using the archive and host MCP services, report the SHA-256 of the \
exported grok pager Cargo manifest and the current free GiB of the isolated probe-build volume. \
Base your answer on real tool results. Do not change files, run shell commands, or delegate.";

/// Catalog row ids: the two base Claude rows whose first-title calls were captured, plus one Chat
/// and one Responses row so the cross-wire control sends for real.
const OPUS_ROW: &str = "claude-opus-5.5";
const SONNET_ROW: &str = "claude-sonnet-5.5";
const CHAT_ROW: &str = "gpt-offline-chat";
const RESPONSES_ROW: &str = "gpt-offline-responses";

/// Immutable sanitized captures of the first-title call as the deployment really sent it on the
/// Messages wire (probe `prb-early-20261020`, `initial-title-*`). These are the expectation for the
/// received body with exactly one change: the whole `tool_choice` replaced. Reproduced verbatim.
///
/// - `initial-title-opus.request-body.json` sha256 `eac20cc37d16a1f9496fde4ea001c397239a0c5d74a73a03765499b9e785ff85`
/// - `initial-title-sonnet.request-body.json` sha256 `faac51f06fe314f504d438a4678803ecc22f35a5abc7baffddcb097c9d2276d9`
const OPUS_CAPTURE: &str = r##"{
  "model": "claude-opus-5.5",
  "messages": [
    {
      "role": "user",
      "content": [
        {
          "type": "text",
          "text": "<user_query>\nUsing the archive and host MCP services, report the SHA-256 of the exported grok pager Cargo manifest and the current free GiB of the isolated probe-build volume. Base your answer on real tool results. Do not change files, run shell commands, or delegate.\n</user_query>",
          "cache_control": {
            "type": "ephemeral"
          }
        }
      ]
    }
  ],
  "max_tokens": 100,
  "system": [
    {
      "type": "text",
      "text": "You are tasked with generating the session title. The user is asking almost always software engineering related questions on their codebase.\nWe describe the session title below\n# Session Title\nA short and distinctive 5-10 word descriptive title for the session. Super info dense, no filler.\n\nYou will be given the user query below encapsulated in <user_query></user_query>.\n\nJust generate the session_title and nothing else",
      "cache_control": {
        "type": "ephemeral"
      }
    }
  ],
  "tools": [
    {
      "name": "session_title",
      "description": "Generate the session_title which we use for the user_message",
      "input_schema": {
        "type": "object",
        "required": [
          "session_title"
        ],
        "properties": {
          "session_title": {
            "type": "string",
            "description": "Final session title, just 5-10 word descriptive title for the session. Super info dense, no filler."
          }
        },
        "additionalProperties": false
      }
    }
  ],
  "tool_choice": {
    "type": "tool",
    "name": "session_title"
  },
  "temperature": 1.0,
  "stream": true
}"##;
const SONNET_CAPTURE: &str = r##"{
  "model": "claude-sonnet-5.5",
  "messages": [
    {
      "role": "user",
      "content": [
        {
          "type": "text",
          "text": "<user_query>\nUsing the archive and host MCP services, report the SHA-256 of the exported grok pager Cargo manifest and the current free GiB of the isolated probe-build volume. Base your answer on real tool results. Do not change files, run shell commands, or delegate.\n</user_query>",
          "cache_control": {
            "type": "ephemeral"
          }
        }
      ]
    }
  ],
  "max_tokens": 100,
  "system": [
    {
      "type": "text",
      "text": "You are tasked with generating the session title. The user is asking almost always software engineering related questions on their codebase.\nWe describe the session title below\n# Session Title\nA short and distinctive 5-10 word descriptive title for the session. Super info dense, no filler.\n\nYou will be given the user query below encapsulated in <user_query></user_query>.\n\nJust generate the session_title and nothing else",
      "cache_control": {
        "type": "ephemeral"
      }
    }
  ],
  "tools": [
    {
      "name": "session_title",
      "description": "Generate the session_title which we use for the user_message",
      "input_schema": {
        "type": "object",
        "required": [
          "session_title"
        ],
        "properties": {
          "session_title": {
            "type": "string",
            "description": "Final session title, just 5-10 word descriptive title for the session. Super info dense, no filler."
          }
        },
        "additionalProperties": false
      }
    }
  ],
  "tool_choice": {
    "type": "tool",
    "name": "session_title"
  },
  "temperature": 1.0,
  "stream": true
}"##;

/// The provider's answer to either forced choice, captured from the same probe
/// (`initial-title-opus.error.json` sha256 `a84aa0a1ffd65025248d26df2afeac9a01c8e91aa0322d2c9719f4e2ac6fe578`; the sonnet row
/// got the identical message for its own model group). The receiver replays it so a forced choice
/// fails here the way it fails in the deployment.
const PROVIDER_REJECTION: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"litellm.BadRequestError: Vertex_aiException BadRequestError - {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"tool_choice: type \\\"tool\\\" and \\\"any\\\" are not supported for this model.\"},\"request_id\":\"req_vrtx_011CgCkoa51ZokqVzjJ1Je6y\"}. Received Model Group=claude-opus-5.5\nAvailable Model Group Fallbacks=None"}}"#;

/// Which of the session's model calls a received body is. Only `InitialTitle` declares the
/// `session_title` tool; the two post-turn side calls are tool-free and are told apart by their
/// instruction text, and everything else is the main turn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CallKind {
    InitialTitle,
    TitleRefresh,
    TurnSummary,
    Main,
}

fn call_kind(body: &Value) -> CallKind {
    let text = body.to_string();
    if text.contains(TITLE_REFRESH_MARKER) {
        CallKind::TitleRefresh
    } else if text.contains(TURN_SUMMARY_MARKER) {
        CallKind::TurnSummary
    } else {
        CallKind::Main
    }
}

/// One recorded inference call, as the receiver finally received it.
#[derive(Clone, Debug)]
struct Received {
    path: &'static str,
    body: Value,
    kind: CallKind,
}

impl Received {
    /// The title call is the one that declares exactly the eager `session_title` tool.
    fn is_title_call(&self) -> bool {
        let tools = self.body.get("tools").and_then(Value::as_array);
        match tools {
            Some(tools) if tools.len() == 1 => {
                tool_name(&tools[0]).is_some_and(|n| n == "session_title")
            }
            _ => false,
        }
    }
}

fn tool_name(tool: &Value) -> Option<&str> {
    tool.get("name").and_then(Value::as_str).or_else(|| {
        tool.get("function")
            .and_then(|f| f.get("name"))
            .and_then(Value::as_str)
    })
}

/// What the receiver answers an *accepted* title request with on the Messages wire. I-37 T2 needs
/// the whole answer matrix through the real stream collector, not a unit-level parse.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
enum TitleAnswer {
    /// A valid `session_title` tool call — the base behaviour T1 asserts.
    #[default]
    ToolCall,
    /// The title as plain text only: the legal unforced answer a Messages row may give.
    Text,
    /// The same plain-text title, split across several deltas.
    Fragments,
    /// Prose text first, then a valid tool title carrying a different sentinel.
    Mixed,
    /// A `session_title` call whose arguments are truncated JSON.
    MalformedArgs,
    /// A tool call for some other tool.
    UnknownTool,
    /// A tool call for a differently named tool whose arguments are nonetheless a valid
    /// `session_title` object — the name is the only thing that is wrong (R-4).
    WrongToolName,
    /// No usable characters at all.
    Blank,
    /// An HTTP error whose body carries the rejection text in [`TITLE_ERROR_BODY`].
    Rejected,
    /// A valid tool answer the receiver holds open until the test releases the gate, so a manual
    /// rename can be driven while the generated title is genuinely still in flight.
    Delayed,
}

/// What the receiver handed one title request. Recorded so a test can prove what the wire
/// really sent instead of inferring it from what the session ended up with (R-3).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Answered {
    path: &'static str,
    model: String,
    label: &'static str,
}

/// Shared receiver state: every received call plus the verdict handed out for its choice.
#[derive(Default)]
struct ReceiverState {
    received: Vec<Received>,
    over_budget: bool,
    title_answer: TitleAnswer,
    /// Set by a test to release a [`TitleAnswer::Delayed`] response.
    release: Arc<std::sync::atomic::AtomicBool>,
    answers: Vec<Answered>,
    /// Set once a [`TitleAnswer::Delayed`] answer has handed its final event to the transport, so
    /// a race test can wait for the losing title instead of only asserting its absence (R-7).
    delayed_answered: Arc<std::sync::atomic::AtomicBool>,
}

fn record_answer(
    state: &Arc<Mutex<ReceiverState>>,
    path: &'static str,
    model: &str,
    label: &'static str,
) {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .answers
        .push(Answered {
            path,
            model: model.to_owned(),
            label,
        });
}

/// The labels this row's title calls were answered with, in arrival order.
fn title_answer_labels(
    state: &Arc<Mutex<ReceiverState>>,
    path: &str,
    model: &str,
) -> Vec<&'static str> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .answers
        .iter()
        .filter(|a| a.path == path && a.model == model)
        .map(|a| a.label)
        .collect()
}

/// Lets a stalled title response finish, so its result lands after something else already did.
fn release_title_gate(state: &Arc<Mutex<ReceiverState>>) {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .release
        .store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Chooses what the next accepted title call is answered with. The title task is off the turn's
/// critical path, so a caller sets this *before* the session it belongs to and then waits for that
/// session's own title call: by the time the call is recorded the answer bytes are already built,
/// so changing it afterwards cannot disturb an in-flight response.
fn set_title_answer(state: &Arc<Mutex<ReceiverState>>, answer: TitleAnswer) {
    let mut guard = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.title_answer = answer;
}

/// True once a [`TitleAnswer::Delayed`] answer has been written out in full.
fn delayed_title_answered(state: &Arc<Mutex<ReceiverState>>) -> bool {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .delayed_answered
        .load(std::sync::atomic::Ordering::SeqCst)
}

/// Waits for the stalled title answer to reach the transport. Without it, a manual-rename race
/// proves nothing more than that a title task which never ran leaves no notification (R-7).
async fn wait_for_delayed_title_answered(state: &Arc<Mutex<ReceiverState>>) {
    let barrier = async {
        while !delayed_title_answered(state) {
            tokio::time::sleep(POLL).await;
        }
    };
    tokio::time::timeout(TITLE_WAIT, barrier)
        .await
        .expect("the delayed title answer never reached the transport within the title wait");
}

/// A bounded, choice-sensitive loopback inference endpoint.
///
/// The stock `MockInferenceServer` matches on endpoint and request kind only, so it cannot tell a
/// forced choice from `auto` on the same path. This receiver answers on the *content* of
/// `tool_choice`, which is the whole subject of I-37: it rejects both forced shapes exactly as the
/// captured deployment error did, and serves a valid `session_title` answer only to a request it
/// would really accept.
struct TitleReceiver {
    url: String,
    state: Arc<Mutex<ReceiverState>>,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl TitleReceiver {
    fn spawn() -> Self {
        let state = Arc::new(Mutex::new(ReceiverState::default()));
        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let thread_state = Arc::clone(&state);
        let flag = Arc::clone(&shutdown);
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("receiver runtime");
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("bind receiver");
                let addr = listener.local_addr().expect("receiver addr");
                tx.send(format!("http://{addr}/v1")).expect("publish url");
                axum::serve(listener, Self::router(thread_state))
                    .with_graceful_shutdown(async move {
                        while !flag.load(std::sync::atomic::Ordering::SeqCst) {
                            tokio::time::sleep(POLL).await;
                        }
                    })
                    .await
                    .expect("serve receiver");
            });
        });
        let url = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("receiver url");
        Self {
            url,
            state,
            shutdown,
            thread: Some(thread),
        }
    }

    fn url(&self) -> &str {
        &self.url
    }

    fn router(state: Arc<Mutex<ReceiverState>>) -> Router {
        Router::new()
            .route(
                "/v1/models",
                get(|| async { Json(json!({"object": "list", "data": catalog_rows()})) }),
            )
            .route(
                "/v1/user",
                get(|| async {
                    Json(json!({"userId": "offline-user", "email": "offline@test.invalid"}))
                }),
            )
            .route("/v1/settings", get(|| async { StatusCode::NOT_FOUND }))
            .route("/v1/messages", post(Self::handle_messages))
            .route("/v1/chat/completions", post(Self::handle_chat))
            .route("/v1/responses", post(Self::handle_responses))
            .fallback(|uri: axum::http::Uri| async move {
                tracing::debug!(path = %uri.path(), "offline title receiver: unmatched request");
                StatusCode::NOT_FOUND
            })
            .with_state(state)
    }

    /// Records one inference call and decides what the wire answers with.
    fn classify(state: &Arc<Mutex<ReceiverState>>, path: &'static str, body: Value) -> Response {
        let is_title = {
            let mut guard = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.received.len() >= MAX_INFERENCE_REQUESTS {
                guard.over_budget = true;
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            let mut entry = Received {
                path,
                body: body.clone(),
                kind: CallKind::Main,
            };
            if entry.is_title_call() {
                entry.kind = CallKind::InitialTitle;
            } else {
                entry.kind = call_kind(&entry.body);
            }
            let is_title = entry.kind == CallKind::InitialTitle;
            guard.received.push(entry);
            is_title
        };
        let model = body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("offline-model")
            .to_owned();
        if !is_title {
            // The main turn answers on the row's own protocol; only the Messages row speaks
            // Messages, and a wrong-shaped answer would fail the turn instead of the claim.
            let events = match path {
                "/v1/chat/completions" => sse::chat_completion_events(MAIN_TEXT, &model),
                "/v1/responses" => sse::responses_api_events(MAIN_TEXT, &model),
                _ => sse::messages_api_events(MAIN_TEXT, &model, "end_turn"),
            };
            return sse_from_events(events);
        }
        let (answer, gate, delayed) = {
            let guard = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                guard.title_answer,
                Arc::clone(&guard.release),
                Arc::clone(&guard.delayed_answered),
            )
        };
        let choice = body.get("tool_choice").cloned().unwrap_or(Value::Null);
        let choice_type = choice.get("type").and_then(Value::as_str);
        // The captured rejection is what the *Messages* deployment answers a forced choice with.
        // A Responses row legally asks with the string "required", and rejecting that left the
        // non-Messages text-only fallback control with an error to fall back from instead of
        // text, so its Responses half proved nothing (R-3).
        let forced = path == "/v1/messages"
            && (choice_type == Some("tool")
                || choice_type == Some("any")
                || choice.as_str() == Some("required"));
        if forced {
            record_answer(state, path, &model, "forced-400");
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::from_str::<Value>(PROVIDER_REJECTION).expect("rejection parses")),
            )
                .into_response();
        }
        match path {
            // The non-Messages rows answer in text: I-37 T1 asserts only the choice those wires
            // carry, and their text handling is the non-Messages fallback control's subject.
            "/v1/chat/completions" => {
                record_answer(state, path, &model, "text");
                sse_from_events(sse::chat_completion_events(TITLE_SENTINEL, &model))
            }
            "/v1/responses" => {
                record_answer(state, path, &model, "text");
                sse_from_events(sse::responses_api_events(TITLE_SENTINEL, &model))
            }
            _ => {
                record_answer(state, path, &model, "messages");
                Self::messages_title_answer(&model, answer, gate, delayed)
            }
        }
    }

    /// The answer matrix an accepted Messages title request can get, in Messages wire form.
    fn messages_title_answer(
        model: &str,
        answer: TitleAnswer,
        gate: Arc<std::sync::atomic::AtomicBool>,
        delayed: Arc<std::sync::atomic::AtomicBool>,
    ) -> Response {
        if answer == TitleAnswer::Rejected {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::from_str::<Value>(TITLE_ERROR_BODY).expect("error body parses")),
            )
                .into_response();
        }
        let title_json = json!({"session_title": TITLE_SENTINEL}).to_string();
        let events = match answer {
            TitleAnswer::ToolCall => sse::messages_api_tool_use_events(
                "toolu_title_1",
                "session_title",
                &title_json,
                model,
            ),
            TitleAnswer::Text => messages_text_events(&[TEXT_TITLE], model),
            TitleAnswer::Fragments => messages_text_events(&TEXT_FRAGMENTS, model),
            TitleAnswer::Blank => messages_text_events(&[BLANK_TITLE_TEXT], model),
            TitleAnswer::Mixed => messages_mixed_events(MIXED_PROSE, MIXED_TOOL_TITLE, model),
            TitleAnswer::MalformedArgs => sse::messages_api_tool_use_events(
                "toolu_title_bad",
                "session_title",
                MALFORMED_TITLE_ARGS,
                model,
            ),
            TitleAnswer::UnknownTool => sse::messages_api_tool_use_events(
                "toolu_other",
                UNKNOWN_TOOL_NAME,
                UNKNOWN_TOOL_ARGS,
                model,
            ),
            TitleAnswer::WrongToolName => sse::messages_api_tool_use_events(
                "toolu_wrong_name",
                UNKNOWN_TOOL_NAME,
                WRONG_NAME_TITLE_ARGS,
                model,
            ),
            TitleAnswer::Delayed => {
                let model = model.to_owned();
                let stalled = stream::once(async move {
                    let deadline = tokio::time::Instant::now() + GATE_WAIT;
                    while !gate.load(std::sync::atomic::Ordering::SeqCst)
                        && tokio::time::Instant::now() < deadline
                    {
                        tokio::time::sleep(POLL).await;
                    }
                    let events = sse::messages_api_tool_use_events(
                        "toolu_title_1",
                        "session_title",
                        &json!({"session_title": TITLE_SENTINEL}).to_string(),
                        &model,
                    );
                    // The flag flips as the last event is handed over, which is the honest
                    // positive signal a race test can wait for (R-7).
                    let last = events.len().saturating_sub(1);
                    stream::iter(events.into_iter().enumerate().map(move |(idx, e)| {
                        if idx == last {
                            delayed.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                        Event::default().data(e.data)
                    }))
                })
                .flatten();
                return Sse::new(stalled.map(Ok::<_, Infallible>))
                    .keep_alive(KeepAlive::default())
                    .into_response();
            }
            TitleAnswer::Rejected => unreachable!("answered above"),
        };
        sse_from_scripted(events)
    }

    async fn handle_messages(
        State(state): State<Arc<Mutex<ReceiverState>>>,
        Json(body): Json<Value>,
    ) -> Response {
        Self::classify(&state, "/v1/messages", body)
    }

    async fn handle_chat(
        State(state): State<Arc<Mutex<ReceiverState>>>,
        Json(body): Json<Value>,
    ) -> Response {
        Self::classify(&state, "/v1/chat/completions", body)
    }

    async fn handle_responses(
        State(state): State<Arc<Mutex<ReceiverState>>>,
        Json(body): Json<Value>,
    ) -> Response {
        Self::classify(&state, "/v1/responses", body)
    }
}

impl Drop for TitleReceiver {
    fn drop(&mut self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

/// Every inference call the receiver has been given, in arrival order.
fn received(state: &Arc<Mutex<ReceiverState>>) -> Vec<Received> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .received
        .clone()
}

/// The title calls only — the I-37 subject.
fn title_calls(state: &Arc<Mutex<ReceiverState>>) -> Vec<Received> {
    received(state)
        .into_iter()
        .filter(Received::is_title_call)
        .collect()
}

fn sse_from_events(events: Vec<Event>) -> Response {
    Sse::new(stream::iter(events.into_iter().map(Ok::<_, Infallible>)))
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// One Messages SSE turn whose content is `chunks` of text, streamed as separate deltas, ending
/// in `end_turn`. Mirrors `sse::messages_api_events` (which only ever emits one delta) so a
/// fragmented answer is really fragmented on the wire.
fn messages_text_events(
    chunks: &[&str],
    model: &str,
) -> Vec<xai_grok_test_support::scripted::SseEvent> {
    let mut events = messages_start(model);
    events.push(ev(json!({
        "type": "content_block_start", "index": 0,
        "content_block": {"type": "text", "text": ""}
    })));
    for chunk in chunks {
        events.push(ev(json!({
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": chunk}
        })));
    }
    events.push(ev(json!({"type": "content_block_stop", "index": 0})));
    events.extend(messages_stop("end_turn"));
    events
}

/// One Messages SSE turn that streams prose text and *then* a valid `session_title` tool call, in
/// the same answer, so the two title candidates arrive in one assistant item.
fn messages_mixed_events(
    prose: &str,
    tool_title: &str,
    model: &str,
) -> Vec<xai_grok_test_support::scripted::SseEvent> {
    let mut events = messages_start(model);
    events.push(ev(json!({
        "type": "content_block_start", "index": 0,
        "content_block": {"type": "text", "text": ""}
    })));
    events.push(ev(json!({
        "type": "content_block_delta", "index": 0,
        "delta": {"type": "text_delta", "text": prose}
    })));
    events.push(ev(json!({"type": "content_block_stop", "index": 0})));
    events.push(ev(json!({
        "type": "content_block_start", "index": 1,
        "content_block": {"type": "tool_use", "id": "toolu_title_mixed", "name": "session_title", "input": {}}
    })));
    events.push(ev(json!({
        "type": "content_block_delta", "index": 1,
        "delta": {"type": "input_json_delta",
                  "partial_json": json!({"session_title": tool_title}).to_string()}
    })));
    events.push(ev(json!({"type": "content_block_stop", "index": 1})));
    events.extend(messages_stop("tool_use"));
    events
}

fn messages_start(model: &str) -> Vec<xai_grok_test_support::scripted::SseEvent> {
    vec![ev(json!({
        "type": "message_start",
        "message": {
            "id": "msg_title_test", "type": "message", "role": "assistant",
            "content": [], "model": model, "stop_reason": null,
            "usage": {
                "input_tokens": 10, "output_tokens": 0,
                "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0
            }
        }
    }))]
}

fn messages_stop(stop_reason: &str) -> Vec<xai_grok_test_support::scripted::SseEvent> {
    vec![
        ev(json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason},
            "usage": {"output_tokens": 5, "input_tokens": 10}
        })),
        ev(json!({"type": "message_stop"})),
    ]
}

fn ev(value: Value) -> xai_grok_test_support::scripted::SseEvent {
    xai_grok_test_support::scripted::SseEvent::data(value.to_string())
}

fn sse_from_scripted(events: Vec<xai_grok_test_support::scripted::SseEvent>) -> Response {
    Sse::new(stream::iter(
        events
            .into_iter()
            .map(|e| Ok::<_, Infallible>(Event::default().data(e.data))),
    ))
    .keep_alive(KeepAlive::default())
    .into_response()
}

/// The catalog the offline agent hydrates.
fn catalog_rows() -> Vec<Value> {
    [
        (OPUS_ROW, "messages"),
        (SONNET_ROW, "messages"),
        (CHAT_ROW, "chat_completions"),
        (RESPONSES_ROW, "responses"),
    ]
    .into_iter()
    .map(|(id, backend)| entry_json(MockModelEntry::new(id).with_api_backend(backend)))
    .collect()
}

/// `MockModelEntry::to_json` is private; mirror the shape it emits instead of widening its API.
fn entry_json(entry: MockModelEntry) -> Value {
    let mut obj = json!({
        "id": entry.id,
        "object": "model",
        "created": 1234567890,
        "owned_by": "test",
    });
    if let Some(backend) = &entry.api_backend {
        obj["apiBackend"] = json!(backend);
    }
    obj
}

/// Client that auto-approves and records BOTH title notification forms: the
/// `x.ai/session_notification` ext update and the `session_info_update`.
#[derive(Clone, Default)]
struct TitleClient {
    updates: Rc<RefCell<Vec<Value>>>,
    ext: Rc<RefCell<Vec<Value>>>,
    text: Rc<RefCell<String>>,
}

impl TitleClient {
    /// Titles seen in a `session_info_update`, in arrival order.
    fn info_update_titles(&self) -> Vec<String> {
        self.updates
            .borrow()
            .iter()
            .filter(|u| u["sessionUpdate"] == "session_info_update")
            .map(|u| u["title"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    /// Titles seen in the `x.ai/session_notification` ext form. `XaiSessionUpdate` is internally
    /// tagged (`#[serde(tag = "sessionUpdate", rename_all = "snake_case")]`), so the generated
    /// title arrives flat: `update.sessionUpdate == "session_summary_generated"` with the title in
    /// `update.session_summary`.
    fn ext_summary_titles(&self) -> Vec<String> {
        self.ext
            .borrow()
            .iter()
            .filter(|p| p["update"]["sessionUpdate"] == "session_summary_generated")
            .filter_map(|p| p["update"]["session_summary"].as_str().map(str::to_owned))
            .collect()
    }

    /// Raw ext payloads, for a failure message that can tell "not sent" from "read at the wrong
    /// path".
    fn ext_raw(&self) -> Vec<Value> {
        self.ext.borrow().clone()
    }

    fn streamed_text(&self) -> String {
        self.text.borrow().clone()
    }
}

#[async_trait::async_trait(?Send)]
impl acp::Client for TitleClient {
    async fn request_permission(
        &self,
        args: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        Ok(acp::RequestPermissionResponse::new(
            acp_harness::allow_once(&args),
        ))
    }

    async fn session_notification(&self, args: acp::SessionNotification) -> acp::Result<()> {
        if let acp::SessionUpdate::AgentMessageChunk(chunk) = &args.update
            && let acp::ContentBlock::Text(text) = &chunk.content
        {
            self.text.borrow_mut().push_str(&text.text);
        }
        if let Ok(raw) = serde_json::to_value(&args.update) {
            self.updates.borrow_mut().push(raw);
        }
        Ok(())
    }

    async fn ext_notification(&self, args: acp::ExtNotification) -> acp::Result<()> {
        if args.method.as_ref() == "x.ai/session_notification"
            && let Ok(params) = serde_json::from_str::<Value>(args.params.get())
        {
            self.ext.borrow_mut().push(params);
        }
        Ok(())
    }
}

/// `session/new` on an explicit catalog row. The shared harness helper is pinned to the mock
/// catalog's `test-model`, which is not one of the I-37 rows.
async fn new_session_on_row(
    conn: &acp::ClientSideConnection,
    cwd: &std::path::Path,
    model_id: &str,
) -> acp::SessionId {
    tokio::time::timeout(
        RPC_TIMEOUT,
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

async fn prompt_turn(conn: &acp::ClientSideConnection, session_id: &acp::SessionId, text: &str) {
    let resp = tokio::time::timeout(
        RPC_TIMEOUT,
        conn.prompt(acp::PromptRequest::new(
            session_id.clone(),
            vec![acp::ContentBlock::Text(acp::TextContent::new(
                text.to_owned(),
            ))],
        )),
    )
    .await
    .unwrap_or_else(|_| panic!("prompt on {} timed out", session_id.0))
    .unwrap_or_else(|e| panic!("prompt on {} failed: {e}", session_id.0));
    assert!(
        matches!(resp.stop_reason, acp::StopReason::EndTurn),
        "expected EndTurn on {}, got {:?}",
        session_id.0,
        resp.stop_reason
    );
}

/// Waits for the title task's notification pair: the observable point after real adoption.
async fn wait_for_title_notification(client: &TitleClient, want: &str) {
    tokio::time::timeout(TITLE_WAIT, async {
        loop {
            if client.info_update_titles().iter().any(|t| t == want)
                && client.ext_summary_titles().iter().any(|t| t == want)
            {
                return;
            }
            tokio::time::sleep(POLL).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "no title notification pair for {want:?} within {TITLE_WAIT:?}; info_update={:?} \
             ext_titles={:?} ext_raw={:?}",
            client.info_update_titles(),
            client.ext_summary_titles(),
            client.ext_raw()
        )
    });
}

/// Waits until the receiver has logged this row's title call on `path` (the title task is off the
/// turn's critical path, so the arrival is the only honest barrier). One session's call must not
/// satisfy the barrier for a later row, so the row identity is part of the barrier.
async fn wait_for_title_call(state: &Arc<Mutex<ReceiverState>>, path: &str, model: &str) -> Value {
    let barrier = async {
        loop {
            let found = received(state)
                .into_iter()
                .filter(|r| r.is_title_call() && r.path == path)
                .find(|r| r.body.get("model").and_then(Value::as_str) == Some(model))
                .map(|r| r.body);
            if let Some(body) = found {
                return body;
            }
            tokio::time::sleep(POLL).await;
        }
    };
    tokio::time::timeout(TITLE_WAIT, barrier)
        .await
        .unwrap_or_else(|_| {
            panic!("no title call for model {model:?} reached {path} within {TITLE_WAIT:?}")
        })
}

/// Waits until the receiver has logged the `nth` (1-based) title call for `model` on `path`, and
/// returns its body. Several sessions may share one row, so arrival order is the only identity.
async fn wait_for_nth_title_call(
    state: &Arc<Mutex<ReceiverState>>,
    path: &str,
    model: &str,
    nth: usize,
) -> Value {
    let barrier = async {
        loop {
            let calls: Vec<Value> = received(state)
                .into_iter()
                .filter(|r| r.is_title_call() && r.path == path)
                .filter(|r| r.body.get("model").and_then(Value::as_str) == Some(model))
                .map(|r| r.body)
                .collect();
            if calls.len() >= nth {
                return calls[nth - 1].clone();
            }
            tokio::time::sleep(POLL).await;
        }
    };
    tokio::time::timeout(TITLE_WAIT, barrier)
        .await
        .unwrap_or_else(|_| panic!(
            "only {} of {nth} title call(s) for model {model:?} reached {path} within {TITLE_WAIT:?}",
            title_calls_for(state, path, model).len()
        ))
}

/// Every title call recorded for one row on one path, in arrival order.
fn title_calls_for(state: &Arc<Mutex<ReceiverState>>, path: &str, model: &str) -> Vec<Value> {
    received(state)
        .into_iter()
        .filter(|r| r.is_title_call() && r.path == path)
        .filter(|r| r.body.get("model").and_then(Value::as_str) == Some(model))
        .map(|r| r.body)
        .collect()
}

/// How many times each notification form has carried `want`.
fn title_notification_counts(client: &TitleClient, want: &str) -> (usize, usize) {
    (
        client
            .info_update_titles()
            .iter()
            .filter(|t| *t == want)
            .count(),
        client
            .ext_summary_titles()
            .iter()
            .filter(|t| *t == want)
            .count(),
    )
}

/// How many titles each notification form has carried at all.
fn title_notification_pair(client: &TitleClient) -> (usize, usize) {
    (
        client.info_update_titles().len(),
        client.ext_summary_titles().len(),
    )
}

/// Waits until BOTH notification forms have carried at least `want_count` titles, whatever their
/// value. Reading the durable title after this barrier therefore describes *this* session's
/// adoption, and a session that produced no title at all cannot hide behind an earlier one.
async fn wait_for_title_notifications(client: &TitleClient, want_count: usize) {
    let barrier = async {
        loop {
            let (info, ext) = title_notification_pair(client);
            if info >= want_count && ext >= want_count {
                return;
            }
            tokio::time::sleep(POLL).await;
        }
    };
    tokio::time::timeout(TITLE_WAIT, barrier)
        .await
        .unwrap_or_else(|_| panic!(
            "fewer than {want_count} title notification pair(s) arrived within {TITLE_WAIT:?};              info={:?}",
            client.info_update_titles()
        ))
}

/// Counts the calls of one kind the receiver has recorded.
fn calls_of_kind(state: &Arc<Mutex<ReceiverState>>, kind: CallKind) -> usize {
    received(state)
        .into_iter()
        .filter(|r| r.kind == kind)
        .count()
}

/// Waits until the receiver has recorded `want` calls of `kind`.
async fn wait_for_call_kind(state: &Arc<Mutex<ReceiverState>>, kind: CallKind, want: usize) {
    let barrier = async {
        loop {
            if calls_of_kind(state, kind) >= want {
                return;
            }
            tokio::time::sleep(POLL).await;
        }
    };
    tokio::time::timeout(TITLE_WAIT, barrier)
        .await
        .unwrap_or_else(|_| {
            panic!(
                "only {} call(s) of kind {kind:?} arrived within {TITLE_WAIT:?}, wanted {want}",
                calls_of_kind(state, kind)
            )
        })
}

/// Asserts nothing beyond `want` calls of `kind` shows up in a bounded absence window. The window
/// is only honest because the paired NORMAL leg proves the same call arrives sooner when it is
/// supposed to arrive at all.
async fn assert_no_further_call(state: &Arc<Mutex<ReceiverState>>, kind: CallKind, want: usize) {
    let extra = tokio::time::timeout(SETTLE_ABSENCE, async {
        loop {
            if calls_of_kind(state, kind) > want {
                return true;
            }
            tokio::time::sleep(POLL).await;
        }
    })
    .await;
    assert!(
        !extra.unwrap_or(false),
        "exactly {want} call(s) of kind {kind:?} were allowed, but          {} arrived within {SETTLE_ABSENCE:?}",
        calls_of_kind(state, kind)
    );
}

/// A manual rename through the real client boundary, `x.ai/session/rename`.
async fn rename_session(
    conn: &acp::ClientSideConnection,
    session_id: &acp::SessionId,
    cwd: &std::path::Path,
    title: &str,
) -> Value {
    acp_harness::ext_method(
        conn,
        "x.ai/session/rename",
        json!({
            "sessionId": session_id.0.as_ref(),
            "title": title,
            "cwd": cwd.to_string_lossy(),
        }),
    )
    .await
}

/// The truthful fallback title: the first ten words of the same prompt-derived text the helper
/// builds, so a fallback is compared against a value this fixture can state independently.
fn expected_fallback_title(prompt: &str) -> String {
    prompt
        .split_whitespace()
        .take(10)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Durable readback of the adopted title, through the same adapter the persistence actor writes with.
async fn stored_title(session_id: &acp::SessionId, cwd: &std::path::Path) -> (String, bool) {
    let info = Info {
        id: session_id.clone(),
        cwd: cwd.to_string_lossy().into_owned(),
    };
    let storage = JsonlStorageAdapter::new();
    let summary = storage
        .load_summary(&info)
        .await
        .unwrap_or_else(|e| panic!("load_summary for {}: {e}", session_id.0));
    (summary.session_summary.clone(), summary.title_is_manual)
}

/// The offline analogue of the harness `run_agent_test_with_models`: the same env isolation and
/// runtimes, but the base URL points at [`TitleReceiver`] because this fixture must answer on the
/// request's `tool_choice`.
fn run_title_test<F, Fut>(body: F)
where
    F: FnOnce(std::path::PathBuf, Arc<Mutex<ReceiverState>>, String) -> Fut + 'static,
    Fut: std::future::Future<Output = ()> + 'static,
{
    run_title_test_with_profile(None, body)
}

/// The same fixture on a real disk-loaded feature profile: `profile` is written verbatim to
/// `$GROK_HOME/config.toml` before the agent starts, and the session actor reloads disk config at
/// spawn (`acp_session_impl/spawn.rs`) and resolves both post-turn flags from it. Both post-turn
/// environment overrides are neutralized in EVERY profiled run, so the only difference between the
/// NORMAL and POSTTURN_OFF legs can be the two feature values themselves. `None` keeps the stock
/// runner's behaviour, where the post-turn side calls are switched off by the default test env.
fn run_title_test_with_profile<F, Fut>(profile: Option<&str>, body: F)
where
    F: FnOnce(std::path::PathBuf, Arc<Mutex<ReceiverState>>, String) -> Fut + 'static,
    Fut: std::future::Future<Output = ()> + 'static,
{
    let _env_guard = hold_global_env();
    xai_grok_shell::agent::remote_config::settings_get::reset_startup_settings_for_tests();
    xai_grok_extra_ca::ensure_default_crypto_provider();

    let receiver = TitleReceiver::spawn();
    let base_url = receiver.url().to_owned();
    let state = Arc::clone(&receiver.state);
    let grok_home = shared_grok_home();
    let xdg_config = grok_home.join(".config");
    let workdir = tempfile::TempDir::new().expect("workdir");
    // SAFETY: the env lock above is held for the whole run and the receiver's worker threads never
    // read process env — the same contract the shared harness documents for `set_test_env`.
    unsafe {
        // R-8: this target drives dozens of real prompt turns, and a turn runs whatever global
        // hooks the ambient home declares. HOME and XDG_CONFIG_HOME move into the same private
        // temp home GROK_HOME already uses, so no run reaches the operator's real home.
        std::fs::create_dir_all(&xdg_config).expect("private XDG config home");
        std::env::set_var("HOME", grok_home);
        std::env::set_var("XDG_CONFIG_HOME", &xdg_config);
        std::env::set_var("GROK_HOME", grok_home);
        std::env::set_var("GROK_CLI_CHAT_PROXY_BASE_URL", &base_url);
        std::env::set_var("GROK_XAI_API_BASE_URL", &base_url);
        std::env::set_var("XAI_API_KEY", "test-key-for-ci");
        std::env::set_var("GROK_TELEMETRY_ENABLED", "false");
        std::env::set_var("GROK_FEEDBACK_ENABLED", "false");
        std::env::set_var("GROK_TRACE_UPLOAD", "false");
        match profile {
            Some(toml) => {
                std::env::remove_var("GROK_TURN_SUMMARY");
                std::env::remove_var("GROK_TITLE_REFRESH");
                std::fs::write(grok_home.join("config.toml"), toml)
                    .expect("write the feature profile");
            }
            // No profile: the first title has no feature gate, and switching the later per-turn
            // summaries off here keeps the title call the only side call this fixture can produce.
            // The shared home is the one thing these runs share, so a previous leg's profile must
            // not leak into this one.
            None => {
                std::env::remove_var("GROK_TITLE_REFRESH");
                std::env::set_var("GROK_TURN_SUMMARY", "false");
                let profile_path = grok_home.join("config.toml");
                if profile_path.exists() {
                    std::fs::remove_file(&profile_path).expect("clear the shared feature profile");
                }
            }
        }
    }

    let agent_rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("agent runtime");
    let local = tokio::task::LocalSet::new();
    let teardown_state = Arc::clone(&state);
    local.block_on(
        &agent_rt,
        body(workdir.path().to_path_buf(), state, base_url),
    );
    // The cap is only "fails loudly" if something enforces it: an over-budget receiver answers
    // 503, which a session can survive, so the run itself must fail (R-5).
    let (recorded, over_budget) = {
        let guard = teardown_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (guard.received.len(), guard.over_budget)
    };
    assert!(
        !over_budget,
        "the fixture issued more than {MAX_INFERENCE_REQUESTS} inference calls \
         ({recorded} recorded), so its counts prove nothing"
    );
    drop(receiver);
}

/// The one grok home this binary can use: `xai_dirs::grok_home()` caches its first resolution for
/// the whole process, so a per-run directory would be silently ignored after the first run. The
/// disk-loaded feature profile is therefore written into this shared home before each spawn.
fn shared_grok_home() -> &'static std::path::Path {
    static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
    HOME.get_or_init(|| tempfile::TempDir::new().expect("grok home"))
        .path()
}

fn hold_global_env() -> std::sync::MutexGuard<'static, ()> {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Stands up a real agent on the offline catalog and authenticates with the API-key method.
async fn connect_offline_agent(
    client: TitleClient,
    client_type: &str,
) -> acp::ClientSideConnection {
    let (conn, _init) = connect_and_auth_live(client, client_type, AgentConfig::default()).await;
    conn
}

/// Every field of the received title body must equal the capture with ONLY the whole `tool_choice`
/// replaced. Any other difference is named field-by-field, never hidden by a subset compare.
fn assert_body_equals_capture(received: &Value, capture: &Value, want_choice: &Value, row: &str) {
    let mut want = capture.clone();
    want.as_object_mut()
        .expect("capture is an object")
        .insert("tool_choice".to_owned(), want_choice.clone());
    let got_keys: std::collections::BTreeSet<String> = received
        .as_object()
        .expect("received body is an object")
        .keys()
        .cloned()
        .collect();
    let want_keys: std::collections::BTreeSet<String> = want
        .as_object()
        .expect("want is an object")
        .keys()
        .cloned()
        .collect();
    let mut diffs = Vec::new();
    for key in got_keys.symmetric_difference(&want_keys) {
        diffs.push(format!("key {key} present on only one side"));
    }
    for key in &want_keys {
        if got_keys.contains(key) && received[key] != want[key] {
            diffs.push(format!(
                "{key}: received {} expected {}",
                received[key], want[key]
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "received {row} title body differs from the immutable capture (only tool_choice may \
         change):\n{}\nreceived: {received}\ncapture-with-auto: {want}",
        diffs.join("\n")
    );
}

/// I-37 T1: on a Messages row the first-title call must reach the wire asking with `auto`, and the
/// title it earns must be the one that persists and notifies.
///
/// Observed before the repair: the call left the process with the forced
/// `{"type":"tool","name":"session_title"}` choice, this receiver answered with the captured
/// provider 400, and the session got a fallback title instead.
#[test]
fn title_side_request_messages_auto_at_real_send() {
    run_title_test(|cwd, state, _base_url| async move {
        let client = TitleClient::default();
        let conn = connect_offline_agent(client.clone(), "i37-t1").await;
        let mut seen_title_calls = 0usize;
        for (row, capture) in [(OPUS_ROW, OPUS_CAPTURE), (SONNET_ROW, SONNET_CAPTURE)] {
            seen_title_calls += 1;
            let session_id = new_session_on_row(&conn, &cwd, row).await;
            prompt_turn(&conn, &session_id, FIRST_PROMPT).await;

            // The main turn itself must complete, and must not be the thing carrying the title.
            assert!(
                client.streamed_text().contains(MAIN_TEXT),
                "main turn on {row} streamed {:?}",
                client.streamed_text()
            );

            let want: Value = serde_json::from_str(capture).expect("inline capture parses");
            let received = wait_for_title_call(&state, "/v1/messages", row).await;
            let calls = title_calls(&state);
            assert_eq!(
                calls.len(),
                seen_title_calls,
                "each row must make exactly one title call, not a retry or a second side call"
            );
            assert_eq!(
                calls
                    .last()
                    .and_then(|c| c.body.get("model"))
                    .and_then(Value::as_str),
                Some(row),
                "the newest title call must belong to {row}"
            );
            assert_eq!(
                received.get("model").and_then(Value::as_str),
                Some(row),
                "title call on {row} named a different model"
            );
            assert_eq!(
                received.get("tool_choice"),
                Some(&json!({"type": "auto"})),
                "Messages title call on {row} must ask with the whole choice replaced by auto"
            );
            assert_body_equals_capture(&received, &want, &json!({"type": "auto"}), row);

            // An accepted call must land its own title, not the prompt-derived fallback. The
            // count barrier plus the per-value count is what makes this *this* row's pair: a
            // value-only barrier is already satisfied by the previous row (R-6).
            wait_for_title_notifications(&client, seen_title_calls).await;
            let (info, ext) = title_notification_counts(&client, TITLE_SENTINEL);
            assert_eq!(
                (info, ext),
                (seen_title_calls, seen_title_calls),
                "{row}: each row must contribute exactly one title notification pair"
            );
            let (stored, manual) = stored_title(&session_id, &cwd).await;
            assert_eq!(
                stored, TITLE_SENTINEL,
                "durable title on {row} is not the model-generated one"
            );
            assert!(
                !manual,
                "a generated title must not mark the title manual on {row}"
            );
        }
    });
}

/// I-37 T1 cross-wire control: the Messages repair must not travel. A Chat row keeps the forced
/// function choice and a Responses row keeps `required` with `store=false`, both observed on the
/// body the receiver actually got.
#[test]
fn title_side_request_chat_responses_choice_unchanged() {
    run_title_test(|cwd, state, _base_url| async move {
        let client = TitleClient::default();
        let conn = connect_offline_agent(client.clone(), "i37-t1-crosswire").await;

        let chat_session = new_session_on_row(&conn, &cwd, CHAT_ROW).await;
        prompt_turn(&conn, &chat_session, FIRST_PROMPT).await;
        let chat_body = wait_for_title_call(&state, "/v1/chat/completions", CHAT_ROW).await;
        assert_eq!(
            title_calls(&state).len(),
            1,
            "the Chat row must make exactly one title call"
        );
        assert_eq!(
            chat_body.get("tool_choice"),
            Some(&json!({"type": "function", "function": {"name": "session_title"}})),
            "Chat title call must keep the forced function choice"
        );
        assert_eq!(
            chat_body
                .get("tools")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1),
            "Chat title call must declare exactly the one title tool"
        );
        assert_eq!(
            chat_body.get("max_tokens").and_then(Value::as_i64),
            Some(100),
            "Chat title call keeps its 100-token cap"
        );

        let responses_session = new_session_on_row(&conn, &cwd, RESPONSES_ROW).await;
        prompt_turn(&conn, &responses_session, FIRST_PROMPT).await;
        let responses_body = wait_for_title_call(&state, "/v1/responses", RESPONSES_ROW).await;
        assert_eq!(
            title_calls(&state).len(),
            2,
            "each row must make exactly one title call, so the Responses row is the second"
        );
        assert_eq!(
            responses_body.get("tool_choice"),
            Some(&json!("required")),
            "Responses title call must keep the required choice"
        );
        assert_eq!(
            responses_body.get("store"),
            Some(&json!(false)),
            "Responses title call must keep store=false (deployment contract)"
        );
    });
}

/// I-37 T2: a Messages row asked with `auto` may answer its title as plain text, and that text is
/// the session's title — cleaned, persisted and notified. A fragmented answer counts as one title.
///
/// Observed before the repair: the answer was collected and then thrown away by the tool-only
/// parser, so the session silently kept the prompt-derived fallback.
#[test]
fn title_side_request_messages_text_is_model_title() {
    run_title_test(|cwd, state, _base_url| async move {
        let client = TitleClient::default();
        let conn = connect_offline_agent(client.clone(), "i37-t2-text").await;
        let fallback = expected_fallback_title(FIRST_PROMPT);
        let mut seen = 0usize;
        for (row, answer) in [
            (OPUS_ROW, TitleAnswer::Text),
            (SONNET_ROW, TitleAnswer::Fragments),
        ] {
            seen += 1;
            set_title_answer(&state, answer);
            let session_id = new_session_on_row(&conn, &cwd, row).await;
            prompt_turn(&conn, &session_id, FIRST_PROMPT).await;
            assert!(
                client.streamed_text().contains(MAIN_TEXT),
                "main turn on {row} must still complete: {:?}",
                client.streamed_text()
            );

            let body = wait_for_title_call(&state, "/v1/messages", row).await;
            assert_eq!(
                title_calls(&state).len(),
                seen,
                "a legal text answer must not provoke a second title call on {row}"
            );
            assert_eq!(
                body.get("tool_choice"),
                Some(&json!({"type": "auto"})),
                "the text answer was only ever served to a request asking with auto on {row}"
            );

            wait_for_title_notifications(&client, seen).await;
            let (info, ext) = title_notification_counts(&client, TEXT_TITLE);
            assert_eq!(
                (info, ext),
                (seen, seen),
                "{row}: each answer form must contribute exactly one title notification pair"
            );
            let (stored, manual) = stored_title(&session_id, &cwd).await;
            assert_eq!(
                stored, TEXT_TITLE,
                "the cleaned model title must reach persistence on {row}, not the fallback                  {fallback:?}"
            );
            assert!(
                !manual,
                "a generated title must not mark the title manual on {row}"
            );
        }
    });
}

/// I-37 T2 precedence control: one streamed answer holding prose first and a valid `session_title`
/// call second must adopt the tool title. Neither the prose nor the fallback may become the title.
///
/// This is preservation, not a claim that the parent gave text precedence: it already held on the
/// T1 candidate, and `CF-TEXT-BEFORE-TOOL` is what proves the control bites.
#[test]
fn title_side_request_messages_tool_precedes_text() {
    run_title_test(|cwd, state, _base_url| async move {
        let client = TitleClient::default();
        let conn = connect_offline_agent(client.clone(), "i37-t2-mixed").await;
        let fallback = expected_fallback_title(FIRST_PROMPT);
        set_title_answer(&state, TitleAnswer::Mixed);

        let session_id = new_session_on_row(&conn, &cwd, OPUS_ROW).await;
        prompt_turn(&conn, &session_id, FIRST_PROMPT).await;
        let body = wait_for_title_call(&state, "/v1/messages", OPUS_ROW).await;
        assert_eq!(
            body.get("tool_choice"),
            Some(&json!({"type": "auto"})),
            "the mixed answer belongs to an auto-choice request"
        );

        wait_for_title_notification(&client, MIXED_TOOL_TITLE).await;
        let (stored, manual) = stored_title(&session_id, &cwd).await;
        assert_eq!(
            stored, MIXED_TOOL_TITLE,
            "only the structured tool title may be adopted from a mixed answer"
        );
        assert!(!manual);

        let (info, ext) = (client.info_update_titles(), client.ext_summary_titles());
        for title in [MIXED_PROSE, fallback.as_str()] {
            assert!(
                !info.iter().any(|t| t == title) && !ext.iter().any(|t| t == title),
                "prose/fallback title {title:?} must never be notified from a mixed answer;                  info={info:?} ext={ext:?}"
            );
        }
    });
}

/// I-37 T2 non-Messages control: the Chat and Responses wires keep answering their title call in
/// text, and that text must NOT be taken as a title — the unchanged tool-only path keeps its
/// truthful prompt-derived fallback.
#[test]
fn title_side_request_chat_responses_text_keeps_fallback() {
    run_title_test(|cwd, state, _base_url| async move {
        let client = TitleClient::default();
        let conn = connect_offline_agent(client.clone(), "i37-t2-nonmessages").await;
        let fallback = expected_fallback_title(FIRST_PROMPT);
        let mut seen = 0usize;
        for row in [CHAT_ROW, RESPONSES_ROW] {
            seen += 1;
            let session_id = new_session_on_row(&conn, &cwd, row).await;
            prompt_turn(&conn, &session_id, FIRST_PROMPT).await;
            assert!(
                client.streamed_text().contains(MAIN_TEXT),
                "main turn on {row} must still complete: {:?}",
                client.streamed_text()
            );

            let path = if row == CHAT_ROW {
                "/v1/chat/completions"
            } else {
                "/v1/responses"
            };
            wait_for_nth_title_call(&state, path, row, 1).await;
            assert_eq!(
                title_calls(&state).len(),
                seen,
                "each non-Messages row makes exactly one title call"
            );

            // R-3: this control only means something if the wire really answered in text.
            assert_eq!(
                title_answer_labels(&state, path, row),
                vec!["text"],
                "{row}: its title call must have been answered with text, not a rejection, or                  the fallback it shows is the error fallback"
            );
            wait_for_title_notifications(&client, seen).await;
            let (info, ext) = title_notification_counts(&client, &fallback);
            assert_eq!(
                (info, ext),
                (seen, seen),
                "{row}: each row must contribute exactly one fallback notification pair"
            );
            let (stored, manual) = stored_title(&session_id, &cwd).await;
            assert_eq!(
                stored, fallback,
                "a text answer must not become the title on the non-Messages wire {row}"
            );
            assert!(
                !manual,
                "the fallback title is still a generated one on {row}"
            );

            let (info, ext) = (client.info_update_titles(), client.ext_summary_titles());
            assert!(
                !info.iter().any(|t| t == TITLE_SENTINEL)
                    && !ext.iter().any(|t| t == TITLE_SENTINEL),
                "the streamed text sentinel {TITLE_SENTINEL:?} leaked into a non-Messages title;                  info={info:?} ext={ext:?}"
            );
        }
    });
}

/// I-37 T2 truthfulness controls: a blank answer, unparseable arguments, a call for another tool
/// and a hard request error must all keep the known prompt-derived fallback, must never adopt the
/// error body or the wrong-tool text, and must leave the main turn live.
#[test]
fn title_side_request_unusable_or_error_keeps_fallback() {
    run_title_test(|cwd, state, _base_url| async move {
        let client = TitleClient::default();
        let conn = connect_offline_agent(client.clone(), "i37-t2-controls").await;
        let fallback = expected_fallback_title(FIRST_PROMPT);
        let cases = [
            ("blank answer", TitleAnswer::Blank),
            ("malformed tool arguments", TitleAnswer::MalformedArgs),
            ("a call for another tool", TitleAnswer::UnknownTool),
            ("a rejected request", TitleAnswer::Rejected),
        ];
        let mut seen = 0usize;
        for (what, answer) in cases {
            seen += 1;
            set_title_answer(&state, answer);
            let session_id = new_session_on_row(&conn, &cwd, OPUS_ROW).await;
            prompt_turn(&conn, &session_id, FIRST_PROMPT).await;
            assert!(
                client.streamed_text().contains(MAIN_TEXT),
                "the main turn stays live after {what}: {:?}",
                client.streamed_text()
            );

            wait_for_nth_title_call(&state, "/v1/messages", OPUS_ROW, seen).await;
            assert_eq!(
                title_calls(&state).len(),
                seen,
                "{what} must not provoke a retry or a second side call"
            );
            assert_eq!(
                title_calls_for(&state, "/v1/messages", OPUS_ROW).len(),
                seen,
                "{what}: the receiver must have seen this session's own title call"
            );

            wait_for_title_notifications(&client, seen).await;
            let (stored, manual) = stored_title(&session_id, &cwd).await;
            assert_eq!(
                stored, fallback,
                "{what} must keep the truthful prompt-derived fallback title"
            );
            assert!(
                !manual,
                "the fallback title is still a generated one after {what}"
            );
            let (info, ext) = title_notification_counts(&client, &fallback);
            assert_eq!(
                (info, ext),
                (seen, seen),
                "every unusable answer must adopt exactly one fallback title, in both forms"
            );

            let (info, ext) = title_notification_counts(&client, TITLE_SENTINEL);
            assert_eq!(
                (info, ext),
                (0, 0),
                "no usable title exists after {what}, so nothing else may be notified"
            );
            for needle in ["Zephyr Error Body", UNKNOWN_TOOL_ARGS] {
                let leaked = client
                    .info_update_titles()
                    .iter()
                    .chain(client.ext_summary_titles().iter())
                    .any(|t| t.contains(needle));
                assert!(
                    !leaked,
                    "{needle:?} must never masquerade as a title ({what})"
                );
            }
        }
    });
}

/// I-37 R-4 control: a Messages answer may only deliver its title through the tool the request
/// declared. A call for a *differently named* tool whose arguments are nonetheless a valid
/// `session_title` object takes the unknown-tool path: the session keeps the prompt-derived
/// fallback, and the wrong-named title is never persisted or notified.
///
/// Observed before the repair: the structured branch read the arguments without looking at the
/// name, so this answer became the durable session title.
#[test]
fn title_side_request_messages_wrong_tool_name_keeps_fallback() {
    run_title_test(|cwd, state, _base_url| async move {
        let client = TitleClient::default();
        let conn = connect_offline_agent(client.clone(), "i37-fix1-r4").await;
        let fallback = expected_fallback_title(FIRST_PROMPT);
        set_title_answer(&state, TitleAnswer::WrongToolName);

        let session_id = new_session_on_row(&conn, &cwd, OPUS_ROW).await;
        prompt_turn(&conn, &session_id, FIRST_PROMPT).await;
        assert!(
            client.streamed_text().contains(MAIN_TEXT),
            "the main turn stays live after a wrong-named title call: {:?}",
            client.streamed_text()
        );

        let body = wait_for_title_call(&state, "/v1/messages", OPUS_ROW).await;
        assert_eq!(
            title_calls(&state).len(),
            1,
            "a wrong-named tool call must not provoke a retry or a second side call"
        );
        assert_eq!(
            body.get("tool_choice"),
            Some(&json!({"type": "auto"})),
            "the answer was served to an auto request, so nothing about the request excused it"
        );

        wait_for_title_notifications(&client, 1).await;
        let (stored, manual) = stored_title(&session_id, &cwd).await;
        assert_eq!(
            stored, fallback,
            "only a call named session_title may carry the title: the wrong-named call's \
             arguments must not become the durable title"
        );
        assert!(!manual, "the fallback title is still a generated one");

        let (info, ext) = title_notification_counts(&client, WRONG_NAME_TITLE);
        assert_eq!(
            (info, ext),
            (0, 0),
            "a wrong-named title must never be notified; info={info:?} ext={ext:?}"
        );
    });
}

/// I-37 T3 preservation: with only the two post-turn features OFF the first title must still be
/// requested once, answered by the model, persisted and notified, and the main turn must still
/// complete — while NORMAL positively shows the two later side-call types those switches disable.
///
/// The parent's switches are not claimed defective: this is preservation, and its RED is
/// `CF-SUPPRESS-FIRST` / `CF-BYPASS-TITLE-CALL`.
#[test]
fn title_side_request_postturn_off_preserves_first_title() {
    let normal_refresh_latency = Arc::new(Mutex::new(None::<Duration>));

    // NORMAL first: its observed refresh latency calibrates the absence window the OFF leg uses.
    run_title_test_with_profile(Some(NORMAL_PROFILE), {
        let observed = Arc::clone(&normal_refresh_latency);
        move |cwd, state, _base_url| async move {
            let client = TitleClient::default();
            let conn = connect_offline_agent(client.clone(), "i37-t3-normal").await;
            let session_id = new_session_on_row(&conn, &cwd, OPUS_ROW).await;
            prompt_turn(&conn, &session_id, FIRST_PROMPT).await;

            let body = wait_for_nth_title_call(&state, "/v1/messages", OPUS_ROW, 1).await;
            assert_eq!(
                body.get("tool_choice"),
                Some(&json!({"type": "auto"})),
                "NORMAL: the first title request must be the one Auto request"
            );
            assert_eq!(
                calls_of_kind(&state, CallKind::InitialTitle),
                1,
                "NORMAL: exactly one first-title request"
            );
            wait_for_title_notification(&client, TITLE_SENTINEL).await;
            let (stored, manual) = stored_title(&session_id, &cwd).await;
            assert_eq!(
                stored, TITLE_SENTINEL,
                "NORMAL: the model title must persist, not a fallback"
            );
            assert!(!manual);

            let checkpoint_start = tokio::time::Instant::now();
            for turn in 2..=6 {
                prompt_turn(&conn, &session_id, &follow_up_prompt(turn)).await;
            }
            wait_for_call_kind(&state, CallKind::TitleRefresh, 1).await;
            let latency = checkpoint_start.elapsed();
            *observed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(latency);
            assert!(
                latency < SETTLE_ABSENCE,
                "the OFF absence window {SETTLE_ABSENCE:?} must be wider than the refresh latency\n                 actually observed here ({latency:?})"
            );
            assert!(
                calls_of_kind(&state, CallKind::TurnSummary) >= 1,
                "NORMAL must positively observe the per-turn summary whose absence OFF asserts"
            );
            assert_eq!(
                calls_of_kind(&state, CallKind::InitialTitle),
                1,
                "NORMAL: later turns never re-run the first-title call"
            );
            let (stored, _) = stored_title(&session_id, &cwd).await;
            assert!(
                !stored.is_empty() && stored != expected_fallback_title(FIRST_PROMPT),
                "NORMAL: the title stays a generated one after the refresh checkpoints"
            );
        }
    });

    assert!(
        normal_refresh_latency
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some(),
        "the NORMAL leg must reach its refresh checkpoint"
    );

    // POSTTURN_OFF: the identical driver, only the two feature values different.
    run_title_test_with_profile(Some(OFF_PROFILE), move |cwd, state, _base_url| async move {
        let client = TitleClient::default();
        let conn = connect_offline_agent(client.clone(), "i37-t3-off").await;
        let session_id = new_session_on_row(&conn, &cwd, OPUS_ROW).await;
        prompt_turn(&conn, &session_id, FIRST_PROMPT).await;

        let body = wait_for_nth_title_call(&state, "/v1/messages", OPUS_ROW, 1).await;
        assert_eq!(
            body.get("tool_choice"),
            Some(&json!({"type": "auto"})),
            "POSTTURN_OFF: the first title must still be requested once, with auto"
        );
        wait_for_title_notification(&client, TITLE_SENTINEL).await;
        let (stored, manual) = stored_title(&session_id, &cwd).await;
        assert_eq!(
            stored, TITLE_SENTINEL,
            "POSTTURN_OFF: the non-fallback model title must still be persisted"
        );
        assert!(!manual);
        assert!(
            client.streamed_text().contains(MAIN_TEXT),
            "POSTTURN_OFF: the main turn still completes"
        );

        for turn in 2..=6 {
            prompt_turn(&conn, &session_id, &follow_up_prompt(turn)).await;
        }
        assert!(
            client.streamed_text().matches(MAIN_TEXT).count() >= 6,
            "POSTTURN_OFF: all six turns must complete, got {:?}",
            client.streamed_text()
        );

        assert_no_further_call(&state, CallKind::TitleRefresh, 0).await;
        assert_no_further_call(&state, CallKind::TurnSummary, 0).await;
        assert_eq!(
            calls_of_kind(&state, CallKind::InitialTitle),
            1,
            "POSTTURN_OFF: the disabled later calls must not be replaced by extra first-title calls"
        );
        assert_eq!(
            calls_of_kind(&state, CallKind::Main),
            6,
            "POSTTURN_OFF: exactly the six main turns, no hidden side call"
        );
        let (stored, _) = stored_title(&session_id, &cwd).await;
        assert_eq!(
            stored, TITLE_SENTINEL,
            "POSTTURN_OFF: the first title survives the checkpoints untouched"
        );
    });
}

/// I-37 T3 one-attempt controls: the producer gets exactly one attempt per session — repeated
/// identical content never re-runs it, and a blank first chunk neither spends nor duplicates it.
#[test]
fn title_side_request_one_first_attempt() {
    run_title_test(move |cwd, state, _base_url| async move {
        let client = TitleClient::default();
        let conn = connect_offline_agent(client.clone(), "i37-t3-attempts").await;

        let first = new_session_on_row(&conn, &cwd, OPUS_ROW).await;
        prompt_turn(&conn, &first, FIRST_PROMPT).await;
        wait_for_call_kind(&state, CallKind::InitialTitle, 1).await;
        wait_for_title_notification(&client, TITLE_SENTINEL).await;

        for _ in 2..=3 {
            prompt_turn(&conn, &first, FIRST_PROMPT).await;
        }
        assert_no_further_call(&state, CallKind::InitialTitle, 1).await;
        let (stored, _) = stored_title(&first, &cwd).await;
        assert_eq!(
            stored, TITLE_SENTINEL,
            "duplicate content must not re-title the session"
        );

        // A blank first chunk must stay idle, so the next chunk with text gets the one attempt.
        let second = new_session_on_row(&conn, &cwd, SONNET_ROW).await;
        prompt_turn(&conn, &second, BLANK_USER_PROMPT).await;
        prompt_turn(&conn, &second, FIRST_PROMPT).await;
        let body = wait_for_nth_title_call(&state, "/v1/messages", SONNET_ROW, 1).await;
        assert!(
            body.to_string().contains(FIRST_PROMPT),
            "the blank chunk must not spend the attempt: the one title call on\n                 {SONNET_ROW} must carry the non-blank prompt"
        );
        assert_no_further_call(&state, CallKind::InitialTitle, 2).await;
        assert_eq!(
            title_calls_for(&state, "/v1/messages", SONNET_ROW).len(),
            1,
            "blank-then-text gets exactly one attempt"
        );
    });
}

/// I-37 T3 manual-rename preservation: a `/rename` that lands while the generated title is still
/// in flight wins, and the later whole-conversation refresh cannot flip it back.
#[test]
fn title_side_request_manual_rename_wins() {
    run_title_test_with_profile(
        Some(NORMAL_PROFILE),
        move |cwd, state, _base_url| async move {
            let client = TitleClient::default();
            let conn = connect_offline_agent(client.clone(), "i37-t3-rename").await;
            set_title_answer(&state, TitleAnswer::Delayed);

            let session_id = new_session_on_row(&conn, &cwd, OPUS_ROW).await;
            prompt_turn(&conn, &session_id, FIRST_PROMPT).await;
            wait_for_call_kind(&state, CallKind::InitialTitle, 1).await;

            let renamed = rename_session(&conn, &session_id, &cwd, MANUAL_TITLE).await;
            assert_eq!(
                renamed["success"], true,
                "the rename boundary must accept a manual title: {renamed}"
            );
            let (stored, manual) = stored_title(&session_id, &cwd).await;
            assert_eq!(stored, MANUAL_TITLE, "the manual title must be stored");
            assert!(manual, "a rename marks the title manual");

            release_title_gate(&state);
            // R-7: make the losing title arrive before asserting it was never adopted, so the
            // checks below are not satisfied by a title task that simply never completed.
            wait_for_delayed_title_answered(&state).await;
            // Past both TITLE_REFRESH_TURNS checkpoints (3 and 6). A manual rename does not merely
            // win the race: `on_title_renamed` (acp_session_impl/title_refresh.rs) moves the checkpoint
            // index to `TITLE_REFRESH_TURNS.len()`, which freezes the auto refresh, so the honest
            // oracle is that no refresh call happens at all and the manual title is never replaced.
            for turn in 2..=6 {
                prompt_turn(&conn, &session_id, &follow_up_prompt(turn)).await;
            }
            assert_no_further_call(&state, CallKind::TitleRefresh, 0).await;

            let (stored, manual) = stored_title(&session_id, &cwd).await;
            assert_eq!(
                stored, MANUAL_TITLE,
                "neither the late generated title nor a refresh may overwrite a manual rename"
            );
            assert!(manual);
            for title in client.info_update_titles() {
                assert_ne!(
                    title, TITLE_SENTINEL,
                    "a rejected generated title must never be notified"
                );
            }
            for title in client.ext_summary_titles() {
                assert_ne!(
                    title, TITLE_SENTINEL,
                    "a rejected generated title must never be notified (ext form)"
                );
            }
        },
    );
}
