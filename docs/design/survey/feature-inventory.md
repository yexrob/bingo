# Feature inventory of the old bingo (survey, 2026-08-28)

> Source: subagent report, archived verbatim. Facts were verified on the date in the title; re-verify before depending on a version.

I have a complete picture. Here is the inventory.

---

# bingo — Feature Inventory

## 1. Product description

bingo is a local, single-binary coding-agent harness written in Rust (~149K LOC across `src/`). It drives an LLM from the terminal to do coding and system work: streaming tool-call loop, a permission gate over every side effect, sub-agents, project-scoped "agent teams," task tracking, context compaction, cross-session memory and an "experience library," plus MCP, skills and shell hooks. The stated design axiom is that "the model only produces tool_use intent; permissions, parallelism, side effects, compaction, memory and the UI are the local harness's job." It targets solo developers who want a Claude-Code-shaped agent they run themselves, against any Anthropic- or OpenAI-protocol endpoint (including ChatGPT/opencode subscriptions via OAuth). One `AppCore` session actor (`src/app/`) is projected into three frontends: the ratatui TUI, `--print` headless, and an experimental `bingo app-server` JSON-RPC for a future GUI.

---

## 2. Feature inventory by area

### 2.1 CLI entry & modes — `src/main.rs`

Only **3 real subcommands** exist; everything else is a global flag or a slash command.

| Feature | What it does | Files | Tag |
|---|---|---|---|
| Interactive TUI (default) | ratatui fullscreen alternate-screen canvas | `src/tui/`, `main.rs:606` | [core] |
| `--inline` / `--fullscreen` | inline keeps finalized output in terminal scrollback (and enables kitty-graphics scrollback images); fullscreen is default, flag kept for compat; mutually exclusive | `main.rs:68-116` | [core] |
| `-p/--print` headless | one prompt (arg or stdin) → prose on stdout, everything else stderr; permission prompts asked on stderr, answered on stdin; `[error] code=… msg=…` contract on non-TTY | `src/print.rs` (306 L) | [core] |
| `--model`, `--permission-mode`, `--continue`, `--resume <session>`, `--no-team` | session bootstrap flags; `--resume` accepts a stem or fragment, refuses non-matches with `SESSION_NOT_FOUND` | `main.rs:61-110` | [core] |
| `bingo share [session] [--public] [--open] [-o path]` | self-contained HTML export; local by default, `--public` uploads to `bingo.ruobin.dev` after a sensitive-content warning; upload failure falls back to local | `src/share.rs` (1070 L), `src/share_html.rs` (1194 L), `main.rs:650-746` | [nice] |
| `bingo update [--check]` | self-update from GitHub Releases: platform asset + `checksums.txt` SHA-256, unpack, same-dir tmp + atomic rename | `src/update.rs` (1024 L) | [nice] |
| `bingo app-server` | JSON-RPC 2.0 / NDJSON on stdio; ~24 request methods + ~40 notifications; server-owned sessions, gapless sequence numbers, server-initiated interactions | `src/app_server/` (protocol.rs, protocol/requests.rs, protocol/notifications.rs, stdio/, session.rs) | [exotic] |
| `bingo app-server generate-schema --out <dir>` | writes Draft-7 schema bundle + method manifest, generated from the Rust types | `src/app_server/schema.rs`, `schema/app-server/` | [exotic] |
| Stray-positional guard | a bare word that isn't a command errors instead of silently launching the chat UI (regression fix for shell aliases) | `main.rs:226-237` | [nice] |
| `src/watch.rs` (1647 L) | **not** a CLI command — the background-task registry: watchable lifecycle, poll intervals, `notify_on`/`notify_regex` conditions, signal-size caps, notification injection at turn boundaries | `src/watch.rs` | [nice] |
| `src/live.rs` (524 L) | **not** a CLI command — the foreground-shell liveness seam: last-5-lines output tail, ~10/s throttle, and the Ctrl+B promote channel | `src/live.rs` | [nice] |
| `src/team_cmd.rs` (972 L) | **not** a CLI command — the `/team` slash-command family implementation | `src/team_cmd.rs` | [exotic] |

### 2.2 Providers & auth

| Feature | What it does | Files | Tag |
|---|---|---|---|
| Anthropic Messages API | default protocol; SSE streaming, `count_tokens` | `src/api/client.rs`, `src/api/sse.rs`, `src/api/providers/anthropic.rs` | [core] |
| OpenAI Responses API | `protocol: "openai"` per named provider; bearer auth, `reasoning.effort`, no count_tokens → local estimate | `src/api/providers/openai.rs` | [core] |
| Named providers | `providers.<name>` with `protocol/apiKey/envKey/apiBaseUrl/supportsImages/oauth/models`; credential order `apiKey > envKey > stored/OAuth` | `src/settings.rs:177-210` | [core] |
| Built-in presets | `codex` (ChatGPT subscription, openai + `oauth.kind: codex`) and `opencode-go` — loginable with zero settings, shown with a built-in badge | `src/api/providers/presets.rs` | [nice] |
| OAuth login | `/provider login <name>` — loopback PKCE browser flow, `--device-auth` for headless/SSH, `--manual <token>`; tokens in `~/.local/share/bingo/auth.json` (0600); eager refresh 5 min before expiry + on 401 | `src/auth.rs` (276 L), `src/api/auth.rs` | [nice] |
| Model catalog | `~/.config/bingo/model-catalog.json` with `builtin` (rewritten on upgrade) + `overrides` (user, prefix-keyed longest-match) for contextWindow/maxTokens/thinking/vision | `src/model_families.rs` (321 L) | [nice] |
| Endpoint model list + cache | undeclared providers pull `/v1/models`, cached 24h in `models-cache.json`; `r` in the menu re-asks | `src/model_cache.rs`, `src/api/models.rs` | [nice] |
| Learned context windows | a context-overflow rejection naming the real ceiling is remembered per provider+model in `learned-windows.json` | `src/api/learned.rs` | [exotic] |
| Vision gating | per-model `vision` flag; image blocks are dropped and replaced with `[image omitted: …]` for text-only models; the model is told its own capabilities in the system prompt | `src/system.rs` (`model_capability_block`), `src/api/image.rs` | [nice] |
| Retry ladder | 10 jittered-backoff reconnects on 429/5xx/stream breaks; silent-response retry once; context-overflow recovery ladder (summarize → truncate tool results → drop oldest) | `src/api/client.rs`, `src/compact.rs` | [core] |

### 2.3 Tools — `src/tool/` (24 built-in tools + dynamic MCP)

Assembled in `src/tools.rs` by depth and experimental flags. All behind one `Tool` trait (`src/tool/mod.rs`) with schemars-generated schemas.

| Tool | Notable behavior | File | Tag |
|---|---|---|---|
| `Bash` | own process group/tree; 120 s default timeout; combined output capped at 48,000 chars (`bashOutputMaxChars`, also the ceiling); **rejects interactive/TTY programs** by a large hardcoded table (top/htop, vim/nano, ranger/mc, lazygit/tig, sudo -i, ssh w/o cmd, docker attach, tmux attach, python/node/irb REPLs, sqlite3/psql/mysql/mongosh/redis-cli); `background: true` returns a task id; periodic commands (`watch`/`while`/`tail -f`) auto-background with `notify_on`/`notify_regex` | `bash.rs` (1922 L) | [core] |
| `Read` | 20,000-char cap with truncation notice; huge files read prefix only; `start_line`/`end_line`; **image files return a real image block** so screenshots/charts can be inspected | `read.rs` (552 L) | [core] |
| `Glob` | `exclude` patterns, `max_depth`; skips `.git`/`target`/`node_modules`/dot-dirs | `glob.rs` | [core] |
| `Grep` | `context`, case-insensitive, whole-word, fixed-string, files-only | `grep.rs` | [core] |
| `Edit` / `Write` | dry-run unified-diff preview computed without touching the file (`preview_diff`); take rewind pre-images | `edit.rs`, `write.rs`, `diff.rs` | [core] |
| `WebFetch` | shared reqwest pool, no redirect follow; `domain:` rules; ~40 pre-approved doc domains auto-allowed | `webfetch.rs`, `src/preapproved.rs` | [core] |
| `WebSearch` | **scrapes `html.duckduckgo.com`** and unwraps DDG redirect URLs — no search API key | `websearch.rs` | [nice] |
| `Agent` | spawn named sub-agent; async by default, `background:false` blocks; per-instance `model`/`provider`/`thinking`; cross-provider fork requires explicit model | `agent.rs` (2089 L) | [core] |
| `SendMessage` | the one speech verb; `to` = `@name` (agent) or `#room`; `urgent`, `summary` (carried but drives nothing), `ack_timeout` (default 300 s, 3 chase rounds) | `agent.rs`, `address.rs` | [exotic] |
| `AgentControl` | main-session only: `list`/`messages`/`stop`/`delete`; message states queued/delivered/answered/dropped | `agent.rs` | [nice] |
| `Team` | main-session only: `status`/`validate`/`start`/`stop`/`save`; **every change forces a user prompt in every mode including bypassPermissions** via `confirm_reason` | `team.rs` (1220 L) | [exotic] |
| `TaskCreate/Update/Get/List` | disk-backed task store shared with the Ctrl+T panel; TaskCreated/TaskCompleted hooks | `task.rs` (762 L), `src/tasks.rs` | [nice] |
| `ExperiencePropose/Commit/Query/Outcome/Forget` | cross-session experience library; BM25 ranking; helpful/harmful outcome counters with SHA-256-bound evidence | `experience.rs` (879 L), `src/experience.rs`, `src/bm25.rs` | [exotic] |
| `AskUserQuestion` | main-session only; multiple-choice, reuses the permission modal | `ask.rs` | [nice] |
| `Skill` | invoke a loaded skill by name | `skill.rs` | [nice] |
| `Channel` | experimental (`experimental.agentChannels`): room create/add/remove members; main + depth-1 subagents only | `channel.rs` (907 L) | [exotic] |
| `mcp__<server>__<tool>` | MCP tools adapted to the same trait; a run of calls into one server folds into `⏺ Called 3 lark tools` | `src/mcp.rs` | [core] |

### 2.4 Permission system — `src/permission.rs` (1092 L)

- **5 modes**: `default` (read-only allowed, rest asks) · `acceptEdits` · `plan` (read-only + tasks, rest denied) · `dontAsk` (non-read-only denied, no prompting) · `bypassPermissions`. [core]
- **Rule syntax** `Tool(content)`, `:*` prefix wildcard, `*` matches all; `permissions.{allow,deny,ask}`. [core]
- **Bash matching**: command split on shell operators (`&& ; |`, `$()`, subshells, braces); deny/ask hits on any sub-command; allow requires **one rule covering every** sub-command; unterminated quotes are never auto-allowed. [core]
- **Path matching**: `~` expansion, relative→cwd, `..` resolution before prefix match. [core]
- **Decision order**: deny → ask → read-only/pre-approved → sensitive-path check → bypass → acceptEdits → allow rules → ask. Sensitive dirs (`.git`/`.claude`/`.vscode`/`.idea`) always prompt, even in bypass. [core]
- **MCP is never exempt** by a server's self-reported read-only hint. [nice]
- **Approval prompt**: shows Bash command lines or a dry-run diff; 3 options — Yes · "Yes, and don't ask again this session" (Shift+Tab; only offered when the narrowest scoped rule, e.g. `Bash(cargo:*)` / `Edit(/dir/)`, would actually stop the gate — in-memory only, never written to settings) · "No, and tell bingo what to do differently" with a feedback row whose text reaches the model. `Ctrl+E` expands preview + shows the exact rule. Enter/digits inert for 400 ms. [core]
- **`confirm_reason`** escape hatch: a tool can force the prompt in every mode; only `deny` outranks it. Currently used only by `Team`. [exotic]

### 2.5 Hooks — `src/hooks.rs` (770 L)

**10 events**: `PreToolUse`, `PostToolUse`, `PreCompact`, `PostCompact`, `UserPromptSubmit`, `Stop`, `SessionStart`, `SessionEnd`, `TaskCreated`, `TaskCompleted`. [nice]
- Shape `{matcher, hooks:[{type:"command", command}]}`; matcher is a whole-string anchored regex, falls back to exact match on compile failure.
- Event JSON on stdin, JSON on stdout. Exit 0 = ok, 2 = blocking (stderr injected into the model), other non-zero = user-visible non-blocking.
- `PreToolUse` may return `{"decision":"deny|ask","reason","updatedInput"}` — can rewrite tool input.
- 60 s timeout (SessionEnd: 1.5 s), process killed on timeout.

### 2.6 MCP — `src/mcp.rs` (1064 L)

- Transports: **stdio** (`command`/`args`/`env`) and **streamable HTTP** (`type:"http"`, `url`, custom `headers`). `sse`/`ws` configured → error on connect (unimplemented). [core]
- Driven by the official `rmcp` SDK; tools listed on connect, adapted to the `Tool` trait, named `mcp__<server>__<tool>`.
- Connections dialed **all at once at session start** (`spawn_connect`), not at first turn — a slow server is simply absent from the turn that raced it. `--print` loses this.
- `/mcp` status · `/mcp enable|disable [name|all]` (persisted) · `/mcp reconnect [name]`. stdio stderr → `~/.local/share/bingo/logs/mcp-<name>.log`, rewritten per connection. [nice]

### 2.7 Skills — `src/skills.rs` (813 L)

- Load order: user `~/.config/bingo/skills/` → project `.bingo/skills/` (walked upward, nearest first) → bundled `guide` (fallback only; disk skills of the same name win). [nice]
- One dir per skill: `<name>/SKILL.md`, YAML frontmatter (`description`/`when_to_use`/`arguments`) + markdown body, with argument substitution.
- Invoked by the model via `SkillTool`, or by the user as `/skill-name [args]`.
- **Exactly one bundled skill**: `guide` (`src/skills/bundled/guide.md`, 565 lines) — bingo's own 25-page manual, compiled into the binary, that the model consults for any question about bingo. [exotic]

### 2.8 Sessions / transcript / compaction / memory / rewind

| Feature | Detail | Files | Tag |
|---|---|---|---|
| Transcript | `~/.local/share/bingo/transcripts/<project>-<ts>.jsonl`, one Message per line; corrupt lines skipped with a count; file opened at session start so it can be listed/resumed/renamed before anything is said | `src/transcript.rs` (966 L) | [core] |
| Resume | `--continue` (latest *used* session), `--resume <stem-or-fragment>`, `/resume [name]` picker, `/rename`; every interactive exit prints `bingo --resume <stem>` | `main.rs`, `src/share.rs::resolve_transcript` | [core] |
| Retention / `/gc` | 30-day TTL + newest-100-inactive cap + 24 h activity grace; share snapshots follow their transcript; prompt-history same TTL, 100-file cap; runs at startup and on demand | `src/storage.rs` (666 L) | [nice] |
| Context budget | per-model window − output budget; auto-compact at 90% of the effective window; 20k headroom warning; local token estimation (ASCII ≈4 chars/token, CJK ≈1, image flat 1600) when `count_tokens` is unavailable; every response's usage re-anchors the count | `src/budget.rs`, `src/context_usage.rs`, `src/token_rate.rs` | [core] |
| Compaction | summarize old + keep recent 12 (also token-capped); split point advances past tool_result boundaries; appends a **summary marker** rather than rewriting the file (so `/share` still exports the original); fuse after 3 consecutive failures | `src/compact.rs` (1329 L) | [core] |
| Memory | memdir auto-memory `~/.config/bingo/memdir/<project>-<path-hash>.md` + project `CLAUDE.md`/`AGENTS.md` as system memory; per-turn auto-recall of ≤3 experiences + memory facts as a system-reminder | `src/memory.rs`, `src/system.rs` | [nice] |
| **Rewind** | Esc-Esc on empty composer → list of user turns with file-change counts → 5 options: restore code+conversation / conversation / code / summarize-from-here / never mind. Pre-images taken by Edit/Write once per file per turn under `~/.local/share/bingo/rewind/`, git-independent, evicted past 50 MB or 200 turns, files >8 MB recorded unsnapshotted. **Does not cover anything Bash wrote.** | `src/rewind.rs` (514 L), `src/tui/rewind_ui.rs` | [exotic] |
| Images-as-session-state | pasted/attached images stored content-addressed in `~/.local/share/bingo/assets/`, marker rows are a transcript sidecar so `#[image N]` resolves after resume; swept after 30 days once no index names them | `src/api/image.rs`, `src/app/asset.rs` | [nice] |

### 2.9 Multi-agent features — the concept stack

Six overlapping concepts. In dependency order:

1. **Sub-agent** (`src/agents.rs` 2498 L, `src/tool/agent.rs`) — an `Agent` tool spawn. Has a name, its own history, its own model/provider/thinking. Async by default; completion notification injected into the parent's next turn. Depth ≥1 keeps `Agent` (can spawn further) but loses `AgentControl`, `AskUserQuestion` and `Team`. [core]
2. **Named agent definitions** (`~/.config/bingo/agents/*.md`, `.bingo/agents/*.md`) — frontmatter `name/description/model/provider/thinking/inherit_system` + body as system prompt. Project layer wins on name clash. [nice]
3. **Room / channel** (`src/channels.rs` 2571 L, `src/tool/channel.rs`) — the only group conversation. Created by `Channel`; creating one seats only the creator. Members speak with `SendMessage(to:"#room")`; messages enter every member's inbox. `serial` mode bounces stale posts back with new messages attached; `free` allows interleaving. Budget gates freeze the room. An `@name` is a **recorded debt** with a 5-minute watchdog that re-asks in the room. Gated by `experimental.agentChannels`. [exotic]
4. **Agent team / crew** (`src/team.rs` 3151 L, `src/team_cmd.rs`, `src/tool/team.rs`) — `.bingo/team.json` pins a roster of roles to a project: `name`, `channel{mode,messageLimit}`, `channels[]`, `teams[{name?,path}]` (recursive org chart), `members[{name,agent,avatar?,model?,provider?,thinking?}]`. Auto-started at launch (`team.autoStart`, default true); members idle at zero token cost until assigned. `.bingo/team-norms.md` is the crew's prose working agreement, injected as a system block to every member and hire. **Crew-first rule**: where a crew is pinned, an `Agent` spawn is a *temporary hire* — never entering team.json, listed as `hire`, recorded in `decisions.md`, and swept once idle. [exotic]
5. **Team memory** — `~/.config/bingo/teams/<project-hash>/<branch>/<team>/<name>.md` + `.json`, scoped by project path + git branch, plus append-only `decisions.md`. Members are **pointed at** their transcript, not preloaded with it. `/team memory list|gc`. [exotic]
6. **Experience library** (`src/experience.rs` 1014 L) — per-project reusable operational knowledge in `~/.config/bingo/experience/<project-key>/entries/<id>.md`. Entry = trigger keywords / summary / steps / verify / evidence + helpful-harmful counters + append-only outcome history. Status lifecycle `active → degraded → stale` (stale exits injection, stays queryable). Active index (≤10) injected at session start; per-turn auto-recall of ≤3. Project key derived from git remote URL → git root → normalized path. [exotic]
7. **Tasks** (`src/tasks.rs`) — a plain disk-backed todo list, per-session (keyed on transcript stem), surfaced in the Ctrl+T panel and via `TaskCreate/Update/Get/List`; owner and `blocked by` are display-only. [nice]

Notification/rendering policy across these: a **dispatch** row `◉ @name: task` with last-3-activities → settles to `Done (N tool uses · Nk tokens · time)`; a **completion** dim line only for a run this turn's own `Agent` call dispatched; a **failure** `⚠ @name · reason` + attention ring (coalesced for bursts); everything else (start/idle/stop/cancel/room post/mail to main) writes **nothing**. Main's mail is digested on a 2 s debounce (15 s ceiling).

### 2.10 TUI — `src/tui/` (~50 modules)

| Area | Features | Files |
|---|---|---|
| Composer / input | multi-line (`\`+Enter, Ctrl+J, Shift+Enter via kitty protocol); `!` sticky bash mode; bracketed-paste detection with `[Pasted text #N +M lines]` collapse and a key-burst fallback; readline editing (Ctrl+A/E/W/U/K/D, Alt+B/F/D/Backspace with path-segment word stops); 10-entry kill ring (Ctrl+Y / Alt+Y); Ctrl+S stash; Ctrl+_ undo; Ctrl+G / Ctrl+X Ctrl+E `$VISUAL`/`$EDITOR` compose as one undo step | `input.rs`, `composer.rs`, `history.rs` | [core] |
| Key bindings | **34 bindings** in one table read by the `?` panel, footer and docs | `keys.rs` (BINDINGS) | [core] |
| Slash commands | **24 commands** + 4 aliases (`/?`, `/reset`, `/new`, `/quit`) + `/skill-name`; the table lives in `app/action.rs`, the dropdown only ranks (prefix > substring, shorter wins) | `slash.rs`, `src/app/action.rs` | [core] |
| Argument completion | past a command name the dropdown completes its **argument** from the same data the command validates against (models, themes, think levels, sessions, provider names/subcommands, rooms, MCP servers); free-form args offer nothing | `complete.rs`, `action.rs::ArgumentSpec` | [nice] |
| Mentions | `@` at word start → fuzzy dropdown over project files (git-tracked + untracked-not-ignored, else bounded walk, 5000 cap) and running agents; line-initial `@name`/`#room` is a **direct send** bypassing the model, receipted by a transient `Sent to @name` | `complete.rs`, `intent.rs` | [exotic] |
| Themes | dark/light/auto, fully RGB with 256-color approximation fallback; 3 text tiers (primary/secondary/muted); `/theme` live | `theme.rs` | [core] |
| Markdown / code / diff | markdown renderer; syntax highlighting for **21 language tags** mapping to 19 grammars (rs, py, js, ts, json, sh, toml, yaml, md, diff, c, cpp, go, java, css, html, sql, xml, lua, rb) with a thread-local memo; unknown/missing fence = monochrome; diffs carry old/new line-number gutters on every surface | `markdown.rs`, `highlight.rs`, `tool/diff.rs` | [core] |
| Images | kitty graphics protocol with **Unicode placeholders only (U=1)**; capability probe (`a=q` + DA + `14t`); tmux passthrough auto-enabled (`allow-passthrough all`); WezTerm/Konsole get the `#[image]` text placeholder with a one-time notice; size/pixel caps; `#[image ✗ load failed]` on fetch failure | `gfx.rs`, `images.rs` | [exotic] |
| Transcript view | Ctrl+O alternate-screen pager over the whole session with every tool output and thinking block expanded; `ctrl+e` collapse, `/` search with n/N, j/k/PgUp/PgDn/g/G, `o` opens an image in the desktop viewer, q closes | `transcript.rs`, `bufferview.rs` | [nice] |
| Pages / "zoom" | `Enter` on a conversation row turns the screen into that agent's or room's page, drawn by the same pipeline as `@main` and banked into scrollback; console grammar (`/`, `!`, `@name`, `#room`) keeps working; `/compact` on a page compacts *that agent*; Shift+Tab cycles *that agent's* permission mode; Esc has 4 ordered meanings | `zoom.rs`, `conv.rs`, `conversation.rs` | [exotic] |
| Conversation rows ("roster") | constant list under the composer: `● @main`, then instances, then rooms; status, unread badges (`•` / `•3` in accent), `owes #build #7`, `waiting on you (permission)`; max 3 rows with a scrolling window; `↓` at history end falls onto them, `k` stops one | `roster.rs`, `tree.rs` | [exotic] |
| Background dialog | Ctrl+B (with no foreground shell) → modal with **Agents / Shells / Rooms** sections; `Enter` detail, `f` foreground, `x` stop; ordered running-first then by recency, cursor follows its row | `background.rs` | [nice] |
| Pickers / menus | two-level model picker (provider → model, `r` refetch), provider picker (`s` = session-only, Enter persists), think-level picker, theme picker, session picker, `/images` picker; `1-9` jump | `picker.rs`, `model_menu.rs`, `chat_menus.rs` | [nice] |
| Status layer | running row `✻ {verb}… (esc to interrupt · Ns · ↓ N tokens)` with a per-120 ms glyph cycle, a glimmer, an eased token count, and a 3-second warning color; live `N tok/s`; 4-cell context bar counting down to the auto-compact trigger | `chrome.rs`, `activities.rs`, `token_rate.rs` | [nice] |
| Motion gate | `motion: auto|off` (or `BINGO_NO_MOTION`) — one switch over every animation; informational colors keep changing | `motion.rs` | [nice] |
| Notifications | 5 channels: auto / bell / iterm2 (`OSC 9`) / kitty (`OSC 99`) / ghostty (`OSC 777`) / off; auto-detects from `TERM_PROGRAM`/`TERM`; fires on waiting permission, ≥10 s turn complete, turn failed, agent-needs-you; terminal title tracked via `OSC 2` and handed back on exit *including after a panic*; tmux passthrough envelope | `notify.rs` | [nice] |
| Avatars | `experimental.chatAvatars` (off by default): 8 bundled anime portraits, 4×2 cells in a left gutter on image-capable terminals, initial-on-color chips elsewhere; team members pin one in team.json; main has a reserved portrait | `avatar.rs`, `assets/avatars/` | [exotic] |
| Ask/permission modal | shared modal for approvals and `AskUserQuestion`, with feedback row | `ask.rs` | [core] |
| Selection / folds / mouse | wheel scroll, click-to-expand folds, text selection | `selection.rs`, `collapse.rs`, `chat_feed.rs` | [nice] |

### 2.11 Configuration — `src/settings.rs` (1294 L)

**23 top-level keys** (`KNOWN_KEYS`), lint-warned on typo. Three layers: user `~/.config/bingo/settings.json` (XDG-aware) < project `.bingo/settings.json` < local `.bingo/local.json`. Later layer overrides per key, **except**: `permissions` lists and `disabledMcpServers` *accumulate*, `providers` merges per name, `experimental` latches on, `mcpServers` is replaced wholesale.

| Key | Type |
|---|---|
| `apiKey` | `Option<String>` |
| `apiBaseUrl` | `Option<String>` |
| `providers` | `HashMap<String, ProviderConfig{apiKey?, apiBaseUrl, protocol?, oauth?{kind, account?}, supportsImages?, envKey?, models?}>` |
| `provider` | `Option<String>` |
| `model` | `Option<String>` |
| `models` | `Option<Vec<ModelEntry>>` — id string or `{id, display?, contextWindow?, maxTokens?, thinking?, vision?}` |
| `sendImages` | `Option<bool>` (default true) |
| `thinkingLevel` | `Option<String>` — `off\|low\|medium\|high\|xhigh\|max` |
| `permissionMode` | `Option<String>` — 5 modes |
| `theme` | `Option<String>` — `auto\|dark\|light` |
| `motion` | `Option<String>` — `auto\|off` |
| `notifications` | `Option<String>` — `auto\|bell\|iterm2\|kitty\|ghostty\|off` |
| `cacheControl` | `Option<bool>` (default false) |
| `respondToBashCommands` | `Option<bool>` (default true) |
| `bashOutputMaxChars` | `Option<usize>` (default & max 48,000) |
| `shell` | `Option<String>` (macOS `/bin/zsh`, Unix `/bin/bash`, Windows `powershell.exe`) |
| `hooks` | `HooksConfig` — 10 `Vec<HookRule{matcher, hooks:[Hook{type, command}]}>` |
| `mcpServers` | `HashMap<String, McpServerConfig{type?, command?, args, env, url?, headers}>` |
| `disabledMcpServers` | `Vec<String>` |
| `permissions` | `PermissionRules{allow:Vec<String>, deny, ask}` |
| `experimental` | `{agentChannels: bool, channelMessageLimit: Option<u64> (500), agentMessageLimit: Option<u64> (50), chatAvatars: bool}` |
| `team` | `{autoStart: Option<bool>}` (default true) |
| `share` | `{baseUrl: Option<String>}` (default `https://bingo.ruobin.dev`) |

Adjacent config files not in settings.json: `~/.config/bingo/model-catalog.json`, `~/.local/share/bingo/auth.json`, `models-cache.json`, `learned-windows.json`, `update-check.json`, `.bingo/team.json`, `.bingo/team-norms.md`.

### 2.12 Anything else

- **Action table** (`src/app/action.rs`) — **28 actions** (`session.reset/rename/gc/share/cd`, `conversation.compact/rewind`, `model.select`, `provider.select/login/logout`, `thinking.select`, `permission.mode/ruleAdd/ruleRemove`, `mcp.enable/disable/reconnect`, `skill.invoke`, `team.start/assign/stop/scaffold/memoryGc`, `room.join/leave`, `command.promote`, `theme.set`), each with typed arguments and a catalog source. This is the single table `/help`, the app-server's `action/list`, and the completion dropdown all read. [core]
- **Parity ledger** (`src/app/parity.rs`) — a checked table saying, for every slash command, action, notification, submission branch and terminal event, whether it lives in the core or the terminal. [exotic]
- **Instant vs queued commands** — read-only commands run mid-turn; mutating ones queue behind the turn.
- **Queue / steering** (`src/app/queue.rs`, `src/app/submit.rs`) — Enter while busy queues; the running turn folds plain queued messages into its context at the next tool call (`↪`), the rest sends at turn end; queued messages are pullable back with `↑`.
- **`/cd <dir>`** — switches the session-owned working directory without changing process cwd; re-resolves tools, project skills, agent defs, team lookup, experience keys, memory, image paths.
- **Panic log** — `~/.local/share/bingo/logs/panic.log`, appended across sessions; `TURN_LOST` retriable by Enter on the empty composer.
- **Startup update check** — async, 24 h cached, silent on failure, not in `--print`; the welcome card shows a "New version available" notice that breathes for 9 s.
- **Stable error contract** — `[error] code=SCREAMING_SNAKE msg=<≤200 chars>` on non-TTY (`src/error.rs`).
- **Windows support** — native msvc target, PowerShell `-Command` vs `-c` dialect detection reported to the model and to JSON clients (`src/platform.rs`).

---

## 3. Feature count summary

| Thing | Count |
|---|---|
| CLI subcommands | **3** (`share`, `update`, `app-server`) + 1 nested (`app-server generate-schema`) |
| CLI global flags | **8** (`--print`, `--fullscreen`, `--inline`, `--model`, `--no-team`, `--permission-mode`, `--continue`, `--resume`) + `prompt` positional |
| Slash commands | **24** + 4 aliases + N dynamic `/skill-name` |
| Actions (core action table) | **28** |
| Built-in tools | **24** (19 always + `SendMessage` + 3 main-only + `Channel` behind a flag) + unbounded `mcp__*` |
| Settings keys (top-level) | **23** |
| Hook events | **10** |
| Permission modes | **5** |
| Key bindings (documented table) | **34** |
| Notification channels | **6** (incl. auto/off) |
| Syntax-highlighted languages | **19** grammars / 40+ fence aliases |
| Provider protocols | **2** (anthropic, openai) + **2** built-in presets |
| Thinking levels | **6** |
| Bundled skills | **1** (`guide`) |
| app-server methods / notifications | **~24 requests / ~40 notifications** |

---

## 4. Observations

**Explicitly experimental or unreleased**
- `bingo app-server` — README §App-server: *"Status: experimental… no released consumer yet, so it carries no compatibility promise."* Non-goals list multi-client, durable journal, network transports. ~2 dozen methods, a committed schema bundle, and a full design doc (`notes/design/gui-app-server.md`) with **zero consumers**. Highest cost-to-value ratio in the repo.
- `experimental.agentChannels` — rooms/channels are off by default. `src/channels.rs` is 2571 lines plus `tool/channel.rs` 907 lines, plus the mention-debt watchdog, serial-mode staleness bouncing, budget freezing, and room pages/rows in the TUI — all behind a default-off flag.
- `experimental.chatAvatars` — off by default; 8 bundled anime portraits, a gutter renderer, a reserved main portrait, and a known unfixable degradation (scrollback rows keep blank columns after a terminal image-store purge).

**Superseded / legacy residue**
- `--fullscreen` is documented as "retained for compatibility" — it is now the default, so the flag is a no-op that only exists to conflict with `--inline`.
- `/share --local` is parsed and deliberately ignored ("the flag predates it and stays harmless").
- The v1 JSON protocol flags (`--json-events`, `--probe`, `--inspect`, `--session`) were deleted (D140) with a test asserting they no longer parse; `notes/gui-json-events-legacy-check.md` and `notes/json-events-*.md` are its fossils.
- `SendMessage`'s `summary` field is "accepted and carried but currently drives no surface — the `@name❯` line and the tree preview it once fed are retired."
- `keys.rs` has a test (`the_panel_names_no_retired_surface`) guarding against six removed surfaces: the workspace modal, the conversation switcher, `/open`, the team directory, "the record", "perspective" — i.e. at least six TUI concepts have already been built and deleted.
- `zoom.rs` documents itself as the vestige of a retired alt-screen modal (D105 → v6 → D135), now down to "the target vocabulary, the accounting hand-off, and the tree's enter."

**Half-finished / thin**
- `sse` and `ws` MCP transports parse but error on connect.
- `WebSearch` scrapes DuckDuckGo HTML with regex — no API, fragile by construction.
- Rewind's blind spot is large and acknowledged: **anything a Bash command wrote is not recoverable**, which in a coding agent is most writes once the model reaches for `sed`, `git checkout` or a build script.
- opencode-go's chat/completions models "need an adapter that is not implemented yet."
- Only one bundled skill exists (`guide`), and it is 565 lines of prose documenting bingo to itself — effectively a second copy of the README that AGENTS.md requires be kept in sync by hand.

**Looks bolted-on / disproportionate**
- The multi-agent stack is the single largest feature area: `team.rs` (3151) + `channels.rs` (2571) + `agents.rs` (2498) + `tool/agent.rs` (2089) + `tool/team.rs` (1220) + `tool/agent_tests.rs` (2482) + `team_cmd.rs` (972) + `tool/channel.rs` (907) ≈ **16K lines**, ~11% of the codebase, for features that are project-scoped, mostly default-off (channels/avatars) or require a hand-written `.bingo/team.json`. The recursive org chart (`teams[{name?,path}]`, "reaching a department from the root"), the mention-debt watchdog, the ack-chase state machine (queued/delivered/answered/dropped, 3 rounds, 300 s), and the "temporary hire" sweep are each a product on their own.
- The `Team` tool's `confirm_reason` is a bespoke permission-system escape hatch with exactly one caller.
- `share.rs` + `share_html.rs` (2264 lines) plus a hosted service at `bingo.ruobin.dev`, a PRD, a page design, a CSS template and a review script in `notes/design/` — a whole publishing product inside a CLI.
- `experience.rs` + `tool/experience.rs` + `bm25.rs` (2147 lines) implements a bespoke BM25 index with CJK bigrams, a 3-state lifecycle, and SHA-256-evidence-bound outcome counters — for a knowledge base that only writes when the model chooses to propose one.
- `watch.rs` at 1647 lines for background command tracking, with its own condition-matching DSL (`notify_on`/`notify_regex`), signal-size caps, and hit-line aggregation.
- The README/guide themselves are a signal: 1013 + 565 lines of dense prose with design-decision IDs (D31, D114, D137, D156…) leaking into user-facing docs. Many features are documented primarily by what they *don't* do, which usually means the behavior was iterated on more than once.
