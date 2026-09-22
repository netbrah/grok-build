//! [`SamplerConfig`] is the per-request configuration handed to the sampler.
//! It deliberately does **not** alias `xai_grok_sampling_types::SamplingConfig`.
//! Aliasing would pull transitive dependencies on shell-specific types (`xai-grok-tools`, etc.) into the sampler crate.

use std::path::PathBuf;

use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use xai_grok_sampling_types::{
    ApiBackend, CompactionAtTokens, CompactionsRemaining, ConversationGroupId,
    DoomLoopRecoveryPolicy, ReasoningEffort,
};

use crate::attribution::SharedAttributionCallback;
use crate::retry::{DEFAULT_MAX_RETRIES, RATE_LIMIT_RETRY_THRESHOLD};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthScheme {
    #[default]
    Bearer,
    XApiKey,
}

/// All knobs that control a single sampling request.
/// Auth is selected separately via `auth_scheme`, while `api_backend` controls only the request/response protocol shape.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SamplerConfig {
    pub api_key: Option<String>,
    pub base_url: String,
    /// Resolved local directory for this model's mTLS client identity.
    #[serde(default)]
    pub mtls_cert_dir: Option<PathBuf>,
    pub model: String,
    pub max_completion_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub api_backend: ApiBackend,
    #[serde(default)]
    pub auth_scheme: AuthScheme,
    /// Extra request headers applied verbatim. The sampler never inspects the URL to derive headers.
    /// Callers (the session) inject proxy auth and other access headers here before constructing the config.
    pub extra_headers: IndexMap<String, String>,
    /// Additional Responses API `include` values not represented by the typed client.
    #[serde(default)]
    pub extra_response_includes: Vec<String>,
    /// Query parameters folded into every request URL (percent-encoded).
    #[serde(default)]
    pub query_params: IndexMap<String, String>,
    /// Header name to environment variable, resolved into request headers at client build and never persisted.
    #[serde(default)]
    pub env_http_headers: IndexMap<String, String>,
    /// Total context window size in tokens.
    /// The sampler does not enforce it; the session uses it for compaction decisions.
    pub context_window: u64,
    pub force_http1: bool,
    pub max_retries: Option<u32>,
    /// Total-attempt ceiling for rate-limited requests.
    /// `None` keeps the actor's [`RetryPolicy::rate_limit_retry_threshold`].
    #[serde(default)]
    pub rate_limit_retry_threshold: Option<u32>,
    pub stream_tool_calls: bool,
    pub idle_timeout_secs: Option<u64>,

    // Reasoning effort
    pub reasoning_effort: Option<ReasoningEffort>,

    /// Provider family for this model (e.g. "xai", "codex"). Gates provider-specific
    /// request patches (Codex instruction roles, max/ultra wire mapping, multi-agent v2).
    /// `None` means xAI-default behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_family: Option<String>,

    /// True when the target backend enforces the strict OpenAI/Azure
    /// Responses input schema: replayed `reasoning` items whose `content`
    /// array is non-empty 400 with `array_above_max_length` (REPLAY-1).
    /// Gates the provider-seam input projection; `false` (default) keeps the
    /// replay byte-identical for lenient (vLLM-dialect) backends.
    #[serde(default)]
    pub strict_responses_input: bool,
    /// True when a family-less (or flagged) row must rewrite
    /// `input_text`/`output_text` content parts to `"text"` for a Responses
    /// shim that rejects the native part types (apex-ayl.77 E2 ruling R-B,
    /// binding flag spec). The flagless path is byte-identical to the
    /// pre-cut status quo.
    #[serde(default)]
    pub normalize_content_types: bool,
    /// Menu-derived wire value for a locally-carried `ultra` (PROACTIVE-ULTRA-1 /
    /// apex-ayl.86, ruling R-MENU-DERIVED): the highest advertised menu tier below
    /// `ultra` (canonical effort order); `None` when the model advertises no menu
    /// (or no non-ultra tier) — the egress then falls back to wire "max".
    /// Resolved by the shell at the .59 seed gate; consumed by
    /// `provider::patch_responses_request` (AXIS 1). Not a flag: it is derived
    /// MENU DATA.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ultra_wire_effort: Option<ReasoningEffort>,

    // Client identity
    pub origin_client: Option<OriginClientInfo>,
    pub client_identifier: Option<String>,
    pub deployment_id: Option<String>,
    pub user_id: Option<String>,
    /// Stable root conversation identifier emitted as `x-grok-conv-group-id`.
    #[serde(default)]
    pub conversation_group_id: Option<ConversationGroupId>,
    pub client_version: Option<String>,

    /// Hook invoked on every 401 response with the bearer that was actually sent on the wire.
    /// Implementations typically compare it against a live credential source to tell a stale token from a server-rejected live one.
    /// `None` (default) is a no-op; the 401 arm still returns `SamplingError::Auth`.
    #[serde(skip)]
    pub attribution_callback: Option<SharedAttributionCallback>,

    /// Resolves a fresh bearer for each request. `None` uses the construction-time `api_key`.
    #[serde(skip)]
    pub bearer_resolver: Option<SharedBearerResolver>,

    #[serde(default)]
    pub supports_backend_search: bool,

    /// Per-model config for the `x-compactions-remaining` header; `None` disables it.
    #[serde(default)]
    pub compactions_remaining: Option<CompactionsRemaining>,

    /// Per-model config for the `x-compaction-at` header; `None` disables it.
    #[serde(default)]
    pub compaction_at_tokens: Option<CompactionAtTokens>,

    /// Messages-wire stable-head cache retention tier ("5m" or "1h");
    /// `None` = the wire default 5m. Validated at the config layer
    /// (unknown values are refused and mapped to None there).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_ttl: Option<String>,

    /// Server-side doom-loop check policy; `None` disables it.
    /// It also absorbs the reported trigger events (unlike environment headers in [`Self::extra_headers`], this gates the client's decode behavior).
    #[serde(default)]
    pub doom_loop_recovery: Option<DoomLoopRecoveryPolicy>,

    /// Per-request header injector (e.g. OTel traceparent). Called in `post()`.
    #[serde(skip)]
    pub header_injector: Option<SharedHeaderInjector>,

    /// Top-k sampling (docs GA L3060); row key `top_k` (u32 scalar),
    /// resolved by the shell (MGW F2, apex-ayl.113). `None` = absent on the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Custom stop strings (docs GA L1246); row key `stop_sequences` is a
    /// comma-separated string, split + trimmed at config resolution
    /// (empty/whitespace → `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    /// Row opt-in: disable parallel tool use (F5, apex-ayl.114); row key
    /// `disable_parallel_tool_use` (bool). Nested into `tool_choice` on the
    /// messages wire by the producer. `None` = toggle absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_parallel_tool_use: Option<bool>,
    /// Row opt-in: per-tool cache breakpoint (F5, apex-ayl.114); the PLURAL
    /// row key `tools_cache_breakpoint` (`off` | `last`) resolves to
    /// `Option<ToolCacheBreakpoint>` at config resolution
    /// (`off`/absent ⇒ `None`). SINGULAR field — do not unify.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_cache_breakpoint: Option<xai_grok_sampling_types::conversation::ToolCacheBreakpoint>,
    /// Config-selected server-tool union members (canonical dated type
    /// strings; the shell row key is the comma-separated STRING
    /// `server_tools`, resolved at config resolution) (MSGW F1, apex-ayl.115).
    /// `None` = no members (absent on the wire).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_tools: Option<Vec<String>>,
    /// Remote MCP server declarations (BETA shape, pre-wire form; the ST
    /// `McpServerDecl` type rides the carrier — no mirror type).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<xai_grok_sampling_types::messages::McpServerDecl>>,
    /// Names an `mcp_servers` entry; pairs with the "mcp_toolset" member
    /// (the config layer hard-refuses a pairing violation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_toolset_server: Option<String>,
}

impl Default for SamplerConfig {
    /// Empty defaults so callers can use `..Default::default()` and new fields don't ripple through every literal site.
    fn default() -> Self {
        Self {
            api_key: None,
            base_url: String::new(),
            mtls_cert_dir: None,
            model: String::new(),
            max_completion_tokens: None,
            temperature: None,
            top_p: None,
            api_backend: ApiBackend::default(),
            auth_scheme: AuthScheme::default(),
            extra_headers: IndexMap::new(),
            extra_response_includes: Vec::new(),
            query_params: IndexMap::new(),
            env_http_headers: IndexMap::new(),
            context_window: 0,
            force_http1: false,
            max_retries: None,
            rate_limit_retry_threshold: None,
            stream_tool_calls: false,
            idle_timeout_secs: None,
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
            cache_ttl: None,
            doom_loop_recovery: None,
            header_injector: None,
            model_family: None,
            strict_responses_input: false,
            normalize_content_types: false,
            ultra_wire_effort: None,
            top_k: None,
            stop_sequences: None,
            disable_parallel_tool_use: None,
            tool_cache_breakpoint: None,
            server_tools: None,
            mcp_servers: None,
            mcp_toolset_server: None,
        }
    }
}

/// Cheap sync read of the current bearer for [`SamplerConfig::bearer_resolver`].
pub trait BearerResolver: Send + Sync + std::fmt::Debug {
    fn current_bearer(&self) -> Option<String>;

    /// Awaited by the client right before it stamps a request; [`Self::current_bearer`] is read afterwards.
    /// A resolver that can renew its bearer does so here when the cached one would not survive the send, so the request never leaves with no credential.
    /// Default: no-op.
    fn prepare_for_send(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }
}

pub type SharedBearerResolver = std::sync::Arc<dyn BearerResolver>;

/// Per-request header injection (e.g. OTel `traceparent`).
pub trait HeaderInjector: Send + Sync + std::fmt::Debug {
    fn inject(&self, headers: &mut reqwest::header::HeaderMap);
}

pub type SharedHeaderInjector = std::sync::Arc<dyn HeaderInjector>;

/// Retry knobs for the sampler's internal transport-error retry loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_retries: u32,
    /// Total-attempt ceiling for rate-limited requests before escalating to the caller.
    /// Lower than `max_retries` because rate-limit waits can be long.
    pub rate_limit_retry_threshold: u32,
    #[serde(default)]
    pub retry_only_before_output: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_MAX_RETRIES,
            rate_limit_retry_threshold: RATE_LIMIT_RETRY_THRESHOLD,
            retry_only_before_output: false,
        }
    }
}

/// Identity of the client that originated the request, used for User-Agent rendering.
/// The shell layer composes this with platform info into a final UA string.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OriginClientInfo {
    pub product: String,
    pub version: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Configs serialized before the field existed must keep deserializing.
    #[test]
    fn config_without_doom_loop_recovery_deserializes_to_none() {
        let mut stripped = serde_json::to_value(SamplerConfig::default()).unwrap();
        let object = stripped.as_object_mut().unwrap();
        object.remove("doom_loop_recovery");
        object.remove("extra_response_includes");
        object.remove("mtls_cert_dir");
        object.remove("rate_limit_retry_threshold");
        let config: SamplerConfig = serde_json::from_value(stripped).unwrap();
        assert!(config.doom_loop_recovery.is_none());
        assert!(config.extra_response_includes.is_empty());
        assert!(config.mtls_cert_dir.is_none());
        assert!(config.rate_limit_retry_threshold.is_none());

        let with_policy = SamplerConfig {
            doom_loop_recovery: Some(DoomLoopRecoveryPolicy {
                max_threshold: 8,
                max_retries: 2,
                ..Default::default()
            }),
            ..Default::default()
        };
        let round_tripped: SamplerConfig =
            serde_json::from_value(serde_json::to_value(&with_policy).unwrap()).unwrap();
        assert_eq!(
            round_tripped.doom_loop_recovery,
            with_policy.doom_loop_recovery
        );
    }
}
