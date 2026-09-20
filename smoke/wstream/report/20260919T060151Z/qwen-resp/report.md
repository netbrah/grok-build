# wstream cell: qwen-resp

- model: `qwen3.8-27b`
- api_backend: `responses` (native)
- expected wire: `responses`
- open questions: OQ-3
- verdict: **PASS**
- binary: sha256_12 f9e7a15d6b6b · wiretap :52387 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (2/2)
- side calls (display-only, e.g. session-title): 1 (non-200: none)
- reasoning (main turn): frames=1636 tokens=None encrypted=False thinking_block=False

## Invariants (must pass)
- [PASS] `status` = 200 (expected 200)
- [PASS] `route` = responses (expected responses)
- [PASS] `n_model_calls_min` = 2 (expected >= 1)

## Observations (OQ capture)
- `route` = "responses"
- `status` = 200
- `n_model_calls` = 2
- `all_stream` = true
- `any_stream` = true
- `req_stream_values` = [true, true]
- `reasoning_present` = true
- `reasoning_frames` = 1636
- `reasoning_encrypted_content` = false
- `reasoning_thinking_block` = false
- `reasoning_thought_signature` = false
- `reasoning_sample` = "The"
- `finish` = null
- `budget_trap` = false
- `final_text_nonempty` = false
- `final_text_len` = 0
- `usage` = null

## Per-call detail
- call 1: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=39 reasoning_frames=31 finish=max_output_tokens
- call 2: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=1640 reasoning_frames=1636 finish=None

## Reasoning sample (truncated)
```
The
```
