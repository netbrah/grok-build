#!/usr/bin/env python3
"""wstream — wire-streaming matrix runner.

PURPOSE
-------
Capture STREAMING behavior (the harness's real path) across the
model × api_backend × wire cross-product, using the dogfood hermetic-home
mechanism (live config copy + per-cell api_backend patch + wiretap2 ->
real proxy + headless streaming turn). Non-streaming probes (W10-B) hid
reasoning content on the vertex dialects; streaming is the harness's real
path, so this suite resolves the W10 open questions the harness can reach:
  - OQ-1  sol /responses native-vs-bridge (does the stream carry
          encrypted_content / reasoning deltas; what is the id encoding)
  - OQ-2  vertex-gemini reasoning content IN STREAMING
  - OQ-7  vertex-xai (grok-4.6) thinking content IN STREAMING (PILOT)
  - OQ-3  qwen budget-trap behavior IN STREAMING (empty text + max_tokens)
  - OQ-5  /messages per-class dispatch trace (which route the messages
          wire actually hits per backend class)
OQ-4 (thoughtSignature multi-turn) and OQ-10 (gemini naming) are
proxy-level google-dialect questions NOT reachable via the harness's three
api_backends (responses/messages/chat_completions) — noted, not tested here.

MECHANISM (pattern source — copied, NOT imported, to stay decoupled from the
actively-edited smoke/redteam lane):
  - Hermetic home + config-patch surgery: smoke/redteam/run.py
    _apply_config_patch (incl. the SWEEPFIX-64 append fix) + HermeticHome.
  - Wire capture: smoke/wiretap/wiretap.py (wiretap2, frame-fidelity
    resp-NNN.jsonl: one line per SSE frame as it streams).
  - Headless streaming turn: `bin -m <model> -p <prompt>
    --output-format streaming-json --always-approve` (redteam
    run_headless_turn pattern).
  - L1 env contract: provider vars unset, GROK_AUTH_EXPIRED=1, ambient
    CODEX_LLM_PROXY_KEY only (never echoed).

Stdlib only. No pip deps. No cargo.

Usage:
  python3 smoke/wstream/run.py --selftest                # offline gate
  python3 smoke/wstream/run.py --cell grok-resp          # run one cell
  python3 smoke/wstream/run.py --cells grok-resp,sol-resp
  python3 smoke/wstream/run.py --native                  # all native cells
  python3 smoke/wstream/run.py                           # all enabled cells
Env:
  WSTREAM_BIN=<path>          (default target/release/grok-responses)
  WSTREAM_UPSTREAM=<url>      (default llm-proxy)
  WSTREAM_TIMEOUT_S=<n>       (default 180, per-turn kill budget)
"""
import argparse
import glob as globmod
import hashlib
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
import urllib.parse
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
WIRETAP = os.path.abspath(os.path.join(HERE, "..", "wiretap", "wiretap.py"))
CELLS_DIR = os.path.join(HERE, "cells")
MANIFEST = os.path.join(HERE, "manifest.json")
REPORT_ROOT = os.path.join(HERE, "report")

DEFAULT_BIN = os.path.join(REPO_ROOT, "target", "release", "grok-responses")
DEFAULT_LIVE_HOME = os.path.expanduser("~/.grok")
DEFAULT_UPSTREAM = "https://llm-proxy-api.ai.eng.netapp.com"
DEFAULT_TIMEOUT_S = 180

# L1 env contract (redteam): provider vars unset so the harness rides the
# hermetic config's env_key (CODEX_LLM_PROXY_KEY), not ambient provider creds.
PROVIDER_VARS_UNSET = [
    "OPENAI_API_KEY", "OPENAI_BASE_URL",
    "ANTHROPIC_API_KEY", "ANTHROPIC_BASE_URL",
]

# api_backend (config value) -> /v1/<route> the wire should actually hit.
BACKEND_TO_ROUTE = {
    "responses": "responses",
    "messages": "messages",
    "chat_completions": "chat/completions",
}

# A reasoning-inducing prompt: bounded (modular exponentiation, ~6 squaring
# steps) so output is small, but reliably elicits thinking on every
# reasoning family. No tool calls needed.
DEFAULT_PROMPT = (
    "Compute 7^8432 mod 5557 using modular exponentiation. Show each "
    "squaring step briefly, then state the final remainder on its own "
    "line. Do not use any tools."
)


def utc_ts():
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def sha256_12(value):
    if isinstance(value, bytes):
        return hashlib.sha256(value).hexdigest()[:12]
    return hashlib.sha256(value.encode("utf-8", "replace")).hexdigest()[:12]


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def log(msg):
    print("[%s] %s" % (time.strftime("%H:%M:%S"), msg), flush=True)


# ---------------------------------------------------------------------------
# Config-patch surgery (pattern source: smoke/redteam/run.py
# _apply_config_patch, incl. the SWEEPFIX-64 append fix). Self-contained
# copy — NOT imported — to stay decoupled from the actively-edited redteam
# lane (only touch smoke/wstream/).
# ---------------------------------------------------------------------------

def _toml_scalar(v):
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, int):
        return str(v)
    if isinstance(v, float):
        return repr(v)
    if isinstance(v, str):
        return json.dumps(v)
    raise ValueError("unsupported config_patch scalar: %r" % (v,))


def _split_toml_dotted(name):
    parts = re.split(r'\.(?=(?:[^"]*"[^"]*")*[^"]*$)', name)
    return [p.strip().strip('"') if p.strip().startswith('"')
            else p.strip() for p in parts]


def _header_name(parts):
    out = []
    for part in parts:
        out.append(part if re.fullmatch(r"[A-Za-z0-9_-]+", part)
                   else json.dumps(part))
    return ".".join(out)


def _split_patch_key(key):
    if "/" in key:
        parts = [p for p in key.split("/") if p]
    else:
        parts = _split_toml_dotted(key)
    return parts


def apply_config_patch(cfg_text, patch):
    """Apply a {key: scalar} patch to TOML text by section surgery.

    Key form: `section.../field` (slash-separated; needed for model ids
    with dots: model/qwen3.8-27b/api_backend). Finds the target section and
    sets the key in place (replacing an existing assignment) or appends a
    new section at EOF. Deterministic; no TOML writer dependency.
    """
    for key, value in patch.items():
        parts = _split_patch_key(key)
        if len(parts) < 2:
            raise ValueError("config_patch key needs section+field: %r" % key)
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
            cfg_text += ("\n[%s]\n%s" % (_header_name(section_parts), line))
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
                # SWEEPFIX-64: insert at the END OF THE PARENT'S OWN DIRECT
                # CONTENT — immediately before the first subtable header of
                # any kind ([a.b] or [[a.b]]) that follows the section
                # header, or at the section end when there are no subtables.
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


def _apply_no_watch(cfg_text):
    """Add --no-watch to the codegraph MCP args in the COPIED config (the
    codegraph server must not spawn a watcher into the worktree .codegraph/).
    Idempotent. No-op if the section is absent. Returns (cfg, applied)."""
    m = re.search(r"\[mcp_servers\.codegraph\](.*?)(?=\n\[|\Z)", cfg_text, re.S)
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
    new_section = m.group(1)[:am.start(1)] + inner + m.group(1)[am.end(1):]
    return cfg_text[:m.start(1)] + new_section + cfg_text[m.end(1):], True


def align_models_cache(cache_path, wire_port):
    """Align the copied live catalog cache to the hermetic scope (origin /
    identity / renewed_at) so -m <model> resolves offline, race-free."""
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
        h = hashlib.sha256()
        for part in ("models-api-key", xai_key, ""):
            h.update(part.encode())
            h.update(b"\x00")
        cache["identity"] = h.hexdigest()
    with open(cache_path, "w") as fh:
        json.dump(cache, fh, indent=2)


def encode_cwd_dirname(cwd):
    return urllib.parse.quote(cwd, safe="")


class HermeticHome:
    """Tempdir GROK_HOME: live config copy + turn_summary off + per-cell
    api_backend patch + --no-watch, with base_urls rewritten to the local
    wiretap and the catalog cache aligned to the hermetic scope. The derived
    config.toml is kept in the report dir as the FROZEN per-run config."""

    def __init__(self, root, live_home, config_patch, wire_port):
        self.home = os.path.join(root, "home")
        self.cwd = os.path.join(root, "cwd")
        os.makedirs(self.home, mode=0o700, exist_ok=True)
        os.makedirs(self.cwd, exist_ok=True)
        with open(os.path.join(live_home, "config.toml")) as fh:
            cfg = fh.read()
        # Disable the display-only per-turn side-calls (cost) first, then the
        # cell patch (api_backend etc.) so the cell patch takes precedence.
        cfg = apply_config_patch(cfg, {"features/turn_summary": False})
        cfg = apply_config_patch(cfg, config_patch or {})
        cfg, self.no_watch_applied = _apply_no_watch(cfg)
        # proxy-auth-stub: copy + rewrite the path.
        stub_src = os.path.join(live_home, "proxy-auth-stub.sh")
        if os.path.isfile(stub_src):
            stub_dst = os.path.join(self.home, "proxy-auth-stub.sh")
            shutil.copy(stub_src, stub_dst)
            os.chmod(stub_dst, 0o755)
            cfg = cfg.replace(stub_src, stub_dst)
        if wire_port:
            proxy_base = "http://127.0.0.1:%d/v1" % wire_port
            cfg = re.sub(r'^base_url\s*=\s*"[^"]*"',
                         'base_url = "%s"' % proxy_base, cfg, flags=re.M)
            cfg = re.sub(r'^models_base_url\s*=\s*"[^"]*"',
                         'models_base_url = "%s"' % proxy_base, cfg, flags=re.M)
        with open(os.path.join(self.home, "config.toml"), "w") as fh:
            fh.write(cfg)
        cache_src = os.path.join(live_home, "models_cache.json")
        if os.path.isfile(cache_src):
            cache_dst = os.path.join(self.home, "models_cache.json")
            shutil.copy(cache_src, cache_dst)
            align_models_cache(cache_dst, wire_port)

    def session_dir_for(self, cwd, sid):
        return os.path.join(self.home, "sessions",
                            encode_cwd_dirname(cwd), sid)


def build_env(home):
    env = {k: v for k, v in os.environ.items() if k not in PROVIDER_VARS_UNSET}
    env["GROK_AUTH_EXPIRED"] = "1"
    env["GROK_HOME"] = home
    return env


class Wiretap:
    def __init__(self, port, upstream, capture_dir, ambient_key):
        self.port = port
        self.capture_dir = capture_dir
        env = dict(os.environ)
        if ambient_key:
            # HYG-1: hand the key to wiretap via env, never argv.
            env["WIRETAP_AMBIENT_KEY"] = ambient_key
        self.proc = subprocess.Popen(
            [sys.executable, WIRETAP, "--port", str(port),
             "--upstream", upstream, "--capture", capture_dir],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
        self.out_log = open(capture_dir + ".wiretap-stdout.log", "w")

    def wait_ready(self, timeout=20):
        end = time.time() + timeout
        while time.time() < end:
            try:
                with socket.create_connection(("127.0.0.1", self.port), 0.5):
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
# Headless streaming turn
# ---------------------------------------------------------------------------

def run_headless_turn(bin_path, home, cwd, model, prompt,
                      output_format="streaming-json", resume_sid=None,
                      timeout_s=DEFAULT_TIMEOUT_S):
    args = [bin_path, "-m", model, "-p", prompt,
            "--output-format", output_format, "--always-approve"]
    if resume_sid:
        args += ["--resume", resume_sid]
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

    killer = threading.Timer(timeout_s, _kill)
    killer.daemon = True
    killer.start()
    try:
        out, err = proc.communicate()
    finally:
        killer.cancel()
    elapsed = time.time() - start
    return {"exit_code": proc.returncode, "killed": killed,
            "elapsed": elapsed, "stdout": out or "", "stderr": err or ""}


def parse_ndjson(text):
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


def ndjson_session_id(events, fmt):
    if fmt == "streaming-messages-json":
        for e in events:
            if e.get("type") == "system" and e.get("session_id"):
                return e["session_id"]
        return None
    for e in reversed(events):
        if e.get("type") == "end" and e.get("sessionId"):
            return e["sessionId"]
    return None


# ---------------------------------------------------------------------------
# Wire analysis (the new value): parse the wiretap capture into a per-cell
# analysis the invariant/observe engine reads.
# ---------------------------------------------------------------------------

def _route_from_path(path):
    p = path.split("?", 1)[0].rstrip("/")
    for r in ("responses", "messages", "chat/completions"):
        if p.endswith("/" + r):
            return r
    return None


def _sse_data(frame_text):
    """A raw SSE frame is 'event: X\\ndata: {...}' (or just 'data: {...}').
    Return (parsed_json_or_None, raw_data_string)."""
    data_lines = []
    for ln in frame_text.splitlines():
        if ln.startswith("data:"):
            data_lines.append(ln[len("data:"):].strip())
    if not data_lines:
        return None, frame_text.strip()
    data = "\n".join(data_lines)
    if data == "[DONE]":
        return "[DONE]", data
    try:
        return json.loads(data), data
    except Exception:
        return None, data


def _parse_resp_jsonl(resp_path):
    status = None
    frames = []  # list of raw frame texts, in order
    with open(resp_path) as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                d = json.loads(line)
            except Exception:
                continue
            if "status" in d:
                status = d.get("status")
            elif "frame" in d:
                frames.append(d["frame"])
    return status, frames


def analyze_stream(frames):
    """Per-call stream analysis over the raw SSE frame texts."""
    frame_types = {}
    reasoning_frames = 0
    thinking_block = False
    encrypted_content = False
    finish = None
    text_parts = []
    reasoning_sample = None
    usage = None
    raw_all = "\n".join(frames)
    thought_signature = ("thoughtSignature" in raw_all
                         or "thought_signature" in raw_all)
    for ft in frames:
        data, raw = _sse_data(ft)
        if not isinstance(data, dict):
            if data == "[DONE]":
                continue
            continue
        t = data.get("type")
        if isinstance(t, str):
            frame_types[t] = frame_types.get(t, 0) + 1
        # --- responses wire ---
        if isinstance(t, str) and "reasoning" in t:
            delta = data.get("delta") or ""
            if delta:
                reasoning_frames += 1
                if reasoning_sample is None:
                    reasoning_sample = str(delta)[:160]
        if isinstance(t, str) and t in (
                "response.output_item.added", "response.output_item.done"):
            item = data.get("item") or {}
            if item.get("type") == "reasoning":
                if item.get("encrypted_content") is not None:
                    encrypted_content = True
                summ = item.get("summary")
                if isinstance(summ, list):
                    for s in summ:
                        txt = s.get("text", "") if isinstance(s, dict) else ""
                        if txt and reasoning_sample is None:
                            reasoning_sample = txt[:160]
                if (item.get("encrypted_content") is not None
                        or isinstance(summ, list) and summ):
                    reasoning_frames += 1
        if "encrypted_content" in raw:
            encrypted_content = True
        if t == "response.completed":
            resp = data.get("response") or {}
            usage = resp.get("usage")
            if resp.get("status_reason"):
                finish = resp.get("status_reason")
            elif resp.get("incomplete_details"):
                finish = "max_output_tokens"
        if isinstance(t, str) and t.endswith(".output_text.delta"):
            text_parts.append(data.get("delta", ""))
        # --- messages wire ---
        if t == "content_block_start" and \
                (data.get("content_block") or {}).get("type") == "thinking":
            thinking_block = True
        if t == "content_block_delta":
            d2 = data.get("delta") or {}
            if d2.get("type") == "thinking_delta":
                thinking_block = True
                txt = d2.get("thinking", "")
                reasoning_frames += 1
                if txt and reasoning_sample is None:
                    reasoning_sample = txt[:160]
            elif d2.get("type") == "text_delta":
                text_parts.append(d2.get("text", ""))
        if t == "message_delta":
            usage = data.get("usage")
        if t == "message_stop":
            if finish is None:
                finish = "stop"
        # --- chat/completions wire ---
        for ch in data.get("choices") or []:
            delta = ch.get("delta") or {}
            rc = delta.get("reasoning_content")
            if rc:
                reasoning_frames += 1
                if reasoning_sample is None:
                    reasoning_sample = rc[:160]
            if delta.get("content"):
                text_parts.append(delta["content"])
            fr = ch.get("finish_reason")
            if fr:
                finish = fr
    return {
        "n_frames": len(frames),
        "frame_types": frame_types,
        "reasoning_frames": reasoning_frames,
        "thinking_block": thinking_block,
        "encrypted_content": encrypted_content,
        "thought_signature": thought_signature,
        "reasoning_sample": reasoning_sample,
        "finish": finish,
        "usage": usage,
        "final_text": "".join(text_parts),
    }


def analyze_capture(capture_dir, expected_route, cell_id):
    """Aggregate the wiretap capture for a cell into a flat analysis the
    invariant/observe engine reads."""
    reqs = sorted(globmod.glob(os.path.join(capture_dir, "req-*.json")))
    resp_files = sorted(globmod.glob(os.path.join(capture_dir, "resp-*.jsonl")))
    resp_by_n = {}
    for rp in resp_files:
        m = re.search(r"resp-(\d+)\.jsonl$", rp)
        if m:
            resp_by_n[int(m.group(1))] = rp
    calls = []
    for rf in reqs:
        try:
            with open(rf) as fh:
                d = json.load(fh)
        except Exception:
            continue
        if d.get("method") != "POST":
            continue
        route = _route_from_path(d.get("path", ""))
        if route is None:
            continue  # /v1/models etc.
        body = d.get("body") or {}
        m = re.search(r"req-(\d+)\.json$", rf)
        n = int(m.group(1)) if m else None
        sa = {"route": route, "req_model": body.get("model"),
              "req_stream": body.get("stream"),
              "req_max_tokens": body.get("max_tokens")
                                 or (body.get("max_output_tokens"))}
        rp = resp_by_n.get(n)
        if rp:
            status, frames = _parse_resp_jsonl(rp)
            sa["status"] = status
            sa.update(analyze_stream(frames))
        else:
            sa["status"] = None
            sa.update({"n_frames": 0, "frame_types": {},
                       "reasoning_frames": 0, "thinking_block": False,
                       "encrypted_content": False, "thought_signature": False,
                       "reasoning_sample": None, "finish": None,
                       "usage": None, "final_text": ""})
        calls.append(sa)

    # --- classify main turn vs display side-calls ---
    # The harness fires a display-only session-title side-call on the
    # responses wire (tool_choice.function 'session_title',
    # max_output_tokens ~100) in addition to the real turn (max_output_tokens
    # = the model's max_completion_tokens). The fidelity questions are about
    # the MAIN turn; a non-200 side call is a finding, not a main-turn
    # failure (it 500s on the responses wire: litellm rejects the
    # chat-completions-style tool_choice the title call sends).
    def _maxtok(c):
        return c.get("req_max_tokens") or 0
    ordered = sorted(calls, key=_maxtok, reverse=True)
    main = ordered[0] if ordered else None
    side = ordered[1:]

    routes = sorted({c["route"] for c in calls})
    # route = the MAIN turn's route (the fidelity question is about the main
    # turn). A side call may use a different model + wire (the session-title
    # call rides grok-4.6 / /responses even when the main turn is on
    # /messages) — that is recorded separately, not folded into `route`.
    route = main.get("route") if main else \
        (routes[0] if len(routes) == 1 else ("mixed" if routes else "none"))
    status = main.get("status") if main else None
    streams = [c.get("req_stream") for c in calls]
    all_stream = all(s is True for s in streams) if streams else False
    any_stream = any(s is True for s in streams)
    main_text = main.get("final_text", "") if main else ""
    budget_trap = bool(main) and (not main_text.strip()) and \
        (main.get("finish") in ("max_output_tokens", "max_tokens"))
    usage = main.get("usage") if main else None
    side_errors = [c.get("status") for c in side
                   if c.get("status") not in (200, None)]
    # reasoning_tokens from usage (the frame-level reasoning_frames and the
    # usage-level reasoning_tokens answer DIFFERENT questions: gemini counts
    # 12444 reasoning tokens but streams ZERO reasoning frames — the bridge
    # strips the content, only the count survives).
    det = (usage or {}).get("output_tokens_details") or {}
    reasoning_tokens = det.get("reasoning_tokens", det.get("thinking_tokens"))
    routes_mixed = len(routes) > 1

    flat = {
        "cell": cell_id,
        "route": route,
        "expected_route": expected_route,
        "routes_mixed": routes_mixed,
        "status": status,
        "n_model_calls": len(calls),
        "n_side_calls": len(side),
        "side_call_errors": side_errors,
        "all_stream": all_stream,
        "any_stream": any_stream,
        "req_stream_values": streams,
        "reasoning_present": bool(main) and main.get("reasoning_frames", 0) > 0,
        "reasoning_frames": main.get("reasoning_frames", 0) if main else 0,
        "reasoning_tokens": reasoning_tokens,
        "reasoning_encrypted_content": bool(main) and main.get("encrypted_content"),
        "reasoning_thinking_block": bool(main) and main.get("thinking_block"),
        "reasoning_thought_signature": bool(main) and main.get("thought_signature"),
        "reasoning_sample": (main or {}).get("reasoning_sample"),
        "finish": (main or {}).get("finish"),
        "budget_trap": budget_trap,
        "final_text_len": len(main_text),
        "final_text_nonempty": main_text.strip() != "",
        "usage": usage,
        "n_frames_total": main.get("n_frames", 0) if main else 0,
    }
    return flat, calls


# ---------------------------------------------------------------------------
# Invariant / observe engine
# ---------------------------------------------------------------------------

def check_invariants(flat, invariants):
    results = []
    for key, expected in invariants.items():
        if key.endswith("_min"):
            field = key[:-4]
            actual = flat.get(field)
            ok = (actual is not None) and (actual >= expected)
            results.append({"key": key, "expected": ">= %s" % expected,
                            "actual": actual, "ok": bool(ok)})
        else:
            actual = flat.get(key)
            ok = (actual == expected)
            results.append({"key": key, "expected": expected,
                            "actual": actual, "ok": bool(ok)})
    return results


def collect_observations(flat, observe):
    return {k: flat.get(k) for k in observe}


# ---------------------------------------------------------------------------
# Report writer
# ---------------------------------------------------------------------------

def write_cell_report(cell, flat, calls, inv_results, obs, report_dir,
                      derived_config, bin_sha, wire_port, upstream,
                      key_sha, raw_key_hits, turns_meta):
    os.makedirs(report_dir, exist_ok=True)
    verdict = all(r["ok"] for r in inv_results) if inv_results else False
    result = {
        "cell": cell["cell"],
        "model": cell.get("model"),
        "api_backend": cell.get("api_backend"),
        "backend_class": cell.get("backend_class"),
        "verdict": "PASS" if verdict else "FAIL",
        "invariants": inv_results,
        "observations": obs,
        "analysis": flat,
        "calls": calls,
        "turns": turns_meta,
        "binary_sha256_12": bin_sha,
        "wire_port": wire_port,
        "upstream": upstream,
        "key_sha256_12": key_sha,
        "raw_key_hits": raw_key_hits,
        "ts": utc_ts(),
    }
    with open(os.path.join(report_dir, "result.json"), "w") as fh:
        json.dump(result, fh, indent=2)

    lines = []
    lines.append("# wstream cell: %s" % cell["cell"])
    lines.append("")
    lines.append("- model: `%s`" % cell.get("model"))
    lines.append("- api_backend: `%s` (%s)" % (
        cell.get("api_backend"), cell.get("backend_class")))
    lines.append("- expected wire: `%s`" % flat.get("expected_route"))
    lines.append("- open questions: %s" % ", ".join(cell.get("open_questions", [])))
    lines.append("- verdict: **%s**" % result["verdict"])
    lines.append("- binary: sha256_12 %s · wiretap :%d → %s" % (
        bin_sha, wire_port, upstream))
    lines.append("- streaming wire calls (stream:true): %s (%d/%d)" % (
        "ALL" if flat.get("all_stream") else
        ("some" if flat.get("any_stream") else "NONE"),
        sum(1 for s in flat.get("req_stream_values", []) if s is True),
        flat.get("n_model_calls", 0)))
    if flat.get("n_side_calls"):
        lines.append("- side calls (display-only, e.g. session-title): %d "
                     "(non-200: %s)" % (
                         flat.get("n_side_calls", 0),
                         flat.get("side_call_errors") or "none"))
    lines.append("- reasoning (main turn): frames=%s tokens=%s "
                 "encrypted=%s thinking_block=%s" % (
                     flat.get("reasoning_frames"), flat.get("reasoning_tokens"),
                     flat.get("reasoning_encrypted_content"),
                     flat.get("reasoning_thinking_block")))
    lines.append("")
    lines.append("## Invariants (must pass)")
    for r in inv_results:
        lines.append("- [%s] `%s` = %s (expected %s)" % (
            "PASS" if r["ok"] else "FAIL", r["key"], r["actual"],
            r["expected"]))
    lines.append("")
    lines.append("## Observations (OQ capture)")
    for k, v in obs.items():
        lines.append("- `%s` = %s" % (k, json.dumps(v)))
    lines.append("")
    lines.append("## Per-call detail")
    for i, c in enumerate(calls, 1):
        lines.append("- call %d: route=`%s` model=`%s` stream=%s status=%s "
                     "frames=%d reasoning_frames=%d finish=%s" % (
                         i, c.get("route"), c.get("req_model"),
                         c.get("req_stream"), c.get("status"),
                         c.get("n_frames", 0), c.get("reasoning_frames", 0),
                         c.get("finish")))
    if flat.get("reasoning_sample"):
        lines.append("")
        lines.append("## Reasoning sample (truncated)")
        lines.append("```")
        lines.append(flat["reasoning_sample"])
        lines.append("```")
    with open(os.path.join(report_dir, "report.md"), "w") as fh:
        fh.write("\n".join(lines) + "\n")

    # The derived (frozen-for-this-run) config, for the record.
    with open(os.path.join(report_dir, "derived-config.toml"), "w") as fh:
        fh.write(derived_config)
    return result


def raw_key_hits_in(report_dir, key):
    if not key:
        return 0
    n = 0
    for root, _dirs, files in os.walk(report_dir):
        for f in files:
            p = os.path.join(root, f)
            try:
                with open(p, "rb") as fh:
                    if key.encode() in fh.read():
                        n += 1
            except Exception:
                continue
    return n


# ---------------------------------------------------------------------------
# Cell runner
# ---------------------------------------------------------------------------

def run_cell(cell, bin_path, live_home, upstream, ambient_key, timeout_s,
             run_ts):
    cell_id = cell["cell"]
    model = cell["model"]
    backend = cell["api_backend"]
    expected_route = BACKEND_TO_ROUTE.get(backend, backend)
    turns = cell.get("turns", 1)
    prompt = cell.get("prompt", DEFAULT_PROMPT)
    output_format = cell.get("output_format", "streaming-json")

    report_dir = os.path.join(REPORT_ROOT, run_ts, cell_id)
    capture_dir = os.path.join(report_dir, "capture")
    os.makedirs(capture_dir, exist_ok=True)
    os.chmod(report_dir, 0o700)

    log("cell %s: model=%s backend=%s wire=%s turns=%d" % (
        cell_id, model, backend, expected_route, turns))

    port = free_port()
    wt = Wiretap(port, upstream, capture_dir, ambient_key)
    if not wt.wait_ready():
        log("cell %s: wiretap not ready on :%d — ABORT" % (cell_id, port))
        wt.stop()
        return {"cell": cell_id, "verdict": "ABORT",
                "reason": "wiretap not ready"}

    home = HermeticHome(report_dir, live_home,
                        {"model/%s/api_backend" % model: backend}, port)

    bin_sha = sha256_12(_read_bin(bin_path))
    key_sha = sha256_12(ambient_key) if ambient_key else "none"

    turns_meta = []
    resume_sid = None
    last = None
    try:
        for t in range(1, turns + 1):
            args_resume = resume_sid
            last = run_headless_turn(bin_path, home.home, home.cwd, model,
                                     prompt, output_format=output_format,
                                     resume_sid=args_resume,
                                     timeout_s=timeout_s)
            events = parse_ndjson(last["stdout"])
            sid = ndjson_session_id(events, output_format)
            if sid and t == 1:
                resume_sid = sid
            with open(os.path.join(report_dir,
                                   "turn_%d.ndjson" % t), "w") as fh:
                fh.write(last["stdout"])
            with open(os.path.join(report_dir,
                                   "turn_%d.stderr" % t), "w") as fh:
                fh.write(last["stderr"])
            turns_meta.append({
                "turn": t, "exit_code": last["exit_code"],
                "killed": last["killed"], "elapsed": round(last["elapsed"], 2),
                "session_id": sid,
                "n_events": len(events),
            })
            log("cell %s: turn %d exit=%s killed=%s elapsed=%.1fs sid=%s" % (
                cell_id, t, last["exit_code"], last["killed"],
                last["elapsed"], (sid or "")[:8]))
    finally:
        wt.stop()

    flat, calls = analyze_capture(capture_dir, expected_route, cell_id)
    inv_results = check_invariants(flat, cell.get("invariants", {}))
    obs = collect_observations(flat, cell.get("observe", []))

    derived_config = ""
    cfg_path = os.path.join(home.home, "config.toml")
    if os.path.isfile(cfg_path):
        with open(cfg_path) as fh:
            derived_config = fh.read()
    hits = raw_key_hits_in(report_dir, ambient_key)

    result = write_cell_report(cell, flat, calls, inv_results, obs,
                               report_dir, derived_config, bin_sha, port,
                               upstream, key_sha, hits, turns_meta)
    result["report_dir"] = report_dir
    if hits:
        log("cell %s: REDACTION VIOLATION: %d raw-key file(s)" % (
            cell_id, hits))
    log("cell %s: verdict=%s" % (cell_id, result["verdict"]))
    return result


def _read_bin(path):
    try:
        with open(path, "rb") as fh:
            return fh.read()
    except Exception:
        return b""


# ---------------------------------------------------------------------------
# Manifest / cells
# ---------------------------------------------------------------------------

def load_manifest():
    with open(MANIFEST) as fh:
        return json.load(fh)


def load_cell(cell_id):
    with open(os.path.join(CELLS_DIR, cell_id, "cell.json")) as fh:
        return json.load(fh)


def select_cells(args):
    man = load_manifest()
    by_id = {c["cell"]: c for c in man.get("cells", [])}
    if args.cells:
        ids = [c.strip() for c in args.cells.split(",") if c.strip()]
    elif args.cell:
        ids = [args.cell]
    elif args.native:
        ids = [c["cell"] for c in man.get("cells", [])
               if c.get("backend_class") == "native"]
    elif args.enabled:
        ids = [c["cell"] for c in man.get("cells", [])
               if c.get("enabled", True)]
    else:
        ids = [c["cell"] for c in man.get("cells", [])
               if c.get("enabled", True)]
    out = []
    for cid in ids:
        if cid not in by_id:
            log("cell %s not in manifest — skipped" % cid)
            continue
        out.append(load_cell(cid))
    return out, man


# ---------------------------------------------------------------------------
# Self-test (offline gate: no proxy, no binary)
# ---------------------------------------------------------------------------

def selftest():
    import tempfile
    checks = []

    def check(name, ok, detail=""):
        checks.append((name, ok, detail))
        print("  [%s] %s %s" % ("ok" if ok else "FAIL", name, detail))

    # 1. Manifest + every cell.json parse + schema sanity.
    man = load_manifest()
    check("manifest-parses", isinstance(man, dict) and man.get("cells"),
          "%d cells" % len(man.get("cells", [])))
    valid_backends = set(BACKEND_TO_ROUTE)
    all_ok = True
    for c in man.get("cells", []):
        cell = load_cell(c["cell"])
        ok = (cell.get("cell") == c["cell"]
              and cell.get("model")
              and cell.get("api_backend") in valid_backends
              and isinstance(cell.get("invariants"), dict)
              and isinstance(cell.get("observe"), list))
        if not ok:
            all_ok = False
            check("cell-%s-schema" % c["cell"], False)
    check("all-cells-schema", all_ok)

    # 2. Config-patch surgery on a fixture: set a model api_backend + a new
    #    section, verify placement (SWEEPFIX-64: before a trailing [[..]]).
    fixture = (
        '[model."qwen3.8-27b"]\n'
        'api_backend = "responses"\n'
        'model_family = "qwen"\n'
        '\n'
        '[[model."qwen3.8-27b".reasoning_efforts]]\n'
        'name = "low"\n'
        '\n'
        '[endpoints]\n'
        'default_api_backend = "responses"\n'
    )
    patched = apply_config_patch(
        fixture, {"model/qwen3.8-27b/api_backend": "messages"})
    # The MODEL's api_backend line is replaced in place (exactly one
    # `api_backend = "messages"`), and the [endpoints] default is untouched.
    # (Note: the substring `api_backend = "responses"` still appears once —
    # inside `default_api_backend = "responses"` — so we assert on the exact
    # default line, not a bare substring.)
    check("patch-replace-in-place",
          patched.count('api_backend = "messages"') == 1
          and 'default_api_backend = "responses"' in patched)
    patched2 = apply_config_patch(
        fixture, {"model/qwen3.8-27b/context_window": 12000})
    # New key must land BEFORE the [[...]] subtable (SWEEPFIX-64).
    idx_key = patched2.find("context_window = 12000")
    idx_sub = patched2.find("[[model.")
    check("patch-append-before-subtable",
          idx_key != -1 and idx_sub != -1 and idx_key < idx_sub,
          "key@%d subtable@%d" % (idx_key, idx_sub))

    # 3. SSE frame parsing + per-wire reasoning detection on canned streams.
    #    responses wire: reasoning summary delta + encrypted reasoning item.
    resp_frames = [
        'event: response.created\ndata: {"type":"response.created"}\n',
        'event: response.reasoning_summary_text.delta\n'
        'data: {"type":"response.reasoning_summary_text.delta",'
        '"delta":"let me think"}\n',
        'event: response.output_item.added\n'
        'data: {"type":"response.output_item.added",'
        '"item":{"type":"reasoning","encrypted_content":"enc_abc",'
        '"summary":[{"text":"reasoning text"}]}}\n',
        'event: response.output_text.delta\n'
        'data: {"type":"response.output_text.delta","delta":"42"}\n',
        'event: response.completed\n'
        'data: {"type":"response.completed","response":'
        '{"status_reason":null,"usage":{"output_tokens":10}}}\n',
    ]
    sa = analyze_stream(resp_frames)
    check("resp-reasoning-detected", sa["reasoning_frames"] >= 1,
          "frames=%d" % sa["reasoning_frames"])
    check("resp-encrypted-content", sa["encrypted_content"] is True)
    check("resp-text", sa["final_text"] == "42")
    check("resp-finish", sa["finish"] is None)  # no status_reason/incomplete

    #    messages wire: claude thinking block + text.
    msg_frames = [
        'data: {"type":"message_start","message":{"role":"assistant"}}\n',
        'data: {"type":"content_block_start","index":0,'
        '"content_block":{"type":"thinking"}}\n',
        'data: {"type":"content_block_delta","index":0,'
        '"delta":{"type":"thinking_delta","thinking":"hmm"}}\n',
        'data: {"type":"content_block_start","index":1,'
        '"content_block":{"type":"text","text":""}}\n',
        'data: {"type":"content_block_delta","index":1,'
        '"delta":{"type":"text_delta","text":"answer"}}\n',
        'data: {"type":"message_delta","usage":{"output_tokens":5,'
        '"output_tokens_details":{"thinking_tokens":3}}}\n',
        'data: {"type":"message_stop"}\n',
    ]
    sa = analyze_stream(msg_frames)
    check("msg-thinking-block", sa["thinking_block"] is True)
    check("msg-reasoning-detected", sa["reasoning_frames"] >= 1)
    check("msg-text", sa["final_text"] == "answer")
    check("msg-usage", (sa["usage"] or {}).get("output_tokens") == 5)

    #    chat wire: reasoning_content delta + finish_reason.
    chat_frames = [
        'data: {"choices":[{"delta":{"reasoning_content":"step1"}}]}\n',
        'data: {"choices":[{"delta":{"content":"42"},'
        '"finish_reason":"stop"}]}\n',
        'data: [DONE]\n',
    ]
    sa = analyze_stream(chat_frames)
    check("chat-reasoning-detected", sa["reasoning_frames"] >= 1)
    check("chat-text", sa["final_text"] == "42")
    check("chat-finish", sa["finish"] == "stop")

    #    budget trap: empty text + max_tokens finish (chat wire).
    trap_frames = [
        'data: {"choices":[{"delta":{"reasoning_content":"x"}}]}\n',
        'data: {"choices":[{"delta":{},"finish_reason":"length"}]}\n',
        'data: [DONE]\n',
    ]
    sa = analyze_stream(trap_frames)
    check("trap-no-text", sa["final_text"] == "")

    # 4. route derivation.
    check("route-responses",
          _route_from_path("/v1/responses") == "responses")
    check("route-messages", _route_from_path("/v1/messages") == "messages")
    check("route-chat",
          _route_from_path("/v1/chat/completions") == "chat/completions")
    check("route-models-ignored",
          _route_from_path("/v1/models") is None)

    # 5. Invariant engine.
    flat = {"status": 200, "n_model_calls": 2, "reasoning_present": True,
            "route": "responses", "all_stream": True}
    inv = check_invariants(flat, {"status": 200,
                                  "n_model_calls_min": 1,
                                  "route": "responses",
                                  "reasoning_present": True})
    check("invariants-pass", all(r["ok"] for r in inv))
    inv = check_invariants(flat, {"status": 400})
    check("invariants-fail", not all(r["ok"] for r in inv))

    failed = [c for c in checks if not c[1]]
    print("\nSELFTEST %s (%d checks, %d failed)" % (
        "GREEN" if not failed else "RED", len(checks), len(failed)))
    return 0 if not failed else 1


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def reanalyze(run_ts):
    """Re-score an existing run's captures (no proxy, no binary) after a
    runner analysis fix. Reuses the saved capture/ + derived-config.toml + the
    run metadata from the original result.json; rewrites per-cell
    report.md/result.json + the run matrix.json (reanalyzed: true)."""
    matrix_dir = os.path.join(REPORT_ROOT, run_ts)
    if not os.path.isdir(matrix_dir):
        log("reanalyze: no such run dir: %s" % matrix_dir)
        return 2
    rows = []
    for cid in sorted(os.listdir(matrix_dir)):
        cell_dir = os.path.join(matrix_dir, cid)
        if not os.path.isdir(cell_dir):
            continue
        capture = os.path.join(cell_dir, "capture")
        if not os.path.isdir(capture):
            continue
        try:
            cell = load_cell(cid)
        except Exception as e:
            log("reanalyze: cell %s not loadable (%s) — skipped" % (cid, e))
            continue
        expected_route = BACKEND_TO_ROUTE.get(cell["api_backend"],
                                              cell["api_backend"])
        flat, calls = analyze_capture(capture, expected_route, cid)
        inv = check_invariants(flat, cell.get("invariants", {}))
        obs = collect_observations(flat, cell.get("observe", []))
        meta = {}
        rj = os.path.join(cell_dir, "result.json")
        if os.path.isfile(rj):
            try:
                meta = json.load(open(rj))
            except Exception:
                meta = {}
        derived = ""
        dc = os.path.join(cell_dir, "derived-config.toml")
        if os.path.isfile(dc):
            with open(dc) as fh:
                derived = fh.read()
        result = write_cell_report(cell, flat, calls, inv, obs, cell_dir,
                                   derived, meta.get("binary_sha256_12", "?"),
                                   meta.get("wire_port", "?"),
                                   meta.get("upstream", "?"),
                                   meta.get("key_sha256_12", "?"),
                                   meta.get("raw_key_hits", 0),
                                   meta.get("turns", []))
        result["report_dir"] = cell_dir
        log("reanalyze %-12s verdict=%s route=%s status=%s rtok=%s" % (
            cid, result["verdict"], flat.get("route"), flat.get("status"),
            flat.get("reasoning_tokens")))
        rows.append(result)
    mrows = []
    for r in rows:
        a = r.get("analysis", {})
        mrows.append({
            "cell": r.get("cell"), "verdict": r.get("verdict"),
            "route": a.get("route"), "expected_route": a.get("expected_route"),
            "status": a.get("status"),
            "n_model_calls": a.get("n_model_calls"),
            "all_stream": a.get("all_stream"),
            "reasoning_present": a.get("reasoning_present"),
            "reasoning_tokens": a.get("reasoning_tokens"),
            "reasoning_encrypted_content":
                a.get("reasoning_encrypted_content"),
            "reasoning_thinking_block":
                a.get("reasoning_thinking_block"),
            "budget_trap": a.get("budget_trap"),
            "final_text_len": a.get("final_text_len"),
        })
    with open(os.path.join(matrix_dir, "matrix.json"), "w") as fh:
        json.dump({"ts": run_ts, "cells": mrows, "reanalyzed": True},
                  fh, indent=2)
    log("reanalyze done: %d cell(s) · %s" % (
        len(mrows), os.path.join(matrix_dir, "matrix.json")))
    return 0


def main(argv=None):
    p = argparse.ArgumentParser(description="wstream wire-streaming matrix")
    p.add_argument("--selftest", action="store_true",
                   help="offline gate (no proxy, no binary)")
    p.add_argument("--reanalyze", metavar="RUN_TS",
                   help="re-score an existing run's captures (no proxy)")
    p.add_argument("--cell", help="run one cell by id")
    p.add_argument("--cells", help="comma-separated cell ids")
    p.add_argument("--native", action="store_true",
                   help="run all native-backend cells")
    p.add_argument("--enabled", action="store_true",
                   help="run all enabled cells (default when no selector)")
    p.add_argument("--bin", default=os.environ.get("WSTREAM_BIN", DEFAULT_BIN),
                   help="grok-responses binary")
    p.add_argument("--live-home",
                   default=os.environ.get("GROK_HOME", DEFAULT_LIVE_HOME),
                   help="live ~/.grok (read-only source of config)")
    p.add_argument("--upstream",
                   default=os.environ.get("WSTREAM_UPSTREAM", DEFAULT_UPSTREAM),
                   help="proxy origin (no trailing /v1)")
    p.add_argument("--timeout", type=int,
                   default=int(os.environ.get("WSTREAM_TIMEOUT_S",
                                              DEFAULT_TIMEOUT_S)),
                   help="per-turn kill budget (seconds)")
    args = p.parse_args(argv)

    if args.selftest:
        return selftest()
    if args.reanalyze:
        return reanalyze(args.reanalyze)

    if not os.path.isfile(args.bin):
        log("binary missing: %s (cargo build --release -p xai-grok-pager-bin)"
            % args.bin)
        return 2

    ambient_key = os.environ.get("CODEX_LLM_PROXY_KEY", "")
    cells, man = select_cells(args)
    if not cells:
        log("no cells selected")
        return 2
    log("wstream: %d cell(s) · bin sha256_12 %s · upstream %s" % (
        len(cells), sha256_12(_read_bin(args.bin)), args.upstream))

    run_ts = utc_ts()
    summary = []
    for cell in cells:
        r = run_cell(cell, args.bin, args.live_home, args.upstream,
                     ambient_key, args.timeout, run_ts)
        summary.append(r)

    # Consolidated matrix for this run.
    matrix_dir = os.path.join(REPORT_ROOT, run_ts)
    os.makedirs(matrix_dir, exist_ok=True)
    rows = []
    for r in summary:
        rows.append({
            "cell": r.get("cell"),
            "model": r.get("analysis", {}).get("route") and r.get("cell"),
            "verdict": r.get("verdict"),
            "route": r.get("analysis", {}).get("route"),
            "expected_route": r.get("analysis", {}).get("expected_route"),
            "status": r.get("analysis", {}).get("status"),
            "n_model_calls": r.get("analysis", {}).get("n_model_calls"),
            "all_stream": r.get("analysis", {}).get("all_stream"),
            "reasoning_present": r.get("analysis", {}).get("reasoning_present"),
            "reasoning_encrypted_content":
                r.get("analysis", {}).get("reasoning_encrypted_content"),
            "reasoning_thinking_block":
                r.get("analysis", {}).get("reasoning_thinking_block"),
            "budget_trap": r.get("analysis", {}).get("budget_trap"),
            "final_text_len": r.get("analysis", {}).get("final_text_len"),
        })
    with open(os.path.join(matrix_dir, "matrix.json"), "w") as fh:
        json.dump({"ts": run_ts, "cells": rows}, fh, indent=2)
    log("wstream done: %d cell(s) · matrix %s" % (
        len(rows), os.path.join(matrix_dir, "matrix.json")))
    for r in rows:
        log("  %-16s verdict=%-6s route=%-16s status=%s stream=%s "
            "reasoning=%s" % (
                r["cell"], r["verdict"], r.get("route"), r.get("status"),
                r.get("all_stream"), r.get("reasoning_present")))
    return 0 if all(r.get("verdict") in ("PASS",) for r in summary) else 1


if __name__ == "__main__":
    sys.exit(main())
