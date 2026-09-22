//! Integration tests for the actor and request_task layer.
//!
//! They live in `tests/` because they need a real `tokio::runtime` and a mock axum HTTP server for the `SamplingClient` to talk to.
//! Happy-path SSE payloads come from `xai_grok_test_support::sse`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::sse::{Event, Sse};
use axum::routing::post;
use futures_util::stream::{self, StreamExt};
use indexmap::IndexMap;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};

use xai_grok_sampler::{
    ApiBackend, RequestId, RetryPolicy, SamplerActor, SamplerConfig, SamplingChannel,
    SamplingErrorKind, SamplingEvent, StripReason,
};
use xai_grok_sampling_types::{
    ConversationItem, ConversationRequest, DoomLoopRecoveryPolicy, INVALID_IMAGE_ERROR_CODE,
    UserItem,
};
use xai_grok_test_support::{SseEvent, sse};

// ---------------------------------------------------------------------------
// Mock server harness
// ---------------------------------------------------------------------------

struct MockServer {
    addr: SocketAddr,
    shutdown_tx: oneshot::Sender<()>,
}

impl MockServer {
    async fn spawn(app: Router) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
        });
        // Give the server a moment to start.
        tokio::time::sleep(Duration::from_millis(20)).await;
        Self { addr, shutdown_tx }
    }

    fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
    }
}

// ---------------------------------------------------------------------------
// Config + request helpers
// ---------------------------------------------------------------------------

fn test_config(base_url: String, model: &str) -> SamplerConfig {
    SamplerConfig {
        api_key: Some("test-key".into()),
        base_url,
        mtls_cert_dir: None,
        model: model.into(),
        max_completion_tokens: Some(1024),
        temperature: None,
        top_p: None,
        api_backend: ApiBackend::ChatCompletions,
        auth_scheme: Default::default(),
        extra_headers: IndexMap::new(),
        extra_response_includes: Vec::new(),
        query_params: IndexMap::new(),
        env_http_headers: IndexMap::new(),
        context_window: 128_000,
        force_http1: false,
        // Keep retries minimal so tests don't take forever.
        max_retries: Some(2),
        rate_limit_retry_threshold: None,
        stream_tool_calls: false,
        idle_timeout_secs: Some(30),
        reasoning_effort: None,
        origin_client: None,
        client_identifier: None,
        deployment_id: None,
        user_id: None,
        conversation_group_id: None,
        client_version: None,
        attribution_callback: None,
        bearer_resolver: None,
        supports_backend_search: false,
        compactions_remaining: None,
        compaction_at_tokens: None,
        doom_loop_recovery: None,
        header_injector: None,
        model_family: None,
        strict_responses_input: false,
        normalize_content_types: false,
        ultra_wire_effort: None,
        cache_ttl: None,
        thinking_replay: None,
        top_k: None,
        stop_sequences: None,
        disable_parallel_tool_use: None,
        tool_cache_breakpoint: None,
        server_tools: None,
        mcp_servers: None,
        mcp_toolset_server: None,
    }
}

fn user_request(text: &str) -> ConversationRequest {
    ConversationRequest {
        items: vec![ConversationItem::User(UserItem {
            content: vec![xai_grok_sampling_types::ContentPart::Text {
                text: std::sync::Arc::<str>::from(text),
            }],
            synthetic_reason: None,
            ..Default::default()
        })],
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// SSE generators
// ---------------------------------------------------------------------------

/// Render test-helper [`SseEvent`]s (optional `event:` name and `data:`) as axum SSE events for this file's router-based harness.
fn sse_events_to_axum(events: Vec<SseEvent>) -> Vec<Event> {
    events
        .into_iter()
        .map(|e| {
            let ev = Event::default().data(e.data);
            match e.event {
                Some(name) => ev.event(name),
                None => ev,
            }
        })
        .collect()
}

/// Reshape events the way an OpenAI-compatible gateway (LiteLLM) emits them: no `sequence_number`,
/// and output items missing the fields `async_openai` marks required but that gateways omit.
fn as_gateway_payload(events: Vec<SseEvent>) -> Vec<SseEvent> {
    fn strip_item(item: &mut serde_json::Value) {
        let Some(obj) = item.as_object_mut() else {
            return;
        };
        obj.remove("status");
        match obj.get("type").and_then(|v| v.as_str()) {
            Some("reasoning") => {
                obj.remove("summary");
            }
            Some("message") => {
                if let Some(content) = obj.get_mut("content").and_then(|v| v.as_array_mut()) {
                    for part in content {
                        if let Some(part) = part.as_object_mut() {
                            part.remove("annotations");
                        }
                    }
                }
            }
            _ => {}
        }
    }

    events
        .into_iter()
        .map(|mut e| {
            if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&e.data) {
                if let Some(obj) = v.as_object_mut() {
                    obj.remove("sequence_number");
                }
                if let Some(item) = v.pointer_mut("/item") {
                    strip_item(item);
                }
                if let Some(output) = v
                    .pointer_mut("/response/output")
                    .and_then(|v| v.as_array_mut())
                {
                    for item in output {
                        strip_item(item);
                    }
                }
                e.data = v.to_string();
            }
            e
        })
        .collect()
}

/// Drop `sequence_number` from every event payload, reproducing what OpenAI-compatible
/// gateways such as LiteLLM emit on the Responses wire.
fn strip_sequence_numbers(events: Vec<SseEvent>) -> Vec<SseEvent> {
    events
        .into_iter()
        .map(|mut e| {
            if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&e.data) {
                if let Some(obj) = v.as_object_mut() {
                    obj.remove("sequence_number");
                }
                e.data = v.to_string();
            }
            e
        })
        .collect()
}

/// Drop `summary_index` from every event payload, reproducing what LiteLLM's responses-compat
/// synthesis emits on reasoning-summary frames (OpenAI always sends the field).
fn strip_summary_index(events: Vec<SseEvent>) -> Vec<SseEvent> {
    events
        .into_iter()
        .map(|mut e| {
            if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&e.data) {
                if let Some(obj) = v.as_object_mut() {
                    obj.remove("summary_index");
                }
                e.data = v.to_string();
            }
            e
        })
        .collect()
}

fn text_chunk_event(content: &str, finish: bool) -> Event {
    let chunk = json!({
        "id": "chatcmpl-test",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "test-model",
        "choices": [{
            "index": 0,
            "delta": { "role": "assistant", "content": content },
            "finish_reason": if finish { json!("stop") } else { json!(null) }
        }]
    });
    Event::default().data(chunk.to_string())
}

// ---------------------------------------------------------------------------
// Actor lifecycle
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_then_active_count_zero_then_cancel_unknown_is_noop() {
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let cfg = test_config("http://127.0.0.1:0/v1".into(), "test-model");
    let handle = SamplerActor::spawn(cfg, RetryPolicy::default(), event_tx);
    assert_eq!(handle.active_count().await, 0);
    handle.cancel(RequestId::from("nonexistent"));
    assert_eq!(handle.active_count().await, 0);
}

// ---------------------------------------------------------------------------
// Submit + event flow
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submit_emits_started_first_token_channel_completed() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let events = sse::chat_completion_events("hello world", "test-model");
            Sse::new(stream::iter(
                events.into_iter().map(Ok::<_, std::convert::Infallible>),
            ))
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let cfg = test_config(server.base_url(), "test-model");
    let handle = SamplerActor::spawn(cfg, RetryPolicy::default(), event_tx);

    let rid = RequestId::from("req-1");
    handle.submit(rid.clone(), user_request("hi"));

    let events = drain_until_terminal(&mut event_rx, Duration::from_secs(5)).await;
    server.shutdown();

    assert!(matches!(events[0], SamplingEvent::StreamStarted { .. }));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SamplingEvent::FirstToken { .. }))
    );

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
    assert_eq!(texts.join(""), "hello world");

    match events.last().unwrap() {
        SamplingEvent::Completed {
            request_id,
            response,
            ..
        } => {
            assert_eq!(request_id, &rid);
            if let Some(a) = response.assistant() {
                assert_eq!(a.content.as_ref(), "hello world");
            } else {
                panic!("expected Assistant message");
            }
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// submit_and_collect
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submit_and_collect_returns_response() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let events = sse::chat_completion_events("collected response", "test-model");
            Sse::new(stream::iter(
                events.into_iter().map(Ok::<_, std::convert::Infallible>),
            ))
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let cfg = test_config(server.base_url(), "test-model");
    let handle = SamplerActor::spawn(cfg, RetryPolicy::default(), event_tx);

    let rid = RequestId::from("req-collect");
    let result = handle
        .submit_and_collect(rid, user_request("hi"))
        .await
        .expect("collected ok");
    server.shutdown();

    let (response, _metrics) = result;
    let a = response.assistant().expect("assistant item present");
    assert_eq!(a.content.as_ref(), "collected response");
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_in_flight_request_terminates_task() {
    // The server yields one chunk then hangs
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let stream = stream::iter(vec![Ok::<_, std::convert::Infallible>(text_chunk_event(
                "starting", false,
            ))])
            .chain(stream::pending());
            Sse::new(stream)
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let cfg = test_config(server.base_url(), "test-model");
    let handle = SamplerActor::spawn(cfg, RetryPolicy::default(), event_tx);

    let rid = RequestId::from("req-cancel");
    handle.submit(rid.clone(), user_request("hi"));

    // Wait for the first token to arrive so we know the request is in flight.
    let _ = await_event_matching(
        &mut event_rx,
        |e| matches!(e, SamplingEvent::FirstToken { .. }),
        Duration::from_secs(5),
    )
    .await
    .expect("first token");

    handle.cancel(rid.clone());

    // Expect a Failed event with the cancellation message.
    let failed = await_event_matching(
        &mut event_rx,
        |e| matches!(e, SamplingEvent::Failed { .. }),
        Duration::from_secs(5),
    )
    .await
    .expect("Failed event after cancel");

    if let SamplingEvent::Failed { error, .. } = failed {
        assert!(error.message.contains("cancelled"));
    }

    // Wait briefly for the task to clean up.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(handle.active_count().await, 0);
    server.shutdown();
}

// ---------------------------------------------------------------------------
// Concurrent requests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_concurrent_requests_complete_with_correct_request_ids() {
    let counter = Arc::new(AtomicU32::new(0));
    let counter_handler = Arc::clone(&counter);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let counter = Arc::clone(&counter_handler);
            async move {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                let events = sse::chat_completion_events(&format!("response-{n}"), "test-model");
                Sse::new(stream::iter(
                    events.into_iter().map(Ok::<_, std::convert::Infallible>),
                ))
            }
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let cfg = test_config(server.base_url(), "test-model");
    let handle = SamplerActor::spawn(cfg, RetryPolicy::default(), event_tx);

    let rid_a = RequestId::from("req-a");
    let rid_b = RequestId::from("req-b");
    handle.submit(rid_a.clone(), user_request("a"));
    handle.submit(rid_b.clone(), user_request("b"));

    // Drain until we see Completed for both.
    let mut completed_a = false;
    let mut completed_b = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !(completed_a && completed_b) {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            panic!(
                "timed out waiting for both requests to complete: a={completed_a}, b={completed_b}"
            );
        }
        let remaining = deadline - now;
        match tokio::time::timeout(remaining, event_rx.recv()).await {
            Ok(Some(SamplingEvent::Completed { request_id, .. })) if request_id == rid_a => {
                completed_a = true;
            }
            Ok(Some(SamplingEvent::Completed { request_id, .. })) if request_id == rid_b => {
                completed_b = true;
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("event channel closed"),
            Err(_) => panic!("timeout"),
        }
    }
    server.shutdown();
}

// ---------------------------------------------------------------------------
// Retry on transient transport error
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retries_on_500_then_succeeds() {
    let counter = Arc::new(AtomicU32::new(0));
    let counter_handler = Arc::clone(&counter);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let counter = Arc::clone(&counter_handler);
            async move {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    // First attempt: server error.
                    Err::<Sse<_>, (StatusCode, String)>((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        json!({ "error": { "message": "transient" } }).to_string(),
                    ))
                } else {
                    // Subsequent attempts: success.
                    let events = sse::chat_completion_events("ok", "test-model");
                    Ok(Sse::new(stream::iter(
                        events.into_iter().map(Ok::<_, std::convert::Infallible>),
                    )))
                }
            }
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    // Lots of retries available; backoff is jittered around 2s on first retry, so this test takes a bit to run
    let cfg = test_config(server.base_url(), "test-model");
    let handle = SamplerActor::spawn(cfg, RetryPolicy::default(), event_tx);

    let rid = RequestId::from("req-retry");
    handle.submit(rid.clone(), user_request("hi"));

    let events = drain_until_terminal(&mut event_rx, Duration::from_secs(15)).await;
    server.shutdown();

    let saw_retrying = events
        .iter()
        .any(|e| matches!(e, SamplingEvent::Retrying { .. }));
    assert!(saw_retrying, "expected at least one Retrying event");

    match events.last().unwrap() {
        SamplingEvent::Completed { response, .. } => {
            if let Some(a) = response.assistant() {
                assert_eq!(a.content.as_ref(), "ok");
            }
        }
        other => panic!("expected Completed after retry, got {other:?}"),
    }

    assert!(
        counter.load(Ordering::SeqCst) >= 2,
        "server hit at least twice"
    );
}

/// A coded `invalid_image` 400 strips the image, emits ServerRejected, retries, and completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_image_code_strips_and_retries() {
    const IMAGE_URI: &str = "data:image/png;base64,cG9pc29uZWQ=";
    let bodies = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let bodies_handler = Arc::clone(&bodies);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |body: String| {
            let bodies = Arc::clone(&bodies_handler);
            async move {
                let n = {
                    let mut b = bodies.lock().unwrap();
                    b.push(body);
                    b.len()
                };
                if n == 1 {
                    // The FLAT envelope the xAI API's non-stream rejections actually use; the message alone must not matter
                    Err::<Sse<_>, (StatusCode, String)>((
                        StatusCode::BAD_REQUEST,
                        json!({
                            "code": INVALID_IMAGE_ERROR_CODE,
                            "error": "some future wording without the legacy phrase",
                        })
                        .to_string(),
                    ))
                } else {
                    let events = sse::chat_completion_events("recovered", "test-model");
                    Ok(Sse::new(stream::iter(
                        events.into_iter().map(Ok::<_, std::convert::Infallible>),
                    )))
                }
            }
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let handle = SamplerActor::spawn(
        test_config(server.base_url(), "test-model"),
        RetryPolicy::default(),
        event_tx,
    );

    let mut request = user_request("what is in this image?");
    if let Some(ConversationItem::User(u)) = request.items.first_mut() {
        u.add_image(IMAGE_URI);
    }
    handle.submit(RequestId::from("req-image-code-strip"), request);

    let events = drain_until_terminal(&mut event_rx, Duration::from_secs(15)).await;
    server.shutdown();

    assert!(
        events.iter().any(|e| match e {
            SamplingEvent::ImagesStripped {
                stripped_urls,
                reason: xai_grok_sampler::StripReason::ServerRejected,
                ..
            } => stripped_urls.len() == 1 && stripped_urls[0].as_ref() == IMAGE_URI,
            _ => false,
        }),
        "expected server-rejected ImagesStripped carrying the poisoned URL, got {events:?}"
    );
    assert!(
        matches!(events.last(), Some(SamplingEvent::Completed { .. })),
        "expected Completed after strip-retry"
    );

    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2, "one rejection, one strip-retry");
    assert!(bodies[0].contains(IMAGE_URI), "first attempt sends image");
    assert!(
        !bodies[1].contains(IMAGE_URI),
        "strip-retry must not resend the image"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn responses_invalid_image_strips_as_server_rejected() {
    const IMAGE_URI: &str = "data:image/png;base64,cG9pc29uZWQ=";
    let bodies = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let bodies_handler = Arc::clone(&bodies);
    let app = Router::new().route(
        "/v1/responses",
        post(move |body: String| {
            let bodies = Arc::clone(&bodies_handler);
            async move {
                let n = {
                    let mut b = bodies.lock().unwrap();
                    b.push(body);
                    b.len()
                };
                if n == 1 {
                    Err::<Sse<_>, (StatusCode, String)>((
                        StatusCode::BAD_REQUEST,
                        json!({
                            "code": INVALID_IMAGE_ERROR_CODE,
                            "error": "Invalid PNG image.",
                        })
                        .to_string(),
                    ))
                } else {
                    let events = sse_events_to_axum(sse::responses_api_reasoning_and_text_events(
                        "ok",
                        "recovered",
                        "test-model",
                    ));
                    Ok(Sse::new(stream::iter(
                        events.into_iter().map(Ok::<_, std::convert::Infallible>),
                    )))
                }
            }
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let handle = SamplerActor::spawn(
        responses_config(server.base_url(), None),
        RetryPolicy::default(),
        event_tx,
    );

    let mut request = user_request("what is in this image?");
    if let Some(ConversationItem::User(u)) = request.items.first_mut() {
        u.add_image(IMAGE_URI);
    }
    handle.submit(RequestId::from("req-responses-invalid-image"), request);

    let events = drain_until_terminal(&mut event_rx, Duration::from_secs(15)).await;
    server.shutdown();

    assert!(
        events.iter().any(|e| match e {
            SamplingEvent::ImagesStripped {
                stripped_urls,
                reason: StripReason::ServerRejected,
                ..
            } => stripped_urls.len() == 1 && stripped_urls[0].as_ref() == IMAGE_URI,
            _ => false,
        }),
        "Responses invalid_image must strip as ServerRejected, got {events:?}"
    );
    assert!(
        matches!(events.last(), Some(SamplingEvent::Completed { .. })),
        "expected Completed after strip-retry"
    );
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2, "one rejection, one strip-retry");
    assert!(bodies[0].contains(IMAGE_URI), "first attempt sends image");
    assert!(
        !bodies[1].contains(IMAGE_URI),
        "strip-retry must not resend the image"
    );
}

/// A legacy-phrase 400 with no code still strips and recovers, but the reason is `PayloadHeuristic`.
/// Without the deterministic code the server blamed nothing specific, so the strip must stay request-local.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_phrase_400_strips_as_heuristic() {
    const IMAGE_URI: &str = "data:image/png;base64,cG9pc29uZWQ=";
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter_handler = Arc::clone(&counter);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let counter = Arc::clone(&counter_handler);
            async move {
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err::<Sse<_>, (StatusCode, String)>((
                        StatusCode::BAD_REQUEST,
                        json!({
                            "error": {
                                "message": "Could not process image",
                                "type": "invalid_request_error",
                            }
                        })
                        .to_string(),
                    ))
                } else {
                    let events = sse::chat_completion_events("recovered", "test-model");
                    Ok(Sse::new(stream::iter(
                        events.into_iter().map(Ok::<_, std::convert::Infallible>),
                    )))
                }
            }
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let handle = SamplerActor::spawn(
        test_config(server.base_url(), "test-model"),
        RetryPolicy::default(),
        event_tx,
    );

    let mut request = user_request("what is in this image?");
    if let Some(ConversationItem::User(u)) = request.items.first_mut() {
        u.add_image(IMAGE_URI);
    }
    handle.submit(RequestId::from("req-legacy-phrase-strip"), request);

    let events = drain_until_terminal(&mut event_rx, Duration::from_secs(15)).await;
    server.shutdown();

    assert!(
        events.iter().any(|e| matches!(
            e,
            SamplingEvent::ImagesStripped {
                reason: StripReason::PayloadHeuristic,
                ..
            }
        )),
        "codeless legacy-phrase 400 must strip as PayloadHeuristic, got {events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            SamplingEvent::ImagesStripped {
                reason: StripReason::ServerRejected,
                ..
            }
        )),
        "no deterministic code, so never ServerRejected: {events:?}"
    );
    assert!(
        matches!(events.last(), Some(SamplingEvent::Completed { .. })),
        "expected Completed after strip-retry"
    );
}

/// Guards that `user_facing_api_error_message` keeps the `.image.source` path in a codeless `invalid_request_error`.
/// That way the codeless image-strip recovery fires on many-image dimension 400s instead of hard-failing every turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn many_image_dimension_400_strips_as_heuristic() {
    const IMAGE_URI: &str = "data:image/png;base64,cG9pc29uZWQ=";
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter_handler = Arc::clone(&counter);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let counter = Arc::clone(&counter_handler);
            async move {
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err::<Sse<_>, (StatusCode, String)>((
                        StatusCode::BAD_REQUEST,
                        json!({
                            "error": {
                                "message": "messages.0.content.4.image.source.base64.data: At least one of the image dimensions exceed max allowed size for many-image requests: 2000 pixels",
                                "type": "invalid_request_error",
                            }
                        })
                        .to_string(),
                    ))
                } else {
                    let events = sse::chat_completion_events("recovered", "test-model");
                    Ok(Sse::new(stream::iter(
                        events.into_iter().map(Ok::<_, std::convert::Infallible>),
                    )))
                }
            }
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let handle = SamplerActor::spawn(
        test_config(server.base_url(), "test-model"),
        RetryPolicy::default(),
        event_tx,
    );

    let mut request = user_request("what is in this image?");
    if let Some(ConversationItem::User(u)) = request.items.first_mut() {
        u.add_image(IMAGE_URI);
    }
    handle.submit(RequestId::from("req-many-image-dimension-strip"), request);

    let events = drain_until_terminal(&mut event_rx, Duration::from_secs(15)).await;
    server.shutdown();

    assert!(
        events.iter().any(|e| matches!(
            e,
            SamplingEvent::ImagesStripped {
                reason: StripReason::PayloadHeuristic,
                ..
            }
        )),
        "codeless many-image dimension 400 must strip as PayloadHeuristic, got {events:?}"
    );
    assert!(
        matches!(events.last(), Some(SamplingEvent::Completed { .. })),
        "expected Completed after strip-retry"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn image_400_with_nothing_left_to_strip_is_fatal_after_one_cycle() {
    // `stripped == 0` is the only bound on the strip-retry loop.
    let counter = Arc::new(AtomicU32::new(0));
    let counter_handler = Arc::clone(&counter);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let counter = Arc::clone(&counter_handler);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err::<Sse<futures_util::stream::Empty<Result<Event, std::convert::Infallible>>>, _>(
                    (
                        StatusCode::BAD_REQUEST,
                        json!({
                            "code": INVALID_IMAGE_ERROR_CODE,
                            "error": "Base64 string of provided image cannot be decoded.",
                        })
                        .to_string(),
                    ),
                )
            }
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let handle = SamplerActor::spawn(
        test_config(server.base_url(), "test-model"),
        RetryPolicy::default(),
        event_tx,
    );

    let mut request = user_request("what is in this image?");
    if let Some(ConversationItem::User(u)) = request.items.first_mut() {
        u.add_image("data:image/png;base64,cG9pc29uZWQ=");
    }
    handle.submit(RequestId::from("req-strip-exhausted"), request);

    let events = drain_until_terminal(&mut event_rx, Duration::from_secs(15)).await;
    server.shutdown();

    let strips = events
        .iter()
        .filter(|e| matches!(e, SamplingEvent::ImagesStripped { .. }))
        .count();
    assert_eq!(strips, 1, "exactly one strip cycle");
    assert!(
        matches!(events.last(), Some(SamplingEvent::Failed { .. })),
        "second image 400 with nothing left to strip must be fatal, got {events:?}"
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        2,
        "one rejection, one strip-retry, then stop"
    );
}

/// RST with a zero retry budget: the decision is Fatal, so the proactive heuristic strip must NOT run.
/// There is no mutation, no ImagesStripped event, and no "left out of the retry" note for a retry that never happens.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fatal_decision_does_not_strip_or_emit_images_stripped() {
    const IMAGE_URI: &str = "data:image/png;base64,cG9pc29uZWQ=";
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        // RST every connection: peek then drop (see xai-grok-http).
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                accepted = listener.accept() => {
                    let Ok((sock, _)) = accepted else { break };
                    let mut buf = [0u8; 64];
                    let _ = sock.peek(&mut buf).await;
                    drop(sock);
                }
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let mut config = test_config(format!("http://{addr}/v1"), "test-model");
    config.max_retries = Some(0);
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let handle = SamplerActor::spawn(config, RetryPolicy::default(), event_tx);

    let mut request = user_request("what is in this image?");
    if let Some(ConversationItem::User(u)) = request.items.first_mut() {
        u.add_image(IMAGE_URI);
    }
    handle.submit(RequestId::from("req-fatal-no-strip"), request);

    let events = drain_until_terminal(&mut event_rx, Duration::from_secs(15)).await;
    let _ = shutdown_tx.send(());

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SamplingEvent::ImagesStripped { .. })),
        "a Fatal decision must not strip or emit ImagesStripped, got {events:?}"
    );
    assert!(
        matches!(events.last(), Some(SamplingEvent::Failed { .. })),
        "expected terminal Failed, got {events:?}"
    );
}

/// An RST mid-upload (nginx-style 413) emits PayloadHeuristic and strips the request only.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_reset_emits_payload_heuristic_and_strips_request() {
    const IMAGE_URI: &str = "data:image/png;base64,cG9pc29uZWQ=";
    let bodies = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let bodies_handler = Arc::clone(&bodies);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        // Peek then drop so the peer sees RST (see xai-grok-http).
        if let Ok((sock, _)) = listener.accept().await {
            let mut buf = [0u8; 64];
            let _ = sock.peek(&mut buf).await;
            drop(sock);
        }
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move |body: String| {
                let bodies = Arc::clone(&bodies_handler);
                async move {
                    bodies.lock().unwrap().push(body);
                    let events = sse::chat_completion_events("recovered", "test-model");
                    Sse::new(stream::iter(
                        events.into_iter().map(Ok::<_, std::convert::Infallible>),
                    ))
                }
            }),
        );
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let handle = SamplerActor::spawn(
        test_config(format!("http://{addr}/v1"), "test-model"),
        RetryPolicy::default(),
        event_tx,
    );

    let mut request = user_request("what is in this image?");
    if let Some(ConversationItem::User(u)) = request.items.first_mut() {
        u.add_image(IMAGE_URI);
    }
    handle.submit(RequestId::from("req-heuristic-strip"), request);

    let events = drain_until_terminal(&mut event_rx, Duration::from_secs(15)).await;
    let _ = shutdown_tx.send(());

    assert!(
        events.iter().any(|e| match e {
            SamplingEvent::ImagesStripped {
                stripped_urls,
                reason: StripReason::PayloadHeuristic,
                ..
            } => stripped_urls.len() == 1 && stripped_urls[0].as_ref() == IMAGE_URI,
            _ => false,
        }),
        "connection reset must emit PayloadHeuristic ImagesStripped, got {events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            SamplingEvent::ImagesStripped {
                reason: StripReason::ServerRejected,
                ..
            }
        )),
        "heuristic path must not be labeled ServerRejected, got {events:?}"
    );
    assert!(
        matches!(events.last(), Some(SamplingEvent::Completed { .. })),
        "strip-retry must complete, got {events:?}"
    );
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1, "only the post-strip retry hits HTTP");
    assert!(
        !bodies[0].contains(IMAGE_URI),
        "in-flight request must be stripped before the retry"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_failure_does_not_emit_images_stripped() {
    // Connection refused is `is_connect`, not a body-upload reset.
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let handle = SamplerActor::spawn(
        test_config("http://127.0.0.1:1/v1".into(), "test-model"),
        RetryPolicy::default(),
        event_tx,
    );

    let mut request = user_request("what is in this image?");
    if let Some(ConversationItem::User(u)) = request.items.first_mut() {
        u.add_image("data:image/png;base64,cG9pc29uZWQ=");
    }
    handle.submit(RequestId::from("req-connect-fail"), request);

    let events = drain_until_terminal(&mut event_rx, Duration::from_secs(15)).await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SamplingEvent::ImagesStripped { .. })),
        "connect failure must not strip images, got {events:?}"
    );
    assert!(
        matches!(events.last(), Some(SamplingEvent::Failed { .. })),
        "exhausted connect retries must be Failed, got {events:?}"
    );
}

// ---------------------------------------------------------------------------
// Rate-limit thresholds
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rate_limit_exhausts_at_default_threshold_and_yields_failed() {
    let counter = Arc::new(AtomicU32::new(0));
    let counter_handler = Arc::clone(&counter);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let counter = Arc::clone(&counter_handler);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err::<
                    Sse<
                        futures_util::stream::Iter<
                            std::vec::IntoIter<Result<Event, std::convert::Infallible>>,
                        >,
                    >,
                    (StatusCode, String),
                >((
                    StatusCode::TOO_MANY_REQUESTS,
                    json!({ "error": { "message": "slow down" } }).to_string(),
                ))
            }
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let cfg = test_config(server.base_url(), "test-model");
    let handle = SamplerActor::spawn(cfg, RetryPolicy::default(), event_tx);

    let rid = RequestId::from("req-429-default");
    handle.submit(rid, user_request("hi"));

    let events = drain_until_terminal(&mut event_rx, Duration::from_secs(60)).await;
    server.shutdown();

    match events.last().unwrap() {
        SamplingEvent::Failed { error, .. } => {
            assert_eq!(error.kind, SamplingErrorKind::RateLimited);
            assert_eq!(error.status_code, Some(429));
        }
        other => panic!("expected Failed(RateLimited), got {other:?}"),
    }

    // The request task awaits and classifies each wire attempt before starting the next, so scheduling cannot add another request.
    assert_eq!(
        counter.load(Ordering::SeqCst),
        2,
        "the default threshold permits one retry after the initial request"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_rate_limit_threshold_controls_total_wire_attempts() {
    let counter = Arc::new(AtomicU32::new(0));
    let counter_handler = Arc::clone(&counter);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let counter = Arc::clone(&counter_handler);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", "0")],
                    json!({ "error": { "message": "slow down" } }).to_string(),
                )
            }
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let mut cfg = test_config(server.base_url(), "test-model");
    cfg.max_retries = Some(6);
    cfg.rate_limit_retry_threshold = Some(4);
    let handle = SamplerActor::spawn(cfg, RetryPolicy::default(), event_tx);

    let rid = RequestId::from("req-429");
    handle.submit(rid.clone(), user_request("hi"));

    let events = drain_until_terminal(&mut event_rx, Duration::from_secs(60)).await;
    server.shutdown();

    match events.last().unwrap() {
        SamplingEvent::Failed { error, .. } => {
            assert_eq!(error.kind, SamplingErrorKind::RateLimited);
            assert_eq!(error.status_code, Some(429));
        }
        other => panic!("expected Failed(RateLimited), got {other:?}"),
    }

    let hits = counter.load(Ordering::SeqCst);
    assert_eq!(
        hits, 4,
        "the configured threshold is a total-attempt ceiling and must override the policy default of 2"
    );
}

// ---------------------------------------------------------------------------
// Auth error -> EmitToSession (immediate)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_401_emits_failed_immediately_no_retry() {
    let counter = Arc::new(AtomicU32::new(0));
    let counter_handler = Arc::clone(&counter);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let counter = Arc::clone(&counter_handler);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err::<
                    Sse<
                        futures_util::stream::Iter<
                            std::vec::IntoIter<Result<Event, std::convert::Infallible>>,
                        >,
                    >,
                    (StatusCode, String),
                >((StatusCode::UNAUTHORIZED, "unauthorized".to_string()))
            }
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let cfg = test_config(server.base_url(), "test-model");
    let handle = SamplerActor::spawn(cfg, RetryPolicy::default(), event_tx);

    let rid = RequestId::from("req-auth");
    handle.submit(rid.clone(), user_request("hi"));

    let events = drain_until_terminal(&mut event_rx, Duration::from_secs(5)).await;
    server.shutdown();

    // The session owns auth errors: `classify_error` returns `EmitToSession`, so the actor emits Failed immediately without retrying
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SamplingEvent::Retrying { .. }))
    );
    match events.last().unwrap() {
        SamplingEvent::Failed { error, .. } => {
            assert_eq!(error.kind, SamplingErrorKind::Auth);
        }
        other => panic!("expected Failed(Auth), got {other:?}"),
    }
    assert_eq!(counter.load(Ordering::SeqCst), 1, "no retries on 401");
}

// ---------------------------------------------------------------------------
// Anthropic Messages API: refusal stop_reason + mid-stream parse failure
// ---------------------------------------------------------------------------

fn messages_config(base_url: String) -> SamplerConfig {
    let mut cfg = test_config(base_url, "messages-compatible-model");
    cfg.api_backend = ApiBackend::Messages;
    cfg
}

/// Regression for the refusal-stop_reason incident.
/// A well-formed stream terminated by `stop_reason: "refusal"` must produce a successful completion from EXACTLY ONE request, no retry storm.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_refusal_stream_completes_with_single_request() {
    let counter = Arc::new(AtomicU32::new(0));
    let counter_handler = Arc::clone(&counter);
    let app = Router::new().route(
        "/v1/messages",
        post(move || {
            let counter = Arc::clone(&counter_handler);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let events = sse::messages_api_events(
                    "I can't help with that.",
                    "messages-compatible-model",
                    "refusal",
                );
                Sse::new(stream::iter(
                    events.into_iter().map(Ok::<_, std::convert::Infallible>),
                ))
            }
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let handle = SamplerActor::spawn(
        messages_config(server.base_url()),
        RetryPolicy::default(),
        event_tx,
    );

    let result = handle
        .submit_and_collect(RequestId::from("req-refusal"), user_request("hi"))
        .await;
    server.shutdown();

    let (response, _metrics) = result.expect("refusal-terminated turn must complete");
    let a = response.assistant().expect("assistant item present");
    assert_eq!(a.content.as_ref(), "I can't help with that.");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "refusal must not trigger retries"
    );
}

/// Empty-bodied refusal: `message_start → message_delta(refusal) → message_stop` with zero content blocks must complete from exactly one request.
/// The content-less response must not be classified as a retryable EmptyResponse.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_empty_refusal_completes_without_retry() {
    let counter = Arc::new(AtomicU32::new(0));
    let counter_handler = Arc::clone(&counter);
    let app = Router::new().route(
        "/v1/messages",
        post(move || {
            let counter = Arc::clone(&counter_handler);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let mut events =
                    sse::messages_api_events("", "messages-compatible-model", "refusal");
                // Drop the content block events; keep start/delta/stop only.
                events.drain(1..4);
                Sse::new(stream::iter(
                    events.into_iter().map(Ok::<_, std::convert::Infallible>),
                ))
            }
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let handle = SamplerActor::spawn(
        messages_config(server.base_url()),
        RetryPolicy::default(),
        event_tx,
    );

    handle.submit(RequestId::from("req-empty-refusal"), user_request("hi"));
    let events = drain_until_terminal(&mut event_rx, Duration::from_secs(10)).await;
    server.shutdown();

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SamplingEvent::Retrying { .. })),
        "content-less refusal must not be retried"
    );
    match events.last().unwrap() {
        SamplingEvent::Completed { response, .. } => {
            assert_eq!(
                response.stop_reason,
                Some(xai_grok_sampling_types::StopReason::ContentFilter)
            );
        }
        other => panic!("expected Completed, got {other:?}"),
    }
    assert_eq!(counter.load(Ordering::SeqCst), 1, "exactly one request");
}

/// A mid-stream event that fails serde (after a valid `message_start`) is a deterministic response-parse failure.
/// It is Fatal on the first attempt and surfaces as a non-retryable Serialization error, never a retry storm.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_unparseable_event_is_fatal_without_retry() {
    let counter = Arc::new(AtomicU32::new(0));
    let counter_handler = Arc::clone(&counter);
    let app =
        Router::new().route(
            "/v1/messages",
            post(move || {
                let counter = Arc::clone(&counter_handler);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let mut events =
                        sse::messages_api_events("hello", "messages-compatible-model", "end_turn");
                    // Replace the tail with a `message_delta` missing the required `delta` field, which fails MessageStreamEvent serde
                    events.truncate(4);
                    events.push(Event::default().data(
                        json!({"type":"message_delta","usage":{"output_tokens":1}}).to_string(),
                    ));
                    Sse::new(stream::iter(
                        events.into_iter().map(Ok::<_, std::convert::Infallible>),
                    ))
                }
            }),
        );
    let server = MockServer::spawn(app).await;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let handle = SamplerActor::spawn(
        messages_config(server.base_url()),
        RetryPolicy::default(),
        event_tx,
    );

    handle.submit(RequestId::from("req-bad-event"), user_request("hi"));
    let events = drain_until_terminal(&mut event_rx, Duration::from_secs(10)).await;
    server.shutdown();

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SamplingEvent::Retrying { .. })),
        "serde failures must not be retried"
    );
    match events.last().unwrap() {
        SamplingEvent::Failed { error, .. } => {
            assert_eq!(error.kind, SamplingErrorKind::Serialization);
            assert!(!error.is_retryable, "surfaced info must be non-retryable");
        }
        other => panic!("expected Failed(Serialization), got {other:?}"),
    }
    assert_eq!(counter.load(Ordering::SeqCst), 1, "exactly one attempt");
}

// ---------------------------------------------------------------------------
// UpdateConfig invalidates cache + applies to subsequent requests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_config_changes_subsequent_request_model() {
    use std::sync::Mutex;

    let captured_models: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_handler = Arc::clone(&captured_models);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let captured = Arc::clone(&captured_handler);
            async move {
                let model = body
                    .get("model")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_string();
                captured.lock().unwrap().push(model);
                let events = sse::chat_completion_events("ok", "test-model");
                Sse::new(stream::iter(
                    events.into_iter().map(Ok::<_, std::convert::Infallible>),
                ))
            }
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let cfg = test_config(server.base_url(), "model-A");
    let handle = SamplerActor::spawn(cfg, RetryPolicy::default(), event_tx);

    let _ = handle
        .submit_and_collect(RequestId::from("req-1"), user_request("hi"))
        .await
        .expect("first req ok");

    let mut new_cfg = test_config(server.base_url(), "model-B");
    new_cfg.api_key = Some("test-key".into());
    handle.update_config(new_cfg);

    let _ = handle
        .submit_and_collect(RequestId::from("req-2"), user_request("hi"))
        .await
        .expect("second req ok");

    server.shutdown();

    let models = captured_models.lock().unwrap();
    assert_eq!(
        models.as_slice(),
        &["model-A".to_string(), "model-B".to_string()]
    );
}

// ---------------------------------------------------------------------------
// Responses doom-loop check signals
// ---------------------------------------------------------------------------

fn responses_config(base_url: String, doom_loop: Option<DoomLoopRecoveryPolicy>) -> SamplerConfig {
    let mut cfg = test_config(base_url, "test-model");
    cfg.api_backend = ApiBackend::Responses;
    cfg.doom_loop_recovery = doom_loop;
    cfg
}

/// Named keepalive, data-only keepalive, and Responses metadata are transport
/// liveness frames. A valid turn containing all three must preserve its real
/// reasoning and assistant output, complete successfully, and avoid retries.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn responses_auxiliary_sse_frames_preserve_reasoning_and_text_without_retry() {
    let counter = Arc::new(AtomicU32::new(0));
    let counter_handler = Arc::clone(&counter);
    let app = Router::new().route(
        "/v1/responses",
        post(move || {
            let counter = Arc::clone(&counter_handler);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let mut events = sse::responses_api_reasoning_and_text_events(
                    "reasoning survives",
                    "visible answer survives",
                    "test-model",
                );
                events.insert(
                    1,
                    SseEvent::with_event("keepalive", json!({"type": "keepalive"}).to_string()),
                );
                events.insert(3, SseEvent::data(json!({"type": "keepalive"}).to_string()));
                events.insert(
                    5,
                    SseEvent::with_event(
                        "response.metadata",
                        json!({
                            "type": "response.metadata",
                            "response_id": "resp_auxiliary_test"
                        })
                        .to_string(),
                    ),
                );
                Sse::new(stream::iter(
                    sse_events_to_axum(events)
                        .into_iter()
                        .map(Ok::<_, std::convert::Infallible>),
                ))
            }
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let mut config = responses_config(server.base_url(), None);
    config.max_retries = Some(0);
    let handle = SamplerActor::spawn(config, RetryPolicy::default(), event_tx);

    let result = handle
        .submit_and_collect(
            RequestId::from("req-responses-auxiliary"),
            user_request("hi"),
        )
        .await;
    server.shutdown();

    let (response, _metrics) = result.expect("auxiliary SSE frames must not fail a valid turn");
    assert_eq!(
        response.reasoning_items().count(),
        1,
        "reasoning survives auxiliary frames"
    );
    assert_eq!(response.assistant_text(), "visible answer survives");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "exactly one HTTP request"
    );
}

/// Server-reported doom-loop triggers flow through the actor rung onto the completed response, without retries.
/// The trigger is non-confident (`@response` channel), so the recovery, which resamples only confident signals, leaves it alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn responses_doom_loop_signals_reach_completed_response() {
    let counter = Arc::new(AtomicU32::new(0));
    let counter_handler = Arc::clone(&counter);
    let app = Router::new().route(
        "/v1/responses",
        post(move || {
            let counter = Arc::clone(&counter_handler);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let events = sse_events_to_axum(sse::responses_api_doom_loop_terminal_only_events(
                    &["tail_repetition:4@response"],
                    "some thought",
                    "an answer",
                    "test-model",
                ));
                Sse::new(stream::iter(
                    events.into_iter().map(Ok::<_, std::convert::Infallible>),
                ))
            }
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let handle = SamplerActor::spawn(
        responses_config(server.base_url(), Some(DoomLoopRecoveryPolicy::default())),
        RetryPolicy::default(),
        event_tx,
    );

    let result = handle
        .submit_and_collect(RequestId::from("req-doom-signal"), user_request("hi"))
        .await;
    server.shutdown();

    let (response, _metrics) = result.expect("a signalled turn still completes");
    assert_eq!(counter.load(Ordering::SeqCst), 1, "warn-only: no resample");
    assert_eq!(response.doom_loop_signals.len(), 1);
    assert_eq!(
        response.doom_loop_signals[0].raw,
        "tail_repetition:4@response"
    );
    assert_eq!(response.assistant_text(), "an answer");
}

/// A gateway that omits `sequence_number` (LiteLLM / Vertex passthrough) still drives a full turn.
/// `async_openai` declares the field as required on every Responses event struct, so without the
/// sanitize-path default the first event fails to deserialize and the whole turn dies with
/// `serialization error: missing field 'sequence_number'`. This exercises the real SSE path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn responses_stream_without_sequence_numbers_completes_turn() {
    let app = Router::new().route(
        "/v1/responses",
        post(move || async move {
            let events = strip_sequence_numbers(sse::responses_api_reasoning_and_text_events(
                "a thought",
                "an answer",
                "test-model",
            ));
            let events = sse_events_to_axum(events);
            Sse::new(stream::iter(
                events.into_iter().map(Ok::<_, std::convert::Infallible>),
            ))
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let handle = SamplerActor::spawn(
        responses_config(server.base_url(), None),
        RetryPolicy::default(),
        event_tx,
    );

    let result = handle
        .submit_and_collect(RequestId::from("req-no-seq"), user_request("hi"))
        .await;
    server.shutdown();

    let (response, _metrics) = result.expect("turn completes without sequence_number");
    assert_eq!(response.assistant_text(), "an answer");
}

/// A gateway whose responses-compat synthesis omits `summary_index` on reasoning-summary frames
/// (LiteLLM) still drives a full turn. `async_openai` declares the field as required on exactly the
/// four summary event structs, so without the sanitize-path default the first summary frame fails
/// to deserialize and the whole turn dies with
/// `serialization error: missing field 'summary_index'`. This exercises the real SSE path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn responses_stream_without_summary_index_completes_turn() {
    let app = Router::new().route(
        "/v1/responses",
        post(move || async move {
            let events = strip_summary_index(sse::responses_api_reasoning_and_text_events(
                "a thought",
                "an answer",
                "test-model",
            ));
            let events = sse_events_to_axum(events);
            Sse::new(stream::iter(
                events.into_iter().map(Ok::<_, std::convert::Infallible>),
            ))
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let handle = SamplerActor::spawn(
        responses_config(server.base_url(), None),
        RetryPolicy::default(),
        event_tx,
    );

    let result = handle
        .submit_and_collect(RequestId::from("req-no-summindex"), user_request("hi"))
        .await;
    server.shutdown();

    let (response, _metrics) = result.expect("turn completes without summary_index");
    assert_eq!(response.assistant_text(), "an answer");
}

/// The full live LiteLLM shape end-to-end: reasoning-summary frames missing BOTH
/// `sequence_number` and `summary_index` (the verbatim AT-AZ-VXG-r2 hole). Without the
/// dialect-layer defaults the turn dies on the first summary frame.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn responses_stream_gateway_shape_missing_seq_and_summary_completes_turn() {
    let app = Router::new().route(
        "/v1/responses",
        post(move || async move {
            let events = strip_sequence_numbers(strip_summary_index(
                sse::responses_api_reasoning_and_text_events(
                    "a thought",
                    "an answer",
                    "test-model",
                ),
            ));
            let events = sse_events_to_axum(events);
            Sse::new(stream::iter(
                events.into_iter().map(Ok::<_, std::convert::Infallible>),
            ))
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let handle = SamplerActor::spawn(
        responses_config(server.base_url(), None),
        RetryPolicy::default(),
        event_tx,
    );

    let result = handle
        .submit_and_collect(RequestId::from("req-no-seq-si"), user_request("hi"))
        .await;
    server.shutdown();

    let (response, _metrics) =
        result.expect("turn completes without sequence_number and summary_index");
    assert_eq!(response.assistant_text(), "an answer");
}

// ---------------------------------------------------------------------------
// Responses gateway capture — SUMMINDEX-2 kill frame (AT-AZ-VXG-r2)
// ---------------------------------------------------------------------------

/// Verbatim `data:` payloads from the AT-AZ-VXG-r2 resp-003 capture
/// (`smoke/redteam/report-verify-87/at-az-vxg-r2/at-az-vxg/wire/resp-003.jsonl`),
/// byte-faithful with ids as-captured — extracted programmatically from the
/// capture, never hand-typed. Minimal faithful subset of the 12-frame stream:
/// frame_index 1 — response.in_progress.
/// frame_index 2 — response.output_item.added (reasoning).
/// frame_index 3 — response.reasoning_summary_text.delta (NO sequence_number, NO summary_index — the .87 hole).
/// frame_index 5 — response.reasoning_summary_part.done.
/// frame_index 6 — response.output_item.done (reasoning).
/// frame_index 7 — response.output_text.delta.
/// frame_index 8 — response.output_text.done.
/// frame_index 9 — response.content_part.done, part {type: reasoning_text, reasoning: ...} — THE KILL FRAME (no text, no seq).
/// frame_index 10 — response.output_item.done (message, assistant text).
/// frame_index 11 — response.completed (both output items).
const R2_RESP_003_KILL_STREAM: &[&str] = &[
    r##"{"type":"response.in_progress","response":{"id":"resp_bGl0ZWxsbTpjdXN0b21fbGxtX3Byb3ZpZGVyOnZlcnRleF9haTttb2RlbF9pZDo2MjE2ZDUyNzQ1Mzc2ZmQ1MGNlMGI5ZGU4MTY5MjA2ZDYwY2YwNmI3N2I4OWZhN2IyNmM5MmZjZjZmNWRjYTZhO3Jlc3BvbnNlX2lkOnEwR3VhcERVTGY2WW90UVA4Ym11a1E4","created_at":1789804973,"model":"gemini-3.5-flash","object":"response","output":[],"parallel_tool_calls":true,"tool_choice":"auto","tools":[{"name":"run_terminal_command","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["command","description"],"properties":{"command":{"description":"The bash command to run.","type":"string"},"timeout":{"description":"Optional timeout in milliseconds (max 36000000). Default: 120000. Kill deadline for a command that is still in the foreground. This does not extend how long the tool waits: a foreground command still running after about 15s is moved to the background and you receive a task id. Once backgrounded, the command is no longer bound by this value; it runs until it exits (background cap 10h). If you do not receive a task id, the command was killed at timeout instead.","format":"uint64","minimum":0,"default":120000,"maximum":36000000,"anyOf":[{"type":"integer","description":"Optional timeout in milliseconds (max 36000000). Default: 120000. Kill deadline for a command that is still in the foreground. This does not extend how long the tool waits: a foreground command still running after about 15s is moved to the background and you receive a task id. Once backgrounded, the command is no longer bound by this value; it runs until it exits (background cap 10h). If you do not receive a task id, the command was killed at timeout instead."},{"type":"null","description":"Optional timeout in milliseconds (max 36000000). Default: 120000. Kill deadline for a command that is still in the foreground. This does not extend how long the tool waits: a foreground command still running after about 15s is moved to the background and you receive a task id. Once backgrounded, the command is no longer bound by this value; it runs until it exits (background cap 10h). If you do not receive a task id, the command was killed at timeout instead."}]},"description":{"description":"One sentence explanation as to why this command needs to be run and how it contributes to the goal.","type":"string"},"background":{"description":"Set to true for long-running commands that should run in the background (e.g., dev servers, long builds). Returns a task id immediately while the command keeps running in the background; you are notified on completion, so do not poll or sleep-wait for it.","type":"boolean","default":false}},"type":"object"},"type":"function","description":"Run a bash command and return its output.\n\nUsage notes:\n  - You can specify an optional timeout in milliseconds (up to 36000000ms). Foreground commands block this tool for at most about 15s. A command still running at that point is moved to the background — it is not killed and has not timed out — and you receive a task id; wait for it with get_command_or_subagent_output. If you do not receive a task id, the command was killed at timeout instead. timeout is a separate kill deadline that only applies while the command is still in the foreground; once backgrounded the command runs until it exits (background cap 10h). Setting timeout never makes this tool wait longer than about 15s. Commands launched with background: true are not bounded by the default: with timeout omitted or 0 they run until they exit or are killed; a positive timeout still applies.\n  - Timeout enforcement:when the timeout fires on an explicit `background: true` command, the wrapper kills the child process group (SIGTERM, escalated to SIGKILL after a ~1s grace period). Descendants that did not detach via `setsid` / `nohup` will also be killed. `timeout: 0` in `background: true` mode disables the wrapper timeout entirely; the child's lifetime is owned by the model via kill_command_or_subagent.\n  - If the output exceeds 40000 characters, the middle is truncated (you keep the beginning and end) and the result includes the path to a log file with the full output, which you can read or search.\n  - You can use the background parameter to run the command in the background (e.g., dev servers, long builds): it returns a task id immediately and keeps running in the background. You are notified on completion, so do not poll or sleep-wait for it. You do not need to use '&' at the end of the command when using this parameter."},{"name":"read_file","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["target_file"],"type":"object","properties":{"target_file":{"description":"The path of the file to read. You can use either a relative path in the workspace or an absolute path. If an absolute path is provided, it will be preserved as is.","type":"string"},"offset":{"description":"The line number to start reading from. Only provide if the file is too large to read at once.","type":"integer","default":1},"limit":{"description":"The number of lines to read. Only provide if the file is too large to read at once.","type":"integer"},"pages":{"description":"Page range for PDF files (e.g. '1-5', '3', '10-'). Required for PDFs with more than 10 pages. Max 20 pages per call. Ignored for non-PDF files.","anyOf":[{"type":"string","description":"Page range for PDF files (e.g. '1-5', '3', '10-'). Required for PDFs with more than 10 pages. Max 20 pages per call. Ignored for non-PDF files."},{"type":"null","description":"Page range for PDF files (e.g. '1-5', '3', '10-'). Required for PDFs with more than 10 pages. Max 20 pages per call. Ignored for non-PDF files."}]},"format":{"description":"Output format for PDF files. 'image' (default) renders pages as images. 'text' extracts text content. Ignored for non-PDF files.","anyOf":[{"type":"string","description":"Output format for PDF files. 'image' (default) renders pages as images. 'text' extracts text content. Ignored for non-PDF files."},{"type":"null","description":"Output format for PDF files. 'image' (default) renders pages as images. 'text' extracts text content. Ignored for non-PDF files."}]}}},"type":"function","description":"Read a file.\n\nUsage:\n- The target_file parameter can be a relative path in the workspace or an absolute path\n- By default, it reads up to 1000 lines starting from the beginning of the file\n- Line numbers (1-based) appear as anchors in the format LINE_NUMBER→LINE_CONTENT on the first returned line and on every 10th line of the file; the lines in between show content only. Count from the nearest anchor when referring to a specific line\n- This tool can read PDF files (.pdf), PowerPoint files (.pptx), Jupyter notebooks (.ipynb files), and image files (e.g. PNG, JPG, etc).\n- When reading an image file the contents are presented visually as this tool uses multimodal LLMs."},{"name":"search_replace","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["file_path","old_string","new_string"],"properties":{"file_path":{"description":"The path to the file to modify. You can use either a relative path in the workspace or an absolute path.","type":"string"},"old_string":{"description":"The text to replace","type":"string"},"new_string":{"description":"The text to replace it with (must be different from old_string)","type":"string"},"replace_all":{"description":"Replace all occurrences of old_string (default false)","type":"boolean","default":false}},"type":"object"},"type":"function","description":"Replace an exact string in a file.\n\n- `read_file` prefixes each line with \"LINE_NUMBER→\". That prefix is not part of the file: match only what comes after the →, with its exact indentation.\n- `old_string` must match exactly one place in the file. If it appears more than once, add surrounding lines to make it unique, or set `replace_all` to change every occurrence (handy for renaming an identifier).\n- To create a new file, set `old_string` to an empty string."},{"name":"list_dir","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["target_directory"],"type":"object","properties":{"target_directory":{"description":"Path to directory to list contents of, relative to the workspace root or absolute.","type":"string"}}},"type":"function","description":"Lists files and directories in a given path.\nThe 'target_directory' parameter can be relative to the workspace root or absolute.\n\nOther details:\n    - The result does not display dot-files and dot-directories.\n    - Respects .gitignore patterns (files/directories ignored by git are not shown).\n    - Large directories are summarized with file counts and extension breakdowns instead of listing all files."},{"name":"grep","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["pattern"],"type":"object","properties":{"pattern":{"description":"The regular expression pattern to search for in file contents (rg --regexp)","type":"string"},"path":{"description":"File or directory to search in (rg pattern -- PATH). Defaults to workspace path.","anyOf":[{"type":"string","description":"File or directory to search in (rg pattern -- PATH). Defaults to workspace path."},{"type":"null","description":"File or directory to search in (rg pattern -- PATH). Defaults to workspace path."}]},"glob":{"description":"Glob pattern (rg --glob GLOB -- PATH) to filter files (e.g. \"*.js\", \"*.{ts,tsx}\").","anyOf":[{"type":"string","description":"Glob pattern (rg --glob GLOB -- PATH) to filter files (e.g. \"*.js\", \"*.{ts,tsx}\")."},{"type":"null","description":"Glob pattern (rg --glob GLOB -- PATH) to filter files (e.g. \"*.js\", \"*.{ts,tsx}\")."}]},"-B":{"description":"Number of lines to show before each match (rg -B).","type":"integer"},"-A":{"description":"Number of lines to show after each match (rg -A).","type":"integer"},"-C":{"description":"Number of lines to show before and after each match (rg -C).","type":"integer"},"-i":{"description":"Case insensitive search (rg -i).","type":"boolean","default":false},"type":{"description":"File type to search (rg --type). Common types: js, py, rust, go, java, etc. More efficient than glob for standard file types.","anyOf":[{"type":"string","description":"File type to search (rg --type). Common types: js, py, rust, go, java, etc. More efficient than glob for standard file types."},{"type":"null","description":"File type to search (rg --type). Common types: js, py, rust, go, java, etc. More efficient than glob for standard file types."}]},"head_limit":{"description":"Limit output to first N lines/entries, equivalent to \"| head -N\". Defaults to 200 lines or 500 entries.","type":"integer"},"multiline":{"description":"Enable multiline mode where . matches newlines and patterns can span lines (rg -U --multiline-dotall).","type":"boolean","default":false}}},"type":"function","description":"Search file contents with regular expressions (ripgrep).\n\n- Full regex syntax, so escape literal special characters: `functionCall\\(`, or `interface\\{\\}` to find interface{} in Go.\n- Pass pattern as a raw regex string — no surrounding quotes.\n- Respects .gitignore unless you pass a broad glob like '--glob *'.\n- Only filter by 'type' or 'glob' when you are sure of the file type; import paths may not match source file types (.js vs .ts).\n- Output is ripgrep-style: ':' marks match lines, '-' marks context lines, grouped by file. Large results are capped and report \"at least\" counts."},{"name":"kill_command_or_subagent","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["task_id"],"properties":{"task_id":{"description":"The task ID to terminate","type":"string"}},"type":"object"},"type":"function","description":"Terminate a running background task, monitor, or subagent.\n\nUsage notes:\n- Pass its task_id (a monitor's task_id is returned by monitor).\n- Sends SIGTERM/SIGKILL to a bash task or monitor; sends Cancel+Shutdown to a subagent.\n- Returns success if the task was killed or had already exited."},{"name":"todo_write","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["todos"],"type":"object","properties":{"merge":{"description":"Optional. When true (default), merges the provided todos into the existing list by id — send only the items you are changing, and to flip status without changing content send just id + status. When false, the provided todos replace the existing list.","type":"boolean","default":true},"todos":{"description":"Array of todo items to write to the workspace","type":"array","items":{"type":"object","properties":{"id":{"description":"Unique identifier for the todo item","type":"string"},"content":{"description":"The description/content of the todo item","anyOf":[{"type":"string","description":"The description/content of the todo item"},{"type":"null","description":"The description/content of the todo item"}]},"status":{"description":"The status of the todo item: pending, in_progress, completed, or cancelled","enum":["pending","in_progress","completed","cancelled",null],"anyOf":[{"type":"string","description":"The status of the todo item: pending, in_progress, completed, or cancelled"},{"type":"null","description":"The status of the todo item: pending, in_progress, completed, or cancelled"}]}},"required":["id"]}}}},"type":"function","description":"Create and manage a structured task list. The user sees this list live — it is your primary way to show progress.\n\nUse for any task with 3+ steps. Skip for trivial single-step work."},{"name":"get_command_or_subagent_output","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","properties":{"task_ids":{"description":"Task IDs to get output from. Pass one or more; for a single task use a one-element array. With a positive timeout_ms, multiple ids wait until all complete. Omit timeout_ms or pass 0 for a non-blocking snapshot.","type":"array","items":{"type":"string"},"default":[]},"timeout_ms":{"description":"Max wait time in milliseconds, up to 3600000 (~1 h). A positive value waits for completion; omit or pass 0 for a non-blocking status poll.","format":"uint64","minimum":0,"default":null,"maximum":3600000,"anyOf":[{"type":"integer","description":"Max wait time in milliseconds, up to 3600000 (~1 h). A positive value waits for completion; omit or pass 0 for a non-blocking status poll."},{"type":"null","description":"Max wait time in milliseconds, up to 3600000 (~1 h). A positive value waits for completion; omit or pass 0 for a non-blocking status poll."}]}},"type":"object","required":[]},"type":"function","description":"Get output and status from a background task, monitor, or subagent.\n\nUsage notes:\n- Pass task_ids with one or more ids from background=true commands or subagents (a monitor's task_id is returned by monitor); for a single task use a one-element array. Multiple ids with a positive timeout_ms wait until all complete\n- Omit timeout_ms or pass 0 for a non-blocking status snapshot; set a positive timeout_ms to wait up to that many milliseconds, capped at 3600000 (~1 h)\n- Returns current output, status, and exit code if completed\n- If output is large, use read_file on the output_file path"},{"name":"spawn_subagent","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["prompt","description"],"properties":{"prompt":{"description":"The full task prompt for the subagent to execute.","type":"string"},"description":{"description":"Short description of the task (3-5 words).","type":"string"},"subagent_type":{"description":"Name of the subagent type to launch. Built-in types: \"general-purpose\", \"explore\", \"plan\". Additional user-defined types may also be available.","type":"string","default":"general-purpose"},"background":{"description":"Returns immediately with a subagent_id. Use the task output tool to retrieve results. This is set to true by default.","type":"boolean","default":true},"isolation":{"description":"Isolation mode: \"none\" (default, shared workspace) or \"worktree\" (isolated git worktree). Worktree mode prevents the child's edits from affecting the parent workspace until explicitly merged.","enum":["none","worktree",null],"anyOf":[{"type":"string","description":"Isolation mode: \"none\" (default, shared workspace) or \"worktree\" (isolated git worktree). Worktree mode prevents the child's edits from affecting the parent workspace until explicitly merged."},{"type":"null","description":"Isolation mode: \"none\" (default, shared workspace) or \"worktree\" (isolated git worktree). Worktree mode prevents the child's edits from affecting the parent workspace until explicitly merged."}]},"resume_from":{"description":"Resume from a previously completed subagent's conversation. Pass the subagent_id returned by a prior task call. The new subagent continues the previous one's raw transcript with the new task prompt appended. The source must be completed (not running), belong to the current session, and use the same subagent_type.","anyOf":[{"type":"string","description":"Resume from a previously completed subagent's conversation. Pass the subagent_id returned by a prior task call. The new subagent continues the previous one's raw transcript with the new task prompt appended. The source must be completed (not running), belong to the current session, and use the same subagent_type."},{"type":"null","description":"Resume from a previously completed subagent's conversation. Pass the subagent_id returned by a prior task call. The new subagent continues the previous one's raw transcript with the new task prompt appended. The source must be completed (not running), belong to the current session, and use the same subagent_type."}]},"cwd":{"description":"Explicit working directory for the subagent. The path must exist and be a directory. Mutually exclusive with isolation=\"worktree\". Ignored when resume_from is set (the resumed child inherits its source's cwd/worktree).","anyOf":[{"type":"string","description":"Explicit working directory for the subagent. The path must exist and be a directory. Mutually exclusive with isolation=\"worktree\". Ignored when resume_from is set (the resumed child inherits its source's cwd/worktree)."},{"type":"null","description":"Explicit working directory for the subagent. The path must exist and be a directory. Mutually exclusive with isolation=\"worktree\". Ignored when resume_from is set (the resumed child inherits its source's cwd/worktree)."}]},"model":{"description":"Optional model slug for this agent. If provided, it must resolve to one of the available model slugs. If omitted, the subagent uses the same model as the parent agent. Do not pass if resume_from is set (prior model will be used). Only choose an explicit model when the user directly requests it.","anyOf":[{"type":"string","description":"Optional model slug for this agent. If provided, it must resolve to one of the available model slugs. If omitted, the subagent uses the same model as the parent agent. Do not pass if resume_from is set (prior model will be used). Only choose an explicit model when the user directly requests it."},{"type":"null","description":"Optional model slug for this agent. If provided, it must resolve to one of the available model slugs. If omitted, the subagent uses the same model as the parent agent. Do not pass if resume_from is set (prior model will be used). Only choose an explicit model when the user directly requests it."}]},"effort":{"description":"Optional reasoning effort for this agent (e.g. \"low\", \"medium\", \"high\"). Must be a value the agent's model supports; it is rejected for models without a reasoning-effort menu (e.g. claude). If omitted, the agent inherits the parent session's reasoning effort.","anyOf":[{"type":"string","description":"Optional reasoning effort for this agent (e.g. \"low\", \"medium\", \"high\"). Must be a value the agent's model supports; it is rejected for models without a reasoning-effort menu (e.g. claude). If omitted, the agent inherits the parent session's reasoning effort."},{"type":"null","description":"Optional reasoning effort for this agent (e.g. \"low\", \"medium\", \"high\"). Must be a value the agent's model supports; it is rejected for models without a reasoning-effort menu (e.g. claude). If omitted, the agent inherits the parent session's reasoning effort."}]}},"type":"object"},"type":"function","description":"Start a subagent that works on a task independently and reports back.\n\nAgent types:\n\n- **general-purpose**: General purpose agent for multi-step tasks. Has access to: run_terminal_command, read_file, search_replace, list_dir, grep, , and todo_write.\n- **explore**: Fast, read-only agent specialized for codebase exploration. Read-only — has access to: read_file, list_dir, grep.\n- **plan**: Software architect for planning implementation strategies. Read-only — has access to: read_file, list_dir, grep, , and todo_write. File editing and command execution are not available.\n- **codebase-memory-auditor**: Bounded-scope graph audit with check_index_coverage and source read/grep fallback.\n- **codebase-memory**: Default task-directed graph verification with check_index_coverage and source read/grep fallback.\n- **codebase-memory-scout**: Fast positive, provisional graph lookup with check_index_coverage and source read/grep fallback.\n- **codex-sol**: GPT-5.6 Sol on the /responses wire (Codex dialect, encrypted reasoning via East US 2).\n- **grok**: Grok-4.6 on the /responses wire (xAI native dialect).\n\n## Usage notes\n- When the agent is done, it returns a single message with its agent ID. Use that ID to resume the agent later for follow-up work.\n- background: Returns immediately with a subagent_id. Use get_command_or_subagent_output to retrieve results. This is set to true by default.\n- Subagents receive a compacted version of project instructions (AGENTS.md). If the task requires detailed conventions (e.g., build rules, testing patterns), include the relevant rules directly in the prompt.\n- When using the spawn_subagent tool, you must specify a subagent_type parameter to select which agent type to use.\n- When launching independent subagents, you MUST incorporate the results into the task based on requirements BEFORE concluding.\n\nResuming a previous agent (resume_from):\n- Use resume_from to continue a previously completed subagent's conversation. Pass the subagent_id returned by a prior spawn_subagent call. A resumed agent keeps its full transcript and tool state, so you only need to describe what changed since the last run — don't re-explain the original task.\n- The resumed agent must use the same subagent_type as the source.\n\nIsolation mode:\n- Use isolation to control the child's execution environment. With \"worktree\", the child runs in an isolated git worktree whose edits don't affect the parent workspace; the worktree is preserved after completion and its path is returned in the output.\n\nIf the user explicitly asks for the model of a subagent/task, you may ONLY use model slugs from this list:\n- Qwen3-Embedding-8B\n- answerai-colbert-small-v1\n- claude-haiku-4-5\n- claude-haiku-4-5-20251001\n- claude-haiku-4.5\n- claude-opus-5\n- claude-sonnet-5\n- gemini-3-flash-preview\n- gemini-3-pro-preview\n- gemini-3.1-flash-lite\n- gemini-3.1-flash-lite-preview\n- gemini-3.1-pro-preview\n- gemini-3.5-flash\n- gemini-3.5-flash-lite\n- gemini-3.6-flash\n- gemini-3.7-flash\n- gemini-3.8-flash\n- gemma-4-31b\n- glm-5.2\n- gpt-5.6-luna\n- gpt-5.6-sol\n- gpt-5.6-terra\n- grok-4.6\n- o1\n- o1-mini\n- o1-preview\n- o3\n- o3-mini\n- o4-mini\n- qwen3.8-27b\n- text-embedding-3-large\n- text-embedding-3-small\n- text-embedding-ada-002\n\nIf the user does not explicitly request a model, omit `model` to inherit the parent model."},{"name":"scheduler_create","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","properties":{"task_id":{"description":"Id of an existing task to update in place: provided fields replace old values, omitted ones are unchanged, the schedule keeps its phase, and an unknown id errors. Omit to create a task.","default":null,"anyOf":[{"type":"string","description":"Id of an existing task to update in place: provided fields replace old values, omitted ones are unchanged, the schedule keeps its phase, and an unknown id errors. Omit to create a task."},{"type":"null","description":"Id of an existing task to update in place: provided fields replace old values, omitted ones are unchanged, the schedule keeps its phase, and an unknown id errors. Omit to create a task."}]},"interval":{"description":"Interval between executions, e.g. \"5m\", \"2h\", \"1d\". Required to create; optional with task_id","default":null,"anyOf":[{"type":"string","description":"Interval between executions, e.g. \"5m\", \"2h\", \"1d\". Required to create; optional with task_id"},{"type":"null","description":"Interval between executions, e.g. \"5m\", \"2h\", \"1d\". Required to create; optional with task_id"}]},"prompt":{"description":"The prompt text to execute on each scheduled fire. Required to create; optional with task_id","default":null,"anyOf":[{"type":"string","description":"The prompt text to execute on each scheduled fire. Required to create; optional with task_id"},{"type":"null","description":"The prompt text to execute on each scheduled fire. Required to create; optional with task_id"}]},"durable":{"description":"Whether the task persists across sessions. Default: false. Create-only: ignored with task_id","default":null,"anyOf":[{"type":"boolean","description":"Whether the task persists across sessions. Default: false. Create-only: ignored with task_id"},{"type":"null","description":"Whether the task persists across sessions. Default: false. Create-only: ignored with task_id"}]},"fire_immediately":{"description":"Whether to fire immediately on creation (true) or wait for the first interval (false). Default: false. Create-only: ignored with task_id","type":"boolean","default":false}},"type":"object","required":[]},"type":"function","description":"Create a scheduled task that runs a prompt on a recurring interval, or update an existing one in place.\n\nUse this tool when a user asks you to loop, repeat, or schedule a prompt or a task.\n\nSet fire_immediately: true to also fire once on creation; by default the first run waits for the interval.\n\nTo change an existing task, pass its task_id: provided fields replace old values, omitted ones are unchanged, and the schedule keeps its phase. An unknown id errors.\n\nUsage notes:\n- Interval format: \"5m\" (minutes), \"2h\" (hours), \"1d\" (days), \"60s\" (seconds, min 60)\n- Maximum 50 scheduled tasks at once\n- Tasks auto-expire after 7 days\n- For one-time delayed work, run a background terminal command (e.g. `sleep 1800 && <command>`) instead; its completion notifies you"},{"name":"scheduler_delete","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["id"],"type":"object","properties":{"id":{"description":"The task ID to cancel (from scheduler_create output)","type":"string"}}},"type":"function","description":"Cancel a scheduled task by ID.\n\nReturns success: true if the task was found and removed, false if no task with that ID exists."},{"name":"scheduler_list","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","type":"object","properties":{},"required":[]},"type":"function","description":"List all active scheduled tasks with their IDs, prompts, intervals, and next fire times."},{"name":"monitor","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["command","description"],"type":"object","properties":{"command":{"description":"Shell command or script. Each stdout line is an event; exit ends the watch.","type":"string"},"description":{"description":"Short human-readable description of what you are monitoring (shown in every notification).","type":"string"},"timeout_ms":{"description":"Kill the monitor after this deadline (ms). Default: 36000000 (10 hr). Max: 36000000 (10 hr).","format":"uint64","minimum":0,"default":36000000,"anyOf":[{"type":"integer","description":"Kill the monitor after this deadline (ms). Default: 36000000 (10 hr). Max: 36000000 (10 hr)."},{"type":"null","description":"Kill the monitor after this deadline (ms). Default: 36000000 (10 hr). Max: 36000000 (10 hr)."}]},"persistent":{"description":"Run for the lifetime of the session (no timeout). Stop with kill_command_or_subagent.","type":"boolean","default":false}}},"type":"function","description":"Start a background monitor that streams events from a long-running script. Each stdout line is an event - you can keep working and notifications arrive in the chat. Exit ends the watch.\n\n**Output volume**: Every stdout line is a main-agent wake. Print only `DONE`/`FAILED`/`CANCELLED`. No progress or CHANGE lines. Use `grep --line-buffered` in pipes (plain `grep` buffers and delays events by minutes).\n\n**Responsiveness**: Emit `FAILED` to notify immediately when any required item fails; never wait for unrelated work to finish. Include every tracked failure signal in this immediate failure condition.\n\nSet `persistent: true` for session-length watches (PR monitoring, log tails) -- the monitor runs until you call kill_command_or_subagent or until the session ends. Otherwise it stops at `timeout_ms` (default 10h)."},{"name":"search_tool","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["query"],"properties":{"query":{"description":"Keywords to match against tool names, server names, and descriptions.\nInclude the server name and action for best results\n(e.g. \"linear create issue\", \"slack read thread history\").","type":"string"},"limit":{"description":"Maximum number of results to return (default 5).","format":"uint8","minimum":0,"maximum":255,"default":5,"anyOf":[{"type":"integer","description":"Maximum number of results to return (default 5)."},{"type":"null","description":"Maximum number of results to return (default 5)."}]}},"type":"object"},"type":"function","description":"Search for MCP tools by keyword and retrieve their input schemas.\n\nIf status is \"partial\", some servers may still be connecting."},{"name":"use_tool","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["tool_name","tool_input"],"properties":{"tool_name":{"description":"The qualified name of the integration tool to call (e.g., \"linear__save_issue\").\nMust be a tool previously discovered via `search_tool`.","type":"string"},"tool_input":{"description":"The arguments to pass to the tool, as a JSON object.\nUse the parameter schema returned by `search_tool` to construct this.","type":"object","additionalProperties":true}},"type":"object"},"type":"function","description":"Call an MCP integration tool.\n\nThe `tool_name` must be the qualified `server__tool` name (e.g., `linear__save_issue`). The `tool_input` must conform exactly to the tool's input schema as returned by `search_tool`."},{"name":"workflow","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["source"],"type":"object","properties":{"source":{"description":"Exactly one workflow source. The `type` tag selects a registered name, inline script, script path, same-process resume, or a pause/stop of a run this session launched.","oneOf":[{"type":"object","properties":{"name":{"description":"Name of a registered workflow (built-in, or discovered from the project `.grok/workflows/` or user `~/.grok/workflows/`).","type":"string"},"type":{"type":"string","const":"name"}},"required":["type","name"]},{"type":"object","properties":{"script":{"description":"Inline Rhai workflow script. It must start with a pure-literal `let meta = #{ name: ..., description: ... };` map. Before authoring, read the `create-workflow` skill's SKILL.md. Run the path-specific `validate_only` smoke check with representative args.","type":"string"},"type":{"type":"string","const":"script"}},"required":["type","script"]},{"type":"object","properties":{"script_path":{"description":"Path to a .rhai workflow script on disk.","type":"string"},"type":{"type":"string","const":"script_path"}},"required":["type","script_path"]},{"type":"object","properties":{"resume_from_run_id":{"description":"Resume a same-process paused run, continuing its original immutable source and args. A budget-limited run resumes only when `agent_budget` is passed with a higher cap. Process-restart interruptions are terminal.","type":"string"},"type":{"type":"string","const":"resume"}},"required":["type","resume_from_run_id"]},{"type":"object","properties":{"run_id":{"description":"Pause an active run this session launched, by its `run_id` or display name. Its child agents are cancelled and the run is marked paused; continue it with the `resume` source.","type":"string"},"type":{"type":"string","const":"pause"}},"required":["type","run_id"]},{"type":"object","properties":{"run_id":{"description":"Stop a run this session launched, by its `run_id` or display name. Its child agents are cancelled and the run is marked cancelled (finished). It keeps its journal, so `resume` can still continue it later.","type":"string"},"type":{"type":"string","const":"stop"}},"required":["type","run_id"]}]},"agent_budget":{"description":"Absolute cumulative cap on logical child-agent calls for this run. Every agent() and every parallel() item consumes one slot; schema retries do not. Defaults to 128 and may be set from 1 through 1,024. A panel that would exceed the remaining budget is rejected before any of its children launch.","format":"uint64","minimum":1,"maximum":1024,"default":null,"anyOf":[{"type":"integer","description":"Absolute cumulative cap on logical child-agent calls for this run. Every agent() and every parallel() item consumes one slot; schema retries do not. Defaults to 128 and may be set from 1 through 1,024. A panel that would exceed the remaining budget is rejected before any of its children launch."},{"type":"null","description":"Absolute cumulative cap on logical child-agent calls for this run. Every agent() and every parallel() item consumes one slot; schema retries do not. Defaults to 128 and may be set from 1 through 1,024. A panel that would exceed the remaining budget is rejected before any of its children launch."}]},"args":{"description":"JSON value bound to the script's `args` global. Use an object for named arguments.","default":null,"type":"object"},"validate_only":{"description":"Run a path-specific smoke check without launching: validate metadata, compile the full script, and execute the single path selected by the supplied args and canned host results. It does not exercise every branch or prove live tools and agent outputs work.","type":"boolean","default":false}}},"type":"function","description":"Launch or control a workflow: a Rhai script that orchestrates subagents as one background run. Provide exactly one `source`: a registered workflow `name`, an inline `script`, a `script_path`, a same-process `resume`, or a `pause` / `stop` of a run this session launched (by `run_id` or display name). Optionally pass `args` (bound to the script's `args`) and `agent_budget`, an absolute cap on cumulative child-agent calls: every agent() and parallel() item consumes one slot (schema retries do not); default 128. The host also caps live children per run (32 by default, host-configured) — larger parallel() panels are queued and still act as a barrier. The call returns immediately; progress appears in `/workflow runs` and completion is reported automatically — do not poll or sleep-wait.\n\nPrefer a registered workflow when one fits; author a script for bounded fan-out over a known work list, staged research and verification, or several independent perspectives. Before writing or editing a script, read the `create-workflow` skill's SKILL.md. `validate_only: true` runs a path-specific smoke check (metadata, compile, one canned-host path) — not proof that every branch or live tool works.\n\nA started run gets a session-unique display name (e.g. `review-changes`, `review-changes-2`) — the handle to show the user, who manages runs with `/workflow pause|resume|stop <name>`; keep run IDs internal. To stop or pause a run yourself, call this tool with `source: { type: \"stop\", run_id }` or `{ type: \"pause\", run_id }` (run id or display name); both cancel the run's child agents and keep its journal, so either can be continued later with `resume`. Pause only applies to an active run; stop applies to any run that has not finished or hit its agent budget (a budget-limited run is already stopped and needs `resume` with a higher `agent_budget`). Each launch persists an editable `script_path`; edit it and launch as a new run to iterate. Use the `resume` source only for a same-process paused run (process restarts are terminal); it reuses the run's original immutable source and args, and a budget-limited run resumes only with a higher `agent_budget`. Save reusable scripts to `.grok/workflows/<name>.rhai`."},{"name":"enter_plan_mode","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","type":"object","properties":{},"required":[]},"type":"function","description":"Use this tool when a task has ambiguity about the right approach or when the user asks you to write a plan. This tool enables a read-only plan mode where you explore the codebase and create an implementation plan for the user."},{"name":"exit_plan_mode","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","type":"object","properties":{},"required":[]},"type":"function","description":"Exit plan mode and present your plan to the user.\n\nUse this after you have finished writing your plan to the plan file in plan mode."},{"name":"ask_user_question","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["questions"],"properties":{"questions":{"description":"The questions to ask, each with its own options.","type":"array","items":{"description":"A single question with its options.","type":"object","properties":{"question":{"description":"The question to ask, phrased as a full question.","type":"string"},"options":{"description":"The choices for this question.","type":"array","items":{"description":"A single option within a question.","type":"object","properties":{"label":{"description":"Option text shown to the user. A few words at most.","type":"string"},"description":{"description":"What picking this option means or implies.","type":"string"},"preview":{"description":"Optional content shown while the option is focused — mockups, code snippets, anything the user should compare. Single-select questions only.","anyOf":[{"type":"string","description":"Optional content shown while the option is focused — mockups, code snippets, anything the user should compare. Single-select questions only."},{"type":"null","description":"Optional content shown while the option is focused — mockups, code snippets, anything the user should compare. Single-select questions only."}]}},"required":["label","description"]}},"multi_select":{"description":"Let the user pick more than one option (default false).","default":null,"anyOf":[{"type":"boolean","description":"Let the user pick more than one option (default false)."},{"type":"null","description":"Let the user pick more than one option (default false)."}]}},"required":["question","options"]}}},"type":"object"},"type":"function","description":"Ask the user one or more multiple-choice questions.\n\n- Every question automatically gets an \"Other\" choice where the user can type their own answer.\n- Put your recommended option first and append \"(Recommended)\" to its label."},{"name":"send_feedback","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["title","details","type"],"type":"object","properties":{"title":{"description":"Short summary for the draft.","type":"string"},"details":{"description":"Short labeled bullets in this order: What happened, What the user said, Repro, optional Evidence, then optional verified Cause. Keep each to 1–3 lines; do not use narrative paragraphs.","type":"string"},"area":{"description":"Optional product area; omit when unclear.","anyOf":[{"type":"string","description":"Optional product area; omit when unclear."},{"type":"null","description":"Optional product area; omit when unclear."}]},"type":{"description":"Feedback classification.","type":"string","enum":["bug","idea","missing_capability"]},"task_category":{"description":"Optional task category; omit when unclear.","enum":["code_edit","debug","explain","plan","shell","search","review","other",null],"anyOf":[{"type":"string","description":"Optional task category; omit when unclear."},{"type":"null","description":"Optional task category; omit when unclear."}]},"failure_mode":{"description":"Optional model-behavior failure mode; omit for a pure product or tool bug.","enum":["overeager","stopped_early","unwanted_scope","didnt_ask_for_help","excessive_questions","subagent_overspawn","over_correction","ignored_instructions","hallucinated","sloppy_code","destructive","lost_context","stuck_in_a_loop","model_regression","disputed","wrong_tone","unclear_output","other",null],"anyOf":[{"type":"string","description":"Optional model-behavior failure mode; omit for a pure product or tool bug."},{"type":"null","description":"Optional model-behavior failure mode; omit for a pure product or tool bug."}]},"draft_id":{"description":"Existing local draft to update. When set, this call does not append a second draft.","anyOf":[{"type":"string","description":"Existing local draft to update. When set, this call does not append a second draft."},{"type":"null","description":"Existing local draft to update. When set, this call does not append a second draft."}]}}},"type":"function","description":"# Overview\n\nSave or update user feedback for later review. Feedback is stored as local drafts and is never sent without explicit approval through the `/feedback` modal. This tool opens no UI and does not stop the current turn.\n\n# Invocation\n\nWhen the user types `/feedback` bare into the prompt bar, the modal opens with the Write and Drafts tabs. The Write tab is only for the user to hand-write feedback.\nIf the user types `/feedback` with text inline, the system inserts it like a skill. Only when feedback is requested that way, or the user explicitly wants you to update an existing feedback draft, may you use the draft_id field.\nWhen draft_id is set, update that existing draft. Do not duplicate drafts. draft_id is only a tool argument. Never write it into title, details, or area.\n\nWhen the user wants to share feedback implicitly, draft it with this tool, whether it is a product or model-behavior issue.\n\n# Usage\n\nWrite details as short labeled bullets in this order: What happened, What the user said, Repro, optional Evidence, then optional verified Cause.\nSet failure_mode only for model-behavior feedback; omit it for a pure product or tool bug.\nIf mapping feedback is incredibly unclear, only then may you use ask_user_question to confirm ambiguity with the user. Use this sparingly.\n\n# Confirmation\n\nAfter drafting feedback and ending your turn, tell the user they can verify and send it to the team by typing `/feedback` to open the modal and going to the Drafts section.\n\n# Misc\nThis session's drafts file is smoke/redteam/report-verify-87/at-az-vxg-r2/at-az-vxg/home/sessions/%2FUsers%2Fpalanisd%2FProjects%2Fupstream%2Fwt%2Fgrok-build-responses/437d5dde-9df8-47a3-9dfc-013c94080e48/feedback_drafts.json.\nIf the user's feedback can be answered from the docs (for example UI element locations or setup), read the Grok Build docs locally or online and answer alongside the created draft.\n\nDoing the wrong amount of work\n- Overeager: Did more than asked, acted before being told, jumped in without enough info\n- Stopping early: Quit early, handed back work that could have been finished\n- Unwanted scope: Not stopping\n- Didn't ask for help: Didn't ask the user for help when stuck\n- Excessive questions: Asked clarifying questions when there was enough to proceed\n- Subagent overspawn: Launched more subagents than the task warranted\n- Over correction: Fixed feedback by swinging too far the other way\n\nWrong outputs\n- Instruction following: Ignored or missed explicit instructions or constraints\n- Overconfidence and hallucination: Stated something confidently that was wrong or fabricated\n- Code quality: Buggy, sloppy, or poorly structured code\n- Destructive actions: Did or risked something hard to reverse\n- Context and memory: Lost earlier context, forgot established facts, contradicted itself\n- Repetition and looping: Repeated output or retried the same failing action\n- Model regression: Behavior noticeably worse than a previous model version\n\nStyle\n- Dispute or decline: Refused or argued against a reasonable request\n- Tone or preachiness: Wrong tone — moralizing, condescending, sycophantic, verbose\n- Unclear output: Output was hard to read or interpret\n- Other: Model-behavior issue fitting none of the above"},{"name":"image_gen","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["prompt"],"type":"object","properties":{"prompt":{"description":"Text description of the image to generate.","type":"string"},"aspect_ratio":{"description":"Aspect ratio of the generated image, decide it based on the user's request. Defaults to 'auto'. 1:1 for square (icons, profiles), 16:9 for wide (landscapes, cinematic), 9:16 for tall (phone wallpapers, stories), 3:2 for horizontal photos, 2:3 for vertical (portraits, posters).","type":"string","default":"auto"}}},"type":"function","description":"Generate a new image from a text description using Imagine; returns the saved image's absolute path. When telling the user where it was saved, refer to it by its short session-relative path (e.g. `images/1.jpg`) rather than the absolute path, so it renders as a clickable link that opens the image. To produce multiple images, emit multiple tool calls with distinct prompts."},{"name":"image_edit","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["prompt","image"],"type":"object","properties":{"prompt":{"description":"A text description of the desired edit or transformation. Describe what the output image should look like, referencing the input image(s).","type":"string"},"image":{"description":"Reference image(s) to condition the edit on. Each is one reference, in priority order: (1) a user attachment — its placeholder token, e.g. \"[Image #1]\" (attachments have no path you can see, so never invent one); (2) an absolute filesystem path the user gave you; (3) a `data:image/...;base64,...` URL.","type":"array","items":{"type":"string"}},"aspect_ratio":{"description":"The aspect ratio of the output image. For single-image edits this is ignored — the output matches the input image's aspect ratio. For multi-image edits, defaults to 'auto'. Supported values: 1:1, 16:9, 9:16, 4:3, 3:4, 3:2, 2:3, 2:1, 1:2, 19.5:9, 9:19.5, 20:9, 9:20, auto.","type":"string","default":"auto"}}},"type":"function","description":"Edit or transform existing image(s) via the xAI Imagine API; use instead of image_gen for image-to-image work (preserve likeness, transfer style, remix). Returns the saved image's absolute path. When telling the user where it was saved, refer to it by its short session-relative path (e.g. `images/1.jpg`) rather than the absolute path, so it renders as a clickable link that opens the image. Each required `image` is one reference — a user-attachment token (e.g. \"[Image #1]\"), an absolute filesystem path, or a `data:image/...;base64,...` URL (see the `image` parameter for the resolution order and details)."},{"name":"image_to_video","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["image"],"type":"object","properties":{"prompt":{"description":"Optional prompt to guide the video generation model. If omitted, a natural animation applies automatically.","default":null,"anyOf":[{"type":"string","description":"Optional prompt to guide the video generation model. If omitted, a natural animation applies automatically."},{"type":"null","description":"Optional prompt to guide the video generation model. If omitted, a natural animation applies automatically."}]},"image":{"description":"Source image to animate. Provide an absolute filesystem path, HTTPS URL, or `data:image/...;base64,...` URL.","type":"string"},"duration":{"description":"Duration of the video generation, either 6 or 10 seconds. Default to 6 unless the user requests longer.","format":"uint32","minimum":0,"anyOf":[{"type":"integer","description":"Duration of the video generation, either 6 or 10 seconds. Default to 6 unless the user requests longer."},{"type":"null","description":"Duration of the video generation, either 6 or 10 seconds. Default to 6 unless the user requests longer."}]},"resolution_name":{"description":"Resolution name of the video generation, only specify it when user asks for a specific resolution, either 480p or 720p. Defaults to 480p unless the user specifically requests for higher quality.","type":"string","default":"480p"}}},"type":"function","description":"Generate a video from a single source image; returns the saved video's absolute path. When telling the user where it was saved, refer to it by its short session-relative path (e.g. `videos/1.mp4`) rather than the absolute path, so it renders as a clickable link that opens the video. Provide `image` for the image to animate and optionally a `prompt` to guide the animation. Use this tool when the user provides an image and wants it animated, turned into a video, or used as the first frame. Example: image_to_video(image=\"/Users/me/photo.jpg\", prompt=\"gentle camera push-in with wind moving the hair\", duration=6, resolution_name=\"480p\")"},{"name":"reference_to_video","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["prompt","aspect_ratio"],"type":"object","properties":{"prompt":{"description":"Prompt to guide the video generation model. Describe the desired video.","type":"string"},"images":{"description":"Reference images, up to 7 entries; the images are used as style/content references for the generated video (people, objects, clothing, settings). Each entry may be an absolute filesystem path, HTTPS URL, or `data:image/...;base64,...` URL. Reference them in the prompt as `<IMAGE_0>`, `<IMAGE_1>`, ... May be empty when `voices` is provided.","type":"array","items":{"type":"string"}},"voices":{"description":"Optional preset voices the subject(s) speak in, up to 3 entries, each a voice identifier from the built-in roster (e.g. \"ara\", \"eve\", \"leo\", \"rex\"; same voices as the xAI text-to-speech API; an unknown identifier fails with the list of available voices). Reference them in the prompt as `<AUDIO_0>`, `<AUDIO_1>`, `<AUDIO_2>`. Usable alongside `images` or on their own.","type":"array","items":{"type":"string"}},"aspect_ratio":{"description":"Aspect ratio of the generated video, decide it based on the user's request. 1:1 for square (icons, profiles), 16:9 for wide (landscapes, cinematic), 9:16 for tall (phone wallpapers, stories), 4:3 or 3:2 for horizontal photos, 3:4 or 2:3 for vertical (portraits, posters).","type":"string"},"duration":{"description":"Duration of the video in seconds, between 1 and 15. Defaults to 6.","format":"uint32","minimum":0,"anyOf":[{"type":"integer","description":"Duration of the video in seconds, between 1 and 15. Defaults to 6."},{"type":"null","description":"Duration of the video in seconds, between 1 and 15. Defaults to 6."}]},"resolution_name":{"description":"Resolution name of the video generation, only specify it when user asks for a specific resolution, either 480p or 720p. Defaults to 480p.","type":"string","default":"480p"}}},"type":"function","description":"Generate a video from reference images and/or preset voices, guided by a required text prompt; returns the saved video's absolute path. When telling the user where it was saved, refer to it by its short session-relative path (e.g. `videos/1.mp4`) rather than the absolute path, so it renders as a clickable link that opens the video. Provide up to 7 `images` (style/content references: people, objects, clothing, settings) and/or up to 3 `voices` (preset voice identifiers the subjects speak in); at least one of either is required. Tag references in the prompt as `<IMAGE_0>`, `<IMAGE_1>`, ... and `<AUDIO_0>`, `<AUDIO_1>`, ... Use this tool when the user wants a video referencing existing images without locking the first frame, or wants a speaking subject with a specific voice. Example: reference_to_video(prompt=\"The person from <IMAGE_0> presents the product from <IMAGE_1>, speaking with the voice from <AUDIO_0>\", images=[\"/Users/me/host.jpg\", \"/Users/me/product.jpg\"], voices=[\"eve\"], aspect_ratio=\"16:9\", duration=10, resolution_name=\"480p\")"},{"name":"write","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["file_path","content"],"properties":{"file_path":{"description":"The absolute path to the file to write.","type":"string"},"content":{"description":"The full file content to write.","type":"string"}},"type":"object"},"type":"function","description":"Create or overwrite a file.\n\n- Writing to an existing path replaces the file — read it first with the read_file tool.\n- Parent directories are created for you."},{"name":"spawn_agent","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["task_name","message"],"additionalProperties":false,"type":"object","properties":{"task_name":{"description":"Stable lowercase name for the named agent: letters, digits, `_`.","type":"string"},"message":{"description":"The work item delivered to the child as its first agent message.","type":"string"},"agent_type":{"description":"Subagent type to run (defaults to `general-purpose`).","anyOf":[{"type":"string","description":"Subagent type to run (defaults to `general-purpose`)."},{"type":"null","description":"Subagent type to run (defaults to `general-purpose`)."}]},"model":{"description":"Optional explicit model slug for the child.","anyOf":[{"type":"string","description":"Optional explicit model slug for the child."},{"type":"null","description":"Optional explicit model slug for the child."}]},"fork_turns":{"description":"Initial context: `none` (fresh), `all` (inherit the parent\nconversation; default), or a positive integer string for the most\nrecent N turns.","anyOf":[{"type":"string","description":"Initial context: `none` (fresh), `all` (inherit the parent\nconversation; default), or a positive integer string for the most\nrecent N turns."},{"type":"null","description":"Initial context: `none` (fresh), `all` (inherit the parent\nconversation; default), or a positive integer string for the most\nrecent N turns."}]}}},"type":"function","description":"Start a named background agent. task_name uses lowercase letters, digits, and underscores. fork_turns defaults to all; use none for fresh context or a positive integer string for recent turns. Same-model forks retain compatible history; cross-model forks use a plaintext digest. The host's depth and permission limits still apply. Reuse an existing task with followup_task."},{"name":"send_message","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["target","message"],"type":"object","properties":{"target":{"description":"Canonical task path, relative task name, or agent ID.","type":"string"},"message":{"description":"The message body (nonempty, at most 32768 bytes).","type":"string"}},"additionalProperties":false},"type":"function","description":"Deliver a message promptly to an agent by canonical task path, relative task name, or ID. Does not start a turn when the recipient is idle. Use followup_task to assign work."},{"name":"followup_task","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["target","message"],"type":"object","properties":{"target":{"description":"Canonical task path, relative task name, or agent ID.","type":"string"},"message":{"description":"The message body (nonempty, at most 32768 bytes).","type":"string"}},"additionalProperties":false},"type":"function","description":"Send work to a non-root agent. Starts a turn when idle and delivers promptly when running. Reuses a completed named agent with its own model, role, working directory, and history."},{"name":"list_agents","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","additionalProperties":false,"type":"object","properties":{"path_prefix":{"description":"Optional canonical task-path prefix filter (no trailing slash).","anyOf":[{"type":"string","description":"Optional canonical task-path prefix filter (no trailing slash)."},{"type":"null","description":"Optional canonical task-path prefix filter (no trailing slash)."}]}},"required":[]},"type":"function","description":"List this team's named agents and lifecycle status without exposing transcripts. Optionally filter by a canonical task-path prefix without a trailing slash."},{"name":"wait_agent","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","additionalProperties":false,"type":"object","properties":{"timeout_ms":{"description":"Deadline in milliseconds. Defaults to 30000; maximum 600000; 0 polls.","format":"uint64","minimum":0,"anyOf":[{"type":"integer","description":"Deadline in milliseconds. Defaults to 30000; maximum 600000; 0 polls."},{"type":"null","description":"Deadline in milliseconds. Defaults to 30000; maximum 600000; 0 polls."}]}},"required":[]},"type":"function","description":"Wait for agent messages or final-status activity. Returns an activity summary, not message contents. Ends early for steered user input. timeout_ms defaults to 30000, maximum 600000; 0 polls."},{"name":"interrupt_agent","parameters":{"$schema":"http://json-schema.org/draft-07/schema#","required":["target"],"type":"object","properties":{"target":{"description":"The agent whose current turn should be interrupted.","type":"string"}},"additionalProperties":false},"type":"function","description":"Interrupt an agent's current turn without deleting its history or task identity. The agent remains reusable with followup_task. Cannot target the root or yourself."}],"top_p":1.0,"reasoning":{"effort":null,"summary":null},"status":"in_progress","store":true},"model":"gemini-3.5-flash"}"##, // frame_index 1 — response.in_progress
    r#"{"type":"response.output_item.added","output_index":0,"item":{"id":"rs_8c805841-8a44-41fd-9628-f7235614d7ae","type":"reasoning","status":"in_progress"},"model":"gemini-3.5-flash"}"#, // frame_index 2 — response.output_item.added (reasoning)
    r#"{"type":"response.reasoning_summary_text.delta","item_id":"rs_8c805841-8a44-41fd-9628-f7235614d7ae","output_index":0,"delta":"**Confirming Final Command**\n\nI am currently reviewing the immediate context to ensure no other constraints or prior instructions conflict with your explicit directive. My focus is on verifying that the sole requirement is to provide the specified reply.\n\n","model":"gemini-3.5-flash"}"#, // frame_index 3 — response.reasoning_summary_text.delta (NO sequence_number, NO summary_index — the .87 hole)
    r#"{"type":"response.reasoning_summary_part.done","item_id":"rs_8c805841-8a44-41fd-9628-f7235614d7ae","output_index":0,"sequence_number":5,"summary_index":0,"part":{"type":"summary_text","text":"**Confirming Final Command**\n\nI am currently reviewing the immediate context to ensure no other constraints or prior instructions conflict with your explicit directive. My focus is on verifying that the sole requirement is to provide the specified reply.\n\n"},"model":"gemini-3.5-flash"}"#, // frame_index 5 — response.reasoning_summary_part.done
    r#"{"type":"response.output_item.done","output_index":0,"sequence_number":6,"item":{"id":"rs_8c805841-8a44-41fd-9628-f7235614d7ae","type":"reasoning","summary":[{"type":"summary_text","text":"**Confirming Final Command**\n\nI am currently reviewing the immediate context to ensure no other constraints or prior instructions conflict with your explicit directive. My focus is on verifying that the sole requirement is to provide the specified reply.\n\n"}]},"model":"gemini-3.5-flash"}"#, // frame_index 6 — response.output_item.done (reasoning)
    r#"{"type":"response.output_text.delta","item_id":"msg_1ba862e0-7285-48cb-acf1-589f40eacb5f","output_index":0,"content_index":0,"delta":"AT-AZ-VXG-DONE","model":"gemini-3.5-flash"}"#, // frame_index 7 — response.output_text.delta
    r#"{"type":"response.output_text.done","item_id":"msg_1ba862e0-7285-48cb-acf1-589f40eacb5f","output_index":0,"content_index":0,"text":"AT-AZ-VXG-DONE","model":"gemini-3.5-flash"}"#, // frame_index 8 — response.output_text.done
    r#"{"type":"response.content_part.done","item_id":"msg_1ba862e0-7285-48cb-acf1-589f40eacb5f","output_index":0,"content_index":0,"part":{"type":"reasoning_text","reasoning":"**Confirming Final Command**\n\nI am currently reviewing the immediate context to ensure no other constraints or prior instructions conflict with your explicit directive. My focus is on verifying that the sole requirement is to provide the specified reply.\n\n"},"model":"gemini-3.5-flash"}"#, // frame_index 9 — response.content_part.done, part {type: reasoning_text, reasoning: ...} — THE KILL FRAME (no text, no seq)
    r#"{"type":"response.output_item.done","output_index":0,"sequence_number":1,"item":{"id":"msg_1ba862e0-7285-48cb-acf1-589f40eacb5f","status":"completed","type":"message","role":"assistant","content":[{"type":"output_text","text":"AT-AZ-VXG-DONE","annotations":[]}]},"model":"gemini-3.5-flash"}"#, // frame_index 10 — response.output_item.done (message, assistant text)
    r#"{"type":"response.completed","response":{"id":"resp_bGl0ZWxsbTpjdXN0b21fbGxtX3Byb3ZpZGVyOnZlcnRleF9haTttb2RlbF9pZDo2MjE2ZDUyNzQ1Mzc2ZmQ1MGNlMGI5ZGU4MTY5MjA2ZDYwY2YwNmI3N2I4OWZhN2IyNmM5MmZjZjZmNWRjYTZhO3Jlc3BvbnNlX2lkOnEwR3VhcERVTGY2WW90UVA4Ym11a1E4","created_at":1789804973,"metadata":{},"model":"gemini-3.5-flash","object":"response","output":[{"type":"reasoning","id":"rs_8c805841-8a44-41fd-9628-f7235614d7ae","status":"completed","role":"assistant","content":[{"type":"output_text","text":"**Confirming Final Command**\n\nI am currently reviewing the immediate context to ensure no other constraints or prior instructions conflict with your explicit directive. My focus is on verifying that the sole requirement is to provide the specified reply.\n\n","annotations":[]}]},{"type":"message","id":"msg_1ba862e0-7285-48cb-acf1-589f40eacb5f","status":"completed","role":"assistant","content":[{"type":"output_text","text":"AT-AZ-VXG-DONE","annotations":[]}]}],"parallel_tool_calls":false,"temperature":0.0,"tool_choice":"auto","tools":[],"status":"completed","text":{},"usage":{"input_tokens":30392,"input_tokens_details":{"cached_tokens":0,"text_tokens":30392},"output_tokens":312,"output_tokens_details":{"reasoning_tokens":304,"text_tokens":8},"total_tokens":30704,"cost":0.048396},"provider_specific_fields":{"traffic_type":"ON_DEMAND","thought_signatures":["AY89a1+QhjwuRSIfk9toU5IPRXYI5LQnw5jI+nXypQYMlPZFz8Aqn2+tIXrlfpDW7E4Zlm3pq3Yt5Vs2vnmOrqbcNU/kErnyQG2v9OtzhcI94PxAVVr+1P52vFjkoRHfh4s8+uWVa4pcsX7k6uxwfPmjm3PmEqKYUtFzKogVWwz2Ic/T9eReCXytXD2htiz6XDk9nByIdDhsfPYWfnPeo3vpZtehsKMIe7w5lNxQWffdhEFy5xwuLLyV1wDdpGuffoBWnzyqgIYNY7h/32hkKpJLpN0X4q2FwrLmAIry+THkcN0+4tlq0dkzhXBzVtfdGlNbsgFXPodgpeFZeLK0Ey6uGijLsETZw0zD7lWuZ7rwj+vzN6xBCnJKUzONcGJzomzH8IK/FSAiFLwzvGFklzwk1zm1ADe56PleRIQLI2fTYcy2gO0Vc5Nlgt00/XcuUNH83OsZS8jeY/9DaIQyNBCMpXiT+T4IABn1ysg8nYQTFpeOfVpZWsyVWqLh7WoViXeYAoJaRuo77OjDBnZ68ZW//W+4YHE3lHYamFtUqYHjRHdj1JPavJ1Vxnsoe1clFFfuR9ZVUZivVPRLD47VxGS0rgO6rVuJT7I4U0beqZrRDr33ETyQkcot62+UaPzVv4jvwu8XpO4dH7lF1e9LjMRMSjs9hoVL4hDRstOyo0kn9KM/+kVgozVuJZs//2dYhmmSRauUPxnBDMA2pKkIc22B4IyBYGjPQXkGLRjOO0/ZN9bV61GQ9V7DaLZVS5iMlknkg0WOWiPU/ihahIAy5E0pef8s4lW3sgw9wd6dN3VkXhdqW4t+fSDEcrnpIrt3BrS9qNbffX3uWTi+bWUNkIrlcxMGSnYe9z2HSdxarDyOJaTXp0ALc4h6L7x1o/CmkguL+aiiKdrHG6uNSgxneSQ+VAEe1bQP5W2ivZ6dFPifn5Puy56MhYozQNxz6+ZIcMu7hU3WeYsjqVHaONpWjKRZ3RGmxgMoUQPHoDYRZFovFF6+PK2NbqM97h+jTqmRzIfWDCgn5qe/3m+7DDgJeMj/F5S8uMscn9mstEAwRYB/bA/4dI3nep7Jp53dSPETqzR15bOG3VdbCHmtDXN7mgKG6x6WrHEIT1uVRg5U6/iNgdZrHLws9YdTPsXqKlQ6CX2Y3IxjrzFjuATFxy+UgLVTOTRS5AXj8vmQiyng6rzqRZx/MKSaY6PqKc8YP/IPXtX6tGbKmTF+N2OBC8C4OpEJ59iZL6mNJTaD4Zb8X4Jv4tn8j1wjlVmfD0F6RP/AK2qBG8Ebg/71JaaJI57aP+BiNy2SCd9aeqnuf9gvJpfgcTqb2NZboTLsGP3kYty/smWnpRF2oD4encvDjhP2IvxlQbH/VwmhaQhhrBvfM3xwPlDdeYWuGRAxbRpsEEH5nSgKZYbx9h94w8KS5n/EPKkoa9EEJ8y3SbSuQ+Pf+bagvrTgtYZWmL6oEox5MPwph+2wm1lut7VzWaIdcOUvRQCxRdKJeWZnLjYp4I2BDwipwqq3YiNWm1HFBJI1FghMtASBXwZYcbKhgaa9bUZLCjEuodq5IhuDBK+YvoGbyf2NNjpZGmPEqdbW+av6ZjUuEu63EZsqHpf3nViMS2PmfTrxjuMOTApRZc52N2feSjXuvNrKZxriiO/1Yf2MbZDh/3aELLazAFlbn1me8/nziiEKfknTLyz+ezwOyAVkk967XlM/2aXLBNhlO2XgnoOYAx/qMwtK8aIbTXtzPv8gYP26YLk9L189kI2pN6dbztA4qSloUEr8pbYC/Pd/oMg+B9El80HJRHIWIDkoyddnOCePjih514Osda+K8s+8+w9IRFPvv/ZFJxYVr0JGD42TiVYU+Y2wXWJpSUblPTrv/qhErkV5H0f8eFOL/13E0ONFw420v3WbI0zsg7OUb0WwpfEl4rbNCrRAtThFk7XZaLNh0ahWzSg0NoexlhnyJNjljcqV6oTl7jrw0qXsq8tdYyySLA=="]}},"model":"gemini-3.5-flash"}"#, // frame_index 11 — response.completed (both output items)
];

/// The verbatim AT-AZ-VXG-r2 thinking + direct-text turn. The summary delta
/// carries the .87 hole (NO `sequence_number`, NO `summary_index`) and frame 9
/// carries the SUMMINDEX-2 kill frame: a `reasoning_text` content part whose
/// payload field is `reasoning` and which has NO `text`, which `async_openai`
/// declares required on `ReasoningTextContent`. Without the sanitize-path
/// repairs the turn dies on the kill frame with
/// `serialization error: missing field 'text'`. This exercises the real SSE
/// path and proves the two repairs compose.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn responses_stream_reasoning_text_part_missing_text_completes_turn() {
    let app = Router::new().route(
        "/v1/responses",
        post(move || async move {
            let events = sse_events_to_axum(
                R2_RESP_003_KILL_STREAM
                    .iter()
                    .map(|data| SseEvent::data((*data).to_string()))
                    .collect(),
            );
            Sse::new(stream::iter(
                events.into_iter().map(Ok::<_, std::convert::Infallible>),
            ))
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let handle = SamplerActor::spawn(
        responses_config(server.base_url(), None),
        RetryPolicy::default(),
        event_tx,
    );

    let result = handle
        .submit_and_collect(RequestId::from("req-r2-kill-frame"), user_request("hi"))
        .await;
    server.shutdown();

    let (response, _metrics) =
        result.expect("turn completes over the verbatim r2 kill stream");
    assert_eq!(response.assistant_text(), "AT-AZ-VXG-DONE");
}

/// The agent loop over a gateway: a reasoning item plus a tool call, with every gateway-omitted
/// field absent. If any event fails to deserialize the turn dies and the agent silently stops
/// taking actions, so this asserts the tool call survives the repair path intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn responses_gateway_shaped_tool_call_survives() {
    let app = Router::new().route(
        "/v1/responses",
        post(move || async move {
            let events = as_gateway_payload(sse::responses_api_reasoning_then_tool_call_events(
                "a thought",
                "call_1",
                "read_file",
                r#"{"path":"a.txt"}"#,
                "test-model",
            ));
            let events = sse_events_to_axum(events);
            Sse::new(stream::iter(
                events.into_iter().map(Ok::<_, std::convert::Infallible>),
            ))
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let handle = SamplerActor::spawn(
        responses_config(server.base_url(), None),
        RetryPolicy::default(),
        event_tx,
    );

    let result = handle
        .submit_and_collect(RequestId::from("req-gateway-tool"), user_request("hi"))
        .await;
    server.shutdown();

    let (response, _metrics) = result.expect("gateway-shaped tool call turn completes");
    let calls = response.tool_calls();
    assert_eq!(calls.len(), 1, "the tool call reached the agent loop");
    assert_eq!(calls[0].name, "read_file");
}

/// Acceptance spec for the recovery rung: a confident tail signal is resampled once while its detector label remains observable.
/// The clean second response is accepted on its own budget even with transport retries disabled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn responses_confident_doom_loop_signal_resamples_once() {
    let counter = Arc::new(AtomicU32::new(0));
    let counter_handler = Arc::clone(&counter);
    let bodies = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let bodies_handler = Arc::clone(&bodies);
    let app = Router::new().route(
        "/v1/responses",
        post(move |body: String| {
            let counter = Arc::clone(&counter_handler);
            let bodies = Arc::clone(&bodies_handler);
            async move {
                bodies
                    .lock()
                    .unwrap()
                    .push(serde_json::from_str(&body).unwrap());
                let attempt = counter.fetch_add(1, Ordering::SeqCst);
                let events = if attempt == 0 {
                    sse::responses_api_doom_loop_terminal_only_events(
                        &["tail_repetition:8@thinking"],
                        "loop loop loop",
                        "poisoned answer",
                        "test-model",
                    )
                } else {
                    sse::responses_api_reasoning_and_text_events(
                        "fresh thought",
                        "clean answer",
                        "test-model",
                    )
                };
                let events = sse_events_to_axum(events);
                Sse::new(stream::iter(
                    events.into_iter().map(Ok::<_, std::convert::Infallible>),
                ))
            }
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let mut config = responses_config(server.base_url(), Some(DoomLoopRecoveryPolicy::default()));
    config.max_retries = Some(0);
    let handle = SamplerActor::spawn(config, RetryPolicy::default(), event_tx);

    let collected = handle
        .submit_and_collect_with_metadata(RequestId::from("req-doom-resample"), user_request("hi"))
        .await;
    server.shutdown();

    assert!(collected.terminal_event_queued);
    assert_eq!(
        collected.doom_loop_signals,
        vec!["tail_repetition:8@thinking".to_string()],
    );
    assert_eq!(1, collected.doom_loop_recovery_attempts.len());
    assert_eq!(
        collected.doom_loop_recovery_attempts[0].triggers,
        vec!["tail_repetition:8@thinking".to_string()]
    );
    let (response, _metrics) = collected
        .result
        .expect("recovery accepts the clean resample");
    assert_eq!(counter.load(Ordering::SeqCst), 2, "exactly one resample");
    assert_eq!(response.assistant_text(), "clean answer");
    assert!(
        response.doom_loop_signals.is_empty(),
        "the accepted response is the clean resample"
    );

    let events = drain_until_terminal(&mut event_rx, Duration::from_secs(1)).await;
    assert!(events.iter().any(|event| matches!(
        event,
        SamplingEvent::DoomLoopSignals { triggers, .. }
            if triggers == &["tail_repetition:8@thinking".to_string()]
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        SamplingEvent::Retrying {
            doom_loop_triggers: Some(triggers),
            ..
        } if triggers == &["tail_repetition:8@thinking".to_string()]
    )));

    let bodies = bodies.lock().unwrap();
    let retry_input = bodies[1]["input"].as_array().unwrap();
    // The unified multi-agent-mode item (apex-ayl.86, R-UNIFIED-ITEM)
    // rides every responses-wire request: the resample's input grows to
    // 5 — system, reasoning, assistant, the fresh developer item (before
    // the last user message), and the reminder user turn.
    assert_eq!(retry_input.len(), 5);
    assert_eq!(retry_input[1]["summary"][0]["text"], "loop loop loop");
    assert_eq!(retry_input[2]["role"], "assistant");
    assert_eq!(retry_input[2]["content"], "poisoned answer");
    assert_eq!(retry_input[3]["role"], "developer");
    let mode_text = retry_input[3]["content"][0]["text"]
        .as_str()
        .expect("the unified item is a text part");
    assert!(
        mode_text.starts_with("<multi_agent_mode>")
            && mode_text.ends_with("</multi_agent_mode>")
            && mode_text.contains("explicit_request_only"),
        "the resample carries the unified explicit-only item: {mode_text}"
    );
    assert_eq!(retry_input[4]["role"], "user");
    let reminder = retry_input[4]["content"]
        .as_str()
        .expect("the reminder is a text item");
    assert!(
        reminder.starts_with("<system_reminder>") && reminder.ends_with("</system_reminder>"),
        "the retry closes with a synthetic system-reminder envelope: {reminder}"
    );
}

/// A caller that opted into `retry_only_before_output` cannot retract text it already received.
/// So a doomed turn that streamed output fails instead of resampling over the delivered prefix.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn responses_doom_loop_does_not_resample_after_output_when_retry_only_before_output() {
    let counter = Arc::new(AtomicU32::new(0));
    let counter_handler = Arc::clone(&counter);
    let app = Router::new().route(
        "/v1/responses",
        post(move || {
            let counter = Arc::clone(&counter_handler);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let events = sse_events_to_axum(sse::responses_api_doom_loop_terminal_only_events(
                    &["tail_repetition:8@thinking"],
                    "loop loop loop",
                    "poisoned answer",
                    "test-model",
                ));
                Sse::new(stream::iter(
                    events.into_iter().map(Ok::<_, std::convert::Infallible>),
                ))
            }
        }),
    );
    let server = MockServer::spawn(app).await;
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let retry_policy = RetryPolicy {
        retry_only_before_output: true,
        ..RetryPolicy::default()
    };
    let handle = SamplerActor::spawn(
        responses_config(server.base_url(), Some(DoomLoopRecoveryPolicy::default())),
        retry_policy,
        event_tx,
    );

    let result = handle
        .submit_and_collect(RequestId::from("req-doom-no-retract"), user_request("hi"))
        .await;
    server.shutdown();

    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "no resample after output"
    );
    assert!(
        matches!(
            result,
            Err(xai_grok_sampling_types::SamplingError::DoomLoopDetected { .. })
        ),
        "the doomed turn is surfaced rather than resampled: {result:?}"
    );
}

// ---------------------------------------------------------------------------
// Helpers for draining the event channel
// ---------------------------------------------------------------------------

/// Drain the event channel until a terminal event (`Completed` or `Failed`) is received, or until `deadline` elapses.
async fn drain_until_terminal(
    rx: &mut mpsc::UnboundedReceiver<SamplingEvent>,
    timeout: Duration,
) -> Vec<SamplingEvent> {
    let mut out = Vec::new();
    let start = tokio::time::Instant::now();
    loop {
        let elapsed = start.elapsed();
        if elapsed >= timeout {
            panic!(
                "drain_until_terminal timed out after {:?}; got {} events",
                timeout,
                out.len()
            );
        }
        let remaining = timeout - elapsed;
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(ev)) => {
                let terminal = matches!(
                    ev,
                    SamplingEvent::Completed { .. } | SamplingEvent::Failed { .. }
                );
                out.push(ev);
                if terminal {
                    return out;
                }
            }
            Ok(None) => panic!("event channel closed before terminal event"),
            Err(_) => panic!(
                "drain_until_terminal timed out after {:?}; got {} events",
                timeout,
                out.len()
            ),
        }
    }
}

/// Wait for the next event matching `pred`, or return `None` on timeout.
async fn await_event_matching(
    rx: &mut mpsc::UnboundedReceiver<SamplingEvent>,
    mut pred: impl FnMut(&SamplingEvent) -> bool,
    timeout: Duration,
) -> Option<SamplingEvent> {
    let start = tokio::time::Instant::now();
    loop {
        let elapsed = start.elapsed();
        if elapsed >= timeout {
            return None;
        }
        let remaining = timeout - elapsed;
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(ev)) => {
                if pred(&ev) {
                    return Some(ev);
                }
            }
            Ok(None) => return None,
            Err(_) => return None,
        }
    }
}

// ---------------------------------------------------------------------------
// Codex remote compaction v2
// ---------------------------------------------------------------------------

/// Provenance: open-grok@240c99c9 xai-grok-sampler/tests/test_actor.rs:188 :: xai_function_exec_history_request (rewritten on `ToolResult` — the worktree `ConversationItem` enum has no `CustomToolOutput` variant)
fn xai_function_exec_history_request() -> ConversationRequest {
    ConversationRequest::from_items(vec![
        ConversationItem::assistant_tool_calls(vec![xai_grok_sampling_types::ToolCall {
            id: "call-xai-exec".into(),
            name: "exec".into(),
            arguments: r#"{"source":"return 42"}"#.into(),
        }]),
        ConversationItem::tool_result("call-xai-exec", "42"),
        ConversationItem::user("continue"),
    ])
}

/// Provenance: open-grok@240c99c9 xai-grok-sampler/tests/test_actor.rs:2102 :: codex_remote_compaction_v2_uses_responses_stream_contract (adapted per review Claim 5: `SamplingClient::new` + `model_family: Some("codex")`; turn-state capture/assertion and the two coalescing assertions dropped; `prompt_cache_key` set explicitly on the request; reasoning summary asserts `"concise"` — the worktree From-projection hardcodes it)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_remote_compaction_v2_uses_responses_stream_contract() {
    use std::sync::Mutex;

    let captured: Arc<Mutex<Vec<(axum::http::HeaderMap, serde_json::Value)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let captured_handler = Arc::clone(&captured);
    let app = Router::new().route(
        "/v1/responses",
        post(
            move |headers: axum::http::HeaderMap, body: axum::Json<serde_json::Value>| {
                let captured = Arc::clone(&captured_handler);
                async move {
                    captured.lock().unwrap().push((headers, body.0));
                    let events = vec![
                        Event::default().event("response.output_item.done").data(
                            json!({
                                "type": "response.output_item.done",
                                "output_index": 0,
                                "item": {"type": "message", "id": "ignored-message"}
                            })
                            .to_string(),
                        ),
                        Event::default().event("response.output_item.done").data(
                            json!({
                                "type": "response.output_item.done",
                                "output_index": 1,
                                "item": {
                                    "type": "compaction",
                                    "encrypted_content": "opaque-v2-summary"
                                }
                            })
                            .to_string(),
                        ),
                        Event::default().event("response.completed").data(
                            json!({
                                "type": "response.completed",
                                "response": {
                                    "id": "resp_compact_v2",
                                    "usage": {
                                        "input_tokens": 321,
                                        "output_tokens": 9,
                                        "input_tokens_details": {"cached_tokens": 123},
                                        "output_tokens_details": {"reasoning_tokens": 7}
                                    }
                                }
                            })
                            .to_string(),
                        ),
                    ];
                    Sse::new(stream::iter(
                        events.into_iter().map(Ok::<_, std::convert::Infallible>),
                    ))
                    .into_response()
                }
            },
        ),
    );
    let server = MockServer::spawn(app).await;
    let mut config = responses_config(server.base_url(), None);
    config.model_family = Some("codex".into());
    config.model = "gpt-5.6-sol".into();
    config.reasoning_effort = Some(xai_grok_sampling_types::ReasoningEffort::High);
    config
        .extra_headers
        .insert("x-codex-beta-features".into(), "existing_feature".into());

    let client = xai_grok_sampler::SamplingClient::new(config).expect("Codex sampling client");
    let mut request = xai_function_exec_history_request();
    request.x_grok_session_id = Some("session-cache-key".into());
    request.prompt_cache_key = Some("session-cache-key".into());
    request.reasoning_effort = Some(xai_grok_sampling_types::ReasoningEffort::High);
    request.hosted_tools = vec![xai_grok_sampling_types::HostedTool::WebSearch { options: None }];
    request.json_schema = Some(json!({
        "type": "object",
        "properties": {"answer": {"type": "string"}},
        "required": ["answer"],
        "additionalProperties": false
    }));
    let result = client
        .compact_codex_conversation_v2(request, "base instructions", true)
        .await
        .expect("remote compaction v2 should complete over /responses SSE");
    server.shutdown();

    assert_eq!(result.response_id, "resp_compact_v2");
    let usage = result.usage.expect("completion usage should be captured");
    assert_eq!(usage.input_tokens, 321);
    assert_eq!(usage.output_tokens, 9);
    assert_eq!(usage.total_tokens, 330);
    assert_eq!(usage.input_tokens_details.cached_tokens, 123);
    assert_eq!(usage.output_tokens_details.reasoning_tokens, 7);
    let replay = ConversationRequest::from_items(vec![result.compaction_item])
        .raw_responses_input_replacements(
            xai_grok_sampling_types::ResponsesReplayDialect::Codex,
        );
    assert_eq!(replay[0].value["encrypted_content"], "opaque-v2-summary");
    assert!(
        replay[0].value.get("id").is_none(),
        "the typed empty-ID sentinel must never leak into replay input"
    );

    let captured = captured.lock().unwrap();
    assert_eq!(captured.len(), 1, "v2 compaction must make one request");
    let (headers, body) = &captured[0];
    assert_eq!(
        headers
            .get("x-codex-beta-features")
            .and_then(|value| value.to_str().ok()),
        Some("existing_feature,remote_compaction_v2")
    );
    assert_eq!(body["model"], "gpt-5.6-sol");
    assert_eq!(body["instructions"], "base instructions");
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["parallel_tool_calls"], true);
    assert_eq!(body["prompt_cache_key"], "session-cache-key");
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert_eq!(body.pointer("/reasoning/effort"), Some(&json!("high")));
    assert_eq!(body.pointer("/reasoning/summary"), Some(&json!("concise")));
    assert_eq!(
        body.pointer("/text/format/type"),
        Some(&json!("json_schema"))
    );
    assert!(body["tools"].as_array().is_some_and(|tools| {
        tools
            .iter()
            .any(|tool| tool.get("type") == Some(&json!("web_search")))
    }));
    assert!(body["include"].as_array().is_some_and(|includes| {
        includes
            .iter()
            .any(|include| include == "reasoning.encrypted_content")
    }));
    let input = body["input"].as_array().expect("input must be an array");
    assert_eq!(
        input.last(),
        Some(&json!({"type": "compaction_trigger"})),
        "the compaction trigger must be the exact final input item"
    );
    assert_eq!(
        input
            .iter()
            .filter(|item| item.get("type") == Some(&json!("compaction_trigger")))
            .count(),
        1,
        "the request must contain exactly one compaction trigger"
    );
    assert!(
        body.get("previous_response_id")
            .is_none_or(|value| matches!(value, serde_json::Value::Null)),
        "HTTP compaction v2 replays full input and must not chain response IDs"
    );
}

/// Provenance: hyper-grok-build@45e984f3 packages/ai/xai-grok-sampler/tests/codex_compact.rs:82 :: codex_compact_uses_unary_schema_and_replays_turn_state (adapted to the v2-over-`/responses` protocol: the v1 unary endpoint is 403-blocked on the proxy and unmodeled in the worktree; turn-state replay dropped — review Claim 5. The carried contract: live auth (bearer + configured extra headers) rides the v2 request, and the next turn's conversation request splices the exact opaque compaction output.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_remote_compaction_v2_carries_live_auth_and_splices_next_turn() {
    use std::sync::{Mutex, atomic::AtomicU32};

    let request_number = Arc::new(AtomicU32::new(0));
    let captured: Arc<Mutex<Vec<(axum::http::HeaderMap, serde_json::Value)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let captured_handler = Arc::clone(&captured);
    let counter_handler = Arc::clone(&request_number);
    let app = Router::new().route(
        "/v1/responses",
        post(
            move |headers: axum::http::HeaderMap, body: axum::Json<serde_json::Value>| {
                let captured = Arc::clone(&captured_handler);
                let counter = Arc::clone(&counter_handler);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    captured.lock().unwrap().push((headers, body.0));
                    let events = if counter.load(Ordering::SeqCst) == 1 {
                        // v2 compaction stream: one opaque compaction item + terminal
                        vec![
                            Event::default().event("response.output_item.done").data(
                                json!({
                                    "type": "response.output_item.done",
                                    "output_index": 0,
                                    "item": {
                                        "type": "compaction",
                                        "encrypted_content": "ENCRYPTED_COMPACT_STATE"
                                    }
                                })
                                .to_string(),
                            ),
                            Event::default().event("response.completed").data(
                                json!({
                                    "type": "response.completed",
                                    "response": {"id": "resp_compact_next_turn"}
                                })
                                .to_string(),
                            ),
                        ]
                    } else {
                        // next turn: ordinary message stream
                        sse::responses_api_events("next turn text", "gpt-5.6-sol")
                    };
                    Sse::new(stream::iter(
                        events.into_iter().map(Ok::<_, std::convert::Infallible>),
                    ))
                    .into_response()
                }
            },
        ),
    );
    let server = MockServer::spawn(app).await;
    let mut config = responses_config(server.base_url(), None);
    config.model_family = Some("codex".into());
    config.model = "gpt-5.6-sol".into();
    config
        .extra_headers
        .insert("x-openai-originator".into(), "grok-shell".into());
    let client = xai_grok_sampler::SamplingClient::new(config).expect("codex client");

    let compact_request = ConversationRequest::from_items(vec![
        ConversationItem::system("authoritative instructions"),
        ConversationItem::user("keep this request"),
    ])
    .with_model("gpt-5.6-sol");
    let result = client
        .compact_codex_conversation_v2(compact_request, "authoritative instructions", true)
        .await
        .expect("v2 compaction succeeds");

    // Next turn: a conversation carrying the opaque compaction carrier must
    // splice the exact provider item at the typed placeholder's position.
    let mut next_turn = ConversationRequest::from_items(vec![result.compaction_item]);
    next_turn.model = Some("gpt-5.6-sol".into());
    // STABLE-REMINDER-1 (apex-ayl.110): test client has no per-session
    // anchor state — `&mut None` (conv-id gating keeps this Legacy,
    // i.e. byte-identical to the pre-cut wire shape the test asserts).
    let _stream = client
        .conversation_stream_responses(next_turn, &mut None)
        .await
        .expect("next-turn stream starts");
    server.shutdown();

    let captured = captured.lock().unwrap();
    assert_eq!(captured.len(), 2, "compaction + one next-turn request");

    // (1) the v2 request carries live auth and the configured extra headers
    let (compact_headers, _) = &captured[0];
    assert_eq!(
        compact_headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer test-key")
    );
    assert_eq!(
        compact_headers
            .get("x-openai-originator")
            .and_then(|value| value.to_str().ok()),
        Some("grok-shell")
    );

    // (2) the next turn receives the exact opaque item, byte-for-byte
    let next_input = captured[1].1["input"]
        .as_array()
        .expect("next-turn input array");
    let spliced = next_input
        .iter()
        .find(|item| item.get("type") == Some(&json!("compaction")))
        .expect("the exact compaction item must be spliced into the next turn: {next_input:?}");
    assert_eq!(
        spliced["encrypted_content"], "ENCRYPTED_COMPACT_STATE",
        "the spliced item must be the exact provider payload"
    );
}

/// Provenance: hyper-grok-build@45e984f3 packages/ai/xai-grok-sampler/tests/codex_compact.rs:155 :: unsupported_compact_endpoint_is_cached_but_auth_is_not_fallback + unavailable_compact_model_channel_falls_back_but_generic_503_does_not (adapted to v2-over-`/responses`: the worktree sampler makes exactly one attempt and classifies the error; the 3-attempt retry loop and sticky Schema suppression live in the shell `run_compact_inner` (commit 3), so this test pins the classification, not retry behavior)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_remote_compaction_v2_classifies_failures_without_sampler_retry() {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    async fn classify(
        status: StatusCode,
        body: serde_json::Value,
    ) -> (xai_grok_sampling_types::SamplingError, usize) {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_handler = Arc::clone(&calls);
        let app = Router::new().route(
            "/v1/responses",
            post(move || {
                let calls = Arc::clone(&calls_handler);
                async move {
                    calls.fetch_add(1, AtomicOrdering::SeqCst);
                    (status, axum::Json(body))
                }
            }),
        );
        let server = MockServer::spawn(app).await;
        let mut config = responses_config(server.base_url(), None);
        config.model_family = Some("codex".into());
        config.model = "gpt-5.6-sol".into();
        let client = xai_grok_sampler::SamplingClient::new(config).expect("codex client");
        let request = ConversationRequest::from_items(vec![ConversationItem::user("compact me")])
            .with_model("gpt-5.6-sol");
        let error = client
            .compact_codex_conversation_v2(request, "", true)
            .await
            .expect_err("failure must classify");
        server.shutdown();
        (error, calls.load(AtomicOrdering::SeqCst))
    }

    // 400 invalid_request_error: non-retryable API error, one request
    let (error, requests) = classify(
        StatusCode::BAD_REQUEST,
        json!({"error": {"message": "invalid request", "code": "invalid_request_error"}}),
    )
    .await;
    assert_eq!(requests, 1, "the sampler must not retry a 400");
    assert!(!error.is_retryable(), "400 must be non-retryable: {error}");
    assert!(matches!(
        error,
        xai_grok_sampling_types::SamplingError::Api { status, .. }
            if status == StatusCode::BAD_REQUEST
    ));

    // 401: auth rejection, never retried by the sampler
    let (error, requests) = classify(
        StatusCode::UNAUTHORIZED,
        json!({"error": {"message": "bad token"}}),
    )
    .await;
    assert_eq!(requests, 1, "the sampler must not retry a 401");
    assert!(
        !error.is_retryable(),
        "auth rejection must not be retryable"
    );
    assert!(matches!(
        error,
        xai_grok_sampling_types::SamplingError::Auth { .. }
    ));

    // generic 503: retryable — the shell attempt loop owns the retries
    let (error, requests) = classify(
        StatusCode::SERVICE_UNAVAILABLE,
        json!({"error": {"message": "temporarily unavailable"}}),
    )
    .await;
    assert_eq!(requests, 1, "the sampler must make exactly one attempt");
    assert!(error.is_retryable(), "503 must be retryable: {error}");
}
