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

/// Table 2 case 2 — CARRIER SURVIVAL (sdd-71 §4 invariant 2), pinned on BOTH
/// boundaries this shape can be projected to.
///
/// History with a `CodexRawInput` carrier + foreign reasoning → project → the
/// carrier survives untouched and opaque (value equality through `as_value`; the
/// H-2 silent-loss class the projector must not reproduce at switch time), the
/// reasoning is projected per floor, and the drop ledger stays empty.
///
/// The two legs do NOT prove the same half of the invariant:
/// - `VLLenient` → the carrier is inert. No Vertex arm is reachable, so this
///   leg only pins "a lenient target never mutates the carrier". It cannot see
///   a carrier dropped: the T3 guard
///   (`ConversationItem::BackendToolCall(b) if boundary == Boundary::Vertex &&
///   !is_carrier(b)`, `projection.rs:214-215`) short-circuits on `&&` and never
///   calls `is_carrier` off Vertex.
/// - `Vertex` → the ONLY boundary where `is_carrier` is consulted, so this is
///   the crate's sole falsifier of its TRUE branch. It is also the only leg
///   that can tell a surviving carrier from a dropped one, which is why it
///   asserts the ledger as well as the record's value: dropping the carrier files
///   `vertex_non_carrier_backend_call`, a reason asserting the item was NOT a
///   carrier, about an item that was. Before this leg existed,
///   `is_carrier(_b) -> false` (harness mutant `S-CARRIER-ALL`) passed all 792
///   library tests in both profiles.
///
/// Both legs share the body because this shape's counts are the same on both
/// boundaries — 4 records in, 4 out, 0 drops, reasoning re-keyed T1 either way
/// (owner `gpt-5.6-sol` is foreign to both targets). A shape whose counts
/// differed per boundary would need per-boundary asserts, not this loop.
///
/// Right-reason failure after the fn lands: carriers mutated/dropped — the
/// `drops.is_empty()` line fires first on a drop (naming the ledger that
/// claimed a non-carrier), the value-identity line on a mutation.
#[test]
fn xw_proj_carrier_survival() {
    let (items, values) = items_from(carrier_shape());
    for (target_model_id, boundary) in [
        ("qwen3.8-27b", Boundary::VLLenient),
        ("claude-sonnet-5", Boundary::Vertex),
    ] {
        let history = project_switch_history(&items, target_model_id, boundary, None);
        let out = proj_items(&history);
        assert!(
            history.drops.is_empty(),
            "invariant 2 violated at {boundary:?}: a CodexRawInput carrier must never \
             reach a drop arm, but the ledger records {:?}",
            history.drops
        );
        assert_eq!(
            out.len(),
            4,
            "carrier shape at {boundary:?}: projected record count changed"
        );
        assert_eq!(
            as_value(&out[1]),
            values[1],
            "invariant 2 violated at {boundary:?}: CodexRawInput carrier must survive \
             untouched and opaque (value equality through `as_value`, the crate's \
             parsed-equal standard — not a byte comparison)"
        );
        let ConversationItem::Reasoning(r) = &out[2] else {
            panic!("carrier shape at {boundary:?}: reasoning slot lost in projection");
        };
        assert!(
            r.id.as_deref().is_some_and(|s| s.starts_with("xw_")),
            "foreign reasoning beside the carrier must be projected per floor (T1 re-key), got {:?}",
            r.id
        );
        assert!(
            r.encrypted_content.is_none(),
            "foreign encrypted_content must be stripped"
        );
        assert_eq!(r.summary.len(), 1, "summary must be kept");
        // The partition the projection result claims by construction. Stated
        // honestly: with the empty ledger and `out.len() == 4` already asserted
        // above, this reduces to `items.len() == 4` — it guards the shape of
        // `carrier_shape()` rather than the projector, and it is not this test's
        // falsifier for a drop-and-don't-record defect (the ledger assert is).
        assert_eq!(
            out.len() + history.drops.len(),
            items.len(),
            "carrier shape at {boundary:?}: projected items and recorded drops must \
             partition the input"
        );
    }
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
            "sol→sol: T0-KEEP must be value-identical (id + encrypted kept, same boundary)"
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
            r.id.as_deref().is_some_and(|s| s.starts_with("xw_")),
            "sol→qwen: T1 must re-key to an xw_ id, got {:?}",
            r.id
        );
        assert_ne!(r.id.as_deref(), Some("encitem_foreign"));
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
            r.id.as_deref(), Some("encitem_foreign"),
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
            r.id.as_deref().is_some_and(|s| s.starts_with("xw_")),
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
            r.id.as_deref().is_some_and(|s| !s.is_empty()),
            "sonnet→qwen: empty-id mint must never survive projection (invariant 4)"
        );
        assert!(
            r.id.as_deref().is_some_and(|s| s.starts_with("xw_")),
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

/// Table 2 case 5 — VALUE identity of non-projected records (the case the spec
/// labels sdd-71 §4 invariant 6 “BYTE-IDENTITY of non-projected items”, and the
/// over-strip guard). Every record whose expected mirror is unchanged (T0) must
/// come back equal to its storage form **as parsed JSON values**.
///
/// What is asserted, precisely: `as_value` serialises the projected record and
/// the fixture record to `serde_json::Value` and compares those, so key order,
/// number spelling and escape spelling are all invisible — this is the corpus's
/// own stated verification standard (“parsed-equal”), not a byte comparison. The
/// name used to read `byte_identity`, which claimed more than the comparison
/// enforces: a projector that re-serialised an untouched record with reordered
/// keys or a re-escaped string would pass here, and nothing in this crate claims
/// otherwise. Byte-level identity over these fixtures is owed to whoever first
/// compares the stored text against what the projector sends, byte for byte.
///
/// Right-reason failure after the fn lands: incidental mutation of a record this
/// switch had no reason to touch (content, id, or a strip that was not owed).
#[test]
fn xw_proj_value_identity_nonprojected() {
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
                     value-identical to the storage form (`as_value` = parsed-equal)"
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
                r.id.as_deref().is_some_and(|s| !s.is_empty()),
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
        r.id.as_deref().is_some_and(|s| s.starts_with("xw_")),
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
            ConversationItem::Reasoning(r) => r.id.as_deref(),
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
/// its own ciphertext — D-ENC-spared, survives value-identically) beside foreign
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
    assert_eq!(r.id.as_deref(), Some("encitem_mf6_match"), "AZ->AZ keeps the original id (no re-key)");
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
    assert_eq!(r.id.as_deref(), Some("encitem_mf6_mismatch"), "AZ->AZ keeps the original id on strip too");
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
    assert_eq!(r.id.as_deref(), Some("encitem_mf6_t0"), "T0 keeps the id verbatim");
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
    assert!(r.id.as_deref().is_some_and(|s| s.starts_with("xw_")), "foreign T1 row keeps the re-key");
    assert_eq!(
        r.mint_tag.as_deref(),
        Some("East US 2"),
        "the mint tag rides the re-keyed item too (provenance survives T1)"
    );
}

/// Rule 13's testable half at this seam: the projector may **remove** a record —
/// and then it must name it in `drops` — and it may rewrite a `Reasoning` item,
/// which is the one class the T1 ladder exists to rewrite. It may not silently
/// **change** a record it keeps: `ProjectionDrop` is `{ index, reason }`, a
/// removal, so a modified survivor has no accounting surface anywhere (§4.2 rule
/// 13, `xwire-boundary-map.md:311` — “The transition MUST be recorded, never
/// silent”). This test is that reconstruction claim: drop the indices the ledger
/// names, and every remaining projected record must still equal the stored record
/// it came from.
///
/// Both shapes carry an assistant `tool_call` with no result — the input the
/// `/messages` build's D5 pass would rewrite (`messages.rs:109-118` filters
/// `tool_calls` to the paired set, then deletes the record if that empties it and
/// its content is empty). Mirroring that half is deliberately out of scope here
/// (comment at `projection.rs:180`; owner: the follow-on bead filed from the px29
/// F-2 handoff), so this test is NOT a pin that the gap stays: if that bead lands
/// a rewrite, it extends this assertion **together with** the ledger, which is
/// exactly the forcing function the assertion is for. Harness mutant `MUT-H`
/// filters `tool_calls` without naming the change, so it fails the assertion below
/// on the first shape; the second shape is its deletion half, which the ledger CAN
/// name — a removal this projector is allowed to make and record — so that shape is
/// reconstruction-clean by design, not by accident.
#[test]
fn vertex_ledger_explains_every_change_to_a_record_it_keeps() {
    let shapes = [
        // Content plus one paired and one orphaned call: `clean_orphaned_items`
        // would keep this record and shrink its `tool_calls`.
        serde_json::json!([
            {"type": "user", "content": [{"type": "text", "text": "go"}]},
            {"type": "assistant", "content": "reading", "model_id": "gpt-5.6-sol",
             "tool_calls": [{"id": "tc_paired", "name": "read_file", "arguments": "{}"},
                            {"id": "tc_orphan", "name": "write_file", "arguments": "{}"}]},
            {"type": "tool_result", "tool_call_id": "tc_paired", "content": "body",
             "is_error": false}
        ]),
        // No content and only an orphaned call: `clean_orphaned_items` would
        // delete this record outright.
        serde_json::json!([
            {"type": "user", "content": [{"type": "text", "text": "go"}]},
            {"type": "assistant", "content": "", "model_id": "gpt-5.6-sol",
             "tool_calls": [{"id": "tc_orphan", "name": "write_file", "arguments": "{}"}]},
            {"type": "user", "content": [{"type": "text", "text": "next"}]}
        ]),
    ];
    for shape in shapes {
        let (items, _) = items_from(shape);
        let history = project_switch_history(&items, "claude-sonnet-5", Boundary::Vertex, None);
        let dropped: Vec<usize> = history.drops.iter().map(|drop| drop.index).collect();
        let kept: Vec<serde_json::Value> = items
            .iter()
            .enumerate()
            .filter(|(index, _)| !dropped.contains(index))
            .map(|(_, item)| as_value(item))
            .collect();
        let projected: Vec<serde_json::Value> = proj_items(&history).iter().map(as_value).collect();
        assert_eq!(
            projected, kept,
            "unaccounted change: the ledger names {:?} as the only removals, so every \
             other record must come back exactly as stored",
            history.drops
        );
    }
}

// ============================================================================
// apex-waj.2 §3.3 — D1 ReKey: the `tso_` mint (SPEC-W2 PA-10..PA-14)
// ============================================================================

/// Known-answer constant for the D1 re-mint, computed OUTSIDE this crate by the
/// oracle below (homebrew `python3` 3.14.7, run 2026-09-29). The constant is that
/// snippet's output; it was never read back out of a Rust run — a known-answer
/// test that prints its own answer asserts nothing.
///
///     cell, ord, call_id = "gpt-5.6-sol", 0, "call_waj2_d1"
///     tools = [{"type": "namespace", "name": "mcp__ratchet_fixture",
///               "tools": [{"type": "function", "name": "crm_fixture_tool_00"}]}]
///     preimage = f"{cell}|{ord}|{call_id}|{json.dumps(tools, sort_keys=True)}"
///     "tso_" + hashlib.sha256(preimage.encode()).hexdigest()[:24]
///       -> tso_55773eb0b1c06bc93e583369
///
/// `json.dumps(..., sort_keys=True)` is the `py_json_canonicalize` form (sorted
/// keys, `", "` / `": "` separators), NOT serde_json's compact form (PA-11; the
/// canonicalization-parity note at `projection.rs:453-457`).
const EXPECTED_TSO_D1: &str = "tso_55773eb0b1c06bc93e583369";

/// One CX3-shaped client-executed discovery pair — completed call plus completed
/// namespaced output sharing one join key — whose output still carries the id the
/// ORIGIN row minted. That stale `tso_` id is what a switch to another target row
/// hands the D1 arm: the row that issued it cannot resolve it.
const D1_STALE_PAIR: &str = r#"[
    {"type":"user","content":[{"type":"text","text":"find the crm tools"}]},
    {"type":"discovery","item":{"type":"tool_search_call","id":"tsc_waj2_d1","call_id":"call_waj2_d1","status":"completed","execution":"client","arguments":{"query":"crm order management","limit":8}}},
    {"type":"discovery","item":{"type":"tool_search_output","id":"tso_waj2_d1_stale","call_id":"call_waj2_d1","status":"completed","execution":"client","tools":[{"type":"namespace","name":"mcp__ratchet_fixture","tools":[{"type":"function","name":"crm_fixture_tool_00"}]}]}},
    {"type":"assistant","content":"found them","model_id":"gpt-5.6-sol"}
]"#;

/// The discovery half of a history, in the slice form `pairing_of` consumes.
fn d1_discovery_slice(items: &[ConversationItem]) -> Vec<super::tool_search::ToolSearchItem> {
    items.iter().filter_map(|item| item.discovery().cloned()).collect()
}

fn d1_half(
    items: &[super::tool_search::ToolSearchItem],
    kind: super::tool_search::ToolSearchKind,
) -> &super::tool_search::ToolSearchItem {
    items
        .iter()
        .find(|item| item.kind() == kind)
        .expect("fixture carries one call and one output")
}

/// PA-10..PA-14 (D1 ReKey): the output's own `id` becomes the known-answer `tso_`
/// digest of `(cell, ord, call_id, tools)`, and NOTHING else moves — the
/// provider-minted `tsc_` id and the `call_id` join key are copied, never
/// rewritten, and the pair is still a pair.
///
/// RED at this tip: `project_discovery_rekey` does not exist yet (PA-11's mint is
/// the absent piece; the seam still runs the interim unconditional keep of
/// `projection.rs:325`), so this fails to compile. Against an arm that keeps the
/// item verbatim it fails on clause 1 instead — the stale id is not the answer —
/// which is why the fixture carries a deliberately stale one.
#[test]
fn d1_rekey_mints_a_known_answer_tso_id_and_keeps_the_join_key() {
    let (items, _) = items_from_fixture(D1_STALE_PAIR);
    let source = d1_discovery_slice(&items);
    let source_call = d1_half(&source, super::tool_search::ToolSearchKind::Call);
    let source_output = d1_half(&source, super::tool_search::ToolSearchKind::Output);
    // Fixture sanity: if the stored id already were the answer, clause 1 below
    // would be vacuous — it would pass on a keep arm that rewrote nothing.
    assert_ne!(
        source_output.id(),
        Some(EXPECTED_TSO_D1),
        "fixture must hand the arm a stale output id"
    );

    let projected_items = super::projection::project_discovery_rekey(&items, "gpt-5.6-sol");
    let projected = d1_discovery_slice(&projected_items);
    let projected_call = d1_half(&projected, super::tool_search::ToolSearchKind::Call);
    let projected_output = d1_half(&projected, super::tool_search::ToolSearchKind::Output);

    // Clause 1 (PA-11): the mint is the crate's one id grammar over the four-term
    // preimage, not an arbitrary fresh id and not the stale stored one.
    assert_eq!(
        projected_output.id(),
        Some(EXPECTED_TSO_D1),
        "D1 must re-mint the output id as tso_ + sha256(cell|ord|call_id|py_json(tools))[:24]"
    );
    // Clause 2 (PA-10 / PA-13): both provider-minted handles are copied verbatim.
    assert_eq!(
        projected_call.id(),
        source_call.id(),
        "D1 must not re-key the tool_search_call's provider-minted tsc_ id"
    );
    assert_eq!(
        projected_output.call_id(),
        source_output.call_id(),
        "D1 must not rewrite the call_id join key"
    );
    // Clause 3 (PA-14 / XD-2): re-keying leaves a pair a pair.
    assert_eq!(
        super::tool_search::pairing_of(&projected),
        super::tool_search::pairing_of(&source),
        "D1 must preserve pair atomicity over the discovery slice"
    );
}

// ---------------------------------------------------------------------------
// apex-waj.2 — the route-independent residue of §3: the D1 rekey mint
// (PA-10..PA-14) and the D3 pair strip (PA-28/PA-29). Neither is consulted by
// `project_switch_history` yet — the tier needs the `TargetRoute` tuple, which
// is apex-waj.35's — so these tests pin the two callees the arm will call.
// ---------------------------------------------------------------------------

/// Further known answers for the same grammar. The four `tso_…` values below this
/// rule were recomputed outside Rust over the PA-11 preimage
/// `"{cell}|{ord}|{call_id}|{json.dumps(tools,sort_keys=True)}"` and none of them
/// was read back from a Rust run:
///
/// - `("gpt-5.6-terra", 0, "call_waj2_d1", namespaced tools)` → `tso_e8d6cb294ae1fc7f1e1021c6`
/// - `("gpt-5.6-sol", 1, "call_waj2_d1", namespaced tools)`  → `tso_bbb0a8fa07d62a107b690edb`
///   (any shift of `ord` off 0 lands elsewhere — that is the whole point)
/// - `("gpt-5.6-sol", 0, "call_waj2_tools", [])`             → `tso_f3a23488f9257db800116032`
/// - canonicalising an absent `tools` as `null` (MUT-D1f)     → `tso_654432754e73137a6cb59eff`
///
/// The three §4 mutants then die exactly where the oracle says they must, each
/// measured once on the reverted tree: MUT-D1f produces the `null` value above;
/// MUT-D1c (item index 3 instead of output index 0) produces
/// `tso_2896f8044bde80f003d26f07`; MUT-D1d (no `{cell}`) collapses both rows onto
/// `tso_090ba8bc86a591bfbeadba66`; and MUT-D1b (serde_json instead of
/// `py_json_canonicalize`) produces `tso_dba0163ebf44e0b7675c1ca9` — serde differs
/// twice over, compact separators AND document key order, since it does not sort.
const D1_TERRA_TSO: &str = "tso_e8d6cb294ae1fc7f1e1021c6";
const D1_EMPTY_TOOLS_TSO: &str = "tso_f3a23488f9257db800116032";

/// The D1 pair with a `reasoning` row interleaved BEFORE it. `{ord}` is the
/// index among discovery OUTPUTS, not the item index (PA-11 spells that out:
/// "an interleaved reasoning item must not shift it"), so the answer is still
/// `EXPECTED_TSO_D1` even though the output sits at item index 3.
const D1_PAIR_AFTER_REASONING: &str = r#"[
    {"type":"user","content":[{"type":"text","text":"find the crm tools"}]},
    {"type":"reasoning","id":"encitem_waj2_ord","encrypted_content":"az_ciphertext","summary":[{"type":"summary_text","text":"s"}]},
    {"type":"discovery","item":{"type":"tool_search_call","id":"tsc_waj2_d1","call_id":"call_waj2_d1","status":"completed","execution":"client","arguments":{"query":"crm order management","limit":8}}},
    {"type":"discovery","item":{"type":"tool_search_output","id":"tso_waj2_d1_stale","call_id":"call_waj2_d1","status":"completed","execution":"client","tools":[{"type":"namespace","name":"mcp__ratchet_fixture","tools":[{"type":"function","name":"crm_fixture_tool_00"}]}]}},
    {"type":"assistant","content":"found them","model_id":"gpt-5.6-sol"}
]"#;

/// PA-12's R6 shape: an output this harness authored, carrying NO `id` key. Its
/// key set is exactly `{call_id, execution, status, tools, type}` — the set the
/// projected item must still have, because minting onto an item that had none
/// manufactures a handle the origin never issued.
const D1_R6_OUTPUT_WITHOUT_ID: &str = r#"[
    {"type":"discovery","item":{"type":"tool_search_call","id":"tsc_waj2_r6","call_id":"call_waj2_r6","status":"completed","execution":"client","arguments":{"query":"crm order management"}}},
    {"type":"discovery","item":{"type":"tool_search_output","call_id":"call_waj2_r6","execution":"client","status":"completed","tools":[{"type":"namespace","name":"mcp__ratchet_fixture","tools":[{"type":"function","name":"crm_fixture_tool_00"}]}]}}
]"#;

/// The two `tools` shapes PA-11/T-12 says must agree: an ABSENT `tools` key
/// canonicalises as `[]`, exactly like `xw_reasoning_id` treats an absent
/// `content`. Same cell, same join key, same ordinal ⇒ same id.
const D1_OUTPUT_WITHOUT_TOOLS: &str = r#"[
    {"type":"discovery","item":{"type":"tool_search_call","id":"tsc_waj2_tools","call_id":"call_waj2_tools","status":"completed","execution":"client","arguments":{"query":"crm order management"}}},
    {"type":"discovery","item":{"type":"tool_search_output","id":"tso_waj2_tools_stale","call_id":"call_waj2_tools","status":"completed","execution":"client"}}
]"#;

const D1_OUTPUT_WITH_EMPTY_TOOLS: &str = r#"[
    {"type":"discovery","item":{"type":"tool_search_call","id":"tsc_waj2_tools","call_id":"call_waj2_tools","status":"completed","execution":"client","arguments":{"query":"crm order management"}}},
    {"type":"discovery","item":{"type":"tool_search_output","id":"tso_waj2_tools_stale","call_id":"call_waj2_tools","status":"completed","execution":"client","tools":[]}}
]"#;

/// The PA-12 shape `id()` reads as "has none" but that no fixture here held: the
/// `id` key is PRESENT and empty. `ToolSearchItem::id` filters `""`
/// (`tool_search.rs:556-561`) because the A2 lint requires a non-empty id where the
/// field is present, and an empty string "identifies nothing" (`tool_search.rs:
/// 545-555`, where in-repo fixture bytes carrying `id: ""` are named). The re-key
/// gates on that accessor, so this output is a no-mint and must ride byte-for-byte.
const D1_OUTPUT_WITH_EMPTY_ID: &str = r#"[
    {"type":"discovery","item":{"type":"tool_search_call","id":"tsc_waj2_emptyid","call_id":"call_waj2_emptyid","status":"completed","execution":"client","arguments":{"query":"crm order management"}}},
    {"type":"discovery","item":{"type":"tool_search_output","id":"","call_id":"call_waj2_emptyid","status":"completed","execution":"client","tools":[{"type":"namespace","name":"mcp__ratchet_fixture","tools":[{"type":"function","name":"crm_fixture_tool_00"}]}]}}
]"#;

/// The rekey twin of `D0_UNKNOWN_KEY_PAIR`: bytes this harness does not model
/// (`created_by`, `z_provider_debug`) written in NON-alphabetical document positions
/// on the D1 path. `D0_UNKNOWN_KEY_PAIR` only ever reaches the keep arm, which clones
/// the item wholesale and therefore cannot lose a key or re-order one; this fixture
/// reaches `project_discovery_rekey`, which DOES rewrite a key, so it is the only
/// place PA-7's byte-verbatim doctrine is falsifiable on the rekey arm. The `tools`
/// value is the ordinary namespaced one, so the answer stays `EXPECTED_TSO_D1`:
/// neither unknown key is a mint term.
const D1_UNKNOWN_KEY_REKEY_PAIR: &str = r#"[
    {"type":"discovery","item":{"created_by":"provider","type":"tool_search_call","id":"tsc_waj2_d1","call_id":"call_waj2_d1","status":"completed","execution":"client","arguments":{"query":"crm order management","limit":8}}},
    {"type":"discovery","item":{"z_provider_debug":{"shard":"eu-1"},"type":"tool_search_output","created_by":"provider","id":"tso_waj2_d1_stale","call_id":"call_waj2_d1","status":"completed","execution":"client","tools":[{"type":"namespace","name":"mcp__ratchet_fixture","tools":[{"type":"function","name":"crm_fixture_tool_00"}]}]}}
]"#;

/// Two complete discovery pairs, with a `reasoning` row between them, so the second
/// output is OUTPUT #1 at ITEM index 5. This is the fixture that pins the DOMAIN of
/// `{ord}`: every other rekey fixture in this file holds exactly one output, so
/// `ord = 0` was the only ordinal ever observed and a callee that numbered every
/// output 0 — or that numbered items rather than outputs — was indistinguishable from
/// the correct one. Both answers are oracle values over the PA-11 preimage (see the
/// re-derivation note above `D1_TERRA_TSO`).
const D1_TWO_PAIR_ORD: &str = r#"[
    {"type":"user","content":[{"type":"text","text":"two searches"}]},
    {"type":"discovery","item":{"type":"tool_search_call","id":"tsc_waj2_pair_a","call_id":"call_waj2_pair_a","status":"completed","execution":"client","arguments":{"query":"crm"}}},
    {"type":"discovery","item":{"type":"tool_search_output","id":"tso_waj2_pair_a_stale","call_id":"call_waj2_pair_a","status":"completed","execution":"client","tools":[{"type":"namespace","name":"mcp__ratchet_fixture","tools":[{"type":"function","name":"crm_fixture_tool_00"}]}]}},
    {"type":"reasoning","id":"encitem_waj2_ord2","encrypted_content":"az_ciphertext","summary":[{"type":"summary_text","text":"s"}]},
    {"type":"discovery","item":{"type":"tool_search_call","id":"tsc_waj2_d1","call_id":"call_waj2_d1","status":"completed","execution":"client","arguments":{"query":"crm order management","limit":8}}},
    {"type":"discovery","item":{"type":"tool_search_output","id":"tso_waj2_d1_stale","call_id":"call_waj2_d1","status":"completed","execution":"client","tools":[{"type":"namespace","name":"mcp__ratchet_fixture","tools":[{"type":"function","name":"crm_fixture_tool_00"}]}]}},
    {"type":"assistant","content":"found them","model_id":"gpt-5.6-sol"}
]"#;

/// The mint for `("gpt-5.6-sol", 0, "call_waj2_pair_a", namespaced tools)`.
/// Re-derived outside Rust: `json.dumps` of the preimage
/// `"gpt-5.6-sol|0|call_waj2_pair_a|…"`, sha256, first 24 hex.
const D1_PAIR_A_ORD0_TSO: &str = "tso_7f946c06854b0cabddb17a0e";

/// The mint for `("gpt-5.6-sol", 1, "call_waj2_d1", namespaced tools)` — the ordinal
/// `D1_TWO_PAIR_ORD`'s second output must land on. It is also the documented
/// "any shift of `ord` off 0 lands elsewhere" answer above `D1_TERRA_TSO`, and it
/// differs from `EXPECTED_TSO_D1` (same pair, `ord = 0`) by that one term alone.
const D1_PAIR_B_ORD1_TSO: &str = "tso_bbb0a8fa07d62a107b690edb";

/// The provider bytes of a discovery item, whole-value (not field-by-field).
fn raw_json(item: &ConversationItem) -> serde_json::Value {
    item.discovery()
        .expect("fixture row must be a Discovery item")
        .raw()
        .clone()
}

/// The keys of a discovery item's `raw` in DOCUMENT order (`serde_json` is built
/// with `preserve_order`, so this is the order the provider wrote, not a sorted
/// one) — the observable key-order preservation PA-7's byte doctrine needs.
fn raw_key_order(item: &ConversationItem) -> Vec<String> {
    item.discovery()
        .expect("fixture row must be a Discovery item")
        .raw()
        .as_object()
        .expect("discovery raw is an object")
        .keys()
        .cloned()
        .collect()
}

/// The projected history's items as one comparable value: "byte-identical" in
/// this file means this, so a re-write anywhere in the item shows up.
fn items_json(items: &[ConversationItem]) -> serde_json::Value {
    serde_json::to_value(items).expect("history must serialize")
}

/// The `tool_search_output` id that `project_discovery_rekey` leaves on `cell`.
fn rekeyed_output_id(items: &[ConversationItem], cell: &str) -> String {
    let projected = super::projection::project_discovery_rekey(items, cell);
    let slice = d1_discovery_slice(&projected);
    let output = d1_half(&slice, super::tool_search::ToolSearchKind::Output);
    output
        .id()
        .expect("fixture output carries an id")
        .to_string()
}

/// Every projected `tool_search_output` id, in transcript order — the view
/// [`rekeyed_output_id`] cannot give, because that one stops at the first output and
/// so structurally cannot see how a second output was numbered.
fn rekeyed_output_ids(items: &[ConversationItem], cell: &str) -> Vec<String> {
    super::projection::project_discovery_rekey(items, cell)
        .iter()
        .filter_map(ConversationItem::discovery)
        .filter(|search| search.kind() == super::tool_search::ToolSearchKind::Output)
        .map(|search| search.id().unwrap_or_default().to_string())
        .collect()
}

/// The stored bytes of a discovery item with ONLY its `id` rewritten — the whole-item
/// oracle for a re-key. Built from the stored `raw` (not from typed accessors, which
/// would beg the key-order question this file is pinning) so a projection that added,
/// dropped or re-ordered any other key of `raw` fails against it.
fn rekey_oracle(item: &ConversationItem, minted: &str) -> serde_json::Value {
    let mut expected = raw_json(item);
    expected
        .as_object_mut()
        .expect("discovery raw is an object")
        .insert("id".to_string(), serde_json::Value::String(minted.to_string()));
    expected
}

/// PA-12 (T-11): the mint runs ONLY on an output that already carries an `id`.
/// Our own scored probe authors the output with no id at all, so a re-key must
/// leave that item exactly as stored — minting onto it manufactures a handle the
/// origin never issued, which `tool_search.rs`'s own doctrine forbids ("copy what
/// is there, mint nothing"). Killed by MUT-D1e (unconditional mint).
#[test]
fn an_absent_output_id_stays_absent() {
    let (items, _) = items_from_fixture(D1_R6_OUTPUT_WITHOUT_ID);
    let source = d1_discovery_slice(&items);
    assert_eq!(
        d1_half(&source, super::tool_search::ToolSearchKind::Output).id(),
        None,
        "fixture sanity: the R6 shape omits the id"
    );

    let projected_items = super::projection::project_discovery_rekey(&items, "gpt-5.6-sol");
    let projected = d1_discovery_slice(&projected_items);
    let projected_output = d1_half(&projected, super::tool_search::ToolSearchKind::Output);
    assert_eq!(
        projected_output.id(),
        None,
        "PA-12: an absent id must stay absent — minting one advertises a handle the \
         origin never issued"
    );
    assert_eq!(
        items_json(&projected_items),
        items_json(&items),
        "PA-12: the R6-shaped pair must project verbatim, key set included"
    );
    let mut keys: Vec<String> = raw_key_order(&projected_items[1]);
    keys.sort();
    assert_eq!(
        keys,
        ["call_id", "execution", "status", "tools", "type"].map(String::from),
        "PA-12 observable: the projected key set is exactly the stored one"
    );
}

/// PA-11 + T-12: an ABSENT `tools` key canonicalises as `[]`, exactly the
/// missing-key rule `xw_reasoning_id` applies to an absent `content`. Absent and
/// `[]` are therefore the same logical record and MUST mint the same id, and that
/// id is the oracle's `[]` answer — not the answer a `null` preimage would give.
/// Killed by MUT-D1f (canonicalise absent as `null`).
#[test]
fn a_missing_tools_key_canonicalises_as_empty_array() {
    let (without_tools, _) = items_from_fixture(D1_OUTPUT_WITHOUT_TOOLS);
    let (empty_tools, _) = items_from_fixture(D1_OUTPUT_WITH_EMPTY_TOOLS);
    let absent = rekeyed_output_id(&without_tools, "gpt-5.6-sol");
    let empty = rekeyed_output_id(&empty_tools, "gpt-5.6-sol");
    assert_eq!(
        absent, empty,
        "PA-11: absent `tools` and `tools: []` are one logical record"
    );
    assert_eq!(
        absent, D1_EMPTY_TOOLS_TSO,
        "PA-11: the absent key must canonicalise as `[]`, not `null`"
    );

    // The absent-key default is a MINT input only — it must never reach the item. An
    // implementation that wrote the canonicalised `[]` back into `raw` alongside the
    // mint mints the same id and so passes every id assert above; only a whole-item
    // compare against the stored bytes catches it. `raw_key_order` is compared
    // UNSORTED: a materialised `tools` would also append a key, but the order assert
    // is what pins that nothing else moved either.
    let projected_absent = super::projection::project_discovery_rekey(&without_tools, "gpt-5.6-sol");
    assert_eq!(
        raw_json(&projected_absent[1]),
        rekey_oracle(&without_tools[1], &absent),
        "PA-7: absent `tools` canonicalises as `[]` for the mint but must NOT \
         materialise as a `tools` key on the projected item"
    );
    assert_eq!(
        raw_key_order(&projected_absent[1]),
        raw_key_order(&without_tools[1]),
        "T-6: the absent-tools output keeps its stored key order and key set exactly"
    );
    assert_eq!(
        raw_json(&projected_absent[0]),
        raw_json(&without_tools[0]),
        "PA-13: the call half of an absent-tools pair is byte-identical"
    );
}

/// T-9 plus the `{ord}` clause of PA-11. Two claims, both needed: the projection
/// is idempotent (XD-6 / S-1 — the retry path re-sends byte-identical payload),
/// AND `{ord}` counts discovery OUTPUTS rather than items, so the `reasoning` row
/// interleaved ahead of the pair must not shift the id. The second clause is what
/// kills MUT-D1c: idempotence alone survives an item-indexed `ord`, so T-9's
/// byte-equality assert on its own cannot detect that mutation.
#[test]
fn the_mint_is_stable_across_a_second_projection() {
    let (items, _) = items_from_fixture(D1_PAIR_AFTER_REASONING);
    assert_eq!(
        items[3]
            .discovery()
            .expect("pair member")
            .id()
            .expect("stored id"),
        "tso_waj2_d1_stale",
        "fixture sanity: the output sits at ITEM index 3 while being output #0"
    );

    let once = super::projection::project_discovery_rekey(&items, "gpt-5.6-sol");
    assert_eq!(
        rekeyed_output_id(&items, "gpt-5.6-sol"),
        EXPECTED_TSO_D1,
        "PA-11: an interleaved reasoning row must not shift `ord` off 0"
    );
    let twice = super::projection::project_discovery_rekey(&once, "gpt-5.6-sol");
    assert_eq!(
        items_json(&twice),
        items_json(&once),
        "XD-6 / S-1: a second projection must be byte-identical, not just equal in ids"
    );
    assert_eq!(
        once.len(),
        items.len(),
        "D1 removes nothing: every item of the input survives the re-key"
    );
}

/// PA-11 + T-10: `{cell}` is the target row in the preimage, so re-keying to a
/// different row MUST change the id — that is the entire point of D1. Killed by
/// MUT-D1d (drop `{cell}`), which silently degrades D1 to D0: the origin row's id
/// rides the new row's request. PA-13's half is checked too: the call half is
/// byte-identical across both rows, because it carries nothing client-mintable.
#[test]
fn the_mint_changes_when_the_target_row_changes() {
    let (items, _) = items_from_fixture(D1_STALE_PAIR);
    let source = d1_discovery_slice(&items);
    let source_call = raw_json(&items[1]);

    let sol = rekeyed_output_id(&items, "gpt-5.6-sol");
    let terra = rekeyed_output_id(&items, "gpt-5.6-terra");
    assert_eq!(sol, EXPECTED_TSO_D1);
    assert_eq!(terra, D1_TERRA_TSO);
    assert_ne!(
        sol, terra,
        "PA-11: two target rows must not mint one id (D1 would be D0)"
    );

    let sol_items = super::projection::project_discovery_rekey(&items, "gpt-5.6-sol");
    let terra_items = super::projection::project_discovery_rekey(&items, "gpt-5.6-terra");
    assert_eq!(
        raw_json(&sol_items[1]),
        source_call,
        "PA-13: the tool_search_call is byte-identical on a D1 projection"
    );
    assert_eq!(
        raw_json(&sol_items[1]),
        raw_json(&terra_items[1]),
        "PA-13: the call half cannot depend on the target row — it carries no \
         client-mintable id (both `tsc_` and `call_id` are provider-minted)"
    );
    assert_eq!(
        source.len(),
        d1_discovery_slice(&sol_items).len(),
        "PA-14: re-keying drops neither half"
    );
}

/// The DOMAIN of `{ord}` (PA-11 / XD-6): `ord` is the index among the discovery
/// outputs of the slice handed to the callee, so two outputs of one history mint two
/// different handles, and the second one mints under `ord = 1`.
///
/// This is the clause no other test here could see: every other rekey fixture holds
/// exactly one output, so an implementation that minted every output at `ord = 0` was
/// green across the whole file. That bug is not cosmetic — two outputs of one
/// conversation collapsing onto one handle is a wrong join, not a cache-break.
///
/// The second half of the test states the flip side of the same fact and is the
/// reason the doc calls the slice a contract: handing the callee a WINDOW that starts
/// at the second pair renumbers it to `ord = 0`, i.e. mints different bytes for the
/// same stored item.
#[test]
fn two_discovery_pairs_mint_ordinal_distinct_handles() {
    let (items, _) = items_from_fixture(D1_TWO_PAIR_ORD);
    assert_eq!(
        items
            .iter()
            .filter_map(ConversationItem::discovery)
            .filter(|search| search.kind() == super::tool_search::ToolSearchKind::Output)
            .count(),
        2,
        "fixture sanity: two outputs, or the ordinal domain is untested"
    );

    let ids = rekeyed_output_ids(&items, "gpt-5.6-sol");
    assert_eq!(
        ids,
        [D1_PAIR_A_ORD0_TSO.to_string(), D1_PAIR_B_ORD1_TSO.to_string()],
        "PA-11: `ord` is the discovery-OUTPUT index within the handed slice — output \
         #0 and output #1 of one history must not share a handle"
    );
    assert_ne!(
        ids[0], ids[1],
        "PA-11: an implementation that minted every output at `ord = 0` gives one id here"
    );

    // The windowed-slice flip side, stated as a measurement rather than as prose: the
    // same stored pair, handed to the callee as its own slice, mints under `ord = 0`.
    // `D1_TWO_PAIR_ORD[3..]` starts at the interleaved reasoning row and holds only
    // the second pair.
    assert_eq!(
        rekeyed_output_ids(&items[3..], "gpt-5.6-sol"),
        [EXPECTED_TSO_D1.to_string()],
        "the ordinal is positional in the handed slice, not a property of the stored \
         item — the caller must hand the same (full) history every time or the handle \
         moves under the provider's cache (see the slice contract on \
         `project_discovery_rekey`)"
    );

    // Re-projecting the whole history is still a fixed point: the second projection
    // sees the same output positions, so the ids it mints are the ids already there.
    let once = super::projection::project_discovery_rekey(&items, "gpt-5.6-sol");
    let twice = super::projection::project_discovery_rekey(&once, "gpt-5.6-sol");
    assert_eq!(
        items_json(&twice),
        items_json(&once),
        "XD-6 / S-1: idempotence holds pair-for-pair, not just on a one-output history"
    );
}

/// PA-7 byte-verbatim ON THE REKEY ARM (design §3 step 4: rewrite the `id` key **in
/// place** so every other key **and the key ORDER** survive).
///
/// `d0_preserves_key_order` pins this for the keep arm only, and that arm cannot
/// fail it — it clones the whole item. The re-key arm is the one that touches `raw`,
/// and `Value` equality is key-order-INSENSITIVE (stated in-crate at
/// `tool_search.rs:4302-4304`), so the whole-item asserts elsewhere in this file
/// cannot see a re-ordering. Only an unsorted `raw_key_order` compare can, which is
/// what this test is for. A rebuild through typed accessors — which is what an
/// in-place `insert` was chosen to avoid — re-emits the modelled keys in the order the
/// rebuild happens to write them and drops both unmodelled keys; both are fatal here.
/// A re-ordered prefix is a cache-break ($), not cosmetics (`tool_search.rs:4313`).
#[test]
fn d1_rekey_preserves_key_order_and_unmodelled_keys() {
    let (items, _) = items_from_fixture(D1_UNKNOWN_KEY_REKEY_PAIR);
    let stored_output_keys = raw_key_order(&items[1]);
    let mut sorted = stored_output_keys.clone();
    sorted.sort();
    assert_ne!(
        stored_output_keys, sorted,
        "fixture sanity: the stored order is not alphabetical, or the order assert \
         below is vacuous (this is the same guard `d0_preserves_key_order` runs)"
    );
    assert_eq!(
        stored_output_keys
            .iter()
            .position(|key| key == "id")
            .expect("the rekey fixture carries an id"),
        3,
        "fixture sanity: `id` sits at document position 3, where a sorted or rebuilt \
         item would not put it"
    );

    let projected = super::projection::project_discovery_rekey(&items, "gpt-5.6-sol");
    assert_eq!(
        raw_json(&projected[1]),
        rekey_oracle(&items[1], EXPECTED_TSO_D1),
        "PA-7: `id` is the ONLY key a re-key touches, value-wise, unknown keys included"
    );
    assert_eq!(
        raw_key_order(&projected[1]),
        stored_output_keys,
        "PA-7 / T-6 on the REKEY arm: `id` keeps its document position and no other \
         key is reordered — a sorted or accessor-rebuilt `raw` fails here while Value \
         equality passes it"
    );
    assert!(
        raw_json(&projected[1])
            .get("z_provider_debug")
            .is_some(),
        "A-26: the unmodelled `z_provider_debug` must ride along — the strip-list \
         belongs to the pairing/repair path, not to this seam"
    );
    assert_eq!(
        raw_json(&projected[0]),
        raw_json(&items[0]),
        "PA-13: the `tool_search_call` half is byte-identical, echoed `created_by` \
         included — it carries nothing the target row owns"
    );
    assert_eq!(
        raw_key_order(&projected[0]),
        raw_key_order(&items[0]),
        "PA-13 / T-6: and its key order too"
    );
}

/// PA-12's third shape: an output whose `id` key is PRESENT and EMPTY. The re-key
/// gate reads [`super::tool_search::ToolSearchItem::id`], which filters `""`
/// (`tool_search.rs:556-561`), so an empty handle reads as "has none" and the item is
/// left byte-identical rather than re-minted. That is a DECISION, not an accident of
/// accessor semantics: re-minting would manufacture a handle for a record the origin
/// declined to name, which is exactly what PA-12 forbids ("copy what is there, mint
/// nothing"). The counterfactual is asserted by name below so a future re-point of
/// this branch is a deliberate edit rather than a silent flip.
#[test]
fn an_empty_output_id_reads_as_absent_and_rides_verbatim() {
    let (items, _) = items_from_fixture(D1_OUTPUT_WITH_EMPTY_ID);
    assert_eq!(
        raw_json(&items[1]).get("id").and_then(|id| id.as_str()),
        Some(""),
        "fixture sanity: the key is PRESENT and empty (a raw-key view, not the accessor)"
    );
    assert_eq!(
        items[1].discovery().expect("output half").id(),
        None,
        "fixture sanity: the accessor this gate reads filters the empty string"
    );

    let projected = super::projection::project_discovery_rekey(&items, "gpt-5.6-sol");
    assert_eq!(
        raw_json(&projected[1]),
        raw_json(&items[1]),
        "PA-12: a present-but-empty id reads as absent, so the item rides verbatim"
    );
    assert_eq!(
        raw_key_order(&projected[1]),
        raw_key_order(&items[1]),
        "T-6: and the empty-`id` item keeps its key order and key set exactly"
    );
    // Named counterfactual: the mint for
    // `("gpt-5.6-sol", 0, "call_waj2_emptyid", namespaced tools)`, re-derived outside
    // Rust. Asserting against the specific value the other gate would produce is what
    // makes the assert above a decision rather than a tautology about clones.
    assert_ne!(
        raw_json(&projected[1]).get("id").and_then(|id| id.as_str()),
        Some("tso_62472ec9a2001c47c1e81563"),
        "if this ever fires, the gate moved from `id().is_none()` to raw-key presence \
         and now re-mints an id the origin left empty — that is a PA-12 decision, and \
         it must be re-argued, not stumbled into"
    );
    assert_eq!(
        raw_json(&projected[0]),
        raw_json(&items[0]),
        "PA-13: the call half of an empty-id output pair is byte-identical"
    );
}

/// A pair carrying bytes this harness does not model: the echoed `created_by` a
/// live replay rejected (PLAN:1325-1328), and an unknown `z_provider_debug` key on
/// the output written in a NON-alphabetical document position. The projection
/// seam's only obligation about these bytes is to keep them (`tool_search.rs`: the
/// strip-list belongs to the pairing/repair path, not to this type), so this is the
/// guard on the interim keep arm at `projection.rs` — T-4/T-5/T-6.
const D0_UNKNOWN_KEY_PAIR: &str = r#"[
    {"type":"discovery","item":{"type":"tool_search_call","created_by":"provider","id":"tsc_waj2_d0","call_id":"call_waj2_d0","status":"completed","execution":"client","arguments":{"query":"crm order management"}}},
    {"type":"discovery","item":{"type":"tool_search_output","z_provider_debug":{"shard":"eu-1"},"id":"tso_waj2_d0","call_id":"call_waj2_d0","status":"completed","execution":"client","tools":[{"type":"namespace","name":"mcp__ratchet_fixture","tools":[{"type":"function","name":"crm_fixture_tool_00"}]}]}},
    {"type":"assistant","content":"found them","model_id":"gpt-5.6-sol"}
]"#;

/// The three boundary regimes this seam can be handed, as the existing keep test
/// enumerates them.
const ALL_BOUNDARIES: [Boundary; 3] =
    [Boundary::AzStrict, Boundary::VLLenient, Boundary::Vertex];

/// T-4 (PA-7): the interim D0 arm keeps BOTH halves byte-identically across a
/// cross-row switch, including the keys no code in this crate models. Losing the
/// unknown key is the invisible half of A-26: the provider rebuilds the loaded tool
/// set from these bytes.
#[test]
fn d0_keeps_the_pair_byte_identically_including_unknown_keys() {
    let (items, _) = items_from_fixture(D0_UNKNOWN_KEY_PAIR);
    for boundary in ALL_BOUNDARIES {
        let projected = project_switch_history(&items, "gpt-5.6-terra", boundary, None);
        assert_eq!(
            raw_json(&projected.items[0]),
            raw_json(&items[0]),
            "{boundary:?} rewrote the tool_search_call's provider bytes"
        );
        assert_eq!(
            raw_json(&projected.items[1]),
            raw_json(&items[1]),
            "{boundary:?} rewrote the tool_search_output's provider bytes"
        );
    }
}

/// T-5: the keep arm records nothing, and a keep that DID record a drop would be
/// the ledger lying about an item it did not remove. "No tools change" is scoped
/// honestly: this seam has no `tools[]` surface at all (the D2 declaration half is
/// ruling apex-waj.18's send-time encoder), so the observable here is that the
/// projection touches no item of the history.
#[test]
fn d0_emits_no_drop_and_no_tools_change() {
    let (items, _) = items_from_fixture(D0_UNKNOWN_KEY_PAIR);
    for boundary in ALL_BOUNDARIES {
        let projected = project_switch_history(&items, "gpt-5.6-sol", boundary, None);
        assert!(
            projected.drops.is_empty(),
            "{boundary:?} recorded a drop of an item it kept: {:?}",
            projected.drops
        );
        assert_eq!(
            items_json(&projected.items),
            items_json(&items),
            "{boundary:?} changed a record it was only allowed to keep"
        );
    }
}

/// T-6: the byte-verbatim doctrine covers key ORDER too. `serde_json` is built with
/// `preserve_order`, so a rebuild through a typed accessor or a `Map<String, Value>`
/// without that feature would silently re-sort the provider's object.
#[test]
fn d0_preserves_key_order() {
    let (items, _) = items_from_fixture(D0_UNKNOWN_KEY_PAIR);
    let stored_order = raw_key_order(&items[1]);
    let mut sorted = stored_order.clone();
    sorted.sort();
    assert_ne!(
        stored_order, sorted,
        "fixture sanity: the stored order is not alphabetical, or this test is vacuous"
    );
    for boundary in ALL_BOUNDARIES {
        let projected = project_switch_history(&items, "gpt-5.6-sol", boundary, None);
        assert_eq!(
            raw_key_order(&projected.items[1]),
            stored_order,
            "{boundary:?} reordered the keys of a discovery item's raw bytes"
        );
    }
}

/// PA-28 / PA-29 (T-28): the D3 strip emits no typed discovery item AND records one
/// drop per removed item — never one drop for the pair. The partition claim
/// (`items` + `drops` account for every input row) is asserted HERE rather than
/// left to the seam's `debug_assert_eq!`, which the release GATE compiles out, so a
/// one-drop-for-the-pair implementation would otherwise under-count the ledger with
/// nothing failing. Killed by MUT-D3a; its label clause is also T-29's falsifier
/// (PA-29 forbids borrowing a Vertex label whose text asserts a different class).
#[test]
fn d3_removes_both_halves_and_records_both() {
    let (items, _) = items_from_fixture(D1_STALE_PAIR);
    let (kept, drops) = super::projection::strip_discovery_pair(&items, 0);

    assert_eq!(
        kept.iter().filter(|item| item.discovery().is_some()).count(),
        0,
        "PA-28: no typed discovery item may survive a D3 projection"
    );
    assert_eq!(
        drops,
        [
            super::projection::ProjectionDrop {
                index: 1,
                reason: super::projection::DropReason::DiscoveryDemoted,
            },
            super::projection::ProjectionDrop {
                index: 2,
                reason: super::projection::DropReason::DiscoveryDemoted,
            },
        ],
        "PA-29: one drop per removed half, carrying its SOURCE index, in source order"
    );
    let labels: Vec<&str> = drops.iter().map(|drop| drop.reason.as_str()).collect();
    assert_eq!(
        labels,
        ["discovery_demoted", "discovery_demoted"],
        "PA-29: the demotion names itself, not a Vertex arm"
    );
    assert_eq!(
        kept.len() + drops.len(),
        items.len(),
        "PA-29: kept items and recorded drops must partition the input"
    );
    assert_eq!(
        items_json(&kept),
        items_json(&[items[0].clone(), items[3].clone()]),
        "PA-28 strips the discovery pair and nothing else"
    );

    let (offset_kept, offset_drops) = super::projection::strip_discovery_pair(&items, 7);
    assert_eq!(
        offset_drops.iter().map(|drop| drop.index).collect::<Vec<_>>(),
        [8, 9],
        "the recorded index is a SOURCE index, not an index into the slice handed in"
    );
    assert_eq!(offset_kept.len(), kept.len());
}
