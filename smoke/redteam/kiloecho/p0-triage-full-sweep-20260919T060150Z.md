# P0 Forensic Triage — full-sweep-20260919T060150Z

Author: QWEN WORKER (kiloecho lane). Read-only audit; writes = this file + the 10-row promotion to
`smoke/redteam/triage/signatures.json` (per coordinator instruction, existing beads only).

Sweep: `smoke/redteam/report/full-sweep-20260919T060150Z/` — 73 rows: **55 FAIL / 8 VACUOUS / 7 PASS / 2 SKIP / 1 FINDING-PASS**.
Env (report.json): ts `20260919T060151Z`, git `fc0b3f9` (branch HEAD at run start, 05:57:34Z), bin_sha `f9e7a15d6b6b`,
key_sha `9f3f56a263da`, upstream `https://llm-proxy-api.ai.eng.netapp.com`. Launcher: `/tmp/full-sweep-2.sh` (still on disk).

Census corrections vs the incoming handoff (re-derived from report.json rows, all counts verified):

- Zero-call deaths = **40** (not 36): 38 FAIL + 2 VACUOUS (`WS9-S01-TOOL-LOOP`, `WS9-S02-TOOL-FAIL` — the
  premise-control design masked the harness death as "model premise 0-hit").
- calls>0 TRUE-FAILs = **17** (not 7): the 7 coordinator-triaged + **10** (AT-AZ-VXG, XW-VXM-VXG, AT-AZ-VXR,
  AT-SPAWN-XWIRE, XW-AZ-VLQ, XW-VXM-AZ, XW-VXM-VLG, XW-VXM-VLG-GUARD, XW-VXM-VLQ, XW-VXM-VLQ-V2) — the 10 are
  now rowed in `smoke/redteam/triage/signatures.json`.
- Death-point split: **31** headless (eager-auth bail) + **9** ACP (`session/new` -32602 "Path is not absolute"),
  not 35+1.
- The `summary_index` fix is **6b13992** (SUMMINDEX-1, base fc0b3f9), **not 5f4411a** (5f4411a is the
  XW-ORPHAN/COMPACT-BOUNDARM/TITLECALL/CITATIONS union; `git log -S summary_index -- client.rs` across all
  refs returns only 6b13992).

---

## 1. AUTH class (headline) — one relative-path bug kills 40/73 cases

**Mechanism.** The sweep was launched by `/tmp/full-sweep-2.sh` with a **relative** `--out`
(`python3.12 smoke/redteam/run.py --out "smoke/redteam/report/full-sweep-$TS"`, line 9 of that script).
`run.py` never absolutizes it (`a.out = os.path.join(REPORT_ROOT, utc_ts())` at run.py:5566 only fires when
`--out` is omitted; run.py:5566-5567). So the hermetic home is a relative path, and `build_env` sets
`env["GROK_HOME"] = home` relative (run.py:666-676). Cases **without** a declared absolute `cwd` get
`case_cwd = <case>/cwd` and the binary is `Popen`ed with that cwd (run.py:687-689), so `$GROK_HOME`
resolves to a **doubly-nested path** under `<case>/cwd/`:

- On-disk proof (the binary created the nested home and logged into it):
  `smoke/redteam/report/full-sweep-20260919T060150Z/rt-m6/cwd/smoke/redteam/report/full-sweep-20260919T060150Z/rt-m6/home/logs/unified.jsonl`
  (1458 lines, 27 launches). Its `config.toml` is the 2-line binary default
  (`[marketplace] default_skills_installs_purged = true`) — no `[model.*]` rows.
- The intended home had the real config: `<case>/home/config.toml` = 68 `[model.*]` sections, 4 with
  `env_key = "CODEX_LLM_PROXY_KEY"` (grok-4.6, gpt-5.6-sol, glm-5.2, qwen3.8-27b), plus `models_cache.json`
  + `proxy-auth-stub.sh` (HermeticHome, run.py:372-380) — never found, because the relative resolution
  landed elsewhere.

Consequence chain (verified in the nested unified logs): no `env_key` rows resolvable → `has_byok=false` →
the inherited `XAI_API_KEY` (== `CODEX_LLM_PROXY_KEY` here; both runs recorded the same ambient key
sha12 `9f3f56a263da`, so rotation is irrelevant) triggers the first-party env-key probe
(`auth: first-party API key probe` → `verdict Unusable, allows_advertise false, attempts 1, key_suffix
db110f921f4c`, 137-143ms — rt-m6 unified line 45 ff, 27× in that file alone) against the default
`https://api.x.ai/v1/api-key` (no `[endpoints] xai_api_base_url` in the 2-line config) with the NetApp
proxy key → 401-class Unusable → `xai.api_key` never advertised → headless eager auth bails
(`startup phase eager_auth` → `connect finished outcome: error`); the user-facing text
`Error: Not signed in. Run 'grok login' to authenticate (or 'grok login --device-code' if no browser is
available).` rides the **unpersisted stdout/stderr tail** (present in report.json stderr_tail for 29 rows;
RT-M6's rode the lost stdout — see §5). ACP-mode cases get further: initialize completes, then the runner's
`session/new` sends the **relative** `cwd` and the binary rejects it — 9 cases die at
`{"code":-32602,"message":"Invalid params","data":"Path is not absolute: smoke/redteam/report/full-sweep-20260919T060150Z/<case>/cwd"}`
 (rt-46c1, rt-crosswire1, rt-crosswire1b, rt-m1..m4, rt-xreplay1, rt-xreplay2 — one hit each in their acp.log).

**Why 09-18 passed:** its launcher used an **absolute** `--out` (09-18 unified logs show absolute
`GROK_HOME`), so the hermetic config was found, the 4 `env_key` rows resolved (`has_byok=true`), and the
probe was skipped. **Binary gate unchanged:** no commits on the probe/auth paths in `dbf9441..fc7d64a`.
**Hermetic homes never carry auth.json**, so live auth state is irrelevant.

**Fix (for the harness lane).**
1. run.py:5567 (after out-resolution, before makedirs): `a.out = os.path.abspath(a.out)` — one line cures
   the whole class (every derived path — home, case_cwd, `session/new` cwd — becomes absolute).
2. Defense-in-depth, run.py:675: `env["GROK_HOME"] = os.path.abspath(home)`.
3. Secondary (note, optional): the `_align_models_cache` (~run.py:430) forge is stale — the binary now
   classifies custom-endpoint fetches as `custom_endpoint` scope (4-part identity incl. origin;
   `model_fetch_auth.rs:64-115`, `cache.rs:77`), so the forged `api_key`-scope cache is rejected in ALL
   cases (runner forge `6b2ac573…` = sha256(["models-api-key",key,""]) vs binary-persisted
   `6466748c…` = sha256(["models-custom-endpoint","…52013…",key,""]); both reproduced from the current env
   key). The catalog actually loads via **live wiretap fetch** (`GET /v1/models` = req-001 in live cases) —
   true since ≥09-18.

**Dead list (40):** 9 ACP (session/new) = rt-46c1, rt-crosswire1, rt-crosswire1b, rt-m1..m4, rt-xreplay1/2.
31 headless (login gate) = rt-c1..c6, rt-m5..m6, rt-m8..m11r (m8, m9, m10a, m10b, m11, m11r), rt-r1..r3,
rt-s1..s4, ws9-arm-tagflip, ws9-arm-xsearch, ws9-s01, ws9-s02, ws9-s03, ws9-s05, ws9-s09, ws9-s11,
dead-seat-kill-resume, xw-resume-crossfam.

---

## 2. The 17 calls>0 TRUE-FAILs

**Class A — pre-fix `summary_index` serialization death (2).** See §3. AT-AZ-VXG (sol→gemini-3.5-flash,
cell az-vlq) + XW-VXM-VXG (claude-sonnet-5→gemini-3-pro-preview, cell vxm-vxg). Both: clean HTTP 200,
turn dies via `prompt_complete` `serialization error: missing field 'summary_index'`, RPC -32603, exit 1.

**Class B — .71-era-stale storage goldens (11; 8 of them = the 10 newly rowed minus the 2 Class-A cells'
companions).** All: clean 200 turn(s), DONE text, zero error events; sole scored miss =
`xwfix_cell_diff.storage` at a reasoning item:

| case | flip | cell | diff |
|---|---|---|---|
| AT-VLQ-VXG | qwen→gemini-3.5-flash | vlq-vlg | idx2 (11=11) — **no expected_red; coordinator adjudicated .71-stale, do not re-litigate** |
| AT-AZ-VXR | sol→grok-4.6 | az-vlq | idx5 (53=53) |
| AT-SPAWN-XWIRE | sol→qwen3.8-27b | az-vlq | idx5 (53=53) |
| XW-AZ-VLQ | sol→qwen3.8-27b | az-vlq | idx5 (53=53) |
| XW-AZ-AZ | sol→terra | az-az | idx2 — stored reasoning lost `encrypted_content` (SYNTH-AZ-CIPHERTEXT placeholders absent) |
| XW-AZ-VXM | sol→claude | az-vxm | idx2 — target-dependent id rekey `encitem_bGl0Z…`→`xw_b185c020…`/`xw_fa9463aa…` |
| XW-VXM-AZ | claude→terra | vxm-az | idx6 (36=36) |
| XW-VXM-VLG / -VLG-GUARD | claude→glm-5.2 | vxm-vlg | idx2 (10=10) + literal ride pin `"id": ""` (rekeyed: `xw_3b4fee1cdfa6687e8b3b75a4`, `xw_bfcc932c3afb776e74bd27a3` verified in req-003) |
| XW-VXM-VLQ / -VLQ-V2 | claude→qwen3.8-27b | vxm-vlq(-v2) | idx2 (10=10) + ride pin (rekeyed: `xw_06a1e8c0a2d2d5bd9691be65`, `xw_123f1f641906b1362b40d44c` verified in req-003) |

The `.71` XW-PROJECT-1 switch-time projector (bb59cff, in dbf9441..fc7d64a) rewrites/rekeys the seeded
reasoning items at switch time, so every pre-.71 T0 golden is era-stale — **re-pin debt, not product
delta** (all carry `expected_red: apex-ayl.71` except AT-VLQ-VXG). The 4 XW-VXM-* companion ride pins
(`wire.grep '"id": ""'`) are the same story on the wire: the empty-id rides were rekeyed, not dropped.

**Class C — documented wall, now PROVEN (1).** XW-VLQ-VLG (qwen→glm-5.2): both glm requests 0×
`"type": "reasoning"`; compaction artifacts on disk (`session/compaction/segment_000.md`,
`compaction_requests/d6c4b5f5-….json` summary non-null, 06:38:13Z, 9 turns digested). The .68-matrix
preemptive compact ate the ride exactly as the case title predicted — **case-design owed**: map the
documented double-miss to VACUOUS/expected_red (PREDICTED-CLEAN verdict semantics were optimistic).

**Class D — env interference: codegraph writer lock (2).** T21-MSG-S5-MCP-TOOL (claude-sonnet-5,
messages wire) + T21-RESP-QWEN3827B-MCP-TOOL (qwen3.8-27b): wire proves the full MCP round-trip; the
codegraph call failed with `Mcp error: -32603: Codegraph writer lock held by PID 81872 (fallback mode)`
(rode back in the next request's tool_result; `ps -p 81872` → gone now). Only failing pin both: the
`xai-grok-sampler/src/retry.rs` content pin — the model honestly reported the lock failure and refused to
invent the path. **Owe: re-run with no live codegraph writer/daemon on the worktree**
(or unset `CODEGRAPH_NO_DAEMON` / stale `…/.codegraph/writer.pid` per the error's own remediation).
NOTE: the verdict.json premise notes ("MCP server never announced — rig failed", "use_tool never issued")
are stale template text vs the live `hit=true` values and the wire — fix the note generator.

**Class E — .21 model-behavior vacuous (1).** XW-VXM-AZ-LIVE (ADV1-R1): turn-0 live claude-sonnet-5 turn
wire-verified `thinking_tokens: 0` (resp-004 message_delta) → no thinking item ever produced → nothing in
storage to ride → both pins miss. Final 200, no brick phrasing. OQ-11 (lenient-shim empty-id tolerance on
the LIVE path) unproven. **Owe: case premise-gate** (assert turn-0 emits thinking) or re-run with
thinking-guaranteed config.

---

## 3. New phrasing `serialization error: missing field 'summary_index'` — RESOLVED

The handoff's three open hypotheses, closed:

- **(a) binary predates the fix — CONFIRMED, with the correct commit.** The fix is **6b13992**
  (SUMMINDEX-1, bead apex-ayl.87, base fc0b3f9, committed **07:38:05Z**). `git log --all -S summary_index`
  on client.rs returns **only** 6b13992 — it is NOT in 5f4411a (the handoff's attribution was wrong), NOT
  in fc0b3f9 (the run-start HEAD, 05:57Z), NOT in fc7d64a. The sweep binary f9e7a15d6b6b predates it
  behaviorally (both gemini cases died with the pre-fix signature in-sweep). The post-sweep rebuild
  (`target/release/grok-responses` now sha **aced8a93263766be**, mtime 07:37:11Z = 54s BEFORE the fix
  commit) **still lacks the fix** — a rerun today would die the same way.
- **(b) dialect scope — REFUTED.** `responses_wire_dialect_for_model_family` (provider.rs:59-68):
  codex→Codex, xai→Xai, **None→Xai**, other→Strict. The gemini-3-pro-preview row (live
  `~/.grok/config.toml:521`) carries **no `model_family`** → None → **Xai dialect** → the lenient
  normalize path. The fix's scope (Xai|Codex, 4 summary types, client.rs:213-224 @HEAD) covers this exact
  frame.
- **(c) non-stream path — REFUTED.** The fatal frame is a stream SSE frame:
  `xw-vxm-vxg/wire/resp-003.jsonl` frame_index 3 = `response.reasoning_summary_text.delta`, item_id
  `rs_dd81ef8a-0b79-41df-9dc0-c24482a7ce1c`, **no summary_index, no sequence_number** — one of the 4
  in-scope types. Frame 4 (`.text.done`) carries `summary_index:0` — LiteLLM omits the field on `.delta`
  only.

Provenance (from 6b13992's commit message + on-disk): first seen **2026-09-19T03:01Z** in
`smoke/redteam/report-at1/AT-AZ-VXG-r2/at-az-vxg/acp.log` (4 hits); 3/4 live gemini 200-turns died then
(AT-AZ-VXG r1/r2, AT-VLQ-VXG r3; the 4th stream emitted no summary frames). Not present in the 09-18
sweep (`smoke/redteam/report/20260918T103338Z/` — 0 hits) nor any older report dir. In THIS sweep the
phrasing is NOT exclusive to XW-VXM-VXG: **AT-AZ-VXG died with it too** (acp.log + unified.jsonl).

**Row status:** drafted and **promoted** as rows #1/#2 of `smoke/redteam/triage/signatures.json`
(class `shell.serialization`, bead apex-ayl.87, status PROPOSED) — see §6 for the global-catalog option.

---

## 4. VACUOUS one-liners (8) + SKIP (2)

- `T21-RESP-GLM52-MCP-TOOL` (glm-5.2, 5 calls): **genuine .21 vacuous.** Codegraph round-trip happened
  (req-008 carries `codegraph__codegraph_explore` + `function_call_output`, 200, `strip_model_bound_state`
  query rode) with **no writer-lock evidence in this case's wire** — but the model never issued the
  declared echo (escaped-command 0-hit, turn-1 text truncated at "Starting with …") and never quoted
  `xai-grok-sampler/src/retry.rs` → `vacuous_reason: model premise 0-hit: sh_call`. Temperament.
- `T21-RESP-GROK46-MCP-TOOL` (grok-4.6, 5 calls): **env interference, Class D.** `writer lock held by PID
  81872` + `Mcp error -32603` ride in req-007/req-008; DONE/ECHO text pins OK, retry.rs pin + escaped echo
  miss → vacuous via `sh_call`. Same re-run as the two FAIL twins.
- `T21-RESP-SOL-MCP-TOOL` (gpt-5.6-sol, 6 calls): **env interference, Class D.** Lock error in
  req-008/req-009; final text literally says "CodeGraph failed: writer lock held"; retry.rs + escaped echo
  miss → vacuous via `sh_call`. Same re-run.
- `WS9-S01-TOOL-LOOP` (0 calls): **harness death misclassified VACUOUS.** `vacuous_reason: model premise
  0-hit: sh_call` — the premise is trivially 0-hit because the model never ran (§1 auth class, nested
  home; verdict.json: model_calls=0, duration 3.1s, no wire files, `Error: Not signed in` in
  stderr_tail).
- `WS9-S02-TOOL-FAIL` (0 calls): same — `model premise 0-hit: fail_call`, model_calls=0, 1.5s, auth-class
  death.
- `WS9-S06-SPAWN-MSG` (15 calls): **genuine .21 vacuous.** Full spawn flow completed (WORKER-ACK,
  PART1/PART2, PARENT-OK, exit 0); `vacuous_reason: model premise 0-hit: reuse_call` — the parent never
  issued the worker-reuse call (temperament). Scored miss: the escaped-form send_message instruction pin
  ("no file of 9") while the raw instruction text rides in 13/17 req files — flag the pin's file scope /
  escape form for case-design review.
- `WS9-S07-INT-CHILD` (31 calls): **genuine .21 vacuous + storm record.** Interrupt flow completed
  (INTERRUPTED + WS9-S07-AFTER, exit 0); `vacuous_reason: model premise 0-hit: v2_flow` (escaped-form
  pattern 0-hit while `task_name`/`worker` ride in 28-29/33 files — same pin-format question as S06).
  `wire.count` 31 vs band 3..9 = qwen storm, recorded; two turns hit runner timeouts (`timeout after
  120s`/`300s`, run.py:1004/1194).
- `WS9-S12-ALIAS-SWITCH` (5 calls): **catalog alias gap, NOT .21.** `session/new` with
  `claude-sonnet-4-5` returned a session with `"currentModelId":"claude-opus-5"` — **silent fallback to
  the catalog default** (acp.log: the requested model was simply not in `availableModels`); ALL 5 model
  calls went out as `POST /v1/messages claude-opus-5`; `session/set_model claude-sonnet-4.5` (id 5) →
  `{"code":-32602,"message":"Invalid params","data":"unknown model id"}` (acp.log line 52). Turns A1/A2/B1/B2
  "succeeded" as text because opus-5 echoed the nonces. `vacuous_reason: model premise 0-hit: thinking_rides`
  is downstream of the wrong model running. **Owe:** (a) case needs the aliases present in the binary's
  catalog (or the catalog needs the rows — apex-93d territory); (b) product-side note: `session/new`
  silently downgrades an unknown model to the default — that deserves an explicit error or at least a
  logged warning.

SKIP: `WS9-S08-RESP-LITE` (Responses Lite substrate absent from the binary — PROP-3/W6 gate row, records
UNPROVEN by design) and `WS9-S10-ASYNC-MSG` (`send_user_message_async` absent — same gate family).

---

## 5. stdout-persistence gap — fix spec

Confirmed: `run_headless_turn` (run.py:679-738) does `out, err = proc.communicate()`;
`r.stdout = out or ""` (run.py:719) is **in-memory only** — never written to disk. `capture_dir` is used
only for `count_wire_model_calls` (run.py:729-730). No other stdout-to-disk path exists for headless
(ACP writes its full stream to `acp.log`; wiretap stdout → `wire.wiretap-stdout.log`). Report rows carry
only `text` (200B) + `stderr_tail` (300B) — which is why RT-M6's failure text is invisible on disk
(`stderr_tail: None`; the ndjson error rode stdout).

Spec (harness lane): in `run_headless_turn`, after run.py:719 (`r.stdout = out or ""`), before event
parsing:

```python
if capture_dir:
    seq = len(globmod.glob(os.path.join(capture_dir, "stdout.ndjson.*")))
    with open(os.path.join(capture_dir, "stdout.ndjson.%d" % seq), "w") as fh:
        fh.write(r.stdout)
```

- Per-turn files: a case has multiple headless turns (row/turn/kill/compact — call sites run.py:3339,
  3419, 3466, 3545, all passing `capture_dir=ctx.capture_dir`; step index `si` exists at 3419/3466/3545
  but not at the row-loop site 3339, so the glob-count seq is the signature-free choice; the runner is
  single-process and sequential per case).
- Files land under the report out-dir → covered by the final redaction sweep (same as wire captures).
- ACP cases unaffected (they have `acp.log`).
- Alternative (if a stable seq is wanted): thread the caller's `si`/row-index through as a `seq=` kwarg —
  touches all 4 call sites, no benefit for the read path.

---

## 6. Coordinator decision list (ordered)

1. **Rebuild the binary** from ≥ 6b13992 — the deployed binary (aced8a93, 07:37Z) predates SUMMINDEX-1 by
   54s; every gemini row that emits a summary `.delta` frame will keep dying until the rebuild.
2. **Apply the 2-line run.py abspath fix** (§1) — cures all 40 zero-call deaths; then a full re-sweep is
   the only way to know the true live-failure surface (the 40 deaths masked whatever those cases would
   have found).
3. **Re-pin the .71-era goldens** after adjudicating the projected post-switch storage shape:
   az-vlq (covers AT-AZ-VXR, AT-SPAWN-XWIRE, XW-AZ-VLQ + the coordinator's XW-AZ-AZ/XW-AZ-VXM),
   vxm-az (XW-VXM-AZ), vxm-vlg + vxm-vlq(+v2) (the 4 XW-VXM-* twins — re-pin storage AND rewrite the
   literal `"id": ""` ride pins to the rekeyed `xw_*` shape). All roll into apex-ayl.71.
4. **Re-run the 5 T21 MCP rows** (2 FAIL + 3 VACUOUS) with no live codegraph writer on the worktree
   (kill/stop the daemon or unset `CODEGRAPH_NO_DAEMON`; stale lock → delete `…/.codegraph/writer.pid`).
   Expected: the `retry.rs` content pins become live again; glm52 may still vacuous on `sh_call`.
5. **Case-design:** XW-VLQ-VLG double-miss → VACUOUS/expected_red (wall now PROVEN, .68 matrix holds);
   XW-VXM-AZ-LIVE → turn-0 thinking premise-gate; WS9-S12 → aliases must exist in the binary catalog
   (and decide: is silent `session/new` fallback to the default model acceptable — product question);
   WS9-S01/S02 → premise-control design should report a zero-call launch as harness-death, not VACUOUS;
   WS9-S06/S07 → audit the escaped-form pin scope ("of 9"/"of 31" file sets vs raw-text rides).
6. **Triage catalog:** 10 rows promoted to `smoke/redteam/triage/signatures.json` (beads apex-ayl.87 ×2,
   apex-ayl.71 ×8). Decide whether rows #1/#2 (`summary_index`) also get promoted into the global
   `smoke/triage/signatures.json` as `#40` — recommended: yes, once the rebuild lands (status PROPOSED →
   OPEN, pointer = "rebuild + rerun AT-AZ-VXG/XW-VXM-VXG").
7. **stdout persistence** per §5 spec — unblocks seeing headless failures on disk (RT-M6 class).

**Also on record (not owed by this lane):** the `_align_models_cache` forge is dead (custom_endpoint
scope mismatch, §1.3) — catalog loads via live `GET /v1/models`; verdict.json premise notes emit stale
template text when the live `hit` value disagrees (T21 rows); the post-sweep binary rebuild (aced8a93)
was not re-stamped into runs.jsonl (git_head fc7d64a is a backfill).
