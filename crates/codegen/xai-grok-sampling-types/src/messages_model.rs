//! Per-model knowledge for the Anthropic `/v1/messages` wire (MW-1 spec D4).
//!
//! Pure, slug-keyed resolvers in the [`crate::catalog_wire`] pattern:
//! [`crate::conversation::build_messages_request`] has `req.model`,
//! `req.reasoning_effort`, and `req.max_output_tokens` and consults the
//! per-model table here.
//!
//! **The table is empty by design (D4 sourcing audit, pre-brief).** The
//! normative pin (`wirejig/refs/anthropic @ d3d5028`) carries schema only
//! (models.ts:202-207; messages.ts:1830), no per-model values, and the live
//! proxy catalog is IDs-only. A value row may be added only from a pinned
//! snapshot of an official `/v1/models` response (snapshot + SHA recorded in
//! the pin/ledger) — that lands in MW-3. Until then the private table fns
//! below return `None`, the default arms win, and
//! [`messages_max_output_tokens`] returns [`MESSAGES_MAX_OUTPUT_TOKENS_FLOOR`].
//! Filling a row is a one-line `Some(...)` inside the private table fn.

use crate::ReasoningEffort;
use crate::messages::{ThinkingConfig, ThinkingDisplay};

/// A `/v1/messages` request must never serialize `max_tokens < 1` (pin
/// messages.ts:1830 — a thinking budget must be < max_tokens). When the
/// request carries no explicit budget and the per-model table has no row,
/// the floor is what goes on the wire.
pub const MESSAGES_MAX_OUTPUT_TOKENS_FLOOR: u32 = 1;

/// Whether the slug names an Anthropic Claude model.
///
/// Behavioral reference (not copied): xli@3d4a08271e + audited-ledger
/// xli@6d3784158c — codex-rs/provider-anthropic/src/model.rs:47-52. Matching
/// is case-insensitive `contains("claude")`, which also catches
/// provider-proxied slugs (`anthropic/claude-…`) and Vertex AI variants
/// (`claude-…@default`).
pub fn is_anthropic_model(slug: &str) -> bool {
    slug.to_ascii_lowercase().contains("claude")
}

/// Per-model thinking config for the `/v1/messages` wire (D4).
///
/// `Some` when the per-model table pins knowledge for this slug (always-on
/// thinking or a fixed budget); `None` otherwise. With the table empty
/// (MW-1) this is exactly the current behavior: an effort that maps onto
/// the wire yields `Adaptive` with summarized display, and no effort (or
/// `None`/`Minimal`) yields no thinking. Driven by `reasoning_effort` only,
/// not by json_schema.
pub fn messages_thinking_config(
    model_slug: &str,
    reasoning_effort: Option<ReasoningEffort>,
) -> Option<ThinkingConfig> {
    per_model_thinking_config(model_slug).or_else(|| {
        reasoning_effort
            .and_then(|e| e.to_messages_api())
            .map(|_| ThinkingConfig::Adaptive {
                display: Some(ThinkingDisplay::Summarized),
            })
    })
}

/// Per-model `max_tokens` cap for the `/v1/messages` wire (D4).
///
/// Returns the table row when one exists, else
/// [`MESSAGES_MAX_OUTPUT_TOKENS_FLOOR`] (the empty-table fallback). The
/// builder still prefers an explicit `req.max_output_tokens` over this
/// value, floored at [`MESSAGES_MAX_OUTPUT_TOKENS_FLOOR`].
pub fn messages_max_output_tokens(model_slug: &str) -> u32 {
    per_model_max_output_tokens(model_slug).unwrap_or(MESSAGES_MAX_OUTPUT_TOKENS_FLOOR)
}

// ---------------------------------------------------------------------------
// Per-model table (D4) — EMPTY by design until MW-3 lands a pinned
// /v1/models snapshot; see the module docs.
// ---------------------------------------------------------------------------

fn per_model_thinking_config(_slug: &str) -> Option<ThinkingConfig> {
    None
}

fn per_model_max_output_tokens(_slug: &str) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // (d) — suite lettering per MW-1 spec D7. New grok-shape tests for the
    // D4 helper API: known-slug and unknown-slug arms; nothing here was
    // adapted from a named port-source test (xli's model.rs test module
    // covers its own effort/thinking param fns, not is_anthropic_model).

    #[test]
    fn is_anthropic_model_arms() {
        assert!(is_anthropic_model("claude-sonnet-5"));
        assert!(is_anthropic_model("anthropic/claude-sonnet-5"));
        assert!(is_anthropic_model("claude-sonnet-5@default"));
        assert!(!is_anthropic_model("grok-4"));
        assert!(
            is_anthropic_model("ANTHROPIC/CLAUDE-SONNET-5"),
            "matching is case-insensitive"
        );
    }

    #[test]
    fn messages_thinking_config_arms() {
        // Empty table: unknown AND known slugs take the effort-driven
        // default (current behavior, D4).
        let cfg = messages_thinking_config("claude-sonnet-5", Some(ReasoningEffort::High))
            .expect("an effort that maps onto the wire keeps the current Adaptive default");
        assert!(
            matches!(
                cfg,
                ThinkingConfig::Adaptive {
                    display: Some(ThinkingDisplay::Summarized)
                }
            ),
            "{cfg:?}"
        );
        assert!(
            messages_thinking_config("some-unknown-slug", Some(ReasoningEffort::High)).is_some_and(
                |c| {
                    matches!(
                        c,
                        ThinkingConfig::Adaptive {
                            display: Some(ThinkingDisplay::Summarized)
                        }
                    )
                }
            ),
            "unknown slug: behavior unchanged"
        );
        assert!(
            messages_thinking_config("claude-sonnet-5", None).is_none(),
            "no effort: no thinking"
        );
        assert!(
            messages_thinking_config("claude-sonnet-5", Some(ReasoningEffort::Minimal)).is_none(),
            "minimal maps to no thinking on the wire"
        );
    }

    #[test]
    fn messages_max_output_tokens_arms() {
        // Empty table (MW-1): known and unknown slugs both fall to the
        // floor; the floor is the expected outcome, not a failure.
        assert_eq!(
            messages_max_output_tokens("claude-sonnet-5"),
            MESSAGES_MAX_OUTPUT_TOKENS_FLOOR
        );
        assert_eq!(
            messages_max_output_tokens("some-unknown-slug"),
            MESSAGES_MAX_OUTPUT_TOKENS_FLOOR
        );
    }
}
