# wstream cell: qwen-churn-resp

- model: `qwen3.8-27b`
- api_backend: `responses` (native)
- expected wire: `responses`
- open questions: REAL-TASK qwen /responses churn-redesign arm (on-prem, cost 0)
- verdict: **PASS**
- binary: sha256_12 f9e7a15d6b6b · wiretap :55739 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (77/77)
- side calls (display-only, e.g. session-title): 76 (non-200: [500])
- reasoning (main turn): frames=428 tokens=1130 encrypted=False thinking_block=False

## Invariants (must pass)
- [PASS] `status` = 200 (expected 200)
- [PASS] `route` = responses (expected responses)
- [PASS] `n_model_calls_min` = 77 (expected >= 1)

## Observations (OQ capture)
- `route` = "responses"
- `status` = 200
- `n_model_calls` = 77
- `all_stream` = true
- `any_stream` = true
- `req_stream_values` = [true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true]
- `reasoning_present` = true
- `reasoning_frames` = 428
- `reasoning_encrypted_content` = false
- `reasoning_thinking_block` = false
- `reasoning_thought_signature` = false
- `reasoning_sample` = "Let"
- `finish` = null
- `budget_trap` = false
- `final_text_nonempty` = true
- `final_text_len` = 122
- `usage` = {"input_tokens": 24291, "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0}, "output_tokens": 1379, "output_tokens_details": {"reasoning_tokens": 1130}, "total_tokens": 25670}

## Per-call detail
- call 1: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=45 reasoning_frames=37 finish=max_output_tokens
- call 2: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=465 reasoning_frames=428 finish=None
- call 3: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=134 reasoning_frames=102 finish=None
- call 4: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=92 reasoning_frames=65 finish=None
- call 5: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=150 reasoning_frames=109 finish=None
- call 6: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=173 reasoning_frames=132 finish=None
- call 7: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=214 reasoning_frames=165 finish=None
- call 8: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=115 reasoning_frames=82 finish=None
- call 9: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=82 reasoning_frames=53 finish=None
- call 10: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=52 reasoning_frames=22 finish=None
- call 11: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=202 reasoning_frames=167 finish=None
- call 12: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=113 reasoning_frames=72 finish=None
- call 13: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=172 reasoning_frames=125 finish=None
- call 14: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=121 reasoning_frames=89 finish=None
- call 15: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=76 reasoning_frames=46 finish=None
- call 16: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=223 reasoning_frames=199 finish=None
- call 17: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=84 reasoning_frames=54 finish=None
- call 18: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=38 reasoning_frames=14 finish=None
- call 19: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=425 reasoning_frames=361 finish=None
- call 20: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=39 reasoning_frames=18 finish=None
- call 21: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=37 reasoning_frames=16 finish=None
- call 22: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=40 reasoning_frames=19 finish=None
- call 23: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=35 reasoning_frames=14 finish=None
- call 24: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=43 reasoning_frames=22 finish=None
- call 25: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=30 reasoning_frames=9 finish=None
- call 26: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=33 reasoning_frames=13 finish=None
- call 27: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=27 reasoning_frames=7 finish=None
- call 28: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=30 reasoning_frames=9 finish=None
- call 29: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=34 reasoning_frames=13 finish=None
- call 30: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=41 reasoning_frames=20 finish=None
- call 31: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=41 reasoning_frames=20 finish=None
- call 32: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=1330 reasoning_frames=1304 finish=None
- call 33: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=42 reasoning_frames=19 finish=None
- call 34: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=59 reasoning_frames=36 finish=None
- call 35: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=67 reasoning_frames=45 finish=None
- call 36: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=56 reasoning_frames=34 finish=None
- call 37: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=52 reasoning_frames=30 finish=None
- call 38: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=185 reasoning_frames=153 finish=None
- call 39: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=54 reasoning_frames=30 finish=None
- call 40: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=1689 reasoning_frames=1644 finish=None
- call 41: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=52 reasoning_frames=20 finish=None
- call 42: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=57 reasoning_frames=27 finish=None
- call 43: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=163 reasoning_frames=142 finish=None
- call 44: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=31 reasoning_frames=10 finish=None
- call 45: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=102 reasoning_frames=79 finish=None
- call 46: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=36 reasoning_frames=15 finish=None
- call 47: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=55 reasoning_frames=34 finish=None
- call 48: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=39 reasoning_frames=18 finish=None
- call 49: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=28 reasoning_frames=7 finish=None
- call 50: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=37 reasoning_frames=16 finish=None
- call 51: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=41 reasoning_frames=21 finish=None
- call 52: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=211 reasoning_frames=175 finish=None
- call 53: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=83 reasoning_frames=62 finish=None
- call 54: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=31 reasoning_frames=7 finish=None
- call 55: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=95 reasoning_frames=63 finish=None
- call 56: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=28 reasoning_frames=7 finish=None
- call 57: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=44 reasoning_frames=23 finish=None
- call 58: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=41 reasoning_frames=20 finish=None
- call 59: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=57 reasoning_frames=36 finish=None
- call 60: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=54 reasoning_frames=33 finish=None
- call 61: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=975 reasoning_frames=931 finish=None
- call 62: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=70 reasoning_frames=47 finish=None
- call 63: route=`responses` model=`qwen3.8-27b` stream=True status=500 frames=1 reasoning_frames=0 finish=None
- call 64: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=36 reasoning_frames=14 finish=None
- call 65: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=499 reasoning_frames=461 finish=None
- call 66: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=195 reasoning_frames=170 finish=None
- call 67: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=4734 reasoning_frames=4705 finish=None
- call 68: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=78 reasoning_frames=57 finish=None
- call 69: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=40 reasoning_frames=19 finish=None
- call 70: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=32 reasoning_frames=11 finish=None
- call 71: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=3566 reasoning_frames=3549 finish=None
- call 72: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=3080 reasoning_frames=3056 finish=None
- call 73: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=1379 reasoning_frames=1346 finish=None
- call 74: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=130 reasoning_frames=97 finish=None
- call 75: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=48 reasoning_frames=26 finish=None
- call 76: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=74 reasoning_frames=54 finish=None
- call 77: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=115 reasoning_frames=22 finish=None

## Reasoning sample (truncated)
```
Let
```
