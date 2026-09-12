#!/bin/zsh
# P1+ live-acceptance smoke suite (Layer 1 of smoke-harness-spec.md).
# Runs the built headless binary against the live proxy, one scenario at a time.
# Env contract: unsets ambient provider vars; GROK_AUTH_EXPIRED=1 declines the login refresh.
# Usage: smoke/run-smoke.sh [scenario-name ...]   (default: all)
set -u
cd "${0:A:h}/.."
BIN=${BIN:-./target/debug/grok-responses}
TIMEOUT=${TIMEOUT:-120}
[ -x "$BIN" ] || { echo "binary missing: $BIN (cargo build --bin grok-responses)" >&2; exit 2; }

# name | model | prompt | expect-substring | agents-json(- for none)
CASES=(
  "a-hydrated|qwen3.8-27b|Reply with exactly: P1-OK|P1-OK|-"
  "control|gpt-5.6-sol|Reply with exactly: CONTROL-OK|CONTROL-OK|-"
  "b-messages|claude-sonnet-5|Reply with exactly: MSG-OK|MSG-OK|-"
  "c-subagent|qwen3.8-27b|Spawn the echo subagent and ask it to reply with exactly: CHILD-OK|CHILD-OK|{\"echo\":{\"model\":\"qwen3.8-27b\",\"prompt\":\"You are an echo agent. Reply with exactly what the parent asks for, nothing else.\"}}"
)

run_case() {
  local name=$1 model=$2 prompt=$3 expect=$4 agents=$5
  local args=(-m "$model" -p "$prompt" --output-format json --always-approve)
  [ "$agents" != "-" ] && args+=(--agents "$agents")
  local out rc pid wd tmp
  # No GNU timeout on stock macOS: background the case and kill it via a sleep watchdog.
  tmp=$(mktemp "${TMPDIR:-/tmp}/grok-smoke.XXXXXX")
  ( env -u OPENAI_API_KEY -u OPENAI_BASE_URL -u ANTHROPIC_API_KEY -u ANTHROPIC_BASE_URL \
      GROK_AUTH_EXPIRED=1 "$BIN" "${args[@]}" >"$tmp" 2>&1 ) &
  pid=$!
  ( sleep "$TIMEOUT"; kill -9 "$pid" 2>/dev/null ) &
  wd=$!
  wait "$pid" 2>/dev/null
  rc=$?
  kill "$wd" 2>/dev/null
  wait "$wd" 2>/dev/null
  out=$(cat "$tmp")
  find "$tmp" -delete
  if [ $rc -ne 0 ]; then
    echo "FAIL  $name (exit $rc): ${out:0:400}"
    return 1
  fi
  local text
  text=$(printf '%s\n' "$out" | python3 -c '
import json, sys
raw = sys.stdin.read()
start = raw.find("{")
try:
    d = json.loads(raw[start:])
except Exception:
    print(raw[:200])
    raise SystemExit(3)
print(d.get("text") or d.get("stopReason") or "")' 2>/dev/null)
  if [ $? -ne 0 ] || [[ "$text" != *"$expect"* ]]; then
    echo "FAIL  $name (no '$expect' in output): ${out:0:400}"
    return 1
  fi
  echo "PASS  $name"
  return 0
}

selected=()
if [ $# -gt 0 ]; then
  for want in "$@"; do
    for c in "${CASES[@]}"; do
      [ "${c%%|*}" = "$want" ] && selected+=("$c")
    done
  done
else
  selected=("${CASES[@]}")
fi

fails=0
for c in "${selected[@]}"; do
  IFS='|' read -r name model prompt expect agents <<< "$c"
  run_case "$name" "$model" "$prompt" "$expect" "$agents" || fails=$((fails+1))
done
echo "---"
if [ $fails -eq 0 ]; then
  echo "SMOKE GREEN: ${#selected[@]}/${#selected[@]} scenarios"
  exit 0
fi
echo "SMOKE RED: $fails failure(s)"
exit 1
