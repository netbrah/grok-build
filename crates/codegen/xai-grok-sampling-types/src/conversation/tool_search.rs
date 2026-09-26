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
//! `captures/…` citations are relative to the campaign corpus root
//! `/Users/palanisd/Projects/upstream/grok/plans/harness/hosted-tool-search/`;
//! `fixtures/…` citations are relative to its `ratchet-capture/` subdirectory
//! (that is where the fixture tree actually sits). Both are deliberately
//! OUTSIDE this repository — capture bytes are campaign state, not product code.
//! A cite that does not resolve is stale, not approximate: re-resolve it before
//! repeating the claim. `PLAN:` cites are line numbers in
//! `docs/superpowers/plans/2026-09-25-s3a-responses-native-tool-search.md`,
//! which does live in this tree.
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
//!   (PLAN:1325-1329), and the strip-list that owns that is T15's
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
//!   response stream ever contains that item — `_recon-fixtures.md` §6b, scoped
//!   to the 7 response files under `ratchet-capture/fixtures/`), where donor
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

/// Wire `type` tag of the model-authored discovery request item.
pub const TOOL_SEARCH_CALL_ITEM_TYPE: &str = "tool_search_call";

/// Wire `type` tag of the client-authored discovery result item.
pub const TOOL_SEARCH_OUTPUT_ITEM_TYPE: &str = "tool_search_output";

/// `execution` value of the CLIENT-executed quadrant: the harness runs the
/// search and authors the `tool_search_output` in the FOLLOWING request.
/// Observed on both halves of every client-quadrant fixture
/// (`CX1…/next-turn.json`, `R6-client-loop/next-turn.json`). The Messages wire
/// has no such field.
///
/// This name is a crate-wide constant: `super::responses::ToolSearchExecution::Client`
/// resolves to it, so renaming it breaks the declaration-emission path.
pub const CLIENT_EXECUTION: &str = "client";

/// `execution` value of the SERVER-executed quadrant: the provider runs the
/// search and mints BOTH halves of the pair itself, with `call_id: null`.
/// Observed on 8/8 discovery items across the four hosted-search captures under
/// `captures/2026-09-25-wire-grounding/`. PLAN:947 exempts this quadrant from
/// orphan removal, which is why the two values get their own constant.
///
/// As of this cut the declaration-emission enum does NOT read it —
/// `super::responses::ToolSearchExecution::as_str` returns the literal
/// `"server"` for that arm (`conversation/responses.rs:537`) and only the
/// `Client` arm resolves through a constant. So this is the predicate's value,
/// not yet the emitter's; that asymmetry is on the `apex-waj.3` lane.
pub const SERVER_EXECUTION: &str = "server";

/// How much of a model-authored query [`ToolSearchItem::text_summary`] echoes.
pub const MAX_SUMMARY_QUERY_BYTES: usize = 200;

/// The keys a replayed **`tool_search_call`** may carry, as observed on the wire
/// (PLAN:1325-1329): replaying the echoed call with the provider's extra fields
/// returns `400 Unknown parameter: 'input[1].created_by'`.
///
/// **CALL HALF ONLY — this is not an output allow-list.** The observation is of
/// a `tool_search_call`, and a `tool_search_output` carries a key that is absent
/// from this set and demonstrably replayable: `tools` is the field the loaded-tool
/// set lives in (A-14, PLAN:1532-1533) and every accepted replayed output in the
/// corpus has it (`fixtures/codex/CX1-toolsearch-mcp-dryrun/next-turn.json`
/// `input[4]`, `CX3-toolsearch-5.5-LIVE/next-turn.json` `input[12]`,
/// `fixtures/grok-probe/R6-client-loop/next-turn.json` `input[2]`). `error`
/// (PLAN:22's D-ERR text) is likewise outside the observed set and is sent on real
/// outputs. A pass that applied this set to both halves would strip the
/// loaded-tool set out of history — the exact A-14/A-26 failure. The output half's
/// allow-list has never been observed and must not be guessed from this one.
///
/// Deciding *what to strip and where* belongs to the pairing/repair path — the
/// plan says so explicitly, "T15's strip-list" (PLAN:1421). This constant exists
/// so that path does not have to re-derive the boundary's parameter allow-list
/// from a 400, and so the observation is not lost when this module's author stops
/// paying attention. It is a key set, not a predicate: nothing here mutates
/// [`ToolSearchItem::raw`].
pub const CALL_REPLAYABLE_KEYS: &[&str] =
    &["arguments", "call_id", "execution", "id", "status", "type"];

/// Which half of the discovery pair an item is.
///
/// Stored alongside the raw item inside [`ToolSearchItem`] so a projector can
/// branch without re-reading the JSON tag. It is not settable: `from_wire` reads
/// it from `raw`'s `type` tag and is the only constructor, so the two cannot be
/// made to disagree and a malformed pair is refused at construction rather than
/// discovered at request-build time.
///
/// No serde derives, on purpose: the derived vocabulary is `"call"`/`"output"`,
/// which is NOT the wire tag (`from_value::<ToolSearchKind>(json!("tool_search_call"))`
/// fails — measured). [`Self::item_type`] / [`Self::from_item_type`] are the wire
/// mapping, and a second spelling of the same fact is the fourth vocabulary A-24
/// warns about. Nothing in the crate serializes a kind: [`ToolSearchItem`]'s
/// `Serialize` writes `raw`, which carries the real `type` tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSearchKind {
    /// `tool_search_call` — the model asked for tools; the harness answers.
    Call,
    /// `tool_search_output` — the definitions the harness loaded.
    Output,
}

impl ToolSearchKind {
    /// The wire `type` tag this kind serializes to.
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

/// Lifecycle state of a discovery item.
///
/// The streamed duplication is the reason this needs a type rather than a
/// string check: a Responses stream delivers the SAME item twice —
/// `output_item.added` then `output_item.done`
/// (`fixtures/codex/CX3-toolsearch-5.5-LIVE/response.sse`, both copies at
/// `output_index: 1`). **Gate on this status, never on frame order**: the only
/// genuine provider stream in the corpus (CX3, `response_provenance: "GENUINE
/// provider bytes, live"`) emits `added` as a skeleton — `"in_progress"` with
/// `arguments: {}` — and fills the values in `done`. A second shape exists in
/// the corpus, `CX1-toolsearch-mcp-dryrun/response.sse`, whose `added` copy is
/// ALREADY `completed` with real arguments, but that file's own meta.json marks
/// it `response_provenance: "MOCK (codex-arm/mock_upstream.py) -- response-
/// derived assertions are NOT donor truth"`: it is what our mock upstream
/// emits, not what the provider emits. It is still the reason not to key on
/// frame order — a harness-side consumer has to survive both — but it must not
/// be cited as provider behaviour. "Take the first frame" fingerprints an empty
/// item on the live capture and works on the mock: exactly the A-22 defect class.
///
/// The two terminal strings are the wire's, not this enum's invention:
/// `"completed"` and `"error"` are what the D-ERR channel emits
/// (PLAN:22, and the plan's own types spell the field
/// `pub status: String /* "completed" | "error" */` at PLAN:222;
/// `SearchStatus { Completed, Error}` at PLAN:599; the A2 lint pins
/// `status ∈ {completed, error}` at PLAN:1035).
///
/// `Deserialize` is derived with `#[serde(other)]` so a consumer that embeds this
/// enum in its own struct cannot fail-to-load an item it is obliged to replay.
/// MEASURED scope of that promise, both branches pinned by
/// `an_unknown_status_degrades_the_view_not_the_bytes`: an unrecognised **string**
/// (`"queued"`) loads as [`Self::Unknown`]; a `status` that is not a string at all
/// (e.g. `null`) is a serde type error at the consumer's site — unlike
/// [`Self::from_wire`], which reads an absent or non-string value as
/// [`Self::Unknown`] because it goes through `Value::as_str`. A consumer embedding
/// the enum beside bytes it must replay therefore wants `from_wire`, not the
/// derived impl.
///
/// No `Serialize`: this type is a VIEW, and the derived impl would emit
/// `"unknown"` — a status string the wire has never sent (measured). The bytes
/// that go back out are [`ToolSearchItem::raw`]'s, which keep the provider's own
/// spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSearchStatus {
    /// Streamed skeleton: present but not yet final.
    InProgress,
    /// Final copy. The only state whose definitions may be loaded.
    Completed,
    /// Terminal failure of the search (`status: "error"`, `tools: []` plus an
    /// `error` text field — PLAN:22). It still ANSWERS its call: see
    /// [`ToolSearchItem::is_pairable`].
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
            Some("in_progress") => Self::InProgress,
            Some("completed") => Self::Completed,
            Some("error") => Self::Error,
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
    /// Carries the offending value so a caller can log without re-reading.
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
    /// [`Self::from_wire`], the only constructor.
    kind: ToolSearchKind,
    /// Exact provider item, as received. This is what the STORE keeps.
    ///
    /// It is **not** automatically what goes back on the wire: the echoed
    /// `tool_search_call` carries `created_by`, and replaying it verbatim returns
    /// `400 Unknown parameter: 'input[1].created_by'` (PLAN:1325-1329, observed on
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

    /// The item's own id (`tsc_*` / `tso_*`), verbatim — never synthesized.
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
    /// it" are BOTH false as absolutes — each is a quadrant fact. (`_recon-fixtures.md`
    /// §6b's census is correct and correctly scoped to its 7 response files; it
    /// was the `captures/` tree that was outside its census.)
    ///
    /// The rule here is the same in every quadrant: copy what is there, mint
    /// nothing. An id is a handle (wire invariant 6) and an invented one that
    /// collides with a real mint is worse than an absent one. When we author the
    /// item we take policy 3 — the omitting side our own scored window proves the
    /// boundary accepts — rather than defaulting to the donor's mint.
    pub fn id(&self) -> Option<&str> {
        self.raw.get("id").and_then(Value::as_str)
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
    /// lost — the A-22 audit found exactly that in every streamed-item extractor
    /// of this campaign.
    pub fn is_completed(&self) -> bool {
        self.status() == ToolSearchStatus::Completed
    }

    /// Whether this copy may take part in pairing at all.
    ///
    /// Deliberately NOT [`Self::is_completed`], and deliberately different on
    /// the two halves. Strictness follows what the half can DO:
    ///
    /// - the output must positively claim a TERMINAL state — `completed` (its
    ///   definitions may load) or `error` (it carries none, but it DID answer).
    ///   `status` is REQUIRED on the output (PLAN:1247), so an absent or
    ///   unmodelled status cannot stand in for an answer. Treating `error` as an
    ///   answer is not a courtesy: a repair pass that sees `UnansweredCall` here
    ///   synthesises a SECOND output for a search that already answered.
    /// - the call is only a request, and the donor contract makes `status`
    ///   OPTIONAL there (PLAN:1248). Absence is first-party-normal, so a call
    ///   leaves pairing on the one positive claim of non-finality the wire
    ///   makes — the `in_progress` stream skeleton.
    fn is_pairable(&self) -> bool {
        match self.kind {
            ToolSearchKind::Output => {
                matches!(
                    self.status(),
                    ToolSearchStatus::Completed | ToolSearchStatus::Error
                )
            }
            ToolSearchKind::Call => self.status() != ToolSearchStatus::InProgress,
        }
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
    /// must read [`Self::execution`] / [`Self::is_server_executed`] rather than
    /// negate this.
    pub fn is_client_executed(&self) -> bool {
        self.execution() == Some(CLIENT_EXECUTION)
    }

    /// Exactly: the field is present and says `"server"` — the provider-executed
    /// quadrant, whose outputs the plan exempts from orphan removal (PLAN:947).
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
    pub fn query(&self) -> Option<&str> {
        self.arguments()?.get("query").and_then(Value::as_str)
    }

    /// The `limit` of a call, if present and integral.
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
    /// 'input[1].created_by'` (PLAN:1325-1329), so the splice path owes a strip
    /// pass — see [`CALL_REPLAYABLE_KEYS`] for the observed call-half set and
    /// PLAN:1421 for why that pass is T15's, not this type's.
    pub fn raw(&self) -> &Value {
        &self.raw
    }

    /// One bounded, human-readable line for the pager and for text extraction.
    ///
    /// The query is echoed up to [`MAX_SUMMARY_QUERY_BYTES`] and then truncated,
    /// because a `tool_search` query is model-authored and this line is what a
    /// projector has available when it needs a summary rather than the item —
    /// an unbounded echo there is prefix churn on a cached prompt (§6.7 of the
    /// worktree rules). Never a source of truth: the raw item stays intact for
    /// replay regardless of what this returns.
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
/// [`ToolSearchItem::raw`], so an unrecognized field (`strict`,
/// `defer_loading`, a future `allowed_callers`) cannot be dropped between the
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
    /// declaration type on this deployment (PLAN:1899-1902), so "unmodelled" is
    /// not the same as "impossible".
    Other,
}

impl<'a> DiscoveredTool<'a> {
    /// Classify a provider entry by its `type` tag. Private: the only producer is
    /// [`ToolSearchItem::tools`], so `kind` can never be made to disagree with the
    /// bytes it claims to describe.
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

    /// The entry's `name` as the provider wrote it. For a namespace this is
    /// the GROUP name, which is not invocable — see
    /// [`Self::callable_definitions`].
    pub fn name(&self) -> Option<&'a str> {
        self.raw.get("name").and_then(Value::as_str)
    }

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
    /// KNOWN SEAM, named rather than papered over: a namespace child whose
    /// `type` is not `function` disappears from the loaded set here without the
    /// `Other` marker its top-level twin gets. Nothing in the corpus has emitted
    /// one (across `fixtures/` + `captures/`, namespace children are `function`
    /// 11/11), but `custom` IS a live declaration type on this deployment
    /// (PLAN:1899-1902: `function` x10, `custom` x1, `tool_search` x1), so the
    /// first capture showing a non-function child inside a group lands here. This is the seam the Messages-wire arm needs:
    /// that wire requires every referenced tool to be DECLARED (A-23) and a
    /// namespace group is not a declaration of its children.
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
/// "does a counterpart with this key exist on the required side?" and NOT "did
/// the 1:1 pass hand me a partner" — see [`Self::CounterpartPresent`] for the
/// difference, which is the difference between leaving history alone and deleting
/// the loaded-tool set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSearchPairing {
    /// A call paired 1:1 with the output that follows it under the same `call_id`.
    Paired,
    /// A final copy with no 1:1 partner, while a counterpart of the other half
    /// carrying the same `call_id` IS present where the wire requires it.
    ///
    /// **Not a defect, and not repairable here.** Three shapes reach it, all of
    /// them provider-legal: a reused `call_id` (PLAN:1248 names reuse as a
    /// first-party reason for the field being optional), a search answered twice
    /// (the second output has no call of its own to match, but the key IS in the
    /// call set, so PLAN:947 does not remove it), and a pair whose other half is
    /// only the stream skeleton. A pass that routed this with
    /// [`Self::OrphanOutput`] would delete real definitions; a pass that routed
    /// it with [`Self::UnansweredCall`] would synthesise a second answer for a
    /// search that already got one.
    CounterpartPresent,
    /// A keyed call with **no** output under its `call_id` anywhere after it
    /// (search still in flight, or the turn was cancelled between the two).
    UnansweredCall,
    /// A **keyed** output with **no** call under its `call_id` anywhere before
    /// it. This is the defect class, and it is keyed: probe R5 sent a client
    /// `tool_search_output` whose call had vanished and the boundary answered
    /// `400 invalid_request_error` — "No tool call found for tool search output
    /// with call_id …" (PLAN:1161-1162). The error names a `call_id`, so it
    /// cannot fire on an item that has none; see [`Self::Unkeyed`].
    OrphanOutput,
    /// A final copy carrying no `call_id`, so no join is possible in principle.
    ///
    /// This is a fact about the KEY, not about the quadrant — read
    /// [`ToolSearchItem::is_server_executed`] before deciding anything. The two
    /// shapes it covers are opposite in consequence:
    ///
    /// * **server-executed** (the observed case): the provider mints the pair
    ///   itself with `"call_id": null` on both halves — four live hosted-search
    ///   captures carry it
    ///   (`captures/2026-09-25-wire-grounding/wire_resp_20260925T062640Z_R1_SOL_HOSTED.json`
    ///   `output[1]` + `output[2]`, and the same in R2_TERRA, R2_LUNA and
    ///   R1_SOL_ts). PLAN:947 exempts this quadrant from orphan removal outright,
    ///   so it must never be dropped: that is the A-26 failure mode.
    /// * **client-executed with no key**: not exempt. PLAN:1035's lint requires a
    ///   non-empty `call_id` on outputs, so this is a lint-flagged shape, and
    ///   whether the boundary accepts such a request at all is UNPROBED — no
    ///   capture in the corpus carries a keyless client output. Do not treat it
    ///   as safe to replay, and do not treat it as the PLAN:1161 400 either.
    Unkeyed,
    /// Not a copy pairing accepts: the `in_progress` stream skeleton, or an
    /// output that never claimed a terminal state. Not conversation state yet,
    /// and never replayable on its own: the skeleton carries `arguments: {}` and
    /// no definitions, so persisting it silently loses the discovery (A-22 found
    /// this in every streamed-item extractor of this campaign).
    Incomplete,
}

/// Partner index for each discovery item, pair-atomic by construction.
///
/// `call_id` is the only join key: the two halves carry different item ids and
/// the output's id is optional on the wire (`R6-client-loop/next-turn.json`
/// `input[2]` has none), so nothing else can pair them. Each call is paired
/// with the nearest UNCLAIMED output AFTER it sharing its `call_id`, so a caller
/// that filters history can drop or keep both halves together — the
/// pairing-survival invariant (wire invariant 5) is pair-ATOMIC, and keeping
/// one half alone produces exactly the orphan shape the provider 400s on.
///
/// The join is ORDERED because the wire's is: probe R5 sent a client
/// `tool_search_output` whose call did not precede it and the boundary answered
/// `400 invalid_request_error` "No tool call found for tool search output with
/// call_id …" (PLAN:1161-1162), and the repair rule is that the matching output
/// "must follow" (PLAN:946). An output placed BEFORE its call is therefore not a
/// pair; calling it `Paired` is how a repair pass ships that 400.
///
/// A partner is claimed once: the result is symmetric by construction
/// (`partners[partners[i]] == Some(i)` for every `Some(i)`). Reused `call_id`s
/// are a named first-party shape (PLAN:1248 says `call_id` is optional partly
/// to accommodate "codex-reused ids"), so without that rule two calls would
/// both claim one output and the map would contradict itself.
///
/// Only final copies take part — see `ToolSearchItem::is_pairable`, which is
/// private to this module and whose rules the verdicts above state. Without that
/// rule a `call_id`-matched skeleton would consume the real output as its
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

/// Pairing verdict for every discovery item, in input order.
///
/// Input is the discovery items only, in conversation order; the indices in
/// [`partner_indices`] and in the return of this function are **slice**
/// coordinates, not conversation coordinates — a projector that keeps or drops
/// items in the full conversation must carry its own mapping from this slice
/// back to those positions, or it will act on the wrong item.
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
/// removal applies to `execution != "server"` only, so a pass must also read
/// [`ToolSearchItem::is_server_executed`]. [`ToolSearchPairing::Unkeyed`] says
/// nothing about the quadrant.
pub fn pairing_of(items: &[ToolSearchItem]) -> Vec<ToolSearchPairing> {
    let partners = partner_indices(items);
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            if !item.is_pairable() {
                return ToolSearchPairing::Incomplete;
            }
            if partners[i].is_some() {
                return ToolSearchPairing::Paired;
            }
            let Some(key) = item.call_id() else {
                return ToolSearchPairing::Unkeyed;
            };
            if counterpart_present(items, i, key) {
                return ToolSearchPairing::CounterpartPresent;
            }
            match item.kind {
                ToolSearchKind::Call => ToolSearchPairing::UnansweredCall,
                ToolSearchKind::Output => ToolSearchPairing::OrphanOutput,
            }
        })
        .collect()
}

/// Whether any item of the other half carries `key` on the side the wire
/// requires: a call's answer must FOLLOW it, an output's call must PRECEDE it
/// (PLAN:946, and probe R5's 400 for the inverted order — PLAN:1161-1162).
///
/// State-blind on purpose. The repair rules ask "is the key in the set?", and
/// the state-blind reading is the conservative one: an output whose only call is
/// the stream skeleton, and a call whose only output has not reached a terminal
/// state, both have a counterpart present, so neither invites a synthetic second
/// answer or a deletion.
fn counterpart_present(items: &[ToolSearchItem], index: usize, key: &str) -> bool {
    let required_kind = match items[index].kind {
        ToolSearchKind::Call => ToolSearchKind::Output,
        ToolSearchKind::Output => ToolSearchKind::Call,
    };
    let side: &[ToolSearchItem] = match items[index].kind {
        ToolSearchKind::Call => &items[index + 1..],
        ToolSearchKind::Output => &items[..index],
    };
    side.iter()
        .any(|other| other.kind == required_kind && other.call_id() == Some(key))
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
    /// The namespace group name this definition arrived under, or `None` for a
    /// definition the provider returned flat.
    namespace: Option<&'a str>,
}

impl<'a> LoadedDefinition<'a> {
    /// The definition's own `name`.
    pub fn name(&self) -> Option<&'a str> {
        self.definition.get("name").and_then(Value::as_str)
    }

    /// The exact definition JSON, as the provider returned it.
    pub fn definition(&self) -> &'a Value {
        self.definition
    }

    /// The namespace group this definition was discovered under, or `None` when it
    /// came back flat. Both forms are needed to invoke a child (A-16).
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
/// qualifier. The completion rule is A-22's (PLAN:1753-1759, the loader that
/// fingerprinted the IN-PROGRESS item and scored an empty arguments object). The
/// distinction matters downstream because A-14 is the amendment cited as the
/// reason a *set* may be materialised onto the Messages wire.
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
/// [`ToolSearchItem::tools`], which gates on kind too — measured, not assumed:
/// deleting the check here changes no observable behaviour (battery mutant M23
/// SURVIVED, because `tools()` has already returned an empty Vec), while the
/// accessor's own gate is pinned by `a_call_never_contributes_definitions_…`
/// (mutant M3 KILLED). It stays because this is the primitive other lanes compose
/// against, and it must not inherit its safety from another function's internals.
///
/// De-duplicated on `(definition bytes, namespace)`, so one definition surfaced
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
/// FLAT name (`mcp__codegraph__codegraph_status`) — A-16, both sides captured
/// live (CX3/S5 and CC2/S3). This returns `(short, namespace)` and deliberately
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
/// namespaces is two entries (the set deduplicates on `(definition bytes,
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

    /// `CX1-toolsearch-mcp-dryrun/next-turn.json` `input[4]` — a namespace
    /// result, trimmed to three children, plus the donor's passthrough field
    /// (an unknown field that must survive).
    /// STRUCTURALLY like `CX1-toolsearch-mcp-dryrun/next-turn.json` `input[4]` —
    /// same item id, `call_id`, namespace group name, child names and passthrough
    /// — but the children are ABBREVIATED (the capture's carry long
    /// `description` strings and a second parameter). Do not read this as the
    /// capture's bytes: it is a fixture with that item's identity, and nothing
    /// here asserts byte-equality against CX1. The `create_time` float and the
    /// unknown passthrough field ARE the capture's, because a float is exactly
    /// the value an integer-typed view would silently eat.
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
    /// `output[2].tools[0]` is reduced to `name` + `type` — the capture's child
    /// also carries `parameters`, `strict`, `allowed_callers`, `defer_loading`,
    /// `description` and `output_schema`, none of which this module reads.
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

    /// `grok-probe/R6-client-loop/next-turn.json` `input[2]` — the
    /// client-authored result: flat definition form, and NO `id`.
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
        assert_eq!(call.tools().len(), 0, "a call carries no results");
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
    fn output_id_is_optional_and_never_synthesized() {
        assert_eq!(item(r6_flat_output()).id(), None);
        assert_eq!(
            item(cx1_namespaced_output()).id(),
            Some("tso_01a0d978-6771-7420-8b21-567a1f96b61c")
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
    /// the group's bytes stay intact for replay. No capture has emitted one yet
    /// (namespace children are `function` 11/11 across `fixtures/` +
    /// `captures/`), so this is a ratchet rather than an observation — the first
    /// real `custom` child must change this test on purpose.
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
    /// (PLAN:1753-1759 — the loader that fingerprinted the IN-PROGRESS item and
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

    /// The join is ORDERED, because the provider's is. A `tool_search_output`
    /// whose call does not precede it is the documented 400 class ("No tool call
    /// found for tool search output with call_id …", PLAN:1161-1162), so it must
    /// never be reported as `Paired` — a repair pass that saw `Paired` here would
    /// ship the request verbatim.
    #[test]
    fn an_output_before_its_call_is_not_a_pair() {
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
                ToolSearchPairing::OrphanOutput,
                ToolSearchPairing::UnansweredCall
            ]
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
        // Still real discovery state: the definitions are loadable and replayable.
        assert_eq!(loaded_tool_set(&items).len(), 1);
        assert_eq!(
            items[1].id().unwrap(),
            "tso_0ce980d5c6afd41f016ab61424031481908982e2c788dcc429"
        );
        // `created_by` stays in the bytes: stripping it is T15's strip-list
        // (PLAN:1421), and this type's job is to not lose it first.
        assert!(items[0].raw().get("created_by").is_some());
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
    /// parameter: 'input[1].created_by'` (PLAN:1325-1329). Stripping it is T15's
    /// job (PLAN:1421) — this only pins that the key set names the hazard.
    ///
    /// Both halves are asserted directly: the old form walked the fixture's keys
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
        // Value equality is key-order-insensitive, so pin the order itself: under
        // `preserve_order` the round-trip must not hand the next projector a
        // re-ordered prefix (cache-break = $, §6.7).
        assert_eq!(
            serde_json::to_string(&reread).unwrap(),
            text,
            "re-serialising the re-read item yields the same bytes"
        );
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
        assert_eq!(
            duplicate.call_id(),
            Some("call_AOphypzlL1KKckJugyBS2PYn"),
            "the duplicate carries the same key, read independently"
        );
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

    /// A call leaves pairing for exactly ONE reason: it is the `in_progress`
    /// stream skeleton. Its `status` is OPTIONAL (PLAN:1247), so an unmodelled
    /// value must not strand a real call — including `"failed"`, which is not
    /// part of this wire's vocabulary at all (the terminal pair is
    /// `completed`/`error`, PLAN:22) and therefore lands in `Unknown`.
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

    /// The donor contract binding on T3 (PLAN:1246-1252) makes `call_id` an
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
    }

    /// `status` is REQUIRED on the output but OPTIONAL on the call
    /// (PLAN:1247). Absence is first-party-normal on the call, so it must not
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
        // `UnansweredCall`: the key is in the call set, so PLAN:947 does not
        // remove the item and PLAN:948 does not add another answer. Neither
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
    /// accessor for (the donor's passthrough, `created_by`) must come back
    /// unchanged. This asserts the STORE only — it is deliberately not a claim
    /// that these bytes are wire-ready: the echoed call's `created_by` 400s on
    /// replay (PLAN:1325-1329) and the strip-list that removes it is T15's
    /// (PLAN:1421).
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
        assert_eq!(
            reread.raw()["internal_chat_message_metadata_passthrough"]["turn_id"],
            "01a0d978-6741-7721-aa1d-aae236e4ed3e"
        );
        assert_eq!(
            reread.raw()["internal_chat_message_metadata_passthrough"]["create_time"],
            1790354941.809797,
            "a float survive an integer-typed view — nothing re-serialises these bytes"
        );
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
        assert_eq!(reread.raw(), &raw);
        assert_eq!(
            reread.raw()["created_by"],
            json!("user:palanisd"),
            "the field T15 has to strip is still present after a store round-trip"
        );
    }

    /// The two reach claims on the derived `Deserialize`, measured rather than
    /// asserted from the serde docs: an unmodelled status STRING is tolerated
    /// (`#[serde(other)]`), a non-string `status` is a hard type error — which
    /// is NOT what [`ToolSearchItem::from_wire`] does with the same value, so
    /// the two paths must not be described as equivalent.
    #[test]
    fn the_status_view_tolerates_an_unknown_string_and_rejects_a_non_string() {
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
        assert!(grouped.text_summary().len() < 80, "summary stays bounded");
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

    /// The summary is what a pager or a text-extraction arm gets, and the query
    /// inside it is model-authored, so the echo is capped (§6.7: bounded,
    /// stable model-visible fragments).
    #[test]
    fn the_query_echo_in_the_summary_is_capped() {
        let mut long = cx3_call();
        long["arguments"]["query"] = json!("é ".repeat(400));
        let long = item(long);
        let summary = long.text_summary();
        assert!(summary.starts_with("[tool_search] \"é"), "{summary}");
        assert!(
            summary.len() < MAX_SUMMARY_QUERY_BYTES + 40,
            "the cap holds, got {} bytes",
            summary.len()
        );
        assert!(summary.ends_with('…'), "and it is visibly truncated");
        // A cap that never fires is an untested cap: below the limit there is no
        // ellipsis and no truncation.
        let short = item(cx3_call());
        assert!(!short.text_summary().ends_with('…'));

        // The `(no query)` arm is the stream skeleton, the one shape that always
        // reaches it.
        assert_eq!(
            item(cx3_skeleton_call()).text_summary(),
            "[tool_search] (no query)"
        );
    }

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
    }

    #[test]
    fn kind_and_wire_tag_agree_in_both_directions() {
        for kind in [ToolSearchKind::Call, ToolSearchKind::Output] {
            assert_eq!(ToolSearchKind::from_item_type(kind.item_type()), Some(kind));
        }
        assert_eq!(ToolSearchKind::from_item_type("function_call"), None);
        assert_eq!(ToolSearchKind::from_item_type("reasoning"), None);
    }

    #[test]
    fn duplicate_definitions_across_searches_load_once() {
        let first = item(r6_flat_output());
        let second = item(r6_flat_output());
        assert_eq!(loaded_tool_set([&first, &second]).len(), 1);
    }

    /// The dedup key is `(definition bytes, namespace)`, and the namespace half
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
    /// stays `CounterpartPresent`, and a call must never reach past an output that
    /// already belongs to an earlier call.
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
        // c1 has two candidates (o1 then o2); c2 is keyed differently and its
        // only candidate sits AFTER o2, so a greedy "scan on" implementation
        // would hand c2 the wrong item or let c1 skip o1.
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
}
