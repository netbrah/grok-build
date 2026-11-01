#!/usr/bin/env python3
"""refprobe: minimal stdio MCP server for the GEM-MCP-REF-01 redteam case.

Exposes one tool whose inputSchema uses `$defs` plus `"$ref": "#/$defs/cluster_ref"`,
the same shape as the ontap-cluster MCP server's ssh_execute/cli_execute/zapi_call
tools that produced the Vertex 400 on gemini-3.8-flash-cyber (SCS session
01a179e9-392e-7093-a395-21692e2e9399). Newline-delimited JSON-RPC 2.0 on stdio,
stdlib only, no network, no side effects.
"""
import json
import sys

TOOL = {
    "name": "cluster_probe",
    "description": "Probe a cluster by alias. Test fixture for schema $ref handling; returns a fixed string.",
    "inputSchema": {
        "$defs": {
            "cluster_ref": {
                "description": 'Inline creds {host,username,password,...} or alias string (e.g. "vsim").'
            }
        },
        "properties": {
            "cluster": {"$ref": "#/$defs/cluster_ref"},
            "command": {"type": "string"},
        },
        "required": ["cluster", "command"],
        "type": "object",
    },
}


def reply(msg_id, result=None, error=None):
    out = {"jsonrpc": "2.0", "id": msg_id}
    if error is not None:
        out["error"] = error
    else:
        out["result"] = result
    sys.stdout.write(json.dumps(out) + "\n")
    sys.stdout.flush()


def handle(msg):
    method = msg.get("method")
    msg_id = msg.get("id")
    if msg_id is None:
        return  # notification (e.g. notifications/initialized)
    if method == "initialize":
        version = (msg.get("params") or {}).get("protocolVersion") or "2025-06-18"
        reply(msg_id, {
            "protocolVersion": version,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "refprobe", "version": "1"},
        })
    elif method == "ping":
        reply(msg_id, {})
    elif method == "tools/list":
        reply(msg_id, {"tools": [TOOL]})
    elif method == "tools/call":
        args = (msg.get("params") or {}).get("arguments") or {}
        text = "probe ok: cluster=%s command=%s" % (args.get("cluster"), args.get("command"))
        reply(msg_id, {"content": [{"type": "text", "text": text}], "isError": False})
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
            continue
        handle(msg)


if __name__ == "__main__":
    main()
