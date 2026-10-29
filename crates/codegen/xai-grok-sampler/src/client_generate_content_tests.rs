// End-to-end wire tests for the native Gemini `streamGenerateContent` send
// path: route shape, auth header, request projection (opaque-reasoning
// replay) and SSE chunk parsing, against an in-process axum mock.
// Included from `client.rs` (`mod generate_content_tests`).

use super::*;
use axum::http::{HeaderMap, Uri};
use xai_grok_sampling_types::conversation::{ToolExposure, ToolSpec};
use xai_grok_sampling_types::gemini::{FinishReason, GenerateContentResponse};
use xai_grok_sampling_types::{ConversationItem, ReasoningEffort, ToolCall};

const GEMINI_MODEL: &str = "gemini-3-pro-preview";

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
    oneshot::Receiver<Captured>,
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
    body: &'static str,
) -> (
    std::net::SocketAddr,
    oneshot::Receiver<Captured>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, rx) = oneshot::channel();
    let tx = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
    let app = Router::new().route(
        route,
        post(move |uri: Uri, headers: HeaderMap, body_bytes: Bytes| {
            let tx = tx.clone();
            async move {
                let parsed = serde_json::from_slice(&body_bytes).expect("request body is JSON");
                if let Some(tx) = tx.lock().unwrap().take() {
                    let _ = tx.send(Captured {
                        uri,
                        headers,
                        body: parsed,
                    });
                }
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
    let (addr, rx, server) =
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
    let captured = rx.await.expect("mock captured the request");
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
    let (addr, rx, server) = spawn_mock(
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
    let captured = rx.await.expect("mock captured the request");
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

#[tokio::test]
#[serial]
async fn generate_content_json_error_relayed_as_200_surfaces_as_a_stream_error() {
    // LiteLLM relays a guardrail block as HTTP 200 with an OpenAI-shaped
    // error envelope. Fed to the SSE decoder it has no `data:` line and
    // the stream used to end as a silent truncation.
    let (addr, rx, server) = spawn_mock_with(
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
    rx.await.expect("mock captured the request");
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
    let (addr, rx, server) = spawn_mock_with(
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
    rx.await.expect("mock captured the request");
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
async fn generate_content_429_envelope_carries_status_and_retry_info_delay() {
    // Google's 429 names the RPC status and advises the delay inside the
    // envelope (`google.rpc.RetryInfo`), without a `Retry-After` header.
    let (addr, rx, server) = spawn_mock_with(
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
    rx.await.expect("mock captured the request");
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
