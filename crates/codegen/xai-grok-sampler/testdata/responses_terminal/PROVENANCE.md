# Responses terminal fixture provenance

Every frame in this directory is synthetic test data. No frame was copied from
a live request, response, session log, screenshot, or wire capture. Envelope
fields follow the repository's `xai_grok_test_support::sse` builders and use
fixed fixture identifiers, timestamps, models, token counts, tool names, and
error text.

The corpus models the terminal classes investigated under Bead
`apex-ayl.152`. Its research provenance is
`/Users/palanisd/Projects/upstream/on-prem/research/2026-09-29-flash-next-blank-and-garbling-investigation.md`.
That investigation motivates the structural cases but is not a source of raw
fixture payloads.

Whitespace is semantic fixture data: E1 preserves zero text bytes, while E2
preserves exactly the two-byte string `" \n"` and a zero-byte trimmed value.
Array order and SSE frame order are intentional and must remain unchanged. N1
places an empty nonterminal text delta before its valid V1-style terminal; T1
ends at finite EOF without a provider terminal; C1 keeps a final synthetic
completion only so the test harness can block before it and cancel after the
preceding frame.

Fallback rows state replay-gate inputs independently from their expected
routes. Their provider-state payloads are also synthetic: the first-class
reasoning ciphertext and opaque `CodexRawInput` carrier use distinct sentinel
strings so later boundary tests can remove only the former and compare the
latter byte-for-byte. V2 intentionally orders a synthetic reasoning item with
ciphertext before its function call for the accepted tool-handback test.
