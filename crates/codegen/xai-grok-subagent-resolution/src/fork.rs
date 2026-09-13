//! Fork directive: how a child's initial context is seeded once the
//! effective child model is known.
//!
//! Provenance: re-expressed from open-grok@240c99c9
//! `crates/codegen/xai-grok-shell/src/agent/subagent/mod.rs:1612-1642`
//! (item 9, MA-1 deliverable 2). The item-provenance helpers at the bottom
//! have no OG counterpart — they exist so the v1 fork path (which pins the
//! child to the parent's current model and therefore cannot compare
//! child-vs-parent model ids) can still detect cross-model history per
//! item.

use xai_grok_sampling_types::ConversationItem;
use xai_tool_types::{SubagentContextMode, SubagentContextRequest};

/// How the child's initial context is seeded, resolved after the effective
/// child model is known: the caller's `context` request beats the child
/// model's catalog default, which beats fresh; a resolved fork then splits
/// on model identity — same model copies the parent's raw items, a
/// different model routes to the plaintext digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkDirective {
    /// Fresh spawn — no inherited history.
    None,
    /// Same-model fork: the parent's raw items are copied (window/tail
    /// guards, summarized `<background_context>` fallback).
    Verbatim,
    /// Cross-model fork: the parent history is rendered into a plaintext
    /// `<forked_context>` digest. Raw items never cross the model boundary.
    Digest,
}

impl ForkDirective {
    /// Resolve the directive from the caller's context request, the child
    /// model's catalog default, and the resolved child/parent model ids.
    pub fn resolve(
        requested: SubagentContextRequest,
        child_model_default: Option<SubagentContextMode>,
        child_model_id: &str,
        parent_model_id: &str,
    ) -> Self {
        let effective = requested.resolve(child_model_default);
        if effective != SubagentContextMode::Fork {
            return Self::None;
        }
        if child_model_id == parent_model_id {
            Self::Verbatim
        } else {
            Self::Digest
        }
    }
}

/// Whether any assistant item in `items` was produced by a model other
/// than the child's canonical or wire model id.
///
/// Items without provenance (`model_id = None`) are unknown, not crossing:
/// a missing marker never by itself pushes a fork onto the digest path.
/// Non-assistant items carry no model provenance and never count.
pub fn fork_crosses_model(
    items: &[ConversationItem],
    child_model_id: &str,
    child_wire_model: &str,
) -> bool {
    items.iter().any(|item| match item {
        ConversationItem::Assistant(assistant) => match &assistant.model_id {
            Some(source) => source != child_model_id && source != child_wire_model,
            None => false,
        },
        _ => false,
    })
}

/// Item-provenance fork directive for the v1 fork path: `Digest` when any
/// parent assistant item came from a different model than the child,
/// `Verbatim` otherwise (the v1 path pins the child to the parent's
/// current model, so a child-vs-parent id comparison is structurally
/// useless there — the items' own provenance is the signal).
pub fn fork_directive_for_items(
    items: &[ConversationItem],
    child_model_id: &str,
    child_wire_model: &str,
) -> ForkDirective {
    if fork_crosses_model(items, child_model_id, child_wire_model) {
        ForkDirective::Digest
    } else {
        ForkDirective::Verbatim
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use xai_grok_sampling_types::conversation::{CodexRawInputItem, ToolResultItem};
    use xai_grok_sampling_types::conversation::{
        BackendToolCallItem, BackendToolKind,
    };

    /// Provenance: open-grok@240c99c9 crates/codegen/xai-grok-shell/src/agent/subagent/tests/mod.rs:1636 :: fork_directive_resolution_precedence_and_model_routing (adapted: ported into the resolution crate; `SubagentContextRequest` now lives in xai-tool-types)
    #[test]
    fn fork_directive_resolution_precedence_and_model_routing() {
        use xai_tool_types::SubagentContextMode as Mode;
        // Explicit fresh always wins, even over a fork model default.
        assert_eq!(
            ForkDirective::resolve(SubagentContextRequest::FRESH, Some(Mode::Fork), "m", "m"),
            ForkDirective::None,
        );
        // Explicit fork ignores a fresh model default.
        assert_eq!(
            ForkDirective::resolve(SubagentContextRequest::FORK, Some(Mode::Fresh), "m", "m"),
            ForkDirective::Verbatim,
        );
        // Default defers to the child model's catalog default.
        assert_eq!(
            ForkDirective::resolve(SubagentContextRequest::Default, Some(Mode::Fork), "m", "m"),
            ForkDirective::Verbatim,
        );
        assert_eq!(
            ForkDirective::resolve(SubagentContextRequest::Default, Some(Mode::Fresh), "m", "m"),
            ForkDirective::None,
        );
        // No default at all falls back to fresh.
        assert_eq!(
            ForkDirective::resolve(SubagentContextRequest::Default, None, "m", "m"),
            ForkDirective::None,
        );
        // A resolved fork across differing models becomes a digest fork.
        assert_eq!(
            ForkDirective::resolve(
                SubagentContextRequest::FORK,
                None,
                "kimi-k3",
                "gpt-5.6-sol"
            ),
            ForkDirective::Digest,
        );
        assert_eq!(
            ForkDirective::resolve(
                SubagentContextRequest::Default,
                Some(Mode::Fork),
                "gpt-5.6-sol",
                "grok-4.6"
            ),
            ForkDirective::Digest,
        );
    }

    /// New (no OG counterpart — the OG check is a child-vs-parent id
    /// comparison, which the item-provenance helper replaces on the v1
    /// path). Asserts the provenance rules: only a foreign `model_id`
    /// crosses; unknown provenance and non-assistant items never do.
    #[test]
    fn fork_crosses_model_flags_only_foreign_provenance() {
        let child = "model-b";

        // No items at all: no crossing.
        assert!(!fork_crosses_model(&[], child, child));

        // User/tool-result items carry no model provenance.
        let no_assistants = vec![
            ConversationItem::user("hello"),
            ConversationItem::ToolResult(ToolResultItem {
                tool_call_id: "tc-1".to_string(),
                content: Arc::from("ok"),
                images: Vec::new(),
            }),
        ];
        assert!(!fork_crosses_model(&no_assistants, child, child));

        // Missing provenance is unknown, not crossing.
        let unknown = vec![ConversationItem::assistant("no marker")];
        assert!(!fork_crosses_model(&unknown, child, child));

        // Either child id (canonical or wire) is "ours".
        let own_canonical = vec![ConversationItem::assistant_with_model(
            "same model",
            child,
        )];
        assert!(!fork_crosses_model(&own_canonical, child, "wire-b"));
        let own_wire =
            vec![ConversationItem::assistant_with_model("same wire", "wire-b")];
        assert!(!fork_crosses_model(&own_wire, "canonical-b", "wire-b"));

        // A foreign provenance crosses even if some items are ours.
        let mixed = vec![
            ConversationItem::assistant_with_model("same", "wire-b"),
            ConversationItem::user("in between"),
            ConversationItem::assistant_with_model("foreign", "model-a"),
        ];
        assert!(fork_crosses_model(&mixed, "canonical-b", "wire-b"));
    }

    /// New (no OG counterpart). The item-provenance directive maps crossing
    /// history to `Digest` and everything else to `Verbatim`.
    #[test]
    fn fork_directive_for_items_digests_only_cross_model_history() {
        let child = "model-b";
        let same_model = vec![
            ConversationItem::system("sys"),
            ConversationItem::assistant_with_model("a", "model-b"),
            ConversationItem::assistant("unmarked"),
        ];
        assert_eq!(
            fork_directive_for_items(&same_model, child, child),
            ForkDirective::Verbatim
        );

        let cross_model = vec![
            ConversationItem::assistant_with_model("a", "model-b"),
            ConversationItem::BackendToolCall(BackendToolCallItem {
                kind: BackendToolKind::CodexRawInput(CodexRawInputItem {
                    id: "raw-1".to_string(),
                    raw: serde_json::json!({"type": "compaction"}),
                    cross_provider_fallback: None,
                }),
            }),
            ConversationItem::assistant_with_model("b", "model-a"),
        ];
        assert_eq!(
            fork_directive_for_items(&cross_model, child, child),
            ForkDirective::Digest
        );
    }
}
