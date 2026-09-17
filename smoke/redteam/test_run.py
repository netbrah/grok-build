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
                 init_bps=0, init_line=None):
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
        elif method == "session/close":
            self._respond(t + 0.1, tid, {})

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
        self._notify(t_end, "_x.ai/session_notification",
                     {"sessionId": self.session_id,
                      "update": {"sessionUpdate": "turn_completed",
                                 "prompt_id": pid,
                                 "stop_reason": stop,
                                 "elapsed_ms": 42}})
        self._notify(t_pc, "_x.ai/session/prompt_complete",
                     {"sessionId": self.session_id,
                      "promptId": pid,
                      "stopReason": stop,
                      "agentResult": self.turn_agent_result})
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
            len(slim), 6, "the 6 slim drop-in cases must be in the tree")
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

    6 slim pass; a legacy case (no schema_version) skips the NEW rules;
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
        self.assertGreaterEqual(len(slim), 6,
                                "slim drop-in set must be in the tree")
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


def _write_fake_live_home(tmp, upstream):
    """A minimal live GROK_HOME: config pointing at the local fake
    upstream + the auth stub HermeticHome copies."""
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
           "turn_summary = false\n" % (upstream, upstream, stub))
    with open(os.path.join(live, "config.toml"), "w") as fh:
        fh.write(cfg)
    return live


def _write_fake_binary(tmp, upstream):
    """A fake pager binary: reads the HERMETIC config's base_url,
    POSTs one turn request through it, exits 0 iff the upstream said
    200. (The hermetic base_url is the wiretap, which forwards to the
    fake upstream — fully offline.)"""
    b = os.path.join(tmp, "fake-grok")
    with open(b, "w") as fh:
        fh.write(
            "#!/bin/sh\n"
            "python3 - \"$GROK_HOME/config.toml\" <<'PYEOF'\n"
            "import json, re, sys, urllib.request, urllib.error\n"
            "cfg = open(sys.argv[1]).read()\n"
            "base = re.search(r'^base_url = \"([^\"]+)\"', cfg, re.M)\n"
            "if not base:\n"
            "    sys.exit(3)\n"
            "base = base.group(1)\n"
            "req = urllib.request.Request(\n"
            "    base + '/responses',\n"
            "    data=json.dumps({'model': 'm-x', 'store': False,\n"
            "                     'input': [{'role': 'user',\n"
            "                                'content': 'ping'}]})\n"
            "            .encode(),\n"
            "    headers={'content-type': 'application/json'})\n"
            "try:\n"
            "    r = urllib.request.urlopen(req, timeout=15)\n"
            "    r.read()\n"
            "    sys.exit(0)\n"
            "except urllib.error.HTTPError:\n"
            "    sys.exit(1)\n"
            "PYEOF\n")
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
    records $PWD into the file named by env CWD_RECORDER (survives
    build_env — not in PROVIDER_VARS_UNSET). Pre-fix RED: the
    hardcoded home.cwd is recorded instead."""

    def test_popen_cwd_is_case_cwd(self):
        tmp = tempfile.mkdtemp(prefix="rt-c1-")
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        srv, upstream = _start_fake_upstream()
        self.addCleanup(srv.shutdown)
        live = _write_fake_live_home(tmp, upstream)
        rec = os.path.join(tmp, "recorded-cwd.txt")
        binp = os.path.join(tmp, "cwd-recorder")
        with open(binp, "w") as fh:
            fh.write("#!/bin/sh\npwd > \"$CWD_RECORDER\"\n")
        os.chmod(binp, 0o755)
        workdir = os.path.join(tmp, "workdir")
        os.makedirs(workdir)
        saved = os.environ.get("CWD_RECORDER")
        os.environ["CWD_RECORDER"] = rec
        try:
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
        finally:
            if saved is None:
                os.environ.pop("CWD_RECORDER", None)
            else:
                os.environ["CWD_RECORDER"] = saved
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


def main():
    unittest.main(verbosity=2)


if __name__ == "__main__":
    main()
