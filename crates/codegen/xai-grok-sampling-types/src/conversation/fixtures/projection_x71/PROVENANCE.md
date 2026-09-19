# Fixtures — projection_x71 (apex-ayl.71 WAVE-C unit REDs, sdd-71 §8 Table 2)

Bead owner: **apex-ayl.71** · Driver: xw_wavec71_red (overwatch/coordinator lane) · Date: 2026-09-18 (NY)
Ruling: fixtures live in the crate test tree where the test runs (`include_str!`) —
sdd-71 §8 Table 2 preamble + §9 step 4; in-tree precedent
`conversation/fixtures/xsearch_replay/` (apex-ayl.76) + `testdata/affinity/` (apex-ayl.75).

## Mint source

Byte mirrors of the live `smoke/xwfix` corpus cells, copied first-hand from the
multi-session worktree `/Users/palanisd/Projects/upstream/wt/grok-build-responses`
(dirty with sibling cuts; the .70 lane's re-pinned corpus state is the mint
source — these mirrors are the .71 lane's read-only fixture set).

| fixture | corpus source | source sha256:12 | mirrored sha256:12 | byte-verified |
|---|---|---|---|---|
| `vxm_az_pre.json` | `smoke/xwfix/cells/vxm-az/pre_switch.json` | `7919f5a536b3` | `7919f5a536b3` | 2026-09-18 |
| `vxm_az_expected.json` | `smoke/xwfix/cells/vxm-az/expected.json` | `d5f6de89e379` | `d5f6de89e379` | 2026-09-18 |
| `az_vlq_pre.json` | `smoke/xwfix/cells/az-vlq/pre_switch.json` | `1bb347e7b27c` | `1bb347e7b27c` | 2026-09-18 |
| `az_vlq_expected.json` | `smoke/xwfix/cells/az-vlq/expected.json` | `8b71740159c9` | `8b71740159c9` | 2026-09-18 |

Corpus cell.json shas at mint (live, first-hand): vxm-az `b6ce13571041` (HOLD vs
sdd-71 §12 verification notes); az-vlq `c4b7e5c8a3c5` (live .70-lane state).

### Corpus composition (first-hand count at mint)

- **vxm-az** (VX-M sonnet → AZ-strict terra, PREDICTED-CLEAN): 36 pre-switch
  records (system 1, user 12, reasoning 5, assistant 8, tool_result 10); the 5
  reasoning items carry `id:""` + encrypted_content + summary (VX-M persist gap,
  `stream/messages.rs:543-549`). expected = same 36 records: 5 reasoning T1
  re-keyed (`xw_` ids, encrypted stripped, summary+content kept), 31 T0
  byte-identical.
- **az-vlq** (AZ sol → VL-qwen, PROVEN-REACTIVE — the .58 incident's
  double-keyed trigger set): **53** pre-switch records (system 1, user 17,
  reasoning 7, assistant 5, tool_result 23); all 7 reasoning items carry
  `encitem_` ids + encrypted_content (4 of 7 are empty-summary shells).
  expected = same 53 records: 7 reasoning T1 re-keyed, 46 T0 byte-identical.

  **DRIFT NOTE (recorded, not re-pinned by this lane):** sdd-71 §8 Table 1 and
  the WAVE-C tasking brief both describe az-vlq as "re-pin 49 recs: 3 T1 + 46
  T0". The LIVE corpus at mint is **53 recs: 7 T1 + 46 T0** (both cell.json and
  the expected mirror agree; the 12/12 ID-grammar count — vxm-az 5/5 + az-vlq
  7/7 — reproduces byte-for-byte against this live state, so the live state is
  the governing one). The 49/3 figure is a stale earlier re-pin proposal
  (empty-summary shell sub-class, matrix H-3); if the .70 lane re-pins the cell
  to the 49-record shape before W2, STOP per sdd-71 §11 stop 3 and re-derive
  the goldens from the live cell. NEVER edit a mirror to fit a RED.

### `orphan_shape.json` (synthetic — Table 2 case 1)

Minimal synthetic storage-form shape `[user, BackendToolCall(fc_x),
ToolResult(fc_x), assistant]` (4 records). NOT a corpus mirror. The
`backend_tool_call` record is an `rs::CustomToolCall` x_search carrier in
storage form (`{"type":"backend_tool_call","kind":{"tool_type":"x_search",...}}`
— `BackendToolCallItem { kind }` @0fc1060 `conversation.rs:267-270`, `kind`
required, no default; `BackendToolKind` internally tagged `tool_type`/snake_case
@40ffad1 `conversation.rs:511-524`; payload shape per the in-tree precedent
`conversation/fixtures/xsearch_replay/carrier_xs_123.json`, which is the
`rs::CustomToolCall` PAYLOAD (`{call_id,input,name,id}`), not a full record).
`fc_x` is a synthetic id — no wire material. The case-1 target tier (vertex)
is where the call drops; the pairing assertion is on the output.

**Shape-conformance note (2026-09-18, GREEN lane, coordinator ruling):** the
record as minted was flat (`tool_type` + payload fields at the record top
level), which does not conform to the serde schema — the four Table-2 tests
that parse this fixture and `carrier_shape()` (`orphaned_result_direction`,
`carrier_survival`, `idempotence`, `no_empty_id_no_foreign_encrypted`)
panicked in `items_from` with `missing field 'kind'` (wrong-reason failure;
they never reached their documented invariant assertions). Fix = input-data
conformance: the payload is nested under `kind` here and in `carrier_shape()`
(`projection_tests.rs`, same ruling). Zero assertion edits, zero pin edits;
the semantic shape (fc_x pairing, carrier raw payload,
cross_provider_fallback) is unchanged. The four corpus mirrors are untouched.

## Known false-positive raw-key classes (recorded, NOT redacted — fixture text,
not credentials)

Campaign sweep (review-gates-q4 §6:
`grep -cE '(sk-[A-Za-z0-9]{8,}|x-litellm-tag\:|api_key\s*=|Bearer\ )'` —
quoted in the escaped self-safe form so this file sweeps 0 against its own
quoted pattern):

1. **Bearer-scheme-word prose (vxm_az_pre.json + vxm_az_expected.json, 1 hit
   each):** record index 26 (a T0 `tool_result`, tool_call_id
   `toolu_vrtx_01EcrBpspP8tK56zbVFN5Fkv`) quotes an authentication doc table
   row from the session's read_file output — the `access_token` schema
   description column, whose text starts with the bearer-scheme word followed
   by a space (the campaign pattern's 4th alternative matches that word+space
   in prose). Documentation prose captured in a real-session tool result —
   no token material; the `access_token` row is a schema description, not a
   value. (The verbatim row is reproducible from the corpus source at
   `smoke/xwfix/cells/vxm-az/pre_switch.json` record 26; deliberately not
   quoted here so this file sweeps 0.)
2. **`sk-` path-substring class (az_vlq_pre.json + az_vlq_expected.json):**
   record index 3 (a T0 `user` record) lists task file paths
   `.../superpowers/sdd/code-intelligence-concordance/task-4-codegraph-contract-research.md`
   and `task-5-cbm-contract-research.md`. The campaign pattern does NOT match
   these (`sk-[A-Za-z0-9]{8,}` requires 8+ alphanumerics after `sk-`; the
   match would be inside `ta**sk-4**-...` and breaks at the hyphen). Fixture
   file paths, not keys.

Sweep result at mint: **0** on the synthetic + the az-vlq pair; **1** on each
vxm-az mirror, solely class 1 above. No credential-shaped material in any file
here (CODEX_LLM_PROXY_KEY never appears; no `x-litellm-tag`, no `api_key`
assignment). Per the coordinator ruling 2026-09-18, the known
path-substring/prose class is recorded here and not redacted; the md/patch
deliverables of the WAVE-C cut sweep 0.

## Discipline

- These mirrors are RED inputs + parsed-equality oracles. Redcycle STOP rule 1
  binds: a RED that does not fail as documented is a defect in the
  cell/op → STOP. NEVER edit `*_expected.json` (or any pin) to fit a RED.
- The 12/12 known-answer IDs (vxm-az 5/5 + az-vlq 7/7) are reproduced
  byte-for-byte from the live cells via the sdd-71 §5 grammar
  (`"xw_" + sha256("{cell}|{ord}|{json.dumps(content,sort_keys=True)}|{json.dumps(summary,sort_keys=True)}").hexdigest()[:24]`,
  Python-default canonicalization) — verified first-hand 2026-09-18; the table
  lives in `projection_tests.rs` (`xw_proj_id_grammar_canonical`).
