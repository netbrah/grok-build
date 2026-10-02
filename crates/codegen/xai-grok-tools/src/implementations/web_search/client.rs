use std::time::Duration;

use super::gemini::{
    self, GROUNDING_RETRY_INSTRUCTION, GeminiToolKind, GeminiUtilityRequest, GeminiUtilityResult,
    GroundingChunk,
};
use super::types::WebSearchConfig;
use crate::attribution::{SharedAttributionCallback, ToolConsumer};
use crate::implementations::grok_build::web_fetch::domain::normalize_domain;
use crate::types::SharedApiKeyProvider;
use async_openai::types::responses as rs;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
/// A minimal, purpose-built HTTP client for calling the Responses API
/// with web search capability.
#[derive(Clone)]
pub struct WebSearchClient {
    http: reqwest::Client,
    base_url: String,
    model: String,
    /// Authoritative domain allowlist from `[toolset.web_search] allowed_domains`. When set it
    /// governs the search and the model's per-call `allowed_domains` is ignored (see
    /// [`Self::resolve_filters`]). Mutually exclusive with `default_excluded_domains`.
    default_allowed_domains: Option<Vec<String>>,
    /// Authoritative domain blocklist from `[toolset.web_search] excluded_domains`.
    /// The model cannot un-set it by naming a blocked domain in its own
    /// `allowed_domains`. Mutually exclusive with `default_allowed_domains`.
    default_excluded_domains: Option<Vec<String>>,
    api_key_provider: Option<SharedApiKeyProvider>,
    /// Optional 401-attribution hook. Callers can wire this so a 401
    /// from the Responses API emits an `auth_401_attribution` event
    /// with `consumer == "WebSearch"`.
    attribution_callback: Option<SharedAttributionCallback>,
    /// Per-attempt total timeout for the native generateContent wire
    /// (SDD §4.1, cf M6); production default is
    /// `gemini::ATTEMPT_TIMEOUT_SECS` (60 s). Tests shorten it.
    gemini_attempt_timeout: Duration,
    /// Dedicated client for the native generateContent wire, mirroring the
    /// web_fetch pattern: the same corp-CA builder and default headers as the
    /// shared client plus a 10 s connect timeout. reqwest has no
    /// per-request `connect_timeout`, so it rides on this client; the total
    /// per-attempt timeout is applied per request on top of it. The shared
    /// `http` client stays unbounded for legacy-wire parity.
    gemini_http: reqwest::Client,
}
impl WebSearchClient {
    /// Create a new web search client from `WebSearchConfig::Enabled`.
    ///
    /// Returns `Err` if the config is `Disabled` or if header values are invalid.
    pub fn new(
        config: &WebSearchConfig,
        api_key_provider: Option<SharedApiKeyProvider>,
    ) -> Result<Self, xai_tool_runtime::ToolError> {
        let WebSearchConfig::Enabled {
            api_key,
            base_url,
            model,
            extra_headers,
            alpha_test_key,
            allowed_domains,
            excluded_domains,
        } = config
        else {
            return Err(xai_tool_runtime::ToolError::execution(
                xai_tool_protocol::ToolId::new("web_search").expect("valid"),
                "Cannot create WebSearchClient from disabled config".to_string(),
            ));
        };
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {api_key}")).map_err(|e| {
                xai_tool_runtime::ToolError::execution(
                    xai_tool_protocol::ToolId::new("web_search").expect("valid"),
                    format!("Invalid API key for header: {e}"),
                )
            })?,
        );
        for (key, value) in extra_headers {
            let header_name = HeaderName::from_bytes(key.as_bytes()).map_err(|e| {
                xai_tool_runtime::ToolError::execution(
                    xai_tool_protocol::ToolId::new("web_search").expect("valid"),
                    format!("Invalid header name '{key}': {e}"),
                )
            })?;
            let header_value = HeaderValue::from_str(value).map_err(|e| {
                xai_tool_runtime::ToolError::execution(
                    xai_tool_protocol::ToolId::new("web_search").expect("valid"),
                    format!("Invalid header value for '{key}': {e}"),
                )
            })?;
            headers.insert(header_name, header_value);
        }
        let _ = alpha_test_key;
        let key = crate::util::shared_http::cache_key("web_search", &headers);
        let http = crate::util::shared_http::cached_client(key, || {
            xai_grok_extra_ca::build_reqwest_client(|builder| {
                builder.default_headers(headers.clone())
            })
        })
        .map_err(|e| {
            xai_tool_runtime::ToolError::execution(
                xai_tool_protocol::ToolId::new("web_search").expect("valid"),
                format!("Failed to build HTTP client: {e}"),
            )
        })?;
        let gemini_http = xai_grok_extra_ca::build_reqwest_client(|builder| {
            builder
                .default_headers(headers.clone())
                .connect_timeout(Duration::from_secs(gemini::CONNECT_TIMEOUT_SECS))
        })
        .map_err(|e| {
            xai_tool_runtime::ToolError::execution(
                xai_tool_protocol::ToolId::new("web_search").expect("valid"),
                format!("Failed to build Gemini HTTP client: {e}"),
            )
        })?;
        Ok(Self {
            http,
            gemini_http,
            base_url: base_url.clone(),
            model: model.clone(),
            default_allowed_domains: allowed_domains.clone(),
            default_excluded_domains: excluded_domains.clone(),
            api_key_provider,
            attribution_callback: None,
            gemini_attempt_timeout: Duration::from_secs(gemini::ATTEMPT_TIMEOUT_SECS),
        })
    }
    /// Resolve the effective domain filters for a request. This is required for `excluded_domains` to be a real block. Otherwise the model could
    /// bypass the user's blocklist simply by naming the blocked domain in its own `allowed_domains`. Only when no config policy is set does the
    /// model's per-call allowlist apply. The two lists are mutually exclusive, so at most one of the returned options is `Some`.
    fn resolve_filters(
        &self,
        model_allowed: Option<Vec<String>>,
    ) -> (Option<Vec<String>>, Option<Vec<String>>) {
        if let Some(allowed) = self
            .default_allowed_domains
            .clone()
            .filter(|d| !d.is_empty())
        {
            return (Some(allowed), None);
        }
        if let Some(excluded) = self
            .default_excluded_domains
            .clone()
            .filter(|d| !d.is_empty())
        {
            return (None, Some(excluded));
        }
        (model_allowed.filter(|d| !d.is_empty()), None)
    }
    /// Build the serialized `/responses` request body for a single web search. The request always
    /// carries exactly one tool (`web_search`) at index 0.
    fn build_request_json(
        &self,
        query: &str,
        allowed_domains: Option<Vec<String>>,
        excluded_domains: Option<Vec<String>>,
    ) -> Result<serde_json::Value, xai_tool_runtime::ToolError> {
        let err = |msg: String| {
            xai_tool_runtime::ToolError::execution(
                xai_tool_protocol::ToolId::new("web_search").expect("valid"),
                msg,
            )
        };
        let web_search = rs::WebSearchToolArgs::default()
            .filters(rs::WebSearchToolFilters { allowed_domains })
            .build()
            .map_err(|e| err(format!("Failed to build web search tool: {e}")))?;
        let request = rs::CreateResponseArgs::default()
            .model(self.model.clone())
            .input(query.to_string())
            .tools(vec![rs::Tool::WebSearch(web_search)])
            .store(false)
            .temperature(0.1)
            .top_p(0.95)
            .max_output_tokens(8192u32)
            .build()
            .map_err(|e| err(format!("Failed to build request: {e}")))?;
        let mut body = serde_json::to_value(&request)
            .map_err(|e| err(format!("Failed to serialize request: {e}")))?;
        if let Some(excluded) = excluded_domains.filter(|d| !d.is_empty()) {
            let tool = body
                .get_mut("tools")
                .and_then(|t| t.as_array_mut())
                .and_then(|arr| arr.first_mut())
                .and_then(|t| t.as_object_mut());
            if let Some(tool) = tool {
                let filters = tool
                    .entry("filters")
                    .or_insert_with(|| serde_json::json!({}));
                filters["excluded_domains"] = serde_json::json!(excluded);
            }
        }
        Ok(body)
    }
    /// Wire a 401-attribution callback into this client. Idempotent;
    /// safe to call before or after the first request.
    pub fn with_attribution_callback(
        mut self,
        callback: Option<SharedAttributionCallback>,
    ) -> Self {
        self.attribution_callback = callback;
        self
    }
    async fn current_bearer(&self) -> Option<String> {
        crate::types::api_key_provider::resolve_bearer(self.api_key_provider.as_ref()).await
    }
    fn record_401_attribution(&self, sent_bearer: Option<&str>) {
        crate::attribution::emit_401(
            self.attribution_callback.as_ref(),
            ToolConsumer::WebSearch,
            sent_bearer,
        );
    }
    /// D4: derived wire selection — gemini-prefixed model pins take the native
    /// generateContent wire; everything else stays on the legacy `/responses`
    /// wire byte-identical (G3). Lowercase FIRST: the proxy lowercases the model
    /// before its `startswith('gemini')` test (llm-proxy tool.py:39-42), so a
    /// case-sensitive check would disagree with the proxy on mixed-case pins.
    pub(crate) fn is_gemini_wire(&self) -> bool {
        self.model.to_ascii_lowercase().starts_with("gemini")
    }
    /// D1/D2/D3: the Gemini-native side-call wire. Fires the generateContent
    /// utility call (non-streaming, §4.1), applies the §4.3 no-grounding
    /// retry, and post-filters the grounding chunks per D7.
    async fn search_gemini(
        &self,
        query: &str,
        model_allowed: Option<Vec<String>>,
    ) -> Result<(String, Vec<GroundingChunk>), xai_tool_runtime::ToolError> {
        let first = self
            .gemini_attempt(GeminiToolKind::GoogleSearch, query, None)
            .await?;
        let result = if first.grounded() && first.finish_reason != "MAX_TOKENS" {
            first
        } else {
            // §4.3: exactly ONE retry, forcing the search tool (R3 mitigation).
            let retried = self
                .gemini_attempt(
                    GeminiToolKind::GoogleSearch,
                    query,
                    Some(GROUNDING_RETRY_INSTRUCTION),
                )
                .await?;
            if retried.grounded() && !retried.text.is_empty() {
                retried
            } else {
                // Never return success on an ungrounded search (R2/P4 trap).
                return Ok(("No search results found.".to_string(), Vec::new()));
            }
        };
        let (allowed, excluded) = self.resolve_filters(model_allowed);
        Ok((
            result.text,
            filter_grounding_chunks(result.chunks, (allowed, excluded)),
        ))
    }
    /// One generateContent attempt: build the §4.1 request for `kind`, POST it
    /// through the session proxy with per-attempt timeouts (cf M6), and parse
    /// the response. Transport errors, 5xx, 401 (→ attribution), and
    /// unparseable 200 bodies are `Err`; callers retry only per §4.3, never on
    /// these. The GoogleSearch wire is byte-identical to the pre-Phase-2
    /// (apex-ylqz) shape; the UrlContext arm pins temperature 0 (deterministic
    /// extraction) with the same production maxOutputTokens.
    async fn gemini_attempt(
        &self,
        kind: GeminiToolKind,
        query: &str,
        system_instruction: Option<&str>,
    ) -> Result<GeminiUtilityResult, xai_tool_runtime::ToolError> {
        let err = |msg: String| {
            xai_tool_runtime::ToolError::execution(
                xai_tool_protocol::ToolId::new("web_search").expect("valid"),
                msg,
            )
        };
        let mut request = GeminiUtilityRequest::new(self.model.as_str(), query, kind);
        if kind == GeminiToolKind::UrlContext {
            request = request.temperature(0.0);
        }
        let request = match system_instruction {
            Some(instruction) => request.system_instruction(instruction),
            None => request,
        };
        let url = format!("{}{}", gemini::url_root(&self.base_url), request.path());
        let sent_bearer = self.current_bearer().await;
        let body = request.body();
        let mut req = self
            .gemini_http
            .post(&url)
            .json(&body)
            .timeout(self.gemini_attempt_timeout);
        if let Some(ref key) = sent_bearer {
            req = req.header(AUTHORIZATION, format!("Bearer {key}"));
        }
        let response = req
            .send()
            .await
            .map_err(|e| err(format!("generateContent request failed: {e}")))?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            self.record_401_attribution(sent_bearer.as_deref());
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read error body".to_string());
            return Err(xai_tool_runtime::ToolError::unauthorized(format!(
                "generateContent returned 401 Unauthorized: {body}"
            ))
            .with_details(serde_json::json!({
                "tool_id": "web_search",
                "status": 401,
            })));
        }
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read error body".to_string());
            return Err(err(format!("generateContent returned {status}: {body}")));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|e| err(format!("Failed to read generateContent body: {e}")))?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| err(format!("Failed to parse generateContent body: {e}")))?;
        GeminiUtilityResult::parse(&value).ok_or_else(|| {
            let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]);
            err(format!(
                "generateContent returned 200 but the body is not a successful search (head: {head})"
            ))
        })
    }
    /// The urlContext utility wire (apex-ylqz Phase 2, SDD §13.4): one
    /// generateContent attempt with `tools:[{"urlContext":{}}]` and
    /// temperature 0. No grounding retry (R8: the web_fetch caller falls back
    /// to its direct fetch on any failure; a second LLM hop is never made).
    pub async fn url_context(
        &self,
        prompt: &str,
    ) -> Result<GeminiUtilityResult, xai_tool_runtime::ToolError> {
        self.gemini_attempt(GeminiToolKind::UrlContext, prompt, None)
            .await
    }
    /// Test knob (mirrors the direct field writes in the T3 tests): shorten
    /// the per-attempt timeout so the timeout path is exercisable in other
    /// modules without waiting 60 s.
    #[cfg(test)]
    pub(crate) fn set_gemini_attempt_timeout_for_test(&mut self, timeout: Duration) {
        self.gemini_attempt_timeout = timeout;
    }
    /// Perform a web search query using the Responses API. Returns `(content, citations)` where
    /// content is the assistant's text and citations are unique URLs found in the response
    /// annotations.
    pub async fn search(
        &self,
        query: &str,
        allowed_domains: Option<Vec<String>>,
    ) -> Result<(String, Vec<String>), xai_tool_runtime::ToolError> {
        if self.is_gemini_wire() {
            let (content, chunks) = self.search_gemini(query, allowed_domains).await?;
            return Ok((content, chunks.into_iter().map(|chunk| chunk.uri).collect()));
        }
        let (allowed, excluded) = self.resolve_filters(allowed_domains);
        let request = self.build_request_json(query, allowed, excluded)?;
        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));
        let sent_bearer = self.current_bearer().await;
        let mut req = self.http.post(&url).json(&request);
        if let Some(ref key) = sent_bearer {
            req = req.header(AUTHORIZATION, format!("Bearer {key}"));
        }
        let response = req.send().await.map_err(|e| {
            xai_tool_runtime::ToolError::execution(
                xai_tool_protocol::ToolId::new("web_search").expect("valid"),
                format!("HTTP request failed: {e}"),
            )
        })?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            self.record_401_attribution(sent_bearer.as_deref());
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read error body".to_string());
            return Err(xai_tool_runtime::ToolError::unauthorized(format!(
                "Responses API returned 401 Unauthorized: {body}"
            ))
            .with_details(serde_json::json!({
                "tool_id": "web_search",
                "status": 401,
            })));
        }
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read error body".to_string());
            return Err(xai_tool_runtime::ToolError::execution(
                xai_tool_protocol::ToolId::new("web_search").expect("valid"),
                format!("Responses API returned {status}: {body}"),
            ));
        }
        let bytes = response.bytes().await.map_err(|e| {
            xai_tool_runtime::ToolError::execution(
                xai_tool_protocol::ToolId::new("web_search").expect("valid"),
                format!("Failed to read response body: {e}"),
            )
        })?;
        let response_obj: rs::Response = serde_json::from_slice(&bytes).map_err(|e| {
            xai_tool_runtime::ToolError::execution(
                xai_tool_protocol::ToolId::new("web_search").expect("valid"),
                format!("Failed to parse response: {e}"),
            )
        })?;
        let content = response_obj
            .output_text()
            .unwrap_or_else(|| "No search results found.".to_string());
        let citations = extract_citations(&response_obj);
        Ok((content, citations))
    }
    /// Same as [`Self::search`] but also extracts per-citation titles when the Responses API surfaces them. Returns `(content,
    /// citations_with_titles)` where each citation is `(title, url)`. Empty `title` strings indicate the upstream didn't supply one for that URL.
    /// Used by the cursor-compat `WebSearch` adapter to render a `Links:\n1. [title](url)` list instead of the LLM synthesis text.
    pub async fn search_with_titles(
        &self,
        query: &str,
        allowed_domains: Option<Vec<String>>,
    ) -> Result<(String, Vec<(String, String)>), xai_tool_runtime::ToolError> {
        if self.is_gemini_wire() {
            let (content, chunks) = self.search_gemini(query, allowed_domains).await?;
            return Ok((
                content,
                chunks
                    .into_iter()
                    .map(|chunk| (chunk.title, chunk.uri))
                    .collect(),
            ));
        }
        let (allowed, excluded) = self.resolve_filters(allowed_domains);
        let request = self.build_request_json(query, allowed, excluded)?;
        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));
        let sent_bearer = self.current_bearer().await;
        let mut req = self.http.post(&url).json(&request);
        if let Some(ref key) = sent_bearer {
            req = req.header(AUTHORIZATION, format!("Bearer {key}"));
        }
        let response = req.send().await.map_err(|e| {
            xai_tool_runtime::ToolError::execution(
                xai_tool_protocol::ToolId::new("web_search").expect("valid"),
                format!("HTTP request failed: {e}"),
            )
        })?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            self.record_401_attribution(sent_bearer.as_deref());
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read error body".to_string());
            return Err(xai_tool_runtime::ToolError::unauthorized(format!(
                "Responses API returned 401 Unauthorized: {body}"
            ))
            .with_details(serde_json::json!({
                "tool_id": "web_search",
                "status": 401,
            })));
        }
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read error body".to_string());
            return Err(xai_tool_runtime::ToolError::execution(
                xai_tool_protocol::ToolId::new("web_search").expect("valid"),
                format!("Responses API returned {status}: {body}"),
            ));
        }
        let bytes = response.bytes().await.map_err(|e| {
            xai_tool_runtime::ToolError::execution(
                xai_tool_protocol::ToolId::new("web_search").expect("valid"),
                format!("Failed to read response body: {e}"),
            )
        })?;
        let response_obj: rs::Response = serde_json::from_slice(&bytes).map_err(|e| {
            xai_tool_runtime::ToolError::execution(
                xai_tool_protocol::ToolId::new("web_search").expect("valid"),
                format!("Failed to parse response: {e}"),
            )
        })?;
        let content = response_obj
            .output_text()
            .unwrap_or_else(|| "No search results found.".to_string());
        let pairs = extract_citation_pairs(&response_obj);
        Ok((content, pairs))
    }
}
/// Extract citation URLs from the Response output items.
/// The async-openai crate doesn't provide a helper for this, and the `url` field
/// in `UrlCitationBody` is private, so we serialize to JSON to extract it.
fn extract_citations(response: &rs::Response) -> Vec<String> {
    let mut citations = Vec::new();
    for output_item in &response.output {
        if let rs::OutputItem::Message(output_message) = output_item {
            for message_content in &output_message.content {
                if let rs::OutputMessageContent::OutputText(text_content) = message_content {
                    for annotation in &text_content.annotations {
                        if let rs::Annotation::UrlCitation(url_citation) = annotation
                            && let Ok(json) = serde_json::to_value(url_citation)
                            && let Some(url) = json.get("url").and_then(|v| v.as_str())
                        {
                            citations.push(url.to_string());
                        }
                    }
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    citations.retain(|url| seen.insert(url.clone()));
    citations
}
/// Extract `(title, url)` pairs from the Responses API annotations. `title` may be an empty string
/// when upstream doesn't supply one. URLs are deduplicated while preserving the first-seen order so
/// the rendered `Links:` list is stable and free of duplicates.
fn extract_citation_pairs(response: &rs::Response) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for output_item in &response.output {
        if let rs::OutputItem::Message(output_message) = output_item {
            for message_content in &output_message.content {
                if let rs::OutputMessageContent::OutputText(text_content) = message_content {
                    for annotation in &text_content.annotations {
                        if let rs::Annotation::UrlCitation(url_citation) = annotation
                            && let Ok(json) = serde_json::to_value(url_citation)
                        {
                            let url = json.get("url").and_then(|v| v.as_str()).unwrap_or("");
                            if url.is_empty() {
                                continue;
                            }
                            let title = json
                                .get("title")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            pairs.push((title, url.to_string()));
                        }
                    }
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    pairs.retain(|(_t, url)| seen.insert(url.clone()));
    pairs
}
/// D7 match rule: the chunk's effective domain for policy matching.
/// The normalized `web.domain` when present and non-empty; otherwise the host
/// parsed from `web.uri` (urlContext chunks carry the target URL and no
/// domain, P5). On googleSearch chunks `web.uri` is the opaque
/// `vertexaisearch.cloud.google.com/grounding-api-redirect/…` URL, so the
/// domain — not the uri host — is what the policy must match (M1).
fn chunk_domain(chunk: &GroundingChunk) -> Option<String> {
    if let Some(domain) = chunk.domain.as_deref()
        && !domain.is_empty()
    {
        return Some(normalize_domain(domain));
    }
    url::Url::parse(&chunk.uri)
        .ok()
        .and_then(|parsed| parsed.host_str().map(normalize_domain))
}
/// §D7 post-filter: keep allowed / drop excluded on the grounding chunks,
/// preserving `resolve_filters`' mutually-exclusive semantics, then dedup by
/// uri in first-seen order (§4.2). A chunk with no matchable domain is
/// dropped under an allowlist, kept under a blocklist. The model's text is
/// never filtered (R4).
fn filter_grounding_chunks(
    chunks: Vec<GroundingChunk>,
    filters: (Option<Vec<String>>, Option<Vec<String>>),
) -> Vec<GroundingChunk> {
    let normalize_list = |list: &[String]| {
        list.iter()
            .map(|d| normalize_domain(d))
            .filter(|d| !d.is_empty())
            .collect::<std::collections::HashSet<_>>()
    };
    let allowed = filters.0.as_deref().map(normalize_list);
    let excluded = filters.1.as_deref().map(normalize_list);
    let mut kept: Vec<GroundingChunk> = chunks
        .into_iter()
        .filter(|chunk| match (&allowed, &excluded) {
            (Some(allowed), _) => chunk_domain(chunk)
                .as_deref()
                .is_some_and(|domain| allowed.contains(domain)),
            (None, Some(excluded)) => chunk_domain(chunk)
                .as_deref()
                .map(|domain| !excluded.contains(domain))
                .unwrap_or(true),
            (None, None) => true,
        })
        .collect();
    let mut seen = std::collections::HashSet::new();
    kept.retain(|chunk| seen.insert(chunk.uri.clone()));
    kept
}
#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    /// Helper to create a Response from JSON for testing.
    fn response_from_json(json: serde_json::Value) -> rs::Response {
        serde_json::from_value(json).expect("Failed to parse test Response JSON")
    }
    /// Build a client with the given configured domain defaults.
    fn client_with_defaults(
        allowed: Option<Vec<String>>,
        excluded: Option<Vec<String>>,
    ) -> WebSearchClient {
        let config = WebSearchConfig::Enabled {
            api_key: "test-key".to_string(),
            base_url: "https://api.x.ai/v1".to_string(),
            model: "test-model".to_string(),
            extra_headers: IndexMap::new(),
            alpha_test_key: None,
            allowed_domains: allowed,
            excluded_domains: excluded,
        };
        WebSearchClient::new(&config, None).expect("client should build")
    }
    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }
    #[test]
    fn resolve_filters_config_allowlist_wins_over_model() {
        let client = client_with_defaults(Some(v(&["config.com"])), None);
        let (allowed, excluded) = client.resolve_filters(Some(v(&["model.com"])));
        assert_eq!(allowed, Some(v(&["config.com"])));
        assert!(excluded.is_none());
    }
    #[test]
    fn resolve_filters_uses_config_allowlist_when_model_silent() {
        let client = client_with_defaults(Some(v(&["config.com"])), None);
        let (allowed, excluded) = client.resolve_filters(None);
        assert_eq!(allowed, Some(v(&["config.com"])));
        assert!(excluded.is_none());
    }
    #[test]
    fn resolve_filters_config_blocklist_applies_when_no_allowlist() {
        let client = client_with_defaults(None, Some(v(&["reddit.com"])));
        let (allowed, excluded) = client.resolve_filters(None);
        assert!(allowed.is_none());
        assert_eq!(excluded, Some(v(&["reddit.com"])));
    }
    #[test]
    fn resolve_filters_config_blocklist_cannot_be_bypassed_by_model() {
        let client = client_with_defaults(None, Some(v(&["github.com"])));
        let (allowed, excluded) = client.resolve_filters(Some(v(&["github.com"])));
        assert!(
            allowed.is_none(),
            "model allowlist must not override the block"
        );
        assert_eq!(excluded, Some(v(&["github.com"])));
    }
    #[test]
    fn resolve_filters_no_config_honors_model_allowlist() {
        let client = client_with_defaults(None, None);
        let (allowed, excluded) = client.resolve_filters(Some(v(&["model.com"])));
        assert_eq!(allowed, Some(v(&["model.com"])));
        assert!(excluded.is_none());
    }
    #[test]
    fn build_request_json_injects_excluded_domains() {
        let client = client_with_defaults(None, None);
        let body = client
            .build_request_json("q", None, Some(v(&["reddit.com"])))
            .expect("request json builds");
        let filters = &body["tools"][0]["filters"];
        assert_eq!(
            filters["excluded_domains"],
            serde_json::json!(["reddit.com"])
        );
        assert!(filters.get("allowed_domains").is_none());
    }
    #[test]
    fn build_request_json_allowlist_only_has_no_excluded_key() {
        let client = client_with_defaults(None, None);
        let body = client
            .build_request_json("q", Some(v(&["docs.x.ai"])), None)
            .expect("request json builds");
        let filters = &body["tools"][0]["filters"];
        assert_eq!(filters["allowed_domains"], serde_json::json!(["docs.x.ai"]));
        assert!(filters.get("excluded_domains").is_none());
    }
    #[test]
    fn test_new_client_uses_configured_model() {
        let config = WebSearchConfig::Enabled {
            api_key: "test-key".to_string(),
            base_url: "https://api.x.ai/v1".to_string(),
            model: "custom-enterprise-model".to_string(),
            extra_headers: IndexMap::new(),
            alpha_test_key: None,
            allowed_domains: None,
            excluded_domains: None,
        };
        let client = WebSearchClient::new(&config, None).expect("client should build");
        assert_eq!(client.model, "custom-enterprise-model");
    }
    /// Counts attribution callback invocations for the test below.
    #[derive(Default, Debug)]
    struct CountingCallback {
        invocations: std::sync::Mutex<Vec<(ToolConsumer, Option<String>)>>,
    }
    impl crate::attribution::Auth401AttributionCallback for CountingCallback {
        fn record_401(&self, consumer: ToolConsumer, sent_bearer_suffix: Option<&str>) {
            self.invocations
                .lock()
                .unwrap()
                .push((consumer, sent_bearer_suffix.map(|s| s.to_string())));
        }
    }
    /// `record_401_attribution` invokes the wired callback with
    /// `ToolConsumer::WebSearch` and the truncated bearer prefix.
    /// The full bearer never crosses the trait boundary.
    #[test]
    fn record_401_attribution_passes_truncated_prefix_to_callback() {
        let cb = std::sync::Arc::new(CountingCallback::default());
        let cb_dyn: crate::attribution::SharedAttributionCallback = cb.clone();
        let config = WebSearchConfig::Enabled {
            api_key: "ignored".to_string(),
            base_url: "https://api.x.ai/v1".to_string(),
            model: "test-model".to_string(),
            extra_headers: IndexMap::new(),
            alpha_test_key: None,
            allowed_domains: None,
            excluded_domains: None,
        };
        let client = WebSearchClient::new(&config, None)
            .expect("client should build")
            .with_attribution_callback(Some(cb_dyn));
        client.record_401_attribution(Some("bearer-with-long-tail-aaaadistinct"));
        let calls = cb.invocations.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, ToolConsumer::WebSearch);
        assert_eq!(calls[0].1.as_deref(), Some("aaaadistinct"));
        assert_eq!(
            calls[0].1.as_deref().map(str::len),
            Some(crate::attribution::BEARER_SUFFIX_LEN),
        );
    }
    /// `record_401_attribution` is a no-op when no callback is wired
    /// -- the BYOK / standalone case must not panic or allocate.
    #[test]
    fn record_401_attribution_is_noop_without_callback() {
        let config = WebSearchConfig::Enabled {
            api_key: "test-key".to_string(),
            base_url: "https://api.x.ai/v1".to_string(),
            model: "test-model".to_string(),
            extra_headers: IndexMap::new(),
            alpha_test_key: None,
            allowed_domains: None,
            excluded_domains: None,
        };
        let client = WebSearchClient::new(&config, None).expect("client should build");
        client.record_401_attribution(Some("any-bearer"));
        client.record_401_attribution(None);
    }
    #[test]
    fn test_extract_citations_empty_response() {
        let response = response_from_json(serde_json::json!({
            "id": "resp_test",
            "object": "response",
            "created_at": 1234567890,
            "status": "completed",
            "output": [],
            "model": "test-model"
        }));
        let citations = extract_citations(&response);
        assert!(citations.is_empty());
    }
    #[test]
    fn test_extract_citations_with_url_citations() {
        let response = response_from_json(serde_json::json!({
            "id": "resp_test",
            "object": "response",
            "created_at": 1234567890,
            "status": "completed",
            "model": "test-model",
            "output": [
                {
                    "type": "message",
                    "id": "msg_1",
                    "status": "completed",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "output_text",
                            "text": "Here is some info about Rust.",
                            "annotations": [
                                {
                                    "type": "url_citation",
                                    "url": "https://www.rust-lang.org/",
                                    "title": "Rust Programming Language",
                                    "start_index": 0,
                                    "end_index": 10
                                },
                                {
                                    "type": "url_citation",
                                    "url": "https://docs.rs/",
                                    "title": "Docs.rs",
                                    "start_index": 11,
                                    "end_index": 20
                                }
                            ]
                        }
                    ]
                }
            ]
        }));
        let citations = extract_citations(&response);
        assert_eq!(citations.len(), 2);
        assert_eq!(citations[0], "https://www.rust-lang.org/");
        assert_eq!(citations[1], "https://docs.rs/");
    }
    #[test]
    fn test_extract_citations_deduplicates() {
        let response = response_from_json(serde_json::json!({
            "id": "resp_test",
            "object": "response",
            "created_at": 1234567890,
            "status": "completed",
            "model": "test-model",
            "output": [
                {
                    "type": "message",
                    "id": "msg_1",
                    "status": "completed",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "output_text",
                            "text": "Info with duplicate citations.",
                            "annotations": [
                                {
                                    "type": "url_citation",
                                    "url": "https://example.com/page1",
                                    "title": "Page 1",
                                    "start_index": 0,
                                    "end_index": 5
                                },
                                {
                                    "type": "url_citation",
                                    "url": "https://example.com/page2",
                                    "title": "Page 2",
                                    "start_index": 6,
                                    "end_index": 10
                                },
                                {
                                    "type": "url_citation",
                                    "url": "https://example.com/page1",
                                    "title": "Page 1 Again",
                                    "start_index": 11,
                                    "end_index": 15
                                }
                            ]
                        }
                    ]
                }
            ]
        }));
        let citations = extract_citations(&response);
        assert_eq!(citations.len(), 2);
        assert_eq!(citations[0], "https://example.com/page1");
        assert_eq!(citations[1], "https://example.com/page2");
    }
    #[test]
    fn test_extract_citations_multiple_messages() {
        let response = response_from_json(serde_json::json!({
            "id": "resp_test",
            "object": "response",
            "created_at": 1234567890,
            "status": "completed",
            "model": "test-model",
            "output": [
                {
                    "type": "message",
                    "id": "msg_1",
                    "status": "completed",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "output_text",
                            "text": "First message",
                            "annotations": [
                                {
                                    "type": "url_citation",
                                    "url": "https://first.com/",
                                    "title": "First",
                                    "start_index": 0,
                                    "end_index": 5
                                }
                            ]
                        }
                    ]
                },
                {
                    "type": "message",
                    "id": "msg_2",
                    "status": "completed",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "output_text",
                            "text": "Second message",
                            "annotations": [
                                {
                                    "type": "url_citation",
                                    "url": "https://second.com/",
                                    "title": "Second",
                                    "start_index": 0,
                                    "end_index": 6
                                }
                            ]
                        }
                    ]
                }
            ]
        }));
        let citations = extract_citations(&response);
        assert_eq!(citations.len(), 2);
        assert_eq!(citations[0], "https://first.com/");
        assert_eq!(citations[1], "https://second.com/");
    }
    #[test]
    fn test_extract_citations_ignores_non_url_annotations() {
        let response = response_from_json(serde_json::json!({
            "id": "resp_test",
            "object": "response",
            "created_at": 1234567890,
            "status": "completed",
            "model": "test-model",
            "output": [
                {
                    "type": "message",
                    "id": "msg_1",
                    "status": "completed",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "output_text",
                            "text": "Some text",
                            "annotations": [
                                {
                                    "type": "url_citation",
                                    "url": "https://valid.com/",
                                    "title": "Valid",
                                    "start_index": 0,
                                    "end_index": 4
                                }
                            ]
                        }
                    ]
                }
            ]
        }));
        let citations = extract_citations(&response);
        assert_eq!(citations.len(), 1);
        assert_eq!(citations[0], "https://valid.com/");
    }
    /// A provider that always returns `None`, simulating an API-key user
    /// whose token has aged past the client-side TTL.
    struct NoneProvider;
    impl crate::types::ApiKeyProvider for NoneProvider {
        fn current_api_key(&self) -> Option<String> {
            None
        }
    }
    /// When the dynamic provider returns `None`, the static `api_key` from config must still be
    /// sent as the Authorization header. This is a regression scenario: API-key users past the
    /// 30-day client TTL saw 401 because no auth was sent.
    #[tokio::test]
    async fn static_api_key_is_fallback_when_provider_returns_none() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("Authorization", "Bearer static-key-from-config"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "resp_test",
                "object": "response",
                "created_at": 1234567890,
                "status": "completed",
                "model": "test-model",
                "output": [{
                    "type": "message",
                    "id": "msg_1",
                    "status": "completed",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "search result",
                        "annotations": []
                    }]
                }]
            })))
            .mount(&server)
            .await;
        let config = WebSearchConfig::Enabled {
            api_key: "static-key-from-config".to_string(),
            base_url: server.uri(),
            model: "test-model".to_string(),
            extra_headers: IndexMap::new(),
            alpha_test_key: None,
            allowed_domains: None,
            excluded_domains: None,
        };
        let provider: SharedApiKeyProvider = std::sync::Arc::new(NoneProvider);
        let client = WebSearchClient::new(&config, Some(provider)).expect("client should build");
        let (content, _citations) = client
            .search("test query", None)
            .await
            .expect("search must succeed with static key fallback");
        assert_eq!(content, "search result");
    }
    /// When the provider returns a fresh key, it overrides the static one.
    #[tokio::test]
    async fn provider_key_overrides_static_key() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        struct FreshProvider;
        impl crate::types::ApiKeyProvider for FreshProvider {
            fn current_api_key(&self) -> Option<String> {
                Some("fresh-key-from-provider".to_string())
            }
        }
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("Authorization", "Bearer fresh-key-from-provider"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "resp_test",
                "object": "response",
                "created_at": 1234567890,
                "status": "completed",
                "model": "test-model",
                "output": [{
                    "type": "message",
                    "id": "msg_1",
                    "status": "completed",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "fresh result",
                        "annotations": []
                    }]
                }]
            })))
            .mount(&server)
            .await;
        let config = WebSearchConfig::Enabled {
            api_key: "stale-static-key".to_string(),
            base_url: server.uri(),
            model: "test-model".to_string(),
            extra_headers: IndexMap::new(),
            alpha_test_key: None,
            allowed_domains: None,
            excluded_domains: None,
        };
        let provider: SharedApiKeyProvider = std::sync::Arc::new(FreshProvider);
        let client = WebSearchClient::new(&config, Some(provider)).expect("client should build");
        let (content, _citations) = client
            .search("test query", None)
            .await
            .expect("search must succeed with provider key");
        assert_eq!(content, "fresh result");
    }
    #[test]
    fn test_extract_citations_no_annotations() {
        let response = response_from_json(serde_json::json!({
            "id": "resp_test",
            "object": "response",
            "created_at": 1234567890,
            "status": "completed",
            "model": "test-model",
            "output": [
                {
                    "type": "message",
                    "id": "msg_1",
                    "status": "completed",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "output_text",
                            "text": "Plain text with no annotations",
                            "annotations": []
                        }
                    ]
                }
            ]
        }));
        let citations = extract_citations(&response);
        assert!(citations.is_empty());
    }
    // G3 golden: with a NON-gemini model pin, the legacy /responses request body is
    // byte-identical to the pre-cut wire (captured from the uncut tree, 2026-10-02).
    // The golden pins the REQUEST, not the proxy outcome — the live proxy 403s this
    // wire on this deployment (SDD P1/P1c); that is documented reality, not a failure.
    const G3_MODEL: &str = "gpt-5.6-terra";
    const G3_QUERY: &str = "what is the latest linux kernel";
    const G3_LEGACY_BODY_NO_FILTERS: &str = r#"{"input":"what is the latest linux kernel","max_output_tokens":8192,"model":"gpt-5.6-terra","store":false,"temperature":0.10000000149011612,"tools":[{"type":"web_search","filters":{}}],"top_p":0.949999988079071}"#;
    const G3_LEGACY_BODY_FILTERS: &str = r#"{"input":"what is the latest linux kernel","max_output_tokens":8192,"model":"gpt-5.6-terra","store":false,"temperature":0.10000000149011612,"tools":[{"type":"web_search","filters":{"allowed_domains":["docs.x.ai"],"excluded_domains":["reddit.com"]}}],"top_p":0.949999988079071}"#;

    fn g3_client() -> WebSearchClient {
        let config = WebSearchConfig::Enabled {
            api_key: "test-key".to_string(),
            base_url: "https://api.x.ai/v1".to_string(),
            model: G3_MODEL.to_string(),
            extra_headers: IndexMap::new(),
            alpha_test_key: None,
            allowed_domains: None,
            excluded_domains: None,
        };
        WebSearchClient::new(&config, None).expect("client should build")
    }

    #[test]
    fn g3_non_gemini_pin_keeps_legacy_responses_body_byte_identical() {
        let client = g3_client();
        assert!(
            !client.is_gemini_wire(),
            "golden model must take the legacy wire"
        );
        let plain = client
            .build_request_json(G3_QUERY, None, None)
            .expect("request json builds");
        assert_eq!(
            serde_json::to_string(&plain).unwrap(),
            G3_LEGACY_BODY_NO_FILTERS
        );
        let filtered = client
            .build_request_json(
                G3_QUERY,
                Some(vec!["docs.x.ai".to_string()]),
                Some(vec!["reddit.com".to_string()]),
            )
            .expect("request json builds");
        assert_eq!(
            serde_json::to_string(&filtered).unwrap(),
            G3_LEGACY_BODY_FILTERS
        );
    }
    // ── T3: search_gemini end-to-end against a local wiremock stub ──────────
    // wiremock (an existing dev-dep) serves the canned generateContent bodies:
    // xai-grok-test-support's MockInferenceServer only knows /v1/chat/completions
    // and /v1/responses (implementation decision, noted per SDD §5 T3).

    const T3_MODEL: &str = "gemini-test-model";
    const T3_PATH: &str = "/v1beta/models/gemini-test-model:generateContent";

    /// Read the `body` of a §9 envelope fixture (sanitized RESPONSE capture).
    fn t3_fixture_body(name: &str) -> serde_json::Value {
        let raw = match name {
            "P3a__resp.json" => include_str!("fixtures/P3a__resp.json"),
            "P3b__resp.json" => include_str!("fixtures/P3b__resp.json"),
            "P4__resp.json" => include_str!("fixtures/P4__resp.json"),
            "P5__resp.json" => include_str!("fixtures/P5__resp.json"),
            "P6__resp.json" => include_str!("fixtures/P6__resp.json"),
            other => panic!("unknown fixture {other}"),
        };
        let envelope: serde_json::Value = serde_json::from_str(raw).expect("fixture is valid JSON");
        envelope["body"].clone()
    }

    fn t3_fixture_text(name: &str) -> String {
        t3_fixture_body(name)
            .pointer("/candidates/0/content/parts/0/text")
            .and_then(serde_json::Value::as_str)
            .expect("fixture carries candidate text")
            .to_string()
    }

    fn t3_fixture_chunk_uri(name: &str, index: usize) -> String {
        t3_fixture_body(name)
            .pointer(&format!(
                "/candidates/0/groundingMetadata/groundingChunks/{index}/web/uri"
            ))
            .and_then(serde_json::Value::as_str)
            .expect("fixture carries the grounding chunk uri")
            .to_string()
    }

    fn t3_client(
        server: &wiremock::MockServer,
        allowed: Option<Vec<String>>,
        excluded: Option<Vec<String>>,
    ) -> WebSearchClient {
        let config = WebSearchConfig::Enabled {
            api_key: "test-key".to_string(),
            base_url: server.uri(),
            model: T3_MODEL.to_string(),
            extra_headers: IndexMap::new(),
            alpha_test_key: None,
            allowed_domains: allowed,
            excluded_domains: excluded,
        };
        WebSearchClient::new(&config, None).expect("client should build")
    }

    /// Sequential canned bodies: call N gets body[N] (the last one repeats), so
    /// the retry tests pin "first attempt X, retry Y" on a single mock.
    struct SequentialBodies(std::sync::atomic::AtomicUsize, Vec<serde_json::Value>);

    impl wiremock::Respond for SequentialBodies {
        fn respond(&self, _request: &wiremock::Request) -> wiremock::ResponseTemplate {
            let n = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body = &self.1[n.min(self.1.len() - 1)];
            wiremock::ResponseTemplate::new(200).set_body_json(body.clone())
        }
    }

    #[tokio::test]
    async fn t3_p3a_grounded_success_returns_text_and_citation_uri() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(T3_PATH))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(t3_fixture_body("P3a__resp.json")),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = t3_client(&server, None, None);
        let (content, citations) = client
            .search("what is the latest linux kernel", None)
            .await
            .expect("grounded P3a must succeed");
        assert_eq!(content, t3_fixture_text("P3a__resp.json"));
        assert_eq!(citations, vec![t3_fixture_chunk_uri("P3a__resp.json", 0)]);
        let reqs = server.received_requests().await.expect("requests recorded");
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].url.path(), T3_PATH);
        let body: serde_json::Value = reqs[0].body_json().expect("request body is JSON");
        assert_eq!(
            body["contents"][0]["parts"][0]["text"],
            serde_json::json!("what is the latest linux kernel")
        );
        assert_eq!(
            body["generationConfig"]["maxOutputTokens"],
            serde_json::json!(8192)
        );
        assert_eq!(
            body["generationConfig"]["temperature"],
            serde_json::json!(0.1)
        );
        assert_eq!(body["tools"], serde_json::json!([{"googleSearch": {}}]));
        assert!(
            body.get("systemInstruction").is_none(),
            "base request carries no systemInstruction"
        );
        assert!(body.get("stream").is_none(), "non-streaming only");
    }

    #[tokio::test]
    async fn t3_not_grounded_retries_once_with_the_forcing_instruction() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(T3_PATH))
            .respond_with(SequentialBodies(
                std::sync::atomic::AtomicUsize::new(0),
                vec![
                    t3_fixture_body("P3b__resp.json"),
                    t3_fixture_body("P3a__resp.json"),
                ],
            ))
            .expect(2)
            .mount(&server)
            .await;
        let client = t3_client(&server, None, None);
        let (content, citations) = client
            .search("what is the latest linux kernel", None)
            .await
            .expect("retry path must succeed");
        assert_eq!(content, t3_fixture_text("P3a__resp.json"));
        assert_eq!(citations, vec![t3_fixture_chunk_uri("P3a__resp.json", 0)]);
        let reqs = server.received_requests().await.expect("requests recorded");
        assert_eq!(reqs.len(), 2, "exactly one retry");
        let first: serde_json::Value = reqs[0].body_json().expect("first body is JSON");
        let second: serde_json::Value = reqs[1].body_json().expect("second body is JSON");
        assert!(
            first.get("systemInstruction").is_none(),
            "first attempt carries no systemInstruction"
        );
        assert_eq!(
            second["systemInstruction"]["parts"][0]["text"],
            serde_json::json!(GROUNDING_RETRY_INSTRUCTION)
        );
        assert_eq!(
            first["contents"], second["contents"],
            "the retry changes ONLY the systemInstruction"
        );
        assert_eq!(first["generationConfig"], second["generationConfig"]);
        assert_eq!(first["tools"], second["tools"]);
    }

    #[tokio::test]
    async fn t3_retry_still_not_grounded_returns_the_no_results_convention() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(T3_PATH))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(t3_fixture_body("P3b__resp.json")),
            )
            .expect(2)
            .mount(&server)
            .await;
        let client = t3_client(&server, None, None);
        let (content, citations) = client
            .search("what is the latest linux kernel", None)
            .await
            .expect("exhausted retries are Ok, not Err");
        assert_eq!(content, "No search results found.");
        assert!(citations.is_empty());
        let reqs = server.received_requests().await.expect("requests recorded");
        assert_eq!(
            reqs.len(),
            2,
            "exactly one retry, then the no-results convention"
        );
    }

    #[tokio::test]
    async fn t3_p4_responses_wire_body_is_an_error_never_a_success() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(T3_PATH))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(t3_fixture_body("P4__resp.json")),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = t3_client(&server, None, None);
        let err = client
            .search("what is the latest linux kernel", None)
            .await
            .expect_err("P4 false-green trap must surface as an error");
        assert_eq!(err.kind, xai_tool_runtime::ToolErrorKind::Execution);
        assert!(
            err.detail.contains("200"),
            "detail carries the status: {}",
            err.detail
        );
        let reqs = server.received_requests().await.expect("requests recorded");
        assert_eq!(reqs.len(), 1, "unparseable 200 bodies are not retried");
    }

    #[tokio::test]
    async fn t3_401_maps_to_unauthorized_with_attribution_and_no_retry() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(T3_PATH))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(serde_json::json!({"error": "invalid api key"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let cb = std::sync::Arc::new(CountingCallback::default());
        let cb_dyn: crate::attribution::SharedAttributionCallback = cb.clone();
        let client = t3_client(&server, None, None).with_attribution_callback(Some(cb_dyn));
        let err = client
            .search("what is the latest linux kernel", None)
            .await
            .expect_err("401 must never surface as success");
        assert_eq!(err.kind, xai_tool_runtime::ToolErrorKind::Unauthorized);
        assert!(
            err.detail.contains("401"),
            "detail carries the status: {}",
            err.detail
        );
        let details = err.details.clone().expect("401 details present");
        assert_eq!(details["tool_id"], serde_json::json!("web_search"));
        assert_eq!(details["status"], serde_json::json!(401));
        let calls = cb.invocations.lock().expect("callback lock");
        assert_eq!(calls.len(), 1, "attribution emitted exactly once");
        assert_eq!(calls[0].0, ToolConsumer::WebSearch);
        let reqs = server.received_requests().await.expect("requests recorded");
        assert_eq!(reqs.len(), 1, "401 is not retried");
    }

    #[tokio::test]
    async fn t3_500_maps_to_execution_error_with_no_retry() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(T3_PATH))
            .respond_with(
                ResponseTemplate::new(500).set_body_json(serde_json::json!({"error": "boom"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = t3_client(&server, None, None);
        let err = client
            .search("what is the latest linux kernel", None)
            .await
            .expect_err("5xx must surface as an error");
        assert_eq!(err.kind, xai_tool_runtime::ToolErrorKind::Execution);
        assert!(
            err.detail.contains("500"),
            "detail carries the status: {}",
            err.detail
        );
        let reqs = server.received_requests().await.expect("requests recorded");
        assert_eq!(reqs.len(), 1, "5xx is not retried");
    }

    #[tokio::test]
    async fn t3_slow_response_aborts_within_the_attempt_timeout() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(T3_PATH))
            .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(10)))
            .mount(&server)
            .await;
        let mut client = t3_client(&server, None, None);
        // Test knob: the production default is gemini::ATTEMPT_TIMEOUT_SECS (60 s).
        client.gemini_attempt_timeout = std::time::Duration::from_secs(2);
        let started = std::time::Instant::now();
        let err = client
            .search("what is the latest linux kernel", None)
            .await
            .expect_err("a 10 s response must trip the 2 s attempt timeout");
        assert_eq!(err.kind, xai_tool_runtime::ToolErrorKind::Execution);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(8),
            "timed out after {} s: the per-attempt timeout is not applied",
            started.elapsed().as_secs_f64()
        );
        let reqs = server.received_requests().await.expect("requests recorded");
        assert_eq!(reqs.len(), 1, "transport errors are not retried");
    }

    #[tokio::test]
    async fn t3_allowlist_falls_back_to_uri_host_when_domain_is_absent() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        // P5's urlContext chunk carries no web.domain (the uri IS the site),
        // plus a chunk on a domain the operator allowlist does not cover.
        let mut body = t3_fixture_body("P5__resp.json");
        let chunks = body
            .pointer_mut("/candidates/0/groundingMetadata/groundingChunks")
            .and_then(serde_json::Value::as_array_mut)
            .expect("P5 carries grounding chunks");
        chunks.push(serde_json::json!({
            "web": {"uri": "https://evil.example/page", "title": "Evil", "domain": "evil.example"}
        }));
        Mock::given(method("POST"))
            .and(path(T3_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
        let client = t3_client(&server, Some(vec!["kernel.org".to_string()]), None);
        let (content, citations) = client
            .search("what is the latest linux kernel", None)
            .await
            .expect("grounded P5 must succeed");
        assert_eq!(
            content,
            t3_fixture_text("P5__resp.json"),
            "model text is never domain-filtered (R4)"
        );
        assert_eq!(
            citations,
            vec!["https://kernel.org".to_string()],
            "allowlist keeps only the kernel.org chunk (uri-host fallback, D7)"
        );
    }

    #[tokio::test]
    async fn t3_blocklist_matches_the_real_domain_not_the_redirect_uri_host() {
        // M1: a googleSearch chunk's uri is the opaque
        // vertexaisearch.cloud.google.com/grounding-api-redirect/… URL. Matching on
        // the uri host would keep this chunk under an excluded kernel.org; the
        // block must match web.domain.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let mut body = t3_fixture_body("P3a__resp.json");
        let chunks = body
            .pointer_mut("/candidates/0/groundingMetadata/groundingChunks")
            .and_then(serde_json::Value::as_array_mut)
            .expect("P3a carries grounding chunks");
        chunks.push(serde_json::json!({
            "web": {"uri": "https://example.com/a", "title": "Example", "domain": "example.com"}
        }));
        Mock::given(method("POST"))
            .and(path(T3_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
        let client = t3_client(&server, None, Some(vec!["kernel.org".to_string()]));
        let (content, citations) = client
            .search("what is the latest linux kernel", None)
            .await
            .expect("grounded P3a must succeed");
        assert!(!content.is_empty());
        assert_eq!(
            citations,
            vec!["https://example.com/a".to_string()],
            "kernel.org chunk dropped by its real domain; example.com kept"
        );
    }

    #[tokio::test]
    async fn t3_blocklist_dropping_every_chunk_keeps_text_with_empty_citations() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(T3_PATH))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(t3_fixture_body("P3a__resp.json")),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = t3_client(&server, None, Some(vec!["kernel.org".to_string()]));
        let (content, citations) = client
            .search("what is the latest linux kernel", None)
            .await
            .expect("a fully filtered grounded result is still Ok");
        assert!(
            !content.is_empty(),
            "text is returned even when every chunk is filtered"
        );
        assert!(citations.is_empty());
    }

    #[tokio::test]
    async fn t3_search_with_titles_dedupes_by_uri_first_seen() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let mut body = t3_fixture_body("P3a__resp.json");
        let chunks = body
            .pointer_mut("/candidates/0/groundingMetadata/groundingChunks")
            .and_then(serde_json::Value::as_array_mut)
            .expect("P3a carries grounding chunks");
        chunks.push(chunks[0].clone()); // duplicate uri: first-seen order must win
        chunks.push(serde_json::json!({
            "web": {"uri": "https://example.com/b", "title": "Example B", "domain": "example.com"}
        }));
        Mock::given(method("POST"))
            .and(path(T3_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
        let client = t3_client(&server, None, None);
        let (content, pairs) = client
            .search_with_titles("what is the latest linux kernel", None)
            .await
            .expect("grounded P3a must succeed");
        assert_eq!(content, t3_fixture_text("P3a__resp.json"));
        assert_eq!(pairs.len(), 2, "duplicate uri collapses to one pair");
        assert_eq!(pairs[0].0, "kernel.org");
        assert_eq!(pairs[0].1, t3_fixture_chunk_uri("P3a__resp.json", 0));
        assert_eq!(pairs[1].0, "Example B");
        assert_eq!(pairs[1].1, "https://example.com/b");
    }

    // ── Phase 2 (apex-ylqz): urlContext wire + GoogleSearch wire pin ──────

    #[tokio::test]
    async fn t3_url_context_wire_uses_url_context_tool_and_zero_temperature() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(T3_PATH))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(t3_fixture_body("P5__resp.json")),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = t3_client(&server, None, None);
        let result = client
            .url_context("fetch https://kernel.org")
            .await
            .expect("urlContext attempt must succeed on a grounded P5 body");
        assert!(result.grounded());
        let reqs = server.received_requests().await.expect("requests recorded");
        assert_eq!(
            reqs.len(),
            1,
            "url_context is a single attempt (R8: no retry hop)"
        );
        let body: serde_json::Value = reqs[0].body_json().expect("request body is JSON");
        assert_eq!(body["tools"], serde_json::json!([{"urlContext": {}}]));
        assert_eq!(
            body["generationConfig"]["temperature"].as_f64(),
            Some(0.0),
            "urlContext is deterministic extraction: temperature 0"
        );
        assert_eq!(
            body["generationConfig"]["maxOutputTokens"].as_u64(),
            Some(8192),
            "production maxOutputTokens pin — the 256 probe budget is NOT it"
        );
        assert!(body.get("systemInstruction").is_none());
    }

    #[tokio::test]
    async fn t3_google_search_request_body_stays_pinned_for_phase_2() {
        // P3(b): parameterizing gemini_attempt must leave the GoogleSearch
        // wire byte-identical to Phase 1 (tools, generationConfig, path).
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(T3_PATH))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(t3_fixture_body("P3a__resp.json")),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = t3_client(&server, None, None);
        client
            .search("what is the latest linux kernel", None)
            .await
            .expect("grounded P3a must succeed");
        let reqs = server.received_requests().await.expect("requests recorded");
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].url.path(), T3_PATH);
        let body: serde_json::Value = reqs[0].body_json().expect("request body is JSON");
        assert_eq!(body["tools"], serde_json::json!([{"googleSearch": {}}]));
        assert_eq!(
            body["generationConfig"]["temperature"].as_f64(),
            Some(0.1),
            "GoogleSearch keeps the 0.1 search temperature"
        );
        assert_eq!(
            body["generationConfig"]["maxOutputTokens"].as_u64(),
            Some(8192)
        );
        assert!(body.get("systemInstruction").is_none());
    }

    #[test]
    fn t2_is_gemini_wire_lowercases_before_the_prefix() {
        fn takes_gemini_wire(model: &str) -> bool {
            let config = WebSearchConfig::Enabled {
                api_key: "test-key".to_string(),
                base_url: "https://api.x.ai/v1".to_string(),
                model: model.to_string(),
                extra_headers: IndexMap::new(),
                alpha_test_key: None,
                allowed_domains: None,
                excluded_domains: None,
            };
            WebSearchClient::new(&config, None)
                .expect("client should build")
                .is_gemini_wire()
        }
        assert!(takes_gemini_wire("gemini-3.8-flash"));
        assert!(takes_gemini_wire("Gemini-3.1-Pro-Preview"));
        assert!(takes_gemini_wire("GEMINI-3.8-FLASH"));
        assert!(takes_gemini_wire("gemini"));
        assert!(!takes_gemini_wire("gpt-5.6-terra"));
        assert!(!takes_gemini_wire("grok-4.5"));
        assert!(!takes_gemini_wire("test-model"));
        assert!(
            !takes_gemini_wire("google/gemini-3.1-pro-preview"),
            "n12 edge: a provider-qualified id falls to the legacy wire"
        );
    }
}
