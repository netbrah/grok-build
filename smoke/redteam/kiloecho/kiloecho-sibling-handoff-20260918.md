# KILOECHO — STREAM-A HANDOFF (for the sibling session)

date: 2026-09-18 (late) / 2026-09-19 UTC · author: coordinator (stream A) ·
audience: the sibling/overwatch session (stream B) · status: FINAL — read
this first; the five stream-A docs are the artifacts.

## 1. The artifacts (all in this directory, stream-A prefix)

1. `kiloecho-harness-map-20260918.md` — inventory + capability topology of
   the whole smoke/ stack (layered; what each layer CAN/CANNOT drive).
2. `kiloecho-unified-gate-design-20260918.md` — the unified gate: case-model
   extension, config-derivation contract, L0-L3 layers, explicit ship-gate
   predicate, crate-ization boundary.
3. `kiloecho-repo-promotion-20260918.md` — commit-vs-local decisions,
   .gitignore policy, two-job CI shape, crate-ization seam.
4. `kiloecho-adversarial-doctrine-20260918.md` — threat model, failure-class
   taxonomy, S1-S9 attack surfaces with breach/collapse/detection, ROE,
   sweep cadence.
5. `kiloecho-bead-proposals-20260918.md` — deduped bead candidates against
   the live graph (354 issues as-read) + explicit NON-proposals.

Every doc: delta appendix filled (read-only pass vs your docs), raw-key
sweep 0, path:line cites as-read at HEAD.

Context for why coordinator-authored: the two qwen doc seats (KE-1,
KE-1-R2) died to `stream disconnected before completion` with zero
artifacts (third such transport death in the campaign; see §3.9). Backoff
ROE applied: coordinator wrote the docs first-hand instead of a fourth
seat attempt.

## 2. What we may have found that your stream hasn't (the gaps)

Order = confidence × impact. Each is independently verified first-hand by
the coordinator (not seat-trust).

1. **M-1 attribution is still wrong in two of your docs.** Your
   wire-topology-matrix carries a self-correction (~L101: "the clamp is
   STOCK litellm, flag-driven") BUT §0 verdict 6 (L13), L57, and L63 still
   say "proxy-image house-patch delta, NOT in the stock v1.90.0 checkout"
   / "NOT stock-checkout unit tests alone". First-hand proof: litellm
   checkout HEAD = v1.90.0 tag exactly (rev-list count 0);
   `normalize_reasoning_effort_value` at
   `litellm/llms/anthropic/experimental_pass_through/utils.py:16-57`
   (xhigh→high when the row lacks supports_xhigh_reasoning_effort);
   callers `adapters/handler.py:389,400,407` (messages) +
   `responses_adapters/handler.py:80,84` (responses). Consequence: the
   ratchet can use a stock-checkout unit test + config assert for the M-1
   class (your design's condition 2 over-bounds it). Fix in YOUR docs
   (your lane); also owed: signatures.json row #17 RCA text carries the
   same stale claim (coordinator exfil errata).
2. **Bridge file path adjudication.** Three files share the
   "transformation" name; the claims in the campaign point at three
   different ones. Verified: (a) the REQUEST-param 15-list whitelist
   (store/include/truncation/background silently dropped, silent under
   drop_params:true) = `litellm/responses/litellm_completion_transformation/
   transformation.py:83-100` (KE-2's line cites match this file); (b)
   `completion_extras/litellm_responses_transformation/transformation.py`
   = OUTPUT direction (stored completion → responses items,
   encrypted_content round-trip) — KE-3 N6's re-anchor pointed here,
   imprecise for the whitelist claim; (c) `interactions/...` = the
   Interactions API transformer, unrelated. The row-#14 shim you cite at
   `litellm_responses_transformation/transformation.py:27` (wire-topology
   L56) is therefore the completion_extras file — verify which direction
   that cite intends before ratcheting a test against it.
3. **G-1..G-14 vs W6-W15: your .8lh bead is the vehicle; the diff is the
   work.** KE-2's ratchet (error-surface-ratchet.md §2) independently
   found 14 gaps; your TRIAGE-PROMOTE-1 (apex-8lh, W6-W15 = rows #18-#27)
   already covers the brick shapes I verified (W6 vLLM content-400, W7
   azure content-array max-0 — the `input[N].content array too long`
   variant has no row today). Diff the two lists in the .8lh lane; extend
   .8lh's scope for any G-gap not in W6-W15; no new bead.
4. **`smoke/redteam/manifest.json` is UNTRACKED.** The 61-cell adjudication
   record (sealed report refs, binary sha12, RULING refs) is not in the
   repo; the xwfix manifest (its twin) IS tracked. Plus: `report-*/`
   sealed dirs + `__pycache__` + `derived-config.toml` are neither tracked
   nor gitignored (only `smoke/redteam/report/` is ignored, L7).
   Promotion doc §2-3 has the full decision table.
5. **The gate key is missing from your design.** KE-2 finding #1: the
   deployed image is not branch-reproducible (no llm-proxy branch carries
   both the qwen row and the 1.90 pin). Your unified design has no
   (X-Litellm-Version live-read + proxy rev + config sha256:12) key —
   without it, a post-drift sweep "pass" is not a pass. My design §3.5 /
   §9-R3.
6. **Orphaned tool_result direction is unverified (S3).** .48's
   repair_dangling_tool_calls covers missing-results for live calls; the
   REVERSE (a projected T3 drop orphans its tool_result on replay) has no
   test, no cell, no classifier arm. If the target 400s with a shape
   outside the classifier families, the reactive net cannot self-heal.
   Owed: xwfix synthetic recipe + live cell.
7. **Cross-model subagent pool: expressible, never run.** agents_json
   per-agent "model" makes parent-A/child-B pools expressible (pattern in
   run-smoke.sh c-subagent) but there is no dedicated live cell (AT-SPAWN-
   XWIRE is same-family spawn × cross-wire). Owed cell.
8. **Live BPS gate: offline only.** 424-BPS sims exist (selftest) but no
   live scoring cell (the bps-plateau class closed as runner-hardening,
   .32). Owed cell.
9. **Transport death (infra lead, operator lane).** 3 qwen doc seats died
   2026-09-18 20:3x-22:0xZ local with `stream disconnected before
   completion`, zero artifacts each; a 4th (single-doc scope) survived
   60+ min. The error string is the SAME CLASS as the CDX-1 documented
   client behavior (wiretap2 provenance: hyper/reqwest client against a
   malformed/truncated chunked stream → stream ends early → client
   re-POSTs the full request, 4× quota leak). Remote path here = seat →
   llm-proxy (litellm) → on-prem vLLM. Not root-caused (needs the
   vLLM-side: stream keepalive/timeouts, litellm stream timeout, chunked
   framing). Until it is: single-doc qwen seats max one at a time, or
   coordinator-authored.
10. **Their subagent cases are blocked-by .66.** SUB-COMMS-TRUST +
    SUB-DEAD-SEAT (adversarial-brief) will RED on the trust channel, not
    on the wire, until apex-ayl.66 COMMS-TRUST-FIX-1 lands. File the cells
    with blocked-by, or annotate expected_red with the trust cause.

## 3. What is NOT stream-A's to do (lane boundaries)

- Your docs / README registry: coordinator does ONE guarded single-writer
  registry pass (KILOECHO-A-CLOSE-1), after both streams settle; you fix
  M-1 lines in YOUR docs (your lane), I do not.
- run.py / schema cuts: .22-lane ownership (SCHEMA-EXT-1 is a PROPOSAL —
  filing awaits operator GO; bead-proposals doc §4 has the discipline).
- Triaging rows: .8lh lane (sibling write-operator per its description).
- Exfil: coordinator only (gate → commit → annotated tag → push fork +
  mirror → ls-remote both; origin never).
- The .71 GREEN cut + its shell-side seam: sibling session, in flight;
  the exfil chain triggers on its landing (my lane).

## 4. Suggested sibling actions (your call)

1. Fix the three stale M-1 lines (wire-topology L13/L57/L63) — your
   design's ratchet condition 2 depends on them.
2. Re-verify your row-#14 shim cite against item 2(b)/(a) above.
3. Run the G↔W diff in the .8lh lane.
4. Absorb the gate key (item 5) into your design before the ws9 gate
   wires it.
5. Cross-check my L0-L3 axis labels vs yours (unified-gate DELTA-1) and
   pick one numbering for the consolidation.

---
Raw-key sweep: 0.
— end —
