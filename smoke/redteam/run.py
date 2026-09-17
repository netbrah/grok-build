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

DEFAULT_BIN = os.path.join(REPO_ROOT, "target", "debug", "grok-responses")
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


class HermeticHome:
    """Tempdir GROK_HOME: live config copy + patch + hermetic auth stub,
    with all base_urls rewritten to the local wiretap (when wirecap)."""

    def __init__(self, root: str, live_home: str, config_patch: dict,
                 wire_port, keep: bool = False):
        self.home = os.path.join(root, "home")
        self.cwd = os.path.join(root, "cwd")
        os.makedirs(self.home, mode=0o700, exist_ok=True)
        os.makedirs(self.cwd, exist_ok=True)
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


def build_env(home: str, extra_unset=None) -> dict:
    # apex-ayl.22 D-7: the case env.provider_vars_unset extends the
    # runner L1 set (the union is unset; a subset declaration is
    # rejected at the NEW-case gate).
    unset = set(PROVIDER_VARS_UNSET)
    if extra_unset:
        unset.update(extra_unset)
    env = {k: v for k, v in os.environ.items() if k not in unset}
    env["GROK_AUTH_EXPIRED"] = "1"
    env["GROK_HOME"] = home
    return env


def run_headless_turn(bin_path, home, cwd, model, prompt, resume_sid,
                      output_format="streaming-json", extra_args=None,
                      timeout_s=300, kill_after_s=None,
                      capture_dir=None, extra_unset=None):
    args = [bin_path, "-m", model, "-p", prompt,
            "--output-format", output_format, "--always-approve"]
    if resume_sid:
        args += ["--resume", resume_sid]
    args += extra_args or []
    env = build_env(home, extra_unset)
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
                 set_model_timeout_s=None, extra_unset=None):
        self.bin_path = bin_path
        self.home = home
        self.cwd = cwd
        self.model = model
        self.log_path = log_path
        self.timeout_s = timeout_s
        # apex-ayl.22 D-7: the case env.provider_vars_unset extends
        # the runner L1 set (build_env unset union).
        self.extra_unset = extra_unset
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
        self.log = open(log_path, "w")
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

    def start(self):
        env = build_env(self.home, self.extra_unset)
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
        """
        end = time.time() + timeout
        pending = None
        line_bytes = 0
        last_beat = 0.0
        while time.time() < end:
            r, _, _ = self._select.select([self.proc.stdout], [], [],
                                          max(0.1, end - time.time()))
            if not r:
                # Slow-line heartbeat (console only — acp.log mirrors
                # the raw transcript and must stay byte-identical).
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
            buf = (pending or "") + text
            line_bytes += len(raw)
            while True:
                nl = buf.find("\n")
                if nl < 0:
                    break
                line, buf = buf[:nl], buf[nl + 1:]
                line = line.strip()
                if line:
                    yield line
            pending = buf
            line_bytes = len(buf)

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
        """
        completion = None
        response = None
        # Older-shell fallback tracking: whether this turn's expected
        # promptId showed up in a queue row. On this binary it always
        # does (queue rows reuse the prompt id); if a future binary
        # ignored the client-chosen id, completions carry the binary's
        # own id and the first unclaimed completion is adopted (the
        # pre-fix behavior — the consumed-guard still protects it).
        expected_seen = [False]

        def _match(pid):
            if pid and pid in self._completed_prompt_ids:
                return False
            if expected_prompt_id and pid:
                if pid == expected_prompt_id:
                    return True
                if expected_seen[0]:
                    return False  # ours is known; this one is foreign
                return True      # older-shell fallback (first unclaimed)
            if expected_prompt_id and pid is None:
                return True      # unmapped completion (older shell)
            return True          # no expected id: first unclaimed

        for line in self._iter_lines(timeout):
            try:
                d = json.loads(line)
            except Exception:
                continue
            if d.get("method") is None:
                # JSON-RPC response (result or error): informational.
                if isinstance(d.get("id"), int) and d["id"] == tid \
                        and response is None:
                    response = d
                continue
            p = d.get("params") or {}
            if p.get("sessionId") not in (None, self.session_id):
                continue
            self._track_update(d)
            m = d.get("method")
            if m == "_x.ai/queue/changed":
                # apex-ayl.28: queue rows carry the prompt identity
                # (entries[].id "reuses the prompt's unique prompt_id",
                # runningPromptId = the designated correlation signal —
                # xai-prompt-queue/src/types.rs). Seeing ours here is
                # the confirmation that the binary adopted it.
                if expected_prompt_id and not expected_seen[0]:
                    if p.get("runningPromptId") == expected_prompt_id \
                            or any(e.get("id") == expected_prompt_id
                                   for e in p.get("entries") or []):
                        expected_seen[0] = True
                continue
            if m == "_x.ai/session/prompt_complete":
                pid = p.get("promptId")
                if not _match(pid):
                    log("ACP desync guard: ignoring prompt_complete "
                        "pid=%s (expected %s) — late/foreign "
                        "completion" % (pid, expected_prompt_id))
                    continue
                completion = {"promptId": pid or expected_prompt_id,
                              "stopReason": p.get("stopReason"),
                              "agentResult": p.get("agentResult"),
                              "via": "prompt_complete"}
                break
            elif m == "_x.ai/session_notification":
                u = p.get("update") or {}
                if u.get("sessionUpdate") != "turn_completed":
                    continue
                pid = u.get("prompt_id")
                if not _match(pid):
                    log("ACP desync guard: ignoring turn_completed "
                        "pid=%s (expected %s) — late/foreign "
                        "completion" % (pid, expected_prompt_id))
                    continue
                completion = {"promptId": pid or expected_prompt_id,
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
                    for gline in self._iter_lines(ACP_ERROR_GRACE_S):
                        try:
                            gd = json.loads(gline)
                        except Exception:
                            continue
                        if gd.get("method") \
                                != "_x.ai/session/prompt_complete":
                            continue
                        gp = gd.get("params") or {}
                        if gp.get("sessionId") not in \
                                (None, self.session_id):
                            continue
                        gpid = gp.get("promptId")
                        if not _match(gpid):
                            continue
                        completion = {"promptId": gpid
                                      or completion["promptId"],
                                      "stopReason":
                                          completion["stopReason"],
                                      "agentResult":
                                          gp.get("agentResult"),
                                      "via": "prompt_complete"}
                        break
                break
        if completion is not None and completion.get("promptId"):
            self._completed_prompt_ids.add(completion["promptId"])
        return completion, response

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
        """
        t = timeout or self.timeout_s
        prompt_id = str(uuid.uuid4())
        tid = self._send("session/prompt",
                         {"sessionId": self.session_id,
                          "prompt": [{"type": "text", "text": text}],
                          "_meta": {"promptId": prompt_id}})
        completion, response = self._read_turn_completion(
            tid, t, expected_prompt_id=prompt_id)
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
        return {"error": {"message": "timeout after %ss" % t}}, None

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
            p = d.get("params") or {}
            if p.get("sessionId") not in (None, self.session_id):
                continue
            self._track_update(d)
            u = (p.get("update") or {}) if d.get("method") == \
                "_x.ai/session_notification" else {}
            if u.get("sessionUpdate") and \
                    (u.get("error") or u.get("error_type")):
                return {"error": {"message": "set_model failed via "
                                              "notification",
                                  "data": u}}
        return {"error": {"message": "timeout after %ss waiting for "
                                     "set_model response" % t}}

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
    def __init__(self, port, upstream, capture_dir, ambient_key):
        self.port = port
        self.capture_dir = capture_dir
        env = dict(os.environ)
        if ambient_key:
            # HYG-1: hand the key to wiretap via env, never argv
            # (argv is ps-visible; 5 dead orphans leaked it that way).
            env["WIRETAP_AMBIENT_KEY"] = ambient_key
        self.proc = subprocess.Popen(
            [sys.executable, os.path.abspath(WIRETAP),
             "--port", str(port), "--upstream", upstream,
             "--capture", capture_dir],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env=env)
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
    if op == "count":
        n = sum(1 for e in events if e.get("type") == ev)
        ok = n >= spec.get("min", 0) and n <= spec.get("max", 10 ** 9)
        return AssertResult(spec, "ndjson.count", ok,
                            "%d events of type %s" % (n, ev),
                            "ndjson: %d x type=%s" % (n, ev))
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
        matches = [e for e in events if e.get("type") == ev]
        which = spec.get("which", "first")
        m = matches[0] if which == "first" else matches[-1] if matches else None
        if m is None:
            ok = False if op == "eq" else True
            return AssertResult(spec, "ndjson.%s" % op, ok,
                                "no event of type %s" % ev,
                                "ndjson: none")
        found, val = get_path(m, spec.get("field"))
        want = spec.get("value")
        ok = _val_eq(val, want) if op == "eq" else not _val_eq(val, want)
        return AssertResult(spec,
                            "ndjson.%s" % op, ok,
                            "type=%s %s=%r want %r" % (ev, spec.get("field"),
                                                       val, want),
                            "ndjson line %s: type=%s %s=%r"
                            % (m.get("_line"), ev, spec.get("field"), val),
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


def _wire_filter(paths, where):
    if not where:
        return paths
    out = []
    for p in paths:
        try:
            with open(p) as fh:
                d = json.load(fh)
        except Exception:
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


def synth_output_pins(case):
    """apex-ayl.22 D-4b: an output_contains on a declared mcp_call/
    tool_call IS a scored synthesized wire-grep pin. The runner
    synthesizes a wire assert (file = req-*.json, where = the case
    wire path + body.model, any-form) for the expected output
    substring; 0 hits = FAIL. The container-close pin (the cg_rt
    class) stays the case's own pin — the synthesis adds the payload
    check, it does not replace the lifecycle pin. Legacy cases (no
    declarations) get no pins. The runner never rewrites prompts —
    the contract is the declared output_contains."""
    out = []
    where = {"method": "POST"}
    wire = case.get("wire")
    if wire in WIRE_PATHS:
        where["path"] = WIRE_PATHS[wire]
    model = case.get("model")
    if isinstance(model, str) and model:
        where["body.model"] = model
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


def check_wire(spec, capture_dir):
    kind = spec.get("kind")
    base = capture_dir if capture_dir else os.devnull
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
                pos = text.find(needle) if needle else -1
                if pos >= 0:
                    hit = (f, pos)
                    break
            if hit is not None:
                f, pos = hit
                return AssertResult(spec, "wire.grep", False,
                                    "%s contains %r (absent in all %d)"
                                    % (os.path.basename(f), needle,
                                       len(paths)),
                                    "wire: %s grep %r"
                                    % (os.path.basename(f),
                                       needle[:80]),
                                    cite=_wire_cite(f,
                                                    [pos, pos +
                                                     len(needle)]))
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
                pos = text.find(needle) if needle else -1
                if pos >= 0:
                    return AssertResult(spec, "wire.grep", True,
                                        "%s contains %r (any of %d)"
                                        % (os.path.basename(f), needle,
                                           len(paths)),
                                        "wire: %s grep %r"
                                        % (os.path.basename(f),
                                           needle[:80]),
                                        cite=_wire_cite(f,
                                                        [pos,
                                                         pos + len(
                                                             needle)]))
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
        present = needle in text
        ok = (not present) if spec.get("absent") else present
        cite = None
        if present:
            pos = text.find(needle)
            cite = _wire_cite(f, [pos, pos + len(needle)])
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
        req_paths = _resolve_glob(spec.get("file", "req-*.json"), base)
        if not req_paths:
            return AssertResult(spec, "wire.resp_status", False,
                                "no wire files match glob %r"
                                % spec.get("file"),
                                "wire: glob %s -> none" % spec.get("file"))
        req_paths = _wire_filter(req_paths, spec.get("where"))
        if not req_paths:
            return AssertResult(spec, "wire.resp_status", False,
                                "no request matches where-filter %r "
                                "(probe request never fired)"
                                % spec.get("where"),
                                "wire: resp_status where -> none")
        def _cap_n(p):
            try:
                with open(p) as fh:
                    return json.load(fh).get("n")
            except Exception:
                return None
        def _n_key(p):
            n = _cap_n(p)
            return (0, n) if isinstance(n, int) else (1, p)
        req_paths.sort(key=_n_key)
        if spec.get("which") == "last":
            req_paths = list(reversed(req_paths))
        f = req_paths[0]
        n = _cap_n(f)
        if not isinstance(n, int):
            return AssertResult(spec, "wire.resp_status", False,
                                "request %s carries no capture n"
                                % os.path.basename(f),
                                "wire: resp_status -> no n")
        resp_path = os.path.join(base, "resp-%03d.jsonl" % n)
        if not os.path.isfile(resp_path):
            resp_path = None
            for rp in sorted(globmod.glob(
                    os.path.join(base, "resp-*.jsonl"))):
                try:
                    with open(rp) as fh:
                        if json.loads(fh.readline()).get("n") == n:
                            resp_path = rp
                            break
                except Exception:
                    continue
        if resp_path is None:
            return AssertResult(spec, "wire.resp_status", False,
                                "no response captured for %s (n=%s) - "
                                "upstream never answered"
                                % (os.path.basename(f), n),
                                "wire: resp_status -> no resp capture")
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
        # apex-ayl.22 D-12 (G15): any = the newest mtime (paths[0]
        # under the prefer_last sort); first = the oldest (paths[-1]);
        # the always-ok recon contract is unchanged either way.
        if spec.get("first"):
            nth = len(paths) - 1
        elif spec.get("any"):
            nth = 0
        else:
            nth = spec.get("nth", 0)
        if spec.get("grep") is not None:
            val = "<no files>"
            if nth < len(paths):
                with open(paths[nth], errors="replace") as fh:
                    text = fh.read()
                hits = [ln for ln in text.splitlines()
                        if spec["grep"] in ln]
                val = "%d line(s) contain %r: %s" % (
                    len(hits), spec["grep"][:60],
                    (hits[0][:200] if hits else "-"))
        elif paths and nth < len(paths):
            with open(paths[nth]) as fh:
                d = json.load(fh)
            found, val = get_path(d, spec.get("field"))
            val = val if found else "<missing>"
        else:
            val = "<no files>"
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
# file; recon asserts are never scored.
AUDITED_WIRE_KINDS = ("field", "grep", "resp_status", "size_lt")


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
        if r.ok and not _cite_resolvable(r.cite, wire_dir):
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


def recheck_models_available(upstream, model, ambient_key):
    """apex-ayl.22 D-5: a FRESH `GET /v1/models` through the proxy
    (HYG-1: the ambient key goes in the in-memory header, never
    argv/disk). Returns True iff `model` is still in the catalog. Any
    failure to reach or parse the catalog is False (fail-closed: a
    catalog we cannot confirm is treated as changed — the cell FAILs,
    no retry; 404 != 400, manifest §4)."""
    if not model:
        return False
    url = (upstream or "").rstrip("/") + "/v1/models"
    req = urllib.request.Request(url)
    if ambient_key:
        req.add_header("Authorization", "Bearer " + ambient_key)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            if getattr(resp, "status", 200) != 200:
                return False
            data = json.loads(resp.read().decode("utf-8", "replace"))
    except Exception:
        return False
    items = data.get("data") if isinstance(data, dict) else data
    for it in items or []:
        if isinstance(it, dict) and it.get("id") == model:
            return True
        if it == model:
            return True
    return False


def _retry_decision(case, capture_dir, model, attempt, max_attempts,
                    upstream, ambient_key=""):
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
    last = _last_response_status(capture_dir, model)
    if last is None or last[0] not in on_status:
        return (False, "last response status %r not in on_status %r"
                % (last[0] if last else None, on_status), None)
    if retry_block.get("recheck_models"):
        present = recheck_models_available(upstream, model, ambient_key)
        if not present:
            return (False,
                    "model %r gone on recheck (catalog change, not an "
                    "env flap)" % model, False)
        return (True, "env flap: last status %d in on_status %r; model "
                "%r still in catalog" % (last[0], on_status, model),
                True)
    return (True, "env flap: last status %d in on_status %r"
            % (last[0], on_status), None)


def finalize_verdict(case, pre_status, results, wire_dir=None):
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
    Returns (status, info)."""
    info = {"premises": [], "findings": [], "evidence_violations": [],
            "blocked_reason": None, "vacuous_reason": None,
            "no_evidence": False}
    if case.get("recon"):
        return (pre_status if pre_status == "RECON" else pre_status), info
    if pre_status == "SKIP":
        return pre_status, info
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
    row = _run_case_once(case, args, budget, run_dir, attempt)
    rechecks = []
    while row["status"] in ("FAIL", "BLOCKED"):
        ok, reason, recheck = _retry_decision(
            case, os.path.join(run_dir, "wire"), case.get("model"),
            attempt, max_attempts, live_upstream(args.live_home),
            getattr(args, "ambient_key", ""))
        if not ok:
            log("  %s: no env retry (%s)" % (cid, reason))
            break
        if recheck is not None:
            rechecks.append({"op": "recheck_models",
                             "model": case.get("model"),
                             "present": recheck,
                             "ts": utc_ts()})
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


def _run_case_once(case, args, budget, run_dir, attempt):
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
            return _skip_row(case, "disabled: %s" % case.get("reason",
                                                            case.get(
                                                                "unblocks_at",
                                                                "")))
        est = case.get("est_calls", 3)
        if budget.used + est > args.budget:
            return _skip_row(case, "budget: used=%d est=%d limit=%d"
                             % (budget.used, est, args.budget))

        wirecap = (case.get("wirecap", True) and args.wirecap)
        port = free_port() if wirecap else None
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
        home = HermeticHome(run_dir, args.live_home,
                            case.get("config_patch", {}), port,
                            keep=args.keep_home)
        case_cwd = case_cwd or home.cwd
        upstream = live_upstream(args.live_home)
        wt = None
        if wirecap:
            ctx.capture_dir = os.path.join(run_dir, "wire")
            wt = Wiretap(port, upstream, ctx.capture_dir, args.ambient_key)
            if not wt.wait_ready():
                mark("BLOCKED", "wiretap did not start")
                return _finish(case, run_dir, ctx, results, status, started,
                               wt, home, args, budget)
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

        driver = case.get("driver", "headless")
        if driver == "acp":
            acp = AcpSession(args.bin, home.home, case_cwd, cur_model,
                             os.path.join(run_dir, "acp.log"),
                             init_timeout_s=_effective_acp_init_timeout(
                                 case),
                             set_model_timeout_s=
                             _effective_set_model_timeout(case),
                             extra_unset=extra_unset)
            acp.start()
            ctx.acp = acp
            log("  %s: ACP session %s model=%s" % (cid, acp.session_id,
                                                   cur_model))
            ctx.session_id = acp.session_id
            ctx.session_dir = home.session_dir_for(case_cwd, acp.session_id)
            ctx.fmt = "acp"
        extra = []
        if case.get("agents_json"):
            extra += ["--agents", json.dumps(case["agents_json"])]

        rows = case.get("rows")
        if rows:
            ctx.row_order = []
            want = {r.strip() for r in args.rows.split(",")} if args.rows \
                else None
            for row in rows:
                model = row["model"]
                if want and model not in want:
                    continue
                ctx.row_order.append(model)
                tr = run_headless_turn(
                    args.bin, home.home, case_cwd, model,
                    row.get("prompt", "Reply with exactly: HT1-OK"),
                    None, output_format=fmt,
                    timeout_s=_effective_turn_timeout(case),
                    capture_dir=ctx.capture_dir,
                    extra_unset=extra_unset)
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
                if driver == "acp":
                    before = len(acp.updates)
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
                    rec = {"op": "acp_turn", "step": si, "model": model,
                           "stop": stop,
                           "error": resp.get("error"),
                           "prompt_id": prompt_id,
                           "text": "".join(chunks)[:200]}
                    ctx.turns.append(rec)
                    for c in chunks:
                        ctx.events.append({"type": "acp_message",
                                           "data": c, "_line": 0})
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
                        extra_unset=extra_unset)
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
                        extra_unset=extra_unset)
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
                        extra_unset=extra_unset)
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

        _copy_session_evidence(home, ctx, run_dir)
        assert_block = case.get("assert", {})
        for spec in assert_block.get("ndjson", []):
            if spec.get("op") == "recon":
                results.append(check_recon(spec, ctx))
                continue
            results.append(check_ndjson(spec, ctx.events, fmt, ctx))
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
        if wirecap:
            # apex-ayl.22 D-4b: the declared output_contains pins are
            # scored synthesized wire-grep pins (added to the case's
            # own explicit wire pins).
            wire_specs = list(assert_block.get("wire", []))
            wire_specs.extend(synth_output_pins(case))
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


def _finish(case, run_dir, ctx, results, status, started, wt, home, args,
            budget, row_asserts=None, attempt=1):
    if wt:
        wt.stop()
    if row_asserts:
        run_order = getattr(ctx, "row_order", [])
        for spec in row_asserts.get("wire", []):
            rm = _row_model_for_spec(spec)
            if rm and rm not in run_order:
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
                    spec["nth"] = run_order.index(rm)
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
    status, vinfo = finalize_verdict(case, status, results,
                                     wire_dir=wire_dir)
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
    if v.get("no_evidence") or v.get("blocked_reason"):
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
    return {"session_id": ctx.session_id,
            "session_dir": session_dir,
            "cwd": cwd,
            "model": model,
            "wire": case.get("wire"),
            "acp_log": acp_log,
            "wire_dir": wire_dir,
            "unified_log": unified_log,
            "classification_hint": hint,
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
    outcome = "FAIL" if status == "FAIL" else \
        ("BLOCKED" if status == "BLOCKED" else "PASS")
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
    r'(?i)(sk-[A-Za-z0-9_-]{16,}|AKIA[A-Z0-9]{16}|'
    r'(?:api[_-]?key|token)\s*[:=]\s*"[A-Za-z0-9._-]{24,}"'
    r'|user=[A-Za-z0-9]+:[A-Za-z0-9+/=]{20,}@)')


def redaction_sweep(root: str, ambient_key: str, files=None):
    """Grep the run dir (or the named files within it) for the ambient key
    + key-like heuristics. Returns (hits, details)."""
    hits = 0
    details = []
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
            n = text.count(ambient_key) if ambient_key else 0
            if n:
                hits += n
                details.append("%s: %d ambient-key hit(s)"
                               % (os.path.relpath(p, root), n))
            for m in KEY_HEURISTIC.finditer(text):
                frag = m.group(0)
                if ambient_key and frag in ambient_key:
                    continue
                hits += 1
                details.append("%s: key-heuristic %r"
                               % (os.path.relpath(p, root), frag[:40]))
    return hits, details


# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------

def slice_evidence(row, limit=10, secrets=()):
    """For FAIL rows: the 10-line log/wire slice around the broken assert."""
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
    md.append("# HT-1 L3 red-team run report")
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
            md.extend(slice_evidence(r, secrets=(args.ambient_key,)))
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

STEP_OPS = ("turn", "switch", "kill", "compact", "idle", "recon_note")
NDJSON_OPS = ("count", "absent", "present", "eq", "ne", "text_contains",
              "tools_absent", "tools_present", "recon")
WIRE_KINDS = ("field", "grep", "size_lt", "count", "resp_status", "recon")
ARTIFACT_OPS = ("count", "grep", "recon")


def _validate_where(ctx, where, errs):
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
    if op == "idle":
        if not (isinstance(st.get("s"), int) and st["s"] > 0):
            errs.append("%s: idle needs a positive int 's'" % ctx)
    if op in ("turn", "compact") and "timeout_s" in st \
            and not isinstance(st["timeout_s"], (int, float)):
        errs.append("%s: timeout_s must be a number" % ctx)


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
        (D-7; a subset declaration is rejected).
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
        errs.append(
            "%s: harness-class premise without mcp_calls (plain tools "
            "are always offered by the binary; no harness premise "
            "exists)" % name)
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
                if kind == "field" and "path" not in spec:
                    errs.append("%s: field needs 'path'" % ctx)
                if kind == "grep" \
                        and not isinstance(spec.get("grep"), str):
                    errs.append("%s: grep needs a string 'grep'" % ctx)
                if kind in ("field", "grep", "count", "resp_status") \
                        and not spec.get("file"):
                    errs.append("%s: %s needs a 'file' glob" % (ctx, kind))
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
                _validate_where(ctx, spec["where"], errs)
    # apex-ayl.22 D-2: NEW (schema_version) files take the full gate;
    # legacy files stop here (back-compat, no new failures).
    if case.get("schema_version") is not None:
        errs.extend(_validate_new_case(name, case, engine))
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
            is_pass = (doc.get("outcome") == "PASS"
                       or doc.get("status") == "PASS")
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


def main(argv=None):
    p = argparse.ArgumentParser(description="HT-1 L3 red-team runner")
    p.add_argument("case_ids", nargs="*",
                   help="case ids (RT-C1 ...); none = full matrix")
    p.add_argument("--selftest", action="store_true",
                   help="run the offline test suite (test_run.py) and "
                        "exit — no proxy key, no binary, no live calls")
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
        import unittest
        if HERE not in sys.path:
            sys.path.insert(0, HERE)
        import test_run
        suite = unittest.defaultTestLoader.loadTestsFromModule(test_run)
        result = unittest.TextTestRunner(verbosity=2).run(suite)
        log("selftest: %s (%d run, %d failures, %d errors)"
            % ("PASS" if result.wasSuccessful() else "FAIL",
               result.testsRun, len(result.failures),
               len(result.errors)))
        return 0 if result.wasSuccessful() else 1
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
    os.makedirs(a.out, exist_ok=True)
    # apex-ayl.22 D-14(a): the campaign id defaults to the run dir
    # name; the manifest is sealed once below, after the case roster
    # is loaded (expected_cases).
    a.campaign_id = a.campaign_id or os.path.basename(a.out)

    ambient = os.environ.get("CODEX_LLM_PROXY_KEY", "")
    if not ambient:
        log("FATAL: CODEX_LLM_PROXY_KEY not set (env-only, never on disk)")
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
    "key_sha": sha256_12(ambient),
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
    log("report: %s" % md)
    log("calls used: %d / %d" % (budget.used, budget.limit))
    bad = [r for r in rows if r["status"] in ("FAIL", "BLOCKED")]
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
