# wstream cell: ws9-s12-alias-claude-msg

- model: `claude-sonnet-5`
- api_backend: `messages` (native)
- expected wire: `messages`
- open questions: WS9-S12 control baseline (JIG L3 stream-fidelity per wire): thinking-block visibility IN STREAM on the anthropic-family control seat used by the S06/S12 rows — the existing 10 cells cover claude-opus-5 (opus-msg) but NOT claude-sonnet-5; the alias pair claude-sonnet-4-5/claude-sonnet-4.5 (S12) sits in this family, so the sonnet-5 streaming baseline is the family reference for the same-boundary affinity question.
- verdict: **PASS**
- binary: sha256_12 e635d5d7f2fe · wiretap :61188 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (2/2)
- side calls (display-only, e.g. session-title): 1 (non-200: [500])
- reasoning (main turn): frames=606 tokens=10735 encrypted=False thinking_block=True

## Invariants (must pass)
- [PASS] `status` = 200 (expected 200)
- [PASS] `route` = messages (expected messages)
- [PASS] `n_model_calls_min` = 2 (expected >= 1)

## Observations (OQ capture)
- `route` = "messages"
- `status` = 200
- `n_model_calls` = 2
- `all_stream` = true
- `any_stream` = true
- `req_stream_values` = [true, true]
- `reasoning_present` = true
- `reasoning_frames` = 606
- `reasoning_encrypted_content` = false
- `reasoning_thinking_block` = true
- `reasoning_thought_signature` = false
- `reasoning_sample` = "Since I can"
- `finish` = "stop"
- `budget_trap` = false
- `final_text_nonempty` = true
- `final_text_len` = 1315
- `usage` = {"input_tokens": 2, "cache_creation_input_tokens": 29639, "cache_read_input_tokens": 0, "output_tokens": 11511, "output_tokens_details": {"thinking_tokens": 10735}}

## Per-call detail
- call 1: route=`responses` model=`grok-4.6` stream=True status=500 frames=1 reasoning_frames=0 finish=None
- call 2: route=`messages` model=`claude-sonnet-5` stream=True status=200 frames=741 reasoning_frames=606 finish=stop

## Reasoning sample (truncated)
```
Since I can
```
