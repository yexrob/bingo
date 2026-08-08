# bingo

> **中文版**：[README.zh-CN.md](README.zh-CN.md) — 中文文档见这里。

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
  scoped by project path + git branch.
- **Experience library**: agents accumulate reusable operational experience
  per project (trigger/summary/steps/verify), shared across sessions via
  Propose/Commit/Query/Forget tools.
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
  recovery), memdir auto-memory, plus CLAUDE.md/AGENTS.md project memory.
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

Startup fails with an error if no API key is present.

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
  expand to real content on send.
- `Ctrl+R` reverse history search; `↑↓` history recall (move the cursor first
  inside multi-line input).
- `Ctrl+S` stash/restore the input, `Ctrl+Y` paste back deleted text,
  `Ctrl+_` undo.

### Key bindings (press `?` on an empty input for the full table)

| Key | Action |
|---|---|
| `Esc` | interrupt while busy / close dropdowns and panels / clear input on double-press |
| `Ctrl+C` | interrupt while busy / clear text / exit on two presses with empty input |
| `Ctrl+T` | toggle the task area |
| `Ctrl+O` | expand/collapse: expanded replays the full transcript for scrolling up |
| `Ctrl+G` | agent/channel picker (agent view shows full instance conversation; channel view is a WeChat-style room) |
| `Ctrl+L` | clear and redraw |
| `Shift+Tab` | cycle permission modes (default → acceptEdits → plan) |
| `Alt+T` | toggle thinking |
| Enter while busy | queue the message; auto-sends when the turn ends |

### Slash commands (full list via `/help`)

`/model [name]` (no argument opens the provider → model picker; the choice
persists to `.bingo/settings.json`), `/provider [name]` (list/switch among
multiple providers), `/think [off|low|medium|high|xhigh|max]` (no argument
opens the level picker; the choice persists), `/theme`,
`/permissions [allow|deny|ask] [rule]`,
`/mcp` (status) · `/mcp enable|disable [name|all]` · `/mcp reconnect <name>`,
`/skills` (listing; `/skill-name` executes directly), `/context` (usage),
`/status`, `/compact` (force compaction), `/resume [name]` (resume a past
session), `/rename`, `/share [--public] [--open]`, `/clear`, `/exit`.
`/share` writes a self-contained HTML file locally by default. `--public` is
an explicit opt-in to upload it to a link anyone can access; bingo shows the
sensitive-content warning before upload. `--open` opens the local file or the
published URL. The equivalent CLI is `bingo share [session] [--public]
[--open] [-o path]`.
`/team` (project teams): `list` (roster + runtime), `start` (pull up / reuse),
`status`, `assign <member> <task>`, `stop`, `validate`, `new` (scaffold
`team.json`), `memory list|gc`.

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

Three layers are shallow-merged; later layers override earlier ones:

1. **user**: `~/.config/bingo/settings.json` (`XDG_CONFIG_HOME` takes precedence)
2. **project**: `.bingo/settings.json` (committed to the repo — keep secrets out)
3. **local**: `.bingo/local.json` (personal overrides, not committed)

| Key | Type | Description |
|---|---|---|
| `apiKey` | string | API key (settings take precedence over `ANTHROPIC_API_KEY`/`DEEPSEEK_API_KEY`) |
| `apiBaseUrl` | string | API endpoint (settings take precedence over `ANTHROPIC_BASE_URL`; default is the official one) |
| `providers` | object | named providers: `{name: {protocol?, apiKey, apiBaseUrl?, supportsImages?, oauth?}}`, switch with `/provider <name>`; `protocol` is `"anthropic"` (default) or `"openai"` (Responses API, bearer auth; `apiBaseUrl` defaults to `https://api.openai.com`); `oauth: {kind: "codex"}` enables OAuth login (`/provider login`, apiKey wins) |
| `model` | string | default model (written by `/model`); precedence: `--model` > settings > built-in `claude-sonnet-5` |
| `thinkingLevel` | string | `off` omits thinking params (DeepSeek-compatible, default); `low`/`medium`/`high`/`xhigh`/`max` send adaptive thinking + `output_config.effort` at that level |
| `permissionMode` | string | `default` / `acceptEdits` / `plan` / `dontAsk` / `bypassPermissions` |
| `theme` | string | `auto` (follow terminal background) / `dark` / `light` |
| `cacheControl` | bool | enable prompt caching (default off: unreliable on non-official endpoints) |
| `respondToBashCommands` | bool | whether `!` commands are handed back to the model after running (default true) |
| `shell` | string | shell program for the Bash tool and hooks. Default per platform: macOS `/bin/zsh`, other Unix `/bin/bash`, Windows `powershell.exe`. PowerShell-family shells run with `-Command`; any other configured shell (e.g. Git Bash's `bash.exe`) runs with `-c` |
| `mcpServers` | object | see MCP below |
| `disabledMcpServers` | string[] | disabled MCP servers (written by `/mcp disable`) |
| `permissions` | object | `{allow[], deny[], ask[]}`, rule syntax under Permission system below |
| `experimental` | object | experimental features: `agentChannels`, `channelMessageLimit` (default 500), `agentMessageLimit` (default 50) |
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
    "openai": { "protocol": "openai", "apiKey": "sk-...", "apiBaseUrl": "https://api.openai.com" }
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
| `TaskCreate`/`TaskUpdate`/`TaskGet`/`TaskList` | task tracking (disk-backed, same source as the TUI task area, lifecycle hooks) |
| `ExperiencePropose`/`ExperienceCommit`/`ExperienceQuery`/`ExperienceForget` | cross-session experience library (see below) |
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
  `name` + `channel {mode, messageLimit}` + `members [{name, agent}]` — each
  member references an AgentDef, so a persona lives in one place
  (`.bingo/agents/<name>.md`) and can join multiple teams.
- **Startup pull-up**: with `settings.team.autoStart` (default true) the team
  is pulled up at startup — spawn members and create the room, but do **not**
  wake them (members sit idle at zero token cost until `/team assign` or a
  channel message). Opt out via `--no-team` or `team.autoStart: false`.
  Idempotent: instance names are the key, repeated `/team start` reuses.
- **Slash commands**: `/team list` (roster + runtime in one screen),
  `start`, `status` (● idle / ◐ busy / ✗ error / ○ offline), `assign`,
  `stop`, `validate` (same checks as start — if validate passes, start
  succeeds), `new` (interactive scaffold that always produces a valid file),
  `memory list|gc`.
- **Cross-session memory**: member history and append-only decision records
  persist to `~/.config/bingo/teams/<project-hash>/<branch>/<team>/` — scoped
  by project path + git branch, so main and a feature worktree never share
  memory. Restored on pull-up without waking the members.

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
- Channels appear as `◇ #name` rows in the transcript; Ctrl+G opens a fullscreen
  room where you can post as the user.

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
  `evidence` (where it came from) — frontmatter + free-form body.
- **Tools**:
  - `ExperiencePropose` — generates a candidate with a stable id; writes nothing.
  - `ExperienceCommit` — persists an entry (goes through the permission gate);
    identical content maps to the same id, re-committing updates instead of
    duplicating; `status: stale` stops injection into new sessions but stays
    queryable.
  - `ExperienceQuery` — matches on any trigger keyword (case-insensitive
    substring); active entries rank above stale/degraded, then by hit count.
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
  `/rename` renames.
- **Context budget**: 200k window, 64k output budget, effective input window =
  window − output budget; auto-compaction threshold = 90% of the effective
  window (≈122k), with a 20k headroom warning (`/context`). Compaction
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
  experience.rs    cross-session experience library
  tool/experience.rs  ExperiencePropose/Commit/Query/Forget tools
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
