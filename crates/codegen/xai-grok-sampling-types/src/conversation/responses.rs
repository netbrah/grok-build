//! Responses-side conversation flattening (stock upstream,
//! monorepo-synced at the R0 branch base).
//!
//! Provenance: P2.1 codex remote compaction v2 (ledger §P2.1) — the
//! ported region in this file is the `rs::OutputItem::Compaction` arm of
//! `response_to_conversation_items` (encrypted replacement →
//! `CodexRawInput` carrier), added by commit 20d782d: verbatim from
//! open-grok@240c99c9
//! `crates/codegen/xai-grok-sampling-types/src/conversation.rs:3836`
//! (donor-side monolithic conversation.rs; byte-identical, re-verified
//! 2026-09-15; part of the item's 27/27 verbatim spot-checks — Sagan
//! review, ledger §P2.1). The item's remaining P2.1 markers sit on the
//! shell collector (`xai-grok-shell/src/session/compaction.rs`) and the
//! test files; this header is this carrier file's surface.

use std::collections::BTreeMap;

use super::*;

/// Bounded provider-neutral context used until the selected transport
/// restores a provider-native search item, if that provider supports one.
/// (apex-ayl.76; donor parity: open-grok@049664b5 conversation.rs:2161-2163.)
pub(super) const PROVIDER_NATIVE_SEARCH_REPLAY_SUMMARY: &str =
    "[A provider-native search was completed earlier in the conversation.]";

/// Flatten `response.output` into `ConversationItem`s, preserving emission order.
/// Replaying that order byte for byte on the next turn is what keeps the server-side prefix cache hot.
pub fn response_to_conversation_items(response: rs::Response) -> Vec<ConversationItem> {
    let model_id = response.model.clone();
    let model_fingerprint = response
        .metadata
        .as_ref()
        .and_then(|m| m.get("system_fingerprint"))
        .cloned()
        .filter(|s| !s.is_empty());
    let reasoning_effort = response
        .reasoning
        .as_ref()
        .and_then(|r| r.effort.clone())
        .map(crate::ReasoningEffort::from_responses_api);

    let mut items: Vec<ConversationItem> = Vec::with_capacity(response.output.len() + 1);
    let mut content = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut backend_tool_count: usize = 0;

    for item in response.output {
        match item {
            rs::OutputItem::Message(msg) => {
                for content_part in msg.content {
                    if let rs::OutputMessageContent::OutputText(text_content) = content_part {
                        if !content.is_empty() {
                            content.push('\n');
                        }
                        content.push_str(&text_content.text);
                    }
                }
            }
            rs::OutputItem::FunctionCall(fc) => {
                // Tied to the assistant turn: a ToolResult must follow each one in conversation order, so they are not siblings
                tool_calls.push(ToolCall {
                    id: Arc::<str>::from(fc.call_id),
                    name: fc.name,
                    arguments: Arc::<str>::from(fc.arguments),
                });
            }
            rs::OutputItem::Reasoning(r) => {
                items.push(ConversationItem::Reasoning(r.into()));
            }
            rs::OutputItem::Compaction(compaction) => {
                // Remote compaction v2 emits its encrypted replacement as a
                // normal Responses output item. Keep the provider payload
                // opaque and in-order so it can be replayed exactly on the
                // next Codex turn. async-openai requires an `id` even though
                // the wire permits it to be absent; an empty typed-boundary
                // sentinel must never become a fabricated provider ID.
                let rs::CompactionBody {
                    id,
                    encrypted_content,
                    created_by,
                } = compaction;
                let local_id = if id.is_empty() {
                    format!("codex_compaction_{}", items.len())
                } else {
                    id.clone()
                };
                let mut raw = serde_json::json!({
                    "type": "compaction",
                    "encrypted_content": encrypted_content,
                });
                if !id.is_empty() {
                    raw["id"] = serde_json::Value::String(id);
                }
                if let Some(created_by) = created_by {
                    raw["created_by"] = serde_json::Value::String(created_by);
                }
                backend_tool_count += 1;
                items.push(ConversationItem::BackendToolCall(BackendToolCallItem {
                    kind: BackendToolKind::CodexRawInput(CodexRawInputItem {
                        id: local_id,
                        raw,
                        cross_provider_fallback: None,
                        mint_tag: None,
                    }),
                }));
            }
            // These calls already ran server-side; they are kept so later turns replay the same context
            rs::OutputItem::WebSearchCall(ws) => {
                backend_tool_count += 1;
                items.push(ConversationItem::BackendToolCall(BackendToolCallItem {
                    kind: BackendToolKind::WebSearch(ws),
                }));
            }
            rs::OutputItem::CustomToolCall(ct) => {
                backend_tool_count += 1;
                items.push(ConversationItem::BackendToolCall(BackendToolCallItem {
                    kind: BackendToolKind::XSearch(ct),
                }));
            }
            rs::OutputItem::CodeInterpreterCall(ci) => {
                backend_tool_count += 1;
                items.push(ConversationItem::BackendToolCall(BackendToolCallItem {
                    kind: BackendToolKind::CodeInterpreter(ci),
                }));
            }
            rs::OutputItem::McpCall(_) => {
                backend_tool_count += 1;
            }
            _ => {}
        }
    }

    if backend_tool_count > 0 {
        tracing::info!(
            backend_tool_count,
            "response contained backend-executed tool calls"
        );
    }

    tracing::info!(model_id = %model_id, ?model_fingerprint, ?reasoning_effort, "response_to_conversation_items setting model metadata on AssistantItem");
    items.push(ConversationItem::Assistant(AssistantItem {
        content: Arc::<str>::from(content),
        tool_calls,
        model_id: Some(model_id),
        model_fingerprint,
        reasoning_effort,
    }));

    items
}

impl From<&ConversationRequest> for rs::CreateResponse {
    fn from(req: &ConversationRequest) -> Self {
        let input = build_responses_input(req);
        let tools = build_responses_tools(req);

        let tool_choice = req.tool_choice.as_ref().map(|tc| match tc {
            ConversationToolChoice::Auto => rs::ToolChoiceParam::Option(rs::ToolChoiceOptions::Auto),
            ConversationToolChoice::None => rs::ToolChoiceParam::Option(rs::ToolChoiceOptions::None),
            ConversationToolChoice::Required => {
                rs::ToolChoiceParam::Option(rs::ToolChoiceOptions::Required)
            }
            ConversationToolChoice::Function(name) => {
                rs::ToolChoiceParam::Function(rs::ToolChoiceFunction { name: name.clone() })
            }
        });

        let text = req
            .json_schema
            .as_ref()
            .map(|schema| rs::ResponseTextParam {
                format: rs::TextResponseFormatConfiguration::JsonSchema(
                    rs::ResponseFormatJsonSchema {
                        description: None,
                        name: STRUCTURED_OUTPUT_SCHEMA_NAME.to_string(),
                        schema: schema.clone(),
                        strict: Some(true),
                    },
                ),
                verbosity: None,
            });

        rs::CreateResponse {
            background: None,
            conversation: None,
            include: None,
            input,
            instructions: None,
            max_output_tokens: req.max_output_tokens,
            max_tool_calls: None,
            metadata: None,
            model: req.model.clone(),
            parallel_tool_calls: None,
            previous_response_id: None,
            prompt: None,
            prompt_cache_key: req
                .prompt_cache_key
                .clone()
                .or_else(|| req.x_grok_conv_id.clone()),
            prompt_cache_retention: None,
            reasoning: Some(rs::Reasoning {
                effort: req.reasoning_effort.map(|e| e.to_responses_api()),
                summary: Some(rs::ReasoningSummary::Concise),
                mode: None,
                context: None,
            }),
            safety_identifier: None,
            service_tier: None,
            store: None,
            stream: None,
            stream_options: None,
            temperature: req.temperature,
            text,
            tool_choice,
            tools: if tools.is_empty() { None } else { Some(tools) },
            top_logprobs: None,
            top_p: req.top_p,
            truncation: None,
            context_management: None,
            moderation: None,
            prompt_cache_options: None,
        }
    }
}

/// Reasoning items stay top-level siblings rather than folding into the assistant, so the input replays the model's original order.
pub(super) fn build_responses_input(req: &ConversationRequest) -> rs::InputParam {
    let items: Vec<rs::InputItem> = req
        .items
        .iter()
        .flat_map(conversation_item_to_input_items)
        .collect();
    rs::InputParam::Items(items)
}

/// Inject the `type: "reasoning_text"` discriminator the API requires.
/// `async-openai`'s `ReasoningTextContent` has no `type` field, so it serializes to `{"text": ...}` and the API answers 400.
/// Delete this once upstream grows the field.
pub fn patch_reasoning_text_types(body: &mut serde_json::Value) {
    let Some(input) = body.get_mut("input").and_then(|v| v.as_array_mut()) else {
        return;
    };
    for item in input.iter_mut() {
        if item.get("type").and_then(|t| t.as_str()) != Some("reasoning") {
            continue;
        }
        let Some(content) = item.get_mut("content").and_then(|c| c.as_array_mut()) else {
            continue;
        };
        for c in content.iter_mut() {
            if let Some(obj) = c.as_object_mut() {
                obj.entry("type")
                    .or_insert_with(|| serde_json::Value::String("reasoning_text".into()));
            }
        }
    }
}

/// First-send repair for empty reasoning ids (apex-ayl.69, XW-EMPTYID-1).
///
/// messages-wire sessions persist reasoning items with `id: ""` (the
/// sampler's `stream/messages.rs` persist seam). The first cross-wire replay
/// onto a strict responses target 400s on that empty id (incident
/// 01a0b046, cell vxm-az). The sampler calls this beside
/// `patch_reasoning_text_types` on every /responses send path, so the
/// repair is retroactive for ALL persisted sessions — no migration:
///
/// - A `type == "reasoning"` input item whose `id` is absent, `null`, or
///   `""` gets the shared xw_ grammar — the .71 switch-time projector's
///   `projection::xw_reasoning_id_values` core, so persisted-then-switched
///   and direct-replay paths agree (one rule across goldens + L0 +
///   send-time patch; the original `rs_`+hash proposal is superseded,
///   sdd-69 §2.5): `content` := the item's `content` (absent → `[]`),
///   `summary` := the item's `summary` (absent → `null`), `ord` := the
///   0-based index of the item among the input's reasoning items.
/// - Non-empty ids are untouched (vLLM-coined `rs_…` ids ride verbatim); the
///   patch is idempotent by construction.
///
/// Ordering: the sampler runs this BEFORE `project_strict_responses_input`,
/// so strict rows keep the REPLAY-1 behavior (the projector strips the id
/// afterwards) and lenient rows get the synthesized id (maximum fidelity).
pub fn patch_reasoning_empty_ids(body: &mut serde_json::Value, cell: &str) {
    let Some(input) = body.get_mut("input").and_then(serde_json::Value::as_array_mut) else {
        return;
    };
    let mut ord = 0usize;
    for item in input.iter_mut() {
        if item.get("type").and_then(|t| t.as_str()) != Some("reasoning") {
            continue;
        }
        let has_empty_id = item
            .get("id")
            .and_then(serde_json::Value::as_str)
            .is_none_or(str::is_empty);
        if has_empty_id {
            let content = item
                .get("content")
                .cloned()
                .unwrap_or(serde_json::Value::Array(Vec::new()));
            let summary = item.get("summary").cloned().unwrap_or(serde_json::Value::Null);
            item["id"] = serde_json::Value::String(super::projection::xw_reasoning_id_values(
                cell, ord, &content, &summary,
            ));
        }
        ord += 1;
    }
}

pub(super) fn conversation_item_to_input_items(item: &ConversationItem) -> Vec<rs::InputItem> {
    match item {
        ConversationItem::System(s) => {
            vec![rs::InputItem::EasyMessage(rs::EasyInputMessage {
                r#type: rs::MessageType::Message,
                role: rs::Role::System,
                content: rs::EasyInputContent::Text(s.content.as_ref().to_owned()),
                phase: None,
            })]
        }
        ConversationItem::User(u) => {
            let content = content_parts_to_easy_input_content(&u.content);
            vec![rs::InputItem::EasyMessage(rs::EasyInputMessage {
                r#type: rs::MessageType::Message,
                role: rs::Role::User,
                content,
                phase: None,
            })]
        }
        ConversationItem::Reasoning(r) => {
            // `status` is output-only and rejected on input.
            let mut r = r.clone();
            r.status = None;
            vec![rs::InputItem::Item(rs::Item::Reasoning(r.item))]
        }
        ConversationItem::Assistant(a) => {
            let mut items = Vec::new();

            if !a.content.is_empty() {
                items.push(rs::InputItem::EasyMessage(rs::EasyInputMessage {
                    r#type: rs::MessageType::Message,
                    role: rs::Role::Assistant,
                    content: rs::EasyInputContent::Text(a.content.as_ref().to_owned()),
                    phase: None,
                }));
            }

            for tc in &a.tool_calls {
                let arguments = sanitize_tool_arguments(&tc.id, &tc.name, tc.arguments.clone());
                items.push(rs::InputItem::Item(rs::Item::FunctionCall(
                    rs::FunctionToolCall {
                        call_id: tc.id.as_ref().to_owned(),
                        name: tc.name.clone(),
                        arguments: arguments.as_ref().to_owned(),
                        id: None,
                        status: None,
                        namespace: None,
                        caller: None,
                        r#async: None,
                    },
                )));
            }

            items
        }
        // Invariant: an output array only ever exists when at least one image part materialized,
        // and a tool result carries one textual part — its own result text, leading that array;
        // with no image the output collapses to the bare string, as the donor does
        // (`normalize_tool_output`, codex-rs/codex-api/src/endpoint/content_type_compat.rs:94
        // in the external donor tree) — an all-text output array is the shape a
        // Responses->ChatCompletions shim hard-rejects, and nothing repairs it downstream:
        // `normalize_content_types` (`xai-grok-sampler/src/provider.rs:470-491`) walks only
        // `input[*].content[*]`, and this item carries `output`, so it never reaches these bytes.
        ConversationItem::ToolResult(t) => {
            let images: Vec<rs::InputContent> = t
                .images
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Image { url } => {
                        Some(rs::InputContent::InputImage(rs::InputImageContent {
                            detail: rs::ImageDetail::Auto,
                            file_id: None,
                            image_url: Some(url.as_ref().to_owned()),
                            prompt_cache_breakpoint: None,
                        }))
                    }
                    // A text part riding inside `images` is dropped here and never reaches this
                    // wire: the harness gives a tool result one textual part — its result text,
                    // leading the array. The sibling dialects drop it the same way, their loop
                    // arm matching only `ContentPart::Image` with no text arm at all
                    // (conversation/messages.rs:836, conversation/chat_completions.rs:144, each
                    // behind an `images.is_empty()` branch at :828 / :137); the one textual part
                    // they do emit is the result text, pushed separately at :831 / :140.
                    ContentPart::Text { .. } => None,
                })
                .collect();
            let output = if images.is_empty() {
                rs::FunctionCallOutput::Text(t.content.as_ref().to_owned())
            } else {
                let mut parts = vec![rs::InputContent::InputText(rs::InputTextContent {
                    text: t.content.as_ref().to_owned(),
                    prompt_cache_breakpoint: None,
                })];
                parts.extend(images);
                rs::FunctionCallOutput::Content(parts)
            };
            vec![rs::InputItem::Item(rs::Item::FunctionCallOutput(
                rs::FunctionCallOutputItemParam {
                    call_id: Some(t.tool_call_id.clone()),
                    output,
                    id: None,
                    status: None,
                    name: None,
                    namespace: None,
                    caller: None,
                },
            ))]
        }
        ConversationItem::BackendToolCall(b) => {
            vec![match &b.kind {
                BackendToolKind::WebSearch(ws) => {
                    rs::InputItem::Item(rs::Item::WebSearchCall(ws.clone()))
                }
                // `CustomToolCall` is only a persistence carrier for xAI's
                // backend-executed X Search. Letting that carrier serialize
                // directly would create an orphan client custom-tool call —
                // no `custom` tool is ever declared on any dialect — so the
                // item would be undeclared on the wire (apex-ayl.76 hazard).
                // Keep the generic typed request provider-neutral and
                // bounded; the xAI transport restores this exact flattened
                // slot with the native `x_search_call` wire item after
                // serialization (`x_search_call_wire_value`), and every other
                // dialect keeps the placeholder (fail-closed).
                // (apex-ayl.76; donor parity: open-grok@049664b5
                // conversation.rs:4436-4446.)
                BackendToolKind::XSearch(_) => rs::InputItem::EasyMessage(rs::EasyInputMessage {
                    r#type: rs::MessageType::Message,
                    role: rs::Role::Assistant,
                    content: rs::EasyInputContent::Text(
                        PROVIDER_NATIVE_SEARCH_REPLAY_SUMMARY.to_owned(),
                    ),
                    phase: None,
                }),
                BackendToolKind::CodeInterpreter(ci) => {
                    rs::InputItem::Item(rs::Item::CodeInterpreterCall(ci.clone()))
                }
                // async-openai does not model the `compaction` input item (or
                // future replacement-history variants). Emit one typed,
                // harmless placeholder here; the sampler replaces this exact
                // flattened input position with `item.raw` immediately after
                // request serialization and only for the Codex wire dialect.
                BackendToolKind::CodexRawInput(raw) => {
                    rs::InputItem::EasyMessage(rs::EasyInputMessage {
                        r#type: rs::MessageType::Message,
                        role: raw.responses_placeholder_role(),
                        phase: None,
                        // A non-Codex request deliberately does not receive
                        // the opaque provider item. Give cross-provider model
                        // switches the safe retained-message summary instead
                        // of leaking encrypted JSON or losing all context.
                        content: rs::EasyInputContent::Text(raw.text_summary()),
                    })
                }
            }]
        }
    }
}

fn content_parts_to_easy_input_content(parts: &[ContentPart]) -> rs::EasyInputContent {
    if parts.len() == 1
        && let ContentPart::Text { text } = &parts[0]
    {
        return rs::EasyInputContent::Text(text.as_ref().to_owned());
    }

    let items: Vec<rs::InputContent> = parts
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => rs::InputContent::InputText(rs::InputTextContent {
                text: text.as_ref().to_owned(),
                prompt_cache_breakpoint: None,
            }),
            ContentPart::Image { url } => rs::InputContent::InputImage(rs::InputImageContent {
                image_url: Some(url.as_ref().to_owned()),
                file_id: None,
                detail: rs::ImageDetail::default(),
                prompt_cache_breakpoint: None,
            }),
        })
        .collect();

    rs::EasyInputContent::ContentList(items)
}

/// The request's client function tools.
/// A function tool whose name collides with a backend-hosted tool is dropped: sending both is rejected as a duplicate, so the hosted tool wins.
/// Both ride the raw-JSON [`extra_tool_entries`] channel instead.
fn build_responses_tools(req: &ConversationRequest) -> Vec<rs::Tool> {
    let tools: Vec<rs::Tool> = req
        .tools
        .iter()
        .filter(|t| {
            let collides = req.hosted_tools.iter().any(|h| h.wire_name() == t.name);
            if collides {
                tracing::warn!(
                    tool = %t.name,
                    "dropping function tool that collides with a backend-hosted tool"
                );
            }
            !collides
        })
        .map(|t| {
            rs::Tool::Function(rs::FunctionTool {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: Some(t.parameters.clone()),
                strict: None,
                defer_loading: None,
                r#async: None,
                output_schema: None,
                allowed_callers: None,
            })
        })
        .collect();

    tools
}

/// Every hosted tool as a raw JSON entry, which the sampler client splices into the serialized `tools` array.
/// `web_search` rides it because async_openai's `rs::WebSearchToolFilters` models only `allowed_domains` and cannot carry `excluded_domains`.
/// Emitting either as a typed `rs::Tool` as well would send it twice, which the API rejects as a duplicate.
pub fn extra_tool_entries(hosted_tools: &[HostedTool]) -> Vec<serde_json::Value> {
    extra_tool_entries_with_declaration(hosted_tools, None)
}

// ─── Hosted `tool_search` declaration ───────────────────────────────────────
//
// Provenance: openai/codex codex-rs/core/src/tools/handlers/tool_search_spec.rs ::
// create_tool_search_tool (re-expressed for this crate, bead apex-waj.3). The donor-pinned
// bytes are the description template, the `parameters` schema, the entry key order and the
// shared source-description budget. Enforced in-tree, independently of the source set, is the
// source-independent half only: `type`/`execution`/`parameters` and their key order, asserted by
// `responses_tests::tool_search_declaration_fixed_half_is_donor_exact` for the `client` variant it
// drives; `responses_tests::tool_search_declaration_execution_is_total` pins the emitted `server`
// value. The whole-entry test compares the emitted entry against a literal transcribed from a CX1
// capture held in the campaign tree, outside this repository: a producer change reddens it, so what
// goes unenforced is only the literal's fidelity to that capture.
// ────────────────────────────────────────────────────────────────────────────

/// `type` tag of the declaration entry, shared by the entry, its description and the call the
/// model makes with it. The discovery *item* tags live in `conversation::tool_search`
/// (`TOOL_SEARCH_CALL_ITEM_TYPE` / `TOOL_SEARCH_OUTPUT_ITEM_TYPE`); this names the declaration.
const TOOL_SEARCH_DECLARATION_TYPE: &str = "tool_search";

/// Cap on the whole rendered source list, shared by every source's description.
/// Donor parity: `core/src/tools/handlers/tool_search_spec.rs:8`.
pub(super) const MAX_TOOL_SEARCH_SOURCE_DESCRIPTION_BYTES: usize = 512 * 1024;

/// Who executes a `tool_search` call. These are the only values the wire accepts, so the
/// donor's `sync` — which the API 400s — has no variant here rather than a runtime check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ToolSearchExecution {
    Server,
    Client,
}

impl ToolSearchExecution {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            // Defined once for the whole crate by `conversation::tool_search`.
            Self::Client => super::tool_search::CLIENT_EXECUTION,
        }
    }
}

/// One searchable tool source advertised in the declaration's description.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ToolSearchSource<'a> {
    pub(super) name: &'a str,
    pub(super) description: Option<&'a str>,
}

/// Whether the declaration lists the enabled sources itself or leaves them to another surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ToolSearchSourceListing {
    Include,
    Omit,
}

/// The declaration's description: the donor `create_tool_search_tool` template byte for byte.
pub(super) fn tool_search_description(
    sources: &[ToolSearchSource<'_>],
    listing: ToolSearchSourceListing,
) -> String {
    let source_section = match listing {
        ToolSearchSourceListing::Include => format!(
            "\n\nYou have access to tools from the following sources:\n{}\n",
            render_tool_search_sources(sources)
        ),
        ToolSearchSourceListing::Omit => "\n\n".to_string(),
    };
    format!(
        "# Tool discovery\n\nSearches over deferred tool metadata with BM25 and exposes matching tools for the next model call.{source_section}Some of the tools may not have been provided to you upfront, and you should use this tool (`{TOOL_SEARCH_DECLARATION_TYPE}`) to search for the required tools. For MCP tool discovery, always use `{TOOL_SEARCH_DECLARATION_TYPE}` instead of `list_mcp_resources` or `list_mcp_resource_templates`."
    )
}

/// The declaration as a raw JSON tool entry. `rs::Tool` has no `tool_search` variant, so the
/// sampler's raw-JSON tool channel is the only way it can reach the wire. Key order is the
/// wire order: this crate builds serde_json with `preserve_order`.
///
/// `default_limit` is only interpolated into the `limit` description. The donor keeps it a
/// parameter of `create_tool_search_tool` (`tool_search_spec.rs:16`) too, whose only non-test
/// caller is `core/src/tools/handlers/tool_search.rs:146` — the three other call sites
/// (`tool_search_spec.rs:118`, `:161`, `:186`) are tests — and it always passes
/// `TOOL_SEARCH_DEFAULT_LIMIT: usize = 8` (donor `tools/src/tool_discovery.rs:7`, passed at
/// `core/src/tools/handlers/tool_search.rs:148`); the CX1 capture shows "Defaults to 8.".
pub(super) fn tool_search_declaration_entry(
    execution: ToolSearchExecution,
    sources: &[ToolSearchSource<'_>],
    listing: ToolSearchSourceListing,
    default_limit: usize,
) -> serde_json::Value {
    let limit_description =
        format!("Maximum number of tools to return. Defaults to {default_limit}.");
    serde_json::json!({
        "type": TOOL_SEARCH_DECLARATION_TYPE,
        "execution": execution.as_str(),
        "description": tool_search_description(sources, listing),
        "parameters": {
            "type": "object",
            "properties": {
                "limit": {
                    "type": "number",
                    "description": limit_description,
                },
                "query": {
                    "type": "string",
                    "description": "Search query for deferred tools.",
                },
            },
            "required": ["query"],
            "additionalProperties": false,
        },
    })
}

/// One line per source sorted by name: `- <name>`, plus `: <description>` while the shared
/// [`MAX_TOOL_SEARCH_SOURCE_DESCRIPTION_BYTES`] budget lasts. Names are never truncated: a
/// source whose `- <name>` line will not fit is skipped whole, and a cut description stops
/// on a UTF-8 char boundary. A re-listed name keeps one entry, its description coalesced.
/// Both the reservation and that fit test charge `name.len()`, i.e. UTF-8 BYTES, so a multi-byte
/// name reserves and gates its encoded length, not its code-point count
/// (`responses_tests::tool_search_source_listing_accounts_name_bytes_not_char_count`).
fn render_tool_search_sources(sources: &[ToolSearchSource<'_>]) -> String {
    let mut by_name: BTreeMap<&str, Option<&str>> = BTreeMap::new();
    for source in sources {
        by_name
            .entry(source.name)
            .and_modify(|existing| {
                if existing.is_none() {
                    *existing = source.description;
                }
            })
            .or_insert(source.description);
    }
    if by_name.is_empty() {
        return "None currently enabled.".to_string();
    }

    let reserved_name_bytes = by_name
        .keys()
        .fold(by_name.len().saturating_sub(1), |reserved, name| {
            reserved.saturating_add(2).saturating_add(name.len())
        });
    let mut description_budget =
        MAX_TOOL_SEARCH_SOURCE_DESCRIPTION_BYTES.saturating_sub(reserved_name_bytes);
    let mut rendered = String::new();
    for (name, description) in by_name {
        let separator_bytes = usize::from(!rendered.is_empty());
        let required = separator_bytes.saturating_add(2).saturating_add(name.len());
        if required > MAX_TOOL_SEARCH_SOURCE_DESCRIPTION_BYTES.saturating_sub(rendered.len()) {
            continue;
        }

        if !rendered.is_empty() {
            rendered.push('\n');
        }
        rendered.push_str("- ");
        rendered.push_str(name);

        if let Some(description) = description
            && description_budget >= 2
        {
            rendered.push_str(": ");
            description_budget -= 2;
            let bounded_description = truncate_bytes(description, description_budget);
            rendered.push_str(bounded_description);
            description_budget -= bounded_description.len();
        }
    }
    rendered
}

/// Whether a route may advertise the hosted `tool_search` declaration.
/// Both signals must hold, so they travel as named fields rather than positional booleans a
/// callsite could swap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SearchAdmission {
    /// The selected model row accepts the hosted `tool_search` declaration.
    pub(super) supports_search_tool: bool,
    /// At least one deferred tool exists to be found; supplied by the discovery manifest once it
    /// lands. Deliberately not derived from the `sources` handed to
    /// [`tool_search_declaration_entry`]: that advertised list may legitimately be empty or
    /// omitted and the donor still emits a declaration then
    /// (`core/src/tools/handlers/tool_search_spec.rs:48-49` renders "None currently enabled.").
    pub(super) has_searchable_tools: bool,
}

impl SearchAdmission {
    /// A route is admitted only when both signals hold. A discovery manifest, once it exists,
    /// maps onto `has_searchable_tools` with no change here.
    pub(super) fn admitted(self) -> bool {
        self.supports_search_tool && self.has_searchable_tools
    }
}

/// [`extra_tool_entries`] with the `tool_search` declaration placed first, ahead of the hosted
/// entries. `declaration: None` — what a non-admitted route passes — returns exactly
/// [`extra_tool_entries`], so unadmitted routes stay byte-identical.
///
/// D3-A / A-25: the decision is TOP-LEVEL placement regardless of `use_responses_lite`, and
/// nothing shapes `tools` per that flag today — it is catalog/config plumbing only, so the live
/// per-row proof is owed by bead `apex-waj.20`. `additional_tools` is deliberately not used for
/// it: the live probe R4 dropped a declaration sent through that container, so it would never
/// reach the model.
///
/// The sampler appends these entries to the serialized body's top-level `tools`, creating the array
/// when the serialized body carries no typed `tools` key — absent, or present but not an array
/// (`client.rs:886-889` `splice_extra_tool_entries`). A body that does carry one — the typed
/// `rs::Tool::Function` entries `build_responses_tools` emits for the request's client-declared
/// `ToolSpec`s, which is where a function tool and any MCP tool the harness declares both ride — is
/// extended in place instead. Hosted `web_search`/`x_search` never ride that typed channel: each
/// travels only as a raw-JSON entry (see [`extra_tool_entries`]; `client.rs:877-878`), because
/// emitting either as a typed `rs::Tool` as well is rejected as a duplicate. The declaration
/// therefore leads the hosted entries because this function pushes it first; the hosted entries then
/// follow the caller's `hosted_tools` slice order, so
/// `functions…, tool_search, web_search, x_search` is that slice's order, not a rule placed here.
///
/// Cache cost: the declaration is model-visible and can carry up to
/// `MAX_TOOL_SEARCH_SOURCE_DESCRIPTION_BYTES` of source text into that request-level `tools[]`.
/// `render_tool_search_sources` is deterministic for a fixed arrival order — sorted by name, one
/// entry per name. The coalesce keeps the first description a name ever carries that is not `None`
/// (an empty one counts), so a reshuffle is byte-neutral unless it changes which description that
/// first non-`None` arrival is. Duplicates carrying different descriptions can disagree about the
/// survivor, which can change the rendered bytes
/// (`tool_search_source_listing_first_description_wins_among_duplicates`); a re-order that keeps
/// the same survivor — any `None`-beside-a-description pair, pinned both ways by
/// `tool_search_source_listing_rendering` — is neutral, and so is a re-order of distinct names,
/// which the name-keyed sort erases entirely.
/// The pinned async-openai `CreateResponse` (workspace-root `Cargo.toml:4`, rev `4d72e1d`) derives
/// `Serialize` with `input` before `tools` (`types/responses/response.rs:588` vs `:727`), so a
/// `tools[]` change cannot move bytes that precede it; the provider's prompt-token order is not
/// knowable from this tree. The cap is donor-pinned; live cache-hit is owed by bead `apex-waj.20`.
///
/// The declaration is taken by value: [`tool_search_declaration_entry`] builds one per request
/// and its description can carry the whole source budget, so the channel never clones it.
pub(super) fn extra_tool_entries_with_declaration(
    hosted_tools: &[HostedTool],
    declaration: Option<serde_json::Value>,
) -> Vec<serde_json::Value> {
    let declaration_slots = usize::from(declaration.is_some());
    let mut entries = Vec::with_capacity(hosted_tools.len().saturating_add(declaration_slots));
    if let Some(declaration) = declaration {
        entries.push(declaration);
    }
    for tool in hosted_tools {
        entries.push(match tool {
            HostedTool::WebSearch { options } => match options {
                Some(o) => o.to_tool_entry(),
                None => WebSearchOptions::default().to_tool_entry(),
            },
            HostedTool::XSearch { options } => match options {
                Some(o) => o.to_tool_entry(),
                None => XSearchOptions::default().to_tool_entry(),
            },
        });
    }
    entries
}
