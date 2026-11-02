#!/usr/bin/env python3
"""HT-1 L3 red-team runner: compaction x cross-model x resume, wire-definitive.

PROVENANCE
----------
Pattern source: the L2 ACP harness
(`crates/codegen/xai-grok-shell/tests/responses_acceptance.rs` +
`tests/acp_harness/mod.rs`: NDJSON-RPC 2.0 over stdio, `session/new` with
`_meta.modelId`, live turns driven against the real proxy, auth-failure
signature asserts) and the 01a09be2 over-capacity local-compaction
forensics (incident session dir `~/.grok/sessions/.../01a09be2-75ee-...`:
compaction_requests artifacts with `summary: null` +
`error: "compact failed: stream error (unknown): litellm.APIError: Response
API in-stream error"`). The L1 env contract (provider vars unset,
GROK_AUTH_EXPIRED=1, ambient CODEX_LLM_PROXY_KEY only) is taken from
`smoke/run-smoke.sh`.

Stdlib python3 only. NO pip deps. Rust-free by design: drives the built
pager binary in headless mode (`-p`/`-m`/`--resume`/`--output-format`) and
ACP-stdio mode (`grok agent stdio`, RECON-pinned NDJSON-RPC framing).

Usage:
  python3 smoke/redteam/run.py [case-id ...] [--budget N] [--no-wirecap]
                               [--rows a,b] [--keep-home]
  python3 smoke/redteam/run.py --selftest

--selftest runs the offline gate (apex-ayl.22 D-1/D-2): the check_schema
draft-07 dual-path gate (jsonschema when importable, else the
hand-rolled keyword subset) over the NEW (schema_version) cases, the
in-tree case-contract gate over the whole set, and the offline test
suite (smoke/redteam/test_run.py — step-machine desync replay, 424 BPS
init-budget sims, the case-set contract validation, and the 16-T
smoke-matrix surface) and exits. No proxy key, no binary, no live
calls: this is the offline gate for runner changes.
No case args = full matrix (disabled cases skipped). Report ->
smoke/redteam/report/<UTC-ts>/{report.md,report.json,<case>/...}.

Case-file contract: see smoke/redteam/cases/*.json and spec §4.
"""
import argparse
import codecs
import copy
import glob as globmod
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import tomllib
import urllib.parse
import urllib.request
import uuid
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
WIRETAP = os.path.join(HERE, "..", "wiretap", "wiretap.py")
CASES_DIR = os.path.join(HERE, "cases")
REPORT_ROOT = os.path.join(HERE, "report")
# apex-ayl.22 D-1: the frozen case schema (byte copy of
# grok/plans/c21c22/schema/case.schema.json; AC-2 pins byte-identity).
SCHEMA_PATH = os.path.join(HERE, "case.schema.json")

# 2026-09-19: release is the binary of record (debug build removed per
# lean-down order); GROK_BIN env overrides.
DEFAULT_BIN = os.environ.get("GROK_BIN") or os.path.join(
    REPO_ROOT, "target", "release", "grok-responses")
DEFAULT_LIVE_HOME = os.path.expanduser("~/.grok")
DEFAULT_UPSTREAM = "https://llm-proxy-api.ai.eng.netapp.com"

# MA-2 G18 negative evidence pinned as a REGRESSION PIN (flips at MA-3).
V2_TOOL_NAMES = [
    "spawn_agent", "send_message", "followup_task",
    "list_agents", "wait_agent", "interrupt_agent",
]
V1_SPAWN_TOOL = "spawn_subagent"

# Header fields the runner scrubs when embedding log slices in reports.
PROVIDER_VARS_UNSET = [
    "OPENAI_API_KEY", "OPENAI_BASE_URL",
    "ANTHROPIC_API_KEY", "ANTHROPIC_BASE_URL",
]

# REDTEAM-REFRESH T3 (r3 §4.3.1 as amended by r3a A5/A6): the declared
# credential set, stated ONCE. The 7 names are every variable a live config
# can route a request with — a row `env_key`, an `env_http_headers` variable,
# and the product fallback chain that a row declaring neither resolves
# through (auth_method.rs:13-15, :30-34). The swept subset is this tuple
# minus `SWEEP_EXCLUDED_NAMES`: `GROK_DIRECT_PLACEHOLDER_KEY` is handed to the
# child by sign-in rung S1 (a reachable row's own `env_key`) and by rung S2
# (only when S1 sets nothing: the first declared `SWEEP_EXCLUDED_NAMES`
# `env_key` that the runner's environment holds a non-empty value for; the
# approved D-1 exception, sk8i spec r2 §2.2, RR-32). It is matched by neither
# the scrub nor the sweep (R2-4 — its value occurs in 1 155 files of the
# 2026-10-29 run of record, the five real credentials in 0).
XAI_CREDENTIAL_CHAIN = ("XAI_API_KEY", "GROK_CODE_XAI_API_KEY",
                        "APEX_LLM_PROXY_KEY")
CREDENTIAL_SURFACE_NAMES = ("APEX_LLM_PROXY_KEY", "CODEX_LLM_PROXY_KEY",
                            "CODEX_TRUE_FOUNDRY_KEY",
                            "GROK_CODE_XAI_API_KEY",
                            "GROK_DIRECT_ONPREM_AUTH",
                            "GROK_DIRECT_PLACEHOLDER_KEY",
                            "XAI_API_KEY")
SWEEP_EXCLUDED_NAMES = ("GROK_DIRECT_PLACEHOLDER_KEY",)
# The one swept set every consumer shares (r3a A5): the write-time scrub, the
# sweep backstop, slice_evidence and the literal check all read this tuple.
# r3a A5 replaced r3 §4.3.1's 5-name derivation. A variable outside the
# canonical tuple that a config declares is appended by credential_surface,
# so a new row's key is swept the moment it lands.
LITERAL_SWEEP_NAMES = tuple(n for n in sorted(CREDENTIAL_SURFACE_NAMES)
                            if n not in SWEEP_EXCLUDED_NAMES)

# r3 §4.3.4 / R2-7: the child environment is an explicit allowlist, not
# `os.environ` minus a denylist. r2's name regex is deleted, not tightened:
# a credential-shaped NAME is neither necessary nor sufficient to leak, and
# `SCS_PASS` / `OPERATOR_SSH_KEY` ride to the binary on the frozen bytes.
# Widening this is a one-line, reviewed, test-covered edit (R-15).
# `HOME`, like `GROK_HOME` and `GROK_AUTH_EXPIRED`, is listed here and then
# overwritten by `build_env` with a runner-pinned value (apex-h2u2).
CHILD_ENV_NON_CREDENTIAL_ALLOWLIST = (
    "PATH", "HOME", "LANG", "LC_ALL", "TERM", "TZ", "USER", "SHELL",
    "TMPDIR", "SSL_CERT_FILE", "SSL_CERT_DIR", "REQUESTS_CA_BUNDLE",
    "NODE_EXTRA_CA_CERTS", "GROK_HOME", "GROK_AUTH_EXPIRED")


def credential_surface(rows):
    """The declared credential set for one config (r3 §4.3.1, r3a A5).

    Both credential rungs a row can carry are read: `env_key` AND the
    `env_http_headers` variable (the latter is the rung r1 could not see, and
    it is what the two direct on-prem rows authenticate with). The fallback
    chain is added only when some row declares NEITHER, because the chain is
    what that row resolves through — a config with no such row contributes no
    chain name, which is the assertion that fails if a rung is hard-coded in.

    Derived from the config, never from the process env: §4.3.3 makes a
    name's runtime state something to RECORD (present / absent-unreachable /
    absent-reachable), not something to derive."""
    names = []
    chain_needed = False
    for row in rows or []:
        if not isinstance(row, dict):
            continue
        for key in ("env_key", "credential_variable"):
            var = row.get(key)
            if isinstance(var, str) and var and var not in names:
                names.append(var)
        if not row.get("env_key") and not row.get("credential_variable"):
            chain_needed = True
    if chain_needed:
        for rung in XAI_CREDENTIAL_CHAIN:
            if rung not in names:
                names.append(rung)
    swept = [n for n in LITERAL_SWEEP_NAMES if n in names]
    swept += [n for n in names
              if n not in LITERAL_SWEEP_NAMES
              and n not in SWEEP_EXCLUDED_NAMES]
    return {"names": names,
            "swept_names": swept,
            "excluded_names": [n for n in names
                               if n in SWEEP_EXCLUDED_NAMES]}


def literal_sweep_values(env=None):
    """The VALUES the §4.4 rules match, in LITERAL_SWEEP_NAMES order.

    An unset name contributes nothing (r3a A5: absent is a state, not a
    failure) and an excluded name never appears (R2-4). This function never
    prints, logs, hashes or returns anything derived from a value — callers
    use the list for in-memory matching only, which is what keeps RR-2 true
    while the sweep still matches the real deployment's keys."""
    env = os.environ if env is None else env
    return [env[n] for n in LITERAL_SWEEP_NAMES
            if n not in SWEEP_EXCLUDED_NAMES and env.get(n)]


def strip_credential_scheme(value):
    """The form §4.4.1 rule 1 compares on: a leading `Basic ` / `Bearer ` is
    removed from BOTH the value and the candidate span. The proxy echoes the
    TrueFoundry token with its scheme word stripped and the on-prem
    credential is a `Basic` blob captured without the word, so a rule that
    matched the whole header value missed every real leak (§2.5)."""
    return re.sub(r"^(?:basic|bearer)\s+", "", value or "", flags=re.I)


def credential_env_meta(names, env=None):
    """env_meta's credential block (r3 §4.3.3): the STATE of each declared
    name, never its value. An unset name is `ABSENT` — recorded, not fatal;
    a set one is a digest, so the report says which rung the run had without
    publishing it."""
    env = os.environ if env is None else env
    out = {}
    for name in names or ():
        val = env.get(name)
        out[name] = "ABSENT" if not val else "set:" + sha256_12(val)
    return out


def resolve_route_credentials(route_map, env=None, declared=None):
    """(values, causes) for one case's route map (r3 §4.3.3, r3a A6).

    `values` maps credential VARIABLE NAME -> the value the child and the
    wiretap must both present, so the two never disagree about which key a
    row runs with. A name no reachable route resolves to is simply absent
    from it, which is §4.3.3's `absent-and-unreachable` state: rc unchanged,
    nothing recorded as a failure.

    `causes` is that case's BLOCKED reason list. A route whose variable is
    DECLARED by the config and unset in `env` blocks the case with
    `credential-unset: <NAME>`; a route that declares no variable resolves
    through `XAI_CREDENTIAL_CHAIN` in auth_method.rs order, taking the FIRST
    name that is SET, and blocks only when no rung is set — naming the whole
    chain, because the caller cannot tell which rung was meant (r3a A6).

    `declared=None` means no config declaration was supplied, so only the
    chain leg can produce a cause: the declared-name rule needs the derived
    set, which is the caller's (`credential_surface`)."""
    env = os.environ if env is None else env
    declared = set(declared or ())
    values = {}
    causes = []
    for wire_id in sorted(route_map or {}):
        route = route_map[wire_id]
        if not isinstance(route, dict):
            continue
        var = route.get("credential_variable")
        if isinstance(var, str) and var:
            val = env.get(var)
            if val:
                values[var] = val
            elif var in declared:
                cause = "credential-unset: %s" % var
                if cause not in causes:
                    causes.append(cause)
            continue
        for rung in XAI_CREDENTIAL_CHAIN:
            if env.get(rung):
                values[rung] = env[rung]
                break
        else:
            cause = "credential-unset: %s" % "|".join(XAI_CREDENTIAL_CHAIN)
            if cause not in causes:
                causes.append(cause)
    return values, causes


def resolve_signin_credentials(route_map, row_routes, env=None):
    """(values, source, causes): the sign-in key the binary's gate accepts
    without a live probe (apex-sk8i, spec r2 §2.2).

    S1 (RR-32 F-4): for each reachable route, a row it carries whose `env_key`
    is that route's own `credential_variable` or a `SWEEP_EXCLUDED_NAMES` name
    hands that `env_key` when `env` holds a non-empty value for it. A row
    whose header variable won its route and whose `env_key` is a real key
    contributes nothing; the wiretap never presents that key. `source` is
    "route-row" when S1 set `values`.

    S2, only when S1 set nothing on a non-empty map: the first variable, in
    config order, that some row declares as its `env_key`, that is in
    `SWEEP_EXCLUDED_NAMES` (a candidate) and that `env` holds a non-empty
    value for; `source` is "declared-placeholder".

    S3 (RR-33 D-8 (a)): `values` still empty and either a reachable route has
    no `credential_variable` (it resolves through the fallback chain) or a
    candidate is declared -> one `credential-unset: ... (sign-in)` cause and
    `source` None. A route set with no chain route in a config declaring no
    candidate gets no cause (RR-33's carve-out: the binary refuses it
    deterministically, without a probe). An empty or absent map returns
    ({}, None, []).

    As in resolve_route_credentials, nothing derived from a value is printed,
    logged, hashed or returned except the value itself in `values`."""
    env = os.environ if env is None else env
    rows = {r.get("row_name"): r for r in row_routes or ()
            if isinstance(r, dict)}
    values = {}
    for wire_id in sorted(route_map or {}):
        route = route_map[wire_id]
        if not isinstance(route, dict):
            continue
        for name in route.get("row_names") or ():
            key = (rows.get(name) or {}).get("env_key")
            if not (isinstance(key, str) and key):
                continue
            if key != route.get("credential_variable") \
                    and key not in SWEEP_EXCLUDED_NAMES:
                continue
            if env.get(key):
                values[key] = env[key]
    if values or not route_map:
        return values, ("route-row" if values else None), []
    candidates = []
    for row in row_routes or ():
        key = row.get("env_key") if isinstance(row, dict) else None
        if isinstance(key, str) and key in SWEEP_EXCLUDED_NAMES \
                and key not in candidates:
            candidates.append(key)
    for key in candidates:
        if env.get(key):
            values[key] = env[key]
            return values, "declared-placeholder", []
    chain_route = any(
        isinstance(route, dict)
        and not (isinstance(route.get("credential_variable"), str)
                 and route.get("credential_variable"))
        for route in route_map.values())
    if not (chain_route or candidates):
        return {}, None, []
    causes = ["credential-unset: %s (sign-in)" % "|".join(candidates)
              if candidates else
              "credential-unset: sign-in (no row declares a sweep-excluded "
              "env_key)"]
    return {}, None, causes


def write_credential_block(run_dir, causes, wire_ids):
    """Records §4.3.3's `absent-and-reachable` cause where finalize_verdict's
    first rule already reads route causes, before the case runs any step: a
    credential the route does not have must never be forwarded anyway."""
    wire_dir = os.path.join(run_dir, "wire")
    os.makedirs(wire_dir, mode=0o700, exist_ok=True)
    with open(os.path.join(wire_dir, ROUTE_BLOCK_FILE), "a") as fh:
        for cause in causes:
            fh.write(json.dumps({"kind": "credential-unset",
                                 "wire_model_id": ",".join(wire_ids),
                                 "detail": cause}) + "\n")


# ---------------------------------------------------------------------------
# R-12 (r3 §4.5 item 3): the two auth-failure classes. The 2026-10-29
# classifier's single `credentials-403` class mixed "our harness auth is
# broken" with "right key, wrong origin"; the second is a provider answer and
# the first is a harness defect, and only the second should ever read as one.
# ---------------------------------------------------------------------------
AUTH_CLASS_UPSTREAM = "upstream-auth-error"
AUTH_CLASS_LOCAL = "harness-auth-failure"
AUTH_STATUSES = (401, 403)


def _wire_candidates(wire_dir):
    """The dirs to read: a caller may hand the run dir or the wire dir."""
    out = []
    for cand in (wire_dir, os.path.join(wire_dir or "", "wire")):
        if cand and os.path.isdir(cand):
            out.append(cand)
    return out


def _auth_status_seen(wire_dir):
    """True when this cell has an auth signal at all: a captured 401/403, or
    an `auth_error` body, or a credential-unset cause. Nothing else may be
    read as an auth failure — `auth_failure_class` calls a dir with no
    captured request a harness failure, and that must not fire on a row that
    never had an auth problem."""
    for cand in _wire_candidates(wire_dir):
        for path in sorted(globmod.glob(os.path.join(cand,
                                                     "resp-*.jsonl"))):
            try:
                with open(path, errors="replace") as fh:
                    text = fh.read()
            except Exception:
                continue
            try:
                head = json.loads(text.split("\n", 1)[0] or "{}")
            except Exception:
                head = {}
            if head.get("status") in AUTH_STATUSES:
                return True
            if '"auth_error"' in text:
                return True
    return False


def auth_failure_class(wire_dir):
    """Which auth class this cell's auth failure belongs to (R-12).

    The rule is mechanical, not interpretive: an UPSTREAM auth problem has a
    captured request that reached an origin and an upstream 401/403. A LOCAL
    one has ZERO captured requests — nothing ever left, so the failure is
    ours (an unset credential, a route map that never resolved, a header the
    wiretap did not inject) and must never be reported as a provider
    rejection. Returns None when the cell saw no auth status at all."""
    requests = 0
    auth_status = False
    for cand in _wire_candidates(wire_dir):
        requests += len(globmod.glob(os.path.join(cand, "req-*.json")))
        if _auth_status_seen(cand):
            auth_status = True
    if not requests:
        return AUTH_CLASS_LOCAL
    if auth_status:
        return AUTH_CLASS_UPSTREAM
    return None


def utc_ts() -> str:
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def sha256_12(value: str) -> str:
    import hashlib
    return hashlib.sha256(value.encode()).hexdigest()[:12]


def free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def log(msg: str) -> None:
    print("[%s] %s" % (time.strftime("%H:%M:%S"), msg), flush=True)


def redact(text: str, secrets) -> str:
    for s in secrets:
        if s:
            text = text.replace(s, "***REDACTED***")
    return text


# apex-ayl.70 R2-RED (coordinator adjudication 2026-09-18): the agent's
# _x.ai/mcp/servers_updated notification carries LIVE MCP server env
# values (credentials). They are scrubbed at acp.log write time (name
# kept — not sensitive; value redacted) and the final redaction sweep
# carries a backstop for the class.
MCP_ENV_REDACTED = "***REDACTED***"
# Coordinator ruling 2026-09-18 (marker unification): the canonical
# write-time marker is ***REDACTED***; the legacy [REDACTED] (the
# coordinator's 106-file historical scrub) is ALSO a redaction
# marker — the sweep backstop must not flag it as a live value. New
# write-time scrubs emit the canonical form only.
LEGACY_REDACTED_MARKER = "[REDACTED]"
REDACTED_MARKERS = (MCP_ENV_REDACTED, LEGACY_REDACTED_MARKER)
SERVERS_UPDATED_METHOD = "_x.ai/mcp/servers_updated"


def redact_mcp_env_values(line: str) -> str:
    """Write-time scrub for one acp.log line: redact every env entry's
    "value" in a _x.ai/mcp/servers_updated notification. Any other line
    (and a servers_updated line that is not complete parseable JSON)
    passes through byte-identical."""
    body = line[6:] if line.startswith("SEND: ") else line
    if SERVERS_UPDATED_METHOD not in body:
        return line
    try:
        obj = json.loads(body)
    except Exception:
        return line
    servers = (obj.get("params") or {}).get("mcpServers")
    if not isinstance(servers, list):
        return line
    changed = False
    for server in servers:
        env = server.get("env") if isinstance(server, dict) else None
        if not isinstance(env, list):
            continue
        for entry in env:
            if isinstance(entry, dict) and "value" in entry:
                entry["value"] = MCP_ENV_REDACTED
                changed = True
    if not changed:
        return line
    out = json.dumps(obj, separators=(",", ":"))
    if line.startswith("SEND: "):
        out = "SEND: " + out
    if line.endswith("\n"):
        out += "\n"
    return out


class _AcpLogRedactor:
    """Line-buffered write filter wrapping the acp.log file handle.

    The receive path (AcpSession._iter_lines) mirrors raw read1() chunks,
    so a servers_updated notification can straddle chunk boundaries —
    the scrub must see COMPLETE lines: buffer until newline, scrub each
    complete line, write through. A pending partial line is never emitted
    on flush() (it may still complete into a scrub target on the next
    chunk); it is written (scrubbed best-effort) only on close()."""

    def __init__(self, fh):
        self._fh = fh
        self._buf = ""

    def write(self, text):
        self._buf += text
        while "\n" in self._buf:
            line, self._buf = self._buf.split("\n", 1)
            self._fh.write(redact_mcp_env_values(line + "\n"))

    def flush(self):
        # Underlying buffer only — never emit the pending partial line.
        self._fh.flush()

    def close(self):
        try:
            if self._buf:
                self._fh.write(redact_mcp_env_values(self._buf))
            self._fh.flush()
        finally:
            self._fh.close()


def encode_cwd_dirname(cwd: str) -> str:
    """Mirror xai-grok-config paths.rs encode_cwd_dirname (short form)."""
    return urllib.parse.quote(cwd, safe="")


def read_live_config(live_home: str) -> str:
    with open(os.path.join(live_home, "config.toml")) as fh:
        return fh.read()


def live_upstream(live_home: str) -> str:
    cfg = read_live_config(live_home)
    m = re.search(r'^models_base_url\s*=\s*"([^"]+)"', cfg, re.M)
    if m:
        base = m.group(1).rstrip("/")
        # The wiretap appends the full route path grok issues against
        # models_base_url (/v1/<route>), so the origin it forwards to
        # must NOT already end in /v1 (else: https://host/v1/v1/<route>
        # -> 403 "Route is blocked").
        if base.endswith("/v1"):
            base = base[:-3]
        return base
    return DEFAULT_UPSTREAM


def _origin_from_base_url(base) -> str:
    """REDTEAM-REFRESH T2 (r3 §4.2.1): the origin (scheme://host[:port]) a
    base_url value points at.

    The wiretap appends the full route path the client issues against
    base_url (/v1/<route>), so an origin must NOT already end in /v1 — the
    same rule live_upstream applies to the deployment default, shared here so
    a row and the default are reduced identically."""
    base = (base or "").strip().rstrip("/")
    if base.endswith("/v1"):
        base = base[:-3]
    return base.rstrip("/")


def hermetic_default_origin(cfg_text) -> str:
    """The origin a row with no base_url of its own inherits: the config's
    models_base_url, else base_url, else the deployment default."""
    for key in ("models_base_url", "base_url"):
        m = re.search(r'^%s\s*=\s*"([^"]*)"' % key, cfg_text or "", re.M)
        if m and m.group(1).strip():
            return _origin_from_base_url(m.group(1))
    return DEFAULT_UPSTREAM


def _model_section(header):
    """Split a TOML header body into (model_id, sub_table).

    A ROW is `[model."<id>"]` or its bare-key twin `[model.<id>]` — the
    live config uses both (3 of its 18 rows are bare) and api_backend_for_model
    has always read both, so a row parser that accepts only the quoted spelling
    silently drops those rows. `[...<id>.<sub>]` is a sub-table
    (env_http_headers, extra_headers, reasoning_efforts, supported_efforts) and
    must never be mistaken for a row; `[[...]]` is an array of tables and is
    never a row. A TOML bare key holds no '.', so the first '.' after the row
    name opens the sub-table."""
    if not header.startswith("model."):
        return (None, None)
    rest = header[len("model."):]
    if not rest:
        return (None, None)
    if rest[0] in "\"'":
        end = rest.find(rest[0], 1)
        if end < 0:
            return (None, None)
        return (rest[1:end], rest[end + 1:].lstrip(".") or None)
    name, _, sub = rest.partition(".")
    if not name:
        return (None, None)
    return (name, sub or None)


def derive_row_routes(cfg_text, default_origin):
    """REDTEAM-REFRESH T2 (r3 §4.2.1) — the per-row route table.

    One line scan, no TOML dependency (the L18 discipline the runner already
    follows in api_backend_for_model). Each `[model."<id>"]` row contributes:

      * `origin` — its own base_url, else `default_origin` (a row that
        declares none inherits the config default exactly as the product
        resolves it);
      * `wire_model_id` — its `model` field, else the row name. THE MAP IS
        KEYED ON THIS, not the row name: the wire carries the model id, and
        two rows may answer to one id (r3 §4.2.1);
      * the credential that row authenticates with, as a VARIABLE NAME and a
        header name — never a value. env_http_headers' insert wins because
        that insert runs before the auth-header one (client.rs:2198 -> :1743);
        else the row's env_key; else None, meaning the product fallback chain
        (auth_method.rs:13-15, :30-34), whose rung T2 must not guess (r3
        §4.2.1: the chain's answer depends on the process env).

    Returns a list in config order."""
    rows = {}

    def row(name):
        if name not in rows:
            rows[name] = {"row_name": name, "wire_model_id": name,
                          "base_url": None, "env_key": None,
                          "auth_scheme": None, "credential_variable": None,
                          "credential_header": None}
        return rows[name]

    current = None        # row named by a [model."<id>"] header
    current_hdr = None    # row whose [model."<id>".env_http_headers] this is
    for raw in (cfg_text or "").splitlines():
        line_text = raw.strip()
        if not line_text or line_text.startswith("#"):
            continue
        if line_text.startswith("[") and line_text.endswith("]"):
            name, sub = _model_section(line_text[1:-1].strip())
            current = current_hdr = None
            if name is not None and sub is None:
                current = name
                row(name)
            elif name is not None and sub == "env_http_headers":
                current_hdr = name
            continue
        if "=" not in line_text:
            continue
        key, val = line_text.split("=", 1)
        key = key.strip().strip('"\'')
        val = val.strip().strip('"\'')
        if current_hdr is not None:
            r = row(current_hdr)
            if r["credential_variable"] is None and key and val:
                r["credential_variable"] = val
                r["credential_header"] = key.lower()
            continue
        if current is None:
            continue
        r = row(current)
        if key == "base_url":
            r["base_url"] = val
        elif key == "model":
            r["wire_model_id"] = val or r["row_name"]
        elif key == "env_key":
            r["env_key"] = val
        elif key == "auth_scheme":
            r["auth_scheme"] = val
    out = []
    for name in rows:
        r = rows[name]
        if r["credential_variable"] is None:
            r["credential_variable"] = r["env_key"]
            r["credential_header"] = ("x-api-key"
                                      if r["auth_scheme"] == "x-api-key"
                                      else "authorization")
        out.append({"row_name": name,
                    "wire_model_id": r["wire_model_id"],
                    "origin": (_origin_from_base_url(r["base_url"])
                               if r["base_url"] else default_origin),
                    "credential_variable": r["credential_variable"],
                    "credential_header": r["credential_header"],
                    # T3 (r3a A5): a row whose env_http_headers wins still
                    # declares an env_key, and BOTH are credential rungs the
                    # sweep must cover. The route map keeps using
                    # credential_variable alone (r3 §4.2.1).
                    "env_key": r["env_key"]})
    return out


UNROUTED_FILE = "unrouted.jsonl"
ROUTE_BLOCK_FILE = "route-block.jsonl"


def _read_cause_names(wire_dir, filename, field):
    """Distinct values of `field` in one of a case's cause files, in first-seen
    order. [] when the file is absent — the normal case: a case whose requests
    all route cleanly never creates either file."""
    if not wire_dir:
        return []
    path = os.path.join(wire_dir, filename)
    if not os.path.isfile(path):
        return []
    names = []
    try:
        with open(path) as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    d = json.loads(line)
                except ValueError:
                    continue
                v = d.get(field) if isinstance(d, dict) else None
                if isinstance(v, str) and v and v not in names:
                    names.append(v)
    except OSError:
        return []
    return names


def _count_cause_lines(wire_dir, filename):
    """Line count of a cause file (0 when absent)."""
    if not wire_dir:
        return 0
    path = os.path.join(wire_dir, filename)
    if not os.path.isfile(path):
        return 0
    try:
        with open(path) as fh:
            return sum(1 for line in fh if line.strip())
    except OSError:
        return 0


def write_route_block(run_dir, causes):
    """Records the r3b B1 rule-3 decisions where finalize_verdict's first rule
    reads them. Written BEFORE the case runs any step: an ambiguous pin route
    is decided up front, so nothing can be forwarded on a guessed origin."""
    wire_dir = os.path.join(run_dir, "wire")
    os.makedirs(wire_dir, mode=0o700, exist_ok=True)
    with open(os.path.join(wire_dir, ROUTE_BLOCK_FILE), "a") as fh:
        for cause in causes:
            # The kind is the cause's own prefix, so the
            # selector-path cause (RR-3 F-3 "ambiguous-route")
            # stays distinct from the pin-path one (r3b B1 rule 3
            # "ambiguous-pin-route") in the file itself.
            kind, _, wid = cause.partition(":")
            fh.write(json.dumps({"kind": (kind.strip()
                                  or "ambiguous-pin-route"),
                                 "wire_model_id": wid.strip(),
                                 "detail": cause}) + "\n")


def _add_selector(seq, val):
    if isinstance(val, str) and val and val not in seq:
        seq.append(val)


def case_route_selectors(case, rows_override=None):
    """(selectors, pin_ids) for the route map — the r3a A4 selector set.

    selectors (what the case declares it will run): `model`, `steps[].model`,
    `rows[].model` (or the --rows override), `agents_json.*/model`,
    `tool_calls[].args.model`, and every `model/<id>/...` config_patch key.
    pin_ids (ids a pin selects a request BY): every `where.body.model` in
    `assert.wire` and `row_asserts.wire` — a pin that names an id the map
    cannot resolve must fail closed too, not silently score 0 hits.
    """
    selectors = []
    pins = []
    _add_selector(selectors, case.get("model"))
    for st in case.get("steps") or []:
        if not isinstance(st, dict):
            continue
        _add_selector(selectors, st.get("model"))
        agents = st.get("agents_json")
        if isinstance(agents, dict):
            for agent in agents.values():
                if isinstance(agent, dict):
                    _add_selector(selectors, agent.get("model"))
        for tc in st.get("tool_calls") or []:
            if isinstance(tc, dict) and isinstance(tc.get("args"), dict):
                _add_selector(selectors, tc["args"].get("model"))
    rows = rows_override or case.get("rows") or []
    if isinstance(rows, str):
        # `--rows a,b` (run.py:24, argparse :5999) arrives as ONE CLI
        # string, which the rows path splits at :3780. Iterating a str
        # yields characters: the rows actually run would never reach
        # the map and the map would fill with one-letter keys.
        rows = [part.strip() for part in rows.split(",")
                if part.strip()]
    for entry in rows:
        if isinstance(entry, dict):
            _add_selector(selectors, entry.get("model"))
        else:
            _add_selector(selectors, entry)
    for key in (case.get("config_patch") or {}):
        parts = str(key).split("/")
        if len(parts) >= 2 and parts[0] == "model":
            _add_selector(selectors, parts[1])
    for block in (case.get("assert"), case.get("row_asserts")):
        if not isinstance(block, dict):
            continue
        for spec in block.get("wire") or []:
            if not isinstance(spec, dict) or not isinstance(spec.get("where"),
                                                           dict):
                continue
            _add_selector(pins, spec["where"].get("body.model"))
    return (selectors, pins)


# ---------------------------------------------------------------------------
# Hermetic GROK_HOME
# ---------------------------------------------------------------------------

def _toml_scalar(v) -> str:
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, int):
        return str(v)
    if isinstance(v, float):
        return repr(v)
    if isinstance(v, str):
        return json.dumps(v)
    raise ValueError("unsupported config_patch scalar: %r" % (v,))


def _split_toml_dotted(name: str):
    """Split a TOML dotted key/section respecting quoted parts:
    `model."qwen3.8-27b"` -> ['model', 'qwen3.8-27b']."""
    parts = re.split(r'\.(?=(?:[^"]*"[^"]*")*[^"]*$)', name)
    return [p.strip().strip('"') if p.strip().startswith('"')
            else p.strip() for p in parts]


def _header_name(parts) -> str:
    """['model', 'qwen3.8-27b'] -> `model."qwen3.8-27b"` (quote non-bare)."""
    out = []
    for part in parts:
        out.append(part if re.fullmatch(r"[A-Za-z0-9_-]+", part)
                   else json.dumps(part))
    return ".".join(out)


def _split_patch_key(key: str):
    """Case-file config_patch keys use `/` between parts (unambiguous for
    model ids containing dots): `model/qwen3.8-27b/context_window`.
    Plain dotted keys are also accepted (quote-aware split)."""
    if "/" in key:
        parts = [p for p in key.split("/") if p]
    else:
        parts = _split_toml_dotted(key)
    return parts


def _apply_config_patch(cfg_text: str, patch: dict) -> str:
    """Apply a {key: scalar} patch to TOML text by section surgery.

    Key form: `section.../field` (slash-separated; the matrix needs
    model/<id>/<field> and features/<field>). Finds the target section
    (quoted or unquoted header) and sets the key in place (replacing an
    existing assignment), or appends a new section at EOF. Deterministic;
    no TOML writer dependency.

    SWEEPFIX-64 (F-COMP): the append branch inserts the new key at the
    END OF THE PARENT'S OWN DIRECT CONTENT — immediately before the
    first subtable header of any kind ([a.b.c] or [[a.b.c]]) that
    follows the section header, or at the section end when there are no
    subtables. Appending at the raw span end (after [[...]] blocks)
    makes TOML bind the key to the LAST array element of the trailing
    [[...]] subtable, where the lenient serde row drops it silently
    (2026-09-17: model.<id>.context_window=12000 re-bound into a
    reasoning_efforts element -> catalog window won -> no compaction).
    """
    for key, value in patch.items():
        parts = _split_patch_key(key)
        if len(parts) < 2:
            raise ValueError("config_patch key needs section+field: %r"
                             % key)
        section_parts, field = parts[:-1], parts[-1]
        header_re = re.compile(r'^\[(?P<name>[^\]]+)\]\s*$')
        lines = cfg_text.splitlines(keepends=True)
        section_bounds = []
        for i, ln in enumerate(lines):
            m = header_re.match(ln)
            if m:
                section_bounds.append((i, m.group("name").strip()))
        target = None
        for i, name in section_bounds:
            if _split_toml_dotted(name) == section_parts:
                target = i
                break
        line = "%s = %s\n" % (field, _toml_scalar(value))
        if target is None:
            if not cfg_text.endswith("\n"):
                cfg_text += "\n"
            cfg_text += ("\n[%s]\n%s" % (_header_name(section_parts),
                                           line))
        else:
            end = len(lines)
            for i, _ in section_bounds:
                if i > target:
                    end = i
                    break
            key_re = re.compile(r"^%s\s*=" % re.escape(field))
            replaced = False
            for i in range(target + 1, end):
                if key_re.match(lines[i].strip()):
                    lines[i] = line
                    replaced = True
                    break
            if not replaced:
                # SWEEPFIX-64: find the first subtable header of any
                # kind in the span — the parent's direct content ends
                # there (any header, including [[...]] array-of-tables,
                # ends the current table's key region in TOML).
                subtable_re = re.compile(
                    r"^\[([^\]]+)\]\s*$|^\[\[([^\]]+)\]\]\s*$")
                insert_at = end
                for i in range(target + 1, end):
                    if subtable_re.match(lines[i]):
                        insert_at = i
                        break
                while (insert_at > target + 1
                       and lines[insert_at - 1].strip() == ""):
                    insert_at -= 1
                lines.insert(insert_at, line)
            cfg_text = "".join(lines)
    return cfg_text


def _apply_no_watch(cfg_text: str):
    """apex-ayl.22 D-10 (G11/OQ-7): the codegraph MCP server must not
    spawn a watcher into the worktree's .codegraph/ (an S-8 class
    breach). Invariant line-edit appending "--no-watch" to the COPIED
    [mcp_servers.codegraph] args array (idempotent). Returns
    (cfg, applied). Applied AFTER the case patch in HermeticHome so a
    case patch cannot remove it (disclosed in the report header)."""
    m = re.search(r"\[mcp_servers\.codegraph\](.*?)(?=\n\[|\Z)",
                  cfg_text, re.S)
    if not m:
        return cfg_text, False
    if "--no-watch" in m.group(1):
        return cfg_text, True
    am = re.search(r"args\s*=\s*\[(.*?)\]", m.group(1), re.S)
    if not am:
        return cfg_text, False
    inner = am.group(1).rstrip()
    if inner and not inner.endswith(","):
        inner += ","
    inner += '\n "--no-watch",'
    new_section = m.group(1)[:am.start(1)] + inner + \
        m.group(1)[am.end(1):]
    return cfg_text[:m.start(1)] + new_section + cfg_text[m.end(1):], True


# apex-h2u2 A-1' (r2 §4.1): the per-attempt scratch USER home, a sibling of
# GROK_HOME. The runner's own HOME carries the operator's zsh/bash startup
# files and every home-rooted product source the binary reads (r2 §3.2), and
# `HOME` is an allowlisted name, so without the pin in build_env the child
# reads all of it.
HERMETIC_USER_HOME_NAME = "userhome"


def hermetic_user_home(home: str) -> str:
    """The scratch user home for one attempt: a sibling of its GROK_HOME.
    Pure — it creates nothing (HermeticHome does)."""
    return os.path.join(os.path.dirname(os.path.abspath(home)),
                        HERMETIC_USER_HOME_NAME)


_HERMETIC_POLICY_CAUSE = ("hermetic-home: shell_environment_policy %s would "
                          "drop or override HOME in tool commands")


def _hermetic_policy_match(patterns, name):
    """True if any `patterns` entry matches `name` under the product's glob:
    `WildMatchPattern<'*', '?'>`, case-insensitive (shell_env_policy.rs:10-11).
    r2 §4.4's model escapes the whole pattern and translates only the two
    wildcards, so `[` stays literal in both directions; `\\` is literal too
    (U-11: how the product treats it is UNVERIFIED)."""
    target = name.upper()
    for pattern in patterns:
        expr = (re.escape(pattern.upper())
                .replace(re.escape("*"), ".*")
                .replace(re.escape("?"), "."))
        if re.fullmatch(expr, target):
            return True
    return False


def _hermetic_policy_shape(policy):
    """(exclude, include_only, set_names) for a policy table, or None when its
    shape is not the product's — a table this runner cannot read cannot be
    shown to keep HOME, so the caller fails closed."""
    exclude = policy.get("exclude", [])
    include_only = policy.get("include_only", [])
    set_map = policy.get("set", {})
    ok = (isinstance(exclude, list) and isinstance(include_only, list)
          and isinstance(set_map, dict)
          and all(isinstance(p, str) for p in exclude)
          and all(isinstance(p, str) for p in include_only))
    if not ok:
        return None
    return exclude, include_only, list(set_map)


def hermetic_shell_policy_cause(cfg_text):
    """apex-h2u2 M-3' (r2 §4.4): the reason the config's
    `[shell_environment_policy]` would drop or override `HOME` in every tool
    command, or None when it cannot.

    `build_env` pins `HOME` to the per-attempt scratch user home, but the
    product builds each tool command's base environment from this one table —
    `env_clear()` plus the policy-derived map (shell_env_policy.rs:211-219) —
    so a policy that empties it, excludes `HOME`, or overwrites `HOME` through
    `set` undoes that pin inside the shell. OD-2 is ruled (a): block,
    fail-closed (RR-45). Rules a'-d' are r2 §4.4's, evaluated in the product's
    own application order (shell_env_policy.rs:176-197): inherit, the default
    excludes, exclude, set, include_only. The default excludes `*KEY*`,
    `*SECRET*`, `*TOKEN*` (shell_env_policy.rs:36-38) never match `HOME`, so
    `ignore_default_excludes` cannot change the answer and is not consulted.

    The cause names keys, never values, and is deliberately not a
    `credential-unset` record: `finalize_verdict` reads those as route causes
    (§4.4's reason for not reusing `write_credential_block`).
    """
    try:
        policy = tomllib.loads(cfg_text).get("shell_environment_policy")
    except tomllib.TOMLDecodeError:
        return _HERMETIC_POLICY_CAUSE % "parse"
    if policy is None:
        return None
    if not isinstance(policy, dict):
        return _HERMETIC_POLICY_CAUSE % "parse"
    shape = _hermetic_policy_shape(policy)
    if shape is None:
        return _HERMETIC_POLICY_CAUSE % "parse"
    exclude, include_only, set_names = shape
    if policy.get("inherit") == "none":
        return _HERMETIC_POLICY_CAUSE % "a"
    if _hermetic_policy_match(exclude, "HOME"):
        return _HERMETIC_POLICY_CAUSE % "b"
    if any(name.upper() == "HOME" for name in set_names):
        return _HERMETIC_POLICY_CAUSE % "d"
    if include_only and not _hermetic_policy_match(include_only, "HOME"):
        return _HERMETIC_POLICY_CAUSE % "c"
    return None


class HermeticHome:
    """Tempdir GROK_HOME: live config copy + patch + hermetic auth stub,
    with all base_urls rewritten to the local wiretap (when wirecap)."""

    def __init__(self, root: str, live_home: str, config_patch: dict,
                 wire_port, keep: bool = False):
        self.home = os.path.join(root, "home")
        self.cwd = os.path.join(root, "cwd")
        self.user_home = hermetic_user_home(self.home)
        os.makedirs(self.home, mode=0o700, exist_ok=True)
        os.makedirs(self.cwd, exist_ok=True)
        # apex-h2u2 I-3: fresh, private (0700) and empty at creation; nothing
        # is ever written into it.
        os.makedirs(self.user_home, mode=0o700, exist_ok=True)
        cfg = read_live_config(live_home)
        # Harness invariant (disclosed in the report): this fork fires
        # display-only side-calls on every headless turn — a full-history
        # "dashboard line" call and a session-title refresh, both gated by
        # features.turn_summary (title_refresh.rs: shares the
        # turn_summary_enabled gate). They triple per-turn proxy spend
        # without touching any behavior under test (compaction, switch,
        # resume, tools), so the hermetic home disables them. Case
        # patches still take precedence (applied after).
        cfg = _apply_config_patch(cfg, {"features/turn_summary": False})
        cfg = _apply_config_patch(cfg, config_patch or {})
        # apex-ayl.22 D-10 (G11/OQ-7): AFTER the case patch — a case
        # patch on this section cannot remove --no-watch (the runner
        # re-asserts it; disclosed in the report header).
        cfg, self.no_watch_applied = _apply_no_watch(cfg)
        stub_dst = os.path.join(self.home, "proxy-auth-stub.sh")
        shutil.copy(os.path.join(live_home, "proxy-auth-stub.sh"), stub_dst)
        os.chmod(stub_dst, 0o755)
        cfg = cfg.replace(os.path.join(live_home, "proxy-auth-stub.sh"),
                          stub_dst)
        # REDTEAM-REFRESH T2 (r3 §4.2.1): the route table must be derived
        # from the config as this case sees it (case patch applied) BEFORE
        # every base_url is rewritten to the one loopback port — after the
        # rewrite every row has the same origin and the per-row distinction
        # this item exists to enforce is gone.
        self.route_cfg = cfg
        self.default_origin = hermetic_default_origin(cfg)
        if wire_port:
            proxy_base = "http://127.0.0.1:%d/v1" % wire_port
            cfg = re.sub(r'^base_url\s*=\s*"[^"]*"',
                         'base_url = "%s"' % proxy_base, cfg, flags=re.M)
            cfg = re.sub(r'^models_base_url\s*=\s*"[^"]*"',
                         'models_base_url = "%s"' % proxy_base, cfg,
                         flags=re.M)
        with open(os.path.join(self.home, "config.toml"), "w") as fh:
            fh.write(cfg)
        cache_src = os.path.join(live_home, "models_cache.json")
        if os.path.isfile(cache_src):
            # Catalog cache: -m <model> resolves against config + cache;
            # without it the hermetic registry only sees [model."X"]
            # sections and catalog models (qwen3.8-27b) are "unknown id".
            cache_path = os.path.join(self.home, "models_cache.json")
            shutil.copy(cache_src, cache_path)
            _align_models_cache(cache_path, wire_port)
        if not keep:
            pass  # cleanup via caller (tempdir)
        self.keep = keep

    def session_dir_for(self, cwd: str, sid: str) -> str:
        return os.path.join(self.home, "sessions",
                            encode_cwd_dirname(cwd), sid)

    def row_routes(self):
        if getattr(self, "_row_routes", None) is None:
            self._row_routes = derive_row_routes(self.route_cfg,
                                                self.default_origin)
        return self._row_routes

    def route_map(self, selectors, pin_ids):
        """(map, causes) — the routes this case may use, keyed on the WIRE
        MODEL ID (r3 §4.2.1), plus the r3b B1 pin decisions.

        A route is the triple (origin, credential_variable, credential_header);
        two rows sharing a wire id share a route only if their triples are
        identical, which is what makes `gpt-6.1-sol` / `gpt-6.1-sol-1m`
        ambiguous today while `claude-opus-5.5` / `-1m` are not. The
        sol pair shares ONE origin — the named row's base_url and the
        config's models_base_url reduce to the same host — and differs
        in the credential: `gpt-6.1-sol` declares env_key
        CODEX_LLM_PROXY_KEY, `gpt-6.1-sol-1m` declares no row
        credential and resolves through the product fallback chain.

        Pin rules, in order (r3b B1):
          1. a pin whose wire id a selector already resolves adds nothing —
             the selected row's own route stands, sibling rows excluded;
          2. else, if every row with that wire id shares one route, add them;
          3. else record "ambiguous-pin-route: <wire id>" in `causes`: the
             case must BLOCK rather than guess an origin.
        An empty selector set declares nothing, so every live row IS
        routed — EXCEPT an id whose own rows disagree on the route
        triple, which the F-3 collision path drops while recording
        "ambiguous-route: <wire id>" (today exactly `gpt-6.1-sol`, for
        the credential reason above). An id no live row declares adds NO
        route (ruling RR-3 F-1,
        r3 §4.2.1: "there is no second key and no default-origin
        fallback"): the request is refused by the wiretap and the case reads
        BLOCKED `unrouted: <id>`, exactly as an unresolvable pin already does.

        Selector ambiguity (ruling RR-3 F-3, r3b B1 rule 3 applied to the
        selector channel): two selected rows on one wire id with DIFFERENT
        triples leave the key out and record
        "ambiguous-route: <wire id>" — dict order never picks the
        origin a credential is later presented to.

        Values are variable NAMES and origins only — never a credential value
        (r3 §4.2.1; the blob reaches the wiretap through the env seam)."""
        routes = self.row_routes()
        by_name = {r["row_name"]: r for r in routes}
        by_wire = {}
        for r in routes:
            by_wire.setdefault(r["wire_model_id"], []).append(r)
        out = {}
        causes = []

        def triple(r):
            return (r["origin"], r["credential_variable"],
                    r["credential_header"])

        def route_triple(route):
            """The triple of an ALREADY-BUILT route, for the RR-3
            F-3 collision test (a built route lists its rows, a row does
            not)."""
            return (route["origin"], route["credential_variable"],
                    route["credential_header"])

        def add(wire_id, group):
            out[wire_id] = {"origin": group[0]["origin"],
                            "credential_variable":
                                group[0]["credential_variable"],
                            "credential_header": group[0]["credential_header"],
                            "row_names": [g["row_name"] for g in group]}

        sels = [s for s in (selectors or []) if isinstance(s, str) and s]
        pins = [p for p in (pin_ids or []) if isinstance(p, str) and p]
        if not sels:
            sels = list(by_name)
        undecided = set()
        for sel in sels:
            r = by_name.get(sel)
            if r is not None:
                wire_id = r["wire_model_id"]
                if wire_id in undecided:
                    continue
                prev = out.get(wire_id)
                if prev is not None and route_triple(prev) != triple(r):
                    # RR-3 F-3: two selected rows, one wire id, two
                    # triples. Neither wins: dropping the key and
                    # recording the cause is what stops the LAST
                    # selector from silently choosing the origin.
                    out.pop(wire_id, None)
                    undecided.add(wire_id)
                    causes.append("ambiguous-route: %s" % wire_id)
                    continue
                same = [g for g in by_wire[wire_id]
                        if triple(g) == triple(r)]
                add(wire_id, same)
                continue
            group = by_wire.get(sel)
            if group is None:
                # RR-3 F-1: an unresolvable selector adds no route,
                # so its request is refused and the case BLOCKS.
                # A synthesized default-origin entry here is r3 §2.4
                # re-manufactured: a credential presented to an
                # origin that does not serve the row.
                continue
            elif len({triple(g) for g in group}) == 1:
                add(sel, group)
            else:
                causes.append("ambiguous-pin-route: %s" % sel)
        for pin in pins:
            if pin in out:
                continue
            group = by_wire.get(pin)
            if not group:
                continue
            if len({triple(g) for g in group}) > 1:
                causes.append("ambiguous-pin-route: %s" % pin)
                continue
            add(pin, group)
        return (out, sorted(set(causes)))


def _align_models_cache(cache_path: str, wire_port) -> None:
    """Align the copied live catalog cache to the hermetic scope.

    The cache scope gate (remote_config/cache.rs try_load_fresh) checks
    grok_version, auth_method, origin, identity, and a 300s TTL. The
    live copy mismatches on origin (rewritten base_url under wirecap),
    identity (computed from the live session's XAI_API_KEY), and
    freshness (TTL). grok_version already matches (same CARGO_PKG_VERSION
    for source builds) and auth_method is "api_key" whenever XAI_API_KEY
    is set in the environment (the L1 contract keeps it). Rewriting the
    three fields materializes "the catalog fetched for this hermetic
    scope" so -m <catalog-model> resolves offline and race-free; the
    catalog content is credential-independent (same 76-model list).
    """
    try:
        with open(cache_path) as fh:
            cache = json.load(fh)
    except Exception:
        return
    if not isinstance(cache, dict) or not cache.get("models"):
        return
    cache["renewed_at"] = datetime.now(timezone.utc).strftime(
        "%Y-%m-%dT%H:%M:%S.%fZ")
    if wire_port:
        cache["origin"] = "http://127.0.0.1:%d/v1/models" % wire_port
    xai_key = os.environ.get("XAI_API_KEY", "")
    if xai_key:
        import hashlib
        h = hashlib.sha256()
        for part in ("models-api-key", xai_key, ""):
            # model_fetch_auth.rs scope_hash: NUL-separated sha256.
            # alpha_test_key is unset in the live config (verified).
            h.update(part.encode())
            h.update(b"\x00")
        cache["identity"] = h.hexdigest()
    with open(cache_path, "w") as fh:
        json.dump(cache, fh, indent=2)


# ---------------------------------------------------------------------------
# History seeder (R-2 pinned schema; round-trip validated)
# ---------------------------------------------------------------------------

SYSTEM_PROMPT = (
    "You are Grok released by xAI. You are an interactive CLI tool that "
    "helps users with software engineering tasks. This is a synthetic "
    "seeded history for the HT-1 red-team harness; do not attempt any "
    "real work."
)

FILLER = (
    "Synthetic context paragraph {i}: the compaction threshold is a "
    "fraction of the harness-resolved context window, so deterministic "
    "token targets make auto-compaction reproducible. The wire capture "
    "records every request body so the harness can assert on replayed "
    "history, model fields, and backend paths directly. Token estimates "
    "for seeding use four characters per token as a stable heuristic."
)


def seed_history(home: str, cwd: str, sid: str, model: str,
                 target_tokens: int, target_bytes: int = 0):
    """Write chat_history.jsonl + summary.json for a fresh session.

    Schema pinned from the 01a09be2 incident session (RECON R-2):
      system:    {"type":"system","content":str}
      user:      {"type":"user","content":[{"type":"text","text":str}]}
      assistant: {"type":"assistant","content":str,"model_id":str,
                  "reasoning_effort":str,"tool_calls":[]}
    target_bytes (optional) takes precedence over target_tokens.
    Returns (session_dir, n_lines, total_bytes, est_tokens).
    """
    sdir = home.session_dir_for(cwd, sid)
    os.makedirs(sdir, mode=0o700, exist_ok=True)
    lines = [json.dumps({"type": "system", "content": SYSTEM_PROMPT})]
    est = len(SYSTEM_PROMPT) // 4
    i = 0

    def _size():
        return sum(len(l) for l in lines)

    def _cap_met():
        # target_bytes takes precedence (docstring); token cap otherwise.
        if target_bytes:
            return _size() >= target_bytes
        return est >= target_tokens

    while not _cap_met():
        for role_lines in (
            [json.dumps({"type": "user", "content": [
                {"type": "text",
                 "text": FILLER.format(i=i) + " " + FILLER.format(i=i + 1)}]}),
             json.dumps({"type": "assistant",
                         "content": "Acknowledged. Paragraphs %d-%d noted."
                                    % (i, i + 1),
                         "model_id": model,
                         "reasoning_effort": "medium",
                         "tool_calls": []})]):
            lines.append(role_lines)
            est += len(role_lines) // 4
            i += 1
    history_path = os.path.join(sdir, "chat_history.jsonl")
    with open(history_path, "w") as fh:
        fh.write("\n".join(lines) + "\n")
    now = datetime.now(timezone.utc).isoformat()
    summary = {
        "info": {"id": sid, "cwd": cwd},
        "agent_id": "ag1.ht1seed0000000000000000000000",
        "session_summary": "HT-1 seeded synthetic history (%d est tokens)"
                           % target_tokens,
        "created_at": now,
        "updated_at": now,
        "num_messages": len(lines),
        "num_chat_messages": len(lines),
        "current_model_id": model,
        "chat_format_version": 1,
        "grok_home": home.home,
        "agent_name": "grok-build-plan",
    }
    with open(os.path.join(sdir, "summary.json"), "w") as fh:
        json.dump(summary, fh, indent=2)
    total_bytes = os.path.getsize(history_path)
    return sdir, len(lines), total_bytes, est


REQUIRED_KEYS = {
    "system": {"type", "content"},
    "user": {"type", "content"},
    "assistant": {"type", "content", "model_id", "tool_calls"},
    "reasoning": {"type", "content", "id", "status"},
    "tool_result": {"type", "content", "tool_call_id"},
}


def validate_roundtrip(history_path: str):
    """Parse every line back; assert per-type key contract (R-2 round-trip)."""
    n = 0
    with open(history_path) as fh:
        for ln, line in enumerate(fh, 1):
            line = line.strip()
            if not line:
                continue
            d = json.loads(line)
            t = d.get("type")
            req = REQUIRED_KEYS.get(t)
            if req is None:
                raise ValueError("line %d: unknown type %r" % (ln, t))
            missing = req - set(d.keys())
            if missing:
                raise ValueError("line %d: missing %s" % (ln, missing))
            n += 1
    return n


# ---------------------------------------------------------------------------
# NDJSON event parsing (RECON R-4/R-5 pinned vocabularies)
# ---------------------------------------------------------------------------

def parse_ndjson(text: str):
    events = []
    for ln, line in enumerate(text.splitlines(), 1):
        line = line.strip()
        if not line:
            continue
        try:
            d = json.loads(line)
        except Exception:
            continue
        d["_line"] = ln
        events.append(d)
    return events


def parse_json_output(text: str):
    """`--output-format json`: one final JSON object after possible noise."""
    start = text.find("{")
    if start == -1:
        return None
    try:
        return json.loads(text[start:])
    except Exception:
        return None


def ndjson_session_id(events, fmt: str):
    if fmt == "streaming-messages-json":
        for e in events:
            if e.get("type") == "system" and e.get("session_id"):
                return e["session_id"]
        return None
    for e in reversed(events):
        if e.get("type") == "end" and e.get("sessionId"):
            return e["sessionId"]
    return None


def ndjson_text(events, fmt: str) -> str:
    if fmt == "acp":
        return "".join(e.get("data", "") for e in events
                       if e.get("type") == "acp_message")
    if fmt == "streaming-messages-json":
        for e in reversed(events):
            if e.get("type") == "result" and isinstance(e.get("result"), str):
                return e["result"]
        return ""
    return "".join(e.get("data", "") for e in events
                   if e.get("type") == "text")


def ndjson_available_commands(events, fmt: str):
    """All advertised tool-name lists (RECON: streaming-json
    available_commands.tools; messages-json system.tools)."""
    out = []
    for e in events:
        if fmt == "streaming-messages-json":
            if e.get("type") == "system" and isinstance(e.get("tools"), list):
                out.append(e["tools"])
        elif e.get("type") == "available_commands" and \
                isinstance(e.get("tools"), list):
            out.append(e["tools"])
    return out


# ---------------------------------------------------------------------------
# Process execution (headless)
# ---------------------------------------------------------------------------

class TurnResult:
    def __init__(self):
        self.exit_code = None
        self.killed = False
        self.elapsed = 0.0
        self.stdout = ""
        self.stderr = ""
        self.events = []
        self.session_id = None
        self.wire_model_calls = 0


def build_env(home: str, extra_unset=None,
              credential_names=None) -> dict:
    """The child environment (r3 §4.3.4, R2-7): an explicit ALLOWLIST.

    The frozen rule was `os.environ` minus four names, so every credential in
    the runner's environment — plus SCS_PASS, OPERATOR_SSH_KEY and anything
    else the operator happens to export — reached the binary that writes a
    report folder. `os.environ` is now read only to LOOK UP allowlisted names,
    never to enumerate them, so an undeclared name is absent by construction
    rather than by a pattern that has to be right every time.

    `credential_names` is the union's credential half: the reachable routes'
    credential variables (resolve_route_credentials) ∪ the sign-in variable
    (resolve_signin_credentials: S1, a reachable row's own `env_key`; S2, only
    when S1 sets nothing, the first declared `SWEEP_EXCLUDED_NAMES` `env_key`
    that the runner's environment holds a non-empty value for, which may come
    from a row no reachable route carries: the approved D-1 exception). Nothing
    else credential-shaped rides in, and a route the case cannot name gets no
    key.

    The child's user home is hermetic (apex-h2u2). `HOME` is pinned to the
    attempt's `hermetic_user_home(home)`, a directory that is empty when the
    attempt starts. It is written after both loops, so neither the runner's
    environment nor a case's `provider_vars_unset` can change it. No user
    startup file, Claude settings `env`, skill, rule, agent, plugin, hook or
    MCP file under the operator's home is then reached through `HOME` or `~`
    by the binary or by any descendant that inherits its environment. The
    system `/etc/zsh*` files still run, and a command that names an absolute
    path is not covered.

    apex-ayl.22 D-7 is preserved: the case env.provider_vars_unset extends the
    runner L1 set and still removes every looked-up allowlisted name (a subset
    declaration is rejected at the NEW-case gate). Only the three runner-pinned
    names — `GROK_HOME`, `GROK_AUTH_EXPIRED` and `HOME` — are exempt, because
    `build_env` writes them after both loops."""
    unset = set(PROVIDER_VARS_UNSET)
    if extra_unset:
        unset.update(extra_unset)
    env = {}
    for name in CHILD_ENV_NON_CREDENTIAL_ALLOWLIST:
        value = os.environ.get(name)
        if value is not None and name not in unset:
            env[name] = value
    for name in credential_names or ():
        value = os.environ.get(name)
        if value is not None and name not in unset:
            env[name] = value
    env["GROK_AUTH_EXPIRED"] = "1"
    # kiloecho P0 class 1 (defense-in-depth): never hand the binary a
    # relative GROK_HOME.
    env["GROK_HOME"] = os.path.abspath(home)
    # apex-h2u2 A-1' (r2 §4.1): the child's user home is the per-attempt
    # scratch, never the runner's HOME. Written after both lookup loops, so a
    # case env.provider_vars_unset naming HOME cannot remove it (I-8), and the
    # credential half of the allowlist is untouched (I-1).
    env["HOME"] = hermetic_user_home(home)
    return env


def run_headless_turn(bin_path, home, cwd, model, prompt, resume_sid,
                      output_format="streaming-json", extra_args=None,
                      timeout_s=300, kill_after_s=None,
                      capture_dir=None, extra_unset=None,
                      credential_names=None):
    args = [bin_path, "-m", model, "-p", prompt,
            "--output-format", output_format, "--always-approve"]
    if resume_sid:
        args += ["--resume", resume_sid]
    args += extra_args or []
    env = build_env(home, extra_unset,
                  credential_names)
    start = time.time()
    proc = subprocess.Popen(args, cwd=cwd, env=env,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            text=True, preexec_fn=os.setsid)
    killed = False
    def _kill():
        nonlocal killed
        try:
            os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
            killed = True
        except Exception:
            try:
                proc.kill()
                killed = True
            except Exception:
                pass
    killer = None
    kill_s = kill_after_s if kill_after_s is not None else timeout_s
    killer = threading.Timer(kill_s, _kill)
    killer.daemon = True
    killer.start()
    try:
        out, err = proc.communicate()
    finally:
        killer.cancel()
    elapsed = time.time() - start
    r = TurnResult()
    r.exit_code = proc.returncode
    r.killed = killed
    r.elapsed = elapsed
    r.stdout = out or ""
    r.stderr = err or ""
    # kiloecho P0 triage §5: persist raw headless stdout per turn (was
    # in-memory only — RT-M6-class failures invisible on disk). Per-turn
    # files under capture_dir ride the final redaction sweep; ACP cases are
    # unaffected (they have acp.log).
    if capture_dir:
        seq = len(globmod.glob(os.path.join(capture_dir, "stdout.ndjson.*")))
        with open(os.path.join(capture_dir, "stdout.ndjson.%d" % seq),
                  "w") as fh:
            fh.write(r.stdout)
    fmt = "streaming-messages-json" if output_format == \
        "streaming-messages-json" else output_format
    r.events = parse_ndjson(r.stdout) if output_format.startswith(
        "streaming") else []
    if output_format == "json":
        r.events = [parse_json_output(r.stdout)]
        r.events = [e for e in r.events if e is not None]
    r.session_id = ndjson_session_id(r.events, fmt)
    if capture_dir:
        r.wire_model_calls = count_wire_model_calls(capture_dir)
    return r


def count_wire_model_calls(capture_dir: str) -> int:
    n = 0
    for f in sorted(globmod.glob(os.path.join(capture_dir, "req-*.json"))):
        try:
            with open(f) as fh:
                d = json.load(fh)
        except Exception:
            continue
        # apex-ayl.22 D-9 (G1): the cc wire counts too — a
        # /v1/chat/completions exchange is a model call.
        if d.get("method") == "POST" and \
                (d.get("path", "").endswith("/responses") or
                 d.get("path", "").endswith("/messages") or
                 d.get("path", "").endswith("/chat/completions")):
            n += 1
    return n


# ---------------------------------------------------------------------------
# ACP stdio driver (RECON R-1b: NDJSON-RPC 2.0, stdlib-reachable)
# ---------------------------------------------------------------------------

ACP_ERROR_GRACE_S = 3.0
# ACP-2: on error turns the provider error text (e.g. the Azure 400
# body) arrives in prompt_complete.agentResult, typically a few lines
# AFTER the turn_completed notification. This grace read waits that
# long for the matching prompt_complete before falling back to the
# generic 'turn ended with stopReason=error' shape.

# apex-ayl.32 (XREPLAY-2): the ACP parent read path hit a deterministic
# 424 BPS plateau in cold windows (EDR/auditd per-syscall inspection
# heat + 30s OTEL fleet-policy startup wait; phase timeline:
# grok/plans/xreplay2-close-report.md §6). The initialize result line
# measures 26,001B -> 61.3s at the plateau, which the old hardcoded
# 60s init budget could not cover (0.98x margin -> the observed
# intermittent init failures). 180s = 2.94x margin at the plateau
# (180 / (26001/424)), combined with the chunked read below — the
# plateau was read(1)-specific (26,001 per-byte syscalls vs ~1 bulk
# read of the same line; child-side raw read = 0.97s).
ACP_INIT_TIMEOUT_S = 180
# Chunk size for the ACP stdout read path (apex-ayl.32): bounds the
# per-line read syscalls (the 26,001B init line = 1 read, not 26,001).
ACP_READ_CHUNK = 65536
# Slow-line heartbeat (apex-ayl.32 observability): while a single line
# is still incomplete, log its size to the CONSOLE (never acp.log,
# which mirrors the raw transcript) every N seconds once the pending
# line passes the byte threshold — cold-window evidence in the
# runner's own log, not only in external probes.
SLOW_LINE_BYTES = 8192
SLOW_LINE_LOG_INTERVAL_S = 15.0
# apex-ayl.22 D-6: the default set_model budget (the G6 gap — the
# hardcoded `t = 60`; the four ACP carried cells ride 180).
DEFAULT_SET_MODEL_TIMEOUT_S = 180


def _effective_turn_timeout(case):
    """apex-ayl.22 D-6: timeouts.turn_s wins over watchdog_s (the
    case-declared floor stays for cases without a timeouts block)."""
    t = (case.get("timeouts") or {}).get("turn_s")
    if isinstance(t, (int, float)) and t > 0:
        return t
    return case.get("watchdog_s", 300)


def _effective_acp_init_timeout(case):
    """apex-ayl.22 D-6: timeouts.acp_init_s beats the
    ACP_INIT_TIMEOUT_S floor (R6: first-in-cold-window cells get 240)."""
    t = (case.get("timeouts") or {}).get("acp_init_s")
    if isinstance(t, (int, float)) and t > 0:
        return t
    return ACP_INIT_TIMEOUT_S


def _effective_set_model_timeout(case):
    """apex-ayl.22 D-6: timeouts.acp_set_model_s beats the
    DEFAULT_SET_MODEL_TIMEOUT_S floor."""
    t = (case.get("timeouts") or {}).get("acp_set_model_s")
    if isinstance(t, (int, float)) and t > 0:
        return t
    return DEFAULT_SET_MODEL_TIMEOUT_S


class AcpSession:
    def __init__(self, bin_path, home, cwd, model, log_path,
                 timeout_s=180, init_timeout_s=None,
                 set_model_timeout_s=None, extra_unset=None,
                 load_first_sid=None, env=None,
                 credential_names=None):
        self.env = dict(env) if env is not None else None
        self.bin_path = bin_path
        self.home = home
        self.cwd = cwd
        self.model = model
        self.log_path = log_path
        self.timeout_s = timeout_s
        # apex-ayl.22 D-7: the case env.provider_vars_unset extends
        # the runner L1 set (build_env unset union).
        self.extra_unset = extra_unset
        self.credential_names = credential_names
        # apex-ayl.22 D-6: the case timeouts beat the hardcoded floors
        # (timeouts.acp_init_s / timeouts.acp_set_model_s).
        self.init_timeout_s = (init_timeout_s
                               if init_timeout_s is not None
                               else ACP_INIT_TIMEOUT_S)
        self.set_model_timeout_s = (set_model_timeout_s
                                    if set_model_timeout_s is not None
                                    else DEFAULT_SET_MODEL_TIMEOUT_S)
        self.proc = None
        self._id = 0
        self.session_id = None
        # apex-ayl.70 R2-RED stage-2 (coordinator ruling 2026-09-18,
        # Option A APPROVED): load-first entry for cell-seeded cases —
        # the sid is pre-minted + pre-seeded on disk BEFORE the ACP
        # connect; start() enters via session/load (no session/new) so
        # the actor spawns COLD from disk (session_setup.rs:1023:
        # disk restore only for non-resident sessions). None = legacy
        # session/new entry (non-cell cases).
        self.load_first_sid = load_first_sid
        self.load_result = None
        self.log = _AcpLogRedactor(open(log_path, "w"))
        self.updates = []  # all session/update params, in order
        # apex-ayl.28: promptIds already consumed as a turn completion,
        # for the lifetime of this session. A completion notification
        # whose promptId is in this set is a LATE arrival for an
        # already-finished turn (the live B-ii desync: M1's
        # prompt_complete landed inside M2's read window and was read
        # as M2's completion) — it must never complete a later turn.
        self._completed_prompt_ids = set()
        # apex-ayl.32: the stdout read path bypasses the TextIOWrapper
        # (see _iter_lines) and decodes incrementally — a multi-byte
        # UTF-8 char can straddle two _iter_lines calls, so the
        # decoder state must outlive any single one.
        self._utf8 = codecs.getincrementaldecoder("utf-8")()
        # apex-bdfz BZ-4a: the line carry-over, session-scoped like the
        # decoder (see _iter_lines): complete lines read from stdout but not
        # yet yielded, in stdout order, and the text after the last newline.
        from collections import deque
        self._carry_lines = deque()
        self._carry_partial = ""
        # apex-bdfz BZ-2: the deferred inbox. A mid-turn reader that
        # consumes stdout on a turn's behalf leaves here the turn's
        # completion and its session/prompt JSON-RPC response (id == the
        # prompt's tid), plus mid-turn request responses by id;
        # prompt_collect drains the first two before it reads a byte.
        self._inbox = {"responses": {}, "completion": None,
                       "prompt_response": None}
        # apex-bdfz BZ-3: the turn prompt_send started and prompt_collect has
        # not yet collected, as its _TurnWaiter; mid-turn requests
        # (interject, mid_turn_set_model) pump stdout on its behalf. None =
        # no turn in flight, and then nothing is sent mid-turn.
        self._inflight = None

    def start(self):
        env = (self.env if self.env is not None else
               build_env(self.home, self.extra_unset,
                         self.credential_names))
        self.proc = subprocess.Popen(
            [self.bin_path, "agent", "stdio"], cwd=self.cwd, env=env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, text=True,
            preexec_fn=os.setsid)
        import select
        self._select = select
        init_params = {
            "protocolVersion": 1,
            "clientCapabilities": {"fs": {}, "terminal": False},
            "clientInfo": {"name": "ht1-l3-runner", "version": "0"},
            "_meta": {"startupHints": {"nonInteractive": True,
                                       "skipGitStatus": True}},
        }
        # apex-ayl.32: ACP_INIT_TIMEOUT_S (was hardcoded 60 — 0.98x at
        # the 424 BPS plateau for the 26,001B init line). session/new
        # shares the budget: child-side startup delays (OTEL fleet
        # policy, MCP init) can precede either response.
        resp = self.call("initialize", init_params,
                         timeout=self.init_timeout_s)
        if "result" not in resp:
            raise RuntimeError("ACP initialize failed: %s" % resp)
        if self.load_first_sid:
            # apex-ayl.70 R2-RED stage-2 (coordinator ruling 2026-09-18,
            # Option A): enter via session/load on the PRE-MINTED sid —
            # the seeded dir is already on disk at the canonical
            # persistence location (home/sessions/<enc-cwd>/<sid>/), so
            # the session is non-resident and the actor spawns cold
            # from disk with the seeded history in memory. NO
            # session/new: new→seed→load made the session resident at
            # load time and silently discarded the seed (FINDING A).
            # The load result carries models.currentModelId (the
            # seeded pre-switch model) — adopted as the actor's model
            # state (the mechanism, recorded per the ruling).
            ld = self.call(
                "session/load",
                {"sessionId": self.load_first_sid, "cwd": self.cwd,
                 "mcpServers": []},
                timeout=self.init_timeout_s)
            if "result" not in ld:
                raise RuntimeError("ACP session/load failed: %s" % ld)
            self.session_id = self.load_first_sid
            self.load_result = ld["result"]
            return self
        ns = self.call("session/new",
                       {"cwd": self.cwd, "mcpServers": [],
                        "_meta": {"modelId": self.model}},
                       timeout=self.init_timeout_s)
        if "result" not in ns:
            raise RuntimeError("ACP session/new failed: %s" % ns)
        self.session_id = ns["result"]["sessionId"]
        return self

    def _send(self, method, params, want_id=True):
        m = {"jsonrpc": "2.0", "method": method, "params": params}
        if want_id:
            self._id += 1
            m["id"] = self._id
        self.proc.stdin.write(json.dumps(m) + "\n")
        self.proc.stdin.flush()
        self.log.write("SEND: " + json.dumps(m) + "\n")
        self.log.flush()
        return None if not want_id else self._id

    def _iter_lines(self, timeout):
        """Yield raw stdout lines, each mirrored to self.log as read.

        The whole iteration is bounded by one deadline (`timeout` from
        the first read); iteration ends at the deadline or on EOF.

        apex-ayl.32: reads in ACP_READ_CHUNK blocks, not per byte.
        The 424 BPS cold-window plateau was read(1)-specific — one
        EDR/auditd-inspected syscall per character (26,001 syscalls
        for the init line). The read is `stdout.buffer.read1(
        ACP_READ_CHUNK)`: at most ONE raw read per select wake,
        returning whatever is available — bulk reads without the
        TextIOWrapper read(n) trap (read(n) on a non-interactive pipe
        blocks until n chars or EOF; on the 26,001B init line that is
        a deadlock, verified in the hardening wave). Decoding is
        incremental UTF-8 (session-scoped decoder: a multi-byte char
        can straddle two calls).

        apex-bdfz BZ-4a: the lines read but not yet yielded and the
        partial tail are session-scoped too (self._carry_lines,
        self._carry_partial), so a reader that stops early (call() on
        its reply, a turn's terminal, a mid-turn reply) leaves the rest
        of its read1 block to the next call, in stdout order. Carried
        lines are yielded first, before any select and whatever the
        deadline: they are already read. Each line is popped before it
        is yielded: an abandoned generator is closed at its suspended
        yield, so a pop after the yield would never run and the line a
        reader stopped on would reach the next reader again.
        """
        end = time.time() + timeout
        line_bytes = 0
        last_beat = 0.0
        while self._carry_lines:
            line = self._carry_lines.popleft()
            yield line
        while time.time() < end:
            r, _, _ = self._select.select([self.proc.stdout], [], [],
                                          max(0.1, end - time.time()))
            if not r:
                # Slow-line heartbeat (console only — acp.log mirrors the
                # raw transcript; sole deviation: servers_updated env
                # values are scrubbed at write time, apex-ayl.70 R2-RED).
                now = time.time()
                if (line_bytes >= SLOW_LINE_BYTES
                        and now - last_beat >= SLOW_LINE_LOG_INTERVAL_S):
                    log("ACP slow line: %d bytes of one line so far "
                        "(budget %ss, apex-ayl.32 cold window?)"
                        % (line_bytes, timeout))
                    last_beat = now
                continue
            raw = self.proc.stdout.buffer.read1(ACP_READ_CHUNK)
            if not raw:
                return
            text = self._utf8.decode(raw)
            if text:
                self.log.write(text)
            buf = self._carry_partial + text
            line_bytes += len(raw)
            while True:
                nl = buf.find("\n")
                if nl < 0:
                    break
                line, buf = buf[:nl], buf[nl + 1:]
                line = line.strip()
                if line:
                    self._carry_lines.append(line)
            self._carry_partial = buf
            line_bytes = len(self._carry_partial)
            while self._carry_lines:
                line = self._carry_lines.popleft()
                yield line

    def _session_scoped(self, d):
        """apex-bdfz BZ-3 (plan r3 section 2.2, review F-10): the
        session-identity guard, owned by the reader. A notification whose
        params.sessionId is neither absent nor this session's belongs to
        another session; every reader of stdout drops it with this one
        predicate, before _track_update and before any waiter sees it. A
        JSON-RPC response carries no method and is always in scope."""
        if d.get("method") is None:
            return True
        p = d.get("params") or {}
        return p.get("sessionId") in (None, self.session_id)

    def _track_update(self, d):
        if d.get("method") != "session/update":
            return
        p = d.get("params") or {}
        u = p.get("update") or {}
        u["_sessionId"] = p.get("sessionId")
        self.updates.append(u)

    def _read_until(self, target_id, timeout):
        for line in self._iter_lines(timeout):
            try:
                d = json.loads(line)
            except Exception:
                continue
            self._track_update(d)
            if isinstance(d.get("id"), int) and d["id"] == target_id:
                return d
        return {"error": {"message": "timeout after %ss" % timeout}}

    def call(self, method, params, timeout=None):
        tid = self._send(method, params)
        return self._read_until(tid, timeout or self.timeout_s)

    class _TurnWaiter(object):
        """The turn-completion matcher of _read_turn_completion as a
        feedable object (apex-bdfz BZ-1), so every reader of stdout
        applies the identical rules: apex-ayl.28 promptId correlation
        with the consumed-guard, and the ACP-2 error grace.

        feed(d) consumes one parsed JSON-RPC object and returns
          "wait"  - not this turn's terminal: keep reading;
          "grace" - a turn_completed error terminal, which carries no
                    agentResult: the turn is over, but the matching
                    trailing prompt_complete (the provider detail) is
                    still read, up to wait()'s deadline;
          "done"  - nothing further is needed: a normal terminal, a
                    prompt_complete terminal (it already carries
                    agentResult, error or not), the grace upgrade, or
                    close() once the grace window ended.
        The reader owns the session-identity guard and _track_update
        and applies them before feed. completed_prompt_ids is the
        session's consumed set; the completion's promptId joins it once
        the waiter is done.
        """

        def __init__(self, expected_prompt_id=None, tid=None,
                     completed_prompt_ids=None):
            self.expected_prompt_id = expected_prompt_id
            self.tid = tid
            self.completed_prompt_ids = (
                completed_prompt_ids if completed_prompt_ids is not None
                else set())
            self.completion = None
            self.response = None
            # Older-shell fallback tracking: whether this turn's expected
            # promptId showed up in a queue row. On this binary it always
            # does (queue rows reuse the prompt id); if a future binary
            # ignored the client-chosen id, completions carry the binary's
            # own id and the first unclaimed completion is adopted (the
            # pre-fix behavior — the consumed-guard still protects it).
            self.expected_seen = False
            self.state = "wait"
            self.grace_deadline = None

        def _match(self, pid):
            if pid and pid in self.completed_prompt_ids:
                return False
            if self.expected_prompt_id and pid:
                if pid == self.expected_prompt_id:
                    return True
                if self.expected_seen:
                    return False  # ours is known; this one is foreign
                return True      # older-shell fallback (first unclaimed)
            if self.expected_prompt_id and pid is None:
                return True      # unmapped completion (older shell)
            return True          # no expected id: first unclaimed

        def feed(self, d):
            if self.state == "done":
                return "done"
            if self.state == "grace":
                return self._feed_grace(d)
            if d.get("method") is None:
                # JSON-RPC response (result or error): informational.
                if isinstance(d.get("id"), int) and d["id"] == self.tid \
                        and self.response is None:
                    self.response = d
                return "wait"
            p = d.get("params") or {}
            m = d.get("method")
            if m == "_x.ai/queue/changed":
                # apex-ayl.28: queue rows carry the prompt identity
                # (entries[].id "reuses the prompt's unique prompt_id",
                # runningPromptId = the designated correlation signal —
                # xai-prompt-queue/src/types.rs). Seeing ours here is
                # the confirmation that the binary adopted it.
                if self.expected_prompt_id and not self.expected_seen:
                    if p.get("runningPromptId") == self.expected_prompt_id \
                            or any(e.get("id") == self.expected_prompt_id
                                   for e in p.get("entries") or []):
                        self.expected_seen = True
                return "wait"
            if m == "_x.ai/session/prompt_complete":
                pid = p.get("promptId")
                if not self._match(pid):
                    log("ACP desync guard: ignoring prompt_complete "
                        "pid=%s (expected %s) — late/foreign "
                        "completion" % (pid, self.expected_prompt_id))
                    return "wait"
                self.completion = {"promptId": pid or self.expected_prompt_id,
                                   "stopReason": p.get("stopReason"),
                                   "agentResult": p.get("agentResult"),
                                   "via": "prompt_complete"}
                return self.close()
            if m == "_x.ai/session_notification":
                u = p.get("update") or {}
                if u.get("sessionUpdate") != "turn_completed":
                    return "wait"
                pid = u.get("prompt_id")
                if not self._match(pid):
                    log("ACP desync guard: ignoring turn_completed "
                        "pid=%s (expected %s) — late/foreign "
                        "completion" % (pid, self.expected_prompt_id))
                    return "wait"
                self.completion = {"promptId": pid or self.expected_prompt_id,
                                   "stopReason": u.get("stop_reason"),
                                   "agentResult": None,
                                   "via": "turn_completed"}
                if u.get("stop_reason") == "error":
                    # ACP-2: turn_completed carries no agentResult;
                    # the provider error text lives in the
                    # matching prompt_complete, which lands a few
                    # lines later (wave-2 forensics: acp.log
                    # turn_completed+3 lines). Keep reading briefly
                    # for it so the synthesized acp_error carries
                    # the detail. Match by promptId; when the turn
                    # has no mapped promptId, take the first
                    # prompt_complete for this session (recency).
                    # Timeout fallback = the generic shape above.
                    self.state = "grace"
                    self.grace_deadline = time.time() + ACP_ERROR_GRACE_S
                    return "grace"
                return self.close()
            return "wait"

        def _feed_grace(self, d):
            if d.get("method") != "_x.ai/session/prompt_complete":
                return "grace"
            gp = d.get("params") or {}
            gpid = gp.get("promptId")
            if not self._match(gpid):
                return "grace"
            self.completion = {"promptId": gpid
                                   or self.completion["promptId"],
                               "stopReason": self.completion["stopReason"],
                               "agentResult": gp.get("agentResult"),
                               "via": "prompt_complete"}
            return self.close()

        def wait(self, deadline=None):
            """The absolute time the reader must keep reading to: the
            caller's own deadline while the turn runs; the grace deadline
            while the error detail is owed (not cut by the caller's
            deadline, as the sequential second read never was); None once
            nothing is outstanding."""
            if self.state == "grace":
                return self.grace_deadline
            if self.state == "wait":
                return deadline
            return None

        def close(self):
            """Finish: the completion stands (the grace window may have
            closed without its prompt_complete) and its promptId joins
            the consumed set."""
            if self.state != "done":
                self.state = "done"
                if self.completion is not None \
                        and self.completion.get("promptId"):
                    self.completed_prompt_ids.add(
                        self.completion["promptId"])
            return "done"

        def pump(self, session, timeout):
            """The sequential read of _read_turn_completion, unchanged:
            one _iter_lines window of `timeout`; for every notification
            the session-identity guard, then _track_update, then feed;
            on "grace" the same second bounded read as before, a fresh
            ACP_ERROR_GRACE_S window in which only a matching
            prompt_complete for this session counts. Returns
            (completion, response)."""
            for line in session._iter_lines(timeout):
                try:
                    d = json.loads(line)
                except Exception:
                    continue
                if not session._session_scoped(d):
                    continue
                session._track_update(d)
                state = self.feed(d)
                if state == "grace":
                    for gline in session._iter_lines(ACP_ERROR_GRACE_S):
                        try:
                            gd = json.loads(gline)
                        except Exception:
                            continue
                        if gd.get("method") \
                                != "_x.ai/session/prompt_complete":
                            continue
                        if not session._session_scoped(gd):
                            continue
                        if self.feed(gd) == "done":
                            break
                    break
                if state == "done":
                    break
            self.close()
            return self.completion, self.response

    def _read_turn_completion(self, tid, timeout, expected_prompt_id=None):
        """Wait for a prompt turn to end on the notification rail.

        The grok-responses ACP binary announces turn end via the
        `_x.ai/session/prompt_complete` notification ({sessionId,
        promptId, stopReason, agentResult} — payload always carries the
        promptId on this binary: turn_completion.rs prompt_complete_
        payload) and the `turn_completed` session_notification
        (prompt_id). apex-ayl.28: `expected_prompt_id` is the prompt's
        OWN identity — client-chosen, sent in the session/prompt
        params._meta.promptId (accepted at acp_agent.rs:1121-1126,
        echoed in the queue rows — xai-prompt-queue types.rs
        "reusing the prompt's unique prompt_id" — and in the response
        _meta). Completions are consumed ONLY on that id (plus the
        consumed-guard for ids already delivered to a previous step);
        arrival order never decides. A JSON-RPC response for `tid`
        (result or error, arriving before or after the notification)
        is captured informationally; its absence never fails the turn.
        Returns (completion, response); either may be None.

        apex-bdfz BZ-1: the rules live in _TurnWaiter; this is its thin
        sequential driver (keeps reading while feed answers "wait", and
        to the grace deadline while it answers "grace").
        """
        waiter = self._TurnWaiter(
            expected_prompt_id, tid=tid,
            completed_prompt_ids=self._completed_prompt_ids)
        return waiter.pump(self, timeout)

    def prompt_send(self, text):
        """Send one prompt turn; return (tid, prompt_id) without reading.

        apex-bdfz BZ-2: the send half of prompt() — the client-chosen
        uuid4 promptId in params._meta (apex-ayl.28) and the session/prompt
        request, exactly as prompt() sends them. tid is the request's
        JSON-RPC id; prompt_collect waits for the turn.
        """
        prompt_id = str(uuid.uuid4())
        tid = self._send("session/prompt",
                         {"sessionId": self.session_id,
                          "prompt": [{"type": "text", "text": text}],
                          "_meta": {"promptId": prompt_id}})
        self._inflight = self._TurnWaiter(
            prompt_id, tid=tid,
            completed_prompt_ids=self._completed_prompt_ids)
        return tid, prompt_id

    def prompt_collect(self, tid, prompt_id, timeout):
        """Wait for the turn prompt_send started; return (resp, stop).

        apex-bdfz BZ-2: the deferred inbox is drained before a single byte
        is read. A mid-turn reader that consumed stdout on this turn's
        behalf leaves there the turn's completion and its session/prompt
        JSON-RPC response (id == tid); a deferred completion is returned
        without reading. Otherwise _read_turn_completion reads for
        `timeout` seconds (relative, as that reader takes them), and a
        deferred response stays the first one seen. The tail is prompt()'s:
        a JSON-RPC response is the canonical resp; else the
        notification-only completion is synthesized; else the timeout
        terminal, which names `timeout`.
        """
        self._inflight = None
        completion = self._inbox["completion"]
        response = self._inbox["prompt_response"]
        self._inbox["completion"] = None
        self._inbox["prompt_response"] = None
        if completion is None:
            completion, read_response = self._read_turn_completion(
                tid, timeout, expected_prompt_id=prompt_id)
            if response is None:
                response = read_response
        if response is not None:
            # A JSON-RPC response (result or error) arrived: keep it as
            # the canonical resp so caller error surfaces (acp_error +
            # FAIL) fire unchanged.
            stop = None
            if "result" in response:
                stop = (response["result"] or {}).get("stopReason")
            if stop is None and completion is not None:
                stop = completion.get("stopReason")
            return response, stop
        if completion is not None:
            # Notification-only completion (the normal path on
            # grok-responses): synthesize the resp. A turn that ended
            # in error still surfaces through the caller's
            # "error" in resp check via stopReason=error.
            stop = completion.get("stopReason")
            if stop == "error":
                return ({"error": {"message": completion.get(
                                     "agentResult") or
                                     "turn ended with stopReason=error",
                                   "via": completion["via"],
                                   "promptId": completion.get("promptId")}},
                        stop)
            return ({"result": {"stopReason": stop,
                                "_via": completion["via"],
                                "promptId": completion.get("promptId")}},
                    stop)
        return {"error": {"message": "timeout after %ss" % timeout}}, None

    def prompt(self, text, timeout=None):
        """Send one prompt turn and wait for ITS completion.

        apex-ayl.28: the prompt identity is client-chosen — a uuid4
        sent in params._meta.promptId, which the binary adopts as the
        turn's promptId (acp_agent.rs:1121-1126: client value or a
        binary-generated uuid fallback) and echoes in every
        completion notification and the queue rows. The read loop
        therefore consumes only this turn's completion; late
        completions for previously finished turns (the live B-ii
        desync) are dropped by the consumed-guard in
        _read_turn_completion.

        apex-bdfz BZ-2: prompt_send + prompt_collect, with the turn
        budget t computed as before.
        """
        t = timeout or self.timeout_s
        tid, prompt_id = self.prompt_send(text)
        return self.prompt_collect(tid, prompt_id, t)

    def set_model(self, model):
        """Switch the session model; wait for the switch verdict.

        The binary answers `session/set_model` with a JSON-RPC response
        (result on success, error on refusal); as a hedge, an error
        carrying `error`/`error_type` fields in a
        session_notification is surfaced the same way. Both error
        shapes return the {"error": ...} form the M-2 surface
        (acp_error event + FAIL) keys on; a timeout returns that shape
        for the never-answers world.
        """
        self.model = model
        tid = self._send("session/set_model",
                          {"sessionId": self.session_id, "modelId": model})
        # apex-ayl.22 D-6 (G6): the case set_model budget (was t=60).
        t = self.set_model_timeout_s
        for line in self._iter_lines(t):
            try:
                d = json.loads(line)
            except Exception:
                continue
            if d.get("method") is None:
                if isinstance(d.get("id"), int) and d["id"] == tid:
                    return d  # result OR error response
                continue
            if not self._session_scoped(d):
                continue
            self._track_update(d)
            refusal = self._set_model_refusal(d)
            if refusal is not None:
                return refusal
        return {"error": {"message": "timeout after %ss waiting for "
                                     "set_model response" % t}}

    @staticmethod
    def _set_model_refusal(d):
        """set_model's notification hedge: a session_notification carrying
        error / error_type answers the switch as a refusal, in the
        {"error": ...} shape the M-2 surface keys on. apex-bdfz BZ-3: shared
        by set_model and mid_turn_set_model. None when d is no refusal."""
        p = d.get("params") or {}
        u = (p.get("update") or {}) if d.get("method") == \
            "_x.ai/session_notification" else {}
        if u.get("sessionUpdate") and \
                (u.get("error") or u.get("error_type")):
            return {"error": {"message": "set_model failed via "
                                          "notification",
                              "data": u}}
        return None

    def interject(self, text, interjection_id=None, timeout=None):
        """Queue a mid-turn interjection into the turn prompt_send started.

        apex-bdfz BZ-3 (plan r3 section 2.2 item 4): sends the x.ai/interject
        ext request {sessionId, text, interjectionId?} and reads its reply
        through the single-reader pump while the turn keeps running, so the
        turn's updates keep reaching self.updates and its completion keeps
        being matched. The wire method is _x.ai/interject: the agent's ACP
        library decodes a client request whose method starts with "_" as the
        ext method named without it, and answers an unprefixed one
        method_not_found (agent-client-protocol 0.10.4,
        AgentSide::decode_request). Returns (resp, witness): resp is the raw
        JSON-RPC reply ({"status": "queued"}, or an error reply such as
        "session not found", returned as is), the timeout shape when no reply
        came within `timeout` (default self.timeout_s), or the not-sent shape
        when no turn is in flight; witness records the exchange."""
        params = {"sessionId": self.session_id, "text": text}
        if interjection_id is not None:
            params["interjectionId"] = interjection_id
        t = timeout or self.timeout_s
        reply, witness = self._mid_turn_request("_x.ai/interject", params, t)
        if not witness["sent"]:
            return ({"error": {"message": "no turn in flight: "
                                          "x.ai/interject not sent"}},
                    witness)
        if reply is None:
            return {"error": {"message": "timeout after %ss" % t}}, witness
        return reply, witness

    def mid_turn_set_model(self, model):
        """Switch the session model while the turn prompt_send started runs.

        apex-bdfz BZ-3 (plan r3 section 2.2 item 5): set_model's request,
        set_model_timeout_s budget and verdict shapes (the JSON-RPC response,
        result or error; the notification-carried refusal; the timeout
        shape), read through the single-reader pump because the turn's lines
        share the rail. Returns (resp, witness), as interject does."""
        t = self.set_model_timeout_s
        reply, witness = self._mid_turn_request(
            "session/set_model",
            {"sessionId": self.session_id, "modelId": model}, t,
            verdict=self._set_model_refusal)
        if not witness["sent"]:
            return ({"error": {"message": "no turn in flight: "
                                          "session/set_model not sent"}},
                    witness)
        self.model = model
        if reply is None:
            return ({"error": {"message": "timeout after %ss waiting for "
                                          "set_model response" % t}},
                    witness)
        return reply, witness

    def _mid_turn_request(self, method, params, timeout, verdict=None):
        """Send one request into the in-flight turn and pump its reply.

        apex-bdfz BZ-3 (plan r3 sections 2.1-2.3): nothing is sent unless a
        turn is in flight (prompt_send recorded it and its completion has not
        been matched). The request id is registered in the deferred inbox and
        one _iter_lines window of `timeout` is read, each line through
        _pump_line, until the reply arrives or `verdict(d)` names a
        notification that answers the request. A turn whose error terminal
        still owes its grace read is read to the grace deadline and its
        waiter closed, so the inbox only ever receives a closed completion.
        Returns (reply or None, witness)."""
        waiter = self._inflight
        witness = {"method": method, "id": None,
                   "prompt_id": (waiter.expected_prompt_id
                                 if waiter is not None else None),
                   "sent": False, "sent_at": None, "reply_at": None}
        if self.session_id is None or waiter is None \
                or waiter.state != "wait":
            return None, witness
        rid = self._send(method, params)
        self._inbox["responses"][rid] = None
        witness.update(id=rid, sent=True, sent_at=time.time())
        reply = None
        for line in self._iter_lines(timeout):
            d = self._pump_line(waiter, line)
            if d is None:
                continue
            reply = self._inbox["responses"][rid]
            if reply is None and verdict is not None \
                    and d.get("method") is not None:
                reply = verdict(d)
            if reply is not None:
                witness["reply_at"] = time.time()
                break
        if waiter.state == "grace":
            for line in self._iter_lines(
                    max(0.0, waiter.wait() - time.time())):
                self._pump_line(waiter, line)
                if waiter.state == "done":
                    break
            if waiter.state == "grace":
                waiter.close()
                self._inbox["completion"] = waiter.completion
        self._inbox["responses"].pop(rid, None)
        return reply, witness

    def _pump_line(self, waiter, line):
        """One stdout line on the mid-turn pump: parsed; dropped when foreign
        (_session_scoped, in front of tracking and feeding); a JSON-RPC
        response is routed into the deferred inbox (the turn's own
        session/prompt response, the first one seen; a registered mid-turn
        reply); then _track_update and the turn's _TurnWaiter, whose matched
        completion is left in the inbox for prompt_collect. A grace window
        that has run out closes the waiter as the sequential grace read
        does. Returns the parsed object, or None when unparseable or
        foreign."""
        try:
            d = json.loads(line)
        except Exception:
            return None
        if not self._session_scoped(d):
            return None
        rid = d.get("id")
        if d.get("method") is None and isinstance(rid, int):
            if rid == waiter.tid:
                if self._inbox["prompt_response"] is None:
                    self._inbox["prompt_response"] = d
            elif rid in self._inbox["responses"] \
                    and self._inbox["responses"][rid] is None:
                self._inbox["responses"][rid] = d
        self._track_update(d)
        if waiter.state == "grace" and time.time() >= waiter.wait():
            waiter.close()
            self._inbox["completion"] = waiter.completion
        elif waiter.state != "done" and waiter.feed(d) == "done":
            self._inbox["completion"] = waiter.completion
        return d

    @staticmethod
    def _trigger_hit(arm, d, counters):
        """Whether an armed interject trigger holds after the parsed stdout
        line d (apex-bdfz BZ-4, plan r3 section 2.3).

        arm is the armed record {"kind", "arg", ...}: after_tool_calls N holds
        once N distinct calls are announced; after_tool NAME holds on the
        tool_call line announcing NAME; after_s T holds once T seconds have
        passed since counters["t_sent"], the parent prompt's send time.
        counters is the turn's running state, shared by every arm and folded
        here: only a session/update whose sessionUpdate is exactly "tool_call"
        announces a call (tool_call_update is a status transition and
        tool_call_delta_chunk an argument fragment), and counters
        ["tool_call_ids"] keeps each toolCallId once, so a re-emitted Pending
        line, or one line offered to several arms, never counts twice. A
        call's name is _meta["x.ai/tool"]["name"] when stamped, else its
        title (native tool_search calls carry no x.ai/tool); kind is never a
        trigger. The clock is the module-level time, which the offline tests
        patch; time.monotonic would be invisible to them."""
        u = None
        if isinstance(d, dict) and d.get("method") == "session/update":
            u = (d.get("params") or {}).get("update") or {}
            if u.get("sessionUpdate") != "tool_call":
                u = None
            elif u.get("toolCallId") not in counters["tool_call_ids"]:
                counters["tool_call_ids"].append(u.get("toolCallId"))
        kind, arg = arm["kind"], arm["arg"]
        if kind == "after_tool_calls":
            return len(counters["tool_call_ids"]) >= arg
        if kind == "after_tool":
            if u is None:
                return False
            tool = (u.get("_meta") or {}).get("x.ai/tool")
            name = tool.get("name") if isinstance(tool, dict) else None
            return (name if name is not None else u.get("title")) == arg
        if kind == "after_s":
            return time.time() - counters["t_sent"] >= arg
        raise ValueError("unknown interject trigger: %r" % (kind,))

    # apex-bdfz BZ-5 (plan r3 sections 2.4-2.5): the disarm reasons that are
    # rig failures (D-4, D-6). harness_caused is derived from disarm_reason by
    # this table, so a window closed by the turn's own terminal or budget
    # (turn_completed, turn_error, turn_timeout) stays a model-path reading.
    _HARNESS_DISARM_REASONS = ("ext_error", "no_inflight_turn", "session_died")

    def _pump_mid_turn(self, arms, waiter, deadline, step=0, t_sent=None):
        """Read stdout while the turn prompt_send started is in flight, firing
        its interject arms (apex-bdfz BZ-5, plan r3 sections 2.3-2.5).

        arms is the turn step's interject list; deadline the turn's absolute
        deadline; step the step index in arm_id "step<si>.arm<ai>"; t_sent the
        parent prompt's send time (default now: the pump starts right after
        prompt_send, which never reads). Every line goes through _pump_line
        (session guard, inbox routing, tracking, the turn's waiter). While the
        turn runs, an armed arm fires on the first line its trigger holds on
        (_trigger_hit): the request is sent at once and its reply matched by
        JSON-RPC id within interject_timeout_s (default min(30, the turn's
        remaining budget)), never beyond the turn's deadline. The pump reads
        to the turn's terminal: the matched completion closes every un-fired
        arm and nothing more is sent (D-1; D-2 for an error terminal, whose
        grace read the pump performs); owed replies are still awaited; the
        turn's deadline closes un-fired arms (D-5); end of file disarms every
        unsettled arm (D-6); with no turn in flight nothing is sent (D-4).

        Returns one record per arm, in arm order: status fired | missed |
        mismatched_tool | send_timeout | disarmed (the section 2.4
        precedence), disarm_reason, harness_caused, the runner cause, fired_at
        and the reply. mid_turn is never taken from the reply: a fired arm is
        mid-turn only on evidence that its request went out while the turn
        ran: the turn_completed is stamped (_meta.agentTimestampMs, the
        agent's epoch milliseconds) after the send, a queue row running this
        prompt is read after the send, or the x.ai/session/interjection
        broadcast of its interjection (its interjectionId, else its text)
        lands before the turn's completion. A line counts as read after the
        send unless it was already read when the request went out (the
        carry-over still held it)."""
        t_sent = time.time() if t_sent is None else t_sent
        pid = waiter.expected_prompt_id if waiter is not None else None
        counters = {"tool_call_ids": [], "t_sent": t_sent}
        updates0 = len(self.updates)
        recs = []
        for ai, spec in enumerate(arms):
            at = dict(spec.get("at") or {})
            kind = next(iter(at), None)
            rec = {"arm_id": "step%d.arm%d" % (step, ai), "at": at,
                   "kind": kind, "arg": at.get(kind),
                   "send": spec.get("send", "interject"), "status": "armed",
                   "fired": False, "disarm_reason": None,
                   "harness_caused": False, "cause": None, "fired_at": None,
                   "mid_turn": False, "mid_turn_evidence": [], "resp": None,
                   "prompt_id": pid, "request_id": None, "sent_at": None,
                   "reply_at": None, "_spec": spec}
            if rec["send"] == "set_model":
                rec["model"] = spec.get("model")
            else:
                rec["interject_text"] = spec.get("text")
            recs.append(rec)
        seen = {"lines": 0, "completion_line": None, "completion_ms": None}
        owed = []

        def settle(rec, status, reason=None):
            harness = (status == "send_timeout"
                       or reason in self._HARNESS_DISARM_REASONS)
            rec.update(status=status, fired=status == "fired",
                       disarm_reason=reason, harness_caused=harness)
            if status == "send_timeout":
                rec["cause"] = "interject-send-timeout: %s" % rec["arm_id"]
            elif harness:
                rec["cause"] = "interject-unavailable: %s.%s" % (
                    rec["arm_id"], reason)

        def close(rec, reason):
            # An un-fired arm's window closed: a rig cause disarms it; the
            # turn's terminal or budget leaves a model-path reading.
            if reason in self._HARNESS_DISARM_REASONS:
                settle(rec, "disarmed", reason)
            elif rec["kind"] == "after_tool":
                settle(rec, "mismatched_tool", reason)
            else:
                settle(rec, "missed", reason)

        def unregister(rec):
            owed.remove(rec)
            self._inbox["responses"].pop(rec["request_id"], None)

        def credit(rec, name):
            if name not in rec["mid_turn_evidence"]:
                rec["mid_turn_evidence"].append(name)

        def fire(rec):
            spec = rec["_spec"]
            if rec["send"] == "set_model":
                method = "session/set_model"
                params = {"sessionId": self.session_id,
                          "modelId": rec["model"]}
            else:
                method = "_x.ai/interject"
                params = {"sessionId": self.session_id,
                          "text": rec["interject_text"]}
                if spec.get("interjection_id") is not None:
                    params["interjectionId"] = spec["interjection_id"]
            rid = self._send(method, params)
            self._inbox["responses"][rid] = None
            now = time.time()
            budget = (spec.get("interject_timeout_s")
                      or min(30.0, deadline - now))
            rec.update(status="sent", request_id=rid, sent_at=now,
                       fired_at={"elapsed_s": now - t_sent,
                                 "n_tool_calls":
                                     len(counters["tool_call_ids"]),
                                 "n_updates": len(self.updates) - updates0},
                       _reply_by=min(now + budget, deadline),
                       _read_before=seen["lines"] + len(self._carry_lines)
                       + (1 if self._carry_partial else 0))
            if rec["send"] == "set_model":
                self.model = rec["model"]
            owed.append(rec)

        def witness(d):
            m = d.get("method")
            p = d.get("params") or {}
            after_send = [r for r in recs if r["request_id"] is not None
                          and seen["lines"] > r["_read_before"]]
            if m == "_x.ai/queue/changed" and pid \
                    and p.get("runningPromptId") == pid:
                for rec in after_send:
                    credit(rec, "running_prompt_after_send")
            elif m == "_x.ai/session/interjection" \
                    and seen["completion_line"] is None:
                for rec in after_send:
                    iid = rec["_spec"].get("interjection_id")
                    if rec["send"] == "interject" and (
                            p.get("interjectionId") == iid if iid is not None
                            else p.get("text") == rec["interject_text"]) \
                            and "interjection_broadcast_before_completion" \
                            not in rec["mid_turn_evidence"]:
                        credit(rec, "interjection_broadcast_before_completion")
                        break

        def horizon():
            # Read to the turn's terminal; then through its grace read and for
            # every reply still owed; None once nothing is left to read for.
            if waiter.state == "wait":
                return deadline
            ends = [rec["_reply_by"] for rec in owed]
            if waiter.state == "grace":
                ends.append(waiter.wait())
            return max(ends) if ends else None

        if self.session_id is None or waiter is None \
                or waiter is not self._inflight or waiter.state != "wait":
            for rec in recs:
                close(rec, "no_inflight_turn")
            return [{k: v for k, v in rec.items() if not k.startswith("_")}
                    for rec in recs]
        eof = False
        while True:
            window = horizon()
            if window is None:
                break
            for line in self._iter_lines(max(0.0, window - time.time())):
                seen["lines"] += 1
                before = waiter.state
                d = self._pump_line(waiter, line)
                now = time.time()
                if before == "wait" and waiter.state != "wait":
                    seen["completion_line"] = seen["lines"]
                    if d is not None and d.get("method") == \
                            "_x.ai/session_notification":
                        seen["completion_ms"] = ((d.get("params") or {})
                                                 .get("_meta") or {}).get(
                                                     "agentTimestampMs")
                if d is not None:
                    witness(d)
                    if d.get("method") is None:
                        for rec in list(owed):
                            reply = self._inbox["responses"].get(
                                rec["request_id"])
                            if reply is not None:
                                unregister(rec)
                                rec.update(resp=reply, reply_at=now)
                                if "error" in reply:
                                    settle(rec, "disarmed", "ext_error")
                                else:
                                    settle(rec, "fired")
                for rec in list(owed):
                    if now >= rec["_reply_by"]:
                        unregister(rec)
                        settle(rec, "send_timeout")
                if waiter.state != "wait":
                    # D-1 / D-2: the turn is over; nothing more is sent.
                    reason = ("turn_error" if (waiter.completion or {}).get(
                        "stopReason") == "error" else "turn_completed")
                    for rec in recs:
                        if rec["status"] == "armed":
                            close(rec, reason)
                elif d is not None:
                    for rec in recs:
                        if rec["status"] == "armed" \
                                and self._trigger_hit(rec, d, counters):
                            fire(rec)
                if horizon() != window:
                    break
            else:
                now = time.time()
                if now < window:
                    # _iter_lines ended before its deadline: end of file.
                    eof = True
                    break
                for rec in list(owed):
                    if now >= rec["_reply_by"]:
                        unregister(rec)
                        settle(rec, "send_timeout")
                if waiter.state == "wait":
                    # D-5: the turn's own budget ran out first.
                    for rec in recs:
                        if rec["status"] == "armed":
                            close(rec, "turn_timeout")
                    for rec in list(owed):
                        unregister(rec)
                        settle(rec, "send_timeout")
                    break
                if waiter.state == "grace" and now >= waiter.wait():
                    waiter.close()
                    self._inbox["completion"] = waiter.completion
        if eof:
            for rec in recs:
                if rec["status"] == "armed":
                    close(rec, "session_died")
            for rec in list(owed):
                unregister(rec)
                settle(rec, "disarmed", "session_died")
            if waiter.state == "grace":
                waiter.close()
                self._inbox["completion"] = waiter.completion
        for rec in recs:
            if rec["request_id"] is not None \
                    and seen["completion_ms"] is not None \
                    and seen["completion_ms"] > rec["sent_at"] * 1000:
                credit(rec, "completion_after_send")
            rec["mid_turn"] = rec["fired"] and bool(rec["mid_turn_evidence"])
        return [{k: v for k, v in rec.items() if not k.startswith("_")}
                for rec in recs]

    def available_commands(self):
        tools = []
        for u in self.updates:
            if u.get("sessionUpdate") == "available_commands_update":
                for c in u.get("availableCommands") or []:
                    tools.append(c.get("name"))
        return tools

    def wire_model_calls(self, capture_dir):
        return count_wire_model_calls(capture_dir)

    def kill(self):
        try:
            if self.proc and self.proc.poll() is None:
                os.killpg(os.getpgid(self.proc.pid), signal.SIGKILL)
        except Exception:
            pass
        try:
            self.log.close()
        except Exception:
            pass


class Wiretap:
    def __init__(self, port, upstream, capture_dir, ambient_key,
                 route_map=None):
        self.port = port
        self.capture_dir = capture_dir
        env = dict(os.environ)
        if ambient_key:
            # HYG-1: hand the key to wiretap via env, never argv
            # (argv is ps-visible; 5 dead orphans leaked it that way).
            env["WIRETAP_AMBIENT_KEY"] = ambient_key
        argv = [sys.executable, os.path.abspath(WIRETAP),
                "--port", str(port), "--upstream", upstream,
                "--capture", capture_dir]
        if route_map is not None:
            # REDTEAM-REFRESH T2 (r3 §4.2.1): the route map is the whole
            # routing authority, so it is never an argv option — it names each
            # row's origin and credential VARIABLE and goes through the same
            # env seam as the ambient key. Values are not in it by
            # construction: derive_row_routes reads names only.
            env["WIRETAP_ROUTE_MAP"] = json.dumps(route_map, sort_keys=True)
        self.argv = argv
        self.env = env
        self.proc = subprocess.Popen(argv, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE, env=env)
        self.out_log = open(capture_dir + ".wiretap-stdout.log", "w")

    def wait_ready(self, timeout=15):
        import socket as sk
        end = time.time() + timeout
        while time.time() < end:
            try:
                with sk.create_connection(("127.0.0.1", self.port), 0.5):
                    return True
            except OSError:
                time.sleep(0.2)
        return False

    def stop(self):
        try:
            if self.proc.poll() is None:
                self.proc.terminate()
                try:
                    self.proc.wait(timeout=5)
                except Exception:
                    self.proc.kill()
        except Exception:
            pass
        try:
            self.out_log.close()
        except Exception:
            pass


# ---------------------------------------------------------------------------
# JSONPath-lite + assert engine (spec §4 contract)
# ---------------------------------------------------------------------------

def get_path(obj, path):
    """$.a.b[0].c style lookup. Returns (found, value)."""
    if path is None:
        return (True, obj)
    cur = obj
    rest = path.strip()
    if rest.startswith("$."):
        rest = rest[2:]
    elif rest.startswith("$"):
        rest = rest[1:]
    if rest == "":
        return (True, cur)
    for tok in re.findall(r'[^\.\[\]]+|\[\d+\]', rest):
        if tok.startswith("["):
            idx = int(tok[1:-1])
            if not isinstance(cur, list) or idx >= len(cur):
                return (False, None)
            cur = cur[idx]
        else:
            if not isinstance(cur, dict) or tok not in cur:
                return (False, None)
            cur = cur[tok]
    return (True, cur)


class AssertResult:
    def __init__(self, spec, kind, ok, detail, evidence, recon=False,
                 cite=None):
        self.spec = spec
        self.kind = kind
        self.ok = ok
        self.detail = detail
        self.evidence = evidence
        self.recon = recon
        # apex-ayl.22 D-8: machine evidence index {file,
        # request_n, model, byte_range} (wire) / {event_line}
        # (ndjson) / {file} (artifact). The textual `evidence` stays
        # for humans; this is the machine-readable citation the
        # require_wire_evidence audit resolves.
        self.cite = cite


def _val_eq(a, b):
    if isinstance(b, str) and isinstance(a, str):
        return a == b
    if a is None or b is None:
        return a is b or (a == b)
    if isinstance(a, (int, float)) and isinstance(b, (int, float)):
        return a == b
    return a == b


def check_ndjson(spec, events, fmt, ctx=None):
    op = spec.get("op")
    ev = spec.get("event")
    # apex-bdfz BZ-7 (plan r3 section 2.5.2): count, eq and ne apply the
    # optional where map before counting or selecting. Each entry is an
    # equality filter on the event's own field (dot-path key, scalar value,
    # compared as _wire_filter compares; an absent field never matches). The
    # shape is enforced at validation by case.schema.json and run.py
    # _validate_where.
    where = spec.get("where") or {}

    def selected(e):
        if e.get("type") != ev:
            return False
        for k, v in where.items():
            found, val = get_path(e, k)
            if not found or not _val_eq(val, v):
                return False
        return True

    shown = ev if not where else "%s where %s" % (
        ev, json.dumps(where, sort_keys=True))
    if op == "count":
        n = sum(1 for e in events if selected(e))
        ok = n >= spec.get("min", 0) and n <= spec.get("max", 10 ** 9)
        return AssertResult(spec, "ndjson.count", ok,
                            "%d events of type %s" % (n, shown),
                            "ndjson: %d x type=%s" % (n, shown))
    if op in ("absent", "present"):
        n = sum(1 for e in events if e.get("type") == ev)
        ok = (n == 0) if op == "absent" else (n >= 1)
        detail = "%d events of type %s" % (n, ev)
        if (op == "present" and not ok and spec.get("absent_ok")
                and ctx is not None
                and ctx.last_exit not in (0, None)):
            # absent_ok: the event's absence is accepted ONLY on an error
            # terminal (run's last exit non-zero). Pins the honest-failure
            # shape (no clean end event because the turn errored) while a
            # silent no-terminal run (exit 0, no event) still FAILs.
            ok = True
            detail += " (absent_ok: error terminal, exit=%s)" % ctx.last_exit
        return AssertResult(spec, "ndjson.%s" % op, ok, detail,
                            "ndjson: %d x type=%s" % (n, ev))
    if op in ("eq", "ne"):
        matches = [e for e in events if selected(e)]
        which = spec.get("which", "first")
        m = matches[0] if which == "first" else matches[-1] if matches else None
        if m is None:
            ok = False if op == "eq" else True
            return AssertResult(spec, "ndjson.%s" % op, ok,
                                "no event of type %s" % shown,
                                "ndjson: none")
        found, val = get_path(m, spec.get("field"))
        want = spec.get("value")
        ok = _val_eq(val, want) if op == "eq" else not _val_eq(val, want)
        return AssertResult(spec,
                            "ndjson.%s" % op, ok,
                            "type=%s %s=%r want %r" % (shown, spec.get("field"),
                                                       val, want),
                            "ndjson line %s: type=%s %s=%r"
                            % (m.get("_line"), shown, spec.get("field"), val),
                            cite={"event_line": m.get("_line")})
    if op == "text_contains":
        text = ndjson_text(events, fmt)
        want = spec.get("value")
        ok = want in text
        return AssertResult(spec, "ndjson.text_contains", ok,
                            "text %s contain %r" % ("contains" if ok
                                                    else "lacks", want),
                            "ndjson text: %r" % text[:200])
    if op in ("tools_absent", "tools_present"):
        lists = ndjson_available_commands(events, fmt)
        all_tools = set()
        for t in lists:
            all_tools.update(t)
        names = spec.get("names", [])
        hits = [n for n in names if n in all_tools]
        ok = (not hits) if op == "tools_absent" else (len(hits) == len(names))
        return AssertResult(spec, "ndjson.%s" % op, ok,
                            "tools %s: %s (of %d advertised)"
                            % ("present-but-not-allowed" if op ==
                               "tools_absent" else "seen",
                               hits or "none", len(all_tools)),
                            "ndjson available_commands: %d lists, %d tools"
                            % (len(lists), len(all_tools)))
    raise ValueError("unknown ndjson op: %r" % op)


def _resolve_glob(glob, base_dir, prefer_last=True):
    if globmod.os.path.isabs(glob):
        paths = globmod.glob(glob)
    else:
        paths = globmod.glob(os.path.join(base_dir, glob))
    if not paths:
        return []
    paths.sort(key=lambda p: (os.path.getmtime(p), p))
    return list(reversed(paths)) if prefer_last else paths


def check_artifact(spec, session_dir):
    if spec.get("op") == "count":
        paths = _resolve_glob(spec.get("file", ""), session_dir)
        n = len(paths)
        ok = n >= spec.get("min", 0) and n <= spec.get("max", 10 ** 9)
        return AssertResult(spec, "artifact.count", ok,
                            "%d files match %s" % (n, spec.get("file")),
                            "artifacts: glob %s -> %d" % (spec.get("file"),
                                                          n))
    if spec.get("op") == "grep":
        paths = _resolve_glob(spec.get("file", ""), session_dir)
        if not paths:
            return AssertResult(spec, "artifact.grep", False,
                                "no files match %s" % spec.get("file"),
                                "artifacts: none")
        n = 0
        for p in paths:
            with open(p, errors="replace") as fh:
                n += fh.read().count(spec.get("grep", ""))
        ok = n >= spec.get("min_count", 0) and             n <= spec.get("max_count", 10 ** 9)
        return AssertResult(spec, "artifact.grep", ok,
                            "%d occurrence(s) of %r in %s"
                            % (n, spec.get("grep")[:60],
                               os.path.basename(paths[0])),
                            "artifacts: %s" % spec.get("file"))
    paths = _resolve_glob(spec.get("file", ""), session_dir)
    which = spec.get("which", "last")
    cands = paths if which == "last" else list(reversed(paths))
    if not cands:
        ok = spec.get("absent_ok", False)
        return AssertResult(spec, "artifact", ok,
                            "no files match %s" % spec.get("file"),
                            "artifacts: glob %s -> none" % spec.get("file"))
    f = cands[0]
    with open(f) as fh:
        d = json.load(fh)
    field = spec.get("field")
    found, val = get_path(d, field)
    rel = os.path.relpath(f, session_dir)
    if not found:
        return AssertResult(spec, "artifact", False,
                            "field %s missing in %s" % (field, rel),
                            "artifacts/%s" % rel)
    if spec.get("ne_null") is True:
        ok = val is not None
        return AssertResult(spec, "artifact", ok,
                            "%s:%s %s null" % (rel, field,
                                               "" if ok else "is"),
                            "artifacts/%s: %s=%r" % (rel, field, val),
                            cite={"file": rel})
    if "eq" in spec:
        ok = _val_eq(val, spec["eq"])
    elif "ne" in spec:
        ok = not _val_eq(val, spec["ne"])
    else:
        ok = True
    return AssertResult(spec, "artifact", ok,
                        "%s:%s=%r" % (rel, field, val),
                        "artifacts/%s: %s=%r" % (rel, field, val),
                        cite={"file": rel})


def _load_capture_doc(p):
    """Request document a `where` filter applies to for capture `p`.
    req-NNN.json loads directly; resp-NNN.jsonl resolves to its PAIRED
    req-NNN.json (same capture number) — the resp-side form of the G10
    scope: a response is in scope iff the request that produced it
    matches the filter (CW-WRITE-USAGE-01: resp-frame value pins are
    the only way to assert on provider-reported usage). Unparseable or
    unpaired captures are dropped (fail-closed: an unscoped file is
    never treated as matching)."""
    name = os.path.basename(p)
    if name.endswith(".jsonl"):
        stem = name[: -len(".jsonl")]
        if stem.startswith("resp-"):
            p = os.path.join(
                os.path.dirname(p),
                "req-" + stem[len("resp-"):] + ".json")
    try:
        with open(p) as fh:
            return json.load(fh)
    except Exception:
        return None


def _wire_filter(paths, where):
    if not where:
        return paths
    out = []
    for p in paths:
        d = _load_capture_doc(p)
        if d is None:
            continue
        ok = True
        for k, v in where.items():
            found, val = get_path(d, k)
            if isinstance(v, dict):
                # Prefix matching on string values (COMP-3: distinguishing
                # xai-compact-* tagged requests from ordinary turn requests).
                # `not_prefix` treats an absent field as not carrying the
                # prefix, so it passes the filter.
                if "prefix" in v:
                    m = (found and isinstance(val, str)
                         and val.startswith(v["prefix"]))
                elif "not_prefix" in v:
                    m = not (found and isinstance(val, str)
                             and val.startswith(v["not_prefix"]))
                else:
                    m = False
                if not m:
                    ok = False
                    break
            elif not found or not _val_eq(val, v):
                ok = False
                break
        if ok:
            out.append(p)
    return out


def _nth_select(paths, spec):
    nth = spec.get("nth", 0)
    if spec.get("last") is not None:
        nth = max(0, len(paths) - 1 - spec["last"])
    return nth


# apex-ayl.22 D-9: the wire label -> upstream path map (the 3-way
# config authority, wire_path_for_model, resolves the same set from
# the live config; this is the runner-side canonical map).
WIRE_PATHS = {"responses": "/v1/responses",
              "messages": "/v1/messages",
              "chat_completions": "/v1/chat/completions"}


def api_backend_for_model(config_text, model_id):
    """apex-ayl.22 D-9 (G2): the 3-way config authority — per model id,
    [model."<id>"] api_backend wins; else [endpoints]
    default_api_backend; else "responses". Minimal TOML line scan (no
    toml dependency — L18 discipline). Read-only: live-config drift is
    recorded, never edited (S-7)."""
    backend = None
    default = "responses"
    current_section = None
    current_model = None
    for raw in (config_text or "").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            current_section = line[1:-1].strip()
            current_model = None
            if current_section.startswith("model."):
                rest = current_section[len("model."):]
                if len(rest) >= 2 and rest[0] in "\"'" \
                        and rest.endswith(rest[0]):
                    rest = rest[1:-1]
                current_model = rest
            continue
        if "=" not in line:
            continue
        key, val = line.split("=", 1)
        key = key.strip()
        val = val.strip().strip("\"'")
        if key == "api_backend" and current_model == model_id:
            backend = val
        elif key == "default_api_backend" \
                and current_section == "endpoints":
            default = val
    return backend if backend is not None else default


def wire_path_for_model(config_text, model_id):
    """apex-ayl.22 D-9 (G2): resolve the upstream wire path for a model
    from the live config via the 3-way authority."""
    backend = api_backend_for_model(config_text, model_id)
    return WIRE_PATHS.get(backend, "/v1/responses")


def row_wire_id(name, routes=None):
    """REDTEAM-REFRESH T4 (r3 §4.7, M-11): the wire model id the row NAMED
    `name` sends — its `model` override, else the name itself.

    `routes` is the case's own row table (`HermeticHome.row_routes()`: the
    config as this case sees it, before the base_url rewrite). An id that
    names no row (an id already spelled as the wire byte, a catalog model,
    an unknown id) comes back verbatim, so a pin that already carries the
    byte selects the same captures (§4.7 invariant 1). No table, as on the
    key-free path (no wiretap), means no resolution."""
    if not isinstance(name, str) or not name:
        return name
    for r in routes or ():
        if r.get("row_name") == name:
            return r.get("wire_model_id") or name
    return name


def synth_output_pins(case, routes=None):
    """apex-ayl.22 D-4b: an output_contains on a declared mcp_call/
    tool_call IS a scored synthesized wire-grep pin. The runner
    synthesizes a wire assert (file = req-*.json, where = the case
    wire path + body.model, any-form) for the expected output
    substring; 0 hits = FAIL. The container-close pin (the cg_rt
    class) stays the case's own pin — the synthesis adds the payload
    check, it does not replace the lifecycle pin. Legacy cases (no
    declarations) get no pins. The runner never rewrites prompts —
    the contract is the declared output_contains.

    REDTEAM-REFRESH T4 (r3 §4.7, r3a A10): `body.model` is the wire id the
    case's row sends (`row_wire_id` over `routes`), not the row name; with
    no table the authored byte stands."""
    out = []
    where = {"method": "POST"}
    wire = case.get("wire")
    if wire in WIRE_PATHS:
        where["path"] = WIRE_PATHS[wire]
    model = case.get("model")
    if isinstance(model, str) and model:
        where["body.model"] = row_wire_id(model, routes)
    for decl in (list(case.get("mcp_calls") or []) +
                 list(case.get("tool_calls") or [])):
        oc = decl.get("output_contains")
        if not oc:
            continue
        out.append({"id": "synth_%s" % decl.get("id", "x"),
                    "kind": "grep",
                    "file": "req-*.json",
                    "where": dict(where),
                    "any": True,
                    "grep": oc,
                    "synth": True})
    return out


# ---------------------------------------------------------------------------
# apex-ayl.70 / apex-ayl.79 (XW-FIXTURES-1 op + XW-JIG-1 ratchet ①) —
# the golden-kind wire engine + the switch_model op's XW-FIXTURES cell
# helpers. ONE compare engine: check_wire kind=golden and the
# switch_model op's wire hook both call golden_compare (R1C: the
# retired cell_diff kind has no second comparator).
# ---------------------------------------------------------------------------

XWFIX_REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(
    os.path.abspath(__file__))))


def _xwfix_cell_dir(cell):
    return (cell if os.path.isabs(cell)
            else os.path.join(XWFIX_REPO_ROOT, cell))


def xwfix_seed_from_cell(home, cwd, sid, model, cell_dir):
    """Seed a fresh session with the cell's pre_switch.json VERBATIM (one
    compact JSON record per line) + a seed_history-style summary.json. The
    records are the byte-pinned foreign pre-switch history — no token-count
    filler. Returns (session_dir, n_lines, total_bytes) or None."""
    pre = os.path.join(cell_dir, "pre_switch.json")
    if not os.path.isfile(pre):
        return None
    with open(pre) as fh:
        records = json.load(fh)
    sdir = home.session_dir_for(cwd, sid)
    os.makedirs(sdir, mode=0o700, exist_ok=True)
    lines = [json.dumps(r) for r in records]
    history_path = os.path.join(sdir, "chat_history.jsonl")
    with open(history_path, "w") as fh:
        fh.write("\n".join(lines) + "\n")
    now = datetime.now(timezone.utc).isoformat()
    summary = {
        "info": {"id": sid, "cwd": cwd},
        "agent_id": "ag1.xwfixcellseed0000000000000000",
        "session_summary": "xwfix cell-seeded history (%d records)"
                           % len(records),
        "created_at": now,
        "updated_at": now,
        "num_messages": len(lines),
        "num_chat_messages": len(lines),
        "current_model_id": model,
        "chat_format_version": 1,
        "grok_home": home.home,
        "agent_name": "xwfix-cell-seed",
    }
    with open(os.path.join(sdir, "summary.json"), "w") as fh:
        json.dump(summary, fh, indent=2)
    return sdir, len(lines), os.path.getsize(history_path)


def _norm_storage_records(records):
    """Normalize runtime-regenerated system records before the storage
    deep-equality. The harness rewrites the system record with the runtime
    system prompt (shape depends on interactive vs nonInteractive), so a
    cell golden's pinned system content can never match post-load; scoring
    it is a permanent index-0/type=system artifact (OVERWATCH-ADVERSARIAL
    r2 wave 2026-09-19: it made the .71 lane's 'GREEN after' storage
    contract, xwfix-driver §4, unreachable). Every other field and record
    stays scored byte-for-byte; the system content diff is informational."""
    norm = []
    for rec in records:
        if isinstance(rec, dict) and rec.get("type") == "system":
            rec = dict(rec)
            rec["content"] = "<runtime-system-prompt>"
        norm.append(rec)
    return norm


def xwfix_cell_diff_storage(cell_dir, session_dir):
    """Storage-form cell diff (evaluated AT SWITCH TIME, before the
    post-switch turn appends records): the persisted chat_history.jsonl
    must equal the cell's expected.json (record array, order-sensitive
    deep equality; system record CONTENT excluded from scoring — it is
    runtime-regenerated, see _norm_storage_records). The switch_model op
    appends this result at switch time so the post-switch turn cannot
    contaminate the pin."""
    spec = {"op": "xwfix_cell_diff", "form": "storage",
            "cell": os.path.basename(cell_dir)}
    exp_p = os.path.join(cell_dir, "expected.json")
    hist_p = os.path.join(session_dir or "", "chat_history.jsonl")
    if not (os.path.isfile(exp_p) and os.path.isfile(hist_p)):
        return AssertResult(
            spec, "xwfix_cell_diff.storage", False,
            "missing expected.json or session chat_history.jsonl",
            "cell_dir=%s session_dir=%r" % (cell_dir, session_dir))
    with open(exp_p) as fh:
        expected = json.load(fh)
    with open(hist_p) as fh:
        actual = [json.loads(l) for l in fh if l.strip()]
    if actual == expected:
        return AssertResult(
            spec, "xwfix_cell_diff.storage", True,
            "%d records match the cell golden (post-switch storage == "
            "expected)" % len(expected),
            "storage form; byte pin %s" % exp_p)
    n_actual = _norm_storage_records(actual)
    n_expected = _norm_storage_records(expected)
    if n_actual == n_expected:
        sysdiff = [i for i in range(min(len(actual), len(expected)))
                   if isinstance(actual[i], dict)
                   and actual[i].get("type") == "system"
                   and actual[i] != expected[i]]
        return AssertResult(
            spec, "xwfix_cell_diff.storage", True,
            "%d records match the cell golden (post-switch storage == "
            "expected; system record content excluded from scoring — "
            "runtime-regenerated, index(es) %s)" % (len(expected), sysdiff),
            "storage form; byte pin %s" % exp_p)
    first = next((i for i in range(min(len(n_actual), len(n_expected)))
                  if n_actual[i] != n_expected[i]),
                 min(len(n_actual), len(n_expected)))
    t = (n_actual[first].get("type") if first < len(n_actual) else "<EOF>")
    return AssertResult(
        spec, "xwfix_cell_diff.storage", False,
        "post-switch stored history != cell golden (system content "
        "excluded from scoring): actual=%d expected=%d "
        "first_diff_index=%d (type=%s)" % (len(actual), len(expected), first, t),
        "storage form; byte pin %s" % exp_p)


def _golden_canon(v):
    """The golden-kind canonical form: key-sorted compact JSON (the
    XW-FIXTURES wire normalization). Object key order is insensitive;
    list order is SIGNIFICANT — the input[] conversation order is
    semantic and projection must not reorder it, so no order-insensitive
    list normalization exists in this engine."""
    return json.dumps(v, sort_keys=True, separators=(",", ":"))


def _golden_null_path(obj, path):
    """Deep-copy `obj` with the value at dotted capture-root `path`
    nulled (dot keys + list indices — a bare numeric token indexes a
    list, the 'body.input.0.id' form; [i] is accepted too). Returns
    (copy, applied): applied is True iff the final token actually
    nulled a value — an absent path is a no-op (the same fail-open
    semantics as the read path) and reports applied=False, so the
    caller can make an unapplied declaration loud (MINOR-1)."""
    out = copy.deepcopy(obj)
    rest = (path or "").strip()
    if rest.startswith("$."):
        rest = rest[2:]
    elif rest.startswith("$"):
        rest = rest[1:]
    toks = re.findall(r'[^\.\[\]]+|\[\d+\]', rest)
    cur = out
    for tok in toks[:-1]:
        if tok.startswith("["):
            idx = int(tok[1:-1])
            if not isinstance(cur, list) or idx >= len(cur):
                return out, False
            cur = cur[idx]
            continue
        if isinstance(cur, list) and tok.isdigit():
            idx = int(tok)
            if idx >= len(cur):
                return out, False
            cur = cur[idx]
        elif isinstance(cur, dict) and tok in cur:
            cur = cur[tok]
        else:
            return out, False
    if not toks:
        return out, False
    last = toks[-1]
    applied = False
    if last.startswith("["):
        idx = int(last[1:-1])
        if isinstance(cur, list) and idx < len(cur):
            cur[idx] = None
            applied = True
    elif isinstance(cur, list) and last.isdigit():
        if int(last) < len(cur):
            cur[int(last)] = None
            applied = True
    elif isinstance(cur, dict) and last in cur:
        cur[last] = None
        applied = True
    return out, applied


def _golden_snip(v):
    s = v if isinstance(v, str) else _golden_canon(v)
    return s[:120]


def _golden_field_diff(actual, want, limit=5):
    """The structured field-level diff: walk both (already normalized)
    request bodies in parallel (dicts in canonical key order, lists by
    index) and collect the first `limit` diverging canonical paths as
    {path (body-root-relative), got, want}."""
    divergences = []

    def _walk(a, w, p):
        if len(divergences) >= limit:
            return
        if isinstance(a, dict) and isinstance(w, dict):
            for k in sorted(set(a) | set(w)):
                pa = "%s.%s" % (p, k) if p else k
                if k not in a:
                    divergences.append({"path": pa, "got": "<absent>",
                                        "want": _golden_snip(w[k])})
                elif k not in w:
                    divergences.append({"path": pa,
                                        "got": _golden_snip(a[k]),
                                        "want": "<absent>"})
                else:
                    _walk(a[k], w[k], pa)
                if len(divergences) >= limit:
                    return
        elif isinstance(a, list) and isinstance(w, list):
            for i in range(min(len(a), len(w))):
                _walk(a[i], w[i], "%s.%d" % (p, i))
                if len(divergences) >= limit:
                    return
            if len(a) != len(w):
                divergences.append({"path": "%s.<len>" % (p or ""),
                                    "got": len(a), "want": len(w)})
        elif _golden_canon(a) != _golden_canon(w):
            divergences.append({"path": p or "<root>",
                                "got": _golden_snip(a),
                                "want": _golden_snip(w)})

    _walk(actual, want, "")
    return divergences


def _golden_norm_note(declared, unapplied):
    """The MINOR-1 loudness note: a declared normalize path not applied
    this run (the volatile field absent from this capture) must be loud,
    not silent: the hazard direction is false-RED only (a pin failing at
    a field that was meant to be nulled), and that must be
    diagnosable from the detail — hard-failing would turn a
    legitimate field-absent run into a mechanism error."""
    if not declared:
        return ""
    n_applied = len(declared) - len(unapplied)
    if unapplied:
        return (" [normalize %d/%d applied; unapplied: %s]"
                % (n_applied, len(declared), ", ".join(unapplied)))
    return " [normalize %d/%d applied]" % (n_applied, len(declared))


def golden_compare_doc(actual_doc, fixture_doc, normalize, capture_name,
                       fixture_name, label="body", declared_normalize=None,
                       pre_unapplied=None, norm_note=None):
    """The shared golden core (apex-2o6): canonical compare of
    `actual_doc` vs `fixture_doc` after the DOC-ROOT-RELATIVE
    `normalize` paths are nulled in `actual_doc` (body-root-relative
    for the response-side kinds resp_body/resp_stream; the request-side
    golden kind pre-nulls the capture doc with capture-root paths and
    passes its declared set + pre-computed note via
    `declared_normalize`/`pre_unapplied` so its detail and diff doc
    are byte-identical to the pre-cut engine). Returns (ok, detail,
    diff_doc); diff_doc is the structured field-level diff on mismatch
    (the TDD RED artifact; the caller writes it to the case report dir
    via _golden_write_diff)."""
    unapplied = list(pre_unapplied or [])
    for p in (normalize or []):
        actual_doc, applied = _golden_null_path(actual_doc, p)
        if not applied:
            unapplied.append(p)
    declared = list(declared_normalize if declared_normalize is not None
                    else (normalize or []))
    if norm_note is None:
        norm_note = _golden_norm_note(declared, unapplied)
    got = _golden_canon(actual_doc)
    if got == _golden_canon(fixture_doc):
        return (True, "canonical %s match (%d B) vs %s%s"
                % (label, len(got), fixture_name, norm_note), None)
    div = _golden_field_diff(actual_doc, fixture_doc)
    diff = {"capture": capture_name,
            "fixture": fixture_name,
            "normalize": declared,
            "normalize_unapplied": list(unapplied),
            "divergences": div,
            "first_divergence": div[0]["path"] if div else "<root>"}
    detail = ("canonical %s mismatch: %d divergence(s), first %s — %s%s"
              % (label, len(div), diff["first_divergence"],
                 "; ".join("%s: got %s want %s"
                           % (d["path"], d["got"], d["want"])
                           for d in div[:3]),
                 norm_note))
    if unapplied:
        detail += (" — if the mismatch is at that position, the "
                   "volatile field was absent this run")
    return (False, detail, diff)


def golden_compare(capture_path, fixture_path, normalize):
    """The golden-kind compare (request side) — the ONE engine for
    WIRE-FORM byte pins (the check_wire kind=golden branch and the
    switch_model op's wire hook both route here). The capture's
    `normalize` paths are CAPTURE-ROOT-RELATIVE (e.g.
    'body.input.0.id'); they are nulled on the capture doc, then the
    body is handed to golden_compare_doc. Canonical JSON of the
    capture's FULL request body vs the fixture's JSON. Returns
    (ok, detail, diff_doc) — behavior-identical to the pre-apex-2o6
    engine (detail note + diff doc shape pinned by
    GoldenCanonicalTest)."""
    with open(capture_path) as fh:
        doc = json.load(fh)
    with open(fixture_path) as fh:
        want = json.load(fh)
    unapplied = []
    for p in (normalize or []):
        doc, applied = _golden_null_path(doc, p)
        if not applied:
            unapplied.append(p)
    body = doc.get("body")
    if not isinstance(body, dict):
        return (False,
                "capture %s has no JSON request body"
                % os.path.basename(capture_path), None)
    return golden_compare_doc(
        body, want, [],
        os.path.basename(capture_path), os.path.basename(fixture_path),
        label="body", declared_normalize=list(normalize or []),
        pre_unapplied=unapplied)


def _golden_write_diff(capture_dir, spec, diff):
    """The TDD RED artifact: the structured field-level diff lands in the
    case report dir (the parent of the wire capture dir) as
    golden-diff-<assert-id>.json."""
    report_dir = os.path.dirname(os.path.abspath(capture_dir or ""))
    name = "golden-diff-%s.json" % (spec.get("id") or "unnamed")
    try:
        with open(os.path.join(report_dir, name), "w") as fh:
            json.dump(diff, fh, indent=2)
            fh.write("\n")
    except Exception:
        pass


def _resp_pair(spec, base):
    """The shared response-pairing machinery (apex-2o6; factored out of
    the resp_status branch, status/pairing logic UNCHANGED): resolve +
    where-filter the request glob, n-sort (capture sequence), which
    first|last, then pair to resp-NNN.jsonl (direct name, fallback
    scan). Fail-closed like the sibling ops. Returns
    (fail_result, req_path, n, resp_path) — fail_result is a ready
    AssertResult (kind 'wire.<kind>') on any fail-closed exit, else
    None with the triple populated."""
    kind = spec.get("kind", "resp_status")
    req_paths = _resolve_glob(spec.get("file", "req-*.json"), base)
    if not req_paths:
        return (AssertResult(spec, "wire.%s" % kind, False,
                             "no wire files match glob %r"
                             % spec.get("file"),
                             "wire: glob %s -> none" % spec.get("file")),
                None, None, None)
    req_paths = _wire_filter(req_paths, spec.get("where"))
    if not req_paths:
        return (AssertResult(spec, "wire.%s" % kind, False,
                             "no request matches where-filter %r "
                             "(probe request never fired)"
                             % spec.get("where"),
                             "wire: %s where -> none" % kind),
                None, None, None)
    def _n_key(p):
        try:
            with open(p) as fh:
                n = json.load(fh).get("n")
        except Exception:
            n = None
        return (0, n) if isinstance(n, int) else (1, p)
    req_paths.sort(key=_n_key)
    if spec.get("which") == "last":
        req_paths = list(reversed(req_paths))
    f = req_paths[0]
    try:
        with open(f) as fh:
            n = json.load(fh).get("n")
    except Exception:
        n = None
    if not isinstance(n, int):
        return (AssertResult(spec, "wire.%s" % kind, False,
                             "request %s carries no capture n"
                             % os.path.basename(f),
                             "wire: %s -> no n" % kind),
                None, None, None)
    resp_path = os.path.join(base, "resp-%03d.jsonl" % n)
    if not os.path.isfile(resp_path):
        resp_path = None
        for rp in sorted(globmod.glob(os.path.join(base, "resp-*.jsonl"))):
            try:
                with open(rp) as fh:
                    if json.loads(fh.readline()).get("n") == n:
                        resp_path = rp
                        break
            except Exception:
                continue
    if resp_path is None:
        return (AssertResult(spec, "wire.%s" % kind, False,
                             "no response captured for %s (n=%s) - "
                             "upstream never answered"
                             % (os.path.basename(f), n),
                             "wire: %s -> no resp capture" % kind),
                None, None, None)
    return (None, f, n, resp_path)


def _sse_frames(resp_path):
    """apex-2o6: parse a resp-NNN.jsonl STREAM capture into
    (status, frames) — the first int 'status' record value, then one
    (event, data) per record whose 'frame' text carries SSE
    'event:'/'data:' heads with at least one data line.
    Comment/keep-alive frames (no data line, e.g. ': keepalive ping')
    are SKIPPED (documented); unparseable data is kept as None (the
    per-frame compare then fails closed)."""
    status = None
    frames = []
    with open(resp_path, errors="replace") as fh:
        for ln in fh:
            try:
                rec = json.loads(ln)
            except Exception:
                continue
            if status is None and isinstance(rec.get("status"), int):
                status = rec["status"]
            fr = rec.get("frame")
            if not isinstance(fr, str):
                continue
            ev = None
            datas = []
            for line in fr.split("\n"):
                if line.startswith("event:"):
                    ev = line[6:].strip()
                elif line.startswith("data:"):
                    datas.append(line[5:].strip())
            if not datas:
                continue
            try:
                data = json.loads("\n".join(datas))
            except Exception:
                data = None
            frames.append((ev, data))
    return status, frames


def _frame_match(actual_fr, fx_fr, normalize):
    """apex-2o6: one resp_stream frame compare — event-name equality +
    the shared golden core on the frame's data object (the normalize
    paths are FRAME-DATA-ROOT-RELATIVE, nulled in the actual frame).
    Returns (ok, detail)."""
    actual_ev, actual_data = actual_fr
    fx_ev = fx_fr.get("event") if isinstance(fx_fr, dict) else None
    if actual_ev != fx_ev:
        return (False, "event %r != fixture %r" % (actual_ev, fx_ev))
    fx_data = fx_fr.get("data") if isinstance(fx_fr, dict) else None
    if not isinstance(actual_data, dict):
        return (False, "frame data is not a JSON object (got %s)"
                % type(actual_data).__name__)
    ok, detail, _diff = golden_compare_doc(
        actual_data, fx_data, normalize or [], "<frame>",
        "fixture frame", label="frame data")
    return (ok, detail)


def _recon_file_val(spec, paths):
    """The file-form recon read (shared by check_recon and the
    check_wire kind=recon branch). grep = a REGEX search over ALL
    matched files (ADV1-R1, apex-2o6, 2026-09-19: the pre-cut literal
    substring over the NEWEST single file made regex alternations a
    structural no-op and earlier captures invisible — the seat's
    BRICK-phrasing pin never fired); an invalid pattern is recorded,
    not raised (recon is non-scoring). field = the JSON read of the
    selected file (nth/any/first, D-12 G15 unchanged)."""
    if spec.get("grep") is not None:
        pat = spec["grep"]
        if not paths:
            return "<no files>"
        try:
            rx = re.compile(pat)
        except re.error as e:
            return "<invalid recon regex %r: %s>" % (pat[:60], e)
        total = 0
        nfiles = 0
        first_hit = None
        for p in sorted(paths):
            try:
                with open(p, errors="replace") as fh:
                    text = fh.read()
            except Exception:
                continue
            nfiles += 1
            hits = [ln for ln in text.splitlines() if rx.search(ln)]
            if hits:
                total += len(hits)
                if first_hit is None:
                    first_hit = "%s: %s" % (os.path.basename(p),
                                            hits[0][:200])
        return ("%d line(s) in %d file(s) match %r: %s"
                % (total, nfiles, pat[:60], first_hit or "-"))
    if spec.get("first"):
        nth = len(paths) - 1
    elif spec.get("any"):
        nth = 0
    else:
        nth = spec.get("nth", 0)
    if paths and nth < len(paths):
        with open(paths[nth]) as fh:
            d = json.load(fh)
        found, val = get_path(d, spec.get("field"))
        val = val if found else "<missing>"
    else:
        val = "<no files>"
    return val


def check_wire(spec, capture_dir):
    kind = spec.get("kind")
    base = capture_dir if capture_dir else os.devnull
    if kind == "reasoning_replay":
        import reasoning_replay
        ok, detail, cite = reasoning_replay.evaluate(
            spec, base, _resolve_glob, _wire_filter)
        return AssertResult(spec, "wire.reasoning_replay", ok, detail,
                            "wire: numeric request/completed-response lineage",
                            cite=cite)
    if kind == "field":
        paths = _resolve_glob(spec.get("file", ""), base)
        paths = _wire_filter(paths, spec.get("where"))
        nth = _nth_select(paths, spec)
        if nth >= len(paths):
            return AssertResult(spec, "wire.field", False,
                                "only %d wire files match (nth=%d)"
                                % (len(paths), nth),
                                "wire: glob %s -> none at nth"
                                % spec.get("file"))
        f = paths[nth]
        with open(f) as fh:
            d = json.load(fh)
        found, val = get_path(d, spec.get("path"))
        want = spec.get("eq", spec.get("ne"))
        if not found:
            return AssertResult(spec, "wire.field", False,
                                "%s missing in %s" % (spec.get("path"),
                                                      os.path.basename(f)),
                                "wire/%s" % os.path.basename(f),
                                cite=_wire_cite(f))
        if "eq" in spec:
            ok = _val_eq(val, spec["eq"])
        elif "ne" in spec:
            ok = not _val_eq(val, spec["ne"])
        else:
            ok = True
        return AssertResult(spec, "wire.field", ok,
                            "%s:%s=%r want %r" % (os.path.basename(f),
                                                  spec.get("path"), val,
                                                  want),
                            "wire/%s: %s=%r" % (os.path.basename(f),
                                                spec.get("path"), val),
                            cite=_wire_cite(f))
    if kind == "grep":
        paths = _resolve_glob(spec.get("file", ""), base)
        paths = _wire_filter(paths, spec.get("where"))
        # Opt-in regex match (CW-WRITE-USAGE-01 lesson): resp-frame captures
        # carry the SSE payload JSON-ESCAPED inside the 'frame' string, so a
        # literal needle must hand-escape every quote (the T21 precedent) and
        # can never express a value class like "non-zero". regex:true
        # compiles the needle once (fail-closed on a bad pattern); the
        # default stays literal str.find — every pre-existing case is
        # untouched.
        _gre = None
        if spec.get("regex"):
            try:
                _gre = re.compile(spec.get("grep", ""))
            except re.error as exc:
                return AssertResult(spec, "wire.grep", False,
                                    "regex:true pattern does not compile: %s"
                                    % exc,
                                    "wire: bad regex rejected")

        def _gspan(text):
            """Match span (start, end) for the needle in `text`, or None."""
            if _gre is not None:
                m = _gre.search(text)
                return (m.start(), m.end()) if m else None
            if not needle:
                return None
            pos = text.find(needle)
            return (pos, pos + len(needle)) if pos >= 0 else None

        if spec.get("all"):
            # MA-3: absent across ALL filtered files (not just the selected
            # nth). Fail-closed: `all` without `absent` is rejected, and an
            # empty match set is a harness failure, not a vacuous pass.
            if not spec.get("absent"):
                return AssertResult(spec, "wire.grep", False,
                                    "all:true requires absent:true",
                                    "wire: all without absent rejected")
            if not paths:
                return AssertResult(spec, "wire.grep", False,
                                    "no wire files match glob %r"
                                    % spec.get("file"),
                                    "wire: glob %s -> none"
                                    % spec.get("file"))
            needle = spec.get("grep", "")
            hit = None
            for f in paths:
                with open(f) as fh:
                    text = fh.read()
                span = _gspan(text)
                if span:
                    hit = (f, span)
                    break
            if hit is not None:
                f, span = hit
                return AssertResult(spec, "wire.grep", False,
                                    "%s contains %r (absent in all %d)"
                                    % (os.path.basename(f), needle,
                                       len(paths)),
                                    "wire: %s grep %r"
                                    % (os.path.basename(f),
                                       needle[:80]),
                                    cite=_wire_cite(f, list(span)))
            return AssertResult(spec, "wire.grep", True,
                                "all %d files lack %r"
                                % (len(paths), needle),
                                "wire: all files grep %r" % needle[:80],
                                cite={"file": None,
                                      "files": [os.path.basename(p)
                                                for p in paths]})
        if spec.get("any"):
            # MA-3: present in at least ONE filtered file (complement of the
            # `all`+absent form). Fail-closed: `any` with `absent` is
            # rejected; an empty match set is a harness failure, not a pass.
            if spec.get("absent"):
                return AssertResult(spec, "wire.grep", False,
                                    "any:true requires presence (absent not allowed)",
                                    "wire: any with absent rejected")
            if not paths:
                return AssertResult(spec, "wire.grep", False,
                                    "no wire files match glob %r"
                                    % spec.get("file"),
                                    "wire: glob %s -> none"
                                    % spec.get("file"))
            needle = spec.get("grep", "")
            for f in paths:
                with open(f) as fh:
                    text = fh.read()
                span = _gspan(text)
                if span:
                    return AssertResult(spec, "wire.grep", True,
                                        "%s contains %r (any of %d)"
                                        % (os.path.basename(f), needle,
                                           len(paths)),
                                        "wire: %s grep %r"
                                        % (os.path.basename(f),
                                           needle[:80]),
                                        cite=_wire_cite(f, list(span)))
            return AssertResult(spec, "wire.grep", False,
                                "no file of %d contains %r"
                                % (len(paths), needle),
                                "wire: any-of files grep %r" % needle[:80])
        nth = _nth_select(paths, spec)
        if nth >= len(paths):
            return AssertResult(spec, "wire.grep", False,
                                "only %d wire files match (nth=%d)"
                                % (len(paths), nth),
                                "wire: glob %s -> none" % spec.get("file"))
        f = paths[nth]
        with open(f) as fh:
            text = fh.read()
        needle = spec.get("grep", "")
        span = _gspan(text)
        present = span is not None
        ok = (not present) if spec.get("absent") else present
        cite = None
        if present:
            cite = _wire_cite(f, list(span))
        elif ok:
            cite = _wire_cite(f)
        return AssertResult(spec, "wire.grep", ok,
                            "%s %s %r" % (os.path.basename(f),
                                          "lacks" if not present
                                          else "contains", needle),
                            "wire/%s grep %r" % (os.path.basename(f),
                                                 needle[:80]),
                            cite=cite)
    if kind == "size_lt":
        pa = _resolve_glob(spec["a"]["file"], base)
        pa = _wire_filter(pa, spec["a"].get("where"))
        nth_a = _nth_select(pa, spec["a"])
        pb = _resolve_glob(spec["b"]["file"], base)
        pb = _wire_filter(pb, spec["b"].get("where"))
        nth_b = _nth_select(pb, spec["b"])
        if nth_a >= len(pa) or nth_b >= len(pb):
            return AssertResult(spec, "wire.size_lt", False,
                                "a: %d files, b: %d files" % (len(pa),
                                                              len(pb)),
                                "wire: size_lt unresolved")
        sa = os.path.getsize(pa[nth_a])
        sb = os.path.getsize(pb[nth_b])
        ok = sa < sb
        return AssertResult(spec, "wire.size_lt", ok,
                            "%s (%d B) %s %s (%d B)" % (
                                os.path.basename(pa[nth_a]), sa,
                                "<" if ok else ">=",
                                os.path.basename(pb[nth_b]), sb),
                            "wire: %d B vs %d B" % (sa, sb),
                            cite={"file": os.path.basename(pa[nth_a]),
                                  "file_b":
                                  os.path.basename(pb[nth_b])})
    if kind == "count":
        all_paths = _resolve_glob(spec.get("file", ""), base)
        if not all_paths:
            # Fail-closed like the grep ops: a glob that matches nothing is a
            # harness failure, not a vacuous pass (MA-3 discipline).
            return AssertResult(spec, "wire.count", False,
                                "no wire files match glob %r" % spec.get("file"),
                                "wire: glob %s -> none" % spec.get("file"))
        paths = _wire_filter(all_paths, spec.get("where"))
        n = len(paths)
        lo = spec.get("min", 0)
        hi = spec.get("max")
        ok = n >= lo and (hi is None or n <= hi)
        return AssertResult(spec, "wire.count", ok,
                            "%d wire files match %s (want %d..%s)" % (
                                n, spec.get("file"), lo,
                                str(hi) if hi is not None else "inf"),
                            "wire: %d match %s" % (n, spec.get("file")),
                            cite={"file": None,
                                  "files": [os.path.basename(p)
                                            for p in paths],
                                  "count": n})
    if kind == "resp_status":
        # XREPLAY-1: response-side status extraction. Pairs the selected
        # request (matched with the standard where-filter on REQUEST
        # fields) with its resp-NNN.jsonl by capture sequence number,
        # and records the upstream HTTP status plus, for error frames,
        # the error JSON — machine-readable verdict evidence for probes
        # whose finding is the acceptance outcome (200 vs 400) rather
        # than a pinned expectation. Fail-closed like the sibling ops:
        # an empty glob, an empty where-filter result, a missing resp
        # capture, and an unparseable status line are all harness
        # failures. `status_in` (optional whitelist) makes the check
        # assertive; omit it for evidence-only (ok iff a status line
        # exists).
        fail, f, n, resp_path = _resp_pair(spec, base)
        if fail is not None:
            return fail
        status = None
        error_json = None
        with open(resp_path, errors="replace") as fh:
            for ln in fh:
                try:
                    rec = json.loads(ln)
                except Exception:
                    continue
                if status is None and "status" in rec:
                    status = rec.get("status")
                frame = rec.get("frame")
                if error_json is None and isinstance(frame, str):
                    try:
                        inner = json.loads(frame)
                    except Exception:
                        continue
                    if isinstance(inner, dict) and "error" in inner:
                        error_json = json.dumps(inner["error"],
                                                 ensure_ascii=False)
        if not isinstance(status, int):
            return AssertResult(spec, "wire.resp_status", False,
                                "no parseable status line in %s"
                                % os.path.basename(resp_path),
                                "wire: resp_status -> unparseable")
        want = spec.get("status_in")
        ok = (status in want) if isinstance(want, list) else True
        model = None
        try:
            with open(f) as fh:
                model = (json.load(fh).get("body") or {}).get("model")
        except Exception:
            pass
        detail = "req-%03d (model=%s) -> HTTP %d" % (n, model, status)
        if error_json:
            detail += "; error=%s" % error_json[:300]
        return AssertResult(spec, "wire.resp_status", ok, detail,
                            "wire/%s (status=%s, want=%s)"
                            % (os.path.basename(resp_path), status,
                               want if want is not None else "any"),
                            cite={"file": os.path.basename(resp_path),
                                  "request_n": n, "model": model,
                                  "status": status} if ok else None)
    if kind == "golden":
        # apex-ayl.79 (R1B): the declarative WIRE-FORM byte pin — the
        # selected capture's FULL request body, canonically (sort_keys,
        # compact) after the normalize paths are nulled, vs the fixture.
        # Selection reuses the count-tolerance machinery exactly (the
        # where-filter + nth/last/any; nth=0 = newest, fail-closed on an
        # empty set). One engine: golden_compare.
        if not isinstance(spec.get("golden"), str) \
                or not spec.get("golden"):
            return AssertResult(spec, "wire.golden", False,
                                "golden assert needs a string 'golden' "
                                "fixture path",
                                "wire: golden without fixture rejected")
        paths = _resolve_glob(spec.get("file", ""), base)
        paths = _wire_filter(paths, spec.get("where"))
        fixture = os.path.join(base, spec["golden"])
        if not paths:
            return AssertResult(spec, "wire.golden", False,
                                "no wire files match glob %r"
                                % spec.get("file"),
                                "wire: glob %s -> none" % spec.get("file"))
        if not os.path.isfile(fixture):
            return AssertResult(spec, "wire.golden", False,
                                "golden fixture %r not found (relative "
                                "to the wire dir)" % spec["golden"],
                                "wire: golden fixture missing")
        if spec.get("any"):
            for f in paths:
                ok, detail, _diff = golden_compare(f, fixture,
                                                   spec.get("normalize"))
                if ok:
                    return AssertResult(spec, "wire.golden", True,
                                        "%s: %s (any of %d)" % (
                                            os.path.basename(f), detail,
                                            len(paths)),
                                        "wire/%s golden %s" % (
                                            os.path.basename(f),
                                            spec["golden"]),
                                        cite=_wire_cite(f))
            return AssertResult(spec, "wire.golden", False,
                                "no file of %d matches the golden pin"
                                % len(paths),
                                "wire: any-of golden %s" % spec["golden"])
        nth = _nth_select(paths, spec)
        if nth >= len(paths):
            return AssertResult(spec, "wire.golden", False,
                                "only %d wire files match (nth=%d)"
                                % (len(paths), nth),
                                "wire: glob %s -> none" % spec.get("file"))
        f = paths[nth]
        ok, detail, diff = golden_compare(f, fixture, spec.get("normalize"))
        if not ok and diff is not None:
            _golden_write_diff(capture_dir, spec, diff)
        return AssertResult(spec, "wire.golden", ok,
                            "%s: %s" % (os.path.basename(f), detail),
                            "wire/%s golden %s" % (os.path.basename(f),
                                                   spec["golden"]),
                            cite=_wire_cite(f) if ok else None)
    if kind == "resp_body":
        # apex-2o6: the RESPONSE-SIDE byte pin — the selected request's
        # paired NON-STREAM response body (the resp-NNN.jsonl frame
        # lines concatenated + JSON-parsed) vs the fixture, through the
        # shared golden core (golden_compare_doc; the normalize paths
        # are BODY-ROOT-RELATIVE, unlike the request-side golden
        # kind's capture-root paths). Fixture duality on the top-level
        # 'status': an INT = the HTTP assert (the paired capture's
        # status line must equal it; 'status' is popped from BOTH the
        # fixture and the actual body before the canonical compare —
        # the body's own status field is subsumed by the HTTP pin);
        # a STRING (e.g. "completed") = the body's own field, left in
        # the compare (the two forms are mutually exclusive — one
        # object, one key).
        if not isinstance(spec.get("golden"), str) \
                or not spec.get("golden"):
            return AssertResult(spec, "wire.resp_body", False,
                                "resp_body assert needs a string "
                                "'golden' fixture path",
                                "wire: resp_body without fixture "
                                "rejected")
        fixture = os.path.join(base, spec["golden"])
        if not os.path.isfile(fixture):
            return AssertResult(spec, "wire.resp_body", False,
                                "resp_body fixture %r not found "
                                "(relative to the wire dir)"
                                % spec["golden"],
                                "wire: resp_body fixture missing")
        with open(fixture) as fh:
            fx = json.load(fh)
        if not isinstance(fx, dict):
            return AssertResult(spec, "wire.resp_body", False,
                                "resp_body fixture must be a JSON "
                                "object", "wire: resp_body fixture "
                                "malformed")
        fail, f, n, resp_path = _resp_pair(spec, base)
        if fail is not None:
            return fail
        want_status = None
        if isinstance(fx.get("status"), int):
            want_status = fx.pop("status")
        actual_status = None
        frame_texts = []
        with open(resp_path, errors="replace") as fh:
            for ln in fh:
                try:
                    rec = json.loads(ln)
                except Exception:
                    continue
                if actual_status is None \
                        and isinstance(rec.get("status"), int):
                    actual_status = rec["status"]
                fr = rec.get("frame")
                if isinstance(fr, str):
                    frame_texts.append(fr)
        if actual_status is None:
            return AssertResult(spec, "wire.resp_body", False,
                                "no parseable status line in %s"
                                % os.path.basename(resp_path),
                                "wire: resp_body -> unparseable")
        if want_status is not None and actual_status != want_status:
            return AssertResult(
                spec, "wire.resp_body", False,
                "req-%03d -> HTTP %d != fixture status %d"
                % (n, actual_status, want_status),
                "wire/%s (status=%d, want=%d)"
                % (os.path.basename(resp_path), actual_status,
                   want_status),
                cite={"file": os.path.basename(resp_path),
                      "request_n": n, "status": actual_status})
        if not frame_texts:
            return AssertResult(
                spec, "wire.resp_body", False,
                "no frames in %s (non-stream body missing)"
                % os.path.basename(resp_path),
                "wire: resp_body -> no frames")
        try:
            actual_body = json.loads("\n".join(frame_texts))
        except Exception:
            return AssertResult(
                spec, "wire.resp_body", False,
                "response body is not a single JSON document (%d "
                "frame line(s) in %s) — fail-closed"
                % (len(frame_texts), os.path.basename(resp_path)),
                "wire: resp_body -> unparseable body")
        if not isinstance(actual_body, dict):
            return AssertResult(
                spec, "wire.resp_body", False,
                "response body is %s, not a JSON object — fail-closed"
                % type(actual_body).__name__,
                "wire: resp_body -> body not an object")
        if want_status is not None and "status" in actual_body:
            # The int-HTTP form subsumes the body's own status field:
            # pop it from the actual side too (the fixture already
            # lost it) so the canonical compare is symmetric.
            actual_body = dict(actual_body)
            actual_body.pop("status", None)
        ok, detail, diff = golden_compare_doc(
            actual_body, fx, spec.get("normalize"),
            os.path.basename(resp_path),
            os.path.basename(spec["golden"]),
            label="response body")
        if not ok and diff is not None:
            _golden_write_diff(capture_dir, spec, diff)
        return AssertResult(spec, "wire.resp_body", ok,
                            "req-%03d (HTTP %d): %s"
                            % (n, actual_status, detail),
                            "wire/%s body %s"
                            % (os.path.basename(resp_path),
                               spec["golden"]),
                            cite={"file": os.path.basename(resp_path),
                                  "request_n": n,
                                  "status": actual_status}
                            if ok else None)
    if kind == "resp_stream":
        # apex-2o6: the STREAM-FRAME surface pin — the paired
        # resp-NNN.jsonl SSE frames parsed (event:/data: heads;
        # comment/keep-alive frames carry no data and are SKIPPED),
        # the 'drop' list removes volatile event names, the
        # min_frames/max_frames band bounds the POST-DROP count (the
        # COMP-3 storm-band pattern, response-side), and the fixture
        # frame sequence must appear IN ORDER (mode 'subsequence',
        # the default — interleaved chunks tolerated) or 1:1 (mode
        # 'exact'). Per-frame compare = event equality + the shared
        # golden core on the frame's data object (normalize paths
        # nulled per frame, body-root-relative).
        if not isinstance(spec.get("golden"), str) \
                or not spec.get("golden"):
            return AssertResult(spec, "wire.resp_stream", False,
                                "resp_stream assert needs a string "
                                "'golden' fixture path",
                                "wire: resp_stream without fixture "
                                "rejected")
        fixture = os.path.join(base, spec["golden"])
        if not os.path.isfile(fixture):
            return AssertResult(spec, "wire.resp_stream", False,
                                "resp_stream fixture %r not found "
                                "(relative to the wire dir)"
                                % spec["golden"],
                                "wire: resp_stream fixture missing")
        with open(fixture) as fh:
            fx = json.load(fh)
        if not isinstance(fx, dict) or not isinstance(fx.get("frames"),
                                                      list):
            return AssertResult(
                spec, "wire.resp_stream", False,
                "resp_stream fixture must be an object with a "
                "'frames' list", "wire: resp_stream fixture "
                "malformed")
        mode = spec.get("mode") or fx.get("mode") or "subsequence"
        if mode not in ("subsequence", "exact"):
            return AssertResult(
                spec, "wire.resp_stream", False,
                "resp_stream 'mode' must be subsequence|exact (got "
                "%r)" % mode, "wire: resp_stream bad mode")
        want_status = fx.get("status")
        fail, f, n, resp_path = _resp_pair(spec, base)
        if fail is not None:
            return fail
        actual_status, actual_frames = _sse_frames(resp_path)
        if actual_status is None:
            return AssertResult(spec, "wire.resp_stream", False,
                                "no parseable status line in %s"
                                % os.path.basename(resp_path),
                                "wire: resp_stream -> unparseable")
        if isinstance(want_status, int) \
                and actual_status != want_status:
            return AssertResult(
                spec, "wire.resp_stream", False,
                "req-%03d -> HTTP %d != fixture status %d"
                % (n, actual_status, want_status),
                "wire/%s (status=%d, want=%d)"
                % (os.path.basename(resp_path), actual_status,
                   want_status),
                cite={"file": os.path.basename(resp_path),
                      "request_n": n, "status": actual_status})
        drop = set(spec.get("drop") or [])
        kept = [fr for fr in actual_frames if fr[0] not in drop]
        lo = spec.get("min_frames", 0)
        hi = spec.get("max_frames")
        if not (len(kept) >= lo and (hi is None or len(kept) <= hi)):
            return AssertResult(
                spec, "wire.resp_stream", False,
                "%d post-drop frames outside the band %d..%s "
                "(min_frames=%s max_frames=%s)"
                % (len(kept), lo,
                   str(hi) if hi is not None else "inf", lo,
                   str(hi) if hi is not None else "none"),
                "wire: resp_stream count band")
        normalize = spec.get("normalize")
        cite = {"file": os.path.basename(resp_path), "request_n": n}
        if mode == "exact":
            if len(kept) != len(fx["frames"]):
                return AssertResult(
                    spec, "wire.resp_stream", False,
                    "exact mode: %d post-drop frames != %d fixture "
                    "frames (1:1 required)"
                    % (len(kept), len(fx["frames"])),
                    "wire: resp_stream exact length")
            for i, (actual_fr, fx_fr) in enumerate(
                    zip(kept, fx["frames"])):
                okf, detf = _frame_match(actual_fr, fx_fr, normalize)
                if not okf:
                    return AssertResult(
                        spec, "wire.resp_stream", False,
                        "exact frame %d: %s" % (i, detf),
                        "wire: resp_stream exact frame %d" % i)
            return AssertResult(
                spec, "wire.resp_stream", True,
                "req-%03d: exact 1:1 match of %d frame(s) (HTTP %d)"
                % (n, len(kept), actual_status),
                "wire/%s stream %s (exact)"
                % (os.path.basename(resp_path), spec["golden"]),
                cite=cite)
        # subsequence (default): fixture frames in order, interleaved
        # actual chunks tolerated.
        pos = 0
        for fi, fx_fr in enumerate(fx["frames"]):
            matched = False
            while pos < len(kept):
                okf, _detf = _frame_match(kept[pos], fx_fr, normalize)
                pos += 1
                if okf:
                    matched = True
                    break
            if not matched:
                fx_ev = fx_fr.get("event") if isinstance(fx_fr, dict) \
                    else None
                return AssertResult(
                    spec, "wire.resp_stream", False,
                    "fixture frame %d (%s) not found in order in the "
                    "%d post-drop actual frame(s)"
                    % (fi, fx_ev, len(kept)),
                    "wire: resp_stream subsequence frame %d" % fi)
        return AssertResult(
            spec, "wire.resp_stream", True,
            "req-%03d: all %d fixture frame(s) matched in order "
            "(%d post-drop actual, HTTP %d)"
            % (n, len(fx["frames"]), len(kept), actual_status),
            "wire/%s stream %s (subsequence)"
            % (os.path.basename(resp_path), spec["golden"]),
            cite=cite)
    if kind == "recon":
        # ADV1-R1 (apex-2o6): the wire-level recon pin — non-scoring
        # (always ok; the evidence text is the match summary), regex
        # over ALL matched files (the pre-cut engine had no
        # check_wire recon branch at all — kind=recon raised).
        label = spec.get("label", "recon")
        if not spec.get("file"):
            return AssertResult(spec, "wire.recon", True,
                                "%s: <n/a>" % label,
                                "recon: " + label, recon=True)
        paths = _resolve_glob(spec["file"], base)
        val = _recon_file_val(spec, paths)
        return AssertResult(spec, "wire.recon", True,
                            "%s: %s" % (label, str(val)[:400]),
                            "recon: " + label, recon=True)
    raise ValueError("unknown wire kind: %r" % kind)


def check_recon(spec, ctx):
    label = spec.get("label", "recon")
    if spec.get("op") == "recon" and spec.get("what") == "final-text":
        val = ndjson_text(ctx.events, ctx.fmt)
        val_s = val[:400]
        return AssertResult(spec, "recon", True,
                            "%s: %s" % (label, val_s),
                            "recon: " + label, recon=True)
    if "file" in spec:
        base = ctx.capture_dir if spec.get("base") == "wire" \
            else (ctx.session_dir or "")
        paths = _resolve_glob(spec["file"], base or "")
        # ADV1-R1 (apex-2o6, 2026-09-19): the grep form is a REGEX
        # over ALL matched files (the pre-cut literal substring over
        # the NEWEST single file made regex alternations a structural
        # no-op and earlier captures invisible); the field form keeps
        # the D-12 (G15) selection (any = newest, first = oldest,
        # nth). The always-ok recon contract is unchanged either way.
        val = _recon_file_val(spec, paths)
    elif "event" in spec:
        matches = [e for e in ctx.events if e.get("type") == spec["event"]]
        if matches:
            m = matches[-1]
            found, val = get_path(m, spec.get("field"))
            val = val if found else m
        else:
            val = "<event not seen>"
    else:
        val = spec.get("value", "<n/a>")
    val_s = json.dumps(val, ensure_ascii=False)[:400] \
        if not isinstance(val, str) else val[:400]
    return AssertResult(spec, "recon", True,
                        "%s: %s" % (label, val_s),
                        "recon: " + label, recon=True)


# apex-ayl.22 D-3: the extended status order (report.md/report.json
# sort + status_label) — VACUOUS and FINDING-PASS join the set.
STATUS_ORDER = ["PASS", "FINDING-PASS", "RECON", "VACUOUS", "FAIL",
                "BLOCKED", "SKIP"]
# D-3 caps block (the runner REPORTS the caps; the coordinator
# ENFORCES them at the gate — Q5).
CAPS = {"full_blocked_max_no_ruling": 3,
        "slim_vacuous_ruling_threshold": 3,
        "env_retry_max_per_cell": 1}
# apex-ayl.22 D-8: the scored wire kinds that make a per-file claim
# (audited under scoring.require_wire_evidence). `count` is exempt:
# its machine index is the matched file SET (cite["files"]), not one
# file; recon asserts are never scored. apex-2o6: the response-side
# kinds (resp_body/resp_stream) cite their paired resp-NNN.jsonl.
AUDITED_WIRE_KINDS = ("field", "grep", "resp_status", "size_lt",
                      "resp_body", "resp_stream", "reasoning_replay")


def _req_n_from_name(path):
    m = re.search(r"-(\d+)\.jsonl?$", os.path.basename(path))
    return int(m.group(1)) if m else None


def _wire_cite(f, needle_pos=None, extra=None):
    """D-8 machine index for a wire assert that read file `f`:
    {file, request_n, model, byte_range} (+ extras). The evidence
    string stays for humans; this is the machine-readable citation."""
    cite = {"file": os.path.basename(f),
            "request_n": _req_n_from_name(f)}
    try:
        with open(f) as fh:
            d = json.load(fh)
        cite["model"] = (d.get("body") or {}).get("model")
        cite["byte_range"] = [0, os.path.getsize(f)]
    except Exception:
        pass
    if needle_pos is not None:
        cite["byte_range"] = list(needle_pos)
    if extra:
        cite.update(extra)
    return cite


def _cite_resolvable(cite, wire_dir):
    """D-8: a citation is resolvable when it names a file that exists
    (under the wire dir for relative names). A PASS without one is a
    NO-EVIDENCE downgrade, never a silent pass."""
    if not isinstance(cite, dict):
        return False
    f = cite.get("file")
    if not f:
        return bool(cite.get("files"))
    if not os.path.isabs(f) and wire_dir:
        f = os.path.join(wire_dir, f)
    return os.path.exists(f)


def audit_wire_evidence(case, results, wire_dir=None):
    """D-8 audit (scoring.require_wire_evidence, default true): every
    SCORED (non-recon) wire PASS must carry a resolvable citation.
    Returns violation strings (empty = clean)."""
    scoring = case.get("scoring") or {}
    if not scoring.get("require_wire_evidence", True):
        return []
    out = []
    for r in results:
        if r.recon:
            continue
        spec = r.spec or {}
        if spec.get("kind") not in AUDITED_WIRE_KINDS:
            continue
        resolved = _cite_resolvable(r.cite, wire_dir)
        if spec.get("kind") == "reasoning_replay":
            files = (r.cite or {}).get("files") or []
            resolved = resolved and bool(files) and all(
                _cite_resolvable({"file": path}, wire_dir) for path in files)
        if r.ok and not resolved:
            out.append("NO-EVIDENCE: scored wire PASS %r (kind %s) "
                       "has no resolvable citation"
                       % (spec.get("id"), spec.get("kind")))
    return out


def _last_response_status(capture_dir, model):
    """D-5/D-15: the last captured HTTP status for `model`'s requests
    (max capture n with body.model == model, paired resp-NNN.jsonl).
    Returns (status, req_name) or None (no wire evidence)."""
    if not capture_dir or not os.path.isdir(capture_dir):
        return None
    best = None
    for f in sorted(globmod.glob(os.path.join(capture_dir,
                                              "req-*.json"))):
        try:
            with open(f) as fh:
                d = json.load(fh)
        except Exception:
            continue
        if d.get("method") != "POST":
            continue
        if model is not None and \
                (d.get("body") or {}).get("model") != model:
            continue
        n = d.get("n")
        if n is None:
            continue
        if best is None or n > best[0]:
            best = (n, os.path.basename(f))
    if best is None:
        return None
    resp_path = os.path.join(capture_dir, "resp-%03d.jsonl" % best[0])
    if not os.path.isfile(resp_path):
        return None
    try:
        with open(resp_path, errors="replace") as fh:
            rec = json.loads(fh.readline())
    except Exception:
        return None
    if isinstance(rec.get("status"), int):
        return (rec["status"], best[1])
    return None


class RecheckUnavailable(Exception):
    """r3a A13: the route gave no readable model list. That is a different
    fact from "the model left the catalog" — one is a route that cannot
    answer the question, the other is an answer. r3b B7 exists because the
    frozen code folded both into a silent no-retry that recorded nothing."""

    def __init__(self, host, status):
        self.host = host
        self.status = status
        Exception.__init__(self, "recheck unsupported on %s (%s)"
                           % (host, status))


def _url_host(url):
    rest = (url or "").split("://", 1)[-1]
    return rest.split("/", 1)[0] or "?"


def _route_for_model(route_ctx, model):
    """(wire_id, route) for the `model` value a case declares.

    r3b B7 is exactly this lookup: a case declares a row NAME, while
    `_last_response_status` matches captures on `body.model`, the WIRE ID.
    Looking the row name up finds no capture, so the recheck was never
    reached and every env-retry on a named row was silently lost."""
    route_map = (route_ctx or {}).get("route_map") or {}
    route = route_map.get(model)
    if isinstance(route, dict):
        return model, route
    for wire_id, candidate in route_map.items():
        if (isinstance(candidate, dict)
                and model in (candidate.get("row_names") or [])):
            return wire_id, candidate
    return model, None


def recheck_models_available(origin, model, credential):
    """apex-ayl.22 D-5, route-aware per r3a A13: a FRESH `GET /v1/models` on
    the ROUTE's own origin, authenticating with that ROUTE's credential.

    The ambient key no longer rides along (r3a A13 leg 3): a route that
    declares `env_key = SOME_OTHER_KEY` must not be probed with the proxy
    key, which is r3 §2.4's credential/origin mismatch re-created on a GET.
    When the route's credential variable IS the ambient key the value is the
    same and the proxy row is unaffected. HYG-1 still holds: the value goes
    in the in-memory header, never argv or disk.

    Returns True iff `model` (the WIRE id) is in the catalog. A route that
    gives no readable answer raises RecheckUnavailable so the caller records
    why the retry was lost; a catalog that answers and no longer lists the id
    returns False (a catalog change is a FAIL, not an env flap; 404 != 400).
    """
    if not model:
        return False
    import urllib.error
    host = _url_host(origin)
    url = (origin or "").rstrip("/") + "/v1/models"
    req = urllib.request.Request(url)
    if credential:
        value = credential
        if not re.match(r"^(?:basic|bearer)\s", value, flags=re.I):
            value = "Bearer " + value
        req.add_header("Authorization", value)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            status = getattr(resp, "status", 200)
            if status != 200:
                raise RecheckUnavailable(host, status)
            data = json.loads(resp.read().decode("utf-8", "replace"))
    except RecheckUnavailable:
        raise
    except urllib.error.HTTPError as exc:
        raise RecheckUnavailable(host, exc.code)
    except Exception as exc:
        raise RecheckUnavailable(host, type(exc).__name__)
    items = data.get("data") if isinstance(data, dict) else data
    if not isinstance(items, list):
        raise RecheckUnavailable(host, "no model list")
    for it in items:
        if isinstance(it, dict) and it.get("id") == model:
            return True
        if it == model:
            return True
    return False


def _retry_decision(case, capture_dir, model, attempt, max_attempts,
                    upstream, ambient_key="", route_ctx=None):
    """apex-ayl.22 D-5: the env retry decision table. `attempt` = the
    attempt that just completed (1-based); `max_attempts` = the case's
    retry.max_attempts = the max env RETRIES per cell (the cap; the
    Q5 env_retry_max_per_cell is REPORTED in the caps block and
    ENFORCED by the coordinator at the gate). Returns
    (retry, reason, recheck):
      - last response status in retry.on_status:
          - recheck_models: model still in the catalog -> retry (an
            env flap; the recheck is recorded as a recon row); model
            gone -> NO retry (a catalog change is a FAIL, not an env
            flap — 404 != 400);
          - no recheck declared: retry;
      - any other last status (or no wire evidence): never retry;
      - attempt > max_attempts: never (the cap)."""
    retry_block = case.get("retry") or {}
    on_status = retry_block.get("on_status") or []
    if not on_status:
        return (False, "no retry.on_status declared", None)
    if attempt > max_attempts:
        return (False, "retry cap: attempt %d exceeds max_attempts %d"
                % (attempt, max_attempts), None)
    # r3b B7: resolve the row NAME to its WIRE ID before the capture lookup,
    # or the lookup can never find the request this case made.
    wire_id, route = _route_for_model(route_ctx, model)
    last = _last_response_status(capture_dir, wire_id)
    if last is None or last[0] not in on_status:
        return (False, "last response status %r not in on_status %r"
                % (last[0] if last else None, on_status), None)
    if retry_block.get("recheck_models"):
        origin = upstream
        credential = ambient_key
        if route is not None:
            origin = route.get("origin") or upstream
            values = (route_ctx or {}).get("credential_values") or {}
            var = route.get("credential_variable")
            if var:
                credential = values.get(var, "")
            else:
                for rung in XAI_CREDENTIAL_CHAIN:
                    if values.get(rung):
                        credential = values[rung]
                        break
        try:
            present = recheck_models_available(origin, wire_id, credential)
        except RecheckUnavailable as exc:
            # r3a A13: still fail-closed (no retry), but the lost retry is
            # now NAMED instead of silent (r3b B7).
            return (False, str(exc), False)
        if not present:
            return (False,
                    "model %r gone on recheck (catalog change, not an "
                    "env flap)" % wire_id, False)
        return (True, "env flap: last status %d in on_status %r; model "
                "%r still in catalog" % (last[0], on_status, wire_id),
                True)
    return (True, "env flap: last status %d in on_status %r"
            % (last[0], on_status), None)


def finalize_verdict(case, pre_status, results, wire_dir=None,
                     interject_cause=None):
    """apex-ayl.22 D-3 — the verdict engine (semantics: C §(c)
    BINDING + SDD D-3). A premise pass over scoring.vacuous_if
    PRECEDES pin scoring:
      1. any harness-class premise 0-hit -> BLOCKED (the rig failed;
         rule 1 dominates, including over a scored FAIL);
      2. else any model-class premise 0-hit -> VACUOUS (the model
         never took the probe path; rule 2 BEATS a scored FAIL — the
         probe was meaningless, the failure uninterpretable);
      3. else the scored pins decide: any FAIL -> FAIL (tolerant cases
         -> FINDING-PASS with the failed pins listed as findings);
         all PASS -> PASS.
    recon asserts never affect the verdict. A 0-hit = the pin result
    is missing (not evaluated — fail-closed) or not ok.
    apex-bdfz BZ-7 (plan r3 section 2.5.1 items 2-3, R-4): interject_cause is
    the runner's named cause for an interject arm the rig never made work
    (interject-send-timeout: <arm_id>; interject-unavailable:
    <arm_id>.<disarm_reason>, no_record included). It is decided outside and
    before `if wire_dir:`, since it needs no wire artifact and a crashed turn
    may leave no wire/ directory, so it also precedes D-8 and the
    no-scored-pins return.
    Returns (status, info)."""
    info = {"premises": [], "findings": [], "evidence_violations": [],
            "blocked_reason": None, "vacuous_reason": None,
            "no_evidence": False}
    if case.get("recon"):
        return (pre_status if pre_status == "RECON" else pre_status), info
    if pre_status == "SKIP":
        return pre_status, info
    if interject_cause:
        info["blocked_reason"] = interject_cause
        return "BLOCKED", info
    # REDTEAM-REFRESH T2 (r3 §4.2.2 rule 1, r3a A2): a request the route map
    # could not resolve never reached any origin, so nothing this case scored
    # is interpretable. An undecided pin route (r3b B1 rule 3) is the same
    # class: the harness never chose where the request belonged.
    if wire_dir:
        undecided = _read_cause_names(wire_dir, ROUTE_BLOCK_FILE,
                                      "detail")
        if undecided:
            # The cause file carries the whole cause string, so the
            # row and verdict.json report the kind actually
            # decided — "ambiguous-pin-route: <id>" (r3b B1) or
            # "ambiguous-route: <id>" (RR-3 F-3) — instead of one
            # hardcoded prefix covering both.
            info["blocked_reason"] = "; ".join(undecided)
            return "BLOCKED", info
        unrouted = _read_cause_names(wire_dir, UNROUTED_FILE, "body_model")
        if unrouted:
            info["blocked_reason"] = "unrouted: " + ", ".join(unrouted)
            return "BLOCKED", info
    # D-8: require_wire_evidence downgrade (a claim without a file is
    # not a claim) — precedes the premise pass.
    violations = audit_wire_evidence(case, results, wire_dir)
    if violations:
        info["evidence_violations"] = violations
        info["no_evidence"] = True
        info["blocked_reason"] = ("NO-EVIDENCE: "
                                  + "; ".join(violations))
        return "BLOCKED", info
    scored = [r for r in results if not r.recon]
    # A runtime verdict with NO scored evidence at all (a crashed
    # turn, the wiretap never started, no pins evaluated): the verdict
    # engine adjudicates scored runs — with none, the runtime verdict
    # stands (a runtime FAIL is not a silent pass; the W3 retry loop
    # keys off this: an env-flap cell is FAIL with no scored pins).
    if not scored:
        return pre_status, info
    pin_results = {}
    for r in results:
        if r.recon:
            continue
        sid = (r.spec or {}).get("id")
        if isinstance(sid, str):
            pin_results.setdefault(sid, r)
    scoring = case.get("scoring") or {}
    vif = scoring.get("vacuous_if") or []
    if isinstance(vif, list):
        for entry in vif:
            if not isinstance(entry, dict):
                continue
            pin = entry.get("pin")
            cls = entry.get("class", "model")
            r = pin_results.get(pin) if isinstance(pin, str) else None
            hit = bool(r is not None and r.ok)
            info["premises"].append({
                "pin": pin, "class": cls, "hit": hit,
                "note": entry.get("note", ""),
                "unresolved": r is None})
    harness_miss = [p for p in info["premises"]
                    if p["class"] == "harness" and not p["hit"]]
    if harness_miss:
        info["blocked_reason"] = "harness premise 0-hit: " + "; ".join(
            "%s%s" % (p["pin"],
                      (" (%s)" % p["note"][:120]) if p["note"] else "")
            for p in harness_miss)
        return "BLOCKED", info
    model_miss = [p for p in info["premises"]
                  if p["class"] == "model" and not p["hit"]]
    if model_miss:
        info["vacuous_reason"] = "model premise 0-hit: " + "; ".join(
            "%s%s" % (p["pin"],
                      (" (%s)" % p["note"][:120]) if p["note"] else "")
            for p in model_miss)
        return "VACUOUS", info
    hard = [r for r in results if not r.ok and not r.recon]
    if hard:
        if case.get("tolerant"):
            info["findings"] = [
                {"id": (r.spec or {}).get("id"), "kind": r.kind,
                 "detail": r.detail} for r in hard]
            return "FINDING-PASS", info
        return "FAIL", info
    return ("PASS" if pre_status in ("PASS", "FAIL") else pre_status), info


# ---------------------------------------------------------------------------
# Case execution
# ---------------------------------------------------------------------------

class CaseCtx:
    def __init__(self):
        self.events = []          # merged pseudo-event stream
        self.fmt = "streaming-json"
        self.session_dir = None
        self.session_id = None
        self.capture_dir = None
        self.turns = []           # per-turn records
        self.snapshots = []       # history snapshots per step
        self.model_calls = 0
        self.last_exit = None
        self.acp = None
        self.crashed = False
        self.row_order = []
        # apex-ayl.22 D-3/D-5: the verdict info + the env-retry count.
        self.verdict = None
        self.retry_count = 0


def snapshot_history(ctx, op, home, cwd):
    path = os.path.join(home.session_dir_for(cwd, ctx.session_id),
                        "chat_history.jsonl") if ctx.session_id else None
    rec = {"op": op}
    if path and os.path.exists(path):
        with open(path) as fh:
            content = fh.read()
        rec["chat_history_lines"] = len([l for l in content.splitlines()
                                         if l.strip()])
        rec["bytes"] = len(content)
    ctx.snapshots.append(rec)


def run_case(case, args, budget):
    """apex-ayl.22 D-5: the env retry loop around one case.
    attempt-1 runs at the run_dir root; attempt-N under
    <case>/attempt-N/ (fresh hermetic home + wiretap each attempt —
    evidence RETAINED, never overwritten). A retried-pass cell is
    stability FLAKY (runbook L84/L1315)."""
    cid = case["id"]
    base_run_dir = os.path.join(args.out, cid.lower())
    os.makedirs(base_run_dir, exist_ok=True)
    retry_block = case.get("retry") or {}
    max_attempts = max(1, int(retry_block.get("max_attempts", 1)))
    attempt = 1
    run_dir = base_run_dir
    # T3 (r3a A13): the route table this case decided is what the recheck
    # asks — the row name is not an origin and not a wire id. _run_case_once
    # fills it because it is the only place that has the hermetic config
    # before its base_urls collapse onto the wiretap.
    route_ctx = {}
    row = _run_case_once(case, args, budget, run_dir, attempt, route_ctx)
    rechecks = []
    while row["status"] in ("FAIL", "BLOCKED"):
        ok, reason, recheck = _retry_decision(
            case, os.path.join(run_dir, "wire"), case.get("model"),
            attempt, max_attempts, live_upstream(args.live_home),
            getattr(args, "ambient_key", ""), route_ctx=route_ctx)
        if recheck is not None:
            # r3b B7: a LOST retry is recorded too, so "the catalog could not
            # be read" is visible in the row instead of looking like a
            # failure with no explanation.
            rechecks.append({"op": "recheck_models",
                             "model": _route_for_model(
                                 route_ctx, case.get("model"))[0],
                             "present": recheck,
                             "reason": reason,
                             "ts": utc_ts()})
        if not ok:
            log("  %s: no env retry (%s)" % (cid, reason))
            break
        backoff = retry_block.get("backoff_s", 30)
        log("  %s: env retry %d/%d after %ss backoff (%s)"
            % (cid, attempt + 1, max_attempts, backoff, reason))
        time.sleep(backoff)
        attempt += 1
        run_dir = os.path.join(base_run_dir, "attempt-%d" % attempt)
        row = _run_case_once(case, args, budget, run_dir, attempt)
        if row["status"] not in ("FAIL", "BLOCKED"):
            row["stability"] = "FLAKY"
    if rechecks:
        row["turns"] = rechecks + row["turns"]
    return row


def _run_case_once(case, args, budget, run_dir, attempt, route_out=None):
    cid = case["id"]
    os.makedirs(run_dir, exist_ok=True)
    started = time.time()
    ctx = CaseCtx()
    fmt = case.get("output_format", "streaming-json")
    ctx.fmt = fmt
    results = []
    status = "PASS"
    exp_red = case.get("expected_red") or {}

    def mark(status_, why=""):
        nonlocal status
        if status == "FAIL" and status_ != "FAIL":
            return
        status = status_
        if why:
            shown = ("RED-EXPECTED (%s)" % exp_red.get("id", "?")
                     if exp_red and status_ == "FAIL" else status_)
            log("  %s -> %s (%s)" % (cid, shown, why))

    try:
        if case.get("disabled"):
            row = _skip_row(case, "disabled: %s" % case.get("reason",
                                                           case.get(
                                                               "unblocks_at",
                                                               "")))
            # N1: terminal-shape line so sweepctl's log parser closes the
            # === marker (done=74/74, calls=0); the reason rides in
            # verdict.json + the report table.
            log("  %s: SKIP in 0.0s calls=0" % cid)
            _skip_verdict(run_dir, case, row, ctx, args)
            return row
        est = case.get("est_calls", 3)
        if budget.used + est > args.budget:
            row = _skip_row(case, "budget: used=%d est=%d limit=%d"
                            % (budget.used, est, args.budget))
            log("  %s: SKIP in 0.0s calls=0" % cid)
            _skip_verdict(run_dir, case, row, ctx, args)
            return row

        wirecap = (case.get("wirecap", True) and args.wirecap)
        proxy_none = (case.get("env") or {}).get("proxy") == "none"
        # apex-2o6: the L1 key-free runtime backstop (validate_case_file
        # is the primary gate; this catches hand-run rosters).
        if proxy_none and (case.get("rows")
                           or any(isinstance(st, dict)
                                  and st.get("op") in LIVE_MODEL_OPS
                                  for st in case.get("steps") or [])):
            mark("BLOCKED", "proxy:none case carries live model ops "
                            "(rows or a LIVE_MODEL_OPS step) — the "
                            "contract gate should have caught this")
            return _finish(case, run_dir, ctx, results, status,
                           started, None, None, args, budget,
                           attempt=attempt)
        port = free_port() if (wirecap or proxy_none) else None
        # apex-ayl.22 D-10 (G4): the case cwd wins over the hermetic
        # home cwd (the binary runs read-only in the case cwd); a
        # non-absolute cwd fail-closes to BLOCKED at the runtime
        # (the NEW-case gate catches it at validation).
        case_cwd = case.get("cwd")
        if case_cwd is not None and not os.path.isabs(case_cwd):
            mark("BLOCKED", "case cwd %r must be an absolute path (G4)"
                 % case_cwd)
            return _finish(case, run_dir, ctx, results, status,
                           started, None, None, args, budget,
                           attempt=attempt)
        # apex-ayl.22 D-7: the case env.provider_vars_unset extends
        # the runner L1 set for every binary launch in this case.
        extra_unset = (case.get("env") or {}).get(
            "provider_vars_unset")
        # T3 §4.3.4: no route map (no wiretap) means
        # no reachable-route credential to hand over.
        case_credential_names = []
        home = HermeticHome(run_dir, args.live_home,
                            case.get("config_patch", {}), port,
                            keep=args.keep_home)
        # apex-h2u2 M-3' (r2 §4.1/§4.4, OD-2 (a) fail-closed): a policy that
        # drops or overrides HOME undoes the scratch-home pin inside every
        # tool command, so the case is decided before anything runs.
        policy_cause = hermetic_shell_policy_cause(home.route_cfg)
        if policy_cause:
            mark("BLOCKED", policy_cause)
            return _finish(case, run_dir, ctx, results, status,
                           started, None, home, args, budget,
                           attempt=attempt)
        # REDTEAM-REFRESH T2 (r3 §4.2.1, r3b B1): the case's routes are
        # decided before anything runs. Only a case the wiretap fronts has a
        # map — the L1 key-free path starts no wiretap (and reaches no
        # origin), so routing cannot and must not change it.
        route_map = None
        if wirecap:
            r_sel, r_pins = case_route_selectors(case,
                                                getattr(args, "rows", None))
            route_map, route_causes = home.route_map(r_sel, r_pins)
            if route_causes:
                write_route_block(run_dir, route_causes)
                mark("BLOCKED", "; ".join(route_causes))
                return _finish(case, run_dir, ctx, results, status,
                               started, None, home, args, budget,
                               attempt=attempt)
            # r3 §4.3.3: the third unset state — a name a REACHABLE route
            # resolves to — blocks this case before its first step. Nothing
            # may be forwarded with a credential the route does not have.
            surface = credential_surface(home.row_routes())
            cred_values, cred_causes = resolve_route_credentials(
                route_map, declared=surface["names"])
            if cred_causes:
                write_credential_block(run_dir, cred_causes,
                                       sorted(route_map))
                mark("BLOCKED", "; ".join(cred_causes))
                return _finish(case, run_dir, ctx, results, status,
                               started, None, home, args, budget,
                               attempt=attempt)
            # apex-sk8i (spec r2 §2.2): the sign-in key joins the route
            # credentials in the child only; route_out and the route map keep
            # the routes' own credentials.
            signin_values, signin_source, signin_causes = (
                resolve_signin_credentials(route_map, home.row_routes()))
            if signin_causes:
                write_credential_block(run_dir, signin_causes,
                                       sorted(route_map))
                mark("BLOCKED", "; ".join(signin_causes))
                return _finish(case, run_dir, ctx, results, status,
                               started, None, home, args, budget,
                               attempt=attempt)
            case_credential_names = sorted(set(cred_values)
                                           | set(signin_values))
            if signin_values:
                log("  %s: sign-in credential %s (%s)"
                    % (cid, ",".join(sorted(signin_values)), signin_source))
            if route_out is not None:
                route_out["route_map"] = route_map
                route_out["credential_values"] = cred_values
        case_cwd = case_cwd or home.cwd
        upstream = live_upstream(args.live_home)
        wt = None
        if wirecap or proxy_none:
            ctx.capture_dir = os.path.join(run_dir, "wire")
            # apex-ayl.79 (XW-JIG-1): a case may pre-place its input wire
            # dir — <CASES_DIR>/<case-stem>/wire/ (the mechanism-case
            # convention; the xwfix cell-seed pattern at the wire level):
            # pre-placed captures + golden fixtures land in the run's
            # wire dir so the wire asserts (file/golden globs) resolve
            # against them. Copy-if-absent into the run's wire dir at
            # case start (idempotent across retry attempts — attempts
            # re-run into fresh dirs). Wiretap captures with open("w")
            # (smoke/wiretap/wiretap.py:194-197, review-verified), so a
            # live capture with the SAME name overwrites the pre-placed
            # file = the live capture is the evidence (a pre-placed
            # file can never mask a live capture). Residual hazard is
            # GHOST names: a pre-placed req-*.json the run never
            # generates persists in the wire dir and can land in a
            # where-filter (bounded by the count pin) — future live
            # switch_model cases must NOT pre-place req-*.json;
            # expected fixtures use non-colliding names (golden-*.json,
            # expected_wire.json).
            _pl_src = os.path.join(CASES_DIR, cid.lower(), "wire")
            if os.path.isdir(_pl_src):
                os.makedirs(ctx.capture_dir, mode=0o700, exist_ok=True)
                for _pl in sorted(os.listdir(_pl_src)):
                    _pl_dst = os.path.join(ctx.capture_dir, _pl)
                    if not os.path.exists(_pl_dst):
                        shutil.copyfile(os.path.join(_pl_src, _pl),
                                        _pl_dst)
                        log("  %s: pre-placed wire input %s"
                            % (cid, _pl))
            if proxy_none and not wirecap:
                # apex-2o6: the L1 key-free mode — the pre-placed wire
                # dir is the case's ONLY evidence source; no Wiretap
                # (the hermetic base_url points at the dead port).
                log("  %s: proxy:none L1 key-free — pre-placed wire "
                    "inputs only (no Wiretap; dead port %d)"
                    % (cid, port))
            if wirecap:
                wt = Wiretap(port, upstream, ctx.capture_dir,
                             args.ambient_key, route_map=route_map)
                if not wt.wait_ready():
                    mark("BLOCKED", "wiretap did not start")
                    return _finish(case, run_dir, ctx, results, status,
                                   started, wt, home, args, budget)
                log("  %s: wiretap on :%d capture=%s/wire" % (cid, port,
                                                              cid.lower()))

        cur_model = case.get("model")
        seed = case.get("seed")
        if seed and seed.get("hist_tokens"):
            sid = str(uuid.uuid4())
            ctx.session_id = sid
            sdir, n_lines, nbytes, est_tok = seed_history(
                home, case_cwd, sid, cur_model, seed["hist_tokens"],
                seed.get("hist_bytes", 0))
            ctx.session_dir = sdir
            v = validate_roundtrip(os.path.join(sdir, "chat_history.jsonl"))
            log("  %s: seeded %s lines=%d bytes=%d est_tokens=%d "
                "(target %d) roundtrip=OK" % (cid, sid, v, nbytes, est_tok,
                                              seed["hist_tokens"]))
            snapshot_history(ctx, "seed", home, case_cwd)

        # apex-ayl.70 (XW-FIXTURES-1): a switch_model step with a cell
        # seeds the fresh session with the cell's pre_switch.json
        # VERBATIM at case start (headless: each turn process re-reads
        # chat_history.jsonl at start, so this is the proven seeder
        # ordering). The ACP re-seed happens after session/new, below.
        _xw_cell = next((s.get("cell") for s in case.get("steps", [])
                         if s.get("op") == "switch_model"
                         and s.get("cell")), None)
        if _xw_cell and case.get("driver", "headless") == "headless" \
                and not (seed and seed.get("hist_tokens")):
            _xw_sid = str(uuid.uuid4())
            ctx.session_id = _xw_sid
            _xw = xwfix_seed_from_cell(home, case_cwd, _xw_sid, cur_model,
                                       _xwfix_cell_dir(_xw_cell))
            if _xw:
                ctx.session_dir = _xw[0]
                log("  %s: xwfix cell-seeded %s records=%d bytes=%d "
                    "(cell=%s)" % (cid, _xw_sid, _xw[1], _xw[2],
                                   _xw_cell))
                snapshot_history(ctx, "xwfix_seed", home, case_cwd)
            else:
                mark("BLOCKED", "xwfix cell seed failed: unreadable "
                                "pre_switch.json for %s" % _xw_cell)
                return _finish(case, run_dir, ctx, results, status,
                               started, wt, home, args, budget)

        driver = case.get("driver", "headless")
        # apex-ayl.70 R2-RED stage-2 (coordinator ruling 2026-09-18,
        # Option A APPROVED): cell-seeded ACP cases enter LOAD-FIRST —
        # the session id is pre-minted HERE and the cell history is
        # pre-seeded on disk at the canonical persistence location
        # (home/sessions/<enc-cwd>/<sid>/) BEFORE the ACP connect, then
        # the session is entered via session/load (no session/new).
        # session/load re-hydrates from disk ONLY for non-resident
        # sessions (session_setup.rs:1023); the old new→seed→load flow
        # made the session resident at load time and silently
        # discarded the seed (FINDING A — voided ② run-1/run-2). Same
        # two-file layout (chat_history.jsonl + summary.json) the
        # proven headless cold seeder uses; only the entry path
        # changes.
        _xw_load_sid = None
        _xw_pre = None
        if _xw_cell and driver == "acp":
            _xw_load_sid = str(uuid.uuid4())
            ctx.session_id = _xw_load_sid
            _xw_pre = xwfix_seed_from_cell(
                home, case_cwd, _xw_load_sid, cur_model,
                _xwfix_cell_dir(_xw_cell))
            if not _xw_pre:
                mark("BLOCKED", "xwfix cell seed failed (pre-connect): "
                                "unreadable pre_switch.json for %s"
                                % _xw_cell)
                return _finish(case, run_dir, ctx, results, status,
                               started, wt, home, args, budget)
            ctx.session_dir = _xw_pre[0]
            log("  %s: xwfix cell pre-seeded BEFORE connect %s "
                "records=%d bytes=%d (cell=%s; load-first entry)"
                % (cid, _xw_load_sid, _xw_pre[1], _xw_pre[2], _xw_cell))
            snapshot_history(ctx, "xwfix_seed", home, case_cwd)
        if driver == "acp":
            acp = AcpSession(args.bin, home.home, case_cwd, cur_model,
                             os.path.join(run_dir, "acp.log"),
                             init_timeout_s=_effective_acp_init_timeout(
                                 case),
                             set_model_timeout_s=
                             _effective_set_model_timeout(case),
                             extra_unset=extra_unset,
                             credential_names=case_credential_names,
                             load_first_sid=_xw_load_sid)
            acp.start()
            ctx.acp = acp
            if _xw_load_sid:
                # Model state (Option A): the seeded summary.json
                # carries the pre-switch model; the load result
                # carries models.currentModelId — ADOPTED as the
                # actor's model state (the mechanism, recorded per the
                # ruling). Never a raw session/new on this path.
                _xw_ld_model = ((acp.load_result or {}).get("models")
                                or {}).get("currentModelId")
                ctx.events.append({"type": "acp_session_load",
                                   "sessionId": acp.session_id,
                                   "currentModelId": _xw_ld_model,
                                   "case_model": cur_model, "_line": 0})
                log("  %s: xwfix load-first session %s "
                    "currentModelId=%s (case model=%s; records=%d "
                    "pre-seeded)" % (cid, acp.session_id, _xw_ld_model,
                                     cur_model, _xw_pre[1]))
                if _xw_ld_model and _xw_ld_model != cur_model:
                    mark("FAIL", "load result currentModelId %s != case "
                                 "model %s (seed not honored?)"
                         % (_xw_ld_model, cur_model))
                elif not _xw_ld_model:
                    log("  %s: NOTE — load result carries no "
                        "currentModelId; pre-switch model taken from "
                        "the seeded summary.json (case model=%s)"
                        % (cid, cur_model))
            else:
                log("  %s: ACP session %s model=%s" % (cid, acp.session_id,
                                                       cur_model))
            ctx.session_id = acp.session_id
            ctx.session_dir = home.session_dir_for(case_cwd,
                                                   acp.session_id)
            ctx.fmt = "acp"
        extra = []
        if case.get("agents_json"):
            extra += ["--agents", json.dumps(case["agents_json"])]

        rows = case.get("rows")
        if rows:
            ctx.row_order = []
            # REDTEAM-REFRESH T4 (r3 §4.7, s0 F-10): the run order records
            # the wire id each row sends; the authored names ride beside it,
            # one entry per row, so two rows on one wire id stay apart.
            ctx.row_names = []
            ctx.row_routes = home.row_routes() if wirecap else None
            want = {r.strip() for r in args.rows.split(",")} if args.rows \
                else None
            for row in rows:
                model = row["model"]
                if want and model not in want:
                    continue
                ctx.row_order.append(row_wire_id(model, ctx.row_routes))
                ctx.row_names.append(model)
                tr = run_headless_turn(
                    args.bin, home.home, case_cwd, model,
                    row.get("prompt", "Reply with exactly: HT1-OK"),
                    None, output_format=fmt,
                    timeout_s=_effective_turn_timeout(case),
                    capture_dir=ctx.capture_dir,
                    extra_unset=extra_unset,
                    credential_names=case_credential_names)
                ctx.turns.append({"op": "row", "model": model,
                                  "exit": tr.exit_code,
                                  "killed": tr.killed,
                                  "elapsed": tr.elapsed,
                                  "session_id": tr.session_id,
                                  "text": ndjson_text(tr.events, fmt)[:200]})
                ctx.events.extend(tr.events)
                mark("FAIL" if (tr.exit_code not in (0, None)
                                and not tr.killed) else status,
                     "row %s exit=%s" % (model, tr.exit_code))
            if wirecap:
                ctx.model_calls = count_wire_model_calls(
                    os.path.join(run_dir, "wire"))
            _copy_session_evidence(home, ctx, run_dir)
            return _finish(case, run_dir, ctx, results, status, started,
                           wt, home, args, budget,
                           row_asserts=case.get("row_asserts"),
                           attempt=attempt)

        for si, step in enumerate(case.get("steps", [])):
            op = step.get("op")
            if op == "turn":
                model = step.get("model", cur_model)
                # apex-bdfz BZ-7 (plan r3 sections 2.5.1, 2.8): the turn's
                # interject arms and the records the pump writes for them.
                arms = step.get("interject")
                interjects, pumped_s = [], None
                if driver == "acp":
                    before = len(acp.updates)
                    if arms:
                        # The single-reader pump reads the turn to its
                        # terminal and fires the arms; prompt_collect then
                        # takes the turn from the inbox, inside the budget
                        # prompt() would have had (timeout or self.timeout_s).
                        turn_s = step.get("timeout_s",
                                          _effective_turn_timeout(case)) \
                            or acp.timeout_s
                        tid, pid = acp.prompt_send(step["prompt"])
                        t_sent = time.time()
                        deadline = t_sent + turn_s
                        interjects = acp._pump_mid_turn(
                            arms, acp._inflight, deadline, step=si,
                            t_sent=t_sent)
                        pumped_s = time.time() - t_sent
                        resp, stop = acp.prompt_collect(
                            tid, pid, max(0.0, deadline - time.time()))
                    else:
                        resp, stop = acp.prompt(step["prompt"],
                                                timeout=step.get(
                                                    "timeout_s",
                                                    _effective_turn_timeout(
                                                        case)))
                    chunks = []
                    for u in acp.updates[before:]:
                        if u.get("sessionUpdate") == \
                                "agent_message_chunk":
                            c = u.get("content") or {}
                            if c.get("type") == "text":
                                chunks.append(c.get("text", ""))
                    # apex-ayl.28: the correlated prompt identity of
                    # THIS step (forensics: which prompt the recorded
                    # turn actually was — the B-ii desync made step
                    # labels silently off-by-one).
                    prompt_id = (resp.get("result") or
                                 resp.get("error") or {}).get("promptId")
                    if prompt_id is None:
                        # apex-bdfz BZ-R7 F-4(a): the session/prompt response
                        # the pump kept (it read past the turn's terminal)
                        # carries the id in result._meta only.
                        prompt_id = ((resp.get("result") or {}).get("_meta")
                                     or {}).get("promptId")
                    rec = {"op": "acp_turn", "step": si, "model": model,
                           "stop": stop,
                           "error": resp.get("error"),
                           "prompt_id": prompt_id,
                           "text": "".join(chunks)[:200]}
                    if arms:
                        rec["interjects"] = interjects
                    ctx.turns.append(rec)
                    for c in chunks:
                        ctx.events.append({"type": "acp_message",
                                           "data": c, "_line": 0})
                    # One acp_interject pseudo-event per arm record. The text
                    # rides under interject_text, never as data and never as
                    # an acp_message: ndjson_text reads data off acp_message
                    # only, so the runner can never satisfy a nonce pin by
                    # quoting its own injection. tool_calls_seen and
                    # elapsed_s are taken at the fire, or at the end of the
                    # pumped read for an arm that never fired.
                    if interjects:
                        calls = len({u.get("toolCallId")
                                     for u in acp.updates[before:]
                                     if u.get("sessionUpdate") == "tool_call"})
                        for arm_rec in interjects:
                            fa = arm_rec.get("fired_at") or {}
                            arm_ev = {"type": "acp_interject",
                                      "arm_id": arm_rec["arm_id"],
                                      "fired": arm_rec["fired"],
                                      "mid_turn": arm_rec["mid_turn"],
                                      "status": arm_rec["status"],
                                      "disarm_reason":
                                          arm_rec["disarm_reason"],
                                      "harness_caused":
                                          arm_rec["harness_caused"],
                                      "tool_calls_seen":
                                          fa.get("n_tool_calls", calls),
                                      "elapsed_s":
                                          fa.get("elapsed_s", pumped_s),
                                      "_line": 0}
                            for k in ("model", "interject_text"):
                                if k in arm_rec:
                                    arm_ev[k] = arm_rec[k]
                            ctx.events.append(arm_ev)
                            # apex-bdfz BZ-R7 F-1: a set_model arm that fired
                            # lands the switch as the switch step does
                            # (acp_set_model; later steps default to the
                            # target, while this turn completes on the model it
                            # was sent with). A JSON-RPC error on its own
                            # request is the product refusing the switch:
                            # acp_error + FAIL, as the switch step reports it,
                            # and no rig cause below (interject arms keep
                            # section 2.4 D-6).
                            if arm_rec.get("send") != "set_model":
                                continue
                            if arm_rec["status"] == "fired":
                                ctx.events.append(
                                    {"type": "acp_set_model",
                                     "model": arm_rec["model"], "_line": 0})
                                cur_model = arm_rec["model"]
                            elif arm_rec["disarm_reason"] == "ext_error":
                                err = (arm_rec.get("resp") or {}).get("error")
                                ctx.events.append({"type": "acp_error",
                                                   "error": err,
                                                   "model": arm_rec["model"]})
                                mark("FAIL", "acp mid-turn set_model error: %s"
                                     % str(err)[:120])
                    if "error" in resp:
                        ctx.events.append({"type": "acp_error",
                                           "error": resp["error"],
                                           "model": model})
                        mark("FAIL", "acp prompt error: %s"
                             % str(resp["error"])[:120])
                    else:
                        ctx.events.append({"type": "acp_end",
                                           "stopReason": stop,
                                           "model": model,
                                           "_line": 0})
                    # OVERWATCH rig repair (2026-09-19, AT-AZ-VXR): the ACP
                    # path never set ctx.last_exit (headless-only) -> ACP
                    # cases declaring exit:0 failed 'last exit None want 0'.
                    # ACP analog: 0 = prompt closed without an ACP error.
                    ctx.last_exit = 1 if "error" in resp else 0
                    if ctx.session_dir and not os.path.isdir(
                            ctx.session_dir):
                        ctx.session_dir = home.session_dir_for(
                            case_cwd, acp.session_id)
                else:
                    tr = run_headless_turn(
                        args.bin, home.home, case_cwd, model,
                        step["prompt"], ctx.session_id, output_format=fmt,
                        extra_args=extra,
                        timeout_s=step.get("timeout_s",
                                           _effective_turn_timeout(case)),
                        kill_after_s=step.get("kill_after_s"),
                        capture_dir=ctx.capture_dir,
                        extra_unset=extra_unset,
                        credential_names=case_credential_names)
                    ctx.turns.append({"op": "turn", "step": si,
                                      "model": model,
                                      "exit": tr.exit_code,
                                      "killed": tr.killed,
                                      "elapsed": tr.elapsed,
                                      "session_id": tr.session_id,
                                      "text": ndjson_text(tr.events,
                                                          fmt)[:200],
                                      "stderr_tail": tr.stderr[-300:]})
                    ctx.events.extend(tr.events)
                    if tr.session_id:
                        ctx.session_id = tr.session_id
                        ctx.session_dir = home.session_dir_for(
                            case_cwd, tr.session_id)
                    if tr.killed:
                        mark("FAIL" if step.get("expect_kill") is False
                             else status,
                             "turn killed (watchdog/kill_after)")
                    elif tr.exit_code != 0:
                        want_exit = (case.get("assert") or {}).get("exit")
                        if tr.exit_code == want_exit:
                            # Case-declared expected error terminal: an
                            # honest-failure pin, not a crash. The hard
                            # `exit` assert still verifies the exact value
                            # at assert stage (drift either way FAILs).
                            log("  %s: turn exit=%s (expected terminal)"
                                % (cid, tr.exit_code))
                        else:
                            mark("FAIL", "turn exit=%s" % tr.exit_code)
                            ctx.crashed = True
                    ctx.last_exit = tr.exit_code
                if arms:
                    # The runner's BLOCKED cause (plan r3 section 2.5.1 items
                    # 2-3, erratum E-1): each written record's own cause,
                    # unless its arm says require: false (that suppresses the
                    # cause only), and interject-unavailable:
                    # <arm_id>.no_record for a declared arm the turn left
                    # without a record (the headless driver never arms one).
                    # A set_model arm the product refused adds none: it was
                    # reported as acp_error + FAIL above (BZ-R7 F-1).
                    # Joined across the case's armed turns; _finish passes it
                    # to finalize_verdict.
                    written = {r.get("arm_id"): r for r in interjects}
                    causes = []
                    for ai, arm_spec in enumerate(arms):
                        aid = "step%d.arm%d" % (si, ai)
                        arm_rec = written.get(aid)
                        if arm_rec is None:
                            causes.append(
                                "interject-unavailable: %s.no_record" % aid)
                        elif arm_rec.get("cause") and not (
                                isinstance(arm_spec, dict)
                                and arm_spec.get("require") is False) \
                                and not (arm_rec.get("send") == "set_model"
                                         and arm_rec.get("disarm_reason")
                                         == "ext_error"):
                            causes.append(arm_rec["cause"])
                    if causes:
                        prior = getattr(ctx, "interject_cause", None)
                        ctx.interject_cause = "; ".join(
                            ([prior] if prior else []) + causes)
                snapshot_history(ctx, "turn%d" % si, home, case_cwd)
            elif op == "kill":
                model = step.get("model", cur_model)
                if driver == "acp":
                    acp.kill()
                    ctx.turns.append({"op": "kill", "step": si})
                else:
                    tr = run_headless_turn(
                        args.bin, home.home, case_cwd, model,
                        step.get("prompt", "KILL-PROBE"),
                        ctx.session_id, output_format=fmt, extra_args=extra,
                        timeout_s=60,
                        kill_after_s=step.get("after_s", 1.5),
                        capture_dir=ctx.capture_dir,
                        extra_unset=extra_unset,
                        credential_names=case_credential_names)
                    ctx.turns.append({"op": "kill", "step": si,
                                      "killed": tr.killed,
                                      "elapsed": tr.elapsed})
                    snapshot_history(ctx, "kill", home, case_cwd)
            elif op == "switch":
                cur_model = step["model"]
                if step.get("via", "acp") == "acp" and driver == "acp":
                    resp = acp.set_model(cur_model)
                    ctx.turns.append({"op": "switch", "step": si,
                                      "model": cur_model,
                                      "resp": str(resp)[:200]})
                    ctx.events.append({"type": "acp_set_model",
                                       "model": cur_model, "_line": 0})
                    if "error" in resp:
                        ctx.events.append({"type": "acp_error",
                                           "error": resp["error"],
                                           "model": cur_model})
                        mark("FAIL", "acp set_model error: %s"
                             % str(resp["error"])[:120])
                else:
                    ctx.turns.append({"op": "switch", "step": si,
                                      "model": cur_model, "via": "resume"})
            elif op == "switch_model":
                # apex-ayl.70 (XW-FIXTURES-1): mid-session switch + the
                # XW-FIXTURES cell RED diff. Drives the switch exactly
                # like `switch`, then scores the storage-form cell diff
                # AT SWITCH TIME so the post-switch turn cannot
                # contaminate the pinned history (the wire-form diff is
                # scored at wirecap time, below, through the golden
                # engine).
                cur_model = step["model"]
                cell = step.get("cell", "")
                form = step.get("assert_form", "storage")
                cell_dir = _xwfix_cell_dir(cell) if cell else None
                if step.get("via", "acp") == "acp" and driver == "acp":
                    resp = acp.set_model(cur_model)
                    ctx.turns.append({"op": "switch_model", "step": si,
                                      "model": cur_model,
                                      "resp": str(resp)[:200]})
                    ctx.events.append({"type": "acp_set_model",
                                       "model": cur_model, "_line": 0})
                    if "error" in resp:
                        ctx.events.append({"type": "acp_error",
                                           "error": resp["error"],
                                           "model": cur_model})
                        mark("FAIL", "acp set_model error: %s"
                             % str(resp["error"])[:120])
                else:
                    ctx.turns.append({"op": "switch_model", "step": si,
                                      "model": cur_model, "via": "resume"})
                snapshot_history(ctx, "switch_model", home, case_cwd)
                if cell_dir:
                    ctx.xwfix_wire_pending = (cell_dir, cur_model, form)
                    if form == "storage":
                        results.append(xwfix_cell_diff_storage(
                            cell_dir, ctx.session_dir))
                    ctx.turns[-1]["cell"] = cell
                    ctx.turns[-1]["assert_form"] = form
            elif op == "compact":
                model = step.get("model", cur_model)
                if driver == "acp":
                    resp, stop = acp.prompt("/compact",
                                            timeout=_effective_turn_timeout(
                                                case))
                    ctx.turns.append({"op": "compact", "step": si,
                                      "stop": stop,
                                      "error": str(resp.get("error"))[:300]})
                    if "error" in resp:
                        ctx.events.append({"type": "acp_error",
                                           "error": resp["error"]})
                else:
                    tr = run_headless_turn(
                        args.bin, home.home, case_cwd, model, "/compact",
                        ctx.session_id, output_format=fmt, extra_args=extra,
                        timeout_s=_effective_turn_timeout(case),
                        capture_dir=ctx.capture_dir,
                        extra_unset=extra_unset,
                        credential_names=case_credential_names)
                    ctx.turns.append({"op": "compact", "step": si,
                                      "exit": tr.exit_code,
                                      "killed": tr.killed,
                                      "text": ndjson_text(tr.events,
                                                          fmt)[:200]})
                    ctx.events.extend(tr.events)
                    if tr.exit_code not in (0,) and not tr.killed:
                        mark("FAIL", "compact exit=%s" % tr.exit_code)
                snapshot_history(ctx, "compact", home, case_cwd)
            elif op == "idle":
                s = int(step.get("s", 60))
                log("  %s: idle %ds" % (cid, s))
                time.sleep(s)
            elif op == "recon_note":
                ctx.turns.append({"op": "recon_note", "step": si,
                                  "note": step.get("note", "")})
            else:
                raise ValueError("unknown step op: %r" % op)

        if driver == "acp" and ctx.acp and ctx.acp.proc and \
                ctx.acp.proc.poll() is None:
            try:
                ctx.acp.call("session/close",
                             {"sessionId": ctx.acp.session_id},
                             timeout=10)
            except Exception:
                pass
            ctx.acp.kill()
        if wirecap:
            ctx.model_calls = count_wire_model_calls(
                os.path.join(run_dir, "wire"))
        elif ctx.turns:
            ctx.model_calls = sum(
                1 for t in ctx.turns
                if t.get("op") in ("turn", "row") and t.get("exit") == 0)

        # apex-ayl.70 (XW-FIXTURES-1, ratchet ① R1A/NIT-3): the wire-form
        # cell diff — the first post-switch request is captured by
        # wirecap only after the post-switch turn, so it is scored here
        # (the storage form was scored at switch time). ONE engine: the
        # golden-kind compare (the retired cell_diff kind has no second
        # comparator). Active for a cell once expected_wire.json lands;
        # skipped with a note until then.
        if wirecap and getattr(ctx, "xwfix_wire_pending", None):
            _xw_cd, _xw_m, _xw_f = ctx.xwfix_wire_pending
            _xw_exp = os.path.join(_xw_cd, "expected_wire.json")
            if _xw_f == "wire" or os.path.isfile(_xw_exp):
                _xw_spec = {"op": "xwfix_cell_diff", "form": "wire",
                            "cell": os.path.basename(_xw_cd),
                            "model": _xw_m}
                _xw_paths = _wire_filter(
                    globmod.glob(os.path.join(run_dir, "wire",
                                              "req-*.json")),
                    {"method": "POST", "body.model": _xw_m})
                if not os.path.isfile(_xw_exp):
                    results.append(AssertResult(
                        _xw_spec, "xwfix_cell_diff.wire", True,
                        "expected_wire.json absent — wire-form byte pin "
                        "not cut yet (storage form is the active RED)",
                        "skipped (no byte pin)"))
                elif not _xw_paths:
                    results.append(AssertResult(
                        _xw_spec, "xwfix_cell_diff.wire", False,
                        "no captured request to model=%s" % _xw_m,
                        "wire form"))
                else:
                    _xw_first = min(_xw_paths, key=os.path.basename)
                    ok, detail, _xw_diff = golden_compare(
                        _xw_first, _xw_exp, [])
                    if not ok and _xw_diff is not None:
                        _golden_write_diff(os.path.join(run_dir, "wire"),
                                           _xw_spec, _xw_diff)
                    results.append(AssertResult(
                        _xw_spec, "xwfix_cell_diff.wire", ok,
                        "%s: %s" % (os.path.basename(_xw_first), detail),
                        "wire form; %s" % os.path.basename(_xw_first),
                        cite=_wire_cite(_xw_first) if ok else None))

        _copy_session_evidence(home, ctx, run_dir)
        assert_block = case.get("assert", {})
        for spec in assert_block.get("ndjson", []):
            if spec.get("op") == "recon":
                results.append(check_recon(spec, ctx))
                continue
            # OVERWATCH rig repair (2026-09-19, AT-AZ-VXR): ctx.fmt, not the
            # local headless fmt — the ACP driver stores turn text as
            # acp_message events (ctx.fmt="acp"); the local fmt made every
            # ACP text_contains pin read an empty stream.
            results.append(check_ndjson(spec, ctx.events, ctx.fmt, ctx))
        for spec in assert_block.get("artifact", []):
            if spec.get("op") == "recon":
                results.append(check_recon(spec, ctx))
                continue
            results.append(check_artifact(spec,
                                          os.path.join(run_dir, "session")
                                          if os.path.isdir(
                                              os.path.join(run_dir,
                                                           "session"))
                                          else (ctx.session_dir or
                                               os.devnull)))
        if wirecap or proxy_none:
            # apex-ayl.22 D-4b: the declared output_contains pins are
            # scored synthesized wire-grep pins (added to the case's
            # own explicit wire pins). apex-2o6: also for proxy:none
            # (the pre-placed wire dir is the evidence source).
            wire_specs = list(assert_block.get("wire", []))
            # T4 (r3 §4.7): only a case the wiretap fronts has a row table;
            # the key-free path keeps the authored byte (r3 §2.6).
            wire_specs.extend(synth_output_pins(
                case, home.row_routes() if wirecap else None))
            for spec in wire_specs:
                if spec.get("kind") == "recon" or spec.get("op") == "recon":
                    results.append(check_recon(spec, ctx))
                    continue
                results.append(check_wire(spec, os.path.join(run_dir,
                                                             "wire")))
        if "exit" in assert_block:
            want = assert_block["exit"]
            got = ctx.last_exit
            ok = (got == want)
            results.append(AssertResult({"exit": want}, "exit", ok,
                                         "last exit %s want %s" % (got,
                                                                  want),
                                         "turns: %d" % len(ctx.turns)))

        recon_only = bool(case.get("recon", False))
        hard_fails = [r for r in results if not r.ok and not r.recon]
        if recon_only:
            mark("RECON", "recon case recorded")
        elif hard_fails:
            mark("FAIL", "%d assert(s) failed" % len(hard_fails))
        if ctx.crashed and status == "PASS":
            mark("FAIL", "process crashed")
    except Exception as e:
        import traceback
        with open(os.path.join(run_dir, "runner-exception.txt"), "w") as fh:
            fh.write(traceback.format_exc())
        mark("FAIL", "runner exception: %s" % e)
    return _finish(case, run_dir, ctx, results, status, started, wt,
                   home, args, budget, attempt=attempt)


def _copy_session_evidence(home, ctx, run_dir):
    dst = os.path.join(run_dir, "session")
    if ctx.session_dir and os.path.isdir(ctx.session_dir) and \
            not os.path.exists(dst):
        keep = ["chat_history.jsonl", "summary.json", "events.jsonl",
                # apex-ayl.22 D-8: the hermetic home's unified session
                # log is copied too (T-S4 pins the copy behavior).
                "unified.jsonl",
                # apex-ayl.101 RED surface (CW-WRITE-USAGE-01): the usage
                # receipt is a session-dir artifact — cache-economics
                # ratchets assert on session.cacheCreationTokens.
                "usage.json",
                "compaction", "compaction_requests",
                "compaction_checkpoints", "subagents"]
        os.makedirs(dst, exist_ok=True)
        for k in keep:
            p = os.path.join(ctx.session_dir, k)
            if os.path.isdir(p):
                shutil.copytree(p, os.path.join(dst, k),
                                dirs_exist_ok=True)
            elif os.path.isfile(p):
                shutil.copy(p, os.path.join(dst, k))
    # apex-ayl.22 D-8: the ACP log (run_dir/acp.log) is copied into
    # session/ as well — the triage hunt protocol runs on the copied
    # layout only (never ~/.grok).
    acp_src = os.path.join(run_dir, "acp.log")
    if os.path.isfile(acp_src):
        os.makedirs(dst, exist_ok=True)
        shutil.copy(acp_src, os.path.join(dst, "acp.log"))


def _skip_row(case, why):
    return {"id": case["id"], "title": case.get("title", ""),
            "status": "SKIP", "duration_s": 0.0, "model_calls": 0,
            "turns": [], "asserts": [], "skipped": why,
            "run_dir": None, "snapshots": []}


def _skip_verdict(run_dir, case, row, ctx, args):
    """apex-ayl.91 N1: SKIP rows persist verdict.json (status/outcome
    SKIP, zero calls, empty evidence_index) so the campaign census is
    machine-readable end to end — the aggregator and seal validator
    count verdict files, and a log-only skip note left the row
    invisible to both (72/74 with two empty cell dirs)."""
    write_verdict_json(run_dir, case, row, ctx, [],
                       getattr(args, "campaign_id", None))


def _row_model_for_spec(spec):
    """The row identity of one row_asserts wire spec. The legacy form
    carries the `row_model` key; schema-compliant rows cases cannot
    (the frozen case.schema.json wire_assert has additionalProperties:
    false and no row_model — SWEEP-1: the 2026-09-17 rt-m6 regen), so
    the per-row identity rides the where filter's body.model value
    (bare or `$.`-prefixed). Returns None when the spec is not
    row-scoped (evaluated unscoped, as before)."""
    rm = spec.get("row_model")
    if rm is None:
        where = spec.get("where")
        if isinstance(where, dict):
            for wk, wv in where.items():
                key = wk[2:] if isinstance(wk, str) \
                    and wk.startswith("$.") else wk
                if key == "body.model" and isinstance(wv, str):
                    rm = wv
                    break
    return rm


def _row_position(ctx, rm):
    """REDTEAM-REFRESH T4 (r3 §4.7, r3a A10, s0 F-10): the run-order position
    of the row a row-scoped spec names, or None when no single row of this
    run is it (the caller records the recon `wire.row-skip`).

    A row name addresses that row only: a declared row that did not run is
    never stood in for by a row that sends the same wire id. Any other
    identity goes through `row_wire_id` and addresses the one row of this
    run that sends it; an id two rows of this run send addresses neither, as
    before T4 (r3 §3.7 keeps both members of a pair out of one case)."""
    names = getattr(ctx, "row_names", None) or []
    order = getattr(ctx, "row_order", [])
    routes = getattr(ctx, "row_routes", None)
    if rm in names:
        return names.index(rm)
    if any(r.get("row_name") == rm for r in routes or ()):
        return None
    wid = row_wire_id(rm, routes)
    return order.index(wid) if order.count(wid) == 1 else None


def _finish(case, run_dir, ctx, results, status, started, wt, home, args,
            budget, row_asserts=None, attempt=1):
    if wt:
        wt.stop()
    if row_asserts:
        for spec in row_asserts.get("wire", []):
            rm = _row_model_for_spec(spec)
            pos = _row_position(ctx, rm) if rm else None
            if rm and pos is None:
                results.append(AssertResult(
                    spec, "wire.row-skip", True,
                    "row %s not in this run (filtered by --rows)" % rm,
                    "wire: skipped", recon=True))
                continue
            if rm:
                spec = dict(spec)
                if spec.get("where"):
                    # Per-row where-filter: nth selects within the row's
                    # own requests (default 0 = newest of that row).
                    spec.setdefault("nth", 0)
                else:
                    spec["nth"] = pos
            results.append(check_wire(spec, os.path.join(run_dir, "wire")))
    hard_fails = [r for r in results if not r.ok and not r.recon]
    if row_asserts and hard_fails and status == "PASS":
        status = "FAIL"
    # apex-ayl.22 D-3/D-8: the verdict engine at the choke point —
    # every path (steps, rows, the early-wiretap BLOCKED) funnels
    # here; the premise pass + the NO-EVIDENCE downgrade apply to all.
    wire_dir = os.path.join(run_dir, "wire")
    if not os.path.isdir(wire_dir):
        wire_dir = None
    # apex-bdfz BZ-7 (plan r3 section 2.5.1 item 2): the dispatcher's joined
    # interject cause reaches the verdict engine here, on every path.
    status, vinfo = finalize_verdict(
        case, status, results, wire_dir=wire_dir,
        interject_cause=getattr(ctx, "interject_cause", None))
    ctx.verdict = vinfo
    budget.add(ctx.model_calls)
    exp_red = case.get("expected_red") or {}
    status_out = status
    if exp_red:
        rid = exp_red.get("id", "?")
        status_out = ("RED-EXPECTED (%s)" % rid if status == "FAIL"
                      else "RED-CLEARED (%s)" % rid)
        if status == "PASS":
            log("  %s: RED-CLEARED (%s) — documented red no longer "
                "reproduces; promote to hard assert" % (case["id"], rid))
    row = {"id": case["id"], "title": case.get("title", ""),
           "status": status,
           "expected_red": exp_red or None,
           "duration_s": round(time.time() - started, 1),
           "model_calls": ctx.model_calls,
           "session_id": ctx.session_id,
           "turns": ctx.turns,
           "asserts": [{"kind": r.kind, "ok": r.ok, "recon": r.recon,
                        "detail": r.detail, "evidence": r.evidence,
                        "spec": r.spec} for r in results],
           "snapshots": ctx.snapshots,
           "run_dir": run_dir,
           "attempt": attempt}
    # r3b B4: BLOCKED alone does not say what to fix. blocked_reason is set by
    # finalize_verdict (route causes, harness premise, NO-EVIDENCE) and the
    # triage hint below keys off it. `unrouted` is recorded whenever the
    # wiretap refused something — including the recon / SKIP rows whose status
    # rule 1 deliberately leaves alone.
    if vinfo.get("blocked_reason"):
        row["blocked_reason"] = vinfo["blocked_reason"]
    unrouted_n = _count_cause_lines(wire_dir, UNROUTED_FILE)
    if unrouted_n:
        row["unrouted"] = unrouted_n
    # apex-ayl.22 D-7: anomalies are RECORDED, never FAILed —
    # store=true on responses-req bodies (the store=false invariant
    # for proxy runs) and a case env.home that is not "hermetic"
    # (a live-config mismatch).
    anoms = []
    env_block = case.get("env")
    if isinstance(env_block, dict):
        hh = env_block.get("home")
        if hh and hh != "hermetic":
            anoms.append("env.home=%r (the runner home is hermetic; "
                         "a live-config mismatch is an anomaly, "
                         "never a case FAIL)" % hh)
    if wire_dir:
        for f in sorted(globmod.glob(os.path.join(wire_dir,
                                                  "req-*.json"))):
            try:
                with open(f) as fh:
                    d = json.load(fh)
            except Exception:
                continue
            body = d.get("body") if isinstance(d, dict) else None
            if isinstance(body, dict) and body.get("store") is True:
                anoms.append("store=true on %s (the store=false "
                             "invariant for proxy runs)"
                             % os.path.basename(f))
    if anoms:
        row["anomalies"] = anoms
    # apex-ayl.22 D-15: the failure triage hook (grok-session-triage)
    # on every FAIL/BLOCKED non-recon row.
    if status in ("FAIL", "BLOCKED") and not case.get("recon"):
        row["triage"] = _triage_block(case, row, ctx, run_dir, home)
    # apex-ayl.22 D-8/D-14(b): the per-cell machine result (atomic).
    if status != "SKIP":
        write_verdict_json(run_dir, case, row, ctx, results,
                           getattr(args, "campaign_id", None))
    log("  %s: %s in %.1fs calls=%d" % (case["id"], status_out,
                                        row["duration_s"],
                                        ctx.model_calls))
    return row


def _triage_block(case, row, ctx, run_dir, home):
    """apex-ayl.22 D-15: the failure triage hook (grok-session-triage
    contract). Every FAIL/BLOCKED (non-recon) row carries this block
    in the report AND in verdict.json. The hunt protocol runs on
    HERMETIC evidence only (the copied <run>/<case>/{wire,session}
    layout; ~/.grok is never named — step 6 of the triage loop is
    trivially satisfied by the copy). classification_hint: env (the
    last response status is in retry.on_status, or the run was
    BLOCKED with no resolvable evidence / a harness rig failure) ->
    one D-5 retry; case-bug (a pin self-contradiction, the B-i
    static-nth-at-N=1 class: 'only N wire files match') -> fix the
    case and re-run; temperament (VACUOUS) -> record and move on;
    else product -> FAIL + escalate."""
    session_dir = os.path.join(run_dir, "session")
    wire_dir = os.path.join(run_dir, "wire")
    acp_log = os.path.join(run_dir, "acp.log")
    unified_log = os.path.join(session_dir, "unified.jsonl")
    model = case.get("model")
    cwd = case.get("cwd") or getattr(home, "cwd", None)
    v = ctx.verdict or {}
    on_status = (case.get("retry") or {}).get("on_status") or []
    last = _last_response_status(wire_dir, model)
    reason = v.get("blocked_reason") or ""
    if reason.startswith(("unrouted:", "ambiguous-pin-route:",
                          "ambiguous-route:")):
        hint = "route-map"
    elif v.get("no_evidence") or v.get("blocked_reason"):
        hint = "env"
    elif last is not None and last[0] in on_status:
        hint = "env"
    elif any(not a.get("ok") and re.search(
            r"only \d+ wire files match", a.get("detail", ""))
            for a in row.get("asserts", [])):
        hint = "case-bug"
    elif row["status"] == "VACUOUS":
        hint = "temperament"
    else:
        hint = "product"
    ready = [
        "ls -la %s" % wire_dir,
        "grep -l '%s' %s/req-*.json 2>/dev/null || true"
        % (model or "*", wire_dir),
        "tail -n 50 %s" % unified_log,
        "grep -inE 'error|timeout|refused' %s | tail -n 20" % acp_log,
    ]
    if last is not None:
        ready.append("tail -n 5 %s/resp-%03d.jsonl" % (wire_dir,
                                                       last[0]))
    else:
        ready.append("ls %s/resp-*.jsonl 2>/dev/null || true" % wire_dir)
    # R-12: `credentials-403` covered both "our harness auth is broken" and
    # "right key, wrong origin". They are decided apart mechanically — see
    # auth_failure_class — and the key is always present so a reader cannot
    # mistake an absent class for an upstream verdict.
    auth_class = None
    if reason.startswith("credential-unset") or _auth_status_seen(wire_dir):
        auth_class = auth_failure_class(wire_dir)
    return {"session_id": ctx.session_id,
            "session_dir": session_dir,
            "cwd": cwd,
            "model": model,
            "wire": case.get("wire"),
            "acp_log": acp_log,
            "wire_dir": wire_dir,
            "unified_log": unified_log,
            "classification_hint": hint,
            "auth_class": auth_class,
            "ready_commands": ready}


def write_verdict_json(run_dir, case, row, ctx, results, campaign_id=None):
    """apex-ayl.22 D-8/D-14(b): the per-cell machine result (the
    runbook's result schema) — <run>/<case>/verdict.json, written
    atomically (verdict.json.tmp + os.replace): a partial file is a
    malformed result the campaign validator must reject (T-A1).
    `outcome` maps to the runbook's outcome enum (FAIL -> FAIL,
    BLOCKED -> BLOCKED, everything else -> PASS; the nuance stays in
    `status` + `verdict`)."""
    v = ctx.verdict or {}
    status = row["status"]
    # apex-ayl.91 N1: SKIP persists as outcome SKIP (previously PASS,
    # which the validator's is_pass check then rejected as "incomplete
    # evidence" for the zero-evidence skip row).
    if status == "FAIL":
        outcome = "FAIL"
    elif status == "BLOCKED":
        outcome = "BLOCKED"
    elif status == "SKIP":
        outcome = "SKIP"
    else:
        outcome = "PASS"
    doc = {
        "schema_version": 1,
        "campaign_id": campaign_id,
        "cell_id": row["id"],
        "case_id": case.get("id"),
        "attempt": row.get("attempt", 1),
        "status": status,
        "outcome": outcome,
        "stability": row.get("stability"),
        "model": case.get("model"),
        "wire": case.get("wire"),
        "session_id": row.get("session_id"),
        "duration_s": row.get("duration_s"),
        "model_calls": row.get("model_calls"),
        "est_calls": case.get("est_calls"),
        "retry_count": getattr(ctx, "retry_count", 0),
        # apex-ayl.22 T-A1: a watchdog-killed cell derives TIMEOUT
        # from this in the campaign aggregator.
        "killed": any(t.get("killed") for t in row.get("turns", [])),
        "verdict": v,
        "evidence_index": [r.cite for r in results
                           if r.ok and not r.recon and r.cite],
        "triage": row.get("triage"),
        "ts": utc_ts(),
    }
    tmp = os.path.join(run_dir, "verdict.json.tmp")
    with open(tmp, "w") as fh:
        json.dump(doc, fh, indent=2, default=str)
        fh.write("\n")
    os.replace(tmp, os.path.join(run_dir, "verdict.json"))


# ---------------------------------------------------------------------------
# Redaction sweep
# ---------------------------------------------------------------------------

KEY_HEURISTIC = re.compile(
    r'(?i)((?<![A-Za-z0-9])sk-[A-Za-z0-9_-]{16,}|AKIA[A-Z0-9]{16}|'
    r'(?:api[_-]?key|token)\s*[:=]\s*"[A-Za-z0-9._-]{24,}"'
    r'|user=[A-Za-z0-9]+:[A-Za-z0-9+/=]{20,}@)')


def unredacted_mcp_env_values(text):
    """Yield (line_no, env_name) for every env value in a servers_updated
    notification that is not the redaction marker — the sweep-side
    backstop for the write-time scrub (apex-ayl.70 R2-RED)."""
    out = []
    for i, line in enumerate(text.splitlines(), 1):
        body = line[6:] if line.startswith("SEND: ") else line
        if SERVERS_UPDATED_METHOD not in body:
            continue
        try:
            obj = json.loads(body)
        except Exception:
            continue
        servers = (obj.get("params") or {}).get("mcpServers")
        if not isinstance(servers, list):
            continue
        for server in servers:
            env = server.get("env") if isinstance(server, dict) else None
            if not isinstance(env, list):
                continue
            for entry in env:
                if (isinstance(entry, dict)
                        and entry.get("value") not in
                        (None,) + REDACTED_MARKERS):
                    out.append((i, entry.get("name", "?")))
    return out


# r3 §4.4.1's bare-token shapes. The proxy's echo carries NO scheme word, so
# r1's `Basic\s+…` heuristic could not match either leak class; the shapes
# below are fixed and bounded ("no length-or-charset guessing beyond rule 2",
# §4.4.1) so no legitimate payload is mangled.
JWT_SHAPE = re.compile(r"eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}"
                       r"\.[A-Za-z0-9_-]{10,}")
# spec r2 §2.3 (apex-au7k): `sk` must follow a non-alphanumeric character or
# a JSON escape (a backslash plus n, t or r) and must not continue a base64url
# run (a `-` or `_` after a base64url character). Each lookbehind is
# fixed-width, as `re` requires.
SK_SHAPE = re.compile(r"(?:(?<![A-Za-z0-9])|(?<=\\[nrt]))"
                      r"(?<![A-Za-z0-9_-][-_])sk[_-][A-Za-z0-9_.\-]{16,}",
                      re.IGNORECASE)
AUTH_SCHEME_BLOB = re.compile(
    r"(?:Bearer|Basic)\s+([A-Za-z0-9_\-+/]{40,}={0,2})")
ERROR_FIELD_BLOB = re.compile(
    r'"(?:param|message)"\s*:\s*"([A-Za-z0-9_\-+/]{40,}={0,2})"')


def _sweep_line(text, offset):
    return text.count("\n", 0, offset) + 1


def _sweep_hit(root, path, lineno, pattern, fragment):
    """§4.4.2: a detail is `path:line: <pattern> <sha256_12>` and NOTHING
    else. The frozen code appended `key-heuristic %r % frag[:40]`, writing up
    to 40 characters of a live key into report.json — the very artifact the
    sweep exists to protect — and write_report copies details into it."""
    return "%s:%d: %s %s" % (os.path.relpath(path, root), lineno, pattern,
                             sha256_12(fragment))


def credential_suffixes(values):
    """spec r2 §2.4 (apex-1s3b): the binary logs the last 12 characters of a
    key as `key_suffix` (`api_key_probe.rs:20-25@8072d8ed`). Returns one
    `(suffix, value)` pair per distinct suffix of a value longer than 12
    characters, keeping the first owner; callers pass the scheme-stripped
    swept values in LITERAL_SWEEP_NAMES order. A shorter value adds nothing,
    because its full-value match covers it. The value is kept so a detail
    digests the FULL value: a suffix's sha256_12 would be brute-forceable."""
    pairs = []
    for value in values:
        suffix = value[-12:]
        if len(value) > 12 and all(suffix != s for s, _v in pairs):
            pairs.append((suffix, value))
    return pairs


def _sweep_texts(root, ambient_key, files=None, texts=None):
    """The shared §4.4.1 pass over (relpath, path, text) triples. Returns
    (hits, details)."""
    hits = 0
    details = []
    values = [strip_credential_scheme(v) for v in literal_sweep_values()]
    values = [v for v in values if v]
    suffixes = credential_suffixes(values)
    for rel, path, text in texts:
        if ambient_key:
            for m in re.finditer(re.escape(ambient_key), text):
                hits += 1
                details.append(_sweep_hit(root, path,
                                          _sweep_line(text, m.start()),
                                          "ambient-key", ambient_key))
        for value in values:
            for m in re.finditer(re.escape(value), text):
                hits += 1
                details.append(_sweep_hit(root, path,
                                          _sweep_line(text, m.start()),
                                          "credential-literal", value))
        # spec r2 §2.4: a suffix inside any full value's span belongs to that
        # full-value leak, which the leg above has already counted.
        if suffixes:
            spans = [m.span() for value in values
                     for m in re.finditer(re.escape(value), text)]
            for suffix, full in suffixes:
                for m in re.finditer(re.escape(suffix), text):
                    if any(a <= m.start() and m.end() <= b
                           for a, b in spans):
                        continue
                    hits += 1
                    details.append(_sweep_hit(root, path,
                                              _sweep_line(text, m.start()),
                                              "credential-suffix", full))
        for pattern, rx, group in (("jwt-shape", JWT_SHAPE, 0),
                                   ("sk-shape", SK_SHAPE, 0),
                                   ("base64-blob-shape", AUTH_SCHEME_BLOB,
                                    1),
                                   ("error-field-blob-shape",
                                    ERROR_FIELD_BLOB, 1),
                                   ("key-heuristic", KEY_HEURISTIC, 0)):
            for m in rx.finditer(text):
                frag = m.group(group) if group else m.group(0)
                if ambient_key and frag in ambient_key:
                    continue
                hits += 1
                details.append(_sweep_hit(root, path,
                                          _sweep_line(text, m.start()),
                                          pattern, frag))
        for i, name in unredacted_mcp_env_values(text):
            hits += 1
            details.append("%s:%d: mcp env value NOT redacted in "
                           "servers_updated (name=%s)"
                           % (rel, i, name))
    return hits, details


def redaction_sweep(root: str, ambient_key: str, files=None):
    """The backstop layer of §4.4 over the run dir (or the named files in
    it): the ambient key, the declared credential values on their STRIPPED
    form, and the bare-token shapes — case-insensitively, because `Sk-` and
    `SK-` forms pass a case-sensitive sweep and git grep skips the captures
    that carry them (Agents.md §9). Returns (hits, details).

    The sweep is a BACKSTOP, not the only layer: the wiretap scrubs the same
    content at the response write path (§4.4.1), so a leak is caught before
    it lands and again before it is shared."""
    hits = 0
    details = []
    found = []
    for dirpath, dirnames, filenames in os.walk(root):
        for fn in filenames:
            if files is not None and fn not in files:
                continue
            p = os.path.join(dirpath, fn)
            try:
                with open(p, errors="replace") as fh:
                    text = fh.read()
            except Exception:
                continue
            found.append((os.path.relpath(p, root), p, text))
    return _sweep_texts(root, ambient_key, files=files, texts=found)


GREP = "/usr/bin/grep"
# Indirection so the §4.4.5 test can observe the argv this check builds; the
# rule it pins is that no credential value is ever an argv element (HYG-1).
GREP_RUNNER = subprocess.run


def literal_check(root: str, env=None):
    """The house literal-key check (Agents.md §9, r3 §4.4.5): grep the report
    dir for the declared credential VALUES.

    It belongs to the post-report sweep, never `--selftest` (§4.4.5):
    `--selftest` reads no key, no report dir and no binary by design, and
    this check needs the values. The patterns reach grep through a **file**
    (`grep -rlF -f`), never `-e`: argv is ps-visible, and 5 dead wiretap
    orphans leaked the ambient key that way (HYG-1). The patterns file is
    0600 and unlinked as soon as grep returns.

    An unset declared name contributes no pattern and is not an error —
    §4.3.3's states, not an abort. Returns (hits, details)."""
    values = literal_sweep_values(env)
    if not values:
        return 0, ["literal check: no declared credential value set "
                   "(skipped — nothing to match)"]
    # spec r2 §2.4 (RR-32 F-1): the suffix grep runs first, through its own
    # 0600 patterns file, so the full-value grep below stays the last
    # GREP_RUNNER call; the two results merge after both greps.
    suffixes = credential_suffixes(
        [strip_credential_scheme(value) for value in values])
    suffix_names, suffix_note = [], None
    if suffixes:
        sfd, spat = tempfile.mkstemp(prefix="rt-literal-suffix-",
                                     suffix=".patterns",
                                     dir=os.environ.get("TMPDIR") or None)
        try:
            os.fchmod(sfd, 0o600)
            with os.fdopen(sfd, "w") as fh:
                for suffix, _full in suffixes:
                    fh.write(suffix + "\n")
            sproc = GREP_RUNNER([GREP, "-rlF", "-f", spat, root],
                                capture_output=True, text=True)
        finally:
            try:
                os.unlink(spat)
            except OSError:
                pass
        if sproc.returncode == 0:
            suffix_names = [n for n in sproc.stdout.splitlines()
                            if n.strip()]
        elif sproc.returncode != 1:
            suffix_note = ("literal check: suffix grep rc %d (check did not "
                           "run)" % sproc.returncode)
    fd, pat = tempfile.mkstemp(prefix="rt-literal-", suffix=".patterns",
                               dir=os.environ.get("TMPDIR") or None)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w") as fh:
            for value in values:
                fh.write(value + "\n")
                stripped = strip_credential_scheme(value)
                if stripped != value:
                    fh.write(stripped + "\n")
        proc = GREP_RUNNER([GREP, "-rlF", "-f", pat, root],
                           capture_output=True, text=True)
    finally:
        try:
            os.unlink(pat)
        except OSError:
            pass
    details = []
    names = []
    if proc.returncode == 0:
        names = [n for n in proc.stdout.splitlines() if n.strip()]
    elif proc.returncode != 1:
        details.append("literal check: grep rc %d (check did not run)"
                       % proc.returncode)
    if names:
        digests = sorted(sha256_12(n) for n in names)
        details.append("literal check: %d file(s) match a credential value "
                       "(names sha256_12 %s)" % (len(names), digests))
    only = [n for n in suffix_names if n not in names]
    if only:
        digests = sorted(sha256_12(n) for n in only)
        details.append("literal check: %d file(s) match a credential suffix "
                       "(last 12 chars) (names sha256_12 %s)"
                       % (len(only), digests))
    if suffix_note:
        details.append(suffix_note)
    return len(names) + len(only), details


def run_verdict_code(bad=0, sweep_hits=0):
    """§4.4.3: the leak count joins the verdict. The frozen code returned
    `1 if bad else 0` over FAIL/BLOCKED rows only, so a run that published a
    live credential exited 0. The sweep may ADD a failure and never mask one
    that the rows already found."""
    return 1 if (bad or sweep_hits) else 0


def persist_run_verdict(out: str, rows, sweep_hits=0, code=0):
    """§4.4.3 / s0 F-8: `--daemon` self-daemonizes and the ORIGINAL process
    exits 0, so even a correct exit code never reaches the caller's `$?`. The
    verdict is therefore also written into the report dir, with a FIXED key
    set and no captured text, for a caller that cannot see `$?` to read a
    false green as one."""
    statuses = [r.get("status") for r in rows or []]
    doc = {"status": "PASS" if not code else "FAIL",
           "exit_code": code,
           "sweep_hits": sweep_hits,
           "rows_total": len(statuses),
           "rows_pass": sum(1 for s in statuses if s == "PASS"),
           "rows_fail": sum(1 for s in statuses if s == "FAIL"),
           "rows_blocked": sum(1 for s in statuses if s == "BLOCKED")}
    path = os.path.join(out, "run-verdict.json")
    tmp = path + ".tmp"
    with open(tmp, "w") as fh:
        json.dump(doc, fh, indent=2)
        fh.write("\n")
    os.replace(tmp, path)
    os.chmod(path, 0o600)
    return doc


# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------

def slice_evidence(row, limit=10, secrets=None):
    """For FAIL rows: the 10-line log/wire slice around the broken assert.

    §4.4.4: the frozen call site passed `secrets=(args.ambient_key,)`, so a
    TrueFoundry or on-prem credential inside a FAIL row's slice rode into
    report.md untouched — which is how a 403-heavy run turns its own report
    into a leak. The default is now the whole swept set; `redact` is a
    byte-exact replacement, so the excluded placeholder changes no real
    secret's coverage by being absent from it."""
    if secrets is None:
        secrets = tuple(literal_sweep_values())
    run_dir = row.get("run_dir")
    if not run_dir or not os.path.isdir(run_dir):
        return []
    lines = []
    for a in row.get("asserts", []):
        if a.get("ok") or a.get("recon"):
            continue
        ev = a.get("evidence", "")
        src = None
        if ev.startswith("wire/"):
            cand = os.path.join(run_dir, "wire", ev.split(" ")[0].split(":")[0])
            if os.path.exists(cand):
                src = (cand, 0)
        elif ev.startswith("artifacts/"):
            cand = os.path.join(run_dir, "session",
                                ev[len("artifacts/"):].split(":")[0])
            if os.path.exists(cand):
                src = (cand, 0)
        if src is None:
            cand = os.path.join(run_dir, "acp.log")
            if os.path.exists(cand):
                src = (cand, 0)
            else:
                cand = os.path.join(run_dir, "home")
                logs = []
                for dp, _, fns in os.walk(run_dir):
                    for fn in fns:
                        if fn.endswith((".err", ".log")):
                            logs.append(os.path.join(dp, fn))
                if logs:
                    src = (logs[-1], 0)
        if src is None:
            lines.append("  - FAIL [%s] %s (no slice)" % (a["kind"],
                                                          a["detail"]))
            continue
        p, off = src
        with open(p, errors="replace") as fh:
            all_lines = fh.read().splitlines()
        sl = all_lines[off:off + limit]
        lines.append("  - FAIL [%s] %s" % (a["kind"], a["detail"]))
        lines.append("    evidence: %s" % ev)
        lines.append("    slice (%s):" % os.path.relpath(p, run_dir))
        lines += ["      | " + redact(l[:300], secrets) for l in sl]
        break  # one slice per FAIL row is enough
    return lines


def write_report(out: str, rows, env_meta, args, sweep_hits, sweep_details):
    os.makedirs(out, exist_ok=True)

    def status_label(r):
        # expected_red annotation: documented reds render as
        # RED-EXPECTED/RED-CLEARED in the summary; the raw status
        # stays in the row (report.json) for machine consumers.
        exp = r.get("expected_red")
        if not exp:
            return r["status"]
        rid = exp.get("id", "?")
        if r["status"] == "FAIL":
            return "RED-EXPECTED (%s)" % rid
        if r["status"] == "PASS":
            return "RED-CLEARED (%s)" % rid
        return r["status"]

    # apex-ayl.22 D-3: the extended status order (VACUOUS +
    # FINDING-PASS join the report sort).
    order = STATUS_ORDER
    rows_sorted = sorted(rows, key=lambda r: (order.index(r["status"])
                                              if r["status"] in order
                                              else 99, r["id"]))
    counts = {}
    for r in rows_sorted:
        counts[r["status"]] = counts.get(r["status"], 0) + 1
    md = []
    md.append("# L3 red-team instrumented run report")
    md.append("")
    md.append("- ts: %s" % env_meta["ts"])
    md.append("- binary: %s (sha256_12 %s)" % (env_meta["bin"],
                                               env_meta["bin_sha"]))
    md.append("- git: %s" % env_meta["git"])
    md.append("- ambient key sha256_12: %s" % env_meta["key_sha"])
    md.append("- upstream: %s" % env_meta["upstream"])
    md.append("- live home: %s" % args.live_home)
    md.append("- wirecap default: %s, budget: %d" %
              ("on" if args.wirecap else "off", args.budget))
    md.append("- redaction sweep: %d hit(s)" % sweep_hits)
    md.append("- hermetic invariant (OQ-7): --no-watch appended to the "
              "copied [mcp_servers.codegraph] args — applied AFTER the "
              "case patch, not removable, disclosed here "
              "(apex-ayl.22 D-10)")
    md.append("")
    md.append("**Summary:** %s" % ", ".join(
        "%s=%d" % (k, v) for k, v in sorted(counts.items())))
    md.append("")
    md.append("| id | status | dur(s) | calls | title |")
    md.append("|---|---|---|---|---|")
    for r in rows_sorted:
        md.append("| %s | %s | %s | %s | %s |" % (
            r["id"], status_label(r), r["duration_s"], r["model_calls"],
            r.get("title", "")[:60]))
    md.append("")
    for r in rows_sorted:
        md.append("## %s — %s" % (r["id"], status_label(r)))
        if r.get("skipped"):
            md.append("- skipped: %s" % r["skipped"])
            md.append("")
            continue
        md.append("- duration: %ss, model calls: %d, session: %s" % (
            r["duration_s"], r["model_calls"], r.get("session_id") or "-"))
        for t in r.get("turns", []):
            bits = ["%s(step=%s)" % (t.get("op"), t.get("step", "-"))]
            if t.get("model"):
                bits.append("model=%s" % t["model"])
            if "exit" in t:
                bits.append("exit=%s" % t["exit"])
            if t.get("stop"):
                bits.append("stop=%s" % t["stop"])
            if t.get("killed"):
                bits.append("KILLED")
            if t.get("elapsed") is not None:
                bits.append("%.1fs" % t["elapsed"])
            if t.get("error"):
                bits.append("ERROR %s" % str(t["error"])[:120])
            if t.get("text"):
                bits.append("text=%r" % t["text"][:80])
            md.append("- " + " ".join(bits))
            if "interjects" in t:
                # apex-bdfz BZ-R7 F-2 (plan r3 section 2.8): one line per
                # armed turn naming each arm's send and status, so the arms of
                # a VACUOUS or BLOCKED row read here too; report.json keeps
                # the whole records.
                arms = []
                for a in t["interjects"]:
                    send = a.get("send")
                    if send == "set_model":
                        send = "set_model %s" % a.get("model")
                    status = a.get("status")
                    if a.get("disarm_reason"):
                        status = "%s(%s)" % (status, a["disarm_reason"])
                    arms.append("%s %s %s%s" % (
                        a.get("arm_id"), send, status,
                        " mid_turn" if a.get("mid_turn") else ""))
                md.append("  - interject arms: %s"
                          % ("; ".join(arms) or "none recorded"))
        for s in r.get("snapshots", []):
            md.append("- snapshot %s: lines=%s bytes=%s" % (
                s.get("op"), s.get("chat_history_lines", "-"),
                s.get("bytes", "-")))
        for a in r.get("asserts", []):
            flag = "recon" if a.get("recon") else ("ok" if a.get("ok")
                                                   else "FAIL")
            md.append("- [%s] %s: %s" % (flag, a["kind"], a["detail"]))
            if not a.get("ok") or a.get("recon"):
                md.append("  - evidence: %s" % a["evidence"])
        tri = r.get("triage")
        if tri:
            md.append("- triage: hint=%s session_dir=%s" % (
                tri.get("classification_hint") or "-",
                tri.get("session_dir") or "-"))
            for cmd in tri.get("ready_commands", []):
                md.append("  - cmd: %s" % cmd)
        for anom in r.get("anomalies") or []:
            md.append("- anomaly: %s" % anom)
        if r["status"] == "FAIL":
            md.extend(slice_evidence(
                r, secrets=(args.ambient_key,) + tuple(literal_sweep_values())))
        md.append("")
    if sweep_details:
        md.append("## Redaction sweep details")
        md.extend("- " + d for d in sweep_details)
        md.append("")
    with open(os.path.join(out, "report.md"), "w") as fh:
        fh.write("\n".join(md))
    with open(os.path.join(out, "report.json"), "w") as fh:
        json.dump({"env": env_meta,
                   "rows": rows_sorted,
                   # apex-ayl.22 D-3: the caps block — the runner
                   # REPORTS the caps; the coordinator ENFORCES them
                   # at the gate (Q5).
                   "caps_block": {"blocked_count":
                                  counts.get("BLOCKED", 0),
                                  "vacuous_count":
                                  counts.get("VACUOUS", 0),
                                  "finding_count":
                                  counts.get("FINDING-PASS", 0),
                                  "caps": CAPS},
                   "redaction_hits": sweep_hits}, fh, indent=2,
                  default=str)
    return os.path.join(out, "report.md")


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def load_cases(args):
    cases = []
    for f in sorted(globmod.glob(os.path.join(CASES_DIR, "*.json"))):
        with open(f) as fh:
            c = json.load(fh)
        cases.append(c)
    if args.case_ids:
        # F-ORD (Wave-S campaign 20260917T102912Z): preserve the positional
        # roster order — the set filter discarded it, so the glob-sorted
        # order ran instead of the binding roster. Unknown ids skip.
        want = [x.upper() for x in args.case_ids]
        first_by_id = {}
        for c in cases:
            first_by_id.setdefault(c.get("id", "").upper(), c)
        cases = [first_by_id[cid] for cid in want if cid in first_by_id]
    return cases


# ---------------------------------------------------------------------------
# Case-file contract (offline validation — no live run required).
#
# The case set is the campaign's acceptance pin surface: a contract
# drift here silently breaks runs later. validate_case_file() encodes
# exactly what run_case() and the check_* functions consume, so the
# whole set can be validated OFFLINE (smoke/redteam/test_run.py
# CaseContractTest; also `python3 smoke/redteam/run.py --selftest`).
# ---------------------------------------------------------------------------

STEP_OPS = ("turn", "switch", "switch_model", "kill", "compact", "idle",
            "recon_note")
NDJSON_OPS = ("count", "absent", "present", "eq", "ne", "text_contains",
              "tools_absent", "tools_present", "recon")
WIRE_KINDS = ("field", "grep", "size_lt", "count", "resp_status", "recon",
              "golden", "resp_body", "resp_stream", "reasoning_replay")
# apex-2o6: the step ops that fire a LIVE model call — a proxy:none
# (L1 key-free) case may carry none of these (validate_case_file
# gate + _run_case_once runtime backstop).
LIVE_MODEL_OPS = ("turn", "switch", "switch_model", "kill", "compact",
                  "mcp_call", "subagent")
ARTIFACT_OPS = ("count", "grep", "recon")


def _validate_where(ctx, where, errs, ndjson_op=None):
    # apex-bdfz BZ-R7 F-5(a): check_ndjson applies an ndjson where on count, eq
    # and ne only, so on any other ndjson op it is refused rather than silently
    # ignored. Wire and artifact asserts pass no op.
    if ndjson_op is not None and ndjson_op not in ("count", "eq", "ne"):
        errs.append("%s: where filters only ndjson ops count, eq and ne "
                    "(op %r would ignore it)" % (ctx, ndjson_op))
    if not isinstance(where, dict):
        errs.append("%s: 'where' must be an object" % ctx)
        return
    for k, v in where.items():
        if not isinstance(k, str) or not k:
            errs.append("%s: where key %r must be a non-empty path" % (ctx, k))
            continue
        if isinstance(v, dict):
            # _wire_filter: prefix / not_prefix forms (COMP-3).
            subs = set(v)
            if not (subs & {"prefix", "not_prefix"}):
                errs.append("%s: where %r: dict form needs 'prefix' or "
                            "'not_prefix'" % (ctx, k))
            if subs - {"prefix", "not_prefix"}:
                errs.append("%s: where %r: unsupported sub-key(s) %s"
                            % (ctx, k, sorted(subs - {"prefix",
                                                      "not_prefix"})))
            for sub in subs:
                if not isinstance(v[sub], str):
                    errs.append("%s: where %r.%s must be a string"
                                % (ctx, k, sub))
        elif isinstance(v, (list, dict)):
            errs.append("%s: where %r: value must be scalar or prefix "
                        "form" % (ctx, k))


def _validate_step(ctx, st, errs):
    if not isinstance(st, dict):
        errs.append("%s: step must be an object" % ctx)
        return
    op = st.get("op")
    if op not in STEP_OPS:
        errs.append("%s: unknown op %r (known: %s)" % (ctx, op,
                                                       ", ".join(STEP_OPS)))
        return
    if op == "turn" and not isinstance(st.get("prompt"), str):
        errs.append("%s: turn needs a string 'prompt'" % ctx)
    if op == "switch":
        if not isinstance(st.get("model"), str):
            errs.append("%s: switch needs a string 'model'" % ctx)
        if st.get("via", "acp") not in ("acp", "resume"):
            errs.append("%s: switch 'via' must be acp|resume" % ctx)
    if op == "switch_model":
        if not isinstance(st.get("model"), str):
            errs.append("%s: switch_model needs a string 'model'" % ctx)
        if st.get("via", "acp") not in ("acp", "resume"):
            errs.append("%s: switch_model 'via' must be acp|resume" % ctx)
        if not isinstance(st.get("cell"), str) or not st.get("cell"):
            errs.append("%s: switch_model needs a string 'cell' "
                        "(smoke/xwfix/cells/<cell>)" % ctx)
        if st.get("assert_form", "storage") not in ("storage", "wire"):
            errs.append("%s: switch_model 'assert_form' must be "
                        "storage|wire" % ctx)
    if op == "idle":
        if not (isinstance(st.get("s"), int) and st["s"] > 0):
            errs.append("%s: idle needs a positive int 's'" % ctx)
    if op in ("turn", "compact") and "timeout_s" in st \
            and not isinstance(st["timeout_s"], (int, float)):
        errs.append("%s: timeout_s must be a number" % ctx)
    # apex-bdfz BZ-6 (plan r3 section 2.7, R-7): the step-local half of the
    # interject mirror; the driver and echo rules need the whole case and
    # live in _validate_new_case. The schema subset has no oneOf, so
    # "exactly one trigger key" is enforced here, not by the schema.
    if "interject" in st:
        if op != "turn":
            errs.append("%s: interject is only legal on op turn (got %r)"
                        % (ctx, op))
        if "kill_after_s" in st or "expect_kill" in st:
            errs.append("%s: interject cannot ride a step carrying "
                        "kill_after_s or expect_kill" % ctx)
        arms = st["interject"]
        if not (isinstance(arms, list) and arms):
            errs.append("%s: interject must be a non-empty list of arms"
                        % ctx)
            arms = []
        for j, arm in enumerate(arms):
            actx = "%s interject[%d]" % (ctx, j)
            if not isinstance(arm, dict):
                errs.append("%s: arm must be an object" % actx)
                continue
            at = arm.get("at")
            if not (isinstance(at, dict) and len(at) == 1 and set(at)
                    <= {"after_tool_calls", "after_tool", "after_s"}):
                errs.append("%s: at needs exactly one trigger key "
                            "(after_tool_calls | after_tool | after_s), "
                            "got %r" % (actx, sorted(at)
                                        if isinstance(at, dict) else at))
            send = arm.get("send", "interject")
            # apex-bdfz BZ-R7 F-6: the step-local half of the schema's send
            # enum, so a legacy-form case (which never reaches the schema gate)
            # cannot carry a send the pump would send as x.ai/interject.
            if send not in ("interject", "set_model"):
                errs.append("%s: send must be interject or set_model (got "
                            "%r)" % (actx, send))
            text = arm.get("text")
            if send == "interject" and not (
                    isinstance(text, str) and text and "model" not in arm):
                errs.append("%s: send interject needs a non-empty string "
                            "'text' and no 'model'" % actx)
            if send == "set_model" and not (
                    isinstance(arm.get("model"), str) and "text" not in arm):
                errs.append("%s: send set_model needs a string 'model' "
                            "and no 'text'" % actx)
            if isinstance(text, str):
                if text == st.get("prompt"):
                    errs.append("%s: text is a copy of the step prompt (a "
                                "copy is not an interjection)" % actx)
                # surrogatepass: a lone surrogate from a JSON escape counts
                # instead of raising.
                size = len(text.encode("utf-8", "surrogatepass"))
                if size > 4096:
                    errs.append("%s: text is %d bytes, over the 4096-byte "
                                "bound (the product truncates a long "
                                "interjection silently)" % (actx, size))


# apex-ayl.22 D-1/D-2 — draft-07 dual-path engine + NEW-case gate.
#
# Dual path (OQ-1): jsonschema is a guarded opportunistic import (NOT a
# dependency — L18 discipline); when it is importable the gate uses it,
# else the hand-rolled keyword-subset engine below. The subset is the
# schema's own documented keyword set (type/properties/required/items/
# enum/pattern/minimum/maximum/additionalProperties/anyOf/description,
# local #/definitions $ref) plus minItems (used on `rows`). Any
# out-of-subset keyword encountered in the schema fails closed (the
# engine reports it instead of silently passing).

_JSONSCHEMA = {"mod": None, "tried": False}


def _jsonschema_engine():
    if not _JSONSCHEMA["tried"]:
        _JSONSCHEMA["tried"] = True
        try:
            import jsonschema
            _JSONSCHEMA["mod"] = jsonschema
        except Exception:
            _JSONSCHEMA["mod"] = None
    return _JSONSCHEMA["mod"]


_D7_HAND_KEYWORDS = {
    "$schema", "$id", "title", "description", "default", "type",
    "properties", "required", "items", "enum", "pattern", "minimum",
    "maximum", "additionalProperties", "anyOf", "minItems", "$ref",
    "definitions",
}


def _d7_hand_type_ok(inst, t):
    if t == "null":
        return inst is None
    if t == "boolean":
        return isinstance(inst, bool)
    if t == "integer":
        return isinstance(inst, int) and not isinstance(inst, bool)
    if t == "number":
        return isinstance(inst, (int, float)) and not isinstance(inst, bool)
    if t == "string":
        return isinstance(inst, str)
    if t == "array":
        return isinstance(inst, list)
    if t == "object":
        return isinstance(inst, dict)
    return False


def _d7_hand(inst, schema, root, path, errs):
    if not isinstance(schema, dict):
        return
    for kw in schema:
        if kw not in _D7_HAND_KEYWORDS:
            errs.append("%s: schema keyword %r not in the hand-rolled "
                        "subset — refusing to validate (fail-closed)"
                        % (path, kw))
            return
    ref = schema.get("$ref")
    if ref:
        if not ref.startswith("#/definitions/"):
            errs.append("%s: unsupported $ref %r (local #/definitions "
                        "only)" % (path, ref))
            return
        name = ref[len("#/definitions/"):]
        sub = (root.get("definitions") or {}).get(name)
        if sub is None:
            errs.append("%s: $ref %r does not resolve" % (path, ref))
            return
        _d7_hand(inst, sub, root, path, errs)
        return
    t = schema.get("type")
    if t is not None:
        types = t if isinstance(t, list) else [t]
        if not any(_d7_hand_type_ok(inst, x) for x in types):
            errs.append("%s: value %r has type %s, expected %r"
                        % (path, str(inst)[:60],
                           type(inst).__name__, t))
            return
    if "enum" in schema and inst not in schema["enum"]:
        errs.append("%s: value %r not in enum %s"
                    % (path, str(inst)[:60], schema["enum"]))
    if isinstance(inst, str):
        pat = schema.get("pattern")
        if pat is not None and re.search(pat, inst) is None:
            errs.append("%s: %r does not match pattern %r"
                        % (path, inst[:60], pat))
    if isinstance(inst, (int, float)) and not isinstance(inst, bool):
        if "minimum" in schema and inst < schema["minimum"]:
            errs.append("%s: %r < minimum %r" % (path, inst,
                                                 schema["minimum"]))
        if "maximum" in schema and inst > schema["maximum"]:
            errs.append("%s: %r > maximum %r" % (path, inst,
                                                 schema["maximum"]))
    if isinstance(inst, list):
        if "minItems" in schema and len(inst) < schema["minItems"]:
            errs.append("%s: %d items < minItems %d"
                        % (path, len(inst), schema["minItems"]))
        items = schema.get("items")
        if items is not None:
            for i, it in enumerate(inst):
                _d7_hand(it, items, root, "%s[%d]" % (path, i), errs)
    if isinstance(inst, dict):
        props = schema.get("properties") or {}
        for req in schema.get("required") or []:
            if req not in inst:
                errs.append("%s: missing required property %r"
                            % (path, req))
        for k, sub in props.items():
            if k in inst:
                _d7_hand(inst[k], sub, root, "%s.%s" % (path, k), errs)
        ap = schema.get("additionalProperties")
        if ap is False:
            extra = [k for k in inst if k not in props]
            if extra:
                errs.append("%s: additionalProperties is false; "
                            "unexpected %s" % (path, sorted(extra)))
        elif isinstance(ap, dict):
            for k in inst:
                if k not in props:
                    _d7_hand(inst[k], ap, root, "%s.%s" % (path, k), errs)
    if "anyOf" in schema:
        if not any(_d7_hand_ok(inst, sub, root)
                   for sub in schema["anyOf"]):
            errs.append("%s: value does not match anyOf" % path)


def _d7_hand_ok(inst, schema, root):
    errs = []
    _d7_hand(inst, schema, root, "", errs)
    return not errs


def _d7_hand_errors(inst, root):
    errs = []
    _d7_hand(inst, root, root, "", errs)
    return errs


def _d7_js_errors(inst, schema):
    js = _jsonschema_engine()
    if js is None:
        raise RuntimeError(
            "jsonschema engine requested but not importable in this "
            "environment")
    v = js.Draft7Validator(schema)
    return ["%s: %s" % (list(e.absolute_path), e.message)
            for e in sorted(v.iter_errors(inst), key=str)]


def draft07_gate(case, schema, engine=None):
    """D-1 draft-07 gate over one parsed case. `engine`: "jsonschema" |
    "hand" | None (opportunistic). Returns violation strings."""
    if engine == "jsonschema":
        return _d7_js_errors(case, schema)
    if engine == "hand":
        return _d7_hand_errors(case, schema)
    if _jsonschema_engine() is not None:
        return _d7_js_errors(case, schema)
    return _d7_hand_errors(case, schema)


def validator_engine_used(engine=None):
    if engine == "jsonschema":
        return "jsonschema"
    if engine == "hand":
        return "hand-rolled-subset"
    return ("jsonschema" if _jsonschema_engine() is not None
            else "hand-rolled-subset")


_SCHEMA_CACHE = {"path": None, "schema": None, "tried": False}


def _load_schema(path=None):
    """Load (and cache) the frozen case schema. None when absent."""
    path = path or SCHEMA_PATH
    if _SCHEMA_CACHE["tried"] and _SCHEMA_CACHE["path"] == path:
        return _SCHEMA_CACHE["schema"]
    schema = None
    try:
        with open(path) as fh:
            schema = json.load(fh)
    except Exception:
        schema = None
    _SCHEMA_CACHE.update({"path": path, "schema": schema, "tried": True})
    return schema


def check_schema(cases_dir=None, schema_path=None, engine=None):
    """D-1 offline schema gate (the --selftest `check_schema` step).

    Every NEW (schema_version) case in the set must pass draft-07
    (0 errors) on the selected engine; legacy files are exempt (D-2
    back-compat split) and still ride the in-tree contract via
    validate_cases_dir. Returns ({basename: [errors]}, engine_used,
    legacy_count)."""
    cases_dir = cases_dir or CASES_DIR
    schema = _load_schema(schema_path)
    out = {}
    legacy = 0
    if schema is None:
        return ({}, validator_engine_used(engine), legacy)
    for path in sorted(globmod.glob(os.path.join(cases_dir, "*.json"))):
        try:
            with open(path) as fh:
                case = json.load(fh)
        except Exception:
            continue
        if not isinstance(case, dict):
            continue
        if case.get("schema_version") is None:
            legacy += 1
            continue
        errs = draft07_gate(case, schema, engine)
        if errs:
            out[os.path.basename(path)] = errs
    return out, validator_engine_used(engine), legacy


def _validate_drift(name, case, errs):
    """apex-ayl.22 D-4c (T-S3): prompt<->declaration drift. For a
    2-step (or more) case with declarations, the LAST turn prompt must
    carry each mcp_call expect_name (default server__tool), each
    declared mcp_call/tool_call arg value (str(v)), and the
    id:"done" text_contains pin value. A mismatch is a case-contract
    error (fails at validation, not at the wire); the runner never
    rewrites prompts."""
    steps = [s for s in (case.get("steps") or [])
             if isinstance(s, dict)]
    turns = [s for s in steps if s.get("op") == "turn"]
    mcp = [d for d in (case.get("mcp_calls") or [])
           if isinstance(d, dict)]
    tools = [d for d in (case.get("tool_calls") or [])
             if isinstance(d, dict)]
    done = None
    for a in (case.get("assert") or {}).get("ndjson") or []:
        if isinstance(a, dict) and a.get("id") == "done" \
                and a.get("op") == "text_contains":
            done = a.get("value")
    if len(turns) < 2 or not (mcp or tools or done):
        return
    prompt = turns[-1].get("prompt") or ""
    for decl in mcp:
        fq = decl.get("expect_name") or \
            "%s__%s" % (decl.get("server", ""), decl.get("tool", ""))
        if fq and fq not in prompt:
            errs.append("%s: prompt drift — the last turn prompt is "
                        "missing the declared MCP FQ name %r "
                        "(case-contract error)" % (name, fq))
        for k, v in (decl.get("args") or {}).items():
            sv = str(v)
            if sv and sv not in prompt:
                errs.append("%s: prompt drift — the last turn prompt "
                            "is missing the mcp_call arg %s=%r "
                            "(case-contract error)" % (name, k, v))
    for decl in tools:
        for k, v in (decl.get("args") or {}).items():
            sv = str(v)
            if sv and sv not in prompt:
                errs.append("%s: prompt drift — the last turn prompt "
                            "is missing the tool_call arg %s=%r "
                            "(case-contract error)" % (name, k, v))
    if isinstance(done, str) and done and done not in prompt:
        errs.append("%s: prompt drift — the last turn prompt is "
                    "missing the 'done' reply line %r "
                    "(case-contract error)" % (name, done))


def _validate_new_case(name, case, engine=None):
    """apex-ayl.22 D-2 — the NEW-case gate (schema_version=1 files only).

    Legacy cases (no schema_version, RT-* pre-version) never reach here:
    back-compat, zero new failures (D-2). NEW cases carry the full
    draft-07 gate (D-1, dual path) PLUS the campaign rules:
      - bead required (string);
      - suite mcp-call/mcp-tool => mcp_calls non-empty;
        suite tool-call/mcp-tool => tool_calls non-empty;
      - driver acp => model string + absolute cwd + scalar config_patch;
      - cwd, when present, absolute (G4);
      - every scored (non-recon) wire assert carries `where` (G10);
      - assert/call ids unique (the shared pin namespace);
      - every scoring.vacuous_if[].pin resolves to an assert or
        declared-call id; exactly one harness-class premise when
        mcp_calls is declared (D-4a);
      - env.provider_vars_unset is a superset of the runner L1 set
        (D-7; a subset declaration is rejected);
      - an interject block needs driver acp, and each send:interject text
        carries a scored text_contains nonce and asks for it back
        verbatim (apex-bdfz BZ-6, the echo rule F-3).
    """
    errs = []
    schema = _load_schema()
    if schema is None:
        errs.append("%s: case.schema.json missing or unparseable "
                    "(NEW-case gate requires the frozen schema)" % name)
    else:
        for e in draft07_gate(case, schema, engine):
            errs.append("schema: %s" % e)
    if not (isinstance(case.get("bead"), str) and case.get("bead")):
        errs.append("%s: NEW case needs a string 'bead'" % name)
    suite = case.get("suite")
    if suite in ("mcp-call", "mcp-tool") \
            and not (isinstance(case.get("mcp_calls"), list)
                     and case.get("mcp_calls")):
        errs.append("%s: suite %s needs non-empty 'mcp_calls'"
                    % (name, suite))
    if suite in ("tool-call", "mcp-tool") \
            and not (isinstance(case.get("tool_calls"), list)
                     and case.get("tool_calls")):
        errs.append("%s: suite %s needs non-empty 'tool_calls'"
                    % (name, suite))
    cwd = case.get("cwd")
    if cwd is not None and \
            (not isinstance(cwd, str) or not os.path.isabs(cwd)):
        errs.append("%s: NEW case 'cwd' must be an absolute path "
                    "(got %r)" % (name, cwd))
    if case.get("driver", "headless") == "acp":
        if cwd is None:
            errs.append("%s: acp NEW case needs an absolute 'cwd'" % name)
        cp = case.get("config_patch")
        if isinstance(cp, dict):
            for k, v in cp.items():
                if isinstance(v, (dict, list)):
                    errs.append(
                        "%s: acp NEW case config_patch %r must be scalar"
                        % (name, k))
    # G10: every scored (non-recon) wire assert carries `where`.
    for block_name, specs in (
            ("assert.wire",
             (case.get("assert") or {}).get("wire") or []),
            ("row_asserts.wire",
             (case.get("row_asserts") or {}).get("wire") or [])):
        if not isinstance(specs, list):
            continue
        for i, spec in enumerate(specs):
            if not isinstance(spec, dict):
                continue
            recon = (spec.get("kind") == "recon"
                     or spec.get("op") == "recon")
            where = spec.get("where")
            if not recon and not (isinstance(where, dict) and where):
                errs.append(
                    "%s %s[%d]: scored wire assert needs 'where' (G10)"
                    % (name, block_name, i))
    # The pin namespace: assert ids and declared-call ids are shared
    # (vacuous_if may name either) and must be unique.
    ids = []
    ab = case.get("assert") or {}
    for block in ("ndjson", "wire", "artifact"):
        for spec in ab.get(block) or []:
            if isinstance(spec, dict) and isinstance(spec.get("id"), str):
                ids.append(spec["id"])
    call_ids = []
    for decl in list(case.get("mcp_calls") or []) + \
            list(case.get("tool_calls") or []):
        if isinstance(decl, dict) and isinstance(decl.get("id"), str):
            call_ids.append(decl["id"])
    seen = set()
    for x in ids + call_ids:
        if x in seen:
            errs.append("%s: duplicate assert/call id %r (ids must be "
                        "unique)" % (name, x))
        seen.add(x)
    scoring = case.get("scoring") or {}
    vif = scoring.get("vacuous_if") or []
    known = set(ids) | set(call_ids)
    harness_count = 0
    if isinstance(vif, list):
        for j, entry in enumerate(vif):
            if not isinstance(entry, dict):
                errs.append("%s: scoring.vacuous_if[%d] must be an object"
                            % (name, j))
                continue
            pin = entry.get("pin")
            if not (isinstance(pin, str) and pin in known):
                errs.append(
                    "%s: scoring.vacuous_if[%d].pin %r does not resolve "
                    "to an assert or declared-call id" % (name, j, pin))
            if entry.get("class", "model") == "harness":
                harness_count += 1
    if case.get("mcp_calls"):
        if harness_count != 1:
            errs.append(
                "%s: mcp_calls case needs exactly one harness-class "
                "vacuous_if premise (got %d)" % (name, harness_count))
    elif harness_count:
        # Non-MCP rig premises must bind to scored wire evidence.
        scored_wire_ids = {
            spec["id"] for spec in ab.get("wire") or []
            if isinstance(spec, dict) and isinstance(spec.get("id"), str)
            and spec.get("kind") in WIRE_KINDS
            and spec.get("kind") != "recon" and spec.get("op") != "recon"}
        for j, entry in enumerate(vif):
            if not isinstance(entry, dict) or entry.get("class") != "harness":
                continue
            pin = entry.get("pin")
            if not (isinstance(pin, str) and pin in scored_wire_ids
                    and scoring.get("require_wire_evidence") is True):
                errs.append(
                    "%s: scoring.vacuous_if[%d] harness-class premise "
                    "without mcp_calls must name a scored wire assert "
                    "and require_wire_evidence=true" % (name, j))
    env = case.get("env") or {}
    pvs = env.get("provider_vars_unset")
    if pvs is not None:
        if not (isinstance(pvs, list)
                and all(isinstance(x, str) for x in pvs)):
            errs.append("%s: env.provider_vars_unset must be a list of "
                        "strings" % name)
        else:
            missing = [x for x in PROVIDER_VARS_UNSET if x not in pvs]
            if missing:
                errs.append(
                    "%s: env.provider_vars_unset must be a superset of "
                    "the runner L1 set; missing %s" % (name, missing))
    # apex-bdfz BZ-6 (plan r3 section 2.7, R-7): the case-level half of the
    # interject mirror. The text_contains pin reads only model-authored
    # chunks, so a nonce the model was never asked to repeat is never
    # produced (F-3); the check is greppable: the nonce as a substring plus
    # the word "verbatim".
    nd = ab.get("ndjson")
    nonces = [spec.get("value") for spec in (nd if isinstance(nd, list)
                                             else [])
              if isinstance(spec, dict) and spec.get("op") == "text_contains"
              and isinstance(spec.get("value"), str) and spec.get("value")]
    steps = case.get("steps")
    for i, st in enumerate(steps if isinstance(steps, list) else []):
        if not (isinstance(st, dict) and "interject" in st):
            continue
        if case.get("driver", "headless") != "acp":
            errs.append("%s steps[%d]: interject needs driver acp (headless "
                        "is one process per turn)" % (name, i))
        arms = st["interject"]
        for j, arm in enumerate(arms if isinstance(arms, list) else []):
            if not isinstance(arm, dict) \
                    or arm.get("send", "interject") != "interject":
                continue
            text = arm.get("text")
            if not (isinstance(text, str)
                    and any(n in text for n in nonces)
                    and re.search(r"\bverbatim\b", text, re.I)):
                errs.append(
                    "%s steps[%d].interject[%d]: echo rule (F-3): the text "
                    "must carry a nonce the case scores with text_contains "
                    "and ask for it back verbatim" % (name, i, j))
    # apex-ayl.22 D-4c (T-S3): the prompt<->declaration drift check
    # (last turn prompt must carry the declared names/args/done line).
    _validate_drift(name, case, errs)
    return errs


def validate_case_file(path, engine=None):
    """Offline contract check for one case file. Returns a list of
    violation strings (empty = valid).

    apex-ayl.22: `engine` forces the draft-07 path (D-1 dual path):
    "jsonschema" | "hand"; None = opportunistic (jsonschema when
    importable, else the hand-rolled subset). Applies only to NEW
    (schema_version) cases; legacy files validate against the in-tree
    contract only."""
    try:
        with open(path) as fh:
            case = json.load(fh)
    except Exception as e:
        return ["json: %s" % e]
    if not isinstance(case, dict):
        return ["case must be a JSON object"]
    errs = []
    name = os.path.basename(path)

    cid = case.get("id")
    if not isinstance(cid, str) or not cid:
        errs.append("%s: missing string case id" % name)

    driver = case.get("driver", "headless")
    if driver not in ("headless", "acp"):
        errs.append("%s: driver must be headless|acp (got %r)" % (name,
                                                                  driver))
    if driver == "acp" and not isinstance(case.get("model"), str):
        errs.append("%s: acp case needs a string 'model'" % name)

    if "disabled" in case and not isinstance(case["disabled"], bool):
        errs.append("%s: 'disabled' must be a bool" % name)
    if "watchdog_s" in case \
            and not isinstance(case["watchdog_s"], (int, float)):
        errs.append("%s: watchdog_s must be a number" % name)
    if "est_calls" in case and not isinstance(case["est_calls"], int):
        errs.append("%s: est_calls must be an int" % name)

    if "config_patch" in case:
        cp = case["config_patch"]
        if not isinstance(cp, dict):
            errs.append("%s: config_patch must be an object" % name)
        else:
            for k, v in cp.items():
                if not isinstance(k, str) or "/" not in k:
                    errs.append("%s: config_patch key %r must be dotted "
                                "(section/key)" % (name, k))
                if isinstance(v, (dict, list)):
                    errs.append("%s: config_patch %r value must be scalar"
                                % (name, k))

    has_steps = "steps" in case
    has_rows = "rows" in case
    if not has_steps and not has_rows:
        errs.append("%s: case needs 'steps' or 'rows'" % name)
    if has_steps:
        steps = case["steps"]
        if not isinstance(steps, list):
            errs.append("%s: steps must be a list" % name)
        else:
            for i, st in enumerate(steps):
                _validate_step("%s steps[%d]" % (name, i), st, errs)
    if has_rows:
        rows = case["rows"]
        if not isinstance(rows, list) or not rows:
            errs.append("%s: rows must be a non-empty list" % name)
        else:
            for i, r in enumerate(rows):
                if not isinstance(r, dict) \
                        or not isinstance(r.get("model"), str):
                    errs.append("%s rows[%d]: needs a string 'model'"
                                % (name, i))
        if "row_asserts" not in case:
            errs.append("%s: rows case needs 'row_asserts'" % name)
        elif not isinstance(case["row_asserts"], dict):
            errs.append("%s: row_asserts must be an object" % name)

    ab = case.get("assert", {})
    if not isinstance(ab, dict):
        errs.append("%s: assert must be an object" % name)
        return errs
    for k in ab:
        if k not in ("ndjson", "wire", "artifact", "exit"):
            errs.append("%s: assert: unknown block %r" % (name, k))
    if "exit" in ab and not isinstance(ab["exit"], int):
        errs.append("%s: assert.exit must be an int" % name)

    for block, allowed in (("ndjson", NDJSON_OPS),
                           ("wire", WIRE_KINDS),
                           ("artifact", ARTIFACT_OPS)):
        specs = ab.get(block, []) or []
        if not isinstance(specs, list):
            errs.append("%s: assert.%s must be a list" % (name, block))
            continue
        for i, spec in enumerate(specs):
            ctx = "%s assert.%s[%d]" % (name, block, i)
            if not isinstance(spec, dict):
                errs.append("%s: must be an object" % ctx)
                continue
            recon = spec.get("op") == "recon" or spec.get("kind") == "recon"
            kind = spec.get("kind") if "kind" in spec else spec.get("op")
            # artifact admits a fourth, op-less field form (file +
            # field + eq/ne/ne_null) — check_artifact fallthrough.
            if kind not in allowed and not (block == "artifact"
                                            and kind is None):
                errs.append("%s: unknown %s %r (known: %s)"
                            % (ctx, "kind" if block == "wire" else "op",
                               kind, ", ".join(allowed)))
                continue
            if block == "ndjson":
                if kind in ("count", "absent", "present", "eq", "ne") \
                        and not isinstance(spec.get("event"), str):
                    errs.append("%s: op %s needs a string 'event'"
                                % (ctx, kind))
                if kind in ("eq", "ne") \
                        and ("field" not in spec or "value" not in spec):
                    errs.append("%s: op %s needs 'field' and 'value'"
                                % (ctx, kind))
                if kind in ("tools_absent", "tools_present") \
                        and not isinstance(spec.get("names"), list):
                    errs.append("%s: op %s needs a 'names' list"
                                % (ctx, kind))
                if kind == "text_contains" and "value" not in spec:
                    errs.append("%s: text_contains needs 'value'" % ctx)
                if kind == "count" \
                        and not all(isinstance(spec.get(b), int)
                                    for b in ("min", "max")
                                    if b in spec):
                    errs.append("%s: count min/max must be ints" % ctx)
            elif block == "wire":
                if kind == "reasoning_replay":
                    import reasoning_replay
                    errs.extend("%s: %s" % (ctx, error)
                                for error in reasoning_replay.validate_spec(spec))
                if kind == "field" and "path" not in spec:
                    errs.append("%s: field needs 'path'" % ctx)
                if kind == "grep" \
                        and not isinstance(spec.get("grep"), str):
                    errs.append("%s: grep needs a string 'grep'" % ctx)
                if kind in ("field", "grep", "count", "resp_status",
                            "resp_body", "resp_stream") \
                        and not spec.get("file"):
                    errs.append("%s: %s needs a 'file' glob" % (ctx, kind))
                if kind == "golden":
                    if not spec.get("file"):
                        errs.append("%s: golden needs a 'file' glob" % ctx)
                    if not isinstance(spec.get("golden"), str) \
                            or not spec.get("golden"):
                        errs.append("%s: golden needs a string 'golden' "
                                    "fixture path" % ctx)
                    nz = spec.get("normalize")
                    if nz is not None and (
                            not isinstance(nz, list)
                            or not all(isinstance(x, str) for x in nz)):
                        errs.append("%s: golden 'normalize' must be a "
                                    "list of dotted-path strings" % ctx)
                if kind == "count" \
                        and not all(isinstance(spec.get(b), int)
                                    for b in ("min", "max")
                                    if b in spec):
                    errs.append("%s: count min/max must be ints" % ctx)
                if kind == "resp_status":
                    if spec.get("which") not in (None, "first", "last"):
                        errs.append("%s: resp_status 'which' must be "
                                    "first|last" % ctx)
                    si = spec.get("status_in")
                    if si is not None and (
                            not isinstance(si, list)
                            or not all(isinstance(s, int) for s in si)):
                        errs.append("%s: resp_status status_in must be "
                                    "a list of ints" % ctx)
                if kind == "size_lt" and not (
                        isinstance(spec.get("a"), dict)
                        and isinstance(spec.get("b"), dict)):
                    errs.append("%s: size_lt needs 'a' and 'b' sub-specs"
                                % ctx)
                if kind in ("resp_body", "resp_stream"):
                    if not isinstance(spec.get("golden"), str) \
                            or not spec.get("golden"):
                        errs.append("%s: %s needs a string 'golden' "
                                    "fixture path" % (ctx, kind))
                    nz = spec.get("normalize")
                    if nz is not None and (
                            not isinstance(nz, list)
                            or not all(isinstance(x, str) for x in nz)):
                        errs.append("%s: %s 'normalize' must be a list "
                                    "of dotted-path strings" % (ctx,
                                                                kind))
                if kind == "resp_stream":
                    if spec.get("mode") not in (None, "subsequence",
                                                "exact"):
                        errs.append("%s: resp_stream 'mode' must be "
                                    "subsequence|exact" % ctx)
                    drp = spec.get("drop")
                    if drp is not None and (
                            not isinstance(drp, list)
                            or not all(isinstance(x, str) for x in drp)):
                        errs.append("%s: resp_stream 'drop' must be a "
                                    "list of event names" % ctx)
                    for bk in ("min_frames", "max_frames"):
                        if bk in spec \
                                and not isinstance(spec[bk], int):
                            errs.append("%s: resp_stream %s must be an "
                                        "int" % (ctx, bk))
                if recon and not (spec.get("file") or spec.get("event")
                                  or "value" in spec
                                  or spec.get("what") == "final-text"):
                    errs.append("%s: recon needs file|event|value|what"
                                % ctx)
            if block == "artifact" and not recon:
                # check_artifact: three forms — op:count, op:grep, and
                # the field form (no op: file + field + eq/ne/ne_null).
                if kind in ("count", "grep"):
                    if not spec.get("file"):
                        errs.append("%s: %s needs a 'file' glob"
                                    % (ctx, kind))
                    if kind == "count" \
                            and not all(isinstance(spec.get(b), int)
                                         for b in ("min", "max")
                                         if b in spec):
                        errs.append("%s: count min/max must be ints" % ctx)
                    if kind == "grep" \
                            and not isinstance(spec.get("grep"), str):
                        errs.append("%s: grep needs a string 'grep'" % ctx)
                    if not all(isinstance(spec.get(b), int)
                               for b in ("min_count", "max_count")
                               if b in spec):
                        errs.append("%s: min_count/max_count must be ints"
                                    % ctx)
                if kind is None:
                    if not spec.get("file") or "field" not in spec:
                        errs.append("%s: artifact field form needs "
                                    "'file' and 'field'" % ctx)
                    if "which" in spec and spec["which"] not in (
                            "first", "last"):
                        errs.append("%s: artifact 'which' must be "
                                    "first|last" % ctx)
                    if "ne_null" in spec and spec["ne_null"] is not True:
                        errs.append("%s: artifact ne_null must be true"
                                    % ctx)
            if "where" in spec:
                _validate_where(ctx, spec["where"], errs,
                                ndjson_op=kind if block == "ndjson" else None)
    # apex-ayl.22 D-2: NEW (schema_version) files take the full gate;
    # legacy files stop here (back-compat, no new failures).
    if case.get("schema_version") is not None:
        errs.extend(_validate_new_case(name, case, engine))
    # apex-2o6 (HARNESS-UNIFY-IMPL-1): the L1 key-free mode contract —
    # env.proxy='none' declares a hermetic, ZERO-LIVE-OP case whose
    # wire inputs are PRE-PLACED at the case-file sibling
    # <case-stem>/wire/ (copied if-absent into the run dir). Live
    # model ops (LIVE_MODEL_OPS) or a rows case contradict the
    # declaration (there is no proxy to talk to); absent pre-placed
    # inputs make every wire assert vacuous (nothing to pin).
    if (case.get("env") or {}).get("proxy") == "none":
        bad_steps = [i for i, st in enumerate(case.get("steps") or [])
                     if isinstance(st, dict)
                     and st.get("op") in LIVE_MODEL_OPS]
        if bad_steps:
            errs.append(
                "%s: proxy:none forbids live model ops (steps %s: %s) "
                "— a key-free case has no proxy to talk to"
                % (name,
                   ", ".join("steps[%d]" % i for i in bad_steps),
                   ", ".join(case["steps"][i].get("op")
                             for i in bad_steps)))
        if "rows" in case:
            errs.append("%s: proxy:none forbids rows cases (each row "
                        "fires a live headless turn)" % name)
        stem = (os.path.join(os.path.dirname(os.path.abspath(path)),
                             str(cid).lower(), "wire")
                if isinstance(cid, str) and cid else None)
        if not (stem and os.path.isdir(stem) and os.listdir(stem)):
            errs.append(
                "%s: proxy:none requires pre-placed wire inputs at the "
                "case-file sibling %r (copied if-absent into the run's "
                "wire dir; absent = every wire assert is vacuous)"
                % (name, os.path.join(str(cid).lower(), "wire")
                   if isinstance(cid, str) and cid else "?"))
    return errs


def validate_cases_dir(dirpath=None, engine=None):
    """Offline validation over the whole case set. Returns
    {basename: [violations]} — empty when everything is clean."""
    dirpath = dirpath or CASES_DIR
    failures = {}
    for path in sorted(globmod.glob(os.path.join(dirpath, "*.json"))):
        errs = validate_case_file(path, engine=engine)
        if errs:
            failures[os.path.basename(path)] = errs
    return failures


# ---------------------------------------------------------------------------
# apex-ayl.22 D-14 — the campaign/manifest layer (runbook L20 first
# follow-up: the manifest, result schema, validator, and aggregator).
# The runner campaigns use the runbook's seal shape (campaign.json +
# runbook_sha256) with the D-14 fields; the runbook's quick|
# targeted-ratchet|full-functional suite machinery is a future lane
# (OQ-4) and stays out of scope.
# ---------------------------------------------------------------------------

def seal_campaign(campaign_dir, campaign_id, runbook_path, cells,
                  mode="adhoc", meta=None, started_utc=None):
    """apex-ayl.22 D-14(a): seal the campaign — ONE write at run start.
    Copies the runbook byte-exact to <cam>/runbook.md, records
    runbook_sha256, and atomically writes campaign.json carrying
    sealed_sha256 — a canonical digest over the manifest fields with
    started_utc EXCLUDED, so identical inputs seal to a stable sha
    (started_utc is stored, not hashed). Returns the sealed sha."""
    import hashlib
    os.makedirs(campaign_dir, exist_ok=True)
    runbook_sha = None
    if runbook_path:
        with open(runbook_path, "rb") as fh:
            rb_bytes = fh.read()
        with open(os.path.join(campaign_dir, "runbook.md"), "wb") as fh:
            fh.write(rb_bytes)
        runbook_sha = hashlib.sha256(rb_bytes).hexdigest()
    if started_utc is None:
        started_utc = utc_ts()
    base = {
        "schema_version": 1,
        "campaign_id": campaign_id,
        "mode": mode,
        "expected_cases": list(cells),
    }
    if runbook_sha is not None:
        base["runbook_sha256"] = runbook_sha
    if meta:
        for k, v in meta.items():
            if v is not None:
                base[k] = v
    canonical = json.dumps(base, sort_keys=True,
                           separators=(",", ":"), default=str)
    sealed = hashlib.sha256(canonical.encode("utf-8")).hexdigest()
    doc = dict(base)
    doc["started_utc"] = started_utc
    doc["sealed_sha256"] = sealed
    tmp = os.path.join(campaign_dir, "campaign.json.tmp")
    with open(tmp, "w") as fh:
        json.dump(doc, fh, indent=2, default=str)
        fh.write("\n")
    os.replace(tmp, os.path.join(campaign_dir, "campaign.json"))
    return sealed


def _load_verdict_doc(path, rejections, prefix):
    try:
        with open(path) as fh:
            return json.load(fh)
    except Exception:
        rejections.append("%s: malformed verdict.json (unparseable "
                          "result) at %s" % (prefix, path))
        return None


def _campaign_cell_verdicts(cell_dir, rejections, prefix):
    """Collect a cell's verdict docs: the root verdict.json (the
    attempt-1 evidence) plus every subdirectory verdict.json
    (attempt-N/). Malformed docs are recorded and skipped. Returns
    [(location, doc_or_None)]."""
    docs = []
    root_v = os.path.join(cell_dir, "verdict.json")
    if os.path.isfile(root_v):
        docs.append((root_v, _load_verdict_doc(root_v,
                                               rejections, prefix)))
    try:
        subs = sorted(os.listdir(cell_dir))
    except OSError:
        subs = []
    for sub in subs:
        sv = os.path.join(cell_dir, sub, "verdict.json")
        if os.path.isfile(sv):
            docs.append((sv, _load_verdict_doc(sv, rejections,
                                               prefix)))
    return docs


def validate_campaign(campaign_dir):
    """apex-ayl.22 D-14(c): the campaign validator (the runner subset
    of the runbook's 10 rejection classes): malformed result JSON;
    duplicate cell/attempt identity; stale/mismatched campaign_id
    (artifacts from another campaign); identity drift vs the manifest
    expected_cases; PASS with incomplete evidence; retry overwrote
    earlier evidence (attempt-N without attempt-N-1; attempt-1 = the
    cell root). Returns a list of rejection strings (empty = clean)."""
    prefix = campaign_dir
    rejections = []
    man_path = os.path.join(campaign_dir, "campaign.json")
    expected = []
    campaign_id = None
    try:
        with open(man_path) as fh:
            man = json.load(fh)
        expected = list(man.get("expected_cases") or [])
        campaign_id = man.get("campaign_id")
    except Exception as e:
        return ["%s: campaign.json missing or malformed (%s)"
                % (prefix, e)]
    seen = {}  # (cell_id, attempt) -> first location
    try:
        entries = sorted(os.listdir(campaign_dir))
    except OSError:
        entries = []
    for entry in entries:
        cell_dir = os.path.join(campaign_dir, entry)
        if not os.path.isdir(cell_dir):
            continue
        docs = _campaign_cell_verdicts(cell_dir, rejections, prefix)
        if not docs:
            continue
        cell_id = None
        attempts = set()
        for loc, doc in docs:
            if doc is None:
                continue
            if cell_id is None:
                cell_id = doc.get("cell_id") or entry
            attempt = doc.get("attempt", 1)
            attempts.add(attempt)
            key = (cell_id, attempt)
            if key in seen:
                rejections.append(
                    "%s: duplicate cell/attempt identity — cell %r "
                    "attempt %s at both %s and %s"
                    % (prefix, cell_id, attempt, seen[key], loc))
            else:
                seen[key] = loc
            vc = doc.get("campaign_id")
            if vc and campaign_id and vc != campaign_id:
                rejections.append(
                    "%s: campaign_id %r stale/mismatch vs manifest "
                    "campaign_id %r (%s) — artifact from another "
                    "campaign" % (prefix, vc, campaign_id, loc))
            cid = doc.get("case_id")
            if expected and cid not in expected:
                rejections.append(
                    "%s: identity drift — case_id %r not in manifest "
                    "expected_cases %s (%s)" % (prefix, cid, expected,
                                                loc))
            # apex-ayl.91 N4: only a genuine PASS that actually made
            # model calls must carry wire evidence — RECON/VACUOUS/
            # FINDING-PASS rows (outcome-mapped PASS) and the 0-call
            # hermetic mechanism cases legitimately have none.
            is_pass = (doc.get("status") == "PASS"
                       and int(doc.get("model_calls") or 0) > 0)
            if is_pass and not doc.get("evidence_index"):
                rejections.append(
                    "%s: PASS with incomplete evidence — cell %r "
                    "attempt %s has an empty evidence_index (%s)"
                    % (prefix, cell_id, attempt, loc))
        for n in sorted(attempts):
            if n > 1 and (n - 1) not in attempts:
                rejections.append(
                    "%s: retry overwrote earlier evidence — cell %r "
                    "has attempt %d without attempt %d (%s)"
                    % (prefix, cell_id, n, n - 1, cell_dir))
    # apex-ayl.91 N5: every sealed expected case must have a persisted
    # verdict (a lost cell dir or a log-only skip breaks the census),
    # and the seal must still match the manifest fields it was cut over.
    import hashlib
    sealed = man.get("sealed_sha256")
    if sealed:
        base = {k: v for k, v in man.items()
                if k not in ("started_utc", "sealed_sha256")}
        canonical = json.dumps(base, sort_keys=True,
                               separators=(",", ":"), default=str)
        if (hashlib.sha256(canonical.encode("utf-8")).hexdigest()
                != sealed):
            rejections.append(
                "%s: seal broken — sealed_sha256 %s… does not match "
                "the canonical manifest fields" % (prefix, sealed[:12]))
    known = set()
    for cell_id, _attempt in seen:
        if cell_id:
            known.add(cell_id)
            known.add(str(cell_id).lower())
    for cid in expected:
        if cid not in known and cid.lower() not in known:
            rejections.append(
                "%s: no verdict persisted — expected case %r has no "
                "verdict.json in the campaign dir" % (prefix, cid))
    return rejections


def _evidence_grade(evidence_index):
    """apex-ayl.22 D-14(d): evidence grade — wire > ndjson > recon
    (a file citation = wire; an event_line citation = ndjson;
    anything else / none = recon)."""
    for e in evidence_index or []:
        if isinstance(e, dict) and e.get("file"):
            return "wire"
    for e in evidence_index or []:
        if isinstance(e, dict) and e.get("event_line") is not None:
            return "ndjson"
    return "recon"


def aggregate_summary(campaign_dir):
    """apex-ayl.22 D-14(d): after validation — write <cam>/summary.json
    + <cam>/summary.md and return the summary dict. Per cell, the FINAL
    attempt decides: outcome FAIL > BLOCKED > TIMEOUT (the final
    attempt not FAIL/BLOCKED but the cell was killed) > PASS;
    stability FLAKY (the final stability is FLAKY or the final attempt
    > 1 — a retried cell) > STABLE > NOT_ASSESSED. The campaign
    outcome/stability is the worst case across cells (runbook
    L84/L1315)."""
    man = {}
    try:
        with open(os.path.join(campaign_dir, "campaign.json")) as fh:
            man = json.load(fh)
    except Exception:
        pass
    cells = []
    blocked, incomplete, flaky = [], [], []
    expected_changes = []
    try:
        entries = sorted(os.listdir(campaign_dir))
    except OSError:
        entries = []
    for entry in entries:
        cell_dir = os.path.join(campaign_dir, entry)
        if not os.path.isdir(cell_dir):
            continue
        docs = [d for _, d in _campaign_cell_verdicts(
            cell_dir, [], campaign_dir) if d is not None]
        if not docs:
            continue
        docs.sort(key=lambda d: (d.get("attempt") or 1))
        final = docs[-1]
        cell_id = final.get("cell_id") or entry
        base = final.get("outcome") or final.get("status") or "PASS"
        outcome = base if base in ("FAIL", "BLOCKED", "TIMEOUT",
                                   "PASS") else "PASS"
        if outcome not in ("FAIL", "BLOCKED") and final.get("killed"):
            outcome = "TIMEOUT"
        per_flaky = (final.get("stability") == "FLAKY"
                     or (final.get("attempt") or 1) > 1)
        if per_flaky:
            stability = "FLAKY"
        elif outcome == "BLOCKED":
            stability = "NOT_ASSESSED"
        else:
            stability = "STABLE"
        if outcome == "BLOCKED":
            blocked.append(cell_id)
        if outcome == "PASS" and not final.get("evidence_index"):
            incomplete.append(cell_id)
        if per_flaky:
            flaky.append(cell_id)
        if outcome == "PASS" and final.get("wire") == \
                "chat_completions":
            # D-14(d)/G3: a PASS cell observed on the
            # chat_completions wire is a cc first-observation — a
            # RECORDED change (Q3 ratify-then-pin), never a silent
            # golden.
            expected_changes.append({
                "cell_id": cell_id,
                "note": "cc first-observation (Q3 ratify-then-pin): "
                        "the PASS observed on /v1/chat/completions is "
                        "a RECORDED change — adjudicate, then pin",
            })
        cells.append({
            "cell_id": cell_id,
            "case_id": final.get("case_id"),
            "outcome": outcome,
            "stability": stability,
            "final_attempt": final.get("attempt") or 1,
            "attempts": len(docs),
            "killed": bool(final.get("killed")),
            "model_calls": final.get("model_calls"),
            "duration_s": final.get("duration_s"),
            "evidence_grade": _evidence_grade(
                final.get("evidence_index")),
        })
    rank = {"FAIL": 3, "BLOCKED": 2, "TIMEOUT": 1, "PASS": 0}
    outcome = "PASS"
    for c in cells:
        if rank.get(c["outcome"], 0) > rank.get(outcome, 0):
            outcome = c["outcome"]
    if any(c["stability"] == "FLAKY" for c in cells):
        stability = "FLAKY"
    elif any(c["stability"] == "STABLE" for c in cells):
        stability = "STABLE"
    else:
        stability = "NOT_ASSESSED"
    summary = {
        "schema_version": 1,
        "campaign_id": man.get("campaign_id"),
        "mode": man.get("mode"),
        "outcome": outcome,
        "stability": stability,
        "cells": cells,
        "blocked": blocked,
        "incomplete": incomplete,
        "flaky": flaky,
        "expected_changes": expected_changes,
        "aggregated_utc": utc_ts(),
    }
    with open(os.path.join(campaign_dir, "summary.json"), "w") as fh:
        json.dump(summary, fh, indent=2, default=str)
        fh.write("\n")
    md = ["# Campaign summary — %s" % man.get("campaign_id", "?"),
          "",
          "- mode: %s" % man.get("mode", "-"),
          "- outcome: **%s**" % outcome,
          "- stability: **%s**" % stability,
          ""]
    md.append("| cell | outcome | stability | attempt | calls | "
              "dur(s) | evidence |")
    md.append("|---|---|---|---|---|---|---|")
    for c in cells:
        md.append("| %s | %s | %s | %s | %s | %s | %s |" % (
            c["cell_id"], c["outcome"], c["stability"],
            c["final_attempt"], c["model_calls"],
            c["duration_s"], c["evidence_grade"]))
    md.append("")
    if flaky:
        md.append("- flaky: %s" % ", ".join(flaky))
    if blocked:
        md.append("- blocked: %s" % ", ".join(blocked))
    if incomplete:
        md.append("- incomplete: %s" % ", ".join(incomplete))
    for ec in expected_changes:
        md.append("- expected change (%s): %s" % (ec["cell_id"],
                                                  ec["note"]))
    with open(os.path.join(campaign_dir, "summary.md"), "w") as fh:
        fh.write("\n".join(md) + "\n")
    return summary


class Budget:
    def __init__(self, limit):
        self.limit = limit
        self.used = 0

    def add(self, n):
        self.used += n


def resolve_bin_path(raw):
    """Resolve --bin (F-BIN, Wave-S campaign 20260917T102912Z).

    Cases without a `cwd` field run under the hermetic home (D-10), so a
    relative --bin reached Popen unresolved (ENOENT -> 0-call FAIL).
    Relative paths resolve against REPO_ROOT; absolute paths pass through.
    """
    if os.path.isabs(raw):
        return raw
    return os.path.join(REPO_ROOT, raw)


def selftest_contract_pins():
    """Offline contract pins (apex-ayl.70 R2-RED; coordinator adjudication
    2026-09-18, first-hand verified):
    (1) every ACP session/load call in this runner passes "mcpServers": []
        — the field is REQUIRED by agent-client-protocol-schema
        (LoadSessionRequest, no serde default); its absence made every
        cell-seeded run INVALID (run 1, 20260918T034309Z).
    (2) the servers_updated env-value redaction: a synthetic notification
        with a live-looking hex value is scrubbed at write time (name
        kept, marker applied) AND the final redaction_sweep would flag an
        unredacted copy (backstop) while the scrubbed copy passes; the
        legacy [REDACTED] marker (coordinator's 106-file historical
        scrub, ruling 2026-09-18) is a REDACTION MARKER, not a live
        value, and must not be flagged.
    (3) load-first entry (Option A, ruling 2026-09-18): AcpSession.
        start() reaches session/load BEFORE session/new (the
        load_first_sid branch), the case runner's AcpSession
        construction passes load_first_sid=, and the FINDING-A
        resident-path reseed (seed under acp.session_id) is gone.
    Returns a list of failure strings (empty = green)."""
    failures = []
    try:
        with open(os.path.join(HERE, "run.py"), errors="replace") as fh:
            src_lines = fh.read().splitlines()
    except Exception as e:
        return ["pin 1: cannot read own source: %s" % e]
    load_sites = [i for i, l in enumerate(src_lines)
                  if l.strip().startswith('"session/load"')]
    if not load_sites:
        failures.append("pin 1: no ACP session/load call found in run.py")
    for i in load_sites:
        window = "\n".join(src_lines[i:i + 5])
        if '"mcpServers"' not in window or "[]" not in window:
            failures.append("pin 1: session/load params at run.py:%d lack "
                            '"mcpServers": [] (required field)' % (i + 1))
    probe_value = "0123456789abcdef0123456789abcdef"
    probe = ('{"jsonrpc":"2.0","method":"%s","params":{"mcpServers":['
             '{"name":"probe-mcp","type":"stdio","env":[{"name":'
             '"PROBE_SECRET","value":"%s"}]}]}}'
             % (SERVERS_UPDATED_METHOD, probe_value))
    scrubbed = redact_mcp_env_values(probe + "\n")
    if probe_value in scrubbed:
        failures.append("pin 2: write-time scrub left the env value in "
                        "place")
    if '"name":"PROBE_SECRET"' not in scrubbed.replace(" ", ""):
        failures.append("pin 2: write-time scrub dropped/renamed the env "
                        "name")
    if MCP_ENV_REDACTED not in scrubbed:
        failures.append("pin 2: write-time scrub did not apply the marker")
    tmp = tempfile.mkdtemp(prefix="ht1-pins-")
    try:
        with open(os.path.join(tmp, "acp.log"), "w") as fh:
            fh.write(probe + "\n")
        with open(os.path.join(tmp, "acp_scrubbed.log"), "w") as fh:
            fh.write(scrubbed)
        # marker-unification ruling: the legacy [REDACTED] value (the
        # coordinator's historical scrub) must pass the sweep.
        with open(os.path.join(tmp, "acp_legacy.log"), "w") as fh:
            fh.write(probe.replace(probe_value,
                                   LEGACY_REDACTED_MARKER) + "\n")
        _, det = redaction_sweep(tmp, ambient_key="")
        flagged = [d for d in det if "mcp env value NOT redacted" in d]
        if not any(d.startswith("acp.log") for d in flagged):
            failures.append("pin 2: redaction_sweep did not flag the raw "
                            "servers_updated env value (backstop broken)")
        if any(d.startswith("acp_scrubbed.log") for d in flagged):
            failures.append("pin 2: redaction_sweep flagged the SCRUBBED "
                            "probe (false positive)")
        if any(d.startswith("acp_legacy.log") for d in flagged):
            failures.append("pin 2: redaction_sweep flagged the LEGACY "
                            "marker [REDACTED] (ruling: it is a "
                            "redaction marker, not a live value)")
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
    # (3) load-first entry contract (Option A):
    start_idx = next((i for i, l in enumerate(src_lines)
                      if l.strip() == "def start(self):"), -1)
    if start_idx < 0:
        failures.append("pin 3a: AcpSession.start() not found")
    else:
        end_idx = next((i for i in range(start_idx + 1, len(src_lines))
                        if src_lines[i].startswith("    def ")),
                       len(src_lines))
        block = src_lines[start_idx:end_idx]
        block_src = "\n".join(block)
        # substring (quoted) match: the session/new call is inline
        # (ns = self.call("session/new", ...)) while the load-first
        # call puts the method on its own line.
        load_pos = next((j for j, l in enumerate(block)
                         if '"session/load"' in l), -1)
        new_pos = next((j for j, l in enumerate(block)
                        if '"session/new"' in l), -1)
        if (load_pos < 0 or new_pos < 0 or load_pos > new_pos
                or "self.load_first_sid" not in block_src):
            failures.append(
                "pin 3a: AcpSession.start() must enter via "
                "session/load (self.load_first_sid branch) BEFORE "
                "session/new")
    ctor_idx = next((i for i, l in enumerate(src_lines)
                     if l.strip().startswith("acp = AcpSession(")),
                    -1)
    if ctor_idx < 0:
        failures.append("pin 3b: AcpSession construction not found in "
                        "_run_case_once")
    elif "load_first_sid=" not in "\n".join(
            src_lines[ctor_idx:ctor_idx + 10]):
        failures.append("pin 3b: the AcpSession construction does not "
                        "pass load_first_sid= (load-first entry broken)")
    # split literal: the pattern must not appear contiguously in this
    # pin's own source (self-match would make the pin unsatisfiable).
    _res_pat = "xwfix_seed_from_cell(home, case_cwd, " + "acp.session_id"
    if any(_res_pat in l for l in src_lines):
        failures.append("pin 3c: FINDING-A resident-path reseed (seed "
                        "under acp.session_id) is back — cell seeds "
                        "must be pre-connected + load-first")
    return failures


def main(argv=None):
    p = argparse.ArgumentParser(description="HT-1 L3 red-team runner")
    p.add_argument("case_ids", nargs="*",
                   help="case ids (RT-C1 ...); none = full matrix")
    p.add_argument("--selftest", action="store_true",
                   help="run the offline test suite (test_run.py) and "
                        "exit — no proxy key, no binary, no live calls")
    p.add_argument("--daemon", action="store_true",
                   help="cut 0.5 (D-3): self-daemonize (setsid+double-fork) "
                        "and register in smoke/runs.jsonl; logs to "
                        "<out>/run.log, pid to <out>/daemon.pid — designed "
                        "to be launched by smoke/sweepctl.py")
    p.add_argument("--budget", type=int, default=40,
                   help="model-call budget (abort remaining cases past it)")
    p.add_argument("--no-wirecap", action="store_true",
                   help="disable wire capture (default: on)")
    p.add_argument("--rows", default=None,
                   help="for row-based cases (RT-M5): comma model list")
    p.add_argument("--keep-home", action="store_true",
                   help="do not delete hermetic homes")
    p.add_argument("--bin", default=DEFAULT_BIN)
    p.add_argument("--live-home", default=DEFAULT_LIVE_HOME)
    p.add_argument("--out", default=None,
                   help="report root (default smoke/redteam/report/<ts>)")
    p.add_argument("--campaign-id", default=None,
                   help="campaign id stamped into verdict.json "
                        "(apex-ayl.22 D-14)")
    p.add_argument("--wave", default=None,
                   help="wave label for the campaign manifest "
                        "(apex-ayl.22 D-13/D-14)")
    p.add_argument("--mode", default="adhoc",
                   help="campaign mode: slim|full|adhoc "
                        "(apex-ayl.22 D-14; the runbook's quick|"
                        "targeted-ratchet|full-functional enum is the "
                        "future suite lane — OQ-4)")
    p.add_argument("--runbook", default=None,
                   help="runbook copied byte-exact into the campaign "
                        "dir and sealed (apex-ayl.22 D-14/D-16)")
    p.add_argument("--campaign-dir", default=None,
                   help="campaign dir for --aggregate (offline "
                        "validate + summary; no proxy key needed)")
    p.add_argument("--aggregate", action="store_true",
                   help="offline: validate the campaign dir and write "
                        "summary.json/summary.md; exit 1 on any "
                        "rejection (apex-ayl.22 D-14)")
    a = p.parse_args(argv)
    if a.daemon:
        # cut 0.5 (D-3): self-daemonize FIRST (m10 — before any log()/
        # argument-driven I/O); the original process exits 0 immediately and
        # the daemon grandchild continues below with a.out (hence the
        # default campaign id) already fixed.
        if a.selftest or a.aggregate:
            p.error("--daemon is incompatible with --selftest/--aggregate")
        if a.out is None:
            a.out = os.path.join(REPORT_ROOT, utc_ts())
        a.out = os.path.abspath(a.out)
        os.makedirs(a.out, exist_ok=True)
        if os.path.dirname(HERE) not in sys.path:
            sys.path.insert(0, os.path.dirname(HERE))
        import lib.launch as _launch
        _launch.self_daemonize(os.path.join(a.out, "run.log"),
                               os.path.join(a.out, "daemon.pid"))
    a.bin = resolve_bin_path(a.bin)
    if a.selftest:
        # Offline gate: no env key, no report dir, no binary. The suite
        # lives next to this file (test_run.py, stdlib-only) and is
        # importable as `run`'s sibling module.
        # apex-ayl.22 D-1: the offline gate is check_schema (draft-07
        # dual path over the NEW cases) + the in-tree contract over the
        # whole set + the offline test suite.
        schema_failures, engine_used, n_legacy = check_schema()
        if _load_schema() is None:
            log("selftest: FAIL — %s missing (frozen contract drop-in)"
                % os.path.relpath(SCHEMA_PATH, REPO_ROOT))
            return 1
        log("selftest: check_schema engine=%s NEW cases=%d failures=%d "
            "(legacy cases exempt: %d)"
            % (engine_used,
               len(globmod.glob(os.path.join(CASES_DIR, "*.json")))
               - n_legacy,
               sum(len(v) for v in schema_failures.values()),
               n_legacy))
        for fname, ferrs in sorted(schema_failures.items()):
            for e in ferrs:
                log("  schema: %s: %s" % (fname, e))
        contract_failures = validate_cases_dir()
        for fname, ferrs in sorted(contract_failures.items()):
            for e in ferrs:
                log("  contract: %s: %s" % (fname, e))
        if schema_failures or contract_failures:
            log("selftest: FAIL — schema/contract gate has violations")
            return 1
        pin_failures = selftest_contract_pins()
        for e in pin_failures:
            log("  pin: %s" % e)
        if pin_failures:
            log("selftest: FAIL — contract pin violation")
            return 1
        log("selftest: contract pins OK (session/load mcpServers; "
            "servers_updated env redaction: write-time + sweep backstop "
            "[canonical + legacy markers]; load-first entry Option A)")
        import unittest
        if HERE not in sys.path:
            sys.path.insert(0, HERE)
        import test_run
        import test_reasoning_replay
        suite = unittest.TestSuite(
            unittest.defaultTestLoader.loadTestsFromModule(module)
            for module in (test_run, test_reasoning_replay))
        result = unittest.TextTestRunner(verbosity=2).run(suite)
        log("selftest: %s (%d run, %d failures, %d errors)"
            % ("PASS" if result.wasSuccessful() else "FAIL",
               result.testsRun, len(result.failures),
               len(result.errors)))
        # cut 0.5 (D-4): fold the sweepctl ST-SURV survival suite into the
        # offline gate (3/3 hermetic; pass count 73 -> 76).
        import test_sweepctl
        sweep_ok = test_sweepctl.main() == 0
        log("selftest: sweepctl ST-SURV suite %s"
            % ("PASS (3/3)" if sweep_ok else "FAIL (see lines above)"))
        return 0 if (result.wasSuccessful() and sweep_ok) else 1
    if a.aggregate:
        # apex-ayl.22 D-14: the offline campaign lane — no proxy key,
        # no run dir; validate first, and only a clean campaign gets
        # a summary.
        if not a.campaign_dir:
            log("FATAL: --aggregate requires --campaign-dir")
            return 2
        rejections = validate_campaign(a.campaign_dir)
        for r in rejections:
            log("campaign validate: %s" % r)
        if rejections:
            log("campaign: %d rejection(s) — summary NOT written"
                % len(rejections))
            return 1
        summary = aggregate_summary(a.campaign_dir)
        log("campaign: outcome=%s stability=%s flaky=%s summary=%s"
            % (summary["outcome"], summary["stability"],
               ",".join(summary["flaky"]) or "-",
               os.path.join(a.campaign_dir, "summary.json")))
        return 0
    a.wirecap = not a.no_wirecap
    if a.out is None:
        a.out = os.path.join(REPORT_ROOT, utc_ts())
    # kiloecho P0 (full-sweep-20260919T060150Z, class 1): abspath a relative
    # --out before any derived path (GROK_HOME, case cwd, ACP session/new
    # cwd) is computed — a relative home nested an empty hermetic home and
    # killed 40/73 cases. No-op when a.out is already absolute (D-3 daemon).
    a.out = os.path.abspath(a.out)
    os.makedirs(a.out, exist_ok=True)
    # apex-ayl.22 D-14(a): the campaign id defaults to the run dir
    # name; the manifest is sealed once below, after the case roster
    # is loaded (expected_cases).
    a.campaign_id = a.campaign_id or os.path.basename(a.out)

    ambient = os.environ.get("CODEX_LLM_PROXY_KEY", "")
    if not ambient:
        # apex-2o6: the L1 key-free mode — a roster in which EVERY
        # case declares env.proxy='none' runs without a key (the
        # hermetic home points at a dead local port; zero live ops).
        # Anything else stays FATAL (a live case without a key is a
        # misfire, not a feature).
        pre = load_cases(a)
        if pre and all((c.get("env") or {}).get("proxy") == "none"
                       for c in pre):
            log("L1 key-free run: all %d case(s) declare "
                "env.proxy=none (no proxy key read)" % len(pre))
        else:
            log("FATAL: CODEX_LLM_PROXY_KEY not set (env-only, never "
                "on disk) — or run a proxy:none-only roster key-free "
                "(L1)")
            return 2
    a.ambient_key = ambient
    budget = Budget(a.budget)

    git = subprocess.run(["git", "rev-parse", "--short", "HEAD"],
                         cwd=REPO_ROOT, capture_output=True, text=True)
    sha = subprocess.run(["shasum", "-a", "256", a.bin],
                         capture_output=True, text=True)
    env_meta = {
        "ts": utc_ts(),
        "bin": a.bin,
        "bin_sha": (sha.stdout.split()[0][:12] if sha.stdout else "?"),
    "git": git.stdout.strip() or "?",
    "key_sha": sha256_12(ambient) if ambient else "none",
    "py": sys.version.split()[0],
    "upstream": live_upstream(a.live_home),
}
    log("HT-1 L3 run -> %s (budget=%d wirecap=%s)" % (a.out, a.budget,
                                                      a.wirecap))
    cases = load_cases(a)
    log("cases: %s" % ", ".join(c["id"] for c in cases))
    # apex-ayl.22 D-14(a): the campaign manifest — sealed ONCE at run
    # start (campaign.json + the byte-exact runbook copy when
    # --runbook; the sealed sha256 is stable for identical inputs).
    import hashlib
    try:
        cfg_sha = hashlib.sha256(
            read_live_config(a.live_home).encode("utf-8")).hexdigest()
    except Exception:
        cfg_sha = None
    sealed = seal_campaign(a.out, a.campaign_id, a.runbook,
                           [c["id"] for c in cases], mode=a.mode,
                           meta={"change_id": a.wave,
                                 "git_head": env_meta["git"],
                                 "bin_sha256_12": env_meta["bin_sha"],
                                 "key_sha256_12": env_meta["key_sha"],
                                 "upstream": env_meta["upstream"],
                                 "config_sha256": cfg_sha,
                                 "protocol_constants": {
                                     "schema_version": 1,
                                     "caps": CAPS}},
                           started_utc=env_meta["ts"])
    log("campaign: sealed %s (sealed_sha256 %s)" % (a.campaign_id,
                                                    sealed))
    rows = []
    for case in cases:
        log("=== %s: %s" % (case["id"], case.get("title", "")))
        rows.append(run_case(case, a, budget))
    unrouted_total = 0
    for _unr in globmod.glob(os.path.join(a.out, "**", UNROUTED_FILE),
                             recursive=True):
        unrouted_total += _count_cause_lines(os.path.dirname(_unr),
                                             UNROUTED_FILE)
    env_meta["wiretap_unrouted"] = unrouted_total
    if unrouted_total:
        log("HT-1: %d request(s) refused by the route map "
            "(wiretap_unrouted)" % unrouted_total)
    sweep_hits, sweep_details = redaction_sweep(a.out, ambient)
    md = write_report(a.out, rows, env_meta, a, sweep_hits, sweep_details)
    # G4 contract covers the run dir INCLUDING the generated reports, so
    # sweep report.md/report.json after write_report; a hit there fails
    # the sweep like any other artifact.
    report_hits, report_details = redaction_sweep(
        a.out, ambient, files=("report.md", "report.json"))
    sweep_hits += report_hits
    sweep_details.extend(report_details)
    if sweep_hits:
        log("REDACTION SWEEP: %d HIT(S) — inspect before sharing"
            % sweep_hits)
    else:
        log("redaction sweep: 0 hits")
    # r3 §4.4.5: the literal-key check belongs HERE (post-report, over the
    # report dir), never in --selftest. Key-free mode has no credential to
    # check, so it is skipped with a printed reason rather than passing
    # silently.
    if ambient:
        lit_hits, lit_details = literal_check(a.out)
        sweep_hits += lit_hits
        sweep_details.extend(lit_details)
    else:
        log("literal check: skipped (key-free run — no credential set)")
    log("report: %s" % md)
    log("calls used: %d / %d" % (budget.used, budget.limit))
    bad = [r for r in rows if r["status"] in ("FAIL", "BLOCKED")]
    # §4.4.3: a leaked credential fails the run, and the verdict is persisted
    # because --daemon swallows the exit status (s0 F-8).
    code = run_verdict_code(bad=len(bad), sweep_hits=sweep_hits)
    persist_run_verdict(a.out, rows, sweep_hits=sweep_hits, code=code)
    log("verdict: %s exit=%d sweep_hits=%d (run-verdict.json)"
        % ("FAIL" if code else "PASS", code, sweep_hits))
    return code


if __name__ == "__main__":
    code = main()
    sys.exit(code)
