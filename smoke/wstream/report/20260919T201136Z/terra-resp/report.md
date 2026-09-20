# wstream cell: terra-resp

- model: `gpt-5.6-terra`
- api_backend: `responses` (native)
- expected wire: `responses`
- open questions: PARITY-ARMS terra /responses arm vs recorded sol arm (same binary f9e7a15d6b6b)
- verdict: **PASS**
- binary: sha256_12 f9e7a15d6b6b · wiretap :58935 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (20/20)
- side calls (display-only, e.g. session-title): 19 (non-200: none)
- reasoning (main turn): frames=86 tokens=60 encrypted=True thinking_block=False

## Invariants (must pass)
- [PASS] `status` = 200 (expected 200)
- [PASS] `route` = responses (expected responses)
- [PASS] `n_model_calls_min` = 20 (expected >= 1)

## Observations (OQ capture)
- `route` = "responses"
- `status` = 200
- `n_model_calls` = 20
- `all_stream` = true
- `any_stream` = true
- `req_stream_values` = [true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true]
- `reasoning_present` = true
- `reasoning_frames` = 86
- `reasoning_encrypted_content` = true
- `reasoning_thinking_block` = false
- `reasoning_thought_signature` = false
- `reasoning_sample` = "**Planning skills usage**\n\nI"
- `finish` = null
- `budget_trap` = false
- `final_text_nonempty` = false
- `final_text_len` = 0
- `usage` = {"input_tokens": 16135, "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 16132}, "output_tokens": 270, "output_tokens_details": {"reasoning_tokens": 60}, "total_tokens": 16405, "cost": 0.043576000000000004}

## Per-call detail
- call 1: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=20 reasoning_frames=0 finish=None
- call 2: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=109 reasoning_frames=86 finish=None
- call 3: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=113 reasoning_frames=2 finish=None
- call 4: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=115 reasoning_frames=90 finish=None
- call 5: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=67 reasoning_frames=0 finish=None
- call 6: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=66 reasoning_frames=4 finish=None
- call 7: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=491 reasoning_frames=333 finish=None
- call 8: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=48 reasoning_frames=0 finish=None
- call 9: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=45 reasoning_frames=0 finish=None
- call 10: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=49 reasoning_frames=0 finish=None
- call 11: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=167 reasoning_frames=0 finish=None
- call 12: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=45 reasoning_frames=0 finish=None
- call 13: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=68 reasoning_frames=0 finish=None
- call 14: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=68 reasoning_frames=0 finish=None
- call 15: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=50 reasoning_frames=0 finish=None
- call 16: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=91 reasoning_frames=0 finish=None
- call 17: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=92 reasoning_frames=2 finish=None
- call 18: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=96 reasoning_frames=0 finish=None
- call 19: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=40 reasoning_frames=0 finish=None
- call 20: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=96 reasoning_frames=0 finish=None

## Reasoning sample (truncated)
```
**Planning skills usage**

I
```
