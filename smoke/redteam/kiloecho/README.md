# kiloecho — in-repo wire-intel & harness-design home

Established 2026-09-18 (apex-v2-grok-build campaign, overwatch session). Durable,
committed home for cross-wire topology intel, error-class catalog, red-team campaign
plans, and harness-design docs. Rides the established `smoke/` pathspec family
(exfil + coordinator commit flow).

## Conventions
- Naming: `<topic>-YYYYMMDD.md`. One topic per file.
- Every doc carries: date, author (seat/lane), bead id, sources (paths + shas), status
  (LIVE / DRAFT / SUPERSEDED / STALE-BY-DRIFT).
- No raw keys: key references are `sha256:12` only. Current campaign key sha12
  `9f3f56a263da`.
- Docs cite code by `path:line` at the HEAD they were read at. When HEAD moves past a
  citation, mark the doc STALE-BY-DRIFT — do not silently re-edit settled intel.
- Layering: session ledger (`~/Projects/upstream/grok/plans/ledger.md`, outside this
  worktree) remains the LIVE SoT for campaign state; per-lane SDD/plan mds remain the
  per-lane SoT. kiloecho is the durable cross-lane consolidation layer — it consolidates,
  it does not replace an SDD.

## Registry

| doc | bead | status | one-liner |
|---|---|---|---|
| `71-handoff-brief-20260918.md` | apex-ayl.71 | LIVE | Handoff to the .71 long-pole: live cut state (first-hand verified), W2 protocol, R3 RED-CLEARED contract, re-pin table, gate, what ships |
| `harness-map-20260918.md` | apex-bqm (CLOSED) | LANDED (qwen seat, recovery-after-nudge; overwatch-reviewed 2026-09-18) | Full inventory (5 py suites + 3 zsh runners + dogfood launcher + L0 Rust tier); plans/ Intel Registry (324 md / 66k lines); 3 live probes (dogfood wiring, wirecap E2E GREEN, ACP stdio GREEN) |
| `unified-harness-design-20260918.md` | apex-bqm (CLOSED) | LANDED (qwen seat, recovery-after-nudge; overwatch-reviewed 2026-09-18) | Layered gate (L0/L1/L2/L3) over ONE case registry + ONE verdict engine; new ops (resume, mcp_call, subagent w/ model-at-spawn, compact remote-v2 D-ENC-gated, proxy:none hermetic); resp_body + resp_stream pin kinds; mint protocol; promotion + CI shape; .71 L2 re-verify subsumed as first L2 wave |
| `wire-topology-matrix-20260918.md` | apex-6vu (CLOSED) | LANDED (glm2 seat; overwatch-reviewed 2026-09-18) | family × wire × backend matrix; ratchet = YES-IN-PART (4 conditions); proxy blind to switch semantics; 6 live probes P1-P6 |
| `error-class-audit-20260918.md` | apex-6vu (CLOSED) | LANDED (glm2 seat; overwatch-reviewed 2026-09-18) | 15 wire classes W1-W15 + infra; 5 PROPOSED / 10 MISSING triage rows; BRICK gap = highest severity |
| `adversarial-brief-20260918.md` | apex-6vu (CLOSED) | LANDED (glm2 seat; overwatch-reviewed 2026-09-18) | 6 attack surfaces w/ case.json proposals; top-5 ranked; best discriminator `xw-vxm-az-live-replay` |
| `litellm-transform-matrix.md` | (sibling KE-2 lane) | LANDED (xw_ke2_litellm_qwen, parallel overwatch session, 23:10Z) | litellm 1.90 transformation matrix + ratchet key = (version, proxy-branch, config digest); X-Litellm-Version header read at gate time; M-1 = STOCK flag-driven clamp (see wire-topology-matrix §7 addendum) |

## Referenced SoT (plans/ — outside this worktree; pointer, never copy)

Per-lane and campaign SoTs live in `~/Projects/upstream/grok/plans/`. kiloecho docs
POINT at these; they are not copied in (drift rule). Canonical pointers:

| path | what it is |
|---|---|
| `ledger.md` | live campaign ledger (append-only SoT) |
| `smoke-harness-spec.md` | harness layering spec |
| `HT-1-redteam-harness-spec.md` | redteam suite spec |
| `xwire/sdd-71-projector.md` | .71 SDD (T0-T3 ladder) |
| `xwire/review-gates-q3.md` | gate SoT (canonical 6-pkg command, A6 name-by-name, sweeps) |
| `xwire/crosswire-matrix.md` | wire-topology matrix (living) — primary source for the kiloecho matrix doc |
| `xwire/golden-engine-capability-glm-20260918.md` | golden engine capability map (451L): request-body pin YES / resp+stream NO / storage separate; wire-form pin DORMANT (no `expected_wire.json` fixtures; op hook hardcodes normalize `[]` + first-by-filename) |
| `xwire/w2-runbook-20260918.md` | W2 runbook (.71) |
| `provenance/deliverable-map-20260918.md` | unifying deliverable/provenance audit map |
| `provenance/donors.md` | upstream donor provenance (ratchet rows) |

## Layering ruling (2026-09-18, operator-asked)

Do NOT retire `plans/xwire/` or `plans/provenance/`.
- `plans/` = per-lane session SoT (SDDs, adjudications, runbooks) — stays where lanes write.
- `plans/provenance/` = supply-chain audit layer (donors, parity, deliverable map) — different mission.
- `kiloecho/` = the in-repo consolidated LIVING FACTS (matrix, error catalog, adversarial
  campaign, harness design) + pointer registry. Copy = drift; retire = context loss;
  extract-and-point is the rule. Docs born in-repo live natively here.
