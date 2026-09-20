# wstream cell: grok-resp

- model: `grok-4.6`
- api_backend: `responses` (native)
- expected wire: `responses`
- open questions: OQ-7
- verdict: **PASS**
- binary: sha256_12 f9e7a15d6b6b · wiretap :56737 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (2/2)
- side calls (display-only, e.g. session-title): 1 (non-200: none)
- reasoning (main turn): frames=0 tokens=0 encrypted=False thinking_block=False

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
- `reasoning_thought_signature` = false
- `reasoning_sample` = null
- `finish` = null
- `budget_trap` = false
- `final_text_nonempty` = true
- `final_text_len` = 614
- `usage` = {"input_tokens": 7338, "output_tokens": 294, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 7632, "cost": 0.01644}

## Per-call detail
- call 1: route=`responses` model=`grok-4.6` stream=True status=200 frames=35 reasoning_frames=0 finish=None
- call 2: route=`responses` model=`grok-4.6` stream=True status=200 frames=300 reasoning_frames=0 finish=None
