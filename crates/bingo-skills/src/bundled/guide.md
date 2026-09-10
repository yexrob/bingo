---
name: guide
description: >-
  The map of bingo itself: what it is, running it, sessions, the commands the
  kernel and the surface own, the settings layers and keys, and where files
  live. Read it before any question about bingo; it ends with one line per
  plugin page.
---

# bingo

A local coding-agent harness: a minimal kernel with everything else a plugin,
and one ordered event stream every surface reads as a client. The kernel owns
the session actor, the journal, the turn state machine, the permission gate and
the plugin host. Providers, tools, the permission policy, hooks, skills, MCP
servers, session storage and every surface are plugin crates behind traits in
`bingo-sdk`.

One consequence is worth knowing: a surface holds no session state. The TUI,
`--print`, the JSON-RPC server and any other client fold the same frames and
derive what they draw at render time. What one client sees, another can see.

This page is the map, not the manual: what a plugin owns is on its own page.

## Running it

- `bingo` — the terminal UI, when stdin and stdout are both a terminal.
- `bingo "prompt"` or `bingo --print "prompt"` — one turn, headless, then exit.
  `--output-format text|json|stream-json` says what reaches stdout;
  `--input-format stream-json` reads a turn per line from stdin.
- `bingo serve --stdio` — JSON-RPC over stdin and stdout, a message per line,
  for a host that drives sessions itself.

Flags that apply anywhere: `--provider`, `--model`, `--cwd`, `--settings
<file>`, `--permission-mode <mode>`, `--allowed-tools <rule,rule>`,
`--max-turns <n>`, `--dangerously-skip-permissions`.

## Sessions

A session is the only conversational noun: a sub-agent is a session with a
parent, a room is a session with no model, and all render through the same
reducer. Each lives on disk as a journal, so reopening one replays what
happened rather than a summary of it. `--continue` reopens the most recent here,
`--resume <id>` one by id, `--session-id <key>` names one for a host that routes
by key; in the TUI `/clear` starts a fresh one and `/resume` picks from a list.

## Commands

A line starting with `/` is a command, a line starting with `!` a shell line;
the session actor parses both, and no surface parses a command it does not own.
These are the kernel's and the surface's — every other is a plugin's.

- `/model [<provider>/]<model>` — what the next turn runs on. There is no
  `/provider`: `anthropic/claude-x` names both.
- `/think minimal|low|medium|high|xhigh|max|off` — reasoning effort.
- `/compact [instructions]` — summarise the conversation so far and keep going.
- `/permission [mode]` — read or set this session's permission mode.
- `/status` — cwd, provider and model, mode, context against the window, tokens.
- `/login <provider> [browser|device|paste]`, `/logout <provider>` — the
  credential of a provider that signs in rather than take a key; `bingo login
  <provider>` is the same from a shell. Tokens live in `~/.bingo/data/`.
- `/help`, `/clear`, `/resume`, `/exit` — the surface's own, never the kernel's.
- `!<line>` — a shell line in the session's directory, now, with the person's
  own privileges and no gate: they typed it. Its output is recorded and read.

An instant command (`/permission`, `!`) runs while a turn is busy; anything
else queues behind it, a skill's `/name` included, because that is a prompt.

## Settings

JSONC — comments and trailing commas are fine — merged from four layers, lowest
first: `~/.bingo/settings.json`, `<cwd>/.bingo/settings.json`,
`<cwd>/.bingo/settings.local.json`, then `--settings <file>` and the flags
above. An unknown key is reported at startup rather than ignored, and an
explicit `null` in a higher layer clears what the layers below it set. The
kernel owns six top-level keys — `provider`, `model`, `thinking`,
`maxTokens`, `models` (per-model facts, keyed `<provider>/<model>`) and
`pictures` (`pictures.cacheDays`); every other belongs to the plugin that
claims it, and is named on that plugin's page.

## Where things live

- `~/.bingo/` — `settings.json`, `skills/`, `data/` (sessions, logs, history).
- `<project>/.bingo/` — `settings.json`, `settings.local.json`, `skills/`:
  above the person's for settings, below them for skills.
- `AGENTS.md`, or `CLAUDE.md` where there is none, in each directory from the
  project root down to the working one: instructions the model is given, the
  nearest last.
