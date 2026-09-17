//! REQVALID-1 47a, T16: over-cap Messages requests are rejected locally
//! BEFORE transport — the wire never sees their bytes. The under-cap
//! control proves the mock endpoint is live, so the zero-hit assertion
//! below is not vacuous.
//!
//! REQVALID-1 47b, D-5 (encode counting + retry carrier):
//!   - T20 `upload_reset_retry_reencodes_stripped` — a reset while
//!     uploading a large (~4 MB base64) image trips the image-strip retry:
//!     exactly one extra encode, and the re-sent body is byte-identical to
//!     a fresh encode of the image-stripped conversation.
//!   - T22 `retry_backoff_reuses_exact_encoded_bytes` — a 500 trips a
//!     backoff retry that must reuse the exact encoded bytes (NIT-2): two
//!     HTTP hits, one encode (the counter pin is the RED-2 red pin; the
//!     byte-equality pin holds in both phases).
//!   - T23 `image_strip_retry_revalidates_and_reencodes` — a 413 trips the
//!     image-strip retry: two encodes total, and the second body equals a
//!     fresh encode of the stripped conversation (green by construction in
//!     RED-2 per SP-2n; GREEN pins two-not-three).
//!   - T25 `cached_carrier_debug_no_body_leak` — the carrier's Debug
//!     renders only the byte length, never the payload.
//!   - T26 `validation_sites_unchanged` — source audit pinning both
//!     messages send sites and both responses-serialization sites.

mod support;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::http::{Response, StatusCode};
use axum::routing::post;
use axum::Router;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use xai_grok_sampler::{
    ApiBackend, RequestId, RetryPolicy, SamplerActor, SamplingClient, SamplingEvent,
};
use xai_grok_sampler::SamplerHandle;
use xai_grok_sampling_types::messages::{Message, MessageContent, MessageRole, MessagesRequest};
use xai_grok_sampling_types::request_builder::{DraftMessageSequence, MessagesRequestBuilder};
use xai_grok_sampling_types::request_validation::{
    RequestValidationError, encode_call_count, reset_encode_call_count,
};
use xai_grok_sampling_types::{
    ContentPart, ConversationItem, ConversationRequest, MessagesRequestWrapper, SamplingError,
    UserItem,
};

/// Minimal wire-legal `/v1/messages` success body.
const SUCCESS_BODY: &str = r#"{"id":"msg_test","type":"message","role":"assistant","content":[],"model":"test-model","stop_reason":"end_turn","usage":{"input_tokens":2,"output_tokens":1}}"#;

/// Minimal wire-legal `/v1/messages` SSE success stream (message_start
/// carries the full MessagesResponse; message_delta carries stop_reason +
/// usage; message_stop is required or the stream counts as truncated).
const SSE_SUCCESS_BODY: &str = r#"data: {"type":"message_start","message":{"id":"msg_test","type":"message","role":"assistant","content":[],"model":"test-model","stop_reason":null,"usage":{"input_tokens":2,"output_tokens":0}}}

data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi there"}}

data: {"type":"content_block_stop","index":0}

data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}

data: {"type":"message_stop"}

"#;

/// The encode counter is process-global and test threads run in parallel,
/// so every counter-sensitive test takes this lock before touching it.
static ENCODE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn start_mock() -> (String, Arc<AtomicU64>) {
    let hits = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&hits);
    let app = Router::new().route(
        "/v1/messages",
        post(move || {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                SUCCESS_BODY
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/v1"), hits)
}

/// 47b RED-2 migration of the 47a struct-literal helper: the test no longer
/// writes `MessagesRequest` fields directly — everything routes through the
/// builder (the only outbound construction path once the fields flip).
fn messages_request(messages: Vec<Message>) -> MessagesRequest {
    let mut sequence = DraftMessageSequence::new();
    for message in messages {
        sequence
            .push_message(message)
            .expect("RED-2 pass-through push never fails");
    }
    MessagesRequestBuilder::new()
        .model("test-model".to_string())
        .max_tokens(64)
        .message_sequence(sequence)
        .build()
        .expect("RED-2 pass-through build never fails")
}

fn messages_config(base_url: &str) -> xai_grok_sampler::SamplerConfig {
    xai_grok_sampler::SamplerConfig {
        api_backend: ApiBackend::Messages,
        ..support::test_config(base_url, "test-key")
    }
}

fn user_text_conv() -> ConversationRequest {
    ConversationRequest {
        items: vec![ConversationItem::User(UserItem {
            content: vec![ContentPart::Text {
                text: Arc::<str>::from("say hi"),
            }],
            ..Default::default()
        })],
        ..Default::default()
    }
}

fn image_conv(image_b64: &str) -> ConversationRequest {
    ConversationRequest {
        items: vec![ConversationItem::User(UserItem {
            content: vec![
                ContentPart::Text {
                    text: Arc::<str>::from("what is in this image"),
                },
                ContentPart::Image {
                    url: Arc::<str>::from(format!("data:image/png;base64,{image_b64}")),
                },
            ],
            ..Default::default()
        })],
        ..Default::default()
    }
}

fn spawn_messages_actor(
    base_url: &str,
) -> (SamplerHandle, mpsc::UnboundedReceiver<SamplingEvent>) {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let handle = SamplerActor::spawn(
        messages_config(base_url),
        RetryPolicy {
            max_retries: 3,
            ..Default::default()
        },
        event_tx,
    );
    (handle, event_rx)
}

/// 47b D-5 mock: the first `/v1/messages` hit returns `first_status`, every
/// later hit returns the SSE success stream. Captures every request body.
async fn start_status_then_sse_mock(
    first_status: u16,
) -> (String, Arc<Mutex<Vec<Vec<u8>>>>, Arc<AtomicU64>) {
    let hits = Arc::new(AtomicU64::new(0));
    let captured: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let handler_hits = Arc::clone(&hits);
    let handler_captured = Arc::clone(&captured);
    let app = Router::new().route(
        "/v1/messages",
        post(move |body: axum::body::Bytes| {
            let hits = Arc::clone(&handler_hits);
            let captured = Arc::clone(&handler_captured);
            async move {
                captured.lock().unwrap().push(body.to_vec());
                let hit = hits.fetch_add(1, Ordering::SeqCst) + 1;
                if hit == 1 {
                    Response::builder()
                        .status(StatusCode::from_u16(first_status).expect("valid status"))
                        .body(axum::body::Body::from("mock failure"))
                        .expect("valid response")
                } else {
                    Response::builder()
                        .status(StatusCode::OK)
                        .header("content-type", "text/event-stream")
                        .body(axum::body::Body::from(SSE_SUCCESS_BODY.to_string()))
                        .expect("valid response")
                }
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/v1"), captured, hits)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn over_cap_request_rejected_before_http() {
    support::pin_env();
    let (base_url, hits) = start_mock().await;
    let client = SamplingClient::new(support::test_config(&base_url, "test-key")).expect("client builds");

    // 100_001 one-char messages: ~2.8 MB if it were ever encoded — but N1
    // must fire first, with zero serialization.
    let one_char = Message {
        role: MessageRole::User,
        content: MessageContent::Text("x".into()),
    };
    let request = MessagesRequestWrapper::new(messages_request(vec![one_char; 100_001]));

    let err = client
        .create_message(request)
        .await
        .expect_err("over-cap request must fail locally");
    assert!(
        matches!(
            err,
            SamplingError::RequestValidation(RequestValidationError::TooManyMessages {
                count: 100_001
            })
        ),
        "got: {err}"
    );
    support::settle_pool().await;
    assert_eq!(hits.load(Ordering::SeqCst), 0, "over-cap request reached the wire");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn under_cap_request_reaches_http() {
    support::pin_env();
    // The successful encode below increments the process-global D-5
    // counter, so take the counter lock like the other counter-sensitive
    // tests (the over-cap sibling rejects at N1 before any encode).
    let _guard = ENCODE_TEST_LOCK.lock().await;
    let (base_url, hits) = start_mock().await;
    let client = SamplingClient::new(support::test_config(&base_url, "test-key")).expect("client builds");

    let short = Message {
        role: MessageRole::User,
        content: MessageContent::Text("hi".into()),
    };
    let request = MessagesRequestWrapper::new(messages_request(vec![short; 2]));

    let response = client
        .create_message(request)
        .await
        .expect("under-cap request must be sent");
    assert_eq!(response.id, "msg_test");
    support::settle_pool().await;
    assert_eq!(hits.load(Ordering::SeqCst), 1, "under-cap request must hit the mock exactly once");
}


/// T20 (47b D-5): a connection reset while uploading a large image payload
/// must surface as the image-strip retry, with exactly one extra encode and
/// the re-sent body byte-identical to a fresh encode of the stripped
/// conversation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upload_reset_retry_reencodes_stripped() {
    support::pin_env();
    let _guard = ENCODE_TEST_LOCK.lock().await;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let captured: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let server_captured = Arc::clone(&captured);
    tokio::spawn(async move {
        let mut conn_index = 0u32;
        loop {
            let (socket, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => break,
            };
            conn_index += 1;
            let conn_no = conn_index;
            let captured = Arc::clone(&server_captured);
            tokio::spawn(async move {
                let mut stream = match TcpStream::from_std(socket.into_std().unwrap()) {
                    Ok(stream) => stream,
                    Err(_) => return,
                };
                let mut header = Vec::new();
                let mut chunk = [0u8; 8192];
                loop {
                    match stream.read(&mut chunk).await {
                        Ok(0) => return,
                        Ok(n) => {
                            header.extend_from_slice(&chunk[..n]);
                            if header.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(_) => return,
                    }
                }
                let lower = String::from_utf8_lossy(&header).to_ascii_lowercase();
                let content_length = lower
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if conn_no == 1 {
                    // Body never read: dropping the socket sends a RST, so
                    // the client sees the reset mid-upload.
                    drop(stream);
                    return;
                }
                // The header loop may have read body bytes past "\r\n\r\n"
                // in the same chunk (the retried body is small enough to
                // arrive together with the headers); carry those bytes over
                // instead of waiting for a second copy that never comes.
                let header_end = header
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|p| p + 4)
                    .unwrap_or(header.len());
                let mut body = header.split_off(header_end);
                while body.len() < content_length {
                    match stream.read(&mut chunk).await {
                        Ok(0) => break,
                        Ok(n) => body.extend_from_slice(&chunk[..n]),
                        Err(_) => return,
                    }
                }
                if body.len() > content_length {
                    body.truncate(content_length);
                }
                captured.lock().unwrap().push(body);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    SSE_SUCCESS_BODY.len(),
                    SSE_SUCCESS_BODY
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });

    let base_url = format!("http://{addr}/v1");
    let client = SamplingClient::new(messages_config(&base_url)).expect("client builds");
    reset_encode_call_count();
    let (handle, _events) = spawn_messages_actor(&base_url);

    // ~4 MB base64 image: N1/N2/N3-legal, large enough that the reset lands
    // while the client is still writing the body.
    let conv = image_conv(&"A".repeat(4_000_000));
    let (response, _stats) = handle
        .submit_and_collect(RequestId::random(), conv.clone())
        .await
        .expect("retry after upload reset must succeed");
    assert!(!response.items.is_empty(), "streamed text must produce items");

    // Attempt 1 encodes; the image-strip retry encodes the stripped
    // conversation exactly once more.
    assert_eq!(
        encode_call_count(),
        2,
        "reset + image-strip retry must encode twice"
    );
    assert_eq!(
        captured.lock().unwrap().len(),
        1,
        "only the retried request reaches the server"
    );

    let mut stripped = conv.clone();
    assert!(!stripped.strip_images().is_empty(), "test conv must carry an image");
    let expected = client
        .encode_conversation_messages(&stripped)
        .expect("stripped conv encodes");
    assert_eq!(
        captured.lock().unwrap()[0].as_slice(),
        expected.as_bytes(),
        "re-sent body must be a fresh encode of the stripped conversation"
    );
    support::settle_pool().await;
}

/// T22 (47b D-5, NIT-2): a retryable 500 must be retried with the exact
/// encoded bytes. In RED-2 the per-attempt re-encode makes the counter
/// assertion the red pin (`== 2` today, `== 1` after the GREEN carrier);
/// the byte-equality pin holds in both phases.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_backoff_reuses_exact_encoded_bytes() {
    support::pin_env();
    let _guard = ENCODE_TEST_LOCK.lock().await;
    let (base_url, bodies, hits) = start_status_then_sse_mock(500).await;
    reset_encode_call_count();
    let (handle, _events) = spawn_messages_actor(&base_url);

    let (response, _stats) = handle
        .submit_and_collect(RequestId::random(), user_text_conv())
        .await
        .expect("backoff retry after 500 must succeed");
    assert!(!response.items.is_empty());

    assert_eq!(hits.load(Ordering::SeqCst), 2, "500 then success");
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert_eq!(
        bodies[0].as_slice(),
        bodies[1].as_slice(),
        "retry must reuse the exact encoded bytes (NIT-2)"
    );
    assert_eq!(
        encode_call_count(),
        1,
        "GREEN carrier reuses the encoded bytes on backoff retry (RED-2 re-encodes per attempt)"
    );
    support::settle_pool().await;
}

/// T23 (47b D-5): a 413 must trip the image-strip retry; the second body
/// must equal a fresh encode of the stripped conversation, with two encodes
/// total (GREEN pins two-not-three).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn image_strip_retry_revalidates_and_reencodes() {
    support::pin_env();
    let _guard = ENCODE_TEST_LOCK.lock().await;
    let (base_url, bodies, hits) = start_status_then_sse_mock(413).await;
    let client = SamplingClient::new(messages_config(&base_url)).expect("client builds");
    reset_encode_call_count();
    let (handle, _events) = spawn_messages_actor(&base_url);

    let one_px_png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
    let conv = image_conv(one_px_png);
    let (response, _stats) = handle
        .submit_and_collect(RequestId::random(), conv.clone())
        .await
        .expect("image-strip retry after 413 must succeed");
    assert!(!response.items.is_empty());

    assert_eq!(hits.load(Ordering::SeqCst), 2, "413 then success");
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert_ne!(
        bodies[0].as_slice(),
        bodies[1].as_slice(),
        "the stripped retry must change the body"
    );
    // Count before the test's own verification encode below: that call
    // increments the process-global D-5 counter as well.
    assert_eq!(
        encode_call_count(),
        2,
        "original encode + stripped re-encode (never a third)"
    );

    let mut stripped = conv.clone();
    assert!(!stripped.strip_images().is_empty());
    let expected = client
        .encode_conversation_messages(&stripped)
        .expect("stripped conv encodes");
    assert_eq!(
        bodies[1].as_slice(),
        expected.as_bytes(),
        "second body must be a fresh encode of the stripped conversation"
    );
    support::settle_pool().await;
}

/// T25 (47b D-5): the cached carrier's Debug renders only the byte length —
/// neither the payload text nor the model name ever appears.
#[tokio::test]
async fn cached_carrier_debug_no_body_leak() {
    let _guard = ENCODE_TEST_LOCK.lock().await;
    let mut sequence = DraftMessageSequence::new();
    sequence
        .push_message(Message {
            role: MessageRole::User,
            content: MessageContent::Text("reqvalid-leak-sentinel-payload".into()),
        })
        .expect("push");
    let request = MessagesRequestBuilder::new()
        .model("reqvalid-leak-model".to_string())
        .max_tokens(64)
        .message_sequence(sequence)
        .build()
        .expect("build");
    let encoded = xai_grok_sampling_types::request_builder::DraftMessagesRequest::new(request)
        .validate()
        .expect("valid")
        .encode()
        .expect("encode");
    let debug = format!("{encoded:?}");
    assert!(
        debug.contains(&format!("bytes: {}", encoded.len())),
        "Debug must show the byte count, got: {debug}"
    );
    assert!(
        !debug.contains("reqvalid-leak-sentinel-payload"),
        "Debug leaks the payload: {debug}"
    );
    assert!(!debug.contains("reqvalid-leak-model"), "Debug leaks the model: {debug}");
}

/// T26 (47b D-5): both messages send sites keep the pre-HTTP validation
/// call and both responses-serialization sites are structurally intact
/// (whitespace collapsed — the calls wrap across lines in source).
#[test]
fn validation_sites_unchanged() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/client.rs");
    let source = std::fs::read_to_string(path).expect("client.rs readable");
    // Collapse whitespace on both sides of each match: the source wraps the
    // calls over multiple lines with a trailing comma, and the
    // serialization-error pattern is a string literal whose internal spaces
    // would be erased by collapsing only the source side.
    let collapse = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let collapsed_source = collapse(&source);
    let count = |pattern: &str| collapsed_source.matches(&collapse(pattern)).count();
    assert!(
        count("validate_and_encode_messages_request(&request.inner,)") >= 2,
        "both messages send sites must keep the pre-HTTP V1 validation call"
    );
    assert_eq!(
        count("Failed to serialize responses request"),
        2,
        "responses-serialization sites must be structurally intact"
    );
    assert!(
        count("splice_extra_tool_entries(&mut") >= 2,
        "responses tool splice must remain at both sites"
    );
}
