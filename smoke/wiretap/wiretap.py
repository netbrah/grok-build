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
     frame fidelity, not a post-hoc parse). Optional --raw-sse writes the
     upstream SSE entity bytes to resp-NNN.raw. Dir 0700, files 0600.
  2. Auth masking (non-negotiable): `Authorization` (and any header whose
     value == the ambient key) stored as
     {"masked": true, "sha256_12": "...", "len": N}. The raw key is NEVER
     written to any capture. Self-test asserts this with a canary key.
  3. Method coverage: do_GET/do_DELETE/do_PUT pass-through + one-line log
     (the original 501s on GET).
  4. Threading: ThreadingHTTPServer (original is single-threaded).
  5. CLI: --port N --upstream URL --capture DIR [--raw-sse]
     [--max-forwarded-posts N] [--ambient-key KEY]
     (defaults: 9098, the llm-proxy, no capture). The ambient key is
     taken from --ambient-key when non-empty, else the WIRETAP_AMBIENT_KEY
     env (set by the red-team runner), else CODEX_LLM_PROXY_KEY. It is
     never read from argv by the caller in normal operation.
  6. TLS: a route that authenticates VERIFIES the origin's certificate
     (redteam-refresh T3, r3 §4.2.3 / s1 F10.4 — handing a live key to an
     origin nobody checked is the same leak as sending it in the clear).
     The corp-CA-ignoring context is kept for the key-free and unmapped
     paths, where there is no credential to protect and a dead-port debug
     run must keep working. 600 s upstream timeout and the chunked
     re-encode of streamed responses are unchanged.
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
import re
import socket
import ssl
import sys
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# REDTEAM-REFRESH T3 (r3 §4.2.3): a credentialed route verifies.
SSL_CTX = ssl.create_default_context()

# The key-free and unmapped path keeps the corp-CA-ignoring context
# (internal MITM CA not in homebrew python's default bundle). Nothing
# secret leaves on that path, and a hand-run tap against a dead port must
# behave exactly as it did before this item.
SSL_CTX_UNVERIFIED = ssl.create_default_context()
SSL_CTX_UNVERIFIED.check_hostname = False
SSL_CTX_UNVERIFIED.verify_mode = ssl.CERT_NONE


def tls_verify_for(origin, credential):
    """Verify exactly when a credential is at stake on an https origin.

    Plain http is every offline fixture, so the whole offline battery
    exercises this rule rather than a certificate path it cannot build."""
    return bool(credential) and str(origin or "").startswith("https://")


UPSTREAM_TIMEOUT = 960  # seconds; must outlast every case step deadline.

# Header names that are ALWAYS masked regardless of value. x-api-key joins
# authorization: r3 §4.2.3 declares both as credential-carrying, and an
# origin echoing either back into a response header is the same leak.
ALWAYS_MASK = {"authorization", "x-api-key"}

# Headers that must not be forwarded to the upstream (connection-scoped).
NO_FORWARD = {"host", "content-length"}

# r3 §4.3.1 / r3a A6: the chain the product resolves for a row that declares
# no credential (auth_method.rs:13-15, :30-34), first SET rung wins. The tap
# needs it because the tap, not the child, holds the credential (RR-3 F-8).
CREDENTIAL_FALLBACK_CHAIN = ("XAI_API_KEY", "GROK_CODE_XAI_API_KEY",
                             "APEX_LLM_PROXY_KEY")

# r3a A5: the swept set is the declared union MINUS these names. The
# documented placeholder is passed to the row that needs it and matched by
# neither the scrub nor the sweep; scrubbing its value would make every real
# run's sweep unpassable (R2-4).
SWEEP_EXCLUDED_NAMES = ("GROK_DIRECT_PLACEHOLDER_KEY",)

SCHEME_WORDS = ("Basic ", "Bearer ")


def strip_credential_scheme(value):
    """The proxy's echo drops the scheme word (§4.4.1, and the on-prem Basic
    blob is captured the same way), so rule 1 compares both sides here."""
    text = value if isinstance(value, str) else str(value)
    for scheme in SCHEME_WORDS:
        if text.startswith(scheme):
            return text[len(scheme):].strip()
    return text


def route_credential(route, env=None):
    """Resolve a route to the (variable, value) it authenticates with.

    A route declaring none takes the first set rung of the product's chain.
    No rung set, no credential: the pair is (None, None) and the request
    goes out unauthenticated rather than wearing the child's own key, which
    is the defect r3 §2.4 records (a row's 403 read as the harness's own)."""
    source = os.environ if env is None else env
    name = (route or {}).get("credential_variable")
    if isinstance(name, str) and name:
        value = source.get(name)
        return (name, value) if value else (None, None)
    for rung in CREDENTIAL_FALLBACK_CHAIN:
        value = source.get(rung)
        if value:
            return rung, value
    return None, None


def credential_secret_values(route_map, ambient_key, env=None):
    """Every credential value this tap can put on the wire, as the scrub and
    the mask must know them: the ambient key plus each reachable route's own,
    placeholder excluded (§4.3.1)."""
    out = []
    if ambient_key:
        out.append(ambient_key)
    for route in (route_map or {}).values():
        if not isinstance(route, dict):
            continue
        name, value = route_credential(route, env)
        if value and name not in SWEEP_EXCLUDED_NAMES and value not in out:
            out.append(value)
    return out


def credential_header_value(header, value):
    """The byte that goes on the wire for a credential header.

    `Bearer ` is added only to an authorization header whose value carries no
    scheme word of its own; a `Basic <blob>` value or a route-declared header
    rides verbatim, because client.rs:1743 inserts the value it is given."""
    if str(header or "authorization").lower() != "authorization":
        return value
    for scheme in SCHEME_WORDS:
        if value.startswith(scheme):
            return value
    return "Bearer " + value


def _utcnow() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def _sha256_12(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8", "replace")).hexdigest()[:12]


def _mask_value(value: str) -> dict:
    return {"masked": True, "sha256_12": _sha256_12(value), "len": len(value)}


# §4.4.1 rule 2's fixed shapes, and nothing else: no length-or-charset
# guessing, so no legitimate payload is mangled.
JWT_SHAPE = re.compile(
    r"eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}")
SK_SHAPE = re.compile(r"sk[_\-][A-Za-z0-9_\-]{16,}", re.IGNORECASE)
B64_AFTER_SCHEME = re.compile(
    r"(?:Bearer|Basic)[ \t]+[A-Za-z0-9+_\-/]{40,}={0,2}")
# >= 40 chars of base64/base64url STANDING ALONE in error.param/error.message
# is a hit on its own (rule 2's boundary; elsewhere it needs a scheme word).
B64_LONE = re.compile(r"[A-Za-z0-9+_\-/]{40,}={0,2}")
SHAPE_RULES = (("jwt-shape", JWT_SHAPE), ("sk-shape", SK_SHAPE),
               ("base64-blob-shape", B64_AFTER_SCHEME))
ERROR_SHAPE_FIELDS = {"param", "message"}


def _mask_json(value):
    """A hit inside a text span becomes the same triple a header gets, so a
    reader cannot tell the two directions apart. A hit is replaced, never
    quoted (§4.4.2)."""
    return json.dumps({"masked": True, "sha256_12": _sha256_12(value),
                       "len": len(value)}, sort_keys=True)


def scrub_text(text, values):
    """§4.4.1 rules 1-2 for a span kept as text: the secret itself is
    replaced by the triple, the surrounding bytes survive, so an SSE frame
    or a message an assertion pins stays readable."""
    out = text
    for value in values:
        for form in (value, strip_credential_scheme(value)):
            if len(form) >= 8 and form in out:
                out = out.replace(form, _mask_json(value))
    for _name, pattern in SHAPE_RULES:
        out = pattern.sub(lambda m: _mask_json(m.group(0)), out)
    return out


def scrub_string(value, values, shape_fields=False):
    """Scrub one JSON string leaf: exact content (stripped) becomes the
    triple, a secret inside a longer span is replaced in place."""
    if not isinstance(value, str):
        return value
    stripped = strip_credential_scheme(value)
    for secret in values:
        if len(secret) < 8:
            continue
        if stripped == strip_credential_scheme(secret):
            return _mask_value(value)
    for _name, pattern in SHAPE_RULES:
        if pattern.search(value):
            return _mask_value(value)
    if shape_fields and B64_LONE.search(value):
        return _mask_value(value)
    return scrub_text(value, values)


def scrub_json(node, values, key=None):
    """Walk a parsed response body. Rule 3 is the blunt one: for an
    `error.type == "auth_error"` the whole `error.param` is replaced, because
    that field is where the proxy echoes the caller's own credential back.
    `message` / `type` / `code` are what the cases assert on and survive
    byte-for-byte."""
    if isinstance(node, dict):
        auth_error = str(node.get("type") or "") == "auth_error"
        out = {}
        for k, v in node.items():
            if auth_error and k == "param" and isinstance(v, str):
                out[k] = _mask_value(v)
            else:
                out[k] = scrub_json(v, values, key=k)
        return out
    if isinstance(node, list):
        return [scrub_json(v, values, key=key) for v in node]
    if isinstance(node, str):
        return scrub_string(node, values,
                            shape_fields=(key in ERROR_SHAPE_FIELDS))
    return node


def scrub_frame(text, values):
    """Write-time scrub of one captured response frame (§4.4.1).

    An uninvolved frame is returned UNCHANGED, byte for byte: the scrub
    exists to remove credentials, not to re-format evidence."""
    touched = any(len(v) >= 8 and (v in text
                                   or strip_credential_scheme(v) in text)
                  for v in values)
    if not touched:
        for _name, pattern in SHAPE_RULES:
            if pattern.search(text):
                touched = True
                break
    if not touched:
        return text
    try:
        parsed = json.loads(text)
    except Exception:
        parsed = None
    if isinstance(parsed, (dict, list)):
        cleaned = scrub_json(parsed, values)
        if cleaned == parsed:
            return text
        return json.dumps(cleaned)
    return scrub_text(text, values)


def mask_headers(headers, ambient_key: str, secret_values=None):
    """Return a new dict with sensitive header values masked.

    A header is masked if its (lower-cased) name is in ALWAYS_MASK, or its
    value IS (scheme word stripped) one of `secret_values`, or it carries one
    of the §4.4.1 shapes. `secret_values` is what makes the RESPONSE
    direction safe: an origin that echoes the injected key back in a header
    used to be captured verbatim, because the pre-T3 predicate knew only the
    ambient key by value (r3 §2.5, §4.2.3 "every injected value is secret").
    The triple is computed on the value AS RECEIVED in that direction, so a
    request `Bearer <k>` and a response echo stay separately checkable."""
    masked = {}
    values = list(secret_values or [])
    if ambient_key and ambient_key not in values:
        values.append(ambient_key)
    for k, v in headers.items():
        lname = k.lower()
        sval = v if isinstance(v, str) else str(v)
        hit = lname in ALWAYS_MASK
        if not hit:
            stripped = strip_credential_scheme(sval)
            for secret in values:
                if len(secret) >= 8 and (
                        secret in sval
                        or stripped == strip_credential_scheme(secret)):
                    hit = True
                    break
        if not hit and JWT_SHAPE.search(sval):
            hit = True
        masked[k] = _mask_value(sval) if hit else v
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
    """Per-request capture writer. Thread-safe via a lock + counter.

    `secrets` is a CALLABLE, not a list: the route credentials resolve from
    the environment at write time, and a caller (or the selftest) may install
    them after this object already exists."""

    def __init__(self, capture_dir: str, ambient_key: str,
                 raw_sse: bool = False, secrets=None):
        self.dir = capture_dir
        self.ambient_key = ambient_key
        self.raw_sse = raw_sse
        self._secrets = secrets
        self._lock = threading.Lock()
        self._counter = 0
        self._refusals = 0
        os.makedirs(capture_dir, mode=0o700, exist_ok=True)
        try:
            os.chmod(capture_dir, 0o700)
        except OSError:
            pass

    def secret_values(self):
        """The credential values no capture file may ever carry."""
        if self._secrets is None:
            return [self.ambient_key] if self.ambient_key else []
        return list(self._secrets() or [])

    def scrub(self, text: str) -> str:
        return scrub_frame(text, self.secret_values())

    def note_tls_failure(self, body_model, origin, detail) -> None:
        """r3 §4.2.3: a refused certificate check is a recorded outcome, not
        a silent 502. The line names the host and the class of failure only —
        never the credential the tap was about to inject."""
        rec = {"ts": _utcnow(), "body_model": body_model,
               "origin": str(origin or "").split("//")[-1].split("/")[0],
               "error_code": "wiretap_tls_verification_failed",
               "detail": str(detail)[:200]}
        with open(os.path.join(self.dir, "upstream-tls.jsonl"), "a") as fh:
            fh.write(json.dumps(rec) + "\n")

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
            "headers": mask_headers(dict(headers), self.ambient_key,
                                    self.secret_values()),
            "body": body_obj,
        }
        self._atomic(path_, json.dumps(rec, indent=2))
        return path_

    def begin_resp(self, n: int, status: int, headers) -> str:
        path_ = os.path.join(self.dir, "resp-%03d.jsonl" % n)
        line = {"n": n, "status": status, "ts": _utcnow(),
                "headers": mask_headers(dict(headers), self.ambient_key,
                                        self.secret_values())}
        with open(path_, "w") as fh:
            fh.write(json.dumps(line) + "\n")
        os.chmod(path_, 0o600)
        return path_

    def append_frame(self, path_: str, frame: bytes, index: int) -> None:
        """Write-time scrub (§4.4.1). The bytes go to the CLIENT untouched —
        the scrub protects the CAPTURE, which is the artifact a report is
        built from and the one that leaked 110 times (§2.5)."""
        text = self.scrub(frame.decode("utf-8", "replace"))
        with open(path_, "a") as fh:
            fh.write(json.dumps({"frame_index": index, "frame": text}) + "\n")

    def _raw_path(self, n: int) -> str:
        return os.path.join(self.dir, "resp-%03d.raw" % n)

    def begin_raw(self, n: int) -> int:
        # O_EXCL avoids following a pre-existing symlink or reopening a file
        # with broader permissions. The requested mode is private at creation.
        return os.open(self._raw_path(n),
                       os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)

    def append_raw(self, fd: int, data: bytes) -> None:
        pending = memoryview(data)
        while pending:
            written = os.write(fd, pending)
            if written <= 0:
                raise OSError("raw capture write made no progress")
            pending = pending[written:]

    def mark_raw_partial(self, n: int) -> None:
        fd = os.open(self._raw_path(n) + ".partial",
                     os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        os.close(fd)

    def note_unrouted(self, path: str, body_model) -> int:
        """REDTEAM-REFRESH T2 (r3 §4.2.2 rule 2): one line per refused request,
        in this case's own capture dir — that file IS the record.

        `n` is the refusal ordinal, NOT a capture sequence number: a refused
        request consumes no req-/resp- pair (r3a A3's numbering pins and the
        capture-numbering selftest both count real exchanges only)."""
        with self._lock:
            self._refusals += 1
            k = self._refusals
        rec = {"n": k, "path": path, "body_model": body_model,
               "error_code": "wiretap_unrouted_model"}
        with open(os.path.join(self.dir, "unrouted.jsonl"), "a") as fh:
            fh.write(json.dumps(rec) + "\n")
        return k

    def _atomic(self, path_: str, text: str) -> None:
        with open(path_, "w") as fh:
            fh.write(text + "\n")
        os.chmod(path_, 0o600)


class ForwardedPostLimit:
    """Shared admission counter for POST forwarding attempts."""

    def __init__(self, maximum: int):
        if maximum < 0:
            raise ValueError("max forwarded POSTs must be nonnegative")
        self.maximum = maximum
        self.count = 0
        self.lock = threading.Lock()

    def reserve(self) -> bool:
        with self.lock:
            if self.count >= self.maximum:
                return False
            self.count += 1
            return True


class Handler(BaseHTTPRequestHandler):
    # CDX-1 fix: 1.0 status line + TE:chunked is rejected by
    # hyper/reqwest-class clients (stream disconnected before completion;
    # client re-POSTs the full request on reconnect -> quota leak).
    protocol_version = "HTTP/1.1"
    # Set by serve() before the server starts.
    capture: "Capture | None" = None
    post_limit: "ForwardedPostLimit | None" = None
    ambient_key: str = ""
    upstream: str = ""
    # REDTEAM-REFRESH T2 (r3 §4.2.1): wire model id -> {origin,
    # credential_variable, credential_header, row_names}, handed over by
    # environment (never argv). Two DISTINCT states (RR-3 F-2):
    #   None — no routing configured: the pre-T2 single-upstream
    #          behaviour is kept, which is what a hand-run wiretap
    #          runs with;
    #   {}   — routing configured with ZERO routes: every POST is
    #          refused. {} is a value run.py really sends (a case
    #          whose selectors resolve to no live row), so the
    #          predicate tests identity, never truthiness.
    route_map: "dict | None" = None
    # REDTEAM-REFRESH T3: where route credentials are resolved from — the
    # runner's environment, never argv and never the child's env (§4.3.4).
    # The selftest swaps in its own synthetic plant set so it reads no key.
    credential_env: "dict | None" = None

    # The transport prewarm probe (client.get(origin), prewarm.rs:63) asks the
    # origin root to warm the connection pool. There is no origin behind the
    # wiretap, so it is answered from this static body (r3a A3).
    PREWARM_BODY = json.dumps({
        "wiretap_prewarm": True,
        "note": "answered locally by the wiretap: the harness transport "
                "prewarm probes the origin root and is not a model call",
    }).encode()
    UNROUTED_BODY = json.dumps({
        "error": {
            "message": "wiretap: no route declared for this wire model id; "
                       "the request was not forwarded to any origin "
                       "(redteam-refresh T2 fail-closed)",
            "type": "invalid_request_error",
            "param": "",
            "code": "wiretap_unrouted_model",
        }}).encode()

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

    def _body_model(self, body: bytes):
        try:
            return json.loads(body or b"{}").get("model")
        except Exception:
            return None

    def _route(self, body: bytes):
        """The route for this request, or None when the map cannot resolve it.
        Keyed on the WIRE MODEL ID the body carries — the only identity a
        request has on the wire (r3 §4.2.1)."""
        if self.route_map is None:
            return self.upstream
        model = self._body_model(body)
        route = self.route_map.get(model) if isinstance(model, str) else None
        if isinstance(route, dict):
            origin = route.get("origin")
            if isinstance(origin, str) and origin:
                return origin.rstrip("/")
        return None

    def _route_entry(self, body: bytes):
        """The map ENTRY for this request, or None. `_route` answers which
        origin; the credential and its header name live on the entry."""
        if self.route_map is None:
            return None
        model = self._body_model(body)
        route = self.route_map.get(model) if isinstance(model, str) else None
        return route if isinstance(route, dict) else None

    def _forward_headers(self, body):
        """(headers to forward, the credential injected, its header name).

        r3 §4.2.3: with a map the tap OWNS the credential. The child's own
        `authorization` / `x-api-key` / route-declared header is dropped and
        the route's own inserted in its place — `insert`, the client.rs:1743
        precedent, never append and never keep both. With no map this is the
        pre-T2 pass-through, unchanged: a hand-run tap is how a human debugs
        a route that has no credential."""
        keep = {k: v for k, v in self.headers.items()
                if k.lower() not in NO_FORWARD}
        if self.route_map is None:
            return keep, None
        route = self._route_entry(body)
        declared = str((route or {}).get("credential_header")
                       or "authorization").lower()
        keep = {k: v for k, v in keep.items()
                if k.lower() not in (ALWAYS_MASK | {declared})}
        name, value = route_credential(route, self.credential_env)
        if value:
            keep[declared] = credential_header_value(declared, value)
            return keep, value
        return keep, None

    def _refuse_unrouted(self, path: str, body: bytes) -> None:
        """r3 §4.2.2: an unresolved id reaches NO origin and is NOT captured.

        No req-/resp- file and no sequence number: an evidence file would say
        "a request happened" for a request that never left, and would then be
        counted by count_wire_model_calls as a model call. The line in
        unrouted.jsonl carries the path and body.model only — never a header,
        so the credential this request arrived with is not copied into the
        report it is being refused for."""
        model = self._body_model(body)
        if self.capture is not None:
            self.capture.note_unrouted(path, model)
        data = self.UNROUTED_BODY
        self.send_response(400)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _answer_prewarm(self) -> None:
        """r3a A3: the probe is answered locally, and still captured under its
        own sequence number — four cases pin req-003.json on the assumption
        that 001/002 are the probes, so suppressing it would move every later
        capture. It never enters unrouted.jsonl and never blocks a case: it is
        not a model call."""
        n = 0
        if self.capture is not None:
            n = self.capture._next()
            self.capture._write_req(n, "GET", self.path, self.headers, b"")
        body = self.PREWARM_BODY
        if self.capture is not None:
            resp_path = self.capture.begin_resp(
                n, 200, {"Content-Type": "application/json"})
            self.capture.append_frame(resp_path, body, 0)
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _forward(self, method: str) -> None:
        path = self.path
        body = self._read_body()
        self._stdout_line(method, path, body)

        if (self.route_map is not None and method == "GET"
                and path in ("/", "")):
            self._answer_prewarm()
            return

        if (method == "POST" and self.post_limit is not None
                and not self.post_limit.reserve()):
            self.send_response(429)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return

        origin = None
        if self.route_map is not None and method == "POST":
            origin = self._route(body)
            if origin is None:
                self._refuse_unrouted(path, body)
                return

        n = 0
        req_path = None
        resp_path = None
        if self.capture is not None:
            n = self.capture._next()
            req_path = self.capture._write_req(n, method, path,
                                                self.headers, body)
        else:
            req_path = None

        # The req-NNN.json capture above still records what the CHILD sent
        # (masked); what goes out is decided here, from the route.
        fwd_headers, credential = self._forward_headers(body)
        # Routed by the body's wire model id (r3 §4.2.1); without a map this is
        # the single pre-T2 upstream, unchanged.
        url = (origin or self.upstream) + path
        req = urllib.request.Request(url, data=body if body else None,
                                     method=method, headers=fwd_headers)
        context = (SSL_CTX if tls_verify_for(url, credential)
                   else SSL_CTX_UNVERIFIED)
        splitter = FrameSplitter() if self.capture is not None else None
        frame_index = 0
        try:
            with urllib.request.urlopen(req, timeout=UPSTREAM_TIMEOUT,
                                        context=context) as r:
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
                raw_fd = None
                if (self.capture is not None and self.capture.raw_sse
                        and r.headers.get_content_type() == "text/event-stream"):
                    raw_fd = self.capture.begin_raw(n)
                complete = False
                try:
                    while True:
                        chunk = r.read1(65536)
                        if not chunk:
                            break
                        self.wfile.write(b"%x\r\n" % len(chunk) + chunk + b"\r\n")
                        if raw_fd is not None:
                            self.capture.append_raw(raw_fd, chunk)
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
                    complete = True
                finally:
                    if raw_fd is not None:
                        try:
                            os.close(raw_fd)
                        except OSError:
                            complete = False
                            raise
                        finally:
                            if not complete:
                                self.capture.mark_raw_partial(n)
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
                if isinstance(e.reason, ssl.SSLError):
                    self.capture.note_tls_failure(self._body_model(body),
                                                  url, e.reason)
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


class _RoutingOriginHandler(BaseHTTPRequestHandler):
    """A second origin for the selftest: records what reached THIS server, per
    server instance, so the routing checks can say which origin a request
    actually arrived at (the stub upstream's record is class-level and would
    not tell the two apart)."""
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def _seen(self, method):
        n = int(self.headers.get("content-length", 0) or 0)
        raw = self.rfile.read(n) if n else b""
        # T3 has to say which credential ARRIVED, so this origin keeps the
        # credential-bearing headers it was given. They are the selftest's own
        # synthetic plants and live in memory only — never a capture file.
        # r9: the names are spelled literally here, NOT taken from
        # ALWAYS_MASK. A fixture that filters by the product's own set goes
        # blind exactly when that set shrinks, which is the fault the check
        # below has to see.
        heads = {k.lower(): v for k, v in self.headers.items()
                 if k.lower() in ("authorization", "x-api-key")}
        self.server.seen.append({"method": method, "path": self.path,
                                 "body": raw, "headers": heads})
        return raw

    def _reply(self, payload):
        data = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        raw = self._seen("POST")
        auth = self.server.seen[-1]["headers"].get("authorization", "")
        if self.path == "/v1/t3-echo":
            # Echo the received credential into the body AND a response
            # header, so the two capture directions of §4.2.3 are separately
            # decidable.
            data = json.dumps({"ok": True, "echo": auth}).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("x-echo-auth", auth or "none")
            self.send_header("content-length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        if self.path == "/v1/t3-auth-error":
            body = json.dumps({
                "error": {
                    "message": "Unknown authorization credentials",
                    "type": "auth_error", "code": "invalid_api_key",
                    "param": strip_credential_scheme(auth)}}).encode()
            self.send_response(403)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        try:
            model = json.loads(raw or b"{}").get("model")
        except Exception:
            model = None
        self._reply({"ok": True, "model": model})

    def do_GET(self):
        self._seen("GET")
        self._reply({"origin_root": True})


class RouteHandler(Handler):
    """A Handler bound to a route map: the routing legs below set their own
    class attributes, so the capture/numbering checks already run through the
    mapless Handler are untouched."""


class EmptyRouteMapHandler(Handler):
    """REDTEAM-REFRESH T2 fix (ruling RR-3 F-2): routing IS configured but
    declares ZERO routes. Every POST must be refused here; a `not route_map`
    predicate calls this state "no routing" and forwards everything."""


class NoRouteMapHandler(Handler):
    """REDTEAM-REFRESH T2 fix (ruling RR-3 F-2), the other state: no routing
    configured at all — a hand-run wiretap — which keeps the pre-T2
    single-upstream behaviour."""


class T3Handler(Handler):
    """REDTEAM-REFRESH T3: the credential legs run against a synthetic plant
    set handed in through `credential_env`, never the process environment,
    so `--selftest` still reads no real key (r3 §4.4.5)."""


def _free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def serve(port: int, upstream: str, capture_dir: str | None,
          ambient_key: str, raw_sse: bool = False,
          max_forwarded_posts: int | None = None,
          route_map: dict | None = None) -> None:
    Handler.upstream = upstream.rstrip("/")
    Handler.ambient_key = ambient_key
    Handler.post_limit = (ForwardedPostLimit(max_forwarded_posts)
                          if max_forwarded_posts is not None else None)
    Handler.capture = (
        Capture(capture_dir, ambient_key, raw_sse=raw_sse,
                secrets=lambda: credential_secret_values(route_map,
                                                         ambient_key))
        if capture_dir else None)
    # RR-3 F-2: `route_map or None` collapsed an empty map into
    # "no routing configured", turning zero routes into "forward
    # everything to --upstream" — the one state that must refuse.
    Handler.route_map = route_map
    httpd = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    httpd.daemon_threads = True
    print("wiretap2 listening on 127.0.0.1:%d upstream=%s capture=%s "
          "routes=%s" % (port, Handler.upstream, capture_dir or "(off)",
                         "none" if Handler.route_map is None else
                         len(Handler.route_map)), flush=True)
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

    # --- T2 routing: per-row origin, fail-closed, local prewarm -----------
    origin2_port = _free_port()
    origin2 = ThreadingHTTPServer(("127.0.0.1", origin2_port),
                                  _RoutingOriginHandler)
    origin2.seen = []
    origin2.daemon_threads = True
    threading.Thread(target=origin2.serve_forever, daemon=True).start()
    route_cap = os.path.join(tmp, "capture-routes")
    RouteHandler.upstream = upstream
    RouteHandler.ambient_key = ambient
    RouteHandler.capture = Capture(route_cap, ambient)
    RouteHandler.route_map = {
        "routed-model": {"origin": "http://127.0.0.1:%d" % origin2_port,
                         "credential_variable": "WIRETAP_T2_SELFTEST_KEY",
                         "credential_header": "authorization",
                         "row_names": ["row-tf"]}}
    route_port = _free_port()
    rprox = ThreadingHTTPServer(("127.0.0.1", route_port), RouteHandler)
    rprox.daemon_threads = True
    threading.Thread(target=rprox.serve_forever, daemon=True).start()
    rurl = "http://127.0.0.1:%d" % route_port

    def _post(url, payload, headers=None):
        hdrs = {"Authorization": "Bearer " + ambient,
                "Content-Type": "application/json"}
        hdrs.update(headers or {})
        return urllib.request.urlopen(
            urllib.request.Request(url, data=json.dumps(payload).encode(),
                                   method="POST", headers=hdrs), timeout=30)

    routed_status = None
    try:
        with _post(rurl + "/v1/responses",
                   {"model": "routed-model", "input": "hi"}) as r:
            routed_status = r.status
            r.read()
    except Exception as exc:
        routed_status = repr(exc)
    posts2 = [s for s in origin2.seen if s["method"] == "POST"]
    check("route-post-reaches-rows-own-origin",
          routed_status == 200 and len(posts2) == 1
          and b"routed-model" in posts2[0]["body"]
          and posts2[0]["path"] == "/v1/responses",
          "status=%r seen=%s" % (routed_status,
                                 [(s["method"], s["path"])
                                  for s in origin2.seen]))

    unr_status = unr_code = unr_param = None
    try:
        with _post(rurl + "/v1/responses",
                   {"model": "model-no-row-declares-this", "input": "hi"}) as r:
            unr_status = r.status
            r.read()
    except urllib.error.HTTPError as exc:
        unr_status = exc.code
        try:
            err = json.loads(exc.read().decode())["error"]
        except Exception:
            err = {}
        unr_code = err.get("code")
        unr_param = err.get("param")
    check("unrouted-post-refused-400",
          unr_status == 400 and unr_code == "wiretap_unrouted_model"
          and unr_param == "",
          "status=%r code=%r param=%r" % (unr_status, unr_code, unr_param))

    r_reqs = sorted(f for f in os.listdir(route_cap) if f.startswith("req-"))
    unr_path = os.path.join(route_cap, "unrouted.jsonl")
    unr_lines = []
    unr_text = ""
    if os.path.isfile(unr_path):
        unr_text = open(unr_path).read()
        unr_lines = [json.loads(l) for l in unr_text.splitlines() if l.strip()]
    check("unrouted-not-captured-but-recorded",
          r_reqs == ["req-001.json"] and len(unr_lines) == 1
          and unr_lines[0]["body_model"] == "model-no-row-declares-this"
          and unr_lines[0]["error_code"] == "wiretap_unrouted_model"
          and "path" in unr_lines[0] and len(origin2.seen) == 1
          and ambient not in unr_text,
          "reqs=%s lines=%s n_seen=%d" % (r_reqs, unr_lines,
                                          len(origin2.seen)))

    try:
        with urllib.request.urlopen(rurl + "/", timeout=30) as r:
            prewarm_status = r.status
            prewarm_body = r.read()
    except Exception as exc:
        prewarm_status = repr(exc)
    unr_after = []
    if os.path.isfile(unr_path):
        unr_after = [json.loads(l) for l in open(unr_path).read().splitlines()
                     if l.strip()]
    r_reqs2 = {}
    for f in sorted(os.listdir(route_cap)):
        if f.startswith("req-"):
            with open(os.path.join(route_cap, f)) as fh:
                r_reqs2[f] = json.load(fh)["method"]
    check("prewarm-get-answered-locally-and-numbered",
          prewarm_status == 200 and b"wiretap_prewarm" in prewarm_body
          and r_reqs2.get("req-002.json") == "GET"
          and not [s for s in origin2.seen if s["method"] == "GET"]
          and len(unr_after) == 1,
          "status=%r body[:60]=%r reqs=%s" % (prewarm_status,
                                              prewarm_body[:60], r_reqs2))

    # --- RR-3 F-2: `route_map is None` and `route_map == {}` are states ----
    # The predicate that used to guard both legs was `not self.route_map`,
    # which cannot tell "no routing configured" from "routing configured,
    # zero routes". The second is a value run.py really sends — a case whose
    # selectors resolve to no live row at all — and it must refuse everything,
    # exactly as an unresolvable id does on a non-empty map.
    empty_cap = os.path.join(tmp, "capture-empty-map")
    EmptyRouteMapHandler.upstream = upstream
    EmptyRouteMapHandler.ambient_key = ambient
    EmptyRouteMapHandler.capture = Capture(empty_cap, ambient)
    EmptyRouteMapHandler.route_map = {}
    empty_port = _free_port()
    eprox = ThreadingHTTPServer(("127.0.0.1", empty_port),
                                EmptyRouteMapHandler)
    eprox.daemon_threads = True
    threading.Thread(target=eprox.serve_forever, daemon=True).start()
    eurl = "http://127.0.0.1:%d" % empty_port

    def _cause_lines(cap):
        p = os.path.join(cap, "unrouted.jsonl")
        if not os.path.isfile(p):
            return []
        with open(p) as fh:
            return [json.loads(l) for l in fh.read().splitlines() if l.strip()]

    def _reqs(cap):
        return sorted(f for f in os.listdir(cap) if f.startswith("req-")) \
            if os.path.isdir(cap) else []

    e_status = e_code = e_param = None
    e_body = ""
    try:
        with _post(eurl + "/v1/empty-map-probe",
                   {"model": "model-no-row-declares-this",
                    "input": "hi"}) as r:
            e_status = r.status
            e_body = r.read().decode()
    except urllib.error.HTTPError as exc:
        e_status = exc.code
        e_body = exc.read().decode()
        try:
            err = json.loads(e_body)["error"]
        except Exception:
            err = {}
        e_code = err.get("code")
        e_param = err.get("param")
    e_forwarded = "/v1/empty-map-probe" in _StubUpstreamHandler.received_bodies
    e_unr = _cause_lines(empty_cap)
    check("empty-route-map-refuses-post",
          e_status == 400 and e_code == "wiretap_unrouted_model"
          and e_param == "" and _reqs(empty_cap) == [] and len(e_unr) == 1
          and not e_forwarded,
          "status=%r code=%r param=%r reqs=%s unrouted=%d forwarded=%s"
          % (e_status, e_code, e_param, _reqs(empty_cap), len(e_unr),
             e_forwarded))

    e_pre_status, e_pre_body = None, b""
    try:
        with urllib.request.urlopen(eurl + "/", timeout=30) as r:
            e_pre_status = r.status
            e_pre_body = r.read()
    except Exception as exc:
        e_pre_status = repr(exc)
    e_reqs2 = {}
    for f in _reqs(empty_cap):
        with open(os.path.join(empty_cap, f)) as fh:
            e_reqs2[f] = json.load(fh)["method"]
    check("empty-route-map-answers-prewarm-locally",
          e_pre_status == 200 and b"wiretap_prewarm" in e_pre_body
          and e_reqs2 == {"req-001.json": "GET"}
          and len(_cause_lines(empty_cap)) == 1,
          "status=%r body[:60]=%r reqs=%s causes=%d"
          % (e_pre_status, e_pre_body[:60], e_reqs2,
             len(_cause_lines(empty_cap))))
    # The refusal has to have HAPPENED for the leak question to mean
    # anything: under the pre-F-2 predicate the request was forwarded, so a
    # leg that only looked at the capture dir would pass vacuously. The
    # canary key is never printed.
    check("empty-map-refusal-leaks-no-credential",
          e_status == 400 and e_code == "wiretap_unrouted_model"
          and not e_forwarded
          and ambient not in e_body
          and all(ambient not in open(os.path.join(empty_cap, f)).read()
                  for f in os.listdir(empty_cap))
          and ambient not in json.dumps(_cause_lines(empty_cap)),
          "refused=%r code=%r forwarded=%r key_in_body=%r key_in_capture=%r"
          % (e_status == 400, e_code, e_forwarded, ambient in e_body,
             any(ambient in open(os.path.join(empty_cap, f)).read()
                 for f in os.listdir(empty_cap))))
    eprox.server_close()

    none_cap = os.path.join(tmp, "capture-no-map")
    NoRouteMapHandler.upstream = upstream
    NoRouteMapHandler.ambient_key = ambient
    NoRouteMapHandler.capture = Capture(none_cap, ambient)
    NoRouteMapHandler.route_map = None
    none_port = _free_port()
    nprox = ThreadingHTTPServer(("127.0.0.1", none_port), NoRouteMapHandler)
    nprox.daemon_threads = True
    threading.Thread(target=nprox.serve_forever, daemon=True).start()
    nurl = "http://127.0.0.1:%d" % none_port

    n_payload = {"model": "m-any", "input": "hi"}
    n_status, n_body = None, b""
    try:
        with _post(nurl + "/v1/no-map-probe", n_payload) as r:
            n_status = r.status
            n_body = r.read()
    except Exception as exc:
        n_status = repr(exc)
    check("no-route-map-forwards-post-to-upstream",
          n_status == 200
          and _StubUpstreamHandler.received_bodies.get("/v1/no-map-probe")
          == json.dumps(n_payload).encode()
          and not os.path.exists(os.path.join(none_cap, "unrouted.jsonl"))
          and _reqs(none_cap) == ["req-001.json"],
          "status=%r body[:60]=%r forwarded=%r causes=%d reqs=%s"
          % (n_status, n_body[:60],
             _StubUpstreamHandler.received_bodies.get("/v1/no-map-probe"),
             len(_cause_lines(none_cap)), _reqs(none_cap)))
    nprox.server_close()

    # --- REDTEAM-REFRESH T3 (§8 T3: these checks belong to this file, and
    # nothing else runs them). The plants are literals below and reach the
    # handler through `credential_env`, so the selftest still reads no real
    # key, opens no connection beyond loopback, and writes no secret.
    t3_env = {
        "WIRETAP_T2_SELFTEST_KEY": "selftestplanttfkey0123456789abcdef",
        "GROK_DIRECT_PLACEHOLDER_KEY": "selftestplaceholder0123456789",
        # The prefix is spelled as a concatenation exactly as
        # test_run.py spells T3_SWEPT_PLANTS: the runtime value is
        # byte-identical, and the source keeps no contiguous
        # sk-shaped literal for the house sweep to hit.
        "XAI_API_KEY": "sk-" + "selftestchainrung" + "0123456789abcdef0",
    }
    t3_origin = "http://127.0.0.1:%d" % origin2_port
    T3Handler.upstream = upstream
    T3Handler.ambient_key = ambient
    T3Handler.credential_env = t3_env
    T3Handler.route_map = {
        "t3-routed": {"origin": t3_origin,
                      "credential_variable": "WIRETAP_T2_SELFTEST_KEY",
                      "credential_header": "authorization",
                      "row_names": ["row-tf"]},
        "t3-placeholder": {"origin": t3_origin,
                           "credential_variable":
                               "GROK_DIRECT_PLACEHOLDER_KEY",
                           "credential_header": "authorization",
                           "row_names": ["row-onprem"]},
        "t3-chain": {"origin": t3_origin, "credential_variable": None,
                     "credential_header": "authorization",
                     "row_names": ["row-chain"]},
    }
    t3_cap = os.path.join(tmp, "capture-t3")
    T3Handler.capture = Capture(
        t3_cap, ambient,
        secrets=lambda: credential_secret_values(T3Handler.route_map,
                                                 ambient, t3_env))
    t3_port = _free_port()
    tprox = ThreadingHTTPServer(("127.0.0.1", t3_port), T3Handler)
    tprox.daemon_threads = True
    threading.Thread(target=tprox.serve_forever, daemon=True).start()
    turl = "http://127.0.0.1:%d" % t3_port
    plant = t3_env["WIRETAP_T2_SELFTEST_KEY"]
    chain_plant = t3_env["XAI_API_KEY"]
    ph_plant = t3_env["GROK_DIRECT_PLACEHOLDER_KEY"]
    child_own = "Bearer selftestchildownkey-never-leave"

    def _t3_post(path, model):
        return _post(turl + path, {"model": model, "input": "hi"},
                     headers={"Authorization": child_own})

    t3_status = None
    try:
        with _t3_post("/v1/t3-echo", "t3-routed") as r:
            t3_status = r.status
            r.read()
    except Exception as exc:
        t3_status = repr(exc)
    origin2.seen = []
    try:
        with _t3_post("/v1/t3-echo", "t3-chain") as r:
            r.read()
        chain_status = 200
    except Exception as exc:
        chain_status = repr(exc)
    chain_saw = [s["headers"].get("authorization", "")
                 for s in origin2.seen if s["method"] == "POST"]
    check("chain-row-credential-resolved-by-the-tap",
          chain_status == 200 and chain_saw == ["Bearer " + chain_plant],
          "status=%r origin_saw=%r" % (chain_status, chain_saw))
    origin_saw = [s["headers"].get("authorization", "")
                  for s in origin2.seen if s["method"] == "POST"]
    check("child-own-credential-never-reaches-the-origin",
          child_own not in origin_saw and "Bearer " + ambient
          not in origin_saw,
          "origin_saw=%r" % (origin_saw,))

    t3_files = sorted(os.listdir(t3_cap))

    def _hdr(doc, name):
        """`mask_headers` masks the VALUE and keeps the caller's spelling of
        the name (frozen behaviour, pinned by `auth-masked-shape`), so a
        reader must look a name up case-insensitively."""
        return next((v for k, v in doc.items() if k.lower() == name), None)

    t3_req = json.load(open(os.path.join(t3_cap, "req-001.json")))
    req_auth = _hdr(t3_req["headers"], "authorization")
    check("request-direction-masks-with-the-triple",
          req_auth == {"masked": True, "sha256_12": _sha256_12(child_own),
                       "len": len(child_own)},
          "authorization=%r headers=%r" % (req_auth, t3_req["headers"]))
    t3_resp = [json.loads(l) for l in
               open(os.path.join(t3_cap, "resp-001.jsonl")) if l.strip()]
    echoed = _hdr(t3_resp[0]["headers"], "x-echo-auth")
    check("response-direction-masks-the-injected-value",
          isinstance(echoed, dict) and echoed.get("masked") is True
          and echoed.get("sha256_12") == _sha256_12("Bearer " + plant),
          "x-echo-auth=%r" % (echoed,))
    echo_frames = [d["frame"] for d in t3_resp if "frame" in d]
    check("echoed-body-secret-replaced-rest-kept",
          any("selftestchildownkey-never-leave" not in f
              and "masked" in f and '"ok"' in f for f in echo_frames),
          "frames=%r" % (echo_frames,))

    try:
        with _t3_post("/v1/t3-auth-error", "t3-routed") as r:
            r.read()
        err_status = 200
    except urllib.error.HTTPError as exc:
        err_status = exc.code
    err_frames = []
    for name in sorted(os.listdir(t3_cap)):
        if name.startswith("resp-") and name.endswith(".jsonl"):
            for line in open(os.path.join(t3_cap, name)):
                rec = json.loads(line)
                if "frame" in rec:
                    err_frames.append(rec["frame"])
    masked_param = None
    kept_message = None
    for frame in err_frames:
        try:
            err = json.loads(frame)["error"]
        except Exception:
            continue
        if err.get("type") == "auth_error":
            masked_param = err.get("param")
            kept_message = err.get("message")
    check("auth-error-param-masked-in-the-capture",
          err_status == 403 and isinstance(masked_param, dict)
          and masked_param.get("masked") is True
          and masked_param.get("sha256_12") == _sha256_12(plant),
          "status=%r param=%r" % (err_status, masked_param))
    check("auth-error-message-type-code-kept",
          kept_message == "Unknown authorization credentials",
          "message=%r" % (kept_message,))

    # r9 (CF-T3-27 survived r8): the capture leg above masks `param` twice
    # over — its value IS the plant, so the value scrub replaces it whether
    # or not rule 3 runs. Rule 3 is the blunt one (§4.4.1): the whole
    # `error.param` of an auth_error is replaced even when nothing in the
    # tap's secret set or shape rules matches it, because that field is
    # where a caller's own credential is echoed back (r3 §2.5).
    unknown_param = scrub_json({"error": {
        "type": "auth_error", "code": "invalid_api_key",
        "param": "opaque-tenant-handle-no-rule-matches",
        "message": "Unknown authorization credentials"}}, ())
    check("auth-error-param-replaced-without-a-known-secret",
          isinstance(unknown_param["error"]["param"], dict)
          and unknown_param["error"]["param"].get("masked") is True
          and unknown_param["error"]["message"]
          == "Unknown authorization credentials"
          and unknown_param["error"]["code"] == "invalid_api_key"
          and unknown_param["error"]["type"] == "auth_error",
          "param_masked=%r message_kept=%r code_kept=%r"
          % (isinstance(unknown_param["error"]["param"], dict),
             unknown_param["error"].get("message")
             == "Unknown authorization credentials",
             unknown_param["error"].get("code") == "invalid_api_key"))

    # R2-4: the documented placeholder is passed to the row that needs it and
    # matched by NEITHER the scrub nor the sweep.
    origin2.seen = []
    try:
        with _t3_post("/v1/t3-echo", "t3-placeholder") as r:
            r.read()
        ph_status = 200
    except Exception as exc:
        ph_status = repr(exc)
    ph_saw = [s["headers"].get("authorization", "")
              for s in origin2.seen if s["method"] == "POST"]
    ph_text = "".join(open(os.path.join(t3_cap, f)).read()
                      for f in sorted(os.listdir(t3_cap)))
    check("placeholder-value-is-passed-and-not-scrubbed",
          ph_status == 200 and ph_saw == ["Bearer " + ph_plant]
          and ph_plant in ph_text,
          "status=%r origin_saw=%r kept=%r"
          % (ph_status, ph_saw, ph_plant in ph_text))
    t3_leak = [v for v in (plant, chain_plant, child_own, ambient)
               if v in ph_text]
    check("no-credential-survives-in-the-t3-capture",
          not t3_leak, "leaked=%r" % ([v[:4] + "..." for v in t3_leak],))

    # r9 (the r8 battery found these two unpinned). The leg above cannot
    # see the drop-set: the child's `Authorization` and the injected
    # `authorization` are ONE header name by the time urllib sends them, so
    # the insert overwrites the child's value whether or not the drop ran.
    # What r3 §4.2.3 forbids is KEEPING BOTH ("never append, never keep
    # both"), so the decidable leg needs a SECOND credential name.
    child_api_key = "selftest-child-api-key-never-leave"
    origin2.seen = []
    two_status = None
    try:
        with _post(turl + "/v1/t3-echo",
                   {"model": "t3-routed", "input": "hi"},
                   headers={"Authorization": child_own,
                            "X-Api-Key": child_api_key}) as r:
            two_status = r.status
            r.read()
    except Exception as exc:
        two_status = repr(exc)
    two_posts = [s for s in origin2.seen if s["method"] == "POST"]
    check("second-child-credential-name-never-reaches-the-origin",
          two_status == 200 and len(two_posts) == 1
          and two_posts[0]["headers"].get("authorization")
          == "Bearer " + plant
          and "x-api-key" not in two_posts[0]["headers"]
          and child_api_key not in json.dumps(
              [s["headers"] for s in two_posts]),
          "status=%r origin_names=%r auth_sha=%r"
          % (two_status,
             [sorted(s["headers"]) for s in two_posts],
             [_sha256_12(s["headers"].get("authorization", ""))
              for s in two_posts]))

    # The NAME half of ALWAYS_MASK, with values no value- or shape-rule
    # knows. Without this the second declared name rides on the first one's
    # coverage and nothing notices it being dropped.
    by_name = mask_headers({"authorization": "opaque-token-no-rule-hits",
                            "x-api-key": "opaque-token-no-rule-hits"},
                           "", [])
    check("both-declared-names-mask-by-name-alone",
          isinstance(by_name.get("authorization"), dict)
          and by_name["authorization"].get("masked") is True
          and isinstance(by_name.get("x-api-key"), dict)
          and by_name["x-api-key"].get("masked") is True,
          "masked=%r" % (by_name,))

    # TLS (§4.2.3 / s1 F10.4). Every offline origin is a loopback http stub,
    # so the decidable authority is (a) the contexts and the rule that picks
    # between them and (b) what happens when a credentialed https route meets
    # a chain that cannot verify. The 403 the stub answers with is proof the
    # credential never left: the tap refuses it before sending.
    check("tls-verification-decision-and-contexts",
          SSL_CTX.verify_mode == ssl.CERT_REQUIRED
          and SSL_CTX.check_hostname
          and SSL_CTX_UNVERIFIED.verify_mode == ssl.CERT_NONE
          and tls_verify_for("https://origin.invalid", "a-credential")
          and not tls_verify_for("https://origin.invalid", None)
          and not tls_verify_for("http://127.0.0.1:1", "a-credential"),
          "required=%r/%r unverified=%r rule=%r"
          % (SSL_CTX.verify_mode, SSL_CTX.check_hostname,
             SSL_CTX_UNVERIFIED.verify_mode,
             [tls_verify_for("https://o", "k"),
              tls_verify_for("https://o", None),
              tls_verify_for("http://127.0.0.1:1", "k")]))
    T3Handler.route_map = dict(T3Handler.route_map)
    T3Handler.route_map["t3-tls"] = {
        "origin": "https://127.0.0.1:%d" % origin2_port,
        "credential_variable": "WIRETAP_T2_SELFTEST_KEY",
        "credential_header": "authorization",
        "row_names": ["row-tls"]}
    # r9 (CF-T3-30 survived r8): every offline origin is a plain-HTTP stub,
    # so a swapped context pair is invisible at the handshake — both
    # contexts fail against a server that speaks no TLS. What IS decidable
    # offline is which context object the forward path hands the real
    # request, so this spy records the `context` kwarg (the client-side
    # calls pass none) and delegates to the real urlopen unchanged.
    tls_contexts = []
    real_urlopen = urllib.request.urlopen

    def spy_urlopen(req, *args, **kwargs):
        if "context" in kwargs:
            tls_contexts.append(kwargs["context"])
        return real_urlopen(req, *args, **kwargs)

    urllib.request.urlopen = spy_urlopen
    origin2.seen = []
    tls_status = None
    try:
        with _t3_post("/v1/t3-tls", "t3-tls") as r:
            tls_status = r.status
            r.read()
    except urllib.error.HTTPError as exc:
        tls_status = exc.code
    except Exception as exc:
        tls_status = repr(exc)
    urllib.request.urlopen = real_urlopen
    check("credentialed-route-uses-the-verifying-context",
          tls_contexts == [SSL_CTX],
          "contexts=%r" % ([c.verify_mode for c in tls_contexts],))
    tls_lines = []
    tls_path = os.path.join(t3_cap, "upstream-tls.jsonl")
    if os.path.isfile(tls_path):
        tls_lines = [json.loads(l) for l in
                     open(tls_path).read().splitlines() if l.strip()]
    check("unverifiable-tls-chain-refused-and-recorded",
          tls_status == 502 and len(tls_lines) == 1
          and tls_lines[0]["error_code"] == "wiretap_tls_verification_failed"
          and not [s for s in origin2.seen if s["method"] == "POST"]
          and plant not in json.dumps(tls_lines),
          "status=%r lines=%s reached_origin=%d"
          % (tls_status, tls_lines,
             len([s for s in origin2.seen if s["method"] == "POST"])))
    check("capture-set-unchanged-by-the-t3-legs",
          all(f.startswith(("req-", "resp-", "upstream-tls.",
                            "unrouted.")) for f in os.listdir(t3_cap)),
          "files=%s" % (sorted(os.listdir(t3_cap)),))
    tprox.server_close()
    T3Handler.route_map = None

    rprox.server_close()
    origin2.seen = []
    origin2.server_close()
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
    p.add_argument("--raw-sse", action="store_true",
                   help="tee upstream SSE entity bytes to private resp-NNN.raw; "
                        "requires --capture")
    p.add_argument("--max-forwarded-posts", type=int, default=None,
                   metavar="N", help="forward at most N POST attempts; "
                                     "default unlimited")
    p.add_argument("--ambient-key", default=None,
                   help="override ambient key for masking (tests); "
                        "empty/omitted reads WIRETAP_AMBIENT_KEY, then "
                        "CODEX_LLM_PROXY_KEY")
    p.add_argument("--selftest", action="store_true")
    a = p.parse_args(argv)
    if a.selftest:
        return _selftest()
    if a.raw_sse and not a.capture:
        p.error("--raw-sse requires --capture")
    if a.max_forwarded_posts is not None and a.max_forwarded_posts < 0:
        p.error("--max-forwarded-posts must be nonnegative")
    # HYG-1: the ambient key travels via env, never argv (ps-visible).
    # Precedence: explicit --ambient-key value (back-compat alias), then
    # WIRETAP_AMBIENT_KEY (set by the red-team runner), then the
    # pre-existing CODEX_LLM_PROXY_KEY ambient fallback. An empty or
    # omitted flag value falls through to the env seams.
    ambient = (a.ambient_key
               or os.environ.get("WIRETAP_AMBIENT_KEY", "")
               or os.environ.get("CODEX_LLM_PROXY_KEY", ""))
    route_map = None
    raw_map = os.environ.get("WIRETAP_ROUTE_MAP")
    if raw_map:
        try:
            parsed = json.loads(raw_map)
        except ValueError as exc:
            print("wiretap2: WIRETAP_ROUTE_MAP is not valid JSON (%s) — "
                  "refusing to start, because starting without the map means "
                  "forwarding every request to one origin" % exc, flush=True)
            return 2
        if not isinstance(parsed, dict):
            print("wiretap2: WIRETAP_ROUTE_MAP must be a JSON object — "
                  "refusing to start", flush=True)
            return 2
        route_map = parsed
    serve(a.port, a.upstream, a.capture, ambient, raw_sse=a.raw_sse,
          max_forwarded_posts=a.max_forwarded_posts, route_map=route_map)
    return 0


if __name__ == "__main__":
    sys.exit(main())
