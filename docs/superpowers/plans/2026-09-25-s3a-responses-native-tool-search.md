# S3a — Responses Native Tool Search Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement native client-executed `tool_search` on the Responses wire (spec S3a): deferred tool declarations, a frozen admitted catalog, structured search output, durable discovery records, and mode-aware display — with every route gate default OFF and non-admitted routes byte-identical to today.

**Architecture:** A provider-neutral seam (`ToolSpec.exposure`, frozen `DiscoveryManifest`, structured `SearchToolOutput`, durable `ToolResultItem.discovery`) is lowered faithfully at the Responses encoder/dispatcher. No new `ConversationItem` VARIANT in this plan (discovery rides an ADDITIVE field on `ToolResultItem`; the Phase B variant is S3c). Search reuse = the existing BM25 `ToolSearchIndex` trait restricted to the admitted manifest. The pinned `async-openai` dep is NOT touched: a local type seam in `xai-grok-sampling-types` models the 5 new wire types (INTEGRATION POINT A default; a later seat may swap in a dependency extension without changing this plan's task boundaries).

**Tech Stack:** Rust (cargo workspace `xai-grok-*` crates), serde (JSON wire + JSONL persistence), existing BM25 index, existing doom-loop failed-response capture for retry tests.

**Spec:** `/Users/palanisd/Projects/upstream/grok/plans/harness/hosted-tool-search/2026-09-24-hosted-tool-search-design.md` (v1.3 + nit-fix, plans repo HEAD — approved by operator 2026-09-25 after 3 gate rounds). Supporting inputs (same dir): `codex-donor-contract.md`, `seam-map-normalization.md`, `invariant-projection-concordance.md`, `HARNESS-MAP.md` (acceptance gate).

## Global Constraints

Every task's requirements implicitly include this section (values verbatim from spec v1.3):

- **Base:** worktree `wt/apex-ayl-hosted-tool-search`, branch `apex-ayl-hosted-tool-search`, off `feat/first-class-responses-catalog` @ `2f49335e`. Do not touch `wt/grok-build-responses` (operator tree) or `upstream/codex` (dirty; blob-reads only).
- **Churn budget = spec §7 closed list.** Any file outside it is a spec violation at review. The S3a subset is listed per task; S3b/S3c budget rows (Messages wire, Phase B variant files, `xai-chat-state` actor arms, `xai-compaction-transcript`) are NOT touched by this plan.
- **Capability gates default OFF.** `responses_client_tool_search` is the only gate this plan enables; it opens only when BOTH named inputs hold: (a) model-row flag `supports_search_tool` true in the grok catalog; (b) ≥1 deferred entry in the admitted manifest. Enabling is probe-evidence driven, never inferred from model-family labels or header presence.
- **Manifest freeze:** freezes when `mcp_initialized` becomes true for the turn (init pass final refresh landed, `mcp_init.rs:192-210` → `mcp_snapshot.rs:94`); if `wait_for_mcp_initialized_bounded_once` (`mcp_snapshot.rs:432-448`) times out first, NO manifest freezes and the gate stays closed (legacy fallback for that turn).
- **Declaration JSON (byte fidelity):** `limit` is JSON **number** (not integer), default limit **8** (`TOOL_SEARCH_DEFAULT_LIMIT`), parameter descriptions verbatim (`"Search query for deferred tools."` / `"Maximum number of tools to return. Defaults to 8."`), property order on the wire LEXICOGRAPHIC (**limit, query** — BTreeMap serialization), `required: ["query"]`, `additionalProperties: false`. Top-level `description` is DYNAMIC (source-listing) — byte-exactness applies to the parameter sub-fields at a fixed source set.
- **D-ERR channel:** unparseable search args → typed `tool_search_output` with `call_id` = the search call's real id, `tools: []`, `status: "error"` + `error` text field. No empty id anywhere. The donor's `error_or_panic` is NOT ported. Scope: search-tool errors only.
- **Ids:** `additional_tools` item id = two-stage: `prefix_ns = v5(NAMESPACE_OID, thread_id_bytes)`, `id = with_suffix("at", v5(prefix_ns, serde_json(tools)))`. Synthesized interrupted-output id = `with_suffix("tso", v5(SYNTHETIC_OUTPUT_ID_NAMESPACE, "tso:<source-call-item-id>"))` — derived from the call's **item id**, not call_id. Ids are never empty, never fabricated beyond these two classes (H-3 exemption; session-minted fallback if a strict route 400s).
- **Zero-match** = success with `tools: []` (no fallback text). **Parallel** search calls in one response are first-class (all outputs coalesced). **No reinjection:** loaded definitions stay in retained `tool_search_output` history; follow-up requests must NOT re-inject definitions into top-level arrays.
- **xt2.10 precondition:** the image-path `function_call_output` must not emit `input_text`-typed parts (spec §2 seam fold) — Task 5 closes it BEFORE any search output rides the channel.
- **Non-admitted routes** keep today's `search_tool → use_tool` behavior byte-for-byte (legacy dialects unchanged).
- **Commit discipline:** pathspec-limited commits; raw-key sweep (grep for credential patterns) MUST print 0 before every commit; message format `feat(responses): <scope> (apex-ayl.142)`.
- **Store:** `store: false` on every Responses body (H-4) — already tree-wide; tests must not regress it.
- **Live probes (bead apex-ayl.146, operator-run)** gate EXECUTION of Task 17's live arms, not the code tasks.

## File Structure (S3a decomposition — locked)

| file | responsibility (S3a) |
|---|---|
| `crates/codegen/xai-grok-sampling-types/src/conversation.rs` | `ToolExposure` enum + `ToolSpec.exposure` (T1); `ToolDiscovery`/`DiscoveredTool` + `ToolResultItem.discovery` (T12) |
| `crates/codegen/xai-grok-sampling-types/src/conversation/tool_search.rs` (NEW) | local type seam: 5 wire types + permissive decoders (T3) |
| `crates/codegen/xai-grok-sampling-types/src/conversation/responses.rs` | xt2.10 fix (T5); `tool_search` declaration + both placements (T6); deferred-aware `build_responses_tools` (T7); response→IR arms for search items (T8); `tool_search_output` IR (T10) |
| `crates/codegen/xai-grok-sampling-types/src/manifest.rs` (NEW) | `DiscoveryManifest` frozen epoch (T4) |
| `crates/codegen/xai-grok-models/src/lib.rs` | `DefaultModelEntry` +2 additive flags (T2) |
| `crates/codegen/xai-grok-models/{catalog_generated.json, catalog_overlay.json, default_models.json}` | row flag values: 7 ON / 3 OFF (T2) |
| `crates/codegen/xai-grok-shell/config.schema.json` | param-gate schema for the new row fields (T2) |
| `crates/codegen/xai-grok-shell/src/agent/config.rs` | row→runtime mapping of the new flags (T2) |
| `crates/codegen/xai-grok-tools/src/types/output.rs` | `SearchToolOutput` redefinition + `SearchStatus` + serde alias (T9) |
| `crates/codegen/xai-grok-tools/src/implementations/search_tool/mod.rs` | manifest-restricted search + structured output + exact-name bypass + D-ERR (T10/T11) |
| `crates/codegen/xai-grok-shell/src/session/acp_session_impl/tool_calls.rs` | dispatch arm for `tool_search_call`; construction seam attaches `discovery` (T10/T12) |
| `crates/codegen/xai-grok-shell/src/session/acp_session_impl/turn.rs` | manifest freeze at the capture boundary (T4) |
| `crates/codegen/xai-grok-sampler/src/stream/responses.rs` | preserve admitted-route search stream items (T13) |
| `crates/codegen/xai-grok-sampler/src/events.rs`, `shell/.../acp_session_impl/sampling_events.rs`, `shell/.../acp_session_impl/tool_dispatch.rs` | search event plumbing (T13) |
| `crates/codegen/xai-grok-pager/src/scrollback/blocks/tool/search_tool.rs`, `tool/mod.rs`, `pager/src/acp/tracker.rs` | mode-aware display (T14) |
| `crates/codegen/xai-grok-shell/src/session/{compaction.rs, goal_evaluator.rs}` + other literal sites | `ToolResultItem` literal updates for the new field (T12) — S3a rows only (test/fixture files listed per task) |
| `crates/codegen/xai-grok-sampling-types/src/conversation/outbound_lint.rs` + plans-corpus `HARDENING-SPEC.md` §2.1 | invariant obligations (T16) |
| `crates/codegen/xai-grok-shell/src/tests/redteam/hts_*.rs` (NEW) + probe-kit wiring | redteam suite + acceptance (T17) |
| `docs/responses-compat-seam.md` + `docs/FORK-MANIFEST.md` (NEW, worktree root docs/) | seam doc + fork manifest (T17) |

Note: files budgeted in spec §7 but NOT touched by S3a (they belong to S3b/S3c): `sampling-types/src/messages.rs`, `conversation/messages.rs`, `shell/.../mcp.rs`, `mcp_snapshot.rs` (read-only here), `helpers/replay.rs`, `implementations/use_tool/mod.rs`, and the Phase B variant blast-radius files. If a task discovers it needs one of them, STOP and raise it — that is a plan/spec boundary violation.

---

### Task 1: `ToolExposure` enum + `ToolSpec.exposure` field

**Files:**
- Modify: `crates/codegen/xai-grok-sampling-types/src/conversation.rs:814-820` (the `pub struct ToolSpec` at :814)
- Test: `crates/codegen/xai-grok-sampling-types/src/conversation/tests.rs` (append to the existing test module; if the file is `conversation_tests.rs` in-crate, append there — verify with `grep -rn "mod tests" crates/codegen/xai-grok-sampling-types/src/conversation.rs` first)

**Interfaces:**
- Consumes: nothing (foundational).
- Produces: `pub enum ToolExposure { Immediate, Deferred }` (serde default `Immediate`); `ToolSpec.exposure: ToolExposure` field. Every later task that constructs a `ToolSpec` literal must add `exposure: ToolExposure::default()` (the derive gives a `Default` — see test).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn tool_exposure_defaults_immediate_and_round_trips() {
    // Old serialized form (no `exposure` key) must deserialize as Immediate.
    let old_json = r#"{ "name": "read_file", "description": "d", "parameters": {} }"#;
    let spec: ToolSpec = serde_json::from_str(old_json).unwrap();
    assert_eq!(spec.exposure, ToolExposure::Immediate);

    // New form round-trips both variants.
    let mut d = spec.clone();
    d.exposure = ToolExposure::Deferred;
    let v = serde_json::to_value(&d).unwrap();
    assert_eq!(v["exposure"], "deferred"); // wire case = lowercase (crate convention; SDD ruling 2026-09-25 T1 — brief originally said "Deferred", contradiction ruled on)
    let back: ToolSpec = serde_json::from_value(v).unwrap();
    assert_eq!(back.exposure, ToolExposure::Deferred);
    assert_eq!(serde_json::to_value(back.exposure).unwrap(), "deferred");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p xai-grok-sampling-types tool_exposure_defaults_immediate_and_round_trips`
Expected: FAIL — `no field/variant named exposure` / `cannot find value ToolExposure`.

- [ ] **Step 3: Write minimal implementation**

In `conversation.rs`, directly above `pub struct ToolSpec`:

```rust
/// How a tool is exposed to the model on the Responses wire.
/// `Immediate` (default) = declared in `tools[]` today; `Deferred` =
/// discoverable only via native `tool_search` (S3a) / `tool_reference` (S3b).
/// Additive: old code and old history are untouched by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolExposure {
    #[default]
    Immediate,
    Deferred,
}
```

and add to `ToolSpec`:

```rust
    /// Exposure mode (additive; serde default Immediate — old JSONL unaffected).
    #[serde(default)]
    pub exposure: ToolExposure,
```

Then fix every `ToolSpec { ... }` struct literal in the workspace that breaks (expected: a small set — `grep -rn "ToolSpec {" crates/ --include=*.rs | grep -v test` to enumerate; add `exposure: ToolExposure::default(),` to each. These literals live in budgeted files only; if a break appears OUTSIDE the §7 budget, STOP and raise it.)

- [ ] **Step 4: Run tests and verify they pass**

Run: `cargo test -p xai-grok-sampling-types` then `cargo build --workspace` (workspace build catches the literal fixes across crates).
Expected: PASS, no errors.

- [ ] **Step 5: Commit**

```bash
git add crates/codegen/xai-grok-sampling-types/ && git add <other literal-fix files, pathspec-explicit>
git commit -m "feat(responses): ToolSpec.exposure additive enum (Immediate/Deferred) (apex-ayl.142)"
```

---

### Task 2: Model-catalog capability flags (`supports_search_tool`, `use_responses_lite`)

**Files:**
- Modify: `crates/codegen/xai-grok-models/src/lib.rs:66-92` (`DefaultModelEntry`)
- Modify: `crates/codegen/xai-grok-models/catalog_overlay.json` (curated row values — the SINGLE curation surface; per catalog_gate.py doc). `default_models.json` + `catalog_drift_report.json` are REGENERATED by the re-bake (Step 4) — never hand-edit them. **NEVER hand-edit `catalog_generated.json`** (endpoint snapshot from `scripts/catalog_generate.py`; the next generate run clobbers it).
- Modify: `crates/codegen/xai-grok-shell/config.schema.json` (param-gate schema for the two new fields — spec: schema OR STRUCT_ONLY_ALLOWED; schema is the chosen route)
- Modify: `crates/codegen/xai-grok-shell/src/agent/config.rs:4906-4914` (row→runtime mapping) + runtime fields at :5023/:5219
- Test: `crates/codegen/xai-grok-models/src/lib.rs` (existing `#[cfg(test)]` module) + `crates/codegen/xai-grok-shell/src/agent/config_tests.rs` if present (verify path with `ls crates/codegen/xai-grok-shell/src/agent/`)

**Interfaces:**
- Consumes: nothing.
- Produces: `DefaultModelEntry.supports_search_tool: Option<bool>`, `DefaultModelEntry.use_responses_lite: Option<bool>`; runtime fields on the agent config struct (`pub supports_search_tool: bool`, `pub use_responses_lite: bool`, both default `false`). Task 4 (gate) and Task 6 (placement) read these through the existing config plumbing.

**Row values (spec §2 re-keyed gate, grok catalog @ 2f49335e):** `supports_search_tool: true` for exactly: `gpt-5.6-sol`, `gpt-5.6-terra`, `gpt-5.6-luna`, `gpt-5.5`, `gpt-5.4`, `gpt-5.4-mini`, `gpt-5.2`. `false` for: `glm-5.2`, `grok-4.6`, `qwen3.8-27b`. All other rows: field ABSENT (serde default = off). `use_responses_lite: true` for the lite rows present in the grok catalog at the pin (the strict/azure-strict gpt-5.6-sol/terra/luna class — verify the exact set from `catalog_overlay.json` `strict_responses_input` rows at Step 1; the initial set is stated in the commit message).

- [ ] **Step 1: Verify the row space and the param-gate mechanism**

Run: `grep -n "gpt-5.6-sol\|gpt-5.5\|gpt-5.4\|gpt-5.2\|glm-5.2\|grok-4.6\|qwen3.8-27b" crates/codegen/xai-grok-models/catalog_generated.json` and `grep -n "STRUCT_ONLY_ALLOWED\|strict_responses_input" crates/codegen/xai-grok-models/src/param_gate_check.rs crates/codegen/xai-grok-models/build.rs | head`
Expected: the 10 named rows present; the fail-closed gate logic visible (new struct field without schema definition = build fail).

- [ ] **Step 2: Write the failing test**

```rust
#[test]
fn catalog_flags_parse_and_default_off() {
    let m: DefaultModels = serde_json::from_str(DEFAULT_MODELS_JSON).unwrap();
    let find = |id: &str| m.models.iter().find(|e| e.model == id);
    assert_eq!(find("gpt-5.6-sol").unwrap().supports_search_tool, Some(true));
    assert_eq!(find("gpt-5.2").unwrap().supports_search_tool, Some(true));
    assert_eq!(find("glm-5.2").unwrap().supports_search_tool, Some(false));
    assert_eq!(find("qwen3.8-27b").unwrap().supports_search_tool, Some(false));
    // absent-on-row = off by default
    let off_row = m.models.iter().find(|e| e.supports_search_tool.is_none()).expect("a row without the flag");
    assert_eq!(off_row.supports_search_tool.unwrap_or(false), false);
}
```

- [ ] **Step 3: Write minimal implementation**

`lib.rs` `DefaultModelEntry` (after `strict_responses_input`):

```rust
    /// Native hosted tool discovery (S3a): row advertises the model-side
    /// `tool_search` contract. Additive; absent = off.
    pub supports_search_tool: Option<bool>,
    /// Responses-lite declaration placement (tools ride a leading
    /// `additional_tools` input item, not top-level `tools`). Additive; absent = off.
    pub use_responses_lite: Option<bool>,
```

Catalog JSON: add the 10 rows' values to `catalog_overlay.json` ONLY (overlay rows are full-row curation entries — keep that contract; schema field order follows existing row style). Do not touch `catalog_generated.json`. `config.schema.json`: add the two properties to the model-entry schema (boolean, optional) mirroring the `strict_responses_input` entry. `agent/config.rs` :4906-4914: extend the mapping pattern (`row.supports_search_tool.unwrap_or(false)` / `row.use_responses_lite.unwrap_or(false)`) into the runtime fields at :5023/:5219.

- [ ] **Step 4: Run tests and verify they pass (re-bake + merge + build gates)**

Run, in order:
1. `cargo test -p xai-grok-models` (the new flag test).
2. Re-bake the catalog (catalog_gate.py MERGES generated + overlay IN PLACE into default_models.json and re-snapshots the drift report):
   `scripts/catalog_gate.py --generated crates/codegen/xai-grok-models/catalog_generated.json --overlay crates/codegen/xai-grok-models/catalog_overlay.json --schema crates/codegen/xai-grok-shell/config.schema.json --out crates/codegen/xai-grok-models/default_models.json`
   Expected: exit 0; drift report re-snapshotted (the 10 touched rows appear as curated; no new `removed_but_curated`/`alerted` beyond the pre-existing set).
3. `scripts/catalog_merge_tests.py` — Expected: ALL PASS (the merge contract, incl. overlay-beats-generated).
4. `grep -c "supports_search_tool" crates/codegen/xai-grok-models/default_models.json` — the fields MUST be present in the BAKED artifact (in the no-endpoint/apex-release build the baked catalog is the fleet's only config surface — this grep is the release-surface proof; a release rebuild is NOT in this task's scope).
5. `cargo build --workspace` (the build.rs param gate runs here — struct field <-> schema property fail-closed in both directions; baked row keys stay inside the schema surface).
Expected: all PASS / green.

- [ ] **Step 5: Commit**

```bash
git add crates/codegen/xai-grok-models/ crates/codegen/xai-grok-shell/config.schema.json crates/codegen/xai-grok-shell/src/agent/config.rs
git commit -m "feat(responses): model-catalog capability flags supports_search_tool/use_responses_lite (7 ON / 3 OFF / rest absent) (apex-ayl.142)"
```

---

### Task 3: Local type seam — the 5 new wire types (INTEGRATION POINT A default)

**Files:**
- Create: `crates/codegen/xai-grok-sampling-types/src/conversation/tool_search.rs`
- Modify: `crates/codegen/xai-grok-sampling-types/src/conversation/mod.rs` (or wherever the `conversation` submodule is declared — verify with `grep -n "pub mod" crates/codegen/xai-grok-sampling-types/src/conversation.rs crates/codegen/xai-grok-sampling-types/src/lib.rs | head`)
- Test: in-module `#[cfg(test)]` in the new file

**Interfaces:**
- Consumes: `ToolExposure` (T1).
- Produces (exact names — later tasks rely on these):
  - `pub struct ToolSearchDeclaration { pub tool_type: &'static str /* "tool_search" */, pub execution: &'static str /* "client" */, pub description: String, pub parameters: serde_json::Value }` with `pub fn json(&self) -> serde_json::Value`
  - `pub struct ToolSearchCall { pub item_id: Option<String>, pub call_id: Option<String>, pub execution: Option<String>, pub arguments: serde_json::Value }`
  - `pub struct ToolSearchOutputItem { pub item_id: Option<String>, pub call_id: String, pub status: String /* "completed" | "error" */, pub execution: String /* "client" */, pub tools: Vec<serde_json::Value>, pub error: Option<String> }`
  - `pub struct AdditionalToolsItem { pub item_id: String /* "at_<uuid>" */, pub role: &'static str /* "developer" */, pub tools: Vec<serde_json::Value> }`
  - Permissive decode rule (goose lesson): `item_id`/`status` are `Option` on ALL decoders; a decode never fails on their absence.
  - Wire serialization targets the existing `rs::Item`/`rs::InputItem` containers via a raw-JSON escape hatch (the seam serializes into the request body JSON directly — the pinned async-openai enum has no variants for these types; the encoder inserts the JSON values at the `input` array position). This is the whole reason for the local seam: zero dependency churn, full byte control.

- [ ] **Step 1: Write the failing test (the ten probe assertions become unit tests — spec §2 type boundary)**

```rust
#[test]
fn tool_search_declaration_is_byte_exact() {
    let d = ToolSearchDeclaration {
        tool_type: "tool_search",
        execution: "client",
        description: "Search for deferred tools.".to_string(), // dynamic in production
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "limit": {"type": "number", "description": "Maximum number of tools to return. Defaults to 8."},
                "query": {"type": "string", "description": "Search query for deferred tools."}
            },
            "required": ["query"],
            "additionalProperties": false
        }),
    };
    let v = d.json();
    assert_eq!(v["type"], "tool_search");
    assert_eq!(v["execution"], "client");
    // LEXICOGRAPHIC property order on the wire (BTreeMap): limit before query.
    let keys: Vec<&str> = v["parameters"]["properties"].as_object().unwrap().keys().map(|k| k.as_str()).collect();
    assert_eq!(keys, vec!["limit", "query"]);
    assert_eq!(v["parameters"]["properties"]["limit"]["type"], "number");
    assert_eq!(v["parameters"]["properties"]["limit"]["description"], "Maximum number of tools to return. Defaults to 8.");
}

#[test]
fn permissive_decoders_never_fail_on_missing_id_status() {
    let v = serde_json::json!({"type": "tool_search_call", "call_id": "c1", "execution": "client", "arguments": {}});
    let c: ToolSearchCall = serde_json::from_value(v).unwrap();
    assert!(c.item_id.is_none() && c.status_or_default_is_client_execution());
    let v2 = serde_json::json!({"type": "tool_search_output", "call_id": "c1", "tools": []});
    let o: ToolSearchOutputItem = serde_json::from_value(v2).unwrap();
    assert!(o.item_id.is_none() && o.error.is_none());
}
```

(Note: `status_or_default_is_client_execution` = a small helper asserting `execution.as_deref() == Some("client")`; if you prefer, inline the assertion — the point is NO decode failure on missing optionals.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p xai-grok-sampling-types tool_search`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Write minimal implementation**

New module: the four structs above with `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]`, `#[serde(default)]` on the optionals, `rename_all = "snake_case"` where the wire is snake_case, and explicit `type`-discriminant handling via `#[serde(tag = "type", rename_all = "snake_case")]` on a local enum `ToolSearchWire { ToolSearch(ToolSearchDeclaration), ToolSearchCall(ToolSearchCall), ToolSearchOutput(ToolSearchOutputItem), AdditionalTools(AdditionalToolsItem) }` used by decoders. `ToolSearchDeclaration::json()` builds the BTreeMap-ordered parameters (use `serde_json::Map` built in sorted order or `BTreeMap` so serialization is lexicographic). The raw-JSON insertion helper: `pub fn insert_into_input(input: &mut Vec<serde_json::Value>, at: usize, value: serde_json::Value)`.

- [ ] **Step 4: Run tests and verify they pass**

Run: `cargo test -p xai-grok-sampling-types`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/codegen/xai-grok-sampling-types/src/conversation/
git commit -m "feat(responses): local type seam for tool_search wire types (5 types, permissive decoders, no async-openai churn) (apex-ayl.142)"
```

---

### Task 4: `DiscoveryManifest` — frozen admitted-catalog epoch

**Files:**
- Create: `crates/codegen/xai-grok-sampling-types/src/manifest.rs`
- Modify: `crates/codegen/xai-grok-shell/src/session/acp_session_impl/turn.rs:2568` area (the `prepare_tool_definitions_timed` capture boundary — freeze HERE, at the turn's tool-prep point)
- Modify: `crates/codegen/xai-grok-shell/src/session/acp_session_impl/mcp_snapshot.rs` (read the published `ToolMetadataSnapshot` AFTER `mcp_initialized`; expose a `published_snapshot_at_init_complete()` accessor if none exists — verify with `grep -n "mcp_initialized" crates/codegen/xai-grok-shell/src/session/acp_session_impl/mcp_snapshot.rs`)
- Test: in-module tests in `manifest.rs` + a turn-level test in the existing turn test file (locate with `ls crates/codegen/xai-grok-shell/src/session/acp_session_impl/*tests*`)

**Interfaces:**
- Consumes: `ToolMetadataSnapshot` (existing), `mcp_initialized` flag (existing), `wait_for_mcp_initialized_bounded_once` (existing).
- Produces:
  - `pub struct DiscoveredToolDef { pub tool_name: String, pub schema_version: String, pub definition: ToolSpec }`
  - `pub struct DiscoveryManifest { pub catalog_epoch: String /* deterministic: hash of admitted set */, pub tools: Vec<DiscoveredToolDef>, pub deferred_count: usize }`
  - `impl DiscoveryManifest { pub fn is_empty(&self) -> bool; pub fn contains(&self, name: &str) -> bool }`
  - Freeze semantics: built exactly once per turn at the capture boundary; `Arc<DiscoveryManifest>` (or `Option<Arc<_>>`) threaded into the request builder; retries of the same turn reuse the SAME instance (no re-read).
  - **Timeout path:** if `wait_for_mcp_initialized_bounded_once` timed out for this turn (`full_mcp_wait_timed_out()`), freeze is SKIPPED — manifest = `None`, gate input (b) fails, legacy fallback for the turn.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn manifest_freezes_once_and_is_retry_stable() {
    let snap = fake_snapshot(vec![("server__a".into(), tool_spec("server__a", ToolExposure::Deferred)),
                                  ("read_file".into(), tool_spec("read_file", ToolExposure::Immediate))]);
    let m1 = DiscoveryManifest::freeze(&snap);
    let m2 = DiscoveryManifest::freeze(&snap);
    assert_eq!(m1.catalog_epoch, m2.catalog_epoch); // deterministic
    assert_eq!(m1.deferred_count, 1);
    assert!(m1.contains("server__a") && !m1.contains("read_file"));
}

#[test]
fn manifest_timeout_yields_none() {
    assert!(DiscoveryManifest::freeze_if_ready(&fake_snapshot(vec![]), /* mcp_initialized */ false).is_none());
    assert!(DiscoveryManifest::freeze_if_ready(&fake_snapshot(vec![]), /* mcp_initialized */ true).is_some());
}
```

(`freeze_if_ready(snap, mcp_initialized)` is the turn-facing entry; `freeze` = the pure builder used by tests.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p xai-grok-sampling-types manifest_`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Write minimal implementation**

`manifest.rs`: the two structs + `freeze_if_ready` (returns `None` when `!mcp_initialized`) + `freeze` (pure: filters `exposure == Deferred` with searchable metadata, computes `catalog_epoch` as a hex FNV-1a over the sorted `(tool_name, schema_version)` pairs — the crate already uses FNV-1a for portability, see `search_tool/mod.rs` `hash_value`). `turn.rs`: at the capture boundary (next to `prepare_tool_definitions_timed`, :2568), call `freeze_if_ready` on the published snapshot (read via the `mcp_snapshot` accessor; the snapshot's `mcp_initialized` field is the gate), store `Option<Arc<DiscoveryManifest>>` on the turn state, and pass it into request construction. Retries must read the stored value, never re-freeze (add a regression test: two encode calls in the same turn produce identical `catalog_epoch`).

- [ ] **Step 4: Run tests and verify they pass**

Run: `cargo test -p xai-grok-sampling-types -p xai-grok-shell manifest_ && cargo test -p xai-grok-shell turn_`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/codegen/xai-grok-sampling-types/src/manifest.rs crates/codegen/xai-grok-shell/src/session/acp_session_impl/turn.rs crates/codegen/xai-grok-shell/src/session/acp_session_impl/mcp_snapshot.rs
git commit -m "feat(responses): DiscoveryManifest frozen epoch (freeze at mcp_initialized; timeout -> None -> legacy) (apex-ayl.142)"
```

---

### Task 5: xt2.10 precondition — no `input_text` parts in `function_call_output` on the image path

**Files:**
- Modify: `crates/codegen/xai-grok-sampling-types/src/conversation/responses.rs:351-377` (the `ConversationItem::ToolResult` arm of `conversation_item_to_input_items`)
- Test: the existing responses conversion test file (locate: `ls crates/codegen/xai-grok-sampling-types/src/conversation/*test*`)

**Interfaces:**
- Consumes: `ToolResultItem` (unchanged), `rs::FunctionCallOutput::{Text, Content}` (existing).
- Produces: the corrected lowering — text-only results keep `Text(string)`; image results carry `Content(parts)` where parts = image parts ONLY (no leading `InputText`). This is a PREREQUISITE for `tool_search_output` riding the same output channel on shimmed routes (seam map PASS 2 gap). Legacy routes (non-admitted) must remain byte-identical — see Step 3 scope rule.

- [ ] **Step 1: Write the failing test (golden of the current bad shape first, then the fix)**

```rust
#[test]
fn tool_result_image_path_has_no_input_text_part() {
    let item = ConversationItem::ToolResult(ToolResultItem {
        tool_call_id: "c1".into(),
        content: "look at this".into(),
        images: vec![ContentPart::Image { url: "file://x.png".into() }],
        is_error: false,
    });
    let items = conversation_item_to_input_items(&item);
    // The output array must contain ONLY image parts on the image path.
    if let rs::InputItem::Item(rs::Item::FunctionCallOutput(f)) = &items[0] {
        match &f.output {
            rs::FunctionCallOutput::Content(parts) => {
                for p in parts {
                    assert!(matches!(p, rs::InputContent::InputImage(_)),
                        "no InputText allowed in output array: {p:?}");
                }
                assert!(!parts.is_empty());
            }
            other => panic!("expected Content, got {other:?}"),
        }
    } else { panic!("expected FunctionCallOutput") }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p xai-grok-sampling-types tool_result_image_path_has_no_input_text_part`
Expected: FAIL — the first part is `InputText` (the xt2.10 shape at :356-360).

- [ ] **Step 3: Write minimal implementation**

In the `ToolResult` arm (:351-367): when `t.images` is non-empty, build `parts` from the images ONLY (drop the leading `InputText` push at :356-360); the text content is not lost — it rides the result's textual channel elsewhere in the same item (verify: `FunctionCallOutputItemParam` has no text field when output is `Content` — if the text would be dropped entirely, carry it as the LAST part via the existing textual mechanism the donor's pass-2 uses: relabel-to-`text` is a SHIM concern; for this cut the donor-compatible minimal form is image-only parts — confirm against the donor pass-2 `content_type_compat.rs:94-152` behavior via the donor contract §3, and record the choice in the commit message). Scope rule: this arm serves ALL routes (it is the single lowering) — the non-admitted-route byte-identity constraint is satisfied because the OLD shape was the bug (no golden pins the buggy shape; if a wire golden does pin it, update that golden and note it in the commit message — the golden change is in-budget via `conversation/responses.rs`).

- [ ] **Step 4: Run tests and verify they pass**

Run: `cargo test -p xai-grok-sampling-types` (full crate — catches any golden that pinned the old shape)
Expected: PASS (or the noted golden updates applied).

- [ ] **Step 5: Commit**

```bash
git add crates/codegen/xai-grok-sampling-types/src/conversation/responses.rs <golden files if any>
git commit -m "fix(responses): xt2.10 precondition — image-path tool results no longer emit input_text parts in the output array (apex-ayl.142)"
```

---

### Task 6: `tool_search` declaration + both placements

> **BLOCKED — see Amendment A-1 (D3).** Live probe R4 proves the lite placement silently defeats hosted search on the exact rows that carry both flags. Do not implement dual placement until D3 is decided.

**Files:**
- Modify: `crates/codegen/xai-grok-sampling-types/src/conversation/responses.rs` (`build_responses_tools` :454 + the request-encoding path that assembles `input` — locate the encode entry with `grep -n "fn.*responses.*request\|pub fn build_responses" crates/codegen/xai-grok-sampling-types/src/conversation/responses.rs | head`)
- Test: conversion test file (same as T5)

**Interfaces:**
- Consumes: `ToolSearchDeclaration` (T3), `DiscoveryManifest` (T4), catalog flags `supports_search_tool`/`use_responses_lite` (T2), thread id (available on the request context — verify with `grep -n "thread_id\|session_id" crates/codegen/xai-grok-sampling-types/src/conversation/responses.rs | head`).
- Produces: on ADMITTED routes (gate open, T10's gate helper — until then this code is inert): (a) the declaration JSON appended to the tools set; (b) lite rows: ALL tools (incl. declaration) move into a leading `AdditionalToolsItem` with the two-stage v5 id (`at_` suffix, NAMESPACE_OID/thread_id prefix-namespace, `v5(prefix_ns, serde_json(tools))`); non-lite rows: declaration joins top-level `tools`. Non-admitted routes: NOTHING changes (the gate is the only entry — verified by T17 legacy goldens).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn declaration_placement_lite_vs_full() {
    let manifest = fake_manifest(deferred_only);
    // full (non-lite): declaration joins top-level tools; input unchanged.
    let req = admitted_request(&manifest, /* use_responses_lite */ false, "thread-1");
    let encoded = encode_for_test(&req);
    assert_eq!(encoded["tools"].as_array().unwrap().iter().filter(|t| t["type"] == "tool_search").count(), 1);
    assert!(encoded["input"].as_array().unwrap().iter().all(|i| i.get("type").is_none() || i["type"] != "additional_tools"));
    // lite: leading additional_tools item carries ALL tools incl. declaration; top-level tools absent
    let req = admitted_request(&manifest, /* use_responses_lite */ true, "thread-1");
    let encoded = encode_for_test(&req);
    let first = &encoded["input"].as_array().unwrap()[0];
    assert_eq!(first["type"], "additional_tools");
    assert!(first["id"].as_str().unwrap().starts_with("at_"));
    // two-stage id determinism: same thread + same tools -> same id
    let encoded2 = encode_for_test(&req);
    assert_eq!(first["id"], encoded2["input"][0]["id"]);
}
```

(`encode_for_test` = a test-only harness that runs the same encode path; if the encode function is not directly callable, test at the lowest callable boundary and note it in the test doc-comment.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p xai-grok-sampling-types declaration_placement_lite_vs_full`
Expected: FAIL — no declaration emitted, no additional_tools handling.

- [ ] **Step 3: Write minimal implementation**

- Gate helper (shared with T10): `fn admitted_for_search(flags: &RouteFlags, manifest: &Option<Arc<DiscoveryManifest>>) -> bool` — `flags.supports_search_tool && manifest.as_ref().is_some_and(|m| !m.is_empty())`. (RouteFlags = the T2 runtime fields, threaded through the request context — add the plumbing at the encode entry; keep it in `responses.rs` or a sibling `gate.rs` if the file grows — preference: sibling, in-budget as part of `conversation/responses.rs`'s module.)
- Declaration: build `ToolSearchDeclaration` with the DYNAMIC description (source listing from the manifest's servers — reuse the donor's interpolation shape per donor contract §2.2: `- <source>: <description>` lines, 512 KiB budget, names always kept) and the byte-exact parameters (T3's `json()`).
- Placement: at the encode entry, when `admitted_for_search`: lite rows → assemble `tools` JSON array (existing `build_responses_tools` output + declaration), compute the two-stage id, prepend `AdditionalToolsItem` to `input`, and clear the top-level tools field; non-lite → push the declaration into the top-level tools array. Use `uuid` crate v5 (`Uuid::new_v5`) — verify the dep exists (`grep -n "uuid" crates/codegen/xai-grok-sampling-types/Cargo.toml`; if absent at this crate, do the v5 derivation where `uuid` IS a dep and pass the resulting `String` ids in — do NOT add a new dependency).

- [ ] **Step 4: Run tests and verify they pass**

Run: `cargo test -p xai-grok-sampling-types`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/codegen/xai-grok-sampling-types/src/conversation/
git commit -m "feat(responses): tool_search declaration + dual placement (top-level vs leading additional_tools, two-stage at_ id) (apex-ayl.142)"
```

---

### Task 7: Deferred-aware `build_responses_tools`

**Files:**
- Modify: `crates/codegen/xai-grok-sampling-types/src/conversation/responses.rs:454` (`build_responses_tools`)
- Test: conversion test file

**Interfaces:**
- Consumes: `ToolSpec.exposure` (T1), gate helper (T6), `DiscoveryManifest` (T4).
- Produces: on admitted routes: deferred tools are EXCLUDED from the direct tools set (they ride the search index + manifest only); the search declaration is present (T6); invariant: the direct set always contains ≥1 non-deferred tool (the search tool itself is always direct — if the manifest is empty the gate is closed, so the invariant holds by construction; assert it in tests). Non-admitted: byte-identical to today (deferred is not a concept there — `exposure` exists but no route acts on it).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn deferred_tools_excluded_only_on_admitted_routes() {
    let tools = vec![tool_spec("read_file", ToolExposure::Immediate),
                     tool_spec("server__deploy", ToolExposure::Deferred)];
    // non-admitted: both present, no search declaration
    let out = build_for_test(&tools, &gate_closed());
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|t| t.get("type").is_none()));
    // admitted: deferred excluded, declaration present
    let out = build_for_test(&tools, &gate_open());
    assert!(out.iter().all(|t| t.get("name").and_then(|n| n.as_str()) != Some("server__deploy")));
    assert!(out.iter().any(|t| t.get("type").and_then(|t| t.as_str()) == Some("tool_search")));
}

#[test]
fn direct_set_never_empty_when_gate_open() {
    // manifest non-empty implies the gate is open and the declaration is direct
    // -> the direct set always has >=1 entry.
    let out = build_for_test(&vec![], &gate_open_with_manifest(1));
    assert!(!out.is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p xai-grok-sampling-types deferred_tools_excluded`
Expected: FAIL — filter does not consider `exposure`.

- [ ] **Step 3: Write minimal implementation**

Extend the `build_responses_tools` filter (:458-466): when the gate is open (thread the `Option<Arc<DiscoveryManifest>>` + flags into the function signature — this is a private fn; the encode entry is the caller), drop `exposure == Deferred` tools from the direct set and append the declaration. Keep the existing hosted-tool collision filter untouched.

- [ ] **Step 4: Run tests and verify they pass**

Run: `cargo test -p xai-grok-sampling-types`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/codegen/xai-grok-sampling-types/src/conversation/responses.rs
git commit -m "feat(responses): deferred-aware build_responses_tools (exclusion only on admitted routes; direct-set invariant) (apex-ayl.142)"
```

---

### Task 8: Response→IR arms for search items

**Files:**
- Modify: `crates/codegen/xai-grok-sampling-types/src/conversation/responses.rs` (the response-output→IR conversion — locate with `grep -n "fn.*output.*item\|ResponseOutputItem\|OutputItem::" crates/codegen/xai-grok-sampling-types/src/conversation/responses.rs | head`)
- Test: conversion test file

**Interfaces:**
- Consumes: the T3 wire types (decode side), `BackendToolCallItem::CodexRawInput` (existing catch-all — server-executed / null-call-id search items are preserved verbatim through it; NO new `ConversationItem` variant in S3a).
- Produces:
  - client-executed `tool_search_call` (non-null call_id) → surfaced to dispatch as a structured `ToolSearchCall` (the dispatcher consumes this via the stream/event path, T10/T13 — the IR keeps the raw item AND the parsed view).
  - server-executed (`execution: "server"`) or null-call-id items → `BackendToolCallItem::CodexRawInput(raw)` preserved verbatim, NEVER dispatched locally, round-tripped on the next request.
  - unknown/other new item types: preserved verbatim via the same catch-all (the no-silent-drop rule for admitted routes — the legacy `_ => {}` stream drop is fixed in T13; at the IR layer the raw item is always retained).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn search_call_client_executed_vs_server_preserved() {
    let client = serde_json::json!({"type": "tool_search_call", "id": "i1", "call_id": "c1", "execution": "client", "arguments": {"query": "deploy"}});
    let ir = output_to_ir(&client);
    assert!(ir.is_search_call_client_executed()); // parsed view with call_id "c1"
    let server = serde_json::json!({"type": "tool_search_call", "id": "i2", "call_id": "c2", "execution": "server", "arguments": {"query": "x"}});
    let ir2 = output_to_ir(&server);
    assert!(ir2.is_raw_preserved()); // CodexRawInput, never dispatchable
    let nullid = serde_json::json!({"type": "tool_search_call", "id": "i3", "arguments": {}});
    assert!(output_to_ir(&nullid).is_raw_preserved());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p xai-grok-sampling-types search_call_client_executed`
Expected: FAIL — no arm for these item types.

- [ ] **Step 3: Write minimal implementation**

Add the decode arms: `ToolSearchWire` (T3) discriminator → client-executed with non-null call_id = parsed `ToolSearchCall` view; everything else in the 5-type set = `CodexRawInput` verbatim. The raw item is cloned into the IR so the next request re-encodes it byte-identically (round-trip property — add a round-trip assert to the test).

- [ ] **Step 4: Run tests and verify they pass**

Run: `cargo test -p xai-grok-sampling-types`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/codegen/xai-grok-sampling-types/src/conversation/responses.rs
git commit -m "feat(responses): response->IR arms for tool_search items (client-exec parsed; server/null-id preserved verbatim via CodexRawInput) (apex-ayl.142)"
```

---

### Task 9: `SearchToolOutput` redefinition + `SearchStatus`

**Files:**
- Modify: `crates/codegen/xai-grok-tools/src/types/output.rs:594-597` (struct) + `:616` (variant unchanged) + `:1306` (trait impl)
- Modify (construction sites, verified by cf2/cf3): `crates/codegen/xai-grok-tools/src/util/mcp_truncate.rs:463`, `crates/codegen/xai-grok-tools/src/implementations/search_tool/mod.rs:344`, `crates/codegen/xai-grok-shell/src/session/acp_session_impl/tool_layer_images.rs:187`, `crates/codegen/xai-grok-shell/src/session/telemetry/active_agent_message_tests.rs:183`
- Test: `crates/codegen/xai-grok-tools/src/types/output_tests.rs` if present, else in-crate tests

**Interfaces:**
- Consumes: `DiscoveredToolDef` (T4 — the `results` entries).
- Produces (exact — T10/T11/T12 rely on these):
  - `pub enum SearchStatus { Completed, Error }` (serde lowercase: `"completed"`/`"error"`)
  - `pub struct SearchToolOutput { pub results: Vec<DiscoveredToolDef>, pub total_hidden_tools: usize, pub status: SearchStatus, pub legacy_content: String }`
  - **Compatibility:** the old serialized form `{ "result_count": N, "content": "..." }` MUST still deserialize (pager parses the legacy `content` JSON at `tracker.rs:2755`). Implement with `#[serde(default)]` on all new fields + a `Deserialize` impl (or `#[serde(alias)]`-free manual visitor) that maps old `content` → `legacy_content`, old `result_count` → `results.len()`-compatible default (`results: vec![]`). Serialization emits the NEW form only.
  - `legacy_content` = the old human-readable text (kept for fallback-route rendering; T14 renders it there).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn search_tool_output_old_json_still_parses_new_shape_round_trips() {
    let old = r#"{ "type": "SearchTool", "result_count": 3, "content": "Found 3 tools: a, b, c" }"#;
    let out: ToolOutput = serde_json::from_str(old).unwrap();
    let st = match &out { ToolOutput::SearchTool(s) => s, _ => panic!() };
    assert_eq!(st.legacy_content, "Found 3 tools: a, b, c");
    assert!(st.results.is_empty() && st.status == SearchStatus::Completed);
    // new shape round-trip
    let mut s2 = st.clone();
    s2.results = vec![discovered_def("server__a", "v1")];
    s2.total_hidden_tools = 12;
    let v = serde_json::to_value(ToolOutput::SearchTool(s2)).unwrap();
    assert_eq!(v["results"].as_array().unwrap().len(), 1);
    assert_eq!(v["status"], "completed");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p xai-grok-tools search_tool_output_old_json`
Expected: FAIL — unknown fields / missing `SearchStatus`.

- [ ] **Step 3: Write minimal implementation**

The new struct + `SearchStatus` + a custom `impl<'de> Deserialize<'de> for SearchToolOutput` (deserialize into a helper with all-optional fields incl. legacy `result_count`/`content`; fold legacy into the new shape). Update the 4 construction sites to the new fields (each site currently sets `result_count`/`content` — set `results`/`total_hidden_tools`/`status: Completed`/`legacy_content` from the same values they compute today; `mcp_truncate.rs:463` truncates `legacy_content` as it truncates `content` today).

- [ ] **Step 4: Run tests and verify they pass**

Run: `cargo test -p xai-grok-tools -p xai-grok-shell` (the telemetry test file is in shell)
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/codegen/xai-grok-tools/ crates/codegen/xai-grok-shell/src/session/acp_session_impl/tool_layer_images.rs crates/codegen/xai-grok-shell/src/session/telemetry/
git commit -m "feat(responses): SearchToolOutput redefinition (results/status/legacy_content; old JSON still parses via compat visitor) (apex-ayl.142)"
```

---

### Task 10: Search dispatch — manifest-restricted BM25 + structured results

**Files:**
- Modify: `crates/codegen/xai-grok-tools/src/implementations/search_tool/mod.rs` (the search path — `SEARCH_TOOL_NAME` :15, `ToolIndex` usage :7)
- Modify: `crates/codegen/xai-grok-shell/src/session/acp_session_impl/tool_calls.rs` (dispatch arm for the parsed `ToolSearchCall` from T8; the existing legacy `search_tool` function dispatch stays for non-admitted routes)
- Test: `crates/codegen/xai-grok-tools/src/implementations/search_tool/mod.rs` in-crate tests + `tool_calls` test file

**Interfaces:**
- Consumes: `ToolSearchIndex::search_snapshot(query, limit) -> SearchSnapshot` (existing trait, `crates/codegen/xai-grok-tools/src/types/tool_index.rs:58`), `DiscoveryManifest` (T4), `SearchToolOutput` (T9), `ToolExposure` (T1).
- Produces:
  - `AdmittedManifestIndex` — a thin wrapper implementing `ToolSearchIndex` that delegates to the underlying index and FILTERS results to manifest members (names + schema versions). The dispatcher constructs it from (underlying index, manifest) when the gate is open; otherwise the legacy path runs untouched.
  - Dispatch arm: parsed client-executed `ToolSearchCall` → parse args (`{ query, limit? }`; default limit 8) → `AdmittedManifestIndex::search_snapshot` → **exact-name bypass** (campaign divergence: a query that exactly equals a manifest member's name/qualified-name returns that single result, ignoring `limit`; first-snapshot-hit-wins on cross-server bare-name collisions — golden-pinned) → `SearchToolOutput { results, total_hidden_tools: manifest.deferred_count, status: Completed, legacy_content: <legacy text rendering of results, kept for T14 fallback> }` → the `tool_search_output` wire item (T11 shape) is emitted on the NEXT request's input by the encoder (T6's placement rules apply to outputs too — coalesced with parallel calls).
  - Zero matches → `results: []`, `status: Completed`, no fallback text (legacy_content empty).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn admitted_search_is_manifest_restricted_with_bypass_and_zero_match() {
    let idx = fake_index(entries: [
        ("server__deploy", "Deploy the app"),          // in manifest
        ("server__internal", "Hidden helper"),         // NOT in manifest
        ("other__deploy", "Another deploy"),           // in manifest, bare-name collision candidate
    ]);
    let manifest = fake_manifest(vec!["server__deploy", "other__deploy"]);
    let wrapped = AdmittedManifestIndex::new(idx.clone(), &manifest);
    let snap = wrapped.search_snapshot("internal", 8);
    assert!(snap.results.is_empty() == false || true); // shape check below is load-bearing:
    assert!(snap.results.iter().all(|r| manifest.contains(&r.name)));
    // exact-name bypass: single result, limit ignored
    let snap2 = wrapped.search_snapshot("server__deploy", 1);
    assert_eq!(snap2.results.len(), 1);
    assert_eq!(snap2.results[0].name, "server__deploy");
    // zero match
    let snap3 = wrapped.search_snapshot("zzz-no-such", 8);
    assert!(snap3.results.is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p xai-grok-tools admitted_search_is_manifest_restricted`
Expected: FAIL — `AdmittedManifestIndex` does not exist.

- [ ] **Step 3: Write minimal implementation**

`AdmittedManifestIndex` (new file `search_tool/manifest_index.rs` or in `mod.rs` — preference: separate file, in-budget under the `search_tool` module): wraps `Arc<dyn ToolSearchIndex>` + `Arc<DiscoveryManifest>`; `search_snapshot` = delegate → filter to `manifest.contains(name)` → if the raw query (or its underscore/space forms) exactly equals a surviving entry's name, return `vec![that entry]` (bypass); `list_server_summaries` = delegate filtered to manifest servers. `tool_calls.rs`: new dispatch arm — when the incoming item is a parsed client-executed `ToolSearchCall` and the gate is open: parse args (bad args → T11 D-ERR), run the wrapped index, build `SearchToolOutput`, push the durable result item (T12 helper) and queue the wire output for the next request. Legacy `search_tool` function-name dispatch: unchanged for non-admitted routes.

- [ ] **Step 4: Run tests and verify they pass**

Run: `cargo test -p xai-grok-tools -p xai-grok-shell`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/codegen/xai-grok-tools/src/implementations/search_tool/ crates/codegen/xai-grok-shell/src/session/acp_session_impl/tool_calls.rs
git commit -m "feat(responses): manifest-restricted search dispatch (AdmittedManifestIndex, exact-name bypass, zero-match tools:[]) (apex-ayl.142)"
```

---

### Task 11: D-ERR error channel

> **AMENDED — see A-2.** Orphan/stale discovery output is a provider HTTP 400 (R5), not an in-band D-ERR. Prevent orphan emission at encode; map the 400 as a transport error.

**Files:**
- Modify: `crates/codegen/xai-grok-tools/src/implementations/search_tool/mod.rs` (arg-parse failure path)
- Modify: `crates/codegen/xai-grok-sampling-types/src/conversation/tool_search.rs` (the `error` field — already in T3's struct; add the builder)
- Test: in-crate tests

**Interfaces:**
- Consumes: `ToolSearchOutputItem` (T3), `SearchStatus::Error` (T9).
- Produces:
  - `ToolSearchOutputItem::error_result(call_id: &str, error: &str) -> Self` — `status: "error"`, `tools: vec![]`, `error: Some(error)`, `execution: "client"`, `call_id` = the REAL call id (never empty).
  - Dispatch wiring (T10's arm): unparseable search args → `error_result` (message names the parse failure); NO `error_or_panic`-class behavior (a parse failure NEVER panics the turn); the item is persisted (T12) and rides the next request (pairing holds by construction — real id ∈ tool-search call set, so the repair orphan-pass is a no-op; T15).
  - Contingency (spec): if a strict deployment rejects the `error` FIELD on ingest (a 400 naming it), the fallback is STATUS-ONLY (`status:"error"`, `tools:[]`, no `error` field) — implemented as `error_result_status_only(call_id)`; the live probe arm (T17) surfaces which form the route accepts.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn d_err_channel_never_emits_empty_id() {
    let item = ToolSearchOutputItem::error_result("call-123", "invalid arguments: missing query");
    assert_eq!(item.call_id, "call-123");
    assert_eq!(item.status, "error");
    assert!(item.tools.is_empty());
    assert_eq!(item.error.as_deref(), Some("invalid arguments: missing query"));
    let v = serde_json::to_value(&item).unwrap();
    assert_eq!(v["call_id"], "call-123");
    assert_eq!(v["status"], "error");
    // status-only contingency
    let s = ToolSearchOutputItem::error_result_status_only("call-123");
    let vs = serde_json::to_value(&s).unwrap();
    assert!(vs.get("error").is_none() || vs["error"].is_null());
    assert_eq!(vs["status"], "error");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p xai-grok-sampling-types d_err_channel`
Expected: FAIL — builders do not exist.

- [ ] **Step 3: Write minimal implementation**

The two builders in `tool_search.rs`; the dispatch arm in `tool_calls.rs` calls `error_result` on arg-parse failure (search scope only — the general RespondToModel path is untouched). `#[serde(skip_serializing_if = "Option::is_none")]` on `error` makes the status-only form drop the field.

- [ ] **Step 4: Run tests and verify they pass**

Run: `cargo test -p xai-grok-sampling-types -p xai-grok-tools -p xai-grok-shell`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/codegen/xai-grok-sampling-types/src/conversation/tool_search.rs crates/codegen/xai-grok-shell/src/session/acp_session_impl/tool_calls.rs
git commit -m "feat(responses): D-ERR error channel (status:error + error text, real call_id, no empty ids, no panic class) (apex-ayl.142)"
```

---

### Task 12: Durable `ToolResultItem.discovery` record

**Files:**
- Modify: `crates/codegen/xai-grok-sampling-types/src/conversation.rs:414` (`ToolResultItem`) — additive field + the `ToolDiscovery`/`DiscoveredTool` types (spec §1.4 shape)
- Modify (construction sites): `crates/codegen/xai-grok-shell/src/session/acp_session_impl/tool_calls.rs` (new helper `ConversationItem::tool_result_with_discovery(call_id, content, discovery)` beside the existing `tool_result`/`tool_result_with_images` helpers at :2875-2898) + every spread-less `ToolResultItem { … }` literal that breaks: `sampling-types/src/conversation/messages_tests.rs:2771/:3201`, `shell/src/session/memory/hooks.rs:303`, `shell/src/session/compaction_inline_auto_compact_flow_tests.rs:892/:1293`, `shell/src/session/acp_session_tests/laziness/laziness_debug_tests.rs:132`, `xai-grok-subagent-resolution/src/fork.rs:162`, `xai-chat-state/src/compaction_utils.rs:402/:429/:503` (S3a subset — the Phase B variant files are NOT touched here)
- Test: jsonl round-trip test in the shell session storage test file (`shell/src/session/storage/jsonl/tests.rs` — add a test there; the 3 exhaustive matches at :1827/:1877/:1951 are Phase B's, NOT S3a's — S3a adds a FIELD not a variant, so those matches do not break)

**Interfaces:**
- Consumes: `DiscoveredToolDef` (T4), `SearchToolOutput` (T9).
- Produces:
  - `pub struct DiscoveredTool { pub tool_name: String, pub schema_version: String, pub definition: ToolSpec }`
  - `pub struct ToolDiscovery { pub catalog_epoch: String, pub tools: Vec<DiscoveredTool> }`
  - `ToolResultItem.discovery: Option<ToolDiscovery>` with `#[serde(default, skip_serializing_if = "Option::is_none")]` — old JSONL unchanged.
  - Construction rule (spec §1.3 separation of duties): the construction seam ATTACHES the record (wire-neutral); wire projection happens at encode time from the record. The search dispatch arm (T10) attaches `Some(discovery)`; all other tools attach `None` (default).
  - Bare names are rejected by design: `schema_version` + full `definition` make the record reconstructable without the live catalog.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn discovery_record_survives_jsonl_round_trip_resume_fork() {
    let disc = ToolDiscovery {
        catalog_epoch: "epoch-1".into(),
        tools: vec![DiscoveredTool { tool_name: "server__a".into(), schema_version: "v1".into(), definition: tool_spec("server__a", ToolExposure::Deferred) }],
    };
    let item = ConversationItem::tool_result_with_discovery("c1".into(), "found".into(), Some(disc.clone()));
    // serialize -> deserialize (old reader sees discovery; pre-change files without the key parse fine)
    let v = serde_json::to_value(&item).unwrap();
    let back: ConversationItem = serde_json::from_value(v).unwrap();
    let ConversationItem::ToolResult(tr) = &back else { panic!() };
    assert_eq!(tr.discovery.as_ref().unwrap().catalog_epoch, "epoch-1");
    let old_json = r#"{ "tool_call_id": "c2", "content": "x" }"#;
    let old: ToolResultItem = serde_json::from_str(old_json).unwrap();
    assert!(old.discovery.is_none());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p xai-grok-sampling-types discovery_record_survives`
Expected: FAIL — no field/helper.

- [ ] **Step 3: Write minimal implementation**

The two types + field + `tool_result_with_discovery` helper (mirrors the existing `tool_result`/`tool_result_with_images` constructors — locate them with `grep -n "pub fn tool_result" crates/codegen/xai-grok-sampling-types/src/conversation.rs`); fix the 8 listed literal sites (add `discovery: None`); the T10 dispatch arm attaches `Some(ToolDiscovery { catalog_epoch: manifest.catalog_epoch.clone(), tools: results.map(...) })`.

- [ ] **Step 4: Run tests and verify they pass**

Run: `cargo test -p xai-grok-sampling-types -p xai-grok-shell -p xai-grok-subagent-resolution -p xai-grok-chat-state`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A crates/codegen/   # pathspec: only the files listed above — verify with git status BEFORE committing
git commit -m "feat(responses): durable ToolResultItem.discovery record (schema_version+definition; old JSONL unchanged) (apex-ayl.142)"
```

---

### Task 13: Stream preservation + event plumbing

**Files:**
- Modify: `crates/codegen/xai-grok-sampler/src/stream/responses.rs` (the `ResponseStreamEvent` match — the `_ => {}` catch-alls at ~:586/:601 silently drop unknown items today; add typed arms for search items on admitted routes)
- Modify: `crates/codegen/xai-grok-sampler/src/events.rs` (new `SamplingEvent` variants: `ToolSearchCallReceived { request_id, call_id, query, limit }`, `ToolSearchCompleted { request_id, call_id, result_count, status }`)
- Modify: `crates/codegen/xai-grok-shell/src/session/acp_session_impl/sampling_events.rs` + `tool_dispatch.rs` (consume the new events → dispatch arm (T10) / display (T14))
- Test: `crates/codegen/xai-grok-sampler/src/stream/*tests*` if present, else a new focused test with a scripted `ResponseStreamEvent` sequence

**Interfaces:**
- Consumes: the raw search item events from the Responses stream (the pinned async-openai decodes what it knows; search items arrive as the raw/unknown event shape — decode via the T3 `ToolSearchWire` from the event's JSON payload).
- Produces: the two `SamplingEvent` variants above (exact field lists — T14/T17 rely on them); partial-batch rule (spec §2): a decode failure mid batch retains completed search items, incomplete calls re-derive on retry from the frozen manifest (T15), aborted-before-completion → synthesized-empty (T15); a partial tool array never rides the wire.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn admitted_route_search_stream_events_are_not_dropped() {
    let seq = vec![
        search_call_event("c1", "deploy"),        // client-executed
        search_output_event("c1", "completed", 2), // next-turn echo
    ];
    let events: Vec<SamplingEvent> = run_stream_transform(seq, /* admitted */ true).collect();
    assert!(events.iter().any(|e| matches!(e, SamplingEvent::ToolSearchCallReceived { call_id, .. } if call_id == "c1")));
    assert!(events.iter().any(|e| matches!(e, SamplingEvent::ToolSearchCompleted { call_id, status, .. } if call_id == "c1" && status == "completed")));
}

#[test]
fn non_admitted_route_search_items_stay_silent() {
    let seq = vec![search_call_event("c1", "deploy")];
    let events: Vec<SamplingEvent> = run_stream_transform(seq, /* admitted */ false).collect();
    assert!(events.iter().all(|e| !matches!(e, SamplingEvent::ToolSearchCallReceived { .. } | SamplingEvent::ToolSearchCompleted { .. })));
}
```

(`run_stream_transform` = test harness driving the transform fn with scripted events; the `admitted` flag threads the gate state into the transform — verify the transform's current signature and add the flag as an `Option<&DiscoveryManifest>` parameter, defaulting to `None`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p xai-grok-sampler admitted_route_search_stream`
Expected: FAIL — events hit `_ => {}`.

- [ ] **Step 3: Write minimal implementation**

The two `SamplingEvent` variants (serde-free, in-process); the transform arms (decode `ToolSearchWire` from the event payload; client-executed + non-null call_id → `ToolSearchCallReceived`; output items → `ToolSearchCompleted`); the shell-side consumers forward to the dispatch arm (T10) and queue display events (T14). Non-admitted: the arms check the gate flag and stay inert (items still retained in IR history by T8 — the stream layer only emits EVENTS).

- [ ] **Step 4: Run tests and verify they pass**

Run: `cargo test -p xai-grok-sampler -p xai-grok-shell`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/codegen/xai-grok-sampler/src/ crates/codegen/xai-grok-shell/src/session/acp_session_impl/sampling_events.rs crates/codegen/xai-grok-shell/src/session/acp_session_impl/tool_dispatch.rs
git commit -m "feat(responses): search stream events preserved on admitted routes (no silent drop; non-admitted inert) (apex-ayl.142)"
```

---

### Task 14: TUI mode-aware display

**Files:**
- Modify: `crates/codegen/xai-grok-pager/src/scrollback/blocks/tool/search_tool.rs` (the "Search Tools" block)
- Modify: `crates/codegen/xai-grok-pager/src/scrollback/blocks/tool/mod.rs` + `crates/codegen/xai-grok-pager/src/acp/tracker.rs` (event→block routing; the legacy `IntegrationSearch` mapping at `tracker.rs:2080-2102` stays for fallback routes)
- Test: the pager's existing block test files (locate: `ls crates/codegen/xai-grok-pager/src/scrollback/blocks/tool/*test*`)

**Interfaces:**
- Consumes: T13 events; `SearchToolOutput` (T9 — `legacy_content` renders on fallback routes; structured `results` on native).
- Produces: the block carries an explicit MODE — `native-client` / `hosted` / `legacy` (the label "Search Tools" alone is NOT proof of native discovery — the mode is data, not a label). Display: query, result count, loaded qualified identities, dispatch errors — NEVER raw schema dumps (sensitive-payload discipline). ACP events mirror the TUI types (tracker.rs emits the ACP event with the mode field).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn search_block_carries_mode_and_never_raw_schemas() {
    let block = search_block_native(query: "deploy", results: vec!["server__deploy"], error: None);
    assert_eq!(block.mode, SearchMode::NativeClient);
    assert!(block.rendered().contains("server__deploy"));
    assert!(!block.rendered().contains(r#""parameters""#)); // no raw schema dump
    let legacy = search_block_legacy(query: "deploy", legacy_content: "Found 1: server__deploy");
    assert_eq!(legacy.mode, SearchMode::Legacy);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p xai-grok-pager search_block_carries_mode`
Expected: FAIL — no mode field.

- [ ] **Step 3: Write minimal implementation**

`pub enum SearchMode { NativeClient, Hosted, Legacy }` on the block struct (serde default `Legacy` — old renders unchanged); the tracker routes T13 events → `NativeClient`/`Hosted` blocks and legacy `search_tool` function results → `Legacy` (existing rendering, now tagged); ACP emission includes `mode`.

- [ ] **Step 4: Run tests and verify they pass**

Run: `cargo test -p xai-grok-pager`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/codegen/xai-grok-pager/
git commit -m "feat(responses): TUI/ACP search blocks carry explicit mode (native-client/hosted/legacy); no raw schema dumps (apex-ayl.142)"
```

---

### Task 15: Pairing, repair, and synthesized-empty outputs

**Files:**
- Modify: `crates/codegen/xai-grok-sampling-types/src/conversation/responses.rs` (the encode-time repair pass — add it where the input array is finalized; the tree has no equivalent of the donor's `remove_orphan_outputs`, so this is NEW logic in the budgeted encoder file)
- Modify: `crates/codegen/xai-grok-sampling-types/src/conversation/tool_search.rs` (the `tso_` id derivation helper)
- Test: conversion test file

**Interfaces:**
- Consumes: T3 types, T12 durable record.
- Produces:
  - **Pairing atomicity (I-OB-3):** truncation/compaction touching search items keeps or drops call+output TOGETHER (both kept or both dropped; carriers byte-identical). Enforced at the encode-time pass: for every `tool_search_call` with `execution:"client"` in the input, a matching `tool_search_output` (same call_id) must follow in the same request — if the history lost one half (truncation), the pass restores the pair from the DURABLE record (T12) or drops both (never a lone result — an orphaned result is a proven 400 class).
  - **Orphan client outputs:** a `tool_search_output` whose call_id is not in the request's tool-search call set is REMOVED with telemetry (a `tracing::warn!` + counter) — the donor's `error_or_panic` is NOT ported (no panic class); server-executed outputs are NEVER removed (the orphan rule applies to `execution != "server"` only, donor parity).
  - **Synthesized empty (interrupted calls):** `pub fn tso_synthetic_id(call_item_id: &str) -> String` = `with_suffix("tso", v5(SYNTHETIC_OUTPUT_ID_NAMESPACE, "tso:<call_item_id>"))` — derived from the call's **item id** (not call_id); idempotent across resume (same input → same id); the synthesized `ToolSearchOutputItem { status: "completed", tools: vec![], call_id: <real call_id>, item_id: Some(tso id) }` is emitted for any client call with no output (interrupted turn) and PERSISTS to `chat_history.jsonl` as a durable `ToolResultItem` (pairing requires the result to exist durably).
  - **Separate id sets:** tool-search call/output ids never enter the function-call id domain (the two domains are disjoint by construction — the call_id grammar differs; assert in tests).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn pairing_atomicity_and_orphan_removal_and_synthetic_empty() {
    // orphan client output removed (telemetry, no panic)
    let input = vec![search_call_json("c1", "q"), search_output_json("c9", "completed", 0)];
    let out = encode_with_repair(&input);
    assert!(out.iter().all(|i| i.get("call_id").and_then(|c| c.as_str()) != Some("c9")));
    // interrupted call -> synthesized empty with tso_ id, real call_id
    let input2 = vec![search_call_json("c1", "q")]; // no output
    let out2 = encode_with_repair(&input2);
    let synth: Vec<&serde_json::Value> = out2.iter().filter(|i| i["type"] == "tool_search_output").collect();
    assert_eq!(synth.len(), 1);
    assert_eq!(synth[0]["call_id"], "c1");
    assert!(synth[0]["id"].as_str().unwrap().starts_with("tso_"));
    assert_eq!(synth[0]["status"], "completed");
    // idempotent across resume
    let out3 = encode_with_repair(&input2);
    assert_eq!(synth[0]["id"], out3.iter().find(|i| i["type"] == "tool_search_output").unwrap()["id"]);
    // separate id domains: a function_call_output with a tso_ id is still an orphan (removed)
    let mixed = vec![function_call_output_json("tso_deadbeef")];
    assert!(encode_with_repair(&mixed).is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p xai-grok-sampling-types pairing_atomicity`
Expected: FAIL — no repair pass / no id helper.

- [ ] **Step 3: Write minimal implementation**

The repair pass (a pure fn `repair_search_items(input: &mut Vec<serde_json::Value>, durable: &DurableHistory, is_admitted: bool)` called at the encode entry after history serialization): build the call set (client-executed tool_search_call call_ids) and output set; remove orphan client outputs (warn + telemetry counter); for calls missing an output, append the synthesized empty (tso id via the new helper — `SYNTHETIC_OUTPUT_ID_NAMESPACE` = a NEW fixed UUIDv5 namespace constant defined in `tool_search.rs` with a provenance comment; do NOT reuse an unrelated namespace — a wrong namespace breaks the cache-stability contract); the durable-record restore path (T12) runs BEFORE orphan removal so a truncated-but-durable pair is restored, not dropped.

- [ ] **Step 4: Run tests and verify they pass**

Run: `cargo test -p xai-grok-sampling-types`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/codegen/xai-grok-sampling-types/src/conversation/
git commit -m "feat(responses): search pairing/repair pass (atomic pairs, orphan removal w/ telemetry, tso_ synthesized-empty, no panic class) (apex-ayl.142)"
```

---

### Task 16: Invariant obligations (lint coverage + HARDENING-SPEC normative line)

**Files:**
- Modify: `crates/codegen/xai-grok-sampling-types/src/conversation/outbound_lint.rs` (A2 in-product lint: the D-5 non-empty-only H-3 scope already excludes the mint clause — verify the header note; add coverage so the NEW item types pass outbound lint on every send: declaration, additional_tools, tool_search_call, tool_search_output incl. the D-ERR error field)
- Modify (plans repo, NOT the worktree): `/Users/palanisd/Projects/upstream/grok/plans/items/parity-formalism/HARDENING-SPEC.md` §2.1 — the NORMATIVE exemption line (A1 corpus change: the two client-minted id classes are exempt from the mint clause on store:false rows; structural mechanism = A1's response-echo-derived mint set; 400 = the debt-failed signal)
- Test: lint unit tests in `outbound_lint.rs`

**Interfaces:**
- Consumes: T3 types, T11 D-ERR.
- Produces: A2 lint passes for all new item shapes (I-OB-1: every discovery item passes outbound lint on every send — no silent A1/A2 divergence); the HARDENING-SPEC line (committed to the plans repo as a separate commit with message `docs(parity): H-3 exemption for client-minted search ids (S3a) (apex-ayl.142)`); the live 200 evidence stays on bead apex-ayl.146 (operator) — the spec's §6.2a arm references this task's shapes.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn new_search_item_shapes_pass_outbound_lint() {
    let shapes = vec![
        declaration_json(), additional_tools_json("at_x"),
        search_call_json("c1", "q"), search_output_json("c1", "completed", 0),
        serde_json::json!({"type":"tool_search_output","call_id":"c1","status":"error","error":"bad args","tools":[],"execution":"client"}),
    ];
    for s in shapes {
        let v = outbound_lint::lint_item(&s, /* store_false_row */ true);
        assert!(v.hard_violations.is_empty(), "lint failed for {s:?}: {v:?}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p xai-grok-sampling-types new_search_item_shapes_pass_outbound_lint`
Expected: FAIL (unknown shapes flagged, or the lint has no arm for them — either failure is the signal).

- [ ] **Step 3: Write minimal implementation**

Extend the A2 lint with the 5 item types (shape rules: non-empty id where present, call_id non-empty on outputs, status ∈ {completed, error}, tools is an array, error is a string) — the A2 scope stays D-5 (non-empty only; no mint-set check in-product, per the pinned `h3_unknown_id_clean_in_product_d5` behavior). Write the HARDENING-SPEC §2.1 line (plans repo commit — separate from the code commit, pathspec-limited to that file).

- [ ] **Step 4: Run tests and verify they pass**

Run: `cargo test -p xai-grok-sampling-types outbound_lint`
Expected: PASS.

- [ ] **Step 5: Commit (two commits: code, then plans-repo doc)**

```bash
git add crates/codegen/xai-grok-sampling-types/src/conversation/outbound_lint.rs
git commit -m "feat(responses): A2 outbound lint coverage for the 5 search item types (D-5 scope unchanged) (apex-ayl.142)"
# plans repo (separate commit, pathspec-limited):
cd /Users/palanisd/Projects/upstream/grok/plans
git add items/parity-formalism/HARDENING-SPEC.md
git commit -m "docs(parity): H-3 exemption for client-minted search ids (S3a: at_ + tso_ classes; structural via A1 echo-mint set) (apex-ayl.142)"
```

---

### Task 17: Acceptance suite — redteam cases, goldens, seam docs, live arms

**Files:**
- Create: `crates/codegen/xai-grok-shell/src/tests/redteam/hts_responses.rs` (the `hts-*` redteam cases — locate the existing redteam test harness convention first: `ls crates/codegen/xai-grok-shell/src/tests/` and mirror the closest existing suite's structure)
- Modify: `crates/codegen/xai-grok-sampling-types` golden test files (per-compat-class goldens: full / azure-strict / vllm-shim / sglang-shim emit-reject selections + the no-`input_text`-in-output-array assert on the image path, any class)
- Create: `docs/responses-compat-seam.md` + `docs/FORK-MANIFEST.md` (worktree root docs/)
- Modify (probe kit, plans repo — operator-run arms): `/Users/palanisd/Projects/upstream/grok/plans/harness/hosted-tool-search/probe.py` (add the wire-fact assertions as NEW instrumentation: zero-match `tools:[]` shape; no-reinjection on follow-up requests; H-3 200 check for the fabricated ids (bead 146); unparseable-args D-ERR channel)
- Test: this task IS the test suite

**Interfaces:**
- Consumes: everything T1-T16 produced.
- Produces:
  - **17-row donor oracle transcription** (donor contract §9) as `hts-*` cases with wiretap evidence; the 4 rows encoding donor don't-port product surfaces transcribe as NEGATIVE cases (asserting the product surface is ABSENT), marked `donor-surface:skip`.
  - `hts-unparseable-error-channel` (T11), `hts-retry-bytes` (CAPTURE-PAIR: arm A ordinary turn, arm B same turn via the doom-loop failed-response path — `FailedResponseCapture` at `request_task.rs:710-719`; byte-diff scoped to the CATALOG-DERIVED PROJECTION — manifest/tools/deferred segments — NOT whole-body), compat-class goldens (T16/T5), legacy dialects (Responses-without-capability + Chat Completions retain textual search output — regression goldens), JSONL round-trip (T12), unknown-route (capabilities default off; a beta header alone changes nothing).
  - **Seam doc** (`docs/responses-compat-seam.md`, donor authority-doc pattern): boundaries / capability table (the 4 compat classes) / empirical probes (what the live arms assert) / invariants (I-OB-1..5 + pairing + no-reinjection) / verification gate (the offline 14-command gate from HARNESS-MAP).
  - **FORK-MANIFEST**: MUST-SURVIVE tracking of the local type-seam diff (the seam is in-tree, not a fork — the manifest tracks the seam module's public surface so a future async-openai extension (INTEGRATION POINT A seat) can be adopted without breaking it). If INTEGRATION POINT A's queued seat later says "extend the dependency," this manifest is the adoption checklist.
  - **Offline gate:** the HARNESS-MAP 14-command suite runs green (run it in full before declaring done: `cd /Users/palanisd/Projects/upstream/grok/plans && make` or the 14 commands listed in HARNESS-MAP.md §3 — follow that file).
  - **Live arms (operator, bead 146):** the probe.py additions are committed and documented in the probe README; the operator runs them from an unrestricted shell; captures land in `captures/` (gitignored); raw-key sweep 0 before any commit that touches captures-adjacent files.
  - **Exfil:** after the offline gate is green, request the operator's green-light to push the branch to fork + mirror (campaign rule: coordinator pushes, exfil = fork + mirror, per-push light; precedent apex-ayl.145).

- [ ] **Step 1: Transcribe the oracle (write the cases first — they are the spec's acceptance, red-first)**

For each of the 17 donor-oracle rows (donor contract §9 — read it; the rows are the query/limit/shapes with expected wire facts): write the `hts_*` test asserting the wire shape our implementation must produce (admitted route). The 4 don't-port rows → negative assertions. Run: `cargo test -p xai-grok-shell hts_` — Expected: FAIL (cases not yet wired / behaviors incomplete).

- [ ] **Step 2: Implement whatever the cases expose (each gap is a fix IN BUDGET — if a gap requires an out-of-budget file, STOP and raise it; that is a plan violation)**

Iterate test→fix→test until `cargo test -p xai-grok-shell hts_` is green.

- [ ] **Step 3: Compat-class goldens + legacy regression goldens + unknown-route test**

Write the four compat goldens (T5's image-path assert included in each) + the legacy dialects (non-admitted Responses + Chat Completions byte-identical to the pre-cut baseline — capture the baseline shapes from the current goldens BEFORE this task's changes; if none exist, generate them at the base commit first and note the hashes) + the unknown-route test (gate off, header present, behavior unchanged).

- [ ] **Step 4: Seam doc + fork manifest + probe-kit assertions**

Write `docs/responses-compat-seam.md` (the five donor-pattern sections) and `docs/FORK-MANIFEST.md` (public surface of `conversation/tool_search.rs` + `manifest.rs`: exact exported names). Add the probe.py wire-fact assertions (plans repo commit, pathspec-limited: `harness/hosted-tool-search/probe.py` + its README) — capture-only kit gains the assertion functions; the ARMS themselves run on the operator's shell.

- [ ] **Step 5: Full offline gate + commit + exfil request**

Run the HARNESS-MAP 14-command offline gate in full (plans repo). All green → commit the plan's final artifacts → message the operator: offline gate green, live arms ready (bead 146), exfil green-light requested.

```bash
git add crates/codegen/ docs/
git commit -m "feat(responses): S3a acceptance suite (17-row oracle hts-*, compat goldens, seam doc, fork manifest) (apex-ayl.142)"
# plans repo:
cd /Users/palanisd/Projects/upstream/grok/plans
git add harness/hosted-tool-search/probe.py harness/hosted-tool-search/README.md
git commit -m "harness: probe kit wire-fact assertions (zero-match, no-reinjection, H-3 200, D-ERR) for S3a live arms (apex-ayl.142)"
```

---

## Self-Review (performed 2026-09-25, writing-plans skill checklist)

**1. Spec coverage:** §1 shared seam → T1 (exposure), T4 (manifest), T9 (SearchToolOutput), T12 (durable record) ✓. §2 Responses wire → T5 (xt2.10), T6 (declaration+placements), T7 (deferred-aware), T8 (IR arms), T10 (dispatch+BM25+bypass+zero-match), T11 (D-ERR), T15 (pairing/repair/synthesized-empty), T13 (stream), T14 (TUI/ACP) ✓. §4 persistence → T12 (record), T15 (retry/stability), compaction rule = S3b's file set (this plan's S3a rows only — the Phase B variant files are explicitly out) ✓. §6 acceptance → T16 (lint), T17 (oracle, goldens, live arms, seam doc, fork manifest) ✓. §7 budget → every task's Files list is a §7 S3a row; S3b/S3c rows are named and excluded ✓. **Gaps found & resolved during review:** (a) "no new ConversationItem variant in S3a" stated in Architecture + T8 (server items ride `CodexRawInput`); (b) uuid dependency availability handled in T6 step 3 (derive where the dep exists, pass Strings); (c) the `ToolMetadataSnapshot` accessor may need a small read-only addition in `mcp_snapshot.rs` (T4 files list covers it).

**2. Placeholder scan:** no TBD/TODO/"similar to Task N"/"add appropriate handling" — every code step carries code or an exact grep+expect. The two deliberate "verify with grep first" steps (T1 test-file location, T13 transform signature) state the exact command and the expected shape — they pin the target, they do not delegate the decision.

**3. Type consistency:** `ToolExposure` (T1) used identically in T2-mention/T7/T12; `DiscoveryManifest`/`DiscoveredToolDef` (T4) = `DiscoveredTool` fields (T12) — T12's `DiscoveredTool` and T4's `DiscoveredToolDef` are the SAME type: T4 is renamed to `DiscoveredTool` at execution time if the names collide (the plan uses both names in different tasks — resolution: `DiscoveredTool` wins, T4's `DiscoveredToolDef` is a drafting alias); `ToolSearchOutput` (T9) consumed by T10/T14; `ToolSearchOutputItem` (T3, WIRE type) vs `SearchToolOutput` (T9, TOOLS-layer type) are deliberately different types — the encoder converts between them (T10's arm: `SearchToolOutput` → wire item). `tso_synthetic_id` (T15) matches the Global Constraints id rule. Event names (T13) match T14's consumption.

## Execution notes

- TDD order is the task order; T1-T4 are the foundation and may land as one review wave; T5-T8 the encoder wave; T9-T12 the dispatch/persistence wave; T13-T16 the stream/invariant wave; T17 is the gate wave.
- After EACH wave: run the wave's crate test suites + the raw-key sweep + pathspec-limited commit per task.
- Live probe runs (bead apex-ayl.146) may run in parallel with code waves (they exercise the CURRENT tree baseline first — that is the pre-divergence baseline the D-ERR and H-3 arms compare against).
- If any task discovers an out-of-budget file is needed: STOP, record it in the task's notes, raise to the coordinator — do not expand scope silently (spec rule: "Anything outside this list is a spec violation at review").

---

## Amendments (post-plan, evidence-driven)

These amendments record deltas discovered AFTER the plan was ratified. SPEC v1.3 stays frozen; these are
plan/record deltas only. Evidence: `plans/harness/hosted-tool-search/wire-grounding-probes.md` (live paid probes,
2026-09-25, 21 POSTs, $0.0388) and `instrumentation-formalism-map.md` (Opus formalism map).

### A-1 (T6) — DECISION REQUIRED (D3): lite placement silently defeats hosted search

**Conflict.** T6 as written sends lite rows' tools (declaration included) in a leading `additional_tools` item and
clears top-level `tools`. Live probe **R4** (`gpt-5.6-sol`) shows this deployment ACCEPTS that request (HTTP 200)
but: omits `tool_search` from the echo, sets `defer_loading:null` on every function, emits NO
`tool_search_call`/`tool_search_output` pair, and calls a function directly. Probe **R1** shows the SAME model with
TOP-LEVEL placement performs full server-side discovery.

**Verified from raw bytes (controller, not seat prose).** R1 echo `tools[]` = `[function, function, tool_search]`
with `defer_loading:[true,true]`; R4 echo `tools[]` = `[function, function]` — the `tool_search` declaration is
DROPPED and `defer_loading` is stripped to `null`. So lite placement does not merely skip discovery: it discards
the deferral contract, leaving tools we intended to hide fully exposed AND eagerly callable — the exact inverse of
S3a's "zero prompt bytes for undeclared tools" intent. Captures: `captures/2026-09-25-wire-grounding/`
(46 files + MANIFEST.tsv, auth-swept clean); py and TS arms agree independently.

**Blast radius.** T2 baked BOTH flags onto the same three rows — `gpt-5.6-sol`, `gpt-5.6-terra`, `gpt-5.6-luna`
carry `supports_search_tool:true` AND `use_responses_lite:true`. Under T6 as written, discovery never runs on
exactly the rows S3a targets, and the failure is silent (no error, no marker).

**Options.**
- **D3-A (controller lean):** placement precedence — when a route is ADMITTED for search, use TOP-LEVEL placement
  even on lite rows; lite placement applies only to non-admitted routes. Rationale: R1 proves top-level works on
  these exact rows; `use_responses_lite` governs declaration placement, NOT the strict replay contract
  (`strict_responses_input` is the separate REPLAY-1 field). Requires a T17 live arm asserting the search pair.
- **D3-B (fail-closed):** treat `supports_search_tool && use_responses_lite` as a CLOSED gate — no declaration, no
  discovery, log the suppression. Safest, but S3a delivers nothing on its three flagship rows.
- **D3-C:** keep dual placement and add a live-verified per-row override flag. Most config surface, most churn.

**Until D3 is decided, T6 is BLOCKED.** Do not implement the dual-placement encode as specced.

### A-2 (T11) — orphan/stale discovery is a provider HTTP 400, not an in-band D-ERR

Probe **R5**: a fresh `gpt-5.6-sol` request carrying a client `tool_search_output` with no matching preceding call
returns Azure `invalid_request_error` (param `input`): "No tool call found for tool search output with call_id …".
The request never reaches a model turn, so the proposed in-band D-ERR channel CANNOT carry this class. T11 must
(a) PREVENT orphan emission at encode time (pairing precondition, shared with T15), and (b) treat the 400 as a
transport-level failure with its own mapped error, not a D-ERR item.

### A-3 (T4) — capability gate confirmed necessary by negative evidence

Probe **R3** (`qwen3.8-27b`, design-OFF row): the same hosted declaration returns HTTP 200, the echo normalizes
`defer_loading:null`, a skeletal `tool_search` survives, and NO search item is ever produced. Unsupported rows
degrade SILENTLY rather than failing closed. The T2 catalog flag + T4 gate are therefore load-bearing, and the
read side must never infer "no search happened" as "search unsupported".

### A-4 (T5/T12) — pending operator decisions D1/D2 (from the formalism map)

- **D1 (T5):** codex's own suite (`codex-rs/core/tests/suite/search_tool.rs:126-145`, `:296-312`) asserts NAMESPACE
  children inside `tool_search_output.tools[]`, contradicting design.md:151 "flat tools only". Controller lean:
  unwrap namespace children into flat `DiscoveredTool`s carrying a `namespace` field.
- **D2 (T12):** silent-drop arms make a dropped `tool_search_call` indistinguishable from "no search". Probe R3
  makes this concrete. Controller lean: RECORD unknown output items (flag/log), never `continue`-skip.

### A-5 (wave 5 / apex-ayl.146) — remaining capture gap

R1/R2 close the SERVER-executed top-level shapes. Still uncaptured: the CLIENT-executed harness path
(call → output → next turn with NO re-injection, the H-3 obligation). The 146 baseline must capture that path with
k≥3 and confounders pinned (`reasoning.effort` and the token cap materially changed R2-luna's outcome; the 64-token
cap truncated the Anthropic server-search result).

### A-6 (CORRECTION — the existing mechanism was mis-stated) — `search_tool` + `use_tool` already ship

**The error.** Earlier framing said the harness has "no search mechanism; every tool declared every turn". That is
WRONG and the operator corrected it. A generic lazy tool-load mechanism ships TODAY:

- `search_tool` (`xai-grok-tools/src/implementations/search_tool/mod.rs`, 757 lines) — "discover MCP tools via BM25
  keyword search" over `ToolIndex`. Input `{query: String, limit: Option<u8> = 5}`; returns tool schemas.
- `use_tool` (`.../implementations/use_tool/mod.rs`, 1629 lines, `USE_TOOL_NAME`) — meta-dispatch. Input
  `{tool_name: "linear__save_issue", tool_input: {...}}`; the target must have been discovered via `search_tool`.
- Contract is enforced by injected prompt text (see fixture
  `xai-grok-sampling-types/fixtures/outbound_lint/bodies/h2-accept-mxai-c04-req004-EV-9.json`): "To use MCP tools,
  you MUST call `search_tool` first to retrieve the tool's input schema before calling `use_tool`. NEVER guess
  parameter names." MCP servers are announced by blurb + tool count, NOT by per-tool schema.
- Enable/precedence plumbing exists (`xai-grok-shell/src/util/config/resolve/toolset.rs`), as does telemetry
  (`tool_search_count`) and UI special-casing (`UseToolCallBlock`).

**Corrected S3a value proposition.** S3a is NOT "add discovery". Discovery exists and is already provider-agnostic
(two ordinary function tools work on every wire). S3a is: (a) express the SAME BM25 discovery natively on the wire,
and (b) REMOVE THE INDIRECTION — today the model emits `use_tool{tool_name, tool_input}`; probe R1 proves native
mode emits a real `function_call` for `lookup_shipping_eta` itself. Secondary win: server-executed mode removes the
extra round trip (search turn -> dispatch turn collapses into one response).

**Collisions this exposes — CHECK BEFORE IMPLEMENTING T5 AND T9.**
1. T9 "SearchToolOutput redefinition" edits a LIVE shipping type. Consumers today:
   `xai-grok-tools/src/types/output.rs:594,616,1306`, `xai-grok-pager/src/acp/tracker.rs:30,2090`,
   `xai-grok-shell/src/session/acp_session_impl/tool_layer_images.rs:66,187`,
   `xai-grok-tools/src/util/mcp_truncate.rs:463`. A redefinition is a breaking change to the legacy path, not a
   greenfield addition. The "old mechanism stays byte-identical" premise must be RE-PROVEN against these call sites.
2. T5 introduces `DiscoveredTool`, but `xai-grok-pager/src/acp/tracker.rs:2755` already parses SearchToolOutput
   "into DiscoveredTool entries". Name/shape collision to resolve before writing T5.

**Three-tier strategy this implies** (one BM25 `ToolIndex` behind all tiers):
server-native where verified (Responses sol/terra/luna; Anthropic BM25 tool type) -> client-native
(`execution:"client"` Responses, UNPROVEN; Anthropic `tool_reference`, PROVEN) -> legacy `search_tool`/`use_tool`
everywhere else. A fallback ladder, not a fork.
