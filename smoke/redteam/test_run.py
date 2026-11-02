#!/usr/bin/env python3
"""Offline test suite for smoke/redteam/run.py — stdlib only (no pip).

Runner-hardening gate for apex-ayl.28 (step-machine desync: ACP
completions consumed in arrival order, no promptId correlation) and
apex-ayl.32 (424 BPS ACP read plateau vs the 60s init budget).

Run it (OFFLINE: no proxy key, no binary, no live calls, no cargo):

  python3 smoke/redteam/test_run.py -v          # direct
  python3 smoke/redteam/run.py --selftest      # via the runner's own gate

Test map (house SDD: TDD RED-first — the RED/GREEN states below are the
evidence recorded in grok/plans/runner-hardening-report-qwen.md):

  DesyncReplayTest.test_bi_replay_off_by_one   RED pre-fix / GREEN post-fix
      Synthetic ACP transcript replay reproducing the LIVE B-ii ordering
      (smoke/redteam/report/20260915T030218Z/rt-crosswire1b/acp.log):
      M1's prompt_complete lands AFTER M2 is sent (the session was still
      draining M1: MCP init 7-30s, M2's queue registration delayed).
      Pre-fix, the step machine adopts M1's late completion as M2's
      (off-by-one: set_model + Q1 fire while M2 is still queued — the
      B-ii MidTurnAbort). Post-fix, promptId correlation (client-chosen
      params._meta.promptId, accepted by the binary at
      xai-grok-shell/src/agent/mvp_agent/acp_agent.rs:1121-1126) holds
      the steps in order.
  DesyncReplayTest.test_normal_ordering        GREEN both (regression pin:
      no drain tail — the next turn's queue/running line beats the
      previous turn's trailing prompt_complete, the common live path)
  DesyncReplayTest.test_error_turn_grace       GREEN both (ACP-2: stop=error
      turn — agentResult detail still captured from the trailing
      prompt_complete inside the grace window)
  InitBudgetTest.test_budget_margin_2x         RED pre-fix / GREEN post-fix
      (the 26,001B / 424BPS = 61.3s worst-case math; margin >= 2x)
  InitBudgetTest.test_windowed_plateau_424bps  RED pre-fix / GREEN post-fix
      (real start() handshake against a 26,001B initialize line
      delivered at exactly 424 BPS — the XREPLAY-2 plateau model)
  InitBudgetTest.test_bulk_read_syscall_pin    RED pre-fix / GREEN post-fix
      (per-syscall plateau model: the read path must issue bulk reads,
      not one read(1) per byte — 26,001 syscalls x 2.35ms = 61.1s)
  CaseContractTest                             GREEN both (0 regressions
      over the FULL existing case set — the campaign's acceptance pins)

The replay drives the REAL AcpSession (start() handshake, _iter_lines,
_read_until, call, prompt, set_model) — only the process is fake:
subprocess.Popen is mocked to a scripted binary (ScriptedBinary) that
schedules its notifications on a fake clock, and select/time are faked
to match. No live process, no network.
"""
import ast
import io
import json
import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
import types
import time as _real_time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)
import run  # noqa: E402  (the runner under test)

# XREPLAY-2 measurements (grok/plans/xreplay2-close-report.md §6,
# ledger XREPLAY-2 + CROSSWIRE-1 carry items apex-ayl.28/.32):
PLATEAU_BPS = 424              # deterministic read-throughput plateau
INIT_LINE_BYTES = 26001        # measured initialize result line
PLATEAU_SYSCALL_S = 1.0 / PLATEAU_BPS  # per-syscall model: read(1) ~2.35ms


# ---------------------------------------------------------------------------
# Fake process plumbing (fake clock + fake stdout + fake select)
# ---------------------------------------------------------------------------

class FakeClock(object):
    def __init__(self):
        self.now = 0.0

    def advance(self, dt):
        self.now += dt


class FakeTime(object):
    """Stands in for the time module inside run.py: time() is faked,
    everything else (strftime, ...) delegates to the real module."""

    def __init__(self, clock):
        self._clock = clock

    def time(self):
        return self._clock.now

    def __getattr__(self, name):
        return getattr(_real_time, name)


class _FakeBuffer(object):
    """proc.stdout.buffer stand-in: read1(n) returns whatever is due,
    up to n bytes — one raw read per call (the runner's read path)."""

    def __init__(self, fake):
        self._fake = fake

    def read1(self, n):
        return self._fake.read_bytes(n)


class FakeStdout(object):
    """Byte-level stand-in for proc.stdout under the fake clock.

    Lines are scheduled at absolute fake-clock times;
    buffer.read1(n) serves up to n bytes of what is due (returns what
    is available — never blocks for n; b"" only after eof()).
    read_calls/read_bytes pin the syscall PATTERN of the read path —
    apex-ayl.32: the 424 BPS plateau was read(1)-specific per-syscall
    EDR/auditd inspection heat (child-side raw read of the same line
    = 0.97s).
    """

    def __init__(self, clock):
        self.clock = clock
        self._chunks = []   # (release_at, bytes), scheduled order
        self._buf = b""
        self._eof = False
        self.read_calls = 0
        self.read_bytes_total = 0
        self._buffer = _FakeBuffer(self)

    @property
    def buffer(self):
        return self._buffer

    def schedule(self, at, text):
        self._chunks.append((at, text.encode("utf-8")))

    def eof(self):
        self._eof = True

    def next_release_time(self):
        due = [at for at, _ in self._chunks if at > self.clock.now]
        return min(due) if due else None

    def release_due(self):
        still = []
        for at, data in self._chunks:
            if at <= self.clock.now:
                self._buf += data
            else:
                still.append((at, data))
        self._chunks = still

    def pending(self):
        return bool(self._buf)

    def read_bytes(self, n):
        self.read_calls += 1
        self.release_due()
        if not self._buf and self._eof:
            return b""
        take = min(n, len(self._buf))
        out = self._buf[:take]
        self._buf = self._buf[take:]
        self.read_bytes_total += len(out)
        return out


class FakeSelectModule(object):
    """Stands in for the select module: readable when fake-stdout has
    bytes due; a wait advances the fake clock to the next scheduled
    chunk (or by the timeout, whichever comes first)."""

    def __init__(self, clock, stdout):
        self._clock = clock
        self._stdout = stdout

    def select(self, rlist, wlist, eflist, timeout):
        self._stdout.release_due()
        if self._stdout.pending():
            return [rlist[0]], [], []
        nxt = self._stdout.next_release_time()
        if nxt is not None and nxt <= self._clock.now + timeout:
            self._clock.advance(nxt - self._clock.now)
            self._stdout.release_due()
            return ([rlist[0]] if self._stdout.pending() else [], [], [])
        self._clock.advance(timeout)
        return [], [], []


class _FakeStdin(object):
    """Text stdin of the fake process: each line written by the runner
    is parsed and routed to the scripted binary, which schedules its
    replies on the shared fake stdout."""

    def __init__(self, binary, clock):
        self._binary = binary
        self._clock = clock
        self._buf = ""

    def write(self, s):
        self._buf += s
        while "\n" in self._buf:
            line, self._buf = self._buf.split("\n", 1)
            if line.strip():
                self._binary.on_send(json.loads(line), self._clock.now)

    def flush(self):
        pass


class FakeProc(object):
    def __init__(self, binary, clock, stdout):
        self.binary = binary
        self.stdout = stdout
        self.stdin = _FakeStdin(binary, clock)
        self.pid = -1

    def poll(self):
        return None


# ---------------------------------------------------------------------------
# The scripted binary (minimal reactive ACP server)
# ---------------------------------------------------------------------------

class ScriptedBinary(object):
    """Minimal reactive ACP server for transcript replay.

    Faithful to the live B-ii notification ordering
    (report/20260915T030218Z/rt-crosswire1b/acp.log): per prompt send
    the binary

      1. registers the prompt in the queue (queue/changed with
         runningPromptId) — IMMEDIATELY (+reg_s) unless the session is
         still draining the previous turn, in which case registration
         is delayed to the previous turn's prompt_complete +
         drain_tail_s (B-ii: M2's registration landed ~8s after the
         send, acp.log lines 33->41; the session was still draining
         M1, MCP init 7-30s);
      2. ends the turn run_s after registration with turn_completed
         (B-ii line 52);
      3. trails prompt_complete by pc_gap_s after turn_completed
         (B-ii lines 52->54; with a drain tail, the previous turn's
         prompt_complete lands just PAST the next prompt's send —
         B-ii lines 32->33->34);
      4. answers the prompt's JSON-RPC request only after the turn ends
         (B-ii line 37, result carrying _meta.promptId —
         build_prompt_response_meta, acp_agent.rs:1453).

    The prompt id: the client-chosen params._meta.promptId when present
    (accepted at acp_agent.rs:1121-1126), else a binary-generated id —
    the exact fallback the real binary uses.
    """

    def __init__(self, stdout, clock, run_s=0.35, reg_s=0.05,
                 pc_gap_s=0.15, drain_tail_s=0.0,
                 turn_stops=None, turn_agent_result=None,
                 init_bps=0, init_line=None,
                 inter_ack_s=0.1, turn_chunks=None,
                 inter_replies=None, inter_broadcast=False,
                 tool_calls=None, prompt_error=None, prompt_error_s=0.1):
        self.stdout = stdout
        self.clock = clock
        self.run_s = run_s
        self.reg_s = reg_s
        self.pc_gap_s = pc_gap_s
        self.drain_tail_s = drain_tail_s
        self.turn_stops = turn_stops or []
        self.turn_agent_result = turn_agent_result
        self.init_bps = init_bps
        self.init_line = init_line
        self.session_id = "01a0replay-0000-4000-8000-replay00000001"
        self.turn_pids = []    # promptId per prompt, in send order
        self.t_reg = []        # queue-registration time per turn
        self.t_end = []        # turn_completed time per turn
        self.t_pc = []         # prompt_complete time per turn
        self.early_step_fired = False
        # apex-bdfz BZ-3: the x.ai/interject ack delay, each turn's
        # agent_message_chunk script [(seconds after registration, text)],
        # and every _x.ai/interject request received, as (t, request).
        self.inter_ack_s = inter_ack_s
        self.turn_chunks = turn_chunks or []
        self.interjects = []
        # apex-bdfz BZ-5: per-request x.ai/interject answers in request order
        # ({"after": s, "reply": "queued" | "error" | "silent"}; default queued
        # after inter_ack_s); the x.ai/session/interjection broadcast the
        # product sends for every interject; Pending tool_call lines
        # [{"at": s after registration, "toolCallId", "name", "rawInput",
        # "turn": i}]; and an error turn's session/prompt JSON-RPC error,
        # answered prompt_error_s after its prompt_complete (acp_agent.rs
        # prompt(): prompt_complete is emitted before the Err branch returns).
        self.inter_replies = inter_replies or []
        self.inter_broadcast = inter_broadcast
        self.tool_calls = tool_calls or []
        self.prompt_error = prompt_error
        self.prompt_error_s = prompt_error_s

    # -- wire helpers -------------------------------------------------------

    def _notify(self, t, method, params):
        self.stdout.schedule(t, json.dumps(
            {"jsonrpc": "2.0", "method": method, "params": params}) + "\n")

    def _respond(self, t, tid, result=None, error=None):
        m = {"jsonrpc": "2.0", "id": tid}
        if error is not None:
            m["error"] = error
        else:
            m["result"] = result
        self.stdout.schedule(t, json.dumps(m) + "\n")

    def _stop_for(self, i):
        return self.turn_stops[i] if i < len(self.turn_stops) \
            else "end_turn"

    # -- the reactive side ---------------------------------------------------

    def on_send(self, d, t):
        method = d.get("method")
        tid = d.get("id")
        if method == "initialize":
            if self.init_line is not None:
                line = self.init_line
                if self.init_bps:
                    # continuous plateau: bps-sized chunks, 1s apart
                    for i in range(0, len(line), self.init_bps):
                        self.stdout.schedule(
                            t + i / float(self.init_bps),
                            line[i:i + self.init_bps])
                else:
                    self.stdout.schedule(t, line)
            else:
                self._respond(t, tid, {"protocolVersion": 1,
                                       "agentCapabilities": {}})
        elif method == "session/new":
            self._notify(t, "_x.ai/mcp/servers_updated",
                         {"sessionId": self.session_id})
            self._respond(t + 0.05, tid, {"sessionId": self.session_id})
        elif method == "session/prompt":
            self._on_prompt(d, tid, t)
        elif method == "session/set_model":
            if self._has_unended_prior(t):
                self.early_step_fired = True
            mid = (d.get("params") or {}).get("modelId")
            self._notify(t + 0.05, "_x.ai/session_notification",
                         {"sessionId": self.session_id,
                          "update": {"sessionUpdate": "model_changed",
                                     "modelId": mid}})
            self._respond(t + 0.1, tid, {"modelId": mid})
        elif method == "_x.ai/interject":
            self._on_interject(d, tid, t)
        elif method == "session/close":
            self._respond(t + 0.1, tid, {})

    def _on_interject(self, d, tid, t):
        # The x.ai/interject ext request (extensions/interject.rs:35-56):
        # queued into the session's running turn, answered
        # {"status": "queued"}. Mid-turn when a sent prompt's turn has not
        # yet delivered turn_completed (the early_step_fired detector).
        if self._has_unended_prior(t):
            self.early_step_fired = True
        spec = (self.inter_replies[len(self.interjects)]
                if len(self.interjects) < len(self.inter_replies) else {})
        self.interjects.append((t, d))
        params = d.get("params") or {}
        if self.inter_broadcast:
            # broadcast_interjection (interjection.rs): {sessionId, text,
            # interjectionId?} to every attached client, on receipt.
            echo = {"sessionId": self.session_id, "text": params.get("text")}
            if params.get("interjectionId") is not None:
                echo["interjectionId"] = params["interjectionId"]
            self._notify(t + 0.05, "_x.ai/session/interjection", echo)
        reply = spec.get("reply", "queued")
        after = spec.get("after", self.inter_ack_s)
        if reply == "queued":
            self._respond(t + after, tid, {"status": "queued"})
        elif reply == "error":
            # invalid_params with the session-not-found data
            # (extensions/interject.rs:41-45).
            self._respond(t + after, tid, error={
                "code": -32602, "message": "Invalid params",
                "data": "session not found: %s" % params.get("sessionId")})
        elif reply != "silent":
            raise ValueError("unknown interject reply: %r" % (reply,))

    def _tool_call_params(self, tc, at):
        # A Pending tool_call session/update as captured (BZ-4's
        # _bz4_tool_call, plus the notification _meta): the title is the wire
        # name and _meta["x.ai/tool"] is stamped.
        return {"sessionId": self.session_id,
                "update": {"sessionUpdate": "tool_call",
                           "toolCallId": tc["toolCallId"],
                           "title": tc["name"],
                           "rawInput": tc.get("rawInput", {}),
                           "_meta": {"x.ai/tool": _bz4_xai_tool(tc["name"])}},
                "_meta": {"eventId": "replay-event-%s" % tc["toolCallId"],
                          "agentTimestampMs": int(round(at * 1000))}}

    def _has_unended_prior(self, t, excluding_current_send=False):
        # A step fired while a previously sent prompt's turn has not yet
        # delivered turn_completed — the B-ii off-by-one at the binary's
        # state level (set_model + Q1 while M2 was still queued).
        n = len(self.turn_pids) - (1 if excluding_current_send else 0)
        for i in range(n):
            if i < len(self.t_end) and self.t_end[i] > t:
                return True
        return False

    def _on_prompt(self, d, tid, t):
        if self._has_unended_prior(t, excluding_current_send=True):
            self.early_step_fired = True
        params = d.get("params") or {}
        pid = (params.get("_meta") or {}).get("promptId")
        if not isinstance(pid, str) or not pid:
            pid = "replay-pid-%03d" % (len(self.turn_pids) + 1)
        text = (params.get("prompt") or [{}])[0].get("text", "")
        i = len(self.turn_pids)
        self.turn_pids.append(pid)
        stop = self._stop_for(i)

        # Registration: immediate, or delayed while the session is still
        # draining the previous turn (the B-ii desync window).
        t_reg = t + self.reg_s
        if i > 0 and self.drain_tail_s > 0:
            t_reg = max(t_reg, self.t_pc[i - 1] + self.drain_tail_s)
        self.t_reg.append(t_reg)
        t_end = t_reg + self.run_s
        self.t_end.append(t_end)
        t_pc = t_end + self.pc_gap_s
        self.t_pc.append(t_pc)

        self._notify(t_reg, "_x.ai/queue/changed",
                     {"sessionId": self.session_id,
                      "entries": [],
                      "runningPromptId": pid,
                      "runningText": text,
                      "runningKind": "prompt"})
        for off, chunk in (self.turn_chunks[i]
                           if i < len(self.turn_chunks) else []):
            self._notify(t_reg + off, "session/update",
                         {"sessionId": self.session_id,
                          "update": {"sessionUpdate": "agent_message_chunk",
                                     "content": {"type": "text",
                                                 "text": chunk}}})
        for tc in self.tool_calls:
            if tc.get("turn", 0) == i:
                self._notify(t_reg + tc["at"], "session/update",
                             self._tool_call_params(tc, t_reg + tc["at"]))
        self._notify(t_end, "_x.ai/session_notification",
                     {"sessionId": self.session_id,
                      "update": {"sessionUpdate": "turn_completed",
                                 "prompt_id": pid,
                                 "stop_reason": stop,
                                 "elapsed_ms": 42},
                      # build_notification_meta (updates.rs): the agent's
                      # emit time in epoch milliseconds.
                      "_meta": {"eventId": "replay-event-%03d" % (i + 1),
                                "agentTimestampMs": int(round(t_end * 1000))}})
        self._notify(t_pc, "_x.ai/session/prompt_complete",
                     {"sessionId": self.session_id,
                      "promptId": pid,
                      "stopReason": stop,
                      "agentResult": self.turn_agent_result})
        if stop == "error" and self.prompt_error is not None:
            self._respond(t_pc + self.prompt_error_s, tid,
                          error=self.prompt_error)
            return
        # JSON-RPC response after the turn (B-ii line 37), carrying the
        # prompt id in _meta (build_prompt_response_meta, :1453).
        self._respond(t_end, tid,
                      {"stopReason": stop,
                       "_meta": {"sessionId": self.session_id,
                                 "requestId": pid,
                                 "promptId": pid}})


def build_init_line(total=INIT_LINE_BYTES):
    """A JSON-valid initialize response line of exactly `total` chars
    (the XREPLAY-2-measured 26,001B line), model catalog in _meta."""
    def dump(pad):
        result = {
            "protocolVersion": 1,
            "agentCapabilities": {"loadSession": True,
                                  "promptCapabilities": {"image": False}},
            "authMethods": [{"id": "xai.api_key", "name": "xai.api_key"}],
            "_meta": {
                "grokShell": True,
                "modelState": {
                    "currentModelId": "replay-model",
                    "availableModels": [
                        {"modelId": "replay-model-%02d" % i,
                         "name": "replay-model-%02d" % i}
                        for i in range(40)]},
                "pad": pad,
            },
        }
        return json.dumps({"jsonrpc": "2.0", "id": 1, "result": result}) + "\n"
    line = dump("")
    delta = total - len(line)
    if delta < 0:
        raise AssertionError("init line base (%d) exceeds target %d"
                             % (len(line), total))
    return dump("x" * delta)


# ---------------------------------------------------------------------------
# Replay session: the REAL AcpSession against the scripted binary
# ---------------------------------------------------------------------------

class ReplaySession(run.AcpSession):
    """Drives the real start()/_iter_lines/_read_until/call/prompt/
    set_model code; only the process is fake (Popen mock + fake
    select/time, installed by the tests)."""

    def __init__(self, binary, tmpdir):
        super(ReplaySession, self).__init__(
            "grok-responses",
            os.path.join(tmpdir, "home"),
            os.path.join(tmpdir, "cwd"),
            "replay-model",
            os.path.join(tmpdir, "acp.log"))
        self._binary = binary
        self.prompt_return_times = []

    def _install_fakes(self):
        proc = FakeProc(self._binary, self._binary.clock, self._binary.stdout)
        select_mod = FakeSelectModule(self._binary.clock, self._binary.stdout)
        fake_time = FakeTime(self._binary.clock)
        self._fake_proc = proc
        self._fake_select = select_mod
        self._fake_time = fake_time

    def prompt(self, text, timeout=None):
        r = super(ReplaySession, self).prompt(text, timeout)
        self.prompt_return_times.append(self._binary.clock.now)
        return r

    def cleanup(self):
        try:
            self.log.close()
        except Exception:
            pass


class ReplayBase(unittest.TestCase):
    """Installs the process fakes (Popen/select/time) around the real
    AcpSession.start() handshake; restores all globals in tearDown."""

    def setUp(self):
        super(ReplayBase, self).setUp()
        self._tmps = []
        self._saved = {}

    def _make_replay(self, binary_kwargs=None):
        tmp = tempfile.mkdtemp(prefix="rt-replay-")
        os.makedirs(os.path.join(tmp, "home"))
        os.makedirs(os.path.join(tmp, "cwd"))
        clock = FakeClock()
        stdout = FakeStdout(clock)
        binary = ScriptedBinary(stdout, clock, **(binary_kwargs or {}))
        sess = ReplaySession(binary, tmp)
        sess._install_fakes()
        self._tmps.append(tmp)
        return sess

    def _patch(self, sess):
        self._saved["popen"] = subprocess.Popen
        subprocess.Popen = lambda *a, **k: sess._fake_proc
        self._saved["select"] = sys.modules.get("select")
        sys.modules["select"] = sess._fake_select
        self._saved["time"] = run.time
        run.time = sess._fake_time

    def tearDown(self):
        for key, val in self._saved.items():
            if key == "popen":
                subprocess.Popen = val
            elif key == "select":
                sys.modules["select"] = val
            elif key == "time":
                run.time = val
        self._saved = {}
        for t in getattr(self, "_tmps", []):
            shutil.rmtree(t, ignore_errors=True)


# ---------------------------------------------------------------------------
# apex-ayl.28 — step-machine desync (synthetic B-ii transcript replay)
# ---------------------------------------------------------------------------

class DesyncReplayTest(ReplayBase):
    def test_bi_replay_off_by_one(self):
        """The live B-ii ordering (20260915T030218Z/rt-crosswire1b):
        M1's prompt_complete lands AFTER M2 is sent (drain tail).
        Pre-fix: M2's completion = M1's pid (arrival-order adoption),
        set_model + Q1 fire while M2 is still queued (off-by-one).
        Post-fix: promptId correlation holds the steps in order."""
        sess = self._make_replay(
            {"run_s": 0.35, "reg_s": 0.05, "pc_gap_s": 0.15,
             "drain_tail_s": 1.5})
        self._patch(sess)
        try:
            sess.start()
            self.assertIsNotNone(sess.session_id)

            r1, stop1 = sess.prompt("M1")
            self.assertEqual(stop1, "end_turn")
            r2, stop2 = sess.prompt("M2")
            self.assertEqual(stop2, "end_turn")

            b = sess._binary
            p1, p2 = b.turn_pids[0], b.turn_pids[1]
            self.assertNotEqual(p1, p2)

            # M1's completion is M1's own prompt.
            self.assertEqual(r1["result"]["promptId"], p1)
            # CORE: M2's completion is M2's own prompt — pre-fix this is
            # p1 (M1's late prompt_complete adopted in arrival order).
            self.assertEqual(
                r2["result"]["promptId"], p2,
                "M2's completion carried the WRONG promptId "
                "(arrival-order adoption of M1's late prompt_complete — "
                "the B-ii desync)")
            # TIMING: the M2 step may only fire after M2's real turn end.
            self.assertGreaterEqual(
                sess.prompt_return_times[1], b.t_end[1],
                "step machine returned for M2 before M2's turn completed "
                "(early firing — set_model/Q1 would race the queued turn)")

            # Downstream: switch + Q1 run in order, no early step.
            resp = sess.set_model("replay-target")
            self.assertNotIn("error", resp)
            r3, stop3 = sess.prompt("Q1")
            self.assertEqual(stop3, "end_turn")
            self.assertEqual(r3["result"]["promptId"], b.turn_pids[2])
            self.assertFalse(
                b.early_step_fired,
                "a step fired while a previously sent turn was still "
                "queued/running (the B-ii off-by-one symptom)")

            close = sess.call("session/close",
                              {"sessionId": sess.session_id}, timeout=10)
            self.assertIn("result", close)
        finally:
            sess.cleanup()

    def test_normal_ordering(self):
        """Regression pin (common live path, no drain tail): the next
        turn's queue/running line beats the previous turn's trailing
        prompt_complete. Must pass pre-fix AND post-fix."""
        sess = self._make_replay(
            {"run_s": 5.0, "reg_s": 0.5, "pc_gap_s": 1.0,
             "drain_tail_s": 0.0})
        self._patch(sess)
        try:
            sess.start()
            r1, stop1 = sess.prompt("M1")
            r2, stop2 = sess.prompt("M2")
            r3, stop3 = sess.prompt("Q1")
            b = sess._binary
            self.assertEqual(stop1, "end_turn")
            self.assertEqual(stop2, "end_turn")
            self.assertEqual(stop3, "end_turn")
            self.assertEqual(r1["result"]["promptId"], b.turn_pids[0])
            self.assertEqual(r2["result"]["promptId"], b.turn_pids[1])
            self.assertEqual(r3["result"]["promptId"], b.turn_pids[2])
            self.assertFalse(b.early_step_fired)
        finally:
            sess.cleanup()

    def test_error_turn_grace(self):
        """ACP-2 regression pin: a stop=error turn — the agentResult
        detail lives in the trailing prompt_complete (a few lines after
        turn_completed) and must still be captured by the grace read,
        with the prompt id attached. Pre-fix AND post-fix."""
        sess = self._make_replay(
            {"run_s": 10.0, "reg_s": 0.5, "pc_gap_s": 2.0,
             "drain_tail_s": 0.0,
             "turn_stops": ["error", "end_turn"],
             "turn_agent_result":
                 "API error (status 404 Not Found): litellm.NotFoundError"})
        self._patch(sess)
        try:
            sess.start()
            r1, stop1 = sess.prompt("M1")
            self.assertEqual(stop1, "error")
            self.assertIn("error", r1)
            self.assertIn("404", r1["error"]["message"])
            self.assertEqual(r1["error"].get("promptId"),
                             sess._binary.turn_pids[0])
            r2, stop2 = sess.prompt("M2")
            self.assertEqual(stop2, "end_turn")
            self.assertEqual(r2["result"]["promptId"],
                             sess._binary.turn_pids[1])
            self.assertFalse(sess._binary.early_step_fired)
        finally:
            sess.cleanup()


# ---------------------------------------------------------------------------
# apex-ayl.32 — 424 BPS plateau vs the init budget
# ---------------------------------------------------------------------------

class InitBudgetTest(ReplayBase):
    def test_budget_margin_2x(self):
        """The 2x-margin math: 26,001B init line at the 424 BPS plateau
        = 61.3s worst case. Pre-fix budget (hardcoded 60s) = 0.98x ->
        RED. Post-fix named budget must hold >= 2x."""
        budget = getattr(run, "ACP_INIT_TIMEOUT_S", None)
        self.assertIsNotNone(
            budget,
            "no named init budget — pre-fix the 60s is hardcoded in "
            "AcpSession.start() (margin 60/61.3 = 0.98x at the plateau)")
        worst_case_s = float(INIT_LINE_BYTES) / PLATEAU_BPS
        margin = float(budget) / worst_case_s
        self.assertGreaterEqual(
            margin, 2.0,
            "margin %.2fx < 2x at the 424 BPS plateau (worst case "
            "%.1fs vs budget %ss)" % (margin, worst_case_s, budget))

    def _make_init_replay(self, init_bps):
        clock = FakeClock()
        stdout = FakeStdout(clock)
        binary = ScriptedBinary(
            stdout, clock, init_bps=init_bps,
            init_line=build_init_line())
        tmp = tempfile.mkdtemp(prefix="rt-init-")
        os.makedirs(os.path.join(tmp, "home"))
        os.makedirs(os.path.join(tmp, "cwd"))
        self._tmps.append(tmp)
        sess = ReplaySession(binary, tmp)
        sess._install_fakes()
        self._patch(sess)
        return sess

    def test_windowed_plateau_424bps(self):
        """The XREPLAY-2 plateau model, end to end: the 26,001B
        initialize line arrives at exactly 424 BPS (continuous).
        Pre-fix: 61.3s > 60s budget -> initialize times out -> start()
        raises (the observed intermittent init failures). Post-fix:
        completes inside the named budget."""
        sess = self._make_init_replay(init_bps=PLATEAU_BPS)
        try:
            sess.start()   # raises pre-fix (60s < 61.3s)
            self.assertIsNotNone(sess.session_id)
            self.assertLessEqual(
                sess._binary.clock.now,
                float(getattr(run, "ACP_INIT_TIMEOUT_S", 10 ** 9)))
        finally:
            sess.cleanup()

    def test_bulk_read_syscall_pin(self):
        """Per-syscall plateau model: bytes available immediately; the
        cost is per read() call (1/424 s each — the EDR/auditd
        per-syscall inspection heat). Pre-fix read(1): 26,001 calls x
        2.35ms = 61.1s -> the plateau. Post-fix chunked reads: a
        handful of bulk calls. The read path must issue bulk reads."""
        sess = self._make_init_replay(init_bps=0)
        try:
            sess.start()
            self.assertIsNotNone(sess.session_id)
            calls = sess._binary.stdout.read_calls
            implied_s = calls * PLATEAU_SYSCALL_S
            budget = float(getattr(run, "ACP_INIT_TIMEOUT_S", 60))
            self.assertLess(
                implied_s, budget,
                "read path issued %d read() calls; at the plateau's "
                "per-syscall cost that is %.1fs vs the %ss budget "
                "(the 424 BPS read(1) pattern)" % (calls, implied_s,
                                                   budget))
            self.assertLessEqual(
                calls, 8,
                "read path must issue bulk reads (got %d — the "
                "read(1)-per-byte pattern)" % calls)
        finally:
            sess.cleanup()


# ---------------------------------------------------------------------------
# apex-bdfz BZ-1 — the turn-completion matcher as a feedable object
# ---------------------------------------------------------------------------

_BZ1_SID = "01a0replay-0000-4000-8000-replay00000001"
_BZ1_DETAIL = ("API error (status 400 Bad Request): litellm.BadRequestError: "
               "bz1 provider detail")


def _bz1_turn_completed(pid, stop="end_turn"):
    """turn_completed in ScriptedBinary's wire shape."""
    return {"jsonrpc": "2.0", "method": "_x.ai/session_notification",
            "params": {"sessionId": _BZ1_SID,
                       "update": {"sessionUpdate": "turn_completed",
                                  "prompt_id": pid, "stop_reason": stop,
                                  "elapsed_ms": 42}}}


def _bz1_prompt_complete(pid, stop="end_turn", agent_result=None):
    """prompt_complete in ScriptedBinary's wire shape."""
    return {"jsonrpc": "2.0", "method": "_x.ai/session/prompt_complete",
            "params": {"sessionId": _BZ1_SID, "promptId": pid,
                       "stopReason": stop, "agentResult": agent_result}}


def _bz1_queue_changed(running_pid):
    """queue/changed in ScriptedBinary's wire shape."""
    return {"jsonrpc": "2.0", "method": "_x.ai/queue/changed",
            "params": {"sessionId": _BZ1_SID, "entries": [],
                       "runningPromptId": running_pid,
                       "runningText": "M", "runningKind": "prompt"}}


class MidTurnWaiterTest(ReplayBase):
    """apex-bdfz BZ-1 (plan r3 section 2.2 item 1, section 3 F-16, review
    R-5): AcpSession._TurnWaiter.feed applies _read_turn_completion's rules
    one parsed line at a time and answers "wait" / "grace" / "done". "grace"
    is owed only by a turn_completed error terminal, which carries no
    agentResult; a prompt_complete terminal already carries it and is "done"
    at once. The sequential prompt() path keeps its reads and its surfaced
    error."""

    def _fake_time(self, now=100.0):
        clock = FakeClock()
        clock.now = now
        return clock, FakeTime(clock)

    def _drive_one_prompt(self, binary_kwargs):
        """One prompt() turn on the real AcpSession against ScriptedBinary.
        The process globals are swapped for this call only, so two calls in
        one test cannot leak a fake into the next."""
        sess = self._make_replay(binary_kwargs)
        saved = (subprocess.Popen, sys.modules.get("select"), run.time)
        subprocess.Popen = lambda *a, **k: sess._fake_proc
        sys.modules["select"] = sess._fake_select
        run.time = sess._fake_time
        try:
            sess.start()
            b = sess._binary
            before = b.stdout.read_calls
            resp, stop = sess.prompt("M1")
            return {"resp": resp, "stop": stop,
                    "reads": b.stdout.read_calls - before,
                    "returned_at": b.clock.now, "pid": b.turn_pids[0]}
        finally:
            subprocess.Popen, sys.modules["select"], run.time = saved
            sess.cleanup()

    def test_feed_matches_expected_prompt_id_only(self):
        """The live B-ii sequence (apex-ayl.28): a late turn_completed for an
        already-consumed prompt, the queue row naming ours, then ours. feed
        answers wait / wait / done, and only the late line trips the desync
        guard, with its log line verbatim."""
        consumed = {"pid-0"}
        waiter = run.AcpSession._TurnWaiter(
            "pid-1", completed_prompt_ids=consumed)
        logged = []
        saved = run.log
        run.log = logged.append
        try:
            states = [waiter.feed(_bz1_turn_completed("pid-0"))]
            first = list(logged)
            states.append(waiter.feed(_bz1_queue_changed("pid-1")))
            states.append(waiter.feed(_bz1_turn_completed("pid-1")))
        finally:
            run.log = saved
        self.assertEqual(states, ["wait", "wait", "done"])
        self.assertEqual(
            first,
            ["ACP desync guard: ignoring turn_completed pid=pid-0 "
             "(expected pid-1) — late/foreign completion"],
            "the late completion must be rejected by the consumed-guard")
        self.assertEqual(logged, first, "only the late line trips the guard")
        self.assertEqual(waiter.completion,
                         {"promptId": "pid-1", "stopReason": "end_turn",
                          "agentResult": None, "via": "turn_completed"})
        self.assertEqual(consumed, {"pid-0", "pid-1"},
                         "ours joins the consumed set once the waiter is done")

    def test_error_completion_asks_for_the_grace_read(self):
        """F-5 / F-16: a turn_completed error terminal carries no agentResult,
        so feed answers "grace": the turn is over and the reader keeps reading
        up to wait()'s deadline, inside ACP_ERROR_GRACE_S of now; the matching
        trailing prompt_complete upgrades the completion to the
        prompt_complete shape carrying the provider detail, which prompt()
        then surfaces verbatim."""
        waiter = run.AcpSession._TurnWaiter("pid-1")
        clock, fake = self._fake_time(100.0)
        saved = run.time
        run.time = fake
        try:
            state = waiter.feed(_bz1_turn_completed("pid-1", "error"))
            deadline = waiter.wait(clock.now + 300.0)
            clock.advance(1.0)
            upgraded = waiter.feed(
                _bz1_prompt_complete("pid-1", "error", _BZ1_DETAIL))
            after = waiter.wait(clock.now + 300.0)
        finally:
            run.time = saved
        with self.subTest(leg="waiter"):
            self.assertEqual(state, "grace",
                             "an error turn_completed must ask for the "
                             "grace read, not end the read")
            self.assertIsNotNone(deadline, "no grace deadline exposed")
            self.assertGreater(deadline, 100.0)
            self.assertLessEqual(deadline - 100.0, run.ACP_ERROR_GRACE_S)
            self.assertEqual(upgraded, "done")
            self.assertEqual(waiter.completion,
                             {"promptId": "pid-1", "stopReason": "error",
                              "agentResult": _BZ1_DETAIL,
                              "via": "prompt_complete"})
            self.assertIsNone(after)
        seq = self._drive_one_prompt(
            {"run_s": 10.0, "reg_s": 0.5, "pc_gap_s": 2.0,
             "drain_tail_s": 0.0, "turn_stops": ["error"],
             "turn_agent_result": _BZ1_DETAIL})
        with self.subTest(leg="sequential surfaced error"):
            err = seq["resp"].get("error") or {}
            self.assertEqual(seq["stop"], "error")
            self.assertEqual(err.get("message"), _BZ1_DETAIL,
                             "no agentResult in the surfaced error: %r"
                             % (err.get("message"),))
            self.assertEqual(err.get("via"), "prompt_complete")
            self.assertEqual(err.get("promptId"), seq["pid"])

    def test_prompt_complete_error_never_asks_for_grace(self):
        """R-5: a prompt_complete terminal already carries agentResult, error
        or not, so feed answers "done" with no grace deadline outstanding,
        and the sequential path takes no extra read and no ACP_ERROR_GRACE_S
        stall on it: its reads equal those of the same turn ending
        end_turn."""
        waiter = run.AcpSession._TurnWaiter("pid-1")
        clock, fake = self._fake_time(100.0)
        saved = run.time
        run.time = fake
        try:
            state = waiter.feed(
                _bz1_prompt_complete("pid-1", "error", _BZ1_DETAIL))
            outstanding = waiter.wait(clock.now + 300.0)
        finally:
            run.time = saved
        with self.subTest(leg="waiter"):
            self.assertIsNone(outstanding,
                              "a grace deadline is outstanding after a "
                              "prompt_complete terminal")
            self.assertEqual(state, "done")
            self.assertEqual(waiter.completion,
                             {"promptId": "pid-1", "stopReason": "error",
                              "agentResult": _BZ1_DETAIL,
                              "via": "prompt_complete"})
        # pc_gap_s < 0: prompt_complete is the terminal; turn_completed and
        # the JSON-RPC response trail it by 0.5 s.
        base = {"run_s": 5.0, "reg_s": 0.5, "pc_gap_s": -0.5,
                "drain_tail_s": 0.0, "turn_agent_result": _BZ1_DETAIL}
        err = self._drive_one_prompt(dict(base, turn_stops=["error"]))
        ok = self._drive_one_prompt(dict(base, turn_stops=["end_turn"]))
        with self.subTest(leg="sequential reads"):
            self.assertEqual(err["reads"], ok["reads"],
                             "one extra read is taken on the prompt_complete "
                             "error terminal (%d reads vs %d)"
                             % (err["reads"], ok["reads"]))
            self.assertEqual(err["returned_at"], ok["returned_at"],
                             "the reader stalled past the error terminal")
            self.assertEqual((err["resp"].get("error") or {}).get("message"),
                             _BZ1_DETAIL)
            self.assertEqual(ok["stop"], "end_turn")

    def test_normal_completion_is_done(self):
        """The other end of the three-way contract: a non-terminal line and the
        turn's JSON-RPC response are "wait" (keep reading to the caller's own
        deadline; the response is kept, informational); a normal
        turn_completed and a normal prompt_complete are each "done" at once
        with nothing outstanding."""
        waiter = run.AcpSession._TurnWaiter("pid-1", tid=7)
        chunk = {"jsonrpc": "2.0", "method": "session/update",
                 "params": {"sessionId": _BZ1_SID,
                            "update": {"sessionUpdate": "agent_message_chunk",
                                       "content": {"type": "text",
                                                   "text": "hi"}}}}
        reply = {"jsonrpc": "2.0", "id": 7,
                 "result": {"stopReason": "end_turn"}}
        self.assertEqual(waiter.feed(chunk), "wait")
        self.assertEqual(waiter.feed(reply), "wait")
        self.assertIs(waiter.response, reply)
        self.assertEqual(waiter.wait(250.0), 250.0)
        self.assertEqual(waiter.feed(_bz1_turn_completed("pid-1")), "done")
        self.assertIsNone(waiter.wait(250.0))
        self.assertEqual(waiter.completion,
                         {"promptId": "pid-1", "stopReason": "end_turn",
                          "agentResult": None, "via": "turn_completed"})
        other = run.AcpSession._TurnWaiter("pid-2")
        self.assertEqual(
            other.feed(_bz1_prompt_complete("pid-2", "end_turn", "all done")),
            "done")
        self.assertIsNone(other.wait(250.0))
        self.assertEqual(other.completion,
                         {"promptId": "pid-2", "stopReason": "end_turn",
                          "agentResult": "all done",
                          "via": "prompt_complete"})


# ---------------------------------------------------------------------------
# apex-bdfz BZ-2 — prompt_send / prompt_collect and the deferred inbox
# ---------------------------------------------------------------------------

class _DeferredTurnBinary(ScriptedBinary):
    """ScriptedBinary whose prompt turns were already read off stdout on the
    session's behalf: a mid-turn reader consumed the turn's lines and left
    the completion (and the prompt's JSON-RPC response) in AcpSession._inbox
    (plan r3 section 2.2 item 3). session/prompt is accepted and recorded;
    no line of the turn is scheduled, so nothing more arrives on stdout for
    it and whatever prompt_collect still read would hang to its deadline."""

    def __init__(self, *args, **kwargs):
        super(_DeferredTurnBinary, self).__init__(*args, **kwargs)
        self.prompt_tids = []
        self.prompt_params = []

    def _on_prompt(self, d, tid, t):
        params = d.get("params") or {}
        self.turn_pids.append((params.get("_meta") or {}).get("promptId"))
        self.prompt_tids.append(tid)
        self.prompt_params.append(params)


class MidTurnSplitTest(ReplayBase):
    """apex-bdfz BZ-2 (plan r3 section 2.2 items 2-3, review F-6): prompt()
    is prompt_send (the session/prompt request; returns (tid, prompt_id)
    without reading) plus prompt_collect, which drains AcpSession._inbox
    before it reads a single byte. A deferred completion is returned with no
    read and no wait (the F-1 race as a unit); a deferred session/prompt
    JSON-RPC response wins over the synthesized notification shape, as a
    read one does in prompt()."""

    # prompt_collect's read budget in seconds, relative to the call (the
    # plan's "deadline"; _read_turn_completion's convention).
    BUDGET_S = 30.0

    def _make_deferred(self):
        clock = FakeClock()
        stdout = FakeStdout(clock)
        binary = _DeferredTurnBinary(stdout, clock)
        tmp = tempfile.mkdtemp(prefix="rt-bz2-")
        os.makedirs(os.path.join(tmp, "home"))
        os.makedirs(os.path.join(tmp, "cwd"))
        self._tmps.append(tmp)
        sess = ReplaySession(binary, tmp)
        sess._install_fakes()
        self._patch(sess)
        return sess

    def test_collect_returns_deferred_completion_without_reading(self):
        """The turn's completion was consumed by a mid-turn reader and left
        in the inbox. prompt_collect returns it at once: stop "end_turn" and
        the resp prompt() synthesizes for it, with no read of stdout and no
        wait on the fake clock; the inbox is drained, so the completion can
        never be handed to a later turn."""
        sess = self._make_deferred()
        try:
            sess.start()
            b = sess._binary
            tid, pid = sess.prompt_send("M")
            with self.subTest(leg="send"):
                self.assertEqual(b.prompt_tids, [tid])
                self.assertEqual(
                    b.prompt_params,
                    [{"sessionId": b.session_id,
                      "prompt": [{"type": "text", "text": "M"}],
                      "_meta": {"promptId": pid}}])
            sess._inbox["completion"] = {"promptId": pid,
                                         "stopReason": "end_turn",
                                         "agentResult": None,
                                         "via": "turn_completed"}
            reads, now = b.stdout.read_calls, b.clock.now
            resp, stop = sess.prompt_collect(tid, pid, self.BUDGET_S)
            with self.subTest(leg="no wait"):
                self.assertEqual(
                    b.clock.now, now,
                    "prompt_collect read to the fake deadline (%.1f fake s) "
                    "instead of returning the deferred completion (the F-1 "
                    "hang)" % (b.clock.now - now))
            with self.subTest(leg="stop"):
                self.assertEqual(stop, "end_turn")
                self.assertEqual(resp, {"result": {"stopReason": "end_turn",
                                                   "_via": "turn_completed",
                                                   "promptId": pid}})
            with self.subTest(leg="no read"):
                self.assertEqual(
                    b.stdout.read_calls, reads,
                    "prompt_collect read stdout although the completion was "
                    "deferred")
            with self.subTest(leg="drained"):
                self.assertEqual(sess._inbox, {"responses": {},
                                               "completion": None,
                                               "prompt_response": None})
        finally:
            sess.cleanup()

    def test_pump_captured_prompt_response_wins(self):
        """F-6: the mid-turn reader also captured the turn's session/prompt
        JSON-RPC response (id == tid) into _inbox["prompt_response"].
        prompt_collect returns that response object itself, with its own
        stopReason, not the notification shape it would synthesize from the
        completion: prompt()'s preference for a response, reached through the
        inbox with no second read."""
        sess = self._make_deferred()
        try:
            sess.start()
            b = sess._binary
            tid, pid = sess.prompt_send("M")
            stored = {"jsonrpc": "2.0", "id": tid,
                      "result": {"stopReason": "stop_sequence",
                                 "_meta": {"sessionId": b.session_id,
                                           "requestId": pid,
                                           "promptId": pid}}}
            sess._inbox["prompt_response"] = stored
            sess._inbox["completion"] = {"promptId": pid,
                                         "stopReason": "end_turn",
                                         "agentResult": None,
                                         "via": "turn_completed"}
            reads, now = b.stdout.read_calls, b.clock.now
            resp, stop = sess.prompt_collect(tid, pid, self.BUDGET_S)
            got = (resp.get("result") or {}).get("stopReason")
            with self.subTest(leg="response wins"):
                self.assertEqual(
                    got, "stop_sequence",
                    "a synthesised stopReason %r where the captured "
                    "session/prompt response carried a real one" % (got,))
                self.assertIs(resp, stored)
                self.assertEqual(stop, "stop_sequence")
            with self.subTest(leg="no second read"):
                self.assertEqual(b.stdout.read_calls, reads)
                self.assertEqual(b.clock.now, now)
            with self.subTest(leg="drained"):
                self.assertEqual(sess._inbox, {"responses": {},
                                               "completion": None,
                                               "prompt_response": None})
        finally:
            sess.cleanup()


# ---------------------------------------------------------------------------
# apex-bdfz BZ-3 — interject() / mid_turn_set_model() on the single-reader pump
# ---------------------------------------------------------------------------

_BZ3_FOREIGN_SID = "01a0foreign-0000-4000-8000-foreign000001"


def _bz3_chunk(sid, text):
    """An agent_message_chunk session/update line (the captured wire shape)."""
    return json.dumps(
        {"jsonrpc": "2.0", "method": "session/update",
         "params": {"sessionId": sid,
                    "update": {"sessionUpdate": "agent_message_chunk",
                               "content": {"type": "text",
                                           "text": text}}}}) + "\n"


def _bz3_chunk_texts(updates):
    return [(u.get("content") or {}).get("text") for u in updates
            if u.get("sessionUpdate") == "agent_message_chunk"]


class MidTurnSendTest(ReplayBase):
    """apex-bdfz BZ-3 (plan r3 section 2.2 items 4-5, section 2.1, review
    F-10): interject() sends the x.ai/interject ext request (wire method
    _x.ai/interject) into the turn prompt_send started and reads its reply
    through the single-reader pump while the turn keeps streaming; the
    turn's updates keep reaching sess.updates and its one completion is
    consumed by prompt_collect. Every reader drops a foreign session's line
    with the one predicate _session_scoped, in front of tracking and
    feeding."""

    def test_interject_is_awaited_while_turn_continues(self):
        """turn_completed at t=4; interject("HELLO") at t=0.5 is answered
        {"status": "queued"} after inter_ack_s while the turn runs: the
        request carries sessionId and text, the reply is returned, the chunks
        streamed during and after the wait land in sess.updates, the fake
        sees the request mid-turn, and prompt_collect consumes the turn's one
        completion. With no turn in flight nothing is sent."""
        # start() leaves the fake clock at 0.05: registration 0.1, end 4.0.
        sess = self._make_replay(
            {"reg_s": 0.05, "run_s": 3.9, "pc_gap_s": 0.15,
             "inter_ack_s": 0.1,
             "turn_chunks": [[(0.2, "C0"), (0.45, "C1"), (1.4, "C2"),
                              (2.4, "C3"), (3.4, "C4")]]})
        self._patch(sess)
        try:
            sess.start()
            b = sess._binary
            early, early_w = sess.interject("EARLY")
            with self.subTest(leg="no turn in flight: nothing sent"):
                self.assertIn("error", early)
                self.assertFalse(early_w["sent"])
                self.assertEqual(b.interjects, [])
            tid, pid = sess.prompt_send("M")
            self.assertAlmostEqual(b.t_end[0], 4.0)
            b.clock.advance(0.5 - b.clock.now)
            resp, witness = sess.interject("HELLO")
            returned_at = b.clock.now
            texts_at_return = _bz3_chunk_texts(sess.updates)
            self.assertEqual(len(b.interjects), 1)
            t_sent, req = b.interjects[0]
            with self.subTest(leg="ext request on stdin"):
                self.assertAlmostEqual(t_sent, 0.5)
                self.assertEqual(req["method"], "_x.ai/interject")
                self.assertEqual(req["params"],
                                 {"sessionId": b.session_id, "text": "HELLO"})
                self.assertIsInstance(req.get("id"), int)
            with self.subTest(leg="reply returned"):
                self.assertEqual(resp, {"jsonrpc": "2.0", "id": req["id"],
                                        "result": {"status": "queued"}})
                self.assertEqual(witness["id"], req["id"])
                self.assertEqual(witness["prompt_id"], pid)
                self.assertTrue(witness["sent"])
                self.assertAlmostEqual(witness["sent_at"], 0.5)
                self.assertAlmostEqual(witness["reply_at"], 0.6)
            with self.subTest(leg="mid-turn"):
                self.assertTrue(b.early_step_fired,
                                "the interjection reached the fake after the "
                                "turn ended, not inside it")
                self.assertAlmostEqual(returned_at, 0.6)
                self.assertLess(returned_at, b.t_end[0])
            with self.subTest(leg="the pump tracked the turn while waiting"):
                self.assertEqual(texts_at_return, ["C0", "C1"])
            resp2, stop = sess.prompt_collect(tid, pid, 30.0)
            with self.subTest(leg="chunks between t=0.5 and t=4 in updates"):
                self.assertEqual(_bz3_chunk_texts(sess.updates),
                                 ["C0", "C1", "C2", "C3", "C4"])
            with self.subTest(leg="exactly one completion consumed"):
                self.assertEqual(stop, "end_turn")
                self.assertEqual(resp2["result"]["promptId"], pid)
                self.assertEqual(sess._completed_prompt_ids, {pid})
                self.assertEqual(sess._inbox, {"responses": {},
                                               "completion": None,
                                               "prompt_response": None})
            late, late_w = sess.interject("LATE")
            with self.subTest(leg="collected turn: nothing sent"):
                self.assertIn("error", late)
                self.assertFalse(late_w["sent"])
                self.assertEqual(len(b.interjects), 1)
        finally:
            sess.cleanup()

    def test_foreign_session_lines_are_dropped_before_tracking(self):
        """A foreign session's agent_message_chunk read by the pump never
        reaches sess.updates (so never acp_message or a text_contains pin),
        nor does its prompt_complete reach the turn's waiter, while this
        session's next line is tracked; the sequential reader drops it the
        same way. _session_scoped is the one predicate both readers call, in
        front of _track_update and feed: no reader carries its own copy of
        the guard, and neither _track_update nor feed owns it."""
        import inspect
        scoped_src = inspect.getsource(run.AcpSession._session_scoped)
        with self.subTest(leg="one predicate, owned by the reader"):
            guard = re.compile(
                r'\.get\("sessionId"\)\s+(?:not\s+)?in[\s\\]+\(None,')
            self.assertEqual(
                len(guard.findall(inspect.getsource(run.AcpSession))), 1,
                "a reader carries its own copy of the session guard")
            self.assertEqual(len(guard.findall(scoped_src)), 1)
            for name, fn in (("_TurnWaiter.pump", run.AcpSession._TurnWaiter.pump),
                             ("_pump_line", run.AcpSession._pump_line),
                             ("set_model", run.AcpSession.set_model)):
                self.assertIn("_session_scoped(", inspect.getsource(fn),
                              "%s does not call _session_scoped" % name)
            for name, fn in (("_track_update", run.AcpSession._track_update),
                             ("_TurnWaiter.feed", run.AcpSession._TurnWaiter.feed)):
                self.assertNotIn("_session_scoped", inspect.getsource(fn),
                                 "%s owns the session guard; the reader must"
                                 % name)
            pump_src = inspect.getsource(run.AcpSession._pump_line)
            at = pump_src.index("_session_scoped(")
            self.assertLess(at, pump_src.index("_track_update("))
            self.assertLess(at, pump_src.index(".feed("))
        # Registration lands at 0.95, after the wait: the turn's waiter has
        # not yet seen its queue row when the foreign lines arrive.
        sess = self._make_replay({"reg_s": 0.9, "run_s": 3.05,
                                  "pc_gap_s": 0.15, "inter_ack_s": 0.3})
        self._patch(sess)
        try:
            sess.start()
            b = sess._binary
            tid, pid = sess.prompt_send("M")
            b.stdout.schedule(0.6, _bz3_chunk(_BZ3_FOREIGN_SID,
                                              "FOREIGN-CHUNK"))
            b.stdout.schedule(0.65, json.dumps(
                {"jsonrpc": "2.0", "method": "_x.ai/session/prompt_complete",
                 "params": {"sessionId": _BZ3_FOREIGN_SID,
                            "promptId": "foreign-pid-0001",
                            "stopReason": "end_turn",
                            "agentResult": None}}) + "\n")
            b.stdout.schedule(0.7, _bz3_chunk(b.session_id, "OWN-CHUNK"))
            b.stdout.schedule(2.0, _bz3_chunk(_BZ3_FOREIGN_SID,
                                              "FOREIGN-LATE"))
            b.clock.advance(0.5 - b.clock.now)
            resp, _ = sess.interject("HELLO")
            with self.subTest(leg="pump: foreign chunk never tracked"):
                self.assertEqual(resp.get("result"), {"status": "queued"})
                self.assertEqual(_bz3_chunk_texts(sess.updates), ["OWN-CHUNK"])
                self.assertNotIn(_BZ3_FOREIGN_SID,
                                 [u.get("_sessionId") for u in sess.updates])
            resp2, stop = sess.prompt_collect(tid, pid, 30.0)
            with self.subTest(leg="pump: foreign completion never fed"):
                self.assertEqual(stop, "end_turn")
                self.assertEqual(resp2["result"]["promptId"], pid,
                                 "the turn adopted another session's "
                                 "completion")
                self.assertEqual(sess._completed_prompt_ids, {pid})
            with self.subTest(leg="sequential reader drops it too"):
                self.assertEqual(_bz3_chunk_texts(sess.updates), ["OWN-CHUNK"])
        finally:
            sess.cleanup()


# ---------------------------------------------------------------------------
# apex-bdfz BZ-4 — the trigger matcher: N tool calls / named tool / T seconds
# ---------------------------------------------------------------------------

_BZ4_SID = "01a0replay-0000-4000-8000-replay00000004"
# _meta["x.ai/tool"] (kind, label, read_only) per tool, as the pinned capture
# corpus records them.
_BZ4_TOOLS = {
    "wait_agent": ("active_agent_message", "Send Subagent Message", True),
    "run_terminal_command": ("execute", "Run Command", False),
}
_BZ4_A = "call_BZ4000000000000000000001"
_BZ4_B = "call_BZ4000000000000000000002"
_BZ4_EARLIER = "call_BZ4000000000000000000000"
_BZ4_WAIT_IN = {"timeout_ms": 30000}
_BZ4_CMD_IN = {"command": "sleep 5", "description": "wait"}


def _bz4_line(update, method="session/update"):
    return {"jsonrpc": "2.0", "method": method,
            "params": {"sessionId": _BZ4_SID, "update": update}}


def _bz4_xai_tool(name):
    kind, label, read_only = _BZ4_TOOLS[name]
    return {"version": 1, "name": name, "kind": kind,
            "namespace": "grok_build", "label": label,
            "read_only": read_only}


def _bz4_tool_call(tcid, name, raw_input, title=None):
    """A Pending tool_call as captured: on this first emission the title is
    the wire name and _meta["x.ai/tool"] is stamped."""
    return _bz4_line({"sessionUpdate": "tool_call", "toolCallId": tcid,
                      "title": name if title is None else title,
                      "rawInput": raw_input,
                      "_meta": {"x.ai/tool": _bz4_xai_tool(name)}})


def _bz4_updates(tcid, name, raw_input, display):
    """One call's tool_call_update lines as captured: the display refresh,
    in_progress, completed."""
    return [
        _bz4_line({"sessionUpdate": "tool_call_update", "toolCallId": tcid,
                   "title": display, "kind": "other", "locations": [],
                   "rawInput": raw_input,
                   "_meta": {"x.ai/tool": _bz4_xai_tool(name)}}),
        _bz4_line({"sessionUpdate": "tool_call_update", "toolCallId": tcid,
                   "status": "in_progress", "rawOutput": {},
                   "content": [{"type": "content",
                                "content": {"type": "text",
                                            "text": "running"}}]}),
        _bz4_line({"sessionUpdate": "tool_call_update", "toolCallId": tcid,
                   "status": "completed", "rawOutput": {}}),
    ]


def _bz4_delta_chunks(tcid, name, n):
    """n tool_call_delta_chunk lines streaming one call's arguments ahead of
    its Pending line, as captured: x.ai session notifications (not
    session/update); the first names the call by its snake-case tool_call_id,
    the rest are argument fragments that carry no call id at all."""
    args = json.dumps(_BZ4_WAIT_IN)
    out = [_bz4_line({"sessionUpdate": "tool_call_delta_chunk",
                      "tool_index": 0, "tool_call_id": tcid, "name": name},
                     method="_x.ai/session_notification")]
    for i in range(n - 1):
        out.append(_bz4_line({"sessionUpdate": "tool_call_delta_chunk",
                              "tool_index": 0,
                              "arguments_delta": args[i % len(args)]},
                             method="_x.ai/session_notification"))
    return out


def _bz4_arm(arm_id, kind, arg):
    return {"arm_id": arm_id, "kind": kind, "arg": arg, "status": "armed"}


class MidTurnTriggerTest(unittest.TestCase):
    """apex-bdfz BZ-4 (plan r3 section 2.3, section 3 F-3): the pure trigger
    matcher AcpSession._trigger_hit(arm, d, counters), no process.
    after_tool_calls N holds from the line announcing the Nth distinct
    toolCallId (a session/update whose sessionUpdate is exactly tool_call);
    after_tool NAME matches _meta["x.ai/tool"]["name"], else the title, never
    kind; after_s T reads the module-level time the harness patches."""

    @staticmethod
    def _offer(hit, arms, lines, counters):
        """The section 2.3 pump's trigger pass: each line is offered to every
        un-fired arm, and an arm fires on the first line its trigger holds.
        Returns [(arm_id, line index)] in firing order."""
        fired, armed = [], list(arms)
        for i, d in enumerate(lines):
            for arm in list(armed):
                if hit(arm, d, counters):
                    fired.append((arm["arm_id"], i))
                    armed.remove(arm)
        return fired

    def test_counts_distinct_tool_call_ids_only(self):
        """F-3: after_tool_calls 2 fires once, on the second distinct call's
        tool_call line. Six tool_call_update lines over the two calls, a
        duplicated Pending line, a status update for a call announced before
        the window and 40 tool_call_delta_chunk lines never advance the count:
        an arm on 3 never fires over two distinct calls, and one line offered
        to two arms is counted once."""
        hit = run.AcpSession._trigger_hit
        pa = _bz4_tool_call(_BZ4_A, "run_terminal_command", _BZ4_CMD_IN)
        pb = _bz4_tool_call(_BZ4_B, "wait_agent", _BZ4_WAIT_IN)
        ua = _bz4_updates(_BZ4_A, "run_terminal_command", _BZ4_CMD_IN,
                          "Run Command: sleep 5")
        ub = _bz4_updates(_BZ4_B, "wait_agent", _BZ4_WAIT_IN,
                          "Waiting for agents")
        earlier = _bz4_updates(_BZ4_EARLIER, "run_terminal_command",
                               _BZ4_CMD_IN, "Run Command: sleep 5")[2]
        legs = [
            ("six updates over two calls and a duplicated Pending",
             [pa, ua[0], ua[1], pa, ua[2], pb] + ub, 5),
            ("Pending lines only", [pa, pb], 1),
            ("a status update for a call announced before the window",
             [pa, earlier, pb], 2),
            ("40 delta chunks between two calls",
             [pa] + _bz4_delta_chunks(_BZ4_B, "wait_agent", 40) + [pb], 41),
        ]
        for leg, lines, at in legs:
            with self.subTest(leg=leg):
                counters = {"t_sent": 0.0, "tool_call_ids": []}
                fired = self._offer(
                    hit, [_bz4_arm("step0.arm0", "after_tool_calls", 2),
                          _bz4_arm("step0.arm1", "after_tool_calls", 3)],
                    lines, counters)
                self.assertEqual(fired, [("step0.arm0", at)],
                                 "after_tool_calls fired on a line that does "
                                 "not announce a new distinct call")
                self.assertEqual(counters["tool_call_ids"], [_BZ4_A, _BZ4_B])
        with self.subTest(leg="N=1: the Pending line, not an argument fragment"):
            counters = {"t_sent": 0.0, "tool_call_ids": []}
            lines = (_bz4_delta_chunks(_BZ4_A, "run_terminal_command", 40)
                     + [pa] + ua)
            fired = self._offer(
                hit, [_bz4_arm("step0.arm0", "after_tool_calls", 1)],
                lines, counters)
            self.assertEqual(fired, [("step0.arm0", 40)])
            self.assertEqual(counters["tool_call_ids"], [_BZ4_A])

    def test_tool_name_prefers_x_ai_tool_meta_then_title(self):
        """after_tool compares _meta["x.ai/tool"]["name"] when it is stamped,
        else the title, exactly. A display title beside the stamped name never
        matches; a native tool_search call (no x.ai/tool) matches only via its
        title; a top-level kind alone matches nothing; and only the tool_call
        line announces the tool, not its tool_call_update nor a
        tool_call_delta_chunk naming it."""
        hit = run.AcpSession._trigger_hit
        display = _bz4_tool_call(_BZ4_A, "wait_agent", _BZ4_WAIT_IN,
                                 title="Waiting\u2026")
        search_title = "Search tools: background wait"
        search = _bz4_line(
            {"sessionUpdate": "tool_call", "toolCallId": _BZ4_B,
             "title": search_title, "kind": "search",
             "status": "in_progress",
             "rawInput": {"query": "background wait", "limit": 5},
             "_meta": {"tool_search": True}})
        kind_only = _bz4_line({"sessionUpdate": "tool_call",
                               "toolCallId": _BZ4_EARLIER, "kind": "search"})
        update = _bz4_updates(_BZ4_A, "wait_agent", _BZ4_WAIT_IN,
                              "Waiting for agents")[0]
        chunk = _bz4_delta_chunks(_BZ4_A, "wait_agent", 1)[0]

        def matches(name, d):
            return hit(_bz4_arm("step0.arm0", "after_tool", name), d,
                       {"t_sent": 0.0, "tool_call_ids": []})

        with self.subTest(leg="the stamped name, not the display title"):
            self.assertTrue(matches("wait_agent", display),
                            "the display-title line did not match its "
                            "x.ai/tool name")
            self.assertFalse(matches("Waiting\u2026", display),
                             "the display title matched although x.ai/tool "
                             "names the tool")
        with self.subTest(leg="tool_search: only via the title"):
            self.assertTrue(matches(search_title, search))
            for name in ("tool_search", "search", "search_tool"):
                self.assertFalse(matches(name, search), name)
        with self.subTest(leg="a top-level kind alone matches nothing"):
            self.assertFalse(matches("search", kind_only))
        with self.subTest(leg="only the tool_call line announces the tool"):
            self.assertFalse(matches("wait_agent", update))
            self.assertFalse(matches("wait_agent", chunk))

    def test_after_s_uses_patched_time(self):
        """after_s T holds from exactly T seconds after the parent prompt's
        send time and not before, whatever the line, on the module-level time
        the harness patches. The fake clock sits at a wall-clock epoch, the
        domain of time.time(); time.monotonic would never see it move."""
        hit = run.AcpSession._trigger_hit
        clock = FakeClock()
        clock.now = 1793491200.0   # 2026-11-01T00:00:00Z
        lines = [json.loads(_bz3_chunk(_BZ4_SID, "still thinking")),
                 _bz4_tool_call(_BZ4_A, "wait_agent", _BZ4_WAIT_IN),
                 _bz1_queue_changed("pid-1")]
        saved = run.time
        run.time = FakeTime(clock)
        try:
            counters = {"t_sent": run.time.time(), "tool_call_ids": []}
            arm = _bz4_arm("step0.arm0", "after_s", 2.0)
            seen = []
            for i, dt in enumerate((0.0, 1.5, 0.25, 0.25, 0.25)):
                clock.advance(dt)
                seen.append((clock.now - counters["t_sent"],
                             hit(arm, lines[i % len(lines)], counters)))
        finally:
            run.time = saved
        self.assertEqual(seen, [(0.0, False), (1.5, False), (1.75, False),
                                (2.0, True), (2.25, True)],
                         "after_s must hold at the 2.0 s boundary and not "
                         "before, on the patched clock")


# ---------------------------------------------------------------------------
# apex-bdfz BZ-4a — session-scoped line carry-over between stdout readers
# ---------------------------------------------------------------------------

class LineCarryTest(ReplayBase):
    """apex-bdfz BZ-4a (amendment A-1 r2 section 4): AcpSession keeps the
    lines it read from stdout but did not yet yield, and the partial tail
    after the last newline, across _iter_lines calls. A reader that stops
    early (call() on its reply, the mid-turn pump on its reply, the turn
    pump's grace read on the detail) leaves the rest of its read1 block to
    the next reader, in stdout order, each line exactly once. A shared write
    is scheduled on the fake's own stdout by an instance patch of the reply
    the fake would send (A-1 r2 section 4 construction), so every reply is
    still sent once."""

    DETAIL = ("API error (status 400 Bad Request): litellm.BadRequestError: "
              "bz4a provider detail")

    @staticmethod
    def _reply(rid, result):
        """A JSON-RPC result line in ScriptedBinary._respond's wire shape."""
        return json.dumps({"jsonrpc": "2.0", "id": rid,
                           "result": result}) + "\n"

    @staticmethod
    def _commands_update(sid):
        """An available_commands_update session/update line in the captured
        wire shape (the first command of a pinned corpus line)."""
        command = {"name": "compact",
                   "description": "Compress conversation history to save "
                                  "context window",
                   "input": {"hint": "optional context about what to "
                                     "preserve"}}
        return json.dumps(
            {"jsonrpc": "2.0", "method": "session/update",
             "params": {"sessionId": sid,
                        "update": {
                            "sessionUpdate": "available_commands_update",
                            "availableCommands": [command]}}}) + "\n"

    def _make_session_new_writes(self, writes, binary_kwargs=None):
        """A patched replay session whose fake answers session/new with
        writes(tid, sid) -> [(delay, text)], each scheduled delay seconds
        after the request, in place of its own reply."""
        sess = self._make_replay(binary_kwargs)
        b = sess._binary

        def on_send(d, t):
            if d.get("method") != "session/new":
                return ScriptedBinary.on_send(b, d, t)
            for delay, text in writes(d["id"], b.session_id):
                b.stdout.schedule(t + delay, text)

        b.on_send = on_send
        self._patch(sess)
        return sess

    def test_mid_turn_reply_block_lines_reach_the_turn(self):
        """R1: the _x.ai/interject reply and two of the turn's
        agent_message_chunk lines arrive in one write. interject() returns on
        the reply; the two chunks are left for the next reader, so
        prompt_collect ends with both in sess.updates, in stdout order
        between the chunk streamed before the wait and the one after it."""
        # start() leaves the fake clock at 0.05: registration 0.1, C0 at 0.3,
        # the reply's write at 0.6, C1 at 1.5, turn end 3.1.
        sess = self._make_replay(
            {"reg_s": 0.05, "run_s": 3.0, "pc_gap_s": 0.15,
             "inter_ack_s": 0.1,
             "turn_chunks": [[(0.2, "C0"), (1.4, "C1")]]})
        b = sess._binary

        def on_interject(d, tid, t):
            if b._has_unended_prior(t):
                b.early_step_fired = True
            b.interjects.append((t, d))
            b.stdout.schedule(t + b.inter_ack_s,
                              self._reply(tid, {"status": "queued"})
                              + _bz3_chunk(b.session_id, "S1")
                              + _bz3_chunk(b.session_id, "S2"))

        b._on_interject = on_interject
        self._patch(sess)
        try:
            sess.start()
            tid, pid = sess.prompt_send("M")
            b.clock.advance(0.5 - b.clock.now)
            resp, witness = sess.interject("HELLO")
            with self.subTest(leg="interject returned on its reply"):
                self.assertEqual(resp, {"jsonrpc": "2.0", "id": witness["id"],
                                        "result": {"status": "queued"}})
                self.assertAlmostEqual(witness["reply_at"], 0.6)
                self.assertTrue(b.early_step_fired)
            resp2, stop = sess.prompt_collect(tid, pid, 30.0)
            with self.subTest(leg="the reply's block reached the turn"):
                self.assertEqual(
                    _bz3_chunk_texts(sess.updates), ["C0", "S1", "S2", "C1"],
                    "the chunks that shared the interject reply's read1 "
                    "block were dropped when interject() returned")
            with self.subTest(leg="the turn's one completion"):
                self.assertEqual(stop, "end_turn")
                self.assertEqual(resp2["result"]["promptId"], pid)
                self.assertEqual(sess._completed_prompt_ids, {pid})
        finally:
            sess.cleanup()

    def test_grace_read_surfaces_detail_in_terminal_block(self):
        """R2: an error turn_completed, its trailing prompt_complete detail
        and the turn's JSON-RPC response arrive in one write (pc_gap_s 0).
        The pump's grace read takes the detail from the lines its outer read
        left, so the collected result carries it, with no ACP_ERROR_GRACE_S
        stall. Exactly once: the next prompt() on the session is never handed
        that prompt_complete again, so it logs no desync-guard line for the
        first turn's promptId."""
        sess = self._make_replay(
            {"run_s": 10.0, "reg_s": 0.5, "pc_gap_s": 0.0,
             "turn_stops": ["error", "end_turn"],
             "turn_agent_result": self.DETAIL})
        self._patch(sess)
        logged = []
        saved_log = run.log
        run.log = logged.append
        try:
            sess.start()
            b = sess._binary
            r1, stop1 = sess.prompt("M1")
            p1 = b.turn_pids[0]
            with self.subTest(leg="the detail in the terminal's block"):
                err = r1.get("error") or {}
                self.assertEqual(stop1, "error")
                self.assertEqual(
                    err.get("message"), self.DETAIL,
                    "the provider detail that shared the error terminal's "
                    "read1 block was dropped (got %r)" % (err.get("message"),))
                self.assertEqual(err.get("via"), "prompt_complete")
                self.assertEqual(err.get("promptId"), p1)
                self.assertAlmostEqual(
                    sess.prompt_return_times[0], b.t_end[0],
                    msg="the grace read waited out ACP_ERROR_GRACE_S for a "
                        "detail it had already read")
            r2, stop2 = sess.prompt("M2")
            with self.subTest(leg="exactly once: the next turn"):
                self.assertEqual(stop2, "end_turn")
                self.assertEqual(r2["result"]["promptId"], b.turn_pids[1])
                self.assertEqual(
                    [line for line in logged
                     if line.startswith("ACP desync guard: ignoring")
                     and p1 in line], [],
                    "the first turn's prompt_complete reached the next "
                    "turn's reader a second time")
        finally:
            run.log = saved_log
            sess.cleanup()

    def test_call_leaves_following_update_for_next_reader(self):
        """R3: the session/new reply that ends start()'s call() and an
        available_commands_update arrive in one write. call() returns on the
        reply; the update is left for the next reader, the first turn's pump,
        so it reaches sess.updates (available_commands() names it) and the
        turn still ends on its own promptId."""
        sess = self._make_session_new_writes(
            lambda tid, sid: [(0.05, self._reply(tid, {"sessionId": sid})
                               + self._commands_update(sid))],
            {"reg_s": 0.5, "run_s": 1.0, "pc_gap_s": 0.15})
        try:
            sess.start()
            b = sess._binary
            with self.subTest(leg="call() returned on its reply"):
                self.assertEqual(sess.session_id, b.session_id)
                self.assertEqual(sess.updates, [])
            r, stop = sess.prompt("M")
            with self.subTest(leg="the update reached the next reader"):
                self.assertEqual(
                    sess.available_commands(), ["compact"],
                    "the session/update that shared call()'s reply block "
                    "was dropped when call() returned")
            with self.subTest(leg="the turn"):
                self.assertEqual(stop, "end_turn")
                self.assertEqual(r["result"]["promptId"], b.turn_pids[0])
        finally:
            sess.cleanup()

    def test_partial_line_survives_reader_change(self):
        """R4: an available_commands_update line split across two writes, the
        first also carrying the session/new reply that ends start()'s call().
        The fake's own reply is replaced and the halves are 20 ms apart, so
        nothing else can arrive between them (A-1 approval N-2). The next
        reader, set_model, completes the line from the carried partial and
        tracks it."""
        def writes(tid, sid):
            line = self._commands_update(sid)
            cut = len(line) // 2
            return [(0.05, self._reply(tid, {"sessionId": sid}) + line[:cut]),
                    (0.07, line[cut:])]

        sess = self._make_session_new_writes(writes)
        try:
            sess.start()
            with self.subTest(leg="call() returned on its reply"):
                self.assertEqual(sess.session_id, sess._binary.session_id)
                self.assertEqual(sess.updates, [])
            resp = sess.set_model("replay-target")
            with self.subTest(leg="the next reader completed the line"):
                self.assertEqual(
                    sess.available_commands(), ["compact"],
                    "the partial line after call()'s reply was dropped, so "
                    "its tail never parsed")
            with self.subTest(leg="set_model's own answer"):
                self.assertEqual(resp.get("result"),
                                 {"modelId": "replay-target"})
        finally:
            sess.cleanup()


# ---------------------------------------------------------------------------
# apex-bdfz BZ-5 — the mid-turn pump: arm, fire, await, collect, disarm
# ---------------------------------------------------------------------------

_BZ5_DETAIL = ("API error (status 503 Service Unavailable): "
               "litellm.ServiceUnavailableError: bz5 provider detail")
_BZ5_A = "call_BZ5000000000000000000001"
_BZ5_B = "call_BZ5000000000000000000002"


def _bz5_call(at, tcid=_BZ5_A, name="run_terminal_command"):
    """A ScriptedBinary tool_calls entry: a Pending x.ai/tool-stamped call `at`
    seconds after the turn's registration, with its captured raw input."""
    return {"at": at, "toolCallId": tcid, "name": name,
            "rawInput": (_BZ4_CMD_IN if name == "run_terminal_command"
                         else _BZ4_WAIT_IN)}


def _bz5_arm(at, text, **extra):
    """One interject arm in the turn step's shape (plan r3 section 2.7)."""
    arm = {"at": at, "send": "interject", "text": text}
    arm.update(extra)
    return arm


def _bz5_compaction(sid, at_ms, kind):
    """An auto-compaction update on the x.ai rail, in the captured shape (the
    params and update key sets of the pinned corpus)."""
    if kind == "started":
        update = {"sessionUpdate": "auto_compact_started",
                  "context_window": 200000, "percentage": 9,
                  "reason": "Context window 9% full", "tokens_used": 18000}
    else:
        update = {"sessionUpdate": "auto_compact_completed",
                  "elapsed_ms": 1800, "summary_preview": None,
                  "tokens_after": 4000, "tokens_before": 18000}
    return json.dumps(
        {"jsonrpc": "2.0", "method": "_x.ai/session_notification",
         "params": {"sessionId": sid, "update": update,
                    "_meta": {"eventId": "replay-event-compact-%s" % kind,
                              "agentTimestampMs": at_ms}}}) + "\n"


class MidTurnPumpTest(ReplayBase):
    """apex-bdfz BZ-5 (plan r3 sections 2.3-2.5; section 3 F-1, F-2, F-4, F-5,
    F-8 to F-12, F-16): AcpSession._pump_mid_turn, the single reader while the
    turn prompt_send started is in flight. An armed arm fires on the first line
    its trigger holds on, and nothing is sent once the turn's completion is
    matched (D-1, D-2); replies are matched by JSON-RPC id; the pump reads to
    the turn's terminal, through an error terminal's grace read, so
    prompt_collect then returns the turn without reading. Each arm ends fired |
    missed | mismatched_tool | send_timeout | disarmed, with disarm_reason,
    harness_caused and the runner cause on its record; mid_turn comes from the
    turn's own evidence, never from the reply."""

    TURN_S = 30.0

    class _EofSelect(FakeSelectModule):
        """The fake select plus a closed pipe: once eof() was called and every
        scheduled byte was read, stdout selects readable and read1 returns b"",
        as a pipe at end of file does."""

        def select(self, rlist, wlist, eflist, timeout):
            out = self._stdout
            out.release_due()
            if out._eof and not out._buf and not out._chunks:
                return [rlist[0]], [], []
            return FakeSelectModule.select(self, rlist, wlist, eflist,
                                           timeout)

    def _pumped_turn(self, binary_kwargs, arms, budget=None, before=None,
                     after=None, binary_cls=ScriptedBinary, eof_select=False):
        """One pumped turn on the real AcpSession: start(), prompt_send("M"),
        before(sess, binary) to schedule extra lines, the pump with the turn's
        deadline, prompt_collect with the budget left, then after(out) while
        the fakes are still installed. The process globals are swapped for
        this call only, so two calls in one test cannot leak a fake."""
        tmp = tempfile.mkdtemp(prefix="rt-bz5-")
        self._tmps.append(tmp)
        os.makedirs(os.path.join(tmp, "home"))
        os.makedirs(os.path.join(tmp, "cwd"))
        clock = FakeClock()
        stdout = FakeStdout(clock)
        binary = binary_cls(stdout, clock, **(binary_kwargs or {}))
        sess = ReplaySession(binary, tmp)
        sess._install_fakes()
        if eof_select:
            sess._fake_select = self._EofSelect(clock, stdout)
        saved = (subprocess.Popen, sys.modules.get("select"), run.time)
        subprocess.Popen = lambda *a, **k: sess._fake_proc
        sys.modules["select"] = sess._fake_select
        run.time = sess._fake_time
        try:
            sess.start()
            tid, pid = sess.prompt_send("M")
            waiter = sess._inflight
            if before is not None:
                before(sess, binary)
            budget = self.TURN_S if budget is None else budget
            deadline = clock.now + budget
            recs = sess._pump_mid_turn(arms, waiter, deadline)
            pumped_at, reads = clock.now, stdout.read_calls
            left = max(0.0, deadline - clock.now)
            resp, stop = sess.prompt_collect(tid, pid, left)
            out = {"sess": sess, "binary": binary, "tid": tid, "pid": pid,
                   "waiter": waiter, "recs": recs, "resp": resp, "stop": stop,
                   "left": left, "pumped_at": pumped_at,
                   "collect_reads": stdout.read_calls - reads,
                   "collect_waited": clock.now - pumped_at,
                   "sent": [(t, r["method"],
                             (r.get("params") or {}).get("text"))
                            for t, r in binary.interjects],
                   "texts": _bz3_chunk_texts(sess.updates),
                   "inbox": dict(sess._inbox)}
            if after is not None:
                after(out)
            return out
        finally:
            subprocess.Popen, sys.modules["select"], run.time = saved
            sess.cleanup()

    def test_race_completion_before_trigger_sends_nothing(self):
        """F-1, completion first. In the read that ends the turn the matched
        completion is fed before any line the trigger could fire on, so it
        closes the arm first (D-1): missed, disarm_reason turn_completed, not
        harness-caused, no runner cause, and no x.ai/interject is written.
        Leg after_s: T elapses in a silent stretch whose first line is the
        completion (after_s is judged per line). Leg after_tool_calls: the
        trigger's tool_call trails the completion in the same read. D-4: once
        the turn is collected the pump sends nothing and reads nothing."""
        # start() leaves the clock at 0.05: registration 0.1, and every
        # terminal line of the turn at 1.05 (pc_gap_s 0).
        base = {"reg_s": 0.05, "run_s": 0.95, "pc_gap_s": 0.0}
        expect = {"status": "missed", "fired": False,
                  "disarm_reason": "turn_completed", "harness_caused": False,
                  "cause": None, "mid_turn": False, "request_id": None}
        late = {}

        def no_turn_in_flight(out):
            sess, b = out["sess"], out["binary"]
            reads = b.stdout.read_calls
            for key, waiter in (("collected", sess._inflight),
                                ("stale waiter", out["waiter"])):
                late[key] = sess._pump_mid_turn(
                    [_bz5_arm({"after_s": 0.5}, "BZ5-LATE")], waiter,
                    b.clock.now + self.TURN_S)
            late["reads"] = b.stdout.read_calls - reads
            late["sent"] = len(b.interjects)

        def trigger_after_terminal(sess, b):
            b._notify(b.t_end[0], "session/update",
                      b._tool_call_params(_bz5_call(0.0), b.t_end[0]))

        legs = {
            "after_s": self._pumped_turn(
                base, [_bz5_arm({"after_s": 0.5}, "BZ5-RACE")],
                after=no_turn_in_flight),
            "after_tool_calls": self._pumped_turn(
                base, [_bz5_arm({"after_tool_calls": 1}, "BZ5-RACE")],
                before=trigger_after_terminal),
        }
        for leg, out in legs.items():
            with self.subTest(leg=leg):
                self.assertEqual(out["sent"], [],
                                 "an interjection was sent into a finished "
                                 "turn")
                rec = out["recs"][0]
                self.assertEqual({k: rec[k] for k in expect}, expect)
                self.assertEqual(rec["arm_id"], "step0.arm0")
                self.assertEqual(out["stop"], "end_turn")
                self.assertEqual(out["collect_reads"], 0)
        with self.subTest(leg="D-4: no turn in flight"):
            for key in ("collected", "stale waiter"):
                rec = late[key][0]
                self.assertEqual(
                    {k: rec[k] for k in ("status", "disarm_reason",
                                         "harness_caused", "cause")},
                    {"status": "disarmed", "disarm_reason": "no_inflight_turn",
                     "harness_caused": True,
                     "cause": "interject-unavailable: step0.arm0."
                              "no_inflight_turn"}, key)
            self.assertEqual(late["reads"], 0)
            self.assertEqual(late["sent"], 0)

    def test_completion_arriving_during_await_is_not_lost(self):
        """F-1, trigger first: the trigger line is fed before the completion,
        so the request is written, and the completion that arrives while its
        reply is owed is kept for prompt_collect, which returns the turn with
        no further read and no wait. Leg same read: the completion shares the
        trigger's read, so it was stamped (turn_completed agentTimestampMs) no
        later than the send; the reply is queued, but mid_turn is false with
        no witness, and the fake saw the request after the turn had ended.
        Leg a later read: the completion is stamped 1 ms after the send, the
        completion-after-send witness, so mid_turn is true."""
        # Registration 0.1; turn_completed and the turn's response at 1.05;
        # the reply 0.3 s after the send; prompt_complete at 1.55.
        base = {"reg_s": 0.05, "run_s": 0.95, "pc_gap_s": 0.5,
                "inter_ack_s": 0.3}
        legs = {"same read": (0.95, False, [], False),
                "a later read": (0.949, True, ["completion_after_send"], True)}
        for leg, (at, mid, evidence, early) in legs.items():
            out = self._pumped_turn(
                dict(base, tool_calls=[_bz5_call(at)]),
                [_bz5_arm({"after_tool_calls": 1}, "BZ5-AWAIT")])
            rec, b = out["recs"][0], out["binary"]
            with self.subTest(leg=leg, part="the request was written"):
                self.assertEqual(len(out["sent"]), 1)
                self.assertAlmostEqual(out["sent"][0][0], b.t_reg[0] + at)
                self.assertEqual(rec["status"], "fired")
                self.assertEqual(rec["resp"],
                                 {"jsonrpc": "2.0", "id": rec["request_id"],
                                  "result": {"status": "queued"}})
            with self.subTest(leg=leg, part="the completion is not lost"):
                self.assertEqual(out["collect_reads"], 0,
                                 "prompt_collect read again for a completion "
                                 "the pump had already read")
                self.assertEqual(out["collect_waited"], 0.0)
                self.assertEqual(out["stop"], "end_turn")
                self.assertEqual(out["resp"]["result"]["_meta"]["promptId"],
                                 out["pid"])
                self.assertEqual(out["inbox"], {"responses": {},
                                                "completion": None,
                                                "prompt_response": None})
            with self.subTest(leg=leg, part="the witness"):
                self.assertEqual(rec["mid_turn"], mid)
                self.assertEqual(rec["mid_turn_evidence"], evidence)
                self.assertEqual(b.early_step_fired, early)

    def test_two_interjections_in_order_with_out_of_order_replies(self):
        """F-4 and D-3: after_tool_calls 1 fires on the tool call at 0.1 and
        after_s 2 on the first line at or after 2 s (the chunk at 2.5: after_s
        is judged per line, and fired_at records it). Both requests are
        written in trigger order; their replies arrive in the other order and
        each is matched to its own arm by JSON-RPC id. A third arm, armed
        beyond the turn's end at 9, is closed by the completion: missed,
        turn_completed, not harness-caused. Both fired arms are mid-turn: the
        completion is stamped after each send, and the queue row at 1.5 that
        still runs this prompt witnesses only the arm sent before it."""
        # Registration and the tool call at 0.1; chunks at 1.1, 2.5, 5.1 and
        # 8.1; turn_completed at 9.0. The first request is answered after
        # 3.0 s, the second after 0.2 s. At 1.5 a prompt queues behind the
        # running one (a queue row running this prompt, with one entry).
        def queue_row(sess, b):
            b._notify(1.5, "_x.ai/queue/changed",
                      {"sessionId": b.session_id,
                       "entries": [{"id": "replay-queued-0001",
                                    "kind": "prompt", "position": 0,
                                    "text": "NEXT", "version": 0}],
                       "runningPromptId": sess._inflight.expected_prompt_id,
                       "runningText": "M", "runningKind": "prompt"})

        out = self._pumped_turn(
            {"reg_s": 0.05, "run_s": 8.9, "pc_gap_s": 0.15,
             "turn_chunks": [[(1.0, "C0"), (2.4, "C1"), (5.0, "C2"),
                              (8.0, "C3")]],
             "tool_calls": [_bz5_call(0.0)],
             "inter_replies": [{"after": 3.0}, {"after": 0.2}]},
            [_bz5_arm({"after_tool_calls": 1}, "BZ5-FIRST"),
             _bz5_arm({"after_s": 2.0}, "BZ5-SECOND"),
             _bz5_arm({"after_s": 20.0}, "BZ5-BEYOND")],
            before=queue_row)
        first, second, beyond = out["recs"]
        with self.subTest(leg="both sends, in trigger order"):
            self.assertEqual([(round(t, 6), m, x) for t, m, x in out["sent"]],
                             [(0.1, "_x.ai/interject", "BZ5-FIRST"),
                              (2.5, "_x.ai/interject", "BZ5-SECOND")])
            self.assertEqual([first["request_id"], second["request_id"]],
                             [r["id"] for _, r in out["binary"].interjects])
        with self.subTest(leg="replies out of order, matched by id"):
            self.assertLess(second["reply_at"], first["reply_at"])
            for rec in (first, second):
                self.assertEqual(rec["status"], "fired")
                self.assertEqual(rec["resp"],
                                 {"jsonrpc": "2.0", "id": rec["request_id"],
                                  "result": {"status": "queued"}})
        with self.subTest(leg="after_s judged per line, recorded"):
            self.assertEqual(first["fired_at"]["n_tool_calls"], 1)
            self.assertAlmostEqual(first["fired_at"]["elapsed_s"], 0.05)
            self.assertAlmostEqual(second["fired_at"]["elapsed_s"], 2.45)
        with self.subTest(leg="the arm beyond the turn"):
            self.assertEqual(
                {k: beyond[k] for k in ("status", "disarm_reason",
                                        "harness_caused", "cause")},
                {"status": "missed", "disarm_reason": "turn_completed",
                 "harness_caused": False, "cause": None})
        with self.subTest(leg="mid-turn, each with its own arm_id"):
            self.assertEqual([r["arm_id"] for r in out["recs"]],
                             ["step0.arm0", "step0.arm1", "step0.arm2"])
            self.assertEqual([first["mid_turn"], second["mid_turn"]],
                             [True, True])
            self.assertEqual(sorted(first["mid_turn_evidence"]),
                             ["completion_after_send",
                              "running_prompt_after_send"])
            self.assertEqual(second["mid_turn_evidence"],
                             ["completion_after_send"])
        with self.subTest(leg="the turn"):
            self.assertEqual(out["stop"], "end_turn")
            self.assertEqual(out["collect_reads"], 0)
            self.assertEqual(out["texts"], ["C0", "C1", "C2", "C3"])

    def test_tool_never_arrives_is_mismatched_not_missed(self):
        """F-2: the model answers in prose and calls no tool. The after_tool
        arm on get_command_or_subagent_output ends mismatched_tool and the
        after_tool_calls arm missed: two different statuses, both closed by the
        turn's completion (disarm_reason turn_completed), neither
        harness-caused nor carrying a runner cause, so the verdict engine reads
        a model-path miss (VACUOUS on the declared premise), never BLOCKED.
        Nothing is sent."""
        out = self._pumped_turn(
            {"reg_s": 0.05, "run_s": 2.0, "pc_gap_s": 0.15,
             "turn_chunks": [[(0.5, "prose one"), (1.5, "prose two")]]},
            [_bz5_arm({"after_tool": "get_command_or_subagent_output"},
                      "BZ5-TOOL"),
             _bz5_arm({"after_tool_calls": 1}, "BZ5-CALLS")])
        keys = ("status", "fired", "disarm_reason", "harness_caused", "cause",
                "mid_turn")
        self.assertEqual(
            [{k: r[k] for k in keys} for r in out["recs"]],
            [{"status": "mismatched_tool", "fired": False,
              "disarm_reason": "turn_completed", "harness_caused": False,
              "cause": None, "mid_turn": False},
             {"status": "missed", "fired": False,
              "disarm_reason": "turn_completed", "harness_caused": False,
              "cause": None, "mid_turn": False}])
        self.assertEqual(out["sent"], [])
        self.assertEqual(out["stop"], "end_turn")
        self.assertEqual(out["texts"], ["prose one", "prose two"])

    def test_send_timeout_is_a_harness_cause(self):
        """F-9: the request is written and never answered while the turn keeps
        streaming. Past interject_timeout_s (arm-level) the arm lands as
        send_timeout with the runner cause interject-send-timeout: step0.arm0
        and its id is unregistered; the pump keeps reading the live turn
        (every chunk after the timeout is still collected) and the turn
        returns its own stop reason."""
        # Tool call 0.3; chunks 1.1 to 7.1; turn_completed 8.0. The reply
        # budget ends at 2.3 and is judged at the next line (3.1).
        out = self._pumped_turn(
            {"reg_s": 0.05, "run_s": 7.9, "pc_gap_s": 0.15,
             "turn_chunks": [[(1.0 + i, "C%d" % i) for i in range(7)]],
             "tool_calls": [_bz5_call(0.2)],
             "inter_replies": [{"reply": "silent"}]},
            [_bz5_arm({"after_tool_calls": 1}, "BZ5-SILENT",
                      interject_timeout_s=2.0)])
        rec = out["recs"][0]
        with self.subTest(leg="send_timeout, a harness cause"):
            self.assertEqual(
                {k: rec[k] for k in ("status", "fired", "disarm_reason",
                                     "harness_caused", "cause", "resp",
                                     "reply_at", "mid_turn")},
                {"status": "send_timeout", "fired": False,
                 "disarm_reason": None, "harness_caused": True,
                 "cause": "interject-send-timeout: step0.arm0", "resp": None,
                 "reply_at": None, "mid_turn": False})
            self.assertEqual(len(out["sent"]), 1)
            self.assertNotIn(rec["request_id"], out["inbox"]["responses"])
        with self.subTest(leg="the live turn is still read"):
            self.assertEqual(out["texts"], ["C%d" % i for i in range(7)])
            self.assertEqual(out["stop"], "end_turn")
            self.assertEqual(out["collect_reads"], 0)

    def test_ext_error_disarms_as_harness_caused(self):
        """F-8: the request is answered with a JSON-RPC error (session not
        found) while the turn continues. The arm is disarmed with
        disarm_reason ext_error, harness-caused, with the runner cause
        interject-unavailable: step0.arm0.ext_error and the error reply kept
        on its record; the turn is still read to its own clean terminal, so an
        ext-request error never reads as a turn error."""
        out = self._pumped_turn(
            {"reg_s": 0.05, "run_s": 3.9, "pc_gap_s": 0.15,
             "turn_chunks": [[(1.0, "C0"), (2.0, "C1")]],
             "tool_calls": [_bz5_call(0.2)],
             "inter_replies": [{"reply": "error", "after": 0.2}]},
            [_bz5_arm({"after_tool_calls": 1}, "BZ5-REFUSED")])
        rec, b = out["recs"][0], out["binary"]
        self.assertEqual(
            {k: rec[k] for k in ("status", "fired", "disarm_reason",
                                 "harness_caused", "cause", "resp")},
            {"status": "disarmed", "fired": False,
             "disarm_reason": "ext_error", "harness_caused": True,
             "cause": "interject-unavailable: step0.arm0.ext_error",
             "resp": {"jsonrpc": "2.0", "id": rec["request_id"],
                      "error": {"code": -32602, "message": "Invalid params",
                                "data": "session not found: %s"
                                        % b.session_id}}})
        self.assertEqual(out["stop"], "end_turn")
        self.assertNotIn("error", out["resp"])
        self.assertEqual(out["texts"], ["C0", "C1"])

    def test_turn_timeout_disarms_arms(self):
        """F-10 and D-5: the turn's 5 s budget runs out inside the pump's read
        while both arms are armed. Each closes with disarm_reason turn_timeout,
        not harness-caused (missed; mismatched_tool for the after_tool arm),
        nothing is sent, and prompt_collect, handed the budget left, returns
        the existing timeout terminal without reading."""
        out = self._pumped_turn(
            {"reg_s": 0.05, "run_s": 59.9, "pc_gap_s": 0.15,
             "turn_chunks": [[(1.0, "C0"), (2.0, "C1")]]},
            [_bz5_arm({"after_tool_calls": 1}, "BZ5-CALLS"),
             _bz5_arm({"after_tool": "wait_agent"}, "BZ5-TOOL")],
            budget=5.0)
        keys = ("status", "disarm_reason", "harness_caused", "cause")
        self.assertEqual(
            [{k: r[k] for k in keys} for r in out["recs"]],
            [{"status": "missed", "disarm_reason": "turn_timeout",
              "harness_caused": False, "cause": None},
             {"status": "mismatched_tool", "disarm_reason": "turn_timeout",
              "harness_caused": False, "cause": None}])
        self.assertEqual(out["sent"], [])
        self.assertGreaterEqual(out["pumped_at"], 5.05)
        self.assertEqual(out["left"], 0.0)
        self.assertEqual((out["resp"], out["stop"]),
                         ({"error": {"message": "timeout after %ss"
                                                % out["left"]}}, None))
        self.assertEqual(out["collect_reads"], 0)
        self.assertEqual(out["texts"], ["C0", "C1"])

    def test_error_completion_still_reads_the_grace_lines(self):
        """F-16 on the pumped path, in the real binary's order (amendment A-1
        r2 section 5): the error turn_completed at 1.1 carries no agentResult,
        so the turn's waiter asks for its grace read; the interject reply,
        interleaved at 1.5, is still routed to its arm; the pump reads on to
        the matching prompt_complete at 1.9, whose provider detail upgrades
        the completion and reaches acp.log, and stops there, before the
        session/prompt JSON-RPC error sent at 2.0. prompt_collect, with no
        read of its own, surfaces error.message == the detail. The second
        arm's after_s holds from 1.35, inside the grace window, yet the error
        terminal closed it first (D-2: missed, turn_error) and it is never
        sent."""
        error = {"code": -32603, "message": "Internal error",
                 "data": {"message": _BZ5_DETAIL, "http_status": 503}}
        out = self._pumped_turn(
            {"reg_s": 0.05, "run_s": 1.0, "pc_gap_s": 0.8,
             "turn_stops": ["error"], "turn_agent_result": _BZ5_DETAIL,
             "tool_calls": [_bz5_call(0.2)], "inter_ack_s": 1.2,
             "prompt_error": error, "prompt_error_s": 0.1},
            [_bz5_arm({"after_tool_calls": 1}, "BZ5-GRACE"),
             _bz5_arm({"after_s": 1.3}, "BZ5-NEVER")])
        fired, closed = out["recs"]
        with self.subTest(leg="the pump performed the grace read"):
            self.assertEqual(out["stop"], "error")
            self.assertEqual(out["resp"], {"error": {
                "message": _BZ5_DETAIL, "via": "prompt_complete",
                "promptId": out["pid"]}})
            self.assertEqual(out["collect_reads"], 0)
            with open(out["sess"].log_path, encoding="utf-8") as fh:
                detail_lines = [json.loads(line) for line in fh
                                if _BZ5_DETAIL in line
                                and not line.startswith("SEND: ")]
            self.assertEqual([d.get("method") for d in detail_lines],
                             ["_x.ai/session/prompt_complete"])
        with self.subTest(leg="the reply interleaved in the grace window"):
            self.assertEqual(fired["status"], "fired")
            self.assertEqual(fired["resp"]["result"], {"status": "queued"})
            self.assertGreater(fired["reply_at"], out["binary"].t_end[0])
            self.assertTrue(fired["mid_turn"])
        with self.subTest(leg="no arm fires during the grace window"):
            self.assertEqual([x for _, _, x in out["sent"]], ["BZ5-GRACE"])
            self.assertEqual(
                {k: closed[k] for k in ("status", "disarm_reason",
                                        "harness_caused", "cause")},
                {"status": "missed", "disarm_reason": "turn_error",
                 "harness_caused": False, "cause": None})

    def test_interject_during_compaction_sequence(self):
        """F-5: auto-compaction updates inside the tool loop are never the
        turn's terminal. Leg mid-turn: the arm fires on the tool call at 0.3,
        compaction starts and completes, the reply at 1.1 is still routed to
        the arm, the turn ends on its own turn_completed at 2.1, and the arm is
        mid-turn on two witnesses (the interjection broadcast before the
        completion; the completion stamped after the send). Leg
        post-completion: the second tool call shares the turn's final read, so
        its arm's request goes out with the completion already stamped and the
        reply arrives after it: queued, yet mid_turn is false (a queued reply
        is no witness); the arm still armed at the completion is closed and
        nothing more is sent (D-1), which would otherwise start an
        interject-fallback turn."""
        def compaction(started_at, completed_at):
            def schedule(sess, b):
                for at, kind in ((started_at, "started"),
                                 (completed_at, "completed")):
                    b.stdout.schedule(at, _bz5_compaction(
                        b.session_id, int(round(at * 1000)), kind))
            return schedule

        mid = self._pumped_turn(
            {"reg_s": 0.05, "run_s": 2.0, "pc_gap_s": 0.15,
             "tool_calls": [_bz5_call(0.2)], "inter_ack_s": 0.8,
             "inter_broadcast": True},
            [_bz5_arm({"after_tool_calls": 1}, "BZ5-COMPACT")],
            before=compaction(0.5, 0.9))
        rec = mid["recs"][0]
        with self.subTest(leg="mid-turn"):
            self.assertEqual(rec["status"], "fired")
            self.assertEqual(rec["resp"]["result"], {"status": "queued"})
            self.assertAlmostEqual(rec["reply_at"], 1.1)
            self.assertTrue(rec["mid_turn"])
            self.assertEqual(sorted(rec["mid_turn_evidence"]),
                             ["completion_after_send",
                              "interjection_broadcast_before_completion"])
            self.assertEqual(mid["resp"], {"result": {
                "stopReason": "end_turn", "_via": "turn_completed",
                "promptId": mid["pid"]}})
            self.assertGreaterEqual(mid["pumped_at"], mid["binary"].t_end[0])
        post = self._pumped_turn(
            {"reg_s": 0.05, "run_s": 0.9, "pc_gap_s": 0.15,
             "tool_calls": [_bz5_call(0.2), _bz5_call(0.9, _BZ5_B)],
             "inter_ack_s": 0.5, "inter_broadcast": True},
            [_bz5_arm({"after_tool_calls": 2}, "BZ5-LAST-CALL"),
             _bz5_arm({"after_s": 1.2}, "BZ5-AFTER-END")],
            before=compaction(0.5, 0.8))
        last, after_end = post["recs"]
        with self.subTest(leg="post-completion"):
            self.assertEqual(last["status"], "fired")
            self.assertEqual(last["resp"]["result"], {"status": "queued"})
            self.assertGreater(last["reply_at"], post["binary"].t_end[0])
            self.assertFalse(last["mid_turn"],
                             "a queued reply was taken as mid-turn proof")
            self.assertEqual(last["mid_turn_evidence"], [])
            self.assertFalse(post["binary"].early_step_fired)
            self.assertEqual(
                {k: after_end[k] for k in ("status", "disarm_reason",
                                           "harness_caused")},
                {"status": "missed", "disarm_reason": "turn_completed",
                 "harness_caused": False})
            self.assertEqual([x for _, _, x in post["sent"]],
                             ["BZ5-LAST-CALL"])

    def test_multibyte_split_across_reads(self):
        """F-12: an agent_message_chunk whose UTF-8 text is split inside a
        three-byte character across two reads, between the trigger and the
        interject reply, is decoded intact by the one reader (its decoder and
        its partial line outlive each read), and the reply after it is still
        parsed and routed."""
        text = "BZ5 na\u00efve caf\u00e9 \u2014 \u2713 \u65e5\u672c\u8a9e"
        raw = {}

        def split_chunk(sess, b):
            line = json.dumps(
                {"jsonrpc": "2.0", "method": "session/update",
                 "params": {"sessionId": b.session_id,
                            "update": {"sessionUpdate": "agent_message_chunk",
                                       "content": {"type": "text",
                                                   "text": text}}}},
                ensure_ascii=False).encode("utf-8") + b"\n"
            cut = line.index("\u2713".encode("utf-8")) + 1
            raw["head"], raw["tail"] = line[:cut], line[cut:]
            # FakeStdout.schedule takes text; these writes are raw bytes.
            b.stdout._chunks.append((0.6, raw["head"]))
            b.stdout._chunks.append((0.8, raw["tail"]))

        out = self._pumped_turn(
            {"reg_s": 0.05, "run_s": 2.9, "pc_gap_s": 0.15,
             "tool_calls": [_bz5_call(0.2)], "inter_ack_s": 1.0},
            [_bz5_arm({"after_tool_calls": 1}, "BZ5-UTF8")],
            before=split_chunk)
        with self.assertRaises(UnicodeDecodeError):
            raw["head"].decode("utf-8")
        self.assertEqual(out["texts"], [text])
        rec = out["recs"][0]
        self.assertEqual(rec["status"], "fired")
        self.assertEqual(rec["resp"]["result"], {"status": "queued"})
        self.assertAlmostEqual(rec["reply_at"], 1.3)
        self.assertEqual(out["stop"], "end_turn")

    def test_eof_mid_pump_is_clean(self):
        """F-11 and D-6: the process exits mid-turn, its last line torn, while
        the arm is still un-fired. The pump sees end of file when it happens,
        with no traceback, and disarms the arm as session_died, harness-caused,
        with the runner cause interject-unavailable: step0.arm0.session_died;
        the torn line is dropped, never tracked; prompt_collect meets the same
        end of file and returns the timeout terminal at once."""
        def dying_turn(sess, b):
            b._notify(0.1, "_x.ai/queue/changed",
                      {"sessionId": b.session_id, "entries": [],
                       "runningPromptId": sess._inflight.expected_prompt_id,
                       "runningText": "M", "runningKind": "prompt"})
            b.stdout.schedule(0.6, _bz3_chunk(b.session_id, "C0"))
            b.stdout.schedule(1.0, _bz3_chunk(b.session_id, "TORN")[:-12])
            b.stdout.eof()

        out = self._pumped_turn(
            {}, [_bz5_arm({"after_tool_calls": 1}, "BZ5-EOF")],
            before=dying_turn, binary_cls=_DeferredTurnBinary,
            eof_select=True)
        rec = out["recs"][0]
        self.assertEqual(
            {k: rec[k] for k in ("status", "fired", "disarm_reason",
                                 "harness_caused", "cause")},
            {"status": "disarmed", "fired": False,
             "disarm_reason": "session_died", "harness_caused": True,
             "cause": "interject-unavailable: step0.arm0.session_died"})
        self.assertEqual(out["pumped_at"], 1.0,
                         "end of file was not seen when it happened")
        self.assertEqual(out["texts"], ["C0"])
        self.assertEqual(out["sent"], [])
        self.assertEqual((out["resp"], out["stop"]),
                         ({"error": {"message": "timeout after %ss"
                                                % out["left"]}}, None))
        self.assertEqual(out["collect_waited"], 0.0)


# ---------------------------------------------------------------------------
# Case-file contract — offline validation over the FULL case set
# ---------------------------------------------------------------------------

class CaseContractTest(unittest.TestCase):
    def test_all_case_files_validate(self):
        """0 regressions over the full existing case set (the campaign's
        acceptance pins: rt-xreplay1 post-fix shape, rt-crosswire1/1b,
        rt-c3, rt-m*). Every cases/*.json must satisfy the runner's
        case contract."""
        files = sorted(run.globmod.glob(os.path.join(run.CASES_DIR, "*.json")))
        self.assertGreaterEqual(
            len(files), 20,
            "expected the full acceptance-pin case set, found %d"
            % len(files))
        seen_ids = {}
        failures = {}
        for path in files:
            errs = run.validate_case_file(path)
            if errs:
                failures[os.path.basename(path)] = errs
            with open(path) as fh:
                cid = json.load(fh).get("id")
            if cid in seen_ids:
                failures.setdefault(
                    os.path.basename(path), []).append(
                    "duplicate case id %r (also in %s)"
                    % (cid, seen_ids[cid]))
            else:
                seen_ids[cid] = os.path.basename(path)
        self.assertEqual(
            failures, {},
            "case contract violations:\n" + "\n".join(
                "%s: %s" % (f, e) for f, es in sorted(failures.items())
                for e in es))

    def test_validator_rejects_broken_case(self):
        """The validator must actually fail on contract violations
        (unknown op, bad where-filter shape, missing keys)."""
        tmp = tempfile.mkdtemp(prefix="rt-casebad-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        bad = os.path.join(tmp, "bad.json")
        with open(bad, "w") as fh:
            json.dump({"id": "RT-BAD", "driver": "acp",
                       "steps": [{"op": "turnover"},
                                 {"op": "switch", "model": "m",
                                  "via": "warp"}],
                       "assert": {"ndjson": [{"op": "absent"}],
                                  "wire": [{"kind": "grep",
                                            "where": {"$.x": {"bogus": 1}}}]}}
                       , fh)
        errs = run.validate_case_file(bad)
        joined = " | ".join(errs)
        self.assertTrue(any("unknown op" in e for e in errs), joined)
        self.assertTrue(any("via" in e for e in errs), joined)
        self.assertTrue(any("event" in e for e in errs), joined)
        self.assertTrue(any("where" in e for e in errs), joined)
        self.assertTrue(
            any("acp case needs a string 'model'" in e for e in errs),
            joined)


def _have_jsonschema():
    try:
        import jsonschema  # noqa: F401
        return True
    except Exception:
        return False


def _base_new_case():
    """Minimal schema_version=1 case that passes the NEW-case gate
    (apex-ayl.22 SDD D-1/D-2); the T-S1 broken fixtures mutate this."""
    return {
        "schema_version": 1,
        "id": "TS1-BASE",
        "title": "dual-validator fixture base",
        "bead": "apex-ayl.22",
        "suite": "wire-shape",
        "tier": "slim",
        "driver": "headless",
        "model": "m-test",
        "wire": "responses",
        "steps": [{"op": "turn", "prompt": "Reply with exactly: TS1-OK"}],
        "assert": {
            "wire": [{
                "id": "w1",
                "kind": "field",
                "file": "req-*.json",
                "where": {"method": "POST", "body.model": "m-test"},
                "nth": 0,
                "path": "$.body.model",
                "eq": "m-test",
            }],
        },
    }


def _base_mcp_tool_case():
    """Minimal schema_version=1 mcp-tool case (the slim shape, one call
    per class); T-S2 mutates individual NEW-gate rules off it."""
    c = _base_new_case()
    c.update({
        "id": "TS2-MCP",
        "suite": "mcp-tool",
        "mcp_calls": [{
            "id": "cg1", "server": "codegraph",
            "tool": "codegraph_explore",
            "expect_name": "codegraph__codegraph_explore",
            "via": "search_tool_use_tool",
            "args": {"query": "strip_model_bound_state"},
            "premise": "model",
            "output_contains": "strip_model_bound_state",
        }],
        "tool_calls": [{
            "id": "sh1", "tool": "run_terminal_command",
            "args": {"command": "echo TS2-ECHO"},
            "premise": "model",
            "output_contains": "TS2-ECHO",
        }],
        "scoring": {"require_wire_evidence": True, "vacuous_if": [
            {"pin": "cg_ann", "class": "harness"},
            {"pin": "cg_call", "class": "model"},
        ]},
    })
    c["assert"]["wire"].insert(0, {
        "id": "cg_ann", "kind": "grep", "file": "req-*.json",
        "where": {"method": "POST", "path": "/v1/responses",
                  "body.model": "m-test"},
        "any": True, "grep": "- codegraph (",
    })
    c["assert"]["wire"].insert(1, {
        "id": "cg_call", "kind": "grep", "file": "req-*.json",
        "where": {"method": "POST", "path": "/v1/responses",
                  "body.model": "m-test"},
        "any": True, "grep": '\\"tool_name\\":',
    })
    return c


def _write_case(tmp, name, case):
    p = os.path.join(tmp, name)
    with open(p, "w") as fh:
        json.dump(case, fh)
    return p


class DualValidatorTest(unittest.TestCase):
    """T-S1 (apex-ayl.22 D-1): dual-validator equivalence.

    The jsonschema engine and the hand-rolled keyword-subset engine must
    agree on pass/fail for: the 6 slim drop-in cases + every in-tree
    case + the 7 broken fixtures (bad id pattern; missing `where` on a
    scored wire pin; dangling vacuous_if pin; empty mcp_calls on an
    mcp-tool suite; non-absolute cwd; array config_patch; unknown
    suite). Pre-fix RED: the hand-rolled engine does not exist and the
    NEW-case rules are absent (the broken fixtures pass validation)."""

    BROKEN = [
        "bad-id",
        "missing-where",
        "dangling-pin",
        "empty-mcp",
        "bad-cwd",
        "array-cp",
        "unknown-suite",
    ]

    def _broken_case(self, kind):
        c = _base_new_case()
        if kind == "bad-id":
            c["id"] = "lower_case"
        elif kind == "missing-where":
            del c["assert"]["wire"][0]["where"]
        elif kind == "dangling-pin":
            c["scoring"] = {"vacuous_if": [
                {"pin": "no_such_pin", "class": "model"}]}
        elif kind == "empty-mcp":
            c["suite"] = "mcp-tool"
            c["mcp_calls"] = []
        elif kind == "bad-cwd":
            c["cwd"] = "relative/path"
        elif kind == "array-cp":
            c["config_patch"] = {"features/turn_summary": [False]}
        elif kind == "unknown-suite":
            c["suite"] = "bogus-suite"
        return c

    def _engine_verdicts(self, tmp):
        """{basename: (jsonschema_pass, hand_pass)} over the in-tree
        case set + the 7 broken fixtures."""
        if not _have_jsonschema():
            self.skipTest("jsonschema not importable in this env")
        verdicts = {}
        files = sorted(run.globmod.glob(
            os.path.join(run.CASES_DIR, "*.json")))
        for kind in self.BROKEN:
            files.append(_write_case(
                tmp, "broken-%s.json" % kind, self._broken_case(kind)))
        for path in files:
            name = os.path.basename(path)
            ea = run.validate_case_file(path, engine="jsonschema")
            eb = run.validate_case_file(path, engine="hand")
            verdicts[name] = (not ea, not eb)
        return verdicts

    def test_dual_paths_agree_pass_fail(self):
        tmp = tempfile.mkdtemp(prefix="rt-ts1-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        verdicts = self._engine_verdicts(tmp)
        slim = [n for n in verdicts if n.startswith("t21-")]
        self.assertGreaterEqual(
            len(slim), 5, "the slim drop-in cases must be in the tree "
                         "(was 6; t21-cc-gemini37 retired 2026-09-17 — "
                         "gemini family cc->responses reversal)")
        for name, (pa, pb) in sorted(verdicts.items()):
            self.assertEqual(
                pa, pb,
                "%s: jsonschema path pass=%s vs hand-rolled pass=%s "
                "(dual-validator divergence)" % (name, pa, pb))

    def test_slim_pass_both_broken_rejected_both(self):
        tmp = tempfile.mkdtemp(prefix="rt-ts1b-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        if not _have_jsonschema():
            self.skipTest("jsonschema not importable in this env")
        verdicts = self._engine_verdicts(tmp)
        for name, (pa, pb) in sorted(verdicts.items()):
            if name.startswith("t21-"):
                self.assertTrue(pa and pb,
                                "%s must PASS on both paths" % name)
            if name.startswith("broken-"):
                self.assertFalse(pa and pb,
                                 "%s must be REJECTED on both paths" % name)


class NewCaseGateTest(unittest.TestCase):
    """T-S2 (apex-ayl.22 D-2): NEW-case gate pass/fail split.

    5 slim pass (was 6; t21-cc-gemini37 retired 2026-09-17 — gemini
    family cc->responses reversal); a legacy case (no schema_version)
    skips the NEW rules;
    a schema_version=1 case failing a NEW rule is rejected with the
    rule named. Pre-fix RED: the gate does not exist — every mutation
    below validates clean."""

    def _gate(self, case, tmp):
        p = _write_case(tmp, "ts2-%s.json"
                        % (case.get("id", "X").lower()[:20] + "-"
                           + str(id(case))[-6:]), case)
        return run.validate_case_file(p)

    def test_slim_pass_new_gate(self):
        tmp = tempfile.mkdtemp(prefix="rt-ts2-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        slim = sorted(run.globmod.glob(
            os.path.join(run.CASES_DIR, "t21-*.json")))
        self.assertGreaterEqual(len(slim), 5,
                                "slim drop-in set must be in the tree "
                                "(was 6; t21-cc-gemini37 retired 2026-09-17)")
        for path in slim:
            self.assertEqual(run.validate_case_file(path), [],
                             os.path.basename(path))

    def test_legacy_case_skips_new_rules(self):
        """A pre-version case (no schema_version, no bead/suite/tier)
        must stay valid: back-compat, no new failures."""
        tmp = tempfile.mkdtemp(prefix="rt-ts2l-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        legacy = {"id": "RT-LEGACY", "title": "legacy shape",
                  "driver": "headless", "model": "m-x",
                  "steps": [{"op": "turn", "prompt": "hi"}],
                  "assert": {"wire": [{"kind": "field",
                                        "file": "req-*.json",
                                        "path": "$.body.model",
                                        "eq": "m-x"}]}}
        self.assertEqual(self._gate(legacy, tmp), [])

    def test_new_rule_violations_named(self):
        tmp = tempfile.mkdtemp(prefix="rt-ts2v-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))

        def violates(case, needle):
            errs = self._gate(case, tmp)
            joined = " | ".join(errs)
            self.assertTrue(
                any(needle in e for e in errs),
                "expected a violation naming %r, got: %s" % (needle, joined))

        c = _base_new_case()
        del c["bead"]
        violates(c, "bead")

        c = _base_mcp_tool_case()
        c["mcp_calls"] = []
        violates(c, "mcp_calls")

        c = _base_mcp_tool_case()
        c["suite"] = "tool-call"
        c["tool_calls"] = []
        violates(c, "tool_calls")

        c = _base_new_case()
        del c["assert"]["wire"][0]["where"]
        violates(c, "where")

        c = _base_new_case()
        c["scoring"] = {"vacuous_if": [
            {"pin": "ghost", "class": "model"}]}
        violates(c, "vacuous_if")

        c = _base_new_case()
        c["cwd"] = "relative/path"
        violates(c, "cwd")

        c = _base_new_case()
        c["env"] = {"provider_vars_unset": ["OPENAI_API_KEY"]}
        violates(c, "provider_vars_unset")

        c = _base_new_case()
        c["assert"]["ndjson"] = [
            {"id": "w1", "op": "count", "event": "end", "min": 1}]
        violates(c, "unique")

        c = _base_new_case()
        c["driver"] = "acp"
        del c["model"]
        violates(c, "model")

        c = _base_new_case()
        c["driver"] = "acp"
        violates(c, "cwd")


# ---------------------------------------------------------------------------
# apex-bdfz BZ-6 — the interject block: schema (hand subset) + mirrored rules
# ---------------------------------------------------------------------------

_BZ6_NONCE = "BZ6-INTERJECT-NONCE"
_BZ6_TEXT = ("%s - the operator interjected this line mid-wait. Include the "
             "nonce %s verbatim in your next reply, then finish the wait."
             % (_BZ6_NONCE, _BZ6_NONCE))


def _bz6_text_of_bytes(n):
    """An echo-carrying interjection text of exactly n UTF-8 bytes, padded
    with two-byte characters so that it holds fewer than n characters (the
    section 2.7 bound counts bytes)."""
    base = _BZ6_TEXT + " "
    need = n - len(base.encode("utf-8"))
    return base + "\u00e9" * (need // 2) + "x" * (need % 2)


def _bz6_case(cwd):
    """A schema_version=1 ACP case whose turn step carries three interject
    arms: the three trigger keys, both send kinds and every optional arm key
    (plan r3 section 2.7). It scores the nonce its interject texts echo (F-3)
    and declares the model premise as the where-filtered count (section 2.5.1
    item 5, erratum E-1)."""
    c = _base_new_case()
    c.update({
        "id": "BZ6-INTERJECT",
        "title": "BZ-6 interject contract fixture",
        "bead": "apex-bdfz",
        "driver": "acp",
        "cwd": cwd,
        "steps": [{
            "op": "turn",
            "prompt": "Start the worker in the background, then wait for it.",
            "interject": [
                {"at": {"after_tool": "get_command_or_subagent_output"},
                 "send": "interject", "text": _BZ6_TEXT,
                 "interjection_id": "bz6-arm0", "interject_timeout_s": 30,
                 "require": True},
                {"at": {"after_tool_calls": 2}, "text": _BZ6_TEXT},
                {"at": {"after_s": 2.5}, "send": "set_model",
                 "model": "m-test-2"},
            ],
        }],
        "scoring": {"require_wire_evidence": True, "vacuous_if": [
            {"pin": "interject_fired", "class": "model",
             "note": "an arm never fired: the probe never happened"}]},
    })
    c["assert"]["ndjson"] = [
        {"id": "nonce_echoed", "op": "text_contains", "value": _BZ6_NONCE},
        {"id": "interject_fired", "op": "count", "event": "acp_interject",
         "where": {"status": "fired"}, "min": 3, "max": 3},
    ]
    c["assert"]["wire"].append({
        "id": "nonce_in_req", "kind": "grep", "file": "req-*.json",
        "where": {"method": "POST", "body.model": "m-test"}, "any": True,
        "grep": _BZ6_NONCE})
    return c


# Each broken variant -> the substrings one of its errors must carry on every
# engine (its named reason). The first eleven are plan r3 section 4 BZ-6 RED
# (b), in that order; the rest cover the remaining section 2.7 mirror rules,
# both halves of the echo predicate, and section 2.6's send: resume.
_BZ6_BROKEN = {
    "two-trigger-keys": ("exactly one trigger key",),
    "interject-without-text": ("send interject needs a non-empty string",),
    "set-model-with-text": ("send set_model needs a string 'model'",),
    "after-s-zero": ("after_s", "minimum"),
    "interject-on-compact": ("only legal on op turn",),
    "headless-driver": ("headless is one process per turn",),
    "arm-additional-property": ("interject", "bogus"),
    "interject-object": ("interject must be a non-empty list of arms",),
    "interject-empty-list": ("interject must be a non-empty list of arms",),
    "no-echo": ("echo rule (F-3)",),
    "text-4097-bytes": ("4097 bytes", "4096-byte bound"),
    "interject-with-model": ("send interject needs a non-empty string",),
    "set-model-without-model": ("send set_model needs a string 'model'",),
    "text-copies-prompt": ("copy of the step prompt",),
    "kill-after-s": ("kill_after_s or expect_kill",),
    "expect-kill": ("kill_after_s or expect_kill",),
    "nonce-without-verbatim": ("echo rule (F-3)",),
    "verbatim-without-nonce": ("echo rule (F-3)",),
    "send-resume": ("send", "resume"),
}


def _bz6_broken_case(kind, cwd):
    c = _bz6_case(cwd)
    step = c["steps"][0]
    arms = step["interject"]
    if kind == "two-trigger-keys":
        arms[0]["at"]["after_s"] = 2
    elif kind == "interject-without-text":
        del arms[0]["text"]
    elif kind == "set-model-with-text":
        arms[2]["text"] = _BZ6_TEXT
    elif kind == "after-s-zero":
        arms[2]["at"]["after_s"] = 0
    elif kind == "interject-on-compact":
        step["op"] = "compact"
    elif kind == "headless-driver":
        c["driver"] = "headless"
    elif kind == "arm-additional-property":
        arms[0]["bogus"] = 1
    elif kind == "interject-object":
        step["interject"] = arms[0]
    elif kind == "interject-empty-list":
        step["interject"] = []
    elif kind == "no-echo":
        arms[0]["text"] = "Also tell me about the weather in your reply."
    elif kind == "text-4097-bytes":
        arms[0]["text"] = _bz6_text_of_bytes(4097)
    elif kind == "interject-with-model":
        arms[0]["model"] = "m-test-2"
    elif kind == "set-model-without-model":
        del arms[2]["model"]
    elif kind == "text-copies-prompt":
        step["prompt"] = arms[0]["text"]
    elif kind == "kill-after-s":
        step["kill_after_s"] = 5
    elif kind == "expect-kill":
        step["expect_kill"] = False
    elif kind == "nonce-without-verbatim":
        arms[0]["text"] = "%s was interjected; mention it." % _BZ6_NONCE
    elif kind == "verbatim-without-nonce":
        arms[0]["text"] = "Repeat the word OK verbatim in your next reply."
    elif kind == "send-resume":
        arms[0]["send"] = "resume"
    else:
        raise ValueError(kind)
    return c


def _bz6_engines():
    """The hand-rolled subset is binding (it is what runs when jsonschema is
    absent); the jsonschema path joins whenever it is importable."""
    return ["hand"] + (["jsonschema"] if _have_jsonschema() else [])


class InterjectContractTest(unittest.TestCase):
    """apex-bdfz BZ-6 (plan r3 sections 2.6, 2.7; section 3 F-15, F-17): the
    turn step's interject block. case.schema.json declares it inside the
    fail-closed hand-rolled subset; _validate_step mirrors the step-local
    rules (exactly one trigger key, the 4096-byte text bound and the rest of
    section 2.7's step-local list); _validate_new_case owns the driver rule
    and the echo rule (R-7), because only it sees the whole case."""

    maxDiff = None

    def test_interject_block_valid_and_mirrored(self):
        tmp = tempfile.mkdtemp(prefix="rt-bz6-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        good = _write_case(tmp, "bz6-good.json", _bz6_case(tmp))
        c = _bz6_case(tmp)
        c["steps"][0]["interject"][0]["text"] = _bz6_text_of_bytes(4096)
        bound = _write_case(tmp, "bz6-text-4096-bytes.json", c)
        broken = {}
        for kind in _BZ6_BROKEN:
            broken[kind] = _write_case(tmp, "bz6-%s.json" % kind,
                                       _bz6_broken_case(kind, tmp))
        for engine in _bz6_engines():
            with self.subTest(part="a: well-formed arms", engine=engine):
                self.assertEqual(
                    run.validate_case_file(good, engine=engine), [])
            with self.subTest(part="a: 4096-byte text", engine=engine):
                self.assertEqual(
                    run.validate_case_file(bound, engine=engine), [])
            with self.subTest(part="b: broken variants", engine=engine):
                unnamed = {}
                for kind, needles in sorted(_BZ6_BROKEN.items()):
                    errs = run.validate_case_file(broken[kind], engine=engine)
                    if not any(all(n in e for n in needles) for e in errs):
                        unnamed[kind] = errs
                self.assertEqual(
                    unnamed, {},
                    "not rejected with the named reason on the %s engine"
                    % engine)
        with self.subTest(part="c: no new top-level op"):
            with open(run.SCHEMA_PATH) as fh:
                schema = json.load(fh)
            self.assertEqual(
                schema["definitions"]["step"]["properties"]["op"]["enum"],
                list(run.STEP_OPS))
            self.assertNotIn("interject", run.STEP_OPS)


class InterjectDualValidatorTest(unittest.TestCase):
    """apex-bdfz BZ-6 (plan r3 section 2.7 rule 0, review F-1): the two
    draft-07 engines agree on interject-bearing cases before any such case
    lands in the tree, and case.schema.json stays inside the keyword subset
    the hand-rolled engine accepts."""

    maxDiff = None

    def test_interject_arm_agrees_on_both_paths(self):
        if not _have_jsonschema():
            self.skipTest("jsonschema not importable in this env")
        tmp = tempfile.mkdtemp(prefix="rt-bz6d-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        c = _bz6_case(tmp)
        c["steps"][0]["interject"][0]["text"] = _bz6_text_of_bytes(4096)
        cases = {"good": _bz6_case(tmp), "text-4096-bytes": c}
        cases.update((k, _bz6_broken_case(k, tmp)) for k in _BZ6_BROKEN)
        verdicts, expected = {}, {}
        for name, case in sorted(cases.items()):
            p = _write_case(tmp, "bz6d-%s.json" % name, case)
            verdicts[name] = (
                not run.validate_case_file(p, engine="jsonschema"),
                not run.validate_case_file(p, engine="hand"))
            ok = name in ("good", "text-4096-bytes")
            expected[name] = (ok, ok)
        self.assertEqual(verdicts, expected,
                         "(jsonschema pass, hand pass) per case")

    def test_schema_keywords_in_hand_subset(self):
        """Every keyword case.schema.json uses at any schema node is one the
        hand-rolled engine accepts, so an out-of-subset keyword (oneOf,
        minLength, ...) fails here instead of as a fail-closed refusal on the
        first host without jsonschema."""
        with open(run.SCHEMA_PATH) as fh:
            schema = json.load(fh)
        outside = []

        def walk(node, path):
            if not isinstance(node, dict):
                return
            for kw, sub in node.items():
                if kw not in run._D7_HAND_KEYWORDS:
                    outside.append("%s: %s" % (path, kw))
                if kw in ("properties", "definitions"):
                    for name, s in sub.items():
                        walk(s, "%s/%s/%s" % (path, kw, name))
                elif kw in ("items", "additionalProperties"):
                    walk(sub, "%s/%s" % (path, kw))
                elif kw == "anyOf":
                    for i, s in enumerate(sub):
                        walk(s, "%s/anyOf[%d]" % (path, i))

        walk(schema, "#")
        self.assertEqual(outside, [])


# ---------------------------------------------------------------------------
# apex-bdfz BZ-7 — the dispatcher: the acp_interject record, the runner's
# BLOCKED cause, the ndjson where filter
# ---------------------------------------------------------------------------

_BZ7_NONCE = "BZ7-INTERJECT-NONCE"
_BZ7_TEXT = ("%s - the operator interjected this line mid-turn. Include the "
             "nonce %s verbatim in your next reply." % (_BZ7_NONCE, _BZ7_NONCE))
_BZ7_NOTE = "the arm never fired inside the parent turn, so no probe happened"
_BZ7_NO_RECORD = "interject-unavailable: step0.arm0.no_record"
# ScriptedBinary turns (start() leaves the clock at 0.05; the prompt registers
# at 0.1). FIRED: the tool call at 0.3 fires an after_tool_calls arm, its reply
# comes at 0.6, a chunk at 1.1, and turn_completed at 2.1 is stamped after the
# send; ECHO's chunk repeats the nonce. PROSE: two chunks and no tool call.
# SILENT: the arm fires and is never answered while the turn streams to 4.1.
# REFUSED: the reply is a JSON-RPC error at 0.5.
_BZ7_FIRED = {"reg_s": 0.05, "run_s": 2.0, "pc_gap_s": 0.15,
              "inter_ack_s": 0.3, "tool_calls": [_bz5_call(0.2)],
              "turn_chunks": [[(1.0, "Done.")]]}
_BZ7_ECHO = dict(_BZ7_FIRED,
                 turn_chunks=[[(1.0, "Noted: %s." % _BZ7_NONCE)]])
_BZ7_PROSE = {"reg_s": 0.05, "run_s": 2.0, "pc_gap_s": 0.15,
              "turn_chunks": [[(0.5, "prose one"), (1.5, "prose two")]]}
_BZ7_SILENT = {"reg_s": 0.05, "run_s": 4.0, "pc_gap_s": 0.15,
               "tool_calls": [_bz5_call(0.2)],
               "inter_replies": [{"reply": "silent"}],
               "turn_chunks": [[(1.0 + i, "C%d" % i) for i in range(3)]]}
_BZ7_REFUSED = {"reg_s": 0.05, "run_s": 3.0, "pc_gap_s": 0.15,
                "tool_calls": [_bz5_call(0.2)],
                "inter_replies": [{"reply": "error", "after": 0.2}],
                "turn_chunks": [[(1.0, "C0")]]}


def _bz7_arm(at, **extra):
    """One interject arm (plan r3 section 2.7) carrying the echo text."""
    return _bz5_arm(at, _BZ7_TEXT, **extra)


def _bz7_case(cwd, arm, driver="acp"):
    """A schema_version=1 case with one turn step armed with `arm`. The model
    premise is declared as section 2.5.1 item 5 and erratum E-1 require:
    interject_fired is a count on acp_interject where status is fired, min 1,
    named in scoring.vacuous_if with class model (no harness-class entry). The
    scored pins are the echoed nonce and the absence of acp_error. The runs
    below have wirecap off, so no wire assert is scored and D-8 has nothing to
    audit."""
    c = _base_new_case()
    c.update({
        "id": "BZ7-DISPATCH",
        "title": "BZ-7 dispatcher fixture",
        "bead": "apex-bdfz",
        "driver": driver,
        "cwd": cwd,
        "steps": [{"op": "turn",
                   "prompt": "Run one command, then wait for the operator.",
                   "interject": [arm]}],
        "scoring": {"require_wire_evidence": True, "vacuous_if": [
            {"pin": "interject_fired", "class": "model", "note": _BZ7_NOTE}]},
    })
    c["assert"]["ndjson"] = [
        {"id": "interject_fired", "op": "count", "event": "acp_interject",
         "where": {"status": "fired"}, "min": 1},
        {"id": "nonce_echoed", "op": "text_contains", "value": _BZ7_NONCE},
        {"id": "no_acp_error", "op": "absent", "event": "acp_error"},
    ]
    return c


# apex-bdfz BZ-7 fix round 1 (ruling BZ-R7): the mid-turn switch in RT-MSW-B's
# shape (plan r3 section 1.3), and the product refusing it.
_BZ7_SWITCH_TARGET = "m-test-2"
_BZ7_SWITCH_NOTE = ("the mid-turn switch was never sent: the pumped turn "
                    "finished before the after_s window opened")
# The long turn registers at 0.1 and streams chunks at 0.4, 0.8 and 1.6 to its
# turn_completed at 2.1; an after_s 0.5 arm fires on the chunk at 0.8, so
# session/set_model reaches the binary mid-turn and turn_completed is stamped
# after the send. The second chunk list is the post-switch turn's.
_BZ7_SWITCH = {"reg_s": 0.05, "run_s": 2.0, "pc_gap_s": 0.15,
               "turn_chunks": [[(0.3, "1. One."), (0.7, "2. Two."),
                                (1.5, "3. Three.")],
                               [(0.5, "BZ7-SWITCH-C1")]]}


def _bz7_switch_arm():
    """RT-MSW-B's arm (plan r3 sections 1.3, 2.7): set_model on after_s."""
    return {"at": {"after_s": 0.5}, "send": "set_model",
            "model": _BZ7_SWITCH_TARGET}


def _bz7_switch(case, declared=True):
    """Rewrite a _bz7_case fixture in place into RT-MSW-B's shape (plan r3
    section 1.3): the fixture's armed turn is the long turn and the post-switch
    turn follows, scored by RT-MSW-B's existing pins (no_acp_error,
    switch_landed, two_turns). declared adds what BZ-L2 adds: the model premise
    interject_fired (erratum E-1's where-filtered count) in
    scoring.vacuous_if, and interject_mid_turn. Without it the case carries no
    scoring key, as rt-msw-b.json does today."""
    arms = case["steps"][0]["interject"]
    case.update({
        "id": "BZ7-SWITCH",
        "title": "BZ-7 mid-turn switch fixture (RT-MSW-B's shape)",
        "steps": [
            {"op": "turn",
             "prompt": "Write a numbered list from 1 to 150, one sentence "
                       "per number.",
             "interject": arms},
            {"op": "turn", "prompt": "Reply with exactly: BZ7-SWITCH-C1"}],
    })
    case["assert"]["ndjson"] = [
        {"id": "no_acp_error", "op": "absent", "event": "acp_error"},
        {"id": "switch_landed", "op": "present", "event": "acp_set_model"},
        {"id": "two_turns", "op": "count", "event": "acp_end",
         "min": 2, "max": 2},
    ]
    if declared:
        case["scoring"] = {"vacuous_if": [
            {"pin": "interject_fired", "class": "model",
             "note": _BZ7_SWITCH_NOTE}]}
        case["assert"]["ndjson"] += [
            {"id": "interject_fired", "op": "count", "event": "acp_interject",
             "where": {"status": "fired"}, "min": 1},
            {"id": "interject_mid_turn", "op": "count",
             "event": "acp_interject",
             "where": {"status": "fired", "mid_turn": True}, "min": 1},
        ]
    else:
        case.pop("scoring", None)
    return case


def _bz7_switch_refusal(model):
    """The product's JSON-RPC error refusing a switch to `model` for an
    incompatible agent (ModelSwitchIncompatibleAgentError.into_acp_error,
    agent/config.rs: InvalidRequest, its message, its camelCase data)."""
    return {"code": -32600,
            "message": "Cannot switch to model '%s': it requires agent "
                       "'replay-agent-2' but the active agent is "
                       "'replay-agent'. Start a new session to use this "
                       "model." % model,
            "data": {"code": "MODEL_SWITCH_INCOMPATIBLE_AGENT",
                     "activeAgentType": "replay-agent",
                     "requiredAgentType": "replay-agent-2",
                     "modelId": model, "suggestion": "start_new_session"}}


class _BZ7RefusedSwitchBinary(ScriptedBinary):
    """ScriptedBinary whose session/set_model is refused: no model_changed
    update, and the JSON-RPC error of _bz7_switch_refusal 0.1 s after the
    request (the base fake's answer delay)."""

    def on_send(self, d, t):
        if d.get("method") != "session/set_model":
            return ScriptedBinary.on_send(self, d, t)
        if self._has_unended_prior(t):
            self.early_step_fired = True
        self._respond(t + 0.1, d.get("id"), error=_bz7_switch_refusal(
            (d.get("params") or {}).get("modelId")))


class MidTurnDispatchTest(unittest.TestCase):
    """apex-bdfz BZ-7 (plan r3 sections 2.5, 2.5.1, 2.5.2, 2.8; errata E-1,
    E-2): the step dispatcher, _run_case_once, runs an interject-armed ACP turn
    through the single-reader pump and records each arm twice: on the turn
    record (turns[].interjects, report.json's machine surface) and as one
    acp_interject pseudo-event. From the records it computes the runner's
    BLOCKED cause (send_timeout, a harness-caused disarm, or a declared arm
    left without a record) and passes it as finalize_verdict(...,
    interject_cause=...), which returns BLOCKED before anything else decides.
    check_ndjson's count, eq and ne apply a where map before counting or
    selecting."""

    maxDiff = None

    def _dispatch(self, binary_kwargs, arm, mutate=None, wire=True,
                  pump=None, driver="acp", binary_cls=ScriptedBinary):
        """One case through the real _run_case_once with wirecap off. ACP: the
        process is faked as MidTurnPumpTest does (Popen, select and run.time
        swapped for this call only). Headless: the binary is a stub that exits
        0 with no output; the headless driver is one process per turn, so it
        never arms an interject arm. CaseCtx is recorded so ctx.events can be
        read. wire=True gives the run a wire/ directory (a wiretap that
        captured nothing); wire=False leaves none. pump, when given, stands in
        for AcpSession._pump_mid_turn for this call only. binary_cls is the
        scripted binary's class, for a reply the base fake never sends."""
        tmp = tempfile.mkdtemp(prefix="rt-bz7-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        case = _bz7_case(tmp, arm, driver=driver)
        if mutate is not None:
            mutate(case)
        live = _write_fake_live_home(tmp, "http://127.0.0.1:9")
        stub = "grok-responses"
        if driver == "headless":
            stub = os.path.join(tmp, "stub-grok")
            with open(stub, "w") as fh:
                fh.write("#!/bin/sh\nexit 0\n")
            os.chmod(stub, 0o755)
        args = argparse.Namespace(out=os.path.join(tmp, "report"),
                                  live_home=live, wirecap=False, budget=20,
                                  ambient_key="", bin=stub, rows=None,
                                  keep_home=False, campaign_id=None)
        run_dir = os.path.join(tmp, "run")
        os.makedirs(os.path.join(run_dir, "wire") if wire else run_dir)
        made, binary = [], None

        class _RecordedCtx(run.CaseCtx):
            def __init__(self):
                super(_RecordedCtx, self).__init__()
                made.append(self)

        saved = (subprocess.Popen, sys.modules.get("select"), run.time,
                 run.CaseCtx, run.AcpSession._pump_mid_turn)
        run.CaseCtx = _RecordedCtx
        if driver == "acp":
            clock = FakeClock()
            stdout = FakeStdout(clock)
            binary = binary_cls(stdout, clock, **binary_kwargs)
            subprocess.Popen = lambda *a, **k: FakeProc(binary, clock, stdout)
            sys.modules["select"] = FakeSelectModule(clock, stdout)
            run.time = FakeTime(clock)
        if pump is not None:
            run.AcpSession._pump_mid_turn = pump
        try:
            row = run._run_case_once(case, args, run.Budget(args.budget),
                                     run_dir, 1)
        finally:
            (subprocess.Popen, sys.modules["select"], run.time, run.CaseCtx,
             run.AcpSession._pump_mid_turn) = saved
        with open(os.path.join(run_dir, "verdict.json")) as fh:
            verdict = json.load(fh)
        return {"row": row, "ctx": made[0], "verdict": verdict,
                "binary": binary, "case": case, "run_dir": run_dir}

    def test_turn_step_with_interject_records_the_arm(self):
        """The first test through the dispatcher's ACP turn branch with an
        interject block. The arm (after_tool_calls 1) fires on the tool call at
        0.3 while the turn runs; turn_completed at 2.1 is stamped after the
        send. The turn record carries the arm record, with interject_text as a
        key; ctx.events holds exactly one acp_interject event for the arm, the
        text under interject_text and never as data; status and fired agree;
        the turn has exactly one acp_end; and the case's own pins decide (the
        model echoed the nonce: PASS)."""
        out = self._dispatch(_BZ7_ECHO, _bz7_arm({"after_tool_calls": 1}))
        ctx, b = out["ctx"], out["binary"]
        rec = ctx.turns[-1]["interjects"][0]
        events = [e for e in ctx.events if e.get("type") == "acp_interject"]
        with self.subTest(leg="the arm record on the turn record"):
            keys = ("arm_id", "send", "status", "fired", "disarm_reason",
                    "harness_caused", "cause", "mid_turn",
                    "mid_turn_evidence", "interject_text")
            self.assertEqual(
                {k: rec.get(k) for k in keys},
                {"arm_id": "step0.arm0", "send": "interject",
                 "status": "fired", "fired": True, "disarm_reason": None,
                 "harness_caused": False, "cause": None, "mid_turn": True,
                 "mid_turn_evidence": ["completion_after_send"],
                 "interject_text": _BZ7_TEXT})
            self.assertIn("interject_text", rec)
            self.assertEqual(rec["prompt_id"], ctx.turns[-1]["prompt_id"])
        with self.subTest(leg="one acp_interject event for the arm"):
            self.assertEqual(
                [{k: v for k, v in e.items() if k != "elapsed_s"}
                 for e in events],
                [{"type": "acp_interject", "arm_id": "step0.arm0",
                  "fired": True, "mid_turn": True, "status": "fired",
                  "disarm_reason": None, "harness_caused": False,
                  "tool_calls_seen": 1, "interject_text": _BZ7_TEXT,
                  "_line": 0}])
            self.assertAlmostEqual(events[0]["elapsed_s"], 0.25)
        with self.subTest(leg="status is authoritative and fired agrees"):
            for r in ctx.turns[-1]["interjects"] + events:
                self.assertEqual(r["fired"], r["status"] == "fired")
        with self.subTest(leg="only the model's chunk is an acp_message"):
            self.assertEqual(
                [e["data"] for e in ctx.events
                 if e.get("type") == "acp_message"],
                ["Noted: %s." % _BZ7_NONCE])
        with self.subTest(leg="exactly one acp_end"):
            self.assertEqual(
                [e["type"] for e in ctx.events
                 if e.get("type") in ("acp_end", "acp_error")],
                ["acp_end"])
        with self.subTest(leg="the request reached the binary mid-turn"):
            self.assertEqual(
                [(round(t, 6), r["method"], r["params"]["text"])
                 for t, r in b.interjects],
                [(0.3, "_x.ai/interject", _BZ7_TEXT)])
            self.assertTrue(b.early_step_fired)
        with self.subTest(leg="the case's pins decide"):
            self.assertIs(out["row"]["turns"], ctx.turns)
            self.assertEqual(out["row"]["status"], "PASS")

    def test_verdict_mapping_per_arm_status(self):
        """Section 2.5's table end to end: one run through the dispatcher per
        arm status, over a case that declares the model premise (erratum E-1:
        a where-filtered count). fired: the case's pins decide (PASS with the
        echo, FAIL without it). missed and mismatched_tool: VACUOUS, the failed
        pins still in row["asserts"], the status readable from the turn record
        and verdict.json's vacuous_reason naming the pin, not the status.
        send_timeout and disarmed(ext_error): BLOCKED with the runner's named
        cause in blocked_reason. (a) The same missed run with the declaration
        removed scores FAIL. (b) The two BLOCKED rows on a run with no wire/
        directory are still BLOCKED: the cause leg sits outside `if wire_dir:`.
        E-1: a declared arm with no record (the pump returned none; the
        headless driver never arms one) is BLOCKED with
        interject-unavailable: step0.arm0.no_record. require false suppresses
        the cause only: VACUOUS, with the record still written. Two armed
        turns: each cause names its own step and arm, and they join in order.
        No case declares a harness-class premise, and every declared case
        validates."""
        calls = _bz7_arm({"after_tool_calls": 1})
        tool = _bz7_arm({"after_tool": "get_command_or_subagent_output"})
        timed = _bz7_arm({"after_tool_calls": 1}, interject_timeout_s=1)
        timeout_cause = "interject-send-timeout: step0.arm0"
        ext_cause = "interject-unavailable: step0.arm0.ext_error"
        table = [
            ("fired, echoed", _BZ7_ECHO, calls, "fired", "PASS", None),
            ("fired, not echoed", _BZ7_FIRED, calls, "fired", "FAIL", None),
            ("missed", _BZ7_PROSE, calls, "missed", "VACUOUS", None),
            ("mismatched_tool", _BZ7_PROSE, tool, "mismatched_tool",
             "VACUOUS", None),
            ("send_timeout", _BZ7_SILENT, timed, "send_timeout", "BLOCKED",
             timeout_cause),
            ("disarmed(ext_error)", _BZ7_REFUSED, calls, "disarmed",
             "BLOCKED", ext_cause),
        ]
        declared = []
        for leg, script, arm, arm_status, status, cause in table:
            out = self._dispatch(script, arm)
            declared.append(out["case"])
            row, v = out["row"], out["verdict"]
            with self.subTest(leg=leg):
                self.assertEqual(
                    (row["status"], row.get("blocked_reason"), v["status"],
                     v["verdict"]["blocked_reason"]),
                    (status, cause, status, cause))
                self.assertEqual(row["turns"][0]["interjects"][0]["status"],
                                 arm_status)
            if status == "VACUOUS":
                with self.subTest(leg=leg, part="VACUOUS record obligations"):
                    self.assertEqual(
                        sorted(a["spec"]["id"] for a in row["asserts"]
                               if not a["ok"]),
                        ["interject_fired", "nonce_echoed"])
                    reason = v["verdict"]["vacuous_reason"]
                    self.assertIn("interject_fired", reason)
                    self.assertNotIn(arm_status, reason)
        undeclared = self._dispatch(
            _BZ7_PROSE, calls,
            mutate=lambda c: c["scoring"].pop("vacuous_if"))
        with self.subTest(leg="(a) the missed run, premise undeclared"):
            row, v = undeclared["row"], undeclared["verdict"]
            self.assertEqual(
                (row["status"], row.get("blocked_reason"),
                 v["verdict"]["vacuous_reason"], v["verdict"]["premises"]),
                ("FAIL", None, None, []))
            self.assertEqual(row["turns"][0]["interjects"][0]["status"],
                             "missed")
        for leg, script, arm, cause in (
                ("send_timeout", _BZ7_SILENT, timed, timeout_cause),
                ("disarmed(ext_error)", _BZ7_REFUSED, calls, ext_cause)):
            out = self._dispatch(script, arm, wire=False)
            with self.subTest(leg="(b) no wire/ directory: " + leg):
                self.assertFalse(
                    os.path.isdir(os.path.join(out["run_dir"], "wire")))
                self.assertEqual(
                    (out["row"]["status"], out["row"].get("blocked_reason")),
                    ("BLOCKED", cause))
        real_pump = run.AcpSession._pump_mid_turn

        def pump_without_records(sess, arms, waiter, deadline, **kw):
            # Fault injection: the real pump reads the turn and fires the arm,
            # and its record never reaches the dispatcher.
            real_pump(sess, arms, waiter, deadline, **kw)
            return []

        lost = self._dispatch(_BZ7_ECHO, calls, pump=pump_without_records)
        headless = self._dispatch(None, calls, driver="headless")
        for leg, out in (("E-1: the pump wrote no record", lost),
                         ("E-1: the headless driver never arms", headless)):
            with self.subTest(leg=leg):
                self.assertEqual(
                    (out["row"]["status"], out["row"].get("blocked_reason")),
                    ("BLOCKED", _BZ7_NO_RECORD))
                self.assertEqual(
                    [e for e in out["ctx"].events
                     if e.get("type") == "acp_interject"], [])
        with self.subTest(leg="E-1: the lost record is not on the turn"):
            self.assertEqual(lost["row"]["turns"][0]["interjects"], [])
        tolerant = self._dispatch(
            _BZ7_SILENT,
            _bz7_arm({"after_tool_calls": 1}, interject_timeout_s=1,
                     require=False))
        declared.append(tolerant["case"])
        with self.subTest(leg="require false: the cause only is suppressed"):
            row = tolerant["row"]
            self.assertEqual((row["status"], row.get("blocked_reason")),
                             ("VACUOUS", None))
            self.assertEqual(row["turns"][0]["interjects"][0]["status"],
                             "send_timeout")
            self.assertEqual(
                [e["status"] for e in tolerant["ctx"].events
                 if e.get("type") == "acp_interject"], ["send_timeout"])
        # Two armed turns: step 0's arm times out; step 1's first arm is
        # refused and its second (after_tool wait_agent) never matches. The
        # causes carry their own step and arm index and join in that order;
        # the mismatched arm adds none.
        two_turns = self._dispatch(
            {"reg_s": 0.05, "run_s": 4.0, "pc_gap_s": 0.15,
             "tool_calls": [_bz5_call(0.2), dict(_bz5_call(0.2), turn=1)],
             "inter_replies": [{"reply": "silent"},
                               {"reply": "error", "after": 0.2}],
             "turn_chunks": [[(1.0 + i, "C%d" % i) for i in range(3)],
                             [(1.0, "D0")]]},
            timed,
            mutate=lambda c: c["steps"].append(
                {"op": "turn", "prompt": "Now check on the worker.",
                 "interject": [calls,
                               _bz7_arm({"after_tool": "wait_agent"})]}))
        declared.append(two_turns["case"])
        with self.subTest(leg="two armed turns: causes joined in order"):
            row = two_turns["row"]
            self.assertEqual(
                (row["status"], row.get("blocked_reason")),
                ("BLOCKED", timeout_cause + "; interject-unavailable: "
                 "step1.arm0.ext_error"))
            self.assertEqual(
                [[(r["arm_id"], r["status"]) for r in t["interjects"]]
                 for t in row["turns"]],
                [[("step0.arm0", "send_timeout")],
                 [("step1.arm0", "disarmed"),
                  ("step1.arm1", "mismatched_tool")]])
            self.assertEqual(
                [e["arm_id"] for e in two_turns["ctx"].events
                 if e.get("type") == "acp_interject"],
                ["step0.arm0", "step1.arm0", "step1.arm1"])
        battery = declared + [undeclared["case"], lost["case"],
                              headless["case"]]
        with self.subTest(leg="no harness-class premise in the battery"):
            self.assertEqual(
                sorted({e.get("class") for c in battery
                        for e in c["scoring"].get("vacuous_if", [])}),
                ["model"])
        with self.subTest(leg="every declared case validates"):
            tmp = tempfile.mkdtemp(prefix="rt-bz7v-")
            self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
            errs = {}
            for i, c in enumerate(declared):
                p = _write_case(tmp, "bz7-%d.json" % i, c)
                for engine in _bz6_engines():
                    found = run.validate_case_file(p, engine=engine)
                    if found:
                        errs[(i, engine)] = found
            self.assertEqual(errs, {})

    def test_where_filter_counts_per_status(self):
        """Section 2.5.2: count, eq and ne take a where map of equality filters
        on the event's own fields, applied before counting or selecting. Over
        two fired arms and one disarmed arm, count where status is fired gives
        2, so min 2 max 2 holds and min 3 does not (counting by type alone
        gives 3); two keys must both match; eq and ne read the disarmed arm's
        record; a where-less count is unchanged. The case-file half: a scalar
        where validates on both engines, and a list value is refused by
        both."""
        tmp = tempfile.mkdtemp(prefix="rt-bz7w-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        good = _write_case(tmp, "bz7-where.json",
                           _bz7_case(tmp, _bz7_arm({"after_tool_calls": 1})))
        listed = _bz7_case(tmp, _bz7_arm({"after_tool_calls": 1}))
        listed["assert"]["ndjson"][0]["where"] = {"status": ["fired"]}
        bad = _write_case(tmp, "bz7-where-list.json", listed)
        for engine in _bz6_engines():
            with self.subTest(part="the where property", engine=engine):
                self.assertEqual(run.validate_case_file(good, engine=engine),
                                 [])
                errs = run.validate_case_file(bad, engine=engine)
                self.assertTrue(any("where" in e for e in errs), errs)
        events = [
            {"type": "acp_message", "data": "chunk", "_line": 0},
            {"type": "acp_interject", "arm_id": "step0.arm0", "fired": True,
             "mid_turn": True, "status": "fired", "disarm_reason": None,
             "harness_caused": False, "_line": 0},
            {"type": "acp_interject", "arm_id": "step0.arm1", "fired": True,
             "mid_turn": True, "status": "fired", "disarm_reason": None,
             "harness_caused": False, "_line": 0},
            {"type": "acp_interject", "arm_id": "step0.arm2", "fired": False,
             "mid_turn": False, "status": "disarmed",
             "disarm_reason": "ext_error", "harness_caused": True,
             "_line": 0},
            {"type": "acp_end", "stopReason": "end_turn", "_line": 0},
        ]

        def holds(**spec):
            spec["event"] = "acp_interject"
            return run.check_ndjson(spec, events, "acp").ok

        fired = {"status": "fired"}
        legs = {
            "count fired, min 2 max 2": (
                holds(op="count", where=fired, min=2, max=2), True),
            "count fired, min 3": (
                holds(op="count", where=fired, min=3), False),
            "count fired on step0.arm1": (
                holds(op="count", where={"status": "fired",
                                         "arm_id": "step0.arm1"},
                      min=1, max=1), True),
            "eq reads the disarmed record": (
                holds(op="eq", where={"status": "disarmed"},
                      field="disarm_reason", value="ext_error"), True),
            "ne reads the disarmed record": (
                holds(op="ne", where={"status": "disarmed"},
                      field="harness_caused", value=False), True),
            "count without where": (holds(op="count", min=3, max=3), True),
        }
        self.assertEqual({k: v[0] for k, v in legs.items()},
                         {k: v[1] for k, v in legs.items()})

    def test_miss_path_writes_no_fired_record(self):
        """The miss path: the arm's trigger (one tool call) never holds before
        the turn completes, so its one acp_interject record says fired False,
        status missed, closed by turn_completed and not harness-caused; no
        record says fired True, and nothing was sent."""
        out = self._dispatch(_BZ7_PROSE, _bz7_arm({"after_tool_calls": 1}))
        events = [e for e in out["ctx"].events
                  if e.get("type") == "acp_interject"]
        keys = ("arm_id", "fired", "status", "disarm_reason",
                "harness_caused", "mid_turn", "interject_text")
        self.assertEqual(
            [{k: e.get(k) for k in keys} for e in events],
            [{"arm_id": "step0.arm0", "fired": False, "status": "missed",
              "disarm_reason": "turn_completed", "harness_caused": False,
              "mid_turn": False, "interject_text": _BZ7_TEXT}])
        self.assertEqual([e for e in events if e["fired"]], [])
        self.assertEqual(out["binary"].interjects, [])

    def test_mid_turn_set_model_lands_the_switch(self):
        """BZ-R7 F-1, plan r3 section 3 F-13's early shape, in RT-MSW-B's case
        shape: the after_s arm sends session/set_model at 0.8 while the long
        turn runs to 2.1, and the reply carries no error. The armed turn branch
        records the switch as the switch step does: one acp_set_model naming
        the target, after the arm's acp_interject and before the turn's
        acp_end. The in-flight turn completes on the model it was sent with
        and the post-switch turn runs on the target. switch_landed is present,
        the census is two acp_end, the fake saw the request before the turn
        ended (early_step_fired), the arm fired with mid_turn true, and
        RT-MSW-B's pins plus BZ-L2's declared premise hold (PASS). Both case
        shapes validate on every engine."""
        out = self._dispatch(_BZ7_SWITCH, _bz7_switch_arm(),
                             mutate=_bz7_switch)
        ctx, b, row = out["ctx"], out["binary"], out["row"]
        rec = ctx.turns[0]["interjects"][0]
        with self.subTest(leg="switch_landed present: one acp_set_model"):
            self.assertEqual(
                [e for e in ctx.events if e.get("type") == "acp_set_model"],
                [{"type": "acp_set_model", "model": _BZ7_SWITCH_TARGET,
                  "_line": 0}])
            self.assertEqual(
                [e["type"] for e in ctx.events if e.get("type") in (
                    "acp_interject", "acp_set_model", "acp_end",
                    "acp_error")],
                ["acp_interject", "acp_set_model", "acp_end", "acp_end"])
        with self.subTest(leg="two turns: the post-switch turn runs on the "
                              "target"):
            self.assertEqual([(t["step"], t["model"]) for t in ctx.turns],
                             [(0, "m-test"), (1, _BZ7_SWITCH_TARGET)])
            self.assertEqual(
                [e["model"] for e in ctx.events
                 if e.get("type") == "acp_end"],
                ["m-test", _BZ7_SWITCH_TARGET])
        with self.subTest(leg="early_step_fired: the request reached the "
                              "binary mid-turn"):
            self.assertTrue(b.early_step_fired)
            self.assertLess(rec["sent_at"], b.t_end[0])
        with self.subTest(leg="the arm fired with mid_turn true"):
            keys = ("arm_id", "send", "model", "status", "fired",
                    "disarm_reason", "harness_caused", "cause", "mid_turn",
                    "mid_turn_evidence")
            self.assertEqual(
                {k: rec.get(k) for k in keys},
                {"arm_id": "step0.arm0", "send": "set_model",
                 "model": _BZ7_SWITCH_TARGET, "status": "fired",
                 "fired": True, "disarm_reason": None,
                 "harness_caused": False, "cause": None, "mid_turn": True,
                 "mid_turn_evidence": ["completion_after_send"]})
            self.assertAlmostEqual(rec["sent_at"], 0.8)
        with self.subTest(leg="the pins of RT-MSW-B and the declared premise "
                              "decide: PASS"):
            self.assertEqual(
                {a["spec"]["id"]: a["ok"] for a in row["asserts"]},
                {"no_acp_error": True, "switch_landed": True,
                 "two_turns": True, "interject_fired": True,
                 "interject_mid_turn": True})
            self.assertEqual((row["status"], row.get("blocked_reason"),
                              out["verdict"]["status"]),
                             ("PASS", None, "PASS"))
        with self.subTest(leg="both case shapes validate"):
            tmp = tempfile.mkdtemp(prefix="rt-bz7s-")
            self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
            errs = {}
            for name, declared in (("declared", True), ("undeclared", False)):
                c = _bz7_switch(_bz7_case(tmp, _bz7_switch_arm()),
                                declared=declared)
                p = _write_case(tmp, "bz7-switch-%s.json" % name, c)
                for engine in _bz6_engines():
                    found = run.validate_case_file(p, engine=engine)
                    if found:
                        errs[(name, engine)] = found
            self.assertEqual(errs, {})

    def test_mid_turn_set_model_refusal_reads_fail(self):
        """BZ-R7 F-1's refusal semantics: the product answers the set_model
        arm's own request with a JSON-RPC error (an incompatible-agent
        rejection). The arm record stays disarmed with ext_error; the armed
        turn branch appends acp_error carrying that error and the target model,
        and no acp_set_model; the model does not advance. On RT-MSW-B's case as
        it stands (its three pins, no scoring key) the row reads FAIL, as its
        switch_landed label says, with no interject cause in blocked_reason.
        With no scored pin at all the runtime verdict is FAIL, as it is for a
        refused switch step. An interject arm refused the same way keeps
        section 2.4 D-6: BLOCKED, interject-unavailable: step0.arm0.ext_error."""
        refusal = _bz7_switch_refusal(_BZ7_SWITCH_TARGET)
        out = self._dispatch(
            _BZ7_SWITCH, _bz7_switch_arm(),
            mutate=lambda c: _bz7_switch(c, declared=False),
            binary_cls=_BZ7RefusedSwitchBinary)
        ctx, b, row = out["ctx"], out["binary"], out["row"]
        rec = ctx.turns[0]["interjects"][0]
        with self.subTest(leg="the arm record stays disarmed with ext_error"):
            keys = ("arm_id", "send", "model", "status", "fired",
                    "disarm_reason", "harness_caused", "cause", "mid_turn")
            self.assertEqual(
                {k: rec.get(k) for k in keys},
                {"arm_id": "step0.arm0", "send": "set_model",
                 "model": _BZ7_SWITCH_TARGET, "status": "disarmed",
                 "fired": False, "disarm_reason": "ext_error",
                 "harness_caused": True,
                 "cause": "interject-unavailable: step0.arm0.ext_error",
                 "mid_turn": False})
            self.assertEqual(rec["resp"]["error"], refusal)
            self.assertTrue(b.early_step_fired)
        with self.subTest(leg="acp_error carries the refusal; no "
                              "acp_set_model"):
            self.assertEqual(
                [e for e in ctx.events
                 if e.get("type") in ("acp_set_model", "acp_error")],
                [{"type": "acp_error", "error": refusal,
                  "model": _BZ7_SWITCH_TARGET}])
            self.assertEqual(
                [e["type"] for e in ctx.events if e.get("type") in (
                    "acp_interject", "acp_set_model", "acp_end",
                    "acp_error")],
                ["acp_interject", "acp_error", "acp_end", "acp_end"])
        with self.subTest(leg="the model does not advance"):
            self.assertEqual([(t["step"], t["model"]) for t in ctx.turns],
                             [(0, "m-test"), (1, "m-test")])
        with self.subTest(leg="the pins of RT-MSW-B read FAIL, not BLOCKED"):
            self.assertEqual(
                (row["status"], row.get("blocked_reason"),
                 out["verdict"]["status"],
                 out["verdict"]["verdict"]["blocked_reason"]),
                ("FAIL", None, "FAIL", None))
            self.assertEqual(
                sorted(a["spec"]["id"] for a in row["asserts"]
                       if not a["ok"]),
                ["no_acp_error", "switch_landed"])

        def no_pins(c):
            _bz7_switch(c, declared=False)
            c["assert"]["ndjson"] = []

        bare = self._dispatch(_BZ7_SWITCH, _bz7_switch_arm(), mutate=no_pins,
                              binary_cls=_BZ7RefusedSwitchBinary)
        with self.subTest(leg="no scored pin: the runtime verdict is FAIL"):
            self.assertEqual(bare["row"]["asserts"], [])
            self.assertEqual(
                (bare["row"]["status"], bare["row"].get("blocked_reason")),
                ("FAIL", None))
        interject = self._dispatch(_BZ7_REFUSED,
                                   _bz7_arm({"after_tool_calls": 1}))
        with self.subTest(leg="an interject arm keeps D-6: BLOCKED"):
            self.assertEqual(
                (interject["row"]["status"],
                 interject["row"].get("blocked_reason")),
                ("BLOCKED", "interject-unavailable: step0.arm0.ext_error"))
            self.assertEqual(
                [e for e in interject["ctx"].events
                 if e.get("type") == "acp_error"], [])

    def test_report_md_prints_one_arm_line_per_armed_turn(self):
        """BZ-R7 F-2 (plan r3 section 2.8): write_report prints, under each
        armed turn's line in report.md, one line naming every arm of that turn
        with its send and status (the disarm reason in parentheses, mid_turn
        when proved), so the arms of a VACUOUS or BLOCKED row read in report.md
        and not only in report.json. A turn without an interject block gets no
        arm line; an armed turn whose records were lost says so. The rows are
        real dispatcher rows: a VACUOUS miss followed by an unarmed turn, a
        BLOCKED case with two armed turns (three arms), and erratum E-1's
        lost-record row. BZ-8 (ruling BZ-R8, fix review F-4) adds the two
        fired shapes: the mid-turn switch of
        test_mid_turn_set_model_lands_the_switch, whose line names the
        set_model target and mid_turn, and a fired interject arm."""
        calls = _bz7_arm({"after_tool_calls": 1})
        timed = _bz7_arm({"after_tool_calls": 1}, interject_timeout_s=1)

        def named(rid, more=None):
            def mutate(c):
                c["id"] = rid
                if more is not None:
                    more(c)
            return mutate

        vacuous = self._dispatch(_BZ7_PROSE, calls, mutate=named(
            "BZ7-REPORT-VACUOUS", lambda c: c["steps"].append(
                {"op": "turn", "prompt": "Anything else?"})))
        two = self._dispatch(
            {"reg_s": 0.05, "run_s": 4.0, "pc_gap_s": 0.15,
             "tool_calls": [_bz5_call(0.2), dict(_bz5_call(0.2), turn=1)],
             "inter_replies": [{"reply": "silent"},
                               {"reply": "error", "after": 0.2}],
             "turn_chunks": [[(1.0 + i, "C%d" % i) for i in range(3)],
                             [(1.0, "D0")]]},
            timed, mutate=named("BZ7-REPORT-TWO", lambda c: c["steps"].append(
                {"op": "turn", "prompt": "Now check on the worker.",
                 "interject": [calls,
                               _bz7_arm({"after_tool": "wait_agent"})]})))
        real_pump = run.AcpSession._pump_mid_turn

        def pump_without_records(sess, arms, waiter, deadline, **kw):
            # Fault injection (erratum E-1): the records never reach the
            # dispatcher.
            real_pump(sess, arms, waiter, deadline, **kw)
            return []

        lost = self._dispatch(_BZ7_ECHO, calls, pump=pump_without_records,
                              mutate=named("BZ7-REPORT-LOST"))

        def switched(c):
            _bz7_switch(c)
            c["id"] = "BZ7-REPORT-SWITCH"

        switch = self._dispatch(_BZ7_SWITCH, _bz7_switch_arm(),
                                mutate=switched)
        fired = self._dispatch(_BZ7_ECHO, calls,
                               mutate=named("BZ7-REPORT-FIRED"))
        rows = [vacuous["row"], two["row"], lost["row"], switch["row"],
                fired["row"]]
        out_dir = tempfile.mkdtemp(prefix="rt-bz7r-")
        self.addCleanup(lambda: shutil.rmtree(out_dir, ignore_errors=True))
        env_meta = {"ts": "2026-11-01T00:00:00Z", "bin": "grok-responses",
                    "bin_sha": "-", "git": "-", "key_sha": "-",
                    "upstream": "http://127.0.0.1:9"}
        args = argparse.Namespace(live_home="-", wirecap=False, budget=20,
                                  ambient_key="")
        run.write_report(out_dir, rows, env_meta, args, 0, [])
        with open(os.path.join(out_dir, "report.md")) as fh:
            md = fh.read()
        sections, cur = {}, None
        for line in md.splitlines():
            if line.startswith("## "):
                cur = sections.setdefault(line[3:].split(" ")[0], [])
            elif cur is not None and line.startswith("- acp_turn("):
                cur.append(line.split(" ")[1])
            elif cur is not None and line.startswith("  - interject arms:"):
                cur.append(line)
        with self.subTest(leg="row statuses"):
            self.assertEqual([r["status"] for r in rows],
                             ["VACUOUS", "BLOCKED", "BLOCKED", "PASS", "PASS"])
        with self.subTest(leg="one arm line per armed turn"):
            self.assertEqual(sections, {
                "BZ7-REPORT-VACUOUS": [
                    "acp_turn(step=0)",
                    "  - interject arms: step0.arm0 interject "
                    "missed(turn_completed)",
                    "acp_turn(step=1)"],
                "BZ7-REPORT-TWO": [
                    "acp_turn(step=0)",
                    "  - interject arms: step0.arm0 interject send_timeout",
                    "acp_turn(step=1)",
                    "  - interject arms: step1.arm0 interject "
                    "disarmed(ext_error); step1.arm1 interject "
                    "mismatched_tool(turn_completed)"],
                "BZ7-REPORT-LOST": [
                    "acp_turn(step=0)",
                    "  - interject arms: none recorded"],
                "BZ7-REPORT-SWITCH": [
                    "acp_turn(step=0)",
                    "  - interject arms: step0.arm0 set_model m-test-2 fired "
                    "mid_turn",
                    "acp_turn(step=1)"],
                "BZ7-REPORT-FIRED": [
                    "acp_turn(step=0)",
                    "  - interject arms: step0.arm0 interject fired "
                    "mid_turn"]})

    def test_prompt_id_read_from_the_kept_prompt_response(self):
        """BZ-R7 F-4(a): when the pump reads past the turn's terminal (here the
        arm's reply is owed until 2.6, after turn_completed and the
        session/prompt response at 2.1), prompt_collect returns the kept
        session/prompt response as it arrived, and that response carries the
        prompt id only in result._meta.promptId. The turn record's prompt_id
        (apex-ayl.28) reads it there: the id prompt_send chose, the one the
        binary adopted and the arm record names."""
        script = {"reg_s": 0.05, "run_s": 2.0, "pc_gap_s": 0.15,
                  "inter_ack_s": 1.0, "tool_calls": [_bz5_call(1.5)],
                  "turn_chunks": [[(1.0, "Noted: %s." % _BZ7_NONCE)]]}
        out = self._dispatch(script, _bz7_arm({"after_tool_calls": 1}))
        ctx, b = out["ctx"], out["binary"]
        rec = ctx.turns[0]["interjects"][0]
        with self.subTest(leg="the pump read past the terminal"):
            self.assertEqual(rec["status"], "fired")
            self.assertGreater(rec["reply_at"], b.t_end[0])
        with self.subTest(leg="the turn record prompt_id is the turn prompt "
                              "id"):
            self.assertEqual((ctx.turns[0]["prompt_id"], rec["prompt_id"]),
                             (b.turn_pids[0], b.turn_pids[0]))

    def test_where_refused_on_other_ndjson_ops(self):
        """BZ-R7 F-5(a): check_ndjson applies an ndjson where only on count, eq
        and ne, so validation refuses a where on every other ndjson op instead
        of letting it validate and be ignored (present acp_interject where
        status fired would hold on a missed record). Every ndjson op is tried
        on a legacy-form case (validate_case_file's in-tree contract); on a NEW
        case both engines refuse present with a where, while its count + where
        and its wire assert's where still validate."""
        extra = {"count": {"min": 1}, "absent": {}, "present": {},
                 "eq": {"field": "mid_turn", "value": True},
                 "ne": {"field": "mid_turn", "value": False},
                 "text_contains": {"value": _BZ7_NONCE},
                 "tools_absent": {"names": ["x"]},
                 "tools_present": {"names": ["x"]}, "recon": {}}
        tmp = tempfile.mkdtemp(prefix="rt-bz7f-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))

        def verdict(errs, op):
            if errs == []:
                return []
            if len(errs) == 1 and all(n in errs[0] for n in (
                    "where", "%r" % op, "count, eq and ne")):
                return "refused"
            return errs

        with self.subTest(part="legacy: every ndjson op"):
            self.assertEqual(sorted(extra), sorted(run.NDJSON_OPS))
            got, want = {}, {}
            for op, more in sorted(extra.items()):
                spec = dict({"op": op, "event": "acp_interject",
                             "where": {"status": "fired"}}, **more)
                legacy = {"id": "BZ7-WHERE-%s" % op.upper(), "driver": "acp",
                          "model": "m-test",
                          "steps": [{"op": "turn", "prompt": "Reply."}],
                          "assert": {"ndjson": [spec]}}
                errs = run.validate_case_file(
                    _write_case(tmp, "bz7-where-%s.json" % op, legacy))
                got[op] = verdict(errs, op)
                want[op] = [] if op in ("count", "eq", "ne") else "refused"
            self.assertEqual(got, want)
        good = _write_case(tmp, "bz7-where-new-good.json",
                           _bz7_case(tmp, _bz7_arm({"after_tool_calls": 1})))
        bad = _bz7_case(tmp, _bz7_arm({"after_tool_calls": 1}))
        bad["assert"]["ndjson"].append(
            {"id": "present_where", "op": "present", "event": "acp_interject",
             "where": {"status": "fired"}})
        bad = _write_case(tmp, "bz7-where-new-present.json", bad)
        for engine in _bz6_engines():
            with self.subTest(part="NEW case", engine=engine):
                self.assertEqual(
                    (run.validate_case_file(good, engine=engine),
                     verdict(run.validate_case_file(bad, engine=engine),
                             "present")),
                    ([], "refused"))

    def test_unknown_send_rejected_by_validate_step(self):
        """BZ-R7 F-6: _validate_step rejects an arm whose send is neither
        interject nor set_model, the step-local half of the schema's send enum.
        A legacy-form case (no schema_version) never reaches the schema gate,
        so without this rule send resume, a typo or null validated, and the
        pump would have sent the arm as x.ai/interject. The two known sends
        still validate, with send defaulting to interject."""
        tmp = tempfile.mkdtemp(prefix="rt-bz7u-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        arms = {
            "resume": {"at": {"after_s": 1}, "send": "resume",
                       "model": _BZ7_SWITCH_TARGET},
            "typo": {"at": {"after_s": 1}, "send": "set-model",
                     "model": _BZ7_SWITCH_TARGET},
            "null": {"at": {"after_s": 1}, "send": None,
                     "model": _BZ7_SWITCH_TARGET},
            "set_model": _bz7_switch_arm(),
            "interject": _bz7_arm({"after_s": 1}),
            "default": {"at": {"after_s": 1}, "text": _BZ7_TEXT},
        }

        def verdict(errs, send):
            if errs == []:
                return []
            if len(errs) == 1 and all(n in errs[0] for n in (
                    "send", "%r" % (send,), "interject or set_model")):
                return "refused"
            return errs

        got, want = {}, {}
        for name, arm in sorted(arms.items()):
            step = {"op": "turn", "prompt": "Reply.", "interject": [arm]}
            legacy = {"id": "BZ7-SEND", "driver": "acp", "model": "m-test",
                      "steps": [step], "assert": {}}
            errs = run.validate_case_file(
                _write_case(tmp, "bz7-send-%s.json" % name, legacy))
            step_errs = []
            run._validate_step("bz7-send steps[0]", step, step_errs)
            send = arm.get("send", "interject")
            got[name] = (verdict(errs, send), verdict(step_errs, send))
            want[name] = (([], []) if name in ("set_model", "interject",
                                               "default")
                          else ("refused", "refused"))
        self.assertEqual(got, want)


# ---------------------------------------------------------------------------
# apex-bdfz BZ-8 — anti-vacuity controls, and the test-only legs rulings BZ-R7
# and BZ-R8 route to BZ-8
# ---------------------------------------------------------------------------

# BZ-L1's arm text (plan r3 BZ-L1): the nonce, and the ask to repeat it.
_BZ8_NONCE = "MXAI-C12-INTERJECT"
_BZ8_TEXT = ("MXAI-C12-INTERJECT \u2014 the operator interjected this line "
             "mid-wait. Include the nonce MXAI-C12-INTERJECT verbatim in your "
             "next reply, then finish the wait.")
# ScriptedBinary turns (start() leaves the clock at 0.05; the prompt registers
# at 0.1): the tool call at 0.3 fires an after_tool_calls arm, its reply comes
# at 0.6, and turn_completed at 2.1 is stamped after the send. NO_CHUNK: the
# agent writes no message chunk. ECHOED: its one chunk, at 1.1, repeats the
# nonce. PROSE: its one chunk answers without it.
_BZ8_NO_CHUNK = {"reg_s": 0.05, "run_s": 2.0, "pc_gap_s": 0.15,
                 "inter_ack_s": 0.3, "tool_calls": [_bz5_call(0.2)]}
_BZ8_ECHOED = dict(_BZ8_NO_CHUNK,
                   turn_chunks=[[(1.0, "Noted: %s." % _BZ8_NONCE)]])
_BZ8_PROSE = dict(_BZ8_NO_CHUNK, turn_chunks=[[(1.0, "Done.")]])
# A same-session prompt id this session never sent: the product's fallback
# turn prefix (INTERJECT_FALLBACK_PROMPT_PREFIX, interjection.rs at the base).
_BZ8_FOREIGN_PID = "interject-fallback-019e24b8"
_BZ8_GUARD = ("ACP desync guard: ignoring prompt_complete pid=%s (expected "
              "%s) \u2014 late/foreign completion")


def _bz8_nonce_case(case):
    """A _bz7_case fixture whose scored echo pin reads BZ-L1's nonce."""
    case["id"] = "BZ8-ANTI-VACUITY"
    for spec in case["assert"]["ndjson"]:
        if spec["id"] == "nonce_echoed":
            spec["value"] = _BZ8_NONCE


def _bz8_carrying(events, needle):
    """The events in which `needle` occurs in any string value, in order."""
    def has(v):
        if isinstance(v, str):
            return needle in v
        if isinstance(v, dict):
            return any(has(x) for x in v.values())
        if isinstance(v, (list, tuple)):
            return any(has(x) for x in v)
        return False
    return [e for e in events if has(e)]


def _bz8_format_interjection(text):
    """format_interjection (xai-interjection-core format.rs at the base): the
    mid-turn note, the user_query envelope and the unfinished-task trailer."""
    return ("The user sent a message while you were working:\n<user_query>\n"
            "%s\n</user_query>\nMake sure to complete any unfinished tasks "
            "from previous turns." % text)


class _BZ8UserEchoBinary(ScriptedBinary):
    """ScriptedBinary that also echoes every interjection it receives as a
    user-message-shaped update, 0.05 s after the request: the
    user_message_chunk the shell writes for a mid-turn interjection
    (persist_synthetic_user_message_with_model, interjection.rs at the base;
    replayed on session/load), carrying format_interjection's text, the typed
    text as the text block's displayText, and _meta modelId plus interjection
    true. With inter_broadcast it also sends the live
    x.ai/session/interjection broadcast."""

    def _on_interject(self, d, tid, t):
        ScriptedBinary._on_interject(self, d, tid, t)
        text = (d.get("params") or {}).get("text")
        self._notify(t + 0.05, "session/update", {
            "sessionId": self.session_id,
            "update": {"sessionUpdate": "user_message_chunk",
                       "content": {"type": "text",
                                   "text": _bz8_format_interjection(text),
                                   "_meta": {"displayText": text}},
                       "_meta": {"modelId": "replay-model",
                                 "interjection": True}}})


class _BZ8ForeignCompletionBinary(ScriptedBinary):
    """ScriptedBinary that also delivers, 0.8 s into the first turn (after its
    queue row, before its own turn_completed), a same-session prompt_complete
    for _BZ8_FOREIGN_PID, in the base fake's prompt_complete shape."""

    foreign_at = None

    def _on_prompt(self, d, tid, t):
        first = not self.turn_pids
        ScriptedBinary._on_prompt(self, d, tid, t)
        if first:
            self.foreign_at = self.t_reg[0] + 0.8
            self._notify(self.foreign_at, "_x.ai/session/prompt_complete",
                         {"sessionId": self.session_id,
                          "promptId": _BZ8_FOREIGN_PID,
                          "stopReason": "end_turn",
                          "agentResult": "BZ8-FOREIGN-TURN"})


class InterjectAntiVacuityTest(unittest.TestCase):
    """apex-bdfz BZ-8 (plan r3 section 4 BZ-8, section 3 F-14, section 2.8's
    hard rule and read scope): the injected text can never satisfy its own
    nonce pin, and a model-authored chunk can. Every event list comes from
    the step dispatcher itself (the real _run_case_once over the fake peer, as
    MidTurnDispatchTest drives it), so a runner change that publishes the
    interjection where ndjson_text reads turns these legs red."""

    maxDiff = None
    _dispatch = MidTurnDispatchTest._dispatch

    def _armed(self, script, binary_cls=ScriptedBinary):
        """One after_tool_calls arm carrying BZ-L1's text, dispatched."""
        return self._dispatch(script,
                              _bz5_arm({"after_tool_calls": 1}, _BZ8_TEXT),
                              mutate=_bz8_nonce_case, binary_cls=binary_cls)

    @staticmethod
    def _holds(events):
        return run.check_ndjson(
            {"op": "text_contains", "value": "MXAI-C12-INTERJECT"}, events,
            "acp").ok

    def test_injected_text_cannot_satisfy_its_own_nonce(self):
        """check_ndjson({"op": "text_contains", "value": "MXAI-C12-INTERJECT"},
        events, "acp") over four dispatcher event lists, each from a run whose
        arm fired mid-turn and sent BZ-L1's text. (a) The events carrying the
        nonce in a run whose agent wrote nothing, which are only the runner's
        own acp_interject record: False. (b) A run whose agent repeated the
        nonce in an agent_message_chunk: True, the positive half. (c) The full
        stream of a run whose agent answered without the nonce: False, so the
        message-dropped verdict stays reachable (the case's own nonce pin fails
        and the row reads FAIL). (d) The full stream of a run in which the
        product echoed the interjection as a user-message-shaped update (the
        user_message_chunk the shell writes for an interjection, beside the
        x.ai/session/interjection broadcast) and the agent never said it:
        False."""
        silent = self._armed(_BZ8_NO_CHUNK)
        echoed = self._armed(_BZ8_ECHOED)
        prose = self._armed(_BZ8_PROSE)
        user_echo = self._armed(dict(_BZ8_PROSE, inter_broadcast=True),
                                binary_cls=_BZ8UserEchoBinary)
        with self.subTest(leg="each arm fired mid-turn and sent the text"):
            for out in (silent, echoed, prose, user_echo):
                rec = out["ctx"].turns[0]["interjects"][0]
                self.assertEqual((rec["status"], rec["mid_turn"]),
                                 ("fired", True))
                self.assertEqual([r["params"]["text"]
                                  for _, r in out["binary"].interjects],
                                 [_BZ8_TEXT])
        own = _bz8_carrying(silent["ctx"].events, _BZ8_NONCE)
        with self.subTest(leg="a: the runner record alone"):
            self.assertIs(self._holds(own), False,
                          "the runner's own record satisfied its nonce pin: "
                          "%r" % (own,))
            self.assertEqual(
                [(e["type"], e.get("interject_text"), "data" in e)
                 for e in own],
                [("acp_interject", _BZ8_TEXT, False)])
        with self.subTest(leg="b: a model chunk repeats the nonce"):
            self.assertIs(self._holds(echoed["ctx"].events), True,
                          "a model-authored chunk did not satisfy the pin")
        with self.subTest(leg="c: the agent never said it"):
            self.assertIs(self._holds(prose["ctx"].events), False,
                          "a stream without the model's echo satisfied the "
                          "nonce pin: %r" % (prose["ctx"].events,))
            self.assertEqual(
                (prose["row"]["status"],
                 [a["ok"] for a in prose["row"]["asserts"]
                  if a["spec"]["id"] == "nonce_echoed"]),
                ("FAIL", [False]))
        with self.subTest(leg="d: a user-message echo only"):
            self.assertIs(self._holds(user_echo["ctx"].events), False,
                          "a user-message echo of the interjection satisfied "
                          "the nonce pin: %r" % (user_echo["ctx"].events,))
            self.assertEqual(
                [(u["content"]["text"], u["content"]["_meta"]["displayText"])
                 for u in user_echo["ctx"].acp.updates
                 if u.get("sessionUpdate") == "user_message_chunk"],
                [(_bz8_format_interjection(_BZ8_TEXT), _BZ8_TEXT)])
            self.assertEqual(
                sorted(user_echo["ctx"].turns[0]["interjects"][0]
                       ["mid_turn_evidence"]),
                ["completion_after_send",
                 "interjection_broadcast_before_completion"])

    def test_record_site_never_publishes_the_text_as_message_data(self):
        """The cheap structural pin beside the behavioural one (plan r3 BZ-8):
        in run.py's step dispatcher (_run_case_once), every event literal built
        from an arm record (a dict display that reads arm_rec and has a type
        key) has a type other than acp_message and no data key, exactly one of
        them is the acp_interject record and it carries arm_id, and no
        statement that reads arm_rec names a data key."""
        with open(run.__file__, encoding="utf-8") as fh:
            tree = ast.parse(fh.read())
        fn = next(n for n in ast.walk(tree)
                  if isinstance(n, ast.FunctionDef)
                  and n.name == "_run_case_once")

        def reads_arm_rec(node):
            return any(isinstance(n, ast.Name) and n.id == "arm_rec"
                       for n in ast.walk(node))

        def names_data(node):
            return any(isinstance(n, ast.Constant) and n.value == "data"
                       for n in ast.walk(node))

        sites, data_lines = [], []
        for node in ast.walk(fn):
            if isinstance(node, ast.Dict) and reads_arm_rec(node):
                pairs = {k.value: v for k, v in zip(node.keys, node.values)
                         if isinstance(k, ast.Constant)}
                if "type" in pairs:
                    t = pairs["type"]
                    sites.append((node.lineno,
                                  t.value if isinstance(t, ast.Constant)
                                  else None, sorted(pairs)))
            elif isinstance(node, (ast.Assign, ast.AugAssign, ast.AnnAssign,
                                   ast.Expr, ast.Return)):
                if reads_arm_rec(node) and names_data(node):
                    data_lines.append(node.lineno)
            elif isinstance(node, ast.For) and names_data(node.iter) \
                    and any(reads_arm_rec(s) for s in node.body):
                data_lines.append(node.lineno)
        with self.subTest(leg="no arm event is an acp_message or has data"):
            self.assertEqual(
                [s for s in sites if s[1] == "acp_message" or "data" in s[2]],
                [], sites)
            self.assertEqual(data_lines, [])
        with self.subTest(leg="one acp_interject record site with arm_id"):
            self.assertEqual(
                [(t, "arm_id" in keys) for _, t, keys in sites
                 if t == "acp_interject"],
                [("acp_interject", True)], sites)


class InterjectRoutedLegsTest(unittest.TestCase):
    """apex-bdfz BZ-8: the test-only legs coordinator rulings BZ-R7 and BZ-R8
    route to BZ-8, each through the real step dispatcher (_run_case_once over
    the fake peer, as MidTurnDispatchTest drives it): a turn that dies for a
    product reason reads VACUOUS with its acp_error pin failed (BZ-R7 F-3);
    the pumped path keeps the apex-ayl.28 desync guard (BZ-R7 F-4(c));
    require false never suppresses no_record (BZ-R7 F-7, D-4); a set_model
    arm armed beyond the turn's end sends nothing and reads VACUOUS (fix
    review F-3)."""

    maxDiff = None
    _dispatch = MidTurnDispatchTest._dispatch

    def test_dead_turn_reads_vacuous_with_its_acp_error_pin_failed(self):
        """BZ-R7 F-3 (plan r3 section 2.5's closing note, section 3 F-10): the
        parent turn dies for a product reason while its arm is still armed.
        Leg turn timeout: the step's 5 s budget runs out (the turn would end at
        60 s), so D-5 closes the arm missed(turn_timeout) and prompt_collect
        returns the timeout terminal. Leg error turn, in amendment A-1's order:
        turn_completed with stop_reason error, then the matching
        prompt_complete carrying the provider detail, then the session/prompt
        JSON-RPC error; D-2 closes the arm missed(turn_error). Neither close is
        harness-caused, so no runner cause is written: the declared model
        premise 0-hits and the row reads VACUOUS, not BLOCKED, with the arm
        record on the turn record, its acp_interject event, the dead turn's
        acp_error, and the failed pins interject_fired, nonce_echoed and
        no_acp_error all still in row["asserts"]."""
        error = {"code": -32603, "message": "Internal error",
                 "data": {"message": _BZ5_DETAIL, "http_status": 503}}
        legs = [
            ("turn timeout",
             {"reg_s": 0.05, "run_s": 59.9, "pc_gap_s": 0.15,
              "turn_chunks": [[(1.0, "C0"), (2.0, "C1")]]},
             lambda c: c["steps"][0].update(timeout_s=5),
             "turn_timeout", "timeout after 0.0s"),
            ("error turn in amendment A-1 order",
             {"reg_s": 0.05, "run_s": 1.0, "pc_gap_s": 0.8,
              "turn_stops": ["error"], "turn_agent_result": _BZ5_DETAIL,
              "prompt_error": error, "prompt_error_s": 0.1,
              "turn_chunks": [[(0.5, "C0")]]},
             None, "turn_error", _BZ5_DETAIL),
        ]
        for leg, script, mutate, reason, message in legs:
            out = self._dispatch(script, _bz7_arm({"after_tool_calls": 1}),
                                 mutate=mutate)
            row, ctx = out["row"], out["ctx"]
            with self.subTest(leg=leg):
                self.assertEqual(
                    (row["status"], row.get("blocked_reason"),
                     out["verdict"]["status"]),
                    ("VACUOUS", None, "VACUOUS"))
                self.assertIn("interject_fired",
                              out["verdict"]["verdict"]["vacuous_reason"])
                self.assertEqual(
                    sorted(a["spec"]["id"] for a in row["asserts"]
                           if not a["ok"]),
                    ["interject_fired", "no_acp_error", "nonce_echoed"])
                rec = row["turns"][0]["interjects"][0]
                self.assertEqual(
                    {k: rec[k] for k in ("arm_id", "status", "fired",
                                         "disarm_reason", "harness_caused",
                                         "cause")},
                    {"arm_id": "step0.arm0", "status": "missed",
                     "fired": False, "disarm_reason": reason,
                     "harness_caused": False, "cause": None})
                self.assertEqual(
                    [(e["arm_id"], e["status"], e["disarm_reason"])
                     for e in ctx.events if e.get("type") == "acp_interject"],
                    [("step0.arm0", "missed", reason)])
                self.assertEqual(
                    [e["error"]["message"] for e in ctx.events
                     if e.get("type") == "acp_error"], [message])
                self.assertEqual(out["binary"].interjects, [])

    def test_pumped_path_keeps_the_desync_guard(self):
        """BZ-R7 F-4(c) (batch-3 review F-2, batch-5 review F-3; apex-ayl.28):
        on an armed turn the pump reads the turn's queue row, which names this
        prompt, so the turn's own waiter knows its id. A same-session
        prompt_complete for an id the session never sent (an
        interject-fallback- prompt, plan r3 section 3 F-6) then arrives
        mid-turn, before the turn's own turn_completed. It is not adopted: the
        desync guard logs it verbatim, the pump reads on to the turn's own
        terminal, the chunk after the foreign line is still this turn's, the
        turn record's prompt_id is this prompt's, the foreign id never joins
        the consumed set, and the case's pins decide (PASS)."""
        logged, saved = [], run.log
        run.log = logged.append
        try:
            out = self._dispatch(_BZ7_ECHO, _bz7_arm({"after_tool_calls": 1}),
                                 binary_cls=_BZ8ForeignCompletionBinary)
        finally:
            run.log = saved
        ctx, b, row = out["ctx"], out["binary"], out["row"]
        ours = b.turn_pids[0]
        with self.subTest(leg="the foreign line came mid-turn, after the row"):
            self.assertLess(b.t_reg[0], b.foreign_at)
            self.assertLess(b.foreign_at, b.t_end[0])
        with self.subTest(leg="the guard refused and logged it"):
            self.assertEqual(
                [m for m in logged if m.startswith("ACP desync guard:")],
                [_BZ8_GUARD % (_BZ8_FOREIGN_PID, ours)])
        with self.subTest(leg="the turn stays with this prompt"):
            self.assertEqual((ctx.turns[0]["prompt_id"], ctx.turns[0]["stop"]),
                             (ours, "end_turn"))
            self.assertEqual(
                [e["data"] for e in ctx.events
                 if e.get("type") == "acp_message"],
                ["Noted: %s." % _BZ7_NONCE])
            self.assertIn(ours, ctx.acp._completed_prompt_ids)
            self.assertNotIn(_BZ8_FOREIGN_PID, ctx.acp._completed_prompt_ids)
        with self.subTest(leg="the arm and the case pins decide"):
            rec = ctx.turns[0]["interjects"][0]
            self.assertEqual((rec["status"], rec["mid_turn"]),
                             ("fired", True))
            self.assertEqual((row["status"], row.get("blocked_reason")),
                             ("PASS", None))

    def test_require_false_never_suppresses_no_record(self):
        """BZ-R7 F-7, the accepted BZ-7 decision D-4 (erratum E-1): require
        false suppresses only the cause a written record carries. A declared
        arm the turn left without any record is a rig failure whatever its
        require flag: with the records lost (the real pump reads the turn and
        fires the arm, and its record never reaches the dispatcher) and require
        false on the arm, the row is still BLOCKED with
        interject-unavailable: step0.arm0.no_record, with no acp_interject
        event and no record on the turn."""
        real_pump = run.AcpSession._pump_mid_turn

        def pump_without_records(sess, arms, waiter, deadline, **kw):
            # Fault injection (erratum E-1): the records never reach the
            # dispatcher.
            real_pump(sess, arms, waiter, deadline, **kw)
            return []

        out = self._dispatch(
            _BZ7_ECHO, _bz7_arm({"after_tool_calls": 1}, require=False),
            pump=pump_without_records)
        with self.subTest(leg="require false: still BLOCKED on no_record"):
            self.assertIs(out["case"]["steps"][0]["interject"][0]["require"],
                          False)
            self.assertEqual(
                (out["row"]["status"], out["row"].get("blocked_reason"),
                 out["verdict"]["status"]),
                ("BLOCKED", _BZ7_NO_RECORD, "BLOCKED"))
        with self.subTest(leg="no record reached the dispatcher"):
            self.assertEqual(out["row"]["turns"][0]["interjects"], [])
            self.assertEqual([e for e in out["ctx"].events
                              if e.get("type") == "acp_interject"], [])
            self.assertEqual(len(out["binary"].interjects), 1)

    def test_late_set_model_arm_sends_nothing_and_reads_vacuous(self):
        """Fix review F-3 (plan r3 section 3 F-13's late variant; section 2.4
        D-1 supersedes its "the switch still lands"): in RT-MSW-B's shape the
        set_model arm's after_s (5 s) lies beyond the long turn's end (2.1).
        The turn's completion closes the arm first: missed, turn_completed,
        not harness-caused, and nothing is sent, so the runner wrote no
        session/set_model (acp.log holds no such SEND line, and the fake never
        saw a step while the turn ran: early_step_fired false). No
        acp_set_model is recorded, both turns run on the pre-switch model and
        end in two acp_end. With BZ-L2's declared premise the row reads
        VACUOUS (the premise 0-hits; switch_landed and interject_mid_turn fail
        too); without it, FAIL on switch_landed."""
        late = {"at": {"after_s": 5.0}, "send": "set_model",
                "model": _BZ7_SWITCH_TARGET}
        declared = self._dispatch(_BZ7_SWITCH, dict(late),
                                  mutate=_bz7_switch)
        undeclared = self._dispatch(
            _BZ7_SWITCH, dict(late),
            mutate=lambda c: _bz7_switch(c, declared=False))
        ctx, b, row = declared["ctx"], declared["binary"], declared["row"]
        with self.subTest(leg="nothing was sent: no session/set_model"):
            with open(os.path.join(declared["run_dir"], "acp.log"),
                      encoding="utf-8") as fh:
                sent = [json.loads(line[6:]).get("method") for line in fh
                        if line.startswith("SEND: ")]
            self.assertNotIn("session/set_model", sent)
            self.assertEqual(sent.count("session/prompt"), 2)
            self.assertFalse(b.early_step_fired)
        with self.subTest(leg="the arm is missed, closed by turn_completed"):
            keys = ("arm_id", "send", "model", "status", "fired",
                    "disarm_reason", "harness_caused", "cause", "mid_turn",
                    "request_id")
            self.assertEqual(
                {k: ctx.turns[0]["interjects"][0].get(k) for k in keys},
                {"arm_id": "step0.arm0", "send": "set_model",
                 "model": _BZ7_SWITCH_TARGET, "status": "missed",
                 "fired": False, "disarm_reason": "turn_completed",
                 "harness_caused": False, "cause": None, "mid_turn": False,
                 "request_id": None})
        with self.subTest(leg="no acp_set_model and two acp_end"):
            self.assertEqual(
                [e["type"] for e in ctx.events if e.get("type") in (
                    "acp_interject", "acp_set_model", "acp_end",
                    "acp_error")],
                ["acp_interject", "acp_end", "acp_end"])
            self.assertEqual([(t["step"], t["model"]) for t in ctx.turns],
                             [(0, "m-test"), (1, "m-test")])
        with self.subTest(leg="the declared shape reads VACUOUS"):
            self.assertEqual(
                (row["status"], row.get("blocked_reason"),
                 declared["verdict"]["status"]),
                ("VACUOUS", None, "VACUOUS"))
            self.assertIn("interject_fired",
                          declared["verdict"]["verdict"]["vacuous_reason"])
            self.assertEqual(
                sorted(a["spec"]["id"] for a in row["asserts"]
                       if not a["ok"]),
                ["interject_fired", "interject_mid_turn", "switch_landed"])
        with self.subTest(leg="undeclared: FAIL on switch_landed"):
            self.assertEqual(
                (undeclared["row"]["status"],
                 undeclared["row"].get("blocked_reason")),
                ("FAIL", None))
            self.assertEqual(
                sorted(a["spec"]["id"] for a in undeclared["row"]["asserts"]
                       if not a["ok"]),
                ["switch_landed"])


def _vc_wire_path(wire):
    return {"responses": "/v1/responses", "messages": "/v1/messages",
            "chat_completions": "/v1/chat/completions"}[wire]


def _vc_capture(tmp, wire, bodies, resp_status=200):
    """Build a byte-exact wiretap capture dir (C 7-scenario pattern):
    req-NNN.json pretty-printed json.dumps(indent=2, ensure_ascii=True)
    + resp-NNN.jsonl (status line + one frame)."""
    d = os.path.join(tmp, "wire")
    os.makedirs(d, exist_ok=True)
    path = _vc_wire_path(wire)
    for i, body in enumerate(bodies, 1):
        req = {"n": i, "method": "POST", "path": path,
               "ts": "2026-01-01T00:00:%02d.000Z" % (i - 1),
               "headers": {"content-type": "application/json"},
               "body": body}
        with open(os.path.join(d, "req-%03d.json" % i), "w") as fh:
            fh.write(json.dumps(req, indent=2, ensure_ascii=True))
        with open(os.path.join(d, "resp-%03d.jsonl" % i), "w") as fh:
            fh.write(json.dumps({"n": i, "status": resp_status,
                                 "ts": "2026-01-01T00:00:%02d.100Z"
                                 % (i - 1),
                                 "headers": {}}) + "\n")
            fh.write(json.dumps({"frame":
                                 '{"type":"response.completed"}'})
                     + "\n")
    return d


def _vc_bodies(announce, mcp_call, tool_call, close, payload):
    """Synthetic responses-wire request bodies for the T-V1 scenarios
    (MODEL = m-test; the slim case shape, minimal items)."""
    bodies = [{"model": "m-test", "store": False,
               "input": [{"role": "user", "content": [
                   {"type": "input_text", "text": "turn one"}]}]}]
    if announce:
        bodies.append({"model": "m-test", "store": False,
                       "input": [
                           {"role": "system", "content": [
                               {"type": "input_text",
                                "text": ("MCP server(s) connected: "
                                         "- codegraph (7 tools): "
                                         "codegraph_explore, ...")}]}]})
    if mcp_call:
        args = json.dumps({"tool_name": "codegraph__codegraph_explore",
                           "arguments": {"query": "strip_model_bound_state"}})
        bodies.append({"model": "m-test", "store": False,
                       "input": [{"role": "assistant", "content": [
                           {"type": "function_call", "name": "use_tool",
                            "call_id": "call_m", "arguments": args}]}]})
    if tool_call:
        args = json.dumps({"command": "echo TS-ECHO"})
        bodies.append({"model": "m-test", "store": False,
                       "input": [{"role": "assistant", "content": [
                           {"type": "function_call",
                            "name": "run_terminal_command",
                            "call_id": "call_t", "arguments": args}]}]})
    if close:
        bodies.append({"model": "m-test", "store": False,
                       "input": [
                           {"role": "tool", "content": [
                               {"type": "function_call_output",
                                "call_id": "call_m",
                                "output": payload}]},
                           {"role": "tool", "content": [
                               {"type": "function_call_output",
                                "call_id": "call_t",
                                "output": "TS-ECHO"}]}]})
    return bodies


def _vc_case(wire, payload="strip_model_bound_state"):
    """The slim-shape case for the T-V1 pipeline (wire pins only)."""
    path = _vc_wire_path(wire)
    where = {"method": "POST", "path": path, "body.model": "m-test"}
    return {
        "schema_version": 1,
        "id": "TV1-PIPE",
        "title": "T-V1 pipeline case",
        "bead": "apex-ayl.22",
        "suite": "mcp-tool",
        "tier": "slim",
        "driver": "headless",
        "model": "m-test",
        "wire": wire,
        "steps": [{"op": "turn", "prompt": "Reply with exactly: TV1-OK"}],
        "mcp_calls": [{"id": "cg1", "server": "codegraph",
                       "tool": "codegraph_explore",
                       "expect_name": "codegraph__codegraph_explore",
                       "via": "search_tool_use_tool",
                       "args": {"query": "strip_model_bound_state"},
                       "premise": "model",
                       "output_contains": payload}],
        "tool_calls": [{"id": "sh1", "tool": "run_terminal_command",
                        "args": {"command": "echo TS-ECHO"},
                        "premise": "model",
                        "output_contains": "TS-ECHO"}],
        "assert": {"wire": [
            {"id": "cg_ann", "kind": "grep", "file": "req-*.json",
             "where": where, "any": True, "grep": "- codegraph ("},
            {"id": "cg_call", "kind": "grep", "file": "req-*.json",
             "where": where, "any": True,
             "grep": '\\"tool_name\\":'},
            {"id": "cg_tgt", "kind": "grep", "file": "req-*.json",
             "where": where, "any": True,
             "grep": '\\"codegraph__codegraph_explore\\"'},
            {"id": "sh_call", "kind": "grep", "file": "req-*.json",
             "where": where, "any": True,
             "grep": '\\"command\\": \\"echo TS-ECHO\\"'},
            {"id": "cg_rt", "kind": "grep", "file": "req-*.json",
             "where": where, "any": True,
             "grep": '"type": "function_call_output"'},
            {"id": "cg1_payload", "kind": "grep", "file": "req-*.json",
             "where": where, "any": True, "grep": payload},
            {"id": "model_f", "kind": "field", "file": "req-*.json",
             "where": where, "nth": 0, "path": "$.body.model",
             "eq": "m-test"},
            {"id": "status_rec", "kind": "resp_status",
             "file": "req-*.json", "where": where, "which": "last"},
        ]},
        "scoring": {"require_wire_evidence": True,
                    "vacuous_if": [
                        {"pin": "cg_ann", "class": "harness"},
                        {"pin": "cg_call", "class": "model"},
                        {"pin": "cg_tgt", "class": "model"},
                        {"pin": "sh_call", "class": "model"}]},
    }


def _vc_run(case, wire_dir):
    """Evaluate the case's wire pins through the REAL check_wire
    (explicit pins + the D-4b synthesized output_contains pins),
    then run the D-3 verdict engine (the T-V1 pipeline)."""
    results = []
    for spec in list(case["assert"]["wire"]) + run.synth_output_pins(case):
        results.append(run.check_wire(spec, wire_dir))
    pre = "FAIL" if any(not r.ok for r in results) else "PASS"
    return run.finalize_verdict(case, pre, results, wire_dir=wire_dir)


class VerdictMatrixTest(unittest.TestCase):
    """T-V1 (apex-ayl.22 D-3): the verdict matrix — 4-row premise table
    x 3 wires + tolerant = 9 synthetic byte-exact captures (built
    in-test, C 7-scenario pattern), including the binding precedence
    row (scored FAIL + 0-hit model premise -> VACUOUS; C §(c) BINDING:
    rule 2 wins over rule 3). Pre-fix RED: finalize_verdict does not
    exist and VACUOUS/FINDING-PASS are unexpressible."""

    def _scenario(self, wire, bodies, case_mutate=None, resp_status=200):
        tmp = tempfile.mkdtemp(prefix="rt-tv1-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        case = _vc_case(wire)
        if case_mutate:
            case_mutate(case)
        wire_dir = _vc_capture(tmp, wire, bodies, resp_status)
        return _vc_run(case, wire_dir)

    def _full_bodies(self):
        return _vc_bodies(True, True, True, True,
                          "strip_model_bound_state")

    def test_01_blocked_no_announcement(self):
        status, info = self._scenario(
            "responses", _vc_bodies(False, False, False, False, ""))
        self.assertEqual(status, "BLOCKED")
        self.assertTrue(any(p["pin"] == "cg_ann" and not p["hit"]
                            for p in info["premises"]))

    def test_02_vacuous_model_never_called(self):
        status, info = self._scenario(
            "responses", _vc_bodies(True, False, False, False, ""))
        self.assertEqual(status, "VACUOUS")
        self.assertTrue(any(p["pin"] == "cg_ann" and p["hit"]
                            for p in info["premises"]))

    def test_03_scored_fail(self):
        status, info = self._scenario(
            "responses", self._full_bodies(),
            case_mutate=lambda c: c["mcp_calls"][0].__setitem__(
                "output_contains", "NOT-PRESENT-PAYLOAD"))
        self.assertEqual(status, "FAIL")

    def test_04_pass(self):
        status, info = self._scenario("responses", self._full_bodies())
        self.assertEqual(status, "PASS")

    def test_05_precedence_scored_fail_plus_zero_hit_model(self):
        """THE binding precedence row: a scored pin FAILs while a model
        premise is 0-hit -> VACUOUS (rule 2 wins over rule 3; the probe
        was meaningless, the scored failure is uninterpretable)."""
        status, info = self._scenario(
            "responses", _vc_bodies(True, False, False, False, ""),
            case_mutate=lambda c: c["mcp_calls"][0].__setitem__(
                "output_contains", "NOT-PRESENT-PAYLOAD"))
        self.assertEqual(status, "VACUOUS")
        self.assertFalse(any(p["hit"] for p in info["premises"]
                             if p["class"] == "model"))

    def test_06_messages_wire_pass(self):
        bodies = _vc_bodies(True, True, True, True,
                            "strip_model_bound_state")
        status, info = self._scenario("messages", bodies)
        self.assertEqual(status, "PASS")

    def test_07_cc_wire_pass_and_counted(self):
        """The cc wire variant — and G1 in the loop: a
        /v1/chat/completions exchange counts as a model call."""
        tmp = tempfile.mkdtemp(prefix="rt-tv1cc-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        wire_dir = _vc_capture(
            tmp, "chat_completions", self._full_bodies())
        status, info = _vc_run(_vc_case("chat_completions"), wire_dir)
        self.assertEqual(status, "PASS")
        # 5 = the full bodies (turn-one base + announce + mcp_call +
        # tool_call + close): every cc request is a model call (G1).
        self.assertEqual(run.count_wire_model_calls(wire_dir), 5,
                         "cc requests must count as model calls (G1)")

    def test_08_tolerant_finding_pass(self):
        status, info = self._scenario(
            "responses", self._full_bodies(),
            case_mutate=lambda c: (c.update({"tolerant": True}),
                                   c["mcp_calls"][0].__setitem__(
                                       "output_contains",
                                       "NOT-PRESENT-PAYLOAD")))
        self.assertEqual(status, "FINDING-PASS")
        self.assertTrue(info["findings"])

    def test_09_blocked_wins_over_scored_fail(self):
        """Rule 1 dominance: harness premise 0-hit + scored FAIL ->
        BLOCKED (the rig failed; no product conclusion)."""
        status, info = self._scenario(
            "responses", _vc_bodies(False, False, False, False, ""),
            case_mutate=lambda c: c["mcp_calls"][0].__setitem__(
                "output_contains", "NOT-PRESENT-PAYLOAD"))
        self.assertEqual(status, "BLOCKED")


class NoEvidenceTest(unittest.TestCase):
    """T-V2 (apex-ayl.22 D-8): require_wire_evidence — a scored wire
    PASS without a resolvable citation is downgraded to
    BLOCKED/NO-EVIDENCE (never a silent pass). Pre-fix RED: the audit
    does not exist."""

    def _case(self, tmp):
        case = _vc_case("responses")
        return case

    def test_uncitable_pass_downgraded(self):
        tmp = tempfile.mkdtemp(prefix="rt-tv2-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        wire_dir = _vc_capture(
            tmp, "responses",
            _vc_bodies(True, True, True, True, "strip_model_bound_state"))
        results = []
        for spec in self._case(tmp)["assert"]["wire"]:
            r = run.check_wire(spec, wire_dir)
            r.cite = None  # simulate an engine that passed without citing
            results.append(r)
        status, info = run.finalize_verdict(
            self._case(tmp), "PASS", results, wire_dir=wire_dir)
        self.assertEqual(status, "BLOCKED")
        self.assertIn("NO-EVIDENCE",
                      (info.get("blocked_reason") or ""))

    def test_citable_pass_not_downgraded(self):
        tmp = tempfile.mkdtemp(prefix="rt-tv2b-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        wire_dir = _vc_capture(
            tmp, "responses",
            _vc_bodies(True, True, True, True,
                       "strip_model_bound_state"))
        case = self._case(tmp)
        results = [run.check_wire(s, wire_dir)
                   for s in case["assert"]["wire"]]
        status, info = run.finalize_verdict(case, "PASS", results,
                                            wire_dir=wire_dir)
        self.assertEqual(status, "PASS")
        self.assertEqual(info["evidence_violations"], [])


class RowSkipExemptionTest(unittest.TestCase):
    """apex-ayl.62 (sw1e M6 rig defect): the auto-emitted wire.row-skip
    record (a catalog row filtered out by --rows) is a recon record —
    it makes no per-file claim, so the D-8 require_wire_evidence audit
    must not downgrade the case to BLOCKED/NO-EVIDENCE, and recon
    records must not populate the vacuous_if pin map. Pre-fix RED:
    row-skip carried recon=False + cite=None, so every --rows run
    BLOCKed on uncitable 'scored' records (sw1e: RT-M6 BLOCKED with
    42 row-skip records)."""

    def _wire_dir(self):
        tmp = tempfile.mkdtemp(prefix="rt-rowskip-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        with open(os.path.join(tmp, "req-001.json"), "w") as fh:
            json.dump({"method": "POST", "path": "/v1/responses",
                       "body": {"model": "m1"}}, fh, indent=2)
        return tmp

    def _scored_pass(self, wire_dir):
        spec = {"id": "model_ride", "kind": "grep", "file": "req-*.json",
                "where": {"body.model": "m1"},
                "any": True, "grep": '"model": "m1"'}
        r = run.check_wire(spec, wire_dir)
        self.assertTrue(r.ok, r.detail)
        return r

    def _rowskip(self):
        return run.AssertResult(
            {"id": "model_ride_row", "kind": "grep", "file": "req-*.json",
             "where": {"body.model": "m2"}, "any": True,
             "grep": '"model": "m1"'},
            "wire.row-skip", True,
            "row m2 not in this run (filtered by --rows)",
            "wire: skipped", recon=True)

    def test_rowskip_excluded_from_evidence_audit(self):
        wire_dir = self._wire_dir()
        case = {"scoring": {"require_wire_evidence": True}}
        results = [self._scored_pass(wire_dir), self._rowskip()]
        self.assertEqual(run.audit_wire_evidence(case, results,
                                                 wire_dir=wire_dir), [])

    def test_rowskip_only_verdict_not_blocked(self):
        wire_dir = self._wire_dir()
        case = {"scoring": {"require_wire_evidence": True}}
        results = [self._scored_pass(wire_dir), self._rowskip()]
        status, info = run.finalize_verdict(case, "PASS", results,
                                            wire_dir=wire_dir)
        self.assertEqual(status, "PASS")
        self.assertIsNone(info["blocked_reason"])


class CapsBlockTest(unittest.TestCase):
    """T-V3 (apex-ayl.22 D-3): caps block shape + counts in report.json
    (blocked/vacuous/finding) + the extended status order. Pre-fix
    RED: report.json has no caps block and the status order lacks
    FINDING-PASS/VACUOUS. Also pins the D-15 triage block on
    FAIL/BLOCKED rows."""

    def _rows(self):
        mk = lambda i, s: {
            "id": i, "title": i, "status": s, "duration_s": 1.0,
            "model_calls": 1, "turns": [], "asserts": [], "snapshots": [],
            "run_dir": None}
        rows = [mk("RA", "PASS"), mk("RB", "FINDING-PASS"),
                mk("RC", "RECON"), mk("RD", "VACUOUS"),
                mk("RE", "FAIL"), mk("RF", "BLOCKED"),
                mk("RG", "SKIP")]
        rows[6]["skipped"] = "budget"
        for r in rows:
            r["run_dir"] = tempfile.mkdtemp(prefix="rt-tv3-")
        # D-15: FAIL and BLOCKED rows carry a triage block.
        rows[4]["triage"] = {"classification_hint": "product",
                             "session_dir": "/x/session",
                             "ready_commands": ["grep -c x wire/"]}
        rows[5]["triage"] = {"classification_hint": "env",
                             "session_dir": "/x/session",
                             "ready_commands": []}
        return rows

    def test_caps_block_shape_and_counts(self):
        out = tempfile.mkdtemp(prefix="rt-tv3out-")
        self.addCleanup(lambda: shutil.rmtree(out, ignore_errors=True))
        args = argparse.Namespace(live_home="/x", wirecap=True,
                                  budget=40, ambient_key="")
        env_meta = {"ts": "t", "bin": "b", "bin_sha": "s", "git": "g",
                    "key_sha": "k", "upstream": "u"}
        md = run.write_report(out, self._rows(), env_meta, args, 0, [])
        with open(os.path.join(out, "report.json")) as fh:
            rep = json.load(fh)
        cb = rep.get("caps_block")
        self.assertIsNotNone(cb, "report.json must carry the caps block")
        self.assertEqual(cb["blocked_count"], 1)
        self.assertEqual(cb["vacuous_count"], 1)
        self.assertEqual(cb["finding_count"], 1)
        self.assertEqual(cb["caps"], {
            "full_blocked_max_no_ruling": 3,
            "slim_vacuous_ruling_threshold": 3,
            "env_retry_max_per_cell": 1})
        # status order: [PASS, FINDING-PASS, RECON, VACUOUS, FAIL,
        # BLOCKED, SKIP]
        self.assertEqual([r["id"] for r in rep["rows"]],
                         ["RA", "RB", "RC", "RD", "RE", "RF", "RG"])
        # D-15 triage block survives into report.json.
        by_id = {r["id"]: r for r in rep["rows"]}
        self.assertEqual(by_id["RE"]["triage"]["classification_hint"],
                         "product")
        self.assertEqual(by_id["RF"]["triage"]["classification_hint"],
                         "env")
        self.assertTrue(os.path.exists(md))


# ---------------------------------------------------------------------------
# apex-ayl.22 W3 — D-5 env retry + D-6 timeout overrides
# ---------------------------------------------------------------------------

class SilentSetModelBinary(ScriptedBinary):
    """ScriptedBinary that never answers session/set_model (the
    never-answers world: set_model must burn its budget and return the
    timeout error shape)."""

    def on_send(self, d, t):
        if d.get("method") == "session/set_model":
            return
        super(SilentSetModelBinary, self).on_send(d, t)


def _retry_capture(tmp, model, resp_status):
    """One model request + one response at `resp_status` — the shape
    _last_response_status / _retry_decision read."""
    return _vc_capture(tmp, "responses",
                       [{"model": model, "store": False,
                         "input": [{"role": "user", "content": "ping"}]}],
                       resp_status=resp_status)


class RetryDecisionTest(unittest.TestCase):
    """T-R1 (apex-ayl.22 D-5): the retry decision table — 404 +
    model-present -> retry (fresh home attempt-N); 404 + model-gone ->
    no retry (catalog change, 404 != 400); 400 -> never; 502/503 same
    as 404; max_attempts enforced; the recheck verdict is surfaced
    (the loop records it as a recon row). Pre-fix RED:
    run._retry_decision / run.recheck_models_available do not exist."""

    def _case(self, recheck=True, on_status=(404, 502, 503),
              max_attempts=1):
        return {"id": "RT-R1", "model": "m-x",
                "retry": {"on_status": list(on_status),
                          "max_attempts": max_attempts,
                          "backoff_s": 0,
                          "recheck_models": recheck}}

    def test_404_present_retries(self):
        tmp = tempfile.mkdtemp(prefix="rt-r1-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        wire = _retry_capture(tmp, "m-x", 404)
        calls = []
        self._orig = run.recheck_models_available
        run.recheck_models_available = \
            lambda upstream, model, key: (calls.append(1), True)[1]
        try:
            ok, reason, recheck = run._retry_decision(
                self._case(), wire, "m-x", attempt=1, max_attempts=1,
                upstream="http://upstream.test")
        finally:
            run.recheck_models_available = self._orig
        self.assertTrue(ok, reason)
        self.assertEqual(recheck, True,
                         "recheck verdict must be surfaced")
        self.assertEqual(len(calls), 1,
                         "recheck_models must drive a catalog lookup")

    def test_404_gone_no_retry(self):
        tmp = tempfile.mkdtemp(prefix="rt-r1g-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        wire = _retry_capture(tmp, "m-x", 404)
        self._orig = run.recheck_models_available
        run.recheck_models_available = lambda *a, **k: False
        try:
            ok, reason, recheck = run._retry_decision(
                self._case(), wire, "m-x", attempt=1, max_attempts=1,
                upstream="http://upstream.test")
        finally:
            run.recheck_models_available = self._orig
        self.assertFalse(ok)
        self.assertIn("gone", reason)
        self.assertEqual(recheck, False)

    def test_400_never_retries(self):
        tmp = tempfile.mkdtemp(prefix="rt-r1b-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        wire = _retry_capture(tmp, "m-x", 400)
        self._orig = run.recheck_models_available

        def _boom(*a, **k):
            self.fail("recheck must not run for a non-on_status 400")
        run.recheck_models_available = _boom
        try:
            ok, reason, recheck = run._retry_decision(
                self._case(), wire, "m-x", attempt=1, max_attempts=1,
                upstream="http://upstream.test")
        finally:
            run.recheck_models_available = self._orig
        self.assertFalse(ok)
        self.assertEqual(recheck, None)

    def test_502_503_like_404(self):
        for status in (502, 503):
            tmp = tempfile.mkdtemp(prefix="rt-r1s%d-" % status)
            self.addCleanup(lambda d=tmp: shutil.rmtree(d,
                                                        ignore_errors=True))
            wire = _retry_capture(tmp, "m-x", status)
            self._orig = run.recheck_models_available
            run.recheck_models_available = lambda *a, **k: True
            try:
                ok, reason, _ = run._retry_decision(
                    self._case(), wire, "m-x", attempt=1,
                    max_attempts=1, upstream="http://upstream.test")
            finally:
                run.recheck_models_available = self._orig
            self.assertTrue(ok, "%d must be an env-retry status: %s"
                            % (status, reason))

    def test_cap_enforced(self):
        tmp = tempfile.mkdtemp(prefix="rt-r1c-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        wire = _retry_capture(tmp, "m-x", 404)
        self._orig = run.recheck_models_available
        run.recheck_models_available = lambda *a, **k: True
        try:
            ok, reason, _ = run._retry_decision(
                self._case(max_attempts=1), wire, "m-x", attempt=2,
                max_attempts=1, upstream="http://upstream.test")
        finally:
            run.recheck_models_available = self._orig
        self.assertFalse(ok)
        self.assertIn("cap", reason)


import http.server as _http_server


class _FakeUpstreamHandler(_http_server.BaseHTTPRequestHandler):
    """stdlib BaseHTTPRequestHandler: POST /v1/<route> returns 404 on
    the FIRST hit, 200 after (the env-flap-then-recover shape);
    GET /v1/models answers the recheck with the model present."""
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        pass

    def _send(self, status, payload):
        body = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        ln = int(self.headers.get("content-length") or 0)
        if ln:
            self.rfile.read(ln)
        srv = self.server
        with srv.lock:
            srv.posts += 1
            n = srv.posts
        if n == 1:
            self._send(404, {"error": "model not found (flap)"})
        else:
            self._send(200, {"ok": True, "post": n})

    def do_GET(self):
        if self.path.startswith("/v1/models"):
            self._send(200, {"data": [{"id": "m-x"}]})
        else:
            self._send(404, {"error": "no route"})


def _start_fake_upstream():
    import http.server
    import threading
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0),
                                          _FakeUpstreamHandler)
    srv.posts = 0
    srv.lock = threading.Lock()
    t = threading.Thread(target=srv.serve_forever, daemon=True)
    t.start()
    return srv, "http://127.0.0.1:%d" % srv.server_address[1]


def _write_fake_live_home(tmp, upstream, row_name=None,
                          env_key="WIRETAP_T2_KEY_X",
                          default_upstream=None):
    """A minimal live GROK_HOME: config pointing at the local fake
    upstream + the auth stub HermeticHome copies.

    r9 fixture extension (the default keeps the bytes every existing caller
    produces): `default_upstream` is where the config DEFAULT lives
    (`models_base_url`, which `live_upstream` reads) while `upstream` stays
    the ROW's origin. The recheck's r3a A13 claim is about which of the two
    is asked, so a fixture with one origin cannot express it."""
    live = os.path.join(tmp, "live")
    os.makedirs(live)
    stub = os.path.join(live, "proxy-auth-stub.sh")
    with open(stub, "w") as fh:
        fh.write("#!/bin/sh\nexit 0\n")
    os.chmod(stub, 0o755)
    cfg = ('base_url = "%s/v1"\n'
           'models_base_url = "%s/v1"\n'
           'auth_stub = "%s"\n'
           "[features]\n"
           "turn_summary = false\n"
           # r3b B4 (fixture adaptation; no assertion changes): T2's route
           # map fails closed on an unmapped id and this home's fake binary
           # sends body.model "m-x", so the fixture declares a named row for
           # it, routed to the fake upstream. Its env_key names a TEST-ONLY
           # variable, never a fallback-chain name (r3c seat-1 F4).
           '@@ROWHDR@@\n'
           'base_url = "%s/v1"\n'
           'api_backend = "responses"\n'
           'env_key = "@@ENKV@@"\n'
           % (upstream, default_upstream or upstream, stub, upstream))
    cfg = (cfg.replace("@@ROWHDR@@",
                       '[model."%s"]\nmodel = "m-x"' % row_name
                       if row_name else '[model."m-x"]')
           .replace("@@ENKV@@", env_key))
    with open(os.path.join(live, "config.toml"), "w") as fh:
        fh.write(cfg)
    return live


def _write_fake_binary(tmp, upstream, model="m-x", send_auth_env=None,
                       prewarm_record=None, auth_literal=None,
                       ran_record=None):
    """A fake pager binary: reads the HERMETIC config's base_url,
    POSTs one turn request through it, exits 0 iff the upstream said
    200. (The hermetic base_url is the wiretap, which forwards to the
    fake upstream — fully offline.)

    T2 fixture extension (the defaults keep the bytes every existing caller
    produces): `model` picks the body.model byte so a fixture can name a
    mapped or an unmapped id; `send_auth_env` attaches that variable's value
    as an authorization header; `prewarm_record` makes the binary first GET
    the origin root exactly as the real binary's prewarm_transport does
    (`client.get(origin)`, prewarm.rs:63) and write the reply there, so a
    test can tell the wiretap's local static answer from a forwarded one
    (r3c seat-1 F3). `auth_literal` sends that exact string as the child's
    `authorization` header instead of reading a variable: T3 narrows the
    child environment to an allowlist (§4.3.4), so a test that needs the
    CHILD to own a credential cannot plant one by name. `ran_record`
    (apex-sk8i, spec r2 §4 fixture extensions) makes the binary write one
    fixed line to that path before it posts; only the file's existence is
    read (it ran, or it never ran), never anything the binary saw.
    """
    b = os.path.join(tmp, "fake-grok")
    if auth_literal is not None:
        # the literal rides inside a single-quoted shell argument
        assert "'" not in auth_literal, "auth_literal must not quote"
    with open(b, "w") as fh:
        fh.write(
            "#!/bin/sh\n"
            "python3 - \"$GROK_HOME/config.toml\" "
            "'@@MODEL@@' '@@AUTH@@' '@@PREC@@' '@@AUTHLIT@@' <<'PYEOF'\n"
            "import json, os, re, sys, urllib.request, urllib.error\n"
            "@@RANREC@@"
            "cfg = open(sys.argv[1]).read()\n"
            "base = re.search(r'^base_url = \"([^\"]+)\"', cfg, re.M)\n"
            "if not base:\n"
            "    sys.exit(3)\n"
            "base = base.group(1)\n"
            "model, auth_env, prec = sys.argv[2], sys.argv[3], sys.argv[4]\n"
            "authlit = sys.argv[5] if len(sys.argv) > 5 else '-'\n"
            "hdrs = {'content-type': 'application/json'}\n"
            "if auth_env != '-':\n"
            "    hdrs['authorization'] = ('Bearer '\n"
            "                             + os.environ.get(auth_env, ''))\n"
            "if authlit != '-':\n"
            "    hdrs['authorization'] = authlit\n"
            "if prec != '-':\n"
            "    origin = base[:-3] if base.endswith('/v1') else base\n"
            "    try:\n"
            "        pr = urllib.request.urlopen(origin + '/', timeout=15)\n"
            "        text = '%d %s' % (pr.status, pr.read().decode())\n"
            "    except urllib.error.HTTPError as e:\n"
            "        text = '%d %s' % (e.code, e.read().decode())\n"
            "    except Exception as e:\n"
            "        text = 'ERR %r' % (e,)\n"
            "    open(prec, 'w').write(text)\n"
            "req = urllib.request.Request(\n"
            "    base + '/responses',\n"
            "    data=json.dumps({'model': model, 'store': False,\n"
            "                     'input': [{'role': 'user',\n"
            "                                'content': 'ping'}]})\n"
            "            .encode(),\n"
            "    headers=hdrs)\n"
            "try:\n"
            "    r = urllib.request.urlopen(req, timeout=15)\n"
            "    r.read()\n"
            "    sys.exit(0)\n"
            "except urllib.error.HTTPError:\n"
            "    sys.exit(1)\n"
            "PYEOF\n"
            # token substitution, not %-formatting: the embedded python
            # itself contains % operators, which %-formatting would eat.
            .replace("@@MODEL@@", model)
            .replace("@@AUTH@@", send_auth_env or "-")
            .replace("@@PREC@@", prewarm_record or "-")
            .replace("@@AUTHLIT@@", auth_literal or "-")
            .replace("@@RANREC@@",
                     "open(%s, 'w').write('ran\\n')\n" % json.dumps(ran_record)
                     if ran_record is not None else ""))
    os.chmod(b, 0o755)
    return b



class RetryLoopTest(unittest.TestCase):
    """T-R1 loop mechanics (apex-ayl.22 D-5): a FAIL cell whose last
    response status is in retry.on_status retries the WHOLE case once
    in a FRESH hermetic home under <case>/attempt-N/ — evidence
    retained (no overwrite), backoff slept, the recheck recorded as a
    recon row — and a retried-pass cell is stability FLAKY. Offline:
    fake live home -> local fake upstream, fake binary (one POST per
    attempt), the real Wiretap. Pre-fix RED: run_case has no retry
    loop (no attempt-2 dir, no FLAKY, no recheck row)."""

    # Sentinel backoff: distinct from the 0.2s Wiretap.wait_ready poll
    # (the recorded sleep is the fake — nothing really sleeps).
    BACKOFF = 7.777

    def _run_retry_case(self, backoff_s=None, recheck_models=True):
        if backoff_s is None:
            backoff_s = self.BACKOFF
        tmp = tempfile.mkdtemp(prefix="rt-rloop-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        srv, upstream = _start_fake_upstream()
        self.addCleanup(srv.shutdown)
        # T3 §4.3.3 (fixture adaptation; no assertion changes): this
        # fixture's row declares env_key WIRETAP_T2_KEY_X, so the
        # absent-and-reachable rule blocks the case unless the test
        # plants that test-only name (the house guard, r3c F4).
        _t2_env_guard(self)
        live = _write_fake_live_home(tmp, upstream)
        fake_bin = _write_fake_binary(tmp, upstream)
        out = os.path.join(tmp, "report")
        os.makedirs(out)
        case = {"id": "RT-R1-LOOP", "title": "env retry loop",
                "driver": "headless", "model": "m-x",
                "steps": [{"op": "turn", "prompt": "ping"}],
                "wirecap": True, "est_calls": 1,
                "retry": {"on_status": [404, 502, 503],
                          "max_attempts": 1,
                          "backoff_s": backoff_s,
                          "recheck_models": recheck_models}}
        args = argparse.Namespace(out=out, live_home=live,
                                  wirecap=True, budget=10,
                                  ambient_key="", bin=fake_bin,
                                  rows=None, keep_home=False,
                                  campaign_id=None)
        sleeps = []
        orig_sleep = run.time.sleep
        run.time.sleep = lambda s: sleeps.append(s)
        try:
            row = run.run_case(case, args, run.Budget(args.budget))
        finally:
            run.time.sleep = orig_sleep
        run_dir = os.path.join(out, "rt-r1-loop")
        return row, run_dir, srv, sleeps

    def test_flap_retries_to_pass_flaky(self):
        row, run_dir, srv, sleeps = self._run_retry_case()
        self.assertEqual(row["status"], "PASS",
                         "attempt 2 (upstream 200) must pass")
        self.assertEqual(row["stability"], "FLAKY",
                         "a retried-pass cell is FLAKY (runbook L84)")
        self.assertEqual(row["attempt"], 2)
        attempt2 = os.path.join(run_dir, "attempt-2")
        self.assertTrue(os.path.isdir(attempt2),
                        "retry must run in a fresh <case>/attempt-2/")
        # Evidence RETAINED: attempt 1's 404 capture stays at the root;
        # attempt 2's 200 capture lives under attempt-2 (no overwrite).
        self.assertTrue(os.path.isfile(
            os.path.join(run_dir, "wire", "resp-001.jsonl")))
        self.assertTrue(os.path.isfile(
            os.path.join(attempt2, "wire", "resp-001.jsonl")))
        with open(os.path.join(run_dir, "wire", "resp-001.jsonl")) as fh:
            self.assertEqual(json.loads(fh.readline())["status"], 404)
        with open(os.path.join(attempt2, "wire", "resp-001.jsonl")) as fh:
            self.assertEqual(json.loads(fh.readline())["status"], 200)
        # Per-attempt verdict.json (machine result, never overwritten).
        self.assertTrue(os.path.isfile(
            os.path.join(run_dir, "verdict.json")))
        with open(os.path.join(attempt2, "verdict.json")) as fh:
            v2 = json.load(fh)
        self.assertEqual(v2["attempt"], 2)
        self.assertEqual(v2["outcome"], "PASS")
        # The recheck is recorded as a recon row on the final row.
        rechecks = [t for t in row["turns"]
                    if t.get("op") == "recheck_models"]
        self.assertEqual(len(rechecks), 1,
                         "recheck must be recorded as a recon row")
        self.assertEqual(rechecks[0]["present"], True)
        # The declared backoff was slept exactly once (the 0.2s entries
        # are the wait_ready polls, not the retry backoff).
        self.assertEqual([s for s in sleeps if s == self.BACKOFF],
                         [self.BACKOFF])
        self.assertEqual(srv.posts, 2, "one POST per attempt")

    def test_404_gone_fails_no_retry(self):
        """Catalog change (model gone on recheck) = FAIL, no retry —
        404 != 400 (manifest §4)."""
        tmp = tempfile.mkdtemp(prefix="rt-rloopg-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        srv, upstream = _start_fake_upstream()
        self.addCleanup(srv.shutdown)
        # T3 §4.3.3 fixture adaptation (see _run_retry_case).
        _t2_env_guard(self)
        # Model gone: /v1/models answers without m-x.
        orig_do_get = _FakeUpstreamHandler.do_GET

        def _gone_get(self):
            if self.path.startswith("/v1/models"):
                self._send(200, {"data": []})
            else:
                orig_do_get(self)
        _FakeUpstreamHandler.do_GET = _gone_get
        try:
            live = _write_fake_live_home(tmp, upstream)
            fake_bin = _write_fake_binary(tmp, upstream)
            out = os.path.join(tmp, "report")
            os.makedirs(out)
            case = {"id": "RT-R1-GONE", "title": "catalog change",
                    "driver": "headless", "model": "m-x",
                    "steps": [{"op": "turn", "prompt": "ping"}],
                    "wirecap": True, "est_calls": 1,
                    "retry": {"on_status": [404, 502, 503],
                              "max_attempts": 1, "backoff_s": 0,
                              "recheck_models": True}}
            args = argparse.Namespace(out=out, live_home=live,
                                      wirecap=True, budget=10,
                                      ambient_key="", bin=fake_bin,
                                      rows=None, keep_home=False,
                                      campaign_id=None)
            row = run.run_case(case, args, run.Budget(args.budget))
        finally:
            _FakeUpstreamHandler.do_GET = orig_do_get
        run_dir = os.path.join(out, "rt-r1-gone")
        self.assertEqual(row["status"], "FAIL")
        self.assertNotIn("stability", row)
        self.assertFalse(os.path.isdir(os.path.join(run_dir,
                                                    "attempt-2")),
                         "a catalog change must NOT retry")
        self.assertEqual(srv.posts, 1, "no second attempt")


class TimeoutOverrideTest(ReplayBase):
    """T-T1 (apex-ayl.22 D-6): timeout overrides — set_model uses the
    case value (fake clock), acp_init_s beats the ACP_INIT_TIMEOUT_S
    floor (fake clock), turn_s wins over watchdog_s (the resolver).
    Pre-fix RED: AcpSession has no init_timeout_s / set_model_timeout_s
    (hardcoded ACP_INIT_TIMEOUT_S in start(), t=60 in set_model);
    run._effective_turn_timeout / _effective_acp_init_timeout /
    _effective_set_model_timeout / DEFAULT_SET_MODEL_TIMEOUT_S do not
    exist."""

    def _make_silent(self):
        clock = FakeClock()
        stdout = FakeStdout(clock)
        binary = SilentSetModelBinary(stdout, clock)
        tmp = tempfile.mkdtemp(prefix="rt-tt1-")
        os.makedirs(os.path.join(tmp, "home"))
        os.makedirs(os.path.join(tmp, "cwd"))
        self._tmps.append(tmp)
        sess = ReplaySession(binary, tmp)
        sess._install_fakes()
        self._patch(sess)
        return sess

    def test_set_model_uses_case_value(self):
        sess = self._make_silent()
        try:
            sess.start()
            sess.set_model_timeout_s = 5
            resp = sess.set_model("m-target")
            self.assertIn("error", resp)
            self.assertIn("timeout after 5s", resp["error"]["message"],
                          "set_model must use the case budget, not the "
                          "hardcoded 60s")
            self.assertGreaterEqual(sess._binary.clock.now, 5)
            self.assertLess(sess._binary.clock.now, 59,
                           "the 60s hardcoded budget leaked through")
        finally:
            sess.cleanup()

    def test_init_case_value_beats_floor(self):
        # The init line needs 30 fake seconds at 867 BPS; the case
        # value 10 must cut the wait at ~10s. Pre-fix: the 180s floor
        # lets the line complete at 30s and start() succeeds.
        clock = FakeClock()
        stdout = FakeStdout(clock)
        binary = ScriptedBinary(stdout, clock, init_bps=867,
                                init_line=build_init_line())
        tmp = tempfile.mkdtemp(prefix="rt-tt1i-")
        os.makedirs(os.path.join(tmp, "home"))
        os.makedirs(os.path.join(tmp, "cwd"))
        self._tmps.append(tmp)
        sess = ReplaySession(binary, tmp)
        sess._install_fakes()
        self._patch(sess)
        try:
            sess.init_timeout_s = 10
            with self.assertRaises(RuntimeError):
                sess.start()
            self.assertLess(clock.now, 30,
                            "the case init budget must cut the wait "
                            "before the 30s line completes")
        finally:
            sess.cleanup()

    def test_turn_s_beats_watchdog_s(self):
        self.assertEqual(run._effective_turn_timeout(
            {"timeouts": {"turn_s": 120}, "watchdog_s": 300}), 120)
        self.assertEqual(run._effective_turn_timeout(
            {"watchdog_s": 45}), 45)
        self.assertEqual(run._effective_turn_timeout({}), 300)
        self.assertEqual(run._effective_turn_timeout(
            {"timeouts": {}}), 300)

    def test_acp_resolvers_default(self):
        self.assertEqual(run.DEFAULT_SET_MODEL_TIMEOUT_S, 180)
        self.assertEqual(run._effective_acp_init_timeout(
            {"timeouts": {"acp_init_s": 240}}), 240)
        self.assertEqual(run._effective_acp_init_timeout({}),
                         run.ACP_INIT_TIMEOUT_S)
        self.assertEqual(run._effective_set_model_timeout({}),
                         run.DEFAULT_SET_MODEL_TIMEOUT_S)
        self.assertEqual(run._effective_set_model_timeout(
            {"timeouts": {"acp_set_model_s": 90}}), 90)


# ---------------------------------------------------------------------------
# apex-ayl.22 W4 — D-4c drift / D-7 env / D-9 3-way / D-10 cwd + no-watch /
# D-12 recon any/first / D-8 session evidence / D-14 campaign
# ---------------------------------------------------------------------------

class PromptDriftTest(unittest.TestCase):
    """T-S3 (apex-ayl.22 D-4c): prompt<->declaration drift — for a
    2-step case, the turn-2 prompt must carry each mcp_call
    expect_name (default server__tool), each declared arg value, each
    tool_call arg value, and the id:"done" text_contains pin value;
    a mismatch is a case-contract error (fails at validation, not at
    the wire). The in-tree slim cases (authored against this contract)
    pass clean. Pre-fix RED: the drift check does not exist."""

    FQ = "codegraph__codegraph_explore"
    QUERY = "strip_model_bound_state"
    ECHO = "T21-DRIFT-ECHO"
    DONE = "T21-DRIFT-DONE"

    def _case(self, fq=True, args_ok=True, echo_ok=True, done_ok=True):
        name = self.FQ if fq else "the explore tool"
        query = self.QUERY if args_ok else "some-other-query"
        echo = self.ECHO if echo_ok else "OTHER-ECHO"
        done = self.DONE if done_ok else "SOMETHING-ELSE"
        return {
            "schema_version": 1, "id": "DRIFT-1", "title": "drift",
            "bead": "apex-ayl.22", "suite": "mcp-tool", "tier": "slim",
            "driver": "headless", "model": "m-d", "wire": "responses",
            "cwd": "/tmp/anywhere",
            "steps": [
                {"op": "turn", "prompt": "nonce ONE"},
                {"op": "turn",
                 "prompt": ("Perform: (1) use_tool to call %s with "
                            "query '%s'; (2) run_terminal_command "
                            "command: echo %s. Then reply with "
                            "exactly: %s") % (name, query, echo, done)},
            ],
            "mcp_calls": [{"id": "cg1", "server": "codegraph",
                           "tool": "codegraph_explore",
                           "expect_name": self.FQ,
                           "args": {"query": self.QUERY}}],
            "tool_calls": [{"id": "sh1", "tool": "run_terminal_command",
                            "args": {"command": "echo %s" % self.ECHO}}],
            "assert": {
                "ndjson": [{"id": "done", "op": "text_contains",
                            "value": self.DONE}],
                "wire": [{"id": "cg_ann", "kind": "grep",
                          "file": "req-*.json",
                          "where": {"method": "POST",
                                    "path": "/v1/responses",
                                    "body.model": "m-d"},
                          "any": True, "grep": "- codegraph ("}],
            },
            "scoring": {"require_wire_evidence": True,
                        "vacuous_if": [{"pin": "cg_ann",
                                        "class": "harness"}]},
        }

    def _gate(self, case, tmp, tag):
        p = _write_case(tmp, "rt-s3-%s.json" % tag, case)
        return run.validate_case_file(p)

    def test_no_drift_clean(self):
        tmp = tempfile.mkdtemp(prefix="rt-s3a-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        errs = self._gate(self._case(), tmp, "clean")
        self.assertEqual(errs, [], "no drift: %s" % errs)

    def test_missing_fq_name_named(self):
        tmp = tempfile.mkdtemp(prefix="rt-s3b-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        errs = self._gate(self._case(fq=False), tmp, "fq")
        joined = " | ".join(errs)
        self.assertIn(self.FQ, joined,
                      "the error must name the missing FQ name: %s"
                      % joined)
        self.assertTrue(any("drift" in e for e in errs),
                        "the error must be a drift/case-contract "
                        "error: %s" % errs)

    def test_missing_arg_value_named(self):
        tmp = tempfile.mkdtemp(prefix="rt-s3c-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        errs = self._gate(self._case(args_ok=False), tmp, "arg")
        joined = " | ".join(errs)
        self.assertIn(self.QUERY, joined,
                      "the error must name the missing arg value: %s"
                      % joined)

    def test_missing_echo_named(self):
        tmp = tempfile.mkdtemp(prefix="rt-s3d-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        errs = self._gate(self._case(echo_ok=False), tmp, "echo")
        joined = " | ".join(errs)
        self.assertIn(self.ECHO, joined,
                      "the error must name the missing echo: %s"
                      % joined)

    def test_missing_done_reply_named(self):
        tmp = tempfile.mkdtemp(prefix="rt-s3e-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        errs = self._gate(self._case(done_ok=False), tmp, "done")
        joined = " | ".join(errs)
        self.assertIn(self.DONE, joined,
                      "the error must name the missing done line: %s"
                      % joined)

    def test_in_tree_slim_cases_no_drift(self):
        """The slim drop-in set was authored against the D-4c
        contract — every in-tree slim case must validate with zero
        drift errors."""
        for path in sorted(run.globmod.glob(
                os.path.join(run.CASES_DIR, "t21-*.json"))):
            errs = run.validate_case_file(path)
            self.assertEqual(errs, [], "%s: %s"
                             % (os.path.basename(path), errs))


class EnvContractTest(unittest.TestCase):
    """T-E1 (apex-ayl.22 D-7): the env contract — build_env unsets the
    union of the runner L1 PROVIDER_VARS_UNSET and the case's declared
    provider_vars_unset; a SUBSET declaration is rejected by the
    NEW-case gate; the temp hermetic home is generated fresh (0700,
    config + auth stub). Pre-fix RED: build_env has no extra_unset."""

    def test_build_env_union(self):
        saved = {}
        for k in run.PROVIDER_VARS_UNSET + ["RT_E1_EXTRA_KEY"]:
            saved[k] = os.environ.get(k)
            os.environ[k] = "leak-check"
        try:
            env = run.build_env("/nonexistent-home",
                                extra_unset=["RT_E1_EXTRA_KEY"])
        finally:
            for k, v in saved.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
        for k in run.PROVIDER_VARS_UNSET:
            self.assertNotIn(k, env, "%s must be unset" % k)
        self.assertNotIn("RT_E1_EXTRA_KEY", env,
                         "the case-declared extra must be unset too")
        self.assertEqual(env.get("GROK_AUTH_EXPIRED"), "1")

    def test_subset_declaration_rejected(self):
        tmp = tempfile.mkdtemp(prefix="rt-e1-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        c = _base_new_case()
        c["env"] = {"provider_vars_unset": ["OPENAI_API_KEY"]}
        p = _write_case(tmp, "rt-e1-subset.json", c)
        errs = run.validate_case_file(p)
        self.assertTrue(any("provider_vars_unset" in e for e in errs),
                        "a subset declaration must be rejected, got: %s"
                        % errs)

    def test_hermetic_home_generated(self):
        tmp = tempfile.mkdtemp(prefix="rt-e1h-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        live = _write_fake_live_home(tmp, "http://127.0.0.1:9")
        home = run.HermeticHome(os.path.join(tmp, "root"), live, {}, None)
        self.assertEqual(os.stat(home.home).st_mode & 0o777, 0o700)
        self.assertTrue(os.path.isfile(
            os.path.join(home.home, "config.toml")))
        self.assertTrue(os.path.isfile(
            os.path.join(home.home, "proxy-auth-stub.sh")))


class CcCountTest(unittest.TestCase):
    """T-N1 (apex-ayl.22 D-9/G1): a /v1/chat/completions exchange
    counts as a model call. NOTE: the G1 fix landed in W2 (T-V1's
    test_07 requires it), so this RED could not be reproduced —
    recorded as a deviation in the impl report; the pin is still the
    contract."""

    def test_cc_counts_as_model_call(self):
        tmp = tempfile.mkdtemp(prefix="rt-n1-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        wire = _vc_capture(tmp, "chat_completions",
                           _vc_bodies(True, True, True, True,
                                      "strip_model_bound_state"))
        self.assertEqual(run.count_wire_model_calls(wire), 5)


class ThreeWayAuthorityTest(unittest.TestCase):
    """T-N2 (apex-ayl.22 D-9/G2): the 3-way config authority in
    run.py — [model."<id>"] api_backend per model, fallback
    [endpoints] default_api_backend, fallback "responses".
    SCOPE NOTE: the SDD's G2 file (gen-matrix.py) is the .21 lane's
    (coordinator ownership; .21's in-flight copy keeps its interim
    prefix heuristic) — .22 lands the resolver in run.py, tested here
    against fixture configs. Pre-fix RED: the resolvers do not
    exist."""

    CFG = ('[endpoints]\n'
           'default_api_backend = "responses"\n'
           '\n'
           '[model."gpt-5.2-codex"]\n'
           'api_backend = "chat_completions"\n'
           '\n'
           '[model."claude-sonnet-5"]\n'
           'api_backend = "messages"\n')

    def test_backends(self):
        self.assertEqual(run.api_backend_for_model(
            self.CFG, "gpt-5.2-codex"), "chat_completions")
        self.assertEqual(run.api_backend_for_model(
            self.CFG, "claude-sonnet-5"), "messages")
        self.assertEqual(run.api_backend_for_model(
            self.CFG, "qwen3.8-27b"), "responses",
            "absent model -> [endpoints] default_api_backend")

    def test_paths(self):
        self.assertEqual(run.wire_path_for_model(
            self.CFG, "gpt-5.2-codex"), "/v1/chat/completions")
        self.assertEqual(run.wire_path_for_model(
            self.CFG, "claude-sonnet-5"), "/v1/messages")
        self.assertEqual(run.wire_path_for_model(
            self.CFG, "qwen3.8-27b"), "/v1/responses")

    def test_unquoted_section_and_empty(self):
        cfg = '[model.foo]\napi_backend = "messages"\n'
        self.assertEqual(run.api_backend_for_model(cfg, "foo"),
                         "messages")
        self.assertEqual(run.api_backend_for_model("", "x"),
                         "responses")


class ConfigPatchSurgeryTest(unittest.TestCase):
    """SWEEPFIX-64 (F-COMP, RCA sweep1find64): _apply_config_patch's
    append branch must insert the new key into the PARENT table —
    immediately before the first subtable header of any kind
    ([a.b.c] or [[a.b.c]]) that follows the section header. Pre-fix
    RED: appending at the raw span end (after [[...]] array-of-tables
    blocks) made TOML bind the key to the LAST [[...]] element, where
    the lenient serde row drops it silently — the 2026-09-17 qwen
    context_window=12000 incident (catalog window 262144 won, no
    compaction). In-place replacement and the no-subtable EOF append
    are regression pins (behavior unchanged)."""

    CFG = ('[other]\n'
           'foo = 1\n'
           '\n'
           '[model.x]\n'
           'api_backend = "responses"\n'
           'max_completion_tokens = 4096\n'
           '\n'
           '[[model.x.reasoning_efforts]]\n'
           'value = "low"\n'
           'label = "Low Effort"\n'
           '\n'
           '[[model.x.reasoning_efforts]]\n'
           'value = "high"\n'
           'label = "High Effort"\n'
           '\n'
           '[model.y]\n'
           'api_backend = "messages"\n')

    @staticmethod
    def _effort_blocks(text):
        """Raw text of each [[model.x.reasoning_efforts]] block, in
        file order (byte-exact comparison)."""
        blocks, cur = [], None
        for ln in text.splitlines(keepends=True):
            if ln.startswith("[[model.x.reasoning_efforts]]"):
                if cur is not None:
                    blocks.append("".join(cur))
                cur = [ln]
            elif ln.startswith("[") and cur is not None:
                blocks.append("".join(cur))
                cur = None
            elif cur is not None:
                cur.append(ln)
        if cur is not None:
            blocks.append("".join(cur))
        return blocks

    def test_append_binds_parent_with_subtables(self):
        out = run._apply_config_patch(
            self.CFG, {"model/x/context_window": 12000})
        doc = tomllib.loads(out)
        self.assertEqual(doc["model"]["x"]["context_window"], 12000,
                         "the appended key must bind to the model row, "
                         "not to a [[...]] array element")
        effs = doc["model"]["x"]["reasoning_efforts"]
        self.assertEqual(len(effs), 2)
        for e in effs:
            self.assertNotIn("context_window", e,
                             "no reasoning_efforts element may carry "
                             "the appended key")
        self.assertEqual(self._effort_blocks(self.CFG),
                         self._effort_blocks(out),
                         "the two [[...]] blocks must be byte-intact")
        lines = out.splitlines()
        i_key = lines.index("context_window = 12000")
        self.assertLess(i_key,
                        lines.index("[[model.x.reasoning_efforts]]"),
                        "the key must land BEFORE the first subtable "
                        "header")
        self.assertGreater(i_key, lines.index(
            "max_completion_tokens = 4096"),
            "the key must land after the parent's own direct keys")
        self.assertEqual(doc["other"]["foo"], 1)
        self.assertEqual(doc["model"]["y"]["api_backend"], "messages")

    def test_in_place_replace_position_preserved(self):
        cfg = ('[model.x]\n'
               'api_backend = "responses"\n'
               'context_window = 262144\n'
               'max_completion_tokens = 4096\n')
        out = run._apply_config_patch(cfg,
                                      {"model/x/context_window": 12000})
        self.assertEqual(out.splitlines().index("context_window = 12000"),
                         cfg.splitlines().index("context_window = 262144"),
                         "in-place replace must keep the assignment at "
                         "its original line")
        doc = tomllib.loads(out)
        self.assertEqual(doc["model"]["x"]["context_window"], 12000)
        self.assertEqual(doc["model"]["x"]["api_backend"], "responses")
        self.assertEqual(doc["model"]["x"]["max_completion_tokens"], 4096)

    def test_append_section_without_subtables(self):
        cfg = ('[endpoints]\n'
               'default_api_backend = "responses"\n'
               '\n'
               '[model.x]\n'
               'api_backend = "responses"\n')
        out = run._apply_config_patch(cfg,
                                      {"model/x/context_window": 12000})
        doc = tomllib.loads(out)
        self.assertEqual(doc["model"]["x"]["context_window"], 12000)
        lines = out.splitlines()
        self.assertEqual(lines.index("context_window = 12000"),
                         lines.index('api_backend = "responses"') + 1,
                         "no-subtable section: EOF behavior unchanged "
                         "(appended after the last direct line)")
        self.assertEqual(doc["endpoints"]["default_api_backend"],
                         "responses")


class RowModelDerivationTest(unittest.TestCase):
    """SWEEPFIX-64 (F-CAT): run._row_model_for_spec — the row
    identity of a row_asserts wire spec. The frozen case.schema.json
    wire_assert allows no row_model key (additionalProperties: false),
    so schema-compliant rows cases scope each spec through the where
    filter's body.model value; the legacy row_model key must keep
    working (old generated files) and non-row specs stay unscoped."""

    def test_legacy_row_model_key_wins(self):
        spec = {"kind": "field", "file": "req-*.json",
                "row_model": "m-legacy",
                "where": {"method": "POST",
                          "$.body.model": "m-filter"}}
        self.assertEqual(run._row_model_for_spec(spec), "m-legacy",
                         "the legacy row_model key takes precedence")

    def test_dollar_prefixed_body_model(self):
        spec = {"kind": "field", "file": "req-*.json",
                "where": {"method": "POST",
                          "$.body.model": "m-dotted"}}
        self.assertEqual(run._row_model_for_spec(spec), "m-dotted",
                         "the frozen-schema form: identity rides the "
                         "where filter ($.body.model)")

    def test_bare_body_model(self):
        spec = {"kind": "field", "file": "req-*.json",
                "where": {"method": "POST", "body.model": "m-bare"}}
        self.assertEqual(run._row_model_for_spec(spec), "m-bare",
                         "the schema accepts the bare dotted form too")

    def test_non_row_spec_unscoped(self):
        self.assertIsNone(run._row_model_for_spec(
            {"kind": "field", "file": "req-*.json",
             "where": {"method": "POST"}, "path": "$.body.model"}),
            "a spec without a row identity stays unscoped")
        self.assertIsNone(run._row_model_for_spec(
            {"kind": "recon", "file": "req-*.json"}))


class ReconAnyFirstTest(unittest.TestCase):
    """T-R3 (apex-ayl.22 D-12/G15): check_recon any/first over a
    multi-match file set — any = the newest mtime (paths[0] under the
    prefer_last sort), first = the oldest (paths[-1]); recon NEVER
    fails a case (ok=True always, empty match set included).
    Pre-fix RED: check_recon has no any/first handling."""

    def _write(self, tmp, name, v, mtime):
        p = os.path.join(tmp, name)
        with open(p, "w") as fh:
            fh.write(json.dumps({"v": v}))
        os.utime(p, (mtime, mtime))
        return p

    def test_any_newest_first_oldest(self):
        tmp = tempfile.mkdtemp(prefix="rt-r3-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        self._write(tmp, "wire-a.json", "old", 1000)
        self._write(tmp, "wire-b.json", "mid", 2000)
        self._write(tmp, "wire-c.json", "new", 3000)
        ctx = run.CaseCtx()
        ctx.session_dir = tmp
        r = run.check_recon({"id": "rec_a", "file": "wire-*.json",
                             "field": "$.v", "any": True}, ctx)
        self.assertTrue(r.ok, "recon never fails")
        self.assertIn("new", r.detail,
                      "any = the newest mtime (got: %s)" % r.detail)
        r = run.check_recon({"id": "rec_f", "file": "wire-*.json",
                             "field": "$.v", "first": True}, ctx)
        self.assertTrue(r.ok)
        self.assertIn("old", r.detail,
                      "first = the oldest mtime (got: %s)" % r.detail)

    def test_recon_never_fails_empty(self):
        tmp = tempfile.mkdtemp(prefix="rt-r3e-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        ctx = run.CaseCtx()
        ctx.session_dir = tmp
        r = run.check_recon({"id": "rec_e", "file": "nope-*.json",
                             "field": "$.v", "any": True}, ctx)
        self.assertTrue(r.ok, "recon must never fail the case")


class CaseCwdTest(unittest.TestCase):
    """T-C1 (apex-ayl.22 D-10/G4): the binary is launched with cwd =
    the case cwd (absolute), not the hermetic home cwd. A fake binary
    records $PWD into a recorder path baked into the script itself. (T3
    adaptation: it used to name that path through env CWD_RECORDER, which
    the §4.3.4 child allowlist no longer forwards — the recorded value and
    every assertion below are unchanged.) Pre-fix RED: the hardcoded
    home.cwd is recorded instead."""

    def test_popen_cwd_is_case_cwd(self):
        tmp = tempfile.mkdtemp(prefix="rt-c1-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        srv, upstream = _start_fake_upstream()
        self.addCleanup(srv.shutdown)
        # T3 §4.3.3 fixture adaptation (see _run_retry_case).
        _t2_env_guard(self)
        live = _write_fake_live_home(tmp, upstream)
        rec = os.path.join(tmp, "recorded-cwd.txt")
        binp = os.path.join(tmp, "cwd-recorder")
        assert "'" not in rec, "the recorder path must be quotable"
        with open(binp, "w") as fh:
            fh.write("#!/bin/sh\npwd > '%s'\n" % rec)
        os.chmod(binp, 0o755)
        workdir = os.path.join(tmp, "workdir")
        os.makedirs(workdir)
        out = os.path.join(tmp, "report")
        os.makedirs(out)
        case = {"id": "RT-C1", "title": "cwd", "driver": "headless",
                "model": "m-x", "cwd": workdir,
                "steps": [{"op": "turn", "prompt": "hi"}],
                "wirecap": True, "est_calls": 1}
        args = argparse.Namespace(out=out, live_home=live,
                                  wirecap=True, budget=10,
                                  ambient_key="", bin=binp,
                                  rows=None, keep_home=False,
                                  campaign_id=None)
        row = run.run_case(case, args, run.Budget(args.budget))
        self.assertEqual(row["status"], "PASS")
        self.assertTrue(os.path.isfile(rec))
        # macOS: tempfile's /var/... is a symlink to /private/var/...
        # and the shell's `pwd` reports the physical path — compare
        # realpath so the pin is the cwd identity, not the symlink
        # spelling.
        recorded = open(rec).read().strip()
        self.assertEqual(os.path.realpath(recorded),
                         os.path.realpath(workdir),
                         "the Popen cwd must be the case cwd")

    def test_relative_cwd_blocked(self):
        """A legacy case with a NON-absolute cwd fail-closes to
        BLOCKED (the NEW-case gate rejects it at validation; the
        runtime guards the legacy path)."""
        tmp = tempfile.mkdtemp(prefix="rt-c1r-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        srv, upstream = _start_fake_upstream()
        self.addCleanup(srv.shutdown)
        live = _write_fake_live_home(tmp, upstream)
        binp = os.path.join(tmp, "ok-bin")
        with open(binp, "w") as fh:
            fh.write("#!/bin/sh\nexit 0\n")
        os.chmod(binp, 0o755)
        out = os.path.join(tmp, "report")
        os.makedirs(out)
        case = {"id": "RT-C1R", "title": "relative cwd",
                "driver": "headless", "model": "m-x",
                "cwd": "relative/path",
                "steps": [{"op": "turn", "prompt": "hi"}],
                "wirecap": False, "est_calls": 1}
        args = argparse.Namespace(out=out, live_home=live,
                                  wirecap=False, budget=10,
                                  ambient_key="", bin=binp,
                                  rows=None, keep_home=False,
                                  campaign_id=None)
        row = run.run_case(case, args, run.Budget(args.budget))
        self.assertEqual(row["status"], "BLOCKED")


class NoWatchTest(unittest.TestCase):
    """T-C2 (apex-ayl.22 D-10/G11): the hermetic
    [mcp_servers.codegraph] args carry --no-watch — the invariant
    line-edit applied AFTER the case patch (the codegraph server must
    not spawn a watcher into the worktree's .codegraph/); a case patch
    touching the section cannot remove it. Pre-fix RED: the copied
    args stay verbatim (no --no-watch)."""

    def _live_with_codegraph(self, tmp):
        live = os.path.join(tmp, "live")
        os.makedirs(live)
        stub = os.path.join(live, "proxy-auth-stub.sh")
        with open(stub, "w") as fh:
            fh.write("#!/bin/sh\nexit 0\n")
        os.chmod(stub, 0o755)
        cfg = ('base_url = "http://127.0.0.1:9/v1"\n'
               'models_base_url = "http://127.0.0.1:9/v1"\n'
               'auth_stub = "%s"\n'
               "\n[mcp_servers.codegraph]\n"
               'command = "codegraph"\n'
               "args = [\n"
               ' "serve",\n'
               ' "--mcp",\n'
               "]\n" % stub)
        with open(os.path.join(live, "config.toml"), "w") as fh:
            fh.write(cfg)
        return live

    def _codegraph_section(self, cfg_text):
        m = re.search(r"\[mcp_servers\.codegraph\](.*?)(\n\[|\Z)",
                      cfg_text, re.S)
        self.assertIsNotNone(m, "codegraph section missing")
        return m.group(1)

    def test_no_watch_present(self):
        tmp = tempfile.mkdtemp(prefix="rt-c2-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        live = self._live_with_codegraph(tmp)
        home = run.HermeticHome(os.path.join(tmp, "root"), live, {}, None)
        cfg = open(os.path.join(home.home, "config.toml")).read()
        self.assertIn('"--no-watch"',
                      self._codegraph_section(cfg),
                      "the copied codegraph args must carry --no-watch")
        self.assertTrue(getattr(home, "no_watch_applied", False),
                        "no_watch_applied must be recorded for the "
                        "report disclosure")

    def test_not_removable_by_case_patch(self):
        tmp = tempfile.mkdtemp(prefix="rt-c2p-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        live = self._live_with_codegraph(tmp)
        home = run.HermeticHome(os.path.join(tmp, "root"), live,
                                {"mcp_servers/codegraph/command":
                                 "codegraph-renamed"}, None)
        cfg = open(os.path.join(home.home, "config.toml")).read()
        section = self._codegraph_section(cfg)
        self.assertIn('"codegraph-renamed"', section)
        self.assertIn('"--no-watch"', section,
                      "--no-watch must survive a case patch on the "
                      "section (applied AFTER the patch)")


class SessionEvidenceTest(unittest.TestCase):
    """T-S4 (apex-ayl.22 D-8): session-evidence completeness — the
    hermetic home logs (unified.jsonl) and run_dir/acp.log are copied
    into <run>/<case>/session/ by _copy_session_evidence (the copy
    behavior pinned on a synthetic layout). Pre-fix RED: unified.jsonl
    is not in the keep list and acp.log is not copied."""

    def test_unified_and_acp_copied(self):
        tmp = tempfile.mkdtemp(prefix="rt-s4-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        run_dir = os.path.join(tmp, "rt-s4-case")
        os.makedirs(run_dir)
        session_src = os.path.join(tmp, "hermetic-session")
        os.makedirs(session_src)
        with open(os.path.join(session_src, "unified.jsonl"), "w") as fh:
            fh.write('{"type":"x"}\n')
        with open(os.path.join(run_dir, "acp.log"), "w") as fh:
            fh.write("SEND: {}\n")
        ctx = run.CaseCtx()
        ctx.session_dir = session_src
        run._copy_session_evidence(object(), ctx, run_dir)
        dst = os.path.join(run_dir, "session")
        self.assertTrue(os.path.isfile(
            os.path.join(dst, "unified.jsonl")),
            "unified.jsonl must be copied into session/")
        self.assertTrue(os.path.isfile(os.path.join(dst, "acp.log")),
                        "run_dir/acp.log must be copied into session/")


class CampaignTest(unittest.TestCase):
    """T-A1 (apex-ayl.22 D-14): campaign seal (byte-exact runbook
    copy + stable full-64-hex sha256 + atomic campaign.json),
    verdict.json atomicity (a failed write leaves NO partial file),
    summary outcome+stability derivation (incl. FLAKY from a
    retried-pass cell and TIMEOUT from a watchdog-killed cell).
    WAVE NOTE: the SDD §3.3 wave lists omit T-A1 (15/16 listed); it is
    placed in W4 RED->GREEN — no deviation from the 16-T RED-first
    contract (recorded in the report). Pre-fix RED: seal_campaign /
    validate_campaign / aggregate_summary do not exist."""

    def _runbook(self, tmp):
        p = os.path.join(tmp, "runbook.md")
        with open(p, "w") as fh:
            fh.write("# runbook\nsealed content\n")
        return p

    def _meta(self):
        return {"change_id": "47b",
                "git_head": "dec6b27",
                "bin_sha256_12": "912a5a2f2a5f",
                "key_sha256_12": "aaaa1111bbbb",
                "upstream": "https://llm-proxy.example",
                "catalog_snapshot": {
                    "fetched_at": "2026-09-17T00:00:00Z",
                    "sha256": "1fffa296bca6" + "0" * 56,
                    "row_count": 71},
                "config_sha256": "c" * 64,
                "protocol_constants": {"schema_version": 1}}

    def _seal(self, tmp, tag):
        cam = os.path.join(tmp, tag)
        s = run.seal_campaign(cam, "20260917T050000Z",
                              self._runbook(tmp), ["CA", "CB"],
                              mode="slim", meta=self._meta(),
                              started_utc="2026-09-17T05:00:00Z")
        return cam, s

    def test_seal_stable_sha_and_runbook_copy(self):
        tmp = tempfile.mkdtemp(prefix="rt-a1-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        cam1, s1 = self._seal(tmp, "c1")
        cam2, s2 = self._seal(tmp, "c2")
        self.assertEqual(s1, s2, "the sealed sha must be stable")
        self.assertRegex(s1, r"^[0-9a-f]{64}$")
        with open(os.path.join(cam1, "campaign.json")) as fh:
            c = json.load(fh)
        self.assertEqual(c["schema_version"], 1)
        self.assertEqual(c["campaign_id"], "20260917T050000Z")
        self.assertEqual(c["mode"], "slim")
        self.assertEqual(c["expected_cases"], ["CA", "CB"])
        self.assertRegex(c["runbook_sha256"], r"^[0-9a-f]{64}$")
        self.assertEqual(c["sealed_sha256"], s1)
        self.assertFalse(os.path.exists(os.path.join(
            cam1, "campaign.json.tmp")))
        with open(self._runbook(tmp), "rb") as fh:
            rb_bytes = fh.read()
        with open(os.path.join(cam1, "runbook.md"), "rb") as fh:
            self.assertEqual(fh.read(), rb_bytes,
                             "the runbook copy must be byte-exact")

    def test_verdict_json_atomic_no_partial(self):
        tmp = tempfile.mkdtemp(prefix="rt-a1v-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        case = {"id": "CA", "model": "m-x", "wire": "responses"}
        row = {"id": "CA", "status": "PASS", "duration_s": 1.0,
               "model_calls": 1, "session_id": "sid", "attempt": 1,
               "turns": []}
        ctx = run.CaseCtx()
        ctx.verdict = {"premises": []}
        run.write_verdict_json(tmp, case, row, ctx, [],
                               campaign_id="20260917T050000Z")
        p = os.path.join(tmp, "verdict.json")
        self.assertTrue(os.path.isfile(p))
        with open(p) as fh:
            v = json.load(fh)   # parseable = complete
        self.assertEqual(v["campaign_id"], "20260917T050000Z")
        self.assertFalse(os.path.exists(os.path.join(tmp,
                                                     "verdict.json.tmp")))
        # A failed os.replace leaves NO verdict.json (no partial).
        tmp2 = tempfile.mkdtemp(prefix="rt-a1v2-")
        self.addCleanup(lambda: shutil.rmtree(tmp2, ignore_errors=True))
        orig = run.os.replace

        def _boom(src, dst):
            raise OSError("disk full (simulated)")
        run.os.replace = _boom
        try:
            with self.assertRaises(OSError):
                run.write_verdict_json(tmp2, case, row, ctx, [],
                                       campaign_id="20260917T050000Z")
        finally:
            run.os.replace = orig
        self.assertFalse(
            os.path.exists(os.path.join(tmp2, "verdict.json")),
            "a failed write must leave no verdict.json (partial or "
            "otherwise)")

    def _verdict(self, d, cell, case_id, attempt, status, outcome,
                 evidence=True, killed=False, stability=None):
        doc = {"schema_version": 1,
               "campaign_id": "20260917T050000Z",
               "cell_id": cell, "case_id": case_id,
               "attempt": attempt, "status": status,
               "outcome": outcome, "stability": stability,
               "model": "m-x", "wire": "responses",
               "session_id": "sid", "duration_s": 1.0,
               "model_calls": 1, "est_calls": 1,
               "retry_count": attempt - 1, "killed": killed,
               "verdict": {"premises": []},
               "evidence_index": (
                   [{"file": "req-001.json", "request_n": 1,
                     "model": "m-x", "byte_range": [0, 10]}]
                   if evidence else []),
               "triage": None, "ts": "2026-09-17T05:00:01Z"}
        with open(os.path.join(d, "verdict.json"), "w") as fh:
            json.dump(doc, fh)

    def test_summary_outcome_stability(self):
        tmp = tempfile.mkdtemp(prefix="rt-a1s-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        cam, _ = self._seal(tmp, "c1")
        # Cell CA: retried-pass — attempt-1 FAIL at the root,
        # attempt-2 PASS under attempt-2/ (stability FLAKY).
        cell_a = os.path.join(cam, "ca")
        os.makedirs(cell_a)
        self._verdict(cell_a, "CA", "CA", 1, "FAIL", "FAIL")
        attempt2 = os.path.join(cell_a, "attempt-2")
        os.makedirs(attempt2)
        self._verdict(attempt2, "CA", "CA", 2, "PASS", "PASS",
                      stability="FLAKY")
        # Cell CB: clean PASS.
        cell_b = os.path.join(cam, "cb")
        os.makedirs(cell_b)
        self._verdict(cell_b, "CB", "CB", 1, "PASS", "PASS")
        self.assertEqual(run.validate_campaign(cam), [],
                         "a well-formed campaign must validate clean")
        summary = run.aggregate_summary(cam)
        self.assertEqual(summary["outcome"], "PASS")
        self.assertEqual(summary["stability"], "FLAKY",
                         "a retried-pass cell makes the campaign "
                         "FLAKY")
        self.assertIn("CA", summary["flaky"])
        self.assertTrue(os.path.isfile(os.path.join(cam,
                                                    "summary.json")))
        self.assertTrue(os.path.isfile(os.path.join(cam,
                                                    "summary.md")))
        # A BLOCKED cell flips the outcome to BLOCKED.
        cell_c = os.path.join(cam, "cc")
        os.makedirs(cell_c)
        self._verdict(cell_c, "CC", "CC", 1, "BLOCKED", "BLOCKED",
                      evidence=False)
        summary = run.aggregate_summary(cam)
        self.assertEqual(summary["outcome"], "BLOCKED")
        self.assertIn("CC", summary["blocked"])
        # A watchdog-killed clean cell -> TIMEOUT.
        cell_d = os.path.join(cam, "cd")
        os.makedirs(cell_d)
        self._verdict(cell_d, "CD", "CD", 1, "PASS", "PASS",
                      killed=True)
        summary = run.aggregate_summary(cam)
        self.assertEqual(summary["outcome"], "BLOCKED")  # CC still blocks
        cd = [c for c in summary["cells"] if c["cell_id"] == "CD"][0]
        self.assertEqual(cd["outcome"], "TIMEOUT")

    def test_validate_rejections(self):
        tmp = tempfile.mkdtemp(prefix="rt-a1r-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        cam, _ = self._seal(tmp, "c1")
        cell = os.path.join(cam, "ca")
        os.makedirs(cell)
        # Malformed result JSON.
        with open(os.path.join(cell, "verdict.json"), "w") as fh:
            fh.write("{not json")
        rej = run.validate_campaign(cam)
        self.assertTrue(any("malformed" in r for r in rej), rej)
        os.remove(os.path.join(cell, "verdict.json"))
        # Duplicate cell/attempt identity.
        self._verdict(cell, "CA", "CA", 1, "PASS", "PASS")
        dup = os.path.join(cell, "dup-attempt-1")
        os.makedirs(dup)
        self._verdict(dup, "CA", "CA", 1, "PASS", "PASS")
        rej = run.validate_campaign(cam)
        self.assertTrue(any("duplicate" in r for r in rej), rej)
        shutil.rmtree(dup)
        # Stale artifact: campaign_id mismatch.
        p = os.path.join(cell, "verdict.json")
        v = json.load(open(p))
        v["campaign_id"] = "99999999T999999Z"
        with open(p, "w") as fh:
            json.dump(v, fh)
        rej = run.validate_campaign(cam)
        self.assertTrue(any("campaign_id" in r and
                            ("stale" in r or "mismatch" in r)
                            for r in rej), rej)
        v["campaign_id"] = "20260917T050000Z"
        with open(p, "w") as fh:
            json.dump(v, fh)
        # Identity drift: case_id not in expected_cases.
        v["case_id"] = "GHOST"
        with open(p, "w") as fh:
            json.dump(v, fh)
        rej = run.validate_campaign(cam)
        self.assertTrue(any("identity" in r or "expected_cases" in r
                            for r in rej), rej)
        v["case_id"] = "CA"
        with open(p, "w") as fh:
            json.dump(v, fh)
        # PASS with incomplete evidence.
        v["evidence_index"] = []
        with open(p, "w") as fh:
            json.dump(v, fh)
        rej = run.validate_campaign(cam)
        self.assertTrue(any("incomplete" in r or "evidence" in r
                            for r in rej), rej)
        v["evidence_index"] = [{"file": "req-001.json"}]
        with open(p, "w") as fh:
            json.dump(v, fh)
        # N5 (apex-ayl.91): the census is complete only when every sealed
        # case has a persisted verdict — the fixture mirrors a complete
        # campaign (CB previously had no verdict and was silently absent).
        os.makedirs(os.path.join(cam, "cb"), exist_ok=True)
        self._verdict(os.path.join(cam, "cb"), "CB", "CB", 1, "PASS", "PASS")
        self.assertEqual(run.validate_campaign(cam), [])
        # Retry overwrote earlier evidence: attempt-2 without attempt-1.
        p2 = os.path.join(cam, "cb2")
        os.makedirs(os.path.join(p2, "attempt-2"))
        self._verdict(os.path.join(p2, "attempt-2"), "CB2", "CB2", 2,
                      "PASS", "PASS")
        rej = run.validate_campaign(cam)
        self.assertTrue(any("overwrote" in r or "earlier" in r
                            for r in rej), rej)


class BinResolutionTest(unittest.TestCase):
    """F-BIN (Wave-S 20260917T102912Z): a relative --bin must resolve even
    when a case runs under the hermetic-home cwd (D-10), else Popen gets
    ENOENT and the cell 0-call FAILs."""

    def test_absolute_passthrough(self):
        self.assertEqual(run.resolve_bin_path("/abs/bin/grok-responses"),
                         "/abs/bin/grok-responses")

    def test_relative_resolves_against_repo_root(self):
        got = run.resolve_bin_path(os.path.join("target", "release",
                                                "grok-responses"))
        self.assertEqual(got, os.path.join(run.REPO_ROOT, "target",
                                           "release", "grok-responses"))
        self.assertTrue(os.path.isabs(got))


class CaseOrderTest(unittest.TestCase):
    """F-ORD (Wave-S 20260917T102912Z): load_cases must preserve the
    positional roster order (the set filter ran glob-sorted order)."""

    def _make_cases_dir(self, ids):
        d = tempfile.mkdtemp(prefix="caseorder-")
        for i, cid in enumerate(ids):
            with open(os.path.join(d, "%02d-%s.json" % (i, cid.lower())),
                      "w") as fh:
                json.dump({"id": cid}, fh)
        return d

    def setUp(self):
        self._orig_cases_dir = run.CASES_DIR
        self._tmp = []

    def tearDown(self):
        run.CASES_DIR = self._orig_cases_dir
        for d in self._tmp:
            shutil.rmtree(d, ignore_errors=True)

    def _load(self, case_ids):
        return run.load_cases(types.SimpleNamespace(case_ids=case_ids))

    def test_positional_order_preserved(self):
        d = self._make_cases_dir(["ZULU", "ALPHA", "MIKE"])
        self._tmp.append(d)
        run.CASES_DIR = d
        got = [c["id"] for c in self._load(["MIKE", "ZULU", "ALPHA"])]
        self.assertEqual(got, ["MIKE", "ZULU", "ALPHA"])

    def test_unknown_id_skipped_order_held(self):
        d = self._make_cases_dir(["ALPHA", "MIKE"])
        self._tmp.append(d)
        run.CASES_DIR = d
        got = [c["id"] for c in self._load(["GHOST", "MIKE", "ALPHA"])]
        self.assertEqual(got, ["MIKE", "ALPHA"])


class CampaignVerdictNitsTest(unittest.TestCase):
    """apex-ayl.91 N1/N4/N5 (20260919T084258Z bookkeeping campaign):
    SKIP rows persist a verdict (outcome SKIP), the validator stops
    false-rejecting RECON/0-call mechanism rows as "incomplete
    evidence", and it now catches missing verdicts and seal tampering."""

    def _tmpdir(self):
        return tempfile.mkdtemp(prefix="nits-")

    @staticmethod
    def _verdict(path, cell, status, outcome, calls, evidence):
        with open(path, "w") as fh:
            json.dump({"schema_version": 1, "campaign_id": "nits-test",
                       "cell_id": cell, "case_id": cell, "attempt": 1,
                       "status": status, "outcome": outcome,
                       "model_calls": calls, "evidence_index": evidence,
                       "verdict": {}, "killed": False}, fh)

    def _sealed_campaign(self, d, cells):
        run.seal_campaign(d, "nits-test", None, cells, mode="adhoc")
        for c in cells:
            os.makedirs(os.path.join(d, c.lower()), exist_ok=True)

    def test_skip_row_persists_verdict_outcome_skip(self):
        d = self._tmpdir()
        try:
            case = {"id": "S-1", "est_calls": 3}
            row = run._skip_row(case, "budget: used=5 est=3 limit=5")
            ctx = types.SimpleNamespace(verdict=None, retry_count=0)
            args = types.SimpleNamespace(campaign_id="nits-test")
            run._skip_verdict(d, case, row, ctx, args)
            with open(os.path.join(d, "verdict.json")) as fh:
                doc = json.load(fh)
            self.assertEqual(doc["status"], "SKIP")
            self.assertEqual(doc["outcome"], "SKIP")
            self.assertEqual(doc["model_calls"], 0)
            self.assertEqual(doc["evidence_index"], [])
        finally:
            shutil.rmtree(d, ignore_errors=True)

    def test_validator_rejects_missing_verdict(self):
        d = self._tmpdir()
        try:
            self._sealed_campaign(d, ["A-1", "A-2"])
            self._verdict(os.path.join(d, "a-1", "verdict.json"), "A-1",
                          "PASS", "PASS", 2,
                          [{"file": "req-001.json", "request_n": 1}])
            rejs = run.validate_campaign(d)
            missing = [r for r in rejs if "no verdict persisted" in r]
            self.assertTrue(missing, rejs)
            self.assertIn("A-2", missing[0])
        finally:
            shutil.rmtree(d, ignore_errors=True)

    def test_validator_rejects_broken_seal(self):
        d = self._tmpdir()
        try:
            self._sealed_campaign(d, ["A-1"])
            man_path = os.path.join(d, "campaign.json")
            with open(man_path) as fh:
                man = json.load(fh)
            man["expected_cases"].append("TAMPERED")
            with open(man_path, "w") as fh:
                json.dump(man, fh)
            rejs = run.validate_campaign(d)
            self.assertTrue(any("seal broken" in r for r in rejs), rejs)
        finally:
            shutil.rmtree(d, ignore_errors=True)

    def test_validator_no_false_reject_recon_or_zero_call(self):
        d = self._tmpdir()
        try:
            self._sealed_campaign(d, ["R-1", "Z-1"])
            self._verdict(os.path.join(d, "r-1", "verdict.json"), "R-1",
                          "RECON", "PASS", 2, [])
            self._verdict(os.path.join(d, "z-1", "verdict.json"), "Z-1",
                          "PASS", "PASS", 0, [])
            rejs = run.validate_campaign(d)
            self.assertFalse(any("incomplete evidence" in r for r in rejs),
                             rejs)
        finally:
            shutil.rmtree(d, ignore_errors=True)

    def test_validator_still_rejects_pass_without_evidence(self):
        d = self._tmpdir()
        try:
            self._sealed_campaign(d, ["P-1"])
            self._verdict(os.path.join(d, "p-1", "verdict.json"), "P-1",
                          "PASS", "PASS", 3, [])
            rejs = run.validate_campaign(d)
            self.assertTrue(any("incomplete evidence" in r for r in rejs),
                             rejs)
        finally:
            shutil.rmtree(d, ignore_errors=True)


# ---------------------------------------------------------------------------
# REDTEAM-REFRESH T2 (bead apex-mwga) — per-row upstream routing keyed on the
# WIRE MODEL ID, fail-closed. r3 §4.2 + §4.6 as amended by r3a A2/A3/A12,
# r3b B1/B4 and r3c seat-1 F3/F4/F5.
#
# Offline only (r3a A12): the fake upstream below stays the single transport;
# no test here opens a non-loopback socket. Every fixture row declares an
# env_key naming a TEST-ONLY variable whose value this file sets to a synthetic
# string under the env guard (r3c seat-1 F4) — no fallback-chain name
# (XAI_API_KEY / GROK_CODE_XAI_API_KEY / APEX_LLM_PROXY_KEY) is ever read.
#
# T2 owns routing, the fail-closed path and credential stripping on the REFUSED
# path. Credential replacement, masking, TLS and the child env are T3 (r3 §8).
# ---------------------------------------------------------------------------

T2_TEST_ONLY_KEYS = ("WIRETAP_T2_KEY_X", "WIRETAP_T2_KEY_B",
                     "WIRETAP_T2_KEY_HDR", "WIRETAP_T2_KEY_AMB",
                     "WIRETAP_T2_KEY_AMB1M")


def _t2_env_plant(name):
    """The value `_t2_env_guard` installs for one test-only name. One source of
    the spelling, so a test that has to name the plant (RR-10 F-8) cannot spell
    it differently from the guard — the RR-F5 lesson about a plant that reads
    upper-case and therefore matches nothing."""
    return "synthetic-plant-%s-value" % name.lower()


def _t2_env_guard(test):
    """r3c seat-1 F4: synthetic values for the fixture rows' own env_key
    names, restored on cleanup (the house EnvGuard pattern)."""
    saved = {}
    for k in T2_TEST_ONLY_KEYS:
        saved[k] = os.environ.get(k)
        os.environ[k] = _t2_env_plant(k)

    def restore():
        for k, v in saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
    test.addCleanup(restore)


# Six `base_url` lines (one top-level + five rows), three distinct row origins
# plus the inherited default, two rows sharing one wire id, and every sub-table
# spelling that must NOT be read as a row (r3 §4.2.1 "Derivation": the runner
# keeps its no-dependency line scan — no toml dependency is added).
T2_DERIVE_CFG = '''base_url = "http://default.invalid/v1"
models_base_url = "http://default.invalid/v1"
auth_stub = "/nonexistent/proxy-auth-stub.sh"

[model."r1"]
base_url = "http://originA.invalid/v1"
api_backend = "responses"
env_key = "WIRETAP_T2_KEY_X"

[model."r2"]
model = "wire-b"
base_url = "http://originB.invalid/v1"
api_backend = "responses"
env_key = "WIRETAP_T2_KEY_B"

[model."r2".env_http_headers]
x-api-key = "WIRETAP_T2_KEY_HDR"

[model."r2".extra_headers]
x-litellm-tags = "fixture-tags"

[model."r3"]
base_url = "http://originC.invalid/v1"
api_backend = "responses"

[model."r4"]
api_backend = "responses"
env_key = "WIRETAP_T2_KEY_B"

[model."r5"]
base_url = "http://originB.invalid/v1"
api_backend = "responses"
env_key = "WIRETAP_T2_KEY_B"

[model."r5".reasoning_efforts]
default = "low"

[[model."r5".supported_efforts]]
value = "low"

[model."r6"]
model = "wire-b"
base_url = "http://originB.invalid/v1"
api_backend = "responses"
env_key = "WIRETAP_T2_KEY_B"

[model."r6".env_http_headers]
x-api-key = "WIRETAP_T2_KEY_HDR"

[features]
turn_summary = false
'''

# The live config writes some rows with a BARE key (`[model.gpt-6-luna]`), not
# `[model."gpt-6-luna"]`. Both are the same TOML row; the fixture mirrors the
# live spelling mix so a parser that accepts only one cannot pass.
T2_BARE_KEY_CFG = '''base_url = "http://default.invalid/v1"
models_base_url = "http://default.invalid/v1"
auth_stub = "/nonexistent/proxy-auth-stub.sh"

[model."quoted-row"]
base_url = "http://originA.invalid/v1"
api_backend = "responses"
env_key = "WIRETAP_T2_KEY_X"

[model.bare-row]
base_url = "http://originB.invalid/v1"
api_backend = "responses"
env_key = "WIRETAP_T2_KEY_B"

[model.bare-row.extra_headers]
x-litellm-tags = "fixture-tags"

[model.bare-model]
model = "bare/wire-id"
base_url = "http://originC.invalid/v1"
api_backend = "responses"
env_key = "WIRETAP_T2_KEY_B"

[features]
turn_summary = false
'''


class RowRouteDerivationTest(unittest.TestCase):
    """r3 §4.6 row 1: the per-row origin set must be derived from the config.
    RED on the frozen bytes: run.py has no route derivation at all — every
    base_url is rewritten to the single loopback port and the one upstream is
    live_upstream(models_base_url), so the origin set collapses to 1."""

    def _routes(self):
        default = run._origin_from_base_url("http://default.invalid/v1")
        return {r["row_name"]: r
                for r in run.derive_row_routes(T2_DERIVE_CFG, default)}

    def test_three_origins_plus_inherited_default(self):
        routes = self._routes()
        origins = sorted({r["origin"] for r in routes.values()})
        self.assertEqual(
            origins,
            ["http://default.invalid", "http://originA.invalid",
             "http://originB.invalid", "http://originC.invalid"],
            "3 row origins + the inherited default must be derived; a row "
            "with no base_url maps to the default (r3 §4.2.1)")
        self.assertEqual(routes["r4"]["origin"], "http://default.invalid",
                         "a row with no base_url derives the default origin")

    def test_wire_id_is_model_field_else_row_name(self):
        routes = self._routes()
        self.assertEqual(routes["r2"]["wire_model_id"], "wire-b")
        self.assertEqual(routes["r1"]["wire_model_id"], "r1",
                         "no model field => the row name IS the wire id")

    def test_subtables_are_not_rows(self):
        routes = self._routes()
        self.assertEqual(sorted(routes),
                         ["r1", "r2", "r3", "r4", "r5", "r6"],
                         "extra_headers / env_http_headers / "
                         "reasoning_efforts sub-tables must not become rows")

    def test_bare_key_row_headers_are_rows(self):
        """Defect found by this unit's r3 §8 Checks leg (map keys must equal
        the live wire_model_id set): 3 of the 18 live rows are written with a
        bare TOML key and were silently not rows at all."""
        default = run._origin_from_base_url("http://default.invalid/v1")
        routes = {r["row_name"]: r
                  for r in run.derive_row_routes(T2_BARE_KEY_CFG, default)}
        self.assertEqual(sorted(routes), ["bare-model", "bare-row",
                                          "quoted-row"],
                         "a bare [model.<id>] header names a ROW exactly as "
                         "[model.\"<id>\"] does; its .extra_headers "
                         "sub-table must not")
        self.assertEqual(routes["bare-row"]["origin"],
                         "http://originB.invalid",
                         "a bare-key row keeps its OWN origin — the live "
                         "config writes 3 rows this way")
        self.assertEqual(routes["bare-model"]["wire_model_id"], "bare/wire-id")

    def test_bare_key_and_quoted_rows_share_one_row_space(self):
        default = run._origin_from_base_url("http://default.invalid/v1")
        routes = {r["row_name"]: r
                  for r in run.derive_row_routes(T2_BARE_KEY_CFG, default)}
        origins = sorted({r["origin"] for r in routes.values()})
        self.assertEqual(origins, ["http://originA.invalid",
                                   "http://originB.invalid",
                                   "http://originC.invalid"],
                         "quoted and bare spellings must be one row set, not "
                         "two half-sets")


def _t2_write_cfg_home(tmp, cfg_text):
    """A fake live GROK_HOME whose config is exactly cfg_text (plus the auth
    stub HermeticHome copies)."""
    live = os.path.join(tmp, "live")
    os.makedirs(live, exist_ok=True)
    stub = os.path.join(live, "proxy-auth-stub.sh")
    with open(stub, "w") as fh:
        fh.write("#!/bin/sh\nexit 0\n")
    os.chmod(stub, 0o755)
    with open(os.path.join(live, "config.toml"), "w") as fh:
        fh.write(cfg_text.replace("/nonexistent/proxy-auth-stub.sh", stub))
    return live


class HermeticRoutingMapTest(unittest.TestCase):
    """r3 §4.6 row 2 + r3b B1. RED on the frozen bytes: HermeticHome emits one
    loopback URL and nothing else, so there is no map, no env channel for it,
    and no pin-rule decision at all."""

    def _home(self, tmp, cfg_text, port=59999):
        live = _t2_write_cfg_home(tmp, cfg_text)
        return run.HermeticHome(os.path.join(tmp, "root"), live, {}, port)

    # A route is the triple (origin, credential_variable, header), so the two
    # rows sharing wire id "wire-b" must agree on all three to be ONE route;
    # r6 declares the same env_http_headers insert as r2 for exactly that
    # reason, and the ambiguous fixture (test_ambiguous_pin_route_blocks)
    # differs on the credential alone.
    def test_bare_key_row_is_routed_to_its_own_origin(self):
        """The reachable harm, at the map boundary: an unrouted row is not
        merely absent from the map, it inherits the deployment default origin,
        so a request for an on-prem or TrueFoundry row is forwarded to the
        proxy — r3 §2.4's mismatch, manufactured by our own parser."""
        tmp = tempfile.mkdtemp(prefix="rt-t2-bare-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        home = self._home(tmp, T2_BARE_KEY_CFG)
        route_map, causes = home.route_map(["bare-row", "bare-model"], [])
        self.assertEqual(causes, [])
        self.assertEqual(route_map["bare-row"]["origin"],
                         "http://originB.invalid")
        self.assertEqual(route_map["bare/wire-id"]["origin"],
                         "http://originC.invalid")
        self.assertNotEqual(route_map["bare-row"]["origin"],
                            home.default_origin,
                            "a bare-key row must not inherit the default "
                            "origin")

    def test_map_keyed_on_wire_id_carries_route_triple(self):
        tmp = tempfile.mkdtemp(prefix="rt-t2-map-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        home = self._home(tmp, T2_DERIVE_CFG)
        route_map, causes = home.route_map(["r2"], [])
        self.assertEqual(causes, [])
        self.assertEqual(sorted(route_map), ["wire-b"],
                         "the map is keyed on the WIRE MODEL ID, not the "
                         "row name")
        route = route_map["wire-b"]
        self.assertEqual(route["origin"], "http://originB.invalid")
        self.assertEqual(route["credential_variable"], "WIRETAP_T2_KEY_HDR",
                         "env_http_headers' insert wins (client.rs:2198 -> "
                         ":1743), so that row's own variable is the rung")
        self.assertEqual(route["credential_header"], "x-api-key")
        self.assertEqual(sorted(route["row_names"]), ["r2", "r6"],
                         "both rows sharing the wire id and one route are "
                         "listed behind the single route")

    def test_map_reaches_wiretap_by_env_not_argv(self):
        tmp = tempfile.mkdtemp(prefix="rt-t2-env-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        cap = os.path.join(tmp, "wire")
        os.makedirs(cap)
        route_map = {"wire-b": {"origin": "http://originB.invalid",
                                "credential_variable": "WIRETAP_T2_KEY_HDR",
                                "credential_header": "x-api-key",
                                "row_names": ["r2", "r6"]}}
        wt = run.Wiretap(run.free_port(), "http://127.0.0.1:9", cap, "",
                         route_map=route_map)
        self.addCleanup(wt.stop)
        # RR-10 F-8: one value-free leg for all three rules. `assertNotIn(x,
        # joined, msg)` renders BOTH operands on this interpreter (measured in
        # tmp/F8-assertnotin-render.json), so the leg meant to catch a
        # credential in argv used to publish one; `_t3_assert_absent` names the
        # offender and shows sha256:12 only.
        _t3_assert_absent(
            self, "\n".join(wt.argv),
            [("--route-map", "--route-map"),
             ("credential_variable", "credential_variable")]
            + [(k, _t2_env_plant(k)) for k in T2_TEST_ONLY_KEYS],
            "the wiretap argv (HYG-1: argv is ps-visible, run.py:1274-1277; "
            "the route map reaches the wiretap by env only, r3 §4.2.1)")
        self.assertEqual(json.loads(wt.env["WIRETAP_ROUTE_MAP"]), route_map,
                         "the map travels to the wiretap by environment only")
        self.assertNotIn("synthetic-plant", json.dumps(
            json.loads(wt.env["WIRETAP_ROUTE_MAP"])),
                         "the blob carries variable NAMES, never values")

    # ---- r3b B1: a wire pin adds a row only when its route is unambiguous --

    def test_pin_on_selected_wire_id_adds_nothing(self):
        """B1 rule 1: the case's own selector already resolves a live row with
        that wire id, so the pin adds nothing even though a sibling row shares
        the id on a DIFFERENT route."""
        tmp = tempfile.mkdtemp(prefix="rt-t2-b1a-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        cfg = T2_DERIVE_CFG + (
            '\n[model."r7"]\n'
            'base_url = "http://originA.invalid/v1"\n'
            'api_backend = "responses"\nenv_key = "WIRETAP_T2_KEY_X"\n'
            '\n[model."r7-1m"]\nmodel = "r7"\n'
            'base_url = "http://originC.invalid/v1"\n'
            'api_backend = "responses"\nenv_key = "WIRETAP_T2_KEY_B"\n')
        home = self._home(tmp, cfg)
        route_map, causes = home.route_map(["r7"], ["r7"])
        self.assertEqual(causes, [],
                         "rule 1: an already-selected wire id is not ambiguous")
        self.assertEqual(sorted(route_map), ["r7"])
        self.assertEqual(route_map["r7"]["origin"], "http://originA.invalid",
                         "the selected row's own route stands")

    def test_pin_declared_model_is_routed(self):
        """B1 rule 2: the pin names a wire id no selector resolves; every row
        with that id shares one route, so the pin adds that one route."""
        tmp = tempfile.mkdtemp(prefix="rt-t2-b1b-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        home = self._home(tmp, T2_DERIVE_CFG)
        route_map, causes = home.route_map(["r1"], ["wire-b"])
        self.assertEqual(causes, [])
        self.assertEqual(sorted(route_map), ["r1", "wire-b"])
        self.assertEqual(route_map["wire-b"]["origin"],
                         "http://originB.invalid")

    def test_ambiguous_pin_route_blocks(self):
        """B1 rule 3: rows sharing the pin's wire id sit on different routes,
        so nothing is guessed — the case is BLOCKED before any step, with the
        cause in the row AND in verdict.json (r3c seat-1 F5).

        The fixture deliberately carries a same-wire-id sibling on a DIFFERENT
        origin and credential, so the withdrawn r3a rule ("add every live row
        whose wire_model_id equals it") would route it instead of blocking."""
        tmp = tempfile.mkdtemp(prefix="rt-t2-b1c-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        srv, upstream = _start_recording_upstream()
        self.addCleanup(srv.shutdown)
        cfg = ('base_url = "%s/v1"\nmodels_base_url = "%s/v1"\n'
               '[features]\nturn_summary = false\n'
               '\n[model."row-x"]\nmodel = "m-x"\n'
               'base_url = "%s/v1"\napi_backend = "responses"\n'
               'env_key = "WIRETAP_T2_KEY_X"\n'
               '\n[model."amb"]\nmodel = "m-amb"\nbase_url = "%s/v1"\n'
               'api_backend = "responses"\nenv_key = "WIRETAP_T2_KEY_AMB"\n'
               '\n[model."amb-1m"]\nmodel = "m-amb"\n'
               'api_backend = "responses"\nenv_key = "WIRETAP_T2_KEY_AMB1"\n'
               % (upstream, upstream, upstream, upstream))
        live = _t2_write_cfg_home(tmp, cfg)
        fake_bin = _write_fake_binary(tmp, upstream, model="m-x")
        out = os.path.join(tmp, "report")
        os.makedirs(out)
        case = {"id": "RT-T2-AMB", "title": "ambiguous pin route",
                "driver": "headless", "model": "row-x",
                "steps": [{"op": "turn", "prompt": "ping"}],
                "wirecap": True, "est_calls": 1,
                "assert": {"wire": [{
                    "id": "t2_amb_pin", "kind": "grep",
                    "file": "req-*.json",
                    "where": {"body.model": "m-amb"},
                    "any": True, "grep": '"model": "m-amb"'}]}}
        args = argparse.Namespace(out=out, live_home=live, wirecap=True,
                                  budget=10, ambient_key="", bin=fake_bin,
                                  rows=None, keep_home=False,
                                  campaign_id=None)
        row = run.run_case(case, args, run.Budget(args.budget))
        run_dir = os.path.join(out, "rt-t2-amb")
        want = "ambiguous-pin-route: m-amb"
        self.assertEqual(row["status"], "BLOCKED",
                         "an ambiguous pin route must BLOCK the case")
        self.assertEqual(row.get("blocked_reason"), want,
                         "the row must carry the cause the verdict engine set")
        verdict = json.load(open(os.path.join(run_dir, "verdict.json")))
        self.assertEqual(verdict["status"], "BLOCKED")
        self.assertEqual(verdict["verdict"]["blocked_reason"], want)
        self.assertEqual([r for r in srv.requests if r["method"] == "POST"],
                         [], "the decision is made before any step — nothing "
                             "may be forwarded")


class _RecordingUpstreamHandler(_http_server.BaseHTTPRequestHandler):
    """A fake upstream that RECORDS every request it receives, GETs included.

    r3c seat-1 F3 requires a test that proves the wiretap answers `GET /`
    locally, which is only decidable if the upstream can say whether it ever
    saw one; `run.run_case` + the real wiretap is the binding harness for these
    behaviours (r3a A12), so the recorder has to live on this side.
    """
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        pass

    def _record(self, method):
        ln = int(self.headers.get("content-length") or 0)
        raw = self.rfile.read(ln) if ln else b""
        model = None
        try:
            model = json.loads(raw or b"{}").get("model")
        except Exception:
            pass
        with self.server.lock:
            self.server.requests.append(
                {"method": method, "path": self.path, "model": model,
                 "headers": {k.lower(): v
                             for k, v in self.headers.items()}})
        return raw

    def _send(self, status, payload):
        body = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        raw = self._record("POST")
        try:
            model = json.loads(raw or b"{}").get("model")
        except Exception:
            model = None
        self._send(200, {"ok": True, "model": model})

    def do_GET(self):
        self._record("GET")
        if self.path.startswith("/v1/models"):
            self._send(200, {"data": [{"id": "m-x"}]})
        else:
            self._send(200, {"origin_root": True})


def _start_recording_upstream(handler_class=None):
    """`handler_class` puts a test's own handler behind the same request log
    (T3 needs an echoing one and a 403-answering one). An attribute set on the
    server object afterwards does nothing: socketserver reads
    RequestHandlerClass when it accepts each connection."""
    import http.server
    import threading
    srv = http.server.ThreadingHTTPServer(
        ("127.0.0.1", 0),
        handler_class or _RecordingUpstreamHandler)
    srv.requests = []
    srv.lock = threading.Lock()
    t = threading.Thread(target=srv.serve_forever, daemon=True)
    t.start()
    return srv, "http://127.0.0.1:%d" % srv.server_address[1]


def _t2_case_args(tmp, live, fake_bin, rows=None, wirecap=True):
    out = os.path.join(tmp, "report")
    os.makedirs(out, exist_ok=True)
    return argparse.Namespace(out=out, live_home=live, wirecap=wirecap,
                              budget=20, ambient_key="", bin=fake_bin,
                              rows=rows, keep_home=False, campaign_id=None), out


# Two named rows on the default origin plus one row on a second origin: the
# shape the live config has (proxy row + TrueFoundry row + on-prem row).
T2_TWO_ORIGIN_CFG = ('''base_url = "%(a)s/v1"
models_base_url = "%(a)s/v1"
[features]
turn_summary = false

[model."row-x"]
model = "m-x"
base_url = "%(a)s/v1"
api_backend = "responses"
env_key = "WIRETAP_T2_KEY_X"

[model."row-y"]
model = "m-y"
base_url = "%(a)s/v1"
api_backend = "responses"
env_key = "WIRETAP_T2_KEY_X"

[model."row-tf"]
model = "lxg-sc-stg/wire-tf"
base_url = "%(b)s/v1"
api_backend = "responses"
env_key = "WIRETAP_T2_KEY_B"

[model."row-tf".env_http_headers]
x-api-key = "WIRETAP_T2_KEY_HDR"
''')

# One mapped row; the fixture binary sends an id no selector names.
T2_ORPHAN_CFG = ('''base_url = "%(a)s/v1"
models_base_url = "%(a)s/v1"
[features]
turn_summary = false

[model."row-x"]
model = "m-x"
base_url = "%(a)s/v1"
api_backend = "responses"
env_key = "WIRETAP_T2_KEY_X"

[model."row-y"]
model = "m-y"
base_url = "%(a)s/v1"
api_backend = "responses"
env_key = "WIRETAP_T2_KEY_X"
''')


class RouteResolutionFailClosedTest(unittest.TestCase):
    """r3 §4.6 row 3. Two fake upstreams so the routing choice is observable;
    the second binds AFTER the default so port assignment stays deterministic.
    RED on the frozen bytes: one upstream is the only upstream, so a
    TrueFoundry-class row is forwarded to the default origin."""

    def _fixture(self, cfg, model="m-x", send_auth_env=None):
        tmp = tempfile.mkdtemp(prefix="rt-t2-route-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        srv_a, upstream_a = _start_recording_upstream()
        self.addCleanup(srv_a.shutdown)
        srv_b, upstream_b = _start_recording_upstream()
        self.addCleanup(srv_b.shutdown)
        live = _t2_write_cfg_home(tmp, cfg % {"a": upstream_a,
                                             "b": upstream_b})
        fake_bin = _write_fake_binary(tmp, upstream_a, model=model,
                                      send_auth_env=send_auth_env)
        return tmp, live, fake_bin, srv_a, srv_b

    def test_row_reaches_its_own_origin_not_the_default(self):
        tmp, live, fake_bin, srv_a, srv_b = self._fixture(
            T2_TWO_ORIGIN_CFG, model="lxg-sc-stg/wire-tf")
        args, out = _t2_case_args(tmp, live, fake_bin)
        case = {"id": "RT-T2-ROUTE", "title": "per-row origin",
                "driver": "headless", "model": "row-tf",
                "steps": [{"op": "turn", "prompt": "ping"}],
                "wirecap": True, "est_calls": 1,
                "assert": {"exit": 0}}
        row = run.run_case(case, args, run.Budget(args.budget))
        posts_b = [r for r in srv_b.requests if r["method"] == "POST"]
        posts_a = [r for r in srv_a.requests if r["method"] == "POST"]
        self.assertEqual(len(posts_b), 1,
                         "the row's own origin must receive the request")
        self.assertEqual(posts_b[0]["model"], "lxg-sc-stg/wire-tf")
        self.assertEqual(posts_a, [],
                         "the default origin must NOT receive a request that "
                         "belongs to another row's origin (r3 §2.4)")
        self.assertEqual(row["status"], "PASS")

    def test_map_carries_that_rows_own_credential_variable(self):
        """The route must name THAT row's credential, not the ambient one —
        the map is what T3 replaces the value with (r3 §4.2.1's triple)."""
        tmp = tempfile.mkdtemp(prefix="rt-t2-route-cred-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        live = _t2_write_cfg_home(
            tmp, T2_TWO_ORIGIN_CFG % {"a": "http://127.0.0.1:9",
                                     "b": "http://127.0.0.1:9"})
        home = run.HermeticHome(os.path.join(tmp, "root"), live, {}, None)
        route_map, causes = home.route_map(["row-tf"], [])
        self.assertEqual(causes, [])
        route = route_map["lxg-sc-stg/wire-tf"]
        self.assertEqual(route["credential_variable"], "WIRETAP_T2_KEY_HDR")
        self.assertEqual(route["credential_header"], "x-api-key")
        self.assertEqual(route["origin"], "http://127.0.0.1:9")

    def test_unrouted_request_reaches_no_origin_and_carries_no_credential(self):
        # Ruling RR-F5: this guard was vacuous on the pre-T3 bytes. It read
        # the environment BEFORE _fixture() installed _t2_env_guard, and it
        # spelled the plant upper-case while the guard writes
        # `synthetic-plant-<name.lower()>-value`, so `or plant` made the
        # assert true by construction AND the leak scan below searched a
        # string the child never sends: the test would have passed with a
        # real credential captured verbatim. T3 rewrites the credential
        # path this test guards, so the guard is fixed here.
        _t2_env_guard(self)
        plant = "synthetic-plant-%s-value" % "WIRETAP_T2_KEY_X".lower()
        self.assertEqual(os.environ.get("WIRETAP_T2_KEY_X"), plant,
                         "the plant this test traces must be the value "
                         "the guard actually installed")
        tmp, live, fake_bin, srv_a, srv_b = self._fixture(
            T2_ORPHAN_CFG, model="m-orphan",
            send_auth_env="WIRETAP_T2_KEY_X")
        args, out = _t2_case_args(tmp, live, fake_bin)
        case = {"id": "RT-T2-NOROUTE", "title": "fail closed",
                "driver": "headless", "model": "row-x",
                "steps": [{"op": "turn", "prompt": "ping"}],
                "wirecap": True, "est_calls": 1,
                "assert": {"exit": 1}}
        row = run.run_case(case, args, run.Budget(args.budget))
        self.assertEqual([r for r in srv_a.requests
                          if r["method"] == "POST"], [],
                         "an unresolved request must not be forwarded")
        self.assertEqual([r for r in srv_b.requests
                          if r["method"] == "POST"], [],
                         "an unresolved request must reach NO origin")
        wire_dir = os.path.join(out, "rt-t2-noroute", "wire")
        leak = [f for f in sorted(os.listdir(wire_dir))
                if plant in open(os.path.join(wire_dir, f),
                                 errors="replace").read()]
        self.assertEqual(leak, [],
                         "no credential may be captured or echoed on the "
                         "refused path (r3 §4.2.2 rule 2)")
        self.assertEqual(row["status"], "BLOCKED")


class UnroutedRequestTest(unittest.TestCase):
    """r3 §4.2.2 + r3a A2 (five methods) + r3c seat-1 F3. On the RetryLoopTest
    harness (r3a A12): the fake upstream, run_case with wirecap, the real
    wiretap."""

    def _run(self, tag, case_id, case, model="m-orphan", prewarm=False,
             cfg=None):
        tmp = tempfile.mkdtemp(prefix="rt-t2-unr-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        srv, upstream = _start_recording_upstream()
        self.addCleanup(srv.shutdown)
        live = _t2_write_cfg_home(tmp, (cfg or T2_ORPHAN_CFG)
                                  % {"a": upstream})
        prec = os.path.join(tmp, "prewarm-record.txt") if prewarm else None
        fake_bin = _write_fake_binary(tmp, upstream, model=model,
                                      prewarm_record=prec)
        args, out = _t2_case_args(tmp, live, fake_bin)
        case = dict(case, id=case_id)
        row = run.run_case(case, args, run.Budget(args.budget))
        run_dir = os.path.join(out, case_id.lower())
        return row, run_dir, srv, prec

    def _orphan_case(self, extra_assert=None):
        c = {"title": "unrouted request", "driver": "headless",
             "model": "row-x",
             "steps": [{"op": "turn", "prompt": "ping"}],
             "wirecap": True, "est_calls": 1,
             # every scored pin passes on the fixed bytes and the turn exits
             # non-zero: without rule 1 the verdict engine maps that FAIL to
             # PASS (run.py:3079), which is the defect A2 closes.
             "assert": {"exit": 1}}
        if extra_assert:
            c["assert"] = extra_assert
        return c

    def test_unrouted_request_writes_no_req_file(self):
        row, run_dir, srv, _ = self._run("noreq", "RT-T2-NOREQ",
                                        self._orphan_case())
        wire_dir = os.path.join(run_dir, "wire")
        reqs = sorted(f for f in os.listdir(wire_dir)
                      if f.startswith("req-"))
        self.assertEqual(reqs, [],
                         "no req-NNN.json may be written for a request that "
                         "was never forwarded (r3 §4.2.2 rule 3)")
        unr = os.path.join(wire_dir, "unrouted.jsonl")
        self.assertTrue(os.path.isfile(unr),
                        "the record is exactly one line in the case's own "
                        "capture dir")
        lines = [json.loads(l) for l in open(unr) if l.strip()]
        self.assertEqual(len(lines), 1)
        self.assertEqual(lines[0]["body_model"], "m-orphan")
        self.assertEqual(lines[0]["error_code"], "wiretap_unrouted_model")
        self.assertIn("path", lines[0])

    def test_unrouted_request_does_not_count_as_model_call(self):
        row, run_dir, srv, _ = self._run("count", "RT-T2-COUNT",
                                        self._orphan_case())
        wire_dir = os.path.join(run_dir, "wire")
        self.assertEqual(run.count_wire_model_calls(wire_dir), 0,
                         "an unrouted request is not a model call")
        self.assertEqual(row["model_calls"], 0,
                         "…so it must not be charged to the case budget")

    def test_steps_case_with_unrouted_request_is_blocked(self):
        row, run_dir, srv, _ = self._run("steps", "RT-T2-STEPBLK",
                                        self._orphan_case())
        want = "unrouted: m-orphan"
        self.assertEqual(row["status"], "BLOCKED",
                         "the first rule of finalize_verdict dominates the "
                         "FAIL->PASS mapping (r3a A2)")
        self.assertEqual(row.get("blocked_reason"), want)
        verdict = json.load(open(os.path.join(run_dir, "verdict.json")))
        self.assertEqual(verdict["status"], "BLOCKED")
        self.assertEqual(verdict["verdict"]["blocked_reason"], want)
        self.assertEqual(row["triage"]["classification_hint"], "route-map",
                         "no retry cures a route-map miss (r3b B4)")

    def test_rows_case_with_unrouted_request_is_blocked(self):
        """The rows path returns before the steps/assert dispatch, so it has
        no scored pin: on the frozen bytes this cell is a plain PASS."""
        row, run_dir, srv, _ = self._run(
            "rows", "RT-T2-ROWSBLK",
            {"title": "unrouted rows", "driver": "headless",
             "model": "row-x",
             "rows": [{"model": "row-x"}, {"model": "row-y"}],
             "wirecap": True, "est_calls": 2})
        want = "unrouted: m-orphan"
        self.assertEqual(row["status"], "BLOCKED")
        self.assertEqual(row.get("blocked_reason"), want)
        verdict = json.load(open(os.path.join(run_dir, "verdict.json")))
        self.assertEqual(verdict["verdict"]["blocked_reason"], want)
        self.assertEqual(row["model_calls"], 0)

    def test_get_probe_keeps_its_capture_number(self):
        """GUARD (r3b B4): green before and after — the frozen wiretap already
        numbers every request, and r3a A3 requires the fixed one to keep doing
        so (four cases pin req-003.json assuming 001/002 are the probes)."""
        row, run_dir, srv, _ = self._run("num", "RT-T2-NUM",
                                         {"title": "probe numbering",
                                          "driver": "headless",
                                          "model": "row-x",
                                          "steps": [{"op": "turn",
                                                     "prompt": "ping"}],
                                          "wirecap": True, "est_calls": 1,
                                          "assert": {"exit": 0}},
                                         model="m-x", prewarm=True)
        wire_dir = os.path.join(run_dir, "wire")
        methods = {}
        for f in sorted(os.listdir(wire_dir)):
            if f.startswith("req-"):
                methods[f] = json.load(
                    open(os.path.join(wire_dir, f)))["method"]
        self.assertEqual(methods.get("req-001.json"), "GET",
                         "the prewarm probe keeps sequence number 1")
        self.assertEqual(methods.get("req-002.json"), "POST",
                         "…so the model request is number 2, not 1")
        self.assertNotIn("req-003.json", methods)

    def test_get_probe_answered_locally(self):
        """RED (r3c seat-1 F3): on the frozen bytes do_GET forwards `GET /`
        upstream, so the upstream sees it and the client gets the upstream's
        body rather than the wiretap's static one."""
        row, run_dir, srv, prec = self._run("prewarm", "RT-T2-PREWARM",
                                            {"title": "prewarm answered "
                                                     "locally",
                                             "driver": "headless",
                                             "model": "row-x",
                                             "steps": [{"op": "turn",
                                                        "prompt": "ping"}],
                                             "wirecap": True, "est_calls": 1,
                                             "assert": {"exit": 0}},
                                            model="m-x", prewarm=True)
        gets = [r for r in srv.requests if r["method"] == "GET"]
        self.assertEqual(gets, [],
                         "GET / is the binary's transport prewarm on the "
                         "origin root — it must be answered locally, never "
                         "forwarded (r3a A3)")
        text = open(prec).read()
        self.assertIn("wiretap_prewarm", text,
                      "the client must receive the wiretap's own static body")
        wire_dir = os.path.join(run_dir, "wire")
        self.assertFalse(os.path.exists(os.path.join(wire_dir,
                                                    "unrouted.jsonl")),
                         "the probe is not a model call and never BLOCKS a "
                         "case")


class KeyFreeModeUnchangedTest(unittest.TestCase):
    """GUARD (r3 §4.6 last row): routing must not touch env.proxy = "none".
    Green before and after — r3a A12's harness note aside, the key-free path
    starts no wiretap, so it has no route map at all."""

    def test_key_free_case_starts_no_wiretap_and_no_unrouted_residue(self):
        tmp = tempfile.mkdtemp(prefix="rt-t2-keyfree-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        cases_dir = os.path.join(tmp, "cases")
        pre = os.path.join(cases_dir, "rt-t2-keyfree", "wire")
        os.makedirs(pre)
        with open(os.path.join(pre, "golden-wire.json"), "w") as fh:
            fh.write(json.dumps({"golden": True}))
        saved_cases = run.CASES_DIR
        run.CASES_DIR = cases_dir
        try:
            live = _write_fake_live_home(tmp, "http://127.0.0.1:9")
            fake_bin = _write_fake_binary(tmp, "http://127.0.0.1:9")
            started = []
            real_wiretap = run.Wiretap

            def _spy(*a, **kw):
                started.append((a, kw))
                return real_wiretap(*a, **kw)
            run.Wiretap = _spy
            self.addCleanup(setattr, run, "Wiretap", real_wiretap)
            args, out = _t2_case_args(tmp, live, fake_bin, wirecap=False)
            # r3c seat-1 F4 harness note: the L1 key-free path rejects a
            # case carrying a live model op, so the fixture mirrors the
            # shape of the real key-free cases (no steps, est_calls 0,
            # the pre-placed wire dir is the only evidence).
            case = {"id": "RT-T2-KEYFREE", "title": "key-free unchanged",
                    "driver": "headless", "model": "m-x",
                    "env": {"proxy": "none"},
                    "steps": [], "wirecap": False, "est_calls": 0,
                    "assert": {"wire": [{"id": "golden_present",
                                         "kind": "count",
                                         "file": "golden-*.json",
                                         "min": 1, "max": 1}]}}
            row = run.run_case(case, args, run.Budget(args.budget))
        finally:
            run.CASES_DIR = saved_cases
        self.assertEqual(started, [],
                         "env.proxy=none still runs key-free against a dead "
                         "port: no wiretap, therefore no route map")
        run_dir = os.path.join(out, "rt-t2-keyfree")
        wire_dir = os.path.join(run_dir, "wire")
        self.assertFalse(os.path.exists(os.path.join(wire_dir,
                                                    "unrouted.jsonl")),
                         "a key-free case never reaches the fail-closed path")
        self.assertTrue(os.path.isfile(os.path.join(wire_dir,
                                                   "golden-wire.json")),
                        "the pre-placed wire inputs are still copied in")
        self.assertEqual(run.count_wire_model_calls(wire_dir), 0,
                         "a key-free case is still not a model call")
        # The preserved value, measured on the frozen pre-T2 bytes: PASS.
        self.assertEqual(row["status"], "PASS",
                         "L1 key-free status must be unchanged by T2")


# ---------------------------------------------------------------------------
# REDTEAM-REFRESH T2 FIX pass — coordinator ruling RR-3 (bead apex-mwga, the
# fresh batch-2 review of unit T2). Three behaviours, each proved test-first
# on this batch's own starting bytes:
#   F-1 arm (b): an unresolvable SELECTOR adds no route — there is no
#     default-origin fallback (r3 §4.2.1) — so its request is refused and the
#     case BLOCKS, exactly as an unresolvable pin already does.
#   F-2: the wiretap keeps `route_map is None` (no routing configured) apart
#     from `{}` (routing configured, zero routes): an empty map refuses every
#     POST instead of forwarding everything to --upstream.
#   F-3: two selected rows sharing one wire id on DIFFERENT triples record
#     "ambiguous-route: <wire id>" and get no route; dict order never picks
#     the origin a credential would later be presented to.
# A fourth, self-found in the same function while writing these: `--rows a,b`
# reaches case_route_selectors as the raw CLI string and was iterated
# character by character, so the rows actually run never entered the map.
#
# Same offline harness as the classes above (r3a A12): run_case + the real
# wiretap + loopback recorders. `.invalid` origins appear only in
# map-construction tests that dial nothing. Synthetic credential NAMES under
# the existing env guard only (approval F2) — no value is read by an assert.
# ---------------------------------------------------------------------------

# The exact shape RR-3 F-3 names (the live pair is gpt-6.1-sol /
# gpt-6.1-sol-1m): two rows, ONE wire id, different origin AND different
# credential variable, so the triples differ.
RR3_AMBIG_CFG = '''base_url = "%(a)s/v1"
models_base_url = "%(a)s/v1"
[features]
turn_summary = false

[model."sol"]
model = "m-sol"
base_url = "%(a)s/v1"
api_backend = "responses"
env_key = "WIRETAP_T2_KEY_X"

[model."sol-1m"]
model = "m-sol"
base_url = "%(b)s/v1"
api_backend = "responses"
env_key = "WIRETAP_T2_KEY_B"
'''


class RR3SelectorRouteTest(unittest.TestCase):
    """ruling RR-3 F-1 + F-3: the selector half of `HermeticHome.route_map`,
    at the map boundary and end-to-end through the real wiretap."""

    def _home(self, tmp, cfg_text, port=59999):
        live = _t2_write_cfg_home(tmp, cfg_text)
        return run.HermeticHome(os.path.join(tmp, "root"), live, {}, port)

    # ---- F-1: an unresolvable selector adds no route ----------------------

    def test_unknown_selector_adds_no_route(self):
        """RED on the batch's starting bytes: the builder synthesized
        {origin: default, credential_variable: None, header: authorization}
        for any selector that is neither a row name nor a wire id, which r3
        §4.2.1 forbids in as many words ("no second key and no default-origin
        fallback") and which r3 §2.4 is the record of."""
        tmp = tempfile.mkdtemp(prefix="rt-t2f-ghost-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        home = self._home(tmp, T2_DERIVE_CFG)
        route_map, causes = home.route_map(["m-ghost"], [])
        self.assertEqual(
            route_map, {},
            "an unresolvable selector must add NO route (RR-3 F-1); the "
            "request is then refused by the wiretap, not forwarded to the "
            "deployment default origin")
        self.assertEqual(causes, [],
                         "an unresolvable selector is not an ambiguity: it "
                         "adds nothing, like an unresolvable pin")

    def test_unknown_selector_never_yields_the_default_origin(self):
        """93 of the 139 cases declare at least one such selector (retired
        ids), so this is the majority shape of the corpus, not a constructed
        input."""
        tmp = tempfile.mkdtemp(prefix="rt-t2f-mixed-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        home = self._home(tmp, T2_DERIVE_CFG)
        route_map, causes = home.route_map(["r2", "m-ghost", "grok-4.6"], [])
        self.assertEqual(sorted(route_map), ["wire-b"],
                         "only the resolvable selector contributes a route")
        self.assertEqual(causes, [])
        for key, route in route_map.items():
            self.assertNotEqual(
                route["origin"], home.default_origin,
                "no synthesized default-origin route may survive (key %s)"
                % key)

    def test_declared_but_unresolvable_id_is_not_forwarded(self):
        """The reachable harm, end-to-end: a case that declares an id no live
        row serves used to have that request forwarded to the deployment
        default origin carrying the child's own credential. Fail-closed means
        it now reaches NO origin and the row reads `unrouted: <id>` (R-17's
        named-id residue)."""
        tmp = tempfile.mkdtemp(prefix="rt-t2f-decl-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        srv, upstream = _start_recording_upstream()
        self.addCleanup(srv.shutdown)
        live = _t2_write_cfg_home(tmp, T2_ORPHAN_CFG % {"a": upstream})
        fake_bin = _write_fake_binary(tmp, upstream, model="m-ghost")
        args, out = _t2_case_args(tmp, live, fake_bin,
                                  rows=["m-ghost"])
        case = {"id": "RT-T2F-DECLARED", "title": "declared, unserved id",
                "driver": "headless", "model": "row-x",
                "steps": [{"op": "turn", "prompt": "ping"}],
                "wirecap": True, "est_calls": 1,
                "assert": {"exit": 1}}
        row = run.run_case(case, args, run.Budget(args.budget))
        self.assertEqual([r for r in srv.requests if r["method"] == "POST"],
                         [],
                         "an id no row declares must reach no origin — the "
                         "default origin is not a fallback (RR-3 F-1)")
        self.assertEqual(row["status"], "BLOCKED")
        self.assertEqual(row.get("blocked_reason"), "unrouted: m-ghost",
                         "the cause names the id, so the residue says what "
                         "to remap")
        wire_dir = os.path.join(out, "rt-t2f-declared", "wire")
        self.assertEqual(sorted(f for f in os.listdir(wire_dir)
                                if f.startswith("req-")), [],
                         "a refused request is not a model call")
        self.assertEqual(run.count_wire_model_calls(wire_dir), 0)

    # ---- self-found: `--rows a,b` is one CLI string, not a sequence -------

    def test_rows_override_is_split_on_commas(self):
        """`--rows a,b` (run.py:24, argparse :5999) is consumed as a
        comma-separated string at run.py:3780. Handing that same string to
        case_route_selectors iterated CHARACTERS: the rows actually run never
        entered the selector set and the map filled with one-letter keys."""
        case = {"id": "PROBE", "model": "row-x",
                "rows": [{"model": "row-x"}, {"model": "row-tf"}]}
        sel, pins = run.case_route_selectors(case, "row-x,row-tf")
        self.assertEqual(sel, ["row-x", "row-tf"],
                         "the --rows override must contribute the row names "
                         "it names, not their characters")
        self.assertNotIn(",", sel)
        self.assertEqual([s for s in sel if len(s) == 1], [],
                         "a one-letter selector is a parsing artifact, and "
                         "every one of them resolves to no row")

    # ---- F-3: the selector path must not let dict order choose ------------

    def test_two_rows_sharing_wire_id_on_different_routes_record_cause(self):
        """r3b B1 rule 3 applied to the selector channel. The starting bytes
        resolved `m-sol` to whichever of `sol` / `sol-1m` was processed last
        and left `causes` empty."""
        tmp = tempfile.mkdtemp(prefix="rt-t2f-amb-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        cfg = RR3_AMBIG_CFG % {"a": "http://originA.invalid",
                               "b": "http://originB.invalid"}
        home = self._home(tmp, cfg)
        route_map, causes = home.route_map(["sol", "sol-1m"], [])
        self.assertEqual(causes, ["ambiguous-route: m-sol"],
                         "RR-3 F-3: the collision is recorded, not resolved "
                         "by dict order")
        self.assertNotIn("m-sol", route_map,
                         "no route may be guessed for an ambiguous wire id")
        route_map_r, causes_r = home.route_map(["sol-1m", "sol"], [])
        self.assertEqual((route_map_r, causes_r), (route_map, causes),
                         "the outcome must not depend on selector order")

    def test_same_triple_siblings_are_one_route_not_an_ambiguity(self):
        """PRESERVATION (green before and after): the fix must not
        over-block. r2 and r6 share the wire id `wire-b` AND one triple, so
        selecting both is one route behind two row names — the live
        claude-opus-5.5 / -1m shape."""
        tmp = tempfile.mkdtemp(prefix="rt-t2f-sib-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        home = self._home(tmp, T2_DERIVE_CFG)
        route_map, causes = home.route_map(["r2", "r6"], [])
        self.assertEqual(causes, [])
        self.assertEqual(sorted(route_map), ["wire-b"])
        self.assertEqual(sorted(route_map["wire-b"]["row_names"]),
                         ["r2", "r6"])

    def test_ambiguous_selector_blocks_the_case_with_named_route(self):
        """End-to-end for F-3: the decision is made before any step, so
        nothing is forwarded on a guessed origin, the row and verdict.json
        carry the new cause verbatim, the cause file names its kind, and the
        triage hint says route-map (r3b B4) rather than env."""
        tmp = tempfile.mkdtemp(prefix="rt-t2f-ambcase-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        srv_a, upstream_a = _start_recording_upstream()
        self.addCleanup(srv_a.shutdown)
        srv_b, upstream_b = _start_recording_upstream()
        self.addCleanup(srv_b.shutdown)
        live = _t2_write_cfg_home(tmp, RR3_AMBIG_CFG
                                  % {"a": upstream_a, "b": upstream_b})
        fake_bin = _write_fake_binary(tmp, upstream_a, model="m-sol")
        args, out = _t2_case_args(tmp, live, fake_bin)
        case = {"id": "RT-T2F-AMBCASE", "title": "ambiguous selector route",
                "driver": "headless", "model": "sol",
                "rows": [{"model": "sol", "prompt": "ping"},
                         {"model": "sol-1m", "prompt": "ping"}],
                "wirecap": True, "est_calls": 2}
        row = run.run_case(case, args, run.Budget(args.budget))
        want = "ambiguous-route: m-sol"
        self.assertEqual(row["status"], "BLOCKED",
                         "an ambiguous selector route must BLOCK, never "
                         "forward on the last writer's origin")
        self.assertEqual(row.get("blocked_reason"), want,
                         "the row carries the cause verbatim, naming the id")
        run_dir = os.path.join(out, "rt-t2f-ambcase")
        verdict = json.load(open(os.path.join(run_dir, "verdict.json")))
        self.assertEqual(verdict["status"], "BLOCKED")
        self.assertEqual(verdict["verdict"]["blocked_reason"], want)
        causes = [json.loads(l) for l in
                  open(os.path.join(run_dir, "wire", "route-block.jsonl"))
                  if l.strip()]
        self.assertEqual(causes, [{"kind": "ambiguous-route",
                                   "wire_model_id": "m-sol",
                                   "detail": want}],
                         "the cause file keeps its own kind distinct from the "
                         "pin-path kind")
        self.assertEqual(row["triage"]["classification_hint"], "route-map",
                         "no env retry cures an ambiguous route (r3b B4)")
        for srv in (srv_a, srv_b):
            self.assertEqual([r for r in srv.requests
                              if r["method"] == "POST"], [],
                             "nothing may be forwarded once a route is "
                             "undecided")


class RR3RouteMapEmptinessTest(unittest.TestCase):
    """ruling RR-3 F-2, through the real child process: the blob is handed
    over by env (`WIRETAP_ROUTE_MAP`), so the two states have to survive the
    env seam, the JSON parse, `serve()` and the handler predicate."""

    def _child(self, tmp, upstream, route_map):
        cap = os.path.join(tmp, "wire")
        os.makedirs(cap, exist_ok=True)
        wt = run.Wiretap(run.free_port(), upstream, cap, "",
                         route_map=route_map)
        self.addCleanup(wt.stop)
        self.assertTrue(wt.wait_ready(), "the wiretap child never came up")
        return wt, cap

    def _post(self, wt, path, model):
        import urllib.error
        import urllib.request
        req = urllib.request.Request(
            "http://127.0.0.1:%d%s" % (wt.port, path),
            data=json.dumps({"model": model, "input": "hi"}).encode(),
            method="POST",
            headers={"Content-Type": "application/json",
                     "Authorization": "Bearer synthetic-plant-value"})
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                return r.status, None, None, r.read()
        except urllib.error.HTTPError as exc:
            body = exc.read()
            try:
                err = json.loads(body.decode())["error"]
            except Exception:
                err = {}
            return exc.code, err.get("code"), err.get("param"), body

    def test_empty_route_map_refuses_every_post(self):
        """RED on the starting bytes: `if not self.route_map` folded the
        empty map into "no routing configured" and forwarded everything to
        --upstream, so a map that declares zero routes was the one state that
        routed nothing at all."""
        tmp = tempfile.mkdtemp(prefix="rt-t2f-empty-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        srv, upstream = _start_recording_upstream()
        self.addCleanup(srv.shutdown)
        wt, cap = self._child(tmp, upstream, {})
        status, code, param, _body = self._post(
            wt, "/v1/responses", "model-no-row-declares-this")
        self.assertEqual(
            (status, code, param),
            (400, "wiretap_unrouted_model", ""),
            "an empty map is routing configured with zero routes: refuse, "
            "with the same body a fail-closed miss has (RR-3 F-2)")
        self.assertEqual([r for r in srv.requests
                          if r["method"] == "POST"], [],
                         "the empty map must not degrade to mapless "
                         "forwarding to --upstream")
        self.assertEqual(sorted(f for f in os.listdir(cap)
                                if f.startswith("req-")), [],
                         "a refused request is still not captured")
        with open(os.path.join(cap, "unrouted.jsonl")) as fh:
            lines = [json.loads(l) for l in fh if l.strip()]
        self.assertEqual(len(lines), 1)
        self.assertEqual(lines[0]["error_code"], "wiretap_unrouted_model")

    def test_no_route_map_keeps_mapless_forwarding(self):
        """PRESERVATION leg of RR-3 F-2's "test both": route_map None means
        no routing was configured at all (a hand-run wiretap), and that
        behaviour is unchanged — the POST reaches --upstream and nothing is
        written to unrouted.jsonl. Green before and after; the point is that
        the fix must not make the mapless wiretap refuse its own traffic."""
        tmp = tempfile.mkdtemp(prefix="rt-t2f-none-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        srv, upstream = _start_recording_upstream()
        self.addCleanup(srv.shutdown)
        wt, cap = self._child(tmp, upstream, None)
        status, code, _param, body = self._post(wt, "/v1/responses", "m-any")
        posts = [r for r in srv.requests if r["method"] == "POST"]
        self.assertEqual(status, 200,
                         "without a route map the wiretap still forwards: "
                         "got status=%r code=%r body=%r"
                         % (status, code, body[:120]))
        self.assertEqual(len(posts), 1)
        self.assertEqual(posts[0]["model"], "m-any")
        self.assertFalse(os.path.exists(os.path.join(cap, "unrouted.jsonl")),
                         "a mapless wiretap never takes the fail-closed path")


def main():
    unittest.main(verbosity=2)


if __name__ == "__main__":
    main()
# ---------------------------------------------------------------------------
# REDTEAM-REFRESH T3 (batch 3) — the declared credential set, replacement,
# response scrub, sweep exit status, the child allowlist and the route-aware
# recheck. r3 §4.3/§4.4/§4.6 + §8 T3, amended by r3a A4/A5/A6/A7/A12/A13,
# r3b B4/B7/B8 and r3c seat-1 F4, under approval F2/F5 and ruling RR-4.
#
# Offline only: the fake upstream is still the only transport (r3a A12). Every
# credential in this block is a SYNTHETIC plant written into this process's own
# environment under a guard and restored on cleanup; no real credential value
# is ever read, printed, hashed or piped by this file or by the writer of it
# (brief r2 §4, ruling RR-2).
#
# The swept-name guard plants a DISTINCT SHAPE per name on purpose:
#   * a `sk_` form, a JWT, a >= 40-char base64 blob and a `Basic`-style blob,
#     so a sweep that only greps the ambient key cannot pass it;
#   * the placeholder name gets its own value, which must NEVER be matched.
# ---------------------------------------------------------------------------

T3_SWEPT_PLANTS = {
    "APEX_LLM_PROXY_KEY": "sk_" + "plantapexproxykey" + "0123456789abcdef",
    "CODEX_LLM_PROXY_KEY": "sk-" + "plantcodexproxykey" + "-0123456789abcdef",
    "CODEX_TRUE_FOUNDRY_KEY": (
        "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9"
        "eyJzdWIiOiJwbGFudCJ9.plantsignaturepartthree"),
    "GROK_DIRECT_ONPREM_AUTH": (
        "pGxYW50b25wcmVtYmFzZWg2NGJsb2Jmb3J0aGVhdXhw"
        "bGFudHZhbHVlMDEyMzQ1Njc4OQ=="),
    "XAI_API_KEY": "sk_" + "plantxaiapikey" + "0123456789abcdef0",
    "GROK_CODE_XAI_API_KEY": "sk-" + "plantlegacycodekey" +
                             "-0123456789abcdef",
    "GROK_DIRECT_PLACEHOLDER_KEY": "placeholder-plant-value-not-swept",
}
T3_SWEEP_EXCLUDED = ("GROK_DIRECT_PLACEHOLDER_KEY",)


def _t3_env_plant(name):
    """The value this file plants for one credential NAME.

    Never a real value: these are literals in this file, and the guard below
    installs them for the duration of one test and restores the prior state."""
    return T3_SWEPT_PLANTS[name]


def _t3_swept_env_guard(test, names=None, unset=()):
    """House EnvGuard for the declared credential names (approval F2).

    Sets every swept name plus the placeholder to its synthetic plant, except
    the names in `unset`, which are REMOVED for the test so the three states of
    r3 §4.3.3 (present / absent-and-unreachable / absent-and-reachable) all
    reachable offline. Restored on cleanup."""
    names = list(names if names is not None else T3_SWEPT_PLANTS)
    saved = {}
    for k in names:
        saved[k] = os.environ.get(k)
        if k in unset:
            os.environ.pop(k, None)
        else:
            os.environ[k] = _t3_env_plant(k)

    def restore():
        for k, v in saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
    test.addCleanup(restore)


def _t3_env_unset(test, name):
    """Remove one name for the duration of the test (the `absent` state)."""
    saved = os.environ.get(name)
    os.environ.pop(name, None)

    def restore():
        if saved is None:
            os.environ.pop(name, None)
        else:
            os.environ[name] = saved
    test.addCleanup(restore)
    return saved


# One row per rung class (r3 §4.3.5): a proxy row with env_key, a
# TrueFoundry-class row with env_key, an on-prem row with env_key PLUS an
# env_http_headers variable, and a row declaring neither — the last is what
# puts the product fallback chain into the derived set.
T3_RUNG_CFG = '''base_url = "http://default.invalid/v1"
models_base_url = "http://default.invalid/v1"
auth_stub = "/nonexistent/proxy-auth-stub.sh"

[model."row-proxy"]
base_url = "http://proxy.invalid/v1"
api_backend = "responses"
env_key = "CODEX_LLM_PROXY_KEY"

[model."row-tf"]
base_url = "http://tf.invalid/v1"
api_backend = "responses"
env_key = "CODEX_TRUE_FOUNDRY_KEY"

[model."row-onprem"]
base_url = "http://onprem.invalid/v1"
api_backend = "responses"
env_key = "GROK_DIRECT_PLACEHOLDER_KEY"

[model."row-onprem".env_http_headers]
Authorization = "GROK_DIRECT_ONPREM_AUTH"

[model."row-chain"]
base_url = "http://chain.invalid/v1"
api_backend = "responses"

[features]
turn_summary = false
'''

T3_RUNG_UNION = ("APEX_LLM_PROXY_KEY", "CODEX_LLM_PROXY_KEY",
                 "CODEX_TRUE_FOUNDRY_KEY", "GROK_CODE_XAI_API_KEY",
                 "GROK_DIRECT_ONPREM_AUTH", "GROK_DIRECT_PLACEHOLDER_KEY",
                 "XAI_API_KEY")


def _t3_rung_rows():
    default = run._origin_from_base_url("http://default.invalid/v1")
    return run.derive_row_routes(T3_RUNG_CFG, default)


class DeclaredRungCoverageTest(unittest.TestCase):
    """r3 §4.3.5 / §4.6 row 6, as amended by r3a A5.

    A SIBLING of `EnvContractTest`, not a subclass: r3 §4.3.5 says
    "extending EnvContractTest", and subclassing would re-run the parent's
    three methods under this class name, which breaks the R2-5 arithmetic
    ("testsRun delta == the number of `def test_` methods this cut added").
    The two classes together are the coverage r3 asks for; the deviation is
    recorded in unit-T3-result.json."""

    def test_derived_set_covers_every_rung(self):
        surface = run.credential_surface(_t3_rung_rows())
        self.assertEqual(sorted(surface["names"]), sorted(T3_RUNG_UNION),
                         "all 7 names must be derived (r3 §4.3.1: the "
                         "env_key rungs, the env_http_headers variable and "
                         "the fallback chain)")
        self.assertEqual(surface["swept_names"],
                         list(run.LITERAL_SWEEP_NAMES),
                         "one swept set, stated once (r3a A5)")
        self.assertEqual(len(surface["swept_names"]), 6,
                         "r3a A5: 7 names minus the placeholder = 6")
        self.assertEqual(surface["excluded_names"], list(T3_SWEEP_EXCLUDED),
                         "sweep_excluded_names is exactly "
                         "GROK_DIRECT_PLACEHOLDER_KEY")
        self.assertIn("GROK_CODE_XAI_API_KEY", surface["swept_names"],
                      "the optional chain rung is swept when it is set "
                      "(r3a A5 replaced the r3 derivation)")

    def test_dropping_a_rung_changes_the_derived_set(self):
        rows = [r for r in _t3_rung_rows() if r["row_name"] != "row-chain"]
        surface = run.credential_surface(rows)
        for rung in run.XAI_CREDENTIAL_CHAIN:
            self.assertNotIn(rung, surface["names"],
                             "no row declares neither credential, so the "
                             "fallback chain must not be in the set — this "
                             "is the assertion that fails if a rung is "
                             "dropped or hard-coded in (r3 §4.3.5)")
        rows2 = [r for r in _t3_rung_rows()
                 if r["row_name"] != "row-onprem"]
        surface2 = run.credential_surface(rows2)
        self.assertNotIn("GROK_DIRECT_ONPREM_AUTH", surface2["names"],
                         "the header variable is the rung r1 could not see")
        self.assertNotIn("GROK_DIRECT_PLACEHOLDER_KEY",
                         surface["swept_names"],
                         "adding the placeholder to the swept set must "
                         "fail the test (R2-4)")

    def test_provider_vars_unset_superset_rule_still_enforced(self):
        """GUARD (green before and after): §4.3.2 keeps the case-level
        superset rule enforced even though the allowlist makes it trivially
        true — a case must not widen the child env by declaring a
        superset."""
        tmp = tempfile.mkdtemp(prefix="rt-t3-superset-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        c = _base_new_case()
        c["env"] = {"provider_vars_unset": ["OPENAI_API_KEY"]}
        p = _write_case(tmp, "rt-t3-superset.json", c)
        errs = run.validate_case_file(p)
        self.assertTrue(any("provider_vars_unset" in e for e in errs),
                        "a subset declaration must still be rejected: %s"
                        % errs)


class ChildEnvAllowlistTest(unittest.TestCase):
    """r3 §4.3.4's four named tests, plus r3a A4's reachability guard and
    r3a A6's chain resolution. RED on the frozen bytes: `build_env` returns
    `os.environ` minus four names, so every planted credential name is
    in it."""

    PLANTED = ("APEX_OTHER_LLM_PROXY_KEY", "CLAUDE_CODE_LLM_PROXY_KEY",
               "CODEX_INFERENCE_DIRECT_KEY", "LLM_PROXY_KEY",
               "NETCODE_LLM_PROXY_KEY", "PI_DEV_LLM_PROXY_KEY",
               "SPECTRE_LLM_PROXY_KEY", "SCS_PASS", "OPERATOR_SSH_KEY",
               "KEY")

    def _plant(self, names):
        saved = {}
        for k in names:
            saved[k] = os.environ.get(k)
            os.environ[k] = "synthetic-plant-%s-value" % k.lower()

        def restore():
            for k, v in saved.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
        self.addCleanup(restore)

    def test_undeclared_credential_names_never_reach_the_child(self):
        self._plant(self.PLANTED)
        env = run.build_env("/nonexistent-home")
        for k in self.PLANTED:
            self.assertNotIn(k, _t3_names(env),
                             "%s is not an allowlisted name and no "
                             "route declares it — it must be absent "
                             "BY CONSTRUCTION, not by a regex "
                             "(R2-7)" % k)

    def test_non_credential_allowlist_is_the_named_15(self):
        self.assertEqual(
            tuple(run.CHILD_ENV_NON_CREDENTIAL_ALLOWLIST),
            ("PATH", "HOME", "LANG", "LC_ALL", "TERM", "TZ", "USER",
             "SHELL", "TMPDIR", "SSL_CERT_FILE", "SSL_CERT_DIR",
             "REQUESTS_CA_BUNDLE", "NODE_EXTRA_CA_CERTS", "GROK_HOME",
             "GROK_AUTH_EXPIRED"),
            "the 15 names of r3 §4.3.4, in the order the plan lists them")
        self._plant(self.PLANTED)
        _t2_env_guard(self)
        env = run.build_env("/nonexistent-home")
        allowed = set(run.CHILD_ENV_NON_CREDENTIAL_ALLOWLIST)
        extra = sorted(k for k in env if k not in allowed)
        self.assertEqual(extra, [],
                         "build_env may return nothing beyond the allowlist "
                         "when no route credential is declared")

    def test_env_contract_variables_still_reach_the_child(self):
        """GUARD (green before and after): the allowlist must not break
        §4.5's auth stub or the hermetic home."""
        home = "/nonexistent-home"
        env = run.build_env(home)
        self.assertEqual(env.get("GROK_AUTH_EXPIRED"), "1")
        self.assertEqual(env.get("GROK_HOME"), os.path.abspath(home))
        self.assertIn("PATH", _t3_names(env))

    def test_unreachable_route_credential_is_absent(self):
        _t2_env_guard(self)
        env = run.build_env("/nonexistent-home")
        self.assertNotIn("WIRETAP_T2_KEY_B", _t3_names(env),
                         "a case whose selectors never name that row"
                         " gets no credential for it")
        self.assertNotIn("WIRETAP_T2_KEY_HDR", _t3_names(env),
                         "the same for a direct-on-prem header "
                         "variable")

    def test_reachable_route_credential_is_present(self):
        _t2_env_guard(self)
        env = run.build_env("/nonexistent-home",
                            credential_names=["WIRETAP_T2_KEY_X"])
        _t3_assert_value(self, env.get("WIRETAP_T2_KEY_X"),
                         "synthetic-plant-wiretap_t2_key_x-value",
                         "the reachable route's own credential is"
                         " what the guard planted")
        for other in ("WIRETAP_T2_KEY_B", "WIRETAP_T2_KEY_HDR",
                      "WIRETAP_T2_KEY_AMB", "WIRETAP_T2_KEY_AMB1M"):
            self.assertNotIn(other, _t3_names(env),
                             "nothing else credential-shaped may"
                             " ride in")

    def test_pin_declared_model_route_is_reachable(self):
        """GUARD (r3b B4: green on the frozen bytes because build_env
        inherits everything, green after). r3a A4: a wire pin's
        `where.body.model` is part of the selector set, so that row's route
        is in the map and that row's credential is in the child env."""
        _t2_env_guard(self)
        tmp = tempfile.mkdtemp(prefix="rt-t3-pin-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        live = _t2_write_cfg_home(
            tmp, T2_TWO_ORIGIN_CFG % {"a": "http://127.0.0.1:9",
                                      "b": "http://127.0.0.1:9"})
        case = {"id": "RT-T3-PIN", "title": "pin-declared route",
                "driver": "headless", "model": "row-x",
                "steps": [{"op": "turn", "prompt": "ping"}],
                "wirecap": True, "est_calls": 1,
                "assert": {"wire": [{"path": "req-*.json",
                                     "where": {"body.model":
                                               "lxg-sc-stg/wire-tf"},
                                     "field": "model", "eq": True}]}}
        sel, pins = run.case_route_selectors(case)
        home = run.HermeticHome(os.path.join(tmp, "root"), live, {}, None)
        route_map, causes = home.route_map(sel, pins)
        self.assertEqual(causes, [])
        self.assertIn("lxg-sc-stg/wire-tf", route_map,
                      "a model named only by a wire pin still needs a route")
        values, _causes = run.resolve_route_credentials(
            route_map, env={"WIRETAP_T2_KEY_X": "synthetic-a",
                            "WIRETAP_T2_KEY_HDR": "synthetic-b"})
        env = run.build_env("/nonexistent-home",
                            credential_names=sorted(values))
        self.assertIn("WIRETAP_T2_KEY_HDR", _t3_names(env),
                      "the pinned row's own credential must reach "
                      "the child that runs it")

    def test_chain_row_resolves_first_set_name(self):
        """r3a A6: for a row that declares neither credential, the variable
        is the FIRST name set at run time in auth_method.rs order.

        The guard is not decorative: `build_env` reads `os.environ` (it is
        the child-env builder), so without it this fixture would hand the
        child the operator's live key and print it in the failure text."""
        _t3_swept_env_guard(self)
        route_map = {"m-chain": {"origin": "http://chain.invalid",
                                 "credential_variable": None,
                                 "credential_header": "authorization",
                                 "row_names": ["row-chain"]}}
        env = {"APEX_LLM_PROXY_KEY": _t3_env_plant("APEX_LLM_PROXY_KEY")}
        values, causes = run.resolve_route_credentials(route_map, env=env)
        self.assertEqual(causes, [],
                         "a chain row is not BLOCKED when a rung is set")
        self.assertEqual(sorted(values), ["APEX_LLM_PROXY_KEY"],
                         "the first SET name wins, not the first name")
        child = run.build_env("/nonexistent-home",
                              credential_names=sorted(values))
        _t3_assert_value(self, child.get("APEX_LLM_PROXY_KEY"),
                         _t3_env_plant("APEX_LLM_PROXY_KEY"),
                         "the child gets the same value the wiretap "
                         "injects (A6)")
        env2 = {"GROK_CODE_XAI_API_KEY":
                _t3_env_plant("GROK_CODE_XAI_API_KEY"),
                "APEX_LLM_PROXY_KEY": _t3_env_plant("APEX_LLM_PROXY_KEY")}
        values2, _c2 = run.resolve_route_credentials(route_map, env=env2)
        self.assertEqual(sorted(values2), ["GROK_CODE_XAI_API_KEY"],
                         "the chain order is XAI_API_KEY, "
                         "GROK_CODE_XAI_API_KEY, APEX_LLM_PROXY_KEY")


class CredentialUnsetStateTest(unittest.TestCase):
    """r3 §4.3.3 + r3a A6: present / absent-and-unreachable /
    absent-and-reachable are three different states. r2's rule (any unset
    declared name aborts the run) is what this replaces."""

    def test_absent_unreachable_name_is_recorded_absent(self):
        names = list(T3_RUNG_UNION)
        meta = run.credential_env_meta(names, env={})
        self.assertEqual(set(meta), set(names))
        self.assertEqual(set(meta.values()), {"ABSENT"},
                         "an unset declared name is recorded ABSENT, never "
                         "a failure and never a value")
        meta2 = run.credential_env_meta(names, env={
            "XAI_API_KEY": _t3_env_plant("XAI_API_KEY")})
        self.assertNotEqual(meta2["XAI_API_KEY"], "ABSENT")
        _t3_assert_absent(self, meta2,
                          [("XAI_API_KEY",
                            _t3_env_plant("XAI_API_KEY"))],
                          "env_meta carries a digest, never a value")
        route_map = {"m-x": {"origin": "http://x.invalid",
                             "credential_variable": "WIRETAP_T2_KEY_X",
                             "credential_header": "authorization",
                             "row_names": ["row-x"]}}
        values, causes = run.resolve_route_credentials(route_map, env={})
        self.assertEqual(causes, [],
                         "an absent name no reachable route resolves to "
                         "changes nothing (rc unchanged)")
        self.assertEqual(values, {})

    def test_absent_reachable_name_blocks_the_case(self):
        tmp = tempfile.mkdtemp(prefix="rt-t3-unset-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        _t3_env_unset(self, "WIRETAP_T2_KEY_X")
        srv, upstream = _start_recording_upstream()
        self.addCleanup(srv.shutdown)
        live = _t2_write_cfg_home(tmp, T2_ORPHAN_CFG % {"a": upstream})
        fake_bin = _write_fake_binary(tmp, upstream, model="m-x")
        args, out = _t2_case_args(tmp, live, fake_bin)
        case = {"id": "RT-T3-UNSET", "title": "unset reachable credential",
                "driver": "headless", "model": "row-x",
                "steps": [{"op": "turn", "prompt": "ping"}],
                "wirecap": True, "est_calls": 1,
                "assert": {"exit": 0}}
        row = run.run_case(case, args, run.Budget(args.budget))
        self.assertEqual(row["status"], "BLOCKED",
                         "a reachable route whose credential is unset must "
                         "BLOCK that case before its first step")
        self.assertEqual(row.get("blocked_reason"),
                         "credential-unset: WIRETAP_T2_KEY_X")
        self.assertEqual([r for r in srv.requests if r["method"] == "POST"],
                         [], "nothing may be forwarded on an unset "
                             "credential (r3 §4.2.3)")

    def test_chain_row_all_unset_blocks_with_the_chain(self):
        route_map = {"m-chain": {"origin": "http://chain.invalid",
                                 "credential_variable": None,
                                 "credential_header": "authorization",
                                 "row_names": ["row-chain"]}}
        values, causes = run.resolve_route_credentials(route_map, env={})
        self.assertEqual(
            causes, ["credential-unset: XAI_API_KEY|GROK_CODE_XAI_API_KEY"
                     "|APEX_LLM_PROXY_KEY"],
            "only when NO name in the chain is set is the case blocked, and "
            "the cause names the whole chain (r3a A6)")
        self.assertEqual(values, {})


# ---------------------------------------------------------------------------
# End of T3 chunk 1.
# ---------------------------------------------------------------------------


class _T3EchoUpstreamHandler(_RecordingUpstreamHandler):
    """Records, then echoes the `authorization` it received into BOTH the
    response body and a response header, so the two capture directions of
    r3 §4.2.3 ("every injected value is secret") are separately decidable."""

    def do_POST(self):
        self._record("POST")
        with self.server.lock:
            auth = self.server.requests[-1]["headers"].get("authorization",
                                                           "")
        body = json.dumps({"ok": True, "echo": auth}).encode("utf-8")
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("x-echo-auth", auth or "none")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class _T3AuthErrorUpstreamHandler(_RecordingUpstreamHandler):
    """The 403 the run of record produced: `Unknown authorization
    credentials` with the credential echoed back in `error.param`, scheme word
    included (r3 §2.5). `echo_param` chooses what is echoed, so one handler
    serves the credential case and the placeholder case."""

    ECHO_NAME = "WIRETAP_T2_KEY_X"

    def do_POST(self):
        self._record("POST")
        param = "Bearer " + (os.environ.get(self.ECHO_NAME) or "")
        body = json.dumps({
            "error": {"message": "Unknown authorization credentials",
                      "type": "auth_error",
                      "code": "invalid_api_key",
                      "param": param}}).encode("utf-8")
        self.send_response(403)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def _t3_header(doc, name):
    """Look a captured header up case-insensitively.

    `mask_headers` masks the VALUE and keeps the spelling the sender used —
    that is frozen behaviour, pinned by wiretap's own `auth-masked-shape`
    check — so a reader must not assume a lower-cased key."""
    return next((v for k, v in (doc or {}).items() if k.lower() == name),
                None)


# ---------------------------------------------------------------------------
# RR-7 F-1 — value-free assertion renderings, and the guard that enforces them.
#
# Coordinator ruling `coordinator-ruling-RR-7-batch3-fix-20261031.json`,
# upholding RR-5 `counterfactual_hygiene`, makes this a standing rule over every
# assertion this batch owns: a failure may NAME a credential and print its
# digest, but may never print a credential VALUE, a header map or an environment
# map. The batch's own counterfactual run is the proof of why: CF-T3-20 installed
# a fault, the fault fired `RecheckRouteTest`'s ambient-key assertion, and that
# assertion's message published two bearer values into the run's named log.
#
# Why the fix is a rewrite and not a flag (measured, not assumed — see
# `batch-3-T3/t3f_unittest_render_probe.py` and `T3F-render-matrix.json`):
# unittest renders the OPERANDS of a failed assertion as well as the message, so
# handing a secret-bearing text to a plain `assert*` leaks even with
# `longMessage = False` (the diff path is not gated by it, and an assertion given
# no message leaks under every configuration). And a value the test itself
# formats into its own message survives EVERY configuration. So the message and
# the operands both have to stop naming values: that is what these helpers do.
#
# These are the only sanctioned renderings. `T3AssertionHygieneTest` at the end
# of this file parses this file and fails if an assertion in a T3-owned class
# renders anything else.
# ---------------------------------------------------------------------------

T3_HYGIENE_SOURCE = os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "test_run.py")

# A credential-shaped run inside any text a test could print: a scheme word plus
# a long token, the house `sk` shape (Agents.md §9, case-insensitive), or a
# JWT-shaped triplet.
T3_SECRET_SHAPE = re.compile(
    r"(?i)(?:(?:bearer|basic)\s+[A-Za-z0-9._~+/=-]{12,}"
    r"|sk[-_][a-z0-9_-]{16,}"
    r"|eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_.-]{8,})")


def _t3_render(value):
    """Canonical text form of a value, for DETECTING a needle inside it. Its
    result must never be placed in a failure message by itself."""
    if isinstance(value, str):
        return value
    return json.dumps(value, sort_keys=True, default=str)


def _t3_shape(value):
    """A value-free shape. For a mapping the KEYS are header/field NAMES, which
    is exactly what a failure is allowed to show."""
    if isinstance(value, dict):
        return "dict(keys=%s)" % sorted(str(k) for k in value)
    if isinstance(value, (list, tuple, set, frozenset)):
        return "%s(len=%d)" % (type(value).__name__, len(value))
    return type(value).__name__


def _t3_audit(value):
    """The only way a secret-bearing value may reach a failure message: its
    sha256:12, its length and a value-free shape (RR-7 F-1)."""
    text = _t3_render(value)
    return "sha256:12=%s len=%d shape=%s" % (run.sha256_12(text), len(text),
                                             _t3_shape(value))


def _t3_names(container):
    """A mapping or sequence as its sorted NAMES — safe for an env, header or
    argv container, whose VALUES are what must stay out of the message. A
    membership test against this projection asks the same question as one
    against the container, because membership is over keys/elements either way."""
    return sorted(str(k) for k in container)


def _t3_headers_audit(headers):
    """`name sha256:12=… len=…` per header: names plus digests, never a value
    and never a dict body (RR-7 F-1 arm (a))."""
    return " ".join("%s %s" % (str(k).lower(), _t3_audit(v))
                    for k, v in sorted((headers or {}).items()))


def _t3_argv_audit(argv):
    """argv with flags literal and every other element reduced to a digest.
    HYG-1 makes argv ps-visible, so a failure must not print it whole."""
    return [a if re.fullmatch(r"-{1,2}[\w.:-]+", str(a)) else _t3_audit(a)
            for a in argv]


def _t3_lines_digest(text):
    """The sha256:12 of every line of a secret-bearing text: enough to see that
    something is present without printing a single line."""
    if not isinstance(text, str):
        text = _t3_render(text)
    return sorted(run.sha256_12(ln) for ln in text.splitlines())


def _t3_secret_in(text, values):
    """Which of `values` occur in `text`. Detection is by value; the caller
    reports the NAME and the digest only."""
    return sorted(v for v in values if v and v in text)


def _t3_assert_absent(test, text, needles, where):
    """`needles` is a sequence of (NAME, value). Assert that no needle VALUE
    occurs in `text`; a failure names the credential and prints digests only, so
    the leak this catches cannot announce itself with a copy of the leak."""
    rendered = _t3_render(text)
    offenders = ["%s %s" % (n, _t3_audit(v)) for n, v in needles
                 if v and v in rendered]
    test.assertEqual(
        offenders, [],
        "%s: %d of %d swept values occurred in the text under test "
        "(%s); offenders: %s"
        % (where, len(offenders), len(needles), _t3_audit(rendered),
           "; ".join(offenders)))


def _t3_assert_present(test, text, name, value, where):
    """The mirror leg: one specific value IS expected (the excluded placeholder
    must survive the sweep), reported by name and digest only."""
    rendered = _t3_render(text)
    test.assertTrue(
        bool(value) and value in rendered,
        "%s: %s (%s) is expected in the text under test (%s)"
        % (where, name, _t3_audit(value), _t3_audit(rendered)))


def _t3_assert_value(test, actual, expected, where):
    """Compare two values by digest, length and shape, so a failure prints
    neither one (RR-7 F-1 arm (b)). Digest equality over the canonical text is
    value equality for these fixtures, and the shape leg distinguishes a dict
    from a string of the same length."""
    test.assertEqual(_t3_audit(actual), _t3_audit(expected),
                     "%s: the actual value must equal the expected one; "
                     "actual %s expected %s"
                     % (where, _t3_audit(actual), _t3_audit(expected)))


def _t3_assert_detail_shape(test, detail, where):
    """A sweep detail must be exactly `path:line: <pattern> <sha256_12>`. The
    shape IS the contract (§4.4.2), so assert it and report a non-conforming
    detail by digest: a detail that FAILS the shape is precisely one that may
    quote a fragment, so its repr is the last thing this message may contain."""
    test.assertTrue(
        re.fullmatch(r"[^:\s]+:\d+: [a-z0-9][a-z0-9_-]* [0-9a-f]{12}",
                     detail or "") is not None,
        "%s: a sweep detail must be `path:line: <pattern> <sha256_12>` and "
        "nothing else; got %s" % (where, _t3_audit(detail)))


def _t3_assert_values_differ(test, actual, expected, where):
    """The `assertNotEqual` twin of `_t3_assert_value`: the values must differ,
    reported by digest only."""
    test.assertNotEqual(_t3_audit(actual), _t3_audit(expected),
                        "%s: the value must not be the one under test (%s)"
                        % (where, _t3_audit(actual)))


class T3AssertionHygieneTest(unittest.TestCase):
    """RR-7 F-1 — the one Major of the batch-3 review, kept fixed by a test.

    An assertion in a T3-owned test must not be able to print a credential
    value, a header map or an environment map. Five legs, because the failure
    modes are independent:

      * `test_no_t3_assertion_renders_a_credential_value` — RR-7 F-1 arm (c) as
        a durable guard. It parses THIS file and reports every `self.assert*`
        inside a T3-owned class whose message or compared operand renders a
        value-bearing expression, unless that expression passes through a
        sanctioned reducer (`_t3_audit`, `_t3_names`, `_t3_headers_audit`,
        `_t3_argv_audit`, `_t3_lines_digest`, `_t3_shape`, `run.sha256_12`,
        `len`, `sorted`, …) or is a digest field of the mask triple.
      * `test_the_scan_sees_every_t3_class` — the scan cannot pass by seeing
        nothing (it fails loudly if a T3 class is renamed or dropped).
      * `test_a_failing_ambient_key_leg_prints_names_and_hashes_only` — the
        behavioural half of arm (a): force r3a A13's ambient-key assertion to
        fail in-process, exactly as CF-T3-20 forced it, and read the text
        unittest would have written to the log.
      * `test_a_routing_argv_leak_prints_names_and_hashes_only` — RR-10 F-8,
        the same rule on the T2 routing class RR-7 left outside this list.
      * `test_the_sanctioned_helpers_print_no_values` — the helpers are the
        rule's only escape hatch, so they are proven against a plant rather
        than trusted.

    On the bytes this batch STARTED from (`test_run.py`
    834aaa7c9f7be21c7494befb62b74b7399a31b8b723a5d68f9b0dad9f6fad4ec) the scan
    reported 55 hits and the forced-failure leg printed `authorization: Bearer
    <plant>` plus the header dict — see `tmp/T3F-scan-red.json` and
    `tmp/T3F-behavioural-red.json`."""

    #: every class the hygiene rule owns: T3's, from the first install through
    #: r9, plus this one, plus the T2 routing class ruling RR-10 F-8 puts under
    #: it (the last raw env-value assertion in the file lived there)
    T3_CLASSES = (
        "DeclaredRungCoverageTest", "ChildEnvAllowlistTest",
        "CredentialUnsetStateTest", "HeaderReplacementTest",
        "ResponseBodyScrubTest", "SweepLiteralSetTest",
        "SweepOutputNoFragmentTest", "SweepExitStatusTest",
        "SliceEvidenceSecretsTest", "LiteralCheckSetTest", "RecheckRouteTest",
        "AuthErrorClassSplitTest", "T3AssertionHygieneTest",
        "HermeticRoutingMapTest", "SignInCredentialRouteRowTest",
        "SignInCredentialFallbackTest", "SkShapeBoundaryTest",
        "CredentialSuffixLegTest",
    )

    # --- the predicate, mirroring batch-3-T3/t3f_scan.py exactly ------------
    VALUE_NAMES = frozenset({
        "joined", "texts", "body", "details", "argv", "env", "child",
        "masked", "echoed", "got", "plant", "token", "canary", "frag",
        "lines", "seen", "patterns", "elem", "d", "req", "head", "posts",
        "gets", "d1", "d2",
    })
    VALUE_ATTRS = frozenset({"AMBIENT_CANARY", "CHILD_OWN"})
    VALUE_FUNCS = frozenset({"_t3_env_plant"})
    RENDER_SAFE = frozenset({
        "_t3_audit", "_t3_names", "_t3_headers_audit", "_t3_argv_audit",
        "_t3_lines_digest", "_t3_shape", "sha256_12", "len", "sorted", "set",
        "tuple", "list", "bool", "abs", "min", "max", "fullmatch", "match",
        "search", "findall", "splitlines", "lower", "strip", "append", "add",
        "pop", "index", "getsource", "sorted_names",
    })
    DIGEST_FIELDS = frozenset({
        "sha256_12", "sha256", "len", "length", "masked", "span", "count",
        "hits", "mode", "status", "exit_code", "attempt", "present", "reason",
        "ok", "kind", "evidence", "detail", "recon",
    })
    SECRET_KEYS = frozenset({
        "headers", "authorization", "x-api-key", "x_api_key", "api_key",
        "apikey", "argv", "patterns", "environ", "env", "key", "credential",
        "params", "param", "frame", "body",
    })
    # Predicates whose first operand cannot appear in the failure text under
    # ANY configuration (measured in T3F-render-matrix.json): `assertIsNotNone`
        # fails only when the value IS None.
    NEVER_RENDERS_FIRST = ("assertIsNotNone", "assertRaises", "assertWarns",
                           "assertAlmostEqual", "assertNotAlmostEqual")
    UNSCORED_FIRST = ("assertTrue", "assertFalse", "assertRaises",
                      "assertWarns", "assertAlmostEqual")

    def _value_bearing(self, node, bound=frozenset()):
        """True when `node` can carry a credential value, a header map or an
        environment map into the text a failing assertion produces."""
        if isinstance(node, ast.Name):
            return node.id in self.VALUE_NAMES and node.id not in bound
        if isinstance(node, ast.Attribute):
            return node.attr in self.VALUE_ATTRS
        if isinstance(node, ast.Call):
            fname = node.func.attr if isinstance(node.func, ast.Attribute) \
                else (node.func.id if isinstance(node.func, ast.Name) else "")
            if fname in self.RENDER_SAFE:
                return False
            if fname in self.VALUE_FUNCS:
                return True
            if fname in ("get", "pop") and isinstance(node.func, ast.Attribute):
                base = self._value_bearing(node.func.value, bound)
                key = node.args[0] if node.args else None
                key_s = key.value if isinstance(key, ast.Constant) and \
                    isinstance(key.value, str) else None
                if key_s in self.SECRET_KEYS:
                    return True
                if key_s in self.CONTRACT_NAMES:
                    return False
                return base and (key_s is None
                                 or key_s not in self.DIGEST_FIELDS)
            if isinstance(node.func, ast.Attribute) and node.func.attr in (
                    "items", "values"):
                return self._value_bearing(node.func.value, bound)
            return any(self._value_bearing(a, bound) for a in node.args)
        if isinstance(node, ast.Subscript):
            key = node.slice
            key_s = key.value if isinstance(key, ast.Constant) and \
                isinstance(key.value, str) else None
            if key_s is not None:
                if key_s in self.SECRET_KEYS:
                    return True
                if key_s in self.DIGEST_FIELDS or key_s in self.CONTRACT_NAMES:
                    return False
            return self._value_bearing(node.value, bound)
        if isinstance(node, ast.JoinedStr):
            return any(self._value_bearing(v, bound) for v in node.values)
        if isinstance(node, ast.FormattedValue):
            return self._value_bearing(node.value, bound)
        if isinstance(node, ast.BinOp):
            return (self._value_bearing(node.left, bound)
                    or self._value_bearing(node.right, bound))
        if isinstance(node, (ast.Tuple, ast.List, ast.Set)):
            return any(self._value_bearing(e, bound) for e in node.elts)
        if isinstance(node, ast.Dict):
            return any(self._value_bearing(v, bound) for v in node.values)
        if isinstance(node, ast.Compare):
            return (self._value_bearing(node.left, bound)
                    or any(self._value_bearing(c, bound)
                           for c in node.comparators))
        if isinstance(node, ast.BoolOp):
            return any(self._value_bearing(v, bound) for v in node.values)
        if isinstance(node, ast.IfExp):
            return (self._value_bearing(node.body, bound)
                    or self._value_bearing(node.orelse, bound))
        if isinstance(node, ast.UnaryOp):
            return self._value_bearing(node.operand, bound)
        if isinstance(node, (ast.ListComp, ast.SetComp, ast.GeneratorExp)):
            names = set(bound)
            for gen in node.generators:
                names |= {t.id for t in ast.walk(gen.target)
                          if isinstance(t, ast.Name)}
            return self._value_bearing(node.elt, frozenset(names))
        if isinstance(node, ast.DictComp):
            names = set(bound)
            for gen in node.generators:
                names |= {t.id for t in ast.walk(gen.target)
                          if isinstance(t, ast.Name)}
            names |= {t.id for t in ast.walk(node.key)
                      if isinstance(t, ast.Name)} - names
            return self._value_bearing(node.value, frozenset(names))
        return False

    @property
    def CONTRACT_NAMES(self):
        """Names that cannot hold a credential, taken from the product's own
        contract (`run.CHILD_ENV_NON_CREDENTIAL_ALLOWLIST`, r3 §4.3.4's 15) so
        the exemption cannot drift from what the harness forwards."""
        return frozenset(run.CHILD_ENV_NON_CREDENTIAL_ALLOWLIST)

    def _kv_join_carriers(self, tree, owner):
        """Locals assigned from a key/value join, which carry that join's values
        into any later assertion that names them (RR-7 F-1(a)'s site builds
        `joined` one statement above the assert that prints it)."""
        out = set()

        def is_kv(node):
            for sub in ast.walk(node):
                if not isinstance(sub, ast.Call) or not isinstance(
                        sub.func, ast.Attribute) or sub.func.attr != "join":
                    continue
                for g in ast.walk(sub):
                    if isinstance(g, ast.BinOp) and isinstance(g.op, ast.Mod) \
                            and isinstance(g.left, ast.Constant) \
                            and isinstance(g.left.value, str) \
                            and "%" in g.left.value:
                        return True
                    if isinstance(g, ast.comprehension) and isinstance(
                            g.iter, ast.Call) \
                            and isinstance(g.iter.func, ast.Attribute) \
                            and g.iter.func.attr in ("items", "values"):
                        return True
            return False

        for node in ast.walk(tree):
            if owner(getattr(node, "lineno", -1)) is None:
                continue
            if isinstance(node, ast.Assign):
                names = [t.id for t in node.targets if isinstance(t, ast.Name)]
                if is_kv(node.value):
                    out.update(names)
            elif isinstance(node, (ast.ListComp, ast.GeneratorExp,
                                   ast.SetComp)) and is_kv(node):
                for gen in node.generators:
                    if isinstance(gen.target, ast.Name) \
                            and is_kv(gen.iter):
                        out.add(gen.target.id)
        return out

    def _scan(self):
        with open(T3_HYGIENE_SOURCE) as fh:
            src = fh.read()
        tree = ast.parse(src)
        lines = src.splitlines()
        ranges = {}
        for node in tree.body:
            if isinstance(node, ast.ClassDef) and node.name in self.T3_CLASSES:
                ranges[node.name] = (node.lineno, node.end_lineno)

        def owner(lineno):
            for name, (lo, hi) in ranges.items():
                if lo <= lineno <= hi:
                    return name
            return None

        kv_names = self._kv_join_carriers(tree, owner)
        reports = []
        for node in ast.walk(tree):
            who = owner(getattr(node, "lineno", -1))
            if who is None or not isinstance(node, ast.Call):
                continue
            fname = node.func.attr if isinstance(node.func, ast.Attribute) \
                else ""
            if not fname.startswith("assert"):
                continue
            scored = not any(fname.startswith(p)
                             for p in self.UNSCORED_FIRST)
            for idx, arg in enumerate(node.args):
                if any(fname.startswith(p)
                       for p in self.NEVER_RENDERS_FIRST) and idx == 0:
                    continue
                if scored and idx < 2:
                    role = "operand"
                elif idx == 0 and not scored:
                    continue
                else:
                    role = "message"
                carrier = next(
                    (n.id for n in ast.walk(arg)
                     if isinstance(n, ast.Name) and n.id in kv_names), None)
                if carrier is not None:
                    reports.append("%s:%d %s %s(P1) %s"
                                   % (who, node.lineno, fname, role, carrier))
                elif self._value_bearing(arg):
                    reports.append("%s:%d %s %s %s"
                                   % (who, node.lineno, fname, role,
                                      _t3_shape(arg)))
            for kw in node.keywords:
                if kw.arg not in (None, "msg"):
                    continue
                if self._value_bearing(kw.value):
                    reports.append("%s:%d %s message-kw %s"
                                   % (who, node.lineno, fname,
                                      _t3_shape(kw.value)))
        return ranges, sorted(set(reports)), src

    def test_the_scan_sees_every_t3_class(self):
        """Anti-blindness: a scan that saw no T3 class would trivially pass."""
        ranges, _reports, _src = self._scan()
        self.assertEqual(sorted(ranges), sorted(self.T3_CLASSES),
                         "the hygiene scan must cover every T3-owned class, "
                         "or it proves nothing")
        self.assertEqual(len(self.T3_CLASSES), 18)

    def test_no_t3_assertion_renders_a_credential_value(self):
        """RR-7 F-1 arm (c), kept as a test so the sweep cannot rot.

        A hit is one of the three shapes RR-7 names — a `"%s: %s"` join over a
        mapping, a `%r`/`str()` render of a headers/env/credential mapping, or a
        raw equality/membership assertion on a credential value — and each is
        fixed by asserting on NAMES plus sha256:12 instead."""
        _ranges, reports, _src = self._scan()
        self.assertEqual(reports, [],
                         "these assertions can print a credential value, a "
                         "header map or an env map (RR-7 F-1): %s"
                         % "; ".join(reports[:40]))

    def _run_suite_text(self, suite):
        """Run a suite in-process and hand back the text unittest would have
        written to the log."""
        stream = io.StringIO()
        result = unittest.TextTestRunner(stream=stream, verbosity=0).run(suite)
        return stream.getvalue(), result

    def test_a_failing_ambient_key_leg_prints_names_and_hashes_only(self):
        """RR-7 F-1 arm (a), behaviourally: when the ambient-key rule IS
        violated, the failure must stay legible without publishing a value.

        The violation is forced the way CF-T3-20 forced it — the route resolves
        to `None`, so the recheck authenticates with the ambient key — but in
        memory, by shadowing `run._route_for_model`, so no product byte moves."""
        real = run._route_for_model

        def blind(route_ctx, model):
            wire_id, _route = real(route_ctx, model)
            return wire_id, None

        run._route_for_model = blind
        self.addCleanup(setattr, run, "_route_for_model", real)
        _text, result = self._run_suite_text(unittest.TestSuite([
            RecheckRouteTest(
                "test_ambient_key_rides_only_when_it_is_the_routes_credential")
        ]))
        # The text is captured in a local, never written to disk: it is the
        # artifact RR-7 F-1(f) makes the lane redact, and writing a fresh copy
        # would re-create the leak this test exists to prevent.
        text = _text
        self.assertEqual(
            (len(result.failures), len(result.errors)), (1, 0),
            "the fault must make exactly the ambient-key leg fail, got %d "
            "failures and %d errors"
            % (len(result.failures), len(result.errors)))
        # `assertTrue`, not `assertIn`: the membership is over the text, and
        # handing that text to a rendering assert would print it on the very
        # leg that is checking it does not contain a value.
        self.assertTrue("authorization" in text,
                        "the failure must still name the header that carried "
                        "it: %s" % (_t3_audit(text),))
        self.assertTrue("sha256:12=" in text,
                        "and must show a value only as a digest: %s"
                        % (_t3_audit(text),))
        _t3_assert_absent(
            self, text,
            [("ambient-canary", RecheckRouteTest.AMBIENT_CANARY),
             ("route-credential",
              "synthetic-plant-wiretap_t2_key_x-value")],
            "the forced-failure text of the ambient-key leg")
        match = T3_SECRET_SHAPE.search(text)
        self.assertIsNone(
            match, "the failure text carries a credential-shaped run at span "
                   "%r" % (match.span() if match else None,))

    def test_a_routing_argv_leak_prints_names_and_hashes_only(self):
        """RR-10 F-8 — the same rule on the T2 routing class RR-7 left behind.

        `HermeticRoutingMapTest.test_map_reaches_wiretap_by_env_not_argv`
        asserted `assertNotIn(os.environ[k], joined, ...)`, and on this
        interpreter (3.14.7, measured in tmp/F8-assertnotin-render.json) that
        renders BOTH operands — `AssertionError: '<value>' unexpectedly found
        in '<container>'` — with or without a custom message. So the leg that
        existed to catch a credential in argv published one when it fired.

        The violation is forced the way a regression would look: a Wiretap
        whose argv carries the value the environment holds, shadowed in memory
        so no product byte moves. The failure must stay legible — it must name
        the variable and show a digest — without publishing the value it
        reports.
        """
        _t2_env_guard(self)
        real = run.Wiretap

        class ArgvCarryingWiretap(real):
            def __init__(self, *a, **kw):
                super().__init__(*a, **kw)
                self.argv = list(self.argv) + [
                    os.environ["WIRETAP_T2_KEY_X"]]

        run.Wiretap = ArgvCarryingWiretap
        self.addCleanup(setattr, run, "Wiretap", real)
        text, result = self._run_suite_text(unittest.TestSuite([
            HermeticRoutingMapTest(
                "test_map_reaches_wiretap_by_env_not_argv")
        ]))
        self.assertEqual(
            (len(result.failures), len(result.errors)), (1, 0),
            "the fault must make exactly the routing leg fail, got %d "
            "failures and %d errors"
            % (len(result.failures), len(result.errors)))
        _t3_assert_absent(
            self, text,
            [(k, _t2_env_plant(k)) for k in T2_TEST_ONLY_KEYS],
            "the forced-failure text of the routing argv leg")
        # `assertTrue`, not `assertIn`: handing that text to a rendering assert
        # would print it on the very leg that is checking it (the RR-7 F-1
        # lesson this class exists to enforce).
        self.assertTrue("WIRETAP_T2_KEY_X" in text,
                        "the failure must still name the variable whose value "
                        "leaked: %s" % (_t3_audit(text),))
        self.assertTrue("sha256:12=" in text,
                        "and must show a value only as a digest: %s"
                        % (_t3_audit(text),))
        match = T3_SECRET_SHAPE.search(text)
        self.assertIsNone(
            match, "the failure text carries a credential-shaped run at span "
                   "%r" % (match.span() if match else None,))

    def test_the_sanctioned_helpers_print_no_values(self):
        """The reducers are the rule's only escape hatch, so they are proven
        against a plant rather than trusted."""
        plant = _t3_env_plant("APEX_LLM_PROXY_KEY")
        canary = RecheckRouteTest.AMBIENT_CANARY
        audit = _t3_audit(plant)
        headers = _t3_headers_audit({"authorization": "Bearer " + plant,
                                     "x-api-key": canary,
                                     "host": "127.0.0.1:1"})
        # The reducer's OUTPUT is a different thing from argv, and naming it
        # `_audit` is the discipline the rule enforces: only an audited
        # rendering may reach an assertion from here on.
        argv_audit = _t3_argv_audit(["-rlF", "-f", "/tmp/p", canary])
        blob = " ".join([audit, headers, repr(argv_audit),
                         repr(_t3_names({"authorization": canary})),
                         repr(_t3_lines_digest("line " + plant)),
                         repr(_t3_shape({"authorization": canary}))])
        self.assertIn("sha256:12=", blob, "the digest is the whole point")
        self.assertIn("authorization", blob, "and the NAME survives")
        self.assertIn("-rlF", repr(argv_audit),
                      "a flag stays literal so argv is still legible")
        _t3_assert_absent(self, blob,
                          [("proxy-key", plant), ("ambient-canary", canary)],
                          "the sanctioned renderings")
        match = T3_SECRET_SHAPE.search(blob)
        self.assertIsNone(
            match, "a sanctioned rendering printed a credential-shaped run at "
                   "span %r" % (match.span() if match else None,))


def _t3_capture_texts(wire_dir):
    """Every byte of every capture file in a run's wire dir, as one string."""
    out = []
    for name in sorted(os.listdir(wire_dir)):
        with open(os.path.join(wire_dir, name), errors="replace") as fh:
            out.append(fh.read())
    return "\n".join(out)


class HeaderReplacementTest(unittest.TestCase):
    """r3 §4.2.3 + §4.6 row 5, on the binding harness (r3a A12): the fake
    upstream, `run_case` with `wirecap: True`, the REAL wiretap process.

    RED on the frozen bytes: the wiretap forwards the child's own
    `authorization` verbatim (`wiretap.py:416` copies `self.headers`) and
    masks only `authorization` in the REQUEST direction plus the ambient key
    by value — so the echoed credential lands in `resp-001.jsonl` as
    captured text.

    The child's own header is a literal string in the fixture binary rather
    than an environment read: after T3 the child env is an allowlist, so an
    arbitrary name would not be present in it and the test could not tell
    "forwarded the child's own" from "replaced with the route's" (fixture
    extension recorded in the item, approval F5's precedent)."""

    CHILD_OWN = "Bearer synthetic-child-own-plant-value"

    def _run(self, tag):
        tmp = tempfile.mkdtemp(prefix="rt-t3-hdr-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        srv, upstream = _start_recording_upstream(_T3EchoUpstreamHandler)
        self.addCleanup(srv.shutdown)
        live = _t2_write_cfg_home(tmp, T2_ORPHAN_CFG % {"a": upstream})
        fake_bin = _write_fake_binary(tmp, upstream, model="m-x",
                                      auth_literal=self.CHILD_OWN)
        args, out = _t2_case_args(tmp, live, fake_bin)
        case = {"id": "RT-T3-HDR-" + tag, "title": "credential replacement",
                "driver": "headless", "model": "row-x",
                "steps": [{"op": "turn", "prompt": "ping"}],
                "wirecap": True, "est_calls": 1,
                "assert": {"exit": 0}}
        row = run.run_case(case, args, run.Budget(args.budget))
        wire_dir = os.path.join(out, "rt-t3-hdr-" + tag, "wire")
        plant = "synthetic-plant-wiretap_t2_key_x-value"
        return row, srv, wire_dir, plant

    def test_route_credential_replaces_the_childs_own_header(self):
        row, srv, wire_dir, plant = self._run("repl")
        posts = [r for r in srv.requests if r["method"] == "POST"]
        self.assertEqual(len(posts), 1, "the route exists, so the request "
                                        "must be forwarded once")
        got = posts[0]["headers"].get("authorization", "")
        _t3_assert_value(self, got, "Bearer " + plant,
                         "the route's own credential must be what "
                         "reaches the origin (client.rs:1743 insert "
                         "is the precedent)")
        _t3_assert_absent(self, got,
                          [("child-own",
                            "synthetic-child-own-plant-value")],
                          "the child's own credential is never "
                          "forwarded")
        self.assertEqual(row["status"], "PASS")

    def test_injected_value_is_masked_in_both_capture_directions(self):
        _row, _srv, wire_dir, plant = self._run("mask")
        req = json.load(open(os.path.join(wire_dir, "req-001.json")))
        masked = _t3_header(req["headers"], "authorization")
        self.assertTrue(isinstance(masked, dict),
                        "the request direction must mask the value "
                        "(a dict triple, not a raw header): %s"
                        % (_t3_audit(masked),))
        _t3_assert_value(
            self, masked,
            {"masked": True,
             "sha256_12": run.sha256_12(self.CHILD_OWN),
             "len": len(self.CHILD_OWN)},
            "the mask triple is {masked, sha256_12, len}")
        resp_path = os.path.join(wire_dir, "resp-001.jsonl")
        head = json.loads(open(resp_path).readline())
        echoed = _t3_header(head["headers"], "x-echo-auth")
        _t3_assert_values_differ(
            self, echoed, "Bearer " + plant,
            "the response direction must mask the value the wiretap "
            "itself injected")
        self.assertTrue(isinstance(echoed, dict),
                        "the response header is masked with the same "
                        "triple: %s" % (_t3_audit(echoed),))
        self.assertEqual(echoed["sha256_12"], run.sha256_12("Bearer "
                                                           + plant))
        joined = _t3_capture_texts(wire_dir)
        _t3_assert_absent(self, joined,
                          [("wiretap-t2-key-x", plant)],
                          "no capture file may carry the credential")


class ResponseBodyScrubTest(unittest.TestCase):
    """r3 §4.4.1's write-time scrub, on the binding harness (r3a A12).
    RED on the frozen bytes: `Capture.append_frame` writes the frame text
    verbatim (`wiretap.py:194-197`)."""

    def _run_403(self, tag, cfg, echo_name):
        tmp = tempfile.mkdtemp(prefix="rt-t3-scrub-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        _t3_swept_env_guard(self)
        handler = type("H", (_T3AuthErrorUpstreamHandler,),
                       {"ECHO_NAME": echo_name})
        srv, upstream = _start_recording_upstream(handler)
        self.addCleanup(srv.shutdown)
        live = _t2_write_cfg_home(tmp, cfg % {"a": upstream})
        fake_bin = _write_fake_binary(tmp, upstream, model="m-x")
        args, out = _t2_case_args(tmp, live, fake_bin)
        case = {"id": "RT-T3-SCRUB-" + tag, "title": "response scrub",
                "driver": "headless", "model": "row-x",
                "steps": [{"op": "turn", "prompt": "ping"}],
                "wirecap": True, "est_calls": 1,
                "assert": {"exit": 1}}
        row = run.run_case(case, args, run.Budget(args.budget))
        wire_dir = os.path.join(out, "rt-t3-scrub-" + tag, "wire")
        return row, wire_dir

    def test_bare_credential_in_a_403_body_is_masked_and_the_rest_kept(self):
        plant = "synthetic-plant-wiretap_t2_key_x-value"
        row, wire_dir = self._run_403("cred", T2_ORPHAN_CFG,
                                      "WIRETAP_T2_KEY_X")
        self.assertEqual(row["status"], "PASS")
        joined = _t3_capture_texts(wire_dir)
        _t3_assert_absent(self, joined,
                          [("wiretap-t2-key-x", plant)],
                          "the echoed credential must not survive "
                          "into resp-001.jsonl (r3 §4.4.1 rule 1: "
                          "the match is on the BARE content, scheme "
                          "word stripped)")
        frames = [json.loads(l) for l in
                  open(os.path.join(wire_dir, "resp-001.jsonl"))
                  if l.strip()]
        body = None
        for f in frames:
            if "frame" in f:
                body = json.loads(f["frame"])
                break
        self.assertIsNotNone(body, "no response frame was captured")
        err = body["error"]
        self.assertEqual(err["message"], "Unknown authorization credentials",
                         "the message is what the cases assert on and must "
                         "survive byte-for-byte")
        self.assertEqual(err["type"], "auth_error")
        self.assertEqual(err["code"], "invalid_api_key")
        _t3_assert_value(
            self, err["param"],
            {"masked": True,
             "sha256_12": run.sha256_12("Bearer " + plant),
             "len": len("Bearer " + plant)},
            "an auth_error's error.param is replaced whole "
            "(§4.4.1 rule 3)")

    def test_placeholder_value_is_not_scrubbed(self):
        """R2-4: the documented placeholder is passed to the route that needs
        it and matched by NEITHER the scrub nor the sweep."""
        plant = _t3_env_plant("GROK_DIRECT_PLACEHOLDER_KEY")
        cfg = '''base_url = "%(a)s/v1"
models_base_url = "%(a)s/v1"
[features]
turn_summary = false

[model."row-x"]
model = "m-x"
base_url = "%(a)s/v1"
api_backend = "responses"
env_key = "GROK_DIRECT_PLACEHOLDER_KEY"
'''
        row, wire_dir = self._run_403("ph", cfg,
                                      "GROK_DIRECT_PLACEHOLDER_KEY")
        self.assertEqual(row["status"], "PASS")
        joined = _t3_capture_texts(wire_dir)
        _t3_assert_present(self, joined,
                           "GROK_DIRECT_PLACEHOLDER_KEY-plant", plant,
                           "a placeholder value scrubbed out of the "
                           "capture would make every real run's "
                           "sweep unpassable (R2-4)")


class SweepLiteralSetTest(unittest.TestCase):
    """r3 §4.4.1's backstop + r3a A5's one swept set.
    RED on the frozen bytes: `redaction_sweep(dir, "")` counts only the
    ambient key and `KEY_HEURISTIC`, so a second declared key, a bare JWT and
    a bare `Basic` blob are all missed."""

    def _dir(self, name, *texts):
        root = tempfile.mkdtemp(prefix="rt-t3-sweep-")
        self.addCleanup(lambda: shutil.rmtree(root, ignore_errors=True))
        for i, text in enumerate(texts):
            with open(os.path.join(root, "resp-%03d.jsonl" % i), "w") as fh:
                fh.write(text + "\n")
        home = os.path.join(root, "home")
        os.makedirs(home, exist_ok=True)
        with open(os.path.join(home, "config.toml"), "w") as fh:
            fh.write("")
        return root

    def test_every_declared_shape_is_hit(self):
        _t3_swept_env_guard(self)
        plants = [name for name in T3_SWEPT_PLANTS
                  if name not in T3_SWEEP_EXCLUDED]
        texts = [_t3_env_plant(n) for n in plants]
        texts += ["Basic " + _t3_env_plant("GROK_DIRECT_ONPREM_AUTH"),
                  "Bearer " + _t3_env_plant("APEX_LLM_PROXY_KEY"),
                  "SK_" + "PLANTCASEINSENSITIVE0123456789ABCDEF"]
        root = self._dir("shapes", *texts)
        hits, details = run.redaction_sweep(root, "")
        self.assertGreaterEqual(hits, len(plants) + 1,
                                "every swept value (scheme-stripped) "
                                "plus the case-insensitive sk shape "
                                "must hit; hits=%d details=%s"
                                % (hits, _t3_audit(details)))
        names = set(d.split(": ", 1)[1].split()[0] for d in details)
        self.assertTrue(any("literal" in n for n in names),
                        "the exact-content match must be reported: "
                        "%s" % (_t3_audit(details),))
        joined = "\n".join(details)
        for n in plants:
            _t3_assert_absent(self, joined, [(n, _t3_env_plant(n))],
                              "a hit is never quoted (§4.4.2)")

    def test_bare_shapes_are_hit_without_any_declared_value(self):
        _t3_swept_env_guard(self, unset=tuple(T3_SWEPT_PLANTS))
        root = self._dir(
            "shapes-only",
            "eyJSaWduZWQiOiJwbGFudCJ9.c2lnbmF0dXJlcGFydA.cGxhbnR0aGlyZHBhcnQ",
            'some prefix {"error":{"param":"'
            'cGxhaW50YmFzZTY0YmxvYnN0YW5kaW5naW5wYXJhbQ==","message":'
            '"x"}} some suffix',
            "sk_" + "plantskformwithmorethansixteenchars")
        hits, details = run.redaction_sweep(root, "")
        self.assertGreaterEqual(hits, 3,
                                "the JWT shape, the standalone >= "
                                "40-char base64 in error.param and "
                                "the sk_ form must each hit with no "
                                "declared value set at all: %s"
                                % (_t3_audit(details),))

    def test_uppercase_sk_form_is_hit_case_insensitively(self):
        """§4.4.1 rule 2 / Agents.md §9: the `sk_`/`sk-` shape is matched
        CASE-INSENSITIVELY.

        Every declared value is UNSET and the directory holds exactly one
        token, so `hits == 1` can only come from the shape rule and the
        pattern name says which one. A count-only assertion elsewhere cannot
        show the rule is live: with a case-sensitive `SK_SHAPE` and no
        IGNORECASE on the runner's sweep, an uppercase underscore form is
        missed by `SK_SHAPE` AND by `KEY_HEURISTIC` (whose sk alternative is
        the dash form only), and the run still exits 0 on a live key."""
        _t3_swept_env_guard(self, unset=tuple(T3_SWEPT_PLANTS))
        token = "SK_" + "PLANTCASEINSENSITIVE0123456789ABCDEF"
        root = self._dir("skcase", "echoed param %s in a 403 body" % token)
        hits, details = run.redaction_sweep(root, "")
        joined = "\n".join(details)
        self.assertEqual(hits, 1,
                         "exactly one shape hit is possible here: %s"
                         % (_t3_audit(details),))
        _t3_assert_present(self, joined, "sk-shape", "sk-shape",
                           "the uppercase form must hit the same "
                           "rule")
        _t3_assert_absent(self, joined,
                          [("SK_-shape-plant", token)],
                          "a hit is never quoted (§4.4.2)")

    def test_placeholder_value_alone_gives_no_hit(self):
        """The R2-4 assertion: a directory holding only the placeholder value
        must sweep 0, or no real report home can ever pass."""
        _t3_swept_env_guard(self)
        plant = _t3_env_plant("GROK_DIRECT_PLACEHOLDER_KEY")
        root = self._dir("ph", plant, "Bearer " + plant)
        hits, details = run.redaction_sweep(root, "")
        _t3_assert_value(self, (hits, details), (0, []),
                         "the placeholder is excluded from the swept "
                         "set")

    def test_unset_optional_rung_is_absent_not_a_failure(self):
        _t3_swept_env_guard(self, unset=("GROK_CODE_XAI_API_KEY",))
        # A short, shape-free stand-in: this file must sweep 0 because the
        # name is UNSET (rule 1 has no pattern for it), and nothing here may
        # be a 4.4.1 bare shape — that would hit through rule 2 and prove
        # nothing about the unset rung.
        root = self._dir("legacy", "legacycodekey-plant-standin")
        hits, details = run.redaction_sweep(root, "")
        _t3_assert_value(self, (hits, details), (0, []),
                         "an unset name has nothing to match and is "
                         "not a failure (r3a A5)")
        meta = run.credential_env_meta(list(T3_RUNG_UNION))
        self.assertEqual(meta["GROK_CODE_XAI_API_KEY"], "ABSENT")


class SweepOutputNoFragmentTest(unittest.TestCase):
    """§4.4.2: today `redaction_sweep` appends `"key-heuristic %r" %
    frag[:40]`, which writes up to 40 characters of a live key into
    `report.json` — and `write_report` copies `sweep_details` into that very
    file, so a detail that carries no fragment is what keeps report.md and
    report.json clean."""

    def test_details_carry_path_line_digest_and_pattern_only(self):
        _t3_swept_env_guard(self, unset=tuple(T3_SWEPT_PLANTS))
        plant = "sk-" + "x" * 12 + "-plaintext-plant-fragment"
        root = tempfile.mkdtemp(prefix="rt-t3-nofrag-")
        self.addCleanup(lambda: shutil.rmtree(root, ignore_errors=True))
        with open(os.path.join(root, "resp-001.jsonl"), "w") as fh:
            fh.write('{"frame": "%s"}\n' % plant)
        hits, details = run.redaction_sweep(root, "")
        self.assertGreaterEqual(hits, 1, "the planted sk- form must hit")
        joined = "\n".join(details)
        _t3_assert_absent(self, joined, [("sk-plant", plant)],
                          "the no-fragment sweep details")
        _t3_assert_absent(self, joined, [("sk-fragment", "x" * 12)],
                          "no prefix, suffix or slice of a secret "
                          "may be quoted (the frozen code writes "
                          "frag[:40])")
        for d in details:
            _t3_assert_detail_shape(self, d,
                                    "the no-fragment detail shape")


class SweepExitStatusTest(unittest.TestCase):
    """§4.4.3 + r3a A7 + r3b B8. `main()` needs a roster and a real binary,
    so the offline authority pins the two functions it must call and their
    ORDER in main's own source, and the fixtures drive the functions."""

    VERDICT_KEYS = {"status", "exit_code", "sweep_hits", "rows_total",
                    "rows_pass", "rows_fail", "rows_blocked"}

    def _rows(self, *statuses):
        return [{"status": s, "id": "C%d" % i, "run_dir": None}
                for i, s in enumerate(statuses)]

    def test_planted_hit_sets_the_exit_code_and_the_persisted_verdict(self):
        tmp = tempfile.mkdtemp(prefix="rt-t3-exit-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        self.assertEqual(run.run_verdict_code(bad=0, sweep_hits=1), 1,
                         "a run that leaks a credential may not exit 0")
        run.persist_run_verdict(tmp, self._rows("PASS"), sweep_hits=1,
                               code=1)
        path = os.path.join(tmp, "run-verdict.json")
        doc = json.load(open(path))
        self.assertEqual(set(doc), self.VERDICT_KEYS,
                         "r3b B8: the persisted verdict has a fixed key set "
                         "and carries no captured text")
        self.assertEqual(doc["exit_code"], 1)
        self.assertEqual(doc["sweep_hits"], 1)
        self.assertEqual(doc["status"], "FAIL")
        self.assertEqual(doc["rows_pass"], 1)

    def test_a_clean_dir_returns_the_row_derived_code_unchanged(self):
        self.assertEqual(run.run_verdict_code(bad=0, sweep_hits=0), 0)
        self.assertEqual(run.run_verdict_code(bad=1, sweep_hits=0), 1,
                         "the sweep may add a failure, never mask one")
        self.assertEqual(run.run_verdict_code(bad=1, sweep_hits=3), 1)

    def test_a_hit_only_in_report_md_is_counted_and_persisted(self):
        tmp = tempfile.mkdtemp(prefix="rt-t3-exit-rep-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t3_swept_env_guard(self)
        out = os.path.join(tmp, "report")
        os.makedirs(out)
        with open(os.path.join(out, "report.md"), "w") as fh:
            fh.write("# report\n%s\n" % _t3_env_plant("APEX_LLM_PROXY_KEY"))
        run_dir_hits, _d1 = run.redaction_sweep(tmp, "")
        rep_hits, _d2 = run.redaction_sweep(tmp, "",
                                            files=("report.md", "report.json"))
        self.assertEqual(run_dir_hits, rep_hits,
                         "the leak is in the report file, which the run-dir "
                         "pass already sees only because both live here")
        self.assertGreaterEqual(rep_hits, 1)
        run.persist_run_verdict(out, self._rows("PASS"),
                                sweep_hits=run_dir_hits + rep_hits, code=1)
        doc = json.load(open(os.path.join(out, "run-verdict.json")))
        self.assertEqual(set(doc), self.VERDICT_KEYS)
        self.assertGreater(doc["sweep_hits"], 0,
                           "a verdict written at write_report time would "
                           "record 0 for exactly the --daemon caller that "
                           "cannot see $? (r3a A7)")

    def test_placeholder_only_run_dir_keeps_the_row_derived_code(self):
        tmp = tempfile.mkdtemp(prefix="rt-t3-exit-ph-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t3_swept_env_guard(self)
        plant = _t3_env_plant("GROK_DIRECT_PLACEHOLDER_KEY")
        wire = os.path.join(tmp, "wire")
        home = os.path.join(tmp, "home")
        os.makedirs(wire)
        os.makedirs(home)
        with open(os.path.join(wire, "resp-001.jsonl"), "w") as fh:
            fh.write('{"frame": "%s"}\n' % plant)
        with open(os.path.join(home, "config.toml"), "w") as fh:
            fh.write("env_key = %r\n" % plant)
        hits, details = run.redaction_sweep(tmp, "")
        _t3_assert_value(self, (hits, details), (0, []),
                         "captures AND homes holding only the "
                         "placeholder sweep 0 (R2-4, r3a A's fourth "
                         "fixture)")
        self.assertEqual(run.run_verdict_code(bad=0, sweep_hits=hits), 0)

    def test_main_wires_both_and_persists_after_the_report_sweep(self):
        src = __import__("inspect").getsource(run.main)
        for name in ("run_verdict_code", "persist_run_verdict",
                     "redaction_sweep", "write_report"):
            self.assertIn(name, run.main.__code__.co_names,
                          "main() must call %s (r3 §4.4.3)" % name)
        i_report = src.index("write_report(")
        i_sweep = src.index("redaction_sweep(", i_report)
        i_persist = src.index("persist_run_verdict(", i_sweep)
        self.assertLess(i_report, i_persist,
                        "the verdict must be persisted AFTER write_report")
        self.assertLess(i_sweep, i_persist,
                        "and after the report-only sweep (r3a A7)")

    def test_the_verdict_call_site_carries_the_hit_count(self):
        """§4.4.3's third leg, added at r9 after CF-T3-15 survived.

        The test above pins WHICH functions `main()` calls and their order;
        `run_verdict_code(bad=len(bad))` — the accumulated hit count dropped
        — satisfies all of it and still exits 0 on a run whose sweep found a
        leaked credential. The helper itself is pinned by the fixtures, so
        the argument at the call site is the only part left to pin, and this
        class is where the file pins main() (it needs a roster and a real
        binary to be driven)."""
        ast = __import__("ast")
        tree = ast.parse(__import__("inspect").getsource(run.main))
        calls = [n for n in ast.walk(tree)
                 if isinstance(n, ast.Call)
                 and getattr(n.func, "id", None) == "run_verdict_code"]
        self.assertEqual(len(calls), 1,
                         "main() must derive its exit code from "
                         "run_verdict_code exactly once")
        kwargs = {k.arg: k for k in calls[0].keywords}
        self.assertIn("sweep_hits", kwargs,
                      "the exit code must see the hit count; the call site "
                      "passes %r" % (sorted(kwargs),))
        self.assertIsInstance(kwargs["sweep_hits"].value, ast.Name,
                              "the call site must pass the accumulated "
                              "count, not a constant")


class SliceEvidenceSecretsTest(unittest.TestCase):
    """§4.4.4: `slice_evidence` is called with `secrets=(args.ambient_key,)`,
    so any non-ambient credential inside a FAIL row's slice rides into
    report.md untouched. RED on the frozen bytes: the default `secrets=()`
    redacts nothing at all."""

    def test_non_ambient_credential_is_redacted_from_the_slice(self):
        _t3_swept_env_guard(self)
        plant = _t3_env_plant("CODEX_TRUE_FOUNDRY_KEY")
        tmp = tempfile.mkdtemp(prefix="rt-t3-slice-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        with open(os.path.join(tmp, "acp.log"), "w") as fh:
            fh.write("line one\nrequest failed with param %s\nend\n" % plant)
        row = {"run_dir": tmp,
               "asserts": [{"ok": False, "recon": False, "kind": "wire",
                            "detail": "no match", "evidence": "acp.log"}]}
        lines = run.slice_evidence(row)
        joined = "\n".join(lines)
        self.assertTrue(joined, "the slice must still be produced")
        _t3_assert_present(self, joined, "REDACTED-marker",
                           "***REDACTED***", "the redacted slice")
        _t3_assert_absent(self, joined,
                          [("codex-true-foundry-key", plant)],
                          "the whole swept set, not just the ambient "
                          "key, is what `redact` must replace "
                          "(§4.4.4)")

    def test_the_report_call_site_passes_the_swept_set(self):
        """§4.4.4's other half: the CALL SITE.

        r3 §8 T3 names `slice_evidence` *called with
        `secrets=(args.ambient_key,)`* in its Files cell, and §4.4.4's table
        row is the call site, not the signature. A test that only calls
        `slice_evidence` cannot tell a widened default from a call site that
        still overrides it, so this drives `write_report` and reads
        `report.md` — the artifact the review of record names as the leak."""
        _t3_swept_env_guard(self)
        plant = _t3_env_plant("CODEX_TRUE_FOUNDRY_KEY")
        out = tempfile.mkdtemp(prefix="rt-t3-report-")
        self.addCleanup(lambda: shutil.rmtree(out, ignore_errors=True))
        run_dir = tempfile.mkdtemp(prefix="rt-t3-report-row-")
        self.addCleanup(lambda: shutil.rmtree(run_dir, ignore_errors=True))
        with open(os.path.join(run_dir, "acp.log"), "w") as fh:
            fh.write("step failed: param %s was echoed back\n" % plant)
        rows = [{"id": "R1", "title": "t", "status": "FAIL",
                 "duration_s": 1.0, "model_calls": 1, "turns": [],
                 "asserts": [{"ok": False, "recon": False, "kind": "log",
                              "detail": "boom", "evidence": "acp.log"}],
                 "snapshots": [], "run_dir": run_dir}]
        args = argparse.Namespace(live_home="/x", wirecap=False, budget=1,
                                  ambient_key="")
        env_meta = {"ts": "t", "bin": "b", "bin_sha": "s", "git": "g",
                    "key_sha": "k", "upstream": "u"}
        run.write_report(out, rows, env_meta, args, 0, [])
        with open(os.path.join(out, "report.md")) as fh:
            body = fh.read()
        _t3_assert_present(self, body, "slice-head", "step failed",
                           "the slice must still be rendered")
        _t3_assert_absent(self, body,
                          [("codex-true-foundry-key", plant)],
                          "report.md carried a declared credential "
                          "value out of a FAIL row's slice")
        _t3_assert_present(self, body, "REDACTED-marker",
                           "***REDACTED***", "report.md")


class LiteralCheckSetTest(unittest.TestCase):
    """§4.4.5 + r3a A5. The literal check belongs to the post-report sweep;
    its patterns reach `grep` through a FILE, never argv, and it uses the one
    swept set."""

    def _check(self, root):
        _t3_swept_env_guard(self)
        return run.literal_check(root)

    def test_placeholder_only_home_gives_zero_hits(self):
        tmp = tempfile.mkdtemp(prefix="rt-t3-lit-ph-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t3_swept_env_guard(self)
        plant = _t3_env_plant("GROK_DIRECT_PLACEHOLDER_KEY")
        for name in ("report.md", "report.json"):
            with open(os.path.join(tmp, name), "w") as fh:
                fh.write(plant + "\n")
        hits, details = run.literal_check(tmp)
        _t3_assert_value(self, (hits, details), (0, []),
                         "the placeholder must never be a literal "
                         "hit")

    def test_planted_declared_value_gives_a_hit(self):
        tmp = tempfile.mkdtemp(prefix="rt-t3-lit-hit-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t3_swept_env_guard(self)
        with open(os.path.join(tmp, "report.md"), "w") as fh:
            fh.write("echoed: %s\n" % _t3_env_plant("APEX_LLM_PROXY_KEY"))
        hits, details = self._check(tmp)
        self.assertGreaterEqual(hits, 1,
                                "a planted declared credential must "
                                "hit: %s" % (_t3_audit(details),))
        _t3_assert_absent(self, details,
                          [("APEX_LLM_PROXY_KEY",
                            _t3_env_plant("APEX_LLM_PROXY_KEY"))],
                          "the literal check must not quote the value"
                          " it matched")

    def test_unset_name_is_skipped_not_fatal(self):
        tmp = tempfile.mkdtemp(prefix="rt-t3-lit-unset-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t3_swept_env_guard(self, unset=("GROK_CODE_XAI_API_KEY",))
        with open(os.path.join(tmp, "report.md"), "w") as fh:
            fh.write("harmless\n")
        hits, details = self._check(tmp)
        self.assertEqual(hits, 0)

    def test_patterns_reach_the_check_through_a_file(self):
        tmp = tempfile.mkdtemp(prefix="rt-t3-lit-argv-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t3_swept_env_guard(self)
        with open(os.path.join(tmp, "report.md"), "w") as fh:
            fh.write("echoed: %s\n" % _t3_env_plant("APEX_LLM_PROXY_KEY"))
        seen = {}
        real = run.GREP_RUNNER

        def spy(argv, *a, **kw):
            seen["argv"] = list(argv)
            if "-f" in argv:
                live = argv[argv.index("-f") + 1]
                with open(live) as fh:      # grep has not run yet, so the
                    seen["patterns"] = fh.read()   # file still exists
                seen["mode"] = os.stat(live).st_mode & 0o777
            return real(argv, *a, **kw)
        run.GREP_RUNNER = spy
        self.addCleanup(lambda: setattr(run, "GREP_RUNNER", real))
        hits, _details = run.literal_check(tmp)
        self.assertGreaterEqual(hits, 1)
        argv = seen["argv"]
        plant = _t3_env_plant("APEX_LLM_PROXY_KEY")
        self.assertNotIn("-e", _t3_names(argv),
                         "no pattern may be an argv element")
        for elem in argv:
            _t3_assert_absent(self, elem,
                              [("APEX_LLM_PROXY_KEY", plant)],
                              "a credential value must never be "
                              "argv-visible (HYG-1: argv is "
                              "ps-visible)")
        self.assertIn("-f", _t3_names(argv),
                        "the patterns come from a file")
        _t3_assert_present(self, seen["patterns"],
                           "APEX_LLM_PROXY_KEY-plant", plant,
                           "the file is where the patterns live")
        self.assertEqual(seen["mode"], 0o600,
                         "the patterns file is private (0600)")

# ---------------------------------------------------------------------------
# T3 chunk 3 — the route-aware retry recheck (r3a A13 + r3b B7) and the
# auth-failure class split (r3 §4.5 item 3, §4.6 row 12, R-12).
# Offline: the fake upstream is the only transport (r3a A12); every fixture
# row declares a TEST-ONLY env_key under the existing env guard, and no
# fallback-chain name is read by an offline test (r3c seat-1 F4).
# ---------------------------------------------------------------------------

class _T3RecheckUpstreamHandler(_RecordingUpstreamHandler):
    """The recorder, plus the two shapes the retry loop needs: the first POST
    answers `flap_status` (so `retry.on_status` matches and the loop reaches
    the recheck), and `GET /v1/models` answers with `catalog_ids` — or with
    `catalog_status`, which is the r3a A13 "route gives no readable model
    list" case.

    It records every request INCLUDING the recheck GET, because A13's third
    leg ("neither request carries the ambient key") is only decidable by
    looking at what the origin actually received."""

    flap_status = 404
    catalog_status = 200
    catalog_ids = ("m-x",)

    def do_POST(self):
        self._record("POST")
        with self.server.lock:
            self.server.posts += 1
            n = self.server.posts
        if n == 1:
            self._send(self.flap_status, {"error": "model not found (flap)"})
            return
        self._send(200, {"ok": True, "post": n})

    def do_GET(self):
        self._record("GET")
        if self.path.startswith("/v1/models"):
            if self.catalog_status != 200:
                self._send(self.catalog_status, {"error": "no catalog here"})
                return
            self._send(200, {"data": [{"id": i} for i in self.catalog_ids]})
            return
        self._send(404, {"error": "no route"})


def _start_t3_recheck_upstream(handler=None):
    """`_start_recording_upstream` for the recheck fixture: the same request
    log, plus the POST counter the flap handler needs."""
    import http.server
    import threading
    handler = handler or _T3RecheckUpstreamHandler
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    srv.requests = []
    srv.posts = 0
    srv.lock = threading.Lock()
    t = threading.Thread(target=srv.serve_forever, daemon=True)
    t.start()
    return srv, "http://127.0.0.1:%d" % srv.server_address[1]


class RecheckRouteTest(unittest.TestCase):
    """r3a A13 (T3 owns the recheck) + r3b B7 (the route-aware recheck must be
    reachable). One row whose `model` differs from its name — that mismatch
    is what makes both legs decidable:

      * `_last_response_status` keeps only captures whose `body.model` equals
        the value it is handed, so looking the row NAME up finds no capture and
        the recheck is never reached (B7);
      * the recheck must ask the ROUTE's `/v1/models` for the WIRE id, not the
        single upstream for the row name (A13).

    RED on the frozen bytes: `recheck_models_available(live_upstream,
    case["model"], ambient_key)` asks the one upstream for the row name, and a
    route that answers 404 is indistinguishable from a catalog change — the
    lost retry records nothing (B7: "the no-retry path currently records
    nothing")."""

    AMBIENT_CANARY = "synthetic-ambient-canary-DO-NOT-FORWARD"

    def _fixture(self, handler=None, ambient_key="", row_name="row-alias",
                 env_key="WIRETAP_T2_KEY_X"):
        tmp = tempfile.mkdtemp(prefix="rt-t3-recheck-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        srv, upstream = _start_t3_recheck_upstream(handler)
        self.addCleanup(srv.shutdown)
        live = _write_fake_live_home(tmp, upstream, row_name=row_name,
                                     env_key=env_key)
        fake_bin = _write_fake_binary(tmp, upstream, model="m-x")
        out = os.path.join(tmp, "report")
        os.makedirs(out)
        case = {"id": "RT-T3-RECHECK", "title": "route-aware recheck",
                "driver": "headless", "model": row_name,
                "steps": [{"op": "turn", "prompt": "ping"}],
                "wirecap": True, "est_calls": 1,
                "retry": {"on_status": [404, 502, 503], "max_attempts": 1,
                          "backoff_s": 0, "recheck_models": True}}
        args = argparse.Namespace(out=out, live_home=live, wirecap=True,
                                  budget=10, ambient_key=ambient_key,
                                  bin=fake_bin, rows=None, keep_home=False,
                                  campaign_id=None)
        sleeps = []
        orig_sleep = run.time.sleep
        run.time.sleep = lambda s: sleeps.append(s)
        try:
            row = run.run_case(case, args, run.Budget(args.budget))
        finally:
            run.time.sleep = orig_sleep
        gets = [r for r in srv.requests
                if r["method"] == "GET" and r["path"].startswith("/v1/models")]
        posts = [r for r in srv.requests if r["method"] == "POST"]
        return row, srv, gets, posts

    def test_route_listing_the_wire_id_gives_the_retry(self):
        """The row is named `row-alias` and sends `m-x`. The route's catalog
        lists `m-x`, so this is an env flap and the cell retries to a PASS."""
        row, srv, gets, posts = self._fixture()
        self.assertEqual(len(gets), 1,
                         "the recheck must reach the route's own /v1/models "
                         "exactly once; the origin saw %r"
                         % ([(r["method"], r["path"]) for r in
                             srv.requests],))
        self.assertEqual(row["status"], "PASS",
                         "the flap must retry to the 200 attempt")
        self.assertEqual(row["attempt"], 2)
        rechecks = [t for t in row["turns"]
                    if t.get("op") == "recheck_models"]
        self.assertEqual(len(rechecks), 1)
        self.assertEqual(rechecks[0]["present"], True)
        self.assertEqual(rechecks[0]["model"], "m-x",
                         "the recheck is recorded against the WIRE id, not "
                         "the row name (%r)" % (rechecks[0],))

    def test_the_recheck_asks_the_routes_origin_not_the_default(self):
        """r3a A13's OTHER half, added at r9 after CF-T3-20 survived.

        Every other fixture in this class puts the row and the deployment
        default on ONE origin, so `origin = upstream` and `origin =
        route["origin"]` are the same string and the question "which origin
        is asked?" cannot be answered. Here the two disagree on purpose: the
        row's origin lists `m-x`, the config default lists only `m-other`.
        Asking the default answers "that model is not in my catalog" — a
        catalog change, which must NOT retry — for a row that is plainly
        present where it lives."""

        class _OtherCatalog(_T3RecheckUpstreamHandler):
            catalog_ids = ("m-other",)

        tmp = tempfile.mkdtemp(prefix="rt-t3-recheck-two-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        dflt, dflt_url = _start_t3_recheck_upstream(_OtherCatalog)
        self.addCleanup(dflt.shutdown)
        route, route_url = _start_t3_recheck_upstream()
        self.addCleanup(route.shutdown)
        live = _write_fake_live_home(tmp, route_url, row_name="row-alias",
                                     default_upstream=dflt_url)
        fake_bin = _write_fake_binary(tmp, route_url, model="m-x")
        out = os.path.join(tmp, "report")
        os.makedirs(out)
        case = {"id": "RT-T3-RECHECK-2O", "title": "two-origin recheck",
                "driver": "headless", "model": "row-alias",
                "steps": [{"op": "turn", "prompt": "ping"}],
                "wirecap": True, "est_calls": 1,
                "retry": {"on_status": [404, 502, 503], "max_attempts": 1,
                          "backoff_s": 0, "recheck_models": True}}
        args = argparse.Namespace(out=out, live_home=live, wirecap=True,
                                  budget=10, ambient_key="",
                                  bin=fake_bin, rows=None, keep_home=False,
                                  campaign_id=None)
        sleeps = []
        orig_sleep = run.time.sleep
        run.time.sleep = lambda s: sleeps.append(s)
        try:
            row = run.run_case(case, args, run.Budget(args.budget))
        finally:
            run.time.sleep = orig_sleep

        def gets(srv):
            return [r for r in srv.requests
                    if r["method"] == "GET"
                    and r["path"].startswith("/v1/models")]

        self.assertEqual(row["status"], "PASS",
                         "the row must be judged where it lives; the row "
                         "record is %r" % ({k: row.get(k) for k in
                                            ("status", "reason")},))
        self.assertEqual(len(gets(route)), 1,
                         "the route's own catalog must be asked exactly once")
        self.assertEqual(gets(dflt), [],
                         "the deployment default must never be asked about "
                         "a routed row; it saw %r"
                         % ([(r["method"], r["path"])
                             for r in dflt.requests],))

    def test_unreadable_catalog_gives_no_retry_and_records_the_reason(self):
        """r3a A13: a route that does not return a readable model list gives
        no retry (fail-closed, as today) AND records
        `recheck unsupported on <host> (<status>)` so the lost retry is
        visible instead of silent (r3b B7)."""
        class _NoCatalog(_T3RecheckUpstreamHandler):
            catalog_status = 404

        row, _srv, gets, _posts = self._fixture(_NoCatalog)
        self.assertEqual(len(gets), 1, "the recheck was still attempted")
        self.assertEqual(row["status"], "FAIL")
        self.assertEqual(row["attempt"], 1,
                         "an unreadable catalog must not buy a retry")
        rechecks = [t for t in row["turns"]
                    if t.get("op") == "recheck_models"]
        self.assertEqual(len(rechecks), 1,
                         "the lost retry must be recorded in the row: %r"
                         % (row["turns"],))
        self.assertEqual(rechecks[0]["present"], False)
        self.assertIn("recheck unsupported on", rechecks[0]["reason"])
        self.assertIn("(404)", rechecks[0]["reason"])

    def test_ambient_key_rides_only_when_it_is_the_routes_credential(self):
        """r3a A13 leg 3: neither the forwarded turn request nor the recheck
        carries the ambient key unless that route's credential variable IS the
        ambient key."""
        row, _srv, gets, posts = self._fixture(
            ambient_key=self.AMBIENT_CANARY)
        self.assertEqual(row["status"], "PASS")
        for label, seen in (("recheck", gets), ("turn", posts)):
            for req in seen:
                joined = "\n".join("%s: %s" % (k, v)
                                   for k, v in req["headers"].items())
                _t3_assert_absent(
                    self, req["headers"],
                    [("ambient-canary", self.AMBIENT_CANARY)],
                    "the %s request's authorization header must not "
                    "carry the ambient key though the route declares "
                    "env_key=WIRETAP_T2_KEY_X" % label)
        self.assertEqual(len(gets), 1)
        _t3_assert_value(
            self, gets[0]["headers"].get("authorization"),
            "Bearer synthetic-plant-wiretap_t2_key_x-value",
            "the recheck authenticates with the ROUTE's own "
            "credential, header-replaced as in r3 §4.2.3")

        # The other leg: the route's credential variable IS the ambient key,
        # so carrying it is correct and refusing it would break the proxy row.
        row2, _srv2, gets2, _posts2 = self._fixture(
            ambient_key="synthetic-plant-wiretap_t2_key_x-value")
        self.assertEqual(row2["status"], "PASS")
        self.assertEqual(len(gets2), 1)
        _t3_assert_value(
            self, gets2[0]["headers"].get("authorization"),
            "Bearer synthetic-plant-wiretap_t2_key_x-value",
            "a route whose credential is the ambient key still "
            "authenticates")


class AuthErrorClassSplitTest(unittest.TestCase):
    """r3 §4.5 item 3 + R-12: the 2026-10-29 classifier's single
    `credentials-403` class mixes "harness auth broken" with "right key, wrong
    origin". The split is mechanical: an upstream auth error has a captured
    request and an upstream status; a local auth failure has ZERO captured
    requests, so it is a harness defect and must never read as a provider
    rejection. RED on the frozen bytes: `run.py` names no such class at all —
    `_triage_block` offers only route-map / env / case-bug / temperament /
    product, so both shapes fall through to the same label."""

    def _wire(self, tmp, posts, resp_status, server_header=True):
        d = os.path.join(tmp, "wire")
        os.makedirs(d, exist_ok=True)
        for n in range(1, posts + 1):
            with open(os.path.join(d, "req-%03d.json" % n), "w") as fh:
                fh.write(json.dumps({"n": n, "method": "POST",
                                     "path": "/v1/responses",
                                     "headers": {},
                                     "body": {"model": "m-x"}}))
            headers = {"content-type": "application/json"}
            if server_header:
                headers["server"] = "openresty"
            with open(os.path.join(d, "resp-%03d.jsonl" % n), "w") as fh:
                fh.write(json.dumps({"n": n, "status": resp_status,
                                     "headers": headers}) + "\n")
                fh.write(json.dumps({"frame": json.dumps(
                    {"error": {"message": "Unknown authorization credentials",
                               "type": "auth_error",
                               "code": "invalid_api_key",
                               "param": "masked"}})}) + "\n")
        return d

    def test_upstream_auth_error_and_local_failure_are_distinct_classes(self):
        tmp = tempfile.mkdtemp(prefix="rt-t3-authclass-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        upstream = self._wire(os.path.join(tmp, "u"), 1, 403)
        local = self._wire(os.path.join(tmp, "l"), 0, 403)
        self.assertNotEqual(upstream, local)
        self.assertEqual(run.auth_failure_class(upstream),
                         run.AUTH_CLASS_UPSTREAM,
                         "a captured 403 from an origin IS an upstream auth "
                         "problem")
        self.assertEqual(run.auth_failure_class(local),
                         run.AUTH_CLASS_LOCAL,
                         "zero captured requests is a harness failure")
        self.assertNotEqual(run.AUTH_CLASS_UPSTREAM, run.AUTH_CLASS_LOCAL)
        self.assertEqual(run.auth_failure_class(
            self._wire(os.path.join(tmp, "p"), 1, 200)), None,
            "a row that never saw an auth status has no auth class")

    def test_triage_block_never_calls_a_zero_capture_row_an_upstream_problem(
            self):
        tmp = tempfile.mkdtemp(prefix="rt-t3-authtag-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        run_dir = os.path.join(tmp, "run")
        os.makedirs(run_dir)
        self._wire(run_dir, 0, 403)
        ctx = run.CaseCtx()
        ctx.verdict = {"blocked_reason": "credential-unset: WIRETAP_T2_KEY_X"}
        row = {"id": "RT-T3-AUTH", "status": "BLOCKED", "recon": False,
               "asserts": [], "attempt": 1}
        block = run._triage_block({"id": "RT-T3-AUTH", "model": "row-x"},
                                  row, ctx, run_dir, None)
        self.assertEqual(block["auth_class"], run.AUTH_CLASS_LOCAL,
                         "a case with no captured request is a harness "
                         "defect, not a provider rejection (R-12)")
        self.assertNotEqual(block["auth_class"], run.AUTH_CLASS_UPSTREAM)
        up_dir = os.path.join(tmp, "run2")
        os.makedirs(up_dir)
        self._wire(up_dir, 1, 403)
        ctx2 = run.CaseCtx()
        ctx2.verdict = {}
        row2 = {"id": "RT-T3-AUTH2", "status": "FAIL", "recon": False,
                "asserts": [], "attempt": 1}
        block2 = run._triage_block({"id": "RT-T3-AUTH2", "model": "row-x"},
                                   row2, ctx2, up_dir, None)
        self.assertEqual(block2["auth_class"], run.AUTH_CLASS_UPSTREAM,
                         "the two classes must be separately reportable")


# ---------------------------------------------------------------------------
# End of T3 chunk 3.
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# REDTEAM-REFRESH T4 (batch 4) — a row NAME must resolve to that row's WIRE ID
# at the two runner sites that build a `where` on `body.model` out of a name:
# `synth_output_pins` and the rows path (r3 §4.7 + §4.6 row 17, M-11 / R2-6,
# as amended by r3a A10; s0 F-10's disposition: resolution must not collapse
# distinct rows).
#
# Offline, on the T2 harness (r3a A12): run_case + the real wiretap + the
# loopback recorder. Every fixture row's env_key names a TEST-ONLY variable
# under `_t2_env_guard` (approval F2); no assertion reads or prints a value.
# The fixture binary resolves `-m <row>` to that row's `model` field the way
# the product does, so the byte a capture carries is the byte the row sends.
#
# r3a A10: the runner rewrites SYNTHESIZED pins only. A row assert's own
# `where` is never rewritten; the rows path resolves membership and `nth`.
# ---------------------------------------------------------------------------

# Two rows whose name differs from the byte they send (`row-tf`, the m11
# shape; `sol` + `sol-1m`, a PAIR on one wire id with one route triple, as the
# four live pairs of r3 §3.7 fact 1), and one row that sends its own name.
T4_CFG = '''base_url = "%(a)s/v1"
models_base_url = "%(a)s/v1"
[features]
turn_summary = false

[model."row-x"]
base_url = "%(a)s/v1"
api_backend = "responses"
env_key = "WIRETAP_T2_KEY_X"

[model."row-tf"]
model = "lxg-sc-stg/wire-tf"
base_url = "%(a)s/v1"
api_backend = "responses"
env_key = "WIRETAP_T2_KEY_X"

[model."sol"]
model = "m-sol"
base_url = "%(a)s/v1"
api_backend = "responses"
env_key = "WIRETAP_T2_KEY_X"

[model."sol-1m"]
model = "m-sol"
base_url = "%(a)s/v1"
api_backend = "responses"
env_key = "WIRETAP_T2_KEY_X"
'''

T4_WIRE_TF = "lxg-sc-stg/wire-tf"
T4_NEEDLE = "T4-NEEDLE"

# r3 §3.3 / §4.7 (`m11_table`): the five cases whose synthesized scored pins
# can never match on the batch-start bytes — 5 cases / 7 pins.
T4_M11_CASE_FILES = ("cw-stable-reminder-01.json", "mxai-c11.json",
                     "mxai-c13.json", "t21-responses-sol-mcp-tool.json",
                     "ws9-s01-tool-loop.json")


def _t4_row_binary(tmp, name="fake-grok-t4", post_path="/responses"):
    """A fake pager binary that sends the byte the product would: it reads
    `-m <row>` and `-p <prompt>`, looks the row up in the hermetic config and
    POSTs that row's `model` field (else the row name) with the prompt as the
    input text, to the hermetic base_url + `post_path`. The body also names
    the row it ran as (`t4_row`)."""
    b = os.path.join(tmp, name)
    with open(b, "w") as fh:
        fh.write(
            "#!/bin/sh\n"
            "exec python3 - \"$GROK_HOME/config.toml\" \"$@\" <<'PYEOF'\n"
            "import json, sys, urllib.request, urllib.error\n"
            "cfg = open(sys.argv[1]).read().splitlines()\n"
            "argv = sys.argv[2:]\n"
            "row = argv[argv.index('-m') + 1]\n"
            "prompt = argv[argv.index('-p') + 1]\n"
            "base = next(l.split('=', 1)[1].strip().strip('\"')\n"
            "            for l in cfg if l.startswith('base_url'))\n"
            "wire, cur = row, False\n"
            "for line in cfg:\n"
            "    s = line.strip()\n"
            "    if s.startswith('['):\n"
            "        cur = s in ('[model.\"%s\"]' % row, '[model.%s]' % row)\n"
            "    elif cur and '=' in s:\n"
            "        k, v = s.split('=', 1)\n"
            "        if k.strip() == 'model':\n"
            "            wire = v.strip().strip('\"')\n"
            "body = {'model': wire, 'store': False, 't4_row': row,\n"
            "        'input': [{'role': 'user', 'content': prompt}]}\n"
            "req = urllib.request.Request(base + '@@PATH@@',\n"
            "    data=json.dumps(body).encode(),\n"
            "    headers={'content-type': 'application/json'})\n"
            "try:\n"
            "    urllib.request.urlopen(req, timeout=15).read()\n"
            "except urllib.error.HTTPError:\n"
            "    sys.exit(1)\n"
            "PYEOF\n".replace("@@PATH@@", post_path))
    os.chmod(b, 0o755)
    return b


def _t4_post_captures(run_dir):
    """Basenames of the case's captured POST requests, oldest first."""
    wire_dir = os.path.join(run_dir, "wire")
    out = []
    for name in sorted(os.listdir(wire_dir)):
        if name.startswith("req-") and name.endswith(".json"):
            with open(os.path.join(wire_dir, name)) as fh:
                if json.load(fh).get("method") == "POST":
                    out.append(name)
    return out


class NameToWireIdResolutionTest(unittest.TestCase):
    """r3 §4.6 row 17 / §4.7 — T4's acceptance is this runner test, not the
    generator's static `m11_table` (R2-6).

    Synthesized pins (both invariants, r3a A10):
      * a case naming a row whose `model` override differs from its name
        keys its synthesized pin on the wire id that row sends (RED on the
        batch-start bytes: the pin keys on the name and selects nothing);
      * a case that already names the wire id selects the same capture
        before and after (invariant 1, PRESERVATION);
      * the key-free path, which has no route table, keeps the authored byte
        (GUARD, r3 §2.6);
      * the five real m11 cases' declarations, run through the pipeline,
        select the capture their row sends (RED: 5 cases / 7 pins).
    Rows path (membership and `nth` through the resolver only, r3a A10):
      * a row assert whose identity is the wire id its row sends is scored,
        not skipped as recon (RED); a no-`where` one gets that row's own
        run-order position as `nth` (RED);
      * s0 F-10, PRESERVATION: a filtered row is never stood in for by a row
        sending the same wire id; two rows on one wire id keep their own
        positions; an id two rows of this run send addresses neither;
      * r3a A10, PRESERVATION: a row assert whose own `where` holds the bare
        row name still fails as a scored empty selection until its T5 batch
        rewrites that byte (`RT-M5`'s and `RT-M6`'s shape).
    """

    def _run(self, case, cfg=None, rows=None):
        tmp = tempfile.mkdtemp(prefix="rt-t4-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        srv, upstream = _start_recording_upstream()
        self.addCleanup(srv.shutdown)
        live = _t2_write_cfg_home(tmp, (cfg or T4_CFG) % {"a": upstream})
        args, _out = _t2_case_args(tmp, live, _t4_row_binary(tmp), rows=rows)
        row = run.run_case(case, args, run.Budget(args.budget))
        return row, [r["model"] for r in srv.requests if r["method"] == "POST"]

    def _one(self, row, aid):
        hits = [a for a in row["asserts"]
                if isinstance(a.get("spec"), dict)
                and a["spec"].get("id") == aid]
        self.assertEqual(len(hits), 1, "exactly one result for %s" % aid)
        return hits[0]

    @staticmethod
    def _synth_case(cid, model):
        return {"id": cid, "title": "synthesized output_contains pin",
                "driver": "headless", "model": model, "wire": "responses",
                "mcp_calls": [{"id": "cg1", "output_contains": T4_NEEDLE}],
                "steps": [{"op": "turn", "prompt": "ping " + T4_NEEDLE}],
                "wirecap": True, "est_calls": 1, "assert": {"exit": 0}}

    @staticmethod
    def _rows_case(cid, rows, specs):
        return {"id": cid, "title": "row asserts through the resolver",
                "driver": "headless", "model": rows[0],
                "rows": [{"model": m, "prompt": "ping"} for m in rows],
                "wirecap": True, "est_calls": len(rows),
                "row_asserts": {"wire": specs}}

    # --- site 1: `synth_output_pins` ----------------------------------------

    def test_synth_pin_keys_on_the_wire_id_its_row_sends(self):
        """RED: the scored `synth_cg1` grep is keyed on the row NAME, so it
        selects no capture although the row's request carries the needle."""
        row, posts = self._run(self._synth_case("RT-T4-SYNTH-NAME", "row-tf"))
        self.assertEqual(posts, [T4_WIRE_TF],
                         "fixture: the row named row-tf sends its override")
        res = self._one(row, "synth_cg1")
        self.assertEqual(res["kind"], "wire.grep")
        self.assertTrue(res["ok"], "the synthesized pin must select the "
                        "request its row sent: %s" % res["detail"])
        self.assertEqual(res["spec"]["where"].get("body.model"), T4_WIRE_TF,
                         "the pin keys on the wire id the row sends (§4.7)")
        self.assertEqual(row["status"], "PASS")

    def test_synth_pin_already_naming_the_wire_id_selects_the_same_capture(
            self):
        """PRESERVATION (§4.7 invariant 1): green before and after."""
        row, posts = self._run(self._synth_case("RT-T4-SYNTH-WIRE",
                                                T4_WIRE_TF))
        self.assertEqual(posts, [T4_WIRE_TF])
        caps = _t4_post_captures(row["run_dir"])
        self.assertEqual(len(caps), 1)
        res = self._one(row, "synth_cg1")
        self.assertEqual(
            (res["kind"], res["ok"], res["detail"],
             res["spec"]["where"].get("body.model")),
            ("wire.grep", True,
             "%s contains %r (any of 1)" % (caps[0], T4_NEEDLE), T4_WIRE_TF),
            "an already-wire-id pin selects exactly the capture it did")

    def test_key_free_synth_pin_keeps_the_authored_byte(self):
        """GUARD (r3 §2.6, green before and after): the key-free path starts
        no wiretap and has no route table, so its synthesized pin keeps the
        byte the case authored and still matches the pre-placed capture."""
        tmp = tempfile.mkdtemp(prefix="rt-t4-keyfree-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        cases_dir = os.path.join(tmp, "cases")
        pre = os.path.join(cases_dir, "rt-t4-keyfree", "wire")
        os.makedirs(pre)
        with open(os.path.join(pre, "req-001.json"), "w") as fh:
            json.dump({"n": 1, "method": "POST", "path": "/v1/responses",
                       "body": {"model": "row-tf", "store": False,
                                "input": [{"role": "user",
                                           "content": "ping " + T4_NEEDLE}]}},
                      fh, indent=2)
        saved = run.CASES_DIR
        run.CASES_DIR = cases_dir
        self.addCleanup(setattr, run, "CASES_DIR", saved)
        live = _t2_write_cfg_home(tmp, T4_CFG % {"a": "http://127.0.0.1:9"})
        args, _out = _t2_case_args(tmp, live, _t4_row_binary(tmp),
                                   wirecap=False)
        case = dict(self._synth_case("RT-T4-KEYFREE", "row-tf"),
                    env={"proxy": "none"}, steps=[], wirecap=False,
                    est_calls=0)
        case.pop("assert")
        row = run.run_case(case, args, run.Budget(args.budget))
        res = self._one(row, "synth_cg1")
        self.assertEqual(
            (res["ok"], res["spec"]["where"].get("body.model")),
            (True, "row-tf"),
            "a key-free synthesized pin keeps the authored byte: %s"
            % res["detail"])

    def test_m11_census_pins_select_what_their_row_sends(self):
        """RED (r3 §3.3: the test reproduces the residue before it is claimed
        fixed). Each m11 case's own declarations run through the pipeline, its
        `model` declared as a row whose override is `lxg-sc-stg/<model>` — on
        the batch-start bytes every case names `gpt-5.6-sol`, so this is the
        corpus pair `gpt-5.6-sol` -> `lxg-sc-stg/gpt-5.6-sol` exactly. The case
        files are read, never written; the disabled flag and every other
        field stay in the file."""
        tmp = tempfile.mkdtemp(prefix="rt-t4-m11-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        _t2_env_guard(self)
        srv, upstream = _start_recording_upstream()
        self.addCleanup(srv.shutdown)
        reals = []
        for fname in T4_M11_CASE_FILES:
            with open(os.path.join(run.CASES_DIR, fname)) as fh:
                reals.append(json.load(fh))
        cfg = ['base_url = "%s/v1"' % upstream,
               'models_base_url = "%s/v1"' % upstream,
               "[features]", "turn_summary = false"]
        for m in sorted({r["model"] for r in reals}):
            cfg += ['[model."%s"]' % m, 'model = "lxg-sc-stg/%s"' % m,
                    'base_url = "%s/v1"' % upstream,
                    'api_backend = "responses"',
                    'env_key = "WIRETAP_T2_KEY_X"']
        live = _t2_write_cfg_home(tmp, "\n".join(cfg) + "\n")
        n_pins, unresolved = 0, {}
        for i, real in enumerate(reals):
            decls = [d for d in (list(real.get("mcp_calls") or [])
                                 + list(real.get("tool_calls") or []))
                     if d.get("output_contains")]
            path = run.WIRE_PATHS[real["wire"]]
            binary = _t4_row_binary(tmp, "fake-grok-t4-%d" % i,
                                    post_path=path[len("/v1"):])
            args, _out = _t2_case_args(tmp, live, binary)
            case = {"id": "RT-T4-M11-%d" % i,
                    "title": "m11 census: %s" % real["id"],
                    "driver": "headless", "model": real["model"],
                    "wire": real["wire"],
                    "mcp_calls": list(real.get("mcp_calls") or []),
                    "tool_calls": list(real.get("tool_calls") or []),
                    "steps": [{"op": "turn", "prompt": "census " + " ".join(
                        d["output_contains"] for d in decls)}],
                    "wirecap": True, "est_calls": 1, "assert": {"exit": 0}}
            row = run.run_case(case, args, run.Budget(args.budget))
            pins = [a for a in row["asserts"]
                    if isinstance(a.get("spec"), dict)
                    and a["spec"].get("synth")]
            n_pins += len(pins)
            bad = ["%s keyed %s, row sends lxg-sc-stg/%s"
                   % (a["spec"]["id"], a["spec"]["where"].get("body.model"),
                      real["model"]) for a in pins if not a["ok"]]
            if bad:
                unresolved[real["id"]] = bad
        self.assertEqual((len(reals), n_pins), (5, 7),
                         "the m11 census: 5 cases / 7 synthesized pins")
        self.assertEqual(unresolved, {},
                         "never-matchable synthesized pins: %s"
                         % json.dumps(unresolved, sort_keys=True))

    # --- site 2: the rows path (membership + `nth`) --------------------------

    def test_row_assert_keyed_on_the_wire_id_is_scored_not_skipped(self):
        """RED: the run order holds row NAMES, so a row assert whose identity
        is the wire id its row sends is dropped as a recon `wire.row-skip`
        (`ok=True`) — silence, not red (r3 §3.3)."""
        row, posts = self._run(self._rows_case(
            "RT-T4-ROWS-WIRE", ["row-tf"],
            [{"id": "rw_wire", "kind": "field", "file": "req-*.json",
              "path": "$.body.model", "eq": T4_WIRE_TF,
              "where": {"method": "POST", "$.body.model": T4_WIRE_TF}}]))
        self.assertEqual(posts, [T4_WIRE_TF])
        res = self._one(row, "rw_wire")
        self.assertEqual(res["kind"], "wire.field",
                         "a row assert naming the wire id its row sends is a "
                         "scored claim, not a recon skip: %s" % res["detail"])
        self.assertTrue(res["ok"], res["detail"])
        self.assertEqual(row["status"], "PASS")

    def test_no_where_row_assert_takes_its_rows_position_as_nth(self):
        """RED: a no-`where` row assert whose identity is the wire id of the
        second row is skipped; through the resolver its `nth` is that row's
        own run-order position."""
        row, posts = self._run(self._rows_case(
            "RT-T4-ROWS-NTH", ["row-x", "row-tf"],
            [{"id": "rw_nth", "row_model": T4_WIRE_TF, "kind": "field",
              "file": "req-*.json", "path": "$.method", "eq": "POST"}]))
        self.assertEqual(posts, ["row-x", T4_WIRE_TF])
        res = self._one(row, "rw_nth")
        self.assertEqual(res["kind"], "wire.field",
                         "the identity resolves to a row of this run: %s"
                         % res["detail"])
        self.assertEqual(res["spec"].get("nth"), 1,
                         "nth is row-tf's own position in the run order")
        self.assertTrue(res["ok"], res["detail"])

    def test_a_filtered_row_is_not_stood_in_for_by_its_twin(self):
        """PRESERVATION (s0 F-10, green before and after): `--rows sol` runs
        `sol` only; a row assert for `sol-1m`, which sends the same wire id,
        must keep its recon skip instead of being scored on `sol`'s request."""
        row, posts = self._run(self._rows_case(
            "RT-T4-ROWS-FILTER", ["sol", "sol-1m"],
            [{"id": "rw_twin", "row_model": "sol-1m", "kind": "field",
              "file": "req-*.json", "path": "$.body.model", "eq": "m-sol",
              "where": {"method": "POST", "$.body.model": "m-sol"}}]),
            rows="sol")
        self.assertEqual(posts, ["m-sol"], "only the selected row ran")
        res = self._one(row, "rw_twin")
        self.assertEqual((res["kind"], res["recon"]), ("wire.row-skip", True),
                         "a filtered row's assert must not be scored on its "
                         "twin's request: %s" % res["detail"])
        self.assertIn("sol-1m", res["detail"])

    def test_two_rows_on_one_wire_id_keep_their_own_positions(self):
        """PRESERVATION (s0 F-10 / r3 §3.7 fact 2, green before and after):
        `sol` and `sol-1m` send one byte; each row name still addresses its
        own run-order position, never the first twin's."""
        row, posts = self._run(self._rows_case(
            "RT-T4-ROWS-PAIR", ["sol", "sol-1m"],
            [{"id": aid, "row_model": name, "kind": "field",
              "file": "req-*.json", "path": "$.method", "eq": "POST"}
             for aid, name in (("nth_sol", "sol"),
                               ("nth_sol_1m", "sol-1m"))]))
        self.assertEqual(posts, ["m-sol", "m-sol"])
        got = {aid: (self._one(row, aid)["kind"],
                     self._one(row, aid)["spec"].get("nth"))
               for aid in ("nth_sol", "nth_sol_1m")}
        self.assertEqual(got, {"nth_sol": ("wire.field", 0),
                               "nth_sol_1m": ("wire.field", 1)},
                         "two rows on one wire id keep two positions")

    def test_an_id_two_rows_send_addresses_neither(self):
        """PRESERVATION (s0 F-10, green before and after): `m-sol` is sent by
        both rows of this run, so it names no single row. It is never scored
        against the first twin; it stays unscored as on the batch-start bytes
        (r3 §3.7 keeps both members of a pair out of one corpus case)."""
        row, _posts = self._run(self._rows_case(
            "RT-T4-ROWS-AMBIG", ["sol", "sol-1m"],
            [{"id": "nth_amb", "row_model": "m-sol", "kind": "field",
              "file": "req-*.json", "path": "$.method", "eq": "POST"}]))
        res = self._one(row, "nth_amb")
        self.assertNotEqual(res["kind"], "wire.field",
                            "an id two rows send must not be scored against "
                            "one of them: %s" % res["detail"])
        self.assertNotIn("nth", res["spec"])

    def test_row_assert_pinning_a_bare_name_still_fails_until_t5(self):
        """PRESERVATION (r3a A10, green before and after): the runner does not
        rewrite an explicit `where`. A row assert whose `where` holds the bare
        row name — `RT-M5`'s form (with `row_model`) and `RT-M6`'s (identity
        in `where` only) — stays a scored empty selection; T5.5 and T5.7 own
        those bytes."""
        specs = [{"id": "rw_bare_legacy", "row_model": "row-tf",
                  "kind": "field", "file": "req-*.json",
                  "path": "$.body.model", "eq": T4_WIRE_TF,
                  "where": {"method": "POST", "$.body.model": "row-tf"}},
                 {"id": "rw_bare_schema", "kind": "field",
                  "file": "req-*.json", "path": "$.body.model",
                  "eq": T4_WIRE_TF,
                  "where": {"method": "POST", "$.body.model": "row-tf"}}]
        row, _posts = self._run(self._rows_case("RT-T4-ROWS-BARE", ["row-tf"],
                                                specs))
        for aid in ("rw_bare_legacy", "rw_bare_schema"):
            res = self._one(row, aid)
            self.assertEqual(
                (res["kind"], res["ok"], res["detail"]),
                ("wire.field", False, "only 0 wire files match (nth=0)"),
                "%s: the pin's own bare-name byte selects nothing" % aid)
        self.assertEqual(row["status"], "FAIL")


# ---------------------------------------------------------------------------
# End of T4.
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# REDTEAM-REFRESH T6 (batch 10) - turn budgets on the max-effort candidates and
# the wiretap's UPSTREAM_TIMEOUT. r3 section 5.1/5.2/5.4 and section 8 T6, as
# amended by r3a A11 (UpstreamTimerCouplingTest; no watchdog_s edit), r3f F1
# and r3g G1/G2 (the case list is re-derived after T5.9 with the generator's
# budget_candidate predicate over the frozen 7-row max-effort row input).
#
# Offline only: both tests read case files through run.load_cases and read
# wiretap.py's source text; no process, socket or credential is touched.
# ---------------------------------------------------------------------------
T6_TARGET_TURN_S = 900

# The re-derived T6 case list (batch-10 B10-T6-derive-open.json, 78 cases).
T6_BUDGET_CANDIDATES = (
    "AT-EFFORT-2HOP", "AT-ENC-PROBE-UNPIN", "AT-STRICT-XWIRE-01", "AT-VLQ-VXG",
    "CW-WRITE-USAGE-01", "DEAD-SEAT-KILL-RESUME", "ENC-AZ-AZ-MISMATCH",
    "ENC-AZ-AZ-RESUME", "ENC-SOL-2TURN-PINNED", "HTS-3V1S-LHR-LUNA",
    "HTS-3V1S-LHR-SOL", "HTS-3V1S-LHR-TERRA", "HTS-3V1S-LNR-LUNA",
    "HTS-3V1S-LNR-SOL", "HTS-3V1S-LNR-TERRA", "INGRESS77-SOL-FLAG-01",
    "MGW-COMPACTV2-01", "MGW-COMPACTV2-02", "MXAI-C01", "MXAI-C04", "MXAI-C05",
    "MXAI-C06", "MXAI-C07", "MXAI-C08", "MXAI-C08-BASELINE", "MXAI-C09",
    "MXAI-C10", "MXAI-C11", "MXAI-C13", "MXAI-C14", "OW-CACHECTL-01",
    "OW-COSTCTL-01", "OW-METADATA-01", "OW-REASONSURF-01", "RT-C1", "RT-C2",
    "RT-C3", "RT-C4", "RT-C5", "RT-CROSSWIRE1", "RT-CROSSWIRE1B", "RT-M1",
    "RT-M10a", "RT-M10b", "RT-M11", "RT-M11r", "RT-M2", "RT-M3", "RT-M4",
    "RT-M5", "RT-M8", "RT-M9", "RT-MSW-A", "RT-MSW-C", "RT-R1", "RT-R2",
    "RT-R3", "RT-S2", "RT-S3", "RT-XREPLAY1", "RT-XREPLAY2",
    "T21-RESP-QWEN3827B-MCP-TOOL", "T21-RESP-SOL-MCP-TOOL",
    "WS9-S01-TOOL-LOOP", "WS9-S02-TOOL-FAIL", "WS9-S03-STREAM-TRUNC",
    "WS9-S04-MODEL-SWITCH", "WS9-S06-SPAWN-MSG", "WS9-S07-INT-CHILD",
    "WS9-S09-FREEFORM-PATCH", "WS9-S11-LEGACY-RESUME", "XW-EFFORT-CLAMP-3FAM",
    "XW-REPLAY-R2-01", "XW-RESUME-CROSSFAM", "XW-VXM-AZ", "XW-VXM-AZ-LIVE",
    "XW-VXM-VLQ", "XW-VXM-VLQ-V2",
)

# Written decision (r3 section 5.2: a per-step value that is the assertion is
# decided in writing inside its batch, never silently raised). In the headless
# driver a turn step's kill_after_s replaces its deadline (run_headless_turn),
# so no budget edit binds there, and raising the kill would remove the subject.
T6_KILL_TIMED_EXEMPTIONS = {
    ("RT-S2", 0): "kill_after_s 6 with expect_kill: the mid-model-call kill "
                  "is the case's subject (tool list only, minimal proxy use)",
}


def _t6_cases():
    return run.load_cases(types.SimpleNamespace(case_ids=None))


def _t6_deadlines(case):
    """(site, step, seconds) for every turn, compact and rows-matrix turn, in
    the precedence the runner applies at that site (r3 section 5.1 table)."""
    resolver = run._effective_turn_timeout(case)
    if case.get("rows"):
        return [("rows-matrix turn", None, resolver)]
    acp = case.get("driver", "headless") == "acp"
    out = []
    for i, step in enumerate(case.get("steps") or []):
        op = step.get("op")
        if op == "turn":
            seconds = step.get("timeout_s", resolver)
            if not acp and step.get("kill_after_s") is not None:
                seconds = step["kill_after_s"]
            out.append(("turn", i, seconds))
        elif op == "compact":
            out.append(("compact", i, resolver))
    return out


def _t6_wiretap_upstream_timeout():
    """UPSTREAM_TIMEOUT as wiretap.py's source assigns it. Read, never
    imported: the runner's unittest stage runs the wiretap only as a process."""
    with open(run.WIRETAP) as fh:
        tree = ast.parse(fh.read())
    values = [ast.literal_eval(node.value) for node in tree.body
              if isinstance(node, ast.Assign)
              and [getattr(t, "id", None) for t in node.targets]
              == ["UPSTREAM_TIMEOUT"]]
    if len(values) != 1:
        raise AssertionError("wiretap.py must assign UPSTREAM_TIMEOUT once at "
                             "module level; found %d" % len(values))
    return values[0]


class EffectiveTurnBudgetTest(unittest.TestCase):
    """T6 (r3 section 8 T6): every turn and compact step of every candidate
    resolves, through the runner's own precedence at that site, to at least
    T6_TARGET_TURN_S. A compact step's own timeout_s is never read, and
    watchdog_s is only the resolver's fallback, so neither satisfies this test
    where it does not bind."""

    def test_every_candidate_step_resolves_to_the_target(self):
        by_id = {}
        for case in _t6_cases():
            by_id.setdefault(case.get("id"), []).append(case)
        missing = [c for c in T6_BUDGET_CANDIDATES if len(by_id.get(c, [])) != 1]
        self.assertEqual(missing, [], "every candidate is exactly one case file "
                                      "the runner loads")
        for (cid, step), why in sorted(T6_KILL_TIMED_EXEMPTIONS.items()):
            case = by_id[cid][0]
            st = (case.get("steps") or [])[step]
            self.assertIn(cid, T6_BUDGET_CANDIDATES)
            self.assertEqual(
                (st.get("op"), case.get("driver", "headless") != "acp",
                 st.get("kill_after_s") is not None, st.get("expect_kill")),
                ("turn", True, True, True),
                "%s step %d is exempt only while its kill timer is the "
                "subject: %s" % (cid, step, why))
        short, checked = [], 0
        for cid in T6_BUDGET_CANDIDATES:
            sites = _t6_deadlines(by_id[cid][0])
            self.assertTrue(sites, "%s has no budget-bearing step" % cid)
            for site, step, seconds in sites:
                if (cid, step) in T6_KILL_TIMED_EXEMPTIONS:
                    continue
                checked += 1
                if not seconds >= T6_TARGET_TURN_S:
                    short.append((cid, site, step, seconds))
        self.assertGreater(checked, 0)
        self.assertEqual(
            short, [],
            "%d step deadline(s) below %d s in %d candidate(s): %s" % (
                len(short), T6_TARGET_TURN_S, len({s[0] for s in short}),
                "; ".join("%s %s#%s=%s" % s for s in short)))


class UpstreamTimerCouplingTest(unittest.TestCase):
    """T6 (r3a A11): UPSTREAM_TIMEOUT bounds one blocking upstream read, so it
    must outlast the longest step deadline the runner applies to any enabled
    case; otherwise the transport answers a request the runner still waits on."""

    def test_upstream_timer_outlasts_every_enabled_step_deadline(self):
        timer = _t6_wiretap_upstream_timeout()
        sites = [(seconds, case.get("id"), site, step)
                 for case in _t6_cases() if not case.get("disabled")
                 for site, step, seconds in _t6_deadlines(case)]
        self.assertTrue(sites, "no enabled case has a budget-bearing step")
        top = max(s[0] for s in sites)
        self.assertGreater(
            timer, top,
            "UPSTREAM_TIMEOUT=%s is not above the largest enabled step deadline "
            "%s: %s" % (timer, top, "; ".join(sorted(
                "%s %s#%s=%s" % (c, site, st, s)
                for s, c, site, st in sites if s >= timer))))


# ---------------------------------------------------------------------------
# End of T6.
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# apex-sk8i / apex-au7k (spec r2 §2.2, §3, §4): the sign-in credential the
# binary's gate accepts without a live probe. The child environment is read
# at the launch boundary (RR-32 F-2), never through the fake binary's view.
# ---------------------------------------------------------------------------


def _spy_binary_launches(test, bin_path):
    """The launch spy (spec r2 §4 fixture extensions, RR-32 F-2).

    Replaces `subprocess.Popen`, the module attribute both binary launches
    call (`run_headless_turn`, `AcpSession.start`), with a pass-through. For
    each call whose argv[0] is `bin_path` it appends the argv, the sorted env
    NAMES and the one non-credential constant GROK_AUTH_EXPIRED to the list
    it returns, then delegates to the saved Popen; every other launch (the
    wiretap's interpreter, anything `subprocess.run` starts) passes through
    unrecorded. It never keeps the env dict or any value from it. Restored on
    cleanup (the ReplayBase._patch precedent)."""
    real = subprocess.Popen
    launches = []

    def spy(args, *a, **kw):
        argv = list(args) if isinstance(args, (list, tuple)) else [args]
        if argv and argv[0] == bin_path:
            given = kw.get("env")
            launches.append({
                "argv": argv,
                "env_names": sorted(given) if given is not None else None,
                "grok_auth_expired": (given or {}).get("GROK_AUTH_EXPIRED")})
        return real(args, *a, **kw)

    subprocess.Popen = spy
    test.addCleanup(setattr, subprocess, "Popen", real)
    return launches


def _binary_signin_gate(child_names, row_routes, probe_allows=False):
    """A test-local MODEL of the binary's sign-in gate over NAMES (spec r2
    §1.2 steps 1-6, read at 8072d8ed):

      * has_byok: some loaded row's own credential resolves, i.e. its env_key
        is in the child's names (acp_agent.rs:325-329 ranges over every loaded
        model; config.rs:5948-5967 own_credential, :6283-6293; an
        env_http_headers variable is not a rung);
      * probes: no row key and a fallback-chain rung present
        (api_key_probe.rs:27-37 should_probe; the rungs are
        auth_method.rs:24-34's three names, run.XAI_CREDENTIAL_CHAIN);
      * has_external_api_key: has_byok, or a chain rung the probe allows
        (shell auth_method.rs:37-50).

    `probe_allows` stands for the live probe's verdict, which no offline model
    knows; False is the deterministic refusal T9 measured. A hand-built model:
    it says nothing about the live path (Agents.md §8)."""
    names = set(child_names or ())
    has_byok = any(isinstance(r.get("env_key"), str) and r["env_key"]
                   and r["env_key"] in names for r in row_routes or ())
    chain = any(n in names for n in run.XAI_CREDENTIAL_CHAIN)
    return {"has_byok": has_byok, "probes": (not has_byok) and chain,
            "has_external_api_key": has_byok or (chain and bool(probe_allows))}


class SignInCredentialRouteRowTest(unittest.TestCase):
    """apex-sk8i SA-1 (spec r2 §4 SA-1; §2.2 S1 as narrowed by RR-32 F-4): a
    reachable row's own env_key that is its route's credential_variable or a
    SWEEP_EXCLUDED_NAMES name reaches the child, so on-prem-only and mixed
    route sets hold a row key (BYOK) and sign in without a probe. RED on
    c3812b21: the child holds only the route credentials (T9's deterministic
    refusal and its probe race), and resolve_signin_credentials does not
    exist."""

    # One on-prem-shaped row (plan r3 §4.3.1: env_key is the placeholder and
    # an env_http_headers variable authenticates); a test-only header variable
    # stands in for the real one.
    ONPREM_CFG = ('''base_url = "%(a)s/v1"
models_base_url = "%(a)s/v1"
[features]
turn_summary = false

[model."row-onprem"]
model = "m-onprem"
base_url = "%(a)s/v1"
api_backend = "responses"
env_key = "GROK_DIRECT_PLACEHOLDER_KEY"

[model."row-onprem".env_http_headers]
Authorization = "WIRETAP_T2_KEY_HDR"
''')

    # The same row plus a row declaring neither credential: its route resolves
    # through the fallback chain (T9's mixed route set).
    MIXED_CFG = ONPREM_CFG + ('''
[model."row-chain"]
model = "m-chain"
base_url = "%(a)s/v1"
api_backend = "responses"
''')

    ONPREM_STEPS = [{"op": "turn", "prompt": "ping"}]
    MIXED_STEPS = [{"op": "turn", "prompt": "ping", "model": "row-chain"}]

    def _plants(self):
        """(NAME, value) for every synthetic value this file plants."""
        return ([(k, _t2_env_plant(k)) for k in T2_TEST_ONLY_KEYS]
                + [(k, _t3_env_plant(k)) for k in sorted(T3_SWEPT_PLANTS)])

    def _credential_names(self, launch):
        return sorted(set(launch["env_names"])
                      - set(run.CHILD_ENV_NON_CREDENTIAL_ALLOWLIST))

    def _run_e2e(self, cfg, steps):
        """run_case end-to-end through the real wiretap and a recording
        upstream; the fake binary marks that it ran and the launch spy records
        each launch of it. Returns (row, launches, ran_marker, row_routes)."""
        tmp = tempfile.mkdtemp(prefix="rt-sk8i-s1-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        srv, upstream = _start_recording_upstream()
        self.addCleanup(srv.shutdown)
        cfg_text = cfg % {"a": upstream}
        live = _t2_write_cfg_home(tmp, cfg_text)
        ran = os.path.join(tmp, "fake-grok.ran")
        fake_bin = _write_fake_binary(tmp, upstream, model="m-onprem",
                                      ran_record=ran)
        launches = _spy_binary_launches(self, fake_bin)
        args, _out = _t2_case_args(tmp, live, fake_bin)
        case = {"id": "RT-SK8I-S1", "title": "sign-in row key",
                "driver": "headless", "model": "row-onprem",
                "steps": steps, "wirecap": True, "est_calls": 1,
                "assert": {"exit": 0}}
        row = run.run_case(case, args, run.Budget(args.budget))
        return row, launches, ran, run.derive_row_routes(cfg_text, None)

    def _home(self, tmp, label, cfg_text):
        live = _t2_write_cfg_home(os.path.join(tmp, label), cfg_text)
        return run.HermeticHome(os.path.join(tmp, label, "root"), live, {},
                                None)

    def test_onprem_route_set_child_env_holds_its_row_key(self):
        """spec r2 §3 F-1, F-8: the on-prem-only child holds its row's own key
        beside the route's header variable, so the gate reads BYOK and never
        probes; nothing else credential-shaped rides in, and no plant reaches
        argv."""
        _t2_env_guard(self)
        _t3_swept_env_guard(self, names=("GROK_DIRECT_PLACEHOLDER_KEY",))
        _row, launches, ran, rows = self._run_e2e(self.ONPREM_CFG,
                                                  self.ONPREM_STEPS)
        self.assertTrue(os.path.exists(ran),
                        "the fake binary must have run (no ran marker)")
        self.assertEqual(len(launches), 1,
                         "one launch of the binary is expected, got %d"
                         % len(launches))
        names = self._credential_names(launches[0])
        gate = _binary_signin_gate(launches[0]["env_names"], rows)
        self.assertEqual(
            names, ["GROK_DIRECT_PLACEHOLDER_KEY", "WIRETAP_T2_KEY_HDR"],
            "the child's credential names must be exactly the row's own key "
            "and the route's header variable; gate model: has_byok=%s "
            "probes=%s has_external_api_key=%s"
            % (gate["has_byok"], gate["probes"],
               gate["has_external_api_key"]))
        self.assertTrue(gate["has_byok"], "the gate must read BYOK")
        self.assertFalse(gate["probes"], "a row key means no probe")
        self.assertTrue(gate["has_external_api_key"],
                        "the gate must accept the child without a probe")
        self.assertEqual(launches[0]["grok_auth_expired"], "1",
                         "GROK_AUTH_EXPIRED is unchanged")
        _t3_assert_absent(self, launches[0]["argv"], self._plants(),
                          "the binary's argv")

    def test_mixed_route_set_needs_no_probe(self):
        """spec r2 §3 F-2, F-8, a test about chain resolution: the chain names
        are planted from T3_SWEPT_PLANTS under the guard, so the first set
        rung is run.XAI_CREDENTIAL_CHAIN[0]. The child holds the on-prem row's
        key, so the gate never probes although a chain rung is present."""
        _t2_env_guard(self)
        _t3_swept_env_guard(self, names=tuple(run.XAI_CREDENTIAL_CHAIN)
                            + ("GROK_DIRECT_PLACEHOLDER_KEY",))
        _row, launches, ran, rows = self._run_e2e(self.MIXED_CFG,
                                                  self.MIXED_STEPS)
        self.assertTrue(os.path.exists(ran),
                        "the fake binary must have run (no ran marker)")
        self.assertEqual(len(launches), 1,
                         "one launch of the binary is expected, got %d"
                         % len(launches))
        rung = run.XAI_CREDENTIAL_CHAIN[0]
        names = self._credential_names(launches[0])
        gate = _binary_signin_gate(launches[0]["env_names"], rows)
        self.assertEqual(
            names, sorted(["GROK_DIRECT_PLACEHOLDER_KEY", "WIRETAP_T2_KEY_HDR",
                           rung]),
            "the child's credential names must be exactly the header "
            "variable, the first set chain rung and the on-prem row's own "
            "key; gate model: has_byok=%s probes=%s has_external_api_key=%s"
            % (gate["has_byok"], gate["probes"],
               gate["has_external_api_key"]))
        self.assertTrue(rung in names, "a chain rung is in the child")
        self.assertFalse(gate["probes"],
                         "no probe although a chain rung is present "
                         "(api_key_probe.rs:36)")
        self.assertTrue(gate["has_byok"], "the gate must read BYOK")
        self.assertEqual(launches[0]["grok_auth_expired"], "1",
                         "GROK_AUTH_EXPIRED is unchanged")
        _t3_assert_absent(self, launches[0]["argv"], self._plants(),
                          "the binary's argv")

    def test_resolve_signin_credentials_s1_unit(self):
        """S1 as a unit over three route sets, values compared by digest only:
        (1) on-prem and (2) mixed hand the placeholder as the route row's key;
        (3) T2_TWO_ORIGIN_CFG with row-x and row-tf hands WIRETAP_T2_KEY_X
        only: row-tf's env_key WIRETAP_T2_KEY_B is neither its route's
        credential_variable (the header variable wins) nor sweep-excluded
        (RR-32 F-4). Leg (3)'s env holds values for both (RR-34 F-2 (b))."""
        _t2_env_guard(self)
        _t3_swept_env_guard(self, names=tuple(run.XAI_CREDENTIAL_CHAIN)
                            + ("GROK_DIRECT_PLACEHOLDER_KEY",))
        tmp = tempfile.mkdtemp(prefix="rt-sk8i-s1u-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        held = {"GROK_DIRECT_PLACEHOLDER_KEY":
                _t3_env_plant("GROK_DIRECT_PLACEHOLDER_KEY"),
                "WIRETAP_T2_KEY_HDR": _t2_env_plant("WIRETAP_T2_KEY_HDR"),
                run.XAI_CREDENTIAL_CHAIN[0]:
                _t3_env_plant(run.XAI_CREDENTIAL_CHAIN[0])}
        legs = (
            ("(1) on-prem", self.ONPREM_CFG, ["row-onprem"]),
            ("(2) mixed", self.MIXED_CFG, ["row-onprem", "row-chain"]))
        for label, cfg, selectors in legs:
            home = self._home(tmp, label[1], cfg % {"a": "http://127.0.0.1:9"})
            route_map, route_causes = home.route_map(selectors, [])
            self.assertEqual(route_causes, [], "%s: the map resolves" % label)
            values, source, causes = run.resolve_signin_credentials(
                route_map, home.row_routes(), env=held)
            self.assertEqual(sorted(values), ["GROK_DIRECT_PLACEHOLDER_KEY"],
                             "%s: S1 hands the route row's own key" % label)
            self.assertEqual((source, causes), ("route-row", []),
                             "%s: source and causes" % label)
            _t3_assert_value(self, values.get("GROK_DIRECT_PLACEHOLDER_KEY"),
                             held["GROK_DIRECT_PLACEHOLDER_KEY"],
                             "%s: the handed value is the planted one" % label)
        home = self._home(tmp, "3", T2_TWO_ORIGIN_CFG % {
            "a": "http://127.0.0.1:9", "b": "http://127.0.0.1:9"})
        route_map, route_causes = home.route_map(["row-x", "row-tf"], [])
        self.assertEqual(route_causes, [], "(3): the map resolves")
        t2_held = {k: _t2_env_plant(k) for k in
                   ("WIRETAP_T2_KEY_X", "WIRETAP_T2_KEY_B",
                    "WIRETAP_T2_KEY_HDR")}
        values, source, causes = run.resolve_signin_credentials(
            route_map, home.row_routes(), env=t2_held)
        self.assertEqual(sorted(values), ["WIRETAP_T2_KEY_X"],
                         "(3): row-tf's env_key is not its route credential "
                         "and not sweep-excluded, so it is not handed")
        self.assertEqual((source, causes), ("route-row", []),
                         "(3): source and causes")
        _t3_assert_value(self, values.get("WIRETAP_T2_KEY_X"),
                         t2_held["WIRETAP_T2_KEY_X"],
                         "(3): the handed value is the planted one")

    def test_no_route_and_key_free_paths_unchanged(self):
        """spec r2 §3 F-7: an empty route map (no selector resolves) and no
        map at all (the key-free path) hand nothing and block nothing, even
        with a sweep-excluded key available; KeyFreeModeUnchangedTest is the
        end-to-end twin."""
        rows = run.derive_row_routes(
            self.ONPREM_CFG % {"a": "http://127.0.0.1:9"}, None)
        held = {"GROK_DIRECT_PLACEHOLDER_KEY":
                _t3_env_plant("GROK_DIRECT_PLACEHOLDER_KEY")}
        for label, route_map in (("empty map", {}),
                                 ("no map (key-free)", None)):
            result = run.resolve_signin_credentials(route_map, rows, env=held)
            _t3_assert_value(self, result, ({}, None, []),
                             "%s: ({}, None, []) expected" % label)

    def test_route_map_and_recheck_credentials_unchanged(self):
        """GUARD (green before and after; spec r2 §3 F-10): the sign-in rule
        changes neither the routing authority nor the recheck's credential.
        WIRETAP_ROUTE_MAP stays byte-equal to json.dumps(route_map,
        sort_keys=True) of the case's own route map, and
        route_out["credential_values"] holds exactly the route credentials."""
        _t2_env_guard(self)
        _t3_swept_env_guard(self, names=tuple(run.XAI_CREDENTIAL_CHAIN)
                            + ("GROK_DIRECT_PLACEHOLDER_KEY",))
        real = run.Wiretap
        maps = []

        class MapRecordingWiretap(real):
            def __init__(self, *a, **kw):
                super().__init__(*a, **kw)
                maps.append(self.env.get("WIRETAP_ROUTE_MAP"))

        run.Wiretap = MapRecordingWiretap
        self.addCleanup(setattr, run, "Wiretap", real)
        legs = (("on-prem", self.ONPREM_CFG, ["row-onprem"], self.ONPREM_STEPS),
                ("mixed", self.MIXED_CFG, ["row-onprem", "row-chain"],
                 self.MIXED_STEPS))
        for label, cfg, selectors, steps in legs:
            del maps[:]
            tmp = tempfile.mkdtemp(prefix="rt-sk8i-guard-")
            self.addCleanup(lambda t=tmp: shutil.rmtree(t, ignore_errors=True))
            srv, upstream = _start_recording_upstream()
            self.addCleanup(srv.shutdown)
            live = _t2_write_cfg_home(tmp, cfg % {"a": upstream})
            fake_bin = _write_fake_binary(tmp, upstream, model="m-onprem")
            args, out = _t2_case_args(tmp, live, fake_bin)
            case = {"id": "RT-SK8I-GUARD", "title": "routing unchanged",
                    "driver": "headless", "model": "row-onprem",
                    "steps": steps, "wirecap": True, "est_calls": 1,
                    "assert": {"exit": 0}}
            route_out = {}
            run._run_case_once(case, args, run.Budget(args.budget),
                               os.path.join(out, "rt-sk8i-guard"), 1,
                               route_out)
            expect_home = run.HermeticHome(os.path.join(tmp, "expect"), live,
                                           {}, None)
            expect_map, expect_causes = expect_home.route_map(selectors, [])
            self.assertEqual(expect_causes, [], "%s: the map resolves" % label)
            self.assertEqual(len(maps), 1,
                             "%s: one wiretap is started, got %d"
                             % (label, len(maps)))
            self.assertEqual(maps[0], json.dumps(expect_map, sort_keys=True),
                             "%s: WIRETAP_ROUTE_MAP is the case's own route "
                             "map, byte for byte" % label)
            self.assertEqual(route_out.get("route_map"), expect_map,
                             "%s: route_out's route map is unchanged" % label)
            expect_values, _expect_causes = run.resolve_route_credentials(
                expect_map)
            self.assertEqual(sorted(route_out["credential_values"]),
                             sorted(expect_values),
                             "%s: the recheck authenticates with the route "
                             "credentials only (r3a A13)" % label)
            self.assertNotIn("GROK_DIRECT_PLACEHOLDER_KEY",
                             sorted(route_out["credential_values"]),
                             "%s: no sign-in-only variable in route_out"
                             % label)
            _t3_assert_value(self, route_out["credential_values"],
                             expect_values,
                             "%s: route_out's credential values" % label)


# The spec r2 §3 F-6 test-local constant (RR-32 F-5): T2_ORPHAN_CFG, unedited,
# plus one row no case selects that declares the sweep-excluded placeholder as
# its env_key, so S2 has a set candidate it must leave alone when S1 found a
# row key.
T2_ORPHAN_PLACEHOLDER_CFG = T2_ORPHAN_CFG + ('''
[model."row-onprem"]
model = "m-onprem"
base_url = "%(a)s/v1"
api_backend = "responses"
env_key = "GROK_DIRECT_PLACEHOLDER_KEY"
''')


class SignInCredentialFallbackTest(unittest.TestCase):
    """apex-sk8i SA-2 (spec r2 §4 SA-2; §2.2 S2, S3 with RR-33 D-8 (a)'s
    trigger, and the names-only sign-in log line). When no reachable row hands
    its own key, the child gets the first set SWEEP_EXCLUDED_NAMES variable
    some row declares as its env_key (S2); a route set left without one BLOCKS
    before its first step with a named credential-unset cause (S3), except
    RR-33's carve-out (no chain route and no declared candidate). RED on SA-1
    GREEN: a chain-only child holds no row key so the probe decides, nothing
    BLOCKS, and no sign-in line is logged."""

    # A chain row (neither credential), an unreachable on-prem-shaped row (the
    # placeholder env_key plus a header variable) and an unreachable
    # proxy-shaped row whose env_key is a test-only stand-in for a real key.
    CHAIN_CFG = ('''base_url = "%(a)s/v1"
models_base_url = "%(a)s/v1"
[features]
turn_summary = false

[model."row-chain"]
model = "m-chain"
base_url = "%(a)s/v1"
api_backend = "responses"

[model."row-onprem"]
model = "m-onprem"
base_url = "%(a)s/v1"
api_backend = "responses"
env_key = "GROK_DIRECT_PLACEHOLDER_KEY"

[model."row-onprem".env_http_headers]
Authorization = "WIRETAP_T2_KEY_HDR"

[model."row-proxy"]
model = "m-proxy"
base_url = "%(a)s/v1"
api_backend = "responses"
env_key = "WIRETAP_T2_KEY_B"
''')

    # Chain rows only: no row declares any env_key (spec r2 §3 F-5).
    CHAIN_ONLY_CFG = ('''base_url = "%(a)s/v1"
models_base_url = "%(a)s/v1"
[features]
turn_summary = false

[model."row-chain"]
model = "m-chain"
base_url = "%(a)s/v1"
api_backend = "responses"

[model."row-chain-b"]
model = "m-chain-b"
base_url = "%(a)s/v1"
api_backend = "responses"
''')

    # Two sweep-excluded env_key rows in a fixed config order, neither
    # reachable from row-chain (spec r2 §3 F-9). The test-only name is first
    # in the config and second in the extended SWEEP_EXCLUDED_NAMES, so the
    # pick can only follow the config.
    ORDER_CFG = ('''base_url = "%(a)s/v1"
models_base_url = "%(a)s/v1"
[features]
turn_summary = false

[model."row-chain"]
model = "m-chain"
base_url = "%(a)s/v1"
api_backend = "responses"

[model."row-first"]
model = "m-first"
base_url = "%(a)s/v1"
api_backend = "responses"
env_key = "WIRETAP_T2_KEY_AMB"

[model."row-second"]
model = "m-second"
base_url = "%(a)s/v1"
api_backend = "responses"
env_key = "GROK_DIRECT_PLACEHOLDER_KEY"
''')

    CHAIN_STEPS = [{"op": "turn", "prompt": "ping"}]
    CHAIN_NAMES = tuple(run.XAI_CREDENTIAL_CHAIN) + (
        "GROK_DIRECT_PLACEHOLDER_KEY",)

    def _plants(self):
        """(NAME, value) for every synthetic value this class plants."""
        return ([(k, _t2_env_plant(k)) for k in T2_TEST_ONLY_KEYS]
                + [(k, _t3_env_plant(k)) for k in sorted(T3_SWEPT_PLANTS)]
                + [(k, _t2_env_plant(k))
                   for k in ChildEnvAllowlistTest.PLANTED])

    def _plant_undeclared(self):
        """ChildEnvAllowlistTest.PLANTED's ten names, which no row declares and
        the allowlist does not hold, set to that class's own synthetic
        spelling (_t2_env_plant's) and restored on cleanup (spec r2 §3 F-8)."""
        saved = {k: os.environ.get(k) for k in ChildEnvAllowlistTest.PLANTED}
        for k in ChildEnvAllowlistTest.PLANTED:
            os.environ[k] = _t2_env_plant(k)

        def restore():
            for k, v in saved.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
        self.addCleanup(restore)

    def _credential_names(self, launch):
        return sorted(set(launch["env_names"])
                      - set(run.CHILD_ENV_NON_CREDENTIAL_ALLOWLIST))

    def _home(self, tmp, label, cfg_text):
        live = _t2_write_cfg_home(os.path.join(tmp, label), cfg_text)
        return run.HermeticHome(os.path.join(tmp, label, "root"), live, {},
                                None)

    def _run_e2e(self, cfg, selector, wire_model, steps, asserts=None,
                 route_out=None):
        """One case end-to-end through the real wiretap and a recording
        upstream: run_case, or _run_case_once when the test reads route_out.
        The fake binary marks that it ran and the launch spy records each
        launch of it. Returns the row, the launches, the ran-marker path, the
        config's row routes, the POST count, the live home and the tmp dir."""
        tmp = tempfile.mkdtemp(prefix="rt-sk8i-s2-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        srv, upstream = _start_recording_upstream()
        self.addCleanup(srv.shutdown)
        cfg_text = cfg % {"a": upstream}
        live = _t2_write_cfg_home(tmp, cfg_text)
        ran = os.path.join(tmp, "fake-grok.ran")
        fake_bin = _write_fake_binary(tmp, upstream, model=wire_model,
                                      ran_record=ran)
        launches = _spy_binary_launches(self, fake_bin)
        args, out = _t2_case_args(tmp, live, fake_bin)
        case = {"id": "RT-SK8I-S2", "title": "sign-in fallback",
                "driver": "headless", "model": selector, "steps": steps,
                "wirecap": True, "est_calls": 1,
                "assert": {"exit": 0} if asserts is None else asserts}
        if route_out is None:
            row = run.run_case(case, args, run.Budget(args.budget))
        else:
            row = run._run_case_once(case, args, run.Budget(args.budget),
                                     os.path.join(out, "rt-sk8i-s2"), 1,
                                     route_out)
        with srv.lock:
            post_count = sum(1 for r in srv.requests if r["method"] == "POST")
        return {"row": row, "launches": launches, "ran": ran,
                "rows": run.derive_row_routes(cfg_text, None),
                "post_count": post_count, "live": live, "tmp": tmp}

    def test_chain_only_route_set_child_env_holds_the_placeholder(self):
        """spec r2 §3 F-3, F-8, F-10; a test about chain resolution, so the
        chain names are planted from T3_SWEPT_PLANTS under the guard and the
        first set rung is run.XAI_CREDENTIAL_CHAIN[0]. A case reaching only the
        chain row gets S2's declared placeholder beside its rung, so the gate
        reads BYOK and never probes. The unreachable rows' variables and
        ChildEnvAllowlistTest.PLANTED's ten names stay out, no plant reaches
        argv, and WIRETAP_ROUTE_MAP is the case's own route map."""
        _t2_env_guard(self)
        _t3_swept_env_guard(self, names=self.CHAIN_NAMES)
        self._plant_undeclared()
        real = run.Wiretap
        maps = []

        class MapRecordingWiretap(real):
            def __init__(self, *a, **kw):
                super().__init__(*a, **kw)
                maps.append(self.env.get("WIRETAP_ROUTE_MAP"))

        run.Wiretap = MapRecordingWiretap
        self.addCleanup(setattr, run, "Wiretap", real)
        res = self._run_e2e(self.CHAIN_CFG, "row-chain", "m-chain",
                            self.CHAIN_STEPS)
        launches = res["launches"]
        self.assertTrue(os.path.exists(res["ran"]),
                        "the fake binary must have run (no ran marker)")
        self.assertEqual(len(launches), 1,
                         "one launch of the binary is expected, got %d"
                         % len(launches))
        rung = run.XAI_CREDENTIAL_CHAIN[0]
        names = self._credential_names(launches[0])
        gate = _binary_signin_gate(launches[0]["env_names"], res["rows"])
        self.assertEqual(
            names, sorted([rung, "GROK_DIRECT_PLACEHOLDER_KEY"]),
            "the chain-only child's credential names must be exactly the "
            "first set chain rung and the declared placeholder; gate model: "
            "has_byok=%s probes=%s has_external_api_key=%s"
            % (gate["has_byok"], gate["probes"],
               gate["has_external_api_key"]))
        for name in (("WIRETAP_T2_KEY_B", "WIRETAP_T2_KEY_HDR")
                     + tuple(ChildEnvAllowlistTest.PLANTED)):
            self.assertNotIn(name, launches[0]["env_names"],
                             "%s must not reach the child" % name)
        self.assertTrue(gate["has_byok"], "the gate must read BYOK")
        self.assertFalse(gate["probes"],
                         "a row key means no probe, although a chain rung "
                         "is present (api_key_probe.rs:36)")
        self.assertTrue(gate["has_external_api_key"],
                        "the gate must accept the child without a probe")
        self.assertEqual(launches[0]["grok_auth_expired"], "1",
                         "GROK_AUTH_EXPIRED is unchanged")
        _t3_assert_absent(self, launches[0]["argv"], self._plants(),
                          "the binary's argv")
        expect_home = run.HermeticHome(os.path.join(res["tmp"], "expect"),
                                       res["live"], {}, None)
        expect_map, expect_causes = expect_home.route_map(["row-chain"], [])
        self.assertEqual(expect_causes, [], "the map resolves")
        self.assertEqual(len(maps), 1,
                         "one wiretap is started, got %d" % len(maps))
        self.assertEqual(maps[0], json.dumps(expect_map, sort_keys=True),
                         "WIRETAP_ROUTE_MAP is the case's own route map, "
                         "byte for byte (spec r2 §3 F-10)")

    def test_placeholder_unset_blocks_before_the_first_step(self):
        """spec r2 §3 F-4 (chain resolution): the placeholder is declared but
        unset, so S2 has a candidate and no value and S3 BLOCKS the case before
        its first step with that name; nothing is forwarded and the binary
        never runs."""
        _t2_env_guard(self)
        _t3_swept_env_guard(self, names=tuple(run.XAI_CREDENTIAL_CHAIN))
        _t3_env_unset(self, "GROK_DIRECT_PLACEHOLDER_KEY")
        res = self._run_e2e(self.CHAIN_CFG, "row-chain", "m-chain",
                            self.CHAIN_STEPS)
        row = res["row"]
        self.assertEqual(row["status"], "BLOCKED",
                         "a chain-only case whose declared placeholder is "
                         "unset must BLOCK before its first step")
        self.assertEqual(row.get("blocked_reason"),
                         "credential-unset: GROK_DIRECT_PLACEHOLDER_KEY "
                         "(sign-in)")
        self.assertEqual(res["post_count"], 0,
                         "nothing may be forwarded on a BLOCKED sign-in")
        self.assertFalse(os.path.exists(res["ran"]),
                         "the fake binary must never run (ran marker exists)")
        self.assertEqual(len(res["launches"]), 0,
                         "the launch spy must record no launch, got %d"
                         % len(res["launches"]))

    def test_no_declared_placeholder_blocks_with_the_named_cause(self):
        """spec r2 §3 F-5 (chain resolution): no row declares a sweep-excluded
        env_key, so S2 has no candidate even with the placeholder set in the
        runner's environment, and the chain route makes S3 fire with the
        no-declaration cause; nothing is forwarded."""
        _t3_swept_env_guard(self, names=self.CHAIN_NAMES)
        res = self._run_e2e(self.CHAIN_ONLY_CFG, "row-chain", "m-chain",
                            self.CHAIN_STEPS)
        row = res["row"]
        self.assertEqual(row["status"], "BLOCKED",
                         "a chain-only case in a config declaring no "
                         "sweep-excluded env_key must BLOCK")
        self.assertEqual(row.get("blocked_reason"),
                         "credential-unset: sign-in (no row declares a "
                         "sweep-excluded env_key)")
        self.assertEqual(res["post_count"], 0,
                         "nothing may be forwarded on a BLOCKED sign-in")

    def test_fallback_choice_follows_config_order(self):
        """spec r2 §3 F-9 and RR-34 F-1's leg; values are compared by digest
        only. SWEEP_EXCLUDED_NAMES is extended (restored on cleanup) by a
        test-only name ORDER_CFG declares before the placeholder. Over a map
        reaching only the chain row, S2 hands the first candidate in config
        order on every call; with both unset, S3 names both, '|'-joined, in
        config order. RR-34 F-1: over SA-1's on-prem fixture, whose one route
        carries a credential_variable (no chain route), the declared candidate
        alone triggers S3 when the placeholder is unset."""
        tmp = tempfile.mkdtemp(prefix="rt-sk8i-s2u-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        saved = run.SWEEP_EXCLUDED_NAMES
        self.addCleanup(setattr, run, "SWEEP_EXCLUDED_NAMES", saved)
        run.SWEEP_EXCLUDED_NAMES = saved + ("WIRETAP_T2_KEY_AMB",)
        home = self._home(tmp, "order",
                          self.ORDER_CFG % {"a": "http://127.0.0.1:9"})
        route_map, route_causes = home.route_map(["row-chain"], [])
        self.assertEqual(route_causes, [], "the order map resolves")
        self.assertEqual(sorted(route_map), ["m-chain"],
                         "the order map reaches only the chain row")
        held = {"WIRETAP_T2_KEY_AMB": _t2_env_plant("WIRETAP_T2_KEY_AMB"),
                "GROK_DIRECT_PLACEHOLDER_KEY":
                _t3_env_plant("GROK_DIRECT_PLACEHOLDER_KEY")}
        for call in range(3):
            values, source, causes = run.resolve_signin_credentials(
                route_map, home.row_routes(), env=held)
            self.assertEqual(sorted(values), ["WIRETAP_T2_KEY_AMB"],
                             "call %d: S2 hands the first candidate in "
                             "config order" % call)
            _t3_assert_value(self, values.get("WIRETAP_T2_KEY_AMB"),
                             held["WIRETAP_T2_KEY_AMB"],
                             "call %d: the handed value is the planted one"
                             % call)
            self.assertEqual((source, causes), ("declared-placeholder", []),
                             "call %d: source and causes" % call)
        for call in range(3):
            result = run.resolve_signin_credentials(
                route_map, home.row_routes(), env={})
            self.assertEqual(
                result,
                ({}, None, ["credential-unset: WIRETAP_T2_KEY_AMB"
                            "|GROK_DIRECT_PLACEHOLDER_KEY (sign-in)"]),
                "call %d: with both unset, S3 names both candidates in "
                "config order" % call)
        run.SWEEP_EXCLUDED_NAMES = saved
        onprem = self._home(tmp, "onprem",
                            SignInCredentialRouteRowTest.ONPREM_CFG
                            % {"a": "http://127.0.0.1:9"})
        route_map, route_causes = onprem.route_map(["row-onprem"], [])
        self.assertEqual(route_causes, [], "the on-prem map resolves")
        self.assertTrue(route_map and all(
            r.get("credential_variable") for r in route_map.values()),
            "every reachable on-prem route carries a credential_variable")
        result = run.resolve_signin_credentials(
            route_map, onprem.row_routes(), env={})
        self.assertEqual(
            result,
            ({}, None, ["credential-unset: GROK_DIRECT_PLACEHOLDER_KEY "
                        "(sign-in)"]),
            "RR-34 F-1: no chain route, the placeholder declared and unset: "
            "the candidates disjunct alone triggers S3")

    def test_proxy_row_case_child_env_unchanged(self):
        """GUARD (green before and after; spec r2 §3 F-6, RR-32 F-5, RR-34 F-2
        (c)). A case reaching T2_ORPHAN_CFG's row-x, whose env_key is its own
        route credential, in a config that also declares the placeholder on an
        unreachable row, with only the placeholder planted by the swept guard
        and _t2_env_guard supplying row-x's test-only key so the case
        launches. S1 finds row-x's key, so S2 adds nothing: the child's
        credential names are exactly the route credentials."""
        _t2_env_guard(self)
        _t3_swept_env_guard(self, names=("GROK_DIRECT_PLACEHOLDER_KEY",))
        res = self._run_e2e(T2_ORPHAN_PLACEHOLDER_CFG, "row-x", "m-x",
                            self.CHAIN_STEPS)
        launches = res["launches"]
        self.assertTrue(os.path.exists(res["ran"]),
                        "the fake binary must have run (no ran marker)")
        self.assertEqual(len(launches), 1,
                         "one launch of the binary is expected, got %d"
                         % len(launches))
        expect_home = run.HermeticHome(os.path.join(res["tmp"], "expect"),
                                       res["live"], {}, None)
        expect_map, expect_causes = expect_home.route_map(["row-x"], [])
        self.assertEqual(expect_causes, [], "the map resolves")
        self.assertEqual(
            self._credential_names(launches[0]),
            sorted(run.resolve_route_credentials(expect_map)[0]),
            "the child's credential names must be exactly the route "
            "credentials")
        self.assertNotIn("GROK_DIRECT_PLACEHOLDER_KEY",
                         launches[0]["env_names"],
                         "S2 must not add the placeholder when S1 found a "
                         "row key")
        _t3_assert_absent(self, launches[0]["argv"], self._plants(),
                          "the binary's argv")

    def test_signin_log_line_carries_names_only(self):
        """spec r2 §3 F-11 and F-10 (RR-32 F-9, RR-34 F-2 (a)); its S2 case is
        about chain resolution. run.log is captured over three end-to-end
        cases. SA-1's on-prem fixture (S1) logs exactly one sign-in line,
        naming the placeholder and ending (route-row); this class's chain-only
        fixture (S2) logs exactly one, naming the placeholder and ending
        (declared-placeholder); both keep route_out's credentials the routes'
        own. A case whose explicit selector resolves to no row, with no steps
        (GOLDEN-SMOKE-01's shape), passes the sign-in call site with an empty
        map and logs none. No plant value occurs in any captured line."""
        _t2_env_guard(self)
        _t3_swept_env_guard(self, names=self.CHAIN_NAMES)
        logged = []
        real_log = run.log
        run.log = logged.append
        self.addCleanup(setattr, run, "log", real_log)
        legs = (("S1", SignInCredentialRouteRowTest.ONPREM_CFG, "row-onprem",
                 "m-onprem", SignInCredentialRouteRowTest.ONPREM_STEPS,
                 "(route-row)"),
                ("S2", self.CHAIN_CFG, "row-chain", "m-chain",
                 self.CHAIN_STEPS, "(declared-placeholder)"))
        for label, cfg, selector, wire_model, steps, ending in legs:
            del logged[:]
            route_out = {}
            self._run_e2e(cfg, selector, wire_model, steps,
                          route_out=route_out)
            _t3_assert_absent(self, logged, self._plants(),
                              "%s: the captured log lines" % label)
            signin = [m for m in logged if "sign-in credential" in m]
            self.assertEqual(len(signin), 1,
                             "%s: exactly one sign-in credential line is "
                             "expected, got %d" % (label, len(signin)))
            self.assertIn("GROK_DIRECT_PLACEHOLDER_KEY", signin[0],
                          "%s: the line names the handed variable" % label)
            self.assertTrue(signin[0].endswith(ending),
                            "%s: the line ends %s" % (label, ending))
            self.assertNotIn("GROK_DIRECT_PLACEHOLDER_KEY",
                             sorted(route_out.get("credential_values") or {}),
                             "%s: route_out keeps the route credentials only"
                             % label)
        del logged[:]
        route_out = {}
        self._run_e2e(self.CHAIN_CFG, "row-unknown", "m-chain", [],
                      asserts={}, route_out=route_out)
        _t3_assert_absent(self, logged, self._plants(),
                          "{}: the captured log lines")
        self.assertEqual(route_out.get("route_map"), {},
                         "{}: the explicit selector resolves to no row and "
                         "the case passed the sign-in call site")
        signin = [m for m in logged if "sign-in credential" in m]
        self.assertEqual(len(signin), 0,
                         "{}: no sign-in credential line is expected, got %d"
                         % len(signin))

    def test_blocked_signin_cause_reads_as_harness_auth_failure(self):
        """spec r2 §3 F-4's row read through the existing R-12 triage (chain
        resolution): the sign-in cause starts with credential-unset and the
        case captured no request, so _triage_block's auth_class is
        harness-auth-failure. This item does not edit the triage; the test
        proves it need not."""
        _t2_env_guard(self)
        _t3_swept_env_guard(self, names=tuple(run.XAI_CREDENTIAL_CHAIN))
        _t3_env_unset(self, "GROK_DIRECT_PLACEHOLDER_KEY")
        res = self._run_e2e(self.CHAIN_CFG, "row-chain", "m-chain",
                            self.CHAIN_STEPS)
        row = res["row"]
        triage = row.get("triage") or {}
        self.assertEqual(triage.get("auth_class"), run.AUTH_CLASS_LOCAL,
                         "the blocked sign-in row must read as a harness auth "
                         "failure; row status %s, triage block %s"
                         % (row["status"],
                            "present" if row.get("triage") else "absent"))
        self.assertTrue((row.get("blocked_reason") or "").endswith(
            "(sign-in)"), "the BLOCK is the sign-in cause")


class SkShapeBoundaryTest(unittest.TestCase):
    """apex-au7k SA-3 (spec r2 §4 SA-3; §2.3 (B) as RR-32 F-6 words it).
    `SK_SHAPE` holds three conditions in one pattern: the candidate `sk`
    follows a non-alphanumeric character (the house left boundary) or a JSON
    escape (a backslash plus n, t or r), and it does not continue a base64url
    run (a `-` or `_` right before it, itself preceded by a base64url
    character). RED on SA-2 GREEN, where `SK_SHAPE` is as at 7622c21a with no
    left boundary: identifier substrings and base64url runs each draw one
    `sk-shape` detail, and a key glued after a dropped run is digested
    together with the run's tail. The three GUARDs pin what the pattern must
    keep: the true-positive forms, a key after a JSON escape and the
    fragment-free detail. Every key-shaped literal is a concatenation, every
    declared value is unset, and the assertions count details by pattern name
    and print counts, labels and digests only."""

    # 20 base64url characters with no `sk` in any case: the tail the synthetic
    # keys and runs below share.
    TAIL = "Q1w2E3r4T5y6U7i8O9p0"
    # A ciphertext-like base64url run (Fernet-style prefix), with no `sk` in
    # any case and no JWT prefix; it ends in a letter.
    RUN = "gAAAAABpQx7Rm2Vt9LwZ" + "0aHc4UeJ1nYb8DfG3iKo" + "5TqW6pXz"
    # spec r2 §3 F-12: a real tool name, a disk identifier and an upper-case
    # constant, each carrying `sk_` plus 16+ word characters inside a word.
    IDENTIFIERS = (("tool-name", "get_task_or_subagent_output"),
                   ("disk-identifier", "disk_free_kib_before_after_check"),
                   ("upper-case-constant", "TASK_RESULT_DIRECTORY_FOR_AGENT"))
    # §3 F-13: the embedded `sk` glued after a letter of the run.
    ALNUM_PRECEDED = RUN + "sk" + "-" + TAIL + "Ab9Cd8Ef7"
    # §3 F-14: the embedded `sk` after `b_` or `b-`, which the boundary alone
    # accepts and only the base64url lookbehind rejects.
    CONTINUATIONS = (
        ("underscore-then-dash", RUN + "b_" + "sk" + "-" + TAIL + "Ab9Cd8Ef7"),
        ("dash-then-underscore", RUN + "b-" + "sk" + "_" + TAIL + "Ab9Cd8Ef7"))
    # §3 F-25: a run ending in a dropped candidate, a `.`, then a standalone
    # key at the end of the line.
    STANDALONE_KEY = "sk" + "-" + "Zx8Cv7Bn6Mm5Ll4Kj3Hh2"
    DROPPED_RUN_THEN_KEY = (RUN + "b_" + "sk" + "-" + TAIL + "."
                            + STANDALONE_KEY)
    # §3 F-15: (label, key, line template) per true-positive context.
    TRUE_POSITIVES = (
        ("after-bearer", "sk" + "-" + "bearerform" + TAIL,
         "Authorization: Bearer %s"),
        ("inside-json-quotes", "sk" + "_" + "jsonquoted" + TAIL,
         '{"value": "%s"}'),
        ("after-equals", "sk" + "-" + "assignment" + TAIL, "LOCAL_VALUE=%s"),
        ("line-start", "sk" + "_" + "linestart" + TAIL, "%s trailing words"),
        ("upper-case-after-space", "SK" + "-" + "UPPERCASEFORM" + TAIL.upper(),
         "header value %s"),
        ("lone-dash-after-space", "sk" + "-" + "afteraflag" + TAIL,
         "flag -%s"))
    # §3 F-24: (label, raw separator, the JSON escape json.dumps writes for
    # it, key).
    ESCAPED = (
        ("newline", "\n", "\\n", "sk" + "-" + "newlineform" + TAIL),
        ("tab", "\t", "\\t", "sk" + "_" + "tabbedform" + TAIL),
        ("carriage-return", "\r", "\\r", "sk" + "-" + "returnform" + TAIL))

    def _sweep_files(self, *texts):
        """One one-line file per text in a fresh run dir, swept the way main()
        sweeps a run dir. Returns (hits, details)."""
        root = tempfile.mkdtemp(prefix="rt-sk8i-skshape-")
        self.addCleanup(lambda: shutil.rmtree(root, ignore_errors=True))
        for i, text in enumerate(texts):
            with open(os.path.join(root, "resp-%03d.jsonl" % i), "w") as fh:
                fh.write(text + "\n")
        return run.redaction_sweep(root, "")

    @staticmethod
    def _pattern_name(detail):
        """The `<pattern>` field of `path:line: <pattern> <sha256_12>`."""
        return detail.split(": ", 1)[1].split()[0]

    def _sk_shape_digests(self, *texts):
        """The `<sha256_12>` field of every `sk-shape` detail of a sweep."""
        _hits, found = self._sweep_files(*texts)
        return [x.split()[-1] for x in found
                if self._pattern_name(x) == "sk-shape"]

    def test_identifier_substrings_do_not_hit(self):
        """§3 F-12, one file per fixture. RED on SA-2 GREEN: one `sk-shape`
        detail per file."""
        _t3_swept_env_guard(self, unset=tuple(T3_SWEPT_PLANTS))
        counts = {}
        for label, text in self.IDENTIFIERS:
            counts[label] = len(self._sk_shape_digests(text))
        self.assertEqual(counts, {label: 0 for label, _t in self.IDENTIFIERS},
                         "sk-shape details per one-line identifier file: an "
                         "identifier substring is never a key")

    def test_base64url_alnum_preceded_sk_does_not_hit(self):
        """§3 F-13. RED on SA-2 GREEN: one `sk-shape` detail."""
        _t3_swept_env_guard(self, unset=tuple(T3_SWEPT_PLANTS))
        self.assertEqual(len(self._sk_shape_digests(self.ALNUM_PRECEDED)), 0,
                         "a base64url run with a letter before its embedded "
                         "sk draws no sk-shape detail")

    def test_base64url_run_continuation_does_not_hit(self):
        """§3 F-14, the `_` and the `-` variants, one file each. The boundary
        alone accepts both; only the base64url lookbehind rejects them
        (counterfactual (ii)). RED on SA-2 GREEN: one `sk-shape` detail each.
        Asserted on `sk-shape` only: the `key-heuristic` count the underscore
        variant draws is written down by the unit record, not asserted."""
        _t3_swept_env_guard(self, unset=tuple(T3_SWEPT_PLANTS))
        counts = {}
        for label, text in self.CONTINUATIONS:
            counts[label] = len(self._sk_shape_digests(text))
        self.assertEqual(counts,
                         {label: 0 for label, _t in self.CONTINUATIONS},
                         "sk-shape details per base64url continuation file")

    def test_key_after_a_dropped_run_still_hits(self):
        """§3 F-25 (RR-32 F-6). The run's own candidate is dropped, and the
        standalone key after the `.` still hits once, digested alone. RED on
        SA-2 GREEN: one detail whose digest covers the whole span from the
        dropped candidate's `sk` (the greedy tail runs through the key); a
        post-filter design gives zero (counterfactual (iv))."""
        _t3_swept_env_guard(self, unset=tuple(T3_SWEPT_PLANTS))
        digests = self._sk_shape_digests(self.DROPPED_RUN_THEN_KEY)
        self.assertEqual(len(digests), 1,
                         "exactly one sk-shape detail is expected, got %d"
                         % len(digests))
        self.assertEqual(digests[0], run.sha256_12(self.STANDALONE_KEY),
                         "the detail's digest must be the standalone key's "
                         "sha256_12 alone")

    def test_real_key_forms_still_hit_once(self):
        """GUARD, §3 F-15: one file per true-positive context, exactly one
        `sk-shape` detail each, counted by pattern name (a dash form may add a
        `key-heuristic` detail). Green before and after."""
        _t3_swept_env_guard(self, unset=tuple(T3_SWEPT_PLANTS))
        counts = {}
        for label, key, template in self.TRUE_POSITIVES:
            counts[label] = len(self._sk_shape_digests(template % key))
        self.assertEqual(counts,
                         {label: 1 for label, _k, _t in self.TRUE_POSITIVES},
                         "sk-shape details per true-positive file")

    def test_key_after_a_json_escape_still_hits(self):
        """GUARD, §3 F-24 (RR-32 F-6): files written with json.dumps, whose
        string value puts a line break, a tab or a carriage return before a
        key shape, hold the escape itself (a backslash plus n, t or r); exactly
        one `sk-shape` detail per file. Green before and after; red under the
        boundary without the escape alternative (counterfactual (iii))."""
        _t3_swept_env_guard(self, unset=tuple(T3_SWEPT_PLANTS))
        counts = {}
        for label, raw, escape, key in self.ESCAPED:
            text = json.dumps({"text": "field one" + raw + key})
            self.assertTrue(escape in text,
                            "%s: the file must hold the JSON escape" % label)
            counts[label] = len(self._sk_shape_digests(text))
        self.assertEqual(counts,
                         {label: 1 for label, _r, _e, _k in self.ESCAPED},
                         "sk-shape details per JSON-escape file")

    def test_details_never_quote_a_fragment(self):
        """GUARD, §3 F-16: over §3 F-15's plants every detail is
        `path:line: <pattern> <sha256_12>`, and no plant, nor its first or
        last 12 characters, appears in any detail. Green before and after."""
        _t3_swept_env_guard(self, unset=tuple(T3_SWEPT_PLANTS))
        _hits, found = self._sweep_files(
            *[template % key for _l, key, template in self.TRUE_POSITIVES])
        sk_count = sum(1 for x in found
                       if self._pattern_name(x) == "sk-shape")
        self.assertEqual(sk_count, len(self.TRUE_POSITIVES),
                         "one sk-shape detail per plant is expected, got %d"
                         % sk_count)
        for x in found:
            _t3_assert_detail_shape(self, x, "SA-3 sweep detail")
        needles = []
        for label, key, _template in self.TRUE_POSITIVES:
            needles += [(label, key), (label + " first-12", key[:12]),
                        (label + " last-12", key[-12:])]
        _t3_assert_absent(self, "\n".join(found), needles,
                          "the SA-3 sweep details")


class CredentialSuffixLegTest(unittest.TestCase):
    """apex-1s3b's redteam half, SA-4 (spec r2 §4 SA-4; §2.4 (C) as RR-32 F-1
    words it). The binary logs the last 12 characters of a key as
    `key_suffix` (`api_key_probe.rs:20-25@8072d8ed`), which neither the
    full-value sweep nor the full-value literal check can see. RED on SA-3
    GREEN, where both match whole values only. The three GUARDs pin what must
    not move: no double count inside a full value, no placeholder pattern and
    the ambient-key leg. Chain plants are set only where spec r2's SA-4 cells
    name them (the `XAI_API_KEY` plant, and the suffix the
    `APEX_LLM_PROXY_KEY`, `CODEX_LLM_PROXY_KEY` and `GROK_CODE_XAI_API_KEY`
    plants share); the two GUARDs that need no chain value unset every chain
    name. Assertions print counts, labels and digests only."""

    # The probe's line as the binary writes it: `LogEntry`'s fields in
    # serialization order (`unified_log.rs:62-84@8072d8ed`), then the probe's
    # ctx (`api_key_probe.rs:176-188`), compact JSON. Only `key_suffix` varies.
    PROBE_ENTRY = {"ts": "2026-11-01T02:57:18.123Z", "src": "shell",
                   "pid": 4242, "ver": "1.0.24", "lvl": "info",
                   "msg": "auth: first-party API key probe"}
    PROBE_CTX = {"verdict": "Unknown", "allows_advertise": True,
                 "elapsed_ms": 411, "timeout_ms": 400, "attempts": 1}

    def _probe_home(self, key_suffix):
        """A run dir whose only file is one case home's
        `home/logs/unified.jsonl`, holding the probe line. Returns
        (root, path)."""
        root = tempfile.mkdtemp(prefix="rt-sk8i-suffix-")
        self.addCleanup(lambda: shutil.rmtree(root, ignore_errors=True))
        logs = os.path.join(root, "case-01", "home", "logs")
        os.makedirs(logs)
        entry = dict(self.PROBE_ENTRY,
                     ctx=dict(self.PROBE_CTX, key_suffix=key_suffix))
        path = os.path.join(logs, "unified.jsonl")
        with open(path, "w") as fh:
            fh.write(json.dumps(entry, separators=(",", ":")) + "\n")
        return root, path

    def _dir_with(self, *texts):
        """One one-line `resp-NNN.jsonl` file per text in a fresh run dir.
        Returns (root, paths)."""
        root = tempfile.mkdtemp(prefix="rt-sk8i-suffix-")
        self.addCleanup(lambda: shutil.rmtree(root, ignore_errors=True))
        paths = []
        for i, text in enumerate(texts):
            path = os.path.join(root, "resp-%03d.jsonl" % i)
            with open(path, "w") as fh:
                fh.write(text + "\n")
            paths.append(path)
        return root, paths

    @staticmethod
    def _pattern_name(detail):
        """The `<pattern>` field of `path:line: <pattern> <sha256_12>`."""
        return detail.split(": ", 1)[1].split()[0]

    def _digests(self, found, pattern):
        """The `<sha256_12>` field of every `pattern` detail of a sweep."""
        return [x.split()[-1] for x in found
                if self._pattern_name(x) == pattern]

    def test_suffix_only_occurrence_hits_the_sweep(self):
        """§3 F-17: the probe line carries only the last 12 characters of the
        `XAI_API_KEY` plant, a suffix no other plant contains. RED on SA-3
        GREEN: zero hits."""
        _t3_swept_env_guard(self)
        key = _t3_env_plant("XAI_API_KEY")
        root, _path = self._probe_home(key[-12:])
        hits, found = run.redaction_sweep(root, "")
        digests = self._digests(found, "credential-suffix")
        self.assertEqual((hits, len(digests)), (1, 1),
                         "exactly one hit, a credential-suffix detail, is "
                         "expected; got %d hit(s), %d credential-suffix"
                         % (hits, len(digests)))
        self.assertEqual(digests[0], run.sha256_12(key),
                         "the detail digests the FULL value, never the "
                         "suffix")
        for x in found:
            _t3_assert_detail_shape(self, x, "SA-4 sweep detail")
        _t3_assert_absent(self, "\n".join(found),
                          [("XAI_API_KEY", key),
                           ("XAI_API_KEY last-12", key[-12:])],
                          "the suffix-leg sweep details")

    def test_literal_check_reports_suffix_files_through_a_file(self):
        """§3 F-17 through `literal_check` (RR-32 F-1). A list-accumulating
        spy on `run.GREP_RUNNER` records every call's argv and, for a call
        carrying `-f`, the patterns file's text and mode (read before grep
        runs, while the file exists) and grep's exit status. The suffix call
        is the one whose file holds the plant's last 12 characters but not
        the plant. RED on SA-3 GREEN: `(0, [])` from a single, full-value
        grep call; no suffix call exists."""
        _t3_swept_env_guard(self)
        key = _t3_env_plant("XAI_API_KEY")
        root, path = self._probe_home(key[-12:])
        calls = []
        real = run.GREP_RUNNER

        def spy(argv, *a, **kw):
            call = {"argv": list(argv)}
            if "-f" in argv:
                live = argv[argv.index("-f") + 1]
                with open(live) as fh:      # grep has not run yet, so the
                    call["patterns"] = fh.read()   # file still exists
                call["mode"] = os.stat(live).st_mode & 0o777
            calls.append(call)
            proc = real(argv, *a, **kw)
            call["exit_code"] = proc.returncode
            return proc
        run.GREP_RUNNER = spy
        self.addCleanup(lambda: setattr(run, "GREP_RUNNER", real))
        hits, details = run.literal_check(root)
        suffix_idx = [i for i, c in enumerate(calls)
                      if key[-12:] in c.get("patterns", "")
                      and key not in c.get("patterns", "")]
        full_idx = [i for i, c in enumerate(calls)
                    if key in c.get("patterns", "")]
        self.assertEqual(len(suffix_idx), 1,
                         "exactly one suffix grep call is expected; %d grep "
                         "call(s) recorded, %d of them suffix calls, and "
                         "literal_check returned %d hit(s) with %d "
                         "detail(s)" % (len(calls), len(suffix_idx), hits,
                                        len(details)))
        self.assertEqual(len(full_idx), 1,
                         "exactly one full-value grep call is expected; got "
                         "%d" % len(full_idx))
        self.assertEqual(calls[suffix_idx[0]]["mode"], 0o600,
                         "the suffix patterns file is private (0600)")
        self.assertLess(suffix_idx[0], full_idx[0],
                        "the suffix grep runs before the full-value grep")
        self.assertEqual(full_idx[0], len(calls) - 1,
                         "the full-value grep is the last GREP_RUNNER call")
        self.assertEqual(calls[full_idx[0]]["exit_code"], 1,
                         "this dir holds no full value, so the full-value "
                         "grep exits 1 (the early-return path)")
        _t3_assert_value(
            self, (hits, details),
            (1, ["literal check: 1 file(s) match a credential suffix (last "
                 "12 chars) (names sha256_12 %s)" % [run.sha256_12(path)]]),
            "one suffix file is reported although the full-value grep "
            "exits 1")
        needles = [("XAI_API_KEY", key), ("XAI_API_KEY last-12", key[-12:])]
        for call in calls:
            self.assertFalse("-e" in call["argv"],
                             "no pattern may be an argv element: %s"
                             % (_t3_argv_audit(call["argv"]),))
            for elem in call["argv"]:
                _t3_assert_absent(self, elem, needles,
                                  "a credential value or suffix must never "
                                  "be argv-visible (HYG-1)")
        self.assertTrue("-f" in calls[suffix_idx[0]]["argv"],
                        "the suffix patterns come from a file: %s"
                        % (_t3_argv_audit(calls[suffix_idx[0]]["argv"]),))

    def test_shared_suffix_counts_once(self):
        """§3 F-20. The `APEX_LLM_PROXY_KEY`, `CODEX_LLM_PROXY_KEY` and
        `GROK_CODE_XAI_API_KEY` plants share one 12-character suffix: one
        occurrence is one detail, digested from the first owner in
        `LITERAL_SWEEP_NAMES` order. A value of 12 characters or fewer adds
        no suffix pattern. RED on SA-3 GREEN: zero hits."""
        _t3_swept_env_guard(self)
        owners = ("APEX_LLM_PROXY_KEY", "CODEX_LLM_PROXY_KEY",
                  "GROK_CODE_XAI_API_KEY")
        tails = {n: _t3_env_plant(n)[-12:] for n in owners}
        self.assertEqual(len(set(tails.values())), 1,
                         "the three plants must share one suffix")
        root, _path = self._probe_home(tails["APEX_LLM_PROXY_KEY"])
        hits, found = run.redaction_sweep(root, "")
        digests = self._digests(found, "credential-suffix")
        self.assertEqual(len(digests), 1,
                         "one occurrence of a shared suffix is one "
                         "credential-suffix detail; got %d (of %d hit(s))"
                         % (len(digests), hits))
        self.assertEqual(digests[0],
                         run.sha256_12(_t3_env_plant("APEX_LLM_PROXY_KEY")),
                         "the detail is attributed to the first owner in "
                         "LITERAL_SWEEP_NAMES order")
        short = "short" + "plant" + "12"
        self.assertEqual(len(short), 12, "the boundary: 12 characters")
        values = [short if n == "CODEX_TRUE_FOUNDRY_KEY" else _t3_env_plant(n)
                  for n in run.LITERAL_SWEEP_NAMES]
        self.assertEqual(
            [run.sha256_12(v) for _s, v in run.credential_suffixes(values)],
            [run.sha256_12(_t3_env_plant(n)) for n in
             ("APEX_LLM_PROXY_KEY", "GROK_DIRECT_ONPREM_AUTH", "XAI_API_KEY")],
            "one suffix pattern per distinct suffix, first owner kept; the "
            "12-character value under CODEX_TRUE_FOUNDRY_KEY adds none")

    def test_suffix_leak_sets_the_exit_status(self):
        """§3 F-22: a run dir whose only finding is §3 F-17's line fails the
        run and the persisted verdict records the hit. RED on SA-3 GREEN:
        hits 0, code 0."""
        _t3_swept_env_guard(self)
        key = _t3_env_plant("XAI_API_KEY")
        root, _path = self._probe_home(key[-12:])
        hits, _found = run.redaction_sweep(root, "")
        code = run.run_verdict_code(bad=0, sweep_hits=hits)
        self.assertEqual(code, 1,
                         "a run whose only finding is a key suffix may not "
                         "exit 0; hits=%d" % hits)
        run.persist_run_verdict(
            root, [{"status": "PASS", "id": "C0", "run_dir": None}],
            sweep_hits=hits, code=code)
        with open(os.path.join(root, "run-verdict.json")) as fh:
            doc = json.load(fh)
        self.assertGreater(doc["sweep_hits"], 0,
                           "the persisted verdict records the suffix hit")
        self.assertEqual(doc["exit_code"], 1)

    def test_full_value_is_not_double_counted(self):
        """GUARD, §3 F-18: a file holding the whole `XAI_API_KEY` plant, which
        also contains the suffix three other plants share, so an occurrence
        inside a full value's span is exercised. One `credential-literal`
        detail and no `credential-suffix` detail; `literal_check` counts the
        file once. Green before and after."""
        _t3_swept_env_guard(self)
        key = _t3_env_plant("XAI_API_KEY")
        self.assertTrue(_t3_env_plant("APEX_LLM_PROXY_KEY")[-12:] in key,
                        "the plant must contain the shared suffix, or the "
                        "any-full-value-span rule goes unexercised")
        root, paths = self._dir_with('{"frame": "%s"}' % key)
        _hits, found = run.redaction_sweep(root, "")
        counts = {p: len(self._digests(found, p))
                  for p in ("credential-literal", "credential-suffix")}
        self.assertEqual(counts, {"credential-literal": 1,
                                  "credential-suffix": 0},
                         "details per pattern for one full-value leak")
        _t3_assert_value(
            self, run.literal_check(root),
            (1, ["literal check: 1 file(s) match a credential value (names "
                 "sha256_12 %s)" % [run.sha256_12(paths[0])]]),
            "literal_check counts the file once")

    def test_placeholder_suffix_is_never_a_pattern(self):
        """GUARD, §3 F-19 (R2-4): a file holding only the last 12 characters
        of the placeholder plant gives `(0, [])` from the sweep and from
        `literal_check`. The chain names are unset; the other swept names hold
        their plants, so `literal_check` runs both greps. Green before and
        after."""
        _t3_swept_env_guard(self, unset=tuple(run.XAI_CREDENTIAL_CHAIN))
        root, _paths = self._dir_with(
            _t3_env_plant("GROK_DIRECT_PLACEHOLDER_KEY")[-12:])
        _t3_assert_value(self, run.redaction_sweep(root, ""), (0, []),
                         "the sweep: the placeholder's suffix is never a "
                         "pattern")
        _t3_assert_value(self, run.literal_check(root), (0, []),
                         "literal_check: the placeholder's suffix is never "
                         "a pattern")

    def test_ambient_key_leg_unchanged(self):
        """GUARD, §3 F-21: swept with `ambient_key=` the plant `main` reads as
        the ambient key (`CODEX_LLM_PROXY_KEY`'s), a file holding the whole
        plant draws exactly one `ambient-key` detail and a file holding only
        its last 12 characters draws none. Only that name is set. Green
        before and after."""
        _t3_swept_env_guard(self, unset=tuple(
            n for n in T3_SWEPT_PLANTS if n != "CODEX_LLM_PROXY_KEY"))
        ambient = _t3_env_plant("CODEX_LLM_PROXY_KEY")
        root, _paths = self._dir_with('{"frame": "%s"}' % ambient,
                                      '{"frame": "%s"}' % ambient[-12:])
        _hits, found = run.redaction_sweep(root, ambient)
        labels = {"resp-000.jsonl": "whole", "resp-001.jsonl": "last-12"}
        counts = {"whole": 0, "last-12": 0}
        digests = []
        for x in found:
            if self._pattern_name(x) == "ambient-key":
                counts[labels[x.split(":", 1)[0]]] += 1
                digests.append(x.split()[-1])
        self.assertEqual(counts, {"whole": 1, "last-12": 0},
                         "ambient-key details per file")
        self.assertEqual(digests, [run.sha256_12(ambient)],
                         "the ambient-key detail digests the whole ambient "
                         "value")
