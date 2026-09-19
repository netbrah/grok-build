//! AUTHORITY-47B-1 (apex-72c): the 47b per-field authority replay must
//! attribute values that come from a BUNDLED (baked) row to
//! `FieldSource::BundledRow` for keys WITHOUT a prefetched (live) row.
//!
//! Since CATALOG-BAKE-1, bundled rows routinely carry EXPLICIT fields
//! (api_backend, model_family, menus) that the effective resolver uses
//! for offline bindings. The replay sourced such keys from the built-in
//! default (the built-in tier owned non-prefetched entries and no seam
//! ever ran on them), so the STOP-4 debug_asserts fired in DEBUG builds
//! on offline bundled-row bindings (release: silent wrong audit data).
//!
//! Placement: declared from config.rs via `#[path]` (33z
//! config_schema_tests precedent) — config_tests.rs was owned by an
//! in-flight sibling seat at authoring time; the campaign rule forbids
//! interleaving edits there.

use std::num::NonZeroU64;

use indexmap::IndexMap;

use crate::agent::config::{
    bind_messages_wire_model, resolve_model_list, Config, FieldSource, ModelEntry, ModelInfo,
};
use crate::sampling::ApiBackend;

/// A synthetic prefetched (live) row: the remote-hydration 256k cw
/// placeholder (seam-eligible for donor inheritance) and no api_backend
/// (built-in default — seam-eligible for fills). Mirrors the `live_row`
/// helper in catalog_bake_tests.
fn live_row(model_id: &str) -> ModelEntry {
    let mut entry = ModelEntry {
        info: ModelInfo::fallback(model_id),
        mtls_cert_dir: None,
        api_key: None,
        env_key: None,
        auth_provider: None,
        api_base_url: None,
    };
    entry.info.context_window = NonZeroU64::new(256_000).unwrap();
    entry
}

/// (1) Offline bundled row, explicit api_backend: a baked curated
/// gpt-5.x row from default_models.json. No prefetched row, no config
/// row — the effective resolver's answer is the bundled row itself, and
/// the 47b replay must attribute the explicit fields to BundledRow
/// (values read straight off the real resolver output — STOP-4). At
/// HEAD this binding panics the STOP-4 debug_assert in DEBUG builds
/// (replay sourced the built-in default: ChatCompletions vs the bundled
/// Responses) and the BundledRow variant does not exist.
#[test]
fn offline_bundled_row_fields_are_bundled_row_authority() {
    let cfg = Config::default();
    let resolved = resolve_model_list(&cfg, None);
    let oracle = resolved
        .get("gpt-5-codex")
        .expect("gpt-5-codex rides the baked catalog");
    let view = bind_messages_wire_model(&cfg, None, "gpt-5-codex")
        .expect("offline bundled row binds (non-Messages backend passes the strict gate)");

    assert_eq!(view.model, oracle.info.model, "gpt-5-codex model");
    assert_eq!(
        view.api_backend.value, oracle.info.api_backend,
        "gpt-5-codex api_backend value (STOP-4: read off the resolver output)"
    );
    assert_eq!(view.api_backend.value, ApiBackend::Responses, "baked curated responses pin");
    assert!(
        matches!(view.api_backend.source, FieldSource::BundledRow),
        "offline explicit bundled api_backend must be BundledRow, got: {:?}",
        view.api_backend.source
    );
    assert_eq!(
        view.context_window.value, oracle.info.context_window,
        "gpt-5-codex context_window value (STOP-4)"
    );
    assert!(
        matches!(view.context_window.source, FieldSource::BundledRow),
        "offline bundled cw must be BundledRow, got: {:?}",
        view.context_window.source
    );
    assert_eq!(
        view.model_family.value.as_deref(),
        oracle.info.model_family.as_deref(),
        "gpt-5-codex model_family value (STOP-4)"
    );
    assert!(
        matches!(view.model_family.source, FieldSource::BundledRow),
        "offline explicit bundled model_family must be BundledRow, got: {:?}",
        view.model_family.source
    );
    // kb6 (CATALOG-REQUIRED-CURATION-1): the fill now curates gpt-5-codex's
    // menu in the overlay, so the bundled row arrives menu-FULL — the
    // explicit field carries BundledRow authority (semantics unchanged:
    // explicit bundled fields are BundledRow; the attribution now simply
    // reflects a menu-full row).
    assert_eq!(
        view.reasoning_efforts.value, oracle.info.reasoning_efforts,
        "gpt-5-codex reasoning_efforts value (STOP-4)"
    );
    assert!(
        matches!(view.reasoning_efforts.source, FieldSource::BundledRow),
        "kb6 menu-full bundled row must be BundledRow, got: {:?}",
        view.reasoning_efforts.source
    );

    // CatalogInference menu-empty coverage PRESERVED (kb6): gpt-4.1 stays
    // menu-[] (A1), so the empty-menu slug-inference attribution path is
    // still exercised — on a row of the same (responses) wire.
    let gpt41 = resolved
        .get("gpt-4.1")
        .expect("gpt-4.1 rides the baked catalog");
    let view41 = bind_messages_wire_model(&cfg, None, "gpt-4.1")
        .expect("gpt-4.1 offline bundled row binds (responses backend passes the strict gate)");
    assert_eq!(view41.model, gpt41.info.model, "gpt-4.1 model");
    assert_eq!(
        view41.reasoning_efforts.value, gpt41.info.reasoning_efforts,
        "gpt-4.1 reasoning_efforts value (STOP-4)"
    );
    assert!(
        matches!(view41.reasoning_efforts.source, FieldSource::CatalogInference),
        "menu-empty bundled row must stay CatalogInference, got: {:?}",
        view41.reasoning_efforts.source
    );
}

/// (a) The donor contract applies to PREFETCHED rows only — unchanged
/// by the BundledRow tier: a live grok-4.6 row at the 256k hydration
/// placeholder inherits the bundled donor values (Donor tier). OFFLINE,
/// the same pre-bake key is its own bundled row — no donation happens
/// offline, so its explicit fields are BundledRow, not Donor.
#[test]
fn pre_bake_donor_contract_unchanged_by_bundled_row_tier() {
    let cfg = Config::default();
    let mut prefetched = IndexMap::new();
    prefetched.insert("grok-4.6".to_string(), live_row("grok-4.6"));
    let view = bind_messages_wire_model(&cfg, Some(prefetched), "grok-4.6")
        .expect("prefetched grok-4.6 binds");
    assert!(
        matches!(view.context_window.source, FieldSource::Donor),
        "prefetched donor cw inheritance unchanged, got: {:?}",
        view.context_window.source
    );
    assert!(
        matches!(view.api_backend.source, FieldSource::Donor),
        "prefetched donor api_backend inheritance unchanged, got: {:?}",
        view.api_backend.source
    );

    // Offline: the pre-bake row is its own bundled row (seam-exempt, no
    // donation) — its explicit fields carry BundledRow authority.
    let view = bind_messages_wire_model(&cfg, None, "grok-4.6")
        .expect("offline grok-4.6 binds");
    assert_eq!(view.api_backend.value, ApiBackend::Responses);
    assert!(
        matches!(view.api_backend.source, FieldSource::BundledRow),
        "offline pre-bake explicit backend is BundledRow (not Donor), got: {:?}",
        view.api_backend.source
    );
    assert_eq!(view.context_window.value, NonZeroU64::new(500_000).unwrap());
    assert!(
        matches!(view.context_window.source, FieldSource::BundledRow),
        "offline pre-bake cw 500000 is BundledRow, got: {:?}",
        view.context_window.source
    );
}

/// (b) Tier order unchanged: an explicit `[model.<key>]` config row
/// beats the bundled row for the same key (offline, no prefetched
/// row) — Config remains the highest tier of the 6-tier chain.
#[test]
fn config_tier_still_wins_over_the_bundled_row() {
    let toml_src = r#"
[model.gpt-5-codex]
api_backend = "responses"
context_window = 250000
model_family = "codex"
"#;
    let cfg = Config::new_from_toml_cfg(&toml::from_str(toml_src).unwrap())
        .expect("config parses");
    let view = bind_messages_wire_model(&cfg, None, "gpt-5-codex")
        .expect("offline config-row binding");
    assert!(
        matches!(view.api_backend.source, FieldSource::Config),
        "config api_backend wins over the bundled row, got: {:?}",
        view.api_backend.source
    );
    assert_eq!(view.context_window.value, NonZeroU64::new(250_000).unwrap());
    assert!(
        matches!(view.context_window.source, FieldSource::Config),
        "config cw wins over the bundled row, got: {:?}",
        view.context_window.source
    );
    assert!(
        matches!(view.model_family.source, FieldSource::Config),
        "config family wins over the bundled row, got: {:?}",
        view.model_family.source
    );
}
