# ACP

Another coding agent, driving as a model. An adapter that speaks the Agent
Client Protocol — Claude Code, Codex, Gemini, whatever ships next — is spawned
as a child process and answers `session/prompt` the way a provider answers a
request. What comes back is journaled as model events, so the transcript, every
surface and every replay read one record whoever produced it.

The agent brings its own hands: its own tools, its own login, its own
permission machinery. What bingo adds is a session to hang it on and, over the
bridge below, the tools this house has that it does not.

## Configuring an adapter

One row per agent under `acp.adapters`, by the name a person types. A new agent
is a new row, never a line of code — which is why no adapter is named in the
build:

```jsonc
{
  "acp": {
    "adapters": {
      "claude-acp": {
        "command": "npx",
        "args": ["-y", "@agentclientprotocol/claude-agent-acp"],
        "options": { "mode": "dontAsk" }
      },
      "codex-acp": {
        "command": "npx",
        "args": ["-y", "@agentclientprotocol/codex-acp"],
        "env": { "CODEX_APPROVAL_POLICY": "on-request" },
        "enabled": false
      }
    }
  }
}
```

- `command`, `args`, `env` — what to run. The environment is added to the one
  this process already has, because the adapter reads its own credentials from
  there.
- `options` — what to set on the agent every time a session with it opens, in
  **the agent's own ids and values**: one `session/set_config_option` each,
  sent before the first prompt. It is the door an adapter with no flag and no
  variable leaves open; bingo knows nothing about what is said through it and
  checks it only against what the agent declared.
- `enabled` — a row kept but not registered today.
- `tools` — an explicit list of what to offer over the bridge, replacing the
  derivation below entirely, exclusions included.
- `forwardMcp` — whether the MCP servers configured for bingo ride
  `session/new` so the agent dials them itself. On by default: one hop instead
  of two, and their tools then leave the bridge so nothing is served twice.

The row's name is an identity: it is what `--provider` and `/model` say, one
word, no `/`, and never `anthropic`, `codex`, `fake` or `openai`, which this
build already answers to. Signing in is the adapter's own (`claude login`,
`codex login`), and so is permission — say what the agent may do in *its* words,
on its row, because the row speaks first.

## Running a turn through one

`bingo --provider claude-acp --model agent`, or `/model claude-acp/agent` in a
session. `agent` is bingo's own label for "whatever model the agent would have
picked itself"; it is always valid and never crosses the wire. Any other model
name is one the agent declared, and `/model` and `/think` reach it as
`session/set_config_option` between turns — the effort ladder is matched
against the values the agent listed, never invented, so a level it does not
offer is one bingo does not send.

One ACP session per bingo session, kept on the agent's side. Reopening climbs:
`session/resume` where the agent can reattach without replaying, else
`session/load` whose replay is swallowed rather than journaled twice, else a
fresh session whose first prompt names a file holding the transcript so far.

The agent's own tool calls arrive as finished work, not as instructions: the
call, its status and its output ride the item's provider metadata and are
journaled whole, never executed here. A surface that wants tool rows for them
reads that metadata; the terminal does.

## The bridge

The agent has hands but no way to speak into this house, so bingo's shared
tools are served to it as an MCP server over a socket, one rendezvous per run
and one token per ACP session. What crosses is the turn's own tool list minus
two things: the machine's own hands, which the agent already brought
(`Read`, `Write`, `Edit`, `Glob`, `Grep`, `Bash`, `BashOutput`, `KillShell`,
`WebFetch`, `WebSearch`, `SpawnAgent`, `AskUserQuestion`), and the servers a
person's own rows hand it directly. A tool registered mid-session still
reaches it: the catalogue moving is `tools/list_changed`.

## What a bridge session cannot do

- **No compaction, and no side questions.** A request carrying an explicit
  purpose is refused: ACP delegates execution, so even a text-only summary
  might run tools on the other side. `/compact` on an ACP session fails, and so
  does any plugin's question asked beside the conversation.
- **No system prompt, no caching, no token counting.** Usage is whatever the
  adapter reports and honestly zero otherwise; the context window is the
  agent's business, not this one's. ACP's plans, modes and slash commands are
  unmapped, and `fs/*` and `terminal/*` are declared unsupported — the agent
  reaches the disk as itself.
- **Permission questions are the adapter's own.** One that asks anyway is put
  to whoever is at the session, in the agent's own words. Where nobody answers
  it fails closed with one of the reject options the agent itself offered, or
  is cancelled where it offered none, and a notice names the row to configure.
  A free-form `elicitation/create` is declined the same way.

Interrupting is `session/cancel`. A child that dies between turns is replaced
rather than asked, with a notice, and every child ends with the process that
spawned it.
