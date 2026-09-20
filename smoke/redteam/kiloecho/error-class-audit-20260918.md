# Error-class audit — observed classes vs triage signatures (gap list)

Date: 2026-09-18 · Author/seat: glm read-only intel seat (apex-v2-grok-build, kiloecho recon, RETRY) · Bead: `apex-6vu` · Status: **DRAFT**
Sources: `smoke/triage/signatures.json` @0fc1060 (18 rows, first-hand parse); `plans/xwire/crosswire-matrix.md` §4-§7; `plans/provenance/deliverable-map-20260918.md` D-11; `smoke/redteam/report-r3r/` verdict trees; live probes P1-P6 (this seat, §5 of wire-topology doc). Key sha12 `9f3f56a263da`. Raw-key sweep: 0. Classifier first-hand: `retry.rs:105-186` (`classify_error` → `RetryDecision::Fatal` fallthrough `:186`); predicates in `xai-grok-sampling-types`.

Triage status legend: **PLUMBED** = OPEN/IGNORED row with RCA; **PROPOSED** = signatures row filed, pending promote; **MISSING** = no triage row. Root-cause layer: harness / proxy / model / deploy / infra-seat.

## §1 — Wire-error classes (the campaign-relevant surface)

| # | Class · exact signature (first-hand or cited) | observed-on | root-cause layer | triage status | owner bead |
|---|---|---|---|---|---|
| W1 | `Invalid 'input[N].id': ''` — empty-id round-trip (messages-wire `id:""` replayed onto strict responses → Azure 400) | incident 01a0b046; R3 cells vxm-az/az-vlq | harness (persist mint gap + strict replay) | **PROPOSED** (sig #13) | `apex-ayl.69` (.77 ingress-normalize re-pins id/encitem_ to absent) |
| W2 | `Input should be 'text' [type=literal_error, input_value='input_text']` — vLLM responses→chat shim rejects responses-dialect content parts (glm-5.2 group); TRIGGER-DEPENDENT | triage #14 RCA; predecessor-death 2026-09-18 (codex-combined v1 transport) | proxy (litellm shim) + infra-seat (codex-combined wire routing) | **PROPOSED** (sig #14) | `apex-xt2` |
| W3 | `Expected the 'type' field of a(n) 'tools' array element to be 'function'; found 'x_search'` — grok-4.6 Vertex rejects hosted x_search tool | .78 ship-gate recon | deploy (vertex tool policy) | **PROPOSED** (sig #15) | `apex-ayl.76` |
| W4 | `Unsupported Responses API input item type: "compaction_trigger"` — compact-400 storm (remote-compaction-v2 trailing trigger rejected; no model-bound arm on compact path; 15× retry) | incident 01a09be2 lineage, 2026-09-17 dogfood | proxy (rejects trigger) + harness (no compact-path strip arm) | **PROPOSED** (sig #16) | `apex-ayl.82` |
| W5 | `Unexpected reasoning effort high. Supported types are xhigh (default), medium, and low.` — xhigh→high selective clamp on `/v1/messages`→openai-class bridge (qwen rejects "high"; medium passes) | matrix §7.1 M-1; **probe P6=400 this seat** | proxy (deployed image house-patch delta; NOT in v1.90.0 checkout) | **PROPOSED** (sig #17) | `apex-ayl.83` |
| W6 | class-(a) vLLM 400 content-array-too-long — `input[N].content array too long` (vLLM pydantic dotted `input.N.id` without "invalid") — **LAPSED** on deployed qwen/glm (clean 200, probes P1/P2) | R2/R3 r2/r3 trees; RULING 3 07:08Z; 20:25Z L2 | model/proxy (vLLM shim strictness) — LAPSED (drift) | **MISSING** (no sig row; BRICK-risk register matrix §4.2) | — (re-pin to [200], P6 clean-200) |
| W7 | Azure 400 `input[N].content array too long, expected max 0` (strict-sol class — strict rows forbid non-empty reasoning.content on replay) | matrix §4.2 BRICK register; F5 | harness (strict projector gap pre-F2) / model (Azure strict schema) | **MISSING** (no sig row) | .74 owns (pair-aware strip) |
| W8 | 401 `x-litellm-tags` East-US-2 affinity — "not allowed to access model due to tags" (model-bound, deployment-routing boundary flip) | D-11 A/B/D probes; incident 01a09d0e | deploy (Azure pool affinity) + proxy (tags routing) | **MISSING** as distinct row (covered by D-11 classifier family-6 Auth arm, `is_model_bound_history_error`) | `apex-ayl.75` (D-11) — **G17: no committed redteam case** |
| W9 | 403 `Route is blocked` (`type:auth_error`, param=path) — unlisted route blocked by `BlockDisallowedRoutesMiddleware` | **probe P3=403 this seat** (`/v1/responses/compact`); .78 NF-3/S05 | proxy (route allowlist `app/common/routes.py:38`) | **MISSING** (no sig row; .78 ship-gate S05, PR 1914 unmerged) | .78-B ws9 |
| W10 | 403 `Tool is blocked for model '<gemini>'` — per-model tool policy on gemini `/v1/messages` route | matrix §7.2 M-2; gemini-msg wstream cell | deploy/proxy (gemini messages tool policy) | **MISSING** (no sig row) | `apex-ayl.20` |
| W11 | `litellm.APIError … Response API in-stream error` — deterministic in-stream proxy failure during compact stream | matrix §4.2; classifier `is_deterministic_in_stream_error` → Fatal (`retry.rs:159`) | proxy (opaque in-stream) | **MISSING** (no sig row) | — |
| W12 | COMP-3 storm — repeated compact-trigger 400 same payload (max_retries=15; c2 call-count, ≤18 threshold bounds it) | incident 01a09be2; brief §6 RCA; R3 c2≤18 guard | harness (no model-bound arm on compact path) + proxy (trigger reject) | **MISSING** as distinct row (rolled into W4 sig #16 RCA) | `apex-ayl.82` |
| W13 | image_strip hang — 0%-CPU tokio park (image-processing retry arm stalls) | `apex-ayl.73` | harness (tokio async stall) | **MISSING** (no sig row) | `apex-ayl.73` |
| W14 | `history incompatible with the current model` — D-ENC cross-region `cmp_` decrypt boundary mismatch | matrix §0 verdict 3 (D-ENC ungated); D-11 | model (cross-region ciphertext decrypt) + harness (D-ENC unconditional strip) | **MISSING** (no sig row; D-11 model-bound classifier catches the rejection) | `apex-ayl.75` (D-ENC policy-conditional owed) |
| W15 | `UnsupportedParamsError reasoning_effort=xhigh is not supported` — stock litellm v1.90.0 drop-or-raise (NOT the deployed clamp) | `gpt_5_transformation.py:237-250` (source read) | proxy (stock litellm) | **MISSING** (distinct from W5; stock behavior, not deployed) | — (informational; W5 supersedes deployed) |

## §2 — Infra / operational classes (signatures #1-#12; non-wire, recorded for completeness)

| sig# | class · signature | root-cause | status | bead |
|---|---|---|---|---|
| #1 | `spawn_failed/code-graph` — binary missing | infra ( unbuilt MCP binary) | OPEN | `apex-ayl.51` |
| #2 | `spawn_failed/jenkins-jarvis` — binary missing | infra | OPEN | `apex-ayl.51` |
| #3 | `handshake_failed/github` — MCP auth/protocol | infra | OPEN | `apex-ayl.51` |
| #4 | `timeout/codebase-memory-mcp` — cold-start | infra | OPEN | `apex-ayl.51` |
| #5a | `| subagent failed |` — max_tokens truncation (opus plan agent 01a0a5f4) | harness (max_tokens) | OPEN | `apex-ayl.51` |
| #5b | `| shell.turn.inference_failed |` — same root as #5a | harness | OPEN | `apex-ayl.51` |
| #6 | `failed to deserialize parameters` / `expected usize` — negative int into usize pty param | harness (pty client validation); wire/model-agnostic, 2 grok-4.6 instances | OPEN | `apex-ayl.51` |
| #7 | `| paywall_check_error |` — steady-state noise (n~2736) | infra (no correlated failure) | IGNORED | `apex-ayl.51` |
| #8 | `| term.writer.blocked |` — terminal backpressure (n=16) | infra | IGNORED | `apex-ayl.51` |
| #9 | `auth gate: Unknown BYOK` — unknown provider key class (n=76/6 sessions) | infra (config/key-class) | PROPOSED | `apex-ayl.51` |
| #10 | `| shell.turn.inference_retry |` — retry surface (n=7) | harness (transient/max_tokens family of #5) | PROPOSED | `apex-ayl.51` |
| #11 | `| leader.response.orphaned |` — multi-agent v2 dropped leader resp (n=4) | harness (v2 coordination) | PROPOSED | `apex-ayl.51` |
| #12 | `| turn.terminal_failure |` — terminal turn fail (n=2; correlates #5) | harness | PROPOSED | `apex-ayl.51` |

## §3 — Adjacent / infra (the glm transport-400 — predecessor death, 2026-09-18 ~23:2xZ)

**Signature (predecessor death, recorded fresh):** `litellm.BadRequestError OpenAIException · 39 validation errors for ChatCompletionRequest · messages.N.content … type should be 'text' (got 'input_text') … Model Group=glm-5.2`.

**Classification:** infra/seat-transport (codex-combined v1 runtime), **ADJACENT to — not part of — the product under test**. The product (grok harness) configures glm-5.2 on `api_backend="responses"` (`~/.grok/config.toml:35`) with `model_family="glm"` (`:36`), so in the product glm rides `/v1/responses` where `input_text` parts are native (probe P2=200). The 400 fires on the codex-combined v1 transport when a user/tool-result message is replayed as responses-shape content parts on the **chat_completions** wire — the responses→chat shim (`litellm_responses_transformation/transformation.py:27`) expects `text`, receives `input_text`.

**Triage mapping:** this IS signatures row **#14** (`input_value='input_text'`, class `wire.request_400`, bead `apex-xt2`, PROPOSED). The predecessor-death instance is a **new occurrence of an already-PROPOSED class**, not a novel class. Row #14 RCA: "vllm Responses->ChatCompletions shim (litellm, glm-5.2 group) rejects responses-dialect content parts (input_text). TRIGGER-DEPENDENT, not a [steady] … non-blocking for the grok campaign."

**Backend-routing asymmetry (task hypothesis verification):** "glm rides chat_completions → unconverted parts leak; qwen rides responses → parts native" — **NOT confirmed from config.** The grok product config has BOTH glm and qwen on `api_backend="responses"` (`:35,:59`). The codex-combined transport (`~/.codex/config.toml`) has a single `model_providers.llm_proxy` (`:8-10`, base_url the same gateway) and NO per-model `api_backend`/`model_family`/wire selection — default `model="gpt-5.6-sol"` (`:1`). The glm→chat_completions routing is therefore a **codex-combined v1 runtime-internal behavior, UNVERIFIED from config files**. The observed asymmetry (qwen seat ran fine in parallel) is consistent with qwen riding responses-native in that runtime, but the routing decision is not visible in either config file I can read. **Cite:** `~/.codex/config.toml:8-17` (single provider, "Everything routes through llm_proxy"); `~/.grok/config.toml:35,59` (both responses). Status: UNVERIFIED-routing (runtime-internal).

**Notable effort-menu asymmetry (relevant to W5, verified first-hand):** glm's reasoning menu = {high(default), medium, low} (`~/.grok/config.toml:43-54`); qwen's = {xhigh(default), medium, low} (`:65-72`). glm includes "high" so the W5 xhigh→high clamp is a no-op for glm (consistent with glm-resp PASS); qwen's default "xhigh" is clamped to "high" which qwen rejects (W5). Context-window: grok config glm `context_window=128000` (`:38`) vs proxy `max_input_tokens=262144` (`config_seclab.yaml:1583`) — client policy is tighter (the 256k-tight policy is client-side).

## §4 — Gap list (classes with NO triage row and/or NO case coverage)

| gap class | sig row? | redteam case? | owner | note |
|---|---|---|---|---|
| W6 vLLM content-array-too-long (LAPSED) | NO | re-pin to [200] (P6) | — | BRICK-risk register matrix §4.2; lapsed drift class |
| W7 Azure content-array max-0 (strict-sol) | NO | NO | .74 | F5; pair-aware strip owed |
| W8 401 x-litellm-tags affinity | NO (D-11 classifier only) | NO (G17 gap) | `apex-ayl.75` | D-11 has unit tests + fixtures but NO committed redteam case |
| W9 403 Route is blocked | NO | .78 S05 (PR 1914 unmerged) | .78-B | probe P3 confirms; needs case + merged PR |
| W10 403 Tool is blocked (gemini) | NO | NO | `apex-ayl.20` | M-2 record-only |
| W11 in-stream error (compact) | NO | NO | — | `is_deterministic_in_stream_error` → Fatal; no case |
| W12 COMP-3 storm | NO (rolled into W4 #16) | c2≤18 guard only | `apex-ayl.82` | no dedicated case; bounded by count band |
| W13 image_strip hang | NO | NO | `apex-ayl.73` | no case |
| W14 history-incompatible (D-ENC) | NO (D-11 catches rejection) | NO | `apex-ayl.75` | D-ENC policy-conditional owed |
| W15 stock xhigh drop-or-raise | NO | NO | — | informational (stock v1.90.0; deployed W5 differs) |

**Coverage summary:** 5 of 15 wire-error classes have a PROPOSED triage row (W1-W5 = sig #13-17). 10 are MISSING a triage row. Of those, 3 have a partial safety net (W8/W14 via D-11 classifier+fixtures; W12 via W4 #16 RCA). The BRICK-class gap (W6/W7 — unclassified 400 → Fatal `retry.rs:186`) is the highest-severity missing-coverage item: an unclassified 400 is terminal and persistent until compaction, with no triage row and no dedicated case.

— end —
