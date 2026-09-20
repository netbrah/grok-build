#!/usr/bin/env python3
"""mxai_longprefix — stdlib-only MCP stdio TEST DOUBLE for MERGE-XAI-SYNC C-13
(06 §2 C-13, ADOPT 01 §3 S6: MCP long-prefix admission).

A fixture, NOT a harness extension: a throwaway server the operator provisions
in the LIVE config for the MXAI-C13 run (see META.json — config_patch is
scalar-only and cannot add [mcp_servers]). The entire point is the >256-char
qualified-name edge:

  tool name       = "longprefix_tool_" + "x" * 284   (300 chars, COMPUTED)
  qualified name  = "mxai_longprefix__" + <tool>     (317 chars > 256)

Protocol: MCP stdio transport — JSON-RPC 2.0, newline-delimited.
  initialize    -> echo protocolVersion + capabilities.tools + serverInfo
  tools/list    -> exactly ONE tool (the longprefix edge; inputSchema {echo:string})
  tools/call    -> echoes args["echo"] verbatim as a text content block
  notifications -> ignored (no response, per JSON-RPC)
  unknown req   -> -32601 Method not found

Stdlib only; no network; stdout is the protocol channel (diagnostics -> stderr).
"""
import json
import sys

TOOL_NAME = "longprefix_tool_" + "x" * 284  # 300 chars — the edge is computed, never hardcoded
SERVER_NAME = "mxai_longprefix"
FQ_NAME = SERVER_NAME + "__" + TOOL_NAME  # 317 chars — the double-underscore flat-MCP seam

# A rename upstream must not silently shrink the edge out of the >256 class.
assert len(TOOL_NAME) == 300, f"tool name drifted: {len(TOOL_NAME)}"
assert len(FQ_NAME) == 317 and len(FQ_NAME) > 256, f"FQ drifted: {len(FQ_NAME)}"


def handle(req):
    """Return the result payload, an error dict, or None (no response)."""
    method = req.get("method")
    rid = req.get("id")
    params = req.get("params") or {}

    if method == "initialize":
        return {
            "protocolVersion": params.get("protocolVersion", "2025-03-26"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": SERVER_NAME, "version": "0.0.1"},
        }
    if method == "tools/list":
        return {
            "tools": [
                {
                    "name": TOOL_NAME,
                    "description": (
                        "MERGE-XAI-SYNC C-13 test double: echoes the echo "
                        "argument verbatim."
                    ),
                    "inputSchema": {
                        "type": "object",
                        "properties": {"echo": {"type": "string"}},
                        "required": ["echo"],
                    },
                }
            ]
        }
    if method == "tools/call":
        if params.get("name") != TOOL_NAME:
            return {"__error": {"code": -32602, "message": "unknown tool"}}
        args = params.get("arguments") or {}
        return {"content": [{"type": "text", "text": args.get("echo", "")}]}

    # A request with an id that we do not implement -> -32601.
    if rid is not None:
        return {"__error": {"code": -32601, "message": "Method not found: %s" % method}}
    return None  # unknown notification -> no response


def main():
    out = sys.stdout
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError:
            reply = {"jsonrpc": "2.0", "id": None,
                     "error": {"code": -32700, "message": "Parse error"}}
            out.write(json.dumps(reply, separators=(",", ":")) + "\n")
            out.flush()
            continue
        result = handle(req)
        if result is None:
            continue
        if "__error" in result:
            reply = {"jsonrpc": "2.0", "id": req.get("id"), "error": result["__error"]}
        else:
            reply = {"jsonrpc": "2.0", "id": req.get("id"), "result": result}
        out.write(json.dumps(reply, separators=(",", ":")) + "\n")
        out.flush()


if __name__ == "__main__":
    main()
