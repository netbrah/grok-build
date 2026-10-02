//! The native-fork prefix's discovery-pair decisions (apex-waj.21 r2, cut review R2L2-02).
//!
//! These arms were booked as "compile-time + comment only, no behavioural claim" and had
//! neither a test nor a mutant, while in fact deciding what a forked child inherits. Two of
//! them decided the wrong way for the common shape: a parent history ending on an ANSWERED
//! `[tool_search_call, tool_search_output]` pair was rejected as if it were an unanswered
//! call, so the child lost the provider's loaded-tool-set record and re-paid for the search;
//! and the turn window's `split_off` could start between the halves, handing the child a
//! lone output. Declared from `agent/subagent/mod.rs`, next to the code they pin (house rule
//! AGENTS.md §7).

use super::{clean_fork_prefix_len, conversation_tail_is_complete, select_native_fork_turns};
use xai_grok_sampling_types::ConversationItem;
use xai_grok_sampling_types::conversation::tool_search::ToolSearchItem;

fn discovery(raw: serde_json::Value) -> ConversationItem {
    ConversationItem::Discovery {
        item: ToolSearchItem::from_wire(raw).expect("fixture is a tool_search item"),
    }
}

fn call(call_id: &str) -> ConversationItem {
    discovery(serde_json::json!({
        "type": "tool_search_call",
        "id": format!("tsc_fork_{call_id}"),
        "call_id": call_id,
        "status": "completed",
        "execution": "client",
        "arguments": { "query": "crm order management", "limit": 8 }
    }))
}

fn output(call_id: &str) -> ConversationItem {
    discovery(serde_json::json!({
        "type": "tool_search_output",
        "id": format!("tso_fork_{call_id}"),
        "call_id": call_id,
        "status": "completed",
        "execution": "client",
        "tools": [{ "type": "function", "name": "crm_fixture_tool_00" }]
    }))
}

fn discovery_count(items: &[ConversationItem]) -> usize {
    items
        .iter()
        .filter(|item| item.discovery().is_some())
        .count()
}

/// An answered pair at the tail is the provider's own record of which tools are loaded.
/// Rejecting the VARIANT (the shipped arm) dropped it from every native fork whose parent
/// happened to end on a completed search — the child re-ran the search it had already been
/// answered for. The predicate is the pair, not the variant.
#[test]
fn a_closed_discovery_pair_at_the_tail_stays_in_the_native_fork_prefix() {
    let items = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("find the crm tools"),
        ConversationItem::assistant("searching"),
        ConversationItem::user("and the shipping one?"),
        ConversationItem::assistant("searching again"),
        call("call_fork_closed"),
        output("call_fork_closed"),
    ];
    assert_eq!(
        clean_fork_prefix_len(&items),
        items.len(),
        "a completed pair is a clean fork boundary: {items:?}"
    );
    let selected = select_native_fork_turns(items.clone(), Some(1));
    assert_eq!(
        discovery_count(&selected),
        2,
        "the child inherits the loaded-tool-set record verbatim (A-26): {selected:?}"
    );
    let kinds: Vec<&str> = selected
        .iter()
        .filter_map(ConversationItem::discovery)
        .map(|search| search.kind().item_type())
        .collect();
    assert_eq!(
        kinds,
        vec!["tool_search_call", "tool_search_output"],
        "and it inherits them in the order the wire reads them"
    );
}

/// The shape that genuinely is a mid-turn artifact: a call with no answer yet. It must not
/// ride into the child — that is the strict-backend 400, and it carries no loaded-tool-set
/// payload anyway.
#[test]
fn an_unanswered_search_tail_is_not_a_clean_fork_prefix() {
    let items = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("find the crm tools"),
        ConversationItem::assistant("searching"),
        call("call_fork_lone"),
    ];
    let len = clean_fork_prefix_len(&items);
    assert_eq!(
        len,
        3,
        "the walk stops below the unanswered call rather than forking it: {items:?}"
    );
    assert_eq!(discovery_count(&items[..len]), 0);
}

/// The turn-window cut: a `User` row can sit BETWEEN the halves (the harness's own
/// interjection, or a provider echo), and `split_off` on that index hands the child an
/// answer whose call stayed in the discarded head. The start snaps DOWN over the pair.
#[test]
fn the_native_fork_turn_window_never_starts_inside_a_discovery_pair() {
    let items = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("first prompt"),
        ConversationItem::assistant("first answer"),
        call("call_fork_straddle"),
        ConversationItem::user("second prompt"),
        output("call_fork_straddle"),
        ConversationItem::assistant("second answer"),
    ];
    let selected = select_native_fork_turns(items, Some(1));
    assert_eq!(
        discovery_count(&selected),
        2,
        "the window widened below the call instead of starting between the halves: \
         {selected:?}"
    );
    assert!(
        matches!(selected.first(), Some(ConversationItem::System(_))),
        "the System head is still hoisted to the front: {selected:?}"
    );
}

/// `conversation_tail_is_complete` keeps the VERBATIM mirror's contract as written: the
/// parent must end on a plain assistant text turn, so a history ending on a search —
/// answered or not — goes to the summarised path. That is deliberate and pre-existing (the
/// same rule rejects a trailing `ToolResult` or `Reasoning`); pinned here so a future
/// change to it is a decision and not a drift.
#[test]
fn a_search_ended_history_does_not_take_the_verbatim_mirror_path() {
    let answered_pair_tail = vec![
        ConversationItem::user("find the crm tools"),
        ConversationItem::assistant("searching"),
        call("call_fork_verbatim"),
        output("call_fork_verbatim"),
    ];
    assert!(
        !conversation_tail_is_complete(&answered_pair_tail),
        "the verbatim mirror requires a text-final assistant turn"
    );
    let text_tail = vec![
        ConversationItem::user("find the crm tools"),
        ConversationItem::assistant("the tools are …"),
    ];
    assert!(conversation_tail_is_complete(&text_tail));
}
