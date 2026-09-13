#!/bin/zsh
# Model-matrix diagnostic runner (roadmap item 11 pre-pass; 2026-09-12).
# Headless, one model at a time, live proxy. Smoke-use quota (operator-authorized):
# one short prompt per model. Per-model wire evidence captured (RUST_LOG=info stderr)
# for error triage. At the info level the product logs only auth PREFIXES
# (client_post) — the session_setup credential leak is DEBUG (A6-F2), excluded here;
# still grep the raw key out of every log before citing it.
# Usage: smoke/run-matrix.sh <case-file>
#   case-file lines: name|model|backend   (backend: - = stock resolution, else "responses"|"messages"|"chat_completions")
#   blank lines and #-comments skipped.
# Env: OUTDIR (default /tmp/matrix-20260912), TIMEOUT (default 90s per case),
#      GROK_MATRIX_PROXY_BASE (default the ambient llm-proxy).
set -u
cd "${0:A:h}/.."
BIN=${BIN:-./target/debug/grok-responses}
TIMEOUT=${TIMEOUT:-90}
OUTDIR=${OUTDIR:-/tmp/matrix-20260912}
[ -x "$BIN" ] || { echo "binary missing: $BIN (cargo build --bin grok-responses)" >&2; exit 2; }
[ -n "${CODEX_LLM_PROXY_KEY:-}" ] || { echo "CODEX_LLM_PROXY_KEY not set" >&2; exit 2; }
[ $# -ge 1 ] || { echo "usage: run-matrix.sh <case-file>" >&2; exit 2; }
PROXY_BASE=${GROK_MATRIX_PROXY_BASE:-https://llm-proxy-api.ai.eng.netapp.com/v1}
mkdir -p "$OUTDIR"
: > "$OUTDIR/manifest.txt"

run_case() {
  local name=$1 model=$2 backend=$3
  local home tmp rc pid wd text
  home=$(mktemp -d "${TMPDIR:-/tmp}/matrix-home.XXXXXX")
  {
    echo "[endpoints]"
    echo "models_base_url = \"$PROXY_BASE\""
    echo "default_api_backend = \"responses\""
    echo "default_env_key = \"CODEX_LLM_PROXY_KEY\""
    echo "default_context_window = 256000"
    echo "default_model_family = \"codex\""
    echo "default_agent_type = \"grok-build-plan\""
    if [ "$backend" != "-" ]; then
      printf '\n[model."%s"]\napi_backend = "%s"\n' "$model" "$backend"
    fi
  } > "$home/config.toml"
  tmp="$OUTDIR/$name"
  ( env -u OPENAI_API_KEY -u OPENAI_BASE_URL -u ANTHROPIC_API_KEY -u ANTHROPIC_BASE_URL \
      GROK_AUTH_EXPIRED=1 GROK_HOME="$home" RUST_LOG=info \
      "$BIN" -m "$model" -p "Reply with exactly: MATRIX-OK" --output-format json --always-approve \
      >"$tmp.out" 2>"$tmp.err" ) &
  pid=$!
  ( sleep "$TIMEOUT"; kill -9 "$pid" 2>/dev/null ) &
  wd=$!
  wait "$pid" 2>/dev/null
  rc=$?
  kill "$wd" 2>/dev/null
  wait "$wd" 2>/dev/null
  rm -rf "$home"
  if [ $rc -ne 0 ]; then
    echo "FAIL  $name (model=$model backend=$backend exit=$rc)" | tee -a "$OUTDIR/manifest.txt"
    grep -a -m2 -aE 'error|Error|invalid|status|4[0-9][0-9]|5[0-9][0-9]' "$tmp.err" 2>/dev/null | head -2 | sed 's/^/      /' | tee -a "$OUTDIR/manifest.txt"
    return 1
  fi
  text=$(python3 -c '
import json,sys
raw=open(sys.argv[1]).read()
s=raw.find("{")
try: d=json.loads(raw[s:])
except Exception: print("(unparseable)")
else: print(d.get("text") or d.get("stopReason") or "")' "$tmp.out" 2>/dev/null)
  if [[ "$text" == *MATRIX-OK* ]]; then
    echo "PASS  $name (model=$model backend=$backend)" | tee -a "$OUTDIR/manifest.txt"
    return 0
  fi
  echo "FAIL  $name (model=$model backend=$backend no MATRIX-OK in: ${text:0:120})" | tee -a "$OUTDIR/manifest.txt"
  return 1
}

fails=0; total=0
while IFS='|' read -r name model backend; do
  [[ -z "$name" || "$name" == \#* ]] && continue
  total=$((total+1))
  run_case "$name" "$model" "$backend" || fails=$((fails+1))
done < "$1"
echo "---"
echo "MATRIX DONE: $((total-fails))/$total pass"
