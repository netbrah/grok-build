#!/usr/bin/env python3
"""T1/T2/T4 acceptance tests for smoke/triage/grok-triage.

Plain-assert script (NO pytest — run as `python3 test_grok_triage.py`).
Builds a synthetic fake grok-home tree in tempfile.mkdtemp() and asserts
every T1/T2/T4 requirement from the plan
(grok/plans/session-triage-skill-plan-20260915.md):

  T1  census counts, dedup (incl. M-1 volatile-value collapse), m-3 torn
      line handling, show-card fields (target rendered, request_id
      last-turn-only), m-7 prescreen precision, M-4 key sweep (sentinel
      absent + exit 3 non-vacuous), M-6 --out under home = exit 2,
      M-3 wirecap numeric pairing + optional-when-present enrichment.
  T2  hot mode: live-session filtering, no pre-start events,
      subagent attribution, --state opt-in (path under home = exit 2),
      graceful degradation on idle/torn active_sessions.json.
  T4  census --write-catalog: one PROPOSED row per genuinely new class,
      idempotent on re-run, seeded rows untouched.

Exit 0 = all pass. Exit 1 on first failure (TDD RED-first).
"""

import json
import os
import random
import re
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "grok-triage"
CATALOG = HERE / "signatures.json"

# ---------------------------------------------------------------- constants

SID_A = "11111111-2222-4333-8444-555555555555"          # live session (proj-A)
SID_B = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"          # subagent session
UUID_SUB = "01b00000-1111-2222-3333-444455556666"
CWD_A = "/fake/proj-A"
CWD_B = f"/fake/worktrees/t-repo/subagent-{UUID_SUB}"

T_START = "2026-09-15T10:00:00.000000Z"                  # A opened_at / created_at
CALL_ID = str(uuid.uuid4())                              # X-Litellm-Call-Id on resp-002 only
SENTINEL1 = "SENTINEL-RAW-KEY-" + "".join(random.choices("abcdef0123456789", k=24))
SENTINEL2 = "SENTINEL-LEAK-" + "".join(random.choices("abcdef0123456789", k=24))

PASS_COUNT = 0
CHECK_NAMES = []


def check(name, cond, detail=""):
    """Assert that keeps going is tempting, but TDD wants a crisp RED: stop at first failure."""
    global PASS_COUNT
    PASS_COUNT += 1
    CHECK_NAMES.append(name)
    if not cond:
        print(f"FAIL {PASS_COUNT:02d} - {name}\n  detail: {detail}\n")
        sys.exit(1)
    print(f"ok {PASS_COUNT:02d} - {name}")


def run(args, home, extra_env=None):
    """Run the script against a fake home with a clean, controlled env."""
    env = {k: v for k, v in os.environ.items()
           if k not in ("CODEX_LLM_PROXY_KEY", "GROK_TRIAGE_TEST_KEY")}
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        [sys.executable, str(SCRIPT), "--home", str(home), *args],
        capture_output=True, text=True, env=env, timeout=120)


def table_line(output, identity):
    """Return the census table row containing the given identity substring, or None."""
    for line in output.splitlines():
        if identity in line and re.search(r"\bn=\d+\b", line):
            return line
    return None


def line_count(output, identity):
    return len([l for l in output.splitlines()
                if identity in l and re.search(r"\bn=\d+\b", l)])


def row_n(line):
    return int(re.search(r"\bn=(\d+)\b", line).group(1))


# ---------------------------------------------------------------- fixtures

def w(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


def wjson(path, obj):
    w(path, json.dumps(obj))


def build_home(root):
    """Synthetic fake grok-home. Returns the home path."""
    home = Path(root)
    sessions = home / "sessions"

    # non-session entries at the sessions root: must be skipped (m-2)
    w(sessions / "session_search.sqlite", "not a session, never open as dir")
    w(sessions / "README.txt", "cwd-encoded dirs live here")

    # ---- session A: live, /fake/proj-A, claude-sonnet-5
    a = sessions / "%2Ffake%2Fproj-A" / SID_A
    wjson(a / "summary.json", {
        "info": {"id": SID_A, "cwd": CWD_A},
        "agent_id": "ag1.test", "agent_name": "grok-build",
        "attempt_id": "at1.test", "session_summary": "Fake A",
        "created_at": T_START, "updated_at": "2026-09-15T10:30:00.000000Z",
        "last_active_at": "2026-09-15T10:30:00.000000Z",
        "num_messages": 10, "num_chat_messages": 10,
        "current_model_id": "claude-sonnet-5",
        "request_id": str(uuid.uuid4()),
        "grok_home": str(home), "generated_title": "Fake A",
        "sandbox_profile": "off",
    })
    spawn_msg = ("Failed to spawn MCP server 'alpha-mcp': "
                 "No such file or directory (os error 2)")
    events_a = [
        {"ts": f"2026-09-15T10:0{i}:00.000Z", "type": "mcp_server_failed",
         "server_name": "alpha-mcp", "transport": "stdio",
         "target": "/fake/bin/alpha-mcp", "error_type": "spawn_failed",
         "error_message": spawn_msg} for i in (1, 2, 3)
    ] + [
        {"ts": "2026-09-15T10:04:00.000Z", "type": "mcp_server_failed",
         "server_name": "beta-mcp", "transport": "stdio",
         "target": "/fake/bin/beta-mcp", "error_type": "handshake_failed",
         "error_message": "MCP server 'beta-mcp' handshake failed: process exited (code 1)"},
        # T4 genuinely-new class (agent_error)
        {"ts": "2026-09-15T10:05:00.000Z", "type": "agent_error",
         "message": "exploding widget: value 99"},
        # coarse outcome flag: context note only, must NOT become a signature
        {"ts": "2026-09-15T10:06:00.000Z", "type": "turn_ended",
         "outcome": "error"},
    ]
    w(a / "events.jsonl", "\n".join(json.dumps(e) for e in events_a) + "\n")

    chat_a = [
        # benign tool_result: no precision shape, no wide match -> never collected
        {"type": "tool_result", "tool_call_id": "c1",
         "content": "all good, nothing failed here"},
        # bait: user message matching the wide regex -> never collected (not a tool_result)
        {"type": "user",
         "content": "please fix the tool call error in main.py"},
        # signature #6 shape (pty -32602/usize) -> must stamp catalog #6
        {"type": "tool_result", "tool_call_id": "c2",
         "content": "Tool `pty__pty_read` failed via `use_tool`: Mcp error: -32602: "
                    "failed to deserialize parameters: invalid value: integer -30, "
                    "expected usize"},
        # precision shape "is_error":true -> its own signature
        {"type": "tool_result", "tool_call_id": "c3", "is_error": True,
         "content": "boom 42"},
        # M-2: unframed quoted 'Mcp error' (transcript echo) -> never collected
        {"type": "user",
         "content": "earlier the transcript showed a Mcp error, ignore it"},
        # wide-regex-only hit (tool_result content): card diagnostic,
        # NOT a census signature (m-7 second pass is card-scoped)
        {"type": "tool_result", "tool_call_id": "c4",
         "content": "the tool returned an error status"},
    ]
    w(a / "chat_history.jsonl",
      "\n".join(json.dumps(c) for c in chat_a) + "\n")

    wjson(a / "mcp" / f"call-{uuid.uuid4()}-1.json",
          {"tool": "alpha", "error": "alpha tool exploded 7"})
    w(a / "mcp" / f"call-{uuid.uuid4()}-2.txt", "all good, clean call output")

    # ---- session B: subagent (decoded cwd suffix /subagent-<uuid>), m-1 twins + m-3 torn line
    b = sessions / ("%2Ffake%2Fworktrees%2Ft-repo%2Fsubagent-" + UUID_SUB) / SID_B
    wjson(b / "summary.json", {
        "info": {"id": SID_B, "cwd": CWD_B},
        "agent_id": "ag2.test", "agent_name": "grok-build-sub",
        "attempt_id": "at2.test", "session_summary": "Fake B",
        "created_at": "2026-09-15T10:02:00.000000Z",
        "updated_at": "2026-09-15T10:12:00.000000Z",
        "last_active_at": "2026-09-15T10:12:00.000000Z",
        "num_messages": 4, "num_chat_messages": 4,
        "current_model_id": "qwen3.8-27b",
        "request_id": str(uuid.uuid4()),
        "grok_home": str(home), "generated_title": "Fake B",
    })
    twin = lambda code: {
        "ts": "2026-09-15T10:07:00.000Z", "type": "mcp_server_failed",
        "server_name": "gamma-mcp", "transport": "stdio",
        "target": "/fake/bin/gamma-mcp", "error_type": "handshake_failed",
        "error_message": f"Handshake failed with code {code}"}
    events_b = [twin(41), twin(42)]
    w(b / "events.jsonl",
      "\n".join(json.dumps(e) for e in events_b) + "\n"
      # torn final line (m-3): prescreen hit, JSON parse fails -> unparsed: 1
      + '{"ts":"2026-09-15T10:09:59.000Z","type":"mcp_server_failed","server_name":"gamm\n')

    # ---- session C: torn summary.json (skip + note, rc stays 0)
    c = sessions / "%2Ffake%2Fproj-C" / "cccccccc-1111-4222-8333-444455556666"
    w(c / "summary.json", '{"info": {"id": "cccccccc-1111-4222-8333-')
    w(c / "events.jsonl", "")

    # ---- unified.jsonl: 6 lines (error 5 + warn 1)
    uni = [
        # L1: top-level sid=SID_A, in-window, stamped #5b
        {"ts": "2026-09-15T10:05:00.000Z", "src": "shell", "pid": 1, "ver": "1.0",
         "lvl": "error", "sid": SID_A, "msg": "shell.turn.inference_failed",
         "ctx": {"kind": "max_tokens_truncation",
                 "message": "response truncated by max_tokens"}},
        # L2: no top-level sid; ctx.subagent_id=SID_B -> attributed to B, never A
        {"ts": "2026-09-15T10:10:00.000Z", "src": "shell", "pid": 1, "ver": "1.0",
         "lvl": "error", "msg": "subagent failed",
         "ctx": {"subagent_id": SID_B, "subagent_type": "plan",
                 "effective_model": "qwen3.8-27b", "success": False,
                 "error": "Subagent was cancelled"}},
        # L3: unattributed, PRE-START (09:00 < 10:00) -> excluded from hot A
        {"ts": "2026-09-15T09:00:00.000Z", "src": "shell", "pid": 1, "ver": "1.0",
         "lvl": "error", "msg": "disk_pressure", "ctx": {"kind": "disk"}},
        # L4: sid=SID_A; carries SENTINEL2 (the sweep value when the test sets it)
        {"ts": "2026-09-15T10:12:00.000Z", "src": "shell", "pid": 1, "ver": "1.0",
         "lvl": "error", "sid": SID_A, "msg": "api_error",
         "ctx": {"message": f"upstream rejected request; echoed header {SENTINEL2}"}},
        # L5: warn level -> collected, PROPOSED candidate
        {"ts": "2026-09-15T10:11:00.000Z", "src": "shell", "pid": 1, "ver": "1.0",
         "lvl": "warn", "msg": "paywall_check_error", "ctx": {"kind": "http_status"}},
        # L6: unattributed, IN-WINDOW (10:20) -> included in hot A only
        {"ts": "2026-09-15T10:20:00.000Z", "src": "shell", "pid": 1, "ver": "1.0",
         "lvl": "error", "msg": "network_blip", "ctx": {"kind": "net"}},
    ]
    w(home / "logs" / "unified.jsonl",
      "\n".join(json.dumps(u) for u in uni) + "\n")

    # ---- mcp stderr: one non-empty file with an ERROR line (last-200 window)
    w(home / "logs" / "mcp" / "alpha-mcp.stderr.log",
      "2026-09-15T10:00:00.000000Z  INFO  alpha_mcp: booting\n"
      "2026-09-15T10:00:01.000000Z  ERROR  alpha_mcp: bind failed: addr in use (port 7777)\n"
      "2026-09-15T10:00:02.000000Z  INFO  alpha_mcp: retrying\n")

    # ---- active_sessions.json: exactly one live session (A)
    wjson(home / "active_sessions.json",
          [{"session_id": SID_A, "pid": 4242, "cwd": CWD_A,
            "opened_at": T_START}])

    # ---- dogfood wirecap: numeric pairing; call-id on resp-002 ONLY; M-4 sentinel
    wire = home / "dogfood" / "20260915T120000Z" / "wire"
    wjson(wire / "req-001.json",
          {"n": 1, "method": "GET", "path": "/v1/models",
           "ts": "2026-09-15T12:00:00Z",
           "headers": {"authorization": SENTINEL1,
                       "host": "127.0.0.1:51354"}, "body": ""})
    w(wire / "resp-001.jsonl",
      json.dumps({"n": 1, "status": 200, "ts": "2026-09-15T12:00:00Z",
                  "headers": {"content-type": "application/json"}}) + "\n")
    wjson(wire / "req-002.json",
          {"n": 2, "method": "POST", "path": "/v1/chat/completions",
           "ts": "2026-09-15T12:00:01Z",
           "headers": {"authorization": {"masked": True,
                                          "sha256_12": "abcd1234ef56", "len": 85}},
           "body": json.dumps({"model": "claude-sonnet-5"})})
    w(wire / "resp-002.jsonl",
      json.dumps({"n": 2, "status": 500, "ts": "2026-09-15T12:00:01Z",
                  "headers": {"x-litellm-call-id": CALL_ID}}) + "\n"
      + json.dumps({"id": "chatcmpl-1", "model": "claude-sonnet-5",
                    "choices": []}) + "\n")

    return home


# ---------------------------------------------------------------- main

def main():
    check("01 script exists", SCRIPT.exists(), f"missing {SCRIPT}")
    check("02 script executable", os.access(SCRIPT, os.X_OK), "not executable")
    check("03 catalog exists", CATALOG.exists(), f"missing {CATALOG}")

    tmp = Path(tempfile.mkdtemp(prefix="grok-triage-test-"))
    home = build_home(tmp / "home")

    # ================= T1: census (scan) =================
    p = run(["scan"], home)
    check("04 scan exit 0", p.returncode == 0,
          f"rc={p.returncode}\nstdout={p.stdout[-800:]}\nstderr={p.stderr[-800:]}")
    out = p.stdout
    check("05 indexes exactly 2 sessions (non-session entries skipped)",
          "sessions=2" in out, out[:400])

    sig = {
        "spawn_failed/alpha-mcp": 3,
        "handshake_failed/beta-mcp": 1,
        "handshake_failed/gamma-mcp": 2,   # M-1: 2 twin events -> 1 signature
    }
    for ident, n in sig.items():
        line = table_line(out, ident)
        check(f"06 census {ident} present", line is not None, out[:1500])
        check(f"07 census {ident} count={n}", line is not None and row_n(line) == n,
              f"line={line!r}")
    check("08 M-1 volatile collapse: exactly ONE gamma-mcp signature row",
          line_count(out, "gamma-mcp") == 1, out)
    for ident in ("subagent failed", "shell.turn.inference_failed",
                  "disk_pressure", "api_error", "paywall_check_error",
                  "network_blip"):
        check(f"09 unified class collected: {ident!r}",
              table_line(out, ident) is not None, out[:2000])
    check("10 chat #6 shape collected",
          table_line(out, "failed to deserialize parameters") is not None,
          out[:2000])
    check("11 chat is_error shape collected (boom)",
          table_line(out, "boom <N>") is not None, out[:2000])
    check("12 mcp stderr ERROR line collected",
          table_line(out, "bind failed: addr in use") is not None, out[:2000])
    check("13 agent_error (new class) collected",
          table_line(out, "exploding widget: value <N>") is not None, out[:2000])
    check("14 coarse turn_ended outcome=error is a note, not a signature",
          line_count(out, "turn_ended") == 0 and "turn_ended" in out, out)
    # precision (m-7): benign lines must not surface
    check("15 benign tool_result not collected",
          "nothing failed here" not in out, out)
    check("16 user-message wide-regex bait not collected",
          "tool call error in main.py" not in out, out)
    check("16b M-2 unframed quoted Mcp error not collected",
          "showed a Mcp error" not in out, out)
    check("17a wide-regex-only tool_result not a census signature",
          "returned an error status" not in out, out)
    # catalog stamping (seeded rows on the synthetic tree)
    for row_id, ident in (("#5a", "subagent failed"),
                          ("#5b", "shell.turn.inference_failed"),
                          ("#6", "failed to deserialize parameters")):
        line = table_line(out, ident)
        check(f"17 catalog stamp {row_id} on {ident!r}",
              line is not None and row_id in line, f"line={line!r}")
    line7 = table_line(out, "paywall_check_error")
    check("17b paywall_check_error stamped #7 (IGNORED row)",
          line7 is not None and "#7" in line7 and "IGNORED" in line7,
          f"line={line7!r}")
    check("17c torn summary.json skipped with note, rc 0",
          "torn summary.json" in out and "sessions=2" in out, out[-600:])

    # ================= T1: show card =================
    pa = run(["show", SID_A], home)
    check("18 show A exit 0", pa.returncode == 0,
          f"rc={pa.returncode}\nstderr={pa.stderr[-400:]}")
    card = pa.stdout
    for field, val in (("cwd", CWD_A), ("model", "claude-sonnet-5"),
                       ("agent", "grok-build"), ("created", T_START),
                       ("target", "/fake/bin/alpha-mcp")):
        check(f"19 show A field {field}", val in card, card[:1200])
    rid_line = next((l for l in card.splitlines() if "request_id" in l), "")
    check("20 request_id labeled last-turn-only",
          rid_line and "last turn" in rid_line.lower(), rid_line)
    check("21 show A renders 3 spawn_failed events verbatim",
          card.count("No such file or directory (os error 2)") == 3, card[:1500])
    check("22 show A includes sid-attributed unified error (#5b)",
          "shell.turn.inference_failed" in card, card)
    check("23 show A excludes subagent-attributed line (L2 -> B)",
          "Subagent was cancelled" not in card, card)
    check("24 show A excludes unattributed pre-start line (L3)",
          "disk_pressure" not in card, card)
    check("25 show A includes #6 chat error verbatim",
          "expected usize" in card, card)
    check("26 show A includes is_error chat line", "boom 42" in card, card)
    check("26a show A includes wide-regex card hit",
          "returned an error status" in card, card)
    check("27 show A mcp/call scan hit", "alpha tool exploded 7" in card, card)
    check("28 show A mcp/call benign file not a hit",
          "clean call output" not in card, card)

    # ================= T1: wirecap (M-3/M-4) =================
    check("29 wirecap numeric pairing req-001 -> 200",
          re.search(r"req-001[^\n]*200", card) is not None, card)
    check("30 wirecap numeric pairing req-002 -> 500",
          re.search(r"req-002[^\n]*500", card) is not None, card)
    check("31 optional call-id enrichment shown (resp-002 only)",
          CALL_ID in card and card.count(CALL_ID) == 1, card)
    check("32 optional model enrichment from resp body",
          re.search(r"model[=:] *claude-sonnet-5", card) is not None, card)
    check("33 raw auth header redacted", "<redacted" in card, card)
    check("34 M-4 sentinel absent from output", SENTINEL1 not in card, card)

    # ================= T1: show B (torn read, subagent flag) =================
    pb = run(["show", SID_B], home)
    check("35 show B exit 0 (torn line did not crash)", pb.returncode == 0,
          f"rc={pb.returncode}\nstderr={pb.stderr[-400:]}")
    cardb = pb.stdout
    check("36 show B unparsed: 1", "unparsed: 1" in cardb, cardb)
    check("37 show B subagent flag (decoded cwd suffix)",
          re.search(r"subagent:\s*yes", cardb) is not None, cardb[:400])

    # ================= T1: key sweep (M-4) =================
    check("38 sweep value IS rendered pre-sweep (non-vacuous)",
          SENTINEL2 in card, card)
    ps = run(["show", SID_A], home, extra_env={"GROK_TRIAGE_TEST_KEY": SENTINEL2})
    check("39 sweep violation -> exit 3", ps.returncode == 3,
          f"rc={ps.returncode}\nstdout={ps.stdout[-400:]}\nstderr={ps.stderr[-400:]}")
    check("40 sweep violation prints nothing",
          ps.stdout.strip() == "" and ps.stderr.strip() == "",
          f"stdout={ps.stdout[-200:]!r} stderr={ps.stderr[-200:]!r}")
    check("41 sentinel absent on sweep run",
          SENTINEL2 not in ps.stdout and SENTINEL2 not in ps.stderr, "")

    # ================= global path guards =================
    po = run(["scan", "--out", str(home / "report.txt")], home)
    check("42 --out under home -> exit 2", po.returncode == 2,
          f"rc={po.returncode} stderr={po.stderr[-200:]}")
    check("43 --out under home wrote nothing", not (home / "report.txt").exists(), "")
    ok = run(["scan", "--out", str(tmp / "report.txt")], home)
    check("44 --out outside home ok + file written",
          ok.returncode == 0 and (tmp / "report.txt").exists()
          and (tmp / "report.txt").stat().st_size > 0,
          f"rc={ok.returncode} stderr={ok.stderr[-200:]}")
    pst = run(["scan", "--state", str(home / "state.json")], home)
    check("45 --state under home -> exit 2", pst.returncode == 2,
          f"rc={pst.returncode} stderr={pst.stderr[-200:]}")
    pbad = run(["show", "00000000-0000-4000-8000-000000000000"], home)
    check("45a show unknown sid -> exit 2, no traceback (m-1)",
          pbad.returncode == 2 and "Traceback" not in pbad.stderr
          and "unknown session id" in pbad.stderr,
          f"rc={pbad.returncode} stderr={pbad.stderr[-300:]}")
    guarded = home / "catalog-guarded.json"
    pcatg = run(["census", "--catalog", str(guarded), "--write-catalog"], home)
    check("45b census --write-catalog under home -> exit 2, no file (M-1)",
          pcatg.returncode == 2 and not guarded.exists()
          and "rejected" in pcatg.stderr,
          f"rc={pcatg.returncode} exists={guarded.exists()} "
          f"stderr={pcatg.stderr[-200:]}")

    # ================= T2: hot mode =================
    ph = run(["hot", "--latest"], home)
    check("46 hot --latest exit 0", ph.returncode == 0,
          f"rc={ph.returncode}\nstderr={ph.stderr[-400:]}")
    hot = ph.stdout
    check("47 hot A includes sid-attributed error",
          "shell.turn.inference_failed" in hot, hot)
    check("48 hot A includes in-window unattributed error (L6)",
          "network_blip" in hot, hot)
    check("49 hot A excludes pre-start error (L3)",
          "disk_pressure" not in hot, hot)
    check("50 hot A excludes subagent-attributed error (L2 -> B)",
          "Subagent was cancelled" not in hot, hot)
    check("51 hot A includes session events",
          "No such file or directory (os error 2)" in hot, hot)
    ph2 = run(["hot", SID_A], home)
    check("52 hot <sid> same scope", ph2.returncode == 0
          and "network_blip" in ph2.stdout and "disk_pressure" not in ph2.stdout,
          f"rc={ph2.returncode}")

    sf = tmp / "hot-state.json"
    ph3 = run(["hot", "--latest", "--state", str(sf)], home)
    check("53 --state outside home honored", ph3.returncode == 0 and sf.exists(),
          f"rc={ph3.returncode} exists={sf.exists()}")
    state = json.loads(sf.read_text())
    check("54 state file carries session_id + last_run_ts",
          state.get("session_id") == SID_A and "last_run_ts" in state,
          str(state))

    # graceful degradation: idle home (no active_sessions.json)
    idle = tmp / "idle-home"
    (idle / "sessions").mkdir(parents=True)
    pidle = run(["hot", "--latest"], idle)
    check("55 idle home: exit 0 + 'no live sessions' notice",
          pidle.returncode == 0 and "no live sessions" in pidle.stdout,
          f"rc={pidle.returncode}\nstdout={pidle.stdout[:300]}")
    # torn active_sessions.json
    torn = tmp / "torn-home"
    (torn / "sessions").mkdir(parents=True)
    w(torn / "active_sessions.json", '[{"session_id": "1111')
    ptorn = run(["hot", "--latest"], torn)
    check("56 torn active_sessions.json degrades to no-live-sessions",
          ptorn.returncode == 0 and "no live sessions" in ptorn.stdout,
          f"rc={ptorn.returncode}\nstdout={ptorn.stdout[:300]}")

    # ================= T4: census --write-catalog =================
    tmpcat = tmp / "catalog.json"
    tmpcat.write_text(CATALOG.read_text())
    rows_before = len(json.loads(tmpcat.read_text())["signatures"])
    pc = run(["census", "--catalog", str(tmpcat), "--write-catalog"], home)
    check("57 census --write-catalog exit 0", pc.returncode == 0,
          f"rc={pc.returncode}\nstderr={pc.stderr[-400:]}")
    rows_after = json.loads(tmpcat.read_text())["signatures"]
    check("58 exactly 9 new PROPOSED rows appended (paywall now stamps #7)",
          len(rows_after) == rows_before + 9,
          f"before={rows_before} after={len(rows_after)}")
    new_rows = rows_after[rows_before:]
    check("59 all new rows PROPOSED with SIG- slug",
          all(r.get("status") == "PROPOSED" and r.get("id", "").startswith("SIG-")
              and r.get("pattern") for r in new_rows), str(new_rows)[:400])
    ids = [r.get("id") for r in new_rows]
    check("59a proposed slugs unique across sibling unknowns (m-2)",
          len(ids) == len(set(ids)), str(ids))
    check("59b census display: 3 unique SIG-mcp-server-failed slugs",
          pc.stdout.count("UNKNOWN(SIG-mcp-server-failed-") == 3,
          pc.stdout[:2000])
    check("60 exactly one PROPOSED row for the genuinely new agent_error class",
          sum(1 for r in new_rows if r.get("class") == "agent_error") == 1,
          str([r.get("class") for r in new_rows]))
    check("61 seeded rows untouched",
          [r["id"] for r in rows_after[:rows_before]] ==
          [r["id"] for r in json.loads(CATALOG.read_text())["signatures"]], "")
    pc2 = run(["census", "--catalog", str(tmpcat), "--write-catalog"], home)
    rows_after2 = len(json.loads(tmpcat.read_text())["signatures"])
    check("62 write-catalog idempotent on re-run",
          pc2.returncode == 0 and rows_after2 == len(rows_after),
          f"rc={pc2.returncode} {len(rows_after)} -> {rows_after2}")
    check("63 re-run stamps PROPOSED rows (no UNKNOWN for known-new)",
          "SIG-agent-error" in pc2.stdout and pc2.stdout.count("PROPOSED") >= 9,
          pc2.stdout[:1500])
    check("64 census emits repro pointers for unknowns",
          "unit-test home" in pc.stdout and "nearest case" in pc.stdout,
          pc.stdout[:1500])

    # ================= T1: perf =================
    t0 = time.monotonic()
    pf = run(["scan"], home)
    wall = time.monotonic() - t0
    check("65 full synthetic scan < 2s wall",
          pf.returncode == 0 and wall < 2.0, f"wall={wall:.3f}s rc={pf.returncode}")
    print(f"     (synthetic scan wall: {wall:.3f}s)")

    print(f"\nALL {PASS_COUNT} CHECKS PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
