# wstream cell: glm-resp

- model: `glm-5.2`
- api_backend: `responses` (native)
- expected wire: `responses`
- open questions: 
- verdict: **PASS**
- binary: sha256_12 f9e7a15d6b6b · wiretap :52872 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (2/2)
- side calls (display-only, e.g. session-title): 1 (non-200: none)
- reasoning (main turn): frames=4739 tokens=0 encrypted=False thinking_block=False

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
- `reasoning_present` = true
- `reasoning_frames` = 4739
- `reasoning_encrypted_content` = false
- `reasoning_thinking_block` = false
- `reasoning_thought_signature` = false
- `reasoning_sample` = "The"
- `finish` = null
- `budget_trap` = false
- `final_text_nonempty` = true
- `final_text_len` = 1615
- `usage` = {"input_tokens": 19267, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 10201, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 29468}

## Per-call detail
- call 1: route=`responses` model=`glm-5.2` stream=True status=200 frames=20 reasoning_frames=0 finish=None
- call 2: route=`responses` model=`glm-5.2` stream=True status=200 frames=5226 reasoning_frames=4739 finish=None

## Reasoning sample (truncated)
```
The
```
