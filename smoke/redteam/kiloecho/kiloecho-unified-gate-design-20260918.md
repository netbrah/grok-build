# KILOECHO — UNIFIED GATE DESIGN (stream-A independent)

date: 2026-09-18 · author: coordinator (stream A) · bead: candidate, apex-bqm
HARNESS-UNIFY-1 family (dedupe owed) · scope: design of ONE unified adversarial
red-team gate across ndjson harness + ACP + dogfood launcher · status: FINAL v1
(design — no implementation claims)

Companion: kiloecho-harness-map-20260918.md (inventory this design consumes).
STALE-BY-DRIFT: .71 GREEN in flight; schema/manifest cites as-read 2026-09-18 ~22:1xZ.

## 1. Problem + design goals

Today the red-team surface is five partially-overlapping assets (run-smoke /
run-matrix / run-compaction-smoke / redteam-run.py / wstream / xwfix / triage).
Each layer answers a different question; nothing composes them into a single
verdict. Operator asks: one gate, comprehensive (multi-turn → model switch →
/compact → MCP → cross-model subagents, across every wire), invokable as a
ship-gate, and crate-izable at unit-test level.

Goals:
- G1 ONE case model: a single case schema is the source of truth for every
  scripted scenario, whether it runs offline (selftest), against fixtures
  (unit), or live (proxy).
- G2 ONE config contract: the frozen per-run derived-config.toml (wstream
  mechanism) becomes the single config-derivation contract for all live layers.
- G3 LAYERED VERDICT: L0 offline → L1 fixtures → L2 live single-cell → L3
  matrix sweep. Each layer's green is a precondition of the next; the ship
  gate is the conjunction.
- G4 REGRESSION PROTECTION that outranks convenience: byte-pinned golden
  fixtures, sealed report dirs, binary-of-record sha12 on every live run,
  raw-key sweep 0, class-1 FP logging (STOP-1).
- G5 ADVERSARIAL BY CONSTRUCTION: RED-EXPECTED cells are first-class
  (expected_red=true, ratcheted to green when the fix lands); unclassified
  wire shapes trigger report-and-stop, not tolerance.

## 2. Unified case model

### 2.1 Existing driver ops (run.py:2908-3100)
turn, kill, switch, switch_model, compact, idle, recon_note.

### 2.2 Wire-row assertion ops (run.py:1358-1416)
count, absent (absent_ok), present, eq, ne, text_contains, tools_absent,
tools_present — asserted over wirecap rows (rows a,b selector).

### 2.3 Required schema extension (the blocking prerequisite — KE-3 N2)
The adversarial brief's Surfaces 2+6 cases currently FAIL case.schema.json
validation (additionalProperties:false on steps). Design decision: extend the
enum and the step props explicitly, then KEEP additionalProperties:false —
strictness is the feature; do not open a bag of props.

- New ops (each with a driver implementation + a selftest offline case):
  - `persist_resume` — persist the in-run session state, resume it in-step,
    assert the resumed view. Closes the mid-run resume gap (harness-map
    §10.3). Case-level --resume stays for boot-time resume.
  - `set_effort` — in-run effort switch (the /effort seam). RECON-REQUIRED
    before implementation: confirm the ACP surface for effort in
    acp_session_impl (does the ACP channel expose an effort/mode set, or is
    effort config-only headless?). If config-only, implement as a
    config_patch + restart-turn rather than an ACP op — do NOT fake an ACP
    op that doesn't exist.
  - `seed_context` — load a recorded history (file or inline) as the session
    starting state, without replaying turns. NOTE: case-level `seed` and
    `config_patch` props already exist (schema top-level) — seed_context may
    be expressible TODAY via seed for file-backed histories; file the op only
    if inline/record-ref seeding is unexpressible. Recon before cut.
- New step props (each justified, each selftest-pinned):
  - `effort` (with set_effort), `label` (report readability),
    `target_model` (with switch_model — makes the switch explicit per step
    instead of step-model-inference), `tokens` (BPS/latency scoring input
    for the live BPS gate).
- Validation path: the dual-path selftest gate (jsonschema draft-07 OR the
  hand-rolled keyword subset, run.py --selftest) must cover every new
  keyword in BOTH paths (the hand-rolled subset is the on-prem path — it
  silently under-validates if not extended in lockstep).

### 2.4 Capability × mechanism matrix (comprehensiveness audit)

| Capability            | Mechanism today                              | Status      |
|-----------------------|----------------------------------------------|-------------|
| multi-turn            | turn op ×N                                   | ✓           |
| mid-session /model    | switch_model op (+config_patch, derived cfg) | ✓ (wire-form pin pending) |
| /compact (triggered)  | compact op                                   | ✓           |
| remote compaction v2  | run-compaction-smoke.sh (L1c)                | ✓ gated D-ENC |
| MCP tool calls        | tool_calls / mcp_calls case props            | ✓ (t21 ×5 models) |
| subagents (same-model)| agents_json pool + turn                      | ✓ (c-subagent, ws9-s06) |
| subagents (cross-model)| agents_json per-agent "model" field        | ✓ expressible (pattern: run-smoke.sh c-subagent) — NO dedicated live cell yet |
| v2-spawn × cross-wire | agents_json + switch cells                   | ✓ AT-SPAWN-XWIRE (eb21336) |
| resume (boot)         | case-level --resume                          | ✓ (ws9-s11 legacy-resume) |
| resume (mid-run)      | —                                            | ✗ persist_resume op |
| effort switch (mid-run)| —                                           | ✗ set_effort op (RECON first) |
| interrupt / kill      | kill + expect_kill                           | ✓ (ws9-s07) |
| async user msg        | turn while child runs                        | ✓ (ws9-s10) |
| alias switch          | switch op (provider alias, not model)        | ✓ (ws9-s12) |

## 3. Config-derivation contract (the frozen .config.toml)

Standardize the wstream mechanism as THE contract for every live layer:
1. Base = live `~/.grok/config.toml` (never store full copies in the tree).
2. Cell declares a patch set: `api_backend` + any campaign flags
   (strict_responses_input, model_family, context_window, effort defaults).
3. Materialized per-run as `derived-config.toml` in the cell's report dir;
   the report header logs its sha256:12 + the base config's mtime/sha256:12.
4. Hermetic home: the run's GROK_HOME is a tempdir seeded with the derived
   config (run-compaction-smoke.sh pattern); store=false on all captures.
5. Gate key (KE-2 §3 ratchet): (X-Litellm-Version read LIVE from response
   headers, proxy rev, config sha256:12) — results are only comparable
   within a gate key; the deployed image is not branch-reproducible
   (KE-2 finding #1), so the version must be read at gate time, never
   assumed.

## 4. Gate layers

L0 — OFFLINE (no proxy, no binary, no key):
- `run.py --selftest`: dual-path schema gate + in-tree case-contract gate +
  test_run.py suite (step-machine desync replay, 424 BPS init-budget sims,
  16-T surface) + the triage catalog test (test_grok_triage.py, plain-assert).
- Predicate: exit 0. This is the merge gate for ANY runner/schema/fixture
  change. Runs in CI-shaped form: one command, hermetic.

L1 — FIXTURE UNIT (no proxy; binary optional):
- xwfix golden corpus (apex-ayl.70): per-cell TDD RED-first against the
  .71 projector (storage form today; wire form once the switch_model op
  capture lands). Crate-ization per §6.
- Predicate: every cell's red_tests green at the current binary; invariants
  1-5 hold per projection (no empty id, no foreign encrypted_content,
  pairing integrity, carrier survival, byte-identical non-projected).

L2 — LIVE SINGLE-CELL (proxy, binary, wirecap ON):
- xwfix live cells (flip cells: vxm-az, az-vlq, vxm-vlg, ...) · wstream 12
  cells · ws9 .78 ship-gate 12 rows · at-* adversarial 5 cells ·
  run-smoke 4 cases · run-compaction-smoke (D-ENC-gated t3).
- Every run: dogfood wirecap mode, binary sha12 logged, derived-config
  sha256:12 logged, report in a NEW sealed dir (report-<lane>-<UTC-ts>/ —
  sealed dirs are never rewritten).
- Predicate: invariants pass (MUST-PASS pins); observations recorded
  (not gated); RED-EXPECTED cells match their expected_red signature
  exactly (a different failure shape than expected = report-and-stop).

L3 — MATRIX SWEEP (the ship-gate row):
- .72/.65 full 15-cell cross-wire matrix + wstream frontier 12 +
  run-matrix.sh model-matrix pass.
- Predicate: 0 unclassified wire-error shapes (census over all captures;
  every 400/5xx matches a signatures.json row), 0 invariants broken,
  sweep 0 raw keys, A6 name-compare shows 0 NEW failing names vs baseline.

## 5. Ship-gate predicate (explicit)

SHIPPED = L0 green
        ∧ L1 all xwfix cells green (per-cell T0-T3 projection, invariants 1-5)
        ∧ L2 invariants pass on every live cell (observations logged, not gated)
        ∧ L3 sweep: 0 unclassified + 0 invariants broken + sweep 0 + A6-clean
        ∧ binary-of-record: the binary sha12 under test is the release binary
          (not a mid-cut debug build — the f0f2455a6ca2 pattern)
        ∧ release gate: canonical 6-pkg, 0 new names vs A6, .73 standing
          (--skip image_strip_tests) unless .73 closed.

RED-EXPECTED ratchet: cells with expected_red=true are NOT failures — they
are pins on the pre-fix shape. When the fix lands (e.g., .71 GREEN for
xw-vxm-vlq / xw-vxm-vlg-guard flips), the cell's expected flips and the
storage diff at switch moves from "idx2=reasoning strip" to the PROJECTED
shape (T0 no-op on same-boundary; az-az stays byte-identical = the T0
proof). Manifest status strings are the adjudication record — append-only.

## 6. Crate-ization (unit level)

Two ratchets, one corpus:
- RUST side (logic): xwfix cell fixtures promoted into-crate. The pattern
  ALREADY EXISTS: `crates/codegen/xai-grok-sampling-types/src/conversation/
  fixtures/projection_x71/` + projection_tests.rs (JSON fixtures,
  self-contained SHA-256, no extra deps — per the .71 GREEN cut). Extend:
  one fixture dir per xwfix cell (storage form now; wire form post-op),
  one test per invariant. Triage: each signatures.json pattern → a
  classifier unit test against the recorded error body (the F1-F9
  families + .74 F5-ext/F7/F8 + .82 family-9 get Rust-side pins).
- PYTHON side (driver/contract): run.py selftest stays the contract ratchet
  (schema, case-set, step-machine). It never imports the Rust tree.
- Shared source: the xwfix cell.json is the canonical record; a generator
  (extend gen-matrix.py's role) materializes the Rust fixture view from it.
  Hand-editing either view without the generator = STOP.

## 7. Triage integration

Every L2/L3 sweep feeds `census` over its captures. Unknown shape →
STOP-1: triage row (PROPOSED slug SIG-<class>-<n>) + xwfix cell (synthetic
recipe) + bead, same session. Catalog mutation goes through the install
contract (explicit --catalog outside ~/.grok; re-install re-sync; M-1
fail-closed guard on the under-home default).

## 8. Ownership + single-writer

- run.py + schema: the .22 lane (case-op merges; switch_model_op.patch
  DRAFT lives there).
- xwfix corpus: the .70 family.
- triage catalog: the census-owning lane per T4.x annotations.
- registry (kiloecho README): coordinator, one guarded single-writer pass.
- This design: stream-A doc; stream-B's unified-harness-design is an
  INDEPENDENT stream (two-overwatch); deltas recorded, not aligned in-seat.

## 9. Risks

- R1 INFRA: on-prem qwen transport stream deaths (harness-map §10.5) —
  gates that require long live sessions on qwen seats are exposed; the
  operator's vLLM-side investigation is the unblock. Mitigation: gates run
  on the harness binary against the proxy (not on agent seats), so the
  gate itself is unaffected — only seat-driven authoring is.
- R2 D-ENC: remote compaction t3 gated on proxy /responses session affinity
  (3-Azure-region LB). Until it lands, the compaction ship-gate row is
  t1/t2-only and the gate MUST say so (no silent pass).
- R3 PROXY DRIFT: deployed image not branch-reproducible (KE-2 #1) — the
  gate key (§3.5) makes drift visible; a gate-key mismatch = no verdict,
  not a pass.
- R4 SCHEMA STRICTNESS: extending the schema without extending the
  hand-rolled selftest path in lockstep = silent under-validation on
  on-prem (no jsonschema). Selftest must pin both paths per keyword.

## 10. Cross-stream delta appendix

vs stream-B `unified-harness-design-20260918.md` (layered gate, ONE case
registry):
- CONVERGE (independent): layered-gate + one-case-registry position;
  selftest as the offline layer; "formalize, don't invent" rationale.
- DELTA-1 (numbering): their L0 = crate-unit, L1 = selftest; this design's
  L0 = selftest, L1 = fixtures. Same layers, different axis labels — the
  registry consolidation should pick ONE axis (recommend: order by
  resource cost: selftest first, crate-unit second, both key-free).
- DELTA-2 (material): their ratchet condition 2 (wire-topology §6 L63:
  "the ratchet must be LIVE integration tests against the DEPLOYED proxy,
  supplemented by config asserts — NOT stock-checkout unit tests alone")
  rests on the M-1 "proxy-image delta" attribution — DISPROVEN first-hand
  2026-09-18 (stock v1.90.0, utils.py:16-57 + both handler call sites;
  checkout HEAD = tag). Stock-checkout unit test + config assert IS feasible
  for the M-1 class; the 3-tuple gate key (§3.5) still bounds live layers.
- DELTA-3: their design does not carry the gate key (X-Litellm-Version live
  read + proxy rev + config sha256:12) — required because the deployed image
  is not branch-reproducible (KE-2 finding #1).
- DELTA-4: SCHEMA-EXT RECON guards (set_effort ACP-surface check;
  seed_context expressibility via case-level seed+config_patch) are this
  stream's addition; stream-B's adversarial cases assume the ops exist.

---
Raw-key sweep: 0.
— end —
