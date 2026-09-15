//! Per-model knowledge for the Anthropic `/v1/messages` wire (MW-1 spec D4).
//!
//! Pure, slug-keyed resolvers in the [`crate::catalog_wire`] pattern:
//! [`crate::conversation::build_messages_request`] has `req.model`,
//! `req.reasoning_effort`, and `req.max_output_tokens` and consults the
//! per-model table here.
//!
//! **The `max_tokens` table is pin-sourced (MW-3 R5).** The D4 sourcing
//! audit (pre-brief) ruled that a value row may land only from a pinned
//! snapshot of an official `/v1/models` response with the snapshot + SHA
//! recorded in the pin/ledger. MW-3 R5 adopts exactly that: the two pinned
//! proxy snapshots `grok/plans/pins/v1-models-20260912.json` and
//! `grok/plans/pins/v1-model-info-20260912.json` (both sha256-verified at
//! port time) are cross-checked, and ONLY the endpoint-agreement rows are
//! adopted — the 8 slugs where the two endpoints disagree stay `None`
//! (withheld pending the ledger item-11 live verification, 2026-09-12
//! ruling; no side of a divergence is picked). For slugs without a row a
//! no-budget request still serializes `max_tokens: 0` — the observed proxy
//! contract tolerates 0 (pre-MW-1 wire parity; a floor-1 fallback
//! truncated every no-budget turn live, ledger D4 ruling 2026-09-12).
//!
//! The thinking-config table remains empty by design (no pinned source
//! carries per-model thinking values); `messages_thinking_config` therefore
//! still resolves purely from `reasoning_effort`.

use crate::ReasoningEffort;
use crate::messages::{ThinkingConfig, ThinkingDisplay};

/// Defensive floor for an EXPLICIT request budget: a budget the caller set
/// must never serialize as `max_tokens < 1` (pin messages.ts:1830 — a
/// thinking budget must be < max_tokens), hence `budget.max(FLOOR)`. A
/// no-budget request with no table row serializes 0 instead: the live proxy
/// tolerates 0 (pre-MW-1 wire parity) and a floor-1 fallback would
/// guarantee truncation of every no-budget turn (D4 ruling, ledger
/// 2026-09-12 — supersedes the spec v1 floor-for-all fallback).
pub const MESSAGES_MAX_OUTPUT_TOKENS_FLOOR: u32 = 1;

/// Floor for a no-budget request on the Responses wire when neither the
/// request nor the model config carries a budget and the model table has no
/// row: 64K. The Responses wire must never serialize an omitted
/// `max_output_tokens` — an omitted field takes the per-model upstream
/// (proxy/LiteLLM) default, which for claude-opus-5 is exactly 4,096 (probe
/// A, 2026-09-15) and is consumed 100% by default-on reasoning
/// (text_tokens 0), leaving the turn with no usable output.
pub const RESPONSES_DEFAULT_MAX_OUTPUT_TOKENS: u32 = 64_000;

/// The fallback budget for a no-budget conversation request: the alias-aware
/// R5 table row when one exists (claude slugs), else
/// [`RESPONSES_DEFAULT_MAX_OUTPUT_TOKENS`]. Drives the conversation/responses
/// default fills (xai-grok-sampler `apply_conversation_defaults` /
/// `apply_response_defaults`) so the Responses wire always carries an
/// explicit `max_output_tokens`.
pub fn responses_budget_fallback(model_slug: &str) -> u32 {
    messages_max_output_tokens_opt(model_slug)
        .unwrap_or(RESPONSES_DEFAULT_MAX_OUTPUT_TOKENS)
}

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

/// Per-model `max_tokens` cap for the `/v1/messages` wire (D4, R5,
/// alias-aware per ANTHROPIC-WIRE-1 cut 5).
///
/// Returns the pin-sourced table row when one exists (the 9
/// endpoint-agreement rows — see [`per_model_max_output_tokens`]), else
/// the row for the slug's version alias ([`alias_slug`]) — the dotted
/// spellings of the adopted opus/sonnet 4.6–4.8 rows — else `None`. The
/// builder prefers an explicit `req.max_output_tokens` (floored at
/// [`MESSAGES_MAX_OUTPUT_TOKENS_FLOOR`]); a no-budget request falls back
/// to the table row, else warns and serializes `0` (proxy-tolerated,
/// pre-warm wire parity — see the `MESSAGES_MAX_OUTPUT_TOKENS_FLOOR`
/// docs for the ruling).
pub fn messages_max_output_tokens_opt(model_slug: &str) -> Option<u32> {
    per_model_max_output_tokens_resolved(model_slug)
}

// ---------------------------------------------------------------------------
// Per-model table (D4/R5) — see the module docs for the sourcing ruling.
// ---------------------------------------------------------------------------

fn per_model_thinking_config(_slug: &str) -> Option<ThinkingConfig> {
    None
}

/// The R5 `max_tokens` table: the 9 endpoint-agreement rows only.
///
/// Pin-sourced (A2 provenance, sha256-verified against the snapshots at
/// port time; a re-pin must re-derive BOTH hashes and re-run the
/// cross-check before rows change):
/// - `grok/plans/pins/v1-models-20260912.json`
///   sha256 f3284e07b7c905f64d0aafd60269b4aeec09c5da46e68904cd2373f56e8e7bed
/// - `grok/plans/pins/v1-model-info-20260912.json`
///   sha256 b276bb13ffb1f2ac0f42f9ad8a0b1c342d292d3aaa508aa76eaba9787118c5de
///
/// Adoption policy: ONLY the slugs where both endpoints report the same
/// `max_output_tokens` are adopted. The 8 divergent slugs
/// (`claude-opus-4.5`, `claude-opus-4.6`, `claude-opus-4.7`,
/// `claude-opus-4.8`, `claude-opus-4-5`, `claude-sonnet-4.5`,
/// `claude-sonnet-4.6`, `claude-sonnet-4-5` — both endpoints carry them,
/// disagreeing 64000 vs 128000) stay `None`: neither side of a divergence
/// is picked; they are withheld pending the ledger item-11 live
/// verification (2026-09-12 ruling). Unknown slugs return `None` so the
/// builder's 0-fallback wins (proxy-tolerated).
fn per_model_max_output_tokens(slug: &str) -> Option<u32> {
    match slug {
        // 128000 — endpoint agreement (both pins):
        "claude-sonnet-5" | "claude-opus-5" | "claude-opus-4-8" | "claude-opus-4-7"
        | "claude-opus-4-6" | "claude-sonnet-4-6" => Some(128_000),
        // 64000 — endpoint agreement (both pins):
        "claude-haiku-4.5" | "claude-haiku-4-5" | "claude-haiku-4-5-20251001" => Some(64_000),
        // 8 divergent slugs: withheld as None (see fn docs); unknown slugs:
        // None (the builder's 0-fallback wins).
        _ => None,
    }
}

/// ANTHROPIC-WIRE-1 (cut 5): the alias-aware resolver — the raw R5 table
/// row for the slug, else the row for its version alias ([`alias_slug`]).
/// The raw table and its withholding ruling stay untouched: the alias only
/// bridges a spelling to a row the table already adopted; no side of a
/// divergence is picked.
fn per_model_max_output_tokens_resolved(slug: &str) -> Option<u32> {
    per_model_max_output_tokens(slug)
        .or_else(|| alias_slug(slug).and_then(|alias| per_model_max_output_tokens(&alias)))
}

/// ANTHROPIC-WIRE-1 (cut 5): qwen-code `tokenLimits.ts:162`-style version
/// alias for claude slugs (behavioral reference, not copied). Lowercases,
/// strips the `claude-` prefix, and normalizes the major.minor version
/// segment so dotted and hyphenated spellings cross-resolve:
/// `claude-opus-4.6` → `claude-opus-4-6`, `claude-opus-4-6` →
/// `claude-opus-4.6`, `claude-haiku-4-5-20251001` →
/// `claude-haiku-4.5-20251001`; a dotted patch folds away
/// (`claude-opus-4.8.0` → `claude-opus-4-8`). `None` when the slug is not
/// a claude slug with an all-digit major.minor segment (e.g.
/// `claude-opus-5`, `gpt-5.6`, unknown shapes).
fn alias_slug(slug: &str) -> Option<String> {
    let lower = slug.to_ascii_lowercase();
    let rest = lower.strip_prefix("claude-")?;
    let digit = rest.find(|c: char| c.is_ascii_digit())?;
    let (family_raw, version) = rest.split_at(digit);
    let family = family_raw.strip_suffix('-')?;
    if family.is_empty() {
        return None;
    }
    let major_len = version.bytes().take_while(|b| b.is_ascii_digit()).count();
    let (major, after) = version.split_at(major_len);
    if major.is_empty() || after.is_empty() {
        return None; // no minor segment (`claude-opus-5`)
    }
    let (sep, tail) = after.split_at(1);
    let minor_len = tail.bytes().take_while(|b| b.is_ascii_digit()).count();
    let (minor, suffix) = tail.split_at(minor_len);
    if minor.is_empty() {
        return None; // separator not followed by an all-digit minor
    }
    match sep {
        "." => {
            // Dotted input: normalize to the hyphen spelling; a dotted
            // patch (`4.5.1`) folds away.
            let patch = if suffix.starts_with('.') { "" } else { suffix };
            Some(format!("claude-{family}-{major}-{minor}{patch}"))
        }
        "-" => Some(format!("claude-{family}-{major}.{minor}{suffix}")),
        _ => None,
    }
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
    fn messages_max_output_tokens_opt_arms() {
        // MW-3 R5 (disclosed amendment of the MW-1 empty-table arm):
        // claude-sonnet-5 now carries a pin-sourced agreement row.
        assert_eq!(
            messages_max_output_tokens_opt("claude-sonnet-5"),
            Some(128_000)
        );
        // Unknown slugs still return None — the builder then serializes a
        // no-budget request as 0 (proxy tolerated; D4 ruling 2026-09-12).
        assert_eq!(messages_max_output_tokens_opt("some-unknown-slug"), None);
    }

    /// R5: all 9 endpoint-agreement rows are adopted with the pinned value
    /// (both pins report the same number for each of these slugs).
    ///
    /// Provenance: pinned-snapshot-sourced — grok/plans/pins/v1-models-20260912.json (sha256 f3284e07b7c905f64d0aafd60269b4aeec09c5da46e68904cd2373f56e8e7bed) + grok/plans/pins/v1-model-info-20260912.json (sha256 b276bb13ffb1f2ac0f42f9ad8a0b1c342d292d3aaa508aa76eaba9787118c5de); values adopted from the endpoint-agreement cross-check, no port-source code involved
    #[test]
    fn r5_pinned_agreement_rows_adopted() {
        let rows: &[(&str, u32)] = &[
            ("claude-sonnet-5", 128_000),
            ("claude-opus-5", 128_000),
            ("claude-opus-4-8", 128_000),
            ("claude-opus-4-7", 128_000),
            ("claude-opus-4-6", 128_000),
            ("claude-sonnet-4-6", 128_000),
            ("claude-haiku-4.5", 64_000),
            ("claude-haiku-4-5", 64_000),
            ("claude-haiku-4-5-20251001", 64_000),
        ];
        for (slug, cap) in rows {
            assert_eq!(
                messages_max_output_tokens_opt(slug),
                Some(*cap),
                "agreement row {slug} must adopt the pinned value"
            );
        }
    }

    /// R5: the 8 divergent slugs (both endpoints disagree: 64000 vs 128000)
    /// are WITHHELD as None — no side of a divergence is picked; they await
    /// the ledger item-11 live verification (2026-09-12 ruling).
    ///
    /// Provenance: pinned-snapshot-sourced — same two pins as r5_pinned_agreement_rows_adopted; the divergence set is the complement of the agreement set in the cross-check
    /// ANTHROPIC-WIRE-1 (cut 5): the resolver is ALIAS-AWARE on top of the
    /// intact ruling. The dotted spellings of the 4 adopted opus/sonnet
    /// 4.6–4.8 rows resolve through the qwen-code-style version alias to
    /// their hyphenated agreement rows (128_000); the 4 slugs with no row in
    /// EITHER spelling (the opus/sonnet 4.5 pairs) stay withheld as None —
    /// the builder then warns and serializes the pre-warm 0 fallback.
    #[test]
    fn r5_divergent_slugs_alias_resolution() {
        // Dotted spellings of adopted rows: resolved via alias.
        for slug in [
            "claude-opus-4.6",
            "claude-opus-4.7",
            "claude-opus-4.8",
            "claude-sonnet-4.6",
        ] {
            assert_eq!(
                messages_max_output_tokens_opt(slug),
                Some(128_000),
                "dotted slug {slug} must alias to its adopted row"
            );
        }
        // No row in either spelling: still withheld (None).
        for slug in [
            "claude-opus-4.5",
            "claude-opus-4-5",
            "claude-sonnet-4.5",
            "claude-sonnet-4-5",
        ] {
            assert_eq!(
                messages_max_output_tokens_opt(slug),
                None,
                "slug {slug} has no row in either spelling; the \
                 builder's warn + 0 fallback wins"
            );
        }
    }

    /// ANTHROPIC-WIRE-1 (cut 5): the RAW table keeps the R5 ruling intact —
    /// all 8 divergent slugs stay withheld at the raw layer; the alias only
    /// bridges to already-adopted rows above it. (Passes pre-cut as well:
    /// this pins that the cut did not touch the raw table.)
    #[test]
    fn r5_raw_table_withholds_all_divergent_slugs() {
        let withheld = [
            "claude-opus-4.5",
            "claude-opus-4.6",
            "claude-opus-4.7",
            "claude-opus-4.8",
            "claude-opus-4-5",
            "claude-sonnet-4.5",
            "claude-sonnet-4.6",
            "claude-sonnet-4-5",
        ];
        for slug in withheld {
            assert_eq!(
                per_model_max_output_tokens(slug),
                None,
                "raw divergent slug {slug} must stay withheld (None)"
            );
        }
    }

    /// A2 provenance self-check: both pin sha256s must remain recorded
    /// verbatim in this file (the table fn's doc), so a future edit cannot
    /// silently detach the table from its pins.
    ///
    /// Fresh-written: MW-3 A2 provenance-grep convention (include_str! self-grep — the pin files themselves live outside the worktree and are not include-able)
    #[test]
    fn r5_provenance_hashes_recorded_in_doc() {
        let src = include_str!("messages_model.rs");
        for hash in [
            "f3284e07b7c905f64d0aafd60269b4aeec09c5da46e68904cd2373f56e8e7bed",
            "b276bb13ffb1f2ac0f42f9ad8a0b1c342d292d3aaa508aa76eaba9787118c5de",
        ] {
            assert!(
                src.contains(hash),
                "both pin sha256s must stay verbatim in messages_model.rs"
            );
        }
    }

    /// ANTHROPIC-WIRE-1 (cut 5): `alias_slug` shape — qwen-code
    /// tokenLimits.ts:162-style normalization (behavioral reference, not
    /// copied): dotted and hyphenated spellings cross-resolve, a dotted
    /// patch folds away, and non-claude / no-minor / unknown shapes have
    /// no alias.
    #[test]
    fn alias_slug_shape() {
        let cases: &[(&str, Option<&str>)] = &[
            ("claude-opus-4.6", Some("claude-opus-4-6")),
            ("claude-opus-4-6", Some("claude-opus-4.6")),
            ("claude-sonnet-4.6", Some("claude-sonnet-4-6")),
            ("claude-haiku-4.5", Some("claude-haiku-4-5")),
            ("claude-haiku-4-5-20251001", Some("claude-haiku-4.5-20251001")),
            ("claude-haiku-4.5-20251001", Some("claude-haiku-4-5-20251001")),
            ("claude-opus-4.8.0", Some("claude-opus-4-8")),
            ("CLAUDE-OPUS-4.6", Some("claude-opus-4-6")),
            ("claude-opus-5", None),
            ("gpt-5.6", None),
            ("some-unknown-slug", None),
        ];
        for (slug, expected) in cases {
            assert_eq!(
                alias_slug(slug).as_deref(),
                *expected,
                "alias_slug({slug})"
            );
        }
    }
}
