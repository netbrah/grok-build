// End-to-end wire tests for the native Gemini `streamGenerateContent` send
// path: route shape, auth header, request projection (opaque-reasoning
// replay) and SSE chunk parsing, against an in-process axum mock.
// Included from `client.rs` (`mod generate_content_tests`).

use super::*;
use axum::http::{HeaderMap, Uri};
use tokio::sync::mpsc;
use xai_grok_sampling_types::conversation::{ToolExposure, ToolSpec};
use xai_grok_sampling_types::gemini::{FinishReason, GenerateContentResponse};
use xai_grok_sampling_types::{ConversationItem, ReasoningEffort, ToolCall};

const GEMINI_MODEL: &str = "gemini-3-pro-preview";

#[derive(Debug)]
struct Captured {
    uri: Uri,
    headers: HeaderMap,
    body: serde_json::Value,
}

/// Two SSE frames the way the service streams them with `alt=sse`: a thought
/// summary, then a signed `functionCall` closing the candidate.
const SSE_BODY: &str = concat!(
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"Let me look that up.\",\"thought\":true}]},\"index\":0}],\"responseId\":\"resp-1\",\"modelVersion\":\"gemini-3-pro-preview\"}\r\n\r\n",
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"functionCall\":{\"name\":\"lookup\",\"args\":{\"q\":\"x\"}},\"thoughtSignature\":\"SIG_NEW\"}]},\"finishReason\":\"STOP\",\"index\":0}],\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":5,\"totalTokenCount\":20,\"thoughtsTokenCount\":5},\"responseId\":\"resp-1\",\"modelVersion\":\"gemini-3-pro-preview\"}\r\n\r\n",
);

fn lookup_tool() -> ToolSpec {
    ToolSpec {
        name: "lookup".into(),
        description: Some("Look something up".into()),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "q": { "type": "string" } },
            "required": ["q"]
        }),
        exposure: ToolExposure::default(),
    }
}

fn signed_reasoning(summary: &str, signature: &str) -> ConversationItem {
    ConversationItem::Reasoning(
        rs::ReasoningItem {
            id: Some(String::new()),
            summary: vec![rs::SummaryPart::SummaryText(rs::SummaryTextContent {
                text: summary.to_string(),
            })],
            content: None,
            encrypted_content: Some(signature.to_string()),
            status: None,
        }
        .into(),
    )
}

/// A prior Gemini step (thought + signed call + result) followed by a new
/// user turn, so the projection has a signature to replay.
fn history() -> Vec<ConversationItem> {
    vec![
        ConversationItem::system("Be terse."),
        ConversationItem::user("go"),
        signed_reasoning("earlier thought", "SIG_OLD"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_1".into(),
            name: "lookup".into(),
            arguments: r#"{"q":"a"}"#.into(),
        }])
        .with_model_id(GEMINI_MODEL),
        ConversationItem::tool_result("call_1", "result-a"),
        ConversationItem::user("and now?"),
    ]
}

/// Serve `SSE_BODY` on `{route}`, capturing the first request. Returns the
/// bound address, the capture receiver and the server task.
async fn spawn_mock(
    route: &'static str,
) -> (
    std::net::SocketAddr,
    mpsc::UnboundedReceiver<Captured>,
    tokio::task::JoinHandle<()>,
) {
    spawn_mock_with(route, 200, "text/event-stream", SSE_BODY).await
}

/// [`spawn_mock`] with a caller-chosen status, `content-type` and body: the
/// proxy shapes (JSON error relayed as 200, `alt=sse` dropped, Google 429
/// envelope) that the SSE happy path never produces.
async fn spawn_mock_with(
    route: &'static str,
    status: u16,
    content_type: &'static str,
    body: &str,
) -> (
    std::net::SocketAddr,
    mpsc::UnboundedReceiver<Captured>,
    tokio::task::JoinHandle<()>,
) {
    spawn_mock_sequence(route, vec![(status, content_type, body.to_owned())]).await
}

async fn spawn_mock_sequence(
    route: &'static str,
    responses: Vec<(u16, &'static str, String)>,
) -> (
    std::net::SocketAddr,
    mpsc::UnboundedReceiver<Captured>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let responses = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from(
        responses,
    )));
    let app = Router::new().route(
        route,
        post(move |uri: Uri, headers: HeaderMap, body_bytes: Bytes| {
            let tx = tx.clone();
            let responses = responses.clone();
            async move {
                let parsed = serde_json::from_slice(&body_bytes).expect("request body is JSON");
                tx.send(Captured {
                    uri,
                    headers,
                    body: parsed,
                })
                .expect("capture receiver remains open");
                let (status, content_type, body) = responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("unexpected extra HTTP request");
                axum::response::Response::builder()
                    .status(status)
                    .header("content-type", content_type)
                    .body(axum::body::Body::from(body))
                    .unwrap()
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, rx, server)
}

fn gemini_client(base_url: String, model: &str) -> SamplingClient {
    SamplingClient::new(SamplerConfig {
        base_url,
        model: model.to_string(),
        api_backend: ApiBackend::GenerateContent,
        auth_scheme: AuthScheme::XGoogApiKey,
        ..minimal_config()
    })
    .unwrap()
}

async fn drain(
    mut stream: BoxStream<'static, Result<GenerateContentResponse>>,
) -> Vec<GenerateContentResponse> {
    let mut chunks = Vec::new();
    while let Some(item) = stream.next().await {
        chunks.push(item.expect("chunk parses"));
    }
    chunks
}

#[tokio::test]
#[serial]
async fn generate_content_stream_routes_authenticates_projects_and_parses() {
    let (addr, mut rx, server) =
        spawn_mock("/v1beta/models/gemini-3-pro-preview:streamGenerateContent").await;
    let client = gemini_client(format!("http://{addr}/v1beta"), GEMINI_MODEL);

    let request = ConversationRequest {
        items: history(),
        tools: vec![lookup_tool()],
        reasoning_effort: Some(ReasoningEffort::Low),
        x_grok_req_id: Some("req-gemini-1".into()),
        ..Default::default()
    };
    let (stream, _metadata) = client
        .conversation_stream_generate_content(request)
        .await
        .expect("stream request succeeds");
    let chunks = drain(stream).await;
    let captured = rx.recv().await.expect("mock captured the request");
    server.abort();

    // Route: the model rides the path, `alt=sse` selects SSE framing.
    assert_eq!(
        captured.uri.path(),
        "/v1beta/models/gemini-3-pro-preview:streamGenerateContent"
    );
    assert_eq!(captured.uri.query(), Some("alt=sse"));

    // Auth: Google's key header, and none of the other wires' credentials.
    let header = |name: &str| {
        captured
            .headers
            .get(name)
            .map(|v| v.to_str().unwrap().to_owned())
    };
    assert_eq!(header("x-goog-api-key").as_deref(), Some("test-key"));
    assert!(
        header("authorization").is_none(),
        "no bearer on the Gemini wire"
    );
    assert!(header("x-api-key").is_none(), "no Anthropic key header");
    assert!(header("anthropic-version").is_none());
    assert_eq!(header("accept").as_deref(), Some("text/event-stream"));
    assert_eq!(header("content-type").as_deref(), Some("application/json"));
    assert_eq!(header("x-grok-req-id").as_deref(), Some("req-gemini-1"));

    // Body: the projection, with the prior step's opaque reasoning replayed.
    let body = &captured.body;
    assert!(
        body.get("model").is_none(),
        "model rides the URL, never the body"
    );
    assert_eq!(body["systemInstruction"]["parts"][0]["text"], "Be terse.");
    let contents = body["contents"].as_array().unwrap();
    assert_eq!(
        contents.len(),
        3,
        "user / model / user (functionResponse merged with the next user text)"
    );
    assert_eq!(contents[1]["role"], "model");
    let model_parts = contents[1]["parts"].as_array().unwrap();
    assert_eq!(
        model_parts.len(),
        1,
        "signature-only replay: the thought summary is not sent back"
    );
    assert_eq!(model_parts[0]["functionCall"]["name"], "lookup");
    assert_eq!(
        model_parts[0]["thoughtSignature"], "SIG_OLD",
        "the stored thoughtSignature rides the functionCall it was issued on"
    );
    assert_eq!(contents[2]["role"], "user");
    let user_parts = contents[2]["parts"].as_array().unwrap();
    assert_eq!(
        user_parts[0]["functionResponse"]["name"], "lookup",
        "functionResponse answers by name"
    );
    assert_eq!(
        user_parts[0]["functionResponse"]["response"],
        serde_json::json!({ "output": "result-a" })
    );
    assert_eq!(user_parts[1]["text"], "and now?");
    assert_eq!(
        body["tools"][0]["functionDeclarations"][0]["name"],
        "lookup"
    );
    let generation = &body["generationConfig"];
    assert_eq!(generation["thinkingConfig"]["includeThoughts"], true);
    assert_eq!(generation["thinkingConfig"]["thinkingLevel"], "LOW");
    assert!(
        generation.get("maxOutputTokens").is_none(),
        "no Responses-shaped token floor on the Gemini wire: omitted means the model ceiling"
    );

    // Chunks: one complete `GenerateContentResponse` per `data:` frame.
    assert_eq!(chunks.len(), 2);
    let first = chunks[0].candidates[0].content.as_ref().unwrap();
    assert_eq!(first.parts[0].thought, Some(true));
    assert_eq!(first.parts[0].text.as_deref(), Some("Let me look that up."));
    let last = &chunks[1].candidates[0];
    assert_eq!(last.finish_reason, Some(FinishReason::Stop));
    let call_part = &last.content.as_ref().unwrap().parts[0];
    assert_eq!(call_part.function_call.as_ref().unwrap().name, "lookup");
    assert_eq!(
        call_part.thought_signature.as_deref(),
        Some("SIG_NEW"),
        "the new signature survives parsing so the next turn can replay it"
    );
    let usage = chunks[1].usage_metadata.as_ref().unwrap();
    assert_eq!(
        (usage.prompt_token_count, usage.thoughts_token_count),
        (10, 5)
    );
}

#[tokio::test]
#[serial]
async fn generate_content_stream_appends_resource_path_models_verbatim() {
    // A Vertex-style resource path is already fully qualified: no
    // `models/` prefix is inserted ahead of it.
    let (addr, mut rx, server) = spawn_mock(
        "/v1/projects/p/locations/us-central1/publishers/google/models/gemini-3-pro-preview:streamGenerateContent",
    )
    .await;
    let client = gemini_client(
        format!("http://{addr}/v1"),
        "projects/p/locations/us-central1/publishers/google/models/gemini-3-pro-preview",
    );
    let (stream, _) = client
        .conversation_stream_generate_content(ConversationRequest {
            items: vec![ConversationItem::user("hi")],
            ..Default::default()
        })
        .await
        .expect("stream request succeeds");
    let chunks = drain(stream).await;
    let captured = rx.recv().await.expect("mock captured the request");
    server.abort();

    assert_eq!(
        captured.uri.path(),
        "/v1/projects/p/locations/us-central1/publishers/google/models/gemini-3-pro-preview:streamGenerateContent"
    );
    assert_eq!(captured.uri.query(), Some("alt=sse"));
    assert_eq!(captured.body["contents"][0]["parts"][0]["text"], "hi");
    assert_eq!(chunks.len(), 2);
}

const GEMINI_ROUTE: &str = "/v1beta/models/gemini-3-pro-preview:streamGenerateContent";

fn plain_request() -> ConversationRequest {
    ConversationRequest {
        items: vec![ConversationItem::user("hi")],
        ..Default::default()
    }
}

fn sse_frame(value: serde_json::Value) -> String {
    format!("data: {value}\n\n")
}

#[tokio::test]
#[serial]
async fn generate_content_continuation_reuses_the_request_and_accumulates_hop_usage() {
    let first = [
        sse_frame(serde_json::json!({
            "candidates": [{"content": {"role": "model", "parts": [{"text": "Hello"}]}}],
            "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 1, "totalTokenCount": 11}
        })),
        sse_frame(serde_json::json!({
            "candidates": [{
                "finishReason": "CONTINUATION",
                "finishMessage": "per-request limit",
                "continuationToken": "dG9rZW4tMQ=="
            }],
            "usageMetadata": {
                "promptTokenCount": 10, "candidatesTokenCount": 2, "thoughtsTokenCount": 3,
                "totalTokenCount": 15, "cachedContentTokenCount": 2
            }
        })),
    ].concat();
    let second = [
        sse_frame(serde_json::json!({
            "candidates": [{"content": {"role": "model", "parts": [{"text": " world!"}]}}],
            "usageMetadata": {
                "promptTokenCount": 4, "candidatesTokenCount": 1, "thoughtsTokenCount": 1,
                "totalTokenCount": 6
            }
        })),
        sse_frame(serde_json::json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "", "thoughtSignature": "SIG_FINAL"}]},
                "finishReason": "STOP"
            }]
        })),
        sse_frame(serde_json::json!({
            "usageMetadata": {
                "promptTokenCount": 4, "candidatesTokenCount": 3, "thoughtsTokenCount": 2,
                "totalTokenCount": 9, "cachedContentTokenCount": 1
            }
        })),
    ].concat();
    let (addr, mut rx, server) = spawn_mock_sequence(
        GEMINI_ROUTE,
        vec![
            (200, "text/event-stream", first),
            (200, "text/event-stream", second),
        ],
    )
    .await;
    let client = gemini_client(format!("http://{addr}/v1beta"), GEMINI_MODEL);
    let response = client
        .conversation_collect(ConversationRequest {
            items: history(),
            tools: vec![lookup_tool()],
            max_output_tokens: Some(128),
            reasoning_effort: Some(ReasoningEffort::Low),
            ..Default::default()
        })
        .await
        .unwrap();
    let first_request = rx.recv().await.unwrap();
    let mut second_request = rx.recv().await.unwrap();
    assert!(rx.try_recv().is_err());
    server.abort();

    assert_eq!(first_request.uri, second_request.uri);
    assert_eq!(
        first_request.headers["x-goog-api-key"],
        second_request.headers["x-goog-api-key"]
    );
    assert!(first_request.body.get("continuationToken").is_none());
    assert_eq!(
        second_request
            .body
            .as_object_mut()
            .unwrap()
            .remove("continuationToken"),
        Some(serde_json::json!("dG9rZW4tMQ=="))
    );
    assert_eq!(
        first_request.body, second_request.body,
        "only the opaque token changes"
    );
    assert_eq!(
        first_request.body["generationConfig"]["maxOutputTokens"],
        128
    );
    assert_eq!(
        response.assistant().unwrap().content.as_ref(),
        "Hello world!"
    );
    assert_eq!(response.raw_stop_reason.as_deref(), Some("STOP"));
    assert!(
        response.stop_message.is_none(),
        "intermediate stop details do not leak"
    );
    let reasoning = response.items.iter().find_map(|item| match item {
        ConversationItem::Reasoning(r) => r.encrypted_content.as_deref(),
        _ => None,
    });
    assert_eq!(reasoning, Some("SIG_FINAL"));
    let usage = response.usage.unwrap();
    assert_eq!(
        (
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.total_tokens
        ),
        (14, 10, 24)
    );
    assert_eq!((usage.reasoning_tokens, usage.cached_prompt_tokens), (5, 3));
}

#[tokio::test]
#[serial]
async fn generate_content_continuation_also_handles_json_array_and_object_hops() {
    let first = serde_json::json!([{
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": "first"}]},
            "finishReason": "CONTINUATION",
            "continuationToken": "dG9rZW4tMQ=="
        }]
    }]);
    let second = serde_json::json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": "second"}]},
            "finishReason": "STOP"
        }]
    });
    let (addr, mut rx, server) = spawn_mock_sequence(
        GEMINI_ROUTE,
        vec![
            (200, "application/json", first.to_string()),
            (200, "application/json", second.to_string()),
        ],
    )
    .await;
    let client = gemini_client(format!("http://{addr}/v1beta"), GEMINI_MODEL);
    let response = client.conversation_collect(plain_request()).await.unwrap();
    assert_eq!(
        response.assistant().unwrap().content.as_ref(),
        "firstsecond"
    );
    assert_eq!(response.raw_stop_reason.as_deref(), Some("STOP"));
    rx.recv().await.unwrap();
    assert_eq!(
        rx.recv().await.unwrap().body["continuationToken"],
        "dG9rZW4tMQ=="
    );
    assert!(rx.try_recv().is_err());
    server.abort();
}

#[tokio::test]
#[serial]
async fn dropping_a_generate_content_stream_does_not_start_a_continuation() {
    let response = sse_frame(serde_json::json!({
        "candidates": [{"finishReason": "CONTINUATION", "continuationToken": "dG9rZW4tMQ=="}]
    }));
    let (addr, mut rx, server) =
        spawn_mock_with(GEMINI_ROUTE, 200, "text/event-stream", &response).await;
    let client = gemini_client(format!("http://{addr}/v1beta"), GEMINI_MODEL);
    let (mut stream, _) = client
        .conversation_stream_generate_content(plain_request())
        .await
        .unwrap();
    assert!(stream.next().await.unwrap().is_ok());
    drop(stream);
    rx.recv().await.unwrap();
    assert!(rx.try_recv().is_err());
    server.abort();
}

#[tokio::test]
#[serial]
async fn generate_content_raw_stream_rejects_long_partial_summaries_without_a_terminal() {
    for content_type in ["text/event-stream", "application/json"] {
        for finish in [None, Some("FINISH_REASON_UNSPECIFIED")] {
            let value = serde_json::json!({
                "candidates": [{
                    "content": {"role": "model", "parts": [{"text": "x".repeat(600)}]},
                    "finishReason": finish
                }]
            });
            let body = if content_type == "text/event-stream" {
                sse_frame(value)
            } else {
                value.to_string()
            };
            let (addr, mut rx, server) =
                spawn_mock_with(GEMINI_ROUTE, 200, content_type, &body).await;
            let client = gemini_client(format!("http://{addr}/v1beta"), GEMINI_MODEL);
            let (stream, _) = client
                .conversation_stream_generate_content(plain_request())
                .await
                .unwrap();
            let chunks: Vec<_> = stream.collect().await;
            rx.recv().await.unwrap();
            server.abort();
            assert_eq!(chunks.len(), 2);
            assert_eq!(
                chunks[0].as_ref().unwrap().candidates[0]
                    .content
                    .as_ref()
                    .unwrap()
                    .parts[0]
                    .text
                    .as_ref()
                    .unwrap()
                    .len(),
                600
            );
            assert!(matches!(
                &chunks[1],
                Err(SamplingError::StreamError { error_type, .. }) if error_type == "stream_truncated"
            ));
        }
    }
}

#[tokio::test]
#[serial]
async fn generate_content_raw_stream_rejects_a_malformed_terminal() {
    let response = sse_frame(serde_json::json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": "x".repeat(600)}]},
            "finishReason": "MALFORMED_RESPONSE",
            "finishMessage": "invalid output"
        }]
    }));
    let (addr, mut rx, server) =
        spawn_mock_with(GEMINI_ROUTE, 200, "text/event-stream", &response).await;
    let client = gemini_client(format!("http://{addr}/v1beta"), GEMINI_MODEL);
    let (stream, _) = client
        .conversation_stream_generate_content(plain_request())
        .await
        .unwrap();
    let chunks: Vec<_> = stream.collect().await;
    rx.recv().await.unwrap();
    server.abort();
    assert!(matches!(
        chunks.last(),
        Some(Err(SamplingError::StreamError { error_type, message, .. }))
            if error_type == "malformed_model_output" && message.contains("invalid output")
    ));
}

#[tokio::test]
#[serial]
async fn generate_content_continuation_requires_a_new_terminal_token() {
    let continuation = |token: Option<&str>| {
        sse_frame(serde_json::json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "partial"}]},
                "finishReason": "CONTINUATION",
                "continuationToken": token
            }]
        }))
    };
    let checkpoint_only = sse_frame(serde_json::json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": "partial"}]},
            "continuationToken": "dG9rZW4tMQ=="
        }]
    }));
    for (bodies, diagnostic) in [
        (vec![continuation(None)], "did not include"),
        (vec![continuation(Some(""))], "did not include"),
        (
            vec![
                continuation(Some("dG9rZW4tMQ==")),
                continuation(Some("dG9rZW4tMQ==")),
            ],
            "repeated",
        ),
        (vec![checkpoint_only], "without a finishReason"),
    ] {
        let expected_requests = bodies.len();
        let (addr, mut rx, server) = spawn_mock_sequence(
            GEMINI_ROUTE,
            bodies
                .into_iter()
                .map(|body| (200, "text/event-stream", body))
                .collect(),
        )
        .await;
        let client = gemini_client(format!("http://{addr}/v1beta"), GEMINI_MODEL);
        let error = client
            .conversation_collect(plain_request())
            .await
            .expect_err("incomplete generation");
        assert!(error.to_string().contains(diagnostic), "{error}");
        for _ in 0..expected_requests {
            rx.recv().await.unwrap();
        }
        assert!(
            rx.try_recv().is_err(),
            "invalid tokens must not start another request"
        );
        server.abort();
    }
}

#[tokio::test]
#[serial]
async fn generate_content_continuation_propagates_a_later_hop_api_error() {
    let first = sse_frame(serde_json::json!({
        "candidates": [{"finishReason": "CONTINUATION", "continuationToken": "dG9rZW4tMQ=="}]
    }));
    let (addr, mut rx, server) = spawn_mock_sequence(
        GEMINI_ROUTE,
        vec![
            (200, "text/event-stream", first),
            (429, "application/json", r#"{"error":{"code":429,"status":"RESOURCE_EXHAUSTED","message":"quota limit","details":[{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"7s"}]}}"#.into()),
        ],
    ).await;
    let client = gemini_client(format!("http://{addr}/v1beta"), GEMINI_MODEL);
    let (mut stream, _) = client
        .conversation_stream_generate_content(plain_request())
        .await
        .unwrap();
    assert!(stream.next().await.unwrap().is_ok());
    let error = stream.next().await.unwrap().unwrap_err();
    assert!(matches!(
        error,
        SamplingError::Api { status, retry_after_secs: Some(7), .. }
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS
    ));
    assert!(stream.next().await.is_none());
    rx.recv().await.unwrap();
    rx.recv().await.unwrap();
    server.abort();
}

#[tokio::test]
#[serial]
async fn generate_content_final_reasons_and_prompt_blocks_do_not_resume_checkpoints() {
    let responses = [
        serde_json::json!({"candidates": [{"finishReason": "STOP", "continuationToken": "checkpoint"}]}),
        serde_json::json!({"candidates": [{"finishReason": "MAX_TOKENS", "continuationToken": "checkpoint"}]}),
        serde_json::json!({"candidates": [{"finishReason": "SAFETY", "continuationToken": "checkpoint"}]}),
        serde_json::json!({"promptFeedback": {"blockReason": "PROHIBITED_CONTENT"}}),
    ];
    for response in responses {
        let (addr, mut rx, server) =
            spawn_mock_with(GEMINI_ROUTE, 200, "application/json", &response.to_string()).await;
        let client = gemini_client(format!("http://{addr}/v1beta"), GEMINI_MODEL);
        let (stream, _) = client
            .conversation_stream_generate_content(plain_request())
            .await
            .unwrap();
        assert_eq!(drain(stream).await.len(), 1);
        rx.recv().await.unwrap();
        assert!(rx.try_recv().is_err());
        server.abort();
    }
}

#[tokio::test]
#[serial]
async fn generate_content_collect_preserves_requested_alias_and_native_call_id() {
    const RESPONSE: &str = concat!(
        "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[",
        "{\"functionCall\":{\"id\":\"native/a\",\"name\":\"lookup\",\"args\":{}},",
        "\"thoughtSignature\":\"SIG_ALIAS\"}]},\"finishReason\":\"STOP\"}],",
        "\"modelVersion\":\"gemini-3.5-flash\"}\n\n",
    );
    for (configured, requested, expected) in [
        ("gemini-flash-latest", None, "gemini-flash-latest"),
        ("unused-default", Some("prod-coder"), "prod-coder"),
    ] {
        let (addr, mut rx, server) = spawn_mock_with(
            "/v1beta/models/{operation}",
            200,
            "text/event-stream",
            RESPONSE,
        )
        .await;
        let client = gemini_client(format!("http://{addr}/v1beta"), configured);
        let mut request = plain_request();
        request.model = requested.map(str::to_owned);
        let response = client.conversation_collect(request).await.unwrap();
        let captured = rx.recv().await.unwrap();
        server.abort();
        assert_eq!(
            captured.uri.path(),
            format!("/v1beta/models/{expected}:streamGenerateContent")
        );
        assert_eq!(
            response.assistant().unwrap().model_id.as_deref(),
            Some(expected)
        );

        let mut items = vec![ConversationItem::user("hi")];
        items.extend(response.items);
        items.push(ConversationItem::tool_result("native/a", "found"));
        let replay = build_generate_content_request(
            &ConversationRequest::from_items(items).with_model(expected),
        );
        let call = &replay.contents[1].parts[0];
        assert_eq!(call.thought_signature.as_deref(), Some("SIG_ALIAS"));
        assert_eq!(
            call.function_call.as_ref().unwrap().id.as_deref(),
            Some("native/a")
        );
        assert_eq!(
            replay.contents[2].parts[0]
                .function_response
                .as_ref()
                .unwrap()
                .id
                .as_deref(),
            Some("native/a")
        );
    }
}

#[tokio::test]
#[serial]
async fn generate_content_json_error_relayed_as_200_surfaces_as_a_stream_error() {
    // LiteLLM relays a guardrail block as HTTP 200 with an OpenAI-shaped
    // error envelope. Fed to the SSE decoder it has no `data:` line and
    // the stream used to end as a silent truncation.
    let (addr, mut rx, server) = spawn_mock_with(
        GEMINI_ROUTE,
        200,
        "application/json",
        r#"{"error":{"message":"Violated guardrail policy","type":"content_filter","guardrail_name":"pii","model":"gemini-3-pro-preview"}}"#,
    )
    .await;
    let client = gemini_client(format!("http://{addr}/v1beta"), GEMINI_MODEL);
    let (mut stream, _) = client
        .conversation_stream_generate_content(plain_request())
        .await
        .expect("a 200 opens the stream");
    let first = stream.next().await.expect("one item");
    assert!(stream.next().await.is_none(), "the error ends the stream");
    rx.recv().await.expect("mock captured the request");
    server.abort();

    match first {
        Err(SamplingError::StreamError {
            error_type,
            message,
            ..
        }) => {
            assert_eq!(error_type, "content_filter");
            assert_eq!(message, "Violated guardrail policy");
        }
        other => panic!("expected a StreamError, got {other:?}"),
    }
}

#[tokio::test]
#[serial]
async fn generate_content_json_array_body_is_decoded_as_chunks() {
    // A proxy that drops `alt=sse` answers with the unary JSON array form of
    // `streamGenerateContent`; the chunks are the same objects.
    let (addr, mut rx, server) = spawn_mock_with(
        GEMINI_ROUTE,
        200,
        "application/json; charset=UTF-8",
        concat!(
            "[{\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"Hel\"}]},\"index\":0}],\"modelVersion\":\"gemini-3-pro-preview\"},",
            "{\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"lo\"}]},\"finishReason\":\"STOP\",\"index\":0}],\"usageMetadata\":{\"promptTokenCount\":3,\"candidatesTokenCount\":2,\"totalTokenCount\":5}}]",
        ),
    )
    .await;
    let client = gemini_client(format!("http://{addr}/v1beta"), GEMINI_MODEL);
    let (stream, _) = client
        .conversation_stream_generate_content(plain_request())
        .await
        .expect("stream request succeeds");
    let chunks = drain(stream).await;
    rx.recv().await.expect("mock captured the request");
    server.abort();

    assert_eq!(chunks.len(), 2);
    let text: String = chunks
        .iter()
        .flat_map(|c| c.candidates[0].content.as_ref().unwrap().parts.iter())
        .filter_map(|p| p.text.clone())
        .collect();
    assert_eq!(text, "Hello");
    assert_eq!(
        chunks[1].candidates[0].finish_reason,
        Some(FinishReason::Stop)
    );
    assert_eq!(
        chunks[1]
            .usage_metadata
            .as_ref()
            .unwrap()
            .prompt_token_count,
        3
    );
}

#[tokio::test]
#[serial]
async fn generate_content_json_diagnostics_cannot_become_successful_chunks() {
    for (body, diagnostic) in [
        (
            concat!(
                "[{\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"partial\"}]},\"finishReason\":\"STOP\"}]},",
                "{\"error\":{\"code\":429,\"status\":\"RESOURCE_EXHAUSTED\",\"message\":\"quota limit\"}}]",
            ),
            "quota limit",
        ),
        (
            r#"{"detail":"deployment is not available"}"#,
            "deployment is not available",
        ),
    ] {
        let (addr, mut rx, server) =
            spawn_mock_with(GEMINI_ROUTE, 200, "application/json", body).await;
        let client = gemini_client(format!("http://{addr}/v1beta"), GEMINI_MODEL);
        let error = client
            .conversation_collect(plain_request())
            .await
            .expect_err("an explicit error must not become a completed or empty response");
        rx.recv().await.unwrap();
        server.abort();
        assert!(error.to_string().contains(diagnostic), "{error}");
    }
}

#[test]
fn generate_content_json_metadata_and_future_fields_remain_valid() {
    let chunks = decode_generate_content_json_body(
        br#"[{"modelVersion":"gemini-3.5-flash"},{"usageMetadata":{"promptTokenCount":3}},{"futureMetadata":{"detail":"not an error"}}]"#,
    );
    assert_eq!(chunks.len(), 3);
    assert!(chunks.iter().all(Result::is_ok));
    assert_eq!(
        chunks[1]
            .as_ref()
            .unwrap()
            .usage_metadata
            .as_ref()
            .unwrap()
            .prompt_token_count,
        3
    );
}

#[tokio::test]
#[serial]
async fn generate_content_429_envelope_carries_status_and_retry_info_delay() {
    // Google's 429 names the RPC status and advises the delay inside the
    // envelope (`google.rpc.RetryInfo`), without a `Retry-After` header.
    let (addr, mut rx, server) = spawn_mock_with(
        GEMINI_ROUTE,
        429,
        "application/json; charset=UTF-8",
        r#"{"error":{"code":429,"message":"You exceeded your current quota.","status":"RESOURCE_EXHAUSTED","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"RATE_LIMIT_EXCEEDED","domain":"googleapis.com"},{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"7s"}]}}"#,
    )
    .await;
    let client = gemini_client(format!("http://{addr}/v1beta"), GEMINI_MODEL);
    let err = client
        .conversation_stream_generate_content(plain_request())
        .await
        .err()
        .expect("a 429 fails the request");
    rx.recv().await.expect("mock captured the request");
    server.abort();

    match err {
        SamplingError::Api {
            status,
            message,
            retry_after_secs,
            ..
        } => {
            assert_eq!(status, reqwest::StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(
                message,
                "RESOURCE_EXHAUSTED: You exceeded your current quota."
            );
            assert_eq!(
                retry_after_secs,
                Some(7),
                "RetryInfo stands in for Retry-After"
            );
        }
        other => panic!("expected an Api error, got {other:?}"),
    }
}
