use super::{
    CODEX_CONTEXT_WINDOW_TRUNCATED_OUTPUT_MESSAGE, codex_compaction_auth_refresh_allowed,
    codex_remote_compaction_v2_excludes_two_pass_prefire,
    rewrite_codex_tool_outputs_to_fit_context_window,
};
use xai_grok_sampling_types::{ApiBackend, ConversationItem};

/// Provenance: open-grok@240c99c9 crates/codegen/xai-grok-shell/src/session/compaction.rs:258 :: codex_compaction_auth_refresh_never_escapes_attempt_budget (verbatim)
#[test]
fn codex_compaction_auth_refresh_never_escapes_attempt_budget() {
    assert!(codex_compaction_auth_refresh_allowed(1, 3, false));
    assert!(codex_compaction_auth_refresh_allowed(2, 3, false));
    assert!(!codex_compaction_auth_refresh_allowed(3, 3, false));
    assert!(!codex_compaction_auth_refresh_allowed(1, 3, true));
}

/// Provenance: open-grok@240c99c9 crates/codegen/xai-grok-shell/src/session/compaction.rs:266 :: codex_remote_preflight_rewrites_only_trailing_tool_outputs (verbatim; the worktree `ToolResult` has no `ordered_content` field and the worktree `ConversationItem` has no `CustomToolOutput` variant — the OG test asserts neither, so nothing is dropped)
#[test]
fn codex_remote_preflight_rewrites_only_trailing_tool_outputs() {
    let original_user = "u".repeat(400);
    let mut items = vec![
        ConversationItem::user(original_user.clone()),
        ConversationItem::tool_result("call_1", "x".repeat(4_000)),
    ];
    let budget = xai_chat_state::estimate_conversation_tokens(&items) / 2;

    assert_eq!(
        rewrite_codex_tool_outputs_to_fit_context_window(&mut items, budget),
        1
    );
    assert_eq!(items.len(), 2, "preflight must never drop history items");
    assert_eq!(items[0].text_content(), original_user);
    assert_eq!(
        items[1].text_content(),
        CODEX_CONTEXT_WINDOW_TRUNCATED_OUTPUT_MESSAGE
    );

    let mut non_trailing = vec![
        ConversationItem::tool_result("call_2", "x".repeat(4_000)),
        ConversationItem::assistant("ordinary tail"),
    ];
    assert_eq!(
        rewrite_codex_tool_outputs_to_fit_context_window(&mut non_trailing, 1),
        0,
        "codex-rs stops rather than deleting an ordinary tail item"
    );
}

/// Fresh-written (flagged in the P2.1 report): the coordinator test-port audit found no
/// session-level v2 E2E in either source repo, so the session-flow contract is pinned at
/// predicate level instead. Full session-flow acceptance is the coordinator's live run.
#[test]
fn codex_remote_compaction_v2_excludes_two_pass_prefire_only_for_enabled_codex_responses() {
    assert!(
        codex_remote_compaction_v2_excludes_two_pass_prefire(
            Some("codex"),
            ApiBackend::Responses,
            true
        ),
        "codex + responses + v2 on: local prefire is redundant"
    );
    assert!(
        !codex_remote_compaction_v2_excludes_two_pass_prefire(
            Some("codex"),
            ApiBackend::Responses,
            false
        ),
        "flag off falls back to local compaction, whose prefire stays valid"
    );
    assert!(
        !codex_remote_compaction_v2_excludes_two_pass_prefire(
            Some("codex"),
            ApiBackend::Messages,
            true
        ),
        "a non-responses backend is not a v2 path"
    );
    assert!(
        !codex_remote_compaction_v2_excludes_two_pass_prefire(
            Some("grok"),
            ApiBackend::Responses,
            true
        ),
        "a non-codex family is not a v2 path"
    );
    assert!(
        !codex_remote_compaction_v2_excludes_two_pass_prefire(None, ApiBackend::Responses, true),
        "an unlisted model family is not a v2 path"
    );
}
