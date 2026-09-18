# Ratchet ① (XW-JIG-1) driver evidence — qwen DRIVER seat

Beads: apex-ayl.79 (XW-JIG-1) / apex-ayl.70 (XW-FIXTURES-1, op side).
Worktree: wt/grok-build-responses @ 4fb487f, branch feat/first-class-responses-catalog.
Lane: python only (no cargo, no network/proxy/model calls, no git commit/push/stash).

## Files touched (pathspec)
- MODIFY  smoke/redteam/case.schema.json   (kind enum += golden; golden/normalize props; step op enum += switch_model; cell/assert_form props; where + golden descriptions)
- MODIFY  smoke/redteam/run.py             (golden engine + check_wire kind; WIRE_KINDS/STEP_OPS; switch_model op: validation, headless + ACP(session/load) cell seeding, dispatch, wire hook via golden_compare; case input-wire pre-place)
- CREATE  smoke/redteam/cases/golden-smoke-01.json
- CREATE  smoke/redteam/cases/golden-smoke-01/wire/req-010.json            (REAL capture, verbatim bytes, sha256:12 cfed3352cc1c, source: smoke/redteam/report/20260913T212423Z/rt-r1/wire/req-010.json)
- CREATE  smoke/redteam/cases/golden-smoke-01/wire/golden-smoke-01-expected.json  (fixture = capture body with the 3 normalize paths nulled)
- CREATE  smoke/redteam/test_run_golden.py (18 tests, stdlib only)

Hands-off files verified untouched (mtime pre-dates this session): rt-m5.json,
rt-s2.json, all ws9-*.json, test_run.py, smoke/triage/*, smoke/wstream/*,
smoke/xwfix/*. Pathspec files were CLEAN at 4fb487f before editing
(git status --porcelain on case.schema.json + run.py = empty at start).

## Baseline (BEFORE any edit; the suite runs against another session's
uncommitted test_run.py — that is the baseline per R1E)
- baseline-test_run.txt:   `Ran 73 tests in 3.818s / FAILED (failures=1)` —
  the single failure = CaseContractTest.test_all_case_files_validate,
  ALL violations in the other session's in-flight ws9-*.json files.
- baseline-selftest.txt:   `selftest: FAIL — schema/contract gate has
  violations` — same ws9-* set only (18 NEW cases, 6 schema failures, all ws9).

## RED-1 (mechanism, pre-change) — red-1-prechange.txt
Ran 18 tests: 5 failures + 12 errors (all new-code tests red, as expected).
The decisive pair (both MUST fail pre-change — they do):
  (a) test_case_passes_schema_validation ... FAIL
      "golden-smoke-01.json assert.wire[0]: unknown kind 'golden' (known:
       field, grep, size_lt, count, resp_status, recon)"
      "schema: ['assert','wire',0,'kind']: 'golden' is not one of [...]"
  (b) test_check_wire_golden_passes ... ERROR
      ValueError: unknown wire kind: 'golden'   (run.py:1802 pre-change)

## Post-change
- post-golden-suite.txt:   `Ran 18 tests ... OK` (18/18).
- post-test_run.txt:       `Ran 73 tests ... OK` (0 failures — the other
  session's ws9 files were fixed in-tree between 21:51 and 22:11; the
  baseline failure set is a SUPERSET of the post set: no NEW failures).
- post-selftest.txt:       `selftest: PASS (73 run, 0 failures, 0 errors)`
  (schema/contract gate clean over the full set, including golden-smoke-01).
- Dual-engine check: draft07_gate + validate_case_file on
  golden-smoke-01.json with engine=jsonschema AND engine=hand: 0 errors.
- Mutation safety: `git diff --stat run.py` = 486 insertions, 2 deletions —
  the only 2 deletions are the STEP_OPS/WIRE_KINDS tuple lines (extended);
  every one of the six existing wire-kind branches is byte-identical.
  Offline regression = CaseContractTest over the full existing case set
  (73/73). A live end-to-end run of an existing case was NOT possible in
  this lane (would require model calls — forbidden); the review seats may
  want one live case as belt-and-braces.

## RED-2 (teeth, post-change) — red-2.txt
Corrupt the in-tree fixture (flip body.reasoning.summary "concise" ->
"detailed") -> full runner: exit=1 status=FAIL,
golden detail: "req-010.json: canonical body mismatch: 1 divergence(s),
first reasoning.summary — reasoning.summary: got concise want detailed",
structured diff (golden-diff-golden_body.json) written to the case report
dir (captured in red-2.txt). Restore (sha256-verified identical) ->
exit=0 status=PASS, "canonical body match (34813 B)".

## GREEN-1 — green-1.txt
Full runner (offline: zero-step headless case — no binary launch, no model
call, wiretap idle on loopback): `GOLDEN-SMOKE-01: PASS in 0.2s calls=1`,
row status=PASS, wire.golden ok=true ("canonical body match (34813 B) vs
golden-smoke-01-expected.json"), wire.count ok=true (N=1 bound holds).

## Raw-key hygiene (CODEX_LLM_PROXY_KEY sweep live — env var set)
grep -cF on every created/modified file: all 0
(case.schema.json, run.py, test_run_golden.py, cases/golden-smoke-01.json,
cases/golden-smoke-01/wire/req-010.json,
cases/golden-smoke-01/wire/golden-smoke-01-expected.json,
report-r1/fixture-backup.json). Runner's own report sweep: "redaction sweep:
0 hits".

## NITs applied (coordinator update, glm concordance)
1. NIT 1 (V1a): ACP cell seeding routes through session/load (the patch's
   FALLBACK) — the primary re-materialization is NOT implemented (actor
   loads only summary.json at session/new; no chat_history re-read at first
   prompt). Headless seeding at case start per the patch (proven ordering).
   Live ACP flow unverified in this lane (no model calls) — flagged for the
   operator/review seats.
2. NIT 2: WIREPRESENCE-1 misattribution accepted — the golden kind does NOT
   reuse any WIREPRESENCE-1 normalizer; its canonicalization is the patch's
   _norm base (json.dumps sort_keys, compact separators), order-SENSITIVE
   for lists (input[] conversation order is semantic). Pinned by
   GoldenCanonicalTest.test_key_order_insensitive_list_order_significant.
3. NIT 4+6 (R1C): `cell_diff` is NOT a wire kind (not in the schema enum,
   not in WIRE_KINDS, no check_wire branch) and there is no second wire
   comparator (no xwfix_cell_diff_wire); the switch_model op's wire hook
   calls golden_compare — one engine. Pinned by SwitchModelOpTest.
4. NIT 7: where-filters scope by headers.x-grok-session-id (verified the
   wiretap capture carries the session id in that header on every
   authenticated request — 1924 captures in the report tree); the exact
   dotted key is documented in the schema `where` description; the case
   pins it; GoldenSelectionTest proves a foreign session's same-shape
   request is excluded and that dropping the key breaks the scope bound.
5. NIT 5: confirmed — the golden compare is over the FULL request body
   (the capture's `body`), not a body.input extraction.
