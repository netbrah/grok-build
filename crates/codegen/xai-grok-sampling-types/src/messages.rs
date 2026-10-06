//! Anthropic Messages API (`/v1/messages`) wire types.

use schemars::JsonSchema;
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};

use crate::presence::{RequestPresence, WirePresence};

// ============================================================================
// Request Types
// ============================================================================

/// POST /v1/messages request body
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessagesRequest {
    // REQVALID-1 47b RED-3: fields are private; the getter surface and the
    // MessagesRequestBuilder are the only construction/read paths outside
    // this crate (serde keeps field-level attrs for wire parity).
    model: String,
    messages: Vec<Message>,
    max_tokens: u32,
    #[serde(default, deserialize_with = "crate::serde_helpers::rejecting_null", skip_serializing_if = "Option::is_none")]
    system: Option<SystemParam>,
    #[serde(default, deserialize_with = "crate::serde_helpers::rejecting_null", skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolParam>>,
    #[serde(default, deserialize_with = "crate::serde_helpers::rejecting_null", skip_serializing_if = "Option::is_none")]
    tool_choice: Option<ToolChoiceParam>,
    #[serde(default, deserialize_with = "crate::serde_helpers::rejecting_null", skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(default, deserialize_with = "crate::serde_helpers::rejecting_null", skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(default, deserialize_with = "crate::serde_helpers::rejecting_null", skip_serializing_if = "Option::is_none")]
    top_k: Option<u32>,
    #[serde(default, deserialize_with = "crate::serde_helpers::rejecting_null", skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(default, deserialize_with = "crate::serde_helpers::rejecting_null", skip_serializing_if = "Option::is_none")]
    stop_sequences: Option<Vec<String>>,
    #[serde(default, deserialize_with = "crate::serde_helpers::rejecting_null", skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingConfig>,
    #[serde(default, deserialize_with = "crate::serde_helpers::rejecting_null", skip_serializing_if = "Option::is_none")]
    output_config: Option<OutputConfig>,
    #[serde(default, deserialize_with = "crate::serde_helpers::rejecting_null", skip_serializing_if = "Option::is_none")]
    metadata: Option<Metadata>,
    /// BETA `mcp_servers[]` (MSGW F1, apex-ayl.115): config-declared remote
    /// MCP servers, mapped from the pre-wire `McpServerDecl` form by the
    /// producer (`r#type: "url"`). Absent (None) on every row that does not
    /// declare one — byte-identical pre-cut bodies.
    #[serde(default, deserialize_with = "crate::serde_helpers::rejecting_null", skip_serializing_if = "Option::is_none")]
    mcp_servers: Option<Vec<McpServerParam>>,
}

impl MessagesRequest {
    // REQVALID-1 47b (D-4): the pub getter surface. The RED-3 privacy flip
    // turns these into the only in-crate read path; the fill seams below
    // are the only in-crate write path for the client message-defaults
    // funnel (mirroring `apply_message_defaults` semantics exactly).

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn max_tokens(&self) -> u32 {
        self.max_tokens
    }

    pub fn system(&self) -> Option<&SystemParam> {
        self.system.as_ref()
    }

    pub fn tools(&self) -> Option<&[ToolParam]> {
        self.tools.as_deref()
    }

    pub fn tool_choice(&self) -> Option<&ToolChoiceParam> {
        self.tool_choice.as_ref()
    }

    pub fn temperature(&self) -> Option<f32> {
        self.temperature
    }

    pub fn top_p(&self) -> Option<f32> {
        self.top_p
    }

    pub fn top_k(&self) -> Option<u32> {
        self.top_k
    }

    pub fn stream(&self) -> Option<bool> {
        self.stream
    }

    pub fn stop_sequences(&self) -> Option<&[String]> {
        self.stop_sequences.as_deref()
    }

    pub fn thinking(&self) -> Option<&ThinkingConfig> {
        self.thinking.as_ref()
    }

    pub fn output_config(&self) -> Option<&OutputConfig> {
        self.output_config.as_ref()
    }

    pub fn metadata(&self) -> Option<&Metadata> {
        self.metadata.as_ref()
    }

    pub fn mcp_servers(&self) -> Option<&[McpServerParam]> {
        self.mcp_servers.as_deref()
    }

    /// Message-defaults fill: set `model` when empty (the
    /// `apply_message_defaults` seam — an explicit empty model is
    /// indistinguishable from unset here by design).
    pub fn fill_model(&mut self, model: String) {
        if self.model.is_empty() {
            self.model = model;
        }
    }

    /// Message-defaults fill: set `max_tokens` when 0 (N4: 0 means unset;
    /// the full u32 range incl. 0 is a VALID validated value, the default
    /// funnel simply never leaves it at 0).
    pub fn fill_max_tokens(&mut self, max_tokens: u32) {
        if self.max_tokens == 0 {
            self.max_tokens = max_tokens;
        }
    }

    /// Message-defaults fill: set `temperature` when unset.
    pub fn fill_temperature(&mut self, temperature: Option<f32>) {
        if self.temperature.is_none() {
            self.temperature = temperature;
        }
    }

    /// Message-defaults fill: set `top_p` when unset.
    pub fn fill_top_p(&mut self, top_p: Option<f32>) {
        if self.top_p.is_none() {
            self.top_p = top_p;
        }
    }

    /// Transport seam: the messages-wire funnel always streams
    /// (`create_message_stream_inner` parity — unconditional, not a
    /// default fill).
    pub fn set_stream(&mut self, stream: Option<bool>) {
        self.stream = stream;
    }
}

// ============================================================================
// REQVALID-1 47b (D-4): in-crate construction seam
// ============================================================================

/// REQVALID-1 47b (D-4): the in-crate construction parts for
/// `MessagesRequest`. The fields of `MessagesRequest` are private to this
/// module; the two sanctioned in-crate producers —
/// `request_builder::MessagesRequestBuilder::build` and the trusted
/// pipeline `build_messages_request` — route through
/// [`MessagesRequest::from_parts`]. Construction outside the crate stays a
/// compile error (the T3 D-6 fixture pins it).
#[derive(Debug, Clone, Default)]
pub(crate) struct MessagesRequestParts {
    pub(crate) model: String,
    pub(crate) messages: Vec<Message>,
    pub(crate) max_tokens: u32,
    pub(crate) system: Option<SystemParam>,
    pub(crate) tools: Option<Vec<ToolParam>>,
    pub(crate) tool_choice: Option<ToolChoiceParam>,
    pub(crate) temperature: Option<f32>,
    pub(crate) top_p: Option<f32>,
    pub(crate) top_k: Option<u32>,
    pub(crate) stream: Option<bool>,
    pub(crate) stop_sequences: Option<Vec<String>>,
    pub(crate) thinking: Option<ThinkingConfig>,
    pub(crate) output_config: Option<OutputConfig>,
    pub(crate) metadata: Option<Metadata>,
    pub(crate) mcp_servers: Option<Vec<McpServerParam>>,
}

impl MessagesRequest {
    /// REQVALID-1 47b (D-4): in-crate construction from parts (see
    /// [`MessagesRequestParts`]). The only way in-crate code outside this
    /// module builds a `MessagesRequest`.
    pub(crate) fn from_parts(parts: MessagesRequestParts) -> Self {
        Self {
            model: parts.model,
            messages: parts.messages,
            max_tokens: parts.max_tokens,
            system: parts.system,
            tools: parts.tools,
            tool_choice: parts.tool_choice,
            temperature: parts.temperature,
            top_p: parts.top_p,
            top_k: parts.top_k,
            stream: parts.stream,
            stop_sequences: parts.stop_sequences,
            thinking: parts.thinking,
            output_config: parts.output_config,
            metadata: parts.metadata,
            mcp_servers: parts.mcp_servers,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    /// Spec A0 (L92-99): optional-AND-nullable — Omitted/Null/Value never collapse.
    #[serde(default, skip_serializing_if = "RequestPresence::is_absent")]
    pub effort: RequestPresence<String>,
    /// Spec A0 (L92-99): optional-AND-nullable — Omitted/Null/Value never collapse.
    #[serde(default, skip_serializing_if = "RequestPresence::is_absent")]
    pub format: RequestPresence<OutputFormat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputFormat {
    JsonSchema { schema: serde_json::Value },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: MessageContent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemParam {
    Text(String),
    Blocks(Vec<TextBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextBlock {
    #[serde(rename = "type")]
    pub r#type: String, // always "text"
    pub text: String,
    #[serde(default, deserialize_with = "crate::serde_helpers::rejecting_null", skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

/// A prompt-cache breakpoint marker.
///
/// `ttl` is the extended cache retention tier ("5m" or "1h"); `None`
/// (default) serializes no ttl field — the wire's 5m default. Probes S1/P2
/// (2026-09-15): the live proxy accepts `ttl: "1h"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub r#type: String, // "ephemeral"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
}

impl CacheControl {
    /// The wire-default (5m) breakpoint — serializes no ttl field.
    pub fn ephemeral() -> Self {
        Self {
            r#type: "ephemeral".to_owned(),
            ttl: None,
        }
    }
    /// A breakpoint on the extended retention tier (e.g. "1h").
    pub fn ephemeral_with_ttl(ttl: impl Into<String>) -> Self {
        Self {
            r#type: "ephemeral".to_owned(),
            ttl: Some(ttl.into()),
        }
    }
}

/// Maps a configured retention tier onto the wire: only "1h" emits a ttl
/// field; "5m"/absent = the wire default (no field); any other value is a
/// config typo mapped to the 5m default (the config layer warns).
pub fn cache_control_ttl(ttl: Option<&str>) -> Option<&str> {
    match ttl {
        Some("1h") => Some("1h"),
        None | Some("5m") => None,
        Some(_) => None,
    }
}

/// `skip_serializing_if` guard for `ContentBlock::ToolResult.is_error`:
/// the wire omits the field on success (never emits `"is_error": false`).
fn is_false(value: &bool) -> bool {
    !value
}

/// Content blocks used in both requests and responses
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    Image {
        source: ImageSource,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    ToolResult {
        tool_use_id: String,
        content: ToolResultContent,
        /// Provenance: fresh — spec L3898-3902 (GAP-B4 is_error on failed
        /// tool results). Wire invariant: emits `"is_error": true` or omits
        /// the field; never serializes `"is_error": false`.
        #[serde(default, skip_serializing_if = "is_false")]
        is_error: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    Thinking {
        thinking: String,
        /// The start block may omit the signature (the real API delivers it
        /// via `signature_delta`; xli's wire type carries no signature field
        /// at all), so a missing one parses as empty — same house pattern as
        /// the MW-2 R9 `id` leniency. A missing `thinking` stays fatal.
        #[serde(default)]
        signature: String,
    },
    /// Encrypted reasoning the model chose to redact: an opaque `data` blob, never plaintext.
    /// Parsed so a stream carrying one deserializes instead of failing the whole event parse; request-building and the sampler never construct one.
    RedactedThinking { data: String },
    /// A tool handle the vendor names inside a `tool_result` select-list without
    /// advertising it in `tools[]` (CC5 deferred-tool lifecycle). Carries the
    /// handle only, never content; `tool_name` is the verbatim wire key.
    ToolReference { tool_name: String },
    /// A content block kind this build does not model (R1 forward-compat).
    /// Stream-decode ONLY: the `MessageStreamEvent` parse site maps an unknown
    /// `content_block` kind to this variant so the stream transform opens a
    /// swallowed phantom block (spec D3) instead of failing the frame — which
    /// would poison the block's later delta/stop into the fatal unopened-index
    /// classes (spec G3). `kind` carries the verbatim wire `type` string for
    /// logging. Never constructed on the request side or by the non-stream
    /// `MessagesResponse` parse (its `Deserialize` impl stays strict over the
    /// seven known kinds above), and never produced on a serialization path.
    Unknown { kind: String },
}

impl<'de> Deserialize<'de> for ContentBlock {
    /// Strict over the seven known kinds: the `Unknown` variant is a
    /// stream-decode-only construct (see its doc), so neither the non-stream
    /// response parse nor any request-side parse may produce it. An unknown
    /// `type` or a known kind missing a required field is a fatal
    /// deserialization error at every call site except the `MessageStreamEvent`
    /// parse site, which maps unknown kinds to the phantom variant (R1/D3).
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Mirror of the known variants (kept in sync with the enum above):
        // the derive on the public enum cannot exclude the stream-only
        // `Unknown` variant from deserialization, so the strict parse runs
        // against this mirror.
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum StrictBlock {
            Text {
                text: String,
                cache_control: Option<CacheControl>,
            },
            Image {
                source: ImageSource,
                cache_control: Option<CacheControl>,
            },
            ToolUse {
                id: String,
                name: String,
                input: serde_json::Value,
                cache_control: Option<CacheControl>,
            },
            ToolResult {
                tool_use_id: String,
                content: ToolResultContent,
                #[serde(default)]
                is_error: bool,
                cache_control: Option<CacheControl>,
            },
            Thinking {
                thinking: String,
                #[serde(default)]
                signature: String,
            },
            RedactedThinking {
                data: String,
            },
            ToolReference {
                tool_name: String,
            },
        }
        let block = StrictBlock::deserialize(deserializer)?;
        Ok(match block {
            StrictBlock::Text {
                text,
                cache_control,
            } => ContentBlock::Text {
                text,
                cache_control,
            },
            StrictBlock::Image {
                source,
                cache_control,
            } => ContentBlock::Image {
                source,
                cache_control,
            },
            StrictBlock::ToolUse {
                id,
                name,
                input,
                cache_control,
            } => ContentBlock::ToolUse {
                id,
                name,
                input,
                cache_control,
            },
            StrictBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                cache_control,
            } => ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                cache_control,
            },
            StrictBlock::Thinking {
                thinking,
                signature,
            } => ContentBlock::Thinking {
                thinking,
                signature,
            },
            StrictBlock::RedactedThinking { data } => ContentBlock::RedactedThinking { data },
            StrictBlock::ToolReference { tool_name } => ContentBlock::ToolReference { tool_name },
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

/// Tool definition (Anthropic Messages API format)
/// A `tools[]` entry: the flat custom tool (today's wire shape, byte-identical)
/// or a dated server-tool member (GA union L1442–3044 + BETA mcp_toolset).
/// Untagged: the Custom variant serializes exactly as the pre-cut flat struct;
/// the Server variant carries its `type` tag (MSGW F1, apex-ayl.115).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolParam {
    Custom(ToolCustom),
    Server(ToolServer),
}

/// The pre-cut flat custom tool, renamed (fields unchanged, incl. F5's tail
/// `cache_control`; apex-pwr appends `defer_loading` after it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCustom {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
    /// Per-tool cache breakpoint (docs: CacheControlEphemeral on the tool entry; tools hash into
    /// the prefix earlier than system). Set by the producer ONLY for the last tool when the row
    /// opts in via `tools_cache_breakpoint = "last"` (§3.5) — spends the free 4th marker slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
    /// Deferred-tool class marker (CC5 `tools[].defer_loading`): `Some(true)` on a tool the
    /// vendor holds back until a `tool_reference` promotes it (a promoted entry keeps the
    /// flag). A class marker, not a per-tool boolean — a static tool omits the key, so this
    /// must never serialize `false`/`null`. Last field: existing key order is byte-untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defer_loading: Option<bool>,
}

/// Dated server-tool members (docs GA create.md L1442–3044 + BETA
/// mcp_toolset). Internally tagged on `type`; 18 of the 21 members carry the
/// fixed docs `name` literal; the 3 nameless (browser/computer toolset,
/// mcp_toolset) have NO `name` field (FIX-PASS 1 R1). Field transcription
/// from the pinned `948d49c` docs snapshot (SDD §2.1/§3.1): every
/// docs-listed non-required field is `Option<…>` + skip-when-None;
/// Value-passthrough fields (`input_examples`/`citations`/`url_sources`/
/// `configs`) stay `serde_json::Value` until the future-knob lane types them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolServer {
    #[serde(rename = "bash_20250124")]
    Bash20250124 {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_callers: Option<Vec<AllowedCaller>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defer_loading: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_examples: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
    },
    #[serde(rename = "code_execution_20250522")]
    CodeExecution20250522 {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_callers: Option<Vec<AllowedCaller>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defer_loading: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
    },
    #[serde(rename = "code_execution_20250825")]
    CodeExecution20250825 {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_callers: Option<Vec<AllowedCaller>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defer_loading: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
    },
    #[serde(rename = "code_execution_20260120")]
    CodeExecution20260120 {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_callers: Option<Vec<AllowedCaller>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defer_loading: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
    },
    #[serde(rename = "code_execution_20260521")]
    CodeExecution20260521 {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_callers: Option<Vec<AllowedCaller>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defer_loading: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
    },
    /// Nameless member (docs GA :1673): type/cache_control/configs only.
    #[serde(rename = "browser_toolset_20260801")]
    BrowserToolset20260801 {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        configs: Option<serde_json::Value>,
    },
    #[serde(rename = "memory_20250818")]
    Memory20250818 {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_callers: Option<Vec<AllowedCaller>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defer_loading: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_examples: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
    },
    /// Nameless member (docs GA :2105): type/cache_control/configs only.
    #[serde(rename = "computer_toolset_20260801")]
    ComputerToolset20260801 {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        configs: Option<serde_json::Value>,
    },
    #[serde(rename = "text_editor_20250124")]
    TextEditor20250124 {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_callers: Option<Vec<AllowedCaller>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defer_loading: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_examples: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
    },
    #[serde(rename = "text_editor_20250429")]
    TextEditor20250429 {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_callers: Option<Vec<AllowedCaller>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defer_loading: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_examples: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
    },
    #[serde(rename = "text_editor_20250728")]
    TextEditor20250728 {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_callers: Option<Vec<AllowedCaller>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defer_loading: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_examples: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_characters: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
    },
    #[serde(rename = "web_search_20250305")]
    WebSearch20250305 {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_callers: Option<Vec<AllowedCaller>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_domains: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blocked_domains: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defer_loading: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_uses: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_location: Option<UserLocation>,
    },
    #[serde(rename = "web_fetch_20250910")]
    WebFetch20250910 {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_callers: Option<Vec<AllowedCaller>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_domains: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blocked_domains: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        citations: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defer_loading: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_content_tokens: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_uses: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url_sources: Option<serde_json::Value>,
    },
    #[serde(rename = "web_search_20260209")]
    WebSearch20260209 {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_callers: Option<Vec<AllowedCaller>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_domains: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blocked_domains: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defer_loading: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_uses: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_location: Option<UserLocation>,
    },
    #[serde(rename = "web_fetch_20260209")]
    WebFetch20260209 {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_callers: Option<Vec<AllowedCaller>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_domains: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blocked_domains: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        citations: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defer_loading: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_content_tokens: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_uses: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url_sources: Option<serde_json::Value>,
    },
    #[serde(rename = "web_fetch_20260309")]
    WebFetch20260309 {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_callers: Option<Vec<AllowedCaller>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_domains: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blocked_domains: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        citations: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defer_loading: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_content_tokens: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_uses: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url_sources: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        use_cache: Option<bool>,
    },
    #[serde(rename = "web_search_20260318")]
    WebSearch20260318 {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_callers: Option<Vec<AllowedCaller>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_domains: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blocked_domains: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defer_loading: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_uses: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response_inclusion: Option<SearchResponseInclusion>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_location: Option<UserLocation>,
    },
    #[serde(rename = "web_fetch_20260318")]
    WebFetch20260318 {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_callers: Option<Vec<AllowedCaller>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_domains: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blocked_domains: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        citations: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defer_loading: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_content_tokens: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_uses: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response_inclusion: Option<SearchResponseInclusion>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url_sources: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        use_cache: Option<bool>,
    },
    #[serde(rename = "tool_search_tool_bm25_20251119")]
    ToolSearchToolBm2520251119 {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_callers: Option<Vec<AllowedCaller>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defer_loading: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
    },
    #[serde(rename = "tool_search_tool_regex_20251119")]
    ToolSearchToolRegex20251119 {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed_callers: Option<Vec<AllowedCaller>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defer_loading: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
    },
    /// BETA member (docs BETA :4014) — nameless: `mcp_server_name` (required,
    /// producer-filled from the row's `mcp_toolset_server`; the config layer
    /// hard-refuses a bare selection) instead of a `name` literal.
    #[serde(rename = "mcp_toolset")]
    McpToolset {
        mcp_server_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        configs: Option<std::collections::BTreeMap<String, serde_json::Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default_config: Option<serde_json::Value>,
    },
}

/// Caller of a server tool (docs shared shape): `direct` or a dated
/// code-execution type string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AllowedCaller {
    Direct,
    #[serde(rename = "code_execution_20250825")]
    CodeExecution20250825,
    #[serde(rename = "code_execution_20260120")]
    CodeExecution20260120,
    #[serde(rename = "code_execution_20260521")]
    CodeExecution20260521,
}

/// The user's approximate location (web_search `user_location`; docs: the
/// `type` member is the fixed literal `"approximate"`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserLocation {
    pub r#type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

/// Whether the search results are included in the response
/// (web_search_20260318+ `response_inclusion`; docs: `full` | `excluded`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchResponseInclusion {
    Full,
    Excluded,
}

/// BETA `mcp_servers[]` wire entry (BETA create.md L1901+; docs: maxItems 20).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerParam {
    #[serde(rename = "type")]
    pub r#type: String,
    pub name: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_configuration: Option<serde_json::Value>,
}

/// A config-declared MCP server (pre-wire form; the producer maps it to
/// [`McpServerParam`] with `r#type: "url"`). `JsonSchema`: the shell row
/// surface (`ConfigModelOverride`, `ModelsConfig`) carries this type and
/// derives `JsonSchema` for the committed config schema artifact.
/// `PartialEq`: the shell 47b resolver replays the row value through
/// `debug_assert_eq!` (compile-forced, MSGW F1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct McpServerDecl {
    pub name: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_configuration: Option<serde_json::Value>,
}

/// Total string→variant map over the 21 known type strings (the config
/// layer validates against the same whitelist). Default member forms:
/// `{type, name}` for the 18 named members; `{type}` only for the 2
/// nameless GA toolsets (mcp_toolset carries the empty producer-filled
/// `mcp_server_name` placeholder — the config layer hard-refuses a bare
/// selection upstream, so the placeholder never reaches the wire).
pub fn server_tool_from_type(t: &str) -> Option<ToolServer> {
    use ToolServer::*;
    Some(match t {
        "bash_20250124" => Bash20250124 {
            name: "bash".to_owned(),
            allowed_callers: None,
            cache_control: None,
            defer_loading: None,
            input_examples: None,
            strict: None,
        },
        "code_execution_20250522" => CodeExecution20250522 {
            name: "code_execution".to_owned(),
            allowed_callers: None,
            cache_control: None,
            defer_loading: None,
            strict: None,
        },
        "code_execution_20250825" => CodeExecution20250825 {
            name: "code_execution".to_owned(),
            allowed_callers: None,
            cache_control: None,
            defer_loading: None,
            strict: None,
        },
        "code_execution_20260120" => CodeExecution20260120 {
            name: "code_execution".to_owned(),
            allowed_callers: None,
            cache_control: None,
            defer_loading: None,
            strict: None,
        },
        "code_execution_20260521" => CodeExecution20260521 {
            name: "code_execution".to_owned(),
            allowed_callers: None,
            cache_control: None,
            defer_loading: None,
            strict: None,
        },
        "browser_toolset_20260801" => BrowserToolset20260801 {
            cache_control: None,
            configs: None,
        },
        "memory_20250818" => Memory20250818 {
            name: "memory".to_owned(),
            allowed_callers: None,
            cache_control: None,
            defer_loading: None,
            input_examples: None,
            strict: None,
        },
        "computer_toolset_20260801" => ComputerToolset20260801 {
            cache_control: None,
            configs: None,
        },
        "text_editor_20250124" => TextEditor20250124 {
            name: "str_replace_editor".to_owned(),
            allowed_callers: None,
            cache_control: None,
            defer_loading: None,
            input_examples: None,
            strict: None,
        },
        "text_editor_20250429" => TextEditor20250429 {
            name: "str_replace_based_edit_tool".to_owned(),
            allowed_callers: None,
            cache_control: None,
            defer_loading: None,
            input_examples: None,
            strict: None,
        },
        "text_editor_20250728" => TextEditor20250728 {
            name: "str_replace_based_edit_tool".to_owned(),
            allowed_callers: None,
            cache_control: None,
            defer_loading: None,
            input_examples: None,
            max_characters: None,
            strict: None,
        },
        "web_search_20250305" => WebSearch20250305 {
            name: "web_search".to_owned(),
            allowed_callers: None,
            allowed_domains: None,
            blocked_domains: None,
            cache_control: None,
            defer_loading: None,
            max_uses: None,
            strict: None,
            user_location: None,
        },
        "web_fetch_20250910" => WebFetch20250910 {
            name: "web_fetch".to_owned(),
            allowed_callers: None,
            allowed_domains: None,
            blocked_domains: None,
            cache_control: None,
            citations: None,
            defer_loading: None,
            max_content_tokens: None,
            max_uses: None,
            strict: None,
            url_sources: None,
        },
        "web_search_20260209" => WebSearch20260209 {
            name: "web_search".to_owned(),
            allowed_callers: None,
            allowed_domains: None,
            blocked_domains: None,
            cache_control: None,
            defer_loading: None,
            max_uses: None,
            strict: None,
            user_location: None,
        },
        "web_fetch_20260209" => WebFetch20260209 {
            name: "web_fetch".to_owned(),
            allowed_callers: None,
            allowed_domains: None,
            blocked_domains: None,
            cache_control: None,
            citations: None,
            defer_loading: None,
            max_content_tokens: None,
            max_uses: None,
            strict: None,
            url_sources: None,
        },
        "web_fetch_20260309" => WebFetch20260309 {
            name: "web_fetch".to_owned(),
            allowed_callers: None,
            allowed_domains: None,
            blocked_domains: None,
            cache_control: None,
            citations: None,
            defer_loading: None,
            max_content_tokens: None,
            max_uses: None,
            strict: None,
            url_sources: None,
            use_cache: None,
        },
        "web_search_20260318" => WebSearch20260318 {
            name: "web_search".to_owned(),
            allowed_callers: None,
            allowed_domains: None,
            blocked_domains: None,
            cache_control: None,
            defer_loading: None,
            max_uses: None,
            response_inclusion: None,
            strict: None,
            user_location: None,
        },
        "web_fetch_20260318" => WebFetch20260318 {
            name: "web_fetch".to_owned(),
            allowed_callers: None,
            allowed_domains: None,
            blocked_domains: None,
            cache_control: None,
            citations: None,
            defer_loading: None,
            max_content_tokens: None,
            max_uses: None,
            response_inclusion: None,
            strict: None,
            url_sources: None,
            use_cache: None,
        },
        "tool_search_tool_bm25_20251119" => ToolSearchToolBm2520251119 {
            name: "tool_search_tool_bm25".to_owned(),
            allowed_callers: None,
            cache_control: None,
            defer_loading: None,
            strict: None,
        },
        "tool_search_tool_regex_20251119" => ToolSearchToolRegex20251119 {
            name: "tool_search_tool_regex".to_owned(),
            allowed_callers: None,
            cache_control: None,
            defer_loading: None,
            strict: None,
        },
        "mcp_toolset" => McpToolset {
            mcp_server_name: String::new(),
            cache_control: None,
            configs: None,
            default_config: None,
        },
        _ => return None,
    })
}

/// Tool choice (Anthropic Messages API format, GA create.md L1328–1376).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolChoiceParam {
    Auto {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        disable_parallel_tool_use: Option<bool>,
    },
    Any {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        disable_parallel_tool_use: Option<bool>,
    },
    Tool {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        disable_parallel_tool_use: Option<bool>,
    },
    /// GA `{"type":"none"}` — the model may not use tools (docs L1372–1376; no dptu member on this variant).
    None,
}

/// Three modes per the Anthropic Messages API: Adaptive: 4.6+ models, API decides budget; Enabled: 4.0-4.5 models,
/// explicit budget_tokens; Disabled: pre-thinking models or thinking_budget=0.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingDisplay {
    Omitted,
    Summarized,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThinkingConfig {
    Enabled {
        budget_tokens: u32,
    },
    Adaptive {
        // Newer thinking-capable models omit thinking content unless display = "summarized".
        // Older models ignore this field; skipping `None` keeps the old wire shape
        #[serde(default, deserialize_with = "crate::serde_helpers::rejecting_null", skip_serializing_if = "Option::is_none")]
        display: Option<ThinkingDisplay>,
    },
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metadata {
    /// Spec A0 (L92-99): optional-AND-nullable — Omitted/Null/Value never collapse.
    #[serde(default, skip_serializing_if = "RequestPresence::is_absent")]
    pub user_id: RequestPresence<String>,
}

// ============================================================================
// Response Types
// ============================================================================

/// Non-streaming response from POST /v1/messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagesResponse {
    /// Compatibility metadata only: some providers omit the response id on
    /// `message_start`, so a missing id parses as empty (""), never as an
    /// error. `tool_use` ids stay required — they are semantic.
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type")]
    pub r#type: String, // "message"
    pub role: String, // "assistant"
    pub content: Vec<ContentBlock>,
    pub model: String,
    pub stop_reason: Option<StopReason>,
    pub usage: MessagesUsage,
    /// Spec A0 L107 (Q6): required-nullable — Missing/Null/Value never collapse; the delta rule is retain-on-omission/null (§4.3).
    #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
    pub container: WirePresence<serde_json::Value>,
    /// Spec A0 L108 (Q7): required-nullable — preserved in the start event; the terminal delta REPLACES this field, never overlays it (L110, Q9).
    #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
    pub stop_details: WirePresence<StopDetails>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    ToolUse,
    StopSequence,
    Refusal,
    PauseTurn,
    ModelContextWindowExceeded,
    /// Catch-all so a new server-side stop reason never fails the terminal `message_delta` parse and discards an already-streamed response.
    /// Preserves the wire string for logging and faithful re-serialization.
    /// Must stay the LAST variant: serde tries the tagged variants above first.
    #[serde(untagged)]
    Unknown(String),
}

impl StopReason {
    /// The verbatim wire string, derived from the serde `snake_case` renames so it cannot drift from the wire contract.
    /// `Unknown` yields its inner string unchanged.
    pub fn wire_str(&self) -> String {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::String(s)) => s,
            other => {
                debug_assert!(
                    false,
                    "StopReason must serialize to a string, got {other:?}"
                );
                "end_turn".to_string()
            }
        }
    }
}

/// Breakdown of output tokens by category (wire shape: pin
/// wirejig/refs/anthropic@d3d5028 spec/src/resources/messages/messages.ts:1290).
/// `output_tokens` remains the authoritative total; this is a read-only
/// decomposition for observability.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputTokensDetails {
    /// Output tokens the model generated as internal reasoning (thinking).
    #[serde(default)]
    pub thinking_tokens: u32,
}

/// Breakdown of cached input tokens by TTL (pin
/// wirejig/refs/anthropic@d3d5028 spec/src/resources/messages/messages.ts:310-321).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheCreation {
    #[serde(default)]
    pub ephemeral_5m_input_tokens: u32,
    #[serde(default)]
    pub ephemeral_1h_input_tokens: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessagesUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Spec A0 L109 (Q8): required-nullable — preserve Missing/Null/Value independently (was bare u32+default: missing→0 AND null→error, second collapse class).
    #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
    pub cache_creation_input_tokens: WirePresence<u32>,
    /// Spec A0 L109 (Q8): required-nullable — preserve Missing/Null/Value independently (was bare u32+default: missing→0 AND null→error, second collapse class).
    #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
    pub cache_read_input_tokens: WirePresence<u32>,
    /// Spec A0 L109 (Q8): required-nullable — preserve Missing/Null/Value independently (pin messages.ts:2423; present on proxy responses carrying a thinking decomposition).
    #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
    pub output_tokens_details: WirePresence<OutputTokensDetails>,
    /// Spec A0 L109 (Q8) + L4257: required-nullable, start-only — not a delta field; retains its exact start state (pin messages.ts:2388).
    #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
    pub cache_creation: WirePresence<CacheCreation>,
}

// ============================================================================
// Streaming Event Types
// ============================================================================

/// Top-level streaming event (SSE `type` field determines variant)
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageStreamEvent {
    MessageStart {
        message: MessagesResponse,
    },
    MessageDelta {
        delta: MessageDeltaBody,
        usage: MessageDeltaUsage,
    },
    MessageStop,
    ContentBlockStart {
        index: u32,
        content_block: ContentBlock,
    },
    ContentBlockDelta {
        index: u32,
        delta: StreamDelta,
    },
    ContentBlockStop {
        index: u32,
    },
    Ping,
    Error {
        error: StreamError,
    },
}

/// Wire tag strings of the known top-level event types (serde `snake_case`
/// renames of the variants above). A payload carrying any other tag is an
/// unknown event type: the R1 table maps it to `Ping` (liveness,
/// forward-compat, never fatal).
const KNOWN_EVENT_TAGS: &[&str] = &[
    "message_start",
    "message_delta",
    "message_stop",
    "content_block_start",
    "content_block_delta",
    "content_block_stop",
    "ping",
    "error",
];

/// Wire tag strings of the known `content_block` kinds (serde `snake_case`
/// renames of the `ContentBlock` variants). An unknown kind keeps the
/// `content_block_start` event and maps the block to the phantom
/// `ContentBlock::Unknown` variant (R1/D3).
///
/// This is the *stream* kind mirror and it is deliberately narrower than
/// `ContentBlock`: `tool_reference` (the seventh modelled kind) is a
/// request-side block — CC5 carries it in `req-*.json` (10/10 turns) and in 0 of
/// the 10 `resp-*.sse` — so a stream that names it phantom-opens rather than
/// models it. Consequence: the sampler's `ContentBlock::ToolReference` stream
/// arms (`stream/messages.rs:402`, `stream_classify.rs:155`) are compile-only
/// exhaustivity while this list excludes the kind (the phantom `Unknown` and a
/// known-inert block converge downstream). Listing the kind here would also put
/// it on the strict path, where a malformed frame is fatal, so it must not join
/// without a response-side wire capture and flips
/// `streamed_tool_reference_block_phantom_opens_as_unknown_today` in that same
/// commit.
const KNOWN_BLOCK_KINDS: &[&str] = &[
    "text",
    "image",
    "tool_use",
    "tool_result",
    "thinking",
    "redacted_thinking",
];

/// Wire tag strings of the known `StreamDelta` subtypes (serde `snake_case`
/// renames of its variants). An unknown subtype maps the whole
/// `content_block_delta` event to `Ping` (R1).
const KNOWN_DELTA_SUBTYPES: &[&str] = &[
    "text_delta",
    "input_json_delta",
    "thinking_delta",
    "signature_delta",
];

impl<'de> Deserialize<'de> for MessageStreamEvent {
    /// The single production parse site (spec D2): the client decodes every
    /// SSE data payload against this impl, so the R1 forward-compat table
    /// lives here and nowhere else.
    ///
    /// Target semantics (MW-3 spec R1, binding):
    /// - unknown top-level event type -> `Ping` (liveness, never fatal);
    /// - unknown `content_block` kind in `content_block_start` -> phantom-open
    ///   (the event survives with `ContentBlock::Unknown`; the transform
    ///   opens-and-swallow the block — spec D3);
    /// - unknown delta subtype in `content_block_delta` -> `Ping`;
    /// - KNOWN event type missing a required field -> FATAL (wire corruption
    ///   must not be hidden; surfaces as `SamplingError::Serialization` at
    ///   the client);
    /// - `ping` -> `Ping`; well-known shapes parse strictly as before.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Stage 1: capture the wire tag plus the remaining payload fields, so
        // the known-vs-unknown decision happens BEFORE any strict variant
        // parse (an unknown tag must never surface as a fatal serde error).
        #[derive(Deserialize)]
        struct EventProbe {
            #[serde(rename = "type")]
            tag: String,
            #[serde(flatten)]
            fields: serde_json::Map<String, serde_json::Value>,
        }
        let EventProbe { tag, fields } = EventProbe::deserialize(deserializer)?;

        if !KNOWN_EVENT_TAGS.contains(&tag.as_str()) {
            // R1: unknown top-level event type -> Ping.
            return Ok(MessageStreamEvent::Ping);
        }

        // Stage 2: known tag — strict-parse the variant payload.
        let mut payload = fields;
        payload.insert("type".to_owned(), serde_json::Value::String(tag));
        let value = serde_json::Value::Object(payload);

        #[derive(Deserialize)]
        struct MessageStartWire {
            message: MessagesResponse,
        }
        #[derive(Deserialize)]
        struct MessageDeltaWire {
            delta: MessageDeltaBody,
            usage: MessageDeltaUsage,
        }
        #[derive(Deserialize)]
        struct ContentBlockStartWire {
            index: u32,
            content_block: serde_json::Value,
        }
        #[derive(Deserialize)]
        struct ContentBlockDeltaWire {
            index: u32,
            delta: serde_json::Value,
        }
        #[derive(Deserialize)]
        struct ContentBlockStopWire {
            index: u32,
        }
        #[derive(Deserialize)]
        struct ErrorWire {
            error: StreamError,
        }

        match value.as_str_tag() {
            "message_start" => {
                let MessageStartWire { message } =
                    serde::Deserialize::deserialize(value).map_err(serde::de::Error::custom)?;
                Ok(MessageStreamEvent::MessageStart { message })
            }
            "message_delta" => {
                let MessageDeltaWire { delta, usage } =
                    serde::Deserialize::deserialize(value).map_err(serde::de::Error::custom)?;
                Ok(MessageStreamEvent::MessageDelta { delta, usage })
            }
            "message_stop" => Ok(MessageStreamEvent::MessageStop),
            "content_block_start" => {
                let ContentBlockStartWire {
                    index,
                    content_block,
                } = serde::Deserialize::deserialize(value).map_err(serde::de::Error::custom)?;
                let kind = content_block
                    .get("type")
                    .and_then(serde_json::Value::as_str);
                let content_block = if KNOWN_BLOCK_KINDS.contains(&kind.unwrap_or_default()) {
                    // Known kind: strict (a known shape missing a required
                    // field stays fatal — wire corruption must not be hidden).
                    serde::Deserialize::deserialize(content_block)
                        .map_err(serde::de::Error::custom)?
                } else {
                    // R1/D3: unknown kind -> phantom (index survives; the
                    // transform opens-and-swallow the block).
                    ContentBlock::Unknown {
                        kind: kind.unwrap_or_default().to_owned(),
                    }
                };
                Ok(MessageStreamEvent::ContentBlockStart {
                    index,
                    content_block,
                })
            }
            "content_block_delta" => {
                let ContentBlockDeltaWire { index, delta } =
                    serde::Deserialize::deserialize(value).map_err(serde::de::Error::custom)?;
                let subtype = delta.get("type").and_then(serde_json::Value::as_str);
                if !KNOWN_DELTA_SUBTYPES.contains(&subtype.unwrap_or_default()) {
                    // R1: unknown delta subtype -> Ping (liveness).
                    return Ok(MessageStreamEvent::Ping);
                }
                // Known subtype: strict (wrong-typed/missing fields stay fatal).
                let delta =
                    serde::Deserialize::deserialize(delta).map_err(serde::de::Error::custom)?;
                Ok(MessageStreamEvent::ContentBlockDelta { index, delta })
            }
            "content_block_stop" => {
                let ContentBlockStopWire { index } =
                    serde::Deserialize::deserialize(value).map_err(serde::de::Error::custom)?;
                Ok(MessageStreamEvent::ContentBlockStop { index })
            }
            "ping" => Ok(MessageStreamEvent::Ping),
            "error" => {
                let ErrorWire { error } =
                    serde::Deserialize::deserialize(value).map_err(serde::de::Error::custom)?;
                Ok(MessageStreamEvent::Error { error })
            }
            // Unreachable: KNOWN_EVENT_TAGS is the exact tag set above.
            other => Err(serde::de::Error::custom(format!(
                "unhandled known event tag {other:?}"
            ))),
        }
    }
}

/// Helper for the match dispatch: the re-attached `type` field.
trait StrTag {
    fn as_str_tag(&self) -> &str;
}
impl StrTag for serde_json::Value {
    fn as_str_tag(&self) -> &str {
        self.get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageDeltaBody {
    pub stop_reason: Option<StopReason>,
    /// The stop sequence that was matched, present only when `stop_reason == "stop_sequence"`; `None` otherwise.
    /// Consumers echo it on the Messages API `message.stop_sequence`.
    /// Optional so its absence never fails the terminal parse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<String>,
    /// Spec A0 L110 (Q9): terminal replacement — Missing → missing completed field, Null → explicit null, Value → that value; none retains the start value.
    #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
    pub stop_details: WirePresence<StopDetails>,
    /// Spec A0 L105 (Q4): omission and null retain the start container; a non-null value replaces it.
    #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
    pub container: WirePresence<serde_json::Value>,
}

/// Detail for a terminal `message_delta`, e.g. `{"type":"refusal","category":"frontier_llm","explanation":"..."}`.
/// All fields optional so an unknown shape never fails the terminal parse.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StopDetails {
    #[serde(rename = "type", default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub explanation: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageDeltaUsage {
    pub output_tokens: u32,
    /// Spec A0 L104 (Q3): accept omission and explicit null and retain the corresponding `message_start` value; a present non-null value replaces it.
    #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
    pub input_tokens: WirePresence<u32>,
    /// Spec A0 L104 (Q3): accept omission and explicit null and retain the corresponding `message_start` value; a present non-null value replaces it.
    #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
    pub cache_read_input_tokens: WirePresence<u32>,
    /// Spec A0 L104 (Q3): accept omission and explicit null and retain the corresponding `message_start` value; a present non-null value replaces it.
    #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
    pub cache_creation_input_tokens: WirePresence<u32>,
    /// Spec A0 L104 (Q3): accept omission and explicit null and retain the corresponding `message_start` value; a present non-null value replaces it (pin messages.ts:2423; terminal-delta thinking decomposition).
    #[serde(default, skip_serializing_if = "WirePresence::is_absent")]
    pub output_tokens_details: WirePresence<OutputTokensDetails>,
}

/// Content delta within a content_block_delta event
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamDelta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
    ThinkingDelta { thinking: String },
    SignatureDelta { signature: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamError {
    #[serde(rename = "type")]
    pub r#type: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ANTHROPIC-WIRE-1 (cut 3, probes S1/P2 2026-09-15): the wire
    /// `cache_control.ttl` extension. The default 5m tier serializes NO ttl
    /// field (byte-identical to the pre-cut wire), "1h" emits exactly
    /// `{"type":"ephemeral","ttl":"1h"}`, and absent/unknown tiers map to
    /// the 5m default at the config boundary (`cache_control_ttl`).
    #[test]
    fn cache_control_ttl_serialization_and_parse() {
        let json = serde_json::to_value(CacheControl::ephemeral()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "type": "ephemeral" }),
            "the default tier must serialize no ttl key"
        );
        let json = serde_json::to_value(CacheControl::ephemeral_with_ttl("1h")).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "type": "ephemeral", "ttl": "1h" })
        );

        let parsed: CacheControl = serde_json::from_str(r#"{"type":"ephemeral"}"#).unwrap();
        assert_eq!(parsed.ttl, None, "absent ttl must parse to None");
        let parsed: CacheControl =
            serde_json::from_str(r#"{"type":"ephemeral","ttl":"1h"}"#).unwrap();
        assert_eq!(parsed.ttl.as_deref(), Some("1h"));

        assert_eq!(cache_control_ttl(Some("1h")), Some("1h"));
        assert_eq!(cache_control_ttl(Some("5m")), None);
        assert_eq!(cache_control_ttl(None), None);
        assert_eq!(
            cache_control_ttl(Some("7d")),
            None,
            "an unknown tier falls back to the wire default; the config layer warns"
        );
    }

    #[test]
    fn stop_reason_deserializes_all_known_values_and_catches_unknown() {
        let parse = |raw: &str| -> StopReason {
            serde_json::from_str(&format!("\"{raw}\""))
                .unwrap_or_else(|e| panic!("stop_reason {raw:?} must parse: {e}"))
        };
        assert!(matches!(parse("end_turn"), StopReason::EndTurn));
        assert!(matches!(parse("max_tokens"), StopReason::MaxTokens));
        assert!(matches!(parse("tool_use"), StopReason::ToolUse));
        assert!(matches!(parse("stop_sequence"), StopReason::StopSequence));
        assert!(matches!(parse("refusal"), StopReason::Refusal));
        assert!(matches!(parse("pause_turn"), StopReason::PauseTurn));
        assert!(matches!(
            parse("model_context_window_exceeded"),
            StopReason::ModelContextWindowExceeded
        ));
        match parse("some_future_stop_reason") {
            StopReason::Unknown(s) => assert_eq!(s, "some_future_stop_reason"),
            other => panic!("unknown value must preserve the wire string, got {other:?}"),
        }

        // wire_str is the inverse: known variants round-trip through the serde renames, Unknown yields its inner string unchanged
        assert_eq!(StopReason::MaxTokens.wire_str(), "max_tokens");
        assert_eq!(
            StopReason::ModelContextWindowExceeded.wire_str(),
            "model_context_window_exceeded"
        );
        assert_eq!(
            StopReason::Unknown("some_future_stop_reason".to_string()).wire_str(),
            "some_future_stop_reason"
        );
        assert_eq!(
            serde_json::to_string(&StopReason::Unknown("some_future_stop_reason".into())).unwrap(),
            "\"some_future_stop_reason\"",
            "catch-all must re-serialize the wire string faithfully"
        );
        // The catch-all must also work through the Option<StopReason> field it is parsed from in production
        let delta: MessageDeltaBody =
            serde_json::from_str(r#"{"stop_reason":"mystery_reason"}"#).unwrap();
        match delta.stop_reason {
            Some(StopReason::Unknown(s)) => assert_eq!(s, "mystery_reason"),
            other => panic!("expected Unknown through Option, got {other:?}"),
        }
    }

    /// The terminal `message_delta` of a refusal-terminated stream must parse.
    /// The fixture is a full event because the internally-tagged `MessageStreamEvent` wrapper is the production parse site.
    #[test]
    fn message_delta_with_refusal_stop_reason_parses() {
        let event: MessageStreamEvent = serde_json::from_str(
            r#"{"type":"message_delta","delta":{"stop_reason":"refusal"},"usage":{"output_tokens":5,"input_tokens":10}}"#,
        )
        .expect("refusal message_delta must deserialize");
        match event {
            MessageStreamEvent::MessageDelta { delta, usage } => {
                assert!(matches!(delta.stop_reason, Some(StopReason::Refusal)));
                assert!(delta.stop_details.is_missing(), "no stop_details on the wire");
                assert_eq!(usage.output_tokens, 5);
            }
            other => panic!("expected MessageDelta, got {other:?}"),
        }
    }

    /// A refusal `message_delta` carrying `stop_details` (as emitted by
    /// Anthropic ToS auto-refusals) must parse and preserve the explanation,
    /// and unknown keys inside `stop_details` must not fail the parse.
    #[test]
    fn message_delta_with_refusal_stop_details_parses() {
        let event: MessageStreamEvent = serde_json::from_str(
            r#"{"type":"message_delta","delta":{"stop_reason":"refusal","stop_sequence":null,"stop_details":{"type":"refusal","category":"frontier_llm","explanation":"This request was blocked.","future_key":42}},"usage":{"output_tokens":0}}"#,
        )
        .expect("refusal message_delta with stop_details must deserialize");
        match event {
            MessageStreamEvent::MessageDelta { delta, .. } => {
                assert!(matches!(delta.stop_reason, Some(StopReason::Refusal)));
                let details = delta.stop_details.as_ref().expect("stop_details must be captured");
                assert_eq!(details.r#type.as_deref(), Some("refusal"));
                assert_eq!(details.category.as_deref(), Some("frontier_llm"));
                assert_eq!(
                    details.explanation.as_deref(),
                    Some("This request was blocked.")
                );
            }
            other => panic!("expected MessageDelta, got {other:?}"),
        }
    }

    /// A `stop_sequence`-terminated `message_delta` must parse and preserve the matched string.
    /// Consumers echo it on the Messages API `message.stop_sequence`.
    #[test]
    fn message_delta_captures_matched_stop_sequence() {
        let event: MessageStreamEvent = serde_json::from_str(
            r#"{"type":"message_delta","delta":{"stop_reason":"stop_sequence","stop_sequence":"END"},"usage":{"output_tokens":7}}"#,
        )
        .expect("stop_sequence message_delta must deserialize");
        match event {
            MessageStreamEvent::MessageDelta { delta, .. } => {
                assert!(matches!(delta.stop_reason, Some(StopReason::StopSequence)));
                assert_eq!(delta.stop_sequence.as_deref(), Some("END"));
            }
            other => panic!("expected MessageDelta, got {other:?}"),
        }

        // Absent `stop_sequence` stays `None` and never fails the parse.
        let event: MessageStreamEvent = serde_json::from_str(
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
        )
        .expect("end_turn message_delta must deserialize");
        match event {
            MessageStreamEvent::MessageDelta { delta, .. } => {
                assert_eq!(delta.stop_sequence, None);
            }
            other => panic!("expected MessageDelta, got {other:?}"),
        }
    }

    /// A `redacted_thinking` content block must deserialize into the dedicated variant, preserving the opaque `data`.
    /// Failing the whole `content_block_start` parse would discard an already-streamed response.
    #[test]
    fn redacted_thinking_content_block_parses() {
        let event: MessageStreamEvent = serde_json::from_str(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"EvwBCkgY...opaque"}}"#,
        )
        .expect("redacted_thinking content_block_start must deserialize");
        match event {
            MessageStreamEvent::ContentBlockStart { content_block, .. } => match content_block {
                ContentBlock::RedactedThinking { data } => {
                    assert_eq!(data, "EvwBCkgY...opaque");
                }
                other => panic!("expected RedactedThinking, got {other:?}"),
            },
            other => panic!("expected ContentBlockStart, got {other:?}"),
        }

        // Round-trips to Claude's wire shape.
        let json =
            serde_json::to_value(ContentBlock::RedactedThinking { data: "abc".into() }).unwrap();
        assert_eq!(json["type"], "redacted_thinking");
        assert_eq!(json["data"], "abc");
    }

    #[test]
    fn output_format_json_schema_wire_shape() {
        let fmt = OutputFormat::JsonSchema {
            schema: serde_json::json!({"type": "object", "properties": {"x": {"type": "string"}}}),
        };
        let json = serde_json::to_value(&fmt).unwrap();
        assert_eq!(json["type"], "json_schema");
        assert_eq!(json["schema"]["type"], "object");
        assert!(json.get("name").is_none());

        let config = OutputConfig {
            effort: RequestPresence::omitted(),
            format: RequestPresence::value(fmt),
        };
        let json = serde_json::to_value(&config).unwrap();
        assert!(json.get("effort").is_none(), "effort Omitted state emits no member");
        assert_eq!(json["format"]["type"], "json_schema");
    }
    // ========================================================================
    // MW-2 R9 — response-id leniency
    // ========================================================================

    /// Provenance: hyper-grok-build@45e984f3 — packages/ai/xai-grok-sampling-types/src/messages.rs :: message_start_accepts_missing_response_id (re-expressed; near-verbatim — same-shaped types: HY's `MessagesResponse.id` already carries `#[serde(default)]` at that file's line 217)
    /// A `message_start` without a response-level `id` is missing
    /// compatibility metadata, not a protocol error: the id defaults to
    /// empty and the event still parses.
    #[test]
    fn message_start_accepts_missing_response_id() {
        let event: MessageStreamEvent = serde_json::from_str(
            r#"{"type":"message_start","message":{"type":"message","role":"assistant","content":[],"model":"claude-sonnet-5","stop_reason":null,"usage":{"input_tokens":3,"output_tokens":0}}}"#,
        )
        .expect("a response-level id is optional compatibility metadata");

        match event {
            MessageStreamEvent::MessageStart { message } => {
                assert!(message.id.is_empty());
                assert_eq!(message.model, "claude-sonnet-5");
            }
            other => panic!("expected MessageStart, got {other:?}"),
        }
    }

    /// Provenance: hyper-grok-build@45e984f3 — packages/ai/xai-grok-sampling-types/src/messages.rs :: message_start_still_requires_tool_call_ids (re-expressed; near-verbatim — same-shaped types)
    /// Response-level id leniency must not leak into `tool_use` ids: those
    /// are semantic (they pair calls with results) and stay required.
    #[test]
    fn message_start_still_requires_tool_call_ids() {
        let event = serde_json::from_str::<MessageStreamEvent>(
            r#"{"type":"message_start","message":{"type":"message","role":"assistant","content":[{"type":"tool_use","name":"read_file","input":{}}],"model":"claude-sonnet-5","stop_reason":null,"usage":{"input_tokens":3,"output_tokens":0}}}"#,
        );
        assert!(
            event.is_err(),
            "tool-use ids are semantic and must remain required"
        );
    }

    // ========================================================================
    // MW-2 R4 — usage detail fields
    // ========================================================================

    /// Fresh-written: R4's usage-detail deserialize (spec §4 fresh list) against the exact Q1 proxy usage shape (research-04 §4: `output_tokens_details{thinking_tokens}` + `cache_creation{ephemeral_5m/1h}`). Field names per wirejig/refs/anthropic@d3d5028 spec/src/resources/messages/messages.ts:1290 (OutputTokensDetails), :2423 (Usage.output_tokens_details), :310-321 (CacheCreation), :2388 (Usage.cache_creation).
    #[test]
    fn usage_deserializes_output_tokens_details_and_cache_creation() {
        let usage: MessagesUsage = serde_json::from_str(
            r#"{
                "input_tokens": 100,
                "output_tokens": 42,
                "cache_creation_input_tokens": 10,
                "cache_read_input_tokens": 5,
                "output_tokens_details": { "thinking_tokens": 31 },
                "cache_creation": {
                    "ephemeral_5m_input_tokens": 8,
                    "ephemeral_1h_input_tokens": 2
                }
            }"#,
        )
        .expect("proxy usage shape with detail objects must parse");

        assert_eq!(
            usage.output_tokens_details.as_ref(),
            Some(&OutputTokensDetails {
                thinking_tokens: 31
            })
        );
        assert_eq!(
            usage.cache_creation.as_ref(),
            Some(&CacheCreation {
                ephemeral_5m_input_tokens: 8,
                ephemeral_1h_input_tokens: 2
            })
        );
    }

    /// Fresh-written: the detail objects are absent on most proxy responses;
    /// absence must stay lenient (None) on both the response and the delta
    /// usage shapes.
    #[test]
    fn usage_detail_fields_default_to_none_when_absent() {
        let usage: MessagesUsage =
            serde_json::from_str(r#"{"input_tokens":1,"output_tokens":2}"#).unwrap();
        assert!(usage.output_tokens_details.is_missing());
        assert!(usage.cache_creation.is_missing());

        let delta: MessageDeltaUsage = serde_json::from_str(r#"{"output_tokens":7}"#).unwrap();
        assert!(delta.output_tokens_details.is_missing());
    }

    /// Fresh-written: the Q1 probe returned `thinking_tokens: 0` (claude-sonnet-5
    /// emitted no thinking block); zero must deserialize as PRESENT zero, not
    /// be conflated with an absent detail object.
    #[test]
    fn usage_zero_thinking_tokens_is_present_zero() {
        let usage: MessagesUsage = serde_json::from_str(
            r#"{"input_tokens":0,"output_tokens":1,"output_tokens_details":{"thinking_tokens":0}}"#,
        )
        .unwrap();
        assert_eq!(
            usage.output_tokens_details.as_ref(),
            Some(&OutputTokensDetails { thinking_tokens: 0 })
        );
    }

    // ========================================================================
    // MW-3 R1 — SSE frame forward-compat (spec v1 R1 semantics table)
    //
    // Parse site: this `MessageStreamEvent` Deserialize impl is the single
    // production parse site (D2; client.rs decodes each SSE data payload
    // against it). The table: unknown top-level type -> Ping; unknown
    // content_block kind -> phantom-open (D3, R2); unknown delta subtype ->
    // Ping; known type missing a required field -> FATAL Serialization;
    // ping -> Ping; id-less message_start -> lenient (MW-2 R9 pin).
    // ========================================================================

    /// Provenance: hyper-grok-build@45e984f3 — packages/ai/xai-grok-sampler/src/client.rs :: decode_messages_sse_frame_skips_unknown_event_types (re-expressed; grok maps the unknown kind to a liveness Ping instead of HY's decode-skip — same forward-compat class, grok-shaped event)
    /// A top-level event type this build does not model is liveness, not a
    /// protocol error: it maps to `Ping` and the stream continues.
    #[test]
    fn unknown_top_level_event_type_maps_to_ping() {
        let event: MessageStreamEvent = serde_json::from_str(r#"{"type":"citation","index":0}"#)
            .expect("an unknown event type must not fail the frame parse");
        assert!(
            matches!(event, MessageStreamEvent::Ping),
            "unknown top-level event type must map to Ping, got {event:?}"
        );
    }

    /// Provenance: hyper-grok-build@45e984f3 — packages/ai/xai-grok-sampler/src/client.rs :: decode_messages_sse_frame_skips_unknown_content_block_kinds (re-expressed, start half; grok deviates from HY's Ping mapping per the D3 phantom-open ruling: the unknown kind must open a swallowed phantom block, not disappear — otherwise its later delta/stop hit the fatal unopened-index classes (spec G3))
    /// An unknown `content_block` kind in `content_block_start` keeps the
    /// event (with its index) and maps the block to the phantom `Unknown`
    /// variant, so the stream transform opens-and-swallow the block. A block
    /// object with no `type` at all is likewise an unknown kind (phantom with
    /// an empty kind string), not corruption of a known shape.
    #[test]
    fn unknown_content_block_kind_opens_phantom_block() {
        let event: MessageStreamEvent = serde_json::from_str(
            r#"{"type":"content_block_start","index":3,"content_block":{"type":"brand_new_block","id":"b1"}}"#,
        )
        .expect("an unknown content block kind must not fail the frame parse");
        match event {
            MessageStreamEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                assert_eq!(index, 3, "the index must survive for the phantom-open");
                match content_block {
                    ContentBlock::Unknown { kind } => assert_eq!(kind, "brand_new_block"),
                    other => panic!(
                        "unknown kind must map to the phantom Unknown variant, got {other:?}"
                    ),
                }
            }
            other => panic!(
                "unknown content block kind must stay a ContentBlockStart (phantom-open), got {other:?}"
            ),
        }

        let event: MessageStreamEvent = serde_json::from_str(
            r#"{"type":"content_block_start","index":0,"content_block":{"id":"b2"}}"#,
        )
        .expect("a block object without a type is an unknown kind, not corruption");
        match event {
            MessageStreamEvent::ContentBlockStart { content_block, .. } => match content_block {
                ContentBlock::Unknown { kind } => assert!(kind.is_empty()),
                other => panic!("typeless block must map to the phantom variant, got {other:?}"),
            },
            other => panic!("expected ContentBlockStart, got {other:?}"),
        }
    }

    /// Provenance: apex-pwr review R1 (vacuity seat) — `KNOWN_BLOCK_KINDS` is a
    /// second block-kind mirror and `ContentBlock::ToolReference` deliberately
    /// does not join it.
    /// `tool_reference` is modelled on the transcript side only: across the CC5
    /// lifecycle it appears in `req-*.json` (10/10 turns) and in 0 of the 10
    /// `resp-*.sse`, so the stream parse must keep treating it as an unknown
    /// kind and phantom-open it. That is what makes the sampler's
    /// `ContentBlock::ToolReference` stream arms (stream/messages.rs:402,
    /// stream_classify.rs:155) compile-only exhaustivity: the phantom
    /// `Unknown` and a known-inert kind converge at stream/messages.rs:460,621.
    /// Listing the kind here would put a streamed `tool_reference` on the strict
    /// path (a malformed frame becomes fatal) and would make those arms live —
    /// do it only with a response-side wire capture, flipping this pin in the
    /// same commit.
    #[test]
    fn streamed_tool_reference_block_phantom_opens_as_unknown_today() {
        let event: MessageStreamEvent = serde_json::from_str(
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_reference","tool_name":"mcp__sem__sem_callers"}}"#,
        )
        .expect("a streamed tool_reference must not fail the frame parse");
        match event {
            MessageStreamEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                assert_eq!(index, 2, "the index must survive for the phantom-open");
                match content_block {
                    ContentBlock::Unknown { kind } => assert_eq!(kind, "tool_reference"),
                    other => panic!(
                        "tool_reference is not a known stream kind, so it must phantom-open as \
                         Unknown, got {other:?}"
                    ),
                }
            }
            other => panic!("expected ContentBlockStart, got {other:?}"),
        }
    }

    /// Provenance: hyper-grok-build@45e984f3 — packages/ai/xai-grok-sampler/src/client.rs :: decode_messages_sse_frame_skips_unknown_content_block_kinds (re-expressed, delta half; HY asserts Ping for an unknown delta subtype — same mapping on this wire)
    /// An unknown delta subtype in `content_block_delta` maps the whole event
    /// to `Ping` (liveness): the delta is dropped, never fatal.
    #[test]
    fn unknown_delta_subtype_maps_to_ping() {
        let event: MessageStreamEvent = serde_json::from_str(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"brand_new_delta","x":"y"}}"#,
        )
        .expect("an unknown delta subtype must not fail the frame parse");
        assert!(
            matches!(event, MessageStreamEvent::Ping),
            "unknown delta subtype must map the event to Ping, got {event:?}"
        );
    }

    /// Provenance: none (56a CITATIONS-1 record pin). At this base a live
    /// `citation_delta` is an unknown delta subtype under R1 forward-compat:
    /// the whole `content_block_delta` frame maps to `Ping` (liveness
    /// swallow) — the RECORDED-ABSENT pre-56b behavior of the
    /// `TextBlock.citations` surface (spec r23 L106 row Q5; registry entry
    /// CITATIONS-1). Names this surface explicitly;
    /// `unknown_delta_subtype_maps_to_ping` uses `brand_new_delta` and
    /// predates the record. When 56b models the surface, this pin flips in
    /// the same commit (strict-parse pin) — a mid-state tree fails the gate
    /// by construction (SDD §6 R4).
    #[test]
    fn citation_delta_maps_to_ping_today() {
        let event: MessageStreamEvent = serde_json::from_str(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"citation_delta","x":"y"}}"#,
        )
        .expect("an unmodeled delta subtype must not fail the frame parse");
        assert!(
            matches!(event, MessageStreamEvent::Ping),
            "RECORDED-ABSENT (56a): citation_delta must map to Ping (liveness swallow) \
             until registry entry CITATIONS-1 flips modeled_by in the same commit \
             (56b), got {event:?}"
        );
    }

    /// Provenance: hyper-grok-build@45e984f3 — packages/ai/xai-grok-sampler/src/client.rs :: decode_messages_sse_frame_keeps_malformed_known_events_strict (re-expressed; near-verbatim)
    /// Forward-compat must not hide wire corruption: a KNOWN event type
    /// missing a required field is a fatal deserialization error.
    #[test]
    fn malformed_known_event_missing_required_field_stays_fatal() {
        let event = serde_json::from_str::<MessageStreamEvent>(r#"{"type":"content_block_stop"}"#);
        assert!(
            event.is_err(),
            "a known type missing a required field must stay fatal"
        );
    }

    /// Provenance: hyper-grok-build@45e984f3 — packages/ai/xai-grok-sampler/src/client.rs :: decode_messages_sse_frame_parses_ping_and_text_delta (re-expressed; near-verbatim)
    /// The two already-lenient rows: an explicit `ping` parses as Ping and a
    /// well-known `text_delta` parses as a ContentBlockDelta.
    #[test]
    fn ping_and_text_delta_parse() {
        assert!(matches!(
            serde_json::from_str::<MessageStreamEvent>(r#"{"type":"ping"}"#).unwrap(),
            MessageStreamEvent::Ping
        ));
        let event: MessageStreamEvent = serde_json::from_str(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
        )
        .unwrap();
        assert!(
            matches!(event, MessageStreamEvent::ContentBlockDelta { .. }),
            "a well-known text_delta must parse as ContentBlockDelta, got {event:?}"
        );
    }

    /// Fresh-written: R7 row-18 wrong-type subcase at the serde level — a KNOWN
    /// delta subtype carrying a wrong-typed field is wire corruption, not an
    /// unknown shape: it must stay a fatal error (the R1 unknown-subtype -> Ping
    /// mapping must not swallow it).
    #[test]
    fn known_delta_subtype_with_wrong_typed_field_stays_fatal() {
        let event = serde_json::from_str::<MessageStreamEvent>(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":42}}"#,
        );
        assert!(
            event.is_err(),
            "a known subtype with a wrong-typed field must stay fatal, not map to Ping"
        );
    }

    /// Provenance: xli@3d4a08271e — codex-rs/codex-api/src/sse/messages_wire_types.rs :: Thinking wire variant (re-expressed pin: xli's wire type carries NO signature field and its parse test accepts `{"type":"thinking","thinking":"hmm"}`; the real API delivers the signature via `signature_delta`, so a start block omitting it is a legitimate wire shape, not corruption — same house pattern as the MW-2 R9 id leniency on `MessagesResponse`)
    /// A thinking `content_block_start` omitting `signature` must parse with
    /// an empty signature, never fail (wire-fidelity pin; RED-verified
    /// against the pre-fix strict type — the R4 fixture replays
    /// eq-03/eq-04/eq-16 demonstrate the same red at the stream site).
    #[test]
    fn thinking_block_start_without_signature_parses_empty() {
        let block: ContentBlock = serde_json::from_str(r#"{"type":"thinking","thinking":"hmm"}"#)
            .expect("a thinking start without signature is a real wire shape");
        match block {
            ContentBlock::Thinking {
                thinking,
                signature,
            } => {
                assert_eq!(thinking, "hmm");
                assert_eq!(signature, "");
            }
            other => panic!("expected Thinking, got {other:?}"),
        }
        // A KNOWN required field is still fatal (corruption stays fatal).
        let err = serde_json::from_str::<ContentBlock>(r#"{"type":"thinking"}"#);
        assert!(
            err.is_err(),
            "a thinking block missing `thinking` must stay fatal"
        );
    }

    /// Fresh-written: A5 no-diff guard for the R1 serde adaptation — the R1
    /// unknown-kind leniency lives at the STREAM parse site only. The
    /// non-stream `MessagesResponse` parse must stay strict: an unknown
    /// content block kind in a non-stream response is still a fatal error.
    #[test]
    fn non_stream_response_unknown_block_kind_stays_fatal() {
        let event = serde_json::from_str::<MessagesResponse>(
            r#"{"type":"message","role":"assistant","content":[{"type":"brand_new_block","id":"b1"}],"model":"claude-sonnet-5","stop_reason":null,"usage":{"input_tokens":1,"output_tokens":1}}"#,
        );
        assert!(
            event.is_err(),
            "non-stream block-kind leniency would be an unscoped behavior change"
        );
    }

    // ---- apex-pwr: Messages transcript IR shapes (RED first, design-pwr §4) ----

    /// R1 — the transcript half of a CC5 turn: a `tool_result` whose nested
    /// `content[]` is a select-list of `tool_reference` blocks. The non-stream
    /// parse goes through the hand-written `Deserialize` over `StrictBlock`, so
    /// the enum variant alone is not enough — that is the invariant under test.
    #[test]
    fn tool_reference_block_round_trips_cc5_bytes() {
        // CC5 req-002 $.messages[3].content[0], verbatim (P1's answering tool_result).
        const CC5_TOOL_RESULT: &str = r#"{"type":"tool_result","tool_use_id":"toolu_vrtx_01LF8PjSdXuFPg3KNCgnxbqd","content":[{"type":"tool_reference","tool_name":"mcp__sem__sem_callers"},{"type":"tool_reference","tool_name":"mcp__sem__sem_impact"},{"type":"tool_reference","tool_name":"mcp__sem__sem_context"},{"type":"tool_reference","tool_name":"mcp__code-graph__get_call_graph"}]}"#;

        // (a) it must DESERIALIZE (this is the half the manual impl breaks).
        let block: ContentBlock = serde_json::from_str(CC5_TOOL_RESULT)
            .expect("CC5 tool_result with tool_reference blocks must parse");

        // (b) the nested list must be four ToolReference blocks in SELECT-LIST order.
        let ContentBlock::ToolResult {
            content,
            tool_use_id,
            is_error,
            ..
        } = &block
        else {
            panic!("expected ToolResult, got {block:?}");
        };
        assert_eq!(tool_use_id, "toolu_vrtx_01LF8PjSdXuFPg3KNCgnxbqd");
        assert!(!is_error);
        let ToolResultContent::Blocks(inner) = content else {
            panic!("tool_result content must parse as Blocks, got {content:?}");
        };
        let names: Vec<&str> = inner
            .iter()
            .map(|b| match b {
                ContentBlock::ToolReference { tool_name } => tool_name.as_str(),
                other => panic!("expected ToolReference, got {other:?}"),
            })
            .collect();
        assert_eq!(
            names,
            vec![
                "mcp__sem__sem_callers",
                "mcp__sem__sem_impact",
                "mcp__sem__sem_context",
                "mcp__code-graph__get_call_graph"
            ],
            "select-list order is the wire order (CC5 P1)"
        );

        // (c) it must RE-SERIALIZE to the same JSON value (key order is irrelevant, shape is not).
        let round: serde_json::Value = serde_json::to_value(&block).unwrap();
        let original: serde_json::Value = serde_json::from_str(CC5_TOOL_RESULT).unwrap();
        assert_eq!(round, original, "round-trip must be lossless");
    }

    /// R2 — `defer_loading` is a class marker, not a per-tool boolean: a static
    /// tool must emit NO key at all (an explicit `false`/`null` is off-donor on
    /// every request), and only a deferred tool emits `true`.
    #[test]
    fn tool_custom_defer_loading_is_omitted_unless_set() {
        let plain = ToolCustom {
            name: "Bash".into(),
            description: None,
            input_schema: serde_json::json!({"type": "object"}),
            cache_control: None,
            defer_loading: None,
        };
        let v = serde_json::to_value(&plain).unwrap();
        assert!(v.get("defer_loading").is_none(), "absent, not false: {v}");

        let deferred = ToolCustom {
            defer_loading: Some(true),
            ..plain.clone()
        };
        assert_eq!(
            serde_json::to_value(&deferred)
                .unwrap()
                .get("defer_loading"),
            Some(&serde_json::json!(true))
        );

        // Donor bytes: CC5 req-001 $.tools[11] (DeferredToolPlaceholder).
        let donor: ToolCustom = serde_json::from_str(
            r#"{"name":"DeferredToolPlaceholder","description":"Reserved placeholder that keeps deferred tool loading active; never call this tool.","input_schema":{"type":"object","properties":{}},"defer_loading":true}"#,
        )
        .expect("donor placeholder must parse");
        assert_eq!(donor.defer_loading, Some(true));
    }

    /// R3 — every donor `tools[]` entry observed across all 10 CC5 turns carries
    /// exactly one of two key sets (`{name,description,input_schema}` or the same
    /// plus `defer_loading`) in that order, so byte-equality after a round-trip is
    /// the right assertion and is not vacuous. It is taken on the rendered string,
    /// never on `serde_json::Value`: with Cargo.toml's `preserve_order` a Map is an
    /// IndexMap whose `PartialEq` is per-key, so Value equality cannot see a reordered
    /// field and `defer_loading` could silently stop serialising last.
    /// Bytes inlined from the fixtures
    /// (req-001 `$.tools[0]`/`$.tools[11]`, req-010 `$.tools[11]` promoted).
    #[test]
    fn cc5_donor_tool_entries_round_trip_losslessly() {
        const DONOR_TOOL_PLAIN: &str = r#"{"name":"Agent","description":"Launch a new agent to handle complex, multi-step tasks. Each agent type has specific capabilities and tools available to it.\n\nAvailable agent types are listed in <system-reminder> messages in the conversation.\n\nWhen using the Agent tool, specify a subagent_type parameter to select which agent type to use. If omitted, the general-purpose agent is used.\n\n## When to use\n\nReach for this when the task matches an available agent type, when you have independent work to run in parallel, or when answering would mean reading across several files — delegate it and you keep the conclusion, not the file dumps. For a single-fact lookup where you already know the file, symbol, or value, search directly. Once you've delegated a search, don't also run it yourself — wait for the result.\n\n- The agent's final report is not shown to the user — relay what matters.\n- Use SendMessage with the agent's ID or name to continue a previously spawned agent with its context intact; a new Agent call starts fresh.\n- Each agent type's model, reasoning effort, and tools come from its definition (`.claude/agents/*.md` frontmatter or SDK `agents`).\n- `isolation: \"worktree\"` gives the agent its own git worktree (auto-cleaned if unchanged).\n- Subagents run in the background by default; you'll be notified when one completes. Pass `run_in_background: false` only when your very next action depends on the result and nothing else could usefully happen while it runs — otherwise background it so the user can interject. Never fabricate or predict a pending agent's results — the notification is never something you write yourself; if the user asks before it arrives, say it's still running.","input_schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"description":{"description":"A short (3-5 word) description of the task","type":"string"},"prompt":{"description":"The task for the agent to perform","type":"string"},"subagent_type":{"description":"The type of specialized agent to use for this task","type":"string"},"model":{"description":"Optional model override for this agent. Takes precedence over the agent definition's model frontmatter and the configured default subagent model. If omitted, uses the agent definition's model, else the default (inherits from the parent unless a default subagent model is configured). Ignored for subagent_type: \"fork\" — forks always inherit the parent model.","type":"string","enum":["sonnet","opus","haiku","fable"]},"run_in_background":{"description":"Agents run in the background by default; you will be notified when one completes. Set to false only when your very next action depends on this agent's result and nothing else could usefully happen while it runs — otherwise leave it in the background so the user can hand you other work.","type":"boolean"},"isolation":{"description":"Isolation mode. \"worktree\" creates a temporary git worktree so the agent works on an isolated copy of the repo. \"remote\" launches the agent in a remote cloud environment (always runs in background; availability is gated).","type":"string","enum":["worktree","remote"]}},"required":["description","prompt"],"additionalProperties":false}}"#;
        const DONOR_TOOL_PLACEHOLDER: &str = r#"{"name":"DeferredToolPlaceholder","description":"Reserved placeholder that keeps deferred tool loading active; never call this tool.","input_schema":{"type":"object","properties":{}},"defer_loading":true}"#;
        const DONOR_TOOL_PROMOTED: &str = r#"{"name":"mcp__code-graph__get_call_graph","description":"Multi-hop call chain. Replaces N rounds of `grep \"X(\"` + Read. Pass route_path='GET /api/x' to trace HTTP handler → downstream (folds the old trace_http_chain).","input_schema":{"type":"object","properties":{"compact":{"description":"Compact mode: name+file+depth only (saves tokens)","type":"boolean"},"depth":{"description":"Max depth (default 3)","type":"number"},"direction":{"description":"Direction (default 'both'); ignored when route_path is set (always 'callees')","enum":["callers","callees","both"],"type":"string"},"file_path":{"description":"Disambiguate same-name functions","type":"string"},"include_middleware":{"description":"For route_path mode: include downstream middleware/calls (default true)","type":"boolean"},"include_tests":{"description":"Include test callers (default false)","type":"boolean"},"project":{"description":"Project alias (from CODE_GRAPH_PROJECTS). Omit for default project.","type":"string"},"route_path":{"description":"HTTP route like 'GET /api/users' — traces from matched route handler(s) down. Mutually exclusive with symbol_name.","type":"string"},"symbol_name":{"description":"Function/method name (mutually exclusive with route_path)","type":"string"}}},"defer_loading":true}"#;

        for donor in [
            DONOR_TOOL_PLAIN,
            DONOR_TOOL_PLACEHOLDER,
            DONOR_TOOL_PROMOTED,
        ] {
            let entry: ToolCustom = serde_json::from_str(donor).unwrap();
            assert_eq!(
                serde_json::to_string(&entry).unwrap(),
                donor,
                "donor tools[] bytes, key order included, must round-trip: {donor}"
            );
        }

        let entry: ToolCustom = serde_json::from_str(DONOR_TOOL_PLAIN).unwrap();
        assert!(
            serde_json::to_value(&entry)
                .unwrap()
                .get("defer_loading")
                .is_none(),
            "a static donor tool must not grow a defer_loading key: {DONOR_TOOL_PLAIN}"
        );

        for donor in [DONOR_TOOL_PLACEHOLDER, DONOR_TOOL_PROMOTED] {
            let entry: ToolCustom = serde_json::from_str(donor).unwrap();
            assert_eq!(
                serde_json::to_value(&entry).unwrap().get("defer_loading"),
                Some(&serde_json::json!(true)),
                "a deferred donor tool keeps defer_loading true, promoted or not: {donor}"
            );
        }
    }
}
