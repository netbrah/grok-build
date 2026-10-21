## Spec Review (Round 2)

**Spec:** `docs/superpowers/specs/2026-10-20-grok-build-mod-subagent-middleware-design.md` (v2)
**Ground truth:** `refs/netbrah-grok-build` @ `feat/hts-native-shipping` / `2fcd9729`
**Round 1:** `2026-10-20-grok-build-mod-subagent-middleware-review-r1.md`

**Status:** Issues Found

v2 fixes the R1 architecture problems: the crate cycle, namespace rule, command containment, Rhai retry and contract handling, MCP bridge scope, and `read_only` derivation. The remaining blockers sit in the code-level contracts. Several types, trait signatures, enum variants and struct fields in §3, §4.5, §5 and §6.4 don't match the repo. An implementation plan built on them would fail to compile, and in two places (the lease settle and `effective_model_id`) it would also behave wrongly at runtime.

All paths below are relative to `refs/netbrah-grok-build/crates/codegen/` unless they start with "spec".

---

### Round 1 resolution matrix

| R1 | Topic | v2 status |
|---|---|---|
| #1 | Crate dependency cycle | **Resolved in direction.** `xai-grok-agent` is untouched, mods no longer depends on agent or shell, and shell implements `ModHostBridge` (spec:68). The named dependency crate is wrong, though (see Issue 1). |
| #2a | `AgentDefinition` construction / `ModelOverride` | **Resolved.** Construction now goes through the parse/constructor path. |
| #2b | Catalog load hook and lifecycle | **Partially resolved.** A hook now exists (spec:88, :294), but it assumes the wrong container type and the wrong lifecycle point (Issue 3). |
| #2c | `plugin.name == namespace` | **Resolved.** Matches `xai-grok-agent/src/discovery.rs:329-357, 432-460`. |
| #3 | Bare `node` vs containment | **Resolved.** §4.4 Case A uses an interpreter allowlist plus `which`; Case B uses `dunce::canonicalize` plus `starts_with`. |
| #4 | Lease ordering | **Partially resolved.** `commit_admission` now comes before the ack wait, but the settle/return semantics after commit are wrong and use a variant that doesn't exist (Issue 2). |
| #5 | Wake/followup path | **Partially resolved.** Same-`request.id` wake, `started_deferred` / `settle_deferred_start` and `accepts_native_message` are now covered. The mailbox-flush mechanism and the `ChildReporter` API are described incorrectly (Issue 4). |
| #6 | Lifecycle and presentation parity | **Partially resolved.** The spec now covers `StartArtifactPublication`, `meta.json`, `mark_spawned_notification_emitted`, gating and the `SubagentResult` mapping. The `StartArtifactPublication` / `StartedChild` / `PublicationBoundary` APIs are wrong (Issue 4). |
| #7 | Rhai structured output and retry | **Resolved.** `appendHandback: false` by default; retry uses `resume_from = previous child_session_id`. |
| #8 | Tool bridge scope and `read_only` | **Resolved in design.** The MCP pool API details are inaccurate (Issue 5). |
| #9 | Upstream budget | **Re-baselined.** The per-file touch table needs recounting after the fixes to Issues 1–4 (for example, the mod.rs registration point moves). |
| Adv. ×6 | Terminology, sun_path, YAGNI fields, sessions.json lock, line drift | **Resolved.** One line-drift and path regression remains (see Recommendations). |

---

**Issues (blocking planning):**

1. **[§1.2 spec:24, §3.1 spec:64-65 and :71-73, §5.1 spec:319-394, §5.3 spec:445/:457/:465, §6.4 spec:636/:642]: wrong crate, wrong future aliases, an extra trait method, and invented struct fields. None of these would compile.**
   - **Wrong crate.** `xai-grok-shell-base` contains only `lib.rs`, `env.rs`, `cpu_profile.rs` and `util/` (`xai-grok-shell-base/src/lib.rs:1-8`). It has no `subagent_coordinator/` module, so the paths at spec:24 and spec:636 don't exist. The real locations are:
     - `LocalBoxFuture`, `SendBoxFuture`, `SubagentProgress`, `ChildControl` and `StartedChild` live in `xai-grok-tools/src/implementations/grok_build/task/coordinator_state.rs:22-115`.
     - `ActiveAgentMessage*` and `ActiveMessageAdmission` are re-exported from `task/active_message.rs` via `task/types.rs:32-37` and `coordinator_state.rs:37`.
     - `AgentMailboxMessage` is at `types.rs:281`.
     - `DeliveryLease` doesn't exist anywhere. The lease is `ActiveMessageAdmissionLease`, `pub(crate)` in `active_message.rs:228`, so it isn't reachable from mods.

     **Fix:** `xai-grok-mods` should depend on `xai-grok-tools`. It is already a dependency of agent and shell and doesn't depend on either, so the graph stays acyclic. Drop `xai-message-delivery-core` and redraw the diagram at spec:71-73.
   - **The future aliases take no lifetime.** They are declared as `pub type LocalBoxFuture<T> = Pin<Box<dyn Future<Output = T> + 'static>>;` and `pub type SendBoxFuture<T> = ... + Send + 'static>>;` (`coordinator_state.rs:22-23`). So `LocalBoxFuture<'_, _>` and `SendBoxFuture<'static, _>` at spec:337, :347, :389, :445, :457, :465 and :642 are E0107 errors. `ModHostBridge` methods that borrow `&self` need a crate-local alias `Pin<Box<dyn Future<Output=T> + Send + 'a>>`, or else owned/`Arc` captures. `ChildControl::progress` must return the associated `type ProgressFuture: Future<Output = SubagentProgress> + 'static` (`coordinator_state.rs:60-62`), not a boxed borrowed future.
   - **`ChildControl` has no `shutdown`.** The trait has exactly 7 methods: `progress`, `send_active_message`, `cancel`, `interrupt`, `accepts_native_message`, `deliver_initial_message` and `deliver_followup` (`coordinator_state.rs:59-100`, implemented at `xai-grok-shell/src/agent/subagent/child_runtime.rs:29-133`). The `fn shutdown` at spec:389-394 is E0407. Either move teardown to an inherent method on `ModChildControl`, or have `cancel()` / Drop handle it.
   - **`NativeShellChildRuntime` fields are invented.** spec:319-329 lists `handle`, `thread`, `notification_rx`, `progress`, `is_cancelled`, `is_interrupted`, `cancel_token`, `child_session_id` and `native_enabled`. The real fields are `child_cmd_tx`, `message_delivery`, `active_message_target_session_id`, `child_signals`, `_child_thread`, `receipt_sink`, `active_message_parent_session_id`, `active_message_parent_prompt_index`, plus the test-only `force_queue_envelope` (`child_runtime.rs:14-27`). The refactor should rename the existing struct verbatim; it shouldn't introduce a new field set. This matters for the line budget and for the constructor sites R1 #9 flagged (`handle_request.rs:~1956`, `child_runtime_tests.rs:86`).

2. **[§6.4 spec:636-676 and :689-705]: `control.rs` uses a variant that doesn't exist, wrong field names, and post-commit outcomes that break the lease state machine.**
   - **`ActiveMessageAdmission::Delivered` doesn't exist.** The variants are `Admitted`, `Unsupported`, `ChannelClosed` and `Rejected` (`active_message.rs:212-217`). Spec:671 should use `Admitted`.
   - **Returning anything except `Admitted` after `commit_admission` makes the outcome "uncertain".** The sequence is:
     1. `commit_admission` moves the lease `Open → Claimed → Committed` (`active_message.rs:253-268`).
     2. When the future resolves, `ActiveMessageFuture::poll` calls `lease.settle(admission)` (`coordinator/active_message.rs:261`).
     3. On a `Committed` lease, `settle(Rejected | ChannelClosed | Unsupported)` returns `false` (`active_message.rs:270-290`).
     4. That `false` becomes `ActiveAgentMessageOutcome::AdmissionUncertain` (`coordinator/active_message.rs:186-188`).
     5. That in turn marks child finalization `TerminalDrainDisposition::Uncertain` (`coordinator/active_message.rs:48-52, 824-837`).

     **The spec must state two rules:**
     - (a) Check stdin-writer liveness or capacity *before* `commit_admission`, and return `ChannelClosed` / `Rejected` from that pre-check without committing. This mirrors `child_runtime.rs:64-103`.
     - (b) Once the commit closure has enqueued `RunSteer`, the future must resolve to `Admitted`. A later negative `steer_ack` or a disconnect can only be logged or surfaced as a warning; it can't be reported as `Rejected`.

     As written, the `Ok(false)`, `Err` and timeout arms at spec:671-676 make every vendor NACK taint finalization.
   - **Field names are wrong.**
     - `delivery.message()` returns `&ActiveAgentMessage { message_id: String, sender_session_id: String, text: Arc<str> }`, with accessors `delivery.message_id()` and `delivery.text()` (`active_message.rs:154-184`). The spec uses `msg.id`, `msg.from` and `msg.text` (spec:651, :655-656).
     - `AgentMailboxMessage` has `message_id`, `from_agent_id` and `body` (`types.rs:281-291`). The spec uses `message.id`, `message.from` and `message.text` (spec:701, :703-704).

3. **[§3.2 spec:88, §4.5 spec:291-307]: `cli_agents` is a `Vec`, and the plugin registry is populated lazily, not at session start.**
   - **Container type.** The field is `pub cli_agents: Vec<xai_grok_agent::config::AgentDefinition>` (`xai-grok-shell/src/agent/config.rs:1499`), and the validation context collects names with `.iter().map(|d| d.name.clone())` (`xai-grok-shell/src/agent/mvp_agent/subagent_spawn.rs:62`). So `.keys()` (spec:291) and `.entry(..).or_insert(..)` (spec:307) don't compile. The insert needs to be `if !cfg.cli_agents.iter().any(|d| d.name == qualified_name) { cfg.cli_agents.push(def) }`, plus a defined replace/remove rule for when a background catalog refresh changes the model list.
   - **Lifecycle.** `MvpAgent.plugin_registry_handle` is empty at construction (`mvp_agent/agent_ops.rs:2325`), because plugin discovery is deferred until after ACP `initialize`. It gets filled in three places:
     - `ensure_plugin_registry` (`agent_ops.rs:388-406`)
     - `SessionCreatePrefetch::launch_from_meta` (`mvp_agent/session_setup.rs:358, 1035`)
     - `/plugins reload` (`mvp_agent/mod.rs:1933`)

     "On session startup (in `MvpAgent`)" (spec:294) and the single mod.rs call (spec:88) therefore run against an empty registry, or never re-run after reload. The spec has to pick one of two approaches:
     - call `register_mod_catalog_agents` at each population/reload point, or
     - inject at read time inside `build_subagent_validation_context` / `try_build_subagent_spawn_context`, which already call `self.plugin_registry_handle.snapshot()` (`subagent_spawn.rs:68, 347`) and clone `cfg` into `agent_config` (`:361`).

     The choice changes the upstream touch table.

4. **[§5 spec:409-428, §5.4 spec:473-488, §6.2 spec:531]: the dispatch-site locals, `StartArtifactPublication`, `StartedChild` and `ChildReporter` APIs don't match `handle_request.rs`, and initial-prompt seeding is wrong.**
   - **`ModChildInvocation` at the 885/886 dispatch point.**
     - `wake_origin` was already moved by `match wake_origin { Some(origin) => ... }` (`xai-grok-shell/src/agent/subagent/handle_request.rs:369-372`). Passing it at spec:418 is a use-after-move. Pass `wake_message_source` / `wake_message_id` (plus `is_wake`) instead.
     - The struct omits locals the mod path needs:
       - `cancel_token` (destructured from `run.cancellation`, `:359`)
       - `gateway: GatewaySender` (`:351`)
       - `attempt_identity` / `attempt_id` (`:361`, `:394`)
       - `agent_address` (`:365`)
       - `spawn_prepare_span` (`:392`)
       - `child_depth` (`:886`)
       - `worktree_freshly_created` (needed for cleanup when start fails)

     Without `gateway`, `run_mod_child` can't build `StartArtifactPublication` or emit `SubagentFinished`. Without `cancel_token`, it can't observe cancellation. (`start` and `spawn_timer` are in scope at `:387-388`, so those two are fine.)
   - **`StartArtifactPublication`.**
     - The constructor is `StartArtifactPublication::new(boundary, metadata_parent, prepared: PreparedStartArtifacts, gateway, reporter, &ctx)` (`start_artifact_publication.rs:53-62`). There is no `StartArtifactPublicationInput`, and it doesn't take `&mut completion_data` (spec:473).
     - `PublicationBoundary` is `{ Prepared, Started }` (`:17-21`); there's no `PreStart` (spec:474).
     - It doesn't call `mark_spawned_notification_emitted()`. The caller does that after `publish_at(Prepared)` for ordinary spawns (`handle_request.rs:1255-1258`) and after `publish_at(Started)` for wakes (`handle_request.rs:2072-2074`).
     - The spec must also specify how `mod_adapter` builds `PreparedStartArtifacts { meta_dir, meta: SubagentMeta, spawned: SessionUpdate, advertised_address, reasoning_effort, role_name, parent_prompt_id }` (`:23-31`).
     - On completion it must call `terminal_persistence_allowed()` before `persist_subagent_output` / `persist_subagent_completion` (`handle_request.rs:2407-2412`).
   - **`StartedChild` has 8 required public fields:** `child_session_id`, `persona`, `resumed_from`, `child_cwd`, `worktree_path`, `effective_model_id`, `definition_background` and `control` (`coordinator_state.rs:103-115`). The two-field literal at spec:477 is E0063. Leaving `effective_model_id` empty also has a runtime cost: `notify_native_completion` treats the v2 agent as never started and retires its `/root/<task_name>` entry from `native_names` (`task/coordinator/native.rs:598-602`). Spec:473 says the vendor model goes into publication metadata; it must also go into `StartedChild.effective_model_id`.
   - **`ChildReporter`.**
     - `started`, `started_deferred` and `settle_deferred_start` all return `bool` (`coordinator_state.rs:346, 352, 378`), so `outcome.is_admitted()` (spec:480, :486) doesn't exist.
     - `reporter.cancel_admitted_start()` (spec:483) doesn't exist anywhere in the repo. The cited `handle_request.rs:1994` is wake-start attempt plumbing.
     - The claim at spec:488 that `settle_deferred_start(true)` drains the mailbox through `deliver_followup` is also wrong. On a wake, the waking message becomes `request.prompt` of the re-run (`coordinator/wake.rs:~71-84`). Mailbox flush calls `deliver_initial_message` from the `Started` handling (`coordinator.rs:926, 1089-1096`).
   - **`NativeAgentSpawn.message` seeding (spec:44, :481-483, :531).** For v2 `spawn_agent`, `request.prompt` is a placeholder ("Complete the task in the agent message supplied with this turn."). The real task is in `request.runtime_overrides.native_agent.message` (`xai-grok-tools/.../grok_build/native_agents.rs:345-369`). The native path seeds that message into the child conversation *before* the first turn (`handle_request.rs:1134-1147`). The spec instead starts the sidecar on `request.prompt` and pushes the message afterwards via `deliver_initial_message`, so the vendor's first turn runs on the placeholder. `RunStart.prompt` must be built from `native_agent.message.body` when present, and the spec should reserve `deliver_initial_message` for coordinator mailbox flushes.

5. **[§5.3 spec:455-465, §6.6 spec:728-760]: the `SharedMcpPool` API doesn't support the listing and calling the bridge describes.**
   - `SharedMcpPool` exposes only `server_names()`, `get_client(name) -> Option<&Arc<McpClient>>` and `retain_clients(..)` (`xai-grok-mcp/src/servers.rs:1085-1135`). It has no tool-listing method.
   - `McpClient::get_tool_registrations` needs an `Arc<tokio::sync::Mutex<McpState>>` (`servers.rs:4306`), which `SubagentSpawnContext` doesn't carry. The spec must pick a schema source: either thread `McpState` through, or filter MCP entries out of `ctx.parent_tool_definitions` (snapshotted at `xai-grok-shell/src/agent/subagent/spawn.rs:150-156`).
   - Registered MCP names are `<server>__<tool>` (`MCP_TOOL_NAME_DELIMITER = "__"`, `servers.rs:1458, 4405`), not `mcp__<server>__<tool>` (spec:735).
   - `McpClient::call_tool(tool_name, arguments)` takes the raw tool name with the server prefix stripped, and returns `Result<rmcp::model::CallToolResult, McpError>` (`servers.rs:4432`). The spec must define the split rule and serialize the result with `serde_json::to_value`. Otherwise `mcp.call_tool` routing is ambiguous for server names that themselves contain `__`.

---

**Recommendations (advisory, do not block approval):**
- **Path regression at spec:27.** `xai-grok-shell/src/workflow/host_service.rs` doesn't exist. The real file is `xai-grok-shell/src/session/workflow/host_service.rs`: `spawn_agent` is at `:434` (not :413) and the `resume_from` handling is at `:448`, `:539-547` and `:577`. The interpreter crate directory is `xai-workflow`, not `xai-Workflow`.
- **Visibility.** `mod_adapter` is a child of `agent::subagent`, so it can already reach private items in `subagent/mod.rs`: `persist_subagent_output` (`:2845`), `persist_subagent_completion` (`:2849`), `child_run_output` (`:2335`), `resolve_inherited_mcp_pool` (`:1957`), and the `ShellCompletionData` setters (`:580`, `:592`). `start_artifact_publication` items are `pub(super)` and also reachable. No visibility edits are needed, so don't count them in the budget. Items private to sibling `handle_request.rs` would need `pub(super)`; `dispose_worktree_after_completion` (`handle_request.rs:2634`) is already `pub(crate)`.
- **Telemetry and usage.** `present_child_completion` reports `completion_data.telemetry_tokens()` (`spawn.rs:473`), and the native path also folds child usage into the parent bill (`handle_request.rs:2416-2451`). The spec should say whether mod children call `set_telemetry_tokens(tokens_used)` and whether vendor tokens are folded into or excluded from xAI billing. If they're excluded, it should also say whether `mark_child_usage_not_applied_with_fallback` is skipped.
- **Catalog refresh semantics.** Define whether a background `catalog.mjs` refresh affects only the next session or reload, or mutates `cfg.cli_agents` mid-session. This interacts with the choice in Issue 3.
- **Test matrix (spec:793, :796).** Once Issue 4 is fixed, the "idle wake + `deliver_followup`" test should become "wake re-run with message as prompt", and "`deliver_initial_message` emits Followup" should cover the mailbox flush on `Started`. Add a test that a negative `steer_ack` after commit still yields `ActiveAgentMessageOutcome::Accepted`, not `AdmissionUncertain`.
