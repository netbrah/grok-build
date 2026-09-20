# wstream cell: opus-msg-hard

- model: `claude-opus-5`
- api_backend: `messages` (native)
- expected wire: `messages`
- open questions: PARITY-ARMS opus /messages arm, hardest-question pick (single-opus budget)
- verdict: **PASS**
- binary: sha256_12 f9e7a15d6b6b · wiretap :49373 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (11/11)
- side calls (display-only, e.g. session-title): 10 (non-200: none)
- reasoning (main turn): frames=0 tokens=0 encrypted=False thinking_block=False

## Invariants (must pass)
- [PASS] `status` = 200 (expected 200)
- [PASS] `route` = messages (expected messages)
- [PASS] `n_model_calls_min` = 11 (expected >= 1)

## Observations (OQ capture)
- `route` = "messages"
- `status` = 200
- `n_model_calls` = 11
- `all_stream` = true
- `any_stream` = true
- `req_stream_values` = [true, true, true, true, true, true, true, true, true, true, true]
- `reasoning_present` = false
- `reasoning_frames` = 0
- `reasoning_encrypted_content` = false
- `reasoning_thinking_block` = false
- `reasoning_thought_signature` = false
- `reasoning_sample` = null
- `finish` = "stop"
- `budget_trap` = false
- `final_text_nonempty` = true
- `final_text_len` = 64
- `usage` = {"input_tokens": 2, "cache_creation_input_tokens": 29836, "cache_read_input_tokens": 0, "output_tokens": 117, "output_tokens_details": {"thinking_tokens": 0}}

## Per-call detail
- call 1: route=`messages` model=`claude-opus-5` stream=True status=200 frames=17 reasoning_frames=0 finish=stop
- call 2: route=`messages` model=`claude-opus-5` stream=True status=200 frames=32 reasoning_frames=0 finish=stop
- call 3: route=`messages` model=`claude-opus-5` stream=True status=200 frames=547 reasoning_frames=34 finish=stop
- call 4: route=`messages` model=`claude-opus-5` stream=True status=200 frames=168 reasoning_frames=77 finish=stop
- call 5: route=`messages` model=`claude-opus-5` stream=True status=200 frames=84 reasoning_frames=0 finish=stop
- call 6: route=`messages` model=`claude-opus-5` stream=True status=200 frames=55 reasoning_frames=12 finish=stop
- call 7: route=`messages` model=`claude-opus-5` stream=True status=200 frames=107 reasoning_frames=22 finish=stop
- call 8: route=`messages` model=`claude-opus-5` stream=True status=200 frames=248 reasoning_frames=18 finish=stop
- call 9: route=`messages` model=`claude-opus-5` stream=True status=200 frames=133 reasoning_frames=46 finish=stop
- call 10: route=`messages` model=`claude-opus-5` stream=True status=200 frames=131 reasoning_frames=18 finish=stop
- call 11: route=`messages` model=`claude-opus-5` stream=True status=200 frames=175 reasoning_frames=13 finish=stop
