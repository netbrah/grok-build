# P0 Triage — full-sweep-20260919T084258Z (18 unexpected FAILs)

Author: kiloecho fail-triage lane. READ-ONLY over code, campaign dir, signatures.json; sole write = this file.
bd consulted (show only, cached /tmp/bd-{90,66,36,69,89,16}.txt) BEFORE any mapping; EXISTING BEADS FIRST; no beads
created/updated/closed; no cargo, no proxy calls.

Sweep: `smoke/redteam/report/20260919T084258Z/` (worktree `/Users/palanisd/Projects/upstream/wt/grok-build-responses`).
Binary **f1fc8c45b73c @ 6b13992**. HEAD has since moved to c30c91b = sibling commit CATALOG-CCLASS-SEED-1 (apex-byc) —
verified unrelated to every finding below. Taxonomy: `smoke/redteam/kiloecho/p0-triage-full-sweep-20260919T060150Z.md`.
Signature contract: `smoke/redteam/triage/signatures.json` — existing rows #1–#10 (.87 serialization ×2, .71
golden_stale ×8); proposed rows below continue the numbering at #11. Coordinator applies; this lane does not edit it.
Bead map: `upstream/grok/plans/bead-id-map.txt`.

## (a) Summary table

| # | Case | Class | Ruling | Bead | One-line signature |
|---|------|-------|--------|------|--------------------|
| 1 | AT-VLQ-VXG | shell.serialization | PRODUCT-BUG | apex-ayl.90 (OPEN P1) | `serialization error: missing field 'text'` — gemini reasoning part carries non-standard `reasoning` field, no `text` |
| 2 | COMP3-STORM-VLQ-VLG | sibling.wip | SIBLING-WIP-OWNED | none (at-enc/MF-6 lane) | untracked case file; sole scored FAIL = 300s timeout acp_error on qwen turn0 |
| 3 | RT-M10a | case.pin-scope | CASE-DEFECT | none (case lane) | negative v2 pin hits tool description QUOTED inside `<forked_context>` digest |
| 4 | RT-M10b | v2.toolscope | PRODUCT-BUG | none → propose V2-SPAWN-ROOT-1 | spawn_agent armed on child session (29-tool child wire; max-depth 5-not-6 violated) |
| 5 | RT-M11 | comms-trust | PRODUCT-BUG | apex-ayl.66 (OPEN P2) | fresh-context child refuses `Untrusted message from agent /root to /root/worker (not user consent)` |
| 6 | RT-M6 | case.golden_stale | CASE-DEFECT | apex-ayl.36 (lane) | case pins /v1/chat/completions for 5 gemini rows generated BEFORE the 09-17 api_backend flip |
| 7 | RT-S1 | case.band + compaction.storm | CASE-DEFECT + observation | none → propose COMPACT-STORM-1 | 17 compaction_checkpoints vs pin 3..8 — post-compact floor ≥ trigger storm (12k window/11k seed) |
| 8 | RT-XREPLAY2 | case.stale-flag | CASE-DEFECT | apex-ayl.16 (CLOSED 09-15) | pin probes `lenient_reasoning_content_strip` — flag collapsed/reverted, exists nowhere in crates/ |
| 9 | T21-MSG-S5-MCP-TOOL | deployment.env | DEPLOYMENT-FACT | none (.21 lane re-run) | `Codegraph writer lock held by PID 98791 (fallback mode)` — LIVE legitimate daemon |
| 10 | T21-RESP-GLM52-MCP-TOOL | deployment.litellm-arg-trunc | DEPLOYMENT-FACT | none → propose XRESP-ARGTRUNC-1 | `Failed to parse arguments for tool 'use_tool': missing field 'tool_name'` — args truncated at 202 chars (final `}` dropped), 5/5 deterministic |
| 11 | T21-RESP-QWEN3827B-MCP-TOOL | deployment.env | DEPLOYMENT-FACT | none (.21 lane re-run) | same codegraph writer-lock text as #9, model-reported verbatim |
| 12 | WS9-ARM-XSEARCH | deployment.wire-fact | DEPLOYMENT-FACT | apex-ayl.76 (fact HOLDS) | 400 `Expected the 'type' field of a(n) 'tools' array element to be 'function'; found 'x_search'` on retry; FAIL = pin scoped to first request's resp_status |
| 13 | WS9-S05-COMPACT-RESUME | case.premise-band (+ deployment.litellm-compact-empty) | CASE-DEFECT (primary) + DEPLOYMENT-FACT (secondary) | apex-ayl.78 (case lane) | seed lands in 75–85% prefire band → 2 background prefire compacts (200 + zero-frame stream), 0 artifacts, resume replays full history |
| 14 | XW-AZ-AZ | expected_red (.71-era) | EXPECTED-RED-holding | apex-ayl.71 | storage idx2 (10=10): golden `SYNTH-AZ-CIPHERTEXT-azvxm-0001` vs ABSENT in stored |
| 15 | XW-AZ-VXM | expected_red (.71-era) | EXPECTED-RED-holding | apex-ayl.71 | storage idx2 id rekey `encitem_bGl0ZWxsbT…` → `xw_b185c0202dc5576357e07e45` (deterministic, identical to prior sweep) |
| 16 | XW-RESUME-CROSSFAM | model.temperament | MODEL-TEMPERAMENT | none (.72 case-design: add premise-gate → VACUOUS) | premise 0-hit — sol (effort low) answered as plain text, minted NO reasoning frames, nothing to ride |
| 17 | XW-VLQ-VLG | model.temperament | MODEL-TEMPERAMENT (VACUOUS reclass; confirms .72 noted ruling) | none | both glm reqs 0× `"type": "reasoning"`; preemptive lossy compact → storage idx10 post-compact shape |
| 18 | XW-VXM-AZ-LIVE | case.record-pin-scored | CASE-DEFECT | apex-ayl.72 (lane) | record-only `'"id": ""'` pin SCORED; it misses exactly when the .71 projector rekeyed (GOOD outcome = red) |

Ruling distribution (18/18): PRODUCT-BUG 3 · CASE-DEFECT 6 · DEPLOYMENT-FACT 4 · MODEL-TEMPERAMENT 2 ·
EXPECTED-RED-holding 2 · SIBLING-WIP-OWNED 1. (WS9-S05 counted once under its primary ruling CASE-DEFECT; its
deployment facet is rowed in §(c) as a note, not a 5th DEPLOYMENT row.)

## (b) Per-case evidence

### 1. AT-VLQ-VXG — PRODUCT-BUG — apex-ayl.90
- Wire death on clean 200. Verbatim, `smoke/redteam/report/20260919T084258Z/at-vlq-vxg/acp.log`:
  - L31: `retry_state failed error_type=serialization message="serialization error: missing field \`text\`"`
  - L33: `turn_completed stop_reason=error agent_result="serialization error: missing field \`text\`"`
  - L34: `session/prompt_complete stopReason=error agentResult=…(same)`
  - L37: RPC `{"code":-32603,"message":"Internal error","data":"serialization error: missing field \`text\`"}`
- Root frame verified: `at-vlq-vxg/wire/resp-003.jsonl` frame idx11 = `response.content_part.done` with part
  `{"type":"reasoning_text","reasoning":"**Confirming Final Output**…"}` — LiteLLM gemini adapter emits a
  NON-STANDARD `reasoning` field; the part lacks the SDK-required `text`.
- Code locators: SDK fork panic at `crates/…/response.rs:1581-1595`; normalize early-return
  `crates/…/client.rs:175+`. apex-ayl.90 fix = `part.text = part.reasoning` for `content_part.*` reasoning parts.
- Frame idx4 = reasoning summary `.delta` missing summary_index/seq = the .87 shape — now lenient-parsed on
  6b13992 (SUMMINDEX-1), so it no longer kills; this run proves .87 hold and .90 as the live killer.
- Secondary scored miss: storage idx2 (11=11) = .71-era rekey `rs_6a4c8e2f` → `xw_f5c9ceda469a2d2a5bbdd85f` —
  covered by existing .71 rows (#3–#10 in signatures.json).
- Action: new signature row #11 → apex-ayl.90.

### 2. COMP3-STORM-VLQ-VLG — SIBLING-WIP-OWNED
- `smoke/redteam/cases/comp3-storm-vlq-vlg.json` is **untracked** (git `??`) — at-enc/MF-6 lane, sibling-owned.
- Shape: only scored FAIL = 1 `acp_error` from `timeout after 300s` on the qwen turn0; switch glm-5.2; turn2 200; 3 calls.
- No product ruling taken; routed to the owning lane.

### 3. RT-M10a — CASE-DEFECT (pin-scope)
- Negative pin `Start a named background agent` (spawn_agent description) is expected ABSENT in forked child wire.
  It hit inside `body.messages[0].content` = the `<forked_context>` digest, where the parent qwen text QUOTES the
  tool description — fork_turns=all legitimately carries quoted descriptions into the digest.
- Child tool array (23 tools) is v2-clean; the other 5 needles pass. Flow completed: CROSSMODEL-OK / PARENT-OK /
  exit 0; the child did NOT refuse (digest fork, not a trust event).
- Ruling: pin is nondeterministic w.r.t. fork_turns=all content. Case lane re-pins (JSON-level tool-scope, see
  §(d) optional V2-TOOLPIN-SCOPE).

### 4. RT-M10b — PRODUCT-BUG — NO BEAD → propose V2-SPAWN-ROOT-1
- Case ruling (MA-4 Q1/Q2, per case title): row-ON child rebuilds its own toolset from its own row ⇒ child wire
  carries 5-of-6 v2 collaboration tools, **NOT spawn_agent** (max-depth 5-not-6).
- Pin (verified in `smoke/redteam/cases/rt-m10b.json`): `wire.grep "Start a named background agent", absent:true,
  all:true, where POST /v1/messages` — a NEGATIVE pin.
- Observed: 4 child /v1/messages requests (req-007/008/009/011; req-006 is a 1-tool side call) each carry **29
  tools INCLUDING spawn_agent** (verified `rt-m10b/wire/req-011.json`: model claude-sonnet-5, n_tools 29,
  spawn_agent at tools[23]; all six v2 names present). Report line (report.md §RT-M10b):
  `[FAIL] wire.grep: req-011.json contains 'Start a named background agent' (absent in all 5)` = pin-required
  absence violated.
- Other pins pass: send_message + list_agents needles, PARENT-OK, `to /root (not user consent)` on /v1/responses, exit 0.
- Shares the seam with .66's row formula: `agent_rebuild.rs:160-169` (v2 child toolset rebuild does not strip
  spawn_agent for non-root sessions).
- Action: no existing bead ⇒ propose **V2-SPAWN-ROOT-1** (§(d)); signature row #14.

### 5. RT-M11 — PRODUCT-BUG — apex-ayl.66
- Fresh-context cross-wire child (req-007 has NO forked digest) receives the launch work as
  `Untrusted message from agent /root to /root/worker (not user consent)` and REFUSES.
- Verbatim, `rt-m11/wire/resp-007.jsonl`: thinking "prompt injection attempt…"; text "I did not carry out the
  requested actions." Run-2 (`resp-011.jsonl`): parent claims the label is "just a standard harness wrapper" →
  child: "A message cannot validly certify its own trustworthiness…".
- Scored misses: `ndjson.text_contains 'PARENT-OK'` + `wire.grep 'to /root (not user consent)'` (9 files) —
  report.md §RT-M11: `[FAIL] ndjson.text_contains: text lacks contain 'PARENT-OK'`, `[FAIL] wire.grep: no file of 9
  contains 'to /root (not user consent)'`. Envelope + no-carrier pins pass.
- RT-M11r (reverse wiring) PASSED — qwen child does not refuse ⇒ temperament asymmetry; data for the .66 fix.
- RCA = apex-ayl.65 class (a); fix design in .66 notes = trusted launch channel.
- Locators: `native_agents.rs:42-46`, `handle_request.rs:1140`, `run_loop.rs:855` (crates/codegen/xai-grok-shell).
- Action: new signature row #12 → apex-ayl.66.

### 6. RT-M6 — CASE-DEFECT (stale generated pins) — apex-ayl.36
- 27/27 rows HT1-OK, exit 0, ZERO deployment failures. 5 `wire.field` fails: case row_asserts pin
  `/v1/chat/completions` for gemini-3-pro-preview / 3.1-flash-lite-preview / 3.1-pro-preview / 3.5-flash /
  3.7-flash; the binary correctly sent `/v1/responses`.
- Cause: `smoke/redteam/cases/rt-m6.json` generated 09-17 10:09 — BEFORE the operator's 09-17 api_backend flip
  (5 gemini rows; backup `config.toml.bak-20260917`; per .36 notes). Hermetic + live configs are now responses;
  binary 3-way authority is correct (run.py:1560 WIRE_PATHS, api_backend_for_model). gen-matrix.py
  gemini*→cc heuristic is likewise stale.
- Action: regenerate case under apex-ayl.36; no new bead.

### 7. RT-S1 — CASE-DEFECT (band miscalibration) + storm observation → propose COMPACT-STORM-1
- Scored FAIL (report.md §RT-S1): `[FAIL] artifact.count: 17 files match compaction_checkpoints/*.json` vs pin
  3..8. Everything else passes: 0 error events, CHILD-OK ×3 (real subagent work; v1 unaffected by .66),
  PARENT-DONE, exit 0, compaction_requests `$.error=None` + `$.summary` non-null.
- Storm (verified): 17 auto-compact checkpoints 09:28:16→09:38:37 (all prompt_index=1), 8
  compaction_requests artifacts (trigger=auto, variant=detailed, 1 attempt each, summaries 4.2–5.6k chars),
  each `compacted_history` = 5 items ~33KB.
- Driver: qwen context_window=12000 (home/config) + 11k seed = 92% ⇒ post-compact floor (system+summary+
  scaffolding) still ≥ trigger ⇒ re-fire every turn; no headroom recovery. 11 min, 41 calls.
- Aggravator: live config `features/multi_agent_v2=true` leaked v2 tools into this v1 case (parent used
  list_agents re-verification loops — visible in latest compact summary 061f6ddc, 09:36:59); case lacks a
  config_patch pin-off (hygiene debt).
- NOT .89 (that is the Messages-wire brick via 10k per-item cap — different wire, different mechanism).
- WS9-S05 (§13) is a DISTINCT mechanism (prefire band + proxy empty-200); it does NOT corroborate this storm —
  COMPACT-STORM-1 scope stays RT-S1-only.
- Action: re-pin band in case lane; propose **COMPACT-STORM-1** (§(d)); optional signature row #15.

### 8. RT-XREPLAY2 — CASE-DEFECT (stale probe of reverted flag) — apex-ayl.16 (CLOSED)
- config_patch sets `lenient_reasoning_content_strip=true` — flag exists NOWHERE in crates/ at 6b13992, c30c91b,
  or any ref (only test commit 28ca83c/.64).
- apex-ayl.16 CLOSED 2026-09-15: "FLAG COLLAPSE: lenient-strip reverted" (impl patch
  `grok/plans/xreplay2-flag-collapse-impl.patch`).
- Observed: reasoning item dropped wholesale (post-collapse default), 2 sol reqs (want 1..1), all 200, turn OK.
- CUT pins can never go green post-collapse. Action: retire or convert the case; lane = .16's successor.

### 9. T21-MSG-S5-MCP-TOOL — DEPLOYMENT-FACT (env interference) — .21 lane re-run
- Verbatim tool_result (`t21-msg-s5-mcp-tool/wire/req-009.json`):
  `Mcp error: -32603: Codegraph writer lock held by PID 98791 (fallback mode). … delete
  /Users/palanisd/Projects/upstream/wt/grok-build-responses/.codegraph/writer.pid`.
- Model honestly reported the error → retry.rs pin misses; DONE/ECHO pins pass.
- PID 98791 = LIVE legitimate fallback daemon (started 08:07:44Z) — do NOT pursue as stale lock.
- Mechanism: case codegraph (`serve --mcp --no-watch`, no --root) walks up the hermetic cwd to the enclosing
  worktree index; stderr log "Shared daemon unavailable; serving this session in-process (degraded)" =
  contributing (a fallback-mode daemon can't be proxied), ROOT = the live daemon holding the worktree lock
  during the sweep.
- Action: .21 lane re-run after sweep window; no new bead.

### 10. T21-RESP-GLM52-MCP-TOOL — DEPLOYMENT-FACT (new .52-family seam) — NO BEAD → propose XRESP-ARGTRUNC-1
- Proxy-side DETERMINISTIC truncation: glm-5.2 via vLLM/LiteLLM responses-compat drops the final `}` of
  `function_call_arguments` when the argument string exceeds 202 chars.
- Verified across all 5 attempts (wire/ of `t21-resp-glm52-mcp-tool/`):
  | resp | item_id | call_id | deltas | delta sum | .done args | .done JSON |
  |------|---------|---------|--------|-----------|------------|------------|
  | resp-007 | fc_7611d7d3 | call_6925a4f6… | 28 | 202 | 202 | INVALID @char 202 |
  | resp-008 | fc_0e0117a3 | call_77cd4ad6… | 28 | 202 | 202 | INVALID @char 202 |
  | resp-009 | fc_c380ef76 | call_9774d115… | 28 | 202 | 202 | INVALID @char 202 |
  | resp-010 | fc_b3261a6b | call_8595ad4d… | 28 | 202 | 202 | INVALID @char 202 |
  | resp-012 | fc_2f37597f | call_6e85a9a8… | 28 | 202 | 202 | INVALID @char 202 |
  delta stream sum == .done frame exactly (match=True) ⇒ truncation is UPSTREAM of the binary (wiretap shows the
  truncated stream). Truncated tail, verbatim: `… "projectPath": "/Users/palanisd/Projects/upstream/wt/grok-build-responses"}`
  — only the `tool_input` brace closed; the outer use_tool brace missing (valid length would be 203).
  Shorter calls on the same model pass (42/48/90-char args, VALID, resp-003/006/011).
- Harness chain (designed behavior, misleading error): `tool_calls.rs:1428-1495` → `from_str::<Value>` EOF at
  col 202 → `try_extract_concatenated_json_objects` (`tool_input_parsing.rs:1-30`) → None (0 objects <2) →
  salvage `{"raw": …}` → `UseToolInput` (`use_tool/mod.rs:17`) → `missing field 'tool_name'`; user-facing message
  built at `tool_dispatch.rs:375-414` (locator L380; the JSON-position note at L397 is actually helpful).
  Verbatim (tool_result in `wire/req-008.json` L156 / `req-012.json` L156+L183):
  `Failed to parse arguments for tool \`use_tool\`: missing field \`tool_name\` … Note: the arguments above contain
  invalid JSON — EOF while parsing an object at line 1 column 202. Please fix the syntax and retry.`
- Model retried 5× with identical shape (same long projectPath) → gave up → retry.rs pin misses.
- NOT double-encode (lead mismatch); does NOT share the .52 seam (apply_patch builtin is a different path).
  codegraph degraded-daemon = red herring here (the call never reached the MCP server).
- Action: propose **XRESP-ARGTRUNC-1** (§(d), proxy-side fix); signature row #13.

### 11. T21-RESP-QWEN3827B-MCP-TOOL — DEPLOYMENT-FACT (same env as #9)
- Model text verbatim: "N/A — the explore call failed (CodeGraph writer lock held by PID 98791, fallback mode)…"
  → retry.rs pin misses; DONE/ECHO pass.
- Same root as #9 (live daemon PID 98791 holding the worktree `.codegraph` lock). Action: .21 re-run; no new bead.

### 12. WS9-ARM-XSEARCH — DEPLOYMENT-FACT (apex-ayl.76 fact HOLDS) — not vacuous
- req-003 (first grok-4.6 request, x_search riding — anti-vacuous pin satisfied) got NO resp capture (transport
  loss, "upstream never answered"); retry req-004 → **400**, verbatim (`ws9-arm-xsearch/wire/resp-004.jsonl`):
  `Expected the 'type' field of a(n) 'tools' array element to be 'function'; found 'x_search'.` (INVALID_ARGUMENT).
- Turn died exit 1 (expected; absent_ok). Only scored FAIL = deployment_verdict pin scoped to the FIRST
  request's resp_status (no capture ⇒ no status ⇒ red).
- Case-lane nit: pin robustness — scope to any grok-4.6 resp, or treat no-capture+retry-400 as the recorded 400.
- New one-liner observation: first-response transport loss — record, don't escalate; NOT .38.

### 13. WS9-S05-COMPACT-RESUME — CASE-DEFECT (premise band miscalibration) + DEPLOYMENT-FACT (proxy silent empty-200)
Case: `smoke/redteam/cases/ws9-s05-compact-resume.json` (bead apex-ayl.78), qwen3.8-27b, responses wire, 28k seed
(snapshot 229 lines/113337B), config_patch `model/qwen3.8-27b/context_window=32768`, `remote_compaction_v2=false`
(DORMANT branch per case). Scored FAILs (report.md L1505-1546): `artifact.count compaction_requests/*.json` 0 vs
1..1; `$.attempts eq 1` + `$.summary ne_null` vacuous (no files); `wire.count xai-compact-` 2 vs 0..1
(">1 = compact storm = defect (S3)" per case). Passing: both turns exact-text (WS9-S05-T1/T2), exit 0.

**Flow (verified end-to-end).** Two headless processes (the case's "RESUME" shape): p23438 = T1 (09:49:17–28),
p25324 = T2 (09:49:28–33). Per process, one background two-pass PREFIRE pass-1 compact fires concurrent with the
turn:
- T1: inference_start 09:49:21.064 (turn req-004 built/sent); compact req-003
  `xai-compact-89605d52-65cb-4c42-87a5-5ec8b5ee2d27` sent ~09:49:21.5 (resp-003 ts 09:49:23 −
  X-Litellm-Response-Duration-Ms 1531) — i.e. AFTER the turn request ⇒ NOT the blocking pre-sampling
  auto-compact (that would await before build_request).
- T2: same shape — req-006 `xai-compact-51b3fea1-688b-44c8-b8ac-3471d2163a33` ~09:49:30.5, turn req-007.
- Both compact requests are prefire pass-1: last input item = the 5-section two-pass prompt
  (`build_two_pass_compaction_prompt` — "1. Primary Request and Intent … 5. Optional Next Step", NO "Files and
  Code Sections"; verified in req-003/req-006 bodies, 3147B), `temperature: 1.0`, `tool_choice: "auto"`, 31 tools,
  `reasoning: {summary: "concise"}` (no effort), 232/234-item full-context inputs.
- Both compact calls: **200 + zero-frame SSE stream** (`wire/resp-003.jsonl` / `resp-006.jsonl` = header line
  only: `Content-Type: text/event-stream`, `X-Litellm-Model-Group: qwen3.8-27b`, zero `frame_index` lines;
  durations 1531ms / 541ms). Turn calls fine: resp-004 29 frames, resp-007 24 frames, first_token
  09:49:28.228 / 09:49:32.036, `turn_ended outcome=completed`.

**Why no artifact (case premise broken).** Resolved auto-compact threshold = 85%
(`DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT`, `util/config/resolve/compaction.rs:2`; no config/env override in this
case — case env is `store:false` only). Prefire gate = threshold − lead(10%) = 75%
(`DEFAULT_PREFIRE_LEAD_PERCENT`, `compaction.rs:59`; `should_prefire_two_pass` `compaction.rs:249-277`).
`Feature::TwoPassCompaction` default-ON (`xai-grok-config-types/src/registry.rs:191-197`). Observed behavior pins
the binary's token estimate to the **prefire band [75%, 85%)**: prefire fired (req-003/006) yet the blocking
`check_auto_compact_needed` (85% line, `compaction.rs:2602`, `should_auto_compact` :2547) never fired — no
"Pre-sampling auto-compact trigger" line, no blocking request, no second compact. The case premise — "28k est
tokens, above the plausible max resolved auto-compact threshold (0.9×29532)" — is wrong twice over: the resolved
threshold is 85% (not ≤90%), and the binary's estimator places the seed BELOW the 85% line.
Decisive: the prefire path NEVER writes a `compaction_requests` artifact — the artifact write exists only in the
hard/manual run_compact path (`persist_compaction_request_artifact` `compaction.rs:2838`, called :1855;
`persistence.rs:2363-2371`). So in the prefire band the artifact pins (count 1..1, attempts, summary) are
UNSATISFIABLE regardless of proxy health.

**Why the compacts failed (deployment fact).** The local LiteLLM proxy (127.0.0.1:55909) deterministically
returns 200 + empty SSE stream for the compact-SHAPE request (2/2 across both processes) while the turn-shape
request on the same model succeeds immediately after (29/24 frames). Contrast, same model+proxy 50 min earlier
(rt-c3, 08:59): the compact got an in-stream error instead — artifact verbatim
(`rt-c3/session/compaction_requests/20046ef1-08b0-4a71-8460-bdf05da0a4a8.json`):
`error: "Compaction sampler build failed: compact failed: stream error (unknown): litellm.APIError: Response API
in-stream error"` (attempts 1, outcome deterministic). Same broken proxy compact-shape seam, two failure modes.

**Binary handling = designed, not a bug.** Empty stream ⇒ `content=""` ⇒ empty check
(`session_compact.rs:847-858`) ⇒ `CompactFailure::Transient("compact failed: model returned empty response")` ⇒
prefire maps to `PrefireOutcome::SampleFailed` with a `tracing::warn!` only
("two_pass: summarization sample failed", `compaction.rs:243`) — no cache, no session event, no artifact, no
notification; turn proceeds. If the estimate had crossed 85%, the hard path would have surfaced the same failure
user-visibly with suppression (that path's EmptyResponse arm also stops after 1 attempt — no retry storm).

**Case's own FAIL definition mapping.** "resume that replays the full pre-compact history (S2 persistent fidelity
loss)" holds on the wire: req-007 167723B > req-004 161724B; snapshots 113337→116813→122701B monotone; zero
compaction events in `session/events.jsonl`. But the cause is band miscalibration + proxy fact, not a binary
regression. The "wire.count 2 = storm" label is met in count but is NOT a binary retry storm: one prefire per
process, 2 processes by the case's own resume design.

**Product observation (record-only, no bead; optional propose PREFIRE-VIS-1 §(d)).** Prefire failures are
invisible on every evidence surface the case relies on: no artifact (by design), no session event, no user
notification, and plain tracing isn't captured in `home/logs/unified.jsonl` (same for rt-c3's successful
compact — verified: 0 "Sending compact request" lines there too). A failed prefire leaves only span telemetry.

**Actions.** (1) Case lane (apex-ayl.78): re-anchor the premise — seed the BINARY's estimate past the 85% hard
line (larger seed, or lower the hard line via `GROK_AUTO_COMPACT_THRESHOLD_PERCENT`/config in config_patch), or
re-pin to prefire-band semantics (0 artifacts, ≤1 xai-compact per process, prefire request presence); correct the
"0.9×29532" premise text to the actual 85% default. (2) Proxy lane: record qwen3.8-27b responses-compat
silent-empty-200 on compact shape (cross-ref rt-c3 in-stream error). (3) Signature row #16.

### 14. XW-AZ-AZ — EXPECTED-RED-holding (.71-era-stale)
- Clean turn; sole diff: storage idx2 (10=10) `encrypted_content`: golden `"SYNTH-AZ-CIPHERTEXT-azvxm-0001"` vs
  ABSENT in stored (full-item diff verified). Same shape as the prior sweep's .71 adjudication.
- Goldens on disk NOT re-pinned (mtimes: az-az Sep 18 04:00; latest commit touching cells d404fe1) although the
  09:51Z map shows .70/.71 closed "re-pin cut 09-19" (sibling lane) ⇒ rerun in the re-pinned worktree to confirm;
  else reopen. Bead: apex-ayl.71.

### 15. XW-AZ-VXM — EXPECTED-RED-holding (.71-era-stale)
- idx2 diff = id rekey `encitem_bGl0ZWxsbT…` → `xw_b185c0202dc5576357e07e45` — IDENTICAL to the prior sweep's
  rekey (deterministic). Bead: apex-ayl.71; same rerun-after-repin condition as #14.

### 16. XW-RESUME-CROSSFAM — MODEL-TEMPERAMENT (premise 0-hit) — .72 case-design
- Premise = sol turn mints encitem_ reasoning; actual: sol (reasoning_effort=low) answered the recursion
  question as PLAIN TEXT — NO reasoning frames in resp-003/004, no reasoning items in the 13-line history ⇒
  nothing to ride. Anti-vacuous pin failed by design; affinity guard vacuously clean.
- .38 D-ENC axis untested (rerun with a reasoning-minting seed). Action: .72 lane adds a premise-gate ⇒ VACUOUS.

### 17. XW-VLQ-VLG — MODEL-TEMPERAMENT (VACUOUS/expected_red reclass; CONFIRMS .72 noted ruling)
- Both glm reqs (req-003/004) contain 0× `"type": "reasoning"`; preemptive lossy compact fired
  (`compaction/segment_000.md` + 1 compaction_requests artifact; calls=2 = compact+turn).
- Storage idx10 (type=assistant, 11=11) = downstream of compact: golden idx10 = qwen turn-1 "README describes…"
  vs actual = post-compact 9-line shape. No product delta.

### 18. XW-VXM-AZ-LIVE — CASE-DEFECT (record-pin scored; good outcome = red) — .72 lane
- **LEAD MISMATCH resolved**: .72 note's "turn-0 thinking premise-gate" — the premise HELD: claude turn 0 = 2
  calls (resp-003 = session_title tool "Multiply 17 by 23"; resp-004 = thinking block "This is simple math:
  17×23…") ⇒ reasoning persisted as `xw_1e4e2acfe1f55edd8aff5e71` (hist item 7); sol req-005 carries the ride,
  NO empty id.
- The `'"id": ""'` pin is explicitly labeled in the case as "record, not a defect in itself" with dual-outcome
  semantics (miss = .71 projector re-keyed = GOOD) yet is SCORED ⇒ FAIL.
- Anti-vacuous pin PASS, final 200, 1 sol req (no storm), no brick phrasing.
- OQ-11 resolves GOOD-direction: the .71 projector rekey is live on the live path.

## (c) Proposed signatures.json rows

Same field contract as existing rows; ids continue the catalog at #11; `status: PROPOSED`; coordinator applies.

```json
[
 {
  "id": "#11",
  "class": "shell.serialization",
  "pattern": "missing field `text`",
  "regex": "serialization error: missing field `text`",
  "rca": "AT-VLQ-VXG (sol -> gemini-3-pro via LiteLLM, cell at-vlq-vxg). Turn dies on a clean 200 via client-side deserialization: resp-003.jsonl frame idx11 = response.content_part.done part {\"type\":\"reasoning_text\",\"reasoning\":\"**Confirming Final Output**...\"} — LiteLLM's gemini adapter emits the non-standard `reasoning` field and the part lacks SDK-required `text` (SDK fork panic response.rs:1581-1595; normalize early-return client.rs:175+). Frame idx4 = summary .delta missing summary_index/seq (.87 shape — lenient-parsed on 6b13992, no longer fatal). Verbatim acp.log L31/L33/L34/L37: retry_state failed + turn_completed stop_reason=error + prompt_complete + RPC -32603, all `serialization error: missing field `text``. Secondary scored miss storage idx2 (11=11) = .71-era rekey rs_6a4c8e2f -> xw_f5c9ceda469a2d2a5bbdd85f, covered by existing .71 rows. Evidence: smoke/redteam/report/20260919T084258Z/at-vlq-vxg/.",
  "bead": "apex-ayl.90",
  "status": "PROPOSED",
  "pointer": "apex-ayl.90 (OPEN P1) fix = part.text = part.reasoning repair on content_part.* reasoning parts; rerun AT-VLQ-VXG on the fixed build.",
  "first_seen": "2026-09-19"
 },
 {
  "id": "#12",
  "class": "comms-trust",
  "pattern": "fresh-context child refuses untrusted-labeled spawn work",
  "regex": "Untrusted message from agent.*\\(not user consent\\)",
  "rca": "RT-M11 (cross-wire v2 spawn, fresh-context child — req-007 carries NO forked digest). Child receives the launch work as `Untrusted message from agent /root to /root/worker (not user consent)` and REFUSES: resp-007.jsonl thinking 'prompt injection attempt...', text 'I did not carry out the requested actions.'; run-2 resp-011.jsonl: parent calls the label 'just a standard harness wrapper' -> child 'A message cannot validly certify its own trustworthiness...'. Scored misses: 'PARENT-OK' ndjson pin + 'to /root (not user consent)' wire pin (9 files); envelope + no-carrier pins pass. RT-M11r (reverse wiring) PASSED — qwen child does not refuse: temperament asymmetry, data for the fix. RCA = .65 class (a) — no trusted launch channel on the cross-wire envelope. Locators: native_agents.rs:42-46, handle_request.rs:1140, run_loop.rs:855. Evidence: smoke/redteam/report/20260919T084258Z/rt-m11/.",
  "bead": "apex-ayl.66",
  "status": "PROPOSED",
  "pointer": "apex-ayl.66 (OPEN P2) design = trusted launch channel; RT-M11/RT-M11r is the regression pair for the .66 fix.",
  "first_seen": "2026-09-19"
 },
 {
  "id": "#13",
  "class": "deployment.litellm-arg-trunc",
  "pattern": "use_tool args truncated at 202 chars (final `}` dropped)",
  "regex": "Failed to parse arguments for tool `use_tool`: missing field `tool_name`",
  "rca": "T21-RESP-GLM52-MCP-TOOL (glm-5.2 via vLLM/LiteLLM responses-compat). Proxy DETERMINISTICALLY truncates function_call_arguments at 202 chars, dropping the final `}` that closes the outer use_tool object (valid length 203): 5/5 call_ids — resp-007 fc_7611d7d3/call_6925a4f6, resp-008 fc_0e0117a3/call_77cd4ad6, resp-009 fc_c380ef76/call_9774d115, resp-010 fc_b3261a6b/call_8595ad4d, resp-012 fc_2f37597f/call_6e85a9a8 — each 28 deltas summing to exactly 202 = the .done frame (truncation upstream of the binary; wiretap shows the truncated stream); shorter calls (42/48/90 chars) pass. Harness chain is designed but misleading: tool_calls.rs:1428-1495 -> from_str EOF col 202 -> try_extract_concatenated_json_objects (tool_input_parsing.rs:1-30) None -> {\"raw\":...} salvage -> UseToolInput (use_tool/mod.rs:17) -> missing field 'tool_name'; message built tool_dispatch.rs:375-414 (locator L380, JSON-position note L397). Model retried 5x, gave up -> retry.rs pin miss. NOT double-encode; does not share the .52 seam; codegraph degraded-daemon a red herring (call never reached the MCP server). Evidence: smoke/redteam/report/20260919T084258Z/t21-resp-glm52-mcp-tool/wire/.",
  "bead": "none",
  "status": "PROPOSED",
  "pointer": "propose XRESP-ARGTRUNC-1 — proxy-side fix (LiteLLM vLLM responses-compat drops the final delta for glm-5.2 args >202 chars); keep the EOF-position note ahead of the missing-field error (already present, tool_dispatch.rs:397).",
  "first_seen": "2026-09-19"
 },
 {
  "id": "#14",
  "class": "v2.toolscope",
  "pattern": "spawn_agent armed on child session (max-depth 5-not-6 violated)",
  "regex": "Start a named background agent.*\\(absent in all 5\\)",
  "rca": "RT-M10b (v2 cross-model pair, child row-ON: qwen3.8-27b parent, claude-sonnet-5 child). Case ruling MA-4 Q1/Q2: row-ON child rebuilds its toolset from its own row => 5-of-6 v2 tools, NOT spawn_agent (max-depth 5-not-6). Observed: all 4 child /v1/messages turns (req-007/008/009/011; req-006 is a 1-tool side call) carry 29 tools INCLUDING spawn_agent at tools[23] (verified req-011.json: model claude-sonnet-5, n_tools 29, all six v2 names present) — the negative pin (absent:true, all:true, POST /v1/messages, needle 'Start a named background agent') fails. Other pins pass (send_message/list_agents needles, PARENT-OK, 'to /root (not user consent)' on /v1/responses, exit 0). Shares the seam with .66's row formula agent_rebuild.rs:160-169. Evidence: smoke/redteam/report/20260919T084258Z/rt-m10b/.",
  "bead": "none",
  "status": "PROPOSED",
  "pointer": "propose V2-SPAWN-ROOT-1 — restrict spawn_agent to root sessions in the v2 child toolset rebuild (agent_rebuild.rs:160-169).",
  "first_seen": "2026-09-19"
 },
 {
  "id": "#15",
  "class": "compaction.storm",
  "pattern": "auto-compact re-fires when post-compact floor stays >= trigger",
  "regex": "17 files match compaction_checkpoints/\\*\\.json",
  "rca": "RT-S1 (qwen3.8-27b, 12k window / 11k seed = 92%). 17 auto-compact checkpoints 09:28:16->09:38:37 (all prompt_index=1), 8 compaction_requests artifacts (trigger=auto, variant=detailed, 1 attempt each, summaries 4.2-5.6k chars), each compacted_history = 5 items ~33KB; 11 min, 41 calls. Flow otherwise healthy: 3 v1 subagents CHILD-OK, PARENT-DONE, exit 0, 0 errors. Post-compact floor (system+summary+scaffolding) stays >= the trigger, so every turn re-fires; no headroom recovery. NOT .89 (Messages-wire brick via 10k per-item cap). WS9-S05 is a distinct mechanism (prefire band + proxy empty-200) and does NOT corroborate this storm. Aggravator: live multi_agent_v2=true leaked v2 tools (list_agents re-verification loops in the 061f6ddc summary). Evidence: smoke/redteam/report/20260919T084258Z/rt-s1/.",
  "bead": "none",
  "status": "PROPOSED",
  "pointer": "propose COMPACT-STORM-1 — headroom check/backoff so a compact that leaves the floor >= trigger does not re-fire per turn; case-lane re-pin of the checkpoint band. Optional row — record if the coordinator prefers the storm un-rowed.",
  "first_seen": "2026-09-19"
 },
 {
  "id": "#16",
  "class": "case.compaction-band",
  "pattern": "seed lands in the 75-85% prefire band: background prefire compacts (200 + zero-frame proxy stream), 0 artifacts, resume replays full history",
  "regex": "0 files match compaction_requests/\\*\\.json",
  "rca": "WS9-S05-COMPACT-RESUME (case bead apex-ayl.78; qwen3.8-27b; 28k seed; context_window=32768 patch). Case premise '28k est tokens above the auto-compact threshold (0.9x29532)' is miscalibrated: the resolved default threshold is 85% (no override), and the binary's estimate lands in the prefire band [75%, 85%) — prefire fired (2x, one per resume process) while the blocking 85% auto-compact never fired. Prefire (Feature::TwoPassCompaction default-on; compaction.rs:249/292) runs concurrent with the turn and NEVER writes a compaction_requests artifact (writer lives only in the run_compact path, compaction.rs:1855/2838) — the artifact pins are unsatisfiable in this band regardless of proxy health. Both prefire samples also failed at the proxy: 200 + zero-frame SSE stream (resp-003 1531ms, resp-006 541ms) for the compact shape (temp 1.0 + tool_choice auto + reasoning{summary only}), while the same model's turn shape streams fine — deterministic 2/2; same proxy returned an in-stream litellm.APIError for the rt-c3 compact 50 min earlier. Binary handling is by design (EmptyResponse -> terminal after 1 attempt, warn-only prefire SampleFailed); session healthy, both turns exact-text, exit 0. Verbatim FAILs: '[FAIL] artifact.count: 0 files match compaction_requests/*.json'; '[FAIL] wire.count: 2 wire files match req-*.json (want 0..1)'. Evidence: smoke/redteam/report/20260919T084258Z/ws9-s05-compact-resume/ + wire/ + session/.",
  "bead": "apex-ayl.78",
  "status": "PROPOSED",
  "pointer": "case lane (apex-ayl.78): re-anchor the seed past the 85% hard line (larger seed or GROK_AUTO_COMPACT_THRESHOLD_PERCENT in config_patch) or re-pin to prefire-band semantics (0 artifacts, <=1 xai-compact per process); correct the 0.9x29532 premise to the 85% default. Proxy lane: record qwen responses-compat silent-empty-200 on compact shape (cross-ref rt-c3 in-stream error).",
  "first_seen": "2026-09-19"
 }
]
```

## (d) No bead — propose (2-line scopes)

- **V2-SPAWN-ROOT-1** (RT-M10b): restrict `spawn_agent` to root sessions in the v2 child toolset rebuild
  (`agent_rebuild.rs:160-169`); per MA-4 Q1/Q2 the child gets 5-of-6 (max-depth 5-not-6), but the observed
  29-tool child /v1/messages wire (rt-m10b req-007/008/009/011) arms spawn_agent at tools[23].
- **XRESP-ARGTRUNC-1** (T21-RESP-GLM52): proxy-side (LiteLLM vLLM responses-compat) drops the final
  `function_call_arguments` delta for glm-5.2 when args exceed 202 chars (202/203, deterministic across 5
  attempts; 42/48/90-char calls unaffected) — fix upstream in the proxy; optional harness: keep the
  EOF-position note ahead of the misleading missing-field error (already present, tool_dispatch.rs:397).
- **COMPACT-STORM-1** (RT-S1): auto-compact re-fires when the post-compact floor (system+summary+scaffolding)
  stays >= trigger (12k window/11k seed: 17 checkpoints, 8 auto artifacts, 11 min, 41 calls) with no headroom
  recovery; add a headroom check or backoff. WS9-S05 is a distinct mechanism and stays OUT of this scope.
- (Optional) **V2-TOOLPIN-SCOPE** (RT-M10a): scope the RT-M10 fail-closed v2 pins to tool definitions
  (JSON-level `tools[].name`/description), not raw body text — forked-context digests legitimately quote
  tool descriptions under fork_turns=all.
- (Optional, low) **PREFIRE-VIS-1** (WS9-S05): make two-pass prefire outcomes observable — record pass-1
  failures (incl. the verbatim stream state) in a compaction artifact or session event; today the only trace is
  a tracing warn, so a failed prefire leaves no artifact/event/notification evidence surface.

## Leads that did NOT match the hypothesis

1. **XW-VXM-AZ-LIVE** — .72's "turn-0 thinking premise-gate": premise HELD (thinking on the 2nd turn-0 call
   after the session_title round; persisted `xw_1e4e2acfe1f55edd8aff5e71` rode sol req-005, no empty id). The
   actual FAIL = the record-only `'"id": ""'` pin being scored against the GOOD (rekeyed) outcome ⇒ case defect;
   OQ-11 resolved good-direction.
2. **T21-RESP-GLM52** — "double-encode/arg-parse seam": it is streaming-args TRUNCATION upstream (final char
   dropped at exactly 202/203 by the proxy, deterministic 5/5); the `missing field 'tool_name'` is a downstream
   artifact of the `{"raw":…}` salvage path. Separate seam from .52 (confirmed not shared).
3. **WS9-ARM-XSEARCH** — suggested VACUOUS/expected_red reclass: NO. The .76 deployment fact confirmed live
   (retry 400, verbatim .76 phrasing); the FAIL is pin-scoped-to-first-request plus first-response transport
   loss (record-only observation).
4. **codegraph degraded-daemon** — contributing context for T21-MSG-S5 / T21-QWEN (fallback-mode daemon can't be
   proxied) but NOT the root (the live daemon PID 98791 holds the worktree lock); pure red herring for GLM52
   (the call never reached codegraph).
5. **WS9-S05** — handoff's "silent no-op auto-compact loop (200 responses, no artifact, no error, context
   unshrunk)": refined. It is NOT an auto-compact no-op loop — the compacts that fired are the background
   two-pass PREFIRE (a different path with no artifact surface by design, failing silently by design), the seed
   sits in the 75–85% prefire band so the artifact-producing hard compact never fired, and the "200, no error"
   is a proxy-side silent empty stream which the binary DID classify (EmptyResponse → terminal after 1 attempt;
   had it been the hard path, a user-visible failure + suppression would have surfaced).

## Triage completeness

18/18 FAILs adjudicated. Existing beads engaged: .90, .66, .36, .71 (×3), .72 (lane, ×3), .76, .78, .21 (lane,
×2), .16 (closed), .65 (RCA class for .66), .89 (explicitly excluded). New proposals: V2-SPAWN-ROOT-1,
XRESP-ARGTRUNC-1, COMPACT-STORM-1 (+2 optional). Signatures rows proposed: #11–#16 (coordinator applies).
No code, campaign-dir, or signatures.json edits made by this lane; at-enc-probe-unpin.json and the
CATALOG-HYDRATE lane untouched (sibling-owned).
