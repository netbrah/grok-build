# KILOECHO — HARNESS MAP (stream-A independent)

> **SUPERSEDED-WHERE-CONFLICTING (2026-09-19 11:55Z, coordinator):** this inventory is first-hand as of 2026-09-18 (PRE cut-0.5). State of record: grok/plans/smoke-gate-unification-design-20260919.md (upstream root) §6 post-sweep errata + grok/plans/provenance/ledger.md entries 2026-09-19 11:45Z/11:55Z. Known-stale classes: run.py line numbers (cut-0.5 rewrites), case counts (61 -> 74), long-run launch patterns (sweepctl daemon mode + terminal-line registry is the driver of record; nohup/PTY script house rule retired into code).


date: 2026-09-18 · author: coordinator (stream A — KE-1 seat lane transport-dead ×3;
coordinator-authored under backoff ROE) · bead: candidate, apex-bqm family (dedupe owed,
see kiloecho-bead-proposals) · scope: inventory + capability topology of the smoke/
red-team harness stack · status: FINAL v1

STALE-BY-DRIFT: the worktree is under active mutation (.71 GREEN cut in flight by the
sibling session; stream B commits landing). path:line cites are as-read at HEAD on
2026-09-18 ~22:1xZ.

TWO-OVERWATCH NOTE: this map was built from code, harness artifacts, the triage
catalog, and shared planning docs only. Stream-B docs were NOT used as inputs;
cross-stream deltas are recorded in the appendix after the final delta pass.

## 1. Layer 0 — binary + launch contracts

- `grok-responses` binary (worktree `target/{debug,release}/grok-responses`).
  Headless contract: `-p <prompt>` (TRUE headless; the positional-prompt form is the
  TUI launch form and requires a controlling terminal — run-compaction-smoke.sh
  header note), `-m <model>`, `--resume <sid>`, `--output-format json`,
  `--always-approve`, `--agents <json>` (subagent pool definition inline).
- ACP-stdio contract: `grok agent stdio` — NDJSON-RPC 2.0 over stdio, `session/new`
  with `_meta.modelId`, mid-session ops (incl. `session/setModel`) over the same
  channel. This is the only way to script mid-session switches/compaction; the raw
  binary cannot take them.
- L1 env contract (shared by every layer): `env -u OPENAI_API_KEY -u OPENAI_BASE_URL
  -u ANTHROPIC_API_KEY -u ANTHROPIC_BASE_URL GROK_AUTH_EXPIRED=1`; ambient
  `CODEX_LLM_PROXY_KEY` only (run-smoke.sh:44-46, grok-dogfood.v2 run_env).
- Dogfood launcher `smoke/redteam/grok-dogfood.v2` (8.5 KB, staged; coordinator
  installs over `~/.grok/bin/grok-dogfood`; never writes ~/.grok itself):
  - `GROK_DOGFOOD_BIN` — binary path (default: worktree debug build)
  - `GROK_DOGFOOD_LOG` — RUST_LOG; `debug-all` = plain `debug`; default =
    `info,xai_grok_shell=debug,xai_grok_sampler=debug,xai_grok_pager=debug,
    xai_chat_state=debug,xai_grok_agent=debug,xai_grok_config=debug`
  - `GROK_DOGFOOD_WIRECAP=1` — forensic wire capture (default OFF); capture dir
    `~/.grok/dogfood/<UTC-ts>/wire`
  - `GROK_DOGFOOD_CLEANUP=1` — delete capture + temp home post-run
  - `GROK_DOGFOOD_WIRETAP` — wiretap2 path (default: worktree smoke/wiretap/wiretap.py)
  - wiretap2 origin rule: the upstream origin must NOT carry a trailing `/v1`
    (wiretap2 appends grok's full route path `/v1/<route>` to the origin).
- Can drive: any headless session incl. resume, subagent pools, arbitrary RUST_LOG.
  Cannot: wire capture without wirecap mode; TUI-only interactive flows.

## 2. Layer 1 — run-smoke.sh (live acceptance, single-turn)

- 4 fixed cases, table `name|model|prompt|expect-substring|agents-json`
  (run-smoke.sh:17-21): `a-hydrated` (qwen, P1-OK), `control` (sol, CONTROL-OK),
  `b-messages` (sonnet-5, MSG-OK), `c-subagent` (qwen echo pool, CHILD-OK).
- Assertion = expect-substring on the JSON stdout; macOS watchdog (background +
  sleep-kill); temp home per case.
- Can drive: one-turn liveness per model/wire + one subagent-spawn smoke.
  Cannot: multi-turn, switches, compaction.

## 3. Layer 1b — run-matrix.sh (model-matrix diagnostics)

- Case file lines `name|model|backend`; backend ∈ {-, responses, messages,
  chat_completions} (run-matrix.sh:11-13).
- Per-model isolated mktemp home with generated `[endpoints]` config; per-model wire
  evidence via `RUST_LOG=info` stderr (product logs only auth PREFIXES at info —
  the session_setup credential leak is DEBUG, A6-F2; grep the raw key out of every
  log before citing).
- Can drive: one-prompt-per-model × backend matrix with log triage.
  Cannot: multi-turn; no wirecap (log-level evidence only).

## 4. Layer 1c — run-compaction-smoke.sh (P2.1 remote-compaction acceptance)

- Three headless turns in an isolated GROK_HOME (tempdir, `[endpoints]` heredoc:
  models_base_url=proxy, default_api_backend=responses, default_env_key,
  default_context_window=256000, default_model_family=codex):
  1. context turn (sol) — build real history
  2. `/compact` — must take the v2 REMOTE path (v2 log lines on stderr)
  3. follow-up — compacted carrier history replays coherently
- KNOWN GATE (D-ENC): the proxy LBs `/responses` across three Azure regions with no
  session affinity → the `cmp_` carrier cannot decrypt cross-region → t3 fails with
  the designed friendly error ("history incompatible with the current model").
  t1/t2 are the D-ENC-free assertions; t3 green only once the proxy gains
  /responses session affinity.
- Can drive: the remote-compaction v2 path end-to-end (gated). Cannot: local
  compaction paths (that surface is covered by redteam `compact` op cells).

## 5. Layer 2 — redteam ndjson harness (the engine)

- `smoke/redteam/run.py` (5109 L, stdlib-only python3, NO pip, Rust-free by design):
  drives the pager binary in headless mode AND ACP-stdio mode.
  Provenance (run.py:1-38 header): pattern source = the L2 ACP harness
  (`crates/codegen/xai-grok-shell/tests/responses_acceptance.rs` +
  `tests/acp_harness/mod.rs`) + the 01a09be2 over-capacity compaction forensics;
  L1 env contract from run-smoke.sh.
- Case contract — `case.schema.json` (966 L). Top-level case props (as-read):
  agents_json, assert, bead, config_patch, cwd, disabled, driver, env, est_calls,
  expected_red, id, mcp_calls, model, output_format, reason, recon, retry,
  row_asserts, rows, schema_version, scoring, seed, steps, suite, tier, timeouts,
  title, tolerant, tool_calls, unblocks_at, watchdog_s, wire, wirecap.
  Step props (as-read): after_s, assert_form, cell, expect_kill, kill_after_s,
  model, note, op, prompt, s, timeout_s, via. Step `additionalProperties: false`.
- Driver ops implemented (run.py:2908-3100): `turn`, `kill`, `switch`,
  `switch_model`, `compact`, `idle`, `recon_note`.
- Wire-row assertion ops (run.py:1358-1416): `count`, `absent`, `present`
  (absent_ok modifier), `eq`, `ne`, `text_contains`, `tools_absent`, `tools_present`.
- `--selftest` = the OFFLINE gate (apex-ayl.22 D-1/D-2): check_schema draft-07
  dual-path (jsonschema when importable, else hand-rolled keyword subset) over the
  schema_version cases + in-tree case-contract gate over the whole set + the
  offline test suite (test_run.py: step-machine desync replay, 424 BPS init-budget
  sims, case-set contract validation, 16-T smoke-matrix surface). No proxy key, no
  binary, no live calls. This is the ratchet for runner changes.
- `manifest.json` (434 L, 61 cells as-read) — families: `xw` (11 cross-wire cells),
  `rt` (14 legacy corpus: M/S/C-series, crosswire, xreplay, 46c1), `t21` (5 MCP
  tool-call matrix), `ws9` (12 .78 ship-gate rows; case-ops LANDED d404fe1; L2 runs
  = .78-B lane; python3.12 mandatory), `at` (5 adversarial cells, landed eb21336:
  AT-AZ-VXR/VXG, AT-VLQ-VXG, AT-EFFORT-2HOP, AT-SPAWN-XWIRE), `golden-smoke-01`.
  Status strings are the rolling adjudication record (sealed report dirs, binary
  sha12, RULING refs).
- Report homes: `report/<UTC-ts>/{report.md,report.json,<case>/...}`; sealed
  variants report-r1/r2/r2b/r3/r3r/green/at1.
- CAN drive: multi-turn scripts, mid-session model switch (`switch_model` +
  `config_patch` + per-run derived config), compaction (`compact`), kill/interrupt
  (`kill` + expect_kill), resume (case-level `--resume`), MCP tool calls
  (`tool_calls`/`mcp_calls`), subagent pools (`agents_json`; AT-SPAWN-XWIRE covers
  v2-spawn × cross-wire), budget/watchdog control (`est_calls`, `watchdog_s`,
  `timeout_s`), BPS scoring (`scoring`).
- CANNOT (current gaps — feeds §10): mid-run persist-then-resume assertion (no
  `persist_resume` op in enum); effort/label/target_model/tokens step props rejected
  by `additionalProperties: false` (KE-3 N2); wire-FORM pins (the storage form only
  — xwfix §forms); streaming-frame-level assertions (that is wstream's layer).

## 6. Layer 3 — wstream (streaming matrix)

- `smoke/wstream/` (README + manifest + run.py + configs/). Captures STREAMING
  behavior — the harness's real path — across model × api_backend × wire via the
  dogfood hermetic-home mechanism. Motivation: W10-B non-streaming probes hid
  reasoning content on the vertex dialects (grok-4.6 rtok 594, content NULL
  non-streaming); only SSE frames settle fidelity questions.
- 12 cells (manifest): native × 6 — `grok-resp` (PILOT), `sol-resp`, `opus-msg`,
  `gemini-resp`, `qwen-resp`, `glm-resp`; overrides × 6 (backend-cross-wire arm) —
  `sol-msg`, `qwen-msg`, `gemini-msg`, `grok-chat`.
- api_backend → wire: responses → `/v1/responses`; messages → `/v1/messages`;
  chat_completions → `/v1/chat/completions`.
- FROZEN per-run config = live `~/.grok/config.toml` copy + one declared
  `api_backend` patch → `derived-config.toml` in the cell report dir. Deliberately
  NOT full 293-line config copies (maintenance); the same derivation is what the
  redteam switch cells reuse to flip model/backend mid-run.
- OQ table (as-read): OQ-1 sol native-vs-bridge encrypted carry (sol-resp) ·
  OQ-2 gemini reasoning in-stream (gemini-resp/msg) · OQ-7 grok-4.6 thinking
  in-stream (grok-resp/grok-chat) · OQ-3 qwen budget-trap in-stream (qwen-resp) ·
  OQ-5 /messages per-class dispatch (sol-msg/qwen-msg/gemini-msg). Out of harness
  reach: OQ-4 (thoughtSignature multi-turn, /generateContent), OQ-10 (gemini
  naming) — proxy-probe territory.
- Per cell `cells/<cell>/cell.json`: `invariants` (MUST-PASS pins on known W10
  behavior: status/route/model-calls) vs `observations` (open questions the cell
  answers).
- Can drive: streaming SSE frame fidelity, reasoning content in-stream, per-class
  dispatch traces. Cannot: mid-run switches (one turn per cell today).

## 7. Layer 3b — xwfix (cross-wire golden corpus, apex-ayl.70)

- Purpose: TDD RED-first per cell for XW-PROJECT-1 (apex-ayl.71) and live-sweep
  assertions for XW-MATRIX-1 (apex-ayl.72).
- Fidelity ladder per target wire, applied at switch time:
  - T0 native replay — item unchanged (same model+wire+encryption boundary)
  - T1 re-keyed — foreign reasoning: synthesized valid id + encrypted_content
    stripped + summary kept (target tolerates summary-text reasoning)
  - T2 lossy — reasoning/thinking → plain text (or nothing); signatures dropped
  - T3 drop — item removed, portable transcript intact (the .58 reactive
    all-or-nothing behavior, demoted from default to floor)
- Invariants (every cell, every tier): (1) no record/item with id == "";
  (2) no foreign encrypted_content survives into the target request; (3) pairing
  integrity — every tool_result has a matching tool_call; a T3 drop of a call must
  re-project its result to T2 or drop both; (4) carrier survival — compaction
  carrier items + raw_codex_input_replacements survive projection opaquely;
  (5) non-projected records byte-identical to pre_switch.
- T1 id synthesis: `id = "xw_" + sha256("{cell}|{reasoning_index}|
  {json(content,sorted)}|{json(summary,sorted)}").hexdigest()[:24]` — deterministic
  per cell, never wall-clock.
- Forms: storage form (now) = grok conversation records (chat_history.jsonl
  schema), one JSON array per file; wire form (later) = the exact /v1/responses
  `input` array — byte pins land when the `switch_model` case op captures the first
  post-switch request (`smoke/xwfix/switch_model_op.patch`, DRAFT; the .22 lane
  owns the run.py merge).
- Cell record schema (README as-read): cell, flip{from,to: family/model/wire},
  status (PROVEN-REACTIVE | PORTED | UNPROVEN | PREDICTED | synthetic),
  incident|recipe (session dir / code file:line / synthetic), pre_switch{file,
  records, composition}, expected{file, records, projection map}, invariants[],
  red_tests[].
- Evidence discipline: real incident cells cite session dir + source file;
  captures as sha256:12; raw-key sweep = 0 on every file in the tree;
  store=false on any live capture.

## 8. Evidence layer — wiretap2 (smoke/wiretap/wiretap.py)

- Logging pass-through proxy in front of the llm-proxy; the definitive wire-
  evidence layer (HT-1 A1). Extends `~/bin/wiretap.py` (86 L, other agent,
  2026-09-13 — READ-ONLY original, untouched) and incorporates the CDX-1 verified
  one-line fix.
- Capture: per request → `<capture_dir>/req-NNN.json` (method, path, ts,
  headers-MASKED, full body) + `<capture_dir>/resp-NNN.jsonl` (status, response
  headers, then ONE NDJSON line per SSE frame as it streams — frame fidelity, not
  post-hoc parse). Dir 0700, files 0600.
- Auth masking (non-negotiable): `Authorization` (and any header whose value ==
  the ambient key) stored as `{"masked": true, "sha256_12": "...", "len": N}`.
  The raw key is NEVER written to any capture; self-test asserts with a canary key.
- Method coverage: do_GET/do_DELETE/do_PUT pass-through (original 501s on GET);
  ThreadingHTTPServer (original single-threaded).
- CLI: `--port N --upstream URL --capture DIR [--ambient-key KEY]`
  (defaults: 9098, the llm-proxy, no capture).
- CDX-1 regression pin: HTTP/1.0 status line + manual `Transfer-Encoding: chunked`
  is rejected by hyper/reqwest-class clients with `stream disconnected before
  completion`, whose reconnect logic re-POSTs the full request (4× quota leak
  observed). `--selftest` HTTP/1.1 keep-alive SSE assertion pins it.

## 9. Triage layer (smoke/triage/)

- `signatures.json` catalog (18 rows as-read 2026-09-18): seed catalog (T4)
  standing baseline + T4.x annotation discipline in the `note` field (each append
  records date, lane, row ids, provenance). Matching: fixed substring `pattern`
  first, one alternation `regex` second. Unknown signatures emitted as UNKNOWN with
  proposed slug SIG-<class>-<n>; `census --write-catalog` appends them as PROPOSED
  rows (no auto-RCA); proposed slugs deduped across sibling unknowns in-run.
- `install.sh` contract: writes only to `~/.grok/bin` + `~/.agents/skills`
  (sanctioned install locations; never touches sessions/config/logs). The M-1
  guard makes `--write-catalog` on the default under-home path FAIL CLOSED;
  catalog refreshes go to an explicit `--catalog` outside ~/.grok, then
  re-install re-syncs the copy.
- `test_grok_triage.py`: plain-assert (NO pytest), synthetic fake grok-home in
  tempfile, T1/T2/T4 acceptance (census counts + M-1 volatile-value collapse +
  m-3 torn line + show-card fields + m-7 prescreen + M-4 key sweep sentinel
  exit-3 + M-6 --out under home exit-2 + M-3 wirecap numeric pairing; T2 hot mode
  live-session filtering + subagent attribution + --state opt-in + graceful
  degradation; T4 one PROPOSED row per genuinely new class, idempotent, seeds
  untouched).
- Report-and-stop discipline (STOP-1): any new unclassified wire-error/400 shape →
  triage row + xwfix cell + bead, same session. Never edit expected.json/pins/
  mirrors to fit; record class-1 FPs instead.

## 10. Gaps + known holes (feeds unified-gate-design)

1. SCHEMA EXTENSION (prerequisite, blocking): ops `persist_resume` / `set_effort`
   / `seed_context` absent from the op enum; step `additionalProperties: false`
   rejects effort/label/target_model/tokens. The adversarial brief's Surfaces 2+6
   cases FAIL validation as written (KE-3 N2). Surface 1 (xw-vxm-az-live-replay)
   WOULD parse. Note: case-level `seed` + `config_patch` props already exist —
   set_effort may be expressible via config_patch today; persist_resume is the
   genuine gap.
2. WIRE-FORM PINS: xwfix is storage-form only; the switch_model op capture
   (switch_model_op.patch DRAFT) is the golden-cap gap #1 — per-cell
   `expected_wire.json` minting from live captures is the .78-B/.72 dependency.
3. MID-RUN PERSIST-RESUME: resume exists case-level (--resume binary flag) but no
   in-run persist-then-resume assertion op.
4. BPS/LATENCY: `scoring`/`est_calls` props exist + offline 424-BPS sims, but no
   live BPS gate cell yet (bps-plateau carried bead).
5. INFRA (operator lane): on-prem qwen transport — 3 coordinator sub-agent doc
   seats died 2026-09-18 20:3x–22:0xZ with `stream disconnected before completion`
   (zero artifacts each; a 4th seat, single-doc scope, still alive at 22:1xZ).
   The error string is the SAME SIGNATURE CLASS as the CDX-1 documented client
   behavior against a malformed/truncated chunked stream (wiretap2 provenance
   note: client re-POSTs the full request on reconnect — quota-leak vector).
   NOT root-caused here (remote https path, vLLM/litellm side); needs the
   operator's vLLM-side investigation (stream keepalive/timeouts, litellm stream
   timeout, chunked framing). Until then: backoff — single-doc qwen seats, max
   one at a time, or coordinator-authored docs.
6. REGISTRY: the shared-dir README registry is stale (2 registered files absent
   at apex-bqm DRAFT state, 1 on-disk file unregistered — KE-3 N3). Coordinator
   consolidates in one guarded single-writer pass after both streams' docs land.
7. TRIAGE ERRATA (owed, next exfil pass): signatures.json row #17 RCA text says
   "stock v1.90.0 source path does not contain it" — DISPROVEN first-hand
   2026-09-18: `normalize_reasoning_effort_value` is stock v1.90.0
   (`litellm/llms/anthropic/experimental_pass_through/utils.py:16-57`, callers
   `adapters/handler.py:389,400,407` messages path + `responses_adapters/
   handler.py:80,84` responses path; checkout HEAD = v1.90.0 tag exactly).
   Correct attribution: stock clamp + model_info flag absence (config-side),
   not a proxy-image delta.

## 11. Cross-stream delta appendix (final delta pass, read-only)

vs stream-B `harness-map-20260918.md` (12-suite inventory table):
- CONVERGE (independent): suite inventory, the L0 Rust-unit tier boundary
  (in-crate `include_str!` fixtures: xw_orphan / projection_x71 / affinity;
  "mirrors must stay byte-equal to the smoke corpus — drift notes, never
  edit-to-fit-RED"), manifest 434 L / 61-cell registry.
- DELTA-1: their map carries no tracked-state of `smoke/redteam/manifest.json`
  — it is UNTRACKED at HEAD (adjudication record absent from the repo). The
  promotion doc (§2) owns the commit decision; stream-B silent on it.
- DELTA-2: their map names `testdata/xw_orphan/` + `testdata/affinity/` in the
  L0 tier; this map cites projection_x71 only. Complementary, no conflict.
- DELTA-3: xwfix cell count framing — they say "cells/ 10 + deferred 5,
  manifest 542 L"; this map counts 11 xw-family case FILES in redteam/
  (including xw-vlq-vlg-ws, a guard variant). Corpus identity is the same;
  the deferred-5 set should be listed by id in the registry consolidation.

---
Raw-key sweep: 0 (this doc carries no credentials; key references use sha256:12
or env-var names only).
— end —
