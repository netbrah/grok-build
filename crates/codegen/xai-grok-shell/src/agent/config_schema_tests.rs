//! Golden-fixture staleness test for the generated config schema.
//!
//! Donor: codex `codex-rs/core/src/config/schema_tests.rs`
//! (`config_schema_matches_fixture`), ported 1:1: regenerate the schema from
//! the live types, canonicalize both sides, JSON-diff against the committed
//! fixture; on mismatch fail with a unified diff + the regen command. A second
//! assertion pins the writer output byte-for-byte against the fixture.
//!
//! A second golden (apex-33z) does the same for the self-contained HTML
//! reference: `config_reference_html_matches` regenerates
//! `config.reference.html` from the live schema and byte-compares it against
//! the committed file (plus a same-build determinism assert and a byte-for-
//! byte writer check).

use super::html::{render_config_reference_html, write_config_reference_html};
use super::{canonicalize, config_schema_json, write_config_schema};
use similar::TextDiff;
use tempfile::TempDir;

fn fixture_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config.schema.json")
}

fn html_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config.reference.html")
}

fn trim_single_trailing_newline(contents: &str) -> &str {
    contents.strip_suffix('\n').unwrap_or(contents)
}

#[test]
fn config_schema_matches_fixture() {
    let fixture = std::fs::read_to_string(fixture_path()).expect("read config schema fixture");
    let fixture_value: serde_json::Value =
        serde_json::from_str(&fixture).expect("parse config schema fixture");
    let schema_json = config_schema_json().expect("serialize schema json");
    let schema_value: serde_json::Value =
        serde_json::from_slice(&schema_json).expect("decode schema json");
    let fixture_value = canonicalize(&fixture_value);
    let schema_value = canonicalize(&schema_value);
    if fixture_value != schema_value {
        let expected =
            serde_json::to_string_pretty(&fixture_value).expect("serialize fixture json");
        let actual = serde_json::to_string_pretty(&schema_value).expect("serialize schema json");
        let diff = TextDiff::from_lines(&expected, &actual)
            .unified_diff()
            .header("fixture", "generated")
            .to_string();
        panic!(
            "Current schema for `config.toml` doesn't match the fixture. \
Run `cargo run -p xai-grok-shell --bin config-schema-write` to overwrite with your changes.\n\n{diff}"
        );
    }

    // The writer must reproduce the fixture byte-for-byte (modulo one trailing newline).
    let tmp = TempDir::new().expect("create temp dir");
    let tmp_path = tmp.path().join("config.schema.json");
    write_config_schema(&tmp_path).expect("write config schema to temp path");
    let tmp_contents =
        std::fs::read_to_string(&tmp_path).expect("read back config schema from temp path");
    assert_eq!(
        trim_single_trailing_newline(&fixture),
        trim_single_trailing_newline(&tmp_contents),
        "fixture should match exactly with generated schema"
    );
}

#[test]
fn config_reference_html_matches() {
    // Determinism first: two renders in the same build must be byte-identical
    // (sorted traversal, no timestamps) — this is what makes the golden
    // compare meaningful.
    let first = render_config_reference_html().expect("render config reference html");
    let second = render_config_reference_html().expect("render config reference html");
    assert_eq!(first, second, "config reference render must be deterministic");

    let committed = std::fs::read_to_string(html_path()).expect("read config.reference.html");
    if committed != first {
        let diff = similar::TextDiff::from_lines(&committed, &first)
            .unified_diff()
            .header("committed", "generated")
            .to_string();
        panic!(
            "config.reference.html doesn't match the live schema. \
Run `cargo run -p xai-grok-shell --bin config-schema-write -- --html` to regenerate.\n\n{diff}"
        );
    }

    // The writer must reproduce the committed reference byte-for-byte.
    let tmp = TempDir::new().expect("create temp dir");
    let tmp_path = tmp.path().join("config.reference.html");
    write_config_reference_html(&tmp_path).expect("write config reference html to temp path");
    let tmp_contents =
        std::fs::read_to_string(&tmp_path).expect("read back config reference html");
    assert_eq!(
        committed, tmp_contents,
        "writer output should match the committed reference byte-for-byte"
    );
}
