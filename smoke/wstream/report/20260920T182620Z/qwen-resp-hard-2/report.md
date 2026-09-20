# wstream cell: qwen-resp-hard-2

- model: `qwen3.8-27b`
- api_backend: `responses` (native)
- expected wire: `responses`
- open questions: PARITY-ARMS qwen /responses second cell — REPEAT of hard question (no second question exists in the v2 set)
- verdict: **PASS**
- binary: sha256_12 1463fe4cfb4e · wiretap :60258 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (17/17)
- side calls (display-only, e.g. session-title): 16 (non-200: none)
- reasoning (main turn): frames=7531 tokens=19910 encrypted=False thinking_block=False

## Invariants (must pass)
- [PASS] `status` = 200 (expected 200)
- [PASS] `route` = responses (expected responses)
- [PASS] `n_model_calls_min` = 17 (expected >= 1)

## Observations (OQ capture)
- `route` = "responses"
- `status` = 200
- `n_model_calls` = 17
- `all_stream` = true
- `any_stream` = true
- `req_stream_values` = [true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true]
- `reasoning_present` = true
- `reasoning_frames` = 7531
- `reasoning_encrypted_content` = false
- `reasoning_thinking_block` = false
- `reasoning_thought_signature` = false
- `reasoning_sample` = "Let"
- `finish` = null
- `budget_trap` = false
- `final_text_nonempty` = true
- `final_text_len` = 83
- `usage` = {"input_tokens": 20572, "input_tokens_details": {"cached_tokens": 64, "cache_write_tokens": 0}, "output_tokens": 20000, "output_tokens_details": {"reasoning_tokens": 19910}, "total_tokens": 40572}

## Per-call detail
- call 1: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=22 reasoning_frames=8 finish=None
- call 2: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=7557 reasoning_frames=7531 finish=None
- call 3: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=46 reasoning_frames=7 finish=None
- call 4: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=48 reasoning_frames=14 finish=None
- call 5: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=57 reasoning_frames=19 finish=None
- call 6: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=42 reasoning_frames=21 finish=None
- call 7: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=55 reasoning_frames=23 finish=None
- call 8: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=72 reasoning_frames=44 finish=None
- call 9: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=29 reasoning_frames=6 finish=None
- call 10: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=120 reasoning_frames=87 finish=None
- call 11: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=77 reasoning_frames=47 finish=None
- call 12: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=605 reasoning_frames=551 finish=None
- call 13: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=123 reasoning_frames=99 finish=None
- call 14: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=531 reasoning_frames=481 finish=None
- call 15: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=62 reasoning_frames=41 finish=None
- call 16: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=189 reasoning_frames=152 finish=None
- call 17: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=316 reasoning_frames=100 finish=None

## Reasoning sample (truncated)
```
Let
```
