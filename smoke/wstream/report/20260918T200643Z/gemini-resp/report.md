# wstream cell: gemini-resp

- model: `gemini-3.1-pro-preview`
- api_backend: `responses` (native)
- expected wire: `responses`
- open questions: OQ-2
- verdict: **PASS**
- binary: sha256_12 24a941bac02b · wiretap :55697 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (2/2)
- side calls (display-only, e.g. session-title): 1 (non-200: none)
- reasoning (main turn): frames=0 tokens=18682 encrypted=False thinking_block=False

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
- `reasoning_present` = false
- `reasoning_frames` = 0
- `reasoning_encrypted_content` = false
- `reasoning_thinking_block` = false
- `reasoning_thought_signature` = true
- `reasoning_sample` = null
- `finish` = null
- `budget_trap` = false
- `final_text_nonempty` = true
- `final_text_len` = 1480
- `usage` = {"input_tokens": 16427, "input_tokens_details": {"cached_tokens": 0, "text_tokens": 16427}, "output_tokens": 19581, "output_tokens_details": {"reasoning_tokens": 18682, "text_tokens": 899}, "total_tokens": 36008, "cost": 0.267826}

## Per-call detail
- call 1: route=`responses` model=`gemini-3.1-pro-preview` stream=True status=200 frames=9 reasoning_frames=0 finish=None
- call 2: route=`responses` model=`gemini-3.1-pro-preview` stream=True status=200 frames=47 reasoning_frames=0 finish=None
