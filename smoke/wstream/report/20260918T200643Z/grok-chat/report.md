# wstream cell: grok-chat

- model: `grok-4.6`
- api_backend: `chat_completions` (override)
- expected wire: `chat/completions`
- open questions: OQ-7
- verdict: **PASS**
- binary: sha256_12 24a941bac02b · wiretap :57005 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (2/2)
- side calls (display-only, e.g. session-title): 1 (non-200: none)
- reasoning (main turn): frames=0 tokens=None encrypted=False thinking_block=False

## Invariants (must pass)
- [PASS] `status` = 200 (expected 200)
- [PASS] `route` = chat/completions (expected chat/completions)
- [PASS] `n_model_calls_min` = 2 (expected >= 1)

## Observations (OQ capture)
- `route` = "chat/completions"
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
- `final_text_len` = 589
- `usage` = null

## Per-call detail
- call 1: route=`chat/completions` model=`grok-4.6` stream=True status=200 frames=4 reasoning_frames=0 finish=tool_calls
- call 2: route=`chat/completions` model=`grok-4.6` stream=True status=200 frames=288 reasoning_frames=0 finish=stop
