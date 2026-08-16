# bingo

> **Chinese version**: [README.zh-CN.md](README.zh-CN.md)

bingo is a local agent CLI (agent harness) written in Rust. It drives large
language models from your terminal to complete coding and system tasks: tool
calls, permission approval, sub-agent orchestration, task tracking, context
compaction, memory, and MCP extensions — all running locally. The model only
produces intent; side effects are gated by the harness.

## Highlights

- **Streaming main loop**: streaming Messages API responses; tool calls go
  through the permission gate, execute, and feed results back. Multiple
  concurrency-safe tools run in parallel within a single turn.
- **Unified permission gate**: five permission modes × rule tables
  (allow/deny/ask) → allow / deny / ask.
- **Tool set**: Bash, Read/Glob/Grep, Edit/Write, WebFetch/WebSearch, the Task
  family, AskUserQuestion, Skill, Agent (sub-agents), and MCP tools — all
  behind the same `Tool` trait.
- **Sub-agents (hub-and-spoke)**: the main agent spawns named sub-agents that
  run asynchronously; completion notifications are injected into context
  automatically. `SendMessage` continues a sub-agent, `AgentControl` manages
  its lifecycle.
- **Agent teams (project-scoped)**: a `.bingo/team.json` fixes a roster of
  roles to a project; the team is pulled up automatically at startup (members
  idle at zero token cost) and managed via `/team`; cross-session memory is
  scoped by project path + git branch. Where a crew is pinned it is the default
  workforce — work routes to a member, and a subagent spawned beside it is a
  temporary hire that never joins the roster. `.bingo/team-norms.md` is the
  crew's working agreement, carried by every member.
- **Experience library**: agents accumulate reusable operational experience
  per project (trigger/summary/steps/verify), share it across sessions, and
  record verified helpful/harmful outcomes without automatic self-promotion.
- **TUI**: ratatui dual-mode (default fullscreen alternate-screen canvas;
  `--inline` keeps finalized output in terminal scrollback and enables
  kitty-graphics image rendering), reverse history search, and a slash-command
  menu.
- **Skills**: drop-in `SKILL.md` (YAML frontmatter + markdown); bundled `guide`
  skill plus user/project skill directories.
- **MCP**: stdio and streamable HTTP servers, adapted to the same Tool trait.
- **Context management**: token budget monitoring, automatic compaction
  (summary of old messages + keep recent), manual `/compact`, and a fuse after
  repeated compaction failures.
- **Sessions & memory**: JSONL transcript persistence (`--continue`/`/resume`
  recovery), bounded 30-day/latest-100 retention with a 24-hour activity
  grace (`/gc`), memdir auto-memory, plus CLAUDE.md/AGENTS.md project memory.
- **Hooks extension points**: shell hooks for pre/post-tool, session
  start/end, compaction, Stop, and task lifecycle events (JSON on stdin,
  decisions returned on stdout).

## Building & installing

Requirements: Rust 2024 edition (stable toolchain, e.g. via `rustup`).

### Install directly from GitHub (cargo install)

```bash
cargo install --git https://github.com/yexrob/bingo --locked
```

- Installs to `~/.cargo/bin/bingo` (make sure `~/.cargo/bin` is on your `PATH`).
- `--locked` uses the committed `Cargo.lock` so dependency versions are
  reproducible.
- `rsmarkdown-core` is a git dependency; cargo fetches it automatically.

Update to the latest version:

```bash
cargo install --git https://github.com/yexrob/bingo --locked --force
```

### Official binaries (GitHub Releases)

Every version tag publishes prebuilt binaries (ZIP on Windows, tarballs on
macOS/Linux, each with a `checksums.txt` of SHA-256 values):

| Platform | File |
|---|---|
| Windows x86_64 | `bingo-x86_64-pc-windows-msvc.zip` (contains `bingo.exe`) |
| macOS (Apple Silicon) | `bingo-aarch64-apple-darwin.tar.gz` |
| macOS (Intel) | `bingo-x86_64-apple-darwin.tar.gz` |
| Linux x86_64 | `bingo-x86_64-unknown-linux-gnu.tar.gz` |

No WSL or Rust toolchain is needed — download, unpack, and run `bingo` /
`bingo.exe` directly. The Windows build runs on the native
`x86_64-pc-windows-msvc` target with PowerShell as the default shell (see
`shell` below).

### Build from source

```bash
cargo build --release          # binary at target/release/bingo
cargo install --path .         # or install to ~/.cargo/bin
```

Verify:

```bash
cargo test          # unit tests
cargo clippy -- -D warnings   # lint must pass with zero warnings
```

## Quick start

1. **Configure an API key** (either):
   - Environment variable: `export ANTHROPIC_API_KEY=sk-ant-...`
     (or DeepSeek: `DEEPSEEK_API_KEY`);
   - Settings file: `~/.config/bingo/settings.json` with `{"apiKey": "..."}`
     (settings take precedence over environment variables).
   - Custom endpoints via `ANTHROPIC_BASE_URL` or the `apiBaseUrl`/`providers`
     settings.
2. **Run**:

```bash
bingo                       # interactive TUI (fullscreen mode by default)
bingo --inline              # inline mode: keep history in terminal scrollback
bingo -p "fix this bug"      # headless: prompt argument, reply to stdout
bingo -p < prompt.txt       # headless: read the prompt from stdin
bingo --continue            # resume the most recent session
```

bingo starts even with no credentials: the welcome card carries onboarding (`/provider login codex` for a ChatGPT subscription, or write `apiKey` in settings); requests fail fast with next-step guidance until credentials exist.

## Command-line options

| Option | Description |
|---|---|
| `-p, --print` | headless mode: print the reply to stdout (prompt from argument or stdin) |
| `--inline` | inline mode: keep finalized output in terminal scrollback instead of using the default fullscreen canvas; conflicts with `--fullscreen` |
| `--fullscreen` | explicitly select the default fullscreen mode (alternate-screen canvas, input docked at bottom, in-app scrolling); retained for compatibility; conflicts with `--inline` |
| `--model <name>` | use the given model (falls back to the `model` settings key, then `claude-sonnet-5`) |
| `--no-team` | don't auto-start the project team (overrides settings `team.autoStart`) |
| `--permission-mode <mode>` | permission mode: `default`/`acceptEdits`/`plan`/`dontAsk`/`bypassPermissions` (default from settings) |
| `--continue` | resume the most recent session |
| `bingo share [session] [--public] [--open] [-o path]` | export self-contained HTML locally by default; `--public` explicitly publishes a link anyone can access (with a sensitive-content warning before upload) |
| `prompt` | non-interactive prompt (read from stdin if omitted; ignored in interactive mode) |

## Interface

### Input

- `Enter` sends; `\`+Enter or Ctrl+J inserts a newline (multi-line input).
- Typing `!` enters bash mode: commands execute directly, bypassing the model
  (`!echo hello`); the prefix sticks; an empty input exits with Esc/Backspace/
  Ctrl+U. Interactive/TTY commands (top/vim/ssh/fzf, etc.) are rejected — use
  batch equivalents (`top -b -n 1`).
- Large pastes collapse into a `[Pasted text #N +M lines]` placeholder and
  expand to real content on send. A paste is not typing: its newlines stay
  newlines instead of sending, and an `@` or `/` inside it is a character
  rather than a dropdown.
- `Ctrl+R` reverse history search; `↑↓` (or `Ctrl+P`/`Ctrl+N`) history recall
  (move the cursor first inside multi-line input).
- `Ctrl+S` stash/restore the input, `Ctrl+Y` paste back deleted text (`Alt+Y`
  right after it cycles the kill ring), `Ctrl+_` undo.
- `Ctrl+G`, or the readline chord `Ctrl+X Ctrl+E`, composes the draft in
  `$VISUAL`/`$EDITOR` and puts the saved content back as one undo step.
- `@` at the start of a word opens the mention dropdown over the project's
  files (git-tracked and untracked-but-not-ignored inside a repository,
  otherwise a bounded walk) and the running agents; `Tab`/`Enter` inserts the
  path relative to the session directory, or `@name` for an agent. Past a
  slash command's name the dropdown completes its **argument** instead —
  `/model`, `/theme`, `/think`, `/resume`, `/provider login` — always from the
  same data the command itself validates against.
- A shell command running in the foreground shows the last five lines of its
  output under its row while it runs, and `Ctrl+B` moves it to the background
  without restarting it: the tool call returns a task id at once and the
  completion arrives as a background-task notification.

### Key bindings (press `?` on an empty input for the full table)

| Key | Action |
|---|---|
| `Esc` | close the topmost dialog/menu/panel first / interrupt while busy / on double-press: clear the input, or open Rewind when it is empty |
| `Ctrl+C` | interrupt while busy / clear text / exit on two presses with empty input |
| `Ctrl+T` | toggle the task area |
| `↓` (at history end) | onto the conversation rows (`Enter` opens · `k` stops · `↑`/`Esc` back) |
| `Enter` (viewing) | send the draft to the agent on screen; `Esc` stops its run, then returns |
| `Ctrl+O` | open the transcript view: the whole session with every tool output, on its own screen (`ctrl+e` collapse · `/` search · `o` open the image in view · `q` close) |
| `Ctrl+G` | compose the draft in `$VISUAL`/`$EDITOR` (or the readline chord `Ctrl+X Ctrl+E`); a non-zero exit keeps the draft |
| `Ctrl+P` / `Ctrl+N` | prompt history — the same keys as `↑`/`↓`, including pulling a queued message back |
| `Alt+B` / `Alt+F` | move one word, stopping at `/` `-` `_` `.` so a path is walked a segment at a time |
| `Alt+D` / `Alt+Backspace` | kill one word forward / back (`Ctrl+W` takes the whole whitespace token) |
| `Ctrl+K` / `Alt+K` | kill to the end of the line |
| `Ctrl+Y` / `Alt+Y` | yank the newest kill; `Alt+Y` right after it cycles the 10-entry kill ring |
| `Shift+Enter` | insert a newline (wherever the terminal speaks the kitty keyboard protocol) |
| `Ctrl+B` | move the running shell command to the background; with none running, open the background dialog — agents, shells and rooms (`Enter` a detail, `f` foregrounds one, `x` stops one) |
| `Ctrl+L` | clear and redraw |
| `@` / `#` | at the start of a line, send the rest straight to that agent or room; mid-line, `@` mentions a project file or a running agent (fuzzy dropdown, `Tab`/`Enter` inserts) |
| `Tab` | complete the slash command, its argument, the selected mention, or a `!` shell-history prefix |
| `Shift+Tab` | cycle permission modes (default → acceptEdits → plan); in an approval prompt, take `Yes, and don't ask again this session` |
| `Ctrl+E` | in an approval prompt, expand the full command/diff preview and the session rule it would install |
| `Alt+T` | toggle thinking |
| Enter while busy | queue the message; the running turn folds it in at its next tool call, otherwise it sends when the turn ends |

### Slash commands (full list via `/help`)

`/model [name]` (no argument opens the provider → model picker; provider and
model persist as one pair), `/provider [name]` (list/switch among multiple
providers; `/provider login <name> [--device-auth|--manual <token>]` signs in
to subscription endpoints, `logout` signs out),
`/think [off|low|medium|high|xhigh|max]` (no argument
opens the level picker; the choice persists), `/theme`,
`/images` (the pictures this session has shown, newest first; Enter opens one in
the system viewer),
`/permissions [allow|deny|ask] [rule]`,
`/mcp` (status) · `/mcp enable|disable [name|all]` · `/mcp reconnect <name>`,
`/skills` (listing; `/skill-name` executes directly),
`/context` (usage),
`/status`, `/config` (effective config with per-key source layer/env, current
endpoint, unknown-key hints), `/compact` (force compaction), `/resume [name]` (resume a past
session), `/rename`, `/gc` (clean expired session data), `/share [--public] [--open]`, `/clear`, `/exit`.
`/share` writes a self-contained HTML file locally by default. `--public` is
an explicit opt-in to upload it to a link anyone can access; bingo shows the
sensitive-content warning before upload. `--open` opens the local file or the
published URL. The equivalent CLI is `bingo share [session] [--public]
[--open] [-o path]`.
`/team` (project teams): `list` (roster + runtime), `start` (pull up / reuse),
`status`, `assign <member> <task>`, `stop`, `validate`, `new` (scaffold
`team.json` + `team-norms.md`), `norms` (the working agreement),
`memory list|gc`.

### Themes, code and diffs

Both themes are spelled entirely in RGB, so what you see is bingo's palette
rather than your terminal's ANSI mapping (terminals without truecolor get a
256-colour approximation of the same colours). Text sits on one of three tiers:
primary for content, secondary for text *about* content (result lines, tool
output, diff context), muted for furniture (hints, stamps, rules, the diff
gutter).

Fenced code blocks are syntax-highlighted when the fence names a language —
`rust`, `python`, `javascript`/`typescript`, `json`, `bash`/`sh`, `toml`,
`yaml`, `markdown`, `diff` and a dozen more; an unknown or missing tag renders
monochrome rather than guessing. Diffs — the approval preview, the completed
edit rows and the transcript view alike — carry an old/new line-number gutter,
and long lines wrap with the gutter left blank so the code column stays
straight. `/theme` switches all of it live.

### Image rendering

Markdown images in model replies (`![alt](path)`, supporting `~/`, relative
paths, data:, and http(s)) render on kitty-graphics terminals (Ghostty/kitty,
etc.) in both modes: fullscreen places them in the live viewport, `--inline`
also flushes them into scrollback. Unsupported terminals show a `#[image]`
placeholder. Inside tmux, bingo enables passthrough automatically
(`tmux set -p allow-passthrough on`) and scrollback images render via Unicode
placeholders (U=1) when the outer terminal is Ghostty/kitty; WezTerm and
Konsole speak the graphics protocol but not placeholders, so they still show
the `#[image]` placeholder behind tmux (the live fullscreen/inline viewport
also keeps the placeholder inside tmux).

## Configuration (settings.json)

Three layers are shallow-merged; later layers override earlier ones.
UI selections (/model /provider /theme /think) persist to the layer where they
take effect: a layer that already defines the key is updated in place,
otherwise the user layer — no `.bingo/` is conjured in arbitrary directories
(`/permissions` and `/mcp disable` are project state and still write there):

1. **user**: `~/.config/bingo/settings.json` (`XDG_CONFIG_HOME` takes precedence)
2. **project**: `.bingo/settings.json` (committed to the repo — keep secrets out)
3. **local**: `.bingo/local.json` (personal overrides, not committed)

| Key | Type | Description |
|---|---|---|
| `apiKey` | string | API key (settings take precedence over `ANTHROPIC_API_KEY`/`DEEPSEEK_API_KEY`) |
| `apiBaseUrl` | string | API endpoint (settings take precedence over `ANTHROPIC_BASE_URL`; default is the official one) |
| `providers` | object | named providers: `{name: {protocol?, apiKey?, envKey?, apiBaseUrl?, supportsImages?, oauth?, models?}}`, switch with `/provider <name>`; `protocol` is `"anthropic"` (default) or `"openai"` (Responses API, bearer auth; `apiBaseUrl` defaults to `https://api.openai.com`); `envKey` names an environment variable holding the key (credential order: `apiKey` > `envKey` > stored key / OAuth); `oauth: {kind: "codex"}` enables OAuth login (`/provider login`, apiKey wins) |
| `model` | string | default model (written by `/model`); precedence: `--model` > settings > built-in `claude-sonnet-5` |
| `models` | array | the default provider's model list; per-provider under `providers.<name>.models`. Entries are ids (`"gpt-5.6-sol"`) or objects (`{id, display?, contextWindow?, maxTokens?, thinking?, vision?}`). Declared = authoritative: `/model` shows exactly this list with no request, and the metadata overrides the built-in table. `maxTokens` is the model's output ceiling — it is sent as the request's `max_tokens` and reserved out of the input window, clamped to half the window so a small `contextWindow` still leaves room to work. `vision` says whether the model accepts image input; the model is told its own capabilities in the system prompt (a text-only model refuses image-first work instead of failing silently — distinct from the endpoint-wide `sendImages`/`supportsImages` send gates). Undeclared providers pull `/v1/models` and the result is cached for 24h (`r` in the menu re-asks) |
| `thinkingLevel` | string | `off` omits thinking params (DeepSeek-compatible, default); `low`/`medium`/`high`/`xhigh`/`max` send adaptive thinking + `output_config.effort` at that level |
| `permissionMode` | string | `default` / `acceptEdits` / `plan` / `dontAsk` / `bypassPermissions` |
| `theme` | string | `auto` (follow terminal background) / `dark` / `light` |
| `cacheControl` | bool | enable prompt caching (default off: unreliable on non-official endpoints) |
| `respondToBashCommands` | bool | whether `!` commands are handed back to the model after running (default true) |
| `shell` | string | shell program for the Bash tool and hooks. Default per platform: macOS `/bin/zsh`, other Unix `/bin/bash`, Windows `powershell.exe`. PowerShell-family shells run with `-Command`; any other configured shell (e.g. Git Bash's `bash.exe`) runs with `-c` |
| `mcpServers` | object | see MCP below |
| `disabledMcpServers` | string[] | disabled MCP servers (written by `/mcp disable`) |
| `permissions` | object | `{allow[], deny[], ask[]}`, rule syntax under Permission system below |
| `experimental` | object | experimental features: `agentChannels`, `channelMessageLimit` (default 500), `agentMessageLimit` (default 50), `chatAvatars` (default false; the one switch every avatar follows — off = no avatar gutter, chips or watch-row portraits anywhere, on = faces everywhere the terminal can draw them) |
| `team` | object | team startup behavior: `{"autoStart": true}` (default true = auto-pull the project team at startup; `--no-team` or false disables) |
| `hooks` | object | per-event hooks, see Hooks below |

Example:

```json
{
  "apiKey": "sk-ant-xxxx",
  "apiBaseUrl": "https://api.anthropic.com",
  "providers": {
    "deepseek": { "apiKey": "sk-ds", "apiBaseUrl": "https://api.deepseek.com" },
    "local": { "apiKey": "sk-any", "apiBaseUrl": "http://127.0.0.1:11434/v1" },
    "openai": { "protocol": "openai", "apiKey": "sk-...", "apiBaseUrl": "https://api.openai.com" },
    "proxy": {
      "protocol": "openai",
      "apiBaseUrl": "https://proxy.example/v1",
      "envKey": "PROXY_API_KEY",
      "models": ["gpt-5.6-sol", { "id": "deepseek-v4", "display": "DeepSeek V4", "contextWindow": 131072, "maxTokens": 8000, "thinking": false, "vision": false }]
    }
  },
  "model": "claude-sonnet-5",
  "thinkingLevel": "medium",
  "permissionMode": "acceptEdits",
  "mcpServers": {
    "files": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "."] },
    "remote": { "type": "http", "url": "https://mcp.example.com/mcp", "headers": { "Authorization": "Bearer xxx" } }
  },
  "permissions": { "deny": ["Bash(git push:*)"] }
}
```

### Model catalog (model-catalog.json)

`~/.config/bingo/model-catalog.json` (created on first start, next to settings.json) holds the
per-family model defaults — `contextWindow`, `maxTokens` (output ceiling), `thinking`, `vision` —
keyed by model-id **prefix**, longest match winning field by field. Two sections with different owners:

- `builtin` is bingo's: a mirror of the researched defaults compiled into the binary, rewritten
  on upgrade so corrected numbers reach you. Edits here are reverted on the next start.
- `overrides` is yours: never touched by bingo, consulted between a settings `models` declaration
  and the built-in table. A full model id is just the longest prefix, so
  `"overrides": {"deepseek-v4-flash": {"maxTokens": 32000}}` lifts one model's output ceiling
  while `"deepseek"` entries keep governing the rest of the family.

Precedence per field: settings `models` declaration → `overrides` → built-in table → conservative
default. An unreadable file degrades to the built-ins with a startup warning and is never
overwritten (delete it to re-seed).

## Tool set

All tools go through the unified `Tool` trait (serde/schemars generates the
schema from a single source of truth):

| Tool | Description |
|---|---|
| `Bash` | runs shell commands in a separate process group (Unix) / process tree (Windows); timeout/cancel kills the whole tree, no orphan processes; non-interactive commands |
| `Read` / `Glob` / `Grep` | read-only search; skips `.git`/`target`/`node_modules` and hidden dirs by default |
| `Edit` / `Write` | file editing (produces a unified diff preview for the UI) |
| `WebFetch` / `WebSearch` | web fetching and search (shared HTTP connection pool; pre-approved domains auto-allowed) |
| `Agent` | spawns a named sub-agent (async by default, completion notification injected into context; `background:false` waits synchronously) |
| `SendMessage` | the one speech tool: `to` is an agent (`name` / `@name`) or a room (`#name`); the main agent reaches any instance, a sub-agent reaches `main` and its rooms |
| `AgentControl` | sub-agent lifecycle management (main session only) |
| `Team` | the project crew (main session only): `status`/`validate` read freely, `start`/`stop`/`save` are confirmed by the user in every permission mode |
| `TaskCreate`/`TaskUpdate`/`TaskGet`/`TaskList` | task tracking (disk-backed, same source as the TUI task area, lifecycle hooks) |
| `ExperiencePropose`/`ExperienceCommit`/`ExperienceQuery`/`ExperienceOutcome`/`ExperienceForget` | cross-session experience library (see below) |
| `AskUserQuestion` | asks the user multiple-choice questions (reuses the permission prompt modal in the TUI) |
| `Skill` | skill invocation (see below) |
| `mcp__<server>__<tool>` | tools exposed via MCP (see below) |
| `Channel` | experimental: room management (see below) |

## Sub-agents

- The main agent (depth 0) has `Agent`/`AgentControl`; sub-agents (depth ≥ 1)
  keep `Agent` (they can spawn further) and cannot manage siblings —
  hub-and-spoke topology. `SendMessage` is assembled everywhere and keeps that
  topology by *addressing*: the main agent may write to any instance and any
  room it is in, a sub-agent only to `main` and the rooms it is a member of.
- **Named definitions**: `~/.config/bingo/agents/*.md` and `.bingo/agents/*.md`
  (walked upward from cwd; project-level wins on name clash); frontmatter
  `name/description/model/provider`, body = sub-agent system prompt; referenced
  via the Agent tool's `agent` parameter.
- Instances have names (`name` parameter, defaults to the definition name/
  `agent`, auto `-2`/`-3` on collisions); the turn that spawns one shows
  `◉ @name: task` with the last three things the instance did under it (or one
  `In progress… · 4 tool uses · 8.3k tokens` line in a short window), settling
  into `Done (12 tool uses · 8.3k tokens · 1m 4s)`. A round that dispatched
  several draws one `⏺ Running 2 agents…` block with a `├─ @name: task` row per
  agent. A lifecycle event arriving when no turn is running no
  longer writes into `@main` at all — the agent tree and the instance's own row
  in the `Ctrl+B` dialog carry it instead; history is kept after completion.
- `SendMessage` sends follow-up instructions to an instance (context
  preserved); queued while busy, delivered automatically at the end of the
  current turn. A sub-agent's `SendMessage(to: "main")` lands in the main
  agent's inbox and wakes it when idle — and draws **nothing** in `@main`
  (D114): the message is main's mail, not the user's conversation, and what
  the user sees is the sender's mail signal in the status layer instead. A
  room relay draws nothing either. `urgent: true`
  (sub-agent→main only) rings the terminal attention channel on arrival.
- A run that **fails** draws one `⚠ @name · reason` line in `@main` and rings
  the attention channel; a run main's own turn dispatched that **completes** leaves one dim
  `● @name completed · task` line where its notification reaches the main
  agent's context; a cancellation draws nothing. A run the user triggered
  themselves, by writing to the instance directly, produces no notification and
  no woken turn for the main agent at all.
- A turn the main agent was *woken* into — digesting a notification rather than
  answering the user — ends like any other turn, in prose that renders in `@main`
  as the main agent speaking. The noise control is the wake debounce and the
  dispatch row's own state, not a marker that renders as nothing.
- `AgentControl` can `list`/`stop`/`delete`.
- Async by default: returns the instance name and task id immediately;
  completion notification is injected into the next turn's context.

## Agent teams (project-scoped)

A team fixes a roster of roles to a project. It is a declarative layer on top
of existing primitives: members reference named definitions (AgentDef), the
room reuses the channel machinery, and control stays on the hub-and-spoke
surface.

- **Definition**: `.bingo/team.json` (camelCase, committed to the repo):
  `name` + `channel {mode, messageLimit}` +
  `members [{name, agent, avatar?, model?, provider?, thinking?}]` —
  each member references an AgentDef, so a persona lives in one place
  (`.bingo/agents/<name>.md`) and can join multiple teams. `name` is the name
  shown on the member's messages (give it a person's name, not a role code) and
  `avatar` pins one of the bundled portraits, so a crew is a fixed cast.
- **Engine per member**: `model`, `provider` and `thinking` pin what a member
  runs on. Which model does which job is part of the formation, not a per-spawn
  whim, so it lives in the committed blueprint — a crew can put its reviewer on
  a cheap fast endpoint and its architect on the expensive one. Each falls back
  to the agent definition and then to the session, exactly as an `Agent` call's
  parameters do; a named `provider` other than the session's own needs a `model`
  too, since a model name means nothing at another endpoint. `/team list` and
  `AgentControl list` report the engine each running instance is actually on.
- **Startup pull-up**: with `settings.team.autoStart` (default true) the team
  is pulled up at startup — spawn members and create the room, but do **not**
  wake them (members sit idle at zero token cost until `/team assign` or a
  channel message). Opt out via `--no-team` or `team.autoStart: false`.
  Idempotent: instance names are the key, repeated `/team start` reuses.
- **Slash commands**: `/team list` (roster + runtime in one screen),
  `start`, `status` (● idle / ◐ busy / ✗ error / ○ offline), `assign`,
  `stop`, `validate` (same checks as start — if validate passes, start
  succeeds), `new` (interactive scaffold that always produces a valid file,
  plus a starter working agreement), `norms` (read the agreement),
  `memory list|gc`.
- **The crew is the default workforce**: where a team is pinned, main sees
  the roster in its system prompt along with the rule that goes with it — give
  the work to a member with `SendMessage`, and spawn a subagent only for what
  no member covers. Spawning a stand-in for a member that is already idle
  wastes a crew you are paying for and throws away the memory it holds.
- **A hire is temporary**: an Agent-tool spawn beside a pinned crew is a hire,
  not a member. It never enters `.bingo/team.json`; it is listed apart from the
  crew in `/team list` and `AgentControl list` (`crew` / `hire`); it is recorded
  in the crew's `decisions.md` under `type: hire`; and it is released once its
  task is done — idle, inbox empty, nothing still owed an answer, with one main
  round left to send a follow-up in. The sweep runs only while a crew is
  actually up: in a project with no team, ad-hoc subagents live as long as they
  always did.
- **Team norms**: `.bingo/team-norms.md`, committed beside the blueprint, is the
  crew's working agreement — prose, not a schema, because it is read by models
  and reviewed by people. It reaches every member and every hire as a system
  block, so it applies without being restated, and it carries its own precedence
  rule: a direct instruction outranks it on the point that instruction makes,
  and every other norm still holds. `/team new` scaffolds one (never overwriting
  an existing file); `/team norms` prints what is on disk.
- **Cross-session memory**: member history and append-only decision records
  persist to `~/.config/bingo/teams/<project-hash>/<branch>/<team>/` — scoped
  by project path + git branch, so main and a feature worktree never share
  memory. Each member gets `<name>.md` (the readable transcript) beside
  `<name>.json` (the exact record).
- **Pointed at, not preloaded**: a member spawns with an empty context and one
  line telling it where its own transcript is, so it can read what was decided
  before when the task depends on it. Loading the history instead charged a
  growing, invisible toll on the member's first turn — the file is unbounded
  and monotonic, every session appends and nothing prunes — for relevance that
  decays fast. Main starts each session clean too; a crew member should not
  be the exception. `/team memory list` shows what is on disk; open a `.md` to
  read it yourself.
- **The `Team` tool** (main session only) gives the model the same surface:
  `status` (blueprint + each member's runtime state + the definitions available
  to draft with), `validate`, `start`, `stop`, `save` (writes the blueprint;
  whole-document, so it takes the complete roster). Reads are free; **every
  change is confirmed by the user in person** — the prompt appears in every
  permission mode, including `bypassPermissions`, and an `allow` rule cannot
  pre-authorize it (only `deny` outranks it). The confirmation line names the
  change rather than the file (`Rewrite .bingo/team.json · dev-room · 4 members
  (-ui +qa)`). Hand-editing `.bingo/team.json` with Write/Edit asks the same
  question. Dispatch is not part of the tool: `SendMessage` gives a member work.

## Channels (experimental)

With `settings.experimental.agentChannels: true`:

- The main agent gets `Channel`: create channels, add/remove members
  (members are direct sub-agents; the main agent joins as `main`); members
  speak with `SendMessage(to: "#room")`, and messages enter every member's
  context (same order). The main agent's own copy is digested on a debounce —
  a burst of posts buys one turn, not one turn per post.
- In a `serial` channel, stale posts are bounced back with the new messages
  attached (agents read and adjust — sequential coordination emerges); `free`
  channels allow interleaving.
- Budget overflows freeze the channel and notify the main agent
  (`channelMessageLimit`/`agentMessageLimit` gates).
- Channels appear as `◇ #name` rows in the transcript; `#name <message>` in the
  composer posts to one as the user.

## The transcript, and the one line that leaves it

One terminal, one conversation: yours with the main agent. The flow is that
transcript in order — nothing else is spliced into it, nothing is replayed into
it, and scrolling back shows one thread rather than a braid of visits.

**Saying one thing to somebody else** is a composer line that opens with a name.
`@scout have a look at the parser` delivers the rest to scout's inbox **as you**,
bypassing the model entirely: an idle or stopped instance is resumed, a running
one takes it at its next tool round. `#build tests are green` posts to that room
as `user`, joining you first — announced in the room's own log — if you are not
a member. What you get back is a **transient** `Sent to @scout` above the
composer: never a line in the flow, never in the model's history, gone at your
next keystroke.

**A name that matches nothing is prose.** `@utils explain this code` is not an
error and not magic — it goes to the model as typed, and opens an ordinary turn.

The typeahead says which is which. At the start of a line, `@` lists every
instance the send can reach — `@scout · send message · running`, stopped ones
included, because a message resumes them — with project files under them; `#`
lists the rooms as `#build · post to room`, adding `· joins you` where you are
not a member. Mid-line the sigils mean what they always did: `@` is a file or
agent reference, `#` is a hash in a sentence.

### Rewind

Press `Esc` twice on an empty composer and bingo lists the turns you opened,
newest first, with how many files each one and everything after it changed.
Pick one and it asks what "back" should mean:

1. `Restore code and conversation`
2. `Restore conversation`
3. `Restore code`
4. `Summarize from here`
5. `Never mind`

**Restore conversation** cuts the session's history back to that message and
puts its text into the composer, ready to be asked differently. **Restore code**
puts the files back to what they were when that turn began — a file the turn
created is removed, a file it edited is reverted — and leaves the conversation
alone. **Summarize from here** replaces that turn and everything after it with
a summary of them, when you want the outcome without the transcript.

Pre-images are taken by `Edit` and `Write` just before they change anything,
once per file per turn, and are kept under `~/.local/share/bingo/rewind/` —
independent of git, bounded at 50 MB or 200 turns per session, oldest first.
An option whose half is unavailable is dimmed and says why. **What rewind does
not cover**: anything a `Bash` command wrote. A shell can change any file in any
way and there is no pre-image to take before it does, so those changes stay.

## Rooms and the team

A **room** is the only group conversation bingo has, and its members are any
subset of the team. It does not have to include you: agents form rooms among
themselves to work something out, and creating one seats the creator and nobody
else — `user` and `main` join only when named.

You speak in a room with `#name <message>` from the composer. If you are not a
member, that posts you in — there is no quiet way in, because joining and leaving
are written into the room as dim `· user joined · 14:32` lines that every member
sees. `/join #name` makes you a member without saying anything; `/leave #name` is
the counterpart, and a room you leave stays readable.

**The conversation rows** are what say who is working — and, since D115's
badges, what has been said while you were not looking. They line up under the
composer, constant once anybody exists: `● main` first — filled while main
works — then every instance (`● @scout: reading src/lib.rs… · 12 tool uses ·
8.3k tokens` while it runs, `Idle for 14s`, `[stopped]`), then the rooms you
are in (`#dev-team: 3 members`), names in their own colours, at most three
rows with the cursor scrolling the window (`↓ 2 more` on the edge). Every row
wears its conversation's **badge**: unread is a bare dot (`•`), words at *you*
— a room post naming `@user` — are the count in the accent (`•3`) and ring
once per mention until you read the room; an agent **waiting on your
permission** turns its row to `waiting on you (permission)` in the accent.
There is no key to learn: `↓` at the end of your prompt history drops onto
the rows, `↓/↑` walk them, `Enter` opens the row's conversation as the page
on screen, `k` stops a selected running instance, `↑` off the top or `Esc`
returns to the draft, and any letter just keeps typing. Entering a
conversation reads it, and reading clears its badge. The task panel
(`Ctrl+T`) names an owner who is still here (` (@scout)`, in their colour)
and what a task is waiting on (`› blocked by #3`) — display only, nothing
assigns or claims.

**The pages** put any conversation on the screen for as long as you want it
there — drawn by the very pipeline `@main` is drawn by, flushed into the
terminal's own scrollback as it settles. `Enter` on a row — or `f` in the
`Ctrl+B` dialog — turns the page: `── @scout ──`, then that agent's **whole
record** in order — the task it was given, main's instructions, your own
messages, its work folded the way `@main`'s is, its answers, and whatever it
is streaming right now. Switching banks the page you leave into scrollback
and starts the next at the top; coming home reprints a recent tail of main's.
The composer stays live and addresses the agent in its own colour: what you
type reaches its inbox under your name and appears as the queued message it
is, and a `/` or `!` line is a message rather than a command. A **room's page
is speech only** — what members sent to the room, each post under its
sender's name; typing posts to the room and joins you first. `Esc` stops a
running subject first and comes home on the next press (main's own turn is
out of its reach; `Ctrl+C` keeps the override); `Shift+Tab` cycles *that
agent's* permission mode and the footer badge follows it. A page closes
itself when its subject leaves the registry; a *finished* agent's page stays,
because reading it is the point.

**What the transcript shows of a life it is not living** is four tiers and no
more. A **dispatch** is `◉ @scout: fix the parser`, carrying the last three
things the instance did while it runs — or one `In progress… · 4 tool uses ·
8.3k tokens` line where the window is too short for them — and settling, when
the run ends, into `Done (12 tool uses · 8.3k tokens · 1m 4s)`, which is what
reaches scrollback. Several `Agent` calls from one round draw one
`⏺ Running 2 agents…` block instead, with a `├─ @name: task` row each. A
**completion** adds one dim `● @scout completed · fix the parser` — for a run
this turn's own `Agent` call dispatched, and only for one: a run a delivery
woke (a room post, a queued message) completes into the tree and the dialog,
never the flow (D114). A **failure** adds one
`⚠ @scout · connection reset` and rings the attention channel. Everything else
— an instance starting, going idle, being stopped, a room post, a message an
agent sends main — writes nothing:
state belongs to the tree, mail belongs to main, and a line per room post is
the flood the digest debounce exists to prevent.

**One walk decides who said what**, because the sender is not a field: an
absorbed inbox arrives as one flat prompt and only its literal markers survive
it. An agent's page keeps every counterpart that walk finds; the unread count
keeps exactly one, the pair of you and the agent. So the task an instance was
created with, the main agent's instructions to it, room relays, mail from other
agents and the chases for its silence are all somebody else's conversation and
count as nothing of yours — and the default flips with whose record it is: in an
instance's history unmarked prose is the main agent speaking, in the main
agent's own history it is you.

**Avatars** (`experimental.chatAvatars`, off by default — the one switch every
avatar follows): with it on, terminals that can place kitty images — the same
capability behind inline image rendering (Ghostty/kitty, and tmux with
passthrough) — draw each sender as one of eight bundled
[anime-style portraits](assets/avatars/), 4×2 cells beside the name,
transmitted once per portrait and placed by Unicode placeholder cells. A team
member's portrait is pinned in `.bingo/team.json` (`"avatar": "sora"`) so a
crew keeps a fixed cast; everyone else gets a face derived from their name.
Terminals without that capability keep the sender's initial on a colour, and a
subagent's watch row wears that agent's portrait in place of its `◉`. With the
switch off there is no avatar gutter, no chips and no watch-row portrait
anywhere — identity colours stay, because a colour is not an avatar.

**With avatars on, every conversation wears them, `@main` included.** The face
sits in a left gutter — four or five cells taken out of the width before the
body wraps — with the portrait on the first row of each speaker's run and blank
on every row after it; work steps and system lines take the indentation and no
face. Main has a reserved portrait of its own that no teammate can be dealt or
pin, so the console looks the same in every session. One known degradation: a
terminal that purges its image store (a resize does) gets the faces still on
screen redrawn, but rows already in scrollback keep blank columns where the
portrait was. On a page the sender's name also heads each run of
messages — with more than two speakers in a room, the name is not decoration.

## Skills

- Load order (highest priority first): user `~/.config/bingo/skills/` →
  project `.bingo/skills/` (walked upward from cwd, nearest first) → bundled
  `guide` (compiled into the binary, fallback only); same-name disk skills
  override the bundled one.
- One directory per skill: `<name>/SKILL.md` with YAML frontmatter
  (`description`/`when_to_use`/`arguments`) + markdown body.
- Invocation: the model calls via `SkillTool` automatically; the user runs
  `/skill-name [args]` directly.
- Bundled `guide`: bingo usage & troubleshooting manual (consult it when
  answering "how to configure / why / it doesn't work").

## Experience library

A cross-session knowledge base for operations that recur across a project:
when the agent repeatedly does the same thing, it can propose, commit and
later query reusable experience — the value compounds over sessions.

- **Storage**: `~/.config/bingo/experience/<project-key>/entries/<id>.md`
  (user-global, never touches the project workspace); per-project isolation.
- **Entry shape**: `trigger` (keywords), `summary`, `steps`, `verify`,
  `evidence` (where it came from), plus explicit helpful/harmful outcome counters
  and append-only outcome history with SHA-256-bound evidence — frontmatter +
  free-form body.
- **Tools**:
  - `ExperiencePropose` — generates a candidate with a stable id; writes nothing.
  - `ExperienceCommit` — persists an entry (goes through the permission gate);
    identical content maps to the same id, re-committing updates instead of
    duplicating; `status: stale` stops injection into new sessions but stays
    queryable.
  - `ExperienceQuery` — matches on any trigger keyword (case-insensitive
    substring); active entries rank above stale/degraded, then explicit observed
    outcomes rank before the legacy commit count; results include outcome
    counters and history.
  - `ExperienceOutcome` — after actually applying a queried entry, records a
    permission-confirmed `helpful` or `harmful` result with concrete evidence;
    it appends history and never changes lifecycle `status` or `verified_at`
    automatically.
  - `ExperienceForget` — deletes an entry.
- **Status lifecycle**: `active` → `degraded` → `stale`; active entries are
  injected into new sessions, stale ones are only queryable.

## MCP

`mcpServers` config, driven by the official `rmcp` Rust SDK; tools are listed
on connect and adapted to bingo's Tool trait:

```json
"mcpServers": {
  "files": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "."] },
  "remote": { "type": "http", "url": "https://mcp.example.com/mcp", "headers": { "Authorization": "Bearer xxx" } }
}
```

- Transports: stdio (default `command`/`args`/`env`) and streamable HTTP
  (`type: "http"`, optional custom headers for auth). `sse`/`ws` are not
  implemented yet; configuring them errors on connect.
- Tool naming: `mcp__<server>__<tool>`; use full names in permission rules
  (e.g. the `mcp__server` prefix or the full tool name).
- Diagnostics: `/mcp` shows status; stdio server output goes to
  `~/.local/share/bingo/logs/mcp-<name>.log`; after fixing, run
  `/mcp reconnect <name>`.
- Disable/enable: `/mcp disable|enable [name|all]` (persisted to settings.json).

## Permission system

### Permission modes (`--permission-mode` or settings `permissionMode`)

| Mode | Behavior |
|---|---|
| `default` | read-only tools allowed; everything else asks (rule tables can auto-allow) |
| `acceptEdits` | edit tools (Edit/Write, etc.) allowed automatically |
| `plan` | read-only + task list management; everything else denied (planning mode) |
| `dontAsk` | all non-read-only tools denied (no prompting) |
| `bypassPermissions` | allow everything (but deny/ask rules and sensitive-path checks still apply) |

### Rule syntax (settings `permissions` section)

- Form: `Tool(content)`; `:*` is a prefix wildcard (e.g. `Bash(git push:*)`);
  `*` matches everything.
- **Bash**: split on shell operators (`&&` `;` `|` `$()`, etc.) into
  sub-commands; deny/ask matches if any sub-command hits; allow requires a
  single rule covering **every** sub-command; commands with unterminated quotes
  are never auto-allowed.
- **File tools** (Read/Edit/Write/Grep/Glob): path prefix match after
  normalization (`~` expansion, relative paths resolved against cwd, `..`
  resolved), so `Read(src/)` also matches absolute paths.
- **WebFetch**: supports `domain:` rules and URL prefixes; pre-approved domains
  auto-allow.
- **Skill**: `Skill(name)` exact, `Skill(name:*)` prefix.
- **MCP**: not exempted by the server's self-reported read-only hint; explicit
  allow required.
- Order: deny → ask → (read-only/pre-approved) → sensitive-path check → bypass
  → acceptEdits → allow rules → ask. deny/ask rules still apply in bypass mode;
  destructive writes into sensitive dirs (`.git`/`.claude`/`.vscode`/`.idea`)
  always prompt.

Example:

```json
"permissions": {
  "allow": ["Read(src/*)", "Bash(git status)", "WebFetch(domain:github.com)"],
  "deny":  ["Bash(git push:*)", "Bash(rm -rf)"],
  "ask":   ["Bash(git push)"]
}
```

### The approval prompt

When the gate asks, the prompt shows what it is about to do — a Bash call's
command lines, an Edit/Write's dry-run diff (computed without touching the
file) — above three options:

1. `Yes`
2. `Yes, and don't ask again this session` — `Shift+Tab` confirms it directly.
   Offered only when the narrowest matching rule (`Bash(cargo:*)`,
   `Edit(/path/to/)`, `WebFetch(domain:…)`, or the bare tool name) would really
   stop the gate asking; an `ask` rule of yours and the sensitive-path /
   `confirm_reason` checks outrank allow rules, so for those the option is not
   shown at all. The rule lives in memory for this session and is never written
   to settings.
3. `No, and tell bingo what to do differently (esc)` — Enter opens a feedback
   row; what you type reaches the model with the denial. `Esc` anywhere, and an
   empty feedback submit, are the plain refusal.

`Ctrl+E` expands the preview and shows the exact session rule option 2 would
install. Enter and digits are ignored for the first 0.4s a prompt is on screen,
so a keystroke already in flight cannot approve anything.

## Hooks

Events in the `hooks` config: `PreToolUse` / `PostToolUse` / `PreCompact` /
`PostCompact` / `UserPromptSubmit` / `Stop` / `SessionStart` / `SessionEnd` /
`TaskCreated` / `TaskCompleted`. Each event is a list of
`{matcher, hooks:[{type:"command", command}]}`:

- matcher is an anchored regex (`Edit\|Write`, `mcp__.*`); empty matches
  everything; on compile failure it falls back to exact match with a warning.
- Hooks run via the configured shell (`-c` style; PowerShell `-Command` on
  Windows by default), event JSON on stdin (`hook_event_name`, `tool_name`,
  `tool_input`, `permission_mode`, etc.), JSON on stdout.
- Exit-code semantics: 0 = success; 2 = blocking (stderr injected into the
  model / blocks the turn); other non-zero = user-visible only, non-blocking.
- `PreToolUse` supports `{"decision":"deny|ask","reason","updatedInput"}` to
  rewrite input.
- Normal hooks time out after 60s (SessionEnd: 1.5s fast shutdown); timeouts
  kill the process, leaving no residue.

Example (PreToolUse denies Bash):

```json
"hooks": {
  "PreToolUse": [{
    "matcher": "Bash",
    "hooks": [{ "type": "command", "command": "echo '{\"decision\":\"deny\",\"reason\":\"no\"}'" }]
  }]
}
```

## Sessions, compaction & memory

- **Transcript**: `~/.local/share/bingo/transcripts/<project>-<ts>.jsonl`, one
  Message per line; corrupt lines are skipped without blocking recovery.
  `--continue` resumes the latest session, `/resume [name]` lists/switches,
  `/rename` renames. Startup cleanup and `/gc` retain the newest 100
  inactive sessions and remove sessions older than 30 days; sessions touched in
  the last 24 hours are never count-pruned; matching share snapshots
  follow transcript deletion. Prompt-history files use the same TTL and a
  100-file cap. Local exported HTML and task lists are never removed.
- **Context budget**: per-model window and output budget (from the settings
  declaration, `model-catalog.json`, or the built-in family table; unknown
  models assume 200k/64k), effective input window = window − output budget;
  auto-compaction threshold = 90% of the effective window (≈785k for current
  Claude models), with a 20k headroom warning (`/context`). Compaction
  summarizes old messages and keeps the most recent 8; the split point advances
  safely past tool_result boundaries to avoid orphaned tool_result 400s.
  Fuse after 3 consecutive failures (`/compact` forces manually). Non-Anthropic
  endpoints (no count_tokens) fall back to local estimation (chars/4).
- **Memory**: memdir auto-memory
  (`~/.config/bingo/memdir/<project>-<path-hash>.md`, full-path hash avoids
  collisions between same-named projects) + project CLAUDE.md and AGENTS.md as
  system memory.

## Architecture

```text
CLI (clap)
  → settings, three layers merged (user/project/local)
  → Messages API client (reqwest + SSE streaming)
  → query loop: tool calls → permission gate → concurrent execution → results fed back
  → TUI (ratatui inline/fullscreen + crossterm) | headless --print
       ├─ Tool Registry (trait + schemars schema)
       ├─ MCP adapter layer (rmcp: stdio / streamable HTTP)
       ├─ sub-agents (hub-and-spoke, async + notifications)
       ├─ Hooks (shell, JSON contract)
       ├─ Task store / channels / skills / memory / transcript
       └─ budget monitoring & compaction
```

Core loop semantics: **the model only produces tool_use intent; permissions,
parallelism, side effects, compaction, memory, and the UI are the local
harness's job**. Design decisions live in
[`notes/research.md`](notes/research.md) (D1–D36).

## Project layout

```text
src/
  main.rs          CLI entry (clap), session bootstrap
  api/             Messages API client (client / SSE / types)
  query.rs         main loop (queryLoop), slash-mutable runtime
  tools.rs         tool assembly (by depth / experimental flags)
  tool/            tool implementations + the Tool trait contract
  permission.rs    unified permission gate (modes × rule tables)
  hooks.rs         shell hooks (events / matcher / JSON contract)
  agents.rs        sub-agent sessions & history, named definition loading
  tool/agent.rs    Agent / SendMessage / AgentControl implementations
  team.rs          team parsing / validation / spawn + team memory (D31)
  team_cmd.rs      /team slash-command family
  tool/team.rs     Team tool (model-facing, user-confirmed changes, D46)
  experience.rs    cross-session experience library
  tool/experience.rs  ExperiencePropose/Commit/Query/Outcome/Forget tools
  channels.rs      channel registry (experimental)
  tasks.rs         task store (Task tool family)
  skills.rs        skill loading / frontmatter / argument substitution
  mcp.rs           MCP manager (stdio / streamable HTTP)
  settings.rs      three-layer config loading and merging
  transcript.rs    session persistence (JSONL)
  compact.rs       automatic / manual compaction
  budget.rs        token budget constants
  memory.rs        memdir memory extraction and loading
  watch.rs         background task registry & notifications
  tui/             ratatui UI (chat / view / input / markdown / highlight / gfx …)
  ui.rs            headless hooks and shared rendering
  system.rs        system prompt assembly (memory + project memory + skills listing)
tests/
  fixtures/        integration-test fixtures
notes/
  research.md      technical decision record (D1–D36)
```

## Development conventions

- Rust 2024 edition; errors via thiserror; no unwrap/expect in production code.
- Write code the way the surrounding code does; prefer no comments — comments
  explain only "why".
- No unnecessary dependencies; check crates.io before reinventing a wheel.
- Changes touching user-visible behavior (config keys / slash commands / tools /
  error messages / capability map) must update the bundled skill
  `src/skills/bundled/guide.md` in the same batch (AGENTS.md sync rule).
- Consult `notes/research.md` decision records before changing architecture.
- Every change runs `cargo build` and `cargo clippy -- -D warnings`; relevant
  logic ships with tests.
