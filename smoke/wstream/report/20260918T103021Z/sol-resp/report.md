# wstream cell: sol-resp

- model: `gpt-5.6-sol`
- api_backend: `responses` (native)
- expected wire: `responses`
- open questions: OQ-1
- verdict: **PASS**
- binary: sha256_12 e635d5d7f2fe · wiretap :61455 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (2/2)
- side calls (display-only, e.g. session-title): 1 (non-200: [500])
- reasoning (main turn): frames=99 tokens=2044 encrypted=True thinking_block=False

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
- `reasoning_frames` = 99
- `reasoning_encrypted_content` = true
- `reasoning_thinking_block` = false
- `reasoning_thought_signature` = false
- `reasoning_sample` = "**Calculating modular exponentiation manually**\n\nI"
- `finish` = null
- `budget_trap` = false
- `final_text_nonempty` = true
- `final_text_len` = 723
- `usage` = {"input_tokens": 16516, "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 16513}, "output_tokens": 2438, "output_tokens_details": {"reasoning_tokens": 2044}, "total_tokens": 18954, "cost": 0.131337}

## Per-call detail
- call 1: route=`responses` model=`grok-4.6` stream=True status=500 frames=1 reasoning_frames=0 finish=None
- call 2: route=`responses` model=`gpt-5.6-sol` stream=True status=200 frames=217 reasoning_frames=99 finish=None

## Reasoning sample (truncated)
```
**Calculating modular exponentiation manually**

I
```
