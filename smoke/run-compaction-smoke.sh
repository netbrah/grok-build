#!/bin/zsh
# P2.1 live-acceptance (Layer 1): codex remote compaction v2 over /responses.
# Three headless turns in an ISOLATED GROK_HOME (tempdir, P1 [endpoints] config,
# key env-only):
#   1. context turn (gpt-5.6-sol) — build real history
#   2. /compact — must take the v2 REMOTE path (v2 log lines on stderr)
#   3. follow-up — the compacted carrier history must replay coherently
# KNOWN GATE (D-ENC, ledger 2026-09-12): the proxy LBs /responses across three
# Azure regions with no session affinity, so the compacted cmp_ carrier cannot
# decrypt cross-region and t3 fails with the designed friendly error ("history
# incompatible with the current model"). t3 goes green only once the proxy
# gains /responses session affinity. t1/t2 are the D-ENC-free assertions.
# Launch form is the TRUE headless one: -p <prompt> (the positional-prompt form
# is the TUI launch form and requires a controlling terminal; headless needs
# -p/--single). stdout carries the JSON result, stderr the RUST_LOG lines.
# Env contract mirrors run-smoke.sh. Usage: smoke/run-compaction-smoke.sh
set -u
cd "${0:A:h}/.."
BIN=${BIN:-./target/debug/grok-responses}
TIMEOUT=${TIMEOUT:-180}
PROXY=${GROK_L2_PROXY_BASE_URL:-https://llm-proxy-api.ai.eng.netapp.com/v1}
OUT=${OUT:-/tmp/grok-p21-acceptance}
[ -x "$BIN" ] || { echo "binary missing: $BIN (cargo build -p xai-grok-pager-bin)" >&2; exit 2; }
[ -n "${CODEX_LLM_PROXY_KEY:-}" ] || { echo "CODEX_LLM_PROXY_KEY not set (required)" >&2; exit 2; }
rm -rf "$OUT"; mkdir -p "$OUT"

GH=$(mktemp -d "${TMPDIR:-/tmp}/grok-compact-smoke.XXXXXX")
trap 'rm -rf "$GH"' EXIT
cat > "$GH/config.toml" <<CFG
[endpoints]
models_base_url = "$PROXY"
default_api_backend = "responses"
default_env_key = "CODEX_LLM_PROXY_KEY"
default_context_window = 256000
default_model_family = "codex"
default_agent_type = "grok-build-plan"
CFG

run_turn() { # name continue(0|1) prompt outfile   (stdout=JSON -> outfile, stderr -> outfile.err)
  local name=$1 cont=$2 prompt=$3 out=$4 rc pid wd
  local args=(-m gpt-5.6-sol -p "$prompt")
  [ "$cont" = 1 ] && args+=(-c)
  args+=(--output-format json --always-approve)
  local tmp; tmp=$(mktemp "${TMPDIR:-/tmp}/grok-p21.XXXXXX")
  ( env -u OPENAI_API_KEY -u OPENAI_BASE_URL -u ANTHROPIC_API_KEY -u ANTHROPIC_BASE_URL \
      GROK_HOME="$GH" GROK_AUTH_EXPIRED=1 RUST_LOG=info \
      "$BIN" "${args[@]}" >"$tmp" 2>"$tmp.err" ) &
  pid=$!
  ( sleep "$TIMEOUT"; kill -9 "$pid" 2>/dev/null ) & wd=$!
  wait "$pid" 2>/dev/null; rc=$?
  kill "$wd" 2>/dev/null; wait "$wd" 2>/dev/null
  mv "$tmp" "$out"; mv "$tmp.err" "$out.err"
  return $rc
}

extract_text() {
  python3 -c '
import json, sys
raw = open(sys.argv[1]).read()
start = raw.find("{")
try:
    d = json.loads(raw[start:])
    print(d.get("text") or d.get("stopReason") or "")
except Exception:
    print("__UNPARSEABLE__" + raw[:200])
' "$1"
}

fails=0
echo "== turn 1: context (new headless session) =="
run_turn t1 0 "Write a 400-500 word technical overview of LLM provider routing across three API backends (openai responses, anthropic messages, chat completions). Then list exactly three design challenges, each with a one-sentence rationale, numbered 1-3." "$OUT/t1.out"
rc=$?
t1=$(extract_text "$OUT/t1.out")
if [ $rc -ne 0 ] || [[ "$t1" == __UNPARSEABLE__* ]] || [ -z "$t1" ]; then
  echo "FAIL  t1-context (exit $rc): $(head -c 400 "$OUT/t1.out")"; fails=$((fails+1))
else
  echo "PASS  t1-context (${#t1} chars)"
fi

echo "== turn 2: /compact (-c; v2 remote path) =="
run_turn t2 1 "/compact" "$OUT/t2.out"
rc=$?
v2_stream=$(grep -c 'Codex remote compaction v2 stream completed' "$OUT/t2.out.err" 2>/dev/null || true)
v2_install=$(grep -c 'installed Codex remote-compaction v2 replacement history' "$OUT/t2.out.err" 2>/dev/null || true)
t2=$(extract_text "$OUT/t2.out")
if [ $rc -ne 0 ]; then
  echo "FAIL  t2-compact (exit $rc): $(head -c 400 "$OUT/t2.out")"; fails=$((fails+1))
elif [ "$v2_stream" -lt 1 ] || [ "$v2_install" -lt 1 ]; then
  echo "FAIL  t2-compact (v2 path not taken: stream_completed=$v2_stream install=$v2_install)"; fails=$((fails+1))
else
  echo "PASS  t2-compact (v2 stream + install logs present; reply: ${t2:0:120})"
fi

echo "== turn 3: follow-up on compacted history (-c) =="
run_turn t3 1 "In one sentence, what was design challenge number one? Answer only with that sentence." "$OUT/t3.out"
rc=$?
t3=$(extract_text "$OUT/t3.out")
if [ $rc -ne 0 ] || [[ "$t3" == __UNPARSEABLE__* ]] || [ -z "$t3" ]; then
  echo "FAIL  t3-followup (exit $rc): $(head -c 400 "$OUT/t3.out")"; fails=$((fails+1))
else
  echo "PASS  t3-followup (reply: ${t3:0:200})"
fi

echo "---"
if [ $fails -eq 0 ]; then
  echo "COMP-ACT GREEN: 3/3 turns (outputs in $OUT; binary $(git rev-parse --short HEAD))"
  exit 0
else
  echo "COMP-ACT RED: $fails failure(s) (outputs in $OUT)"
  exit 1
fi
