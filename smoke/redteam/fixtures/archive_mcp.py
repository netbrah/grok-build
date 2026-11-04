#!/usr/bin/env python3
"""archive: minimal stdio MCP server for the apex-3v1s LHR/LNR redteam cases.

Fixture of bead apex-mwga.1 (KILO ECHO, redteam-refresh lane), authored by task T1 from the
contract in spec r2 section 8.1. Newline-delimited JSON-RPC 2.0 on stdio, stdlib only, no
environment read, no network, no filesystem write: every diagnostic goes to stderr and stdout is
the protocol channel alone.

Why a new fixture instead of an existing one, and what this file therefore is:

- the server is named `archive` so the harness's flat qualified name is exactly
  `archive__package_manifest`, which is the discriminator every LHR/LNR pin greps for;
- the payload carries a per-fixture result nonce, so a pin on it cannot be satisfied by
  echoing the prompt - and the payload is *minted here*, never copied out of `arguments`.
  The house anti-pattern this refuses is
  `smoke/redteam/fixtures/merge-xai/c-13-mcp/mcp-longprefix-stub.py`, whose `tools/call`
  returns the caller's own argument verbatim; an echoed nonce would make `answer_payload`,
  `dest_hist_nonce` and `fn_result_nonce` provable from the prompt alone;
- `inputSchema` requires nothing, so the model is never pushed to invent credential-shaped
  arguments for a read-only manifest read (spec 8.1's objection to `refprobe_mcp.py`);
- an unknown tool name is answered with a JSON-RPC error, never with an echo.

Protocol:
  initialize                 -> protocolVersion echo + capabilities.tools + serverInfo "archive"
  notifications/*            -> no response (JSON-RPC: a notification earns no reply)
  ping                       -> {}
  tools/list                 -> exactly ONE tool, `package_manifest`
  tools/call package_manifest -> text block whose JSON LEADS with the result marker
  unknown request with an id  -> -32601; unparseable line -> -32700

The payload is deterministic on purpose: the destination-turn answer is checked against a
constant, so `path`, `bytes` and `sha256` are derived from the fixed manifest bytes below and are
identical on every call and every run.
"""
import hashlib
import json
import sys

SERVER_NAME = "archive"
TOOL_NAME = "package_manifest"
# Case-owned literals (spec 4.5): the description marker sits only in the description, the
# result nonce only in the payload. Neither appears in any case prompt, so a pin on either can
# only be satisfied by a real discovery round trip.
DESCRIPTION_MARKER = "ARCHIVE-PM-PROBE-2e8d5f"
RESULT_NONCE = "ARCHIVE-PM-RSULT-4a7b91"

# The manifest this read-only tool "reads": the isolated probe-build pager crate's Cargo
# manifest, held in the fixture itself so no call touches the filesystem.
MANIFEST_PATH = "probe-build/pager/Cargo.toml"
MANIFEST_TEXT = (
    "# pinned probe-build pager manifest (archive fixture payload, apex-mwga.1 T1)\n"
    "[package]\n"
    'name = "pager"\n'
    'version = "0.3.1"\n'
    'edition = "2021"\n'
    "\n"
    "[[bin]]\n"
    'name = "pager"\n'
    'path = "src/main.rs"\n'
    "\n"
    "[dependencies]\n"
    'clap = "4"\n'
)
MANIFEST_BYTES = MANIFEST_TEXT.encode("utf-8")

TOOL = {
    "name": TOOL_NAME,
    "description": (
        "Read the isolated probe-build pager Cargo manifest from the archive and return its "
        "package manifest as JSON. Read-only: it takes no credentials and changes nothing. "
        "Test fixture marker " + DESCRIPTION_MARKER + "."
    ),
    "inputSchema": {
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": ("Optional archive-relative manifest path; defaults to "
                                + MANIFEST_PATH + "."),
            }
        },
        "required": [],
    },
}


def log(message):
    sys.stderr.write("archive-mcp: %s\n" % message)
    sys.stderr.flush()


def reply(msg_id, result=None, error=None):
    out = {"jsonrpc": "2.0", "id": msg_id}
    if error is not None:
        out["error"] = error
    else:
        out["result"] = result
    sys.stdout.write(json.dumps(out) + "\n")
    sys.stdout.flush()


def payload():
    """The fixed manifest read, as a dict whose first key is the result nonce.

    `marker` is first so the nonce sits at the head of the text block, inside the pair window
    the case pins use (spec 8.1). `bytes` and `sha256` are computed from MANIFEST_BYTES, so the
    three fields agree with each other and never move between calls.
    """
    return {
        "marker": RESULT_NONCE,
        "server": SERVER_NAME,
        "tool": TOOL_NAME,
        "path": MANIFEST_PATH,
        "bytes": len(MANIFEST_BYTES),
        "sha256": hashlib.sha256(MANIFEST_BYTES).hexdigest(),
        "lines": MANIFEST_TEXT.count("\n"),
    }


def handle(msg):
    method = msg.get("method")
    msg_id = msg.get("id")
    if msg_id is None:
        return  # a notification (e.g. notifications/initialized) earns no response
    params = msg.get("params") or {}
    if method == "initialize":
        version = params.get("protocolVersion") or "2025-06-18"
        reply(msg_id, {
            "protocolVersion": version,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": SERVER_NAME, "version": "1"},
        })
    elif method == "ping":
        reply(msg_id, {})
    elif method == "tools/list":
        reply(msg_id, {"tools": [TOOL]})
    elif method == "tools/call":
        name = params.get("name")
        if name != TOOL_NAME:
            # Fail loudly: an unknown tool is a protocol error, never an echo of the caller's
            # arguments (spec 8.1's anti-pattern).
            reply(msg_id, error={"code": -32602, "message": "unknown tool: %s" % (name,)})
            return
        reply(msg_id, {"content": [{"type": "text", "text": json.dumps(payload())}],
                       "isError": False})
    else:
        reply(msg_id, error={"code": -32601, "message": "method not found: %s" % method})


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except ValueError:
            reply(None, error={"code": -32700, "message": "Parse error"})
            log("ignored an unparseable request line")
            continue
        if not isinstance(msg, dict):
            reply(None, error={"code": -32600, "message": "invalid request: not an object"})
            continue
        handle(msg)


if __name__ == "__main__":
    main()
