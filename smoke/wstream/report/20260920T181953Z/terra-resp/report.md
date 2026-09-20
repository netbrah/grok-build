# wstream cell: terra-resp

- model: `gpt-5.6-terra`
- api_backend: `responses` (native)
- expected wire: `responses`
- open questions: PARITY-ARMS terra /responses arm vs recorded sol arm (same binary f9e7a15d6b6b)
- verdict: **PASS**
- binary: sha256_12 1463fe4cfb4e · wiretap :59292 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (12/12)
- side calls (display-only, e.g. session-title): 11 (non-200: none)
- reasoning (main turn): frames=101 tokens=71 encrypted=True thinking_block=False

## Invariants (must pass)
- [PASS] `status` = 200 (expected 200)
- [PASS] `route` = responses (expected responses)
- [PASS] `n_model_calls_min` = 12 (expected >= 1)

## Observations (OQ capture)
- `route` = "responses"
- `status` = 200
- `n_model_calls` = 12
- `all_stream` = true
- `any_stream` = true
- `req_stream_values` = [true, true, true, true, true, true, true, true, true, true, true, true]
- `reasoning_present` = true
- `reasoning_frames` = 101
- `reasoning_encrypted_content` = true
- `reasoning_thinking_block` = false
- `reasoning_thought_signature` = false
- `reasoning_sample` = "**Organizing feature development**\n\nI"
- `finish` = null
- `budget_trap` = false
- `final_text_nonempty` = false
- `final_text_len` = 0
- `usage` = {"input_tokens": 16131, "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 16128}, "output_tokens": 367, "output_tokens_details": {"reasoning_tokens": 71}, "total_tokens": 16498, "cost": 0.04473}

## Per-call detail
- call 1: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=22 reasoning_frames=2 finish=None
- call 2: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=128 reasoning_frames=101 finish=None
- call 3: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=385 reasoning_frames=314 finish=None
- call 4: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=15 reasoning_frames=2 finish=None
- call 5: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=254 reasoning_frames=69 finish=None
- call 6: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=48 reasoning_frames=2 finish=None
- call 7: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=24 reasoning_frames=4 finish=None
- call 8: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=186 reasoning_frames=89 finish=None
- call 9: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=45 reasoning_frames=2 finish=None
- call 10: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=105 reasoning_frames=2 finish=None
- call 11: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=14 reasoning_frames=2 finish=None
- call 12: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=103 reasoning_frames=2 finish=None

## Reasoning sample (truncated)
```
**Organizing feature development**

I
```
