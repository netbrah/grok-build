# KILOECHO — ADVERSARIAL DOCTRINE (stream-A independent)

date: 2026-09-18 · author: coordinator (stream A) · bead: candidate, apex-bqm
family (dedupe owed) · scope: how to break the harness — threat model,
failure-class taxonomy, attack surfaces, breach/collapse categories,
sweep cadence · status: FINAL v1 (doctrine — implementation rides the
named beads)

## 1. Threat model

Asset: wire fidelity — the invariant that a session's history, replayed
after ANY legitimate operator action (model switch, resume, compaction,
subagent spawn, proxy drift), reaches the target wire in a shape that wire
accepts, preserves as much portable context as possible (T0-T3 ladder),
and never leaks credentials.

Adversaries (no malice assumed — the "attacker" is combinatorics):
- A1 THE OPERATOR: any model/wire pair switchable mid-session, any resume,
  any /compact, any subagent pool across wires.
- A2 THE PROXY: version drift (1.90↔1.93 = 916-line bridge delta, KE-2 §5),
  route allowlist changes, LB affinity (D-ENC), per-model config rows
  (supports_* flags, effort menus, x-litellm-tags).
- A3 THE MODELS: per-family wire dialects (AZ encrypted carry, vLLM
  responses-strict, vertex responses-compat, /messages signatures,
  /generateContent bridge) with different acceptance surfaces.

We are NOT testing model quality. We are testing the seams the campaign
owns: projection, retry classification, carrier handling, config derivation.

## 2. Failure-class taxonomy (as-read at HEAD)

Reactive net (the .58 lineage): `classify_error` at retry.rs:105-186
(moved from the error.rs:428-477 era — that section is now the doc-comment
block for the predicates; the predicates — is_model_bound_history_error
etc. — still live at error.rs:476+, per KE-3 N8).

- F1-F5: stock classifier families (error.rs predicate set).
- F5-ext / F7 / F8: added by .74 (redcycle arc) — F8 = vLLM-shim 400 class
  (the session-bricking shape; class-(a) `input[N].content array too long`
  variant has NO triage row yet — KE-2 G-7).
- Family-9: added by .82 (exfil arc).
- 14 PROPOSED gaps (G-1..G-14, error-surface-ratchet.md): unclassified
  deterministic errors fall through to `RetryDecision::Fatal`
  (retry.rs:186) — terminal brick or wrong-retry, NOT retry. Highest
  severity: G-7 (F8 class-(a), the actual brick shape, no triage row) and
  G-2 (501 store/background — 5xx retry storm on a deterministic gate).

Doctrine: an unclassified shape is an EXERCISE FAILURE, not a tolerance.
Report-and-stop (STOP-1): triage row + xwfix cell + bead, same session.

## 3. Attack surfaces

Reporting format per surface: OBJECTIVE (what a pass proves) · BREACH
(the first bad shape observed) · COLLAPSE (session unusable) · DETECTION
(signature + cell).

- S1 CROSS-WIRE REASONING REPLAY. External reasoning items (encrypted
  content, thinking signatures, empty ids) replayed onto a foreign wire.
  Breach: 400/503 on the first post-switch request. Collapse: reactive
  strip drops ALL reasoning (all-or-nothing, the 01a0b046 sonnet→terra
  instance — 5 blobs lost). Detection: F3 empty-id / family-2 signature /
  F1 503 boundary; cells xw-vxm-az, xw-az-vlq, xw-vxm-vlg(-guard),
  at-az-vxg (gemini floor — nothing replayable).
- S2 COMPACTION CARRIERS UNDER SWITCH. cmp_ carrier items +
  raw_codex_input_replacements across a wire/model boundary. Breach:
  carrier 400 or silent drop (invariant 4). Collapse: compaction-trigger
  400 storm (triage row #16, COMPACT-BOUNDARM-1) or D-ENC cross-region
  decrypt failure (designed friendly error). Detection: row #16 + the
  D-ENC friendly string; cells ws9-s05-compact-resume,
  run-compaction-smoke t3 (gated).
- S3 TOOL-PAIRING UNDER PROJECTION. A T3 drop of a tool_call orphans its
  tool_result on replay. repair_dangling_tool_calls (.48 extension) covers
  the MISSING-RESULT direction for live calls; the ORPHANED-RESULT
  direction is UNVERIFIED — if the target 400s with a shape outside the
  classifier families, the reactive net cannot self-heal (a NEW failure
  class). Detection: any 400 on a projected turn containing tool_result
  without tool_call; cell owed (xwfix synthetic recipe: pre_switch with
  tool pair, expected T3-drops the call — the invariant-3 re-projection
  rule must be exercised).
- S4 EFFORT-MENU MISMATCH MID-SESSION. /effort or carried effort projected
  onto a model whose menu rejects it (M-1 class: the stock clamp degrades
  xhigh→high, which the vLLM qwen menu {xhigh,medium,low} rejects with 400 —
  triage row #17, APEX-AYL.83). Breach: 400 on the first post-switch turn.
  Collapse: effort-lock (user cannot pick any effort on the target).
  Detection: row #17; cell at-effort-2hop (2-hop projection).
- S5 SUBAGENTS ACROSS WIRES. Parent on wire A, child pool on wire B (or
  v2 spawn whose child rides a different family). Breach: child 400s on
  inherited context; collapse: parent blocks on dead child (ws9-s07
  interrupt semantics). Detection: child-turn 400 signature + pool
  attribution (triage T2 hot mode); cell AT-SPAWN-XWIRE (eb21336) +
  cross-model pool cell OWEED (harness-map §2.4: expressible, not yet a
  dedicated live cell).
- S6 RESUME ACROSS THE BOUNDARY. Boot-time resume onto a different
  model/wire than the session's minting wire; legacy-format resume; alias
  switch. Breach: replay 400 on the first resumed turn. Collapse: session
  unrecoverable (operator forced to start fresh — the 01a09be2 class).
  Detection: resume-turn 400 + the empty-id F3; cells ws9-s11, ws9-s12,
  xw-vlq-vlg (the UNPROVEN same-vendor flip — r3 FAIL x2 documented as the
  .68 finding).
- S7 PROXY DRIFT. The deployed image is not branch-reproducible (KE-2 #1):
  a version/config change between sweeps silently rewrites the bridge
  (1.90↔1.93 = 916-line delta incl. per-provider web_search_options drop,
  Responses-API-only tool-type drop, _resolve_file_id). Breach: a PASSING
  cell fails with a shape the catalog has no row for. Detection: the
  gate key (X-Litellm-Version + proxy rev + config sha256:12) mismatching
  the last green sweep = NO VERDICT, not a pass.
- S8 BUDGET/LATENCY COLLAPSE. 424 BPS plateau (bps-plateau carried bead),
  qwen budget-trap (OQ-3: empty text + max_tokens), context-window seam
  (operator preference: 256k tight across all models regardless of the
  model's advertised max — per-model context_window config audit owed).
  Breach: est_calls/BPS scoring outside band; collapse: watchdog kill mid-
  compaction (the 01a09be2 over-capacity origin). Detection: scoring props
  + watchdog_s; cell owed (live BPS gate — harness-map §10.4).
- S9 AUTH/IDENTITY. Provider-var leakage into the hermetic home,
  GROK_AUTH_EXPIRED bypass, raw-key echo into logs/captures (A6-F2
  session_setup leak is DEBUG-level — grep before citing), x-litellm-tags
  East-US-2 tag riding where untagged is required (the glm-5.2 401 class).
  Breach: key sha256:12 appears in any artifact ≠ the masked form.
  Collapse: session auth brick (GROK_AUTH_EXPIRED=1 + no refresh = the
  designed L1 contract; anything else is a bug). Detection: M-4 key sweep
  (sentinel absent + exit 3 non-vacuous) on every artifact; wiretap canary
  selftest.

## 4. Rules of engagement (ROE for the exercise)

1. store=false on every live capture — the proxy rejects store=true.
2. Raw-key sweep 0 on every artifact (sha256:12 only).
3. Never edit expected.json/pins/mirrors to fit (STOP-1) — record class-1
   FPs (known: bearer-prose at patch lines 1548/1892 in the vxm_az
   fixtures).
4. Binary-of-record on every live run (sha12 in the report header).
5. Gate key logged on every sweep (S7); mismatch = no verdict.
6. Single-writer per file family (run.py=.22 lane, xwfix=.70 family,
   triage=census lane, registry=coordinator, exfil=coordinator).
7. SIGKILL mid-run = machine artifact (≤1 re-run, never a RED/GREEN).
8. RED-EXPECTED cells: a DIFFERENT failure shape than expected =
   report-and-stop (the ratchet pin is exact).

## 5. Sweep cadence

- Per binary (ship gate): L3 full matrix (.72/.65 row) + wstream frontier
  + run-matrix pass. This is the "cross-wire switches are settled" row.
- Per lane (L2): every code arc lands its own cells (the .78-B 12 rows,
  the at-* 5 cells, the xwfix flip cells) before close.
- Event-driven: a NEW model row on the proxy (new effort menu / supports_*
  flag / api_backend) = a new cell in the same arc that touches it.
- Standing: triage census over any dogfood capture (operator interactive
  runs feed the same catalog as the gates).

## 6. Cross-stream delta appendix

vs stream-B `adversarial-brief-20260918.md` (apex-6vu, closed):
- CONVERGE (independent, same gap from two directions): their proposed
  cases use exactly this stream's SCHEMA-EXT-1 op set — `persist_resume`
  (brief L35, XW-RESUME-CROSSFAM), `seed_context` (L80, COMPACT-ACP-EXPLICIT),
  `set_effort` (L122, XW-EFFORT-MID). Both streams independently hit the
  schema wall (KE-3 N2 for their side; §10.1 for this side). The op set is
  settled by convergence; the RECON guards remain (set_effort ACP surface;
  seed_context expressibility).
- DELTA-1: S3 orphaned-tool_result (the UNVERIFIED reverse direction of the
  .48 repair_dangling_tool_calls) has no counterpart case in their brief —
  owed cell in KILOECHO-CELLS-1.
- DELTA-2: their SUB-COMMS-TRUST + SUB-DEAD-SEAT cases are BLOCKED by
  apex-ayl.66 COMMS-TRUST-FIX-1 (trusted launch channel; fresh-context
  children currently refuse coordinator-authorized work) — file with
  blocked-by .66, or they RED on trust, not on wire.
- DELTA-3: S7 (proxy/image drift, gate-key no-verdict) vs their W9 (403
  route-blocked, .78-B): different axes — route allowlist vs image
  version; CATALOG-DRIFT-1 (apex-6mz P3) covers the catalog axis only.

---
Raw-key sweep: 0.
— end —
