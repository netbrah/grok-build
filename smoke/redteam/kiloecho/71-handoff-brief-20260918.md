# .71 (XW-PROJECT-1) — long-pole handoff brief

Date: 2026-09-18 (22:15Z) · From: overwatch/kiloecho (operator-gated audit session)
For: .71 lane (sibling codex session, in flight in the shared worktree)
Bead: `apex-ayl.71` · SDD: `~/Projects/upstream/grok/plans/xwire/sdd-71-projector.md` (270L)

## 0. TL;DR — what ships, in what order

1. **.70** = cross-wire corpus + re-pins (5-row table §5; promotion package
   `~/Projects/upstream/grok/plans/xwire/xw79-promotion-package-20260918.md`). .70 lane applies.
2. **.71** = this cut: proactive switch-time projector. RED → GREEN → L2 live re-verify (§3).
3. **.72** = L3 full matrix sweep, fed by R3 findings + `kiloecho/adversarial-brief-20260918.md`.
4. **.78-B ws9** = cross-cut ship gate (python3.12; s05/s06/s07/s11/s12).

## 1. World state (overwatch first-hand, 22:15Z)

- Worktree `~/Projects/upstream/wt/grok-build-responses` @ HEAD **0fc1060**
  (branch `feat/first-class-responses-catalog`).
- **EXFIL CURRENT**: `git ls-remote` (22:15Z) → fork `netbrah/grok-build`
  `refs/heads/feat/first-class-responses-catalog` = `0fc1060b3623…` = local HEAD.
- Release binary in tree: `target/release/grok-responses` sha12 **f0f2455a6ca2** =
  binary of record (also preserved at `/tmp/binary-of-record-f0f2455a6ca2`). This is the
  pre-.71 L2 baseline. (The earlier "stale 24a941bac02b on disk" observation is
  superseded — the 17:34Z rebuild landed the record binary on disk.)
- No `cargo`/`rustc` running at check (only mcp server procs). Re-verify before every
  build: `pgrep -lf 'cargo|rustc'`.
- Worktree dirty set = **your lane's work** (`projection.rs`, `projection_tests.rs`,
  `fixtures/projection_x71/`, `provider.rs` M, `conversation.rs` M) + known untracked
  (report trees, `manifest.json`, `__pycache__`, probe files, `grok/plans` mirror).
  Other lanes must not touch your dirty files. You must not touch: `cases/rt-xreplay2.json`
  (DO NOT RESURRECT), `stash@{0}` (FLT-1), report trees (evidence), sibling cuts.

## 2. Your live cut state (overwatch first-hand read — re-verify before building)

- `conversation.rs` (+5): `pub mod projection;` + `#[cfg(test)] #[path] mod
  projection_tests;` — **wiring DONE**.
- `provider.rs` (+38, tests mod): **E2 pin CUT** —
  `is_openai_family_empty_is_true_is_pinned` asserts `is_openai_family("") == true`
  (skip semantics) and that a vLLM-family (`qwen`) body still gets
  `normalize_content_types` (`input_text` → `text`). **E2 re-verify item: SATISFIED.**
  Asserted value = current .77 semantics; the pin's doc marks INGRESS-NORMALIZE-1 (.77)
  as flip-owner. If this pin FAILs on first build, that is base-drift evidence
  (STOP 1: adjudicate against source), not a patch defect.
- `projection.rs`: T0/T1/T3 ladder per item on the STORAGE form; dialect-agnostic.
  **G3 item (ruling owed, non-blocking)**: T1 id-grammar SHA-256 is implemented
  self-contained because `sha2` is a workspace dep but not a direct dep of
  `xai-grok-sampling-types` and pathspec expansion is a coordinator ruling. It is
  KAT-pinned (12/12 known-answer goldens + in-file KATs in
  `projection_tests::xw_proj_id_grammar_canonical`). Overwatch recommendation: keep the
  self-contained digest **through GREEN** (do not churn a live cut); file a post-GREEN
  micro-cut bead to swap in the one-line `sha2` dep (maintainability; FIPS-frozen
  primitive = low-risk, high-clarity).
- `projection_tests.rs`: U3's E0432 (unresolved import at :24) is **structurally
  resolved** — the import now targets the existing `projection` module. Expected first
  RED shifts from a compile E0432 to **the first failing fixture assertion**. STOP 1
  still governs the shape of that RED.
- Fixtures (`fixtures/projection_x71/`): `PROVENANCE.md` byte-mirrors verified
  (vxm-az `7919f5a536b3` / `d5f6de89e379`; az-vlq `1bb347e7b27c` / `8b71740159c9`).
  **U1 RESOLVED in the fixtures**: az-vlq minted LIVE = **53 recs (7 T1 + 46 T0)**;
  drift note recorded (SDD §8 Table 1 "49 recs: 3 T1 + 46 T0" is era-stale; live
  governs). Cell.json shas at mint: vxm-az `b6ce13571041` (HOLD vs SDD §12 note),
  az-vlq `c4b7e5c8a3c5` (.70-lane re-pinned state).
- **Declared file surface for A6** (any file outside this set = STOP):
  `projection.rs` · `projection_tests.rs` · `fixtures/projection_x71/{PROVENANCE.md,
  az_vlq_expected.json, az_vlq_pre.json, orphan_shape.json, vxm_az_expected.json,
  vxm_az_pre.json}` · `conversation.rs` (mod lines only) · `provider.rs` (E2 pin only).
- **Declared new names (A6 extension for this cut)**: `projection` (module),
  `project_switch_history`, `Boundary`, `ProjectedHistory`, `proj_items`,
  `xw_proj_id_grammar_canonical`, `is_openai_family_empty_is_true_is_pinned`.

## 3. Protocol (remaining steps)

1. `pgrep -lf 'cargo|rustc'` — confirm no sibling cargo.
2. **First build: right-reason RED.** Expected: first fixture-assertion RED (E0432 gone).
   Any other RED shape → STOP 1, adjudicate against source before touching anything.
3. GREEN cut per SDD §9 (single `proj_items` chokepoint; D2).
4. Canonical gate (`review-gates-q3.md` §1, until .73 closes):
   `CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 RUST_MIN_STACK=67108864 cargo test
   --release -p xai-grok-sampler -p xai-grok-shell -p xai-grok-sampling-types
   -p xai-chat-state -p xai-grok-config-types -p xai-grok-agent --features
   xai-grok-shell/test-support --no-fail-fast -- --skip image_strip_tests`
5. **A6 §2 name-by-name**: 8 known-name baseline (C-3 union ×5, rotating-wake ×1,
   watcher ×1, env util::hooks ×1) + the declared .71 names (§2). Any other new name =
   STOP.
6. **L2 re-verify (the ship proof)**:
   - Fresh release build; record new sha12; keep a `/tmp/binary-of-record-<sha12>` copy.
   - Re-run the R3 11-cell corpus, python3.12, **per-cell `--out`** (r3 root-seal
     overwrite lesson):
     `python3.12 smoke/redteam/run.py <case> --bin <new-binary> --out <PER-CELL-ROOT>
     --campaign-id apex-ayl.71-l2 --wave L2-<date> --mode adhoc`
   - Expected: **7 RED-EXPECTED clear** · **3 PASS guards hold** (az-az, az-vxm,
     vlq-vlg-ws) · **vlq-vlg UNPROVEN wall cell UNCHANGED** (preemptive lossy compact
     on qwen→glm family switch still fires; T0 storage golden still fails as
     documented; .68 owns; **do not re-pin the wall cell**) · zero COMP-3 storms
     (c2 ≤ 18) · zero acp_error.
   - Recon sweeps: raw-key literal = 0 files in both trees; no new 400 phrasing
     (lapsed-400 drift class must remain lapsed).
7. Exfil + pathspec commit per coordinator flow; tag per campaign convention.

## 4. R3 evidence base (governing = `smoke/redteam/report-r3r/`, 15 calls, 11/11
per-cell seals clean)

Campaign `apex-ayl.70-r3` / wave `R3-RED` · key sha12 `9f3f56a263da` (never echo;
compare by sha12 only).

- **PASS ×3** (guards — break if the projector overreaches):
  `az-az` (7.1s, c1) · `az-vxm` (13.4s, c1) · `vlq-vlg-ws` (16.5s, c2,
  wall-preemption-holds).
- **RED-EXPECTED(.71) ×7** — clear only when the projector re-keys at switch time;
  storage RED exactly as documented:
  `vxm-az` idx6-reasoning · `az-vlq` idx0-system (no storm, c2≤18, final 200) ·
  `vxm-vlg` idx2-reasoning · `vxm-vlg-guard` storage-only (exact shape) ·
  `vxm-vlq` idx2-reasoning · `vxm-vlq-v2` storage-only (exact shape) ·
  `vxm-vxg` idx2-reasoning (10/8).
- **UNPROVEN ×1**: `vlq-vlg` — qwen→glm family switch fires a preemptive lossy
  compact (2 calls = compact + turn) that consumes the seeded reasoning ride → T0
  storage golden fails. Assert-identical across r2 and r3 (stable 2/2). Per cell.json:
  "not a fixture defect". Owner = .68 lane. .71 must not re-pin the wall cell.
- **Second-assert analysis (4 cells)** — era-stale scored pins flipping the GOOD
  direction; disposition = re-pin at promotion, never weaken:
  - `vxm-az` id:'' present-pin now ABSENT + `az-vlq` encitem_ present-pin now ABSENT
    (.77 INGRESS-NORMALIZE normalizes rides pre-send; pins era-marked RE-PIN-at-
    promotion).
  - `vxm-vlg` + `vxm-vlq` `resp_status` want=[400] got 200 — class-(a) vLLM 400 brick
    **LAPSED** on deployed qwen/glm (same drift class as RULING 3 07:08Z; 20:25Z L2
    confirmed; recon surfaced no new 400 phrasing). P6 = clean-200 acceptance.

## 5. Re-pin table (.70 promotion package — exact shape; .70 lane applies)

| case | pin | old | new | reason |
|---|---|---|---|---|
| vxm-az | id:'' | present | absent | .77 INGRESS-NORMALIZE |
| az-vlq | encitem_ | present | absent | .77 INGRESS-NORMALIZE |
| vxm-vlg | last-400 resp_status | [400] | [200] | lapsed-400 drift |
| vxm-vlq | last-400 resp_status | [400] | [200] | lapsed-400 drift |
| cases/xw-vlq-vlg.json | id | XW-VLG-VLG | XW-VLQ-VLG | typo (qwen3.8-27b → glm-5.2) |

## 6. Regression-prevention assessment (operator question: is TDD+smoke+plan enough?)

**Strong.** Layers:
1. RED-first named asserts — every RED cell has a named assert (index + field + want);
   GREEN-without-projector fails the *named* assert, not "some diff".
2. One compare engine (`run.py`) — no per-case verdict special-casing;
   `expected_red` registration in `manifest.json` drives RED-EXPECTED scoring
   (exit code stays 1 — read the log line, not the rc).
3. Seals — `campaign.json` `sealed_sha256` over manifest fields + per-cell root seals;
   r3r = 11/11 clean.
4. A6 name-by-name — undeclared new names = STOP (scope-creep tripwire inside the gate).
5. Recon surfaces — non-scoring evidence (400 phrasing, call counts, wall times) make
   drift visible before it becomes a verdict.
6. TDD — crate fixtures byte-mirrored with `PROVENANCE.md` provenance; L0 crate-unit
   precedent = .74 `xw_orphan` cuts.

**Gaps (kiloecho owns, not .71)**: harness fragmentation (5 python suites + 3 zsh
runners, no committed unified gate in the main repo) · no adversarial layer (see
`kiloecho/adversarial-brief-20260918.md`) · thin cross-wire resume/subagent/compact
coverage (ws9 s05/s06/s07/s11/s12, .78-B gated).

## 7. Standing constraints

- No proxy changes. Raw key: env `CODEX_LLM_PROXY_KEY` (len 85; sha12
  `9f3f56a263da`) — compare by sha12 only, never echo.
- python3 (3.14.7) is NOT runner-legal; **python3.12** (3.12.13) is.
- STOP rules: **1** = adjudicate against source; no mirror/golden edits. **3** =
  re-derive goldens from a live cell; never edit mirrors.
- Manifest: `smoke/redteam/manifest.json` (61 cells; 8 run dirs; `expected_refs.r3` =
  the R3 contract). run.py mechanics: single-cell invocation = single-cell `--out`;
  recon kinds = non-scoring evidence surfaces.
