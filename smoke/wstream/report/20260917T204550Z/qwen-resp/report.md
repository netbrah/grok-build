# wstream cell: qwen-resp

- model: `qwen3.8-27b`
- api_backend: `responses` (native)
- expected wire: `responses`
- open questions: OQ-3
- verdict: **PASS**
- binary: sha256_12 39f836e8633d · wiretap :55141 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (2/2)
- side calls (display-only, e.g. session-title): 1 (non-200: [500])
- reasoning (main turn): frames=613 tokens=1683 encrypted=False thinking_block=False

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
- `reasoning_frames` = 613
- `reasoning_encrypted_content` = false
- `reasoning_thinking_block` = false
- `reasoning_thought_signature` = false
- `reasoning_sample` = "I"
- `finish` = null
- `budget_trap` = false
- `final_text_nonempty` = true
- `final_text_len` = 834
- `usage` = {"input_tokens": 20381, "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0}, "output_tokens": 2246, "output_tokens_details": {"reasoning_tokens": 1683}, "total_tokens": 22627}

## Per-call detail
- call 1: route=`responses` model=`grok-4.6` stream=True status=500 frames=1 reasoning_frames=0 finish=None
- call 2: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=800 reasoning_frames=613 finish=None

## Reasoning sample (truncated)
```
I
```
