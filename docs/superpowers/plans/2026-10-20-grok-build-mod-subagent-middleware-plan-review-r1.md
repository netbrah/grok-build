# Plan Review — Grok-Build Mod Subagent Middleware (`xai-grok-mods` + `plugins/cursor`) (Round 1)

- **Plan reviewed:** `docs/superpowers/plans/2026-10-20-grok-build-mod-subagent-middleware.md`
- **Reference spec:** `docs/superpowers/specs/2026-10-20-grok-build-mod-subagent-middleware-design.md` (Round 3 Approved)
- **Ground-truth codebase:** `refs/netbrah-grok-build` (`feat/hts-native-shipping` @ `2fcd97294a23cfb19b1d6e4c1a788840a17dc569`)

## Plan Review

**Status:** Issues Found

**Issues (if any):**

- **[Task 2, Step 2] (`crates/codegen/xai-grok-mods/src/control.rs` lines 783–794): Invalid `AgentMailboxMessageKind` variant and missing `NativeAgentMessage` fields in `sample_mailbox_msg` prevent compilation** —
  `sample_mailbox_msg` constructs `AgentMailboxMessage` with `kind: AgentMailboxMessageKind::Direct` and `native: Some(NativeAgentMessage { encrypted })`. Against `xai-grok-tools/src/implementations/grok_build/task/types.rs`:
  1. `AgentMailboxMessageKind` (`types.rs:249-254`) defines `{ Message, FollowupTask, NativeMessage, NativeFollowup }` — there is no `Direct` variant.
  2. `NativeAgentMessage` (`types.rs:300-306`) has four required fields (`pub author: String`, `pub recipient: String`, `pub encrypted: bool`, `pub trigger_prompt_id: Option<String>`), so `NativeAgentMessage { encrypted }` fails with `E0063`.
  Fix `sample_mailbox_msg` to use `kind: AgentMailboxMessageKind::NativeMessage` and `NativeAgentMessage { author: "/root".into(), recipient: "/root/worker".into(), encrypted, trigger_prompt_id: None }`.

- **[Task 1, Step 2–4], [Task 3, Step 1–2], and [Task 4, Step 2] (`crates/codegen/xai-grok-mods/src/lib.rs`): `src/lib.rs` code is omitted across all tasks and Task 3 imports root types (`ModHost`, `ModStarted`, `BorrowedSendFuture`) before Task 4 defines them** —
  1. Task 1 lists `Create: crates/codegen/xai-grok-mods/src/lib.rs` and runs `cargo test -p xai-grok-mods` in Step 4, but never provides the initial `src/lib.rs` file content.
  2. Task 3 (`bridge.rs` lines 865, 1004) imports `use crate::ModHost;` and `use crate::{BorrowedSendFuture, ModChildControl, ModStarted};` and runs `cargo test -p xai-grok-mods` in Task 3 Step 2, before Task 4 defines `ModHost`, `ModStarted`, and `BorrowedSendFuture`.
  3. Task 4 Step 2's heading states `Implement crates/codegen/xai-grok-mods/src/session.rs and lib.rs`, but only `session.rs` is shown in the code block (`lib.rs` is never shown).
  Provide the concrete `crates/codegen/xai-grok-mods/src/lib.rs` in Task 1 (and its updates in Tasks 2–4, defining `BorrowedSendFuture`, `ModHost`, `ModStarted`, `ModRunInput`, `ModRunOutcome` by Task 3 Step 1 and `run_mod_child` in Task 4 Step 2) so each task's `cargo test -p xai-grok-mods` step compiles and passes in order.

- **[Task 1, Step 3] (`crates/codegen/xai-grok-mods/src/manifest.rs` lines 373–390, 408–451): `t1` unit test is missing required Spec §9 T1 assertions (symlink-escape and unknown-field rejection) and arg validation deviates from Spec §4.5 rule 4** —
  1. Spec §9 T1 requires verifying: `namespace≠plugin rejected; ../absolute/symlink-escape command and args rejected; bare node resolves via which; unknown field rejected`. `t1_manifest_validates_namespace_and_containment` omits symlink-escape (a symlink inside the plugin root pointing outside `canonical_root`) and unknown-field (`deny_unknown_fields`) test assertions.
  2. Spec §4.5 rule 4 specifies: *"Any arg not starting with `-` that names an existing path relative to root is canonicalized and must `starts_with(canonical_root)`. Absolute paths and `..` are rejected outright."* In `manifest.rs:383-389`, `dunce::canonicalize(&joined)` is called unconditionally on every non-`-` arg even when `joined.symlink_metadata().is_err()` (a non-path literal arg), which would reject non-flag literal args. Check `joined.symlink_metadata().is_ok()` (so both existing files and symlinks — including broken or escaping symlinks — are canonicalized and checked with `starts_with(&root)`).

- **[Task 4, Step 2] (`session.rs`) & [Task 5, Step 2] (`mod_adapter.rs`): Missing Spec §5.4/§5.5/§6.6 warning propagation and three codebase-specific traps in `mod_adapter.rs`** —
  1. **Missing Spec warnings:**
     - Spec §5.5: When `request.fork_context == true`, start fresh and push `"fork_context is not supported by mod subagents; started fresh"` to `result.warnings`.
     - Spec §5.5: When `!definition.mcp_servers.is_empty()` (agent-owned `mcpServers` in frontmatter), ignore them and push a warning to `result.warnings`.
     - Spec §6.6: When `resume_key` is `Some(key)` and vendor session lookup misses (or `session_resume` is false), start fresh and push `"vendor session for <key> not found; started fresh"` to `result.warnings`. Currently `run_mod_child_with_store` (`session.rs:1262-1269`) silently maps a lookup miss to `None` without recording `resume_lookup_missed: bool` on `ModRunOutcome` or emitting the warning.
     - Spec §5.4 step 8: When `outcome.admission_uncertain` is `true`, push a warning to `result.warnings`.
  2. **Test harness `subagent_meta_dir` resolution (`handle_request.rs:465-482, 1075-1087`):**
     Task 5 Step 2 bullet 1 instructs `mod_adapter.rs` to set `subagent_meta_dir = parent_session_dir.join("subagents").join(&d.request.id)`. However, under `#[cfg(test)]`, `handle_request.rs:465-478` resolves wake `resume_source` from `ctx.run_shell_child_harness.as_ref().map(|h| h.meta_dir.as_path())`. If `mod_adapter.rs` does not honor `#[cfg(test)] d.ctx.run_shell_child_harness.as_ref().map(|h| h.meta_dir.clone())` (matching `handle_request.rs:1075-1087`), T8 and T9 (which Spec §9 runs with `run_shell_child_harness`) will write `meta.json` to the wrong path and wake (`is_wake == true`) will fail at `handle_request.rs:525`.
  3. **`ShellCompletionData` is not `Clone` (`mod.rs:545`):**
     `ShellCompletionData` does not derive `Clone` (and `mod.rs` has a 1-line edit budget). In `mod_adapter.rs`, `ShellModHost` should not attempt `completion_data.clone()`; instead, keep `completion_data` in `mod_adapter::run` (and either track a `spawned_emitted: AtomicBool` on `ShellModHost` or inspect `start_artifacts` after `run_mod_child` returns to call `d.completion_data.mark_spawned_notification_emitted()`).
  4. **`context_normalized` type on `SubagentMeta` and `SessionUpdate::SubagentSpawned` (`mod.rs:2608`, `notification.rs:693`):**
     Both fields have type `bool` (`false`), not `Option<bool>` (`None`).

- **[Task 4, Step 3], [Task 5, Step 2 & Step 4], and [Task 6, Step 1 & Step 2]: Major implementation and test files are left as prose summaries rather than concrete code** —
  While Tasks 1–3 and Task 4 Steps 1–2 provide complete copy-pasteable code, the remaining steps omit the implementation code for:
  1. `crates/codegen/xai-grok-mods/tests/mock_sidecar.rs` (Task 4 Step 3: T2 via `SubagentCoordinator`, T6 normal/crash/timeout/cancel-process-group, and T7 idle-finalizing-drain). Also note `mock-sidecar.mjs` in Task 4 Step 3 does not spawn a grandchild in `hang` mode to verify T6's process-group `SIGKILL` (grandchild reaped) requirement from Spec §9 T6.
  2. `crates/codegen/xai-grok-shell/src/agent/subagent/mod_adapter.rs` and its unit/integration tests T8–T11 (Task 5 Steps 2 & 4).
  3. `plugins/cursor/` files (`plugin.json`, `mod.json`, `agents/*.md`, `hooks/hooks.json`, `sidecar/package.json`, `sidecar/runner.mjs`, `sidecar/catalog.mjs`) in Task 6 Steps 1–2.
  Provide complete, self-contained code blocks for these files so an implementer can execute Tasks 4, 5, and 6 without guesswork.

**Recommendations (advisory, do not block approval):**

- **Clippy lints in `subagent` module (`mod.rs:13`):** `crates/codegen/xai-grok-shell/src/agent/subagent/mod.rs:13` has `#![deny(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]`, which applies to `mod_adapter.rs`. Passing a single `Dispatch` struct to `mod_adapter::run(d: Dispatch)` already satisfies this; ensure any helper functions inside `mod_adapter.rs` also stay within ≤ 7 parameters and ≤ 3 `bool` parameters (or use `#[allow(clippy::too_many_arguments)]` where needed).
- **Diff budget Margin (`Cargo.toml`):** Adding `crates/codegen/xai-grok-mods` to both `[workspace].members` and `[workspace.dependencies]` in root `Cargo.toml` is +2 lines (rather than +1 in the Spec §3.2 table), which still keeps the total upstream diff around ~81 lines (well within the ≤ 85 line budget). Alternatively, `xai-grok-shell/Cargo.toml` can use `xai-grok-mods = { path = "../xai-grok-mods" }` (matching `xai-grok-paths`, `xai-fast-worktree`, etc. in `xai-grok-shell/Cargo.toml:132-148`) so root `Cargo.toml` only needs +1 line in `[workspace].members`.
