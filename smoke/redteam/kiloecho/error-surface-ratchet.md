# ERROR-SURFACE RATCHET — kiloecho lane (KE-2)

SEAT: xw_ke2_litellm_qwen (qwen) · apex-v2-grok-build campaign · error-plumbing audit + ratchet mechanism
DATE: 2026-09-19T00:40Z (machine clock) · STATUS: delivered, read-only recon (no code, no live probes)
SIBLING: litellm-transform-matrix.md (the 1.90.0 field-fate matrix — the transformation side of the same question)

Operator question (second half): *"…have we plumbed all the error types?"*

**Answer: NO — PARTIAL.** The client's F1–F9 families plus five PROPOSED triage rows cover the
observed 400 surface, but **14 named emittable shapes** (§1–§2) have no triage row, and the
client's `classify_error` ladder treats unclassified 400/403/424/501 as
`RetryDecision::Fatal` (crates/codegen/xai-grok-sampler/src/retry.rs:186 fallthrough) — an
unclassified deterministic error in a live session is a terminal BRICK, not a retry. The
ratchet mechanism (§3) is designed so the unplumbed surface can never silently widen.

## 1. Error universe — every emittable shape × classification

Legend: F-family = client classifier arm (`is_model_bound_history_error`,
crates/codegen/xai-grok-sampling-types/src/error.rs:476-590; 400-gated except the F1 503
exception and the F6 401). Triage row = `smoke/triage/signatures.json` @0fc1060 (18 rows:
#1-8 MCP/shell/term (apex-ayl.51; #7/#8 IGNORED), #9-12 PROPOSED, #13-17 the F-family rows).
Verdicts: **PLUMBED** (family arm + live evidence; ladder handles as designed) ·
**PROPOSED-COVERED** (triage row filed, pending promote — JIG "row exists" level satisfied) ·
**GAP** (no arm, or arm without a row → unclassified → Fatal fallthrough, or wrong retry) ·
**RECORD-ONLY** (ladder already covers: 429→Backoff, 5xx→Rebuild/Retry, 401→Auth) or moot
under deployed config.

### 1.1 Proxy app layer (llm-proxy; all LLM routes; upstream of litellm)

| # | status · shape (verbatim) | source pin (llm-proxy @956b6d2) | F-family | triage row | verdict |
|---|---|---|---|---|---|
| U1 | 401 malformed credentials | app/api/auth/main.py:219-220 | — (401) | — | RECORD-ONLY (`is_auth_error` = 401-only, error.rs:340-352 → Auth → EmitToSession) |
| U2 | 401 missing credentials | auth/main.py:316-317 | — | — | RECORD-ONLY (as U1) |
| U3 | 403 unknown/expired authorization credentials | auth/main.py:279-287, :304-310 | — (403 NOT auth-classified) | — | **GAP G-1** |
| U4 | 403 InvalidUser | auth/main.py:459 | — | — | **GAP G-1** (same class) |
| U5 | 403 `Route is blocked` (type auth_error, param=path) | app/api/proxy.py:101-147 (_make_403 :139); auth/main.py:232-253 | — | — | advisory (NF-3 PROVEN, ws9 probe; client never calls an unlisted route; catalog row owed to .78-B) |
| U6 | 400 "You must pass a 'user' json field to your request" | auth/main.py:321-322 | — | — | **GAP G-4** |
| U7 | 503 "Service temporarily unavailable, please retry" (DB) | auth/main.py:341-343 | — | — | RECORD-ONLY (5xx → Rebuild/Retry) |
| U8 | 403 "Access to this model requires explicit authorization" | auth/main.py:361-376 | — | — | **GAP G-5** |
| U9 | 403 "Access from {app} requires explicit application authorization on this key" | auth/main.py:397-401 | — | — | **GAP G-6** |
| U10 | 501 "Storing the generated model response for later retrieval is not supported. Please set 'store' field to False or remove it from the request" | auth/main.py:406-413 (POST /v1/responses only) | — | — | **GAP G-2** |
| U11 | 501 "Background mode is not supported. Please remove 'background' field from the request" | auth/main.py:417 | — | — | **GAP G-2** |
| U12 | 424 "Unable to validate user at this time, please retry" (LDAP) | auth/main.py:452-454 | — | — | **GAP G-8** (424 ∉ retryable set {429, 5xx−{525,526}} → Fatal despite "please retry") |
| U13 | 403 "Tool type is blocked for model '<m>'" (claude web_search*/web_fetch*; gemini typed) | app/api/auth/tool.py:44-70 (via auth/main.py:420-424) | — | — | **GAP G-3** (claude variant PREDICTED — probe P-B) |
| U14 | 403 "Tool is blocked for model '<m>'" (gemini untyped tool dict) | tool.py:71-78 | — | — | **GAP G-3** (M-2 PROVEN; apex-ayl.20 lane) |
| U15 | 403 web_search* (other/azure rows) | tool.py:79-89 | — | — | **GAP G-3** (same class) |

### 1.2 litellm router (pre-upstream)

| # | status · shape | source pin (litellm @v1.90.0) | F-family | triage row | verdict |
|---|---|---|---|---|---|
| U16 | 401 "Not allowed to access model due to tags configuration. Passed model=… and tags=[…]" | types/router.py:487-489 → proxy/_types.py:3337-3340; router_strategy/tag_based_routing.py:223-226 | **F6** (401 + tags-config needle) | no distinct row (D-11 classifier arm + fixtures; .75) | RECORD-ONLY — PLUMBED family (PROVEN testdata/affinity/probe-d-401-body.json) |
| U17 | 429 "No healthy deployment available" / "No deployments available for selected model" | proxy/_types.py:3331-3335 | — | — | RECORD-ONLY (429 → Backoff) |
| U18 | 429 "Crossed TPM / RPM / Max Parallel Request Limit" | proxy/_types.py (CommonProxyErrors) | — | — | RECORD-ONLY (429 → Backoff) [status PREDICTED] |
| U19 | 500 UnsupportedParamsError "<provider> does not support parameters: {…}, for model=…" | responses/utils.py:41-72 | — | — | RECORD-ONLY (moot under deployed `drop_params: true`, config_seclab.yaml:1660; becomes GAP if the config flips) |

### 1.3 Adapter level (per wire pair; wire-pair ids from matrix §3)

| # | status · shape | wire pair | source pin | F-family | triage row | verdict |
|---|---|---|---|---|---|---|
| U20 | 400 "Unexpected reasoning effort high. Supported types are xhigh (default), medium, and low." (M-1) | B1 messages→openai/qwen (vLLM) | vLLM body; clamp stock at experimental_pass_through/utils.py:16-57 via responses_adapters/handler.py:80-90 | — | **#17 PROPOSED** (apex-ayl.83) | PROPOSED-COVERED (PROVEN 20260918T200643Z/qwen-msg/capture/req-004.json) |
| U21 | 400 "Invalid 'input[N].id': ''" (W1, empty-id replay) | A6 azure strict rows | Azure body | **F3** | **#13 PROPOSED** (apex-ayl.69) | PROPOSED-COVERED (PROVEN incident 01a0b046 / R3 vxm-az) |
| U22 | 400 "Input should be 'text' [type=literal_error, input_value='input_text']" (W2) | C1 chat→vLLM (adjacent codex-combined transport) | chat-wire shim (error-class-audit §3) | — | **#14 PROPOSED** (apex-xt2) | PROPOSED-COVERED (PROVEN 2026-09-18 predecessor-death) |
| U23 | 400 "Expected the 'type' field of a(n) 'tools' array element to be 'function'; found 'x_search'" (W3) | A4 vertex grok | vertex body | — | **#15 PROPOSED** (apex-ayl.76) | PROPOSED-COVERED (PROVEN .78 ship-gate recon) |
| U24 | 400 "Unsupported Responses API input item type: \"compaction_trigger\"" (W4) | compact path, POST /v1/responses | proxy/litellm body | **F9** | **#16 PROPOSED** (apex-ayl.82) | PROPOSED-COVERED (PROVEN incident 01a09be2) |
| U25 | 400 "N validation errors for ChatCompletionRequest messages.N.ChatCompletionMessageGenericParam.content.str Input should be a valid string [type=string_type, input_value=[{…" (F8 class-(a), empty-id ride) | A1/A2 responses→vLLM (qwen + glm rows, same ONPREM_INFERENCE_API_BASE) | vLLM **server-side** responses→ChatCompletions shim; mechanism derivation smoke/xwfix/cells/vxm-vlq/cell.json | **F8** ("validation error"+(messages.\|input.) — arm exists in current code) | **none** | **GAP G-7** (PROVEN L1801 01a0b07a 17:47Z + L1881 third BRICK instance; fragment sha256:12 1f591ef070d9; the BRICK instances predate the F8 arm) |
| U26 | 400 "input[N].content array too long, expected max 0" (W7, strict-sol) | A6 azure strict rows | Azure body | **F5** | none | **GAP G-13** (.74 owns pair-aware strip) |
| U27 | 400 "input[N].content array too long" (W6, vLLM — LAPSED) | A1/A2 responses→vLLM | vLLM body | **F5** | none | **GAP G-14** (lapsed drift class; re-pin case to [200] per P6; brief §3: lapsed classes must stay lapsed) |
| U28 | 400 "gpt-5 models don't support temperature={}. Only temperature=1 is supported…" | A6/C3 azure gpt-5 | openai/responses/transformation.py:99-127 | — | none | **GAP G-11** (low exposure: AZ rows drop temperature, config :252,…,952) |
| U29 | 500 "Invalid reasoning effort: {x}" (ValueError) | A3 responses→vertex gemini (bridge); C2 chat→vertex gemini | vertex_and_google_ai_studio_gemini.py:946/:999 | — | none | **GAP G-9** (version-dependent: 1.90 branches lack the alias patch; probe P-A) |
| U30 | 400 "Invalid request format: {error_msg}" (AttributeError-in-chain) | all (malformed-body class) | common_request_processing.py:2271-2426 | — | none | **GAP G-12** (low exposure: client bodies are well-formed) |

### 1.4 Upstream relay / stream (all routes)

| # | status · shape | source pin | F-family | triage row | verdict |
|---|---|---|---|---|---|
| U31 | non-stream upstream 4xx/5xx: raw body passthrough, exc status 400-599 preserved else 500 | common_request_processing.py:2271-2426 | per-shape (F1–F9 needles) | per-instance (JIG drill) | mechanism — open class by design; each observed instance gets a needle + row |
| U32 | first stream-chunk error → JSON with real status | common_request_processing.py:359-391 | as U31 | as U31 | mechanism |
| U33 | mid-stream upstream error relayed as raw `data:` SSE frame + [DONE]; serialization fail → `data: {str(e)}` | proxy_server.py:7330-7400 | W11 instance "litellm.APIError … Response API in-stream error" → `is_deterministic_in_stream_error` → Fatal (retry.rs:159) | none | **GAP G-10** (open class; probe P-F captures the shape) |
| U34 | 503 encrypted-content affinity (cross-org pointer / boundary flip) | router_utils/pre_call_checks/encrypted_content_affinity_check.py:1-40 (**1.93/current branches only** — absent on 1.90 branches) | **F1** (503-exception double-keyed encrypted+(unavailable\|boundary)) | row not verified by this seat (not among #13-17; #9-12 contents unverified) | PLUMBED family (PROVEN 20260916T191948Z/wire/resp-003.jsonl; probe-C KEEP) — catalog-row verification owed to the ratchet |
| U35 | timeout → 500 | litellm/exceptions.py:334 (Timeout : openai.APITimeoutError) | — | — | RECORD-ONLY (5xx → Rebuild/Retry) |
| U36 | 499 client disconnect | proxy_server.py:7330-7400 | — | — | n/a (client-side) |

Universe total: 36 shapes — PLUMBED/RECORD-ONLY/PROPOSED-COVERED = 20; **GAP = 14 named
(G-1…G-8, G-9, G-10…G-14)** + 1 advisory (U5, owner .78-B) + 1 catalog-row verification owed
(U34).

## 2. Gap list (exact phrasing · wire pair · exposing cell · ratchet action)

| gap | exact shape (verbatim) | wire pair | exposing live cell | client today | ratchet action | bead owed |
|---|---|---|---|---|---|---|
| G-1 | 403 unknown/expired authorization credentials; 403 InvalidUser | all LLM routes (pre-litellm) | none live (would require a lapsed key) — operator re-run with an expired key exposes it | Fatal (403 ∉ `is_auth_error`) — no refresh path | triage row + client decision: extend auth classification to this 403 class (refreshable) or document as fatal | new |
| G-2 | 501 "Storing the generated model response for later retrieval is not supported. Please set 'store' field to False or remove it from the request" / 501 "Background mode is not supported. Please remove 'background' field from the request" | POST /v1/responses | none (client sends store:false; rows are store:False) — probe P-D | 501 → 5xx → Rebuild/Retry = **wrong** (deterministic gate; retry storm to max_retries, then Fatal) | triage row + client: 501 → Fatal (no retry) or config enforcement | new |
| G-3 | 403 "Tool type is blocked for model '<m>'" (typed) / 403 "Tool is blocked for model '<m>'" (untyped = M-2) / 403 web_search* | /v1/messages PROVEN (M-2 gemini-msg cell); /v1/responses scope PREDICTED (hook at auth/main.py:420-424) | M-2 PROVEN (gemini-msg 403); claude variant = probe P-B | Fatal (403 unclassified) — right outcome (policy violation), uncataloged | triage row (M-2 = apex-ayl.20 lane); probe P-B; JIG row advisory (403 outside JIG's literal 400/500 scope) | apex-ayl.20 |
| G-4 | 400 "You must pass a 'user' json field to your request" | LLM routes (where auth mode requires the user field) | none live | Fatal (400 unclassified) | triage row; harness-side check that the user field is always present (config-gap class) | new |
| G-5 | 403 "Access to this model requires explicit authorization" | all (key-scoped) | none live | Fatal | triage row (advisory); key-management class | new |
| G-6 | 403 "Access from {app} requires explicit application authorization on this key" | all (app-scoped) | none live | Fatal | triage row (advisory); key-management class | new |
| G-7 | 400 "N validation errors for ChatCompletionRequest messages.N.ChatCompletionMessageGenericParam.content.str Input should be a valid string [type=string_type, input_value=[{…" (F8 class-(a)) | A1/A2 responses→vLLM (qwen, glm rows — same ONPREM_INFERENCE_API_BASE) | PROVEN L1801 (01a0b07a 17:47Z) + L1881 (third BRICK instance); fragment sha256:12 1f591ef070d9 | current code: F8 arm → model-bound Strip (post-arm); pre-arm instances = Fatal BRICK (the incidents) | triage row (KE-3 files) + xwfix cell pinning the shape; probe P-E pins the trigger (empty-id vs encrypted ride); fix lane = .77 ingress-normalize re-pin | new (interacts with apex-ayl.69/.74) |
| G-8 | 424 "Unable to validate user at this time, please retry" | all (LDAP path) | none live | Fatal (424 ∉ retryable set) **despite** "please retry" | triage row + one-line client fix: add 424 to the retryable set (transient by design) | new |
| G-9 | 500 "Invalid reasoning effort: {x}" | A3 responses→vertex gemini (bridge); C2 chat→vertex gemini | none live (M-2 died at the proxy gate first; grok = param dropped, not 500) — probe P-A | 500 → Rebuild/Retry (wrong: deterministic per effort value) | triage row + llm-proxy lane: ship the alias patch (proxy.py:197-228) or pin client effort to the vertex vocabulary; version-conditional (1.90 branches) | new (llm-proxy lane) |
| G-10 | mid-stream raw relay: `data: {upstream error JSON}` + [DONE]; `data: {str(e)}` on serialization failure (open class; W11 instance "Response API in-stream error") | all stream wires; bridge wires expose the raw upstream shape | W11 observed on the compact stream (error-class-audit W11) | `is_deterministic_in_stream_error` → Fatal (retry.rs:159) — right for deterministic, wrong for transient | triage row (class-level) + probe P-F (capture verbatim shape) + sub-shape arms if a recurring pattern appears | new |
| G-11 | 400 "gpt-5 models don't support temperature={}. Only temperature=1 is supported…" | A6/C3 azure gpt-5 | none live (rows drop temperature) | Fatal (400 unclassified) | triage row (JIG compliance; informational) | new (low priority) |
| G-12 | 400 "Invalid request format: {error_msg}" | all (malformed-body class) | none live (client bodies well-formed) | Fatal | triage row (JIG compliance; low exposure) | new (low priority) |
| G-13 | 400 "input[N].content array too long, expected max 0" (W7, strict-sol) | A6 azure strict rows (sol/terra replay) | BRICK register (matrix §4.2 lineage; R3 vxm-az class) | F5 arm → model-bound Strip (client OK); no catalog row | triage row + .74 pair-aware strip cell (owner .74) | .74 |
| G-14 | 400 "input[N].content array too long" (W6, vLLM — LAPSED) | A1/A2 responses→vLLM | lapsed (clean 200, P6); observed in R2/R3 trees | F5 arm → Strip; no catalog row | re-pin case to [200] (drift watch: reappearance = new instance → same-session JIG drill); brief §3: lapsed classes must stay lapsed | — (re-pin) |

## 3. Ratchet mechanism

### 3.1 The key — why a 3-tuple

The transformation matrix + error surface is a function of **(litellm version, proxy
branch/rev, config digest)**. Proven version/branch-dependent cells (matrix §4.9/§5): the
bridge surface (916-line behavioral delta 1.90→1.93), the grok `supports_reasoning` flag,
the vertex effort alias patch, the encrypted-content affinity pre-check, the qwen row
itself. The deployed image is not reproducible from any tracked branch (matrix §1.2) → the
key is read LIVE at gate time:

- `x_litellm_version` = the `X-Litellm-Version` response header from any live call (ledger
  .44 read 1.93.0 off an earlier image; operator says 1.90.0 — the header is the arbiter).
- `proxy_git_rev` = llm-proxy rev the deployed image was built from (image metadata).
- `config_sha256_12` = sha256:12 of the deployed config_seclab.yaml.

### 3.2 Artifact spec (machine-checkable)

File (this lane owns instantiation; spec here):
`smoke/redteam/kiloecho/ratchet/error-surface-<xver>-<proxyrev>-<cfgsha12>.json`

```json
{
  "artifact": "kiloecho-error-surface-ratchet",
  "key": {
    "x_litellm_version": "1.90.0",
    "proxy_git_rev": "<rev>",
    "config_sha256_12": "<12-hex>"
  },
  "derived_at": "<machine UTC ts>",
  "derivation": {
    "matrix_doc": "smoke/redteam/kiloecho/litellm-transform-matrix.md",
    "universe_doc": "smoke/redteam/kiloecho/error-surface-ratchet.md",
    "litellm_pin": "v1.90.0/6e8282d",
    "proxy_pin": "bugfix/grok-4.6-reasoning-flags/956b6d2"
  },
  "cells": [
    {
      "id": "U25",
      "gap": "G-7",
      "layer": "upstream-vllm-shim",
      "wire_pairs": ["A1", "A2"],
      "shape": {"http": 400, "message_regex": "validation errors for ChatCompletionRequest messages\\..*content\\.str"},
      "f_family": "F8",
      "triage_row": null,
      "client_decision": "Strip (F8 arm, post-arm); pre-arm instances = Fatal BRICK",
      "exposing_cell": "L1801/L1881",
      "source_pin": "vLLM server shim (no litellm pin); mechanism smoke/xwfix/cells/vxm-vlq/cell.json",
      "status": "GAP",
      "action": "triage-row + xwfix cell + probe P-E"
    }
  ],
  "gate": {
    "rule_rederive": "any key component changed vs the last gated artifact => full re-derive (matrix + universe) against the new pins; diff => every new (http, message-class) shape = new error shape => JIG drill BEFORE ship",
    "rule_new_shape": "JIG.md:50-52 verbatim: 'any 400/500 shape not matched by the current triage catalog gets a catalog row + an xwfix cell in the SAME session; the campaign never fixes around an unclassified wire error.'",
    "rule_client_path": "unclassified 400/500 -> classify_error ladder (retry.rs:103-186) -> RetryDecision::Fatal (retry.rs:186) -> report-and-stop; the gate FAILs if a new 400/500 shape can reach a live cell without a row",
    "rule_lapsed": "G-14/W6 class: case pinned [200]; 400 reappearance = drift instance => same-session drill",
    "rule_403": "403 shapes are outside JIG's literal 400/500 scope: catalog rows advisory, but the client treatment (Fatal) is pinned per gap"
  }
}
```

Per-cell fields: `{id, gap, layer, wire_pairs, shape{http,message_regex}, f_family,
triage_row, client_decision, exposing_cell, source_pin, status, action}` — one entry per
universe row (§1), so the artifact is the diffable ratchet state: gate diffs
old-artifact-cells vs new-derivation-cells.

### 3.3 New-failure-class drill (operational, same session)

1. A live cell or probe returns a 400/500 (the ratchet extends the discipline to 501/424 —
   the deterministic client-Fatal classes).
2. Capture the verbatim body → sha256:12 of the phrasing fragment (never echo keys;
   fragment only).
3. Needle-walk against `smoke/triage/signatures.json` (F1–F9 arms at error.rs:476-590 plus
   rows #1-18).
4. Match → classify through the existing family/row. **No match = NEW class**:
   - signatures.json row (KE-3 owns filing; this lane supplies the spec row),
   - xwfix cell pinning the shape (byte-pinned, vxm-vlq class-(a) pattern),
   - SAME session — never absorbed (JIG.md:50-52).
5. The ratchet artifact records the new cell under the current 3-tuple; the next gate with a
   changed key re-validates every cell.

### 3.4 What the gate blocks

- ship/promote with any 3-tuple component changed and no re-derived artifact (matrix §5: a
  litellm bump is a FULL re-derive — the bridge delta is behavioral, not formatting);
- ship while a GAP cell has a reachable live cell and no triage row (G-7: any cross-model
  switch onto a vLLM row can mint the class-(a) shape);
- a `drop_params` flip (U19 goes RECORD-ONLY → live GAP: every bridge wire with
  store/include/truncation/background then 500s under stock validation);
- lapsed-class 400 reappearance (G-14 drift);
- proxy branch change without re-deriving the proxy-layer shapes (auth/main.py, proxy.py,
  config — the 1.90-vs-current deltas in matrix §5 are the worked example).

## 4. Verdict — PARTIAL

*"Have we plumbed all the error types?"* — **No.**

- **Covered** (ladder + family/row): F1–F9 classifier arms; sig #13-17 PROPOSED (M-1, W1,
  W2, W3, W4/F9); 401 auth + tag-routing (U1/U2/U16, F6 PROVEN); 429 (U17/U18); 5xx-retryable
  (U7/U35); moot U19.
- **Not plumbed** — 14 named gaps (G-1…G-14, §2). Any one of them hitting a live session =
  Fatal fallthrough (400/403/424), a wrong retry storm (501, G-2), or an uncataloged
  mid-stream shape (G-10). Highest severity: **G-7** (F8 class-(a) — the shape that has
  ACTUALLY bricked sessions, L1801/L1881) and **G-2** (501 — wrong retry semantics on a
  deterministic gate).
- **Ratchet**: designed (§3), not yet instantiated — the artifact JSON + gate wiring are
  owed (this lane supplies the spec; .78-B ws9 owns gate wiring; KE-3 owns signature
  filing; operator owns probes P-A/P-B/P-D/P-E/P-F).
- **PARTIAL → FULL conditions**: (1) 14 gap rows filed (KE-3); (2) artifact instantiated +
  gate rule wired into .78-B ws9; (3) operator probes close the PREDICTED cells; (4) two
  client-side rulings: G-1 (is the 403-creds class refreshable?) and G-8 (424 → retryable).

## 5. Raw-key sweep (this deliverable)

Command of record = the four-class house pattern from `plans/xwire/review-gates-q2.md:109`
(form confirmed in review-gates-q3.md:114), run as `grep -cE '<pattern>' <file>`. Per the
q3c self-sweep rule the pattern is **described, not quoted**: (a) provider-key-style
literal (two-char sk- prefix + 8-or-more alphanumerics), (b) the x-litellm-tag header line
with literal colon, (c) api_key assignment token, (d) Bearer-scheme token with trailing
space.

Result (2026-09-18, this seat): **error-surface-ratchet.md = 0 hits** (whole file,
233 lines at sweep time). Known class-1 doc-prose FPs: the sha12 fragments
1f591ef070d9 (F8 phrasing fragment) and 9f3f56a263da (campaign key, where referenced) are
recorded, not edited out — neither matches class (a); no header-colon, assignment, or
bearer-scheme forms appear in this doc.
