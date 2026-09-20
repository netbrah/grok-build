# wstream cell: qwen-resp

- model: `qwen3.8-27b`
- api_backend: `responses` (native)
- expected wire: `responses`
- open questions: OQ-3
- verdict: **PASS**
- binary: sha256_12 24a941bac02b · wiretap :55961 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (2/2)
- side calls (display-only, e.g. session-title): 1 (non-200: none)
- reasoning (main turn): frames=4854 tokens=17458 encrypted=False thinking_block=False

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
- `reasoning_frames` = 4854
- `reasoning_encrypted_content` = false
- `reasoning_thinking_block` = false
- `reasoning_thought_signature` = false
- `reasoning_sample` = "The"
- `finish` = null
- `budget_trap` = false
- `final_text_nonempty` = true
- `final_text_len` = 855
- `usage` = {"input_tokens": 20434, "input_tokens_details": {"cached_tokens": 8640, "cache_write_tokens": 0}, "output_tokens": 18051, "output_tokens_details": {"reasoning_tokens": 17458}, "total_tokens": 38485}

## Per-call detail
- call 1: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=37 reasoning_frames=29 finish=max_output_tokens
- call 2: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=5022 reasoning_frames=4854 finish=None

## Reasoning sample (truncated)
```
The
```
