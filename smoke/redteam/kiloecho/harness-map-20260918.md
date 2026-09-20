# Harness map — every runner/suite in the grok-build-responses worktree

> **SUPERSEDED-WHERE-CONFLICTING (2026-09-19 11:55Z, coordinator):** this inventory is first-hand as of 2026-09-18 (PRE cut-0.5). State of record: grok/plans/smoke-gate-unification-design-20260919.md (upstream root) §6 post-sweep errata + grok/plans/provenance/ledger.md entries 2026-09-19 11:45Z/11:55Z. Known-stale classes: run.py line numbers (cut-0.5 rewrites), case counts (61 -> 74), long-run launch patterns (sweepctl daemon mode + terminal-line registry is the driver of record; nohup/PTY script house rule retired into code).


Date: 2026-09-18 (UTC; written 23:1x-23:3xZ)
Author/seat: kiloecho_harness_qwen (qwen work seat, KE-1, apex-v2-grok-build campaign)
Bead: `apex-bqm` (HARNESS-UNIFY-1)
Sources (all first-hand this session unless marked): worktree
`/Users/palanisd/Projects/upstream/wt/grok-build-responses` @ HEAD **0fc1060**
(branch `feat/first-class-responses-catalog`; dirty with the .71 cut — read-only for this
seat); `~/Projects/upstream/grok/plans/` (ledger + per-lane specs); live proxies: 3 bounded
probes KE-P1..P3 (recorded in §6). Key sha12 `9f3f56a263da` (never echoed).
Status: **DRAFT**
Siblings (same dir, do-not-duplicate): `wire-topology-matrix-20260918.md` (glm, apex-6vu),
`error-class-audit-20260918.md` (glm), `adversarial-brief-20260918.md` (glm),
`litellm-transform-matrix.md` (KE-2 qwen; in flight at write time — §0/§1 landed, §4 pending).

## §0 — Scope + reading notes

Maps ALL smoke/red-team surfaces in this repo (5 python suites + 3 top-level zsh runners +
the ACP dogfood launcher + the in-tree Rust acceptance/unit tiers) and registers the
`plans/` intel corpus (§4). Companion design doc: `unified-harness-design-20260918.md`
(same bead). Conventions per `kiloecho/README.md` (path:line cites @ the HEAD read;
sha256:12 only; pointer-never-copy).

Reading notes (for the record):
- This seat's first pass (22:4xZ) ran before the README + `71-handoff-brief-20260918.md`
  were placed in this dir; the .71-brief facts used then came from the ledger summary
  (plans/ledger.md 22:4xZ entry). Both files have since been verified first-hand; no
  delta to the map resulted (brief §4 R3 base, §5 re-pin table, §6 gaps, §7 constraints
  all agree with the in-repo evidence cited below).
- `71-handoff-brief-20260918.md` is the governing R3/L2 contract for the xw corpus
  (11-cell R3 base in `smoke/redteam/report-r3r/`, 15 model calls, 11/11 per-cell seals
  clean; key sha12 `9f3f56a263da`; campaign `apex-ayl.70-r3`, wave `R3-RED`, git 0fc1060).

## §1 — Inventory

### 1.0 Master table

| # | Suite | Path | Size | Invocation | Transports | Verdict mechanism | Gates today | Cells | Known limits |
|---|---|---|---|---|---|---|---|---|---|
| 1 | L1 CLI smoke | `smoke/run-smoke.sh` | 83 L | `smoke/run-smoke.sh [scenario...]` | headless-ndjson only | exit 0 + JSON `text` substring | P1+ live-acceptance (not the wave gate) | 4 fixed | no wire/ACP; debug binary default |
| 2 | L2 ACP acceptance | `crates/codegen/xai-grok-shell/tests/responses_acceptance.rs` + `tests/acp_harness/mod.rs` (419 L) | — | `cargo test -p xai-grok-shell --features xai-grok-shell/test-support --test responses_acceptance -- --ignored` | ACP protocol (in-process `MvpAgent` over duplex pipes) | per-scenario asserts (reply text, no auth_error updates, `SubagentFinishedRecorder`) | 4 `#[ignore]` live tests; canonical gate is no-network | 4 fixed | in-process (no binary packaging); fixed scenarios |
| 3 | **L3 redteam runner (canonical)** | `smoke/redteam/run.py` | 5109 L | `python3.12 smoke/redteam/run.py [case-id...] [--budget N] [--no-wirecap] [--rows a,b] [--keep-home] [--bin PATH] [--out DIR] [--campaign-id ID] [--wave W] [--mode slim\|full\|adhoc] [--runbook F] [--campaign-dir D] [--aggregate]`; `--selftest` = offline gate | headless-ndjson + ACP stdio (per-case `driver`) | verdict engine `finalize_verdict` (run.py:2491): premise pass (harness 0-hit→BLOCKED, model 0-hit→VACUOUS), tolerant→FINDING-PASS, `require_wire_evidence`→NO-EVIDENCE BLOCKED; `expected_red`→RED-EXPECTED rendering (run.py:2669 mark()); retry loop→FLAKY (run.py:2614) | L2/L3 live matrix; R3 sealed base `report-r3r/` | 61 (rt 30, ws9 14, xw 11, t21 5, golden 1) | key+binary required even for offline mechanism cases; no resp-body/stream-frame pins; op wire hook normalize hardcoded `[]` |
| 4 | Offline test suites | `smoke/redteam/test_run.py` + `test_run_golden.py` | 73 + 19 tests | `python3.12 smoke/redteam/run.py --selftest` (schema draft-07 dual-engine + case-contract gate + unittest) | n/a (offline; no key, no binary, no proxy) | unittest PASS/FAIL | offline gate (part of unified gate L0/L1) | 92 tests | unit-level only (no binary) |
| 5 | wstream | `smoke/wstream/run.py` + `cells/` (12) | 1286 L + 12 cell.json | `python3 smoke/wstream/run.py [--selftest\|--cell X\|--cells a,b\|--native]`; env `WSTREAM_BIN` (default `target/release/grok-responses`), `WSTREAM_UPSTREAM`, `WSTREAM_TIMEOUT_S` (180) | headless streaming-json only | `invariants` MUST-PASS (status 200, route, n_model_calls_min) vs `observe` (no pass/fail — intel capture) | W10 OQ streaming matrix; ws9-s01/s12 cells disabled by default | 12 (10 native/override + 2 ws9 additive, `enabled:false`) | single turn/cell; no ACP, no switch op yet (extension seam documented in its README) |
| 6 | xwfix corpus | `smoke/xwfix/` (cells/ 10 + deferred 5, manifest 542 L, README, `switch_model_op.patch` DRAFT) | — | consumed by suite 3 via `switch_model` op (not a standalone runner) | n/a (fixture set) | storage-form deep equality `xwfix_cell_diff_storage` (run.py:1679) at switch time; wire-form dormant (no `expected_wire.json`) | the .70/.71 RED corpus (TDD RED-first per cell) | 10 cells + 5 deferred | storage form active / wire form DORMANT (fixtures mint .71 green-time) |
| 7 | wiretap2 (capture layer) | `smoke/wiretap/wiretap.py` | 610 L | `wiretap.py --port N --upstream URL --capture DIR [--ambient-key]`; `--selftest` (offline) | n/a (passthrough proxy) | per-request `req-NNN.json` + `resp-NNN.jsonl` (frame fidelity); auth masked to `{masked, sha256_12, len}`; self-test asserts masking + HTTP/1.1 SSE keep-alive | wire evidence for suites 3/5 + dogfood | — | captures only; no assertions itself |
| 8 | triage | `smoke/triage/` (`grok-triage` 1130 L, `signatures.json` 18 rows, `test_grok_triage.py` 548 L, `install.sh`) | — | `grok-triage` (post-mortem classifier on a session dir); tests via unittest | n/a (offline analysis) | regex signature match → class + rca + bead + pointer | NOT a live gate; recon sweep input (report-and-stop registry) | 18 signatures (4 OPEN, 2 IGNORED, 12 PROPOSED) | passive — only as good as the signature rows |
| 9 | Model-matrix diagnostic | `smoke/run-matrix.sh` | 81 L | `smoke/run-matrix.sh <case-file>` (`name|model|backend` lines) | headless-ndjson only (`--output-format json`) | exit 0 + `MATRIX-OK` substring; stderr `RUST_LOG=info` grep for triage | roadmap item-11 pre-pass diagnostics | per case-file | **no wire capture** (stderr-only evidence); one turn/model |
| 10 | P2.1 compaction smoke | `smoke/run-compaction-smoke.sh` | 111 L | `smoke/run-compaction-smoke.sh` | headless-ndjson only (`-p`, `-c` continue) | t1 non-empty; t2 v2-remote log greps (`Codex remote compaction v2 stream completed` + `installed ... v2 replacement history`); t3 non-empty | P2.1 v2 remote-compaction acceptance (sol-only, D-ENC gated — t3 green only with proxy /responses session affinity) | 3 fixed turns | sol-only; log-grep (not wire) evidence; D-ENC known gate |
| 11 | ACP dogfood launcher | `smoke/redteam/grok-dogfood.v2` | 192 L | `grok-dogfood [args...]`; env `GROK_DOGFOOD_BIN`, `GROK_DOGFOOD_LOG`, `GROK_DOGFOOD_WIRECAP=1`, `GROK_DOGFOOD_CLEANUP=1`, `GROK_DOGFOOD_WIRETAP` | headless (args pass-through; ACP reachable by caller driving `agent stdio` through it) | binary exit + wirecap manifest (key sha12, capture dir, exit) + raw-key sweep (exit 3 on hit) | operator forensic dogfood (not automated) | — | staged (never auto-installed; coordinator installs over `~/.grok/bin/grok-dogfood`); default BIN = worktree **debug** build, absent on disk at write time (verified — `GROK_DOGFOOD_BIN` override required) |
| 12 | L0 Rust unit tier (in-crate) | `crates/codegen/xai-grok-sampling-types/testdata/xw_orphan/` (+ `PROVENANCE.md`), `src/conversation/fixtures/{xsearch_replay,projection_x71}/`, `testdata/affinity/` | — | `cargo test` (6-pkg gate, §6 of design doc) | n/a (hermetic; `include_str!` fixtures) | parse-equality / KAT asserts; byte-mirror + sha256:12 mint pattern (`PROVENANCE.md`) | canonical 6-pkg gate + A6 name-by-name | xw_orphan 4 fixtures (9/9 sampling-types, 1/1 sampler, 1/1 shell, 132/132 compaction per plans/xwire/review-gates-q3.md §3); projection_x71 5 fixtures (.71, in flight) | mirrors must stay byte-equal to the smoke corpus (drift notes, never edit-to-fit-RED) |

### 1.1 L1 CLI smoke (`smoke/run-smoke.sh`, 83 L)
- Purpose: P1+ live-acceptance, Layer 1 of `plans/smoke-harness-spec.md`. Four fixed
  scenarios, one headless `-p` turn each: `a-hydrated` (qwen3.8-27b → `P1-OK`),
  `control` (gpt-5.6-sol → `CONTROL-OK`), `b-messages` (claude-sonnet-5 → `MSG-OK`),
  `c-subagent` (echo child via `--agents` JSON → `CHILD-OK`).
- Env contract: `env -u OPENAI_API_KEY -u OPENAI_BASE_URL -u ANTHROPIC_API_KEY
  -u ANTHROPIC_BASE_URL GROK_AUTH_EXPIRED=1`; per-case watchdog (default 120 s) via
  background `sleep`+`kill -9` (no GNU timeout on stock macOS).
- Verdict: exit 0 + `text`/`stopReason` JSON substring; PASS/FAIL table; non-zero exit
  on any failure. No wire capture, no ACP, no report dir — stdout is the evidence.
- Gates: P1 live-acceptance (GREEN 2026-09-12 per spec). Not the wave gate.

### 1.2 L2 ACP acceptance (in-tree Rust)
- Spec: `plans/smoke-harness-spec.md` §Layer 2 (v1 FINAL 2026-09-12). Reuses
  `crates/codegen/xai-grok-shell/tests/acp_harness/mod.rs` (419 L): in-process `MvpAgent`
  over duplex pipes — `connect_and_auth`, `new_session` (`_meta.modelId` validated
  against the /rest/modes catalog), `prompt_turn` (RPC_TIMEOUT 60 s),
  `SubagentFinishedRecorder`, `run_agent_test_with_models` (mock server + tempdir
  GROK_HOME).
- The four `#[ignore = "live proxy acceptance"]` scenarios: `l2_hydrated_no_auth_error`,
  `l2_set_model_mid_session`, `l2_messages_wire`, `l2_subagent`. Model-switch surface
  here = ACP `session/set_session_config_option` config_id `"model"` (handlers/
  config_option.rs:27 → `set_model_gated` mvp_agent/acp_agent.rs:37, dispatched :1926).
- Hermeticity: tempdir GROK_HOME with a minimal `[endpoints]` config (P1 block); key read
  from process env `CODEX_LLM_PROXY_KEY`, never written to disk; unset key → panic.
  Canonical live run: `RUST_MIN_STACK=67108864 CARGO_INCREMENTAL=0
  CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=4 cargo test -p xai-grok-shell
  --features xai-grok-shell/test-support --test responses_acceptance -- --ignored`.
  `LIVE_RPC_TIMEOUT` 180 s (env `GROK_L2_TIMEOUT_SECS`).
- Boundary: L2 covers the protocol surface in-process; L1 binary covers
  packaging/CLI/env. Both kept; L2 does not replace L1.

### 1.3 L3 redteam runner — the canonical engine (`smoke/redteam/run.py`, 5109 L)
- Provenance (run.py:1-30): pattern source = L2 acp-harness + the 01a09be2
  over-capacity local-compaction forensics; stdlib python3 only, NO pip deps,
  Rust-free by design; drives the built pager binary in headless mode
  (`-p`/`-m`/`--resume`/`--output-format`) and ACP-stdio mode (`agent stdio`,
  NDJSON-RPC 2.0).
- Case contract: `cases/*.json` (61 files) validated against
  `case.schema.json` (966 L) — draft-07 dual-engine gate (jsonschema when importable,
  else hand-rolled keyword subset `draft07_gate` run.py:3984) for NEW cases + in-tree
  case-contract gate (`validate_cases_dir` run.py:4457) over the whole set; legacy
  `rt-*` cases are schema-exempt (run.py main selftest: "legacy cases exempt").
  `manifest.json` (434 L) = the 61-cell registry: `cells` (case, file, family, status
  note), `pins` (golden-smoke-01 fixture sha + xwfix_corpus pointer), `expected_refs`
  (ratchet_1 ef5192b, ratchet_2 15 runs, wave_gate 5f4411a, exfil d404fe1, r3),
  `probe_policy` (store=false MUST; raw-key sweep 0; per-turn kill budget 180 s; one
  retry on transport error; report trees pruned after digest capture).
- Case schema surface (case.schema.json, first-hand): 33 top-level props;
  `driver: headless|acp`; `output_format: streaming-json|streaming-messages-json|json`;
  `wire: responses|messages|chat_completions`; `suite: wire-shape|tool-call|mcp-call|
  mcp-tool|cross-wire|compaction|salvage|recon|session-resume`; `tier: slim|full`;
  step ops `turn|switch|switch_model|kill|compact|idle|recon_note` (`via: acp|resume`;
  `assert_form: storage|wire`); wire kinds `field|grep|size_lt|count|resp_status|
  recon|golden` (golden kind schema-pinned at L780-786 per manifest.json); ndjson ops
  `count|absent|present|eq|ne|text_contains|tools_absent|tools_present|recon`;
  artifact ops `count|grep`; `scoring.vacuous_if[{pin,class: model|harness}]`;
  `scoring.require_wire_evidence`; `env{home: hermetic|temp, launcher: runner|dogfood,
  store}`; `mcp_calls[{id,server,tool,expect_name,via: search_tool_use_tool|direct,
  args,premise: model|harness,output_contains}]`; `tool_calls[{id,tool,args,premise:
  model,output_contains}]`; `expected_red{id,reason?}`; `retry{on_status,
  max_attempts,backoff_s,recheck_models}`; `rows` (row-based matrix cases) +
  `row_asserts`.
- Execution (`_run_case_once` run.py:2656): hermetic home (`HermeticHome` run.py:368 =
  live config copy + `config_patch` surgery `_apply_config_patch` run.py:267 +
  `_apply_no_watch` run.py:343 + `_align_models_cache` run.py:424 pointing the catalog
  cache at the wiretap) → optional history seed (`seed_history` run.py:484, synthetic
  `chat_history.jsonl` to a token target, `validate_roundtrip` run.py:558) or xwfix
  cell seed (`xwfix_seed_from_cell` run.py:1643, `pre_switch.json` verbatim) → driver:
  headless turns (`run_headless_turn` run.py:676, resume via carried session id) or
  ACP session (`AcpSession` run.py:813: `bin agent stdio`; `initialize` → `session/new`
  with `_meta.modelId`, or LOAD-FIRST `session/load` on a pre-minted sid for cell-
  seeded ACP cases — Option A ruling 2026-09-18; model switch via
  `session/set_model` run.py:1193; updates tracked via `_x.ai/session_notification`)
  → step dispatch run.py:2908-3104: `turn` (:2908), `kill` (:2996, `kill_after_s`
  mid-turn SIGKILL), `switch` (:3014, via=acp|resume), `switch_model` (:3032 — switch +
  cell storage diff AT SWITCH TIME, scored before the post-switch turn), `compact`
  (:3068 — ACP prompt `/compact` or headless `/compact` turn), `idle` (:3096),
  `recon_note` → wirecap end: model-call count (`count_wire_model_calls` run.py:731) +
  the xwfix wire-form hook (run.py:3126-3165: filter `req-*.json` by
  `{method:POST, body.model:<target>}`; `expected_wire.json` absent → SKIP with note;
  present → `golden_compare(first, fixture, [])` — normalize hardcoded `[]`, selection
  first-by-filename) → assert block: `assert.ndjson` (check_ndjson run.py:1355),
  `assert.artifact` (check_artifact run.py:1435, session-dir copy), `assert.wire`
  (check_wire run.py:1898; plus synthesized `output_contains` pins
  `synth_output_pins` run.py:1593 for declared mcp/tool calls), `assert.exit`.
- Compare engine (golden, run.py:1716-1896): `_golden_canon` (:1716,
  `json.dumps(sort_keys=True, separators=(",",":"))` — object key order insensitive,
  LIST ORDER SIGNIFICANT), `_golden_null_path` (:1725, dotted-path nulling incl.
  `[i]` forms; absent path = no-op, loud-not-fatal), `_golden_field_diff` (:1781,
  first-5 divergences, 120-char snips), `golden_compare` (:1822, capture `.body` vs
  fixture, normalize N/M loudness :1843-1858), `_golden_write_diff` (:1884,
  `golden-diff-<id>.json` TDD artifact). Storage form: `xwfix_cell_diff_storage`
  (run.py:1679, Python `==` order-sensitive deep equality on `chat_history.jsonl` vs
  cell `expected.json`).
- Verdict (`finalize_verdict` run.py:2491): premise pass over
  `scoring.vacuous_if` precedes pin scoring — harness-class premise 0-hit → BLOCKED
  (rig failed); model-class 0-hit → VACUOUS (probe meaningless, beats a scored FAIL);
  else scored pins decide (any FAIL → FAIL, or FINDING-PASS with findings for
  `tolerant` cases; all ok → PASS). `require_wire_evidence` → `audit_wire_evidence`
  (run.py:2362) downgrades a claim-without-file to NO-EVIDENCE BLOCKED (audited kinds:
  `field|grep|resp_status|size_lt`, run.py:2320; `count` exempt — its cite is the
  matched file set). recon asserts never score. Status order (run.py:2309): PASS,
  FINDING-PASS, RECON, VACUOUS, FAIL, BLOCKED, SKIP. `expected_red` (case-level, e.g.
  `apex-ayl.71`, `REPLAY-1` in rt-r2.json) renders a FAIL as RED-EXPECTED (mark()
  run.py:2669) — exit code stays 1; read the log line, not the rc.
- Retry loop (`run_case` run.py:2614): env retry on FAIL/BLOCKED per `retry` block
  (`_retry_decision` run.py:2451, `recheck_models_available` run.py:2422 probes
  /v1/models); attempt-N under `<case>/attempt-N/` (evidence retained); retried-pass
  = stability FLAKY.
- Campaign machinery: `seal_campaign` (run.py:4478) — ONE write at run start:
  byte-exact runbook copy + `campaign.json` with `sealed_sha256` = canonical digest
  over manifest fields with `started_utc` EXCLUDED (stable sha for identical inputs);
  `validate_campaign` (run.py:4555) + `aggregate_summary` (run.py:4646) = the offline
  lane (`--aggregate --campaign-dir`): per-cell final-attempt outcome
  (FAIL > BLOCKED > TIMEOUT > PASS), stability (FLAKY > STABLE > NOT_ASSESSED),
  campaign = worst case; evidence grade (`_evidence_grade` run.py:4633): wire >
  ndjson > recon (file cite > event_line cite > none). Per-cell `verdict.json`
  (`write_verdict_json` run.py:3433) carries outcome/stability/evidence_index + a
  `triage` block (paths + ready_commands for the hunt protocol).
- Pre-placement (run.py:2713-2740): `cases/<stem>/wire/` inputs copied
  copy-if-absent into the run wire dir (mechanism-case convention; GHOST rule — a
  pre-placed `req-*.json` the run never generates persists; expected fixtures use
  non-colliding names). `golden-smoke-01` is the zero-step mechanism case proving the
  `kind=golden` path (report-r1 GREEN; fixture
  `cases/golden-smoke-01/wire/golden-smoke-01-expected.json`, source capture sha12
  cfed3352cc1c per manifest.json `pins`).
- Redaction: `redaction_sweep` (run.py:3515) over the run dir incl. generated reports;
  hits are LOGGED ("inspect before sharing") — the sweep does not by itself flip the
  process exit code (exit keys off FAIL/BLOCKED rows, run.py:5097-5099).
- R3 governing evidence: `report-r3r/` — campaign `apex-ayl.70-r3`, 11 xw cells,
  15 model calls, per-cell `campaign.json` (sealed, wave `R3-RED`, git 0fc1060) +
  nested `<cell>/<cell>/verdict.json`; PASS ×3 (az-az 7.1s c1, az-vxm 13.4s c1,
  vlq-vlg-ws 16.5s c2), RED-EXPECTED(.71) ×7, UNPROVEN ×1 (vlq-vlg wall, .68 owns —
  do not re-pin). Second-assert analysis: 4 cells era-stale pins flip the GOOD
  direction (re-pin at promotion, never weaken — re-pin table in 71-handoff-brief §5).
- Run dirs in manifest `run_history_dirs`: report/, report-r1/, report-r2/,
  report-r2b(+invocation-error-0432Z)/, report-r3/, report-r3r/, report-green/.

### 1.4 wstream (`smoke/wstream/`)
- Purpose: the STREAMING matrix — model × api_backend × wire — because W10-B's
  non-streaming probes hid reasoning content on the vertex dialects; the harness
  always streams, so streaming SSE frames are the only reachable evidence
  (README "Why streaming is the real path").
- Cells (manifest.json, 12): native `grok-resp` (PILOT) · `sol-resp` · `opus-msg` ·
  `gemini-resp` · `qwen-resp` · `glm-resp` + override arms `sol-msg` · `qwen-msg` ·
  `gemini-msg` · `grok-chat` + additive `ws9-s01-toolloop-sol` / `ws9-s12-alias-claude-
  msg` (both `enabled:false`, explicit launch only, apex-ayl.78 Phase-A).
- Cell format (`cells/<cell>/cell.json`): `invariants` (MUST-PASS: `status:200`,
  `route`, `n_model_calls_min`) vs `observe` (capture-only: `all_stream`,
  `reasoning_present`, `reasoning_encrypted_content`, `reasoning_thinking_block`,
  `reasoning_thought_signature`, `reasoning_frames`, `finish`, `budget_trap`,
  `final_text_nonempty`, `usage`).
- FROZEN CONFIG of record: `derived-config.toml` = live `~/.grok/config.toml` copy +
  one declared `api_backend` patch + wiretap base_url rewrite, written to the cell's
  report dir per run (deliberately NOT a stored 293-line config copy).
- Report: `report/<UTC-ts>/matrix.json` + per-cell `report.md`/`result.json`/
  `derived-config.toml`/`turn_1.ndjson`/`capture/` (wiretap2). Raw-key sweep asserted
  per cell (REDCTION VIOLATION on hit); store=false on every responses route.
- Provenance: mechanism COPIED (not imported) from `smoke/redteam/run.py` — the
  README states redteam/ is the operator's actively-edited lane, so the pattern is
  duplicated on purpose.
- Extension seam (README "Extension"): the per-cell api_backend patch + hermetic home
  is exactly what a mid-run switch cell op needs (turn A/backend X → re-derive config
  → resume turn B/backend Y → assert cross-wire projection) — generalizes xwfix from
  static fixtures to a live switch-matrix.

### 1.5 xwfix corpus (`smoke/xwfix/`)
- Purpose (README): golden fixtures for cross-wire model-switch projection —
  per-cell `pre_switch.json` (byte-pinned pre-switch history) + `expected.json`
  (expected post-switch projection), so apex-ayl.71 is TDD RED-first per cell and
  apex-ayl.72 has live sweep assertions.
- Fidelity ladder (per target wire, at switch time): T0 native replay · T1 re-keyed
  (foreign reasoning: synthesized `xw_` id + encrypted_content stripped + summary
  kept) · T2 lossy (reasoning/thinking → plain text) · T3 drop (portable transcript
  intact, the .58 reactive floor). Id synthesis: `xw_` + sha256(`{cell}|{ord}|
  {json(content,sorted)}|{json(summary,sorted)}`)[:24] (deterministic, no clock).
- 5 invariants (every cell/tier): no `id==""`; no foreign encrypted_content; tool
  pairing integrity; carrier survival (compaction carriers opaque); non-projected
  records byte-identical.
- Forms: storage form ACTIVE (all 4 live cells `assert_form:"storage"`); wire form
  DORMANT (0 `expected_wire.json`; op hook skips with a note — golden-engine doc §2).
- Manifest (542 L): 10 cells + 5 deferred + `red_runs_r2` machine record (15 STAGE-2
  RED runs, RULING 3 07:08Z adjudication — the first live ACP session/load
  validation of switch_model).

### 1.6 wiretap2 (`smoke/wiretap/wiretap.py`, 610 L)
- The definitive wire-evidence layer (HT-1 A1). Provenance header: extends
  `~/bin/wiretap.py` (86 L, untouched) + the CDX-1 verified one-line
  `protocol_version = "HTTP/1.1"` fix (root cause: HTTP/1.0 status line + manual
  chunked re-encode rejected by hyper/reqwest → reconnect re-POSTs the full request,
  4× quota leak; the `--selftest` HTTP/1.1 keep-alive SSE assert is the regression pin).
- Capture shape: `req-NNN.json` `{n, method, path, ts, headers-MASKED, body}` +
  `resp-NNN.jsonl` (line 1 `{n, status, ts, headers}`, then ONE NDJSON line per SSE
  frame `{frame_index, frame}` — frame fidelity, not post-hoc parse). Dir 0700,
  files 0600.
- Auth masking (non-negotiable, wiretap.py:35-62 per golden-engine doc §3):
  `Authorization` (and any header equal to the ambient key) → `{masked: true,
  sha256_12, len}`; raw key NEVER written; self-test asserts with a canary key.
- Methods: POST/GET/DELETE/PUT pass-through (original 501'd on GET — the harness
  prefetches /models + /model/info); `ThreadingHTTPServer`; `CERT_NONE` SSL context
  (corp MITM CA); 600 s upstream timeout; chunked re-encode.
- CLI: `--port N --upstream URL --capture DIR [--ambient-key KEY]` (defaults 9098,
  the llm-proxy, no capture); ambient key order: `--ambient-key` > `WIRETAP_AMBIENT_KEY`
  env > `CODEX_LLM_PROXY_KEY`; `--selftest` = offline in-process stub upstream
  (canned SSE + 400) asserting body round-trip, frame order, masking, GET pass-through.
- Consumers: redteam runner (`Wiretap` class run.py:1255, `--capture run_dir/wire`),
  wstream runner, dogfood v2 wrapper.

### 1.7 triage (`smoke/triage/`)
- `grok-triage` (1130 L) + `test_grok_triage.py` (548 L) + `install.sh`: post-mortem
  error classifier run on a copied session layout (never `~/.grok` — the hunt
  protocol consumes the runner's `session/` copy incl. `acp.log`, `unified.jsonl`,
  wire dir).
- `signatures.json` (18 rows): `{id, class, pattern, regex, rca, bead, status,
  pointer}`. Status split at read time: 4 OPEN (#1-#4, MCP spawn/handshake/timeout
  classes), 2 IGNORED, 12 PROPOSED (including the .74/.81/.82-era rows; #17 = M-1
  effort-clamp, newest per HEAD 0fc1060 commit message). Single-writer discipline
  (KE-3 owns filing; report-and-stop on new error shapes — F-N3 precedent:
  ledger 2026-09-18 22:09Z entry recorded a PROPOSED-row candidate and stopped).
- Role in the gate: NOT a live gate — a recon sweep: every verdict FAIL/BLOCKED gets
  a triage block (verdict.json `triage` section: classification_hint +
  ready_commands); new unclassified shapes must land as signature rows.

### 1.8 Model-matrix diagnostic (`smoke/run-matrix.sh`, 81 L)
- Purpose: roadmap item-11 pre-pass (2026-09-12). Headless, one model at a time,
  one short prompt (`MATRIX-OK`), per-model wire evidence from `RUST_LOG=info`
  stderr only (NO wire capture — the script comments note info-level logs only auth
  PREFIXES; the DEBUG session_setup credential leak A6-F2 is excluded; still grep
  the raw key before citing logs).
- Per-model FROZEN CONFIG (tempdir GROK_HOME, written per case): `[endpoints]`
  block = `models_base_url` (env `GROK_MATRIX_PROXY_BASE`, default the llm-proxy
  `/v1`), `default_api_backend = "responses"`, `default_env_key =
  "CODEX_LLM_PROXY_KEY"`, `default_context_window = 256000`, `default_model_family =
  "codex"`, `default_agent_type = "grok-build-plan"`; optional per-model `[model.
  "<name>"] api_backend = "<responses|messages|chat_completions>"` override from the
  case file's third column (`-` = stock resolution).
- Input format: case file lines `name|model|backend`; env `OUTDIR` (default
  /tmp/matrix-20260912), `TIMEOUT` (90 s). Verdict: exit 0 + MATRIX-OK substring.
- This is where `strict_responses_input`/`context_window`/`family`/`effort` are NOT
  set by the harness today (see §2.4).

### 1.9 P2.1 compaction smoke (`smoke/run-compaction-smoke.sh`, 111 L)
- Purpose: P2.1 live-acceptance Layer-1 — codex remote compaction v2 over
  `/responses`. Three headless turns in an isolated tempdir GROK_HOME (P1
  `[endpoints]` config, key env-only): t1 context turn (gpt-5.6-sol, 400-500 word
  overview), t2 `/compact` via `-c` continue — must take the v2 REMOTE path (two
  stderr log greps: `Codex remote compaction v2 stream completed` + `installed Codex
  remote-compaction v2 replacement history`), t3 follow-up on the compacted carrier
  history.
- KNOWN GATE (D-ENC): the proxy LBs `/responses` across three Azure regions with no
  session affinity, so the `cmp_` carrier cannot decrypt cross-region and t3 fails
  with the designed friendly error; t3 goes green only once the proxy gains
  `/responses` session affinity. t1/t2 are the D-ENC-free assertions.
- Launch-form note (load-bearing for transport mapping): `-p <prompt>` is the TRUE
  headless form; the positional-prompt form is the TUI launch form and requires a
  controlling terminal.

### 1.10 ACP dogfood launcher (`smoke/redteam/grok-dogfood.v2`, 192 L)
- Role: the operator's instrumented dogfood launcher (HT-1 A5). STAGED in-repo; the
  coordinator installs it over `~/.grok/bin/grok-dogfood` — the file is never
  auto-installed and never writes to `~/.grok` itself.
- Plain mode (default): v1 contract (provider vars unset, `GROK_AUTH_EXPIRED=1`)
  with the v2 default debug scope `RUST_LOG=info,xai_grok_shell=debug,xai_grok_
  sampler=debug,xai_grok_pager=debug,xai_chat_state=debug,xai_grok_agent=debug,
  xai_grok_config=debug` (`GROK_DOGFOOD_LOG=debug-all` → plain `debug`).
- Wirecap mode (`GROK_DOGFOOD_WIRECAP=1`, forensic, default OFF): free port →
  wiretap2 with `--capture ~/.grok/dogfood/<TS>/wire/`; tempdir GROK_HOME = copy of
  live `config.toml` (+ `proxy-auth-stub.sh`) with `base_url`/`models_base_url`
  sed-rewritten to the loopback wiretap; `models_cache.json` mirrored + aligned
  (origin/identity/renewed_at) so `-m <catalog-model>` resolves offline; `auth.json`
  deliberately NOT copied (HT-1.2-A5: loopback base_url is first-party → session-
  cached credential re-activates the token-refresh gate → 403; without it the env
  key rides `env_key`); targeted `sessions/<sid>` copy for `-r/--resume <sid>`
  (full copy for `-c`/`--continue`/`GROK_DOGFOOD_COPY_SESSIONS=all`).
- Manifest = redaction contract: prints ts, binary sha12, key sha12 (value never
  printed), wirecap port, capture dir, unified.jsonl path, exit; exits 3 on any raw-
  key hit in capture dir + unified log (defense in depth over wiretap2 masking).
  `GROK_DOGFOOD_CLEANUP=1` deletes capture + temp home.
- Transport note: it execs the binary with the caller's args — headless `-p` runs and
  any ACP stdio session (the caller drives the pipes) both work through it; it is NOT
  part of the automated harness paths (the runner has its own `AcpSession` +
  `Wiretap`; wstream copies the pattern).
- Verified live (probe KE-P2, §6): wirecap mode end-to-end GREEN on the release
  binary (turn exit 0, manifest clean, 0 redaction hits). Verified constraint: the
  default `GROK_DOGFOOD_BIN` = worktree `target/debug/grok-responses`, which was
  ABSENT on disk at write time — the override is required today.

### 1.11 gen-matrix (`smoke/redteam/gen-matrix.py`, 236 L)
- Build artifact generator for `cases/rt-m6.json` (the FULL live-catalog sweep,
  item-11 shape): fetches /v1/models (key env-only), drops embedding models, skips
  the live config's `disabled_models` (a disabled model's headless turn fails fast
  with NO model request — the 2026-09-17 SWEEP-1 run's 44 zero-wire rows were
  exactly that list); one row + standard wire asserts per remaining model; the path
  pin follows the LIVE CONFIG ROUTING AUTHORITY = the runner's 3-way resolver
  `api_backend_for_model` (run.py:1549: `[model."<id>"] api_backend` > `[endpoints]
  default_api_backend` > `"responses"`), fallback name heuristic (claude* →
  /v1/messages; gemini*/gemma* → /v1/chat/completions; else /v1/responses) with cc
  pins PREDICTED. Do-not-hand-edit: regenerate after catalog changes.

### 1.12 L0 Rust unit tier (in-crate)
- Precedents (fixture + PROVENANCE.md, `include_str!` loader convention):
  - `.74` `testdata/xw_orphan/` (4 error-body fixtures; PROVENANCE.md: file sha256:12
    + inner-message sha + PROVENANCE class LIVE-capture/SYNTHETIC-recipe/PREDICTION +
    status + hygiene sweep=0; known false-positive raw-key classes recorded, not
    redacted, per coordinator ruling) — results per review-gates-q3.md §3:
    sampling-types xw_orphan 9/9, sampler 1/1 + trigger-conditional pin, shell 1/1,
    compaction crate 132/132 (debug-profile targeted).
  - `.71` `src/conversation/fixtures/projection_x71/` (IN FLIGHT, untracked): 4
    byte-mirrors of live smoke/xwfix corpus cells (vxm-az + az-vlq pre/expected;
    PROVENANCE.md mint table: source sha256:12 == mirrored sha256:12, byte-verified;
    drift note: SDD "49 recs" era-stale vs live 53 = 7 T1 + 46 T0 — live governs,
    never edit a mirror to fit a RED) + 1 synthetic (`orphan_shape.json`,
    BackendToolCall x_search carrier) + KAT-pinned 12/12 id-grammar goldens
    (`xw_proj_id_grammar_canonical`). The cut itself: `projection.rs` 474 L +
    `projection_tests.rs` 592 L (cfg(test), wired in `conversation.rs` +5 mod lines;
    `provider.rs` +38 E2 pin `is_openai_family_empty_is_true_is_pinned`).
  - `.75` `testdata/affinity/`; `.76` `src/conversation/fixtures/xsearch_replay/`
    (in-tree precedent the .71 ruling cites for fixture placement = the crate test
    tree where the test runs).
- Canonical gate surface: the 6-pkg cargo gate + A6 name-by-name (§6 of the design
  doc; byte-identical command in plans/xwire/review-gates-q3.md §1).

## §2 — Transport split

### 2.1 By surface

| Surface | headless-ndjson | ACP | TUI | Notes |
|---|---|---|---|---|
| `run-smoke.sh` | YES | no | no | `--output-format json` |
| `responses_acceptance.rs` (L2) | no | YES (in-process MvpAgent) | no | protocol surface only |
| `run.py` (L3) | YES | YES | no | per-case `driver` — ONE schema across transports |
| `wstream` | YES (streaming-json only) | no | no | streaming frames via wiretap |
| `run-matrix.sh` | YES | no | no | no wire capture |
| `run-compaction-smoke.sh` | YES (`-p`/`-c`) | no | no | positional-prompt form = TUI (needs tty) — deliberately NOT used |
| `grok-dogfood.v2` | YES | reachable (args pass-through; caller drives `agent stdio` pipes) | reachable (bare `grok "prompt"` form) | instrumented operator launcher |
| L0 crate tests | n/a | n/a (in-process actor/mock server) | no | `run_agent_test_with_models` pattern |

- headless-ndjson-only: run-smoke.sh, run-matrix.sh, run-compaction-smoke.sh,
  wstream, and redteam cases with `driver: headless` (the default; rt-*, t21-*, most
  ws9-*).
- ACP-only: the in-tree L2 Rust tests; redteam cases with `driver: acp` (all xw-*,
  ws9-s06/s07 etc.).
- both: the redteam runner is the ONLY python surface with both transports behind one
  case schema.
- TUI: nothing in the automated suites drives the TUI (by design — the headless `-p`
  form is the acceptance surface; the positional-prompt TUI form needs a controlling
  terminal, run-compaction-smoke.sh header comment).

### 2.2 ACP dogfood launcher's role
`grok-dogfood.v2` is the human/forensic entry point: same binary, full debug scope by
default, optional wirecap, redaction-contract manifest. It is deliberately OUTSIDE the
automated harness path (the runner drives its own ACP stdio session + its own wiretap;
the README provenance rule for wstream — copy, don't import — keeps the lanes
decoupled). The L2 in-process Rust harness and the L3 AcpSession are the automated ACP
surfaces. Note the two DIFFERENT ACP model-switch surfaces: L2 in-process drives
`session/set_session_config_option` (config_id `model`), L3 drives
`session/set_model` (run.py:1193) — both land on `set_model_gated` server-side.

### 2.3 Wire capture (wiretap) plumbing per transport
- headless (runner): `Wiretap` (run.py:1255) starts wiretap2 on a free loopback port
  per case; the hermetic home's config surgery (`_apply_config_patch` :267 +
  `_align_models_cache` :424) points `models_base_url` at
  `http://127.0.0.1:<port>/v1`; captures → `<run>/wire/` (`req-NNN.json` +
  `resp-NNN.jsonl`).
- ACP (runner): identical — ONE wiretap per case; the ACP session is one binary
  process, so all its turns (including set_model re-resolution requests) ride the
  same capture dir; the wire hook at case end (run.py:3126-3165) filters by
  `body.model` to find the post-switch request.
- dogfood: the wrapper sed-rewrites every `base_url`/`models_base_url` line in the
  copied config.toml + mirrors/aligns `models_cache.json` (grok-dogfood.v2:127-150);
  wirecap port chosen free; capture under `~/.grok/dogfood/<TS>/wire/`.
- wstream: its own copy of the pattern (hermetic home + `derived-config.toml` +
  wiretap2), captures under `report/<ts>/<cell>/capture/`.
- `run-matrix.sh` and `run-smoke.sh`: NO wire capture (stderr / stdout evidence only)
  — a real gap for wire-shape claims (see §3).

### 2.4 The frozen-config.toml concept — where model knobs are set TODAY
Three frozen-config variants exist (none of them sets the strict/effort knobs):
1. `run-matrix.sh` per-case tempdir config — `[endpoints]` defaults (
   `default_api_backend=responses`, `default_context_window=256000`,
   `default_model_family=codex`, `default_agent_type=grok-build-plan`,
   `default_env_key=CODEX_LLM_PROXY_KEY`) + optional per-model `api_backend`
   override.
2. wstream `derived-config.toml` — live `~/.grok/config.toml` + ONE declared
   `api_backend` patch + wiretap base_url rewrite (per-run, in the report dir).
3. redteam `HermeticHome` — live config copy + case-level `config_patch` (dotted
   keys, `_apply_config_patch` run.py:267 — e.g. cases set
   `model/<id>/context_window` tiny to make compaction thresholds deterministic,
   `features/remote_compaction_v2=true` in rt-c5.json) + `--no-watch` + catalog
   cache alignment.
Where the four knobs live today (first-hand, `~/.grok/config.toml` @ write time,
309 L):
- `strict_responses_input`: ONLY per-model live-config rows — `gpt-5.6-sol`
  (=true, :16; terra/luna rows carry it too per crosswire-matrix §0 verdict 1);
  also per-model CATALOG rows in `models_cache.json` (e.g. false on the
  gpt-35-turbo-0301 row — observed in an r3r hermetic home copy). The harness never
  sets it.
- `context_window`: per-model rows (grok-4.6 =500000 :9; glm-5.2 =128000 :38) +
  `[endpoints] default_context_window = 256000` (:251) + case `config_patch`
  overrides.
- `model_family`: per-model rows (sol=codex :15, grok-4.6=xai :6, glm-5.2=glm :36,
  qwen3.8-27b=qwen :61); the live `[endpoints]` block (:247-252) has NO
  `default_model_family` (run-matrix.sh's frozen config adds `default_model_family
  = "codex"` — a harness-side default the live config lacks).
- effort (`reasoning_efforts` / `supports_reasoning_effort`): per-model rows (sol
  :21-27 low..ultra; glm-5.2 :41-52; qwen3.8-27b :63-74) + catalog rows. The
  deployed proxy additionally applies an xhigh→high effort clamp on the
  /messages→openai-class bridge for vLLM (M-1, apex-ayl.83; a proxy-image delta NOT
  in the stock litellm 1.90.0 checkout — see litellm-transform-matrix.md §1.3).
- Region pin: sol row `extra_headers.x-litellm-tags = "East US 2"` (:29-30) — the
  only per-row affinity lever today (D-ENC cross-region `cmp_` decrypt is the
  known gate, §1.9).

## §3 — Gaps against the unified goal

Goal (kiloecho mandate): multi-turn → mid-session model switch · /compact local +
remote-v2 · MCP tool calls · cross-model subagents (incl. spawn_agent vs
spawn_subagent richness) · cross-model resume. What exists (cited) vs what is missing.

### 3.1 Multi-turn → mid-session model switch — EXISTS (wire pin DORMANT)
- Exists: `switch_model` op (run.py:3032) drives `session/set_model` (ACP) or a
  resume-`-m` (headless) switch, scores the cell storage diff AT SWITCH TIME
  (`xwfix_cell_diff_storage` run.py:1679, before the post-switch turn can
  contaminate), then the wire hook at case end (run.py:3126-3165). 11 xw-* cases
  cover the corpus (R3 base: 7 RED-EXPECTED, 3 PASS guards, 1 UNPROVEN wall —
  71-handoff-brief §4).
- Missing: the wire-form byte pin is DORMANT — 0 `expected_wire.json` fixtures; the
  op hook hardcodes normalize `[]` (run.py:3154 per golden-engine doc §2 row 11) and
  first-by-filename selection (run.py:3152) with no `where`/`nth`/`any` from the case
  file; the declarative `kind=golden` path (run.py:2191) HAS full normalize +
  selection machinery but no switch_model case uses it (only golden-smoke-01, a
  zero-step mechanism case). Fixtures are minted .71 green-time (case-file change,
  no run.py hunk needed for the pin itself).

### 3.2 /compact local + remote-v2 — PARTIAL
- Exists: `compact` op (run.py:3068) on BOTH drivers (ACP prompt `/compact`;
  headless `/compact` turn). Local: rt-c1..c4 (over-capacity pin rt-c3 = the
  01a09be2 class), ws9-s05 (2-turn + compact + resume pins, context_window 32768
  patch). Remote-v2: rt-c5.json (sol, `recon: true`, `config_patch:
  features/remote_compaction_v2=true` + context_window 256000 — D-ENC RECON by
  design, records accept-or-400) and run-compaction-smoke.sh (sol 3-turn, v2 log
  greps, D-ENC t3 gate).
- Missing: no CROSS-MODEL remote-v2 cell (v2 path is codex-dialect-gated:
  `responses_wire_dialect_for_model_family == Codex && api_backend == Responses`,
  client.rs:2625 per wire-topology-matrix §0 verdict 2 — so only the codex-family
  rows can take it; no cell pins the v2 RESPONSE BODY — the remote v2 stream is
  asserted via stderr log greps (run-compaction-smoke.sh) or recon wire greps
  (rt-c5), never a body/frame pin); no ACP-driver `/compact` case in the R3
  governing set (compact cases are headless); the `/v1/responses/compact` route is
  blocked by the proxy route allowlist (403, probe P3 in wire-topology-matrix §3 —
  .78 NF-3/S05 owns).

### 3.3 MCP tool calls — PARTIAL (prompt-driven only)
- Exists: the t21 family (5 cells, one per live MCP-capable model row: responses
  qwen3.8-27b/glm-5.2/grok-4.6/sol + messages sonnet-5) — prompt-driven MCP via the
  `use_tool` search path: `mcp_calls` declarations (server/tool/expect_name/
  `via: search_tool_use_tool`/args/premise/output_contains) become (a) premise pins
  in `scoring.vacuous_if` and (b) synthesized wire-grep `output_contains` pins
  (`synth_output_pins` run.py:1593); discriminating-form wire greps (escaped
  `tool_name`/FQ-name/echo-command needles) + `function_call_output` shape pin +
  `store=false` invariant + `resp_status` evidence. `tool_calls` declarations
  (e.g. `run_terminal_command echo` round-trips) work the same way.
- Missing: no dedicated `mcp_call` STEP op (MCP is exercised through `turn`
  prompts — model-cooperation, premise-gated); no MCP case on the ACP driver (t21 =
  headless); no MCP-across-switch case (server advertised pre-switch, tool invoked
  post-switch); no MCP × subagent case (child spawned under a different model
  calling the parent-side MCP server). The triage signature rows #1-#4 (MCP
  spawn/handshake/timeout classes, OPEN) are the standing failure-shape registry.

### 3.4 Cross-model subagents (spawn_agent vs spawn_subagent richness) — PARTIAL
- Exists: v2 path proven live: ws9-s06 (ACP, qwen parent → `spawn_agent` a
  claude-sonnet-5 CHILD on the messages wire, mailbox `send_message` round-trips
  PART1/PART2, `followup_task` reuse, `wait_agent` gates; scored FAIL surface =
  wire shape — opaque `agent_message` carrier = the designed defect; model-
  cooperation pins as VACUOUS premises); rt-m11/m11r lineage (the RT-M11
  discipline ws9-s06 inherits); v1 path: rt-s1 (headless, v1 task-tool child with
  explicit `model` arg, parent near the compact threshold), run-smoke `c-subagent`
  (echo child in isolation).
- Missing: no RICHNESS-COMPARISON case — no cell that enumerates which of
  `spawn_agent`/`send_message`/`followup_task`/`wait_agent` (v2) vs the v1 task
  tool are ADVERTISED per model/wire/family (`ndjson.tools_absent/tools_present`,
  run.py:1401, is the right primitive and exists — RT-S2's tools-dark pin used the
  pattern pre-MA-3; no current case pins the v2 tool surface per row); no
  subagent spawned under a switched model (spawn AFTER switch_model); no
  cross-pool subagent case (child model from a different pool/family than the
  parent's, beyond ws9-s06's single qwen→claude pair); no subagent × compact
  interaction case (child completes while parent compacts — rt-s1 covers the
  threshold vicinity but not the mid-compact child window).

### 3.5 Cross-model resume — PARTIAL (same-wire only)
- Exists: headless resume is IMPLICIT (each `run_headless_turn` carries
  `ctx.session_id`; there is no explicit `resume` step op); rt-r1 (kill -9 mid-
  session + resume + 2 turns), rt-r2 (resume with `-m gpt-5.6-sol` — cross-MODEL
  but SAME wire responses→responses; carries `expected_red: REPLAY-1`, a
  pre-registered product-side RED: deterministic Azure 400 `array too long`),
  rt-r3 (long-idle resume, 024ef68 auth-refresh regression pin), ws9-s11 (legacy
  resume). ACP: LOAD-FIRST `session/load` entry (run.py:877-897) is the resume
  primitive for cell-seeded sessions.
- Missing: no CROSS-WIRE resume case (a messages-wire history resumed under a
  responses model or vice versa — the cross-wire projection at RESUME time is
  untested; the switch op covers mid-session, not process-death + re-entry); no
  switch→kill→resume composition (mid-session switch, then death, then resume on
  the NEW model); no resume-after-remote-v2-compact case (carrier replay across a
  process boundary on the D-ENC-gated path).

### 3.6 Response-side pin surface — MISSING (the engine gap)
- `resp_status` (run.py:2086) reads the HTTP status only (pairs req→resp by capture
  `n`; `status_in` whitelist; evidence-only or assertive). wiretap2 ALREADY captures
  `resp-NNN.jsonl` with full frame fidelity (status line + one line per SSE frame),
  but NO assert kind reads response bodies or frame sequences — the golden engine
  covers request `.body` only (golden-engine doc §1 "What golden_compare covers vs
  does NOT": Response body / stream frames = NO). Design: `unified-harness-design-
  20260918.md` §3.

### 3.7 Layer gaps (harness-structural)
- No no-proxy binary layer: `main()` fatals without `CODEX_LLM_PROXY_KEY`
  (run.py:4918 block, "FATAL: CODEX_LLM_PROXY_KEY not set (env-only, never on
  disk)") — even a zero-live-call mechanism case (golden-smoke-01) needs the key +
  the binary. The offline lane is unit-level only (`--selftest` + L0 crate tests).
- 5 python suites + 3 zsh runners, no committed unified gate in the main repo
  (71-handoff-brief §6 "Gaps (kiloecho owns, not .71)"). Design: unified-harness-
  design-20260918.md §1/§6.
- Redaction sweep hits do not flip the process exit code (run.py:5097-5099 — exit
  keys off FAIL/BLOCKED rows only); report-r3r's report.md logged "redaction
  sweep: 10 hit(s)" without failing the run — the sweep is a log, the gate must
  enforce (design §6 treats it as a hard wave criterion).
- `run-matrix.sh`/`run-smoke.sh` produce no wire evidence — wire-shape claims from
  them rest on stderr greps (design §5 CI shape excludes them from wire gates).

## §4 — Intel Registry (bounded plans/ sweep)

Sweep scope (this seat's alone): `ls` + `wc -l` of every `*.md` under
`~/Projects/upstream/grok/plans/` (top + all subdirs); FULL reads limited to the
six READ-FIRST docs; per-file status assigned only where the evidence is
mechanical (HEAD-cite grep) — everything else is UNREAD-status, not guessed.
Inventory at write time (2026-09-18 23:2xZ):

| dir | md files | total lines |
|---|---|---|
| plans/ (top) | 225 | 48109 |
| plans/xwire/ | 36 | 7878 |
| plans/provenance/ | 24 | 5860 |
| plans/briefs/ | 19 | 1234 |
| plans/reviews/ | 13 | 1735 |
| plans/xwavec71/ | 3 | 528 |
| plans/audit/ | 2 | 172 |
| plans/matrix/ | 1 | 395 |
| plans/citations/ | 1 | 118 |
| plans/pins/ | 0 (7 json) | — |
| plans/c21c22/ | 0 (schema dir) | — |
| **total** | **324** | **66029** |

Top-level family split (name-pattern census of the 225): 102 per-arc session docs
(review/ratify/concord/rca/findings/driver/preplan/recon/task-review), 35 sdd, 27
other, 18 reports, 12 research (recon series research-01..11 + research-11-concord),
12 plan/runbook/handoff, 10 specs, 8 audit/census/registry, 1 main ledger.

### 4.1 The six READ-FIRST docs — full-read status

| doc | read | verdict for the map |
|---|---|---|
| `plans/smoke-harness-spec.md` (91 L) | FULL | harness layering spec (L1 CLI + L2 ACP v1 FINAL); extension contract P2.x (compaction v2 long-context scenario, multi-agent fan-out, messages subagent) — the layered model this map inventories |
| `plans/HT-1-redteam-harness-spec.md` (119 L) | FULL | redteam suite spec (A1 wiretap2 / A2 run.py / A3 cases / A4 report / A5 dogfood v2); the RT-C/RT-M/RT-R/RT-S matrix (IDs stable, append-never-renumber); case-file contract + assertion engine spec; acceptance criteria incl. redaction grep + canonical-gate-untouched |
| `plans/xwire/golden-engine-capability-glm-20260918.md` (451 L) | FULL (§1 table + §2 verdicts + §3-§5) | the engine capability map: request-body pin YES (golden_compare 1822), resp+stream NO, storage separate (xwfix_cell_diff_storage 1679, Python ==); wire-form pin DORMANT (0 expected_wire.json; op hook normalize `[]` + first-by-filename); normalize N/M loudness declarative-path-only; §3 promotion protocol (capture→fixture: explicit-nulls convention, sha256:12 pin, PROVENANCE.md per xw_orphan pattern); §5 gap list (9 gaps, 1 blocking for .71) |
| `plans/.../kiloecho/README.md` (in-repo) | FULL | conventions + registry + layering ruling (below) |
| `smoke/redteam/kiloecho/71-handoff-brief-20260918.md` (168 L) | FULL | .71 handoff contract: world state @0fc1060 + exfil + binary-of-record f0f2455a6ca2; live cut state; protocol steps 1-7 incl. **§3.6 L2 re-verify** (fresh release build + sha12 record + R3 11-cell corpus re-run, python3.12, per-cell `--out`; expected 7 RED-EXPECTED clear / 3 PASS guards hold / vlq-vlg wall UNCHANGED / zero COMP-3 storms c2≤18 / zero acp_error; recon sweeps) + R3 evidence base §4 + re-pin table §5 + regression-prevention assessment §6 (layers + kiloecho-owned gaps) + constraints §7 (python3.12 mandate, STOP rules) |

### 4.2 Registry table (doc → topic → canonical-or-superseded-by → status)

Status rule: **LIVE** = cites HEAD 0fc1060 (grep-verified this sweep) or is a
structurally-always-live SoT; **STALE-BY-DRIFT** = HEAD-cite demonstrably predates
0fc1060 (mechanical, per README drift rule); **UNREAD** = not opened this sweep —
status NOT assigned (no guessing). 0fc1060-citing docs (LIVE by rule): xwavec71/
w2-exfil-concord-glm-20260918.md, xwavec71/e0716-adjudication-20260918.md,
xwire/concord-overwatch-glm-20260918.md, xwire/tdd71-green-report-qwen-20260918.md,
audit/sequential-landed-audit-20260918.md, audit/sequential-landed-audit-concord-glm.
md, ledger.md. SHA-cite prevalence (grep -rl, this sweep): 0fc1060 ×7 files ·
d404fe1 ×8 · 5f4411a ×11 · 40ffad1 ×32 · 7c82fe7 ×12 · ef5192b ×23. Files touched
today: 42 md (of 324) — the corpus is actively written; per-arc docs are
session-scoped by construction.

| doc (family) | topic | canonical-or-superseded-by | status |
|---|---|---|---|
| `ledger.md` (14k+ L, append-only) | campaign event ledger | LIVE SoT for campaign state (kiloecho README layering ruling); NOTHING supersedes it — it absorbs | LIVE |
| `plans-census-20260915.md` + `-concord-glm-20260915.md` | plans/ census snapshot (2026-09-15) | superseded by newer per-arc docs as they land; the census method stands | STALE-BY-DRIFT (date-stamped snapshot; predates 0fc1060 arc) |
| `research-01..11*.md` (12) | recon series (forks/seams, xli port, harness intel, messages wire, v2-multiagent, runtime profile, design-doc seam mining, runtime audit, compaction truth, model catalog gate, tool-schema caching) | per-recon SoT until a later wave doc closes the question | UNREAD (topic-identified by name; no per-file status) |
| `*-spec.md` (10, incl. smoke-harness, HT-1, P1/P2.1/P2.0/MW-1..3/R0/item9-v2-multiagent) | wave/harness specs | specs are binding per their status lines; HT-1 + smoke-harness = FULL-read (above) | UNREAD except the two FULL reads |
| per-arc `sdd-`/`tdd-`/`-ratify-`/`-review-`/`-concord*`/`-rca-`/`-report` (102+35+18) | session-scoped design/review/close docs for individual beads (apex-ayl.NN arcs) | superseded by their own close reports + the ledger close entries; per-lane SoT until then | UNREAD except where cited by FULL-read docs (sdd-71-projector.md cited by the brief; review-gates-q3.md cited for the 6-pkg gate + A6) |
| `xwire/` (36 md) | xwire lane SoT (cross-wire matrix, TDDs .69/.71/.72/.74/.81/.82, review-gates q2/q3/q4, W2 runbook, redcycle, promotion package, r1/r2/r3 concord+reports) | NOT RETIRED (layering ruling); `crosswire-matrix.md` = living wire-topology SoT (primary source for the glm matrix doc); `review-gates-q3.md` = current-wave gate SoT; `golden-engine-capability-glm-20260918.md` = engine capability SoT (FULL read) | mixed: 3 docs LIVE (0fc1060-citing, listed above); rest UNREAD except cited docs |
| `provenance/` (24 md + fixtures/ + scripts/) | supply-chain audit layer (donors, parity, deliverable map, wiretopology recon set, trinity, turnstate-affinity, proxy-capability-matrix, recon-plan) | NOT RETIRED (layering ruling) — different mission from xwire (audit, not per-lane design) | UNREAD except `deliverable-map-20260918.md` (cited as SoT by README registry) |
| `provenance/ledger.md` (469 L) | **PROVENANCE-DEEP-RECON phase ledger (phase owner: coordinator, opened 2026-09-17) — a phase-scoped ledger, not a duplicate of the main ledger** | the main `ledger.md` remains the campaign SoT; this file scopes to the provenance-deep-recon phase | note recorded verbatim per tasking (not re-litigated) |
| `briefs/` (19 md: CDX-1, HT-1(.1), MA-1/MA-2, MW-1..4, P1, P2.0/P2.1, R0, R1, smoke-L2) | task briefs + task-review briefs | historical (briefs are launch-time snapshots; closed by the arc reports) | UNREAD (topic by name) |
| `audit/` (2) | sequential-landed audit (this arc) | — | LIVE (both 0fc1060-citing) |
| `citations/citations-registry.md` (118 L) | citation registry | SoT for citation discipline | UNREAD |
| `matrix/model-matrix-20260912.md` (395 L) | model matrix (2026-09-12) | superseded by live catalog + crosswire-matrix for topology; the matrix itself is era-stale (pre-dates several re-pins) | STALE-BY-DRIFT (date-stamped) |
| `reviews/` (13) | review pass notes | per-review SoT | UNREAD |
| `xwavec71/` (3 md + projection_tests.rs + fixtures/ + w2-capture/) | the .71 WAVE-C cut record (adjudication, exfil concord, red-prep) | — | LIVE (2 of 3 0fc1060-citing; red-prep = pre-cut record) |

**Layering ruling (recorded verbatim, do not re-litigate; source = kiloecho
README "Layering ruling" + ledger 22:4xZ entry):** pointer-never-copy; xwire/ and
provenance/ are NOT retired (per-lane SoT + supply-chain audit layer); kiloecho =
consolidated living facts + pointers.

## §5 — Live probes (bounded, authorized 2026-09-18 22:4xZ; key sha12 9f3f56a263da, env-only)

| probe | purpose | command (abbrev) | key observation | exit |
|---|---|---|---|---|
| KE-P1 (22:5xZ) | ACP dogfood launcher wiring (no proxy call) | `GROK_DOGFOOD_BIN=<worktree>/target/release/grok-responses zsh smoke/redteam/grok-dogfood.v2 --help` | launcher execs the record binary (sha12 f0f2455a6ca2); binary answers `--help`; the launcher's DEFAULT BIN (debug build) is absent on disk → override required today | 0 |
| KE-P2 (22:52Z) | dogfood wirecap path end-to-end (1 model call, qwen3.8-27b) | `GROK_DOGFOOD_WIRECAP=1 GROK_DOGFOOD_CLEANUP=1 grok-dogfood.v2 -m qwen3.8-27b -p "Reply with exactly: KE-P2-OK" --output-format json --always-approve` | GREEN: tempdir home + wiretap :52390 + base_url rewrite worked; turn exit 0; manifest printed (key sha12 9f3f56a263da, value never printed); 0 redaction hits (no REDACTION VIOLATION line); cleanup deleted capture (default retention = keep) | 0 |
| KE-P3 (22:5xZ) | ACP stdio transport (the L3 AcpSession path, 1 model call) | tempdir GROK_HOME (P1 `[endpoints]` config) + `bin agent stdio`; python3.12 client: initialize → session/new (`_meta.modelId=qwen3.8-27b`) → session/prompt | GREEN: initialize result `protocolVersion:1`, `agentCapabilities.loadSession: True` (backs the load-first entry path); session/new minted a sid; prompt completed `stop=end_turn`; NOTE: the minimal probe client collected empty chunk text (the full AcpSession parser in run.py is the proven text path — this probe is transport evidence, not content evidence) | 0 |

Budget used: 3 of 5 authorized probes. Kept in reserve: none needed — the map is
complete without a live ACP `/compact` or switch probe (both paths are evidenced by
the R3 corpus + the in-tree engine code).

## Footer
- Raw-key sweep of THIS doc: 0 (sha12 references only).
- Read-only discipline honored: no edits to any pre-existing file; only this file
  (and the companion design doc) created under `smoke/redteam/kiloecho/`.
- Next: `unified-harness-design-20260918.md` (same bead, same seat) consumes this map.
