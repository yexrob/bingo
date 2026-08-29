---
name: guide
description: >-
  How bingo itself works: its commands, sessions, permission modes,
  settings, hooks, skills, MCP servers, and where its files live. Read it
  before answering any question about bingo, a `/` command, a permission
  prompt or configuration.
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

## Running it

- `bingo` — the terminal UI, when stdin and stdout are both a terminal.
- `bingo "prompt"` or `bingo --print "prompt"` — one turn, headless, then exit.
  `--output-format text|json|stream-json` says what reaches stdout;
  `--input-format stream-json` reads a turn per line from stdin.
- `bingo serve --stdio` — JSON-RPC over stdin and stdout, one message per line,
  for a host that drives sessions itself.

Flags that apply anywhere: `--provider`, `--model`, `--cwd`, `--settings
<file>`, `--permission-mode <mode>`, `--allowed-tools <rule,rule>`,
`--max-turns <n>`, `--dangerously-skip-permissions`.

## Sessions

A session is the only conversational noun. A sub-agent is a session with a
parent; a room is a session with no model. All of them render through the same
reducer.

- `--continue` reopens the most recent session in this directory.
- `--resume <id>` reopens one by id.
- `--session-id <key>` names a session for a host that routes by key.
- In the TUI, `/clear` starts a fresh one and `/resume` picks from the list.

Sessions live on disk as a journal per session, so reopening one replays what
happened rather than a summary of it.

## Commands

A line starting with `/` is a command; a line starting with `!` is a shell
line. The session actor parses both — no surface parses a command it does not
own.

- `/model [<provider>/]<model>` — what the next turn runs on. There is no
  `/provider`: `anthropic/claude-x` names both.
- `/think minimal|low|medium|high|xhigh|max|off` — reasoning effort.
- `/compact [instructions]` — summarise the conversation so far and keep going.
  It waits for a running turn.
- `/permission [mode]` — read or set this session's permission mode.
- `/help`, `/clear`, `/resume`, `/exit` — the surface's own; they never reach
  the kernel.
- `!<line>` — run a shell line under the session's directory, now, with the
  person's own privileges. It is not gated: a person at the keyboard typed it.
  What it printed is recorded and the model sees it.

An instant command (`/permission`, `!`) runs even while a turn is busy;
anything else queues behind the running turn. A skill's `/name` is a prompt, so
it queues.

## Permissions

Every tool call passes one gate. Rules decide first; the mode decides what
happens when no rule does.

The five modes:

- `default` — trusted read-only tools run; everything else asks.
- `acceptEdits` — edits inside the working directories run without a prompt.
- `plan` — nothing that is not read-only runs at all.
- `bypassPermissions` — everything runs except what only a person may decide.
- `dontAsk` — nobody is there to answer, so what would have asked is denied.

Rules live under `permissions` in the settings, as `allow`, `deny` and `ask`
lists. One rule per line:

- `Bash` or `Bash(*)` — every call of that tool.
- `Bash(git status:*)` — a prefix of the command.
- `Edit(/src/**)` — a path glob, where `*` stops at a separator.
- `WebFetch(domain:example.com)` — the URL host, exactly.
- `mcp__server` or `mcp__server__tool` — a whole MCP server, or one of its
  tools.

Deny and ask read a rule the broad way, allow the narrow way: each takes the
reading that fails closed. A deny rule stands above every mode, and so does a
tool's own confirmation. When a prompt appears, answering "allow for session"
adds the rule to this session only — it is never written back to a file.

Tool properties fail closed: a tool nobody has described is not
concurrency-safe, not read-only, and blocks on interrupt. An MCP tool's
`readOnlyHint` is a claim, not a fact, so the gate asks anyway.

## Settings

JSONC — comments and trailing commas are fine — merged from four layers,
lowest first:

1. `~/.bingo/settings.json`
2. `<cwd>/.bingo/settings.json`
3. `<cwd>/.bingo/settings.local.json`
4. `--settings <file>`, then the command-line flags above.

The kernel owns `provider`, `model`, `thinking` and `maxTokens`. Every other
top-level key belongs to the plugin that claims it: `permissions`, `context`,
`anthropic`, `openai`, `web`, `hooks`, `mcpServers`. An unknown key is
reported at startup rather than ignored in silence, and an explicit `null` in a
higher layer clears what the layers below it set.

## Hooks

Shell commands the harness runs at fixed points, configured under `hooks` as
`event → [{matcher, hooks: [{type: "command", command, timeout}]}]`. The events
are `PreToolUse`, `PostToolUse`, `UserPromptSubmit`, `Stop`, `PreCompact`,
`SessionStart`, `SessionEnd`, `Notification` and `PermissionRequest`. The
matcher is a regex over the tool name; an empty one matches every tool.

Each hook is handed a JSON object on stdin (`hook_event_name`, `session_id`,
`cwd`, and the point's own fields) and answers on stdout. A `PreToolUse` hook
may allow, deny or ask; exit code 2 denies with what it wrote to stderr; any
other non-zero exit warns and the turn goes on.

## Skills

A skill is a `SKILL.md` file: YAML frontmatter and a markdown body that becomes
a prompt. Each one is three things at once — a `/name` command, a name the
`Skill` tool can be called with, and a line in the system prompt saying it
exists.

They are read from, in order of precedence:

1. `~/.bingo/skills/<name>/SKILL.md`
2. `.bingo/skills/<name>/SKILL.md`, from the working directory upwards; the
   nearest wins
3. the guide you are reading, which any skill of the same name overrides

Frontmatter: `name` (the directory name when absent), `description` (the body's
first line when absent), `argument-hint`, `arguments` (names for the positional
arguments), `allowed-tools` and `model` (recorded, not yet enforced).

In the body, `$ARGUMENTS` is everything typed after the name, `$1`…`$9` are its
whitespace-separated words, a name declared in `arguments` is the word at its
position, and `${BINGO_SKILL_DIR}` is the skill's own directory. An edited
`SKILL.md` is picked up on the next look; no restart is needed.

## MCP

Servers configured under `mcpServers` — `{command, args, env, cwd}` for a
stdio server, `{type: "http", url, headers}` for an HTTP one — are dialled in
the background at startup, so a slow server never delays the first prompt.
`disabledMcpServers` turns one off by name, and `--mcp-config <path>` adds a
file's worth of servers for one run.

Their tools arrive as `mcp__<server>__<tool>` and are untrusted by
construction, so the gate asks about them. A stdio server's stderr goes to a
log file, never to the screen. `/mcp` says what is connected, and `/mcp
reconnect|enable|disable <server>` changes it.

## Where things live

- `~/.bingo/settings.json` — the person's settings.
- `~/.bingo/skills/` — their skills.
- `~/.bingo/data/` — sessions, logs, prompt history.
- `<project>/.bingo/settings.json`, `settings.local.json`, `skills/` — the
  project's, above the person's for settings and below them for skills.
- `AGENTS.md`, or `CLAUDE.md` when there is no `AGENTS.md`, in each directory
  from the project root down to the working one: instructions the model is
  given, nearest last.
