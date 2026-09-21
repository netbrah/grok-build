//! F12 (apex-ayl.116) shell-side unit tests: the TTL split in the `usage.json` artifact.
//!
//! The sibling `usage_file_tests.rs` stays untouched (compaction lane owns it), so the
//! F12 artifact pins live in this new file (SDD §0.2 / §7): the wire-echo serialization
//! names (the exact `MGW-TTLUSAGE-01` artifact-grep needles) and the pre-cut-shape
//! backward-compat deserialization.

use super::*;

#[test]
fn usage_summary_serializes_wire_echo_ttl_names() {
    // U-RED-3 (F12 §4): the artifact renders the wire-echo split keys with values —
    // no camelCase, no skip-when-zero (the "0" is the informative value).
    let totals = xai_chat_state::UsageTotals {
        input_tokens: 1000,
        output_tokens: 7,
        cached_read_tokens: 0,
        cache_creation_tokens: 460,
        cache_creation_5m_input_tokens: 120,
        cache_creation_1h_input_tokens: 340,
        reasoning_tokens: 0,
        model_calls: 1,
        api_duration_ms: 0,
        cost_usd_ticks: None,
        cost_missing_calls: 1,
    };
    let summary = UsageSummary::from_totals(&totals, false);
    let json = serde_json::to_string(&summary).expect("UsageSummary serializes");
    assert!(
        json.contains("\"ephemeral_5m_input_tokens\":120"),
        "artifact must carry the 5m wire-echo key with value: {json}"
    );
    assert!(
        json.contains("\"ephemeral_1h_input_tokens\":340"),
        "artifact must carry the 1h wire-echo key with value: {json}"
    );
}

#[test]
fn pre_cut_usage_json_shape_deserializes_with_zero_split() {
    // U-PARITY-1, JSON half (F12 §4/§5): a PRE-CUT usage.json shape (no split keys)
    // loads with split 0s — the two fields are serde-default, so old artifacts
    // written before this cut stay readable.
    let pre_cut = r#"{
        "inputTokens": 1000,
        "outputTokens": 7,
        "cachedReadTokens": 0,
        "cacheCreationTokens": 460,
        "reasoningTokens": 0,
        "totalTokens": 1007,
        "modelCalls": 1
    }"#;
    let summary: UsageSummary = serde_json::from_str(pre_cut).expect("pre-cut usage.json shape loads");
    assert_eq!(summary.cache_creation_tokens, 460);
    assert_eq!(summary.cache_creation_5m_input_tokens, 0);
    assert_eq!(summary.cache_creation_1h_input_tokens, 0);
}
