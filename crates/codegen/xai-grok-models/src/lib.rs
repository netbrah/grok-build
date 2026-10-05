//! Default models loaded from `default_models.json` at runtime.
//! `default_models.json` is the machine-merged catalog (apex-071
//! CATALOG-BAKE-1, D1): the generated proxy capture plus the curated
//! overlay, baked IN PLACE by `scripts/catalog_gate.py` in the upstream
//! shape (role pins + models array). Curate `catalog_overlay.json` —
//! never the merged file.
//!
//! At runtime each model is resolved from the first of these that is set: CLI flag, ENV var, config.toml, remote settings, these defaults.

use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::sync::LazyLock;

use xai_grok_sampling_types::{
    ApiBackend, CompactionAtTokens, CompactionsRemaining, ReasoningEffort, ReasoningEffortOption,
};

/// The raw JSON, embedded at compile time.
/// It is `pub` because `xai_grok_shell::models` re-exports it and `agent::config` reads it.
pub const DEFAULT_MODELS_JSON: &str = include_str!("../default_models.json");

/// Build-time derived param-gate constants (apex-ayl.130
/// ZC-PARAMSCHEMA-GATE-1, P1): `SCHEMA_PROPERTIES`, `REQUIRED_FIELDS`,
/// `EFFORT_VALUES`. Derived at build time from
/// `crates/codegen/xai-grok-shell/config.schema.json` (the single source of
/// truth; the sibling Python gate reads the same schema). Param drift
/// between [`DefaultModelEntry`] and the schema fails the build (see
/// `build.rs`).
///
/// `STRUCT_FIELDS` carries the row surface itself (declaration order) so
/// downstream consumers never re-list the struct fields by hand.
pub mod param_gate;

#[cfg(test)]
mod param_gate_tests;

#[derive(serde::Deserialize)]
struct DefaultModels {
    default: String,
    /// Falls back to `default` if not specified in JSON.
    web_search: Option<String>,
    /// Falls back to `default` if not specified in JSON.
    image_description: Option<String>,
    /// Falls back to `default` if not specified in JSON.
    session_summary: Option<String>,
    models: Vec<DefaultModelEntry>,
}

/// CATALOG-BAKE-1 (apex-071, D2): one row of the embedded catalog — the
/// rich curated fields of `default_models.json`.
///
/// Every field is Option-with-defaults so legacy thin rows
/// (`{"model": "..."}`) still parse. Two generated cap aliases
/// (`max_input_tokens` / `max_output_tokens`, the proxy's
/// `/model_group/info` naming) are accepted so the apex-93d 11-row
/// frontier fixture parses through this same struct; the
/// ModelEntryConfig-named `context_window` / `max_completion_tokens` win
/// when both are present (the shell folds them). The O/H overlay fields
/// beyond the D2 core list (`multi_agent_v2`, `strict_responses_input`,
/// `cache_ttl`, `extra_headers`) ride along because the merged rows carry
/// the full curated catalog — the offline fallback must not lose them.
/// Credential fields are deliberately not modeled: the gate rejects them
/// in the overlay, and the baked catalog must stay credential-free.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct DefaultModelEntry {
    pub id: Option<String>,
    pub model: String,
    pub model_family: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub context_window: Option<NonZeroU64>,
    pub max_completion_tokens: Option<u32>,
    /// Generated alias for `context_window` (proxy naming).
    pub max_input_tokens: Option<u64>,
    /// Generated alias for `max_completion_tokens` (proxy naming); the
    /// shell fold drops values above u32::MAX.
    pub max_output_tokens: Option<u64>,
    pub api_backend: Option<ApiBackend>,
    pub supports_backend_search: Option<bool>,
    pub system_prompt_label: Option<String>,
    pub supports_reasoning_effort: Option<bool>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub reasoning_efforts: Option<Vec<ReasoningEffortOption>>,
    pub auto_compact_threshold_percent: Option<u8>,
    pub compaction_at_tokens: Option<CompactionAtTokens>,
    pub compactions_remaining: Option<CompactionsRemaining>,
    pub multi_agent_v2: Option<bool>,
    pub strict_responses_input: Option<bool>,
    /// Native hosted tool discovery (S3a): row advertises the model-side
    /// `tool_search` contract. Additive; absent = off.
    pub supports_search_tool: Option<bool>,
    /// Responses-lite declaration placement (tools ride a leading
    /// `additional_tools` input item, not top-level `tools`). Additive; absent = off.
    pub use_responses_lite: Option<bool>,
    pub cache_ttl: Option<String>,
    pub extra_headers: Option<BTreeMap<String, String>>,
}

static DEFAULTS: LazyLock<DefaultModels> = LazyLock::new(|| {
    let defaults: DefaultModels = serde_json::from_str(DEFAULT_MODELS_JSON)
        .expect("default_models.json: invalid JSON or missing 'default' field");

    // Baked-in JSON: a mismatch here is a developer error
    let model_ids: Vec<&str> = defaults.models.iter().map(|m| m.model.as_str()).collect();
    assert!(
        model_ids.contains(&defaults.default.as_str()),
        "default_models.json: 'default' is '{}' but 'models' array only has {model_ids:?}",
        defaults.default,
    );

    defaults
});

/// CATALOG-BAKE-1 (apex-071): the curated rows of the embedded catalog
/// (the `models` array). The shell seed path maps these into full
/// `ModelEntryConfig`s, so the offline fallback carries the whole curated
/// catalog (D2).
pub fn default_model_rows() -> &'static [DefaultModelEntry] {
    &DEFAULTS.models
}

/// Primary model for coding tasks and general fallback.
pub fn default_model() -> &'static str {
    &DEFAULTS.default
}

/// Model for web search tool synthesis. Falls back to default model.
pub fn default_web_search_model() -> &'static str {
    DEFAULTS.web_search.as_deref().unwrap_or(&DEFAULTS.default)
}

/// Model for image describe. Falls back to default model.
pub fn default_image_description_model() -> &'static str {
    DEFAULTS
        .image_description
        .as_deref()
        .unwrap_or(&DEFAULTS.default)
}

/// Model for session title generation. Falls back to default model.
pub fn default_session_summary_model() -> &'static str {
    DEFAULTS
        .session_summary
        .as_deref()
        .unwrap_or(&DEFAULTS.default)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// S3a (apex-ayl.142, T2): the two capability flags parse from the
    /// baked catalog; absent-on-row = off by default.
    ///
    /// apex-waj.55 Option B: the bake is the operator-approved 12-row menu,
    /// so the flags are pinned to menu rows. The six non-menu rows spec §2
    /// also names (gpt-5.5, gpt-5.4, gpt-5.4-mini, gpt-5.2, glm-5.2,
    /// qwen3.8-27b) are skip-listed — they carry their flags only where a
    /// config.toml row exists (the fleet config tier), never in the
    /// no-endpoint tier whose whole config surface this baked catalog is.
    #[test]
    fn catalog_flags_parse_and_default_off() {
        let m: DefaultModels = serde_json::from_str(DEFAULT_MODELS_JSON).unwrap();
        let ids_on = |read: fn(&DefaultModelEntry) -> Option<bool>| {
            let mut ids: Vec<&str> = m
                .models
                .iter()
                .filter(|e| read(e) == Some(true))
                // id-less rows fall back to the wire slug, so no row can
                // silently leave the pinned set uncounted.
                .map(|e| e.id.as_deref().unwrap_or(e.model.as_str()))
                .collect();
            ids.sort_unstable();
            ids
        };
        assert_eq!(
            ids_on(|e| e.supports_search_tool),
            ["gpt-5.6-luna", "gpt-5.6-sol", "gpt-5.6-terra"]
        );
        assert_eq!(
            ids_on(|e| e.use_responses_lite),
            [
                "gpt-5.6-luna",
                "gpt-5.6-sol",
                "gpt-5.6-sol-1m",
                "gpt-5.6-terra",
                "gpt-5.6-terra-1m"
            ]
        );
        // the one menu row that curates the flag OFF parses Some(false)
        let off_row = m
            .models
            .iter()
            .find(|e| e.id.as_deref() == Some("grok-4.6"))
            .expect("the grok-4.6 row");
        assert_eq!(off_row.supports_search_tool, Some(false));
        // absent-on-row = off by default: the 1M twin curates
        // `use_responses_lite` but not the search-tool flag, so that flag
        // must parse as absent (the shell maps absent to off). Pinned by id
        // — finding it through `is_none()` would make this unfailable.
        let absent_row = m
            .models
            .iter()
            .find(|e| e.id.as_deref() == Some("gpt-5.6-sol-1m"))
            .expect("the sol 1M twin row");
        assert_eq!(absent_row.supports_search_tool, None);
    }

    /// apex-mrmq deliverable 3 (ruling `map/RULINGS-o1o5.md` §D10 R4): no row of the
    /// BAKED catalog may advertise hosted tool search while the decode seam cannot
    /// map the item that flag invites the provider to send. A premature row curate
    /// must break a test, not a user's turn.
    ///
    /// This is the bake-level view of the invariant and it is taken here because this
    /// is the only place the constant the fleet actually ships is in scope:
    /// [`DEFAULT_MODELS_JSON`] is `include_str!`-baked into the binary at
    /// `crates/codegen/xai-grok-models/src/lib.rs:20`, and `build.rs` only declares
    /// the `cargo:rerun-if-changed=default_models.json` trigger (`build.rs:62`) plus
    /// the unrelated param gate — it does not write the catalog. The sibling
    /// `conversation::responses_tests::no_baked_admitting_row_drives_a_shape_the_decode_seam_refuses`
    /// reads the same file off disk, because a crate cannot depend on its own
    /// dependent; it owns the row-vs-answer-shape attribution, and this test owns the
    /// baked bytes. The row SET itself stays owned by
    /// [`catalog_flags_parse_and_default_off`].
    ///
    /// Non-tautological by construction: nothing here asserts that a match arm
    /// exists. It gates on the row set being non-empty (an all-off bake has nothing
    /// to protect) and then calls the real
    /// [`xai_grok_sampling_types::response_to_conversation_items`] — the same function
    /// the production caller invokes at
    /// `xai-grok-sampler/src/stream/responses.rs:821` — on a provider-shaped
    /// discovery pair and requires BOTH halves to land in the IR. Revert the mapping
    /// to a refusal and this reddens by name.
    #[test]
    fn no_baked_admitting_row_outlives_the_decode_seam() {
        let m: DefaultModels = serde_json::from_str(DEFAULT_MODELS_JSON).unwrap();
        let admitting: Vec<&str> = m
            .models
            .iter()
            .filter(|e| e.supports_search_tool == Some(true))
            // id-less rows fall back to the wire slug, as in
            // `catalog_flags_parse_and_default_off`, so no row escapes the check.
            .map(|e| e.id.as_deref().unwrap_or(e.model.as_str()))
            .collect();
        if admitting.is_empty() {
            // Nothing advertised, so there is nothing the seam can be behind.
            return;
        }

        // The server-executed pair exactly as a response body returned it: both
        // halves carry `"call_id":null` and `created_by`, the shape 8 of 8 discovery
        // items in
        // `plans/harness/hosted-tool-search/captures/2026-09-25-wire-grounding/`
        // present (`wire_resp_20260925T062640Z_R1_SOL_HOSTED.json` `output[1]` +
        // `output[2]`, verbatim below). Not a hand-wave: a fixture that omitted keys
        // the provider sends would not be the item the row actually gets back.
        const CALL: &str = r#"{"id":"tsc_0ce980d5c6afd41f016ab61423e6ec81908939d7d041618fb1","type":"tool_search_call","status":"completed","arguments":{"paths":["lookup_shipping_eta"]},"call_id":null,"execution":"server","created_by":null}"#;
        const OUTPUT: &str = r#"{"id":"tso_0ce980d5c6afd41f016ab61424031481908982e2c788dcc429","type":"tool_search_output","status":"completed","call_id":null,"execution":"server","created_by":null,"tools":[{"type":"function","name":"lookup_shipping_eta","description":"Look up the shipping ETA for an order ID.","defer_loading":true,"strict":true,"output_schema":null,"allowed_callers":null,"parameters":{"type":"object","properties":{"order_id":{"type":"string"}},"required":["order_id"],"additionalProperties":false}}]}"#;

        let output: Vec<serde_json::Value> = [CALL, OUTPUT]
            .into_iter()
            .map(|raw| serde_json::from_str(raw).expect("discovery fixture is valid JSON"))
            .collect();
        let response = serde_json::from_value(serde_json::json!({
            "id": "resp_mrmq_bake",
            "object": "response",
            "created_at": 0u64,
            "status": "completed",
            "model": "gpt-5.6-sol",
            "output": output,
        }))
        .expect("async-openai models this response shape");

        let items = xai_grok_sampling_types::response_to_conversation_items(response)
            .unwrap_or_else(|error| {
                panic!(
                    "the bake carries supports_search_tool: true on {admitting:?}, but the decode \
                     seam refused the discovery pair that flag makes the provider send: {error}. \
                     Land the mapping, or apply D10 R3 and curate those rows off."
                )
            });
        let kinds: Vec<&str> = items
            .iter()
            .filter_map(xai_grok_sampling_types::ConversationItem::discovery)
            .map(|carrier| carrier.kind().item_type())
            .collect();
        assert_eq!(
            kinds,
            ["tool_search_call", "tool_search_output"],
            "{admitting:?} advertise hosted search, so both halves of the provider's answer \
             must reach the IR — a row that gets one half kills the turn at the decode seam \
             instead of this test"
        );
    }
}
