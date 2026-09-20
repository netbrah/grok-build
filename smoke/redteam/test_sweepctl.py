#!/usr/bin/env python3
"""ST-SURV-1..3 — hermetic selftests for cut 0.5 (smoke/lib/launch.py + smoke/sweepctl.py).

RED-first per plan section 7 (D-4). stdlib only (json/os/shutil/signal/subprocess/sys/time).

Run directly:   python3.12 smoke/redteam/test_sweepctl.py     (exit 0 = 3/3 pass)
Importable:     from test_sweepctl import st_surv_1, st_surv_2, st_surv_3, main

Hermetic: everything runs in a private temp dir; the shared smoke/runs.jsonl is
only READ (and asserted byte-unchanged). No proxy, no real runners, no cargo,
no signals to anything outside the test's own process tree.
"""
import json
import os
import shutil
import signal
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))          # .../smoke/redteam
SMOKE = os.path.dirname(HERE)                              # .../smoke
REPO_ROOT = os.path.dirname(SMOKE)
RUNS_MANIFEST = os.path.join(SMOKE, "runs.jsonl")
PY = sys.executable
if SMOKE not in sys.path:
    sys.path.insert(0, SMOKE)

# Stub runner: mimics the D-3 runner wiring (self_daemonize at top of main when
# --daemon is present) plus test hooks controlled purely via STUB_* env vars so
# the argv it receives stays exactly what the launch path assembles (ST-SURV-3).
STUB_SRC = r'''import json, os, sys, time
sys.path.insert(0, "__SMOKE__")
from lib import launch

mode = os.environ.get("STUB_MODE", "daemon")
if os.environ.get("STUB_ARGV_FILE"):
    with open(os.environ["STUB_ARGV_FILE"], "w") as f:
        json.dump(sys.argv, f)
if "--daemon" in sys.argv and mode == "daemon":
    launch.self_daemonize(os.environ["STUB_LOG"], os.environ["STUB_PIDFILE"],
                          manifest_path=os.environ["STUB_MANIFEST"])
    sys.stdout.write("post-daemonize fd1 check\n")   # must land in STUB_LOG via dup2
    sys.stdout.flush()
hb = os.environ.get("STUB_HB")
if hb:
    n = 0
    while True:
        with open(hb, "a") as f:
            f.write("hb %d %d\n" % (os.getpid(), n))
        n += 1
        time.sleep(0.5)
time.sleep(float(os.environ.get("STUB_SLEEP", "0.0")))
sys.exit(0)
'''

# Synthetic redteam-shaped log for the status() crash-detector check (M4: every
# line carries the [HH:MM:SS] prefix; M2: the terminal per-case line is
# "  ID: STATUS in Xs calls=N" — the "->" mark is NOT the terminal line).
# m4: mirrors the verbatim runner shapes (run.py:3112,3838,5609,5635); A-3 has
# a "->" verdict mark but NO terminal ":" line = the crashed-mid-case shape.
SYNTHETIC_LOG = (
    "[00:00:00] HT-1 L3 run -> smoke/redteam/report/synthetic (budget=1000 wirecap=False)\n"
    "[00:00:00] cases: A-1, A-2, A-3\n"
    "[00:00:01] === A-1: synthetic case one\n"
    "[00:00:01]   A-1: wiretap on :51001 capture=a1/wire\n"
    "[00:00:02]   A-1 -> PASS (model-x) (ok)\n"
    "[00:00:02]   A-1: PASS (model-x) in 1.0s calls=2\n"
    "[00:00:02]   A-1: no env retry (no retry.on_status declared)\n"
    "[00:00:03] === A-2: synthetic case two\n"
    "[00:00:04]   A-2 -> FAIL (1 assert(s) failed)\n"
    "[00:00:04]   A-2: FAIL (model-x) in 1.5s calls=3\n"
    "[00:00:05] === A-3: synthetic case three\n"
    "[00:00:05]   A-3: wiretap on :51003 capture=a3/wire\n"
    "[00:00:06]   A-3 -> FAIL (engine error: watchdog timeout)\n"
)


def _mktemp():
    # tempfile is not in the stdlib allow-list; pid+microsecond is enough.
    d = os.path.join("/tmp", "sweepctl-st-%d-%d" % (os.getpid(), int(time.time() * 1e6)))
    os.mkdir(d)
    return d


def _write(path, text):
    with open(path, "w") as f:
        f.write(text)


def _poll(pred, timeout=15.0, interval=0.05):
    t0 = time.time()
    while time.time() - t0 < timeout:
        v = pred()
        if v:
            return v
        time.sleep(interval)
    return None


def _pid_alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


def _read_int(path):
    with open(path) as f:
        return int(f.read().strip())


def _hb_count(path):
    if not os.path.exists(path):
        return 0
    with open(path) as f:
        return len(f.readlines())


def _stub_path(d):
    stub = os.path.join(d, "stub_runner.py")
    _write(stub, STUB_SRC.replace("__SMOKE__", SMOKE))
    return stub


def _drop_stub_env():
    for key in list(os.environ):
        if key.startswith("STUB_"):
            del os.environ[key]
    os.environ.pop("SWEEPCTL_MANIFEST", None)


def _tail_tb(tb, n=8):
    frames = []
    while tb is not None and len(frames) < n:
        f = tb.tb_frame
        frames.append("%s:%d in %s" % (os.path.basename(f.f_code.co_filename),
                                       tb.tb_lineno, f.f_code.co_name))
        tb = tb.tb_next
    return "\n".join("  " + x for x in reversed(frames))


def st_surv_1():
    """ST-SURV-1 (B0 class 1, teardown survival): a stub self_daemonize's; the
    parent then SIGKILLs the LAUNCHER's whole process group — the faithful model
    of the 043056Z incident (nohup'd run died exactly at the launcher session's
    teardown). Assert the daemon is alive ~2s later and its heartbeat grew. A
    control stub (no daemonize, same group) must die in the same killpg, which
    proves the kill reached the group (no vacuous pass)."""
    from lib import launch  # noqa: F401  (import is the RED tripwire)
    import sweepctl          # noqa: F401
    d = _mktemp()
    try:
        stub = _stub_path(d)
        hb = os.path.join(d, "heartbeat.txt")
        hb_ctl = os.path.join(d, "control-hb.txt")
        log = os.path.join(d, "daemon.log")
        pidfile = os.path.join(d, "daemon.pid")
        manifest = os.path.join(d, "runs.jsonl")
        launched = os.path.join(d, "launcher.json")

        env = dict(os.environ)
        env.update({"STUB_MODE": "daemon", "STUB_HB": hb, "STUB_LOG": log,
                    "STUB_PIDFILE": pidfile, "STUB_MANIFEST": manifest})
        env_ctl = dict(env)
        env_ctl.update({"STUB_MODE": "raw", "STUB_HB": hb_ctl})

        launcher_pid = os.fork()
        if launcher_pid == 0:
            # LAUNCHER: its own session (the "tool-exec session" of B0). It
            # spawns the treatment stub (self_daemonize's out of the session)
            # and the control stub (never leaves the session) INSIDE its own
            # group, then idles until the teardown kill.
            os.setsid()
            subprocess.Popen([PY, stub, "--daemon", "--probe-treatment"], env=env)
            ctl = subprocess.Popen([PY, stub, "--probe-control"], env=env_ctl)
            with open(launched, "w") as f:
                json.dump({"control_pid": ctl.pid}, f)
            time.sleep(60)
            os._exit(0)

        info = _poll(lambda: (json.load(open(launched)) if os.path.exists(launched) else None))
        assert info, "launcher never spawned its children (timeout)"
        daemon_pid = _poll(lambda: (_read_int(pidfile) if os.path.exists(pidfile) else None))
        assert daemon_pid, "daemon pidfile never appeared (self_daemonize incomplete?)"
        ctl_pid = info["control_pid"]
        assert _pid_alive(ctl_pid), "control stub died before the teardown kill"
        assert _pid_alive(daemon_pid), "daemon died before the teardown kill"
        assert os.getpgid(ctl_pid) == launcher_pid, "control not in launcher group (test setup broken)"
        assert os.getpgid(daemon_pid) != launcher_pid, "daemon still in launcher group — setsid missing?"

        # THE INCIDENT: launcher-session process-group teardown.
        os.killpg(os.getpgid(launcher_pid), signal.SIGKILL)
        time.sleep(0.7)
        assert not _pid_alive(ctl_pid), "killpg did not reach the launcher group — survival pass would be vacuous"

        # ~2s after teardown: daemon alive and heartbeating.
        time.sleep(1.5)
        assert _pid_alive(daemon_pid), "daemon dead ~2s after launcher-group teardown (B0 class 1 regression)"
        n1 = _hb_count(hb)
        time.sleep(1.1)
        n2 = _hb_count(hb)
        assert _pid_alive(daemon_pid), "daemon died inside the post-teardown window"
        assert n2 > n1 > 0, "heartbeat file not growing (daemon not actually running?)"

        # D-1 SIGTERM path: exactly one killed terminal line, then death.
        os.kill(daemon_pid, signal.SIGTERM)
        assert _poll(lambda: not _pid_alive(daemon_pid), timeout=5.0), "daemon ignored SIGTERM"
        os.waitpid(launcher_pid, 0)  # reap the (killed) launcher
        with open(manifest) as f:
            lines = [json.loads(l) for l in f.read().splitlines() if l.strip()]
        terminals = [l for l in lines if l.get("status") in ("exited", "killed")]
        assert [l for l in lines if l.get("status") == "killed"], "no status:'killed' terminal line after SIGTERM"
        assert len(terminals) == 1, "terminal line double-appended (M5 guard failed): %r" % terminals
    finally:
        _drop_stub_env()
        shutil.rmtree(d, ignore_errors=True)


def st_surv_2():
    """ST-SURV-2 (manifest/status round-trip): launch the stub via the sweepctl
    launch path -> the daemon's manifest line lands (pid matches the live
    daemon, provenance fields present, fd1 redirected to the log, one clean
    'exited' terminal line). Then feed status() logic a synthetic log (roster
    of 3 cases, 2 terminal, 1 current) with a dead pid -> done/total + crash
    flag + exit code 4 (and exit 3 for a clean 'exited' line)."""
    from lib import launch  # noqa: F401
    import sweepctl
    d = _mktemp()
    try:
        with open(RUNS_MANIFEST, "rb") as f:
            shared_before = f.read()
        stub = _stub_path(d)
        log = os.path.join(d, "run.log")
        pidfile = os.path.join(d, "daemon.pid")
        manifest = os.path.join(d, "runs.jsonl")
        os.environ.update({"STUB_MODE": "daemon", "STUB_LOG": log,
                           "STUB_PIDFILE": pidfile, "STUB_MANIFEST": manifest,
                           "STUB_SLEEP": "1.0", "SWEEPCTL_MANIFEST": manifest})
        p, launched = sweepctl.run_launch("stub", ["--campaign-id", "st-surv-2"],
                                          suites={"stub": stub})
        p.wait(timeout=15)  # the pre-daemon runner exits 0 immediately
        assert launched is not None, "run_launch's bounded wait (m7) missed the manifest line"
        assert launched["campaign_id"] == "st-surv-2"

        # 1) the daemon's manifest line lands; pid == live daemon; provenance.
        line = _poll(lambda: sweepctl.last_running_line(manifest, "st-surv-2"))
        assert line, "manifest 'running' line never landed (single-writer path broken?)"
        daemon_pid = _read_int(pidfile)
        assert line["pid"] == daemon_pid, "manifest pid %r != live daemon pid %r" % (line["pid"], daemon_pid)
        assert _pid_alive(line["pid"]), "manifest pid is not a live process"
        for field in ("v", "ts", "campaign_id", "suite", "runner", "pid", "log", "out_dir",
                      "bin_sha12", "git_head", "python", "config_sha12", "argv", "launcher",
                      "lstart", "status"):
            assert field in line, "manifest line missing field: %r" % field
        assert line["v"] == 1 and line["status"] == "running"
        assert line["campaign_id"] == "st-surv-2"

        # 2) the daemon reopens the log on fds 1+2: the stub's stdout landed in STUB_LOG.
        assert _poll(lambda: ("post-daemonize fd1 check" in open(log).read()
                              if os.path.exists(log) else False)), "fd1 not redirected to the log"

        # 3) clean exit -> exactly one 'exited' terminal line with exit_code.
        term = _poll(lambda: sweepctl.last_terminal_line(manifest, "st-surv-2"))
        assert term, "no 'exited' terminal line after the stub's clean exit"
        assert term["pid"] == daemon_pid and term["status"] == "exited" and "exit_code" in term, \
            "terminal line wrong: %r" % term
        assert not _pid_alive(daemon_pid), "stub daemon still alive after its clean exit"

        # 4) synthetic log + dead pid -> done/total, crash flag, exit 4.
        syn = os.path.join(d, "synthetic.log")
        _write(syn, SYNTHETIC_LOG)
        dead_line = dict(line)
        dead_line["log"] = syn
        res = sweepctl.assess(dead_line)
        assert res["exit_code_out"] == 4, "expected exit 4 (dead, no terminal line), got %r" % res["exit_code_out"]
        assert res["crash"] is True, "crash flag not set"
        assert res["done"] == 2 and res["total"] == 3, "done/total wrong: %r/%r" % (res["done"], res["total"])
        assert res["current"] == "A-3", "current case wrong: %r" % res["current"]
        assert res["calls"] == 5, "calls-so-far wrong: %r" % res["calls"]
        assert res["budget"] == 1000, "budget wrong: %r" % res["budget"]

        # exit-code mapping: a terminal 'exited' line -> 3.
        res3 = sweepctl.assess(dict(dead_line, status="exited"))
        assert res3["exit_code_out"] == 3, "expected exit 3 for clean 'exited', got %r" % res3["exit_code_out"]

        # the shared registry was not touched by any of this.
        with open(RUNS_MANIFEST, "rb") as f:
            shared_after = f.read()
        assert shared_after == shared_before, "smoke/runs.jsonl mutated by the selftest"
    finally:
        _drop_stub_env()
        shutil.rmtree(d, ignore_errors=True)


def st_surv_3():
    """ST-SURV-3 (B0 class 2, word-split regression guard): launch with
    multi-word argv elements (campaign id "full sweep test", a case list with
    spaces) via the sweepctl launch code path; the stub records its received
    argv verbatim; assert exact list equality — nothing in the launch path
    may shell-interpolate. Also pins the CLI parse layer: launch args after
    the suite must reach run_launch verbatim, INCLUDING -prefixed flags —
    the original nargs="*" rejected `--budget 100000` at parse time (caught
    live 2026-09-19 during the first sweepctl launch; REMAINDER fixed)."""
    import sweepctl
    d = _mktemp()
    try:
        argv_file = os.path.join(d, "argv.json")
        stub = os.path.join(d, "argv_probe.py")
        _write(stub, "import json, sys\n"
                     "with open(%r, 'w') as f:\n"
                     "    json.dump(sys.argv, f)\n" % argv_file)
        user_args = ["--campaign-id", "full sweep test",
                     "--cases", "alpha case, beta case (with spaces)"]
        _p, _line = sweepctl.run_launch("stub", user_args, suites={"stub": stub},
                                        wait_manifest=0)
        data = _poll(lambda: (json.load(open(argv_file)) if os.path.exists(argv_file) else None))
        expected = [stub] + user_args + ["--daemon"]
        assert data == expected, "argv mangled by the launch path:\n got:  %r\n want: %r" % (data, expected)
        # CLI parse guard: the argparse layer must survive -prefixed runner
        # args (nargs="*" would raise 'unrecognized arguments' here).
        ns = sweepctl.build_parser().parse_args(
            ["launch", "redteam", "--budget", "100000",
             "--cases", "alpha case, beta case (with spaces)"])
        assert ns.cmd == "launch" and ns.suite == "redteam"
        assert ns.args == ["--budget", "100000", "--cases",
                           "alpha case, beta case (with spaces)"], \
            "launch CLI parse mangled runner args: %r" % (ns.args,)
    finally:
        _drop_stub_env()
        shutil.rmtree(d, ignore_errors=True)


def st_surv_4():
    """ST-SURV-4 (apex-ayl.91 N1/N2, campaign 20260919T084258Z):
    (a) dead pid + no terminal line + clean-completion marker in the
    run log -> state 'completed' (exit 3, not a crash); the same line
    without the marker stays DEAD/crash; (b) the SKIP terminal-line
    shape closes its === marker so the registry census reaches the
    full roster."""
    import sweepctl
    d = _mktemp()
    try:
        log = os.path.join(d, "run.log")
        _write(log, (
            "[00:00:00] cases: S-1, S-2\n"
            "[00:00:01] === S-1: case one\n"
            "[00:00:01]   S-1: SKIP in 0.0s calls=0\n"
            "[00:00:02] === S-2: case two\n"
            "[00:00:02]   S-2: PASS in 1.0s calls=1\n"
            "[00:00:03] redaction sweep: 0 hits\n"
            "[00:00:03] report: /tmp/synthetic-report.md\n"
            "[00:00:03] calls used: 1 / 40\n"))
        dead_line = {"pid": 99999999, "lstart": None, "status": "running",
                     "log": log, "campaign_id": "st-surv-4"}
        res = sweepctl.assess(dict(dead_line))
        assert res["state"] == "completed", \
            "expected 'completed' (clean-completion marker), got %r" % res["state"]
        assert res["crash"] is False and res["exit_code_out"] == 3, \
            "completed must not be a crash: %r" % res
        parsed = sweepctl.parse_log(log, "redteam")
        assert parsed["done"] == {"S-1": "SKIP", "S-2": "PASS"}, \
            "SKIP terminal line must close its marker: %r" % parsed["done"]
        assert parsed["calls"] == 1 and parsed["total"] == 2, \
            "calls/total wrong: %r" % parsed
        log2 = os.path.join(d, "run2.log")
        _write(log2, "[00:00:00] cases: S-1\n[00:00:01] === S-1: x\n")
        res2 = sweepctl.assess(dict(dead_line, log=log2))
        assert res2["state"] == "DEAD" and res2["crash"] is True, \
            "no marker + dead pid must stay DEAD/crash: %r" % res2
    finally:
        shutil.rmtree(d, ignore_errors=True)


TESTS = (
    ("ST-SURV-1", st_surv_1),
    ("ST-SURV-2", st_surv_2),
    ("ST-SURV-3", st_surv_3),
    ("ST-SURV-4", st_surv_4),
)


def main(argv=None):
    failures = 0
    for name, fn in TESTS:
        try:
            fn()
        except Exception as e:  # noqa: BLE001 — selftest harness reports, never hides
            failures += 1
            tb = sys.exc_info()[2]
            print("FAIL %s — %s: %s\n%s" % (name, type(e).__name__, e, _tail_tb(tb)))
        else:
            print("PASS %s" % name)
    print("sweepctl selftest: %d/%d passed" % (len(TESTS) - failures, len(TESTS)))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
