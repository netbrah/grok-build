# Merge plan: origin/main into mod-subagent middleware

Branch: `merge/main-into-mod-subagent-middleware`
Worktree: `/workspace/repos/Grok-build-merge-main`
Base: 37949780 (2026-09-09). Branch tip 936e7f20. Main tip 2bdd1d6a (2026-09-29).
Main-only commits: 7 (all `Synced from monorepo`). Branch-only commits: 277.
Git content conflicts: 111. Weave clean: 8. Weave still conflicted: 100. Main-only paths git flagged: 2. Lockfile skipped: 1.

Uncommitted middleware WIP stays on `feat/mod-subagent-middleware` and is NOT in this branch yet.

## Waves

### 0. Do not merge on the dirty feature branch
Merge `origin/main` here first. Then replay the WIP (xai-grok-mods, mod_adapter, plugins/cursor, wake test).

### 1. Accept weave-clean results
- crates/codegen/xai-grok-config-types/src/memory.rs
- crates/codegen/xai-grok-config-types/src/registry.rs
- crates/codegen/xai-grok-pager-pty-harness/tests/pty_e2e_config_ui.rs
- crates/codegen/xai-grok-sampler/src/lib.rs
- crates/codegen/xai-grok-sampling-types/src/types.rs
- crates/codegen/xai-grok-shell/src/agent/init.rs
- crates/codegen/xai-grok-shell/src/session/acp_session_impl/child_tool_projection_tests.rs
- crates/codegen/xai-grok-tools/src/implementations/grok_build/mod.rs

### 2. Take main for files that exist only there
- crates/codegen/xai-grok-cloud-config/src/remote_settings/cache.rs
- crates/codegen/xai-grok-permission-rules/src/policy.rs

### 3. Regenerate Cargo.lock. Do not hand-merge it.

### A. Hand-reconcile mod/task contract (keep both sides' APIs)
- crates/codegen/xai-grok-config-types/Cargo.toml
- crates/codegen/xai-grok-shell/Cargo.toml
- crates/codegen/xai-grok-shell/src/agent/subagent/handle_request.rs
- crates/codegen/xai-grok-shell/src/agent/subagent/tests/mod.rs
- crates/codegen/xai-grok-shell/src/agent/subagent/tests/rest.rs
- crates/codegen/xai-grok-subagent-resolution/src/context.rs
- crates/codegen/xai-grok-tools/src/implementations/grok_build/task/backend.rs
- crates/codegen/xai-grok-tools/src/implementations/grok_build/task/coordinator.rs
- crates/codegen/xai-grok-tools/src/implementations/grok_build/task/coordinator_tests.rs
- crates/codegen/xai-grok-tools/src/implementations/grok_build/task/mod.rs
- crates/codegen/xai-grok-tools/src/implementations/grok_build/task/types.rs
- crates/common/xai-tool-types/src/task.rs

handle_request.rs `run_shell_child` is one entity collision with four hunks:
- Keep main's explicit-model admission error return.
- Keep the branch's fail-closed worktree validation (do not restore main's shared-workspace fallback).
- Keep main's grove_worktree / grove_gate_source fields in the logs.
- After that merge, re-insert mod detect() dispatch before native model resolution.

### B. Session, sampler, chat-state, compaction (not the mod feature; integrate main's sync)
- crates/codegen/xai-chat-state/src/actor/state.rs
- crates/codegen/xai-chat-state/src/actor/tests.rs
- crates/codegen/xai-chat-state/src/commands.rs
- crates/codegen/xai-chat-state/src/compaction_utils.rs
- crates/codegen/xai-chat-state/src/types.rs
- crates/codegen/xai-grok-sampling-types/src/conversation.rs
- crates/codegen/xai-grok-sampling-types/src/conversation/messages_tests.rs
- crates/codegen/xai-grok-sampling-types/src/conversation/responses.rs
- crates/codegen/xai-grok-sampling-types/src/conversation/responses_tests.rs
- crates/codegen/xai-grok-sampling-types/src/lib.rs
- crates/codegen/xai-grok-sampling-types/src/messages.rs
- crates/codegen/xai-grok-shell/src/session/acp_session_impl/child_tool_projection.rs
- crates/codegen/xai-grok-shell/src/session/acp_session_impl/model_switch.rs
- crates/codegen/xai-grok-shell/src/session/acp_session_impl/recap.rs
- crates/codegen/xai-grok-shell/src/session/acp_session_impl/sampler_turn.rs
- crates/codegen/xai-grok-shell/src/session/acp_session_impl/side_call.rs
- crates/codegen/xai-grok-shell/src/session/acp_session_impl/spawn.rs
- crates/codegen/xai-grok-shell/src/session/acp_session_impl/tool_calls.rs
- crates/codegen/xai-grok-shell/src/session/acp_session_impl/tool_dispatch.rs
- crates/codegen/xai-grok-shell/src/session/acp_session_impl/turn.rs
- crates/codegen/xai-grok-shell/src/session/acp_session_tests/auth_error_no_retry_tests.rs
- crates/codegen/xai-grok-shell/src/session/acp_session_tests/cancel_running_task_tests.rs
- crates/codegen/xai-grok-shell/src/session/acp_session_tests/idle_resume_tests.rs
- crates/codegen/xai-grok-shell/src/session/acp_session_tests/inline_auto_compact_flow_tests.rs
- crates/codegen/xai-grok-shell/src/session/acp_session_tests/memory_config_tests.rs
- crates/codegen/xai-grok-shell/src/session/acp_session_tests/recap_display_only_tests.rs
- crates/codegen/xai-grok-shell/src/session/acp_session_tests/replay_buffer_send_update_tests.rs
- crates/codegen/xai-grok-shell/src/session/acp_session_tests/support.rs
- crates/codegen/xai-grok-shell/src/session/acp_session_tests/transient_retry_tests.rs
- crates/codegen/xai-grok-shell/src/session/agent_rebuild.rs
- crates/codegen/xai-grok-shell/src/session/compaction.rs
- crates/codegen/xai-grok-shell/src/session/compaction_inline_auto_compact_flow_tests.rs
- crates/codegen/xai-grok-shell/src/session/helpers/memory_flush_window.rs
- crates/codegen/xai-grok-shell/src/session/helpers/replay.rs
- crates/codegen/xai-grok-shell/src/session/helpers/session_compact_reasoning_compaction_regression_tests.rs
- crates/codegen/xai-grok-shell/src/session/helpers/session_summary.rs
- crates/codegen/xai-grok-shell/src/session/helpers/turn_summary.rs
- crates/codegen/xai-grok-shell/src/session/storage/jsonl/mod.rs
- crates/codegen/xai-grok-shell/src/session/storage/jsonl/tests.rs
- crates/common/xai-grok-compaction/src/code_compaction/sample.rs

### C. Config, auth, mcp, pager, announcements
- crates/codegen/xai-grok-agent/src/prompt/agents_md.rs
- crates/codegen/xai-grok-announcements/src/lib.rs
- crates/codegen/xai-grok-config-types/src/lib.rs
- crates/codegen/xai-grok-config-types/src/mcp.rs
- crates/codegen/xai-grok-config-types/src/registry_tests.rs
- crates/codegen/xai-grok-config/src/loader.rs
- crates/codegen/xai-grok-hooks/src/dispatcher.rs
- crates/codegen/xai-grok-login/src/auth_method.rs
- crates/codegen/xai-grok-mcp/src/servers.rs
- crates/codegen/xai-grok-models/src/lib.rs
- crates/codegen/xai-grok-pager/src/acp/tracker.rs
- crates/codegen/xai-grok-pager/src/slash/commands/effort.rs
- crates/codegen/xai-grok-pager/src/slash/commands/model.rs
- crates/codegen/xai-grok-sampler/src/actor/state.rs
- crates/codegen/xai-grok-sampler/src/client.rs
- crates/codegen/xai-grok-sampler/src/doom_loop_recovery_tests.rs
- crates/codegen/xai-grok-sampler/src/stream/messages.rs
- crates/codegen/xai-grok-sampler/src/stream/messages_tests.rs
- crates/codegen/xai-grok-sampler/tests/test_actor.rs
- crates/codegen/xai-grok-shell/benches/session_list.rs
- crates/codegen/xai-grok-shell/src/agent/config.rs
- crates/codegen/xai-grok-shell/src/agent/config_model_override_parse.rs
- crates/codegen/xai-grok-shell/src/agent/config_tests.rs
- crates/codegen/xai-grok-shell/src/agent/model_providers.rs
- crates/codegen/xai-grok-shell/src/agent/mvp_agent/agent_ops.rs
- crates/codegen/xai-grok-shell/src/agent/mvp_agent/tests.rs
- crates/codegen/xai-grok-shell/src/agent/remote_config.rs
- crates/codegen/xai-grok-shell/src/agent/remote_config/fetch.rs
- crates/codegen/xai-grok-shell/src/agent/remote_config/manager/mod.rs
- crates/codegen/xai-grok-shell/src/agent/remote_config/manager/tests.rs
- crates/codegen/xai-grok-shell/src/agent/remote_config/prefetch.rs
- crates/codegen/xai-grok-shell/src/agent/server.rs
- crates/codegen/xai-grok-shell/src/agent/session_config.rs
- crates/codegen/xai-grok-shell/src/claude_import.rs
- crates/codegen/xai-grok-shell/src/config/mod.rs
- crates/codegen/xai-grok-shell/src/inspect/mod.rs
- crates/codegen/xai-grok-shell/src/remote/client.rs
- crates/codegen/xai-grok-shell/src/remote/client_tests.rs
- crates/codegen/xai-grok-shell/src/remote/model_source/oai.rs
- crates/codegen/xai-grok-shell/src/sampling/conversation.rs
- crates/codegen/xai-grok-shell/src/sampling/error.rs
- crates/codegen/xai-grok-shell/src/test_support/lsp_runtime.rs
- crates/codegen/xai-grok-shell/src/util/config/resolve/features.rs
- crates/codegen/xai-grok-shell/tests/common/mod.rs
- crates/codegen/xai-grok-tools/src/registry/types.rs
- crates/codegen/xai-grok-tools/src/types/compat.rs
- crates/codegen/xai-grok-workspace/src/permission/types.rs
- crates/common/xai-tool-types/src/lib.rs

## Second pass after WIP replay
WIP also edits child_runtime.rs, child_runtime_tests.rs, subagent/mod.rs, tests/wake.rs, shell Cargo.toml, root Cargo.toml, Cargo.lock.
Git auto-merged the committed versions of child_runtime.rs, mod.rs, and wake.rs. Re-apply the WIP patches and expect a second conflict on handle_request.rs, both Cargo.toml files, and Cargo.lock.

## Tools
- weave 0.6.1 is on PATH via /workspace/.tools/ataraxy/bin (glibc 2.36 OK).
- sem 0.28.1 is installed but will not start: it needs glibc 2.39; this machine has 2.36.
- npm postinstall of @ataraxy-labs/sem failed on tar chown; binaries were unpacked from GitHub releases with --no-same-owner.

## Do not
- cargo test -p xai-grok-shell in this 4GB sandbox (rustc is SIGKILL).
- Commit the middleware WIP until the merge resolution is reviewed.
