# wstream cell: ws9-s01-toolloop-sol

- model: `gpt-5.6-sol`
- api_backend: `responses` (native)
- expected wire: `responses`
- open questions: WS9-S01 streaming arm (JIG L3 stream-fidelity per wire): tool-call SSE frames + thinking deltas + usage consistency on the flagship strict seat (strict_responses_input=true). Cross-ref smoke/redteam/cases/ws9-s01-tool-loop.json (the multi-turn redteam arm) + grok/plans/ws9-12row-schema.md (S01 row).
- verdict: **PASS**
- binary: sha256_12 e635d5d7f2fe · wiretap :61127 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (4/4)
- side calls (display-only, e.g. session-title): 3 (non-200: [500])
- reasoning (main turn): frames=0 tokens=0 encrypted=False thinking_block=False

## Invariants (must pass)
- [PASS] `status` = 200 (expected 200)
- [PASS] `route` = responses (expected responses)
- [PASS] `n_model_calls_min` = 4 (expected >= 2)

## Observations (OQ capture)
- `route` = "responses"
- `status` = 200
- `n_model_calls` = 4
- `all_stream` = true
- `any_stream` = true
- `req_stream_values` = [true, true, true, true]
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
- `usage` = {"input_tokens": 16557, "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 16554}, "output_tokens": 44, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 16601, "cost": 0.08366200000000001}

## Per-call detail
- call 1: route=`responses` model=`grok-4.6` stream=True status=500 frames=1 reasoning_frames=0 finish=None
- call 2: route=`responses` model=`gpt-5.6-sol` stream=True status=200 frames=37 reasoning_frames=0 finish=None
- call 3: route=`responses` model=`gpt-5.6-sol` stream=True status=200 frames=37 reasoning_frames=0 finish=None
- call 4: route=`responses` model=`gpt-5.6-sol` stream=True status=200 frames=17 reasoning_frames=0 finish=None
