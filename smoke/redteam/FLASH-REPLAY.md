# Responses reasoning replay checks

`kind: reasoning_replay` is a versioned wire assertion in the existing redteam
runner. It measures client-side continuation fields, not cryptographic validity,
deployment affinity, backend identity or server-side semantic reuse.

## Offline verification

From the code repository root:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 smoke/redteam/test_reasoning_replay.py -v
PYTHONDONTWRITEBYTECODE=1 python3 smoke/redteam/test_run_golden.py -v
```

The focused suite uses synthetic temporary captures. Some existing golden/runner
tests use owned local stubs and loopback HTTP; these are not live model calls.
The full legacy runner suite has separately recorded case-contract failures;
passing the replay checks does not make that suite green.

## Assertion shape

```json
{
  "id": "native_replay",
  "kind": "reasoning_replay",
  "file": "req-*.json",
  "where": {
    "method": "POST",
    "path": "/v1/responses",
    "headers.x-grok-req-id": {"not_prefix": "xai-"}
  },
  "replay": {
    "version": 1,
    "segments": [
      {
        "model": "qwen3.8-flash-next",
        "min_requests": 2,
        "fields": ["encrypted_content", "summary", "content"]
      }
    ]
  }
}
```

Use the exact captured request and returned model identity. Aliases with different
returned names are unsupported by version 1. Add ordered segments for model
switches; each segment must have at least two completed exchanges and its first
response must mint new nonempty state for a subsequent request to replay.
Request counts are not user-turn counts: tool continuation can add requests.

`encrypted_content` is always a required comparison field. Optional `summary`
and `content` compare parsed field values, including missing versus present.
`require_content` and `require_empty_summary` constrain minted item shape;
they do not infer plaintext from an opaque field. `require_tool_continuation`
requires a completed/allowed-status function or custom call and its correctly
typed, nonempty text result in the next same-model request. Other result forms
are unsupported in this version.

## Evidence and failure behavior

- Numeric `req-NNN.json`/`resp-NNN.jsonl` pairing determines order. Only final
  completed response output items mint eligible state; lifecycle echoes do not.
- The existing wiretap JSONL wrapper and streamed/nonstream Responses are
  supported. Missing, malformed, truncated or ambiguous relevant captures fail.
  Terminal and event types are checked explicitly; malformed completion bodies
  cannot disappear before a later valid completion.
- Session identity and exact model lineage are required. Ordinary same-session
  model/protocol transitions cannot be hidden by the endpoint filter. Explicit
  auxiliary-header exclusions and provably unrelated sessions remain distinct.
- Same-model emitted state must recur unchanged. Foreign or unobserved state
  fails. A return to an earlier model may retain its previously observed state;
  fresh state in the return segment must still be emitted and replayed. This
  model-only rule does not establish deployment compatibility.
- The request's `include` list is intent, not emitted/replayed item state.
  Neither message text containing a marker nor the outer `litellm_enc:` prefix
  establishes source/backend identity.
- Reports publish counts, types, marker flags and SHA equality, never raw
  continuation values or reasoning text. Invalid Unicode uses safe diagnostics.

## New opt-in cases

The following cases are disabled pending their recorded live prerequisites:

| Case file | Scenario |
|---|---|
| `cases/flash-native-replay-v1.json` | Three headless turns, post-tool continuation and persisted resume. |
| `cases/flash-to-astra-replay-v1.json` | Two Flash turns followed by two Astra turns. |
| `cases/flash-astra-to-flash-replay-v1.json` | Two Astra turns followed by two Flash turns. |
| `cases/flash-switchback-replay-v1.json` | Flash → Astra → Flash, two turns per segment. |

Before enabling a case, pin its binary/config/case bytes, exact model identities,
route/deployment coordinates and private capture location; enforce POST, output
and wall-time budgets. `store=false` is mandatory. Product fixes and strict
agent-smoke certification prerequisites remain separately tracked in
`apex-ayl.169`, `.173` and `.170`. No expected-red credit substitutes for these
checks. Historical cases/captures/goldens were not renamed or regenerated.
