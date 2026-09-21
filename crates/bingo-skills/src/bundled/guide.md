---
name: guide
description: >-
  The map of bingo itself: what it is, running it, sessions, the commands the
  kernel and the surface own, the settings layers and keys, and where files
  live. Read it before any question about bingo; it ends with one line per
  plugin page.
---

# bingo

A local coding-agent harness: a minimal kernel, and one ordered event stream
every surface reads as a client. The kernel owns the session actor, the
journal, the turn state machine, the permission gate and the plugin host;
providers, tools, the permission policy, hooks, skills, MCP servers, session
storage and every surface are plugins behind `bingo-sdk` traits, and no surface
holds session state. This page is the map; what a plugin owns is on its page.

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
- `/think minimal|low|medium|high|xhigh|max|off` — saved effort for any model;
  `off` omits the parameter, leaving the server's default in place.
- `/compact [instructions]` — summarise the conversation so far and keep going.
- `/permission [mode]` — read or set this session's permission mode.
- `/plugins [enable|disable <name>]`, alias `/modules` — a table of every
  plugin: version, state, why one is off, where it came from. A switch writes
  `enabledPlugins` in the user layer and is in force at the next start, never
  mid-session; `bingo plugins list|enable|disable` says the same from a shell.
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

TOML, merged from four layers, lowest first: `~/.bingo/settings.toml`,
`<cwd>/.bingo/settings.toml`, `<cwd>/.bingo/settings.local.toml`, then
`--settings <file>` and the flags above. A layer that still holds only a
`settings.json` is read as it always was and converted at the next start: the
TOML is written beside it, the JSON is kept as `settings.json.bak`, and a
notice names both. TOML has no `null`, so only a JSON layer can clear what a
layer below it set. An unknown key is reported at startup rather than ignored.
The kernel owns seven top-level keys — `provider`, `model`, `thinking`,
`maxTokens`, `models` (per-model facts, keyed `<provider>/<model>`), `pictures`
(`pictures.cacheDays`) and `enabledPlugins`; every other belongs to the plugin
that claims it, and is named on that plugin's page.

## Where things live

- `~/.bingo/` — `settings.toml`, `skills/`, `data/` (sessions, logs, history).
- `<project>/.bingo/` — `settings.toml`, `settings.local.toml`, `skills/`:
  above the person's for settings, below them for skills.
- `AGENTS.md`, or `CLAUDE.md` where there is none, in each directory from the
  project root down to the working one: instructions the model is given, the
  nearest last.
