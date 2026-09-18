# XW-FIXTURES — cross-wire golden corpus (apex-ayl.70)

Golden fixtures for cross-wire model-switch projection. Each cell pins the
pre-switch persisted history + the expected post-switch projection, so
XW-PROJECT-1 (apex-ayl.71) is TDD RED-first per cell and XW-MATRIX-1
(apex-ayl.72) has live sweep assertions.

## Fidelity ladder (per target wire, applied at switch time)
- T0 native replay — item unchanged (same model+wire+encryption boundary).
- T1 re-keyed — foreign reasoning: synthesized valid id + encrypted_content
  stripped + summary kept (target tolerates summary-text reasoning).
- T2 lossy — reasoning/thinking -> plain text (or nothing); signatures dropped.
- T3 drop — item removed, portable transcript intact (the .58 reactive net's
  all-or-nothing behavior, demoted from default to floor).

## Invariants (every cell, every tier)
1. No record/item with id == "".
2. No foreign encrypted_content survives into the target request.
3. Pairing integrity: every tool_result has a matching tool_call in the
   projected history (a T3 drop of a call must re-project its result to T2
   or drop both).
4. Carrier survival: compaction carrier items + raw_codex_input_replacements
   survive projection opaquely.
5. Non-projected records are byte-identical to pre_switch.

## Id synthesis (T1)
id = "xw_" + sha256("{cell}|{reasoning_index}|{json(content,sorted)}|{json(summary,sorted)}").hexdigest()[:24]
Deterministic per cell; never depends on wall clock.

## Forms
- Storage form (now): grok conversation records (chat_history.jsonl schema),
  one JSON array per file.
- Wire form (later): the exact /v1/responses `input` array; byte pins land
  when the switch_model case op captures the first post-switch request
  (smoke/xwfix/switch_model_op.patch, DRAFT — the .22 lane owns run.py merge).

## Cell record schema (cells/<cell>/cell.json)
cell, flip{from,to: family/model/wire}, status (PROVEN-REACTIVE | PORTED |
UNPROVEN | PREDICTED | synthetic), incident|recipe (provenance: session dir /
code file:line / "synthetic recipe"), pre_switch{file,records,composition},
expected{file,records,projection map}, invariants[], red_tests[].

## Evidence discipline
- Real incident cells cite the session dir + source file; captures quoted
  as sha256:12.
- Raw-key sweep = 0 on every file in this tree (CODEX_LLM_PROXY_KEY — never echo).
- store=false on any live capture.
- Worktree is multi-session: only touch smoke/xwfix/.
