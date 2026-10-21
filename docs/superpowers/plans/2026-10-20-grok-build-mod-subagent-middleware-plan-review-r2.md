# Plan Review — Grok-Build Mod Subagent Middleware (`xai-grok-mods` + `plugins/cursor`) (Round 2)

- **Plan reviewed:** `docs/superpowers/plans/2026-10-20-grok-build-mod-subagent-middleware.md`
- **Reference spec:** `docs/superpowers/specs/2026-10-20-grok-build-mod-subagent-middleware-design.md` (Round 3 Approved)
- **Prior review:** `docs/superpowers/plans/2026-10-20-grok-build-mod-subagent-middleware-plan-review-r1.md`
- **Ground-truth codebase:** `refs/netbrah-grok-build` (`feat/hts-native-shipping` @ `2fcd97294a23cfb19b1d6e4c1a788840a17dc569`)

## Plan Review

**Status:** Approved

### Round 1 Issue Resolution Verification

1. **[Task 2, Step 2] (`control.rs` `sample_mailbox_msg` types):** Resolved. `sample_mailbox_msg` now uses `AgentMailboxMessageKind::NativeMessage` and initializes all four `NativeAgentMessage` fields (`author`, `recipient`, `encrypted`, `trigger_prompt_id`).
2. **[Tasks 1–4] (`crates/codegen/xai-grok-mods/src/lib.rs` completeness and ordering):** Resolved. Task 1 Step 3 provides the initial `src/lib.rs` with `BorrowedSendFuture`, `ModStarted`, `ModRunInput`, and `ModRunOutcome`; Task 2 Step 3 adds `ModHost`, `control`, and `store`; Task 3 Step 2 adds `bridge`; Task 4 Step 2 adds `process`, `session`, and `run_mod_child`. Every intermediate `cargo test -p xai-grok-mods` step compiles cleanly in sequence.
3. **[Task 1, Step 3] (`manifest.rs` arg validation and T1 assertions):** Resolved. `ModConfig::load` checks `joined.symlink_metadata().is_ok()` before canonicalizing and checking containment against `root`, and `t1_manifest_validates_namespace_and_containment` tests unknown-field rejection (`deny_unknown_fields`) and Unix symlink-escape rejection.
4. **[Task 4, Step 2 & Task 5, Step 2] (`session.rs` and `mod_adapter.rs` warnings and shell integration):** Resolved.
   - `session.rs` emits warnings when `resume_key` misses in `ModSessionStore` (`"vendor session for {key} not found; started fresh"`) or when `session_resume` is disabled.
   - `mod_adapter.rs` emits warnings for `request.fork_context`, non-empty `definition.mcp_servers`, `outcome.warnings`, and `outcome.admission_uncertain`.
   - `meta_dir_for` honors `#[cfg(test)] ctx.run_shell_child_harness.as_ref().map(|h| h.meta_dir.clone())`.
   - `ShellCompletionData` (`#[derive(Clone, Default)]` at `mod.rs:544`) is cloned into `ShellModHost`, and `context_normalized: false` (`bool`) is used on both `SubagentMeta` and `SessionUpdate::SubagentSpawned`.
5. **[Tasks 4, 5, 6] (Concrete code for `mock_sidecar.rs`, `mod_adapter.rs`, `mod_adapter_tests.rs`, and `plugins/cursor/`):** Resolved. All implementation and test files across Tasks 4, 5, and 6 are now provided in full, including grandchild process-group reaping in `mock-sidecar.mjs` / `t6`, coordinator live steer in `t2`, `t7` drain loop, shell integration tests `t8`–`t11`, and the `plugins/cursor` sidecar + unit smoke test (`runner.test.mjs`).

**Issues (if any):** None (0 blocking/major issues).

**Recommendations (advisory, do not block approval):**

- **`mod_adapter_tests.rs` minor test-helper cleanups when implementing Task 5 Step 4:**
  1. Line 2993: Import `TrustStore` from `xai_grok_agent::plugins::TrustStore` (or `xai_grok_agent::plugins::trust::TrustStore`) rather than `xai_grok_agent::plugins::discovery::TrustStore` (`discovery.rs:21` imports `TrustStore` privately, whereas `plugins/mod.rs:26` re-exports it publicly).
  2. Line 3035: Call `discovered.retain(|p| p.manifest.name == "cursor");` before `assert_eq!(discovered.len(), 1, ...)` so `discover_plugins` remains hermetic on developer machines that have user plugins installed under `~/.claude/plugins` or `$GROK_HOME/plugins`.
  3. Line 3086: As noted in the plan's own implementer note on line 3368, `which` is not a dependency of `xai-grok-shell`, so use `std::process::Command::new("node").arg("--version").output().is_err()` directly inside `require_node!`.
