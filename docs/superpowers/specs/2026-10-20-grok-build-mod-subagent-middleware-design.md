# Grok-Build Mod Subagent Middleware (`xai-grok-mods` + `plugins/cursor`) — Design Spec

- **Status**: Approved (Round 3 Code-Verified — Zero Blockers)
- **Date**: 2026-10-20
- **Target**: `netbrah/Grok-build` branch `feat/hts-native-shipping` @ `2fcd97294a23cfb19b1d6e4c1a788840a17dc569` (vendored at `refs/netbrah-grok-build`)
- **Reference port**: `netbrah/cursor-subagents` @ `bc51fd7` (vendored at `refs/netbrah-cursor-subagents`)
- **Research corpus**: `intel/00..04-*.md` in `netbrah/claude-code-recon`
- **Review record**: `...-review-r1.md` (9 issues), `...-review-r2.md` (5 issue groups + 3 advisories), `...-review-r3.md` (**Status: Approved**, 0 issues + 4 advisories folded in). Section 11 maps every finding to its resolution.

All file:line citations below are relative to `refs/netbrah-grok-build/crates/codegen/` unless stated otherwise.

---

## 1. Problem and Decision

### 1.1 Problem
`netbrah/cursor-subagents` lets Claude Code delegate `subagent_type: "cursor:<model>"` to Cursor's SDK. It is built as a `PreToolUse` shadow of `Agent|Task` because Claude Code has no pluggable child runtime. Porting that shape to Grok-Build would only cover the legacy `Task` tool and would miss the other two subagent surfaces, all of which converge on `SubagentCoordinator<ShellChildRunner>` and `run_shell_child` (`xai-grok-shell/src/agent/subagent/handle_request.rs:347`):

1. **Legacy `Task` tool** (`xai-grok-tools/src/implementations/grok_build/task/task_tool.rs`).
2. **Native v2 lifecycle + mailbox tools** (`spawn_agent`, `send_message`, `wait`, `close_agent`, `list_agents`): active-message admission via `ChildControl::send_active_message`, mailbox flush via `deliver_initial_message` / `deliver_followup`, and idle wake via `run.wake_origin`.
3. **Rhai workflows** (`xai-grok-shell/src/session/workflow/host_service.rs:434` `HostService::spawn_agent`): output-contract prompt wrapping, `validate_contract_output`, and schema-failure retry with `resume_from = Some(first_result.child_session_id)`.

### 1.2 Decision
Add a **mod child runtime** behind the existing `ChildControl` seam:

- A new leaf crate `xai-grok-mods` owns the manifest, NDJSON sidecar protocol, process lifecycle, vendor-session store, and MCP bridge socket. It knows nothing about the shell.
- A new shell file `agent/subagent/mod_adapter.rs` owns all shell integration (start publication, coordinator promotion, persistence, worktree disposal). As a child module of `agent/subagent`, it can call private items there (`persist_subagent_output` `mod.rs:2845`, `persist_subagent_completion` `mod.rs:2849`, `child_run_output` `mod.rs:2335`, `failure_result` `mod.rs:2329`, `resolve_inherited_mcp_pool` `mod.rs:1957`, the `pub(super)` items in `start_artifact_publication.rs`) without visibility changes.
- `ShellChildRuntime` becomes a two-variant enum so the coordinator stays monomorphic over `ShellChildRunner`.
- `run_shell_child` branches to the adapter right after definition resolution, gating, resume-source lookup, worktree setup, cwd validation, and capability intersection, which run unchanged for mod agents.
- Agent **discovery needs no Rust changes**. Mod agents are ordinary plugin agents (`<plugin>/agents*/<name>.md`), which existing discovery reads from disk on every lookup.

**Upstream footprint target: ≤ 85 changed lines across 6 existing files, 0 changes in `xai-grok-agent`, `xai-grok-tools`, `spawn.rs`, `start_artifact_publication.rs`, or `xai-workflow`.** The implementation plan's final task measures this with `git diff --stat`.

---

## 2. Goals, Non-Goals, Success Criteria

### Goals
- G1: `cursor:<model>` subagents work identically from `Task`, v2 `spawn_agent`/`send_message`/`wait`/`close_agent`, and Rhai `agent()`/`spawn_agents()`.
- G2: Full lifecycle parity: `SubagentSpawned`/`SubagentFinished` notifications, `meta.json` and `output.json` persistence, resume across turns, idle wake, live steer/queue, cancel and interrupt, and worktree isolation with snapshot disposal.
- G3: Parent MCP servers are reachable from the sidecar through a host-owned socket. No duplicate stdio servers are spawned.
- G4: Keep upstream merge friction minimal (§1.2 budget).

### Non-Goals
- In-process JS function hooks over built-in tools. The existing command/HTTP hook engine stays as is.
- Replacing Rhai or porting Ultracode JS.
- Bridging Grok's built-in workspace tools (read/edit/grep/shell) into the sidecar. The vendor SDK brings its own tools rooted at the child cwd.
- Remote or daemonized sidecars.
- Folding vendor token usage into the parent xAI bill (`capture_and_fold_one_turn_usage`, `handle_request.rs:2427`). Vendor tokens are reported for display only (§5.6).

### Success Criteria
- S1: Mock-sidecar integration tests pass for all three surfaces (§9).
- S2: `cargo test -p xai-grok-mods` and `cargo test -p xai-grok-shell subagent::` pass; `cargo clippy --workspace -D warnings` is clean.
- S3: The diff-stat budget holds.
- S4: A manual dogfood run with real `@cursor/sdk` completes a Task, a v2 steer, and a Rhai contract retry.

---

## 3. Crate Graph and Footprint

### 3.1 Dependencies
`crates/codegen/xai-grok-mods/Cargo.toml` depends on:

- `xai-grok-tools`, for the coordinator types in `implementations::grok_build::task::{coordinator, types}`: `ChildControl`, `LocalBoxFuture<T>`, `SendBoxFuture<T>`, `SubagentProgress`, `ActiveMessageAdmission`, `ActiveAgentMessageDelivery`, `ActiveAgentMessageOperation`, `AgentMailboxMessage`, `SubagentRequest`, `SubagentResult` (`coordinator_state.rs:22-100`, `active_message.rs:154-217`, `types.rs:281-291, 649-680`). `xai-grok-tools` does not depend on `xai-grok-agent` or `xai-grok-shell`, so the graph stays acyclic.
- `xai-tool-types` (`crates/common/xai-tool-types`), for `SubagentCapabilityMode` (`task.rs:238`).
- External crates: `serde`, `serde_json`, `tokio` (process, net, io-util, sync, time), `tokio-util`, `dunce`, `which`, `libc`, `rand`, `thiserror`, `tracing`.

`xai-grok-mods` does **not** depend on `xai-grok-agent`, `xai-grok-mcp`, or `xai-grok-shell`. Shell types reach it only through the `ModHost` trait (§5.2).

```
xai-tool-types ─┐
xai-grok-tools ─┴─► xai-grok-mods ─► xai-grok-shell ◄─ xai-grok-agent, xai-grok-mcp
```

### 3.2 Changed existing files

| File | Δ lines (est.) | Change |
|---|---:|---|
| `Cargo.toml` (workspace root) | +1 | add member `crates/codegen/xai-grok-mods` |
| `xai-grok-shell/Cargo.toml` | +1 | `xai-grok-mods = { path = "../xai-grok-mods" }` |
| `xai-grok-shell/src/agent/subagent/mod.rs` | +1 | `mod mod_adapter;` |
| `xai-grok-shell/src/agent/subagent/child_runtime.rs` | ~+50 / 2 edited | rename struct and impl to `NativeShellChildRuntime` (`:14`, `:29`); add `enum ShellChildRuntime` plus delegating `ChildControl` impl (§5.1) |
| `xai-grok-shell/src/agent/subagent/handle_request.rs` | ~+24 / 2 edited | dispatch block after `:911` (§5.3); wrap literal at `:1956` in `ShellChildRuntime::Native(NativeShellChildRuntime { … })`; one import |
| `xai-grok-shell/src/agent/subagent/child_runtime_tests.rs` | +3 / 2 edited | wrap literal at `:86`; one import |
| **Total** | **≈ 80** | |

Construction sites of `ShellChildRuntime { … }` exist only at `handle_request.rs:1956` and `child_runtime_tests.rs:86`. Other references (`mod.rs:73`, `spawn.rs:91`, `start_artifact_publication.rs:11,39,59`, `tests/wake.rs:27`, `child_runtime_tests.rs:23`) use it only as a type, so they compile unchanged against the enum.

### 3.3 New files
```
xai-grok-shell/src/agent/subagent/mod_adapter.rs      # ModHost impl, dispatch, lifecycle, persistence
xai-grok-mods/
  Cargo.toml
  src/lib.rs        # pub use; run_mod_child entry
  src/manifest.rs   # mod.json schema + containment validation
  src/protocol.rs   # NDJSON frames (§6)
  src/process.rs    # spawn, env scrub, process group, stderr ring, kill escalation
  src/control.rs    # ModChildControl: impl ChildControl
  src/session.rs    # run loop: RunStart → events → drain → terminal
  src/store.rs      # ~/.grok/mods/<ns>/sessions.json (subagent_id → vendor_session_id)
  src/bridge.rs     # per-run Unix socket for MCP calls
  tests/mock_sidecar.rs + tests/fixtures/mock-sidecar.mjs
plugins/cursor/
  .grok-plugin/plugin.json
  mod.json
  agents/*.md                 # checked-in defaults
  agents.generated/.gitkeep   # tracked so dunce::canonicalize succeeds on macOS; *.md gitignored
  hooks/hooks.json            # SessionStart → catalog refresh (background)
  sidecar/{package.json, runner.mjs, catalog.mjs}
```

---

## 4. Plugin Packaging and Discovery

### 4.1 Discovery uses existing plugin agents unchanged
- `by_name_in_cwd_with_plugins_and_home` (`xai-grok-agent/src/discovery.rs:432-460`) splits `cursor:composer-2` on `:`, finds `registry.get("cursor")` with `plugin.enabled` set, and loads `<agent_dir>/composer-2.md` from **each** of `plugin.agent_dirs`. It reads from disk on every call.
- `load_plugin_agent_definition` (`discovery.rs:495-519`) sets `def.plugin_name = Some(plugin.name)`. Untrusted plugins load frontmatter only.
- The manifest `agents` field is `Option<PathOrPaths>` (`xai-grok-agent/src/plugins/manifest.rs:145,176`), so a plugin may list multiple directories.

**Invariant N1**: `plugin.json` `name` == `mod.json` `namespace`. For example, both are `"cursor"`, so agents resolve as `cursor:<file-stem>`.

### 4.2 Dynamic catalog without Rust changes
`sidecar/catalog.mjs --write-agents` queries the vendor SDK model list and atomically rewrites `agents.generated/<name>.md`, one file per model. The plugin `hooks/hooks.json` registers a `SessionStart` command hook (event exists: `xai-grok-hooks/src/event.rs:81`) that runs it detached with a 5 s timeout, so the hook never blocks. New models therefore become valid `subagent_type`s on the next lookup, because discovery reads disk per call. The previous R1/R2 `cfg.cli_agents` injection is dropped: `cli_agents` is a `Vec<AgentDefinition>` populated before the plugin registry exists (`agent/config.rs:1499`, `mvp_agent/agent_ops.rs:388-406`), and touching it would add MvpAgent edits.

Generated agent file (camelCase keys per `#[serde(rename_all = "camelCase")]`, `xai-grok-agent/src/config.rs:684`):
```markdown
---
name: composer-2
description: Cursor Composer 2 coding subagent (via grok-mod cursor)
model: composer-2
capabilityMode: full
---
```
- `model` deserializes to `ModelOverride::Override("composer-2")` (`config.rs:531-556`). The adapter reads it as the **vendor** model id and skips native model resolution (§5.3).
- `capabilityMode: read-only` (kebab-case, aliases at `xai-tool-types/src/task.rs:238-245`) marks a read-only agent. `run_shell_child` already intersects it into `effective_runtime.capability_mode` (`handle_request.rs:882-885`).
- The body, if present and the plugin is trusted, becomes `RunStart.system_prompt`.

### 4.3 `plugins/cursor/.grok-plugin/plugin.json`
```json
{
  "name": "cursor",
  "version": "1.0.0",
  "description": "Cursor SDK subagents via Grok-Build mod protocol v1",
  "agents": ["./agents/", "./agents.generated/"],
  "hooks": "./hooks/hooks.json"
}
```

### 4.4 `plugins/cursor/mod.json`
```json
{
  "schemaVersion": 1,
  "namespace": "cursor",
  "runtime": {
    "command": "node",
    "args": ["sidecar/runner.mjs"],
    "timeoutSecs": 1800,
    "gracefulKillMs": 2000,
    "envAllowlist": ["PATH","HOME","USER","TMPDIR","LANG","LC_ALL","CURSOR_API_KEY","HTTPS_PROXY","HTTP_PROXY","NO_PROXY"]
  },
  "capabilities": {
    "liveSteer": true,
    "sessionResume": true,
    "mcpBridge": true,
    "mcpInReadOnly": false
  },
  "mcpBridge": { "denyServers": ["pty", "codebase-memory", "weave"] }
}
```

### 4.5 Manifest types and validation (`manifest.rs`)
```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModConfig {
    pub schema_version: u32,
    pub namespace: String,
    pub runtime: ModRuntime,
    #[serde(default)] pub capabilities: ModCapabilities,
    #[serde(default)] pub mcp_bridge: ModMcpBridge,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModRuntime {
    pub command: String,
    #[serde(default)] pub args: Vec<String>,
    #[serde(default = "d_timeout")] pub timeout_secs: u64,      // 1800
    #[serde(default = "d_kill")] pub graceful_kill_ms: u64,     // 2000
    #[serde(default)] pub env_allowlist: Vec<String>,
}
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModCapabilities { pub live_steer: bool, pub session_resume: bool, pub mcp_bridge: bool, pub mcp_in_read_only: bool }
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModMcpBridge { #[serde(default)] pub deny_servers: Vec<String> }

pub struct ValidatedMod { pub config: ModConfig, pub root: PathBuf, pub program: PathBuf, pub args: Vec<OsString> }

impl ModConfig {
    pub fn load(plugin_name: &str, canonical_root: &Path) -> Result<ValidatedMod, ModError>;
}
```
Validation rules (`load`):
1. `schema_version == 1`.
2. `namespace == plugin_name`, and it matches `^[a-z0-9][a-z0-9_-]{0,31}$`.
3. **Command.** A bare name (no `/` or `\`) must be in `{node, bun, deno, python3}` and is resolved with `which::which`. Anything else is treated as plugin-relative. In that case, reject absolute paths and any `..` component, then require `dunce::canonicalize(root.join(cmd))?.starts_with(canonical_root)`.
4. **Args.** Any arg not starting with `-` that names an existing path relative to root is canonicalized and must `starts_with(canonical_root)`. Absolute paths and `..` are rejected outright.
5. The result is cached per `(canonical_root, mod.json mtime)` in a `parking_lot::Mutex<HashMap>` inside `xai-grok-mods`.

### 4.6 Mod detection and trust (`mod_adapter::detect`)
```rust
fn detect(def: &AgentDefinition, ctx: &SubagentSpawnContext) -> Option<Result<ValidatedMod, String>>
```
- Returns `None` (native path) unless `def.plugin_name == Some(p)`, `ctx.plugin_registry` has `get(p)` (`xai-grok-agent/src/plugins/registry.rs:248`), and `plugin.canonical_root.join("mod.json").is_file()`.
- Returns `Some(Err(..))` if the plugin is not `trusted` (`registry.rs:27`). Running a sidecar is an executable operation, so an untrusted mod fails closed with "mod plugin '<p>' is not trusted".
- Otherwise returns `Some(ModConfig::load(p, &plugin.canonical_root))`.

---

## 5. Runtime Architecture

### 5.1 `ShellChildRuntime` enum (`child_runtime.rs`)
The existing struct (`:14-27`, fields `child_cmd_tx`, `message_delivery`, `active_message_target_session_id`, `child_signals`, `_child_thread`, `receipt_sink`, `#[cfg(test)] force_queue_envelope`, `active_message_parent_session_id`, `active_message_parent_prompt_index`) and its `impl ChildControl` (`:29-133`) are renamed to `NativeShellChildRuntime` with **no field or body changes**. Then:

```rust
pub(crate) enum ShellChildRuntime {
    Native(NativeShellChildRuntime),
    Mod(xai_grok_mods::ModChildControl),
}

impl ChildControl for ShellChildRuntime {
    type ProgressFuture = LocalBoxFuture<SubagentProgress>;
    fn progress(&self) -> Self::ProgressFuture {
        match self { Self::Native(c) => c.progress(), Self::Mod(c) => c.progress() }
    }
    fn send_active_message(&self, d: ActiveAgentMessageDelivery) -> SendBoxFuture<ActiveMessageAdmission> {
        match self { Self::Native(c) => c.send_active_message(d), Self::Mod(c) => c.send_active_message(d) }
    }
    fn cancel(&self) { match self { Self::Native(c) => c.cancel(), Self::Mod(c) => c.cancel() } }
    fn interrupt(&self) -> bool { match self { Self::Native(c) => c.interrupt(), Self::Mod(c) => c.interrupt() } }
    fn accepts_native_message(&self, m: &AgentMailboxMessage) -> bool {
        match self { Self::Native(c) => c.accepts_native_message(m), Self::Mod(c) => c.accepts_native_message(m) }
    }
    fn deliver_initial_message(&self, m: &AgentMailboxMessage) -> bool {
        match self { Self::Native(c) => c.deliver_initial_message(m), Self::Mod(c) => c.deliver_initial_message(m) }
    }
    fn deliver_followup(&self, m: &AgentMailboxMessage) -> bool {
        match self { Self::Native(c) => c.deliver_followup(m), Self::Mod(c) => c.deliver_followup(m) }
    }
}
```
`NativeShellChildRuntime::ProgressFuture` is already `LocalBoxFuture<SubagentProgress>` (`:30`). `ModChildControl` uses the same associated type, so both arms unify. `ChildControl` has exactly these 7 methods plus the associated type (`coordinator_state.rs:59-100`).

### 5.2 Crate ↔ shell seam (`xai_grok_mods::ModHost`)
```rust
pub type BorrowedSendFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait ModHost: Send + Sync {
    /// Called once on `run.started`. Shell impl builds `StartedChild` and promotes.
    /// Returns false if the coordinator refused promotion (cancel won the race).
    fn on_started<'a>(&'a self, started: ModStarted, control: ModChildControl) -> BorrowedSendFuture<'a, bool>;
    /// Live progress. Shell impl emits `SessionUpdate::SubagentProgress`.
    fn on_progress(&self, progress: &SubagentProgress, activity: Option<&VendorToolActivity>);
    /// Close admission before terminal drain; returns `reporter.finalizing()`.
    fn finalizing<'a>(&'a self) -> BorrowedSendFuture<'a, bool>;
    /// MCP bridge call. `None` server/tool not exposed.
    fn call_mcp_tool<'a>(&'a self, server: &'a str, tool: &'a str, args: serde_json::Value)
        -> BorrowedSendFuture<'a, Result<serde_json::Value, String>>;
}

pub struct ModStarted { pub vendor_session_id: String, pub effective_model: String }

pub struct ModRunInput {
    pub validated: ValidatedMod,
    pub subagent_id: String,                 // request.id (UUIDv7)
    pub parent_session_id: String,
    pub subagent_type: String,
    pub agent_name: String,
    pub vendor_model: String,
    pub cwd: PathBuf,
    pub prompt: String,                      // already folded (§5.4 step 2)
    pub system_prompt: Option<String>,
    pub read_only: bool,
    pub reasoning_effort: Option<String>,
    pub resume_key: Option<String>,          // §6.5
    pub bridged_tools: Vec<BridgedTool>,     // empty ⇒ no socket
    pub cancellation: tokio_util::sync::CancellationToken,  // run.cancellation
}

pub async fn run_mod_child(input: ModRunInput, host: Arc<dyn ModHost>) -> ModRunOutcome;

pub struct ModRunOutcome {
    pub promoted: bool,
    pub final_text: String,
    pub error: Option<String>,
    pub cancelled: bool,
    pub turns: u32,
    pub tool_calls: u32,
    pub tokens_used: u64,
    pub vendor_session_id: Option<String>,
    pub admission_uncertain: bool,
    pub warnings: Vec<String>,
}
```
The shell converts `ModRunOutcome` into a full `SubagentResult` literal (`SubagentResult` also implements `Default` at `types.rs:682`, though explicit field initialization is used for clarity). `output_tokens_used = 0`, `total_tokens_used = tokens_used`, `output_usage_incomplete = true`, `backgrounded = false`, and `worktree_path` comes from the adapter.

### 5.3 Dispatch point (`handle_request.rs`, after line 911)
The dispatch goes after the workflow scheduler-tool strip (`:912-919`) and before `fork_context` model pinning (`:920`) and native model resolution (`:923`). At that point all of these are in scope and final: `request` (cwd sanitized `:851-873`), `ctx`, `completion_data`, `gateway`, `reporter`, `cancel_token`, `attempt_identity`, `attempt_id`, `agent_address`, `is_wake`, `wake_message_id` (from `:369-372`; `wake_origin` itself is already moved), `definition` (tool policy applied `:892-896`), `effective_runtime` (capability intersected `:882-885`), `resume_source` (`:462`), `worktree_path` (`:824`, validated), `worktree_freshly_created` (`:842`), `child_depth` (`:886`), `spawn_prepare_span`, `start`, and `spawn_timer`.

```rust
    if let Some(validated) = super::mod_adapter::detect(&definition, &ctx) {
        return super::mod_adapter::run(super::mod_adapter::Dispatch {
            validated, request, ctx, completion_data, gateway, reporter,
            cancel_token, attempt_identity, attempt_id, agent_address,
            is_wake, wake_message_id, queued_for, session_running,
            definition, effective_runtime, resume_source, worktree_path,
            worktree_freshly_created, child_depth, spawn_prepare_span,
            spawn_timer, start,
        })
        .await;
    }
```
`Dispatch` field types are exactly the in-scope local types. The adapter returns `ChildRunOutput<ShellCompletionData>` via `child_run_output`. If the mod fails before promotion (`validated` is `Err(msg)` or `!outcome.promoted`) and `worktree_freshly_created` is `true`, `mod_adapter::run` removes the unpromoted worktree via `crate::session::worktree::remove_subagent_worktree(wt).await` (matching `cancel_pending_shell_child` at `mod.rs:2445-2455`) before returning `child_run_output(failure_result(&request, &msg), completion_data, None)`.

Why not earlier: worktree creation and rehydration (`:563-850`) and `durable_resume_source_for` must run for mod agents too. Why not later: native model fallback (`:940-951`) would replace `composer-2` with the parent model, and the resume model pin (`:982-1003`) would fail resume because `composer-2` is not in the xAI catalogue.

### 5.4 Adapter lifecycle (`mod_adapter::run`)
Steps, mirroring the native path citation by citation:

1. **Identity.** `child_session_id = request.id` (as at `:1057-1058`). `child_cwd = resolve_child_cwd(worktree_path.as_deref(), select_override_cwd(resume_source.as_ref(), request.cwd.as_deref()), &ctx.parent_cwd)` (`mod.rs:1879,1908`). `subagent_meta_dir = parent_session_dir.join("subagents").join(&request.id)` (as at `:1071-1087`).
2. **Prompt folding.** For a v2 spawn, `request.prompt` is a placeholder and the real first message is `request.runtime_overrides.native_agent.message` (`xai-grok-tools/.../native_agents.rs:345-369`, `types.rs:342-347`). If that message is present, `prompt = format!("{}\n\n{}", request.prompt, message.body)` (the native path seeds it into the conversation at `:1134-1147`). Otherwise `prompt = request.prompt`. Rhai `contract_prompt` wrapping is already inside `request.prompt`.
3. **Vendor model.** `vendor_model = match &definition.model { ModelOverride::Override(m) => m.clone(), Inherit => fail("mod agent '<type>' must declare `model`") }`. `effective_model_id = format!("{ns}/{vendor_model}")`. This is non-empty, which `notify_native_completion` requires (`coordinator/native.rs:598-602`).
4. **Start artifacts.** Build `SubagentMeta` (same 22 fields as `:1164-1190`: `effective_context_source: Some(if resume_key.is_some() {"resumed".into()} else {"new".into()})`, `context_normalized: false`, `fork_copy_error: None`, `prompt: request.prompt.clone()`, `effective_model_id: Some(effective_model_id)`). Build `SessionUpdate::SubagentSpawned` (fields as at `:1212-1232`, `context_normalized: false`, `model: Some(effective_model_id)`, `agent_address: advertised_address(...)` as at `:1207-1211`). Then:
   ```rust
   let boundary = if is_wake { PublicationBoundary::Started } else { PublicationBoundary::Prepared };
   let mut start_artifacts = StartArtifactPublication::new(boundary, (!is_wake).then(|| spawn_prepare_span.span().clone()),
       PreparedStartArtifacts { meta_dir, meta, spawned, advertised_address, reasoning_effort, role_name, parent_prompt_id },
       gateway.clone(), reporter.clone(), &ctx);
   start_artifacts.publish_at(PublicationBoundary::Prepared);
   if !is_wake { completion_data.mark_spawned_notification_emitted(); }
   ```
   (`start_artifact_publication.rs:18-21,23-31,53-62,81-86`; `handle_request.rs:1233-1258`.)
5. **Bridge tool set** (§6.6). `read_only = effective_runtime.capability_mode == Some(SubagentCapabilityMode::ReadOnly)`. If `capabilities.mcp_bridge && (!read_only || capabilities.mcp_in_read_only)`, build the tool list from `resolve_inherited_mcp_pool(ctx.parent_mcp_pool.take(), &definition.mcp_inheritance)` plus `ctx.parent_tool_definitions`. Otherwise use an empty list.
6. **Run.** Construct `ShellModHost` (holds `reporter`, `gateway`, `ctx.parent_session_id`, `request.id`, `attempt_id`, the started-child metadata, `is_wake`, a `Mutex<Option<StartArtifactPublication>>`, `completion_data.clone()`, and the filtered pool) and call `xai_grok_mods::run_mod_child(input, host).await`.
7. **`ShellModHost::on_started`.** Builds
   ```rust
   StartedChild { child_session_id: request.id.clone(), persona: effective_runtime.persona.clone(),
       resumed_from: request.resume_from.clone(), child_cwd: child_cwd_string,
       worktree_path: worktree_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
       effective_model_id: effective_model_id.clone(), definition_background: definition.background.unwrap_or(false),
       control: ShellChildRuntime::Mod(control) }
   ```
   (all 8 fields, `coordinator_state.rs:103-115`), then:
   - Ordinary: `reporter.started(child).await` → bool (`coordinator_state.rs:346`).
   - Wake: `if !reporter.started_deferred(child).await { return false }`. Then `if !reporter.settle_deferred_start(true).await { let _ = reporter.settle_deferred_start(false).await; start_artifacts.forbid_terminal_persistence(); return false }`. Then `if start_artifacts.publish_at(PublicationBoundary::Started) { completion_data.mark_spawned_notification_emitted() }`. This is the same order as `:1971-2077`, minus `WakeStart` transcript persistence, which has no analogue because there is no Grok child transcript.
   - The coordinator itself performs initial and queued mailbox flushes after promotion by calling `deliver_initial_message` / `deliver_followup` on the control (`coordinator.rs:926,1089-1096`). The adapter never calls them.
8. **Finalize.** When the sidecar reports turn idle (§6.4), `run_mod_child` calls `host.finalizing()` (`reporter.finalizing()`, `coordinator_state.rs:395`; the native path does the same at `:2156`). It then drains any committed-but-unconsumed messages before accepting the terminal frame. `admission_uncertain = promoted && !finalizing_ok`, which is surfaced as a `result.warnings` entry.
9. **Terminal persistence.** Mirrors `:2407-2412` and `:2553-2577`:
   ```rust
   if start_artifacts.terminal_persistence_allowed() {
       completion_data.set_persisted_output_dir(persist_subagent_output(&meta_dir, &result));
       persist_subagent_completion(&meta_dir, &result, &gcs_upload_ctx);
   }
   completion_data.set_telemetry_tokens(result.tokens_used);
   if let Some(wt) = &worktree_path && snapshot_enabled {
       let d = dispose_worktree_after_completion(wt, &resolve_subagent_source_repo(&ctx), &meta_dir, &final_status, &request.id).await;
       if d.worktree_removed() { result.worktree_path = None }
       disposed = d.snapshot_ref().map(str::to_owned);
   }
   return child_run_output(result, completion_data, disposed);
   ```
   `gcs_upload_ctx` is built as at `:1191-1206`, with `model_id = Some(effective_model_id)`. `present_child_completion` (`spawn.rs:448+`) emits `SubagentFinished`, because the spawned notification was marked.
10. **Telemetry.** Emit `SubagentLaunched` (as at `:1662-1677`, with `mcp_inherited_count = bridged server count`, `mcp_owned_count = 0`, `skills_inherited_count = 0`) and `SubagentCompleted` (as at `:2459-2480`).

### 5.5 Unsupported request shapes (fail-closed or explicit)
- `request.fork_context == true`: the mod runs fresh and pushes the warning "fork_context is not supported by mod subagents; started fresh".
- Agent-owned `mcpServers` in mod agent frontmatter are ignored, with a warning. Only the inherited pool is bridged.
- Encrypted native messages are refused by `accepts_native_message` (§6.3).

### 5.6 Usage
Vendor tokens populate `SubagentResult.tokens_used` and `completion_data.set_telemetry_tokens` for display in `SubagentFinished`. Nothing is folded into the parent xAI prompt bill.

---

## 6. Sidecar Protocol v1 (`protocol.rs`)

### 6.1 Transport
- Control runs over stdin/stdout NDJSON, one JSON object per line, max 1 MiB per line. Non-JSON stdout lines are logged at `debug!` and dropped.
- stderr goes to a 64 KiB ring, which is included in synthesized failure errors.
- The MCP bridge (optional) is a per-run Unix socket at `$TMPDIR/gm-<8 hex>.sock`. The path is asserted to be under 100 bytes; otherwise the run falls back to `/tmp/gm-<8hex>.sock`. The socket has mode `0600` and is authenticated by a 32-byte random hex `bridge_token`.

### 6.2 Host → sidecar
```rust
#[derive(Serialize)] #[serde(tag = "type")]
pub enum HostFrame {
    #[serde(rename = "run.start")]
    Start { protocol: u32, subagent_id: String, parent_session_id: String, subagent_type: String,
            agent_name: String, model: String, cwd: String, prompt: String,
            system_prompt: Option<String>, resume_vendor_session_id: Option<String>,
            read_only: bool, reasoning_effort: Option<String>,
            bridge: Option<BridgeInfo>, bridged_tools: Vec<BridgedTool> },
    #[serde(rename = "run.message")]
    Message { message_id: String, mode: MessageMode, from: String, text: String },
    #[serde(rename = "run.drain")]   Drain,
    #[serde(rename = "run.finish")]  Finish,
    #[serde(rename = "run.interrupt")] Interrupt,
    #[serde(rename = "run.cancel")]  Cancel { reason: Option<String> },
}
#[derive(Serialize)] #[serde(rename_all = "snake_case")]
pub enum MessageMode { Steer, Queue, Followup }
#[derive(Serialize)] pub struct BridgeInfo { pub socket_path: String, pub token: String }
```

### 6.3 Sidecar → host
```rust
#[derive(Deserialize)] #[serde(tag = "type")]
pub enum SidecarFrame {
    #[serde(rename = "run.started")]  Started { vendor_session_id: String, effective_model: Option<String> },
    #[serde(rename = "run.progress")] Progress { turns: u32, tool_calls: u32, tokens_used: u64,
                                                tool: Option<VendorToolActivity> },
    #[serde(rename = "run.consumed")] Consumed { message_id: String },
    #[serde(rename = "run.idle")]     Idle { final_text: String },
    #[serde(rename = "run.completed")] Completed { final_text: String, turns: u32, tool_calls: u32, tokens_used: u64 },
    #[serde(rename = "run.failed")]   Failed { error: String, cancelled: bool, turns: u32, tool_calls: u32, tokens_used: u64 },
}
#[derive(Deserialize, Clone)] pub struct VendorToolActivity { pub name: String, pub summary: String }
```

### 6.4 Run state machine (`session.rs`)
```
Spawned ──run.start──► Starting ──run.started──► on_started()
   false ⇒ send run.cancel, kill, outcome{promoted:false, cancelled:true}
   true  ⇒ Running
Running ──run.progress──► host.on_progress
Running ──run.consumed{id}──► pending.remove(id)
Running ──run.idle{text}──► last_text=text; ok = host.finalizing().await   // admission now closed
        if pending non-empty ⇒ send run.drain; stay Running (sidecar runs another turn)
        else ⇒ send run.finish ⇒ Finishing
Finishing ──run.completed──► Terminal(success)
any ──run.failed──► Terminal(failure)
any ──stdout EOF / exit──► Terminal(failure "mod sidecar exited (<code>): <stderr tail>")
any ──input.cancellation fired | control.cancel()──► send run.cancel; after graceful_kill_ms SIGKILL group ⇒ Terminal(cancelled)
any ──timeout_secs elapsed──► same as cancel, error "mod sidecar timed out after <n>s"
```
- `pending: Arc<Mutex<HashSet<String>>>` holds message ids **committed** via `send_active_message` or `deliver_*` and not yet `run.consumed`.
- After `finalizing()` returns, the coordinator will not admit new sends (`coordinator_state.rs:395-416`). So `pending` can only shrink, and the drain loop terminates. Each `run.drain` makes the sidecar run one turn over its queued messages, then emit `run.idle` again.
- `run.completed.final_text` is authoritative for `SubagentResult.output`. The host never appends wrappers, which keeps Rhai `validate_contract_output` input byte-exact.

### 6.5 `ModChildControl` (`control.rs`)
```rust
#[derive(Clone)]
pub struct ModChildControl {
    tx: mpsc::UnboundedSender<HostFrame>,      // unbounded ⇒ sync send, no fullness
    pending: Arc<Mutex<HashSet<String>>>,
    progress: Arc<Mutex<SubagentProgress>>,
    live_steer: bool,
    cancel: CancellationToken,
    closed: Arc<AtomicBool>,                   // set on Terminal
}

impl ChildControl for ModChildControl {
    type ProgressFuture = LocalBoxFuture<SubagentProgress>;

    fn progress(&self) -> Self::ProgressFuture {
        let p = self.progress.lock().clone();
        Box::pin(std::future::ready(p))
    }

    fn send_active_message(&self, d: ActiveAgentMessageDelivery) -> SendBoxFuture<ActiveMessageAdmission> {
        // Pre-commit checks: a refusal here leaves the lease Open, so the coordinator's
        // settle(Rejected/ChannelClosed/Unsupported) revokes it cleanly (active_message.rs:270-290).
        if !self.live_steer { return Box::pin(std::future::ready(ActiveMessageAdmission::Unsupported)); }
        if self.closed.load(Acquire) || self.tx.is_closed() {
            return Box::pin(std::future::ready(ActiveMessageAdmission::ChannelClosed));
        }
        let m = d.message();                    // &ActiveAgentMessage { message_id, sender_session_id, text: Arc<str> }
        let frame = HostFrame::Message {
            message_id: m.message_id.clone(),
            mode: match d.operation() { ActiveAgentMessageOperation::Steer => MessageMode::Steer,
                                        ActiveAgentMessageOperation::Queue => MessageMode::Queue },
            from: m.sender_session_id.clone(),
            text: m.text.to_string(),
        };
        let id = m.message_id.clone();
        let (tx, pending) = (self.tx.clone(), self.pending.clone());
        // Synchronous protected insertion inside the lease (coordinator_state.rs:64-66).
        let admitted = d.commit_admission(|| {
            pending.lock().insert(id.clone());
            tx.send(frame).is_ok()
        });
        let out = match admitted {
            Some(true) => ActiveMessageAdmission::Admitted,
            None => ActiveMessageAdmission::Rejected,          // lease already revoked/claimed: Open-state settle path
            Some(false) => { pending.lock().remove(&id); ActiveMessageAdmission::ChannelClosed }
            // ^ committed-then-closed race: settle returns false ⇒ coordinator reports AdmissionUncertain,
            //   which is the truthful outcome (same semantics as parent_message.rs:285-302).
        };
        Box::pin(std::future::ready(out))
    }

    fn cancel(&self) { self.cancel.cancel(); }
    fn interrupt(&self) -> bool { !self.closed.load(Acquire) && self.tx.send(HostFrame::Interrupt).is_ok() }

    fn accepts_native_message(&self, m: &AgentMailboxMessage) -> bool {
        !m.native.as_ref().is_some_and(|n| n.encrypted)
    }
    fn deliver_initial_message(&self, m: &AgentMailboxMessage) -> bool { self.followup(m) }
    fn deliver_followup(&self, m: &AgentMailboxMessage) -> bool { self.followup(m) }
}

impl ModChildControl {
    fn followup(&self, m: &AgentMailboxMessage) -> bool {
        if !self.accepts_native_message(m) || self.closed.load(Acquire) { return false; }
        self.pending.lock().insert(m.message_id.clone());
        let ok = self.tx.send(HostFrame::Message {
            message_id: m.message_id.clone(), mode: MessageMode::Followup,
            from: m.from_agent_id.clone(), text: m.body.clone(),
        }).is_ok();
        if !ok { self.pending.lock().remove(&m.message_id); }
        ok
    }
}
```
Field names follow `ActiveAgentMessage` (`active_message.rs:154-158`) and `AgentMailboxMessage` (`types.rs:281-291`). The admission future always resolves immediately, which is well within `ACTIVE_MESSAGE_ADMISSION_TIMEOUT` (`coordinator_state.rs:46`). `progress` is updated by the run loop on each `run.progress`.

### 6.6 Vendor session resume (`store.rs`)
`~/.grok/mods/<ns>/sessions.json` is a `HashMap<subagent_id, {vendor_session_id, parent_session_id, subagent_type, updated_at}>`. It is written atomically via tmp file and rename, under an advisory `flock` on `sessions.json.lock`.

`resume_key` selection in the adapter:

| Case | Coordinator input | `resume_key` |
|---|---|---|
| Idle wake via `send_message` | same `request.id`, `is_wake` | `request.id` |
| Task/v2 explicit resume | new `request.id`, `resume_from = Some(prev)` | `prev` |
| Rhai contract retry (`host_service.rs:463-485`) | new id, `resume_from = Some(first.child_session_id)` | `first.child_session_id` (== first `request.id`) |
| Fresh spawn | — | `None` |

On `run.started` and `run.completed`, the store upserts `request.id → vendor_session_id`. A lookup miss starts fresh and adds the warning "vendor session for <key> not found; started fresh". An entry whose `parent_session_id` differs from the current one is treated as a miss. `run_shell_child` has already validated ownership through `durable_resume_source_for` (`mod.rs:1918-1954`), which reads the `meta.json` written in §5.4 step 4.

### 6.7 MCP bridge (`bridge.rs` + shell `call_mcp_tool`)
- **Tool list (shell side).** Let `servers = pool.server_names()` after `resolve_inherited_mcp_pool` and after removing `mcpBridge.denyServers` (`SharedMcpPool`, `xai-grok-mcp/src/servers.rs:1105-1134`). Then, for each `ToolSpec` in `ctx.parent_tool_definitions` (`mod.rs:369`, populated for every spawn at `spawn.rs:147-156`) whose `name` starts with `"{server}__"` for some server in `servers`, emit
  `BridgedTool { server, tool: name["{server}__".len()..], name, description, input_schema: spec.parameters }`.
  MCP registration names are `<server>__<tool>` (`servers.rs:1458,4405`). If the parent definitions are unavailable, the list is empty and the bridge is not started.
- **Call (shell side).** `pool.get_client(server)?.call_tool(tool, args).await` (`servers.rs:4432`), with the result serialized via `serde_json::to_value(CallToolResult)`. Errors become strings.
- **Wire** (one NDJSON request/response per line; connections may pipeline):
  ```json
  {"id":"r1","token":"<hex>","method":"tools/call","server":"github","tool":"search_issues","arguments":{}}
  {"id":"r1","ok":true,"result":{...CallToolResult...}}
  {"id":"r1","ok":false,"error":"unknown tool"}
  ```
  The bridge rejects a bad token (closes the connection) and any `(server, tool)` not in `bridged_tools`. Each call has a 120 s timeout. Up to 8 calls may be in flight per run.
- **Read-only boundary.** The host enforces it by not starting the bridge when `read_only && !mcp_in_read_only`. The vendor SDK's own file and shell tools are configured read-only by `runner.mjs` when `read_only` is true. That is a sidecar-side guarantee: Grok cannot inspect vendor-internal tool calls. Worktree isolation is the host-enforced containment for writes.

---

## 7. Process Lifecycle and Security (`process.rs`)
- Spawn: `tokio::process::Command::new(program).args(args).current_dir(cwd).env_clear().envs(allowlisted).env("GROK_MOD_NAMESPACE", ns).env("GROK_MOD_SUBAGENT_ID", id).stdin(piped).stdout(piped).stderr(piped).kill_on_drop(true)`. On Unix, also `.process_group(0)`.
- Interrupt sends `run.interrupt` only, which the sidecar maps to an SDK abort of the current turn. Cancel and timeout send `run.cancel`, wait `graceful_kill_ms`, then `libc::kill(-pgid, SIGKILL)` on Unix (`start_kill()` elsewhere). The socket file is always unlinked by a drop guard.
- Secrets not in `envAllowlist` (for example `XAI_API_KEY`, `GITHUB_TOKEN`) never reach the sidecar.
- Trust: only trusted plugins run sidecars (§4.6). Command and arg containment follows §4.5.

---

## 8. `plugins/cursor` Sidecar Contract
- `runner.mjs` reads `run.start` and constructs a `@cursor/sdk` agent (`model`, `cwd`, `systemPrompt`, read-only mode if `read_only`, resume via `resume_vendor_session_id`). It registers `bridged_tools` as SDK custom tools that proxy to the bridge socket, and emits `run.started`.
- `run.message`: `steer` injects into the active turn when the SDK supports it, otherwise it is queued. `queue` and `followup` are queued for the next turn. The runner emits `run.consumed` when a queued message is included in a turn.
- At the end of each turn it emits `run.idle{final_text}`. On `run.drain` it runs a turn over the queued messages. On `run.finish` it emits `run.completed` and exits 0.
- Port sources: `refs/netbrah-cursor-subagents/sidecar/runner.mjs` (SDK usage, session resume), `sidecar/mcp-schemas.mjs` (deny list, env scrub patterns), `mod.ts` (handback, which is dropped because the host never wraps output).

---

## 9. Verification Matrix

| ID | Layer | Test |
|---|---|---|
| T1 | `manifest.rs` unit | namespace≠plugin rejected; `..`/absolute/symlink-escape command and args rejected; bare `node` resolves via `which`; unknown field rejected |
| T2 | `control.rs` unit | `send_active_message` returns `Unsupported` when `!live_steer`, `ChannelClosed` when closed (lease untouched), `Admitted` with frame sent and id in `pending` inside `commit_admission`; revoked lease ⇒ `Rejected` with nothing sent. Uses `ActiveAgentMessageDelivery` built via the `xai-grok-tools` test-support constructor, or exercised through a `SubagentCoordinator` test harness if the constructor is `pub(crate)` (resolved in the plan) |
| T3 | `control.rs` unit | encrypted message refused by `accepts_native_message`/`deliver_*`; followup sends `MessageMode::Followup` with `from_agent_id`/`body` |
| T4 | `store.rs` unit | atomic write; wake key = `request.id`; resume key = `resume_from`; foreign parent ⇒ miss |
| T5 | `bridge.rs` unit | path < 100 bytes; bad token closes the connection; unlisted tool rejected; 8-way concurrency |
| T6 | `session.rs` + mock sidecar | start→progress→idle→finish→completed; on_started=false ⇒ cancel + kill; EOF ⇒ synthesized failure with stderr tail; timeout; cancel token ⇒ SIGKILL of group (grandchild reaped) |
| T7 | `session.rs` + mock sidecar | message committed after `run.idle` but before `finalizing` returns ⇒ `run.drain` issued, `run.consumed`, then finish |
| T8 | shell `mod_adapter` tests (with `run_shell_child_harness`) | Task spawn writes `meta.json` (running → completed) and `output.json`; `SubagentSpawned` once and `SubagentFinished` once; worktree isolation snapshot ref recorded |
| T9 | shell | v2 spawn folds `native_agent.message.body` into `run.start.prompt`; `send_message` while running ⇒ `Admitted`; idle wake ⇒ `started_deferred` → `settle_deferred_start(true)` → `publish_at(Started)` and resume key = `request.id` |
| T10 | shell | Rhai: invalid JSON first, valid on retry ⇒ second `run.start.resume_vendor_session_id` equals the first run's vendor id |
| T11 | shell | untrusted mod plugin ⇒ failure result, no process spawned; non-mod plugin agent ⇒ native path unchanged (existing tests green) |
| T12 | budget | `git diff --stat <base> -- ':!crates/codegen/xai-grok-mods' ':!**/mod_adapter.rs' ':!plugins' ':!Cargo.lock'` ≤ 85 changed lines |

---

## 10. Risks and Open Questions
- R1: The `ActiveAgentMessageDelivery::new` constructor is `pub(crate)` (`active_message.rs:170`), so T2 cannot build one directly from `xai-grok-mods`. The mitigation is to exercise T2 through `SubagentCoordinator` (public API) with a stub `ChildRunner`. If that proves impractical, the plan may add a `#[cfg(feature = "test-support")]` public constructor to `xai-grok-tools`; this costs +6 lines and is counted against the budget.
- R2: The `<server>__<tool>` naming in `parent_tool_definitions` may be sanitized for some servers. A mismatch only hides that tool from the bridge (fail-safe). T5 and dogfood confirm.
- R3: Cursor SDK steer support: if live injection is unavailable, `steer` degrades to queue semantics inside the sidecar. The admission contract is unaffected.
- R4: Upstream churn in `handle_request.rs` around `:886-923`. The dispatch block is self-contained, so conflict resolution is mechanical.

---

## 11. Review Findings → Resolution

| Finding | Resolution |
|---|---|
| R1#1/#9 crate cycle, shell types | §3.1 leaf crate on `xai-grok-tools`; `ModHost` seam §5.2 |
| R1#2 / R2-3 catalog, `cli_agents` is a `Vec`, lazy registry | §4.2 file-based catalog via existing plugin discovery; no `cli_agents`/MvpAgent edit |
| R1#3 command containment | §4.5 rules 3-4 |
| R1#4 / R2-2 admission lease, `Delivered`, settle semantics, field names | §6.5 pre-checks before commit; `Admitted` only after commit; `ActiveAgentMessage`/`AgentMailboxMessage` real fields |
| R1#5 wake / followup | §5.4 step 7 order matches `:1971-2077`; §6.6 key table; coordinator owns mailbox flush |
| R1#6 lifecycle / gating | §5.3 dispatch after `:911`, reusing gating, resume, worktree, cwd |
| R1#7 Rhai output | §6.4 output byte-exact; §6.6 retry key |
| R1#8 / R2-5 MCP bridge API | §6.7 uses `server_names`/`get_client`/`call_tool`, `parent_tool_definitions`, `<server>__<tool>` |
| R2-1 wrong crate, future lifetimes, `shutdown`, invented fields | §3.1, §5.1 (7 methods, real fields untouched), §5.2 `BorrowedSendFuture<'a, T>` for the seam only |
| R2-4 dispatch locals, `StartArtifactPublication::new`, `PublicationBoundary`, `StartedChild` 8 fields, `bool` returns, v2 placeholder prompt | §5.3 locals list; §5.4 steps 2, 4, 7 |
| R2 adv. host_service path | §1.1 corrected to `session/workflow/host_service.rs:434` |
| R2 adv. visibility | §1.2 child-module access noted |
| R2 adv. telemetry tokens | §5.6 and §5.4 step 9 |
