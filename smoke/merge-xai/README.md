# MERGE-XAI-SYNC — wave-3 operator runbook (hermetic smoke matrix)

Spec (authoritative): `grok/plans/merge-xai/06-smoke-plan.md` — bead apex-l2d.4.
Driver: the EXISTING `smoke/redteam` runner, UNMODIFIED — wave-2 wrote case
files + frozen lane configs + fixtures only (06 §4: no new harness code).
Companion surfaces: `lanes/manifest.json` (cell→lane map, serial order) and
`smoke/redteam/fixtures/merge-xai/README.md` (fixture index + redaction
discipline).

## Layout

- `cases/` — 17 case files: the 14 cells of 06 §2 as case-ops + two
  zero-step `proxy:none` MECHANISM ARMS (MXAI-C01-REPLAY,
  MXAI-C02-TRUNC-SHAPE — they run the halves the live driver cannot induce)
  + MXAI-C08-BASELINE (the pre-merge reference). Symlinked into
  `smoke/redteam/cases/` (17 file symlinks + 2 dir symlinks for the
  mechanism arms' wire). The driver globs ONLY `smoke/redteam/cases/*.json`
  — always run with an EXPLICIT case-id roster.
- `wire/` — pre-placed wire fixtures for the two mechanism arms
  (`mxai-c01-replay/`, `mxai-c02-trunc-shape/`). Copy-if-absent into the run
  dir; a same-named LIVE capture OVERWRITES it (open('w') — the live capture
  is the evidence).
- `lanes/` — 4 frozen lane config templates (full self-contained
  `config.toml`s: model + wire pinned, `base_url` = dead-port PLACEHOLDER the
  launcher rewrites to the wiretap port, `env_key` = the ENV VAR NAME of the
  ambient proxy key (the value never ships), no `[auth]`, no host paths) +
  `manifest.json`. A grok-4.6 /xai lane is deliberately NOT instantiated —
  no cell in the 06 §2 matrix needs the xai wire (06 §1 lists it
  conditionally; a wave-3 red adjudication that demands one is an operator
  add).

## Run command (from the repo root)

    python3 smoke/redteam/run.py <ID> --bin <binary-of-record>

Reports are reanalyze-able. The two mechanism arms run key-free with ZERO
live calls (proxy:none L1 — the `--bin` is never launched for zero-step
cases):

    python3 smoke/redteam/run.py MXAI-C02-TRUNC-SHAPE --bin /bin/true
    python3 smoke/redteam/run.py MXAI-C01-REPLAY     --bin /bin/true   # after fixture population

## Cell table (06 §2, as authored)

| cell | class | case op(s) | lane | scenario | scored surface |
|---|---|---|---|---|---|
| C-01 | PIN (S5/A2) exact-bytes retry | MXAI-C01, MXAI-C01-REPLAY (disabled) | L-01 (L-02 sibling) | one bounded turn on the responses lane of record; the retried pair pre-placed from a real capture | no zstd on any request or response (compression=off verified live); turn recovers exit 0; no storm (1..2 lane requests); retried body == original carrier (C01-REPLAY golden, EMPTY normalize list) |
| C-02 | PIN (S4/A4) stream-truncated hard-fail | MXAI-C02 (disabled), MXAI-C02-TRUNC-SHAPE | L-03 | stream cut without `done` (fault arm) / truncated-capture shape (shape arm — runs now) | honest terminal with the `stream_truncated` phrasing; wire capture carries no message_stop frame; no silent Completed; shape arm: exact frame shape, no frame past the last delta (PASS-verified offline) |
| C-03 | PIN (M5/MW-1) cache-breakpoint sentinel | MXAI-C03 | L-03 | new turn over a trailing-assistant session | head `cache_control {type:ephemeral, ttl:"1h"}`; TIP `{type:ephemeral}` (no ttl) on the synthetic-user `[Continue]` sentinel; NONE on the assistant; request band 2..5; block-level placement = recon (PARTIAL gap) |
| C-04 | PIN (S1/D7/R2) switch slot + projection + merged CMO | MXAI-C04 | L-01 → L-04 | in-session `/model` switch sol → qwen3.8-27b | slot moved (wire face); qwen request on the new wire; merged CMO carries BOTH `reasoning_summary` + `cache_ttl`; no foreign reasoning replay (R8 intact) |
| C-05 | PIN (M17) 6-level effort menu | MXAI-C05 | L-01 | `/effort` post-merge | the LIVE wire face of the top tier (ultra cursor → wire `max`); the exact 6-item count is the crate golden (full gate) + coordinator cross-check (PARTIAL gap) |
| C-06 | PIN (D12/M2) schema goldens + catalog hydration | MXAI-C06 | L-01 | fresh hermetic boot | boot completes exit 0; catalog hydrates FROM THE PROXY (GET /v1/models → 200 through the wiretap); CMO round-trip wire face |
| C-07 | PIN (M7) MAV2 child projection + mailbox | MXAI-C07 | L-01 + L-04 | spawn v2 subagent, parent→child→parent | child tool surface = VerbatimMirror minus `send_subagent_message` (+ the 5 v2 collab tools KEPT — fixtures/merge-xai/c-07-child); mailbox round-trip completes (CHILD-ACK terminal) |
| C-08 | PIN (D9/Q9/R8) cold-spawn identity round-trip | MXAI-C08, MXAI-C08-BASELINE | L-01 | minted identity → SIGKILL mid-essay → cold resume | post-resume `agent_id` EQ the cell-seeded id (identity ADOPTED, never re-minted); attempt stamped; T2 completes; the BASELINE run (pre-wave-3) is the diff reference |
| C-09 | DRIFT (01 S3/R6) image-budget eviction → compaction | MXAI-C09 | L-04 | text-only compaction arm (the 47 MiB image arm = operator) | compaction completes; post-compact request 200 (verbatim-input stability); no COMP-3 storm (3..10 qwen requests); eviction placeholder ABSENT from record (negative control) |
| C-10 | DRIFT (01 S5/R5) concurrent-boot catalog isolation | MXAI-C10 | L-01 | single-boot smoke arm (the concurrent pair = operator) | boot clean + turn closes; catalog hydration 200 (1..3 GET /v1/models fetches) |
| C-11 | DRIFT (01 S2/R7) timed_out task terminal semantics | MXAI-C11 | L-01 | background `sleep 300` with a forced 15 s timeout, 600 s budget | exit 0 (a 600 s watchdog SIGKILL = the wait loop hung = red); `C11-DONE`; `timed_out` in the merged text (the new `is_terminal` member landed under our code) |
| C-12 | ADOPT (01 S10) interject during background wait | MXAI-C12 (disabled) | L-01 | interject arrives mid-background-wait (TUI surface) | — scaffold with the enable contract recorded: `MXAI-C12-INTERJECT` + `MXAI-C12-WORKER-RESULT` nonces, clean terminal |
| C-13 | ADOPT (01 S6) MCP long-prefix admission | MXAI-C13 | L-01 + L-04 | the >256-char qualified-name MCP edge through our session | long-prefix tools ADMITTED (1.0.31 behavior) + `child_tool_projection` passes them (M7); echo round-trips (`MXAI-C13-ECHO` / `MXAI-C13-CHILD`); UNPROVISIONED = BLOCKED (harness class, via `lp_ann`) |
| C-14 | ADOPT (01 S1) Grove-off worktree fallback | MXAI-C14 | L-01 | worktree create (hermetic-Mac grove-off arm; the NFS deployment-class arm = operator on SCS) | RECON, record-only: silent degrade to the copy/btrfs fallback, NO error surfaced; `WT-OUTCOME ok C14-DONE` terminal |

## Sequencing (06 §3)

1. **Pre-wave-3 (the ONLY pre-merge item):** MXAI-C08-BASELINE on the
   CURRENT binary-of-record, operator-driven — one cold-resume run, the
   identity round-trip captured. It seeds from the SAME `c-08-cell` fixture
   as MXAI-C08 (the seeded identity is a fixture constant), so the two runs
   are directly comparable — the baseline report is the reference the
   post-merge run is diffed against (06 §2 C-08: "matches PRE-MERGE
   reference run").
2. **Wave-3 step 6 green** (full gate) — prerequisite for all cells.
3. **Operator GO + budget** (~$0.5–1.0 total, serial, 2–5 min per cell):
   PIN first (C-01…C-08 — they gate "the merge didn't regress us"), then
   DRIFT (C-09…C-11), then ADOPT (C-12…C-14).
4. **Any red → isolate** (single re-run) → reproducibly red = the merge is
   NOT green; fix-forward and re-run the cell + the affected gate slice.
5. Reports land in `smoke/` (reanalyze-able); redacted wire captures
   committed to fixtures; the green board is the close-out evidence for
   apex-l2d.6.

## Operator arms + driver gaps (final set, wave-2)

Case-ops run on the EXISTING driver unmodified; where the 06 scenario is
unexpressible, the closest case-op carries the reachable surface and the gap
is recorded here (no harness extension, per 06 §4).

| cell | gap | closest case-op (what it carries) | operator arm |
|---|---|---|---|
| C-01 | live retry induction — wiretap2 is pass-through, no fault injection | MXAI-C01 (no-zstd / storm / recovery pins) + MXAI-C01-REPLAY (disabled until populated) | induce a retried request on a frozen lane (400-class or aborted turn); populate the wire pair per `fixtures/merge-xai/c-01-replay/META.json`; flip the case; run key-free |
| C-02 | stream-truncated SSE injection | MXAI-C02 (disabled; scored pins ready) + MXAI-C02-TRUNC-SHAPE (runs now — PASS offline) | the operator fault layer cuts the L-03 stream after the content deltas, before message_stop/[DONE]; live capture overwrites the pre-placed `resp-001.jsonl`; same golden scores it |
| C-03 | PARTIAL — block-level `cache_control` placement is positional JSON the driver cannot express (grep is positional-agnostic) | MXAI-C03 scored pins (request-level shape: head 1h ttl, TIP ephemeral, sentinel present, assistant none, band 2..5) + recon pin records the cache_control context lines | coordinator review of the recon record adjudicates block-level placement |
| C-05 | PARTIAL — no slash-menu op (ACP `available_commands` carries tool lists, not the `/effort` command menu) | MXAI-C05 pins the live wire face of the top tier (ultra cursor → wire `max`) | the exact 6-item count is the crate golden (`xai-grok-pager` `acp/model_state.rs:396`, `assert_eq!(ids, ["ultra","max","xhigh","high","medium","low"])`) in the wave-3 full gate — coordinator cross-check |
| C-09 | 47 MiB inline-image induction — no image-attach step op; the seed is text-only | MXAI-C09 text-only arm (compaction completes + post-compact 200 + no storm + placeholder-ABSENT negative control) | the operator image-induction run per `fixtures/merge-xai/c-09-image/META.json` (placeholder PRESENT + compaction + post-compact 200; the blob is redacted to size+hash before commit) |
| C-10 | concurrent-boot pair — the driver does one boot per case per temp home | MXAI-C10 single-boot arm (boot clean + catalog hydration) | two parallel dogfood boots sharing ONE hermetic GROK_HOME; both must complete catalog hydration and the pair's `models_cache.json` + startup settings must be conflict-free (the 1.0.25 isolation fix under our boot path); the board cross-references the pair report against the single-boot record |
| C-12 | no interject step op — the interjection surface is TUI-driven (headless = one prompt per process; ACP `prompt()` blocks to turn completion) | MXAI-C12 DISABLED (scaffold; the enable contract + both nonces are recorded in the case title) | the operator's manual ACP arm (interactive session, operator drives the interjection by hand; the same two nonces are the scored surface); `unblocks_at`: a driver interject step op (later wave) |
| C-13 | setup — the >256 MCP server must exist in the LIVE config; `config_patch` is scalar-only and cannot add `[mcp_servers]` (the hermetic home inherits the live `mcp_servers`) | MXAI-C13 (parent-admission + M7 child-projection arms; stub included at `fixtures/merge-xai/c-13-mcp/`) | operator provisions `[mcp_servers.mxai_longprefix]` in the live config (command/args shape in `fixtures/merge-xai/c-13-mcp/META.json`); UNPROVISIONED = BLOCKED via `lp_ann` (harness class, not red) |
| C-14 | NFS deployment-class arm — 06 §5: a fleet-ops question, not a merge gate | MXAI-C14 hermetic-Mac grove-off arm (RECON, record-only: the probe touches only the hermetic tempdir, self-cleaning) | the operator's SCS arm (the host where grove worktrees are genuinely off, NFS class); a SURFACED worktree error on either arm = the deprecation regressed under our code = new finding, report-and-stop |

## Verification (offline, wave-2 verified 2026-09-19)

- All 17 case files pass `run.validate_case_file` (schema + the NEW-case
  gate) under `python3 smoke/redteam/run.py --selftest`.
- MXAI-C02-TRUNC-SHAPE PASS on the real runner offline (`--bin /bin/true`;
  redaction sweep 0 hits); MXAI-C01-REPLAY SKIPs as a disabled scaffold.
- `--selftest` expectation for wave-3: the ONLY failure is
  `at-enc-probe-unpin.json` — a FOREIGN file from another campaign
  (pre-existing in the redteam case dir, not a merge-xai artifact). Do not
  "fix" it here; report it to the coordinator.
- The c-13 MCP stub was offline-tested (JSON-RPC line sequence piped
  through the stub: initialize / notification / tools/list / tools/call /
  unknown-method — see `fixtures/merge-xai/c-13-mcp/META.json`).

## Wave-2 authoring corrections (record for the board)

1. **D-4a gate:** a case WITHOUT `mcp_calls` cannot carry a harness-class
   `vacuous_if` premise — the gate rejects it. Fixed mxai-c05/c06 (previous
   seat) + mxai-c09/c10 (this seat): the harness premise entries were
   removed; the affected control pins score with report-and-adjudicate
   labels (the mechanism wording is in mxai-c05's assert labels: "the
   verdict engine cannot BLOCK here and the coordinator adjudicates the
   class under the stop-and-report house rule"). MXAI-C13 carries exactly
   ONE harness premise (`lp_ann`), as the drift gate requires.
2. **resp_stream normalize is FRAME-DATA-ROOT-RELATIVE** (engine:
   `_frame_match` → `golden_compare_doc(actual_data, fx_data, normalize)`;
   in-tree precedent `resp-stream-smoke-01`). MXAI-C02-TRUNC-SHAPE was
   corrected from `"data."`-prefixed no-op paths to
   `["message.id","message.usage","content_block.index","index","delta.text"]`
   (`delta.text` added so a LIVE truncated stream scores on its shape, not
   the redacted text). Fixture and capture match; PASS-verified.
3. **Worktree path discipline:** this worktree holds `wt/` only (it is not
   the git checkout) — every write in this set used the absolute worktree
   prefix (a relative path resolves against the checkout root and lands in
   the MAIN checkout, not the worktree; the stray tree was moved and
   removed during wave-2).

## Redaction

Everything committed is sweep-0 (no creds / host paths / IPs /
secret-bearing URLs; model ids + shapes stay; case-owned literals are the
pins). Operator-populated artifacts (the C-01 wire pair, the C-09 image-arm
record) are re-swept before commit. Discipline detail:
`smoke/redteam/fixtures/merge-xai/README.md`.
