#!/usr/bin/env bash

set -euo pipefail

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

assert_file_contains() {
    local file="$1" expected="$2"
    grep -Fqx "$expected" "$file" || fail "$(basename "$file") missing: $expected"
}

assert_count() {
    local expected="$1" pattern="$2" file="$3" actual
    actual=$(grep -Fc "$pattern" "$file" 2>/dev/null || true)
    [[ "$actual" == "$expected" ]] || fail "expected $expected occurrences of '$pattern' in $file, found $actual"
}

test_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd "$test_dir/../../.." && pwd)
launcher="$repo_root/deploy/apexai/apexai.sh"
installer="$repo_root/deploy/apexai/install.sh"

[[ -f "$launcher" ]] || fail "launcher does not exist: $launcher"
[[ -f "$installer" ]] || fail "installer does not exist: $installer"

tmp_root=$(mktemp -d "${TMPDIR:-/tmp}/apexai-launcher-test.XXXXXX")
trap 'rm -rf "$tmp_root"' EXIT
tmp_root=$(cd "$tmp_root" && pwd)

deployment="$tmp_root/deployment"
apex_root="$tmp_root/apex"
fake_home="$tmp_root/home"
capture="$tmp_root/capture"
mkdir -p "$deployment/bin" "$apex_root/opsec/bin" "$apex_root/apex-cse" "$fake_home" "$capture"

cp "$launcher" "$deployment/apexai.sh"
chmod +x "$deployment/apexai.sh"
: > "$deployment/managed_config.toml"
: > "$apex_root/opsec/bin/.apex-mcp-env.dat"
: > "$apex_root/ca-bundle.pem"

cat > "$apex_root/apex" <<'FAKE_APEX'
#!/usr/bin/env bash
[[ "${APEX_OPSEC_TEST_MODE:-}" == "1" ]] || {
    printf 'fake apex must only be sourced in OPSEC test mode\n' >&2
    return 91 2>/dev/null || exit 91
}
_opsec_mcp_env_decode() {
    [[ -r "$1" ]] || return 1
    printf '%s\n' \
        'export APEX_MCP_PREDECODED=1' \
        'export CIT_MCP_BIN=/fleet/bin/cit-mcp' \
        'export MASTRA_HTTP_URL=https://mastra.example.test/mcp' \
        'export LORE_HTTP_URL=https://lore.example.test/mcp'
}
return 0 2>/dev/null || exit 0
FAKE_APEX

cat > "$apex_root/apex-cse/refresh-aiq-token.sh" <<'FAKE_AIQ'
#!/usr/bin/env bash
printf '%s\n' 'fake-aiq-access-token'
FAKE_AIQ
chmod +x "$apex_root/apex-cse/refresh-aiq-token.sh"

cat > "$deployment/bin/apexai" <<'FAKE_BINARY'
#!/usr/bin/env bash
set -euo pipefail
capture=${APEXAI_TEST_CAPTURE:?}
{
    printf 'GROK_HOME=%s\n' "${GROK_HOME:-}"
    printf 'GROK_MANAGED_CONFIG_PATH=%s\n' "${GROK_MANAGED_CONFIG_PATH:-}"
    printf 'GROK_XAI_API_BASE_URL=%s\n' "${GROK_XAI_API_BASE_URL:-}"
    printf 'GROK_CLIENT_NAME=%s\n' "${GROK_CLIENT_NAME:-}"
    printf 'GROK_CLIENT_VERSION=%s\n' "${GROK_CLIENT_VERSION:-}"
    printf 'XAI_API_KEY=%s\n' "${XAI_API_KEY:-}"
    printf 'APEX_MCP_PREDECODED=%s\n' "${APEX_MCP_PREDECODED:-}"
    printf 'CIT_MCP_BIN=%s\n' "${CIT_MCP_BIN:-}"
    printf 'JIRA_TOKEN=%s\n' "${JIRA_TOKEN:-}"
    printf 'CONFLUENCE_USERNAME=%s\n' "${CONFLUENCE_USERNAME:-}"
    printf 'MASTRA_SSO=%s\n' "${MASTRA_SSO:-}"
    printf 'MASTRA_CREDENTIAL=%s\n' "${MASTRA_CREDENTIAL:-}"
    printf 'AIQ_ACCESS_TOKEN=%s\n' "${AIQ_ACCESS_TOKEN:-}"
    printf 'GH_HOST=%s\n' "${GH_HOST:-}"
    printf 'GH_ENTERPRISE_TOKEN=%s\n' "${GH_ENTERPRISE_TOKEN:-}"
    printf 'SSL_CERT_FILE=%s\n' "${SSL_CERT_FILE:-}"
} > "$capture/env"
printf '%s\n' "$@" > "$capture/args"
touch "$capture/invoked"
FAKE_BINARY
chmod +x "$deployment/bin/apexai"

cat > "$fake_home/.bashrc" <<EOF
export APEX_LLM_PROXY_KEY='fake-proxy-key'
export JIRA_TOKEN='fake-jira-token'
export CONFLUENCE_TOKEN='fake-confluence-token'
export CONFLUENCE_USERNAME='engineer@netapp.com'
export GHE_TOKEN='fake-ghe-token'
export JENKINS_STREAMLINE_USER='engineer'
export JENKINS_STREAMLINE_TOKEN='fake-jenkins-token'
touch '$fake_home/bashrc-was-sourced'
export UNRELATED_APEXAI_VALUE='must-not-be-imported'
EOF

ln -s "$deployment/apexai.sh" "$tmp_root/apexai"
run_output="$tmp_root/run-output"
HOME="$fake_home" \
APEX_LLM_PROXY_KEY= \
JIRA_TOKEN= \
CONFLUENCE_TOKEN= \
CONFLUENCE_USERNAME= \
GHE_TOKEN= \
JENKINS_STREAMLINE_USER= \
JENKINS_STREAMLINE_TOKEN= \
APEXAI_SHARED_APEX_ROOT="$apex_root" \
APEXAI_TEST_CAPTURE="$capture" \
APEXAI_SKIP_KEY_VALIDATE=1 \
    "$tmp_root/apexai" --alpha 'two words' >"$run_output" 2>&1

[[ -f "$capture/invoked" ]] || fail 'fleet binary was not invoked'
[[ ! -e "$fake_home/bashrc-was-sourced" ]] || fail '.bashrc was sourced instead of selectively imported'
[[ ! -s "$run_output" ]] || fail 'successful launcher printed unexpected output'

assert_file_contains "$capture/env" "GROK_HOME=$fake_home/.apexai"
assert_file_contains "$capture/env" "GROK_MANAGED_CONFIG_PATH=$deployment/managed_config.toml"
assert_file_contains "$capture/env" 'GROK_XAI_API_BASE_URL=https://llm-proxy-api.ai.eng.netapp.com/v1'
assert_file_contains "$capture/env" 'GROK_CLIENT_NAME=Apex'
assert_file_contains "$capture/env" 'GROK_CLIENT_VERSION=apexai'
assert_file_contains "$capture/env" 'XAI_API_KEY=fake-proxy-key'
assert_file_contains "$capture/env" 'APEX_MCP_PREDECODED=1'
assert_file_contains "$capture/env" 'CIT_MCP_BIN=/fleet/bin/cit-mcp'
assert_file_contains "$capture/env" 'JIRA_TOKEN=fake-jira-token'
assert_file_contains "$capture/env" 'CONFLUENCE_USERNAME=engineer@netapp.com'
assert_file_contains "$capture/env" 'MASTRA_SSO=fake-proxy-key'
assert_file_contains "$capture/env" 'MASTRA_CREDENTIAL=Bearer fake-proxy-key'
assert_file_contains "$capture/env" 'AIQ_ACCESS_TOKEN=fake-aiq-access-token'
assert_file_contains "$capture/env" 'GH_HOST=github.eng.netapp.com'
assert_file_contains "$capture/env" 'GH_ENTERPRISE_TOKEN=fake-ghe-token'
assert_file_contains "$capture/env" "SSL_CERT_FILE=$apex_root/ca-bundle.pem"

printf '%s\n' '--alpha' 'two words' > "$tmp_root/expected-args"
cmp -s "$tmp_root/expected-args" "$capture/args" || fail 'launcher did not preserve argv'

validation_home="$tmp_root/validation-home"
fake_bin="$tmp_root/fake-bin"
mkdir -p "$validation_home" "$fake_bin"
cat > "$validation_home/.bashrc" <<'EOF'
export APEX_LLM_PROXY_KEY='fresh-proxy-key'
EOF
cat > "$fake_bin/curl" <<'FAKE_CURL'
#!/usr/bin/env bash
case " $* " in
    *'Authorization: Bearer stale-proxy-key'*) printf '403' ;;
    *'Authorization: Bearer fresh-proxy-key'*) printf '200' ;;
    *) printf '000' ;;
esac
FAKE_CURL
chmod +x "$fake_bin/curl"
rm -f "$capture/invoked" "$capture/env"
if ! HOME="$validation_home" \
    PATH="$fake_bin:$PATH" \
    APEX_LLM_PROXY_KEY=stale-proxy-key \
    APEXAI_SHARED_APEX_ROOT="$apex_root" \
    APEXAI_TEST_CAPTURE="$capture" \
        "$deployment/apexai.sh" >"$tmp_root/key-refresh-output" 2>&1; then
    fail 'launcher did not recover a stale proxy key from ~/.bashrc'
fi
[[ -f "$capture/invoked" ]] || fail 'fleet binary was not invoked after proxy-key refresh'
assert_file_contains "$capture/env" 'XAI_API_KEY=fresh-proxy-key'
grep -Fq 'refreshed stale APEX_LLM_PROXY_KEY from ~/.bashrc' "$tmp_root/key-refresh-output" \
    || fail 'launcher did not report the proxy-key refresh'
[[ -f "$validation_home/.apexai/.llm-key-validated" ]] \
    || fail 'launcher did not cache successful proxy-key validation'
if grep -Eq 'stale-proxy-key|fresh-proxy-key' "$tmp_root/key-refresh-output"; then
    fail 'launcher leaked a proxy key in refresh diagnostics'
fi

missing_home="$tmp_root/missing-key-home"
mkdir -p "$missing_home"
: > "$missing_home/.bashrc"
rm -f "$capture/invoked"
if APEX_LLM_PROXY_KEY= HOME="$missing_home" \
    APEXAI_SHARED_APEX_ROOT="$apex_root" \
    APEXAI_TEST_CAPTURE="$capture" \
        "$deployment/apexai.sh" >"$tmp_root/missing-key-output" 2>&1; then
    fail 'launcher accepted an empty APEX_LLM_PROXY_KEY'
fi
[[ ! -e "$capture/invoked" ]] || fail 'launcher invoked binary without APEX_LLM_PROXY_KEY'
grep -Fq 'APEX_LLM_PROXY_KEY' "$tmp_root/missing-key-output" || fail 'missing-key failure is not actionable'
if grep -Fq 'fake-proxy-key' "$tmp_root/missing-key-output"; then
    fail 'launcher leaked a key in diagnostics'
fi

install_home="$tmp_root/install-home"
mkdir -p "$install_home"
: > "$install_home/.bashrc"
: > "$install_home/.cshrc"
HOME="$install_home" APEXAI_NFS_LAUNCHER="$deployment/apexai.sh" \
    bash "$installer" >"$tmp_root/install-output" 2>&1
HOME="$install_home" APEXAI_NFS_LAUNCHER="$deployment/apexai.sh" \
    bash "$installer" >>"$tmp_root/install-output" 2>&1

installed_link="$install_home/.apexai/bin/apexai"
[[ -L "$installed_link" ]] || fail 'installer did not create the launcher symlink'
[[ "$(readlink "$installed_link")" == "$deployment/apexai.sh" ]] || fail 'installer symlink target is wrong'
assert_count 1 '# ApexAI CLI' "$install_home/.bashrc"
assert_count 1 '.apexai/bin' "$install_home/.bashrc"
assert_count 1 '# ApexAI CLI' "$install_home/.bash_profile"
assert_count 2 '.apexai/bin' "$install_home/.cshrc"

printf 'PASS: ApexAI launcher hydration, isolation, identity, key gate, argv, and installer\n'
