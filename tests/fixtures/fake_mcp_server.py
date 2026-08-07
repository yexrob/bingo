#!/usr/bin/env python3
"""Fake MCP stdio server for integration tests: initialize / tools/list / tools/call."""
import json
import sys


def respond(msg_id, result=None, error=None):
    resp = {"jsonrpc": "2.0", "id": msg_id}
    if error is not None:
        resp["error"] = error
    else:
        resp["result"] = result
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()


def main():
    for line in sys.stdin:
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        method = msg.get("method")
        if method == "initialize":
            respond(msg["id"], {
                "protocolVersion": msg["params"].get("protocolVersion", "2026-07-28"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fake-mcp", "version": "0.1.0"},
            })
        elif method == "notifications/initialized":
            pass
        elif method == "tools/list":
            respond(msg["id"], {"tools": [{
                "name": "echo",
                "description": "Echo the given text back",
                "inputSchema": {
                    "type": "object",
                    "properties": {"text": {"type": "string"}},
                    "required": ["text"],
                },
            }]})
        elif method == "tools/call":
            params = msg["params"]
            args = params.get("arguments", {})
            text = args.get("text", "")
            respond(msg["id"], {
                "content": [{"type": "text", "text": f"echo: {text}"}],
                "isError": False,
            })
        else:
            respond(msg["id"], error={"code": -32601, "message": f"method not found: {method}"})


if __name__ == "__main__":
    main()
