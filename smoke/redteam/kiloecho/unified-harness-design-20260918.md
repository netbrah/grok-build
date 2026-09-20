# Unified harness design — one case registry, layered gate, ndjson + ACP

> **SUPERSEDED-WHERE-CONFLICTING (2026-09-19 11:55Z, coordinator):** this inventory is first-hand as of 2026-09-18 (PRE cut-0.5). State of record: grok/plans/smoke-gate-unification-design-20260919.md (upstream root) §6 post-sweep errata + grok/plans/provenance/ledger.md entries 2026-09-19 11:45Z/11:55Z. Known-stale classes: run.py line numbers (cut-0.5 rewrites), case counts (61 -> 74), long-run launch patterns (sweepctl daemon mode + terminal-line registry is the driver of record; nohup/PTY script house rule retired into code).


Date: 2026-09-18 (UTC; written after 23:3xZ)
Author/seat: kiloecho_harness_qwen (qwen work seat, KE-1, apex-v2-grok-build campaign)
Bead: `apex-bqm` (HARNESS-UNIFY-1)
Sources: `harness-map-20260918.md` (same dir — the map this design argues from,
all path:line cites therein are @ HEAD **0fc1060**, first-hand);
`smoke/redteam/kiloecho/71-handoff-brief-20260918.md` (FULL read; §3.6 L2 re-verify
protocol subsumed in §6); `plans/xwire/review-gates-q3.md` §1-§2 (6-pkg gate + A6);
`plans/xwire/golden-engine-capability-glm-20260918.md` (engine gaps); siblings
REFERENCED, not duplicated: `wire-topology-matrix-20260918.md` (§3 ratchet verdict),
`error-class-audit-20260918.md` (gap list W1-W14), `adversarial-brief-20260918.md`
(case shapes for .72 sweep), `litellm-transform-matrix.md` (§0/§1.3 version-pinning:
ratchet key = (litellm version, proxy-branch, config digest); §4.9 pending in that
doc — cited as pending, not re-derived here). Key sha12 `9f3f56a263da` (env-only).
Status: **DRAFT**

## §1 — Recommendation: layered gate, ONE case registry (position + argument)

**Position.** NOT "single unified harness" in the monolithic sense, and NOT the status
quo of six independent suites. The recommendation is a **layered gate (L0 crate-unit /
L1 local-hermetic / L2 live-single / L3 matrix-sweep) executed over ONE case registry
(the `smoke/redteam/case.schema.json` + `manifest.json` pair) with ONE verdict engine
(`run.py`), where each layer is a strict subset of the layers above it — a case that
passes at layer N is the SAME case file, re-run at layer N+1 with a live binary +
proxy.**

Argument from the map (harness-map §1-§3):

1. **The engine is already unified where it matters.** `run.py` (5109 L) already
   drives BOTH transports behind ONE schema (per-case `driver: headless|acp`,
   case.schema.json; AcpSession run.py:813 vs run_headless_turn run.py:676), already
   carries the verdict semantics (finalize_verdict run.py:2491: premise pass →
   BLOCKED/VACUOUS; tolerant → FINDING-PASS; require_wire_evidence → NO-EVIDENCE),
   the golden compare engine (golden_compare run.py:1822 + storage diff
   xwfix_cell_diff_storage run.py:1679), expected_red (run.py:2669), the campaign
   seal/aggregate machinery (seal_campaign run.py:4478 / aggregate_summary
   run.py:4646), and the R3 governing base (report-r3r/, 11 cells, 11/11 seals
   clean). A "new unified harness" that rewrote this would discard proven machinery
   for the sake of a label. The fragmentation is in the SURROUNDING suites, not the
   engine.

2. **The fragmentation is real and named.** 71-handoff-brief §6: "5 python suites +
   3 zsh runners, no committed unified gate in the main repo." Each surrounding
   suite (run-smoke.sh, run-matrix.sh, run-compaction-smoke.sh, wstream, triage,
   dogfood) has its own invocation, verdict vocabulary, and evidence layout — and
   three of them (run-smoke/run-matrix/run-compaction) produce NO wire evidence at
   all, so their PASSes cannot support wire-shape claims (harness-map §3.7). The
   fix is promotion INTO the registry (their scenarios become case files) and
   demotion to convenience wrappers (they may remain for one-command ergonomics,
   but their verdicts are no longer gate-grade).

3. **The layers already exist in practice — formalize, don't invent.** L0 = the
   in-crate Rust unit tier (xw_orphan / projection_x71 / xsearch_replay / affinity
   fixtures + `include_str!` KATs, canonical 6-pkg gate + A6 name-by-name —
   review-gates-q3.md §1-§2). L1 = offline/hermetic (the `--selftest` lane: schema
   dual-engine + case-contract + 73+19 unit tests + wiretap `--selftest`; plus the
   pre-placement mechanism-case pattern, run.py:2713-2740, which already runs
   zero-live-call cases against pre-placed captures). L2 = live-single (ONE cell,
   per-cell `--out`, sealed campaign, verdict.json — the exact r3r layout; the .71
   §3.6 re-verify is L2). L3 = matrix-sweep (full manifest roster, budget,
   campaign seal + aggregate, recon sweeps). The design work is (a) making L1
   key-free for hermetic-binary cases, (b) adding the missing case schema ops (§2),
   (c) adding the response-side pin surface (§3), (d) defining the mint protocol
   between L2/L3 and L0 (§4), and (e) writing the gate as ONE wave (§6).

4. **Why not collapse to one live matrix (no layers)?** Cost + flake surface: the
   matrix is the most expensive surface (R3 = 15 calls for 11 cells; the full 61-
   cell roster at est_calls 3-12 each is ~150-300 calls) and the flakiest (env
   flaps, COMP-3 storm bands, model temperament → VACUOUS). L0/L1 exist precisely
   to catch 90% of regression classes (projection shape, error-body classification,
   canonicalization parity, schema drift) with zero proxy spend; the .71/.74 TDD
   arcs already proved the discipline (byte-mirrored fixtures, PROVENANCE.md,
   right-reason RED, STOP rules). Collapsing the layers would make every schema or
   projection change cost live calls.

5. **Why keep the python runner (not a Rust port)?** HT-1 spec §1: "Rust-free by
   design (house rule pressure)". The runner is stdlib-only python3.12 (no pip
   deps), drives the binary as a black box (headless CLI + ACP stdio), and the
   campaign/aggregate/seal machinery is proven. Rust belongs in L0 (where the
   product under test lives); the harness orchestration stays python.

**Shape of the recommendation (one line):** the gate is `L0 (cargo 6-pkg + A6 +
L0 fixture KATs) → L1 (offline: selftest + hermetic-binary mechanism cases) → L2
(live-single: governing cells, per-cell --out) → L3 (live matrix: manifest roster,
sealed campaign, recon sweeps)` — every layer consumes the SAME case registry and
the SAME verdict engine; the layer only changes what is stubbed (L0: nothing, pure
functions; L1: the proxy; L2/L3: nothing — real proxy, real binary).

## §2 — Unified runner design (one case schema across ndjson + ACP)

Principle: **extend the existing schema and op set additively; never fork the
runner.** Every addition below reuses a named existing mechanism (cited) and adds
a schema enum value + one dispatch branch + offline tests in `test_run.py`.

### 2.1 Case schema v2 — the unified surface

Existing (keep, cite = case.schema.json + run.py):
- `driver: headless|acp` (run.py:813 / :676) — the transport declaration. A case
  that must pass on BOTH transports is authored as TWO case files (same id stem,
  `…-h` / `…-a`) — do NOT add a `driver: both` mode: the transports have different
  failure surfaces (ACP notification stream vs NDJSON events) and different
  switch surfaces (`session/set_model` run.py:1193 vs resume-`-m`), so one case
  per transport keeps the verdict attribution clean (the R3 corpus is already
  ACP-only for a reason: the switch cells need the in-session set_model path).
- `output_format: streaming-json|streaming-messages-json|json` (headless only).
- `steps[]` multi-turn (run.py:2908-3104): `turn` (with per-step `model`,
  `timeout_s`, `kill_after_s`, `expect_kill`), `kill` (mid-turn SIGKILL,
  `after_s`), `idle`, `recon_note`, `switch`, `switch_model`, `compact`.
- `rows[]`/`row_asserts` row-based matrices (rt-m5/m6 pattern;
  `api_backend_for_model` run.py:1549 = the 3-way routing authority).
- `seed{hist_tokens,hist_bytes}` synthetic history (run.py:484) + xwfix cell seed
  (run.py:1643) — the determinism tricks.
- `scoring.vacuous_if[{pin,class}]` + `require_wire_evidence` + `tolerant` +
  `expected_red{id,reason}` + `retry{on_status,max_attempts,backoff_s,
  recheck_models}` (run.py:2491/2362/2614/2669).
- `mcp_calls[]` / `tool_calls[]` declarations (premise pins + synthesized
  `output_contains` wire pins, `synth_output_pins` run.py:1593).
- `env{home,launcher,store}` + `config_patch` + `watchdog_s` + `est_calls` +
  `tier slim|full` + `suite` enum.

Additions (the design delta):

1. **Explicit `resume` step op** (headless + ACP). Today resume is IMPLICIT
   (headless carries `ctx.session_id` across turns, run.py:2976-2983; ACP is
   load-first entry run.py:877-897). An explicit op makes resume-a-testable-action:
   `{"op":"resume","driver":<headless|acp>,"sid":<optional — default = current>}`.
   Headless: a fresh `run_headless_turn` with `--resume <sid>` (the binary form
   already used — the op is a semantic marker + evidence anchor: the runner emits
   an `acp_session_load`-style event so ndjson pins can target "post-resume").
   ACP: `session/load` on the CURRENT sid (kill + re-load within one case) or a
   pre-minted sid (the existing load-first path, now reachable mid-case). This is
   what cross-model resume cases (harness-map §3.5) need: switch → kill → resume
   on the new model, with the resume boundary as a scored surface.
2. **`switch_model` extension fields** (back-compat, default-off):
   `wire_normalize: [paths]` and `wire_select: {where, nth, any}` — threaded into
   the case-end wire hook (run.py:3126-3165) so the op path stops hardcoding
   `[]` + first-by-filename (golden-engine doc §2 gaps #2/#3; the declarative
   `kind=golden` path at run.py:2191 already proves the mechanics —
   `_wire_filter` :1497 + `_nth_select` :1534 + per-assert normalize :2241).
   Until the hunk lands, cases use the declarative workaround (assert.wire
   `kind: golden` with `where`/`nth`/`normalize` + companion `count`) — zero
   runner change, already schema-legal.
3. **`compact` op extension**: `{"op":"compact","path":"local"|"remote-v2",
   "d_enc":true}`. `path: remote-v2` requires `config_patch:
   features/remote_compaction_v2=true` (the rt-c5.json shape, now declarative) +
   the codex-dialect gate pre-check (only codex-family rows can take the v2 path —
   wire-topology-matrix §0 verdict 2: `responses_wire_dialect_for_model_family ==
   Codex && api_backend == Responses`, client.rs:2625; a non-codex row with
   `path: remote-v2` FAIL-CLOSES as BLOCKED-harness rather than silently
   self-summarizing). `d_enc: true` marks the expected D-ENC failure class (the
   t3 "history incompatible with the current model" shape — the pin is a
   RECON-class wire grep, not a hard pin, until proxy session affinity lands;
   run-compaction-smoke.sh header = the D-ENC record). The v2 RESPONSE BODY pin
   uses the §3 stream-frame kind (the v2 stream is the only remote-v2 evidence
   worth pinning).
4. **`mcp_call` step op** (harness-map §3.3 gap): the prompt-driven t21 pattern
   STAYS the model-cooperation surface (that is the point — does the MODEL call
   the tool), but the op adds the harness prelude + a dedicated evidence anchor:
   `{"op":"mcp_call","id":"<mcp_calls id>","verify":"advertisance"}` — the
   runner asserts at op time that the server's turn-boundary announcement is on
   the wire (the `cg_ann` harness-premise pin of t21 becomes a hard op gate
   instead of a post-hoc premise), so a model-temperament VACUOUS is never
   confused with a rig failure (the t21 premise design already separates
   harness-vs-model class — the op just moves the harness class to a checkpoint).
   ACP-driver MCP cases become first-class (same op, driver: acp).
5. **`subagent` step op** (harness-map §3.4 gap):
   `{"op":"subagent","via":"spawn_agent"|"spawn_subagent","name":"<child>",
   "model":"<child-model>","task":"<prompt>","wait":true}`. The op is a
   PROMPT-CONSTRUCTING step (the tool call itself is model-cooperation — the
   runner builds the ws9-s06-style prompt from the fields, keeps
   `tool_calls`/premise pins as the cooperation contract, and adds the
   `subagent_finished`/child-wire evidence anchors: the child's model+wire is
   asserted via `where: {body.model:<child-model>}` on a DIFFERENT path than the
   parent (the ws9-s06 wire-shape pins: child ran on its OWN model+wire).
   `model` at spawn = the cross-pool declaration; the richness-comparison case
   (harness-map §3.4) is then a ROWS case over model×wire rows with
   `ndjson.tools_present/tools_absent` (run.py:1401) pinning which of
   spawn_agent/send_message/followup_task/wait_agent (v2) vs the v1 task tool is
   ADVERTISED per row — the RT-S2 tools-dark pattern, generalized to a matrix.
6. **`wire` field promotion**: the top-level `wire: responses|messages|
   chat_completions` (already in schema, used by t21) becomes the ROUTING-
   EXPECTATION: when set, the runner auto-adds the path pin
   (`wire_path_for_model` run.py:1586) as a harness-class premise — a case that
   declares `wire: messages` and whose request hits `/v1/responses` is
   BLOCKED-harness, not a model finding.
7. **L1 key-free hermetic mode**: `env.proxy: "none"` (new) + `--offline-bin`
   (new runner flag): the runner runs hermetic cases whose wire inputs are
   pre-placed (the run.py:2713-2740 copy-if-absent mechanism, generalized: a
   case with `env.proxy: "none"` MUST have `cases/<stem>/wire/` pre-placed
   inputs and MUST declare zero live ops — schema gate rejects a live op in a
   proxy:none case) with the binary pointed at a DEAD local port (the binary's
   own request failures are the evidence; no proxy key is read, so
   `main()`'s key check (run.py:4918 block) is skipped for proxy:none runs).
   This closes harness-map §3.7 gap 1 (no no-proxy binary layer) and makes L1 a
   real binary-level layer instead of unit-only.

### 2.2 Reuse map (what the unified runner does NOT rebuild)

| Concern | Reused function (run.py @0fc1060) | New code |
|---|---|---|
| Request-body byte pin | `golden_compare` 1822 (+ canon 1716, null_path 1725, field_diff 1781, write_diff 1884) | none (op-hook thread-through is a 2-line hunk, §2.1.2) |
| Storage-form pin | `xwfix_cell_diff_storage` 1679 | none |
| Wire asserts | `check_wire` 1898 (field/grep/size_lt/count/resp_status/golden/recon) | 2 new kinds (§3) |
| NDJSON asserts | `check_ndjson` 1355 (count/absent/present/eq/ne/text_contains/tools_absent/tools_present/recon) | none |
| Artifact asserts | `check_artifact` 1435 | none |
| Recon (non-scoring evidence) | `check_recon` 2252 + `recon_note` op | none |
| Verdict | `finalize_verdict` 2491 (premises/tolerant/evidence) | none |
| expected_red | mark() 2669 + manifest registration | none |
| Retry/FLAKY | `run_case` 2614 + `_retry_decision` 2451 + `recheck_models_available` 2422 | none |
| Campaign seal/aggregate | `seal_campaign` 4478 / `validate_campaign` 4555 / `aggregate_summary` 4646 / `_evidence_grade` 4633 | none |
| Redaction | `redaction_sweep` 3515 | gate enforcement (§6: hits FAIL the wave) |
| ACP transport | `AcpSession` 813 (initialize/new/load/prompt/set_model/close) | none (resume op reuses load) |
| Headless transport | `run_headless_turn` 676 | none (resume op reuses `--resume`) |
| Hermetic home | `HermeticHome` 368 + `_apply_config_patch` 267 + `_apply_no_watch` 343 + `_align_models_cache` 424 | none (proxy:none variant) |
| Capture | `Wiretap` 1255 → wiretap2 610 L | none (resp-side READER is new, §3) |
| Seed | `seed_history` 484 / `xwfix_seed_from_cell` 1643 | none |
| Session evidence copy | `_copy_session_evidence` 3221 (keeps chat_history/summary/events/unified/compaction*/subagents + acp.log) | none |
| Triage block | `_triage_block` 3375 → verdict.json `triage` | none |

### 2.3 Per-cell `--out` discipline (the r3 lesson)
R3's governing layout (report-r3r/, verified): EACH cell runs into its OWN root
(`--out <campaign>/<cell>/`) so each cell gets its own sealed `campaign.json`
(canonical digest, `started_utc` excluded → stable sha for identical inputs) +
nested `<cell>/<cell>/verdict.json`; a shared root would let a re-run OVERWRITE
the sibling cell's seal (the "root-seal overwrite lesson" named in 71-handoff-
brief §3.6). The unified gate MANDATES this: L2/L3 wave runs are always
`python3.12 smoke/redteam/run.py <case> --bin <binary> --out <WAVE-ROOT>/<cell>/
--campaign-id <bead>-<wave> --wave <label> --mode adhoc`, followed by the
OFFLINE aggregate lane (`--aggregate --campaign-dir <WAVE-ROOT>` — no key, no
binary, no network) which validates every cell dir (verdict.json present,
campaign.json sealed, evidence_index non-empty for PASS cells) and writes
`summary.json`/`summary.md`. A wave is only admissible if the aggregate lane
passes; the aggregate is the gate artifact (it is reproducible from the sealed
dirs alone — that is the audit property).

### 2.4 Campaign/manifest flow
1. **Author**: case file (schema-gated) + `manifest.json` cell entry (family,
   status note; `expected_red` registration for RED-EXPECTED cells — the manifest
   is where RED expectations are ADVERTISED, run.py renders them).
2. **Seal**: at run start `seal_campaign` (4478) — byte-exact runbook copy
   (`--runbook`) + `campaign.json` with meta: git_head, bin_sha256_12,
   key_sha256_12 (env-only, value never written), upstream, config_sha256 (the
   hermetic-home source config digest — frozen-config provenance), protocol_
   constants.
3. **Run**: per-cell `--out` (§2.3); retry loop per `retry` block; verdict.json
   per cell (4633 evidence grade; 3433 triage block).
4. **Aggregate**: `--aggregate` (4646): per-cell final-attempt outcome
   (FAIL > BLOCKED > TIMEOUT > PASS), stability (FLAKY > STABLE > NOT_ASSESSED),
   campaign = worst case; `incomplete` = PASS with empty evidence_index
   (a PASS without evidence is flagged, matching `require_wire_evidence`
   intent).
5. **Record**: wave root path + aggregate outcome + seal shas into the ledger
   (append-only) + manifest `status` note (the r3r pattern: the manifest cell
   status field IS the running evidence index).

### 2.5 The python3.12 mandate
Runner + all python suites run on **python3.12 (3.12.13,
`/Users/palanisd/.homebrew/bin/python3.12`)**. System `python3` = 3.14.7 (verified
both installed today) is **NOT runner-legal** (71-handoff-brief §7; campaign
discipline). Rationale to preserve in the gate: stdlib drift between 3.12 and
3.14 (e.g. `ssl`, `tomllib` edge behavior, unittest output) makes a 3.14 verdict
uncomparable to the sealed R3 base; the mandate is a PROVENANCE property, not a
preference. Gate enforcement: the wave runner header must log the interpreter
`sys.version` into `env_meta` (extend the existing `env_meta` block, run.py:5030-
5038) and the aggregate lane rejects a wave whose cells ran on non-3.12
interpreters. (Mechanical, zero behavior change for compliant runs.)

## §3 — Response-side pin surface (the engine gap)

The golden engine pins REQUEST bodies only (golden_compare run.py:1822 reads the
`.body` of `req-NNN.json`); `resp_status` (run.py:2086) reads the HTTP status
only; wiretap2 ALREADY writes `resp-NNN.jsonl` with frame fidelity (line 1
`{n, status, ts, headers}`, then ONE NDJSON line per SSE frame
`{frame_index, frame}` — wiretap.py:180-199 `begin_resp`/`append_frame`). The gap
is a READER, not a capture. Two new wire assert kinds:

### 3.1 Kind `resp_body` (non-stream response body)
- Selection: same machinery as `resp_status` — `file` glob + `where` filter on
  the REQUEST side + capture-`n` pairing (run.py:2097-2130 pattern) + optional
  `which: last`.
- Read: concatenate the resp file's frame lines (a non-stream JSON response
  arrives as one or a few chunks), JSON-parse; on parse failure → FAIL-CLOSED
  harness failure (an unparseable status/body is not evidence).
- Compare: canonical compare against the fixture — REUSE `_golden_canon`
  (run.py:1716) + `_golden_null_path` (run.py:1725) + `_golden_field_diff`
  (run.py:1781) by factoring a `golden_compare_doc(actual_doc, fixture_doc,
  normalize)` core out of `golden_compare` (run.py:1822) that both the request-
  body path and this path call (the request path keeps its capture-`.body`
  extraction; zero behavior change to existing pins).
- Fixture: `expected_resp_body-<name>.json` = the response body object with
  EXPLICIT NULLS at volatile positions (the request-side convention from
  golden-smoke-01, schema L795: "the fixture carries the matching explicit
  nulls"). `normalize: [paths]` supported identically (N/M loudness inherited
  from the shared core, run.py:1843-1858 — this ALSO fixes the dormant
  loudness on non-declarative paths for free, since the core is shared).
- Status: the fixture may carry `"status": <int>` → the kind asserts status AND
  body (a `resp_status` + `resp_body` pair collapses into one assert).

### 3.2 Kind `resp_stream` (stream-frame pin)
- Selection: identical to `resp_body` (request-side `where` + `n` pairing).
- Read: the resp file's frame lines as an ordered array.
- Compare (the normalization problem is real — frame sequences carry volatile
  framing): a declared **frame policy**, not raw bytes:
  1. **Drop frames**: `drop: ["event: response.completed"]`-style filters by
     SSE event type (the frame line carries the raw SSE text; the parser
     extracts the `event:`/`data:` head) — volatile terminal bookkeeping.
  2. **Normalize within frames**: per-frame JSON canonicalization of `data`
     (same `_golden_canon` core) + `normalize: [paths]` dotted-path nulling
     applied to each `data` object (reasoning item ids `rs_*`/`encitem_*`,
     usage deltas, `response_id`, timestamps — the volatile set mirrors the
     request-side set from golden-engine doc §3 step 3).
  3. **Count bounds**: `min_frames`/`max_frames` (the COMP-3 storm-band pattern
     of xw-az-vlq.json's count pin 1..18, generalized to the response side).
  4. **Subsequence match** (default `mode: subsequence`): the fixture frame
     sequence must appear IN ORDER in the actual sequence (interleaved chunks,
     keep-alive pings, and reasoning deltas between pinned frames are
     tolerated) — full-sequence exact match is `mode: exact` (opt-in, rare).
- Fixture: `expected_resp_stream-<name>.json` = `{status, mode, frames:
  [{event, data:<canonical object with explicit nulls>}]}`.
- Why subsequence-by-default: the responses wire streams thinking deltas and
  per-token chunks whose COUNT is model/run-dependent while their SHAPES are
  pinned; exact byte frames would make every pin VACUOUS-with-noise. The
  wstream `observe` fields (reasoning_frames, finish, budget_trap) become
  assertive pins when a fixture is attached to them.

### 3.3 Where it lives (run.py placement)
- `WIRE_KINDS` (run.py:3765): append `"resp_body", "resp_stream"`.
- `check_wire` (run.py:1898): two new branches after `resp_status` (run.py:2086)
  — they share its selection/pairing helper (factor `_resp_pair(spec, base)`
  out of the resp_status body — the status logic moves UNCHANGED into the
  helper).
- `AUDITED_WIRE_KINDS` (run.py:2320): append both (they make per-file claims —
  the evidence audit applies: `require_wire_evidence` cases cannot pin a
  response without the cite).
- `case.schema.json`: extend the wire_assert `kind` enum (the L780-786 block,
  ratchet-1 pin — the schema gate will flag the enum change; that is intended,
  it is a ratchet event, ledgered) + `where`/`nth`/`any`/`which` reuse +
  `normalize` (already legal on `golden`).
- `test_run_golden.py` (19 tests): offline tests for both kinds against canned
  `resp-NNN.jsonl` fixtures (the test_run.py pattern — stdlib-only, no proxy).
- Interaction with normalize nulling: the request-side `golden_compare` nulling
  (run.py:1725, applied to the FULL capture doc before `.body` extraction) is
  UNTOUCHED; the resp kinds apply the same `_golden_null_path` to resp docs —
  one nullee, two call sites, no divergence risk (the shared-core factoring is
  what guarantees it).

### 3.4 First users (design validation targets)
- Remote-v2 compaction: pin the v2 replacement stream (the t2 evidence of
  run-compaction-smoke.sh is a stderr log grep today; with `resp_stream` the
  compact response's `compaction_trigger`/replacement-items shape becomes a
  byte pin — closes harness-map §3.2).
- Error-body classes: the W1-W14 gap list (error-class-audit) is currently
  status-only (`resp_status` want/`status_in`); `resp_body` makes each
  classifier-relevant 400 body a pin (the .74 xw_orphan L0 fixtures are the
  L0-side twin of the same bodies — §4 mint protocol connects them).
- D-ENC: the t3 "history incompatible with the current model" friendly error is
  a `resp_body` pin (RECON-class until affinity lands; hard pin after).

## §4 — Crate-ization (what becomes Rust unit tests vs stays python) + mint protocol

### 4.1 The split rule
**Deterministic, I/O-free product functions → L0 Rust (in-crate, `include_str!`
fixtures, PROVENANCE.md). Anything that touches a binary, a proxy, a model, or a
process boundary → python (L1/L2/L3).** The split follows the seam, not the
feature:

| Becomes L0 Rust unit test (precedent cited) | Stays python (live/hermetic) |
|---|---|
| Switch-time projection (T0/T1/T2/T3 storage mapping) — .71 IN FLIGHT: `projection.rs` 474 L + `projection_tests.rs` 592 L, fixtures `projection_x71/` (4 byte-mirrors + 1 synthetic + KAT 12/12) | The live switch itself (set_model, wire capture, post-switch turn) = xw-* cases |
| Id-grammar canonicalization KAT (`xw_proj_id_grammar_canonical` — the `xw_` + sha256[:24] grammar, byte-reproduced from live cells) | The xwfix corpus cells themselves (L2/L3 goldens) |
| Error-body classification (.74 `testdata/xw_orphan/` 4 fixtures; 9/9 sampling-types etc.) + the §3 `resp_body` pins' L0 twins (classify the SAME body objects) | `resp_body`/`resp_stream` live pins (the proxy is the oracle) |
| Canonicalization parity: the Rust projector's `py_json_canonicalize` vs Python `json.dumps(sort_keys, compact)` KAT (sdd-71:121 parity note — the L0 goldens must match the Python golden form) | The golden compare engine itself (run.py — stdlib python by house rule) |
| Ingress normalization (.77 `normalize_content_types`, `is_openai_family` pins — provider.rs:941 E2 pin pattern) | The dogfood launcher + wiretap2 (operators' tooling) |
| Compaction engine shape (the .82 model-bound arm: `ModelBoundHistory` variant, strip-once) — compaction crate 132/132 baseline | Compaction LIVE cells (rt-c*, ws9-s05, run-compaction-smoke) |
| XSEARCH replay dialect (.76 `xsearch_replay` fixtures) | MCP/subagent cooperation cases (model temperament is not unit-testable) |

Anti-patterns the rule prevents: (a) porting the runner to Rust (house rule:
Rust-free harness; and it would dual-maintain the verdict semantics); (b) minting
L0 fixtures from a SYNTHETIC recipe when a LIVE capture exists (the PROVENANCE
class label exists precisely to make that visible — xw_orphan's
`azure_callid_orphan_400_body.json` is an explicit PREDICTION with a needle
check); (c) editing an L0 mirror to fit a RED (redcycle STOP rule 1; the
projection_x71 PROVENANCE.md "NEVER edit a mirror to fit a RED" line).

### 4.2 The mint protocol (L2/L3 capture → L0 fixture; defines the byte-mirror + sha pattern)
Source precedents: `.74 xw_orphan` PROVENANCE.md (file sha + inner-content sha +
provenance class + status + hygiene) and `.71 projection_x71` PROVENANCE.md
(byte-mirror table: source sha256:12 == mirrored sha256:12 + byte-verified date +
drift note + KAT ids). The protocol, as a standing contract:

1. **Mint source** (exactly one, labeled): `LIVE` (wiretap capture from a named
   run dir — the run must be sealed: campaign.json + verdict.json present) ·
   `SYNTHETIC-RECIPE` (deterministic recipe, inputs named) · `PREDICTION`
   (explicit; needle-checked; structurally-unreachable notes required).
2. **Byte mirror** (LIVE mints into L0): copy the corpus file into the crate
   fixture tree; record `source sha256:12`, `mirrored sha256:12`,
   `byte-verified <date>` in a table; the two MUST be equal (a mismatch = the
   mirror is defective, not the source — re-copy, never "fix").
3. **PROVENANCE.md per fixture dir**: for each file — sha256:12 (file + inner
   content where applicable, e.g. the xw_orphan inner-message sha), provenance
   line, status (LIVE/SYNTHETIC/PREDICTION), known false-positive raw-key
   classes (recorded, not redacted, per the .74 coordinator ruling pattern),
   hygiene: raw-key sweep = 0 (the campaign pattern, quoted escaped self-safe
   so the PROVENANCE.md itself sweeps 0).
4. **Drift rule**: when the SMOKE corpus re-pins a cell (e.g. the .70 re-pin
   table, 71-handoff-brief §5), the L0 mirror is either re-minted (new sha
   pair, new byte-verify date, drift note in the same PROVENANCE.md — the
   az-vlq "49 vs 53" drift note is the model) or the L0 test is retired — never
   edited in place. If the corpus re-pin happens MID-CUT (before W2), STOP
   (sdd-71 §11 stop 3) and re-derive from the live cell.
5. **KAT where a grammar exists**: any id/shape synthesis with a grammar (the
   `xw_` id grammar) ships known-answer goldens (12/12 pattern) so a hash
   library swap cannot silently re-key the corpus (the .71 G3 self-contained
   SHA-256 ruling — keep through GREEN, post-GREEN micro-cut bead for the
   `sha2` dep — the KAT is what makes that swap safe).
6. **Placement**: fixtures live in the crate test tree where the test runs
   (`include_str!`) — in-tree precedents: `testdata/affinity/` (.75),
   `src/conversation/fixtures/xsearch_replay/` (.76),
   `src/conversation/fixtures/projection_x71/` (.71), `testdata/xw_orphan/`
   (.74). The crate fixture is a MIRROR; the smoke corpus (or the sealed run
   capture) is the SoT for live-minted pins (pointer-never-copy, inverted:
   here the copy is deliberate and sha-anchored, because L0 must run without
   the worktree's dirty smoke tree).
7. **A6 discipline at mint**: new fixture-backed test NAMES are declared in the
   cut's tdd/ops doc (the .71 declared-name pattern: 7 names declared in
   71-handoff-brief §2) — undeclared names = STOP at the gate.

## §5 — Promotion to the main grok-build repo

### 5.1 What lands in-repo (tracked)
- **Design + map docs**: already in-repo under `smoke/redteam/kiloecho/` (this
  doc, the map, the brief, the siblings) — the README's "Rides the established
  `smoke/` pathspec family (exfil + coordinator commit flow)" applies: they
  promote with the smoke pathspec, not separately.
- **The runner + contract**: `smoke/redteam/{run.py, case.schema.json,
  manifest.json, gen-matrix.py, test_run.py, test_run_golden.py,
  grok-dogfood.v2}`.
- **Cases + fixtures (the registry)**: `smoke/redteam/cases/*.json` (61) +
  pre-placed wire inputs (`cases/<stem>/wire/golden-*.json`,
  `golden-smoke-01-expected.json` — source-capture shas in manifest `pins`) +
  `smoke/xwfix/cells/**` (pre_switch/expected/cell.json + `expected_wire.json`
  when .71 mints them) + `smoke/xwfix/manifest.json`.
- **Capture layer**: `smoke/wiretap/wiretap.py` (+ selftest).
- **wstream**: `smoke/wstream/{run.py, manifest.json, cells/**, README.md}`
  (derived-config.toml is per-run output — gitignored, §5.2).
- **triage**: `smoke/triage/{grok-triage, signatures.json, test_grok_triage.py,
  install.sh}`.
- **L0 fixtures (already in-crate)**: the four fixture dirs + their
  PROVENANCE.md files — promote with the crate pathspec of their owning cuts
  (.71/.74/.75/.76), NOT with the smoke pathspec (different A6 declared-name
  surfaces per cut).
- **NOT promoted**: the one-off zsh runners' DIAGNOSTIC outputs, report trees
  (pruned after digest capture per manifest `probe_policy`), `__pycache__`.

### 5.2 .gitignore strategy for report jsons
Add (smoke-level, one block — the current worktree carries the report trees
UNTRACKED, which is the accident the ignore block cures):
```
# generated evidence (sealed captures; digests land in manifest/ledger, not git)
smoke/redteam/report*/
smoke/wstream/report/
smoke/*/report/
smoke/**/__pycache__/
**/*.wiretap-stdout.log
```
Rationale (tracked vs gitignored):
- **Gitignore = captures + runs**: `report*/` trees are per-run evidence with
  sealed campaign.json + verdict.json — large, key-sha-stamped, reproducible by
  re-running the wave, and pruned after digest capture (manifest
  `probe_policy`). Their AUDIT property is the sealed sha in the ledger/manifest
  (a pointer), not the bytes in git — the pointer-never-copy ruling applies:
  the ledger cites the run dir, the dir is not copied into another tree.
- **Track = pins + contract**: case files, cell goldens, PROVENANCE.md,
  manifests, the runner — the registry must be diffable (a pin change is a
  CODE review, with the re-pin table as the required artifact — 71-handoff-
  brief §5 is the template: 5 rows, each with old/new/reason).
- **Track = the L0 crate mirrors** (byte-anchored; their sha tables make
  mirror-vs-corpus drift greppable in review).
- The ignore block must NOT cover `cases/<stem>/wire/` pre-placed inputs or
  `smoke/xwfix/cells/` — those are pins, not captures (the `smoke/*/report/`
  pattern is scoped to report dirs only).

### 5.3 CI shape (jenkins-runnable vs on-prem-only)
- **Jenkins-runnable (no secrets, no network-to-proxy)**:
  1. L0: the canonical 6-pkg cargo gate (review-gates-q3.md §1 byte-identical
     command) + A6 name-by-name (the name list is data, not a secret).
  2. L1: `python3.12 smoke/redteam/run.py --selftest` (schema + contract +
     73+19 tests — no key, no binary, no proxy by construction) +
     `python3.12 smoke/wiretap/wiretap.py --selftest` +
     `python3.12 smoke/wstream/run.py --selftest` + the triage unittest.
  3. L1 binary-hermetic (once §2.1.7 lands): `proxy:none` mechanism cases
     (pre-placed wire, dead port, no key) — still secret-free.
  4. The `--aggregate` lane over a SEALED wave root (key-free; validates seals,
     writes summary) — this is how a jenkins job can VERIFY an on-prem wave
     without re-running it: the operator ships the wave root (or its digest),
     jenkins re-validates the seals + aggregate.
- **On-prem-only (operator seat)**: L2/L3 live waves (they need
  `CODEX_LLM_PROXY_KEY` in the env + the built binary + the live proxy).
  **Key discipline (non-negotiable, already the house rule, kept): env-only,
  never committed, never written** — run.py reads it from the process env and
  fatals if absent (run.py:4918 block); every artifact carries the key only as
  sha256:12 (campaign.json `key_sha256_12`, report.md header, dogfood manifest);
  the redaction sweep (run.py:3515) + dogfood manifest grep are the
  enforcement; §6 makes sweep hits a hard wave failure. A jenkins job that
  ever receives the key VALUE (even transiently) is out of scope for this
  design — the live lane stays on the operator seat by construction.
- **What CI gates what**: L0+L1 gate every commit touching
  `crates/codegen/{xai-grok-sampler,xai-grok-sampling-types,xai-grok-shell,xai-
  chat-state,xai-grok-config-types,xai-grok-agent}` + `smoke/` (the 6-pkg set +
  the harness selftests). L2/L3 run per-wave (cut-driven: any cut touching
  wires/compaction/switching/resume/subagent/MCP carries an L2 re-verify wave —
  the .71 §3.6 shape — plus an L3 slice at the campaign boundary).

## §6 — The unified gate (pass criteria + the .71 L2 re-verify as one wave)

### 6.1 Wave anatomy (one gate = one WAVE ROOT)
A wave = `<WAVE-ROOT>/` containing, per cell: `<cell>/campaign.json` (sealed) +
`<cell>/<cell>/verdict.json` (+ evidence dirs), plus the aggregate outputs
(`summary.json`/`summary.md` from the `--aggregate` lane) and the gate record
(gate.md: interpreter versions, binary sha12, key sha12, seal sha list, A6
name list, sweep results). The gate passes iff ALL of:

1. **L0 cargo**: canonical 6-pkg gate (review-gates-q3.md §1, byte-identical:
   `CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 RUST_MIN_STACK=67108864 cargo test
   --release -p xai-grok-sampler -p xai-grok-shell -p xai-grok-sampling-types
   -p xai-chat-state -p xai-grok-config-types -p xai-grok-agent --features
   xai-grok-shell/test-support --no-fail-fast`, + `-- --skip image_strip_tests`
   while .73's hang is open — the skip must be reported in the evidence line).
   **A6 name-by-name**: failing test NAMES compared against the baseline — the
   **8 known-name baseline at the last wave gate (5f4411a, 2026-09-18 21:06Z,
   manifest `expected_refs.wave_gate`): C-3 union ×5, rotating-wake ×1,
   watcher ×1, env util::hooks ×1** — plus the A6 baseline SET (review-gates-
   q3.md §2: the C-3 6-name union, rotating-wake family, resume pair, watcher,
   gated-reconnect CONDITIONAL with tripwire, env subagent_429 + util::hooks,
   6 skips) — plus the **per-cut declared names** (declared in the cut's
   tdd/ops doc; the .71 declaration = 71-handoff-brief §2's 7 names:
   `projection`, `project_switch_history`, `Boundary`, `ProjectedHistory`,
   `proj_items`, `xw_proj_id_grammar_canonical`,
   `is_openai_family_empty_is_true_is_pinned`). ANY other new failing name =
   STOP (admission only as conditional rotating flake WITH tripwire +
   isolation evidence, JIG §2 step 5).
2. **L0 fixture KATs**: all minted fixture dirs reproduce their PROVENANCE.md
   sha tables (byte-mirror equality + KAT ids).
3. **L1 offline**: `--selftest` GREEN (schema dual-engine 0 failures on NEW
   cases; case-contract 0; 73+19 tests) + wiretap selftest GREEN + wstream
   selftest GREEN + (post-§2.1.7) proxy:none mechanism cells PASS.
4. **L2/L3 harness corpus verdicts**: the wave's cell roster (per-cell `--out`
   discipline, §2.3) — aggregate outcome per the roster's expectation:
   - RED-EXPECTED cells: FAIL with the DOCUMENTED named assert (the right-
     reason check: the failing assert is the one registered via
     `expected_red` — e.g. the .71 storage-diff assert, not "some diff");
     RED-CLEARED cells (post-fix waves): PASS.
   - PASS-guard cells: PASS with evidence_index non-empty (wire-grade
     evidence where `require_wire_evidence`).
   - VACUOUS/BLOCKED: admissible ONLY with the premise explanation in the
     verdict (model-temperament VACUOUS is a recorded outcome, not a failure —
     the RT-M11 discipline; harness BLOCKED fails the wave).
   - Stability: FLAKY cells are recorded (the runbook caps apply — CAPS,
     run.py:2313 area: full_blocked_max_no_ruling 3, slim_vacuous_ruling_
     threshold 3, env_retry_max_per_cell 1; the runner REPORTS caps, the gate
     ENFORCES them, per the D-3 note).
5. **Recon sweeps** (all hard, all in the gate record):
   - raw-key sweep = 0 files across the wave root (literal key grep; the
     report-r3r "10 hit(s)" log-without-fail behavior is CLOSED by making
     hits a wave failure — §3.7 gap);
   - triage: every FAIL/BLOCKED cell's triage block matched against
     `signatures.json` — a NEW unclassified error shape = a PROPOSED signature
     row filed (report-and-stop; single-writer KE-3) before the wave closes;
   - 400-phrasing drift: no NEW lapsed-400 phrasings (the lapsed-400 drift
     class must remain lapsed — 71-handoff-brief §4 second-assert analysis);
   - plans/ intel registry refresh (the map §4 sweep, dated — STALE-BY-DRIFT
     docs listed, not re-litigated).
6. **Provenance**: binary sha12 of record recorded (fresh release build per
   wave; the `/tmp/binary-of-record-<sha12>` copy convention); git HEAD +
   exfil state recorded (the ls-remote fork+mirror check at the gate moment);
   config_sha256 (the hermetic source config) in every campaign.json.

### 6.2 The .71 L2 re-verify as ONE wave (subsumes 71-handoff-brief §3.6)
The .71 re-verify is not a special procedure — it is the FIRST instance of the
unified gate's L2 layer, and its protocol maps 1:1 onto §6.1:

| 71-handoff-brief §3.6 (binding) | Unified-gate clause |
|---|---|
| Fresh release build; record new sha12; `/tmp/binary-of-record-<sha12>` copy | §6.1.6 (provenance) — wave-level binary of record |
| Re-run the R3 11-cell corpus, python3.12, per-cell `--out` (root-seal overwrite lesson) | §2.3 + §6.1.4 — roster = the R3 11 xw cells; interpreter mandate §2.5 |
| Exact invocation: `python3.12 smoke/redteam/run.py <case> --bin <new-binary> --out <PER-CELL-ROOT> --campaign-id apex-ayl.71-l2 --wave L2-<date> --mode adhoc` | the standard L2 cell invocation (identical; campaign-id = bead + wave, per §2.4) |
| Expected: 7 RED-EXPECTED clear (vxm-az idx6, az-vlq idx0, vxm-vlg idx2, vxm-vlg-guard storage-only, vxm-vlq idx2, vxm-vlq-v2 storage-only, vxm-vxg idx2 — the named asserts) | §6.1.4 RED-CLEARED semantics — clear = the named storage-diff assert PASSes with the projector re-keying at switch time |
| 3 PASS guards hold (az-az, az-vxm, vlq-vlg-ws) | §6.1.4 PASS-guard cells (they break if the projector overreaches) |
| vlq-vlg UNPROVEN wall cell UNCHANGED (preemptive lossy compact on qwen→glm; T0 golden still fails as documented; .68 owns; DO NOT RE-PIN) | §6.1.4 RED-EXPECTED with a STABLE documented shape (assert-identical across r2 and r3 — the stability itself is the pin) |
| zero COMP-3 storms (c2 ≤ 18) · zero acp_error | §6.1.4 count-pin bands + ndjson absent pins (already in the cell files — the gate just enforces the aggregate) |
| Recon sweeps: raw-key literal = 0 files in both trees; no new 400 phrasing | §6.1.5 (both, hard) |
| Then exfil + pathspec commit + tag (brief §3.7) | §6.1.6 provenance step (coordinator flow; the gate record cites the exfil shas) |

So the unified gate does not replace the .71 protocol — it NAMES it (L2 wave)
and generalizes it (the roster, the interpreter mandate, the sweep enforcement,
and the aggregate artifact are the additions; every §3.6 expectation is a
§6.1 clause). Post-.71, the SAME shape runs for .72 (L3 full matrix sweep fed
by R3 findings + `kiloecho/adversarial-brief-20260918.md` case shapes) and for
the .78-B ws9 ship gate (python3.12; s05/s06/s07/s11/s12 — brief §0 item 4).

### 6.3 What this design does NOT do (boundaries)
- Does not change the verdict engine's semantics (finalize_verdict stays; the
  premise/tolerant/evidence classes are proven on the R3 base).
- Does not merge the wstream runner into run.py (its invariants/observe
  vocabulary + derived-config per-run capture are a different contract; the
  extension seam in its README is the future unification point, and it is
  OUT of scope for this gate).
- Does not add Rust to the harness (house rule; L0 is product-crate tests, not
  harness tests).
- Does not re-litigate the layering ruling (pointer-never-copy; xwire/ and
  provenance/ not retired; kiloecho = consolidated living facts + pointers).
- Does not resolve the kiloecho-owned gaps it only registers: the proxy-side
  ratchet gaps (wire-topology-matrix §3 "Missing" list: shim content-part
  normalization test, M-1 effort-clamp live test, route allowlist test —
  `.78-B`/proxy-lane owned) and the litellm version-pinning problem
  (litellm-transform-matrix §0/§1.3: ratchet key = (litellm version, proxy-
  branch, config digest); deployed image NOT branch-reproducible; §4.9 pending
  in that doc). Those are RATCHET targets outside the client gate; the client
  gate (this doc) pins what the CLIENT sees, the proxy ratchet pins what the
  proxy does — both feed the same error-class registry (error-class-audit).

### 6.4 The gate key (addendum 2026-09-19, sibling stream-A handoff gap 5 + KE-2 ratchet §3.1)

A wave verdict is only admissible against the key it was run under:
`(x_litellm_version = X-Litellm-Version response header read LIVE from any
in-wave call, proxy_git_rev = the rev the deployed image was built from,
config_sha256_12 = sha256:12 of the deployed config_seclab.yaml)`. The
deployed image is not reproducible from any tracked branch (KE-2 #1; the
litellm checkout HEAD = the v1.90.0 tag exactly — sibling proof, rev-list 0),
so a post-drift sweep 'pass' recorded WITHOUT the key is NOT a pass. The
runner writes the key into env_meta (alongside sys.version per §2.5) and the
seal_campaign meta; the aggregate lane rejects a wave in which any L2/L3 cell
lacks the key. Artifact spec (machine-checkable): KE-2 ratchet §3.2
(`kiloecho/ratchet/error-surface-<xver>-<proxyrev>-<cfgsha12>.json`). Ledger:
2026-09-18T23:5xZ (M-1 RECONCILED + ratchet-key adoption).

## Footer
- Raw-key sweep of THIS doc: 0 (sha12 references only; the key value appears
  nowhere in either kiloecho deliverable).
- Both deliverables written to the canonical worktree path
  `/Users/palanisd/Projects/upstream/wt/grok-build-responses/smoke/redteam/
  kiloecho/` (the stray-tree incident was not this seat's — no files were ever
  written outside it).
- Everything cited as `path:line` is @ HEAD 0fc1060 first-hand; the only
  UNVERIFIED items are marked as such (litellm-transform-matrix §4.9 = pending
  in the KE-2 doc; no other UNVERIFIED items remain — the two READ-FIRST
  prerequisite docs were located and full-read before writing).
