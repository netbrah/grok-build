//! CATALOG-HYDRATE-1 (apex-93d): reduced redacted 11-row frontier fixture.
//!
//! Source: `grok/plans/model/data/catalog-digest.json` (captured 2026-09-18,
//! `/model_group/info`, `.models[id].group`, `per_model_errors: []`) — the
//! comb doc `config-row-comb-20260918.md` section 4 frontier matrix, 11 rows.
//! Digest-keyed (drift baseline for apex-6mz); redacted to
//! id/max_input_tokens/max_output_tokens/supports_reasoning/
//! supported_reasoning_efforts/providers — no endpoints, keys, or costs.
//!
//! The parse test maps each row onto a synthetic `/v1/models` body and feeds
//! `parse_remote_model_value` — proving the fixture flows through the new
//! provenance fields and the (byte-identical) fold.
const FIXTURE: &str = include_str!("../../tests/fixtures/catalog-frontier-11-20260918.json");

const FRONTIER_IDS: &[&str] = &[
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "qwen3.8-27b",
    "glm-5.2",
    "claude-sonnet-5",
    "claude-opus-5",
    "claude-haiku-4-5",
    "grok-4.6",
    "gemini-3.1-pro-preview",
    "gemini-3-pro-preview",
];

fn fixture_rows() -> (serde_json::Value, Vec<serde_json::Value>) {
    let doc: serde_json::Value = serde_json::from_str(FIXTURE)
        .expect("frontier fixture is valid JSON (committed artifact)");
    let rows = doc["models"]
        .as_array()
        .cloned()
        .expect("fixture must carry a 'models' array");
    (doc, rows)
}

#[test]
fn fixture_is_the_eleven_frontier_rows() {
    let (doc, rows) = fixture_rows();
    assert_eq!(rows.len(), 11, "the frontier matrix has 11 rows");
    let ids: Vec<&str> = rows.iter().map(|row| row["id"].as_str().unwrap()).collect();
    assert_eq!(ids, FRONTIER_IDS, "row order and ids must match the comb section 4 matrix");
    for row in &rows {
        // Every frontier row carries both caps (the 67/76 population).
        assert!(row["max_input_tokens"].is_u64(), "{}: max_input_tokens present", row["id"]);
        assert!(row["max_output_tokens"].is_u64(), "{}: max_output_tokens present", row["id"]);
        assert!(row["supports_reasoning"].is_boolean(), "{}: flag present", row["id"]);
        assert!(
            row["supported_reasoning_efforts"].is_array() || row["supported_reasoning_efforts"].is_null(),
            "{}: menu present (array) or null",
            row["id"]
        );
        assert!(row["providers"].as_array().is_some_and(|p| !p.is_empty()), "{}: providers", row["id"]);
    }
    assert!(
        doc["source"].as_str().is_some() && doc["bead"].as_str() == Some("apex-93d CATALOG-HYDRATE-1"),
        "provenance header must name the digest source and the bead"
    );
    // Redaction: no endpoints, keys, or costs anywhere in the fixture.
    let raw = FIXTURE.to_lowercase();
    for banned in ["http://", "https://", "api_key", "apikey", "sk-", "cost"] {
        assert!(!raw.contains(banned), "fixture must not contain {banned:?}");
    }
}

#[test]
fn fixture_rows_parse_through_provenance_fields() {
    let (_doc, rows) = fixture_rows();
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        // Row -> synthetic /v1/models body (the endpoint the harness fetches
        // every launch; ANTHROPIC-WIRE-2 cut 6 key spellings).
        let feed_body = serde_json::json!({
            "id": id,
            "object": "model",
            "max_input_tokens": row["max_input_tokens"],
            "max_output_tokens": row["max_output_tokens"],
        });
        let parsed = crate::remote::client::parse_remote_model_value(
            &feed_body,
            "https://default.url",
            &indexmap::IndexMap::new(),
        )
        .unwrap_or_else(|| panic!("{id}: fixture row must parse"));
        // Provenance: raw feed values preserved, named:
        assert_eq!(
            parsed.feed_max_input_tokens,
            row["max_input_tokens"].as_u64(),
            "{id}: feed input provenance"
        );
        assert_eq!(
            parsed.feed_max_output_tokens,
            row["max_output_tokens"].as_u64(),
            "{id}: feed output provenance"
        );
        // Fold (pre-cut mapping, byte-identical):
        assert_eq!(
            parsed.context_window.get(),
            row["max_input_tokens"].as_u64().expect("fixture caps are u64"),
            "{id}: max_input_tokens must land on context_window"
        );
        assert_eq!(
            parsed.max_completion_tokens,
            u32::try_from(row["max_output_tokens"].as_u64().unwrap()).ok(),
            "{id}: max_output_tokens must land on max_completion_tokens"
        );
    }
}

#[test]
fn fixture_spot_values_match_catalog_truth() {
    let (_doc, rows) = fixture_rows();
    let get = |id: &str| -> (u64, u64) {
        rows.iter()
            .find(|row| row["id"] == id)
            .map(|row| {
                (
                    row["max_input_tokens"].as_u64().unwrap(),
                    row["max_output_tokens"].as_u64().unwrap(),
                )
            })
            .unwrap_or_else(|| panic!("{id} missing from fixture"))
    };
    // Ground truth from the 2026-09-18 operator review (live
    // models_cache.json + comb section 4):
    assert_eq!(get("gpt-5.6-sol"), (922_000, 128_000));
    assert_eq!(get("gpt-5.6-terra"), (922_000, 128_000));
    assert_eq!(get("gpt-5.6-luna"), (922_000, 128_000));
    assert_eq!(get("qwen3.8-27b"), (262_144, 128_000));
    assert_eq!(get("glm-5.2"), (262_144, 128_000));
    assert_eq!(get("claude-sonnet-5"), (1_000_000, 128_000));
    assert_eq!(get("claude-opus-5"), (1_000_000, 128_000));
    assert_eq!(get("claude-haiku-4-5"), (200_000, 64_000));
    assert_eq!(get("grok-4.6"), (500_000, 500_000));
    assert_eq!(get("gemini-3.1-pro-preview"), (1_048_576, 65_536));
    assert_eq!(get("gemini-3-pro-preview"), (1_048_576, 65_536));
}
