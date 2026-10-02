//! Conversation types: re-exports the canonical set from `xai_grok_sampling_types` plus grok-shell-specific additions.

use std::collections::HashSet;

pub use xai_grok_sampling_types::conversation::*;

#[cfg(test)]
#[path = "conversation_tests.rs"]
mod tests;

/// Tracing context for conversation requests; satisfies `TraceContext` through its blanket impl.
/// Lives in grok-shell because it references shell-internal config and upload types.
#[derive(Debug, Clone)]
pub struct ConversationRequestTrace {
    pub gcs_config: crate::session::repo_changes::TraceExportConfig,
    #[expect(
        dead_code,
        reason = "retained for snapshot compat; wire when sampler path uploads traces"
    )]
    pub(crate) artifact_tracker: Option<crate::upload::manifest::ArtifactTracker>,
}

/// The shell's ONE history cut: keep everything before prompt-turn
/// `target_prompt_index` and drop the rest, in place.
///
/// Every cut the shell makes goes through here — the standard rewind
/// (`acp_session_impl/rewind.rs`), a cancelled turn being rewound
/// (`acp_session_impl/cancel.rs`), both replay paths
/// (`helpers/replay.rs`, three sites) and the persisted fork copy
/// (`storage/jsonl/copy.rs`) — because the count comes from
/// `conversation_truncate_for_prompt`, which snaps the boundary DOWN over any
/// `tool_search_call` / `tool_search_output` pair it would otherwise split
/// (apex-waj.21 review F-4). A site that computed its own count, or truncated on a
/// raw prompt index, would persist half a pair to the wire and to disk: the
/// strict-backend 400 on the next request and the provider's loaded-tool-set desync
/// from then on (ruling apex-waj.18 A-26).
pub(crate) fn truncate_conversation_for_prompt(
    items: &mut Vec<ConversationItem>,
    target_prompt_index: usize,
) {
    let keep = xai_grok_sampling_types::conversation::conversation_truncate_for_prompt(
        items,
        target_prompt_index,
    );
    items.truncate(keep);
}

/// The raw-index sibling of [`truncate_conversation_for_prompt`]: cut at an index the
/// caller already computed, snapping DOWN over a discovery pair first.
///
/// Two checkpoint rewind paths (`session/helpers/replay.rs`, both the replay-state and
/// the session-state variant) compute their cut themselves — `checkpoint_base_len + i`
/// for the `User` row the marker names — so they never pass through the prompt-counting
/// helper above. A cut on a `User` row is exactly where a `call_id: null` pair can
/// straddle (the harness injects its synthetic row between the halves), so these sites
/// had to be brought under the same snap (apex-waj.21 review F-4, coordinator
/// adjudication: the promise is only worth what every cut site honours).
pub(crate) fn truncate_conversation_at(items: &mut Vec<ConversationItem>, at: usize) {
    let snapped = xai_grok_sampling_types::conversation::tool_search::snap_index_over_discovery_pairs(
        items, at,
    );
    items.truncate(snapped);
}

/// Filters chat history copied into a fork. Drops synthetic user messages, then truncates at the last complete turn so the child never sees a partial one.
/// A turn is complete when the Assistant's tool calls are all answered; Reasoning, BackendToolCall and Discovery items are transparent to the scan.
/// Keep the "complete turn" definition in sync with `count_complete_turns` in `xai-grok-subagent-resolution/src/context.rs`.
pub(crate) fn fork_filter_chat(items: &mut Vec<ConversationItem>) {
    items.retain(|item| match item {
        ConversationItem::User(u) => u.synthetic_reason.is_none(),
        // A discovery pair is KEPT: it is provider-side loaded-tool state, and a
        // child that inherits the history must inherit the pair too (A-26). It is
        // also not a synthetic user message, which is the only thing this pass removes.
        ConversationItem::Discovery { .. }
        | ConversationItem::System(_)
        | ConversationItem::Assistant(_)
        | ConversationItem::ToolResult(_)
        | ConversationItem::BackendToolCall(_)
        | ConversationItem::Reasoning(_) => true,
    });

    // Only Assistant advances the boundary; everything else is transparent.
    let mut last_complete_end = 0;
    let mut i = 0;
    while i < items.len() {
        match &items[i] {
            ConversationItem::System(_) => {
                last_complete_end = i + 1;
                i += 1;
            }
            ConversationItem::Assistant(asst) => {
                let expected: HashSet<&str> =
                    asst.tool_calls.iter().map(|tc| tc.id.as_ref()).collect();
                let mut found = HashSet::new();
                let mut j = i + 1;
                while j < items.len() {
                    match &items[j] {
                        ConversationItem::ToolResult(tr) => {
                            if expected.contains(tr.tool_call_id.as_str()) {
                                found.insert(tr.tool_call_id.as_str());
                            }
                            j += 1;
                        }
                        // A discovery pair interleaved here is provider state, NOT the
                        // end of the result run: breaking on it made every legitimately
                        // answered call look unanswered, so the fork silently truncated
                        // everything from that turn on (child-history loss).
                        ConversationItem::Reasoning(_)
                        | ConversationItem::BackendToolCall(_)
                        | ConversationItem::Discovery { .. } => {
                            j += 1;
                        }
                        // Only a real turn boundary ends the run.
                        ConversationItem::User(_)
                        | ConversationItem::Assistant(_)
                        | ConversationItem::System(_) => break,
                    }
                }
                if found == expected {
                    last_complete_end = j;
                    i = j;
                } else {
                    break; // Dangling tool calls: stop at the last complete boundary
                }
            }
            // Everything else is transparent to the boundary walk: it neither opens
            // nor completes a turn. A discovery pair outside an assistant's result
            // run is skipped like a `Reasoning` sibling would be — it is kept by the
            // retain above, and it does not by itself advance the truncation boundary.
            ConversationItem::User(_)
            | ConversationItem::ToolResult(_)
            | ConversationItem::BackendToolCall(_)
            | ConversationItem::Reasoning(_)
            | ConversationItem::Discovery { .. } => {
                i += 1;
            }
        }
    }

    // Pair-atomic boundary (apex-waj.21 review F-5): the scan above is transparent
    // to a discovery pair, so `last_complete_end` can land BETWEEN the halves —
    // an assistant run closes at a `User` row, and a pair whose call sits before
    // that row and whose answer sits after it would be cut in half, handing the
    // child a lone `tool_search_call` (strict-backend 400). Snapping DOWN drops the
    // whole pair with the rest of the incomplete turn instead of keeping half.
    let last_complete_end =
        xai_grok_sampling_types::conversation::tool_search::snap_index_over_discovery_pairs(
            items,
            last_complete_end,
        );
    items.truncate(last_complete_end);

    // The snap cannot help with a pair that has only ONE half in the history at all.
    // A run ending `[assistant(no tool_calls), tool_search_call]` is complete by this
    // scan's own rule (nothing dangles, the search is not a client tool call), so the
    // boundary sits past the call and the child would open its first request with a
    // lone `tool_search_call` — the strict-backend 400 the snap above exists to prevent
    // (cut review WAJ21R2-02). An unanswered call carries no loaded-tool-set payload
    // either: the payload is the OUTPUT, so dropping it is not the A-26 strip, and the
    // child simply re-searches and re-pays. Pair-atomic, exactly like the recap trim:
    // the partner goes wherever it sits, so the child never inherits the other half
    // alone (`session_recap::pop_trailing_tool_run` applies the same rule).
    use xai_grok_sampling_types::conversation::tool_search::trailing_discovery_is_unpaired;
    while trailing_discovery_is_unpaired(items) {
        let key = items.last().and_then(ConversationItem::discovery).and_then(|search| {
            search.call_id().map(str::to_owned)
        });
        items.pop();
        if let Some(key) = key {
            items.retain(|other| {
                other
                    .discovery()
                    .is_none_or(|discovery| discovery.call_id() != Some(key.as_str()))
            });
        }
    }
}
