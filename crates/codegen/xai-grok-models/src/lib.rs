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
}
