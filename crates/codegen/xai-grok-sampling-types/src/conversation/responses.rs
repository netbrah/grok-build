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
///
/// Fails closed (`Err`) instead of silently dropping an item this tree has no IR
/// carrier for — see the `ToolSearchCall`/`ToolSearchOutput` arm. Callers map the
/// `Err` to a failed turn (the pre-re-pin deserializer fatal), never to a silent
/// history loss (U16, round-3 R-3).
pub fn response_to_conversation_items(
    response: rs::Response,
) -> std::result::Result<Vec<ConversationItem>, crate::SamplingError> {
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
            // Fail-closed decode seam (U16, round-3 R-3): async-openai 0.42.1 models
            // `tool_search_call` and `tool_search_output`, but this tree has no IR
            // carrier for them yet (the `Discovery` variant lands with the pair-atomic
            // cut, apex-waj.21). Dropping them here would turn a fatal decode into a
            // silent history loss for items the proxy actually sends, so the projection
            // refuses and the turn is not committed — reproducing the fail-closed
            // turn-kill this seam had before the re-pin, when the SDK rejected these
            // items in the deserializer.
            rs::OutputItem::ToolSearchCall(_) | rs::OutputItem::ToolSearchOutput(_) => {
                return Err(crate::SamplingError::serialization_message(
                    "decode seam: a `tool_search_call`/`tool_search_output` item has no IR carrier in this tree; the turn was not committed (the carrier lands with the Discovery variant, apex-waj.21)",
                ));
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

    Ok(items)
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
        // Native tool-discovery item. async-openai 0.33.1 models no
        // `tool_search_call` / `tool_search_output` input item
        // (`async-openai-rs/.../responses/tool_search.rs:62-66` in the vendored
        // dependency), so the item can only reach the wire by splice — exactly the
        // `CodexRawInput` / `XSearch` precedent this arm copies.
        //
        // INVARIANT (one placeholder per registered splice):
        // `ConversationRequest::raw_responses_input_replacements` computes its
        // splice indices as the prefix sums of THIS function's output length, and
        // `patch_raw_input_replacements` (`xai-grok-sampler/src/client.rs:740`)
        // overwrites `input[index]` wholesale. Emit exactly ONE slot here, or the
        // splice lands on the neighbour item. Pinned by
        // `tool_search::tests::discovery_encoder_flattens_one_slot_per_item_and_the_splice_lands_in_that_slot`.
        //
        // The placeholder is the bounded `text_summary()` (§6.7), so a dialect that
        // deliberately splices nothing (the Xai row class — no wire evidence) still
        // tells the model that tools were loaded instead of losing the turn. On a
        // splicing dialect this exact slot is replaced by `raw()` before the request
        // leaves, so the placeholder never reaches a row that accepts the real item.
        ConversationItem::Discovery { item } => vec![rs::InputItem::EasyMessage(
            rs::EasyInputMessage {
                r#type: rs::MessageType::Message,
                role: rs::Role::Assistant,
                content: rs::EasyInputContent::Text(item.text_summary()),
                phase: None,
            },
        )],
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
///
/// Not the production route entry point: every Responses body site goes through
/// [`extra_tool_entries_for_route`], which is what decides whether the `tool_search`
/// declaration rides along. This variant is the declaration-less half of that pair and its
/// consumers are the un-admitted-route byte-parity pins (in-crate `responses_tests`, the
/// actor's parity test in `xai-chat-state`, the sampler/shell boot parity assertions) plus one
/// backend-search test in `acp_session_tests` — wiring a new body site through it would
/// silently bypass admission (apex-waj.35).
pub fn extra_tool_entries(hosted_tools: &[HostedTool]) -> Vec<serde_json::Value> {
    extra_tool_entries_with_declaration(hosted_tools, None)
}

/// The top-level `tools[]` entries a request carries, given the admission of the route sending
/// it: the hosted entries, preceded by the hosted `tool_search` declaration when the route is
/// admitted. `None` and every `admitted() == false` admission return exactly
/// [`extra_tool_entries`], so the un-admitted route stays byte-identical to the route that existed
/// before this function did — the blast-radius guarantee bead apex-waj.35 asks for, pinned by
/// `responses_tests::unadmitted_route_entries_stay_byte_identical`.
///
/// This is the one place the declaration is assembled for a route, which is why ruling
/// `map/RULINGS-o1o5.md` §D5 puts the `execution` choice here. It is
/// `ToolSearchExecution::Client` and it must stay so: the live strict row `gpt-5.6-sol` (Azure
/// through the proxy) answers `execution: "server"` with **400** whenever the entry carries a
/// `description` or `parameters`, and `tool_search_declaration_entry` emits both, so an
/// `execution: Server` pairing below would 400 every request on that row, and no type prevents
/// building that pair. `client` + `description` + `parameters` is the donor form: it returns 200 on
/// that row and mints a `tool_search_call` whose answer is ours to produce. Producing that answer is
/// NOT this tree's capability yet — the ingest arm in [`response_to_conversation_items`] refuses a
/// `tool_search_call`/`tool_search_output` item because the pair-atomic decode is owed by
/// apex-waj.21/.5, and `xai-grok-sampler`'s streaming path turns that refusal into a FAILED turn,
/// not a silent drop. Emitting this entry therefore arms a route the harness can only lose on until
/// that lands; the entry stays because ruling D5 rules the *shape*, and the ordering of the two hops
/// is the coordinator's call. The emitted value is pinned by
/// `responses_tests::admitted_route_declaration_is_client_executed`.
///
/// The declaration advertises no sources. `ToolSearchSourceListing::Omit` is the honest interim:
/// there is no `DiscoveryManifest` to list (ruling D1 names the manifest as the future body of
/// [`has_searchable_tools`], not of this list), and the donor renders "None currently enabled." for
/// an empty set anyway (`tool_search_spec.rs:48-49`). It also keeps the entry byte-stable across
/// requests, which is what the cache-cost note on `extra_tool_entries_with_declaration` demands
/// of a model-visible fragment; a real source list becomes correct when the manifest lands.
///
/// Called by all three Responses-wire body sites in `xai-grok-sampler/src/client.rs`
/// (`codex_compaction_request_body`, `conversation_stream_responses` and
/// `conversation_responses`), each passing `ConversationRequest::search_admission` — the
/// admission carried WITH the request, never one held on the client, because
/// `ClientDefaults` is built once from `SamplerConfig` while the row changes mid-session
/// through `update_sampling_config`. The request-side writers are two: `xai-chat-state`'s
/// `build_conversation_request` (`xai-chat-state/src/actor/request_builder.rs:88`) for the turn,
/// and `xai-grok-shell`'s `parent_cached_request`
/// (`xai-grok-shell/src/session/acp_session_impl/side_call.rs:139`) for a cache-aligned auxiliary
/// call; each passes the surface THAT request is about to send (ruling D1's authoritative surface)
/// rather than the switch-time snapshot the projector carries. One of the three sites still has a
/// consumer with no production producer: `codex_compaction_request_body` is reached in src only by
/// `xai-grok-shell`'s Codex remote-compaction request (`session/compaction.rs:1039-1042`, sent at
/// `session/compaction.rs:1051`), which leaves `search_admission` at `Default`, so that body emits
/// the un-admitted entries — owed on apex-waj.35 beside the `helpers/session_compact.rs:630` door.
/// Pinned at the body level by
/// `client::tests::admitted_route_sends_the_declaration_on_the_codex_compaction_body`,
/// `client::tests::admitted_route_sends_the_declaration_on_both_responses_send_paths` and
/// `client::tests::unadmitted_route_sends_the_same_bytes_as_the_admission_less_call`.
pub fn extra_tool_entries_for_route(
    hosted_tools: &[HostedTool],
    admission: Option<SearchAdmission>,
) -> Vec<serde_json::Value> {
    if !admission.is_some_and(SearchAdmission::admitted) {
        return extra_tool_entries_with_declaration(hosted_tools, None);
    }
    extra_tool_entries_with_declaration(
        hosted_tools,
        Some(tool_search_declaration_entry(
            ToolSearchExecution::Client,
            &[],
            ToolSearchSourceListing::Omit,
            TOOL_SEARCH_DEFAULT_LIMIT,
        )),
    )
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
///
/// Public because the top-level placement rule lives in the consumer: the serialized body already
/// carries typed `tools` for any request that declares a function tool, and only the sampler's
/// splice sees both that array and this channel, so it is what has to recognise this tag to hoist
/// the declaration to `tools[0]` (`xai-grok-sampler/src/client.rs::splice_extra_tool_entries`).
/// A duplicated literal there could drift from the emitted entry in silence.
pub const TOOL_SEARCH_DECLARATION_TYPE: &str = "tool_search";

/// Cap on the whole rendered source list, shared by every source's description.
/// Donor parity: `core/src/tools/handlers/tool_search_spec.rs:8`.
pub(super) const MAX_TOOL_SEARCH_SOURCE_DESCRIPTION_BYTES: usize = 512 * 1024;

/// The `limit` the declaration documents as its default. Donor parity, openai/codex@af1fc2db:
/// `TOOL_SEARCH_DEFAULT_LIMIT` (`tools/src/tool_discovery.rs:7`) is what the donor passes to
/// `create_tool_search_tool` (`core/src/tools/handlers/tool_search.rs:148`), and its `handle_call`
/// falls back to that const when the model omits `limit` (`:212`). It is only interpolated into
/// the `limit` description (see `tool_search_declaration_limit_description_tracks_default_limit`),
/// never into the prose, so it is a wire-visible constant rather than a knob.
///
/// Visible to the crate's tests so they can pin the PRODUCTION declaration against it
/// (`responses_tests::admitted_route_declaration_is_client_executed`): the donor capture says
/// "Defaults to 8.", so a const that moved without the capture being re-baked must redden.
pub(super) const TOOL_SEARCH_DEFAULT_LIMIT: usize = 8;

/// Who executes a `tool_search` call. These are the only values the wire accepts, so the
/// donor's `sync` — which the API 400s — has no variant here rather than a runtime check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ToolSearchExecution {
    // Constructed only by `responses_tests::tool_search_declaration_execution_is_total`, which
    // exists precisely to pin the bytes ruling D5 rejects. Nothing may construct it in the lib:
    // the live strict row 400s `server` paired with a description/parameters, which
    // `tool_search_declaration_entry` always emits — hence the two named landmines in D5 are the
    // pair, not the variant.
    #[allow(dead_code)]
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
    // Constructed only by `responses_tests`, which exercises the rendered source block and its
    // byte budget. The production route keys off `Omit`: there is no `DiscoveryManifest` to list
    // yet, so advertising sources here would name tools the request does not carry. It becomes a
    // production value when the manifest lands — see `extra_tool_entries_for_route`.
    #[allow(dead_code)]
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
/// Both signals must hold. They are exposed as named fields so a callsite can say which is which,
/// and so a fixture can name the pair it wants: `SearchAdmission { supports_search_tool: row_flag,
/// has_searchable_tools: surface_non_empty }` is the swap-proof literal. Production does not write
/// it by hand — it calls [`Self::for_row`], which takes the surface signal only through
/// [`has_searchable_tools`].
///
/// Public with all three of its parts (type, fields, [`Self::admitted`]) plus [`Self::for_row`] as
/// the construction path, because the route tuple's caller chain lives in another crate
/// (`xai-chat-state` -> `project_switch_history`, bead apex-waj.35, SPEC-W2 PA-3). Half a widening
/// would be invisible here: `private_interfaces` is a rustc WARN and the workspace declares only
/// `[workspace.lints.clippy]`, so a nameable-but-unbuildable type silences the lint and breaks the
/// consumer quietly. See also [`crate::conversation::projection::TargetRoute`], which this rides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchAdmission {
    /// The selected model row accepts the hosted `tool_search` declaration. Production writer:
    /// the operator's per-row `supports_search_tool` tri-state, resolved in
    /// `xai-grok-shell/src/agent/config.rs` and read off the target row at the switch site.
    pub supports_search_tool: bool,
    /// At least one deferred tool exists to be found. Written by exactly one function,
    /// [`has_searchable_tools`], whose interim body is the declared tool surface — see ruling D1
    /// there for why that body is not the discovery manifest yet and why the manifest, when it
    /// lands, replaces that body and nothing else here.
    ///
    /// Deliberately not derived from the `sources` handed to
    /// [`tool_search_declaration_entry`]: that advertised list may legitimately be empty or
    /// omitted and the donor still emits a declaration then
    /// (`core/src/tools/handlers/tool_search_spec.rs:48-49` renders "None currently enabled.").
    pub has_searchable_tools: bool,
}

impl SearchAdmission {
    /// The construction path for anything assembling an admission for a real route (SPEC-W2 PA-3
    /// requires the type be buildable from another crate): the row's own `supports_search_tool` flag
    /// plus the tool surface that route declares, with the second signal taken ONLY through
    /// [`has_searchable_tools`]. A caller cannot pre-fold or bypass the producer through this
    /// constructor, which is what keeps ruling D1's "one producer" a property of the type and not a
    /// convention — the manifest, when it lands, replaces that function's body and every route
    /// follows it.
    ///
    /// Before this constructor the `pub(super)` struct literal was the ONLY construction
    /// path, and a positional alternative would have been unsound rather than merely
    /// awkward: `new(a, b)` and `new(b, a)` both compile and [`Self::admitted`] folds them
    /// with a symmetric `&&`, so a swap would be invisible through the whole path (AGENTS §7
    /// bans exactly this callsite shape). A test that needs a pair the producer cannot emit —
    /// the asymmetric cells that pin which field carries which signal — uses the field-named
    /// literal, which cannot be swapped by accident.
    ///
    /// Freshness is still the caller's obligation (see [`has_searchable_tools`]): pass the surface the
    /// route is about to send, not a stale snapshot.
    pub fn for_row(
        supports_search_tool: bool,
        declared_tools: &[impl DeclaredToolSurface],
    ) -> Self {
        Self {
            supports_search_tool,
            has_searchable_tools: has_searchable_tools(declared_tools),
        }
    }

    /// A route is admitted only when both signals hold. A discovery manifest, once it exists,
    /// maps onto `has_searchable_tools` with no change here.
    ///
    /// The fold is symmetric, so it cannot reveal a swapped pair — only the named fields can, which
    /// is why the tests that build the tuple read the fields back.
    pub fn admitted(self) -> bool {
        self.supports_search_tool && self.has_searchable_tools
    }
}

mod sealed {
    /// Seals [`super::DeclaredToolSurface`] so only this crate can name an implementor: an
    /// unbounded type parameter would let `has_searchable_tools(&[1u8, 2])` open the admission
    /// gate, which is the failure ruling D1 exists to prevent.
    pub trait DeclaredToolSurfaceSealed {}
}

/// A tool-declaration surface [`has_searchable_tools`] may be asked about — the two shapes the
/// harness actually declares tools with: [`crate::conversation::ToolSpec`] (the request's own
/// `tools`) and [`crate::types::ToolDefinition`] (the shell's tool-bridge surface, which is
/// `xai_tool_types`' type re-exported by `xai-grok-tools`, so one impl covers both spellings).
/// Sealed: a third shape must be ruled into D1 here, not passed by a caller.
pub trait DeclaredToolSurface: sealed::DeclaredToolSurfaceSealed {}
impl sealed::DeclaredToolSurfaceSealed for super::ToolSpec {}
impl sealed::DeclaredToolSurfaceSealed for crate::types::ToolDefinition {}
impl DeclaredToolSurface for super::ToolSpec {}
impl DeclaredToolSurface for crate::types::ToolDefinition {}

/// The single producer of [`SearchAdmission::has_searchable_tools`] (bead apex-waj.35, ruling
/// `map/RULINGS-o1o5.md` §D1). Every writer of that signal goes through this function, so the
/// manifest is a body change rather than a scatter of call-site edits: when `DiscoveryManifest`
/// lands it replaces the body below and [`SearchAdmission::admitted`] does not move.
///
/// Interim body, as ruled: a route whose declared tool surface is non-empty is searchable. The
/// argument is the surface the route actually declares, bounded to [`DeclaredToolSurface`] so the
/// ruled input is the only input that compiles.
///
/// Freshness is the caller's obligation, not this function's: the surface must be the one the
/// route is about to send. A switch-time snapshot (the shell's tool-bridge definitions) is valid
/// only for the projector, which does not read the admission yet; the request-side producer —
/// `xai-chat-state`'s `build_conversation_request` — passes `ConversationRequest::tools`, because
/// a session whose tools changed after the switch (MCP connect/disconnect, preset change) has a
/// stale snapshot.
///
/// Two derivations the ruling rejects, recorded here so they are not re-derived: the `sources` list
/// rendered into the declaration description (see [`SearchAdmission::has_searchable_tools`]), and
/// "a deferred tool exists" keyed on `ToolExposure::Deferred`, which has no production writer at
/// this head and would therefore keep `admitted()` false in every live session.
pub fn has_searchable_tools<T: DeclaredToolSurface>(declared_tools: &[T]) -> bool {
    !declared_tools.is_empty()
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
/// The sampler splices these entries into the serialized body's top-level `tools`, creating the
/// array when the serialized body carries no typed `tools` key — absent, or present but not an array
/// (`client.rs` `splice_extra_tool_entries`). A body that does carry one — the typed
/// `rs::Tool::Function` entries `build_responses_tools` emits for the request's client-declared
/// `ToolSpec`s, which is where a function tool and any MCP tool the harness declares both ride — is
/// extended in place, with this declaration hoisted to `tools[0]` of the final array: the producer
/// puts it first here and the splice is what keeps it first in front of the typed entries. Hosted
/// `web_search`/`x_search` never ride that typed channel: each
/// travels only as a raw-JSON entry (see [`extra_tool_entries`]; `client.rs:877-878`), because
/// emitting either as a typed `rs::Tool` as well is rejected as a duplicate.
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
