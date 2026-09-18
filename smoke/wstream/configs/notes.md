# wstream config derivation (the "frozen config" seam)

Each cell's config is **derived, not hand-copied**. This keeps the suite
robust to live-config drift (the operator edits `~/.grok/config.toml`
mid-campaign) and avoids maintaining 10 full 293-line config copies.

## Derivation rule (per cell)

```
hermetic_config = live_config(~/.grok/config.toml)
                - features.turn_summary = false     (cost: display-only side-calls)
                + model/<id>/api_backend = <cell.backend>   (THE cross-product lever)
                + [mcp_servers.codegraph].args += "--no-watch"   (S-8: no watcher into .codegraph/)
                ~ base_url / models_base_url -> http://127.0.0.1:<wiretap>/v1
catalog_cache  = live models_cache.json aligned to the hermetic scope
                (origin / identity / renewed_at) so -m <model> resolves offline
```

The derived file is written to `<report>/<cell>/derived-config.toml` for every
run — that is the FROZEN config of record for that cell/run (diff it against
`~/.grok/config.toml` to see exactly what flipped: one `api_backend` line, the
turn_summary line, the no-watch arg, and the two base_url rewrites).

## The cross-product lever

`api_backend` is a per-model config field (config.toml `[model."<id>"]`), with
`default_api_backend = "responses"` in `[endpoints]` as the fallback. Setting
`model/<id>/api_backend` for a cell forces the wire that model's request hits:

| api_backend | wire | live-config frontier rows using it |
|---|---|---|
| `responses` | `/v1/responses` | grok-4.6, gpt-5.6-sol, glm-5.2, qwen3.8-27b, gemini-* (native) |
| `messages` | `/v1/messages` | claude-* (native) |
| `chat_completions` | `/v1/chat/completions` | gemma-4-31b (native) |

**Native cells** set the backend the live config already uses (explicit, so the
cell is self-documenting and immune to live drift). **Override cells** set a
DIFFERENT backend than the model's default — that is the cross-dialect arm
(`sol-msg` puts azure sol on `/messages`, `grok-chat` puts grok on
`/chat/completions`, etc.). The W10 matrix (`provenance/wire-topology.md` §4)
tells us what litellm does to each override (azure→chat, vllm→responses
bridge, gemini→synthetic) — the streaming suite now shows what the STREAMED
frames look like on each.

## What we deliberately do NOT freeze

- **Full config copies.** The live config is the source of truth; we patch one
  field per cell. A frozen copy would drift the moment the operator adds a
  model row.
- **auth.json.** The hermetic home skips it on purpose (dogfood.v2 HT-1.2-A5):
  the wiretap base_url is loopback → treated first-party → a cached
  session-credential would re-activate the token-refresh gate and 403. Without
  auth.json the harness selects `xai.api_key` and rides `env_key`
  (`CODEX_LLM_PROXY_KEY`) — the same auth semantics the live session uses.

## Reuse by the redteam switch lane

The identical derivation (live config + `model/<id>/api_backend` patch +
hermetic home + wiretap) is what a mid-run switch cell needs: run turn 1 on
model A / backend X, re-derive with model B / backend Y, `--resume` turn 2,
assert the cross-wire projection on the captured turn-2 request. See README
"Extension".
