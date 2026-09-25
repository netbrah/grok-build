//! The canonical `__` tool-name algebra shared by both wires.
//!
//! Responses invokes a discovered tool by its CHILD SHORT NAME (`crm_fixture_tool_03`), Messages invokes the
//! FULL FLAT NAME (`mcp__codegraph__codegraph_status`). Those are not two conventions; they are the decode and
//! encode halves of ONE algebra over the `__` delimiter (A-16, plan `2026-09-25-s3a-responses-native-tool-search.md:1597-1606`;
//! `native-discovery-behaviour-spec.md` §6.3/§6.4; `xwire-boundary-map.md` §4/§7.2).
//!
//! The reference implementation is `netbrah__claude-codex`
//! (`codex-rs/provider-anthropic/src/wire.rs:950-972` flattens every namespace child to `{ns}__{name}` and warns
//! that without the flattening "every namespaced MCP server ... is silently invisible";
//! `codex-rs/codex-wire-extensions/src/tool_name.rs:53,68-91` is the encoder and the newtype;
//! `codex-rs/codex-mcp/src/tools.rs:287-303` is the decoder). We adopt that algebra verbatim and introduce no
//! third dialect.
//!
//! Two disciplines are load-bearing here:
//!
//! 1. **Typed wire name.** [`WireToolName`] has exactly one constructor — the encoder. A bare `to_string()` of a
//!    [`ToolName`] does not type-check at a site typed to accept a [`WireToolName`], which is the compile-time
//!    guard against emitting a short name where the flat name is required.
//! 2. **Fail closed on ambiguity.** [`ToolResolutionMap`] refuses to guess when two namespaces in the loaded set
//!    expose the same child short name. `native-discovery-behaviour-spec.md` §6.4 rule 3: the harness "MUST return
//!    an in-band error and MUST NOT guess"; the failure mode both `AUDIT:176` (C8) and `AUDIT:250` (X6) name is
//!    "wrong MCP server executes a same-named function".

use std::collections::BTreeMap;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Delimiter between an MCP namespace and a tool name on every flat wire.
///
/// `NB/codex-rs/codex-wire-extensions/src/tool_name.rs:53`; the same delimiter our own MCP layer already uses
/// (`xai-grok-mcp/src/servers.rs:1200-1218`).
pub const FLAT_TOOL_NAME_DELIMITER: &str = "__";

/// Prefix that marks a namespace as an MCP server namespace (`mcp__ratchet_fixture`, `mcp__codegraph`).
///
/// The decoder only splits names carrying this prefix, exactly as `parse_flat_mcp_tool_name` does
/// (`NB/codex-rs/codex-mcp/src/tools.rs:287-303`). Bare and non-MCP names pass through unnamespaced.
pub const MCP_NAMESPACE_PREFIX: &str = "mcp__";

/// The canonical identity of a callable tool: the namespace split is preserved, never flattened into storage.
///
/// Storing `namespace` and `name` separately is mandatory, not stylistic: a flat-string-only store cannot
/// round-trip Responses -> Messages -> Responses (`xwire-boundary-map.md` §4, XT-7).
#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct ToolName {
    /// The namespace group name as it appears in a `tool_search_output` namespace element, e.g.
    /// `"mcp__ratchet_fixture"`. `None` for a flat (unnamespaced) tool.
    pub namespace: Option<String>,
    /// The child short name, e.g. `"crm_fixture_tool_03"`. This is what Responses invokes.
    pub name: String,
}

impl ToolName {
    /// Build an identity from an optional namespace and a name. An empty namespace is normalised to `None` so
    /// the encoder and the map agree on one spelling.
    #[must_use]
    pub fn new(namespace: Option<String>, name: impl Into<String>) -> Self {
        Self {
            namespace: namespace.filter(|ns| !ns.is_empty()),
            name: name.into(),
        }
    }

    /// An unnamespaced tool (a built-in, or a flat discovered tool as in the R1 hosted capture).
    #[must_use]
    pub fn plain(name: impl Into<String>) -> Self {
        Self {
            namespace: None,
            name: name.into(),
        }
    }

    /// A namespace child, e.g. `ToolName::namespaced("mcp__ratchet_fixture", "crm_fixture_tool_03")`.
    #[must_use]
    pub fn namespaced(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(Some(namespace.into()), name)
    }

    /// The namespace group name, if any.
    #[must_use]
    pub fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
    }

    /// The child short name.
    #[must_use]
    pub fn short_name(&self) -> &str {
        &self.name
    }

    /// The single blessed encoder: the flat `<namespace>__<name>` string every flat wire advertises.
    #[must_use]
    pub fn to_wire_name(&self) -> WireToolName {
        flat_tool_name(&self.name, self.namespace.as_deref())
    }

    /// The name a Responses `function_call` uses for this tool: the CHILD SHORT NAME (A-16, S5).
    #[must_use]
    pub fn responses_wire_name(&self) -> &str {
        &self.name
    }

    /// The name a Messages `tool_use` / `tool_reference` uses for this tool: the FULL FLAT NAME (A-16, S3).
    #[must_use]
    pub fn messages_wire_name(&self) -> WireToolName {
        self.to_wire_name()
    }

    /// Whether this identity survives `encode -> decode` unchanged.
    ///
    /// The decoder splits at the FIRST `__` after the `mcp__<server>` prefix, so a namespace that itself contains
    /// a further `__` (e.g. `mcp__a__b`) is NOT recoverable from its flat encoding. Such identities are legal and
    /// storable, but a consumer must route them by exact identity, never by decoding the flat string.
    /// [`ToolResolutionMap`] does exactly that: it indexes the encoded form directly.
    #[must_use]
    pub fn round_trips(&self) -> bool {
        parse_flat_tool_name(self.to_wire_name().as_str()) == *self
    }
}

impl From<String> for ToolName {
    fn from(name: String) -> Self {
        Self::plain(name)
    }
}

impl From<&str> for ToolName {
    fn from(name: &str) -> Self {
        Self::plain(name)
    }
}

/// Human-readable rendering ONLY — `<namespace>/<name>`, which is deliberately NOT a wire spelling.
///
/// The wire spelling is [`ToolName::to_wire_name`]. Keeping `Display` visibly different from the wire name is the
/// point: a log line and a wire payload cannot be confused, and a `to_string()` at a wire site is a bug the type
/// system catches because wire sites take a [`WireToolName`].
impl fmt::Display for ToolName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.namespace {
            Some(namespace) => write!(f, "{namespace}/{}", self.name),
            None => f.write_str(&self.name),
        }
    }
}

/// A tool name encoded for a flat wire (Messages, Gemini, chat-completions), which has no structured
/// `{namespace, name}` split.
///
/// The wrapped string is always delimiter-correct because the ONLY constructors are [`flat_tool_name`] and
/// [`ToolName::to_wire_name`]. There is deliberately no `From<String>`, no `Deserialize`, and no constructor from
/// a `Display` rendering: that absence is what makes a bare short name un-substitutable at a site typed to accept
/// a `WireToolName` (`NB/codex-rs/codex-wire-extensions/src/tool_name.rs:78-91`).
#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, Serialize)]
pub struct WireToolName(String);

impl WireToolName {
    /// Borrow the encoded name for serialisation into a wire payload.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume into the owned encoded string.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }

    /// Decode back into the canonical identity. The inverse of the encoder on the round-trip domain
    /// (see [`ToolName::round_trips`]).
    #[must_use]
    pub fn to_tool_name(&self) -> ToolName {
        parse_flat_tool_name(&self.0)
    }
}

impl fmt::Display for WireToolName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl PartialEq<&str> for WireToolName {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<str> for WireToolName {
    fn eq(&self, other: &str) -> bool {
        self.0 == *other
    }
}

/// ENCODE. `(name, namespace)` -> the flat string the flat wires advertise and re-encode.
///
/// - namespaced   -> `"<namespace>__<name>"`
/// - unnamespaced -> `"<name>"`
///
/// History re-encoders MUST route through this and never emit the bare child name: the model's view of its own
/// past calls must stay byte-identical to the advertised tool list
/// (`NB/codex-rs/codex-wire-extensions/src/tool_name.rs:68-76`).
#[must_use]
pub fn flat_tool_name(name: &str, namespace: Option<&str>) -> WireToolName {
    let encoded = match namespace {
        Some(namespace) if !namespace.is_empty() => {
            format!("{namespace}{FLAT_TOOL_NAME_DELIMITER}{name}")
        }
        _ => name.to_string(),
    };
    WireToolName(encoded)
}

/// DECODE. The flat wire string -> the canonical identity.
///
/// `mcp__<server>__<tool>` -> `(Some("mcp__<server>"), "<tool>")`, splitting at the FIRST `__` after the
/// `mcp__<server>` prefix, so `mcp__server__some__deep__tool` -> `(mcp__server, some__deep__tool)`. Everything
/// else — built-ins, non-MCP names, `mcp__no_tool_part` — passes through unnamespaced
/// (`NB/codex-rs/codex-mcp/src/tools.rs:287-303`, pinned tests `:306-345`).
#[must_use]
pub fn parse_flat_tool_name(flat: &str) -> ToolName {
    let Some(rest) = flat.strip_prefix(MCP_NAMESPACE_PREFIX) else {
        return ToolName::plain(flat);
    };
    let Some(index) = rest.find(FLAT_TOOL_NAME_DELIMITER) else {
        return ToolName::plain(flat);
    };
    let (server, after) = rest.split_at(index);
    let tool = &after[FLAT_TOOL_NAME_DELIMITER.len()..];
    if server.is_empty() || tool.is_empty() {
        return ToolName::plain(flat);
    }
    ToolName::namespaced(format!("{MCP_NAMESPACE_PREFIX}{server}"), tool)
}

/// What a resolution attempt decided.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Resolution<'a> {
    /// The call names a tool in the loaded (discovered) set. Dispatch to this identity.
    Discovered(&'a ToolName),
    /// The call names nothing in the loaded set. The caller falls through to step (c) of
    /// `native-discovery-behaviour-spec.md` §6.4: resolve the bare name as an ordinary DECLARED tool.
    /// This is never a guess across namespaces.
    NotDiscovered,
}

impl<'a> Resolution<'a> {
    /// The resolved identity, if the call hit the loaded set. Takes `self` (the enum is `Copy`) so the borrow
    /// lives as long as the map, not as long as the temporary `Resolution`.
    #[must_use]
    pub fn discovered(self) -> Option<&'a ToolName> {
        match self {
            Self::Discovered(tool) => Some(tool),
            Self::NotDiscovered => None,
        }
    }
}

/// A resolution refusal. Every variant is a FAIL-CLOSED outcome: the harness returns this in band and dispatches
/// nothing (`native-discovery-behaviour-spec.md` §6.4 rule 3).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NameResolutionError {
    /// Two or more loaded namespaces expose the same child short name and the call did not disambiguate.
    AmbiguousShortName {
        /// The undisambiguated child short name the model invoked.
        short_name: String,
        /// Every loaded identity carrying that short name, as flat wire names, sorted.
        candidates: Vec<String>,
    },
    /// Two or more loaded identities encode to the same flat wire name.
    AmbiguousFlatName {
        /// The flat name the model invoked.
        flat_name: String,
        /// Every loaded identity that encodes to it, rendered `<namespace>/<name>`, sorted.
        candidates: Vec<String>,
    },
    /// The call carried an empty tool name.
    EmptyName,
}

impl fmt::Display for NameResolutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AmbiguousShortName {
                short_name,
                candidates,
            } => write!(
                f,
                "ambiguous tool name '{short_name}': {} loaded namespaces expose it ({}); re-issue the call with the namespace that owns it",
                candidates.len(),
                candidates.join(", ")
            ),
            Self::AmbiguousFlatName {
                flat_name,
                candidates,
            } => write!(
                f,
                "ambiguous flat tool name '{flat_name}': {} loaded identities encode to it ({})",
                candidates.len(),
                candidates.join(", ")
            ),
            Self::EmptyName => f.write_str("tool call carried an empty name"),
        }
    }
}

impl std::error::Error for NameResolutionError {}

/// One index slot: unique, or contested by several identities.
#[derive(Clone, Debug)]
enum Slot {
    Unique(ToolName),
    Ambiguous(Vec<ToolName>),
}

impl Slot {
    fn push(&mut self, tool: ToolName) {
        match self {
            Self::Unique(existing) => {
                if *existing == tool {
                    return;
                }
                let mut all = vec![existing.clone(), tool];
                all.sort();
                *self = Self::Ambiguous(all);
            }
            Self::Ambiguous(all) => {
                if !all.contains(&tool) {
                    all.push(tool);
                    all.sort();
                }
            }
        }
    }
}

/// The per-turn RESOLUTION MAP required by `native-discovery-behaviour-spec.md` §6.4 rule 1: built at the moment
/// the harness mints a `tool_search_output`, keyed `(namespace_name, child_short_name) -> canonical identity`,
/// with a child-short-name index for the Responses dialect and a flat-name index for the Messages dialect.
///
/// It FAILS CLOSED: a contested short name resolves to [`NameResolutionError::AmbiguousShortName`], never to a
/// candidate.
#[derive(Clone, Debug, Default)]
pub struct ToolResolutionMap {
    /// `(namespace, short name) -> identity`. Exact, never ambiguous by construction.
    by_qualified: BTreeMap<(Option<String>, String), ToolName>,
    /// child short name -> identity, contested when two namespaces expose the same child.
    by_short: BTreeMap<String, Slot>,
    /// encoded flat name -> identity, contested when two identities encode identically.
    by_flat: BTreeMap<String, Slot>,
}

impl ToolResolutionMap {
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one loaded tool. Re-registering the identical identity is idempotent and never creates ambiguity.
    pub fn insert(&mut self, tool: ToolName) {
        let key = (tool.namespace.clone(), tool.name.clone());
        let flat = tool.to_wire_name().into_string();
        self.by_qualified.insert(key, tool.clone());
        self.by_short
            .entry(tool.name.clone())
            .and_modify(|slot| slot.push(tool.clone()))
            .or_insert_with(|| Slot::Unique(tool.clone()));
        self.by_flat
            .entry(flat)
            .and_modify(|slot| slot.push(tool.clone()))
            .or_insert_with(|| Slot::Unique(tool));
    }

    /// Register every child of one namespace group.
    pub fn insert_namespace_children<I, S>(&mut self, namespace: &str, children: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for child in children {
            self.insert(ToolName::namespaced(namespace, child));
        }
    }

    /// Number of distinct identities registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_qualified.len()
    }

    /// Whether the loaded set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_qualified.is_empty()
    }

    /// Every registered identity, in canonical order.
    pub fn tools(&self) -> impl Iterator<Item = &ToolName> {
        self.by_qualified.values()
    }

    /// Every child short name that more than one loaded namespace exposes.
    ///
    /// A projector materialising the loaded set onto another wire can call this BEFORE emitting, so the collision
    /// is reported at build time rather than discovered at invocation time.
    #[must_use]
    pub fn ambiguous_short_names(&self) -> Vec<&str> {
        self.by_short
            .iter()
            .filter(|(_, slot)| matches!(slot, Slot::Ambiguous(_)))
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// Exact resolution by canonical identity. Never guesses, never falls back.
    #[must_use]
    pub fn resolve_qualified(&self, namespace: Option<&str>, name: &str) -> Option<&ToolName> {
        let namespace = namespace.filter(|ns| !ns.is_empty()).map(str::to_string);
        self.by_qualified.get(&(namespace, name.to_string()))
    }

    /// Resolve an inbound RESPONSES `function_call`, per `native-discovery-behaviour-spec.md` §6.4 rule 2.
    ///
    /// a. `namespace` is a non-empty string that DIFFERS from `name` -> exact `(namespace, name)` lookup.
    ///    A miss here returns [`Resolution::NotDiscovered`]; it never retries as a bare name, because that is
    ///    precisely the cross-namespace guess rule 3 forbids.
    /// b. otherwise -> the child-short-name index, which FAILS CLOSED when contested. `namespace == name` is the
    ///    measured R1 spelling for a flat discovered tool (`R1_SOL_HOSTED_ts.json#/output/3`), so it takes this
    ///    branch, as does `namespace: null` (R4).
    /// c. a miss is [`Resolution::NotDiscovered`]: the caller resolves the bare name as an ordinary declared tool.
    pub fn resolve_function_call(
        &self,
        name: &str,
        namespace: Option<&str>,
    ) -> Result<Resolution<'_>, NameResolutionError> {
        if name.is_empty() {
            return Err(NameResolutionError::EmptyName);
        }
        if let Some(namespace) = namespace.filter(|ns| !ns.is_empty() && *ns != name) {
            return Ok(self
                .resolve_qualified(Some(namespace), name)
                .map_or(Resolution::NotDiscovered, Resolution::Discovered));
        }
        self.resolve_short_name(name)
    }

    /// Resolve a bare CHILD SHORT NAME against the loaded set, failing closed when two namespaces contest it.
    pub fn resolve_short_name(&self, name: &str) -> Result<Resolution<'_>, NameResolutionError> {
        if name.is_empty() {
            return Err(NameResolutionError::EmptyName);
        }
        match self.by_short.get(name) {
            Some(Slot::Unique(tool)) => Ok(Resolution::Discovered(tool)),
            Some(Slot::Ambiguous(candidates)) => {
                Err(NameResolutionError::AmbiguousShortName {
                    short_name: name.to_string(),
                    candidates: candidates
                        .iter()
                        .map(|tool| tool.to_wire_name().into_string())
                        .collect(),
                })
            }
            None => Ok(Resolution::NotDiscovered),
        }
    }

    /// Resolve an inbound MESSAGES invocation, which names the FULL FLAT NAME (A-16, S3).
    ///
    /// Order: the exact encoded-name index first (so a namespace containing a further `__` still routes, even
    /// though its flat form is not decodable), then the decoded identity, then the bare short name. Every step
    /// fails closed when contested.
    pub fn resolve_flat_name(&self, flat: &str) -> Result<Resolution<'_>, NameResolutionError> {
        if flat.is_empty() {
            return Err(NameResolutionError::EmptyName);
        }
        match self.by_flat.get(flat) {
            Some(Slot::Unique(tool)) => return Ok(Resolution::Discovered(tool)),
            Some(Slot::Ambiguous(candidates)) => {
                return Err(NameResolutionError::AmbiguousFlatName {
                    flat_name: flat.to_string(),
                    candidates: candidates.iter().map(ToString::to_string).collect(),
                });
            }
            None => {}
        }
        let decoded = parse_flat_tool_name(flat);
        if let Some(tool) = self.resolve_qualified(decoded.namespace(), decoded.short_name()) {
            return Ok(Resolution::Discovered(tool));
        }
        if decoded.namespace().is_none() {
            return self.resolve_short_name(decoded.short_name());
        }
        Ok(Resolution::NotDiscovered)
    }
}

impl FromIterator<ToolName> for ToolResolutionMap {
    fn from_iter<I: IntoIterator<Item = ToolName>>(iter: I) -> Self {
        let mut map = Self::new();
        for tool in iter {
            map.insert(tool);
        }
        map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- ENCODE (the `{ns}__{name}` flattening of NB/wire.rs:950-972) ----

    #[test]
    fn encode_namespaced_uses_double_underscore() {
        assert_eq!(
            flat_tool_name("crm_fixture_tool_03", Some("mcp__ratchet_fixture")),
            "mcp__ratchet_fixture__crm_fixture_tool_03"
        );
    }

    #[test]
    fn encode_matches_the_measured_messages_name() {
        // CC2/CC3 invoke exactly this string (A-16, S3).
        assert_eq!(
            ToolName::namespaced("mcp__codegraph", "codegraph_status")
                .messages_wire_name()
                .as_str(),
            "mcp__codegraph__codegraph_status"
        );
    }

    #[test]
    fn encode_unnamespaced_and_empty_namespace_are_bare() {
        assert_eq!(flat_tool_name("shell", None), "shell");
        assert_eq!(flat_tool_name("shell", Some("")), "shell");
        assert_eq!(ToolName::new(Some(String::new()), "shell").namespace(), None);
    }

    #[test]
    fn display_is_not_the_wire_spelling() {
        // The compile-time guard's reason to exist: a `to_string()` must never look like a wire name.
        let tool = ToolName::namespaced("mcp__codegraph", "codegraph_status");
        assert_eq!(tool.to_string(), "mcp__codegraph/codegraph_status");
        assert_ne!(tool.to_string(), tool.to_wire_name().as_str());
    }

    // ---- DECODE (NB/codex-rs/codex-mcp/src/tools.rs:287-303 and its pinned cases) ----

    #[test]
    fn decode_canonical_namespaced_form() {
        assert_eq!(
            parse_flat_tool_name("mcp__codegraph__codegraph_status"),
            ToolName::namespaced("mcp__codegraph", "codegraph_status")
        );
    }

    #[test]
    fn decode_splits_at_first_delimiter_after_server() {
        assert_eq!(
            parse_flat_tool_name("mcp__server__some__deep__tool"),
            ToolName::namespaced("mcp__server", "some__deep__tool")
        );
    }

    #[test]
    fn decode_passes_through_when_not_namespaced() {
        assert_eq!(parse_flat_tool_name("shell"), ToolName::plain("shell"));
        assert_eq!(
            parse_flat_tool_name("mcp__no_tool_part"),
            ToolName::plain("mcp__no_tool_part")
        );
        // Legacy managed-gateway spelling: no `mcp__` prefix, so it stays whole rather than inventing a namespace.
        assert_eq!(
            parse_flat_tool_name("linear__save_issue"),
            ToolName::plain("linear__save_issue")
        );
    }

    #[test]
    fn decode_rejects_empty_server_or_tool() {
        assert_eq!(
            parse_flat_tool_name("mcp____tool"),
            ToolName::plain("mcp____tool")
        );
        assert_eq!(
            parse_flat_tool_name("mcp__server__"),
            ToolName::plain("mcp__server__")
        );
    }

    // ---- ROUND TRIP ----

    #[test]
    fn round_trip_holds_on_the_measured_identities() {
        for tool in [
            ToolName::namespaced("mcp__ratchet_fixture", "crm_fixture_tool_03"),
            ToolName::namespaced("mcp__codebase_memory_mcp", "get_file_outline"),
            ToolName::namespaced("mcp__codegraph", "codegraph_status"),
            ToolName::plain("lookup_shipping_eta"),
            ToolName::plain("shell"),
        ] {
            assert!(tool.round_trips(), "{tool} must survive encode -> decode");
            assert_eq!(tool.to_wire_name().to_tool_name(), tool);
        }
    }

    #[test]
    fn round_trip_is_honestly_false_outside_its_domain() {
        // A namespace that itself contains `__` is not recoverable from the flat string; the type says so rather
        // than pretending. Such tools are routed by the exact flat index, never by decoding (see the map test).
        let nested = ToolName::namespaced("mcp__a__b", "tool");
        assert_eq!(nested.to_wire_name(), "mcp__a__b__tool");
        assert!(!nested.round_trips());
        assert_eq!(
            parse_flat_tool_name(nested.to_wire_name().as_str()),
            ToolName::namespaced("mcp__a", "b__tool")
        );
    }

    /// Deterministic xorshift so the property test is reproducible and needs no dev-dependency.
    struct Rng(u64);

    impl Rng {
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next_u64() % n as u64) as usize
        }

        /// A segment from the alphabet real tool names use, including single `_` and digits, but never `__`.
        fn segment(&mut self, max_len: usize) -> String {
            const ALPHABET: &[u8] = b"abcz09_";
            let len = 1 + self.below(max_len);
            let mut out = String::new();
            for _ in 0..len {
                let ch = ALPHABET[self.below(ALPHABET.len())] as char;
                if ch == '_' && out.ends_with('_') {
                    out.push('a');
                } else {
                    out.push(ch);
                }
            }
            if out.starts_with('_') {
                out.insert(0, 'a');
            }
            if out.ends_with('_') {
                out.push('a');
            }
            out
        }
    }

    #[test]
    fn property_encode_decode_round_trip() {
        let mut rng = Rng(0x5eed_1234_9abc_def1);
        let mut namespaced = 0usize;
        let mut plain = 0usize;
        for _ in 0..20_000 {
            // Domain: a `mcp__<server>` namespace whose server segment carries no `__`, and a non-empty name.
            // Names MAY contain `__` — the decoder keeps the remainder whole, which is the pinned deep-tool case.
            let name = if rng.below(4) == 0 {
                format!("{}__{}", rng.segment(6), rng.segment(6))
            } else {
                rng.segment(12)
            };
            let tool = if rng.below(3) == 0 {
                plain += 1;
                ToolName::plain(name)
            } else {
                namespaced += 1;
                ToolName::namespaced(format!("{MCP_NAMESPACE_PREFIX}{}", rng.segment(10)), name)
            };
            let wire = tool.to_wire_name();
            let decoded = parse_flat_tool_name(wire.as_str());
            assert_eq!(decoded, tool, "round trip failed for {wire}");
            // Re-encoding the decoded identity is byte-identical: history re-encode == advertisement.
            assert_eq!(decoded.to_wire_name(), wire);
        }
        assert!(namespaced > 1_000 && plain > 1_000, "generator degenerated: {namespaced} namespaced / {plain} plain");
    }

    #[test]
    fn property_plain_names_that_look_namespaced_are_stable_under_decode() {
        // A plain name beginning with `mcp__` decodes to a namespaced identity — the algebra cannot tell the two
        // apart, which is WHY the map indexes the encoded string as well. Decoding is idempotent from step two on.
        let mut rng = Rng(0xfeed_face_0000_0001);
        for _ in 0..5_000 {
            let flat = format!(
                "{MCP_NAMESPACE_PREFIX}{}__{}",
                rng.segment(8),
                rng.segment(8)
            );
            let once = parse_flat_tool_name(&flat);
            let twice = parse_flat_tool_name(once.to_wire_name().as_str());
            assert_eq!(once, twice);
            assert_eq!(once.to_wire_name(), flat.as_str());
        }
    }

    // ---- RESOLUTION MAP: §6.4 rule 2 ----

    fn cx3_map() -> ToolResolutionMap {
        // The measured CX3 discovery result: 2 namespaces, 8 children (next-turn.json#/input/12).
        let mut map = ToolResolutionMap::new();
        map.insert_namespace_children(
            "mcp__ratchet_fixture",
            [
                "crm_fixture_tool_01",
                "crm_fixture_tool_02",
                "crm_fixture_tool_03",
                "crm_fixture_tool_04",
                "crm_fixture_tool_05",
                "crm_fixture_tool_06",
                "crm_fixture_tool_07",
            ],
        );
        map.insert_namespace_children("mcp__codebase_memory_mcp", ["get_file_outline"]);
        map
    }

    #[test]
    fn map_registers_the_measured_cardinality() {
        let map = cx3_map();
        assert_eq!(map.len(), 8);
        assert!(!map.is_empty());
        assert!(map.ambiguous_short_names().is_empty());
    }

    #[test]
    fn rule_2a_qualified_namespace_resolves_exactly() {
        let map = cx3_map();
        let resolved = map
            .resolve_function_call("crm_fixture_tool_03", Some("mcp__ratchet_fixture"))
            .expect("qualified call resolves")
            .discovered()
            .expect("in the loaded set");
        assert_eq!(
            resolved,
            &ToolName::namespaced("mcp__ratchet_fixture", "crm_fixture_tool_03")
        );
    }

    #[test]
    fn rule_2a_wrong_namespace_never_falls_back_to_the_short_name() {
        // The cross-namespace guess is exactly the C8/X6 failure mode: "wrong MCP server executes a same-named
        // function". A namespaced miss must NOT retry as a bare name.
        let map = cx3_map();
        assert_eq!(
            map.resolve_function_call("crm_fixture_tool_03", Some("mcp__codebase_memory_mcp")),
            Ok(Resolution::NotDiscovered)
        );
    }

    #[test]
    fn rule_2b_child_short_name_resolves() {
        // S5: Responses invokes the CHILD SHORT NAME.
        let map = cx3_map();
        assert_eq!(
            map.resolve_function_call("crm_fixture_tool_03", None)
                .expect("short name resolves")
                .discovered(),
            Some(&ToolName::namespaced(
                "mcp__ratchet_fixture",
                "crm_fixture_tool_03"
            ))
        );
    }

    #[test]
    fn rule_2b_r1_spelling_namespace_equals_name_takes_the_bare_path() {
        // Measured R1 shape: {"name":"lookup_shipping_eta","namespace":"lookup_shipping_eta"} for a FLAT tool.
        let map: ToolResolutionMap = [ToolName::plain("lookup_shipping_eta")].into_iter().collect();
        assert_eq!(
            map.resolve_function_call("lookup_shipping_eta", Some("lookup_shipping_eta"))
                .expect("resolves")
                .discovered(),
            Some(&ToolName::plain("lookup_shipping_eta"))
        );
    }

    #[test]
    fn rule_2c_unknown_name_is_not_discovered_not_an_error() {
        let map = cx3_map();
        assert_eq!(
            map.resolve_function_call("shell", None),
            Ok(Resolution::NotDiscovered)
        );
        assert_eq!(
            map.resolve_function_call("shell", Some(""))
                .expect("empty namespace behaves as null"),
            Resolution::NotDiscovered
        );
    }

    #[test]
    fn empty_name_is_rejected() {
        let map = cx3_map();
        assert_eq!(
            map.resolve_function_call("", None),
            Err(NameResolutionError::EmptyName)
        );
        assert_eq!(
            map.resolve_flat_name(""),
            Err(NameResolutionError::EmptyName)
        );
    }

    // ---- RULE 3: FAIL CLOSED ON COLLISION ----

    fn collision_map() -> ToolResolutionMap {
        [
            ToolName::namespaced("mcp__ratchet_fixture", "crm_fixture_tool_03"),
            ToolName::namespaced("mcp__other_server", "crm_fixture_tool_03"),
            ToolName::namespaced("mcp__ratchet_fixture", "crm_fixture_tool_01"),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn collision_short_name_fails_closed_and_names_both_candidates() {
        let map = collision_map();
        let err = map
            .resolve_function_call("crm_fixture_tool_03", None)
            .expect_err("a contested short name MUST NOT resolve");
        assert_eq!(
            err,
            NameResolutionError::AmbiguousShortName {
                short_name: "crm_fixture_tool_03".to_string(),
                candidates: vec![
                    "mcp__other_server__crm_fixture_tool_03".to_string(),
                    "mcp__ratchet_fixture__crm_fixture_tool_03".to_string(),
                ],
            }
        );
        let rendered = err.to_string();
        assert!(rendered.contains("mcp__other_server__crm_fixture_tool_03"), "{rendered}");
        assert!(rendered.contains("mcp__ratchet_fixture__crm_fixture_tool_03"), "{rendered}");
    }

    #[test]
    fn collision_is_reported_before_invocation() {
        assert_eq!(
            collision_map().ambiguous_short_names(),
            vec!["crm_fixture_tool_03"]
        );
    }

    #[test]
    fn collision_still_resolves_when_the_call_disambiguates() {
        let map = collision_map();
        for namespace in ["mcp__ratchet_fixture", "mcp__other_server"] {
            let resolved = map
                .resolve_function_call("crm_fixture_tool_03", Some(namespace))
                .expect("qualified call is unambiguous")
                .discovered()
                .expect("in the loaded set");
            assert_eq!(resolved.namespace(), Some(namespace));
        }
        // And the flat (Messages) spelling of each is unambiguous too.
        assert_eq!(
            map.resolve_flat_name("mcp__other_server__crm_fixture_tool_03")
                .expect("flat name resolves")
                .discovered()
                .map(ToolName::namespace),
            Some(Some("mcp__other_server"))
        );
    }

    #[test]
    fn collision_does_not_leak_into_the_uncontested_sibling() {
        let map = collision_map();
        assert_eq!(
            map.resolve_function_call("crm_fixture_tool_01", None)
                .expect("uncontested")
                .discovered()
                .map(ToolName::namespace),
            Some(Some("mcp__ratchet_fixture"))
        );
    }

    #[test]
    fn identical_identity_inserted_twice_is_not_a_collision() {
        let mut map = ToolResolutionMap::new();
        map.insert(ToolName::namespaced("mcp__codegraph", "codegraph_status"));
        map.insert(ToolName::namespaced("mcp__codegraph", "codegraph_status"));
        assert_eq!(map.len(), 1);
        assert!(map.ambiguous_short_names().is_empty());
        assert!(
            map.resolve_function_call("codegraph_status", None)
                .expect("resolves")
                .discovered()
                .is_some()
        );
    }

    #[test]
    fn flat_name_collision_between_two_identities_fails_closed() {
        // `(None, "mcp__a__b")` and `("mcp__a", "b")` encode to the SAME flat string. The map refuses both.
        let map: ToolResolutionMap = [
            ToolName::plain("mcp__a__b"),
            ToolName::namespaced("mcp__a", "b"),
        ]
        .into_iter()
        .collect();
        let err = map
            .resolve_flat_name("mcp__a__b")
            .expect_err("contested flat name MUST NOT resolve");
        assert_eq!(
            err,
            NameResolutionError::AmbiguousFlatName {
                flat_name: "mcp__a__b".to_string(),
                // Sorted by canonical identity: the unnamespaced identity (`None`) orders first.
                candidates: vec!["mcp__a__b".to_string(), "mcp__a/b".to_string()],
            }
        );
    }

    // ---- MESSAGES DIALECT ----

    #[test]
    fn messages_flat_name_resolves_to_the_same_identity_as_the_short_name() {
        // A-16 both halves, one identity: this is the whole point of the seam.
        let map = cx3_map();
        let by_flat = map
            .resolve_flat_name("mcp__ratchet_fixture__crm_fixture_tool_03")
            .expect("flat resolves")
            .discovered()
            .expect("loaded");
        let by_short = map
            .resolve_function_call("crm_fixture_tool_03", None)
            .expect("short resolves")
            .discovered()
            .expect("loaded");
        assert_eq!(by_flat, by_short);
    }

    #[test]
    fn messages_flat_name_routes_a_namespace_that_is_not_decodable() {
        // `mcp__a__b` as a NAMESPACE does not decode, so the exact encoded index carries it.
        let map: ToolResolutionMap = [ToolName::namespaced("mcp__a__b", "tool")]
            .into_iter()
            .collect();
        assert_eq!(
            map.resolve_flat_name("mcp__a__b__tool")
                .expect("resolves via the encoded index")
                .discovered(),
            Some(&ToolName::namespaced("mcp__a__b", "tool"))
        );
    }

    #[test]
    fn messages_unknown_flat_name_is_not_discovered() {
        let map = cx3_map();
        assert_eq!(
            map.resolve_flat_name("mcp__unknown_server__whatever"),
            Ok(Resolution::NotDiscovered)
        );
    }

    // ---- XT-7: the cross-wire name dialect round trip ----

    #[test]
    fn xt7_responses_to_messages_and_back_preserves_identity() {
        let map = cx3_map();
        let original: Vec<ToolName> = map.tools().cloned().collect();
        assert_eq!(original.len(), 8);

        // Responses -> Messages: every child is materialised as `<ns>__<name>`.
        let messages_names: Vec<String> = original
            .iter()
            .map(|tool| tool.messages_wire_name().into_string())
            .collect();
        assert!(
            messages_names.contains(&"mcp__ratchet_fixture__crm_fixture_tool_03".to_string())
        );

        // Messages -> Responses: the flat names decode back to the same `(namespace, name)` SET, and no child is
        // left carrying the flat spelling on the Responses side.
        let mut back: Vec<ToolName> = messages_names
            .iter()
            .map(|flat| parse_flat_tool_name(flat))
            .collect();
        back.sort();
        let mut expected = original;
        expected.sort();
        assert_eq!(back, expected);
        for tool in &back {
            assert!(
                !tool.responses_wire_name().starts_with(MCP_NAMESPACE_PREFIX),
                "{tool} still carries the flat spelling on the Responses side"
            );
        }
    }

    #[test]
    fn wire_name_serialises_as_the_bare_string() {
        let wire = ToolName::namespaced("mcp__codegraph", "codegraph_status").to_wire_name();
        assert_eq!(
            serde_json::to_string(&wire).expect("serialises"),
            "\"mcp__codegraph__codegraph_status\""
        );
        assert_eq!(wire.to_string(), "mcp__codegraph__codegraph_status");
        assert_eq!(wire.clone().into_string(), "mcp__codegraph__codegraph_status");
    }

    #[test]
    fn tool_name_serde_keeps_the_split() {
        let tool = ToolName::namespaced("mcp__ratchet_fixture", "crm_fixture_tool_03");
        let json = serde_json::to_string(&tool).expect("serialises");
        assert_eq!(
            json,
            r#"{"namespace":"mcp__ratchet_fixture","name":"crm_fixture_tool_03"}"#
        );
        let back: ToolName = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(back, tool);
    }
}
