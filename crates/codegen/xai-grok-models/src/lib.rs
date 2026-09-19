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
