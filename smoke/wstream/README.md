# WSTREAM — wire-streaming matrix (model × api_backend × wire)

Captures **STREAMING** behavior — the harness's real path — across the
`model × api_backend × wire` cross-product, using the dogfood hermetic-home
mechanism. W10-B's probes were **non-streaming** (direct HTTP) and hid
reasoning content on the vertex dialects; the harness always streams, so this
suite is where the W10 open questions the harness can actually reach get
answered.

## Why streaming is the real path

The harness drives every turn as a streaming request. A non-streaming probe
of the same model×wire can show `reasoning_tokens > 0` while the reasoning
**content** is suppressed (W10-B: grok-4.6 rtok 594, content NULL
non-streaming). The streaming SSE frames are what the operator's session
actually sees — thinking deltas, encrypted_content items, thoughtSignature
carriers — so fidelity questions can only be settled here.

## Cross-product (selective, not full enumeration)

6 frontier models × (native api_backend + targeted overrides). The wire the
request hits is determined by the cell's `api_backend`:

| api_backend (config) | wire (`/v1/<route>`) |
|---|---|
| `responses` | `/v1/responses` |
| `messages` | `/v1/messages` |
| `chat_completions` | `/v1/chat/completions` |

Cells (see `manifest.json`):
- **Native** (the model's default backend from the live config): `grok-resp`
  (PILOT) · `sol-resp` · `opus-msg` · `gemini-resp` · `qwen-resp` · `glm-resp`
- **Overrides** (cross-dialect — the "backend cross wire" arm): `sol-msg` ·
  `qwen-msg` · `gemini-msg` · `grok-chat`

Each cell freezes its config by **copying the live `~/.grok/config.toml` + a
declared `api_backend` patch** and writing the derived file to the cell's
report dir (`derived-config.toml`). We deliberately do NOT store full 293-line
config copies (maintenance); the derivation is live-config + one declared
field, captured per-run. The same mechanism is what the redteam switch cells
will reuse to flip model/backend mid-run (see "Extension" below).

## W10 open questions this suite resolves

| OQ | Question | Cell(s) |
|---|---|---|
| OQ-1 | sol `/responses` native-vs-bridge (encrypted_content / reasoning deltas in stream; id encoding) | `sol-resp` |
| OQ-2 | vertex-gemini reasoning content IN STREAMING | `gemini-resp` · `gemini-msg` |
| OQ-7 | vertex-xai (grok-4.6) thinking content IN STREAMING | `grok-resp` (PILOT) · `grok-chat` |
| OQ-3 | qwen budget-trap IN STREAMING (empty text + max_tokens) | `qwen-resp` |
| OQ-5 | `/messages` per-class dispatch trace (which route the messages wire actually hits) | `sol-msg` · `qwen-msg` · `gemini-msg` |

**Out of scope (proxy-level, not harness-reachable via the 3 api_backends):**
OQ-4 (thoughtSignature multi-turn — google-dialect `/generateContent`) and
OQ-10 (gemini naming). Noted in `manifest.json`; a proxy probe resolves them.

## Invariants vs observations

Per cell (`cells/<cell>/cell.json`):
- **`invariants`** — MUST-PASS pins on KNOWN behavior (from the W10 matrix):
  `status: 200`, `route: <expected>`, `n_model_calls_min: 1`. A FAIL here is
  a regression / wiring bug, not new intel.
- **`observe`** — the OQ fields to **capture** (no pass/fail): `all_stream`
  (does the request carry `stream:true`?), `reasoning_present`,
  `reasoning_encrypted_content`, `reasoning_thinking_block`,
  `reasoning_thought_signature`, `reasoning_frames`, `reasoning_sample`,
  `finish`, `budget_trap`, `final_text_nonempty`, `usage`. These are the new
  intel the suite exists to record.

The headline `all_stream` check answers the suite's foundational question:
does `--output-format streaming-json` actually issue a `stream:true` wire
call (vs. a non-streaming call with synthesized output)?

## Run

```
# offline gate (no proxy, no binary): schema + config-patch + SSE parse
python3 smoke/wstream/run.py --selftest

# pilot (the OQ-7 question)
python3 smoke/wstream/run.py --cell grok-resp

# a few at once
python3 smoke/wstream/run.py --cells grok-resp,sol-resp,qwen-resp

# all native / all enabled
python3 smoke/wstream/run.py --native
python3 smoke/wstream/run.py
```

Env: `WSTREAM_BIN` (default `target/release/grok-responses`),
`WSTREAM_UPSTREAM` (default the llm-proxy), `WSTREAM_TIMEOUT_S` (default 180).
The proxy key is read from the ambient `CODEX_LLM_PROXY_KEY` (never echoed).

## Report layout

```
smoke/wstream/report/<UTC-ts>/
  matrix.json                     # consolidated run matrix
  <cell>/
    report.md                     # human summary (invariants + observations)
    result.json                   # full result (analysis + calls + verdict)
    derived-config.toml           # the FROZEN per-run config
    turn_1.ndjson                 # headless stdout (NDJSON events)
    turn_1.stderr
    capture/                      # wiretap2 frame-fidelity capture
      req-NNN.json                # method, path, headers(masked), body
      resp-NNN.jsonl              # status line + one line per SSE frame
      <port>.wiretap-stdout.log
```

## Evidence discipline
- Raw-key sweep = 0 on every file in the report tree (the runner asserts this
  per cell and logs a REDACTION VIOLATION on any hit).
- `store=false` on every responses route (the proxy disallows store=true);
  the runner's prompts never request store.
- Bounded prompt (modular exponentiation) + per-turn kill budget (180s) keep
  proxy spend small.
- Binary-of-record pinned in `manifest.json` (release sha256:12); each run
  re-pins the sha it actually used.
- Worktree is multi-session: **only touch `smoke/wstream/`**. The runner reads
  `~/.grok/config.toml` (read-only source) and writes only under
  `smoke/wstream/report/` + a temp hermetic home (kept in the report dir).

## Provenance (mechanism, copied NOT imported)
- Hermetic home + config-patch surgery + `--no-watch`: pattern from
  `smoke/redteam/run.py` (`_apply_config_patch` incl. SWEEPFIX-64,
  `HermeticHome`, `_apply_no_watch`, `_align_models_cache`).
- Wire capture: `smoke/wiretap/wiretap.py` (wiretap2 — frame-fidelity
  `resp-NNN.jsonl`, auth masking, HTTP/1.1 keep-alive SSE).
- Headless streaming turn: `bin -m <model> -p <prompt>
  --output-format streaming-json --always-approve` (redteam
  `run_headless_turn` pattern).
- L1 env contract (provider vars unset, `GROK_AUTH_EXPIRED=1`, ambient
  `CODEX_LLM_PROXY_KEY` only): `smoke/run-smoke.sh` + `smoke/redteam/run.py`.

Self-contained on purpose: `smoke/redteam/` is the operator's actively-edited
lane (.62 amendments, M6 rig fix in flight), so importing from it would be
fragile. The pattern is copied; the files are independent.

## Extension (next step — the operator's "same mechanism for redteam switch")

The per-cell `api_backend` patch + hermetic home is exactly the seam a
mid-run model/backend switch needs. A future `switch` cell op can:
1. run turn 1 on model A / backend X (this runner's cell),
2. re-derive the hermetic config with a new `model/<id>/api_backend` patch,
3. `--resume <sid>` turn 2 on model B / backend Y,
4. assert the cross-wire projection on the captured turn-2 request.

That generalizes `smoke/xwfix` (cross-wire golden corpus) from a static
fixture set to a live switch-matrix. Enumeration stays selective (the
operator's directive): only the frontier model×backend pairs that exercise a
distinct projection path get a cell.
