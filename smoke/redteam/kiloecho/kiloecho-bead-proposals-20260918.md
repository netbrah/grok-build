# KILOECHO — BEAD PROPOSALS (stream-A independent, deduped)

date: 2026-09-18/19 · author: coordinator (stream A) · bead: this doc IS the
dedupe record (candidates below; FILING is coordinator single-writer lane,
not done here — read-only bd queries per ROE) · scope: bead candidates from
the stream-A kiloecho docs + KE-2 ratchet, deduped against the live graph
(354 issues: 144 open / 32 in-progress as-read) · status: FINAL v1

## 1. Dedupe ground truth (as-read via `bd list --all` / `bd show`)

- `apex-bqm` HARNESS-UNIFY-1 = **CLOSED** (stream B; deliverables =
  harness-map-20260918.md + unified-harness-design-20260918.md in kiloecho/).
- `apex-6vu` KILOECHO-1 = **CLOSED** (stream B; wire-topology + error-class +
  adversarial consolidation).
- `apex-8lh` TRIAGE-PROMOTE-1 = **OPEN P2** (created 2026-09-19, sibling
  session write-operator mode): promotes W6-W15 (rows #18-#27) from
  error-class-audit §4 into signatures.json.
- apex-ayl open/IN_PROGRESS core: `.36` (full-catalog matrix sweep, OPEN) ·
  `.66` COMMS-TRUST-FIX-1 (OPEN) · `.69` XW-EMPTYID-1 (OPEN) · `.70`
  XW-FIXTURES-1 (IN_PROGRESS — includes the switch_model case op + wirecap
  protocol + per-cell byte-pinned post-switch request input) · `.71`
  XW-PROJECT-1 (IN_PROGRESS) · `.72` XW-MATRIX-1 (OPEN P3 — the L3 sweep /
  ship-gate row; >=12 pairs, 6 families) · `.73` IMAGESTRIP-HANG-1 (OPEN) ·
  `.83` XW-QWEN-EFFORT-CLAMP-1 (OPEN P3 bug — the M-1 bead) · `.85`
  CONFIG-PARAMS-1 (OPEN — model-param completeness map incl.
  context_window row-intent) · `.86` PROACTIVE-ULTRA-1 (OPEN).
  Closed and relevant: .22 (smoke matrix automation — schema/validator/
  runner), .58 (XSWITCH-1), .59 EFFORT-SEAM-1, .60 MODELCONFIG-AUDIT-1,
  .74 (orphan/brick fix), .75 (affinity policy), .78 (WS9-SCENARIOS-1),
  .79 (XW-JIG-1 — switch-op adjudication + golden wire-assert kind +
  manifest/tag ratchet discipline), .81, .82.
- WS9 family (stream B, separate lane — do NOT double-file): apex-7xr
  WS9-PROXY-200EMPTY-1, apex-j3x WS9-PREFIRE-OBS-1, apex-3b0
  WS9-RUNNER-ERRATA-1 (all OPEN).
- Also OPEN and adjacent: apex-6mz CATALOG-DRIFT-1 (P3, one-shot proxy
  catalog drift check), apex-93d CATALOG-HYDRATE-1, apex-8kp
  TOOLRES-PERSIST-1, apex-9oj TOOLRES-XWIRE-1, apex-35d INGRESS77-DEBT-1.

## 2. Candidates (proposed — filing awaits operator GO)

### P1 — SCHEMA-EXT-1 (file under apex-ayl, P1)
case.schema.json op + step-prop extension for the adversarial surfaces.
- Scope: ops `persist_resume` (genuine gap), `set_effort` (RECON FIRST:
  confirm the ACP effort surface in acp_session_impl; if config-only
  headless, implement as config_patch + restart-turn, NOT a fake ACP op),
  `seed_context` (recon: case-level `seed` + `config_patch` may already
  express file-backed seeding — file the op only if unexpressible).
  Step props `effort`/`label`/`target_model`/`tokens`. KEEP
  additionalProperties:false after extension.
- Hard requirement: the hand-rolled selftest keyword subset (the on-prem
  path — jsonschema absent) extended IN LOCKSTEP per keyword, or the
  dual-path gate silently under-validates.
- Dedupe: NOT covered by .22 (closed, pre-dates the gaps), NOT .70
  (switch_model op only), NOT .78 (closed; its 12 rows parse today — the
  extension is for the NEW adversarial cases). KE-3 N2 prerequisite:
  adversarial Surfaces 2+6 FAIL validation as written.
- Acceptance: new ops have driver impl + selftest offline cases; Surfaces
  2+6 case files parse under BOTH schema paths; sweep 0.

### P2 — KILOECHO-CELLS-1 (file under apex-ayl, P2)
The three owed live cells surfaced by the doctrine (adversarial-doctrine
§3): (a) cross-model subagent pool cell (parent wire A, child pool wire B —
expressible via agents_json per-agent "model", NO dedicated live cell yet);
(b) orphaned-tool_result probe (xwfix synthetic recipe: pre_switch tool
pair, expected T3-drops the call — exercises invariant-3 re-projection;
the reverse direction of .48's repair_dangling_tool_calls is UNVERIFIED);
(c) live BPS gate cell (scoring/est_props exist + offline 424-BPS sims,
no live cell — the bps-plateau class .32 closed as runner-hardening; the
LIVE gate cell is what's owed).
- Dedupe: NOT .36 (model-catalog sweep), NOT .72 (cross-wire matrix —
  different axis: these are within-wire capability cells), NOT .78 (closed).
- Acceptance: 3 cells parse (needs SCHEMA-EXT-1 where applicable), L2 runs
  on next binary-of-record, findings feed triage per STOP-1.

### P3 — KILOECHO-A-CLOSE-1 (file under apex-ayl, P3, coordinator-owned)
Stream-A close-out, single-writer: (1) cross-stream delta appendices (4
docs, targeted — not a full re-read); (2) the shared-dir README registry
consolidation (KE-3 N3: 2 registered files absent at apex-bqm DRAFT state
— now moot since apex-bqm CLOSED, so the rows get status updates, not
reconciliation; 1 unregistered on-disk file → register or note); (3)
manifest.json + .gitignore promotion commit per kiloecho-repo-promotion
(manifest.json TRACKED — adjudication record; report-*/ + __pycache__ +
derived-config.toml IGNORED); rides the next exfil pass after the .71
GREEN exfil chain.
- Dedupe: distinct from apex-bqm (closed, stream B design) — this is
  stream-A record + repo hygiene.

### P3 (operator call) — TRANSPORT-WATCH-1
On-prem qwen transport `stream disconnected before completion` — 3
coordinator doc seats died 2026-09-18 20:3x-22:0xZ (zero artifacts each),
while a 4th seat (single-doc scope) survived 60+ min. Signature class
matches the CDX-1 documented client behavior (wiretap2 provenance: client
re-POSTs full request on reconnect — quota-leak vector). Operator's
vLLM-side investigation (stream keepalive/timeouts, litellm stream
timeout, chunked framing). Watch gate: seat completion rate. Precedent:
the W-LAT-1 / D-ENC watch entries in the carried list. FILE ONLY IF the
operator wants the watch tracked in the graph; otherwise ledger-note only.

## 3. Explicitly NOT proposed (dedupe outcomes)

- TRIAGE ROWS for G-1..G-14 (KE-2 ratchet): apex-8lh owns W6-W15 rows
  #18-#27 (sibling write-operator). ACTION: diff G-1..G-14 vs W6-W15 in
  the .8lh lane — W6 (vLLM content-400) + W7 (azure content-array max-0)
  already cover the G-7-adjacent brick shapes I verified first-hand; any
  G-gap NOT in W6-W15 extends .8lh's scope there (single-writer), no new
  bead. KE-2's "KE-3 files the 14 signature rows" handoff line is
  SUPERSEDED: .8lh is the vehicle, the diff is the work.
- M-1 / M-2 beads: .83 (XW-QWEN-EFFORT-CLAMP-1) + .20-closed cover them;
  the owed work is the triage row #17 RCA ERRATA (next exfil text fix:
  stock clamp + config absence, disproven "not in stock" claim — checkout
  HEAD = v1.90.0 tag, utils.py:16-57 + both handler call sites verified
  first-hand 2026-09-18).
- WIRE-FORM PINS: inside .70 IN_PROGRESS (switch_model op + per-cell
  byte-pinned post-switch input + wirecap protocol). No new bead.
- L3 SWEEP: .72 OPEN (the ship-gate row). No new bead.
- CONTEXT-WINDOW / model-param audit (operator's 256k-tight preference):
  .85 CONFIG-PARAMS-1 OPEN covers row-intent incl. context_window; .60
  closed the audit spike. No new bead — operator preference rides .85.
- HARNESS-UNIFY / KILOECHO family beads: apex-bqm + apex-6vu CLOSED (stream
  B). Stream-A docs stand as independent records under the two-overwatch
  policy; only KILOECHO-A-CLOSE-1 (P3 above) is owed.
- WS9 items: the WS9-* family (apex-7xr/j3x/3b0) is stream B's lane.

## 4. Filing discipline

- Single-writer: coordinator files (or defers to the operator), via
  `command bd -C $HOME/Projects/bitbucket/apex_tracking`; CHECK before
  create (siblings file in parallel — the .85/.86/.8lh entries landed
  2026-09-18/19 from other sessions while this dedupe ran).
- Every filed bead carries: scope, acceptance, blocked-by, dedupe-against
  (this doc §1-§3 as the record), and the stream-A source doc paths.
- The delta appendices (docs 1-4) must land BEFORE filing KILOECHO-A-CLOSE-1
  so the bead's acceptance includes "appendices complete."

---
Raw-key sweep: 0.
— end —
