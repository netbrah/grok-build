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
        cache_ttl: None,
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
    assert_eq!(retry_input.len(), 4);
    assert_eq!(retry_input[1]["summary"][0]["text"], "loop loop loop");
    assert_eq!(retry_input[2]["role"], "assistant");
    assert_eq!(retry_input[2]["content"], "poisoned answer");
    assert_eq!(retry_input[3]["role"], "user");
    let reminder = retry_input[3]["content"]
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
        .compact_codex_conversation_v2(request, "base instructions")
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
        .raw_codex_input_replacements();
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
        .compact_codex_conversation_v2(compact_request, "authoritative instructions")
        .await
        .expect("v2 compaction succeeds");

    // Next turn: a conversation carrying the opaque compaction carrier must
    // splice the exact provider item at the typed placeholder's position.
    let mut next_turn = ConversationRequest::from_items(vec![result.compaction_item]);
    next_turn.model = Some("gpt-5.6-sol".into());
    let _stream = client
        .conversation_stream_responses(next_turn)
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
            .compact_codex_conversation_v2(request, "")
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
