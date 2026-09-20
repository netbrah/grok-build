# KILOECHO — REPO PROMOTION PLAN (stream-A independent)

date: 2026-09-18 · author: coordinator (stream A) · bead: candidate, apex-bqm
family (dedupe owed) · scope: what goes into the grok-build fork, what stays
local, the .gitignore policy, CI shape, crate-ization boundary · status:
FINAL v1 (plan — no implementation claims)

## 1. Current tracked state (as-read, git ls-files)

- `smoke/redteam/` — 84 files tracked (run.py, case.schema.json, cases/*.json,
  golden-smoke, gen-matrix.py, grok-dogfood.v2, test_run*.py).
- `smoke/xwfix/` — 38 files tracked (README + cells/ corpus).
- `smoke/wstream/` — 16 files tracked (README, manifest, run.py, configs/).
- `smoke/triage/` — 4 files (grok-triage, signatures.json, install.sh,
  test_grok_triage.py).
- `smoke/wiretap/wiretap.py` + the three runner .sh — tracked.

So the promotion question is NOT "move the tree into the repo" — the tree is
already the repo. The question is: which REMAINING artifacts are corpus (commit)
vs evidence (keep local), plus the ignore policy and the crate-ization seam.

## 2. Commit-vs-local decisions (per path)

| Path                              | Decision | Rationale |
|-----------------------------------|----------|-----------|
| smoke/redteam/manifest.json (61 cells, UNTRACKED) | COMMIT | It is the rolling adjudication record (sealed report refs, binary sha12, RULING refs). The xwfix manifest is tracked and does the same job — consistency requires the redteam manifest tracked too. It is append-mostly; mutation is the discipline, not the file. |
| smoke/redteam/kiloecho/ (all docs) | COMMIT | Campaign corpus + the two-overwatch record. Docs, not captures. |
| smoke/redteam/report-*/ (r1/r2/r3/r3r/green/at1) | LOCAL (ignore) | Sealed run evidence incl. request bodies. Sweep-0 clean, but operational data, not corpus. Reproducible by re-running. |
| smoke/wstream/report/             | LOCAL (ignore) | Same: per-run derived configs + captures. |
| smoke/**/__pycache__/             | LOCAL (ignore) | Build artifact. |
| smoke/**/derived-config.toml      | LOCAL (ignore) | Per-run materialization (§3 gate design). The DECLARED patches live in the cell json (committed). |
| smoke/redteam/cases/*.json        | COMMIT (tracked) | The contract corpus. Disabled cases: the rt-m6.json gitignore pattern shows the precedent (case disabled + excluded). Keep: a disabled-but-tracked case documents the history; use gitignore only when the case carries stale/leaky content. |
| crates/.../fixtures/projection_*/ | COMMIT | Rust-side golden fixtures (the .71 pattern). |
| ~/.grok/dogfood/* captures        | OUT OF REPO (home) | The launcher already writes under home by design; never in the worktree. |

## 3. .gitignore policy (proposed additions — none exist yet)

```
smoke/redteam/report-*/
smoke/redteam/report/            # (already present, L7)
smoke/wstream/report/
smoke/**/__pycache__/
smoke/**/derived-config.toml
*.pyc
```
The operator's "gitignore the jsons" reads as: capture jsons YES (they live
under report-*/ — covered by the dir rules), case/corpus jsons NO (they are
the contract). If any future per-cell capture lands outside report-*/ dirs,
add an explicit path — do not broaden the rule to *.json.

## 4. CI shape (two jobs, strict separation)

- CI-JOB-1 (offline, no key, no binary, no pip):
  `python3 smoke/redteam/run.py --selftest`
  + `python3 smoke/triage/test_grok_triage.py`
  stdlib-only by construction — runs on any runner, including on-prem
  runners without proxy reach. This is the merge gate for any
  runner/schema/case/fixture change.
- CI-JOB-2 (Rust, no key): `cargo test -p xai-grok-sampling-types`
  (projection fixture tests — self-contained SHA-256, no network) + the
  classifier unit tests (triage-row → recorded error body, §5).
- L2/L3 live layers: NEVER in CI (they need the ambient proxy key + the
  live proxy). They are operator/fleet jobs (the .78-B lane, the .72 sweep).
  The gate-key discipline (unified-gate-design §3.5) is what makes a
  non-CI layer auditable: every live report carries binary sha12 +
  derived-config sha256:12 + X-Litellm-Version + proxy rev.

## 5. Crate-ization boundary (what becomes a Rust unit test)

- xwfix cells → Rust fixture tests. Pattern exists in-tree:
  `crates/codegen/xai-grok-sampling-types/src/conversation/fixtures/
  projection_x71/` + projection_tests.rs (the .71 GREEN cut: JSON fixtures,
  self-contained SHA-256, no Cargo.toml changes). Extend one fixture dir per
  xwfix cell; one test per invariant (no-empty-id, no-foreign-encrypted,
  pairing-integrity, carrier-survival, byte-identical-non-projected).
- triage signatures → classifier unit tests: each signatures.json pattern
  row (F1-F5, F5-ext, F7, F8, family-9, + the 14 PROPOSED gaps as they
  close) → a test that the recorded error body classifies to the expected
  family and RetryDecision. This pins the reactive net against drift —
  the exact regression class the .58 incident was.
- Driver ops / schema / case-set → stay PYTHON (run.py selftest). The
  driver is python by design (Rust-free); do not port it.
- Shared source: xwfix cell.json is canonical; the Rust fixture view is
  generated from it (extend gen-matrix.py's role) — hand-edits to either
  view without the generator = STOP (unified-gate-design §6).

## 6. Ownership boundary with the .71 lane

- The sibling session is mid-cut on the .71 GREEN (projection core +
  shell-side seam). Nothing in this plan touches its files.
- Exfil coordination: promotion commits (manifest.json, kiloecho/,
  .gitignore additions) ride the NEXT exfil pass after the .71 GREEN
  exfil chain completes (gate → commit → tag → push fork/mirror → ls-remote
  both). Single-writer: coordinator exfil only; origin (xai-org) is never
  pushed.
- The .gitignore change is repo-root-scoped: apply in the same commit as
  the first report-*/ exclusion takes effect, so no untracked-evidence
  window exists where a stray `git add -A` could sweep a capture.

## 7. Cross-stream delta appendix

vs stream-B docs (harness-map / unified-harness-design / adversarial-brief /
wire-topology-matrix): NO promotion/.gitignore/tracked-state section exists
in the stream-B kiloecho set — the repo-hygiene axis (commit-vs-local,
.gitignore policy, CI shape, manifest.json tracking) is stream-A-original
in this directory. No alignment target, no conflict. The one adjacency:
stream-B harness-map §suite-12 states the mirror discipline ("byte-equal to
the smoke corpus") which this plan's .gitignore policy must NOT break — the
tracked corpus (cases/, xwfix/cells/, fixtures, kiloecho/) stays fully
committed; only per-run evidence (report-*/, derived-config.toml, pyc) is
ignored, so the mirror has everything it needs to verify.

---
Raw-key sweep: 0.
— end —
