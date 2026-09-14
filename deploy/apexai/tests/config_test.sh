#!/usr/bin/env bash

set -euo pipefail

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

test_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd "$test_dir/../../.." && pwd)
config="$repo_root/deploy/apexai/managed_config.toml"
source_json=${APEXAI_APEX_SETTINGS:-/u/palanisd/tools/apex/system-settings.json}

[[ -f "$config" ]] || fail "managed config does not exist: $config"
[[ -f "$source_json" ]] || fail "APEX settings snapshot does not exist: $source_json"

python_bin=${APEXAI_PYTHON:-}
if [[ -z "$python_bin" ]]; then
    if command -v python3.11 >/dev/null 2>&1; then
        python_bin=python3.11
    else
        python_bin=python3
    fi
fi

"$python_bin" - "$config" "$source_json" <<'PY'
import json
import re
import sys
import tomllib

config_path, source_path = sys.argv[1:]
with open(config_path, "rb") as fh:
    config = tomllib.load(fh)
with open(source_path, encoding="utf-8") as fh:
    source = json.load(fh)

expected_names = {
    "activeiq", "asup", "brewbot", "cit", "clangd-rs", "confluence",
    "coretool", "coverage-rs", "ghe", "jira-ngage", "lore",
    "mastra-search", "ontap-cluster", "ontap-dev", "ontap-sdk",
    "presubmit-validate", "pyrefly", "reviewboard", "smartsolve",
}
actual = config.get("mcp_servers", {})
assert set(actual) == expected_names, (sorted(actual), sorted(expected_names))
assert set(source["mcpServers"]) == expected_names

def env_ref(value):
    if isinstance(value, str):
        match = re.fullmatch(r"\$\{?([A-Za-z_][A-Za-z0-9_]*)\}?", value)
        if match:
            return "${" + match.group(1) + "}"
    return value

for name in sorted(expected_names):
    src = source["mcpServers"][name]
    got = actual[name]
    for field in ("command", "url"):
        if field in src:
            assert got.get(field) == env_ref(src[field]), (name, field, got.get(field), src[field])
        else:
            assert field not in got, (name, field)
    assert got.get("args", []) == [env_ref(v) for v in src.get("args", [])], name
    assert got.get("env", {}) == {k: env_ref(v) for k, v in src.get("env", {}).items()}, name
    assert got.get("headers", {}) == {k: env_ref(v) for k, v in src.get("headers", {}).items()}, name
    timeout = src["timeout"] // 1000
    assert src["timeout"] % 1000 == 0, name
    assert got.get("startup_timeout_sec") == timeout, name
    assert got.get("tool_timeout_sec") == timeout, name
    assert "trust" not in got and "description" not in got, name

expected_disabled = {
    name: entry["excludeTools"]
    for name, entry in source["mcpServers"].items()
    if entry.get("excludeTools")
}
assert config.get("disabled_mcp_tools", {}) == expected_disabled

model = config["model"]["grok-4.6"]
assert model == {
    "base_url": "https://llm-proxy-api.ai.eng.netapp.com/v1",
    "api_backend": "responses",
    "env_key": "APEX_LLM_PROXY_KEY",
    "context_window": 500000,
    "supports_backend_search": False,
}
assert config["models"]["default"] == "grok-4.6"
assert config["models"]["default_reasoning_effort"] == "xhigh"
assert config["skills"]["paths"] == ["/x/eng/apex/apexai/bundled"]
assert config["paths"]["extra_rule_dirs"] == ["/x/eng/apex/apexai/rules"]
assert config["hints"] == {
    "new_session_worktree_mode": "never",
    "fork_worktree_mode": "never",
}
assert config["features"] == {"remote_compaction_v2": False}

credential_name = re.compile(r"TOKEN|KEY|SECRET|PASSWORD|AUTH|COOKIE", re.I)
allowed_literal_paths = {"AIQ_TOKEN_COMMAND", "SMARTSOLVE_COOKIE_PATH"}
for name, server in actual.items():
    for section in ("env", "headers"):
        for key, value in server.get(section, {}).items():
            if credential_name.search(key) and key not in allowed_literal_paths:
                assert isinstance(value, str) and value.startswith("${"), (name, key)

print("structured config parity: 19 MCPs, timeouts, env, headers, and deny lists")
PY

tmp_root=$(mktemp -d "${TMPDIR:-/tmp}/apexai-config-test.XXXXXX")
trap 'rm -rf "$tmp_root"' EXIT
mkdir -p "$tmp_root/home/.apexai"

original_cargo_home=${CARGO_HOME:-$HOME/.cargo}
original_rustup_home=${RUSTUP_HOME:-$HOME/.rustup}
export HOME="$tmp_root/home"
export CARGO_HOME=$original_cargo_home
export RUSTUP_HOME=$original_rustup_home
export GROK_HOME="$HOME/.apexai"
export GROK_MANAGED_CONFIG_PATH="$config"
export APEX_LLM_PROXY_KEY=dummy
export AIQ_ACCESS_TOKEN=dummy
export BREWBOT_API_KEY=dummy
export CONFLUENCE_TOKEN=dummy
export CONFLUENCE_USERNAME=engineer@netapp.com
export GHE_TOKEN=dummy
export JENKINS_STREAMLINE_TOKEN=dummy
export JENKINS_STREAMLINE_USER=engineer
export JIRA_TOKEN=dummy
export MASTRA_CREDENTIAL='Bearer dummy'
export REVIEWBOARD_API_TOKEN=dummy
export CIT_MCP_BIN=/bin/true
export CLANGD_BIN=/bin/true
export CONFLUENCE_MCP_BIN=/bin/true
export CORETOOL_BIN=/bin/true
export COVERAGE_BIN=/bin/true
export GHE_MCP_BIN=/bin/true
export JIRA_NGAGE_MCP_BIN=/bin/true
export ONTAP_DEV_BIN=/bin/true
export PRESUBMIT_VALIDATE_MCP_BIN=/bin/true
export PYREFLY_INDEX_BIN=/bin/true
export REVIEWBOARD_MCP_BIN=/bin/true
export SMARTSOLVE_FLEET_BIN=/bin/true
export PYREFLY_INDEX_ADDR=http://127.0.0.1:1
export LORE_HTTP_URL=http://127.0.0.1:2/mcp
export MASTRA_HTTP_URL=http://127.0.0.1:3/mcp

inspect_json="$tmp_root/inspect.json"
if [[ -n "${APEXAI_TEST_BIN:-}" ]]; then
    "$APEXAI_TEST_BIN" inspect --json > "$inspect_json"
else
    cargo run --quiet -p xai-grok-pager-bin -- inspect --json > "$inspect_json"
fi

jq -e '.mcpServers | length == 19' "$inspect_json" >/dev/null || fail 'runtime did not load 19 MCP servers'
jq -e '.mcpConfigProblems // [] | length == 0' "$inspect_json" >/dev/null || fail 'runtime reported invalid MCP configuration'
jq -e --arg path "$config" '.configSources.layers[] | select(.role == "managed-path" and .path == $path)' "$inspect_json" >/dev/null \
    || fail 'inspect did not report the launcher-selected managed config'

printf 'PASS: ApexAI managed config parity and runtime parsing\n'
