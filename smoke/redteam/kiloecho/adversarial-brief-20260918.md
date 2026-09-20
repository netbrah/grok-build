# Adversarial brief — boundary-condition red-team campaign

Date: 2026-09-18 · Author/seat: glm read-only intel seat (apex-v2-grok-build, kiloecho recon, RETRY) · Bead: `apex-6vu` · Status: **DRAFT**
Sources: `plans/xwire/crosswire-matrix.md` §2-§5; `plans/xwire/golden-engine-capability-glm-20260918.md` §2; `plans/provenance/deliverable-map-20260918.md`; `smoke/redteam/cases/xw-vlq-vlg.json` (case-shape template, first-hand); `smoke/triage/signatures.json`; live probes P1-P6. Harness @0fc1060; proxy @956b6d2; litellm v1.90.0. Key sha12 `9f3f56a263da`. Raw-key sweep: 0.
Case-shape convention (from `cases/xw-*.json`): `{schema_version,id,title,bead,suite,tier,driver,model,wirecap,steps[],watchdog_s,est_calls,assert{ndjson[],wire[]}}`; step ops = `switch_model`({model,via,cell,assert_form}) / `turn`({prompt}); wire assert kinds = `grep`/`golden` with `where`/`any`/`grep`/`kind`.

## Surface 1 — Cross-wire switch: responses↔messages foreign-reasoning replay

**Hypothesis.** R8 identity suppression + stage-6 thinking-strip are PROVEN on the messages side (VX-M target, `messages.rs:676-678`/`:744-760`/`:266-305`). Responses-side replay of foreign reasoning (an anthropic `thinking_block`-originated item, `id:""`, `encrypted_content`=signature) onto a `/v1/responses` target is **UNPROVEN vs the proxy**: D-ENC strips the `encrypted_content` field (`provider.rs:209`) but `id:""` + summary ride. The litellm /messages→responses bridge silently drops signatures (matrix §4.3-5; v1.90.0 path `litellm/interactions/litellm_responses_transformation/transformation.py:27`). If the shim rejects `id:""` with an unclassified phrasing → BRICK (W6/W7, terminal Fatal `retry.rs:186`).
**Existing coverage.** R3 cells vxm-az (idx6-reasoning RED-EXPECTED), vxm-vlq, vxm-vlg — but these seed SYNTHETIC history, not a LIVE messages→responses switch with a real `thinking_block`. OQ-11 (lenient-shim empty-id tolerance) OPEN.
**Proposed case** `xw-vxm-az-live-replay`:
```json
{"schema_version":1,"id":"XW-VXM-AZ-LIVE","bead":"apex-6vu","suite":"cross-wire","tier":"slim","driver":"acp","model":"claude-sonnet-5","wirecap":true,
 "steps":[{"op":"turn","prompt":"Think step by step: what is 17*23? Show reasoning, then the answer."},
          {"op":"switch_model","model":"gpt-5.6-sol","via":"acp","cell":"smoke/xwfix/cells/vxm-az-live","assert_form":"storage"},
          {"op":"turn","prompt":"Reply with exactly: XW-VXM-AZ-POSTSW1"}],
 "watchdog_s":300,"est_calls":4,
 "assert":{"ndjson":[{"op":"absent","event":"acp_error","label":"post-switch turn did not die"}],
           "wire":[{"kind":"grep","file":"req-*.json","where":{"method":"POST","path":"/v1/responses","body.model":"gpt-5.6-sol"},"any":true,
                    "grep":"\"type\": \"reasoning\"","label":"foreign reasoning crossed the responses wire (anti-vacuous)"},
                   {"kind":"grep","file":"req-*.json","where":{"method":"POST","path":"/v1/responses"},"any":true,
                    "grep":"input\\[\\d+\\].id|content array too long","label":"BRICK watch: unclassified 400 phrasing (MISS = clean)"}]}}
```
**Risk tier.** HIGH (BRICK = terminal persistent 400 until compaction; family-unset default opus-5 concentrates exposure, OQ-14).
**Owner lane.** .74 (BRICK phrasing coverage) / .72 (matrix sweep).

## Surface 2 — Cross-model resume: session file → different family

**Hypothesis.** Resuming a persisted `chat_history.jsonl` (model-A reasoning items) under model-B (different family) must preserve storage invariants: empty-id repair (`.69`/`.77` ingress normalize), `encitem_` affinity markers (cross-boundary → 503 if not stripped, OQ-12), carrier ciphertext (D-ENC). The .71 projector re-keys at SWITCH time on the storage form; a **RESUME** (load disk verbatim) may bypass the projector. A sol session with `encitem_` ids resumed under terra (cross-boundary) → 503 affinity → destructive strip (silent loss, matrix §4.3-1).
**Existing coverage.** `.69` empty-id (`tdd-69-emptyid.md`); `.71` projector (storage form). Resume-vs-switch path coverage thin (ws9 s05/s06/s07, .78-B gated).
**Proposed case** `xw-resume-cross-family`:
```json
{"schema_version":1,"id":"XW-RESUME-CROSSFAM","bead":"apex-6vu","suite":"cross-wire","tier":"slim","driver":"acp","model":"gpt-5.6-sol","wirecap":true,
 "steps":[{"op":"turn","prompt":"Think step by step about recursion, then define it in one sentence."},
          {"op":"persist_resume","target_model":"gpt-5.6-terra","via":"acp"},
          {"op":"turn","prompt":"Reply with exactly: XW-RESUME-POSTSW1"}],
 "watchdog_s":300,"est_calls":4,
 "assert":{"ndjson":[{"op":"absent","event":"acp_error","label":"resume turn did not die"}],
           "wire":[{"kind":"grep","file":"req-*.json","where":{"method":"POST","path":"/v1/responses","body.model":"gpt-5.6-terra"},"any":true,
                    "grep":"encitem_","label":"MISS = encitem_ ids stripped on resume (projector/normalize fired); HIT = 503-affinity leak"},
                   {"kind":"grep","file":"req-*.json","where":{"method":"POST","path":"/v1/responses"},"any":true,
                    "grep":"503|not allowed to access model due to tags","label":"503/401 affinity class watch (MISS = clean)"}]}}
```
**Risk tier.** HIGH (503 affinity → destructive strip, permanent silent loss; backup-only recovery).
**Owner lane.** .71 (projector resume path) / .75 (D-ENC policy-conditional).

## Surface 3 — Subagents: v1 pool vs v2 spawn_agent; COMMS-TRUST; DEAD-SEAT

**Hypothesis.** (a) **COMMS-TRUST** (`apex-ayl.66`): a fresh-context child received launch work labeled "not user consent" and refused — the child's trust boundary rejected parent-injected work. Cross-pool comms (v1 pool ↔ v2 `spawn_agent`) may mislabel/drop the consent signal → functional deadlock. (b) **DEAD-SEAT**: a transport 400 (the glm transport-400, W2/signature #14) kills a lane mid-recon; without files-on-disk-first, the lane's work is lost. This seat's survival rule IS the mitigation.
**Existing coverage.** signatures #11 (`leader.response.orphaned`, v2 coordination, PROPOSED); #5a/#5b (subagent_failed). NO COMMS-TRUST case; NO DEAD-SEAT case.
**Proposed case (COMMS-TRUST)** `sub-comms-trust-v2spawn`:
```json
{"schema_version":1,"id":"SUB-COMMS-TRUST","bead":"apex-6vu","suite":"subagent","tier":"slim","driver":"acp","model":"glm-5.2","wirecap":false,
 "steps":[{"op":"spawn_agent","task":"Read README.md and reply with its first heading","model":"qwen3.8-27b"},
          {"op":"turn","prompt":"Reply with the child's first heading, or report the child refused."}],
 "watchdog_s":180,"est_calls":3,
 "assert":{"ndjson":[{"op":"absent","event":"child_refusal","label":"MISS = child accepted launch work as authorized (COMMS-TRUST clean)"},
           {"op":"present","event":"spawn_agent_ack","label":"child acknowledged the spawned task"}]}}
```
**Proposed case (DEAD-SEAT)** `sub-dead-seat-recovery`:
```json
{"schema_version":1,"id":"SUB-DEAD-SEAT","bead":"apex-6vu","suite":"subagent","tier":"slim","driver":"acp","model":"glm-5.2","wirecap":false,
 "steps":[{"op":"turn","prompt":"Write a 3-line note to /tmp/dead-seat-probe.md, then switch to a failing model."},
          {"op":"switch_model","model":"<transport-400-trigger>","via":"acp"},
          {"op":"assert_files","paths":["/tmp/dead-seat-probe.md"],"label":"partial deliverable survives the transport death"}],
 "watchdog_s":120,"est_calls":2,
 "assert":{"ndjson":[{"op":"present","event":"transport_400","label":"the dead-seat trigger fired (signature #14 class)"},
           {"op":"absent","event":"unrecoverable_loss","label":"coordinator recovered from disk"}]}}
```
**Risk tier.** MEDIUM (COMMS-TRUST = functional deadlock; DEAD-SEAT = lost work, recoverable by files-on-disk-first protocol).
**Owner lane.** .66 (COMMS-TRUST) / coordinator (DEAD-SEAT protocol — files-on-disk-first is the proven mitigation).

## Surface 4 — Compact: acp-triggered vs /compact; preemptive wall; store=false; remote-v2 gate

**Hypothesis.** (a) Preemptive lossy compact on family switch (vlq-vlg wall, stable 2/2) eats the ride — VERIFIED wall not bridge (`is_family_switch` `model_switch.rs:60-68`; wall `:144-163`; `strip_reasoning_blocks` `compaction_utils.rs:89-93`). (b) `store=false` invariant: proxy forbids `store=true` (`config_seclab.yaml:1572` glm `store:False`; harness sets `store=Some(false)` `client.rs:2530`); a `store=true` leak = server-side persistence (data exfil). (c) **remote-v2 gate VERIFIED first-hand**: gate = `ResponsesWireDialect::Codex` (`provider.rs:63`) + `ApiBackend::Responses` (`client.rs:2625`) — NOT an `"OpenAI"` literal; sol/terra/luna pass (family `codex`) but `remote_compaction_v2=false` (`~/.grok/config.toml:308`) globally disables → local self-summarize. (d) The compact path has NO model-bound arm (W4 RCA) → COMP-3 storm (max_retries=15, c2≤18 bound).
**Existing coverage.** vlq-vlg wall cell (UNPROVEN, .68 owns); `.82` COMPACT-BOUNDARM (`tdd-82`); W4 sig #16; R3 c2≤18 guard.
**Proposed case** `compact-acp-vs-explicit-store`:
```json
{"schema_version":1,"id":"COMPACT-ACP-EXPLICIT","bead":"apex-6vu","suite":"compact","tier":"slim","driver":"acp","model":"qwen3.8-27b","wirecap":true,
 "steps":[{"op":"seed_context","tokens":"~115k","label":"near 85% of 128k client window"},
          {"op":"turn","prompt":"Reply: PRE-COMPACT"},
          {"op":"compact","via":"acp_auto","label":"acp-triggered at threshold"},
          {"op":"compact","via":"explicit","label":"explicit /compact"},
          {"op":"turn","prompt":"Reply: POST-COMPACT"}],
 "watchdog_s":300,"est_calls":6,
 "assert":{"ndjson":[{"op":"absent","event":"acp_error","label":"compact did not die"},
           {"op":"le_threshold","metric":"c2_call_count","max":18,"label":"COMP-3 storm bound (≤18)"}],
           "wire":[{"kind":"grep","file":"req-*.json","where":{"method":"POST","path":"/v1/responses"},"any":true,
                    "grep":"\"store\": true","label":"MISS = store=false invariant held (no exfil)"},
                   {"kind":"grep","file":"req-*.json","where":{"method":"POST","path":"/v1/responses"},"any":true,
                    "grep":"compaction_trigger","label":"trigger item present on compact requests (expected); pair with 400 watch"}]}}
```
**Risk tier.** HIGH (COMP-3 storm = 15× retry burn; `store=true` leak = data exfil).
**Owner lane.** .82 (compact-boundarm) / .62 (wall RCA) / .68 (vlq-vlg wall).

## Surface 5 — MCP tool calls cross-model (tool-result pairing; vertex T3 pair-atomic drop)

**Hypothesis.** A `function_call` minted by model-A replayed to model-B (cross-family) must keep its `function_call_output` paired. The `.74` orphan cleanup (`drop_orphaned_tool_results`, XW-ORPHAN-1) drops dangling ToolResults at send. The vertex T3 pair-atomic drop (projection.rs, in-flight .71 cut) atomically drops a pair if the target dialect can't represent it. grok-4.6 rejects `x_search` tool type (W3). A `multi_agent_mode` dev item (v2 splice) replayed onto a non-codex target may 400.
**Existing coverage.** `.74` XW-ORPHAN (pair-aware strip, 7 REDs, `tdd-74-orphan.md`); W3 sig #15 (x_search). T3 pair-atomic = projection.rs (.71 in-flight).
**Proposed case** `xw-mcp-pair-crossmodel`:
```json
{"schema_version":1,"id":"XW-MCP-PAIR-CROSS","bead":"apex-6vu","suite":"cross-wire","tier":"slim","driver":"acp","model":"qwen3.8-27b","wirecap":true,
 "steps":[{"op":"turn","prompt":"Use the calc tool to compute 2+2, then state the result.","tools":["calc"]},
          {"op":"switch_model","model":"grok-4.6","via":"acp","cell":"smoke/xwfix/cells/vlq-vxr-mcp","assert_form":"storage"},
          {"op":"turn","prompt":"Reply with exactly: XW-MCP-PAIR-POSTSW1"}],
 "watchdog_s":300,"est_calls":4,
 "assert":{"ndjson":[{"op":"absent","event":"acp_error","label":"post-switch turn did not die"}],
           "wire":[{"kind":"grep","file":"req-*.json","where":{"method":"POST","path":"/v1/responses","body.model":"grok-4.6"},"any":true,
                    "grep":"x_search","label":"MISS = no x_search tool (grok-4.6 rejects it, W3)"},
                   {"kind":"golden","file":"req-*.json","where":{"method":"POST","path":"/v1/responses","body.model":"grok-4.6"},"any":true,
                    "normalize":[],"label":"pair-atomic: tool_call+tool_result both present OR both absent (no orphan)"}]}}
```
**Risk tier.** MEDIUM (orphan → 400 BRICK; pair drop = silent loss).
**Owner lane.** .74 (orphan) / .71 (T3 projection).

## Surface 6 — Mid-session /effort + reasoning (xhigh clamp; 0-reasoning-tokens; context_window override)

**Hypothesis.** (a) xhigh clamp on vLLM via `/v1/messages` (W5, proven, probe P6=400). Mid-session `/effort` change to xhigh on qwen on the RESPONSES wire is clean (probe P1 qwen-resp xhigh-default=200) — the clamp is **messages-bridge-only**. Discriminate. (b) 0-reasoning-tokens: grok-4.6 returns rtok=0 (OQ-7 deploy issue); gemini returns token-count-only no frames (OQ-2). A mid-session switch TO grok-4.6 produces a 0-reasoning turn — does the harness persist a reasoning item or drop it? (c) per-model `context_window` override: glm grok-config `128000` (`:38`) vs proxy `262144` (`config_seclab.yaml:1583`) — the 256k-tight policy is client-side. Auto-compact at 85% (`compaction.rs:22`) must key off the CLIENT 128k, not the proxy 262k.
**Existing coverage.** W5 sig #17; OQ-7 (grok rtok=0); OQ-2 (gemini); vlq-vlg-ws c2 guard. NO mid-session /effort case; NO context_window-override case.
**Proposed case** `xw-effort-midsession-clamp`:
```json
{"schema_version":1,"id":"XW-EFFORT-MID","bead":"apex-6vu","suite":"cross-wire","tier":"slim","driver":"acp","model":"qwen3.8-27b","wirecap":true,
 "steps":[{"op":"turn","prompt":"Reply: EFFORT-BASELINE","effort":"xhigh"},
          {"op":"set_effort","effort":"xhigh","wire":"messages","label":"force /v1/messages override wire"},
          {"op":"turn","prompt":"Reply: EFFORT-MSG-XHIGH"}],
 "watchdog_s":180,"est_calls":3,
 "assert":{"ndjson":[{"op":"absent","event":"acp_error","label":"responses-wire xhigh turn did not die (clean expected)"}],
           "wire":[{"kind":"grep","file":"req-*.json","where":{"method":"POST","path":"/v1/messages","body.model":"qwen3.8-27b"},"any":true,
                    "grep":"Unexpected reasoning effort high","label":"HIT = W5 clamp reproduced on messages bridge; MISS on /v1/responses = clamp is messages-only (discriminator)"}]}}
```
**Proposed case (context_window)** `xw-ctxwin-override-compact`:
```json
{"schema_version":1,"id":"XW-CTXWIN-OVERRIDE","bead":"apex-6vu","suite":"compact","tier":"slim","driver":"acp","model":"glm-5.2","wirecap":true,
 "steps":[{"op":"seed_context","tokens":"~110k","label":"under 128k client window, under 262k proxy limit"},
          {"op":"turn","prompt":"Reply: CTXWIN-PROBE"}],
 "watchdog_s":300,"est_calls":4,
 "assert":{"ndjson":[{"op":"present","event":"compaction_trigger","label":"auto-compact fired at 85% of CLIENT 128k (~109k), not proxy 262k (~223k)"}]}}
```
**Risk tier.** MEDIUM (effort clamp = 400 on override wire; ctxwin miscompute = premature-or-late compact).
**Owner lane.** .83 (effort clamp) / .62 (compact threshold).

## Close — top-5 attacks ranked (likelihood × blast radius) + best-discriminating case

| rank | attack | likelihood | blast radius | best-discriminating case |
|---|---|---|---|---|
| 1 | BRICK on cross-wire foreign-reasoning replay (S1) | HIGH (family-unset default opus-5, OQ-14) | HIGH (terminal persistent 400 until compaction) | `xw-vxm-az-live-replay` — live thinking_block → strict responses; BRICK-watch grep for unclassified 400 phrasing |
| 2 | COMP-3 compact storm (S4) | HIGH (compact path has no model-bound arm) | HIGH (15× retry burn, c2 budget drain) | `compact-acp-vs-explicit-store` — c2≤18 threshold + store=false grep |
| 3 | 503 affinity on cross-model resume (S2) | MEDIUM (resume may bypass .71 projector) | HIGH (destructive strip, permanent silent loss) | `xw-resume-cross-family` — encitem_ grep MISS = projector fired on resume |
| 4 | DEAD-SEAT transport-400 kills lane (S3) | HIGH (happened twice: predecessor + glm transport W2) | MEDIUM (lost work, recoverable by files-on-disk-first) | `sub-dead-seat-recovery` — assert_files survives the transport death |
| 5 | xhigh clamp on /v1/messages override (S6) | MEDIUM (override cells unrun, OQ-5) | MEDIUM (400, recoverable by wire switch) | `xw-effort-midsession-clamp` — HIT on /v1/messages, MISS on /v1/responses = clamp is messages-only |

**Single best-discriminating case overall:** `xw-vxm-az-live-replay` (rank 1) — it is the only case that exercises a LIVE foreign-reasoning item crossing the responses wire (not synthetic seed), directly probing the BRICK gap (the highest-severity missing-coverage item from the error-class audit) and the unproven responses-side replay vs the proxy. A clean 200 closes OQ-11; an unclassified 400 surfaces the BRICK class with a real phrasing to add to the classifier.

— end —
