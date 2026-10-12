//! End-to-end preservation for compatibility Responses terminals whose final
//! envelope omits (or nulls) `output` after valid content streamed earlier.

use super::support::*;
use super::*;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use xai_grok_test_support::sse::responses_api_script_exact;
use xai_grok_test_support::{MockInferenceServer, ScriptedResponse, SseEvent};

const TODO_ARGS: &str = r#"{"todos":[{"id":"t1","content":"poll","status":"completed"}]}"#;

#[derive(Clone, Copy, Debug)]
enum TerminalOutputShape {
    Missing,
    Null,
}

fn block_on_session(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(f)
        .expect("spawn large-stack test thread")
        .join()
        .expect("test thread");
}

fn current_thread_local<F>(future: F)
where
    F: std::future::Future<Output = ()> + 'static,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    tokio::task::LocalSet::new().block_on(&runtime, future);
}

fn sampler_fixture(id: &str) -> Vec<serde_json::Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../xai-grok-sampler/testdata/responses_terminal")
        .join(id)
        .join("events.json");
    serde_json::from_slice(
        &std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn compatibility_script(id: &str, shape: TerminalOutputShape, tool: bool) -> ScriptedResponse {
    let mut frames = sampler_fixture(id);
    for frame in &mut frames {
        let data = frame
            .get_mut("data")
            .and_then(serde_json::Value::as_object_mut)
            .expect("fixture data object");
        if tool
            && data.get("type").and_then(serde_json::Value::as_str)
                == Some("response.output_item.added")
        {
            let item = data
                .get_mut("item")
                .and_then(serde_json::Value::as_object_mut)
                .expect("V2 added tool item");
            item.insert(
                "name".to_owned(),
                serde_json::Value::String("todo_write".to_owned()),
            );
            item.insert(
                "arguments".to_owned(),
                serde_json::Value::String(TODO_ARGS.to_owned()),
            );
            item.insert(
                "status".to_owned(),
                serde_json::Value::String("completed".to_owned()),
            );
        }
        if matches!(
            data.get("type").and_then(serde_json::Value::as_str),
            Some("response.completed" | "response.incomplete" | "response.failed")
        ) {
            let response = data
                .get_mut("response")
                .and_then(serde_json::Value::as_object_mut)
                .expect("terminal response object");
            if tool
                && let Some(items) = response
                    .get_mut("output")
                    .and_then(serde_json::Value::as_array_mut)
            {
                for item in items {
                    if item.get("type").and_then(serde_json::Value::as_str) == Some("function_call")
                    {
                        let item = item.as_object_mut().expect("function-call object");
                        item.insert(
                            "name".to_owned(),
                            serde_json::Value::String("todo_write".to_owned()),
                        );
                        item.insert(
                            "arguments".to_owned(),
                            serde_json::Value::String(TODO_ARGS.to_owned()),
                        );
                    }
                }
            }
            match shape {
                TerminalOutputShape::Missing => {
                    response.remove("output");
                }
                TerminalOutputShape::Null => {
                    response.insert("output".to_owned(), serde_json::Value::Null);
                }
            }
        }
    }

    ScriptedResponse::sse(
        frames
            .into_iter()
            .map(|frame| {
                let event = frame
                    .get("event")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                let data =
                    serde_json::to_string(frame.get("data").expect("fixture frame carries data"))
                        .expect("serialize fixture data");
                match event {
                    Some(event) => SseEvent::with_event(event, data),
                    None => SseEvent::data(data),
                }
            })
            .collect(),
    )
}

async fn terminal_test_actor(
    server: &MockInferenceServer,
    with_todo: bool,
) -> (
    Arc<SessionActor>,
    tokio::sync::mpsc::UnboundedReceiver<SessionEvent>,
) {
    let sampling_cfg = xai_grok_sampler::SamplerConfig {
        api_key: Some("test-key".to_owned()),
        base_url: server.url(),
        model: "test".to_owned(),
        api_backend: xai_grok_sampler::ApiBackend::Responses,
        model_family: Some("codex".to_owned()),
        context_window: 256_000,
        max_retries: Some(0),
        idle_timeout_secs: Some(30),
        ..Default::default()
    };
    let (sampler_event_tx, mut sampler_event_rx) =
        tokio::sync::mpsc::unbounded_channel::<xai_grok_sampler::SamplingEvent>();
    let sampler_handle = xai_grok_sampler::SamplerActor::spawn(
        sampling_cfg,
        xai_grok_sampler::RetryPolicy {
            max_retries: 0,
            ..Default::default()
        },
        sampler_event_tx,
    );

    let (gateway_tx, mut gateway_rx) =
        tokio::sync::mpsc::unbounded_channel::<xai_acp_lib::AcpClientMessage>();
    tokio::task::spawn_local(async move {
        while let Some(message) = gateway_rx.recv().await {
            if let xai_acp_lib::AcpClientMessage::SessionNotification(args) = message {
                let _ = args.response_tx.send(Ok(()));
            }
        }
    });
    let (persistence_tx, persistence_rx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
    drain_persistence(persistence_rx);

    let (mut actor, event_rx) =
        create_test_actor_ex(0, 256_000, 85, gateway_tx, persistence_tx).await;
    actor.sampler_handle = sampler_handle;
    *actor.agent.borrow_mut() = if with_todo {
        test_grok_build_agent_with_todo().await
    } else {
        test_agent_default().await
    };
    let mut config = actor
        .chat_state_handle
        .get_sampling_config()
        .await
        .expect("test actor has sampling config");
    config.base_url = server.url();
    config.api_backend = xai_grok_sampling_types::ApiBackend::Responses;
    config.model = "test".to_owned();
    actor.chat_state_handle.update_sampling_config(config);
    let mut credentials = actor.chat_state_handle.get_credentials().await;
    credentials.api_key = Some("test-key".to_owned());
    actor.chat_state_handle.update_credentials(credentials);
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

    let actor = Arc::new(actor);
    let event_actor = Arc::clone(&actor);
    tokio::task::spawn_local(async move {
        while let Some(event) = sampler_event_rx.recv().await {
            event_actor.handle_sampling_event(event).await;
        }
    });
    (actor, event_rx)
}

async fn run_prompt(actor: &Arc<SessionActor>, prompt_id: &str) -> PromptTurnResult {
    tokio::time::timeout(
        Duration::from_secs(30),
        actor.handle_prompt(
            prompt_id,
            vec![acp::ContentBlock::Text(acp::TextContent::new(
                "exercise compatibility terminal".to_owned(),
            ))],
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
    .expect("turn terminates")
}

fn notification_text(notification: &acp::SessionNotification) -> Option<&str> {
    match &notification.update {
        acp::SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
            acp::ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        },
        _ => None,
    }
}

fn drain_notification_text(
    event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<SessionEvent>,
) -> String {
    let mut text = String::new();
    while let Ok(event) = event_rx.try_recv() {
        let SessionEvent::Notification(SessionNotification::Acp(notification)) = event else {
            continue;
        };
        if let Some(chunk) = notification_text(&notification) {
            text.push_str(chunk);
        }
    }
    text
}

#[test]
fn missing_null_accumulated_text_is_returned_and_admitted() {
    block_on_session(|| {
        current_thread_local(async {
            for shape in [TerminalOutputShape::Missing, TerminalOutputShape::Null] {
                let server = MockInferenceServer::start().await.expect("mock server");
                server.enqueue_response("/v1/responses", compatibility_script("V1", shape, false));
                let (actor, mut event_rx) = terminal_test_actor(&server, false).await;
                let outcome = run_prompt(&actor, &format!("terminal-text-{shape:?}")).await;
                assert!(outcome.is_ok(), "{shape:?} text turn: {outcome:?}");
                assert_eq!(server.request_count_for("/v1/responses"), 1);

                let emitted = drain_notification_text(&mut event_rx);
                assert!(
                    emitted.contains("ok"),
                    "{shape:?} returned text: {emitted:?}"
                );

                let conversation = actor.chat_state_handle.get_conversation().await;
                assert!(
                    conversation.iter().any(|item| {
                        matches!(item, ConversationItem::Assistant(assistant) if assistant.content.as_ref() == "ok")
                    }),
                    "{shape:?} accumulated text was not admitted: {conversation:#?}"
                );
            }
        });
    });
}

#[test]
fn missing_null_accumulated_tool_is_returned_admitted_and_executed_once() {
    block_on_session(|| {
        current_thread_local(async {
            for shape in [TerminalOutputShape::Missing, TerminalOutputShape::Null] {
                let server = MockInferenceServer::start().await.expect("mock server");
                server.enqueue_response("/v1/responses", compatibility_script("V2", shape, true));
                server.enqueue_response(
                    "/v1/responses",
                    ScriptedResponse::sse(responses_api_script_exact("done", "test")),
                );
                let (actor, _event_rx) = terminal_test_actor(&server, true).await;
                let outcome = run_prompt(&actor, &format!("terminal-tool-{shape:?}")).await;
                assert!(outcome.is_ok(), "{shape:?} tool turn: {outcome:?}");
                assert_eq!(
                    server.request_count_for("/v1/responses"),
                    2,
                    "one tool response plus one post-tool model round"
                );

                let conversation = actor.chat_state_handle.get_conversation().await;
                let matching_calls: Vec<_> = conversation
                    .iter()
                    .filter_map(|item| match item {
                        ConversationItem::Assistant(assistant) => Some(&assistant.tool_calls),
                        _ => None,
                    })
                    .flatten()
                    .filter(|call| &*call.id == "call_v2")
                    .collect();
                assert_eq!(matching_calls.len(), 1, "{shape:?} call count");
                assert_eq!(matching_calls[0].name.as_str(), "todo_write");
                assert_eq!(&*matching_calls[0].arguments, TODO_ARGS);

                let matching_results: Vec<_> = conversation
                    .iter()
                    .filter_map(|item| match item {
                        ConversationItem::ToolResult(result)
                            if result.tool_call_id == "call_v2" =>
                        {
                            Some(result)
                        }
                        _ => None,
                    })
                    .collect();
                assert_eq!(
                    matching_results.len(),
                    1,
                    "{shape:?} preserved tool executes and records one result"
                );
            }
        });
    });
}
