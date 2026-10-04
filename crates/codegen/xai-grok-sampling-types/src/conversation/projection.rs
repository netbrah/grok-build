//! Switch-time projector — proactive per-item cross-wire projection (apex-ayl.71).
//!
//! Companion to `sdd-71-projector.md`: applies the T0-T3 fidelity ladder
//! (xwfix README) per item, at model-switch time, to the STORAGE form
//! (`ConversationItem`) — dialect-agnostic (the send pipeline stays
//! dialect-specific). No new storage: per-item provenance comes from the
//! per-turn `model_id` fields already persisted on assistant records
//! (sdd-71 §2).
//!
//! Ladder (per reasoning item; tool/user/assistant items follow the
//! §4 invariants):
//! - T0 — same model (= same boundary): verbatim, id + field markers kept
//!   (post-.75 same-boundary KEEP default).
//! - T1 — foreign origin: `xw_` re-key + `encrypted_content` stripped +
//!   summary kept. The one no-re-key row is AZ -> AZ (strict targets: the
//!   strict projector IS the strip — id+content removed pre-send, so the
//!   store keeps the original id; matrix §1.4, sdd-71 §3).
//! - T3 (vertex targets only) — the `/messages` wire has no client-wire
//!   site for non-carrier backend tool call items (the build degrades them
//!   to synthetic text and its D5 pairing drops any result that does not
//!   pair with an ASSISTANT call), so the pair-atomic remedy is drop both
//!   (xwfix invariant 3 / sdd-71 §4 invariant 1). `CodexRawInput` carriers
//!   are the exception: carrier survival (invariant 2/4) keeps them opaque
//!   on every tier.
//!
//! Invariants enforced (sdd-71 §4): pairing integrity · carrier survival ·
//! idempotence (`project(project(h)) == project(h)`) · no empty id · no
//! foreign `encrypted_content` · non-projected items come back equal **as
//! parsed JSON values** (the spec labels this invariant “byte-identity”; every
//! comparison in this module round-trips through `serde_json::Value`, so key
//! order, number spelling and escape spelling are outside what it enforces).
//! Removal accounting: every item the projector removes is recorded in
//! `ProjectedHistory::drops`. The mandate is §4.2 rule 13 ("The transition
//! MUST be recorded, never silent.") — written for D3 — extended to the two
//! existing Vertex drop arms: the T3 non-carrier drop and its pair-atomic
//! co-drop. It is
//! NOT XD-1 SURVIVAL: XD-1's subject is a `Discovery` item's presence in the
//! projected history (§4.3), `ConversationItem` has no such variant until
//! apex-waj.21 lands, and no test here can fail for what XD-1 forbids. Reach
//! against XD-1..XD-6, stated rather than implied: this ledger pins XD-2 PAIR
//! ATOMICITY's DROP direction for the one pair class that exists here —
//! `vertex_target_records_both_drop_reasons` records the non-carrier call
//! together with the result co-dropped with it — and
//! `legacy_search_tool_discovery_round_survives_every_boundary` pins XD-2's KEEP
//! direction for the live discovery round, the `search_tool` pair surviving every
//! boundary. XD-6 (idempotence) is also partially pinned. XD-1, XD-3, XD-4 and
//! XD-5 have no discovery subject at this seam until apex-waj.21 lands.
//!
//! Discovery tier decision (xwire-boundary-map.md §4.1): [`discovery_tier`]
//! decides which of the D0/D1/D2/D3 tiers a discovery record takes on a target
//! route, off `ApiBackend` + `Boundary` + the target row's admission gate: §9
//! item 6 is the no-fourth-family rule and names only `Boundary` + `ApiBackend`,
//! while the admission gate is item 3's third key ("keyed on `Boundary` plus
//! `ApiBackend`, and on the TARGET row's admission gate"), not a fourth name for
//! a family.
//! The projector arm itself is NOT here: it is pending beads apex-waj.21 /
//! apex-waj.5 / apex-waj.11, and this function has no production caller yet.
//!
//! Dependency note (sdd-71 §9 step 7, the named G3 item): the T1 id grammar
//! needs SHA-256. `sha2` is a workspace dependency but NOT a direct
//! dependency of this crate, and adding one is outside this cut's file set
//! (any pathspec expansion is a coordinator ruling), so the digest is
//! implemented self-contained below and byte-pinned by the 12/12
//! known-answer goldens in `projection_tests::xw_proj_id_grammar_canonical`
//! plus the in-file KATs.

use std::collections::HashSet;

use serde_json::Value;

use super::responses::SearchAdmission;
use super::{
    BackendToolCallItem, BackendToolKind, ConversationItem, EncAffinityVerdict, ReasoningItemStore,
    enc_affinity_gate,
};
use crate::catalog_wire::{CatalogFamily, catalog_family};
use crate::messages_model::is_anthropic_model;
use crate::rs::ReasoningItem;
use crate::types::ApiBackend;

/// Target boundary regime for a switch projection (prep report decision D2 —
/// the three regimes the Table-2 rows need: sol/terra = AZ-strict rows,
/// qwen/glm = VL-lenient vLLM-shim rows, sonnet = VX-M vertex rows).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boundary {
    /// AZ-strict rows (gpt-5.6 sol/terra/luna): the strict projector strips
    /// id+content pre-send — the schema form IS the strip; do NOT re-key.
    AzStrict,
    /// VL-lenient rows (vLLM-shim families, qwen/glm): T1 re-key for foreign
    /// reasoning, T0 for own.
    VLLenient,
    /// Vertex rows (VX-M, claude on `/messages`): the build has no
    /// responses-native site for backend tool call items; non-carrier calls
    /// are pair-atomically dropped with their results at projection time.
    Vertex,
}

/// Why the projector removed an item. The mandate is §4.2 rule 13 ("The
/// transition MUST be recorded, never silent."), written for D3, extended to
/// the existing Vertex arms: a removal must be visible in the projection
/// result, never a silent skip. One variant per existing Vertex drop arm.
/// XD-1 SURVIVAL (§4.3) is a different subject — the `Discovery` item's own
/// presence — and is pending apex-waj.21. Surfacing this type outside the
/// crate is bead apex-waj.37.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// The T3 arm: a non-carrier backend tool call on a Vertex target —
    /// the `/messages` wire has no client-wire site for it (module ladder,
    /// T3).
    VertexNonCarrierBackendCall,
    /// The pair-atomic co-drop arm: a tool result with no assistant
    /// tool_call to pair with on a Vertex target (covers results paired
    /// only with a dropped backend call and pre-existing orphans alike).
    VertexUnpairedToolResult,
}

impl DropReason {
    /// Stable label for a telemetry drop report. Nothing renders it yet: there
    /// are TWO switch-time log records and neither can see `drops` —
    /// `xai-chat-state/src/actor/mutations.rs:318-324` (`tracing::info!`, fired
    /// only when `changed > 0`, fields `target_model` + `changed`) and
    /// `xai-grok-shell/src/session/acp_session_impl/switch_projection.rs:50-58`
    /// (`unified_log::warn("shell.turn.switch_projection_persisted", …)`, fired
    /// unconditionally with `{"model_id", "outcome", "changed"}`). The accounting
    /// field the two share is `changed`. Surfacing this ledger next to it is bead
    /// apex-waj.37 (in `xai-chat-state` and in the shell — outside this lane).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::VertexNonCarrierBackendCall => "vertex_non_carrier_backend_call",
            Self::VertexUnpairedToolResult => "vertex_unpaired_tool_result",
        }
    }
}

/// One recorded removal: the removed item's SOURCE index plus the reason
/// (§4.2 rule 13 by extension). A switch that deleted items would be
/// distinguishable from one that only re-keyed reasoning items ONCE the actor
/// surfaces this ledger: today `xai-chat-state/src/actor/mutations.rs:306-324`
/// keeps only `.items` and logs `changed`, which `projection_changed_count`
/// (`:67-86`) collapses to `stored.len().max(projected.len())` on a length
/// mismatch. Enabled, not delivered — bead apex-waj.37.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionDrop {
    pub index: usize,
    pub reason: DropReason,
}

/// Projected post-switch history (prep report decision D2 — public `items`
/// field; the L0 suite accesses it through the single `proj_items` choke
/// point).
#[derive(Debug, Clone)]
pub struct ProjectedHistory {
    pub items: Vec<ConversationItem>,
    /// Every removal, in source order (§4.2 rule 13 by extension). Bounded
    /// structurally, not by a cap: the loop routes every input item to exactly
    /// one of the two vectors, so `items` and `drops` partition the input —
    /// asserted by a `debug_assert_eq!` at construction. Debug asserts do not
    /// run in the release GATE, so the executable half of that claim is
    /// `tests::vertex_drops_over_the_x71_case1_shape`. Surfacing this field
    /// outside the crate is bead apex-waj.37.
    pub drops: Vec<ProjectionDrop>,
}

/// Project a persisted history for a cross-wire switch to
/// `target_model_id` on `boundary` (sdd-71 §9 — decision D1: the name and
/// signature are fixed by the spec and the Table-2 tests).
///
/// The `target_model_id` string doubles as the ID-grammar `{cell}` slot
/// (D3 / sdd-71 §5: at runtime it is the target row's model id; in L0 unit
/// tests the fixture cell name is passed explicitly so the 12/12 goldens
/// reproduce).
///
/// `target_pin` (XW-ENC-AFFINITY-1, apex-mf6) is the target row's
/// `x-litellm-tags` pin (`None` = untagged, an empty-string pin normalized
/// at the call site): it feeds the switch-time gate on the one no-re-key
/// AZ -> AZ row (design §3.4) and is inert everywhere else.
///
/// `_route` (apex-waj.35, SPEC-W2 PA-1) is the target row's route tuple: which API the
/// switch lands on, which boundary regime that row is, and whether its admission gate
/// serves native discovery. It is RECEIVED here and not yet READ: the four `DiscoveryTier`
/// arms that consume it are apex-waj.2, and `boundary` deliberately stays a separate
/// argument rather than folding into `route.boundary`, because it is load-bearing for the
/// three shipped T1/T3 arms (`discovery_tier` reads the tuple, these arms read the
/// argument) and re-deriving it here would change three shipped behaviours in one edit.
/// PA-2's fail-closed on a `boundary != route.boundary()` tuple is owed to the same arms:
/// a demotion is not expressible in this seam yet (`DropReason` has no discovery variant,
/// so a recorded drop here could not be accounted), and a `debug_assert!` would not satisfy
/// PA-2 anyway — release builds skip it, which is exactly the hole its text warns about.
/// Its test, `an_inconsistent_route_tuple_demotes_and_records` (SPEC-W2 §4.1 T-2 / MUT-R2),
/// therefore belongs with the arms on **apex-waj.2**, together with PA-29's `DropReason`
/// variant; it is named here so the MUST is tracked rather than dropped between the two beads.
/// The `boundary` argument could fold into the tuple once that guard exists, and not before.
/// The underscore is therefore "unconsumed as of this cut", not "ignored by design"; the
/// value itself reaches here from the target row through `xai-chat-state`
/// (`TargetRoute::new` names the one production caller).
pub fn project_switch_history(
    items: &[ConversationItem],
    target_model_id: &str,
    boundary: Boundary,
    target_pin: Option<&str>,
    _route: &TargetRoute,
) -> ProjectedHistory {
    // Vertex targets only, and ONE direction of the /messages build's D5 pairing
    // (`clean_orphaned_items`, conversation/messages.rs): a `tool_result` reaches
    // that wire only when an ASSISTANT tool_call pairs with it, so a result
    // orphaned by the T3 arm below — or already orphaned in the store — is removed
    // here as well as at build time.
    //
    // The other direction is deliberately NOT mirrored, and nothing here keeps the
    // storage form equal to what the target wire can represent. `messages.rs:109`
    // filters an assistant's `tool_calls` down to the paired set (a rewrite of a
    // stored record) and `messages.rs:115-118` deletes the assistant outright when
    // that leaves no call and no content; assistant items take the passthrough arm
    // below and are cloned verbatim. Why this seam stops at the result direction:
    //   1. it owes pair-atomicity only for the pairs it breaks itself. The T3 arm
    //      removes a backend call, so that call's result goes with it (xwfix
    //      invariant 3 / sdd-71 §4 invariant 1). An assistant call whose result was
    //      never stored is not such a pair, and the build's own D5 pass — plus the
    //      adjacency re-check at `messages.rs:136` — strips it on every `/messages`
    //      build, so it never reaches the target whether or not the store was
    //      pre-cleaned;
    //   2. a rewrite has no accounting surface here. `ProjectionDrop` is
    //      `{ index, reason }`: it names a removal. Filtering a `tool_calls` list
    //      changes neither the record count nor the source index, so the partition
    //      claim below stays true over a silently changed item, and rule 13's “MUST
    //      be recorded, never silent” (xwire-boundary-map.md:311) would be false
    //      for exactly the change this ledger cannot name. The deletion half is
    //      expressible only by borrowing `VertexUnpairedToolResult`, whose label is
    //      about a different item;
    //   3. deleting a stored assistant turn is a model-visible history change with
    //      no fixture that can express it (every assistant call in
    //      `fixtures/projection_x71/` pairs with a result), no corpus material, and
    //      no reader of this ledger until apex-waj.37 surfaces it — the only
    //      non-test call site (`xai-chat-state/src/actor/mutations.rs:306-316`)
    //      keeps `.items` and discards `drops`.
    // Owner of the owed half, and of the rewrite-accounting design it needs first:
    // the follow-on bead filed from the px29 F-2 handoff (apex-waj family). The
    // mutation harness's `MUT-H` implements this half. No fixture here can exercise
    // it — every assistant call in `fixtures/projection_x71/` and in the inline
    // shapes pairs with a result — so what the suite pins instead is the accounting
    // rule this seam actually owns: `projection_tests.rs`
    // `vertex_ledger_explains_every_change_to_a_record_it_keeps` fails on any change
    // to a record the ledger does not name, which is the half that can be falsified
    // honestly without pretending the deferred behaviour is specified.
    let mut assistant_call_ids: HashSet<String> = HashSet::new();
    if boundary == Boundary::Vertex {
        for item in items {
            if let ConversationItem::Assistant(a) = item {
                for tc in &a.tool_calls {
                    assistant_call_ids.insert(tc.id.as_ref().to_string());
                }
            }
        }
    }

    let mut projected = Vec::with_capacity(items.len());
    let mut drops = Vec::new();
    let mut reasoning_ord = 0usize;
    for (idx, item) in items.iter().enumerate() {
        match item {
            ConversationItem::Reasoning(r) => {
                let owner = forward_owner_model(items, idx);
                projected.push(ConversationItem::Reasoning(project_reasoning(
                    r,
                    owner,
                    target_model_id,
                    boundary,
                    reasoning_ord,
                    target_pin,
                )));
                reasoning_ord += 1;
            }
            ConversationItem::BackendToolCall(b)
                if boundary == Boundary::Vertex && !is_carrier(b) =>
            {
                // T3: the vertex wire has no site for this class; its result
                // (if any) is co-dropped by the ToolResult arm below —
                // pair-atomic, portable transcript intact.
                drops.push(ProjectionDrop {
                    index: idx,
                    reason: DropReason::VertexNonCarrierBackendCall,
                });
            }
            ConversationItem::ToolResult(t)
                if boundary == Boundary::Vertex
                    && !assistant_call_ids.contains(t.tool_call_id.as_str()) =>
            {
                // Pair-atomic co-drop (covers results paired only with a
                // dropped backend call and pre-existing orphans alike).
                drops.push(ProjectionDrop {
                    index: idx,
                    reason: DropReason::VertexUnpairedToolResult,
                });
            }
            other => projected.push(other.clone()),
        }
    }

    debug_assert_eq!(
        projected.len() + drops.len(),
        items.len(),
        "projected items and recorded drops must partition the input"
    );
    ProjectedHistory {
        items: projected,
        drops,
    }
}

/// A route for the tests whose subject is a shipped arm keyed on `boundary` — the T1/T3
/// reasoning and backend-call arms, and the id-grammar goldens — none of which reads the
/// tuple yet. What such a fixture owes is only a tuple a real row could produce, with an
/// admission that is CLOSED, so no test that is not about admission can be accused of quietly
/// taking an admitted route. Tests that ARE about the ladder build the tuple themselves through
/// [`TargetRoute::new`].
///
/// The backend pairing is arbitrary on purpose, not a ruled wire fact: no production code reads
/// [`TargetRoute::backend`] until the apex-waj.2 arms land, so this only has to be a value some row
/// could have produced. PA-6 rules the opposite direction from what is written here — that
/// `Messages` x non-`Vertex` stays constructible and total — and does NOT say a `Vertex` boundary
/// implies a `Messages` backend, so do not read these three arms as an estate claim about which wire
/// serves which boundary.
#[cfg(test)]
pub(crate) fn inert_route(boundary: Boundary) -> TargetRoute {
    TargetRoute::new(
        match boundary {
            Boundary::Vertex => ApiBackend::Messages,
            Boundary::AzStrict | Boundary::VLLenient => ApiBackend::Responses,
        },
        boundary,
        SearchAdmission {
            supports_search_tool: false,
            has_searchable_tools: false,
        },
    )
}

/// Forward attribution (sdd-71 §2.3): the owning model of a reasoning item
/// is the `model_id` of the next `Assistant` item after it that carries a
/// resolvable model. `None` when unresolvable (trailing run / no owner) —
/// fail-closed: treat as foreign, never KEEP.
fn forward_owner_model(items: &[ConversationItem], idx: usize) -> Option<&str> {
    items.iter().skip(idx + 1).find_map(|item| match item {
        ConversationItem::Assistant(a) => a.model_id.as_deref(),
        _ => None,
    })
}

/// One reasoning item through the ladder (sdd-71 §3 decision table), with
/// the XW-ENC-AFFINITY-1 (apex-mf6) gate on the one no-re-key AZ -> AZ row
/// (design §3.4): the ciphertext is RETAINED in the store when the item's
/// mint tag is compatible with the TARGET row's pin, stripped otherwise;
/// every other tier keeps the .71 strip. The mint tag always rides the
/// projected item (provenance survives the switch).
fn project_reasoning(
    r: &ReasoningItemStore,
    owner: Option<&str>,
    target_model_id: &str,
    boundary: Boundary,
    ord: usize,
    target_pin: Option<&str>,
) -> ReasoningItemStore {
    // T0 — same model = same boundary (sdd-71 §2 rule (i); post-.75
    // same-boundary KEEP default): verbatim, id + field markers kept (and
    // the mint tag along) — the gate is never consulted for T0.
    if owner == Some(target_model_id) {
        return r.clone();
    }

    // Cross-boundary (or cross-deployment within one boundary): T1 —
    // encrypted_content stripped proactively (D-ENC's job, moved to switch
    // time) + re-key — EXCEPT the one no-re-key row, AZ -> AZ: on strict
    // targets the strict projector IS the strip (id+content removed
    // pre-send), so the store keeps the original id.
    let keep_original_id = boundary == Boundary::AzStrict
        && owner.map(model_boundary_class) == Some(Boundary::AzStrict);
    let mut id = if keep_original_id {
        // 0.42.1 widened `Reasoning.id` to `Option<String>`; an absent id is the
        // empty-id anomaly the next guard already repairs with the T1 synthesis.
        r.id.clone().unwrap_or_default()
    } else {
        xw_reasoning_id(target_model_id, ord, &r.item)
    };

    // Invariant 4 (no empty id, the .69 class): a no-re-key id that is
    // empty is an anomaly (strict rows mint `encitem_` markers); repair
    // with the T1 synthesis so no projected reasoning carries `id:""`. The
    // strict send path strips the id pre-send either way.
    if id.is_empty() {
        id = xw_reasoning_id(target_model_id, ord, &r.item);
    }

    // XW-ENC-AFFINITY-1 (apex-mf6, design §3.4): the AZ -> AZ row is the
    // one where the store may keep the ciphertext — the gate decides by
    // (target row pin x item mint tag); Retain / RetainOptimistic keep it,
    // Strip (and every non-AZ -> AZ tier) removes it (the .71 behavior).
    let encrypted_content = if keep_original_id
        && matches!(
            enc_affinity_gate(target_pin, r.mint_tag.as_deref()),
            EncAffinityVerdict::Retain | EncAffinityVerdict::RetainOptimistic
        ) {
        r.item.encrypted_content.clone()
    } else {
        None
    };

    ReasoningItemStore {
        item: ReasoningItem {
            // 0.42.1 widened `rs::ReasoningItem.id` to `Option<String>`; the store
            // id is always Some after the empty-id repair above.
            id: Some(id),
            summary: r.summary.clone(),
            content: r.content.clone(),
            encrypted_content,
            status: r.status.clone(),
        },
        mint_tag: r.mint_tag.clone(),
    }
}

/// Classify a model row into its boundary regime for the re-key decision.
///
/// Slug-keyed (no row metadata is visible at this seam — the row-aware
/// config fields live in the sampler; the L0 projector classifies from the
/// model id, the same key the crate's slug fallbacks use):
/// - `gpt-*` / o-series slugs (the AZ-strict rows) -> [`Boundary::AzStrict`]
/// - `claude*` slugs (vertex rows, `/messages` wire) -> [`Boundary::Vertex`]
/// - everything else (vLLM-shim families, grok, unknown) ->
///   [`Boundary::VLLenient`] — the fail-closed lenient class: unknown
///   owners are never KEEP-eligible (sdd-71 §2.3) and foreign origins on
///   lenient targets re-key.
pub fn model_boundary_class(model_id: &str) -> Boundary {
    match catalog_family(model_id) {
        CatalogFamily::OpenAi => Boundary::AzStrict,
        _ if is_anthropic_model(model_id) => Boundary::Vertex,
        _ => Boundary::VLLenient,
    }
}

/// Whether a backend tool call item is a `CodexRawInput` carrier (the
/// compaction-carrier class that survives projection opaquely on every
/// tier — xwfix invariant 4 / sdd-71 §4 invariant 2).
fn is_carrier(b: &BackendToolCallItem) -> bool {
    matches!(b.kind, BackendToolKind::CodexRawInput(_))
}

/// T1 id synthesis (sdd-71 §5; xwfix README "Id synthesis (T1)"; shared
/// with .69 — one grammar, by construction):
///
/// ```text
/// id = "xw_" + sha256(
///     "{cell}|{ord}|{json.dumps(content,sort_keys=True)}|{json.dumps(summary,sort_keys=True)}"
/// ).hexdigest()[:24]
/// ```
///
/// - `{cell}` := the target row's model id (D3 slot binding; L0 tests pass
///   the fixture cell name).
/// - `{ord}`  := 0-based index of the reasoning item among the reasoning
///   records of the projected history (the .69 send-time patch: among the
///   request input's reasoning items).
/// - A record whose storage form has no `content` key canonicalizes to `[]`
///   (that is what made the 12/12 goldens reproduce).
/// - Canonicalization is Python `json.dumps` defaults, NOT serde_json's
///   compact form (`py_json_canonicalize` below).
fn xw_reasoning_id(cell: &str, ord: usize, r: &ReasoningItem) -> String {
    let content = match &r.content {
        None => Value::Array(Vec::new()),
        Some(parts) => serde_json::to_value(parts).expect("ReasoningTextContent must serialize"),
    };
    let summary = serde_json::to_value(&r.summary).expect("SummaryPart must serialize");
    xw_reasoning_id_values(cell, ord, &content, &summary)
}

/// The shared xw_ id grammar core (sdd-71 §5; the .69 send-time patch
/// `responses::patch_reasoning_empty_ids` calls this so the switch-time
/// projector and the send-time repair share one rule — goldens + L0 +
/// send-time patch can never disagree; the original `rs_`+hash proposal is
/// superseded, sdd-69 §2.5). `content`/`summary` are the item's values as
/// they ride the wire: a missing `content` canonicalizes as `[]` (the 12/12
/// goldens' reproduction rule), a missing `summary` as `null`.
pub(crate) fn xw_reasoning_id_values(
    cell: &str,
    ord: usize,
    content: &Value,
    summary: &Value,
) -> String {
    let canonical_content = py_json_canonicalize(content);
    let canonical_summary = py_json_canonicalize(summary);
    let mut preimage = String::with_capacity(
        cell.len() + 32 + canonical_content.len() + canonical_summary.len(),
    );
    preimage.push_str(cell);
    preimage.push('|');
    preimage.push_str(&ord.to_string());
    preimage.push('|');
    preimage.push_str(&canonical_content);
    preimage.push('|');
    preimage.push_str(&canonical_summary);

    let digest = sha256(preimage.as_bytes());
    let hex = hex_digest(&digest);
    format!("xw_{}", &hex[..24])
}

/// Python-`json.dumps`-default canonicalization (sdd-71 §5
/// "canonicalization parity — implementation-critical"): sorted object
/// keys, `", "` / `": "` separators, `ensure_ascii` escaping. serde_json's
/// default (compact separators, unescaped UTF-8) does NOT match Python and
/// would break the 12/12 known-answer goldens.
fn py_json_canonicalize(value: &Value) -> String {
    let mut out = String::new();
    py_json_write(value, &mut out);
    out
}

fn py_json_write(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(&py_number_repr(n)),
        Value::String(s) => py_json_write_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                py_json_write(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            // Python sorts object keys by code point; Rust `String` order
            // (UTF-8 byte order) is the same order for valid UTF-8.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                py_json_write_string(key, out);
                out.push_str(": ");
                match map.get(*key) {
                    Some(v) => py_json_write(v, out),
                    None => out.push_str("null"),
                }
            }
            out.push('}');
        }
    }
}

fn py_json_write_string(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            ch if (ch as u32) < 0x20 || (ch as u32) > 0x7E => {
                // ensure_ascii: CPython's c_make_encoder rule
                // `c < 0x20 || c > 0x7e` — DEL (0x7F) and every non-ASCII
                // code point escape as `\uXXXX`, surrogate-paired above
                // 0xFFFF.
                let cp = ch as u32;
                if cp > 0xFFFF {
                    let v = cp - 0x10000;
                    out.push_str(&format!(
                        "\\u{:04x}\\u{:04x}",
                        0xD800 + (v >> 10),
                        0xDC00 + (v & 0x3FF)
                    ));
                } else {
                    out.push_str(&format!("\\u{:04x}", cp));
                }
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

/// Python-`float.__repr__`-compatible number formatting for the
/// canonicalizer. The T1 hash input is structurally strings-only
/// (`SummaryPart` / `ReasoningTextContent` carry no numbers), so this path
/// is defensive parity, not a golden-pinned surface.
fn py_number_repr(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    let f = n.as_f64().expect("serde_json Number carries an int or an f64");
    if !f.is_finite() {
        return match f {
            f64::INFINITY => "Infinity".to_string(),
            f64::NEG_INFINITY => "-Infinity".to_string(),
            _ => "NaN".to_string(),
        };
    }
    if f == f.round() && f.abs() < 1e16 {
        // Python always keeps at least one fractional digit: 1.0 -> "1.0".
        return format!("{f:.1}");
    }
    let abs = f.abs();
    if abs >= 1e16 || (abs > 0.0 && abs < 1e-4) {
        // Python scientific: "1e+20", "1e-05" (signed, >= 2 exponent digits).
        let rendered = format!("{f:e}");
        let (mantissa, exp) = rendered
            .split_once('e')
            .unwrap_or((rendered.as_str(), "0"));
        let exp: i32 = exp.parse().unwrap_or(0);
        let sign = if exp >= 0 { '+' } else { '-' };
        return format!("{mantissa}{sign}{:02}", exp.abs());
    }
    format!("{f}")
}

/// Self-contained SHA-256 (FIPS 180-4). See the module-level dependency
/// note for why the campaign `sha2` workspace dependency is not used here.
/// Byte-pinned by `digest_kat` below and, end-to-end, by the 12/12 xw_
/// goldens in the L0 suite.
const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    // Pre-processing: append 0x80, zero-pad to 56 mod 64, then the 64-bit
    // big-endian bit length.
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    let mut w = [0u32; 64];
    for chunk in msg.chunks_exact(64) {
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) = (
            h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7],
        );
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
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

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

/// Which discovery-fidelity tier a discovery record takes on a target route
/// (xwire-boundary-map.md §4.1 ladder). Decided by [`discovery_tier`]; the
/// projector arm that acts on it is pending beads apex-waj.21 / apex-waj.5 /
/// apex-waj.11.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // pending apex-waj.2 (the projector arms that read the tuple) + .21 / .5 / .11; the route tuple itself landed with apex-waj.35
pub(super) enum DiscoveryTier {
    /// D0 — same mint domain, same row, same boundary: the record rides on
    /// verbatim, `wire_ids` included.
    Keep,
    /// D1 — the logical record and its correlation survive; only ids are
    /// adjusted. The implementer of the arm inherits both constraints:
    ///
    /// - `output_item_id` MUST be re-minted deterministically (§4.2 rule 4) so
    ///   the switch is idempotent (XD-6) and S-1 byte-stability holds across
    ///   retries. That mint is REQUIRED AND NOT YET IMPLEMENTED: rule 4 calls
    ///   the v5 suffix helper "existing" and it is not —
    ///   `SYNTHETIC_OUTPUT_ID_NAMESPACE` and `with_suffix("tso", …)` are PLAN
    ///   names (PLAN:23; `tool_search.rs:100` and `:1236` give the mint to
    ///   T15/Tasks), and this crate does not depend on `uuid` at all. The only
    ///   id grammar present in this file today is `xw_` + SHA-256
    ///   (`xw_reasoning_id`, `xw_reasoning_id_values`).
    /// - The provider-minted `tsc_` `call_item_id` MUST be KEPT. Its designated
    ///   owner is the reactive net (`xai-grok-sampling-types/src/error.rs:399-486`
    ///   `is_model_bound_history_error` -> `xai-grok-sampler/src/retry.rs:142-143`
    ///   `RetryDecision::RetryWithModelBoundStateStrip`) — designated, not
    ///   proven: §8 X-1 records the foreign-`tsc_` replay as never exercised
    ///   and probe P4 is outstanding. Pre-emptively stripping the id is
    ///   FORBIDDEN for no evidenced gain (it also breaks survival, §4.2 rule 1)
    ///   — §4.2 rule 5, `UNDECIDED`.
    ///   The clause apex-waj.21's seat needs: that designated remedy is currently
    ///   BLIND to this class. `ConversationRequest::strip_model_bound_state`
    ///   (`conversation.rs:1102`) delegates to `drop_model_bound_items`
    ///   (`conversation.rs:1297-1325`), which drops only `Reasoning` +
    ///   `BackendToolCall` plus the results paired with those calls and KEEPS
    ///   everything else — a 7th `Discovery` variant lands in its `_ => true` arm
    ///   and survives the strip untouched, `tsc_` id included. The recovery is a
    ///   strict one-retry, and on a history whose only model-bound state is the
    ///   discovery pair the strip returns `0`, so the request task fails closed
    ///   there (`retry.rs:135-136`). Teaching the net this class is part of what
    ///   apex-waj.21 owes; until then "designated owner" names a policy, not a
    ///   mechanism that can act on it.
    ReKey,
    /// D2 — the `/messages` wire has no typed discovery item but a MANDATORY
    /// declaration surface, so the record is emitted as the transcript form AND
    /// every loaded entry is declared in `tools[]` (§4.2 rules 6-11).
    Materialise,
    /// D3 — the fail-closed tier: emit no discovery item at all, hand the loaded
    /// set to the legacy `ToolIndex` tier for rediscovery, and record the
    /// demotion (§4.2 rules 12-13).
    Demote,
}

/// The target route a switch lands on: which API it lands on, which boundary
/// regime its row is, and whether its admission gate serves native discovery.
///
/// `backend` is authoritative for the D2-vs-D0/D1 choice — the tier follows the
/// wire the declarations must appear on. `boundary` only refines the Responses
/// tiers and supplies the fail-closed check in [`discovery_tier`].
///
/// `admission` is [`SearchAdmission`], not a bare bool: rule 3 folds two
/// signals — the target row's `flags.supports_search_tool` and a non-empty
/// discovery manifest (`has_searchable_tools`) — through
/// [`SearchAdmission::admitted`] (defined in `conversation::responses`). A bool would let a
/// caller pass `supports_search_tool` alone and take a D0 KEEP on a route that
/// cannot serve the loaded set, with no compile error to catch it.
///
/// Both fields come from ONE target row, so `Messages` with a non-`Vertex`
/// boundary is a route the CALLER MUST NOT build: a `/messages` row IS a Vertex
/// row. Calling that input "impossible" would overstate the type — nothing in the
/// signatures stops a caller assembling the pair. The derivation that keeps the two
/// fields consistent now exists at exactly one production call site (apex-waj.35:
/// `xai-grok-shell/src/session/acp_session_impl/model_switch.rs` builds both off the
/// target row — `ApiBackend` from the row's own backend, `Boundary` from
/// [`model_boundary_class`] of the same row), because it cannot live in the type:
/// `Boundary` keys on the model slug while `ApiBackend` rides `SamplingConfig`
/// (`xai-grok-sampling-types/src/types.rs:1138`), and `infer_api_backend` sends an Anthropic slug to
/// `ChatCompletions`, not `Messages` (`catalog_wire.rs:82-93`). So the obligation stays
/// a requirement on the caller, stated as one, and the decision stays total over the
/// pair rather than rejecting it — total without inventing a fourth family vocabulary
/// (§9 item 6).
/// `messages_target_on_a_non_vertex_boundary_still_materialises` pins what
/// totality answers there.
///
/// Public, built only through [`TargetRoute::new`] (SPEC-W2 PA-4): a caller that could
/// re-assign one axis after construction re-creates exactly the inconsistency the
/// constructor exists to make hard, and PA-2's release-visible fail-closed on that tuple
/// is owed by the arms (apex-waj.2), so nothing else catches it. The three-part widening
/// PA-3 demands for [`SearchAdmission`] (type, fields + `admitted()`, and the
/// `pub use responses::{…}` list in `conversation.rs`) landed in the same change as this
/// one, for the reason stated there: `private_interfaces` is a rustc WARN and the
/// workspace declares only `[workspace.lints.clippy]`, so half a widening breaks the
/// consumer quietly instead of failing the build.
#[derive(Debug, Clone)]
pub struct TargetRoute {
    backend: ApiBackend,
    boundary: Boundary,
    admission: SearchAdmission,
}

impl TargetRoute {
    /// Assemble the tuple from ONE target row (apex-waj.35). Every argument must come from
    /// the same row: `backend` is that row's own API backend and MUST NOT be
    /// [`crate::catalog_wire::infer_api_backend`], which answers `ChatCompletions` for an
    /// Anthropic slug a `/messages` row is served on and would silently demote the D2 cell
    /// to D3; `boundary` is [`model_boundary_class`] of that same row; `admission` is that
    /// row's flag folded with its declared surface by [`SearchAdmission::for_row`] — the only
    /// producer of the surface half. `xai-grok-shell`'s switch site is the one production
    /// caller.
    pub fn new(backend: ApiBackend, boundary: Boundary, admission: SearchAdmission) -> Self {
        Self {
            backend,
            boundary,
            admission,
        }
    }

    /// The API the switch lands on — authoritative for the D2-vs-D0/D1 choice.
    pub fn backend(&self) -> ApiBackend {
        self.backend.clone()
    }

    /// The boundary regime the target row is.
    pub fn boundary(&self) -> Boundary {
        self.boundary
    }

    /// §4.2 rule 3's gate, carried as the two signals it folds rather than as a bool a
    /// caller could pre-fold wrong.
    ///
    /// STALE BY CONSTRUCTION, and the arms must read it that way: the tuple is assembled once, at
    /// the switch, so `has_searchable_tools` here is the session's declared surface AT THAT MOMENT
    /// (the shell's tool-bridge snapshot). An MCP server that connects or disconnects later, or a
    /// tool-preset change, leaves this value pointing at the old surface — nothing recomputes it, and
    /// the tuple has no request-scoped twin. The D0/D3 arms on apex-waj.2 may therefore use it only
    /// as a statement about the route the switch took, never as "this request can find a tool"; the
    /// request-side producer recomputes it from `ConversationRequest::tools` through
    /// [`SearchAdmission::for_row`], whose freshness clause names the same split.
    ///
    /// One systematic value that is NOT a statement about the session's tools either: on a row whose
    /// `supports_search_tool` is `false` the shell passes [`SearchAdmission::for_row`] an empty
    /// surface by design — the tool-bridge clone it would need cannot change `admitted()`, which
    /// ANDs the flag anyway — so `has_searchable_tools` on a declining row is a cost-avoidance
    /// placeholder, always `false`, whatever that session had declared. The binding that makes it
    /// so is `declared_tools` in `xai-grok-shell`'s `handle_set_session_model`, which is empty
    /// unless the row admits; `target_route_from_row` itself does no such filtering, which is why
    /// its own test (`switch_route_carries_the_rows_admission`) reads `has_searchable_tools == true`
    /// for a declining row over a non-empty surface. A tier arm may therefore key on this field
    /// only together with `supports_search_tool`; on its own it answers "was the row admitted at
    /// the switch", never "does this session have something to find".
    pub fn admission(&self) -> SearchAdmission {
        self.admission
    }
}

/// Where the discovery record came from: the boundary that minted it, and the
/// row that owns it. `owner_model_id` is `None` when attribution is
/// unresolvable (mirrors [`forward_owner_model`]); that is never a KEEP.
#[derive(Debug, Clone)]
#[allow(dead_code)] // pending apex-waj.2 (the projector arms that read the tuple) + .21 / .5 / .11; the route tuple itself landed with apex-waj.35
pub(super) struct DiscoveryOrigin {
    pub boundary: Boundary,
    pub owner_model_id: Option<String>,
}

/// Choose the discovery tier for a switch onto `target` (xwire-boundary-map.md
/// §4.1).
///
/// The §4.1 table fixes the four TIERS and their actions; its `when` column is
/// a per-row description and states NO order. The sequence below is this
/// lane's resolution of the cells the table leaves ambiguous, and four of its
/// cells contradict the table's literal wording. Two are CONTESTED and escalated
/// as ruling **R-1** on bead apex-waj.29; the third is recorded below and R-1,
/// as filed, does not reach it; the fourth is step 4's guard over D1, recorded
/// there and being added to the same R-1 family by the coordinator:
///
/// - Step 1 against the D3 row, whose `when` is "target `ApiBackend ==
///   ChatCompletions`, or any route not admitted for native discovery". Read
///   literally that sends an unadmitted **Messages** route to D3; step 1 sends
///   it to D2. If the controller rules the wording literal,
///   `d2_materialises_on_an_unadmitted_messages_target` flips to
///   `DiscoveryTier::Demote`.
/// - Step 3 against the D0 row, whose `when` is "`ApiBackend == Responses` AND
///   `origin == target Boundary` AND the owner row is unchanged". That is
///   literally satisfied by the `Responses x Vertex` cell step 3 demotes. If
///   that wording rules, `responses_vertex_route_disagreement_demotes` flips to
///   `DiscoveryTier::Keep`.
/// - Step 1 again, against the D2 row, whose `when` is "target `Boundary ==
///   Vertex` (`ApiBackend == Messages`)" — literally a conjunction, so a
///   `Messages` target on a non-`Vertex` boundary matches NO row at all: not
///   D0/D1 (`ApiBackend == Responses`), not D2 (`Boundary == Vertex`), not D3
///   (`ChatCompletions`, or unadmitted). Step 1 materialises it anyway, off
///   §4.2's D2 preamble, which names the tier by the target wire
///   (`xwire-boundary-map.md:279`) and grounds it in that wire's mandatory
///   declaration surface (`:282`, A-23).
///   `messages_target_on_a_non_vertex_boundary_still_materialises` pins the
///   shipped answer; R-1 was filed over the two cells above only, so this one is
///   contested and, as of this pass, not yet escalated.
///
/// Every arm is reachable:
///
/// 1. `Messages` target -> [`DiscoveryTier::Materialise`] (D2), whatever the
///    origin or the gate says: D2 is a property of that wire, whose declaration
///    surface is mandatory (§4.2's D2 preamble, `xwire-boundary-map.md:282`,
///    A-23 — wire authority, not route choice). Rules 6 and 7 are not that
///    sentence: they are the two halves the arm must then produce, 6 the
///    transcript (`:284`) and 7 the declaration (`:289`).
/// 2. `ChatCompletions` target -> [`DiscoveryTier::Demote`] (D3) — that wire has
///    neither a typed item nor a declaration surface.
/// 3. `Responses` target on a `Vertex` boundary -> [`DiscoveryTier::Demote`]:
///    the `Responses x Vertex` cell is the fail-closed case — a route claiming
///    a Vertex row on the Responses wire is internally inconsistent, so nothing
///    is kept. Note the consequence in the other direction: a `Responses`
///    target never reaches D2, because D2 emits declarations into `tools[]` and
///    §4.2 rule 2 forbids a loaded definition entering `tools[]` there.
/// 4. `Responses` target whose row is not admitted for native discovery ->
///    [`DiscoveryTier::Demote`], as a guard over BOTH Responses tiers. §4.2
///    rule 3 (`xwire-boundary-map.md:261-263`) is where the gate comes from,
///    but it reaches only part of this arm's range: verbatim it redirects D0 —
///    "If the target row is not admitted, the projector **MUST** take D3,
///    **not D0**" — so a cell that never satisfied D0's `when` in the first
///    place (owner row changed, boundary changed, or owner unresolvable) says
///    nothing about it. Those cells are exactly the ones §4.1's D1 `when`
///    claims, and that row carries NO admission clause at all ("target
///    `ApiBackend == Responses`, boundary or row changed", `:247`). The two
///    rows therefore COLLIDE for them: read literally, §4.1 says Re-Key and
///    the shipped code says Demote. The warrant for shipping Demote is the D3
///    `when` column itself — "target `ApiBackend == ChatCompletions`, **or any
///    route not admitted** for native discovery" (`:249`), which excepts no
///    tier — read as a guard over D0/D1 rather than as a fourth alternative to
///    them. THAT PRECEDENCE IS THIS LANE'S READING of two colliding rows, not
///    something §4.1 states: the `when` column states no order (preamble
///    above), so this is a FOURTH colliding cell, and the coordinator is adding
///    it to the R-1 family on bead apex-waj.29 — which as filed reaches the two
///    cells named above and logs the `Messages` x non-`Vertex` cell only as a
///    note. If R-1 is ruled the other way — every `when` literal, so D1 owns
///    these cells — the changed-row / changed-boundary / unattributable legs of
///    [`unadmitted_responses_route_demotes_whatever_the_origin_says`] flip from
///    `Demote` to `ReKey`. Rule 3 holds either way, because it only ever spoke
///    about D0.
///
/// Steps 3 and 4 share one D3 arm: the same outcome for two independent
/// reasons, neither reachable from the other —
/// `responses_vertex_route_disagreement_demotes` pins the route-disagreement
/// reason on an admitted row, `unadmitted_responses_target_demotes_not_keeps`
/// pins the admission reason on a non-`Vertex` boundary whose origin is
/// KEEP-shaped, and
/// `unadmitted_responses_route_demotes_whatever_the_origin_says` pins it on the
/// three origins that are not — the cells step 4 and §4.1's D1 row collide over.
/// The code tests both reasons in one condition; the order of the two operands
/// is not a claim any test can observe, and none is made.
///
/// 5. same owner row AND same boundary -> [`DiscoveryTier::Keep`] (D0).
/// 6. otherwise (row changed, boundary changed, or owner unresolvable) ->
///    [`DiscoveryTier::ReKey`] (D1).
///
/// The keying reads `backend`, `boundary` and `admission` only. It never
/// consults `is_family_switch` (§9 item 3 — too narrow, it is false for
/// family-unset rows), and there is no parameter through which it could.
#[allow(dead_code)] // pending apex-waj.2 (the projector arms that read the tuple) + .21 / .5 / .11; the route tuple itself landed with apex-waj.35
pub(super) fn discovery_tier(
    target_model_id: &str,
    target: &TargetRoute,
    origin: &DiscoveryOrigin,
) -> DiscoveryTier {
    match target.backend {
        ApiBackend::Messages => DiscoveryTier::Materialise,
        ApiBackend::ChatCompletions => DiscoveryTier::Demote,
        ApiBackend::Responses => {
            // Two independent fail-closed reasons share the D3 arm, both ahead
            // of the KEEP test: the `Responses x Vertex` route disagreement (a
            // route that contradicts itself has no admission story to consult)
            // and §4.2 rule 3's unadmitted row. Operand order carries no
            // meaning and is unobservable; each reason has its own test.
            if target.boundary == Boundary::Vertex || !target.admission.admitted() {
                DiscoveryTier::Demote
            } else if origin.owner_model_id.as_deref() == Some(target_model_id)
                && origin.boundary == target.boundary
            {
                DiscoveryTier::Keep
            } else {
                DiscoveryTier::ReKey
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::has_searchable_tools;

    /// Storage-form history exercising both Vertex drop arms: index 1 is a
    /// non-carrier backend call (T3 drop), index 2 its result (pair-atomic
    /// co-drop), indices 3/4 an assistant call and its result (must survive
    /// unrecorded), index 5 an orphaned result (co-drop class).
    const DROP_SHAPE: &str = r#"[
        {"type":"user","content":[{"type":"text","text":"drop accounting probe"}]},
        {"type":"backend_tool_call","kind":{"tool_type":"x_search","id":"fc_b","call_id":"fc_b","name":"x_search","input":"q"}},
        {"type":"tool_result","tool_call_id":"fc_b","content":"backend result","is_error":false},
        {"type":"assistant","content":"calling a client tool","tool_calls":[{"id":"tc_a","name":"read_file","arguments":"{}"}],"model_id":"gpt-5.6-sol"},
        {"type":"tool_result","tool_call_id":"tc_a","content":"paired result","is_error":false},
        {"type":"tool_result","tool_call_id":"fc_orphan","content":"orphan result","is_error":false}
    ]"#;

    fn drop_shape() -> Vec<ConversationItem> {
        items_from(DROP_SHAPE)
    }

    /// Synthetic x71 Table-2 case-1 shape (4 records: user, non-carrier
    /// `x_search` backend call, its result, assistant) — owned by bead
    /// **apex-ayl.71**, same directory `projection_tests.rs:38` includes from.
    /// `fixtures/projection_x71/PROVENANCE.md:49` records this file as
    /// "synthetic — Table 2 case 1" and `:52` says "**NOT a corpus mirror**";
    /// `:60` records that `fc_x` is a synthetic id with no wire material, and
    /// the PROVENANCE sha256:12 table (`:15-20`) has rows only for the
    /// `vxm_az_*` / `az_vlq_*` mirrors — there is no sha pin for this fixture.
    /// Its sha256:12 at this commit is `e5515bf853a8`.
    const ORPHAN_SHAPE: &str = include_str!("fixtures/projection_x71/orphan_shape.json");

    /// Whole-list comparison through a serde round-trip, so it asserts VALUE
    /// identity: key order, number formatting and string escaping are invisible
    /// here. That is the standard that file states for itself — "this is the
    /// corpus's own stated verification standard (“parsed-equal”), not a byte
    /// comparison" (`projection_tests.rs:403-404`; `as_value` at `projection_tests.rs:582`).
    fn item_values(items: &[ConversationItem]) -> serde_json::Value {
        serde_json::to_value(items).expect("ConversationItem must serialize")
    }

    /// Rule 13's “never a silent skip” pinned on ONE shape: the three removals
    /// `DROP_SHAPE` produces are all recorded, each with the source index of the
    /// record its arm removed and the reason that names the class of that record,
    /// and the three survivors come back with equal JSON values.
    ///
    /// Scope, stated so the name is not read as more than the shape supports. It
    /// is one fixture, not the class:
    ///   * it has three records, so it says nothing about a fourth removal or
    ///     about ledger length — `vertex_ledger_of_four_removals_is_complete_ordered_and_typed`
    ///     owns that;
    ///   * its only non-carrier backend call is `x_search`, so it cannot tell one
    ///     non-carrier class from another — `vertex_target_drops_a_web_search_call_and_records_it`
    ///     owns `WebSearch`, `projection_tests.rs` `xw_proj_carrier_survival` owns
    ///     the `CodexRawInput` carrier side;
    ///   * every assistant there holds one tool call, so it cannot see pairing
    ///     against a later call in the same item —
    ///     `vertex_pairs_a_result_with_any_call_of_one_assistant_item` owns that.
    /// “In source order” is observed here only weakly: this ledger already happens
    /// to sit in reason order, so it cannot distinguish source order from reason
    /// order. `drop_ledger_records_source_order_not_reason_order` is the shape
    /// built for that distinction.
    #[test]
    fn vertex_target_records_both_drop_reasons() {
        let items = drop_shape();
        let projected = project_switch_history(
            &items,
            "claude-sonnet-5",
            Boundary::Vertex,
            None,
            &inert_route(Boundary::Vertex),
        );
        assert_eq!(
            projected.drops,
            vec![
                ProjectionDrop {
                    index: 1,
                    reason: DropReason::VertexNonCarrierBackendCall,
                },
                ProjectionDrop {
                    index: 2,
                    reason: DropReason::VertexUnpairedToolResult,
                },
                ProjectionDrop {
                    index: 5,
                    reason: DropReason::VertexUnpairedToolResult,
                },
            ],
            "Vertex drops must be recorded in source order with the right reason"
        );
        assert_eq!(
            projected
                .drops
                .iter()
                .map(|drop| drop.reason.as_str())
                .collect::<Vec<_>>(),
            [
                "vertex_non_carrier_backend_call",
                "vertex_unpaired_tool_result",
                "vertex_unpaired_tool_result",
            ],
            "each recorded drop must render under the label of the arm that removed it"
        );
        let survivors = vec![items[0].clone(), items[3].clone(), items[4].clone()];
        assert_eq!(item_values(&projected.items), item_values(&survivors));
    }

    /// A `WebSearch` backend call — the non-carrier class neither `DROP_SHAPE`
    /// nor the x71 case-1 shape exercises (both carry `x_search`). One call
    /// between the two portable records that must survive it.
    const WEB_SEARCH_SHAPE: &str = r#"[
        {"type":"user","content":[{"type":"text","text":"web search probe"}]},
        {"type":"backend_tool_call","kind":{"tool_type":"web_search","id":"ws_b","status":"completed","action":{"type":"search","query":"carrier exemption set"}}},
        {"type":"assistant","content":"answer","model_id":"gpt-5.6-sol"}
    ]"#;

    /// `is_carrier` exempts EXACTLY `CodexRawInput`: a `WebSearch` call IS a
    /// non-carrier, so a Vertex target must remove it AND record it, and a
    /// non-Vertex target must leave it untouched.
    ///
    /// Nothing pinned either half before this test. With every in-file shape on
    /// `x_search`, widening the exemption to
    /// `BackendToolKind::CodexRawInput(_) | BackendToolKind::WebSearch(_)`
    /// (harness mutant `S-CARRIER-WEB`) kept the item on a Vertex target and
    /// deleted its ledger record, and all 792 library tests stayed green in
    /// both profiles. This is the other side of the F-1 guard: the arm that
    /// spares a carrier must strip and account for everything else, and the
    /// exemption set is a real assertion, not a default — whatever class
    /// apex-waj.21 eventually lands discovery as, changing it is a decision a
    /// test has to be edited for.
    #[test]
    fn vertex_target_drops_a_web_search_call_and_records_it() {
        let items = items_from(WEB_SEARCH_SHAPE);
        let projected = project_switch_history(
            &items,
            "claude-sonnet-5",
            Boundary::Vertex,
            None,
            &inert_route(Boundary::Vertex),
        );
        assert_eq!(
            projected.drops,
            vec![ProjectionDrop {
                index: 1,
                reason: DropReason::VertexNonCarrierBackendCall,
            }],
            "a WebSearch call is not a CodexRawInput carrier: the T3 arm must remove it \
             and record it — a carrier class widened to `WebSearch` removes nothing and \
             leaves this ledger empty"
        );
        assert_eq!(
            projected
                .drops
                .iter()
                .map(|drop| drop.reason.as_str())
                .collect::<Vec<_>>(),
            ["vertex_non_carrier_backend_call"],
            "the WebSearch removal must render under the T3 non-carrier label"
        );
        assert_eq!(
            item_values(&projected.items),
            item_values(&[items[0].clone(), items[2].clone()]),
            "the WebSearch record must be the only removal; both neighbours survive unchanged"
        );
        // The boundary axis of the same arm: off Vertex the call is kept and
        // nothing is recorded, so the Vertex asserts above cannot be satisfied
        // by a projector that strips backend calls on every boundary.
        for boundary in [Boundary::AzStrict, Boundary::VLLenient] {
            let kept = project_switch_history(
                &items,
                "gpt-5.6-terra",
                boundary,
                None,
                &inert_route(boundary),
            );
            assert_eq!(
                kept.drops,
                Vec::new(),
                "{boundary:?} target drops a WebSearch call it must keep"
            );
            assert_eq!(item_values(&kept.items), item_values(&items));
        }
    }

    /// The same two arms reached in the order `DropReason` does NOT declare
    /// them: a pre-existing orphan result (index 1) fires the pair-atomic
    /// co-drop BEFORE the T3 non-carrier call (index 3) fires its own arm, so
    /// the ledger is `[UR@1, NC@3, UR@4]` — out of reason order, since
    /// [`DropReason::VertexNonCarrierBackendCall`] is declared first.
    /// `DROP_SHAPE` above cannot see this: its ledger `[NC@1, UR@2, UR@5]`
    /// already happens to sit in reason order, so ordering the ledger by
    /// `reason` is invisible there.
    const OUT_OF_REASON_ORDER_SHAPE: &str = r#"[
        {"type":"user","content":[{"type":"text","text":"source-order probe"}]},
        {"type":"tool_result","tool_call_id":"fc_orphan_first","content":"orphan result","is_error":false},
        {"type":"user","content":[{"type":"text","text":"second turn"}]},
        {"type":"backend_tool_call","kind":{"tool_type":"x_search","id":"fc_b","call_id":"fc_b","name":"x_search","input":"q"}},
        {"type":"tool_result","tool_call_id":"fc_b","content":"backend result","is_error":false}
    ]"#;

    /// Source order is an ASSERTED property of the ledger (§4.2 rule 13 by
    /// extension names it: "the removed item's SOURCE index"), not a
    /// coincidence of the shapes tried so far. `DROP_SHAPE`'s ledger is already
    /// in reason order, so a projector that sorted the ledger by `reason`
    /// passed every other test here; this shape is the one that cannot be
    /// reordered without changing the vector.
    #[test]
    fn drop_ledger_records_source_order_not_reason_order() {
        let items = items_from(OUT_OF_REASON_ORDER_SHAPE);
        let projected = project_switch_history(
            &items,
            "claude-sonnet-5",
            Boundary::Vertex,
            None,
            &inert_route(Boundary::Vertex),
        );
        assert_eq!(
            projected.drops,
            vec![
                ProjectionDrop {
                    index: 1,
                    reason: DropReason::VertexUnpairedToolResult,
                },
                ProjectionDrop {
                    index: 3,
                    reason: DropReason::VertexNonCarrierBackendCall,
                },
                ProjectionDrop {
                    index: 4,
                    reason: DropReason::VertexUnpairedToolResult,
                },
            ],
            "the ledger must follow source order: the co-drop at index 1 precedes \
             the T3 non-carrier drop at index 3, and sorting by `reason` would move \
             index 3 to the front because `VertexNonCarrierBackendCall` is declared first"
        );
        // No index-only restatement of that vector and no partition count here:
        // each follows from the exact ledger above together with the exact
        // survivor list below, so neither could ever be the assertion that fails
        // (the mutation seat's power audit counted the index projection among the
        // two inert asserts in this module). Those two goldens are what enforce
        // the construction's partition claim on this shape.
        let survivors = vec![items[0].clone(), items[2].clone()];
        assert_eq!(item_values(&projected.items), item_values(&survivors));
    }

    /// Responses-target switches remove nothing, so they record nothing.
    #[test]
    fn responses_targets_record_no_drops() {
        let items = drop_shape();
        for boundary in [Boundary::AzStrict, Boundary::VLLenient] {
            let projected = project_switch_history(
                &items,
                "gpt-5.6-terra",
                boundary,
                None,
                &inert_route(boundary),
            );
            assert_eq!(
                projected.drops,
                Vec::new(),
                "{boundary:?} target drops nothing"
            );
            assert_eq!(item_values(&projected.items), item_values(&items));
        }
    }

    /// XD-6 idempotence on the accounting: an already-projected history
    /// projected again to the same target records no further drops and
    /// changes no item.
    #[test]
    fn vertex_projection_records_no_further_drops() {
        let first = project_switch_history(
            &drop_shape(),
            "claude-sonnet-5",
            Boundary::Vertex,
            None,
            &inert_route(Boundary::Vertex),
        );
        let second = project_switch_history(
            &first.items,
            "claude-sonnet-5",
            Boundary::Vertex,
            None,
            &inert_route(Boundary::Vertex),
        );
        assert_eq!(second.drops, Vec::new());
        assert_eq!(item_values(&second.items), item_values(&first.items));
    }

    /// The ledger asserted over the synthetic x71 Table-2 case-1 shape:
    /// projecting it to Vertex records exactly the non-carrier backend call
    /// (index 1) and its co-dropped result (index 2), leaves the user and
    /// assistant records unchanged, and the two vectors partition the input.
    /// `fixtures/projection_x71/PROVENANCE.md:49` records this fixture as
    /// "synthetic — Table 2 case 1" and `:52` states "**NOT a corpus mirror**";
    /// there is no sha pin for it in that file's mirror table (`:15-20`), and
    /// its own sha256:12 at this commit is `e5515bf853a8`. The file is owned by
    /// bead **apex-ayl.71**. What this test earns is the orphan + paired-
    /// survivor index contrast, which no corpus mirror in that directory
    /// provides: both sha-pinned `*_pre.json` mirrors (`vxm_az_pre.json` 36
    /// recs, `az_vlq_pre.json` 53 recs) hold zero `backend_tool_call` records
    /// and every one of their tool_results pairs with an ASSISTANT call, so
    /// projected to Vertex their ledger is empty (probe output in the round-3
    /// report §2). The two goldens below — the complete ledger and the complete
    /// survivor list — are what enforce the construction's partition claim on
    /// this shape: `project_switch_history`'s `debug_assert_eq!` restates a
    /// property those two already entail, so no separate count assert is made
    /// here (a count could only fail if one of the goldens had already failed;
    /// the mutation seat's power audit counted it inert).
    #[test]
    fn vertex_drops_over_the_x71_case1_shape() {
        let items = items_from(ORPHAN_SHAPE);
        let projected = project_switch_history(
            &items,
            "claude-sonnet-5",
            Boundary::Vertex,
            None,
            &inert_route(Boundary::Vertex),
        );
        assert_eq!(
            projected.drops,
            vec![
                ProjectionDrop {
                    index: 1,
                    reason: DropReason::VertexNonCarrierBackendCall,
                },
                ProjectionDrop {
                    index: 2,
                    reason: DropReason::VertexUnpairedToolResult,
                },
            ],
            "the case-1 shape's backend call and its result must both be recorded"
        );
        assert_eq!(
            item_values(&projected.items),
            item_values(&[items[0].clone(), items[3].clone()]),
            "the surviving case-1 records must come back unchanged"
        );
    }

    /// A ledger longer than three records: two non-carrier backend calls of
    /// **different classes** and the two results that ride with them, interleaved
    /// so the complete ordered ledger is `[NC@1, UR@2, NC@3, UR@4]`.
    const FOUR_DROP_SHAPE: &str = r#"[
        {"type":"user","content":[{"type":"text","text":"four-record ledger probe"}]},
        {"type":"backend_tool_call","kind":{"tool_type":"x_search","id":"fc_b","call_id":"fc_b","name":"x_search","input":"q"}},
        {"type":"tool_result","tool_call_id":"fc_b","content":"backend result","is_error":false},
        {"type":"backend_tool_call","kind":{"tool_type":"web_search","id":"ws_c","status":"completed","action":{"type":"search","query":"fourth record"}}},
        {"type":"tool_result","tool_call_id":"ws_c","content":"web result","is_error":false}
    ]"#;

    /// Rule 13's accounting over a FOUR-record ledger. Every shape the suite
    /// carried when the mutation seat ran has at most three removals, which left
    /// the fourth record of any ledger free to be truncated, re-labelled or
    /// reordered — three independent defects, each of which passed 795/795 in the
    /// GATE profile:
    ///   * `S-LEDGER-CAP` (`drops.truncate(3)` before the partition assert) — the
    ///     fourth removal vanishes from the ledger while its record stays gone, so
    ///     the input is no longer accounted for at all;
    ///   * `S-LEDGER-REASON4` (the fourth record's reason overwritten) — the
    ///     wrong-reason falsifiers before this test all sit at index 0..2 of a
    ///     three-record ledger, so the fourth record's label was never observed;
    ///   * `S-LEDGER-ORDER4` (`drops.reverse()` when `len() > 3`) — `K05`'
    ///     unconditional `reverse()` was killed because it also breaks the
    ///     three-record ledgers; a conditional one was invisible.
    /// The `x_search`/`web_search` alternation is load-bearing, not decoration:
    /// `[NC, UR, NC, UR]` is its own reversal and is neither reason-sorted nor
    /// truncatable at three, so all three mutants change the asserted vector.
    /// The reason labels are asserted twice on purpose — as typed values and as
    /// the stable `as_str()` labels — so a label rename cannot hide a reason swap.
    #[test]
    fn vertex_ledger_of_four_removals_is_complete_ordered_and_typed() {
        let items = items_from(FOUR_DROP_SHAPE);
        let projected = project_switch_history(
            &items,
            "claude-sonnet-5",
            Boundary::Vertex,
            None,
            &inert_route(Boundary::Vertex),
        );
        assert_eq!(
            projected.drops,
            vec![
                ProjectionDrop {
                    index: 1,
                    reason: DropReason::VertexNonCarrierBackendCall,
                },
                ProjectionDrop {
                    index: 2,
                    reason: DropReason::VertexUnpairedToolResult,
                },
                ProjectionDrop {
                    index: 3,
                    reason: DropReason::VertexNonCarrierBackendCall,
                },
                ProjectionDrop {
                    index: 4,
                    reason: DropReason::VertexUnpairedToolResult,
                },
            ],
            "the fourth removal is part of the ledger too: it must be present, keep \
             its own reason, and sit in source order between the third and the end"
        );
        assert_eq!(
            projected
                .drops
                .iter()
                .map(|drop| drop.reason.as_str())
                .collect::<Vec<_>>(),
            [
                "vertex_non_carrier_backend_call",
                "vertex_unpaired_tool_result",
                "vertex_non_carrier_backend_call",
                "vertex_unpaired_tool_result",
            ],
            "each of the four records must render under the label of the arm that removed it"
        );
        let survivors = vec![items[0].clone()];
        assert_eq!(
            item_values(&projected.items),
            item_values(&survivors),
            "the four removals must be the only change to this shape"
        );
    }

    /// One assistant item carrying TWO client tool calls, with both results
    /// stored, and a second shape where only the FIRST result is stored.
    const TWO_CALLS_TWO_RESULTS: &str = r#"[
        {"type":"user","content":[{"type":"text","text":"two calls, two answers"}]},
        {"type":"assistant","content":"both at once","tool_calls":[{"id":"tc_first","name":"read_file","arguments":"{\"path\":\"a\"}"},{"id":"tc_second","name":"read_file","arguments":"{\"path\":\"b\"}"}],"model_id":"gpt-5.6-sol"},
        {"type":"tool_result","tool_call_id":"tc_first","content":"first body","is_error":false},
        {"type":"tool_result","tool_call_id":"tc_second","content":"second body","is_error":false}
    ]"#;

    const TWO_CALLS_FIRST_RESULT_ONLY: &str = r#"[
        {"type":"user","content":[{"type":"text","text":"two calls, one answer"}]},
        {"type":"assistant","content":"both at once","tool_calls":[{"id":"tc_first","name":"read_file","arguments":"{\"path\":\"a\"}"},{"id":"tc_second","name":"read_file","arguments":"{\"path\":\"b\"}"}],"model_id":"gpt-5.6-sol"},
        {"type":"tool_result","tool_call_id":"tc_first","content":"first body","is_error":false}
    ]"#;

    /// Pairing at the Vertex pre-pass is **set membership over every call id of
    /// the item**, not a match against the item's first (or last) call. Every
    /// assistant in this module's material holds at most one `tool_calls` entry,
    /// so `for tc in &a.tool_calls` and `for tc in a.tool_calls.iter().take(1)`
    /// were the same program: `S-PAIR-SECOND` survived 795/795 in the GATE
    /// profile, silently dropping the result of a second call as an orphan and
    /// charging it to the ledger. Two shapes, because one position cannot tell
    /// `take(1)` from `skip(1)` — the second-answer shape fails the first, the
    /// first-answer-only shape fails the second.
    ///
    /// Both halves record nothing: an assistant call whose result was never
    /// stored is not an orphan this seam removes (that is the deferred assistant
    /// direction documented at `project_switch_history`'s comment, owned by the
    /// follow-on bead), so the ledger staying empty is part of what is pinned,
    /// not an assumption.
    #[test]
    fn vertex_pairs_a_result_with_any_call_of_one_assistant_item() {
        for (shape, name) in [
            (TWO_CALLS_TWO_RESULTS, "both results stored"),
            (TWO_CALLS_FIRST_RESULT_ONLY, "first result stored only"),
        ] {
            let items = items_from(shape);
            let projected = project_switch_history(
                &items,
                "gpt-5.6-sol",
                Boundary::Vertex,
                None,
                &inert_route(Boundary::Vertex),
            );
            assert_eq!(
                projected.drops,
                Vec::new(),
                "{name}: a result paired with ANY call of its assistant item is \
                 not an orphan — the pairing set must hold every call id of the item"
            );
            assert_eq!(
                item_values(&projected.items),
                item_values(&items),
                "{name}: the Vertex pre-pass must not rewrite a multi-call assistant \
                 round it has no reason to touch"
            );
        }
    }

    /// Backward-compatibility fence over both shapes above: the ledger arm and the
    /// pairing rule are **boundary-scoped**, so the same inputs aimed at a deployed
    /// `AzStrict` / `VLLenient` row come back record-for-record unchanged with an
    /// empty ledger. What a switch used to leave alone must not start disappearing
    /// because the Vertex accounting grew a new shape or a new arm.
    ///
    /// The same claim over `DROP_SHAPE` is pinned by
    /// `responses_targets_record_no_drops` and over the web-search shape by
    /// `vertex_target_drops_a_web_search_call_and_records_it`; this extends the
    /// fence to a four-removal ledger and a two-call assistant, which is where the
    /// next Vertex arm will be written. Harness mutant `M19` (the pair-atomic
    /// co-drop's `boundary == Boundary::Vertex` guard deleted, so every
    /// `tool_result` on every boundary is charged to the Vertex rule) is the defect
    /// class this guards — it was already killed by the two tests named above, so
    /// what this adds is a witness on the newer shapes, not the first one.
    #[test]
    fn non_vertex_targets_leave_multi_call_and_multi_drop_rounds_untouched() {
        for (shape, name) in [
            (FOUR_DROP_SHAPE, "four-removal ledger"),
            (TWO_CALLS_TWO_RESULTS, "two calls, two results"),
            (TWO_CALLS_FIRST_RESULT_ONLY, "two calls, first result only"),
        ] {
            let items = items_from(shape);
            for boundary in [Boundary::AzStrict, Boundary::VLLenient] {
                let projected = project_switch_history(
                    &items,
                    "gpt-5.6-terra",
                    boundary,
                    None,
                    &inert_route(boundary),
                );
                assert_eq!(
                    projected.drops,
                    Vec::new(),
                    "{name} at {boundary:?}: only a Vertex target has an arm that \
                     removes anything, so nothing may be recorded here"
                );
                assert_eq!(
                    item_values(&projected.items),
                    item_values(&items),
                    "{name} at {boundary:?}: a non-Vertex switch must pass every \
                     record through untouched"
                );
            }
        }
    }

    // ---- discovery tier decision (xwire-boundary-map.md §4.1 ladder) ----

    /// Both §4.2 rule 3 signals hold: the row accepts the hosted `tool_search`
    /// declaration and the discovery manifest is non-empty.
    const ADMITTED: SearchAdmission = SearchAdmission {
        supports_search_tool: true,
        has_searchable_tools: true,
    };
    /// Rule 3's second signal missing — nothing searchable to serve.
    const NOTHING_SEARCHABLE: SearchAdmission = SearchAdmission {
        supports_search_tool: true,
        has_searchable_tools: false,
    };
    /// Rule 3's first signal missing — the row rejects the declaration (§5.3's
    /// XT-11 row).
    const ROW_REJECTS: SearchAdmission = SearchAdmission {
        supports_search_tool: false,
        has_searchable_tools: true,
    };
    /// Both of rule 3's signals missing. The fourth combination, and the only
    /// one the other three rows cannot distinguish: an equality fold of the two
    /// signals (`supports_search_tool == has_searchable_tools`, i.e. admitted
    /// when the two agree) reproduces the verdicts of the other three rows and
    /// differs from `&&` exactly here — where it would KEEP a loaded set on a
    /// route that has neither the declaration flag nor a manifest to serve.
    const NEITHER_SIGNAL: SearchAdmission = SearchAdmission {
        supports_search_tool: false,
        has_searchable_tools: false,
    };

    fn route(backend: ApiBackend, boundary: Boundary, admission: SearchAdmission) -> TargetRoute {
        TargetRoute::new(backend, boundary, admission)
    }

    fn origin(boundary: Boundary, owner_model_id: Option<&str>) -> DiscoveryOrigin {
        DiscoveryOrigin {
            boundary,
            owner_model_id: owner_model_id.map(str::to_owned),
        }
    }

    /// The route tuple built the way the production caller builds it (apex-waj.35): the row's
    /// `supports_search_tool`, the second signal from the ONE producer ruling D1 names, folded by
    /// [`SearchAdmission::admitted()`] inside the tier ladder. It is the ladder that proves the
    /// tuple is wired — an admission that never opened would make every Responses target a D3 and
    /// no test below would notice, because D3 is also the correct answer for three other cells.
    ///
    /// The pair of assertions is the whole point: the SAME row, boundary and origin, flipped only
    /// by the row's flag, moves KEEP -> DEMOTE.
    #[test]
    fn the_admission_producer_opens_the_admitted_tier_on_an_admitting_row() {
        let declared_tools = [crate::conversation::ToolSpec {
            name: "read_file".to_string(),
            description: None,
            parameters: serde_json::json!({"type": "object"}),
            exposure: crate::conversation::ToolExposure::default(),
        }];
        let owner_row = "gpt-5.6-terra";
        let from = origin(Boundary::AzStrict, Some(owner_row));

        // An operator-admitted row over a non-empty declared surface takes D0, not the D3 floor.
        let admitted =
            SearchAdmission::for_row(/* supports_search_tool */ true, &declared_tools);
        let route = TargetRoute::new(ApiBackend::Responses, Boundary::AzStrict, admitted);
        assert!(
            admitted.admitted(),
            "a non-empty declared surface is searchable, so the row's flag decides here"
        );
        assert_eq!(
            discovery_tier(owner_row, &route, &from),
            DiscoveryTier::Keep,
            "the admitted route must reach D0"
        );

        // The same row with the operator's flag off is the fail-closed cell: nothing but the flag
        // differs, so a producer or a tuple that ignores it cannot pass.
        let unadmitted =
            SearchAdmission::for_row(/* supports_search_tool */ false, &declared_tools);
        let route = TargetRoute::new(ApiBackend::Responses, Boundary::AzStrict, unadmitted);
        assert_eq!(
            discovery_tier(owner_row, &route, &from),
            DiscoveryTier::Demote,
            "the row's flag alone must be able to close the route"
        );

        // The producer's other half: a request that declares no tool has nothing to search, so the
        // flag alone never opens the route. This one cell calls the producer directly rather than
        // going through `for_row`, because the producer's BODY is the subject here.
        let no_tools: [crate::conversation::ToolSpec; 0] = [];
        assert!(
            !SearchAdmission {
                supports_search_tool: true,
                has_searchable_tools: has_searchable_tools(&no_tools),
            }
            .admitted(),
            "a route with no declared tool surface is not searchable"
        );
    }

    /// D3: the Chat Completions wire has neither a typed discovery item nor a
    /// declaration surface, so it demotes even the input that is a D0 KEEP on
    /// the Responses wire (same row, same boundary, admitted).
    #[test]
    fn chat_completions_target_demotes() {
        let target = route(ApiBackend::ChatCompletions, Boundary::AzStrict, ADMITTED);
        let from = origin(Boundary::AzStrict, Some("gpt-5.6-sol"));
        assert_eq!(
            discovery_tier("gpt-5.6-sol", &target, &from),
            DiscoveryTier::Demote,
            "backend authority: the wire decides, not the unchanged row"
        );
    }

    /// Step 2 is unconditional, so it holds whatever the origin says. The test
    /// above pins only the KEEP-shaped cell (same row, same boundary), which is
    /// exactly the cell an origin-conditional exemption can survive: mutating the
    /// arm to
    /// `ChatCompletions if origin.owner_model_id.as_deref() != Some(target_model_id)
    /// => Keep` left 795/795 green (`S-CHAT-FOREIGN`), i.e. a foreign record could
    /// be KEEPed on a wire that has no place for it.
    ///
    /// Three origins, mirroring
    /// [`unadmitted_responses_route_demotes_whatever_the_origin_says`] on the other
    /// wire: changed owner row, changed boundary, and no attributable owner. The
    /// first and the third are the two the shipped defect flips; the middle one is
    /// the third axis a future exemption could key on and costs nothing to pin
    /// while the arm is one line. Admission is deliberately `ADMITTED` so nothing
    /// but the backend can be the reason for the demote.
    #[test]
    fn chat_completions_demotes_whatever_the_origin_says() {
        let target = route(ApiBackend::ChatCompletions, Boundary::AzStrict, ADMITTED);
        for (shape, model_id, from) in [
            (
                "same boundary, owner row changed",
                "gpt-5.6-sol",
                origin(Boundary::AzStrict, Some("gpt-5.6-terra")),
            ),
            (
                "same model id, boundary changed",
                "gpt-5.6-sol",
                origin(Boundary::VLLenient, Some("gpt-5.6-sol")),
            ),
            (
                "no attributable owner",
                "gpt-5.6-sol",
                origin(Boundary::AzStrict, None),
            ),
        ] {
            assert_eq!(
                discovery_tier(model_id, &target, &from),
                DiscoveryTier::Demote,
                "{shape}: D3 on the Chat Completions wire is keyed on the target \
                 backend alone — an origin-conditional arm would KEEP a record this \
                 wire cannot place"
            );
        }
    }

    /// D2 on the canonical route: a `/messages` target is a Vertex row, and the
    /// foreign Responses-minted record inverts into transcript + declarations.
    #[test]
    fn messages_target_materialises() {
        let target = route(ApiBackend::Messages, Boundary::Vertex, ADMITTED);
        let from = origin(Boundary::AzStrict, Some("gpt-5.6-sol"));
        assert_eq!(
            discovery_tier("claude-sonnet-5", &target, &from),
            DiscoveryTier::Materialise
        );
    }

    /// The `Messages` × non-`Vertex` cell — the THIRD override of §4.1's
    /// wording, and the one ruling R-1 as filed does not reach. §4.1's D2
    /// `when` reads "target `Boundary == Vertex` (`ApiBackend == Messages`)"
    /// (`xwire-boundary-map.md:248`): taken literally it needs BOTH fields, so
    /// a `/messages` target on a non-`Vertex` boundary matches no row at all —
    /// not D0/D1 (`ApiBackend == Responses`), not D2 (`Boundary == Vertex`),
    /// not D3 (`ChatCompletions`, or any unadmitted route). Step 1 of
    /// [`discovery_tier`] decides it anyway and sends it to D2, off §4.2's D2
    /// preamble, which names the tier by the TARGET WIRE
    /// ("**D2 — Messages target. The inversion.**", `xwire-boundary-map.md:279`)
    /// and grounds it in what is mandatory on that wire (`:282`, A-23) — a
    /// property of `/messages`, not of a boundary slug.
    ///
    /// CONTESTED, and pinned as SHIPPED BEHAVIOUR, not as spec: R-1
    /// (apex-waj.29) was filed over the other two cells only
    /// ([`d2_materialises_on_an_unadmitted_messages_target`] and
    /// [`responses_vertex_route_disagreement_demotes`]), so nothing yet rules
    /// this one either way. If R-1 is finally read as "the `when` column is
    /// literal wherever it can be", this cell's expectation is the one that
    /// falls, and the fail-closed answer for a route no row describes is
    /// [`DiscoveryTier::Demote`] (D3).
    ///
    /// The cell is unreachable-by-construction today — `TargetRoute` documents
    /// that both fields come from ONE target row, and no caller can build the
    /// tuple before the route tuple lands (apex-waj.35). That is not the same as
    /// pinned: unreachability says nobody can pass the input, this test says what
    /// the decision function answers when somebody does, which is the only claim
    /// a future caller can rely on.
    #[test]
    fn messages_target_on_a_non_vertex_boundary_still_materialises() {
        let from = origin(Boundary::AzStrict, Some("gpt-5.6-sol"));
        for boundary in [Boundary::AzStrict, Boundary::VLLenient] {
            let target = route(ApiBackend::Messages, boundary, ADMITTED);
            assert_eq!(
                discovery_tier("claude-sonnet-5", &target, &from),
                DiscoveryTier::Materialise,
                "shipped step 1 keys D2 on the wire, so a `/messages` target \
                 materialises whatever boundary it claims — contested cell, see doc"
            );
        }
    }

    /// The `Messages` arm's **admission** axis, sampled completely: all four
    /// combinations of the two rule 3 signals on each of the three boundaries it
    /// can be asked about. Before this test the arm had three samples across two
    /// tests — `ADMITTED` on `Vertex` ([`messages_target_materialises`]),
    /// `ADMITTED` on both non-`Vertex` boundaries
    /// ([`messages_target_on_a_non_vertex_boundary_still_materialises`]) and
    /// `ROW_REJECTS` = `(false, true)` on `Vertex`
    /// ([`d2_materialises_on_an_unadmitted_messages_target`]) — which left two
    /// shapes free. The mutation seat published one of them: an arm preceded by
    /// `Messages if !supports_search_tool && !has_searchable_tools => Demote`, the
    /// `(false, false)` cell, survived 795/795 in the GATE profile
    /// (`S-MESSAGES-FF`). Sampling the fourth combination alone would leave the
    /// fifth hole — `(true, false)`, the shape a `!has_searchable_tools`-keyed
    /// exemption picks out — so the whole axis is pinned here and the arm cannot
    /// be conditioned on either signal or on their fold.
    ///
    /// CONTESTED, pinned as SHIPPED BEHAVIOUR, exactly as
    /// [`messages_target_on_a_non_vertex_boundary_still_materialises`] pins its
    /// cell, and for the same reason: §4.1's D3 `when` reads an unadmitted route
    /// into D3 (“or **any route** not admitted for native discovery”,
    /// `xwire-boundary-map.md:249`) with no backend qualifier, while shipped step
    /// 1 returns D2 on the backend alone. That is the fourth cell of the R-1
    /// family on bead apex-waj.29 — the F-3 handoff (report §12.5) records the
    /// family as 2 filed + 2 noted and asks the controller to rule the `when`
    /// column once for all of them; if R-1 goes the literal wording's way, the
    /// nine unadmitted cells of this table flip from `Materialise` to `Demote` and
    /// the three admitted-boundary cells above do not. Rule 3 is satisfied either
    /// way: it only ever spoke about D0.
    ///
    /// The cell is unreachable-by-construction today for the same reason the
    /// boundary cell is — no caller can build a `TargetRoute` before the route
    /// tuple lands (apex-waj.35), and `discovery_tier` carries
    /// `#[allow(dead_code)]`. Unreachable is not unpinned: this says what the
    /// decision function answers when a caller does arrive.
    #[test]
    fn messages_target_materialises_under_every_admission_shape() {
        let from = origin(Boundary::AzStrict, Some("gpt-5.6-sol"));
        for boundary in [Boundary::Vertex, Boundary::AzStrict, Boundary::VLLenient] {
            for (shape, admission) in [
                ("supports + searchable", ADMITTED),
                ("supports, nothing searchable", NOTHING_SEARCHABLE),
                ("row rejects, searchable", ROW_REJECTS),
                ("neither signal", NEITHER_SIGNAL),
            ] {
                let target = route(ApiBackend::Messages, boundary, admission);
                assert_eq!(
                    discovery_tier("claude-sonnet-5", &target, &from),
                    DiscoveryTier::Materialise,
                    "{boundary:?} + {shape}: D2 is a property of the `/messages` \
                     wire, not of the Responses admission gate — contested cell, see doc"
                );
            }
        }
    }

    /// The `Responses x Vertex` cell — the fail-closed case: the route claims a
    /// Vertex row on the Responses wire, so the two fields disagree and nothing
    /// is kept, even though same row + same boundary + admitted is otherwise
    /// the D0 shape. A Responses target never reaches D2 in any cell, because
    /// D2 declares loaded definitions in `tools[]` and §4.2 rule 2 forbids that
    /// on this wire. CONTESTED cell: §4.1's D0 `when` is literally satisfied by
    /// this input, so this test is the one that flips to `Keep` if ruling R-1
    /// (apex-waj.29) reads that row literally.
    #[test]
    fn responses_vertex_route_disagreement_demotes() {
        let target = route(ApiBackend::Responses, Boundary::Vertex, ADMITTED);
        let from = origin(Boundary::Vertex, Some("claude-sonnet-5"));
        assert_eq!(
            discovery_tier("claude-sonnet-5", &target, &from),
            DiscoveryTier::Demote,
            "a route that contradicts itself must not KEEP a record it cannot place"
        );
    }

    /// §4.2 rule 3: a Responses row not admitted for native discovery must
    /// demote, never keep — otherwise the history claims a loaded set the route
    /// cannot serve. Reachable, not hypothetical: probe R3 answers 200 with no
    /// search item on `qwen3.8-27b` — note that `model_boundary_class` puts that
    /// slug on [`Boundary::VLLenient`] (`:341-347`), not on the `AzStrict` route
    /// this test pins, so this test evidences the strict row only
    /// (`ROW_REJECTS` = §5.3's XT-11 class) and the probe's own boundary is
    /// pinned by the `VLLenient` column of
    /// [`rule_three_fold_gates_the_keep_and_neither_signal_alone_does`].
    #[test]
    fn unadmitted_responses_target_demotes_not_keeps() {
        let target = route(ApiBackend::Responses, Boundary::AzStrict, ROW_REJECTS);
        let from = origin(Boundary::AzStrict, Some("gpt-5.6-sol"));
        assert_eq!(
            discovery_tier("gpt-5.6-sol", &target, &from),
            DiscoveryTier::Demote,
            "admission is a guard on D0, not a suggestion"
        );
    }

    /// Step 4's guard over D1, and the cell the F-3 MAJOR was about. The test
    /// above supplies a KEEP-shaped origin, so it is exactly the D0 cell §4.2
    /// rule 3 does reach ("**MUST** take D3, not D0",
    /// `xwire-boundary-map.md:261-263`). Here the origin is NOT KEEP-shaped, so
    /// rule 3 is silent and §4.1's D1 `when` — "target `ApiBackend ==
    /// Responses`, boundary or row changed" (`:247`) — claims the cell with no
    /// admission clause at all. D1 and D3 therefore COLLIDE over it: the table
    /// read literally says Re-Key, the shipped code says Demote. What ships
    /// Demote is the D3 `when` column (`:249`, "or **any route not admitted**
    /// for native discovery") read as a guard over D0/D1 — this lane's
    /// precedence over two colliding rows, not a reading §4.1 compels, and the
    /// fourth cell of the R-1 family on bead apex-waj.29 (see step 4 of
    /// [`discovery_tier`]'s doc for the flip target if R-1 goes the literal
    /// way). It is pinned as SHIPPED behaviour with its contested status
    /// stated, the same way the three already under R-1 are pinned.
    ///
    /// Three origin shapes, not one, because the colliding class is "every
    /// origin that is not KEEP-shaped" and each of its axes is separately
    /// exemptable inside the guard. `/private/tmp/px29_impl_r3/mutate.py`
    /// carries the three mutants that show each leg earns its place: `MUT-G`
    /// (gate moved below the KEEP test entirely — killed only by this test, and
    /// it survived all 19 assertions that existed before it), `MUT-G2` (gate
    /// exempts a changed boundary) and `MUT-G3` (gate exempts an unattributable
    /// owner). A single-shape cell leaves the other two alive.
    ///
    /// Every admission assertion that predates this one handed the selector a
    /// KEEP-shaped origin (`unadmitted_responses_target_demotes_not_keeps`, and
    /// the fold table's `origin(boundary, Some(model_id))` against a target of
    /// the same boundary and model), so all three shapes below were unsampled
    /// and the "admission guards the whole `Responses` wire" claim was asserted
    /// nowhere.
    #[test]
    fn unadmitted_responses_route_demotes_whatever_the_origin_says() {
        for (shape, boundary, model_id, from) in [
            (
                "same boundary, owner row changed",
                Boundary::AzStrict,
                "gpt-5.6-sol",
                origin(Boundary::AzStrict, Some("gpt-5.6-terra")),
            ),
            (
                "same model id, boundary changed",
                Boundary::VLLenient,
                "qwen3.8-27b",
                origin(Boundary::AzStrict, Some("qwen3.8-27b")),
            ),
            (
                "no attributable owner",
                Boundary::AzStrict,
                "gpt-5.6-sol",
                origin(Boundary::AzStrict, None),
            ),
        ] {
            for admission in [NOTHING_SEARCHABLE, ROW_REJECTS, NEITHER_SIGNAL] {
                let target = route(ApiBackend::Responses, boundary, admission);
                assert_eq!(
                    discovery_tier(model_id, &target, &from),
                    DiscoveryTier::Demote,
                    "{shape} + {admission:?}: the lane's precedence puts rule 3's \
                     gate above D1 as well as D0 (warrant: D3's `when`, \
                     xwire-boundary-map.md:249) — §4.1's D1 `when` has no \
                     admission clause, so this cell is contested under R-1"
                );
            }
        }
    }

    /// D0: the record stays exactly where it was minted — same owner row, same
    /// boundary, admitted.
    #[test]
    fn same_row_same_boundary_keeps() {
        let target = route(ApiBackend::Responses, Boundary::AzStrict, ADMITTED);
        let from = origin(Boundary::AzStrict, Some("gpt-5.6-sol"));
        assert_eq!(
            discovery_tier("gpt-5.6-sol", &target, &from),
            DiscoveryTier::Keep
        );
    }

    /// D1 on the row axis: a different row inside one boundary re-keys.
    #[test]
    fn row_change_within_responses_re_keys() {
        let target = route(ApiBackend::Responses, Boundary::AzStrict, ADMITTED);
        let other_row = origin(Boundary::AzStrict, Some("gpt-5.6-terra"));
        assert_eq!(
            discovery_tier("gpt-5.6-sol", &target, &other_row),
            DiscoveryTier::ReKey
        );
    }

    /// An unresolvable owner is treated as a change (fail-closed): it is never
    /// a KEEP of a record nobody owns. Its own test because the assertion lived
    /// behind a sibling assert that panicked first, so it never executed under a
    /// mutant; M12 (letting an unattributable owner satisfy the KEEP condition)
    /// kills this test and nothing else.
    #[test]
    fn unattributable_owner_never_keeps() {
        let target = route(ApiBackend::Responses, Boundary::AzStrict, ADMITTED);
        let unattributed = origin(Boundary::AzStrict, None);
        assert_eq!(
            discovery_tier("gpt-5.6-sol", &target, &unattributed),
            DiscoveryTier::ReKey,
            "an unattributable record cannot be a KEEP"
        );
    }

    /// Rule 3's FOLD is what gates the KEEP, never one of its inputs, on both
    /// non-`Vertex` Responses boundaries. All four combinations of the two
    /// signals are sampled on each: the KEEP needs both, and each of the other
    /// three demotes. Two defects this table exists to kill, neither of which
    /// the three-row `AzStrict`-only version could see:
    ///
    /// - reading one signal instead of the fold — M13 (read
    ///   `supports_search_tool` instead of `admitted()`) and M14 (read
    ///   `has_searchable_tools`) both die here;
    /// - folding the signals by EQUALITY instead of conjunction. `admitted()` is
    ///   `supports_search_tool && has_searchable_tools` (`SearchAdmission::admitted`,
    ///   `conversation::responses`);
    ///   a fold written `supports == has` reproduces the shipped verdict of
    ///   every row except [`NEITHER_SIGNAL`] and differs from `&&` exactly there,
    ///   where it KEEPs a loaded set on a route that has neither the declaration
    ///   flag nor a manifest to serve. The fourth row is what makes it visible —
    ///   without it all three sampled verdicts are identical under both folds
    ///   (`/private/tmp/px29_impl_r3/fold_proof.rs` reproduces both).
    ///
    /// The `VLLenient` column is not decoration: the other place this goes wrong
    /// is the GUARD, not the fold. A guard rewritten as
    /// `boundary == Vertex || (!admitted && boundary != VLLenient)` passes a
    /// table that samples only `AzStrict`, and silently admits every unadmitted
    /// lenient row. Each cell's origin is the target's own row and boundary, so
    /// every cell reaches the KEEP test and the tier is decided by the fold
    /// alone, never by an origin that fails first.
    #[test]
    fn rule_three_fold_gates_the_keep_and_neither_signal_alone_does() {
        for (boundary, model_id) in [
            (Boundary::AzStrict, "gpt-5.6-sol"),
            (Boundary::VLLenient, "qwen3.8-27b"),
        ] {
            let from = origin(boundary, Some(model_id));
            for (admission, want) in [
                (ADMITTED, DiscoveryTier::Keep),
                (NOTHING_SEARCHABLE, DiscoveryTier::Demote),
                (ROW_REJECTS, DiscoveryTier::Demote),
                (NEITHER_SIGNAL, DiscoveryTier::Demote),
            ] {
                let target = route(ApiBackend::Responses, boundary, admission);
                assert_eq!(
                    discovery_tier(model_id, &target, &from),
                    want,
                    "{boundary:?} + {admission:?} must fold to {want:?}"
                );
            }
        }
    }

    /// D1 on the boundary axis: an unchanged model id landing on a row in
    /// another boundary regime crosses a mint domain and re-keys.
    #[test]
    fn boundary_change_within_responses_re_keys() {
        let target = route(ApiBackend::Responses, Boundary::VLLenient, ADMITTED);
        let from = origin(Boundary::AzStrict, Some("qwen3.8-27b"));
        assert_eq!(
            discovery_tier("qwen3.8-27b", &target, &from),
            DiscoveryTier::ReKey
        );
    }

    /// The whole D0/D1 grid, so neither axis can be exempted in combination with
    /// the other. The two tests above each change ONE axis, which is exactly the
    /// hole the mutation seat published: an arm inserted before the D1 fallback —
    /// `else if origin.owner_model_id.as_deref() != Some(target_model_id)
    /// && origin.boundary != target.boundary { Keep }` — changed both axes at once
    /// and survived 795/795 in the GATE profile (`S-ROUTE-DOUBLE`), because
    /// `row_change_within_responses_re_keys` holds the boundary still and
    /// `boundary_change_within_responses_re_keys` holds the row still.
    /// Sampling the grid closes its unsampled neighbour too: a changed boundary
    /// combined with an **unattributable** owner is a cell none of the earlier
    /// tests reaches (`unattributable_owner_never_keeps` holds the boundary still),
    /// so an exemption on that conjunction was equally free to ship.
    ///
    /// `Boundary::Vertex` is deliberately absent: a Vertex target is decided by
    /// rule 3 before either axis is read, and that cell has its own test
    /// ([`responses_vertex_route_disagreement_demotes`], contested under R-1).
    #[test]
    fn responses_keep_needs_both_axes_and_every_other_origin_cell_re_keys() {
        for (boundary, model_id, other_boundary) in [
            (Boundary::AzStrict, "gpt-5.6-sol", Boundary::VLLenient),
            (Boundary::VLLenient, "qwen3.8-27b", Boundary::AzStrict),
        ] {
            let target = route(ApiBackend::Responses, boundary, ADMITTED);
            for (cell, want, from) in [
                (
                    "same row, same boundary",
                    DiscoveryTier::Keep,
                    origin(boundary, Some(model_id)),
                ),
                (
                    "row changed, boundary same",
                    DiscoveryTier::ReKey,
                    origin(boundary, Some("gpt-5.6-terra")),
                ),
                (
                    "owner unattributable, boundary same",
                    DiscoveryTier::ReKey,
                    origin(boundary, None),
                ),
                (
                    "row same, boundary changed",
                    DiscoveryTier::ReKey,
                    origin(other_boundary, Some(model_id)),
                ),
                (
                    "row changed AND boundary changed",
                    DiscoveryTier::ReKey,
                    origin(other_boundary, Some("gpt-5.6-terra")),
                ),
                (
                    "owner unattributable AND boundary changed",
                    DiscoveryTier::ReKey,
                    origin(other_boundary, None),
                ),
            ] {
                assert_eq!(
                    discovery_tier(model_id, &target, &from),
                    want,
                    "{cell}: D0 needs the same owner row AND the same boundary; \
                     every other cell of this grid crosses a mint domain and re-keys"
                );
            }
        }
    }

    /// Admission gates the Responses tiers ONLY: D2 is a property of the
    /// `/messages` wire, so an unadmitted Messages route still materialises.
    /// Why the gate is not what D2 depends on is an argument, not a capture:
    /// D2's declaration surface is mandatory whatever the route's gate says
    /// (§4.2's D2 preamble, `xwire-boundary-map.md:282`, A-23; rule 7 is the
    /// declaration half that follows from it and rule 6 the transcript half, and
    /// rule 7 is what cites the `P2-missing` oracle below), while the
    /// harness-side admission gate decides whether
    /// the hosted `tool_search` declaration is ADVERTISED at all
    /// (`SearchAdmission::admitted`, `conversation::responses`) — a different decision, taken on a different
    /// surface. The oracle evidences the narrower claim, and only that:
    /// `FIX/grok-probe/P2-declaration-necessity/three-arm.json` — `P2-missing`
    /// (loaded name absent from `tools[]`) is 400, while `P2-plain` (no
    /// `defer_loading` key) and `P2-deferred` (`defer_loading: true`) are BOTH
    /// 200 — what the wire checks is the declaration, not the `defer_loading`
    /// flag. No admission signal exists in any of those three request bodies
    /// (their keys are `model`, `messages`, `tools`, `max_tokens`) and that
    /// probe's `meta.json` records only the declaration/`defer_loading`
    /// question, so the capture says nothing about the gate.
    /// CONTESTED cell: §4.1's D3 `when` reads an unadmitted Messages route into
    /// D3 — this test is the one that flips to `Demote` if ruling R-1
    /// (apex-waj.29) goes the literal wording's way.
    #[test]
    fn d2_materialises_on_an_unadmitted_messages_target() {
        let target = route(ApiBackend::Messages, Boundary::Vertex, ROW_REJECTS);
        let from = origin(Boundary::AzStrict, Some("gpt-5.6-sol"));
        assert_eq!(
            discovery_tier("claude-sonnet-5", &target, &from),
            DiscoveryTier::Materialise,
            "D2 must not inherit the Responses admission gate"
        );
    }

    // ---- discovery invariants at the seam as it exists today ----

    /// Pre-search control (§5.3 XT-12): no item carries discovery state (the IR
    /// has no discovery variant yet — §9 item 1), no orphan pairing, and no
    /// reasoning item — the T1 ladder legitimately re-keys those.
    const NO_DISCOVERY_SHAPE: &str = r#"[
        {"type":"user","content":[{"type":"text","text":"pre-search control"}]},
        {"type":"assistant","content":"reading first","tool_calls":[{"id":"tc_paired","name":"read_file","arguments":"{\"path\":\"x\"}"}],"model_id":"gpt-5.6-sol"},
        {"type":"tool_result","tool_call_id":"tc_paired","content":"file body","is_error":false},
        {"type":"assistant","content":"done","model_id":"gpt-5.6-sol"}
    ]"#;

    /// A legacy client-executed discovery round — the discovery payload that
    /// DOES reach this seam today. `search_tool` is `SEARCH_TOOL_NAME`
    /// (`xai-grok-tools/src/implementations/search_tool/mod.rs:14`), spelled
    /// out here because this crate must not depend on the tools crate.
    const LEGACY_SEARCH_ROUND: &str = r#"[
        {"type":"user","content":[{"type":"text","text":"find the eta tool"}]},
        {"type":"assistant","content":"","tool_calls":[{"id":"tc_search_1","name":"search_tool","arguments":"{\"query\":\"shipping ETA\"}"}],"model_id":"gpt-5.6-sol"},
        {"type":"tool_result","tool_call_id":"tc_search_1","content":"{\"tools\":[{\"namespace\":\"mcp__crm_fixture\",\"name\":\"lookup_shipping_eta\"}]}","is_error":false}
    ]"#;

    fn items_from(json: &str) -> Vec<ConversationItem> {
        serde_json::from_str(json).expect("fixture must deserialize")
    }

    /// XT-12 (`pre_search_control_is_untouched`, §5.3): the input is
    /// discovery-free AND reasoning-free, so on all three boundaries the
    /// projection changes no item value (SDD-71 invariant 6), records nothing,
    /// and projecting twice to the same target is a fixed point (XD-6).
    ///
    /// VALUE identity, not byte identity, and the name says so: `item_values`
    /// round-trips both sides through `serde_json::Value`, which erases key order,
    /// number spelling and escape spelling, so a projector that re-serialised a
    /// record under a different byte layout would pass here. True byte identity
    /// would mean comparing the bytes each side actually goes on the wire — that
    /// is a different fixture question (a stored record's raw JSON text plus a
    /// projector that reports what it touched) and it is NOT what this seam
    /// promises today; see §7 of the lane report for what it would cost.
    ///
    /// Two limits of this pin, stated rather than implied: (a) with 0 reasoning
    /// items it excludes precisely the class this projector rewrites, so the
    /// reasoning-bearing case is NOT owned here — it is pinned over the
    /// provenance-mirrored corpus (bead **apex-ayl.71**, the owner of
    /// `fixtures/projection_x71/`) by `projection_tests.rs:414`
    /// (`xw_proj_value_identity_nonprojected`, whose comparison is `as_value`, the
    /// same value-identity standard as here) and `projection_tests.rs:449`
    /// (`xw_proj_idempotence`), and duplicating it here would step on that
    /// owner; (b) §5.3's named donor load is CX3 `request.json`, which lives in
    /// the campaign fixtures dir outside this repo and is still owed — this
    /// shape is an in-tree stand-in.
    #[test]
    fn xt12_history_with_no_discovery_item_projects_value_identical() {
        let items = items_from(NO_DISCOVERY_SHAPE);
        for boundary in [Boundary::AzStrict, Boundary::VLLenient, Boundary::Vertex] {
            let first = project_switch_history(
                &items,
                "gpt-5.6-sol",
                boundary,
                None,
                &inert_route(boundary),
            );
            assert!(
                first.drops.is_empty(),
                "{boundary:?} removed items from a discovery-free history: {:?}",
                first.drops
            );
            assert_eq!(
                item_values(&first.items),
                item_values(&items),
                "{boundary:?} rewrote a non-projected item"
            );
            let second = project_switch_history(
                &first.items,
                "gpt-5.6-sol",
                boundary,
                None,
                &inert_route(boundary),
            );
            assert_eq!(
                item_values(&second.items),
                item_values(&first.items),
                "{boundary:?} projection is not a fixed point"
            );
            assert!(second.drops.is_empty());
        }
    }

    /// The live surface, pinned so the future discovery arm cannot regress it:
    /// an assistant `search_tool` call and its result must both survive
    /// projection to every boundary, unchanged, with zero drops recorded. On
    /// Vertex this fixture takes the co-drop guard's FALSE branch — its result
    /// pairs with an ASSISTANT call, so the guard does not fire and the pair is
    /// kept. The co-drop arm itself is pinned by `DROP_SHAPE` and by the x71
    /// case-1 shape above; the pairing rule that decides which branch this takes
    /// for a MULTI-call assistant is pinned by
    /// `vertex_pairs_a_result_with_any_call_of_one_assistant_item`.
    ///
    /// “Unchanged” is value identity through `item_values`, not byte identity
    /// (key order, number spelling and escape spelling are invisible there), and
    /// the whole-list assert below is the only claim about the contents: an extra
    /// assert that the `search_tool` call is present cannot fail once that
    /// identity holds, so none is made.
    #[test]
    fn legacy_search_tool_discovery_round_survives_every_boundary() {
        let items = items_from(LEGACY_SEARCH_ROUND);
        for boundary in [Boundary::AzStrict, Boundary::VLLenient, Boundary::Vertex] {
            let projected = project_switch_history(
                &items,
                "gpt-5.6-sol",
                boundary,
                None,
                &inert_route(boundary),
            );
            assert!(
                projected.drops.is_empty(),
                "{boundary:?} must record no drop for a paired search round: {:?}",
                projected.drops
            );
            // Presence, order and content of both halves follow from this one
            // identity assert.
            assert_eq!(
                item_values(&projected.items),
                item_values(&items),
                "{boundary:?} rewrote the live discovery round"
            );
        }
    }
}
