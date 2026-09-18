# ingress77 fixture provenance (bead apex-ayl.77 INGRESS-NORMALIZE-1)

- Mint source: **hand-authored from donor test shapes @ open-grok 049664b5**
  (read-only donor checkout; checkout HEAD == pin, verified at driver STEP 0).
  No live traffic, no capture replay — JIG §2 "synthetic recipe" provenance
  class, sweep-0.
- Donor shape cites (open-grok 049664b5, `crates/codegen/xai-grok-sampler/src/client.rs`):
  - `actionless_web_search_call_done.json` ← :6435-6456
    (`codex_actionless_web_search_item_done_parses_with_sentinel_action`)
  - `native_x_search_call_added.json` ← :6701-6728
    (`deserialize_response_event_normalizes_current_x_search_call_shape`) —
    the current-loose xAI shape (empty `name` + `arguments` + empty `call_id`);
    deliberately the loosest real shape so every donor fallback fires.
  - `idless_custom_tool_call_added.json` ← :6672-6703
    (`deserialize_response_event_fills_custom_tool_call_id_on_output_item_added`)
- Owner: apex-ayl.77 · driver: sdd77_driver_qwen · minted: 2026-09-18 (Phase 0).
- Raw-key sweep: 0 (synthetic ids `ws_123` / `xs_123` / `call_custom_1`; no
  credentials anywhere in the frames).
- Byte-pins: `MANIFEST.sha256` (shasum -a 256, paths relative to this dir).
