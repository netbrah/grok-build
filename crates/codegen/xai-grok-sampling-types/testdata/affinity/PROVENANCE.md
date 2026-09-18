# Fixture provenance — affinity/ (apex-ayl.75)

Owner bead: **apex-ayl.75** (AFFINITY-POLICY-1, R8, P1 RANK 1). Driver: sdd75_driver_qwen.

House fixture ruling (coordinator, 2026-09-18 00:34Z, provenance/ledger.md): "frame-scale
fixtures -> crate test tree; … Fixtures live where the test runs. PROVENANCE.md per fixture
dir (mint source + sweep-0 + bead owner)." These fixtures are consumed by
`crates/codegen/xai-grok-sampling-types/src/error.rs` `#[cfg(test)]` (include_str!).

## probe-d-401-body.json (184 B, byte-identical copy)

- Mint source: incident probe-D 401 body — the gateway tags-config fail-fast, first
  observed 2026-09-17 in the ws8 probe-2 D round (foreign-origin marker + EU2-tagged
  request). Verbatim: `{"error":{"message":"Not allowed to access model due to tags
  configuration. Passed model=gpt-5.6-sol and tags=['East US 2']","type":
  "internal_server_error","param":null,"code":"401"}}`
- Protected copy: `grok/plans/provenance/fixtures/ws8/probe-d-401-body.json` (sweep-0 at
  protection, coordinator 2026-09-18 00:34Z); raw `/tmp/D-r1.json` (byte-identical,
  `cmp`-verified 2026-09-18 by the driver).
- Evidence docs: ws8 probe matrix §2 L33-35 + §3 L44 (`provenance/proxy-capability-matrix.md`);
  companion log `provenance/fixtures/ws8/ws8-probe2.log` ([D-R1] http=401 t=1s RAW: <this body>).
- This file is a byte-identical `cp` of the protected fixture (verified with `cmp`); no
  transformation, no redaction — the body carries no credentials.

## probe-c-same-boundary-keep.json (probe-C shape pin, cut b)

- Mint source: ws8 probe-2 C round (2026-09-17 07:5xZ window, ET 04:0xZ): untagged request
  replaying the prior round's output markers INTACT (id `encitem_<b64(litellm:model_id:<sha>;item_id:rs_..)>`
  + field `litellm_enc:<b64 metadata>;<ciphertext>`), non-streaming, `store=false`,
  `max_output_tokens=256`, `reasoning.effort=high`, math prompts. Result: 3/3 200, serving
  boundary 7ff02bbaab48 (Sweden Central) ×3 — PIN held. Matrix cite:
  `provenance/proxy-capability-matrix.md` §3 L43 (C row) + §4 reading 4 (L56-58).
- Request shape reconstructed from the raw evidence: the probe-2 script captured responses
  only (`/tmp/C-r1.json` = R1 response; `/tmp/C-r2.sse`, `/tmp/C-r3.sse` = R2/R3 SSE
  responses); the continuation request bodies were built in-memory by the probe. The
  `request_shape.body.input` items mirror the replayed R1 output items (real ids; summary
  text and answer text REDACTED) plus the new user round prompt.
- Redaction policy: the id marker and the `litellm_enc:` metadata b64 are retained verbatim
  (routing metadata: deployment model_id sha + item id — already public in the frozen
  evidence and ledger cites); the 2744-char deployment-bound ciphertext is REDACTED to its
  first 8 chars + marker (it encrypts model reasoning content; full bytes only in the
  frozen ws8 evidence `/tmp/C-r1.json` + `provenance/fixtures/ws8/`).
- Ruling pinned by this fixture: same-boundary continuation default = KEEP id + field
  markers (OQ-1 closed for the id+field variant by probe C). Field-only variant NOT
  approved — OQ-1b owed (matrix §5 L79); explicitly NOT built in this cut.

## Sweep-0 statement (raw-key sweep)

Driver raw-key sweep over both fixtures + this PROVENANCE.md, 2026-09-18, patterns per
house convention (`sk-[A0-9a-z]{16,}`, `sk-ant-`, `ghp_`/`gho_`, `AKIA[0-9A-Z]{16}`,
`Bearer ` literals, `CODEX_LLM_PROXY_KEY=`, key-shaped blobs): **0 hits**. No Authorization
header, no API key, no credential material in any file in this directory. The `encitem_` /
`litellm_enc:` values are deployment-bound routing/ciphertext markers, not credentials (W10-B
§8 precedent: thoughtSignature blobs treated the same way).

## Base

Minted on worktree base `4fb487f` (feat/first-class-responses-catalog), bead-declared base
`dec6b27` verified ancestor (6 commits between). Uncommitted; coordinator exfils after
adjudication.
