# wstream cell: opus-churn-msg

- model: `claude-opus-5`
- api_backend: `messages` (native)
- expected wire: `messages`
- open questions: REAL-TASK opus /messages churn-redesign arm (last, most expensive)
- verdict: **PASS**
- binary: sha256_12 f9e7a15d6b6b · wiretap :63641 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (1/1)
- reasoning (main turn): frames=0 tokens=None encrypted=False thinking_block=False

## Invariants (must pass)
- [PASS] `status` = 200 (expected 200)
- [PASS] `route` = messages (expected messages)
- [PASS] `n_model_calls_min` = 1 (expected >= 1)

## Observations (OQ capture)
- `route` = "messages"
- `status` = 200
- `n_model_calls` = 1
- `all_stream` = true
- `any_stream` = true
- `req_stream_values` = [true]
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
- call 1: route=`messages` model=`claude-opus-5` stream=True status=200 frames=0 reasoning_frames=0 finish=None
