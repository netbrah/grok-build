# MERGE-XAI-SYNC — fixtures (wave-2 scaffolding, 06 §4)

Home for the hermetic-smoke fixtures of the 06 §2 cell matrix (spec:
`grok/plans/merge-xai/06-smoke-plan.md`; operator runbook:
`smoke/merge-xai/README.md`). Committed per 04 §5 step 1 (`smoke/**`) —
everything here is REDACTED (sweep-0) before commit.

## Redaction discipline (sweep-0)

- No creds / host paths / IPs / secret-bearing URLs in any fixture.
- Model ids + structural shapes STAY (the shape is what several cells pin:
  the L-03 `cache_ttl = "1h"` row, the child tool surface, the SSE frame
  shape, the 300-char MCP tool name).
- Case-owned literals (nonces, the eviction placeholder string) are fine —
  they are the pin, not a secret.
- Any operator-populated file (the C-01 wire pair, the C-09 image-arm record)
  is re-swept before commit; the population procedures below say so.

## Index

| dir | cell | cases | kind | status |
|---|---|---|---|---|
| `c-01-replay/` | C-01 | MXAI-C01-REPLAY | exact-bytes retry-pair golden (pre-placed wire; populated from a real capture) | DISABLED SCAFFOLD — operator population (procedure in META.json) |
| `c-02-trunc/` | C-02 | MXAI-C02-TRUNC-SHAPE, MXAI-C02 | pre-placed truncated-SSE capture + `resp_stream` golden | shape arm PASS-verified offline 2026-09-19; live arm awaits the operator fault-injection |
| `c-03-cache/` | C-03 | MXAI-C03 | redacted `/v1/messages` request shape carrying the cache-breakpoint pins | reference shape (structural); the live capture is the evidence, block-level placement adjudicates on the recon |
| `c-07-child/` | C-07 | MXAI-C07 | M7 child tool-projection shape (absent / present lists) | reference shape; the scored spot-checks live in MXAI-C07 (last:0 pins) |
| `c-08-cell/` | C-08 | MXAI-C08, MXAI-C08-BASELINE | cell-seeded identity history (`pre_switch.json` + `expected.json`, loaded at runtime) | REDACTED SCAFFOLD — structurally complete |
| `c-09-image/` | C-09 | MXAI-C09 | operator 47 MiB image-induction arm spec (trigger / target / placeholder literals) | operator arm — the driver cannot attach images (DRIVER GAP C-09) |
| `c-13-mcp/` | C-13 | MXAI-C13 | stdlib-only MCP stdio test double (`mcp-longprefix-stub.py`) + provisioning spec | offline-tested 2026-09-19; operator provisions `[mcp_servers.mxai_longprefix]` in the LIVE config |

## Where the wire fixtures physically live

The two `proxy:none` mechanism arms resolve their pre-placed wire through
`smoke/merge-xai/wire/<case-stem>/wire/` (symlinked into
`smoke/redteam/cases/<case-stem>/` so the driver's case-stem resolution finds
it):

- `c-01-replay` → `smoke/merge-xai/wire/mxai-c01-replay/wire/`
  (`req-001.json`, `req-002.json`, `expected-retry-body.json`)
- `c-02-trunc` → `smoke/merge-xai/wire/mxai-c02-trunc-shape/wire/`
  (`req-001.json`, `resp-001.jsonl`, `expected-trunc-frames.json`)

The `c-*/` directories here hold the META / shape documents describing those
files; the wire captures themselves stay under `smoke/merge-xai/wire/`
because that is where the driver's resolution lands them.
