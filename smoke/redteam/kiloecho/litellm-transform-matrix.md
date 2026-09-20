# LITELLM TRANSFORMATION MATRIX — kiloecho lane (KE-2)

SEAT: xw_ke2_litellm_qwen (qwen) · apex-v2-grok-build campaign · LITELLM TRANSFORMATION-MATRIX + ERROR-SURFACE RATCHET
DATE: 2026-09-18T23:10Z (machine clock) · STATUS: delivered, read-only recon (no code, no live probes)
SIBLING: error-surface-ratchet.md (error plumbing + ratchet mechanism + verdict)

## 0. Question answered

Operator: *"Isn't the transformation.py matrix itself the thing we should ratchet against? it
knows how to switch between messages and responses right? ... have we plumbed all the error types?"*

**Answer: YES, the litellm transformation layer is the correct ratchet target — and it is
version-pinned, config-dependent, and fork-patched in three places, so the ratchet key must be
`(litellm version, proxy-branch, config digest)`, not "the matrix".** The matrix below is the
1.90.0 derivation. The error-plumbing answer (PARTIAL, named gaps) is in the sibling doc.

Three load-bearing findings (detail in §1, §3, §4):

1. **Version ambiguity is real and unresolvable from this checkout.** Operator record + today's
   live recon say deployed LiteLLM = **1.90.0**; ledger apex-ayl.44 read
   `X-Litellm-Version: 1.93.0` live off an earlier image; **no branch in the llm-proxy checkout
   ever contained both the qwen3.8-27b model row and a 1.90 pin** (qwen row landed 2026-08-16
   in `5fb4bd4`, AFTER the 1.93 pin bump `628039d` 2026-08-01). The deployed image (which
   obviously has the qwen row — M-1 reached vLLM live today) is not reproducible from any
   tracked branch. → The ratchet MUST read `X-Litellm-Version` from a live response header at
   gate time and re-derive the matrix when it changes (§4.9; sibling doc §c).
2. **The M-1 clamp is stock litellm, not a proxy patch.** `normalize_reasoning_effort_value`
   (litellm 1.90 `litellm/llms/anthropic/experimental_pass_through/utils.py:16-57`) degrades
   `xhigh → high` for any model whose model_info lacks `supports_xhigh_reasoning_effort`; the
   deployed qwen row has no such flag → vLLM's Qwen3 effort vocabulary `{xhigh, medium, low}`
   rejects `high` with the verbatim 400. Crosswire §7.1 attributed the clamp to a "proxy-image
   delta" — this seat re-derives it as stock code + config absence. Same lane owner (llm-proxy)
   either way; fix = add `supports_xhigh_reasoning_effort: true` to the qwen model_info OR an
   alias patch.
3. **The bridge drops `store`, `include`, `truncation`, `background` silently for every
   non-openai provider** (whitelist at `litellm/responses/litellm_completion_transformation/
   transformation.py:79-99` + explicit-dict mapping :161-249; deployed `drop_params: true` at
   `config_seclab.yaml:1660` makes the drop silent, stock would 500 at
   `litellm/responses/utils.py:40-72`). `include: ["reasoning.encrypted_content"]` therefore
   NEVER reaches gemini/grok/vLLM-bridge targets, and AZ-origin `encrypted_content` items are
   silently dropped (no error, no carrier) whenever they ride a bridge wire (§4.5, §4.6).

Legend — every cell carries one: **PROVEN** (live capture/test evidence cited) · **SOURCE-READ**
(this seat, 2026-09-18, in the 1.90.0 source or the llm-proxy app layer) · **PREDICTED**
(inferred; the ONE probe that proves it is listed in §6).

## 1. Version pin, deployed image, and fork patches

### 1.1 Pin evidence (all first-hand this session)

| Source | Statement | Evidence |
|---|---|---|
| operator (brief, 2026-09-18) | deployed LiteLLM = **1.90.0** | "make sure the litellm checkout matches the one that's deployed on our llm proxy, I think 1.90 as we established" |
| crosswire-matrix.md §7.1 (2026-09-18 20:05Z, overwatch) | "the deployed litellm-1.90 /v1/messages → openai-class bridge" | M-1 live cell qwen-msg |
| ledger apex-ayl.44 (qwen seat, earlier) | "deployed X-Litellm-Version 1.93.0; llm-proxy pin litellm[proxy]==1.93.0" | direct header read, first-hand at that time |
| llm-proxy `docker/api/requirements.txt` | branch pins: `bugfix/grok-4.6-reasoning-flags` + master = **1.93.0**; `bugfix/opus-4.7-thinking-flags` + `feature/tool-sanitization-handler` + 4 telemetry branches = **1.90.0**; master HEAD = 1.101.0rc2 | `git show <branch>:docker/api/requirements.txt` |
| litellm checkout | `~/Projects/cli-ops/upstream-infrastructure/litellm` @ **tag v1.90.0** (6e8282d) — the matrix substrate for this doc | `git describe --tags` |
| 1.93 checkout | `/tmp/litellm193/litellm-1.93.0` (from .44 recon) — used for the delta table §5 | dir listing |

### 1.2 Why the deployed image is not branch-reproducible

- qwen3.8-27b first appears in `config_seclab.yaml` at `5fb4bd4` (2026-08-16, "Replace
  qwen3.6-27b with qwen3.8-27b in seclab proxy config") — on the 1.93-pinned line.
- The 1.93 pin bump is `628039d` (2026-08-01). The 1.90-pinned branches predate both and have
  **zero** qwen rows (`grep -c qwen3.8-27b` = 0) and no `encrypted_content_affinity` pre-check.
- The deployed image serves qwen (M-1 400 from vLLM, live today) → it carries a newer config
  than any 1.90 branch, yet (per operator + §7.1) runs 1.90. Possible: image built from a
  since-deleted branch, a cherry-picked config, or a hot-patched deployment. **No tracked branch
  in this checkout matches.**
- Consequence (ratchet design): the matrix is a function of a 3-tuple; the gate artifact must
  carry `(X-Litellm-Version header value, proxy git rev, config_seclab.yaml sha256:12)` and
  re-derive on ANY change. See sibling doc §c.

### 1.3 Fork patches in the llm-proxy app layer (deployed-image delta vs stock litellm)

All verified present in the current checkout @ `bugfix/grok-4.6-reasoning-flags` 956b6d2 and in
the 1.90-pinned branches (store/tool gates verified in both `bugfix/opus-4.7-thinking-flags`
and `feature/tool-sanitization-handler`):

| Patch | File:line (llm-proxy checkout) | Effect |
|---|---|---|
| Route purge + middleware | `app/api/proxy.py:77` (`_purge_disallowed_routes`, import-time), `:101-147` (`BlockDisallowedRoutesMiddleware`; `_make_403` at :139 emits `{"error":{"message":"Route is blocked","type":"auth_error","param":<path>}}`) | any path not in `app/common/routes.py` allowlist → 403. `/v1/responses/compact` has NO POST allowlist entry (only `/v1/responses` POST + `/v1/responses/{id}` GET, routes.py:59-60) → **NF-3 403 proven** (ws9 probe) |
| Vertex cost-suffix flag propagation | `app/api/proxy.py:149-195` (`_patch_vertex_model_cost_suffixes`) | copies boolean capability flags (incl. `supports_reasoning`) from `claude-x` cost-map entries to `claude-x@default` Vertex-suffixed entries; without it `sanitize_vertex_anthropic_output_params` silently strips `output_config.effort` (opus-4.7 thinking break, PR #1914 family; upstream fix = litellm PR #32833, not in 1.90) |
| Vertex effort alias wrapper | `app/api/proxy.py:197-228` (`_patch_vertex_reasoning_effort_aliases`, called :269) | wraps `VertexGeminiConfig._map_reasoning_effort_to_thinking_level/_budget` to map `xhigh/max/ultra → high` BEFORE stock raises `ValueError("Invalid reasoning effort: …")` (stock site: `vertex_and_google_ai_studio_gemini.py:946,999`) |
| grok-4.6 reasoning flag | `app/api/config_seclab.yaml:558` (`supports_reasoning: true`, grok-4.6 model_info, commit 946080c) | gates `supports_reasoning()` (litellm `utils.py:2777`) → `reasoning_effort` enters the vertex supported-params (`vertex_and_google_ai_studio_gemini.py:345-348`); **NOT deployed** = OQ-7 (rtok 0 live) until the branch ships; stream cases xfail (upstream limitation, commit 956b6d2) |
| store/background 501 gate | `app/api/auth/main.py:406-418` | POST /v1/responses: truthy `store` → 501 `Storing the generated model response for later retrieval is not supported. Please set 'store' field to False or remove it from the request`; truthy `background` → 501 `Background mode is not supported. Please remove 'background' field from the request` (type `invalid_request_error`) |
| per-model tool policy | `app/api/auth/main.py:420-424` → `app/api/auth/tool.py` | claude*: `web_search*`/`web_fetch*` tool types → 403 `Tool type is blocked for model '<m>'` (tool.py:66-70); gemini*: only `{googleSearch, codeExecution, functionDeclarations, urlContext, function}` allowed — typed → 403 `Tool type is blocked` (:57-70), **untyped tool dict (Anthropic messages shape has no `type`) → 403 `Tool is blocked for model '<m>'` (:71-78) = M-2 PROVEN**; non-claude/gemini: `web_search*` prefix → 403 (:79-89) |
| router settings | `app/api/config_seclab.yaml:1665-1669` | `optional_pre_call_checks: ["responses_api_deployment_check", "encrypted_content_affinity"]` (current branch; **1.90 branches have only `responses_api_deployment_check`** — version-dependent cell, §5), `enable_tag_filtering: true`, `disable_cooldowns: true`, `num_retries: 0`; litellm_settings: `drop_params: true` (:1660), `request_timeout: 600` (:1661), `num_retries: 0` (:1656) |
| per-row temperature drop | `config_seclab.yaml:252,282,314,838,867,896,924,952` (`additional_drop_params: ["temperature"]` on gpt-5 AZ rows) | defense-in-depth row for chat/messages paths; the responses path is governed by the gpt-5 temperature gate (native adapter) |

## 2. Dispatch map — which adapter carries which wire pair

Entry routes (llm-proxy allowlist, `app/common/routes.py`): POST `/v1/responses` + GET
`/v1/responses/{id}` (`:59-60` RESPONSE_ROUTES_WITHOUT_ID / RESPONSE_ROUTES_WITH_ID),
POST `/v1/messages` (`:52`), POST `/v1/chat/completions` (`:43-44`),
`/v1beta ...:generateContent` / `:countTokens` (`:62-65`). Any other path → 403
`Route is blocked` (`app/api/proxy.py:101-147`; NF-3 PROVEN).

### 2.1 `/v1/responses` wire — entry `litellm/responses/main.py:902+`

Config resolution `main.py:1044-1052` →
`ProviderConfigManager.get_provider_responses_api_config` (`litellm/utils.py:8978-9095`):
Python classes first, then the JSON registry (`litellm/llms/openai_like/json_loader.py:71-76`;
`providers.json` = 24 slugs, **no `vertex_ai`, no `vllm`**). Result `None` →
`LiteLLMCompletionResponsesConfig` **bridge**
(`litellm/responses/litellm_completion_transformation/`).

| row prefix (deployed config) | 1.90.0 config | upstream endpoint |
|---|---|---|
| `openai/qwen3.8-27b`, `openai/glm-5.2` (vLLM, ONPREM_INFERENCE_API_BASE) | `OpenAIResponsesAPIConfig` (native; utils.py:9032-9033) | POST `{api_base}/v1/responses` (openai/responses/transformation.py:300-316) |
| `azure/gpt-5.*` (AZ rows) | `AzureOpenAIResponsesAPIConfig` (gpt: utils.py:9035-9047); o-series → `AzureOpenAIOSeriesResponsesAPIConfig` (no deployed rows) | Azure `/responses` |
| `vertex_ai/gemini-*` · `vertex_ai/xai/grok-4.6` · `vertex_ai/claude-*` | **None** (no VERTEX_AI python branch; not in JSON registry) → **bridge** | POST `/v1/chat/completions` → chat config: gemini → `:generateContent`; `xai/` → MODEL_GARDEN OpenAI-compatible (`vertex_ai/common_utils.py:138-214`, MODEL_GARDEN :197-202); claude → partner models |
| `hosted_vllm/Qwen/Qwen3-Embedding-8B` (embedding row only) | `HostedVLLMResponsesAPIConfig` — subclass of `OpenAIResponsesAPIConfig` (`hosted_vllm/responses/transformation.py:17-78`; dispatch utils.py:9073) | `{api_base}/v1/responses` — embedding row, not an LLM wire |
| not in fleet: `xai/`, `github_copilot`, `chatgpt`, `litellm_proxy`, `volcengine`, `manus`, `perplexity`, `databricks`, `openrouter`, `bedrock_mantle` (python classes exist, utils.py:9048-9094) | — | — |

Bridge `previous_response_id` never reaches upstream: resolved via the in-memory
session (`litellm_completion_transformation/handler.py:92-145` → `session_handler.py`).

### 2.2 `/v1/messages` wire — entry `litellm/llms/anthropic/experimental_pass_through/messages/handler.py:49-60` (`_RESPONSES_API_PROVIDERS = frozenset({"openai"})`)

| row prefix (deployed config) | 1.90.0 path | upstream |
|---|---|---|
| `openai/qwen3.8-27b`, `openai/glm-5.2` | messages→responses adapter (`responses_adapters/handler.py:34-118`; `responses_adapters/transformation.py:55-399`) | POST upstream `/v1/responses` |
| `vertex_ai/claude-*` | native `VertexAIPartnerModelsAnthropicMessagesConfig` (utils.py:8875-8879) | partner `/v1/messages` |
| `vertex_ai/gemini-*`, `vertex_ai/xai/grok-4.6`, `azure/*` (non-claude) | messages→chat adapter (`adapters/handler.py:378-411` `_normalize_reasoning_effort`, calls `normalize_reasoning_effort_value` at :400/:407; `adapters/transformation.py:1094-1140` `_translate_thinking_to_openai`) | chat wire → per-provider chat config |
| not in fleet: `anthropic/`, `bedrock/claude`, `azure_ai/claude`, `minimax/`, `deepseek/` (native, utils.py:8861-8897) | — | — |

### 2.3 `/v1/chat/completions` + `/v1beta`

- chat: per-provider chat config — `openai/` → vLLM `/v1/chat/completions`; vertex
  gemini → `:generateContent` (thinkingConfig via budget/level mapper); `azure/` →
  `/chat/completions` with `additional_drop_params: ["temperature"]` on the gpt-5 AZ rows
  (config_seclab.yaml:252,282,314,838,867,896,924,952).
- `/v1beta ...:generateContent`: native passthrough; no client in this campaign calls it.

## 3. Wire-pair × field-fate matrix

14 wire pairs (A = `/v1/responses` entry, B = `/v1/messages` entry, C =
`/v1/chat/completions` entry) × field fate. Every cell: fate + [status] + cite (1.90.0
checkout unless stated). "bridge" = `LiteLLMCompletionResponsesConfig`.

### 3.1 Request-side field fate — A rows (`/v1/responses` entry)

| # | wire pair (entry → target) | reasoning/effort | reasoning items (request) | encrypted_content / include | store · truncation · background | cache_control | tools / tool_choice |
|---|---|---|---|---|---|---|---|
| A1 | responses → openai/qwen3.8-27b (vLLM), native | passthrough — `reasoning`/`reasoning_effort` mapped (main.py:1057-1066); no clamp on the responses wire; xhigh accepted → 200 [PROVEN P1/P2] | passthrough; empty-id ride (id:"" after cross-model switch) → vLLM **server-side** shim 400, F8 class-(a) "N validation errors for ChatCompletionRequest messages.N…content.str" [PROVEN L1801/L1881; trigger-dependent — r3r = 200] | carried; encitem_ IDs restored pre-upstream (main.py:1155-1156, :2032-2034; responses/utils.py:240-425) [SOURCE-READ; 200 P2] | passthrough (native config has no whitelist); row itself is store:False [SOURCE-READ] | stripped pre-send (openai/responses/transformation.py:129-149) [SOURCE-READ] | OpenAI dialect passthrough; vLLM validates server-side [SOURCE-READ] |
| A2 | responses → openai/glm-5.2 (vLLM), native | same adapter; glm menu {high,medium,low} (client config :43-54) — xhigh would ride raw (no clamp) → 400 or 200 [PREDICTED §6 P-H] | same as A1; F8 class-(a) fragment also recorded on the glm row — same vLLM deployment (ONPREM_INFERENCE_API_BASE; error-class-audit §3) [PROVEN fragment] | same as A1 [SOURCE-READ] | same as A1 [SOURCE-READ] | same as A1 [SOURCE-READ] | same as A1 [SOURCE-READ] |
| A3 | responses → vertex_ai/gemini-*, **bridge** | →`reasoning_effort` (bridge transformation.py:187-225); accepted only if `supports_reasoning(model)` (vertex_and_google_ai_studio_gemini.py:345-348); xhigh → `ValueError("Invalid reasoning effort: xhigh")` → 500 — 1.90 branches lack the alias patch (proxy.py:197-228) [SOURCE-READ; §6 P-A; hazard G-11] | `reasoning` items have no `content` → dropped as `[]` (bridge :988-990) — summaries never reach gemini [SOURCE-READ] | dropped — `include` absent from the 15-param whitelist (bridge :81-102) [SOURCE-READ; finding #3] | dropped (whitelist); silent under `drop_params: true` (config :1660), stock 500 `UnsupportedParamsError` (responses/utils.py:41-72) [SOURCE-READ] | kept on input_image + text blocks (bridge :1296-1298, :1313-1314); fate in the vertex chat transform [PREDICTED §6 P-G] | tool_choice mapping (bridge :107-158); proxy tool gate upstream: gemini allowlist {googleSearch, codeExecution, functionDeclarations, urlContext, function} → 403 typed or untyped (tool.py:57-78) [M-2 PROVEN] |
| A4 | responses → vertex_ai/xai/grok-4.6, **bridge** | 1.90 branches: grok `supports_reasoning` flag absent (config :558 current branch only; 1.90 = 0) → `reasoning_effort` not in supported params → silent drop (drop_params) or stock 500; OQ-7 rtok 0 [PROVEN]; current branch: alias patch + flag → high [SOURCE-READ] | dropped `[]` (bridge :988-990) [SOURCE-READ] | dropped (whitelist) [SOURCE-READ] | dropped (whitelist) [SOURCE-READ] | kept by bridge; MODEL_GARDEN chat is OpenAI-compatible → rides to upstream chat [PREDICTED] | proxy gate: web_search* → 403 (tool.py:79-89); vertex rejects non-function tool types (W3, sig#15 PROPOSED) [PROVEN .78 recon] |
| A5 | responses → vertex_ai/claude-*, **bridge** | partner-chat effort path; no live cell; cost-suffix flag propagation (proxy.py:149-195) gates what survives `sanitize_vertex_anthropic_output_params` (PR #1914 family) [SOURCE-READ; PREDICTED] | dropped `[]` (bridge :988-990) [SOURCE-READ] | dropped (whitelist) [SOURCE-READ] | dropped (whitelist) [SOURCE-READ] | kept by bridge [SOURCE-READ] | proxy gate: claude web_search*/web_fetch* → 403 `Tool type is blocked for model '<m>'` (tool.py:66-70) [SOURCE-READ; §6 P-B] |
| A6 | responses → azure/gpt-5.* (AZ), native | passthrough (native azure config); gpt-5 temperature≠1 → drop-or-400 "gpt-5 models don't support temperature=…" (openai/responses/transformation.py:99-127); AZ rows carry `additional_drop_params: ["temperature"]` [SOURCE-READ] | passthrough; strict-sol replay of id:"" → Azure 400 F3 "Invalid 'input[N].id': ''" [PROVEN incident 01a0b046 / R3 vxm-az] | carried + AZ boundary machinery: response rewrite (responses/utils.py:328-391), stream wrap gated on `litellm_metadata.encrypted_content_affinity_enabled` (responses/streaming_iterator.py:213-238), affinity pre-check (1.93/current branches ONLY — absent on 1.90 branches) [PROVEN F1-503 20260916T191948Z/resp-003; OQ-1; probe-C KEEP] | passthrough (native); 501 gate is proxy-level (auth/main.py:406-418) [SOURCE-READ] | stripped pre-send (openai/responses/transformation.py:129-149) [SOURCE-READ] | passthrough; AZ validates [SOURCE-READ] |

### 3.2 Request-side field fate — B rows (`/v1/messages` entry)

| # | wire pair (entry → target) | reasoning/effort | thinking blocks (request) | encrypted content | cache_control | tools / tool_choice |
|---|---|---|---|---|---|---|
| B1 | messages → openai/qwen3.8-27b (responses adapter → vLLM `/v1/responses`) | `normalize_reasoning_effort_value` clamps xhigh → **high** (qwen model_info lacks `supports_xhigh_reasoning_effort`; experimental_pass_through/utils.py:16-57, called from responses_adapters/handler.py:80-90) → vLLM 400 "Unexpected reasoning effort high. Supported types are xhigh (default), medium, and low." = **M-1** [PROVEN 20260918T200643Z/qwen-msg/capture/req-004.json + 3-variant probe] | assistant thinking → `output_text`, **signature dropped** (responses_adapters/transformation.py:55-177) [SOURCE-READ] | no `include` on the messages wire; encrypted items surface as thinking blocks → text only, carrier lost [SOURCE-READ] | dropped — system→instructions text-only, no cache_control handling (responses_adapters/transformation.py:302-399; 0 references in that adapter) [SOURCE-READ] | web_search→web_search_preview (:184-217); tool_choice: any→required, tool→function{name}, else auto (:219-233); context_management compact_20260112→compaction (:235-259); reasoning {adaptive}→output_config.effort or medium, enabled→10k/5k/2k budget map (:261-300) [SOURCE-READ] |
| B2 | messages → openai/glm-5.2 (responses adapter → vLLM `/v1/responses`) | same clamp; glm menu includes high → clamp is a no-op → 200 [PREDICTED; consistent with glm-resp 200 P2] | same as B1 [SOURCE-READ] | same as B1 [SOURCE-READ] | same as B1 [SOURCE-READ] | same as B1 [SOURCE-READ] |
| B3 | messages → vertex_ai/claude-* (**native** partner messages) | output_config.effort honored by the partner; flag propagation via cost-suffix patch (proxy.py:149-195) — OQ-5: 181 thinking frames / 3090 tok [PROVEN] | native thinking blocks round-trip, **signature preserved** [PROVEN OQ-5] | n/a — claude signatures are the native carrier, not AZ encitem_ [SOURCE-READ] | native config; not observed dropping in the OQ-5 cell [PREDICTED] | proxy gate claude web_search*/web_fetch* → 403 (tool.py:66-70); web_search_20250305 is native on partner [SOURCE-READ] |
| B4 | messages → vertex_ai/gemini-* (messages→chat adapter) | `_normalize_reasoning_effort` → `normalize_reasoning_effort_value` (adapters/handler.py:378-411, calls :400/:407); thinking → `_translate_thinking_to_openai` (adapters/transformation.py:1094-1140; output_config.effort override :1119-1123) → chat wire → gemini thinkingConfig mapper (vertex_and_google_ai_studio_gemini.py:898-1000); xhigh clamps to high, which the gemini mapper accepts [SOURCE-READ] — litellm level never exercised live: M-2 died at the proxy gate first [M-2 PROVEN 403] | → openai-style thinking/output_config on the chat wire [SOURCE-READ] | none [SOURCE-READ] | **conditionally preserved**, model-gated (adapters/transformation.py:310-349 `_add_cache_control_if_applicable`) [SOURCE-READ] | M-2 PROVEN at proxy: untyped tool dict (Anthropic shape has no `type`) → 403 `Tool is blocked for model '<gemini>'` (tool.py:71-78); below the gate the chat adapter maps tools to chat tools [SOURCE-READ] |
| B5 | messages → vertex_ai/xai/grok-4.6 (messages→chat adapter) | same as B4 → MODEL_GARDEN chat; no live cell past the gate [PREDICTED] | same as B4 [SOURCE-READ] | none [SOURCE-READ] | same as B4 [SOURCE-READ] | proxy gate web_search* → 403 (tool.py:79-89) [SOURCE-READ] |

### 3.3 Request-side field fate — C rows (`/v1/chat/completions` entry)

| # | wire pair (entry → target) | reasoning/effort | thinking | encrypted | cache_control | tools |
|---|---|---|---|---|---|---|
| C1 | chat → openai/* (vLLM) | `reasoning_effort` passthrough (qwen xhigh/medium/low; glm high/medium/low) [SOURCE-READ] | `reasoning_content` passthrough where vLLM emits [SOURCE-READ] | none (chat wire has no include) [SOURCE-READ] | openai chat dialect passthrough [SOURCE-READ] | W2/sig#14 class: responses-dialect parts (input_text) leaked onto this wire by the **codex-combined v1 transport shim** (adjacent, not product; error-class-audit §3). The litellm 1.90 bridge normalizes input_*→text (transformation.py:1321-1348), so the litellm-bridge wire is protected against that specific leak [PROVEN adjacent / SOURCE-READ] |
| C2 | chat → vertex_ai/gemini-* | effort → thinkingConfig via budget/level mappers (vertex_and_google_ai_studio_gemini.py:898-1000, applied :1290-1330); unknown value → `ValueError("Invalid reasoning effort: {x}")` → 500 (:946/:999); medium→high clamp for non-3.1-pro [SOURCE-READ] | thinkingConfig only; no reasoning_content carrier back [SOURCE-READ] | none [SOURCE-READ] | dropped by the vertex chat transform [PREDICTED] | gemini chat tool mapping [SOURCE-READ] |
| C3 | chat → azure/gpt-5.* | standard; temperature dropped per row (config_seclab.yaml:252,…,952) [SOURCE-READ] | gpt-5 reasoning via native chat fields [SOURCE-READ] | none [SOURCE-READ] | stripped on the messages side; chat side: openai dialect [SOURCE-READ] | standard [SOURCE-READ] |

### 3.4 Response / stream-side fate

| wire pair | reasoning in response | usage reasoning_tokens | encrypted in response | stream frames |
|---|---|---|---|---|
| A1/A2 (vLLM native responses) | passthrough of whatever vLLM emits (reasoning items) [SOURCE-READ] | from vLLM [SOURCE-READ] | n/a — vLLM does not emit AZ-style encrypted [SOURCE-READ] | SSE responses events passthrough [SOURCE-READ] |
| A3-A5 (bridge) | response map sets `reasoning=None` (bridge :1711); reasoning items ONLY if `message.reasoning_content` is non-empty (`_extract_reasoning_output_items` :1831-1856) → gemini thinking arrives as thoughtSignature, not reasoning_content → **0 items (OQ-2 PROVEN: rtok 12444 / 0 frames / thoughtSignature present)**; grok **0 (OQ-7 PROVEN rtok 0)**; claude PREDICTED 0 | `completion_tokens_details.reasoning_tokens` preserved if present, else 0 (bridge :2155-2162) — so usage can show rtok>0 with **zero reasoning frames** (the OQ-2/OQ-7 signature) [SOURCE-READ] | not carried — the affinity stream-wrap is gated on `litellm_metadata.encrypted_content_affinity_enabled` (responses/streaming_iterator.py:213-238), which the bridge wire never sets [SOURCE-READ] | `response.created` always carries `reasoning {effort:None, summary:None}` (streaming_iterator.py:378-383); reasoning frames only from `reasoning_content` (:618) → 0 on gemini/grok [SOURCE-READ] |
| A6 (azure native) | passthrough | passthrough | carried — OQ-1 PROVEN; F1-503 cross-org class (affinity pre-check, 1.93 branches only); response-ID rewrite `resp_{b64(litellm:provider;model_id;id)}` (responses/utils.py; proxy/hooks/responses_id_security.py:290) [PROVEN OQ-1 / F1-503] | affinity-wrapped stream (gated as above) [PROVEN probe-C KEEP] |
| B1/B2 (messages→responses adapter) | `translate_response`: reasoning summary → thinking block, **signature=None** (responses_adapters/transformation.py:410-490); incomplete → max_tokens [SOURCE-READ] | input/output only (adapter maps no reasoning breakdown) [SOURCE-READ] | none [SOURCE-READ] | adapter streams; thinking as output_text-derived blocks [SOURCE-READ] |
| B3 (native claude messages) | native thinking round-trip (OQ-5 181 frames PROVEN) | native [PROVEN] | none (signature is the carrier) [SOURCE-READ] | native thinking frames [PROVEN OQ-5] |
| B4/B5, C1-C3 (chat-adapter wires) | chat completion response; thinking surfaces only if the chat layer maps it — gemini thoughtSignature is NOT mapped back → dropped [PREDICTED] | standard chat usage [SOURCE-READ] | none [SOURCE-READ] | chat SSE; no reasoning frames [SOURCE-READ] |

### 3.5 Error-fate per wire pair (compact; full surface in error-surface-ratchet.md)

- **Proxy gates, all LLM routes, upstream of litellm** (verified present on 1.90 branches too):
  401 malformed/missing creds (auth/main.py:219-220, :316-317); 403 route-block
  (proxy.py:101-147 + auth/main.py:232-253); 403 unknown/expired creds (auth/main.py:279-287,
  :304-310 — 403, NOT 401); 403 restricted-model / application-access (auth/main.py:361-376,
  :397-401); 400 missing `user` field (auth/main.py:321-322); 424 LDAP transient
  (auth/main.py:452-454); 501 store/background (auth/main.py:406-418); 403 tool policy
  (tool.py:44-89); 503 DB-unavailable (auth/main.py:341-343).
- **Router (litellm)**: 401 tag-routing "Not allowed to access model due to tags
  configuration" (types/router.py:487-489 → proxy/_types.py:3337-3340); 429
  "No healthy deployment available" / "No deployments available" (proxy/_types.py:3331-3335);
  500 `UnsupportedParamsError` (responses/utils.py:41-72) — **moot under deployed
  `drop_params: true`** (config :1660).
- **Adapter-level**: M-1 400 (B1, vLLM via responses adapter); gpt-5 temperature 400
  (A6/C3, openai/responses:99-127); vertex effort ValueError→500 (A3/A4/C2,
  vertex_and_google_ai_studio_gemini.py:946/:999); F8 class-(a) vLLM shim 400 (A1/A2).
- **Upstream relay (all routes)**: non-stream — ProxyException passthrough, httpx raw body,
  exc status 400-599 preserved else 500, AttributeError-in-chain → 400 "Invalid request
  format: …" (common_request_processing.py:2271-2426); first stream-chunk error → JSON with
  real status (:359-391); **mid-stream upstream errors relayed as raw `data:` SSE frames +
  [DONE]** (proxy_server.py:7330-7400); timeout → 500 (litellm/exceptions.py:334 — Timeout is
  an openai.APITimeoutError subclass, no 4xx status).

## 4. Cross-cutting field fates

### 4.1 Tags (x-litellm-tags)

Harness config sets the tag **per model, Azure rows only**: `[model."gpt-5.6-sol".extra_headers]`
style block with the `x-litellm-tags` header = "East US 2" (harness config; region tags in
config_seclab.yaml:568-728, East US 2 at :588). Non-AZ rows (qwen, glm, gemini, grok,
claude) carry no harness tag → no tag filter applies → they reach their vLLM/Vertex
deployment (M-1 reached vLLM live: PROVEN).

Litellm path: header → `metadata.tags` (`litellm/proxy/litellm_pre_call_utils.py:1210-1222`,
comma-split; body `tags` overrides), pre-auth merge for the budget gate (:1260-1310);
router `match_any` default True (`router.py:293-294`); tag filter runs **after** pre-call
checks (`router.py:11168`); match semantics: nonempty intersection
(`router_strategy/tag_based_routing.py:44-70`); no match AND no "default"-tagged deployment
→ `raise ValueError` (:223-226) with message `Not allowed to access model due to tags
configuration` (`types/router.py:487-489`) → `ProxyException` **code 401**
(`proxy/_types.py:3337-3340`); body = `{"message","type","param","code"(+"provider_specific_fields")}`
wrapped in `{"error":{...}}` (proxy_server.py:1300-1310).

Client side: F6 family arm (error.rs:476-590; 401-gated, "not allowed to access model due
to tags" needle) — Auth-shaped, reachable via `actor/request_task.rs:391-403` (model-bound
checked before classify_error). F6 PROVEN (testdata/affinity/probe-d-401-body.json).

### 4.2 store / background / truncation

Order of enforcement: **proxy gate first** — POST `/v1/responses` with truthy `store` → 501
"Storing the generated model response for later retrieval is not supported. Please set
'store' field to False or remove it from the request"; truthy `background` → 501
"Background mode is not supported. Please remove 'background' field from the request"
(both `type: invalid_request_error`; auth/main.py:406-418; verified present on the 1.90
branches too). Below the gate: native wires pass the params through (upstream validates);
**bridge wires drop them** (15-param whitelist, bridge :81-102; no explicit mapping in the
request map :161-249) — silent under deployed `drop_params: true` (config :1660), stock
behavior = 500 `UnsupportedParamsError` (responses/utils.py:41-72). `truncation`: same
dichotomy (bridge: dropped; native: rides).

### 4.3 Reasoning-effort aliases per provider (1.90 branches vs current branch)

| target | effort vocabulary (upstream) | xhigh fate — 1.90 branch | xhigh fate — current branch |
|---|---|---|---|
| vLLM qwen (openai/) responses wire | {xhigh, medium, low} | accepted → 200 [PROVEN P1/P2] | same |
| vLLM qwen (openai/) messages wire | {xhigh, medium, low} | **clamped xhigh→high → 400 = M-1** [PROVEN] | same (clamp is stock litellm) |
| vLLM glm (openai/) | {high, medium, low} | responses: xhigh rides raw → [PREDICTED §6 P-H]; messages: clamp no-op (high accepted) | same |
| vertex gemini | mapper levels (none/low/medium/high; medium→high clamp for non-3.1-pro) | xhigh → ValueError → **500** (no alias patch) [SOURCE-READ; §6 P-A] | alias patch xhigh/max/ultra→high (proxy.py:197-228) → high |
| vertex grok (xai/) | via MODEL_GARDEN | flag absent → `reasoning_effort` silently dropped (or stock 500); OQ-7 rtok 0 [PROVEN] | flag :558 + alias patch → high |
| vertex claude (partner) | native | output_config.effort path; cost-suffix flag patch required (proxy.py:149-195) | same |
| azure gpt-5 | AZ native | xhigh accepted (AZ) [SOURCE-READ] | same |

Clamp mechanics (stock, both 1.90 and 1.93 — full diff cosmetic, §5):
`normalize_reasoning_effort_value` (experimental_pass_through/utils.py:16-57): max →
max/xhigh/high; xhigh → xhigh only with `supports_xhigh_reasoning_effort` else **high**;
minimal → minimal/low. Call sites: messages→responses adapter (responses_adapters/
handler.py:80-90) and messages→chat adapter (adapters/handler.py:378-411). The M-1 fix is
config (add `supports_xhigh_reasoning_effort: true` to the qwen model_info) or the alias
patch — lane owner either way is llm-proxy. W5/sig#17 (PROPOSED, apex-ayl.83) = this 400;
error-class-audit W15 (stock xhigh drop-or-raise) = informational.

### 4.4 Thinking / reasoning frames — the drop sites

1. **Bridge response map**: `reasoning=None` (bridge :1711) + reasoning items only from
   non-empty `message.reasoning_content` (_extract_reasoning_output_items :1831-1856).
   Gemini thinking arrives as thoughtSignature (not reasoning_content) → 0 items (OQ-2
   PROVEN); grok → 0 (OQ-7 PROVEN).
2. **Bridge stream**: `response.created` always `reasoning {effort:None, summary:None}`
   (streaming_iterator.py:378-383); reasoning frames gated on `reasoning_content` (:618) →
   0 reasoning frames on bridge wires while usage can still report rtok>0 (OQ-2 signature).
3. **messages→responses adapter**: request-side thinking blocks → output_text, signature
   dropped (responses_adapters/transformation.py:55-177); response-side summary → thinking
   with signature=None (:410-490).
4. **Native wires preserve**: azure responses (encrypted carrier, OQ-1), vertex claude
   messages (signature carrier, OQ-5 181 frames).

Consequence for the client: persisting reasoning items from a **native-wire** turn carries
a carrier (AZ encrypted / claude signature) that a **bridge-wire** target silently drops
(§4.5/§4.6); persisting from a **bridge-wire** turn yields nothing to persist (0 frames) —
replay-safe but fidelity-lossy, and the usage rtok number does not reflect any recoverable
content.

### 4.5 Encrypted content — request side

`include: ["reasoning.encrypted_content"]` is carried only on native openai-family wires.
encitem_ prefix + `litellm_enc:` build/decode (responses/utils.py:240-425); encrypted-item
IDs are restored pre-upstream (main.py:1155-1156, :2032-2034). On **bridge wires the
carrier never arrives**: `include` is dropped (whitelist §4.2) and reasoning/encrypted
input items drop silently (no `content` → `[]`, bridge :988-990; no-text blocks skipped,
:1305-1307; "encrypted_content" is never referenced anywhere in the bridge module).
Consequence: a history built on an AZ native wire (encrypted items present) replayed onto
a bridge wire = **silent loss, no error, no carrier** — exactly the VX-M ride class; if
the target then rejects the residual shape (empty-id ride → vLLM shim 400, F8 class-(a)),
the client sees an unclassified 400 → Fatal (retry.rs:186) → BRICK.

### 4.6 Encrypted content — response side / AZ boundary

Response rewrite (responses/utils.py:328-391); response-ID rewrite
`resp_{b64(litellm:provider;model_id;id)}` (responses/utils.py; proxy/hooks/
responses_id_security.py:290); stream wrap **only** if
`litellm_metadata.encrypted_content_affinity_enabled` (responses/streaming_iterator.py:
213-238); affinity pre-check (`router_utils/pre_call_checks/
encrypted_content_affinity_check.py:1-40`) pins the deployment and raises the OpenAI-shaped
`invalid_encrypted_content` on cross-org pointers. **Version/branch-dependent**: the pre-check
is wired in `optional_pre_call_checks` ONLY on the current/1.93 branches; 1.90 branches
carry just `responses_api_deployment_check` (verified `git show` this session). Client F1
family = 503-exception double-keyed encrypted+(unavailable|boundary) (error.rs:476-590).
PROVEN: F1-503 (20260916T191948Z/wire/resp-003.jsonl), probe-C same-boundary KEEP
(testdata/affinity/probe-c-same-boundary-keep.json). Bridge wires: no encitem_ carrier in
either direction.

### 4.7 cache_control / metadata / include

- messages→responses adapter: system → instructions **text-only**; `metadata.user_id` →
  `user[:64]`; **no cache_control handling** (0 references in the module) → dropped; no
  store/include (responses_adapters/transformation.py:302-399).
- native openai responses adapter: cache_control markers stripped pre-send
  (openai/responses/transformation.py:129-149; the failure mode it avoids = OpenAI 400
  "Unknown parameter: 'input[0].content[0].cache_control'").
- messages→chat adapter: cache_control **conditionally preserved**, model-gated
  (adapters/transformation.py:310-349 `_add_cache_control_if_applicable`, 35 references in
  the module).
- bridge: kept on input_image and text blocks (bridge :1296-1298, :1313-1314) → then fate
  depends on the target chat config (PREDICTED: dropped by the vertex gemini transform;
  §6 P-G).

### 4.8 tool_choice + content types × the .77 family interaction

Bridge: tool_choice mapping (transformation.py:107-158); input items —
tool_call_output/web_search_call/computer_call_output/tool_result → tool message,
function_call → assistant tool-call message, **web_search_call rides as a tool OUTPUT on
the bridge** (vs first-class on native wires); content `None` → `[]` dropped (:988-990);
content parts: input_file→file, input_image→image_url (cache_control kept), `input_*`
prefix stripped to valid chat types (input_text→text) via
`_get_chat_completion_request_content_type` (:1321-1348), no-text blocks skipped
(:1305-1307). Tool-call-id reconciliation surface: `_recover_tool_call_id_from_assistant`,
`_check_tool_call_exists`, `_reconstruct_tool_call_from_tools` (1.93 adds
`_tool_call_id_from_responses_item` — §5); the client F7 family (input[+.call_id+invalid)
maps to dangling-call-id rejection on these wires [SOURCE-READ for the code; PREDICTED for
the failure mapping].

× .77 interaction: vLLM rows are **family-unset / non-strict** (no `strict_responses_input`
on the qwen/glm rows) → the client's `project_strict_responses_input` is a no-op
(provider.rs:263-278) and `normalize_content_types` does not run → responses-dialect parts
and empty-id items ride raw onto the vLLM wire → the vLLM server-side responses→
ChatCompletions shim rejects the shape (F8 class-(a); W2/sig#14 on the chat wire when the
leak rides there). The strict AZ rows (sol/terra) re-pin id/encitem_ to absent (.77) —
that is the asymmetry the .74 strip work addresses.

### 4.9 Version sensitivity — the ratchet key

The matrix is a function of the 3-tuple **(litellm version, proxy branch/rev, config
digest)**. Proven version/branch-dependent cells this session: A3/A4/A5 bridge surface
(916-line bridge delta 1.90→1.93, §5); A4 grok flag (0 on 1.90 branches, :558 current);
A3 gemini 500-vs-alias-patch (patch absent on 1.90 branches); §4.6 affinity pre-check
(1.93/current branches only); qwen row existence (current branches only — §1.2). The ratchet
artifact spec (machine-checkable, keyed on the 3-tuple, re-derive-on-change rule) is in the
sibling doc §c.

## 5. 1.90.0 → 1.93.0 delta (matrix-relevant files)

Method: `diff <v1.90.0 checkout>/<file> /tmp/litellm193/litellm-1.93.0/<file> | grep -c
'^[<>]'` (changed-line count). Substrate: `~/Projects/cli-ops/upstream-infrastructure/
litellm` @ v1.90.0 (6e8282d) vs the 1.93.0 sdist unpack at `/tmp/litellm193/litellm-1.93.0`
(pyproject version = "1.93.0").

| file | 1.90↔1.93 changed lines | assessment (this seat) |
|---|---|---|
| `anthropic/experimental_pass_through/utils.py` (effort clamp) | 9 | **cosmetic (line-wrap) — full diff read; clamp logic byte-identical in behavior** |
| `anthropic/experimental_pass_through/adapters/transformation.py` (messages→chat) | 549 | line-wrap-dominant in sampled regions; full behavioral audit NOT done |
| `anthropic/experimental_pass_through/adapters/handler.py` | 179 | not audited |
| `anthropic/experimental_pass_through/messages/transformation.py` | 268 | not audited |
| `openai/responses/transformation.py` (native adapter) | 111 | line-wrap-dominant in sampled regions (temperature gate, cache_control strip, reasoning-item revalidate all present in both) |
| `responses/utils.py` (param validation, encrypted build) | 195 | drop_params gate identical in sampled region; not fully audited |
| `router_strategy/tag_based_routing.py` | 131 | not audited |
| `router_utils/pre_call_checks/encrypted_content_affinity_check.py` | 51 | present in both; not audited |
| `responses/litellm_completion_transformation/transformation.py` (**bridge**) | **916** | **behavioral, not just formatting**: 1.93 adds `_should_drop_derived_web_search_options`, a "drop unsupported Responses-API-only tool types" path, `_resolve_file_id` (file payload in raw "input"), `_tool_call_id_from_responses_item`; type-annotation modernization accounts for much of the rest |
| `responses/litellm_completion_transformation/streaming_iterator.py` | 344 | not audited |
| `utils.py` (config resolution) | 2090 | not audited (includes registry/branch additions) |
| `proxy/_types.py` (ProxyException status mapping) | 598 | not audited — the 401/429 mapping is stable at 1.90; re-derive per version |

Honesty note: line-wrap vs behavioral was **not** separated per file beyond the bridge
additions above and the two fully/sampled-read files. The ratchet therefore treats any
version bump as a **full re-derive trigger**, not a no-op (sibling doc §c).

Config deltas (verified `git show` this session, 1.90-pinned branch
`bugfix/opus-4.7-thinking-flags` vs current `bugfix/grok-4.6-reasoning-flags` 956b6d2):
qwen3.8-27b row **absent** (grep -c = 0); `encrypted_content_affinity` pre-check **absent**
(only `responses_api_deployment_check`); grok `supports_reasoning` flag **absent** (0 vs 2
on current); vertex effort alias patch **absent** (0 vs 1).

## 6. Probe list (operator-owned — this seat lists, never runs)

| # | target cell | probe (exact) | expected (1.90-equivalent image) | proves |
|---|---|---|---|---|
| P-A | A3 effort 500 | `POST /v1/responses` `{model: <untagged gemini row>, "reasoning": {"effort": "xhigh"}}` | 500 `Invalid reasoning effort: xhigh` (G-11 confirmed) **or** 200 if the image carries the alias patch — either way it discriminates image vintage | A3 effort cell; G-11 |
| P-B | A5/B3 claude tool gate | `POST /v1/messages` `{model: "vertex_ai/claude-sonnet-5", "tools": [{"type": "web_search_20250305", "name": "web_search"}]}` + minimal message | 403 `Tool type is blocked for model 'claude-sonnet-5'` | G-3; A5/B3 tool cell |
| P-C | A1 tag-401 on qwen | `POST /v1/responses` `{model: "qwen3.8-27b", ...minimal...}` with the `x-litellm-tags` header set to `East US 2` | 401 `Not allowed to access model due to tags configuration. Passed model=qwen3.8-27b and tags=['East US 2']` | F6 qwen-row variant (F6 AZ-row already PROVEN) |
| P-D | 501 gate | `POST /v1/responses` `{..., "store": true}` | 501 `Storing the generated model response for later retrieval is not supported...` | G-2; §4.2 |
| P-E | F8 trigger boundary (OQ-11) | `POST /v1/responses` qwen with one reasoning input item `{type:"reasoning", id:"", summary:[{type:"summary_text",text:"x"}]}` and NO encrypted_content | 400 F8 class-(a) ⇒ trigger = empty id; 200 ⇒ trigger requires the encrypted ride | A1 reasoning-item cell; F8 trigger class |
| P-F | mid-stream relay (G-10) | streaming `POST /v1/responses` gemini (bridge wire), induce an upstream mid-stream 4xx (content filter mid-generation) | raw `data:` SSE error frame + [DONE] relay (proxy_server.py:7330-7400) — record the verbatim shape | G-10; §3.5 stream-error cell |
| P-G | A3 cache_control fate | `POST /v1/responses` gemini (bridge wire) with a cache_control text part, wiretap enabled | marker absent from the upstream generateContent request (PREDICTED drop) or present (re-derive C2/B4 cells) | A3/C2 cache_control cell |
| P-H | A2 glm xhigh | `POST /v1/responses` `{model: "glm-5.2", "reasoning": {"effort": "xhigh"}}` | 400 (vLLM glm menu {high,medium,low} rejects) or 200 (vLLM permissive) — discriminates vLLM-side effort validation for family-unset models | A2 effort cell |

## 7. Raw-key sweep (this deliverable)

Command of record = the four-class house pattern from `plans/xwire/review-gates-q2.md:109`
(form confirmed in review-gates-q3.md:114), run as `grep -cE '<pattern>' <file>`. Per the
q3c self-sweep rule the pattern is **described, not quoted** here: (a) provider-key-style
literal (two-char sk- prefix + 8-or-more alphanumerics), (b) the x-litellm-tag header line
with literal colon, (c) api_key assignment token, (d) Bearer-scheme token with trailing
space.

Result (2026-09-18, this seat): **litellm-transform-matrix.md = 0 hits** (whole file,
405 lines at sweep time). No class-1 doc-prose false positives present (sha digests and
paths in this doc are sha12 fragments, which do not match class (a); header names are
written without the literal-colon form and the campaign key is referenced only by its
sha12 where at all).
