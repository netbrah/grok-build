# ApexAI Fleet Harness OPORD

> **For agentic workers:** REQUIRED SUB-SKILL: use `executing-plans` (or `subagent-driven-development` when Delta explicitly authorizes delegation) and execute the checkboxes in order. Read the APEX fleet, harness, auth, settings, SCS, and headless-smoke skills before touching NFS.

**Goal:** Build this Grok-derived Rust harness for Linux, deploy it as the independent `apexai` fleet harness under `/x/eng/apex/apexai`, and prove a headless Grok 4.6 turn through the NetApp LiteLLM/Vertex route.

**Architecture:** A thin NFS launcher imports user-owned tokens from `~/.bashrc`, reuses only the production APEX OPSEC decoder, exports the ApexAI identity and inference settings, and execs an immutable Linux binary. A launcher-selected full-power managed TOML defines the model, shared skills, fleet rules, and all 19 MCP servers without embedding secrets. The source changes add the two missing runtime seams: `GROK_MANAGED_CONFIG_PATH` and actual discovery of `[paths].extra_rule_dirs`.

**Tech stack:** Rust/Cargo, Bash, TOML, shared NFS, SCS Linux build host.

**Spec:** This OPORD is the approved design and executable handoff. Background analysis is in `docs/fleet-admin-config-and-skill-vendoring.md`; the complete recovered transcript is `/var/folders/42/_lc4qb6d2wnd1x82w43h05nm0000gp/T/grok-transcript-97732961-fc43-48ce-bac3-3196a4bffeef.md` (the original `...05157b32...md` path no longer exists).

## Commander's intent

Deliver a separate fleet harness without changing the live `apex` launcher, binary, settings, or OPSEC source. Users install one symlink and keep credentials in `~/.bashrc`. Fleet-owned paths, service URLs, model selection, skills, and rules remain centrally controlled from NFS.

## End state

```text
/x/eng/apex/apexai/
  bin/apexai
  apexai.sh
  managed_config.toml
  rules/AGENTS.md
  install.sh
  archive/
```

Per-user state:

```text
~/.apexai/
  bin/apexai -> /x/eng/apex/apexai/apexai.sh
  config.toml     # optional user preferences only
```

No `fleet.env` exists. No secret value is stored in TOML, source control, the NFS launcher, or logs.

## Fixed decisions

- `GROK_HOME="$HOME/.apexai"` isolates sessions and user preferences from upstream Grok. Project `.grok` discovery remains repository/cwd based and is unchanged.
- `GROK_MANAGED_CONFIG_PATH=/x/eng/apex/apexai/managed_config.toml` selects the fleet layer.
- `APEX_LLM_PROXY_KEY` is the managed model's `env_key`; the launcher also exports `XAI_API_KEY="$APEX_LLM_PROXY_KEY"` for native fallback paths.
- `GROK_XAI_API_BASE_URL=https://llm-proxy-api.ai.eng.netapp.com/v1` selects the LiteLLM/Vertex route.
- `GROK_CLIENT_NAME=Apex` and `GROK_CLIENT_VERSION=apexai` produce `User-Agent: Apex/apexai grok-shell/<binary-version> (linux; x86_64)` without a UA code fork. The Apex prefix is required by the corporate LLM-proxy key gate.
- MCP `initialize.clientInfo.name` is exactly `apex-mcp-client`; the MCP gate requires that literal identity.
- `[skills].paths = ["/x/eng/apex/apexai/bundled"]`, where `bundled` is a symlink to `/x/eng/ai_engineering/APEX/apex-ontap/canonical/bundled-skills`.
- `[paths].extra_rule_dirs = ["/x/eng/apex/apexai/rules"]` supplies always-on fleet instructions before repo-local instructions.
- The welcome screen uses the XLI feather Braille mark in the existing 14×7 and 10×5 envelopes; the existing shimmer renderer remains unchanged.
- `[hints] new_session_worktree_mode = "never"` and `fork_worktree_mode = "never"` suppress worktree prompts on monolithic NFS checkouts. Removing all worktree UI/code is a follow-on, not a release dependency.
- The existing uncommitted sampler changes in `xai-grok-sampler/src/client.rs` and `tests/test_actor.rs` are part of the build source of truth and must be preserved.
- MCP startup failures caused by the known MCP gate are recorded but do not block the first inference smoke.

## Runtime data flow

```text
~/.bashrc credentials
        +
APEX encrypted OPSEC blob --decoder only--> MCP paths and internal URLs
        |
        v
apexai.sh
  exports GROK_HOME, managed config path, client identity, proxy key/base URL
        |
        v
bin/apexai
  loads managed_config.toml
    |- model grok-4.6 via LiteLLM Responses
    |- /x/eng/apex/apexai/bundled skills symlink
    |- rules/AGENTS.md before project instructions
    `- 19 MCP definitions expanded from process env
```

## Launcher contract

The launcher must:

1. Resolve its own NFS directory through symlinks.
2. Import only known credential exports from `~/.bashrc`; never source arbitrary interactive shell content.
3. Set `APEX_FAILOVER_COPILOT=false` and `APEX_OPSEC_TEST_MODE=1`, source `/u/palanisd/tools/apex/apex`, then unset test mode.
4. Call `_opsec_mcp_env_decode /u/palanisd/tools/apex/opsec/bin/.apex-mcp-env.dat`.
5. Accept decoded text only when it contains an `export` line, then `eval` it without logging it.
6. Never invoke `_apex_loc_check`, `_apex_host_check`, the APEX token banner, or the APEX launch loop.
7. Derive `MASTRA_SSO`, `MASTRA_CREDENTIAL`, and a missing `CONFLUENCE_USERNAME`; pass through user service tokens.
8. Optionally run `/u/palanisd/tools/apex/apex-cse/refresh-aiq-token.sh` and export a non-empty `AIQ_ACCESS_TOKEN`.
9. Export GHE defaults and CA/proxy variables needed by MCP children.
10. Require a non-empty `APEX_LLM_PROXY_KEY`, set `XAI_API_KEY` from it, and never print either value.
11. Export the ApexAI variables listed under Fixed decisions and `exec` the binary with the original argv.

## Managed TOML contract

Model:

```toml
[model."grok-4.6"]
base_url = "https://llm-proxy-api.ai.eng.netapp.com/v1"
api_backend = "responses"
env_key = "APEX_LLM_PROXY_KEY"
context_window = 500000
supports_backend_search = false

[models]
default = "grok-4.6"
default_reasoning_effort = "xhigh"
```

Fleet sources:

```toml
[skills]
paths = ["/x/eng/apex/apexai/bundled"]

[paths]
extra_rule_dirs = ["/x/eng/apex/apexai/rules"]

[hints]
new_session_worktree_mode = "never"
fork_worktree_mode = "never"
```

MCP conversion from live `/u/palanisd/tools/apex/system-settings.json`:

- Preserve the 19 names: `activeiq`, `asup`, `brewbot`, `cit`, `clangd-rs`, `confluence`, `coretool`, `coverage-rs`, `ghe`, `jira-ngage`, `lore`, `mastra-search`, `ontap-cluster`, `ontap-dev`, `ontap-sdk`, `presubmit-validate`, `pyrefly`, `reviewboard`, and `smartsolve`.
- Map `command`, `args`, `env`, `url`, and `headers` directly.
- Canonicalize `$VAR` strings to `${VAR}`; preserve literal non-secret paths and URLs.
- Convert each millisecond `timeout` to seconds and set both `startup_timeout_sec` and `tool_timeout_sec`.
- Convert `excludeTools` to `[disabled_mcp_tools] <server> = [ ... ]`.
- Omit APEX-only `trust` and descriptive metadata unsupported by Grok's MCP schema.
- Validate that no credential value is present; only `${VAR}` references are allowed.

## Harness changes

### Managed path seam

- Add `GROK_MANAGED_CONFIG_PATH` in `xai-grok-config`.
- When non-empty, it replaces `/etc/grok/managed_config.toml` at the existing `system_managed` merge position.
- Preserve `$GROK_HOME/managed_config.toml`, user config, overlays, and requirements precedence.
- Include the selected path in `managed_config_layers()`, config-source inspection, and hook discovery.
- An env-selected file is full-power configuration but is not assumed root-owned. Its hooks and permission-policy metadata must remain non-system provenance so it cannot grant itself the non-disableable/root-policy exemption.

### Fleet rule discovery seam

- Read `[paths].extra_rule_dirs` from the effective configuration.
- Treat each configured path as a directory containing `*.md` rules.
- Expand `~`, ignore absent/unreadable directories, sort files deterministically, and canonical-deduplicate aliases.
- Load configured fleet rules after home/vendor rules and before repo-root-to-cwd rules, so repo-local instructions remain later/higher precedence.
- Route all existing `read_agents_config_with_paths` consumers through the behavior so primary agents, subagents, telemetry, workspace discovery, and `grok inspect` agree.

## Execution plan

### Task 1: Preserve state and codify the operation

**Files:**

- Create: `docs/apexai-fleet-opord.md`
- Preserve: `crates/codegen/xai-grok-sampler/src/client.rs`
- Preserve: `crates/codegen/xai-grok-sampler/tests/test_actor.rs`

- [ ] Record `git status --short` and the sampler diff before editing.
- [ ] Refresh the encrypted APEX operations snapshot before any NFS mutation.
- [ ] Confirm `/x/eng/apex/apexai` does not exist or archive its current contents before replacement.

### Task 2: Add the managed-config path seam with TDD

**Files:**

- Modify: `crates/codegen/xai-grok-config/src/loader.rs`
- Modify: `crates/codegen/xai-grok-config/src/lib.rs`
- Modify: `crates/codegen/xai-grok-shell/src/inspect/mod.rs`
- Test: unit tests beside the loader and inspect code

- [ ] Write tests proving env selection, `/etc` fallback, merge position, config-source reporting, managed-layer enumeration, and non-system hook provenance.
- [ ] Run the focused tests and observe the expected failures.
- [ ] Implement the smallest loader/source-reporting change.
- [ ] Run focused tests green.

### Task 3: Make extra rule directories real with TDD

**Files:**

- Modify: `crates/codegen/xai-grok-agent/src/prompt/agents_md.rs`
- Modify: `crates/codegen/xai-grok-shell/src/inspect/mod.rs` only if its duplicate parsing can be removed
- Test: `agents_md.rs` unit tests

- [ ] Write a test with home, fleet, and project rules and assert exact order `home -> fleet -> project`.
- [ ] Write a test proving an absent configured directory is harmless and canonical duplicates appear once.
- [ ] Run focused tests and observe the expected failures.
- [ ] Implement configured path loading and deterministic discovery.
- [ ] Run focused tests green, including a subagent-facing call path.

### Task 4: Build reproducible fleet artifacts locally

**Files:**

- Create: `deploy/apexai/managed_config.toml`
- Create: `deploy/apexai/apexai.sh`
- Create: `deploy/apexai/install.sh`
- Create: `deploy/apexai/rules/AGENTS.md`
- Create: `deploy/apexai/tests/launcher_test.sh`

- [ ] Write the launcher behavior test first with a fake APEX decoder and fake ApexAI binary.
- [ ] Run it and observe failure before creating the launcher.
- [ ] Implement the launcher and installer.
- [ ] Translate all 19 MCPs into the managed TOML.
- [ ] Tailor APEX fleet doctrine to ApexAI identity and paths; do not copy stale APEX identity claims.
- [ ] Pin MCP `initialize.clientInfo.name` to the gate-required `apex-mcp-client`.
- [ ] Replace the welcome assets with the XLI feather at exactly 14×7 and 10×5 cells while retaining the existing shimmer.
- [ ] Validate shell syntax, launcher behavior, TOML parsing, MCP count/names, timeouts, deny lists, and absence of literal secrets.

### Task 5: Verify source and build Linux on SCS

**Build package:** `xai-grok-pager-bin`

**Artifact:** `target/release/xai-grok-pager`, deployed as `bin/apexai`

- [ ] Run focused config, agent, shell-inspect, and existing sampler tests locally.
- [ ] Run formatting/checks without rewriting unrelated files.
- [ ] Stage the exact dirty working tree in a unique `/tmp/apexai-build-*` directory on SCS; do not persist editable source on NFS.
- [ ] Build with `cargo build -p xai-grok-pager-bin --release` on Linux.
- [ ] Verify the result is an x86-64 ELF executable and record SHA-256.

### Task 6: Stage and atomically promote the new NFS tree

- [ ] Create a timestamped, mode-700 backup under `/x/eng/apex/apexai/archive/` if any prior deployment exists.
- [ ] Upload into a sibling temporary staging directory, not the final live filenames.
- [ ] Verify hashes, executable modes, `bash -n`, TOML parse, and exactly 19 MCP entries on SCS.
- [ ] Create `/x/eng/apex/apexai/bundled` as a symlink to `/x/eng/ai_engineering/APEX/apex-ontap/canonical/bundled-skills`.
- [ ] Promote the tested staging tree atomically to `/x/eng/apex/apexai`.
- [ ] Keep `/u/palanisd/tools/apex` read-only; consume its launcher decoder, OPSEC blob, binaries, and token refresh helper only.

### Task 7: Headless acceptance test

- [ ] Run `/x/eng/apex/apexai/apexai.sh --version` from a fresh login shell.
- [ ] Run `grok inspect` through the launcher and verify the env-selected managed config, shared skill path, fleet rule file, and 19 MCP definitions.
- [ ] Verify HTTP client identity identifies `Apex/apexai` and MCP initialization identifies `apex-mcp-client`.
- [ ] Run:

  ```bash
  /x/eng/apex/apexai/apexai.sh \
    -p "Respond with the literal word ACK" \
    -m grok-4.6 --yolo --output-format json
  ```

- [ ] Pass criteria: exit 0, valid JSON, response contains `ACK`, and the request reached the LiteLLM Responses endpoint.
- [ ] Record MCP-gate failures separately; they do not block the initial inference acceptance test.
- [ ] Run the installer into a disposable HOME, verify the symlink and PATH block, then remove only that disposable HOME.

## Rollback

The initial deployment creates a new path, so rollback is removal of the installer symlink and atomic renaming of the new tree out of service. For later releases:

1. Preserve the currently deployed tree or binary in `archive/<timestamp>/` with mode 700.
2. Promote only the exact hash that passed staging smoke.
3. On regression, atomically restore the archived launcher/config/binary set together; never mix versions.
4. Re-run `--version`, inspect, and the ACK prompt after restore.

## Release gate

Do not call the operation complete until fresh command output proves:

- targeted and sampler tests pass;
- release build exits 0;
- Linux artifact type and hashes match across staging and NFS;
- launcher and installer pass `bash -n` and behavior tests;
- managed TOML parses with 19 expected MCPs and no literal secrets;
- managed config, rules, and skills are visible through the deployed binary;
- headless Grok 4.6 returns `ACK` over LiteLLM;
- user-owned pre-existing repository changes remain intact.

## Deployment record — 2026-09-03

Initial deployment is live at `/x/eng/apex/apexai`.

- Build host/toolchain: SCS, Rust 1.94.1, six Cargo workers, offline vendored dependencies.
- Build-time artifact: `/tmp/apexai-build-pXJvkFUg/target/release/xai-grok-pager` (temporary build tree removed after acceptance).
- Deployed artifact: `/x/eng/apex/apexai/bin/apexai`.
- SHA-256: `8cd992656a655782c5647406451e96592ef3db4aad2e0b5b6e0c4f422d58b2fe` at build, staging, and live paths.
- Artifact: 64-bit x86-64 Linux ELF, approximately 207 MB, maximum required glibc symbol version `GLIBC_2.34`.
- Version output: `grok 1.0.16 (unknown)`.
- Permissions: deployment root `0755`; `archive/` remains `0700`.
- Launcher and managed-config tests pass on Linux. The managed TOML has the expected 19 MCP definitions with source parity, converted timeouts, env/header references, and deny lists.
- Live `inspect --json` reports `/x/eng/apex/apexai/managed_config.toml` as the `managed-path` layer, discovers `/x/eng/apex/apexai/rules/AGENTS.md`, and discovers 41 skills through the shared `bundled` symlink.
- The maintainer account's live inspect shows 22 total MCPs: the 19 fleet-managed definitions plus `c2-orient`, `cognee`, and `wikid` imported from its pre-existing `~/.claude.json` compatibility layer. This is expected per-user compatibility discovery, not fleet-config drift.
- Live headless acceptance through `/x/eng/apex/apexai/apexai.sh` exits 0, returns valid JSON containing literal `ACK`, and writes no stderr.
- The focused MCP client-info unit test passes with the strict identity `apex-mcp-client`. Successful live inference through the proxy also confirms the `Apex/apexai` request identity is accepted.
- A 120×40 PTY launch renders the 14×7 Braille feather and its existing shimmer animation; the compact 80×24 layout intentionally omits the large welcome mark.
- The installer passes twice against a disposable HOME, creates `~/.apexai/bin/apexai` with the correct NFS target, and adds its PATH block idempotently.
- Removed the exact SCS build directory, vendor archive, staging smoke tree, and acceptance-output files after promotion, reclaiming roughly 6.2 GB; `/tmp` was 12% used afterward.

Build reproduction requires both ripgrep bundle variables:

```bash
export GROK_TOOLS_BUNDLE_RG_PATH=/u/palanisd/.spectre/bin/rg
export GROK_SHELL_BUNDLE_RG_PATH=/u/palanisd/.spectre/bin/rg
```

GitHub was unavailable during this build. Cargo dependencies were transferred as a clean vendor archive and the release was built offline. When recreating that archive on macOS, use `COPYFILE_DISABLE=1 tar --no-xattrs` to avoid AppleDouble files.
