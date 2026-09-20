//! WAVE-C unit REDs for the .71 switch-time projector (sdd-71-projector.md §8 Table 2).
//!
//! PRE-GREEN (this cut): the `projection` module does not exist yet — every test
//! below fails at compile stage on `use super::projection::...` (E0432 unresolved
//! import `super::projection`; the coordinator's E0425-class compile RED). The
//! module's absence IS the RED (sdd-71 §9: the GREEN cut lands
//! `conversation/projection.rs` + the `pub mod projection;` line; this file and
//! its `#[path]` wiring are the only .71-WAVE-C additions to this crate).
//!
//! POST-GREEN expectation (sdd-71 §8 G1): the cases fail on the documented
//! invariant violations until the projector is complete, then 8/8 + E2 pin
//! flip for the right reason (assertion, not compile).
//!
//! Fixture discipline: `fixtures/projection_x71/` are byte mirrors of the
//! `smoke/xwfix` corpus cells (PROVENANCE.md there holds the source shas).
//! NEVER edit a mirror (or any pin) to fit a RED (redcycle STOP rule 1).
//!
//! API assumptions flagged to the coordinator (prep report D1/D2): §9 names
//! `project_switch_history` exactly; the `Boundary` variant set
//! (`AzStrict`/`VLLenient`/`Vertex`) and the `ProjectedHistory::items` payload
//! field are the §9-faithful minimal choices — `proj_items` is the single
//! choke point if the GREEN cut names them differently.

use super::projection::{model_boundary_class, project_switch_history, Boundary, ProjectedHistory};
use super::*;

/// The T2 co-projection placeholder — the pairing-integrity remedy (sdd-71 §4
/// invariant 1). Source: `xai-chat-state/src/compaction_utils.rs:505` @40ffad1
/// (tail-retention ToolResult placeholder, first-hand).
const T2_PLACEHOLDER: &str = "Tool call omitted...";

/// Byte-mirrored corpus fixtures (sha256:12 in fixtures/projection_x71/PROVENANCE.md).
const VXM_AZ_PRE: &str = include_str!("fixtures/projection_x71/vxm_az_pre.json");
const VXM_AZ_EXPECTED: &str = include_str!("fixtures/projection_x71/vxm_az_expected.json");
const AZ_VLQ_PRE: &str = include_str!("fixtures/projection_x71/az_vlq_pre.json");
const AZ_VLQ_EXPECTED: &str = include_str!("fixtures/projection_x71/az_vlq_expected.json");
const ORPHAN_SHAPE: &str = include_str!("fixtures/projection_x71/orphan_shape.json");

/// 12/12 known-answer ID-grammar goldens (sdd-71 §5; tdd-69 §2.5; xwfix corpus
/// vxm-az 5/5 + az-vlq 7/7, reproduced byte-for-byte first-hand 2026-09-18).
const VXMAZ_GOLDEN_IDS: [&str; 5] = [
    "xw_bcf9e9828796d9d08c350f8a",
    "xw_f95ed93e0a9badbaccd12afa",
    "xw_7157485b1997e6654dea85d5",
    "xw_55ab10bbffec861ca557f6e6",
    "xw_46a650fad66e55e6928d41b2",
];
const AZVLQ_GOLDEN_IDS: [&str; 7] = [
    "xw_cccf0712784d7b2cdbe38fe3",
    "xw_049bc4d57124ddb9c103b0ee",
    "xw_370feb6aa86c1169d2062546",
    "xw_eab002c1494628d09f653e47",
    "xw_646840d2ca07387222246204",
    "xw_10111cbf8bf63361b34fd03d",
    "xw_c07ab749699b149f6f58d57e",
];

/// Table 2 case 1 (RED-hunt FIRST, sdd-71 §8) — PAIRING INTEGRITY.
///
/// `[user, BackendToolCall(fc_x), ToolResult(fc_x), assistant]` projected to a
/// tier that drops the call: the VX-M/vertex floor (the `/messages` wire has no
/// site for backend tool call items — sdd-71 §3 row "VL → VX-M", matrix §4.2
/// item 5: today structurally unreachable, the projector is the first partial
/// strip that reaches the orphaned-RESULT class). The ToolResult must be
/// co-projected to the T2 placeholder OR both dropped — NO orphaned ToolResult
/// in the output.
///
/// Right-reason failure after the fn lands: the orphan survives (invariant-1
/// violation).
#[test]
fn xw_proj_orphaned_result_direction() {
    let (items, _) = items_from_fixture(ORPHAN_SHAPE);
    let history = project_switch_history(
        &items,
        "claude-sonnet-5",
        Boundary::Vertex,
        None,
    );
    let out = proj_items(&history);
    let orphaned = out.iter().any(|item| {
        matches!(
            item,
            ConversationItem::ToolResult(tr)
                if tr.tool_call_id == "fc_x"
                    && tr.content.as_ref() != T2_PLACEHOLDER
                    && !call_id_present(out, "fc_x")
        )
    });
    assert!(
        !orphaned,
        "invariant 1 violated: orphaned ToolResult(fc_x) in projected output — \
         call dropped, result neither co-projected to the T2 placeholder nor dropped"
    );
}

/// Table 2 case 2 — CARRIER SURVIVAL (sdd-71 §4 invariant 2).
///
/// History with a `CodexRawInput` carrier + foreign reasoning → project → the
/// carrier survives byte-identical/opaque (the H-2 silent-loss class the
/// projector must not reproduce at switch time), the reasoning is projected
/// per floor.
///
/// Right-reason failure after the fn lands: carriers mutated/dropped.
#[test]
fn xw_proj_carrier_survival() {
    let (items, values) = items_from(carrier_shape());
    let history = project_switch_history(
        &items,
        "qwen3.8-27b",
        Boundary::VLLenient,
        None,
    );
    let out = proj_items(&history);
    assert_eq!(out.len(), 4, "carrier shape: projected record count changed");
    assert_eq!(
        as_value(&out[1]),
        values[1],
        "invariant 2 violated: CodexRawInput carrier must survive byte-identical/opaque"
    );
    let ConversationItem::Reasoning(r) = &out[2] else {
        panic!("carrier shape: reasoning slot lost in projection");
    };
    assert!(
        r.id.starts_with("xw_"),
        "foreign reasoning beside the carrier must be projected per floor (T1 re-key), got {:?}",
        r.id
    );
    assert!(
        r.encrypted_content.is_none(),
        "foreign encrypted_content must be stripped"
    );
    assert_eq!(r.summary.len(), 1, "summary must be kept");
}

/// Table 2 case 3 — the §3 decision-table rows as L0 pins.
///
/// sol→sol T0-KEEP · sol→qwen T1 re-key+strip · sol→terra T1-by-target NO
/// re-key · qwen→glm T1 cross-deployment · sonnet→sonnet T0 · sonnet→qwen
/// family-None T1 empty-id.
///
/// Right-reason failure after the fn lands: per-row tier/boundary mismatch.
#[test]
fn xw_proj_boundary_decision_table() {
    // (a) sol→sol — T0 KEEP (same model = same boundary, sdd-71 §2 rule (i);
    // post-.75 same-boundary default: KEEP id + field markers).
    {
        let (items, values) = boundary_row("gpt-5.6-sol", "encitem_self", "enc_self");
        let history = project_switch_history(
            &items,
            "gpt-5.6-sol",
            Boundary::AzStrict,
            None,
        );
        let out = proj_items(&history);
        assert_eq!(out.len(), 3, "sol→sol: projected record count changed");
        assert_eq!(
            as_value(&out[1]),
            values[1],
            "sol→sol: T0-KEEP must be byte-identical (id + encrypted kept, same boundary)"
        );
    }
    // (b) sol→qwen — T1 re-key + strip (foreign reasoning on a lenient target).
    {
        let (items, _) = boundary_row("gpt-5.6-sol", "encitem_foreign", "enc_foreign");
        let history = project_switch_history(
            &items,
            "qwen3.8-27b",
            Boundary::VLLenient,
            None,
        );
        let out = proj_items(&history);
        let ConversationItem::Reasoning(r) = &out[1] else {
            panic!("sol→qwen: reasoning slot lost");
        };
        assert!(
            r.id.starts_with("xw_"),
            "sol→qwen: T1 must re-key to an xw_ id, got {:?}",
            r.id
        );
        assert_ne!(r.id, "encitem_foreign");
        assert!(
            r.encrypted_content.is_none(),
            "sol→qwen: foreign encrypted_content must be stripped"
        );
        assert_eq!(r.summary.len(), 1, "sol→qwen: summary must be kept");
    }
    // (c) sol→terra — T1-by-target, NO re-key (the strict projector IS the
    // strip: it removes id+content pre-send; the store keeps the original id).
    // mf6 (apex-mf6, design §3.4): the AZ->AZ row's ciphertext is
    // gate-decided — here pin None x mint None = RetainOptimistic, so the
    // ciphertext SURVIVES the store projection (the .71 unconditional strip
    // is replaced by the gate; the wire layer + reactive fallback decide
    // the rest at send time).
    {
        let (items, _) = boundary_row("gpt-5.6-sol", "encitem_foreign", "enc_foreign");
        let history = project_switch_history(
            &items,
            "gpt-5.6-terra",
            Boundary::AzStrict,
            None,
        );
        let out = proj_items(&history);
        let ConversationItem::Reasoning(r) = &out[1] else {
            panic!("sol→terra: reasoning slot lost");
        };
        assert_eq!(
            r.id, "encitem_foreign",
            "sol→terra: strict target must NOT re-key (schema form IS the strip), got {:?}",
            r.id
        );
        assert_eq!(
            r.item.encrypted_content.as_deref(),
            Some("enc_foreign"),
            "sol→terra (unpinned x untagged): RetainOptimistic — the ciphertext \
             must SURVIVE the store projection (mf6 gate, design §3.4)"
        );
        assert_eq!(r.summary.len(), 1, "sol→terra: summary must be kept");
    }
    // (d) qwen→glm — T1 cross-deployment (VL→VL: own is T0, foreign is T1;
    // qwen↔glm are distinct vLLM deployments, matrix C4: never lump).
    {
        let (items, _) = boundary_row("qwen3.8-27b", "rs_qwen_1", "enc_qwen");
        let history = project_switch_history(&items, "glm-5.2", Boundary::VLLenient, None);
        let out = proj_items(&history);
        let ConversationItem::Reasoning(r) = &out[1] else {
            panic!("qwen→glm: reasoning slot lost");
        };
        assert!(
            r.id.starts_with("xw_"),
            "qwen→glm: foreign (cross-deployment) reasoning must be T1 re-keyed, got {:?}",
            r.id
        );
        assert!(
            r.encrypted_content.is_none(),
            "qwen→glm: foreign encrypted_content must be stripped"
        );
        assert_eq!(r.summary.len(), 1, "qwen→glm: summary must be kept");
    }
    // (e) sonnet→sonnet — T0 (same model, D5: verbatim).
    {
        let (items, values) = boundary_row("claude-sonnet-5", "rs_sonnet_1", "enc_sonnet");
        let history = project_switch_history(
            &items,
            "claude-sonnet-5",
            Boundary::Vertex,
            None,
        );
        let out = proj_items(&history);
        assert_eq!(
            as_value(&out[1]),
            values[1],
            "sonnet→sonnet: same-model T0 must be verbatim"
        );
    }
    // (f) sonnet→qwen — family-None T1 empty-id (the FRESH-CATCH class:
    // is_family_switch is false for family-unset rows, but the projector must
    // still fire; the empty-id mint is stream/messages.rs:543-549).
    {
        let (items, _) = boundary_row("claude-sonnet-5", "", "enc_sonnet_empty");
        let history = project_switch_history(
            &items,
            "qwen3.8-27b",
            Boundary::VLLenient,
            None,
        );
        let out = proj_items(&history);
        let ConversationItem::Reasoning(r) = &out[1] else {
            panic!("sonnet→qwen: reasoning slot lost");
        };
        assert!(
            !r.id.is_empty(),
            "sonnet→qwen: empty-id mint must never survive projection (invariant 4)"
        );
        assert!(
            r.id.starts_with("xw_"),
            "sonnet→qwen: T1 must re-key the empty id to an xw_ id"
        );
        assert!(
            r.encrypted_content.is_none(),
            "sonnet→qwen: foreign encrypted_content must be stripped"
        );
    }
}

/// Table 2 case 4 — 12/12 known-answer ID-grammar pin via
/// `py_json_canonicalize` parity (sdd-71 §5: Python-default canonicalization —
/// sorted keys, `", "`/`": "` separators, `ensure_ascii`; serde_json's compact
/// separators do NOT reproduce the goldens).
///
/// The `{cell}` slot is the fixture's cell name, passed explicitly via the §9
/// API's model-id argument (sdd-71 §5: "In L0 unit tests the fixture's cell
/// name is passed explicitly so the goldens reproduce").
///
/// Right-reason failure after the fn lands: serde_json compact-separator
/// mismatch (wrong hash → wrong id).
#[test]
fn xw_proj_id_grammar_canonical() {
    let (items, _) = items_from_fixture(VXM_AZ_PRE);
    let history = project_switch_history(
        &items,
        "vxm-az",
        Boundary::AzStrict,
        None,
    );
    let out = proj_items(&history);
    assert_eq!(
        reasoning_ids(out),
        VXMAZ_GOLDEN_IDS,
        "vxm-az 5/5 known-answer ids (py_json_canonicalize parity)"
    );

    let (items, _) = items_from_fixture(AZ_VLQ_PRE);
    let history = project_switch_history(
        &items,
        "az-vlq",
        Boundary::VLLenient,
        None,
    );
    let out = proj_items(&history);
    assert_eq!(
        reasoning_ids(out),
        AZVLQ_GOLDEN_IDS,
        "az-vlq 7/7 known-answer ids (py_json_canonicalize parity)"
    );
}

/// Table 2 case 5 — BYTE-IDENTITY of non-projected (sdd-71 §4 invariant 6, the
/// over-strip guard). Every record whose expected mirror is unchanged (T0)
/// must come back byte-identical (parsed-equal — the corpus's own
/// verification standard) to its storage form.
///
/// Right-reason failure after the fn lands: incidental mutation.
#[test]
fn xw_proj_byte_identity_nonprojected() {
    for (pre_json, exp_json, target, boundary) in [
        (VXM_AZ_PRE, VXM_AZ_EXPECTED, "gpt-5.6-terra", Boundary::AzStrict),
        (AZ_VLQ_PRE, AZ_VLQ_EXPECTED, "qwen3.8-27b", Boundary::VLLenient),
    ] {
        let (items, pre_values) = items_from_fixture(pre_json);
        let expected: Vec<serde_json::Value> =
            serde_json::from_str(exp_json).expect("expected mirror must be valid JSON");
        let history = project_switch_history(&items, target, boundary, None);
        let out = proj_items(&history);
        assert_eq!(
            out.len(),
            pre_values.len(),
            "{target}: projected record count changed"
        );
        for (i, (pv, ev)) in pre_values.iter().zip(expected.iter()).enumerate() {
            if pv == ev {
                assert_eq!(
                    as_value(&out[i]),
                    *pv,
                    "{target}: record {i} mutated — T0 (non-projected) records must be \
                     byte-identical to the storage form"
                );
            }
        }
    }
}

/// Table 2 case 6 — IDEMPOTENCE (sdd-71 §4 invariant 3):
/// `project(project(h)) == project(h)` over all Table-1 shapes (the two
/// mirrored corpus cells) + every Table-2 synthetic shape.
///
/// Right-reason failure after the fn lands: double re-key / double strip.
#[test]
fn xw_proj_idempotence() {
    for (items, target, boundary) in all_table2_shapes() {
        let first = project_switch_history(&items, &target, boundary, None);
        let first_values: Vec<serde_json::Value> =
            proj_items(&first).iter().map(as_value).collect();
        let second = project_switch_history(proj_items(&first), &target, boundary, None);
        let second_values: Vec<serde_json::Value> =
            proj_items(&second).iter().map(as_value).collect();
        assert_eq!(
            second_values, first_values,
            "project(project(h)) != project(h) for target {target}"
        );
    }
}

/// Table 2 case 7 — invariants 4 + 5 (sdd-71 §4) as single asserts over EVERY
/// projected output: no reasoning item has `id:""` (the .69 class), and
/// encrypted_content rides only same-boundary (owner model == target model)
/// — EXCEPT the mf6 (apex-mf6) re-arm of the one no-re-key AZ->AZ row, where
/// the switch-time gate may retain the ciphertext (design §3.4: every shape
/// in this table is pre-mf6 / untagged, so only the RetainOptimistic arm —
/// pin None x mint None — can keep it).
///
/// Right-reason failure after the fn lands: `id:""` or cross-boundary
/// ciphertext present outside the gate's retain arms.
#[test]
fn xw_proj_no_empty_id_no_foreign_encrypted() {
    for (items, target, boundary) in all_table2_shapes() {
        let out = proj_items(&project_switch_history(&items, &target, boundary, None)).to_vec();
        for (i, item) in out.iter().enumerate() {
            let ConversationItem::Reasoning(r) = item else {
                continue;
            };
            assert!(
                !r.id.is_empty(),
                "invariant 4 ({target}): empty reasoning id at projected index {i} (the .69 class)"
            );
            if r.encrypted_content.is_some() {
                let owner = forward_owner_model(&out, i);
                // mf6 re-arm: the AZ->AZ row is the one no-re-key row where
                // the gate may retain foreign (cross-deployment) ciphertext —
                // here pin None (every shape in this table passes None) x
                // the item's mint tag.
                let az_az_gate_retain = boundary == Boundary::AzStrict
                    && owner
                        .as_deref()
                        .map(model_boundary_class)
                        == Some(Boundary::AzStrict)
                    && matches!(
                        enc_affinity_gate(None, r.mint_tag.as_deref()),
                        EncAffinityVerdict::Retain | EncAffinityVerdict::RetainOptimistic
                    );
                assert!(
                    owner.as_deref() == Some(target.as_str()) || az_az_gate_retain,
                    "invariant 5 ({target}): foreign encrypted_content survived projection \
                     at index {i} (owner {owner:?}) — allowed only T0 or the AZ->AZ \
                     row under a gate retain arm"
                );
            }
        }
    }
}

/// Table 2 case 8 — over-strip guard: a call+result pair the target CAN
/// consume (client-side tool calls ride the `/v1/responses` wire on the VL
/// target) survives projection with BOTH items intact — no gratuitous T3 (no
/// placeholder, no drop).
///
/// Right-reason failure after the fn lands: pair dropped.
#[test]
fn xw_proj_surviving_call_keeps_result() {
    let (items, values) = items_from(surviving_pair_shape());
    let history = project_switch_history(
        &items,
        "qwen3.8-27b",
        Boundary::VLLenient,
        None,
    );
    let out = proj_items(&history);
    assert_eq!(
        out.len(),
        5,
        "surviving pair: projected record count changed (gratuitous T3?)"
    );
    assert_eq!(
        as_value(&out[1]),
        values[1],
        "over-strip: assistant tool-call item mutated"
    );
    assert_eq!(
        as_value(&out[2]),
        values[2],
        "over-strip: tool result dropped or placeholderized"
    );
    let ConversationItem::Reasoning(r) = &out[3] else {
        panic!("surviving pair: reasoning slot lost");
    };
    assert!(
        r.id.starts_with("xw_"),
        "foreign reasoning beside the surviving pair must still be projected, got {:?}",
        r.id
    );
    assert!(
        r.encrypted_content.is_none(),
        "foreign encrypted_content must be stripped"
    );
}

// ============================================================================
// Helpers
// ============================================================================

/// Sole access point to the §9-underspecified `ProjectedHistory` payload shape
/// (prep report decision D2 — the GREEN cut must keep a public `items` field
/// or adjust this one line only).
fn proj_items(history: &ProjectedHistory) -> &[ConversationItem] {
    &history.items
}

fn as_value(item: &ConversationItem) -> serde_json::Value {
    serde_json::to_value(item).expect("ConversationItem must serialize")
}

/// Parse a storage-form JSON array of records into typed items + the raw
/// per-record Values (for parsed-equality assertions).
fn items_from(arr: serde_json::Value) -> (Vec<ConversationItem>, Vec<serde_json::Value>) {
    let values = arr
        .as_array()
        .cloned()
        .expect("test shape must be a JSON array of records");
    let items: Vec<ConversationItem> = serde_json::from_value(serde_json::Value::Array(
        values.clone(),
    ))
    .expect("records must deserialize to ConversationItem");
    (items, values)
}

fn items_from_fixture(json: &str) -> (Vec<ConversationItem>, Vec<serde_json::Value>) {
    let value: serde_json::Value = serde_json::from_str(json).expect("fixture must be valid JSON");
    items_from(value)
}

/// The reasoning ids of a projected output, in transcript order.
fn reasoning_ids(out: &[ConversationItem]) -> Vec<&str> {
    out.iter()
        .filter_map(|item| match item {
            ConversationItem::Reasoning(r) => Some(r.id.as_str()),
            _ => None,
        })
        .collect()
}

/// Forward attribution (sdd-71 §2.3): the owning model of a reasoning item is
/// the `model_id` of the NEXT assistant item after it; `None` when
/// unresolvable (fail-closed foreign).
fn forward_owner_model(items: &[ConversationItem], idx: usize) -> Option<String> {
    items.iter().skip(idx + 1).find_map(|item| match item {
        ConversationItem::Assistant(a) => a.model_id.clone(),
        _ => None,
    })
}

/// True when a live call matching `id` is present in `out` (a BackendToolCall
/// with that id, or an assistant client-side tool_call with that id).
fn call_id_present(out: &[ConversationItem], id: &str) -> bool {
    out.iter().any(|item| match item {
        ConversationItem::BackendToolCall(b) => b.id() == id,
        ConversationItem::Assistant(a) => a.tool_calls.iter().any(|tc| tc.id.as_ref() == id),
        _ => false,
    })
}

/// One Table-2 case-3 row: [user, reasoning(id/encrypted, 1 summary block),
/// assistant(model_id = owner)] — the minimal forward-attribution shape.
fn boundary_row(
    owner_model: &str,
    reasoning_id: &str,
    encrypted: &str,
) -> (Vec<ConversationItem>, Vec<serde_json::Value>) {
    items_from(serde_json::json!([
        {"type": "user", "content": [{"type": "text", "text": "go"}]},
        {"type": "reasoning", "id": reasoning_id, "encrypted_content": encrypted,
         "summary": [{"type": "summary_text", "text": "s"}]},
        {"type": "assistant", "content": "done", "model_id": owner_model}
    ]))
}

/// Case-2 shape: a `CodexRawInput` carrier item (opaque raw payload, incl.
/// its own ciphertext — D-ENC-spared, survives byte-identical) beside foreign
/// reasoning.
fn carrier_shape() -> serde_json::Value {
    serde_json::json!([
        {"type": "user", "content": [{"type": "text", "text": "compact it"}]},
        {"type": "backend_tool_call",
         "kind": {"tool_type": "codex_raw_input",
                  "id": "crx_7",
                  "raw": {"type": "context_compaction",
                          "encrypted_content": "gAAA_carrier_payload", "summary": []},
                  "cross_provider_fallback": "retained tail text"}},
        {"type": "reasoning", "id": "rs_foreign_carrier", "encrypted_content": "enc_fk",
         "summary": [{"type": "summary_text", "text": "s"}]},
        {"type": "assistant", "content": "done", "model_id": "gpt-5.6-sol"}
    ])
}

/// Case-8 shape: an assistant client-side tool call + its ToolResult (a pair
/// the VL target can consume) with foreign reasoning interleaved after.
fn surviving_pair_shape() -> serde_json::Value {
    serde_json::json!([
        {"type": "user", "content": [{"type": "text", "text": "read the file"}]},
        {"type": "assistant", "content": "", "model_id": "gpt-5.6-sol",
         "tool_calls": [{"id": "tc_keep", "name": "read_file", "arguments": "{}"}]},
        {"type": "tool_result", "tool_call_id": "tc_keep", "content": "file body",
         "is_error": false},
        {"type": "reasoning", "id": "rs_foreign_keep", "encrypted_content": "enc_fk",
         "summary": [{"type": "summary_text", "text": "s"}]},
        {"type": "assistant", "content": "done", "model_id": "gpt-5.6-sol"}
    ])
}

/// Every shape the unit suite projects: the two mirrored corpus cells (Table-1
/// shapes) + every Table-2 synthetic shape. Cases 6 and 7 iterate this set.
fn all_table2_shapes() -> Vec<(Vec<ConversationItem>, String, Boundary)> {
    let mut shapes = Vec::new();
    let (items, _) = items_from_fixture(VXM_AZ_PRE);
    shapes.push((items, "gpt-5.6-terra".to_string(), Boundary::AzStrict));
    let (items, _) = items_from_fixture(AZ_VLQ_PRE);
    shapes.push((items, "qwen3.8-27b".to_string(), Boundary::VLLenient));
    let (items, _) = items_from_fixture(ORPHAN_SHAPE);
    shapes.push((
        items,
        "claude-sonnet-5".to_string(),
        Boundary::Vertex,
    ));
    let (items, _) = items_from(carrier_shape());
    shapes.push((items, "qwen3.8-27b".to_string(), Boundary::VLLenient));
    let (items, _) = boundary_row("gpt-5.6-sol", "encitem_self", "enc_self");
    shapes.push((items, "gpt-5.6-sol".to_string(), Boundary::AzStrict));
    let (items, _) = boundary_row("gpt-5.6-sol", "encitem_foreign", "enc_foreign");
    shapes.push((items, "qwen3.8-27b".to_string(), Boundary::VLLenient));
    let (items, _) = boundary_row("gpt-5.6-sol", "encitem_foreign", "enc_foreign");
    shapes.push((
        items,
        "gpt-5.6-terra".to_string(),
        Boundary::AzStrict,
    ));
    let (items, _) = boundary_row("qwen3.8-27b", "rs_qwen_1", "enc_qwen");
    shapes.push((items, "glm-5.2".to_string(), Boundary::VLLenient));
    let (items, _) = boundary_row("claude-sonnet-5", "rs_sonnet_1", "enc_sonnet");
    shapes.push((
        items,
        "claude-sonnet-5".to_string(),
        Boundary::Vertex,
    ));
    let (items, _) = boundary_row("claude-sonnet-5", "", "enc_sonnet_empty");
    shapes.push((items, "qwen3.8-27b".to_string(), Boundary::VLLenient));
    let (items, _) = items_from(surviving_pair_shape());
    shapes.push((items, "qwen3.8-27b".to_string(), Boundary::VLLenient));
    shapes
}

// ============================================================================
// apex-mf6 (XW-ENC-AFFINITY-1) U5 — the switch-time affinity gate
//
// Pre-cut: these compile-fail (the 4-arg `project_switch_history` is absent
// — E0061 — and the `ReasoningItemStore` wrapper's `mint_tag` / `item`
// fields are absent — E0609). Post-cut they are the U5 goldens: the AZ->AZ
// row's ciphertext fate is decided by `enc_affinity_gate` (pin x mint_tag),
// T0 and every non-AZ->AZ T1 row are gate-inert (T0 verbatim, T1 strip),
// and `mint_tag` always survives the projection.
//
// The behavior change vs .71: an AZ->AZ switch with NEITHER a target pin
// NOR a mint tag (the unpinned-unpinned row) now RETAINS the ciphertext
// (RetainOptimistic) instead of stripping it — the store retains, the wire
// layer decides at send time, and the reactive strip-fallback covers a
// wrong bet (the N-1 layering; the .71 `no_foreign_encrypted` invariant
// was re-armed to the mf6-aware form alongside the 4-arg call-site update).
// ============================================================================

/// The AZ->AZ minimal row with an explicit mint tag: [user,
/// reasoning(id/encrypted/mint), assistant(model_id = owner)].
fn az_az_pinned_row(
    owner_model: &str,
    reasoning_id: &str,
    encrypted: &str,
    mint_tag: Option<&str>,
) -> Vec<ConversationItem> {
    let (mut items, _) = boundary_row(owner_model, reasoning_id, encrypted);
    for item in &mut items {
        if let ConversationItem::Reasoning(r) = item {
            r.mint_tag = mint_tag.map(str::to_owned);
        }
    }
    items
}

/// The first reasoning item of a projected output (all U5 shapes have
/// exactly one).
fn first_reasoning(out: &[ConversationItem]) -> &super::ReasoningItemStore {
    out.iter()
        .find_map(|item| match item {
            ConversationItem::Reasoning(r) => Some(r),
            _ => None,
        })
        .expect("U5 shapes project exactly one reasoning item")
}

/// U5 arm 1 — pin x mint MATCH (exact string): the AZ->AZ row RETAINS its
/// ciphertext in the storage form (the .71 behavior stripped it here).
#[test]
fn mf6_u5_az_az_pin_match_retains_ciphertext() {
    let items = az_az_pinned_row(
        "gpt-5.6-sol",
        "encitem_mf6_match",
        "litellm_enc:ZXlJbGVI;u5-match",
        Some("East US 2"),
    );
    let history = project_switch_history(
        &items,
        "gpt-5.6-terra",
        Boundary::AzStrict,
        Some("East US 2"),
    );
    let out = proj_items(&history);
    let r = first_reasoning(out);
    assert_eq!(
        r.item.encrypted_content.as_deref(),
        Some("litellm_enc:ZXlJbGVI;u5-match"),
        "pin == mint (exact string): the AZ->AZ row must RETAIN the ciphertext"
    );
    assert_eq!(r.id, "encitem_mf6_match", "AZ->AZ keeps the original id (no re-key)");
    assert_eq!(
        r.mint_tag.as_deref(),
        Some("East US 2"),
        "mint_tag always survives the projection"
    );
    // the storage form persists the mint tag (the as_value discipline)
    assert_eq!(
        as_value(&out[1])["mint_tag"],
        serde_json::json!("East US 2"),
        "stamped reasoning must serialize the mint_tag field"
    );
}

/// U5 arm 2 — pin x mint MISMATCH (exact string, N-3 no case-folding):
/// the ciphertext is stripped, the mint tag preserved.
#[test]
fn mf6_u5_az_az_pin_mismatch_strips_ciphertext() {
    let items = az_az_pinned_row(
        "gpt-5.6-sol",
        "encitem_mf6_mismatch",
        "litellm_enc:ZXlJbGVI;u5-mismatch",
        Some("Sweden Central"),
    );
    let history = project_switch_history(
        &items,
        "gpt-5.6-terra",
        Boundary::AzStrict,
        Some("East US 2"),
    );
    let out = proj_items(&history);
    let r = first_reasoning(out);
    assert!(
        r.item.encrypted_content.is_none(),
        "pin != mint (exact-string mismatch): the AZ->AZ row must STRIP the ciphertext"
    );
    assert_eq!(
        r.mint_tag.as_deref(),
        Some("Sweden Central"),
        "mint_tag is preserved even on the strip arm (provenance stays)"
    );
    assert_eq!(r.id, "encitem_mf6_mismatch", "AZ->AZ keeps the original id on strip too");
}

/// U5 arm 3 — pin present, mint ABSENT: an untagged mint under a known pin
/// cannot be proven boundary-compatible -> STRIP.
#[test]
fn mf6_u5_az_az_pin_mint_absent_strips() {
    let items =
        az_az_pinned_row("gpt-5.6-sol", "encitem_mf6_nomint", "litellm_enc:ZXlJbGVI;u5-nomint", None);
    let history = project_switch_history(
        &items,
        "gpt-5.6-terra",
        Boundary::AzStrict,
        Some("East US 2"),
    );
    let out = proj_items(&history);
    let r = first_reasoning(out);
    assert!(
        r.item.encrypted_content.is_none(),
        "pin present x mint absent: the AZ->AZ row must STRIP (unknown mint under a known pin)"
    );
}

/// U5 arm 4 — NEITHER pin NOR mint (the unpinned-unpinned row, the
/// az-az-unpin cell): RETAIN-OPTIMISTIC — the behavior change vs .71,
/// which stripped here unconditionally. The wire layer decides at send
/// time; the reactive strip-fallback covers a wrong bet.
#[test]
fn mf6_u5_az_az_unpinned_untagged_retains_optimistic() {
    let items =
        az_az_pinned_row("gpt-5.6-sol", "encitem_mf6_unpin", "litellm_enc:ZXlJbGVI;u5-unpin", None);
    let history = project_switch_history(&items, "gpt-5.6-terra", Boundary::AzStrict, None);
    let out = proj_items(&history);
    let r = first_reasoning(out);
    assert_eq!(
        r.item.encrypted_content.as_deref(),
        Some("litellm_enc:ZXlJbGVI;u5-unpin"),
        "no pin x no mint: RetainOptimistic — the .71 strip is replaced by the gate"
    );
    assert!(r.mint_tag.is_none(), "untagged stays untagged");
}

/// U5 arm 5 — mint present, pin ABSENT (the deliberate unpin of a row that
/// minted under a tag): the ciphertext belongs to a tagged deployment the
/// target no longer addresses -> STRIP.
#[test]
fn mf6_u5_az_az_unpinned_minted_strips() {
    let items = az_az_pinned_row(
        "gpt-5.6-sol",
        "encitem_mf6_unpinned_mint",
        "litellm_enc:ZXlJbGVI;u5-unpinned-mint",
        Some("East US 2"),
    );
    let history = project_switch_history(&items, "gpt-5.6-terra", Boundary::AzStrict, None);
    let out = proj_items(&history);
    let r = first_reasoning(out);
    assert!(
        r.item.encrypted_content.is_none(),
        "mint present x pin absent: the AZ->AZ row must STRIP (deliberate unpin)"
    );
    assert_eq!(
        r.mint_tag.as_deref(),
        Some("East US 2"),
        "the stale mint stays recorded (audit trail)"
    );
}

/// U5 gate-inert row A — T0 (owner == target): verbatim clone, no gate
/// consultation at all (same model = same boundary by construction), even
/// with a pin present and a mint that would STRIP under the gate.
#[test]
fn mf6_u5_t0_same_model_is_gate_inert() {
    let items = az_az_pinned_row(
        "gpt-5.6-terra",
        "encitem_mf6_t0",
        "litellm_enc:ZXlJbGVI;u5-t0",
        Some("Sweden Central"),
    );
    let history = project_switch_history(
        &items,
        "gpt-5.6-terra",
        Boundary::AzStrict,
        Some("East US 2"),
    );
    let out = proj_items(&history);
    let r = first_reasoning(out);
    assert_eq!(
        r.item.encrypted_content.as_deref(),
        Some("litellm_enc:ZXlJbGVI;u5-t0"),
        "T0 is a verbatim clone — the gate is never consulted for same-model items"
    );
    assert_eq!(r.id, "encitem_mf6_t0", "T0 keeps the id verbatim");
    assert_eq!(
        r.mint_tag.as_deref(),
        Some("Sweden Central"),
        "T0 carries the mint tag along verbatim"
    );
}

/// U5 gate-inert row B — a non-AZ->AZ T1 row (foreign origin, re-keyed):
/// the ciphertext is stripped unconditionally, pin or no pin, mint or no
/// mint (the affinity gate only re-rules the ONE no-re-key AZ->AZ row).
#[test]
fn mf6_u5_foreign_t1_row_strips_regardless_of_pin() {
    let items = az_az_pinned_row(
        "gpt-5.6-sol",
        "encitem_mf6_t1",
        "litellm_enc:ZXlJbGVI;u5-t1",
        Some("East US 2"),
    );
    let history = project_switch_history(
        &items,
        "qwen3.8-27b",
        Boundary::VLLenient,
        Some("East US 2"),
    );
    let out = proj_items(&history);
    let r = first_reasoning(out);
    assert!(
        r.item.encrypted_content.is_none(),
        "foreign T1 row: the ciphertext is stripped regardless of pin/mint"
    );
    assert!(r.id.starts_with("xw_"), "foreign T1 row keeps the re-key");
    assert_eq!(
        r.mint_tag.as_deref(),
        Some("East US 2"),
        "the mint tag rides the re-keyed item too (provenance survives T1)"
    );
}
