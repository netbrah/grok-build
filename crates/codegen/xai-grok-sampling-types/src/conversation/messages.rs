//! Wire builder for the Anthropic `/v1/messages` request.
//!
//! The builder pipeline (binding order) repairs the translated history
//! before it is serialized:
//!
//! 1. **item-level orphan cleanup** ([`clean_orphaned_items`], MW-1 D5
//!    stage 1) — tool calls with no result and results with no call are
//!    removed pre-translation;
//! 2. **translate** (MW-1 D5 stage 2) — items become `Message`s (this
//!    file's mapping loop). Two MW-2 rules run inside the translation:
//!    **R1 same-role merge** — consecutive user-role content (user text
//!    blocks AND tool_result blocks) accumulates into ONE user message
//!    (xli `append_to_role`, wire.rs:1071; Vertex/proxy-class shape
//!    determinism), and **R8 model-identity suppression** — an assistant
//!    item whose `model_id` mismatches the request model slug suppresses
//!    its sibling `Reasoning` items' Thinking blocks until the next
//!    Assistant/System flush (no-op for a None/empty request model);
//! 3. **message-level adjacency cleanup** (MW-1 D5 stage 3,
//!    [`clean_orphaned_blocks_by_adjacency`]) — a `tool_use` survives only
//!    if the immediately following user message carries its `tool_result`,
//!    and vice versa; emptied messages are removed (user and assistant);
//! 4. **leading-assistant repair** (MW-2 D2) — a message list beginning on
//!    an assistant message gets a synthetic leading user turn
//!    `[Continue]` (unconditional — a head has no result awaiting). Runs
//!    after the adjacency cleanup (so a stripped leading assistant cannot
//!    leave two consecutive user messages) and before the hoist/strip/
//!    trailing-repair;
//! 5. **tool_result hoist** (MW-1 D5 stage 4,
//!    [`hoist_tool_results_to_front`]) — within a user message,
//!    `tool_result` blocks are stable-partitioned to the front (Vertex
//!    backed endpoints reject the other ordering); live since R1 produces
//!    merged messages mixing tool_result and text blocks;
//! 6. **three-part thinking strip** (MW-1 D5 stage 5,
//!    [`strip_thinking_blocks`]) — non-latest assistant messages lose all
//!    thinking blocks; the latest keeps a block only as a verbatim (text,
//!    signature) pair; signature-only blocks are dropped; emptied assistant
//!    messages are removed. R8's suppression runs BEFORE this strip (the
//!    strip is a superset gate over what R8 leaves behind);
//! 7. **trailing-assistant repair** (MW-1 D5 stage 6,
//!    [`repair_trailing_assistant`]) — a history ending on an assistant
//!    message gets a synthetic user sentinel (`[Awaiting tool result]` /
//!    `[Continue]`), because the proxy-routed endpoints reject assistant
//!    prefill;
//! 8. **cache-control window** (MW-1 D5 stage 7,
//!    [`apply_cache_breakpoints`]) — runs last, so its tip can land on the
//!    synthetic sentinel user.

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

/// D5 stage 4 — hoist `tool_result` blocks to the front of their user
/// message (S-022).
///
/// Vertex-backed Claude endpoints reject a request where a user message
/// following a `tool_use`-bearing assistant message does not *begin* with
/// the matching `tool_result` block(s); the direct API accepts either
/// ordering, which is why the failure only surfaces on proxy/Vertex routes.
/// A stable partition (tool_result blocks first, everything else after,
/// relative order preserved within each group) restores the required
/// ordering. Runs after the adjacency stage, so every `tool_result` here
/// already pairs with the preceding assistant message.
///
/// Live since MW-2 R1: the same-role merge produces user messages mixing
/// tool_result and text blocks, so the stable partition (tool_result blocks
/// first, everything else after, relative order preserved) is what keeps the
/// merged shape in the Vertex-required ordering.
pub(crate) fn hoist_tool_results_to_front(messages: &mut [crate::messages::Message]) {
    use crate::messages::{ContentBlock, MessageContent, MessageRole};

    for msg in messages.iter_mut() {
        if !matches!(msg.role, MessageRole::User) {
            continue;
        }
        let MessageContent::Blocks(blocks) = &mut msg.content else {
            continue;
        };
        let result_count = blocks
            .iter()
            .filter(|b| matches!(b, ContentBlock::ToolResult { .. }))
            .count();
        if result_count == 0 {
            continue;
        }
        let leading_results = blocks
            .iter()
            .take_while(|b| matches!(b, ContentBlock::ToolResult { .. }))
            .count();
        if leading_results == result_count {
            continue;
        }
        // Stable partition: tool_result blocks first, everything else after.
        let owned = std::mem::take(blocks);
        let (results, rest): (Vec<ContentBlock>, Vec<ContentBlock>) = owned
            .into_iter()
            .partition(|b| matches!(b, ContentBlock::ToolResult { .. }));
        blocks.extend(results);
        blocks.extend(rest);
    }
}

/// D5 stage 6 — trailing-assistant repair (S-014).
///
/// The Vertex-backed endpoints the proxy routes to reject assistant prefill
/// ("This model does not support assistant message prefill"), so the repair
/// is unconditional — grok is proxy-routed and there is no
/// prefill-capability gate to consult. When the translated history ends on
/// an assistant message, a synthetic user message is appended whose single
/// text block is `"[Awaiting tool result]"` if the trailing assistant still
/// carries a `tool_use` block, else `"[Continue]"`. The sentinel is
/// model-visible: parity with the port source, not an oversight. Nothing is
/// appended when the history ends on a user message (a `tool_result`
/// included). Runs after the thinking strip and before the cache-control
/// window, so the window's tip lands on the synthetic message when it
/// exists.
pub(crate) fn repair_trailing_assistant(messages: &mut Vec<crate::messages::Message>) {
    use crate::messages::{ContentBlock, Message, MessageContent, MessageRole};

    let Some(last) = messages.last() else {
        return;
    };
    if !matches!(last.role, MessageRole::Assistant) {
        return;
    }
    let has_tool_use = match &last.content {
        MessageContent::Blocks(blocks) => blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. })),
        MessageContent::Text(_) => false,
    };
    let sentinel = if has_tool_use {
        "[Awaiting tool result]"
    } else {
        "[Continue]"
    };
    messages.push(Message {
        role: MessageRole::User,
        content: MessageContent::Blocks(vec![ContentBlock::Text {
            text: sentinel.to_string(),
            cache_control: None,
        }]),
    });
}

/// Marks the last block that can carry one, scanning back past `Thinking`, which the API rejects a breakpoint on.
fn mark_message_cache_breakpoint(msg: &mut crate::messages::Message) -> bool {
    mark_message_cache_breakpoint_with(msg, crate::messages::CacheControl::ephemeral())
}

fn mark_message_cache_breakpoint_with(
    msg: &mut crate::messages::Message,
    marker: crate::messages::CacheControl,
) -> bool {
    use crate::messages::{ContentBlock, MessageContent};

    match &mut msg.content {
        MessageContent::Blocks(blocks) => {
            for block in blocks.iter_mut().rev() {
                let cache_control = match block {
                    ContentBlock::Text { cache_control, .. }
                    | ContentBlock::ToolResult { cache_control, .. }
                    | ContentBlock::Image { cache_control, .. }
                    | ContentBlock::ToolUse { cache_control, .. } => cache_control,
                    ContentBlock::Thinking { .. }
                    | ContentBlock::RedactedThinking { .. }
                    | ContentBlock::Unknown { .. } => {
                        continue;
                    }
                };
                *cache_control = Some(marker.clone());
                return true;
            }
            false
        }
        // Plain text cannot carry a breakpoint, so promote it to block form.
        MessageContent::Text(text) => {
            let text = std::mem::take(text);
            msg.content = MessageContent::Blocks(vec![ContentBlock::Text {
                text,
                cache_control: Some(marker),
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
    head_ttl: Option<&str>,
) {
    use crate::messages::{CacheControl, MessageRole};

    let head = match head_ttl {
        Some(ttl) => CacheControl::ephemeral_with_ttl(ttl),
        None => CacheControl::ephemeral(),
    };

    if let Some(last) = system_blocks.last_mut() {
        last.cache_control = Some(head.clone());
    }

    let tip = (0..messages.len())
        .rev()
        .find(|&i| mark_message_cache_breakpoint(&mut messages[i]));

    // Where the previous request ended. R1's same-role merge keeps one user
    // message per user-role run, so the previous tip is the last user
    // message before the last assistant (pre-merge, a turn could append
    // several user messages in a row and the lookup had to skip the whole
    // trailing run).
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

    // ANTHROPIC-WIRE-1 (cut 3): with no system prefix the stable head is
    // the FIRST message. Marked LAST so it wins the head/previous-boundary
    // collision on that same message; without a configured tier no extra
    // breakpoint is added (the default wire stays byte-identical).
    if system_blocks.is_empty()
        && head_ttl.is_some()
        && let Some(first) = messages.first_mut()
    {
        mark_message_cache_breakpoint_with(first, head);
    }
}

/// Tool-call arguments must serialize as a JSON object (MW-2 R2).
///
/// Parseable non-object values and unparseable strings both coerce to `{}`:
/// grok's own tool calls are always object-shaped, so a non-object replayed
/// argument is a local artifact, and `{}` is the safe form that keeps the
/// `tool_use` block serializable. The unparseable case carries a
/// `tracing::warn!` (xli parity, wire.rs:173); the non-object arm is
/// hardening beyond xli (which guards only the unparseable case) and beyond
/// the pin, which types `ToolUseBlockParam.input` as `unknown`
/// (wirejig/refs/anthropic@d3d5028 messages.ts:2355).
fn tool_call_input(arguments: &str, tool_name: &str) -> serde_json::Value {
    match serde_json::from_str::<serde_json::Value>(arguments) {
        Ok(value) if value.is_object() => value,
        Ok(value) => {
            tracing::warn!(
                tool = %tool_name,
                %value,
                "tool_call arguments parsed to a non-object JSON value; substituting an empty object"
            );
            serde_json::json!({})
        }
        Err(err) => {
            tracing::warn!(
                tool = %tool_name,
                "unparseable tool_call arguments; substituting an empty object: {err}"
            );
            serde_json::json!({})
        }
    }
}

/// Parse an image reference into an Anthropic image source, or a labeled
/// text fallback (MW-2 R3).
///
/// One helper serves both the user-content and the tool-result image paths
/// (they diverged pre-MW-2: the tool-result path misclassified non-base64
/// `data:` URIs as Url sources). A well-formed
/// `data:<media_type>;base64,<data>` URI whose media type is in the pinned
/// closed union (wirejig/refs/anthropic@d3d5028
/// spec/src/resources/messages/messages.ts:204) becomes a `Base64` source
/// with the media type extracted exactly — no silent default. `http(s)://`
/// URLs become `Url` sources (the API accepts them). Everything else —
/// malformed data URIs, out-of-union media types, unknown schemes — degrades
/// to the `[invalid image: {url}]` text fallback so a broken reference never
/// fails the request.
fn image_source_or_fallback(url: &str) -> Result<crate::messages::ImageSource, String> {
    use crate::messages::ImageSource;

    if let Some(rest) = url.strip_prefix("data:") {
        if let Some((media_type, data)) = rest.split_once(";base64,") {
            return match media_type {
                "image/jpeg" | "image/png" | "image/gif" | "image/webp" => {
                    Ok(ImageSource::Base64 {
                        media_type: media_type.to_string(),
                        data: data.to_string(),
                    })
                }
                _ => Err(format!("[invalid image: {url}]")),
            };
        }
        return Err(format!("[invalid image: {url}]"));
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return Ok(ImageSource::Url {
            url: url.to_string(),
        });
    }
    Err(format!("[invalid image: {url}]"))
}

pub fn build_messages_request(req: &ConversationRequest) -> crate::messages::MessagesRequest {
    use crate::messages::{
        ContentBlock, Message, MessageContent, MessageRole, MessagesRequest, OutputConfig,
        SystemParam, TextBlock, ToolChoiceParam, ToolParam, ToolResultContent,
    };

    // D5 stage 1: item-level orphan cleanup, before translation.
    let items = clean_orphaned_items(&req.items);

    let mut system_blocks: Vec<TextBlock> = Vec::new();
    let mut messages: Vec<Message> = Vec::new();
    let mut pending_assistant: Vec<ContentBlock> = Vec::new();
    // R1: ONE user-role buffer — consecutive user text and tool_result
    // content merges into a single user message, flushed on an assistant
    // role change (or a System boundary).
    let mut pending_user: Vec<ContentBlock> = Vec::new();

    // R8: model-identity thinking suppression. Set when an Assistant item
    // with model_id Some(m) and m != model_slug is translated; Reasoning
    // items translated while set emit no Thinking block; cleared on the next
    // Assistant or System flush. A None/empty request model makes the rule a
    // no-op (a None-model request is invalid anyway; the guard must not
    // silently strip signed thinking).
    let model_slug = req.model.as_deref().unwrap_or_default();
    let r8_active = !model_slug.is_empty();
    let mut r8_suppress_thinking = false;

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
                // R3: the unified source parse (same helper as the
                // tool-result image path).
                ContentPart::Image { url } => match image_source_or_fallback(url) {
                    Ok(source) => ContentBlock::Image {
                        source,
                        cache_control: None,
                    },
                    Err(text) => ContentBlock::Text {
                        text,
                        cache_control: None,
                    },
                },
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

    let flush_user = |pending: &mut Vec<ContentBlock>, msgs: &mut Vec<Message>| {
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
                // R1 edge: System flushes the pending user run (no message is
                // produced) and clears the R8 flag.
                flush_user(&mut pending_user, &mut messages);
                r8_suppress_thinking = false;
                system_blocks.push(TextBlock {
                    r#type: "text".to_string(),
                    text: s.content.as_ref().to_owned(),
                    cache_control: None,
                });
            }
            ConversationItem::User(u) => {
                flush_assistant(&mut pending_assistant, &mut messages);
                // R1: accumulate instead of pushing a fresh user message per
                // item (xli append_to_role merge, wire.rs:1071) — consecutive
                // user-role content becomes ONE user message.
                pending_user.extend(content_parts_to_anthropic_blocks(&u.content));
            }
            ConversationItem::Assistant(a) => {
                flush_user(&mut pending_user, &mut messages);
                // R8: every Assistant item sets or clears the suppression
                // flag (mismatched model_id sets it; matched/None clears it).
                r8_suppress_thinking =
                    r8_active && a.model_id.as_deref().is_some_and(|m| m != model_slug);

                if !a.content.is_empty() {
                    pending_assistant.push(ContentBlock::Text {
                        text: a.content.as_ref().to_owned(),
                        cache_control: None,
                    });
                }

                for tc in &a.tool_calls {
                    let input = tool_call_input(&tc.arguments, &tc.name);
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
                            // R3: the unified source parse — a degraded
                            // reference becomes a labeled text block instead
                            // of a misclassified image source.
                            match image_source_or_fallback(url) {
                                Ok(source) => {
                                    blocks.push(ContentBlock::Image {
                                        source,
                                        cache_control: None,
                                    });
                                }
                                Err(text) => {
                                    blocks.push(ContentBlock::Text {
                                        text,
                                        cache_control: None,
                                    });
                                }
                            }
                        }
                    }
                    ToolResultContent::Blocks(blocks)
                };
                // R1: tool results accumulate in the shared user buffer.
                pending_user.push(ContentBlock::ToolResult {
                    tool_use_id: sanitize_tool_call_id(&t.tool_call_id),
                    content,
                    cache_control: None,
                });
            }
            // No native equivalent, so emit synthetic text to retain context.
            ConversationItem::BackendToolCall(b) => {
                flush_user(&mut pending_user, &mut messages);
                pending_assistant.push(ContentBlock::Text {
                    text: b.text_summary(),
                    cache_control: None,
                });
            }
            // `tco_*` blobs carry only `signature`; real reasoning sets `thinking`
            ConversationItem::Reasoning(r) => {
                flush_user(&mut pending_user, &mut messages);
                let thinking = reasoning_item_text(r);
                let signature = r
                    .encrypted_content
                    .as_deref()
                    .map(str::to_owned)
                    .unwrap_or_default();
                // R8: while the suppression flag is set (mismatched model_id
                // on the owning assistant), the sibling reasoning emits no
                // Thinking block — the assistant's text + tool_use stand.
                if !r8_suppress_thinking && (!thinking.is_empty() || !signature.is_empty()) {
                    pending_assistant.push(ContentBlock::Thinking {
                        thinking,
                        signature,
                    });
                }
            }
        }
    }

    flush_assistant(&mut pending_assistant, &mut messages);
    flush_user(&mut pending_user, &mut messages);

    // MW-1 D5 stage 3: adjacency cleanup (emptied user and assistant
    // messages are removed).
    clean_orphaned_blocks_by_adjacency(&mut messages);

    // MW-2 D2 (leading-assistant repair): post-merge, pre-strip,
    // pre-trailing-repair — run AFTER the adjacency cleanup so a leading
    // assistant removed by the strip cannot leave two consecutive user
    // messages, and BEFORE the hoist/strip/repair so the synthetic head is
    // part of the shape those stages see. A leading assistant is never the
    // *latest* in a shape that also trails with assistant, so this stage
    // does not interact with the thinking strip. The sentinel is
    // unconditional `[Continue]` — a head has no tool result awaiting, so
    // S-014's label would misdescribe it.
    if messages
        .first()
        .is_some_and(|m| matches!(m.role, MessageRole::Assistant))
    {
        messages.insert(
            0,
            Message {
                role: MessageRole::User,
                content: MessageContent::Blocks(vec![ContentBlock::Text {
                    text: "[Continue]".to_string(),
                    cache_control: None,
                }]),
            },
        );
    }

    // MW-1 D5 stages 4-6: tool_result hoist, the three-part thinking strip,
    // then the trailing-assistant repair.
    hoist_tool_results_to_front(&mut messages);
    strip_thinking_blocks(&mut messages);
    repair_trailing_assistant(&mut messages);

    // ANTHROPIC-WIRE-1 (cut 3): only "1h" reaches the wire; "5m"/absent and
    // unknown tiers map to the wire default (no ttl field).
    let head_ttl = crate::messages::cache_control_ttl(req.cache_ttl.as_deref());
    apply_cache_breakpoints(&mut system_blocks, &mut messages, head_ttl);

    let system: Option<SystemParam> = if system_blocks.is_empty() {
        None
    } else if system_blocks.len() == 1 && system_blocks[0].cache_control.is_none() {
        Some(SystemParam::Text(system_blocks[0].text.clone()))
    } else {
        Some(SystemParam::Blocks(system_blocks))
    };

    // MW-2 R6 (delta row 6) — namespace flattening: N/A in V1. This crate's
    // `ToolSpec` is flat (name/description/parameters — zero `namespace` hits
    // in the crate), so there is nothing to flatten here. xli's actual
    // behavior, if a namespaced spec ever reaches the MCP seam (cite the
    // CODE, gap 15 — the `ToolSpec::Namespace` arm's comment claims a
    // single-dot `<namespace>.<name>` convention and is self-contradictory;
    // the code wins): wire.rs:967 `format!("{}__{}", ns.name, f.name)` +
    // `flat_mcp_tool_name` (codex-wire-extensions/src/tool_name.rs:68,
    // `FLAT_MCP_TOOL_NAME_DELIMITER = "__"` :54) — double-underscore flat
    // names, round-tripped by the decoder. RE-AUDIT TRIGGER: any MCP seam
    // change introducing a namespaced tool spec re-opens delta row 6.
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

    // Per-model thinking knowledge (D4): the table row wins when the slug
    // has one; until MW-3 fills the table this is exactly the current
    // effort-driven default (effort -> Adaptive summarized; else none).
    // Driven by reasoning_effort only, not by json_schema.
    let model = req.model.as_deref().unwrap_or_default();
    let thinking = crate::messages_model::messages_thinking_config(model, req.reasoning_effort);

    let output_config = if effort.is_some() || format.is_some() {
        Some(OutputConfig { effort, format })
    } else {
        None
    };

    MessagesRequest {
        model: req.model.clone().unwrap_or_default(),
        messages,
        max_tokens: match req.max_output_tokens {
            Some(budget) => budget.max(crate::messages_model::MESSAGES_MAX_OUTPUT_TOKENS_FLOOR),
            // D4 ruling (ledger 2026-09-12): no budget falls back to the
            // R5 pin-sourced table row (9 endpoint-agreement slugs + the
            // cut-5 dotted aliases), else 0 — the live proxy tolerates 0
            // (pre-MW-1 wire parity); a
            // floor-1 fallback truncated every no-budget turn (L2
            // l2_messages_wire, stop_reason=max_tokens).
            // ANTHROPIC-WIRE-1 (cut 5): the no-row case warns — the
            // pipeline client fill (responses_budget_fallback) is what
            // floors those slugs; the standalone builder keeps 0.
            None => match crate::messages_model::messages_max_output_tokens_opt(model) {
                Some(cap) => cap,
                None => {
                    tracing::warn!(
                        model = %model,
                        "no-budget messages request: no R5 table row (or alias) for the \
                         slug; serializing max_tokens: 0 (pre-warm semantics)"
                    );
                    0
                }
            },
        },
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
