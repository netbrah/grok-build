#!/usr/bin/env bash

set -euo pipefail

# Credential material must never be exposed when a caller has shell tracing on.
[[ $- == *x* ]] && set +x

_apx_self=${BASH_SOURCE[0]}
while [[ -L "$_apx_self" ]]; do
    _apx_dir=$(cd "$(dirname "$_apx_self")" && pwd)
    _apx_link=$(readlink "$_apx_self")
    case "$_apx_link" in
        /*) _apx_self=$_apx_link ;;
        *) _apx_self=$_apx_dir/$_apx_link ;;
    esac
done
_apx_root=$(cd "$(dirname "$_apx_self")" && pwd)
_apx_shared_apex_root=${APEXAI_SHARED_APEX_ROOT:-/u/palanisd/tools/apex}
export GROK_HOME="$HOME/.apexai"
mkdir -p "$GROK_HOME"

if [[ -r "$HOME/.bashrc" ]]; then
    _apx_credential_names=(
        APEX_LLM_PROXY_KEY GITHUB_TOKEN GHE_TOKEN JIRA_TOKEN
        CONFLUENCE_TOKEN CONFLUENCE_USERNAME REVIEWBOARD_API_TOKEN
        MASTRA_TOKEN JENKINS_STREAMLINE_USER JENKINS_STREAMLINE_TOKEN
        LOGSCALE_API_TOKEN
    )
    for _apx_name in "${_apx_credential_names[@]}"; do
        [[ -n "${!_apx_name:-}" ]] && continue
        _apx_export=$(grep -E "^[[:space:]]*(export[[:space:]]+)?${_apx_name}=" "$HOME/.bashrc" 2>/dev/null | tail -1 || true)
        if [[ -n "$_apx_export" ]]; then
            eval "$_apx_export"
            export "$_apx_name"
        fi
    done
    unset _apx_credential_names _apx_name _apx_export
fi

_apx_apex_launcher=$_apx_shared_apex_root/apex
if [[ ! -r "$_apx_apex_launcher" ]]; then
    printf 'apexai: APEX OPSEC decoder is unavailable at %s\n' "$_apx_apex_launcher" >&2
    exit 1
fi

# Source only APEX's side-effect-free decoder/function prelude.
export APEX_FAILOVER_COPILOT=false
export APEX_OPSEC_TEST_MODE=1
# shellcheck source=/dev/null
source "$_apx_apex_launcher"
unset APEX_OPSEC_TEST_MODE

if ! declare -F _opsec_mcp_env_decode >/dev/null 2>&1; then
    printf 'apexai: APEX OPSEC decoder function was not loaded\n' >&2
    exit 1
fi

_apx_blob=$_apx_shared_apex_root/opsec/bin/.apex-mcp-env.dat
_apx_decoded=$(_opsec_mcp_env_decode "$_apx_blob" 2>/dev/null) || _apx_decoded=
if [[ -n "$_apx_decoded" ]] && grep -q '^[[:space:]]*export[[:space:]]' <<<"$_apx_decoded"; then
    eval "$_apx_decoded" 2>/dev/null || true
fi
unset _apx_decoded

_apx_key_hash() {
    if command -v sha256sum >/dev/null 2>&1; then
        printf '%s' "$1" | sha256sum | cut -c1-16
    elif command -v shasum >/dev/null 2>&1; then
        printf '%s' "$1" | shasum -a 256 | cut -c1-16
    else
        return 1
    fi
}

# Detect a stale inherited proxy key, refresh it from ~/.bashrc, and cache a
# successful validation for one hour. This is adapted from the fleet APEX
# launcher; failures warn but remain the model client's final authority.
_apx_key_selfheal() {
    [[ -n "${APEXAI_SKIP_KEY_VALIDATE:-${APEX_SKIP_KEY_VALIDATE:-}}" ]] && return 0
    [[ -z "${APEX_LLM_PROXY_KEY:-}" ]] && return 0
    command -v curl >/dev/null 2>&1 || return 0

    local sentinel="$GROK_HOME/.llm-key-validated"
    local now current_hash cached_ts cached_hash age
    now=$(date +%s)
    current_hash=$(_apx_key_hash "$APEX_LLM_PROXY_KEY" 2>/dev/null) || return 0
    if [[ -f "$sentinel" ]]; then
        cached_ts=
        cached_hash=
        IFS=' ' read -r cached_ts cached_hash < "$sentinel" 2>/dev/null || true
        if [[ "$cached_hash" == "$current_hash" && "$cached_ts" =~ ^[0-9]+$ ]]; then
            age=$((now - cached_ts))
            [[ $age -lt 3600 ]] && return 0
        fi
    fi

    local code
    local probe_body='{"model":"gpt-3.5-turbo","max_tokens":1,"messages":[{"role":"user","content":"."}]}'
    local probe_ua='Apex/apexai-launcher-precheck'
    code=$(curl -s --max-time 5 -o /dev/null -w '%{http_code}' \
        -H "User-Agent: $probe_ua" \
        -H "Authorization: Bearer $APEX_LLM_PROXY_KEY" \
        -H 'Content-Type: application/json' \
        -X POST 'https://llm-proxy-api.ai.eng.netapp.com/v1/chat/completions' \
        -d "$probe_body" 2>/dev/null) || code=000

    if [[ "$code" != 403 && -n "$code" && "$code" != 000 ]]; then
        printf '%s %s\n' "$now" "$current_hash" > "$sentinel"
        return 0
    fi

    local fresh_line fresh
    fresh_line=$(grep -E '^[[:space:]]*export[[:space:]]+APEX_LLM_PROXY_KEY=' "$HOME/.bashrc" 2>/dev/null | tail -1 || true)
    fresh=
    if [[ -n "$fresh_line" ]]; then
        fresh=$(
            unset APEX_LLM_PROXY_KEY
            eval "$fresh_line"
            printf '%s' "${APEX_LLM_PROXY_KEY:-}"
        )
    fi
    if [[ -n "$fresh" && "$fresh" != "$APEX_LLM_PROXY_KEY" ]]; then
        export APEX_LLM_PROXY_KEY=$fresh
        code=$(curl -s --max-time 5 -o /dev/null -w '%{http_code}' \
            -H "User-Agent: $probe_ua" \
            -H "Authorization: Bearer $APEX_LLM_PROXY_KEY" \
            -H 'Content-Type: application/json' \
            -X POST 'https://llm-proxy-api.ai.eng.netapp.com/v1/chat/completions' \
            -d "$probe_body" 2>/dev/null) || code=000
        if [[ "$code" != 403 && -n "$code" && "$code" != 000 ]]; then
            current_hash=$(_apx_key_hash "$APEX_LLM_PROXY_KEY" 2>/dev/null) || return 0
            if [[ -n "${TMUX:-}" ]] && command -v tmux >/dev/null 2>&1; then
                tmux setenv -g APEX_LLM_PROXY_KEY "$APEX_LLM_PROXY_KEY" 2>/dev/null || true
            fi
            printf '%s %s\n' "$now" "$current_hash" > "$sentinel"
            printf '\033[33m[apexai] refreshed stale APEX_LLM_PROXY_KEY from ~/.bashrc\033[0m\n' >&2
            return 0
        fi
    fi

    printf '\033[31m[apexai] APEX_LLM_PROXY_KEY rejected by proxy (HTTP %s) and ~/.bashrc did not provide a valid replacement.\033[0m\n' "$code" >&2
    printf '\033[31m         Mint a new key: https://llm-proxy-web.ai.eng.netapp.com/Manage_keys\033[0m\n' >&2
}
_apx_key_selfheal
unset -f _apx_key_selfheal _apx_key_hash

if [[ -z "${APEX_LLM_PROXY_KEY:-}" ]]; then
    printf 'apexai: APEX_LLM_PROXY_KEY is missing; export it from ~/.bashrc\n' >&2
    exit 1
fi

export MASTRA_SSO="$APEX_LLM_PROXY_KEY"
export MASTRA_CREDENTIAL="${MASTRA_TOKEN:-Bearer ${MASTRA_SSO}}"

_apx_prepare_confluence_username() {
    [[ -n "${CONFLUENCE_USERNAME:-}" ]] && {
        export CONFLUENCE_USERNAME
        return 0
    }

    local login display first last
    local -a parts
    login=$(id -un 2>/dev/null || whoami 2>/dev/null || true)
    display=
    first=
    last=
    parts=()
    if [[ -n "$login" ]] && command -v getent >/dev/null 2>&1; then
        display=$(getent passwd "$login" 2>/dev/null | awk -F: 'NR == 1 { split($5, p, ","); print p[1] }' || true)
        read -r -a parts <<<"$display"
        if (( ${#parts[@]} >= 2 )); then
            first=$(printf '%s' "${parts[0]}" | tr '[:upper:]' '[:lower:]' | tr -cd '[:alnum:]._-')
            last=$(printf '%s' "${parts[${#parts[@]}-1]}" | tr '[:upper:]' '[:lower:]' | tr -cd '[:alnum:]._-')
        fi
    fi
    if [[ -n "$first" && -n "$last" ]]; then
        export CONFLUENCE_USERNAME="${first}.${last}@netapp.com"
    elif [[ -n "$login" ]]; then
        export CONFLUENCE_USERNAME="${login}@netapp.com"
    fi
}
_apx_prepare_confluence_username
unset -f _apx_prepare_confluence_username

_apx_aiq_refresh=$_apx_shared_apex_root/apex-cse/refresh-aiq-token.sh
if [[ -x "$_apx_aiq_refresh" ]]; then
    _apx_aiq_token=$(bash "$_apx_aiq_refresh" 2>/dev/null) || _apx_aiq_token=
    if [[ -n "$_apx_aiq_token" ]]; then
        export AIQ_ACCESS_TOKEN=$_apx_aiq_token
    fi
    unset _apx_aiq_token
fi

export GH_HOST=github.eng.netapp.com
if [[ -n "${GHE_TOKEN:-}" ]]; then
    export GH_ENTERPRISE_TOKEN=$GHE_TOKEN
fi

_apx_ca=
for _apx_candidate in \
    "$_apx_shared_apex_root/ca-bundle.pem" \
    /etc/pki/tls/certs/ca-bundle.crt \
    /etc/ssl/certs/ca-certificates.crt \
    /etc/ssl/cert.pem; do
    if [[ -f "$_apx_candidate" ]]; then
        _apx_ca=$_apx_candidate
        break
    fi
done
if [[ -n "$_apx_ca" ]]; then
    export NODE_EXTRA_CA_CERTS="${NODE_EXTRA_CA_CERTS:-$_apx_ca}"
    export SSL_CERT_FILE=$_apx_ca
    export REQUESTS_CA_BUNDLE=$_apx_ca
    export CURL_CA_BUNDLE=$_apx_ca
fi

_apx_proxy=${APEXAI_CORP_PROXY:-http://svc-proxy.cls.eng.netapp.com:3128/}
export HTTPS_PROXY=$_apx_proxy
export https_proxy=$_apx_proxy
export HTTP_PROXY=$_apx_proxy
export http_proxy=$_apx_proxy
export NO_GRPC_PROXY='*'
_apx_no_proxy='.eng.netapp.com,.englab.netapp.com,localhost,127.0.0.1'
if [[ -n "${NO_PROXY:-}" ]]; then
    _apx_no_proxy="$NO_PROXY,$_apx_no_proxy"
fi
export NO_PROXY=$_apx_no_proxy
export no_proxy=$_apx_no_proxy

export GROK_MANAGED_CONFIG_PATH="$_apx_root/managed_config.toml"
export GROK_XAI_API_BASE_URL=https://llm-proxy-api.ai.eng.netapp.com/v1
export GROK_CLIENT_NAME=Apex
export GROK_CLIENT_VERSION=apexai
export XAI_API_KEY=$APEX_LLM_PROXY_KEY

_apx_binary=$_apx_root/bin/apexai
if [[ ! -r "$GROK_MANAGED_CONFIG_PATH" ]]; then
    printf 'apexai: managed config is unavailable at %s\n' "$GROK_MANAGED_CONFIG_PATH" >&2
    exit 1
fi
if [[ ! -x "$_apx_binary" ]]; then
    printf 'apexai: fleet binary is unavailable at %s\n' "$_apx_binary" >&2
    exit 1
fi

exec "$_apx_binary" "$@"
