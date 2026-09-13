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
No case args = full matrix (disabled cases skipped). Report ->
smoke/redteam/report/<UTC-ts>/{report.md,report.json,<case>/...}.

Case-file contract: see smoke/redteam/cases/*.json and spec §4.
"""
import argparse
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
import uuid
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
WIRETAP = os.path.join(HERE, "..", "wiretap", "wiretap.py")
CASES_DIR = os.path.join(HERE, "cases")
REPORT_ROOT = os.path.join(HERE, "report")

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
        return m.group(1).rstrip("/")
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
                insert_at = end
                while (insert_at > target + 1
                       and lines[insert_at - 1].strip() == ""):
                    insert_at -= 1
                lines.insert(insert_at, line)
            cfg_text = "".join(lines)
    return cfg_text


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
        cfg = _apply_config_patch(cfg, config_patch or {})
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
        if not keep:
            pass  # cleanup via caller (tempdir)
        self.keep = keep

    def session_dir_for(self, cwd: str, sid: str) -> str:
        return os.path.join(self.home, "sessions",
                            encode_cwd_dirname(cwd), sid)


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


def build_env(home: str) -> dict:
    env = {k: v for k, v in os.environ.items()
           if k not in PROVIDER_VARS_UNSET}
    env["GROK_AUTH_EXPIRED"] = "1"
    env["GROK_HOME"] = home
    return env


def run_headless_turn(bin_path, home, cwd, model, prompt, resume_sid,
                      output_format="streaming-json", extra_args=None,
                      timeout_s=300, kill_after_s=None,
                      capture_dir=None):
    args = [bin_path, "-m", model, "-p", prompt,
            "--output-format", output_format, "--always-approve"]
    if resume_sid:
        args += ["--resume", resume_sid]
    args += extra_args or []
    env = build_env(home)
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
        if d.get("method") == "POST" and \
                (d.get("path", "").endswith("/responses") or
                 d.get("path", "").endswith("/messages")):
            n += 1
    return n


# ---------------------------------------------------------------------------
# ACP stdio driver (RECON R-1b: NDJSON-RPC 2.0, stdlib-reachable)
# ---------------------------------------------------------------------------

class AcpSession:
    def __init__(self, bin_path, home, cwd, model, log_path,
                 timeout_s=180):
        self.bin_path = bin_path
        self.home = home
        self.cwd = cwd
        self.model = model
        self.log_path = log_path
        self.timeout_s = timeout_s
        self.proc = None
        self._id = 0
        self.session_id = None
        self.log = open(log_path, "w")
        self.updates = []  # all session/update params, in order

    def start(self):
        env = build_env(self.home)
        self.proc = subprocess.Popen(
            [self.bin_path, "agent", "stdio"], cwd=self.cwd, env=env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, text=True)
        import select
        self._select = select
        init_params = {
            "protocolVersion": 1,
            "clientCapabilities": {"fs": {}, "terminal": False},
            "clientInfo": {"name": "ht1-l3-runner", "version": "0"},
            "_meta": {"startupHints": {"nonInteractive": True,
                                       "skipGitStatus": True}},
        }
        resp = self.call("initialize", init_params, timeout=60)
        if "result" not in resp:
            raise RuntimeError("ACP initialize failed: %s" % resp)
        ns = self.call("session/new",
                       {"cwd": self.cwd, "mcpServers": [],
                        "_meta": {"modelId": self.model}},
                       timeout=60)
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

    def _read_until(self, target_id, timeout):
        import select
        end = time.time() + timeout
        pending = ""
        while time.time() < end:
            r, _, _ = select.select([self.proc.stdout], [], [],
                                    max(0.1, end - time.time()))
            if not r:
                continue
            ch = self.proc.stdout.read(1)
            if not ch:
                break
            self.log.write(ch)
            if ch == "\n":
                line = pending.strip()
                pending = ""
                if not line:
                    continue
                try:
                    d = json.loads(line)
                except Exception:
                    continue
                if d.get("method") == "session/update":
                    p = d.get("params") or {}
                    u = p.get("update") or {}
                    u["_sessionId"] = p.get("sessionId")
                    self.updates.append(u)
                if isinstance(d.get("id"), int) and d["id"] == target_id:
                    return d
            else:
                pending += ch
        return {"error": {"message": "timeout after %ss" % timeout}}

    def call(self, method, params, timeout=None):
        tid = self._send(method, params)
        return self._read_until(tid, timeout or self.timeout_s)

    def prompt(self, text, timeout=None):
        resp = self.call("session/prompt",
                         {"sessionId": self.session_id,
                          "prompt": [{"type": "text", "text": text}]},
                         timeout=timeout)
        stop = None
        if "result" in resp:
            stop = resp["result"].get("stopReason")
        return resp, stop

    def set_model(self, model):
        self.model = model
        return self.call("session/set_model",
                         {"sessionId": self.session_id, "modelId": model},
                         timeout=60)

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
        self.proc = subprocess.Popen(
            [sys.executable, os.path.abspath(WIRETAP),
             "--port", str(port), "--upstream", upstream,
             "--capture", capture_dir, "--ambient-key", ambient_key],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
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
    def __init__(self, spec, kind, ok, detail, evidence, recon=False):
        self.spec = spec
        self.kind = kind
        self.ok = ok
        self.detail = detail
        self.evidence = evidence
        self.recon = recon


def _val_eq(a, b):
    if isinstance(b, str) and isinstance(a, str):
        return a == b
    if a is None or b is None:
        return a is b or (a == b)
    if isinstance(a, (int, float)) and isinstance(b, (int, float)):
        return a == b
    return a == b


def check_ndjson(spec, events, fmt):
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
        return AssertResult(spec, "ndjson.%s" % op, ok,
                            "%d events of type %s" % (n, ev),
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
                            % (m.get("_line"), ev, spec.get("field"), val))
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
                            "artifacts/%s: %s=%r" % (rel, field, val))
    if "eq" in spec:
        ok = _val_eq(val, spec["eq"])
    elif "ne" in spec:
        ok = not _val_eq(val, spec["ne"])
    else:
        ok = True
    return AssertResult(spec, "artifact", ok,
                        "%s:%s=%r" % (rel, field, val),
                        "artifacts/%s: %s=%r" % (rel, field, val))


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
            if not found or not _val_eq(val, v):
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
                                "wire/%s" % os.path.basename(f))
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
                                                spec.get("path"), val))
    if kind == "grep":
        paths = _resolve_glob(spec.get("file", ""), base)
        paths = _wire_filter(paths, spec.get("where"))
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
        return AssertResult(spec, "wire.grep", ok,
                            "%s %s %r" % (os.path.basename(f),
                                          "lacks" if not present
                                          else "contains", needle),
                            "wire/%s grep %r" % (os.path.basename(f),
                                                 needle[:80]))
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
                            "wire: %d B vs %d B" % (sa, sb))
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
    cid = case["id"]
    run_dir = os.path.join(args.out, cid.lower())
    os.makedirs(run_dir, exist_ok=True)
    started = time.time()
    ctx = CaseCtx()
    fmt = case.get("output_format", "streaming-json")
    ctx.fmt = fmt
    results = []
    status = "PASS"

    def mark(status_, why=""):
        nonlocal status
        if status == "FAIL" and status_ != "FAIL":
            return
        status = status_
        if why:
            log("  %s -> %s (%s)" % (cid, status, why))

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
        home = HermeticHome(run_dir, args.live_home,
                            case.get("config_patch", {}), port,
                            keep=args.keep_home)
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
                home, home.cwd, sid, cur_model, seed["hist_tokens"],
                seed.get("hist_bytes", 0))
            ctx.session_dir = sdir
            v = validate_roundtrip(os.path.join(sdir, "chat_history.jsonl"))
            log("  %s: seeded %s lines=%d bytes=%d est_tokens=%d "
                "(target %d) roundtrip=OK" % (cid, sid, v, nbytes, est_tok,
                                              seed["hist_tokens"]))
            snapshot_history(ctx, "seed", home, home.cwd)

        driver = case.get("driver", "headless")
        if driver == "acp":
            acp = AcpSession(args.bin, home.home, home.cwd, cur_model,
                             os.path.join(run_dir, "acp.log"))
            acp.start()
            ctx.acp = acp
            log("  %s: ACP session %s model=%s" % (cid, acp.session_id,
                                                   cur_model))
            ctx.session_id = acp.session_id
            ctx.session_dir = home.session_dir_for(home.cwd, acp.session_id)
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
                    args.bin, home.home, home.cwd, model,
                    row.get("prompt", "Reply with exactly: HT1-OK"),
                    None, output_format=fmt, timeout_s=case.get(
                        "watchdog_s", 300),
                    capture_dir=ctx.capture_dir)
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
                           row_asserts=case.get("row_asserts"))

        for si, step in enumerate(case.get("steps", [])):
            op = step.get("op")
            if op == "turn":
                model = step.get("model", cur_model)
                if driver == "acp":
                    before = len(acp.updates)
                    resp, stop = acp.prompt(step["prompt"],
                                            timeout=step.get(
                                                "timeout_s",
                                                case.get("watchdog_s", 300)))
                    chunks = []
                    for u in acp.updates[before:]:
                        if u.get("sessionUpdate") == \
                                "agent_message_chunk":
                            c = u.get("content") or {}
                            if c.get("type") == "text":
                                chunks.append(c.get("text", ""))
                    rec = {"op": "acp_turn", "step": si, "model": model,
                           "stop": stop,
                           "error": resp.get("error"),
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
                            home.cwd, acp.session_id)
                else:
                    tr = run_headless_turn(
                        args.bin, home.home, home.cwd, model,
                        step["prompt"], ctx.session_id, output_format=fmt,
                        extra_args=extra,
                        timeout_s=step.get("timeout_s",
                                           case.get("watchdog_s", 300)),
                        kill_after_s=step.get("kill_after_s"),
                        capture_dir=ctx.capture_dir)
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
                            home.cwd, tr.session_id)
                    if tr.killed:
                        mark("FAIL" if step.get("expect_kill") is False
                             else status,
                             "turn killed (watchdog/kill_after)")
                    elif tr.exit_code != 0:
                        mark("FAIL", "turn exit=%s" % tr.exit_code)
                        ctx.crashed = True
                    ctx.last_exit = tr.exit_code
                snapshot_history(ctx, "turn%d" % si, home, home.cwd)
            elif op == "kill":
                model = step.get("model", cur_model)
                if driver == "acp":
                    acp.kill()
                    ctx.turns.append({"op": "kill", "step": si})
                else:
                    tr = run_headless_turn(
                        args.bin, home.home, home.cwd, model,
                        step.get("prompt", "KILL-PROBE"),
                        ctx.session_id, output_format=fmt, extra_args=extra,
                        timeout_s=60,
                        kill_after_s=step.get("after_s", 1.5),
                        capture_dir=ctx.capture_dir)
                    ctx.turns.append({"op": "kill", "step": si,
                                      "killed": tr.killed,
                                      "elapsed": tr.elapsed})
                    snapshot_history(ctx, "kill", home, home.cwd)
            elif op == "switch":
                cur_model = step["model"]
                if step.get("via", "acp") == "acp" and driver == "acp":
                    resp = acp.set_model(cur_model)
                    ctx.turns.append({"op": "switch", "step": si,
                                      "model": cur_model,
                                      "resp": str(resp)[:200]})
                    ctx.events.append({"type": "acp_set_model",
                                       "model": cur_model, "_line": 0})
                else:
                    ctx.turns.append({"op": "switch", "step": si,
                                      "model": cur_model, "via": "resume"})
            elif op == "compact":
                model = step.get("model", cur_model)
                if driver == "acp":
                    resp, stop = acp.prompt("/compact",
                                            timeout=case.get(
                                                "watchdog_s", 300))
                    ctx.turns.append({"op": "compact", "step": si,
                                      "stop": stop,
                                      "error": str(resp.get("error"))[:300]})
                    if "error" in resp:
                        ctx.events.append({"type": "acp_error",
                                           "error": resp["error"]})
                else:
                    tr = run_headless_turn(
                        args.bin, home.home, home.cwd, model, "/compact",
                        ctx.session_id, output_format=fmt, extra_args=extra,
                        timeout_s=case.get("watchdog_s", 300),
                        capture_dir=ctx.capture_dir)
                    ctx.turns.append({"op": "compact", "step": si,
                                      "exit": tr.exit_code,
                                      "killed": tr.killed,
                                      "text": ndjson_text(tr.events,
                                                          fmt)[:200]})
                    ctx.events.extend(tr.events)
                    if tr.exit_code not in (0,) and not tr.killed:
                        mark("FAIL", "compact exit=%s" % tr.exit_code)
                snapshot_history(ctx, "compact", home, home.cwd)
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
            results.append(check_ndjson(spec, ctx.events, fmt))
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
            for spec in assert_block.get("wire", []):
                if spec.get("kind") == "recon":
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
    return _finish(case, run_dir, ctx, results, status, started, None,
                   None, args, budget)


def _copy_session_evidence(home, ctx, run_dir):
    dst = os.path.join(run_dir, "session")
    if ctx.session_dir and os.path.isdir(ctx.session_dir) and \
            not os.path.exists(dst):
        keep = ["chat_history.jsonl", "summary.json", "events.jsonl",
                "compaction", "compaction_requests",
                "compaction_checkpoints", "subagents"]
        os.makedirs(dst, exist_ok=True)
        for k in keep:
            p = os.path.join(ctx.session_dir, k)
            if os.path.exists(p):
                shutil.copytree(p, os.path.join(dst, k),
                                dirs_exist_ok=True)
            elif os.path.isfile(p):
                shutil.copy(p, os.path.join(dst, k))


def _skip_row(case, why):
    return {"id": case["id"], "title": case.get("title", ""),
            "status": "SKIP", "duration_s": 0.0, "model_calls": 0,
            "turns": [], "asserts": [], "skipped": why,
            "run_dir": None, "snapshots": []}


def _finish(case, run_dir, ctx, results, status, started, wt, home, args,
            budget, row_asserts=None):
    if wt:
        wt.stop()
    if row_asserts:
        run_order = getattr(ctx, "row_order", [])
        for spec in row_asserts.get("wire", []):
            rm = spec.get("row_model")
            if rm and rm not in run_order:
                results.append(AssertResult(
                    spec, "wire.row-skip", True,
                    "row %s not in this run (filtered by --rows)" % rm,
                    "wire: skipped"))
                continue
            if rm:
                spec = dict(spec)
                spec["nth"] = run_order.index(rm)
            results.append(check_wire(spec, os.path.join(run_dir, "wire")))
    hard_fails = [r for r in results if not r.ok and not r.recon]
    if row_asserts and hard_fails and status == "PASS":
        status = "FAIL"
    budget.add(ctx.model_calls)
    row = {"id": case["id"], "title": case.get("title", ""),
           "status": status,
           "duration_s": round(time.time() - started, 1),
           "model_calls": ctx.model_calls,
           "session_id": ctx.session_id,
           "turns": ctx.turns,
           "asserts": [{"kind": r.kind, "ok": r.ok, "recon": r.recon,
                        "detail": r.detail, "evidence": r.evidence,
                        "spec": r.spec} for r in results],
           "snapshots": ctx.snapshots,
           "run_dir": run_dir}
    log("  %s: %s in %.1fs calls=%d" % (case["id"], status,
                                        row["duration_s"],
                                        ctx.model_calls))
    return row


# ---------------------------------------------------------------------------
# Redaction sweep
# ---------------------------------------------------------------------------

KEY_HEURISTIC = re.compile(
    r'(?i)(sk-[A-Za-z0-9_-]{16,}|AKIA[A-Z0-9]{16}|'
    r'(?:api[_-]?key|token)\s*[:=]\s*"[A-Za-z0-9._-]{24,}"'
    r'|user=[A-Za-z0-9]+:[A-Za-z0-9+/=]{20,}@)')


def redaction_sweep(root: str, ambient_key: str):
    """Grep the whole run dir for the ambient key + key-like heuristics.
    Returns (hits, details)."""
    hits = 0
    details = []
    for dirpath, dirnames, filenames in os.walk(root):
        for fn in filenames:
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
    order = ["PASS", "RECON", "FAIL", "BLOCKED", "SKIP"]
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
    md.append("")
    md.append("**Summary:** %s" % ", ".join(
        "%s=%d" % (k, v) for k, v in sorted(counts.items())))
    md.append("")
    md.append("| id | status | dur(s) | calls | title |")
    md.append("|---|---|---|---|---|")
    for r in rows_sorted:
        md.append("| %s | %s | %s | %s | %s |" % (
            r["id"], r["status"], r["duration_s"], r["model_calls"],
            r.get("title", "")[:60]))
    md.append("")
    for r in rows_sorted:
        md.append("## %s — %s" % (r["id"], r["status"]))
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
        if r["status"] == "FAIL":
            md.extend(slice_evidence(r, secrets=(env_meta.get("key", ""),)))
        md.append("")
    if sweep_details:
        md.append("## Redaction sweep details")
        md.extend("- " + d for d in sweep_details)
        md.append("")
    with open(os.path.join(out, "report.md"), "w") as fh:
        fh.write("\n".join(md))
    with open(os.path.join(out, "report.json"), "w") as fh:
        json.dump({"env": env_meta, "rows": rows_sorted,
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
        want = {x.upper() for x in args.case_ids}
        cases = [c for c in cases if c.get("id", "").upper() in want]
    return cases


class Budget:
    def __init__(self, limit):
        self.limit = limit
        self.used = 0

    def add(self, n):
        self.used += n


def main(argv=None):
    p = argparse.ArgumentParser(description="HT-1 L3 red-team runner")
    p.add_argument("case_ids", nargs="*",
                   help="case ids (RT-C1 ...); none = full matrix")
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
    a = p.parse_args(argv)
    a.wirecap = not a.no_wirecap
    if a.out is None:
        a.out = os.path.join(REPORT_ROOT, utc_ts())
    os.makedirs(a.out, exist_ok=True)

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
    rows = []
    for case in cases:
        log("=== %s: %s" % (case["id"], case.get("title", "")))
        rows.append(run_case(case, a, budget))
    sweep_hits, sweep_details = redaction_sweep(a.out, ambient)
    if sweep_hits:
        log("REDACTION SWEEP: %d HIT(S) — inspect before sharing"
            % sweep_hits)
    else:
        log("redaction sweep: 0 hits")
    md = write_report(a.out, rows, env_meta, a, sweep_hits, sweep_details)
    log("report: %s" % md)
    log("calls used: %d / %d" % (budget.used, budget.limit))
    bad = [r for r in rows if r["status"] in ("FAIL", "BLOCKED")]
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
