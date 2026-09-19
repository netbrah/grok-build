//! CATALOG-BAKE-1 (apex-071): baked catalog (generated + curated overlay)
//! parse + Layer-1 seam tests.
//!
//! v2 (operator-adjudicated 3-delta, 2026-09-19):
//! - D1: the merged catalog bakes IN PLACE into `default_models.json`
//!   (upstream shape: role pins + models array). There is no separate
//!   baked layer anymore — the bundled seed IS the catalog.
//! - D2: the seed path consumes the enriched `xai-grok-models` rows
//!   (rich O/H fields + the generated cap aliases), so the offline
//!   fallback carries the whole curated catalog.
//! - D3: the gate is fail-closed on overlay completeness (python side);
//!   the overlay is the single curated place for wire semantics.
//!
//! What these tests pin:
//! - overlay (O/H) values ride the bundled rows (overlay wins over
//!   generated; the pre-bake seed rows migrate byte-for-value into the
//!   overlay, so the merged pre-bake rows differ from the old seed ONLY
//!   where the operator curation says so);
//! - generated (C) caps survive the merge (generated wins over defaults,
//!   curated overlay beats generated on collision);
//! - donor contract: the PRE_BAKE_SEED_KEYS rows are the ONLY donors for
//!   prefetched (live) rows — the generated additions ride the fallback
//!   but never donate (live fetch still wins when present);
//! - the row-aware seams run at resolution on the non-pre-bake rows only
//!   (pre-bake rows keep exact pre-bake behavior: no fills);
//! - role pins ride the baked file (crate accessors read them);
//! - the 11-row apex-93d frontier fixture (generated naming:
//!   `max_input_tokens`/`max_output_tokens`) parses through the same
//!   enriched crate struct with the ceiling values surviving the fold.

use std::num::NonZeroU64;

use indexmap::IndexMap;

use crate::agent::config::{
    default_model_entries, entry_config_from_default_row, resolve_model_list, Config,
    EndpointsConfig, ModelEntry, ModelInfo, PRE_BAKE_SEED_KEYS,
};
use crate::models::DefaultModelEntry;
use crate::sampling::ApiBackend;

/// 2026-09-19 capture (3-call contract): 76 generated models + the
/// `bake`-listed seed-migration row (grok-4.5). Drift is the gate's job,
/// not a constant bump: a different model count fails the python suite's
/// `generated: 76 models` check at exfil time.
const BUNDLED_ROW_COUNT: usize = 77;

/// A synthetic prefetched (live) row: the proxy omitted `context_window`
/// (the fetch hydration placeholder — seam-eligible for donor inheritance)
/// and no `api_backend` (built-in default — seam-eligible for fills).
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

#[test]
fn bundled_catalog_parses_rich_rows() {
    let entries = default_model_entries(&EndpointsConfig::default());
    assert_eq!(
        entries.len(),
        BUNDLED_ROW_COUNT,
        "one bundled row per generated model + the bake-listed seed row"
    );

    // Overlay (O/H) values ride the rows — overlay wins over generated.
    let sol = &entries["gpt-5.6-sol"].info;
    assert_eq!(sol.api_backend, ApiBackend::Responses);
    assert!(sol.strict_responses_input, "sol: strict responses pin from the overlay");
    assert_eq!(sol.multi_agent_v2, Some(true), "sol: v2 multi-agent gate");
    assert_eq!(sol.model_family.as_deref(), Some("codex"));
    assert_eq!(
        sol.extra_headers.get("x-litellm-tags").map(String::as_str),
        Some("East US 2"),
        "sol: curated deployment tag"
    );
    assert!(!sol.supports_backend_search, "sol: overlay pins backend search off");
    assert_eq!(
        sol.reasoning_efforts.iter().map(|o| o.value.as_str()).collect::<Vec<_>>(),
        ["low", "medium", "high", "max", "ultra"],
        "sol: curated menu wins over the old seed's xhigh-including menu"
    );

    // C-class (CATALOG-CCLASS-SEED-1): the baked cw is the generated
    // (proxy-truth) 922000 — the 071 overlay 353000 leak is gone ...
    assert_eq!(
        sol.context_window,
        NonZeroU64::new(922_000).unwrap(),
        "sol: cw from the generated catalog (C-class: the proxy's truth)"
    );
    // ... while the generated output cap survives (the overlay is
    // C-class-free by design).
    assert_eq!(sol.max_completion_tokens, Some(128_000));
    assert_eq!(sol.name.as_deref(), Some("GPT-5.6 Sol"), "sol: seed name migrated");
    assert_eq!(sol.auto_compact_threshold_percent, Some(80), "sol: seed compaction curation");

    // grok-4.6: the overlay cures what the old seed did not own —
    // supports_backend_search flips true -> false (config.toml rule).
    let grok = &entries["grok-4.6"].info;
    assert!(!grok.supports_backend_search, "grok-4.6: overlay backend_search=false");
    assert_eq!(
        grok.context_window,
        NonZeroU64::new(500_000).unwrap(),
        "grok-4.6: curated cw (seed value)"
    );
    assert_eq!(
        grok.max_completion_tokens,
        Some(500_000),
        "grok-4.6: generated max_output survives"
    );
    assert_eq!(
        grok.reasoning_efforts.iter().map(|o| o.value.as_str()).collect::<Vec<_>>(),
        ["xhigh", "high", "medium", "low"],
        "grok-4.6: seed menu survives the migration (the overlay had none)"
    );

    // Anthropic pinned to messages + 1h cache + family — never left to
    // inference.
    let sonnet5 = &entries["claude-sonnet-5"].info;
    assert_eq!(sonnet5.api_backend, ApiBackend::Messages);
    assert_eq!(sonnet5.cache_ttl.as_deref(), Some("1h"));
    assert_eq!(sonnet5.multi_agent_v2, Some(true));
    for (key, entry) in &entries {
        if key.as_str().starts_with("claude-") {
            assert_eq!(
                entry.info.api_backend,
                ApiBackend::Messages,
                "{key}: every claude row in the catalog is messages-pinned"
            );
            assert_eq!(
                entry.info.model_family.as_deref(),
                Some("anthropic"),
                "{key}: claude rows carry the anthropic family (D3 required field)"
            );
        }
    }

    // Curated legacy row: the explicit pin rides the row (the row-aware
    // seams run at resolution, not parse time — see the fallback test).
    let gpt4 = &entries["gpt-4"].info;
    assert_eq!(gpt4.api_backend, ApiBackend::ChatCompletions);
    assert_eq!(gpt4.model_family.as_deref(), Some("codex"));
    assert_eq!(gpt4.context_window, NonZeroU64::new(1_047_576).unwrap());
    assert_eq!(gpt4.max_completion_tokens, Some(32_768));

    // Capless rows: no max_completion_tokens is invented (embedding
    // families — the proxy serves no max_output for them).
    let ada = &entries["text-embedding-ada-002"].info;
    assert_eq!(ada.max_completion_tokens, None);
    assert_eq!(ada.context_window, NonZeroU64::new(8_191).unwrap());
}

#[test]
fn pre_bake_seed_rows_keep_the_head_donor_contract() {
    let entries = default_model_entries(&EndpointsConfig::default());
    for key in PRE_BAKE_SEED_KEYS {
        assert!(entries.contains_key(key), "{key}: pre-bake key rides the catalog");
    }
    // The donor fields (context_window / api_backend): the grok rows are
    // the pre-bake seed values; sol's cw is the generated (proxy-truth)
    // 922000 post CATALOG-CCLASS-SEED-1. A future curation drift that
    // changes what a pre-bake row donates to a live row must fail here.
    let donor_fields = |key: &str| {
        let info = &entries[key].info;
        (info.api_backend.clone(), info.context_window.get())
    };
    assert_eq!(donor_fields("grok-4.6"), (ApiBackend::Responses, 500_000));
    assert_eq!(donor_fields("grok-4.5"), (ApiBackend::Responses, 500_000));
    assert_eq!(donor_fields("gpt-5.6-sol"), (ApiBackend::Responses, 922_000));

    // grok-4.5 is seed-only (not on the proxy) — it survives via the
    // overlay `bake` list with its full seed-migrated curation.
    let grok45 = &entries["grok-4.5"].info;
    assert_eq!(grok45.context_window, NonZeroU64::new(500_000).unwrap());
    assert_eq!(
        grok45.reasoning_efforts.iter().map(|o| o.value.as_str()).collect::<Vec<_>>(),
        ["high", "medium", "low"],
        "grok-4.5: seed menu survives (the overlay had none)"
    );
    assert_eq!(grok45.auto_compact_threshold_percent, Some(80));
}

#[test]
fn donor_map_is_pre_bake_seed_keys_only() {
    let mut prefetched = IndexMap::new();
    prefetched.insert("gpt-5.6-sol".to_string(), live_row("gpt-5.6-sol"));
    prefetched.insert("claude-opus-4-5".to_string(), live_row("claude-opus-4-5"));
    let resolved = resolve_model_list(&Config::default(), Some(prefetched));

    // The pre-bake key donates the bundled row's values: the live sol row
    // at the hydration placeholder inherits the generated (proxy-truth)
    // cw 922000 + the responses wire.
    let sol = &resolved["gpt-5.6-sol"];
    assert_eq!(
        sol.info.context_window,
        NonZeroU64::new(922_000).unwrap(),
        "pre-bake donor inherits the generated context_window"
    );
    assert_eq!(
        sol.info.api_backend,
        ApiBackend::Responses,
        "pre-bake donor inherits the api_backend"
    );

    // A generated addition must NEVER donate: the live claude row at the
    // same placeholder keeps its client values (no endpoint defaults in
    // Config::default, and the anthropic catalog inference is silent on
    // the backend).
    let claude = &resolved["claude-opus-4-5"];
    assert_eq!(
        claude.info.context_window,
        NonZeroU64::new(256_000).unwrap(),
        "generated additions ride the fallback but never donate"
    );
    assert_eq!(
        claude.info.api_backend,
        ApiBackend::ChatCompletions,
        "no donor for a non-pre-bake key: the row keeps its built-in backend"
    );
}

#[test]
fn fallback_path_runs_seams_on_non_pre_bake_rows_only() {
    let resolved = resolve_model_list(&Config::default(), None); // fetch dead
    assert_eq!(resolved.len(), BUNDLED_ROW_COUNT);

    // Curated pins survive the row-aware seams (explicit non-default
    // values are seam-invisible).
    let g5c = resolved.get("gpt-5-codex").expect("gpt-5-codex rides the fallback");
    assert_eq!(
        g5c.info.api_backend,
        ApiBackend::Responses,
        "curated responses pin survives the seams"
    );
    assert_eq!(g5c.info.model_family.as_deref(), Some("codex"));
    let gpt4 = resolved.get("gpt-4").expect("gpt-4 rides the fallback");
    assert_eq!(
        gpt4.info.api_backend,
        ApiBackend::ChatCompletions,
        "curated legacy pin: the slug inference is a no-op on it"
    );

    // Rows without a curated menu get the slug-inferred menu at
    // resolution — the same behavior live rows have always had.
    let g51 = resolved.get("gpt-5.1").expect("gpt-5.1 rides the fallback");
    assert_eq!(
        g51.info.reasoning_efforts.len(),
        4,
        "slug-inferred menu rides the curated responses row"
    );

    // Pre-bake rows keep their seed menus (the seams never touch them).
    let grok45 = resolved.get("grok-4.5").expect("seed-only grok-4.5 rides the fallback");
    assert_eq!(grok45.info.reasoning_efforts.len(), 3);
    let grok46 = resolved.get("grok-4.6").expect("grok-4.6 rides the fallback");
    assert_eq!(grok46.info.reasoning_efforts.len(), 4);
    assert!(!grok46.info.supports_backend_search, "overlay curation rides the fallback row");
}

#[test]
fn apex_93d_fixture_rows_parse_through_the_enriched_crate_entry() {
    // The 11-row frontier fixture (generated naming: max_input_tokens /
    // max_output_tokens) parses through the SAME enriched crate struct
    // the bundled file uses; the ceiling values survive the alias fold.
    const FIXTURE: &str =
        include_str!("../../tests/fixtures/catalog-frontier-11-20260918.json");
    let doc: serde_json::Value = serde_json::from_str(FIXTURE).expect("frontier fixture is valid JSON");
    let rows = doc["models"]
        .as_array()
        .cloned()
        .expect("fixture carries a 'models' array");
    assert_eq!(rows.len(), 11);
    let endpoints = EndpointsConfig::default();
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        let parsed: DefaultModelEntry =
            serde_json::from_value(row.clone()).unwrap_or_else(|e| panic!("{id}: {e}"));
        let cfg = entry_config_from_default_row(&parsed, &endpoints);
        let want_cw = row["max_input_tokens"].as_u64().expect("frontier row carries max_input");
        assert_eq!(cfg.context_window.get(), want_cw, "{id}: input ceiling survives the alias fold");
        let want_out = row["max_output_tokens"].as_u64().expect("frontier row carries max_output");
        assert_eq!(
            cfg.max_completion_tokens,
            Some(u32::try_from(want_out).unwrap()),
            "{id}: output ceiling survives the alias fold"
        );
    }
    // Spot-check the comb section 4 matrix headline value.
    let sol: DefaultModelEntry =
        serde_json::from_value(rows[0].clone()).expect("first fixture row is gpt-5.6-sol");
    let cfg = entry_config_from_default_row(&sol, &endpoints);
    assert_eq!(cfg.context_window, NonZeroU64::new(922_000).unwrap());
    assert_eq!(cfg.max_completion_tokens, Some(128_000));
}

#[test]
fn role_pins_come_from_the_baked_file() {
    // D1: the merged file keeps the upstream role pins (curated in the
    // overlay, written by the gate); the crate accessors read them.
    assert_eq!(crate::models::default_model(), "grok-4.6");
    assert_eq!(crate::models::default_web_search_model(), "grok-4.6");
    assert_eq!(crate::models::default_image_description_model(), "grok-4.6");
    assert_eq!(crate::models::default_session_summary_model(), "grok-4.6");
    let root: serde_json::Value =
        serde_json::from_str(crate::models::DEFAULT_MODELS_JSON).expect("baked catalog JSON");
    for pin in ["default", "web_search", "image_description", "session_summary"] {
        assert_eq!(
            root[pin].as_str(),
            Some("grok-4.6"),
            "role pin {pin} rides the baked file"
        );
    }
    assert_eq!(
        root["models"].as_array().map(|a| a.len()),
        Some(BUNDLED_ROW_COUNT),
        "the models array is the merged catalog"
    );
}
