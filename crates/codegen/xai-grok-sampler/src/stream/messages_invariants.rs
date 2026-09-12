//! Stream invariant guards for the Anthropic Messages SSE transform (MW-3 R2).
//!
//! Re-expressed (D1: behavior, not source) from xli@3d4a08271e
//! (`codex-rs/provider-anthropic/src/stream_invariants.rs` + audited-ledger
//! xli@6d3784158c) — the invariant classification of the provider-side
//! Anthropic `/v1/messages` SSE consumer (HI-C5-001..006, -010, HI-C1-004,
//! -012; upstream reference: anthropic-sdk-python `accumulate_event`,
//! _messages.py:362-499). Grok-shaped: these are PURE checks consulted by
//! the transform at open/delta/close/usage; the transform stays the single
//! owner of accumulation (spec D3: no second accumulator — xli's
//! `stream_accumulator.rs` is a check-order reference only).
//!
//! Documented grok divergences from the xli reference:
//! - `UnknownEventType`/`UnknownDeltaSubtype` have no grok arm: spec R1 (D2)
//!   maps both unknown shapes to `Ping` at the serde parse site, so the
//!   transform never observes them. (xli carries them as recoverable
//!   violations on its pre-parse event enum.)
//! - `DuplicateToolCallIndex` is NEW (spec R7 row 18 — no home in xli's set):
//!   a `content_block_start` for an index that already holds an open block.
//!   xli's accumulator silently overwrites; grok warns and keeps the FIRST
//!   block.
//!
//! `is_fatal` split is 1:1 with xli: fatal → `Failed` terminal through the
//! existing error path (actor retry semantics unchanged); recoverable →
//! `tracing::warn!` + continue.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StreamInvariantViolation {
    /// `content_block_stop` for an index that never received a start (xli
    /// `StopForUnknownIndex`) — true wire corruption.
    StopForUnknownIndex { block_index: u32 },
    /// `content_block_delta` for an index that never received a start (xli
    /// `DeltaForUnopenedIndex`) — true wire corruption.
    DeltaForUnopenedIndex {
        block_index: u32,
        delta_type: &'static str,
    },
    /// Provider `error` SSE event mid-stream (xli `InStreamError`).
    InStreamError { payload: String },
    /// `signature_delta` on a thinking block that still carries no thinking
    /// text (xli `SignatureBeforeThinking`).
    SignatureBeforeThinking { block_index: u32 },
    /// A second `signature_delta` for a block that already received one
    /// (xli `DuplicateSignatureDelta`).
    DuplicateSignatureDelta { block_index: u32 },
    /// `content_block_start` for an index that already holds an open block
    /// (NEW, R7 row 18 — xli has no counterpart).
    DuplicateToolCallIndex { block_index: u32 },
    /// A usage counter regressed across `message_start` → `message_delta`
    /// (xli `UsageMonotonicityViolation`; recoverable — R-risk-4 live audit).
    UsageMonotonicityViolation {
        field: &'static str,
        previous: u32,
        incoming: u32,
    },
}

impl StreamInvariantViolation {
    /// Whether the violation fails the stream (`Failed` terminal) or is
    /// warned and recovered. 1:1 with xli `StreamInvariantViolation::is_fatal`.
    pub(crate) fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::StopForUnknownIndex { .. }
                | Self::DeltaForUnopenedIndex { .. }
                | Self::InStreamError { .. }
        )
    }
}

/// Guard a `content_block_delta` before mutating block state
/// (xli `check_content_block_delta`).
pub(crate) fn check_content_block_delta(
    block_index: u32,
    delta_type: &'static str,
    block_open: bool,
) -> Option<StreamInvariantViolation> {
    if !block_open {
        return Some(StreamInvariantViolation::DeltaForUnopenedIndex {
            block_index,
            delta_type,
        });
    }
    None
}

/// Guard a `content_block_stop` before removing block state
/// (xli `check_content_block_stop`).
pub(crate) fn check_content_block_stop(
    block_index: u32,
    block_open: bool,
) -> Option<StreamInvariantViolation> {
    if !block_open {
        return Some(StreamInvariantViolation::StopForUnknownIndex { block_index });
    }
    None
}

/// Guard one usage counter against regression (xli
/// `check_usage_monotonicity`, grok per-field form). A zero or absent
/// incoming value skips the check (xli zero-incoming skip; grok's delta
/// `input_tokens` is `Option` and an absent value skips likewise).
pub(crate) fn check_usage_counter(
    field: &'static str,
    incoming: Option<u32>,
    previous: u32,
) -> Option<StreamInvariantViolation> {
    let Some(incoming) = incoming else {
        return None;
    };
    if incoming > 0 && incoming < previous {
        return Some(StreamInvariantViolation::UsageMonotonicityViolation {
            field,
            previous,
            incoming,
        });
    }
    None
}

/// Guard a `signature_delta` (xli's inline accumulator checks, same order:
/// before-thinking first, then duplicate). Returns the warnings — both are
/// recoverable.
pub(crate) fn check_signature_delta(
    block_index: u32,
    is_thinking_block: bool,
    thinking_empty: bool,
    signature_seen: bool,
) -> Vec<StreamInvariantViolation> {
    let mut out = Vec::new();
    if is_thinking_block && thinking_empty {
        out.push(StreamInvariantViolation::SignatureBeforeThinking { block_index });
    }
    if signature_seen {
        out.push(StreamInvariantViolation::DuplicateSignatureDelta { block_index });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/tests/stream_invariants.rs :: delta_for_unopened_index_is_fatal (re-expressed)
    #[test]
    fn delta_for_unopened_index_is_fatal() {
        let v = check_content_block_delta(0, "thinking_delta", false).unwrap();
        assert_eq!(
            v,
            StreamInvariantViolation::DeltaForUnopenedIndex {
                block_index: 0,
                delta_type: "thinking_delta",
            }
        );
        assert!(v.is_fatal());
    }

    /// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/tests/stream_invariants.rs :: stop_for_unknown_index_is_fatal (re-expressed)
    #[test]
    fn stop_for_unknown_index_is_fatal() {
        let v = check_content_block_stop(1, false).unwrap();
        assert_eq!(
            v,
            StreamInvariantViolation::StopForUnknownIndex { block_index: 1 }
        );
        assert!(v.is_fatal());
    }

    /// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/tests/stream_invariants.rs :: delta_for_opened_index_passes (re-expressed)
    #[test]
    fn delta_for_opened_index_passes() {
        assert!(check_content_block_delta(0, "thinking_delta", true).is_none());
    }

    /// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/tests/stream_invariants.rs :: stop_for_opened_index_passes (re-expressed)
    #[test]
    fn stop_for_opened_index_passes() {
        assert!(check_content_block_stop(0, true).is_none());
    }

    /// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/tests/stream_invariants.rs :: usage_non_monotonic_clamps (re-expressed; grok wire field names, input checked first as xli checks prompt first)
    #[test]
    fn usage_non_monotonic_flags_regressed_field() {
        let v = check_usage_counter("input_tokens", Some(90), 100).unwrap();
        assert_eq!(
            v,
            StreamInvariantViolation::UsageMonotonicityViolation {
                field: "input_tokens",
                previous: 100,
                incoming: 90,
            }
        );
        assert!(!v.is_fatal());
        let v = check_usage_counter("output_tokens", Some(40), 50).unwrap();
        assert!(matches!(
            v,
            StreamInvariantViolation::UsageMonotonicityViolation {
                field: "output_tokens",
                ..
            }
        ));
    }

    /// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/tests/stream_invariants.rs :: usage_monotonic_update_passes (re-expressed)
    #[test]
    fn usage_monotonic_update_passes() {
        assert!(check_usage_counter("input_tokens", Some(110), 100).is_none());
        assert!(check_usage_counter("output_tokens", Some(60), 50).is_none());
    }

    /// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/tests/stream_invariants.rs :: usage_zero_incoming_skips_monotonicity_check (re-expressed; grok adds the absent-Option skip)
    #[test]
    fn usage_zero_or_absent_incoming_skips_monotonicity_check() {
        assert!(check_usage_counter("input_tokens", Some(0), 100).is_none());
        assert!(check_usage_counter("input_tokens", None, 100).is_none());
    }

    /// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/tests/stream_invariants.rs :: in_stream_error_is_fatal (re-expressed)
    #[test]
    fn in_stream_error_is_fatal() {
        let v = StreamInvariantViolation::InStreamError {
            payload: "rate_limit".to_owned(),
        };
        assert!(v.is_fatal());
    }

    /// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/tests/stream_invariants.rs :: signature_before_thinking_is_warn_only (re-expressed)
    #[test]
    fn signature_before_thinking_is_warn_only() {
        let v = StreamInvariantViolation::SignatureBeforeThinking { block_index: 0 };
        assert!(!v.is_fatal());
    }

    /// Provenance: xli@3d4a08271e + audited-ledger xli@6d3784158c — codex-rs/provider-anthropic/tests/stream_invariants.rs :: duplicate_signature_delta_is_warn_only (re-expressed)
    #[test]
    fn duplicate_signature_delta_is_warn_only() {
        let v = StreamInvariantViolation::DuplicateSignatureDelta { block_index: 0 };
        assert!(!v.is_fatal());
    }

    /// Fresh-written: R7 row-18 duplicate-index subcase — NEW recoverable violation with no xli counterpart (xli's accumulator overwrites silently; grok keeps the first block, spec R2 note + R7).
    #[test]
    fn duplicate_tool_call_index_is_recoverable() {
        let v = StreamInvariantViolation::DuplicateToolCallIndex { block_index: 3 };
        assert!(!v.is_fatal());
    }

    /// Fresh-written: signature guard emits both warnings in xli's check order when both conditions hold.
    #[test]
    fn signature_guard_reports_both_conditions_in_order() {
        let v = check_signature_delta(0, true, true, true);
        assert_eq!(
            v,
            vec![
                StreamInvariantViolation::SignatureBeforeThinking { block_index: 0 },
                StreamInvariantViolation::DuplicateSignatureDelta { block_index: 0 },
            ]
        );
        assert!(check_signature_delta(0, true, false, false).is_empty());
        assert!(check_signature_delta(0, false, true, false).is_empty());
    }
}
