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
//! 6. **thinking replay** (MW-1 D5 stage 5,
//!    [`strip_thinking_blocks`]) — `replay_older` (the row key
//!    `thinking_replay`: `None`/default = ON, `"off"` = legacy) selects the
//!    policy: all-older CAP-AWARE verbatim replay (an older assistant's
//!    signed (text, signature) pairs + non-empty-`data` `RedactedThinking`
//!    ride, gated all-or-nothing per message by the N3 per-item cap; a
//!    thinking-only older message never rides) or the LEGACY one-request
//!    strip (non-latest messages lose all thinking blocks; the latest keeps
//!    a block only as a verbatim pair; signature-only blocks dropped;
//!    `RedactedThinking` dropped). Emptied assistant messages are removed in
//!    both. R8's suppression runs BEFORE this strip (the strip is a superset
//!    gate over what R8 leaves behind);
//! 7. **trailing-assistant repair** (MW-1 D5 stage 6,
//!    [`repair_trailing_assistant`]) — a history ending on an assistant
//!    message gets a synthetic user sentinel (`[Awaiting tool result]` /
//!    `[Continue]`), because the proxy-routed endpoints reject assistant
//!    prefill;
//! 8. **cache-control window** (MW-1 D5 stage 7,
//!    [`apply_cache_breakpoints`]) — runs last, so its tip can land on the
//!    synthetic sentinel user.
//!
//! Provenance: MW-1 messages-wire semantics + MW-2 builder strictness
//! (grok/plans/MW-1-spec.md + MW-2-spec.md) — re-expressed, never
//! copied; donors pinned in grok/plans/donors.md:
//! - xli@3d4a08271e (primary) + xli@6d3784158c (audited-ledger), donor
//!   file `codex-rs/provider-anthropic/src/wire.rs`: the MW-1 D5 stage
//!   fns (line numbers as of this backfill) — `clean_orphaned_items`
//!   :79, `clean_orphaned_blocks_by_adjacency` :136,
//!   `strip_thinking_blocks` :266, `hoist_tool_results_to_front` :326,
//!   `repair_trailing_assistant` :374, `apply_cache_breakpoints` :448 —
//!   and MW-2 R1 same-role merge (xli `append_to_role`, wire.rs:1071,
//!   verified at pin; the SHA-less inline refs in this doc resolve to
//!   it).
//! - hyper-grok-build@45e984f3 — MW-2 R8 model-identity thinking
//!   suppression, re-expressing
//!   `packages/ai/xai-grok-sampler/src/pi_messages.rs:1050`
//!   (`identity_mismatch_falls_back_to_portable_text_and_tool_calls`;
//!   endpoint-identity → model-id-identity divergence per MW-2-spec).
//!   HY's MW-2 bedrock/pi wire-evidence tests sit in the sibling
//!   `messages.rs` (8 markers), not this file.
//! Before this header only the companion tests were SHA-marked
//! (DESIGN-AUDIT-1 gap G3).

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

/// §3.3.2 (apex-ayl.108.1) — the N3 per-item estimator for assistant
/// messages, bit-identical to `check_message_tokens` (request_validation.rs)
/// for image-less items — and assistant items are image-less by construction
/// (the builder's assistant blocks are Thinking/Text/ToolUse only): the
/// compact-JSON byte count / BYTES_PER_TOKEN.
fn item_token_estimate(msg: &crate::messages::Message) -> u64 {
    let item_json =
        serde_json::to_vec(msg).expect("Message serializes (N3's own prelude)");
    xai_token_estimation::estimate_tokens(&String::from_utf8_lossy(&item_json))
}

/// D5 stage 5 — the thinking replay rule (xli S-031, spec rule 2; the
/// apex-ayl.108.1 all-older cap-aware replay extension).
///
/// `replay_older = false` (the operator's `thinking_replay = "off"` switch)
/// is the LEGACY one-request strip, byte-identical to the pre-108.1
/// behavior: (a) every assistant message before the latest loses ALL
/// thinking blocks — the API only requires the latest assistant's thinking
/// to replay verbatim, and earlier turns are safe to strip; (b) the latest
/// assistant message keeps a `Thinking` block only if it carries both
/// thinking text and a non-empty signature — the verbatim pair as stored by
/// the stream consumer, which is exactly what Anthropic's "thinking blocks
/// in the latest assistant message cannot be modified" check verifies;
/// (c) a signature-only block (empty thinking text, the opus47 shape) is
/// dropped entirely — there is nothing to replay. `RedactedThinking` blocks
/// are dropped at every position (the pre-108.1 predicate — the true
/// rollback). Assistant messages left empty by the strip are removed.
///
/// `replay_older = true` (default) is the ALL-OLDER cap-aware verbatim
/// replay (D1/D2): an OLDER assistant's safe set rides verbatim — the
/// signed (thinking, signature) pairs (both fields non-empty) and the
/// non-empty-`data` `RedactedThinking` blocks (ADJ-1: the docs mandate the
/// echo; never dropped, even over cap) — gated ALL-OR-NOTHING per message
/// by the N3 per-item cap (`item_token_estimate` ≤
/// `MAX_MODEL_CONTEXT_ITEM_TOKENS`, inclusive); unsigned and signature-only
/// blocks are dropped at every position. The orphan-shape rule: an older
/// message carrying NO non-thinking block (a thinking-only message) never
/// rides. The LATEST arm is predicate-only (NOT cap-gated) and gains the
/// ADJ-1 `RedactedThinking` flip: a non-empty-`data` redacted block rides
/// verbatim; the empty-`data` V1 dead shape is dropped. R8's suppression
/// runs BEFORE this strip, so a suppressed block never reaches the replay
/// decision.
pub(crate) fn strip_thinking_blocks(
    messages: &mut Vec<crate::messages::Message>,
    replay_older: bool,
) {
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
        // The N3 cap estimate needs &msg, so it is computed BEFORE the
        // mutable content borrow below (borrow-checker). Only the all-older
        // replay arm consumes it; every other arm leaves it false (unused).
        let within_cap = if i < last_idx && replay_older {
            item_token_estimate(msg)
                <= crate::request_validation::MAX_MODEL_CONTEXT_ITEM_TOKENS
        } else {
            false
        };
        let MessageContent::Blocks(blocks) = &mut msg.content else {
            continue;
        };
        if i < last_idx {
            if !replay_older {
                // Legacy strip: non-latest assistant loses ALL thinking
                // blocks (byte-identical to the pre-108.1 behavior).
                blocks.retain(|b| {
                    !matches!(
                        b,
                        ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. }
                    )
                });
            } else {
                // Cap-aware all-older verbatim replay (§3.3.1): the safe
                // set (signed pairs + non-empty redacted) rides only when
                // the message has a verbatim pair AND at least one
                // non-thinking block (redacted counts) AND the pre-decision
                // item estimate is within the N3 cap (inclusive).
                let verbatim_pairs = blocks.iter().any(|b| {
                    matches!(
                        b,
                        ContentBlock::Thinking { thinking, signature }
                            if !thinking.is_empty() && !signature.is_empty()
                    )
                });
                let has_non_thinking = blocks
                    .iter()
                    .any(|b| !matches!(b, ContentBlock::Thinking { .. }));
                let replay = verbatim_pairs && has_non_thinking && within_cap;
                if replay {
                    blocks.retain(|b| match b {
                        ContentBlock::Thinking {
                            thinking,
                            signature,
                        } => !thinking.is_empty() && !signature.is_empty(),
                        ContentBlock::RedactedThinking { data } => !data.is_empty(),
                        _ => true,
                    });
                } else {
                    // Cap-gate drop (ALL pairs of this message), the orphan
                    // shape, or nothing to replay: unsigned/signature-only
                    // thinking is dropped; redacted blocks ride (no cap
                    // branch for redacted — ADJ-1).
                    blocks.retain(|b| !matches!(b, ContentBlock::Thinking { .. }));
                }
            }
        } else {
            // Latest assistant: predicate-only (NOT cap-gated). Only the
            // verbatim (text, signature) pair survives; unsigned and
            // signature-only blocks are dropped. The ADJ-1 flip: a
            // non-empty-`data` redacted block rides verbatim (the docs
            // mandate the echo — the V1 arm is dead at ingestion, so this
            // holds structurally at zero current cost); the empty-`data`
            // V1 dead shape is dropped.
            blocks.retain(|b| match b {
                ContentBlock::Thinking {
                    thinking,
                    signature,
                } => !thinking.is_empty() && !signature.is_empty(),
                ContentBlock::RedactedThinking { data } => !data.is_empty(),
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
        ContentBlock, Message, MessageContent, MessageRole, MessagesRequest,
        MessagesRequestParts, OutputConfig, SystemParam, TextBlock, ToolChoiceParam, ToolParam,
        ToolResultContent,
    };
    use crate::presence::RequestPresence;

    // D5 stage 1: item-level orphan cleanup, before translation.
    let items = clean_orphaned_items(&req.items);

    let mut system_blocks: Vec<TextBlock> = Vec::new();
    let mut messages: Vec<Message> = Vec::new();
    let mut pending_assistant: Vec<ContentBlock> = Vec::new();
    // R1: ONE user-role buffer — consecutive user text and tool_result
    // content merges into a single user message, flushed on an assistant
    // role change (or a System boundary).
    // C3c carve-out (apex-ayl.89): a run that contains a CompactionMeta
    // item flushes on a synthetic-class change (see the User arm), so the
    // compaction metadata pieces project as separate sub-cap user
    // messages; CM-free runs keep the plain R1 coalescing byte-for-byte.
    let mut pending_user: Vec<ContentBlock> = Vec::new();
    let mut pending_user_cm = false;
    let mut pending_user_class: Option<SyntheticReason> = None;

    // R8: model-identity thinking suppression (apex-ayl.108.1) — a Reasoning
    // item's Thinking block is suppressed iff its OWNING assistant's model_id
    // is Some(m) with m != the request slug. The owner is the assistant the
    // reasoning joins: the assistant already in the current pending buffer
    // (a Reasoning that FOLLOWS its assistant) when present, else the nearest
    // FORWARD assistant (a Reasoning that OPENS a fresh assistant turn).
    // owner-None / no owner ⇒ NO suppression (NOT fail-closed). A
    // None/empty request model makes the rule a no-op (a None-model request
    // is invalid anyway; the guard must not silently strip signed thinking).
    let model_slug = req.model.as_deref().unwrap_or_default();
    let r8_active = !model_slug.is_empty();
    // Forward-owner model (look-ahead): per item index, the nearest FORWARD
    // Assistant's model_id — None = no forward assistant, or a forward
    // assistant with model_id None. Consumed only when the buffer holds no
    // assistant yet (a Reasoning that opens a fresh turn).
    let forward_model: Vec<Option<&str>> = {
        let mut lookup = vec![None; items.len()];
        let mut next: Option<&str> = None;
        for i in (0..items.len()).rev() {
            lookup[i] = next;
            if let ConversationItem::Assistant(a) = &items[i] {
                next = a.model_id.as_deref();
            }
        }
        lookup
    };
    // Buffer-owner: the model_id of the most recent Assistant added to the
    // current pending_assistant buffer since the last flush (System/User/
    // ToolResult). None = the buffer holds no assistant yet.
    let mut buffer_assistant_model: Option<Option<&str>> = None;

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

    for (item_idx, item) in items.iter().enumerate() {
        match item {
            ConversationItem::System(s) => {
                flush_assistant(&mut pending_assistant, &mut messages);
                buffer_assistant_model = None;
                // R1 edge: System flushes the pending user run (no message
                // is produced).
                flush_user(&mut pending_user, &mut messages);
                pending_user_cm = false;
                pending_user_class = None;
                system_blocks.push(TextBlock {
                    r#type: "text".to_string(),
                    text: s.content.as_ref().to_owned(),
                    cache_control: None,
                });
            }
            ConversationItem::User(u) => {
                flush_assistant(&mut pending_assistant, &mut messages);
                buffer_assistant_model = None;
                // R1: accumulate instead of pushing a fresh user message per
                // item (xli append_to_role merge, wire.rs:1071) — consecutive
                // user-role content becomes ONE user message.
                // C3c (apex-ayl.89): within a run that contains a
                // CompactionMeta item, a synthetic-class change flushes the
                // run first, so each compaction metadata piece and the real
                // query stay their own sub-cap messages. `cm_present` is
                // assigned after the flush on purpose: a same-class run
                // never re-splits, so adjacent real users (and adjacent
                // same-tag items) keep coalescing exactly as pre-cut.
                let class = u.synthetic_reason.clone();
                let cm_present =
                    pending_user_cm || matches!(class, Some(SyntheticReason::CompactionMeta));
                if cm_present && !pending_user.is_empty() && pending_user_class != class {
                    flush_user(&mut pending_user, &mut messages);
                }
                pending_user_cm = cm_present;
                pending_user_class = class;
                pending_user.extend(content_parts_to_anthropic_blocks(&u.content));
            }
            ConversationItem::Assistant(a) => {
                flush_user(&mut pending_user, &mut messages);
                pending_user_cm = false;
                pending_user_class = None;
                buffer_assistant_model = Some(a.model_id.as_deref());
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
                buffer_assistant_model = None;
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
                    // Provenance: fresh — spec L3898-3902 (GAP-B4): failed
                    // executions project as `"is_error": true`.
                    is_error: t.is_error,
                    cache_control: None,
                });
            }
            // No native equivalent, so emit synthetic text to retain context.
            ConversationItem::BackendToolCall(b) => {
                flush_user(&mut pending_user, &mut messages);
                pending_user_cm = false;
                pending_user_class = None;
                pending_assistant.push(ContentBlock::Text {
                    text: b.text_summary(),
                    cache_control: None,
                });
            }
            // `tco_*` blobs carry only `signature`; real reasoning sets `thinking`
            ConversationItem::Reasoning(r) => {
                flush_user(&mut pending_user, &mut messages);
                pending_user_cm = false;
                pending_user_class = None;
                let thinking = reasoning_item_text(r);
                let signature = r
                    .encrypted_content
                    .as_deref()
                    .map(str::to_owned)
                    .unwrap_or_default();
                // R8: the reasoning's owner is the assistant in the current
                // buffer (it FOLLOWS that assistant) when one is present,
                // else the nearest FORWARD assistant (it OPENS a fresh turn).
                // Suppress iff the owner's model_id is Some(m) with m != the
                // request slug; owner-None / no owner ⇒ NO suppression (NOT
                // fail-closed). The assistant's text + tool_use always stand.
                let owner_model: Option<&str> = match buffer_assistant_model {
                    Some(m) => m,
                    None => forward_model[item_idx],
                };
                let r8_suppressed = r8_active
                    && matches!(owner_model, Some(m) if m != model_slug);
                if !r8_suppressed && (!thinking.is_empty() || !signature.is_empty()) {
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

    // MW-1 D5 stages 4-6: tool_result hoist, the thinking replay / legacy
    // strip (`replay_older` = `thinking_replay != Some("off")` — the row
    // key `thinking_replay`: `None`/default = replay ON), then the
    // trailing-assistant repair.
    hoist_tool_results_to_front(&mut messages);
    strip_thinking_blocks(&mut messages, req.thinking_replay.as_deref() != Some("off"));
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
    // MGW F1 (apex-ayl.115): client tools first in original order (the
    // untagged Custom variant is the pre-cut flat struct — byte-identical
    // wire shape), then config-selected server-tool members in config
    // order (binding emission order, cache-stability). A server-tools-only
    // row (no client tools) still projects a non-empty tools array.
    let tools: Option<Vec<ToolParam>> = {
        let mut mapped: Vec<ToolParam> = req
            .tools
            .iter()
            .map(|t| ToolParam::Custom(crate::messages::ToolCustom {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: t.parameters.clone(),
                cache_control: None,
            }))
            .collect();
        // F5 per-tool breakpoint: the marker spends the free 4th marker
        // slot on the LAST CLIENT tool. Union-forced re-wrap (MSGW F1):
        // server members are appended AFTER this block, so "last" keeps
        // its pre-cut semantics — the final client tool, never a server
        // member (which carries no cc by config).
        if req.tool_cache_breakpoint == Some(ToolCacheBreakpoint::Last) && !mapped.is_empty() {
            let last = mapped.len() - 1;
            // The scrutinee is `&mut mapped[last]`: the field binds by
            // implicit `&mut` (an explicit `ref mut` is illegal here).
            if let ToolParam::Custom(crate::messages::ToolCustom { cache_control, .. }) =
                &mut mapped[last]
            {
                *cache_control = Some(crate::messages::CacheControl::ephemeral());
            }
        }
        // Canonical dated type strings (the config layer resolved family
        // names + soft-refused unknown slugs).
        if let Some(ref members) = req.server_tools {
            for t in members {
                match crate::messages::server_tool_from_type(t) {
                    Some(mut member) => {
                        // mcp_toolset: the producer fills the required
                        // mcp_server_name from the row's pairing key. The
                        // config layer HARD-refuses pairing violations,
                        // so the None arm is a defensive backstop, never
                        // a panic (SDD §3.5, FIX-PASS R4/R5).
                        if let crate::messages::ToolServer::McpToolset {
                            ref mut mcp_server_name,
                            ..
                        } = member
                        {
                            match req.mcp_toolset_server.clone() {
                                Some(name) => *mcp_server_name = name,
                                None => {
                                    tracing::warn!(
                                        "mcp_toolset without mcp_toolset_server \
                                         (config-layer hard-refusal backstop); \
                                         skipping member"
                                    );
                                    continue;
                                }
                            }
                        }
                        mapped.push(ToolParam::Server(member));
                    }
                    None => tracing::warn!(
                        type = %t,
                        "unknown server-tool type string (config-layer \
                         validated); skipping"
                    ),
                }
            }
        }
        if mapped.is_empty() {
            None
        } else {
            Some(mapped)
        }
    };

    let dptu = req.disable_parallel_tool_use;
    let tool_choice: Option<ToolChoiceParam> = match req.tool_choice.as_ref() {
        Some(tc) => Some(match tc {
            ConversationToolChoice::Auto => {
                ToolChoiceParam::Auto {
                    disable_parallel_tool_use: dptu,
                }
            }
            ConversationToolChoice::Required => {
                ToolChoiceParam::Any {
                    disable_parallel_tool_use: dptu,
                }
            }
            ConversationToolChoice::Function(name) => ToolChoiceParam::Tool {
                name: name.clone(),
                disable_parallel_tool_use: dptu,
            },
            // GA {"type":"none"} (docs L1372–1376): the fallback to Auto dies here.
            ConversationToolChoice::None => ToolChoiceParam::None,
        }),
        // Operator declared the toggle with no explicit choice: emit explicit auto (behavior-preserving
        // — auto is the default) carrying the toggle.
        None => dptu.map(|v| ToolChoiceParam::Auto { disable_parallel_tool_use: Some(v) }),
    };

    // WIRE-NEUTRAL-2 (apex-ayl.86): ultra is resolved at the REQUEST level,
    // not the enum level — `to_messages_api` still maps Ultra to the raw
    // "ultra" (untouched, pinned by M4); this arm intercepts before
    // emission and carries the shell-resolved menu tier, falling back to
    // wire "max" when it is None (no menu / legacy row). The raw "ultra"
    // never reaches the messages wire (I9). Sub-ultra efforts ride the
    // pre-cut `to_messages_api` path, zero-diff (M2).
    let effort = match req.reasoning_effort {
        Some(crate::ReasoningEffort::Ultra) => Some(
            req
                .ultra_wire_effort
                .unwrap_or(crate::ReasoningEffort::Max)
                .as_str()
                .to_string(),
        ),
        other => other
            .and_then(|e| e.to_messages_api())
            .map(|s| s.to_string()),
    };

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
        Some(OutputConfig {
            effort: effort.map(RequestPresence::value).unwrap_or_default(),
            format: format.map(RequestPresence::value).unwrap_or_default(),
        })
    } else {
        None
    };

    // REQVALID-1 47b (D-4): the trusted pipeline producer builds through
    // the in-crate from_parts seam (the struct fields are private to the
    // messages module; the empty-model output stays legal here — the
    // client funnel's fill_model seam is the pre-fill contract).
    MessagesRequest::from_parts(MessagesRequestParts {
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
        top_k: req.top_k,
        stream: None, // The caller sets this
        stop_sequences: req.stop_sequences.clone(),
        thinking,
        output_config,
        metadata: req
            .user_id
            .clone()
            .map(|uid| crate::messages::Metadata { user_id: RequestPresence::value(uid) }),
        // MGW F1 (apex-ayl.115): the config-declared servers ride the
        // wire in BETA param form (r#type "url").
        mcp_servers: req.mcp_servers.clone().map(|decls| {
            decls
                .into_iter()
                .map(|d| crate::messages::McpServerParam {
                    r#type: "url".to_string(),
                    name: d.name,
                    url: d.url,
                    authorization_token: d.authorization_token,
                    tool_configuration: d.tool_configuration,
                })
                .collect()
        }),
    })
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

#[cfg(test)]
mod tests {
    use super::build_messages_request;
    use crate::conversation::ConversationRequest;

    /// M1 (SDD §4, WIRE-NEUTRAL-2): a messages-wire request carrying ultra
    /// with the shell-resolved menu tier emits the RESOLVED tier in
    /// `output_config.effort` — and the raw string "ultra" appears NOWHERE
    /// in the encoded body (I9).
    #[test]
    fn messages_ultra_resolves_to_menu_tier() {
        let mut req = ConversationRequest::default();
        req.model = Some("claude-opus-5-test".to_owned());
        req.reasoning_effort = Some(crate::ReasoningEffort::Ultra);
        req.ultra_wire_effort = Some(crate::ReasoningEffort::Xhigh);
        let built = build_messages_request(&req);
        let encoded = serde_json::to_string(&built).expect("messages request encodes");
        assert!(
            encoded.contains("\"effort\":\"xhigh\""),
            "ultra must resolve to the menu-derived tier: {encoded}"
        );
        assert!(
            !encoded.contains("ultra"),
            "raw ultra must never reach the messages wire (I9): {encoded}"
        );
    }

    /// M2 (SDD §4, WIRE-NEUTRAL-2): sub-ultra messages bytes are UNCHANGED
    /// — the field is inert when the effort is not ultra, and
    /// None/Minimal leave `output_config` absent, as today.
    #[test]
    fn messages_subultra_unchanged() {
        let mut req = ConversationRequest::default();
        req.model = Some("claude-opus-5-test".to_owned());
        req.reasoning_effort = Some(crate::ReasoningEffort::Xhigh);
        req.ultra_wire_effort = Some(crate::ReasoningEffort::Xhigh);
        let built = build_messages_request(&req);
        let encoded = serde_json::to_string(&built).expect("messages request encodes");
        assert!(
            encoded.contains("\"effort\":\"xhigh\""),
            "sub-ultra goes through the pre-cut to_messages_api path: {encoded}"
        );
        for effort in [
            Some(crate::ReasoningEffort::None),
            Some(crate::ReasoningEffort::Minimal),
            None,
        ] {
            let mut req = ConversationRequest::default();
            req.model = Some("claude-opus-5-test".to_owned());
            req.reasoning_effort = effort;
            req.ultra_wire_effort = Some(crate::ReasoningEffort::Xhigh);
            let built = build_messages_request(&req);
            assert!(
                built.output_config().is_none(),
                "{effort:?}: output_config stays absent, as today"
            );
        }
    }

    /// M3 (SDD §4, WIRE-NEUTRAL-2): a fieldless ultra (menu-less /
    /// ultra-only edge — the S4/S5 analog) falls back to wire "max" —
    /// no raw "ultra" (I9; the R-MENU-DERIVED no-menu fallback).
    #[test]
    fn messages_ultra_fieldless_falls_back_to_max() {
        let mut req = ConversationRequest::default();
        req.model = Some("claude-opus-5-test".to_owned());
        req.reasoning_effort = Some(crate::ReasoningEffort::Ultra);
        // `ultra_wire_effort` stays `None` (default).
        let built = build_messages_request(&req);
        let encoded = serde_json::to_string(&built).expect("messages request encodes");
        assert!(
            encoded.contains("\"effort\":\"max\""),
            "fieldless ultra falls back to wire-legal max: {encoded}"
        );
        assert!(
            !encoded.contains("ultra"),
            "no raw ultra on the messages wire (I9): {encoded}"
        );
    }

    /// M4 (SDD §4, WIRE-NEUTRAL-2): the resolution happens at the REQUEST
    /// level, not the enum level — `to_messages_api` still maps Ultra to
    /// the raw "ultra" at the enum level (untouched), and the
    /// `build_messages_request` ultra arm intercepts before emission
    /// (pins the §3.4b field-approach decision).
    #[test]
    fn resolution_happens_at_request_level_not_enum_level() {
        assert_eq!(
            crate::ReasoningEffort::Ultra.to_messages_api(),
            Some("ultra"),
            "the enum-level mapping is untouched by this cut"
        );
        let mut req = ConversationRequest::default();
        req.model = Some("claude-opus-5-test".to_owned());
        req.reasoning_effort = Some(crate::ReasoningEffort::Ultra);
        req.ultra_wire_effort = Some(crate::ReasoningEffort::Xhigh);
        let built = build_messages_request(&req);
        let encoded = serde_json::to_string(&built).expect("messages request encodes");
        assert!(
            !encoded.contains("ultra"),
            "the request-level consumption intercepts the raw value: {encoded}"
        );
        assert!(
            encoded.contains("\"effort\":\"xhigh\""),
            "the resolved tier reaches the wire: {encoded}"
        );
    }
}
