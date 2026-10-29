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
    let (tx, rx) = oneshot::channel();
    let tx = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
    let app = Router::new().route(
        route,
        post(move |uri: Uri, headers: HeaderMap, body: Bytes| {
            let tx = tx.clone();
            async move {
                let body = serde_json::from_slice(&body).expect("request body is JSON");
                if let Some(tx) = tx.lock().unwrap().take() {
                    let _ = tx.send(Captured { uri, headers, body });
                }
                axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from(SSE_BODY))
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
    assert!(header("authorization").is_none(), "no bearer on the Gemini wire");
    assert!(header("x-api-key").is_none(), "no Anthropic key header");
    assert!(header("anthropic-version").is_none());
    assert_eq!(header("accept").as_deref(), Some("text/event-stream"));
    assert_eq!(header("content-type").as_deref(), Some("application/json"));
    assert_eq!(header("x-grok-req-id").as_deref(), Some("req-gemini-1"));

    // Body: the projection, with the prior step's opaque reasoning replayed.
    let body = &captured.body;
    assert!(body.get("model").is_none(), "model rides the URL, never the body");
    assert_eq!(body["systemInstruction"]["parts"][0]["text"], "Be terse.");
    let contents = body["contents"].as_array().unwrap();
    assert_eq!(
        contents.len(),
        3,
        "user / model / user (functionResponse merged with the next user text)"
    );
    assert_eq!(contents[1]["role"], "model");
    let model_parts = contents[1]["parts"].as_array().unwrap();
    assert_eq!(model_parts[0]["thought"], true);
    assert_eq!(model_parts[0]["text"], "earlier thought");
    assert_eq!(model_parts[1]["functionCall"]["name"], "lookup");
    assert_eq!(
        model_parts[1]["thoughtSignature"], "SIG_OLD",
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
    assert_eq!(body["tools"][0]["functionDeclarations"][0]["name"], "lookup");
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
    assert_eq!((usage.prompt_token_count, usage.thoughts_token_count), (10, 5));
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
