# Fixtures — xsearch_replay (apex-ayl.76 W2-1 XSEARCH-REPLAY-DIALECT)

Bead owner: **apex-ayl.76** · Driver: sdd76_driver_qwen · Date: 2026-09-18 (NY)
Ruling: frame-scale fixtures in the crate test tree next to the consuming test
(brief §4, coordinator ruling, operator-accepted).

## Mint source

**Live mint ATTEMPTED, then DONOR-MINT fallback per brief §5.3.**

- Live attempt: 2026-09-18T005237Z, model grok-4.6, binary-of-record
  sha256:12 `39f836e8633d` (target/release/grok-responses), proxy
  `https://llm-proxy-api.ai.eng.netapp.com` (key from env CODEX_LLM_PROXY_KEY,
  never echoed), store=false, wire captured via smoke/wiretap/wiretap.py.
  Declared hermetic patches (copied config only; live config untouched):
  `features/turn_summary=false`, `model/grok-4.6/api_backend="responses"`,
  `model/grok-4.6/supports_backend_search=true` (live catalog row has it false).
  Capture ref: `grok/plans/provenance/fixtures/xreplay76/mint/report/20260918T005237Z/`
  (req-003 = the grok-4.6 /v1/responses request carrying the `{"type":"x_search"}`
  hosted entry; resp-003 = the 400).
- Live outcome: **no x_search frame in 2 turns (budget exhausted).** The
  grok-4.6 Vertex deployment (via the llm-proxy) REJECTS the x_search tool
  entry deterministically:
  `400 INVALID_ARGUMENT — "Expected the 'type' field of a(n) 'tools' array
  element to be 'function'; found 'x_search'."` (verbatim: resp-003.jsonl
  frame 0). This 400 shape is NOT in smoke/triage/signatures.json
  (unclassified — JIG §2 new-failure-class rule; reported to coordinator for
  catalog routing; lane is outside this driver's pathspec).
- Consequence: the live row cannot surface x_search today
  (`supports_backend_search=false` on the grok-4.6 catalog row is consistent
  with the deployment rejection). **Live-verification debt noted for the .78
  ship gate** (brief §5.1): re-mint when a deployment/row accepts the
  x_search entry, then re-pin these fixtures from the live frame.

## Donor source (DONOR-DERIVED fixtures)

open-grok @ **049664b5** (read-only reference; checkout HEAD = pinned commit):

| fixture | donor ref |
|---|---|
| `carrier_xs_123.json` | `crates/codegen/xai-grok-sampling-types/src/conversation.rs:14296-14302` (test `xai_x_search_history_replays_with_current_provider_native_type` — carrier shape `{call_id, input, name, id}` of the `rs::CustomToolCall` x_search carrier) |
| `wire_x_search_call_golden.json` | `conversation.rs:2168-2186` (`x_search_call_wire_value` — native input-side `x_search_call` wire value) + expected values asserted at `conversation.rs:14310-14314` (`type`/`id`/`status`/`action.query`) |
| `wire_placeholder_golden.json` | `conversation.rs:2161-2163` (`PROVIDER_NATIVE_SEARCH_REPLAY_SUMMARY`) + `conversation.rs:4436-4446` (flattener placeholder: role=assistant) |
| `wire_replay_red.json` | DERIVED (pre-GREEN hazard pin): the worktree's current projection of `carrier_xs_123` — `rs::Item::CustomToolCall` serialized through async-openai fork @rev 95b52eb (`InputItem` untagged, `Item` tagged `type`/snake_case) = `custom_tool_call` wire item with NO `custom` tool declared on any dialect |

## Redaction statement

No credential-shaped material in any file here. Raw-key sweep
(the campaign's credential-shaped pattern list, as applied by the mint
runner and the closeout sweep, plus a full ambient-key byte scan) = **0
hits on every file in this dir, 2026-09-18**. The mint used the ambient
CODEX_LLM_PROXY_KEY exclusively in process env (wiretap env handoff, never
argv, never echoed); no key material appears in the capture tree (runner
sweep = 0 on the report dir as well).
