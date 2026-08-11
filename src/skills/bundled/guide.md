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
  aggregates and clear/consolidate) · Ctrl+T toggle the task area · Ctrl+G opens the fullscreen team/DM workspace directly (Ctrl+K switches channels and DMs; the DM view shows user/agent text while hiding tool activity; channel rooms let you speak directly as user) · Ctrl+B manages running background agents · Ctrl+L clear and redraw · Shift+Tab cycles permission
  modes (default → acceptEdits → plan) · Alt+T thinking toggle (off ↔ the last non-off level, default medium) · while busy, Enter queues the message (sent automatically at turn end; /think /model /provider /theme /status /context /tasks /help /skills run immediately) ·
- During streamed output, the main footer shows a live `N tok/s` indicator; the speed band changes its character animation and cadence, and idle/stalled output hides it. Beside it, context usage stays visible as a four-cell bar, percentage, and `used/window` token count, using the active model's window; 70–90% is warning-colored and above 90% is danger-colored. A running instance's DM composer shows the same indicators. `motion: "off"` freezes the rate frame but keeps the value.
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
| `apiKey` | string | API key (settings take precedence over `ANTHROPIC_API_KEY`/`DEEPSEEK_API_KEY`); prefer the user layer — the project layer gets committed to the repo |
| `apiBaseUrl` | string | API endpoint (settings take precedence over `ANTHROPIC_BASE_URL`; defaults to the official one) |
| `providers` | object | Named providers: `{name: {protocol?, apiKey, apiBaseUrl?, supportsImages?, oauth?}}`, switch via `/provider <name>`; `protocol` is `"anthropic"` (default) or `"openai"` (Responses API, `Authorization: Bearer`; `apiBaseUrl` defaults to `https://api.openai.com`); an empty/absent `apiBaseUrl` falls back to the protocol default; unknown protocols are a config error at startup. `oauth: {kind: "codex"}` enables OAuth login (apiKey wins over OAuth); the codex flow (device / loopback PKCE) is `chatgpt.com`-subscription auth, tokens stored in `~/.local/share/bingo/auth.json` (0600, never in the committed settings) |
| `provider` | string | Current provider (persisted by `/provider` and the /model menu; default `"default"` = top-level `apiKey`/`apiBaseUrl`); restored at startup, an invalid name falls back to default with a warning |
| `sendImages` | bool | Whether the default endpoint sends message-box image attachments to the model (named providers use their own `supportsImages`). Both protocols carry image blocks, so this defaults to **true** — set `false` to opt out an endpoint that speaks the protocol but rejects images (some compat proxies) |
| `thinkingLevel` | string | Thinking level: `off` sends no thinking param (DeepSeek-compatible, default); `low`/`medium`/`high`/`xhigh`/`max` send `{"type":"adaptive"}` adaptive thinking plus `output_config.effort` (the Claude 5 family removed budget_tokens; below `high` saves tokens, `xhigh`/`max` think deeper) |
| `permissionMode` | string | `default` / `acceptEdits` / `plan` / `dontAsk` / `bypassPermissions` |
| `theme` | string | `auto` (follows the terminal background) / `dark` / `light` |
| `motion` | string | TUI motion: `auto` (default) / `off` — motion (e.g. the welcome-card update notice breathing) settles to the base color while the notice itself stays; env `BINGO_NO_MOTION=1` is equivalent |
| `cacheControl` | bool | Send prompt caching; turn off if a non-official endpoint is unstable |
| `respondToBashCommands` | bool | Whether `!` commands are handed to the model for a response after execution (default true; false = pure execution) |
| `bashOutputMaxChars` | integer | Maximum combined stdout/stderr characters returned by the Bash tool (default and maximum 48,000); truncated results point to redirecting output and reading the file |
| `shell` | string | Shell program for the Bash tool and hooks; default per platform: macOS `/bin/zsh`, other Unix `/bin/bash`, Windows `powershell.exe`. PowerShell-family shells run with `-Command`; other configured shells (e.g. Git Bash `bash.exe`) with `-c` |
| `mcpServers` | object | `{name: {type?, command, args, env}}` (stdio, default) or `{name: {type: "http", url, headers?}}` (streamable HTTP) |
| `disabledMcpServers` | string[] | List of disabled MCP servers (written by `/mcp disable`) |
| `permissions` | object | `{allow[], deny[], ask[]}`; rule syntax `Tool(content)`, `:*` is a prefix wildcard (e.g. `Bash(git push:*)`); Bash rules match per subcommand segment; path rules normalize before matching (see diagnostics 4) |
| `experimental` | object | Experimental features: `{"agentChannels": true}` enables agent channel messaging (the main session gets the Channel/Post tools, direct subagents get Post); `channelMessageLimit` (default 500, freezes the channel when exceeded) / `agentMessageLimit` (default 50) are budget gates; `{"chatAvatars": true}` puts faces in the main chat (default false — no sender band, no portrait on a watch row; the workspace views wear theirs regardless) |
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
`/think [off|low|medium|high|xhigh|max]` (thinking level, persists to settings; no arg opens the level picker: ●=in effect, ↑↓/1-6 to browse, Enter confirms, Esc cancels), `/theme [dark|light|auto]` (no arg opens the level picker; the `/theme auto` explicit shortcut stays),
`/cd <dir>` (switch the working directory for this session; relative paths resolve from the current session directory),
`/permissions [allow|deny|ask] [rule]`,
`/mcp` (status) · `/mcp enable|disable [name|all]` · `/mcp reconnect <name>`,
`/skills` (listing; `/skill-name` runs it directly) · `/context` (usage) · `/status` ·
`/compact` (force compaction) · `/resume [name]` (restore a past session; no arg opens the session picker, Enter restores) · `/rename` · `/gc` (clean expired session storage; 30-day TTL, latest 100 inactive sessions, 24-hour activity grace) · `/clear` · `/exit`.
`/team` (project-level crew): `list` (blueprint + runtime on one screen) · `start` (pull up / idempotent reuse) · `status` ·
`assign <member> <task>` (dispatch work) · `stop` · `validate` · `new` (scaffolds team.json + team-norms.md) ·
`norms` (the crew's working agreement) · `memory list|gc` (cross-session memory management).

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
   If that estimate is low and either wire protocol rejects a request as a context overflow, bingo compacts
   the history and retries the rejected request once; a second overflow ends the turn instead of looping.
   If an upstream response completes without assistant content or tool calls (including an unclosed thinking block),
   bingo treats it as malformed and retries the side-effect-free attempt once instead of ending silently; if the retry
   is also empty, the turn shows the normal full-flow retry/back error.
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
   resumes the last one; `/resume` lists/switches). Session storage is cleaned at startup with a 30-day TTL and a latest-100 inactive-session cap plus a 24-hour activity grace; matching share snapshots follow transcript removal. `/gc` applies the same policy on demand. Prompt-history files use the same TTL with a 100-file cap.
8. **Tool output collapsed**: ctrl+o expands all collapsed items and replays the full transcript to the
   terminal (scroll up to read; printed old collapsed copies stay higher up — normal); pressing ctrl+o again in the fully expanded
   state collapses — back to aggregates with a clear/consolidate; long output shows `+N lines`.
9. **Slash dropdown doesn't have the command you want**: type a prefix to filter (e.g. `/m` matches mcp/model/meye);
   Esc closes the menu; skills are listed in `/skills`, run with `/skill-name`.
10. **Grep/Glob finds nothing**: `.git`/`target`/`node_modules` and dot-prefixed directories are skipped by default
    (they still search when `path` points at them explicitly); patterns are relative to the search root
    (`src/**/*.rs` works); patterns without `/` match file names at any depth
    (`*.rs` hits the whole tree); traversal stops when the result cap is reached. Glob accepts `exclude` patterns and `max_depth`;
    Grep accepts `context`, case-insensitive/whole-word/fixed-string modes, and files-only results. Read accepts inclusive,
    1-based `start_line`/`end_line` ranges and reports ranges extending past the file.
11. **Processes left behind after timeout/interruption**: Bash commands run in their own process group; timeouts and cancellation
    terminate the whole group (grandchildren no longer orphan); after Esc interrupts a turn, unfinished tools get placeholder results,
    and the session stays recoverable (no 400s on later requests from orphaned tool_use).

## Capability map (reference when asked "what can bingo do")

- **Built-in tools**: Bash (through the permission gate, combined output capped by `bashOutputMaxChars`),
  Read/Glob/Grep (line ranges, exclusion/depth filters, search context/options; Read returns image files as
  viewable images, so screenshots and rendered charts can be inspected), Edit/Write, WebFetch/WebSearch,
  Agent (subagents), SendMessage/AgentControl (subagent continuation and lifecycle, main session only),
  Team (the project crew, main session only — reads are free, every change asks the user),
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
  (`protocol: openai` + apiKey → opencode.ai/zen/go) are visible in `/provider` (built-in badge) and loginable with
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
  SendMessage to continue, or manage with AgentControl list/messages/stop/delete; each list row includes relative last activity
  (`active now`, `active 3s ago`, `active 2min ago`) so a quiet idle instance is distinguishable from one that just finished.
  **Messaging**: SendMessage returns a `message_id` after enqueueing and dispatches immediately: an idle instance starts
  now, while a running instance drains its inbox between tool rounds. Everything waiting when the receiver drains is
  folded into one prompt. Queued is not an acknowledgement: `AgentControl(action=messages, agent=…)` reports each
  message as delivered (with the run it landed in), still queued (with its age), or dropped because the instance was
  stopped. Stopping or deleting an instance discards its inbox and says how many undelivered messages died with it.
  A run chain that fails leaves its queued messages in place — the recovery dispatcher retries them.
  **Delivered is not answered**: an instance can read a message and end its turn without a word, which from the outside
  looks the same as a hang. The acknowledgement is the reply, so `messages` reports four states — queued, delivered but
  unanswered, answered (naming the run that spoke), dropped — and a turn that produces text answers everything that
  instance had already read, even messages first read during an earlier silent run.
  **Chasing a reply**: the harness does that polling for you, and it is on by default — every SendMessage arms a 300s
  check unless told otherwise. Once the wait elapses it re-reads the same record; while the sender is still owed an
  answer it puts a follow-up in the receiver's inbox (naming which silence it is, and asking for a reply rather than
  repeating the instruction) and triggers the dispatcher again, at most 3 rounds. An answer inside the wait is silent;
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
  over-budget channels auto-freeze and notify the main agent. **Who spoke decides whether a reply is owed**: because delivery wakes every member, each
  spawned member carries a system-prompt rule (only when the flag is on) — answer `user`/`main` once and briefly when they address
  the room, owe another member nothing unless named or unblocked, and never answer an answer (replies to replies are what turn one
  message into a room-wide storm). The rule also states the mechanism the model cannot infer: a turn woken by a channel message
  reports to the hub, so **only Post puts words in the room** — a reply written as turn text reaches nobody in the channel. It lives in the system block on purpose: compaction rewrites the history
  but never touches the system prompt, so the rule survives a long-running member's context being summarised away.
  **Bottom entity area**: when running instances/channels exist, a one-line summary shows above the input box; idle/stopped agents stay out. Running rows include model/thinking/state. With an empty input, ↑/↓ selects running agents and Enter opens that agent's DM; Esc collapses the selector. Ctrl+G opens the fullscreen **Slack-shaped workspace** directly. Ctrl+B opens the main-view background-agent manager (↑/↓ select, Enter detail, x stop, Esc close); detail shows prompt/status/elapsed/tokens/tool count/recent tool activity, with no foreground action. The workspace is one conversation pane, full width, rendering a Slack
  message list (a header naming the channel/instance with the team's name at the right edge, day dividers, avatar + bold sender
  + timestamp, grouped consecutive messages, and an unread divider. DMs show user messages and agent text only—historical and live tool activity stays hidden—while reusing the main transcript's user bubbles, assistant markdown, prefixes, wrapping, and row structure; the existing DM name/avatar gutter stays unchanged. A working indicator remains during silent tool waits. DM headers show model/thinking; channel headers list model/thinking when the names fit and otherwise use one bounded aggregate, so the composer stays visible. There is no rail and no sidebar, and the view paints no background of its own — the terminal's own
  background shows through. Navigation is Ctrl+K (the quick switcher, which lists every conversation with its unread count)
  and alt+↑↓. **Avatars**: on terminals that can place kitty images (the same capability that renders inline images), each
  sender gets one of eight bundled anime-style portraits, 4×2 cells beside the name; elsewhere it falls back to the sender's
  initial on a colour, and the row count is identical either way. A team member's portrait is pinned in `.bingo/team.json`
  (`"avatar": "sora"`), so a crew keeps a fixed cast; everyone else gets a face derived from their name. The **main chat** wears
  the same faces behind `experimental.chatAvatars` (off by default): each message carries a band above it with the speaker's
  portrait and name (`main` for the hub, `You` for your own),
  two rows where portraits place and one where they fall back to the chip. Nothing below the band moves — bodies still run the full
  width. A terminal that purges its image store (a resize) redraws the faces still on screen; ones already in scrollback leave four
  blank columns with the name intact. Switched off, the transcript has no band and a subagent's watch row keeps its `◉` — the
  switch governs the main chat only, never these workspace views. Wake-up scaffolding the
  runtime injected (a relayed channel message, the task reminder) collapses to one dim line instead of being quoted as a message. The composer sends: in a channel it posts as `user` (same
  delivery path as Post, members woken normally; rendering = read, so serial never bounces you), in a DM it uses the same
  immediate dispatch path as SendMessage (shown as pending only until the receiver claims it). Keys: Tab switches between the message
  list and the composer, ↑↓ or the mouse wheel scrolls the transcript (three rows per wheel notch), alt+↑↓ switches conversation,
  Ctrl+K is the quick switcher, Esc returns.
- **agent team** (project-scoped roster): `.bingo/team.json` (camelCase: `name`/`channel{mode,messageLimit}`/`channels[{name,mode?,messageLimit?,members?}]`/`teams[{name?,path}]`/
  `members[{name,agent,avatar?,model?,provider?,thinking?}]`, members reference AgentDefs; `name` is the name shown on the member's messages, so make it a person's name, and `avatar` pins one of the bundled portraits.
  `model`/`provider`/`thinking` pin the member's engine — which model does which job is part of the formation, so a crew can mix a cheap fast reviewer with a stronger designer; each falls back to the agent definition and then to the session, and a named `provider` other than the session's needs a `model` too.
  `/team validate` checks the engine against this session's providers, so a blueprint that passes still starts) pins multiple roles to one project; started by default at launch
  (`settings.team.autoStart`; `--no-team` turns it off; starting ≠ waking — members stand by Idle at zero tokens,
  only `/team assign` or channel messages start them; idempotency key = instance name, repeated start reuses). The `/team` command family
  manages it; team memory is keyed by "project path hash + branch" in `~/.config/bingo/teams/` (each member gets a readable `<name>.md` transcript beside the exact `<name>.json` record; a spawning member is *told where its transcript is* and starts with an empty context rather than having the history preloaded — that file is unbounded and monotonic, so loading it charged a growing invisible toll on the first turn for relevance that decays fast; read it when the task depends on what was already decided, not speculatively +
  append-only decision records; `/team memory list|gc` manages it).
  The model manages the same crew through the **Team tool** (`status`/`validate`/`start`/`stop`/`save`, main session only): reads are free,
  and every change is confirmed by the user in person — the prompt appears in *every* permission mode and an `allow` rule cannot
  pre-authorize it (only `deny` outranks it), because hiring a crew is not something a permission table should be able to consent to on
  the user's behalf. The confirmation names the change, not the file (`Rewrite .bingo/team.json · dev-room · 4 members (-ui +qa)`).
  `save` writes the whole document, so it takes the complete roster — whoever is left out is removed, with one exception: `teams` (the org
  chart) is carried across every save, because it points at other directories and a roster edit is no reason to re-decide it. Hand-editing
  `.bingo/team.json` with Write/Edit asks the same question. Dispatch is not part of the tool: SendMessage gives a member work.
- **rooms and the team tree** (D54): a team declares its rooms in `channels[]`, each with its own mode, budget and roster — a department
  has a standup, a release channel and a design review, and the same person is in some and not others. A team that declares none gets one
  room named after it holding everybody (the `channel{mode,messageLimit}` shorthand, unchanged); a team that declares rooms gets *only*
  those. A blueprint may name child blueprints in `teams[{name?,path}]`, recursively: `path` is relative to that team's own directory
  (absolute is refused) and names either the directory holding a blueprint or the file itself. Each team keeps its own agent definitions,
  working agreement, git branch and memory partition, rooted at its own directory — so reaching a department from the root gives the same
  crew as opening a session inside it, and a member of one is told in a system block where its directory is (tool paths resolve against the
  *session's* cwd, not its team's). Teams, members and rooms are unique across the whole tree, which is what lets `SendMessage("Linh")`
  reach a member three levels down with no team prefix. A room reaches its own team and the teams below it, never a parent or a sibling.
  `/team status|start|stop|validate|memory` and the Team tool's actions all span the chart; `autoStart` brings the whole thing up.
- **crew first, hires temporary** (D53): where a crew is pinned, it is the default workforce — work goes to a member by SendMessage,
  and the Agent tool is for what no member covers. An Agent-tool spawn is a *temporary hire*: it never enters `.bingo/team.json`,
  it is listed apart from the crew (`/team list`, Team `status`, and a `crew`/`hire` prefix on every `AgentControl list` row), it is
  recorded in the crew's `decisions.md` under `type: hire`, and it is released once its task is done — idle, inbox empty, nothing
  still owed an answer, with one hub round left to follow up in. The sweep only runs while a crew is actually up; in a project with
  no crew, ad-hoc subagents live exactly as long as they always did.
- **team norms** (`.bingo/team-norms.md`, committed beside the blueprint): prose, not a schema — the crew's working agreement.
  It reaches every member and every hire as a system block, so it applies without being restated, and it carries its own precedence
  rule: a direct instruction outranks it on the point that instruction makes, and every other norm still holds. `/team new` scaffolds
  a starter agreement (never overwriting one that exists); `/team norms` prints what is on disk.
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
  `#[image ✗ load failed]` and shows a warning line with the url.
- **MCP**: stdio and streamable HTTP (`type: "http"`, with custom headers) server tools are integrated (see above).
- **Memory**: memdir auto-memory (`~/.config/bingo/memdir/`, filenames
  `<project-name>-<path-hash>.md`, same-name directories don't cross-pollute) + project CLAUDE.md (Anthropic convention).
- **Sessions**: transcripts persisted (JSONL), `--continue`/`/resume` restore, `/compact` compacts. Startup cleanup and `/gc` enforce a 30-day TTL plus a latest-100 inactive-session cap plus a 24-hour activity grace; share snapshots are removed with their transcript, while prompt-history files use the same TTL and a 100-file cap. `/cd <dir>` switches the session-owned working directory without changing the process cwd; subsequent Bash/Read/Edit/Write/Glob/Grep calls, project skills/agent definitions, Team/Agent crew lookup, Experience project keys, memory extraction, settings command paths, image paths, and `/team` resolve from the new directory. Startup-loaded settings/MCP configuration and the already-built system prompt are not reloaded.
- **Built-in tools**: Bash (through the permission gate, combined output capped by `bashOutputMaxChars`),
  Read/Glob/Grep (line ranges, exclusion/depth filters, search context/options; Read returns image files as
  viewable images, so screenshots and rendered charts can be inspected), Edit/Write, WebFetch/WebSearch,
  Agent (subagents), SendMessage/AgentControl (subagent continuation and lifecycle, main session only),
  Team (the project crew, main session only — reads are free, every change asks the user in person),
  the Task family (task tracking), AskUserQuestion (main session only — a subagent has no prompt surface),
  Skill (skill invocation),
  ExperiencePropose/Commit/Query/Outcome/Forget (project experience capture, retrieval, and verified-use feedback).
- **Experience**: reuses rerunnable workflows across sessions. At session start, this project's active
  experience index is injected (≤10 entries, explicit observed outcomes ranked before the legacy commit count, nothing when empty); full text is searched via ExperienceQuery by trigger
  tokens (case-insensitive, shared-prefix tolerant, active first);
  ExperiencePropose generates candidates (not persisted); after user confirmation ExperienceCommit persists
  (same content → stable id, re-committing updates rather than duplicating; `status: stale` marks invalidation,
  exiting injection but staying queryable).
  After actually adopting a query result, ExperienceOutcome records a permission-confirmed `helpful` or `harmful`
  outcome with concrete evidence; it only appends outcome history and never automatically changes the lifecycle `status` or `verified_at`.
  ExperienceForget evicts (requires user confirmation). Stored in
  `~/.config/bingo/experience/<project-key>/entries/` (user-level, never in the project repo),
  the project key comes from the git remote URL (normalized) → git root → normalized absolute path, stable across directories/machines.
- **Subagents**: instances spawned by Agent have names (the `name` arg, defaulting to the definition name/agent; name collisions
  auto-suffix -2/-3), shown in the transcript as `◉ name · task`; history is kept after completion, and the main agent can
  SendMessage to continue, or manage with AgentControl list/messages/stop/delete; each list row includes relative last activity
  (`active now`, `active 3s ago`, `active 2min ago`) so a quiet idle instance is distinguishable from one that just finished.
  **Messaging**: SendMessage returns a `message_id` after enqueueing and dispatches immediately: an idle instance starts now,
  while a running instance drains its inbox between tool rounds. Everything waiting when the receiver drains is folded into one prompt. Queued is not an acknowledgement:
  `AgentControl(action=messages, agent=…)` reports each message as delivered (with which run it landed in), still queued (with its wait time),
  or dropped because the instance was stopped. stop/delete clears the mailbox and reports how many undelivered instructions were dropped with it;
  when the run chain fails the messages stay in the mailbox and the recovery dispatcher retries them.
  **Delivered ≠ replied**: an instance can fully read a message, finish a turn without a word, and look identical to a dead one from outside. The receipt is based on "reply",
  so `messages` reports four states — queued, read but unanswered, replied (noting which turn opened its mouth), dropped; as soon as a turn produces any text,
  every message that instance had read before counts as replied (including those read in the silence of an earlier turn).
  **Automatic reply chase**: this round of checking is done by the system, and it's **on by default** — every SendMessage carries a 300s check unless explicitly disabled.
  When the wait elapses it re-reads the same receipt; as long as a reply is still owed to the sender, it drops a follow-up into the recipient's mailbox (stating which kind of silence it was, asking only for a reply,
  not re-sending the original instruction) and triggers the dispatcher again, at most 3 rounds. A reply within the wait stays silent throughout; speaking only after being chased, being dropped, or staying silent through the last round
  are all reported to the main agent as task notifications. `ack_timeout: <seconds>` adjusts the wait (5-3600: shorten it when the reply is expected soon, lengthen it when the work is known to be quiet and long),
  and `ack_timeout: 0` disables the check for that message.
  **Sending images to subagents**: restate the `#[image N]` marker in the Agent prompt or SendMessage text — the attachment table belongs to the session,
  and the subagent receives the real image (the image rides along while the message is queued). This path also works **out of a session that doesn't accept images**: when the current endpoint rejects images,
  fork the subagent onto an image-capable provider (`Agent(provider: …, model: …)`, cross-provider requires an explicit model) and restate the marker,
  so resolution is unaffected by endpoint capability, the subagent sees the real image and reports its conclusion. If the marker arrives without the image it explains which situation it is
  (the endpoint doesn't accept images — listing the usable providers — or the attachment is gone because the session was resumed), rather than having the model hunt for a file that never landed on disk.
  **Named definitions**: `~/.config/bingo/agents/*.md` and `.bingo/agents/*.md` (project-level wins on
  same-name clashes); frontmatter `name/description/model/provider/thinking/inherit_system`, body = the subagent
  system prompt; referenced via the Agent tool's `agent` parameter.
  **Per-instance model/thinking**: the Agent tool's `model`/`provider`/`thinking` parameters can give a single
  subagent a specific model, provider (the settings `providers` section, cross-endpoint/cross-key) and thinking level
  (`off/low/medium/high/xhigh/max`); precedence: explicit parameter > named definition > inheriting the parent session's
  current value (model/provider/thinking are each independent and never affect the parent session).
  **Channel messaging** (experimental, `experimental.agentChannels`): the main agent uses the Channel tool
  to create channels / add and remove members (members are direct subagents; the main agent joins automatically as `main`), members use Post
  to speak — messages enter every member's context (same order), senders stamped by the runtime; in serial channels a lagging
  post bounces back with the new messages attached (agents read, then amend or drop — roll-call ordering emerges this way),
  free channels allow interleaving. Channels appear in the transcript as `◇ #name` rows (expandable to the full group chat);
  when a budget is exceeded the channel freezes automatically and the main agent is notified. **Who is speaking decides whether to reply**: delivery wakes every member, so every spawned member carries a
  system-prompt rule (injected only when the toggle is on) — answer briefly when `user`/`main` speaks to the room, owe nothing when a colleague speaks
  (unless named or you can unblock them), and **never answer an answer** (replies to replies are the source of noise). The rule also spells out the mechanism the model can't infer:
  in a turn woken by a channel message, the body text goes back to the hub — **only Post can put words in the room**. The rule lives in the system block, not the wake payload: compaction rewrites message history but never touches the system prompt,
  so the rule survives even after a long-running member's context is summarized away.
  **Bottom entity area**: when running instances/channels exist, a one-line summary shows above the input; idle/stopped agents stay out, and running rows show model/thinking/state. With empty input, ↑/↓ selects running agents and Enter opens that DM; Esc collapses the selector. Ctrl+G opens the fullscreen **Slack-style workspace** directly. Ctrl+B opens the main-view background-agent manager (↑/↓ select, Enter detail, x stop, Esc close); detail shows prompt/status/elapsed/tokens/tool count/recent tool activity, and hub-and-spoke has no foreground action. The workspace is the whole screen as a single message-flow column (a compact title at the top
  giving the channel/instance with the team name at the right edge; date separators, avatar + bold sender + time, consecutive messages merged, new-message
  dividers; DMs contain only user messages and agent text, with all historical/live tool activity hidden; their bodies reuse the main transcript's bubble/markdown/prefix/wrapping row builders while the existing DM name/avatar gutter stays unchanged, and silent tool waits retain a working indicator). DM headers include model/thinking; channel headers list model/thinking when it fits and otherwise use one bounded aggregate. No rail and no sidebar,
  the view paints no background of its own — the terminal's own background shows through; switching conversations is Ctrl+K (quick switcher listing
  every conversation and its unread count) and alt+↑↓. **Avatars**: terminals that can place kitty images (the same capability behind inline images)
  assign each speaker one of eight bundled anime-style portraits, 4×2 cells to the left of the name; other terminals fall back to an initial-on-color
  chip, and both skins keep the same row count. Team members' avatars are pinned in `.bingo/team.json` (`"avatar": "sora"`),
  so a crew has a fixed cast; other instances get a face by name. The **main chat** uses the same faces behind `experimental.chatAvatars` (off by default): every message gets a band
  above it carrying the speaker's portrait and name (`main` for the hub, `You` for your own); message bodies are unchanged underneath.
  Off, the transcript has no band and a subagent's watch row keeps its `◉`; the switch governs the main chat only. Runtime-injected wake scaffolding (channel-message relays,
  task reminders) collapses into a single dim hint line instead of being quoted as a whole message. Sending from the bottom input box: in a channel you speak as `user` (the same delivery
  path as Post, waking members normally; rendered counts as read, serial won't bounce you), DMs use SendMessage's immediate dispatcher
  (shown as pending only until the receiver claims them). Keys: Tab switches between the message list and the input box, ↑↓ or the mouse wheel
  scrolls the transcript (three rows per wheel notch), alt+↑↓ switches conversations, Ctrl+K quick-jumps, Esc returns.
- **agent team** (project-level crew): `.bingo/team.json` (camelCase: `name`/`channel{mode,messageLimit}`/
  `members[{name,agent,avatar?,model?,provider?,thinking?}]`, members reference AgentDef; `name` is the name shown on the member's messages — give it a person's name, not a role code; `avatar` pins the portrait.
  `model`/`provider`/`thinking` pin the member's engine, each falling back to the agent definition and then to the session; a named `provider` other than the session's needs a `model` too, and `/team validate` checks all of it, so a blueprint that passes still starts.
  `/team list` and `AgentControl list` report the engine each running instance is actually on) fixes several roles to one project; pulled up by default at startup
  (`settings.team.autoStart`, `--no-team` turns it off; starting ≠ waking — members stand by Idle at zero tokens,
  only `/team assign` or channel messages start them; idempotency key = instance name, repeated start reuses). Managed by the `/team` command family;
  team memory is keyed by "project path hash + branch" in `~/.config/bingo/teams/` (each member gets a readable `<name>.md` transcript beside the exact `<name>.json` record; a spawning member is *told where its transcript is* and starts with an empty context rather than having the history preloaded — that file is unbounded and monotonic, so loading it charged a growing invisible toll on the first turn for relevance that decays fast; read it when the task depends on what was already decided, not speculatively +
  append-only decision records, managed via `/team memory list|gc`).
  The model manages the same crew through the **Team tool** (`status`/`validate`/`start`/`stop`/`save`, main session only): reads are free,
  and every change is confirmed by the user in person — the prompt appears in *every* permission mode and an `allow` rule cannot
  pre-authorize it (only `deny` outranks it), because "hiring a crew" is not something a permission table should consent to on
  the user's behalf. The confirmation line names the change, not the file
  (`Rewrite .bingo/team.json · dev-room · 4 members (-ui +qa)`). `save` writes the whole document, so it takes the complete roster — whoever is left out is removed;
  hand-editing `.bingo/team.json` with Write/Edit asks the same question. Dispatch is not part of the tool: use SendMessage to give a member work.
- **Skills**: built-in `guide` (this guide) + `~/.config/bingo/skills/` and `.bingo/skills/`
  directory skills (same-name disk skills override built-ins); the model invokes them via SkillTool, users run them via `/skill-name`.
- **Images**: markdown images in model replies (`![alt](path)`, supports relative paths/data/http(s))
  render inline on terminals that support kitty graphics (Ghostty/kitty/WezTerm etc.); other terminals show
  the `#[image]` placeholder. Inside tmux, bingo enables passthrough automatically (`tmux set -p
  allow-passthrough on`), rendering via Unicode placeholders (U=1) when the outer terminal is Ghostty/kitty,
  so images scroll with the text; when the outer terminal is WezTerm/Konsole (no U=1) or unrecognized it
  shows the `#[image]` placeholder with a one-time notice. Images load automatically with the message and render when the message settles to disk —
  no extra command needed. A failed fetch (network error, 4xx/5xx, undecodable data) marks the row as
  `#[image ✗ load failed]` with a warning line giving the url.
- **MCP**: stdio and streamable HTTP (`type: "http"`, custom headers allowed) server tools are integrated (see above).
- **Memory**: memdir auto-memory (`~/.config/bingo/memdir/`, filenames
  `<project-name>-<path-hash>.md`, same-name directories don't cross-pollute) + project CLAUDE.md (Anthropic convention).
- **Sessions**: transcripts persisted (JSONL), `--continue`/`/resume` restore, `/compact` compacts. `/cd <dir>` switches the session-owned working directory without changing the process cwd; subsequent Bash/Read/Edit/Write/Glob/Grep calls, project skills/agent definitions, Team/Agent crew lookup, Experience project keys, memory extraction, settings command paths, image paths, and `/team` resolve from the new directory. Startup-loaded settings/MCP configuration and the already-built system prompt are not reloaded.
  **Sharing**: `bingo share [session]` generates a self-contained HTML file in the current directory by default (`--output`
  specifies the path), never touching the network. Only an explicit `--public` uploads to the official share service and prints
  a public `https://bingo.ruobin.dev/share/u/<id>` link; **anyone can access it publicly**, so
  bingo prompts before upload starts that the full conversation/tool output may contain sensitive information. `--open` opens the local
  file or the published link. The settings `share.baseUrl` overrides the service base (default
  `https://bingo.ruobin.dev`); an upload failure automatically falls back to the local file. In-session
  `/share [--public] [--open]` uses the same safety semantics: local by default, only `--public` uploads.
  The session key has the same semantics as `/resume` (transcript stem or a matchable fragment, defaulting to the most recent session).
  **Updates**: `bingo update` pulls the latest release from GitHub Releases (yexrob/bingo) and atomically replaces the current
  executable — platform assets (`bingo-<triple>.tar.gz` / `.zip`) + `checksums.txt` SHA-256
  verification, unpacked then replaced via same-directory tmp + rename (Unix keeps the executable bit); `--check` only detects, doesn't download.
  Output: already latest / new version found (`--check`) / update succeeded (new version + install location);
  failures give the reason (network / checksum failure / no permission — suggesting sudo or manual install).
  At startup the TUI checks for new versions asynchronously in the background (`~/.local/share/bingo/update-check.json` 24h TTL cache;
  failures are silent and never block startup; `--print` headless doesn't trigger it); when a new version is found the welcome card shows
  a "New version vX.Y.Z available — run bingo update" notice line (the version and command breathe for 9s
  then settle and stay; a keypress settles it early; `motion: "off"` or `BINGO_NO_MOTION=1` shows it statically).
