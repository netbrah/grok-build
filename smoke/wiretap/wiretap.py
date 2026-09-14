#!/usr/bin/env python3
"""wiretap2 — logging pass-through proxy for the LLM proxy, with per-request
capture and auth masking. The definitive wire-evidence layer of the L3
red-team harness (HT-1).

PROVENANCE
----------
Extended from `~/bin/wiretap.py` (86 L, other agent, 2026-09-13 15:13). That
original is READ-ONLY and is NOT modified in place; wiretap2 is a standalone
superset that keeps its stdout summary behavior (tool names + model per
POST /responses) and its SSL/timeout/chunked-re-encode semantics.

Also incorporates the CDX-1 verified one-line fix at
`/tmp/cdx1-spawn/wiretap9099.py` (diff vs the original = exactly
`protocol_version = "HTTP/1.1"` on the handler, with comment), root-caused
in `grok/plans/cdx1-report.md` §3.3 (wiretap operating-seam bug):
HTTP/1.0 status line + manual `Transfer-Encoding: chunked` is rejected by
hyper/reqwest-class clients (`stream disconnected before completion`),
whose reconnect logic re-POSTs the full request (4x quota leak observed).
The --selftest HTTP/1.1 keep-alive SSE assertion is the regression pin.

Extensions over the original (HT-1 spec §2):
  1. Capture: per request -> <capture_dir>/req-NNN.json (method, path, ts,
     headers-MASKED, full body) + <capture_dir>/resp-NNN.jsonl (status,
     response headers, then ONE NDJSON line per SSE frame as it streams —
     frame fidelity, not a post-hoc parse). Dir 0700, files 0600.
  2. Auth masking (non-negotiable): `Authorization` (and any header whose
     value == the ambient key) stored as
     {"masked": true, "sha256_12": "...", "len": N}. The raw key is NEVER
     written to any capture. Self-test asserts this with a canary key.
  3. Method coverage: do_GET/do_DELETE/do_PUT pass-through + one-line log
     (the original 501s on GET).
  4. Threading: ThreadingHTTPServer (original is single-threaded).
  5. CLI: --port N --upstream URL --capture DIR [--ambient-key KEY]
     (defaults: 9098, the llm-proxy, no capture). The ambient key is
     taken from --ambient-key when non-empty, else the WIRETAP_AMBIENT_KEY
     env (set by the red-team runner), else CODEX_LLM_PROXY_KEY. It is
     never read from argv by the caller in normal operation.
  6. Keep: CERT_NONE SSL context (corp internal MITM CA), 600 s upstream
     timeout, chunked re-encode of streamed responses.
  7. Self-test: --selftest — in-process stub upstream (canned SSE + a 400
     case); asserts body round-trip byte-identity, frame-capture order,
     auth masking, GET pass-through, and the CDX-1 HTTP/1.1 keep-alive
     SSE regression pin. No proxy needed.
  8. HTTP/1.1: `protocol_version = "HTTP/1.1"` (CDX-1 fix; see provenance).

Stdlib only. No pip dependencies.
"""
import argparse
import base64
import hashlib
import json
import os
import socket
import ssl
import sys
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# Local debug proxy: trust the corp CA chain (internal MITM CA not in
# homebrew python's default bundle). Kept from the original wiretap.py.
SSL_CTX = ssl.create_default_context()
SSL_CTX.check_hostname = False
SSL_CTX.verify_mode = ssl.CERT_NONE

UPSTREAM_TIMEOUT = 600  # seconds, kept from the original wiretap.py.

# Header names that are ALWAYS masked regardless of value.
ALWAYS_MASK = {"authorization"}

# Headers that must not be forwarded to the upstream (connection-scoped).
NO_FORWARD = {"host", "content-length"}


def _utcnow() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def _sha256_12(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8", "replace")).hexdigest()[:12]


def _mask_value(value: str) -> dict:
    return {"masked": True, "sha256_12": _sha256_12(value), "len": len(value)}


def mask_headers(headers, ambient_key: str):
    """Return a new dict with sensitive header values masked.

    A header is masked if its (lower-cased) name is in ALWAYS_MASK, or if its
    value equals the ambient key (or a `Bearer <key>` form of it).
    """
    masked = {}
    bearer = ("Bearer " + ambient_key) if ambient_key else None
    for k, v in headers.items():
        lname = k.lower()
        sval = v if isinstance(v, str) else str(v)
        if lname in ALWAYS_MASK:
            masked[k] = _mask_value(sval)
        elif ambient_key and (sval == ambient_key or (bearer and sval == bearer)):
            masked[k] = _mask_value(sval)
        else:
            masked[k] = v
    return masked


class FrameSplitter:
    """Accumulate a byte stream and yield complete SSE frames.

    An SSE frame is delimited by a blank line (`\n\n`). Non-SSE bodies simply
    accumulate and are flushed as a single trailing frame on close, which
    preserves byte fidelity for both cases.
    """

    def __init__(self):
        self._buf = b""

    def feed(self, data: bytes):
        self._buf += data
        frames = []
        while True:
            idx = self._buf.find(b"\n\n")
            if idx == -1:
                break
            frames.append(self._buf[:idx])
            self._buf = self._buf[idx + 2:]
        return frames

    def flush(self):
        if self._buf:
            tail = self._buf
            self._buf = b""
            return [tail]
        return []


class Capture:
    """Per-request capture writer. Thread-safe via a lock + counter."""

    def __init__(self, capture_dir: str, ambient_key: str):
        self.dir = capture_dir
        self.ambient_key = ambient_key
        self._lock = threading.Lock()
        self._counter = 0
        os.makedirs(capture_dir, mode=0o700, exist_ok=True)
        try:
            os.chmod(capture_dir, 0o700)
        except OSError:
            pass

    def _next(self) -> int:
        with self._lock:
            self._counter += 1
            return self._counter

    def _write_req(self, n: int, method: str, path: str,
                   headers, body: bytes) -> str:
        path_ = os.path.join(self.dir, "req-%03d.json" % n)
        try:
            body_obj = json.loads(body.decode("utf-8"))
        except Exception:
            try:
                body_obj = body.decode("utf-8")
            except Exception:
                body_obj = {"_b64": base64.b64encode(body).decode("ascii")}
        rec = {
            "n": n,
            "method": method,
            "path": path,
            "ts": _utcnow(),
            "headers": mask_headers(dict(headers), self.ambient_key),
            "body": body_obj,
        }
        self._atomic(path_, json.dumps(rec, indent=2))
        return path_

    def begin_resp(self, n: int, status: int, headers) -> str:
        path_ = os.path.join(self.dir, "resp-%03d.jsonl" % n)
        line = {"n": n, "status": status, "ts": _utcnow(),
                "headers": mask_headers(dict(headers), self.ambient_key)}
        with open(path_, "w") as fh:
            fh.write(json.dumps(line) + "\n")
        os.chmod(path_, 0o600)
        return path_

    def append_frame(self, path_: str, frame: bytes, index: int) -> None:
        text = frame.decode("utf-8", "replace")
        with open(path_, "a") as fh:
            fh.write(json.dumps({"frame_index": index, "frame": text}) + "\n")

    def _atomic(self, path_: str, text: str) -> None:
        with open(path_, "w") as fh:
            fh.write(text + "\n")
        os.chmod(path_, 0o600)


class Handler(BaseHTTPRequestHandler):
    # CDX-1 fix: 1.0 status line + TE:chunked is rejected by
    # hyper/reqwest-class clients (stream disconnected before completion;
    # client re-POSTs the full request on reconnect -> quota leak).
    protocol_version = "HTTP/1.1"
    # Set by serve() before the server starts.
    capture: "Capture | None" = None
    ambient_key: str = ""
    upstream: str = ""

    def log_message(self, *a):
        pass

    def _read_body(self) -> bytes:
        n = int(self.headers.get("Content-Length", 0) or 0)
        return self.rfile.read(n) if n > 0 else b""

    def _stdout_line(self, method: str, path: str, body: bytes) -> None:
        if method == "POST" and "responses" in path:
            names = _tool_names(body)
            model = "?"
            try:
                model = json.loads(body or b"{}").get("model", "?")
            except Exception:
                pass
            print("POST %s model=%s tools(%d): %s" % (
                path, model, len(names), ", ".join(sorted(names))), flush=True)
        else:
            print("%s %s" % (method, path), flush=True)

    def _forward(self, method: str) -> None:
        path = self.path
        body = self._read_body()
        self._stdout_line(method, path, body)

        n = 0
        req_path = None
        resp_path = None
        if self.capture is not None:
            n = self.capture._next()
            req_path = self.capture._write_req(n, method, path,
                                                self.headers, body)
        else:
            req_path = None

        fwd_headers = {k: v for k, v in self.headers.items()
                       if k.lower() not in NO_FORWARD}
        url = self.upstream + path
        req = urllib.request.Request(url, data=body if body else None,
                                     method=method, headers=fwd_headers)
        splitter = FrameSplitter() if self.capture is not None else None
        frame_index = 0
        try:
            with urllib.request.urlopen(req, timeout=UPSTREAM_TIMEOUT,
                                        context=SSL_CTX) as r:
                if self.capture is not None:
                    resp_path = self.capture.begin_resp(n, r.status, r.headers)
                self.send_response(r.status)
                for k, v in r.headers.items():
                    if k.lower() in ("transfer-encoding", "connection",
                                     "content-encoding"):
                        continue
                    self.send_header(k, v)
                self.send_header("Transfer-Encoding", "chunked")
                self.end_headers()
                while True:
                    chunk = r.read(65536)
                    if not chunk:
                        break
                    self.wfile.write(b"%x\r\n" % len(chunk) + chunk + b"\r\n")
                    if self.capture is not None and resp_path:
                        for frame in splitter.feed(chunk):
                            self.capture.append_frame(resp_path, frame,
                                                      frame_index)
                            frame_index += 1
                self.wfile.write(b"0\r\n\r\n")
                if self.capture is not None and resp_path:
                    for frame in splitter.flush():
                        self.capture.append_frame(resp_path, frame,
                                                  frame_index)
                        frame_index += 1
        except urllib.error.HTTPError as e:
            data = e.read()
            if self.capture is not None:
                resp_path = self.capture.begin_resp(n, e.code, e.headers)
                self.capture.append_frame(resp_path, data, frame_index)
            self.send_response(e.code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        except urllib.error.URLError as e:
            if self.capture is not None:
                resp_path = self.capture.begin_resp(
                    n, 502, {"X-Wiretap-Upstream-Error": str(e.reason)})
            self.send_response(502)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps(
                {"error": {"message": "upstream unreachable: %s" % e.reason}
                 }).encode())

    def do_POST(self):
        self._forward("POST")

    def do_GET(self):
        self._forward("GET")

    def do_DELETE(self):
        self._forward("DELETE")

    def do_PUT(self):
        self._forward("PUT")


def _tool_names(body: bytes):
    try:
        d = json.loads(body)
    except Exception:
        return ["<unparseable>"]
    names = []
    for t in d.get("tools") or []:
        if not isinstance(t, dict):
            continue
        if t.get("type") == "function":
            names.append(t.get("name") or (t.get("function") or {}).get("name", "?"))
        elif t.get("type") == "namespace":
            names.append("<ns:%s>" % t.get("name", "?"))
        else:
            names.append("<%s>" % t.get("type", "?"))
    return names


def _free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def serve(port: int, upstream: str, capture_dir: str | None,
          ambient_key: str) -> None:
    Handler.upstream = upstream.rstrip("/")
    Handler.ambient_key = ambient_key
    Handler.capture = (Capture(capture_dir, ambient_key)
                       if capture_dir else None)
    httpd = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    httpd.daemon_threads = True
    print("wiretap2 listening on 127.0.0.1:%d upstream=%s capture=%s" % (
        port, Handler.upstream, capture_dir or "(off)"), flush=True)
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        httpd.server_close()


# ---------------------------------------------------------------------------
# Self-test
# ---------------------------------------------------------------------------

class _StubUpstreamHandler(BaseHTTPRequestHandler):
    """Canned upstream: echoes POST body bytes for verification, serves a
    canned SSE stream on /v1/responses, JSON on /v1/models, and 400 on
    /v1/fail."""
    received_bodies = {}
    lock = threading.Lock()

    def log_message(self, *a):
        pass

    def _send(self, code, body, content_type="application/json",
              chunked=False):
        self.send_response(code)
        self.send_header("Content-Type", content_type)
        if chunked:
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            for piece in body:
                self.wfile.write(b"%x\r\n" % len(piece) + piece + b"\r\n")
            self.wfile.write(b"0\r\n\r\n")
        else:
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    def do_POST(self):
        n = int(self.headers.get("Content-Length", 0) or 0)
        body = self.rfile.read(n) if n else b""
        with self.lock:
            self.received_bodies[self.path] = body
        if self.path == "/v1/fail":
            self._send(400, json.dumps(
                {"error": {"message": "canned 400"}}).encode())
            return
        if self.path == "/v1/responses":
            frames = [
                b"event: response.created\ndata: {\"a\":1}\n\n",
                b"event: response.output_item.added\ndata: {\"b\":2}\n\n",
                b"data: [DONE]\n\n",
            ]
            self._send(200, frames, content_type="text/event-stream",
                       chunked=True)
            return
        self._send(200, b'{"echo":true}')

    def do_GET(self):
        if self.path == "/v1/models":
            self._send(200, json.dumps(
                {"data": [{"id": "stub-model-1"}, {"id": "stub-model-2"}]}
                ).encode())
            return
        self._send(404, b'{"error":"not found"}')


def _selftest() -> int:
    ambient = "canary-key-DO-NOT-LEAK-0123456789"
    import tempfile
    tmp = tempfile.mkdtemp(prefix="wiretap2-selftest-")
    capture_dir = os.path.join(tmp, "capture")
    failures = []

    def check(name, cond, detail=""):
        status = "ok" if cond else "FAIL"
        print("  [%s] %s %s" % (status, name, detail if not cond else ""))
        if not cond:
            failures.append(name)

    # Start stub upstream.
    stub_port = _free_port()
    stub = ThreadingHTTPServer(("127.0.0.1", stub_port), _StubUpstreamHandler)
    stub.daemon_threads = True
    threading.Thread(target=stub.serve_forever, daemon=True).start()
    upstream = "http://127.0.0.1:%d" % stub_port

    # Start wiretap2 proxy in-process.
    prox_port = _free_port()
    Handler.upstream = upstream
    Handler.ambient_key = ambient
    Handler.capture = Capture(capture_dir, ambient)
    prox = ThreadingHTTPServer(("127.0.0.1", prox_port), Handler)
    prox.daemon_threads = True
    threading.Thread(target=prox.serve_forever, daemon=True).start()
    proxy_url = "http://127.0.0.1:%d" % prox_port

    body = json.dumps({"model": "stub-model-1",
                       "input": "hi",
                       "tools": [{"type": "function",
                                  "function": {"name": "read_file"}}]}).encode()

    # --- POST /v1/responses through the proxy --------------------------
    req = urllib.request.Request(
        proxy_url + "/v1/responses", data=body, method="POST",
        headers={"Authorization": "Bearer " + ambient,
                 "Content-Type": "application/json",
                 "X-Custom": "safe-value"})
    with urllib.request.urlopen(req, timeout=30) as r:
        client_body = r.read()

    # 1. Body round-trip byte-identical (what the stub actually received).
    check("body-roundtrip-byte-identical",
          _StubUpstreamHandler.received_bodies.get("/v1/responses") == body)

    # Client got the full SSE stream.
    expect_sse = (b"event: response.created\ndata: {\"a\":1}\n\n"
                  b"event: response.output_item.added\ndata: {\"b\":2}\n\n"
                  b"data: [DONE]\n\n")
    check("sse-stream-to-client", client_body == expect_sse,
          "got=%r" % client_body[:80])

    # 2. Frame capture order in resp-001.jsonl.
    resp_file = os.path.join(capture_dir, "resp-001.jsonl")
    frames = []
    status_line = None
    with open(resp_file) as fh:
        for line in fh:
            d = json.loads(line)
            if "status" in d:
                status_line = d
            elif "frame_index" in d:
                frames.append(d)
    check("resp-status-line", status_line is not None
          and status_line["status"] == 200)
    check("frame-count", len(frames) == 3, "got %d" % len(frames))
    check("frame-order",
          [f["frame_index"] for f in frames] == [0, 1, 2]
          and frames[0]["frame"].startswith("event: response.created")
          and frames[2]["frame"] == "data: [DONE]",
          repr([f.get("frame", "")[:30] for f in frames]))

    # 3. Auth masking in req-001.json; canary absent everywhere.
    req_file = os.path.join(capture_dir, "req-001.json")
    with open(req_file) as fh:
        req_rec = json.load(fh)
    auth = req_rec["headers"].get("Authorization")
    check("auth-masked-shape",
          isinstance(auth, dict) and auth.get("masked") is True
          and auth.get("len") == len("Bearer " + ambient)
          and auth.get("sha256_12") == _sha256_12("Bearer " + ambient),
          repr(auth))
    check("non-auth-header-kept",
          req_rec["headers"].get("X-Custom") == "safe-value")
    check("body-parsed-in-req",
          isinstance(req_rec["body"], dict)
          and req_rec["body"].get("model") == "stub-model-1")

    canary_leak = False
    for fn in sorted(os.listdir(capture_dir)):
        with open(os.path.join(capture_dir, fn)) as fh:
            if ambient in fh.read():
                canary_leak = True
    check("canary-absent-from-capture", not canary_leak)

    # --- GET /v1/models pass-through ----------------------------------
    with urllib.request.urlopen(proxy_url + "/v1/models", timeout=30) as r:
        models = json.loads(r.read())
    check("get-models-passthrough",
          models.get("data", [{}])[0].get("id") == "stub-model-1")

    # --- 400 case ------------------------------------------------------
    try:
        urllib.request.urlopen(
            urllib.request.Request(proxy_url + "/v1/fail",
                                   data=b"{}", method="POST"),
            timeout=30)
        err_status = None
    except urllib.error.HTTPError as e:
        err_status = e.code
    check("400-passthrough", err_status == 400, "got %r" % err_status)

    # --- CDX-1 regression pin: HTTP/1.1 keep-alive SSE ------------------
    # urllib sets `Connection: close`, so this uses http.client (HTTP/1.1
    # by default) and a REUSED connection: if the status line / body
    # framing is broken (the 1.0+chunked defect), the second request on
    # the same socket fails exactly like codex's hyper client did.
    import http.client
    conn = http.client.HTTPConnection("127.0.0.1", prox_port, timeout=30)
    ok_11 = ok_keepalive = ok_full = False
    first_status = None
    first_body = b""
    try:
        conn.request("POST", "/v1/responses", body=body,
                     headers={"Authorization": "Bearer " + ambient,
                              "Content-Type": "application/json"})
        r1 = conn.getresponse()
        first_status = (r1.status, r1.version)
        first_body = r1.read()
        ok_11 = r1.version == 11 and r1.status == 200
        ok_full = first_body == expect_sse
        conn.request("POST", "/v1/responses", body=body,
                     headers={"Authorization": "Bearer " + ambient,
                              "Content-Type": "application/json"})
        r2 = conn.getresponse()
        second_body = r2.read()
        ok_keepalive = r2.status == 200 and second_body == expect_sse
    except Exception as e:
        check("http11-keepalive-sse", False, repr(e))
    else:
        check("http11-status-line", ok_11,
              "got status=%r (want (200, 11))" % (first_status,))
        check("http11-full-sse-stream", ok_full,
              "body[:80]=%r" % first_body[:80])
        check("http11-keepalive-same-conn", ok_keepalive,
              "second request on same connection failed (CDX-1 class)")
    finally:
        conn.close()

    # --- req/resp numbering consistency --------------------------------
    reqs = sorted(f for f in os.listdir(capture_dir) if f.startswith("req-"))
    resps = sorted(f for f in os.listdir(capture_dir) if f.startswith("resp-"))
    check("capture-numbering", len(reqs) == len(resps) and len(reqs) == 5,
          "reqs=%s resps=%s" % (reqs, resps))

    prox.server_close()
    stub.server_close()
    print("SELFTEST %s (%d failures)" % (
        "GREEN" if not failures else "RED", len(failures)))
    return 0 if not failures else 1


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description="wiretap2 (see module docstring)")
    p.add_argument("--port", type=int, default=9098)
    p.add_argument("--upstream",
                   default="https://llm-proxy-api.ai.eng.netapp.com")
    p.add_argument("--capture", default=None,
                   help="capture dir (per-request req/resp evidence)")
    p.add_argument("--ambient-key", default=None,
                   help="override ambient key for masking (tests); "
                        "empty/omitted reads WIRETAP_AMBIENT_KEY, then "
                        "CODEX_LLM_PROXY_KEY")
    p.add_argument("--selftest", action="store_true")
    a = p.parse_args(argv)
    if a.selftest:
        return _selftest()
    # HYG-1: the ambient key travels via env, never argv (ps-visible).
    # Precedence: explicit --ambient-key value (back-compat alias), then
    # WIRETAP_AMBIENT_KEY (set by the red-team runner), then the
    # pre-existing CODEX_LLM_PROXY_KEY ambient fallback. An empty or
    # omitted flag value falls through to the env seams.
    ambient = (a.ambient_key
               or os.environ.get("WIRETAP_AMBIENT_KEY", "")
               or os.environ.get("CODEX_LLM_PROXY_KEY", ""))
    serve(a.port, a.upstream, a.capture, ambient)
    return 0


if __name__ == "__main__":
    sys.exit(main())
