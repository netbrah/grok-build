# wstream cell: qwen-resp-hard-2

- model: `qwen3.8-27b`
- api_backend: `responses` (native)
- expected wire: `responses`
- open questions: PARITY-ARMS qwen /responses second cell — REPEAT of hard question (no second question exists in the v2 set)
- verdict: **PASS**
- binary: sha256_12 f9e7a15d6b6b · wiretap :63998 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (15/15)
- side calls (display-only, e.g. session-title): 14 (non-200: none)
- reasoning (main turn): frames=12968 tokens=33280 encrypted=False thinking_block=False

## Invariants (must pass)
- [PASS] `status` = 200 (expected 200)
- [PASS] `route` = responses (expected responses)
- [PASS] `n_model_calls_min` = 15 (expected >= 1)

## Observations (OQ capture)
- `route` = "responses"
- `status` = 200
- `n_model_calls` = 15
- `all_stream` = true
- `any_stream` = true
- `req_stream_values` = [true, true, true, true, true, true, true, true, true, true, true, true, true, true, true]
- `reasoning_present` = true
- `reasoning_frames` = 12968
- `reasoning_encrypted_content` = false
- `reasoning_thinking_block` = false
- `reasoning_thought_signature` = false
- `reasoning_sample` = "Let"
- `finish` = null
- `budget_trap` = false
- `final_text_nonempty` = false
- `final_text_len` = 2
- `usage` = {"input_tokens": 20544, "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0}, "output_tokens": 33400, "output_tokens_details": {"reasoning_tokens": 33280}, "total_tokens": 53944}

## Per-call detail
- call 1: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=40 reasoning_frames=32 finish=max_output_tokens
- call 2: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=12995 reasoning_frames=12968 finish=None
- call 3: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=34 reasoning_frames=8 finish=None
- call 4: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=48 reasoning_frames=20 finish=None
- call 5: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=39 reasoning_frames=18 finish=None
- call 6: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=80 reasoning_frames=48 finish=None
- call 7: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=97 reasoning_frames=61 finish=None
- call 8: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=50 reasoning_frames=24 finish=None
- call 9: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=245 reasoning_frames=201 finish=None
- call 10: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=297 reasoning_frames=265 finish=None
- call 11: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=169 reasoning_frames=130 finish=None
- call 12: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=927 reasoning_frames=887 finish=None
- call 13: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=110 reasoning_frames=84 finish=None
- call 14: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=35 reasoning_frames=14 finish=None
- call 15: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=168 reasoning_frames=13 finish=None

## Reasoning sample (truncated)
```
Let
```
