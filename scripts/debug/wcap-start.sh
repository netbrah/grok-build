#!/usr/bin/env bash
# Launch the harness-wire-capture proxy in front of grok's Responses upstream.
# Credential is read from ~/.zshrc into the proxy's env and never printed.
set -euo pipefail

SKILL=~/.agents/skills/harness-wire-capture/scripts
export PATH=/usr/bin:/bin:$PATH

# Compound NetApp proxy credential. Sourced, never echoed: the zshrc entry is
# an indirect reference, so the value only resolves after the env file loads.
set +u
# shellcheck disable=SC1090
. ~/.shell_env_common.sh >/dev/null 2>&1 || true
set -u
CODEX_LLM_PROXY_KEY="${APEX_LLM_PROXY_KEY:-${PI_DEV_LLM_PROXY_KEY:-}}"
export CODEX_LLM_PROXY_KEY
if [ -z "$CODEX_LLM_PROXY_KEY" ]; then
  echo "[wcap] no proxy credential resolved" >&2; exit 1
fi

# Upstream = grok's configured base_url with the trailing /v1 removed, because
# the proxy forwards UP + the client's own path (which already carries /v1).
WIRE_UPSTREAM="$(grep -E '^[[:space:]]*base_url' ~/.grok/config.toml | head -1 \
  | sed -E 's/.*"(.*)"/\1/; s#/v1/?$##')"
export WIRE_UPSTREAM
export WIRE_PORT=8788
export WIRE_OUT=/tmp/wirecap

mkdir -p "$WIRE_OUT"
rm -f "$WIRE_OUT"/wire_* 2>/dev/null || true

echo "[wcap] upstream host: $(echo "$WIRE_UPSTREAM" | sed -E 's#https?://##')"
echo "[wcap] key length: ${#CODEX_LLM_PROXY_KEY}"
echo "[wcap] out: $WIRE_OUT"

nohup python3 "$SKILL/wire_capture_proxy.py" >/tmp/wire_proxy.log 2>&1 </dev/null &
disown
sleep 2
if pgrep -f wire_capture_proxy.py >/dev/null; then
  echo "[wcap] proxy ALIVE"
else
  echo "[wcap] proxy DEAD"; cat /tmp/wire_proxy.log; exit 1
fi
