# MCP

Model Context Protocol servers give a session tools it did not ship with.
Configured servers are dialled in the background at startup, so a slow one
never delays the first prompt: a turn's tool set is whatever had landed when
that turn began, and a server that arrives later is there for the next one.

## Configuring a server

Under `mcpServers`, one entry per name. A child process:

```jsonc
{
  "mcpServers": {
    "files": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/work"],
      "env": { "TOKEN": "…" },
      "cwd": "/work"
    }
  }
}
```

A streamable-HTTP endpoint:

```jsonc
{
  "mcpServers": {
    "docs": {
      "type": "http",
      "url": "https://example.com/mcp",
      "headers": { "Authorization": "Bearer …" },
      "oauth": { "clientId": "…" }
    }
  }
}
```

`type` defaults to `stdio`. A field this plugin does not know, or one that
belongs to the other transport, is a startup failure: a server that silently
never dials is worse than one that says why.

- `disabledMcpServers` — names that start disabled and are never dialled.
- `--mcp-config <path>` — a file's `mcpServers`, and nothing else in it, added
  as a settings layer for one run.

## The tools

A connected server's tools arrive as `mcp__<server>__<tool>` — the name the
permission grammar reads, so `mcp__files` covers a whole server and
`mcp__files__read_file` one tool of it.

They are untrusted by construction: an MCP tool's traits are the fail-closed
default, its `readOnlyHint` is a claim by the thing being gated, and the gate
asks about every call. A stdio server's stderr goes to
`<data_dir>/logs/mcp-<server>.log`, never to the screen. A server may also ask
the person a question of its own (elicitation); it arrives as a card naming the
server, and anything nested is declined rather than half-answered.

## `/mcp`

`/mcp` alone is a table: server, status, tools, auth. The status is
`connecting`, `connected`, `needs authentication`, `failed: <why>` or
`disabled`; the auth column is `-`, `signed in` or `expired`.

Each verb takes exactly one server:

- `/mcp tools <server>` — what a connected server offers, with descriptions.
- `/mcp reconnect <server>` — dial it again.
- `/mcp enable <server>`, `/mcp disable <server>` — for this run.
- `/mcp login <server>` — sign in (below). It holds the queue while it runs.
- `/mcp logout <server>` — revoke the sign-in, forget it, and dial again.

A verb that only starts something answers the moment it has started it: a
handshake takes seconds, and a command that waited for one would hang.

## Signing in

An HTTP server with no static `Authorization` header that answers `401` is
`needs authentication` rather than failed. `/mcp login <server>` runs the OAuth
flow through the session's own dialog — discovery, registration as a native
client, PKCE. The tokens live in `~/.bingo/data/auth.json` under `mcp:<server>`
and never in a settings file; `oauth.clientId` is for a server whose
authorization server registers no clients of its own.

From a shell, `bingo mcp list | get <name> | add <name> … | remove <name> |
login <name> [--paste] | logout <name>` is the headless twin: `add` and
`remove` write the user settings layer, `get` prints header and environment
*names* and never their values, and `--paste` reads the redirect back from the
keyboard instead of opening a browser. `bingo mcp login` is also the way in
from `--print`, which renders no sign-in dialog.
