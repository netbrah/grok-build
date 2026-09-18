# testdata/xw_orphan — fixture provenance (apex-ayl.74, XW-ORPHAN-1)

Minted 2026-09-18 by the .74 RED-only driver (qwen seat) at worktree base
`40ffad10955f337534c75363cd7654bfbadd9891` (feat/first-class-responses-catalog).
Bead owner: `apex-ayl.74`. Loader convention: `include_str!` from
`xai-grok-sampling-types` test mods (`.75` `testdata/affinity` pattern).

## vllm_pydantic_400_body.json

- File sha256:12 = `6dfa78de9f48`
- Inner message (the envelope's `error.message` value) = the VERBATIM class-(a)
  fragment quoted in `grok/plans/crosswire-preplan-glm-20260917.md` §VI.3 and
  `xwire/tdd-74-orphan.md` §1 — 4 lines, 237 chars, **sha256:12 = `1f591ef070d9`**
  (recomputed first-hand at dispatch; reproduces the plan-pinned value exactly).
- PROVENANCE: **synthetic recipe minted from the verbatim quoted fragment**
  (JIG §2 step 1). The raw HTTP body belongs to the codex-combined seat's
  capture (glm-5.2 group, litellm Responses→ChatCompletions shim, LIVE-observed
  2026-09-17; incident 01a0b07a, ledger L1801), not this repo. The recipe wraps
  the verbatim fragment in the structured envelope form the base pins already
  establish (`error.rs` live-wire pin, body shape `{"error":{"message":...,"type":null,"param":null,"code":"400"}}`);
  `"type": null` keeps the user-facing pipeline on the raw-message path
  (`try_parse_error` error_type = "unknown"), so the classifier sees the
  fragment itself (237 chars < `MAX_USER_ERROR_BODY_CHARS` 280 — no cap applied).
- Status: **LIVE** (observed; fragment quoted with its tail truncated in the
  source capture — the `...` at the fragment's end is part of the canonical
  quoted text and of the pinned sha).

## azure_callid_orphan_400_body.json

- File sha256:12 = `aedcf4d3bbd4`
- Inner message (the envelope's `error.message` value) =
  `Invalid 'input[7].call_id': the function_call_output references a call id that is not present in the input.`
  (107 chars; sha256:12 = `ce66106797dc`).
- PROVENANCE: **PREDICTION** (explicit; no live capture) — qwen preplan H-1
  predicted form `Invalid 'input[N].call_id': …` anchored on the incident-400
  style of the base live-wire pin (`error.rs` bracketed Azure body, inner
  message sha256:12 `c1651b7d3aff`). The H-1 ellipsis completion
  ("the function_call_output references a call id that is not present in the
  input.") is this mint's synthetic recipe; it is needle-checked: contains
  `input[` + `.call_id` + `invalid` (the post-fix arm-A triple) and NONE of the
  base family needles (`.id`, `item`, `not found`/`does not exist`,
  `array too long`/`array_above_max_length`, `encrypted*`, `thinking`,
  `signature`).
- Status: **PREDICTION** — latent class, structurally unreachable today
  (tdd-74 §1), reachable by any future provenance partial strip.

## Hygiene

- Raw-key sweep (review-gates-q2.md §6 pattern) = **0** on both fixtures and
  this file (verified at RED dispatch).
- No raw wire bodies beyond the published class-(a) fragment quote (sha
  `1f591ef070d9`, already published in the plan docs and the codex-combined
  seat's preplan).
- Evidence fragments: sha256:12 only; full shard shas above.
