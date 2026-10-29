//! Google Gemini `generateContent` / `streamGenerateContent` wire types.
//!
//! Serde mirror of the REST JSON surface shared by the Gemini Developer API
//! (`POST {base}/models/{model}:streamGenerateContent?alt=sse`) and Vertex AI
//! (`POST {base}/publishers/google/models/{model}:streamGenerateContent?alt=sse`).
//! Field names follow the proto JSON mapping (`camelCase`), pinned against the
//! `aiplatform.v1` discovery document (googleapis/discovery-artifact-manager,
//! `GoogleCloudAiplatformV1{Part,FunctionCall,FunctionResponse,FunctionDeclaration,
//! GenerationConfigThinkingConfig,GenerateContentResponseUsageMetadata,Candidate}`)
//! and `google/ai/generativelanguage/v1beta/{content,generative_service}.proto`.
//!
//! Scope: the subset this fork projects onto and decodes from — text, thought
//! parts and their opaque `thoughtSignature`, function calls/responses, inline
//! and file media, function declarations, tool config, generation config
//! (incl. `thinkingConfig`), usage metadata, finish reasons and prompt
//! feedback. Every struct tolerates unknown fields (no `deny_unknown_fields`)
//! so a wire addition never fails a parse mid-stream.
//!
//! Opaque reasoning. A [`Part`] is a proto `oneof` payload (`text` /
//! `inlineData` / `fileData` / `functionCall` / `functionResponse`) plus two
//! metadata fields that ride ANY payload: `thought` (the part is a thought
//! summary, not final output) and `thoughtSignature` (the model's encrypted
//! reasoning state, to be replayed verbatim on the same part). The struct is
//! deliberately NOT an enum so a signature attached to a function call, a
//! text part or an empty trailing part is representable without loss.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Role of a user-authored (or tool-result-carrying) content.
pub const ROLE_USER: &str = "user";
/// Role of a model-authored content.
pub const ROLE_MODEL: &str = "model";

/// The documented bypass value for `thoughtSignature` on a function call the
/// client injected without one (history migrated from another backend, a
/// signature stripped at a model switch, a model that emitted none).
/// Gemini 3 rejects a current-turn `functionCall` with no signature (400), so
/// the projection substitutes this value where no genuine one is stored.
pub const SKIP_THOUGHT_SIGNATURE_VALIDATOR: &str = "skip_thought_signature_validator";

// ============================================================================
// Request
// ============================================================================

/// Body of `models/{model}:generateContent` / `:streamGenerateContent`.
///
/// The model is NOT a body field on this wire (it rides the URL path), so it
/// is carried as a `serde(skip)` companion here — the single place the
/// client reads it from when forming the request URL.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentRequest {
    /// Model slug for the URL path (`models/{model}:streamGenerateContent`). Never serialized.
    #[serde(skip)]
    pub model: String,
    pub contents: Vec<Content>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<Content>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_config: Option<ToolConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GenerationConfig>,
}

impl GenerateContentRequest {
    /// Approximate serialized body size (used for diagnostics only).
    pub fn body_len_estimate(&self) -> usize {
        serde_json::to_vec(self).map(|b| b.len()).unwrap_or(0)
    }
}

/// One multi-part message. `role` is `user` or `model`; absent means `user`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Content {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default)]
    pub parts: Vec<Part>,
}

impl Content {
    pub fn user(parts: Vec<Part>) -> Self {
        Self {
            role: Some(ROLE_USER.to_owned()),
            parts,
        }
    }

    pub fn model(parts: Vec<Part>) -> Self {
        Self {
            role: Some(ROLE_MODEL.to_owned()),
            parts,
        }
    }

    pub fn is_model(&self) -> bool {
        self.role.as_deref() == Some(ROLE_MODEL)
    }
}

/// A single part: exactly one payload field is normally set, plus the
/// optional `thought` / `thoughtSignature` metadata (see module docs).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `true` marks a thought-summary part (returned when
    /// `thinkingConfig.includeThoughts` is set). Absent/`false` = final output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought: Option<bool>,
    /// Opaque, base64-encoded reasoning state. Replay verbatim on the same part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<Blob>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_data: Option<FileData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_call: Option<FunctionCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_response: Option<FunctionResponse>,
}

impl Part {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            ..Self::default()
        }
    }

    pub fn thought(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            thought: Some(true),
            ..Self::default()
        }
    }

    pub fn function_call(call: FunctionCall) -> Self {
        Self {
            function_call: Some(call),
            ..Self::default()
        }
    }

    pub fn function_response(response: FunctionResponse) -> Self {
        Self {
            function_response: Some(response),
            ..Self::default()
        }
    }

    pub fn inline_data(mime_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            inline_data: Some(Blob {
                mime_type: mime_type.into(),
                data: data.into(),
            }),
            ..Self::default()
        }
    }

    pub fn file_data(mime_type: Option<String>, file_uri: impl Into<String>) -> Self {
        Self {
            file_data: Some(FileData {
                mime_type,
                file_uri: file_uri.into(),
            }),
            ..Self::default()
        }
    }

    pub fn with_thought_signature(mut self, signature: impl Into<String>) -> Self {
        self.thought_signature = Some(signature.into());
        self
    }

    /// Whether this part is a thought summary (`thought: true`).
    pub fn is_thought(&self) -> bool {
        self.thought == Some(true)
    }

    /// Whether the part carries no payload at all (a bare signature carrier,
    /// which streaming can emit as its own chunk).
    pub fn is_payload_empty(&self) -> bool {
        self.text.as_deref().is_none_or(str::is_empty)
            && self.inline_data.is_none()
            && self.file_data.is_none()
            && self.function_call.is_none()
            && self.function_response.is_none()
    }
}

/// Inline media (`inlineData`): base64 payload with its IANA MIME type.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Blob {
    pub mime_type: String,
    pub data: String,
}

/// URI-referenced media (`fileData`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    pub file_uri: String,
}

/// A model-predicted function call. `args` is a JSON object (or absent).
///
/// `id` is optional on this wire and the standard API leaves it unset; this
/// fork mints its own call ids locally and does NOT echo them back (an
/// unsolicited `id` is not part of the model's own record of the turn).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
}

/// The client's answer to a [`FunctionCall`]. `response` MUST be a JSON
/// object; the documented convention is an `output` key for the result and
/// an `error` key for failure details.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub response: Value,
}

/// Tool declaration envelope. Only client function declarations are emitted
/// by this fork; hosted Google tools (search, code execution, ...) are not
/// projected.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub function_declarations: Vec<FunctionDeclaration>,
}

/// One callable function. The parameters are sent as full JSON Schema via
/// `parametersJsonSchema` (mutually exclusive with the OpenAPI-subset
/// `parameters`), so no lossy keyword scrubbing is needed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionDeclaration {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters_json_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_calling_config: Option<FunctionCallingConfig>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionCallingConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<FunctionCallingMode>,
    /// With `mode: ANY`, restricts the model to these names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_function_names: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FunctionCallingMode {
    /// Model decides between a function call and text (the default).
    Auto,
    /// Model must call a function (optionally one of `allowedFunctionNames`).
    Any,
    /// Model must not call functions.
    None,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
    /// `application/json` enables JSON mode (with `responseJsonSchema`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_mime_type: Option<String>,
    /// Full JSON Schema the output must satisfy (requires the JSON MIME type).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_json_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_config: Option<ThinkingConfig>,
}

impl GenerationConfig {
    /// `true` when no field is set (the whole object can then be omitted).
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Thinking controls. Gemini 2.5 takes a token `thinkingBudget` (`-1` =
/// dynamic, `0` = off where the model allows it); Gemini 3 takes a
/// `thinkingLevel`. Setting both is a wire error, so the projection picks one
/// by model slug. `includeThoughts` requests thought-summary parts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_thoughts: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ThinkingLevel>,
}

/// Proto enum names (`LOW`, not `low`): the canonical JSON spelling accepted
/// by both the Gemini Developer API and Vertex AI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ThinkingLevel {
    Minimal,
    Low,
    Medium,
    High,
}

// ============================================================================
// Response
// ============================================================================

/// One `generateContent` response, or one `streamGenerateContent` SSE chunk
/// (each `data:` line is a complete object of this shape carrying a delta of
/// parts; `usageMetadata` and `finishReason` arrive on the trailing chunks).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentResponse {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<Candidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_feedback: Option<PromptFeedback>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_metadata: Option<UsageMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Content>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,
    /// Human-readable detail for the finish reason (set only with one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
}

/// Why a candidate stopped. Proto enum names on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FinishReason {
    FinishReasonUnspecified,
    Stop,
    MaxTokens,
    Safety,
    Recitation,
    Language,
    Other,
    Blocklist,
    ProhibitedContent,
    Spii,
    MalformedFunctionCall,
    ModelArmor,
    ImageSafety,
    ImageProhibitedContent,
    ImageRecitation,
    ImageOther,
    UnexpectedToolCall,
    TooManyToolCalls,
    NoImage,
    MalformedResponse,
    /// Catch-all so a new server-side reason never fails the terminal chunk
    /// parse and discards an already-streamed response. Must stay LAST.
    #[serde(untagged)]
    Unknown(String),
}

impl FinishReason {
    /// The verbatim wire string, derived from the serde renames so it cannot drift.
    pub fn wire_str(&self) -> String {
        match serde_json::to_value(self) {
            Ok(Value::String(s)) => s,
            _ => String::new(),
        }
    }

    /// Reasons that mean "output withheld by a content policy".
    pub fn is_content_filter(&self) -> bool {
        matches!(
            self,
            Self::Safety
                | Self::Recitation
                | Self::Language
                | Self::Blocklist
                | Self::ProhibitedContent
                | Self::Spii
                | Self::ModelArmor
                | Self::ImageSafety
                | Self::ImageProhibitedContent
                | Self::ImageRecitation
                | Self::ImageOther
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptFeedback {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_reason_message: Option<String>,
}

/// Token accounting. `candidatesTokenCount` EXCLUDES thoughts; the thinking
/// spend is `thoughtsTokenCount`. `cachedContentTokenCount` is the cache-hit
/// subset of `promptTokenCount`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageMetadata {
    #[serde(default)]
    pub prompt_token_count: u32,
    #[serde(default)]
    pub candidates_token_count: u32,
    #[serde(default)]
    pub total_token_count: u32,
    #[serde(default)]
    pub thoughts_token_count: u32,
    #[serde(default)]
    pub cached_content_token_count: u32,
    #[serde(default)]
    pub tool_use_prompt_token_count: u32,
}

/// Error envelope (`{"error": {"code", "message", "status"}}`) returned on
/// non-2xx responses and, on a streaming route, as the single chunk of an
/// error stream.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    #[serde(default)]
    pub code: Option<i64>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub status: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn part_round_trips_signature_on_function_call() {
        let wire = json!({
            "functionCall": {"name": "get_weather", "args": {"city": "Paris"}},
            "thoughtSignature": "CvkBA..."
        });
        let part: Part = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(part.function_call.as_ref().unwrap().name, "get_weather");
        assert_eq!(part.thought_signature.as_deref(), Some("CvkBA..."));
        assert!(!part.is_thought());
        assert_eq!(serde_json::to_value(&part).unwrap(), wire);
    }

    #[test]
    fn thought_part_and_bare_signature_carrier() {
        let thought: Part = serde_json::from_value(json!({"text": "plan", "thought": true})).unwrap();
        assert!(thought.is_thought());
        assert!(!thought.is_payload_empty());
        let carrier: Part = serde_json::from_value(json!({"thoughtSignature": "sig"})).unwrap();
        assert!(carrier.is_payload_empty());
        assert_eq!(carrier.thought_signature.as_deref(), Some("sig"));
    }

    #[test]
    fn finish_reason_known_and_unknown() {
        let known: FinishReason = serde_json::from_value(json!("MAX_TOKENS")).unwrap();
        assert_eq!(known, FinishReason::MaxTokens);
        assert_eq!(known.wire_str(), "MAX_TOKENS");
        let unknown: FinishReason = serde_json::from_value(json!("SOMETHING_NEW")).unwrap();
        assert_eq!(unknown, FinishReason::Unknown("SOMETHING_NEW".into()));
        assert_eq!(unknown.wire_str(), "SOMETHING_NEW");
        assert!(FinishReason::Safety.is_content_filter());
        assert!(!FinishReason::Stop.is_content_filter());
    }

    #[test]
    fn request_serializes_camel_case_and_skips_model() {
        let req = GenerateContentRequest {
            model: "gemini-3-pro-preview".into(),
            contents: vec![Content::user(vec![Part::text("hi")])],
            system_instruction: Some(Content {
                role: None,
                parts: vec![Part::text("be brief")],
            }),
            tools: vec![Tool {
                function_declarations: vec![FunctionDeclaration {
                    name: "f".into(),
                    description: None,
                    parameters_json_schema: Some(json!({"type": "object"})),
                    parameters: None,
                }],
            }],
            tool_config: Some(ToolConfig {
                function_calling_config: Some(FunctionCallingConfig {
                    mode: Some(FunctionCallingMode::Any),
                    allowed_function_names: vec!["f".into()],
                }),
            }),
            generation_config: Some(GenerationConfig {
                max_output_tokens: Some(10),
                thinking_config: Some(ThinkingConfig {
                    include_thoughts: Some(true),
                    thinking_budget: None,
                    thinking_level: Some(ThinkingLevel::High),
                }),
                ..Default::default()
            }),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert!(v.get("model").is_none());
        assert_eq!(v["systemInstruction"]["parts"][0]["text"], "be brief");
        assert_eq!(v["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"]["type"], "object");
        assert_eq!(v["toolConfig"]["functionCallingConfig"]["mode"], "ANY");
        assert_eq!(v["generationConfig"]["maxOutputTokens"], 10);
        assert_eq!(v["generationConfig"]["thinkingConfig"]["thinkingLevel"], "HIGH");
        assert_eq!(v["generationConfig"]["thinkingConfig"]["includeThoughts"], true);
    }

    #[test]
    fn response_chunk_parses_usage_and_tolerates_unknown_fields() {
        let chunk: GenerateContentResponse = serde_json::from_value(json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "Hello"}]},
                "finishReason": "STOP",
                "index": 0,
                "safetyRatings": [{"category": "X", "probability": "NEGLIGIBLE"}]
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 5,
                "thoughtsTokenCount": 7,
                "totalTokenCount": 22,
                "cachedContentTokenCount": 4,
                "promptTokensDetails": [{"modality": "TEXT", "tokenCount": 10}]
            },
            "modelVersion": "gemini-3-pro-preview",
            "responseId": "abc"
        }))
        .unwrap();
        let usage = chunk.usage_metadata.unwrap();
        assert_eq!(usage.prompt_token_count, 10);
        assert_eq!(usage.thoughts_token_count, 7);
        assert_eq!(usage.cached_content_token_count, 4);
        assert_eq!(chunk.candidates[0].finish_reason, Some(FinishReason::Stop));
        assert_eq!(chunk.model_version.as_deref(), Some("gemini-3-pro-preview"));
    }
}
