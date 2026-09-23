//! Selftest for the P1 param gate (apex-ayl.130 ZC-PARAMSCHEMA-GATE-1).
//!
//! Runs the exact gate logic that `build.rs` executes: the happy path must
//! pass, and a mutated temp schema (fake property added / required property
//! removed) must produce the expected failure. The committed
//! `config.schema.json` is never touched by the tests.

mod param_gate_check {
    include!("param_gate_check.rs");
}
use param_gate_check as gate;

fn manifest() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn real_inputs() -> (String, String, String) {
    let m = manifest();
    (
        std::fs::read_to_string(m.join("../xai-grok-shell/config.schema.json"))
            .expect("read config.schema.json"),
        std::fs::read_to_string(m.join("src/lib.rs")).expect("read src/lib.rs"),
        std::fs::read_to_string(m.join("default_models.json"))
            .expect("read default_models.json"),
    )
}

#[test]
fn selftest_happy_path_passes() {
    let (schema, lib, baked) = real_inputs();
    let report = gate::run_gate(&schema, &lib, &baked);
    assert!(
        report.errors.is_empty(),
        "unexpected gate errors on the real inputs: {:?}",
        report.errors
    );
    // Derived-constant pins (regression ratchet — update only on an
    // intentional contract change, alongside the gate's allowlists).
    assert_eq!(report.effort_values.len(), 8);
    assert!(report.effort_values.contains(&"xhigh".to_string()));
    assert!(report.effort_values.contains(&"ultra".to_string()));
    assert_eq!(report.required_fields.len(), 15);
    assert!(report.required_fields.contains(&"api_backend".to_string()));
    assert!(report.required_fields.contains(&"reasoning_efforts".to_string()));
    assert!(!report.required_fields.contains(&"model".to_string()));
    assert!(!report.required_fields.contains(&"context_window".to_string()));
    assert_eq!(report.schema_properties.len(), 48);
    assert!(report.schema_properties.contains(&"api_backend".to_string()));
    assert!(report.schema_properties.contains(&"temperature".to_string()));
    assert_eq!(report.struct_fields.len(), 22);
    assert_eq!(
        report.struct_fields.first().map(String::as_str),
        Some("id")
    );
    assert_eq!(
        report.struct_fields.last().map(String::as_str),
        Some("extra_headers")
    );
}

#[test]
fn selftest_fake_property_added_fails() {
    let (schema, lib, baked) = real_inputs();
    // Mutated TEMP schema (written to a temp dir, committed file untouched):
    // a fake property is added to ConfigModelOverride.
    let mut value: serde_json::Value = serde_json::from_str(&schema).expect("parse schema");
    value["definitions"]["ConfigModelOverride"]["properties"]["sneaky_new_param"] =
        serde_json::json!({ "type": ["string", "null"] });
    let dir = std::env::temp_dir().join(format!("zc130-selftest-fake-prop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("config.schema.json");
    std::fs::write(&path, serde_json::to_string(&value).expect("serialize")).expect("write temp schema");
    let mutated = std::fs::read_to_string(&path).expect("read temp schema");
    let report = gate::run_gate(&mutated, &lib, &baked);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        report.errors.len(),
        1,
        "expected exactly one drift error for the fake property, got: {:?}",
        report.errors
    );
    assert!(report.errors[0].contains("sneaky_new_param"), "{}", report.errors[0]);
    assert!(
        report.errors[0].contains("no DefaultModelEntry field"),
        "{}",
        report.errors[0]
    );
}

#[test]
fn selftest_required_property_removed_fails() {
    let (schema, lib, baked) = real_inputs();
    // Mutated temp schema (in-memory): a required curation property
    // (`reasoning_efforts`) is removed from ConfigModelOverride.
    let mut value: serde_json::Value = serde_json::from_str(&schema).expect("parse schema");
    value["definitions"]["ConfigModelOverride"]["properties"]
        .as_object_mut()
        .expect("properties object")
        .remove("reasoning_efforts");
    let mutated = serde_json::to_string(&value).expect("serialize");
    let report = gate::run_gate(&mutated, &lib, &baked);
    // Both directions report the same drift: the struct field lost its schema
    // definition (A), and the baked row key lost its schema definition (C).
    assert_eq!(
        report.errors.len(),
        2,
        "expected the drift in both directions, got: {:?}",
        report.errors
    );
    assert!(report.errors.iter().all(|e| e.contains("reasoning_efforts")), "{:?}", report.errors);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains("no definition in config.schema.json")),
        "{:?}",
        report.errors
    );
}

#[test]
fn selftest_new_struct_field_without_schema_def_fails() {
    let (schema, lib, baked) = real_inputs();
    // New struct field with no schema definition (the headline drift case).
    let mutated = lib.replacen(
        "    pub model: String,\n",
        "    pub model: String,\n    pub sneaky_field: Option<String>,\n",
        1,
    );
    assert_ne!(mutated, lib, "lib.rs mutation anchor not found");
    let report = gate::run_gate(&schema, &mutated, &baked);
    assert_eq!(
        report.errors.len(),
        1,
        "expected exactly one drift error for the new struct field, got: {:?}",
        report.errors
    );
    assert!(report.errors[0].contains("sneaky_field"), "{}", report.errors[0]);
    assert!(
        report
            .errors[0]
            .contains("no definition in config.schema.json"),
        "{}",
        report.errors[0]
    );
}

#[test]
fn selftest_baked_row_key_drift_fails() {
    let (schema, lib, baked) = real_inputs();
    // A row key that is neither struct-modeled nor schema-defined.
    let mut value: serde_json::Value = serde_json::from_str(&baked).expect("parse baked");
    value["models"][0]["bogus_cap"] = serde_json::json!(123);
    let mutated = serde_json::to_string(&value).expect("serialize");
    let report = gate::run_gate(&schema, &lib, &mutated);
    assert!(
        !report.errors.is_empty(),
        "expected drift errors for the bogus row key"
    );
    assert!(
        report.errors.iter().all(|e| e.contains("bogus_cap")),
        "{:?}",
        report.errors
    );
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains("not modeled by DefaultModelEntry")),
        "{:?}",
        report.errors
    );
}

#[test]
fn selftest_struct_field_parser_sanity() {
    let (_schema, lib, _baked) = real_inputs();
    let fields = gate::parse_struct_fields(&lib).expect("parse DefaultModelEntry fields");
    assert_eq!(fields.len(), 22);
    assert_eq!(fields[0], "id");
    assert_eq!(fields[1], "model");
    assert_eq!(fields.last().map(String::as_str), Some("extra_headers"));
    assert_eq!(gate::STRUCT_NAME, "DefaultModelEntry");
    // A file without the struct must fail loudly (never silently pass).
    let missing = gate::parse_struct_fields("pub struct Other { pub a: u8 }");
    assert!(missing.is_err());
}
