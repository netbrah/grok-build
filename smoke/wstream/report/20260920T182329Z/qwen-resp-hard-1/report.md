# wstream cell: qwen-resp-hard-1

- model: `qwen3.8-27b`
- api_backend: `responses` (native)
- expected wire: `responses`
- open questions: PARITY-ARMS qwen /responses hard-question arm (on-prem, cost 0)
- verdict: **PASS**
- binary: sha256_12 1463fe4cfb4e · wiretap :59821 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (6/6)
- side calls (display-only, e.g. session-title): 5 (non-200: none)
- reasoning (main turn): frames=5066 tokens=13829 encrypted=False thinking_block=False

## Invariants (must pass)
- [PASS] `status` = 200 (expected 200)
- [PASS] `route` = responses (expected responses)
- [PASS] `n_model_calls_min` = 6 (expected >= 1)

## Observations (OQ capture)
- `route` = "responses"
- `status` = 200
- `n_model_calls` = 6
- `all_stream` = true
- `any_stream` = true
- `req_stream_values` = [true, true, true, true, true, true]
- `reasoning_present` = true
- `reasoning_frames` = 5066
- `reasoning_encrypted_content` = false
- `reasoning_thinking_block` = false
- `reasoning_thought_signature` = false
- `reasoning_sample` = "The"
- `finish` = null
- `budget_trap` = false
- `final_text_nonempty` = false
- `final_text_len` = 2
- `usage` = {"input_tokens": 20636, "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0}, "output_tokens": 13857, "output_tokens_details": {"reasoning_tokens": 13829}, "total_tokens": 34493}

## Per-call detail
- call 1: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=32 reasoning_frames=18 finish=None
- call 2: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=5086 reasoning_frames=5066 finish=None
- call 3: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=45 reasoning_frames=5 finish=None
- call 4: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=41 reasoning_frames=13 finish=None
- call 5: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=62 reasoning_frames=25 finish=None
- call 6: route=`responses` model=`qwen3.8-27b` stream=True status=200 frames=229 reasoning_frames=86 finish=None

## Reasoning sample (truncated)
```
The
```
