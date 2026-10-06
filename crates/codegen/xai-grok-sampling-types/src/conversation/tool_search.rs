//! Native tool-discovery representation (A-24, apex-waj.1).
//!
//! A-24: `ConversationItem` had six variants and none of them can hold a
//! `tool_search_call` / `tool_search_output` item, so discovery state had no
//! home in the conversation IR. On a Responses->Responses model switch — the
//! seam where the preemptive family-switch compaction is deliberately skipped
//! (`xai-grok-shell/src/session/acp_session_impl/model_switch.rs:464-468`,
//! `family_switch_compact_required`, so that the switch-time projection runs on
//! the real history) and the switch projector is therefore the only actor that
//! rewrites history at the boundary —
//! state the IR cannot represent is simply dropped, while A-19/A-23 forbid
//! re-declaring those definitions in `tools[]`. The model then loses every
//! discovered tool silently. This module is that missing home.
//!
//! # Where the evidence cited below lives
//!
//! Four forms of cite appear below. Forms 1 and 2 resolve against two different
//! roots; form 3 is an abbreviation of a form-1 cite and so shares form 1's root;
//! form 4 resolves against neither of the first two.
//!
//! 1. Campaign artifacts, deliberately OUTSIDE this repository, under the corpus
//!    root `/Users/palanisd/Projects/upstream/grok/plans/harness/hosted-tool-search/`:
//!    `captures/…`, `ratchet-capture/…` and a root script such as
//!    `probe_wire_grounding.py` resolve against that root; a bare `fixtures/…`
//!    resolves against its `ratchet-capture/` subdirectory (that is where the
//!    fixture tree actually sits). Capture bytes are campaign state, not product
//!    code, which is why they are not vendored here.
//! 2. In-repo paths, inside this checkout: `PLAN:` cites are line numbers in
//!    `docs/superpowers/plans/2026-09-25-s3a-responses-native-tool-search.md`;
//!    sibling source is crate-relative (`xai-grok-shell/src/session/…` above), or it
//!    is this crate's own tree and resolves from
//!    `crates/codegen/xai-grok-sampling-types/src/`. Those are the only file paths
//!    below cite this crate's own tree: `conversation/responses.rs`,
//!    `conversation/responses_tests.rs`, this file's own
//!    `conversation/tool_search.rs`, and a bare `conversation.rs` (the module this
//!    one is declared in). The one in-crate FIXTURE cite instead starts at the crate
//!    dir — `src/conversation/fixtures/…` — and an item path joined with `::`
//!    (e.g. `conversation::responses_tests::json_keys`) names an item, not a file.
//! 3. Abbreviated fixture cites — a fixture id plus a location inside it, e.g.
//!    `CX3…/next-turn.json` `input[11]`. The file is a form-1 file, under
//!    `ratchet-capture/fixtures/`; the suffix is a path into that document. The id
//!    may also appear bare, with the `fixtures/<family>/` prefix dropped —
//!    `R6-client-loop/next-turn.json` for
//!    `fixtures/grok-probe/R6-client-loop/next-turn.json`. The 8 fixture ids under
//!    `ratchet-capture/fixtures/` are unique across its 3 families, so the elision
//!    cannot be ambiguous. The id may also be shortened to its short prefix
//!    (`CX3/response.sse`), that prefix being unique per fixture.
//! 4. One file sits outside both of those roots: the campaign's operating-rules
//!    file, cited as `grok-build-responses/Agents.md`, which lives in a SIBLING
//!    worktree at
//!    `/Users/palanisd/Projects/upstream/wt/grok-build-responses/Agents.md` — not in
//!    this checkout and not under the corpus root. A cite to it names the section and
//!    restates the part of that rule this code actually depends on — the cite is a
//!    pointer for a reader, not a transcription — because this crate cannot depend on
//!    a file outside it.
//!
//! A cite that does not resolve is stale, not approximate: re-resolve it before
//! repeating the claim.
//!
//! # Shape: verbatim `raw` is the contract, typed views are derived
//!
//! `async-openai` 0.33.1 (`crate::rs`) does not model either item — the fork
//! at `4d72e1d` has zero `tool_search` item types — so no typed vendor struct
//! can carry these across the store. The house answer for a provider-native
//! Responses item the dependency does not know yet is
//! [`crate::CodexRawInputItem`]: persist the exact provider JSON and derive
//! every decision from it. Same discipline here:
//!
//! * [`ToolSearchItem::raw`] is the single source of truth for what the harness
//!   stores and the only thing a replay path starts from. Nothing here mutates
//!   it — and nothing here claims it is wire-ready either: the echoed
//!   `tool_search_call` carries `created_by`, which the boundary 400s on replay
//!   (PLAN:1325-1328), and the strip-list that owns that is T15's
//!   (PLAN:1421). Storing bytes verbatim and sending bytes verbatim are
//!   different obligations; this module discharges the first.
//! * Every typed accessor (status, arguments, definitions, names) is derived
//!   from `raw` on demand, so the typed view and the stored bytes cannot
//!   drift apart. A typed view that round-trips lossily (e.g. an unknown
//!   `status` string collapsing to [`ToolSearchStatus::Unknown`]) therefore
//!   cannot corrupt a request, and a shape this build does not model degrades
//!   the VIEW instead of being refused at the door — refusing an item hands the
//!   caller a decision whose only easy answer is to drop it, which A-26 forbids.
//! * Opaque ids are handles, not content (wire invariant 6): `tsc_*` /
//!   `tso_*` are copied verbatim and never rewritten. Nothing here mints an
//!   id **in this cut**, and **who mints them depends on the quadrant**: on the
//!   client-executed side the harness authors `tool_search_output` (no fixture
//!   response FILE ever contains that item — `ratchet-capture/_recon-fixtures.md`
//!   §6b, re-measured here over all
//!   7 of its rows and all 7 are zero-hit; that doc's G-e row at :920 says "0 of 6"
//!   under its own inline "SSE census" label (not a heading — §6b's is
//!   "Response-stream census"), and the table's non-streaming row reconciles that
//!   to the six streamed files; either reading, all 7 are clean here), where donor
//!   codex mints a `tso_*` and our own probe omits the field and is a scored
//!   live window; on the server-executed side the PROVIDER mints both halves
//!   (`tso_0ce980d5…` in
//!   `captures/2026-09-25-wire-grounding/wire_resp_20260925T062640Z_R1_SOL_HOSTED.json`).
//!   This module takes the omitting side for items IT authors, and preserves
//!   whoever's id is already on an item it received; see [`ToolSearchItem::id`].
//!   The one planned exception is outside this cut: Task 15 schedules
//!   `tso_synthetic_id` (PLAN:948, "Modify: … `conversation/tool_search.rs`
//!   (the `tso_` id derivation helper)", PLAN:940) into this file — a derived
//!   id for the output of an interrupted call. That is a NEW mint with its own
//!   rules (v5 over the call's ITEM id, idempotent across resume, never entering
//!   the function-call id domain), not an exception to the no-rewrite rule above.
//!
//! # The two wire families are inverted (A-19 / A-23)
//!
//! * **Responses**: the discovered definitions live in `tool_search_output`
//!   history and MUST NOT be re-declared in `tools[]`.
//! * **Messages**: a discovered tool must be DECLARED in `tools[]`
//!   (`defer_loading` is an optimisation, not a validity condition).
//!
//! Which side a target is on is a wire/projector decision, not a discovery
//! fact, so nothing here takes a family/boundary parameter — A-24 warns that
//! `CatalogFamily` / `ResponsesWireDialect` / `Boundary` must not gain a
//! fourth vocabulary for discovery. The accessors are family-neutral and
//! callers decide.

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use super::ConversationItem;

/// Wire `type` tag of the model-authored discovery request item.
pub const TOOL_SEARCH_CALL_ITEM_TYPE: &str = "tool_search_call";

/// Wire `type` tag of the discovery RESULT item.
///
/// Both halves of the pair exist on both quadrants; only the AUTHOR differs —
/// the harness authors this half on the client-executed quadrant, the provider
/// mints it on the server-executed one (`tso_0ce980d5…` with
/// `execution:"server"` at
/// `captures/2026-09-25-wire-grounding/wire_resp_20260925T062640Z_R1_SOL_HOSTED.json`
/// `output[2]`). Nothing in the tag tells a reader which, so read
/// [`ToolSearchItem::execution`].
pub const TOOL_SEARCH_OUTPUT_ITEM_TYPE: &str = "tool_search_output";

/// `execution` value of the CLIENT-executed quadrant: the harness runs the
/// search and authors the `tool_search_output` in the FOLLOWING request.
/// Observed on both halves of all three client-quadrant fixtures
/// (`CX1…/next-turn.json`, `CX3…/next-turn.json`,
/// `fixtures/grok-probe/R6-client-loop/next-turn.json`). The Messages wire has no
/// such field.
///
/// The one name for this value inside the crate: the declaration-emission enum
/// `super::responses::ToolSearchExecution` (`pub(super)`, i.e. visible inside the
/// `conversation` module and nowhere else in the crate)
/// resolves its `Client` arm through this constant
/// (`conversation/responses.rs`, `ToolSearchExecution::as_str`'s
/// `Self::Client => super::tool_search::CLIENT_EXECUTION` arm), so renaming it
/// breaks that path. The
/// `Server` arm does NOT — see [`SERVER_EXECUTION`].
pub const CLIENT_EXECUTION: &str = "client";

/// `execution` value of the SERVER-executed quadrant: the provider runs the
/// search and mints BOTH halves of the pair itself, with `call_id: null`.
/// Observed on 8/8 discovery ITEMS across the four hosted-search captures that
/// carry any, in `captures/2026-09-25-wire-grounding/`:
/// `wire_resp_20260925T062640Z_R1_SOL_HOSTED.json`,
/// `wire_resp_20260925T062640Z_R2_TERRA_HOSTED.json`,
/// `wire_resp_20260925T063014Z_R1_SOL_HOSTED_ts.json` and
/// `wire_resp_20260925T063001Z_R2_LUNA_HOSTED_NONE.json` — 1 call + 1 output in
/// each. The other `*_HOSTED` files in that directory carry the `tool_search`
/// entry in `tools[]` but no discovery items, so they are not evidence here.
/// PLAN:947 exempts this quadrant from orphan removal, which is why the two
/// values get their own constant.
///
/// As of this cut the declaration-emission enum does NOT read it —
/// `super::responses::ToolSearchExecution::as_str` returns the literal
/// `"server"` for that arm (its `Self::Server => "server"` arm), and only the
/// `Client` arm resolves through a constant. So this is the predicate's value,
/// not yet the emitter's; that asymmetry is on the `apex-waj.3` lane.
pub const SERVER_EXECUTION: &str = "server";

/// How much of a model-authored query [`ToolSearchItem::text_summary`] echoes.
pub const MAX_SUMMARY_QUERY_BYTES: usize = 200;

/// The keys a replayed **`tool_search_call`** may carry, as observed on the wire
/// (PLAN:1325-1328): replaying the echoed call with the provider's extra fields
/// returns `400 Unknown parameter: 'input[1].created_by'`.
///
/// **CALL HALF ONLY — this is not an output allow-list.** The observation is of
/// a `tool_search_call`, and a `tool_search_output` carries a key that is absent
/// from this set and demonstrably replayable: `tools` is the field the loaded-tool
/// set lives in (A-14, PLAN:1532-1533) and every accepted replayed output in the
/// corpus has it (`fixtures/codex/CX1-toolsearch-mcp-dryrun/next-turn.json`
/// `input[4]`, `CX3-toolsearch-5.5-LIVE/next-turn.json` `input[12]`,
/// `fixtures/grok-probe/R6-client-loop/next-turn.json` `input[2]`). `error` is
/// outside the observed set too, and is NOT claimed as an observed wire field:
/// PLAN:22's D-ERR channel specifies an `error` text alongside `tools: []` and
/// `status: "error"`, but no `tool_search_*` item in `fixtures/` or `captures/`
/// carries an `error` key (checked across both trees), so an allow-list author
/// has a contract citation and no byte. A pass that applied this set to both
/// halves would still strip the loaded-tool set out of history — the exact
/// A-14/A-26 failure. The output half's allow-list has never been observed and
/// must not be guessed from this one.
///
/// Deciding *what to strip and where* belongs to the pairing/repair path: PLAN:1421
/// asks the repair path (T15) for "a strip-list, not a one-off fix", and
/// PLAN:1494-1495 is the sentence that names the list itself ("T15's strip-list should
/// carry `created_by`"). This constant exists
/// so that path does not have to re-derive the boundary's parameter allow-list
/// from a 400, and so the observation is not lost when this module's author stops
/// paying attention. It is a key set, not a predicate: nothing here mutates
/// [`ToolSearchItem::raw`].
pub const CALL_REPLAYABLE_KEYS: &[&str] =
    &["arguments", "call_id", "execution", "id", "status", "type"];

/// Which half of the discovery pair an item is.
///
/// Stored alongside the raw item inside [`ToolSearchItem`] so a projector can
/// branch without re-reading the JSON tag. It is not settable: OUTSIDE this module
/// `from_wire` is the only constructor and it reads `kind` from `raw`'s `type` tag,
/// so no caller can make the two disagree and a malformed pair is refused at
/// construction rather than discovered at request-build time. Inside this module the
/// private fields are reachable and a mismatched literal COMPILES — the file says so
/// itself where it explains why there is no derived `Deserialize`.
///
/// No serde derives, on purpose. There is no `Deserialize` impl to mis-use at all, so
/// `serde_json::from_value::<ToolSearchKind>(json!("tool_search_call"))` does not
/// compile here (E0277 — the trait bound is unsatisfied); were one derived, its
/// spellings would be the variant names `"Call"`/`"Output"` (this enum carries no
/// `rename_all`, unlike [`ToolSearchStatus`]), and neither is the wire tag. So
/// [`Self::item_type`] / [`Self::from_item_type`] are the ONLY wire mapping, which is
/// the point: a second spelling of the same fact is the fourth vocabulary A-24 warns
/// about. Nothing in the crate serialises a kind: [`ToolSearchItem`]'s `Serialize`
/// writes `raw`, which carries the real `type` tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSearchKind {
    /// `tool_search_call` — the model asked for tools; the harness answers.
    Call,
    /// `tool_search_output` — the definitions the harness loaded.
    Output,
}

impl ToolSearchKind {
    /// The wire `type` tag this kind serialises to.
    pub fn item_type(&self) -> &'static str {
        match self {
            Self::Call => TOOL_SEARCH_CALL_ITEM_TYPE,
            Self::Output => TOOL_SEARCH_OUTPUT_ITEM_TYPE,
        }
    }

    /// Inverse of [`Self::item_type`]; `None` for any other item type.
    pub fn from_item_type(item_type: &str) -> Option<Self> {
        match item_type {
            TOOL_SEARCH_CALL_ITEM_TYPE => Some(Self::Call),
            TOOL_SEARCH_OUTPUT_ITEM_TYPE => Some(Self::Output),
            _ => None,
        }
    }
}

/// The three `status` spellings this module's PAIRING vocabulary names.
///
/// `in_progress` is the stream skeleton and the two terminals are PLAN:22's
/// D-ERR `error` channel and the `completed` every accepted answer carries. The
/// plan's own types spell only the TWO terminals — PLAN:222
/// (`pub status: String /* "completed" | "error" */`), PLAN:599
/// (`SearchStatus { Completed, Error}`) and the A2 lint at PLAN:1035 all say
/// `completed` / `error` — and `in_progress` occurs exactly once in the whole plan,
/// at PLAN:1754, describing a stream frame rather than a type value. This module's
/// pairing vocabulary is therefore wider than the plan's type vocabulary ON PURPOSE:
/// a half-written frame has to be recognised for what it is.
///
/// Kept as constants rather than re-spread as literals because THREE readers must
/// agree on the set: [`ToolSearchStatus::from_wire`] (the view),
/// `ToolSearchItem::is_pairable` (who may take part in a pair — private, so prose) and
/// `ToolSearchItem::has_unmodelled_status` (who is a provider claim this build
/// cannot name — same). A fourth DECISION consults TWO of these constants and no more:
/// `keyless_client_answer_present`. The ARRAY has exactly one reader — only
/// `has_unmodelled_status` consults `STATUSES_IN_VOCABULARY`; [`ToolSearchStatus::from_wire`]
/// and `is_pairable` match the constants one at a time — and that reader is reached only for
/// an item `is_pairable` has already refused, so the array is the vocabulary's declaration,
/// not a fourth reader that must agree on the set. The two counts here differ by
/// construction and not by accident: `ToolSearchItem::raw_status` (private, so
/// prose) counts CALL SITES of that accessor and `is_pairable` contributes TWO of
/// its four PRODUCTION SITES; the accessor's four other call sites are in the test
/// module, which also reads these constants at five sites. This sentence counts the
/// readers that must agree on the whole SET — and [`ToolSearchStatus::from_wire`] is
/// one of those three but maps them to variants instead of reading them through the
/// accessor. If they disagree, a string is either double-classified or falls between
/// two gates. A spelling outside this set is not a hypothetical future:
/// `function_call` items in the same corpus say `"incomplete"` — 12 of them exist and
/// 8 carry a `status`: 6 say `"completed"`, 2 say `"incomplete"` — with 4 more omitting
/// entirely and `"status": null` appearing in 0 files (measured across `captures/`
/// and `ratchet-capture/fixtures/`). `ToolSearchStatus` models none of that.
const STATUS_IN_PROGRESS: &str = "in_progress";
/// The one terminal state whose definitions may be loaded.
const STATUS_COMPLETED: &str = "completed";
/// The terminal failure state (PLAN:22's D-ERR channel).
const STATUS_ERROR: &str = "error";
const STATUSES_IN_VOCABULARY: [&str; 3] = [STATUS_IN_PROGRESS, STATUS_COMPLETED, STATUS_ERROR];

/// Lifecycle state of a discovery item.
///
/// The streamed duplication is the reason this needs a type rather than a
/// string check: a Responses stream delivers the SAME item twice —
/// `output_item.added` then `output_item.done`
/// (`fixtures/codex/CX3-toolsearch-5.5-LIVE/response.sse`, both copies at
/// `output_index: 1`). **Gate on this status, never on frame order**: the only
/// genuine provider stream in the corpus THAT CARRIES A DISCOVERY ITEM (CX3,
/// `response_provenance: "GENUINE
/// provider bytes, live"`) emits `added` as a skeleton — `"in_progress"` with
/// `arguments: {}` — and fills the values in `done`. A second shape exists in
/// the corpus, `CX1-toolsearch-mcp-dryrun/response.sse`, whose `added` copy is
/// ALREADY `completed` with real arguments, but that file's own meta.json marks
/// it `response_provenance: "MOCK (codex-arm/mock_upstream.py) -- response-derived
/// assertions are NOT donor truth"` — one string on one line of that file: it is
/// what our mock upstream emits, not what the provider emits. It is
/// still the reason not to key on frame order — a harness-side consumer has to survive
/// both — but it must not be cited as provider behaviour. "Take the first frame"
/// fingerprints an empty item on the live capture and works on the mock: exactly the
/// A-22 defect class.
///
/// The two terminal strings are the wire's, not this enum's invention, and each
/// has its own source: PLAN:22's D-ERR channel emits `status: "error"` (with
/// `tools: []`, the call's real `call_id`, an `error` text, and "No empty id
/// anywhere"), PLAN:24 makes a zero-hit search a SUCCESS with `tools: []`, and
/// the plan's own types spell the field
/// `pub status: String /* "completed" | "error" */` at PLAN:222;
/// `SearchStatus { Completed, Error}` at PLAN:599; the A2 lint pins
/// `status ∈ {completed, error}` at PLAN:1035.
///
/// `Deserialize` is derived with `#[serde(other)]` so a consumer that embeds this
/// enum in its own struct cannot fail-to-load an item it is obliged to replay.
/// The three reach claims on that impl are pinned by the test
/// `the_status_view_tolerates_an_unknown_string_and_rejects_a_non_string` — each
/// vocabulary spelling reaches its OWN variant (that is what `rename_all` buys), an
/// unrecognised **string** (`"queued"`) loads as [`Self::Unknown`], and a
/// `status` that is not a string at all (e.g. `null`) is a serde type error at
/// the consumer's site. That is unlike [`Self::from_wire`], which reads an absent
/// or non-string value as [`Self::Unknown`] because it goes through
/// `Value::as_str`, and it is the reason [`ToolSearchItem::status`] uses
/// `from_wire`: a consumer embedding the enum beside bytes it must replay wants
/// the tolerant path. (The other test named below,
/// `an_unknown_status_degrades_the_view_not_the_bytes`, pins only the
/// unrecognised-STRING side of `from_wire` — it feeds `"queued"` on a call. The
/// absent and non-string inputs are pinned in
/// `the_status_view_tolerates_an_unknown_string_and_rejects_a_non_string` and
/// `an_output_without_a_readable_status_is_a_skeleton_not_an_unmodelled_answer`.
/// The absence of a `Serialize` impl is a property of this type, not a claim any
/// test makes.)
///
/// What a status OUTSIDE this vocabulary means for pairing is a different question
/// and is answered on `ToolSearchItem::has_unmodelled_status`: the view degrades to
/// [`ToolSearchStatus::Unknown`] and the BYTES are untouched, while the pairing verdict
/// does change: it routes such an item away from both `Incomplete` and `Paired`.
///
/// No `Serialize`: this type is a VIEW, and the derived impl would emit
/// `"unknown"`, a status string nothing in the corpus has ever carried. Measured by
/// item type across the 76 JSON-bearing files of `captures/` +
/// `ratchet-capture/fixtures/` (235 parsed documents), enumerating the statuses an item type is
/// OBSERVED to carry — a type's key-absent rows are counted only where one is named:
/// `tool_search_call`
/// `completed` x17 + `in_progress` x1 (the CX3 stream skeleton); `tool_search_output`
/// `completed` x13; `function_call` `completed` x6 + `incomplete` x2 + key-absent x4;
/// `message` `completed` x2 + `in_progress` x1 + key-absent x53; `custom_tool_call` `completed` x2 +
/// `in_progress` x1. So `"unknown"`, `"queued"` and `"cancelled"` are all unevidenced,
/// while `"incomplete"` — which this enum CANNOT express — is attested on a
/// `function_call` item (and on no other item type here; the response-envelope objects
/// in the same captures carry it too, which is a different axis); `"status": null`
/// appears in 0 files. The bytes that go back out are [`ToolSearchItem::raw`]'s, which
/// keep the provider's own spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSearchStatus {
    /// Streamed skeleton: present but not yet final.
    InProgress,
    /// Final copy. The only state whose definitions may be loaded.
    Completed,
    /// Terminal failure of the search (`status: "error"`, `tools: []` plus an
    /// `error` text field — PLAN:22's D-ERR channel; the `error` key itself is
    /// not present on any captured discovery item, see
    /// [`CALL_REPLAYABLE_KEYS`] for that measurement). It still ANSWERS its
    /// call: see `ToolSearchItem::is_pairable`.
    Error,
    /// Written by a newer provider than this enum knows.
    ///
    /// View-only: it never rewrites `raw`, so the literal string still goes back
    /// out on replay. Carries the `#[serde(other)]` whose measured reach is stated
    /// on the enum — tolerant of an unknown string, not of a non-string `status`.
    #[serde(other)]
    Unknown,
}

impl ToolSearchStatus {
    /// Parse the wire's `status` string. Unknown values map to
    /// [`Self::Unknown`] rather than failing, because an unrecognised status
    /// on an item we still have to replay is not a reason to drop the item.
    pub fn from_wire(status: Option<&str>) -> Self {
        match status {
            Some(STATUS_IN_PROGRESS) => Self::InProgress,
            Some(STATUS_COMPLETED) => Self::Completed,
            Some(STATUS_ERROR) => Self::Error,
            Some(_) => Self::Unknown,
            None => Self::Unknown,
        }
    }
}

/// Why a JSON value was rejected as a discovery item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSearchItemError {
    /// Not an object — nothing to read the `type` tag from.
    NotAnObject,
    /// `type` missing, not a string, or not one of the two discovery tags.
    /// Carries the offending value **when `type` is a string**, so a caller can log it
    /// without re-reading. A non-string `type` is indistinguishable from a missing one here:
    /// the tag is read through `Value::as_str`, so both give `item_type: None` and display as
    /// `(type: <absent>)` — both forms pinned by `the_two_rejections_display_their_own_reasons`.
    NotADiscoveryItem { item_type: Option<String> },
}

impl std::fmt::Display for ToolSearchItemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnObject => write!(f, "discovery item is not a JSON object"),
            Self::NotADiscoveryItem { item_type } => write!(
                f,
                "not a tool-discovery item (type: {})",
                item_type.as_deref().unwrap_or("<absent>")
            ),
        }
    }
}

impl std::error::Error for ToolSearchItemError {}

/// One native tool-discovery item: a `tool_search_call` or a
/// `tool_search_output`.
///
/// The exact provider JSON is the payload; every typed projection is derived
/// from it (see the [module docs](self)). That is deliberate: these items must
/// go back onto the Responses wire byte-for-byte, and a struct that re-emits
/// its own idea of the shape is how a field gets dropped or reordered out of a
/// cached prefix.
/// The fields are private and there is no derived `Deserialize`: the derived
/// pair was a second, unvalidated constructor, and `ToolSearchItem { kind: Call,
/// raw: <a tool_search_output> }` compiles — which would have made the
/// "`kind` cannot disagree with the bytes" claim false on every path except
/// [`Self::from_wire`]. `Serialize`/`Deserialize` below carry `raw` verbatim, so
/// a store round-trip is a byte round-trip and still lands in `from_wire`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSearchItem {
    /// Which half of the pair this is. Read from `raw`'s `type` tag by
    /// [`Self::from_wire`] — the only constructor anywhere outside this module.
    kind: ToolSearchKind,
    /// Exact provider item, as received. This is what the STORE keeps.
    ///
    /// It is **not** automatically what goes back on the wire: the echoed
    /// `tool_search_call` carries `created_by`, and replaying it verbatim returns
    /// `400 Unknown parameter: 'input[1].created_by'` (PLAN:1325-1328, observed on
    /// live provider bytes). The replayable key set observed there is
    /// `{arguments, call_id, execution, id, status, type}`, and PLAN:1421 assigns
    /// the strip-list to the pairing/repair path (T15), not to the item type —
    /// so this module's obligation is to keep the bytes and not to claim they are
    /// wire-ready.
    raw: Value,
}

impl Serialize for ToolSearchItem {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.raw.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ToolSearchItem {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Value::deserialize(deserializer)?;
        Self::from_wire(raw).map_err(serde::de::Error::custom)
    }
}

impl ToolSearchItem {
    /// Validate and wrap a provider/stream item.
    ///
    /// Rejects only what cannot be a discovery item at all: a non-object, or a
    /// `type` that is not one of the two discovery tags. Everything else is
    /// retained, including a `tool_search_call` whose `arguments` is not an
    /// object — see [`Self::arguments`] for why that shape is a degraded view
    /// rather than a rejection.
    ///
    /// Deliberately does NOT require a `call_id` or a `status`: the donor type
    /// contract binding on T3 (PLAN:1247-1250) makes `call_id` an
    /// `Option<String>` on both halves — "accommodating server-null AND
    /// codex-reused ids" — and `status` optional on the call, and states that
    /// typing either more strictly REJECTS what first-party emits. A discovery
    /// item is never droppable (A-26: stripping the pair destroys discovery at
    /// the provider), so an unpairable one is retained and reported, not refused
    /// here. A construction-time `Err` in particular hands the caller a decision
    /// it can only answer by skipping the item — the forbidden answer.
    /// Accepts any `status`, including [`ToolSearchStatus::InProgress`]: the
    /// stream skeleton is a legitimate thing to hand to the consumer that has to
    /// decide what to keep, and the decision surface is [`Self::is_completed`],
    /// not this constructor.
    pub fn from_wire(raw: Value) -> Result<Self, ToolSearchItemError> {
        let Value::Object(map) = &raw else {
            return Err(ToolSearchItemError::NotAnObject);
        };
        let item_type = map.get("type").and_then(Value::as_str);
        let Some(kind) = item_type.and_then(ToolSearchKind::from_item_type) else {
            return Err(ToolSearchItemError::NotADiscoveryItem {
                item_type: item_type.map(str::to_owned),
            });
        };
        Ok(Self { kind, raw })
    }

    /// Which half of the pair this is, as read from `raw`'s `type` tag.
    pub fn kind(&self) -> ToolSearchKind {
        self.kind
    }

    /// The item's own id (`tsc_*` / `tso_*`), verbatim — never synthesised.
    ///
    /// THREE authoring policies are on disk, and a projection decision has to be
    /// right about all of them:
    ///
    /// 1. the PROVIDER mints both halves on the server-executed quadrant —
    ///    `tso_0ce980d5c6afd41f016ab614240314…` sits in the response body of
    ///    `captures/2026-09-25-wire-grounding/wire_resp_20260925T062640Z_R1_SOL_HOSTED.json`;
    /// 2. donor codex mints the output id on the client quadrant
    ///    (`CX1/next-turn.json` `input[4].id`, `CX3/next-turn.json` `input[12].id`);
    /// 3. our own probe OMITS the field on an output it authors
    ///    (`R6-client-loop/next-turn.json` `input[2]`, a scored live window).
    ///
    /// So "the harness always authors this item" and "the provider never mints
    /// it" are BOTH false as absolutes — each is a quadrant fact. (`ratchet-capture/_recon-fixtures.md`
    /// §6b lists 7 response files and this module re-measured all 7 at zero hits; the
    /// G-e row there at :920 says "0 of 6" under its own inline "SSE census" label —
    /// not a heading, §6b's is "Response-stream census" — which that table's
    /// non-streaming row reconciles to six streamed files.) The positive case is
    /// outside that census either way: it is a `captures/` file, not a `fixtures/`
    /// one.
    ///
    /// The rule here is the same in every quadrant: copy what is there, mint
    /// nothing. An id is a handle (wire invariant 6) and an invented one that
    /// collides with a real mint is worse than an absent one. When we author the
    /// item we take policy 3 — the omitting side our own scored window proves the
    /// boundary accepts — rather than defaulting to the donor's mint.
    ///
    /// An EMPTY string reads as absent, for the same reason [`Self::call_id`]
    /// filters it: the A2 lint requires a NON-EMPTY id where the field is
    /// present (PLAN:1035), and `""` is a value this campaign has actually put in
    /// front of the harness — on an in-repo fixture, not on captured provider bytes (a
    /// sweep of `captures/` + `ratchet-capture/fixtures/` for `"id": ""` returns 0):
    /// `src/conversation/fixtures/emptyid_x69/pre_switch_emptyid.json` (9 records, 5 of
    /// them `reasoning` items carrying `id: ""`), loaded via `include_str!` into the
    /// `EMPTYID_X69_PRE` const in `conversation/responses_tests.rs` (the
    /// `XW-EMPTYID-1` tests).
    /// Returning `Some("")` would advertise a handle that identifies nothing, and
    /// a caller keying a durable record on it would join every empty id together.
    pub fn id(&self) -> Option<&str> {
        self.raw
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
    }

    /// The join key between a call and its output, `None` when absent or empty.
    ///
    /// Optional by donor contract (`call_id` is `Option<String>` on both
    /// halves, PLAN:1248). An item without one is retained but **cannot pair**,
    /// so [`partner_indices`] refuses to match two such items against each
    /// other rather than treating two absences as an agreement.
    pub fn call_id(&self) -> Option<&str> {
        self.raw
            .get("call_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
    }

    /// The `status` field as the closed [`ToolSearchStatus`] view. Lossy in one
    /// direction worth naming: an unknown string, an absent key and a non-string value
    /// all read [`ToolSearchStatus::Unknown`], so the view cannot tell "written by a
    /// newer provider" from "no status at all". That distinction is read off `raw_status`
    /// instead — by `pairing_of`, through `has_unmodelled_status`, which has exactly that
    /// one caller (both are private, so this is prose rather than a link). Nothing outside
    /// the pairing guard can ask it, and nothing through this accessor can.
    pub fn status(&self) -> ToolSearchStatus {
        ToolSearchStatus::from_wire(self.raw.get("status").and_then(Value::as_str))
    }

    /// Whether this is the COMPLETED copy of the item.
    ///
    /// A Responses stream delivers every item TWICE, and which frame is the
    /// skeleton is NOT stable across captures: CX3's `added` frame is
    /// (`status: "in_progress"`, `arguments: {}`) and its `done` frame carries
    /// the real values, while the mock upstream's `added` frame is ALREADY
    /// `completed` (see [`ToolSearchStatus`]'s provenance note for which of the
    /// two is donor truth). So the rule is *gate on this status*, never on frame
    /// order or "first/second". A consumer that persists the non-completed copy
    /// stores an empty query and an empty tool set and the discovery is silently
    /// lost. A-22's audit record is the READ half of that defect — every streamed-item
    /// extractor of this campaign fingerprinted the skeleton and scored its empty
    /// `arguments` as truth (PLAN:1749-1761 — defect #1 is the Responses loader at
    /// :1753-1757, defect #2 the Messages adapter at :1759-1761) — while persisting
    /// it is this module's
    /// inference from the same shape, not a second audited defect.
    pub fn is_completed(&self) -> bool {
        self.status() == ToolSearchStatus::Completed
    }

    /// The raw `status` string, without the [`ToolSearchStatus`] view.
    ///
    /// Every "is this a status I can read" gate goes through here — four sites across
    /// `is_pairable`, `has_unmodelled_status` and [`keyless_client_answer_present`] — so they cannot drift on
    /// what "a status we can read" means: an absent key and a non-string value are
    /// both `None`, and an empty string is `Some("")` (a present string outside the
    /// vocabulary — see `Self::has_unmodelled_status` (private), which is where `""` lands).
    fn raw_status(&self) -> Option<&str> {
        self.raw.get("status").and_then(Value::as_str)
    }

    /// Whether this copy may take part in a 1:1 pair at all.
    ///
    /// Deliberately NOT [`Self::is_completed`], and deliberately different on the
    /// two halves, because the two halves carry different promises:
    ///
    /// - **output**: pairable iff `status` is one of the two TERMINALS the wire
    ///   vocabulary names, `completed` or `error`. (An `in_progress` OUTPUT is
    ///   unattested in the corpus — all 13 captured outputs say `completed` — so that
    ///   arm of the exclusion is defensive by design: A-22's defect class is exactly a
    ///   stream copy reaching the conversation.) Three shapes are excluded, each
    ///   for its own reason: the `in_progress` stream skeleton (it is not
    ///   conversation state yet — A-22's defect was treating it as if it were); an
    ///   ABSENT or non-string `status`, which cannot stand in for an answer because
    ///   the donor contract calls the field REQUIRED on this half (PLAN:1247); and
    ///   a status string outside the vocabulary entirely, which
    ///   `Self::has_unmodelled_status` (private) routes away from here and onto
    ///   whichever membership verdict its own key yields — `CounterpartPresent`,
    ///   `OrphanOutput` or `Unkeyed`, as that accessor's doc below spells out.
    /// - **call**: pairable unless it positively claims `in_progress`. The donor
    ///   contract makes `status` OPTIONAL on this half (PLAN:1248), so absence is
    ///   first-party-normal and the only claim of non-finality the wire makes is
    ///   the skeleton. An unmodelled string on a call stays pairable: the plan's own
    ///   `ToolSearchCall` carries no `status` field at all (PLAN:221), the
    ///   `"completed" | "error"` comment sits on `ToolSearchOutputItem.status`
    ///   (PLAN:222), and PLAN:1035's A2 lint lists `status ∈ {completed, error}` among
    ///   the shape rules without naming a half — the OUTPUT half is where the contract
    ///   puts it (PLAN:1247). A call is a request whose answer's state is not its
    ///   business, so a vocabulary this build has not been told about cannot make it
    ///   un-pairable.
    ///
    /// The two known terminals behave identically here — `completed` (its
    /// definitions may load) and `error` (it carries none, but it DID answer).
    /// Treating `error` as an answer is not a courtesy: a repair pass that sees
    /// [`ToolSearchPairing::UnansweredCall`] synthesises a SECOND output for a
    /// search that already answered.
    ///
    /// Both arms read the RAW string rather than the [`ToolSearchStatus`] view, and that
    /// choice is forward-looking, not load-bearing. [`ToolSearchStatus::from_wire`] maps
    /// each name either arm queries (`in_progress`, `completed`, `error`) to exactly its
    /// own variant and every other value — including an absent or non-string `status` —
    /// to `Unknown`, so no input distinguishes the two forms today — the equivalence is
    /// structural, and holds for an absent, null or non-string `status` as well. What reading
    /// the raw string buys is
    /// immunity to that mapping changing: if a later `from_wire` folds a new wire string
    /// into `Completed`/`Error`/`InProgress`, a gate written against the view silently
    /// changes which items it calls final, and "not a state I model" stops being
    /// distinguishable from "not final".
    fn is_pairable(&self) -> bool {
        match self.kind {
            ToolSearchKind::Output => {
                matches!(
                    self.raw_status(),
                    Some(STATUS_COMPLETED) | Some(STATUS_ERROR)
                )
            }
            ToolSearchKind::Call => self.raw_status() != Some(STATUS_IN_PROGRESS),
        }
    }

    /// Whether `status` is a present string this module's vocabulary does not name.
    ///
    /// True for `""`, `"queued"`, `"cancelled"` — anything readable as a string
    /// that is not `in_progress` / `completed` / `error` (`STATUSES_IN_VOCABULARY`, private).
    /// False for an absent or non-string `status`, which is a DIFFERENT defect: the
    /// output half is contractually required to carry the field at all, and that one
    /// is the `Incomplete` case.
    ///
    /// Its only job is to stop an item the provider already finalised from being
    /// filed under "not conversation state yet" (see [`ToolSearchPairing::Incomplete`],
    /// whose drop-eligible reading is exactly what a provider-minted item must not
    /// get) while ALSO refusing to certify it as one half of a valid pair. Both
    /// directions matter: the reading "unknown terminal ⇒ pairable" would make such
    /// an item [`ToolSearchPairing::Paired`], which tells a repair pass the ordering
    /// and the state are fine when this build cannot tell either. So
    /// `Self::is_pairable` (private) is false for it — it never claims a partner and is
    /// never claimed — and [`pairing_of`] routes it to the membership verdicts
    /// ([`ToolSearchPairing::CounterpartPresent`] / [`ToolSearchPairing::OrphanOutput`]
    /// / [`ToolSearchPairing::Unkeyed`]), which are the only verdicts that assert
    /// nothing about its state.
    ///
    /// Only the OUTPUT arm can reach a consequence: a call with an unmodelled string
    /// is pairable anyway.
    fn has_unmodelled_status(&self) -> bool {
        self.kind == ToolSearchKind::Output
            && self
                .raw_status()
                .is_some_and(|status| !STATUSES_IN_VOCABULARY.contains(&status))
    }

    /// The `execution` field, verbatim.
    ///
    /// TWO values are observed on this wire and they are different quadrants:
    /// `"client"` on the harness-driven searches
    /// (`fixtures/codex/CX1…/next-turn.json` `input[3]`/`input[4]`,
    /// `fixtures/grok-probe/R6-client-loop/next-turn.json` `input[1]`/`input[2]`)
    /// and `"server"` on the provider-executed hosted-search captures
    /// (`captures/2026-09-25-wire-grounding/wire_resp_20260925T062640Z_R1_SOL_HOSTED.json`,
    /// both halves of the pair). The same crate already models both —
    /// `super::responses::ToolSearchExecution::{Server,Client}`. The Messages wire
    /// has no such field; do not fabricate one for it.
    pub fn execution(&self) -> Option<&str> {
        self.raw.get("execution").and_then(Value::as_str)
    }

    /// Exactly: the field is present and says `"client"`.
    ///
    /// NOT a retention predicate, and note the polarity trap — an item with no
    /// `execution` field reads `false` here even though the donor contract calls
    /// the field REQUIRED on both halves (PLAN:1247), so "not client" and "said
    /// server" are collapsed. The plan's orphan rule is written the other way
    /// (`execution != "server"`, PLAN:947), so a pass that removes client orphans
    /// must gate POSITIVELY on this accessor: the delete-able set is
    /// `is_client_executed() && OrphanOutput`, written out on
    /// [`Self::is_server_executed`]. The two shortcuts go wrong on different axes
    /// and are not the same set — one is a superset of the delete-able set, the other
    /// excludes it entirely: deleting whatever FAILS
    /// `is_server_executed` is PLAN:947's literal `execution != "server"`, which adds
    /// the absent- and third-value shapes to the removable set; deleting whatever
    /// FAILS `is_client_executed` — negating this — adds those same two shapes, adds
    /// the exempt server rows, and keeps every client orphan it was meant to remove.
    /// How live is the missing field? On the DECLARATION, attested:
    /// PLAN:1319 records that this campaign's first 21 probes omitted the
    /// declaration's `execution` and all of them landed server-side because of it
    /// (PLAN:1318 is the separate sentence making the DECLARATION's field the quadrant
    /// selector). On the ITEM, unattested: the 31 discovery-item instances in
    /// `captures/` + `ratchet-capture/fixtures/` under the census scope of
    /// [`ToolSearchStatus`] (18 calls, 13 outputs) carry `execution` as a non-null
    /// string — 16 `server`, 15 `client` (calls: 10 client + 8 server; outputs:
    /// 5 client + 8 server). Those 31 instances are 18 distinct `(id, call_id, type)`
    /// keys, and the key count is not an item count either: 13 of the instances
    /// re-record an already-counted item (6 re-listings inside the
    /// `captures/2026-09-25-wire-grounding/wire_grounding_*_summary.json` probe aggregates,
    /// 6 `.sse` stream copies whose finalised form the
    /// NEXT request echoes (`CX1…/response.sse`, `CX3…/response.sse` — neither fixture
    /// holds a standalone response-body FILE; only `R6-client-loop/` has `response.json`. Each
    /// `.sse` does end in a `response.completed` frame whose embedded `response.output[]`
    /// carries only the call half, so the file extension alone decides neither question),
    /// and 1 raw repeat — `fixtures/grok-probe/R6-client-loop/` carries
    /// `call_qPYlpfhYrhWFuCTTLktFETYd` in both `next-turn.json` and `response.json`),
    /// and 3 of the 18 keys belong to aggregate rows keyed on placeholders: all 9
    /// aggregate rows carry the literal `call_id: "<call_id>"`, 8 of them also carry
    /// the literal `id: "<id>"` and the 9th omits `id` outright, so those 3 keys are
    /// aggregate artifacts. The other 15 are real item identities and id presence is
    /// still not uniform across them: 13 carry an `id`, 2 carry none
    /// (`call_qPYlpfhYrhWFuCTTLktFETYd` in `fixtures/grok-probe/R6-client-loop/next-turn.json`
    /// and `call_wire_grounding_stale`, the probe R5 orphan named under
    /// [`ToolSearchPairing::Incomplete`]). The MINT DOMAIN of those 13 decides how far they
    /// reach as ID-SHAPE evidence, so it is stated per id rather than collapsed into one number,
    /// and the test is **which bytes first carried the value**, never which file extension it sits
    /// in. **10 are evidence of what the PROVIDER mints**: each first appears in bytes a model
    /// served — 8 in a `wire_resp_*` `output[]`, one in `CX3…/response.sse`, one in
    /// `R6-client-loop/response.json`. **The other 3 never appear in bytes a real provider served**,
    /// and they are not one mint: `tso_01a0d978-…` and `tso_01a0d989-caf9-…` are codex's own and
    /// appear only in REQUEST bytes, while `tsc_dryrun0` was minted by the campaign's MOCK upstream
    /// — it is a literal in `ratchet-capture/codex-arm/mock_upstream.py:34`, it is
    /// served back in
    /// `CX1…/response.sse` (3 occurrences), it is absent from `CX1…/request.json` (0 occurrences),
    /// and codex only replayed it. That is what `CX1…/meta.json` means when `request_provenance`
    /// says "call replay … are codex-minted" while `response_provenance` says "MOCK
    /// (codex-arm/mock_upstream.py) …" — the string continues "-- response-derived
    /// assertions are NOT donor truth". Reading the extension instead of the provenance is the trap
    /// here: `CX1…/response.sse` IS served bytes — served by the mock — and it would file a
    /// mock-minted id with the provider's. The shape corroborates the split — all 10 provider ids
    /// are `tsc_`/`tso_` + 50 hex characters, while the three above are `tsc_dryrun0` and two dashed
    /// UUIDs. Recon rule G-i (`ratchet-capture/_recon-fixtures.md:924`) bars `tsc_dryrun0`,
    /// `dryrun-search-1` and `resp_dryrun1` from id-shape use; `tso_01a0d978-…` is not named there
    /// and is excluded on the same reasoning rather than on the rule's letter.
    /// `tso_01a0d989-caf9-…` appears only in REQUEST bytes — `CX3…/next-turn.json` `input[12]` and,
    /// outside this population, the live capture
    /// `ratchet-capture/captures/2026-09-25-ratchet-live/wire2-live3/req-004.json` — and in no
    /// stream or response file anywhere, including that capture's own `resp-004.sse` and
    /// `CX3…/response.sse`, neither of which carries a `tool_search_output` at all. Read as
    /// PROVIDER id shape the set is therefore the 10; read as non-provider shape it is the other 3,
    /// and only 2 of those 3 evidence a CLIENT mint (the codex dashed-UUID pair) — `tsc_dryrun0` is
    /// neither, it is the campaign's own mock. The 2 id-less rows are
    /// harness-authored too (`R6-client-loop/meta.json` `"provenance": "OUR probe, not a
    /// donor capture"`; `call_wire_grounding_stale` is hard-coded at
    /// `probe_wire_grounding.py:142`). What the corpus therefore shows is that a
    /// CLIENT-authored row may carry an `id`, may omit it, and may mint it itself; a provider
    /// omitting one has never been observed.
    /// Over those 22 instances
    /// alone, the split is 14 `client` + 8 `server` (the aggregate rows are the ones
    /// that push the server count to 16). One key's copies disagree on `status`, and it
    /// is the same item at two times: `call_AOphypzlL1KKckJugyBS2PYn` is `in_progress`
    /// in `fixtures/codex/CX3-toolsearch-5.5-LIVE/response.sse` and `completed` in that
    /// fixture's `next-turn.json` — skeleton vs finalised echo. The 0-of-31 absent field
    /// is 0-of-22 on the real-wire subset, so the conclusion does not lean on aggregate
    /// rows. Absent `execution` on an item is therefore a robustness path
    /// for bytes we have not been fed, not an observed shape; both accessors stay
    /// exact-match so it claims neither quadrant.
    pub fn is_client_executed(&self) -> bool {
        self.execution() == Some(CLIENT_EXECUTION)
    }

    /// Exactly: the field is present and says `"server"` — the provider-executed
    /// quadrant, whose outputs PLAN:947 exempts from orphan removal absolutely: a
    /// `tool_search_output` whose call_id is missing from the call set "is REMOVED
    /// with telemetry", and "server-executed outputs are NEVER removed (the orphan
    /// rule applies to `execution != \"server\"` only, donor parity)".
    ///
    /// Written as an exact match rather than `!self.is_client_executed()` to mirror
    /// PLAN:947's own predicate. Do NOT read the resulting polarity (absent field =>
    /// `false` => "removable") as the safe direction for a deleting pass — it is the
    /// unsafe one. PLAN:1319 shows absent-`execution` bytes behaving SERVER-side in
    /// this campaign, and PLAN:947 grants its exemption only to an item that SAYS
    /// `"server"`, so a pass that deletes whatever merely failed to say `"client"`
    /// risks deleting provider-minted state: that is the A-26 failure this module
    /// exists to prevent. The delete-able set is therefore `is_client_executed() &&
    /// OrphanOutput` — a DELIBERATELY narrower set than PLAN:947's literal
    /// `execution != "server"`, which also covers an item whose `execution` is absent
    /// or spells a third value. Those fall outside both quadrants — the absent shape is
    /// pinned by `an_absent_execution_claims_neither_quadrant`, the third-value shape by
    /// `an_unrecognised_execution_value_claims_neither_quadrant` — and go to a human or a telemetry
    /// counter rather than the deletion branch; the difference is safe in one direction
    /// only, and this is the direction that keeps provider state on the page. Its price,
    /// stated because the paragraph would otherwise be one-sided: what this narrowing can
    /// keep on the page is an orphan whose `execution` is ABSENT or misspelled. Probe R5 is
    /// not that price and must not be cited as it: its `input[0]` spells `execution:
    /// "client"` (key set `call_id, execution, status, tools, type`), so `is_client_executed`
    /// admits it, so the delete set this predicate feeds reaches it (derived from the
    /// set rule, not observed: no repaired re-request is captured, and this cut ships
    /// no deleting pass). The residual is the absent shape, which this type's own
    /// `execution` census measures at 0 of 31 instances (`0-of-22 on the real-wire
    /// subset`) — a hypothetical cost, not an attested one. That an
    /// absent-`execution` item
    /// lands on NEITHER side is pinned by `an_absent_execution_claims_neither_quadrant`;
    /// which side a pass then puts it on is this paragraph, not the code.
    pub fn is_server_executed(&self) -> bool {
        self.execution() == Some(SERVER_EXECUTION)
    }

    /// `arguments` of a `tool_search_call`: a borrowed OBJECT, never a string.
    ///
    /// This is the fact most often gotten wrong. `function_call.arguments` is
    /// a JSON *string*; `tool_search_call.arguments` is a JSON *object*
    /// (`{"query":"crm order management","limit":8}`,
    /// `CX3-toolsearch-5.5-LIVE/next-turn.json` `input[11].arguments`). The
    /// return type makes the confusion unrepresentable: there is no path from
    /// this accessor to a `serde_json::from_str` step.
    ///
    /// A call whose `arguments` is NOT an object (the string form, say) returns
    /// `None` and is still a valid item: the bytes stay in `raw` and go back out
    /// unchanged, and refusing to load it would hand the caller a decision whose
    /// only easy answer — skip the item — is the one A-26 forbids. Enforcement of
    /// "arguments are an OBJECT" lives at the wire fingerprint (gate assertion
    /// `hts-004`), where a real request can be scored, not here.
    ///
    /// `None` for an output item (which has no `arguments`), for a call that
    /// omits the field, and for a call whose `arguments` is not an object.
    pub fn arguments(&self) -> Option<&serde_json::Map<String, Value>> {
        if self.kind != ToolSearchKind::Call {
            return None;
        }
        self.raw.get("arguments").and_then(Value::as_object)
    }

    /// The `query` of a call, if present.
    ///
    /// Unlike [`Self::id`], [`Self::call_id`] and [`DiscoveredTool::name`], this
    /// getter does **not** filter the empty string. Those three produce
    /// identities, and an empty identity is an absent one; the query is echoed
    /// payload, and `""` is exactly what the model sent. Mapping it to `None`
    /// would drive [`Self::text_summary`] to its `(no query)` fallback, which is
    /// a claim about the bytes that the bytes do not support.
    pub fn query(&self) -> Option<&str> {
        self.arguments()?.get("query").and_then(Value::as_str)
    }

    /// The `limit` of a call, if the value reads as a `u64`.
    ///
    /// That is `Value::as_u64`, which is narrower than "integral": `"limit": 8.0` and a negative
    /// number read `None`, indistinguishable from absent —
    /// `a_degenerate_payload_contributes_nothing_rather_than_a_wrong_value` pins those two. A
    /// magnitude above `u64::MAX` also reads `None`, but never through an overflow arm: it does
    /// not survive parsing as an integer in this crate's `serde_json` configuration, which
    /// `a_limit_too_large_for_u64_arrives_as_a_float_not_as_an_overflow` pins by asserting the
    /// parsed value is a float. This population carries only `3` and `8` here, so all of it is a
    /// robustness path for bytes we have not been fed.
    pub fn limit(&self) -> Option<u64> {
        self.arguments()?.get("limit").and_then(Value::as_u64)
    }

    /// The `tools` entries of a `tool_search_output`, classified in order.
    ///
    /// EMPTY for a call. The wire puts no `tools` array on a
    /// `tool_search_call`, but a payload is untrusted bytes: without the kind
    /// check here, a call carrying one would inject definitions into the
    /// loaded-tool set, and the Messages arm (A-23) would then be obliged to
    /// DECLARE tools no search ever loaded.
    pub fn tools(&self) -> Vec<DiscoveredTool<'_>> {
        if self.kind != ToolSearchKind::Output {
            return Vec::new();
        }
        match self.raw.get("tools") {
            Some(Value::Array(tools)) => tools.iter().map(DiscoveredTool::from_value).collect(),
            _ => Vec::new(),
        }
    }

    /// The stored bytes, verbatim — the single accessor for the item's JSON.
    ///
    /// There is deliberately no second accessor (a `to_input_value()` used to sit
    /// here with the same body and the opposite contract): one shape, one name.
    /// **These are what the STORE keeps, not what a request sends.** The echoed
    /// call carries `created_by`, whose replay is `400 Unknown parameter:
    /// 'input[1].created_by'` (PLAN:1325-1328), so the splice path owes a strip
    /// pass — see [`CALL_REPLAYABLE_KEYS`] for the observed call-half set and
    /// PLAN:1421 for why that pass is T15's, not this type's.
    pub fn raw(&self) -> &Value {
        &self.raw
    }

    /// One bounded, human-readable line for the pager and for text extraction.
    ///
    /// The query is echoed up to [`MAX_SUMMARY_QUERY_BYTES`] and then truncated,
    /// because a `tool_search` query is model-authored and this line is what a
    /// projector has available when it needs a summary rather than the item — an
    /// unbounded echo there is prefix churn on a cached prompt, and prefix churn is
    /// cache invalidation (real money) on every request after this item lands. The
    /// campaign rule behind it ("bounded, stable model-visible items") is item 7 of §6 of
    /// the campaign's operating-rules file (`grok-build-responses/Agents.md` — form 4
    /// above: a sibling worktree, not this repository), which is why the bound and its
    /// cost are restated here rather than left at a bare cite. Never a source of truth:
    /// the raw item
    /// stays intact for replay regardless of what this returns.
    pub fn text_summary(&self) -> String {
        match self.kind {
            ToolSearchKind::Call => match self.query() {
                Some(query) if query.len() > MAX_SUMMARY_QUERY_BYTES => format!(
                    "[tool_search] {:?}…",
                    super::truncate_bytes(query, MAX_SUMMARY_QUERY_BYTES)
                ),
                Some(query) => format!("[tool_search] {query:?}"),
                None => "[tool_search] (no query)".to_owned(),
            },
            ToolSearchKind::Output => {
                let mut namespaces = 0;
                let mut callables = 0;
                for tool in self.tools() {
                    match tool.kind {
                        DiscoveredToolKind::Namespace => {
                            namespaces += 1;
                            callables += tool.callable_definitions().len();
                        }
                        DiscoveredToolKind::Function => callables += 1,
                        DiscoveredToolKind::Other => {}
                    }
                }
                let noun = if callables == 1 { "tool" } else { "tools" };
                if namespaces > 0 {
                    format!("[tool_search results] {callables} {noun} in {namespaces} namespace(s)")
                } else {
                    format!("[tool_search results] {callables} {noun}")
                }
            }
        }
    }

    /// Model-visible length for token accounting.
    ///
    /// The whole payload is model-visible: on Responses the definitions ride
    /// in the cached prefix, so [`Self::text_summary`] understates the cost by
    /// orders of magnitude. Serialises the item to measure it — O(payload) per
    /// call, which is fine per item and not per request over a whole history.
    pub fn estimated_model_visible_len(&self) -> usize {
        self.raw.to_string().len()
    }
}

/// One entry of a `tool_search_output`'s `tools` array, as a view over the
/// parent item's bytes.
///
/// Two shapes are observed in retained bytes and they are NOT interchangeable:
///
/// * a flat callable definition —
///   `{"type":"function","name":"lookup_shipping_eta", ...}`
///   (`fixtures/grok-probe/R6-client-loop/next-turn.json` `input[2].tools[0]`);
/// * a NAMESPACE group whose CHILDREN are the callable definitions —
///   `{"type":"namespace","name":"mcp__ratchet_fixture","tools":[...]}`
///   (`fixtures/codex/CX1-toolsearch-mcp-dryrun/next-turn.json`
///   `input[4].tools[0]`; `CX3-toolsearch-5.5-LIVE/next-turn.json`
///   `input[12].tools[]`).
///
/// A namespace is a GROUP, not a tool: the model invokes the CHILD name, never
/// the namespace name (A-16). Flattening a namespace and losing the group
/// boundary therefore mis-models the discovery set, which is why the group
/// survives here with its children addressable through
/// [`DiscoveredTool::callable_definitions`].
///
/// This is a view, not a copy: it borrows from the parent
/// [`ToolSearchItem::raw`], so an unrecognised field (`strict`,
/// `defer_loading`, or `allowed_callers` — the last one is on live provider
/// bytes today: the flat `lookup_shipping_eta` definition in
/// `captures/2026-09-25-wire-grounding/wire_resp_20260925T062640Z_R1_SOL_HOSTED.json`
/// `output[2].tools[0]` carries
/// `allowed_callers, defer_loading, description, name, output_schema, parameters, strict, type`)
/// cannot be dropped between the
/// provider and the replay — the bytes that go back out are the bytes that
/// came in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveredTool<'a> {
    /// Shape of this entry. Private for the same reason [`ToolSearchItem`]'s are:
    /// a `DiscoveredTool { kind: Function, raw: <a namespace group> }` compiles,
    /// and [`DiscoveredTool::callable_definitions`] would then hand the group
    /// object to the caller as a definition to DECLARE — on the Messages wire that
    /// is an A-23 400 built from a value this module said was a definition.
    kind: DiscoveredToolKind,
    /// Exact provider entry. Private: the pair (`kind`, `raw`) is derived together
    /// by [`ToolSearchItem::tools`] and must stay derived-together.
    raw: &'a Value,
}

/// Shape of a [`DiscoveredTool`] entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveredToolKind {
    /// A directly callable definition (`"type":"function"`).
    Function,
    /// A group whose `tools` array holds the callable definitions.
    Namespace,
    /// A shape this build does not model. Retained verbatim.
    ///
    /// It exists so an entry this build cannot type is still carried and replayed
    /// rather than dropped: on Responses the `tool_search_output` history IS the
    /// loaded-tool set (A-14, PLAN:1532-1533), so dropping an entry the provider
    /// sent is a silent loss of model capability — and `custom` is a live
    /// declaration type on this deployment (PLAN:1904), so "unmodelled" is
    /// not the same as "impossible".
    Other,
}

impl<'a> DiscoveredTool<'a> {
    /// Classify a provider entry by its `type` tag. Private, and outside this module
    /// the only producer is [`ToolSearchItem::tools`], so no caller can make `kind`
    /// disagree with the bytes it claims to describe; inside the module a mismatched
    /// literal compiles, as [`DiscoveredTool`]'s field doc records.
    fn from_value(raw: &'a Value) -> Self {
        let kind = match raw.get("type").and_then(Value::as_str) {
            Some("function") => DiscoveredToolKind::Function,
            Some("namespace") => DiscoveredToolKind::Namespace,
            _ => DiscoveredToolKind::Other,
        };
        Self { kind, raw }
    }

    /// Shape of this entry.
    pub fn kind(&self) -> DiscoveredToolKind {
        self.kind
    }

    /// The exact provider entry, borrowing the parent item's bytes.
    pub fn raw(&self) -> &'a Value {
        self.raw
    }

    /// The entry's `name` as the provider wrote it, `None` when it is absent,
    /// not a string, or empty. For a namespace this is the GROUP name, which is
    /// not invocable — see [`Self::callable_definitions`].
    ///
    /// The empty filter makes this uniform with [`ToolSearchItem::id`] and
    /// [`ToolSearchItem::call_id`], which already refuse `""`. Here that value is
    /// one join away from being a tool identity, through
    /// [`LoadedDefinition::namespace`] and [`invocable_names`] (A-16). No node in
    /// the 76-file corpus census carries an empty `name`, so this is the
    /// defensive branch: the A2 lint (PLAN:1035) constrains ids, `status` and
    /// `tools` being an array, not `name`.
    pub fn name(&self) -> Option<&'a str> {
        self.raw
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
    }

    /// The entry's own `description`, unfiltered — like [`ToolSearchItem::query`] and
    /// unlike the three identity getters ([`ToolSearchItem::id`], [`ToolSearchItem::call_id`]
    /// and [`Self::name`]). PLAN:1035's A2 lint constrains ids, `status` and `tools`; nothing
    /// joins on a description, so an empty one is a real empty description rather than
    /// a missing identity, and refusing `Some("")` here would hide a provider-authored
    /// empty string from the caller.
    pub fn description(&self) -> Option<&'a str> {
        self.raw.get("description").and_then(Value::as_str)
    }

    /// Direct callable definitions this entry contributes, borrowing the
    /// parent's bytes.
    ///
    /// A `Function` yields itself; a `Namespace` yields its `function`
    /// children; `Other` yields nothing, because nothing may be inferred from
    /// a shape we do not model.
    ///
    /// KNOWN SEAM, named rather than papered over: a namespace child whose `type`
    /// is not `function` disappears from the loaded set here without the `Other`
    /// marker its top-level twin gets. No DISCOVERY RESULT has emitted one — the
    /// children of the 3 namespace groups inside the captured `tool_search_output`s
    /// — two different items in two different files: CX1 `input[4].tools[0].tools` x3,
    /// CX3 `input[12].tools[0].tools` x7, CX3 `input[12].tools[1].tools` x1 — are
    /// `function` 11/11, but the
    /// shape is established one field away, on the DECLARATION side: GENUINE bytes
    /// at `fixtures/codex/CX2-codemode-5.6sol-LIVE/next-turn.json`
    /// `input[0].tools[0].tools[0]` are `type:"custom"` inside a namespace group,
    /// and PLAN:1904 spells the deployment's tool composition `function` x10 +
    /// `custom` x1 + `tool_search` x1. So if a provider ever returns a `custom`
    /// child inside a discovery output, this arm contributes nothing callable and
    /// loses the invocation form silently. That is also the seam the Messages-wire
    /// arm needs: that wire requires every referenced tool to be DECLARED (A-23)
    /// and a namespace group is not a declaration of its children.
    pub fn callable_definitions(&self) -> Vec<&'a Value> {
        match self.kind {
            DiscoveredToolKind::Function => vec![self.raw],
            DiscoveredToolKind::Namespace => match self.raw.get("tools") {
                Some(Value::Array(children)) => children
                    .iter()
                    .filter(|child| child.get("type").and_then(Value::as_str) == Some("function"))
                    .collect(),
                _ => Vec::new(),
            },
            DiscoveredToolKind::Other => Vec::new(),
        }
    }

    /// Model-visible length of this entry, for token accounting.
    pub fn estimated_model_visible_len(&self) -> usize {
        self.raw.to_string().len()
    }
}

// ============================================================================
// Pairing and the loaded-tool set
// ============================================================================

/// How one discovery item relates to its counterpart in the same history.
///
/// The two repair rules this feeds are SET-MEMBERSHIP rules, not matching rules:
/// PLAN:947 removes a `tool_search_output` "whose call_id is not in the request's
/// tool-search call set", and PLAN:948 synthesises an answer for "any client call
/// with no output". So [`Self::OrphanOutput`] and [`Self::UnansweredCall`] ask
/// "does a counterpart with this key exist ANYWHERE in the slice?" and NOT "did
/// the 1:1 pass hand me a partner" — see [`Self::CounterpartPresent`] for the
/// difference, which is the difference between leaving history alone and deleting
/// the loaded-tool set. Ordering is a separate, non-destructive fact and it is
/// reported per-item by [`call_precedes_output`] / [`output_follows_call`] and as an exclusive
/// 1:1 pair by [`partner_indices`] — never by a verdict here, and not by
/// [`call_id_groups`], which is order-blind by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSearchPairing {
    /// A copy the 1:1 pass matched to its counterpart — reported by BOTH halves, the
    /// call and the output that follows it under the same `call_id`.
    Paired,
    /// A copy — **pairable or not** (see the last bullet: an output whose `status` this
    /// build cannot name lands here too) — with no 1:1 partner, while a counterpart of the
    /// other half carrying the same `call_id` IS present in the slice.
    ///
    /// **Not a defect, and not repairable here.** Four bullets describe the shapes that
    /// reach it — the first bullet names two shapes — and every one of them is
    /// provider-legal or state-blocked rather than broken:
    ///
    /// * a reused `call_id`, and a search answered twice (the second output has no
    ///   call of its own to match, but the key IS in the call set, so PLAN:947 does
    ///   not remove it). Both are donor-contract shapes — PLAN:1248 makes `call_id`
    ///   an `Option<String>` precisely "accommodating server-null AND codex-reused
    ///   ids" — and neither is OBSERVED in a request. Measured over the 11 discovery-
    ///   carrying SHIPPED arrays in the corpus — SHIPPED meaning an array a real request
    ///   or response actually carried: a request's top-level `input[]` (4), a whole-
    ///   response `output[]` (5), or a `.sse` stream event's `response.output` (2) — and
    ///   excluding the 5 discovery-carrying arrays that live inside the
    ///   `captures/2026-09-25-wire-grounding/wire_grounding_*_summary.json` probe aggregates
    ///   (16 arrays in total scope): 0 of the 11 holds two
    ///   items of the same `(call_id, type)`. The count is still 0 over the wider 16, so
    ///   no aggregate row smuggles the shape in. The repeats a naive sweep finds are the
    ///   SAME item echoed across files (request -> response -> next-turn) and a stream
    ///   re-emitting one item across its own events, never two entries under one key
    ///   inside one array. So this shape is
    ///   contract-legal and unevidenced: do not write a repair path that assumes it
    ///   is common, and do not assume it cannot happen either.
    /// * a pair whose other half is only the stream skeleton.
    /// * a pair in the WRONG ORDER (the output precedes its call — PLAN:946's "must
    ///   follow" is violated, so [`partner_indices`] says `None` and
    ///   [`output_follows_call`]/[`call_precedes_output`] say which side is missing, while
    ///   PLAN:947's call set plainly contains the key).
    /// * a pair whose counterpart is an output whose `status` is a string this build
    ///   cannot name (pairable=false, see `ToolSearchItem::has_unmodelled_status`),
    ///   or this item IS such an output.
    ///
    /// PLAN:946 REACHES the last three bullets — its law is unconditional ("truncation/compaction
    /// touching search items keeps or drops call+output TOGETHER (both kept or both
    /// dropped; carriers byte-identical)"), and the bullet above about the wrong-order
    /// pair already says its "must follow" clause is violated. Two things do NOT come with
    /// that law. Its ENFORCEMENT is quadrant-scoped: the same sentence continues "Enforced
    /// at the encode-time pass: for every `tool_search_call` with `execution:"client"` in
    /// the input", so a server-executed group is outside the pass the plan describes even
    /// though the law still binds it. And its REMEDY does not fire: "restores
    /// the pair from the DURABLE record (T12) or drops both" is introduced by "if the
    /// history lost one half (truncation)", and in all three both halves are present, so
    /// there is nothing to restore and no lost half that authorises dropping them.
    /// Keep that distinction exact: a pass that reads "PLAN:946 does not reach this" here
    /// concludes there is no atomicity duty, drops one half, and ships probe R5's 400 —
    /// the outcome the law exists to prevent.
    /// [`call_id_groups`] names the unit these shapes must be reported in (never
    /// item-by-item: dropping a single half is probe R5's 400) and
    /// [`ToolSearchItem::is_server_executed`] tells you whether PLAN:947's removal
    /// exemption also applies. The repair itself is the caller's decision, and "report
    /// it, change nothing" is a legitimate answer for all three. They are exactly why the
    /// two destructive verdicts below must not be routed from [`partner_indices`] alone:
    /// a pass that routed this with [`Self::OrphanOutput`] would delete real definitions;
    /// a pass that routed it with [`Self::UnansweredCall`] would synthesise a second
    /// answer for a search that already got one.
    CounterpartPresent,
    /// A keyed call with **no** output under its `call_id` anywhere in the
    /// slice (search still in flight, or the turn was cancelled between the two).
    /// This is PLAN:948's set predicate verbatim — "any client call with no
    /// output" — so it is order-blind by construction; whether the answer that
    /// does exist is placed correctly is [`partner_indices`]' question.
    ///
    /// Second — the verdict itself being the first — PLAN:948's rule is scoped to **client**
    /// calls and this verdict does not read `execution`: read
    /// [`ToolSearchItem::is_client_executed`] before synthesising, and treat an absent
    /// `execution` as outside BOTH quadrants — which is the narrowing that accessor's
    /// own doc carries, not PLAN:947's literal (see
    /// `an_absent_execution_claims_neither_quadrant`).
    ///
    /// Third, synthesis also needs the call's **item id**: PLAN:948's planned
    /// `tso_synthetic_id` helper (T15 owns the mint — it is NOT a function in this cut)
    /// is keyed on it, and [`ToolSearchItem::id`] returns `None` for an absent, null
    /// or empty id. A call in this verdict with no `id()` cannot be answered without
    /// minting a key out of nothing, so it takes PLAN:946's other branch (drop the
    /// group) — it must not be given a synthetic answer under a shared empty key.
    ///
    /// Fourth, the slice must not already hold a **client** answer that lost its key —
    /// [`keyless_client_answer_present`] is that test, and it is a slice function because
    /// no item can answer it about itself. Conditions two and three are readable per item
    /// ([`ToolSearchItem::is_client_executed`] and [`ToolSearchItem::id`]); the first,
    /// [`Self::UnansweredCall`], is a slice read too — "no output under its `call_id` anywhere in
    /// the slice" — which is the extent the **Precondition** note on
    /// [`keyless_client_answer_present`] spells out. Conditions one, two and three all hold when the
    /// search DID answer through an output whose `call_id` gives it no join key. PLAN:1248 licenses the CAPTURED form
    /// of that — "`call_id` is `Option<String>` on both (accommodating server-null AND
    /// codex-reused ids)" — and 8 of the corpus's 31 discovery items do carry
    /// `call_id: null`, the two halves of each of the four hosted captures; **0 omit the
    /// key**, though [`ToolSearchItem::call_id`] normalises absent, null and empty to the
    /// same `None`, and [`Self::Unkeyed`] exists to report all three.
    /// That answer is invisible to every keyed reader, so minting PLAN:948's synthetic
    /// row (T15's planned `tso_synthetic_id(call_item_id)`) on top of it double-answers a single search, and
    /// the synthetic row then replays from the durable record. So the licence is a
    /// FOURTH-condition check, and the veto must be scoped before it is applied: a keyless
    /// **client-executed** output vetoes once it has ANSWERED, and ANSWERED means its raw
    /// `status` string is `completed` **or** `error` — the reading
    /// [`keyless_client_answer_present`] implements, not the [`ToolSearchStatus`] view. Say plainly
    /// what that widening is and is not: [`ToolSearchStatus::Error`]'s doctrine — an error
    /// copy "still ANSWERS its call" — is a statement about the KEYED domain, which is the
    /// one thing a vetoing row lacks, so it does not by itself prove the veto must fire
    /// here. The arm is taken as a PRECAUTION on the safe side: a mint is a real synthetic
    /// row that replays from the durable record, and an answer that cannot be attributed is
    /// a reason to stop rather than a reason to mint. The cost is equally real and earlier
    /// drafts of this paragraph left it out: the test is slice-wide, so one such row
    /// suspends PLAN:948 synthesis for EVERY unanswered client call in that slice, and
    /// nothing else in this crate repairs a discovery call — `repair_dangling_tool_calls`
    /// in `conversation.rs` walks `Assistant.tool_calls` → `ToolResult` and never reads a
    /// discovery item. A halt here therefore has no fallback path. The `error` arm is
    /// UNEVIDENCED in the corpus (0 `error` outputs among the 13 captured), which is also
    /// why it vetoes no synthesis the captures currently require.
    /// A status-only scan still does not qualify — every keyless `completed` row the corpus
    /// does contain is the provider-minted
    /// `execution:"server"` hosted pair, the "server-executed (the observed case)" shape
    /// on [`Self::Unkeyed`], which is a different quadrant's artifact and PLAN:947's exempt
    /// one; a veto that fired on it would block PLAN:948 synthesis for every unanswered
    /// client call in any history that also carries a hosted search, on a shape that is
    /// provably not that call's answer. A keyless row carries no key, so nothing can
    /// attribute it to THIS call rather than to another unanswered one; PLAN:946's "must
    /// follow" placement is the closest available scoping, and that residual imprecision
    /// is why this fourth condition is a halt-and-report duty, not a per-call predicate.
    /// Pinned conjunct by conjunct — quadrant, ANSWERED (`completed` and `error` each
    /// firing, `in_progress` not), keylessness — by
    /// `an_unkeyed_answer_is_invisible_to_the_synthesis_licence` and
    /// `an_errored_keyless_answer_vetoes_the_synthetic_mint`, which also show the
    /// unscoped scan firing where the scoped one stays silent. This verdict on its own is
    /// not licence to mint.
    UnansweredCall,
    /// A **keyed** output with **no** call under its `call_id` anywhere in the
    /// slice — PLAN:947's removal set, the copies "whose
    /// call_id is not in the request's tool-search call set", LESS the copies the
    /// `Incomplete` guard claims first. PLAN:947's own words are status-blind; this verdict
    /// is not, and the difference is measurable, not rhetorical: an orphaned keyed output
    /// fed every status shape answers `[Incomplete, OrphanOutput, OrphanOutput,
    /// OrphanOutput, Incomplete]` for `in_progress` / `completed` / `error` / an unmodelled
    /// string / absent — pinned by
    /// `orphaned_outputs_split_between_this_verdict_and_incomplete_by_status`. Do not quote
    /// PLAN:947 at this verdict as though the two sets were equal: a removal pass that
    /// re-derived PLAN:947's set directly would sweep up two shapes this verdict sends to
    /// `Incomplete`. It is the
    /// defect class: probe R5 sent a client `tool_search_output` whose call had
    /// vanished and the boundary answered `400 invalid_request_error` — "No tool
    /// call found for tool search output with call_id …" (PLAN:1161-1162). The
    /// error names a `call_id`, so it cannot fire on an item that has none; see
    /// [`Self::Unkeyed`]. Reading this verdict is NOT licence to delete: PLAN:947
    /// scopes the removal to `execution != "server"`, and this module's delete-able set is
    /// narrower still (`is_client_executed() && OrphanOutput`).
    OrphanOutput,
    /// A copy carrying no `call_id`, so no join is possible in principle — **pairable or
    /// not**: an output whose `status` this build cannot name and no key reaches here too
    /// (the key test runs before the counterpart scan, after the `Incomplete` guard).
    ///
    /// This is a fact about the KEY, not about the quadrant — read
    /// [`ToolSearchItem::is_server_executed`] before deciding anything. The two
    /// shapes it covers are opposite in consequence:
    ///
    /// * **server-executed** (the observed case): the provider mints the pair
    ///   itself with `"call_id": null` on both halves — the four hosted-search
    ///   captures named on [`SERVER_EXECUTION`] carry it, e.g.
    ///   `captures/2026-09-25-wire-grounding/wire_resp_20260925T062640Z_R1_SOL_HOSTED.json`
    ///   `output[1]` + `output[2]`. PLAN:947 exempts this quadrant from orphan
    ///   removal outright, so it must never be dropped: that is the A-26 failure
    ///   mode.
    /// * **client-executed with no key**: not exempt. PLAN:1035's lint requires a
    ///   non-empty `call_id` on outputs, so this is a lint-flagged shape, and
    ///   whether the boundary accepts such a request at all is UNPROBED — no
    ///   capture in the corpus carries a keyless client output. Do not treat it
    ///   as safe to replay, and do not treat it as the PLAN:1159-1162 400 either.
    Unkeyed,
    /// Not a copy pairing accepts: the `in_progress` stream skeleton, or an
    /// output with no usable `status` at all (absent or non-string — PLAN:1247
    /// makes the field REQUIRED on this half). Not conversation state yet, and
    /// never replayable on its own: the skeleton carries `arguments: {}` and no
    /// definitions, so persisting it silently loses the discovery — the write-side
    /// form of the read-side defect A-22 audited (PLAN:1749-1761).
    ///
    /// One other verdict family takes what this one rejects, and it exists because a
    /// provider-minted item must not be filed as "not state yet" (this verdict is the
    /// drop-eligible reading). The second bullet is not a family: [`ToolSearchStatus::Unknown`]
    /// is a state of the STATUS view, not a variant of [`ToolSearchPairing`], so no verdict list
    /// could contain it. It is the caveat that keeps the first from being read as a licence:
    ///
    /// * a `status` this build cannot name (`"cancelled"`, `""`, a future
    ///   vocabulary) is **not** here — see
    ///   `ToolSearchItem::has_unmodelled_status`; it goes to
    ///   [`Self::CounterpartPresent`] / [`Self::OrphanOutput`] / [`Self::Unkeyed`],
    ///   which assert nothing about its state. It is also not [`Self::Paired`]:
    ///   this build cannot certify a state it has never seen, so it never claims a
    ///   partner either.
    /// * [`ToolSearchStatus::Unknown`] is a VIEW state (see its docs) and must
    ///   never become a drop decision on its own.
    ///
    /// **Disposition, stated because no other verdict covers it.** This verdict comes from
    /// a predicate, not from a list: the copy's OWN `status` leaves it unpairable to this
    /// build, and not merely because the status is a string this build cannot name (that one
    /// is rescued first and routed elsewhere). The three bullets below are the CLASSES it
    /// partitions — by item kind, and by whether a call exists under the key — and each
    /// output class spans all three unpairable status forms (`in_progress`, an absent
    /// `status`, a non-string `status`), so the bullets are not an item count and must not
    /// be quoted as one.
    ///
    /// The guard that produces the verdict runs FIRST — before the partner test, the key
    /// test and the counterpart scan — so an item here is never also `Paired`,
    /// `CounterpartPresent`, `Unkeyed`, `UnansweredCall` or `OrphanOutput`, whatever else it
    /// carries. Those five do not have the same guarantee: `Unkeyed`, `UnansweredCall` and
    /// `OrphanOutput` are downstream of the guard's position (`Unkeyed` additionally pinned
    /// by `an_unpairable_copy_is_incomplete_before_it_is_unkeyed`, which owns that
    /// guard-order claim), `CounterpartPresent`
    /// because the counterpart scan sits after the guard too, and the `Paired` one is a
    /// consequence of [`partner_indices`] rather than of the guard's position — that pass
    /// forms a pair only between two pairable copies (it skips an unpairable call and skips
    /// an unpairable output), so no item can reach both branches and moving this guard below
    /// the partner test changes no verdict on any input — structurally, because every copy the
    /// guard fires on is one the pass refuses anyway. The inclusion runs one way: the guard is
    /// `!is_pairable() && !has_unmodelled_status()`, so it is a SUBSET of what the pass refuses —
    /// the unmodelled-status copies are refused by the pass too but are rescued before the guard
    /// (the paragraph above says so). Subset is all the argument needs: guard-fired implies
    /// pass-refused implies no partner. That shadowing is the substance
    /// of this paragraph, because it is exactly
    /// what hides the two plan rules a repair pass would otherwise ask:
    ///
    /// * **an OUTPUT whose call IS in the slice but whose own `status` leaves it
    ///   unpairable** — `in_progress`, absent, or a non-string `status`, and ALL THREE are
    ///   unevidenced on an output: the corpus's 13 `tool_search_output` rows are `completed`
    ///   13/13 and its single `in_progress` discovery item is a CALL (the per-type counts
    ///   [`ToolSearchStatus`] states). Same verdict either way, because the guard asks only
    ///   "is this pairable", not "why is it not". Not persistable (that is
    ///   A-22); PLAN:947 does not authorise removing it (its `call_id` IS in the call set);
    ///   and PLAN:946's law binds its [`call_id_groups`] group — "both kept or both
    ///   dropped … never a lone result — an orphaned result is a proven 400 class". Its
    ///   REMEDY does not fire: restoring from the durable record or dropping both is
    ///   conditioned on "if the history lost one half", and here both halves exist, one of
    ///   them not certifiable by this build. Completing the pair from T12, or dropping the
    ///   WHOLE group, are
    ///   therefore EXTENSIONS of PLAN:946 needing a justification of their own — see
    ///   [`Self::CounterpartPresent`], which draws the same line from the other side.
    /// * **an OUTPUT left unpairable by its own `status` — `in_progress`, absent, or a
    ///   non-string — with no call under its key in the slice.** Nothing is hidden in this
    ///   direction: PLAN:947's own words — a `tool_search_output` whose `call_id` "is not
    ///   in the request's tool-search call set is REMOVED", for `execution != "server"` —
    ///   DO reach it, and this verdict reports `Incomplete` anyway, so a removal pass
    ///   routed on the verdict alone keeps the lone output. Ask PLAN:947's set question
    ///   independently. The other orphan outputs are NOT here: a `completed`, `error` or
    ///   unmodelled-string orphan passes `is_pairable` (or the `has_unmodelled_status`
    ///   rescue, which is where a `status` this build cannot name lands) and is reported
    ///   as [`Self::OrphanOutput`], and that is where the captured 400 lives —
    ///   `captures/2026-09-25-wire-grounding/wire_raw_20260925T062640Z_R5_SOL_STALE_OUTPUT.json`
    ///   `input[0]` is one client output (`call_wire_grounding_stale`, `status: "completed"`,
    ///   no call anywhere in the 2-item array) and the paired response is a 400 — probe R5.
    ///   An orphan of that key shape held back by `status: "in_progress"` instead is
    ///   contract-legal and UNOBSERVED.
    /// * **an `in_progress` CALL** — the only `in_progress` DISCOVERY item the corpus
    ///   contains (`tool_search_call` `in_progress` ×1, `tool_search_output` ×0; measured
    ///   over the corpus census, whose scope [`ToolSearchStatus`] states). Here the shadowing hides PLAN:948's synthesis: this is not
    ///   a call whose answer was lost, it is a half-written request, and minting a synthetic
    ///   empty answer for it would answer a search that has not finished. Its exit is the
    ///   same group decision, not the per-verdict repair.
    ///
    /// So the two premises this paragraph used to rest on — "its `call_id` IS in the call
    /// set" and "the call is present" — are true of the first and third bullets and false
    /// of the second, and a duty that follows from either one is a duty for those two
    /// classes only: it must not be read across the verdict. Nor does either bullet's
    /// group duty reach a copy that has no key: [`call_id_groups`] gives an unkeyed item no
    /// group by design, so a keyless member of these classes is REPORT-ONLY — there is no
    /// group to keep or drop. The failure cuts both ways,
    /// which is why the verdict alone cannot carry it: treating `Incomplete` as "skip this
    /// item" keeps a lone output (second bullet) and loses a loaded-tool set that was
    /// merely mid-stream (first bullet); treating it as "remove the orphan" synthesises a
    /// second answer for a search still running (third bullet) and deletes the definitions
    /// of the first.
    Incomplete,
}

/// The FOURTH PLAN:948 synthesis condition, as a predicate over the whole slice.
///
/// [`ToolSearchPairing::UnansweredCall`]'s doc states the synthesis licence as four
/// conditions. Conditions two and three are readable from one item; the first and the fourth are
/// not — an item can neither know that no output anywhere in the slice carries its key, nor
/// whether the slice holds a client answer that lost its key. This is that fourth condition. It returns `true` when some item is (a) a `tool_search_output`,
/// (b) unkeyed — [`ToolSearchItem::call_id`] is `None`, covering absent, `null` and `""`,
/// (c) **client**-executed, because PLAN:948 governs the client quadrant only, and (d)
/// ANSWERED, i.e. the raw `status` string is `completed` or `error`. Like
/// `ToolSearchItem::is_pairable` this reads the RAW string rather than the
/// [`ToolSearchStatus`] view, for the reason that method states: a gate written against
/// the view silently changes which items it calls ANSWERED if `from_wire` ever folds a
/// new wire string into a named variant. The two forms are equivalent today.
///
/// A `true` is a halt-and-report, not a per-call answer: the row carries no key, so nothing
/// attributes it to this call rather than another, and one such row suspends synthesis for
/// every unanswered client call in the slice.
///
/// **Precondition: `items` is the whole discovery history — the same slice [`pairing_of`] was
/// given.** Extent is what this paragraph exists to state, and it bites on TWO of the four
/// conditions, not one. The veto is the only one a truncated window can silence with no key to
/// scope the damage to: [`ToolSearchPairing::UnansweredCall`] is a whole-slice read as well
/// ("no output under its `call_id` anywhere in the slice"), so a window that drops a KEYED
/// answer fails the same way, one call narrower. Both degrade in the unsafe direction: an
/// answer sitting just outside a narrowed window goes unseen, and a caller that synthesises on
/// that has double-answered a search the history already answered, with the synthetic row then
/// replaying from the durable record. No argument of this function can detect a truncated
/// window, so the obligation is the caller's.
///
/// A `false` licenses nothing by itself. It says only "no keyless client answer is in this
/// slice"; the per-call question is [`ToolSearchPairing::UnansweredCall`], and this predicate
/// never answers whether some GIVEN call was answered — for a keyed search that is
/// [`pairing_of`]'s report.
///
/// Pinned conjunct by conjunct by `an_unkeyed_answer_is_invisible_to_the_synthesis_licence`
/// and `an_errored_keyless_answer_vetoes_the_synthetic_mint`.
pub fn keyless_client_answer_present(items: &[ToolSearchItem]) -> bool {
    items.iter().any(|item| {
        item.kind() == ToolSearchKind::Output
            && item.call_id().is_none()
            && matches!(
                item.raw_status(),
                Some(STATUS_COMPLETED) | Some(STATUS_ERROR)
            )
            && item.is_client_executed()
    })
}

/// Partner index of the 1:1 pair for each discovery item, or `None`.
///
/// This is the **pair** relation, deliberately narrower than PLAN:946's
/// **group**: PLAN:946 makes a `tool_search_call` and "a matching
/// `tool_search_output` (same call_id)" each other's restore-or-drop-both unit,
/// and a reused `call_id` (PLAN:1248) makes that group larger than two items.
/// A caller that has to keep or drop a whole group — the repair path, which is
/// exactly the path PLAN:946 governs — must group on [`ToolSearchItem::call_id`]
/// itself (or use [`call_id_groups`]) and must NOT read "no partner" as "no
/// group": an order-inverted or state-blocked pair has `None` here while still
/// being one group. Dropping one half alone is what produces the orphan shape
/// the provider 400s on (probe R5, PLAN:1161-1162), which is why the pairing
/// invariant (wire invariant 5) is stated at group level, not here.
///
/// `call_id` is the only join key: the two halves carry different item ids and
/// the output's id is optional on the wire (`R6-client-loop/next-turn.json`
/// `input[2]` has none), so nothing else can pair them. Each call is paired
/// with the nearest UNCLAIMED output AFTER it sharing its `call_id`.
///
/// Precondition: `items` is in CONVERSATION order. The join reads position, so a
/// slice ordered by anything else reports a different law — and the widening
/// direction is the dangerous one: an item placed earlier than the wire placed it
/// can be matched to an output it never preceded, so the caller ships a pair that
/// violates PLAN:946's "must follow" clause. See [`call_precedes_output`] / [`output_follows_call`] for the per-item
/// form of the same check — a destructive pass should consult those for ORDER,
/// and nothing more: neither one is a licence to remove or synthesise, as both
/// sets of docs spell out.
///
/// The join is ORDERED because PLAN:946's law is: the matching output "must
/// follow" its call "in the same request". Be exact about what the captures do and
/// do not show. What IS measured is the producer convention, and only inside this census's
/// population (`captures/` + `ratchet-capture/fixtures/`). A sweep of every JSON file there finds
/// **11 arrays holding one `tool_search_call` and one `tool_search_output` together, 0 of them with
/// the output at the lower index**. Four of the 11 are excluded below as aggregate artifacts, which
/// leaves the **7** this doc then enumerates, and only 3 of those 7 are pairs the join described
/// above can even form. The groups are not the same kind of evidence:
///
/// * The 3 **keyed** pairs sit in shipped request arrays
///   (`CX1…/next-turn.json` `input[3]`/`input[4]`,
///   `CX3…/next-turn.json` `input[11]`/`input[12]`,
///   `R6-client-loop/next-turn.json` `input[1]`/`input[2]`). Two of the three are donor
///   bytes — `CX1…/meta.json` `request_provenance` reads "GENUINE first-party codex
///   bytes (declaration, call replay, tool_search_output are codex-minted)" — and the third
///   is this harness's own probe (`R6-client-loop/meta.json` `provenance`: "OUR probe, not a
///   donor capture"), so the convention it shows is partly ours.
/// * The next 4 are hosted pairs the PROVIDER minted into its own `output[]`
///   (`wire_resp_…R1_SOL_HOSTED` `output[1]`/`[2]`, `…R2_TERRA_HOSTED` `output[1]`/`[2]`,
///   `…R2_LUNA_HOSTED_NONE` `output[0]`/`[1]`, `…R1_SOL_HOSTED_ts` `output[1]`/`[2]`). Both
///   halves of all four carry `call_id: null` and `execution: "server"`, so no join forms and
///   this module reads every one of them [`ToolSearchPairing::Unkeyed`]. They evidence the
///   provider's own adjacency habit; they say nothing about the order of a keyed join, and
///   PLAN:946's law is written about `execution:"client"` calls in a request. Only the 3 keyed
///   pairs above are pairs this join forms — the hosted 4 cannot join at all, both halves of each
///   carrying `call_id: null`. So "7 of 7" counts ARRAY NEIGHBOURHOODS, not joined pairs.
/// * The remaining 4 sit in the `wire_grounding_…_summary.json` probe aggregates and are keyed on
///   the literal placeholder `call_id: "<call_id>"`. They are excluded on the same basis the census
///   on [`ToolSearchItem::is_client_executed`] excludes those rows from id evidence: a placeholder
///   key is an aggregate artifact, not an item identity, so the row records that the aggregate
///   listed both halves — not that any producer placed them anywhere. Counting them is what makes
///   the denominator 11 instead of 7.
///
/// Recon rule G-i reaches none of that: its two effects are that CX1's
/// *response*-derived assertions are not donor truth and that its three named values are not
/// ID-SHAPE evidence — placement of items inside a genuine request body is neither. What is
/// UNEVIDENCED is the failure mode: no PERSISTED capture shows the boundary rejecting an inverted
/// pair, and no persisted capture contains an inverted pair to reject — a sweep of every JSON file
/// under the campaign root, well beyond this population, finds 0. The inverted pairs that do exist
/// are manufactured for the purpose of expecting rejection:
/// `ratchet-capture/gate_tool_search_fingerprint.py:3418-3420` re-emits the output half
/// BEFORE the call as a `Mutation(…, expect="FAIL")`; the comment at 3411-3414, governing
/// that mutation and its AFTER-side twin at 3415-3417 (`expect="PASS"`), says that
/// without them "the order clause is unfalsifiable on this fixture set", and
/// `ratchet-capture/ab/tests/test_export_fingerprint_episode.py:108-111`
/// (`test_output_before_call_is_not_a_pair`, `call_index=4, output_index=2`) asserts the exporter
/// refuses that shape. Both are this campaign's own gates, not provider behaviour, so they pin our
/// expectations and still say nothing about what the boundary does. Probe R5 — the one captured
/// 400 in this family — is NOT an ordering case. Its request (`captures/2026-09-25-wire-grounding/wire_raw_20260925T062640Z_R5_SOL_STALE_OUTPUT.json`,
/// read first-hand) holds two items, a client `tool_search_output` and a `message`,
/// and no `tool_search_call` anywhere, so the `400 invalid_request_error` "No tool
/// call found for tool search output with call_id …" (PLAN:1161-1162) answers a
/// request whose key set has NO call in it — PLAN:946's own words name the same class,
/// "never a lone result — an orphaned result is a proven 400 class". So an output
/// placed BEFORE its call is not a pair here because PLAN:946 says so, and no capture
/// yet proves the boundary would reject it; [`call_precedes_output`] carries the same
/// distinction. Either way the pair still belongs to one PLAN:946 group, which is why
/// the verdict for that shape is [`ToolSearchPairing::CounterpartPresent`] rather than
/// a destructive one.
///
/// A partner is claimed once: the result is symmetric by construction
/// (`partners[partners[i]] == Some(i)` for every `Some(i)`). Reused `call_id`s
/// are a named first-party shape (PLAN:1248 says `call_id` is optional partly
/// to accommodate "codex-reused ids"), so without that rule two calls would
/// both claim one output and the map would contradict itself.
///
/// Only `is_pairable` copies take part — see `ToolSearchItem::is_pairable`,
/// private to this module and whose rules the verdicts above state. Without
/// that rule a `call_id`-matched skeleton would consume the real output as its
/// partner and leave the completed call looking unanswered — which is precisely
/// how the A-22 extractors went wrong by reading the first copy of a streamed
/// item.
///
/// Cost: the inner scan restarts per call, so this is O(n²) in the number of
/// discovery items in the slice. A conversation holds tens of searches, not
/// tens of thousands, and every caller here already walks the whole history;
/// a hash on `call_id` is the fix if a capture ever shows that assumption
/// breaking.
pub fn partner_indices(items: &[ToolSearchItem]) -> Vec<Option<usize>> {
    let mut partners = vec![None; items.len()];
    for (i, call) in items.iter().enumerate() {
        // The left half of a pair is always the CALL — the ordering note above.
        // `partners[i]` cannot already be set here: only an OUTPUT is ever
        // written as a right half, so a call's slot is filled by its own pass.
        let (Some(call_id), ToolSearchKind::Call) = (call.call_id(), call.kind) else {
            continue;
        };
        if !call.is_pairable() {
            continue;
        }
        for (j, output) in items.iter().enumerate().skip(i + 1) {
            if output.kind != ToolSearchKind::Output
                || partners[j].is_some()
                || output.call_id() != Some(call_id)
                || !output.is_pairable()
            {
                continue;
            }
            partners[i] = Some(j);
            partners[j] = Some(i);
            break;
        }
    }
    partners
}

/// Whether a pairable CALL carrying the same `call_id` appears BEFORE `output_index`.
///
/// This is PLAN:946's ordering law read one item at a time — "for every
/// `tool_search_call` with `execution:"client"` in the input, a matching
/// `tool_search_output` (same call_id) must follow in the same request" — seen from
/// the output's side, and it is what a projector checks before it ships an output.
/// **The ordering half of that law is PLAN-authority, not a measured 400.** All 7 captured
/// pairs in this census put the call first, but only the 3 keyed pairs in shipped request arrays
/// are pairs this join forms; the other 4 are provider-minted hosted pairs whose halves both
/// carry `call_id: null`, which this module reads [`ToolSearchPairing::Unkeyed`] — adjacency,
/// not ordering (all four are enumerated on [`partner_indices`]). No capture shows the boundary
/// rejecting the inverted shape;
/// probe R5's `400 invalid_request_error` — "No tool call found for tool
/// search output with call_id …" (PLAN:1159-1162) — came from a request containing no
/// `tool_search_call` at all, which is the membership half of the same sentence (see
/// [`partner_indices`], whose ordering paragraph states the distinction in full).
///
/// Two other reporters answer different questions, and the difference is what makes
/// this one necessary:
///
/// * [`ToolSearchPairing`] is ORDER-BLIND by design (it is the set predicate PLAN:947
///   and PLAN:948 use), so it cannot tell a well-ordered pair from an inverted one.
/// * [`partner_indices`] is ordered but EXCLUSIVE: it claims each output once, so with
///   a reused `call_id` (PLAN:1248) a second output gets `None` even though a call
///   does precede it — and a caller that read that `None` as "no call precedes this"
///   would delete a legal, well-ordered answer.
///
/// So this is a plain existential over the preceding range with no exclusivity and no
/// pairing state: pairable, same key, right kind, lower index. It is `false` for a
/// keyless item (nothing to join on) and for an `output_index` outside the slice.
///
/// **It answers an ORDER question and is not a licence to do anything to the item at
/// `output_index`.** It reads that item only for its `call_id`; it never consults
/// `items[output_index].is_pairable()`. So `true` does not certify that this output may
/// ship — `[call, out("in_progress")]` reports `true` at the output even though that
/// row is the OUTPUT-SIDE MIRROR of A-22's skeleton, not the attested shape: the one
/// `in_progress` discovery item the corpus holds is a CALL (the per-item-type status
/// census recorded on `ToolSearchStatus`). That in-progress row is not an answer,
/// but not because its status is unknown to this module: `in_progress` is the first
/// member of
/// `STATUSES_IN_VOCABULARY` and the stream skeleton this type exists for. What makes
/// it not an answer is `is_pairable`'s output arm, which admits only the two terminals
/// — an arm this order predicate deliberately does not apply.
///
/// Symmetrically, `false` does not authorise deletion either: PLAN:947 removes an
/// output whose `call_id` "is not in the request's tool-search call
/// set", which is a SET rule, and that call set plainly contains the key of an inverted
/// pair, of a pair whose call half is still a skeleton, and of every server-executed
/// output the plan exempts from removal absolutely. The verdict that tracks PLAN:947's
/// wording is [`ToolSearchPairing::OrphanOutput`], and even that one needs
/// [`ToolSearchItem::is_client_executed`] before anything is removed.
///
/// **Domain: `output_index` must be the index of a `tool_search_output`.** The
/// symmetric function [`output_follows_call`] takes a call's index, and the two are
/// the two halves of PLAN:946's law asked from the two different items. Asking about
/// the wrong kind is a caller bug: the existential below still answers its literal
/// question, which is meaningless as a repair signal, so a `debug_assert` fires in a
/// debug build. It is compiled out of `--release` — the config this crate's gate runs
/// in — so the wrong-kind answer itself is pinned by a test rather than trusted to
/// the assert, and two `--release`-only pins own that answer.
/// `a_wrong_kind_index_gets_a_meaningless_true_when_the_guard_is_compiled_out` pins what the
/// existentials actually return for a wrong-kind index: in release the answer is not a safe
/// `false`, it is a `true` that means nothing.
/// `a_wrong_kind_index_cannot_be_its_own_counterpart` pins the other half, which is
/// narrower than it looks: the indexed item IS read, but only for its key, and the SCAN RANGE
/// never includes the indexed position (`take(output_index)` / `skip(call_index + 1)`), so a
/// wrong-kind index cannot pair with itself. Both of its answers are `false`, so it must not be
/// read as a pin on the meaningless `true`. That is the same division the FIRST of those two pins
/// states when it says a wrong-kind index gets an answer "computed from the item at the index it
/// was handed ONLY for that item's key".
///
/// Coverage of those two, as three numbers, because a single one was ambiguous: of this
/// module's 90 tests, 86 compile in BOTH profiles, 88 compile in a debug build and 88
/// in a release build — measured in each profile, not derived (they go stale the moment
/// a test is added, so re-measure rather than trust them). The 2 a debug run skips are
/// exactly the pair above; the 2 a release run
/// skips are `handing_a_call_index_to_the_output_predicate_is_caught` and
/// `handing_an_output_index_to_the_call_predicate_is_caught`, the `#[cfg(debug_assertions)]`
/// `#[should_panic]` pair. So the asymmetry runs both ways and both halves matter in this
/// repo: a green debug run has NOT reached the wrong-kind answer this paragraph is about (the
/// `--release` module run is what does), and a green `--release` run — the config the house
/// GATE uses — has NOT reached the two domain guards that catch the same caller bug in a debug
/// build.
///
/// `items` must be the same slice `pairing_of` was given and in **conversation
/// order**: this predicate reads POSITION, so a slice reordered by anything else can
/// report the opposite law — sorting by `call_id` alone, say, can slide an output
/// ahead of its own call and report `false` for a pair the provider accepted. A
/// reordered slice, or one narrower than the history, can therefore produce a `false`
/// that no plan rule sanctions any action for; re-derive the verdicts from the whole
/// history before anything destructive.
pub fn call_precedes_output(items: &[ToolSearchItem], output_index: usize) -> bool {
    if let Some(at) = items.get(output_index) {
        debug_assert_eq!(
            at.kind,
            ToolSearchKind::Output,
            "call_precedes_output takes the index of a tool_search_output"
        );
    }
    let Some(key) = items.get(output_index).and_then(ToolSearchItem::call_id) else {
        return false;
    };
    items.iter().take(output_index).any(|other| {
        other.kind == ToolSearchKind::Call && other.is_pairable() && other.call_id() == Some(key)
    })
}

/// Whether a pairable OUTPUT carrying the same `call_id` appears AFTER `call_index`.
///
/// The call's side of the same PLAN:946 law: a client call's matching output "must
/// follow" it. PLAN:946's sentence is the authority for the ordering half — the
/// captured 400 in this family is the MEMBERSHIP case, a request holding a client
/// `tool_search_output` and no `tool_search_call` at all (probe R5, PLAN:1161-1162),
/// not a pair whose halves were in the wrong order. Like [`call_precedes_output`] this is a plain
/// existential over the following range: no exclusivity (a call whose output was
/// already claimed by an earlier call still has an output after it, and PLAN:947 does
/// not remove either), pairability-aware (the `in_progress` skeleton does not count as
/// an answer — that is the A-22 defect), `false` for a keyless item and for a
/// `call_index` outside the slice.
///
/// **An ORDER answer, not a licence — and it is narrower than it looks from both
/// directions.**
///
/// * `false` does NOT mean "this call has no output". It also fires when an output IS
///   present but this build cannot certify it: `[call, out("cancelled")]`,
///   `[call, out("in_progress")]` and `[call, out(<status omitted>)]` all report
///   `false` at the call while carrying its answer. PLAN:948 fires on "any client call
///   with no output", so a pass that synthesised on this `false` would hand a search
///   a SECOND answer, and PLAN:948's synthetic row persists to the durable history.
/// * [`ToolSearchPairing::UnansweredCall`] strictly implies `false` here (no output with
///   the key anywhere ⟹ none after it), so conjoining this predicate with that verdict
///   can only ever widen the "no output" family, never tighten it. PLAN:948's licence is
///   the conjunction of what the verdict tests and what it deliberately does not read:
///   [`ToolSearchPairing::UnansweredCall`] **and** [`ToolSearchItem::is_client_executed`]
///   (the rule is scoped to client calls) **and** [`ToolSearchItem::id`] returning
///   `Some` (PLAN:948's planned `tso_synthetic_id` is keyed on the call's item id) — and that is
///   three of the four conditions the [`ToolSearchPairing::UnansweredCall`] doc states,
///   the missing one being its keyless-client-answer veto, which a predicate about one
///   index cannot express (it needs the whole slice). Beyond the conjunction this
///   predicate adds placement: whether an answer that exists sits where the boundary will
///   accept it.
/// * `true` does not certify the item at `call_index` either — it reads that item only for
///   its `call_id`, never its own pairability, so `[call("in_progress"), out]` reports
///   `true` at a skeleton call.
///
/// **Domain: `call_index` must be the index of a `tool_search_call`** — see
/// [`call_precedes_output`] for why that boundary is asserted rather than implied, and
/// for the two `#[cfg(not(debug_assertions))]` tests that own the release-build answer
/// (`a_wrong_kind_index_gets_a_meaningless_true_when_the_guard_is_compiled_out`,
/// `a_wrong_kind_index_cannot_be_its_own_counterpart`). Both are invisible
/// to a debug-only run: in debug the `debug_assert` fires first and the release answer
/// is never reached.
///
/// Same slice and same conversation-order precondition as [`call_precedes_output`].
/// Together the two separate PLAN:946's shapes for one key: a well-ordered pair of
/// certified halves (both true, at the partner indices), an order-inverted pair
/// (`call_precedes_output` false at the output AND `output_follows_call` false at the
/// call — the shape whose repair must NOT be a delete, because the answer exists), and a
/// group whose answer is uncertified or genuinely absent, which these two cannot tell
/// apart because they look for a *certified* answer by construction. That distinction is
/// [`ToolSearchPairing`]'s question.
///
/// What to DO with a violation is not this module's call, and the plan does not settle
/// it for every shape: PLAN:946's encode-time pass is written for `execution:"client"`
/// calls whose history **lost one half** (restore from the durable record or drop both),
/// and PLAN:947 exempts server-executed outputs from removal absolutely. A server-
/// quadrant group that violates the ordering law, and a client group that still holds
/// BOTH halves (inverted order, skeleton half, unmodelled status), therefore have NO
/// sanctioned action here — the caller reports them and leaves the bytes alone.
pub fn output_follows_call(items: &[ToolSearchItem], call_index: usize) -> bool {
    if let Some(at) = items.get(call_index) {
        debug_assert_eq!(
            at.kind,
            ToolSearchKind::Call,
            "output_follows_call takes the index of a tool_search_call"
        );
    }
    let Some(key) = items.get(call_index).and_then(ToolSearchItem::call_id) else {
        return false;
    };
    items.iter().skip(call_index + 1).any(|other| {
        other.kind == ToolSearchKind::Output && other.is_pairable() && other.call_id() == Some(key)
    })
}

/// The PLAN:946 restore-or-drop-both GROUPS: every item sharing a `call_id`,
/// regardless of kind, state or order.
///
/// PLAN:946 is a group rule — "for every `tool_search_call` with
/// `execution:"client"` in the input, a matching `tool_search_output` (same
/// call_id) must follow in the same request — if the history lost one half …
/// the pass restores the pair from the DURABLE record (T12) or drops both" —
/// and a reused `call_id` (PLAN:1248) makes a group larger than two items. That
/// is wider than [`partner_indices`], which reports only the valid 1:1 pair: the
/// shapes where `partner_indices` says `None` but a group exists (order
/// inverted, one half still a skeleton, one half over-claimed) are precisely the
/// ones a pass must NOT decide item-by-item, because dropping a single half is
/// probe R5's 400 (PLAN:1161-1162) and synthesising a second half double-answers
/// a search that got one.
///
/// Deliberately state-blind and kind-blind for the same reason as
/// `counterpart_present` (private): the group is defined by the KEY, so an
/// `in_progress` skeleton belongs to its group too, and the caller decides what
/// to do with it (`ToolSearchPairing::Incomplete`). Groups are returned in
/// first-seen order, each group's indices ascending, and keyless items
/// (`call_id` absent, null or empty — the provider-minted hosted-search shape)
/// form no group: there is nothing to restore an answer against, and
/// PLAN:947 exempts that quadrant from removal anyway.
///
/// **A group is not a delete set.** Two scopes narrow what a caller may do with
/// one, and both come from the plan's own wording: PLAN:946 governs "every
/// `tool_search_call` with `execution:"client"`", and PLAN:947 exempts
/// server-executed outputs from removal absolutely. So a group whose members all
/// read [`ToolSearchItem::is_server_executed`] is out of scope for drop-both —
/// deleting provider-minted state is the A-26 failure, not a safe default. Filter
/// on `is_client_executed()` before any destructive action, per item.
///
/// The group is the unit for **delete/restore**, not for every action: PLAN:948's
/// synthesis fires per CALL ("any client call with no output"), so a group holding
/// two unanswered client calls is two syntheses. Note the collision that implies —
/// PLAN:948's planned `tso_synthetic_id` is keyed on the call's item id, so two copies
/// of one call item would mint the same `tso_` id twice; deduplicating that is T15's
/// job, since T15 owns the mint (PLAN:940, PLAN:948).
///
/// Cost is one pass for the keys plus one scan per distinct key — O(k·n), which
/// is quadratic only if every item has its own key; like [`partner_indices`],
/// this assumes tens of discovery items per conversation, not tens of thousands.
pub fn call_id_groups(items: &[ToolSearchItem]) -> Vec<Vec<usize>> {
    let mut keys: Vec<&str> = Vec::new();
    for item in items {
        if let Some(key) = item.call_id()
            && !keys.contains(&key)
        {
            keys.push(key);
        }
    }
    keys.into_iter()
        .map(|key| {
            items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.call_id() == Some(key))
                .map(|(index, _)| index)
                .collect()
        })
        .collect()
}

/// Pairing verdict for every discovery item, in input order.
///
/// Input is the discovery items only, in conversation order; the indices in
/// [`partner_indices`] and in the return of this function are **slice**
/// coordinates, not conversation coordinates — a projector that keeps or drops
/// items in the full conversation must carry its own mapping from this slice
/// back to those positions, or it will act on the wrong item.
///
/// The VERDICTS are slice-scoped too, not just the indices: both destructive
/// verdicts are computed against `items` alone, so filtering the slice before
/// calling this changes the answer. Handing a projector's per-request window to
/// a call whose output lives outside it reports `UnansweredCall` for a search
/// that was answered, and PLAN:947's rule is written about "the REQUEST's call
/// set" — pass the whole discovery history of the request, or accept that the
/// destructive verdicts describe your window and not the wire.
///
/// The two destructive verdicts are MEMBERSHIP tests, matching the rules they
/// feed: PLAN:947 removes an output whose `call_id` "is not in the request's
/// tool-search call set", and PLAN:948 synthesises an answer for a client call
/// that has no output. So an item whose key IS present on the required side but
/// which got no partner from the 1:1 pass is [`ToolSearchPairing::CounterpartPresent`]
/// — leave it alone — and never [`ToolSearchPairing::OrphanOutput`] /
/// [`ToolSearchPairing::UnansweredCall`].
///
/// Neither destructive verdict is sufficient on its own to act on: PLAN:947's
/// removal is scoped by `execution`, and this module acts on the NARROWER
/// `is_client_executed() && OrphanOutput`. That conjunction is WRITTEN OUT on
/// [`ToolSearchItem::is_server_executed`]'s doc — which is not the same as its being
/// the predicate to delete on. The two shortcuts are DIFFERENT sets: deleting
/// whatever FAILS `is_server_executed` is PLAN:947's literal, so it also removes the
/// absent- and third-value shapes this narrowing keeps; deleting whatever FAILS
/// [`ToolSearchItem::is_client_executed`] removes those two shapes AND the exempt
/// server rows, while leaving every client orphan in place. Neither is the
/// delete-able set, and each accessor's own doc records its polarity trap.
///
/// Where these verdicts say a copy is not **pairable**, the rule is spelled out here
/// because it lives on a private method that callers must not route on directly: an OUTPUT
/// is pairable when its `status` is `completed` or `error`, and a CALL is pairable when its
/// `status` is not `in_progress`. An absent or non-string `status` leaves an output
/// unpairable; on a call it does not.
pub fn pairing_of(items: &[ToolSearchItem]) -> Vec<ToolSearchPairing> {
    let partners = partner_indices(items);
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            // An output whose `status` is a string this build cannot name is NOT
            // "not conversation state yet" (it came from the provider finalised),
            // and it is NOT pairable (this build cannot certify its state), so it
            // gets a membership verdict — the only family that asserts nothing
            // about the status itself. See `ToolSearchItem::has_unmodelled_status`.
            // Testing it before `Paired` is equivalent to testing `Paired` first, by
            // invariant rather than luck: `partner_indices` writes a slot only past an
            // `is_pairable` check on both halves, so a claimed row is always pairable
            // and the two branches can never both want this item.
            if !item.is_pairable() && !item.has_unmodelled_status() {
                return ToolSearchPairing::Incomplete;
            }
            if partners[i].is_some() {
                return ToolSearchPairing::Paired;
            }
            let Some(key) = item.call_id() else {
                return ToolSearchPairing::Unkeyed;
            };
            let required = match item.kind {
                ToolSearchKind::Call => ToolSearchKind::Output,
                ToolSearchKind::Output => ToolSearchKind::Call,
            };
            if counterpart_present(items, required, key) {
                return ToolSearchPairing::CounterpartPresent;
            }
            match item.kind {
                ToolSearchKind::Call => ToolSearchPairing::UnansweredCall,
                ToolSearchKind::Output => ToolSearchPairing::OrphanOutput,
            }
        })
        .collect()
}

/// Whether the slice contains any item of `required` (the other half) carrying
/// `key`, ANYWHERE in the slice.
///
/// ORDER-BLIND on purpose, because the two rules this serves are set rules and
/// neither of them mentions order: PLAN:947 removes an output "whose call_id is
/// not in the request's tool-search call set" and PLAN:948 synthesises an
/// answer for "any client call with no output". Both are keyed on
/// membership of the whole request's key set. Restricting the scan to the
/// following/preceding side would invent an ordering requirement the plan does
/// not state, and would then route an order-inverted pair to the DESTRUCTIVE
/// verdicts — deleting a real loaded-tool set (PLAN:947's own exemption makes
/// that the A-26 failure) or synthesising a second answer for a search that
/// already got one.
///
/// The ordering requirement is real but lives one level up: PLAN:946 says a
/// lost half is restored from the durable record **or both are dropped**, which
/// is a group decision, and probe R5's 400 (PLAN:1161-1162) is what the
/// boundary says to a half that ships alone. Both facts are visible to a caller
/// as [`partner_indices`] returning `None` for a keyed pair whose order is
/// inverted — that is the signal to take PLAN:946's path, not the signal to
/// delete or synthesise.
///
/// State-blind for the same reason: an output whose only call is the stream
/// skeleton, and a call whose only output has not reached a terminal state, both
/// have a counterpart in the set, so neither invites a synthetic second answer
/// or a deletion. `items` is scanned in full and no item can match itself: `required`
/// is always the OTHER kind, so the item being asked about is never of `required` and can
/// never be the match it is looking for.
fn counterpart_present(items: &[ToolSearchItem], required: ToolSearchKind, key: &str) -> bool {
    items
        .iter()
        .any(|other| other.kind == required && other.call_id() == Some(key))
}

/// A definition together with the namespace group it was discovered under.
///
/// Fields private, constructed only by [`loaded_tool_set`]: a hand-made
/// `LoadedDefinition { definition, namespace: Some("mcp__x") }` for a definition
/// the provider returned FLAT invents the second invocation form (A-16), and the
/// Messages arm would go on to declare a name nobody discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadedDefinition<'a> {
    /// The callable definition's exact provider JSON.
    definition: &'a Value,
    /// The namespace group name this definition arrived under. `None` means either
    /// the provider returned it flat, or the group had no readable `name` — the
    /// half is sourced from [`DiscoveredTool::name`], which filters absent,
    /// non-string and empty.
    namespace: Option<&'a str>,
}

impl<'a> LoadedDefinition<'a> {
    /// The definition's own `name`, `None` when it is absent, not a string, or
    /// empty — the same filter and the same reason as [`DiscoveredTool::name`].
    /// [`invocable_names`] maps this getter, so an unfiltered `Some("")` would
    /// publish an invocable empty tool name.
    pub fn name(&self) -> Option<&'a str> {
        self.definition
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
    }

    /// The exact definition JSON, as the provider returned it.
    pub fn definition(&self) -> &'a Value {
        self.definition
    }

    /// The namespace group this definition was discovered under. `None` does NOT
    /// prove it came back flat: the half is copied from [`DiscoveredTool::name`], so
    /// a group whose `name` is absent, non-string or empty yields `None` here and the
    /// child is indistinguishable from a flat one (see
    /// `an_unnamed_namespace_group_has_no_namespace_half`). That is the intended
    /// degradation — `super::tool_name::flat_tool_name` collapses `Some("")` to the
    /// unnamespaced form anyway, so `None` is what the sanctioned encoder would emit.
    /// Both forms are needed to invoke a child (A-16).
    pub fn namespace(&self) -> Option<&'a str> {
        self.namespace
    }

    /// Model-visible length of the definition.
    ///
    /// Serialises the whole definition to measure it: a length, not a copy, but
    /// still O(payload) per call — fine across the tens of definitions a search
    /// returns, not something to call per request over a large catalogue.
    pub fn estimated_model_visible_len(&self) -> usize {
        self.definition.to_string().len()
    }
}

/// Every definition the conversation has already loaded, in first-seen order,
/// with namespace children flattened out of their groups.
///
/// A-14 is the identity: "The loaded-tool set IS the `tool_search_output.tools`
/// payload carried in history" (PLAN:1532-1533) — note it carries no completion
/// qualifier. The completion rule is A-22's (PLAN:1749-1757, the loader that
/// fingerprinted the IN-PROGRESS item and scored an empty arguments object).
///
/// A-14's own scope is narrower than this function's name suggests, and reading it
/// as the warrant for cross-wire materialisation is the mistake PLAN:1577-1578
/// records: "**Correction to A-14's scope:** A-14's "loaded set survives a model
/// switch" is INTRA-FAMILY only (gpt-5.5 / gpt-5.6-sol / gpt-5.4). It does NOT
/// extend across families; A-15 is the cross-family answer." The amendment that
/// makes a *set* be materialised onto the Messages wire is A-15/A-23, which PLAN:1851-1852
/// states as the two sides of the projection arm ("A-15/A-23 say the Messages arm
/// must MATERIALISE declarations while the Responses arm must NOT"). What A-14
/// supplies is the identity this function reads — the set IS the `tools` payload —
/// nothing more.
///
/// That makes this the one projection both wire arms need and disagree about:
/// Messages must see all of it DECLARED in `tools[]` (A-23 — a `tool_reference`
/// naming an undeclared tool is a 400), Responses must keep all of it OUT of
/// `tools[]` (A-19/A-23). It is also the direct test for A-24's silent-loss
/// failure mode: non-empty here plus nothing declared on a Messages target means
/// the model has been left without its discovered tools.
///
/// Only a COMPLETED **output** contributes: a skeleton claims nothing and an
/// `error` answer carries `tools: []` by contract (PLAN:22), while the kind check
/// stops a malformed `tool_search_call` that grew a `tools` array from granting
/// definitions nobody searched for. That kind check is deliberately redundant with
/// [`ToolSearchItem::tools`], which gates on kind too, and the redundancy is
/// deliberate: with this check deleted, `tools()` has already returned an empty `Vec`
/// for a call, so **no input distinguishes the two today** and this guard has no test of
/// its own — it is a local invariant, not tested behaviour, and the accessor's gate is
/// what the suite pins (`a_call_never_contributes_definitions_whatever_it_carries`).
/// It stays so that this primitive
/// is safe on its own terms: a future change to `tools()` must not be able to
/// silently turn THIS function into the one that grants definitions from a call.
///
/// De-duplicated on `(definition VALUE, namespace)` — a deep `serde_json::Value`
/// comparison of the whole definition object, NOT a name or a byte compare — so
/// one definition surfaced
/// under two different namespace groups appears twice — the flat form and the
/// namespaced form are different names to invoke (A-16), not one duplicate.
/// Equality is a deep `serde_json::Value` compare against everything accepted so
/// far, which is fine at the tens-of-definitions this slice reaches for and is
/// NOT the function to call per-request over a large catalogue.
pub fn loaded_tool_set<'a>(
    items: impl IntoIterator<Item = &'a ToolSearchItem>,
) -> Vec<LoadedDefinition<'a>> {
    let mut loaded: Vec<LoadedDefinition<'a>> = Vec::new();
    for item in items {
        if item.kind != ToolSearchKind::Output || !item.is_completed() {
            continue;
        }
        for tool in item.tools() {
            let namespace = match tool.kind {
                DiscoveredToolKind::Namespace => tool.name(),
                _ => None,
            };
            for definition in tool.callable_definitions() {
                let entry = LoadedDefinition {
                    definition,
                    namespace,
                };
                if !loaded.contains(&entry) {
                    loaded.push(entry);
                }
            }
        }
    }
    loaded
}

/// The names the loaded set can be invoked by, as the PAIR of forms.
///
/// The two wires invoke a discovered namespace child DIFFERENTLY: Responses
/// uses the child's SHORT name (`crm_fixture_tool_03`), Messages uses the FULL
/// FLAT name (`mcp__codegraph__codegraph_status`) — A-16. The two sides are NOT
/// equally captured and the wording follows the sweep rather than A-16's evidence
/// labels: the Messages side is a real invocation item
/// (`fixtures/claude-code/CC3-discovered-invocation-LIVE/next-turn.json`
/// `messages[5].content[0]`, a `tool_use` naming the flat name; CC1 and CC2 carry
/// only the `ToolSearch` call itself), while a sweep of every JSON document under
/// the corpus root finds NO call item naming `crm_fixture_tool_03` — on the
/// Responses side A-16 rests on the S5 arm plus a model-output mention of the
/// dotted form (PLAN:1526-1527), not on a captured invocation. This returns
/// `(short, namespace)` and deliberately
/// does NOT join them: the one sanctioned encode/decode pair lives in
/// [`super::tool_name`], and inventing a second join here is exactly the fourth
/// vocabulary A-24 warns about. A Responses child short name is not unique
/// across servers, so a wrong join silently resolves to nothing or to another
/// server's tool.
///
/// Read the ambiguity check where it actually lives: `parse_flat_tool_name` is
/// pure syntax and never fails, so it is not the fail-closed site — that is
/// [`super::tool_name::ToolResolutionMap::resolve_short_name`] returning
/// [`super::tool_name::NameResolutionError::AmbiguousShortName`], with
/// [`super::tool_name::ToolResolutionMap::ambiguous_short_names`] as the
/// pre-flight report.
///
/// The vector can repeat a short name: the same definition loaded under two
/// namespaces is two entries (the set deduplicates on `(definition value,
/// namespace)`), so a caller wanting unique names must RESOLVE them rather than
/// de-duplicate this list.
pub fn invocable_names<'a>(
    items: impl IntoIterator<Item = &'a ToolSearchItem>,
) -> Vec<(&'a str, Option<&'a str>)> {
    loaded_tool_set(items)
        .iter()
        .filter_map(|loaded| loaded.name().map(|name| (name, loaded.namespace)))
        .collect()
}

/// Every discovery item in a conversation history, with its conversation index.
///
/// The helpers on this slice-level API (`pairing_of`, `partner_indices`,
/// `call_id_groups`) index into a slice of `ToolSearchItem`, not into a history;
/// this is the adapter that carries a history's indices into them, so a caller
/// that must act on the HISTORY (truncate, split, retain) can address the real
/// positions instead of slice positions.
pub fn discovery_items(items: &[ConversationItem]) -> Vec<(usize, &ToolSearchItem)> {
    items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| item.discovery().map(|found| (index, found)))
        .collect()
}

/// The discovery groups in a history, as conversation indices.
///
/// Grouping is by `call_id` for the keyed quadrant and by ORDER for the
/// provider-minted `call_id: null` quadrant. Both rules are KIND-aware: a group is
/// a `tool_search_call` plus the `tool_search_output` that answers it, so items of
/// one kind never make a pair however they share a key
/// ([`partner_indices`] joins across kinds for the same reason, and
/// `same_kind_items_never_pair_even_when_they_share_a_call_id` records the donor's
/// reused-`call_id` shape as a real one).
///
/// The unkeyed quadrant has no join key, so order is the only signal: a null-key
/// call groups with the NEXT null-key output anywhere later in the history, not
/// only with a physically adjacent one. A synthetic `User`/`System` row injected
/// between the halves is a shape the harness itself produces, and reading it as two
/// unrelated records is what let a compaction window delete a COMPLETE pair (cut
/// review F-1).
///
/// This is a grouping helper, never a deletion policy: it drops nothing. It claims
/// only what `items` can support, so a caller that wants to DELETE on the answer
/// must hand it the full history — see [`unpaired_discovery_indices`].
fn discovery_groups(items: &[ConversationItem]) -> Vec<Vec<usize>> {
    let discovery = discovery_items(items);
    let mut groups: Vec<Vec<usize>> = Vec::new();
    // Keyed quadrant: one group per key, holding every item that carries it. A
    // same-key run of two calls is therefore grouped and then reported NOT closed
    // by [`group_is_closed`], rather than being split into two singletons that a
    // caller could read as two independent lone items.
    let mut claimed: Vec<usize> = Vec::new();
    for (index, item) in &discovery {
        if claimed.contains(index) {
            continue;
        }
        let Some(key) = item.call_id() else {
            // The unkeyed quadrant is grouped below, in document order.
            continue;
        };
        let group: Vec<usize> = discovery
            .iter()
            .filter(|(_, other)| other.call_id() == Some(key))
            .map(|(other_index, _)| *other_index)
            .collect();
        claimed.extend(group.iter().copied());
        groups.push(group);
    }
    // Unkeyed quadrant: FIFO over document order — a null-key call takes the next
    // null-key output, and an output with no open call is its own group.
    let mut open_calls: Vec<usize> = Vec::new();
    for (index, item) in &discovery {
        if item.call_id().is_some() {
            continue;
        }
        match item.kind() {
            ToolSearchKind::Call => open_calls.push(*index),
            ToolSearchKind::Output => match open_calls.is_empty() {
                true => groups.push(vec![*index]),
                false => groups.push(vec![open_calls.remove(0), *index]),
            },
        }
    }
    for call in open_calls {
        groups.push(vec![call]);
    }
    // Ascending by first member, which is the group's minimum in both quadrants;
    // [`snap_index_over_discovery_pairs`] no longer relies on that order, but the
    // ledger-style callers want a deterministic one.
    groups.sort_by_key(|group| group.first().copied().unwrap_or(usize::MAX));
    groups
}

/// Whether a group actually answers itself: at least one `tool_search_call` AND at
/// least one `tool_search_output`.
///
/// `group.len() >= 2` is NOT this question (cut review F-3): two `tool_search_call`s
/// sharing a reused `call_id` are a length-2 group that answers nothing, and the
/// wire shape that keeps one of them is the strict-backend 400 this helper family
/// exists to prevent.
fn group_is_closed(discovery: &[(usize, &ToolSearchItem)], group: &[usize]) -> bool {
    let holds = |kind: ToolSearchKind| {
        group.iter().any(|index| {
            discovery
                .iter()
                .any(|(found, item)| *found == *index && item.kind() == kind)
        })
    };
    holds(ToolSearchKind::Call) && holds(ToolSearchKind::Output)
}

/// Whether the LAST item of `items` is a discovery half that cannot be sent as it
/// stands: a `tool_search_call` at the very tail is unanswered, and a
/// `tool_search_output` whose call does not immediately precede it (same `call_id`,
/// or both keyless as the provider mints them) cannot be certified as paired.
///
/// An answered `[call, output]` tail is NOT unpaired: the pair is the provider's own
/// record of the loaded tool set and must never be stripped (ruling apex-waj.18
/// A-26). This is the SUMMARISER/RECAP prep question, so it is deliberately stricter
/// than [`unpaired_discovery_indices`] — it asks about the ORDER the wire reads, not
/// just about key membership, and an output that precedes its call is exactly as
/// unsendable as no output at all.
///
/// Owned here so every prep trim answers the same question with the same rules:
/// `xai-chat-state`'s `truncate_trailing_incomplete_tool_call` (reached only
/// through `prepare_conversation_for_verbatim_summarization`, i.e. summariser/recap
/// input prep) and the `xai-grok-shell` recap pop. **It is NOT a pre-send guard**:
/// no outbound model request runs it — `truncate_trailing_incomplete_tool_call`'s
/// only caller is the summariser prep above, and the recap/compaction callers feed a
/// summariser, not the next turn (cut review WAJ21R2-04). The only defence a lone
/// `tool_search_call` meets on the way to the wire is the outbound lint's H-8
/// call-side arm, which OBSERVES and never mutates the request.
pub fn trailing_discovery_is_unpaired(items: &[ConversationItem]) -> bool {
    let Some((index, item)) = discovery_items(items).last().copied() else {
        return false;
    };
    if index + 1 != items.len() {
        // The tail is some other item type; nothing discovery-shaped to guard.
        return false;
    }
    match item.kind() {
        ToolSearchKind::Call => true,
        ToolSearchKind::Output => {
            match index
                .checked_sub(1)
                .and_then(|previous| items.get(previous))
                .and_then(ConversationItem::discovery)
            {
                Some(before) => {
                    before.kind() != ToolSearchKind::Call
                        || (item.call_id().is_some() && before.call_id() != item.call_id())
                }
                None => true,
            }
        }
    }
}

/// The conversation indices of discovery items that are NOT part of a closed group.
///
/// A closed group holds a `tool_search_call` and the `tool_search_output` that
/// answers it ([`group_is_closed`]). A window that keeps one half of a pair and not
/// the other is not a smaller history, it is a different (invalid) one: a lone
/// `tool_search_call` is the strict-backend 400 shape and a lone
/// `tool_search_output` references a call the provider never saw. So a caller that
/// must cut a history mid-pair — a compaction tail window, a summariser slice —
/// drops the returned indices and keeps every closed group verbatim.
///
/// **The argument is the whole scope of the answer.** Grouping can only use the
/// items it is handed, so passing a WINDOW and deleting on this answer is how a
/// complete pair gets stripped: the cut review's F-1 shape is a `call_id: null`
/// pair whose halves sit on either side of a window edge, which the window reads as
/// two lone items and deletes — the A-26 violation, silently, out of the only
/// verbatim content a compacted history keeps. A caller deleting MUST hand this the
/// FULL history (and move its cut instead, see
/// [`snap_index_over_discovery_pairs`]) so that "no partner" is certified against
/// every item the session holds and not a slice of it.
pub fn unpaired_discovery_indices(items: &[ConversationItem]) -> Vec<usize> {
    let discovery = discovery_items(items);
    let mut unpaired: Vec<usize> = discovery_groups(items)
        .into_iter()
        .filter(|group| !group_is_closed(&discovery, group))
        .flatten()
        .collect();
    unpaired.sort_unstable();
    unpaired
}

/// The widest discovery group a history cut may be lowered across.
///
/// A real pair is narrow. The provider mints `[call, output]` adjacently, and the
/// widest interleaving the harness itself produces is a handful of sibling rows
/// between the halves (reasoning rows, a `User` echo, one tool result) — the
/// straddling shapes every fixture in this module carries span 3 and 4. A group
/// spanning more rows than this is not one pair: it is the reused-`call_id` shape
/// this module's own notes call "a length-2 group that answers nothing"
/// ([`discovery_groups`], keyed quadrant) or a keyless call that never got its
/// answer. Letting one of those drag the cut back turns a pair guard into a
/// history-deletion mechanism: the keyed rule groups EVERY item sharing a key with
/// no distance bound, so one colliding `call_id` at turn 5 and its answer at turn
/// 60 would rewind a rewind/cancel/replay/fork-copy cut across 55 turns of
/// unrelated history and then PERSIST the shortened history (the snap sits on
/// `conversation_truncate_for_prompt`, which is a disk write). That is a far larger
/// silent loss than the half-pair it was preventing (cut review WAJ21R2-03).
///
/// Refusing is the safe direction for a second reason: an over-wide group's answer
/// cannot be certified at all, so the body it produces is the loud failure (a
/// strict-backend 400, flagged on the wire by the outbound lint's H-8 call-side
/// arm), never a silent whole-turn loss.
pub const MAX_SNAP_GROUP_SPAN: usize = 8;

/// Whether a group is narrow enough for the snap to honour it, see
/// [`MAX_SNAP_GROUP_SPAN`].
fn group_is_snappable(group: &[usize]) -> bool {
    let (Some(first), Some(last)) = (
        group.iter().min().copied(),
        group.iter().max().copied(),
    ) else {
        return false;
    };
    last - first + 1 <= MAX_SNAP_GROUP_SPAN
}

/// Snap a history cut DOWN so it can never fall between a `tool_search_call` and
/// the `tool_search_output` that answers it.
///
/// `cut` is a split index in `items`: everything below it is on one side,
/// everything at or above it on the other. That covers every cut this codebase
/// makes — a rewind truncation, the head of a retained budget window, a two-pass
/// split, the start of a compaction tail window — and in every one of them a cut
/// inside a pair leaves one half alone on the wrong side: a call with no answer
/// (a strict-backend 400) or an answer with no call (the reverse desync).
///
/// The snap only ever moves DOWN, to the group's first item, because that is the
/// one direction safe for both readings of a cut: a truncation then drops the
/// whole pair, and a retained window keeps the whole pair. It never moves UP,
/// which would silently retain a half.
///
/// Grouping is [`discovery_groups`]. This is a pair-atomicity guard, never a
/// deletion policy: it drops nothing.
///
/// The snap is BOUNDED. A group wider than [`MAX_SNAP_GROUP_SPAN`] is not one
/// pair, and lowering across it would delete whole turns of unrelated history to
/// save a pair that no wire reading can certify anyway — see
/// [`MAX_SNAP_GROUP_SPAN`] and
/// `a_reused_call_id_spanning_whole_turns_does_not_drag_the_cut_back`.
pub fn snap_index_over_discovery_pairs(items: &[ConversationItem], cut: usize) -> usize {
    let groups = discovery_groups(items);
    let splits = |group: &[usize], at: usize| {
        let first = group.iter().min().copied().unwrap_or(0);
        let last = group.iter().max().copied().unwrap_or(0);
        // `first < at <= last` is exactly "the cut splits this group": some member is
        // below the cut and some is at or above it.
        first < at && at <= last
    };
    let first_of = |group: &[usize]| group.iter().min().copied().unwrap_or(0);
    let mut snapped = cut;
    // One pass is not a fixpoint, and a pass that mutates `snapped` as it walks the
    // group list is order-dependent (cut review F-2): with interleaved searches
    // `[callA, callB, outA, outB]` and cut 3, group `[1,3]` drops the cut to 1, which
    // splits the already-visited group `[0,2]`. So each pass folds every group to the
    // LOWEST first member it forces, regardless of iteration order, and only then
    // moves. Each pass strictly lowers `snapped` and a group's first member is an
    // index into `items`, so at most `items.len()` passes can lower it — the bound is
    // structural, not a hope, and leaving the loop returns a fixed point (a further
    // lowering would require a strictly lower group start, which the last pass would
    // have found).
    for _pass in 0..items.len() {
        let next = groups
            .iter()
            .filter(|group| group_is_snappable(group))
            .filter(|group| splits(group, snapped))
            .map(|group| first_of(group))
            .min()
            .unwrap_or(snapped);
        if next >= snapped {
            if let Some(too_wide) = groups
                .iter()
                .find(|group| !group_is_snappable(group) && splits(group, snapped))
            {
                // Loud, not silent: the cut still splits this group and the snap
                // will NOT fix it. Nothing else in the pipeline reports a split pair
                // whose halves are both present-but-far-apart, so this is the only
                // witness that the body about to be sent is the shape a strict
                // backend rejects (cut review WAJ21R2-03).
                let too_wide_span = too_wide.iter().max().unwrap_or(&0)
                    - too_wide.iter().min().unwrap_or(&0)
                    + 1;
                tracing::error!(
                    span = too_wide_span,
                    max_span = MAX_SNAP_GROUP_SPAN,
                    group = ?too_wide,
                    cut = snapped,
                    "refusing to snap a cut across an over-wide discovery group: an \
                     answer that far from its call is not a pair, and lowering here \
                     would delete the turns between them (apex-waj.21 WAJ21R2-03)"
                );
            }
            return snapped;
        }
        snapped = next;
    }
    snapped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::tool_name::FLAT_TOOL_NAME_DELIMITER;
    use serde_json::json;

    /// `CX3-toolsearch-5.5-LIVE/next-turn.json` `input[11]` — the completed
    /// call, verbatim.
    fn cx3_call() -> Value {
        json!({
            "type": "tool_search_call",
            "id": "tsc_02005a6c7856d15c016ab6aa70e9208194bc939a9b4707c556",
            "call_id": "call_AOphypzlL1KKckJugyBS2PYn",
            "status": "completed",
            "execution": "client",
            "arguments": { "query": "crm order management", "limit": 8 }
        })
    }

    /// A namespace result STRUCTURALLY like `CX1-toolsearch-mcp-dryrun/next-turn.json`
    /// `input[4]`: same item id, `call_id`, namespace group name and the capture's
    /// full child COUNT (measured: the capture's group holds exactly 3 children and
    /// this fixture keeps all 3), but each child is ABBREVIATED — the capture's
    /// children also carry a long `description` and a second parameter, which this
    /// fixture drops. Do not read this as the capture's bytes: it is a fixture with
    /// that item's identity and nothing here asserts byte-equality against CX1. The
    /// `create_time` float is the capture's, because a float is exactly the value an
    /// integer-typed view would silently eat.
    ///
    /// The unknown field that must survive the store round-trip is
    /// `internal_chat_message_metadata_passthrough` — and its provenance is a
    /// caution, not a credential: PLAN:1492-1495 corrects the earlier reading and
    /// records it as ABSENT from the live items, "seen only behind mock_upstream",
    /// because CX1 is the mock-driven dry run. So it is a good carrier for "unknown
    /// fields survive" and a bad one for "the wire sends this". `created_by` is the
    /// opposite: observed on live provider bytes and a proven replay hazard
    /// (PLAN:1325-1328).
    fn cx1_namespaced_output() -> Value {
        json!({
            "type": "tool_search_output",
            "id": "tso_01a0d978-6771-7420-8b21-567a1f96b61c",
            "call_id": "dryrun-search-1",
            "status": "completed",
            "execution": "client",
            "tools": [{
                "type": "namespace",
                "name": "mcp__ratchet_fixture",
                "description": "Deterministic fixture server for wire-fingerprint capture.",
                "tools": [
                    { "type": "function", "name": "crm_fixture_tool_00",
                      "strict": false, "defer_loading": true,
                      "parameters": { "type": "object", "properties": {
                          "customer_id": { "type": "string" } },
                          "required": ["customer_id"] } },
                    { "type": "function", "name": "crm_fixture_tool_06",
                      "strict": false, "defer_loading": true,
                      "parameters": { "type": "object", "properties": {
                          "customer_id": { "type": "string" } },
                          "required": ["customer_id"] } },
                    { "type": "function", "name": "crm_fixture_tool_09",
                      "strict": false, "defer_loading": true,
                      "parameters": { "type": "object", "properties": {
                          "customer_id": { "type": "string" } },
                          "required": ["customer_id"] } }
                ]
            }],
            "internal_chat_message_metadata_passthrough": {
                "turn_id": "01a0d978-6741-7721-aa1d-aae236e4ed3e",
                "create_time": 1790354941.809797
            }
        })
    }

    /// The provider-executed pair from
    /// `captures/2026-09-25-wire-grounding/wire_resp_20260925T062640Z_R1_SOL_HOSTED.json`
    /// `output[1]` / `output[2]`. Field set AND key order are the capture's
    /// (`tools` sits between `status` and `type` on the output, which is why the
    /// order is written out here rather than normalised); the one edit is that
    /// `output[2].tools[0]` is reduced to `name` + `type` — the capture's entry
    /// is a FLAT function definition also carrying `parameters`, `strict`,
    /// `allowed_callers`, `defer_loading`, `description` and `output_schema`; of those,
    /// only `description` has an accessor here ([`DiscoveredTool::description`]) and this
    /// reduction leaves it unexercised — its `Some` reading is pinned on the CX1
    /// namespaced group instead; the other five keys are not read at all.
    /// The two shapes that matter are `execution:"server"` and **`call_id: null`
    /// on both halves**: the provider mints the pair and does not use a join key.
    fn sol_hosted_server_pair() -> [Value; 2] {
        [
            json!({
                "id": "tsc_0ce980d5c6afd41f016ab61423e6ec81908939d7d041618fb1",
                "arguments": { "paths": ["lookup_shipping_eta"] },
                "call_id": null,
                "execution": "server",
                "status": "completed",
                "type": "tool_search_call",
                "created_by": null
            }),
            json!({
                "id": "tso_0ce980d5c6afd41f016ab61424031481908982e2c788dcc429",
                "call_id": null,
                "execution": "server",
                "status": "completed",
                "tools": [{ "name": "lookup_shipping_eta", "type": "function" }],
                "type": "tool_search_output",
                "created_by": null
            }),
        ]
    }

    /// `fixtures/grok-probe/R6-client-loop/next-turn.json` `input[2]`
    /// — the client-authored result: flat definition form, and NO `id`.
    fn r6_flat_output() -> Value {
        json!({
            "type": "tool_search_output",
            "call_id": "call_qPYlpfhYrhWFuCTTLktFETYd",
            "status": "completed",
            "execution": "client",
            "tools": [{
                "type": "function",
                "name": "lookup_shipping_eta",
                "description": "Look up the shipping ETA for an order ID.",
                "parameters": { "type": "object", "properties": {
                    "order_id": { "type": "string" } },
                    "required": ["order_id"], "additionalProperties": false },
                "strict": true
            }]
        })
    }

    /// `CX3/response.sse` `output_item.added` `output_index: 1` — the stream
    /// skeleton: same ids, `status: in_progress`, empty arguments.
    fn cx3_skeleton_call() -> Value {
        json!({
            "id": "tsc_02005a6c7856d15c016ab6aa70e9208194bc939a9b4707c556",
            "type": "tool_search_call",
            "status": "in_progress",
            "arguments": {},
            "call_id": "call_AOphypzlL1KKckJugyBS2PYn",
            "execution": "client"
        })
    }

    fn item(raw: Value) -> ToolSearchItem {
        ToolSearchItem::from_wire(raw).expect("fixture item must validate")
    }

    /// The `call_id` of the CX3 pair, reused by the hand-built fixtures below so a
    /// test can put a call and an output in the same PLAN:946 group.
    const CX3_KEY: &str = "call_AOphypzlL1KKckJugyBS2PYn";

    /// A client call on a chosen key; `status: None` omits the field, which is legal
    /// on this half (PLAN:1248).
    fn keyed_call(call_id: &str, status: Option<&str>) -> ToolSearchItem {
        let mut raw = json!({
            "type": "tool_search_call",
            "id": format!("tsc_probe_{call_id}"),
            "call_id": call_id,
            "execution": "client",
            "status": "completed",
            "arguments": { "query": "crm order management", "limit": 8 }
        });
        match status {
            Some(status) => raw["status"] = json!(status),
            None => {
                raw.as_object_mut().unwrap().remove("status");
            }
        }
        item(raw)
    }

    /// The raw JSON of a client output on a chosen key, carrying zero definitions.
    /// `status: None` omits the field — which PLAN:1247 says is not legal on this
    /// half, and that illegality is itself a case under test. A `Some(Value)` lets a
    /// test feed a non-string status.
    fn output_raw(call_id: &str, status: Option<Value>) -> Value {
        let mut raw = json!({
            "type": "tool_search_output",
            "id": format!("tso_probe_{call_id}"),
            "call_id": call_id,
            "execution": "client",
            "status": "completed",
            "tools": []
        });
        match status {
            Some(status) => raw["status"] = status,
            None => {
                raw.as_object_mut().unwrap().remove("status");
            }
        }
        raw
    }

    /// A client output on a chosen key with zero definitions; `status: None` omits
    /// the field.
    fn keyed_output(call_id: &str, status: Option<&str>) -> ToolSearchItem {
        item(output_raw(call_id, status.map(|status| json!(status))))
    }

    /// One captured call read through the accessors that expose its fields, each one
    /// asserted against a literal from the fixture — the whole-row pin behind the
    /// per-accessor docs. It is not every getter: `arguments()`, the verbatim
    /// `execution()` string, `raw()`, `text_summary()`, `is_server_executed()` and
    /// `estimated_model_visible_len()` are pinned in the tests that follow — as is
    /// `kind()`, which this test reads as the field rather than the accessor — and
    /// `tools()` is deliberately not pinned here (see the note inside).
    #[test]
    fn call_reads_every_field_from_the_live_bytes() {
        let call = item(cx3_call());
        assert_eq!(call.kind, ToolSearchKind::Call);
        assert_eq!(
            call.id(),
            Some("tsc_02005a6c7856d15c016ab6aa70e9208194bc939a9b4707c556")
        );
        assert_eq!(call.call_id(), Some("call_AOphypzlL1KKckJugyBS2PYn"));
        assert_eq!(call.status(), ToolSearchStatus::Completed);
        assert!(call.is_completed());
        assert!(call.is_client_executed());
        assert_eq!(call.query(), Some("crm order management"));
        assert_eq!(call.limit(), Some(8));
        // `tools()` returning empty for THIS fixture proves nothing — the fixture
        // has no `tools` key, so it passes with or without the kind gate. That
        // gate is pinned where it can actually fail, in
        // `a_call_never_contributes_definitions_whatever_it_carries`.
    }

    /// The object-vs-string confusion, and what this type does about it.
    ///
    /// The documented protocol says object and every byte in the corpus is an
    /// object, so the object form is the one the accessors expose. A string form
    /// is NOT refused at construction — see [`ToolSearchItem::from_wire`] for why
    /// a discovery item is never unloadable — it is degraded to no view while its
    /// bytes survive untouched, so the request still replays what the provider
    /// sent. Enforcement of "arguments are an OBJECT" is the wire fingerprint's
    /// (`hts-004`), where a request can actually be scored.
    #[test]
    fn arguments_is_an_object_and_a_string_form_degrades_the_view() {
        let call = item(cx3_call());
        let arguments = call.arguments().expect("arguments are an object");
        assert_eq!(arguments.get("query").unwrap(), "crm order management");

        let mut broken = cx3_call();
        broken["arguments"] = json!("{\"query\":\"crm order management\"}");
        let parsed = item(broken.clone());
        assert_eq!(parsed.kind(), ToolSearchKind::Call);
        assert!(
            parsed.arguments().is_none(),
            "a string is not an object view"
        );
        assert!(parsed.query().is_none());
        assert_eq!(
            parsed.raw(),
            &broken,
            "and the bytes still go back out unchanged"
        );
    }

    /// The call-side views are kind-gated, and the gate is not decorative: an
    /// output that grew an `arguments` object still must not read as a query, or
    /// the summary and every log line built from it would claim that an answer
    /// was a search.
    #[test]
    fn an_output_never_exposes_a_call_view() {
        let mut output = cx1_namespaced_output();
        output["arguments"] = json!({ "query": "crm order management", "limit": 8 });
        let output = item(output);
        assert_eq!(output.kind(), ToolSearchKind::Output);
        assert!(output.arguments().is_none());
        assert!(output.query().is_none());
        assert!(output.limit().is_none());
    }

    /// Both observed authoring policies must load: donor codex writes a
    /// `tso_*` id, our own probe writes none. The type reads what is there and
    /// invents nothing.
    #[test]
    fn output_id_is_optional_and_never_synthesised() {
        assert_eq!(item(r6_flat_output()).id(), None);
        assert_eq!(
            item(cx1_namespaced_output()).id(),
            Some("tso_01a0d978-6771-7420-8b21-567a1f96b61c")
        );

        // An EMPTY string is not a handle. The A2 lint requires a non-empty id
        // where the field is present (PLAN:1035), and `""` is what this campaign has
        // actually put in front of the harness — on the in-repo `emptyid_x69` fixture,
        // NOT on captured provider bytes (see the `id()` docs, where the sweep result
        // is recorded). The accessor must not report one as an identity — same
        // normalisation `call_id()` applies.
        let mut empty_id = cx1_namespaced_output();
        empty_id["id"] = json!("");
        assert_eq!(
            item(empty_id).id(),
            None,
            "an empty id reads as absent, not as Some(\"\")"
        );

        // A non-string `id` is the third unreadable shape and `and_then(Value::as_str)`
        // is what rejects it. UNEVIDENCED here — every captured `id` in the census is a
        // string — so this pins the robustness path, not an observed byte.
        let mut number_id = cx1_namespaced_output();
        number_id["id"] = json!(123);
        assert_eq!(
            item(number_id).id(),
            None,
            "a non-string id is not an identity either"
        );
    }

    /// A status string outside the pairing vocabulary is neither skeleton nor
    /// certified pair: the VIEW degrades to `Unknown`, the item is NOT
    /// `Incomplete` (it is provider-minted conversation state — A-26 forbids
    /// handing out the drop-eligible verdict for vocabulary this build is merely
    /// unfamiliar with), and it is NOT `Paired` either (this build cannot certify
    /// a state it has never seen, so it must not tell a repair pass that the
    /// ordering and the state are fine). It lands on the membership verdicts, and
    /// `loaded_tool_set` still refuses its definitions (only `completed` loads).
    #[test]
    fn an_unmodelled_status_is_neither_a_skeleton_nor_a_certified_pair() {
        // `" completed"` and `"COMPLETED"` are the exact-match controls in this family:
        // a padded or differently-cased spelling is outside the vocabulary, so a read that
        // normalised the string — by trimming or by case-folding — before comparing it
        // would certify the item as final right here. Both arms are needed: the execution
        // family at `an_unrecognised_execution_value_claims_neither_quadrant` pins its own
        // case arm through `SERVER`/`CLIENT`, and this is the status family's equivalent.
        for status in ["cancelled", "expired", "queued", " completed", "COMPLETED"] {
            let call = item(cx3_call());
            let output = keyed_output(CX3_KEY, Some(status));
            assert_eq!(
                output.status(),
                ToolSearchStatus::Unknown,
                "the VIEW degrades — that is what Unknown is for"
            );
            assert!(
                !output.is_pairable(),
                "an unmodelled {status:?} claim cannot be certified as final"
            );
            assert_eq!(
                partner_indices(&[call.clone(), output.clone()]),
                vec![None, None],
                "and it claims no partner under {status:?}"
            );
            assert_eq!(
                pairing_of(&[call.clone(), output.clone()]),
                vec![
                    ToolSearchPairing::CounterpartPresent,
                    ToolSearchPairing::CounterpartPresent,
                ],
                "the key IS in both sets, so neither the destructive verdicts nor \
                 the skeleton verdict may fire under {status:?}"
            );
            // The ORDER is fine and stays reported as fine; it is the STATE this
            // build cannot name. `call_precedes_output` reads position, not status.
            assert!(
                call_precedes_output(&[call.clone(), output.clone()], 1),
                "the call really does precede it under {status:?}"
            );
            assert!(
                !output_follows_call(&[call.clone(), output.clone()], 0),
                "no certifiable answer follows the call under {status:?}"
            );
            // The fixture must be one that WOULD load: `keyed_output` carries
            // `tools: []`, so a zero here would prove nothing about the completion gate.
            let mut defs = cx1_namespaced_output();
            defs["status"] = json!(status);
            defs["call_id"] = json!(CX3_KEY);
            let defs = item(defs);
            assert_eq!(
                loaded_tool_set([&defs]).len(),
                0,
                "only `completed` loads definitions, under {status:?} or any other"
            );
        }
        // Control: the SAME bytes with the vocabulary's own terminal do load, which is
        // what makes the `loaded_tool_set` assert above a test rather than a tautology.
        let mut completed = cx1_namespaced_output();
        completed["call_id"] = json!(CX3_KEY);
        let completed = item(completed);
        assert_eq!(
            loaded_tool_set([&completed]).len(),
            3,
            "the group's 3 children load when the status says `completed`, so the \
             zero above is the gate working, not an empty fixture"
        );
    }

    /// An absent or non-string `status` on the output half is a DIFFERENT defect
    /// from an unmodelled string: PLAN:1247 makes the field REQUIRED there, so it
    /// cannot stand in for an answer and does reach `Incomplete`. `""` is a present
    /// string, so it is the unmodelled case, not this one.
    #[test]
    fn an_output_without_a_readable_status_is_a_skeleton_not_an_unmodelled_answer() {
        let call = keyed_call(CX3_KEY, Some("completed"));
        for unreadable in [json!(null), json!(7), json!({})] {
            let output = item(output_raw(CX3_KEY, Some(unreadable.clone())));
            assert_eq!(
                output.status(),
                ToolSearchStatus::Unknown,
                "the view cannot read {unreadable} either"
            );
            assert_eq!(
                pairing_of(&[call.clone(), output.clone()]),
                vec![
                    ToolSearchPairing::CounterpartPresent,
                    ToolSearchPairing::Incomplete
                ],
                "unreadable status on the output half is Incomplete, not a \
                 membership verdict (input: {unreadable})"
            );
        }
        // Absent key, same verdict.
        let output = item(output_raw(CX3_KEY, None));
        assert_eq!(
            pairing_of(&[call.clone(), output.clone()]),
            vec![
                ToolSearchPairing::CounterpartPresent,
                ToolSearchPairing::Incomplete
            ],
            "an omitted REQUIRED status is Incomplete too"
        );
        // An EMPTY string is a present string outside the vocabulary, so it goes
        // the other way — and that routing must not leak into `id()`/`call_id()`,
        // which both filter `""`.
        let empty = keyed_output(CX3_KEY, Some(""));
        assert_eq!(
            pairing_of(&[call.clone(), empty.clone()]),
            vec![
                ToolSearchPairing::CounterpartPresent,
                ToolSearchPairing::CounterpartPresent
            ],
            "an empty-string status is a claim this build cannot name, not a \
             missing field"
        );
        let mut keyed = output_raw(CX3_KEY, None);
        keyed["call_id"] = json!("");
        assert_eq!(
            item(keyed).call_id(),
            None,
            "the empty-string filter on the JOIN key stays in place"
        );
    }

    // ---- PLAN:946's ordering law, reported per item -------------------------

    /// PLAN:946 says the matching output "must follow" its call, so an output placed
    /// BEFORE its call satisfies neither ordering law — while its key stays present on
    /// both sides, which is what keeps the pairing verdicts non-destructive. The law
    /// being checked here is PLAN:946's own: the captured 400 in this family (probe R5,
    /// PLAN:1161-1162) came from a request with no call in it at all, so no capture
    /// shows the boundary rejecting an inverted pair.
    #[test]
    fn an_output_before_its_call_satisfies_neither_ordering_law() {
        let items = [
            keyed_output(CX3_KEY, Some("completed")),
            keyed_call(CX3_KEY, Some("completed")),
        ];
        assert!(
            !call_precedes_output(&items, 0),
            "no pairable call precedes it"
        );
        assert!(
            !output_follows_call(&items, 1),
            "no output follows the call"
        );
        assert_eq!(partner_indices(&items), vec![None, None]);
        assert_eq!(
            pairing_of(&items),
            vec![
                ToolSearchPairing::CounterpartPresent,
                ToolSearchPairing::CounterpartPresent
            ],
            "the key IS in both sets, so PLAN:947/948 must not fire"
        );
    }

    /// A second output for one call has no 1:1 partner — `partner_indices` says
    /// `None` — yet a pairable call DOES precede it. A pass that read the `None` as
    /// "no call precedes this" would PLAN:947-delete a well-ordered answer. This
    /// fixture says nothing about what was lost when it did: `keyed_output` carries
    /// `tools: []`, so the definitions-loss consequence is pinned in
    /// `a_second_output_for_one_call_is_not_an_orphan`, which uses a populated output.
    #[test]
    fn a_second_output_for_one_call_still_has_a_call_before_it() {
        let items = [
            keyed_call(CX3_KEY, Some("completed")),
            keyed_output(CX3_KEY, Some("completed")),
            keyed_output(CX3_KEY, Some("completed")),
        ];
        assert_eq!(
            partner_indices(&items),
            vec![Some(1), Some(0), None],
            "the 1:1 pair is exclusive"
        );
        assert_eq!(
            pairing_of(&items),
            vec![
                ToolSearchPairing::Paired,
                ToolSearchPairing::Paired,
                ToolSearchPairing::CounterpartPresent
            ]
        );
        assert!(
            call_precedes_output(&items, 2),
            "and that is the difference between leaving it alone and deleting it"
        );
        assert!(output_follows_call(&items, 0));
    }

    /// The mirror case: a reused key whose second call has nothing after it. That
    /// ONE call is PLAN:946's lost half — and note the pairing verdict for it is
    /// `CounterpartPresent` (the key IS in the output set, from the first pair), so
    /// only the ordering predicate can see the hole.
    #[test]
    fn a_reused_key_call_with_an_earlier_output_is_plan_946_s_lost_half() {
        let items = [
            keyed_call(CX3_KEY, Some("completed")),
            keyed_output(CX3_KEY, Some("completed")),
            keyed_call(CX3_KEY, Some("completed")),
        ];
        assert_eq!(partner_indices(&items), vec![Some(1), Some(0), None]);
        assert_eq!(
            pairing_of(&items),
            vec![
                ToolSearchPairing::Paired,
                ToolSearchPairing::Paired,
                ToolSearchPairing::CounterpartPresent
            ]
        );
        assert!(
            !output_follows_call(&items, 2),
            "PLAN:948 must not fire on it (its key is answered) and PLAN:946 must \
             not treat it as a whole pair: this is the group-decision case"
        );
        assert_eq!(
            call_id_groups(&items),
            vec![vec![0, 1, 2]],
            "so the repair unit is the whole group: the answer sitting at [1] can be\
             re-paired with this call or the group goes, but [1] must not be deleted\
             on the strength of [2]'s verdict"
        );
    }

    /// Both ordering predicates are PAIRABILITY-aware: the `in_progress` skeleton is
    /// not an answer (A-22's defect was counting it as one), so a call whose only
    /// output is a skeleton has no output after it — while PLAN:947's set rule stays
    /// state-blind and keeps the call out of `UnansweredCall`.
    #[test]
    fn the_ordering_predicates_ignore_a_copy_pairing_cannot_use() {
        let skeleton = {
            let mut raw = r6_flat_output();
            raw["status"] = json!("in_progress");
            raw["call_id"] = json!(CX3_KEY);
            item(raw)
        };
        let items = [keyed_call(CX3_KEY, Some("completed")), skeleton];
        assert!(
            !output_follows_call(&items, 0),
            "a skeleton does not count as an answer"
        );
        assert!(
            call_precedes_output(&items, 1),
            "the call before it is COMPLETED, so position and pairability both check out"
        );
        // The same filter on the other side: a SKELETON CALL is not the call the
        // law requires before an output.
        let skeleton_call = {
            let mut raw = cx3_skeleton_call();
            raw["call_id"] = json!(CX3_KEY);
            item(raw)
        };
        let after_skeleton = [skeleton_call, keyed_output(CX3_KEY, Some("completed"))];
        assert!(
            !call_precedes_output(&after_skeleton, 1),
            "the preceding call never left the skeleton state"
        );
        // And the join key still has to match: a DIFFERENT search's call, however
        // well-formed and however early, is not this output's call.
        let wrong_key = [
            keyed_call("call_a_different_search", Some("completed")),
            keyed_output(CX3_KEY, Some("completed")),
        ];
        assert!(
            !call_precedes_output(&wrong_key, 1),
            "the ordering law is keyed, not positional"
        );
        assert_eq!(
            pairing_of(&items),
            vec![
                ToolSearchPairing::CounterpartPresent,
                ToolSearchPairing::Incomplete
            ],
            "and the SET rule stays state-blind — no synthesis for a call whose \
             answer is still streaming"
        );
    }

    /// The mirror of the assertion above, on [`output_follows_call`]: the
    /// indexed item is read for its JOIN KEY and nothing else, so `true` at a skeleton
    /// call says what follows it and certifies nothing about the call itself. Without
    /// this pin, conjoining `&& items[call_index].is_pairable()` into the predicate
    /// changes no verdict in the file — which is exactly why the pin exists: with that
    /// conjunction added, the whole module suite passes without it.
    #[test]
    fn a_skeleton_call_still_reports_the_certified_answer_that_follows_it() {
        let items = [
            item(cx3_skeleton_call()),
            keyed_output(CX3_KEY, Some("completed")),
        ];
        assert!(
            output_follows_call(&items, 0),
            "the predicate reads the indexed call only for its key"
        );
        assert_eq!(
            pairing_of(&items),
            vec![
                ToolSearchPairing::Incomplete,
                ToolSearchPairing::CounterpartPresent
            ],
            "the verdicts DO read the state this predicate ignores"
        );
    }

    /// The KIND filter is load-bearing on both predicates: two outputs sharing a key
    /// do not make a pair, and neither do two calls. Without it a group of same-kind
    /// items would satisfy PLAN:946's law on paper, and the slice below holds no call
    /// at all — shipping that output is the captured 400 class, probe R5's
    /// (PLAN:1161-1162).
    #[test]
    fn a_same_kind_neighbour_never_satisfies_the_ordering_law() {
        let two_outputs = [
            keyed_output(CX3_KEY, Some("completed")),
            keyed_output(CX3_KEY, Some("completed")),
        ];
        assert!(
            !call_precedes_output(&two_outputs, 1),
            "the item before it is another output, not the call the law wants"
        );
        let two_calls = [
            keyed_call(CX3_KEY, Some("completed")),
            keyed_call(CX3_KEY, Some("completed")),
        ];
        assert!(
            !output_follows_call(&two_calls, 0),
            "the item after it is another call, not an answer"
        );
    }

    /// A keyless item has nothing to order against, and an index outside the slice
    /// reads false rather than panicking: both predicates are existential over a
    /// range and a projector must be able to call them on any index it holds.
    #[test]
    fn a_keyless_item_and_an_out_of_range_index_order_against_nothing() {
        let pair = sol_hosted_server_pair();
        let items = [item(pair[0].clone()), item(pair[1].clone())];
        assert_eq!(
            pairing_of(&items),
            vec![ToolSearchPairing::Unkeyed, ToolSearchPairing::Unkeyed]
        );
        assert!(
            !call_precedes_output(&items, 1),
            "call_id is null: no key to join on"
        );
        assert!(!output_follows_call(&items, 0));
        assert!(!call_precedes_output(&items, 99));
        assert!(!output_follows_call(&[], 0));
    }

    // ---- what each ordering answer does NOT license ------------------------

    /// The join is keyed on BOTH sides. `call_precedes_output`'s wrong-key case sits in
    /// `the_ordering_predicates_ignore_a_copy_pairing_cannot_use`; this is the mirror,
    /// which that fixture cannot reach — a different-key OUTPUT belongs AFTER the call,
    /// so it can only be caught from the call's side.
    #[test]
    fn output_follows_call_does_not_answer_for_another_search() {
        let items = [
            keyed_call("call_mine", Some("completed")),
            keyed_output("call_someone_elses", Some("completed")),
        ];
        assert!(
            !output_follows_call(&items, 0),
            "a well-formed answer to a DIFFERENT search, sitting immediately after the \
             call, is not this call's output"
        );
        assert_eq!(
            pairing_of(&items),
            vec![
                ToolSearchPairing::UnansweredCall,
                ToolSearchPairing::OrphanOutput
            ],
            "and the set rule agrees: neither key is answered"
        );
        // Positive control on the same shape, so the assert above cannot be satisfied by
        // a predicate that simply always answers false.
        let shipped = [
            keyed_call(CX3_KEY, Some("completed")),
            keyed_output(CX3_KEY, Some("completed")),
        ];
        assert!(
            output_follows_call(&shipped, 0),
            "same key, order intact: the law is satisfied"
        );
        // And the scan is the WHOLE tail, not the next row. PLAN:946 says the answer
        // "must follow", and partner_indices pairs a call with the nearest unclaimed
        // output after it wherever that sits — so an interleaved row from another
        // search does not close the window. Without this case an implementation that
        // looked only at `call_index + 1` passes everything else in the suite.
        let gapped = [
            keyed_call(CX3_KEY, Some("completed")),
            keyed_output("call_someone_elses", Some("completed")),
            keyed_output(CX3_KEY, Some("completed")),
        ];
        assert!(
            output_follows_call(&gapped, 0),
            "two rows after the call is still after it"
        );
    }

    /// PLAN:948's precondition is "any client call with no output", NOT "with no
    /// certified output after it". Every shape below HAS an answer in the slice and still
    /// reports `false`, because the order predicate only counts a pairable answer: it is
    /// the wider family, and [`ToolSearchPairing::UnansweredCall`] (which strictly
    /// implies `false`, never the reverse) is the membership test the plan names. A pass
    /// that synthesised on `false` alone would hand each of these searches a SECOND
    /// answer, and PLAN:948's synthetic row persists to the durable history.
    #[test]
    fn the_synthesis_licence_is_the_verdict_not_the_order_answer() {
        let uncertified: [(&str, ToolSearchItem, ToolSearchPairing); 3] = [
            (
                "cancelled",
                item(output_raw(CX3_KEY, Some(json!("cancelled")))),
                ToolSearchPairing::CounterpartPresent,
            ),
            (
                "in_progress",
                item(output_raw(CX3_KEY, Some(json!("in_progress")))),
                ToolSearchPairing::Incomplete,
            ),
            (
                "status omitted",
                item(output_raw(CX3_KEY, None)),
                ToolSearchPairing::Incomplete,
            ),
        ];
        for (label, out, out_verdict) in uncertified {
            let items = [keyed_call(CX3_KEY, Some("completed")), out];
            assert!(
                !output_follows_call(&items, 0),
                "{label}: the answer is in the slice but this predicate cannot certify it"
            );
            assert_eq!(
                pairing_of(&items),
                vec![ToolSearchPairing::CounterpartPresent, out_verdict],
                "{label}: the set rule still sees the answer, so PLAN:948 must not fire"
            );
        }
        // A call that never claimed a status is the same case, not a fourth one:
        // `status` is OPTIONAL on this half (PLAN:1248), so absence keeps it pairable and
        // still unanswered — it must not fall through to `Incomplete` and escape the rule.
        let statusless = [keyed_call(CX3_KEY, None)];
        assert_eq!(
            pairing_of(&statusless),
            vec![ToolSearchPairing::UnansweredCall],
            "absence on the CALL half is normal, so PLAN:948 still sees an open call"
        );
        // The one shape PLAN:948 does fire on, with everything its licence needs.
        // `output_follows_call` answers `false` here exactly as it did in all three
        // shapes above — which is precisely why it cannot be the discriminator.
        let items = [keyed_call(CX3_KEY, Some("completed"))];
        assert_eq!(
            pairing_of(&items),
            vec![ToolSearchPairing::UnansweredCall],
            "no output with the key anywhere: this is the membership test"
        );
        assert!(!output_follows_call(&items, 0));
        assert!(
            items[0].is_client_executed(),
            "PLAN:948 is scoped to client calls; the verdict does not read execution"
        );
        assert!(
            items[0].id().is_some(),
            "PLAN:948's synthetic row is keyed on the call's item id (tso_synthetic_id), \
             so a call with no item id cannot be given one"
        );
    }

    /// The unmodelled-status routing produces a MEMBERSHIP verdict in every shape; what
    /// a caller may DO depends on the key and the quadrant, never on the status. The
    /// `CounterpartPresent` case has its own test; this one runs four slices across the three
    /// membership shapes it does not reach on its own, with `OrphanOutput` taking two of the four:
    /// `Unkeyed` once, then `OrphanOutput` in the client quadrant, then the same bytes in the
    /// server quadrant, and last `CounterpartPresent` itself, which re-enters from server bytes and
    /// is why that fourth slice is run here rather than there.
    #[test]
    fn an_unmodelled_status_changes_the_verdict_route_in_no_other_shape() {
        // Keyless: it reaches the routing (the `Incomplete` guard runs first), but its
        // membership verdict can only ever be `Unkeyed`, because the membership
        // verdicts join on a key it does not have. This is also the assertion that
        // catches a routing guard narrowed too far — drop the
        // `&& !item.has_unmodelled_status()` escape from that guard and THIS test's first
        // assertion fails, `[Incomplete]` against `[Unkeyed]`. It is not the only test that
        // reddens: `an_unmodelled_status_is_neither_a_skeleton_nor_a_certified_pair` and
        // `orphaned_outputs_split_between_this_verdict_and_incomplete_by_status` fail on
        // their own unmodelled-status rows, and which of them the harness runs first is not a
        // property `cargo test` promises.
        let mut keyless = output_raw(CX3_KEY, Some(json!("cancelled")));
        keyless.as_object_mut().unwrap().remove("call_id");
        assert_eq!(
            pairing_of(&[item(keyless)]),
            vec![ToolSearchPairing::Unkeyed],
            "no key: Unkeyed, whatever the status says"
        );
        // Keyed, client, no call in the slice: PLAN:947's removal DOES reach it. The
        // rationale for the routing ("it came from the provider finalised") is not an
        // exemption — the exemption is the quadrant.
        let lone = item(output_raw(CX3_KEY, Some(json!("cancelled"))));
        assert_eq!(
            pairing_of(std::slice::from_ref(&lone)),
            vec![ToolSearchPairing::OrphanOutput]
        );
        assert!(
            lone.is_client_executed(),
            "so this one is delete-eligible under PLAN:947, unmodelled status and all"
        );
        // Same bytes, server quadrant: identical verdict, forbidden removal.
        let mut server_out = output_raw(CX3_KEY, Some(json!("cancelled")));
        server_out["execution"] = json!(SERVER_EXECUTION);
        let server_out = item(server_out);
        assert_eq!(
            pairing_of(std::slice::from_ref(&server_out)),
            vec![ToolSearchPairing::OrphanOutput],
            "the verdict does not read execution"
        );
        assert!(
            server_out.is_server_executed() && !server_out.is_client_executed(),
            "the caller has to, and PLAN:947 exempts it absolutely"
        );
        // Both halves present, server quadrant, unmodelled status on the output: neither
        // half is destructive, and the order predicate still claims no certified answer.
        let server_call = item(json!({
            "type": "tool_search_call",
            "id": "tsc_r7_server_quadrant",
            "call_id": CX3_KEY,
            "execution": SERVER_EXECUTION,
            "status": STATUS_COMPLETED,
            "arguments": { "query": "lookup_shipping_eta" }
        }));
        let items = [server_call, server_out];
        assert_eq!(
            pairing_of(&items),
            vec![
                ToolSearchPairing::CounterpartPresent,
                ToolSearchPairing::CounterpartPresent
            ]
        );
        assert!(
            !output_follows_call(&items, 0),
            "the cancelled answer is not pairable, so nothing follows that call"
        );
        // No positive placement claim here on purpose, and not because the array is
        // disordered: the call IS pairable and the output does sit after it, so
        // `call_precedes_output` would answer `true` on this slice too. That positive
        // answer is pinned by `an_unmodelled_status_is_neither_a_skeleton_nor_a_certified_pair`;
        // the negative half below (an uncertified answer certifies nothing) is what this
        // slice is for, and `the_synthesis_licence_is_the_verdict_not_the_order_answer`
        // loops that half across the uncertified shapes. Asserting the positive here
        // would pin nothing new.
    }

    /// The `debug_assert_eq!` guards own the debug-build contract. In `--release` — the
    /// config this crate's gate runs in — they are compiled out and a wrong-kind index
    /// silently receives the literal existential answer, computed from the item at
    /// the index it was handed ONLY for that item's key. That answer can be `true`, which is why the boundary is
    /// a caller obligation and not a suggestion. Pinned here, in the one config where
    /// the guard is absent; `a_same_kind_neighbour_never_satisfies_the_ordering_law`
    /// pins the same kind filter in-domain, where the guard would also have been live.
    #[cfg(not(debug_assertions))]
    #[test]
    fn a_wrong_kind_index_gets_a_meaningless_true_when_the_guard_is_compiled_out() {
        // Index 1 is an OUTPUT, so asking the call-side predicate about it is the caller
        // bug. Index 2 is a same-key pairable OUTPUT, so the existential answers `true`:
        // a claim about an item that is not a call at all, about an output that is
        // already the answer to index 0.
        let items = [
            keyed_call(CX3_KEY, Some("completed")),
            keyed_output(CX3_KEY, Some("completed")),
            keyed_output(CX3_KEY, Some("completed")),
        ];
        assert!(
            output_follows_call(&items, 1),
            "the release answer is the literal existential, not a domain-checked false"
        );
        assert_eq!(
            pairing_of(&items),
            vec![
                ToolSearchPairing::Paired,
                ToolSearchPairing::Paired,
                ToolSearchPairing::CounterpartPresent
            ],
            "and the set rule says something different again: the 1:1 pass claims \
             0<->1 and leaves index 2 unclaimed"
        );
        // Mirror bug: index 1 is a CALL asked of the output-side predicate, with a
        // same-key pairable CALL before it. `true` here claims an ordering law between
        // two calls, while the set rule reports both as unanswered.
        let mirror = [
            keyed_call(CX3_KEY, Some("completed")),
            keyed_call(CX3_KEY, Some("completed")),
        ];
        assert!(call_precedes_output(&mirror, 1));
        assert_eq!(
            pairing_of(&mirror),
            vec![
                ToolSearchPairing::UnansweredCall,
                ToolSearchPairing::UnansweredCall
            ]
        );
    }

    // ---- call_id_groups coverage -------------------------------------------

    /// The GROUP is keyed on `call_id` alone: state-blind and kind-blind, so the
    /// stream skeleton sits in the same group as the final pair it belongs to. That
    /// is deliberate — the caller decides what to do with the skeleton — and it is
    /// why a group is not a delete set without the quadrant check.
    #[test]
    fn a_group_contains_the_skeleton_alongside_the_final_pair() {
        let items = [
            item(cx3_skeleton_call()),
            item(cx3_call()),
            keyed_output(CX3_KEY, Some("completed")),
        ];
        assert_eq!(
            call_id_groups(&items),
            vec![vec![0, 1, 2]],
            "one key, one group, whatever the states are"
        );
        assert_eq!(
            pairing_of(&items),
            vec![
                ToolSearchPairing::Incomplete,
                ToolSearchPairing::Paired,
                ToolSearchPairing::Paired
            ]
        );
    }

    /// The other half of the state-blindness claim, pinned separately because the
    /// test above cannot see it: there, an unpairable item shares its key with a
    /// pairable one, so even a key collection that filtered on state would recover
    /// the index from the member scan. Here the UNPAIRABLE item is the key's only
    /// carrier, and a filter that dropped it would drop the whole group — the group
    /// PLAN:946 needs in order to report the shape instead of acting on half of it.
    /// The regression this exists to catch is a state filter on the key collection:
    /// it would drop the whole group along with the unpairable carrier, and the
    /// `call_id_groups` assertion below is what fails when it is added — the verdict
    /// assertion above it survives the same filter, because a filter narrows the
    /// group, not the per-item verdict.
    #[test]
    fn a_key_whose_only_carrier_is_unpairable_still_forms_a_group() {
        for (status, verdict) in [
            // The two unpairable families are different defects and must not be able
            // to diverge here: a not-yet-final skeleton, and a status this build
            // cannot name.
            ("in_progress", ToolSearchPairing::Incomplete),
            ("cancelled", ToolSearchPairing::OrphanOutput),
        ] {
            let lone = keyed_output(CX3_KEY, Some(status));
            assert_eq!(
                pairing_of(std::slice::from_ref(&lone)),
                vec![verdict],
                "the status decides the verdict ({status})…"
            );
            assert_eq!(
                call_id_groups(&[lone]),
                vec![vec![0]],
                "…and nothing at all decides membership: no verdict may make \
                 a keyed item's group disappear"
            );
        }
    }

    /// The shape that matters, run for real: a provider-minted pair whose key is
    /// NOT null, placed output-first. `ToolSearchPairing` cannot express it (both
    /// halves are `CounterpartPresent`, and `partner_indices` is symmetric, so there
    /// is no "second half of an inverted pair" to return) — the ordering predicates
    /// are what report it, from both sides of the law at once.
    #[test]
    fn an_inverted_pair_is_reported_from_both_sides_of_the_law() {
        let mut pair = sol_hosted_server_pair();
        for half in pair.iter_mut() {
            half["call_id"] = json!("call_non_null_server_pair");
        }
        let items = [item(pair[1].clone()), item(pair[0].clone())];
        assert_eq!(
            pairing_of(&items),
            vec![
                ToolSearchPairing::CounterpartPresent,
                ToolSearchPairing::CounterpartPresent
            ],
            "the verdict family stays non-destructive: PLAN:947 exempts this \
             quadrant from removal, and PLAN:946's CLIENT-SCOPED ENFORCEMENT pass \
             does not reach it — its law still binds the group, so a repair here is \
             a group decision and never a per-item delete"
        );
        assert_eq!(partner_indices(&items), vec![None, None]);
        assert!(
            !call_precedes_output(&items, 0),
            "the output precedes its call, so PLAN:946's \"must follow\" is unmet — an \
             ORDER verdict, not the missing-call class probe R5 captured"
        );
        assert!(
            !output_follows_call(&items, 1),
            "and this call has no answer after it — PLAN:946's own wording, \
             reported even where the plan prescribes no action"
        );
    }

    /// The domain boundary is an assertion, not a comment: handing the call's index
    /// to the output-side predicate is a caller bug and a debug build says so
    /// (release keeps the literal — meaningless — answer, which is why the names
    /// carry their subject).
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "takes the index of a tool_search_output")]
    fn handing_a_call_index_to_the_output_predicate_is_caught() {
        let items = [keyed_call(CX3_KEY, Some("completed"))];
        assert!(
            !call_precedes_output(&items, 0),
            "unreachable: the debug_assert fires first"
        );
    }

    /// The mirror guard, same debug-only contract: `output_follows_call` asked about an
    /// OUTPUT is the caller bug a pass hits when it re-uses one loop index for both
    /// predicates. Without this test the assert can be deleted and the suite stays green.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "output_follows_call takes the index of a tool_search_call")]
    fn handing_an_output_index_to_the_call_predicate_is_caught() {
        let items = [keyed_output(CX3_KEY, Some("completed"))];
        assert!(
            !output_follows_call(&items, 0),
            "unreachable: the debug_assert fires first"
        );
    }

    /// These two scenarios expose a limit of the pair relation that a caller must
    /// know about: `partner_indices` is
    /// SYMMETRIC, so an inverted pair has no "second half" it can point at — the
    /// later item simply gets `None`, indistinguishable from a keyless or
    /// state-blocked item. That is why PLAN:946's ordering law is reported by
    /// [`call_precedes_output`] / [`output_follows_call`] and not by the pair map.
    #[test]
    fn the_pair_relation_cannot_name_the_later_half_of_an_inverted_pair() {
        // Scenario 1 — a provider-minted pair with a non-null key, inverted.
        let mut pair = sol_hosted_server_pair();
        for half in pair.iter_mut() {
            half["call_id"] = json!("call_non_null_server_pair");
        }
        let inverted = [item(pair[1].clone()), item(pair[0].clone())];
        assert_eq!(partner_indices(&inverted), vec![None, None]);
        assert_eq!(
            call_id_groups(&inverted),
            vec![vec![0, 1]],
            "one group, no pair: only the group reader sees that these belong together"
        );

        // Scenario 2 — the same shape with a group WORTH keeping: the client case,
        // and the group reader still hands back all three items even though the 1:1
        // pass claims only the first pair. (The `pairing_of`/`output_follows_call`
        // reading of this exact slice is pinned by
        // `a_reused_key_call_with_an_earlier_output_is_plan_946_s_lost_half`; only the
        // group claim is new here.)
        let mixed = [
            keyed_call(CX3_KEY, Some("completed")),
            keyed_output(CX3_KEY, Some("completed")),
            keyed_call(CX3_KEY, Some("completed")),
        ];
        assert_eq!(call_id_groups(&mixed), vec![vec![0, 1, 2]]);
    }

    /// Groups come back in FIRST-SEEN order, not sorted by key: a repair pass walks
    /// them in conversation order and must not have to re-derive it.
    #[test]
    fn groups_come_back_in_first_seen_order_not_sorted_by_key() {
        let items = [
            keyed_output("call_z", Some("completed")),
            keyed_call("call_a", Some("completed")),
            keyed_output("call_a", Some("completed")),
        ];
        assert_eq!(call_id_groups(&items), vec![vec![0], vec![1, 2]]);
    }

    /// PLAN:947's removal is scoped to `execution != "server"` and this verdict does
    /// not read `execution` at all — a SERVER output with a real `call_id` and no
    /// matching call is reported `OrphanOutput` like any other, and the caller must
    /// apply the exemption. Pinned here because the quadrant is invisible in the
    /// verdict: the corpus has no shape more likely to be deleted by mistake (A-26).
    #[test]
    fn a_server_output_is_orphaned_by_key_and_exempted_by_the_caller() {
        let orphan = item(json!({
            "type": "tool_search_output",
            "id": "tso_server_orphan_probe",
            "call_id": "call_server_side_never_minted",
            "status": "completed",
            "execution": "server",
            "tools": []
        }));
        assert!(orphan.is_server_executed());
        assert!(
            !orphan.is_client_executed(),
            "the two quadrant accessors are exact-match, so they do not collapse here; \
             on this item PLAN:947's `execution != \"server\"` grants the exemption while \
             `!is_client_executed()` alone would call it deletable — the delete-able set \
             is `is_client_executed() && OrphanOutput`, not the negation of either accessor"
        );
        assert_eq!(
            pairing_of(std::slice::from_ref(&orphan)),
            vec![ToolSearchPairing::OrphanOutput],
            "the verdict is key-only"
        );
        assert_eq!(
            call_id_groups(&[orphan]),
            vec![vec![0]],
            "and it forms a group, so a group-walking pass must filter on the \
             quadrant before dropping anything (PLAN:947)"
        );
    }

    // ---- error surface and the un-called accessors --------------------------

    /// Both rejection shapes say what they mean, including the `<absent>` form a
    /// caller sees when the `type` tag is missing or is not a string.
    #[test]
    fn the_two_rejections_display_their_own_reasons() {
        assert_eq!(
            ToolSearchItem::from_wire(json!([1, 2]))
                .unwrap_err()
                .to_string(),
            "discovery item is not a JSON object"
        );
        for (raw, expected) in [
            (json!({"type": "function_call"}), "function_call"),
            (json!({"type": "message"}), "message"),
        ] {
            let err = ToolSearchItem::from_wire(raw).unwrap_err();
            assert_eq!(
                err,
                ToolSearchItemError::NotADiscoveryItem {
                    item_type: Some(expected.to_owned())
                }
            );
            assert_eq!(
                err.to_string(),
                format!("not a tool-discovery item (type: {expected})")
            );
        }
        for raw in [json!({}), json!({"type": 7}), json!({"type": null})] {
            let err = ToolSearchItem::from_wire(raw.clone()).unwrap_err();
            assert_eq!(
                err,
                ToolSearchItemError::NotADiscoveryItem { item_type: None }
            );
            assert_eq!(
                err.to_string(),
                "not a tool-discovery item (type: <absent>)",
                "input: {raw}"
            );
        }
    }

    /// The accessors no in-crate consumer calls today are still part of the API
    /// surface: they read the bytes, they do not reinterpret them.
    #[test]
    fn the_borrowed_views_expose_the_bytes_they_wrapped() {
        let output = item(cx1_namespaced_output());
        let group = &output.tools()[0];
        assert!(
            std::ptr::eq(group.raw(), &output.raw()["tools"][0]),
            "the view IS the wrapped node: the same allocation, not an equal copy. A \
             re-derived copy satisfies every field assert below and is still wrong; value \
             equality is IMPLIED by address equality here, so only the address is asserted."
        );
        assert_eq!(group.raw()["name"].as_str(), Some("mcp__ratchet_fixture"));
        assert_eq!(
            group.description(),
            Some("Deterministic fixture server for wire-fingerprint capture.")
        );
        let loaded = loaded_tool_set([&output]);
        let first = &loaded[0];
        assert_eq!(
            first.definition()["name"].as_str(),
            first.name(),
            "definition() is the same borrowed node name() reads"
        );
        assert!(
            std::ptr::eq(first.definition(), &output.raw()["tools"][0]["tools"][0]),
            "and it borrows the child in place, two levels down"
        );
        assert_eq!(
            first.namespace(),
            group.name(),
            "and the namespace rides along for the short-name form"
        );
    }

    /// The `limit()` reading for a magnitude above `u64::MAX` has no distinct arm to reach, and
    /// this test says so with an assertion rather than a comment. The crate builds `serde_json`
    /// without `arbitrary_precision`, so an integer the `u64`/`i64` range cannot hold is parsed
    /// as `f64` and is then rejected for being a float — the same path `8.0` takes. If the
    /// feature is ever enabled, or the parser starts preserving big integers, the `is_f64()`
    /// assertion below flips and this test fails: that is the signal that a genuine overflow arm
    /// now exists and needs its own reasoning, not that the test is stale.
    #[test]
    fn a_limit_too_large_for_u64_arrives_as_a_float_not_as_an_overflow() {
        let raw: Value = serde_json::from_str(
            r#"{"type":"tool_search_call","id":"tsc_big","call_id":"call_big",
                "execution":"client","status":"completed",
                "arguments":{"query":"crm","limit":18446744073709551616}}"#,
        )
        .expect("wire bytes parse");
        let big = &raw["arguments"]["limit"];
        assert!(
            big.is_f64(),
            "the oversized integer did not survive as an integer"
        );
        // `limit()` is `self.arguments()?.get("limit")...`, so a broken `arguments()`
        // answers `None` as well, and the `is_f64` assert above reads `raw` directly and
        // would not notice. This is the control that separates the two.
        assert_eq!(
            item(raw.clone()).query(),
            Some("crm"),
            "the same `arguments()` node still reads, so `limit()` is not \
             None for the wrong reason"
        );
        assert_eq!(
            item(raw).limit(),
            None,
            "and the accessor still yields no count"
        );
    }

    /// Degenerate payloads degrade to nothing rather than to a wrong answer: a
    /// non-integral or non-numeric `limit`, a `tools` that is not an array, and a
    /// definition with no name to invoke.
    #[test]
    fn a_degenerate_payload_contributes_nothing_rather_than_a_wrong_value() {
        // `8.0` is integral in VALUE and still reads as nothing: `as_u64` asks for a JSON
        // integer, not for a number with no fractional part.
        for limit in [
            json!(8.5),
            json!(8.0),
            json!("8"),
            json!(-1),
            json!(null),
            json!({}),
        ] {
            let raw = json!({
                "type": "tool_search_call",
                "id": "tsc_limit_probe",
                "call_id": "call_limit_probe",
                "execution": "client",
                "status": "completed",
                "arguments": { "query": "crm", "limit": limit }
            });
            let call = item(raw);
            assert_eq!(call.limit(), None, "limit {limit} is not a count");
            assert_eq!(
                call.query(),
                Some("crm"),
                "the rest of the view still works"
            );
        }
        let not_an_array = item(json!({
            "type": "tool_search_output",
            "id": "tso_tools_probe",
            "call_id": "call_tools_probe",
            "execution": "client",
            "status": "completed",
            "tools": { "name": "lookup_shipping_eta" }
        }));
        assert!(not_an_array.tools().is_empty(), "a map is not a tool set");
        assert!(loaded_tool_set([&not_an_array]).is_empty());
        let nameless = item(json!({
            "type": "tool_search_output",
            "id": "tso_nameless_probe",
            "call_id": "call_nameless_probe",
            "execution": "client",
            "status": "completed",
            "tools": [{ "type": "function", "parameters": {} }]
        }));
        assert_eq!(
            loaded_tool_set([&nameless]).len(),
            1,
            "the bytes are still loaded"
        );
        assert!(
            invocable_names([&nameless]).is_empty(),
            "but a definition with no name has no invocation form"
        );
    }

    /// A namespace is a GROUP whose children are callable: the group name is
    /// never a callable definition.
    #[test]
    fn a_namespace_group_is_not_a_callable_but_its_children_are() {
        let output = item(cx1_namespaced_output());
        let tools = output.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].kind, DiscoveredToolKind::Namespace);
        assert_eq!(tools[0].name(), Some("mcp__ratchet_fixture"));

        let callables = tools[0].callable_definitions();
        let names: Vec<&str> = callables
            .iter()
            .map(|definition| definition["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec![
                "crm_fixture_tool_00",
                "crm_fixture_tool_06",
                "crm_fixture_tool_09"
            ]
        );
        assert!(
            !names.contains(&"mcp__ratchet_fixture"),
            "the group name must not be invocable"
        );
    }

    /// Pins the KNOWN SEAM named on [`DiscoveredTool::callable_definitions`]: a
    /// namespace child whose `type` is not `function` contributes nothing, while
    /// the group's bytes stay intact for replay. No discovery RESULT has emitted one
    /// yet (children of the namespace groups inside a `tool_search_output` are
    /// `function` 11/11), though `custom` children DO exist in declarations
    /// (`CX2-codemode-5.6sol-LIVE/next-turn.json` `input[0].tools[0].tools[0]`), so
    /// this is a ratchet rather than an observation — the first real `custom` child
    /// inside a discovery output must change this test on purpose.
    #[test]
    fn a_non_function_namespace_child_is_not_callable() {
        let output = item(json!({
            "type": "tool_search_output",
            "id": "tso_seam_probe",
            "call_id": "call_seam_probe",
            "status": "completed",
            "execution": "client",
            "tools": [{
                "type": "namespace",
                "name": "mcp__seam",
                "tools": [
                    { "type": "function", "name": "known_child" },
                    { "type": "custom", "name": "unmodelled_child" },
                ]
            }]
        }));
        let tools = output.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].kind, DiscoveredToolKind::Namespace);
        let callables: Vec<&str> = tools[0]
            .callable_definitions()
            .iter()
            .map(|definition| definition["name"].as_str().unwrap())
            .collect();
        assert_eq!(callables, vec!["known_child"]);
        let loaded: Vec<&str> = loaded_tool_set([&output])
            .iter()
            .filter_map(LoadedDefinition::name)
            .collect();
        assert_eq!(
            loaded,
            vec!["known_child"],
            "the unmodelled child must not reach the loaded-tool set"
        );
        assert_eq!(
            output.raw()["tools"][0]["tools"].as_array().unwrap().len(),
            2,
            "and nothing was dropped on the way through"
        );
    }

    /// Flat (`function`) and grouped (`namespace`) results both land in the
    /// loaded set, and the group boundary survives.
    #[test]
    fn loaded_tool_set_flattens_children_but_keeps_the_namespace() {
        let flat = item(r6_flat_output());
        let grouped = item(cx1_namespaced_output());

        let loaded = loaded_tool_set([&flat, &grouped]);
        let names: Vec<Option<&str>> = loaded.iter().map(|l| l.name()).collect();
        assert_eq!(
            names,
            vec![
                Some("lookup_shipping_eta"),
                Some("crm_fixture_tool_00"),
                Some("crm_fixture_tool_06"),
                Some("crm_fixture_tool_09"),
            ]
        );
        assert_eq!(loaded[0].namespace, None, "flat result has no group");
        assert_eq!(
            loaded[1].namespace,
            Some("mcp__ratchet_fixture"),
            "child keeps the group it was discovered under"
        );
    }

    /// A-16: the two wires invoke the same child differently, so both forms are
    /// reported and neither is pre-joined here — the sanctioned join lives in
    /// `super::tool_name`.
    #[test]
    fn invocable_names_reports_both_forms_without_joining_them() {
        let grouped = item(cx1_namespaced_output());
        let names = invocable_names([&grouped]);
        assert_eq!(names.len(), 3);
        for (short, namespace) in &names {
            assert!(
                !short.contains(FLAT_TOOL_NAME_DELIMITER),
                "short name {short:?} was pre-joined into the flat form"
            );
            assert_eq!(*namespace, Some("mcp__ratchet_fixture"));
        }
        assert_eq!(names[0].0, "crm_fixture_tool_00");

        // The FLAT shape, on its own: A-16's two invocation forms are not just
        // the two halves of a group — a whole output can carry only the flat
        // form, and then the namespace half of the pair must be `None` rather
        // than an invented group. `R6-client-loop/next-turn.json` `input[2]` is
        // exactly that shape.
        let flat = item(r6_flat_output());
        assert_eq!(
            invocable_names([&flat]),
            vec![("lookup_shipping_eta", None)],
            "a flat definition is invocable by its own name, group-less"
        );
        // Both shapes in one history stay two entries, not one joined name.
        let both = invocable_names([&flat, &grouped]);
        assert_eq!(both.len(), 4);
        assert!(
            both.iter().all(|(name, _)| !name.contains("__")),
            "nothing joined a group name onto a child: {both:?}"
        );
    }

    /// The A-22 defect class: a stream delivers the item twice, and the FIRST
    /// copy is an empty skeleton. A consumer that keeps it loses the discovery.
    #[test]
    fn the_stream_skeleton_is_never_conversation_state() {
        let skeleton = item(cx3_skeleton_call());
        assert_eq!(skeleton.status(), ToolSearchStatus::InProgress);
        assert!(!skeleton.is_completed());
        assert_eq!(skeleton.query(), None, "the skeleton has no query");
        assert!(
            skeleton.arguments().is_some_and(|a| a.is_empty()),
            "the skeleton's arguments are an empty object"
        );
        assert_eq!(
            pairing_of(std::slice::from_ref(&skeleton)),
            vec![ToolSearchPairing::Incomplete]
        );

        // The completion guard on the loaded-tool set, measured on an OUTPUT: a
        // `tool_search_call` is refused by the KIND gate (see
        // `a_call_never_contributes_definitions_whatever_it_carries`), so testing
        // the guard with a call would prove nothing about `status`. This is the
        // streaming `done` frame that has not landed yet — definitions already
        // present, terminal state not.
        let mut streaming = cx1_namespaced_output();
        streaming["status"] = json!("in_progress");
        let streaming = item(streaming);
        assert_eq!(
            loaded_tool_set(std::slice::from_ref(&streaming)).len(),
            0,
            "an in_progress output defines nothing yet"
        );
        assert_eq!(
            loaded_tool_set(std::slice::from_ref(&item(cx1_namespaced_output()))).len(),
            3,
            "the same bytes DO load once the terminal state is claimed"
        );
    }

    /// A non-completed RESULT that already carries definitions must not enter
    /// the loaded set. A-14 says the loaded set IS the `tool_search_output.tools`
    /// payload in history (PLAN:1532-1533); A-22 adds which copy counts
    /// (PLAN:1749-1757 — the loader that fingerprinted the IN-PROGRESS item and
    /// scored an empty arguments object as truth). A partial copy would otherwise
    /// grant the model tools the provider never finished loading, and on the
    /// Messages side drive a declaration for a tool that was never loaded.
    #[test]
    fn an_unfinished_result_contributes_no_definitions() {
        for status in ["in_progress", "error"] {
            let mut partial = cx1_namespaced_output();
            partial["status"] = json!(status);
            let parsed = item(partial);
            assert!(
                !parsed.tools().is_empty(),
                "the fixture carries definitions, so this proves the guard works"
            );
            assert!(!parsed.is_completed());
            assert_eq!(
                loaded_tool_set([&parsed]).len(),
                0,
                "a {status} result is not the loaded-tool set"
            );
        }

        // The skeleton is not conversation state at all. (An `error` output is:
        // it answers its call — see
        // `an_error_answer_answers_its_call_and_defines_nothing`.)
        let mut skeleton = cx1_namespaced_output();
        skeleton["status"] = json!("in_progress");
        assert_eq!(
            pairing_of(std::slice::from_ref(&item(skeleton))),
            vec![ToolSearchPairing::Incomplete]
        );

        // Neither half may complete a pair on its own: the guard applies to the
        // candidate partner too, so an unfinished output never answers its call.
        // The call is NOT `UnansweredCall` though — PLAN:948 fires on "a client
        // call with no output", and an output carrying the key IS present, so a
        // pass that synthesised here would hand a search that already answered
        // (or is still streaming) a second answer. The fix belongs to the
        // `Incomplete` half.
        let mut pending = cx1_namespaced_output();
        pending["status"] = json!("in_progress");
        pending["call_id"] = json!("call_AOphypzlL1KKckJugyBS2PYn");
        let pending = item(pending);
        let call = item(cx3_call());
        assert_eq!(
            pairing_of(&[call.clone(), pending.clone()]),
            vec![
                ToolSearchPairing::CounterpartPresent,
                ToolSearchPairing::Incomplete
            ],
            "a pending output must not answer the call"
        );

        let mut done = cx1_namespaced_output();
        done["call_id"] = json!("call_AOphypzlL1KKckJugyBS2PYn");
        let done = item(done);
        assert_eq!(
            pairing_of(&[call, done]),
            vec![ToolSearchPairing::Paired, ToolSearchPairing::Paired],
            "and the completed copy does"
        );
    }

    /// The D-ERR channel (PLAN:22): a failed search still answers its call,
    /// with `call_id` set to the call's real id, `tools: []` and `status:
    /// "error"`. Reading that as `UnansweredCall` is how a repair pass comes to
    /// synthesise a SECOND output for a search that already returned — and an
    /// error copy that somehow carries definitions still must not grant them.
    #[test]
    fn an_error_answer_answers_its_call_and_defines_nothing() {
        let mut failed = cx1_namespaced_output();
        failed["status"] = json!("error");
        failed["error"] = json!("unparseable search arguments");
        failed["call_id"] = json!("call_AOphzlL1KKckJugyBS2PYn");
        let output = item(failed);
        assert_eq!(output.status(), ToolSearchStatus::Error);
        assert!(!output.is_completed());
        assert_eq!(
            loaded_tool_set([&output]).len(),
            0,
            "an error copy is never the loaded-tool set, definitions or not"
        );

        let mut call = cx3_call();
        call["call_id"] = json!("call_AOphzlL1KKckJugyBS2PYn");
        assert_eq!(
            pairing_of(&[item(call), output]),
            vec![ToolSearchPairing::Paired, ToolSearchPairing::Paired],
            "the error answer closes the pair"
        );
    }

    /// The join is ORDERED because PLAN:946 says so; no capture shows the boundary
    /// enforcing placement, and the one 400 in this family (probe R5) came from a request
    /// holding no call at all — see [`partner_indices`]. An output whose call does not
    /// precede it must still never be reported as `Paired`: a pass that saw `Paired` here
    /// would read a shape the plan forbids as the well-ordered one and ship it verbatim.
    /// It must NOT be reported as a defect either:
    /// PLAN:947's call set plainly contains the key, so the destructive verdicts are
    /// wrong for this shape. What PLAN:946 does bind is its atomicity law — these are
    /// one `call_id_groups` group, kept or dropped together. Its restore-or-drop remedy
    /// is conditioned on the history having LOST a half, and both halves are present
    /// here, so the choice of what to do with the group belongs to the caller, not to
    /// PLAN:946 (see [`ToolSearchPairing::CounterpartPresent`]).
    #[test]
    fn an_output_before_its_call_is_not_a_pair_but_still_one_group() {
        let mut output = cx1_namespaced_output();
        output["call_id"] = json!("call_AOphypzlL1KKckJugyBS2PYn");
        let items = [item(output), item(cx3_call())];
        assert_eq!(
            partner_indices(&items),
            vec![None, None],
            "the call is AFTER the output, so no pair exists"
        );
        assert_eq!(
            pairing_of(&items),
            vec![
                ToolSearchPairing::CounterpartPresent,
                ToolSearchPairing::CounterpartPresent
            ],
            "membership holds, so neither half may be deleted or double-answered"
        );
        assert_eq!(
            call_id_groups(&items),
            vec![vec![0, 1]],
            "and the group is the unit PLAN:946 acts on: one group, both halves"
        );
    }

    /// The provider-executed quadrant as captured on the live wire: BOTH halves
    /// minted by the provider with `"call_id": null`. There is no join key, so
    /// there is no pair to report — and reporting the orphan class here would
    /// tell a consumer to strip a pair the provider itself minted, which PLAN:947
    /// exempts from orphan removal and A-26 forbids outright.
    #[test]
    fn a_provider_minted_pair_without_a_join_key_is_unkeyed_not_orphaned() {
        let raws = sol_hosted_server_pair();
        let items = [item(raws[0].clone()), item(raws[1].clone())];
        for discovery in &items {
            assert!(discovery.is_server_executed());
            assert!(!discovery.is_client_executed());
            assert_eq!(discovery.call_id(), None, "the wire sent call_id: null");
        }
        assert_eq!(
            pairing_of(&items),
            vec![ToolSearchPairing::Unkeyed, ToolSearchPairing::Unkeyed]
        );
        // No join key means no PLAN:946 group either: two `null`s are not a key,
        // and a repair pass that grouped on them would "restore" one search's
        // answer onto an unrelated provider-minted pair.
        assert_eq!(
            call_id_groups(&items),
            Vec::<Vec<usize>>::new(),
            "keyless items belong to no group"
        );
        // Still real discovery state: the definitions are loadable and replayable.
        assert_eq!(loaded_tool_set(&items).len(), 1);
        assert_eq!(
            items[1].id().unwrap(),
            "tso_0ce980d5c6afd41f016ab61424031481908982e2c788dcc429"
        );
        // Key order is the capture's, and it is neither alphabetical nor the
        // codex donor's — the provider's own field order survives the store
        // round-trip (asserting this on a fixture I wrote proves the round-trip,
        // not the fixture: the vector came off
        // `wire_resp_20260925T062640Z_R1_SOL_HOSTED.json`).
        for (index, discovery) in items.iter().enumerate() {
            let reread: ToolSearchItem =
                serde_json::from_str(&serde_json::to_string(discovery).unwrap())
                    .expect("an item must re-read from its own wire bytes");
            assert_eq!(
                key_vector(reread.raw()),
                SOL_HOSTED_KEY_ORDERS[index],
                "half {index} keeps the provider's key order through the store"
            );
        }
    }

    /// Both polarity inversions of the two `execution` predicates, fed an item whose
    /// `execution` field is ABSENT. PLAN:1247 calls the field REQUIRED on both
    /// halves, and PLAN:1319 records that this campaign's first 21 probes omitted
    /// it (on the DECLARATION) and all landed server-side — absence has been fed to
    /// this wire before, even though 31/31 captured discovery ITEMS carry the field.
    /// Neither predicate may claim a quadrant for it: the two `== Some(..)`
    /// comparisons must both be equality-with-value, not "not the other one".
    #[test]
    fn an_absent_execution_claims_neither_quadrant() {
        for mut raw in [cx3_call(), cx1_namespaced_output()] {
            raw.as_object_mut().unwrap().remove("execution");
            let item = item(raw);
            assert_eq!(item.execution(), None);
            assert!(
                !item.is_client_executed(),
                "absent is not client: the field never said \"client\""
            );
            assert!(
                !item.is_server_executed(),
                "absent is not server either: PLAN:947's exemption must not be \
                 granted to an item that never claimed the server quadrant, and \
                 the emitter's default is a separate question
                 (PLAN:1318 = quadrant selector, PLAN:1319 = the 21 probes)"
            );
        }
        // And the two present values stay distinct.
        let (server_call, server_output) = (
            item(sol_hosted_server_pair()[0].clone()),
            item(sol_hosted_server_pair()[1].clone()),
        );
        assert!(server_call.is_server_executed() && !server_call.is_client_executed());
        assert!(server_output.is_server_executed() && !server_output.is_client_executed());
        let client = item(cx3_call());
        assert!(client.is_client_executed() && !client.is_server_executed());
    }

    /// The other half of the narrowing on [`ToolSearchItem::is_client_executed`]: an
    /// `execution` that is PRESENT and says neither `client` nor `server`. No capture has
    /// ever emitted one, so this is a robustness pin and is stated as one. It stays apart
    /// from the absent case because the bytes differ: here `execution()` is `Some(v)` for a
    /// `v` that matches neither spelling, there it is `None`. That difference is load
    /// bearing — a pass keyed on "the field is absent" leaves this item alone and PLAN:947's
    /// literal (`execution != "server"`), keyed on "not server", deletes it — and a provider
    /// that ever renamed the field would turn a removal pass into a data-loss pass if a
    /// third spelling read as either quadrant.
    ///
    /// The values are aimed at specific misreads rather than at flavour, and each
    /// claim was checked by mutating the accessor: a server-means-"not client"
    /// reading, a case-insensitive comparison (`SERVER`, `CLIENT`), a `starts_with`
    /// reading (`server_executed`, `client-side`) and a `trim` (`" server"`,
    /// `" client"`); the present-but-empty `""` pins that an empty string is still
    /// `Some("")` where an absent field is `None`.
    #[test]
    fn an_unrecognised_execution_value_claims_neither_quadrant() {
        for value in [
            "worker",
            "SERVER",
            "CLIENT",
            "",
            "server_executed",
            "client-side",
            " server",
            " client",
        ] {
            for mut raw in [cx3_call(), cx1_namespaced_output()] {
                raw["execution"] = json!(value);
                let item = item(raw);
                assert_eq!(
                    item.execution(),
                    Some(value),
                    "{value:?} is a present string, not an absent field"
                );
                assert!(
                    !item.is_client_executed(),
                    "{value:?} must not read as the client quadrant, which is the \
                     quadrant this module deletes from"
                );
                assert!(
                    !item.is_server_executed(),
                    "{value:?} must not read as the server quadrant either, so \
                     PLAN:947's exemption is not granted to it"
                );
            }
        }
    }

    /// A payload is untrusted bytes. A `tool_search_call` that grew a `tools`
    /// array must not put definitions into the loaded set — the Messages arm
    /// (A-23) would then have to DECLARE tools no search ever loaded.
    #[test]
    fn a_call_never_contributes_definitions_whatever_it_carries() {
        let mut call = cx3_call();
        call["tools"] = r6_flat_output()["tools"].clone();
        let call = item(call);
        assert!(call.tools().is_empty(), "the kind gates the accessor");
        assert_eq!(loaded_tool_set([&call]).len(), 0, "and it gates the set");
    }

    /// `CALL_REPLAYABLE_KEYS` has to discriminate the field that actually 400s,
    /// or it is decoration. The live hosted capture's echoed call carries
    /// `created_by`; replaying it verbatim is the documented `400 Unknown
    /// parameter: 'input[1].created_by'` (PLAN:1325-1328). Stripping it is T15's
    /// job (PLAN:1421) — this only pins that the key set names the hazard.
    ///
    /// Both claims are asserted directly, on the CALL half (the only half this set
    /// governs, and the only half this test reads): the old form walked the fixture's keys
    /// that were *already* outside the set and checked each was `created_by`, so
    /// a future edit that added `created_by` to the set made the loop body
    /// unreachable and the test passed vacuously.
    #[test]
    fn replayable_keys_excludes_the_field_the_boundary_rejects() {
        assert!(
            !CALL_REPLAYABLE_KEYS.contains(&"created_by"),
            "the whole point of the set: {CALL_REPLAYABLE_KEYS:?} must not admit \
             the key the boundary rejects"
        );
        // Exact set, so a silent addition has to be a deliberate edit.
        let mut sorted = CALL_REPLAYABLE_KEYS.to_vec();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            ["arguments", "call_id", "execution", "id", "status", "type"]
        );

        let call = item(sol_hosted_server_pair()[0].clone());
        let keys: Vec<&str> = call
            .raw()
            .as_object()
            .expect("item is an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert!(
            keys.contains(&"created_by"),
            "the fixture is the hazard shape, keys: {keys:?}"
        );
        assert_eq!(
            keys.iter()
                .filter(|k| !CALL_REPLAYABLE_KEYS.contains(k))
                .count(),
            1,
            "`created_by` is the only key on the echoed call that a replay has to \
             drop, keys: {keys:?}"
        );
    }

    /// The set is the CALL half. An output — the item that carries the loaded-tool
    /// set in `tools` — is not described by it, and a reader who applied it to
    /// both halves would strip A-14's payload.
    #[test]
    fn the_call_key_set_does_not_describe_an_output() {
        let output = item(cx1_namespaced_output());
        let keys: Vec<&str> = output
            .raw()
            .as_object()
            .expect("item is an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert!(
            keys.contains(&"tools"),
            "the fixture carries the loaded-tool set, keys: {keys:?}"
        );
        assert!(
            !CALL_REPLAYABLE_KEYS.contains(&"tools"),
            "so this constant cannot be an output allow-list"
        );
    }

    /// `from_wire` is the only constructor: the `{kind, raw}` envelope that
    /// derived `Deserialize` used to accept — and which could make `kind`
    /// contradict the bytes — is refused, while the wire form round-trips
    /// byte-exactly.
    #[test]
    fn only_the_wire_form_deserializes_and_preserves_the_bytes() {
        let envelope = json!({"kind": "call", "raw": cx3_call()});
        assert!(
            serde_json::from_value::<ToolSearchItem>(envelope).is_err(),
            "the store envelope is no longer a way past from_wire"
        );

        let original = cx1_namespaced_output();
        let text = serde_json::to_string(&item(original.clone())).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            original,
            "serialising an item IS serialising its raw bytes"
        );
        let reread: ToolSearchItem = serde_json::from_str(&text).unwrap();
        assert_eq!(reread.kind(), ToolSearchKind::Output);
        assert_eq!(reread.raw(), &original);
        // `Value` equality is key-order-INSENSITIVE (see the `preserve_order`
        // canary in `conversation.rs`), so the asserts above cannot see a
        // re-ordering. Pin the ORDER against `CX1_OUTPUT_KEY_ORDER`:
        // type,id,call_id,status,execution,tools,internal_chat_message_metadata_passthrough
        // — deliberately not alphabetical, which is what makes this a real test:
        // without `preserve_order` the map re-emits in sorted order and both
        // asserts below fire. Note what this test does NOT do: it reads no capture
        // (the corpus is outside this repo and tests stay hermetic), it compares
        // two in-file artifacts — the fixture and the transcription vector — so it
        // pins the STORE's order-preservation, and the transcription's fidelity is
        // a report-level fact, not a test-level one. A re-ordered prefix is a
        // cache-break ($), not cosmetics.
        assert_eq!(
            key_vector(&original),
            CX1_OUTPUT_KEY_ORDER,
            "the fixture itself is capture-ordered"
        );
        assert_eq!(
            key_vector(reread.raw()),
            CX1_OUTPUT_KEY_ORDER,
            "and the store round-trip preserves that order"
        );
    }

    /// The key order recorded from `CX1…/next-turn.json` `input[4]` and transcribed
    /// here by hand (the capture is campaign state outside this repo; nothing at test
    /// time reads it). Fidelity of the transcription is checked out-of-band against
    /// the capture's own key order, not by anything this test can reach.
    const CX1_OUTPUT_KEY_ORDER: [&str; 7] = [
        "type",
        "id",
        "call_id",
        "status",
        "execution",
        "tools",
        "internal_chat_message_metadata_passthrough",
    ];

    /// `R1_SOL_HOSTED` `output[1]`/`output[2]` key order, read off the capture —
    /// this one starts with `id` and ends with `created_by`, so it is neither
    /// alphabetical nor the same shape as the codex donor's.
    const SOL_HOSTED_KEY_ORDERS: [&[&str]; 2] = [
        &[
            "id",
            "arguments",
            "call_id",
            "execution",
            "status",
            "type",
            "created_by",
        ],
        &[
            "id",
            "call_id",
            "execution",
            "status",
            "tools",
            "type",
            "created_by",
        ],
    ];

    /// Insertion-order view of an object's keys. `preserve_order` makes `Map` an
    /// index-ordered map, so this reflects the emitted byte order — the same reading
    /// `conversation::responses_tests::json_keys` takes in this crate (that module
    /// is the apex-waj.3 wire-responses lane, not another repo).
    fn key_vector(value: &Value) -> Vec<&str> {
        value
            .as_object()
            .expect("item is an object")
            .keys()
            .map(String::as_str)
            .collect()
    }

    /// A skeleton and a completed copy of the SAME call must not compete for
    /// the output: the pair is a property of the completed copies only.
    #[test]
    fn a_skeleton_never_steals_the_real_partners_pair() {
        let items = [
            item(cx3_skeleton_call()),
            item(cx3_call()),
            item({
                let mut output = cx1_namespaced_output();
                output["call_id"] = json!("call_AOphypzlL1KKckJugyBS2PYn");
                output
            }),
        ];
        assert_eq!(
            pairing_of(&items),
            vec![
                ToolSearchPairing::Incomplete,
                ToolSearchPairing::Paired,
                ToolSearchPairing::Paired,
            ]
        );
        let partners = partner_indices(&items);
        assert_eq!(partners, vec![None, Some(2), Some(1)]);
    }

    /// Joining is ACROSS kinds only. The donor names reused ids as a real shape
    /// (`call_id` is optional partly to accommodate "codex-reused ids",
    /// PLAN:1248) and a replayed window can hold the same completed call twice,
    /// so two calls sharing a key must not be read as a pair.
    #[test]
    fn same_kind_items_never_pair_even_when_they_share_a_call_id() {
        let first = item(cx3_call());
        let duplicate = item(cx3_call());
        assert_eq!(
            first.call_id(),
            Some("call_AOphypzlL1KKckJugyBS2PYn"),
            "the fixture is the reused-id shape: two items, one key"
        );
        // `duplicate` is the same fixture through the same pure accessor, so re-reading
        // its call_id could not disagree with the assert above. The pairing below is what
        // shows two calls sharing one key are not read as a pair.
        let mut output = cx1_namespaced_output();
        output["call_id"] = json!("call_AOphypzlL1KKckJugyBS2PYn");
        let items = [first, duplicate, item(output)];
        assert_eq!(
            pairing_of(&items),
            vec![
                ToolSearchPairing::Paired,
                ToolSearchPairing::CounterpartPresent,
                ToolSearchPairing::Paired
            ],
            "the first call takes the output; the duplicate is un-paired but its \
             key IS in the output set, so PLAN:948 must not add a second answer"
        );
        assert_eq!(partner_indices(&items), vec![Some(2), None, Some(0)]);
    }

    /// The other arm of exclusivity, and until now the untested one.
    /// `same_kind_items_never_pair_even_when_they_share_a_call_id` pins that an
    /// output is never claimed TWICE; nothing pinned that a later call, finding its
    /// nearest same-key candidate already claimed, STEPS OVER it and keeps scanning.
    /// Splitting the claimed-row clause out into its own `break` (give up rather
    /// than step over) left this module green until this test existed; with it,
    /// nothing else in the module goes red on that edit
    /// (`ratchet-capture/IR-DISCOVERY-report.md`, rounds 25/26/28 and 32). The
    /// run prints the index assert alone — a test aborts at its first failure — and
    /// the verdict vector moves with it (hand-derived on the mutant: `[Paired,
    /// CounterpartPresent, Paired, CounterpartPresent]`). The index vector is the
    /// load-bearing one either way: a scan that still claims each output once but
    /// takes the FARTHEST match leaves every call answered and every half `Paired`
    /// here, so only the index vector notices. That
    /// wider edit — bailing out on the FIRST rejected row of any kind — the reused-id
    /// test above already catches. Hand-derived, on `[call k, call k, output k]` that
    /// scan gives `[None, Some(2), Some(1)]` rather than the `[Some(2), None, Some(0)]`
    /// its own index assert pins; no run prints that vector, because the test fails
    /// earlier on its verdict assert. All four
    /// rejection clauses now have owners: kind (that test, among others — 86/2 as a
    /// `break` in BOTH profiles, and 84/4 deleted in release where one of those four
    /// kills does not compile into a debug build, making that run 85/3 there), claimed
    /// (this one), foreign key, and any row
    /// `is_pairable` refuses, i.e. any output `status` but `completed`/`error`:
    /// `in_progress`, absent, non-string, or outside the vocabulary
    /// (`a_wrong_key_or_non_terminal_output_does_not_end_the_scan`).
    #[test]
    fn an_already_claimed_output_is_stepped_over_not_stopped_at() {
        let items = [
            keyed_call("k_step", Some("completed")),
            keyed_call("k_step", Some("completed")),
            keyed_output("k_step", Some("completed")),
            keyed_output("k_step", Some("completed")),
        ];
        assert_eq!(
            partner_indices(&items),
            vec![Some(2), Some(3), Some(0), Some(1)],
            "call 0 takes the first output; call 1 steps over the claimed row \
             and takes the second; stopping there would give \
             [Some(2), None, Some(0), None]"
        );
        assert_eq!(
            pairing_of(&items),
            vec![
                ToolSearchPairing::Paired,
                ToolSearchPairing::Paired,
                ToolSearchPairing::Paired,
                ToolSearchPairing::Paired,
            ],
            "the indices above already fix who pairs with whom; this pins the \
             variant both halves report — `Paired`, not `CounterpartPresent`, the \
             verdict whose own doc warns that routing it with the orphan row deletes \
             real definitions"
        );
    }

    /// The last two rejection clauses of the inner scan. Neither could be told apart
    /// through `partner_indices` before this test: the pairability clause already runs
    /// inside `an_unmodelled_status_is_neither_a_skeleton_nor_a_certified_pair`, but on
    /// a two-row slice, where `continue` and `break` hand back the same map — the two
    /// per-clause mutants scored 87/0 across the whole module until this test existed
    /// (`ratchet-capture/IR-DISCOVERY-report.md`, round 27). Here the real match sits
    /// two rows out, so a scan that stopped at the first rejected row would leave the
    /// call unpaired while its answer sat in the same slice. The `pairing_of` assert
    /// beside each index assert goes red on the same mutant, because the two PAIRED
    /// rows fall to `CounterpartPresent` once the join breaks: `interleaved` becomes
    /// `[CounterpartPresent, OrphanOutput, CounterpartPresent]` under the key mutant,
    /// `skeleton_first` becomes `[CounterpartPresent, Incomplete, CounterpartPresent]`
    /// under the pairability one. The stepped-over middle row is the entry that does
    /// NOT move. A mutant run prints only the index failure — a test aborts at its
    /// first assert.
    #[test]
    fn a_wrong_key_or_non_terminal_output_does_not_end_the_scan() {
        // Interleaved searches: another search's answer sits between this call and
        // its own. PLAN:946's encode-time pass requires a client-executed call's answer
        // to follow somewhere in the request and says nothing about what sits in
        // between, and that freedom is what this scan exercises — the scan is
        // execution-blind, so it is deliberately wider than the plan's pass.
        // `output_follows_call_does_not_answer_for_another_search` builds the same
        // three rows for the order predicate; these are the index-map reading of them,
        // so an edit to one shape has to move with the other.
        let interleaved = [
            keyed_call(CX3_KEY, Some("completed")),
            keyed_output("call_someone_elses", Some("completed")),
            keyed_output(CX3_KEY, Some("completed")),
        ];
        assert_eq!(
            partner_indices(&interleaved),
            vec![Some(2), None, Some(0)],
            "the foreign row is stepped over, not stopped at"
        );
        assert_eq!(
            pairing_of(&interleaved),
            vec![
                ToolSearchPairing::Paired,
                ToolSearchPairing::OrphanOutput,
                ToolSearchPairing::Paired,
            ],
            "and the foreign row keeps `OrphanOutput` because no CALL carries its key"
        );

        // Same key, but the nearer copy carries a status `is_pairable` refuses, so
        // the scan has to reach past it. An `in_progress` OUTPUT is unattested: the
        // per-item-type status census recorded on `ToolSearchStatus` sees `in_progress`
        // on CALLS (the CX3
        // stream skeleton) and never on an output, so this arm is defensive by design
        // — the output-side mirror of A-22's call-side defect. The row left behind
        // keeps `Incomplete` precisely because `in_progress` IS a status this build
        // names; one it cannot name (say `"cancelled"`) takes a membership verdict
        // instead, and WHICH one depends on the rest of the slice: `CounterpartPresent`
        // here, where a same-key call sits (pinned by
        // `an_unmodelled_status_is_neither_a_skeleton_nor_a_certified_pair`), and
        // `OrphanOutput` when no call carries the key (pinned by
        // `orphaned_outputs_split_between_this_verdict_and_incomplete_by_status`).
        let skeleton_first = [
            keyed_call(CX3_KEY, Some("completed")),
            keyed_output(CX3_KEY, Some("in_progress")),
            keyed_output(CX3_KEY, Some("completed")),
        ];
        assert_eq!(
            partner_indices(&skeleton_first),
            vec![Some(2), None, Some(0)],
            "the non-terminal copy is stepped over"
        );
        assert_eq!(
            pairing_of(&skeleton_first),
            vec![
                ToolSearchPairing::Paired,
                ToolSearchPairing::Incomplete,
                ToolSearchPairing::Paired,
            ],
            "and the copy left behind reports `Incomplete` — a named non-terminal, \
             not an absent answer"
        );
    }

    /// The mirror of the reused-id case: ONE call answered by two outputs. The
    /// 1:1 pass pairs the first, and the second is `CounterpartPresent` — never
    /// `OrphanOutput`, because PLAN:947 removes an output whose `call_id` is not
    /// in the CALL SET and this one plainly is. Routing it with the destructive
    /// verdict is how a projector deletes a real loaded-tool set that the model
    /// has already been shown (the A-14/A-26 failure).
    #[test]
    fn a_second_output_for_one_call_is_not_an_orphan() {
        let mut call = cx3_call();
        call["call_id"] = json!("call_qPYlpfhYrhWFuCTTLktFETYd");
        let mut answer_two = r6_flat_output();
        answer_two["tools"][0]["name"] = json!("lookup_return_window");
        let items = [item(call), item(r6_flat_output()), item(answer_two)];
        assert_eq!(
            pairing_of(&items),
            vec![
                ToolSearchPairing::Paired,
                ToolSearchPairing::Paired,
                ToolSearchPairing::CounterpartPresent,
            ]
        );
        assert_eq!(partner_indices(&items), vec![Some(1), Some(0), None]);
        assert_eq!(
            loaded_tool_set(items.iter()).len(),
            2,
            "the over-answered output still contributes its definitions"
        );
    }

    /// The trap inside the over-answered case, pinned because the two halves of
    /// this system read it differently: when an `error` answer and a `completed`
    /// answer share one `call_id`, the 1:1 pair lands on the FIRST (the empty
    /// `error` copy — it is pairable, see `ToolSearchItem::is_pairable`), while
    /// the definitions live in the second. So `partner_indices` is NOT a way to
    /// find "the output that loaded the tools" — `loaded_tool_set` scans every
    /// completed copy independently and gets all of them, and a caller that
    /// walked only the partner would silently drop the tool set.
    #[test]
    fn a_pair_and_a_tool_set_are_different_questions() {
        let key = "call_error_then_ok";
        let call = item(json!({
            "type": "tool_search_call", "call_id": key, "execution": "client",
            "arguments": { "query": "q" }
        }));
        let failed = item(json!({
            "type": "tool_search_output", "call_id": key, "status": "error",
            "execution": "client", "tools": []
        }));
        let done = item(json!({
            "type": "tool_search_output", "call_id": key, "status": "completed",
            "execution": "client",
            "tools": [{ "type": "function", "name": "late_answer" }]
        }));
        let items = [call.clone(), failed.clone(), done.clone()];
        let partners = partner_indices(&items);
        assert_eq!(partners[0], Some(1), "the pair is the FIRST answer");
        assert_eq!(
            pairing_of(&items),
            vec![
                ToolSearchPairing::Paired,
                ToolSearchPairing::Paired,
                ToolSearchPairing::CounterpartPresent,
            ]
        );
        assert_eq!(
            invocable_names(&items),
            vec![("late_answer", None)],
            "and the loaded set comes from the UNPAIRED copy — follow the pair \
             alone and the tool set is lost"
        );
    }

    /// A call leaves pairing for exactly ONE reason: it is the `in_progress`
    /// stream skeleton. Its `status` is OPTIONAL (PLAN:1248), so an unmodelled
    /// value must not strand a real call — including `"failed"`, which is not
    /// part of this wire's vocabulary at all (the terminal pair is
    /// `completed`/`error`, PLAN:222) and therefore lands in `Unknown`.
    #[test]
    fn a_call_leaves_pairing_only_for_the_skeleton() {
        assert_eq!(
            pairing_of(std::slice::from_ref(&item(cx3_skeleton_call()))),
            vec![ToolSearchPairing::Incomplete]
        );
        for status in ["queued", "failed"] {
            let mut call = cx3_call();
            call["status"] = json!(status);
            assert_eq!(
                pairing_of(std::slice::from_ref(&item(call))),
                vec![ToolSearchPairing::UnansweredCall],
                "an unmodelled {status:?} status does not strand a real call"
            );
        }
    }

    /// Wire invariant 5 is pair-ATOMIC: a half is a defect, not a fallback.
    #[test]
    fn half_pairs_are_reported_as_orphan_and_unanswered() {
        let call = item(cx3_call());
        assert_eq!(
            pairing_of(std::slice::from_ref(&call)),
            vec![ToolSearchPairing::UnansweredCall]
        );

        let output = item(cx1_namespaced_output());
        assert_eq!(
            pairing_of(std::slice::from_ref(&output)),
            vec![ToolSearchPairing::OrphanOutput]
        );

        // A call and an output for DIFFERENT searches are two halves, not a pair.
        assert_eq!(
            pairing_of(&[call.clone(), output.clone()]),
            vec![
                ToolSearchPairing::UnansweredCall,
                ToolSearchPairing::OrphanOutput
            ]
        );
    }

    /// The donor contract binding on T3 (PLAN:1246-1251) makes `call_id` an
    /// `Option<String>` on both halves — "accommodating server-null AND
    /// codex-reused ids" — and says typing it stricter REJECTS what first-party
    /// emits. So such an item loads, and is reported as `Unkeyed`: it cannot
    /// pair, and it is NOT the orphan defect class (see
    /// `a_provider_minted_pair_without_a_join_key_is_unkeyed_not_orphaned`).
    #[test]
    fn an_item_without_a_call_id_loads_but_can_never_pair() {
        let mut no_key = cx3_call();
        no_key.as_object_mut().unwrap().remove("call_id");
        let call = item(no_key.clone());
        assert_eq!(call.call_id(), None);

        let mut no_key_output = r6_flat_output();
        no_key_output.as_object_mut().unwrap().remove("call_id");
        let output = item(no_key_output.clone());

        // Two keyless items of opposite kind must NOT be read as a pair: two
        // absences are not an agreement.
        assert_eq!(
            pairing_of(&[call.clone(), output.clone()]),
            vec![ToolSearchPairing::Unkeyed, ToolSearchPairing::Unkeyed],
            "a missing key must not match a missing key"
        );
        assert_eq!(partner_indices(&[call, output]), vec![None, None]);
    }

    /// `""` and an absent key are different JSON values and must behave the
    /// same way here: an empty string is not an identity, so two items that
    /// both carry `"call_id": ""` must not be read as a pair.
    #[test]
    fn an_empty_call_id_is_normalised_to_absent_and_pairs_with_nothing() {
        let mut call = cx3_call();
        call["call_id"] = json!("");
        let mut output = cx1_namespaced_output();
        output["call_id"] = json!("");
        let items = [item(call), item(output)];
        assert_eq!(
            items[0].call_id(),
            None,
            "the empty string is normalised to absent"
        );
        assert_eq!(
            pairing_of(&items),
            vec![ToolSearchPairing::Unkeyed, ToolSearchPairing::Unkeyed]
        );
        assert_eq!(partner_indices(&items), vec![None, None]);
        assert!(
            !call_precedes_output(&items, 1) && !output_follows_call(&items, 0),
            "and the ordering law is not computed from an absent key either: two \
             empty strings are two absences, not an agreement"
        );

        // A NON-STRING key is the same absence for the join: two items both carrying
        // `"call_id": 999` share a JSON value but no key. Unevidenced shape (every
        // captured `call_id` is a string or null), pinned for the same reason `id()`'s
        // non-string case is — the join must never invent an identity out of a byte
        // shape this build cannot name.
        let mut num_call = cx3_call();
        num_call["call_id"] = json!(999);
        let mut num_output = cx1_namespaced_output();
        num_output["call_id"] = json!(999);
        let num_items = [item(num_call), item(num_output)];
        assert_eq!(num_items[0].call_id(), None, "a number is not a key");
        assert_eq!(
            pairing_of(&num_items),
            vec![ToolSearchPairing::Unkeyed, ToolSearchPairing::Unkeyed],
            "pairing does not read one into it"
        );
        assert!(
            call_id_groups(&num_items).is_empty(),
            "and no PLAN:946 group is formed under it either"
        );
    }

    /// `status` is REQUIRED on the output but OPTIONAL on the call
    /// (PLAN:1247-1248). Absence is first-party-normal on the call, so it must not
    /// strand the item as unpairable — while the loaded-tool set still demands
    /// a completed output, where the field is required and no ambiguity exists.
    #[test]
    fn a_call_may_omit_status_but_an_output_may_not() {
        let mut statusless_call = cx3_call();
        statusless_call.as_object_mut().unwrap().remove("status");
        let call = item(statusless_call);
        assert!(
            !call.is_completed(),
            "no status is not a claim of completion"
        );
        assert!(call.is_pairable(), "and no status does not strand it");

        let mut output = cx1_namespaced_output();
        output["call_id"] = json!("call_AOphypzlL1KKckJugyBS2PYn");
        let output = item(output);
        assert_eq!(
            pairing_of(&[call, output]),
            vec![ToolSearchPairing::Paired, ToolSearchPairing::Paired],
            "a status-less call still pairs with its completed output"
        );

        let mut statusless_output = r6_flat_output();
        statusless_output.as_object_mut().unwrap().remove("status");
        let output = item(statusless_output);
        assert_eq!(
            loaded_tool_set([&output]).len(),
            0,
            "an output that never claimed completion defines nothing"
        );

        // Pairing agrees, and this is the half the asymmetry actually protects: an
        // output that never claimed a terminal state does not answer its call —
        // `status` is REQUIRED on the output (PLAN:1247), so absence cannot stand
        // in for an answer. The call is `CounterpartPresent` rather than
        // `UnansweredCall`: an output carrying the key is in the slice, so PLAN:948
        // adds no second answer (PLAN:947's removal is a rule about outputs, so it
        // never reached this item anyway). Neither
        // verdict says `Paired`, which is the claim a repair pass must not make
        // here.
        let mut unclaimed_answer = r6_flat_output();
        unclaimed_answer.as_object_mut().unwrap().remove("status");
        unclaimed_answer["call_id"] = json!("call_AOphypzlL1KKckJugyBS2PYn");
        let unclaimed_answer = item(unclaimed_answer);
        assert_eq!(
            unclaimed_answer.call_id(),
            Some("call_AOphypzlL1KKckJugyBS2PYn")
        );
        assert!(
            !unclaimed_answer.is_pairable(),
            "an output must positively claim a terminal state"
        );
        let items = [item(cx3_call()), unclaimed_answer];
        assert_eq!(
            pairing_of(&items),
            vec![
                ToolSearchPairing::CounterpartPresent,
                ToolSearchPairing::Incomplete
            ],
            "the call stays un-paired: a status-less output did not answer it"
        );
    }

    /// `from_wire` is a gate, not a sniff: a sibling item type is refused, and the error
    /// carries the `type` it actually found.
    #[test]
    fn a_neighbouring_item_type_is_not_a_discovery_item() {
        assert_eq!(
            ToolSearchItem::from_wire(json!({"type": "function_call", "call_id": "c1"})),
            Err(ToolSearchItemError::NotADiscoveryItem {
                item_type: Some("function_call".to_owned())
            })
        );
        assert_eq!(
            ToolSearchItem::from_wire(json!("tool_search_call")),
            Err(ToolSearchItemError::NotAnObject)
        );
    }

    /// The STORE round-trip is byte-exact: every field this type has no named
    /// accessor for must come back unchanged. This fixture carries ONE such field,
    /// `internal_chat_message_metadata_passthrough` — a mock-driven-dry-run artifact
    /// (PLAN:1492-1495 — absent from the live items, "seen only behind
    /// mock_upstream"), and the only accessor-less key on `cx1_namespaced_output()`.
    /// The second such field this module knows about, `created_by`, is a different
    /// story — observed on live provider bytes and a proven replay hazard
    /// (PLAN:1325-1328) — and it is round-tripped where it is actually carried:
    /// `an_unknown_status_degrades_the_view_not_the_bytes` adds it to a call, and the
    /// hosted halves in `SOL_HOSTED_KEY_ORDERS` end with it. This asserts the STORE
    /// only — it is deliberately not a claim that these bytes are wire-ready: the
    /// echoed call's `created_by` 400s on replay and the strip-list that removes it
    /// belongs to T15 (PLAN:1421).
    #[test]
    fn unknown_fields_survive_the_store_round_trip() {
        let original = cx1_namespaced_output();
        let parsed = item(original.clone());

        let stored = serde_json::to_string(&parsed).unwrap();
        let reread: ToolSearchItem = serde_json::from_str(&stored).unwrap();
        assert_eq!(reread, parsed);
        assert_eq!(reread.raw(), &original);
        assert_eq!(
            serde_json::to_string(&reread).unwrap(),
            stored,
            "key order survives too — Value equality alone would not catch a reorder"
        );
        // The passthrough object's turn_id string and its create_time FLOAT need no
        // asserts of their own: the whole-`Value` equality above compares them, and a value
        // re-serialised out of its JSON type would fail THAT compare. The byte-order assert
        // is the one equality alone cannot make.
    }

    /// A status this enum does not model is a VIEW loss, never a byte loss.
    #[test]
    fn an_unknown_status_degrades_the_view_not_the_bytes() {
        let mut raw = cx3_call();
        raw["status"] = json!("queued");
        let parsed = item(raw.clone());
        assert_eq!(parsed.status(), ToolSearchStatus::Unknown);
        assert!(!parsed.is_completed());
        assert_eq!(parsed.raw(), &raw, "bytes unchanged");
        // The strip-list must still be able to see the field it is not told
        // about: losing `created_by` here would turn T15's strip pass into a
        // silent no-op and leave the 400 in place.
        raw["created_by"] = json!("user:palanisd");
        let with_author = item(raw.clone());
        let reread: ToolSearchItem =
            serde_json::from_str(&serde_json::to_string(&with_author).unwrap()).unwrap();
        // Whole-`Value` equality is the pin: `created_by` is a key of `raw`, so losing it
        // fails this compare. An assert naming the key by hand could only fail if the line
        // above changed.
        assert_eq!(reread.raw(), &raw);
    }

    /// The three reach claims on the derived `Deserialize`, measured rather than
    /// asserted from the serde docs: the vocabulary spellings reach their own variants, and
    /// an unmodelled status STRING is tolerated
    /// (`#[serde(other)]`), a non-string `status` is a hard type error — which
    /// is NOT what [`ToolSearchItem::from_wire`] does with the same value, so
    /// the two paths must not be described as equivalent.
    #[test]
    fn the_status_view_tolerates_an_unknown_string_and_rejects_a_non_string() {
        // The three VOCABULARY spellings have to reach their named variants: that is what
        // `rename_all` buys, and without it every real item fails to load in a consumer
        // that embeds this enum, which is the failure the derive promises cannot happen.
        for (wire, want) in [
            ("in_progress", ToolSearchStatus::InProgress),
            ("completed", ToolSearchStatus::Completed),
            ("error", ToolSearchStatus::Error),
        ] {
            assert_eq!(
                serde_json::from_value::<ToolSearchStatus>(json!(wire)).unwrap(),
                want,
                "`{wire}` is a vocabulary spelling and must not fall through"
            );
        }
        assert_eq!(
            serde_json::from_value::<ToolSearchStatus>(json!("queued")).unwrap(),
            ToolSearchStatus::Unknown
        );
        assert!(serde_json::from_value::<ToolSearchStatus>(json!(null)).is_err());
        assert!(serde_json::from_value::<ToolSearchStatus>(json!(3)).is_err());
        // `from_wire` reads the same value as far more tolerantly, and it is the
        // path the store actually uses.
        assert_eq!(ToolSearchStatus::from_wire(None), ToolSearchStatus::Unknown);
        assert_eq!(
            ToolSearchStatus::from_wire(Some("queued")),
            ToolSearchStatus::Unknown
        );
        // The direct-API statement of exact match. `status()` delegates here, so the
        // item-level controls in `an_unmodelled_status_is_neither_a_skeleton_nor_a_certified_pair`
        // also reach a normalising `from_wire`; what this probe owns is the view in
        // isolation, without an item, a key or a pairing in the way.
        assert_eq!(
            ToolSearchStatus::from_wire(Some(" completed")),
            ToolSearchStatus::Unknown,
            "a leading space keeps the status unmodelled"
        );
    }

    /// A definition shape we do not model is retained and replayed, but nothing
    /// is inferred from it.
    #[test]
    fn an_unrecognised_definition_shape_is_retained_but_contributes_nothing() {
        let mut raw = r6_flat_output();
        raw["tools"] = json!([
            { "type": "toolset", "name": "mystery", "members": ["a"] },
            r6_flat_output()["tools"][0].clone()
        ]);
        let parsed = item(raw);
        let tools = parsed.tools();
        assert_eq!(tools[0].kind, DiscoveredToolKind::Other);
        assert!(tools[0].callable_definitions().is_empty());
        let loaded = loaded_tool_set([&parsed]);
        assert_eq!(
            loaded.iter().filter_map(|l| l.name()).collect::<Vec<_>>(),
            vec!["lookup_shipping_eta"]
        );
        // ...and the RETENTION half of the name, which the assertions above do
        // not touch: the shape we do not model must still be replayable verbatim.
        let reread: ToolSearchItem =
            serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(reread.raw(), parsed.raw());
        assert_eq!(
            reread.raw()["tools"][0],
            json!({ "type": "toolset", "name": "mystery", "members": ["a"] }),
            "the unmodelled entry survives intact — no drop-to-null, no rewrite"
        );
    }

    /// Token accounting must see the payload, not the one-line summary: on
    /// Responses these definitions ride in the cached prefix.
    #[test]
    fn model_visible_length_is_the_payload_not_the_summary() {
        let grouped = item(cx1_namespaced_output());
        assert!(grouped.text_summary().len() < 60, "summary stays bounded");
        let payload = grouped.estimated_model_visible_len();
        assert!(
            payload > 10 * grouped.text_summary().len(),
            "the definitions are the cost, and they are model-visible"
        );
        // Falsifiable version of "the measure counts the definitions": remove the
        // payload and the measure must fall by (about) the definitions' own bytes.
        // An implementation that measured the summary, or that counted items
        // instead of bytes, cannot satisfy this.
        let definitions: usize = loaded_tool_set([&grouped])
            .iter()
            .map(LoadedDefinition::estimated_model_visible_len)
            .sum();
        let mut bare = cx1_namespaced_output();
        bare.as_object_mut().unwrap().remove("tools");
        let without = item(bare).estimated_model_visible_len();
        assert!(
            payload - without > definitions * 8 / 10,
            "removing `tools` must remove about the definitions' bytes: \
             {payload} - {without} vs {definitions}"
        );
    }

    /// A search that found nothing is a LEGITIMATE empty answer, not a missing
    /// one: PLAN:24 makes zero-match "success with `tools: []` (no fallback
    /// text)" and PLAN:22's D-ERR channel emits the same empty array with
    /// `status:"error"`. Both must (a) contribute no definitions, (b) summarise
    /// as a count of zero rather than crash or claim a namespace, and (c) still
    /// answer their call — an empty `tools` array is not an absent output.
    #[test]
    fn an_empty_tools_array_is_an_answer_with_zero_definitions() {
        for status in ["completed", "error"] {
            let raw = json!({
                "type": "tool_search_output",
                "call_id": "call_zero_hit",
                "status": status,
                "execution": "client",
                "tools": []
            });
            let output = item(raw);
            assert!(
                output.tools().is_empty(),
                "nothing discovered under {status}"
            );
            assert_eq!(loaded_tool_set([&output]).len(), 0);
            assert_eq!(
                output.text_summary(),
                "[tool_search results] 0 tools",
                "the user-visible line for a zero-hit search under {status}"
            );
            let call = item(json!({
                "type": "tool_search_call", "call_id": "call_zero_hit",
                "execution": "client", "arguments": { "query": "nothing matches" }
            }));
            assert_eq!(
                pairing_of(&[call, output]),
                vec![ToolSearchPairing::Paired, ToolSearchPairing::Paired],
                "an empty answer still closes the pair under {status}"
            );
        }
    }

    /// The summary's counting loop has three arms; the `Other` arm (an entry
    /// this build does not model) must count as ZERO invocable tools — not
    /// crash, and not silently count a shape nobody can invoke.
    #[test]
    fn the_summary_counts_unmodelled_definitions_as_zero_tools() {
        let raw = json!({
            "type": "tool_search_output", "call_id": "call_mixed",
            "status": "completed", "execution": "client",
            "tools": [
                { "type": "custom", "name": "freeform" },
                { "type": "function", "name": "known" }
            ]
        });
        let output = item(raw);
        assert_eq!(
            output.text_summary(),
            "[tool_search results] 1 tool",
            "the custom entry is carried verbatim but contributes no invocable tool"
        );
        assert_eq!(output.tools().len(), 2, "both entries are still retained");
        let only_other = item(json!({
            "type": "tool_search_output", "call_id": "call_custom",
            "status": "completed", "execution": "client",
            "tools": [{ "type": "custom", "name": "freeform" }]
        }));
        assert_eq!(only_other.text_summary(), "[tool_search results] 0 tools");
    }

    /// Precedence inside `pairing_of`: `Incomplete` is decided BEFORE `Unkeyed`.
    /// A copy that is not conversation state yet has nothing to join, so the
    /// state verdict is the informative one; the reverse order would call a
    /// streamed skeleton "unkeyed", which is the quadrant-flagged verdict and
    /// invites a caller to reason about a key that does not exist yet.
    #[test]
    fn an_unpairable_copy_is_incomplete_before_it_is_unkeyed() {
        let mut no_status_no_key = r6_flat_output();
        no_status_no_key.as_object_mut().unwrap().remove("status");
        no_status_no_key.as_object_mut().unwrap().remove("call_id");
        let output = item(no_status_no_key);
        assert_eq!(output.call_id(), None, "the fixture is keyless too");
        assert_eq!(
            pairing_of(std::slice::from_ref(&output)),
            vec![ToolSearchPairing::Incomplete]
        );
        // The skeleton call, same precedence.
        assert_eq!(
            pairing_of(std::slice::from_ref(&item({
                let mut skeleton = cx3_skeleton_call();
                skeleton.as_object_mut().unwrap().remove("call_id");
                skeleton
            }))),
            vec![ToolSearchPairing::Incomplete]
        );
    }

    /// The query echo is capped at [`MAX_SUMMARY_QUERY_BYTES`] and the cap must
    /// survive a UTF-8 boundary landing INSIDE a multi-byte character:
    /// `truncate_bytes` backs off to a boundary, and the escape (`{:?}`) runs
    /// AFTER truncation, so the emitted line can never contain half a character.
    #[test]
    fn the_query_echo_in_the_summary_is_capped_on_a_char_boundary() {
        // 199 ASCII bytes then a 2-byte char: byte 200 is in the MIDDLE of the
        // `é`, so a naive `&query[..200]` panics.
        let query = format!("{}é", "x".repeat(199));
        assert!(
            !query.is_char_boundary(MAX_SUMMARY_QUERY_BYTES),
            "the fixture must straddle the cap or it tests nothing"
        );
        let mut raw = cx3_call();
        raw["arguments"]["query"] = json!(query);
        let summary = item(raw).text_summary();
        assert_eq!(
            summary,
            format!("[tool_search] {:?}…", "x".repeat(199)),
            "cut on the boundary, escaped after the cut, visibly truncated"
        );

        // The over-cap multi-byte case, where the cap lands mid-character many
        // times over: decode the escaped echo back out and check what actually
        // reached the user-visible line.
        let mut multi = cx3_call();
        multi["arguments"]["query"] = json!("é".repeat(400));
        let multi_summary = item(multi).text_summary();
        let inner = multi_summary
            .strip_prefix("[tool_search] ")
            .and_then(|rest| rest.strip_suffix('…'))
            .unwrap_or_else(|| panic!("capped summary shape, got {multi_summary:?}"));
        let decoded: String = serde_json::from_str(inner)
            .expect("the escaped echo must be valid JSON, not half a character");
        assert_eq!(
            decoded,
            "é".repeat(100),
            "200 bytes of a 2-byte char = 100 whole chars, none cut in half"
        );

        // A cap that never fires is an untested cap: below the limit there is no
        // ellipsis and no truncation.
        assert!(!item(cx3_call()).text_summary().ends_with('…'));
    }

    /// The two `text_summary()` shapes a human actually sees, including the `(no query)`
    /// arm that a completed paths-form hosted call reaches as well as the skeleton.
    #[test]
    fn summaries_name_the_query_and_the_loaded_counts() {
        assert_eq!(
            item(cx3_call()).text_summary(),
            "[tool_search] \"crm order management\""
        );
        assert_eq!(
            item(cx1_namespaced_output()).text_summary(),
            "[tool_search results] 3 tools in 1 namespace(s)"
        );
        assert_eq!(
            item(r6_flat_output()).text_summary(),
            "[tool_search results] 1 tool",
            "the count is grammatical — this line is user-visible in the pager"
        );

        // The singular noun INSIDE a group is the one shape no capture produces: CX1's
        // namespace holds 3 children and CX3's hold 7 and 1, so `callables == 1` and
        // `namespaces > 0` never co-occur in the fixtures. One child, one group:
        let one_in_a_group = item(json!({
            "type": "tool_search_output", "call_id": "call_one_child",
            "status": "completed", "execution": "client",
            "tools": [{
                "type": "namespace", "name": "mcp__one",
                "tools": [{ "type": "function", "name": "only" }]
            }]
        }));
        assert_eq!(
            one_in_a_group.text_summary(),
            "[tool_search results] 1 tool in 1 namespace(s)",
            "grammatical inside a group, not only beside one"
        );

        // The `(no query)` arm is NOT only the stream skeleton. A real COMPLETED
        // hosted call reaches it too: `R1_SOL_HOSTED` `output[1]` carries
        // `arguments: {"paths":["lookup_shipping_eta"]}` — a paths-form search
        // with no `query` key at all (see [`ToolSearchItem::query`]). The summary
        // must not pretend such a call asked for nothing, so it says
        // "(no query)" rather than "" — and a caller that wants the actual
        // request reads `raw()`.
        assert_eq!(
            item(cx3_skeleton_call()).text_summary(),
            "[tool_search] (no query)"
        );
        assert_eq!(
            item(sol_hosted_server_pair()[0].clone()).text_summary(),
            "[tool_search] (no query)",
            "a completed paths-form call lands on the same arm"
        );
    }

    /// `ToolSearchKind` and the wire `type` tag are a bijection over the two discovery
    /// tags and map nothing else.
    #[test]
    fn kind_and_wire_tag_agree_in_both_directions() {
        for kind in [ToolSearchKind::Call, ToolSearchKind::Output] {
            assert_eq!(ToolSearchKind::from_item_type(kind.item_type()), Some(kind));
        }
        assert_eq!(ToolSearchKind::from_item_type("function_call"), None);
        assert_eq!(ToolSearchKind::from_item_type("reasoning"), None);
    }

    /// One definition found by two searches loads once — the dedup spans the whole slice,
    /// not each item on its own.
    #[test]
    fn duplicate_definitions_across_searches_load_once() {
        let first = item(r6_flat_output());
        let second = item(r6_flat_output());
        assert_eq!(loaded_tool_set([&first, &second]).len(), 1);
    }

    /// The dedup key is `(definition value, namespace)` — deep `Value` equality,
    /// not a name compare — and the namespace half
    /// is load-bearing: A-16 says the flat form and the namespaced form are two
    /// different ways to invoke, so the same child under two groups is TWO
    /// entries. A `(name)`-keyed or bytes-only dedup silently deletes an
    /// invocable form.
    #[test]
    fn the_same_definition_under_two_namespaces_loads_twice() {
        let child = cx1_namespaced_output()["tools"][0]["tools"][0].clone();
        let group = |name: &str| {
            json!({
                "type": "tool_search_output",
                "call_id": "search-multi-group",
                "status": "completed",
                "execution": "client",
                "tools": [{
                    "type": "namespace", "name": name,
                    "tools": [child]
                }]
            })
        };
        let alpha = item(group("mcp__alpha"));
        let beta = item(group("mcp__beta"));
        let both = loaded_tool_set([&alpha, &beta]);
        assert_eq!(
            both.iter()
                .map(|l| (l.name().unwrap(), l.namespace()))
                .collect::<Vec<_>>(),
            vec![
                ("crm_fixture_tool_00", Some("mcp__alpha")),
                ("crm_fixture_tool_00", Some("mcp__beta")),
            ],
            "two groups, two invocable forms"
        );
        assert_eq!(
            loaded_tool_set([&alpha, &alpha]).len(),
            1,
            "but the SAME group twice is still one entry — the key is not a no-op"
        );
    }

    /// The membership test is ACROSS KINDS, and that gate is what keeps the two
    /// destructive verdicts honest: two unanswered calls sharing a key are TWO
    /// `UnansweredCall`s (PLAN:948 synthesises an answer for each), not each
    /// other's counterpart. Same in reverse for two outputs with no call.
    #[test]
    fn a_counterpart_of_the_same_kind_is_no_counterpart() {
        let call = |id: &str, call_id: &str| {
            item(json!({
                "type": "tool_search_call", "id": id, "call_id": call_id,
                "status": "completed", "execution": "client",
                "arguments": { "query": "q" }
            }))
        };
        let output = |id: &str, call_id: &str| {
            item(json!({
                "type": "tool_search_output", "id": id, "call_id": call_id,
                "status": "completed", "execution": "client",
                "tools": [{ "type": "function", "name": "n" }]
            }))
        };
        assert_eq!(
            pairing_of(&[call("tsc_1", "k"), call("tsc_2", "k")]),
            vec![
                ToolSearchPairing::UnansweredCall,
                ToolSearchPairing::UnansweredCall,
            ],
            "a second CALL does not answer the first"
        );
        assert_eq!(
            pairing_of(&[output("tso_1", "k"), output("tso_2", "k")]),
            vec![
                ToolSearchPairing::OrphanOutput,
                ToolSearchPairing::OrphanOutput,
            ],
            "a second OUTPUT does not supply the missing call"
        );
    }

    /// "Nearest unclaimed output AFTER the call" has to be pinned, not assumed:
    /// with two candidate outputs under one key the first one wins and the second
    /// stays `CounterpartPresent`. Both exclusivity arms are owned elsewhere, not
    /// here: that an output is never claimed TWICE is
    /// `same_kind_items_never_pair_even_when_they_share_a_call_id`, and that a later
    /// call steps OVER a claimed row instead of stopping at it is
    /// `an_already_claimed_output_is_stepped_over_not_stopped_at`.
    #[test]
    fn a_call_takes_the_nearest_unclaimed_output_and_no_further() {
        let keyed = |id: &str, call_id: &str, name: &str| {
            item(json!({
                "type": "tool_search_output",
                "id": id,
                "call_id": call_id,
                "status": "completed",
                "execution": "client",
                "tools": [{ "type": "function", "name": name }]
            }))
        };
        let call = |id: &str, call_id: &str| {
            item(json!({
                "type": "tool_search_call",
                "id": id,
                "call_id": call_id,
                "status": "completed",
                "execution": "client",
                "arguments": { "query": "q" }
            }))
        };
        // c1 has two candidates (o1 then o2) and c2 is keyed differently, with its
        // only candidate after o2. What this fixture kills is a matcher that takes
        // the LAST same-key candidate instead of the nearest: that answer is
        // [Some(2), None, Some(0), Some(4), Some(3)]. A shared-cursor "scan on"
        // implementation is NOT killed here — it reaches o3 by the same route and
        // yields this same vector.
        let items = [
            call("tsc_1", "k1"),
            keyed("tso_1", "k1", "one"),
            keyed("tso_2", "k1", "two"),
            call("tsc_2", "k2"),
            keyed("tso_3", "k2", "three"),
        ];
        assert_eq!(
            partner_indices(&items),
            vec![Some(1), Some(0), None, Some(4), Some(3)],
            "c1 takes the NEAREST candidate; o2 stays unclaimed; c2 finds its own"
        );
        assert_eq!(
            pairing_of(&items),
            vec![
                ToolSearchPairing::Paired,
                ToolSearchPairing::Paired,
                ToolSearchPairing::CounterpartPresent,
                ToolSearchPairing::Paired,
                ToolSearchPairing::Paired,
            ]
        );
    }

    /// The cap is `> MAX_SUMMARY_QUERY_BYTES`, not `>=`.
    #[test]
    fn a_query_of_exactly_the_cap_is_echoed_whole_and_unmarked() {
        let exact = "y".repeat(MAX_SUMMARY_QUERY_BYTES);
        assert_eq!(exact.len(), MAX_SUMMARY_QUERY_BYTES);
        let mut raw = cx3_call();
        raw["arguments"]["query"] = json!(exact.clone());
        let summary = item(raw).text_summary();
        assert_eq!(
            summary,
            format!("[tool_search] {exact:?}"),
            "at exactly the cap the whole query is echoed, with no truncation marker"
        );
        assert!(!summary.ends_with('…'), "nothing was cut: {summary}");
        let mut over = cx3_call();
        over["arguments"]["query"] = json!(format!("{exact}z"));
        assert!(
            item(over).text_summary().ends_with('…'),
            "cap+1 must be the first truncation"
        );
    }

    /// An empty query is echoed, not replaced. `query()` deliberately keeps `""`
    /// (see its doc), and the `(no query)` line belongs to a call that sent no
    /// query at all — not to one that sent an empty one.
    #[test]
    fn an_empty_query_is_echoed_and_the_no_query_fallback_stays_for_absence() {
        let mut raw = cx3_call();
        raw["arguments"]["query"] = json!("");
        let call = item(raw);
        assert_eq!(
            call.query(),
            Some(""),
            "the query is payload, not an identity"
        );
        assert_eq!(
            call.text_summary(),
            "[tool_search] \"\"",
            "the summary echoes the empty query instead of claiming there was none"
        );

        // The fallback still answers the shape it exists for: no `arguments` at
        // all, and a `query` that is not a string.
        let mut no_args = cx3_call();
        no_args.as_object_mut().unwrap().remove("arguments");
        assert_eq!(
            item(no_args).text_summary(),
            "[tool_search] (no query)",
            "an absent query"
        );
        let mut wrong_type = cx3_call();
        wrong_type["arguments"]["query"] = json!(7);
        assert_eq!(
            item(wrong_type).text_summary(),
            "[tool_search] (no query)",
            "and an unparseable one"
        );
    }

    /// A namespace group with no readable `tools` contributes nothing —
    /// above all it must not yield the GROUP itself as a definition.
    #[test]
    fn a_namespace_group_with_no_readable_children_yields_nothing() {
        for shape in ["absent", "string", "map", "null"] {
            let mut raw = cx1_namespaced_output();
            match shape {
                "absent" => {
                    raw["tools"][0].as_object_mut().unwrap().remove("tools");
                }
                "string" => raw["tools"][0]["tools"] = json!("mcp__ratchet_fixture"),
                "map" => raw["tools"][0]["tools"] = json!({"name": "crm_fixture_tool_00"}),
                _ => raw["tools"][0]["tools"] = Value::Null,
            }
            let output = item(raw);
            let group = &output.tools()[0];
            assert_eq!(group.kind, DiscoveredToolKind::Namespace, "{shape}");
            assert!(
                group.callable_definitions().is_empty(),
                "a group with no children declares nothing ({shape})"
            );
            assert!(
                loaded_tool_set([&output]).is_empty(),
                "and the loaded set gains no phantom tool ({shape})"
            );
            // No `invocable_names` assert here on purpose: it is a `filter_map` over the
            // loaded set the line above just proved empty, so it cannot fail
            // independently. The A-16 "a GROUP name is never an invocation form"
            // property is pinned by `a_namespace_group_is_not_a_callable_but_its_children_are`.
        }
    }

    /// One key is exactly ONE group, even when its items interleave with
    /// another key's.
    #[test]
    fn one_key_is_one_group_even_when_two_keys_interleave() {
        // Deliberately NON-alphabetical first-seen keys: with `call_a` first, a first-seen
        // order and a sorted order produce the same expectation and this test is blind to a
        // sort regression. With z-then-a it kills one outright: sorting the keys before the
        // group assembly fails THIS test's group-order assert as well as
        // `groups_come_back_in_first_seen_order_not_sorted_by_key`.
        let items = [
            keyed_call("call_z", Some("completed")),
            keyed_call("call_a", Some("completed")),
            keyed_output("call_z", Some("completed")),
        ];
        assert_eq!(
            call_id_groups(&items),
            vec![vec![0, 2], vec![1]],
            "first-seen key order (a sort would return [[1], [0, 2]]), one group per \
             key, indices ascending"
        );
        let groups = call_id_groups(&items);
        let flat: Vec<usize> = groups.iter().flatten().copied().collect();
        let mut deduped = flat.clone();
        deduped.sort_unstable();
        deduped.dedup();
        assert_eq!(deduped.len(), flat.len(), "groups are disjoint: {groups:?}");
        assert_eq!(
            flat.len(),
            3,
            "and every keyed item is in exactly one group"
        );
    }

    /// The dedup key is the definition's whole VALUE, so two same-named
    /// definitions that differ in any byte both load.
    #[test]
    fn two_different_definitions_sharing_a_name_both_load() {
        let answer = |field: &str| {
            item(json!({
                "type": "tool_search_output", "call_id": "call_rename",
                "status": "completed", "execution": "client",
                "tools": [{
                    "type": "function", "name": "same_name",
                    "parameters": { "type": "object",
                        "properties": { field: { "type": "string" } } }
                }]
            }))
        };
        let first = answer("order_id");
        let second = answer("return_window");
        assert_eq!(
            first.tools()[0].name(),
            second.tools()[0].name(),
            "same invocable name…"
        );
        assert_ne!(
            first.raw()["tools"][0],
            second.raw()["tools"][0],
            "…different bytes"
        );
        assert_eq!(
            loaded_tool_set([&first, &second]).len(),
            2,
            "both load: the dedup key is the definition, not its name (A-14)"
        );
        assert_eq!(
            invocable_names([&first, &second]),
            vec![("same_name", None), ("same_name", None)],
            "the ambiguity is REPORTED as two entries, not resolved by deleting one"
        );
    }

    /// A discovered entry measures its payload, not its name.
    #[test]
    fn a_discovered_entry_measures_its_whole_payload() {
        let output = item(cx1_namespaced_output());
        let group = &output.tools()[0];
        assert_eq!(
            group.estimated_model_visible_len(),
            group.raw().to_string().len(),
            "the entry's own bytes, and that equality is the whole pin: a JSON envelope is \
             always longer than the name inside it, so a second assert comparing the two \
             could fail only on the fixture, never on the code"
        );
        let full = item(r6_flat_output());
        let bare = item(json!({
            "type": "tool_search_output", "call_id": "call_len_probe", "status": "completed",
            "execution": "client",
            "tools": [{ "type": "function", "name": "lookup_shipping_eta" }]
        }));
        assert_eq!(full.tools()[0].name(), bare.tools()[0].name());
        assert!(
            full.tools()[0].estimated_model_visible_len()
                > bare.tools()[0].estimated_model_visible_len(),
            "the same name with a schema costs more than the name alone"
        );
    }

    /// A namespace group carrying no readable `name` has NO namespace half.
    #[test]
    fn an_unnamed_namespace_group_has_no_namespace_half() {
        // `empty` is the shape the `DiscoveredTool::name` filter actually changes; the
        // other two degrade on their own (`as_str` returns None).
        for shape in ["absent", "number", "empty"] {
            let mut raw = cx1_namespaced_output();
            match shape {
                "absent" => {
                    raw["tools"][0].as_object_mut().unwrap().remove("name");
                }
                "empty" => raw["tools"][0]["name"] = json!(""),
                _ => raw["tools"][0]["name"] = json!(7),
            }
            let output = item(raw);
            let loaded = loaded_tool_set([&output]);
            assert_eq!(loaded.len(), 3, "the children still load ({shape})");
            for entry in &loaded {
                assert_eq!(
                    entry.namespace(),
                    None,
                    "no group name, no namespace half ({shape}) — Some(\"\") would \
                     advertise a group that identifies nothing"
                );
                assert!(!entry.name().unwrap_or_default().is_empty(), "{shape}");
            }
            assert!(
                invocable_names([&output])
                    .iter()
                    .all(|(_, ns)| ns.is_none()),
                "and the invocation pair carries no empty group ({shape})"
            );
        }
    }

    /// Empty names: `""` was filtered on `id()` and `call_id()` and on nothing else.
    /// A group named `""` handed out `Some("")` as the namespace
    /// half, and a definition named `""` was reported as an invocable empty name —
    /// either of which becomes a tool identity one join away (A-16). The fix is TWO
    /// sites, `DiscoveredTool::name` and `LoadedDefinition::name`, and the
    /// justification is uniformity with the two id getters: PLAN:1035's A2 lint
    /// constrains ids, `status` and `tools` but never `name`, and no node in the
    /// 76-file corpus census carries an empty `name`.
    #[test]
    fn an_empty_group_or_definition_name_is_not_an_identity() {
        let mut group = cx1_namespaced_output();
        group["tools"][0]["name"] = json!("");
        let group = item(group);
        assert_eq!(
            invocable_names([&group]),
            vec![
                ("crm_fixture_tool_00", None),
                ("crm_fixture_tool_06", None),
                ("crm_fixture_tool_09", None),
            ],
            "a group named \"\" is a group-less child — the namespace half must not be Some(\"\")"
        );

        let nameless = item(json!({
            "type": "tool_search_output", "call_id": "call_empty_name", "status": "completed",
            "execution": "client",
            "tools": [{ "type": "function", "name": "", "parameters": {} }]
        }));
        assert_eq!(
            invocable_names([&nameless]),
            Vec::<(&str, Option<&str>)>::new(),
            "an empty name is not an invocation form, exactly as a missing one is not"
        );
        assert_eq!(
            loaded_tool_set([&nameless]).len(),
            1,
            "the bytes are still loaded — only the invocation form is refused"
        );
    }

    /// Model-visible length is BYTES (the cached prefix is counted in
    /// bytes), including for a multi-byte payload.
    #[test]
    fn model_visible_length_counts_bytes_not_chars() {
        let mut raw = cx3_call();
        raw["arguments"]["query"] = json!("é".repeat(10));
        let text = raw.to_string();
        let item = item(raw);
        assert_ne!(text.len(), text.chars().count(), "the fixture must differ");
        assert_eq!(
            item.estimated_model_visible_len(),
            text.len(),
            "the byte length of the stored JSON, which is what the prefix costs"
        );
    }

    /// An index past the end orders against nothing EVEN WHEN the slice
    /// holds keyed items — the existing out-of-range pin uses a keyless slice, so
    /// it cannot tell a false-because-out-of-range from a false-because-keyless.
    #[test]
    fn an_index_past_the_end_orders_against_nothing_even_for_a_keyed_slice() {
        let items = [
            keyed_call(CX3_KEY, Some("completed")),
            keyed_output(CX3_KEY, Some("completed")),
        ];
        assert_eq!(items[0].call_id(), Some(CX3_KEY), "the slice IS keyed");
        for past in [items.len(), items.len() + 1, usize::MAX] {
            assert!(
                !call_precedes_output(&items, past),
                "no call precedes an index that does not exist: {past}"
            );
            assert!(
                !output_follows_call(&items, past),
                "no output follows an index that does not exist: {past}"
            );
        }
        assert_eq!(
            partner_indices(&items),
            vec![Some(1), Some(0)],
            "and the in-range pair is unaffected"
        );
    }

    /// The empty slice is a real input answered with nothing.
    #[test]
    fn an_empty_discovery_slice_answers_with_nothing() {
        let none: &[ToolSearchItem] = &[];
        assert!(partner_indices(none).is_empty());
        assert!(pairing_of(none).is_empty());
        assert!(call_id_groups(none).is_empty());
        assert!(loaded_tool_set(none).is_empty());
        assert!(invocable_names(none).is_empty());
        assert!(!call_precedes_output(none, 0));
        assert!(!output_follows_call(none, 0));
        assert!(!keyless_client_answer_present(none));
    }

    /// Release-only companion to the wrong-kind pins above: the scan range never includes the
    /// indexed position, so a wrong-kind index cannot pair the item with itself. The indexed item
    /// IS read — for its `call_id` only. Nothing in a debug run catches
    /// this — the `debug_assert` fires first — so the house GATE (which runs
    /// `--release`) is what makes this test load-bearing.
    #[cfg(not(debug_assertions))]
    #[test]
    fn a_wrong_kind_index_cannot_be_its_own_counterpart() {
        let items = [
            keyed_call(CX3_KEY, Some("completed")),
            keyed_output(CX3_KEY, Some("completed")),
        ];
        assert!(
            !output_follows_call(&items, 1),
            "index 1 is an OUTPUT, so the call-side scan starts at skip(2) and never sees index 1"
        );
        assert!(
            !call_precedes_output(&items, 0),
            "and index 0 is a CALL, so the output-side scan is take(0) and never sees index 0"
        );
    }

    /// An answer that carries no `call_id` is invisible to every reader the synthesis
    /// licence is told to consult — and the veto that follows from it is scoped to the
    /// client quadrant, because that is all PLAN:948 governs.
    #[test]
    fn an_unkeyed_answer_is_invisible_to_the_synthesis_licence() {
        // The keyless form the corpus actually carries is `call_id: null` — 8 of the 31
        // discovery items, the two halves of each of the four hosted captures; 0 omit the
        // key — so that is the shape this test builds. The omitted key is STATED as an
        // equivalent below, not pinned by it: that assert runs one accessor over two
        // byte shapes, so no realistic misimplementation of `call_id()` — one that
        // reads `null` and absent alike — could fail it. The two shapes' VERDICTS are
        // pinned apart, not together: the omitted form's `Unkeyed` is
        // `an_item_without_a_call_id_loads_but_can_never_pair` (which never feeds a
        // null), and the captured null form's is this test plus
        // `a_provider_minted_pair_without_a_join_key_is_unkeyed_not_orphaned`.
        let answer = item({
            let mut raw = cx1_namespaced_output();
            raw["call_id"] = Value::Null;
            raw
        });
        let mut omitted_key_form = cx1_namespaced_output();
        omitted_key_form.as_object_mut().unwrap().remove("call_id");
        assert_eq!(
            item(omitted_key_form).call_id(),
            answer.call_id(),
            "an omitted `call_id` and the captured `call_id: null` read identically through \
             `call_id()`; only the null form is stated by PLAN:1248 or present in the corpus"
        );
        let call = keyed_call("call_gone_key", Some("completed"));
        let items = [call.clone(), answer];
        assert_eq!(
            pairing_of(&items),
            vec![
                ToolSearchPairing::UnansweredCall,
                ToolSearchPairing::Unkeyed
            ],
            "the verdict family has no way to say 'answered, but unkeyed'"
        );
        assert!(call.is_client_executed(), "PLAN:948's scope test passes");
        assert!(
            call.id().is_some(),
            "PLAN:948's tso_synthetic_id input exists"
        );
        assert!(
            !output_follows_call(&items, 0),
            "the order predicate agrees there is no answer"
        );
        assert_eq!(
            loaded_tool_set(&items).len(),
            3,
            "…while the search's 3 definitions ARE in the slice"
        );
        assert_eq!(
            call_id_groups(&items),
            vec![vec![0]],
            "and the call's group holds the call alone"
        );
        assert!(
            keyless_client_answer_present(&items),
            "so the fourth condition fires — minting here would double-answer one search"
        );
        // The veto is POSITION-blind, because PLAN:948's predicate is "any client call
        // with no output" and that rule never reads where the answer sits. Same two items,
        // order the only variable: a veto routed through the ordering predicate would stop
        // firing here even though no item changed.
        let answer_first = [items[1].clone(), items[0].clone()];
        assert!(
            keyless_client_answer_present(&answer_first),
            "an answer placed BEFORE its call is still an answer"
        );

        // And it is scoped. Every keyless COMPLETED row the corpus actually contains
        // is the provider-minted hosted pair with `execution:"server"` — the observed
        // case on `ToolSearchPairing::Unkeyed`, which PLAN:947 exempts from removal
        // outright and which is provably not any client call's answer. A status-only
        // veto fires on it and would deny PLAN:948 synthesis to every unanswered client
        // call in a history that also carries a hosted search.
        let hosted: Vec<ToolSearchItem> =
            sol_hosted_server_pair().iter().cloned().map(item).collect();
        assert!(
            hosted.iter().any(|out| {
                out.kind() == ToolSearchKind::Output
                    && out.call_id().is_none()
                    && out.raw_status() == Some(STATUS_COMPLETED)
            }),
            "the hosted output is keyless and completed, so an unscoped veto fires here"
        );
        // The quadrant conjunct on its own: every row of the hosted pair is
        // server-executed, so the non-firing below is that test working and not
        // an accident of this fixture.
        assert!(
            hosted.iter().all(|out| !out.is_client_executed()),
            "every hosted row is server-executed: the quadrant test, not luck"
        );
        let mut mixed = vec![keyed_call("call_still_unanswered", Some("completed"))];
        mixed.extend(hosted);
        assert_eq!(
            pairing_of(&mixed)[0],
            ToolSearchPairing::UnansweredCall,
            "the client call is still unanswered in the mixed history"
        );
        assert!(
            !keyless_client_answer_present(&mixed),
            "and the scoped veto leaves its synthesis alone: {mixed:?}"
        );

        // The other conjuncts, one control case each. Without these the predicate's
        // ANSWERED test (`raw_status()` is `completed` or `error`) and its keylessness test
        // are mutation-immune, and this predicate's four CONJUNCTS (kind / keylessness /
        // quadrant / ANSWERED) are only partly pinned. That four is this module's decomposition of
        // the veto, not PLAN:948's: the LICENCE's four conditions (on
        // `ToolSearchPairing::UnansweredCall`) share only the client-quadrant test with it, and
        // the licence's fourth condition IS this predicate.
        let mut skeleton_raw = output_raw("call_veto_skeleton", Some(json!("in_progress")));
        skeleton_raw["call_id"] = Value::Null;
        let skeleton = item(skeleton_raw);
        assert!(
            skeleton.is_client_executed()
                && skeleton.call_id().is_none()
                && skeleton.raw_status() == Some(STATUS_IN_PROGRESS),
            "the veto's shape minus COMPLETED (asserted on the raw string the \
             predicate itself reads, not through the view)"
        );
        assert!(
            !keyless_client_answer_present(std::slice::from_ref(&skeleton)),
            "an unkeyed stream skeleton has not answered anything, so it cannot veto a mint"
        );
        let keyed = keyed_output("call_veto_keyed", Some("completed"));
        assert!(
            keyed.is_client_executed()
                && keyed.raw_status() == Some(STATUS_COMPLETED)
                && keyed.call_id().is_some(),
            "the veto's shape minus keylessness"
        );
        assert!(
            !keyless_client_answer_present(std::slice::from_ref(&keyed)),
            "a completed answer that carries a key is visible to `pairing_of` itself, so it \
             is not the veto's shape"
        );
        // And it is an OUTPUT test: a keyless client CALL is a half-written search, not an
        // answer, and must not suspend synthesis for the rest of the slice.
        let loose_call = keyed_call("", Some("completed"));
        assert!(
            loose_call.kind() == ToolSearchKind::Call
                && loose_call.call_id().is_none()
                && loose_call.is_client_executed()
                && loose_call.raw_status() == Some(STATUS_COMPLETED),
            "the veto's shape minus the OUTPUT kind"
        );
        assert!(
            !keyless_client_answer_present(std::slice::from_ref(&loose_call)),
            "a keyless client call answers nothing, so it cannot veto a mint"
        );
    }

    /// PLAN:947's removal words are status-blind; this verdict is not. An orphaned keyed
    /// output splits across `OrphanOutput` and `Incomplete` on its OWN `status`, which is
    /// the measurement behind the "LESS the copies the guard claims first" clause on
    /// [`ToolSearchPairing::OrphanOutput`].
    #[test]
    fn orphaned_outputs_split_between_this_verdict_and_incomplete_by_status() {
        for (status, verdict) in [
            (Some(json!("in_progress")), ToolSearchPairing::Incomplete),
            (Some(json!("completed")), ToolSearchPairing::OrphanOutput),
            (Some(json!("error")), ToolSearchPairing::OrphanOutput),
            (Some(json!("cancelled")), ToolSearchPairing::OrphanOutput),
            (None, ToolSearchPairing::Incomplete),
        ] {
            let orphan = item(output_raw("call_no_call_anywhere", status.clone()));
            assert_eq!(orphan.kind(), ToolSearchKind::Output, "fixture sanity");
            assert!(orphan.call_id().is_some(), "…and it IS keyed");
            assert_eq!(
                pairing_of(std::slice::from_ref(&orphan)),
                vec![verdict],
                "no call exists in the slice for any of these five shapes, so the status \
                 alone decides which of the two verdicts reports the orphan"
            );
        }
    }

    /// The `error` arm of the ANSWERED test on
    /// [`ToolSearchPairing::UnansweredCall`]'s fourth condition. An error copy carries no
    /// definitions, so what the veto protects here is not a loaded tool set — it is the
    /// one-answer-per-search invariant.
    #[test]
    fn an_errored_keyless_answer_vetoes_the_synthetic_mint() {
        let answer = item({
            let mut raw = cx1_namespaced_output();
            raw["call_id"] = Value::Null;
            raw["status"] = json!("error");
            raw
        });
        let call = keyed_call("call_answered_by_error", Some("completed"));
        let items = [call.clone(), answer];
        assert_eq!(
            pairing_of(&items),
            vec![
                ToolSearchPairing::UnansweredCall,
                ToolSearchPairing::Unkeyed
            ],
            "the keyed readers still cannot see the answer, so the call reads unanswered"
        );
        assert!(
            loaded_tool_set(&items).is_empty(),
            "the fixture DOES carry 3 definitions — they are skipped because \
             `loaded_tool_set` gates on COMPLETED, which is the point: an errored answer \
             contributes nothing, so what the veto protects here is the one-answer \
             invariant, not a tool set"
        );
        assert!(
            keyless_client_answer_present(&items),
            "yet the search DID answer, so minting `tso_synthetic_id` here would be the \
             second answer the fourth condition exists to prevent"
        );
    }

    /// An empty key must stay out of a real group's indices. The verdicts of empty-keyed
    /// copies and their non-pairing belong to
    /// `an_empty_call_id_is_normalised_to_absent_and_pairs_with_nothing`; what is unique
    /// here is a MIXED slice — the empty key must not
    /// merely be unkeyed, it must not shift or absorb the indices of the real pair beside it.
    #[test]
    fn two_empty_string_keys_group_with_nothing() {
        let loose_call = keyed_call("", Some("completed"));
        let loose_output = keyed_output("", Some("completed"));
        let call = keyed_call("call_real_pair", Some("completed"));
        let output = keyed_output("call_real_pair", Some("completed"));
        let items = [loose_call, loose_output, call, output];
        assert!(
            items[0].call_id().is_none() && items[1].call_id().is_none(),
            "fixture sanity: an empty string is no key"
        );
        assert_eq!(
            call_id_groups(&items),
            vec![vec![2, 3]],
            "the empty key owns no group"
        );
        // pairing_of's verdicts for empty-keyed copies are NOT re-asserted here:
        // `an_empty_call_id_is_normalised_to_absent_and_pairs_with_nothing` owns them, and a
        // second copy of a verdict vector pins nothing new.
    }

    /// A `name` that is not a string is not an identity, but the entry is still a
    /// definition and the bytes still load: this module never rejects a row for one field.
    #[test]
    fn a_non_string_definition_name_is_not_an_identity_but_the_bytes_load() {
        let output = item(json!({
            "type": "tool_search_output", "call_id": "call_name_forms", "execution": "client",
            "status": "completed",
            "tools": [
                { "type": "function", "name": 7, "description": "numeric name" },
                { "type": "function", "name": null, "description": "null name" },
                { "type": "function", "name": "ok_name", "description": "kept" }
            ]
        }));
        let names: Vec<Option<&str>> = output.tools().iter().map(DiscoveredTool::name).collect();
        assert_eq!(
            names,
            vec![None, None, Some("ok_name")],
            "only a real string is a name"
        );
        let items = [output];
        assert_eq!(
            loaded_tool_set(&items).len(),
            3,
            "the two nameless entries are still definitions"
        );
        assert_eq!(
            invocable_names(&items),
            vec![("ok_name", None)],
            "and exactly one of the three is invocable"
        );
    }

    /// The `tools` array is provider-supplied. Entries that are not objects — scalars,
    /// arrays, an object with no fields, one with only a `namespace` — contribute nothing,
    /// break nothing, and keep their bytes for replay.
    #[test]
    fn non_object_tools_entries_load_nothing_and_keep_their_bytes() {
        let raw = json!({
            "type": "tool_search_output", "call_id": "call_junk_tools", "execution": "client",
            "status": "completed",
            "tools": ["a string", 7, null, true, [1, 2], {}, { "namespace": "only_ns" }]
        });
        let output = item(raw.clone());
        assert_eq!(output.tools().len(), 7, "every entry is still an entry");
        let items = [output];
        assert!(
            loaded_tool_set(&items).is_empty(),
            "none of them is a definition"
        );
        assert!(invocable_names(&items).is_empty(), "so none is invocable");

        let stored = serde_json::to_string(&items[0]).expect("a discovery item serialises");
        let reread: ToolSearchItem = serde_json::from_str(&stored).expect("and re-reads");
        assert_eq!(reread.raw(), &raw, "decode tolerant, encode strict");
    }

    /// `description()` is the text accessor that does NOT filter `""`: an empty description
    /// is a real empty description, and only an absent key or a non-string reads `None`.
    /// [`DiscoveredTool::description`] documents the arm; nothing ASSERTED the
    /// empty-string arm before this test — the shared `Some(non-empty)` reading was
    /// already pinned by `the_borrowed_views_expose_the_bytes_they_wrapped`.
    #[test]
    fn an_empty_description_is_a_real_empty_description() {
        let output = item(json!({
            "type": "tool_search_output", "call_id": "call_desc_forms", "execution": "client",
            "status": "completed",
            "tools": [
                { "type": "function", "name": "a", "description": "" },
                { "type": "function", "name": "b" },
                { "type": "function", "name": "c", "description": 5 },
                { "type": "function", "name": "d", "description": "kept" }
            ]
        }));
        let descriptions: Vec<Option<&str>> = output
            .tools()
            .iter()
            .map(DiscoveredTool::description)
            .collect();
        assert_eq!(
            descriptions,
            vec![Some(""), None, None, Some("kept")],
            "`\"\"` stays Some(\"\") — unlike `name()`, which filters it to None"
        );
    }

    // ==========================================================================
    // apex-waj.21 — the `ConversationItem::Discovery` variant battery.
    //
    // Each ST-M-D line names one behaviour, the harness mutant number that breaks
    // the arm, and the test that must redden (harness: /tmp/waj21-evidence/mutate.py,
    // witnesses: /tmp/waj21-evidence/mutants/NN-red.log + NN-green.log).
    // Lines marked NO MUTANT are labelled for what they actually are — a shape
    // rationale, or a mutation the types make impossible — and are NOT witnesses
    // (cut review W21R1-04/06/07: a test that cannot fail is the defect being audited).
    // The chat-state battery keeps its own CS-M-Dn ids; the two maps are namespaced
    // apart on purpose (W21R1-14).
    //   ST-M-D1  (07) envelope `type` tag renamed              → `discovery_envelope_nests_the_provider_item_under_a_named_field`
    //            (r2 correction, cut review R2L2-05): mutant 07 reddens ONLY that test —
    //            `mutants/07-red.log` shows the store round-trip test passing beside it,
    //            because a self-round-trip re-reads the same derive the mutation edits.
    //            The on-disk spelling is pinned SEPARATELY, by the checked-in `STORED_PAIR_LINES`
    //            literal in `the_store_spelling_of_a_stored_pair_is_a_checked_in_literal` — a
    //            different test, not this one — and that one IS discriminating: harness mutant 68
    //            applies this same tag rename and reddens it. 68 exists as its own id because 07's
    //            filter is `discovery`, which that test's name does not contain, so 07's log could
    //            never have shown it (cut review R2L2-05).
    //            The newtype form itself (`Discovery(ToolSearchItem)`) is NOT a one-line
    //            mutation — the `{ item }` struct pattern is written at every arm site — so
    //            `a_newtype_discovery_variant_would_emit_two_type_tags_and_a_loader_would_drop_them`
    //            argues the SHAPE on a local fixture type and cannot be reddened by any
    //            product change. Kept as documentation, claimed as nothing (W21R1-07).
    //   ST-M-D2  (07) inner field renamed / flattened          → `discovery_envelope_nests_the_provider_item_under_a_named_field`
    //   ST-M-D3  (01) `role()` returns Tool                    → `discovery_role_is_assistant_and_its_text_view_is_the_bounded_summary`
    //   ST-M-D4  (02) `text_content()` returns raw / empty     → same test
    //   ST-M-D5  (03) encoder emits 2 slots for one item       → `discovery_encoder_flattens_one_slot_per_item_and_the_splice_lands_in_that_slot`
    //   ST-M-D6  (26) splice rebuilt from the parsed model instead of `raw()` verbatim → same slot test
    //   ST-M-D7  (04) the Xai dialect splices anyway (no wire evidence) → `discovery_replay_is_per_row_class_and_fail_closed_without_wire_evidence`
    //   ST-M-D8  "the encoder may never half-emit" is pinned by the SLOT-COUNT test
    //            above (harness 03), NOT by the lint test: `check_h8` reports only an
    //            empty, orphaned or duplicated `call_id`, so a body with no output half
    //            at all is lint-clean. `discovery_pair_passes_the_outbound_lint…` proves
    //            the lint sees an orphaned output — that, and nothing more (W21R1-06).
    //   ST-M-D9  (23) `drop_model_bound_items` drops the pair  → `discovery_survives_the_model_bound_strip_and_reports_zero_drops`
    //   ST-M-D10 (25) a discovery `call_id` counted as a live tool-call owner → `a_discovery_call_id_is_not_a_live_tool_call_owner`
    //   ST-M-D11 NO MUTANT: `ToolSearchItem::raw(&self) -> &Value` is the only accessor
    //            and there is no setter anywhere in the crate, so no affinity gate, mint
    //            stamp or cwd walk CAN rewrite the bytes. `no_wire_walk_rewrites_discovery_bytes`
    //            pins an invariant the type already enforces; it is listed because the
    //            invariant matters, not because a mutant backs it (W21R1-04).
    //   ST-M-D12 (49) the cross-provider fallback renders the pair → `codex_cross_provider_fallback_omits_discovery_state`
    //   ST-M-D13 (24) the dangling-repair walk stops at a pair → `repair_dangling_tool_calls_is_transparent_to_a_discovery_pair`
    //   ST-M-D14 (28) switch projection drops the pair         → `switch_projection_keeps_discovery_verbatim_on_every_boundary`
    //   ST-M-D15 (21) the ChatCompletions stub clears the reasoning fold → `chat_completions_projection_renders_a_stub_and_keeps_the_reasoning_fold`
    //   ST-M-D16 (22) the Messages wire emits a typed block    → `messages_wire_renders_one_bounded_text_block_per_discovery_item`
    //   ST-M-D17 (05) the pair-snap split test inverted        → `snap_index_over_discovery_pairs_never_splits_a_group`
    //   ST-M-D18 (31) the snap takes ONE pass instead of looping to a fixpoint (review F-2) → `snap_index_over_discovery_pairs_loops_until_a_fixpoint_when_pairs_interleave`
    //   ST-M-D19 (30) closedness read off `len()` instead of one Call + one Output (review F-3) → `unpaired_discovery_indices_requires_one_call_and_one_output`, `a_keyed_pair_groups_across_an_intervening_duplicate_call`
    //   ST-M-D20 (06) the closedness predicate inverted (every pair looks open) → the chat-state tail-window keep tests
    //   ST-M-D21 (43) a history cut stops snapping (review F-4) → `truncate_for_prompt_never_returns_a_count_inside_a_discovery_pair`
    // ==========================================================================

    use crate::conversation::{
        DanglingToolCallReason, ResponsesReplayDialect, Role, ToolCall,
        apply_enc_affinity_gate, codex_cross_provider_fallback, conversation_to_chat_messages,
        drop_model_bound_items, drop_orphaned_tool_results, repair_dangling_tool_calls,
        stamp_reasoning_mint_tag, transform_conversation_cwd,
    };
    use crate::conversation::{ConversationItem, ConversationRequest};

    /// The only way to build the variant outside `conversation.rs`: the payload
    /// type has no public constructor except [`ToolSearchItem::from_wire`].
    fn disc(raw: Value) -> ConversationItem {
        ConversationItem::Discovery { item: item(raw) }
    }

    /// The CX1 namespace payload re-keyed onto the CX3 call, so the two fixtures
    /// form ONE `call_id` group. The captures author them as separate pairs
    /// (`cx3_call` ↔ `call_AOphypzlL1KKckJugyBS2PYn`, the CX1 dry run ↔
    /// `dryrun-search-1`); nothing here claims a provider ever sent this exact
    /// combination — it is the pair shape the harness has to keep together.
    fn paired_output_raw() -> Value {
        let mut raw = cx1_namespaced_output();
        raw["call_id"] = json!(CX3_KEY);
        raw
    }

    /// A whole conversation carrying one complete client-executed pair, in the
    /// order the provider authored it: call, then output, same `call_id`.
    fn history_with_discovery_pair() -> Vec<ConversationItem> {
        vec![
            ConversationItem::system("sys"),
            ConversationItem::user("find the crm tools"),
            disc(cx3_call()),
            disc(paired_output_raw()),
            ConversationItem::assistant("found them"),
        ]
    }

    /// M-D2: the on-disk / in-memory envelope. `ConversationItem` is
    /// internally tagged on `type`, and `ToolSearchItem::Serialize` re-emits the
    /// provider bytes verbatim (which already carry `type`), so the payload MUST
    /// ride under a named field — a flattened newtype emits two `type` keys (see
    /// [`a_newtype_discovery_variant_would_emit_two_type_tags_and_a_loader_would_drop_them`]).
    #[test]
    fn discovery_envelope_nests_the_provider_item_under_a_named_field() {
        let json = serde_json::to_string(&disc(cx3_call())).unwrap();
        let value: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            value["type"],
            json!("discovery"),
            "the IR tag is `discovery`; `item_kind_str` in xai-chat-state and the \
             jsonl loader both read this spelling"
        );
        assert_eq!(
            value["item"], cx3_call(),
            "the provider item rides verbatim under `item`"
        );
        let top_level_keys: Vec<&str> = value
            .as_object()
            .expect("envelope is an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            top_level_keys,
            vec!["type", "item"],
            "the envelope adds exactly one key beside the tag — nothing of the \
             provider item is hoisted or re-wrapped"
        );
        assert_eq!(
            json.matches("\"type\":").count(),
            2,
            "exactly two `type` keys: the IR tag and the provider tag inside `item`"
        );
    }

    /// M-D1 witness for the shape decision, pinned rather than argued: the
    /// internally-tagged NEWTYPE form serialises with two `type` keys, and a
    /// reader that has already parsed the line into a `Value` sees only the
    /// LAST one — the provider's tag — so `from_value::<ConversationItem>` fails
    /// with "unknown variant" and the jsonl loader's `skip_line`
    /// (`xai-grok-shell/src/session/storage/jsonl/mod.rs`) drops the row with a
    /// warning. That is a silent A-26 drop of one half of the pair, which is why
    /// the variant is a named-field struct variant instead.
    #[test]
    fn a_newtype_discovery_variant_would_emit_two_type_tags_and_a_loader_would_drop_them() {
        #[derive(Debug, serde::Serialize, serde::Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum NaiveConversationItem {
            Discovery(ToolSearchItem),
        }

        let naive = NaiveConversationItem::Discovery(item(cx3_call()));
        let json = serde_json::to_string(&naive).unwrap();
        assert_eq!(
            json.matches("\"type\":").count(),
            2,
            "the newtype form flattens `raw` beside the injected tag: {json}"
        );

        let as_value: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            as_value["type"],
            json!("tool_search_call"),
            "a duplicate key collapses to the LAST occurrence, so the IR tag is gone"
        );
        let reloaded: Result<NaiveConversationItem, _> = serde_json::from_value(as_value.clone());
        assert!(
            reloaded.is_err(),
            "and the collapsed line is not a known variant — the loader's path for it \
             is `skip_line`, i.e. the pair loses a half in silence: {reloaded:?}"
        );
    }

    /// The exact bytes `chat_history.jsonl` carries for one pair, as a checked-in
    /// literal (cut review R2L2-05).
    ///
    /// A round-trip through the same `Serialize`/`Deserialize` pair the mutation edits
    /// cannot redden a tag rename — `#[serde(rename = "tool_search_item")]` is
    /// symmetric, so `to_string` → `from_str` → `to_string` compares two identical
    /// spellings and stays green while every previously-stored pair becomes an unknown
    /// variant the loader `skip_line`s past. Only a literal written by a DIFFERENT
    /// author than the derive can pin the spelling, which is exactly what
    /// `discovery_envelope_nests_the_provider_item_under_a_named_field` does for the
    /// tag; this is that literal for the whole stored line, both kinds.
    /// Witnessed by harness mutant 68, which renames the tag exactly as mutant 07 does —
    /// 07's own filter is `discovery` and this test's name does not contain that, so the
    /// literal needed its own id rather than a wider claim on 07.
    const STORED_PAIR_LINES: [&str; 2] = [
        r#"{"type":"discovery","item":{"type":"tool_search_call","id":"tsc_probe_call_store","call_id":"call_store","execution":"client","arguments":{"query":"crm order management","limit":8}}}"#,
        r#"{"type":"discovery","item":{"type":"tool_search_output","id":"tso_probe_call_store","call_id":"call_store","execution":"client","tools":[]}}"#,
    ];

    #[test]
    fn the_store_spelling_of_a_stored_pair_is_a_checked_in_literal() {
        let items = vec![
            ConversationItem::Discovery {
                item: keyed_call("call_store", None),
            },
            ConversationItem::Discovery {
                item: keyed_output("call_store", None),
            },
        ];
        let stored: Vec<String> = items
            .iter()
            .map(|item| serde_json::to_string(item).expect("a discovery row serialises"))
            .collect();
        assert_eq!(
            stored,
            STORED_PAIR_LINES,
            "the spelling a `chat_history.jsonl` row carries is a contract with every \
             file already on disk: a renamed tag or a re-wrapped payload makes the \
             loader's `skip_line` drop a whole pair in silence (apex-waj.18 A-26)"
        );
        // And the literal is live, not decorative: it re-reads to the same items.
        let reread: Vec<ConversationItem> = stored
            .iter()
            .map(|line| serde_json::from_str(line).expect("the checked-in literal parses"))
            .collect();
        assert_eq!(
            serde_json::to_string(&reread).expect("re-read items serialise"),
            serde_json::to_string(&items).expect("the originals serialise"),
            "the literal must re-read to the very items it was written from"
        );
    }

    /// M-D1 + M-D2 + rule 7: `chat_history.jsonl` writes and reads these bytes,
    /// so the pair must round-trip byte-identically AND keep its order and its
    /// shared `call_id` (call precedes output is the H-8 precondition).
    /// The on-disk SPELLING is not this test's claim — see
    /// [`STORED_PAIR_LINES`], which a derive-side rename cannot glide past.
    #[test]
    fn a_discovery_pair_round_trips_the_store_bytes_verbatim_in_call_output_order() {
        let items = history_with_discovery_pair();
        let stored = serde_json::to_string(&items).unwrap();
        let reread: Vec<ConversationItem> = serde_json::from_str(&stored).unwrap();
        assert_eq!(
            serde_json::to_string(&reread).unwrap(),
            stored,
            "byte-for-byte store round-trip — a key reorder is a cache-break"
        );

        let discovery: Vec<&ToolSearchItem> = reread
            .iter()
            .filter_map(ConversationItem::discovery)
            .collect();
        assert_eq!(discovery.len(), 2, "both halves survive");
        assert_eq!(discovery[0].kind(), ToolSearchKind::Call);
        assert_eq!(discovery[1].kind(), ToolSearchKind::Output);
        assert_eq!(discovery[0].raw(), &cx3_call());
        assert_eq!(discovery[1].raw(), &paired_output_raw());
        assert_eq!(
            discovery[0].call_id(),
            discovery[1].call_id(),
            "the pair keeps its join key"
        );
    }

    /// M-D3 + M-D4: `role()` and `text_content()` are the two arms every
    /// wildcard consumer reads through (`CompactionItem`, transcript renderers,
    /// fingerprint, digests). Role MUST be `Assistant`: `Role::Tool` makes
    /// `CompactionItem::is_tool_result()` true and corrupts every split-snap
    /// predicate in the shared compaction engine. Text MUST be the bounded
    /// summary, never the payload (`text_summary()` is bounded by
    /// `MAX_SUMMARY_QUERY_BYTES`; the loaded set can be tens of KB).
    #[test]
    fn discovery_role_is_assistant_and_its_text_view_is_the_bounded_summary() {
        let call = disc(cx3_call());
        let output = disc(paired_output_raw());
        for item in [&call, &output] {
            assert_eq!(
                item.role(),
                Role::Assistant,
                "a discovery item is model-side continuation state, not a tool result"
            );
        }
        assert_eq!(call.text_content(), "[tool_search] \"crm order management\"");
        assert_eq!(
            output.text_content(),
            "[tool_search results] 3 tools in 1 namespace(s)"
        );
        assert!(
            output.text_content().len() < 64,
            "the view stays bounded while the payload is {} bytes",
            output.discovery().unwrap().estimated_model_visible_len()
        );
    }

    /// M-D5 + M-D6: the Responses encoder owns the splice-index contract.
    /// `patch_raw_input_replacements`
    /// (`xai-grok-sampler/src/client.rs:740-763`) OVERWRITES `input[index]`
    /// wholesale, so the encoder must register exactly one placeholder slot per
    /// discovery item and the splice must carry `raw()` verbatim — one extra or
    /// one missing slot silently replaces the WRONG item (the assistant turn, in
    /// the assert below).
    #[test]
    fn discovery_encoder_flattens_one_slot_per_item_and_the_splice_lands_in_that_slot() {
        let request = ConversationRequest::from_items(history_with_discovery_pair());
        let slots: Vec<usize> = request
            .items
            .iter()
            .map(|item| crate::conversation::responses::conversation_item_to_input_items(item).len())
            .collect();
        assert_eq!(
            slots,
            vec![1; 5],
            "one flattened input slot per conversation item"
        );

        let inner: crate::rs::CreateResponse = (&request).into();
        let mut body = serde_json::to_value(inner).unwrap();
        let input = body["input"].as_array().expect("input array").clone();
        assert_eq!(input.len(), 5);
        assert_eq!(
            input[2]["content"],
            json!("[tool_search] \"crm order management\""),
            "pre-splice the slot holds the bounded placeholder, never the provider payload"
        );

        let replacements = request.raw_responses_input_replacements(ResponsesReplayDialect::Other);
        assert_eq!(replacements.len(), 2, "both halves splice");
        let expected_indices: Vec<usize> = replacements
            .iter()
            .map(|r| r.input_item_index)
            .collect();
        assert_eq!(
            expected_indices,
            vec![2, 3],
            "splice indices are the TYPED prefix sums — same rule as the \
             CodexRawInput/XSearch carriers"
        );
        for replacement in &replacements {
            body["input"][replacement.input_item_index] = replacement.value.clone();
        }
        assert_eq!(
            body["input"][2], cx3_call(),
            "the spliced bytes are `raw()` verbatim: no re-serialisation, no \
             re-wrapped `arguments`, no stripped key (T15 owns the `created_by` strip)"
        );
        assert_eq!(body["input"][3], paired_output_raw());
        assert_eq!(
            body["input"][4]["role"],
            json!("assistant"),
            "the neighbour slot is untouched — an off-by-one splice overwrites it"
        );
    }

    /// M-D7: replay is a per-row-class wire decision (wire invariant C4). The
    /// evidence, per dialect:
    /// - `Other` (the Strict family): PROVEN on live bytes —
    ///   `ratchet-capture/captures/2026-09-25-ratchet-live/wire2-live3/req-004.json`
    ///   (gpt-5.5 through the deployed proxy, `store=false`) sent
    ///   `tool_search_call` at `input[11]` and `tool_search_output` at
    ///   `input[12]`, same `call_id`, and `resp-004.sse` opened a response — the
    ///   very capture that measured A-26 (12 tools sent, 14 echoed).
    /// - `Codex`: the donor replays its own pairs
    ///   (`ratchet-capture/fixtures/codex/CX1-toolsearch-mcp-dryrun/next-turn.json`
    ///   `input[3]`/`input[4]`, `CX3-toolsearch-5.5-LIVE/next-turn.json`
    ///   `input[11]`/`input[12]`).
    /// - `Xai`: NO evidence a grok row models either item type, so the carrier
    ///   stays fail-closed on the bounded placeholder (the XSearch precedent)
    ///   rather than inventing a wire type.
    #[test]
    fn discovery_replay_is_per_row_class_and_fail_closed_without_wire_evidence() {
        let request = ConversationRequest::from_items(history_with_discovery_pair());
        for dialect in [
            ResponsesReplayDialect::Other,
            ResponsesReplayDialect::Codex,
        ] {
            let replacements = request.raw_responses_input_replacements(dialect);
            assert_eq!(
                replacements.len(),
                2,
                "{dialect:?} is evidenced to read the pair from history"
            );
            assert_eq!(replacements[0].value, cx3_call());
            assert_eq!(replacements[1].value, paired_output_raw());
        }

        assert!(
            request
                .raw_responses_input_replacements(ResponsesReplayDialect::Xai)
                .is_empty(),
            "no captured grok row accepts a tool_search item — splice nothing"
        );
        let inner: crate::rs::CreateResponse = (&request).into();
        let body = serde_json::to_value(inner).unwrap();
        assert_eq!(
            body["input"][3],
            json!({"type":"message","role":"assistant","content":"[tool_search results] 3 tools in 1 namespace(s)"}),
            "the fail-closed shape is the bounded summary, so the model still knows \
             tools were loaded even where the pair cannot ride"
        );
    }

    /// M-D8: the H-8/H-9/H-10 pair invariants are value-level, so they lint the
    /// spliced request. A whole pair is clean; an encoder that loses the call
    /// half must trip H-8 (`tool_search_output` with no preceding
    /// `tool_search_call`), which is the machine-checked proof that the encoder
    /// may never half-emit.
    #[test]
    fn discovery_pair_passes_the_outbound_lint_and_a_lost_call_half_trips_h8() {
        let request = ConversationRequest::from_items(history_with_discovery_pair());
        let inner: crate::rs::CreateResponse = (&request).into();
        let mut body = serde_json::to_value(inner).unwrap();
        // H-4 is the sampler's own send-time contract (`store = Some(false)`,
        // client.rs) — the typed body built here has not passed through it yet.
        body["store"] = json!(false);
        for replacement in request.raw_responses_input_replacements(ResponsesReplayDialect::Other) {
            body["input"][replacement.input_item_index] = replacement.value.clone();
        }

        let violations = crate::conversation::outbound_lint::lint_outbound_request(
            crate::conversation::projection::Boundary::AzStrict,
            &body,
        );
        assert_eq!(
            violations,
            Vec::new(),
            "a verbatim replayed pair must satisfy H-3/H-8/H-9/H-10 — every id rides \
             as minted: {violations:?}"
        );

        let mut half = body.clone();
        let input = half["input"].as_array_mut().unwrap();
        input.remove(2);
        let violations = crate::conversation::outbound_lint::lint_outbound_request(
            crate::conversation::projection::Boundary::AzStrict,
            &half,
        );
        assert!(
            violations.iter().any(|v| v.rule == "H-8"),
            "the orphaned output must be visible to the lint: {violations:?}"
        );
    }

    /// M-D9 — A-26 at the reactive strip. `drop_model_bound_items` is the single
    /// source of truth for both the sampler's in-flight strip-retry and the
    /// chat-state persisted strip; its retain predicate ends in a wildcard arm,
    /// which is exactly where a 7th variant would be dropped silently. A
    /// discovery pair is provider-side loaded-tool state, not model-bound
    /// continuation state, so it must survive and the strip must report `0` for
    /// it (the sampler's one-retry net fails closed on a zero-count strip —
    /// `retry.rs:135-136`).
    #[test]
    fn discovery_survives_the_model_bound_strip_and_reports_zero_drops() {
        let mut items = history_with_discovery_pair();
        assert_eq!(
            drop_model_bound_items(&mut items),
            0,
            "the pair is not strippable state — and a discovery-only history gives \
             the reactive net nothing to rescue, which is the recorded consequence"
        );
        assert_eq!(
            items.iter().filter_map(ConversationItem::discovery).count(),
            2,
            "both halves survive, in order"
        );

        let mut mixed = history_with_discovery_pair();
        mixed.push(ConversationItem::Reasoning(
            crate::synthesized_reasoning_item("private continuation").into(),
        ));
        let dropped = drop_model_bound_items(&mut mixed);
        assert_eq!(dropped, 1, "the genuinely model-bound item still goes");
        assert_eq!(
            mixed.iter().filter_map(ConversationItem::discovery).count(),
            2,
            "and the pair rides through the same walk"
        );
    }

    /// M-D10: a client `tool_search` is answered by a `tool_search_output`, never
    /// by a `function_call_output`, so its `call_id` is deliberately NOT a live
    /// call owner. Treating it as one would keep a real orphaned tool result on
    /// the wire (the Azure `Invalid 'input[N].call_id'` 400 this net exists to
    /// keep off).
    #[test]
    fn a_discovery_call_id_is_not_a_live_tool_call_owner() {
        let mut items = vec![
            disc(cx3_call()),
            disc(paired_output_raw()),
            ConversationItem::tool_result(CX3_KEY, "looks like a match, is not one"),
        ];
        assert_eq!(
            drop_orphaned_tool_results(&mut items),
            1,
            "the result is orphaned: only an Assistant tool_call or a BackendToolCall owns a call id"
        );
        assert_eq!(items.len(), 2, "the discovery pair itself is never pair-checked here");
    }

    /// M-D11: opaque artifacts are handles, not content (wire invariant 6) and
    /// byte-exact replay is what keeps the cached prefix. No send-time walk may
    /// rewrite discovery bytes — not the affinity gate, not the mint stamp (the
    /// item carries no ciphertext), not the CWD transform (even if a query or a
    /// tool description names the old workspace).
    #[test]
    fn no_wire_walk_rewrites_discovery_bytes() {
        let mut items = history_with_discovery_pair();
        let before: Vec<Value> = items
            .iter()
            .filter_map(ConversationItem::discovery)
            .map(|t| t.raw().clone())
            .collect();

        let (stripped, retained, carriers) = apply_enc_affinity_gate(&mut items, Some("pin-1"));
        assert_eq!((stripped, retained, carriers), (0, 0, 0));
        stamp_reasoning_mint_tag(&mut items, Some("pin-1"));
        transform_conversation_cwd(&mut items, "/old/cwd", "/new/cwd");

        let after: Vec<Value> = items
            .iter()
            .filter_map(ConversationItem::discovery)
            .map(|t| t.raw().clone())
            .collect();
        assert_eq!(
            after, before,
            "a discovery item's raw payload is never rewritten in place"
        );
        assert_eq!(
            serde_json::to_string(&items).unwrap(),
            serde_json::to_string(&history_with_discovery_pair()).unwrap()
        );
    }

    /// M-D12: the plaintext cross-provider fallback is the last-resort transcript
    /// for a session that leaves the provider. The loaded set is provider state,
    /// so it is intentionally ABSENT from the plaintext (A-26 is a Responses-wire
    /// concern); the bounded summary would only churn the fallback text.
    #[test]
    fn codex_cross_provider_fallback_omits_discovery_state() {
        let items = history_with_discovery_pair();
        let fallback = codex_cross_provider_fallback(&items, 4096);
        assert!(fallback.contains("find the crm tools"));
        assert!(fallback.contains("found them"));
        assert!(
            !fallback.contains("tool_search")
                && !fallback.contains("crm_fixture_tool_00")
                && !fallback.contains("mcp__ratchet_fixture"),
            "neither the marker nor any loaded tool name may leak into the fallback: {fallback}"
        );
    }

    /// M-D14: the switch projector keeps the pair verbatim on every boundary
    /// (tier D0, Keep). It is NOT a `BackendToolCall`, so the Vertex T3 arm
    /// cannot reach it, and `DropReason` has no Discovery variant — a discovery
    /// drop is not even expressible here today. `changed == 0` also pins that no
    /// backup-gated full-history rewrite is triggered by a pair (chat-state
    /// `projection_changed_count`).
    #[test]
    fn switch_projection_keeps_discovery_verbatim_on_every_boundary() {
        let items = history_with_discovery_pair();
        for boundary in [
            crate::conversation::projection::Boundary::AzStrict,
            crate::conversation::projection::Boundary::VLLenient,
            crate::conversation::projection::Boundary::Vertex,
        ] {
            let projected = crate::conversation::projection::project_switch_history(&items, "target-row", boundary, None);
            assert_eq!(projected.items.len(), items.len(), "{boundary:?}");
            assert!(projected.drops.is_empty(), "{boundary:?}: {:?}", projected.drops);
            let discovery: Vec<&ToolSearchItem> = projected
                .items
                .iter()
                .filter_map(ConversationItem::discovery)
                .collect();
            assert_eq!(discovery[0].raw(), &cx3_call(), "{boundary:?}");
            assert_eq!(discovery[1].raw(), &paired_output_raw(), "{boundary:?}");
        }
    }

    /// M-D15: the ChatCompletions wire has no typed discovery item. The safe stub
    /// is ONE synthetic assistant text message per half (the BackendToolCall
    /// precedent) — and, like BackendToolCall, it must NOT clear the pending
    /// reasoning fold: a persisted pair sits between the reasoning and its
    /// assistant on every ChatCompletions row, and a `panic!` is not a safe stub.
    #[test]
    fn chat_completions_projection_renders_a_stub_and_keeps_the_reasoning_fold() {
        let mut items = history_with_discovery_pair();
        items.insert(
            2,
            ConversationItem::Reasoning(crate::synthesized_reasoning_item("thinking first").into()),
        );
        let messages = conversation_to_chat_messages(items);
        let roles: Vec<String> = messages
            .iter()
            .map(|m| serde_json::to_value(m.role).unwrap().as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            roles,
            vec!["system", "user", "assistant", "assistant", "assistant"],
            "one synthetic assistant per half, no orphan tool message"
        );
        let stubs: Vec<String> = messages[2..4]
            .iter()
            .map(|m| match &m.content {
                crate::types::MessageContent::Text(text) => text.clone(),
                crate::types::MessageContent::Blocks(blocks) => {
                    panic!("discovery stub must be bare text, got {blocks:?}")
                }
            })
            .collect();
        assert_eq!(
            stubs,
            vec![
                "[tool_search] \"crm order management\"".to_owned(),
                "[tool_search results] 3 tools in 1 namespace(s)".to_owned(),
            ]
        );
        for message in &messages[2..4] {
            assert!(message.tool_calls.is_empty());
            assert_eq!(message.tool_call_id, None, "no tool_use/tool_result id is invented");
        }
        assert_eq!(
            messages[4].reasoning_content.as_deref(),
            Some("thinking first"),
            "the reasoning still folds onto its own assistant turn"
        );
    }

    /// M-D16: the /messages wire has no typed discovery item either. It renders
    /// as bounded assistant text — and explicitly NOT as a `tool_reference`
    /// block: H-11 (declared-name lint) fires for an undeclared tool name, and
    /// A-24.2's `tools[]` materialisation is a send-time encoder job (later bead).
    #[test]
    fn messages_wire_renders_one_bounded_text_block_per_discovery_item() {
        let request = ConversationRequest::from_items(history_with_discovery_pair())
            .with_model("messages-compatible-model");
        let body = serde_json::to_value(crate::conversation::build_messages_request(&request)).unwrap();
        let rendered = body.to_string();
        assert!(rendered.contains("[tool_search] \\\"crm order management\\\""));
        assert!(
            !rendered.contains("tool_reference")
                && !rendered.contains("tool_use")
                && !rendered.contains("tool_result"),
            "no block type the target wire would reject: {rendered}"
        );
        assert!(
            !rendered.contains("crm_fixture_tool_00"),
            "the loaded definitions must not ride into the /messages request"
        );
        // D5 orphan cleanup must never eat a discovery item: it only pair-checks
        // Assistant <-> ToolResult.
        let cleaned = crate::conversation::messages::clean_orphaned_items(&request.items);
        assert_eq!(
            cleaned.iter().filter_map(ConversationItem::discovery).count(),
            2
        );
    }

    /// M-D13: the dangling-repair walk collects answered call ids over the
    /// CONTIGUOUS ToolResult run and breaks on anything else. A discovery pair
    /// between an assistant-with-calls and its results therefore made the calls
    /// look unanswered and spliced DUPLICATE synthetic results into history.
    #[test]
    fn repair_dangling_tool_calls_is_transparent_to_a_discovery_pair() {
        let mut items = vec![
            ConversationItem::assistant_tool_calls(vec![ToolCall {
                id: "call_real".into(),
                name: "read_file".into(),
                arguments: "{}".into(),
            }]),
            disc(cx3_call()),
            disc(paired_output_raw()),
            ConversationItem::tool_result("call_real", "file contents"),
        ];
        assert_eq!(
            repair_dangling_tool_calls(&mut items, DanglingToolCallReason::UserCancelled),
            0,
            "the call IS answered — a discovery item must not end the result run"
        );
        assert_eq!(items.len(), 4);
        assert_eq!(
            items[3].text_content(),
            "file contents",
            "no synthetic duplicate was spliced ahead of the real result"
        );
    }

    /// M-D17: every window/cut decision in the compaction + rewind stack must be
    /// pair-atomic. The snap only ever moves DOWN, to the group's first item, so
    /// a cut can never land between a call and its output: rewind drops both
    /// halves, a retained window keeps both.
    #[test]
    fn snap_index_over_discovery_pairs_never_splits_a_group() {
        let items = history_with_discovery_pair();
        for (cut, expected) in [(0, 0), (1, 1), (2, 2), (3, 2), (4, 4), (5, 5)] {
            assert_eq!(
                snap_index_over_discovery_pairs(&items, cut),
                expected,
                "cut {cut} (3 would keep the call without its output)"
            );
        }

        // A provider-minted pair carries `call_id: null`, so it forms no key
        // group; adjacency is the only grouping signal available and the snap
        // still refuses to split it.
        let server_pair = sol_hosted_server_pair();
        let items = vec![
            ConversationItem::user("q"),
            disc(server_pair[0].clone()),
            disc(server_pair[1].clone()),
            ConversationItem::assistant("a"),
        ];
        assert_eq!(snap_index_over_discovery_pairs(&items, 2), 1);
        assert_eq!(snap_index_over_discovery_pairs(&items, 3), 3);

        // A cut past the whole pair does not move: the fixture's pair is one keyed
        // group at [2, 3] and cut 4 is above its last member.
        assert_eq!(snap_index_over_discovery_pairs(&history_with_discovery_pair(), 4), 4);
    }

    /// Cut review F-2 in its hardest quadrant: the keyed rule groups by `call_id`
    /// across the WHOLE history, so a pair whose output precedes its partner call
    /// (`[out(A), call(B), out(B), call(A)]`) is the group `[0, 3]` — a group whose
    /// FIRST member is an output. A snap that walks the group list once, mutating as
    /// it goes, is order-dependent and can stop on a value that still splits `[0, 3]`,
    /// retaining `out(A)` with no `call(A)` (the provider-desync half of the shape).
    /// The snap must therefore be a fixed point over ALL groups, with an explicit
    /// bound rather than an unbounded `loop`.
    #[test]
    fn snap_index_over_discovery_pairs_survives_any_pair_rotation() {
        let rotated = vec![
            disc_item(keyed_output("call_A", None)),
            disc_item(keyed_call("call_B", None)),
            disc_item(keyed_output("call_B", None)),
            disc_item(keyed_call("call_A", None)),
        ];
        // The review's falsifier, by value: cut 3 must leave the whole history.
        assert_eq!(
            snap_index_over_discovery_pairs(&rotated, 3),
            0,
            "cut 3 splits BOTH [0,3] and [1,2]; the snap must fall to the outermost \
             first member, not stop at 1 and hand a retained window `out(A)` alone"
        );
        assert_eq!(snap_index_over_discovery_pairs(&rotated, 1), 0);
        assert_eq!(snap_index_over_discovery_pairs(&rotated, 2), 0);
        // A cut that splits nothing must not move, in either direction.
        assert_eq!(snap_index_over_discovery_pairs(&rotated, 0), 0);
        assert_eq!(
            snap_index_over_discovery_pairs(&rotated, rotated.len()),
            rotated.len()
        );

        // The same rule as a property, over every rotation of the shape and every
        // cut: never move UP, never land strictly inside any group, and be a no-op
        // where the cut was already pair-atomic.
        for shift in 0..rotated.len() {
            let shape: Vec<ConversationItem> = (0..rotated.len())
                .map(|i| rotated[(i + shift) % rotated.len()].clone())
                .collect();
            let groups = discovery_groups(&shape);
            for cut in 0..=shape.len() {
                let snapped = snap_index_over_discovery_pairs(&shape, cut);
                assert!(snapped <= cut, "rotation {shift}: cut {cut} moved UP to {snapped}");
                for group in &groups {
                    let first = group.iter().min().copied().unwrap_or(0);
                    let last = group.iter().max().copied().unwrap_or(0);
                    assert!(
                        !(first < snapped && snapped <= last),
                        "rotation {shift}: cut {cut} snapped to {snapped}, which splits \
                         group {group:?} of {shape:?}"
                    );
                }
                let split_by_cut = groups.iter().any(|group| {
                    let first = group.iter().min().copied().unwrap_or(0);
                    let last = group.iter().max().copied().unwrap_or(0);
                    first < cut && cut <= last
                });
                if !split_by_cut {
                    assert_eq!(
                        snapped, cut,
                        "rotation {shift}: cut {cut} splits no group, so the snap must be a no-op"
                    );
                }
            }
        }

        // A same-key run that is NOT adjacent — the donor's reused-`call_id` shape,
        // two calls plus the one answer — is one group spanning the whole window, so
        // every interior cut has to fall out of it entirely.
        let same_key = vec![
            disc_item(keyed_call("call_A", None)),
            ConversationItem::user("injected"),
            disc_item(keyed_call("call_A", None)),
            disc_item(keyed_output("call_A", None)),
        ];
        for cut in 1..same_key.len() {
            assert_eq!(
                snap_index_over_discovery_pairs(&same_key, cut),
                0,
                "cut {cut} splits the same-key group [0, 2, 3]"
            );
        }
    }

    /// Every test in this module builds the variant through
    /// [`ToolSearchItem::from_wire`]; this is the history-shaped constructor.
    fn disc_item(item: ToolSearchItem) -> ConversationItem {
        ConversationItem::Discovery { item }
    }

    /// Cut review F-1, at the helper level: the provider mints its hosted pair with
    /// `call_id: null`, and a synthetic `User` row can land between the halves. Order
    /// is then the ONLY grouping signal, and requiring the two halves to be physically
    /// adjacent reads that pair as two unrelated lone items — which is how a compaction
    /// window came to delete a complete pair out of the only verbatim content a
    /// compacted history keeps.
    #[test]
    fn a_keyless_pair_is_one_group_even_when_its_halves_are_not_adjacent() {
        let server_pair = sol_hosted_server_pair();
        let items = vec![
            disc(server_pair[0].clone()),
            ConversationItem::user("injected between the halves"),
            disc(server_pair[1].clone()),
        ];
        assert!(
            unpaired_discovery_indices(&items).is_empty(),
            "a null-key call followed (anywhere later) by a null-key output is ONE closed \
             group; calling it two lone halves is the false-lone strip: {items:?}"
        );
        assert_eq!(snap_index_over_discovery_pairs(&items, 1), 0);
        assert_eq!(
            snap_index_over_discovery_pairs(&items, 2),
            0,
            "a cut between the two halves must move below the call"
        );
        assert_eq!(snap_index_over_discovery_pairs(&items, 3), 3);
    }

    /// Cut review F-2: groups are visited in ascending-first order, so a single pass
    /// lets a LATER group lower `snapped` past an EARLIER one that was already tested
    /// and return an index that splits that earlier group. Interleaved parallel
    /// searches are exactly that shape.
    #[test]
    fn snap_index_over_discovery_pairs_loops_until_a_fixpoint_when_pairs_interleave() {
        let items = vec![
            disc_item(keyed_call("call_A", None)),
            disc_item(keyed_call("call_B", None)),
            disc_item(keyed_output("call_A", None)),
            disc_item(keyed_output("call_B", None)),
        ];
        // Groups: A = [0, 2], B = [1, 3]. A one-pass snap at cut 3 moves to 1 (B's
        // first), which is INSIDE group A — the retained window would then hold `outA`
        // with no `callA`, the bare-output desync this helper exists to prevent. The
        // fixpoint answer is 0: nothing of either pair is split.
        for (cut, expected) in [(0, 0), (1, 0), (2, 0), (3, 0), (4, 4)] {
            let snapped = snap_index_over_discovery_pairs(&items, cut);
            assert_eq!(
                snapped, expected,
                "cut {cut} must land at {expected}, below the first member of every pair \
                 it would otherwise split"
            );
            // The general property, not just the literal: no group may straddle the
            // returned index.
            for group in [[0usize, 2], [1, 3]] {
                let straddles = group[0] < snapped && snapped <= group[1];
                assert!(!straddles, "cut {snapped} splits group {group:?}");
            }
        }
    }

    /// A same-key non-adjacent pair (the replayed-window shape the donor's reused
    /// `call_id` note names) groups across the gap, so a cut inside it snaps below the
    /// first half.
    #[test]
    fn a_keyed_pair_groups_across_an_intervening_duplicate_call() {
        let items = vec![
            disc_item(keyed_call("call_X", None)),
            ConversationItem::user("mid-turn injection"),
            disc_item(keyed_call("call_X", None)),
            disc_item(keyed_output("call_X", None)),
        ];
        // One key group [0, 2, 3]; it holds a call and an output, so it is closed and
        // nothing here is a lone half…
        assert!(unpaired_discovery_indices(&items).is_empty());
        // …and no cut between 0 and 4 survives.
        for cut in 1..4 {
            assert_eq!(snap_index_over_discovery_pairs(&items, cut), 0, "cut {cut}");
        }
        // Past the last member the cut splits nothing, so it does not move.
        assert_eq!(snap_index_over_discovery_pairs(&items, 4), 4);
    }

    /// Cut review WAJ21R2-03: the keyed quadrant groups EVERY item carrying the same
    /// `call_id`, with no distance bound, so one colliding key turns ten turns of
    /// unrelated history into a single group. The snap sits on
    /// `conversation_truncate_for_prompt`, `truncate_conversation_at`, `fork_filter_chat`
    /// and the budget fit, and an unbounded lowering there rewinds a rewind across all
    /// ten turns and then PERSISTS the shortened history — a silent whole-turn loss far
    /// larger than the half-pair it was preventing. So the snap honours only groups
    /// within [`MAX_SNAP_GROUP_SPAN`] and leaves the rest to the loud failure.
    #[test]
    fn a_reused_call_id_spanning_whole_turns_does_not_drag_the_cut_back() {
        let mut items = vec![
            ConversationItem::user("turn A"),
            disc_item(keyed_call("call_reused", None)),
        ];
        for turn in 0..10 {
            items.push(ConversationItem::assistant(format!("answer {turn}")));
            items.push(ConversationItem::user(format!("turn {}", turn + 1)));
        }
        items.push(disc_item(keyed_output("call_reused", None)));

        // Fixture sanity: the reused key really is ONE group across the whole window.
        let wide = discovery_groups(&items)
            .into_iter()
            .find(|group| group.len() == 2)
            .expect("the two reused-key rows are one group");
        let span = wide[1] - wide[0] + 1;
        assert!(
            span > MAX_SNAP_GROUP_SPAN,
            "fixture must straddle MORE than the snap window, got span {span}"
        );

        // A cut that would delete only the answer must not delete the ten turns too.
        let tail_cut = items.len() - 1;
        assert_eq!(
            snap_index_over_discovery_pairs(&items, tail_cut),
            tail_cut,
            "an over-wide group must not drag the cut back across {} turns of unrelated \
             history (retained history after the cut: {:?})",
            tail_cut,
            &items[tail_cut..],
        );
        // The trade-off the bound makes, stated as an assertion rather than a comment:
        // the cut stays, so the retained side DOES hold a lone call. That is the loud
        // shape (a strict-backend 400, and H-8's call-side arm flags it on the wire),
        // which this cut prefers over deleting whole turns.
        assert_eq!(
            unpaired_discovery_indices(&items[..tail_cut]),
            vec![1],
            "the refused snap must leave the loud half-pair (the outbound lint's H-8 \
             call-side arm flags exactly this row), not a silent whole-turn loss"
        );

        // The guard is bounded, not disabled: move the answer next to its call and the
        // very same cut snaps, because a real pair spans a handful of rows at most.
        let mut narrow = items.clone();
        let answer = narrow.pop().expect("the answer row");
        narrow.insert(2, answer);
        assert_eq!(
            discovery_groups(&narrow)
                .into_iter()
                .find(|group| group.len() == 2)
                .map(|group| group[1] - group[0] + 1),
            Some(2),
            "fixture sanity: the narrow pair is adjacent"
        );
        assert_eq!(
            snap_index_over_discovery_pairs(&narrow, 2),
            1,
            "a pair inside the window must still snap DOWN below its call"
        );
    }

    /// Cut review F-3: `group.len() >= 2` is not "this pair is answered". Two
    /// `tool_search_call`s sharing a reused `call_id` (PLAN:1248 names reused ids as a
    /// real shape; `same_kind_items_never_pair_even_when_they_share_a_call_id` pins the
    /// pairing rule) answer nothing, and keeping them verbatim is the strict-backend 400
    /// this helper family exists to remove.
    #[test]
    fn unpaired_discovery_indices_requires_one_call_and_one_output() {
        let two_calls = vec![
            disc_item(keyed_call("call_X", None)),
            disc_item(keyed_call("call_X", None)),
        ];
        assert_eq!(
            unpaired_discovery_indices(&two_calls),
            vec![0, 1],
            "two calls sharing a key are NOT a closed group"
        );
        let two_outputs = vec![
            disc_item(keyed_output("call_Y", None)),
            disc_item(keyed_output("call_Y", None)),
        ];
        assert_eq!(
            unpaired_discovery_indices(&two_outputs),
            vec![0, 1],
            "…and neither are two outputs"
        );
        let pair = vec![
            disc_item(keyed_call("call_Z", None)),
            disc_item(keyed_output("call_Z", None)),
        ];
        assert!(unpaired_discovery_indices(&pair).is_empty());
        // The unkeyed quadrant follows the same rule: an output with no open call is
        // its own group, however many outputs sit together.
        let server_pair = sol_hosted_server_pair();
        let orphan_outputs = vec![
            disc(server_pair[1].clone()),
            disc(server_pair[1].clone()),
        ];
        assert_eq!(unpaired_discovery_indices(&orphan_outputs), vec![0, 1]);
    }
}
