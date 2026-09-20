# Harness Upstream-Churn Redesign — grok-build

Date: 2026-09-19 · Scope: first-party OpenAI (codex family: gpt-5.6-sol/terra/luna) cost parity with official Codex CLI
Companion: `churn_math.py` (run in this dir; all $ figures below are its output)
Merge constraint: must not break the xai family merge surface (see MERGE-XAI-SYNC anchor doc); every change is gated on catalog `model_family` metadata, never model slug/URL.

## Executive summary

1. The 2.81× cost gap (E1: $0.5027/16 req vs $0.1788/6 req) decomposes into 2.67× request count × 1.05× per-request cost — so the redesign first **consolidates turns** (codex-parity `parallel_tool_calls:false`, in-budget MCP pre-exposure, family-gated suppression of side classifier calls), targeting ~16→8 requests, ≈50% of task cost.
2. It then **freezes the prefix**: the `<multi_agent_mode>` developer item becomes a stable, versioned conversation item at a fixed slot (retiring the per-request strip-and-re-inject in `sampler/provider.rs` that E2 caught swapping mid-session and pinning the cache at ~10.3k tokens), so every request is a strict byte-prefix extension of the last and cache-hit length grows monotonically.
3. It lands the **`cache_write_tokens` metering fix (E3) as the verification gate** for all of the above, right-sizes the codex-family tool surface from 29–31 to ~12 tools (E4), and unbricks the claude messages wire's 10k per-item cap with a thinking-aware projection (E6) — with every change family-gated so qwen3.8-27b and glm-5.2 see no wire regression in phase 1.

---

## Evidence base (wire-verified)

| # | Fact | Verified seam (code-confirmed) |
|---|---|---|
| E1 | A/B identical task, gpt-5.6-sol medium: codex $0.1788/6 req; harness $0.5027/16 req (2.81×). Turn granularity dominates. | Agentic loop: `shell/acp_session_impl/turn.rs` (`loop` at ~L2638); per-iteration injections at L2722–2735. |
| E2 | Mid-session: the "developer" item and a `<system-reminder>` user item permanently swap relative order; cached prefix pinned (~10.3k tok stuck; ~113–1,004 cache-write tok/turn until next cold write). | `sampler/provider.rs::inject_multi_agent_mode_item` (L233–251): per-request `input.retain(!is_multi_agent_mode_item)` then insert **before the last user item if the last input item is a user message, else at the end** (L246–250). Called on every request from `sampler/client.rs:2165,2334`. Reminder user items are pushed via `shell/.../reminders.rs::push_system_reminder_with_tag` (L570) → `push_user_message`. |
| E3 | Wire carries `input_tokens_details.cache_write_tokens`; session `usage.json` records `cacheCreationTokens: 0`. | Forked async-openai `types/shared/response_usage.rs::InputTokenDetails` models only `cached_tokens: u32` (required, no `#[serde(default)]`) → serde silently drops the field; `sampler/stream/responses.rs:647–654` then hardcodes `cache_creation_prompt_tokens: 0`. Downstream plumbing already exists: `shell/session/usage_file.rs` (`cache_creation_tokens` → `cacheCreationTokens` in usage.json via `UsageLedger`). |
| E4 | 29–31 built-in tools exposed to first-party codex family; lazy MCP discovery adds ~2 turns per discovered tool. | `grok_build` tool dir (~26 modules + shared `search_tool`/`use_tool`/`memory`/`skills`/`task_output`/`web_search`); `sampler_turn.rs::turn_base_tool_specs` (L343) projects them all; MCP hint "MUST call search_tool first" at `shell/.../mcp.rs:1466`; batched execution exists (`tool_calls.rs::execute_tool_calls` L386–415), so the 2-turn floor per tool is the *discovery* round trip, not serialization. |
| E5 | Official codex sends explicit `parallel_tool_calls: false`; harness omits it (API default true). Harness model parallelizes (5 function_calls/turn observed) yet takes 2.9× the turns. | `sampling-types/conversation/responses.rs:190` — `parallel_tool_calls: None` in `From<&ConversationRequest> for rs::CreateResponse` (also `store: None`, `previous_response_id: None`, `instructions: None`). Parallel calls replay their own `reasoning` siblings verbatim (`response_to_conversation_items` doc, `conversation/responses.rs:26–31`). |
| E6 | Claude models: one reasoning item > local 10,000-token per-item cap ⇒ next turn's local validation fails non-retryable; session bricks. | `sampling-types/request_validation.rs`: `MAX_MODEL_CONTEXT_ITEM_TOKENS = 10_000` (N3), `check_message_tokens` estimates the **whole** final-projected message — including the latest assistant's verbatim `Thinking { thinking, signature }` block that the API itself mandates replaying (`conversation/messages.rs::strip_thinking_blocks`, rule (b)). `RequestValidationError` variants are non-retryable by construction (T14/T24b pins). |

Pricing basis for all arithmetic (labeled assumption, sanity-checked in `churn_math.py` §0): P_in = $1.25/M, P_out = $7.50/M, cache read = 0.10× P_in, cache write = 1.25× P_in. These rates reproduce E1's implied average request cost ($0.0298–0.0314/req) for a ~28–32k-input/2k-output request at 85–90% cache hit.

---

## TOP 5 (ranked by expected cost impact × certainty)

### #1 — Turn consolidation: codex-parity agentic loop (E1, E5, supports E4)

**What to change.** The agentic loop in `shell/acp_session_impl/turn.rs` (`LoopStarted → injections → build_request → sample → execute_tool_calls_batch → loop`) is correct in shape but carries four overhead sources that official codex does not:

1. **Wire parity (E5).** Set `parallel_tool_calls` explicitly at the typed boundary (`conversation/responses.rs:190`, currently `None`) from catalog family metadata: `Some(false)` for the codex family (codex parity), `None` (unchanged) for all other families in phase 1. This is a one-field change on `CreateResponse`; no raw-JSON splice.
2. **In-budget MCP pre-exposure (E4b).** When the summed MCP schema tokens of connected servers fit a budget (default ≤ 2k tok, family-gated config), expose those MCP tools in the `tools` array from session start; the MCP-connect reminder (`mcp.rs`) already lists tool names — extend it with "schemas preloaded: …" so the model skips `search_tool` for those. Above budget, keep lazy discovery but allow `search_tool` to return **multiple** schemas per call (one discovery request instead of N).
3. **Side-call suppression, codex family only.** The laziness classifier (`laziness_classifier.rs:7` — "Idle-triggered classifier that asks the active session model") and the goal classifier make full-context model calls as part of a turn; default them off for the codex family (config flag resolved from family metadata). Rule-based prefilter (repetition/stall heuristics) stays on for everyone; model-backed classification remains available as opt-in.
4. **Compaction burst (no structural change this cut).** Two-pass prefire (`compaction.rs::run_prefire_pass1`, L292) adds a request but pre-warms the cache for the compaction request itself; keep, and let #4's metering show its net cost before touching it.

**Wire shape / state machine.**

```
per request (codex family):  body.parallel_tool_calls = false   (was: omitted)
per session (start):         tools[] = core builtins ∪ MCP-in-budget  (frozen for session life)
loop:  LoopStarted ─→ sample ─→ (tool batch, executed in parallel today) ─→ loop
       side classifier call ─── removed (codex family; rule-based checks remain)
discovery (out-of-budget):   search_tool(query, tools=[...]) → N schemas in 1 request
                             use_tool(...) alongside other calls in the same response
```

**Evidence.** E1 (dominant swing: 2.81× = 2.67× req count × 1.05× per-req), E5 (parity + replay), E4 (2 turns/tool).

**Expected impact** (`churn_math.py` §3, §5, §6): 16→8 requests saves ≈ **$0.25 (50% of the E1 harness cost)**; 16→6 ≈ $0.31. Each in-budget MCP tool pre-exposed saves ~2 requests ≈ **$0.063** for a one-time 2k-tok cache write ($0.003). `parallel_tool_calls:false` pins one reasoning sibling per response: ~$0.003/10 later requests vs the K=5 observed case, plus a byte-stable replay shape.

**Risk / regression surface.**
- `parallel_tool_calls:false` restricts batching **only** where set: codex family in phase 1. qwen3.8-27b / glm-5.2 (Responses wire, `Strict` dialect) keep today's omitted behavior — verify by wire diff (assert field absent for those families).
- Side-call suppression could raise stall rates on the codex family: the rule-based stationarity guard (`turn.rs` identical-tool-call runs) remains the backstop; measure stall/turn counts in the smoke matrix before widening to other families.
- Tool-array changes for codex family must not leak to other families (projection is data-driven from catalog family rows, not slugs — see DO NOT DO #4).
- Merge surface: `client.rs` and `conversation/responses.rs` are both in the xai-merge conflict set; the field is set at the typed `CreateResponse` boundary (survives the in-flight `catalog_wire` extraction) and no raw-body splice is added, keeping the merge cards' named-entity map valid.

**Wire-level verification plan.** Post-fix capture of the E1 A/B task (and a 2-MCP-server variant):
- (a) request count ≤ 8; per-task cost ≤ ~$0.25;
- (b) every codex-family request body has `"parallel_tool_calls": false`; non-OpenAI captures show the field absent;
- (c) zero `search_tool` requests for in-budget servers; first `use_tool` call lands in the first tool response after session start;
- (d) no request carries an `x_grok_req_id` with the laziness/goal classifier prefix (telemetry cross-check);
- (e) each eliminated request corresponds to a previously-emitted overhead class (tagged in the pre-fix capture).

---

### #2 — Prefix freeze: anchored policy items + strict prefix-extension invariant (E2)

**What to change.** Root cause, code-verified: `sampler/provider.rs::inject_multi_agent_mode_item` runs on **every** request (client.rs:2165/2334), strips the `<multi_agent_mode>` developer item from the serialized `input` and re-inserts it *before the last user item if the last input item is a user message, else at the end* (L246–250). Its position is therefore a function of the last item's role, which flips between user-turn requests and tool-loop requests; once a `<system-reminder>` user item lands after the last real user message (MCP connect, date rollover, task-completed auto-wake — all `push_user_message` paths), the developer/reminder relative order swaps once, **permanently**, and the prefix cache pins at the swap point (E2: ~10.3k tok stuck; 113–1,004 cache-write tok/turn).

Redesign:
1. **Conversation-resident, prefix-anchored policy item.** The `<multi_agent_mode>` item becomes a typed `ConversationItem` stored in history at a **fixed slot: index 1** (immediately after the base-instructions item at index 0), inserted once at session setup and replayed verbatim from history like any other item. `inject_multi_agent_mode_item` is retired; no per-request strip/re-inject remains.
2. **Versioned in-place replacement.** Item content is family-rendered exactly as today (codex: bare keyword; other families: expanded sentence — the AXIS-2 design in `provider.rs` is unchanged in rendering, only in *placement*). When the mode text changes (effort flip ultra ↔ sub-ultra, i.e. `proactive` ↔ `explicit_request_only`), the slot's content is replaced **in place** → exactly one cold write of everything after slot 1, which is rare, intentional, and now metered by #4.
3. **Invariant.** For every pair of consecutive requests in a session: `input(N+1)[:len(input(N))] == input(N)` — strict byte-prefix extension. Tail-only growth. The same discipline applies to the other body splices (reasoning `type` patch, compaction-carrier splice): they must remain content-preserving or fixed-slot, never repositioning items.
4. `instructions: None` stays — base instructions remain the history item at slot 0 (no field-level migration; the top-level `instructions` field would be a second, competing anchor and complicates the messages-wire projection).

**Wire shape.**

```
input[0]  = system / base instructions          (unchanged)
input[1]  = developer { text: <multi_agent_mode>…</multi_agent_mode> }   (stable bytes;
                                   replaced in place only on policy change)
input[2..] = append-only conversation (user / assistant / function_call /
             function_call_output / reasoning / system-reminder user items)
state:  InsertedAtSlot1(hash) ── stable replay ──→ ReplacedInPlace(hash') on policy change
```

**Evidence.** E2 directly; contributes to E1's 1.05× per-request factor and to TTFT (the post-swap region is re-prefilled every request).

**Expected impact** (`churn_math.py` §2). Per request after the swap, the region R after the pin was billed at 1.25× instead of 0.10×: R × 1.15 × P_in = **$0.00016 (R=113) to $0.00144 (R=1,004) per request** — $0.016–$0.144 per 100-request session, growing with session length (R is the live tail; 9× between the E2 measurement bounds). Plus the one-time $0.015 re-write of the 10.3k pinned prefix at the swap, and removal of the per-request re-prefill latency on that region. On long sessions (the common case for this harness) this is a second-order dollar cost but a *permanent* one — it never recovers on its own.

**Risk / regression surface.**
- **One-time prefix cold for in-flight sessions on rollout** (next request after upgrade rewrites from slot 2). Bounded and acceptable; sessions are short-lived relative to rollout.
- **Non-OpenAI models (qwen3.8-27b, glm-5.2):** they ride the same Responses wire (`Strict` dialect) and the same developer item (AXIS-2: every family). The item moves from a drifting position (end of tool-loop requests) to slot 1 → the proxy's Responses→ChatCompletions shim will map `developer`→`system` at a new position: one-time cold, then the prefix is *strictly more stable* than today (better for any proxy-side caching). Verify with one non-OpenAI task per family: wire diff shows only the position change; behavior goldens pass.
- The tag-strip logic (`is_multi_agent_mode_item`) must be retained as a **sanitizer** for resumed/forked sessions that may contain a stale-positioned legacy item: on load, if an item matching the tag is not at slot 1, move it there (one-time, at history load — never per request).
- `normalize_content_types` (non-OpenAI content-part rewrite) runs after the patch stage and is idempotent; with the item anchored, it simply sweeps slot 1 like any other item — no ordering change.
- Merge surface: `provider.rs` is **not** in the 51-file conflict set; the store-side insertion lands in `xai-chat-state` (which *is* a conflict file — coordinate with the wave-2 merge cards so the new item variant and the actor's push paths land on the merged side, not a stash port). `conversation/responses.rs` gets the `inject` call removed only; `build_responses_input` is untouched.

**Wire-level verification plan.** Capture a session that crosses ≥1 reminder-injection boundary (MCP connect, date rollover, or task-completed auto-wake) and one mid-session effort flip:
- (a) the strict prefix-extension invariant holds for **every** consecutive request pair (checker script asserts byte equality of the shared prefix);
- (b) the developer item is at input index 1 in 100% of requests, byte-constant except for exactly one in-place replacement at the effort flip;
- (c) usage.json (post-#4): cache-read length grows monotonically toward full prefix; per-request `cache_write_tokens` ≈ newly appended items only (the 113–1,004 pinned-tail write signature disappears);
- (d) a resumed legacy session (pre-fix history) shows exactly one sanitized relocation at load and a clean invariant thereafter.

---

### #3 — Codex-family tool-surface right-sizing (E4, supports E1)

**What to change.** `turn_base_tool_specs` (`sampler_turn.rs:343`) projects all 29–31 built-in `grok_build` tools onto every codex-family request: each definition is ~150–400 tokens of JSON schema riding in **every** request body (and the `tools` array is part of the cacheable prefix), and the broad surface steers the model toward smaller, more cautious steps (one `read_file` per call, separate `list_dir` + `grep` + `read_file` where codex's shell-centric surface yields one compound shell step). Redesign:
1. **Family-scoped projection**, reusing the existing `child_tool_projection` machinery (a per-role projection already exists; this is the same seam applied to a family axis): the codex family gets a **core surface (~10–12 tools)**: `run_terminal_command` (shell), `read_file`, `grep`, `list_dir`, `search_replace`, `todo_write`, the subagent trio (`spawn_subagent`, `get_command_or_subagent_output`, `kill_command_or_subagent` + `wait`), hosted `web_search`, plan-mode pair.
2. **Discoverable tier** (not deletion): media gen (`image_gen`/`image_edit`/`video_gen`), app stubs (`app_builder`, `deploy_app`, `init_or_update_app`), `scheduler`, `lsp`, `ask_user_question`, `send_feedback`, `send_subagent_message`, `workflow`, `memory` get exposed only after a one-turn promotion (model calls a lightweight `enable_tools`-style call, or a catalog capability flag flips on session resume). Promotion happens on a cold-write boundary and is metered by #4.
3. **Session-frozen surface.** The projection is resolved at session start and frozen for the session life (the existing `backend_search_active` gate already removes `web_search` dynamically today — that gate must also become session-stable, or it is itself a prefix buster).

**Wire shape.** `tools[]` on codex-family requests: 29–31 entries → ~12, byte-identical across all requests of a session (invariant: `sha256(tools array)` constant per session; checked in the capture tooling alongside #2's prefix invariant).

**Evidence.** E4 (surface + discovery turns), E1 (step granularity: coarser shell-centric steps under a small surface).

**Expected impact** (`churn_math.py` §4): 30→12 tools × ~250 tok ≈ **4.5k tok/request** removed: ≈ **$0.015 on the 16-request E1 task, ≈$0.06 per 100-request session**, plus TTFT on every cold write, plus compaction delay (each avoided compaction avoids its prefire+compact+continue request burst — a second-order multiplier on #1).

**Risk / regression surface.**
- **Non-OpenAI: zero-diff in phase 1** — the projection is codex-family-only; qwen3.8-27b and glm-5.2 keep the full 29–31 surface. Wire-diff smoke asserts byte-identical `tools[]` for both.
- Behavioral: codex-family tasks that legitimately need `image_gen`/`video_gen` pay one promotion turn; add media-gen goldens to the smoke matrix. If a stub tool (app_builder etc.) is an intentional product A/B, keep it and measure it separately — flag for operator adjudication.
- The `tools` array is prefix-cached: any mid-session surface change invalidates the *entire* session prefix (tools serialize before input). Hence the session-frozen rule above; promotion is deliberately rare and metered.
- Merge surface: projection config rides the catalog family metadata (catalog authority stack — apex-071/72c area, a known collision zone with upstream config drift). Keep the data in existing family rows (a new `tool_tier` field) rather than new config files, so the wave-2 catalog adjudication sees one field addition.

**Wire-level verification plan.**
- (a) codex-family capture: `tools[]` ≤ 12 entries, identical hash across all requests;
- (b) qwen3.8-27b + glm-5.2 captures: `tools[]` byte-identical to pre-fix baseline;
- (c) E1 task input tokens drop ≥ 4k per request vs pre-fix; task goldens (code edit, subagent spawn, media-gen-with-promotion) pass;
- (d) a session with an MCP server reconnect shows exactly one prefix cold (metered) — no silent repeats.

---

### #4 — `cache_write_tokens` accounting end-to-end (E3) — land first

**What to change.** The metering gap, chain-verified: the wire's `response.completed` event carries `usage.input_tokens_details.cache_write_tokens`; the forked async-openai `InputTokenDetails` models only `cached_tokens: u32` (required, no `#[serde(default)]`) so serde silently drops the field; `sampler/stream/responses.rs:647–654` then hardcodes `cache_creation_prompt_tokens: 0`. Everything downstream already works: `TokenUsage.cache_creation_prompt_tokens` (documented "billed at ~1.25x"), the chat-state `UsageLedger`, and `shell/session/usage_file.rs` → `cacheCreationTokens` in usage.json (the messages wire already populates it correctly — only the Responses wire is broken).
1. Fork DTO: `InputTokenDetails { cached_tokens: Option<u32> /* + #[serde(default)] */, cache_write_tokens: Option<u32> }` — both optional+defaulted, so a proxy that omits either detail can no longer fail the whole usage parse (a latent 400-class today).
2. `stream/responses.rs`: map `u.input_tokens_details.cache_write_tokens` into `cache_creation_prompt_tokens` (replace the `0`).
3. Per-request rollup: surface `cache_write_tokens` in the turn-level telemetry event (the `ModelResponseReceived` event at `turn.rs` already records `cache_creation_tokens` — it now gets real data) and in the status line (the invariant at `status_line.rs:84`, `input ≥ cached_read + cache_creation`, now actually constrains).

**Wire shape.** No request-shape change at all; response-decode + bookkeeping only.

**Evidence.** E3.

**Expected impact.** Direct token savings: **zero** — this is the gate, not a lever. It converts every other change in this doc from "claim" to "measured": #2's verification (pinned-tail writes disappearing) and #3's (prefix shrink) are unreadable without it, and it enables standing churn monitoring (cache_write/turn above tail growth ⇒ prefix pin ⇒ alert).

**Risk / regression surface.** Near-zero: additive optional field with serde default, one mapping line, test-support SSE fixtures need `cache_write_tokens` added (mechanical golden updates in `xai-grok-test-support/src/sse.rs`). No non-OpenAI impact (the field simply stays 0/absent on the messages/chat wires, which already record or lack their own cache-write signal).

**Wire-level verification plan.** Capture a 10-request session (codex family): (a) usage.json `cacheCreationTokens` > 0 on request 1 and **exactly equals** the raw capture's `input_tokens_details.cache_write_tokens` for every request (asserted by the capture tool against the untyped SSE bytes); (b) steady-state cache-hit requests record 0; (c) telemetry event and status-line values agree with usage.json.

---

### #5 — Thinking-aware per-item gate on the messages wire (E6)

**What to change.** The messages-wire validator (`sampling-types/request_validation.rs`, N3 gate `check_message_tokens`) estimates each final-projected item as compact-JSON bytes / 4 and rejects at > 10,000 tokens — non-retryable by construction. But the latest assistant message must replay its `Thinking { thinking, signature }` block **verbatim** (spec rule (b), `conversation/messages.rs::strip_thinking_blocks`): that text is opaque provider state the API itself mandates, not harness-authored content. A single high-reasoning claude turn (> ~40 KB thinking text) makes the *next* request's validation fail locally, before HTTP → the session bricks (E6).
1. **Thinking-aware estimate:** in `check_message_tokens`, exclude `Thinking` block text payloads from the item estimate; count the `signature` + block envelope instead. Rationale is the cap's own documented status: a *local* request-safety bound over what the harness emits, "not model capability metadata" — and N2's 32 MB body cap remains the true size guard (plus the model's own context window, handled by the existing auto-compaction).
2. **Degraded replay fallback:** if a latest-assistant thinking pair is still pathologically large after exclusion (signature + rest over cap), replay it as `RedactedThinking { data: signature }` (the API's documented alternative for non-replayable thinking) instead of failing: the model loses that one turn's reasoning continuity, the session survives.
3. **State machine:**

```
Validate(N3-thinking-aware) ── Ok ────────────────────────────────→ send
                          └─ ItemTokenLimitExceeded (latest thinking pair)
                                → ProjectAsRedacted(keep signature)
                                → Re-validate ── Ok ──→ send (continuity degraded, logged)
                                                 └─ still over → typed terminal error
                                (surfaced with /compact hint; NOT a silent brick)
```

N1 (count), N2 (body, two-pass encode), and all V1 invariants are untouched; only the N3 input to `check_message_tokens` changes for `ContentBlock::Thinking`.

**Evidence.** E6.

**Expected impact.** Each brick costs a full restart: cold re-write of the whole context ($0.0625 at 40k ctx, $0.156 at 100k ctx, `churn_math.py` §7) plus all output already generated is stranded and user work is lost. Frequency is low (claude family + high-reasoning turn) but the tail is catastrophic — this is the difference between "session costs 2× once" and "session is unusable mid-task".

**Risk / regression surface.**
- **Non-OpenAI: zero surface change** — the messages wire is claude-models-only; qwen3.8-27b / glm-5.2 ride the Responses wire and never touch this validator.
- Frozen-spec impact: N3's constant, gate order, and error taxonomy are spec-pinned (REQVALID-1, T-pins; non-retryable classifier families verified at T14/T24b). The thinking-exclusion is an **amendment** requiring a spec addendum and re-pinned goldens for T8–T15 (mechanical: fixtures with oversized thinking text flip from reject→accept, plus two new fixtures for the redacted fallback). The Display phrasing sweep (T14a) must re-run so the retryable-classifier families stay disjoint.
- A proxy with its own per-message size limits would still 400 on truly giant thinking — N2 and the 400 classifier families (model-bound family 2: "thinking" + "signature") remain as the outer net; that 400 is *retryable-classified as model-bound*, which is the correct behavior (it is model-bound).

**Wire-level verification plan.** Capture a claude session containing a > 10k-token reasoning turn followed by a tool-call turn: (a) the next request passes local validation (no `ItemTokenLimitExceeded` in logs/events); (b) the wire body's latest assistant message carries the `thinking` block with the original signature (or the `redacted_thinking` equivalent in the fallback case, flagged by a structured log); (c) the session completes the task; (d) full REQVALID suite green with amended goldens, including the T14a phrasing sweep.

---

## DO NOT DO

1. **Don't adopt server-side conversation state (`previous_response_id` / `store: true`).** The proxy load-balances `/responses` across Azure deployments with different API keys, and reasoning/compaction ciphertext is only decryptable by the deployment that produced it (the existing `strip_encrypted_content` rationale, `provider.rs:253–266`) — server-side state is deployment-bound and opaque, and the harness needs local history rewrite (compaction, fork, mid-session model switch, cross-family) that server state forbids. It would trade cache-write cost for a new class of cross-deployment 400s.
2. **Don't pre-expose *all* MCP tools to eliminate discovery turns.** The `tools` array is part of the cached prefix; unbounded MCP schemas bloat every request, and any MCP reconnect/change then busts the *entire* session prefix. #1's in-budget pre-exposure (≤ 2k tok) is the safe version; unbounded is a prefix-stability anti-pattern.
3. **Don't delete the 10k per-item cap wholesale to fix E6.** It is a frozen-spec invariant with byte-pinned goldens and a verified non-retryable classifier family (T14/T24b). The thinking-aware projection (#5) keeps the guard on harness-authored content while unbricking provider-state replay — wholesale deletion also removes the last per-item size check before the 32 MB body cap.
4. **Don't key any of this on model slug or endpoint URL.** Every wire seam in this harness gates on catalog `model_family` metadata by contract (`provider.rs` module doc: "gated on `model_family` metadata — never on model slugs or URLs"). A slug-based special case would break the in-flight xai merge (catalog authority stack is the top collision zone) and silently misroute future family rows.
5. **Don't set `parallel_tool_calls: true` explicitly to "help batching".** E1 shows the harness already parallelizes under the API default and still takes 2.9× the turns — the flag is not the missing lever; explicit-true additionally widens replay (one `reasoning` sibling per parallel call, replayed verbatim forever) and the vLLM/SGLang shims behind qwen/glm have no parity contract for it.

---

## Cross-cutting

**Non-OpenAI regression guardrail (qwen3.8-27b, glm-5.2).** Phase-1 wire-diff matrix for both models, run on the hermetic smoke driver (isolated homedir, frozen dogfood config — smoke/wstream precedent) before and after each change: assert byte-identical request bodies except where the change explicitly targets all families (#2's slot move — one-time cold, asserted by diff), no new error classes, per-task cost within ±5%. #4's metering gives the same A/B readout for cost drift in production.

**Landing order.** #4 first (metering gate, ~1 day, near-zero risk), then #2 (prefix freeze — largest permanent tail, single-file core change), then #1 (turn consolidation), #3 (tool surface), #5 (messages-wire, independent branch). #1 and #3 are coupled (both touch the `tools[]`/turn surface) and should land in one review wave; #2's `xai-chat-state` insertion must be sequenced with the xai-merge wave-2 cards (actor/state.rs is a hard-conflict file).

**What a fully-landed capture must show (campaign gate).** On the E1 A/B task: ≤ 8 LLM requests; `parallel_tool_calls:false` on every codex-family request; prefix-extension invariant 100%; cache-hit length monotone to ≥ 95% of previous request input; `cache_write_tokens` per request ≈ appended items only; per-task cost ≤ ~$0.25 (−50% vs pre-fix harness, within 1.1× of codex).
