# wstream cell: sonnet-msg

- model: `claude-sonnet-5`
- api_backend: `messages` (native)
- expected wire: `messages`
- open questions: PARITY-ARMS claude /messages arm vs recorded sol arm (same binary f9e7a15d6b6b)
- verdict: **PASS**
- binary: sha256_12 1463fe4cfb4e · wiretap :59556 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (9/9)
- side calls (display-only, e.g. session-title): 8 (non-200: none)
- reasoning (main turn): frames=189 tokens=3248 encrypted=False thinking_block=True

## Invariants (must pass)
- [PASS] `status` = 200 (expected 200)
- [PASS] `route` = messages (expected messages)
- [PASS] `n_model_calls_min` = 9 (expected >= 1)

## Observations (OQ capture)
- `route` = "messages"
- `status` = 200
- `n_model_calls` = 9
- `all_stream` = true
- `any_stream` = true
- `req_stream_values` = [true, true, true, true, true, true, true, true, true]
- `reasoning_present` = true
- `reasoning_frames` = 189
- `reasoning_encrypted_content` = false
- `reasoning_thinking_block` = true
- `reasoning_thought_signature` = false
- `reasoning_sample` = "This"
- `finish` = "stop"
- `budget_trap` = false
- `final_text_nonempty` = false
- `final_text_len` = 0
- `usage` = {"input_tokens": 2, "cache_creation_input_tokens": 29815, "cache_read_input_tokens": 0, "output_tokens": 3301, "output_tokens_details": {"thinking_tokens": 3248}}

## Per-call detail
- call 1: route=`messages` model=`claude-sonnet-5` stream=True status=200 frames=15 reasoning_frames=0 finish=stop
- call 2: route=`messages` model=`claude-sonnet-5` stream=True status=200 frames=204 reasoning_frames=189 finish=stop
- call 3: route=`messages` model=`claude-sonnet-5` stream=True status=200 frames=198 reasoning_frames=7 finish=stop
- call 4: route=`messages` model=`claude-sonnet-5` stream=True status=200 frames=278 reasoning_frames=138 finish=stop
- call 5: route=`messages` model=`claude-sonnet-5` stream=True status=200 frames=59 reasoning_frames=8 finish=stop
- call 6: route=`messages` model=`claude-sonnet-5` stream=True status=200 frames=75 reasoning_frames=27 finish=stop
- call 7: route=`messages` model=`claude-sonnet-5` stream=True status=200 frames=95 reasoning_frames=17 finish=stop
- call 8: route=`messages` model=`claude-sonnet-5` stream=True status=200 frames=87 reasoning_frames=30 finish=stop
- call 9: route=`messages` model=`claude-sonnet-5` stream=True status=200 frames=125 reasoning_frames=0 finish=stop

## Reasoning sample (truncated)
```
This
```
