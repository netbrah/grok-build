use super::fork_filter_chat;
use xai_grok_sampling_types::conversation::ConversationItem;

#[test]
fn fork_filter_removes_synthetic_user_messages() {
    use xai_grok_sampling_types::conversation::*;

    let mut items = vec![
        ConversationItem::system("system prompt"),
        ConversationItem::user("real question"),
        ConversationItem::User(UserItem {
            content: vec![ContentPart::Text {
                text: "doom loop".into(),
            }],
            synthetic_reason: Some(SyntheticReason::SystemReminder),
            ..Default::default()
        }),
        ConversationItem::assistant("response"),
    ];
    fork_filter_chat(&mut items);
    assert!(
        !items.iter().any(|i| match i {
            ConversationItem::User(u) => u.synthetic_reason.is_some(),
            _ => false,
        }),
        "synthetic messages should be stripped"
    );
}
#[test]
fn fork_filter_truncates_at_complete_turn() {
    let mut items = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("q1"),
        ConversationItem::assistant("a1"),
        ConversationItem::user("q2"),
    ];
    fork_filter_chat(&mut items);
    assert_eq!(items.len(), 3, "should truncate after last complete turn");
    assert!(matches!(items[2], ConversationItem::Assistant(_)));
}
#[test]
fn fork_filter_consecutive_users_with_tool_calls() {
    use xai_grok_sampling_types::conversation::*;

    let mut items = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("prefix"),
        ConversationItem::user("query"),
        ConversationItem::Assistant(AssistantItem {
            content: String::new().into(),
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "bash".into(),
                arguments: "{}".into(),
            }],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        ConversationItem::tool_result("tc1", "output"),
        ConversationItem::user("follow-up"),
    ];
    fork_filter_chat(&mut items);
    assert_eq!(
        items.len(),
        5,
        "should keep through complete tool turn, drop incomplete follow-up"
    );
}
#[test]
fn fork_filter_preserves_complete_tool_turn() {
    use xai_grok_sampling_types::conversation::*;

    let mut items = vec![
        ConversationItem::user("q"),
        ConversationItem::Assistant(AssistantItem {
            content: String::new().into(),
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "bash".into(),
                arguments: "{}".into(),
            }],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        ConversationItem::tool_result("tc1", "output"),
    ];
    fork_filter_chat(&mut items);
    assert_eq!(items.len(), 3, "complete tool turn should be preserved");
}
#[test]
fn fork_filter_strips_incomplete_tool_turn() {
    use xai_grok_sampling_types::conversation::*;

    let mut items = vec![
        ConversationItem::user("q1"),
        ConversationItem::assistant("a1"),
        ConversationItem::user("q2"),
        ConversationItem::Assistant(AssistantItem {
            content: String::new().into(),
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "bash".into(),
                arguments: "{}".into(),
            }],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
    ];
    fork_filter_chat(&mut items);
    assert_eq!(
        items.len(),
        2,
        "should truncate before incomplete tool turn (trailing user(q2) also dropped)"
    );
    assert!(matches!(items[0], ConversationItem::User(_)));
    assert!(matches!(items[1], ConversationItem::Assistant(_)));
}
#[test]
fn fork_filter_keeps_turn_with_reasoning_between_user_and_assistant() {
    use xai_grok_sampling_types::conversation::*;

    // Reasoning between the user query and the assistant must not end the turn scan
    let mut items = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("q"),
        ConversationItem::Reasoning(xai_grok_sampling_types::synthesized_reasoning_item(
            "thinking",
        ).into()),
        ConversationItem::assistant("a"),
    ];
    fork_filter_chat(&mut items);
    assert_eq!(
        items.len(),
        4,
        "reasoning between user and assistant must not truncate the turn: got {items:?}"
    );
    assert!(matches!(items[3], ConversationItem::Assistant(_)));
}
#[test]
fn fork_filter_keeps_multi_tool_turn_with_reasoning_between_results() {
    use xai_grok_sampling_types::conversation::*;

    // Reasoning between the tool results must not hide the second result from the completeness scan
    let mut items = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("q"),
        ConversationItem::Reasoning(xai_grok_sampling_types::synthesized_reasoning_item("plan").into()),
        ConversationItem::Assistant(AssistantItem {
            content: String::new().into(),
            tool_calls: vec![
                ToolCall {
                    id: "tc1".into(),
                    name: "bash".into(),
                    arguments: "{}".into(),
                },
                ToolCall {
                    id: "tc2".into(),
                    name: "grep".into(),
                    arguments: "{}".into(),
                },
            ],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        ConversationItem::tool_result("tc1", "out1"),
        ConversationItem::Reasoning(xai_grok_sampling_types::synthesized_reasoning_item("mid").into()),
        ConversationItem::tool_result("tc2", "out2"),
        ConversationItem::Reasoning(xai_grok_sampling_types::synthesized_reasoning_item(
            "reflect",
        ).into()),
        ConversationItem::assistant("final"),
    ];
    fork_filter_chat(&mut items);
    assert_eq!(
        items.len(),
        9,
        "multi-tool turn with reasoning between results must be fully kept: got {items:?}"
    );
    match items.last() {
        Some(ConversationItem::Assistant(a)) => assert_eq!(a.content.as_ref(), "final"),
        other => panic!("expected final assistant text last, got {other:?}"),
    }
}
#[test]
fn fork_filter_drops_trailing_incomplete_goal_turn_after_reasoning() {
    use xai_grok_sampling_types::conversation::*;

    // The /goal turn is still running: a trailing user message with no assistant reply
    // It must be dropped even though a Reasoning item sits before the prior assistant
    let mut items = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("q"),
        ConversationItem::Reasoning(xai_grok_sampling_types::synthesized_reasoning_item(
            "thinking",
        ).into()),
        ConversationItem::assistant("a"),
        ConversationItem::user("/goal do the thing"),
    ];
    fork_filter_chat(&mut items);
    assert_eq!(
        items.len(),
        4,
        "trailing bare /goal user turn must be dropped: got {items:?}"
    );
    match items.last() {
        Some(ConversationItem::Assistant(a)) => assert_eq!(a.content.as_ref(), "a"),
        other => panic!("expected trailing assistant, got {other:?}"),
    }
}

/// apex-waj.21 (A-26): a discovery pair interleaved between an assistant's tool calls
/// and their results is provider state, NOT the end of the result run. Before the arm
/// landed, the inner scan broke on it, the calls looked unanswered, and the fork
/// silently truncated everything from that turn on — child-history loss.
#[test]
fn fork_filter_keeps_a_turn_that_has_a_discovery_pair_inside_its_result_run() {
    use xai_grok_sampling_types::conversation::tool_search::ToolSearchItem;
    use xai_grok_sampling_types::ToolCall;
    let discovery = |raw: serde_json::Value| ConversationItem::Discovery {
        item: ToolSearchItem::from_wire(raw).expect("fixture is a tool_search item"),
    };
    let mut items = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("find the crm tools"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_1".into(),
            name: "read_file".to_string(),
            arguments: "{}".into(),
        }]),
        discovery(serde_json::json!({
            "type": "tool_search_call",
            "id": "tsc_fork_1",
            "call_id": "call_fork_1",
            "status": "completed",
            "execution": "client",
            "arguments": { "query": "crm order management", "limit": 8 }
        })),
        discovery(serde_json::json!({
            "type": "tool_search_output",
            "id": "tso_fork_1",
            "call_id": "call_fork_1",
            "status": "completed",
            "execution": "client",
            "tools": [{ "type": "function", "name": "crm_fixture_tool_00" }]
        })),
        ConversationItem::tool_result("call_1", "hits"),
        ConversationItem::assistant("found them"),
    ];
    fork_filter_chat(&mut items);
    assert_eq!(
        items.len(),
        7,
        "the whole turn must survive the fork scan, pair included: {items:?}"
    );
    assert_eq!(
        items
            .iter()
            .filter(|item| item.discovery().is_some())
            .count(),
        2,
        "the child inherits the loaded tool set verbatim (A-26)"
    );
    assert!(matches!(
        items.last(),
        Some(ConversationItem::Assistant(a)) if a.content.as_ref() == "found them"
    ));
    // And a user turn is still the boundary that stops the run: the pair never
    // makes an INCOMPLETE turn look complete.
    let mut incomplete = vec![
        ConversationItem::system("sys"),
        ConversationItem::user("q"),
        ConversationItem::assistant_tool_calls(vec![ToolCall {
            id: "call_2".into(),
            name: "read_file".to_string(),
            arguments: "{}".into(),
        }]),
        discovery(serde_json::json!({
            "type": "tool_search_call",
            "id": "tsc_fork_2",
            "call_id": "call_fork_2",
            "status": "completed",
            "execution": "client",
            "arguments": { "query": "crm", "limit": 1 }
        })),
    ];
    fork_filter_chat(&mut incomplete);
    assert!(
        !incomplete
            .iter()
            .any(|item| matches!(item, ConversationItem::Assistant(a) if !a.tool_calls.is_empty())),
        "an unanswered call must not be forked just because a discovery item follows: {incomplete:?}"
    );
}

/// apex-waj.21 review F-5: the scan is transparent to a pair, so the truncation
/// boundary it computes can land BETWEEN the halves — an assistant run closes at the
/// `User` row that sits between them, which keeps the call and drops the answer. The
/// child then opens its first request with a lone `tool_search_call`, the strict-backend
/// 400 shape. The boundary is snapped DOWN, so the child gets both halves or neither.
#[test]
fn fork_filter_never_hands_the_child_a_lone_half_of_a_discovery_pair() {
    use xai_grok_sampling_types::conversation::tool_search::ToolSearchItem;
    let discovery = |raw: serde_json::Value| ConversationItem::Discovery {
        item: ToolSearchItem::from_wire(raw).expect("fixture is a tool_search item"),
    };
    let call = || {
        discovery(serde_json::json!({
            "type": "tool_search_call",
            "id": "tsc_fork_straddle",
            "call_id": "call_fork_straddle",
            "status": "completed",
            "execution": "client",
            "arguments": { "query": "crm", "limit": 1 }
        }))
    };
    let output = || {
        discovery(serde_json::json!({
            "type": "tool_search_output",
            "id": "tso_fork_straddle",
            "call_id": "call_fork_straddle",
            "status": "completed",
            "execution": "client",
            "tools": [{ "type": "function", "name": "crm_fixture_tool_00" }]
        }))
    };

    // The pair straddles the turn boundary: the run above closes at the `User` row,
    // index 3, which is between the call (2) and its output (4).
    let mut straddling = vec![
        ConversationItem::user("find the crm tools"),
        ConversationItem::assistant("thinking"),
        call(),
        ConversationItem::user("and now?"),
        output(),
    ];
    fork_filter_chat(&mut straddling);
    assert_eq!(
        straddling
            .iter()
            .filter(|item| item.discovery().is_some())
            .count(),
        0,
        "the boundary must drop the whole pair with the incomplete turn, never keep half \
         of it: {straddling:?}"
    );
    assert!(matches!(
        straddling.last(),
        Some(ConversationItem::Assistant(a)) if a.content.as_ref() == "thinking"
    ));

    // The same pair fully inside the retained region rides whole — the snap moves the
    // cut DOWN only, so it never widens a truncation into lost history.
    let mut whole = vec![
        ConversationItem::user("find the crm tools"),
        call(),
        output(),
        ConversationItem::assistant("found them"),
        ConversationItem::user("and now?"),
    ];
    fork_filter_chat(&mut whole);
    assert_eq!(
        whole
            .iter()
            .filter(|item| item.discovery().is_some())
            .count(),
        2,
        "an answered pair inside the forked history is inherited verbatim (A-26): \
         {whole:?}"
    );

    // Cut review WAJ21R2-02: the shape the snap cannot reach, because only ONE half
    // exists in the history. `[user, assistant(no tool calls), tool_search_call]` is
    // COMPLETE by the scan's own rule — nothing dangles, and a search is not a client
    // tool call — so `last_complete_end` walks past the call and `splits` is false for a
    // singleton group. Without the trailing-unpaired pop the child is handed a lone
    // `tool_search_call`, which is the very 400 the F-5 comment above says is prevented.
    let mut lone_call = vec![
        ConversationItem::user("find the crm tools"),
        ConversationItem::assistant("thinking"),
        call(),
    ];
    fork_filter_chat(&mut lone_call);
    assert_eq!(
        lone_call
            .iter()
            .filter(|item| item.discovery().is_some())
            .count(),
        0,
        "an unanswered search must not ride into the child alone: {lone_call:?}"
    );
    assert_eq!(
        lone_call.len(),
        2,
        "only the unanswered call goes; the turn above it is a clean boundary"
    );

    // Its mirror: a lone trailing OUTPUT whose call is nowhere in the forked history.
    let mut lone_output = vec![
        ConversationItem::user("find the crm tools"),
        ConversationItem::assistant("thinking"),
        output(),
    ];
    fork_filter_chat(&mut lone_output);
    assert_eq!(
        lone_output
            .iter()
            .filter(|item| item.discovery().is_some())
            .count(),
        0,
        "an answer with no call in the fork references a call the provider never saw: \
         {lone_output:?}"
    );
}

/// apex-waj.21 review F-4: every shell history cut — standard rewind, cancelled-turn
/// rewind, both replay paths and the persisted fork copy — now goes through this one
/// helper, so the pair-atomicity of the cut has ONE witness and one owner instead of six
/// places that each have to remember. The snap only moves DOWN, so a cut that would land
/// inside a pair drops the WHOLE pair (a truncation never gains history back); the count
/// itself is pinned over in
/// `conversation.rs::truncate_for_prompt_never_returns_a_count_inside_a_discovery_pair`.
#[test]
fn truncate_conversation_for_prompt_cuts_a_pair_whole() {
    use xai_grok_sampling_types::conversation::tool_search::ToolSearchItem;
    let discovery = |raw: serde_json::Value| ConversationItem::Discovery {
        item: ToolSearchItem::from_wire(raw).expect("fixture is a tool_search item"),
    };
    let history = || {
        vec![
            ConversationItem::user("find the crm tools"), // prompt 0
            discovery(serde_json::json!({
                "type": "tool_search_call",
                "id": "tsc_cut_shell",
                "call_id": "call_cut_shell",
                "status": "completed",
                "execution": "client",
                "arguments": { "query": "crm", "limit": 1 }
            })),
            ConversationItem::assistant("searching"),
            ConversationItem::user("and now?"), // prompt 1 — between the halves
            discovery(serde_json::json!({
                "type": "tool_search_output",
                "id": "tso_cut_shell",
                "call_id": "call_cut_shell",
                "status": "completed",
                "execution": "client",
                "tools": [{ "type": "function", "name": "crm_fixture_tool_00" }]
            })),
            ConversationItem::assistant("found them"),
            ConversationItem::user("next topic"), // prompt 2 — above the whole pair
        ]
    };
    let kept_discovery = |items: &[ConversationItem]| {
        items
            .iter()
            .filter(|item| item.discovery().is_some())
            .count()
    };

    // A cut ABOVE both halves keeps the pair verbatim: the snap never moves up. The
    // legacy count for "keep through prompt 1" is index 6 — above the pair's last
    // member at 4 — so nothing moves.
    let mut past_pair = history();
    super::truncate_conversation_for_prompt(&mut past_pair, 1);
    assert_eq!(
        kept_discovery(&past_pair),
        2,
        "the pair rides a cut that does not split it: {past_pair:?}"
    );

    // A cut BETWEEN the halves takes the whole pair — never the lone call the raw
    // prompt boundary would have kept (the unsnapped count, 3, keeps `call@1`).
    let mut inside_pair = history();
    super::truncate_conversation_for_prompt(&mut inside_pair, 0);
    assert_eq!(
        kept_discovery(&inside_pair),
        0,
        "the cut moved below the call instead of keeping half a pair: {inside_pair:?}"
    );
    assert_eq!(
        inside_pair.len(),
        1,
        "only the prompt-0 preamble row survives the widened cut: {inside_pair:?}"
    );
}

/// The pair shapes a history cut can meet, each built so that some cut of it splits a
/// pair: keyed-adjacent inside a tool run, keyed with a synthetic `User` row between the
/// halves, TWO interleaved keyed pairs (the multi-pair quadrant a parity-only check
/// cannot read), and the provider-minted `call_id: null` quadrant (no join key at all, so
/// order is the only signal).
fn gate_pair_shapes() -> Vec<(&'static str, Vec<xai_grok_sampling_types::ConversationItem>)> {
    use xai_grok_sampling_types::conversation::tool_search::ToolSearchItem;
    use xai_grok_sampling_types::ToolCall;
    let disc = |raw: serde_json::Value| ConversationItem::Discovery {
        item: ToolSearchItem::from_wire(raw).expect("fixture is a tool_search item"),
    };
    let call = |id: &str, call_id: Option<&str>| {
        disc(serde_json::json!({
            "type": "tool_search_call",
            "id": id,
            "call_id": call_id,
            "status": "completed",
            "execution": "client",
            "arguments": { "query": "crm order management", "limit": 8 }
        }))
    };
    let output = |id: &str, call_id: Option<&str>| {
        disc(serde_json::json!({
            "type": "tool_search_output",
            "id": id,
            "call_id": call_id,
            "status": "completed",
            "execution": "client",
            "tools": [{ "type": "function", "name": "crm_fixture_tool_00" }]
        }))
    };
    vec![
        (
            "keyed/adjacent-in-a-result-run",
            vec![
                ConversationItem::system("sys"),
                ConversationItem::user("find the crm tools"),
                ConversationItem::assistant_tool_calls(vec![ToolCall {
                    id: "call_gate_1".into(),
                    name: "read_file".to_string(),
                    arguments: "{}".into(),
                }]),
                call("tsc_gate_adj", Some("call_gate")),
                output("tso_gate_adj", Some("call_gate")),
                ConversationItem::tool_result("call_gate_1", "hits"),
                ConversationItem::assistant("found them"),
            ],
        ),
        (
            "keyed/user-row-between-the-halves",
            vec![
                ConversationItem::system("sys"),
                ConversationItem::user("find the crm tools"),
                ConversationItem::assistant("one answer"),
                call("tsc_gate_split", Some("call_gate")),
                ConversationItem::user("next prompt"),
                output("tso_gate_split", Some("call_gate")),
                ConversationItem::assistant("found them"),
            ],
        ),
        (
            // Cut review WAJ21R2-08: TWO pairs, interleaved so a cut can retain
            // `call_A` beside `call_B`. A parity-only gate reads that as "2 halves, even"
            // and passes; both halves are in fact unpaired. One pair alone can never
            // produce this reading, which is why the three single-pair shapes above could
            // not see the difference between "kept the pair" and "kept one half".
            "keyed/two-searches-interleaved",
            vec![
                ConversationItem::system("sys"),
                ConversationItem::user("find the crm tools"),
                call("tsc_gate_two_a", Some("call_gate_two_a")),
                call("tsc_gate_two_b", Some("call_gate_two_b")),
                output("tso_gate_two_a", Some("call_gate_two_a")),
                output("tso_gate_two_b", Some("call_gate_two_b")),
                ConversationItem::assistant("found them"),
            ],
        ),
        (
            "keyless/provider-minted-null-call-id",
            vec![
                ConversationItem::system("sys"),
                ConversationItem::user("find the crm tools"),
                call("tsc_gate_keyless", None),
                ConversationItem::user("injected between the halves"),
                output("tso_gate_keyless", None),
                ConversationItem::assistant("found them"),
            ],
        ),
    ]
}

/// A history handed to the model or persisted to disk is pair-atomic: NO discovery item
/// in it is unpaired. One half is the strict-backend 400 (a lone `tool_search_call`) or
/// the provider's loaded-tool-set desync (a lone `tool_search_output`, which A-26 forbids
/// stripping in the first place).
///
/// The predicate is the IR's own `unpaired_discovery_indices`, not a count (cut review
/// WAJ21R2-08 / R2L2-09): `halves % 2 == 0` passes a retained history holding a lone call
/// from pair A beside a lone output from pair B, and it cannot tell "kept the pair" from
/// "dropped the pair" — so a gate built on it stayed green while `clean_fork_prefix_len`
/// was deleting whole pairs. Whole-or-nothing is a SEPARATE claim, pinned by the
/// 0-or-2 count below plus the per-site expectation the gate states at each call.
fn assert_pair_atomic(items: &[ConversationItem], site: &str) {
    let unpaired = xai_grok_sampling_types::conversation::tool_search::unpaired_discovery_indices(items);
    assert!(
        unpaired.is_empty(),
        "{site}: a cut left discovery indices {unpaired:?} unpaired in {items:?}"
    );
    let halves = items
        .iter()
        .filter(|item| item.discovery().is_some())
        .count();
    assert_eq!(
        halves % 2,
        0,
        "{site}: a cut left {halves} discovery half/halves in {items:?}"
    );
}

/// apex-waj.21 review F-4, coordinator adjudication: the fixpoint promise is only
/// worth what the CUT SITES honour, so this gate drives every history cut the shell
/// and its wire make, over every cut index, against all three pair shapes. A site
/// that computes its own count, or truncates on a raw index without the snap, reddens
/// this test (see also [`every_prompt_boundary_cut_routes_through_the_snapping_helper`],
/// which is the inventory half of the same promise).
#[test]
fn every_history_cut_site_keeps_a_discovery_pair_whole() {
    use crate::session::helpers::memory_flush_window::select_flush_window;
    for (shape, history) in gate_pair_shapes() {
        // Fixture sanity (cut review R2L2-09): a gate that only ever checks "not 1 half"
        // is satisfied by a shape that never held a pair, so the shape itself is pinned
        // first — it must carry an ANSWERED pair for the cut to have anything to protect.
        assert!(
            xai_grok_sampling_types::conversation::tool_search::unpaired_discovery_indices(
                &history
            )
            .is_empty(),
            "fixture {shape} holds an unpaired half before any cut is applied"
        );
        assert!(
            history.iter().any(|item| item.discovery().is_some()),
            "fixture {shape} carries no discovery pair at all, so the gate proves nothing"
        );
        for cut in 0..=history.len() {
            // 1. the shell's ONE prompt-boundary cut (rewind, cancel-rewind, both
            //    replay paths, the persisted fork copy).
            let mut via_prompt = history.clone();
            super::truncate_conversation_for_prompt(&mut via_prompt, cut);
            assert_pair_atomic(&via_prompt, &format!("{shape}/truncate_conversation_for_prompt@{cut}"));

            // 2. the raw-index cut (the two checkpoint rewind paths).
            let mut via_index = history.clone();
            super::truncate_conversation_at(&mut via_index, cut);
            assert_pair_atomic(&via_index, &format!("{shape}/truncate_conversation_at@{cut}"));

            // 3. the sampling-types count the chat-state actor's rewind truncates on.
            let keep = xai_grok_sampling_types::conversation::conversation_truncate_for_prompt(
                &history, cut,
            );
            let mut via_count = history.clone();
            via_count.truncate(keep);
            assert_pair_atomic(&via_count, &format!("{shape}/conversation_truncate_for_prompt@{cut}"));

            // 4. the memory-flush model's window (a request, not a history rewrite).
            let window = select_flush_window(history.clone(), cut);
            assert_pair_atomic(
                &window,
                &format!("{shape}/select_flush_window@{cut}"),
            );
        }

        // 5. the persisted fork copy's completeness scan.
        let mut forked = history.clone();
        super::fork_filter_chat(&mut forked);
        assert_pair_atomic(&forked, &format!("{shape}/fork_filter_chat"));
    }
}

/// Index verbs that can split a conversation by position.
const CUT_VERBS: [&str; 3] = [".truncate(", ".split_off(", ".drain("];

/// Receiver names this crate uses for a live `Vec<ConversationItem>`.
///
/// A naming filter is the strongest signal a source walk has — the walk cannot type-check
/// a receiver — so the pair it closes is stated as such: it caught `subagent/mod.rs`'s two
/// raw cuts the moment they were added, and it cannot catch a new cut written under a name
/// invented later. A reviewer wanting more than that needs `cargo clippy` with a custom
/// lint, which this gate deliberately does not require (cut review R2L2-01).
const CONVERSATION_RECEIVERS: [&str; 7] = [
    "conversation",
    "items",
    "history",
    "chat",
    "chat_history",
    "turn_items",
    "records",
];

/// One position-cut found in production source.
#[derive(Debug, PartialEq, Eq)]
struct CutSite {
    file: String,
    line: usize,
    text: String,
}

/// Every position-cut on a conversation-shaped receiver under `root`, production code only.
///
/// Test sources are skipped: a test builds its own history and cutting it is the point.
/// A file's own `#[cfg(test)]` block ends the scan of that file for the same reason.
fn conversation_cut_sites(root: &std::path::Path) -> Vec<CutSite> {
    let mut found = Vec::new();
    let entries: Vec<walkdir::DirEntry> = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|entry: walkdir::Result<walkdir::DirEntry>| entry.ok())
        .collect();
    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(_) => continue,
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        if rel.contains("/tests/") || rel.ends_with("_tests.rs") || rel == "tests.rs" {
            continue;
        }
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        for (number, line) in lines.iter().enumerate() {
            let line = *line;
            if line == "#[cfg(test)]" {
                // An inline test block ends this file's production scan; a `mod x;`
                // declaration (or an attribute on an item) does not — breaking there would
                // blind the walk to the whole file.
                match lines.get(number + 1) {
                    Some(next) if next.starts_with("mod ") && next.ends_with('{') => break,
                    _ => {}
                }
            }
            for verb in CUT_VERBS {
                let Some(at) = line.find(verb) else { continue };
                let receiver = line[..at]
                    .rsplit(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .next()
                    .unwrap_or_default();
                if CONVERSATION_RECEIVERS.contains(&receiver) {
                    found.push(CutSite {
                        file: rel.clone(),
                        line: number + 1,
                        text: line.to_string(),
                    });
                }
            }
        }
    }
    found.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    found
}

/// The complete set of position-cuts a conversation may be given outside the snapping
/// helpers, each with the reason it is safe. An entry that stops matching is as much a
/// failure as an undeclared site: the inventory must describe the tree, not remember it.
const DECLARED_CUT_SITES: &[(&str, &str)] = &[
    // The sanctioned helpers themselves — the `items.truncate` IS the snap's publish.
    ("sampling/conversation.rs", "items.truncate(keep);"),
    ("sampling/conversation.rs", "items.truncate(snapped);"),
    // The fork copy's completeness scan: snapped, then the lone-half pop.
    ("sampling/conversation.rs", "items.truncate(last_complete_end);"),
    // The native fork prefix walk: `clean_fork_prefix_len` rejects an unanswered
    // `tool_search_call` tail (R2L2-02), so the boundary it returns never splits a pair,
    // and the turn window's `split_off` snaps its start first.
    (
        "agent/subagent/mod.rs",
        "items.truncate(clean_fork_prefix_len(&items));",
    ),
    ("agent/subagent/mod.rs", "let mut selected = items.split_off(start);"),
    // The two cross-compaction preamble trims: what they keep is the session preamble
    // (`System`, then the `User(user_info)` row) and the remainder is replaced wholesale
    // by the replayed conversation, so no pair can straddle them.
    (
        "session/acp_session_impl/rewind.rs",
        "conversation.truncate(1); // keep System only",
    ),
    (
        "session/acp_session_impl/rewind.rs",
        "conversation.truncate(2); // keep System + current user_info",
    ),
    // `records` here are `LineRecord`s of `updates.jsonl`, not conversation rows; the cut
    // count comes from `truncate_for_prompt_by`, i.e. the same prompt-boundary rule. The
    // chat cache derived from that stream can carry no discovery row (WAJ21R2-01), and the
    // rebuild fails closed if one would be lost.
    ("session/storage/jsonl/copy.rs", "records.truncate(keep);"),
    // Not conversations at all: goal-event ring and workflow-event ring.
    ("session/goal_tracker.rs", "o.history.drain(0..overflow);"),
    ("session/workflow/tracker.rs", "self.history.drain(..excess);"),
];

/// The inventory half of the F-4 promise, made mechanical (cut review R2L2-01).
///
/// The earlier version of this gate walked a hand-written list of four files, so a raw cut
/// anywhere else was invisible by construction — and one already existed in this very
/// crate (`agent/subagent/mod.rs`). This walks `src/` instead: every position-cut on a
/// conversation-shaped receiver must appear in [`DECLARED_CUT_SITES`], so a new cut site
/// reddens this test wherever it is written, and a declared site that goes away reddens it
/// too. `session/storage/mod.rs` is inside the walk; it holds no conversation cut, and its
/// A-26 exposure (the derived-cache rebuild) is pinned in
/// `crate::session::storage`'s own tests instead.
#[test]
fn every_conversation_cut_is_declared_in_one_mechanical_inventory() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let sites = conversation_cut_sites(&root);
    let mut undeclared: Vec<&CutSite> = Vec::new();
    let mut used = vec![false; DECLARED_CUT_SITES.len()];
    for site in &sites {
        let matched = DECLARED_CUT_SITES.iter().enumerate().find(|(_, (file, text))| {
            *file == site.file && site.text.contains(text)
        });
        match matched {
            Some((index, _)) => used[index] = true,
            None => undeclared.push(site),
        }
    }
    assert!(
        undeclared.is_empty(),
        "undeclared conversation cut site(s): {undeclared:?}\n\
         a cut on a Vec<ConversationItem> must either call truncate_conversation_for_prompt \
         / truncate_conversation_at / snap_index_over_discovery_pairs, or be added to \
         DECLARED_CUT_SITES in this file WITH the reason it is pair-safe (apex-waj.21 F-4, \
         R2L2-01)."
    );
    let stale: Vec<&(&str, &str)> = used
        .iter()
        .zip(DECLARED_CUT_SITES.iter())
        .filter(|(used, _)| !**used)
        .map(|(_, entry)| entry)
        .collect();
    assert!(
        stale.is_empty(),
        "DECLARED_CUT_SITES names site(s) that no longer exist: {stale:?} — the inventory \
         must describe the tree"
    );
    assert_eq!(
        sites.len(),
        DECLARED_CUT_SITES.len(),
        "the walk and the inventory disagree on the number of cut sites"
    );
}

/// The routing half of the same promise, kept as a text pin so a mutant that swaps a
/// snapping call back to a raw `truncate` reddens something even where the walk's receiver
/// filter would have to be widened.
#[test]
fn every_prompt_boundary_cut_routes_through_the_snapping_helper() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let sites = [
        "session/acp_session_impl/rewind.rs",
        "session/acp_session_impl/cancel.rs",
        "session/helpers/replay.rs",
        "session/storage/jsonl/copy.rs",
        // R2L2-02: the native-fork prefix decides a conversation boundary too, off its own
        // completeness scan rather than the prompt count.
        "agent/subagent/mod.rs",
    ];
    for site in sites {
        let text = std::fs::read_to_string(root.join(site))
            .unwrap_or_else(|e| panic!("{site} must be readable by the cut-site gate: {e}"));
        assert!(
            text.contains("truncate_conversation_for_prompt(")
                || text.contains("truncate_conversation_at(")
                || text.contains("snap_index_over_discovery_pairs("),
            "{site} cuts a conversation and must route through a snapping helper \
             (`truncate_conversation_for_prompt`, `truncate_conversation_at`, or \
             `snap_index_over_discovery_pairs` when the cut index is computed locally); a raw \
             `Vec::truncate` on a prompt index can split a tool_search pair (apex-waj.21 F-4)"
        );
    }
    // The native fork computes its own turn window, so it must snap the window start itself
    // (R2L2-02) — the helper-based files above get the snap for free from their entry point.
    let fork = std::fs::read_to_string(root.join("agent/subagent/mod.rs")).expect("subagent/mod.rs");
    assert_eq!(
        fork.matches("snap_index_over_discovery_pairs(").count(),
        1,
        "the native-fork turn window must snap its start exactly once"
    );
    // The checkpoint paths compute a raw index, so they must use the raw-index sibling.
    let replay =
        std::fs::read_to_string(root.join("session/helpers/replay.rs")).expect("replay.rs");
    assert_eq!(
        replay.matches("truncate_conversation_at(").count(),
        2,
        "both checkpoint rewind paths must snap their raw cut index"
    );
    assert_eq!(
        replay.matches("conversation.truncate(").count(),
        0,
        "no rewind path may truncate a conversation on a raw index"
    );
    // The two preamble trims are the allow-listed exception; pinning the count means a
    // third raw truncate added here has to be argued about in this test.
    let rewind = std::fs::read_to_string(root.join("session/acp_session_impl/rewind.rs"))
        .expect("rewind.rs");
    assert_eq!(
        rewind.matches("conversation.truncate(").count(),
        2,
        "only the cross-compaction preamble trims may cut on a raw index, and only because \
         what they keep is the System + User(user_info) preamble"
    );
}
