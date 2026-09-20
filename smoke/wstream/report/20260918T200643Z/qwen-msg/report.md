# wstream cell: qwen-msg

- model: `qwen3.8-27b`
- api_backend: `messages` (override)
- expected wire: `messages`
- open questions: OQ-5
- verdict: **FAIL**
- binary: sha256_12 24a941bac02b · wiretap :56975 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (2/2)
- side calls (display-only, e.g. session-title): 1 (non-200: none)
- reasoning (main turn): frames=0 tokens=None encrypted=False thinking_block=False

## Invariants (must pass)
- [FAIL] `status` = 400 (expected 200)
- [PASS] `route` = messages (expected messages)
- [PASS] `n_model_calls_min` = 2 (expected >= 1)

## Observations (OQ capture)
- `route` = "messages"
- `status` = 400
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
- `final_text_nonempty` = false
- `final_text_len` = 0
- `usage` = null

## Per-call detail
- call 1: route=`messages` model=`qwen3.8-27b` stream=True status=200 frames=3 reasoning_frames=0 finish=stop
- call 2: route=`messages` model=`qwen3.8-27b` stream=True status=400 frames=1 reasoning_frames=0 finish=None
