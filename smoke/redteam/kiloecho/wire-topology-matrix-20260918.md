# Wire-topology matrix — family × wire × backend (consolidated, ratchet analysis)

Date: 2026-09-18 · Author/seat: glm read-only intel seat (apex-v2-grok-build, kiloecho recon, RETRY after predecessor transport-death) · Bead: `apex-6vu` · Status: **DRAFT**
Sources (first-hand unless marked): harness tree `~/Projects/upstream/wt/grok-build-responses` @ HEAD **0fc1060** (branch `feat/first-class-responses-catalog`); proxy config `~/Projects/cli-ops/upstream-infrastructure/llm-proxy` @ **956b6d2** (branch `bugfix/grok-4.6-reasoning-flags`, deployed = litellm 1.90 + house patches); litellm pin `~/Projects/cli-ops/upstream-infrastructure/litellm` @ tag **v1.90.0** (NEW layout, no `adapters/` dir); harness seat config `~/.grok/config.toml`; codex-combined seat config `~/.codex/config.toml`. Consolidating source docs: `plans/xwire/crosswire-matrix.md` (566L, read @0fc1060 arc), `plans/xwire/golden-engine-capability-glm-20260918.md` §2, `plans/provenance/deliverable-map-20260918.md`. Key sha12 `9f3f56a263da` (never echoed; live-probe auth). Raw-key sweep: 0 in this doc.

## §0 — Read-first verdicts (consolidated, re-verified first-hand @0fc1060)

1. **Fidelity is NOT a function of the wire-family label (R3).** Two axes independent of the family label decide what rides the wire: (a) the per-model-row `strict_responses_input` flag — live ONLY on the three AZ rows (`gpt-5.6-sol` `~/.grok/config.toml:16`; terra/luna per matrix §0 verdict 1 at config :186/:192, family `codex`) — and (b) the **encryption boundary** (which Azure deployment minted the ciphertext). Same family + different boundary → different KEEP/strip policy; different family + same strictness → different tier floor. Source: crosswire-matrix §0 verdict 1; re-verified sol row firsthand.
2. **There is NO `"OpenAI"` literal name-gate for remote compaction.** The remote-compaction-v2 gate is `responses_wire_dialect_for_model_family(model_family) == Codex && api_backend == Responses` (`client.rs:2625`, dialect resolver `provider.rs:59-67`: `codex`→Codex, `xai`→Xai, `None`→Xai, other→Strict). sol/terra/luna are `model_family="codex"` (`~/.grok/config.toml:15,186,192`) so they **PASS** the dialect gate. They self-summarize (local compaction) only because `remote_compaction_v2 = false` (`~/.grok/config.toml:308`, global, overrides the `true` default at `compaction.rs:37`) — NOT because of a name exclusion. **Correction to the task's hypothesis**: the mechanism is the config flag, not an "OpenAI" literal; and the dialect gate would admit sol. The task's *outcome* (sol self-summarizes) is correct; the *mechanism* stated is not.
3. **The chat-completions wire has ZERO projection hooks (matrix glm2 E1, re-verified).** `patch_responses_request` (`provider.rs:87-103`), `normalize_content_types` (`provider.rs:316`), `strip_encrypted_content_input` (`provider.rs:209`), and `project_strict_responses_input` (`provider.rs:268`, strips `content` :278 / `id` :279) all operate on `body["input"]`, absent from chat-completions bodies. The CC funnel `conversation_to_chat_messages` (`chat_completions.rs:185-215`) is the only shaping (reasoning→text fold; id/signature never serialized). The sole live CC proxy row `gemma-4-31b` is **WITHDRAWN** (`config_seclab.yaml:1591` comment: "MIG-backed deployment never answers, every request wedges"). No live CC row exists.
4. **`is_openai_family("")` returns TRUE (matrix glm2 E2, re-verified `provider.rs:112-116`).** `patch_responses_request` does `model_family.unwrap_or_default()` → empty → `is_openai_family` true → `normalize_content_types` skipped. A family-less responses-wire model silently skips content-type normalization. E2 pin `is_openai_family_empty_is_true_is_pinned` lives at `provider.rs:941`.
5. **The BRICK gap is real (matrix verdict 5).** Unclassified 400 phrasings fall through `classify_error` (`retry.rs:105`) to `RetryDecision::Fatal` (`retry.rs:186`, the final fallthrough; 400 is non-retryable). Two known unclassified phrasings: vLLM pydantic dotted `input.N.id` without "invalid"; Azure `input[N].call_id` orphan (`.call_id` ⊅ `.id`). Owner .74 (pair-aware strip + phrasing coverage). [Classifier predicates live in the `xai-grok-sampling-types` `is_*` methods; the matrix's `error.rs:428-477` cite is era-stale vs the 0fc1060 layout where classification is `retry.rs:105-186`.]
6. **The deployed effort-clamp (M-1) is STOCK litellm v1.90.0 flag-driven, NOT a house patch (CORRECTED 2026-09-19, sibling stream-A handoff gap 1: litellm checkout HEAD = v1.90.0 tag exactly, rev-list 0 — the earlier 'proxy-image house-patch delta' claim is retracted).** Stock mechanism: `normalize_reasoning_effort_value` (`litellm/llms/anthropic/experimental_pass_through/utils.py:16-57`) keeps xhigh only when the row's model_info has `supports_xhigh_reasoning_effort`, else clamps to high; callers `adapters/handler.py:389,400,407` (messages) + `responses_adapters/handler.py:80,84` (responses). The qwen row (`config_seclab.yaml:1598-1627`) lacks the flag -> xhigh->high -> the vLLM menu {xhigh,medium,low} rejects 'high' -> 400 (probe P6, LIVE). Separately, the stock openai-class gpt-5 path is drop-or-raise (`gpt_5_transformation.py:237-250`) — that is the W15/triage-#27 informational class, not M-1. Fix = one line `supports_xhigh_reasoning_effort: true` on the qwen row (`apex-ayl.83`, batched into the next llm-proxy deploy). Re-confirmed LIVE (probe P6, §5).
7. **The matrix's litellm cite `_translate_thinking_to_openai` @`adapters/transformation.py:1119-1123` is STALE vs v1.90.0.** The `adapters/` dir does not exist in v1.90.0; the function was not found. The responses-shim transformation is now `litellm/interactions/litellm_responses_transformation/transformation.py` (`transform_interactions_request_to_responses_request` :27; `_transform_interactions_input_to_responses_input` :90). Any ratchet that cites the old path must be re-anchored.

## §1 — The matrix (6 families × 3 client wires × backend)

| Family (model) | Client wire (`~/.grok/config.toml`) | litellm-1.90 backend / transformation | strictness |
|---|---|---|---|
| **AZ** gpt-5.6-sol/terra/luna | `/v1/responses` (`:14`) family `codex` (`:15,186,192`) | Azure passthrough on `/v1/responses`; `EncryptedContentAffinityCheck` on `encitem_` ids (503 class) | `strict_responses_input=true` (sol `:16`; terra/luna per matrix) |
| **VL-qwen** qwen3.8-27b | `/v1/responses` (`:59`) family `qwen` (`:61`) | openai-class → responses→chat shim (`litellm/interactions/litellm_responses_transformation/transformation.py:27`); provider id `litellm:custom_llm_provider:openai` (probe P1) | FALSE (lenient) |
| **VL-glm** glm-5.2 | `/v1/responses` (`:35`) family `glm` (`:36`) ctx 128000 (`:38`) | same openai-class responses→chat shim (probe P2, provider id `openai`) | FALSE (lenient) |
| **VX-R** grok-4.6 | `/v1/responses` (`:5`) family `xai` (`:6`) ctx 500000 (`:9`) | vertex-xai passthrough; `supports_reasoning:true` (`config_seclab.yaml:558`); xhigh alias `proxy.py:197-228` (Vertex-only) | FALSE (lenient); `is_openai_family("xai")=true` → normalize skipped |
| **VX-M** claude-sonnet/opus | `/v1/messages` (`:80+`) | vertex-anthropic NATIVE (`msg_vrtx_` id, probe P5); other classes /messages-translated | n/a (messages-only flag) |
| **VX-G** gemini | `/v1/responses` (`:162`) family unset | responses→vertex `/generateContent` bridge server-side (provider id `vertex_ai`, probe P4); bridge strips reasoning content (OQ-2) | FALSE (lenient); family None → E2 normalize skip |
| **CC** gemma-4-31b | (would be `/v1/chat/completions`) | openai chat — **WITHDRAWN** (`config_seclab.yaml:1591`) | n/a (zero hooks) |

## §2 — Per-cell detail (a: litellm transformation · b: harness client behavior · c: status · d: provenance)

**AZ (sol/terra/luna) × /v1/responses.** (a) Azure passthrough; `EncryptedContentAffinityCheck` on `encitem_` ids. (b) Full responses chain: `patch_reasoning_text_types` → `patch_responses_request` (codex dialect, `provider.rs:87`) → `strip_encrypted_content_input` (`provider.rs:209`, ungated, every send) → `project_strict_responses_input` (`provider.rs:268`, strips id+content, keeps summary). `remote_compaction_v2=false` (`:308`) → local self-summarize. (c) **KNOWN-GOOD** post-F2 (strict projector removes the F3/F4/F5/503 origins; matrix verdict 2). Same-boundary sol→sol KEEP pinned (probe-C, `apex-ayl.75`). (d) D-01 (R0 responses-wire), D-11 (affinity classifier).

**VL-qwen × /v1/responses.** (a) openai-class responses→chat shim (`litellm_responses_transformation/transformation.py:27`); vLLM mints `rs_*` ids, no `encrypted_content`. (b) `normalize_content_types` ACTIVE (family=qwen → `!is_openai_family` → normalize fires, `provider.rs:101`); strict projection off. (c) **KNOWN-GOOD** on responses (probe P1=200; matrix qwen-resp PASS). **KNOWN-BROKEN** on `/v1/messages` bridge: xhigh→high clamp → 400 "Unexpected reasoning effort high" (probe P6=400; matrix §7.1 M-1; bead `apex-ayl.83`). (d) D-01; M-1 = `apex-ayl.83`.

**VL-glm × /v1/responses.** (a) same openai-class shim (probe P2, provider `openai`). (b) normalize ACTIVE (family=glm). glm effort menu {high,medium,low} (`~/.grok/config.toml:43-54`) so the xhigh→high clamp is a no-op for glm (matrix §7.1 point 5). (c) **KNOWN-GOOD** on responses (probe P2=200; glm-resp PASS). **ADJACENT/INFRA**: glm on the codex-combined v1 transport 400s when a user/tool-result message is replayed as responses-shape `input_text` parts on the chat_completions wire (predecessor death 2026-09-18; triage row #14, bead `apex-xt2`; see error-class-audit adjacent section). (d) D-01; row #14 = `apex-xt2`.

**VX-R grok-4.6 × /v1/responses.** (a) vertex-xai passthrough. (b) `is_openai_family("xai")=true` → normalize skipped; `supports_reasoning:true` (`config_seclab.yaml:558`) + xhigh alias (`proxy.py:197`, Vertex-only). (c) **KNOWN-GOOD** for reasoning effort (proxy test `test_grok46_reasoning_effort` parametrized [high,xhigh]); **KNOWN-DEPLOY-ISSUE** rtok=0 streaming (OQ-7, `bugfix/grok-4.6-reasoning-flags`); **KNOWN-BROKEN** hosted `x_search` tool rejected (triage #15, `apex-ayl.76`). (d) D-01; grok-4.6 reasoning PR.

**VX-M sonnet/opus × /v1/messages.** (a) vertex-anthropic native (`msg_vrtx_` id, probe P5=200). (b) messages build only: R8 identity suppression (`messages.rs:676-678` flag, `:744-760` gate) + stage-6 (`:266-305`); harness mints `id:""` (`stream/messages.rs:544`); NO D-ENC/strict (responses-only seams). (c) **KNOWN-GOOD** native route (probe P5); open gap = foreign-signature edge (ratify (a) pending). (d) D-01; R8/stage-6 = matrix §1.2.

**VX-G gemini × /v1/responses.** (a) responses→vertex `/generateContent` bridge (provider `vertex_ai`, probe P4=200). (b) responses chain like VL but family None → E2 normalize skip; thoughtSignature = proxy-surface only (OQ-4, out of harness reach); bridge strips reasoning content (OQ-2, token-count-only). (c) **KNOWN-GOOD** on responses (probe P4=200 — compat claim VERIFIED); thoughtSignature continuity UNPROVEN (proxy-level, OQ-4); `/v1/messages` route 403 "Tool is blocked" per-model (matrix §7.2 M-2, `apex-ayl.20`). (d) D-01; M-2 = `apex-ayl.20`.

**CC gemma × /v1/chat/completions.** (a) openai chat. (b) ZERO projection hooks (verdict 3/6); cc funnel only shaping. (c) **NO LIVE ROW** — withdrawn (`config_seclab.yaml:1591`). T3 floor has no implementation site (matrix OQ-16). (d) D-01 (R0); CC axis = matrix glm2 E1.

## §3 — Ratchet verdict

**The proxy is the ONLY transport** (single gateway `https://llm-proxy-api.ai.eng.netapp.com/v1`; all rows `base_url` = this, `~/.grok/config.toml:34,58` and `~/.codex/config.toml:10`). Two ratchets:

- **Transport ratchet** = pin the proxy-side matrix + `config_seclab.yaml` per-model capability flags in llm-proxy repo tests.
- **Client ratchet** = harness byte-pins + crate fixtures. Storage-form pin **ACTIVE** (all xwfix cells, `xwfix_cell_diff_storage`); wire-form pin **DORMANT** (no `expected_wire.json` fixtures; op wire hook hardcodes `[]` normalize; golden-engine §2 verdict PARTIAL).

**Already pinned (proxy-side, jenkins/pytest):**
- `tests/test_response_api.py::test_grok46_reasoning_effort` (+ `_stream`) — guards `supports_reasoning` flag (`config_seclab.yaml:558`) AND the xhigh→high alias hook (`proxy.py:197`). Parametrized [high, xhigh].
- `tests/test_api_messages.py`, `tests/test_api_gemini.py` — messages/gemini route coverage (existence verified; depth not audited this seat).

**Missing (proxy-side):**
- No test pinning the responses→chat shim **content-part normalization** (`input_text`→`text`) for vLLM — the row-#14 class. The rejection is the request-side pydantic validation (ChatCompletionRequest) of responses-dialect content parts; the request-param whitelist (store/include/truncation/background silently dropped under drop_params:true) is `litellm/responses/litellm_completion_transformation/transformation.py:83-100`. Sibling stream-A adjudication (2026-09-19, gap 2) disambiguated the three same-named files: `completion_extras/litellm_responses_transformation/transformation.py` = OUTPUT direction (stored completion -> responses items, NOT this class — the pre-2026-09-19 cite at ':27' was imprecise), `interactions/...` = the Interactions API transformer (unrelated). A regression here breaks every vLLM responses call.
- No test for the `/v1/messages`→openai-class **effort clamp** (M-1, `apex-ayl.83`) — the xhigh→high selective clamp is STOCK v1.90.0 flag-driven (verdict 6, corrected 2026-09-19): ratchet-able by a stock-checkout unit test on `normalize_reasoning_effort_value` (utils.py:16-57) + a config assert (qwen row lacks `supports_xhigh_reasoning_effort`), with the LIVE probe P6 as the integration confirmation.
- No test for the **route allowlist** — `/v1/responses/compact` is NOT in `ALLOWED_ROUTES` (`app/common/routes.py:38-67`; confirmed LIVE probe P3=403 "Route is blocked"). The .78 ship-gate NF-3/S05 covers this but PR 1914 is unmerged (deliverable-map L173).
- No **per-model capability-flag matrix** test asserting every `config_seclab.yaml` row's `supports_reasoning` / `store` / `mode` / `max_input_tokens` against expected.

**Is the litellm transformation matrix "the thing to ratchet against"? — YES-IN-PART, with conditions:**
1. The proxy IS the only transport, so the transformation matrix IS the wire-shaping boundary of record — ratcheting it is correct in principle.
2. **Condition (deployed-image delta — SCOPED DOWN 2026-09-19):** the deployed image is NOT reproducible from any tracked branch (KE-2 #1) — the condition STILL HOLDS for image-level behavior generally — but M-1 specifically is fully explained by stock v1.90.0 + config (verdict 6 corrected; the house-patch example was retracted). The ratchet key = (X-Litellm-Version live-read from any in-wave call, proxy git rev, config_sha256:12) read at gate time; the M-1 class may be pinned by stock unit test + config assert, everything else needs LIVE integration tests against the DEPLOYED proxy supplemented by config asserts.
3. **Condition (runtime routing):** the transformation is runtime-routed per model class (openai-class → responses shim; vertex-anthropic → native; vertex-xai → passthrough; vertex-gemini → generateContent bridge). A static config-flag matrix is necessary but NOT sufficient — the grok-4.6 test proves this (it guards BOTH the flag AND the alias hook behavior). Every cell needs a behavior-level pin.
4. **Condition (cite re-anchor):** the matrix's litellm cites (`adapters/transformation.py`, `_translate_thinking_to_openai`) are STALE vs v1.90.0's new layout. The ratchet fixtures/cites must be re-anchored to `litellm/interactions/litellm_responses_transformation/` + `litellm/llms/openai/chat/gpt_5_transformation.py` before they can pin anything.

## §4 — Does the proxy matrix know about client switch semantics?

**NO.** Evidence from code: the proxy is **stateless per-request**. `proxy.py` holds the route allowlist (`app/common/routes.py:38`), the Vertex alias hook (`:197-228`), the model-cost suffix patch (`:149`), and `BlockDisallowedRoutesMiddleware` (`:77-143`). None of these carry session/switch/family/model_family state. The client switch semantics are entirely harness-side:
- **thinking-strip / D-ENC** — `provider.rs:209` (`strip_encrypted_content_input`), call sites `client.rs:1981/2148/2406`. Harness-only.
- **R8 identity suppression** — `messages.rs:676-678` / `:744-760` + stage-6 `:266-305`. Harness-only, messages-wire.
- **xw_ re-key** — the .71 projector (`projection.rs`, storage form at switch time). Harness-only.
- **preemptive family-switch compact** — `is_family_switch` (`agent/handlers/model_switch.rs:60-68`) + wall (`acp_session_impl/model_switch.rs:144-163`). Harness-only.

The ONE proxy-side switch-adjacent signal is the `encitem_` id / `x-litellm-tags` affinity pointer (the 503/401 model-bound class, D-11), which is a **deployment-routing** pointer back to the Azure pool that minted ciphertext — NOT a switch semantic. The proxy routes by deployment tag; it does not model "the session switched families". **Conclusion: the proxy matrix is blind to client switch semantics; ratcheting it covers the wire-shaping boundary but NOT the switch-time fidelity invariants, which remain a client-ratchet responsibility.**

## §5 — Live probe log (6 probes, key sha12 `9f3f56a263da`, never echoed; tiny prompts)

| # | Probe | Result | Evidence |
|---|---|---|---|
| P1 | qwen3.8-27b `/v1/responses` 1-tok | **200** | provider id `litellm:…:openai`; lapsed-400 confirmed (class-(a) vLLM brick LAPSED) |
| P2 | glm-5.2 `/v1/responses` 1-tok | **200** | provider id `openai`; clean |
| P3 | POST `/v1/responses/compact` | **403** | `{"message":"Route is blocked","type":"auth_error","param":"/v1/responses/compact"}` — route allowlist confirmed (`routes.py:38`) |
| P4 | gemini-3.5-flash `/v1/responses` 1-tok | **200** | provider id `vertex_ai`; gemini-on-responses compat VERIFIED |
| P5 | claude-sonnet-5 `/v1/messages` 1-tok | **200** | id `msg_vrtx_…`; vertex-anthropic native route confirmed |
| P6 | qwen3.8-27b `/v1/messages` xhigh | **400** | `Unexpected reasoning effort high. Supported types are xhigh (default), medium, and low` — M-1 clamp re-confirmed LIVE |

(2 probes held in reserve; not needed.)

## §6 — Cite-drift register / corrections

- Matrix litellm cite `_translate_thinking_to_openai` @`adapters/transformation.py:1119-1123` → **STALE** (no `adapters/` dir in v1.90.0; function absent). Re-anchored to `litellm/interactions/litellm_responses_transformation/transformation.py:27` + `gpt_5_transformation.py:237`.
- Matrix classifier cite `error.rs:428-477` → era-stale vs 0fc1060; classification now `retry.rs:105-186` (`classify_error` → `RetryDecision::Fatal` fallthrough `:186`).
- Task hypothesis "provider name == 'OpenAI' literal gates remote compaction; sol self-summarizes" → **CORRECTED**: no `"OpenAI"` literal; gate is Codex dialect (`provider.rs:63`); sol passes it (family `codex`); self-summarize is from `remote_compaction_v2=false` (`~/.grok/config.toml:308`).
- Task numbering "#17=M-1 effort-clamp" → first-hand: 18 rows total (IDs #1..#17 plus #5a/#55b split); **#17 = `Unexpected reasoning effort` (M-1, `apex-ayl.83`)**; #16 = compaction_trigger 400 (`apex-ayl.82`). (The #5a/#5b split makes 18 entries.)
- Seat-config asymmetry hypothesis "glm rides chat_completions, qwen rides responses" → **NOT confirmed in config**: in the grok product config BOTH glm and qwen are `api_backend="responses"` (`:35,:59`). The codex-combined transport (`~/.codex/config.toml`) has a single `llm_proxy` provider with no per-model wire selection; glm→chat_completions is a codex-combined v1 runtime-internal behavior, UNVERIFIED from config. Classified infra/adjacent (see error-class-audit).

## §7 — Overwatch reconciliation addendum (2026-09-18 23:5xZ, overwatch first-hand)

**M-1 mechanism CORRECTED.** This doc §2/§6 stated the xhigh→high clamp is a
"deployed-image house-patch delta, NOT in the stock v1.90.0 checkout". First-hand
verification against the v1.90.0 checkout (the path this seat did not search) refutes
that: the clamp is STOCK litellm, flag-driven:
- `litellm/llms/anthropic/experimental_pass_through/utils.py:14-59` —
  `normalize_reasoning_effort_value`: `effort=="xhigh"` → keep "xhigh" ONLY if
  `model_info.get("supports_xhigh_reasoning_effort")`, else → "high". (Also: "max" →
  max/xhigh/high chain; "minimal" → minimal/low.)
- `llm-proxy/app/api/config_seclab.yaml:1598-1627` — the qwen3.8-27b row's
  `model_info` carries NO `supports_xhigh_reasoning_effort` flag → every xhigh on the
  `/v1/messages`→openai-class bridge clamps to "high" → vLLM Qwen3 menu
  {xhigh, medium, low} rejects "high" → 400 (probe P6 signature, verbatim).
- Full chain: harness xhigh → STOCK normalize (flag absent) → "high" → vLLM 400.
  glm is unaffected (its menu includes "high"; clamp is a no-op) — consistent with P2=200.
- **Fix direction (owner apex-ayl.83):** add `supports_xhigh_reasoning_effort: true` to
  the qwen row's `model_info` in config_seclab.yaml (one config line; no proxy code
  patch) — or lower the client-side qwen default effort. The deployment genuinely
  supports xhigh (it is the vLLM default), so the flag is truthful.
- Source of the correction: parallel sibling KE-2 lane doc
  `kiloecho/litellm-transform-matrix.md` (xw_ke2_litellm_qwen, 23:10Z); overwatch
  re-verified both cites first-hand. The "image delta" residual in this doc (what could
  NOT be located) is retired: nothing image-specific is required to explain P6.

— end —
