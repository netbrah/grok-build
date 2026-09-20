#!/usr/bin/env python3
"""sweepctl — driver for the driver (plan section 7, cut 0.5, D-2).

One durable re-attach path for the long smoke suites: the manifest
(smoke/runs.jsonl, gitignored) + the runner log. Manifest + log are the ONLY
re-attach path — PTY session ids do not survive across Codex sessions.

Commands
  launch <suite> [args...]   spawn `python <runner> <args> --daemon` with the
                             argv assembled as a LIST — never shell-interpolated
                             (B0 class-2 guard; concordance M6: Popen, not execv,
                             so the spawn info can be printed and sweepctl exits).
                             The runner self-daemonizes (lib.launch); the
                             manifest line lands within ~1s.
  status [campaign_id|suite] last matching manifest line; kill -0 + ps-lstart
                             cross-check against the manifest lstart field
                             (M1 pid-reuse guard). Exit codes: 0=running,
                             3=exited cleanly, 5=killed (SIGTERM),
                             4=DEAD without a terminal manifest line = crash
                             detector. Full log parse for redteam (roster from
                             `cases:`, done from `ID: STATUS in Xs calls=N`
                             terminal lines (M2), current from `=== ID:`);
                             wstream status is DEGRADED: pid liveness + log
                             tail only — its log has no roster/terminal shapes
                             (M3; aligning wstream log shapes is out of scope).
  tail <campaign_id>         follow the runner log (tail -n 40 -f).
  registry                   all manifest lines with LIVE/DEAD marks.

stdlib only (json/os/subprocess/sys/argparse/time). Read-only on everything it
does not own; the shared manifest is append-only by design (single-writer rule
lives in lib.launch).
"""
import argparse
import json
import os
import subprocess
import sys
import time

SMOKE_ROOT = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(SMOKE_ROOT)
if SMOKE_ROOT not in sys.path:
    sys.path.insert(0, SMOKE_ROOT)
from lib import launch  # noqa: E402

MANIFEST_PATH = os.path.join(SMOKE_ROOT, "runs.jsonl")

# Extensible suite map: suite -> runner path (worktree-relative or absolute).
SUITES = {
    "redteam": "smoke/redteam/run.py",
    "wstream": "smoke/wstream/run.py",
}
DEFAULT_BIN = os.path.join(REPO_ROOT, "target", "release", "grok-responses")


# ---------------------------------------------------------------- launch (D-2)

def run_launch(suite, args, suites=None, python=None, wait_manifest=2.0):
    """Assemble the runner argv as a LIST and spawn it detached (its own
    session); the runner self-daemonizes. Bounded wait (m7): polls the manifest
    up to wait_manifest seconds for the new line so the gate flow (`launch`
    then `status`) never races the ~1s daemonize window. Returns
    (Popen, manifest_line_or_None); the Popen is the launched (pre-daemon)
    runner, which exits 0 immediately after forking the daemon. The manifest
    polled is $SWEEPCTL_MANIFEST if set, else smoke/runs.jsonl (concordance
    nit #4: typo in the env-var name fixed)."""
    suites = SUITES if suites is None else suites
    if suite not in suites:
        raise SystemExit("sweepctl: unknown suite %r (known: %s)"
                         % (suite, ", ".join(sorted(suites))))
    runner = suites[suite]
    if not os.path.isabs(runner):
        runner = os.path.join(REPO_ROOT, runner)
    python = python or sys.executable
    # B0 class-2 guard: argv is a LIST end-to-end; no shell string anywhere.
    argv = [python, runner] + [str(a) for a in args] + ["--daemon"]
    env = dict(os.environ)
    env["_SWEEPCTL_BIN"] = os.environ.get("GROK_BIN", DEFAULT_BIN)
    launcher = "sweepctl launch %s" % suite
    if args:
        launcher += " " + " ".join(str(a) for a in args)
    env["_SWEEPCTL_LAUNCHER"] = launcher
    # M6: Popen (not os.execv) — sweepctl must print spawn info and exit.
    # start_new_session=True also shields the pre-daemonize window from a
    # launcher-session teardown (belt for the daemon's own setsid, suspenders).
    manifest_path = os.environ.get("SWEEPCTL_MANIFEST") or MANIFEST_PATH
    before = len(read_manifest(manifest_path)[0])
    p = subprocess.Popen(argv, start_new_session=True, env=env, cwd=REPO_ROOT)
    print("spawned %s (runner pid %d; self-daemonizes — that pid exits 0 immediately)"
          % (suite, p.pid))
    line = None
    if wait_manifest is not None and wait_manifest > 0:
        t0 = time.time()
        while time.time() - t0 < wait_manifest:
            for cand in read_manifest(manifest_path)[0][before:]:
                if cand.get("launcher") == launcher:
                    line = cand
                    break
            if line:
                break
            time.sleep(0.05)
    if line:
        print("manifest line landed: campaign %s — re-attach with: sweepctl status %s"
              % (line.get("campaign_id"), suite))
    else:
        print("WARNING: no manifest line after %.1fs — runner may still be "
              "pre-daemonize or failed; check: sweepctl status %s"
              % (wait_manifest or 0.0, suite))
    return p, line


# ------------------------------------------------------- manifest (registry)

def read_manifest(path=None):
    """All manifest lines. A torn last line (writer mid-append) is dropped and
    flagged, per the spec risk note. Returns (lines, torn)."""
    path = path or MANIFEST_PATH
    try:
        with open(path, errors="replace") as f:
            raw = [l for l in f.read().splitlines() if l.strip()]
    except FileNotFoundError:
        return [], False
    lines, torn = [], False
    for i, l in enumerate(raw):
        try:
            lines.append(json.loads(l))
        except json.JSONDecodeError:
            torn = i == len(raw) - 1
    return lines, torn


def last_matching(lines, campaign_id=None, suite=None, status=None):
    for l in reversed(lines):
        if campaign_id is not None and l.get("campaign_id") != campaign_id:
            continue
        if suite is not None and l.get("suite") != suite:
            continue
        if status is not None and l.get("status") != status:
            continue
        return l
    return None


def last_running_line(path, campaign_id):
    return last_matching(read_manifest(path)[0],
                         campaign_id=campaign_id, status="running")


def last_terminal_line(path, campaign_id):
    for l in reversed(read_manifest(path)[0]):
        if l.get("campaign_id") == campaign_id and l.get("status") in ("exited", "killed"):
            return l
    return None


# ------------------------------------------------------- liveness (M1 guard)

def pid_alive(pid, expected_lstart=None):
    """kill -0 + `ps lstart` cross-check (pid-reuse guard, M1). expected_lstart
    is the lstart string captured in the manifest line at daemon birth; a live
    pid with a DIFFERENT lstart is a reused pid and counts as dead. Both sides
    are verbatim `ps -o lstart=` output from the SAME host (m3: no epoch
    parsing, no locale sensitivity), compared by string equality.
    Returns (alive, note, etime)."""
    if not isinstance(pid, int) or pid <= 0:
        return False, "no valid pid in manifest line", None
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False, "pid %d gone" % pid, None
    except PermissionError:
        pass  # exists but not ours — treat as alive
    out = subprocess.run(["ps", "-o", "lstart=,etime=", "-p", str(pid)],
                         stdout=subprocess.PIPE, text=True)
    tokens = (out.stdout or "").split()
    if len(tokens) < 2:
        return False, "pid %d gone (ps race)" % pid, None
    etime, lstart = tokens[-1], " ".join(tokens[:-1])
    if expected_lstart and lstart != expected_lstart:
        return False, ("pid %d REUSED (lstart %s != manifest %s)"
                       % (pid, lstart, expected_lstart)), None
    return True, "alive since %s" % lstart, etime


# ------------------------------------------------------- log parsing (M2/M4)

def _strip_ts(line):
    """Every redteam log line is [HH:MM:SS] -prefixed (M4); strip and lstrip."""
    if line.startswith("["):
        j = line.find("]")
        if 0 < j <= 12:
            return line[j + 1:].lstrip()
    return line.lstrip()


def _terminal_case(body):
    """M2: the terminal per-case line is `ID: STATUS in Xs calls=N` (run.py:3838).
    The `ID -> STATUS` mark fires multiple times per case and carries no
    calls=N, so it is NOT a done signal. Returns (id, status, calls) or None."""
    i = body.find(":")
    if i <= 0:
        return None
    cid = body[:i]
    if not cid or " " in cid:
        return None
    rest = body[i + 1:].lstrip()
    j = rest.rfind(" calls=")
    if j < 0:
        return None
    calls = rest[j + len(" calls="):].strip()
    if not calls.isdigit():
        return None
    mid = rest[:j].rstrip()
    k = mid.rfind(" in ")
    if k < 0:
        return None
    secs = mid[k + len(" in "):]
    if not secs.endswith("s"):
        return None
    try:
        float(secs[:-1])
    except ValueError:
        return None
    head = mid[:k].split()
    return (cid, head[0], int(calls)) if head else None


def _abs(p):
    return p if os.path.isabs(p) else os.path.join(REPO_ROOT, p)


def parse_log(path, suite):
    """redteam: roster from the `cases: ...` line, done from terminal per-case
    lines (M2), current = last `=== ID:` marker, calls-so-far, budget.
    wstream: None — degraded status (liveness + tail only, M3)."""
    if suite == "wstream":
        return None
    try:
        with open(path, errors="replace") as f:
            raw = f.read().splitlines()
    except OSError:
        return None
    d = {"total": None, "roster": [], "done": {}, "current": None, "calls": 0,
         "budget": None}
    open_markers = []  # m8: === markers without a subsequent terminal line
    for line in raw:
        body = _strip_ts(line)
        if body.startswith("cases: "):
            d["roster"] = [c.strip() for c in body[len("cases: "):].split(",") if c.strip()]
            d["total"] = len(d["roster"])
            continue
        if body.startswith("=== "):
            cid = body[4:].split(":", 1)[0].strip()
            if cid:
                open_markers.append(cid)
            continue
        if "budget=" in body:
            tail = body.split("budget=", 1)[1].split()[0]
            if tail.isdigit():
                d["budget"] = int(tail)
                continue
        t = _terminal_case(body)
        if t:
            cid, status, calls = t
            d["done"][cid] = status
            d["calls"] += calls
            for k in range(len(open_markers) - 1, -1, -1):
                if open_markers[k] == cid:
                    del open_markers[k]
                    break
    # m8: current = last open marker; "(idle/complete)" when none is open.
    d["current"] = open_markers[-1] if open_markers else "(idle/complete)"
    return d


# ------------------------------------------------------------------ assess

def _log_shows_clean_completion(log_rel):
    """N2 (apex-ayl.91): the runner's clean-exit marker is the
    'calls used:' line (the last line main() writes before the exit
    paths). The manifest's log path is worktree-relative; resolve it
    against the repo root this file anchors to."""
    if not log_rel:
        return False
    log_path = log_rel if os.path.isabs(log_rel) else os.path.join(
        REPO_ROOT, log_rel)
    try:
        with open(log_path, "rb") as fh:
            fh.seek(0, os.SEEK_END)
            size = fh.tell()
            fh.seek(max(0, size - 65536))
            tail = fh.read().decode("utf-8", "replace")
    except OSError:
        return False
    return "calls used:" in tail


def assess(line):
    """Manifest line + liveness + log parse -> status result dict.
    exit_code_out: 0 running, 3 exited cleanly (terminal line, or the
    N2 clean-completion log heuristic), 5 killed (SIGTERM),
    4 DEAD without a terminal line and no clean-completion marker
    (crash detector)."""
    res = {"line": line, "live": False, "crash": False, "state": "unknown",
           "exit_code_out": 4, "done": None, "total": None, "current": None,
           "calls": None, "budget": None, "elapsed": None, "note": ""}
    status = line.get("status")
    if status in ("exited", "killed"):
        res.update(state=status, crash=False,
                   exit_code_out=3 if status == "exited" else 5,
                   note="terminal line: status=%s exit_code=%s"
                        % (status, line.get("exit_code")))
    else:
        live, note, etime = pid_alive(line.get("pid"), line.get("lstart"))
        res["live"], res["note"] = live, note
        if not live:
            if _log_shows_clean_completion(line.get("log")):
                res.update(state="completed", crash=False, exit_code_out=3,
                           note="no terminal line; run log shows clean "
                                "completion (N2 heuristic)")
            else:
                res.update(state="DEAD", crash=True, exit_code_out=4)
        else:
            res.update(state="running", exit_code_out=0, elapsed=etime)
    if line.get("suite") != "wstream" and line.get("log"):
        parsed = parse_log(_abs(line["log"]), line.get("suite"))
        if parsed:
            res.update(done=len(parsed["done"]), total=parsed["total"],
                       current=parsed["current"], calls=parsed["calls"],
                       budget=parsed["budget"])
    else:
        res["note"] = ((res["note"] + " · " if res["note"] else "")
                       + "wstream: degraded status (liveness + tail only, M3)")
    # Concordance nit #2: skip rows (=== marker, no terminal line) leave a
    # stale "current" open on a COMPLETE run — force the idle marker when
    # the manifest line itself is terminal.
    if status in ("exited", "killed") and res["current"] \
            and res["current"] != "(idle/complete)":
        res["current"] = "(idle/complete)"
    return res


# ---------------------------------------------------------------- commands

def cmd_status(query=None):
    lines, torn = read_manifest()
    if torn:
        print("note: torn last manifest line dropped (writer mid-append)")
    if not lines:
        print("no manifest lines in %s" % MANIFEST_PATH)
        return 2
    matches = [l for l in lines
               if query is None
               or l.get("campaign_id") == query
               or l.get("suite") == query]
    if not matches:
        print("no manifest line for %r" % query)
        return 2
    res = assess(matches[-1])
    line = res["line"]
    print("campaign  %s  (suite %s)" % (line.get("campaign_id"), line.get("suite")))
    print("state     %s  [%s]" % (str(res["state"]).upper(), res["note"]))
    if res["live"]:
        print("pid       %s alive (elapsed %s)" % (line.get("pid"), res["elapsed"] or "?"))
    if res["total"] is not None:
        current = " · current: %s" % res["current"] if res["current"] else ""
        print("progress  %s/%s done%s" % (res["done"], res["total"], current))
    if res["calls"] is not None:
        # Concordance nit #11: per-case calls only — excludes compact/seed
        # calls the runner counts in its "calls used" total.
        print("calls     %s (per-case)%s" % (res["calls"],
              ("/%s" % res["budget"]) if res["budget"] is not None else ""))
    print("log       %s" % line.get("log"))
    if line.get("campaign_id"):
        print("hint      sweepctl tail %s" % line["campaign_id"])
    return res["exit_code_out"]


def cmd_registry():
    lines, torn = read_manifest()
    if not lines:
        print("no manifest lines in %s" % MANIFEST_PATH)
        return 2
    fmt = "%-30s %-9s %-8s %-6s %-16s %s"
    print(fmt % ("CAMPAIGN", "SUITE", "PID", "MARK", "TS", "LOG"))
    for l in lines:
        if l.get("status") in ("exited", "killed"):
            mark = str(l["status"]).upper()
        else:
            alive, _note, _etime = pid_alive(l.get("pid"), l.get("lstart"))
            mark = "LIVE" if alive else "DEAD"
        print(fmt % (str(l.get("campaign_id"))[:30], str(l.get("suite"))[:9],
                     str(l.get("pid"))[:8], mark, str(l.get("ts"))[:16],
                     l.get("log")))
    if torn:
        print("note: torn last manifest line dropped (writer mid-append)")
    return 0


def cmd_tail(campaign_id):
    lines, _torn = read_manifest()
    line = last_matching(lines, campaign_id=campaign_id)
    if line is None:
        print("no manifest line for campaign_id %r" % campaign_id)
        return 2
    log = line.get("log")
    if not log:
        print("no log path recorded for campaign %r (runner may not "
              "have reached the manifest write yet)" % campaign_id)
        return 2
    log = _abs(log)
    if not os.path.isfile(log):
        print("log not found: %s" % log)
        return 2
    print("# following %s (campaign %s)" % (log, campaign_id))
    subprocess.call(["tail", "-n", "40", "-f", log])
    return 0


def build_parser():
    ap = argparse.ArgumentParser(
        prog="sweepctl",
        description=(
            "driver for the driver — durable launch/status/re-attach for smoke "
            "suites (cut 0.5). status exit codes: 0=running, 3=exited cleanly, "
            "5=killed (SIGTERM), 4=DEAD without a terminal manifest line "
            "(crash detector). Full log parse for redteam; wstream status is "
            "degraded (pid liveness + log tail only — wstream log has no "
            "roster/terminal shapes; aligning them is out of this cut)."))
    sub = ap.add_subparsers(dest="cmd", required=True)
    pl = sub.add_parser("launch",
                        help="spawn `python <runner> <args> --daemon` "
                             "(argv as a list — never shell-interpolated)")
    pl.add_argument("suite", choices=sorted(SUITES))
    # REMAINDER (not "*"): runner args verbatim INCLUDING flags like
    # --budget — argparse's "*" would reject any -prefixed token here.
    pl.add_argument("args", nargs=argparse.REMAINDER,
                    help="runner args, verbatim (multi-word safe)")
    ps = sub.add_parser("status",
                        help="last matching manifest line + liveness + log parse")
    ps.add_argument("query", nargs="?",
                    help="campaign_id or suite (default: most recent line)")
    pt = sub.add_parser("tail", help="follow the runner log for a campaign")
    pt.add_argument("campaign_id")
    sub.add_parser("registry", help="all manifest lines with LIVE/DEAD marks")
    return ap


def main(argv=None):
    a = build_parser().parse_args(argv)
    if a.cmd == "launch":
        _p, line = run_launch(a.suite, a.args)
        # Concordance nit #5: a missing manifest line is a WARNING (the
        # daemon may still be pre-daemonize), not a launch failure.
        return 0
    if a.cmd == "status":
        return cmd_status(a.query)
    if a.cmd == "tail":
        return cmd_tail(a.campaign_id)
    return cmd_registry()


if __name__ == "__main__":
    sys.exit(main())
