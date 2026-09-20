# wstream cell: sol-msg

- model: `gpt-5.6-sol`
- api_backend: `messages` (override)
- expected wire: `messages`
- open questions: OQ-5
- verdict: **PASS**
- binary: sha256_12 f9e7a15d6b6b · wiretap :53041 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (2/2)
- side calls (display-only, e.g. session-title): 1 (non-200: none)
- reasoning (main turn): frames=0 tokens=None encrypted=False thinking_block=False

## Invariants (must pass)
- [PASS] `status` = 200 (expected 200)
- [PASS] `route` = messages (expected messages)
- [PASS] `n_model_calls_min` = 2 (expected >= 1)

## Observations (OQ capture)
- `route` = "messages"
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
- `finish` = "stop"
- `budget_trap` = false
- `final_text_nonempty` = true
- `final_text_len` = 847
- `usage` = {"input_tokens": 3, "output_tokens": 2608, "cache_creation_input_tokens": 16536}

## Per-call detail
- call 1: route=`messages` model=`gpt-5.6-sol` stream=True status=200 frames=19 reasoning_frames=0 finish=stop
- call 2: route=`messages` model=`gpt-5.6-sol` stream=True status=200 frames=118 reasoning_frames=0 finish=stop
