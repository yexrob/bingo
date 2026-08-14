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
| `Ctrl+O` | open the transcript view: the whole session with every tool output, on its own screen (`ctrl+e` collapse · `/` search · `q` close) |
| `Ctrl+G` | compose the draft in `$VISUAL`/`$EDITOR` (or the readline chord `Ctrl+X Ctrl+E`); a non-zero exit keeps the draft |
| `Ctrl+P` / `Ctrl+N` | prompt history — the same keys as `↑`/`↓`, including pulling a queued message back |
| `Alt+B` / `Alt+F` | move one word, stopping at `/` `-` `_` `.` so a path is walked a segment at a time |
| `Alt+D` / `Alt+Backspace` | kill one word forward / back (`Ctrl+W` takes the whole whitespace token) |
| `Alt+K` | kill to the end of the line (this was `Ctrl+K` before the switcher took that key) |
| `Ctrl+Y` / `Alt+Y` | yank the newest kill; `Alt+Y` right after it cycles the 10-entry kill ring |
| `Shift+Enter` | insert a newline (wherever the terminal speaks the kitty keyboard protocol) |
| `Ctrl+K` | switch conversation: every conversation in one list, type to filter, `Enter` opens, `Ctrl+X` stops a running agent |
| `Ctrl+B` | move the running shell command to the background; with none running, manage background agents |
| `Ctrl+L` | clear and redraw |
| `@` | mention a project file or a running agent: fuzzy dropdown, `Tab`/`Enter` inserts the relative path (or `@name`) |
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
`/permissions [allow|deny|ask] [rule]`,
`/mcp` (status) · `/mcp enable|disable [name|all]` · `/mcp reconnect <name>`,
`/skills` (listing; `/skill-name` executes directly),
`/open <@agent|#channel|#team|hub>` (enter a conversation; Tab completes from
the ones that exist — `Ctrl+K` is the same door without typing a name),
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
| `experimental` | object | experimental features: `agentChannels`, `channelMessageLimit` (default 500), `agentMessageLimit` (default 50), `chatAvatars` (default false = no faces above messages) |
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
| `SendMessage` / `AgentControl` | sub-agent continuation and lifecycle management (main session only) |
| `Team` | the project crew (main session only): `status`/`validate` read freely, `start`/`stop`/`save` are confirmed by the user in every permission mode |
| `TaskCreate`/`TaskUpdate`/`TaskGet`/`TaskList` | task tracking (disk-backed, same source as the TUI task area, lifecycle hooks) |
| `ExperiencePropose`/`ExperienceCommit`/`ExperienceQuery`/`ExperienceOutcome`/`ExperienceForget` | cross-session experience library (see below) |
| `AskUserQuestion` | asks the user multiple-choice questions (reuses the permission prompt modal in the TUI) |
| `Skill` | skill invocation (see below) |
| `mcp__<server>__<tool>` | tools exposed via MCP (see below) |
| `Channel` / `Post` | experimental: agent channel messaging (see below) |

## Sub-agents

- The main agent (depth 0) has `Agent`/`SendMessage`/`AgentControl`; sub-agents
  (depth ≥ 1) keep only `Agent` (they can spawn further) and cannot manage
  siblings — hub-and-spoke topology.
- **Named definitions**: `~/.config/bingo/agents/*.md` and `.bingo/agents/*.md`
  (walked upward from cwd; project-level wins on name clash); frontmatter
  `name/description/model/provider`, body = sub-agent system prompt; referenced
  via the Agent tool's `agent` parameter.
- Instances have names (`name` parameter, defaults to the definition name/
  `agent`, auto `-2`/`-3` on collisions); the transcript shows `◉ name · task`;
  history is kept after completion.
- `SendMessage` sends follow-up instructions to an instance (context
  preserved); queued while busy, delivered automatically at the end of the
  current turn.
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
- **The crew is the default workforce**: where a team is pinned, the hub sees
  the roster in its system prompt along with the rule that goes with it — give
  the work to a member with `SendMessage`, and spawn a subagent only for what
  no member covers. Spawning a stand-in for a member that is already idle
  wastes a crew you are paying for and throws away the memory it holds.
- **A hire is temporary**: an Agent-tool spawn beside a pinned crew is a hire,
  not a member. It never enters `.bingo/team.json`; it is listed apart from the
  crew in `/team list` and `AgentControl list` (`crew` / `hire`); it is recorded
  in the crew's `decisions.md` under `type: hire`; and it is released once its
  task is done — idle, inbox empty, nothing still owed an answer, with one hub
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
  decays fast. The hub starts each session clean too; a crew member should not
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

- The main agent gets `Channel`/`Post`: create channels, add/remove members
  (members are direct sub-agents; the main agent joins as `main`); members
  post via `Post`, and messages enter every member's context (same order).
- In a `serial` channel, stale posts are bounced back with the new messages
  attached (agents read and adjust — sequential coordination emerges); `free`
  channels allow interleaving.
- Budget overflows freeze the channel and notify the main agent
  (`channelMessageLimit`/`agentMessageLimit` gates).
- Channels appear as `◇ #name` rows in the transcript; `/open #name` enters one,
  where you can post as the user.

## Conversations

One terminal, one flow, one conversation at a time. The hub — your conversation
with the model — is one of them; a DM with a running subagent, an agent channel
and the `#team` board are the others, and they all wear the same composer, the
same keys, the same approval dialogs and the same transcript rendering. There is
no separate screen to enter and no second set of controls to learn.

**Entering one** is `Ctrl+K` — every conversation in one list, most recently
active first with the hub pinned on top, filtered as you type, opened with
Enter — or `/open @agent`, `/open #channel`, `/open #team`, `/open hub` (Tab
completes from the conversations that exist); a running agent's DM also opens
from the Ctrl+B manager with Enter. Above the composer, a **conversation bar**
lists what exists — presence for DMs (`●` running, `○` idle), an unread count,
and the one you are in accented — and it appears only once there is more than
one conversation to switch between.

**Saying one thing without going there**: from the hub, a message that opens
with a conversation's name delivers the rest to it and leaves you where you
are — `@scout have a look at the parser` reaches scout, and the flow keeps a
dim `→ @scout: have a look at the parser` receipt. A name that matches nothing
is not an error and not magic: it is prose, and it goes to the model as typed.
Inside a conversation there is no such rule, because the conversation you are
in already *is* the destination. **Esc goes back to the hub** —
navigation before interruption, so a turn running behind you keeps running and
its Esc-to-interrupt waits for you at the hub. Ctrl+C is unchanged and stops the
turn from anywhere.

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

**What switching does**: the draft you were writing stays behind in the
conversation you left and that conversation's own draft comes back; a `── @name ──`
rule goes into the flow, followed by that conversation's last 30 messages. From
then on only that conversation prints. Everything else keeps accumulating where
it already lives and counts up an unread badge — nothing is buffered on your
behalf, so nothing can be lost by not looking at it. Coming back prints a
`── hub ──` rule and whatever the hub finished while you were away.

Because scrollback is written once and never rewritten, a couple of excursions
leave the same conversation on screen more than once. That is deliberate and the
rules mark it; `Ctrl+O` (the transcript view) remains the complete record of the
hub session.

**Sending**: what you type goes to the conversation you are in — a DM delivers
to that instance under your name, a channel posts to the log, and `#team` is a
record rather than a room and says so. None of it starts a model turn. Slash
commands are the exception and act on the application from anywhere, so `/model`
in a DM is still `/model`.

**Avatars**: on terminals that can place kitty images — the same capability
behind inline image rendering (Ghostty/kitty, and tmux with passthrough) — each
sender wears one of eight bundled [anime-style portraits](assets/avatars/), 4×2
cells beside the name, transmitted once per portrait and placed by Unicode
placeholder cells. A team member's portrait is pinned in `.bingo/team.json`
(`"avatar": "sora"`) so a crew keeps a fixed cast; everyone else gets a face
derived from their name. Terminals without that capability keep the sender's initial
on a colour; the row count is identical either way, so only the gutter changes.

**In the main chat**, behind `experimental.chatAvatars` (off by default), the
same faces sit on a band above each message: the
speaker's portrait beside their name — `main` for the hub, `You` for your own
messages, the names the room itself uses. Message bodies are untouched
underneath; they still run the full width, and the `⏺` markers inside a message
keep separating prose from tool rows. The band is two rows where portraits place
and one where they fall back to the chip — nothing below it depends on its
height. One known degradation: a terminal that purges its image store (a resize
does) gets the faces still on screen redrawn, but messages already in scrollback
keep four blank columns where the portrait was, with the name intact. Switched
off, the transcript carries no band and a subagent's watch row keeps its `◉`.
In a DM or a channel the sender's name heads each run of messages either way —
with more than two speakers in a room, the name is not decoration.

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
  tui/             ratatui UI (chat / view / input / markdown / gfx …)
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
