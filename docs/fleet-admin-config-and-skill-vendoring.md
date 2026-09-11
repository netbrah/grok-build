# Fleet admin config + in-binary skill vendoring

Feasibility notes from 2026-09-03. No implementation. Captures how the harness already loads config and skills, what a launcher-driven fleet overlay should use, how to ship skills inside the binary, and the AGENTS.md seam (there is no `[skills].paths` equivalent).

## Goals

1. An **admin-overridable `config.toml`** pointed at by an environment variable, set from a fleet launcher that also loads other env vars.
2. A **skill vendoring mechanism in the harness**: skills compiled into the binary, visible to the model like any discovered skill. Landing on disk at launch vs staying in-memory is acceptable; the important choice is *where* they land.
3. A **fleet AGENTS.md** (always-on project instructions), analogous to `[skills].paths`. This is the last seam.
4. A **fleet launcher** that exports MCP env, the LiteLLM/Vertex API key + base URL, and (if needed) a distinct User-Agent. Inference already has harness work to talk to the deployed LiteLLM Vertex Grok instead of `api.x.ai`.

---

## 1. Config: env-pointed admin TOML

### What already exists

`ConfigLayers::load()` (`crates/codegen/xai-grok-config/src/config_layers.rs`) merges lowest → highest:

| Priority (low → high) | Source | Notes |
|---|---|---|
| Built-in defaults | compiled | |
| System managed | `/etc/grok/managed_config.toml` | Unix only (`system_config_dir()` is hardcoded) |
| User managed | `$GROK_HOME/managed_config.toml` | Console-synced; user-writable |
| User config | `$GROK_HOME/config.toml` | `/settings` writes here |
| Project config | `.grok/config.toml` | Only `[mcp_servers]`, `[plugins]`, `[permission]`, `[mcp] max_output_bytes` |
| Env overlay | `GROK_CONFIG` (inline JSON) or `GROK_CONFIG_PATH` (JSON/TOML file) | **Allowlisted soft keys only** |
| Requirements | `$GROK_HOME/requirements.toml` → `/etc/grok/requirements.toml` → macOS MDM `ai.x.grok` | Admin pin; `pin` keys cannot be overridden |
| Direct env vars | `GROK_*` | e.g. `XAI_API_KEY` |
| CLI flags | `--model`, `--sandbox`, `--yolo`, … | Highest |

`GROK_CONFIG` wins over `GROK_CONFIG_PATH` when the inline blob fully succeeds; empty/malformed inline falls through to the path. Overlay reads are capped at 4 MiB.

### Do not reuse `GROK_CONFIG` / `GROK_CONFIG_PATH` for fleet admin

That overlay is fail-closed to `OVERLAY_ALLOW_PATHS` (`crates/codegen/xai-grok-config/src/config_override.rs`):

- whole tables: `models`, `features`
- leaves: `toolset.bash.login_shell_capture`, `toolset.web_search.{allowed,excluded}_domains`
- `shell_environment_policy` filters only (`inherit`, `exclude`, `include_only`, `ignore_default_excludes`) — **`set` is dropped**

Everything else is stripped, including `[skills].paths`, MCP, auth, sandbox, plugins, permission rules. Security gates also read overlay-free (`effective_config_base_without_overlay`). Pointing `GROK_CONFIG_PATH` at a full fleet TOML looks like it worked and silently drops the fleet tables.

### Recommended new env (small harness change)

Feed an **existing full-power layer**, not the overlay:

| Proposed env | Layer it feeds | User can override? |
|---|---|---|
| `GROK_MANAGED_CONFIG_PATH` | same merge slot as `/etc/grok/managed_config.toml` | yes (`~/.grok/config.toml`) |
| `GROK_REQUIREMENTS_PATH` | same merge slot as `/etc/grok/requirements.toml` | no, for keys marked `pin` |

Change is localized to `ConfigLayers::load` plus inspect/docs/tests. `system_config_dir()` today has no env escape hatch.

If the fleet can write `/etc/grok`, **no new env is required**. Drop `managed_config.toml` and/or `requirements.toml` there.

`skills.paths` is already valid in managed/requirements (`yes` in the config-reference table). A fleet TOML can already advertise extra skill dirs once it is on a real managed/requirements layer.

### Launcher pattern

Config load reads the **agent process environment at startup**. Subagents inherit it. Sandbox / `shell_environment_policy` only filter **tool** subprocesses; they do not re-run config merge.

`.envrc` is cwd-scoped, optional (`session.load_envrc`), and too late for the first merge. Do not source fleet admin config from project `.envrc`.

```sh
# fleet launcher
set -a
. /nfs/grok/fleet.env          # keys, proxy, GROK_HOME, …
set +a
export GROK_MANAGED_CONFIG_PATH=/nfs/grok/managed_config.toml   # proposed
exec grok "$@"
```

Keep secrets in env (`XAI_API_KEY`, proxy creds), not in the TOML. Direct `GROK_*` env already sits above config files.

---

## 2. Skills: in-binary vendoring for fleet

### How discovery works today

Priority (first-seen name wins; Local highest):

1. Local — `cwd/.grok/skills` (also `.agents`, `.claude`, `.cursor` when compat is on)
2. Intermediate dirs up to git root
3. Repo — `<repo>/.grok/skills`
4. User — `~/.grok/skills`
5. `[skills].paths` in config (stamped `config_source = ConfigToml`)
6. Server — launcher-injected `server_skill_dirs` (`~/.grok/server-skills`)
7. **Bundled** — `~/.grok/bundled/skills` + injected `bundled_skill_dirs` (lowest native)
8. Plugin — qualified `plugin:name`; does not steal the bare name from a native skill

Invocation is disk-shaped:

- Walk dirs for `SKILL.md`
- `SkillInfo.path` is a filesystem path
- `load_skill_content` does `tokio::fs::read_to_string` unless `body` is already set
- Companion `scripts/` / `references/` resolve as siblings of that path
- Watcher, slash menu, and `grok inspect` assume a real path

`SkillInfo.body` already exists (product skills, agent-definition preload). Markdown-only skills can skip a disk read. Skills with scripts cannot: the model will `Read` those paths.

### Existing “vendoring” (not in-tree)

Platform skills are **not** compiled into the binary.

- Older builds extracted them into `$GROK_HOME/skills/` on startup. That shadowed user skills and was removed.
- `purge_stale_extracted_skills` (`crates/codegen/xai-grok-shell/src/builtin.rs`) still deletes leftover dirs whose `SKILL.md` SHA-256 matches a known shipped body. User edits are kept.
- Replacement: fetch `GET /v1/subagents/bundle` and extract under `~/.grok/bundled/` via `xai-grok-bundle` (`extract_bundle_archive` / `write_bundle_to_cache`). Checksum `manifest.json`. User-edited files in the cache are not overwritten. ACP: `x.ai/bundle/sync`, `x.ai/bundle/status`.
- `extract_builtin_files` today only writes `README.md` + `.metadata_version`; comment explicitly says user skills are never managed there.
- grok.com `/rest/skills` advertises product skills (docx, pdf, …) as slash commands with no local `SKILL.md`.
- `~/.grok/vendor/` is **tool binaries** (`bfs`, `ugrep`), not skills.

### Preference: compile skills into the binary

Skills are injected at build time. At launch they may stay in the binary or be written to disk; either is fine for the model as long as discovery + `Read` of companions work.

**Do not extract into `~/.grok/skills/`.** That is User scope (beats Bundled/Server). It is the path the old extract used, which is why the purge exists. A same-named personal skill would be clobbered, or a fleet skill would permanently shadow a user skill of the same name.

**Do extract into `~/.grok/bundled/skills/`** (or a sibling such as `~/.grok/bundled/fleet-skills/`) tagged `SkillScope::Bundled`. That reuses:

- directory walk already in `list_skills_with_options`
- checksum / “don’t overwrite user edits” in `xai-grok-bundle`
- inspect label `bundled`
- local/repo/user same-name override

### Implementation sketch (not done)

1. At build time, pack a skills tree (each `name/SKILL.md` plus `scripts/`, `references/`) into the binary (`include_bytes!` of a tar.gz, or `rust-embed`). Same shape `extract_bundle_archive` already accepts (`skills/<name>/…`).
2. On startup (next to `extract_builtin_files` / `purge_stale_extracted_skills` in `init_process`), extract into `$GROK_HOME/bundled/skills/` using the existing manifest checksums so:
   - missing files are restored
   - byte-identical managed files are updated on binary bump
   - user-edited files are left alone
3. Discovery already walks `$GROK_HOME/bundled`. No prompt-path change if files land on disk.
4. Pure in-memory (no extract) is only worth it for markdown-only skills via `SkillInfo.body`. Companion files force a VFS or a Read intercept — not worth it.

Updating a skill means shipping a new binary (or a new bundle fetch, if you also keep the network cache).

### Alternatives (no / less harness work)

| Approach | Effort | When |
|---|---|---|
| `[skills].paths = ["/nfs/grok/skills"]` in managed/requirements | Zero (once the TOML is on a real managed layer) | Shared disk / NFS fleet |
| Plugin (`grok plugin install`) | Zero new harness | Team marketplace |
| `/etc/grok/managed_config.toml` + shared skills dir | Zero | Root-writable hosts |
| Embed + extract to `bundled/skills` | Small–medium | Air-gapped / binary is the distribution unit |
| In-RAM only, no disk | High | Don’t. Companion files + Read tool |

---

## 3. AGENTS.md: no `[skills].paths` equivalent

This is the last fleet seam. Skills have an extra-dir config key. Project instructions do not.

### What already exists

`read_agents_config_with_roots` (`crates/codegen/xai-grok-agent/src/prompt/agents_md.rs`) walks a **fixed** set of roots. There is no `[instructions].paths`, `[agents_md].paths`, or extra-file list in `config.toml` / managed / requirements. `agent.definition` is a different object (named agent profile with YAML frontmatter), not AGENTS.md.

Discovery order (home first, then project; deeper files later in the prompt, so they win on conflict):

| Order | Root | What is scanned |
|---|---|---|
| 1 | `$GROK_HOME` (default `~/.grok`) | Named files (`AGENTS.md`, `Agents.md`, `AGENT.md`, plus Claude names) and `$GROK_HOME/rules/*.md` |
| 2 | `~/.claude/`, `~/.cursor/` | Compat-gated named files and `rules/` |
| 3 | Git root → cwd (inclusive) | Named files plus `<dir>/.grok/rules/` (and `.claude` / `.cursor` rules when compat is on) |
| — | CWD only | Same, when not in a git repo |
| extra | `$XAI_ROOT` + `$XAI_USER` | Optional workspace-user dir inserted into the project chain (`workspace_user.rs`). Unset = no-op. Not a fleet-wide extra path |

Named filenames (compat-gated): `Agents.md`, `Claude.md`, `CLAUDE.md`, `CLAUDE.local.md`, `AGENT.md`, `AGENTS.md`. A directory can contribute more than one. Gitignore **does** apply here (unlike skill roots).

Content is injected as a `<system-reminder>` project-instructions block at session start (`format_agents_md_section`). Auto-load on later `Read`/`list` of out-of-tree dirs is a separate follow-up path.

Config-reference has **no** AGENTS.md path key. Closest keys: `compat.claude.agents` / `compat.cursor.agents` (scan toggles), `agent.definition` / `agent.name` (agent profiles).

### Why this is not like `[skills].paths`

`[skills].paths` is a first-class `SkillsConfig` field, valid in managed/requirements (`yes`), collected after auto-discovery. AGENTS.md has no such field; extra files cannot be named from TOML today.

### Fleet options without a new config key

| Approach | Effort | Behavior |
|---|---|---|
| Write `$GROK_HOME/AGENTS.md` and/or `$GROK_HOME/rules/*.md` at launch (from the binary or the launcher) | Zero harness | Home rules load on every project. Project `AGENTS.md` still wins later in the prompt. Same extract-vs-user-edit question as skills: don’t clobber a user’s `~/.grok/AGENTS.md` |
| Shared NFS file that the launcher copies/symlinks to `$GROK_HOME/AGENTS.md` | Zero harness | Same as above; NFS is the source of truth |
| `$XAI_ROOT` / `$XAI_USER` | Zero harness | Per-user workspace dir, not org-wide; wrong tool for fleet policy |
| Commit `AGENTS.md` in every repo | Zero harness | Not fleet-global; only that tree |

Unlike skills, there is **no** Bundled-scope AGENTS.md cache. Home-level `$GROK_HOME/AGENTS.md` is the only always-on, all-projects slot.

### If a real analog to `[skills].paths` is wanted

Small change, same shape as skills:

```toml
# proposed, in managed/requirements
[instructions]
paths = ["/nfs/grok/AGENTS.md", "/nfs/grok/rules"]
```

Wire extra files/dirs into `read_agents_config_with_roots` as additional home roots (or a dedicated “fleet” bucket that still loads **before** project files). Keep project AGENTS.md last so a repo can still override org policy. Do not allowlist this on `GROK_CONFIG` — discovery sources are explicitly excluded from the overlay.

Embed-and-extract of a fleet `AGENTS.md` into `$GROK_HOME/AGENTS.md` is the no-new-key path if the binary is the distribution unit. Prefer a sibling such as `$GROK_HOME/rules/fleet.md` if you need to leave a user’s `~/.grok/AGENTS.md` untouched (`rules/*.md` is already scanned under `$GROK_HOME`).

---

## Recommendation

For a launcher-driven fleet:

1. **Config:** launcher exports `GROK_MANAGED_CONFIG_PATH` (and optionally `GROK_REQUIREMENTS_PATH`) at a full TOML. Do not use `GROK_CONFIG_PATH`. Secrets stay in `fleet.env`.
2. **Skills, if a shared dir exists:** put `[skills].paths` in that managed TOML. No binary work.
3. **Skills, if the binary is the distribution unit:** embed the skill tree and extract at startup into `~/.grok/bundled/skills/` as Bundled scope. Treat `~/.grok/skills/` as user-owned.
4. **AGENTS.md, no new key:** extract or symlink fleet instructions to `$GROK_HOME/rules/fleet.md` (leaves `~/.grok/AGENTS.md` for the user). Project AGENTS.md still wins.
5. **AGENTS.md, skills-paths analog (optional, small):** add `[instructions].paths` on the managed/requirements layer and inject those files as extra home roots.
6. **Launcher env:** export MCP tokens, `XAI_API_KEY` (LiteLLM/Vertex key), `GROK_XAI_API_BASE_URL` (LiteLLM base). Do not put secrets in TOML. UA is a separate question (see §4).

---

## 4. Launcher: MCP env, API key / LiteLLM base URL, User-Agent

The launcher is the process that `export`s env and `exec`s grok. Config load, MCP `${VAR}` expansion, and the sampling client all read **that process env**. This matches the APEX pattern (`apex` launcher hydrates `OPENAI_API_KEY` / `OPENAI_BASE_URL` then execs).

### Split: launcher vs harness vs managed TOML

| Concern | Where it lives | Launcher enough? |
|---|---|---|
| MCP secrets (`JIRA_TOKEN`, `CONFLUENCE_TOKEN`, `GHE_TOKEN`, …) | Process env; `[mcp_servers.*.env]` / `headers` expand `${VAR}` at load | **Yes.** Export the vars. TOML should only reference `${JIRA_TOKEN}`, never the value |
| Fleet MCP **definitions** (command, url, which servers) | `[mcp_servers]` in managed/requirements (valid there, `yes`) | No — definitions go in the managed TOML the launcher points at. Env only fills secrets |
| Inference **key** | `XAI_API_KEY` (fallback `GROK_CODE_XAI_API_KEY`) | **Yes.** Export the LiteLLM/Vertex key as `XAI_API_KEY` |
| Inference **base URL** | `GROK_XAI_API_BASE_URL` (default `https://api.x.ai/v1`) | **Yes.** Point at LiteLLM (`…/v1`). Already the documented override |
| Extra inference headers | `[models].extra_headers` / `[model.<id>].extra_headers` | Managed TOML, not env. Overlay allowlists `models`, so `GROK_CONFIG` can set these too — still prefer managed |
| User-Agent | Compiled `AGENT_PRODUCT = "grok-shell"`; sampling client always sets `User-Agent` | **Mostly no.** See below |
| `x-grok-client-identifier` | `GROK_CLIENT_NAME` (default `grok-shell`) | Launcher can set this; it is **not** the HTTP User-Agent string |

`[mcp_servers]` is **not** on the `GROK_CONFIG` overlay allowlist (discovery / command-spawn). Fleet MCP tables belong on managed/requirements, same as `[skills].paths`.

### MCP env (launcher job)

`[mcp_servers.*]` string fields (`url`, `command`, `args`, `env` values, `headers` values) expand `$VAR` / `${VAR}` / `${VAR:-default}` at load (`expand_env_vars_in_toml`). Typical fleet TOML:

```toml
[mcp_servers.jira]
command = "npx"
args = ["-y", "jira-mcp"]
env = { JIRA_TOKEN = "${JIRA_TOKEN}" }

[mcp_servers.internal.headers]
Authorization = "Bearer ${INTERNAL_MCP_TOKEN}"
```

Launcher:

```sh
export JIRA_TOKEN=…
export CONFLUENCE_TOKEN=…
export GHE_TOKEN=…
export REVIEWBOARD_API_TOKEN=…
# …then exec grok
```

Do not bake tokens into the TOML. Do not rely on project `.envrc` for these — too late and cwd-scoped. Stdio MCP children inherit the process env plus the per-server `env` table.

### API key + LiteLLM / Vertex Grok (mostly launcher)

Auth advertise path: `XAI_API_KEY` then `GROK_CODE_XAI_API_KEY` (`auth_method.rs`). Admin kill switch: `GROK_DISABLE_API_KEY_AUTH` / `[auth] disable_api_key_auth` (pin). For a fleet that **must** use the corp key, leave that switch off and export the key.

Base URL: `GROK_XAI_API_BASE_URL`, default `https://api.x.ai/v1` (`EndpointsConfig`). Voice STT uses the same resolved base. Catalog fetch is separate (`GROK_MODELS_BASE_URL` / `GROK_MODELS_LIST_URL`) — if LiteLLM’s `/models` is the catalog, set those too or pin models in managed TOML.

Harness work **already done / in flight**: sampling client talks OpenAI-compatible `/chat/completions` (and Responses / Anthropic `/messages`). Uncommitted sampler changes are gateway-shape repairs (missing `sequence_number`, LiteLLM SSE quirks), not UA. Pointing `GROK_XAI_API_BASE_URL` at LiteLLM Vertex Grok is the intended seam; remaining binary work is whatever the Vertex/LiteLLM wire still rejects (tool-call JSON, usage fields, SSE). That stays harness-side, not launcher-side.

Do **not** put the key in managed TOML. Env sits above config files and keeps secrets out of NFS copies.

### User-Agent (likely harness, not launcher)

Sampling `User-Agent` is built in `xai-grok-sampler` / `xai-grok-http`:

```
grok-shell/<version> (os; arch)
```

or, with a distinct origin:

```
<origin>/<origin_ver> grok-shell/<version> (os; arch)
```

`AGENT_PRODUCT` is a **const** (`"grok-shell"`). `GROK_CLIENT_NAME` / `GROK_CLIENT_VERSION` set the **origin** prefix and `x-grok-client-identifier`; they do **not** replace `grok-shell` in the UA. Shared HTTP clients use `process_user_agent_string()` from those env vars plus the const product. There is no `GROK_USER_AGENT` override.

LiteLLM / Vertex billing and allowlists often key off User-Agent. If corp policy needs `apex/…` or `grok-fleet/…` **instead of** `grok-shell`:

- Launcher-only is **not** enough.
- Small harness change: honor `GROK_USER_AGENT` (full string) or treat `GROK_CLIENT_NAME` as the product when set, in both `xai-grok-http::process_user_agent_string` and `xai-grok-sampler::user_agent_string_for`, plus pager `client_identity.rs` (voice).
- Alternative: `[models].extra_headers` cannot replace User-Agent — the client inserts UA after extra_headers (`client.rs` ~701 then ~754). Extra headers can add `X-Request-Tags` etc. for LiteLLM metadata without changing UA.

If LiteLLM only needs an extra header (team, app name), prefer `[models].extra_headers` in managed TOML and leave UA as `grok-shell`.

### Example launcher sketch

```sh
set -a
. /nfs/grok/fleet.env    # tokens, XAI_API_KEY, GROK_XAI_API_BASE_URL, GROK_CLIENT_NAME, …
set +a
export GROK_MANAGED_CONFIG_PATH=/nfs/grok/managed_config.toml   # proposed
# optional: export GROK_CLIENT_NAME=grok-fleet
exec grok "$@"
```

`fleet.env` holds secrets and URLs. `managed_config.toml` holds MCP server blocks (with `${VAR}`), `[skills].paths`, models/extra_headers. Binary still owns UA product string unless we add an override.

## Code map

| Piece | Location |
|---|---|
| Layer merge | `crates/codegen/xai-grok-config/src/config_layers.rs` |
| Overlay allowlist | `crates/codegen/xai-grok-config/src/config_override.rs` (`OVERLAY_ALLOW_PATHS`) |
| `GROK_CONFIG` / `GROK_CONFIG_PATH` | `crates/codegen/xai-grok-config/src/env_overlay.rs` |
| `/etc/grok` | `crates/codegen/xai-grok-config/src/paths.rs` (`system_config_dir`) |
| Managed/requirements loaders | `crates/codegen/xai-grok-config/src/loader.rs` |
| Skill discovery + Bundled walk | `crates/codegen/xai-grok-agent/src/prompt/skills.rs` |
| `SkillScope` / `SkillInfo.body` | `crates/codegen/xai-grok-tools/src/implementations/skills/types.rs` |
| Disk load of skill body | `crates/codegen/xai-grok-tools/src/implementations/skills/skill.rs` (`load_skill_content`) |
| Bundle extract + checksums | `crates/codegen/xai-grok-bundle/src/lib.rs` |
| Startup extract + stale-skill purge | `crates/codegen/xai-grok-shell/src/builtin.rs` |
| Bundle sync ACP | `crates/codegen/xai-grok-shell/src/extensions/bundle.rs` |
| AGENTS.md / rules discovery | `crates/codegen/xai-grok-agent/src/prompt/agents_md.rs` |
| Workspace-user extra dir (`XAI_ROOT` / `XAI_USER`) | `crates/codegen/xai-grok-agent/src/prompt/workspace_user.rs` |
| User-guide (skills / config / rules) | `crates/codegen/xai-grok-pager/docs/user-guide/08-skills.md`, `05-configuration.md`, `12-project-rules.md`, `26-config-reference.md` |
| API key env | `crates/codegen/xai-grok-shell/src/agent/auth_method.rs` (`XAI_API_KEY`, `GROK_CODE_XAI_API_KEY`) |
| Inference base URL | `crates/codegen/xai-grok-shell/src/agent/config.rs` (`GROK_XAI_API_BASE_URL`, default `https://api.x.ai/v1`) |
| Sampling User-Agent | `crates/codegen/xai-grok-sampler/src/client.rs` (`AGENT_PRODUCT`, `user_agent_string_for`) |
| Process User-Agent / `GROK_CLIENT_NAME` | `crates/codegen/xai-grok-http/src/lib.rs` |
| Pager/voice UA | `crates/codegen/xai-grok-pager/src/client_identity.rs` |
| MCP `${VAR}` expansion | `crates/codegen/xai-grok-config/src/loader.rs` (`expand_env_vars_in_toml`); user-guide `07-mcp-servers.md` |
