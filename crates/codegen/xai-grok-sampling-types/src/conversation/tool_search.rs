//! Native tool-discovery representation (A-24, apex-waj.1).
//!
//! A-24: `ConversationItem` had six variants and none of them can hold a
//! `tool_search_call` / `tool_search_output` item, so discovery state had no
//! home in the conversation IR. On a Responses->Responses model switch — the
//! seam where family-switch compaction is deliberately disabled
//! (`model_switch.rs:464-468`) and the switch projector is the only actor —
//! state the IR cannot represent is simply dropped, while A-19/A-23 forbid
//! re-declaring those definitions in `tools[]`. The model then loses every
//! discovered tool silently. This module is that missing home.
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
//! * [`ToolSearchItem::raw`] is the single source of truth and the ONLY thing
//!   replayed on the wire. Nothing mutates it.
//! * Every typed accessor (status, arguments, definitions, names) is derived
//!   from `raw` on demand, so the typed view and the replayed bytes cannot
//!   drift apart. A typed view that round-trips lossily (e.g. an unknown
//!   `status` string collapsing to [`ToolSearchStatus::Unknown`]) therefore
//!   cannot corrupt a request.
//! * Opaque ids are handles, not content (wire invariant 6): `tsc_*` /
//!   `tso_*` are copied verbatim and never rewritten, and the harness does not
//!   mint one when the provider did not (the client-authored output in
//!   `fixtures/grok-probe/R6-client-loop/next-turn.json` carries no `id` at
//!   all, while provider-minted ones do).
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

use std::borrow::Cow;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

/// Wire `type` tag of the model-authored discovery request item.
pub const TOOL_SEARCH_CALL_ITEM_TYPE: &str = "tool_search_call";

/// Wire `type` tag of the client-authored discovery result item.
pub const TOOL_SEARCH_OUTPUT_ITEM_TYPE: &str = "tool_search_output";

/// `execution` value both discovery items carry on the Responses wire.
/// Observed on every retained item, on both the provider-minted call and the
/// client-authored output; the Messages wire has no such field.
pub const CLIENT_EXECUTION: &str = "client";

/// Which half of the discovery pair an item is.
///
/// Stored alongside [`ToolSearchItem::raw`] so a projector can branch without
/// re-reading the JSON tag, and so a malformed pair is rejected at
/// construction rather than at request-build time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
/// `output_item.added` with `status: "in_progress"` and `arguments: {}`, then
/// `output_item.done` with the real values (`fixtures/codex/CX3-toolsearch-5.5-LIVE/response.sse`,
/// both copies at `output_index: 1`). Selecting the first match fingerprints
/// the skeleton. Only the COMPLETED copy is conversation state; the skeleton
/// must never be persisted or replayed, so [`ToolSearchItem::is_completed`]
/// is the gate every consumer must pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSearchStatus {
    /// Streamed skeleton: present but not yet final.
    InProgress,
    /// Final copy. The only state that may enter the conversation.
    Completed,
    /// Terminal failure. Retained verbatim in `raw` for replay.
    Failed,
    /// Written by a newer provider than this enum knows.
    ///
    /// View-only: it never rewrites `raw`, so the literal string still goes
    /// back out on replay.
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
            Some("failed") => Self::Failed,
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
    /// No usable `call_id`. This is the ONLY join key between a call and its
    /// output (the item `id` differs per half and is optional on the output),
    /// so an item without it cannot be paired, projected, or replayed safely.
    MissingCallId,
    /// A `tool_search_call` whose `arguments` is present but not a JSON
    /// object. Kept as a distinct error because it is the classic confusion
    /// with `function_call.arguments`, which IS a JSON string.
    ArgumentsNotAnObject { actual: &'static str },
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
            Self::MissingCallId => write!(
                f,
                "discovery item has no call_id, so it cannot be paired with its counterpart"
            ),
            Self::ArgumentsNotAnObject { actual } => write!(
                f,
                "tool_search_call.arguments must be a JSON object, found {actual} \
                 (function_call.arguments is a string; this one is not)"
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSearchItem {
    /// Which half of the pair this is. Derived from `raw`'s `type` tag at
    /// construction and validated there, so it cannot disagree with the bytes.
    pub kind: ToolSearchKind,
    /// Exact provider item. This is what gets replayed.
    pub raw: Value,
}

impl ToolSearchItem {
    /// Validate and wrap a provider/stream item.
    ///
    /// Rejects non-discovery items, items with no `call_id` (the only join key
    /// between the halves), and a `tool_search_call` whose `arguments` is
    /// present but not an object. Accepts any `status`, including
    /// [`ToolSearchStatus::InProgress`]: the stream skeleton is a legitimate
    /// thing to hand to the consumer that has to decide what to keep, and the
    /// decision surface is [`Self::is_completed`], not this constructor.
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
        if map
            .get("call_id")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            return Err(ToolSearchItemError::MissingCallId);
        }
        if kind == ToolSearchKind::Call
            && let Some(arguments) = map.get("arguments")
            && !arguments.is_object()
        {
            let actual = match arguments {
                Value::String(_) => "a JSON string",
                Value::Null => "null",
                Value::Array(_) => "an array",
                _ => "a non-object value",
            };
            return Err(ToolSearchItemError::ArgumentsNotAnObject { actual });
        }
        Ok(Self { kind, raw })
    }

    /// The item's own id (`tsc_*` / `tso_*`).
    ///
    /// Optional on the wire: the provider-minted copies in
    /// `CX1/CX3/next-turn.json` carry one, the client-authored
    /// `tool_search_output` in `grok-probe/R6-client-loop/next-turn.json`
    /// carries none. Never synthesize a substitute for replay — an id is a
    /// handle (wire invariant 6), and an invented one that collides with a
    /// real mint is worse than an absent one.
    pub fn id(&self) -> Option<&str> {
        self.raw.get("id").and_then(Value::as_str)
    }

    /// The join key between a call and its output.
    ///
    /// Validated non-empty by [`Self::from_wire`].
    pub fn call_id(&self) -> &str {
        self.raw
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    pub fn status(&self) -> ToolSearchStatus {
        ToolSearchStatus::from_wire(self.raw.get("status").and_then(Value::as_str))
    }

    /// Whether this is the COMPLETED copy of the item.
    ///
    /// A Responses stream delivers every item twice — the `added` skeleton
    /// (`status: "in_progress"`, `arguments: {}`) and the `done` copy with the
    /// real values. Only the completed copy is conversation state; a consumer
    /// that persists the skeleton stores an empty query and an empty tool set
    /// and the discovery is silently lost. The A-22 audit found exactly this
    /// in every streamed-item extractor of this campaign.
    pub fn is_completed(&self) -> bool {
        self.status() == ToolSearchStatus::Completed
    }

    /// The `execution` field, verbatim (`Some("client")` on every retained
    /// Responses item). The Messages wire has no such field; do not fabricate
    /// one for it.
    pub fn execution(&self) -> Option<&str> {
        self.raw.get("execution").and_then(Value::as_str)
    }

    pub fn is_client_executed(&self) -> bool {
        self.execution() == Some(CLIENT_EXECUTION)
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
    /// `None` for an output item (which has no `arguments`) and for a call
    /// that omits the field.
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
    /// Empty for a call, and for an output whose `tools` is absent or not an
    /// array.
    pub fn tools(&self) -> Vec<DiscoveredTool<'_>> {
        match self.raw.get("tools") {
            Some(Value::Array(tools)) => tools.iter().map(DiscoveredTool::from_value).collect(),
            _ => Vec::new(),
        }
    }

    /// Every directly callable definition this item loaded, namespace children
    /// included, in wire order and de-duplicated by the bytes that define
    /// them.
    ///
    /// A-14 established that `tool_search_output` items ARE the loaded-tool
    /// set, so this is the set a model switch has to preserve. It is also the
    /// set the two wires treat INVERSELY: Responses must not see any of it in
    /// `tools[]` (A-19/A-23), Messages must see all of it declared (A-23).
    /// Deciding that is the caller's; this only reports what was loaded.
    pub fn loaded_definitions(&self) -> Vec<&Value> {
        let mut seen: Vec<&Value> = Vec::new();
        for tool in self.tools() {
            for definition in tool.callable_definitions() {
                if !seen.contains(&definition) {
                    seen.push(definition);
                }
            }
        }
        seen
    }

    /// The exact JSON to put back on the wire.
    pub fn to_input_value(&self) -> Cow<'_, Value> {
        Cow::Borrowed(&self.raw)
    }

    /// Bounded, human-readable line for the pager and for text extraction.
    ///
    /// Never a source of truth: the raw item stays intact for replay
    /// regardless of what this returns.
    pub fn text_summary(&self) -> String {
        match self.kind {
            ToolSearchKind::Call => match self.query() {
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
                if namespaces > 0 {
                    format!("[tool_search results] {callables} tools in {namespaces} namespace(s)")
                } else {
                    format!("[tool_search results] {callables} tools")
                }
            }
        }
    }

    /// Model-visible length for token accounting.
    ///
    /// The whole payload is model-visible: on Responses the definitions ride
    /// in the history, so the summary above understates the cost by orders of
    /// magnitude.
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
    /// Shape of this entry.
    ///
    /// [`DiscoveredToolKind::Other`] exists so a definition shape this build
    /// does not model is still carried and replayed rather than dropped: on
    /// Responses the `tool_search_output` history IS the loaded-tool set
    /// (A-14), so dropping an entry the provider sent is a silent loss of
    /// model capability.
    pub kind: DiscoveredToolKind,
    /// Exact provider entry.
    pub raw: &'a Value,
}

/// Shape of a [`DiscoveredTool`] entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveredToolKind {
    /// A directly callable definition (`"type":"function"`).
    Function,
    /// A group whose `tools` array holds the callable definitions.
    Namespace,
    /// A shape this build does not model. Retained verbatim.
    Other,
}

impl<'a> DiscoveredTool<'a> {
    /// Classify a provider entry by its `type` tag.
    pub fn from_value(raw: &'a Value) -> Self {
        let kind = match raw.get("type").and_then(Value::as_str) {
            Some("function") => DiscoveredToolKind::Function,
            Some("namespace") => DiscoveredToolKind::Namespace,
            _ => DiscoveredToolKind::Other,
        };
        Self { kind, raw }
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
    /// a shape we do not model. This is the seam the Messages-wire arm needs:
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSearchPairing {
    /// A call paired with an output carrying the same `call_id`.
    Paired,
    /// A call whose output never arrived (search still in flight, or the turn
    /// was cancelled between the two).
    UnansweredCall,
    /// An output with no call. This is what a half-persisted pair looks like
    /// after a crash, and on Responses it is the shape the provider rejects
    /// (the gate's fixture loader names orphan `tool_search_output` as a
    /// documented 400 case).
    OrphanOutput,
    /// Not the completed copy — the `in_progress` stream skeleton, or a
    /// failed item. Not conversation state yet, and never replayable on its
    /// own: the skeleton carries `arguments: {}` and no definitions, so
    /// persisting it silently loses the discovery (A-22 found this in every
    /// streamed-item extractor of this campaign).
    Incomplete,
}

/// Partner index for each discovery item, pair-atomic by construction.
///
/// `call_id` is the only join key: the two halves carry different item ids and
/// the output's id is optional on the wire (`R6-client-loop/next-turn.json`
/// `input[2]` has none), so nothing else can pair them. Each item is paired
/// with the nearest item of the OTHER kind sharing its `call_id`, so a caller
/// that filters history can drop or keep both halves together — the
/// pairing-survival invariant (wire invariant 5) is pair-ATOMIC, and keeping
/// one half alone produces exactly the orphan shape the provider 400s on.
///
/// Only COMPLETED copies take part: a pair is a property of the two completed
/// items, never of a stream skeleton. Without that rule a `call_id`-matched
/// skeleton would consume the real output as its partner and leave the
/// completed call looking unanswered — which is precisely how the A-22
/// extractors went wrong by reading the first copy of a streamed item.
pub fn partner_indices(items: &[ToolSearchItem]) -> Vec<Option<usize>> {
    let mut partners = vec![None; items.len()];
    for (i, left) in items.iter().enumerate() {
        if partners[i].is_some() || !left.is_completed() {
            continue;
        }
        for (j, right) in items.iter().enumerate().skip(i + 1) {
            if right.kind == left.kind || right.call_id() != left.call_id() || !right.is_completed()
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
/// Input is the discovery items only, in conversation order; indices in the
/// result align with the input slice.
pub fn pairing_of(items: &[ToolSearchItem]) -> Vec<ToolSearchPairing> {
    let partners = partner_indices(items);
    items
        .iter()
        .zip(partners)
        .map(|(item, partner)| match (item.is_completed(), partner) {
            // Only completed copies take part in a pair, so a skeleton is
            // never reported as conversation state.
            (false, _) => ToolSearchPairing::Incomplete,
            (true, Some(_)) => ToolSearchPairing::Paired,
            (true, None) if item.kind == ToolSearchKind::Call => ToolSearchPairing::UnansweredCall,
            (true, None) => ToolSearchPairing::OrphanOutput,
        })
        .collect()
}

/// A definition together with the namespace group it was discovered under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadedDefinition<'a> {
    /// The callable definition's exact provider JSON.
    pub definition: &'a Value,
    /// The namespace group name this definition arrived under, or `None` for a
    /// definition the provider returned flat.
    pub namespace: Option<&'a str>,
}

impl<'a> LoadedDefinition<'a> {
    /// The definition's own `name`.
    pub fn name(&self) -> Option<&'a str> {
        self.definition.get("name").and_then(Value::as_str)
    }

    /// Model-visible length of the definition.
    pub fn estimated_model_visible_len(&self) -> usize {
        self.definition.to_string().len()
    }
}

/// Every definition the conversation has already loaded, in first-seen order,
/// with namespace children flattened out of their groups.
///
/// A-14 established that the `tool_search_output` items ARE the loaded-tool
/// set. That makes this the one projection both wire arms need and disagree
/// about: Messages must see all of it DECLARED in `tools[]` (A-23 — a
/// `tool_reference` naming an undeclared tool is a 400), Responses must keep
/// all of it OUT of `tools[]` (A-19/A-23). It is also the direct test for
/// A-24's silent-loss failure mode: non-empty here plus nothing declared on a
/// Messages target means the model has been left without its discovered tools.
///
/// Only COMPLETED outputs contribute; a skeleton carries no definitions.
pub fn loaded_tool_set<'a>(
    items: impl IntoIterator<Item = &'a ToolSearchItem>,
) -> Vec<LoadedDefinition<'a>> {
    let mut loaded: Vec<LoadedDefinition<'a>> = Vec::new();
    for item in items {
        if !item.is_completed() {
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
/// [`super::tool_name`] (`flat_tool_name` / `parse_flat_tool_name`,
/// fail-closed on ambiguity), and inventing a second join here is exactly the
/// fourth vocabulary A-24 warns about. A Responses child short name is not
/// unique across servers, so a wrong join silently resolves to nothing or to
/// another server's tool.
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
                    { "type": "function", "name": "billing_fixture_tool_10",
                      "strict": false, "defer_loading": true,
                      "parameters": { "type": "object", "properties": {
                          "customer_id": { "type": "string" } },
                          "required": ["customer_id"] } }
                ]
            }],
            "internal_chat_message_metadata_passthrough": {
                "turn_id": "01a0d978-6741-7721-aa1d-aae236e4ed3e",
                "create_time": 1790354941
            }
        })
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
        assert_eq!(call.call_id(), "call_AOphypzlL1KKckJugyBS2PYn");
        assert_eq!(call.status(), ToolSearchStatus::Completed);
        assert!(call.is_completed());
        assert!(call.is_client_executed());
        assert_eq!(call.query(), Some("crm order management"));
        assert_eq!(call.limit(), Some(8));
        assert_eq!(call.tools().len(), 0, "a call carries no results");
    }

    /// The fact most often gotten wrong: `tool_search_call.arguments` is an
    /// OBJECT; `function_call.arguments` is a STRING. A string here is a
    /// defect and must be refused, not tolerated.
    #[test]
    fn arguments_is_an_object_and_a_string_is_refused() {
        let call = item(cx3_call());
        let arguments = call.arguments().expect("arguments are an object");
        assert_eq!(arguments.get("query").unwrap(), "crm order management");

        let mut broken = cx3_call();
        broken["arguments"] = json!("{\"query\":\"crm order management\"}");
        assert_eq!(
            ToolSearchItem::from_wire(broken),
            Err(ToolSearchItemError::ArgumentsNotAnObject {
                actual: "a JSON string"
            })
        );
    }

    /// The output's `id` is absent on the client-authored copy and present on
    /// the provider-minted one; the type must not invent one.
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
                "billing_fixture_tool_10"
            ]
        );
        assert!(
            !names.contains(&"mcp__ratchet_fixture"),
            "the group name must not be invocable"
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
                Some("billing_fixture_tool_10"),
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
        assert_eq!(loaded_tool_set([&skeleton]).len(), 0);
        assert_eq!(
            pairing_of(std::slice::from_ref(&skeleton)),
            vec![ToolSearchPairing::Incomplete]
        );
    }

    /// A non-completed RESULT that already carries definitions must not enter
    /// the loaded set. A-14 defines the loaded-tool set as the COMPLETED
    /// `tool_search_output` items; a partial or failed copy would otherwise
    /// grant the model tools the provider never finished loading, and on the
    /// Messages side drive a declaration for a tool that was never loaded.
    #[test]
    fn an_unfinished_result_contributes_no_definitions() {
        for status in ["in_progress", "failed"] {
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
            assert_eq!(
                pairing_of(std::slice::from_ref(&parsed)),
                vec![ToolSearchPairing::Incomplete]
            );
        }

        // Neither half may complete a pair on its own: the guard applies to the
        // candidate partner too, so an unfinished output never answers its call.
        let mut pending = cx1_namespaced_output();
        pending["status"] = json!("in_progress");
        pending["call_id"] = json!("call_AOphypzlL1KKckJugyBS2PYn");
        let pending = item(pending);
        let call = item(cx3_call());
        assert_eq!(
            pairing_of(&[call.clone(), pending.clone()]),
            vec![
                ToolSearchPairing::UnansweredCall,
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

    /// `call_id` is the only join key, so it cannot be absent or empty.
    #[test]
    fn an_item_without_a_call_id_cannot_be_paired_and_is_refused() {
        let mut no_key = cx3_call();
        no_key.as_object_mut().unwrap().remove("call_id");
        assert_eq!(
            ToolSearchItem::from_wire(no_key),
            Err(ToolSearchItemError::MissingCallId)
        );

        let mut empty_key = cx3_call();
        empty_key["call_id"] = json!("");
        assert_eq!(
            ToolSearchItem::from_wire(empty_key),
            Err(ToolSearchItemError::MissingCallId)
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

    /// The raw item IS the replay contract, so fields this type has no named
    /// accessor for (the donor's passthrough, a future `created_by`) must
    /// survive both the store round-trip and the wire projection.
    #[test]
    fn unknown_fields_survive_store_and_wire_round_trip() {
        let original = cx1_namespaced_output();
        let parsed = item(original.clone());

        assert_eq!(parsed.to_input_value().as_ref(), &original);

        let stored = serde_json::to_string(&parsed).unwrap();
        let reread: ToolSearchItem = serde_json::from_str(&stored).unwrap();
        assert_eq!(reread, parsed);
        assert_eq!(reread.to_input_value().as_ref(), &original);
        assert_eq!(
            reread.raw["internal_chat_message_metadata_passthrough"]["turn_id"],
            "01a0d978-6741-7721-aa1d-aae236e4ed3e"
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
        assert_eq!(parsed.to_input_value().as_ref(), &raw, "bytes unchanged");
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
    }

    /// Token accounting must see the payload, not the one-line summary: on
    /// Responses these definitions ride in the cached prefix.
    #[test]
    fn model_visible_length_is_the_payload_not_the_summary() {
        let grouped = item(cx1_namespaced_output());
        assert!(grouped.text_summary().len() < 80, "summary stays bounded");
        assert!(
            grouped.estimated_model_visible_len() > 10 * grouped.text_summary().len(),
            "the definitions are the cost, and they are model-visible"
        );
        assert_eq!(
            grouped.estimated_model_visible_len(),
            grouped.raw.to_string().len()
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
            "[tool_search results] 1 tools"
        );
    }

    #[test]
    fn kind_and_wire_tag_agree_in_both_directions() {
        for kind in [ToolSearchKind::Call, ToolSearchKind::Output] {
            assert_eq!(ToolSearchKind::from_item_type(kind.item_type()), Some(kind));
        }
        assert_eq!(ToolSearchKind::from_item_type("function_call"), None);
        assert_eq!(TOOL_SEARCH_CALL_ITEM_TYPE, "tool_search_call");
        assert_eq!(TOOL_SEARCH_OUTPUT_ITEM_TYPE, "tool_search_output");
    }

    #[test]
    fn duplicate_definitions_across_searches_load_once() {
        let first = item(r6_flat_output());
        let second = item(r6_flat_output());
        assert_eq!(loaded_tool_set([&first, &second]).len(), 1);
    }
}
