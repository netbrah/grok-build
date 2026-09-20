# wstream cell: sonnet-churn-msg

- model: `claude-sonnet-5`
- api_backend: `messages` (native)
- expected wire: `messages`
- open questions: REAL-TASK claude /messages churn-redesign arm
- verdict: **PASS**
- binary: sha256_12 f9e7a15d6b6b · wiretap :55710 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (1/1)
- reasoning (main turn): frames=0 tokens=0 encrypted=False thinking_block=False

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
- `finish` = "stop"
- `budget_trap` = false
- `final_text_nonempty` = false
- `final_text_len` = 0
- `usage` = {"input_tokens": 17, "cache_creation_input_tokens": 1125, "cache_read_input_tokens": 0, "output_tokens": 51, "output_tokens_details": {"thinking_tokens": 0}}

## Per-call detail
- call 1: route=`messages` model=`claude-sonnet-5` stream=True status=200 frames=15 reasoning_frames=0 finish=stop
