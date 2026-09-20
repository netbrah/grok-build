"""lib.launch — cut 0.5 (plan section 7, D-1): self-daemonize for smoke-suite runners.

self_daemonize(log_path, pidfile, manifest_path=None):
  setsid + double-fork (macOS-safe, no daemon(3)). The launched (pre-fork)
  runner exits 0; the daemon grandchild reopens log_path on fds 1+2
  (stdin -> /dev/null), writes the daemon pid to pidfile, and appends its OWN
  manifest line to the shared JSONL registry (SINGLE-WRITER RULE: only the
  daemonized process writes its manifest line — no launcher/daemon race).

  Terminal lines: the daemon installs a SIGTERM handler that appends
  status:"killed" (exit 143), and records status:"exited" + exit_code on clean
  interpreter exit (sys.exit wrapper carries the real code; excepthook covers
  uncaught exceptions; a finalizer is the atexit-free backstop). Exactly one
  terminal line per run (terminal_written guard, concordance M5).

Manifest 'running' line schema: {v:1, ts, campaign_id, suite, runner, pid, log,
  out_dir, bin_sha12, git_head, python, config_sha12, argv, launcher, lstart,
  status:"running"}. lstart (M1) is the daemon's own `ps -o lstart=` captured
  at write time — status() compares live ps lstart against it (pid-reuse guard;
  the manifest ts alone was not implementable).
Terminal line schema: {ts, campaign_id, pid, status:"exited"|"killed",
  exit_code, ended_ts}.

stdlib only. Note: a runner that terminates via os._exit() never reaches the
terminal-line path; `sweepctl status` then reports DEAD (exit 4) — both current
runners terminate via sys.exit(), so this is not a live gap. Same for
`raise SystemExit(N)` (concordance nit #1): CPython never routes SystemExit
through sys.excepthook, so the terminal line would fall to the __del__
backstop with exit_code 0 — runners that want the real code in the terminal
line must call sys.exit(), not raise SystemExit.
"""
import hashlib
import json
import os
import signal
import subprocess
import sys
import time

SMOKE_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REPO_ROOT = os.path.dirname(SMOKE_ROOT)
DEFAULT_MANIFEST = os.path.join(SMOKE_ROOT, "runs.jsonl")

# Per-daemon terminal-line state. Manifest stays None until self_daemonize
# runs, so importers of this module (sweepctl, selftests) can never write one.
_TERMINAL = {"manifest": None, "campaign_id": None, "pid": None, "written": False}


def utc_ts():
    return time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())


def _sha12(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()[:12]


def _ps_lstart(pid):
    out = subprocess.run(["ps", "-o", "lstart=", "-p", str(pid)],
                         stdout=subprocess.PIPE, text=True)
    return " ".join((out.stdout or "").split())


def _git_head():
    try:
        out = subprocess.run(["git", "-C", REPO_ROOT, "rev-parse", "--short", "HEAD"],
                             stdout=subprocess.PIPE, text=True,
                             stderr=subprocess.DEVNULL)
        return (out.stdout or "").strip() or "unknown"
    except OSError:
        return "unknown"


def _rel(path):
    path = os.path.abspath(path)
    rel = os.path.relpath(path, REPO_ROOT)
    return path if rel.startswith("..") else rel


def _campaign_id_from_argv(argv):
    i = 0
    while i < len(argv):
        a = argv[i]
        if a == "--campaign-id" and i + 1 < len(argv):
            return argv[i + 1]
        if a.startswith("--campaign-id="):
            return a.split("=", 1)[1]
        i += 1
    return None


def _suite_from_argv(argv):
    norm = (argv[0] if argv else "").replace(os.sep, "/")
    if "/smoke/" in norm and norm.endswith("/run.py"):
        return norm.rsplit("/", 2)[1]
    d = os.path.dirname(os.path.abspath(norm)) if norm else ""
    return os.path.basename(d) or "unknown"


def _config_sha12(suite):
    candidate = os.path.join(SMOKE_ROOT, suite, "manifest.json")
    if os.path.isfile(candidate):
        try:
            return _sha12(candidate)
        except OSError:
            pass
    return "none"


def _bin_sha12():
    # sweepctl launch exports the resolved release binary ($GROK_BIN override or
    # target/release default) here so the daemon can hash it post-fork.
    raw = os.environ.get("_SWEEPCTL_BIN")
    if raw and os.path.isfile(raw):
        try:
            return _sha12(raw)
        except OSError:
            pass
    return "unknown"


def _append_manifest(manifest_path, obj):
    # One write() of a <1KB JSONL line — atomic-enough on APFS (spec risk note).
    fd = os.open(manifest_path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o644)
    try:
        os.write(fd, (json.dumps(obj) + "\n").encode("utf-8"))
    finally:
        os.close(fd)


def _terminal_diag(msg):
    """N2 (apex-ayl.91): the terminal-line paths self-announce on stderr
    (the daemon's stderr is the run log) so a lost terminal line is
    diagnosable from the log alone — which path fired, which guard
    stopped it, and which manifest it wrote to."""
    try:
        print("[terminal] %s" % msg, file=sys.stderr, flush=True)
    except Exception:
        pass


def append_terminal_line(status, exit_code, state=None):
    """Append exactly one terminal manifest line per run (M5 guard).
    Returns True if this call wrote it. Concordance nit #6: the written flag
    is set only AFTER a successful write, so a failed append leaves the
    single-shot open for a later path instead of silently losing the line."""
    st = state if state is not None else _TERMINAL
    if st["written"] or st["manifest"] is None:
        _terminal_diag("NOT WRITTEN (guard: written=%s manifest=%s) "
                       "status=%s exit_code=%s"
                       % (st["written"], st["manifest"], status, exit_code))
        return False
    try:
        _append_manifest(st["manifest"], {
            "ts": utc_ts(), "campaign_id": st["campaign_id"], "pid": st["pid"],
            "status": status, "exit_code": exit_code, "ended_ts": utc_ts(),
        })
    except OSError as e:
        _terminal_diag("APPEND FAILED status=%s exit_code=%s manifest=%s "
                       "err=%s" % (status, exit_code, st["manifest"], e))
        return False
    st["written"] = True
    _terminal_diag("wrote status=%s exit_code=%s manifest=%s"
                   % (status, exit_code, st["manifest"]))
    return True


def _code_of(code):
    if code is None:
        return 0
    if isinstance(code, int):
        return code
    return 1


def _on_term(signum, frame):
    append_terminal_line("killed", 143)
    os._exit(143)


class _ExitFinalizer:
    """Backstop terminal line at interpreter shutdown (atexit is not in the
    stdlib allow-list). Fires last, so it only writes when neither the sys.exit
    wrapper nor the excepthook wrapper did (terminal_written guard)."""
    def __del__(self):
        _terminal_diag("__del__ backstop firing")
        append_terminal_line("exited", 0)


_FINALIZER = _ExitFinalizer()


def self_daemonize(log_path, pidfile, manifest_path=None):
    """Detach the calling runner (setsid + double-fork) and register it.

    The original process exits 0 immediately; the daemon grandchild reopens the
    log on fds 1+2, writes pidfile, appends its own 'running' manifest line,
    and installs the SIGTERM / clean-exit terminal-line paths. Returns None in
    the daemon (the original never returns).
    """
    manifest_path = manifest_path or DEFAULT_MANIFEST
    if os.fork() > 0:
        os._exit(0)
    try:
        os.setsid()
    except OSError:
        pass  # already a session leader (e.g. Popen start_new_session=True)
    if os.fork() > 0:
        os._exit(0)
    # ---- daemon (grandchild) ----
    log_fd = os.open(log_path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o644)
    null_fd = os.open(os.devnull, os.O_RDONLY)
    os.dup2(null_fd, 0)
    os.dup2(log_fd, 1)
    os.dup2(log_fd, 2)
    # m1: fd 0 from /dev/null, fds 1+2 on the log, then close ALL higher
    # inherited fds (PTY slave, pipes, sockets) — no SIGHUP on PTY close.
    # Must happen before this module or the runner opens anything; D-3 wiring
    # calls self_daemonize at top of main() (m10 ordering).
    for i in range(3, 4096):
        try:
            os.close(i)
        except OSError:
            pass

    argv = list(sys.argv)
    out_dir = os.path.dirname(log_path)
    fields = {
        "v": 1,
        "ts": utc_ts(),
        "campaign_id": (_campaign_id_from_argv(argv)
                        or (os.path.basename(out_dir) or ("campaign-%d" % os.getpid()))),
        "suite": _suite_from_argv(argv),
        "runner": _rel(argv[0]) if argv else "unknown",
        "pid": os.getpid(),
        "log": _rel(log_path),
        "out_dir": _rel(out_dir),
        "bin_sha12": _bin_sha12(),
        "git_head": _git_head(),
        "python": "cpython-%s (%s)" % (sys.version.split()[0], sys.executable),
        "config_sha12": None,  # filled below (needs suite)
        "argv": argv,
        "launcher": os.environ.get("_SWEEPCTL_LAUNCHER", "unknown"),
        "lstart": None,        # filled below (M1)
        "status": "running",
    }
    fields["config_sha12"] = _config_sha12(fields["suite"])
    fields["lstart"] = _ps_lstart(os.getpid())
    # Concordance nit #3: the provenance transport env vars are consumed —
    # drop them so the runner's hermetic children never inherit them.
    os.environ.pop("_SWEEPCTL_BIN", None)
    os.environ.pop("_SWEEPCTL_LAUNCHER", None)
    _TERMINAL["manifest"] = manifest_path
    _TERMINAL["campaign_id"] = fields["campaign_id"]
    _TERMINAL["pid"] = fields["pid"]

    with open(pidfile, "w") as f:
        f.write("%d\n" % os.getpid())

    _append_manifest(manifest_path, fields)

    signal.signal(signal.SIGTERM, _on_term)

    # sys.exit wrapper: carry the real exit code into the terminal line.
    _orig_exit = sys.exit
    def _daemon_exit(code=None):
        append_terminal_line("exited", _code_of(code))
        _orig_exit(code)
    sys.exit = _daemon_exit

    # Uncaught-exception path: terminal line exit_code 1, then the default hook
    # prints the traceback (stderr is the log, via dup2).
    _orig_hook = sys.excepthook
    def _daemon_hook(exc_type, exc, tb):
        append_terminal_line("exited", 1)
        _orig_hook(exc_type, exc, tb)
    sys.excepthook = _daemon_hook

    return None
