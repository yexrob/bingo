---
name: guide
description: >-
  bingo usage guide and diagnostic manual: settings config, slash commands,
  modes, MCP, troubleshooting.
  Use when the user asks how to use/configure bingo, or reports a problem
  ("why", "how to configure", "how to diagnose", "not working").
when_to_use: >-
  User asks how to configure or use bingo · reports a bug or unexpected
  behavior · asks about settings.json / slash commands / MCP / permissions.
---

# bingo Usage and Diagnostic Guide

When answering user questions, use this guide to locate config options, commands, and troubleshooting paths; give concrete file paths,
commands, and verification steps in conclusions. Never speculate about features (capabilities follow actual behavior; when unsure, read the source to confirm).

## Quick start

- Interactive sessions use the fullscreen alternate-screen canvas by default; run `bingo --inline`
  when finalized output should remain in terminal scrollback. `--fullscreen` remains a compatible
  explicit selection and conflicts with `--inline`.
- Starting requires an API key: `ANTHROPIC_API_KEY` (Anthropic) or `DEEPSEEK_API_KEY`
  (DeepSeek); custom endpoints use `ANTHROPIC_BASE_URL`. These can also go in settings.json
  (`apiKey`/`apiBaseUrl`, see below; settings take precedence over environment variables). Startup errors if missing.
- Type `!` to enter bash mode (commands run directly without the model; the `!` prefix is sticky);
  try `!echo hello`. Interactive/fullscreen commands (top/vim/ssh/fzf/lazygit) are rejected —
  use batch alternatives: `top -b -n 1`; for `vim file` use the Edit tool.
- Shortcuts (press `?` with an empty input for the full table): Enter sends · `\`+Enter / Ctrl+J newline (multi-line input) ·
  Esc busy interrupt / close dropdowns and panels / double-press clears input · Ctrl+C busy interrupt / clears text /
  empty input twice exits · ↑↓ history (move the cursor first in multi-line input; busy empty-input ↑ recalls queued messages) ·
  Ctrl+R reverse history search · Ctrl+A/E line start/end · Alt+B/F word movement · Ctrl+W/U/K delete word/to line
  start/end · Ctrl+Y paste back deleted · Ctrl+S stash/restore input · Ctrl+_ undo · ctrl+o
  expand/collapse toggle (expand = replays the full transcript for terminal scroll-up; press again to collapse back to
  aggregates and clear/consolidate) · Ctrl+T toggle the task area · Ctrl+G agent/channel selector (↑↓ to select,
  Enter opens a fullscreen view, Esc closes; the agent view shows that instance's full conversation and streaming output; the channel view
  is a WeChat-style group room where you can speak directly as user) · Ctrl+L clear and redraw · Shift+Tab cycles permission
  modes (default → acceptEdits → plan) · Alt+T thinking toggle (off ↔ the last non-off level, default medium) · while busy, Enter queues the message (sent automatically at turn end; /think /model /provider /theme /status /context /tasks /help /skills run immediately) ·
- Large pastes auto-collapse to a `[Pasted text #N +M lines]` placeholder; the real content expands on send
  (precisely detected via terminal bracketed-paste events; terminals without that feature fall back to a
  key-burst heuristic — extremely fast typing may misdetect, and pausing recovers).
- **Sending images**: on macOS, copy an image (screenshot etc.) and paste (Cmd+V) to attach it;
  the input shows a `#[image N]` placeholder; dragging/pasting image file paths (as their own line or
  `![alt](path)`) attaches on submit too. Message history keeps the placeholder text, and the image goes to the model as a
  base64 content block alongside it (auto-compressed to 2000px / ~3.75MB). Both wire protocols carry image blocks, so this
  works by default; `sendImages: false` (default endpoint) or `supportsImages: false` (named provider) opts out an endpoint
  that speaks the protocol but rejects images, and then only the text is sent. The attachment table belongs to the session,
  not to the input box: any subagent resolves the same `#[image N]` marker, so an opted-out session can still get an image
  looked at by forking one onto a provider that accepts them.

## Config guide (settings.json)

Three config layers, shallow-merged; the later one overrides:
1. **user**: `~/.config/bingo/settings.json` (`XDG_CONFIG_HOME` takes precedence)
2. **project**: `.bingo/settings.json`
3. **local**: `.bingo/local.json` (personal overrides, never committed)

| Setting | Type | Description |
|---|---|---|
| `apiKey` | string | API key（settings 优先于 `ANTHROPIC_API_KEY`/`DEEPSEEK_API_KEY`）；建议放 user 层，项目层会入库 |
| `apiBaseUrl` | string | API 端点（settings 优先于 `ANTHROPIC_BASE_URL`；缺省官方） |
| `providers` | object | Named providers: `{name: {protocol?, apiKey, apiBaseUrl?, supportsImages?, oauth?}}`, switch via `/provider <name>`; `protocol` is `"anthropic"` (default) or `"openai"` (Responses API, `Authorization: Bearer`; `apiBaseUrl` defaults to `https://api.openai.com`); an empty/absent `apiBaseUrl` falls back to the protocol default; unknown protocols are a config error at startup. `oauth: {kind: "codex"}` enables OAuth login (apiKey wins over OAuth); the codex flow (device / loopback PKCE) is `chatgpt.com`-subscription auth, tokens stored in `~/.local/share/bingo/auth.json` (0600, never in the committed settings) |
| `provider` | string | Current provider (persisted by `/provider` and the /model menu; default `"default"` = top-level `apiKey`/`apiBaseUrl`); restored at startup, an invalid name falls back to default with a warning |
| `sendImages` | bool | Whether the default endpoint sends message-box image attachments to the model (named providers use their own `supportsImages`). Both protocols carry image blocks, so this defaults to **true** — set `false` to opt out an endpoint that speaks the protocol but rejects images (some compat proxies) |
| `thinkingLevel` | string | Thinking level: `off` sends no thinking param (DeepSeek-compatible, default); `low`/`medium`/`high`/`xhigh`/`max` send `{"type":"adaptive"}` adaptive thinking plus `output_config.effort` (the Claude 5 family removed budget_tokens; below `high` saves tokens, `xhigh`/`max` think deeper) |
| `permissionMode` | string | `default` / `acceptEdits` / `plan` / `dontAsk` / `bypassPermissions` |
| `theme` | string | `auto` (follows the terminal background) / `dark` / `light` |
| `motion` | string | TUI motion: `auto` (default) / `off`——动效（如欢迎卡更新提示呼吸）静止为基色，提示本身保留；env `BINGO_NO_MOTION=1` 等价 |
| `cacheControl` | bool | Send prompt caching; turn off if a non-official endpoint is unstable |
| `respondToBashCommands` | bool | Whether `!` commands are handed to the model for a response after execution (default true; false = pure execution) |
| `shell` | string | Shell program for the Bash tool and hooks; default per platform: macOS `/bin/zsh`, other Unix `/bin/bash`, Windows `powershell.exe`. PowerShell-family shells run with `-Command`; other configured shells (e.g. Git Bash `bash.exe`) with `-c` |
| `mcpServers` | object | `{name: {type?, command, args, env}}` (stdio, default) or `{name: {type: "http", url, headers?}}` (streamable HTTP) |
| `disabledMcpServers` | string[] | List of disabled MCP servers (written by `/mcp disable`) |
| `permissions` | object | `{allow[], deny[], ask[]}`; rule syntax `Tool(content)`, `:*` is a prefix wildcard (e.g. `Bash(git push:*)`); Bash rules match per subcommand segment; path rules normalize before matching (see diagnostics 4) |
| `experimental` | object | Experimental features: `{"agentChannels": true}` enables agent channel messaging (the main session gets the Channel/Post tools, direct subagents get Post); `channelMessageLimit` (default 500, freezes the channel when exceeded) / `agentMessageLimit` (default 50) are budget gates |
| `team` | object | agent team startup behavior: `{"autoStart": true}` (default true = when a project-bound team exists, start it automatically at launch; members stand by Idle at zero tokens; `--no-team` or false turns it off) |
| `hooks` | object | PreToolUse/PostToolUse/PreCompact/PostCompact/UserPromptSubmit/Stop/SessionStart/SessionEnd/TaskCreated/TaskCompleted, matcher + command; the matcher is a whole-string anchored regex (`Edit\|Write`, `mcp__.*`); invalid regexes fall back to exact matching |

Example (.bingo/settings.json):
```json
{
  "apiKey": "sk-ant-xxxx",
  "apiBaseUrl": "https://api.anthropic.com",
  "providers": {
    "deepseek": { "apiKey": "sk-ds", "apiBaseUrl": "https://api.deepseek.com" },
    "local": { "apiKey": "sk-any", "apiBaseUrl": "http://127.0.0.1:11434/v1" },
    "openai": { "protocol": "openai", "apiKey": "sk-...", "apiBaseUrl": "https://api.openai.com" }
  },
  "provider": "deepseek",
  "thinkingLevel": "medium",
  "permissionMode": "acceptEdits",
  "mcpServers": {
    "files": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "."] },
    "remote": { "type": "http", "url": "https://mcp.example.com/mcp", "headers": { "Authorization": "Bearer xxx" } }
  },
  "permissions": { "deny": ["Bash(git push:*)"] }
}
```

## Slash command quick reference

`/help` for the full list. Common ones: `/model [name]` (no args: two-level picker — level 1 providers → level 2 model list; with a name: switch directly, validated against the known list when available),
`/provider [name]` (no args: picker — ● current, s = session-only, Enter persists; with a name: switch directly), `/provider login <name> [--device-auth|--manual <token>]` (OAuth login: default opens the browser; `--device-auth` prints URL + code and polls for headless/SSH; `--manual` stores a pasted token), `/provider logout <name>` (revokes + clears),
`/think [off|low|medium|high|xhigh|max]`（思考级别，持久化 settings；无参打开档位选择器：●=当前生效、↑↓/1-6 浏览、Enter 确认、Esc 取消）、`/theme [dark|light|auto]`（无参打开档位选择器，`/theme auto` 显式快捷保留）、
`/permissions [allow|deny|ask] [规则]`、
`/mcp`（状态）· `/mcp enable|disable [name|all]` · `/mcp reconnect <name>`、
`/skills`（清单，`/技能名` 直接执行）· `/context`（用量）· `/status` ·
`/compact`（强制压缩）· `/resume [名称]`（恢复历史会话；无参打开会话选择器，Enter 即恢复）· `/rename` · `/clear` · `/exit`。
`/team`（项目级编队）：`list`（图纸+运行区同屏）· `start`（拉起/幂等复用）· `status` ·
`assign <成员> <任务>`（派活）· `stop` · `validate` · `new`（脚手架生成 team.json）·
`memory list|gc`（跨会话记忆管理）。

## Diagnostic guide (common problems → troubleshooting paths)


1. **No credentials**: bingo still starts — the welcome card shows onboarding.
   Either `/provider login codex` (ChatGPT subscription; the device code / auth
   URL stays pinned on screen until the flow finishes), or set
   `ANTHROPIC_API_KEY`/`DEEPSEEK_API_KEY`, or write `apiKey` in settings.json
   (settings take precedence). `/config` shows which source won and the
   current endpoint.
2. **Model request fails/times out**: `/status` shows the current model; `/model` switches it; with multiple providers use
   `/provider <name>` (the settings `providers` section); `/context` shows usage —
   when close to the context window, `/compact` (auto-compaction threshold = 90% of the effective window
   (200k − 64k output budget) ≈ 122k, about 61% of the total window). Endpoints without a
   count_tokens API (DeepSeek/ollama; OpenAI-protocol providers — `count_tokens` is Anthropic-only)
   automatically fall back to local estimation (characters/4), with a one-time warning on first fallback.
3. **MCP server not working**: `/mcp` shows status — `✗ failed: <details>` fixes per the details
   (command missing/spawn failure/handshake failure; for http servers also check url reachability and headers auth);
   the stdio server's own error output lives in `~/.local/share/bingo/logs/mcp-<name>.log`
   (rewritten per connection; never printed into the UI); after fixing, `/mcp reconnect <name>`. `type: sse/ws` errors with "unsupported (stdio / http)".
   Disable/enable: `/mcp disable|enable [name|all]`
   (the disabled list persists to settings.json). MCP tool names are `mcp__<server>__<tool>`;
   use the full name in permission rules (the transcript tool row shows `◆ server:tool`, which is only a display alias).
   Connections run in the background and never block turn input;
   connection-failure notices above the input box auto-expire after about 10s;
   details are authoritative in the `/mcp` status.
4. **Permission prompts/denials not as expected**: `/permissions` lists the current rules; rule syntax is
   `Tool(content)`; `:*` is a prefix wildcard (e.g. `Bash(git push:*)`). Bash rules split the command on
   shell operators (`&&` `;` `|` etc.) into subcommands and match each segment: if any subcommand hits deny/ask, it takes effect;
   allow requires a **single rule covering all subcommands** to skip the prompt; commands containing `$()`/subshells/
   unclosed quotes are never auto-allowed (`Bash(git log)` allows `git log` but not
   `git log | head`). File rules normalize paths before matching (`~` expansion, relative paths expanded against
   cwd, `..` resolved), so `Read(src/)` also matches absolute paths under cwd. MCP tools
   don't skip the prompt just because the server reports read-only; they need an explicit allow (`mcp__server` or
   `mcp__server__tool`). Edit `permissions.allow/deny/ask` or switch
   `permissionMode` (bypassPermissions allows everything; plan is read-only).
5. **`!` command rejected**: interactive/TTY commands (top/vim/ssh/sudo -i/fzf etc.) are rejected by design —
   use non-interactive equivalents (`top -b -n 1`, `ssh host 'cmd'`).
6. **Stuck in bash mode/accidental trigger**: with an empty input, Esc/backspace/Ctrl+U all exit bash mode;
   with non-empty input `!` is an ordinary character; Tab completes from this session's `!` history prefix.
7. **Can't find a historical session**: transcripts live in `~/.local/share/bingo/transcripts` (`--continue`
   resumes the last one; `/resume` lists/switches).
8. **Tool output collapsed**: ctrl+o expands all collapsed items and replays the full transcript to the
   terminal (scroll up to read; printed old collapsed copies stay higher up — normal); pressing ctrl+o again in the fully expanded
   state collapses — back to aggregates with a clear/consolidate; long output shows `+N lines`.
9. **Slash dropdown doesn't have the command you want**: type a prefix to filter (e.g. `/m` matches mcp/model/meye);
   Esc closes the menu; skills are listed in `/skills`, run with `/skill-name`.
10. **Grep/Glob finds nothing**: `.git`/`target`/`node_modules` and dot-prefixed directories are skipped by default
    (they still search when `path` points at them explicitly); patterns are relative to the search root
    (`src/**/*.rs` works); patterns without `/` match file names at any depth
    (`*.rs` hits the whole tree); traversal stops when the result cap is reached.
11. **Processes left behind after timeout/interruption**: Bash commands run in their own process group; timeouts and cancellation
    terminate the whole group (grandchildren no longer orphan); after Esc interrupts a turn, unfinished tools get placeholder results,
    and the session stays recoverable (no 400s on later requests from orphaned tool_use).

## Capability map (reference when asked "what can bingo do")

- **Built-in tools**: Bash (through the permission gate), Read/Glob/Grep (Read returns image files as
  viewable images, so screenshots and rendered charts can be inspected), Edit/Write, WebFetch/WebSearch,
  Agent (subagents), SendMessage/AgentControl (subagent continuation and lifecycle, main session only),
  the Task family (task tracking), AskUserQuestion (main session only — a subagent has no prompt surface),
  Skill (skill invocation),
  ExperiencePropose/Commit/Query/Outcome/Forget (project experience capture, retrieval, and verified-use feedback).
- **Provider protocols**: anthropic (Messages API, default — all existing configs) and openai (Responses API,
  per named provider via `protocol: "openai"` in the settings `providers` section; bearer auth, `reasoning.effort`
  for thinking levels, no count_tokens endpoint → local-estimation fallback). The top-level `apiKey`/`apiBaseUrl`
  always form the anthropic "default" provider; subagent cross-provider rules apply across protocols
  (explicit `model` required when forking to a different provider). opencode-go (subscription) lands as
  `{"protocol": "openai", "apiKey": "<go-key>", "apiBaseUrl": "https://opencode.ai/zen/go"}` — its Responses
  models (e.g. gpt-5.6-luna) work through the openai adapter; its chat/completions models need an adapter
  that is not implemented yet; its anthropic-protocol models can be added as a separate provider entry.
- **Built-in provider presets (zero-config)**: official subscriptions ship inside bingo — `codex` (ChatGPT,
  `protocol: openai` + `oauth.kind: codex` → chatgpt.com/backend-api/codex/responses) and `opencode-go`
  (`protocol: openai` + apiKey → opencode.ai/zen/go) are visible in `/provider` (内置 badge) and loginable with
  no settings entry (`/provider login codex` / `opencode-go --manual <key>`); user `providers.<name>` entries
  override the preset field-by-field (e.g. only `apiBaseUrl` to customize).
- **Provider OAuth (codex/ChatGPT)**: `oauth: {kind: "codex"}` on a named provider enables subscription login —
  `/provider login <name>` (default: opens the browser with loopback PKCE; `--device-auth` prints a URL + one-time
  code and polls for headless/SSH; `--manual <token>` pastes a stored token), `/provider logout <name>` revokes and
  clears. Tokens live in `~/.local/share/bingo/auth.json` (0600, opencode-compatible shape) — never in the committed
  settings; `apiKey` in settings wins over OAuth; refresh is automatic (eager 5 min before expiry + on 401),
  permanent refresh failures clear the login and prompt `/provider login <name>` again. Codex providers route to
  `https://chatgpt.com/backend-api/codex/responses` (Responses wire format, same adapter; `ChatGPT-Account-Id`
  header from the JWT claims; `/model` shows the subscription allowlist: gpt-5.5 / gpt-5.3-codex-spark /
  gpt-5.4 / gpt-5.4-mini).
- **Experience**: reuses rerunnable workflows across sessions. At session start, this project's active
  experience index is injected (≤10 entries, ranked by observed outcomes before the legacy commit count; nothing injected when empty); full text is searched with ExperienceQuery by
  trigger tokens (case-insensitive, shared-prefix tolerant; active first);
  ExperiencePropose generates candidates (not persisted); after user confirmation ExperienceCommit persists
  (same content → stable id, re-committing updates rather than duplicates; `status: stale` marks invalidation,
  exits injection but stays queryable). After actually applying a queried entry, ExperienceOutcome records a
  permission-confirmed `helpful` or `harmful` result with concrete evidence; it appends outcome history and
  never changes lifecycle status or `verified_at` automatically. ExperienceForget evicts (requires user confirmation). Stored in
  `~/.config/bingo/experience/<project-key>/entries/` (user-level, not in the project repo);
  the project key is derived from the git remote URL (normalized) → git root → normalized absolute path, stable across directories/machines.
- **Subagents**: instances spawned by Agent have names (the `name` arg, defaulting to the definition name/agent; name collisions
  auto-suffix -2/-3), shown in the transcript as `◉ name · task`; history is kept after completion, and the main agent can
  SendMessage to continue, or manage with AgentControl list/messages/stop/delete.
  **Messaging**: SendMessage returns a `message_id` and only queues — delivery happens at the turn boundary, where
  every message sent to the same instance in that turn is folded into one prompt (the receiver reads them together
  rather than one per turn). Queued is not an acknowledgement: `AgentControl(action=messages, agent=…)` reports each
  message as delivered (with the run it landed in), still queued (with its age), or dropped because the instance was
  stopped. Stopping or deleting an instance discards its inbox and says how many undelivered messages died with it.
  A run chain that fails leaves its queued messages in place — the next turn boundary retries them.
  **Delivered is not answered**: an instance can read a message and end its turn without a word, which from the outside
  looks the same as a hang. The acknowledgement is the reply, so `messages` reports four states — queued, delivered but
  unanswered, answered (naming the run that spoke), dropped — and a turn that produces text answers everything that
  instance had already read, even messages first read during an earlier silent run.
  **Chasing a reply**: the harness does that polling for you, and it is on by default — every SendMessage arms a 300s
  check unless told otherwise. Once the wait elapses it re-reads the same record; while the sender is still owed an
  answer it puts a follow-up in the receiver's inbox (naming which silence it is, and asking for a reply rather than
  repeating the instruction) and retries the boundary flush, at most 3 rounds. An answer inside the wait is silent;
  anything else — chased into replying, dropped, or still quiet after the last round — comes back as a task
  notification. `ack_timeout: <seconds>` tunes the wait (5-3600: shorter when actively waiting, longer for a task that
  will be quiet for a while), and `ack_timeout: 0` switches the check off for a message needing no answer.
  **Images to subagents**: repeat an `#[image N]` marker in the Agent prompt or SendMessage text; the attachment table
  belongs to the session, so the subagent receives the actual image (also carried along if the message has to queue).
  This also works *out* of a text-only session: when the current endpoint cannot receive images, fork a subagent onto an
  image-capable provider (`Agent(provider: …, model: …)` — crossing providers requires an explicit model) and repeat the
  marker; resolution is independent of endpoint capability, so the subagent sees the real image and reports back. A
  placeholder that arrives without its image now says which case it is (endpoint cannot carry images — with the capable
  providers listed — versus the attachment being gone from a resumed session) instead of leaving the model to hunt for a
  file that was never on disk.
  **Named definitions**: `~/.config/bingo/agents/*.md` and `.bingo/agents/*.md` (same-name project layer wins);
  frontmatter `name/description/model/provider/thinking/inherit_system`, body = the subagent's system prompt; referenced by
  the Agent tool's `agent` argument. The body is appended to the parent's system blocks by default; `inherit_system: false`
  replaces them instead, which also drops the environment info, CLAUDE.md/AGENTS.md and project memory.
  **What a subagent shares with the main session**: MCP connections and the permission-rule table are shared handles
  (a subagent gets the same MCP tools; `/permissions` edits reach running instances), and permission prompts are
  forwarded to the main session's modal — a subagent never has a tool call silently auto-denied. What it does not get:
  AskUserQuestion, and being woken by background-task notifications (its result goes back to the hub instead).
  **Per-instance model/thinking**: the Agent tool's `model`/`provider`/`thinking` args give a single
  subagent a model, provider (the settings `providers` section; cross-endpoint/cross-key), and thinking level
  (`off/low/medium/high/xhigh/max`); precedence: explicit args > named definition > inherit the parent session's
  current values (model/provider/thinking are independent and don't affect the parent session). **Cross-provider
  boundary**: when forking to a provider different from the parent session's current one, the parent model and
  thinking level are NOT inherited — `model` must be explicit (early failure "provider X requires a model"),
  `thinking` defaults to off (no parameter sent, compatible with DeepSeek/Ollama endpoints); `provider` `"default"`
  or omitted = shared parent endpoint (follows parent switches, same as the /model menu); same provider keeps
  inheriting. Invalid `thinking` values are rejected with the allowed list.
  **Channel messaging** (experimental, `experimental.agentChannels`): the main agent uses the Channel tool
  to create channels and manage members (members limited to direct subagents; the main agent is auto-seated as `main`), members speak
  via Post — messages enter every member's context (same order), the sender is stamped by the runtime; in serial channels, a stale
  post is bounced back with the new messages attached (the agent reads them, then re-decides/abandons; count-based ordering emerges this way);
  free channels allow interleaving. Channels show in the transcript as `◇ #name` rows (expandable to the full group chat);
  over-budget channels auto-freeze and notify the main agent. **Silence is the default**: because delivery wakes every member, each
  spawned member carries a system-prompt rule (only when the flag is on) forbidding greetings, acknowledgements and "got it" posts —
  they are what turns one message into a room-wide storm. It lives in the system block on purpose: compaction rewrites the history
  but never touches the system prompt, so the rule survives a long-running member's context being summarised away.
  **Bottom entity area**: when instances/channels exist, a one-line summary shows above the input box; Ctrl+G enters selection
  (↑↓/Enter), and Enter opens the fullscreen **Slack-shaped workspace**: a rail (Home / DMs / Activity), a sidebar listing
  `#channels` and DM instances with presence dots and unread badges, and a conversation pane rendering a Slack message list
  (day dividers, avatar + bold sender + timestamp, grouped consecutive messages, an unread divider, tool calls as attachments,
  a running instance's live tail as a typing indicator). The composer sends: in a channel it posts as `user` (same delivery
  path as Post, members woken normally; rendering = read, so serial never bounces you), in a DM it queues on the instance and
  flushes at the turn boundary (shown as a pending message until then). Keys: Tab cycles panes, alt+↑↓ switches conversation,
  Ctrl+K is the quick switcher, ←→ fold sidebar sections, Esc returns.
- **agent team** (project-scoped roster): `.bingo/team.json` (camelCase: `name`/`channel{mode,messageLimit}`/
  `members[{name,agent}]`, members reference AgentDefs) pins multiple roles to one project; started by default at launch
  (`settings.team.autoStart`; `--no-team` turns it off; starting ≠ waking — members stand by Idle at zero tokens,
  only `/team assign` or channel messages start them; idempotency key = instance name, repeated start reuses). The `/team` command family
  manages it; team memory is keyed by "project path hash + branch" in `~/.config/bingo/teams/` (full history restored across sessions +
  append-only decision records; `/team memory list|gc` manages it).
- **Skills**: built-in `guide` (this guide) + `~/.config/bingo/skills/` and `.bingo/skills/`
  directory skills (same-name disk skills override built-ins); the model invokes them via SkillTool, users run them via `/skill-name`.
- **Images**: markdown images in model replies (`![alt](path)`, supports `~/`, relative paths/data/http(s))
  render via kitty Unicode placeholders (U=1) on terminals that support them (Ghostty/kitty), in both
  modes and everywhere at once: the live viewport, fullscreen, and `--inline` scrollback all paint the
  same placeholder cells the moment the image loads — no waiting for the message to settle. Inside tmux,
  bingo enables passthrough automatically (`tmux set -p allow-passthrough on`) and the same rendering
  works when the outer terminal is Ghostty/kitty; the startup probe needs the pane to be focused.
  WezTerm/Konsole (kitty graphics without U=1) and other terminals show the `#[image]` text placeholder
  with a one-time notice. A failed fetch (network error, 4xx/5xx, undecodable data) marks the row as
  `#[image ✗ 加载失败]` and shows a warning line with the url.
- **MCP**: stdio and streamable HTTP (`type: "http"`, with custom headers) server tools are integrated (see above).
- **Memory**: memdir auto-memory (`~/.config/bingo/memdir/`, filenames
  `<project-name>-<path-hash>.md`, same-name directories don't cross-pollute) + project CLAUDE.md (Anthropic convention).
- **Sessions**: transcripts persisted (JSONL), `--continue`/`/resume` restore, `/compact` compacts.
- **内置工具**：Bash（经权限门）、Read/Glob/Grep（Read 对图片文件返回可看的图像，
  截图与渲染图表可直接查看）、Edit/Write、WebFetch/WebSearch、
  Agent（子代理）、SendMessage/AgentControl（子代理续话与生命周期，仅主会话）、
  Task 族（任务追踪）、AskUserQuestion（仅主会话——子代理没有提问界面）、Skill（技能调用）、
  ExperiencePropose/Commit/Query/Outcome/Forget（项目经验沉淀、检索与验证后反馈）。
- **经验（Experience）**：跨会话复用可重跑的工作流。会话开始时注入本项目
  active 经验索引（≤10 条，显式观察结果优先于旧重复提交计数，空则不注入），全文用 ExperienceQuery 按 trigger
  词元检索（大小写不敏感、共享前缀容错、active 优先）；
  ExperiencePropose 生成候选（不落盘），用户确认后 ExperienceCommit 落盘
  （同内容稳定 id，重提交更新而非重复；status: stale 标记失效后退出注入但仍可查）。
  真正采用查询结果后，ExperienceOutcome 经权限确认记录 `helpful` 或 `harmful` 与具体证据；它仅追加结果历史，绝不自动改变生命周期 `status` 或 `verified_at`。
  ExperienceForget 淘汰（须用户确认）。存储于
  `~/.config/bingo/experience/<project-key>/entries/`（用户级、不进项目仓库），
  项目键取 git remote URL（归一化）→ git 根 → 规范化绝对路径，跨目录/机器稳定。
- **子代理**：Agent 派生的实例有名字（`name` 参数，缺省取定义名/agent，重名
  自动 -2/-3），transcript 显示为 `◉ 名字 · 任务`；完成后历史保留，主 agent 可
  SendMessage 续话、AgentControl list/messages/stop/delete 管理。
  **消息机制**：SendMessage 返回 `message_id` 且只入队，投递发生在回合边界——同一回合发给同一实例的
  多条消息合并成一个 prompt 一次性送达，而不是一条一轮。排队不等于回执：
  `AgentControl(action=messages, agent=…)` 逐条报告已送达（附落在第几轮）、仍排队（附等待时长）、
  或因实例停止而丢弃。stop/delete 会清空信箱并报告有多少条未送达指令随之丢弃；
  运行链失败时消息留在信箱，下一个回合边界重投。
  **送达不等于回复**：实例完全可以读了消息、跑完一轮一声不吭，从外面看和卡死没区别。回执以「回复」为准，
  所以 `messages` 报四种状态——排队、已读但没回、已回复（标明是第几轮开的口）、已丢弃；某一轮只要产出了文本，
  就把该实例此前读过的消息一并算作已回复（包括在更早那轮沉默中读到的）。
  **自动追回复**：这轮复查由系统代劳，且**默认开启**——每条 SendMessage 都挂一个 300s 的检查，除非显式关掉。
  等待到点后重读同一份回执，只要还欠发件人一个回复，就往收件人信箱放一条追问（讲明是哪种沉默，且只要回复、
  不重发原指令）并重试边界投递，最多 3 轮。等待内回复则全程静默；被追问后才开口、被丢弃、或最后一轮仍不吭声，
  都以任务通知回报主 agent。`ack_timeout: <秒>` 调整等待（5-3600：正等着它就调短，明知要安静干很久就调长），
  `ack_timeout: 0` 则对这条消息关闭检查。
  **给子代理传图**：在 Agent prompt 或 SendMessage 文本里复述 `#[image N]` 占位即可——附件表属于会话，
  子代理收到的是真图片（消息排队时图片一同携带）。这条路也能从**不收图的会话往外走**：当前端点不接受图片时，
  把子代理 fork 到支持图片的 provider（`Agent(provider: …, model: …)`，跨 provider 必须显式给 model）并复述占位，
  解析不受端点能力影响，子代理看得到真图并回报结论。占位符到达而图片不在时会说明是哪种情况
  （端点不收图——并列出可用 provider——还是恢复会话导致附件已不在），而不是让模型去找一个从未落盘的文件。
  **具名定义**：`~/.config/bingo/agents/*.md` 与 `.bingo/agents/*.md`（同名项目层
  优先）；frontmatter `name/description/model/provider/thinking/inherit_system`，正文 = 子代理
  system prompt；Agent 工具的 `agent` 参数引用。
  **逐实例模型/思考**：Agent 工具的 `model`/`provider`/`thinking` 参数可给单个
  子代理指定模型、provider（settings 的 providers 段，跨端点/跨 key）与思考级别
  （`off/low/medium/high/xhigh/max`）；优先级 显式参数 > 具名定义 > 继承父会话
  当前值（模型/provider/思考各自独立，互不影响父会话）。
  **频道互发**（实验，`experimental.agentChannels`）：主 agent 用 Channel 工具
  建频道/进出成员（成员限直接子代理，主 agent 名 `main` 自动入席），成员用 Post
  发言——消息进全体成员上下文（同序），发件人由运行时盖戳；serial 频道落后
  发言会被弹回并附新增消息（agent 阅读后自行改口/放弃，报数式顺序由此涌现），
  free 频道允许交叉。频道在 transcript 显示为 `◇ #名字` 行（可展开看完整群聊）；
  预算超限自动冻结频道并通知主 agent。**默认沉默**：投递即唤醒全体成员，所以每个派生成员都带一条
  system prompt 规则（仅在开关打开时注入）——禁止发问候、寒暄、「收到」这类消息，正是它们把一条消息
  滚成满屋噪声。规则放在 system block 而非唤醒载荷里：compact 重写消息历史但从不碰 system prompt，
  长跑成员的上下文被摘要掉之后这条规则仍在。
  **底部实体区**：有实例/频道时输入框上方显示一行摘要，Ctrl+G 进入选择
  （↑↓/Enter），Enter 打开全屏 **Slack 式工作区**：左侧 rail（主页/私信/动态）、
  侧栏列出 `#频道` 与私信实例（在线点 + 未读徽标）、右侧 Slack 式消息流（日期分隔、
  头像 + 粗体发件人 + 时间、连发合并、新消息分隔线、工具调用作附件、运行中实例的
  活尾作「正在输入」）。底部输入框发送：频道以 `user` 身份发言（与 Post 同一投递
  路径，正常唤醒成员；渲染即已读，serial 不会弹你），私信排进实例收件箱、回合边界
  投递（投递前显示为待送达）。按键：Tab 切焦点、alt+↑↓ 换会话、Ctrl+K 快速跳转、
  ←→ 折叠侧栏分组、Esc 返回。
- **agent team**（项目级编队）：`.bingo/team.json`（camelCase：`name`/`channel{mode,messageLimit}`/
  `members[{name,agent}]`，成员引用 AgentDef）把多名角色固定到一个项目；启动默认拉起
  （`settings.team.autoStart`，`--no-team` 关闭；拉起 ≠ 唤醒——成员 Idle 待命零 token，
  等 `/team assign` 或频道消息才开跑；幂等键 = 实例名，重复 start 复用）。`/team` 命令族
  管理；team 记忆按「项目路径哈希 + 分支」存 `~/.config/bingo/teams/`（完整历史跨会话恢复 +
  append-only 决策记录，`/team memory list|gc` 管理）。
- **技能**：内置 `guide`（本指南）+ `~/.config/bingo/skills/` 与 `.bingo/skills/`
  目录技能（同名磁盘技能覆盖内置）；模型经 SkillTool 调用，用户经 `/技能名` 执行。
- **图片**：模型回复中的 markdown 图片（`![alt](路径)`，支持相对路径/data/http(s)）
  在支持 kitty graphics 的终端（Ghostty/kitty/WezTerm 等）内联渲染，其余终端显示
  `#[image]` 占位。tmux 内 bingo 会自动开启 passthrough（`tmux set -p
  allow-passthrough on`），外层终端为 Ghostty/kitty 时经 Unicode 占位符（U=1）
  渲染，图片随文本正常滚动；外层为 WezTerm/Konsole（不支持 U=1）或无法识别时
  显示 `#[image]` 占位并提示一次。图片随消息自动加载并在消息定稿落盘时渲染，
  不需要额外命令。抓取失败（网络错误、4xx/5xx、数据不可解码）时该行显示
  `#[image ✗ 加载失败]`，并有警告行给出 url。
- **MCP**：stdio 与 streamable HTTP（`type: "http"`，可带自定义 headers）服务器工具接入（见上）。
- **记忆**：memdir 自动记忆（`~/.config/bingo/memdir/`，文件名
  `<项目名>-<路径哈希>.md`，同名目录不串味）+ 项目 CLAUDE.md（Anthropic 惯例）。
- **会话**：transcript 持久化（JSONL），`--continue`/`/resume` 恢复，`/compact` 压缩。
  **分享**：`bingo share [会话]` 默认在当前目录生成自包含 HTML 文件（`--output`
  指定路径），不会联网。只有显式加 `--public` 才上传官网分享服务并打印
  `https://bingo.ruobin.dev/share/u/<id>` 公网链接；**任何人可公开访问**，因此
  bingo 会在上传开始前提示完整对话/工具输出可能含敏感信息。`--open` 打开本地
  文件或已发布链接。settings `share.baseUrl` 可覆盖服务基址（缺省
  `https://bingo.ruobin.dev`），上传失败自动回退本地文件。会话内
  `/share [--public] [--open]` 采用相同安全语义：默认本地，`--public` 才上传。
  会话 key 与 `/resume` 同语义（transcript stem 或可匹配片段，缺省最近会话）。
  **更新**：`bingo update` 从 GitHub Releases（yexrob/bingo）拉取最新版并原子替换当前
  可执行文件——平台资产（`bingo-<triple>.tar.gz` / `.zip`）+ `checksums.txt` SHA-256
  校验，解压后同目录 tmp + rename 替换（Unix 保留可执行位）；`--check` 只检测不下载。
  输出：已是最新 / 发现新版本（`--check`）/ 更新成功（新版本号 + 安装位置）；
  失败给出原因（网络 / 校验失败 / 无权限——提示 sudo 或手动安装）。
  启动时 TUI 后台异步检测新版本（`~/.local/share/bingo/update-check.json` 24h TTL 缓存，
  失败静默、不阻塞启动；`--print` headless 不触发），检测到新版本时欢迎卡显示
  「New version vX.Y.Z available — run bingo update」提示行（版本号与命令两段呼吸
  9s 后静止常驻，按键可提前静止；`motion: "off"` 或 `BINGO_NO_MOTION=1` 静态显示）。
