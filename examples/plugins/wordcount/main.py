#!/usr/bin/env python3
"""A bingo plugin in Python 3, standard library only: one tool, one command.

Speaks JSON-RPC 2.0, one message per line, on stdin and stdout. The whole
contract is `schema/plugin.json` at the root of this repository — every type
below is one of the kernel's own, so nothing here is a bingo-specific dialect.

stdout carries messages and nothing else. Anything else you would print goes to
stderr, which the host sends to `<data_dir>/logs/plugin-wordcount.log`.
"""

import json
import os
import sys

PROTOCOL = 3


def send(message):
    message["jsonrpc"] = "2.0"
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def answer(request_id, result):
    send({"id": request_id, "result": result})


def fail(request_id, message):
    send({"id": request_id, "error": {"code": -32000, "message": message}})


def notify(method, params):
    send({"method": method, "params": params})


def handshake():
    """What this plugin is, and everything it contributes."""
    return {
        "protocol": PROTOCOL,
        "name": "wordcount",
        "version": "0.1.0",
        "tools": [
            {
                "name": "count",
                "description": "Counts the words, lines and characters in a file.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "The file to count, relative to the working directory.",
                        }
                    },
                    "required": ["path"],
                },
            }
        ],
        "commands": [
            {
                "name": "wordcount",
                "hint": "count the words in a file",
                "args": {"kind": "free", "hint": "<path>"},
                "instant": True,
                "family": "plugin",
            }
        ],
    }


def counted(cwd, path):
    """The three counts, or an explanation of why there are none."""
    if not path:
        raise ValueError("a path to count is required")
    whole = path if os.path.isabs(path) else os.path.join(cwd, path)
    with open(whole, "r", encoding="utf-8", errors="replace") as handle:
        text = handle.read()
    return {
        "words": len(text.split()),
        "lines": len(text.splitlines()),
        "characters": len(text),
    }


def table(path, counts):
    """A `View` the terminal draws and `--print` folds to text."""
    return {
        "kind": "table",
        "headers": ["file", "words", "lines", "characters"],
        "rows": [
            [
                path,
                str(counts["words"]),
                str(counts["lines"]),
                str(counts["characters"]),
            ]
        ],
    }


def tool_call(request_id, params):
    """`count`: the numbers for the model, the table for the person."""
    path = params.get("input", {}).get("path", "")
    notify("tool/progress", {"callId": params["callId"], "tail": "reading %s" % path})
    try:
        counts = counted(params.get("cwd", "."), path)
    except (OSError, ValueError) as error:
        answer(
            request_id,
            {"output": {"parts": [{"type": "text", "text": str(error)}], "isError": True}},
        )
        return
    said = "%(words)d words, %(lines)d lines, %(characters)d characters" % counts
    answer(
        request_id,
        {
            "output": {
                "parts": [{"type": "text", "text": said}],
                "display": table(path, counts),
            }
        },
    )


def command_run(request_id, params):
    """`/wordcount <path>`: the same numbers, as a person asked for them."""
    path = params.get("args", "").strip()
    try:
        counts = counted(params.get("cwd", "."), path)
    except (OSError, ValueError) as error:
        fail(request_id, str(error))
        return
    answer(request_id, {"outcome": {"kind": "view", "view": table(path, counts)}})


def command_complete(request_id, params):
    """What could follow `/wordcount `: the files in the working directory."""
    partial = params.get("partial", "")
    try:
        names = sorted(os.listdir(params.get("cwd", ".")))
    except OSError:
        names = []
    found = [{"value": name} for name in names if name.startswith(partial)]
    answer(request_id, {"completions": found[:20]})


METHODS = {
    "tool/call": tool_call,
    "command/run": command_run,
    "command/complete": command_complete,
}


def serve(line):
    message = json.loads(line)
    request_id = message.get("id")
    method = message.get("method")
    params = message.get("params") or {}
    if request_id is None:
        return  # `tool/cancel`: this plugin's calls are too short to stop.
    if method == "initialize":
        # The handshake names this plugin's own major; a host that speaks
        # another one refuses it rather than guessing at what it means.
        answer(request_id, handshake())
        return
    handler = METHODS.get(method)
    if handler is None:
        send({"id": request_id, "error": {"code": -32601, "message": "no such method: %s" % method}})
        return
    handler(request_id, params)


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            serve(line)
        except Exception as error:  # a plugin that crashes takes the host's tool with it
            print("wordcount: %s" % error, file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
