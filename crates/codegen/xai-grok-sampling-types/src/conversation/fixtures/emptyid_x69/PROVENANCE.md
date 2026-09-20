# Fixtures — emptyid_x69 (apex-ayl.69 XW-EMPTYID-1, tdd-69 §3)

Bead owner: **apex-ayl.69** · Driver: driver_69_emptyid_qwen (SDD lane) · Date: 2026-09-19 (NY)
Ruling: fixtures live in the crate test tree where the test runs (`include_str!`) —
fixture-home ruling (ledger 2026-09-18 00:34Z); in-tree precedents
`conversation/fixtures/projection_x71/` (apex-ayl.71) + `xsearch_replay/` (apex-ayl.76).

## Mint source

`pre_switch_emptyid.json` (mint sha256:12 `4a16ad6ea696`) — the incident 01a0b046
recipe minted from `smoke/xwfix/cells/vxm-az/pre_switch.json` (live sha256:12
`7919f5a536b3` at mint, first-hand; matches sdd-69 §1 cite) per sdd-69 §3:

- **5 reasoning records VERBATIM** (sorted-key byte-identical to the cell,
  verified at mint): `id:""` + `encrypted_content` + `summary` (the VX-M persist
  gap, `sampler/src/stream/messages.rs`). These are the RED input shape and the
  12/12 known-answer id set (`xw_bcf9e9828796d9d08c350f8a`,
  `xw_f95ed93e0a9badbaccd12afa`, `xw_7157485b1997e6654dea85d5`,
  `xw_55ab10bbffec861ca557f6e6`, `xw_46a650fad66e55e6928d41b2` — cell name
  `vxm-az`, ords 0..4).
- **Minimal portable context** (synthetic, not incident content): 1 system,
  1 user, 1 assistant (one tool call `call_1`), 1 tool_result (paired to
  `call_1`) — so the request builder sees a well-formed conversation. Reasoning
  siblings sit before the assistant turn, matching the storage shape.

Incident provenance (from the cell record): session
`01a0b046-50ad-7070-925d-738c2b154db3` (sonnet VX-M -> gpt-5.6-terra AZ-strict,
2026-09-16); first post-switch request 400 `Invalid input[N].id ''` ->
classifier family-3 -> `drop_model_bound_items` all-or-nothing strip -> retry 200.

Raw-key sweep: 0 (verified at mint: the sweep pattern from review-gates-q2 §6).
Evidence discipline: incident content is the already-pinned xwfix corpus shape
(sha256:12 only in reports; no raw wire bodies).
