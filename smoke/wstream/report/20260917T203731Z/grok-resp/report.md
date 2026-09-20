# wstream cell: grok-resp

- model: `grok-4.6`
- api_backend: `responses` (native)
- expected wire: `responses`
- open questions: OQ-7
- verdict: **FAIL**
- binary: sha256_12 39f836e8633d · wiretap :52560 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (2/2)

## Invariants (must pass)
- [FAIL] `status` = mixed (expected 200)
- [PASS] `route` = responses (expected responses)
- [PASS] `n_model_calls_min` = 2 (expected >= 1)

## Observations (OQ capture)
- `route` = "responses"
- `status` = "mixed"
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
- `final_text_len` = 695
- `usage` = {"input_tokens": 7267, "output_tokens": 350, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 7617, "cost": 0.016634}

## Per-call detail
- call 1: route=`responses` model=`grok-4.6` stream=True status=500 frames=1 reasoning_frames=0 finish=None
- call 2: route=`responses` model=`grok-4.6` stream=True status=200 frames=356 reasoning_frames=0 finish=None
