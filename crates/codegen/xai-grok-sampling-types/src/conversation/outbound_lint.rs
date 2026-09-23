//! A2 — in-product pre-send invariant validator (FMTL-HARDEN-1,
//! apex-ayl.126.8.2; HARDENING-SPEC §3.2).
//!
//! Value-level mirror of the hard invariants H-1..H-5 plus the header-level
//! H-6, per-rule scope EXACTLY mirroring the A1 offline linter
//! (`grok/plans/parity-formalism/tools/invariant_lint.py`) for what a single
//! `(Boundary, body)` pair can decide:
//!
//! - **H-3 (D-5 delta)**: NON-EMPTY id ONLY. The mint-set/unknown-id clause
//!   is A1-only (store state — the linter derives the mint set from the
//!   capture's own responses); unknown ids are deliberately NOT flagged
//!   in-product (pinned by `h3_unknown_id_clean_in_product_d5`).
//! - **H-5 (= H-5' in full; re-scoped 2026-09-23, apex-ayl.126.8.6,
//!   HARDENING-SPEC 3.2a + 2.2a)**: `encitem_*` ids / `litellm_enc:`
//!   blobs on a NON-Azure boundary (VLLenient/Vertex) = violation,
//!   FIELD-SCOPED (item `id` / `encrypted_content` values only — request
//!   substrings out of scope: prompt doc-text legitimately carries
//!   `litellm_enc:`); on AzStrict = SILENT (the mint-domain match is
//!   undecidable from strict-row request bytes; blob decode is O_P, out
//!   of scope). A1/A2 H-5' scopes are IDENTICAL (no domain clause — A1
//!   `_check_h5_encitem_azure_only` early-returns on AzStrict rows). The
//!   enc-affinity gate (`apply_enc_affinity_gate`, applied upstream at
//!   the same send seam) remains the runtime retain/strip authority.
//!   SUPERSEDED 2026-09-23 (apex-ayl.126.8.6, 3.2a); v1 delta note
//!   preserved: "A1's AzStrict arm additionally requires the mint-domain
//!   match (`mint_tag == x-litellm-tags` pin, EV-9) — the row pin is
//!   state this value linter does not receive, so that clause is
//!   unreachable in-product." — false against the frozen A1 (the .126.8.5
//!   H-5' cut removed the domain clause; AzStrict is SILENT in A1 too).
//! - **H-6** is header-level: [`lint_outbound_headers`], not the body fn.
//! - **H-7** is OUT of scope (history shape, not a flat request body;
//!   asserted at the existing projection seam —
//!   `projection_tests::xw_proj_orphaned_result_direction` +
//!   `xw_proj_surviving_call_keeps_result`).
//!
//! Wire model: `AzStrict`/`VLLenient` rows ride the responses wire; `Vertex`
//! rows ride the messages wire. A2 receives the [`Boundary`] directly from
//! the row at send time — no row-class inference (RULE-DRIFT parity note:
//! A1 infers the class from capture data, A2 derives it from the row; D-2).
//!
//! Observe-only: this module NEVER mutates the request and NEVER blocks it.
//! The caller (`xai-grok-sampler` send boundary) decides debug-panic vs
//! release telemetry (spec D-1).

use serde_json::Value;

use super::projection::Boundary;
use super::rules_generated::{HARD_RULES, HardRule};

/// A single hard-invariant violation observed on an outbound request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintViolation {
    /// The rule id from the hard-rule table (`H-1`..`H-6`).
    pub rule: &'static str,
    /// Body path (`input[i].content`, `body.store`, …) or
    /// `header:<name>` for the header-level H-6.
    pub path: String,
    /// The observed value (human-readable; long ids/blobs truncated).
    pub observed: String,
}

/// Lint the FINAL outbound request body against the hard invariants
/// H-1..H-5, scoped per rule (the header-level H-6 lives in
/// [`lint_outbound_headers`]).
///
/// `boundary` is the row's boundary class at send time (no inference).
/// Returns every violation observed — the caller decides what to do with
/// them (debug-panic / release telemetry; D-1). Never mutates `body`.
pub fn lint_outbound_request(boundary: Boundary, body: &Value) -> Vec<LintViolation> {
    let mut out = Vec::new();
    for rule in HARD_RULES {
        if rule.check == "no_empty_x_litellm_tags" {
            continue; // header-level: enforced by `lint_outbound_headers`
        }
        if !rule_in_scope(rule, boundary, body) {
            continue;
        }
        out.extend(match rule.check {
            "reasoning_content_absent_or_empty" => check_h1(body),
            "reasoning_no_encrypted_content" => check_h2(body),
            "item_id_nonempty_and_minted" => check_h3(body),
            "store_false" => check_h4(body),
            "encitem_only_to_matching_azure_row" => check_h5(boundary, body),
            // Unknown check name in the table: a CI escape (the
            // `rule_table_registry_complete` test + the T7 drift test pin
            // table vs dispatch). The observe-only contract (D-1) says skip,
            // never take a session down.
            _ => Vec::new(),
        });
    }
    out
}

/// Lint the FINAL outbound header set against the header-level hard
/// invariant H-6 (spec §2.1; EV-6): no empty-string `x-litellm-tags`.
///
/// `headers` is the final per-request (name, value) pair list — string
/// values only (the caller skips non-UTF-8 values, mirroring A1's
/// string-only header handling). SCOPED to `x-litellm-tags`: other
/// empty-string headers are OUT of scope (OQ-f — the empty `x-grok-*`
/// values that ride with 200s, mgw-toolctl-01 req-004, must not fire).
pub fn lint_outbound_headers(headers: &[(&str, &str)]) -> Vec<LintViolation> {
    let mut out = Vec::new();
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("x-litellm-tags") && value.is_empty() {
            out.push(LintViolation {
                rule: "H-6",
                path: "header:x-litellm-tags".into(),
                observed: "x-litellm-tags=''".into(),
            });
        }
    }
    out
}

/// Class-token scoping for hard rules (spec §2.1 `Class` column), A2 form:
/// the wire is implied by the boundary (responses = AzStrict|VLLenient,
/// messages = Vertex), so the A1 `wire == responses` conjuncts fold into
/// the boundary test (RULE-DRIFT parity note, D-2).
fn rule_in_scope(rule: &HardRule, boundary: Boundary, body: &Value) -> bool {
    match rule.class {
        "AzStrict" => boundary == Boundary::AzStrict,
        "all-responses" => matches!(boundary, Boundary::AzStrict | Boundary::VLLenient),
        "all-envelope" => true,
        "cross-boundary" => body.is_object(),
        _ => false,
    }
}

/// Yield `(index, item)` for object items of a responses-wire `input` list
/// (mirror of A1 `_input_items`: non-object bodies / non-list `input` /
/// non-object items yield nothing).
fn input_items<'a>(
    body: &'a Value,
) -> impl Iterator<Item = (usize, &'a serde_json::Map<String, Value>)> {
    body.as_object()
        .and_then(|obj| obj.get("input"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(i, item)| item.as_object().map(|o| (i, o)))
}

/// H-1 (AzStrict, EV-1): a reasoning item's `content` is ABSENT or `[]` —
/// never a populated list. (A1 `_check_h1_reasoning_content`; the scope
/// conjunct is enforced by the dispatch.)
fn check_h1(body: &Value) -> Vec<LintViolation> {
    let mut out = Vec::new();
    for (i, item) in input_items(body) {
        if !matches!(item.get("type"), Some(Value::String(t)) if t == "reasoning") {
            continue;
        }
        // A1 parity: `item.get("content")` is None for an ABSENT key and
        // for an explicit `null` alike — treat both as clean.
        let Some(content) = item.get("content").filter(|v| !v.is_null()) else {
            continue; // absent (or null) → clean
        };
        if content.is_array() && content.as_array().unwrap().is_empty() {
            continue; // [] → clean
        }
        let n = match content.as_array() {
            Some(parts) => parts.len().to_string(),
            None => "?".to_string(),
        };
        out.push(LintViolation {
            rule: "H-1",
            path: format!("input[{i}].content"),
            observed: format!("content={n} part(s)"),
        });
    }
    out
}

/// H-2 (AzStrict, EV-13/EV-9): CLAUSE (a) ONLY (re-scoped 2026-09-23,
/// apex-ayl.126.8.6; HARDENING-SPEC 3.2a + 2.2a): a strict-row reasoning
/// item carrying `encrypted_content` (KEY PRESENCE incl. null/empty,
/// LA-03) VIOLATES iff the item ALSO carries `id` or `mint_tag` (KEY
/// PRESENCE incl. null/empty — non-strict form / fabricated pin). The
/// strict post-projection form {type, summary, encrypted_content}
/// carries neither (`provider.rs:422` strips content+id; enc untouched),
/// so own-origin enc — T0 KEEP / EV-9, on-disk mxai-c04 req-004 — is
/// SILENT in A2. Parity source: A1 H-2' clause (a) —
/// `_check_h2_no_encrypted_content` (stable check name
/// `reasoning_no_encrypted_content`), 2.2a; A1 additionally enforces
/// clause (b) (string value in the arm's resp mint corpus; a non-string
/// value = violation — the LA-03 non-string sub-form), which is
/// deliberately NOT mirrored pre-send (D-H2B, 3.2a — A1-only residue).
///
/// SUPERSEDED 2026-09-23 (apex-ayl.126.8.6, 3.2a); v1 doc preserved:
/// "H-2 (AzStrict, EV-13; drift guard): a reasoning item carries NO
/// `encrypted_content`. KEY PRESENCE fires, any value including null
/// (A1 `_check_h2_no_encrypted_content`, LA-03)." — the v1 pointer cited
/// the over-warn key-presence parity; post-rescope the parity source is
/// A1 H-2' clause (a) (2.2a).
fn check_h2(body: &Value) -> Vec<LintViolation> {
    let mut out = Vec::new();
    for (i, item) in input_items(body) {
        if !matches!(item.get("type"), Some(Value::String(t)) if t == "reasoning") {
            continue;
        }
        // KEY PRESENCE incl. null/empty (LA-03) — the v1 gate, unchanged.
        if !item.contains_key("encrypted_content") {
            continue;
        }
        // CLAUSE (a) gate (3.2a post-rescope): the item ALSO carries
        // `id` or `mint_tag` (KEY PRESENCE incl. null/empty). Clause (b)
        // (corpus membership; the non-string short-circuit) is A1-only —
        // D-H2B (3.2a): deliberately not mirrored pre-send.
        let pinned = if item.contains_key("id") && item.contains_key("mint_tag") {
            "id/mint_tag"
        } else if item.contains_key("id") {
            "id"
        } else if item.contains_key("mint_tag") {
            "mint_tag"
        } else {
            ""
        };
        if pinned.is_empty() {
            continue; // own-origin shape (no id/mint_tag) → SILENT
        }
        out.push(LintViolation {
            rule: "H-2",
            path: format!("input[{i}].encrypted_content"),
            observed: format!("encrypted_content present (clause (a): {pinned} present)"),
        });
    }
    out
}

/// H-3 (all responses-wire, EV-2/EV-3/EV-14 — in-product = EV-3 clause
/// ONLY, D-5): an item `id`, when present, is a NON-EMPTY string. The
/// mint-set/unknown-id clause (EV-2) is A1-only: it needs the session mint
/// set (store state) that a single `(boundary, body)` pair cannot see.
fn check_h3(body: &Value) -> Vec<LintViolation> {
    let mut out = Vec::new();
    for (i, item) in input_items(body) {
        let Some(vid) = item.get("id") else {
            continue; // absent id → clean
        };
        if !vid.as_str().is_some_and(|s| !s.is_empty()) {
            out.push(LintViolation {
                rule: "H-3",
                path: format!("input[{i}].id"),
                observed: format!("id={vid:?}"),
            });
        }
    }
    out
}

/// H-4 (all responses-wire, EV-5): `store: false` on every request body.
/// Mirrors A1 exactly: the key ABSENT (or not `false`) fires.
fn check_h4(body: &Value) -> Vec<LintViolation> {
    if !body.is_object() {
        return Vec::new();
    }
    let store = body.get("store");
    if store != Some(&Value::Bool(false)) {
        let observed = match store {
            None => "store=null (absent)".to_string(),
            Some(v) => format!("store={v:?}"),
        };
        return vec![LintViolation {
            rule: "H-4",
            path: "body.store".into(),
            observed,
        }];
    }
    Vec::new()
}

/// H-5 (cross-boundary, EV-4/EV-14): `encitem_*` ids and `litellm_enc:`
/// blobs must NOT ride a non-Azure (VLLenient/Vertex) row — FIELD-SCOPED
/// (item `id` / `encrypted_content` values only; request substrings out
/// of scope). AzStrict = SILENT (the mint-domain match is undecidable
/// from strict-row request bytes — A1 is SILENT on AzStrict too; A1/A2
/// H-5' scopes identical, 3.2a).
///
/// SUPERSEDED 2026-09-23 (apex-ayl.126.8.6, 3.2a); v1 doc preserved:
/// "H-5 (cross-boundary, EV-4/EV-9/EV-14): `encitem_*` ids and
/// `litellm_enc:` blobs ride ONLY to the Azure row. In-product (A2):
/// non-Azure boundary = violation; AzStrict = clean (the mint-domain-match
/// clause needs the row's `x-litellm-tags` pin — state this value linter
/// does not receive; see the module docs for the delta note)."
fn check_h5(boundary: Boundary, body: &Value) -> Vec<LintViolation> {
    let mut out = Vec::new();
    for (i, item) in input_items(body) {
        // A1 parity (`_check_h5_encitem_azure_only`): the two hit
        // conditions are if/elif on the ITEM — `vid is a str AND starts
        // with encitem_`, else `enc is a str AND starts with
        // litellm_enc:`. A plain (non-encitem) id must NOT shadow the
        // blob arm.
        let vid = item.get("id").and_then(Value::as_str);
        let enc = item.get("encrypted_content").and_then(Value::as_str);
        let hit = if vid.is_some_and(|s| s.starts_with("encitem_")) {
            Some(("id", id_observed(vid.unwrap())))
        } else if enc.is_some_and(|s| s.starts_with("litellm_enc:")) {
            Some(("encrypted_content", enc_observed(enc.unwrap())))
        } else {
            None
        };
        let Some((field, observed)) = hit else {
            continue;
        };
        if boundary != Boundary::AzStrict {
            out.push(LintViolation {
                rule: "H-5",
                path: format!("input[{i}].{field}"),
                observed: format!("{observed} (boundary: {boundary:?})"),
            });
        }
    }
    out
}

/// A1 truncation for ids: `vid[:24] + "..."` beyond 24 chars.
fn id_observed(s: &str) -> String {
    if s.chars().count() <= 24 {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(24).collect::<String>())
    }
}

/// A1 truncation for `litellm_enc:` blobs: prefix + 12 chars of the blob
/// (`enc[12:24]`) + `...`.
fn enc_observed(s: &str) -> String {
    let rest = &s["litellm_enc:".len()..];
    let tail: String = rest.chars().take(12).collect();
    format!("litellm_enc:{tail}...")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::rules_generated;
    use serde_json::json;

    // ------------------------------------------------------------------
    // Fixture corpus (spec §3.3 / §5.1; EV ids in filenames, provenance
    // stamped per test below). Bodies are the verbatim wire bodies copied
    // from the campaign A1 fixtures (tools/fixtures/), body field only;
    // header fixtures are the string-valued headers of the same envelopes.
    // ------------------------------------------------------------------

    const H1_REJECT_CW1_REQ006: &str =
        include_str!("../../fixtures/outbound_lint/bodies/h1-reject-cw1-req006-EV-1.json");
    const H1_ACCEPT_CW1_REQ007: &str =
        include_str!("../../fixtures/outbound_lint/bodies/h1-accept-cw1-req007-EV-1.json");
    const H2_REJECT_SYNTHETIC: &str =
        include_str!("../../fixtures/outbound_lint/bodies/h2-reject-synthetic-EV-13.json");
    const H2_ACCEPT_MXAI_C04_REQ004: &str =
        include_str!("../../fixtures/outbound_lint/bodies/h2-accept-mxai-c04-req004-EV-9.json");
    const H3_REJECT_EV3: &str =
        include_str!("../../fixtures/outbound_lint/bodies/h3-reject-ev3-empty-id-EV-3.json");
    const H3_ACCEPT_UNKNOWN_ID: &str =
        include_str!("../../fixtures/outbound_lint/bodies/h3-accept-unknown-id-EV-2.json");
    const H4_REJECT_STORE_TRUE: &str =
        include_str!("../../fixtures/outbound_lint/bodies/h4-reject-store-true-EV-5.json");
    const H4_SCOPE_MESSAGES: &str =
        include_str!("../../fixtures/outbound_lint/bodies/h4-scope-messages-store-true-EV-5.json");
    const H5_REJECT_ENCITEM_VLLEN: &str =
        include_str!("../../fixtures/outbound_lint/bodies/h5-reject-encitem-vllenc-EV-4.json");
    const VERTEX_ACCEPT_MGW: &str =
        include_str!("../../fixtures/outbound_lint/bodies/vertex-accept-mgw-req004-OQf.json");
    const VLLEN_ACCEPT_SYNTHETIC: &str =
        include_str!("../../fixtures/outbound_lint/bodies/vllenc-accept-synthetic-EV-7.json");
    const H6_REJECT_EMPTY_TAGS: &str =
        include_str!("../../fixtures/outbound_lint/headers/h6-reject-empty-litellm-tags-EV-6.json");
    const H6_SCOPE_EMPTY_XGROK: &str =
        include_str!("../../fixtures/outbound_lint/headers/h6-scope-empty-xgrok-EV-6.json");

    fn fixture_body(name: &str) -> Value {
        serde_json::from_str(name)
            .unwrap_or_else(|e| panic!("fixture {name:?} is not a JSON object: {e}"))
    }

    fn fixture_header_pairs(name: &str) -> Vec<(String, String)> {
        let obj: serde_json::Map<String, Value> =
            serde_json::from_str(name).unwrap_or_else(|e| panic!("fixture {name:?}: {e}"));
        obj.into_iter()
            .map(|(k, v)| (k, v.as_str().unwrap_or_default().to_string()))
            .collect()
    }

    fn rules_of(violations: &[LintViolation]) -> Vec<&'static str> {
        violations.iter().map(|v| v.rule).collect()
    }

    // -- H-1 -----------------------------------------------------------

    /// H-1 REJECT — verbatim EV-1: CROSSWIRE-1 report 20260915T025451Z,
    /// arm `rt-xreplay1`, wire/req-006.json (gpt-5.6-sol AzStrict,
    /// store:false) → live 400 `Invalid 'input[7].content': array too
    /// long`. Two reasoning items (input[7], input[10]) carry populated
    /// content → H-1 x2, exactly.
    #[test]
    fn h1_reject_cw1_req006_fires_per_reasoning_item() {
        let body = fixture_body(H1_REJECT_CW1_REQ006);
        let v = lint_outbound_request(Boundary::AzStrict, &body);
        assert_eq!(
            rules_of(&v),
            vec!["H-1", "H-1"],
            "req-006 AzStrict must fire H-1 exactly twice (input[7] + input[10]), got {v:?}"
        );
        assert_eq!(v[0].path, "input[7].content");
        assert_eq!(v[1].path, "input[10].content");
    }

    /// H-1 ACCEPT — verbatim EV-1 known-good: CROSSWIRE-1
    /// 20260915T025451Z rt-xreplay1 wire/req-007.json (post-strip 200).
    /// No reasoning items ride → zero violations on AzStrict.
    #[test]
    fn h1_accept_cw1_req007_clean_azstrict() {
        let body = fixture_body(H1_ACCEPT_CW1_REQ007);
        assert!(
            lint_outbound_request(Boundary::AzStrict, &body).is_empty(),
            "req-007 (post-strip 200) must be clean on AzStrict"
        );
    }

    /// H-1 scope + shape pins: absent content and `[]` are clean on
    /// AzStrict; populated content on VLLenient is OUT of scope (AzStrict
    /// only — the lenient row tolerates everything, EV-7).
    #[test]
    fn h1_scope_and_shape_pins() {
        let base = |content: Value| {
            json!({
                "model": "gpt-5.6-sol",
                "store": false,
                "input": [
                    {"type": "reasoning", "summary": [], "content": content},
                ],
            })
        };
        // absent → clean
        let absent = json!({
            "store": false,
            "input": [{"type": "reasoning", "summary": []}],
        });
        assert!(lint_outbound_request(Boundary::AzStrict, &absent).is_empty());
        // explicit null → clean (A1 Python-None parity)
        let nullish = json!({
            "store": false,
            "input": [{"type": "reasoning", "summary": [], "content": null}],
        });
        assert!(lint_outbound_request(Boundary::AzStrict, &nullish).is_empty());
        // [] → clean
        let empty = base(Value::Array(vec![]));
        assert!(lint_outbound_request(Boundary::AzStrict, &empty).is_empty());
        // populated → fires on AzStrict
        let populated = base(json!([{"type": "reasoning_text", "text": "x"}]));
        assert_eq!(
            rules_of(&lint_outbound_request(Boundary::AzStrict, &populated)),
            vec!["H-1"]
        );
        // same body on VLLenient → out of scope (no H-1)
        assert!(lint_outbound_request(Boundary::VLLenient, &populated).is_empty());
    }

    // -- H-2 -----------------------------------------------------------

    /// H-2 REJECT — SYNTHESIZED EV-13 pin (no verbatim exists: D-ENC strips
    /// pre-send; the EV-13 lattice proves strict rows never carry it):
    /// campaign arm `h2-synthetic-strict-enc` (gpt-5.6-sol, reasoning item
    /// with `encrypted_content`) → H-2 once, exactly.
    ///
    /// SUPERSEDED 2026-09-23 (apex-ayl.126.8.6, HARDENING-SPEC 3.2a):
    /// the v1 rationale "no verbatim exists: D-ENC strips" is REFUTED for
    /// the OWN-origin form by the on-disk EV-9 accept capture (mxai-c04
    /// req-004 — own enc rides strict rows by design, 200). The v1
    /// key-presence assertion survives only as CLAUSE (a) semantics: this
    /// fixture ALREADY carries the pin (`mint_tag: "East US 2"`, verified
    /// 2026-09-23 — not re-done), so the re-scoped `check_h2` fires via
    /// clause (a) and the clause attribution is pinned below.
    #[test]
    fn h2_reject_synthetic_fires_key_presence() {
        let body = fixture_body(H2_REJECT_SYNTHETIC);
        let v = lint_outbound_request(Boundary::AzStrict, &body);
        assert_eq!(
            rules_of(&v),
            vec!["H-2"],
            "the H-2 arm must fire H-2 alone (H-5 is AzStrict-silent), got {v:?}"
        );
        assert_eq!(v[0].path, "input[1].encrypted_content");
        assert!(
            v[0].observed.contains("clause (a)") && v[0].observed.contains("mint_tag"),
            "clause (a) attribution must be named in observed, got {:?}",
            v[0].observed
        );
    }

    /// H-2 ACCEPT (wire-verified, the load-bearing pin) — VERBATIM EV-9:
    /// T8 arm `sight-1-mxai-c04` wire/req-004.json body (gpt-5.6-sol
    /// AzStrict, live 200): 7 reasoning items at input[8..14] with keys
    /// exactly {encrypted_content, summary, type} — no id, no mint_tag;
    /// all 7 blobs own-minted (the arm's resp-003.jsonl corpus). Driven
    /// through the real serialize→lint pipeline (fixture = the verbatim
    /// wire body; EV-8 pipeline-integrity pattern): the EXACT expected
    /// violation multiset for the whole body is the EMPTY set — H-2 does
    /// NOT fire (clause (a) false on all 7 items; H-5 AzStrict-silent;
    /// H-1/H-3/H-4 clean by shape).
    #[test]
    fn h2_accept_mxai_c04_req004_exact_multiset() {
        let body = fixture_body(H2_ACCEPT_MXAI_C04_REQ004);
        let v = lint_outbound_request(Boundary::AzStrict, &body);
        assert_eq!(
            v,
            Vec::<LintViolation>::new(),
            "mxai-c04 req-004 (EV-9 accept, 200) must lint to the EMPTY violation \
             multiset on AzStrict (H-2 clause (a) false; H-5 silent; H-1/H-3/H-4 \
             clean), got {v:?}"
        );
    }

    /// H-2 LA-03 split (i) — re-scoped 2026-09-23 (apex-ayl.126.8.6,
    /// HARDENING-SPEC 3.2a D-H2B): a null-valued `encrypted_content` with
    /// NO id/mint_tag is SILENT in A2 (clause (a) false). A1 still fires
    /// clause (b) on the non-string value (its pin
    /// `test_h2_null_valued_encrypted_content_fires` is untouched) — this
    /// is the named accepted divergence class D-H2B sub-form (i).
    ///
    /// SUPERSEDES the WIP pin `h2_null_value_still_fires` (2026-09-23):
    /// v1 asserted KEY PRESENCE fires for the null/no-pin shape — the
    /// over-warn interim form the re-scope removes (the on-disk EV-9
    /// accept capture adjudicates the no-pin shape by-design clean).
    #[test]
    fn h2_null_value_no_pin_silent_dh2b() {
        let body = json!({
            "store": false,
            "input": [
                {"type": "reasoning", "summary": [], "encrypted_content": null},
            ],
        });
        let v = lint_outbound_request(Boundary::AzStrict, &body);
        assert!(
            v.is_empty(),
            "D-H2B sub-form (i): null enc with NO id/mint_tag is SILENT in A2 \
             (clause (a) false; A1 clause (b) is A1-only), got {v:?}"
        );
    }

    /// H-2 LA-03 split (ii) — the null-value pin survives the re-scope
    /// WITH a pin: null-valued `encrypted_content` carrying `mint_tag`
    /// fires H-2 via CLAUSE (a) (KEY PRESENCE incl. null/empty on the pin
    /// field), clause attribution named in the observed string.
    ///
    /// SUPERSEDES (together with split (i)) the WIP pin
    /// `h2_null_value_still_fires` (2026-09-23, apex-ayl.126.8.6, 3.2a).
    #[test]
    fn h2_null_value_with_mint_tag_fires_clause_a() {
        let body = json!({
            "store": false,
            "input": [
                {"type": "reasoning", "summary": [], "mint_tag": "East US 2", "encrypted_content": null},
            ],
        });
        let v = lint_outbound_request(Boundary::AzStrict, &body);
        assert_eq!(
            rules_of(&v),
            vec!["H-2"],
            "null enc + mint_tag must fire H-2 via clause (a), got {v:?}"
        );
        assert_eq!(v[0].path, "input[0].encrypted_content");
        assert!(
            v[0].observed.contains("clause (a)") && v[0].observed.contains("mint_tag"),
            "clause (a) attribution must be named in observed, got {:?}",
            v[0].observed
        );
    }

    /// H-2 scope pins: the same strict fixture on VLLenient is out of
    /// scope for H-2 (its `litellm_enc:` blob legitimately fires H-5
    /// there — A1 parity); a NON-reasoning item carrying
    /// `encrypted_content` on AzStrict does not fire (A1 keys on
    /// `type == "reasoning"`; H-5 is AzStrict-clean in-product).
    #[test]
    fn h2_scope_pins() {
        let body = fixture_body(H2_REJECT_SYNTHETIC);
        assert_eq!(
            rules_of(&lint_outbound_request(Boundary::VLLenient, &body)),
            vec!["H-5"],
            "H-2 is AzStrict-scoped; only the blob's H-5 may fire"
        );
        let non_reasoning = json!({
            "store": false,
            "input": [
                {"type": "message", "role": "assistant", "content": [], "encrypted_content": "litellm_enc:abc"},
            ],
        });
        assert!(lint_outbound_request(Boundary::AzStrict, &non_reasoning).is_empty());
    }

    // -- H-3 -----------------------------------------------------------

    /// H-3 REJECT — DOCUMENT-SOURCED EV-3 (crosswire-preplan-qwen-20260917
    /// cell 1; incident updates.jsonl:78): gpt-5.6-terra store:false body
    /// with `id: ''` at input[6] → live 400 `Invalid 'input[6].id': ''`.
    #[test]
    fn h3_reject_ev3_empty_id_fires() {
        let body = fixture_body(H3_REJECT_EV3);
        let v = lint_outbound_request(Boundary::AzStrict, &body);
        assert_eq!(
            rules_of(&v),
            vec!["H-3"],
            "the EV-3 arm must fire H-3 alone, got {v:?}"
        );
        assert_eq!(v[0].path, "input[6].id");
    }

    /// H-3 shape pins: non-string ids fire; an ABSENT id is clean.
    #[test]
    fn h3_non_string_id_fires_absent_id_clean() {
        for vid in [json!(42), json!(null)] {
            let body = json!({
                "store": false,
                "input": [{"type": "reasoning", "id": vid, "summary": []}],
            });
            let v = lint_outbound_request(Boundary::AzStrict, &body);
            assert_eq!(rules_of(&v), vec!["H-3"], "id={vid} must fire H-3");
        }
        let absent = json!({
            "store": false,
            "input": [{"type": "reasoning", "summary": []}],
        });
        assert!(lint_outbound_request(Boundary::AzStrict, &absent).is_empty());
    }

    /// H-3 D-5 ASYMMETRY PIN: the A1 `h3-synthetic-unknown-id` arm
    /// (store:false AzStrict gpt-5.6-sol carrying the fabricated
    /// `rs_deadbeef…` id, absent from any session mint set) FAILS H-3's
    /// unknown-id clause in A1 — but A2 checks non-emptiness ONLY (the
    /// mint set is store state; D-5). In-product: CLEAN.
    #[test]
    fn h3_unknown_id_clean_in_product_d5() {
        let body = fixture_body(H3_ACCEPT_UNKNOWN_ID);
        assert!(
            lint_outbound_request(Boundary::AzStrict, &body).is_empty(),
            "A2 must NOT fire on the unknown-id arm (D-5: mint-set clause is A1-only)"
        );
    }

    // -- H-4 -----------------------------------------------------------

    /// H-4 REJECT — SYNTHESIZED EV-5 (deployment-contract standing rule,
    /// never a wire capture): campaign arm `h4-synthetic-store-true`
    /// (qwen3.8-27b VLLenient, `store: true`) → H-4 once, exactly.
    #[test]
    fn h4_reject_store_true_fires() {
        let body = fixture_body(H4_REJECT_STORE_TRUE);
        let v = lint_outbound_request(Boundary::VLLenient, &body);
        assert_eq!(rules_of(&v), vec!["H-4"]);
        assert_eq!(v[0].path, "body.store");
    }

    /// H-4 A1-parity pin: `store` ABSENT also fires on responses-wire
    /// bodies (A1: `body.get("store") is not False`).
    #[test]
    fn h4_absent_store_fires_responses_wire() {
        for boundary in [Boundary::AzStrict, Boundary::VLLenient] {
            let body = json!({"model": "m", "input": []});
            let v = lint_outbound_request(boundary, &body);
            assert_eq!(
                rules_of(&v),
                vec!["H-4"],
                "absent store must fire on {boundary:?}"
            );
        }
    }

    /// H-4 SCOPE pin: `store: true` on a MESSAGES-wire (Vertex) body must
    /// NOT fire — the rule is responses-wire-only ("all-responses").
    #[test]
    fn h4_scope_messages_wire_clean() {
        let body = fixture_body(H4_SCOPE_MESSAGES);
        assert!(
            lint_outbound_request(Boundary::Vertex, &body).is_empty(),
            "store:true on the messages wire is out of H-4 scope"
        );
    }

    // -- H-5 -----------------------------------------------------------

    /// H-5 REJECT — RECORD-SOURCED EV-4 (probe D 2026-09-22, ledger:3181 +
    /// EXEMPLARS:123 verbatim 503): campaign arm `ev4-encitem-503-stub`
    /// (qwen3.8-27b VLLenient, `encitem_…` carrier id + mint_tag 'East US
    /// 2') → H-5 once, exactly.
    #[test]
    fn h5_reject_encitem_vllenc_fires() {
        let body = fixture_body(H5_REJECT_ENCITEM_VLLEN);
        let v = lint_outbound_request(Boundary::VLLenient, &body);
        assert_eq!(
            rules_of(&v),
            vec!["H-5"],
            "the EV-4 stub must fire H-5 alone (encitem_ on VLLenient), got {v:?}"
        );
        assert_eq!(v[0].path, "input[2].id");
    }

    /// H-5 blob arm: a `litellm_enc:` blob (no `encitem_` id) on a
    /// non-Azure boundary fires on the `encrypted_content` field.
    #[test]
    fn h5_litellm_enc_blob_fires_on_vllenc() {
        let body = json!({
            "store": false,
            "input": [
                {"type": "reasoning", "id": "rs_000000000000000000000000", "summary": [],
                 "encrypted_content": "litellm_enc:0123456789abcdef0123456789"},
            ],
        });
        let v = lint_outbound_request(Boundary::VLLenient, &body);
        assert_eq!(rules_of(&v), vec!["H-5"]);
        assert_eq!(v[0].path, "input[0].encrypted_content");
        assert!(
            v[0].observed.starts_with("litellm_enc:0123456789ab..."),
            "A1 truncation: prefix + 12 blob chars; got {:?}",
            v[0].observed
        );
    }

    /// H-5 AzStrict-SILENT pin (= H-5' row-class clause; re-scoped
    /// 2026-09-23, apex-ayl.126.8.6, 3.2a): `encitem_` on the AzStrict
    /// boundary is SILENT (the mint-domain match is undecidable from
    /// strict-row request bytes — A1 is SILENT on AzStrict too, scopes
    /// identical; the enc-affinity gate upstream is the runtime
    /// authority). Plus the strict-side known-good (CROSSWIRE-1 req-007,
    /// no enc ids) stays clean.
    ///
    /// SUPERSEDED 2026-09-23 (apex-ayl.126.8.6, 3.2a); v1 doc preserved:
    /// "H-5 A2 delta pin: `encitem_` on the AzStrict boundary is CLEAN
    /// in-product (the mint-domain-match clause needs the row pin; the
    /// enc-affinity gate upstream is the runtime authority)."
    #[test]
    fn h5_azstrict_clean_in_product() {
        let body = json!({
            "store": false,
            "input": [
                {"type": "reasoning", "id": "encitem_0000000000000000000000000000", "mint_tag": "East US 2", "summary": []},
            ],
        });
        assert!(
            lint_outbound_request(Boundary::AzStrict, &body).is_empty(),
            "AzStrict enc rides are clean in-product (pin not visible; gate authority)"
        );
        let strict_accept = fixture_body(H1_ACCEPT_CW1_REQ007);
        assert!(lint_outbound_request(Boundary::AzStrict, &strict_accept).is_empty());
    }

    /// H-5 scope pin: a Vertex-boundary body carrying an `input` list with
    /// an `encitem_` id fires (cross-boundary = any object body; real
    /// messages-wire bodies have no `input` and are no-ops).
    #[test]
    fn h5_encitem_vertex_boundary_fires() {
        let body = json!({
            "store": false,
            "input": [{"type": "reasoning", "id": "encitem_ffffffffffffffffffffffff", "summary": []}],
        });
        let v = lint_outbound_request(Boundary::Vertex, &body);
        assert_eq!(rules_of(&v), vec!["H-5"]);
    }

    // -- H-6 -----------------------------------------------------------

    /// H-6 REJECT — VERBATIM EV-6: TAGPROBE-2 (2026-09-23, /tmp/tagprobe-wire/
    /// → campaign arm `tagprobe-2`) wire/req-002.json string headers:
    /// `x-litellm-tags: ""` → live 401 `tags=['']` (4/4 turns). The same
    /// fixture carries empty `x-grok-*` values — scope pin: ONLY
    /// x-litellm-tags fires.
    #[test]
    fn h6_reject_empty_litellm_tags_fires() {
        let pairs = fixture_header_pairs(H6_REJECT_EMPTY_TAGS);
        let refs: Vec<(&str, &str)> = pairs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let v = lint_outbound_headers(&refs);
        assert_eq!(
            rules_of(&v),
            vec!["H-6"],
            "only the empty x-litellm-tags may fire (empty x-grok-* are out of scope, OQ-f), got {v:?}"
        );
        assert_eq!(v[0].path, "header:x-litellm-tags");
    }

    /// H-6 SCOPE pin — VERBATIM OQ-f counter-evidence: mgw-toolctl-01
    /// report 20260921T232013Z wire/req-004.json (claude-sonnet-5 title
    /// call, 200) carries FOUR empty `x-grok-*` headers and NO
    /// x-litellm-tags → must be clean.
    #[test]
    fn h6_scope_empty_xgrok_clean() {
        let pairs = fixture_header_pairs(H6_SCOPE_EMPTY_XGROK);
        let refs: Vec<(&str, &str)> = pairs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert!(
            lint_outbound_headers(&refs).is_empty(),
            "empty x-grok-* headers must NOT trip H-6 (mgw req-004 known-good)"
        );
    }

    /// H-6 shape pins: a NON-EMPTY x-litellm-tags is clean; the name match
    /// is case-insensitive (A1 `_header_value`).
    #[test]
    fn h6_nonempty_and_case_pins() {
        assert!(
            lint_outbound_headers(&[("x-litellm-tags", "East US 2")]).is_empty(),
            "non-empty tags are clean"
        );
        assert_eq!(
            rules_of(&lint_outbound_headers(&[("X-LITELLM-TAGS", "")])),
            vec!["H-6"]
        );
    }

    // -- known-good sweep + misc ----------------------------------------

    /// T5 RED-shape sweep: the known-good body per boundary is clean —
    /// AzStrict (CROSSWIRE-1 req-007 verbatim), VLLenient (synthesized
    /// EV-7 shape: minted rs_ + both text lanes, store:false), Vertex
    /// (mgw-toolctl-01 req-004 verbatim, messages wire).
    #[test]
    fn known_good_sweep_per_boundary() {
        let az = fixture_body(H1_ACCEPT_CW1_REQ007);
        assert!(lint_outbound_request(Boundary::AzStrict, &az).is_empty());

        let vl = fixture_body(VLLEN_ACCEPT_SYNTHETIC);
        assert!(lint_outbound_request(Boundary::VLLenient, &vl).is_empty());

        let vx = fixture_body(VERTEX_ACCEPT_MGW);
        assert!(lint_outbound_request(Boundary::Vertex, &vx).is_empty());
    }

    /// Non-object bodies (A1 `body_kind != "object"`) are clean for every
    /// body-level rule and never panic.
    #[test]
    fn non_object_body_clean_no_panic() {
        for boundary in [Boundary::AzStrict, Boundary::VLLenient, Boundary::Vertex] {
            let body = Value::String("GET / HTTP/1.1".into());
            assert!(lint_outbound_request(boundary, &body).is_empty());
        }
    }

    /// Registry completeness: every table row's `check` name resolves in
    /// the dispatch (and the dispatch covers exactly the table) — the pin
    /// that makes the dispatch's unknown-check arm unreachable.
    #[test]
    fn rule_table_registry_complete() {
        const DISPATCHED: &[&str] = &[
            "reasoning_content_absent_or_empty",
            "reasoning_no_encrypted_content",
            "item_id_nonempty_and_minted",
            "store_false",
            "encitem_only_to_matching_azure_row",
            "no_empty_x_litellm_tags",
        ];
        let table: Vec<&str> = HARD_RULES.iter().map(|r| r.check).collect();
        for check in DISPATCHED {
            assert!(
                table.contains(check),
                "dispatch arm {check:?} has no table row"
            );
        }
        for rule in HARD_RULES {
            assert!(
                DISPATCHED.contains(&rule.check),
                "table row {} has an unknown check name {:?}",
                rule.id,
                rule.check
            );
        }
        assert_eq!(
            HARD_RULES.len(),
            DISPATCHED.len(),
            "table and dispatch must cover the same 6 rules"
        );
    }

    // ------------------------------------------------------------------
    // T7 drift pins (Rust side): the checked-in projection fixture and the
    // GENERATED table must agree with each other. The campaign JSON ->
    // crate-file drift is pinned by the generator's `--check` mode.
    // ------------------------------------------------------------------

    /// FIPS 180-4 known-answer pins for the self-contained `sha256_hex`
    /// below. `sha2` is not a dependency of this crate and `projection`'s
    /// private sha256 is outside the allowed set, so the drift test
    /// carries its own implementation — pinned independently of the
    /// checksum test that trusts it.
    #[test]
    fn sha256_kat_pins() {
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// T7 drift pin: SHA-256 of the checked-in hard-rules projection
    /// fixture == the generated `HARD_RULES_CHECKSUM`, and the fixture
    /// rows == `HARD_RULES` field by field.
    #[test]
    fn generated_table_matches_checked_in_projection() {
        const FIXTURE: &str = include_str!("../../fixtures/outbound_lint/hard_rules.json");
        assert_eq!(
            sha256_hex(FIXTURE),
            rules_generated::HARD_RULES_CHECKSUM,
            "fixture bytes drifted from the checked-in checksum"
        );
        let doc: Value = serde_json::from_str(FIXTURE).expect("fixture is JSON");
        let rows = doc
            .get("hard_rules")
            .and_then(Value::as_array)
            .expect("hard_rules array");
        assert_eq!(rows.len(), HARD_RULES.len(), "row count drift");
        for (row, rule) in rows.iter().zip(HARD_RULES) {
            assert_eq!(row.get("id").and_then(Value::as_str), Some(rule.id));
            assert_eq!(row.get("class").and_then(Value::as_str), Some(rule.class));
            assert_eq!(
                row.get("severity").and_then(Value::as_str),
                Some(rule.severity)
            );
            let ev: Vec<&str> = row
                .get("ev")
                .and_then(Value::as_array)
                .expect("ev array")
                .iter()
                .filter_map(Value::as_str)
                .collect();
            assert_eq!(ev.as_slice(), rule.ev, "ev drift for {}", rule.id);
            assert_eq!(row.get("check").and_then(Value::as_str), Some(rule.check));
            assert_eq!(
                row.get("description").and_then(Value::as_str),
                Some(rule.description)
            );
        }
    }

    /// Self-contained SHA-256 (hex) — FIPS 180-4, see `sha256_kat_pins`.
    fn sha256_hex(input: &str) -> String {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        let mut data = input.as_bytes().to_vec();
        data.push(0x80);
        while data.len() % 64 != 56 {
            data.push(0);
        }
        data.extend_from_slice(&((input.len() as u64).wrapping_mul(8)).to_be_bytes());

        for chunk in data.chunks_exact(64) {
            let mut w = [0u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes([
                    chunk[4 * i],
                    chunk[4 * i + 1],
                    chunk[4 * i + 2],
                    chunk[4 * i + 3],
                ]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }
            let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
                (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let t1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            h[0] = h[0].wrapping_add(a);
            h[1] = h[1].wrapping_add(b);
            h[2] = h[2].wrapping_add(c);
            h[3] = h[3].wrapping_add(d);
            h[4] = h[4].wrapping_add(e);
            h[5] = h[5].wrapping_add(f);
            h[6] = h[6].wrapping_add(g);
            h[7] = h[7].wrapping_add(hh);
        }
        h.iter().map(|w| format!("{w:08x}")).collect()
    }
}
