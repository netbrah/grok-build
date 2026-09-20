# wstream cell: opus-msg

- model: `claude-opus-5`
- api_backend: `messages` (native)
- expected wire: `messages`
- open questions: OQ-2
- verdict: **PASS**
- binary: sha256_12 f9e7a15d6b6b · wiretap :52023 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (2/2)
- side calls (display-only, e.g. session-title): 1 (non-200: none)
- reasoning (main turn): frames=198 tokens=4292 encrypted=False thinking_block=True

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
- `reasoning_frames` = 198
- `reasoning_encrypted_content` = false
- `reasoning_thinking_block` = true
- `reasoning_thought_signature` = false
- `reasoning_sample` = "I"
- `finish` = "stop"
- `budget_trap` = false
- `final_text_nonempty` = true
- `final_text_len` = 1352
- `usage` = {"input_tokens": 2, "cache_creation_input_tokens": 28237, "cache_read_input_tokens": 0, "output_tokens": 5163, "output_tokens_details": {"thinking_tokens": 4292}}

## Per-call detail
- call 1: route=`messages` model=`claude-opus-5` stream=True status=200 frames=19 reasoning_frames=0 finish=stop
- call 2: route=`messages` model=`claude-opus-5` stream=True status=200 frames=336 reasoning_frames=198 finish=stop

## Reasoning sample (truncated)
```
I
```
