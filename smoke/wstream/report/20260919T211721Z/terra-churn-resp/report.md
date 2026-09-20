# wstream cell: terra-churn-resp

- model: `gpt-5.6-terra`
- api_backend: `responses` (native)
- expected wire: `responses`
- open questions: REAL-TASK terra /responses churn-redesign arm
- verdict: **PASS**
- binary: sha256_12 f9e7a15d6b6b · wiretap :55145 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (13/13)
- side calls (display-only, e.g. session-title): 12 (non-200: none)
- reasoning (main turn): frames=2 tokens=105 encrypted=True thinking_block=False

## Invariants (must pass)
- [PASS] `status` = 200 (expected 200)
- [PASS] `route` = responses (expected responses)
- [PASS] `n_model_calls_min` = 13 (expected >= 1)

## Observations (OQ capture)
- `route` = "responses"
- `status` = 200
- `n_model_calls` = 13
- `all_stream` = true
- `any_stream` = true
- `req_stream_values` = [true, true, true, true, true, true, true, true, true, true, true, true, true]
- `reasoning_present` = true
- `reasoning_frames` = 2
- `reasoning_encrypted_content` = true
- `reasoning_thinking_block` = false
- `reasoning_thought_signature` = false
- `reasoning_sample` = null
- `finish` = null
- `budget_trap` = false
- `final_text_nonempty` = false
- `final_text_len` = 0
- `usage` = {"input_tokens": 19571, "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 19568}, "output_tokens": 349, "output_tokens_details": {"reasoning_tokens": 105}, "total_tokens": 19920, "cost": 0.053114}

## Per-call detail
- call 1: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=24 reasoning_frames=2 finish=None
- call 2: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=22 reasoning_frames=2 finish=None
- call 3: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=18 reasoning_frames=2 finish=None
- call 4: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=14 reasoning_frames=2 finish=None
- call 5: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=12 reasoning_frames=0 finish=None
- call 6: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=14 reasoning_frames=2 finish=None
- call 7: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=14 reasoning_frames=2 finish=None
- call 8: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=108 reasoning_frames=85 finish=None
- call 9: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=575 reasoning_frames=8 finish=None
- call 10: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=16 reasoning_frames=0 finish=None
- call 11: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=107 reasoning_frames=2 finish=None
- call 12: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=12 reasoning_frames=0 finish=None
- call 13: route=`responses` model=`gpt-5.6-terra` stream=True status=200 frames=76 reasoning_frames=0 finish=None
