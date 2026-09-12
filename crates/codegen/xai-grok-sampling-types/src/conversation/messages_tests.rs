use super::test_support::*;
use super::*;

use super::messages::{
    clean_orphaned_blocks_by_adjacency, clean_orphaned_items, hoist_tool_results_to_front,
    repair_trailing_assistant,
};

fn messages_test_request(reasoning_effort: Option<crate::ReasoningEffort>) -> ConversationRequest {
    ConversationRequest {
        items: vec![ConversationItem::user("Hello")],
        model: Some("test-model".to_string()),
        reasoning_effort,
        ..Default::default()
    }
}

#[test]
fn json_schema_and_reasoning_effort_are_orthogonal_in_output_config() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": { "x": { "type": "string" } },
        "required": ["x"]
    });
    let mut req = ConversationRequest::from_items(vec![ConversationItem::user("go")])
        .with_json_schema(schema);
    req.reasoning_effort = Some(crate::ReasoningEffort::High);

    let msgs = build_messages_request(&req);
    let oc = msgs.output_config.expect("output_config present");
    assert_eq!(oc.effort.as_deref(), Some("high"));
    assert!(oc.format.is_some());
    assert!(
        msgs.thinking.is_some(),
        "thinking set when effort is present"
    );
}

#[test]
fn test_messages_request_wire_format_for_supported_variants() {
    for (variant, expected) in [
        (crate::ReasoningEffort::Low, "low"),
        (crate::ReasoningEffort::Medium, "medium"),
        (crate::ReasoningEffort::High, "high"),
        (crate::ReasoningEffort::Xhigh, "xhigh"),
        (crate::ReasoningEffort::Max, "max"),
    ] {
        let req = messages_test_request(Some(variant));
        let msgs = build_messages_request(&req);
        let json = serde_json::to_value(&msgs).unwrap();
        assert_eq!(
            json.pointer("/output_config/effort")
                .and_then(|v| v.as_str()),
            Some(expected),
            "{variant:?} should map to output_config.effort={expected:?}; got: {json:#}",
        );
        assert_eq!(
            json.pointer("/thinking/type").and_then(|v| v.as_str()),
            Some("adaptive"),
            "{variant:?} should auto-pair thinking.type=adaptive; got: {json:#}",
        );
    }
}

#[test]
fn test_messages_request_omits_output_config_when_no_supported_effort() {
    let none_or_unsupported = [
        None,
        Some(crate::ReasoningEffort::None),
        Some(crate::ReasoningEffort::Minimal),
    ];
    for input in none_or_unsupported {
        let req = messages_test_request(input);
        let msgs = build_messages_request(&req);
        assert!(
            msgs.output_config.is_none(),
            "input {input:?} must not produce output_config",
        );
        assert!(
            msgs.thinking.is_none(),
            "input {input:?} must not auto-pair thinking",
        );
    }
}

#[test]
fn test_messages_request_thinking_carries_summarized_display() {
    let req = ConversationRequest {
        reasoning_effort: Some(crate::ReasoningEffort::High),
        ..ConversationRequest::from_items(vec![ConversationItem::user("hi")])
            .with_model("messages-compatible-model")
    };
    let msg = build_messages_request(&req);
    let json = serde_json::to_value(&msg).unwrap();
    assert_eq!(
        json.pointer("/thinking/type").and_then(|v| v.as_str()),
        Some("adaptive"),
        "thinking.type should be 'adaptive'; got: {json:#}",
    );
    assert_eq!(
        json.pointer("/thinking/display").and_then(|v| v.as_str()),
        Some("summarized"),
        "thinking.display must be 'summarized' so 4.7+ surfaces thinking content; got: {json:#}",
    );
}

#[test]
fn test_messages_request_omits_thinking_when_effort_unset() {
    let req = ConversationRequest::from_items(vec![ConversationItem::user("hi")])
        .with_model("messages-compatible-model");
    let msg = build_messages_request(&req);
    let json = serde_json::to_value(&msg).unwrap();
    assert!(
        json.get("thinking").is_none()
            || json
                .pointer("/thinking")
                .map(|v| v.is_null())
                .unwrap_or(false),
        "thinking must be absent when reasoning_effort is unset; got: {json:#}",
    );
    assert!(
        json.get("output_config").is_none()
            || json
                .pointer("/output_config")
                .map(|v| v.is_null())
                .unwrap_or(false),
        "output_config must be absent when reasoning_effort is unset; got: {json:#}",
    );
}

#[test]
fn test_messages_request_previous_tip_skips_a_trailing_user_run() {
    let mut items = vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user("Fix the bug"),
    ];
    items.extend(agent_turn(0));
    items.extend(agent_turn(1));
    // The shape after a parallel batch: tool results, then followups.
    items.push(ConversationItem::user("[Image content]"));
    items.push(ConversationItem::user("<system-reminder>"));

    let json = serde_json::to_value(build_messages_request(
        &ConversationRequest::from_items(items).with_model("messages-compatible-model"),
    ))
    .unwrap();
    let messages = json["messages"].as_array().unwrap();

    let marked: Vec<usize> = (0..messages.len())
        .filter(|&i| marker_on_last_block(&messages[i]).is_some())
        .collect();
    let last_assistant = messages
        .iter()
        .rposition(|m| m["role"] == "assistant")
        .unwrap();
    assert_eq!(marked.len(), 2, "tip and previous tip only: {json:#}");
    assert_eq!(marked[1], messages.len() - 1, "tip: {json:#}");
    assert!(
        marked[0] < last_assistant,
        "the previous tip must sit before the last assistant turn, not inside \
             the trailing user run; got {marked:?} in {json:#}",
    );
}

#[test]
fn test_messages_request_cache_breakpoint_marks_an_image_tip() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::User(UserItem {
            content: vec![
                ContentPart::Text {
                    text: "what is in this screenshot".into(),
                },
                ContentPart::Image {
                    url: "data:image/png;base64,iVBOR".into(),
                },
            ],
            ..Default::default()
        }),
    ])
    .with_model("messages-compatible-model");

    let json = serde_json::to_value(build_messages_request(&req)).unwrap();
    let blocks = json["messages"][0]["content"].as_array().unwrap();

    assert_eq!(blocks.last().unwrap()["type"].as_str(), Some("image"));
    assert_eq!(
        marker_on_last_block(&json["messages"][0]),
        Some("ephemeral"),
        "{json:#}",
    );
    assert!(blocks[0].get("cache_control").is_none(), "{json:#}");
}

#[test]
fn test_messages_request_cache_breakpoint_skips_thinking() {
    // The reasoning item carries a signature: an unsigned thinking block would
    // be stripped from the latest assistant by the MW-1 three-part replay rule
    // before the cache window runs, and the skip would have nothing to act on.
    let mut reasoning = synthesized_reasoning_item("weighing options");
    reasoning.encrypted_content = Some("test_signature".to_string());
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("Fix the bug"),
        ConversationItem::Reasoning(reasoning),
        ConversationItem::assistant("Fixed it."),
    ])
    .with_model("messages-compatible-model");

    let json = serde_json::to_value(build_messages_request(&req)).unwrap();
    let blocks = json["messages"][1]["content"].as_array().unwrap();

    let thinking = blocks
        .iter()
        .find(|b| b["type"] == "thinking")
        .expect("reasoning should emit a thinking block");
    assert!(thinking.get("cache_control").is_none(), "{json:#}");
    // The trailing-assistant repair appends a synthetic user sentinel after
    // this assistant turn, so the cache-breakpoint tip lands on the sentinel
    // (the R2 interaction, pinned by
    // history_cache_control_lands_on_synthetic_user_after_trailing_assistant);
    // the thinking-bearing assistant keeps no marker at all.
    for block in blocks {
        assert!(block.get("cache_control").is_none(), "{json:#}");
    }
    assert_eq!(
        marker_on_last_block(&json["messages"][2]),
        Some("ephemeral"),
        "{json:#}",
    );
}

#[test]
fn test_btw_cross_api_messages_no_regressions() {
    let items = btw_prepare_items(btw_mid_turn_conversation());
    let req = ConversationRequest::from_items(items);
    let msg = build_messages_request(&req);
    let json = serde_json::to_value(&msg).unwrap();

    let messages = json.get("messages").unwrap().as_array().unwrap();

    // No thinking blocks anywhere.
    for (i, m) in messages.iter().enumerate() {
        if let Some(content) = m.get("content").and_then(|c| c.as_array()) {
            for block in content {
                assert_ne!(
                    block.get("type").and_then(|t| t.as_str()),
                    Some("thinking"),
                    "messages[{i}] must not contain thinking blocks",
                );
            }
        }
    }

    // Last assistant message must not have unanswered tool_use.
    let last_assistant = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("assistant"))
        .expect("should have an assistant message");
    if let Some(content) = last_assistant.get("content").and_then(|c| c.as_array()) {
        for block in content {
            assert_ne!(
                block.get("type").and_then(|t| t.as_str()),
                Some("tool_use"),
                "last assistant in btw request must not have unanswered tool_use",
            );
        }
    }

    // Top-level thinking must be absent (no reasoning_effort set).
    assert!(
        json.get("thinking").is_none() || json.pointer("/thinking").is_some_and(|v| v.is_null()),
        "top-level thinking must be absent; got: {json:#}",
    );

    assert!(
        json.get("temperature").is_none()
            || json.pointer("/temperature").is_some_and(|v| v.is_null()),
        "temperature must be absent so proxy defaults can apply; got: {json:#}",
    );

    // The completed tool pair (call_1) must survive.
    let has_tool_use_call_1 = messages.iter().any(|m| {
        m.get("content")
            .and_then(|c| c.as_array())
            .is_some_and(|blocks| {
                blocks.iter().any(|b| {
                    b.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                        && b.get("id").and_then(|id| id.as_str()) == Some("call_1")
                })
            })
    });
    assert!(
        has_tool_use_call_1,
        "completed tool_use call_1 must survive"
    );

    let has_tool_result_call_1 = messages.iter().any(|m| {
        m.get("content")
            .and_then(|c| c.as_array())
            .is_some_and(|blocks| {
                blocks.iter().any(|b| {
                    b.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                        && b.get("tool_use_id").and_then(|id| id.as_str()) == Some("call_1")
                })
            })
    });
    assert!(
        has_tool_result_call_1,
        "completed tool_result for call_1 must survive"
    );
}

#[test]
fn test_tool_result_with_images_to_anthropic() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("Read this"),
        ConversationItem::Assistant(AssistantItem {
            content: String::new().into(),
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: "read_file".to_string(),
                arguments: "{}".into(),
            }],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        ConversationItem::tool_result_with_images(
            "call_1",
            "Read image file: photo.png",
            vec![ContentPart::Image {
                url: "data:image/png;base64,iVBOR".into(),
            }],
        ),
    ]);

    let messages_req = build_messages_request(&req);

    // Find the user message that contains the tool result (the Messages API wraps tool results in user messages)
    let tool_result_msg = messages_req
        .messages
        .iter()
        .find(|m| {
            if let crate::messages::MessageContent::Blocks(blocks) = &m.content {
                blocks
                    .iter()
                    .any(|b| matches!(b, crate::messages::ContentBlock::ToolResult { .. }))
            } else {
                false
            }
        })
        .expect("Expected a message with ToolResult block");

    let crate::messages::MessageContent::Blocks(blocks) = &tool_result_msg.content else {
        panic!("Expected Blocks");
    };
    let tool_result_block = blocks
        .iter()
        .find_map(|b| {
            if let crate::messages::ContentBlock::ToolResult { content, .. } = b {
                Some(content)
            } else {
                None
            }
        })
        .unwrap();

    let crate::messages::ToolResultContent::Blocks(inner) = tool_result_block else {
        panic!("Expected ToolResultContent::Blocks, got Text");
    };
    assert_eq!(inner.len(), 2);
    assert!(
        matches!(&inner[0], crate::messages::ContentBlock::Text { text, .. } if text == "Read image file: photo.png")
    );
    assert!(
        matches!(&inner[1], crate::messages::ContentBlock::Image { source: crate::messages::ImageSource::Base64 { media_type, data }, .. } if media_type == "image/png" && data == "iVBOR")
    );
}

#[test]
fn upgrade_legacy_reasoning_singular_anthropic_no_id() {
    // Messages streaming sets id = "" (see stream/messages.rs:340).
    // The upgrader must still emit a sibling carrying text and signature
    let raw = serde_json::json!({
        "type": "assistant",
        "content": "answer",
        "reasoning": {
            "text": "Let me think about this...",
            "encrypted": "signature-bytes-here",
            "id": ""
        },
        "model_id": "messages-compatible-model"
    });
    let mut seen = std::collections::HashSet::new();
    let siblings = upgrade_legacy_reasoning(&raw, &mut seen);
    assert_eq!(siblings.len(), 1);
    let ConversationItem::Reasoning(r) = &siblings[0] else {
        panic!("expected Reasoning sibling");
    };
    assert_eq!(r.id, "");
    assert_eq!(r.encrypted_content.as_deref(), Some("signature-bytes-here"));
}

// ============================================================================
// D6 golden — fixture F (spec MW-1 D6)
//
// F = user -> assistant+tool_call -> tool_result -> user; model=claude-sonnet-5,
// reasoning_effort=None, max_output_tokens SET, no Reasoning items, no orphaned
// pairs, not ending on assistant. The checked-in golden is the pre-MW-1
// serialization; it must hold after every MW-1 stage lands.
// ============================================================================

fn fixture_f() -> ConversationRequest {
    ConversationRequest {
        items: vec![
            ConversationItem::user("user turn one"),
            ConversationItem::Assistant(AssistantItem {
                content: "assistant reply one".into(),
                tool_calls: vec![ToolCall {
                    id: "call_f1".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"x"}"#.into(),
                }],
                model_id: None,
                model_fingerprint: None,
                reasoning_effort: None,
            }),
            ConversationItem::tool_result("call_f1", "result one"),
            ConversationItem::user("user turn two"),
        ],
        model: Some("claude-sonnet-5".to_string()),
        reasoning_effort: None,
        max_output_tokens: Some(4096),
        ..Default::default()
    }
}

/// The S-021 adjacency invariant (A1 property): every tool_use block's id has a
/// matching tool_result in the immediately following user message, and vice
/// versa.
fn check_adjacency_invariant(messages: &[crate::messages::Message]) -> Result<(), String> {
    use crate::messages::{ContentBlock, MessageContent, MessageRole};

    fn block_ids<'a>(
        msg: &'a crate::messages::Message,
        pick: impl Fn(&'a ContentBlock) -> Option<&'a str>,
    ) -> Vec<&'a str> {
        match &msg.content {
            MessageContent::Blocks(blocks) => blocks.iter().filter_map(pick).collect(),
            MessageContent::Text(_) => Vec::new(),
        }
    }
    for (i, msg) in messages.iter().enumerate() {
        if matches!(msg.role, MessageRole::Assistant) {
            let use_ids = block_ids(msg, |b| match b {
                ContentBlock::ToolUse { id, .. } => Some(id),
                _ => None,
            });
            if use_ids.is_empty() {
                continue;
            }
            let Some(next) = messages.get(i + 1) else {
                return Err(format!(
                    "messages[{i}]: tool_use must be followed by a message"
                ));
            };
            if !matches!(next.role, MessageRole::User) {
                return Err(format!(
                    "messages[{i}]: tool_use must be followed by a user message, got {next:?}"
                ));
            }
            let result_ids = block_ids(next, |b| match b {
                ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id),
                _ => None,
            });
            for id in use_ids {
                if !result_ids.contains(&id) {
                    return Err(format!(
                        "messages[{i}]: tool_use {id} has no matching tool_result in messages[{}]",
                        i + 1
                    ));
                }
            }
        }
        if matches!(msg.role, MessageRole::User) {
            let result_ids = block_ids(msg, |b| match b {
                ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id),
                _ => None,
            });
            for id in result_ids {
                let Some(prev) = messages.get(i - 1) else {
                    return Err(format!(
                        "messages[{i}]: tool_result {id} must be preceded by a message"
                    ));
                };
                if !matches!(prev.role, MessageRole::Assistant) {
                    return Err(format!(
                        "messages[{i}]: tool_result {id} must follow an assistant message, got {prev:?}"
                    ));
                }
                let use_ids = block_ids(prev, |b| match b {
                    ContentBlock::ToolUse { id, .. } => Some(id),
                    _ => None,
                });
                if !use_ids.contains(&id) {
                    return Err(format!(
                        "messages[{i}]: tool_result {id} has no matching tool_use in messages[{}]",
                        i - 1
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Asserting wrapper for the A1 adjacency invariant.
fn assert_adjacency_invariant(messages: &[crate::messages::Message]) {
    check_adjacency_invariant(messages).expect("adjacency invariant violated");
}

#[test]
fn d6_golden_f_serialization_holds() {
    let json = serde_json::to_string(&build_messages_request(&fixture_f())).unwrap();
    let golden = include_str!("../../testdata/messages_golden_f.json");
    assert_eq!(
        json, golden,
        "D6 golden F drifted. Re-baselining is allowed only when the D4 table lands a          claude-sonnet-5 row that must reproduce these exact bytes (spec D6)."
    );
}

#[test]
fn d6_golden_f_invariants() {
    let req = fixture_f();
    // F's invariants (spec D6)
    assert_eq!(req.model.as_deref(), Some("claude-sonnet-5"));
    assert!(req.reasoning_effort.is_none(), "F has no reasoning effort");
    assert_eq!(
        req.max_output_tokens,
        Some(4096),
        "F has max_output_tokens SET"
    );
    assert!(
        !req.items
            .iter()
            .any(|i| matches!(i, ConversationItem::Reasoning(_))),
        "F has no Reasoning items"
    );
    let msgs = build_messages_request(&req);
    assert_eq!(
        msgs.max_tokens, 4096,
        "max_tokens carries the request value"
    );
    assert!(
        !msgs.messages.is_empty()
            && matches!(
                msgs.messages.last().unwrap().role,
                crate::messages::MessageRole::User
            ),
        "F does not end on assistant (no repair expected)"
    );
    assert_adjacency_invariant(&msgs.messages);
    for (i, msg) in msgs.messages.iter().enumerate() {
        if let crate::messages::MessageContent::Blocks(blocks) = &msg.content {
            for block in blocks {
                assert!(
                    !matches!(
                        block,
                        crate::messages::ContentBlock::Thinking { .. }
                            | crate::messages::ContentBlock::RedactedThinking { .. }
                    ),
                    "messages[{i}]: F must carry no thinking blocks"
                );
            }
        }
    }
}

// ============================================================================
// MW-1 stage helpers (typed message constructors for unit tests)
// ============================================================================

fn m_user(blocks: Vec<crate::messages::ContentBlock>) -> crate::messages::Message {
    crate::messages::Message {
        role: crate::messages::MessageRole::User,
        content: crate::messages::MessageContent::Blocks(blocks),
    }
}

fn m_assistant(blocks: Vec<crate::messages::ContentBlock>) -> crate::messages::Message {
    crate::messages::Message {
        role: crate::messages::MessageRole::Assistant,
        content: crate::messages::MessageContent::Blocks(blocks),
    }
}

fn m_text(text: &str) -> crate::messages::ContentBlock {
    crate::messages::ContentBlock::Text {
        text: text.to_string(),
        cache_control: None,
    }
}

fn m_tool_use(id: &str) -> crate::messages::ContentBlock {
    crate::messages::ContentBlock::ToolUse {
        id: id.to_string(),
        name: "shell".to_string(),
        input: serde_json::json!({}),
        cache_control: None,
    }
}

fn m_tool_result(id: &str) -> crate::messages::ContentBlock {
    crate::messages::ContentBlock::ToolResult {
        tool_use_id: id.to_string(),
        content: crate::messages::ToolResultContent::Text("out".to_string()),
        cache_control: None,
    }
}

fn mk_call(id: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "shell".to_string(),
        arguments: r#"{"cmd":"ls"}"#.into(),
    }
}

fn blocks_of(msg: &crate::messages::Message) -> &[crate::messages::ContentBlock] {
    match &msg.content {
        crate::messages::MessageContent::Blocks(blocks) => blocks,
        _ => panic!("expected block content: {msg:?}"),
    }
}

fn sentinel_text(msg: &crate::messages::Message) -> &str {
    let blocks = blocks_of(msg);
    assert_eq!(
        blocks.len(),
        1,
        "the sentinel is a single text block: {blocks:?}"
    );
    let crate::messages::ContentBlock::Text { text, .. } = &blocks[0] else {
        panic!("sentinel must be a text block: {blocks:?}");
    };
    text
}

// ============================================================================
// (a) Orphan cleanup — S-005 (item level) + S-021 (message level)
// ============================================================================

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: orphan_cleanup_empty_input (adapted)
#[test]
fn orphan_cleanup_empty_input() {
    assert!(
        clean_orphaned_items(&[]).is_empty(),
        "empty input should produce empty output"
    );
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: orphan_cleanup_paired_preserved (adapted)
#[test]
fn orphan_cleanup_paired_preserved() {
    let items = vec![
        ConversationItem::user("run ls"),
        ConversationItem::assistant_tool_calls(vec![mk_call("toolu_paired")]),
        ConversationItem::tool_result("toolu_paired", "file.txt"),
    ];
    let cleaned = clean_orphaned_items(&items);
    assert_eq!(
        cleaned.len(),
        3,
        "all 3 items (user + paired call + result) should be preserved"
    );
    let ConversationItem::Assistant(a) = &cleaned[1] else {
        panic!("middle item must stay the assistant");
    };
    assert_eq!(a.tool_calls.len(), 1, "paired tool call survives");
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: orphan_cleanup_orphaned_function_call_removed (adapted)
#[test]
fn orphan_cleanup_orphaned_function_call_removed() {
    let items = vec![
        ConversationItem::user("run ls"),
        ConversationItem::assistant_tool_calls(vec![mk_call("toolu_orphan")]),
        // No matching ToolResult
    ];
    let cleaned = clean_orphaned_items(&items);
    assert_eq!(
        cleaned.len(),
        1,
        "orphaned tool call is removed; the call-only assistant item carries nothing wire-visible"
    );
    assert!(matches!(cleaned[0], ConversationItem::User(_)));
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: orphan_cleanup_orphaned_local_shell_call_removed (re-derived)
/// Re-derived for grok: the port source's LocalShellCall kind has no grok
/// analogue (grok's BackendToolCall maps to text, never tool_use), so the row
/// becomes "Assistant.tool_call with no ToolResult item" inside a longer
/// history: the orphaned assistant drops, the neighbours survive.
#[test]
fn orphan_cleanup_orphaned_tool_call_removed_among_survivors() {
    let items = vec![
        ConversationItem::user("run ls"),
        ConversationItem::assistant_tool_calls(vec![mk_call("shell_orphan")]),
        ConversationItem::assistant("done"),
    ];
    let cleaned = clean_orphaned_items(&items);
    assert_eq!(cleaned.len(), 2, "only the orphaned-call assistant drops");
    assert!(matches!(cleaned[0], ConversationItem::User(_)));
    let ConversationItem::Assistant(a) = &cleaned[1] else {
        panic!("the text assistant must survive");
    };
    assert_eq!(a.content.as_ref(), "done");
    assert!(a.tool_calls.is_empty());
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: orphan_cleanup_mixed_only_orphan_removed (adapted)
#[test]
fn orphan_cleanup_mixed_only_orphan_removed() {
    let items = vec![
        // Paired call + orphaned call in one assistant turn
        ConversationItem::assistant_tool_calls(vec![mk_call("toolu_good"), mk_call("toolu_bad")]),
        // Paired output
        ConversationItem::tool_result("toolu_good", "ok"),
        // Orphaned output (no call)
        ConversationItem::tool_result("toolu_stray", "stray"),
    ];
    let cleaned = clean_orphaned_items(&items);
    // Keep: assistant (only toolu_good), tool_result(toolu_good).
    // Drop: toolu_bad call, tool_result(toolu_stray).
    assert_eq!(cleaned.len(), 2, "only paired items should remain");
    let ConversationItem::Assistant(a) = &cleaned[0] else {
        panic!("first survivor must be the assistant");
    };
    assert_eq!(a.tool_calls.len(), 1);
    assert_eq!(a.tool_calls[0].id.as_ref(), "toolu_good");
    let ConversationItem::ToolResult(t) = &cleaned[1] else {
        panic!("second survivor must be the paired result");
    };
    assert_eq!(t.tool_call_id, "toolu_good");
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: orphan_cleanup_orphaned_output_removed (re-derived)
/// Re-derived for grok: "ToolResult whose id matches no tool_call" (the port
/// source row had no leading message to anchor on).
#[test]
fn orphan_cleanup_orphaned_output_removed() {
    let items = vec![
        ConversationItem::user("hello"),
        ConversationItem::tool_result("toolu_no_call", "output"),
    ];
    let cleaned = clean_orphaned_items(&items);
    assert_eq!(
        cleaned.len(),
        1,
        "orphaned ToolResult (id matches no tool_call) is removed"
    );
    assert!(matches!(cleaned[0], ConversationItem::User(_)));
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: orphan_cleanup_integration_with_translation (adapted)
#[test]
fn orphan_cleanup_integration_with_translation() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("do it"),
        // Orphaned call — stripped before translation
        ConversationItem::assistant_tool_calls(vec![mk_call("toolu_gone")]),
    ]);
    let msgs = build_messages_request(&req);
    assert_eq!(
        msgs.messages.len(),
        1,
        "after orphan removal only the user message survives"
    );
    assert!(matches!(
        msgs.messages[0].role,
        crate::messages::MessageRole::User
    ));
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: s021_adjacent_pair_preserved (adapted)
#[test]
fn s021_adjacent_pair_preserved() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("do it"),
        ConversationItem::assistant_tool_calls(vec![mk_call("tc1")]),
        ConversationItem::tool_result("tc1", "files"),
    ]);
    let msgs = build_messages_request(&req);
    let asst_idx = msgs
        .messages
        .iter()
        .position(|m| {
            matches!(
                &m.content,
                crate::messages::MessageContent::Blocks(b)
                    if b.iter().any(|x| matches!(x, crate::messages::ContentBlock::ToolUse { .. }))
            )
        })
        .expect("must have an assistant message with tool_use");
    let next = &msgs.messages[asst_idx + 1];
    assert!(matches!(next.role, crate::messages::MessageRole::User));
    let crate::messages::MessageContent::Blocks(blocks) = &next.content else {
        panic!("tool result message must carry blocks");
    };
    assert!(
        blocks
            .iter()
            .any(|b| matches!(b, crate::messages::ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "tc1")),
        "adjacent tool_result tc1 must survive"
    );
    assert_adjacency_invariant(&msgs.messages);
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: s021_non_adjacent_tool_use_stripped (adapted)
#[test]
fn s021_non_adjacent_tool_use_stripped() {
    // tool_use in assistant[1]; a user text message intervenes before the
    // tool_result, so neither side is adjacent — both are stripped and the
    // emptied messages removed.
    let mut messages = vec![
        m_user(vec![m_text("go")]),
        m_assistant(vec![m_tool_use("tc1")]),
        m_user(vec![m_text("ack")]),
        m_assistant(vec![m_text("done")]),
        m_user(vec![m_tool_result("tc1")]),
    ];
    clean_orphaned_blocks_by_adjacency(&mut messages);
    // tool_use tc1 is non-adjacent (a user text message intervenes) and the
    // tool_result has no adjacent preceding assistant tool_use: both blocks
    // are stripped, both emptied messages removed — user("go"), user("ack")
    // and the text assistant survive.
    assert_eq!(
        messages.len(),
        3,
        "the two emptied messages are removed: {messages:?}"
    );
    assert!(matches!(
        messages[0].role,
        crate::messages::MessageRole::User
    ));
    assert!(matches!(
        messages[1].role,
        crate::messages::MessageRole::User
    ));
    let crate::messages::MessageContent::Blocks(blocks) = &messages[2].content else {
        panic!();
    };
    assert!(
        blocks.iter().any(
            |b| matches!(b, crate::messages::ContentBlock::Text { text, .. } if text == "done")
        ),
        "the intervening text assistant survives: {blocks:?}"
    );
    for (i, m) in messages.iter().enumerate() {
        if let crate::messages::MessageContent::Blocks(b) = &m.content {
            for block in b {
                assert!(
                    !matches!(
                        block,
                        crate::messages::ContentBlock::ToolUse { .. }
                            | crate::messages::ContentBlock::ToolResult { .. }
                    ),
                    "non-adjacent tool blocks must be stripped (messages[{i}]): {block:?}"
                );
            }
        }
    }
    assert_adjacency_invariant(&messages);
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: s021_parallel_calls_split_by_normalization (adapted)
#[test]
fn s021_parallel_calls_split_by_normalization() {
    // Parallel tool calls split into separate assistant/user pairs: every pair
    // is adjacent, so all survive.
    let mut messages = vec![
        m_user(vec![m_text("do both")]),
        m_assistant(vec![m_tool_use("tcA")]),
        m_user(vec![m_tool_result("tcA")]),
        m_assistant(vec![m_tool_use("tcB")]),
        m_user(vec![m_tool_result("tcB")]),
    ];
    clean_orphaned_blocks_by_adjacency(&mut messages);
    assert_eq!(messages.len(), 5, "both adjacent pairs survive");
    assert_adjacency_invariant(&messages);
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: s021_mixed_assistant_keeps_text_strips_orphan_tool_use (adapted)
#[test]
fn s021_mixed_assistant_keeps_text_strips_orphan_tool_use() {
    let mut messages = vec![
        m_user(vec![m_text("go")]),
        m_assistant(vec![m_text("thinking..."), m_tool_use("tc1")]),
        // Next message is user text, not a tool_result
        m_user(vec![m_text("nevermind")]),
    ];
    clean_orphaned_blocks_by_adjacency(&mut messages);
    assert_eq!(messages.len(), 3);
    let crate::messages::MessageContent::Blocks(blocks) = &messages[1].content else {
        panic!();
    };
    assert_eq!(blocks.len(), 1, "only the text block survives");
    assert!(matches!(
        blocks[0],
        crate::messages::ContentBlock::Text { .. }
    ));
    assert_adjacency_invariant(&messages);
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: s021_e2e_resume_parallel_calls_aborted (adapted)
#[test]
fn s021_e2e_resume_parallel_calls_aborted() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("run both"),
        ConversationItem::assistant_tool_calls(vec![mk_call("tcA")]),
        ConversationItem::tool_result("tcA", "aborted"),
        ConversationItem::assistant_tool_calls(vec![mk_call("tcB")]),
        ConversationItem::tool_result("tcB", "aborted"),
    ]);
    let msgs = build_messages_request(&req);
    // user("run both") -> asst(tcA) -> user(tcA) -> asst(tcB) -> user(tcB)
    let roles: Vec<&str> = msgs
        .messages
        .iter()
        .map(|m| match m.role {
            crate::messages::MessageRole::User => "user",
            crate::messages::MessageRole::Assistant => "assistant",
        })
        .collect();
    assert_eq!(
        roles,
        vec!["user", "assistant", "user", "assistant", "user"],
        "all paired turns survive: {roles:?}"
    );
    assert_adjacency_invariant(&msgs.messages);
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: s021_empty_messages_removed_after_stripping (adapted)
#[test]
fn s021_empty_messages_removed_after_stripping() {
    let mut messages = vec![
        m_assistant(vec![m_tool_use("tc1")]),
        // Not a user message, so tool_use tc1 has no adjacent result
        m_assistant(vec![m_text("oops")]),
    ];
    clean_orphaned_blocks_by_adjacency(&mut messages);
    assert_eq!(messages.len(), 1, "the emptied first message is removed");
    let crate::messages::MessageContent::Blocks(blocks) = &messages[0].content else {
        panic!();
    };
    assert!(
        blocks.iter().any(
            |b| matches!(b, crate::messages::ContentBlock::Text { text, .. } if text == "oops")
        )
    );
}

// Dropped with reason (spec §3(a)): `test_orphaned_tool_search_call_stripped`
// (wire.rs:3027) — grok's ConversationItem has no tool-search kind (Assistant
// tool_calls only; BackendToolCall maps to text, never tool_use), so there is
// no tool-search analogue to strip. Re-audit if a tool-search seam lands.

/// A1 property test: for any input, the post-build messages array satisfies
/// the adjacency invariant — every tool_use id has a matching tool_result in
/// the immediately following user message, and vice versa. Deterministic
/// pseudo-random fuzz over the item alphabet with a small id pool so pairing,
/// orphaning, and interleaving all occur.
#[test]
fn messages_wire_satisfies_adjacency_invariant_for_any_input() {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let next = |s: &mut u64| -> u64 {
        *s = s
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *s >> 31
    };
    for _ in 0..4000 {
        let mut items: Vec<ConversationItem> = Vec::new();
        let n_items = (next(&mut state) % 8) as usize;
        for _ in 0..n_items {
            match next(&mut state) % 5 {
                0 => items.push(ConversationItem::user("u")),
                1 => {
                    let n = 1 + (next(&mut state) % 3) as usize;
                    items.push(ConversationItem::assistant_tool_calls(
                        (0..n)
                            .map(|_| mk_call(&format!("id_{}", next(&mut state) % 4)))
                            .collect(),
                    ));
                }
                2 => items.push(ConversationItem::assistant("a")),
                3 => {
                    let item = crate::synthesized_reasoning_item("think");
                    let signed = next(&mut state) % 2 == 0;
                    items.push(ConversationItem::Reasoning(if signed {
                        crate::rs::ReasoningItem {
                            encrypted_content: Some("sig".to_string()),
                            ..item
                        }
                    } else {
                        item
                    }));
                }
                _ => items.push(ConversationItem::tool_result(
                    format!("id_{}", next(&mut state) % 4),
                    "out",
                )),
            }
        }
        let req = ConversationRequest::from_items(items).with_model("fuzz-model");
        let msgs = build_messages_request(&req);
        match check_adjacency_invariant(&msgs.messages) {
            Ok(()) => {}
            Err(err) => panic!("items={:?} messages={:?}: {err}", req.items, msgs.messages),
        }
    }
}

// ============================================================================
// (b) Thinking replay rule — three-part strip (spec rule 2, xli S-031)
// ============================================================================

fn mk_reasoning(text: &str, signature: Option<&str>) -> ConversationItem {
    let mut item = crate::synthesized_reasoning_item(text);
    item.encrypted_content = signature.map(str::to_string);
    ConversationItem::Reasoning(item)
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: test_thinking_stripped_from_earlier_assistant_messages (adapted)
#[test]
fn test_thinking_stripped_from_earlier_assistant_messages() {
    // Turn 1: user -> thinking + tool_use -> tool_result
    // Turn 2: user -> thinking + text (latest)
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("First question"),
        mk_reasoning("Old thinking", Some("old_sig")),
        ConversationItem::assistant_tool_calls(vec![mk_call("toolu_01")]),
        ConversationItem::tool_result("toolu_01", "files"),
        ConversationItem::user("Second question"),
        mk_reasoning("Latest thinking", Some("latest_sig")),
        ConversationItem::assistant("Final answer"),
    ]);
    let msgs = build_messages_request(&req);

    // First assistant message: thinking stripped, only tool_use remains
    let first_assistant = msgs
        .messages
        .iter()
        .find(|m| {
            matches!(
                &m.content,
                crate::messages::MessageContent::Blocks(b)
                    if b.iter().any(|x| matches!(x, crate::messages::ContentBlock::ToolUse { .. }))
            )
        })
        .expect("should have an assistant message with tool_use");
    let crate::messages::MessageContent::Blocks(blocks) = &first_assistant.content else {
        panic!();
    };
    for block in blocks {
        assert!(
            !matches!(block, crate::messages::ContentBlock::Thinking { .. }),
            "thinking should be stripped from earlier assistant messages: {blocks:?}"
        );
    }

    // Last assistant message keeps its (verbatim, signed) thinking
    let last_assistant = msgs
        .messages
        .iter()
        .rfind(|m| matches!(m.role, crate::messages::MessageRole::Assistant))
        .unwrap();
    let crate::messages::MessageContent::Blocks(blocks) = &last_assistant.content else {
        panic!();
    };
    let thinking = blocks
        .iter()
        .find(|b| matches!(b, crate::messages::ContentBlock::Thinking { .. }))
        .expect("latest assistant message must keep its thinking block");
    let crate::messages::ContentBlock::Thinking { signature, .. } = thinking else {
        panic!();
    };
    assert_eq!(signature, "latest_sig");
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: test_empty_assistant_messages_removed_after_stripping (adapted)
#[test]
fn test_empty_assistant_messages_removed_after_stripping() {
    // An assistant message that contains ONLY a thinking block (non-latest)
    // is emptied by the strip and removed entirely.
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("Q1"),
        mk_reasoning("Only thinking, no text", Some("sig_only_think")),
        // No assistant text follows — this creates an assistant msg with only thinking
        ConversationItem::user("Q2"),
        ConversationItem::assistant("A2"),
    ]);
    let msgs = build_messages_request(&req);
    for (i, msg) in msgs.messages.iter().enumerate() {
        if matches!(msg.role, crate::messages::MessageRole::Assistant) {
            let crate::messages::MessageContent::Blocks(blocks) = &msg.content else {
                continue;
            };
            assert!(
                !blocks.is_empty(),
                "assistant messages emptied by the strip must be removed (messages[{i}])"
            );
        }
    }
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: test_single_assistant_message_thinking_preserved (adapted)
#[test]
fn test_single_assistant_message_thinking_preserved() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("Hello"),
        mk_reasoning("The only thinking block", Some("sig_only")),
        ConversationItem::assistant("Answer"),
    ]);
    let msgs = build_messages_request(&req);
    let last = msgs
        .messages
        .iter()
        .rfind(|m| matches!(m.role, crate::messages::MessageRole::Assistant))
        .expect("assistant message must exist");
    let crate::messages::MessageContent::Blocks(blocks) = &last.content else {
        panic!();
    };
    let thinking = blocks
        .iter()
        .find(|b| matches!(b, crate::messages::ContentBlock::Thinking { .. }))
        .expect("single assistant message must keep its thinking block");
    let crate::messages::ContentBlock::Thinking { thinking, .. } = thinking else {
        panic!();
    };
    assert_eq!(thinking, "The only thinking block");
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: test_opus47_empty_thinking_with_signature_dropped (adapted)
/// S-OPUS47-EMPTY-THINKING regression: a persisted reasoning item whose text is
/// empty but whose signature is present (the opus47 shape) must be dropped
/// entirely from the latest assistant message — a signature-only block cannot
/// be replayed.
#[test]
fn test_opus47_empty_thinking_with_signature_dropped() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::Reasoning(crate::rs::ReasoningItem {
            id: String::new(),
            summary: Vec::new(),
            content: None,
            encrypted_content: Some("Er4CCmUIDhACGAIqQMQHBF5Vrealsig==".to_string()),
            status: None,
        }),
        ConversationItem::assistant("Real reply"),
    ]);
    let msgs = build_messages_request(&req);
    // Both items coalesce into one assistant message; the empty-thinking
    // block must be absent so only the text block survives.
    let last = msgs
        .messages
        .iter()
        .rfind(|m| matches!(m.role, crate::messages::MessageRole::Assistant))
        .expect("assistant message must exist");
    let crate::messages::MessageContent::Blocks(blocks) = &last.content else {
        panic!();
    };
    for block in blocks {
        assert!(
            !matches!(
                block,
                crate::messages::ContentBlock::Thinking { .. }
                    | crate::messages::ContentBlock::RedactedThinking { .. }
            ),
            "signature-only thinking block must be dropped: {block:?}"
        );
    }
    assert!(
        blocks.iter().any(|b| matches!(b, crate::messages::ContentBlock::Text { text, .. } if text == "Real reply")),
        "real assistant text must survive: {blocks:?}"
    );
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: test_fallback_when_no_raw_wire_block (re-derived)
/// Re-derived for grok: V1 has no raw_wire_block storage (spec D3), so grok's
/// untrustworthy class is the unsigned block — thinking text without a
/// signature. As the latest assistant message it would fail Anthropic's
/// `thinking blocks ... cannot be modified` check, so it must be stripped
/// while the real assistant content survives.
#[test]
fn test_unsigned_thinking_stripped_from_latest_assistant() {
    let req = ConversationRequest::from_items(vec![
        mk_reasoning("Reconstructed text", None),
        ConversationItem::assistant("Final answer"),
    ]);
    let msgs = build_messages_request(&req);
    let last = msgs
        .messages
        .iter()
        .rfind(|m| matches!(m.role, crate::messages::MessageRole::Assistant))
        .expect("assistant message must exist");
    let crate::messages::MessageContent::Blocks(blocks) = &last.content else {
        panic!();
    };
    assert!(
        blocks.iter().all(|b| {
            !matches!(
                b,
                crate::messages::ContentBlock::Thinking { .. }
                    | crate::messages::ContentBlock::RedactedThinking { .. }
            )
        }),
        "unsigned thinking must be stripped from the latest assistant: {blocks:?}"
    );
    assert!(
        blocks.iter().any(|b| matches!(b, crate::messages::ContentBlock::Text { text, .. } if text == "Final answer")),
        "real assistant text must survive: {blocks:?}"
    );
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: test_reconstructed_thinking_stripped_from_latest_assistant (re-derived)
/// Re-derived for grok: a lone unsigned Reasoning item lands as the latest
/// assistant message. Its thinking block is stripped and the emptied
/// assistant message is removed entirely.
#[test]
fn test_unsigned_reasoning_only_latest_assistant_removed() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("q"),
        mk_reasoning("Deep thoughts", None),
    ]);
    let msgs = build_messages_request(&req);
    for (i, msg) in msgs.messages.iter().enumerate() {
        if let crate::messages::MessageContent::Blocks(blocks) = &msg.content {
            for block in blocks {
                assert!(
                    !matches!(
                        block,
                        crate::messages::ContentBlock::Thinking { .. }
                            | crate::messages::ContentBlock::RedactedThinking { .. }
                    ),
                    "unsigned thinking must not reach the wire (messages[{i}]): {block:?}"
                );
            }
        }
    }
    assert!(
        !msgs
            .messages
            .iter()
            .any(|m| matches!(m.role, crate::messages::MessageRole::Assistant)),
        "the emptied assistant message must be removed: {:#?}",
        msgs.messages
    );
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: test_raw_thinking_survives_but_reconstructed_dropped_in_latest (re-derived)
/// Re-derived for grok: the port source's raw-vs-reconstructed distinction
/// has no grok V1 counterpart (no raw_wire_block, spec D3). grok's trust
/// signal is completeness: a thinking block survives the latest assistant
/// only as a verbatim (text, signature) pair. Mixed blocks: the complete
/// pair survives, the unsigned one is dropped.
#[test]
fn test_verbatim_pair_survives_but_unsigned_dropped_in_latest() {
    let req = ConversationRequest::from_items(vec![
        mk_reasoning("RAW verbatim thinking", Some("RawSignature==")),
        mk_reasoning("reconstructed thinking", None),
    ]);
    let msgs = build_messages_request(&req);
    let last = msgs
        .messages
        .iter()
        .rfind(|m| matches!(m.role, crate::messages::MessageRole::Assistant))
        .expect("assistant message must exist");
    let crate::messages::MessageContent::Blocks(blocks) = &last.content else {
        panic!();
    };
    let thinking: Vec<&crate::messages::ContentBlock> = blocks
        .iter()
        .filter(|b| matches!(b, crate::messages::ContentBlock::Thinking { .. }))
        .collect();
    assert_eq!(
        thinking.len(),
        1,
        "only the complete (text, signature) pair must survive: {blocks:?}"
    );
    let crate::messages::ContentBlock::Thinking {
        thinking,
        signature,
    } = thinking[0]
    else {
        panic!();
    };
    assert_eq!(thinking, "RAW verbatim thinking");
    assert_eq!(signature, "RawSignature==");
}

// ============================================================================
// (c) Trailing-assistant repair — S-014
// ============================================================================

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: trailing_plain_text_assistant_gets_continue_sentinel (adapted)
#[test]
fn trailing_plain_text_assistant_gets_continue_sentinel() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("hello"),
        ConversationItem::assistant("hi there"),
    ])
    .with_model("messages-compatible-model");
    let msgs = build_messages_request(&req);
    assert_eq!(
        msgs.messages.len(),
        3,
        "user + assistant + synthetic user: {msgs:?}"
    );
    let last = msgs.messages.last().unwrap();
    assert!(
        matches!(last.role, crate::messages::MessageRole::User),
        "the appended sentinel must be a user message"
    );
    assert_eq!(
        sentinel_text(last),
        "[Continue]",
        "plain-text assistant ending gets the [Continue] sentinel"
    );
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: trailing_tool_use_assistant_gets_awaiting_sentinel (re-derived)
/// Re-derived at the repair-fn level: through the full pipeline a trailing
/// assistant can never retain a tool_use — the adjacency stage strips it
/// first, exactly as in the port source, whose own test therefore only
/// asserts the negative side. The awaiting branch is exercised directly here
/// as defensive parity, with the negative arm at the same level.
#[test]
fn trailing_tool_use_assistant_gets_awaiting_sentinel() {
    let mut msgs = vec![m_assistant(vec![m_text("working"), m_tool_use("tc1")])];
    repair_trailing_assistant(&mut msgs);
    assert_eq!(msgs.len(), 2, "a synthetic user sentinel is appended");
    let last = &msgs[1];
    assert!(matches!(last.role, crate::messages::MessageRole::User));
    assert_eq!(
        sentinel_text(last),
        "[Awaiting tool result]",
        "a trailing assistant with a tool_use gets the awaiting sentinel"
    );

    let mut plain = vec![m_assistant(vec![m_text("done")])];
    repair_trailing_assistant(&mut plain);
    assert_eq!(
        sentinel_text(&plain[1]),
        "[Continue]",
        "a tool_use-free trailing assistant gets [Continue]"
    );
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: forked_conversation_ending_with_assistant_gets_sentinel (adapted)
#[test]
fn forked_conversation_ending_with_assistant_gets_sentinel() {
    // A fork/resume snapshot that ends on an assistant boundary.
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("analyze this code"),
        ConversationItem::assistant("I'll analyze the code for you."),
    ])
    .with_model("messages-compatible-model");
    let msgs = build_messages_request(&req);
    assert_eq!(
        msgs.messages.len(),
        3,
        "forked assistant-ending conv needs a sentinel: {msgs:?}"
    );
    let last = msgs.messages.last().unwrap();
    assert!(matches!(last.role, crate::messages::MessageRole::User));
    assert_eq!(sentinel_text(last), "[Continue]");
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: history_cache_control_lands_on_synthetic_user_after_trailing_assistant (adapted)
#[test]
fn history_cache_control_lands_on_synthetic_user_after_trailing_assistant() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("hi"),
        ConversationItem::assistant("response"),
    ])
    .with_model("messages-compatible-model");
    let json = serde_json::to_value(build_messages_request(&req)).unwrap();
    let last = &json["messages"][2];
    assert_eq!(last["role"], "user");
    assert_eq!(
        last["content"][0]["cache_control"]["type"], "ephemeral",
        "the synthetic trailing user message must carry the cache breakpoint: {json:#}"
    );
    for block in json["messages"][1]["content"].as_array().unwrap() {
        assert!(
            block.get("cache_control").is_none(),
            "the assistant turn must not carry the marker: {json:#}"
        );
    }
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: no_synthetic_user_when_ending_on_user (re-derived)
/// Negation arm of the repair rule: a history ending on a user message gets
/// no synthetic append.
#[test]
fn no_synthetic_user_when_ending_on_user() {
    let req = ConversationRequest::from_items(vec![ConversationItem::user("hello")])
        .with_model("messages-compatible-model");
    let msgs = build_messages_request(&req);
    assert_eq!(msgs.messages.len(), 1, "no synthetic message needed");
    assert!(matches!(
        msgs.messages[0].role,
        crate::messages::MessageRole::User
    ));
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: no_synthetic_user_after_tool_result (re-derived)
/// Negation arm of the repair rule: a paired tool result ends the history on
/// a user message, so the last message must be the real tool_result, not a
/// synthetic sentinel.
#[test]
fn no_synthetic_user_after_tool_result() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("run ls"),
        ConversationItem::assistant_tool_calls(vec![mk_call("toolu_pair")]),
        ConversationItem::tool_result("toolu_pair", "file.txt"),
    ])
    .with_model("messages-compatible-model");
    let msgs = build_messages_request(&req);
    let last = msgs.messages.last().unwrap();
    assert!(
        matches!(last.role, crate::messages::MessageRole::User),
        "tool_result is role:user — no synthetic needed"
    );
    assert!(
        matches!(
            blocks_of(last).first(),
            Some(crate::messages::ContentBlock::ToolResult { .. })
        ),
        "the last message must be the real tool_result, not a sentinel: {last:?}"
    );
}

// ============================================================================
// (e) tool_result hoist — S-022
// ============================================================================

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: s022_hoists_tool_result_when_preceded_by_text (adapted)
#[test]
fn s022_hoists_tool_result_when_preceded_by_text() {
    let mut msgs = vec![m_user(vec![
        m_text("Warning: too many processes"),
        m_tool_result("tc1"),
    ])];
    hoist_tool_results_to_front(&mut msgs);
    let blocks = blocks_of(&msgs[0]);
    let crate::messages::ContentBlock::ToolResult { tool_use_id, .. } = &blocks[0] else {
        panic!("the tool_result must be hoisted to the front: {blocks:?}");
    };
    assert_eq!(tool_use_id, "tc1");
    assert!(
        matches!(&blocks[1], crate::messages::ContentBlock::Text { text, .. }
            if text == "Warning: too many processes"),
        "the text block follows: {blocks:?}"
    );
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: s022_stable_partition_preserves_relative_order (adapted)
#[test]
fn s022_stable_partition_preserves_relative_order() {
    let mut msgs = vec![m_user(vec![
        m_tool_result("tcA"),
        m_text("mid"),
        m_tool_result("tcB"),
        m_text("end"),
    ])];
    hoist_tool_results_to_front(&mut msgs);
    let blocks = blocks_of(&msgs[0]);
    assert_eq!(blocks.len(), 4);
    let crate::messages::ContentBlock::ToolResult { tool_use_id: a, .. } = &blocks[0] else {
        panic!("{blocks:?}");
    };
    let crate::messages::ContentBlock::ToolResult { tool_use_id: b, .. } = &blocks[1] else {
        panic!("{blocks:?}");
    };
    assert_eq!(
        (a.as_str(), b.as_str()),
        ("tcA", "tcB"),
        "relative order of results is preserved"
    );
    assert!(
        matches!(&blocks[2], crate::messages::ContentBlock::Text { text, .. } if text == "mid")
            && matches!(&blocks[3], crate::messages::ContentBlock::Text { text, .. } if text == "end"),
        "the text blocks follow in relative order: {blocks:?}"
    );
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: s022_noop_when_tool_results_already_leading (adapted)
#[test]
fn s022_noop_when_tool_results_already_leading() {
    let mut msgs = vec![m_user(vec![m_tool_result("tc1"), m_text("trailing")])];
    hoist_tool_results_to_front(&mut msgs);
    let blocks = blocks_of(&msgs[0]);
    assert_eq!(blocks.len(), 2);
    assert!(
        matches!(&blocks[0], crate::messages::ContentBlock::ToolResult { tool_use_id, .. }
            if tool_use_id == "tc1")
            && matches!(&blocks[1], crate::messages::ContentBlock::Text { text, .. }
                if text == "trailing"),
        "already-correct ordering is a no-op: {blocks:?}"
    );
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: s022_noop_when_no_tool_results (adapted)
#[test]
fn s022_noop_when_no_tool_results() {
    let mut msgs = vec![m_user(vec![m_text("hello"), m_text("world")])];
    hoist_tool_results_to_front(&mut msgs);
    let blocks = blocks_of(&msgs[0]);
    assert!(
        matches!(&blocks[0], crate::messages::ContentBlock::Text { text, .. } if text == "hello")
            && matches!(&blocks[1], crate::messages::ContentBlock::Text { text, .. } if text == "world"),
        "a message with no tool_result is untouched: {blocks:?}"
    );
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: s022_does_not_touch_assistant_messages (adapted)
#[test]
fn s022_does_not_touch_assistant_messages() {
    let mut msgs = vec![m_assistant(vec![m_text("thinking"), m_tool_use("tc1")])];
    hoist_tool_results_to_front(&mut msgs);
    let blocks = blocks_of(&msgs[0]);
    assert!(
        matches!(&blocks[0], crate::messages::ContentBlock::Text { text, .. } if text == "thinking")
            && matches!(&blocks[1], crate::messages::ContentBlock::ToolUse { id, .. } if id == "tc1"),
        "assistant messages are not reordered: {blocks:?}"
    );
}

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: s022_e2e_warning_injected_between_tool_use_and_result (re-derived)
/// Re-derived for grok: the port source merges consecutive user-role items
/// into one message, so an out-of-band warning injected between a tool call
/// and its result lands in the SAME user message as the tool_result and the
/// hoist reorders it to [tool_result, text]. grok's builder does not merge
/// consecutive user messages in V1, so the warning message sits between the
/// assistant tool_use and the tool_result user message: the pair is
/// non-adjacent and both sides are stripped by the S-021 stage instead. The
/// hoist stays defensive in V1 (the builder never emits a mixed
/// text+tool_result user message) and becomes live when a user-merging seam
/// lands (MW-2); this test pins the invariant side of the scenario.
#[test]
fn s022_e2e_warning_injected_between_tool_use_and_result() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("do the thing"),
        ConversationItem::assistant_tool_calls(vec![mk_call("tc1")]),
        // Out-of-band warning injected into history between the tool call
        // and its output.
        ConversationItem::user("Warning: too many processes"),
        ConversationItem::tool_result("tc1", "ok"),
    ])
    .with_model("messages-compatible-model");
    let msgs = build_messages_request(&req);
    // MW-2 R1 ripple (disclosed in the MW-2 R1 commit): the warning and its
    // tool_result are consecutive user-role items, so the same-role merge
    // puts them in ONE user message directly following the assistant. The
    // pair is message-adjacent and SURVIVES (pre-MW-2 the warning sat in
    // its own message, split the pair, and the message-level strip removed
    // both sides) — this is exactly the S-022 shape the hoist was written
    // for.
    assert_eq!(msgs.messages.len(), 3, "{:?}", msgs.messages);
    assert!(
        blocks_of(&msgs.messages[1])
            .iter()
            .any(|b| matches!(b, crate::messages::ContentBlock::ToolUse { .. })),
        "the now-adjacent tool_use must survive: {:?}",
        msgs.messages
    );
    // The load-bearing wire invariant (S-022) is unchanged: any
    // tool_result-bearing message must lead with its tool_result — the
    // hoist puts the result ahead of the warning text inside the merged
    // message.
    for (i, msg) in msgs.messages.iter().enumerate() {
        let blocks = blocks_of(msg);
        if blocks
            .iter()
            .any(|b| matches!(b, crate::messages::ContentBlock::ToolResult { .. }))
        {
            assert!(
                matches!(
                    blocks.first(),
                    Some(crate::messages::ContentBlock::ToolResult { .. })
                ),
                "messages[{i}]: a tool_result-bearing message must lead with it"
            );
        }
    }
    let merged = &msgs.messages[2];
    assert!(
        blocks_of(merged)
            .first()
            .is_some_and(|b| { matches!(b, crate::messages::ContentBlock::ToolResult { .. }) })
            && blocks_of(merged).iter().any(|b| matches!(
                b,
                crate::messages::ContentBlock::Text {
                    text, ..
                } if text == "Warning: too many processes"
            )),
        "the merged user message is [tool_result, warning text]: {merged:?}"
    );
    assert_adjacency_invariant(&msgs.messages);
}

// ============================================================================
// (d) Per-model helpers — combination arms through the full builder
// ============================================================================

/// New grok-shape test (MW-1 spec D4, re-adjudicated ledger 2026-09-12): an
/// explicit request budget wins over the per-model cap and is floored at 1
/// (never serialized below the floor); a no-budget request with an empty
/// table serializes 0 — the live proxy tolerates 0 (pre-MW-1 wire parity;
/// a floor-1 fallback guaranteed truncation, L2 l2_messages_wire).
#[test]
fn d4_max_tokens_combination_arms() {
    fn built_max(model: &str, max_output_tokens: Option<u32>) -> u32 {
        let req = ConversationRequest {
            items: vec![ConversationItem::user("hi")],
            model: Some(model.to_string()),
            reasoning_effort: None,
            max_output_tokens,
            ..Default::default()
        };
        build_messages_request(&req).max_tokens
    }
    assert_eq!(
        built_max("claude-sonnet-5", Some(4096)),
        4096,
        "set + known slug: the request value wins"
    );
    assert_eq!(
        built_max("some-unknown-slug", Some(4096)),
        4096,
        "set + unknown slug: the request value wins"
    );
    assert_eq!(
        built_max("claude-sonnet-5", Some(0)),
        crate::messages_model::MESSAGES_MAX_OUTPUT_TOKENS_FLOOR,
        "explicit budget is floored: Some(0) never serializes 0"
    );
    assert_eq!(
        built_max("claude-sonnet-5", None),
        0,
        "unset + known slug (empty table): proxy-tolerated 0, pre-MW-1 wire parity"
    );
    assert_eq!(
        built_max("some-unknown-slug", None),
        0,
        "unset + unknown slug: proxy-tolerated 0, pre-MW-1 wire parity"
    );
}

// ============================================================================
// Regression pin (spec §3): System → system-param placement
// ============================================================================

/// Regression pin (spec §3, grok-shape — grok already places system items
/// correctly, this exists so a future change cannot silently move them into
/// `messages`): `ConversationItem::System` lands in the request's `system`
/// param and contributes no message of its own.
#[test]
fn regression_pin_system_items_land_in_system_param_not_messages() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::system("You are a helpful assistant."),
        ConversationItem::user("Fix the bug"),
    ])
    .with_model("messages-compatible-model");
    let json = serde_json::to_value(build_messages_request(&req)).unwrap();

    let system = json["system"]
        .as_array()
        .expect("system param present: {json:#}");
    assert!(
        system
            .iter()
            .any(|b| b["text"] == "You are a helpful assistant."),
        "the system text must be in the system param: {json:#}"
    );

    let messages = json["messages"].as_array().unwrap();
    assert_eq!(
        messages.len(),
        1,
        "the system item must not contribute a message: {json:#}"
    );
    assert_eq!(messages[0]["role"], "user");
    for block in messages[0]["content"].as_array().unwrap() {
        assert_ne!(
            block["text"], "You are a helpful assistant.",
            "system text must never appear inside messages: {json:#}"
        );
    }
}
// ============================================================================
// (f) R1 — same-role merge (MW-2)
// ============================================================================

/// Provenance: hyper-grok-build@45e984f3 — packages/ai/xai-grok-sampler/src/bedrock.rs :: request_merges_consecutive_tool_results (re-expressed; Bedrock wire evidence — assertion tightened for the xli merge rule + Vertex/proxy class, per MW-2 spec R1/gap 8)
/// The source pins TR-item merge only: its builder still emits the user text
/// as a second user message (Converse tolerates consecutive users; it asserts
/// 2 messages / 2 blocks). This re-expression asserts the full merge:
/// `[tool_result, tool_result, user_text]` → ONE user message, 3 blocks,
/// pre-hoist order preserved. grok additionally needs the paired assistant
/// tool calls — item-level orphan cleanup (MW-1 stage 1) drops unpaired
/// results, which the source wire does not have.
#[test]
fn request_merges_consecutive_tool_results() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::system("sys"),
        assistant_with_calls(&[("tool_use_1", "read_file"), ("tool_use_2", "read_file")]),
        ConversationItem::tool_result("tool_use_1", "a"),
        ConversationItem::tool_result("tool_use_2", "b"),
        ConversationItem::user("hi"),
    ])
    .with_model("claude-sonnet-5");

    let json = serde_json::to_value(build_messages_request(&req)).unwrap();
    assert_eq!(json["system"].as_array().unwrap().len(), 1, "{json:#}");

    let messages = json["messages"].as_array().unwrap();
    assert_eq!(
        messages.len(),
        3,
        "leading sentinel (D2) + one assistant + ONE merged user message: {json:#}"
    );
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"][0]["text"], "[Continue]");
    assert_eq!(messages[1]["role"], "assistant");

    let blocks = messages[2]["content"].as_array().unwrap();
    assert_eq!(
        blocks.len(),
        3,
        "merged user message: [tool_result, tool_result, text]: {json:#}"
    );
    assert_eq!(blocks[0]["type"], "tool_result");
    assert_eq!(blocks[0]["tool_use_id"], "tool_use_1");
    assert_eq!(blocks[1]["type"], "tool_result");
    assert_eq!(blocks[1]["tool_use_id"], "tool_use_2");
    assert_eq!(blocks[2]["type"], "text");
    assert_eq!(blocks[2]["text"], "hi");
}

/// Fresh-written: MW-2 spec D2 (gap 10) double-sentinel pin. A history whose
/// only content is a single assistant item begins AND ends on assistant, so
/// BOTH sentinels fire: the leading `[Continue]` (D2 — unconditional, since a
/// head has no result pending) and the trailing `[Continue]` (S-014, no
/// tool_use at the tail). The cache window tip lands on the trailing sentinel.
#[test]
fn leading_assistant_only_history_gets_double_sentinel() {
    let req =
        ConversationRequest::from_items(vec![ConversationItem::assistant("lone assistant reply")])
            .with_model("claude-sonnet-5");

    let json = serde_json::to_value(build_messages_request(&req)).unwrap();
    let messages = json["messages"].as_array().unwrap();
    assert_eq!(
        messages.len(),
        3,
        "[U(\"[Continue]\"), A, U(\"[Continue]\")]: {json:#}"
    );
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"][0]["text"], "[Continue]");
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["content"][0]["text"], "lone assistant reply");
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(messages[2]["content"][0]["text"], "[Continue]");
    assert_eq!(
        marker_on_last_block(&messages[2]),
        Some("ephemeral"),
        "the cache window tip lands on the trailing sentinel: {json:#}"
    );
}

/// Fresh-written: D2's sentinel is unconditional `[Continue]` — even when the
/// leading assistant carries a `tool_use` whose result follows in history
/// (S-014's `[Awaiting tool result]` would misdescribe a head where the
/// result is already in the transcript).
#[test]
fn leading_assistant_with_tool_use_gets_continue_not_awaiting() {
    let req = ConversationRequest::from_items(vec![
        assistant_with_calls(&[("dx_1", "read_file")]),
        ConversationItem::tool_result("dx_1", "ok"),
    ])
    .with_model("claude-sonnet-5");

    let json = serde_json::to_value(build_messages_request(&req)).unwrap();
    let messages = json["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3, "{json:#}");
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(
        messages[0]["content"][0]["text"], "[Continue]",
        "the leading sentinel is unconditional: {json:#}"
    );
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[2]["role"], "user");
}

/// Fresh-written: R1 edge (spec gap 9b) — `System` items flush the pending
/// user run without producing a message, so user content on either side of a
/// system item stays in separate user messages.
#[test]
fn r1_system_item_flushes_pending_user_run() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::user("before the system change"),
        ConversationItem::system("new instructions"),
        ConversationItem::user("after the system change"),
    ])
    .with_model("claude-sonnet-5");

    let json = serde_json::to_value(build_messages_request(&req)).unwrap();
    let messages = json["messages"].as_array().unwrap();
    assert_eq!(
        messages.len(),
        2,
        "the system boundary must not merge the two user runs: {json:#}"
    );
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(
        messages[0]["content"][0]["text"],
        "before the system change"
    );
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"][0]["text"], "after the system change");
}

/// Fresh-written: R1 edge (spec gap 9i) — a user message left with EMPTY
/// content by the post-translation adjacency cleanup is removed (mirrors the
/// assistant half of MW-1 rule 2c); the pin's MessageParam.content is
/// non-empty (messages.ts:1207-1210), so `"content": []` must never serialize.
#[test]
fn r1_emptied_user_message_removed_after_adjacency_strip() {
    // A1's only call (pd_1) is non-adjacent — split by the user text, the
    // second assistant turn, and a system boundary — so the message-level
    // strip removes the call; the tool-result-only user message TR(pd_1)
    // sat in is then emptied by the same strip and removed.
    let req = ConversationRequest::from_items(vec![
        assistant_with_calls(&[("pd_1", "t")]),
        ConversationItem::user("in between"),
        assistant_with_calls(&[("pd_2", "t")]),
        ConversationItem::tool_result("pd_2", "late"),
        ConversationItem::system("system boundary"),
        ConversationItem::tool_result("pd_1", "early"),
    ])
    .with_model("claude-sonnet-5");

    let json = serde_json::to_value(build_messages_request(&req)).unwrap();
    let messages = json["messages"].as_array().unwrap();
    for (i, msg) in messages.iter().enumerate() {
        assert!(
            !msg["content"].as_array().is_some_and(Vec::is_empty),
            "messages[{i}] serialized with empty content: {json:#}"
        );
    }
    // The surviving shape: the non-adjacent pair is gone on both sides (no
    // pd_1 anywhere), the emptied user message is removed, and the adjacent
    // pd_2 pair survives.
    assert_eq!(messages.len(), 3, "{json:#}");
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"][0]["text"], "in between");
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(messages[2]["content"][0]["type"], "tool_result");
    assert_eq!(messages[2]["content"][0]["tool_use_id"], "pd_2");
}

/// Fresh-written: MW-2 spec D6 alternation PROPERTY test. For the six focus
/// sequences (a)-(f) — including the D2 double-sentinel `[A]` case — the
/// post-build message list never contains two consecutive same-role messages.
#[test]
fn post_build_messages_never_contain_consecutive_same_role() {
    fn assert_alternates(messages: &[crate::messages::Message], label: &str) {
        for (i, (a, b)) in messages.iter().zip(messages.iter().skip(1)).enumerate() {
            assert_ne!(
                a.role, b.role,
                "{label}: consecutive same-role messages at {i}: {messages:?}"
            );
        }
    }

    // (a) [A(tool_use), TR, U(text)]
    let a = build_messages_request(
        &ConversationRequest::from_items(vec![
            assistant_with_calls(&[("pa_1", "read_file")]),
            ConversationItem::tool_result("pa_1", "ok"),
            ConversationItem::user("after the result"),
        ])
        .with_model("claude-sonnet-5"),
    );
    assert_alternates(&a.messages, "(a)");

    // (b) [U(text), A(tool_use x2), TR, TR]
    let b = build_messages_request(
        &ConversationRequest::from_items(vec![
            ConversationItem::user("before"),
            assistant_with_calls(&[("pb_1", "t"), ("pb_2", "t")]),
            ConversationItem::tool_result("pb_1", "r1"),
            ConversationItem::tool_result("pb_2", "r2"),
        ])
        .with_model("claude-sonnet-5"),
    );
    assert_alternates(&b.messages, "(b)");

    // (c) [A(text), A(tool_use)] — the orphaned call is cleaned pre-translation
    let c = build_messages_request(
        &ConversationRequest::from_items(vec![
            ConversationItem::assistant("first assistant"),
            assistant_with_calls(&[("pc_1", "t")]),
        ])
        .with_model("claude-sonnet-5"),
    );
    assert_alternates(&c.messages, "(c)");

    // (d) [TR, U, A, TR]
    let d = build_messages_request(
        &ConversationRequest::from_items(vec![
            ConversationItem::tool_result("pd_1", "early result"),
            ConversationItem::user("mid"),
            assistant_with_calls(&[("pd_1", "t"), ("pd_2", "t")]),
            ConversationItem::tool_result("pd_2", "late result"),
        ])
        .with_model("claude-sonnet-5"),
    );
    assert_alternates(&d.messages, "(d)");

    // (e) system-only prefix
    let e = build_messages_request(
        &ConversationRequest::from_items(vec![
            ConversationItem::system("one"),
            ConversationItem::system("two"),
            ConversationItem::user("after systems"),
        ])
        .with_model("claude-sonnet-5"),
    );
    assert_alternates(&e.messages, "(e)");

    // (f) the D2 double-sentinel [A] case
    let f = build_messages_request(
        &ConversationRequest::from_items(vec![ConversationItem::assistant("lone assistant")])
            .with_model("claude-sonnet-5"),
    );
    assert_alternates(&f.messages, "(f)");
}

// ============================================================================
// (g) R2 — tool-argument strictness; R3 — unified image-source parse (MW-2)
// ============================================================================

/// Provenance: hyper-grok-build@45e984f3 — packages/ai/xai-grok-sampler/src/pi_messages.rs :: request_rejects_non_data_images_and_non_object_tool_arguments (re-expressed; Pi wire evidence — softened from reject to degrade, per R2/R3)
/// The source hard-rejects (Err(InvalidConfiguration)) non-data images and
/// non-object tool arguments. grok's messages builder degrades instead: an
/// out-of-union / malformed image reference becomes a labeled text fallback
/// (R3) and parseable non-object arguments coerce to `{}` (R2).
#[test]
fn request_rejects_non_data_images_and_non_object_tool_arguments() {
    let req = ConversationRequest::from_items(vec![
        ConversationItem::User(UserItem {
            content: vec![
                ContentPart::Text {
                    text: "look at this".into(),
                },
                ContentPart::Image {
                    url: "not-a-data-uri".into(),
                },
            ],
            ..Default::default()
        }),
        ConversationItem::Assistant(AssistantItem {
            content: String::new().into(),
            tool_calls: vec![ToolCall {
                id: "rg_1".into(),
                name: "t".into(),
                arguments: "[]".into(),
            }],
            model_id: None,
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        ConversationItem::tool_result("rg_1", "done"),
    ])
    .with_model("claude-sonnet-5");

    let json = serde_json::to_value(build_messages_request(&req)).unwrap();
    let messages = json["messages"].as_array().unwrap();

    // (R3) the unknown-scheme image degrades to the labeled text fallback,
    // never an image source.
    let user_blocks = messages[0]["content"].as_array().unwrap();
    assert_eq!(user_blocks[1]["type"], "text", "{json:#}");
    assert_eq!(user_blocks[1]["text"], "[invalid image: not-a-data-uri]");

    // (R2) parseable non-object arguments coerce to an empty object.
    let assistant_blocks = messages[1]["content"].as_array().unwrap();
    let tool_use = assistant_blocks
        .iter()
        .find(|b| b["type"] == "tool_use")
        .expect("tool_use block present: {json:#}");
    assert_eq!(tool_use["input"], serde_json::json!({}));
}

/// Fresh-written: R2 hardening — parseable NON-object arguments coerce to
/// `{}` (grok's own tool calls are always object-shaped; a non-object replayed
/// argument is a local artifact, and `{}` is the safe form). The pinned
/// contract types `ToolUseBlockParam.input` as `unknown`
/// (wirejig/refs/anthropic@d3d5028 messages.ts:2355) — this cites the pin for
/// the `unknown` typing, NOT for any object requirement; the coercion is
/// hardening beyond the pin and beyond xli (which guards only the unparseable
/// case, wire.rs:173/211).
#[test]
fn tool_arguments_parseable_non_object_coerced_to_empty_object() {
    fn tool_use_input(arguments: &str) -> serde_json::Value {
        let req = ConversationRequest::from_items(vec![
            ConversationItem::Assistant(AssistantItem {
                content: String::new().into(),
                tool_calls: vec![ToolCall {
                    id: "rn_1".into(),
                    name: "t".into(),
                    arguments: arguments.into(),
                }],
                model_id: None,
                model_fingerprint: None,
                reasoning_effort: None,
            }),
            ConversationItem::tool_result("rn_1", "ok"),
        ])
        .with_model("claude-sonnet-5");
        let json = serde_json::to_value(build_messages_request(&req)).unwrap();
        let assistant = json["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "assistant")
            .expect("assistant message: {json:#}");
        assistant["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["type"] == "tool_use")
            .expect("tool_use block: {json:#}")["input"]
            .clone()
    }

    assert_eq!(tool_use_input("[]"), serde_json::json!({}));
    assert_eq!(tool_use_input("\"str\""), serde_json::json!({}));
    assert_eq!(tool_use_input("123"), serde_json::json!({}));
    // The pre-existing unparseable arm keeps its `{}` degradation
    // (xli parity, wire.rs:173 — a warn now accompanies it).
    assert_eq!(tool_use_input("{not json"), serde_json::json!({}));
    // Object-shaped arguments pass through untouched.
    assert_eq!(tool_use_input(r#"{"a":1}"#), serde_json::json!({"a": 1}));
}

/// Provenance: hyper-grok-build@45e984f3 — packages/ai/xai-grok-sampler/src/bedrock.rs :: image_data_uri_is_required_and_preserved (re-expressed; Bedrock wire evidence — softened: the source's hard reject is transport-specific, Anthropic accepts URL sources, so invalid references degrade to a labeled text fallback)
/// A well-formed `data:<media_type>;base64,<data>` URI with a union media
/// type (pin Base64ImageSource.media_type,
/// wirejig/refs/anthropic@d3d5028 messages.ts:204) is preserved exactly —
/// media type extracted, no silent default; http(s) URLs pass through as Url
/// sources; everything else degrades to the text fallback.
#[test]
fn image_data_uri_is_required_and_preserved() {
    fn image_block(url: &str) -> serde_json::Value {
        let req = ConversationRequest::from_items(vec![ConversationItem::User(UserItem {
            content: vec![ContentPart::Image { url: url.into() }],
            ..Default::default()
        })])
        .with_model("claude-sonnet-5");
        serde_json::to_value(build_messages_request(&req)).unwrap()["messages"][0]["content"][0]
            .clone()
    }

    // The source's positive arm: a valid png data URI is preserved.
    let good = image_block("data:image/png;base64,iVBORw0KGgo=");
    assert_eq!(good["type"], "image");
    assert_eq!(good["source"]["type"], "base64");
    assert_eq!(good["source"]["media_type"], "image/png");
    assert_eq!(good["source"]["data"], "iVBORw0KGgo=");

    // Every union media type parses, extracted exactly (no silent default).
    for media_type in ["image/jpeg", "image/png", "image/gif", "image/webp"] {
        let block = image_block(&format!("data:{media_type};base64,AAA="));
        assert_eq!(block["type"], "image", "{media_type}: {block:#}");
        assert_eq!(block["source"]["media_type"], media_type);
        assert_eq!(block["source"]["data"], "AAA=");
    }

    // http(s) URLs pass through (Anthropic accepts URL sources).
    let url = image_block("https://example.com/a.png");
    assert_eq!(url["type"], "image");
    assert_eq!(url["source"]["type"], "url");
    assert_eq!(url["source"]["url"], "https://example.com/a.png");

    // Out-of-union media type degrades to the text fallback (gap 11).
    let out_of_union = image_block("data:image/svg+xml;base64,PHN2Zz4=");
    assert_eq!(out_of_union["type"], "text");
    assert_eq!(
        out_of_union["text"],
        "[invalid image: data:image/svg+xml;base64,PHN2Zz4=]"
    );

    // Malformed data URI (no `;base64,` marker) degrades to the text fallback.
    let malformed = image_block("data:image/png");
    assert_eq!(malformed["type"], "text");
    assert_eq!(malformed["text"], "[invalid image: data:image/png]");
}

/// Fresh-written: R3 unified parse on the tool-result image path (spec §4
/// fresh list). The two pre-MW-2 paths diverged — the tool-result path
/// misclassified non-base64 `data:` URIs as Url sources and accepted
/// out-of-union media types; one helper now serves both user content and
/// tool-result images.
#[test]
fn tool_result_images_use_unified_source_parse() {
    let req = ConversationRequest::from_items(vec![
        assistant_with_calls(&[("tr_1", "t")]),
        ConversationItem::ToolResult(ToolResultItem {
            tool_call_id: "tr_1".to_string(),
            content: "output".into(),
            images: vec![
                ContentPart::Image {
                    url: "data:image/jpeg;base64,AAA=".into(),
                },
                ContentPart::Image {
                    url: "data:image/svg+xml;base64,PHN2Zz4=".into(),
                },
                ContentPart::Image {
                    url: "https://example.com/pic.png".into(),
                },
            ],
        }),
    ])
    .with_model("claude-sonnet-5");

    let json = serde_json::to_value(build_messages_request(&req)).unwrap();
    let tr_block = &json["messages"][2]["content"][0];
    assert_eq!(tr_block["type"], "tool_result");
    let inner = tr_block["content"].as_array().unwrap();
    assert_eq!(inner[0]["type"], "text");
    assert_eq!(inner[1]["type"], "image");
    assert_eq!(inner[1]["source"]["type"], "base64");
    assert_eq!(inner[1]["source"]["media_type"], "image/jpeg");
    assert_eq!(inner[2]["type"], "text");
    assert_eq!(
        inner[2]["text"],
        "[invalid image: data:image/svg+xml;base64,PHN2Zz4=]"
    );
    assert_eq!(inner[3]["type"], "image");
    assert_eq!(inner[3]["source"]["type"], "url");
    assert_eq!(inner[3]["source"]["url"], "https://example.com/pic.png");
}

/// Fresh-written: R3 gap-11 recorded note — the unknown-scheme fallback text
/// changes from `"[image: …]"` to `"[invalid image: …]"` so every degraded
/// image reference carries the same invalid label.
#[test]
fn unknown_scheme_image_fallback_text_is_invalid_label() {
    let req = ConversationRequest::from_items(vec![ConversationItem::User(UserItem {
        content: vec![ContentPart::Image {
            url: "ftp://example.com/x.png".into(),
        }],
        ..Default::default()
    })])
    .with_model("claude-sonnet-5");

    let json = serde_json::to_value(build_messages_request(&req)).unwrap();
    let block = &json["messages"][0]["content"][0];
    assert_eq!(block["type"], "text");
    assert_eq!(block["text"], "[invalid image: ftp://example.com/x.png]");
}

// ============================================================================
// (h) R8 — model-identity thinking invalidation (MW-2)
// ============================================================================

fn r8_fixture(model_id: Option<&str>, request_model: Option<&str>) -> ConversationRequest {
    let mut req = ConversationRequest::from_items(vec![
        ConversationItem::user("read the file"),
        ConversationItem::Assistant(AssistantItem {
            content: "portable answer".into(),
            tool_calls: vec![ToolCall {
                id: "rt_1".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"x"}"#.into(),
            }],
            model_id: model_id.map(str::to_string),
            model_fingerprint: None,
            reasoning_effort: None,
        }),
        reasoning_sibling("rs_1", "thinking text", Some("sig_rt_1")),
        ConversationItem::tool_result("rt_1", "file contents"),
    ]);
    req.model = request_model.map(str::to_string);
    req
}

fn r8_assistant_blocks(req: &ConversationRequest) -> Vec<serde_json::Value> {
    let json = serde_json::to_value(build_messages_request(req)).unwrap();
    let messages = json["messages"].as_array().unwrap();
    let assistant = messages
        .iter()
        .find(|m| m["role"] == "assistant")
        .expect("assistant message: {json:#}");
    assistant["content"].as_array().unwrap().clone()
}

/// Provenance: hyper-grok-build@45e984f3 — packages/ai/xai-grok-sampler/src/pi_messages.rs :: identity_mismatch_falls_back_to_portable_text_and_tool_calls (re-expressed; Pi wire evidence — endpoint identity mapped to model-id identity: grok has no endpoint-identity field, and every messages row resolves to the single proxy endpoint, so the catalog slug IS the identity)
/// An assistant item stamped with a model_id different from the request's
/// model slug replays its portable text + tool calls but NOT its signed
/// thinking: the sibling Reasoning item's Thinking block is suppressed at
/// translation time (R8's flag). Without R8 the signed pair would survive
/// the MW-1 latest-assistant strip (verbatim pair) and serialize.
#[test]
fn identity_mismatch_falls_back_to_portable_text_and_tool_calls() {
    let req = r8_fixture(Some("claude-opus-9"), Some("claude-sonnet-5"));
    let blocks = r8_assistant_blocks(&req);
    assert!(
        blocks
            .iter()
            .any(|b| b["type"] == "text" && b["text"] == "portable answer"),
        "portable text is retained: {blocks:?}"
    );
    assert!(
        blocks.iter().any(|b| b["type"] == "tool_use"),
        "tool calls are retained: {blocks:?}"
    );
    assert!(
        blocks.iter().all(|b| b["type"] != "thinking"),
        "mismatched-model thinking is suppressed: {blocks:?}"
    );
}

/// Fresh-written: R8 negative arm — a MATCHED model_id (or the request's own
/// slug) leaves MW-1's rules unchanged: the signed sibling Reasoning still
/// survives the latest-assistant strip as the verbatim (text, signature)
/// pair.
#[test]
fn r8_matched_model_thinking_unaffected() {
    let req = r8_fixture(Some("claude-sonnet-5"), Some("claude-sonnet-5"));
    let blocks = r8_assistant_blocks(&req);
    let thinking = blocks
        .iter()
        .find(|b| b["type"] == "thinking")
        .expect("matched-model thinking survives: {blocks:?}");
    assert_eq!(thinking["thinking"], "thinking text");
    assert_eq!(thinking["signature"], "sig_rt_1");
}

/// Fresh-written: R8 negative arm — `model_id: None` items are untouched by
/// the identity rule (no identity to compare).
#[test]
fn r8_none_model_id_unaffected() {
    let req = r8_fixture(None, Some("claude-sonnet-5"));
    let blocks = r8_assistant_blocks(&req);
    assert!(
        blocks.iter().any(|b| b["type"] == "thinking"),
        "None model_id: signed thinking survives: {blocks:?}"
    );
}

/// Fresh-written: R8 no-op guard — a None/empty request model makes the whole
/// rule a no-op: even a mismatched-looking model_id must NOT strip signed
/// thinking (a None-model request is invalid anyway, and the guard must not
/// silently destroy verbatim reasoning).
#[test]
fn r8_none_request_model_is_noop() {
    let req = r8_fixture(Some("claude-opus-9"), None);
    let blocks = r8_assistant_blocks(&req);
    assert!(
        blocks.iter().any(|b| b["type"] == "thinking"),
        "None request model: the suppression flag must be inert: {blocks:?}"
    );
}

// ============================================================================
// (i) MW-2 D6 — golden G
// ============================================================================

/// The MW-2 D6 fixture (spec D6, revised per gap 7): exercises R1+R2+R3+R7+R8
/// in one body (R4 is response-side — its own deserialize tests, not this
/// golden). user(image) → mismatched-model assistant + tool_call with
/// parseable non-object arguments `[]` → signed sibling Reasoning (suppressed)
/// → compaction carrier (raw sentinel never surfaces) → tool_result → user.
/// claude-sonnet-5; effort=None; max_tokens SET.
fn fixture_g() -> ConversationRequest {
    ConversationRequest {
        items: vec![
            ConversationItem::User(UserItem {
                content: vec![
                    ContentPart::Text {
                        text: "what's in the file?".into(),
                    },
                    ContentPart::Image {
                        url: "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==".into(),
                    },
                ],
                ..Default::default()
            }),
            ConversationItem::Assistant(AssistantItem {
                content: "let me check".into(),
                tool_calls: vec![ToolCall {
                    id: "call_g1".into(),
                    name: "read_file".into(),
                    arguments: "[]".into(), // parseable NON-object: R2's {} coercion is in the bytes
                }],
                model_id: Some("claude-opus-9".into()), // mismatched: R8 suppresses the sibling thinking
                model_fingerprint: None,
                reasoning_effort: None,
            }),
            reasoning_sibling("rs_g1", "weighing how to read it", Some("sig_g1")),
            ConversationItem::BackendToolCall(BackendToolCallItem {
                kind: BackendToolKind::CodexRawInput(CodexRawInputItem {
                    id: "cmp_g1".to_string(),
                    raw: serde_json::json!({
                        "type": "compaction",
                        "id": "cmp_g1",
                        "encrypted_content": "RAW-SENTINEL-BLOB-000"
                    }),
                    cross_provider_fallback: None,
                }),
            }),
            ConversationItem::tool_result("call_g1", "fn main() {}"),
            ConversationItem::user("summarize it"),
        ],
        model: Some("claude-sonnet-5".to_string()),
        reasoning_effort: None,
        max_output_tokens: Some(4096),
        ..Default::default()
    }
}

/// Fresh-written: MW-2 spec D6 (gap 7) — checked-in golden of
/// `serde_json::to_string(&MessagesRequest)` for fixture G (R1+R2+R3+R7+R8 in
/// one body; R4 is response-side and out of this golden). Re-baselining is
/// allowed only on a pipeline-stage change, disclosed in the commit body.
#[test]
fn d6_golden_g_serialization_holds() {
    let json = serde_json::to_string(&build_messages_request(&fixture_g())).unwrap();
    let golden = include_str!("../../testdata/messages_golden_g.json");
    assert_eq!(
        json, golden,
        "MW-2 D6 golden G drifted. Re-baselining is allowed only when a
         pipeline stage changes (spec D6); every such change must be
         disclosed in the commit body."
    );
}

/// Fresh-written: MW-2 spec D6 (gap 7) + A4 — fixture-G invariants: the SET
/// max_tokens carries the request value (D4 ruling: explicit budget), R1 merge
/// → 3 messages, R7 sentinel absent + safe label present, R8 thinking absent,
/// R2 parseable non-object args in the bytes, R3 base64 preserved, MW-1 rules
/// 2/3 + the hoist invariant.
#[test]
fn d6_golden_g_invariants() {
    let req = fixture_g();
    // D6 invariants
    assert_eq!(req.model.as_deref(), Some("claude-sonnet-5"));
    assert!(req.reasoning_effort.is_none());
    assert_eq!(req.max_output_tokens, Some(4096), "G has max_tokens SET");

    let req_built = build_messages_request(&req);
    let json = serde_json::to_string(&req_built).unwrap();

    assert_eq!(
        req_built.max_tokens, 4096,
        "max_tokens carries the request value"
    );
    assert_eq!(
        req_built.messages.len(),
        3,
        "R1 merge: user / assistant / merged user — {req_built:?}"
    );
    assert!(
        !json.contains("RAW-SENTINEL-BLOB-000"),
        "R7: the carrier's raw payload must never surface in the body"
    );
    assert!(
        json.contains("[OpenAI compacted context]"),
        "R7: the compaction carrier renders its safe fallback label"
    );
    assert!(
        !json.contains("\"thinking\""),
        "R8: the mismatched-model sibling thinking must be absent"
    );
    assert!(
        json.contains("\"input\":{}"),
        "R2: the parseable non-object arguments [] must serialize as {{}}"
    );
    assert!(
        json.contains("iVBORw0KGgoAAAANSUhEUg=="),
        "R3: the base64 image data is preserved in the bytes"
    );
    assert_adjacency_invariant(&req_built.messages);
    // MW-1 invariants (A4): no thinking (rule 2), not ending on assistant
    // (rule 3), tool_result hoisted to the front of the merged user message.
    let last = req_built.messages.last().unwrap();
    assert!(
        matches!(last.role, crate::messages::MessageRole::User),
        "G does not end on assistant"
    );
    let merged = blocks_of(last);
    assert!(
        matches!(
            merged.first(),
            Some(crate::messages::ContentBlock::ToolResult { .. })
        ),
        "hoist: the merged user message leads with the tool_result: {merged:?}"
    );
}

// ============================================================================
// (j) MW-2 R5-R7 — item-kind audit + carrier guards
// ============================================================================

/// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/src/wire.rs :: tools_to_anthropic_format (fresh pin, no xli test of this exact shape: the drop IS the xli parity — the catch-all `_ => Vec::new()` at :974, NOT the doc at :885-894 which names web_search; parity basis is the code, gap 12). Pins CURRENT grok behavior per spec R5/D5: hosted tools are dropped from the messages body, silently (pure builder — no error/notice channel).
#[test]
fn hosted_tools_dropped_from_messages_body() {
    let hosted = vec![HostedTool::WebSearch { options: None }];
    let plain = ToolSpec {
        name: "read_file".into(),
        description: Some("read a file".into()),
        parameters: serde_json::json!({ "type": "object" }),
    };

    // Hosted-only request: the `tools` field is absent from the body entirely.
    let hosted_only = ConversationRequest {
        items: vec![ConversationItem::user("find me stuff")],
        model: Some("claude-sonnet-5".to_string()),
        hosted_tools: hosted.clone(),
        ..Default::default()
    };
    let json = serde_json::to_string(&build_messages_request(&hosted_only)).unwrap();
    assert!(
        !json.contains("\"tools\""),
        "R5: hosted-only request carries no tools field: {json}"
    );
    assert!(
        !json.contains("web_search"),
        "R5: the hosted web_search must be absent from the body: {json}"
    );

    // Hosted + plain: the plain tool maps, the hosted one does not.
    let mixed = ConversationRequest {
        items: vec![ConversationItem::user("find me stuff")],
        model: Some("claude-sonnet-5".to_string()),
        hosted_tools: hosted,
        tools: vec![plain],
        ..Default::default()
    };
    let json = serde_json::to_string(&build_messages_request(&mixed)).unwrap();
    assert!(
        json.contains("read_file"),
        "R5: ordinary tools still map 1:1: {json}"
    );
    assert!(
        !json.contains("web_search"),
        "R5: the hosted web_search is dropped beside plain tools: {json}"
    );
}

/// Fresh-written: MW-2 spec R7 carrier guard (P2.1 is our own port — no source
/// equivalent). A `BackendToolCall(CodexRawInput{type: compaction})` item maps
/// to assistant text via `text_summary()`, which returns ONLY the
/// cross-provider fallback or the safe "[OpenAI compacted context]" label — a
/// sentinel embedded in the raw provider payload must never reach the
/// serialized messages body.
#[test]
fn backend_toolcall_compaction_carrier_never_surfaces_raw_sentinel() {
    let req = ConversationRequest {
        items: vec![
            ConversationItem::user("what did we compact?"),
            ConversationItem::BackendToolCall(BackendToolCallItem {
                kind: BackendToolKind::CodexRawInput(CodexRawInputItem {
                    id: "cmp_guard_1".to_string(),
                    raw: serde_json::json!({
                        "type": "compaction",
                        "id": "cmp_guard_1",
                        "encrypted_content": "CARRIER-SENTINEL-BLOB-DEADBEEF"
                    }),
                    cross_provider_fallback: None,
                }),
            }),
        ],
        model: Some("claude-sonnet-5".to_string()),
        ..Default::default()
    };
    let json = serde_json::to_string(&build_messages_request(&req)).unwrap();
    assert!(
        !json.contains("CARRIER-SENTINEL-BLOB-DEADBEEF"),
        "R7: the carrier's raw payload must never surface in the body: {json}"
    );
    assert!(
        json.contains("[OpenAI compacted context]"),
        "R7: the compaction carrier renders its safe fallback label: {json}"
    );
}

/// Fresh-written: MW-2 spec R7 carrier guard (2) — a message-type carrier
/// (`raw.type == "message"`) renders as LABELED plaintext: the retained
/// context text is inlined under the role label, never as raw provider shape.
#[test]
fn backend_toolcall_message_carrier_renders_labeled_plaintext() {
    let req = ConversationRequest {
        items: vec![
            ConversationItem::user("remember that note"),
            ConversationItem::BackendToolCall(BackendToolCallItem {
                kind: BackendToolKind::CodexRawInput(CodexRawInputItem {
                    id: "msg_guard_1".to_string(),
                    raw: serde_json::json!({
                        "type": "message",
                        "role": "user",
                        "content": "deploy freeze on friday"
                    }),
                    cross_provider_fallback: None,
                }),
            }),
        ],
        model: Some("claude-sonnet-5".to_string()),
        ..Default::default()
    };
    let json = serde_json::to_string(&build_messages_request(&req)).unwrap();
    assert!(
        json.contains("[OpenAI retained user context] deploy freeze on friday"),
        "R7: the message carrier renders labeled plaintext: {json}"
    );
    assert!(
        !json.contains("msg_guard_1"),
        "R7: the raw carrier item (id) must not leak into the body: {json}"
    );
}
