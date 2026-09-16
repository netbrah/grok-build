//! REQVALID-1 47a, T16: over-cap Messages requests are rejected locally
//! BEFORE transport — the wire never sees their bytes. The under-cap
//! control proves the mock endpoint is live, so the zero-hit assertion
//! below is not vacuous.

mod support;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::Router;
use axum::routing::post;
use tokio::net::TcpListener;
use xai_grok_sampler::SamplingClient;
use xai_grok_sampling_types::messages::{Message, MessageContent, MessageRole, MessagesRequest};
use xai_grok_sampling_types::request_validation::RequestValidationError;
use xai_grok_sampling_types::{MessagesRequestWrapper, SamplingError};

/// Minimal wire-legal `/v1/messages` success body.
const SUCCESS_BODY: &str = r#"{"id":"msg_test","type":"message","role":"assistant","content":[],"model":"test-model","stop_reason":"end_turn","usage":{"input_tokens":2,"output_tokens":1}}"#;

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

fn messages_request(messages: Vec<Message>) -> MessagesRequest {
    MessagesRequest {
        model: "test-model".to_string(),
        messages,
        max_tokens: 64,
        ..Default::default()
    }
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
