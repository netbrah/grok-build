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
//! NOT XD-1 SURVIVAL: XD-1's subject is a `Discovery` item's presence in the projected
//! history (§4.3). The variant exists (`conversation.rs:318`) and the decode seam produces
//! it (`conversation/responses.rs:141-164`); `projection.rs:295`'s catch-all clones it here.
//! Reach against the XD invariants this seam has a subject for, stated rather than implied: this ledger pins XD-2 PAIR
//! ATOMICITY's DROP direction for the one pair class that exists here —
//! `vertex_target_records_both_drop_reasons` records the non-carrier call
//! together with the result co-dropped with it — and
//! `legacy_search_tool_discovery_round_survives_every_boundary` pins XD-2's KEEP
//! direction for the live discovery round, the `search_tool` pair surviving every
//! boundary. XD-6 (idempotence) is also partially pinned. No arm or test in this module reads a `ConversationItem::Discovery` item,
//! so XD-1 is guarded outside it: a dropped pair reddens `tool_search.rs:6618`, `xai-chat-state/src/actor/tests.rs:5872`. XD-3 is unasserted here: no other test drives a `Discovery` item through this projector, and those two compare `raw()` bytes, never a loaded set. XD-4 has no subject: `project_switch_history` takes `&[ConversationItem]` and returns `ProjectedHistory { items, drops }` (:152-162), so no `tools[]` crosses the seam. XD-5 is pinned for reasoning items only: `projection_tests.rs:364` `xw_proj_id_grammar_canonical` (`reasoning_ids`, :606) and `:481` `xw_proj_no_empty_id_no_foreign_encrypted` (non-`Reasoning` items skipped at :492).
//!
//! Discovery tier decision (xwire-boundary-map.md §4.1): [`discovery_tier`]
//! decides which of the D0/D1/D2/D3 tiers a discovery record takes on a target
//! route, off `ApiBackend` + `Boundary` + the target row's admission gate: §9
//! item 6 is the no-fourth-family rule and names only `Boundary` + `ApiBackend`,
//! while the admission gate is item 3's third key ("keyed on `Boundary` plus
//! `ApiBackend`, and on the TARGET row's admission gate"), not a fourth name for
//! a family.
//! The projector arm itself is NOT here: `project_switch_history` (:196) is production code
//! (`xai-chat-state/src/actor/mutations.rs:309`) with no `Discovery` arm — bead apex-waj.2.
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
/// presence — and no arm here reads one yet; that arm is bead apex-waj.2.
/// Surfacing this type outside the crate is bead apex-waj.37.
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
    // storage form equal to what the target wire can represent. In `conversation/messages.rs`
    // the `paired` set (`:112`) drives the `kept_calls` filter (`:118-123`) that rewrites
    // a stored assistant, and `:124-127` deletes that assistant outright when it leaves
    // no call and no content; assistant items here take the passthrough arm below and
    // are cloned verbatim. Why this seam stops at the result direction:
    //   1. it owes pair-atomicity only for the pairs it breaks itself. The T3 arm
    //      removes a backend call, so that call's result goes with it (xwfix
    //      invariant 3 / sdd-71 §4 invariant 1). An assistant call whose result was
    //      never stored is not such a pair, and the build's own D5 pass — plus the
    //      adjacency re-check at `conversation/messages.rs:159` — strips it on every
    //      `/messages` build, so it never reaches the target whether or not the store was
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

/// Native decisions are qualified KEEP, whole-group F, or refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(super) enum DiscoveryTier {
    Keep,
    Demote,
    Refuse,
    /// Legacy reference value; the native policy never returns it.
    ReKey,
    /// Legacy reference value; the native policy never returns it.
    Materialise,
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
#[allow(dead_code)] // pending apex-waj.2 (the projector arms that read the tuple; D0..D3 ladder apex-waj.29, in flight); the route tuple itself landed with apex-waj.35
pub struct DiscoveryOrigin {
    pub boundary: Boundary,
    pub owner_model_id: Option<String>,
}

/// Qualified native replay policy. Production projection remains unchanged until U8.
#[allow(dead_code)]
pub(super) fn discovery_tier(
    target_model_id: &str,
    target: &TargetRoute,
    origin: &DiscoveryOrigin,
    qualification: &NativeQualification,
) -> DiscoveryTier {
    use super::native_search_history::NativeReplayFamily;
    let Some(domain) = qualification
        .target
        .as_ref()
        .filter(|domain| domain.complete())
    else {
        return DiscoveryTier::Refuse;
    };
    if qualification.request.as_ref() != Some(domain)
        || domain.model != target_model_id
        || domain.backend != target.backend
        || qualification.original_admission != target.admission
        || !qualification.intact_membership
    {
        return DiscoveryTier::Refuse;
    }
    if matches!(
        target.backend,
        ApiBackend::ChatCompletions | ApiBackend::GenerateContent
    ) || !target.admission.admitted()
        || (target.backend == ApiBackend::Responses && target.boundary == Boundary::Vertex)
        || (target.backend == ApiBackend::Messages && target.boundary != Boundary::Vertex)
    {
        return DiscoveryTier::Demote;
    }
    let Some(source) = qualification
        .source
        .as_ref()
        .filter(|source| source.complete())
    else {
        return DiscoveryTier::Demote;
    };
    let supported = matches!(
        (&domain.dialect, qualification.shape.family),
        (
            NativeReplayDialect::Responses(super::ResponsesReplayDialect::Codex),
            NativeReplayFamily::ResponsesClient | NativeReplayFamily::ResponsesServer
        ) | (
            NativeReplayDialect::Messages,
            NativeReplayFamily::MessagesServer
        )
    );
    let qualified_shape = qualification.receipt.as_ref().is_some_and(|receipt| {
        !receipt.receipt_id.is_empty()
            && receipt.target == *domain
            && receipt.shape == qualification.shape
    });
    if source != domain
        || origin.owner_model_id.as_deref() != Some(source.model.as_str())
        || origin.boundary != target.boundary
        || !qualification.shape.completed
        || !qualification.current_backing
        || !qualification.faithful_identity
        || !supported
        || !qualified_shape
    {
        DiscoveryTier::Demote
    } else {
        DiscoveryTier::Keep
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeReplayDialect {
    Responses(super::ResponsesReplayDialect),
    Messages,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeReplayDomain {
    pub backend: ApiBackend,
    pub dialect: NativeReplayDialect,
    pub model: String,
    pub row: String,
    pub provider: String,
    pub deployment: String,
    pub deployment_revision: String,
    pub endpoint: String,
    pub principal: String,
}

impl NativeReplayDomain {
    pub(super) fn complete(&self) -> bool {
        [
            &self.model,
            &self.row,
            &self.provider,
            &self.deployment,
            &self.deployment_revision,
            &self.endpoint,
            &self.principal,
        ]
        .iter()
        .all(|field| !field.is_empty())
            && (self.endpoint.starts_with("https://") || self.endpoint.starts_with("http://"))
            && match (&self.backend, &self.dialect) {
                (ApiBackend::Responses, NativeReplayDialect::Responses(_))
                | (ApiBackend::Messages, NativeReplayDialect::Messages)
                | (ApiBackend::ChatCompletions, _) => true,
                _ => false,
            }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeReplayShape {
    pub family: super::native_search_history::NativeReplayFamily,
    pub keyless_server: bool,
    pub output_id_present: bool,
    pub completed: bool,
    pub result_error: bool,
    pub referenced_members: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeShapeReceipt {
    pub target: NativeReplayDomain,
    pub shape: NativeReplayShape,
    pub receipt_id: String,
}

pub struct NativeQualification {
    pub source: Option<NativeReplayDomain>,
    pub target: Option<NativeReplayDomain>,
    pub request: Option<NativeReplayDomain>,
    pub original_admission: SearchAdmission,
    pub shape: NativeReplayShape,
    pub receipt: Option<NativeShapeReceipt>,
    pub intact_membership: bool,
    pub current_backing: bool,
    pub faithful_identity: bool,
}

pub struct NativePureHistory<'a> {
    pub record: &'a super::native_search_history::NativeSearchHistoryV1,
    pub source: &'a super::native_search_history::NativeSource,
    pub ledger: &'a super::native_search_history::NativeLedger,
    pub identity: &'a super::native_search_history::NativeIdentityExtension,
    pub blobs: &'a std::collections::BTreeMap<String, std::sync::Arc<[u8]>>,
    pub authorship:
        &'a std::collections::BTreeMap<u64, super::native_search_history::NativeIdAuthorship>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeHistoryAction {
    Keep,
    F,
    Strip,
    AwaitClient,
    AwaitResults,
    ContinueOriginal,
    OmitSendOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePreparedMember {
    pub source: super::native_search_history::NativeSourceCoordinate,
    pub action: NativeHistoryAction,
    pub raw: Option<Value>,
    pub neutral: Option<super::native_search_history::NativeNeutralText>,
    pub duplicate_of: Option<super::native_search_history::NativeSourceCoordinate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeGroupDecision {
    pub key: String,
    pub action: NativeHistoryAction,
    pub members: Vec<super::native_search_history::NativeSourceCoordinate>,
    pub waiting_clients: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeHistoryDecision {
    pub groups: Vec<NativeGroupDecision>,
    pub members: Vec<NativePreparedMember>,
    pub f_provenance: super::native_search_history::NativeFProvenance,
    pub qualification: Vec<super::native_search_history::NativeGroupQualification>,
}

pub(super) fn native_policy_blob<'a>(
    history: &NativePureHistory<'a>,
    reference: &super::native_search_history::NativeBlobRef,
    kind: &str,
) -> Result<&'a [u8], super::native_search_history::NativeUnavailable> {
    use super::native_search_history::NativeUnavailable;
    let bytes = history
        .blobs
        .get(&reference.digest)
        .ok_or(NativeUnavailable::InvalidReference)?;
    if reference.kind != kind
        || reference.byte_len != bytes.len() as u64
        || reference.digest != hex_digest(&sha256(bytes))
    {
        return Err(NativeUnavailable::InvalidReference);
    }
    Ok(bytes)
}

pub(super) fn native_content_digest(bytes: &[u8]) -> String {
    hex_digest(&sha256(bytes))
}

pub(super) fn native_policy_history(
    history: &NativePureHistory<'_>,
) -> Result<(), super::native_search_history::NativeUnavailable> {
    use super::native_search_history::*;
    let record = history.record;
    let profile = (
        record.schema_version,
        record.policy_revision,
        record.reader_floor,
        record.limits_revision,
    );
    if !matches!(profile, (1, 1, 1, 1) | (2, 1, 2, 1)) {
        return Err(NativeUnavailable::UnsupportedVersion);
    }
    let source_bytes = native_policy_blob(history, &record.source, "source")?;
    let ledger_bytes = native_policy_blob(history, &record.ledger, "ledger")?;
    NativeResource::SourceBytes.admit(source_bytes.len() as u64, 0)?;
    NativeResource::LedgerBytes.admit(ledger_bytes.len() as u64, 0)?;
    let equal = match (record.schema_version, history.identity) {
        (1, NativeIdentityExtension::Revision1Unknown) => {
            serde_json::from_slice::<NativeSource>(source_bytes)
                .ok()
                .as_ref()
                == Some(history.source)
                && serde_json::from_slice::<NativeLedger>(ledger_bytes)
                    .ok()
                    .as_ref()
                    == Some(history.ledger)
        }
        (
            2,
            NativeIdentityExtension::Revision2 {
                eligible_member_links,
                eligible_member_accounting,
            },
        ) => {
            let source = serde_json::from_slice::<NativeSourceV2>(source_bytes)
                .map_err(|_| NativeUnavailable::InvalidReference)?
                .into_parts();
            let ledger = serde_json::from_slice::<NativeLedgerV2>(ledger_bytes)
                .map_err(|_| NativeUnavailable::InvalidReference)?
                .into_parts();
            source.0 == *history.source
                && source.1 == *eligible_member_links
                && ledger.0 == *history.ledger
                && ledger.1 == *eligible_member_accounting
        }
        _ => return Err(NativeUnavailable::UnsupportedVersion),
    };
    if !equal
        || history.source.schema_version != record.schema_version
        || history.source.lineage != record.lineage.id
        || history.ledger.source_digest != record.source.digest
        || record.lineage.source_digest != record.source.digest
        || history.ledger.active_digest != record.active.digest
    {
        return Err(NativeUnavailable::InvalidReference);
    }
    NativeResource::SourceBlocks.admit(history.source.ordered_blocks.len() as u64, 0)?;
    NativeResource::SourceGroups.admit(history.ledger.groups.len() as u64, 0)?;
    for (index, block) in history.source.ordered_blocks.iter().enumerate() {
        if block.coordinates.occurrence != index as u64
            || block.coordinates.lineage != history.source.lineage
        {
            return Err(NativeUnavailable::InvalidReference);
        }
        let entries: Vec<_> = history
            .ledger
            .accounting
            .iter()
            .filter(|entry| entry.source == block.coordinates)
            .collect();
        if entries.len() != 1 {
            return Err(NativeUnavailable::InvalidReference);
        }
    }
    Ok(())
}

fn native_policy_backing(
    history: &NativePureHistory<'_>,
    block: &super::native_search_history::NativeRawBlock,
    raw: &Value,
    tools: &[super::ToolSpec],
    current: Option<&super::native_search_history::NativeEligibleInputs>,
) -> Result<bool, super::native_search_history::NativeUnavailable> {
    use super::native_search_history::*;
    let leaves = native_candidate_references(raw)?;
    if leaves.is_empty() {
        return Ok(true);
    }
    let NativeIdentityExtension::Revision2 {
        eligible_member_links,
        eligible_member_accounting,
    } = history.identity
    else {
        return Ok(false);
    };
    let belongs = |coordinate: &NativeSourceCoordinate| {
        let mut parent = coordinate.clone();
        parent.submember = None;
        parent == block.coordinates
    };
    let links: Vec<_> = eligible_member_links
        .iter()
        .filter(|link| belongs(&link.source))
        .collect();
    let witnesses: Vec<_> = eligible_member_accounting
        .iter()
        .filter(|entry| belongs(&entry.source))
        .collect();
    let parent = history
        .ledger
        .accounting
        .iter()
        .find(|entry| entry.source == block.coordinates)
        .ok_or(NativeUnavailable::InvalidReference)?;
    if links.len() != leaves.len() || witnesses.len() != leaves.len() {
        return Err(NativeUnavailable::InvalidReference);
    }
    let mut backed = true;
    for (ordinal, ((leaf, link), witness)) in leaves.iter().zip(links).zip(witnesses).enumerate() {
        let mut expected = block.coordinates.clone();
        expected.submember = Some(ordinal as u64);
        if link.source != expected
            || witness.source != expected
            || witness.source_digest != parent.source_digest
        {
            return Err(NativeUnavailable::InvalidReference);
        }
        let NativeEligibleMemberIdentity::Backed { eligible_ref } = &link.identity else {
            backed = false;
            continue;
        };
        if !history.source.eligible_snapshots.contains(eligible_ref) {
            return Err(NativeUnavailable::InvalidReference);
        }
        let snapshot: NativeEligibleSnapshot = serde_json::from_slice(native_policy_blob(
            history,
            eligible_ref,
            "eligible_snapshot",
        )?)
        .map_err(|_| NativeUnavailable::InvalidReference)?;
        let schema = native_policy_blob(history, &snapshot.schema, "eligible_schema")?;
        let binding: Vec<_> = history
            .source
            .producing_bindings
            .iter()
            .filter(|binding| binding.request == block.coordinates.producing_binding)
            .collect();
        if snapshot.version != 1 || binding.len() != 1 || snapshot.binding != binding[0].origin_ref
        {
            return Err(NativeUnavailable::InvalidReference);
        }
        native_policy_blob(history, &snapshot.binding, "producing_binding")?;
        let NativeCandidateReference::Tool {
            namespace,
            name,
            declaration,
        } = leaf
        else {
            return Err(NativeUnavailable::InvalidReference);
        };
        let flat = if snapshot.namespace_kind == NativeNamespaceKind::Mcp {
            format!("{}__{}", snapshot.qualified_namespace, snapshot.child_name)
        } else {
            snapshot.child_name.clone()
        };
        let named = match namespace {
            Some(namespace) => {
                *namespace == snapshot.qualified_namespace && *name == snapshot.child_name
            }
            None => *name == flat,
        };
        if !named
            || declaration.is_some_and(|declaration| {
                declaration
                    .get("parameters")
                    .and_then(|value| encode_native(value, NATIVE_SOURCE_BYTES).ok())
                    .as_deref()
                    != Some(schema)
            })
        {
            return Err(NativeUnavailable::InvalidReference);
        }
        let matches = current
            .filter(|inputs| inputs.matches_specs(tools))
            .map_or(0, |inputs| {
                inputs
                    .entries()
                    .iter()
                    .filter(|entry| {
                        entry.namespace_kind() == snapshot.namespace_kind
                            && entry.qualified_namespace() == snapshot.qualified_namespace
                            && entry.child_name() == snapshot.child_name
                            && entry.schema_digest() == snapshot.schema.digest
                            && entry.schema_bytes() == schema
                    })
                    .count()
            });
        backed &= matches == 1;
    }
    Ok(backed)
}

fn native_render_neutral(
    kind: super::native_search_history::NativeNeutralKind,
    status: super::native_search_history::NativeNeutralStatus,
    count: Option<u64>,
) -> super::native_search_history::NativeNeutralText {
    use super::native_search_history::{NativeNeutralKind, NativeNeutralStatus, NativeNeutralText};
    let text = match kind {
        NativeNeutralKind::Call => {
            "Historical search recorded; this history grants no current tool authority.".into()
        }
        NativeNeutralKind::Result => {
            let status = match status {
                NativeNeutralStatus::Completed => "completed",
                NativeNeutralStatus::Error => "error",
                NativeNeutralStatus::Unknown => "unknown",
            };
            let count = count.map_or_else(|| "unknown".into(), |count| count.to_string());
            format!(
                "Historical search result: {status}; count={count}. This history grants no current tool authority."
            )
        }
        NativeNeutralKind::Omission => {
            "Older historical search records omitted; no current tool authority is retained.".into()
        }
    };
    NativeNeutralText { kind, text }
}

pub struct NativeProjectionInputs<'a> {
    pub target_model_id: &'a str,
    pub target: &'a TargetRoute,
    pub tools: &'a [super::ToolSpec],
    pub current: Option<&'a super::native_search_history::NativeEligibleInputs>,
    pub selected: &'a [super::native_search_history::NativeSourceCoordinate],
    pub origin_domains: &'a std::collections::BTreeMap<String, NativeReplayDomain>,
    pub pending: &'a super::native_search_history::NativePendingContext,
    pub prior_f: Option<&'a super::native_search_history::NativeFProvenance>,
}

fn native_validate_f_provenance(
    history: &NativePureHistory<'_>,
    prior: Option<&super::native_search_history::NativeFProvenance>,
) -> Result<(), super::native_search_history::NativeUnavailable> {
    use super::native_search_history::*;
    let Some(prior) = prior else { return Ok(()) };
    if prior.version != 1 {
        return Err(NativeUnavailable::UnsupportedVersion);
    }
    NativeResource::SourceGroups.admit(prior.groups.len() as u64, 0)?;
    let mut keys = std::collections::BTreeSet::new();
    for old in &prior.groups {
        if !keys.insert(&old.key) || old.call.occurrence >= old.result.occurrence {
            return Err(NativeUnavailable::InvalidReference);
        }
        let group = history
            .ledger
            .groups
            .iter()
            .find(|group| group.key == old.key)
            .ok_or(NativeUnavailable::InvalidReference)?;
        if group.state != "Completed" {
            return Err(NativeUnavailable::InvalidReference);
        }
        for (coordinate, digest) in [
            (&old.call, &old.call_digest),
            (&old.result, &old.result_digest),
        ] {
            if !group.members.contains(coordinate)
                || !history
                    .source
                    .ordered_blocks
                    .iter()
                    .any(|block| &block.coordinates == coordinate && &block.block.digest == digest)
            {
                return Err(NativeUnavailable::InvalidReference);
            }
        }
    }
    Ok(())
}

fn native_apply_f_budget(
    decision: &mut NativeHistoryDecision,
    prior: Option<&super::native_search_history::NativeFProvenance>,
) -> Result<(), super::native_search_history::NativeUnavailable> {
    use super::native_search_history::*;
    if let Some(prior) = prior {
        for old in &prior.groups {
            if !decision
                .f_provenance
                .groups
                .iter()
                .any(|group| group.key == old.key)
            {
                return Err(NativeUnavailable::InvalidReference);
            }
        }
    }
    let mut candidates = Vec::new();
    for (index, group) in decision.groups.iter().enumerate() {
        if group.action != NativeHistoryAction::F {
            continue;
        }
        let emitted: Vec<_> = decision
            .members
            .iter()
            .filter(|member| {
                group.members.contains(&member.source) && member.duplicate_of.is_none()
            })
            .collect();
        if emitted.len() != 2 {
            return Err(NativeUnavailable::InvalidReference);
        }
        let mut bytes = 0usize;
        for member in &emitted {
            let text = member
                .neutral
                .as_ref()
                .ok_or(NativeUnavailable::InvalidReference)?;
            if text.text.len() > 128 {
                return Err(NativeUnavailable::Capacity);
            }
            bytes = bytes
                .checked_add(text.text.len())
                .ok_or(NativeUnavailable::Capacity)?;
        }
        if bytes > 256 {
            return Err(NativeUnavailable::Capacity);
        }
        candidates.push((index, emitted[1].source.occurrence, bytes));
    }
    candidates.sort_by_key(|(_, result, _)| *result);
    let notice = native_render_neutral(
        NativeNeutralKind::Omission,
        NativeNeutralStatus::Unknown,
        None,
    );
    if notice.text.len() > 128 {
        return Err(NativeUnavailable::Capacity);
    }
    let mut retained_bytes = 0usize;
    let mut retained = 0usize;
    for (index, _, bytes) in candidates.into_iter().rev() {
        if retained < 3 && retained_bytes + bytes + notice.text.len() <= 1024 {
            retained += 1;
            retained_bytes += bytes;
        } else {
            decision.groups[index].action = NativeHistoryAction::Strip;
        }
    }
    for group in &decision.groups {
        if group.action == NativeHistoryAction::Strip {
            for member in &mut decision.members {
                if group.members.contains(&member.source) {
                    member.action = NativeHistoryAction::Strip;
                    member.raw = None;
                    member.neutral = None;
                }
            }
        }
    }
    for provenance in &mut decision.f_provenance.groups {
        let group = decision
            .groups
            .iter()
            .find(|group| group.key == provenance.key)
            .ok_or(NativeUnavailable::InvalidReference)?;
        provenance.disposition = if group.action == NativeHistoryAction::Strip {
            NativeFDisposition::Strip
        } else {
            NativeFDisposition::F
        };
    }
    if let Some(last) = decision
        .f_provenance
        .groups
        .iter()
        .filter(|group| group.disposition == NativeFDisposition::Strip)
        .max_by_key(|group| group.result.occurrence)
    {
        let member = decision
            .members
            .iter_mut()
            .find(|member| member.source == last.result)
            .ok_or(NativeUnavailable::InvalidReference)?;
        member.neutral = Some(notice);
    }
    let total: usize = decision
        .members
        .iter()
        .filter_map(|member| member.neutral.as_ref())
        .map(|text| text.text.len())
        .sum();
    if total > 1024 {
        return Err(NativeUnavailable::Capacity);
    }
    Ok(())
}

pub(super) fn prepare_native_history(
    history: &NativePureHistory<'_>,
    target_model_id: &str,
    target: &TargetRoute,
    tools: &[super::ToolSpec],
    current: Option<&super::native_search_history::NativeEligibleInputs>,
    qualification: impl FnMut(
        &super::native_search_history::NativeGroupRecord,
    ) -> (DiscoveryOrigin, NativeQualification),
) -> Result<NativeHistoryDecision, super::native_search_history::NativeUnavailable> {
    let selected: Vec<_> = history
        .source
        .ordered_blocks
        .iter()
        .map(|block| block.coordinates.clone())
        .collect();
    prepare_native_history_selected(
        history,
        target_model_id,
        target,
        tools,
        current,
        &selected,
        &Default::default(),
        &Default::default(),
        qualification,
    )
}

pub(super) fn prepare_native_history_selected(
    history: &NativePureHistory<'_>,
    target_model_id: &str,
    target: &TargetRoute,
    tools: &[super::ToolSpec],
    current: Option<&super::native_search_history::NativeEligibleInputs>,
    selected: &[super::native_search_history::NativeSourceCoordinate],
    origin_domains: &std::collections::BTreeMap<String, NativeReplayDomain>,
    pending: &super::native_search_history::NativePendingContext,
    qualification: impl FnMut(
        &super::native_search_history::NativeGroupRecord,
    ) -> (DiscoveryOrigin, NativeQualification),
) -> Result<NativeHistoryDecision, super::native_search_history::NativeUnavailable> {
    prepare_native_history_reproject(
        history,
        &NativeProjectionInputs {
            target_model_id,
            target,
            tools,
            current,
            selected,
            origin_domains,
            pending,
            prior_f: None,
        },
        qualification,
    )
}

pub(super) fn prepare_native_history_reproject(
    history: &NativePureHistory<'_>,
    input: &NativeProjectionInputs<'_>,
    mut qualification: impl FnMut(
        &super::native_search_history::NativeGroupRecord,
    ) -> (DiscoveryOrigin, NativeQualification),
) -> Result<NativeHistoryDecision, super::native_search_history::NativeUnavailable> {
    use super::native_search_history::*;
    use super::tool_search::ToolSearchItem;
    native_policy_history(history)?;
    native_validate_f_provenance(history, input.prior_f)?;
    let NativeProjectionInputs {
        target_model_id,
        target,
        tools,
        current,
        selected,
        origin_domains,
        pending,
        prior_f: _,
    } = *input;
    let mut raw = std::collections::BTreeMap::new();
    let mut all_raw = std::collections::BTreeMap::new();
    for block in &history.source.ordered_blocks {
        let value: Value =
            serde_json::from_slice(native_policy_blob(history, &block.block, "raw_block")?)
                .map_err(|_| NativeUnavailable::InvalidReference)?;
        all_raw.insert(block.coordinates.occurrence, value.clone());
        if let Ok(item) = ToolSearchItem::from_wire(value) {
            raw.insert(block.coordinates.occurrence, item);
        }
    }
    let mut qualified = Vec::new();
    let mut domains = origin_domains.clone();
    for group in &history.ledger.groups {
        let pair = qualification(group);
        if let Some(source) = pair.1.source.as_ref().filter(|source| source.complete()) {
            if domains
                .get(&group.origin_ref.digest)
                .is_some_and(|old| old != source)
            {
                return Err(NativeUnavailable::ConflictingObservation);
            }
            domains.insert(group.origin_ref.digest.clone(), source.clone());
        }
        qualified.push(pair);
    }
    let same_domain = |left: &NativeBlobRef, right: &NativeBlobRef| {
        left == right
            || domains
                .get(&left.digest)
                .zip(domains.get(&right.digest))
                .is_some_and(|(left, right)| left.complete() && right.complete() && left == right)
    };
    native_validate_waiting_clients(history.source, history.ledger, &all_raw)?;
    let groups = native_validate_groups(
        history.source,
        history.ledger,
        &raw,
        history.authorship,
        same_domain,
    )?;
    let active: NativeActive = serde_json::from_slice(native_policy_blob(
        history,
        &history.record.active,
        "active",
    )?)
    .map_err(|_| NativeUnavailable::InvalidReference)?;
    let mut decision = NativeHistoryDecision {
        groups: Vec::new(),
        members: Vec::new(),
        f_provenance: NativeFProvenance::empty(),
        qualification: Vec::new(),
    };
    for (group, (origin, mut qualified)) in groups.into_iter().zip(qualified) {
        let references = match history.identity {
            NativeIdentityExtension::Revision1Unknown => 0,
            NativeIdentityExtension::Revision2 {
                eligible_member_links,
                ..
            } => eligible_member_links
                .iter()
                .filter(|link| {
                    let mut parent = link.source.clone();
                    parent.submember = None;
                    group.record.members.contains(&parent)
                })
                .count() as u64,
        };
        NativeResource::GroupMembers.admit(group.record.members.len() as u64, references)?;
        let (mut action, waiting_clients) = if group.state == NativeGroupState::Completed {
            let call = &group.primary[0].2;
            let output = &group.primary[1].2;
            let mut backing = true;
            for coordinate in &group.record.members {
                let block = &history.source.ordered_blocks[coordinate.occurrence as usize];
                backing &= native_policy_backing(
                    history,
                    block,
                    raw[&coordinate.occurrence].raw(),
                    tools,
                    current,
                )?;
            }
            qualified.shape = NativeReplayShape {
                family: call.family,
                keyless_server: call.keyless_server,
                output_id_present: output.item_id_present,
                completed: true,
                result_error: output.result_error,
                referenced_members: native_candidate_references(group.primary[1].1.raw())?.len()
                    as u64,
            };
            qualified.intact_membership = true;
            qualified.current_backing = backing;
            qualified.faithful_identity = group
                .primary
                .iter()
                .all(|member| member.2.faithful_identity);
            let tier = discovery_tier(target_model_id, target, &origin, &qualified);
            let policy = match tier {
                DiscoveryTier::Keep => NativeHistoryAction::Keep,
                DiscoveryTier::Demote => NativeHistoryAction::F,
                DiscoveryTier::Refuse | DiscoveryTier::ReKey | DiscoveryTier::Materialise => {
                    return Err(NativeUnavailable::UnknownIdentity);
                }
            };
            let whole_view = group.accepted
                && group
                    .primary
                    .iter()
                    .all(|member| selected.contains(&member.0));
            let retained = native_completed_action(
                whole_view,
                active
                    .dispositions
                    .get(&group.record.key)
                    .map(String::as_str),
            )?;
            (
                if retained == NativeHistoryAction::Keep {
                    policy
                } else {
                    retained
                },
                Vec::new(),
            )
        } else {
            if group.state == NativeGroupState::LivePending
                && group.primary[0].2.family == NativeReplayFamily::ResponsesServer
            {
                let shape = NativeReplayShape {
                    family: NativeReplayFamily::ResponsesServer,
                    keyless_server: group.primary[0].2.keyless_server,
                    output_id_present: false,
                    completed: false,
                    result_error: false,
                    referenced_members: 0,
                };
                let qualified_pending = qualified
                    .source
                    .as_ref()
                    .filter(|source| source.complete())
                    .zip(qualified.receipt.as_ref())
                    .is_some_and(|(source, receipt)| {
                        receipt.target == *source
                            && receipt.shape == shape
                            && !receipt.receipt_id.is_empty()
                            && source.dialect
                                == NativeReplayDialect::Responses(
                                    super::ResponsesReplayDialect::Codex,
                                )
                    });
                if !qualified_pending {
                    return Err(NativeUnavailable::UnknownIdentity);
                }
            }
            native_pending_action(&group, history.source, history.ledger, pending)?
        };
        if let Some(prior) = input
            .prior_f
            .and_then(|prior| prior.groups.iter().find(|old| old.key == group.record.key))
        {
            if group.primary.len() != 2
                || prior.call != group.primary[0].0
                || prior.result != group.primary[1].0
            {
                return Err(NativeUnavailable::InvalidReference);
            }
            action = match prior.disposition {
                NativeFDisposition::F if action != NativeHistoryAction::Strip => {
                    NativeHistoryAction::F
                }
                NativeFDisposition::F | NativeFDisposition::Strip => NativeHistoryAction::Strip,
            };
        }
        let neutral = if matches!(action, NativeHistoryAction::F | NativeHistoryAction::Strip) {
            let call = &group.primary[0].0;
            let result = &group.primary[1];
            let call_block = &history.source.ordered_blocks[call.occurrence as usize];
            let result_block = &history.source.ordered_blocks[result.0.occurrence as usize];
            decision.f_provenance.groups.push(NativeFGroup {
                key: group.record.key.clone(),
                call: call.clone(),
                call_digest: call_block.block.digest.clone(),
                result: result.0.clone(),
                result_digest: result_block.block.digest.clone(),
                disposition: if action == NativeHistoryAction::Strip {
                    NativeFDisposition::Strip
                } else {
                    NativeFDisposition::F
                },
            });
            let status = if !group.accepted {
                NativeNeutralStatus::Unknown
            } else if result.2.result_error {
                NativeNeutralStatus::Error
            } else {
                NativeNeutralStatus::Completed
            };
            let count = if group.accepted {
                u64::try_from(native_candidate_references(result.1.raw())?.len()).ok()
            } else {
                None
            };
            Some((status, count))
        } else {
            None
        };
        if let Some(target) = qualified.target {
            decision.qualification.push(NativeGroupQualification {
                group: group.record.key.clone(),
                source: qualified.source,
                target,
                shape: qualified.shape,
                receipt: qualified.receipt,
            });
        }
        decision.groups.push(NativeGroupDecision {
            key: group.record.key.clone(),
            action,
            members: group.record.members.clone(),
            waiting_clients,
        });
        for coordinate in &group.record.members {
            let duplicate_of = group
                .record
                .duplicate_observations
                .iter()
                .find(|(duplicate, _)| duplicate == coordinate)
                .map(|(_, original)| original.clone());
            let emit = duplicate_of.is_none()
                && matches!(
                    action,
                    NativeHistoryAction::Keep
                        | NativeHistoryAction::AwaitClient
                        | NativeHistoryAction::AwaitResults
                        | NativeHistoryAction::ContinueOriginal
                );
            let emit_neutral = duplicate_of.is_none();
            decision.members.push(NativePreparedMember {
                source: coordinate.clone(),
                action,
                duplicate_of,
                raw: emit.then(|| raw[&coordinate.occurrence].raw().clone()),
                neutral: neutral
                    .filter(|_| action == NativeHistoryAction::F && emit_neutral)
                    .map(|(status, count)| {
                        native_render_neutral(
                            if coordinate == &group.primary[0].0 {
                                NativeNeutralKind::Call
                            } else {
                                NativeNeutralKind::Result
                            },
                            status,
                            count,
                        )
                    }),
            });
        }
    }
    decision
        .members
        .sort_by_key(|member| member.source.occurrence);
    native_apply_f_budget(&mut decision, input.prior_f)?;
    Ok(decision)
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
    /// exemption set is a real assertion, not a default. `ConversationItem::Discovery`
    /// (`conversation.rs:318`) is the class discovery landed as, and the projector has no
    /// arm for it yet; widening this exemption must stay a decision a test is edited for.
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
            discovery_tier(
                owner_row,
                &route,
                &from,
                &u4_qualified_fixture(owner_row, &route, &from)
            ),
            DiscoveryTier::Keep,
            "the admitted route must reach D0"
        );

        // The same row with the operator's flag off is the fail-closed cell: nothing but the flag
        // differs, so a producer or a tuple that ignores it cannot pass.
        let unadmitted =
            SearchAdmission::for_row(/* supports_search_tool */ false, &declared_tools);
        let route = TargetRoute::new(ApiBackend::Responses, Boundary::AzStrict, unadmitted);
        assert_eq!(
            discovery_tier(
                owner_row,
                &route,
                &from,
                &u4_qualified_fixture(owner_row, &route, &from)
            ),
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
            discovery_tier(
                "gpt-5.6-sol",
                &target,
                &from,
                &u4_qualified_fixture("gpt-5.6-sol", &target, &from)
            ),
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
                discovery_tier(
                    model_id,
                    &target,
                    &from,
                    &u4_qualified_fixture(model_id, &target, &from)
                ),
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
            discovery_tier(
                "claude-sonnet-5",
                &target,
                &from,
                &u4_qualified_fixture("claude-sonnet-5", &target, &from)
            ),
            DiscoveryTier::Demote
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
                discovery_tier(
                    "claude-sonnet-5",
                    &target,
                    &from,
                    &u4_qualified_fixture("claude-sonnet-5", &target, &from)
                ),
                DiscoveryTier::Demote,
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
                    discovery_tier(
                        "claude-sonnet-5",
                        &target,
                        &from,
                        &u4_qualified_fixture("claude-sonnet-5", &target, &from)
                    ),
                    DiscoveryTier::Demote,
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
            discovery_tier(
                "claude-sonnet-5",
                &target,
                &from,
                &u4_qualified_fixture("claude-sonnet-5", &target, &from)
            ),
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
            discovery_tier(
                "gpt-5.6-sol",
                &target,
                &from,
                &u4_qualified_fixture("gpt-5.6-sol", &target, &from)
            ),
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
                    discovery_tier(
                        model_id,
                        &target,
                        &from,
                        &u4_qualified_fixture(model_id, &target, &from)
                    ),
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
            discovery_tier(
                "gpt-5.6-sol",
                &target,
                &from,
                &u4_qualified_fixture("gpt-5.6-sol", &target, &from)
            ),
            DiscoveryTier::Keep
        );
    }

    /// D1 on the row axis: a different row inside one boundary re-keys.
    #[test]
    fn row_change_within_responses_re_keys() {
        let target = route(ApiBackend::Responses, Boundary::AzStrict, ADMITTED);
        let other_row = origin(Boundary::AzStrict, Some("gpt-5.6-terra"));
        assert_eq!(
            discovery_tier(
                "gpt-5.6-sol",
                &target,
                &other_row,
                &u4_qualified_fixture("gpt-5.6-sol", &target, &other_row)
            ),
            DiscoveryTier::Demote
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
            discovery_tier(
                "gpt-5.6-sol",
                &target,
                &unattributed,
                &u4_qualified_fixture("gpt-5.6-sol", &target, &unattributed)
            ),
            DiscoveryTier::Demote,
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
                    discovery_tier(
                        model_id,
                        &target,
                        &from,
                        &u4_qualified_fixture(model_id, &target, &from)
                    ),
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
            discovery_tier(
                "qwen3.8-27b",
                &target,
                &from,
                &u4_qualified_fixture("qwen3.8-27b", &target, &from)
            ),
            DiscoveryTier::Demote
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
                    DiscoveryTier::Demote,
                    origin(boundary, Some("gpt-5.6-terra")),
                ),
                (
                    "owner unattributable, boundary same",
                    DiscoveryTier::Demote,
                    origin(boundary, None),
                ),
                (
                    "row same, boundary changed",
                    DiscoveryTier::Demote,
                    origin(other_boundary, Some(model_id)),
                ),
                (
                    "row changed AND boundary changed",
                    DiscoveryTier::Demote,
                    origin(other_boundary, Some("gpt-5.6-terra")),
                ),
                (
                    "owner unattributable AND boundary changed",
                    DiscoveryTier::Demote,
                    origin(other_boundary, None),
                ),
            ] {
                assert_eq!(
                    discovery_tier(
                        model_id,
                        &target,
                        &from,
                        &u4_qualified_fixture(model_id, &target, &from)
                    ),
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
            discovery_tier(
                "claude-sonnet-5",
                &target,
                &from,
                &u4_qualified_fixture("claude-sonnet-5", &target, &from)
            ),
            DiscoveryTier::Demote,
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

    // Identity/schema/link bytes from the executed U3 encoder; r5-u3-payload-extraction.json.
    const U4_PRODUCED: &str = r####"{"record":"{\"schema_version\":2,\"policy_revision\":1,\"reader_floor\":2,\"limits_revision\":1,\"lineage\":{\"id\":\"mock\",\"source_revision\":1,\"source_digest\":\"eb7a340288481abedb855f247f204158cc32fcba7ba2415f439b0a825b625a91\"},\"generation\":1,\"high_water\":1,\"source\":{\"kind\":\"source\",\"byte_len\":2295,\"digest\":\"eb7a340288481abedb855f247f204158cc32fcba7ba2415f439b0a825b625a91\"},\"active\":{\"kind\":\"active\",\"byte_len\":85,\"digest\":\"a2386459883db027fb29e12362319be4ef39700a73c9ab336b45adfef54f8c09\"},\"ledger\":{\"kind\":\"ledger\",\"byte_len\":2409,\"digest\":\"15efd76dc8ef1c0e3c486bca2fe42bcd1908105e489a63dd58a4a50bf0d00099\"},\"history_boundary\":{\"items\":0,\"digest\":\"4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945\"},\"exchange_snapshot\":null,\"previous_checkpoint\":null,\"capacity\":{\"active_bytes\":85,\"control_bytes\":1160,\"ledger_bytes\":2409,\"ledger_groups\":1,\"ledger_members\":6,\"reserved_backup_bytes\":6298,\"reserved_control_bytes\":4194304,\"reserved_current_bytes\":6298,\"reserved_lineage_bytes\":4213198,\"reserved_staging_bytes\":6298,\"snapshot_bytes\":0,\"source_blocks\":2,\"source_bytes\":3804,\"source_groups\":1,\"source_responses\":1,\"terminal_active_bytes\":0,\"terminal_ledger_bytes\":0}}","blobs":{"15efd76dc8ef1c0e3c486bca2fe42bcd1908105e489a63dd58a4a50bf0d00099":"{\"source_digest\":\"eb7a340288481abedb855f247f204158cc32fcba7ba2415f439b0a825b625a91\",\"active_digest\":\"a2386459883db027fb29e12362319be4ef39700a73c9ab336b45adfef54f8c09\",\"groups\":[{\"key\":\"u3-deterministic:1:1:0\",\"family\":\"Messages\",\"execution\":\"server\",\"origin_ref\":{\"kind\":\"producing_binding\",\"byte_len\":130,\"digest\":\"cb0312e961b6257ae5c935cc135d19b6c83d0368516da045078c76a86ecaf3ce\"},\"validated_join\":\"srvtoolu_u3\",\"keyless_server_fifo\":false,\"members\":[{\"lineage\":\"mock\",\"producing_binding\":\"u3-deterministic\",\"turn\":1,\"item\":0,\"response\":0,\"block\":0,\"submember\":null,\"occurrence\":0},{\"lineage\":\"mock\",\"producing_binding\":\"u3-deterministic\",\"turn\":1,\"item\":0,\"response\":0,\"block\":1,\"submember\":null,\"occurrence\":1}],\"duplicate_observations\":[],\"state\":\"Completed\"}],\"lifecycle\":{\"exchanges\":{\"u3-deterministic\":\"Terminal\"},\"groups\":{\"u3-deterministic:1:1:0\":\"Completed\"},\"waiting_clients\":[],\"terminal\":{}},\"accounting\":[{\"source\":{\"lineage\":\"mock\",\"producing_binding\":\"u3-deterministic\",\"turn\":1,\"item\":0,\"response\":0,\"block\":0,\"submember\":null,\"occurrence\":0},\"source_digest\":\"eb7a340288481abedb855f247f204158cc32fcba7ba2415f439b0a825b625a91\",\"view_item\":null,\"view_digest\":null,\"role\":\"SourceOnly\",\"source_subrange\":null},{\"source\":{\"lineage\":\"mock\",\"producing_binding\":\"u3-deterministic\",\"turn\":1,\"item\":0,\"response\":0,\"block\":1,\"submember\":null,\"occurrence\":1},\"source_digest\":\"eb7a340288481abedb855f247f204158cc32fcba7ba2415f439b0a825b625a91\",\"view_item\":null,\"view_digest\":null,\"role\":\"SourceOnly\",\"source_subrange\":null}],\"usage\":[{\"request\":\"u3-deterministic\",\"lease\":1,\"attempt\":1,\"response_occurrence\":0,\"continuation\":0,\"observed\":{\"input_tokens\":11,\"output_tokens\":7},\"source_stop_reason\":\"end_turn\",\"outcome\":\"Accepted\",\"latency_ms\":1,\"http_dispatches\":1}],\"observations\":[{\"request\":\"u3-deterministic\",\"lease\":1,\"attempt\":1,\"digest\":\"e2789a08fd081abd3b049eb3fbbafb578d583ac99238b0b8d4da422a09881bad\"}],\"transitions\":[],\"eligible_member_accounting\":[{\"source\":{\"lineage\":\"mock\",\"producing_binding\":\"u3-deterministic\",\"turn\":1,\"item\":0,\"response\":0,\"block\":1,\"submember\":0,\"occurrence\":1},\"source_digest\":\"eb7a340288481abedb855f247f204158cc32fcba7ba2415f439b0a825b625a91\"},{\"source\":{\"lineage\":\"mock\",\"producing_binding\":\"u3-deterministic\",\"turn\":1,\"item\":0,\"response\":0,\"block\":1,\"submember\":1,\"occurrence\":1},\"source_digest\":\"eb7a340288481abedb855f247f204158cc32fcba7ba2415f439b0a825b625a91\"}]}","34469c5546072d7cce326a0d7bd71814edbd22e0753ec403d4eb085e62b7feff":"{\"type\":\"tool_search_tool_result\",\"tool_use_id\":\"srvtoolu_u3\",\"content\":{\"type\":\"tool_search_tool_search_result\",\"tool_references\":[{\"type\":\"tool_reference\",\"tool_name\":\"alpha__lookup\"},{\"type\":\"tool_reference\",\"tool_name\":\"zeta__fetch\"}]}}","7a5e2899b3dbc43e06de504d98ecaae591e4ce62381a6c9a8b500e75bb1b0d84":"{\"type\":\"object\",\"description\":\"Lookup schema retained verbatim\",\"properties\":{\"query\":{\"type\":\"string\",\"default\":\"account\",\"description\":\"Exact query\"}},\"required\":[\"query\"],\"additionalProperties\":false}","a2386459883db027fb29e12362319be4ef39700a73c9ab336b45adfef54f8c09":"{\"projection_revision\":0,\"view_links\":[],\"dispositions\":{},\"loaded_invalidations\":[]}","ade6bc6092740b2064e4450153d2ebed0fbecf2cbedd7979233b4524fb057e10":"{\"type\":\"object\",\"properties\":{\"url\":{\"type\":\"string\"},\"limit\":{\"type\":\"integer\",\"default\":7}},\"required\":[\"url\"],\"additionalProperties\":false}","cb0312e961b6257ae5c935cc135d19b6c83d0368516da045078c76a86ecaf3ce":"{\"request\":\"u3-deterministic\",\"generation\":null,\"origin\":{\"status\":\"Unknown\",\"reason\":\"producing identity unavailable before U3\"}}","d7bcaf820f5be89d8c8060c00e406ada1a63dd0d73c36ef2d787b71f9b28eb52":"{\"type\":\"server_tool_use\",\"id\":\"srvtoolu_u3\",\"name\":\"tool_search_tool_regex\",\"input\":{\"query\":\"lookup\"}}","eb7a340288481abedb855f247f204158cc32fcba7ba2415f439b0a825b625a91":"{\"schema_version\":2,\"lineage\":\"mock\",\"source_revision\":1,\"producing_bindings\":[{\"request\":\"u3-deterministic\",\"generation\":null,\"origin\":{\"status\":\"Unknown\",\"reason\":\"producing identity unavailable before U3\"},\"origin_ref\":{\"kind\":\"producing_binding\",\"byte_len\":130,\"digest\":\"cb0312e961b6257ae5c935cc135d19b6c83d0368516da045078c76a86ecaf3ce\"}}],\"responses\":[{\"occurrence\":0,\"request\":\"u3-deterministic\",\"lease\":1,\"attempt\":1,\"continuation\":0,\"provider_response_id\":\"msg_u3\",\"source_stop_reason\":\"end_turn\",\"block_start\":0,\"block_end\":2,\"stream_complete\":true,\"status\":\"Accepted\"}],\"ordered_blocks\":[{\"coordinates\":{\"lineage\":\"mock\",\"producing_binding\":\"u3-deterministic\",\"turn\":1,\"item\":0,\"response\":0,\"block\":0,\"submember\":null,\"occurrence\":0},\"wire_index\":0,\"arrival_ordinal\":0,\"block\":{\"kind\":\"raw_block\",\"byte_len\":104,\"digest\":\"d7bcaf820f5be89d8c8060c00e406ada1a63dd0d73c36ef2d787b71f9b28eb52\"},\"partial\":null,\"state\":\"ClosedFaithful\",\"eligible_ref\":null,\"eligible_unavailable\":null},{\"coordinates\":{\"lineage\":\"mock\",\"producing_binding\":\"u3-deterministic\",\"turn\":1,\"item\":0,\"response\":0,\"block\":1,\"submember\":null,\"occurrence\":1},\"wire_index\":1,\"arrival_ordinal\":1,\"block\":{\"kind\":\"raw_block\",\"byte_len\":240,\"digest\":\"34469c5546072d7cce326a0d7bd71814edbd22e0753ec403d4eb085e62b7feff\"},\"partial\":null,\"state\":\"ClosedFaithful\",\"eligible_ref\":null,\"eligible_unavailable\":null}],\"eligible_snapshots\":[{\"kind\":\"eligible_snapshot\",\"byte_len\":345,\"digest\":\"fbeb800577aa31e9f3cf6bced5b30a0a94fe15ecf898d48f5c69a4a44ebe549a\"},{\"kind\":\"eligible_snapshot\",\"byte_len\":343,\"digest\":\"f2cd1b35d0416155002174e86365f59eda6bb09db3edd7b9223b165d75179187\"}],\"older_sources\":[],\"eligible_member_links\":[{\"source\":{\"lineage\":\"mock\",\"producing_binding\":\"u3-deterministic\",\"turn\":1,\"item\":0,\"response\":0,\"block\":1,\"submember\":0,\"occurrence\":1},\"identity\":{\"state\":\"Backed\",\"eligible_ref\":{\"kind\":\"eligible_snapshot\",\"byte_len\":345,\"digest\":\"fbeb800577aa31e9f3cf6bced5b30a0a94fe15ecf898d48f5c69a4a44ebe549a\"}}},{\"source\":{\"lineage\":\"mock\",\"producing_binding\":\"u3-deterministic\",\"turn\":1,\"item\":0,\"response\":0,\"block\":1,\"submember\":1,\"occurrence\":1},\"identity\":{\"state\":\"Backed\",\"eligible_ref\":{\"kind\":\"eligible_snapshot\",\"byte_len\":343,\"digest\":\"f2cd1b35d0416155002174e86365f59eda6bb09db3edd7b9223b165d75179187\"}}}]}","f2cd1b35d0416155002174e86365f59eda6bb09db3edd7b9223b165d75179187":"{\"version\":1,\"namespace_kind\":\"mcp\",\"qualified_namespace\":\"zeta\",\"child_name\":\"fetch\",\"schema\":{\"kind\":\"eligible_schema\",\"byte_len\":143,\"digest\":\"ade6bc6092740b2064e4450153d2ebed0fbecf2cbedd7979233b4524fb057e10\"},\"binding\":{\"kind\":\"producing_binding\",\"byte_len\":130,\"digest\":\"cb0312e961b6257ae5c935cc135d19b6c83d0368516da045078c76a86ecaf3ce\"}}","fbeb800577aa31e9f3cf6bced5b30a0a94fe15ecf898d48f5c69a4a44ebe549a":"{\"version\":1,\"namespace_kind\":\"mcp\",\"qualified_namespace\":\"alpha\",\"child_name\":\"lookup\",\"schema\":{\"kind\":\"eligible_schema\",\"byte_len\":204,\"digest\":\"7a5e2899b3dbc43e06de504d98ecaae591e4ce62381a6c9a8b500e75bb1b0d84\"},\"binding\":{\"kind\":\"producing_binding\",\"byte_len\":130,\"digest\":\"cb0312e961b6257ae5c935cc135d19b6c83d0368516da045078c76a86ecaf3ce\"}}"},"legacy_record":"{\"schema_version\":1,\"policy_revision\":1,\"reader_floor\":1,\"limits_revision\":1,\"lineage\":{\"id\":\"mock\",\"source_revision\":1,\"source_digest\":\"cdb4bed384771da19753940efef285e93976094fd6a634473a0c20d87acf749a\"},\"generation\":1,\"high_water\":1,\"source\":{\"kind\":\"source\",\"byte_len\":1452,\"digest\":\"cdb4bed384771da19753940efef285e93976094fd6a634473a0c20d87acf749a\"},\"active\":{\"kind\":\"active\",\"byte_len\":85,\"digest\":\"a2386459883db027fb29e12362319be4ef39700a73c9ab336b45adfef54f8c09\"},\"ledger\":{\"kind\":\"ledger\",\"byte_len\":1943,\"digest\":\"599681318b87a21803f08ccab754793d5c0c6488a218bb8e9e5c799e542c4987\"},\"history_boundary\":{\"items\":0,\"digest\":\"4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945\"},\"exchange_snapshot\":null,\"previous_checkpoint\":null,\"capacity\":{\"active_bytes\":85,\"control_bytes\":1160,\"ledger_bytes\":1943,\"ledger_groups\":1,\"ledger_members\":4,\"reserved_backup_bytes\":3955,\"reserved_control_bytes\":4194304,\"reserved_current_bytes\":3955,\"reserved_lineage_bytes\":4206169,\"reserved_staging_bytes\":3955,\"snapshot_bytes\":0,\"source_blocks\":2,\"source_bytes\":1927,\"source_groups\":1,\"source_responses\":1,\"terminal_active_bytes\":0,\"terminal_ledger_bytes\":0}}","legacy_blobs":{"32e162bfc7031d849bf853e7dc65e0118dfd1daa57d762de40060eef8844a28f":"{\"request\":\"u3-legacy-fixture\",\"generation\":null,\"origin\":{\"status\":\"Unknown\",\"reason\":\"producing identity unavailable before U3\"}}","34469c5546072d7cce326a0d7bd71814edbd22e0753ec403d4eb085e62b7feff":"{\"type\":\"tool_search_tool_result\",\"tool_use_id\":\"srvtoolu_u3\",\"content\":{\"type\":\"tool_search_tool_search_result\",\"tool_references\":[{\"type\":\"tool_reference\",\"tool_name\":\"alpha__lookup\"},{\"type\":\"tool_reference\",\"tool_name\":\"zeta__fetch\"}]}}","599681318b87a21803f08ccab754793d5c0c6488a218bb8e9e5c799e542c4987":"{\"source_digest\":\"cdb4bed384771da19753940efef285e93976094fd6a634473a0c20d87acf749a\",\"active_digest\":\"a2386459883db027fb29e12362319be4ef39700a73c9ab336b45adfef54f8c09\",\"groups\":[{\"key\":\"u3-legacy-fixture:1:1:0\",\"family\":\"Messages\",\"execution\":\"server\",\"origin_ref\":{\"kind\":\"producing_binding\",\"byte_len\":131,\"digest\":\"32e162bfc7031d849bf853e7dc65e0118dfd1daa57d762de40060eef8844a28f\"},\"validated_join\":\"srvtoolu_u3\",\"keyless_server_fifo\":false,\"members\":[{\"lineage\":\"mock\",\"producing_binding\":\"u3-legacy-fixture\",\"turn\":1,\"item\":0,\"response\":0,\"block\":0,\"submember\":null,\"occurrence\":0},{\"lineage\":\"mock\",\"producing_binding\":\"u3-legacy-fixture\",\"turn\":1,\"item\":0,\"response\":0,\"block\":1,\"submember\":null,\"occurrence\":1}],\"duplicate_observations\":[],\"state\":\"Completed\"}],\"lifecycle\":{\"exchanges\":{\"u3-legacy-fixture\":\"Terminal\"},\"groups\":{\"u3-legacy-fixture:1:1:0\":\"Completed\"},\"waiting_clients\":[],\"terminal\":{}},\"accounting\":[{\"source\":{\"lineage\":\"mock\",\"producing_binding\":\"u3-legacy-fixture\",\"turn\":1,\"item\":0,\"response\":0,\"block\":0,\"submember\":null,\"occurrence\":0},\"source_digest\":\"cdb4bed384771da19753940efef285e93976094fd6a634473a0c20d87acf749a\",\"view_item\":null,\"view_digest\":null,\"role\":\"SourceOnly\",\"source_subrange\":null},{\"source\":{\"lineage\":\"mock\",\"producing_binding\":\"u3-legacy-fixture\",\"turn\":1,\"item\":0,\"response\":0,\"block\":1,\"submember\":null,\"occurrence\":1},\"source_digest\":\"cdb4bed384771da19753940efef285e93976094fd6a634473a0c20d87acf749a\",\"view_item\":null,\"view_digest\":null,\"role\":\"SourceOnly\",\"source_subrange\":null}],\"usage\":[{\"request\":\"u3-legacy-fixture\",\"lease\":1,\"attempt\":1,\"response_occurrence\":0,\"continuation\":0,\"observed\":{\"input_tokens\":11,\"output_tokens\":7},\"source_stop_reason\":\"end_turn\",\"outcome\":\"Accepted\",\"latency_ms\":1,\"http_dispatches\":1}],\"observations\":[{\"request\":\"u3-legacy-fixture\",\"lease\":1,\"attempt\":1,\"digest\":\"e2789a08fd081abd3b049eb3fbbafb578d583ac99238b0b8d4da422a09881bad\"}],\"transitions\":[]}","a2386459883db027fb29e12362319be4ef39700a73c9ab336b45adfef54f8c09":"{\"projection_revision\":0,\"view_links\":[],\"dispositions\":{},\"loaded_invalidations\":[]}","cdb4bed384771da19753940efef285e93976094fd6a634473a0c20d87acf749a":"{\"schema_version\":1,\"lineage\":\"mock\",\"source_revision\":1,\"producing_bindings\":[{\"request\":\"u3-legacy-fixture\",\"generation\":null,\"origin\":{\"status\":\"Unknown\",\"reason\":\"producing identity unavailable before U3\"},\"origin_ref\":{\"kind\":\"producing_binding\",\"byte_len\":131,\"digest\":\"32e162bfc7031d849bf853e7dc65e0118dfd1daa57d762de40060eef8844a28f\"}}],\"responses\":[{\"occurrence\":0,\"request\":\"u3-legacy-fixture\",\"lease\":1,\"attempt\":1,\"continuation\":0,\"provider_response_id\":\"msg_u3\",\"source_stop_reason\":\"end_turn\",\"block_start\":0,\"block_end\":2,\"stream_complete\":true,\"status\":\"Accepted\"}],\"ordered_blocks\":[{\"coordinates\":{\"lineage\":\"mock\",\"producing_binding\":\"u3-legacy-fixture\",\"turn\":1,\"item\":0,\"response\":0,\"block\":0,\"submember\":null,\"occurrence\":0},\"wire_index\":0,\"arrival_ordinal\":0,\"block\":{\"kind\":\"raw_block\",\"byte_len\":104,\"digest\":\"d7bcaf820f5be89d8c8060c00e406ada1a63dd0d73c36ef2d787b71f9b28eb52\"},\"partial\":null,\"state\":\"ClosedFaithful\",\"eligible_ref\":null,\"eligible_unavailable\":\"UnknownIdentity\"},{\"coordinates\":{\"lineage\":\"mock\",\"producing_binding\":\"u3-legacy-fixture\",\"turn\":1,\"item\":0,\"response\":0,\"block\":1,\"submember\":null,\"occurrence\":1},\"wire_index\":1,\"arrival_ordinal\":1,\"block\":{\"kind\":\"raw_block\",\"byte_len\":240,\"digest\":\"34469c5546072d7cce326a0d7bd71814edbd22e0753ec403d4eb085e62b7feff\"},\"partial\":null,\"state\":\"ClosedFaithful\",\"eligible_ref\":null,\"eligible_unavailable\":\"UnknownIdentity\"}],\"eligible_snapshots\":[],\"older_sources\":[]}","d7bcaf820f5be89d8c8060c00e406ada1a63dd0d73c36ef2d787b71f9b28eb52":"{\"type\":\"server_tool_use\",\"id\":\"srvtoolu_u3\",\"name\":\"tool_search_tool_regex\",\"input\":{\"query\":\"lookup\"}}"}}"####;
    struct U4Fixture {
        record: crate::conversation::native_search_history::NativeSearchHistoryV1,
        source: crate::conversation::native_search_history::NativeSource,
        ledger: crate::conversation::native_search_history::NativeLedger,
        identity: crate::conversation::native_search_history::NativeIdentityExtension,
        blobs: std::collections::BTreeMap<String, std::sync::Arc<[u8]>>,
        tools: Vec<crate::ToolSpec>,
        inputs: crate::conversation::native_search_history::NativeEligibleInputs,
        authorship: std::collections::BTreeMap<
            u64,
            crate::conversation::native_search_history::NativeIdAuthorship,
        >,
    }

    impl U4Fixture {
        fn history(&self) -> NativePureHistory<'_> {
            NativePureHistory {
                record: &self.record,
                source: &self.source,
                ledger: &self.ledger,
                identity: &self.identity,
                blobs: &self.blobs,
                authorship: &self.authorship,
            }
        }

        fn produced() -> Self {
            use crate::conversation::native_search_history::*;
            let payload: Value = serde_json::from_str(U4_PRODUCED).unwrap();
            let record: NativeSearchHistoryV1 =
                serde_json::from_str(payload["record"].as_str().unwrap()).unwrap();
            let blobs: std::collections::BTreeMap<String, std::sync::Arc<[u8]>> = payload["blobs"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(digest, value)| {
                    let bytes = value.as_str().unwrap().as_bytes();
                    assert_eq!(
                        &hex_digest(&sha256(bytes)),
                        digest,
                        "U4 must consume exact U3 bytes"
                    );
                    (digest.clone(), std::sync::Arc::from(bytes))
                })
                .collect();
            let (source, links) =
                serde_json::from_slice::<NativeSourceV2>(&blobs[&record.source.digest])
                    .unwrap()
                    .into_parts();
            let (ledger, witnesses) =
                serde_json::from_slice::<NativeLedgerV2>(&blobs[&record.ledger.digest])
                    .unwrap()
                    .into_parts();
            let names = ["alpha__lookup", "zeta__fetch"];
            let descriptions = ["Typed lookup declaration", "Typed fetch declaration"];
            let mut entries = Vec::new();
            let mut tools = Vec::new();
            for (position, reference) in source.eligible_snapshots.iter().enumerate() {
                let snapshot: NativeEligibleSnapshot =
                    serde_json::from_slice(&blobs[&reference.digest]).unwrap();
                let spec = crate::ToolSpec {
                    name: names[position].into(),
                    description: Some(descriptions[position].into()),
                    parameters: serde_json::from_slice(&blobs[&snapshot.schema.digest]).unwrap(),
                    exposure: crate::ToolExposure::Deferred,
                };
                entries.push(
                    NativeEligibleEntry::checked(
                        position,
                        snapshot.namespace_kind,
                        snapshot.qualified_namespace,
                        snapshot.child_name,
                        spec.clone(),
                        blobs[&snapshot.schema.digest].clone(),
                        snapshot.schema.digest,
                    )
                    .unwrap(),
                );
                tools.push(spec);
            }
            let inputs = NativeEligibleInputs::checked(entries, tools.len()).unwrap();
            let authorship = std::collections::BTreeMap::from([
                (0, NativeIdAuthorship::Provider),
                (1, NativeIdAuthorship::Provider),
            ]);
            Self {
                record,
                source,
                ledger,
                identity: NativeIdentityExtension::Revision2 {
                    eligible_member_links: links,
                    eligible_member_accounting: witnesses,
                },
                blobs,
                tools,
                inputs,
                authorship,
            }
        }

        fn shape(&self) -> NativeReplayShape {
            use crate::conversation::native_search_history::*;
            let raw: Value =
                serde_json::from_slice(&self.blobs[&self.source.ordered_blocks[1].block.digest])
                    .unwrap();
            let item = crate::conversation::tool_search::ToolSearchItem::from_wire(raw).unwrap();
            let output = native_item_shape(&item, self.authorship[&1])
                .expect("U4 shape preserves original required join");
            NativeReplayShape {
                family: output.family,
                keyless_server: output.keyless_server,
                output_id_present: output.item_id_present,
                completed: true,
                result_error: output.result_error,
                referenced_members: native_candidate_references(item.raw()).unwrap().len() as u64,
            }
        }
    }

    impl U4Fixture {
        fn put(
            &mut self,
            kind: &str,
            value: &impl serde::Serialize,
        ) -> crate::conversation::native_search_history::NativeBlobRef {
            let bytes = serde_json::to_vec(value).unwrap();
            let reference = crate::conversation::native_search_history::NativeBlobRef {
                kind: kind.into(),
                byte_len: bytes.len() as u64,
                digest: hex_digest(&sha256(&bytes)),
            };
            self.blobs.insert(reference.digest.clone(), bytes.into());
            reference
        }

        fn reseal_shape(&mut self) {
            use crate::conversation::native_search_history::*;
            let source = match &self.identity {
                NativeIdentityExtension::Revision1Unknown => {
                    serde_json::to_value(&self.source).unwrap()
                }
                NativeIdentityExtension::Revision2 {
                    eligible_member_links,
                    ..
                } => serde_json::to_value(NativeSourceV2::from_parts(
                    self.source.clone(),
                    eligible_member_links.clone(),
                ))
                .unwrap(),
            };
            self.record.source = self.put("source", &source);
            self.record.lineage.source_digest = self.record.source.digest.clone();
            self.ledger.source_digest = self.record.source.digest.clone();
            for entry in &mut self.ledger.accounting {
                entry.source_digest = self.record.source.digest.clone();
            }
            if let NativeIdentityExtension::Revision2 {
                eligible_member_accounting,
                ..
            } = &mut self.identity
            {
                for entry in eligible_member_accounting {
                    entry.source_digest = self.record.source.digest.clone();
                }
            }
            let ledger = match &self.identity {
                NativeIdentityExtension::Revision1Unknown => {
                    serde_json::to_value(&self.ledger).unwrap()
                }
                NativeIdentityExtension::Revision2 {
                    eligible_member_accounting,
                    ..
                } => serde_json::to_value(NativeLedgerV2::from_parts(
                    self.ledger.clone(),
                    eligible_member_accounting.clone(),
                ))
                .unwrap(),
            };
            self.record.ledger = self.put("ledger", &ledger);
        }

        fn with_family(
            family: crate::conversation::native_search_history::NativeReplayFamily,
            keyless: bool,
        ) -> Self {
            use crate::conversation::native_search_history::*;
            use crate::conversation::tool_search::ToolSearchItem;
            let mut fixture = Self::produced();
            if family == NativeReplayFamily::MessagesServer {
                return fixture;
            }
            let execution = if family == NativeReplayFamily::ResponsesClient {
                "client"
            } else {
                "server"
            };
            let mut call = serde_json::json!({"type":"tool_search_call","id":"tsc_provider_u4","call_id":"call_u4","execution":execution,"status":"completed","arguments":{"query":"lookup"}});
            let tools = fixture.tools.iter().map(|spec| serde_json::json!({"type":"function","name":spec.name,"description":spec.description,"parameters":spec.parameters})).collect();
            let mut output = if family == NativeReplayFamily::ResponsesClient {
                fixture
                    .authorship
                    .insert(1, NativeIdAuthorship::ClientOutput);
                ToolSearchItem::client_answer("call_u4", tools)
                    .raw()
                    .clone()
            } else {
                serde_json::json!({"type":"tool_search_output","id":"tso_provider_u4","call_id":"call_u4","execution":"server","status":"completed","tools":tools})
            };
            if keyless {
                call.as_object_mut().unwrap().remove("call_id");
                output.as_object_mut().unwrap().remove("call_id");
            }
            fixture.source.ordered_blocks[0].block = fixture.put("raw_block", &call);
            fixture.source.ordered_blocks[1].block = fixture.put("raw_block", &output);
            fixture.ledger.groups[0].family = "Responses".into();
            fixture.ledger.groups[0].execution = execution.into();
            fixture.ledger.groups[0].validated_join = (!keyless).then(|| "call_u4".into());
            fixture.ledger.groups[0].keyless_server_fifo = keyless;
            fixture.reseal_shape();
            fixture
        }
    }

    fn u4_domain(model: &str, backend: ApiBackend) -> NativeReplayDomain {
        let dialect = if backend == ApiBackend::Messages {
            NativeReplayDialect::Messages
        } else {
            NativeReplayDialect::Responses(crate::conversation::ResponsesReplayDialect::Codex)
        };
        NativeReplayDomain {
            backend,
            dialect,
            model: model.into(),
            row: model.into(),
            provider: "offline-provider".into(),
            deployment: "offline-deployment".into(),
            deployment_revision: "revision-1".into(),
            endpoint: "https://offline.invalid/v1".into(),
            principal: "nonsecret-test-tenant".into(),
        }
    }

    fn u4_qualification(
        fixture: &U4Fixture,
        model: &str,
        target: &TargetRoute,
        from: &DiscoveryOrigin,
    ) -> NativeQualification {
        use crate::conversation::native_search_history::*;
        let shape = fixture.shape();
        let source_backend = if shape.family == NativeReplayFamily::MessagesServer {
            ApiBackend::Messages
        } else {
            ApiBackend::Responses
        };
        let domain = u4_domain(model, target.backend());
        let history = fixture.history();
        native_policy_history(&history).unwrap();
        let mut backing = true;
        let mut faithful = true;
        for block in &fixture.source.ordered_blocks {
            let raw: Value = serde_json::from_slice(&fixture.blobs[&block.block.digest]).unwrap();
            backing &=
                native_policy_backing(&history, block, &raw, &fixture.tools, Some(&fixture.inputs))
                    .unwrap();
            let item = crate::conversation::tool_search::ToolSearchItem::from_wire(raw).unwrap();
            faithful &= native_item_shape(&item, fixture.authorship[&block.coordinates.occurrence])
                .unwrap()
                .faithful_identity;
        }
        // Only identity bytes are producer evidence; route/domain/shape qualification is offline.
        NativeQualification {
            source: from
                .owner_model_id
                .as_deref()
                .map(|model| u4_domain(model, source_backend)),
            target: Some(domain.clone()),
            request: Some(domain.clone()),
            original_admission: target.admission(),
            receipt: Some(NativeShapeReceipt {
                target: domain,
                shape: shape.clone(),
                receipt_id: "offline-qualified-shape".into(),
            }),
            shape,
            intact_membership: true,
            current_backing: backing,
            faithful_identity: faithful,
        }
    }

    fn u4_qualified_fixture(
        model: &str,
        target: &TargetRoute,
        from: &DiscoveryOrigin,
    ) -> NativeQualification {
        let fixture = U4Fixture::with_family(
            crate::conversation::native_search_history::NativeReplayFamily::ResponsesClient,
            false,
        );
        u4_qualification(&fixture, model, target, from)
    }

    fn u4_prepare(
        fixture: &U4Fixture,
        model: &str,
        target: &TargetRoute,
        from: &DiscoveryOrigin,
        qualified: NativeQualification,
    ) -> Result<NativeHistoryDecision, crate::conversation::native_search_history::NativeUnavailable>
    {
        let mut qualified = Some(qualified);
        prepare_native_history(
            &fixture.history(),
            model,
            target,
            &fixture.tools,
            Some(&fixture.inputs),
            |_| (from.clone(), qualified.take().unwrap()),
        )
    }

    fn u4_assert_action(
        fixture: &U4Fixture,
        decision: &NativeHistoryDecision,
        action: NativeHistoryAction,
    ) {
        use crate::conversation::native_search_history::NativeIdentityExtension;
        assert_eq!(decision.groups.len(), 1);
        assert_eq!(
            decision.groups[0].action, action,
            "U4 complete-group policy action must match qualification"
        );
        assert_eq!(decision.members.len(), 2);
        for member in &decision.members {
            assert_eq!(
                member.action, action,
                "one whole-group action, never a half KEEP"
            );
            let block = fixture
                .source
                .ordered_blocks
                .iter()
                .find(|block| block.coordinates == member.source)
                .unwrap();
            let raw: Value = serde_json::from_slice(&fixture.blobs[&block.block.digest]).unwrap();
            assert_eq!(
                member.raw,
                (action == NativeHistoryAction::Keep).then_some(raw),
                "native output never mints IDs or leaks host coordinates"
            );
        }
        if let NativeIdentityExtension::Revision2 {
            eligible_member_links,
            ..
        } = &fixture.identity
        {
            for link in eligible_member_links {
                let mut parent = link.source.clone();
                parent.submember = None;
                assert_eq!(
                    decision
                        .members
                        .iter()
                        .find(|member| member.source == parent)
                        .unwrap()
                        .action,
                    action,
                    "every A1 reference inherits its raw parent's action"
                );
            }
        }
    }

    #[test]
    fn native_history_qualified_matrix_and_r1() {
        use crate::conversation::native_search_history::*;
        let model = "u4-model";
        for (family, keyless) in [
            (NativeReplayFamily::ResponsesClient, false),
            (NativeReplayFamily::ResponsesServer, false),
            (NativeReplayFamily::ResponsesServer, true),
            (NativeReplayFamily::MessagesServer, false),
        ] {
            let fixture = U4Fixture::with_family(family, keyless);
            let source_before = serde_json::to_value(&fixture.source).unwrap();
            let backend = if family == NativeReplayFamily::MessagesServer {
                ApiBackend::Messages
            } else {
                ApiBackend::Responses
            };
            let boundary = if backend == ApiBackend::Messages {
                Boundary::Vertex
            } else {
                Boundary::AzStrict
            };
            let target = route(backend.clone(), boundary, ADMITTED);
            let from = origin(boundary, Some(model));
            let qualified = u4_qualification(&fixture, model, &target, &from);
            let decision = u4_prepare(&fixture, model, &target, &from, qualified).unwrap();
            u4_assert_action(&fixture, &decision, NativeHistoryAction::Keep);
            let recreated = U4Fixture::with_family(family, keyless);
            let second = u4_prepare(
                &recreated,
                model,
                &target,
                &from,
                u4_qualification(&recreated, model, &target, &from),
            )
            .unwrap();
            assert_eq!(
                second, decision,
                "identical-byte current registrations require no occurrence continuity"
            );
            for other in [
                ApiBackend::Responses,
                ApiBackend::Messages,
                ApiBackend::ChatCompletions,
            ] {
                let boundary = if other == ApiBackend::Messages {
                    Boundary::Vertex
                } else {
                    Boundary::AzStrict
                };
                let target = route(other, boundary, ADMITTED);
                let decision = u4_prepare(
                    &fixture,
                    "other-row",
                    &target,
                    &from,
                    u4_qualification(&fixture, "other-row", &target, &from),
                )
                .unwrap();
                u4_assert_action(&fixture, &decision, NativeHistoryAction::F);
            }
            assert_eq!(
                serde_json::to_value(&fixture.source).unwrap(),
                source_before,
                "policy never rewrites retained source"
            );
        }
        let fixture = U4Fixture::with_family(NativeReplayFamily::ResponsesClient, false);
        let target = route(ApiBackend::Responses, Boundary::AzStrict, ADMITTED);
        let from = origin(Boundary::AzStrict, Some(model));
        let mut unknown = u4_qualification(&fixture, model, &target, &from);
        unknown.source = None;
        assert_eq!(
            discovery_tier(model, &target, &from, &unknown),
            DiscoveryTier::Demote,
            "U4 legacy boundary/model attribution cannot qualify native replay without domain and identity evidence"
        );
        u4_assert_action(
            &fixture,
            &u4_prepare(&fixture, model, &target, &from, unknown).unwrap(),
            NativeHistoryAction::F,
        );
        for field in [
            "row",
            "provider",
            "deployment",
            "revision",
            "endpoint",
            "principal",
        ] {
            let mut qualified = u4_qualification(&fixture, model, &target, &from);
            let source = qualified.source.as_mut().unwrap();
            match field {
                "row" => source.row.push_str("-foreign"),
                "provider" => source.provider.push_str("-foreign"),
                "deployment" => source.deployment.push_str("-foreign"),
                "revision" => source.deployment_revision.push_str("-foreign"),
                "endpoint" => source.endpoint.push_str("/foreign"),
                "principal" => source.principal.push_str("-foreign"),
                _ => unreachable!(),
            }
            assert_eq!(
                discovery_tier(model, &target, &from, &qualified),
                DiscoveryTier::Demote,
                "exact producing-domain mismatch: {field}"
            );
        }
        for fault in [
            "unknown-target",
            "unknown-request",
            "request-mismatch",
            "model-mismatch",
            "admission-mismatch",
        ] {
            let mut qualified = u4_qualification(&fixture, model, &target, &from);
            match fault {
                "unknown-target" => {
                    qualified.target = None;
                    qualified.source = None;
                }
                "unknown-request" => qualified.request = None,
                "request-mismatch" => qualified
                    .request
                    .as_mut()
                    .unwrap()
                    .deployment
                    .push_str("-different"),
                "model-mismatch" => qualified
                    .target
                    .as_mut()
                    .unwrap()
                    .model
                    .push_str("-different"),
                "admission-mismatch" => qualified.original_admission = ROW_REJECTS,
                _ => unreachable!(),
            }
            assert_eq!(
                discovery_tier(model, &target, &from, &qualified),
                DiscoveryTier::Refuse,
                "independently unknown or incoherent target refuses: {fault}"
            );
        }
        for fault in [
            "no-receipt",
            "wrong-shape",
            "pending-receipt",
            "error-shape",
            "member-count-shape",
            "unsupported-dialect",
        ] {
            let mut qualified = u4_qualification(&fixture, model, &target, &from);
            match fault {
                "no-receipt" => qualified.receipt = None,
                "wrong-shape" => {
                    qualified.receipt.as_mut().unwrap().shape.family =
                        NativeReplayFamily::ResponsesServer
                }
                "pending-receipt" => qualified.receipt.as_mut().unwrap().shape.completed = false,
                "error-shape" => qualified.receipt.as_mut().unwrap().shape.result_error = true,
                "member-count-shape" => {
                    qualified.receipt.as_mut().unwrap().shape.referenced_members = 1
                }
                "unsupported-dialect" => {
                    qualified.target.as_mut().unwrap().dialect = NativeReplayDialect::Responses(
                        crate::conversation::ResponsesReplayDialect::Other,
                    );
                    qualified.source.as_mut().unwrap().dialect =
                        qualified.target.as_ref().unwrap().dialect.clone();
                    qualified.request = qualified.target.clone();
                    qualified.receipt.as_mut().unwrap().target = qualified.target.clone().unwrap();
                }
                _ => unreachable!(),
            }
            assert_eq!(
                discovery_tier(model, &target, &from, &qualified),
                DiscoveryTier::Demote,
                "route-and-shape qualification is exact: {fault}"
            );
        }
        for (backend, boundary, admission) in [
            (ApiBackend::Messages, Boundary::Vertex, ROW_REJECTS),
            (ApiBackend::Responses, Boundary::Vertex, ADMITTED),
            (ApiBackend::Messages, Boundary::AzStrict, ADMITTED),
            (ApiBackend::Responses, Boundary::AzStrict, ROW_REJECTS),
        ] {
            let target = route(backend, boundary, admission);
            for from in [
                origin(boundary, Some(model)),
                origin(Boundary::VLLenient, Some("other-row")),
                origin(boundary, None),
            ] {
                let qualified = u4_qualified_fixture(model, &target, &from);
                assert_eq!(
                    discovery_tier(model, &target, &from, &qualified),
                    DiscoveryTier::Demote,
                    "all five coherent R-1 cells use F"
                );
            }
        }
        for missing in [false, true] {
            let mut partial = U4Fixture::produced();
            if missing {
                partial.tools.pop();
                partial.inputs = partial.inputs.select(&[0], &partial.tools).unwrap();
            } else {
                let NativeIdentityExtension::Revision2 {
                    eligible_member_links,
                    ..
                } = &mut partial.identity
                else {
                    unreachable!()
                };
                eligible_member_links[1].identity = NativeEligibleMemberIdentity::Unavailable {
                    reason: NativeUnavailable::UnknownIdentity,
                };
                partial.source.ordered_blocks[1].eligible_unavailable =
                    Some(NativeUnavailable::UnknownIdentity);
                partial.reseal_shape();
            }
            let target = route(ApiBackend::Messages, Boundary::Vertex, ADMITTED);
            let from = origin(Boundary::Vertex, Some(model));
            let decision = u4_prepare(
                &partial,
                model,
                &target,
                &from,
                u4_qualification(&partial, model, &target, &from),
            )
            .unwrap();
            u4_assert_action(&partial, &decision, NativeHistoryAction::F);
        }
        for field in ["schema", "description", "name"] {
            let mut changed = U4Fixture::produced();
            match field {
                "schema" => {
                    changed.tools[1].parameters["properties"]["limit"]["default"] =
                        serde_json::json!(8)
                }
                "description" => changed.tools[1].description = Some("changed declaration".into()),
                "name" => changed.tools[1].name = "different__fetch".into(),
                _ => unreachable!(),
            }
            changed.inputs = changed.inputs.select(&[0, 1], &changed.tools).unwrap();
            let target = route(ApiBackend::Messages, Boundary::Vertex, ADMITTED);
            let from = origin(Boundary::Vertex, Some(model));
            u4_assert_action(
                &changed,
                &u4_prepare(
                    &changed,
                    model,
                    &target,
                    &from,
                    u4_qualification(&changed, model, &target, &from),
                )
                .unwrap(),
                NativeHistoryAction::F,
            );
        }
        for id in [
            Value::Null,
            serde_json::json!(""),
            serde_json::json!(7),
            serde_json::json!("call_u4"),
            serde_json::json!("digest-derived-id"),
        ] {
            let mut bad = U4Fixture::with_family(NativeReplayFamily::ResponsesClient, false);
            let mut raw: Value =
                serde_json::from_slice(&bad.blobs[&bad.source.ordered_blocks[1].block.digest])
                    .unwrap();
            raw["id"] = id;
            bad.source.ordered_blocks[1].block = bad.put("raw_block", &raw);
            bad.reseal_shape();
            u4_assert_action(
                &bad,
                &u4_prepare(
                    &bad,
                    model,
                    &target,
                    &from,
                    u4_qualification(&bad, model, &target, &from),
                )
                .unwrap(),
                NativeHistoryAction::F,
            );
        }
        for join in [
            None,
            Some(Value::Null),
            Some(serde_json::json!("")),
            Some(serde_json::json!(7)),
        ] {
            let mut bad = U4Fixture::with_family(NativeReplayFamily::ResponsesClient, false);
            let qualified = u4_qualification(&bad, model, &target, &from);
            let mut raw: Value =
                serde_json::from_slice(&bad.blobs[&bad.source.ordered_blocks[1].block.digest])
                    .unwrap();
            raw.as_object_mut().unwrap().remove("call_id");
            if let Some(join) = join {
                raw["call_id"] = join;
            }
            bad.source.ordered_blocks[1].block = bad.put("raw_block", &raw);
            bad.reseal_shape();
            assert!(
                u4_prepare(&bad, model, &target, &from, qualified).is_err(),
                "required client joins never become keyless FIFO"
            );
        }
        for family in [
            NativeReplayFamily::ResponsesClient,
            NativeReplayFamily::ResponsesServer,
            NativeReplayFamily::MessagesServer,
        ] {
            for (count, error) in [(0, false), (1, false), (2, false), (0, true)] {
                let mut shaped = U4Fixture::with_family(family, false);
                let mut output: Value = serde_json::from_slice(
                    &shaped.blobs[&shaped.source.ordered_blocks[1].block.digest],
                )
                .unwrap();
                if family == NativeReplayFamily::MessagesServer {
                    if error {
                        output["content"] = serde_json::json!({"type":"tool_search_tool_result_error","error_code":"unavailable"});
                    } else {
                        output["content"]["tool_references"]
                            .as_array_mut()
                            .unwrap()
                            .truncate(count);
                    }
                } else {
                    output["tools"].as_array_mut().unwrap().truncate(count);
                    if error {
                        output["status"] = serde_json::json!("error");
                    }
                }
                let NativeIdentityExtension::Revision2 {
                    eligible_member_links,
                    eligible_member_accounting,
                } = &mut shaped.identity
                else {
                    unreachable!()
                };
                eligible_member_links.truncate(count);
                eligible_member_accounting.truncate(count);
                shaped.source.ordered_blocks[1].block = shaped.put("raw_block", &output);
                shaped.reseal_shape();
                let backend = if family == NativeReplayFamily::MessagesServer {
                    ApiBackend::Messages
                } else {
                    ApiBackend::Responses
                };
                let boundary = if backend == ApiBackend::Messages {
                    Boundary::Vertex
                } else {
                    Boundary::AzStrict
                };
                let target = route(backend, boundary, ADMITTED);
                let from = origin(boundary, Some(model));
                u4_assert_action(
                    &shaped,
                    &u4_prepare(
                        &shaped,
                        model,
                        &target,
                        &from,
                        u4_qualification(&shaped, model, &target, &from),
                    )
                    .unwrap(),
                    NativeHistoryAction::Keep,
                );
            }
        }
        let mut namespaced = U4Fixture::with_family(NativeReplayFamily::ResponsesClient, false);
        let mut output: Value = serde_json::from_slice(
            &namespaced.blobs[&namespaced.source.ordered_blocks[1].block.digest],
        )
        .unwrap();
        let tools = output["tools"].as_array_mut().unwrap();
        for (index, (namespace, child)) in [("alpha", "lookup"), ("zeta", "fetch")]
            .into_iter()
            .enumerate()
        {
            let mut function = tools[index].clone();
            function["name"] = serde_json::json!(child);
            tools[index] =
                serde_json::json!({"type":"namespace","name":namespace,"tools":[function]});
        }
        namespaced.source.ordered_blocks[1].block = namespaced.put("raw_block", &output);
        namespaced.reseal_shape();
        u4_assert_action(
            &namespaced,
            &u4_prepare(
                &namespaced,
                model,
                &target,
                &from,
                u4_qualification(&namespaced, model, &target, &from),
            )
            .unwrap(),
            NativeHistoryAction::Keep,
        );
        let mut namespace_mismatch = U4Fixture::produced();
        let original = &namespace_mismatch.inputs.entries()[1];
        let changed = NativeEligibleEntry::checked(
            1,
            NativeNamespaceKind::GrokBuild,
            "grok_build".into(),
            original.declaration().name.clone(),
            original.declaration().clone(),
            original.schema_bytes().into(),
            original.schema_digest().into(),
        )
        .unwrap();
        namespace_mismatch.inputs = NativeEligibleInputs::checked(
            vec![namespace_mismatch.inputs.entries()[0].clone(), changed],
            namespace_mismatch.tools.len(),
        )
        .unwrap();
        let messages = route(ApiBackend::Messages, Boundary::Vertex, ADMITTED);
        let messages_from = origin(Boundary::Vertex, Some(model));
        u4_assert_action(
            &namespace_mismatch,
            &u4_prepare(
                &namespace_mismatch,
                model,
                &messages,
                &messages_from,
                u4_qualification(&namespace_mismatch, model, &messages, &messages_from),
            )
            .unwrap(),
            NativeHistoryAction::F,
        );
        let mut wrong_profile = U4Fixture::with_family(NativeReplayFamily::ResponsesClient, false);
        let qualified = u4_qualification(&wrong_profile, model, &target, &from);
        wrong_profile.record.reader_floor = 1;
        assert_eq!(
            u4_prepare(&wrong_profile, model, &target, &from, qualified),
            Err(NativeUnavailable::UnsupportedVersion)
        );
        for fault in ["length", "digest", "kind"] {
            let mut malformed = U4Fixture::with_family(NativeReplayFamily::ResponsesClient, false);
            let qualified = u4_qualification(&malformed, model, &target, &from);
            let NativeIdentityExtension::Revision2 {
                eligible_member_links,
                ..
            } = &mut malformed.identity
            else {
                unreachable!()
            };
            let NativeEligibleMemberIdentity::Backed { eligible_ref } =
                &mut eligible_member_links[1].identity
            else {
                unreachable!()
            };
            match fault {
                "length" => eligible_ref.byte_len += 1,
                "digest" => eligible_ref.digest = "a".repeat(64),
                "kind" => eligible_ref.kind = "raw_block".into(),
                _ => unreachable!(),
            }
            malformed.source.eligible_snapshots[1] = eligible_ref.clone();
            malformed.reseal_shape();
            assert!(
                u4_prepare(&malformed, model, &target, &from, qualified).is_err(),
                "invalid immutable identity reference refuses: {fault}"
            );
        }
        for fault in ["nonterminal-output", "unsupported-arguments"] {
            let mut malformed = U4Fixture::with_family(NativeReplayFamily::ResponsesClient, false);
            let qualified = u4_qualification(&malformed, model, &target, &from);
            let index = usize::from(fault == "nonterminal-output");
            let mut raw: Value = serde_json::from_slice(
                &malformed.blobs[&malformed.source.ordered_blocks[index].block.digest],
            )
            .unwrap();
            if fault == "nonterminal-output" {
                raw["status"] = serde_json::json!("in_progress");
            } else {
                raw["arguments"] = serde_json::json!("unmodeled arguments");
            }
            malformed.source.ordered_blocks[index].block = malformed.put("raw_block", &raw);
            malformed.reseal_shape();
            assert!(
                u4_prepare(&malformed, model, &target, &from, qualified).is_err(),
                "unsupported completed-group shape must refuse: {fault}"
            );
        }
        let mut legacy = U4Fixture::produced();
        let payload: Value = serde_json::from_str(U4_PRODUCED).unwrap();
        legacy.record = serde_json::from_str(payload["legacy_record"].as_str().unwrap()).unwrap();
        for (digest, value) in payload["legacy_blobs"].as_object().unwrap() {
            legacy.blobs.insert(
                digest.clone(),
                std::sync::Arc::from(value.as_str().unwrap().as_bytes()),
            );
        }
        legacy.source =
            serde_json::from_slice(&legacy.blobs[&legacy.record.source.digest]).unwrap();
        legacy.ledger =
            serde_json::from_slice(&legacy.blobs[&legacy.record.ledger.digest]).unwrap();
        legacy.identity = NativeIdentityExtension::Revision1Unknown;
        let messages = route(ApiBackend::Messages, Boundary::Vertex, ADMITTED);
        let messages_from = origin(Boundary::Vertex, Some(model));
        u4_assert_action(
            &legacy,
            &u4_prepare(
                &legacy,
                model,
                &messages,
                &messages_from,
                u4_qualification(&legacy, model, &messages, &messages_from),
            )
            .unwrap(),
            NativeHistoryAction::F,
        );
        let mut ambiguous = U4Fixture::produced();
        let qualified = u4_qualification(&ambiguous, model, &messages, &messages_from);
        ambiguous.ledger.groups[0].members[1] = ambiguous.ledger.groups[0].members[0].clone();
        ambiguous.reseal_shape();
        assert!(
            u4_prepare(&ambiguous, model, &messages, &messages_from, qualified).is_err(),
            "ambiguous membership refuses instead of concealing a guessed group with F"
        );
        let mut unknown_ids = U4Fixture::with_family(NativeReplayFamily::ResponsesServer, false);
        unknown_ids
            .authorship
            .insert(0, NativeIdAuthorship::Unknown);
        unknown_ids
            .authorship
            .insert(1, NativeIdAuthorship::ClientMinted);
        u4_assert_action(
            &unknown_ids,
            &u4_prepare(
                &unknown_ids,
                model,
                &target,
                &from,
                u4_qualification(&unknown_ids, model, &target, &from),
            )
            .unwrap(),
            NativeHistoryAction::F,
        );
    }

    fn u5_duplicate_fixture() -> U4Fixture {
        use crate::conversation::native_search_history::NativeIdAuthorship;
        let mut fixture = U4Fixture::produced();
        let mut duplicate = fixture.source.ordered_blocks[0].clone();
        duplicate.coordinates.occurrence = 2;
        duplicate.coordinates.block = 2;
        duplicate.wire_index = 2;
        duplicate.arrival_ordinal = 2;
        fixture.source.ordered_blocks.push(duplicate.clone());
        fixture.source.responses[0].block_end = 3;
        let mut accounting = fixture.ledger.accounting[0].clone();
        accounting.source = duplicate.coordinates.clone();
        fixture.ledger.accounting.push(accounting);
        fixture.ledger.groups[0]
            .members
            .push(duplicate.coordinates.clone());
        fixture.ledger.groups[0].duplicate_observations.push((
            duplicate.coordinates,
            fixture.source.ordered_blocks[0].coordinates.clone(),
        ));
        fixture.authorship.insert(2, NativeIdAuthorship::Provider);
        fixture.reseal_shape();
        fixture
    }

    #[test]
    fn native_history_whole_history_group_conservation() {
        let fixture = u5_duplicate_fixture();
        let target = route(ApiBackend::Messages, Boundary::Vertex, ADMITTED);
        let from = origin(Boundary::Vertex, Some("u5-model"));
        let qualified = u4_qualification(&fixture, "u5-model", &target, &from);
        let decision = u4_prepare(&fixture, "u5-model", &target, &from, qualified)
            .expect("U5 must account for the complete group and its verified duplicate");
        assert_eq!(decision.members.len(), 3);
        assert_eq!(
            decision
                .members
                .iter()
                .filter(|member| member.raw.is_some())
                .count(),
            2,
            "verified duplicates keep a source witness without another native emission"
        );
        u5_group_cases();
    }

    #[test]
    fn native_history_lifecycle_never_reopens_completed_groups() {
        use crate::conversation::native_search_history::NativeActive;
        let mut fixture = U4Fixture::produced();
        let mut active: NativeActive =
            serde_json::from_slice(&fixture.blobs[&fixture.record.active.digest]).unwrap();
        active
            .dispositions
            .insert(fixture.ledger.groups[0].key.clone(), "F".into());
        fixture.record.active = fixture.put("active", &active);
        fixture.ledger.active_digest = fixture.record.active.digest.clone();
        fixture.reseal_shape();
        let target = route(ApiBackend::Messages, Boundary::Vertex, ADMITTED);
        let from = origin(Boundary::Vertex, Some("u5-model"));
        let decision = u4_prepare(
            &fixture,
            "u5-model",
            &target,
            &from,
            u4_qualification(&fixture, "u5-model", &target, &from),
        )
        .unwrap();
        u4_assert_action(&fixture, &decision, NativeHistoryAction::F);
        assert_eq!(fixture.ledger.groups[0].state, "Completed");
        u5_lifecycle_cases();
    }

    fn u5_pending_fixture(
        family: crate::conversation::native_search_history::NativeReplayFamily,
        abandoned: bool,
    ) -> U4Fixture {
        use crate::conversation::native_search_history::*;
        let mut fixture = U4Fixture::with_family(family, false);
        fixture.source.ordered_blocks.truncate(1);
        fixture.source.responses[0].block_end = 1;
        fixture.ledger.accounting.truncate(1);
        fixture.ledger.groups[0].members.truncate(1);
        fixture.ledger.groups[0].state = if abandoned {
            "AbandonedPending"
        } else {
            "LivePending"
        }
        .into();
        fixture.ledger.lifecycle.groups.insert(
            fixture.ledger.groups[0].key.clone(),
            fixture.ledger.groups[0].state.clone(),
        );
        let response = &fixture.source.responses[0];
        fixture.ledger.lifecycle.exchanges.insert(
            response.request.clone(),
            if abandoned { "Terminal" } else { "LiveBound" }.into(),
        );
        if abandoned {
            fixture.ledger.lifecycle.terminal.insert(
                format!("{}:{}", response.request, response.attempt),
                NativeTerminalMarker {
                    outcome: NativeUnavailable::Cancelled,
                    last_accepted_response: Some(0),
                    observed_bytes: 100,
                    failed_block: None,
                },
            );
        }
        let NativeIdentityExtension::Revision2 {
            eligible_member_links,
            eligible_member_accounting,
        } = &mut fixture.identity
        else {
            unreachable!()
        };
        eligible_member_links.clear();
        eligible_member_accounting.clear();
        fixture.reseal_shape();
        fixture
    }

    fn u5_mixed_fixture() -> U4Fixture {
        use crate::conversation::native_search_history::*;
        let mut fixture = u5_pending_fixture(NativeReplayFamily::MessagesServer, false);
        for (index, id) in ["client_a", "client_b"].into_iter().enumerate() {
            let mut block = fixture.source.ordered_blocks[0].clone();
            let ordinal = index as u64 + 1;
            block.coordinates.occurrence = ordinal;
            block.coordinates.block = ordinal;
            block.wire_index = ordinal as u32;
            block.arrival_ordinal = ordinal;
            block.block = fixture.put("raw_block", &serde_json::json!({"type":"tool_use","id":id,"name":fixture.tools[index].name,"input":{}}));
            fixture.source.ordered_blocks.push(block.clone());
            let mut entry = fixture.ledger.accounting[0].clone();
            entry.source = block.coordinates.clone();
            fixture.ledger.accounting.push(entry);
            fixture
                .ledger
                .lifecycle
                .waiting_clients
                .push(NativeWaitingClient {
                    client_id: id.into(),
                    source: block.coordinates.clone(),
                    result: None,
                    delivery: "Waiting".into(),
                });
            let mut source = block.coordinates;
            source.submember = Some(0);
            let NativeIdentityExtension::Revision2 {
                eligible_member_links,
                eligible_member_accounting,
            } = &mut fixture.identity
            else {
                unreachable!()
            };
            eligible_member_links.push(NativeEligibleMemberLink {
                source: source.clone(),
                identity: NativeEligibleMemberIdentity::Backed {
                    eligible_ref: fixture.source.eligible_snapshots[index].clone(),
                },
            });
            eligible_member_accounting.push(NativeEligibleMemberAccounting {
                source,
                source_digest: fixture.record.source.digest.clone(),
            });
        }
        fixture.source.responses[0].block_end = 3;
        fixture.reseal_shape();
        fixture
    }

    fn u5_selected(
        fixture: &U4Fixture,
        selected: &[crate::conversation::native_search_history::NativeSourceCoordinate],
        pending: &crate::conversation::native_search_history::NativePendingContext,
    ) -> Result<NativeHistoryDecision, crate::conversation::native_search_history::NativeUnavailable>
    {
        use crate::conversation::native_search_history::*;
        let family = match fixture.ledger.groups[0].family.as_str() {
            "Messages" => NativeReplayFamily::MessagesServer,
            _ if fixture.ledger.groups[0].execution == "client" => {
                NativeReplayFamily::ResponsesClient
            }
            _ => NativeReplayFamily::ResponsesServer,
        };
        let complete = U4Fixture::with_family(family, fixture.ledger.groups[0].keyless_server_fifo);
        let backend = if family == NativeReplayFamily::MessagesServer {
            ApiBackend::Messages
        } else {
            ApiBackend::Responses
        };
        let boundary = if backend == ApiBackend::Messages {
            Boundary::Vertex
        } else {
            Boundary::AzStrict
        };
        let target = route(backend, boundary, ADMITTED);
        let from = origin(boundary, Some("u5-model"));
        prepare_native_history_selected(
            &fixture.history(),
            "u5-model",
            &target,
            &fixture.tools,
            Some(&fixture.inputs),
            selected,
            &Default::default(),
            pending,
            |_| {
                (
                    from.clone(),
                    u4_qualification(&complete, "u5-model", &target, &from),
                )
            },
        )
    }

    fn u5_two_genuine_fixture() -> U4Fixture {
        use crate::conversation::native_search_history::*;
        let mut fixture = U4Fixture::produced();
        let originals = fixture.source.ordered_blocks.clone();
        for (index, mut block) in originals.into_iter().enumerate() {
            block.coordinates.occurrence += 2;
            block.coordinates.response = 1;
            fixture.source.ordered_blocks.push(block.clone());
            let mut entry = fixture.ledger.accounting[index].clone();
            entry.source = block.coordinates;
            fixture.ledger.accounting.push(entry);
            fixture
                .authorship
                .insert(index as u64 + 2, NativeIdAuthorship::Provider);
        }
        let mut response = fixture.source.responses[0].clone();
        response.occurrence = 1;
        response.attempt = 2;
        response.block_start = 2;
        response.block_end = 4;
        response.provider_response_id = Some(serde_json::json!("msg_u5_second"));
        fixture.source.responses.push(response);
        let mut group = fixture.ledger.groups[0].clone();
        group.key.push_str(":second");
        group.members = fixture.source.ordered_blocks[2..]
            .iter()
            .map(|block| block.coordinates.clone())
            .collect();
        fixture
            .ledger
            .lifecycle
            .groups
            .insert(group.key.clone(), group.state.clone());
        fixture.ledger.groups.push(group);
        let NativeIdentityExtension::Revision2 {
            eligible_member_links,
            eligible_member_accounting,
        } = &mut fixture.identity
        else {
            unreachable!()
        };
        let links = eligible_member_links.clone();
        let witnesses = eligible_member_accounting.clone();
        for (mut link, mut witness) in links.into_iter().zip(witnesses) {
            let submember = link.source.submember;
            link.source = fixture.source.ordered_blocks[3].coordinates.clone();
            link.source.submember = submember;
            witness.source = link.source.clone();
            eligible_member_links.push(link);
            eligible_member_accounting.push(witness);
        }
        fixture.reseal_shape();
        fixture
    }

    fn u5_cross_response_fixture() -> U4Fixture {
        use crate::conversation::native_search_history::*;
        let mut fixture = U4Fixture::produced();
        let mut result = fixture.source.ordered_blocks.pop().unwrap();
        result.coordinates.occurrence = 2;
        result.coordinates.response = 1;
        result.coordinates.block = 0;
        result.wire_index = 0;
        result.arrival_ordinal = 0;
        let mut text = fixture.source.ordered_blocks[0].clone();
        text.coordinates.occurrence = 1;
        text.coordinates.block = 1;
        text.wire_index = 1;
        text.arrival_ordinal = 1;
        text.block = fixture.put(
            "raw_block",
            &serde_json::json!({"type":"text","text":"between the source halves"}),
        );
        fixture
            .source
            .ordered_blocks
            .extend([text.clone(), result.clone()]);
        fixture.source.responses[0].source_stop_reason = Some("pause_turn".into());
        let mut response = fixture.source.responses[0].clone();
        response.occurrence = 1;
        response.attempt = 2;
        response.block_start = 2;
        response.block_end = 3;
        response.source_stop_reason = Some("end_turn".into());
        response.provider_response_id = Some(serde_json::json!("msg_u5_next"));
        fixture.source.responses.push(response);
        fixture.ledger.accounting[1].source = result.coordinates.clone();
        let mut accounting = fixture.ledger.accounting[0].clone();
        accounting.source = text.coordinates;
        fixture.ledger.accounting.push(accounting);
        fixture
            .ledger
            .accounting
            .sort_by_key(|entry| entry.source.occurrence);
        fixture.ledger.groups[0].members[1] = result.coordinates.clone();
        let NativeIdentityExtension::Revision2 {
            eligible_member_links,
            eligible_member_accounting,
        } = &mut fixture.identity
        else {
            unreachable!()
        };
        for (link, witness) in eligible_member_links
            .iter_mut()
            .zip(eligible_member_accounting)
        {
            let submember = link.source.submember;
            link.source = result.coordinates.clone();
            link.source.submember = submember;
            witness.source = link.source.clone();
        }
        fixture.authorship.insert(2, NativeIdAuthorship::Provider);
        fixture.reseal_shape();
        fixture
    }

    fn u5_group_cases() {
        use crate::conversation::native_search_history::*;
        let fixture = U4Fixture::produced();
        let before = serde_json::to_value(&fixture.source).unwrap();
        let partial = u5_selected(
            &fixture,
            &[fixture.source.ordered_blocks[0].coordinates.clone()],
            &Default::default(),
        )
        .expect("U5 groups full retained history before selecting the request window");
        u4_assert_action(&fixture, &partial, NativeHistoryAction::F);
        assert_eq!(serde_json::to_value(&fixture.source).unwrap(), before);
        let cross = u5_cross_response_fixture();
        let all: Vec<_> = cross
            .source
            .ordered_blocks
            .iter()
            .map(|block| block.coordinates.clone())
            .collect();
        let decision = u5_selected(&cross, &all, &Default::default()).unwrap();
        assert_eq!(
            decision
                .members
                .iter()
                .map(|member| member.source.occurrence)
                .collect::<Vec<_>>(),
            vec![0, 2]
        );
        assert!(
            decision
                .members
                .iter()
                .all(|member| member.action == NativeHistoryAction::Keep),
            "nonadjacent cross-response joins remain one whole group"
        );
        let genuine = u5_two_genuine_fixture();
        let all: Vec<_> = genuine
            .source
            .ordered_blocks
            .iter()
            .map(|block| block.coordinates.clone())
            .collect();
        let decision = u5_selected(&genuine, &all, &Default::default()).unwrap();
        assert_eq!(
            decision.groups.len(),
            2,
            "genuinely distinct identical payload occurrences do not collapse"
        );
        assert_eq!(
            decision
                .members
                .iter()
                .filter(|member| member.raw.is_some())
                .count(),
            4,
            "genuine occurrences each emit once"
        );
        for fault in ["missing-join", "result-before-call", "execution", "domain"] {
            let mut malformed = U4Fixture::produced();
            match fault {
                "missing-join" => {
                    for (index, key) in [(0, "id"), (1, "tool_use_id")] {
                        let mut raw: Value = serde_json::from_slice(
                            &malformed.blobs[&malformed.source.ordered_blocks[index].block.digest],
                        )
                        .unwrap();
                        raw.as_object_mut().unwrap().remove(key);
                        malformed.source.ordered_blocks[index].block =
                            malformed.put("raw_block", &raw);
                    }
                    malformed.ledger.groups[0].validated_join = None;
                }
                "result-before-call" => {
                    let first = malformed.source.ordered_blocks[0].block.clone();
                    malformed.source.ordered_blocks[0].block =
                        malformed.source.ordered_blocks[1].block.clone();
                    malformed.source.ordered_blocks[1].block = first;
                }
                "execution" => malformed.ledger.groups[0].execution = "client".into(),
                "domain" => malformed.ledger.groups[0].origin_ref.digest = "0".repeat(64),
                _ => unreachable!(),
            }
            malformed.reseal_shape();
            let all: Vec<_> = malformed
                .source
                .ordered_blocks
                .iter()
                .map(|block| block.coordinates.clone())
                .collect();
            assert!(
                u5_selected(&malformed, &all, &Default::default()).is_err(),
                "family/domain/execution and genuine joins precede grouping: {fault}"
            );
        }
        let mut stripped = U4Fixture::produced();
        let mut active: NativeActive =
            serde_json::from_slice(&stripped.blobs[&stripped.record.active.digest]).unwrap();
        active
            .dispositions
            .insert(stripped.ledger.groups[0].key.clone(), "strip".into());
        stripped.record.active = stripped.put("active", &active);
        stripped.ledger.active_digest = stripped.record.active.digest.clone();
        stripped.reseal_shape();
        let all: Vec<_> = stripped
            .source
            .ordered_blocks
            .iter()
            .map(|block| block.coordinates.clone())
            .collect();
        let decision = u5_selected(&stripped, &all, &Default::default()).unwrap();
        assert_eq!(decision.groups[0].action, NativeHistoryAction::Strip);
        assert!(decision.members.iter().all(|member| member.raw.is_none()));
    }

    fn u5_lifecycle_cases() {
        use crate::conversation::native_search_history::*;
        use crate::conversation::tool_search::ToolSearchItem;
        let completed = U4Fixture::produced();
        let loss = u5_selected(
            &completed,
            &[completed.source.ordered_blocks[0].coordinates.clone()],
            &Default::default(),
        )
        .unwrap();
        u4_assert_action(&completed, &loss, NativeHistoryAction::F);
        assert_eq!(
            completed.ledger.groups[0].state, "Completed",
            "completed loss is never pending"
        );
        let live = u5_pending_fixture(NativeReplayFamily::MessagesServer, false);
        let binding = live.ledger.groups[0].origin_ref.clone();
        let context = NativePendingContext {
            live_binding: Some(binding.clone()),
            next: NativeNextInput::None,
        };
        let decision = u5_selected(&live, &[], &context).unwrap();
        assert_eq!(
            decision.groups[0].action,
            NativeHistoryAction::ContinueOriginal,
            "pending server work never requests a client-authored answer"
        );
        assert!(
            u5_selected(
                &live,
                &[],
                &NativePendingContext {
                    live_binding: Some(binding.clone()),
                    next: NativeNextInput::Fresh
                }
            )
            .is_err(),
            "live work cannot be omitted for fresh input"
        );
        assert!(u5_selected(&live, &[], &NativePendingContext::default()).is_err());
        let abandoned = u5_pending_fixture(NativeReplayFamily::MessagesServer, true);
        let original_source = serde_json::to_value(&abandoned.source).unwrap();
        let original_ledger = serde_json::to_value(&abandoned.ledger).unwrap();
        let omitted = u5_selected(
            &abandoned,
            &[],
            &NativePendingContext {
                live_binding: None,
                next: NativeNextInput::Fresh,
            },
        )
        .unwrap();
        assert_eq!(omitted.groups[0].action, NativeHistoryAction::OmitSendOnly);
        assert!(
            omitted.members.iter().all(|member| member.raw.is_none()),
            "abandoned omission emits no empty native message"
        );
        assert_eq!(
            serde_json::to_value(&abandoned.source).unwrap(),
            original_source
        );
        assert_eq!(
            serde_json::to_value(&abandoned.ledger).unwrap(),
            original_ledger
        );
        assert!(
            u5_selected(&abandoned, &[], &context).is_err(),
            "persisted terminal state cannot rearm even with the old binding"
        );
        let client = u5_pending_fixture(NativeReplayFamily::ResponsesClient, false);
        let client_context = NativePendingContext {
            live_binding: Some(client.ledger.groups[0].origin_ref.clone()),
            next: NativeNextInput::None,
        };
        let waiting = u5_selected(&client, &[], &client_context).unwrap();
        assert_eq!(waiting.groups[0].action, NativeHistoryAction::AwaitClient);
        assert_eq!(waiting.groups[0].waiting_clients, vec!["call_u4"]);
        for (id, accepted) in [("call_u4", true), ("foreign_call", false)] {
            let context = NativePendingContext {
                live_binding: client_context.live_binding.clone(),
                next: NativeNextInput::Results {
                    messages: Vec::new(),
                    client_outputs: vec![ToolSearchItem::client_answer(id, Vec::new())],
                },
            };
            let result = u5_selected(&client, &[], &context);
            assert_eq!(
                result.is_ok(),
                accepted,
                "only the original legitimate client join can continue"
            );
            if accepted {
                assert_eq!(
                    result.unwrap().groups[0].action,
                    NativeHistoryAction::ContinueOriginal
                );
            }
        }
        let mixed = u5_mixed_fixture();
        let waiting = u5_selected(&mixed, &[], &context).unwrap();
        assert_eq!(
            waiting.groups[0].action,
            NativeHistoryAction::AwaitResults,
            "waiting obligations are captured before cleanup"
        );
        assert_eq!(
            waiting.groups[0].waiting_clients,
            vec!["client_a", "client_b"]
        );
        let results = serde_json::json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"client_a","content":"actual result a"},{"type":"tool_result","tool_use_id":"client_b","content":"actual result b"}]});
        for fault in [
            "valid",
            "missing",
            "empty",
            "duplicate",
            "text",
            "two-messages",
        ] {
            let mut message = results.clone();
            match fault {
                "missing" => {
                    message["content"].as_array_mut().unwrap().pop();
                }
                "empty" => message["content"] = serde_json::json!([]),
                "duplicate" => message["content"][1]["tool_use_id"] = serde_json::json!("client_a"),
                "text" => message["content"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::json!({"type":"text","text":"not results-only"})),
                _ => {}
            }
            let messages = if fault == "two-messages" {
                vec![message.clone(), message]
            } else {
                vec![message]
            };
            let next = NativePendingContext {
                live_binding: Some(binding.clone()),
                next: NativeNextInput::Results {
                    messages,
                    client_outputs: Vec::new(),
                },
            };
            let result = u5_selected(&mixed, &[], &next);
            assert_eq!(
                result.is_ok(),
                fault == "valid",
                "one complete results-only response is required: {fault}"
            );
            if fault == "valid" {
                assert_eq!(
                    result.unwrap().groups[0].action,
                    NativeHistoryAction::ContinueOriginal
                );
            }
        }
        let mut invalid_delivery = u5_mixed_fixture();
        invalid_delivery.ledger.lifecycle.waiting_clients[0].delivery = "unknown-delivery".into();
        invalid_delivery.reseal_shape();
        assert!(
            u5_selected(&invalid_delivery, &[], &context).is_err(),
            "unknown delivery state cannot erase a waiting obligation"
        );
        let mut invalid_source = u5_mixed_fixture();
        invalid_source.ledger.lifecycle.waiting_clients[0].source =
            invalid_source.source.ordered_blocks[0].coordinates.clone();
        invalid_source.reseal_shape();
        assert!(
            u5_selected(&invalid_source, &[], &context).is_err(),
            "waiting clients require their actual ordinary call source"
        );
        let server_responses = u5_pending_fixture(NativeReplayFamily::ResponsesServer, false);
        assert!(
            u5_selected(&server_responses, &[], &context).is_err(),
            "completed-pair qualification cannot authorize a pending Responses server continuation"
        );
    }

    fn u6_lower(items: Vec<ConversationItem>, backend: ApiBackend) -> Value {
        let request = crate::ConversationRequest {
            model: Some("u6-offline".into()),
            items,
            max_output_tokens: Some(4096),
            ..Default::default()
        };
        match backend {
            ApiBackend::Messages => serde_json::to_value(
                crate::conversation::messages::build_messages_request(&request),
            )
            .unwrap(),
            ApiBackend::Responses => serde_json::to_value(
                async_openai::types::responses::CreateResponse::from(&request),
            )
            .unwrap(),
            _ => unreachable!(),
        }
    }

    fn u6_view(fixture: &U4Fixture, decision: &NativeHistoryDecision) -> Vec<ConversationItem> {
        let mut items = vec![ConversationItem::user("Neutral history projection")];
        for block in &fixture.source.ordered_blocks {
            if let Some(member) = decision
                .members
                .iter()
                .find(|m| m.source == block.coordinates)
            {
                if let Some(neutral) = &member.neutral {
                    items.push(ConversationItem::assistant(neutral.text.clone()));
                }
            } else {
                let raw: Value =
                    serde_json::from_slice(&fixture.blobs[&block.block.digest]).unwrap();
                items.push(ConversationItem::assistant(raw["text"].as_str().unwrap()));
            }
        }
        items.push(ConversationItem::user(
            "Continue without historical authority",
        ));
        items
    }

    fn u6_separated() -> U4Fixture {
        let mut fixture = U4Fixture::produced();
        let mut middle = fixture.source.ordered_blocks[0].clone();
        middle.coordinates.block = 1;
        middle.coordinates.occurrence = 1;
        middle.wire_index = 1;
        middle.arrival_ordinal = 1;
        middle.block = fixture.put(
            "raw_block",
            &serde_json::json!({"type":"text","text":"Intervening café 🦀"}),
        );
        fixture.source.ordered_blocks[1].coordinates.block = 2;
        fixture.source.ordered_blocks[1].coordinates.occurrence = 2;
        fixture.source.ordered_blocks[1].wire_index = 2;
        fixture.source.ordered_blocks[1].arrival_ordinal = 2;
        fixture.ledger.groups[0].members[1] = fixture.source.ordered_blocks[1].coordinates.clone();
        fixture.ledger.accounting[1].source = fixture.source.ordered_blocks[1].coordinates.clone();
        let mut accounting = fixture.ledger.accounting[0].clone();
        accounting.source = middle.coordinates.clone();
        fixture.ledger.accounting.insert(1, accounting);
        fixture.source.ordered_blocks.insert(1, middle);
        fixture.source.responses[0].block_end = 3;
        let crate::conversation::native_search_history::NativeIdentityExtension::Revision2 {
            eligible_member_links,
            eligible_member_accounting,
        } = &mut fixture.identity
        else {
            unreachable!()
        };
        for link in eligible_member_links {
            link.source.block = 2;
            link.source.occurrence = 2;
        }
        for witness in eligible_member_accounting {
            witness.source.block = 2;
            witness.source.occurrence = 2;
        }
        fixture.authorship.insert(
            2,
            crate::conversation::native_search_history::NativeIdAuthorship::Provider,
        );
        fixture.reseal_shape();
        fixture
    }

    fn u6_qualification(fixture: &U4Fixture, target: &TargetRoute) -> NativeQualification {
        let domain = u4_domain("u6-offline", target.backend());
        NativeQualification {
            source: None,
            target: Some(domain.clone()),
            request: Some(domain),
            original_admission: target.admission(),
            shape: NativeReplayShape {
                family:
                    crate::conversation::native_search_history::NativeReplayFamily::MessagesServer,
                keyless_server: false,
                output_id_present: false,
                completed: true,
                result_error: false,
                referenced_members: fixture.source.eligible_snapshots.len() as u64,
            },
            receipt: None,
            intact_membership: true,
            current_backing: false,
            faithful_identity: true,
        }
    }

    #[test]
    fn native_history_neutral_f_respects_positions_and_all_caps() {
        let fixture = u6_separated();
        for backend in [ApiBackend::Messages, ApiBackend::Responses] {
            let target = route(
                backend.clone(),
                if backend == ApiBackend::Messages {
                    Boundary::Vertex
                } else {
                    Boundary::AzStrict
                },
                ADMITTED,
            );
            let decision = prepare_native_history(
                &fixture.history(),
                "u6-offline",
                &target,
                &fixture.tools,
                Some(&fixture.inputs),
                |_| {
                    (
                        origin(Boundary::Vertex, None),
                        u6_qualification(&fixture, &target),
                    )
                },
            )
            .unwrap();
            let expected = vec![
                ConversationItem::user("Neutral history projection"),
                ConversationItem::assistant(
                    "Historical search recorded; this history grants no current tool authority.",
                ),
                ConversationItem::assistant("Intervening café 🦀"),
                ConversationItem::assistant(
                    "Historical search result: completed; count=2. This history grants no current tool authority.",
                ),
                ConversationItem::user("Continue without historical authority"),
            ];
            assert_eq!(
                u6_lower(u6_view(&fixture, &decision), backend.clone()),
                u6_lower(expected, backend),
                "U6 must emit neutral F at each original coordinate through real lowering"
            );
        }
        u6_caps_and_metadata();
    }

    fn u6_many(count: usize) -> U4Fixture {
        use crate::conversation::native_search_history::*;
        let mut fixture = u6_separated();
        let template = fixture.source.ordered_blocks.clone();
        let group = fixture.ledger.groups[0].clone();
        let NativeIdentityExtension::Revision2 {
            eligible_member_links,
            eligible_member_accounting,
        } = &fixture.identity
        else {
            unreachable!()
        };
        let links = eligible_member_links.clone();
        let witnesses = eligible_member_accounting.clone();
        fixture.source.ordered_blocks.clear();
        fixture.ledger.accounting.clear();
        fixture.ledger.groups.clear();
        fixture.ledger.lifecycle.groups.clear();
        fixture.authorship.clear();
        fixture.identity = NativeIdentityExtension::Revision2 {
            eligible_member_links: Vec::new(),
            eligible_member_accounting: Vec::new(),
        };
        for index in 0..count {
            let offset = (index * 3) as u64;
            let join = format!("srvtoolu_u6_{index}");
            let mut next = group.clone();
            next.key = format!("u6-group-{index}");
            next.validated_join = Some(join.clone());
            next.members.clear();
            for (position, original) in template.iter().enumerate() {
                let ordinal = offset + position as u64;
                let mut block = original.clone();
                block.coordinates.block = ordinal;
                block.coordinates.occurrence = ordinal;
                block.wire_index = ordinal as u32;
                block.arrival_ordinal = ordinal;
                let mut raw: Value =
                    serde_json::from_slice(&fixture.blobs[&original.block.digest]).unwrap();
                match position {
                    0 => raw["id"] = serde_json::json!(join),
                    1 => {
                        raw["text"] = serde_json::json!(if index == 0 {
                            "Historical search recorded; this history grants no current tool authority.".to_string()
                        } else {
                            format!("Middle {index} café 🦀")
                        })
                    }
                    2 => raw["tool_use_id"] = serde_json::json!(join),
                    _ => unreachable!(),
                }
                block.block = fixture.put("raw_block", &raw);
                fixture.ledger.accounting.push(NativeViewLink {
                    source: block.coordinates.clone(),
                    source_digest: String::new(),
                    view_item: None,
                    view_digest: None,
                    role: "SourceOnly".into(),
                    source_subrange: None,
                });
                if position != 1 {
                    next.members.push(block.coordinates.clone());
                    fixture
                        .authorship
                        .insert(ordinal, NativeIdAuthorship::Provider);
                }
                fixture.source.ordered_blocks.push(block);
            }
            let NativeIdentityExtension::Revision2 {
                eligible_member_links,
                eligible_member_accounting,
            } = &mut fixture.identity
            else {
                unreachable!()
            };
            for (link, witness) in links.iter().zip(&witnesses) {
                let mut link = link.clone();
                let mut witness = witness.clone();
                link.source.block = offset + 2;
                link.source.occurrence = offset + 2;
                witness.source = link.source.clone();
                eligible_member_links.push(link);
                eligible_member_accounting.push(witness);
            }
            fixture
                .ledger
                .lifecycle
                .groups
                .insert(next.key.clone(), "Completed".into());
            fixture.ledger.groups.push(next);
        }
        fixture.source.responses[0].block_end = (count * 3) as u64;
        fixture.reseal_shape();
        fixture
    }

    fn u6_project(
        fixture: &U4Fixture,
        prior: Option<&crate::conversation::native_search_history::NativeFProvenance>,
    ) -> NativeHistoryDecision {
        let selected: Vec<_> = fixture
            .source
            .ordered_blocks
            .iter()
            .map(|b| b.coordinates.clone())
            .collect();
        let target = route(ApiBackend::Messages, Boundary::Vertex, ADMITTED);
        prepare_native_history_reproject(
            &fixture.history(),
            &NativeProjectionInputs {
                target_model_id: "u6-offline",
                target: &target,
                tools: &fixture.tools,
                current: Some(&fixture.inputs),
                selected: &selected,
                origin_domains: &Default::default(),
                pending: &Default::default(),
                prior_f: prior,
            },
            |_| {
                (
                    origin(Boundary::Vertex, None),
                    u6_qualification(fixture, &target),
                )
            },
        )
        .unwrap()
    }

    fn u6_caps_and_metadata() {
        use crate::conversation::native_search_history::*;
        let old = u6_project(&u6_many(3), None);
        assert!(
            old.members
                .iter()
                .filter_map(|m| m.neutral.as_ref())
                .all(|n| n.kind != NativeNeutralKind::Omission)
        );
        let mut fixture = u6_many(5);
        fixture.ledger.groups.reverse();
        fixture.reseal_shape();
        let source_before = fixture.source.clone();
        let active_before = fixture.blobs[&fixture.record.active.digest].clone();
        let decision = u6_project(&fixture, Some(&old.f_provenance));
        let selected: Vec<_> = fixture
            .source
            .ordered_blocks
            .iter()
            .map(|block| block.coordinates.clone())
            .collect();
        let target = route(ApiBackend::Messages, Boundary::Vertex, ADMITTED);
        let core = prepare_native_history_core(
            &fixture.history(),
            &NativeProjectionInputs {
                target_model_id: "u6-offline",
                target: &target,
                tools: &fixture.tools,
                current: Some(&fixture.inputs),
                selected: &selected,
                origin_domains: &Default::default(),
                pending: &Default::default(),
                prior_f: Some(&old.f_provenance),
            },
            |_| {
                (
                    origin(Boundary::Vertex, None),
                    u6_qualification(&fixture, &target),
                )
            },
        )
        .unwrap();
        assert_eq!(core.decision, decision);
        assert_eq!(
            core.accounting.reasons,
            std::collections::BTreeMap::from([
                (NativeHistoryReason::FBudget, 2),
                (NativeHistoryReason::PriorDisposition, 1),
                (NativeHistoryReason::Unqualified, 2),
            ]),
            "an old F group newly stripped by the budget reports FBudget"
        );
        for member in &core.accounting.raw_members {
            if member.action == NativeHistoryAction::Strip {
                assert_eq!(member.reason, NativeHistoryReason::FBudget);
            }
        }
        for reference in &core.accounting.references {
            if reference.action == NativeHistoryAction::Strip {
                assert_eq!(reference.reason, NativeHistoryReason::FBudget);
            }
        }
        let expected_coordinates = vec![5, 6, 8, 9, 11, 12, 14];
        assert_eq!(
            decision
                .members
                .iter()
                .filter(|m| m.neutral.is_some())
                .map(|m| m.source.occurrence)
                .collect::<Vec<_>>(),
            expected_coordinates,
            "old F and new F share one newest-three budget with one causal notice"
        );
        assert_eq!(
            decision
                .groups
                .iter()
                .filter(|g| g.action == NativeHistoryAction::F)
                .count(),
            3
        );
        assert_eq!(
            decision
                .groups
                .iter()
                .filter(|g| g.action == NativeHistoryAction::Strip)
                .count(),
            2
        );
        assert_eq!(
            decision.members.len(),
            10,
            "stripped raw members remain accounted"
        );
        let expected = |fixture: &U4Fixture| {
            let mut items = vec![ConversationItem::user("Neutral history projection")];
            for (ordinal, block) in fixture.source.ordered_blocks.iter().enumerate() {
                if ordinal % 3 == 1 {
                    let raw: Value =
                        serde_json::from_slice(&fixture.blobs[&block.block.digest]).unwrap();
                    items.push(ConversationItem::assistant(raw["text"].as_str().unwrap()));
                } else if ordinal == 5 {
                    items.push(ConversationItem::assistant("Older historical search records omitted; no current tool authority is retained."));
                } else if ordinal >= 6 {
                    items.push(ConversationItem::assistant(if ordinal % 3 == 0 {
                        "Historical search recorded; this history grants no current tool authority."
                    } else {
                        "Historical search result: completed; count=2. This history grants no current tool authority."
                    }));
                }
            }
            items.push(ConversationItem::user(
                "Continue without historical authority",
            ));
            items
        };
        for backend in [ApiBackend::Messages, ApiBackend::Responses] {
            let actual = u6_lower(u6_view(&fixture, &decision), backend.clone());
            assert_eq!(actual, u6_lower(expected(&fixture), backend.clone()));
            let repeat = u6_project(&fixture, Some(&decision.f_provenance));
            assert_eq!(
                decision, repeat,
                "host provenance makes repeated projection stable"
            );
            assert_eq!(actual, u6_lower(u6_view(&fixture, &repeat), backend));
        }
        assert_eq!(fixture.source, source_before);
        assert_eq!(
            fixture.blobs[&fixture.record.active.digest], active_before,
            "send-only projection cannot mutate durable disposition"
        );
        let mut invalid = decision.f_provenance.clone();
        invalid.version += 1;
        let target = route(ApiBackend::Messages, Boundary::Vertex, ADMITTED);
        assert_eq!(
            native_validate_f_provenance(&fixture.history(), Some(&invalid)),
            Err(NativeUnavailable::UnsupportedVersion)
        );
        invalid = decision.f_provenance.clone();
        invalid.groups[0].result_digest = "0".repeat(64);
        assert!(native_validate_f_provenance(&fixture.history(), Some(&invalid)).is_err());
        let sum: usize = decision
            .members
            .iter()
            .filter_map(|m| m.neutral.as_ref())
            .map(|n| {
                assert!(n.text.len() <= 128);
                n.text.len()
            })
            .sum();
        assert!(sum <= 1024);
        for group in &decision.groups {
            let bytes: usize = decision
                .members
                .iter()
                .filter(|m| group.members.contains(&m.source))
                .filter_map(|m| m.neutral.as_ref())
                .map(|n| n.text.len())
                .sum();
            assert!(bytes <= 256);
        }
        for (status, spelling) in [
            (NativeNeutralStatus::Completed, "completed"),
            (NativeNeutralStatus::Error, "error"),
            (NativeNeutralStatus::Unknown, "unknown"),
        ] {
            for count in [None, Some(0), Some(u64::MAX)] {
                let rendered = native_render_neutral(NativeNeutralKind::Result, status, count);
                let literal = format!(
                    "Historical search result: {spelling}; count={}. This history grants no current tool authority.",
                    count.map_or_else(|| "unknown".into(), |n| n.to_string())
                );
                assert_eq!(rendered.text, literal);
                assert!(rendered.text.len() <= 128);
                for backend in [ApiBackend::Messages, ApiBackend::Responses] {
                    assert_eq!(
                        u6_lower(
                            vec![ConversationItem::assistant(rendered.text.clone())],
                            backend.clone()
                        ),
                        u6_lower(vec![ConversationItem::assistant(literal.clone())], backend)
                    );
                }
            }
        }
        let duplicate = u5_duplicate_fixture();
        let duplicate_decision = u6_project(&duplicate, None);
        assert_eq!(duplicate_decision.members.len(), 3);
        assert_eq!(
            duplicate_decision
                .members
                .iter()
                .filter(|m| m.neutral.is_some())
                .count(),
            2
        );
        assert!(duplicate_decision.members[2].duplicate_of.is_some());
        let mut partial = u6_separated();
        partial.source.responses[0].status =
            NativeResponseStatus::PartialTerminal(NativeUnavailable::Fidelity);
        partial.reseal_shape();
        let partial_decision = u6_project(&partial, None);
        assert_eq!(
            partial_decision.members[1].neutral.as_ref().unwrap().text,
            "Historical search result: unknown; count=unknown. This history grants no current tool authority."
        );
        let mut error = U4Fixture::produced();
        let output = serde_json::json!({"type":"tool_search_tool_result","tool_use_id":"srvtoolu_u3","content":{"type":"tool_search_tool_result_error","error_code":"DO NOT ECHO provider error"}});
        error.source.ordered_blocks[1].block = error.put("raw_block", &output);
        let NativeIdentityExtension::Revision2 {
            eligible_member_links,
            eligible_member_accounting,
        } = &mut error.identity
        else {
            unreachable!()
        };
        eligible_member_links.clear();
        eligible_member_accounting.clear();
        error.reseal_shape();
        let error_decision = u6_project(&error, None);
        assert_eq!(
            error_decision.members[1].neutral.as_ref().unwrap().text,
            "Historical search result: error; count=0. This history grants no current tool authority."
        );
        let kept = prepare_native_history(
            &fixture.history(),
            "u6-offline",
            &target,
            &fixture.tools,
            Some(&fixture.inputs),
            |_| {
                let mut q = u6_qualification(&fixture, &target);
                q.source = q.target.clone();
                q.shape.referenced_members = 2;
                q.receipt = Some(NativeShapeReceipt {
                    target: q.target.clone().unwrap(),
                    shape: q.shape.clone(),
                    receipt_id: "offline-complete-shape".into(),
                });
                (origin(Boundary::Vertex, Some("u6-offline")), q)
            },
        )
        .unwrap();
        assert!(
            kept.groups
                .iter()
                .all(|g| g.action == NativeHistoryAction::Keep)
        );
        assert!(
            kept.members
                .iter()
                .all(|m| m.neutral.is_none() && m.raw.is_some())
        );
        assert!(
            kept.f_provenance.groups.is_empty(),
            "native KEEP spends no F budget"
        );
    }
}
