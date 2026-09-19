//! CATALOG-LIVEHYDRATE-1 (apex-8jo): the live `/model_group/info` batch
//! splice — observed-shape parse, URL derivation, cache backward compat
//! (old-schema direction), and the R2 cap-only backfill from the baked
//! catalog.
//!
//! Group-info fixture source: `grok/plans/model/data/catalog-digest.json`
//! (2026-09-18 capture, `.models[id].group`; the 2026-09-19 mint check
//! found the live body byte-identical to the raw capture). Five records:
//! the opus dot/dash per-spelling pair, `gpt-5.6-sol` (ties to the 93d
//! 11-row frontier fixture), `gpt-3.5-turbo` (seven null fields),
//! `text-embedding-3-small` (null `max_output_tokens` — the out-null
//! observation). Costs are DATA in this fixture (the section exists to
//! observe them), as in the committed digest; the redaction sweep bans
//! endpoints, keys, host strings, and long hex.
const GROUP_FIXTURE: &str =
    include_str!("../../tests/fixtures/model-group-info-catalog-20260919.json");

const GROUP_FIXTURE_IDS: &[&str] = &[
    "claude-opus-4.8",
    "claude-opus-4-8",
    "gpt-5.6-sol",
    "gpt-3.5-turbo",
    "text-embedding-3-small",
];

fn group_fixture() -> serde_json::Value {
    serde_json::from_str(GROUP_FIXTURE)
        .expect("group-info fixture is valid JSON (committed artifact)")
}

// A — fixture + zero-assumption parse

#[test]
fn group_fixture_is_the_observed_shape() {
    let doc = group_fixture();
    let rows = doc["data"].as_array().expect("wire container: a top-level 'data' array");
    assert_eq!(
        rows.len(),
        5,
        "the fixture carries exactly the five selected group records"
    );
    let ids: Vec<&str> = rows
        .iter()
        .map(|row| row["model_group"].as_str().expect("every record keys itself"))
        .collect();
    assert_eq!(ids, GROUP_FIXTURE_IDS, "record order and names");
    for row in rows {
        // Observed population: all 24 digest fields present per record
        // (health_checked_at rides in the live body only — the fixture is
        // digest-sourced, so it is not asserted here).
        for field in [
            "model_group",
            "providers",
            "mode",
            "max_input_tokens",
            "max_output_tokens",
            "input_cost_per_token",
            "output_cost_per_token",
            "input_cost_per_pixel",
            "tpm",
            "rpm",
            "itpm",
            "otpm",
            "supports_reasoning",
            "supports_function_calling",
            "supports_parallel_function_calling",
            "supports_vision",
            "supports_web_search",
            "supports_url_context",
            "health_status",
            "health_response_time",
            "is_public_model_group",
            "configurable_clientside_auth_params",
            "supported_reasoning_efforts",
            "supported_openai_params",
        ] {
            assert!(row.get(field).is_some(), "{}: field {field} present", row["model_group"]);
        }
    }
    assert_eq!(
        doc["bead"].as_str(),
        Some("apex-8jo CATALOG-LIVEHYDRATE-1"),
        "provenance header must name the bead"
    );
    assert!(
        doc["source"].as_str().is_some_and(|s| s.contains("catalog-digest.json")),
        "provenance header must name the digest source"
    );
    // Redaction: no endpoints, keys, host strings, or long hex anywhere.
    let raw = GROUP_FIXTURE.to_lowercase();
    for banned in [
        "http://",
        "https://",
        "api_key",
        "apikey",
        "sk-",
        "bearer",
        "netapp",
        "llm-proxy",
        ".eng.",
    ] {
        assert!(!raw.contains(banned), "fixture must not contain {banned:?}");
    }
    assert!(
        !regex::Regex::new(r"\b[0-9a-f]{40,}\b")
            .expect("sweep regex is valid")
            .is_match(GROUP_FIXTURE),
        "fixture must not contain long bare hex"
    );
}

#[test]
fn group_fixture_parses_verbatim() {
    let doc = group_fixture();
    let parsed = crate::remote::client::parse_model_group_info(&doc)
        .expect("the fixture is the observed container shape");
    assert_eq!(parsed.len(), 5, "one row per record, keyed by model_group");
    for row in doc["data"].as_array().unwrap() {
        let name = row["model_group"].as_str().unwrap();
        let stored = parsed
            .get(name)
            .unwrap_or_else(|| panic!("{name}: row must be keyed by its own name"));
        assert_eq!(
            stored, row,
            "{name}: the record must be stored VERBATIM (every field, nulls included)"
        );
    }
    // Nulls stay nulls (observation, not absence, not a default).
    let legacy = &parsed["gpt-3.5-turbo"];
    for field in [
        "input_cost_per_pixel",
        "rpm",
        "itpm",
        "otpm",
        "health_status",
        "health_response_time",
        "configurable_clientside_auth_params",
    ] {
        assert!(legacy[field].is_null(), "gpt-3.5-turbo: {field} observed null stays null");
    }
    // Values survive (costs are data).
    assert_eq!(parsed["gpt-5.6-sol"]["input_cost_per_token"], serde_json::json!(4e-06));
    assert_eq!(parsed["gpt-5.6-sol"]["output_cost_per_token"], serde_json::json!(2e-05));
    assert_eq!(
        parsed["gpt-5.6-sol"]["supported_reasoning_efforts"],
        serde_json::json!(["none", "low", "medium", "high", "xhigh"])
    );
    assert_eq!(parsed["text-embedding-3-small"]["max_output_tokens"].is_null(), true);
}

#[test]
fn group_fixture_spelling_rows_not_merged() {
    let doc = group_fixture();
    let parsed = crate::remote::client::parse_model_group_info(&doc).unwrap();
    let dot = parsed
        .get("claude-opus-4.8")
        .expect("dot spelling is a distinct model id");
    let dash = parsed
        .get("claude-opus-4-8")
        .expect("dash spelling is a distinct model id");
    assert_ne!(dot, dash, "per-spelling rows are stored separately, never merged");
    assert_eq!(parsed.len(), 5, "no row was collapsed away");
}

#[test]
fn parse_model_group_info_shape_mismatches_degrade() {
    // The observed container is an object with a "data" array. Anything
    // else degrades to no section (R1: never invent).
    for body in [
        serde_json::json!({}),
        serde_json::json!({"data": {}}),
        serde_json::json!({"data": null}),
        serde_json::json!([{"model_group": "x"}]),
        serde_json::json!(null),
        serde_json::json!("nope"),
    ] {
        assert!(
            crate::remote::client::parse_model_group_info(&body).is_none(),
            "{body}: unobserved container shape must degrade"
        );
    }
}

#[test]
fn parse_model_group_info_skips_unkeyable_records() {
    let body = serde_json::json!({
        "data": [
            {"model_group": "keep-1", "tpm": 1},
            "not-an-object",
            {"no_key": true},
            {"model_group": ""},
            {"model_group": 7},
            {"model_group": "keep-2", "rpm": null},
        ]
    });
    let parsed = crate::remote::client::parse_model_group_info(&body)
        .expect("the container itself is the observed shape");
    assert_eq!(parsed.len(), 2, "only keyable records are stored");
    assert_eq!(parsed["keep-1"]["tpm"], serde_json::json!(1));
    assert_eq!(parsed["keep-2"]["rpm"].is_null(), true);
    assert!(parsed.get("no_key").is_none());
    // Empty "data" is a legitimate observation: endpoint returned no groups.
    let empty = crate::remote::client::parse_model_group_info(&serde_json::json!({"data": []}))
        .expect("an empty array is the observed shape with zero rows");
    assert!(empty.is_empty());
}

#[test]
fn parse_model_group_info_first_wins_on_duplicate_names() {
    // Observed population: 0 duplicates. Contract (documented,
    // deterministic): the first occurrence wins; later ones are skipped.
    let body = serde_json::json!({
        "data": [
            {"model_group": "dup", "tpm": 1},
            {"model_group": "dup", "tpm": 2},
        ]
    });
    let parsed = crate::remote::client::parse_model_group_info(&body).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed["dup"]["tpm"], serde_json::json!(1));
}

// B — group URL derivation (observed shapes only)

#[test]
fn group_info_url_observed_shapes() {
    // The observed proxy pair: the list URL and the group URL hang off one
    // host base (list = base/v1/models, group = base/model_group/info).
    assert_eq!(
        crate::remote::model_source::oai::group_info_url(
            "https://llm-proxy-api.ai.eng.netapp.com/v1/models"
        ),
        Some("https://llm-proxy-api.ai.eng.netapp.com/model_group/info".to_string())
    );
    // A custom endpoint whose base carries no /v1 segment.
    assert_eq!(
        crate::remote::model_source::oai::group_info_url("https://models.acme.com/models"),
        Some("https://models.acme.com/model_group/info".to_string())
    );
    assert_eq!(
        crate::remote::model_source::oai::group_info_url("https://api.x.ai/v1/models"),
        Some("https://api.x.ai/model_group/info".to_string())
    );
    // Unobserved shapes: no derivation, no assumption.
    assert_eq!(
        crate::remote::model_source::oai::group_info_url("https://models.acme.com/v2/models"),
        None,
        "…/v2/models was never observed; extrapolating would invent a shape"
    );
    assert_eq!(
        crate::remote::model_source::oai::group_info_url("https://models.acme.com/catalog"),
        None,
        "a list URL without the /models suffix cannot be derived"
    );
}

// D1 — R1c backward compat: old binary schema reads a new-format file

/// The pre-cut on-disk schema, mirrored for the rollback direction: an old
/// binary's `serde_json::from_slice::<ModelsCache>` must accept a file whose
/// new binary wrote the second section (serde's default ignore-unknown
/// behavior — no `deny_unknown_fields` on the cache read path).
#[derive(serde::Deserialize)]
struct OldModelsCacheSchema {
    fetched_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    renewed_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    grok_version: Option<String>,
    #[serde(default)]
    auth_method: Option<crate::agent::remote_config::CacheAuthMethod>,
    #[serde(default)]
    origin: Option<String>,
    #[serde(default)]
    identity: Option<String>,
    #[serde(default)]
    etag: Option<String>,
    models: serde_json::Map<String, serde_json::Value>,
}

#[test]
fn old_schema_reads_new_cache_file() {
    // A new-format file: the pre-cut fields (the models value is a plain
    // object — the old-schema mirror below reads it as raw JSON) plus the
    // second section an older binary has never seen.
    let file = serde_json::json!({
        "fetched_at": "2026-09-19T12:00:00Z",
        "grok_version": "0.0.0-test",
        "auth_method": "api_key",
        "origin": "https://o.example/v1/models",
        "identity": "id-1",
        "etag": "etag-1",
        "models": { "grok-4.6": { "info": {} } },
        "model_groups": {
            "claude-opus-4.8": { "model_group": "claude-opus-4.8", "tpm": 128 }
        },
    });
    let cache = serde_json::to_vec(&file).unwrap();
    let old: OldModelsCacheSchema = serde_json::from_slice(&cache)
        .expect("a newer cache file must not brick an older binary at startup");
    assert_eq!(old.models.len(), 1, "the models section survives the old schema");
    assert!(old.models.contains_key("grok-4.6"));
    // The section key is invisible to the old schema (ignored, not fatal).
    let raw = String::from_utf8(cache).unwrap();
    assert!(raw.contains("model_groups"), "the new file carries the section");
}

// G — R2 cap-only backfill from the baked catalog

fn parse_with_baked(
    row: &serde_json::Value,
    baked: &indexmap::IndexMap<String, crate::remote::client::BakedCapRow>,
) -> crate::agent::config::ModelEntryConfig {
    crate::remote::client::parse_remote_model_value(row, "https://default.url", baked)
        .expect("row must parse")
}

#[test]
fn backfill_live_null_caps_from_baked_row() {
    let baked: indexmap::IndexMap<String, crate::remote::client::BakedCapRow> =
        [("cap-null".to_string(), crate::remote::client::BakedCapRow {
            context_window: std::num::NonZeroU64::new(777_000),
            max_completion_tokens: Some(64_000),
        })]
        .into_iter()
        .collect();
    // Live feed: both caps explicitly null.
    let row = serde_json::json!({
        "id": "cap-null",
        "object": "model",
        "max_input_tokens": null,
        "max_output_tokens": null,
    });
    let parsed = parse_with_baked(&row, &baked);
    assert_eq!(
        parsed.context_window.get(),
        777_000,
        "resolved context_window must backfill from the baked observation"
    );
    assert_eq!(
        parsed.max_completion_tokens,
        Some(64_000),
        "resolved max_completion_tokens must backfill from the baked observation"
    );
    assert_eq!(
        parsed.feed_max_input_tokens, None,
        "provenance must record what the LIVE feed said: null stays null"
    );
    assert_eq!(parsed.feed_max_output_tokens, None, "provenance: out null stays null");
}

#[test]
fn backfill_live_null_baked_null_keeps_defaults() {
    // Baked row exists but its capture lacked the caps: the table carries
    // None (observations, not folds) — the pre-cut last resort stands.
    let baked: indexmap::IndexMap<String, crate::remote::client::BakedCapRow> =
        [("no-caps".to_string(), crate::remote::client::BakedCapRow {
            context_window: None,
            max_completion_tokens: None,
        })]
        .into_iter()
        .collect();
    let row = serde_json::json!({
        "id": "no-caps",
        "object": "model",
        "max_input_tokens": null,
        "max_output_tokens": null,
    });
    let parsed = parse_with_baked(&row, &baked);
    assert_eq!(
        parsed.context_window.get(),
        crate::remote::client::DEFAULT_CONTEXT_WINDOW,
        "no live, no baked → the existing built-in last resort (unchanged)"
    );
    assert_eq!(parsed.max_completion_tokens, None, "no live, no baked → stays None");
    // No baked row at all: identical.
    let empty = indexmap::IndexMap::new();
    let parsed = parse_with_baked(&row, &empty);
    assert_eq!(parsed.context_window.get(), crate::remote::client::DEFAULT_CONTEXT_WINDOW);
    assert_eq!(parsed.max_completion_tokens, None);
}

#[test]
fn backfill_live_present_wins_over_baked() {
    let baked: indexmap::IndexMap<String, crate::remote::client::BakedCapRow> =
        [("live-wins".to_string(), crate::remote::client::BakedCapRow {
            context_window: std::num::NonZeroU64::new(1_000_000),
            max_completion_tokens: Some(128_000),
        })]
        .into_iter()
        .collect();
    let row = serde_json::json!({
        "id": "live-wins",
        "object": "model",
        "max_input_tokens": 200_000,
        "max_output_tokens": 64_000,
    });
    let parsed = parse_with_baked(&row, &baked);
    assert_eq!(
        parsed.context_window.get(),
        200_000,
        "a live cap (even if different from baked) always wins"
    );
    assert_eq!(
        parsed.max_completion_tokens,
        Some(64_000),
        "a live cap (even if different from baked) always wins"
    );
}

#[test]
fn backfill_config_toml_wins_at_resolution() {
    // Apply order: backfill at parse (fetch) → config.toml at resolution —
    // operator authority wins over both.
    let baked: indexmap::IndexMap<String, crate::remote::client::BakedCapRow> =
        [("cfg-vs-backfill".to_string(), crate::remote::client::BakedCapRow {
            context_window: std::num::NonZeroU64::new(777_000),
            max_completion_tokens: Some(64_000),
        })]
        .into_iter()
        .collect();
    let row = serde_json::json!({
        "id": "cfg-vs-backfill",
        "object": "model",
        "max_input_tokens": null,
        "max_output_tokens": null,
    });
    let parsed = parse_with_baked(&row, &baked);
    assert_eq!(parsed.context_window.get(), 777_000, "setup: backfill landed");
    let map =
        crate::agent::remote_config::fetch::build_prefetched_map(vec![parsed], None);
    let cfg = crate::agent::config::Config::new_from_toml_cfg(
        &toml::from_str(
            "[model.cfg-vs-backfill]\ncontext_window = 123456\nmax_completion_tokens = 999\n",
        )
        .unwrap(),
    )
    .unwrap();
    let catalog = crate::agent::config::resolve_model_list(&cfg, Some(map));
    let entry = catalog
        .get("cfg-vs-backfill")
        .expect("the live row must resolve in the catalog");
    assert_eq!(
        entry.info.context_window.get(),
        123_456,
        "config.toml must apply AFTER the backfill and win"
    );
    assert_eq!(
        entry.info.max_completion_tokens,
        Some(999),
        "config.toml must apply AFTER the backfill and win (out cap)"
    );
}

#[test]
fn backfill_u32_overflow_live_value_not_backfilled() {
    // Adjudication: a live value present but above u32::MAX is PRESENT —
    // the fold drops it (today's behavior, provenance keeps the u64) and
    // the backfill (a null/absent ruling) does not fire on the out axis.
    // The in axis is independent and still backfills.
    let baked: indexmap::IndexMap<String, crate::remote::client::BakedCapRow> =
        [("overflow".to_string(), crate::remote::client::BakedCapRow {
            context_window: std::num::NonZeroU64::new(555_000),
            max_completion_tokens: Some(128_000),
        })]
        .into_iter()
        .collect();
    let row = serde_json::json!({
        "id": "overflow",
        "object": "model",
        "max_input_tokens": null,
        "max_output_tokens": u64::from(u32::MAX) + 1,
    });
    let parsed = parse_with_baked(&row, &baked);
    assert_eq!(
        parsed.max_completion_tokens,
        None,
        "a present-but-overflowing live value is not null/absent: no backfill"
    );
    assert_eq!(
        parsed.feed_max_output_tokens,
        Some(u64::from(u32::MAX) + 1),
        "provenance keeps the full live u64"
    );
    assert_eq!(
        parsed.context_window.get(),
        555_000,
        "the input axis backfills independently"
    );
}

#[test]
fn backfill_key_matches_prefetched_map_key() {
    // The lookup key must equal build_prefetched_map's key expression
    // (id, else model) — otherwise the backfill misses real rows.
    let baked_by_id: indexmap::IndexMap<String, crate::remote::client::BakedCapRow> =
        [("id-key".to_string(), crate::remote::client::BakedCapRow {
            context_window: std::num::NonZeroU64::new(313_370),
            max_completion_tokens: None,
        })]
        .into_iter()
        .collect();
    // id + model present: the key is the id.
    let row = serde_json::json!({
        "id": "id-key",
        "model": "model-name",
        "object": "model",
        "max_input_tokens": null,
    });
    let parsed = parse_with_baked(&row, &baked_by_id);
    assert_eq!(parsed.context_window.get(), 313_370, "id wins the key");

    // model only (no id): the key is the model.
    let baked_by_model: indexmap::IndexMap<String, crate::remote::client::BakedCapRow> =
        [("model-name".to_string(), crate::remote::client::BakedCapRow {
            context_window: std::num::NonZeroU64::new(313_370),
            max_completion_tokens: None,
        })]
        .into_iter()
        .collect();
    let row = serde_json::json!({
        "model": "model-name",
        "object": "model",
        "max_input_tokens": null,
    });
    let parsed = parse_with_baked(&row, &baked_by_model);
    assert_eq!(
        parsed.context_window.get(),
        313_370,
        "model is the key when the row has no id (map-key parity)"
    );
}

#[test]
fn baked_cap_rows_carry_observations_not_folds() {
    // A synthetic baked row whose capture lacked the caps must contribute
    // None/None — never the 200k fold of entry_config_from_default_row.
    let row = crate::models::DefaultModelEntry {
        model: "baked-no-caps".to_string(),
        ..Default::default()
    };
    let caps = crate::remote::client::baked_cap_row_from_entry(&row);
    assert_eq!(caps.context_window, None, "no fold may enter the table");
    assert_eq!(caps.max_completion_tokens, None, "no fold may enter the table");
    // Curated values win over the generated aliases (the lib.rs contract).
    let row = crate::models::DefaultModelEntry {
        model: "baked-both".to_string(),
        context_window: std::num::NonZeroU64::new(700_000),
        max_input_tokens: Some(100_000),
        max_completion_tokens: Some(70_000),
        max_output_tokens: Some(10_000),
        ..Default::default()
    };
    let caps = crate::remote::client::baked_cap_row_from_entry(&row);
    assert_eq!(caps.context_window, std::num::NonZeroU64::new(700_000));
    assert_eq!(caps.max_completion_tokens, Some(70_000));
    // The real table: every baked row keyed by its id-or-model rule.
    let table = crate::remote::client::baked_cap_rows();
    assert!(!table.is_empty(), "the baked catalog is embedded in the binary");
    assert!(
        table.contains_key("gpt-5.6-sol"),
        "a frontier id must be keyable (ties to the 93d fixture)"
    );
}

#[test]
fn backfill_touches_no_wire_semantics() {
    // The baked row carries curated wire fields; a cap-null live row must
    // keep its own wire resolution (explicit-row > slug-inference chain),
    // untouched by the backfill.
    let row = crate::models::DefaultModelEntry {
        id: Some("wire-untouched".to_string()),
        model: "wire-untouched".to_string(),
        model_family: Some("curated-family".to_string()),
        api_backend: Some(crate::sampling::ApiBackend::Responses),
        ..Default::default()
    };
    let mut baked = crate::remote::client::baked_cap_rows();
    baked.insert(
        "wire-untouched".to_string(),
        crate::remote::client::baked_cap_row_from_entry(&row),
    );
    let feed = serde_json::json!({
        "id": "wire-untouched",
        "object": "model",
        "max_input_tokens": null,
        "max_output_tokens": null,
    });
    let parsed = parse_with_baked(&feed, &baked);
    assert_eq!(
        parsed.model_family,
        None,
        "model_family keeps the row's own (absent) value — no backfill"
    );
    assert_eq!(
        parsed.api_backend,
        crate::sampling::ApiBackend::default(),
        "api_backend keeps the row's own resolution — no backfill"
    );
}
