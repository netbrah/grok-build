//! Gemini-native `generateContent` utility wire (apex-ylqz Phase 1, SDD §4).
//!
//! The web_search engine swap (D1): the tool ID stays `web_search`, the engine
//! becomes a side-call (D2) to the Gemini-native non-streaming `generateContent`
//! route (D3) through the same proxy the session already uses. Wire selection
//! is derived from the model pin (D4): gemini-prefixed pins take this wire,
//! everything else keeps the legacy `/responses` request byte-identical (G3).
//!
//! The fixtures under `fixtures/` are the sanitized RESPONSE captures from the
//! 2026-09-30 live probe (SDD §9) in their `{probed_at, error, headers,
//! http_status, body}` envelope; parser tests read `.body`.

use std::time::Duration;

use serde_json::{Value, json};

/// Per-attempt connect timeout (SDD §4.1, cf M6), mirroring web_fetch/http.rs.
pub const CONNECT_TIMEOUT_SECS: u64 = 10;
/// Per-attempt total timeout (SDD §4.1, cf M6), mirroring the web_fetch pattern.
/// The legacy `/responses` call is unbounded; the native branch MUST bound it.
pub const ATTEMPT_TIMEOUT_SECS: u64 = 60;
/// §4.3: the exact forcing instruction added on the single not-grounded retry
/// (R3 mitigation). Byte-pinned — do not reword.
pub const GROUNDING_RETRY_INSTRUCTION: &str = "You are a web search tool. You MUST answer using the web search tool. Do not answer from memory.";

/// Utility tool kinds the proxy admits on gemini rows (llm-proxy
/// `app/api/auth/tool.py:5` — googleSearch, urlContext).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiToolKind {
    GoogleSearch,
    UrlContext,
}

/// A Gemini `generateContent` utility request (SDD §4.1).
///
/// Builder defaults are the production web-search sampler pins (8192 / 0.1;
/// xai-grok-shell `tools/config.rs:178-193`) — the 256-token probe budget is
/// NOT the production budget (SDD R1).
#[derive(Debug, Clone)]
pub struct GeminiUtilityRequest {
    model: String,
    prompt: String,
    tool_kind: GeminiToolKind,
    max_output_tokens: u32,
    temperature: f64,
    system_instruction: Option<String>,
}

impl GeminiUtilityRequest {
    /// Build a base (non-retry) request: no `systemInstruction` (SDD §4.1).
    pub fn new(
        model: impl Into<String>,
        prompt: impl Into<String>,
        tool_kind: GeminiToolKind,
    ) -> Self {
        Self {
            model: model.into(),
            prompt: prompt.into(),
            tool_kind,
            max_output_tokens: 8192,
            temperature: 0.1,
            system_instruction: None,
        }
    }

    /// Add the optional `systemInstruction`. In v1 the §4.3 retry is the only caller.
    pub fn system_instruction(mut self, text: impl Into<String>) -> Self {
        self.system_instruction = Some(text.into());
        self
    }

    pub fn max_output_tokens(mut self, max: u32) -> Self {
        self.max_output_tokens = max;
        self
    }

    pub fn temperature(mut self, temperature: f64) -> Self {
        self.temperature = temperature;
        self
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// The non-streaming generateContent path (SDD §4.1).
    pub fn path(&self) -> String {
        format!("/v1beta/models/{}:generateContent", self.model)
    }

    /// The request body (SDD §4.1). Key order is serde insertion order:
    /// `contents`, `generationConfig`, `tools`, then `systemInstruction` when set.
    pub fn body(&self) -> Value {
        let tool: Value = match self.tool_kind {
            GeminiToolKind::GoogleSearch => json!({"googleSearch": {}}),
            GeminiToolKind::UrlContext => json!({"urlContext": {}}),
        };
        let mut body = json!({
            "contents": [{"role": "user", "parts": [{"text": self.prompt}]}],
            "generationConfig": {
                "maxOutputTokens": self.max_output_tokens,
                "temperature": self.temperature,
            },
            "tools": [tool],
        });
        if let Some(instruction) = &self.system_instruction {
            body["systemInstruction"] = json!({"parts": [{"text": instruction}]});
        }
        body
    }
}

/// One `groundingMetadata.groundingChunks[].web` entry (SDD §4.2).
///
/// On googleSearch chunks `uri` is an opaque grounding-redirect URL while
/// `domain`/`title` are the real site; on urlContext chunks `uri` is the target
/// URL and `domain` may be absent (P5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroundingChunk {
    pub uri: String,
    pub title: String,
    pub domain: Option<String>,
}

/// A parsed generateContent utility response (SDD §4.2).
#[derive(Debug, Clone)]
pub struct GeminiUtilityResult {
    pub text: String,
    pub finish_reason: String,
    pub chunks: Vec<GroundingChunk>,
}

impl GeminiUtilityResult {
    /// `grounded := groundingMetadata present AND >=1 chunk with non-empty uri`
    /// (SDD §4.2). Absent metadata yields no chunks, so this covers both halves.
    pub fn grounded(&self) -> bool {
        self.chunks.iter().any(|chunk| !chunk.uri.trim().is_empty())
    }

    /// Parse a generateContent response body.
    ///
    /// Absent/malformed `candidates` — INCLUDING the safety-block shape (HTTP
    /// 200, `candidates:[]`, `promptFeedback.blockReason`) and the P4
    /// responses-wire `status:"incomplete"` object — returns `None`; callers
    /// must never surface such a body as a successful search (R2 false-green).
    pub fn parse(value: &Value) -> Option<Self> {
        let candidate = value.get("candidates")?.as_array()?.first()?;
        let parts = candidate.get("content")?.get("parts")?.as_array()?;
        let text = parts
            .iter()
            .filter_map(|part| part.get("text")?.as_str())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let finish_reason = candidate
            .get("finishReason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let mut chunks = Vec::new();
        if let Some(metadata) = candidate.get("groundingMetadata")
            && let Some(chunk_list) = metadata.get("groundingChunks").and_then(Value::as_array)
        {
            for chunk in chunk_list {
                let web = chunk.get("web");
                chunks.push(GroundingChunk {
                    uri: web
                        .and_then(|web| web.get("uri"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    title: web
                        .and_then(|web| web.get("title"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    domain: web
                        .and_then(|web| web.get("domain"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                });
            }
        }
        Some(Self {
            text,
            finish_reason,
            chunks,
        })
    }
}

/// Normalize a session base_url to the Gemini API root (SDD §4.1, normative
/// order): trim trailing slashes → strip ONE trailing `/v1` if present → trim
/// trailing slashes again.
pub fn url_root(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    let stripped = trimmed.strip_suffix("/v1").unwrap_or(trimmed);
    stripped.trim_end_matches('/').to_string()
}

/// Per-attempt timeouts (SDD §4.1, cf M6), mirroring the web_fetch pattern:
/// `connect_timeout(10s)` + total request `timeout(60s)`.
pub fn attempt_timeouts() -> (Duration, Duration) {
    (
        Duration::from_secs(CONNECT_TIMEOUT_SECS),
        Duration::from_secs(ATTEMPT_TIMEOUT_SECS),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read the `body` of a §9 envelope fixture (sanitized RESPONSE capture).
    fn fixture_body(name: &str) -> Value {
        let raw = match name {
            "P3a__resp.json" => include_str!("fixtures/P3a__resp.json"),
            "P3b__resp.json" => include_str!("fixtures/P3b__resp.json"),
            "P4__resp.json" => include_str!("fixtures/P4__resp.json"),
            "P5__resp.json" => include_str!("fixtures/P5__resp.json"),
            "P6__resp.json" => include_str!("fixtures/P6__resp.json"),
            other => panic!("unknown fixture {other}"),
        };
        let envelope: Value = serde_json::from_str(raw).expect("fixture is valid JSON");
        envelope["body"].clone()
    }

    // ── T1: parse on the promoted probe fixtures ──────────────────────────

    #[test]
    fn t1_p3a_v1_grounded_google_search_parses_text_and_chunk() {
        let result = GeminiUtilityResult::parse(&fixture_body("P3a__resp.json"))
            .expect("P3a (the only grounded googleSearch vector) must parse");
        assert!(result.grounded(), "P3a carries a grounding chunk");
        assert_eq!(result.finish_reason, "STOP");
        assert!(!result.text.is_empty());
        assert_eq!(result.chunks.len(), 1);
        let chunk = &result.chunks[0];
        assert_eq!(chunk.domain.as_deref(), Some("kernel.org"));
        assert_eq!(chunk.title, "kernel.org");
        // googleSearch chunk: opaque grounding-redirect uri (D7 match rule).
        assert!(
            chunk
                .uri
                .starts_with("https://vertexaisearch.cloud.google.com/grounding-api-redirect/"),
            "P3a chunk uri is the grounding-redirect form"
        );
    }

    #[test]
    fn t1_p3b_max_tokens_without_grounding_is_not_grounded() {
        let result = GeminiUtilityResult::parse(&fixture_body("P3b__resp.json"))
            .expect("P3b is a valid generateContent body and must parse");
        assert_eq!(result.finish_reason, "MAX_TOKENS");
        assert!(result.chunks.is_empty());
        assert!(
            !result.grounded(),
            "MAX_TOKENS truncation with no chunks is not grounded"
        );
    }

    #[test]
    fn t1_p4_incomplete_ungrounded_never_parses_to_success() {
        let body = fixture_body("P4__resp.json");
        // Pin the exact false-green shape (SDD R2): a plausible responses-wire
        // object (200 + status:"incomplete" + echoed tools:[]) that is NOT a
        // generateContent success.
        assert_eq!(body["status"], "incomplete");
        assert_eq!(body["tools"], json!([]));
        assert!(
            GeminiUtilityResult::parse(&body).is_none(),
            "P4 must never surface as a successful search"
        );
    }

    #[test]
    fn t1_p5_url_context_chunk_is_target_url_with_null_domain() {
        let result =
            GeminiUtilityResult::parse(&fixture_body("P5__resp.json")).expect("P5 must parse");
        assert!(result.grounded());
        assert_eq!(result.finish_reason, "STOP");
        assert_eq!(result.chunks.len(), 1);
        let chunk = &result.chunks[0];
        assert_eq!(chunk.uri, "https://kernel.org");
        assert_eq!(chunk.title, "The Linux Kernel Archives");
        assert_eq!(
            chunk.domain, None,
            "P5 urlContext chunk has no web.domain (D7 uri-host fallback)"
        );
    }

    #[test]
    fn t1_p6_flash_row_is_grounded() {
        let result =
            GeminiUtilityResult::parse(&fixture_body("P6__resp.json")).expect("P6 must parse");
        assert!(result.grounded());
        assert_eq!(result.finish_reason, "STOP");
        assert_eq!(result.chunks.len(), 1);
        assert_eq!(result.chunks[0].domain.as_deref(), Some("kernel.org"));
    }

    #[test]
    fn t1_malformed_and_empty_bodies_parse_to_none() {
        let malformed = [
            json!({}),
            json!(null),
            json!("not an object"),
            json!({"candidates": []}), // safety-block shape: 200 + candidates:[]
            json!({"candidates": [{}]}),
            json!({"candidates": [{"content": {}}]}),
            json!({"candidates": [{"content": {"parts": "nope"}}]}),
        ];
        for value in malformed {
            assert!(
                GeminiUtilityResult::parse(&value).is_none(),
                "malformed body must not parse: {value}"
            );
        }
    }

    #[test]
    fn t1_empty_parts_yield_empty_text_not_grounding() {
        let value = json!({"candidates": [{"content": {"parts": [{}]}}]});
        let result = GeminiUtilityResult::parse(&value)
            .expect("candidate structure with an empty part list is valid");
        assert_eq!(result.text, "");
        assert!(!result.grounded());
    }

    #[test]
    fn t1_text_joins_nonempty_parts_with_newline() {
        let value = json!({"candidates": [{"content": {"parts": [
            {"text": "line one"},
            {"text": ""},
            {"text": "line two"}
        ]}, "finishReason": "STOP"}]});
        let result = GeminiUtilityResult::parse(&value).expect("parses");
        assert_eq!(result.text, "line one\nline two");
    }

    // ── T2: request builder + url_root ─────────────────────────────────────

    const T2_MODEL: &str = "gemini-3.8-flash";
    const T2_QUERY: &str = "what is the latest linux kernel";

    fn t2_request() -> GeminiUtilityRequest {
        GeminiUtilityRequest::new(T2_MODEL, T2_QUERY, GeminiToolKind::GoogleSearch)
    }

    #[test]
    fn t2_body_matches_wire_spec_value_and_order() {
        let body = t2_request().body();
        let expected = json!({
            "contents": [{"role": "user", "parts": [{"text": T2_QUERY}]}],
            "generationConfig": {"maxOutputTokens": 8192, "temperature": 0.1},
            "tools": [{"googleSearch": {}}]
        });
        assert_eq!(body, expected, "body() must equal the §4.1 wire spec");
        // serde insertion order (documented): contents, generationConfig, tools.
        assert_eq!(
            body.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["contents", "generationConfig", "tools"]
        );
        assert!(body.get("systemInstruction").is_none());
        assert!(body.get("stream").is_none(), "non-streaming only");
    }

    #[test]
    fn t2_retry_body_adds_only_the_forcing_system_instruction() {
        let retry = t2_request()
            .system_instruction(GROUNDING_RETRY_INSTRUCTION)
            .body();
        let mut expected = t2_request().body();
        expected["systemInstruction"] = json!({"parts": [{"text": GROUNDING_RETRY_INSTRUCTION}]});
        assert_eq!(retry, expected);
    }

    #[test]
    fn t2_url_context_tool_kind_uses_url_context_wire_key() {
        let body = GeminiUtilityRequest::new(T2_MODEL, "u", GeminiToolKind::UrlContext).body();
        assert_eq!(body["tools"], json!([{"urlContext": {}}]));
    }

    #[test]
    fn t2_forcing_instruction_is_byte_pinned() {
        assert_eq!(
            GROUNDING_RETRY_INSTRUCTION,
            "You are a web search tool. You MUST answer using the web search tool. Do not answer from memory."
        );
    }

    #[test]
    fn t2_path_is_non_streaming_generate_content() {
        assert_eq!(
            t2_request().path(),
            "/v1beta/models/gemini-3.8-flash:generateContent"
        );
    }

    #[test]
    fn t2_url_root_normalizes_all_three_base_url_forms() {
        // §4.1 normative order: trim trailing slashes → strip ONE trailing /v1
        // → trim trailing slashes again.
        assert_eq!(url_root("https://host"), "https://host");
        assert_eq!(url_root("https://host/v1"), "https://host");
        assert_eq!(url_root("https://host/v1/"), "https://host");
    }

    #[test]
    fn t2_url_root_keeps_slashless_proxy_roots() {
        assert_eq!(
            url_root("https://proxy.internal/v1"),
            "https://proxy.internal"
        );
        assert_eq!(url_root("https://proxy.internal"), "https://proxy.internal");
    }

    #[test]
    fn t2_attempt_timeouts_pin_the_spec_values() {
        assert_eq!(CONNECT_TIMEOUT_SECS, 10);
        assert_eq!(ATTEMPT_TIMEOUT_SECS, 60);
    }
}
