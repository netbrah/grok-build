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
                items.push(ConversationItem::Reasoning(r));
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
            ConversationToolChoice::Auto => rs::ToolChoiceParam::Mode(rs::ToolChoiceOptions::Auto),
            ConversationToolChoice::None => rs::ToolChoiceParam::Mode(rs::ToolChoiceOptions::None),
            ConversationToolChoice::Required => {
                rs::ToolChoiceParam::Mode(rs::ToolChoiceOptions::Required)
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
                        schema: Some(schema.clone()),
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

pub(super) fn conversation_item_to_input_items(item: &ConversationItem) -> Vec<rs::InputItem> {
    match item {
        ConversationItem::System(s) => {
            vec![rs::InputItem::EasyMessage(rs::EasyInputMessage {
                r#type: rs::MessageType::Message,
                role: rs::Role::System,
                content: rs::EasyInputContent::Text(s.content.as_ref().to_owned()),
            })]
        }
        ConversationItem::User(u) => {
            let content = content_parts_to_easy_input_content(&u.content);
            vec![rs::InputItem::EasyMessage(rs::EasyInputMessage {
                r#type: rs::MessageType::Message,
                role: rs::Role::User,
                content,
            })]
        }
        ConversationItem::Reasoning(r) => {
            // `status` is output-only and rejected on input.
            let mut r = r.clone();
            r.status = None;
            vec![rs::InputItem::Item(rs::Item::Reasoning(r))]
        }
        ConversationItem::Assistant(a) => {
            let mut items = Vec::new();

            if !a.content.is_empty() {
                items.push(rs::InputItem::EasyMessage(rs::EasyInputMessage {
                    r#type: rs::MessageType::Message,
                    role: rs::Role::Assistant,
                    content: rs::EasyInputContent::Text(a.content.as_ref().to_owned()),
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
                    },
                )));
            }

            items
        }
        ConversationItem::ToolResult(t) => {
            let output = if t.images.is_empty() {
                rs::FunctionCallOutput::Text(t.content.as_ref().to_owned())
            } else {
                let mut parts: Vec<rs::InputContent> =
                    vec![rs::InputContent::InputText(rs::InputTextContent {
                        text: t.content.as_ref().to_owned(),
                    })];
                for img in &t.images {
                    if let ContentPart::Image { url } = img {
                        parts.push(rs::InputContent::InputImage(rs::InputImageContent {
                            detail: rs::ImageDetail::Auto,
                            file_id: None,
                            image_url: Some(url.as_ref().to_owned()),
                        }));
                    }
                }
                rs::FunctionCallOutput::Content(parts)
            };
            vec![rs::InputItem::Item(rs::Item::FunctionCallOutput(
                rs::FunctionCallOutputItemParam {
                    call_id: t.tool_call_id.clone(),
                    output,
                    id: None,
                    status: None,
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
            }),
            ContentPart::Image { url } => rs::InputContent::InputImage(rs::InputImageContent {
                image_url: Some(url.as_ref().to_owned()),
                file_id: None,
                detail: rs::ImageDetail::default(),
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
            })
        })
        .collect();

    tools
}

/// Every hosted tool as a raw JSON entry, which the sampler client splices into the serialized `tools` array.
/// `web_search` rides it because async_openai's `rs::WebSearchToolFilters` models only `allowed_domains` and cannot carry `excluded_domains`.
/// Emitting either as a typed `rs::Tool` as well would send it twice, which the API rejects as a duplicate.
pub fn extra_tool_entries(hosted_tools: &[HostedTool]) -> Vec<serde_json::Value> {
    let mut entries = Vec::new();
    for tool in hosted_tools {
        match tool {
            HostedTool::WebSearch { options } => {
                entries.push(match options {
                    Some(o) => o.to_tool_entry(),
                    None => WebSearchOptions::default().to_tool_entry(),
                });
            }
            HostedTool::XSearch { options } => {
                entries.push(match options {
                    Some(o) => o.to_tool_entry(),
                    None => XSearchOptions::default().to_tool_entry(),
                });
            }
        }
    }
    entries
}
