//! Wire builder for the Anthropic `/v1/messages` request.
//!
//! The builder pipeline (MW-1 spec D5, in order) repairs the translated
//! history before it is serialized:
//!
//! 1. **item-level orphan cleanup** ([`clean_orphaned_items`]) — tool calls
//!    with no result and results with no call are removed pre-translation;
//! 2. **translate** — items become `Message`s (this file's mapping loop);
//! 3. **message-level adjacency cleanup** ([`clean_orphaned_blocks_by_adjacency`])
//!    — a `tool_use` survives only if the immediately following user message
//!    carries its `tool_result`, and vice versa; emptied messages are removed;
//! 4. **three-part thinking strip** ([`strip_thinking_blocks`]) — non-latest
//!    assistant messages lose all thinking blocks; the latest keeps a block
//!    only as a verbatim (text, signature) pair; signature-only blocks are
//!    dropped; emptied assistant messages are removed;
//! 5. **cache-control window** ([`apply_cache_breakpoints`]) — runs last.
//!
//! Stages 4 (hoist) and 6 (trailing-assistant repair) of the full MW-1 D5
//! order land in the follow-up commit of this series; the order above is the
//! canonical pipeline this module implements.

use super::*;

/// D5 stage 1 — item-level orphan cleanup (pre-translation).
///
/// A tool call with no matching result, and a result with no matching call,
/// is unsendable: the Anthropic API rejects unpaired tool_use/tool_result
/// with a 400, so both sides are removed before translation. Pairing is by
/// the stored (unsanitized) id, mirroring the port source; the message-level
/// stage re-checks pairing on the sanitized wire ids.
pub(crate) fn clean_orphaned_items(items: &[ConversationItem]) -> Vec<ConversationItem> {
    use std::collections::HashSet;

    let mut call_ids: HashSet<&str> = HashSet::new();
    let mut result_ids: HashSet<&str> = HashSet::new();
    for item in items {
        match item {
            ConversationItem::Assistant(a) => {
                for tc in &a.tool_calls {
                    call_ids.insert(&tc.id);
                }
            }
            ConversationItem::ToolResult(t) => {
                result_ids.insert(&t.tool_call_id);
            }
            _ => {}
        }
    }
    let paired: HashSet<&str> = call_ids.intersection(&result_ids).map(|id| *id).collect();

    items
        .iter()
        .filter_map(|item| match item {
            ConversationItem::Assistant(a) => {
                let kept_calls: Vec<ToolCall> = a
                    .tool_calls
                    .iter()
                    .filter(|tc| paired.contains(&tc.id[..]))
                    .cloned()
                    .collect();
                if kept_calls.is_empty() && a.content.is_empty() {
                    // The item carries nothing wire-visible once the orphaned
                    // calls are gone; drop it rather than emit an empty turn.
                    None
                } else {
                    Some(ConversationItem::Assistant(AssistantItem {
                        tool_calls: kept_calls,
                        ..a.clone()
                    }))
                }
            }
            ConversationItem::ToolResult(t) => paired
                .contains(t.tool_call_id.as_str())
                .then(|| item.clone()),
            other => Some(other.clone()),
        })
        .collect()
}

/// D5 stage 3 — message-level adjacency cleanup (post-translation).
///
/// The Anthropic wire requires the adjacency, not just the pairing: a
/// `tool_use` block survives only if the immediately following user message
/// carries a `tool_result` for it, and a `tool_result` only if the
/// immediately preceding assistant message carries the matching `tool_use`.
/// A non-adjacent pair (e.g. split by an injected user message) 400s, so
/// both sides are stripped. Messages emptied by the strip are removed.
pub(crate) fn clean_orphaned_blocks_by_adjacency(messages: &mut Vec<crate::messages::Message>) {
    use crate::messages::{ContentBlock, MessageContent, MessageRole};

    let len = messages.len();
    // Assistant pass: keep only tool_use blocks answered by the next user message.
    for i in 0..len {
        if !matches!(messages[i].role, MessageRole::Assistant) {
            continue;
        }
        // Immutable phases first so the neighbour lookups do not fight the
        // later mutable strip.
        let use_ids: std::collections::HashSet<String> = match &messages[i].content {
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                    _ => None,
                })
                .collect(),
            MessageContent::Text(_) => continue,
        };
        if use_ids.is_empty() {
            continue;
        }
        let next_result_ids: std::collections::HashSet<String> = messages
            .get(i + 1)
            .filter(|m| matches!(m.role, MessageRole::User))
            .and_then(|m| match &m.content {
                MessageContent::Blocks(blocks) => Some(
                    blocks
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::ToolResult { tool_use_id, .. } => {
                                Some(tool_use_id.clone())
                            }
                            _ => None,
                        })
                        .collect(),
                ),
                MessageContent::Text(_) => None,
            })
            .unwrap_or_default();
        let matched: std::collections::HashSet<&String> =
            use_ids.intersection(&next_result_ids).collect();
        if matched.len() < use_ids.len() {
            tracing::debug!(
                stripped = use_ids.len() - matched.len(),
                "MW-1: stripping non-adjacent tool_use block(s) from assistant message {i}"
            );
            if let MessageContent::Blocks(blocks) = &mut messages[i].content {
                blocks.retain(|b| match b {
                    ContentBlock::ToolUse { id, .. } => {
                        matched.iter().any(|m| m.as_str() == id.as_str())
                    }
                    _ => true,
                });
            }
        }
    }
    // User pass: keep only tool_result blocks paired with the preceding assistant.
    for i in 0..len {
        if !matches!(messages[i].role, MessageRole::User) {
            continue;
        }
        let result_ids: std::collections::HashSet<String> = match &messages[i].content {
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
                    _ => None,
                })
                .collect(),
            MessageContent::Text(_) => continue,
        };
        if result_ids.is_empty() {
            continue;
        }
        let prev_use_ids: std::collections::HashSet<String> =
            if i > 0 && matches!(messages[i - 1].role, MessageRole::Assistant) {
                match &messages[i - 1].content {
                    MessageContent::Blocks(blocks) => blocks
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                            _ => None,
                        })
                        .collect(),
                    MessageContent::Text(_) => std::collections::HashSet::new(),
                }
            } else {
                std::collections::HashSet::new()
            };
        let matched: std::collections::HashSet<&String> =
            result_ids.intersection(&prev_use_ids).collect();
        if matched.len() < result_ids.len() {
            tracing::debug!(
                stripped = result_ids.len() - matched.len(),
                "MW-1: stripping non-adjacent tool_result block(s) from user message {i}"
            );
            if let MessageContent::Blocks(blocks) = &mut messages[i].content {
                blocks.retain(|b| match b {
                    ContentBlock::ToolResult { tool_use_id, .. } => {
                        matched.iter().any(|m| m.as_str() == tool_use_id.as_str())
                    }
                    _ => true,
                });
            }
        }
    }
    // Remove messages whose content became empty after stripping.
    messages.retain(|m| !matches!(&m.content, MessageContent::Blocks(blocks) if blocks.is_empty()));
}

/// D5 stage 5 — the three-part thinking replay rule (xli S-031, spec rule 2).
///
/// (a) every assistant message before the latest loses ALL thinking blocks —
/// the API only requires the latest assistant's thinking to replay
/// verbatim, and earlier turns are safe to strip;
/// (b) the latest assistant message keeps a `Thinking` block only if it
/// carries both thinking text and a non-empty signature — the verbatim pair
/// as stored by the stream consumer, which is exactly what Anthropic's
/// "thinking blocks in the latest assistant message cannot be modified"
/// check verifies;
/// (c) a signature-only block (empty thinking text, the opus47 shape) is
/// dropped entirely — there is nothing to replay.
///
/// `RedactedThinking` blocks are dropped at every position: grok V1 has no
/// storage path that produces one with verifiable provenance (the stream
/// consumer drops redacted blocks), so none is ever trustworthy here.
/// Assistant messages left empty by the strip are removed.
pub(crate) fn strip_thinking_blocks(messages: &mut Vec<crate::messages::Message>) {
    use crate::messages::{ContentBlock, MessageContent, MessageRole};

    let Some(last_idx) = messages
        .iter()
        .rposition(|m| matches!(m.role, MessageRole::Assistant))
    else {
        return;
    };
    for (i, msg) in messages.iter_mut().enumerate() {
        if !matches!(msg.role, MessageRole::Assistant) {
            continue;
        }
        let MessageContent::Blocks(blocks) = &mut msg.content else {
            continue;
        };
        if i < last_idx {
            // Non-latest assistant: drop all thinking blocks.
            blocks.retain(|b| {
                !matches!(
                    b,
                    ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. }
                )
            });
        } else {
            // Latest assistant: only the verbatim (text, signature) pair
            // survives; unsigned and signature-only blocks are dropped.
            blocks.retain(|b| match b {
                ContentBlock::Thinking {
                    thinking,
                    signature,
                } => !thinking.is_empty() && !signature.is_empty(),
                ContentBlock::RedactedThinking { .. } => false,
                _ => true,
            });
        }
    }
    // Remove assistant messages left empty by the strip.
    messages.retain(|m| {
        !(matches!(m.role, MessageRole::Assistant)
            && matches!(&m.content, MessageContent::Blocks(blocks) if blocks.is_empty()))
    });
}

/// Marks the last block that can carry one, scanning back past `Thinking`, which the API rejects a breakpoint on.
fn mark_message_cache_breakpoint(msg: &mut crate::messages::Message) -> bool {
    use crate::messages::{CacheControl, ContentBlock, MessageContent};

    match &mut msg.content {
        MessageContent::Blocks(blocks) => {
            for block in blocks.iter_mut().rev() {
                let cache_control = match block {
                    ContentBlock::Text { cache_control, .. }
                    | ContentBlock::ToolResult { cache_control, .. }
                    | ContentBlock::Image { cache_control, .. }
                    | ContentBlock::ToolUse { cache_control, .. } => cache_control,
                    ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. } => {
                        continue;
                    }
                };
                *cache_control = Some(CacheControl::ephemeral());
                return true;
            }
            false
        }
        // Plain text cannot carry a breakpoint, so promote it to block form.
        MessageContent::Text(text) => {
            let text = std::mem::take(text);
            msg.content = MessageContent::Blocks(vec![ContentBlock::Text {
                text,
                cache_control: Some(CacheControl::ephemeral()),
            }]);
            true
        }
    }
}

/// An entry is written only at a breakpoint, so marking the system prompt alone leaves the transcript uncached.
/// The third covers a turn that appends more than the API's 20 block lookback.
/// The fourth slot stays free: a gateway that turns on automatic caching takes it, and five is rejected outright.
fn apply_cache_breakpoints(
    system_blocks: &mut [crate::messages::TextBlock],
    messages: &mut [crate::messages::Message],
) {
    use crate::messages::{CacheControl, MessageRole};

    if let Some(last) = system_blocks.last_mut() {
        last.cache_control = Some(CacheControl::ephemeral());
    }

    let tip = (0..messages.len())
        .rev()
        .find(|&i| mark_message_cache_breakpoint(&mut messages[i]));

    // Where the previous request ended
    // A turn can append several user messages in a row, so skip the whole trailing run rather than a neighbour of the tip
    if let Some(tip) = tip
        && let Some(prev) = messages[..tip]
            .iter()
            .rposition(|m| matches!(m.role, MessageRole::Assistant))
            .and_then(|assistant| {
                messages[..assistant]
                    .iter()
                    .rposition(|m| matches!(m.role, MessageRole::User))
            })
    {
        mark_message_cache_breakpoint(&mut messages[prev]);
    }
}

pub fn build_messages_request(req: &ConversationRequest) -> crate::messages::MessagesRequest {
    use crate::messages::{
        ContentBlock, ImageSource, Message, MessageContent, MessageRole, MessagesRequest,
        OutputConfig, SystemParam, TextBlock, ToolChoiceParam, ToolParam, ToolResultContent,
    };

    // D5 stage 1: item-level orphan cleanup, before translation.
    let items = clean_orphaned_items(&req.items);

    let mut system_blocks: Vec<TextBlock> = Vec::new();
    let mut messages: Vec<Message> = Vec::new();
    let mut pending_assistant: Vec<ContentBlock> = Vec::new();
    let mut pending_tool_results: Vec<ContentBlock> = Vec::new();

    let sanitize_tool_call_id = |id: &str| -> String {
        id.chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };

    let content_parts_to_anthropic_blocks = |parts: &[ContentPart]| -> Vec<ContentBlock> {
        parts
            .iter()
            .map(|part| match part {
                ContentPart::Text { text } => ContentBlock::Text {
                    text: text.as_ref().to_owned(),
                    cache_control: None,
                },
                ContentPart::Image { url } => {
                    if url.starts_with("data:") {
                        if let Some((header, data)) = url.split_once(',') {
                            let media_type = header
                                .strip_prefix("data:")
                                .and_then(|h| h.strip_suffix(";base64"))
                                .unwrap_or("image/png")
                                .to_string();
                            ContentBlock::Image {
                                source: ImageSource::Base64 {
                                    media_type,
                                    data: data.to_string(),
                                },
                                cache_control: None,
                            }
                        } else {
                            // Malformed data URI, treat as text
                            ContentBlock::Text {
                                text: format!("[invalid image: {}]", url),
                                cache_control: None,
                            }
                        }
                    } else if url.starts_with("http://") || url.starts_with("https://") {
                        ContentBlock::Image {
                            source: ImageSource::Url {
                                url: url.as_ref().to_owned(),
                            },
                            cache_control: None,
                        }
                    } else {
                        // Unknown format, treat as text
                        ContentBlock::Text {
                            text: format!("[image: {}]", url),
                            cache_control: None,
                        }
                    }
                }
            })
            .collect()
    };

    let flush_assistant = |pending: &mut Vec<ContentBlock>, msgs: &mut Vec<Message>| {
        if !pending.is_empty() {
            msgs.push(Message {
                role: MessageRole::Assistant,
                content: MessageContent::Blocks(pending.clone()),
            });
            pending.clear();
        }
    };

    let flush_tool_results = |pending: &mut Vec<ContentBlock>, msgs: &mut Vec<Message>| {
        if !pending.is_empty() {
            msgs.push(Message {
                role: MessageRole::User,
                content: MessageContent::Blocks(pending.clone()),
            });
            pending.clear();
        }
    };

    for item in &items {
        match item {
            ConversationItem::System(s) => {
                flush_assistant(&mut pending_assistant, &mut messages);
                flush_tool_results(&mut pending_tool_results, &mut messages);
                system_blocks.push(TextBlock {
                    r#type: "text".to_string(),
                    text: s.content.as_ref().to_owned(),
                    cache_control: None,
                });
            }
            ConversationItem::User(u) => {
                flush_assistant(&mut pending_assistant, &mut messages);
                flush_tool_results(&mut pending_tool_results, &mut messages);
                let blocks = content_parts_to_anthropic_blocks(&u.content);
                messages.push(Message {
                    role: MessageRole::User,
                    content: MessageContent::Blocks(blocks),
                });
            }
            ConversationItem::Assistant(a) => {
                flush_tool_results(&mut pending_tool_results, &mut messages);

                if !a.content.is_empty() {
                    pending_assistant.push(ContentBlock::Text {
                        text: a.content.as_ref().to_owned(),
                        cache_control: None,
                    });
                }

                for tc in &a.tool_calls {
                    let input =
                        serde_json::from_str(&tc.arguments).unwrap_or(serde_json::json!({}));
                    pending_assistant.push(ContentBlock::ToolUse {
                        id: sanitize_tool_call_id(&tc.id),
                        name: tc.name.clone(),
                        input,
                        cache_control: None,
                    });
                }
            }
            ConversationItem::ToolResult(t) => {
                flush_assistant(&mut pending_assistant, &mut messages);
                let content = if t.images.is_empty() {
                    ToolResultContent::Text(t.content.as_ref().to_owned())
                } else {
                    let mut blocks = vec![ContentBlock::Text {
                        text: t.content.as_ref().to_owned(),
                        cache_control: None,
                    }];
                    for img in &t.images {
                        if let ContentPart::Image { url } = img {
                            let source = if let Some(rest) = url.strip_prefix("data:") {
                                if let Some((media_type, data)) = rest.split_once(";base64,") {
                                    ImageSource::Base64 {
                                        media_type: media_type.to_string(),
                                        data: data.to_string(),
                                    }
                                } else {
                                    ImageSource::Url {
                                        url: url.as_ref().to_owned(),
                                    }
                                }
                            } else {
                                ImageSource::Url {
                                    url: url.as_ref().to_owned(),
                                }
                            };
                            blocks.push(ContentBlock::Image {
                                source,
                                cache_control: None,
                            });
                        }
                    }
                    ToolResultContent::Blocks(blocks)
                };
                pending_tool_results.push(ContentBlock::ToolResult {
                    tool_use_id: sanitize_tool_call_id(&t.tool_call_id),
                    content,
                    cache_control: None,
                });
            }
            // No native equivalent, so emit synthetic text to retain context.
            ConversationItem::BackendToolCall(b) => {
                flush_tool_results(&mut pending_tool_results, &mut messages);
                pending_assistant.push(ContentBlock::Text {
                    text: b.text_summary(),
                    cache_control: None,
                });
            }
            // `tco_*` blobs carry only `signature`; real reasoning sets `thinking`
            ConversationItem::Reasoning(r) => {
                flush_tool_results(&mut pending_tool_results, &mut messages);
                let thinking = reasoning_item_text(r);
                let signature = r
                    .encrypted_content
                    .as_deref()
                    .map(str::to_owned)
                    .unwrap_or_default();
                if !thinking.is_empty() || !signature.is_empty() {
                    pending_assistant.push(ContentBlock::Thinking {
                        thinking,
                        signature,
                    });
                }
            }
        }
    }

    flush_assistant(&mut pending_assistant, &mut messages);
    flush_tool_results(&mut pending_tool_results, &mut messages);

    // D5 stages 3 and 5: message-level adjacency cleanup, then the three-part
    // thinking strip (the hoist and trailing-repair stages of the full D5
    // order land in the follow-up commit of this series).
    clean_orphaned_blocks_by_adjacency(&mut messages);
    strip_thinking_blocks(&mut messages);

    apply_cache_breakpoints(&mut system_blocks, &mut messages);

    let system: Option<SystemParam> = if system_blocks.is_empty() {
        None
    } else if system_blocks.len() == 1 && system_blocks[0].cache_control.is_none() {
        Some(SystemParam::Text(system_blocks[0].text.clone()))
    } else {
        Some(SystemParam::Blocks(system_blocks))
    };

    let tools: Option<Vec<ToolParam>> = if req.tools.is_empty() {
        None
    } else {
        Some(
            req.tools
                .iter()
                .map(|t| ToolParam {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    input_schema: t.parameters.clone(),
                })
                .collect(),
        )
    };

    let tool_choice: Option<ToolChoiceParam> = req.tool_choice.as_ref().map(|tc| match tc {
        ConversationToolChoice::Auto => ToolChoiceParam::Auto,
        ConversationToolChoice::Required => ToolChoiceParam::Any,
        ConversationToolChoice::Function(name) => ToolChoiceParam::Tool { name: name.clone() },
        ConversationToolChoice::None => ToolChoiceParam::Auto, // ToolChoiceParam has no none variant, so fall back to the default
    });

    let effort = req
        .reasoning_effort
        .and_then(|e| e.to_messages_api())
        .map(|s| s.to_string());

    // A wire schema here suppresses tool calls, so the agent routes structured output through the StructuredOutput tool instead
    let format = req
        .json_schema
        .as_ref()
        .map(|schema| crate::messages::OutputFormat::JsonSchema {
            schema: schema.clone(),
        });

    // thinking is driven by reasoning_effort only, not by json_schema.
    let thinking = effort
        .as_ref()
        .map(|_| crate::messages::ThinkingConfig::Adaptive {
            display: Some(crate::messages::ThinkingDisplay::Summarized),
        });

    let output_config = if effort.is_some() || format.is_some() {
        Some(OutputConfig { effort, format })
    } else {
        None
    };

    MessagesRequest {
        model: req.model.clone().unwrap_or_default(),
        messages,
        max_tokens: req.max_output_tokens.unwrap_or(0),
        system,
        tools,
        tool_choice,
        temperature: req.temperature,
        top_p: req.top_p,
        top_k: None,
        stream: None, // The caller sets this
        stop_sequences: None,
        thinking,
        output_config,
        metadata: None,
    }
}

/// `Thinking` is dropped because this `From` returns a single item; the streaming consumer emits the sibling `Reasoning` item instead.
impl From<crate::messages::MessagesResponse> for ConversationItem {
    fn from(resp: crate::messages::MessagesResponse) -> Self {
        use crate::messages::ContentBlock;

        let mut content = String::new();
        let mut tool_calls = Vec::new();

        for block in resp.content {
            match block {
                ContentBlock::Text { text, .. } => {
                    if !content.is_empty() {
                        content.push('\n');
                    }
                    content.push_str(&text);
                }
                ContentBlock::ToolUse {
                    id, name, input, ..
                } => {
                    tool_calls.push(ToolCall {
                        id: Arc::<str>::from(id),
                        name,
                        arguments: Arc::<str>::from(
                            serde_json::to_string(&input).unwrap_or_default(),
                        ),
                    });
                }
                // Thinking is dropped; see the doc comment above
                ContentBlock::Thinking { .. } => {}
                _ => {} // Image and ToolResult are not expected in assistant responses
            }
        }

        ConversationItem::Assistant(AssistantItem {
            content: Arc::<str>::from(content),
            tool_calls,
            model_id: Some(resp.model),
            model_fingerprint: None,
            reasoning_effort: None,
        })
    }
}
