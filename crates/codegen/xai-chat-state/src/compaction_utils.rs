//! Pure utility functions and types for compaction support.
//!
//! These are stateless functions that operate on conversation data only —
//! no I/O, no actor state. They live in `xai-chat-state` so that both
//! this crate and `xai-grok-shell` can share them without duplication.
//!
//! Provenance: P2.1 codex remote compaction v2 (ledger §P2.1) — the
//! ported region in this stock-upstream file is the contiguous
//! `codex_remote_compaction_v2_*` family added by commit 20d782d (const
//! `CODEX_REMOTE_COMPACTION_V2_RETAINED_USER_TOKENS` through
//! `codex_remote_compaction_v2_interjections`, marked below): verbatim
//! from open-grok@240c99c9
//! `crates/codegen/xai-chat-state/src/compaction_utils.rs:557-732`
//! (single surface diff: `SyntheticReason` import qualification;
//! byte-checked 2026-09-15; part of the item's 27/27 verbatim
//! spot-checks — Sagan review, ledger §P2.1). The donor file's
//! image / `CustomToolOutput` arms are NOT ported (the worktree
//! `ConversationItem` has no such fields). The item's remaining P2.1
//! markers sit on the shell collector
//! (`xai-grok-shell/src/session/compaction.rs`) and the test files; this
//! header is this file's surface.
use std::collections::BTreeSet;
use xai_grok_sampling_types::{ContentPart, ConversationItem, SyntheticReason, ToolResultItem};
pub const AGENT_MESSAGE_MODEL_LABEL: &str =
    "[Message authored by another agent; not a human request or approval.]";
/// Canonical history prepared exactly once for a model-facing request.
/// The private inner value distinguishes prepared history without inspecting payload text.
pub struct ModelRequestHistory(Vec<ConversationItem>);
impl ModelRequestHistory {
    pub fn from_raw(conversation: Vec<ConversationItem>) -> Self {
        Self(
            conversation
                .into_iter()
                .map(|item| match item {
                    ConversationItem::User(mut user)
                        if user.synthetic_reason == Some(SyntheticReason::AgentMessage) =>
                    {
                        user.content.insert(
                            0,
                            ContentPart::Text {
                                text: std::sync::Arc::<str>::from(AGENT_MESSAGE_MODEL_LABEL),
                            },
                        );
                        ConversationItem::User(user)
                    }
                    // Pass-through: only the agent-message label is applied here. A
                    // discovery pair rides unlabelled — its `raw` is provider bytes and
                    // rewriting them is forbidden (wire invariant 6).
                    ConversationItem::User(_)
                    | ConversationItem::System(_)
                    | ConversationItem::Assistant(_)
                    | ConversationItem::ToolResult(_)
                    | ConversationItem::BackendToolCall(_)
                    | ConversationItem::Reasoning(_)
                    | ConversationItem::Discovery { .. } => item,
                })
                .collect(),
        )
    }
    pub fn into_items(self) -> Vec<ConversationItem> {
        self.0
    }
}
/// Drops tool results and backend tool calls, flattening assistant `tool_calls` into text.
/// Mutates assistant text in place — do not send this to a provider that validates signed `reasoning`.
/// Use [`prepare_conversation_for_summarization`], which also strips `reasoning`.
pub(crate) fn strip_tool_messages_for_conversation_item(
    conversation: Vec<ConversationItem>,
) -> Vec<ConversationItem> {
    conversation
        .into_iter()
        .filter_map(|item| match item {
            ConversationItem::ToolResult(_) => None,
            ConversationItem::BackendToolCall(_) => None,
            ConversationItem::Assistant(mut a) => {
                if !a.tool_calls.is_empty() {
                    let tool_names: Vec<String> =
                        a.tool_calls.iter().map(|tc| tc.name.clone()).collect();
                    let tool_info = format!("\n[Called tools: {}]", tool_names.join(", "));
                    a.content = if a.content.is_empty() {
                        std::sync::Arc::<str>::from(tool_info)
                    } else {
                        let mut s = String::with_capacity(a.content.len() + tool_info.len());
                        s.push_str(&a.content);
                        s.push_str(&tool_info);
                        std::sync::Arc::<str>::from(s)
                    };
                    a.tool_calls.clear();
                }
                Some(ConversationItem::Assistant(a))
            }
            // A discovery pair is NOT passed to the summariser verbatim: the whole
            // `tools[]` payload would ride into a summarisation prompt, which breaks
            // the bounded-model-visible-fragment rule (§6.7) and pays cache-busting
            // cost for no summarisation signal. It is substituted by its bounded
            // summary, so the summariser still sees that tools were loaded and with
            // which query — and the STORED pair is untouched (this function only ever
            // builds a throwaway summariser view; the IR keeps the bytes).
            ConversationItem::Discovery { item } => {
                Some(ConversationItem::assistant(item.text_summary()))
            }
            ConversationItem::System(_)
            | ConversationItem::User(_)
            | ConversationItem::Reasoning(_) => Some(item),
        })
        .collect()
}
/// Drops every `ConversationItem::Reasoning(_)` sibling.
/// Required before backends that reject structured reasoning, and before summarization.
pub fn strip_reasoning_blocks(conversation: Vec<ConversationItem>) -> Vec<ConversationItem> {
    conversation
        .into_iter()
        .filter(|item| !matches!(item, ConversationItem::Reasoning(_)))
        .collect()
}
/// Replace `ContentPart::Image` entries with `"[image]"` so downstream
/// consumers (summary model, segment store) don't carry megabytes of base64.
pub(crate) fn strip_images(conversation: Vec<ConversationItem>) -> Vec<ConversationItem> {
    conversation
        .into_iter()
        .map(|item| match item {
            ConversationItem::User(mut u) => {
                for part in &mut u.content {
                    if matches!(part, ContentPart::Image { .. }) {
                        *part = ContentPart::Text {
                            text: std::sync::Arc::<str>::from("[image]"),
                        };
                    }
                }
                ConversationItem::User(u)
            }
            // Only user parts carry `ContentPart::Image` in this conversation model.
            // A discovery item is named because it must never be rewritten here even if
            // a provider ever nested a part inside it: `raw` is an opaque handle domain
            // (wire invariant 6) and byte-editing it breaks the replay it exists for.
            // (`User` is fully consumed by the arm above, so it is not repeated here.)
            ConversationItem::Discovery { .. }
            | ConversationItem::System(_)
            | ConversationItem::Assistant(_)
            | ConversationItem::ToolResult(_)
            | ConversationItem::BackendToolCall(_)
            | ConversationItem::Reasoning(_) => item,
        })
        .collect()
}
/// Prepare a conversation for a summarization call (compaction or memory flush).
/// Strips tool messages, reasoning, and images. Reasoning must go because text mutation
/// invalidates signed `thinking` blocks, which strict providers reject with a 400.
pub fn prepare_conversation_for_summarization(
    conversation: Vec<ConversationItem>,
) -> Vec<ConversationItem> {
    strip_images(strip_reasoning_blocks(
        strip_tool_messages_for_conversation_item(conversation),
    ))
}
/// Segment-store prep (`segments` mode): keep tool I/O verbatim, strip only images + reasoning.
pub fn prepare_conversation_for_segment(
    conversation: Vec<ConversationItem>,
) -> Vec<ConversationItem> {
    strip_images(strip_reasoning_blocks(conversation))
}
/// Drop a trailing assistant turn whose `tool_calls` lack a `ToolResult` (else strict backends reject the dangling `tool_use`).
///
/// This is SUMMARISER / RECAP INPUT PREP, not a pre-send guard (cut review WAJ21R2-04):
/// its only caller is [`prepare_conversation_for_verbatim_summarization`], reached from
/// the recap and compaction paths — never from building the next model request. A lone
/// `tool_search_call` on the wire therefore has NO trim in front of it; the only thing
/// that observes that shape pre-send is the outbound lint's H-8 call-side arm
/// (`xai-grok-sampling-types`), which never mutates the request.
pub fn truncate_trailing_incomplete_tool_call(
    mut conversation: Vec<ConversationItem>,
) -> Vec<ConversationItem> {
    while matches!(
        conversation.last(),
        Some(ConversationItem::Assistant(a)) if !a.tool_calls.is_empty()
    ) {
        conversation.pop();
    }
    // A trailing unanswered `tool_search_call` is the same incomplete shape for a
    // strict backend (PLAN:946 keeps a call and its output together; PLAN:948's
    // synthesis of a missing output is T15's, not this guard's), so it goes too.
    while trailing_discovery_is_unpaired(&conversation) {
        conversation.pop();
    }
    conversation
}

/// Whether the history's LAST item is a discovery half that cannot be sent as it
/// stands: a `tool_search_call` at the very tail is unanswered, and a
/// `tool_search_output` whose call does not immediately precede it (same `call_id`,
/// or both keyless as the provider mints them) cannot be certified as paired.
///
/// An answered `[call, output]` tail is kept: the pair is the provider's own record
/// of the loaded tool set and must never be stripped (A-26). The pop is pair-atomic
/// because the caller loops and re-tests the new tail, so the call behind a popped
/// output goes with it rather than being left dangling.
///
/// The decision itself is owned by
/// `tool_search::trailing_discovery_is_unpaired`, so the recap trim in
/// `xai-grok-shell` answers the same question with the same rules; this is the
/// crate-local name the guard below (and its mutation witness) reads.
fn trailing_discovery_is_unpaired(items: &[ConversationItem]) -> bool {
    xai_grok_sampling_types::conversation::tool_search::trailing_discovery_is_unpaired(items)
}
/// Cache-aligned summarizer prep: keep tool I/O + images so the prefix matches the engine cache; set `strip_reasoning` when the provider rejects mutated thinking blocks.
pub fn prepare_conversation_for_verbatim_summarization(
    conversation: Vec<ConversationItem>,
    strip_reasoning: bool,
) -> Vec<ConversationItem> {
    let conversation = if strip_reasoning {
        strip_reasoning_blocks(conversation)
    } else {
        conversation
    };
    truncate_trailing_incomplete_tool_call(conversation)
}
/// Per-item token estimate via the trigger-side estimator, so `fit`'s budget matches what fired the compaction (counts images + encrypted reasoning).
fn estimate_item_tokens(item: &ConversationItem) -> u64 {
    crate::actor::state::estimate_item_tokens(item)
}
/// Shrink a verbatim conversation to `max_tokens`: drop oldest whole turns (System kept, tool runs unsplit; the last turn is truncated in place rather than dropped).
pub fn fit_conversation_to_budget(
    conversation: Vec<ConversationItem>,
    max_tokens: u64,
) -> Vec<ConversationItem> {
    let total: u64 = conversation.iter().map(estimate_item_tokens).sum();
    if total <= max_tokens {
        return conversation;
    }
    let mut head: Vec<ConversationItem> = Vec::new();
    let mut body: Vec<ConversationItem> = conversation;
    if matches!(body.first(), Some(ConversationItem::System(_))) {
        head.push(body.remove(0));
    }
    let budget = max_tokens.saturating_sub(head.iter().map(estimate_item_tokens).sum::<u64>());
    let mut remaining = budget;
    let mut start = body.len();
    for i in (0..body.len()).rev() {
        let cost = estimate_item_tokens(&body[i]);
        if cost > remaining {
            break;
        }
        remaining -= cost;
        start = i;
    }
    while start < body.len() && matches!(body[start], ConversationItem::ToolResult(_)) {
        start += 1;
    }
    // Pair-atomic window start (apex-waj.21): the budget cut must never begin between
    // a `tool_search_call` and the `tool_search_output` that answers it, or the
    // retained history holds an answer whose call was budgeted away. Snapping DOWN
    // keeps the whole pair — the ToolResult skip above goes UP instead, because a
    // result with no surviving call cannot be sent at all.
    if start < body.len() {
        start = xai_grok_sampling_types::conversation::tool_search::snap_index_over_discovery_pairs(
            &body, start,
        );
    }
    if start < body.len() {
        head.extend(body.into_iter().skip(start));
    } else {
        head.extend(recover_truncated_tail_unit(body, budget));
    }
    head
}
/// Keep the most-recent turn but truncate its content to `budget` (with its owning `tool_use`) instead of dropping it.
fn recover_truncated_tail_unit(
    mut body: Vec<ConversationItem>,
    budget: u64,
) -> Vec<ConversationItem> {
    // The unit this function can rebuild is `assistant + its tool results` only, so a
    // discovery pair left at the tail would be half-consumed: the loop below pops
    // `ToolResult`s only, and the fallback would then hand back a bare
    // `tool_search_output` whose call stayed behind in the dropped part of `body`.
    // Drop the trailing discovery items FIRST, so this last-resort path can only ever
    // lose a pair whole or not at all (apex-waj.21).
    while matches!(body.last(), Some(ConversationItem::Discovery { .. })) {
        body.pop();
    }
    let mut results: Vec<ConversationItem> = Vec::new();
    while matches!(body.last(), Some(ConversationItem::ToolResult(_))) {
        results.push(body.pop().expect("last() was Some"));
    }
    results.reverse();
    if results.is_empty() {
        return match body.pop() {
            Some(item) => vec![truncate_item_to_tokens(item, budget)],
            None => Vec::new(),
        };
    }
    let owner = if matches!(
        body.last(),
        Some(ConversationItem::Assistant(a)) if !a.tool_calls.is_empty()
    ) {
        body.pop()
    } else {
        None
    };
    let owner_cost = owner.as_ref().map(estimate_item_tokens).unwrap_or(0);
    let result_budget = budget.saturating_sub(owner_cost);
    let per = (result_budget / results.len() as u64).max(1);
    let mut unit: Vec<ConversationItem> = Vec::new();
    if let Some(o) = owner {
        unit.push(o);
    }
    unit.extend(results.into_iter().map(|r| truncate_item_to_tokens(r, per)));
    unit
}
/// Truncate one item's content text to at most `max_tokens`, appending a `[... truncated N bytes ...]` marker (structural fields kept).
fn truncate_item_to_tokens(item: ConversationItem, max_tokens: u64) -> ConversationItem {
    let max_bytes = (max_tokens as usize).saturating_mul(4);
    match item {
        ConversationItem::ToolResult(mut t) => {
            if let Some(s) = truncate_text_to_bytes(&t.content, max_bytes) {
                t.content = s;
            }
            ConversationItem::ToolResult(t)
        }
        ConversationItem::Assistant(mut a) => {
            if let Some(s) = truncate_text_to_bytes(&a.content, max_bytes) {
                a.content = s;
            }
            ConversationItem::Assistant(a)
        }
        ConversationItem::User(mut u) => {
            for part in &mut u.content {
                if let ContentPart::Text { text } = part
                    && let Some(s) = truncate_text_to_bytes(text, max_bytes)
                {
                    *text = s;
                }
            }
            ConversationItem::User(u)
        }
        // Unshrinkable, deliberately: a discovery pair (like a backend call or a
        // reasoning item) is opaque provider state, so the lossy budget ladder silently
        // fails past it and whole turns get dropped instead. Any future shrink must be
        // a TYPED stub that drops `tools[]` while keeping both ids and the `call_id` —
        // never a byte truncation of `raw`, which would corrupt a handle (wire
        // invariant 6) and break the byte-exact replay the variant exists for (A-26).
        ConversationItem::Discovery { .. }
        | ConversationItem::BackendToolCall(_)
        | ConversationItem::Reasoning(_)
        | ConversationItem::System(_) => item,
    }
}
/// Char-boundary-safe prefix of `s` (incl. truncation marker) within `max_bytes`; `None` if `s` already fits.
fn truncate_text_to_bytes(s: &str, max_bytes: usize) -> Option<std::sync::Arc<str>> {
    if s.len() <= max_bytes {
        return None;
    }
    const MARKER_RESERVE: usize = 64;
    let keep = max_bytes.saturating_sub(MARKER_RESERVE);
    let mut end = keep.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let dropped = s.len() - end;
    Some(std::sync::Arc::<str>::from(format!(
        "{}\n[... truncated {dropped} bytes to fit the compaction window ...]",
        &s[..end]
    )))
}
/// Tags injected by the runtime that should be stripped from user queries.
const SYSTEM_TAGS: &[&str] = &[
    "user_info",
    "project_layout",
    "git_status",
    "fork-context",
    "system-reminder",
    "agent-memory",
    "system_reminder",
    "background_context",
    "command-name",
    "command-message",
    "command-args",
    "rules",
];
/// Strip all known system/metadata tag blocks from `text`.
/// Unclosed tags are left untouched.
fn strip_system_tags(text: &str) -> String {
    let mut result = text.to_string();
    for tag in SYSTEM_TAGS {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        while let Some(start) = result.find(&open) {
            if let Some(rel_end) = result[start..].find(&close) {
                let end_pos = start + rel_end + close.len();
                result.replace_range(start..end_pos, "");
            } else {
                break;
            }
        }
    }
    result.trim().to_string()
}
/// Extracts the user query from a message that may contain metadata tags.
/// Prefers `<user_query>` content; otherwise strips known metadata tags and returns the rest.
pub fn extract_user_query(text: &str) -> String {
    let stripped = strip_system_tags(text);
    if let Some(start) = stripped.find("<user_query>") {
        let content_start = start + "<user_query>".len();
        if let Some(end) = stripped[content_start..].find("</user_query>") {
            return stripped[content_start..content_start + end]
                .trim()
                .to_string();
        }
    }
    stripped
}
/// Extract the last actual user query text (stripping metadata tags).
/// Walks backward to the last `User` item and extracts via [`extract_user_query`].
pub fn extract_last_user_query(conversation: &[ConversationItem]) -> Option<String> {
    conversation
        .iter()
        .rev()
        .find(|item| matches!(item, ConversationItem::User(_)))
        .map(|item| extract_user_query(&item.text_content()))
        .filter(|q| !q.is_empty())
}
/// The continuation prompt added after auto-compaction.
/// Stored here so query-extraction helpers can exclude it without a circular dependency
/// or a second hard-coded copy.
pub const AUTO_CONTINUE_PROMPT: &str = r#"Continue the conversation from where it left off without asking the user any further questions. Resume directly - do not acknowledge the summary, do not recap what was happening, do not preface with "I'll continue" or similar.
Pick up the last task as if the break never happened."#;
/// `false` twin: no preset in this build injects a bootstrap note.
fn is_bootstrap_reminder_text(_text: &str) -> bool {
    false
}
/// True when extracted query text is a synthetic session-internal turn, not a human prompt.
/// Covers empty metadata-only items, the `__auto_continue__` sentinel, [`AUTO_CONTINUE_PROMPT`],
/// and a `<system_reminder>` bootstrap note.
pub fn is_synthetic_extracted_query(text: &str) -> bool {
    text.is_empty()
        || text == "__auto_continue__"
        || text == AUTO_CONTINUE_PROMPT
        || is_bootstrap_reminder_text(text)
}
/// Classify whether a `ConversationItem` is a real user turn for compaction.
/// Not real if it is non-`User`, has `synthetic_reason`, or its extracted text is synthetic.
/// Image-only prompts ARE real — they must anchor the boundary even with no text.
pub fn is_real_user_turn(item: &ConversationItem) -> bool {
    match item {
        ConversationItem::User(u) => {
            if u.synthetic_reason.is_some() {
                return false;
            }
            let has_images = u
                .content
                .iter()
                .any(|p| matches!(p, ContentPart::Image { .. }));
            if has_images {
                return true;
            }
            let extracted = extract_user_query(&item.text_content());
            !is_synthetic_extracted_query(&extracted)
        }
        // Only a `User` item can anchor a human turn. A discovery item is named
        // explicitly: it must never become a compaction boundary, or the pair would be
        // split by the tail-window walk instead of snapped over.
        ConversationItem::Discovery { .. }
        | ConversationItem::System(_)
        | ConversationItem::Assistant(_)
        | ConversationItem::ToolResult(_)
        | ConversationItem::BackendToolCall(_)
        | ConversationItem::Reasoning(_) => false,
    }
}
/// Extract all real user queries, in order ([`is_real_user_turn`]).
/// Used where human-authored prompts must not be polluted by synthetics or compaction artifacts.
pub fn extract_real_user_queries(conversation: &[ConversationItem]) -> Vec<String> {
    conversation
        .iter()
        .filter(|item| is_real_user_turn(item))
        .map(|item| extract_user_query(&item.text_content()))
        .collect()
}
/// Extract the last real user query text, skipping synthetic turns.
/// Unlike [`extract_last_user_query`], this returns only content the user actually typed.
/// `None` when no real user query is found.
pub fn extract_last_real_user_query(conversation: &[ConversationItem]) -> Option<String> {
    conversation
        .iter()
        .rev()
        .find(|item| is_real_user_turn(item))
        .map(|item| extract_user_query(&item.text_content()))
}
/// The first conversation index a compaction tail window may retain.
///
/// `raw_start` is whatever boundary the caller found (last user turn, last real user
/// turn, last compaction anchor). It is moved DOWN, never up, so a window can never
/// begin between a `tool_search_call` and the `tool_search_output` that answers it.
/// Widening the window is the sanctioned answer to "the pair does not fit the
/// boundary": the alternative readings — keep the output alone (provider desync, and
/// the A-26 strip of the call) or drop the output alone (the A-26 strip of the only
/// record of the loaded tool set) — are both forbidden by ruling apex-waj.18, so the
/// cut moves instead. A window that gains a pair costs the compaction two items.
fn discovery_window_start(conversation: &[ConversationItem], raw_start: usize) -> usize {
    xai_grok_sampling_types::conversation::tool_search::snap_index_over_discovery_pairs(
        conversation,
        raw_start.min(conversation.len()),
    )
}
/// Whether a discovery item at `index` may be left out of a window.
///
/// `lone` is [`unpaired_discovery_indices`] computed over the FULL conversation,
/// which is the point of the whole helper (cut review F-1): the question "does this
/// half have a partner?" must be answered against every item the session holds. A
/// `call_id: null` pair whose halves are not physically adjacent is invisible to
/// anything that only sees the window, so a window-local answer calls a complete pair
/// two lone halves and deletes the provider's loaded-tool-set record — silently, out
/// of the only verbatim content a compacted history keeps.
fn discovery_is_certifiably_lone(lone: &[usize], index: usize) -> bool {
    lone.contains(&index)
}
/// A discovery item for a compaction window: verbatim, or dropped when the FULL
/// history certifies it has no partner. Never stubbed.
///
/// A tail window is the only verbatim content a compacted history keeps, so the
/// choice per discovery pair is: both halves verbatim, or neither. Half a pair is
/// not a smaller history — a lone `tool_search_call` is the strict-backend 400
/// shape and a lone `tool_search_output` names a call the provider never saw — and
/// placeholder-replacement is refused for the same reason it is accepted for a
/// `ToolResult`: the pair is the only record of which tools the provider loaded, so
/// stubbing it would be the A-26 strip wearing a different hat (ruling
/// apex-waj.18, dispatch XT-10: discovery must survive compaction).
///
/// The `Tool call omitted...` stubs at the call sites below stay for `ToolResult`
/// only; that data is re-derivable from the tool, the loaded tool set is not.
fn discovery_for_window(
    item: &xai_grok_sampling_types::conversation::tool_search::ToolSearchItem,
    index: usize,
    lone: &[usize],
) -> Option<ConversationItem> {
    if discovery_is_certifiably_lone(lone, index) {
        None
    } else {
        Some(ConversationItem::Discovery {
            item: item.clone(),
        })
    }
}
/// Extract messages since the last user message. Tool results are placeholder-replaced.
/// Uses the raw `User` boundary, which includes synthetics.
/// For compaction, prefer [`extract_messages_since_last_real_user`].
pub fn extract_messages_since_last_user(
    conversation: &[ConversationItem],
) -> Vec<ConversationItem> {
    // The same suffix as the previous backwards `take_while`, expressed as a start
    // index so the discovery snap can widen it (see [`discovery_window_start`]).
    let raw_start = conversation
        .iter()
        .rposition(|item| matches!(item, ConversationItem::User(_)))
        .map_or(0, |last_user_index| last_user_index + 1);
    verbatim_window_from(conversation, raw_start)
}
/// Extract messages since the last real user turn. Synthetics do not reset the boundary.
/// Prevents compaction from splitting an assistant/tool pair across a synthetic injection
/// (which would orphan a `ToolResult`). Tool results are placeholder-replaced.
pub fn extract_messages_since_last_real_user(
    conversation: &[ConversationItem],
) -> Vec<ConversationItem> {
    let raw_start = conversation.iter().rposition(is_real_user_turn).map_or(0, |idx| idx + 1);
    verbatim_window_from(conversation, raw_start)
}
/// The verbatim tail window: assistant turns, stubbed tool results and every closed
/// discovery group, taken from `raw_start` widened over any pair the cut would split.
fn verbatim_window_from(conversation: &[ConversationItem], raw_start: usize) -> Vec<ConversationItem> {
    let start = discovery_window_start(conversation, raw_start);
    let lone =
        xai_grok_sampling_types::conversation::tool_search::unpaired_discovery_indices(
            conversation,
        );
    conversation[start..]
        .iter()
        .enumerate()
        .filter_map(|(offset, item)| match item {
            ConversationItem::Assistant(a) => Some(ConversationItem::Assistant(a.clone())),
            ConversationItem::ToolResult(t) => Some(ConversationItem::ToolResult(ToolResultItem {
                tool_call_id: t.tool_call_id.clone(),
                content: std::sync::Arc::<str>::from("Tool call omitted..."),
                images: Vec::new(),
                is_error: false,
            })),
            // Verbatim pair, never the `Tool call omitted...` stub — see
            // [`discovery_for_window`] (ruling apex-waj.18 A-26 / XT-10).
            ConversationItem::Discovery { item } => discovery_for_window(item, start + offset, &lone),
            // System/user context is rebuilt around the window; backend tool calls
            // and reasoning items are not portable continuation context here.
            ConversationItem::System(_)
            | ConversationItem::User(_)
            | ConversationItem::BackendToolCall(_)
            | ConversationItem::Reasoning(_) => None,
        })
        .collect()
}
#[derive(Clone, Copy)]
enum AgentMessagePosition {
    Only,
    BeforeHuman,
    AfterHuman,
}
/// Latest raw agent-authored anchor and its structural order relative to human input.
#[derive(Clone)]
pub struct AgentMessageAnchor {
    item: ConversationItem,
    position: AgentMessagePosition,
}
fn extract_latest_agent_message(conversation: &[ConversationItem]) -> Option<AgentMessageAnchor> {
    let agent_message_index = conversation.iter().rposition(|item| {
        matches!(
            item,
            ConversationItem::User(user)
                if user.synthetic_reason == Some(SyntheticReason::AgentMessage)
        )
    })?;
    let position = match conversation.iter().rposition(is_real_user_turn) {
        Some(human_index) if agent_message_index < human_index => AgentMessagePosition::BeforeHuman,
        Some(_) => AgentMessagePosition::AfterHuman,
        None => AgentMessagePosition::Only,
    };
    Some(AgentMessageAnchor {
        item: conversation[agent_message_index].clone(),
        position,
    })
}
fn extract_messages_since_last_compaction_anchor(
    conversation: &[ConversationItem],
) -> Vec<ConversationItem> {
    let boundary = conversation.iter().rposition(|item| {
        is_real_user_turn(item)
            || matches!(
                item,
                ConversationItem::User(user)
                    if user.synthetic_reason == Some(SyntheticReason::AgentMessage)
            )
    });
    let raw_start = boundary.map_or(0, |idx| {
        if matches!(
            &conversation[idx],
            ConversationItem::User(user)
                if user.synthetic_reason == Some(SyntheticReason::AgentMessage)
        ) {
            idx
        } else {
            idx + 1
        }
    });
    // Pair-atomic window start + FULL-history lone certification: this is the
    // window `CompactionStateContext::build` puts into the reassembled history, i.e.
    // the ONLY verbatim content a compacted conversation keeps (ruling apex-waj.18
    // A-26; dispatch XT-10; cut review F-1).
    let start = discovery_window_start(conversation, raw_start);
    let lone =
        xai_grok_sampling_types::conversation::tool_search::unpaired_discovery_indices(
            conversation,
        );
    conversation[start..]
        .iter()
        .enumerate()
        .filter_map(|(offset, item)| match item {
            ConversationItem::User(user)
                if user.synthetic_reason == Some(SyntheticReason::AgentMessage) =>
            {
                Some(ConversationItem::User(user.clone()))
            }
            ConversationItem::Assistant(assistant) => {
                Some(ConversationItem::Assistant(assistant.clone()))
            }
            ConversationItem::ToolResult(result) => {
                Some(ConversationItem::ToolResult(ToolResultItem {
                    tool_call_id: result.tool_call_id.clone(),
                    content: std::sync::Arc::<str>::from("Tool call omitted..."),
                    images: Vec::new(),
                    is_error: false,
                }))
            }
            // The discovery pair rides whole and unstubbed: placeholder-replacing it
            // is how the loaded tool set would vanish.
            ConversationItem::Discovery { item } => discovery_for_window(item, start + offset, &lone),
            // Real human turns rebuild the head of the window, not its tail;
            // synthetic user turns other than the agent-message arm above, system
            // prompts, backend tool calls and reasoning items are all rebuilt or
            // dropped by the reassembly in `build_compacted_history`.
            ConversationItem::System(_)
            | ConversationItem::User(_)
            | ConversationItem::BackendToolCall(_)
            | ConversationItem::Reasoning(_) => None,
        })
        .collect()
}
/// Summary of a running subagent for compaction context.
/// Compaction-layer type; mapped from the protocol type in `run_compact_inner()`.
#[derive(Clone)]
pub struct RunningSubagentSummary {
    /// The subagent's unique ID.
    pub subagent_id: String,
    /// The agent type name (e.g. "Explore", "general-purpose").
    pub subagent_type: String,
    /// Human-readable description of what the subagent is doing.
    pub description: String,
    /// Wall-clock elapsed time since the subagent was spawned, in milliseconds.
    pub elapsed_ms: u64,
}
/// Summary of a running background task for compaction context.
#[derive(Clone)]
pub struct BackgroundTaskSummary {
    pub task_id: String,
    pub command: String,
    pub status: String,
    /// Model-facing name of the tool that created this task (e.g. `monitor`).
    /// `None` omits it from the reminder.
    pub tool_name: Option<String>,
}
/// Summary of a still-registered scheduled loop for compaction context.
#[derive(Clone)]
pub struct ScheduledLoopSummary {
    pub task_id: String,
    pub interval: String,
    pub next_fire_at: String,
    pub prompt: String,
    pub recurring: bool,
    pub durable: bool,
}
/// Summary of a still-live workflow run for compaction context.
#[derive(Clone)]
pub struct WorkflowRunSummary {
    pub name: String,
    pub run_id: String,
    pub status: String,
    pub objective: String,
    pub current_phase: Option<String>,
    pub agents_used: u64,
    pub agent_budget: Option<u64>,
    pub elapsed_ms: u64,
}
/// Summary of a connected MCP server for compaction context.
#[derive(Clone)]
pub struct CompactionServerSummary {
    pub name: String,
    pub tool_count: usize,
    pub description: Option<String>,
}
/// A dependency-free mirror of `TodoStatus` (xai-grok-tools), kept here so
/// this crate avoids that heavy dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoSummaryStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}
impl TodoSummaryStatus {
    pub fn is_actionable(self) -> bool {
        matches!(self, Self::Pending | Self::InProgress)
    }
    /// Mirrors `TodoStatus::tag()` in xai-grok-tools.
    pub fn tag(self) -> &'static str {
        match self {
            Self::Pending => "[pending]",
            Self::InProgress => "[in_progress]",
            Self::Completed => "[completed]",
            Self::Cancelled => "[cancelled]",
        }
    }
}
/// Compaction-layer summary of a todo item. Protocol-layer equivalent is
/// `TodoItem` in xai-grok-tools.
#[derive(Clone)]
pub struct TodoSummary {
    pub id: String,
    pub content: String,
    pub status: TodoSummaryStatus,
}
/// Context captured at compaction time. Pure data.
/// Rendering into system-reminder format is the consumer's job.
pub struct CompactionStateContext {
    /// Monotonic cwd generation; zero preserves the legacy compaction shape.
    pub cwd_generation: u64,
    /// Project instructions resolved for the latest destination cwd.
    pub destination_project_instructions: Option<String>,
    /// Latest agent-authored input and its order relative to the latest human turn.
    pub agent_message_anchor: Option<AgentMessageAnchor>,
    /// Messages since the latest human or agent-message compaction anchor.
    /// Runtime synthetic injections do not reset the boundary.
    pub recent_messages: Vec<ConversationItem>,
    /// The last real user query text (skips synthetic injections and
    /// auto-continue prompts).
    pub last_user_query: Option<String>,
    /// Files the agent edited this session (from agent_edited_paths).
    pub agent_edited_paths: Vec<String>,
    /// Running background tasks.
    pub running_tasks: Vec<BackgroundTaskSummary>,
    /// Subagents that are still running at compaction time.
    pub running_subagents: Vec<RunningSubagentSummary>,
    /// Connected MCP servers, for post-compaction system-reminder injection.
    pub connected_mcp_servers: Vec<CompactionServerSummary>,
    /// Todo list captured at compaction time, for post-compaction
    /// system-reminder injection.
    pub todos: Vec<TodoSummary>,
    /// Scheduled loops still registered at compaction time.
    pub scheduled_loops: Vec<ScheduledLoopSummary>,
    /// Non-terminal workflow runs at compaction time.
    pub workflows: Vec<WorkflowRunSummary>,
    /// Model-facing workflow tool name, when one could be resolved.
    pub workflow_tool_name: Option<String>,
}
/// Live session state captured at compaction time, fed to
/// [`CompactionStateContext::build`].
#[derive(Default)]
pub struct CompactionInputs {
    pub cwd_generation: u64,
    pub destination_project_instructions: Option<String>,
    pub running_tasks: Vec<BackgroundTaskSummary>,
    pub running_subagents: Vec<RunningSubagentSummary>,
    pub agent_edited_paths: BTreeSet<String>,
    pub connected_mcp_servers: Vec<CompactionServerSummary>,
    pub todos: Vec<TodoSummary>,
    pub scheduled_loops: Vec<ScheduledLoopSummary>,
    pub workflows: Vec<WorkflowRunSummary>,
    pub workflow_tool_name: Option<String>,
}
impl CompactionStateContext {
    /// Build the state context from current session state.
    /// Uses a typed compaction boundary for the retained tail while keeping the last-query field human-only.
    pub async fn build(conversation: &[ConversationItem], inputs: CompactionInputs) -> Self {
        Self {
            cwd_generation: inputs.cwd_generation,
            destination_project_instructions: inputs.destination_project_instructions,
            agent_message_anchor: extract_latest_agent_message(conversation),
            recent_messages: extract_messages_since_last_compaction_anchor(conversation),
            last_user_query: extract_last_real_user_query(conversation),
            agent_edited_paths: inputs.agent_edited_paths.into_iter().collect(),
            running_tasks: inputs.running_tasks,
            running_subagents: inputs.running_subagents,
            connected_mcp_servers: inputs.connected_mcp_servers,
            todos: inputs.todos,
            scheduled_loops: inputs.scheduled_loops,
            workflows: inputs.workflows,
            workflow_tool_name: inputs.workflow_tool_name,
        }
    }
    /// Create a task summary from individual fields.
    pub fn task_summary(
        task_id: String,
        command: String,
        status: &str,
        tool_name: Option<String>,
    ) -> BackgroundTaskSummary {
        BackgroundTaskSummary {
            task_id,
            command,
            status: status.to_string(),
            tool_name,
        }
    }
    /// Compaction view: drop the assistant/tool working tail, keep the latest agent-message anchor.
    /// For a single-real-user-turn sub-agent, keeping `recent_messages` frees almost nothing
    /// and re-cues the model to re-read the same files.
    pub fn for_compaction(&self) -> Self {
        Self {
            cwd_generation: self.cwd_generation,
            destination_project_instructions: self.destination_project_instructions.clone(),
            agent_message_anchor: self
                .agent_message_anchor
                .clone()
                .or_else(|| extract_latest_agent_message(&self.recent_messages)),
            recent_messages: Vec::new(),
            last_user_query: self.last_user_query.clone(),
            agent_edited_paths: self.agent_edited_paths.clone(),
            running_tasks: self.running_tasks.clone(),
            running_subagents: self.running_subagents.clone(),
            connected_mcp_servers: self.connected_mcp_servers.clone(),
            todos: self.todos.clone(),
            scheduled_loops: self.scheduled_loops.clone(),
            workflows: self.workflows.clone(),
            workflow_tool_name: self.workflow_tool_name.clone(),
        }
    }
}
/// Clean the compaction model's raw output into the plain-text `Summary:` block.
/// Leading scratchpad is stripped; control tokens echoed in the body are neutralized
/// so they cannot prime the next turn to re-emit a `<summary>` block.
pub fn format_compact_summary(summary: &str) -> String {
    let mut result = summary.to_string();
    while let Some(start) = result.find("<analysis>") {
        let is_leading = match result.find("<summary>") {
            Some(sp) => start < sp || result[sp + "<summary>".len()..start].trim().is_empty(),
            None => result[..start].trim().is_empty(),
        };
        if !is_leading {
            break;
        }
        match result[start..].find("</analysis>") {
            Some(rel) => {
                let end = start + rel + "</analysis>".len();
                result = format!("{}{}", &result[..start], &result[end..]);
            }
            None => {
                let drop_to = result[start..]
                    .find("<summary>")
                    .map_or(result.len(), |rel| start + rel);
                result = format!("{}{}", &result[..start], &result[drop_to..]);
                break;
            }
        }
    }
    if let Some(start) = result.find("<summary>")
        && let Some(end) = result.rfind("</summary>")
        && end > start
    {
        let before = result[..start].to_string();
        let after = result[end + "</summary>".len()..].to_string();
        let inner = strip_leading_scratchpad(result[start + "<summary>".len()..end].trim());
        result = format!("{before}Summary:\n{inner}{after}");
    }
    result = neutralize_compaction_control_tokens(&result);
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }
    result.trim().to_string()
}
/// Peel leading drafting scratchpad off an extracted `<summary>` block.
/// A markdown "**Analysis**" header has no opening tag; drop through the last `</analysis>`.
/// Skip the peel when the block already starts with a numbered section, so an echo cannot truncate.
fn strip_leading_scratchpad(inner: &str) -> String {
    let mut s = inner.trim();
    let lead = s.trim_start_matches(['#', '*', '-', '>', ' ', '\t']);
    if !lead.starts_with(|c: char| c.is_ascii_digit())
        && let Some(pos) = s.rfind("</analysis>")
    {
        s = s[pos + "</analysis>".len()..].trim_start();
    }
    if let Some(rest) = s.strip_prefix("<summary>") {
        s = rest.trim_start();
    }
    s.to_string()
}
/// Defuse compaction-control tokens echoed inside a summary body.
/// Insert a zero-width space after `<` so they cannot be read as live tags next turn.
/// Closers first so the inserted sentinel never re-matches.
fn neutralize_compaction_control_tokens(text: &str) -> String {
    text.replace("</summary>", "<\u{200b}/summary>")
        .replace("<summary>", "<\u{200b}summary>")
        .replace("</analysis>", "<\u{200b}/analysis>")
        .replace("<analysis>", "<\u{200b}analysis>")
        .replace("</summary_request>", "<\u{200b}/summary_request>")
        .replace("<summary_request>", "<\u{200b}summary_request>")
}
/// Clean tags via [`format_compact_summary`] and prepend the continuation
/// preamble. This is the user message content that replaces the compacted
/// conversation.
pub fn format_compact_summary_content(raw_summary: &str) -> String {
    let cleaned = format_compact_summary(raw_summary);
    format!(
        "This session is being continued from a previous conversation that ran out of context. \
         The summary below covers the earlier portion of the conversation.\n\n{cleaned}"
    )
}
/// Floor for the cleaned seed (degenerate band observed at 75–264
/// chars; smallest healthy prod summary observed at 3,242 chars).
const MIN_SUMMARY_SEED_CHARS: usize = 500;
/// True when the cleaned summary seed is too small to plausibly carry the
/// task state of the conversation it would replace. Callers should
/// retry like a transient failure.
pub fn is_degenerate_summary(raw_summary: &str) -> bool {
    format_compact_summary(raw_summary).chars().count() < MIN_SUMMARY_SEED_CHARS
}
/// Cap (in `char`s) for the rejected-summary text captured on
/// [`CompactionAttempt::summary`].
pub const MAX_CAPTURED_SUMMARY_CHARS: usize = 8_192;
/// Bound captured text for the request artifact: whole when within `max_chars`,
/// else head + tail around an elision marker. Splits on `char` boundaries.
pub fn bound_captured_output(s: &str, max_chars: usize) -> String {
    let total = s.chars().count();
    if total <= max_chars {
        return s.to_string();
    }
    let head = max_chars / 2;
    let tail = max_chars - head;
    let head_str: String = s.chars().take(head).collect();
    let tail_str: String = s.chars().skip(total - tail).collect();
    let elided = total - head - tail;
    format!("{head_str}\n\n…[{elided} chars elided]…\n\n{tail_str}")
}
/// Diagnostics for a single compaction model call (one retry-loop iteration).
/// Persisted on the request artifact so a degraded retry is not bumped invisibly.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CompactionAttempt {
    /// 1-based attempt index, cumulative across input-ladder stages.
    pub attempt: u32,
    /// `"success"`, `"degenerate"`, `"deterministic"`, or `"transient"`.
    pub outcome: String,
    /// Raw char count of the content produced this attempt; `0` if none.
    pub summary_chars: u64,
    /// Raw rejected summary text on a degenerate attempt (bounded by
    /// [`bound_captured_output`]). `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Error detail on a failed (`deterministic` / `transient`) attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
/// Render a `<transcript_location>` pointer block.
/// The carrier embeds this so the model can re-read pre-compaction detail instead of carrying the transcript.
/// Spliced right after `</summary_content>`; carries its own leading blank line.
pub fn format_transcript_location(path: &str) -> String {
    format!(
        "\n\n<transcript_location>\n\
         The full, unsummarized transcript of this conversation is saved at:\n{path}\n\
         If you need details that were dropped from the summary above (exact code, \
         error text, file contents, or earlier tool output), read this file to \
         recover them.\n\
         </transcript_location>"
    )
}
/// Wrap text in `<user_query>...</user_query>` tags.
/// Canonical wrapping shared by `xai-chat-state` and `xai-grok-shell`.
pub fn wrap_user_query(text: impl Into<String>) -> String {
    let text = text.into();
    format!("<user_query>\n{text}\n</user_query>")
}
/// Input data for building a compacted conversation history. Plain data, no I/O.
/// The caller generates the summary, renders the optional reminder, and supplies the user-message prefix.
pub struct CompactedHistoryInput<'a> {
    /// The original system message from the conversation.
    pub system_message: ConversationItem,
    /// The user-info / project-layout prefix (not wrapped in `<user_query>`).
    pub user_message_prefix: String,
    /// Pre-rendered AGENTS.md `<system-reminder>` block to re-inject after the
    /// user prefix. `None` means no project instructions to re-inject.
    /// This preserves project instructions verbatim across compaction.
    pub agents_md_reminder: Option<String>,
    /// State context snapshot taken before compaction cleared the conversation.
    pub state_context: &'a CompactionStateContext,
    /// The LLM-generated compaction summary text.
    pub compaction_summary: String,
    /// An optional pre-rendered `<system-reminder>` block to append after the
    /// summary. `None` means no state reminder is appended.
    pub system_reminder: Option<String>,
    /// When `true`, emit the compaction summary *before* recent messages.
    /// When `false` (the default), recent messages come first (grok-build
    /// ordering).
    pub summary_before_recent: bool,
    /// Pre-built transcript hint appended to the summary. `None` to omit.
    /// Appended to BOTH the carrier and the grok-build summary.
    pub transcript_hint: Option<String>,
    /// Number of summaries so far for this user query, including the one being built.
    /// Rendered verbatim into the carrier footer. Ignored by the grok-build path.
    /// Callers that don't track a counter pass `1`.
    pub summary_count: u64,
}
/// `None` twin: the alternate carrier format is not compiled in.
fn summary_before_recent_carrier(_input: &CompactedHistoryInput<'_>) -> Option<String> {
    None
}
/// This is a pure function with no I/O. It mirrors exactly what
/// `run_compact_inner` in `xai-grok-shell` assembles inline, but is
/// independently testable.
pub fn build_compacted_history(input: CompactedHistoryInput<'_>) -> Vec<ConversationItem> {
    let carrier = summary_before_recent_carrier(&input);
    let summary_first = carrier.is_some();
    let summary_item = carrier.map(ConversationItem::user_meta).unwrap_or_else(|| {
        let mut formatted_summary = format_compact_summary_content(&input.compaction_summary);
        if let Some(ref hint) = input.transcript_hint {
            formatted_summary.push_str(hint);
        }
        ConversationItem::user_meta(formatted_summary)
    });
    let mut compacted: Vec<ConversationItem> = vec![
        input.system_message,
        ConversationItem::user_meta(input.user_message_prefix),
    ];
    let project_instructions = if input.state_context.cwd_generation == 0 {
        input.agents_md_reminder.as_ref()
    } else {
        input
            .state_context
            .destination_project_instructions
            .as_ref()
    };
    if let Some(reminder) = project_instructions {
        compacted.push(ConversationItem::project_instructions(reminder.clone()));
    }
    let anchor = input
        .state_context
        .agent_message_anchor
        .clone()
        .filter(|_| {
            !input.state_context.recent_messages.iter().any(|item| {
                matches!(
                    item,
                    ConversationItem::User(user)
                        if user.synthetic_reason == Some(SyntheticReason::AgentMessage)
                )
            })
        });
    if let Some(anchor) = anchor
        .as_ref()
        .filter(|anchor| matches!(anchor.position, AgentMessagePosition::BeforeHuman))
    {
        compacted.push(anchor.item.clone());
    }
    if let Some(ref last_query) = input.state_context.last_user_query {
        compacted.push(ConversationItem::user(wrap_user_query(last_query)));
    }
    if let Some(anchor) =
        anchor.filter(|anchor| !matches!(anchor.position, AgentMessagePosition::BeforeHuman))
    {
        compacted.push(anchor.item);
    }
    if summary_first {
        compacted.push(summary_item);
        for msg in input.state_context.recent_messages.iter().cloned() {
            compacted.push(msg);
        }
    } else {
        for msg in input.state_context.recent_messages.iter().cloned() {
            compacted.push(msg);
        }
        compacted.push(summary_item);
    }
    if let Some(ref reminder) = input.system_reminder {
        compacted.push(ConversationItem::system_reminder(reminder.clone()));
    }
    compacted
}
/// Result of sanitizing a compacted conversation history.
pub struct SanitizeResult {
    /// The sanitized conversation items.
    pub items: Vec<ConversationItem>,
    /// `tool_call_id`s that were stripped because no preceding assistant
    /// `tool_calls` entry matched them.
    pub stripped_tool_call_ids: Vec<String>,
}
/// Check that every `ToolResult` has a matching preceding `Assistant.tool_calls[].id`.
/// Returns violating `tool_call_id`s (empty when valid). Read-only.
pub fn validate_compacted_history(items: &[ConversationItem]) -> Vec<String> {
    let mut seen_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut invalid_ids = Vec::new();
    for item in items {
        match item {
            ConversationItem::Assistant(a) => {
                for tc in &a.tool_calls {
                    seen_ids.insert(&tc.id);
                }
            }
            ConversationItem::ToolResult(tr) if !seen_ids.contains(tr.tool_call_id.as_str()) => {
                invalid_ids.push(tr.tool_call_id.clone());
            }
            // A result whose declaring assistant call came earlier is well-formed.
            ConversationItem::ToolResult(_) => {}
            // Deliberately NOT validated here, and that is a load-bearing decision:
            // a non-empty return makes the shell's compaction path throw away
            // `recent_messages` entirely (session/compaction.rs), i.e. report a
            // discovery problem here and the caller destroys the very pair this
            // invariant protects (ruling apex-waj.18 A-26). The fail-loud home for a
            // split pair is the pre-send shape guard
            // (`truncate_trailing_incomplete_tool_call`) and the outbound H-8 lint,
            // not this validator. Its pairing vocabulary stays Assistant↔ToolResult.
            ConversationItem::Discovery { .. }
            | ConversationItem::System(_)
            | ConversationItem::User(_)
            | ConversationItem::BackendToolCall(_)
            | ConversationItem::Reasoning(_) => {}
        }
    }
    invalid_ids
}
/// Remove orphaned `ToolResult` items that lack a matching preceding assistant call id.
/// Left-to-right: a result whose id is not yet seen is stripped (including result-before-call).
/// Unanswered assistant calls are NOT stripped — that is not the 400 invariant.
pub fn sanitize_compacted_history(items: Vec<ConversationItem>) -> SanitizeResult {
    let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut stripped_tool_call_ids = Vec::new();
    let sanitized = items
        .into_iter()
        .filter(|item| match item {
            ConversationItem::Assistant(a) => {
                for tc in &a.tool_calls {
                    seen_ids.insert(tc.id.as_ref().to_owned());
                }
                true
            }
            ConversationItem::ToolResult(tr) => {
                if seen_ids.contains(&tr.tool_call_id) {
                    true
                } else {
                    stripped_tool_call_ids.push(tr.tool_call_id.clone());
                    false
                }
            }
            // Kept unconditionally, and never co-dropped with its partner: a
            // `tool_search_output` IS the record of which tools the provider loaded,
            // so dropping it is the A-26 strip (ruling apex-waj.18). PLAN:947's
            // "drop client-executed outputs whose call is gone" belongs to T15 and is
            // deliberately not invented here.
            ConversationItem::Discovery { .. }
            | ConversationItem::System(_)
            | ConversationItem::User(_)
            | ConversationItem::BackendToolCall(_)
            | ConversationItem::Reasoning(_) => true,
        })
        .collect();
    SanitizeResult {
        items: sanitized,
        stripped_tool_call_ids,
    }
}
/// What [`repair_history`] changed; all-zero/empty means nothing was rewritten.
#[derive(Debug, Clone, Default)]
pub struct HistoryRepairReport {
    /// Duplicate `ToolResult` entries removed.
    pub duplicates_removed: usize,
    /// `tool_call_id`s of orphaned/displaced `ToolResult`s stripped — the
    /// shape behind "unexpected `tool_use_id` found in `tool_result` blocks".
    pub stripped_tool_result_ids: Vec<String>,
    /// Synthetic `ToolResult`s inserted for unanswered `tool_calls`.
    pub synthetic_results_inserted: usize,
}
impl HistoryRepairReport {
    /// Whether the repair modified the conversation.
    pub fn changed(&self) -> bool {
        self.duplicates_removed > 0
            || !self.stripped_tool_result_ids.is_empty()
            || self.synthetic_results_inserted > 0
    }
}
/// Repair provider tool-pairing violations (orphaned `ToolResult`s 400 on every request).
/// Dedup, strip displaced results, then backfill synthetic results for calls left unanswered.
/// Pure and idempotent.
pub fn repair_history(items: &mut Vec<ConversationItem>) -> HistoryRepairReport {
    let duplicates_removed = xai_grok_sampling_types::dedup_duplicate_tool_results(items);
    let stripped_tool_result_ids = strip_displaced_tool_results(items);
    let synthetic_results_inserted = xai_grok_sampling_types::repair_dangling_tool_calls(
        items,
        xai_grok_sampling_types::DanglingToolCallReason::HarnessHalted {
            class: "history_repair",
        },
    );
    HistoryRepairReport {
        duplicates_removed,
        stripped_tool_result_ids,
        synthetic_results_inserted,
    }
}
/// Strip `ToolResult`s not in the contiguous run immediately after their declaring `Assistant`.
/// Stricter than "matching id anywhere before" because providers require adjacency.
/// Same contiguous-run rule as the other repair passes so they agree on which calls are answered.
pub fn strip_displaced_tool_results(items: &mut Vec<ConversationItem>) -> Vec<String> {
    let mut run_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut stripped = Vec::new();
    items.retain(|item| match item {
        ConversationItem::Assistant(a) => {
            run_ids = a
                .tool_calls
                .iter()
                .map(|tc| tc.id.as_ref().to_owned())
                .collect();
            true
        }
        ConversationItem::ToolResult(tr) => {
            if run_ids.contains(&tr.tool_call_id) {
                true
            } else {
                stripped.push(tr.tool_call_id.clone());
                false
            }
        }
        // Transparent: the assistant→results run CONTINUES across a discovery
        // pair. Clearing `run_ids` here would strip every legitimately-paired
        // `ToolResult` that follows a provider-minted `tool_search_call` as
        // "displaced" — a real history rewrite, reported as
        // `stripped_tool_result_ids`. Same transparency decision as
        // `has_dangling_tool_calls` / `repair_dangling_tool_calls` in
        // xai-grok-sampling-types, so the repair passes still agree on which
        // calls are answered. The pair itself is never stripped (A-26).
        ConversationItem::Discovery { .. } => true,
        // Any other item does end the run: only the contiguous results after
        // their declaring assistant are wire-legal.
        ConversationItem::System(_)
        | ConversationItem::User(_)
        | ConversationItem::BackendToolCall(_)
        | ConversationItem::Reasoning(_) => {
            run_ids.clear();
            true
        }
    });
    stripped
}

// P2.1 ported region (open-grok@240c99c9 compaction_utils.rs:557 verbatim v2 family — see module header).

/// Maximum real-user history retained alongside a Codex remote-compaction-v2
/// item. This mirrors codex-rs's current retained-message budget.
pub const CODEX_REMOTE_COMPACTION_V2_RETAINED_USER_TOKENS: u64 = 64_000;

fn codex_remote_compaction_v2_user(item: &ConversationItem) -> Option<ConversationItem> {
    let ConversationItem::User(user) = item else {
        return None;
    };
    let retain = match user.synthetic_reason {
        Some(SyntheticReason::Interjection) => true,
        None => is_real_user_turn(item),
        Some(_) => false,
    };
    if !retain {
        return None;
    }

    let mut user = user.clone();
    user.content = user
        .content
        .into_iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => {
                let text = extract_user_query(&text);
                (!text.is_empty()).then(|| ContentPart::Text {
                    text: std::sync::Arc::<str>::from(text),
                })
            }
            image @ ContentPart::Image { .. } => Some(image),
        })
        .collect();
    (!user.content.is_empty()).then_some(ConversationItem::User(user))
}

/// Truncate only the text blocks of one boundary user message while retaining
/// all of its images and original content-block order. The upstream V2
/// retained-message contract budgets message text only; images are preserved
/// and do not consume this 64k allowance.
fn truncate_codex_remote_compaction_v2_user(
    item: ConversationItem,
    max_tokens: u64,
) -> Option<ConversationItem> {
    let ConversationItem::User(mut user) = item else {
        return None;
    };
    let mut remaining_text_tokens = max_tokens;
    let mut retained = Vec::with_capacity(user.content.len());
    for part in user.content {
        match part {
            ContentPart::Image { url } => retained.push(ContentPart::Image { url }),
            ContentPart::Text { text } => {
                if remaining_text_tokens == 0 {
                    continue;
                }
                let text_tokens = codex_remote_compaction_v2_text_tokens(text.len());
                if text_tokens <= remaining_text_tokens {
                    remaining_text_tokens -= text_tokens;
                    retained.push(ContentPart::Text { text });
                    continue;
                }

                let available = usize::try_from(
                    remaining_text_tokens.saturating_mul(xai_token_estimation::BYTES_PER_TOKEN),
                )
                .unwrap_or(usize::MAX);
                let kept = truncate_text_to_bytes(&text, available)
                    .unwrap_or_else(|| std::sync::Arc::clone(&text));
                remaining_text_tokens = 0;
                if !kept.is_empty() {
                    retained.push(ContentPart::Text { text: kept });
                }
            }
        }
    }
    user.content = retained;
    if user.content.is_empty() {
        return None;
    }
    let item = ConversationItem::User(user);
    (codex_remote_compaction_v2_user_text_tokens(&item) <= max_tokens).then_some(item)
}

fn codex_remote_compaction_v2_text_tokens(bytes: usize) -> u64 {
    u64::try_from(bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(xai_token_estimation::BYTES_PER_TOKEN.saturating_sub(1))
        / xai_token_estimation::BYTES_PER_TOKEN
}

fn codex_remote_compaction_v2_user_text_tokens(item: &ConversationItem) -> u64 {
    let ConversationItem::User(user) = item else {
        return 0;
    };
    user.content
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => codex_remote_compaction_v2_text_tokens(text.len()),
            ContentPart::Image { .. } => 0,
        })
        .sum::<u64>()
        .max(1)
}

fn fit_codex_remote_compaction_v2_user_tail(
    user_messages: Vec<ConversationItem>,
    max_tokens: u64,
) -> Vec<ConversationItem> {
    let mut remaining = max_tokens;
    let mut reversed = Vec::new();
    for item in user_messages.into_iter().rev() {
        if remaining == 0 {
            break;
        }
        let cost = codex_remote_compaction_v2_user_text_tokens(&item);
        if cost <= remaining {
            remaining -= cost;
            reversed.push(item);
            continue;
        }
        if let Some(item) = truncate_codex_remote_compaction_v2_user(item, remaining) {
            reversed.push(item);
        }
        break;
    }
    reversed.reverse();
    reversed
}

/// Build the provider replay history installed after a successful Codex
/// remote-compaction-v2 request.
///
/// The server's encrypted `compaction` item remains the authoritative summary.
/// A bounded tail of genuine user messages is retained ahead of it so the
/// latest user intent and multimodal inputs remain explicit. Synthetic user
/// messages, assistant output, and tool plumbing are intentionally excluded.
pub fn build_codex_remote_compaction_v2_history(
    prompt_input: &[ConversationItem],
    compaction_item: ConversationItem,
) -> Vec<ConversationItem> {
    let user_messages = prompt_input
        .iter()
        .filter_map(codex_remote_compaction_v2_user)
        .collect::<Vec<_>>();
    let mut retained = fit_codex_remote_compaction_v2_user_tail(
        user_messages,
        CODEX_REMOTE_COMPACTION_V2_RETAINED_USER_TOKENS,
    );
    retained.push(compaction_item);
    retained
}

/// Return genuine user messages appended while a compaction request was in
/// flight, provided the original snapshot is still an exact prefix.
///
/// A prefix mismatch means the conversation was edited, rewound, or otherwise
/// replaced while compaction ran; callers must not install a response derived
/// from stale history in that case. Matching append-only updates are safe, and
/// retaining their real user turns prevents a steer/interjection from being
/// erased by the replacement-history swap.
pub fn codex_remote_compaction_v2_interjections(
    snapshot: &[ConversationItem],
    current: &[ConversationItem],
) -> Option<Vec<ConversationItem>> {
    if current.len() < snapshot.len() {
        return None;
    }
    let unchanged = snapshot.iter().zip(current).all(|(expected, actual)| {
        serde_json::to_value(expected).ok() == serde_json::to_value(actual).ok()
    });
    if !unchanged {
        return None;
    }
    Some(
        current[snapshot.len()..]
            .iter()
            .filter_map(codex_remote_compaction_v2_user)
            .collect(),
    )
}
#[cfg(test)]
#[path = "compaction_utils_tests.rs"]
mod tests;
