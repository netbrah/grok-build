# wstream cell: gemini-resp

- model: `gemini-3.1-pro-preview`
- api_backend: `responses` (native)
- expected wire: `responses`
- open questions: OQ-2
- verdict: **PASS**
- binary: sha256_12 f9e7a15d6b6b · wiretap :52180 → https://llm-proxy-api.ai.eng.netapp.com
- streaming wire calls (stream:true): ALL (2/2)
- side calls (display-only, e.g. session-title): 1 (non-200: none)
- reasoning (main turn): frames=14 tokens=None encrypted=False thinking_block=False

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
- `reasoning_frames` = 14
- `reasoning_encrypted_content` = false
- `reasoning_thinking_block` = false
- `reasoning_thought_signature` = false
- `reasoning_sample` = "**Exploring Modular Exponentiation**\n\nI'm currently working on calculating $7^{8432} \\pmod{5557}$, breaking it down through successive squarings. The process in"
- `finish` = null
- `budget_trap` = false
- `final_text_nonempty` = true
- `final_text_len` = 1064
- `usage` = null

## Per-call detail
- call 1: route=`responses` model=`gemini-3.1-pro-preview` stream=True status=200 frames=9 reasoning_frames=0 finish=None
- call 2: route=`responses` model=`gemini-3.1-pro-preview` stream=True status=200 frames=45 reasoning_frames=14 finish=None

## Reasoning sample (truncated)
```
**Exploring Modular Exponentiation**

I'm currently working on calculating $7^{8432} \pmod{5557}$, breaking it down through successive squarings. The process in
```
