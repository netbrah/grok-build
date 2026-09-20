# wstream cell: qwen-resp-hard-1

- model: `qwen3.8-27b`
- api_backend: `responses` (native)
- expected wire: `responses`
- open questions: PARITY-ARMS qwen /responses hard-question arm (on-prem, cost 0)
- verdict: **PASS**
- binary: sha256_12 f9e7a15d6b6b · wiretap :60163 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (29/29)
- side calls (display-only, e.g. session-title): 28 (non-200: none)
- reasoning (main turn): frames=7890 tokens=20496 encrypted=False thinking_block=False

## Invariants (must pass)
- [PASS] `status` = 200 (expected 200)
- [PASS] `route` = responses (expected responses)
- [PASS] `n_model_calls_min` = 29 (expected >= 1)

## Observations (OQ capture)
- `route` = "responses"
- `status` = 200
- `n_model_calls` = 29
- `all_stream` = true
- `any_stream` = true
- `req_stream_values` = [true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true]
- `reasoning_present` = true
- `reasoning_frames` = 7890
- `reasoning_encrypted_content` = false
- `reasoning_thinking_block` = false
- `reasoning_thought_signature` = false
- `reasoning_sample` = "Let"
- `finish` = null
- `budget_trap` = false
- `final_text_nonempty` = true
- `final_text_len` = 81
- `usage` = {"input_tokens": 20603, "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0}, "output_tokens": 20586, "output_tokens_details": {"reasoning_tokens": 20496}, "total_tokens": 41189}

## Per-call detail
- call 1: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=39 reasoning_frames=31 finish=max_output_tokens
- call 2: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=7916 reasoning_frames=7890 finish=None
- call 3: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=60 reasoning_frames=8 finish=None
- call 4: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=41 reasoning_frames=16 finish=None
- call 5: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=79 reasoning_frames=41 finish=None
- call 6: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=165 reasoning_frames=125 finish=None
- call 7: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=104 reasoning_frames=68 finish=None
- call 8: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=231 reasoning_frames=186 finish=None
- call 9: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=1001 reasoning_frames=963 finish=None
- call 10: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=4396 reasoning_frames=4316 finish=None
- call 11: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=53 reasoning_frames=23 finish=None
- call 12: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=54 reasoning_frames=24 finish=None
- call 13: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=11432 reasoning_frames=11357 finish=None
- call 14: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=103 reasoning_frames=70 finish=None
- call 15: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=251 reasoning_frames=215 finish=None
- call 16: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=77 reasoning_frames=46 finish=None
- call 17: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=597 reasoning_frames=558 finish=None
- call 18: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=277 reasoning_frames=237 finish=None
- call 19: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=205 reasoning_frames=159 finish=None
- call 20: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=8261 reasoning_frames=8179 finish=None
- call 21: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=38 reasoning_frames=11 finish=None
- call 22: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=381 reasoning_frames=326 finish=None
- call 23: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=2118 reasoning_frames=2058 finish=None
- call 24: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=57 reasoning_frames=36 finish=None
- call 25: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=40 reasoning_frames=13 finish=None
- call 26: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=54 reasoning_frames=30 finish=None
- call 27: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=31 reasoning_frames=10 finish=None
- call 28: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=96 reasoning_frames=59 finish=None
- call 29: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=283 reasoning_frames=76 finish=None

## Reasoning sample (truncated)
```
Let
```
