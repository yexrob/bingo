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
- Shortcuts (press `?` with an empty input for the full table): Enter sends · `\`+Enter / Ctrl+J newline (multi-line input;
  Shift+Enter works too wherever the terminal speaks the kitty keyboard protocol, which bingo probes for and enables at startup) ·
  Esc closes the topmost open layer first — permission dialog, picker, the `Ctrl+B` background dialog, slash dropdown, ctrl+r search, info/error rows,
  the `?` panel, a manually opened task panel — and interrupts the turn when none of those is open (bash mode exits below
  the interrupt, so Esc stops a running `!` command first), then double-press clears input · Ctrl+C busy interrupt (unconditional, layers do not shield it) / clears text /
  empty input twice exits · ↑↓ history (move the cursor first in multi-line input; busy empty-input ↑ recalls queued messages, unless the turn has already taken one) ·
  Ctrl+P/N are ↑/↓ exactly (history, and the busy-turn queue pull-back) ·
  Ctrl+R reverse history search · Ctrl+A/E line start/end · Alt+B/F move one word and Alt+D/Alt+Backspace kill one,
  stopping at `/` `-` `_` `.` so a path is walked a segment at a time · Ctrl+W/U delete the whole whitespace word/to line
  start and Ctrl+K/Alt+K to line end · Ctrl+Y paste back the newest deletion and Alt+Y right after it cycles the 10-entry kill ring (consecutive
  kills in the same direction come back as one span) · Ctrl+S stash/restore input · Ctrl+_ undo · ctrl+o
  opens the transcript view: an alternate-screen pager over the whole session with every tool output and thinking block
  expanded (ctrl+e collapses back to the summaries, `/` searches with n/N stepping through hits, j/k · PgUp/PgDn · g/G
  move, o opens the image in view in the desktop's viewer, q/Esc/ctrl+o closes and puts the previous screen back; a permission dialog keeps priority, so ctrl+o is inert
  while one is open) · Ctrl+T cycles the task area, then the agent tree (@main plus one row per instance), then closed; Shift+↑/↓ opens the tree and picks a row (Enter views that instance, k stops it), Ctrl+Shift+O toggles its per-row message preview · Ctrl+G (or the readline chord Ctrl+X Ctrl+E) composes the current draft in `$VISUAL`/`$EDITOR`: the draft goes to a temp file, the editor gets the terminal, and the saved content replaces the input as one undo step; a non-zero exit keeps the draft and says so, an editor that exits having written nothing says so too (an editor that opens its own window needs its wait flag — `code -w` — or it returns before you have typed and its edit is read back as no edit at all), and with neither variable set the info tier says `set $EDITOR to edit the prompt in your editor`. Composing while a turn runs is fine; a permission dialog keeps priority. A composer line opening `@name ` or `#room ` is a direct send (D103): the rest of the line goes to that agent's inbox or that room, under your own name and without the model seeing it, and the only trace is a transient `Sent to @name` above the composer · Ctrl+B moves the shell command running in the foreground to the background (same process, same output, returns a task id and notifies on completion), and with none running manages running background agents · Ctrl+L clear and redraw · Shift+Tab cycles permission
  modes (default → acceptEdits → plan), and inside an approval prompt takes `Yes, and don't ask again this session` · Ctrl+E inside an approval prompt expands the full command/diff preview and the session rule it would install · Alt+T thinking toggle (off ↔ the last non-off level, default medium) · while busy, Enter queues the message; the running turn folds queued plain messages into its own context at its next tool call, marked `↪` in the flow, and whatever it did not take is sent automatically at turn end (a queued slash command always waits for turn end, and so does anything queued behind one or carrying an image; /think /model /provider /theme /images /status /context /tasks /help /skills run immediately) ·
- **Rewind** (Esc twice on an empty composer, `@main` only, not while a turn runs): stage one lists the turns the user opened, newest first, each with its clock and how many files it and everything after it changed; stage two asks what to do with the chosen one — `Restore code and conversation` · `Restore conversation` · `Restore code` · `Summarize from here` · `Never mind` (↑↓ move, 1-5 jump, Enter confirms, Esc returns to the list and then closes). Restoring the conversation truncates the session history to end at that user message and puts its text back in the composer; restoring code rewrites the files `Edit`/`Write` changed at or after that turn (a file the turn created is removed by name) and leaves the conversation alone; both does code first and reports one line. Summarize replaces that turn and everything after it with a model-written summary appended after the message before it. Pre-images are taken once per file per turn under `~/.local/share/bingo/rewind/<session>/`, git-independent, evicted oldest-first past 50 MB or 200 turns, and files over 8 MB are recorded as unsnapshotted rather than stored — the edit still happens either way. Only turns recorded with a turn marker are offered, so sessions started before this version have no rewind points, and a turn folded into a compaction summary is never offered because it no longer exists verbatim. **Not covered**: anything a `Bash` command wrote.
- The running-status row above the composer reads `✻ {verb}… (esc to interrupt · {N}s · ↓ {tokens} tokens)`. The star cycles one glyph per 120ms, a glimmer crosses the verb about once every two seconds, the token count eases to each new value over 300ms rather than jumping, and the verb is picked once per turn and kept. Three seconds with nothing arriving turns the star and the verb warning-coloured and stops the glimmer — the words do not change, because nothing new is known. When the turn ends, the `✻ {verb} for {N}s` line it leaves behind flashes the accent colour once before settling. All of it rests under `motion: "off"` except the two colours, which are information.
- During streamed output, the main footer shows a live `N tok/s` indicator; the speed band changes its character animation and cadence, and idle/stalled output hides it. Beside it, context usage stays visible as a four-cell bar, percentage, and `used/window` token count, using the active model's window; the colors count down to the auto-compaction trigger rather than to the window — warning within 20 percentage points of it, danger within 5. Once the count passes the warning point, the turn also emits a `context at N tokens; auto-compact at M` warning row. A running instance's DM composer shows the same indicators. `motion: "off"` freezes the rate frame but keeps the value.
- `@` at the start of a word opens the mention dropdown: project files (git-tracked plus untracked-but-not-ignored inside a repository, otherwise a bounded walk that skips hidden and build directories, capped at 5000 entries) and the names of running background agents, fuzzy-filtered by what you type after the `@`. ↑↓ select, Tab or Enter inserts — a file as its path relative to the session directory, an agent as `@name` — plus a trailing space; Esc closes it and keeps what you typed. Inside a word (`user@example.com`) it is an ordinary character, and a permission dialog keeps priority so nothing opens behind it.
- Large pastes auto-collapse to a `[Pasted text #N +M lines]` placeholder; the real content expands on send
  (precisely detected via terminal bracketed-paste events; terminals without that feature fall back to a
  key-burst heuristic — extremely fast typing may misdetect, and pausing recovers). A paste is not typing:
  its newlines stay newlines instead of sending, and an `@` or `/` inside it is a character rather than a
  dropdown. (The burst fallback cannot see a paste's first four characters, which is what bracketed paste is for.)
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
| `providers` | object | Named providers: `{name: {protocol?, apiKey?, envKey?, apiBaseUrl?, supportsImages?, oauth?, models?}}`, switch via `/provider <name>`; `protocol` is `"anthropic"` (default) or `"openai"` (Responses API, `Authorization: Bearer`; `apiBaseUrl` defaults to `https://api.openai.com`); an empty/absent `apiBaseUrl` falls back to the protocol default; unknown protocols are a config error at startup. `envKey` names an environment variable holding the key — credential order is `apiKey` > `envKey` > stored key / OAuth, so a project layer can reference a key without containing one. `oauth: {kind: "codex"}` enables OAuth login (apiKey wins over OAuth); the codex flow (device / loopback PKCE) is `chatgpt.com`-subscription auth, tokens stored in `~/.local/share/bingo/auth.json` (0600, never in the committed settings) |
| `models` | array | The default provider's model list; per-provider under `providers.<name>.models`. Entries are model ids (`"gpt-5.6-sol"`) or objects (`{id, display?, contextWindow?, maxTokens?, thinking?, vision?}`). Declaring is authoritative: `/model` lists exactly these with no request, and the declared `contextWindow`/`maxTokens`/`thinking`/`vision` outrank the built-in metadata table. `maxTokens` is the output ceiling sent as the request's `max_tokens` and reserved out of the input window (clamped to half the window). `vision` says whether the model accepts image input — the model is told its own capabilities in the system prompt, and every outgoing request drops image blocks for a model without vision, each replaced by `[image omitted: <model-id> has no vision]` (pasted images and images a tool read alike; the transcript keeps the real block, so switching to a model that can see shows it again) (distinct from `sendImages`/`supportsImages`, which are endpoint-wide send gates). Providers that declare nothing pull the endpoint's own list, cached in `~/.local/share/bingo/models-cache.json` for 24h (`r` in the menu re-asks). bingo never filters an endpoint's models itself. Family-wide defaults (no per-provider declaration needed) live in `~/.config/bingo/model-catalog.json`: `builtin` mirrors the compiled table and is rewritten on upgrade; `overrides` is the user's, keyed by id prefix (longest match wins per field), resolved between the declaration and the built-in table |
| `provider` | string | Current provider (persisted by `/provider` and the /model menu; default `"default"` = top-level `apiKey`/`apiBaseUrl`); restored at startup, an invalid name falls back to default with a warning |
| `sendImages` | bool | Whether the default endpoint sends message-box image attachments to the model (named providers use their own `supportsImages`). Both protocols carry image blocks, so this defaults to **true** — set `false` to opt out an endpoint that speaks the protocol but rejects images (some compat proxies) |
| `thinkingLevel` | string | Thinking level: `off` sends no thinking param (DeepSeek-compatible, default); `low`/`medium`/`high`/`xhigh`/`max` send `{"type":"adaptive"}` adaptive thinking plus `output_config.effort` (the Claude 5 family removed budget_tokens; below `high` saves tokens, `xhigh`/`max` think deeper) |
| `permissionMode` | string | `default` / `acceptEdits` / `plan` / `dontAsk` / `bypassPermissions` |
| `theme` | string | `auto` (follows the terminal background) / `dark` / `light`. Both presets are fully RGB — the palette is bingo's, not the terminal's ANSI mapping (without truecolor the same colours come down as 256-colour approximations). Text uses three tiers: primary (content), secondary (result lines, tool output, diff context), muted (hints, stamps, rules, the diff line-number gutter). Fenced code blocks are syntax-highlighted when the fence names a language (`rust`, `python`, `javascript`/`typescript`, `json`, `bash`/`sh`, `toml`, `yaml`, `markdown`, `diff` and more); an unknown or missing tag stays monochrome. Diffs carry an old/new line-number gutter on every surface — approval preview, completed edit rows, transcript view. `/theme` reapplies all of it live |
| `motion` | string | TUI motion: `auto` (default) / `off` — one gate over every animated surface. `off` rests them all: the status-row spinner freezes on `✻` instead of cycling, the glimmer stops crossing the running verb, the welcome-card update notice stops breathing, the terminal title's marker stops alternating, and a token count that jumps snaps instead of easing. The indicators themselves all stay, and the two colours that carry information keep changing — a turn quiet for 3s still goes warning-coloured, and a finished turn's completion row still gets its accent. Env `BINGO_NO_MOTION=1` is equivalent |
| `notifications` | string | Attention channel: `auto` (default) / `bell` / `iterm2` / `kitty` / `ghostty` / `off`. `auto` reads the terminal — `TERM_PROGRAM=iTerm.app` → iTerm2's `OSC 9`, kitty (`TERM_PROGRAM` or `TERM=xterm-kitty`) → kitty's three-part `OSC 99`, `TERM_PROGRAM=ghostty` → Ghostty's `OSC 777`, anything else → the terminal bell. It fires three times: a permission prompt is waiting (`Waiting for permission`), a turn that ran 10s or longer finished (`Turn complete`), and a turn failed at flow level (`Turn failed`). The terminal title tracks the same states via `OSC 2` — `✳ bingo — working…`, `✳ bingo — waiting for permission`, `bingo — <directory>` — and is handed back on exit (including after a panic). While a turn runs the title's marker alternates `✳ ⠂ ✳ ⠐` about once a second (static `✳` under `motion: "off"`), and a waiting permission prompt keeps the title it needs. Inside tmux the notification travels in a passthrough envelope (needs `allow-passthrough on`); the bell and the title go bare, so tmux's own bell action and pane title still work. `off` silences the notification and the title both |
| `cacheControl` | bool | Send prompt caching; turn off if a non-official endpoint is unstable |
| `respondToBashCommands` | bool | Whether `!` commands are handed to the model for a response after execution (default true; false = pure execution) |
| `bashOutputMaxChars` | integer | Maximum combined stdout/stderr characters returned by the Bash tool (default and maximum 48,000); truncated results point to redirecting output and reading the file |
| `shell` | string | Shell program for the Bash tool and hooks; default per platform: macOS `/bin/zsh`, other Unix `/bin/bash`, Windows `powershell.exe`. PowerShell-family shells run with `-Command`; other configured shells (e.g. Git Bash `bash.exe`) with `-c`. The resolved shell and its dialect (posix/powershell/cmd) are reported to the model (environment block, Bash tool description) and to JSON clients (`session.ready` metadata `shell`/`shellDialect`), so generated commands match the real executor |
| `mcpServers` | object | `{name: {type?, command, args, env}}` (stdio, default) or `{name: {type: "http", url, headers?}}` (streamable HTTP) |
| `disabledMcpServers` | string[] | List of disabled MCP servers (written by `/mcp disable`) |
| `permissions` | object | `{allow[], deny[], ask[]}`; rule syntax `Tool(content)`, `:*` is a prefix wildcard (e.g. `Bash(git push:*)`); Bash rules match per subcommand segment; path rules normalize before matching (see diagnostics 4) |
| `experimental` | object | Experimental features: `{"agentChannels": true}` enables agent room messaging (the main session and direct subagents get the Channel tool, and `SendMessage` accepts `#room` targets); `channelMessageLimit` (default 500, freezes the channel when exceeded) / `agentMessageLimit` (default 50) are budget gates; `{"chatAvatars": true}` puts a subagent's portrait on its watch row in place of `◉` (a row inside a grouped dispatch never wears one) (default false; the avatar gutter every conversation wears is not governed by it) |
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

`/help` for the full list. Common ones: `/model [name]` (no args: two-level picker — level 1 providers → level 2 model list, which comes from the settings `models` declaration when there is one, otherwise the endpoint; `r` re-asks a fetched list; with a name: switch directly, validated against the known list when available),
`/provider [name]` (no args: picker — ● current, s = session-only, Enter persists; with a name: switch directly), `/provider login <name> [--device-auth|--manual <token>]` (OAuth login: default opens the browser; `--device-auth` prints URL + code and polls for headless/SSH; `--manual` stores a pasted token), `/provider logout <name>` (revokes + clears),
`/think [off|low|medium|high|xhigh|max]` (thinking level, persists to settings; no arg opens the level picker: ●=in effect, ↑↓/1-6 to browse, Enter confirms, Esc cancels), `/theme [dark|light|auto]` (no arg opens the level picker; the `/theme auto` explicit shortcut stays),
`/images` (the content images this session has shown — pasted, attached, produced by a tool, or loaded from a markdown URL — newest first as `source · stamp · size`; ↑↓/1-9 browse, Enter opens the picture in the desktop's viewer, Esc closes; the same picture opens by clicking its row in the fullscreen host, or with `o` in the Ctrl+O transcript),
`/cd <dir>` (switch the working directory for this session; relative paths resolve from the current session directory),
`/permissions [allow|deny|ask] [rule]`,
`/mcp` (status) · `/mcp enable|disable [name|all]` · `/mcp reconnect <name>`,
`/skills` (listing; `/skill-name` runs it directly) ·
`/join #room` · `/leave #room` (become a member of a room, or stop being one) ·
`/context` (usage) · `/status` ·
`/compact` (force compaction) · `/resume [name]` (restore a past session; no arg opens the session picker, Enter restores) · `/rename` · `/gc` (clean expired session storage; 30-day TTL, latest 100 inactive sessions, 24-hour activity grace) · `/clear` · `/exit`.
`/team` (project-level crew): `list` (blueprint + runtime on one screen) · `start` (pull up / idempotent reuse; a member already up that is not mid-turn re-reads its definition, keeping its history) · `status` ·
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
   when close to the context window, `/compact` (auto-compaction threshold = 90% of the effective window,
   which is the model's window minus its own output budget: 1M − 128k ≈ 785k for current Claude models,
   1M − 384k ≈ 554k for DeepSeek v4; family defaults live in `~/.config/bingo/model-catalog.json`, and a
   declared `maxTokens` sets the budget for any model). Endpoints without a
   count_tokens API (DeepSeek/ollama; OpenAI-protocol providers — `count_tokens` is Anthropic-only)
   automatically fall back to local estimation (ASCII ≈ 4 characters per token, CJK ≈ 1 token per character,
   an image a flat 1600), with a one-time warning on first fallback.
   If that estimate is low and either wire protocol rejects a request as a context overflow, bingo compacts
   the history and retries the rejected request once; a second overflow ends the turn instead of looping.
   Compaction appends a summary marker to the session file instead of rewriting it: reloads and `/resume`
   replay summary + recent tail without re-summarizing, while `/share` still exports the full original
   conversation.
   If an upstream response completes without assistant content or tool calls (including an unclosed thinking block),
   bingo treats it as malformed and retries the side-effect-free attempt once instead of ending silently; if the retry
   is also empty, the turn shows the normal full-flow retry/back error. Transient error events received after a stream
   opens (`429`, `5xx`, overloaded, `server_error`) restart the model response with jittered exponential backoff up to
   10 times; the first reconnect notice is suppressed, later attempts show `Reconnecting... N/10`, and interactive
   live views discard the failed attempt before showing its replacement (headless stdout cannot retract an already
   written prefix). Quota, plan, invalid-prompt, and context-overflow errors fail immediately; short synchronous
   operations keep their existing 10s read / 15s write timeout tier and do not enter this long-turn retry loop.
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
   The prompt itself shows what it is approving — a Bash command, or an Edit/Write dry-run diff computed
   without touching the file — over three options: `Yes` · `Yes, and don't ask again this session`
   (Shift+Tab; only offered when the narrowest matching rule — `Bash(cargo:*)`, `Edit(/dir/)`,
   `WebFetch(domain:…)`, or the bare tool name — would really stop the gate asking, so an `ask` rule of the
   user's and the sensitive-path / Team-confirmation checks never show it; the rule stays in memory for the
   session and is never written to settings) · `No, and tell bingo what to do differently (esc)` (Enter opens a
   feedback row whose text reaches the model with the denial; Esc anywhere and an empty submit are the plain
   refusal). Ctrl+E expands the preview; Enter and digits are inert for the first 0.4s a prompt is on screen.
5. **`!` command rejected**: interactive/TTY commands (top/vim/ssh/sudo -i/fzf etc.) are rejected by design —
   use non-interactive equivalents (`top -b -n 1`, `ssh host 'cmd'`).
6. **Stuck in bash mode/accidental trigger**: with an empty input, Esc/backspace/Ctrl+U all exit bash mode;
   with non-empty input `!` is an ordinary character; Tab completes from this session's `!` history prefix.
7. **Can't find a historical session**: transcripts live in `~/.local/share/bingo/transcripts` (`--continue`
   resumes the last one; `/resume` lists/switches). Session storage is cleaned at startup with a 30-day TTL and a latest-100 inactive-session cap plus a 24-hour activity grace; matching share snapshots follow transcript removal. `/gc` applies the same policy on demand. Prompt-history files use the same TTL with a 100-file cap.
8. **Tool output collapsed**: ctrl+o opens the transcript view — the whole session on its own screen with every
   collapsed block expanded; ctrl+e toggles back to the collapsed presentation, `/` searches it, q closes and the
   terminal is exactly as you left it. A row on the main screen still shows `+N lines` for what is folded. The
   collapsed rows already printed into scrollback never change: they cannot, which is why the full text lives in a
   view of its own.
9. **Slash dropdown doesn't have the command you want**: type a prefix to filter (e.g. `/m` matches mcp/model/meye);
   Esc closes the menu; skills are listed in `/skills`, run with `/skill-name`. Past the command name the dropdown
   completes the **argument** instead, fuzzy-matched against the same data the command validates against — `/model`
   the declared/known model ids, `/theme` and `/think` their level tables, `/resume` the stored sessions,
   `/provider` its `login`/`logout` subcommands and the provider names (then the names alone after `login`).
   Commands with free-form arguments (`/cd`, `/rename`, `/team message …`) offer nothing. Tab completes the
   argument in place and stops there — Enter is still what sends the line.
10. **Grep/Glob finds nothing**: `.git`/`target`/`node_modules` and dot-prefixed directories are skipped by default
    (they still search when `path` points at them explicitly); patterns are relative to the search root
    (`src/**/*.rs` works); patterns without `/` match file names at any depth
    (`*.rs` hits the whole tree); traversal stops when the result cap is reached. Glob accepts `exclude` patterns and `max_depth`;
    Grep accepts `context`, case-insensitive/whole-word/fixed-string modes, and files-only results. Read accepts inclusive,
    1-based `start_line`/`end_line` ranges and reports ranges extending past the file.
11. **Processes left behind after timeout/interruption**: Bash commands run in their own process group; timeouts and cancellation
    terminate the whole group (grandchildren no longer orphan); after Esc interrupts a turn, unfinished tools get placeholder results,
    and the session stays recoverable (no 400s on later requests from orphaned tool_use). The interrupted turn is kept, not
    discarded: whatever was already said stays in the history and is followed by `[Request interrupted by user]`
    (`[Request interrupted by user for tool use]` when the stop landed during tool execution) — that marker is the user
    stopping you, so treat everything after it as the instruction and do not resume the abandoned work unasked.

## Capability map (reference when asked "what can bingo do")

- **Built-in tools**: Bash (through the permission gate, combined output capped by `bashOutputMaxChars`),
  Read/Glob/Grep (line ranges, exclusion/depth filters, search context/options; Read returns image files as
  viewable images, so screenshots and rendered charts can be inspected), Edit/Write, WebFetch/WebSearch,
  Agent (subagents), SendMessage (the one speech tool — see below), AgentControl (subagent lifecycle, main session only),
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
  header from the JWT claims; `/model` pulls the subscription's own model list, falling back to a
  static snapshot when that request fails — bingo never narrows the list itself).
- **Experience**: reuses rerunnable workflows across sessions. At session start, this project's active
  experience index is injected (≤10 entries, ranked by observed outcomes before the legacy commit count; nothing injected when empty); full text is searched with ExperienceQuery by
  BM25 relevance over triggers/summary/steps/notes (English word stems and CJK bigrams; ties break active-first,
  then observed outcomes). Each user turn also auto-recalls up to 3 relevant active experiences and
  project-memory facts, appended to the turn tail as a system-reminder;
  ExperiencePropose generates candidates (not persisted); after user confirmation ExperienceCommit persists
  (same content → stable id, re-committing updates rather than duplicates; `status: stale` marks invalidation,
  exits injection but stays queryable). After actually applying a queried entry, ExperienceOutcome records a
  permission-confirmed `helpful` or `harmful` result with concrete evidence; it appends outcome history and
  never changes lifecycle status or `verified_at` automatically. ExperienceForget evicts (requires user confirmation). Stored in
  `~/.config/bingo/experience/<project-key>/entries/` (user-level, not in the project repo);
  the project key is derived from the git remote URL (normalized) → git root → normalized absolute path, stable across directories/machines.
- **Subagents**: instances spawned by Agent have names (the `name` arg, defaulting to the definition name/agent; name collisions
  auto-suffix -2/-3), shown as `◉ @name: task` in the turn that spawns them — a lifecycle event arriving
  when no turn is running no longer writes into `@main` at all (D94), and is carried instead by the
  agent tree, the instance's row in the `Ctrl+B` dialog, and its own record.
  **What the dispatch row shows (D106)**: while the run is alive, the last three things the instance did hang
  under it (`⏺ Read(src/lexer.rs)`), or one `In progress… · 4 tool uses · 8.3k tokens` line when the window is too
  short to hold them; when it ends the row settles into `Done (12 tool uses · 8.3k tokens · 1m 4s)`, which is the
  only form that is printed into scrollback. A round that dispatched **several** agents draws one block instead —
  `⏺ Running 2 agents…` (`⏺ 2 agents finished` once none is left) over a `├─ @scout: fix the parser · 1 tool use ·
  2.1k tokens` row per agent with its current activity under it — and opening any of them takes the group apart.
  **A completion leaves one dim `● @name completed · task` line** where the task notification reaches the main
  agent's context, before the main agent narrates anything.
  **A run failing is the one exception (D98)**: it draws one `⚠ @name · reason` alert line in `@main` and rings
  the attention channel, because bad news must not depend on the main agent choosing to narrate it; a
  cancellation draws nothing. **A run the user triggered themselves** — everything in the batch that woke it came from
  their own DM — ends with no task notification and no woken turn for the main agent at all; the lifecycle
  log still records it, because a log is a record. Room and direct mail for the main agent is
  **digested on a debounce**: a burst buys one turn once the inbox has been quiet for two seconds (or after
  fifteen, so a chatty room cannot starve the wake), never one turn per message; `urgent` skips the wait.
  History is kept after completion, and the main agent can
  SendMessage to continue, or manage with AgentControl list/messages/stop/delete; each list row includes relative last activity
  (`active now`, `active 3s ago`, `active 2min ago`) so a quiet idle instance is distinguishable from one that just finished.
  **Messaging (D98)**: `SendMessage` is the one way any participant speaks to any conversation, and `to` is the
  same conversation namespace the composer writes — an instance name or `@name` for an agent, `#name` for a room.
  Addressing narrows by caller: the main agent reaches any instance and any room it is in; a subagent reaches
  `main` and the rooms it is a member of, and anything else is refused (hub-and-spoke, kept by addressing rather
  than by withholding the tool). A subagent's message to `main` lands in main's inbox and starts a turn there if
  it is idle, and leaves **one** line in `@main` (D106): `@scout❯ <summary>` in the sender's identity colour,
  with its send time. The summary is the optional `summary` field — five to ten words, written by the sender,
  offered on a subagent's schema because a subagent's send is the one that gets drawn — and where none was
  written the line falls back to the message's own first fifty columns.
  The whole body is one keystroke away, in the `Ctrl+O` transcript
  and nowhere else — the flow shows who spoke and roughly what, and what the user reads next is what main then
  says. A **room** relay draws nothing at all: a room is a conversation between agents that main overhears, and
  a line per post is the flood the digest debounce exists to prevent. `urgent: true`
  (subagent→main only) additionally rings the terminal attention channel on arrival.
  A turn the main agent was *woken* into — digesting a notification rather than answering the user — ends like any
  other turn, in prose that renders in `@main` as main speaking and counts on its unread. D102 gave it a second,
  silent ending; D103 removed it: the noise control is the wake debounce above and the dispatch row's own state.
  Writing to an instance returns a `message_id` after enqueueing and dispatches immediately: an idle instance starts
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
  AskUserQuestion, and being woken by background-task notifications (its result goes back to main instead).
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
  with `SendMessage(to: "#room")` — messages enter every member's context (same order), the sender is stamped by the runtime; in serial channels, a stale
  post is bounced back with the new messages attached (the agent reads them, then re-decides/abandons; count-based ordering emerges this way);
  free channels allow interleaving. Channels show in the transcript as `◇ #name` rows (expandable to the full group chat);
  over-budget channels auto-freeze and notify the main agent. **Who spoke decides whether a reply is owed**: because delivery wakes every member, each
  spawned member carries a system-prompt rule (only when the flag is on) — answer `user`/`main` once and briefly when they address
  the room, owe another member nothing unless named or unblocked, and never answer an answer (replies to replies are what turn one
  message into a room-wide storm). The rule also states the mechanism the model cannot infer: a turn woken by a channel message
  reports to main, so **only a message addressed to the room puts words in it** — a reply written as turn text reaches nobody in the channel. It lives in the system block on purpose: compaction rewrites the history
  but never touches the system prompt, so the rule survives a long-running member's context being summarised away.
  **One transcript (D103)**: the terminal shows exactly one conversation — yours with the main agent — and the flow is that transcript in order. There is no conversation switching, no bar, no `/open`, no dividers and no replays; `Ctrl+K` is readline's kill-to-end-of-line again (`Alt+K` is its alias), and `↑/↓` belong to the composer's prompt history. **The one grammar that reaches somebody else is the composer's**: a line shaped `@name <message>` delivers the rest to that instance's inbox **as you**, bypassing the model entirely — an idle instance is woken, a running one takes it at its next tool round, and a stopped one is resumed from its kept history — and `#room <message>` posts to that room as `user`, joining you first (with the room's own membership line) if you are not a member. Success is a transient `Sent to @name` / `Sent to #room` above the composer: never a line in the flow, never in the model's history, gone at the next keystroke. **A name that resolves to nothing is prose** — `@utils explain this code` opens an ordinary turn, verbatim — so the grammar never swallows a question. The typeahead says which is which: at the start of a line `@` lists every instance as `@scout · send message · running` (stopped ones included, because a message resumes them) with project files under them, and `#` lists the rooms as `#build · post to room`, adding `· joins you` where you are not a member; mid-line the sigils are unchanged (`@` is the file-and-agent reference, `#` is a hash in a sentence). The conversation itself is one `Enter` away — see the zoomed view below.
  **The status layer (D104)**: what exists and what is running lives around the composer, never in the flow. `Ctrl+T` cycles `tasks → the agent tree → closed` — `tasks ↔ closed` when nobody is on the roster, and Esc closes one stop. The **agent tree** is `@main` plus one row per instance, rebuilt from the registry every frame and never written into scrollback: `@scout: reading src/lib.rs… · 12 tool uses · 8.3k tokens` while it runs, `Idle for 14s` while it waits, `[stopped]` once stopped — names in their own identity colours, main's reserved. `Shift+↑/↓` opens the tree and walks it: `@main` first (that is where the first press parks), then the instances in name order, then a `hide` row, wrapping at both ends. **Selection is explicit** — with nothing selected every key still belongs to the composer — and only a selected instance row makes `k` mean stop, which goes through the same path and the same warning the `Ctrl+B` dialog's `x` uses; `@main` has no `k` and is not stoppable, and `k` on a stopped instance does nothing. `Ctrl+Shift+O` hangs three condensed lines of each instance's own conversation off its row — the last things it said and the calls it made, a call's own `summary`/`description`/`command` where it wrote one (terminals without the kitty keyboard protocol cannot tell the chord from `Ctrl+O` and get the transcript instead). `Esc` clears the cursor first and closes the panel on the second press. `Enter` on a selected row **zooms** it — an instance row opens that agent's view, the `@main` row leaves a view that is open, and the `hide` row collapses the panel; with nothing selected `Enter` still sends the draft. With the tree closed and anybody on the roster, the window's last line is the **pills**: `@main @scout @writer · shift + ↓ to expand`, identity colours, dim where an instance is idle or stopped, `→` where the window is too narrow for all of them. The **task panel** names an owner who is still on the roster as ` (@scout)` in their colour, and marks a blocked task ` › blocked by #3` — display only: nothing here assigns, claims or unblocks.
  **What the transcript shows of an agent's life (D106)**: four tiers and no more. A **dispatch** is `◉ @scout: fix the parser` with the last three things the agent did condensed under it while it runs — or one `In progress… · 4 tool uses · 8.3k tokens` line where the window is too short for them — and several `Agent` calls from one round draw one tree (`⏺ Running 2 agents…`, `├─ @scout: fix the parser · 1 tool use · 2.1k tokens`), which dissolves back into individual rows the moment you expand a member. When the run ends the row **settles** to `Done (12 tool uses · 8.3k tokens · 1m 4s)` and that is what reaches scrollback; the live progress was never stored anywhere it could. A **completion** whose notification is main's own adds one dim `● @scout completed · fix the parser` line before main says anything about it. A **message** from an agent adds one `@scout❯ <summary>` line in the sender's colour (see Messaging above). A **failure** keeps its `⚠ @scout · connection reset` alert and the attention channel with it. Everything else — running, idle, stopped, cancelled, a room post — writes nothing at all: state is the tree's business, and a line per room post is the flood the digest debounce exists to prevent.
  **The zoomed view (D105)**: `Enter` on a selected agent in the tree — or `f` on its row in the `Ctrl+B` dialog — swaps the screen to *that agent's* conversation on the alternate screen, and swaps it back on `Esc`. The header is `Viewing @scout · esc to return` with the task it was dispatched with under it; the body is the agent's **whole record**, in order — the task, main's instructions, your own messages, room relays, reminders, its work folded the way `@main`'s is (`⏺ Searched for 1 pattern, read 2 files`), its answers, and the run it is streaming right now, whoever started it. Everything is drawn by the row builder `@main` uses, so a message looks like itself wherever it is read: the same bubbles, the same markdown, the same left gutter with the sender's portrait on the first row of each run, the same right-aligned `HH:MM` stamp (dated `M/D HH:MM` once it is not from today, dropped rather than wrapping a narrow row), and the sender's name over each run of messages because a room has more than two speakers. The transcript underneath is untouched: nothing is spliced into it, nothing is reprinted, and whatever arrives while the view is open arrives once, in order, on the way out.
  **The composer stays live and addresses the agent**, in its identity colour: what you type goes to that inbox under your own name, echoed on the next frame as the queued message it is, with no `Sent to` receipt because the message itself is on screen. A `/` line is a message, not a command, and so is a `!` line — the console's parser never sees this draft. `PgUp`/`PgDn` and the wheel scroll; the view follows its own tail until you scroll up.
  **`Esc` has two meanings, in this order**: a cursor in the tree is cleared first, then a *running* agent's turn is aborted (its history is kept and the view stays open), and only then does it return. `Shift+Tab` cycles **that agent's** permission mode, not main's, and the hint row shows it. `Shift+↑/↓` walks the roster and the view follows; with the tree open it walks the tree instead and `Enter` decides. A view closes itself when its agent leaves the roster; a *finished* agent's view stays open, because reading it is the point. `Ctrl+O`, `Ctrl+B`, `Ctrl+T`, `Ctrl+G` and `Ctrl+R` are inert while a view is open, because the surfaces they open cannot be shown over it. A **room** has the same view (the room's log, membership lines included, typing posts to it and joins you first), reached by `f` on its row in the `Ctrl+B` dialog.
  **The background dialog (D107)**: `Ctrl+B` — with no shell command running in the foreground — opens one modal over everything working in the background, and a second `Ctrl+B` (or `Esc`, or `←`) closes it. `Background tasks`, the running counts under it, then the sections that have anything in them: **Agents** (`@scout: reading src/lib.rs…`, its identity colour, `(3 unread)` where its conversation has moved since you read it — in the accent where it said your name), **Shells** (`$ cargo build (running)`, the chip green on `done`, red on `error`), and **Rooms** (`#build: 12 messages · main, scout`, marked `you're not in` where you are not a member). A heading only appears where there is another kind to tell it apart from, and an empty dialog says `No tasks currently running`. The bottom line is `↑/↓ to select · Enter to view · f to foreground · x to stop · ←/Esc to close`, with `f` and `x` shown only where the row can be asked for them. Rows are ordered running-first and then by what moved most recently, and the cursor follows *its row* rather than a position, so `x` cannot stop something that slid under it. `f` foregrounds — the zoomed view, for an agent or a room; `x` stops a running instance (one warning, no confirmation; a background shell cannot be stopped and says so); `Enter` opens a detail that replaces the list (an instance's activity, cost, progress and prompt; a shell's status, runtime, command and output tail; a room's members, count and last words), where `←` goes back and `Esc`/`Enter`/`Space` close — `← to go back · Esc/Enter/Space to close`, plus `x` and `f` where they apply.
  **Rooms (D95)**: a **room** is the only group conversation there is, and its members are any subset of the team — it does not have to include you, and agents form rooms among themselves with the `Channel` tool (creating one seats the creator and nobody else; `user` and `main` are seated only when named). You speak in a room with `#room <message>` from the composer; if you are not a member that posts you in, and joining and leaving are **written into the room where every member sees them**, as dim `· user joined · 14:32` lines, so there is no quiet way to lurk and then speak. `/join #room` makes you a member without saying anything, and `/leave #room` is its counterpart; a room you leave stays readable. Roster changes never wake anybody and never make a serial sender stale.
  **The team is not a conversation** — it is a roster and a room list, and since D107 both live in the `Ctrl+B` dialog: the `Agents` section is who exists and what each one is doing, the `Rooms` section is every room with its members, its message count and a `you're not in` mark on the ones you are not in. The agent tree (`Ctrl+T`) is the always-on version of the same roster, and it leads with `@main`, which the dialog does not list — the console is neither stoppable nor foregroundable, and its conversation is the screen you pressed the key on. `/join #room` joins a room without speaking; posting from a room's zoom joins you first. The D95 team directory and its lifecycle feed are retired: what just happened is on the flow's own dispatch and completion rows, and `/team` answers on the info tier. The D96 observation page is retired too (D108): the zoomed view is the live record, and there is no second, read-only copy of it to open.
  **Attribution (D96/D99/D100)**: one walk recovers who said what from an agent's own history, because the sender is not a field — an absorbed inbox is one flat prompt and only its literal markers survive. Both readers of that walk agree by construction: the **zoomed view** keeps every counterpart, and the **unread accounting** keeps exactly one, the pair of you and the agent. So the task it was created with, main's instructions to it, room relays, mail from other agents, the chases for its silence and the task reminder are all somebody else's conversation and count as nothing of yours; its work comes back the way `@main`'s does — one collapsed activity group per run (`⏺ Searched for 4 patterns, read 2 files`) hung on the agent's own message, not a column of `⏺ Tool(…)` lines; and the `[DM from user]` marker is transport rather than text and never renders. The default flips with the protagonist: in a subagent's record unmarked prose is main speaking, in main's own record it is you, and main was never dispatched so there is no spawn task to file as intake.
  **Unread (D99)**: the count is messages, not history rows, so a turn that made forty tool calls counts as one reply; it wears the mention accent only when the agent actually answered you, and it is drawn in the `Ctrl+B` dialog's chip (`(3 unread)`), where entering the conversation clears it. `@main` has a count of its own — main speaking while a view has the screen counts, and a `⚠ @name · …` failure alert counts *and* wants you. A room says your name case-insensitively (`@User`, `@USER,` reach you; `@username` does not).
  **Avatars**: on terminals that can place kitty images (the same capability that renders inline images), each sender gets one of eight bundled anime-style portraits, 4×2 cells beside the name; elsewhere it falls back to the sender's initial on a colour, and the row count is identical either way. A team member's portrait is pinned in `.bingo/team.json` (`"avatar": "sora"`), so a crew keeps a fixed cast; everyone else gets a face derived from their name. **Every conversation wears the face in a left gutter** (D97, extended to `@main` in D99) — four or five cells taken out of the width before the body wraps, the portrait on the first row of each sender's run and blank on every row after it, with work steps and system lines taking the indentation and no face. The main agent has a **reserved portrait**: it is not in the vocabulary `team.json` may pin and the name hash never lands on it, so the console looks the same in every session and no teammate can be mistaken for it. A terminal that purges its image store (a resize) redraws the faces still on screen; rows already in scrollback leave blank columns where the portrait was. `experimental.chatAvatars` (off by default) governs one remaining thing: whether a subagent's watch row wears that agent's portrait instead of its `◉` — a row inside a grouped dispatch wears neither, because the stem and the name in its identity colour already say who it is. The `@name❯` line a message from an agent leaves (D106) wears no face either: it is one row where a portrait is two, and the colour on the name is the identity.
- **agent team** (project-scoped roster): `.bingo/team.json` (camelCase: `name`/`channel{mode,messageLimit}`/`channels[{name,mode?,messageLimit?,members?}]`/`teams[{name?,path}]`/
  `members[{name,agent,avatar?,model?,provider?,thinking?}]`, members reference AgentDefs; `name` is the name shown on the member's messages, so make it a person's name, and `avatar` pins one of the bundled portraits.
  `model`/`provider`/`thinking` pin the member's engine — which model does which job is part of the formation, so a crew can mix a cheap fast reviewer with a stronger designer; each falls back to the agent definition and then to the session, and a named `provider` other than the session's needs a `model` too.
  `/team validate` checks the engine against this session's providers, so a blueprint that passes still starts) pins multiple roles to one project; started by default at launch
  (`settings.team.autoStart`; `--no-team` turns it off; starting ≠ waking — members stand by Idle at zero tokens,
  only `/team assign` or channel messages start them; idempotency key = instance name, so a repeated start never duplicates an instance —
  it re-reads the definition of every member that is not mid-turn and applies it in place, history intact, reporting `refreshed ×N` beside `spawned ×N`/`reused ×N`,
  so editing a member's `.md` or its blueprint row is stop-optional and delete-free: edit, `/team start`, the next turn runs on the new prompt/model.
  A member mid-turn keeps the definition it started under until the next start; a stopped member comes back Idle). The `/team` command family
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
  still owed an answer, with one main round left to follow up in. The sweep only runs while a crew is actually up; in a project with
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
