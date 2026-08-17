# bingo Technical Decision Record

> Goal: a Rust agent CLI.
> Decision date: 2026-08-04. All facts verified against crates.io/docs.rs/GitHub as of 2026-08.

## Architecture Overview

```text
┌─────────────────────────────────────────────────────────────────────┐
│L1  CLI entry · clap (D8)                                            │
│  --version/--help fast path → env sanitize → settings pre-read →    │
│  MCP connect → branch: TUI (ratatui, D26) ｜ headless --print        │
└───────────────────────────────┬─────────────────────────────────────┘
                                ▼
┌─────────────────────────────────────────────────────────────────────┐
│L2  Config layer (D9)                                                │
│  settings.json user/project/local · permissionMode · hooks config   │
│  mcpServers · feature flags (compile-time + runtime switch)         │
└───────────────────────────────┬─────────────────────────────────────┘
                                ▼
┌─────────────────────────────────────────────────────────────────────┐
│L3  Interaction layer (D4)                                           │
│  Chat component ← stream events │ permission card ← canUseTool      │
│  activity hints (thinking/tool) │ SlashCommandMenu · AgentView      │
└───────────────────────────────┬─────────────────────────────────────┘
                                ▼
┌─────────────────────────────────────────────────────────────────────┐
│L4  Agent core (D7)                                                  │
│  queryLoop:                                                         │
│  system assembly (D10) → Messages API stream (D1) → tool_use        │
│  → concurrent queue (D7) → tool_result backfill → re-request        │
│  → end_turn ｜ max_tokens continuation ｜ compact (D12)               │
└───────────────┬─────────────────────────────┬───────────────────────┘
                │
                ▼
┌─────────────────────────────┐   ┌───────────────────────────────────┐
│ Tool Registry (D2)          │   │ MCP adapter (D3)                  │
│ trait Tool + schemars schema│   │ rmcp client → same Tool trait     │
│ Read / Bash / Edit / ...    │   │ stdio ｜ streamable HTTP           │
│ exec: tokio::process (D5)   │   │                                   │
└─────────────────────────────┘   └───────────────────────────────────┘
                │
                ▼
┌─────────────────────────────────────────────────────────────────────┐
│L5  Cross-cutting                                                    │
│  Permission gate (D2): modes × rules × UI approval                  │
│  Hooks: Pre/PostToolUse · Session · Stop · Compact (shell, JSON)    │
│  Transcript (D11) · token budget (D12) · subagents (D14)            │
│  Deferred: Sandbox ｜ telemetry (D13) ｜ plugins ｜ worktree/teammate  │
└─────────────────────────────────────────────────────────────────────┘
```

Round-trip principle: `the model only emits tool_use; the local harness owns permissions, parallelism, side effects, compaction, memory, and UI.`

## Decisions

### D1. Messages API client: hand-rolled (reqwest + SSE)

- Anthropic still ships no official Rust SDK (confirmed: `claude-agent-sdk-rust` returns 404); every community SDK on crates.io is unmaintained or toy-grade.
- Hand-rolled scope: SSE event parsing (accumulate `input_json_delta` until `content_block_stop`, then parse), stop_reason, exponential-backoff retries (429/529, honoring retry-after, capped at 2-3 attempts).
- 2026 API status: prompt caching is GA (top-level `cache_control`, no beta header needed); thinking is configured via `adaptive` (interleaved applies automatically); thinking blocks must be returned verbatim, including the signature.

### D2. Tool protocol: trait + schemars, permissions as a separate gate

- The Zod equivalent: `schemars` generates inputSchema (`schema_for::<T>()`, single source of truth for the input struct; model-returned params are validated by serde deserialization, and errors are fed back to the model via is_error. No jsonschema validator is introduced — nine static, simple schemas gain nothing, and serde already covers type errors).
- Tools use a trait (name/aliases + inputSchema + call + isConcurrencySafe + isReadOnly/isDestructive + validateInput + interruptBehavior), defaulting to fail-closed (not concurrency-safe, allow).
- `checkPermissions` stays out of the trait — permissions are cross-cutting and go through a unified permission gate (cf. goose 2026: tool registration via the rmcp model, permissions behind a separate `Permission` gate).

### D3. MCP: rmcp (the official rust-sdk)

- `modelcontextprotocol/rust-sdk`, official, 3.1.0 (2026-07-31). Client capabilities: **stdio** (TokioChildProcess) + **streamable HTTP** (`transport-streamable-http-client-reqwest`; rmcp 3.1 pins reqwest 0.13, so bingo's reqwest moves to 0.13 to unify the stack); OAuth exists as an SDK capability but is not enabled (static `headers` cover common auth).
- mcpServers config → connect → list_tools → adapt into the same Tool trait (isMcp + mcpInfo).
- Don't touch other MCP crates (mcp-server / mcplease have no client or are rudimentary).

### D4. TUI: iocraft (superseded by D26)

- `iocraft` 0.8.4 (declarative hooks + flexbox + fullscreen render loop); the component architecture is isomorphic to ink; `rsmarkdown-core` (streaming markdown parsing) is kept for AST → line rendering.
- bingo only wires components: the Chat component consumes stream events, the permission card consumes canUseTool, Task → tasks, Agent tool → agents.

### D5. Runtime and processes

- Single tokio runtime; crossterm EventStream + `tokio::select!` event loop; tool execution via JoinHandle/AbortHandle; input interruption = select + watch channel.
- Bash tool: `tokio::process` + `/bin/zsh -c` (same no-pty approach as goose; shlex unused); an interactive shell would come later with `portable-pty`.

### D6. Token counting: official count_tokens API

- Claude's tokenizer is closed-source; `claude-tokenizer` is unmaintained and inaccurate.
- Budget display goes through `POST /v1/messages/count_tokens`; local estimation falls back to `claude-tokenizer` but is never treated as authoritative.

### D7. Main loop semantics

- `stop_reason === 'tool_use'` is unreliable; go by the tool_use blocks actually present.
- Concurrent execution queue: safe tools run in parallel (cap 10); non-safe tools run serially and never overtake an unfinished write.

### D8. CLI entry: clap + explicit startup chain

- Argument parsing with `clap` (derive).
- Startup chain: `--version`/`--help` fast path (no heavy module loading) → env sanitization → settings pre-read → MCP connect → branch to interactive (TUI) or headless (`--print`/`-p`).
- Initial CLI surface: `--model`, `--permission-mode`, `--continue`, `--add-dir`, `-p/--print` (headless).

### D9. Config layering: settings.json + feature flags

- Config layers: user (`~/.config/bingo/settings.json`) / project (`.bingo/settings.json`) / local (`.bingo/local.json`), shallow-merged; local is never committed.
- settings carries: permissionMode, hooks config, mcpServers, theme/notification preferences.
- Feature flags: compile-time `feature()` (the bundle DCE equivalent) + runtime switches; new capabilities default off. No GrowthBook — a local CLI doesn't need remote rollout.

### D10. System prompt assembly + prompt caching strategy

- The system prompt is assembled in segments: base (role/rules) → tool description → CLAUDE.md memory layer (managed/user/project/--add-dir) → session extras.
- The segment order is the caching strategy: cache tools → system in order (`cache_control: ephemeral` placed at the tail breakpoints of system and messages, minimum cache size 512~4096 tokens), guaranteeing that only the tail changes across turns.

### D11. Transcript and session storage

- A transcript (JSON Lines) is persisted per session; `--continue`/`--resume` resumes from it.
- Compact is bounded by the transcript: archive before compacting, then replace with a generated summary segment.
- Retention is enforced at startup and by `/gc`: transcript mtime gets a 30-day TTL plus a latest-100 inactive cap with a 24-hour activity grace, and its share snapshot follows transcript deletion. Prompt-history files use the same TTL with a 100-file cap. Public HTML exports and task lists are outside this policy and are never deleted.

### D12. Token budget management

- Output side: dynamic max_tokens management (upgrade path) and a per-turn output budget (continuation on overflow).
- Input side: token counting via D6; hitting a threshold triggers autoCompact.

### D13. Sandbox and telemetry: explicitly out of scope (initially)

- No sandbox — the permission gate + modes are the security boundary; logged as a future item.
- No telemetry — only local debug logging. Avoid an analytics dependency.

### D14. Multi-agent boundary

- Phase one only covers subagents (the Agent tool recursing into queryLoop; subagents keep their own message history).
- No worktree/teammate/team-collaboration surface (product surface, not core harness); add later as needed.

### D15. Task tracking: v2 Task tool family

- **Tool surface**: `TaskCreate` (subject/description/activeForm?/metadata?, one task per call, returns `{task:{id,subject}}`), `TaskUpdate` (taskId + optional fields, **incremental patch semantics**; status gains `deleted` for permanent removal that also cleans up references from other tasks), `TaskGet`, `TaskList` (filters `metadata._internal`, completed tasks excluded from blockedBy). Shared attributes: shouldDefer, no permission check, renderToolUseMessage null (UI goes through the task area), expanding the task area when a tool is called.
- **Storage**: on disk at `~/.local/share/bingo/tasks/<listId>/<taskId>.json`, one file per task, **persisted across sessions**; numeric ids increment (max+1); fault-tolerant per-entry parsing on read; in-process mutual exclusion (bingo is single-process; no cross-process concurrency).
- **Input repair layer**: repairs near-miss key names (title/name→subject, content→description, active_form→activeForm, unwraps a task wrapper, backfills a missing description); misuse (tasks/todos array params, Agent params) returns guidance text.
- **Hooks**: new `TaskCreated` / `TaskCompleted` events; TaskCompleted's blockingError can **reject** the completed status.
- **Reminder injection**: a `task_reminder` message with thresholds `TURNS_SINCE_WRITE=10` / `TURNS_BETWEEN_REMINDERS=10`; injected when the tool list contains TaskUpdate; injected as a meta user message + "NEVER mention this reminder".
- **What bingo borrows**: v2's incremental semantics (v1's full-list overwrite is a lost-update breeding ground under concurrency) + disk persistence + a single-file lock (same shape as the transcript file convention); **drops** owner/swarm assignment (D14 ruled out teammate) and metadata merge (v1 support is enough); the TaskCompleted blocking hook aligns with existing hook semantics; reminder thresholds taken as-is: 10/10.

## Implementation checklist (in implementation order)

1. Minimal headless loop: API client + queryLoop + Read/Bash tools + permission gate (D1/D2/D7/D8)
2. Concurrent queue + Hooks runtime (shell hook, JSON stdin/stdout)
3. System prompt assembly + transcript storage + token budget (D10/D11/D12)
4. MCP integration (rmcp → Tool adaptation)
5. TUI wiring (ratatui, D26) + slash commands
6. Compact + CLAUDE.md/memdir memory + subagents (Agent tool)
7. Later: sandbox, plugins, worktree/teammate (deferred items from D13/D14)

## References

- goose (aaif-goose/goose, pure-Rust agent; permission gate + execution + agents structure)
- ratatui and crossterm documentation (current rendering/runtime stack, D26)
- [`notes/design/feedback-states.md`](./design/feedback-states.md) (feedback-state spec: unified design conventions for user-visible feedback states, covering both the GUI/CLI sides and the error-code contract)

## Decisions (continued)

### D16. TUI rendering layer: iocraft declarative components (superseded by D26)

- **Rationale**: declarative components (hooks + flexbox + fullscreen render loop) are the efficient shape for terminal UI; after the migration, bingo's UI architecture is isomorphic to mainstream agent TUIs, so layouts can be matched 1:1 against the reference implementation.
- **Trade-offs**: `rsmarkdown-tui` (the App/Component framework, ~12k lines) is dropped entirely; `rsmarkdown-core` (streaming markdown parsing, display-independent) is kept. The whole rendering layer is rewritten as iocraft elements (no ratatui Line adapter bridge kept).
- **New structure**: `src/tui/` — `chat.rs` (state machine + document row building; event semantics and collapse logic kept as-is), `line.rs` (styled line model), `theme.rs` (dark tokens), `markdown.rs` (AST → lines; ported from renderer.rs), `activities.rs` (activity data + ported header/collapse layouts), `components.rs` (iocraft root component + transcript).
- **Layout reference points**: single-column layout (no sidebar) = sticky header + scrollable transcript + task list + notification line + input line + `╰──╯` border + 1-line footer; permission requests render at the bottom of the transcript (non-modal); message blocks marginTop=1; footer left = mode badge (⏸ plan / ⏵⏵ accept edits) + shortcut byline, right = model name.
- **Key pitfall (verified)**: iocraft `State::write()` marks dirty on every deref → an unconditional write() in the component body causes infinite re-render (observable in the mock terminal as ~350 frames/sec of idle spinning). Writes must be "guarded": write only when the layout size or the document is dirty; dirty is set by event/tick consumers.
- **Interaction parity**: mouse-click collapse/expand (local coords from `use_local_terminal_events` → doc line numbers), wheel, ctrl+o global expand, j/k/G/g/PageUp/Down scrolling, Esc/Ctrl+C interrupt while busy, numeric-key selection for permissions. Tested through the dual channels `mock_terminal_render_loop` + line-level `build_rows`.

### D17. TUI rendering fixes: diff residue and small-window misalignment (verified on iocraft 0.8.4)

- **Symptoms**: ghosting/duplicate lines on real terminals — after a "stuck" thinking running-state, the next line re-renders; the input line `❯ ▋` leaves residue in several places; short and long versions of a long reply body coexist; worse when the window shrinks.
- **Root causes (three verified chains)**:
  1. iocraft's fullscreen line diff (`write_canvas`: per-line MoveTo + rewrite) leaves stale lines when **line numbers shift** (markdown wrap line counts change, messages added/removed, sticky appears) — the write sequence itself is correct (verified by per-frame decode), but the terminal state drifts from the in-memory prev.
  2. `use_terminal_size` depends on Resize events; when events are lost/delayed, the canvas height mismatches the real terminal (tmux winsize lag: pane at 16 lines while bingo reads 24) → MoveTo out of bounds → the terminal scrolls → misalignment.
  3. The sticky header occupies 1 layout row → content shifts as a whole when it appears/disappears.
- **Fixes (bingo side; iocraft untouched)**:
  1. Sticky becomes an **absolutely-positioned overlay** (no layout space; `Position::Absolute` inside Transcript).
  2. **Set the `FORCE_FULL_REDRAW` global flag when the doc line count changes or on TurnEnd**; a custom hook (`use_force_redraw_on_resize`) consumes it in `post_component_update` → `updater.clear_terminal_output()` forces a full-screen clear + redraw (bypassing the line diff).
  3. Poll terminal size (the hook's poll_change reads crossterm size) → size changes also trigger a full clear.
- **Verification**: real-API (DeepSeek) tmux multi-turn conversations — streaming bodies, tool rounds, and thinking blocks interleaved, small-window (16 lines) resize round-trips; no ghosting anywhere. Mock regression: all 171 tests green.

### D18. Theme configuration (the `theme` setting)

- **Config**: `settings.json` gains `"theme": "auto" | "dark" | "light"` (default auto).
- **auto detection**: before fullscreen, briefly enter raw mode and send OSC 11 to query the terminal's real background color (`ESC ] 11 ; ? ESC \`), then judge light/dark by BT.709 relative luminance; next, the `$COLORFGBG` seed; if neither, fall back to dark. Pitfall: OSC replies carry no newline and canonical-mode line buffering swallows them — must read in raw mode.
- **Tokens**: two semantic token sets for dark/light (light body text black `rgb(0,0,0)`, `userMessageBackground` 240, primary accent orange identical in both themes). When truecolor is unsupported, RGB degrades to 256 colors (AnsiValue cube approximation).
- **Welcome title**: `Welcome back` uses the primary accent orange, not white.

### D19. Streaming ghosting root fix: event-level forced full clear (diff-path residue)

- **Symptom (user's real terminal Ghostty; not reproducible in tmux)**: as streaming text grows, "half-covered" rows appear — new content covers part of an old line while old characters linger at the end; a TurnEnd full clear restores it.
- **Investigation**: trace confirms the FORCE full-clear path itself is correct (after every doc line-count change the hook consumes it and rewrites everything, 0 mismatches); not reproducible in tmux/pty/simulated terminals → the problem is the **diff path**: when content grows within a line (line count unchanged), iocraft's line diff runs and stale lines remain on real terminals. DeepWiki consultation: `write_ansi_row_without_newline` should in theory always clear the line tail, but row_eq's trimmed comparison can falsely judge equality with background colors/fill (issue #142 family).
- **Fix**: **any event handling (`drain_all` returns true, covering TextDelta/ThinkingDelta/ToolStart etc.) immediately sets `FORCE_FULL_REDRAW`** → every frame with content changes goes through a full clear + redraw, bypassing the line diff. Atomic within one frame under synchronized update (2026), so no flicker; DeepWiki confirms this pattern is idiomatic iocraft (same as inside `use_output`).

### D20. Default interaction switches to REPL inline mode (superseded by D36)

- **Background**: the previous default fullscreen iocraft canvas (alt screen + in-app viewport scrolling + input pinned to the bottom) was perceived by users as a "big app shell that forces repainting"; the reference implementation's default (in the user's environment) is non-fullscreen: settled content lands in scrollback like ordinary terminal output, the wheel scrolls history, and the prompt sits at the end of the conversation rather than pinned to the bottom.
- **Mechanism**: non-fullscreen = print-and-forget (settled messages render once to stdout, never rewritten) + a dynamic tail (streaming/spinner/input lines) redrawn in place (relative to the cursor line diff); lines scrolled out of the viewport freeze, avoiding full redraws.
- **Selection**: first, a hand-written REPL driver (ANSI serialization + rollback/flush bookkeeping + self-managed crossterm event loop, ~500 lines, kept recoverable in `git stash`) to validate the mechanism; then confirmed **iocraft 0.8.4's native `use_output` is the equivalent** (`hooks/use_output.rs:74` exec: `clear_terminal_output` → write to stdout → render loop repaints) → dropped the hand-written driver and reused at the component layer (markdown/theme/activity layout/`row_element` + `to_string()` offscreen rendering).
- **Implementation**: the `Bingo` component gains an `inline` mode — as `doc.settled` (the settlement boundary; the mechanism predates D19) advances, flush each row via `println(row_element(row).to_string())` (multi-line blocks must be flushed line by line: with raw mode's OPOST off, an in-block `\n` doesn't return to column zero, causing staircase misalignment — demonstrated by the rsink demo); the Transcript renders the tail slice `rows[tail_start..]` (flush boundary = max(printed, settled, len−max_live); canvas height always ≤ the terminal; inline erasing never pollutes scrollback); key gate: idle Esc ignored / ctrl+o only passes unsettled messages / idle Ctrl+C → `system.exit()` (requires `ignore_ctrl_c()` so the event reaches the component; while busy, Ctrl+C cancels via on_key); `--fullscreen` keeps the original canvas path.
- **Incidental fix**: `detect_system_theme` used `tokio::fs` to read `/dev/tty` — after a timeout the blocking thread's read() is uncancellable, swallowing the next first input and hanging tokio's shutdown join (the process can't exit); switched to `std::fs` + `O_NONBLOCK` + polling (libc dependency, already locked).
- **Verification**: PTY smoke tests (python pty + winsize 100x24): welcome card flushed, no alt screen, input reflected key by key, thinking spinner redrawn in the dynamic area with rollback, Ctrl+C exit stable 6/6; all 178 tests pass.

### D20b. Two inline-mode fixes (startup clear + input pinned to bottom)

- **Startup clear**: `ForceRedrawOnResize`'s first detection (treating `last=None` as a size change) + a first-frame `FORCE_FULL_REDRAW` (doc line count 0→N) → the first frame calls `clear_terminal_output()` to clear leftover shell output. Fix: the first poll only records the baseline size without counting as a change; `post_component_update` skips the clear on the first frame (the `first` flag). Fullscreen mode is unaffected (entering the alt screen is already blank).
- **Input pinned to bottom**: the inline root View had a fixed `height: height` (fullscreen design) → the canvas always fills the terminal and the input line is pinned to the last screen row. Fix: the inline root View has no fixed height (natural content height) and the Transcript gets `flex_grow: 0` — canvas height = tail + chrome's actual row count (3 rows when idle), and the input line flows with the content, consistent with non-fullscreen.
- Verification (PTY + winsize 100x24): no Clear-All/clear sequence at startup; after input, the redraw rolls back by 2 (canvas height 3, previously 23); Ctrl+C exits in ~0.3s.

### D20c. Double newline on flush fixed (println + trailing \n from to_string)

- **Symptom**: one conversation, two formats — the dynamic area is compact (`Mulling for 1.4s …` lines with no blank lines between), but after flushing, extra blank lines appear between rows (looser line spacing in scrollback).
- **Root cause**: the ANSI string from `ElementExt::to_string()` always ends with `\n` (iocraft canvas Display; tests assert `"hello!\n"`); `StdoutHandle::println` then adds `\r\n` → every flushed row becomes `line\n\r\n`, rendered on screen as "line + blank line".
- **Fix**: `trim_end_matches(['\n', '\r'])` before flushing via println. PTY verification: flush sequence `❯ hello` → margin blank line → next message; no interleaved blank lines.
- **Side investigation (not fixed; pre-existing behavior)**: Ctrl+C cancel while busy — the `client.stream()` connect/stream-setup phase isn't inside `select!` (query.rs:234 awaits before the select), so cancellation can't interrupt a hanging connection (e.g. a local discard port); irrelevant under real API streaming.

### D20d. Flushed color loss fixed (to_string without ANSI)

- **Symptom**: after the D20c double-newline fix, everything flushed to scrollback is colorless (the dynamic area is fine).
- **Root cause**: `Canvas::write()` ("as unstyled text, without ANSI escape codes" — the unstyled path) is the `Display` implementation; `ElementExt::to_string()` = `render(None).to_string()` goes through Display → **every flushed row is plain text**. `write_ansi()` (pub) is the color-carrying output path.
- **Fix**: flushing becomes `row_element(...).render(None).write_ansi(&mut buf)` + trim the trailing `\r\n` before println. PTY verification: flushed bytes contain truecolor/256-color codes.

### D20e. Resize viewport redraw (fullReset / OffscreenFreeze semantics)

- **Symptom**: styling breaks after a window resize — flushed rows keep their old-width wrapping, and the dynamic area misaligns after reflow.
- **Semantics verified** (key point raised by the user): **content inside the viewport is not Static** — it only freezes after scrolling out of the viewport; message re-rendering depends on resize; resize takes the fullReset path: clearTerminal + full rewrite at the new width (including the accumulated in-memory fullStaticOutput).
- **Implementation**: on inline-mode size changes (last_size detection, first frame excepted) → set `chat.dirty` (forcing the doc to rebuild at the new width — without this, dirty isn't set when no tick has run and replay would use the old-width doc) → set the replay flag → during flush: `\x1b[2J\x1b[H` (clear the visible area + home) + redraw the "flushed rows inside the viewport" at the new width (`rows[printed-N..printed]`, N ≈ screen height − dynamic area height; scrollback outside the viewport stays untouched); printed isn't double-counted.
- **Related**: `reply_cache` (markdown render cache) doesn't distinguish widths → clear it on width changes (tracked via `prev_build_width`), otherwise message text keeps old-width wrapping.
- **Pitfall**: the first version forced build_rows via `width_changed`, which stalled the mock render loop (frame scheduling broken); switched to "size change → set dirty" to take the normal rebuild path.
- Verification: all 178 tests pass; PTY resize 100→60: exactly one 2J, replayed rows wrap at 60 (welcome narrow-column text overflow and non-wrapping user messages are pre-existing layout behaviors).

### D20f. Panic on rapid resize (flush cursor out of bounds)

- **Symptom**: during rapid resizes, `thread 'main' panicked at components.rs: range end index 28 out of range for slice of length 26`.
- **Root cause**: width changes on resize → after `build_rows` reflows, `doc.rows` shrinks (narrower widths wrap less), but the flush cursor `printed` still holds the old value (> the new `doc.rows.len()`); the flush slice `rows[*p..live_start]` and the replay slice `rows[*p-replay_n..*p]` go out of bounds. `live_start = max(printed, ...)` also relies on the un-clamped printed.
- **Fix**: at the start of the inline branch (**before** computing `live_start`), clamp the flush cursor `*p = min(*p, doc.rows.len())` — all slices become safe again.
- Verification: 25 rapid width switches (40-120) × 5 rounds with messages sent in between; no panics; all 178 tests pass.

### D20g. Real input cursor (layout equivalent, since iocraft has no cursor API)

- **Requirement**: while typing, the terminal's real cursor should rest after the input text (reference experience) instead of a fake `▋` cursor "floating at the end".
- **Mechanism**: after the render loop outputs, move the real cursor to the position the component declares (the input's cursorOffset); it only freezes outside the viewport.
- **iocraft 0.8.4 has no such API** (after rendering, the cursor always rests at the end of the canvas's last line; TextInput also uses a fake cursor — an absolutely positioned color block). Equivalent implementation: **make the input line the canvas's last line** — after iocraft renders, the real cursor naturally rests at the end of the input text. Inline chrome order adjusted: tasks/warn/waiting/footer/top border/**input line (last)**; the `▋` fake cursor is removed from the input line. Fullscreen keeps the original layout (the `▋` fake cursor).
- **Side investigation (not fixed; environment-specific)**: in the pty test environment, keyboard events occasionally don't arrive — iocraft's `Terminal::new` synchronously calls `crossterm::supports_keyboard_enhancement()` (sends `\x1b[?u` etc., ~2.5s timeout), and in a non-responding pty the probe can swallow input during that window; the HEAD baseline (fullscreen) reproduces it too → not introduced by this change; real terminals respond and are unaffected (typing works normally).

### D20h. Input returns to a full box; the real-cursor approach is rejected

- **Symptom**: the D20g "input line last" layout (no bottom border, footer moved up) was rejected by the user — "the input box is wrong and I can't type".
- **Postmortem**: in the user's screenshot, text was visible after ❯ (typing actually worked) — "can't type" was really a cursor-perception problem: once D20g removed the `▋` fake cursor, iocraft's rendered real cursor rested on the canvas's last line (not following the input), so users couldn't see where they were typing. Real-cursor tracking (ink frame.cursor) is unimplementable in iocraft 0.8.4 (no component-declared cursor API).
- **Fix**: restore the full input box (top border + `❯ {input}▋` + bottom border + footer below); inline mode emits `\x1b[?25l` to hide the real terminal cursor (so the footer's real cursor doesn't get confused with `▋`); iocraft automatically Shows it on exit. Verification: top/bottom borders, `▋`, and the cursor-hide sequence are all present; all 178 tests pass.

### D20i. Total event blackout root cause: render storm from writing State during render (clamp writes every frame)

- **Symptom**: on real terminals (reproduced by the user and in tmux), typing/Ctrl+C/resize all stop working; in the pty environment, startup output balloons to 122KB (full redraw every frame).
- **Root cause**: D20f's flush-cursor clamp `*p = (*p).min(doc.rows.len())` runs **every frame** — iocraft's `State::write` (DerefMut) **unconditionally** marks `did_change` and calls `waker.wake()` (use_state.rs:149-163, even when the value is unchanged) — writing state **during render → wake → immediate re-render** → render storm → in the render loop's `select(root.wait(), term.wait())`, `root.wait()` is always ready → **`term.wait()` starves → terminal events (keyboard/resize) never arrive**. Ctrl+C exit and resize replay (which depend on Resize events triggering replay) die with it — the user's "resize stopped working" is the same root cause.
- **Fix**: the clamp writes only when the value changes (`if clamped != *p { *p = clamped; }`). Audited every render-time state write: flush/replay/cursor_hidden/last_size all have conditional guards; only the clamp was missing one.
- **Verification** (real tmux terminal; the probe responds, so it fully reproduces): typing `❯ zz▋` ✓; Ctrl+C exit (pgrep 0) ✓; resize 100→60 viewport replay (welcome reflowed at width 60) ✓. All 178 tests pass.

### D21. The `!` command (bash mode)

- **Input side**: pressing `!` with an empty input enters shell mode (the `!` itself isn't inserted); in bash mode, backspace on an empty input exits; with non-empty input, `!` inserts normally. The mode is **sticky** (kept after submission). UI: input prefix `!` + the input box border switches to the `bashBorder` color; the footer shows `! for shell mode`.
- **Execution side**: **bypasses the model and the UserPromptSubmit hooks** — the command runs through the unified permission gate (PreToolUse hook + canUseTool + user confirmation) + the Bash tool + the PostToolUse hook; the UI reuses the existing tool activity rows (ToolUseStart/ToolReady/ToolDone). History is written in the shape of a real tool round (`<bash-input>` user text → synthesized assistant ToolUse → user ToolResult — **the API requires tool_result to pair with a tool_use in the same request**), and the output is HTML-escaped (`& < >`) and wrapped in `<bash-stdout>`.
- **Model response** (`respondToBashCommands`, same-named settings key, default true): true → after execution, enter queryLoop as usual (the model sees the output and may continue); false → execute only, and inject a caveat before the history (`<local-command-caveat>` "DO NOT respond to these messages…") so the model doesn't treat the output as instructions. Interruptions/PostToolUse blocks also take the "don't consult the model + caveat" path.
- **Implementation points**: `run_query` splits out `query_loop` (loop body) + `tool_context`, shared by `run_bash_command` and `run_query`; the TUI `Chat` gains a `bash_mode` state and `start_bash_turn`.
- **Trade-off**: stdout/stderr are no longer separated (dual tags) — bingo's Bash tool already merges output and includes the `$ cmd` echo and exit code, so the model loses no information; periodic/background commands (`!watch …`) directly reuse the tool's backgrounding semantics. Permission denials are also fed back to the model (consistent with run_query's `<permission_error>` convention; failures also consult the model).
- **Verification**: all 184 tests pass (new: `!` toggling, bash submission finalization, execution message shape and escaping, component rendering of prefix/border/hint, settings merging); PTY smoke: `!` prefix + `! for shell mode` hint + `!echo hello` executes directly (`✓ Bash $ echo hello · 9ms … +3 lines`) and stays in bash mode afterward.

### D21b. Interactive/TTY command rejection (shared by `!` and the Bash tool)

- **Motivation**: bingo's child processes have piped stdin/stdout but **inherit the controlling terminal** — fullscreen TUIs (top/htop/vim) print garbage, ssh/fzf/sudo reaching straight for `/dev/tty` seize the terminal (the screen gets torn apart in raw mode), and bare shells/REPLs exit immediately with no input (pointless). The reference implementation only warns ("Interactive terminal apps can't be driven by an agent's bash tool" + the tmux wrapper convention); bingo rejects before executing.
- **Implementation**: `tool/bash.rs::interactive_command_reason` (shared by `!` and the Bash tool) — after unwrapping sudo/env/nohup/command/exec/doas wrappers, decide by command name and arguments:
  - **Always reject**: system monitors (top/htop/btop…; `-b/--batch` snapshot mode passes), editors (vim/nano/emacs…), file managers (ranger/yazi/mc…), TUI tools (lazygit/tig/fzf/k9s/screen…), `docker/kubectl attach` and `exec/run -it`, foreground `tmux` (scripted uses like `new -d`/send-keys/capture-pane pass), gdb (`-batch` passes).
  - **Bare rejection**: shells/REPLs (bash/python/node…; with arguments they pass); DB clients (sqlite3/psql/mysql/mongosh/redis-cli — no `-c/-e/--eval` execution flags, no `<` redirect, and no SQL/script positional args means an interactive prompt; non-interactive uses like `--version`/`-l` pass).
  - **ssh**: `-t` forcing a tty, or "host only with no remote command" (password prompts/remote shells occupy `/dev/tty`) → reject; `ssh host 'cmd'` and `-N/-f` (port forwarding/background) pass. `sudo -i/-s` and bare sudo → reject.
- **Landing points**: at the top of `BashTool::call` (takes effect on the model path, fed back as `<tool_use_error>`); `run_bash_command` pre-checks **before the permission gate** (`!top` doesn't pop a pointless permission prompt; when respond is on, the model sees the rejection reason and can suggest alternatives like `top -b -n 1`) and emits a `Warning` event directly — collapsed rows can't be expanded after being flushed in inline mode, so the rejection reason must surface as a warning line. The tool description also states that interactive commands are rejected.
- **Verification**: all 187 tests pass (new: ~60 positive/negative reject/pass cases + rejection assertions on both the tool layer and the `!` path); PTY smoke: `!top` → `⚠ interactive command not allowed: top is a fullscreen interactive monitor (requires a TTY); rejected. One-off snapshots work with \`top -b -n 1\``, the command doesn't run, and it returns to the bash-mode hint.

### D21c. `!` command output preview (BashModeProgress)

- **Symptom**: after `!pwd`/`!ls` runs, the output is swallowed by the collapsed group — the collapse summary line ("Ran 1 bash command") contains no output, and once flushed in inline mode it can't be expanded, so the user can't see the command result.
- **Mechanism**: bash-mode progress = `<bash-input>` line + fullOutput via the same renderer as normal tool results — the output is shown directly under the command (long output collapses as "+N lines").
- **Implementation**: `UiHooks.on_tool_ready` and `UiEvent::ToolReady` gain `standalone: bool` — Bash activities from `!` commands are marked standalone: they only set a summary and **don't join collapse groups** (the model-driven path passes false; collapse behavior unchanged); on ToolDone, non-collapsed Bash activities are **expanded by default**, with content = output minus the `$ cmd` echo and the `[Exited with code N]` footer (`bash_output_preview`), reusing layout_activity's existing `⎿` connector-line rendering.
- **Verification**: all 189 tests pass (new: standalone collapse-decision positive/negative cases, preview expansion and stripping assertions); PTY smoke: `!pwd` → `⏺ ✓ Bash $ pwd · 8ms` + `⎿ /Users/yexrob/Episodes/Projects/bingo`; `!ls` → `⎿ AGENTS.md` + indented continuation lines, multi-line output fully visible.

### D22. AskUserQuestion tool (asking the user multiple-choice questions)

- **Contract**: `questions[1..=4]`, each `{question, header? (≤12 chars), options[2..=4] {label, description?}, multiSelect?}`; question texts and option labels are each unique. v1 implementation: `multiSelect: true` errors with guidance to use single-select; the `preview` field is not included in the schema (no preview panel in the UI); when header is missing, the title is "Question N".
- **Execution**: `src/tool/ask.rs` — `is_concurrency_safe=false` (blocks waiting for the answer; serial); each question calls `ToolContext.ask_question` (None = Esc skips, later questions are not asked); the result is `The user answered: "q"="a", ...` or `The user did not answer the questions.`; model-side input validation failures (count/uniqueness) are fed back as `tool_use_error`.
- **UI reuse**: AskUserQuestion goes through the existing permission modal (`PermissionRequest` + 1-9 keys + Esc) — `UiHooks.ask_question` shares the channel with `ask`, with `Confirm(i) → Some(i)` and `Cancel → None`. **Does not go through the permission gate** (the dialog itself is the approval; the run_query gate short-circuits by name). The tool row summary shows the question text. Subagents (Agent tool) always get None from ask_question (no UI to ask).
- **Incidental fix**: `schema_for` previously discarded the schemars generator's definitions — the schema sent to the model had dangling refs for nested types (`Vec<AskQuestion>` referenced via `$ref`). Now `generator.definitions()` are merged into the root schema (no effect on flat tools).
- **Trade-offs**: multi-select / Other free text / the preview panel are future items (multi-select components and text input need the modal protocol extended); no timeout (default never, wait indefinitely).
- **Verification**: all 196 tests pass (new: schema shape includes definitions, input validation negative cases, answer/skip/multi-select rejection, serialized execution queue, TUI Confirm/Cancel hook mapping).

### D24. MCP management: McpManager connection cache + /mcp command

- **Mechanism**: connect to all servers in parallel at startup (batch; failures don't block); connection states connected/failed/needs-auth/pending/disabled enter AppState; SSE disconnects retry with exponential backoff (5 attempts, 1s→30s); the `mcp__{server}__{tool}` prefix enters ToolRegistry, refreshed dynamically via ToolListChangedNotification; enable/disable persists the `disabledMcpServers`/`enabledMcpServers` lists (project config); `/mcp` is an immediate command = interactive UI (status badges + fork/reconnect/logs/delete menu) + `/mcp enable|disable [name|all]` + `/mcp reconnect <name>` fast path.
- **bingo's existing code reworked**: the original `connect_servers` spawned a child process per turn to reconnect (wasteful). Added **`McpManager`** (attached to `Session.runtime.mcp`): **lazy connection** (`connect_all` on the first turn, cache reused afterward), failed connections are recorded without auto-retry (`/mcp reconnect` is manual — a stdio child process exiting means total failure, so auto-reconnect is pointless), `disconnect`/`set_enabled` take effect immediately.
- **/mcp command** (argumentHint `[enable|disable [server-name]]`): no args lists servers (✓ connected · N tools / ✗ failed: details / ○ disabled / · not connected); `enable|disable [name|all]` updates the list and **persists the top-level `disabledMcpServers` in `.bingo/settings.json`** (same-named mechanism); `reconnect <name>` (when disabled, intercepts with a prompt to enable first).
- **Config contract**: `McpServerConfig` gains a `type` field (TransportSchema); **stdio** (`command`/`args`/`env`) and **http** (`url` + optional `headers`, streamable HTTP) land; sse/ws connections error with a hint (rmcp 3.1 has no legacy SSE; OAuth not done, static headers cover for now). `command` becomes optional (http has no command).
- **Permissions**: MCP tools reuse the unified permission gate (Box<dyn Tool> already provides it); is_concurrency_safe=false (serial, conservative policy).
- **Verification**: all 244 tests pass (new: McpManager state matrix, no retry on failure, reconnect clears failure + /mcp listing, enable-disable persistence, reconnect interception); tmux test (dependency-free Node stdio server): lazy connect 2 tools, badsrv failed + warning line, disable disconnects + persists across sessions, disabled reconnect interception, next turn auto-connects after enable.

### D25. Running status line (ActivityIndicator)

- **Full mechanism** (letting the user know the agent is running):
  1. **Status line (ActivityIndicator)**: one line at the bottom of the transcript, above the input box — spinner (100ms frame) + verb message (`{verb}…`) + thinking timer (`(thinking for 12s)`) + tool timer (`running tool for 3.2s`) + output token count (`↓ N tokens`) + total elapsed. Verb = running tool's activeForm/subject > thinking quip > fallback "Working". **This line is always present, whether the model is thinking, waiting, or running a tool.**
  2. **Thinking placeholder**: `⠋ Mulling for 1.4s` (~150-quip table).
  3. **Tool line**: output preview — `Running…` + elapsed when there's no output; last 5 lines + `~N lines`/`+N lines` + `(timeout 2m)` when there is.
  4. **Tool heartbeat**: 30s without output sends a progress heartbeat; long tasks keep refreshing elapsed.
  5. **Stall detection**: thresholds at 10s/45s/300s since the last token — the spinner drops intensity/switches to warning colors; on 429, shows `Waiting for API response · will retry in X · check your network`.
  6. **Spinner tips**: over 30s running hints `/btw`, over 30min hints `/clear`; when tasks exist, `Next: {subject}`.
- **bingo implementation**: `Chat::running_status()` (when busy, returns `(verb, elapsed)` — running tool summary > thinking quip > "Working"; the `turn_started` Instant is set by TurnStart/TurnEnd) + `status_row` rendered above the input box (one chrome line, visible in both inline and fullscreen; between the task area and the warning line). The verb prefers the tool summary (`$ sleep 2`), consistent with activeForm semantics.
- **Verification**: all 198 tests pass (new: `running_status` verb priority, status-line rendering assertions); PTY test with `!sleep 2`: `⠼ $ sleep 2 for 0.2s → … → 2.0s` ticking frame by frame, disappearing at turn end.
- **Leftover (an upstream iocraft issue, not introduced by this change)**: when the API hangs completely (no events at all), the tick-driven render chain starves within ~1s — the spinner/timer freeze at the moment of submission (the baseline reproduces it; probe timing can reproduce/work around it). No problem when the event stream is healthy (including real-API streaming round-trips). The status line at least shows a visible "Working" before freezing; a full fix needs changes in the iocraft render loop's wake chain (`select(root.wait(), term.wait())` has a wake race for self-driven animations). (Obsolete since the D26 rewrite.)

### D26. TUI rendering layer rewrite: iocraft → ratatui 0.30 + a hand-rolled inline driver

- **Rationale**: iocraft's inline mode redraws the whole canvas every frame (including content that never changes), pads every line with spaces to the full terminal width, its cursor-relative diff desyncs under terminal resize reflow, and a canvas ≥ terminal height triggers `Clear(All)+Purge`, wiping scrollback. The multi-round compensations (chrome bookkeeping, shrink_deficit, reflow whitelist) treated symptoms, not causes — the root of the resize staircase residue is the premise of "redrawing settled content" itself.
- **New architecture** (same as codex-rs / Claude Code): settled lines are written **once** into the terminal scrollback via the scrolling region, never redrawn; only the bottom viewport (unsettled tail + chrome) is redrawn. On resize, the terminal naturally rewraps the old content (like ordinary shell output), so residue structurally can't accumulate.
- **Layering**: `src/ui.rs` holds render-independent contracts (UiEvent/AskRequest/PermissionRequest/DialogAction/tui_hooks, zero rendering dependencies — a future GUI implements another `run_*_session` against the same contract); `tui/term.rs` is the only module in the whole crate that touches the terminal (viewport double-buffer diff + insert_history + CSI ?2026 synchronized-update wrapping, mirroring ratatui's `insert_before` scrolling-regions path and codex's `custom_terminal`); `tui/app.rs` has an explicit `select!` event loop + Frame assembly (frame height = measured row count; no second chrome formula to drift); `tui/view.rs` is pure conversion (crate `Line` → ratatui text).
- **Migration surface**: chat.rs's 4,100 lines of logic + 3,300 lines of tests survive on import changes alone (iocraft's KeyCode/KeyModifiers are already crossterm re-exports); line/theme switch the Color type; components.rs (2,122 lines) is deleted entirely; the iocraft dependency is removed, adding only ratatui (the `scrolling-regions` feature must be explicitly enabled, not default in 0.30.2; crossterm gains `event-stream`).
- **Driver semantics highlights**: while the viewport isn't bottom-pinned, `scroll_region_down` pushes it down (doesn't consume scrollback); once pinned, `scroll_region_up` moves the area above into scrollback in chunks; line tails use `Clear(UntilNewLine)` rather than padding with spaces (key to no wrap garbage on resize); emptiness is judged by full `Cell::EMPTY` equality — a space with a background color is content (the user bubble tail survives thanks to this); `HistoryItem::Raw` (kitty image bytes) is accounted per row and its lines are never cleared; viewport growth writes a real newline on the physical bottom row (the only scroll that preserves scrollback on all terminals).
- **Incidental improvements**: real bracketed-paste events (the burst heuristic drops to a fallback); real strikethrough rendering (CROSSED_OUT); the terminal's hard cursor lands on the `▋` position (D20g's intent; the iocraft no-cursor-API constraint is gone); extremely short terminals drop lines from the top to preserve the input box + footer (the old behavior triggered Purge); D25's leftover tick starvation dies with the render loop (tokio interval has no wake race).
- **Verification**: 468 tests all green (chat.rs's 115 existing tests pass with zero changes; 23 TestBackend scenario tests for the driver, including a "50-line history + narrowing resize leaves no duplicate lines" regression; 25 strategy tests ported for app/view — tail window, chrome integrity, flush not double-counting across widths, ctrl+o gating, suggestion-line counts from one source); `cargo clippy -- -D warnings` clean. Real-machine smoke tests (Ghostty long reply + drag resize, tmux+Ghostty images, Terminal.app, EL line-clearing on BCE terminals) still to be verified.
- **First round of real-terminal follow-up fixes** (2026-08-06, four items from Ghostty testing):
  1. Large blank area after a turn — the settle order was wrong: settled rows were scrolled into scrollback first (viewport still tall), then the viewport shrank, and the freed rows became a permanent blank band (height = the previous reply's row count). Changed to a driver `gap_above` bank + single-batch `frame()`: shrink first (freed rows are booked into the bank), settled rows are written straight into those rows, then the viewport is diffed — settle has zero blank area and normally zero scrolling (test asserts `scrolled_up` is empty); the unconsumed bank is reclaimed by the next growth (grow reclaims the bank before scrolling).
  2. MCP stderr punching through the TUI — now that scrollback is never redrawn, a stdio child process's logs are fixed on screen once they land (the old architecture masked this with full redraws every frame). On spawn, stderr is redirected to `~/.local/share/bingo/logs/mcp-<name>.log` (truncated and rewritten per connection; if the file can't be opened, output is dropped — never inherit the terminal).
  3. The real reason tmux images never displayed — in raw mode, every `placeholder_rows` line ends with a bare `\n`: LF only moves down without returning to column zero, so from the second line the placeholder starts at the previous line's end column, shearing the whole grid diagonally; kitty then places pixels per placeholder cell, so everything scrambles (same disease in the iocraft era — hence "never displayed"). The `kitty_image_bytes` tail advance switches to `\r\n` too.
  4. Drag-resize border ghosting — chrome's full-width lines (borders/bubbles) get reflowed/wrapped by the terminal and escape the viewport's clear area. Added a 120ms debounce: during a storm only the latest size is recorded and nothing is drawn (drawing at the old width only stacks more wrong-width lines); after silence, apply + redraw once — residue drops from one group per step to at most one group per drag (Ctrl+L can clear). The thorough fix is D27.
  Verification: 473 tests all green (driver +4: settle with zero scrolling, banked chunked scrolling, grow reclaim, resize/clear resets the bank), clippy clean.

### D27. Lazy flush + resize rehydration: the large live viewport

- **Requirement** (explicitly requested by the user): content inside the viewport must not be frozen into the terminal — everything in the window stays reflowable; when capacity grows (taller window, smaller font), even **already-flushed** content should be pulled back and re-rendered to fill the screen; "I can accept duplicates when scrolling up in that case".
- **Strategy**:
  1. **Lazy flush**: a settled segment stays in the live document as long as it still fits entirely in the visible window (terminal height − 1 − chrome), participating in diff redraws every frame (width changes reflow via `build_rows`; ctrl+o collapse still works). A segment only freezes when its start row crosses the window top, and **the whole segment freezes at once** — half-frozen would leave the hidden part neither on screen nor in scrollback, with nowhere to look; when a segment freezes wholesale, its visible tail moves from viewport drawing to scrollback, pixel-for-pixel unchanged (absorbed by the gap bank, see D26 follow-up fix 1).
  2. **Rehydrate**: after the resize debounce goes quiet, if the window capacity exceeds the live document's row count, walk back from the most recently flushed segment by checkpoints (question-answer blocks first, then message segments), trial-render via `build_rows`, and back off if the budget overflows — guarantees no settled segment sits above the window top after rehydration, so it can't fight lazy flush. Rehydration is **pure bookkeeping** (the flushed cursor rewinds; nothing is written to the terminal); old copies in scrollback physically can't be recovered — scrolling up shows a duplicate of the old geometry, a trade-off the user explicitly accepted.
- **Mechanism**: `Doc.settled_marks: Vec<SettledMark{row_end, segments, ask_rows}>` — build_rows records a checkpoint at every settlement point (welcome card / each settled message / question-answer block) (values accumulated within a build; `Chat::mark_base` digests the increments across advancing builds); the app layer's `pick_flush_mark` selects the farthest checkpoint whose segment's start row is < the window top, and `advance_flushed_upto` advances partially. The old aggregate fields `settled_segments`/`settled_ask_rows` are deleted (pure duplication of the checkpoints).
- **Side-effect analysis**: after a resize, the first frame's greatly taller viewport pushes the screen's old geometry residue into scrollback (append_lines), and the newly rendered copy then fills the screen — exactly the "accept duplicates" semantics; re-flushing a rehydrated image block re-sends the kitty transfer — U=1 updates the existing placement by id, Direct accumulates new instances without ids but old placement pixels stay put, both harmless.
- **Verification**: 476 tests all green (new: pick_flush_mark strategy matrix, no freeze when it fits, rehydration fills/backs off on budget overflow; 9 existing assertions updated to read checkpoints); clippy -D warnings clean.
- **Real-terminal follow-up fix #2 (/resume big list triggers per-line duplicates + concatenated lines)**: three compounding causes, fixed at the root —
  1. **Illegal single-line DECSTBM region** (the culprit): after lazy flush the viewport height is H−1 and `vp.top==1` is the norm; make_room emits `CSI 1;1r`+`CSI S` per line — DECSTBM requires a region of ≥2 lines, so the illegal parameter is ignored and the region falls back to the **full screen**: every written line scrolls the full screen, interleaving viewport content and writes as they move up → adjacent per-line duplicates + concatenated tails of new lines covering old ones (TestBackend is semantically correct for single-line regions, hence all 27 driver tests green while real hardware fails). Fix: the viewport cap uniformly becomes **H−2** (driver clamp / Frame budget / tail_window, three same-source places), so `vp.top ≥ 2` always holds and the region is always legal.
  2. **`CSI S` doesn't enter scrollback** (explicit kitty/Ghostty semantics: SU-scrolled lines go into a bit bucket): flush scrolling switches to the codex-style primitive — `RawWrite::scroll_into_scrollback`: DECSTBM top-anchored region + cursor parked at the region's bottom row + n LFs + reset (LF is the only scroll that enters scrollback on all terminals); the test-side Recorder maps it back to `scroll_region_up` to keep TestBackend semantically equivalent — all 27 driver tests untouched.
  3. **Transient slash output evicting live content** (policy error): /resume's no-arg list is a TTL transient line at the document tail that squeezes the window and wrongly freezes live conversation (and 2s later the window is half empty). `Doc.transient_rows` marks the transient row count; the lazy-flush window computation excludes them — temporary lists merely cover, they don't evict.
  Verification: 477 tests all green (new: transient-no-freeze regression test; 3 H−1 expectations change with the cap to H−2), clippy clean.
- **Real-terminal follow-up fix #3 (after resize the viewport jumps to the bottom, piling old frames into a multi-width residue stack)**: `term.resize()` unconditionally re-anchored the viewport to the screen bottom — when content didn't fill the screen (e.g. only the welcome card at the top), every width drag flung the viewport to the bottom and redrew, leaving the old-position frame above forever uncleared: one drag piles up a stack of cards at different widths. Changed to **content anchoring** (CC's behavior, named by the user): resize keeps the viewport on its original rows and only moves it up enough to fit when the new screen can't hold it; clearing goes from the viewport's original position to the screen end, wiping the terminal-reflow fragments pushed out of the old viewport. The viewport renders from the content start, consistent with a freshly started session. 477 tests green (`resize_taller_keeps_the_viewport_content_anchored` replaces the old jump-to-bottom assertion).
- **Real-terminal follow-up fix #4 (one row of wrap fragments still remains above the viewport origin after narrowing)**: when the width shrinks, terminal reflow happens before the resize event; old full-width lines wrap and push content down as a whole, and if it hits the screen bottom the whole screen scrolls up — the shift between our recorded viewport origin and physical reality is **protocol-unknowable**, so "clear downward from the origin" inevitably misses fragments pushed above the origin. The ultimate strategy is to stop guessing geometry: after the resize debounce, go through the Ctrl+L channel (`force_redraw` → `clear_visible`) to clear the whole visible screen and redraw the window from the top at the new width; rehydration has already pulled content back to fill the screen, so the picture is lossless and old-geometry copies stay only in scrollback (a trade-off the user accepts). From here on, on-screen behavior after resize is fully deterministic, independent of any terminal reflow behavior.
- **Real-terminal follow-up fix #5 (full-screen misalignment ghosting after Ctrl+C + MCP stderr still punching through)**: two independent causes —
  1. **Reclaim growth clears the diff buffer** (the ghosting culprit): at turn end the status line disappears → the viewport shrinks by 1 (bottom-anchored, origin moves down, top line cleared into the bank); the Ctrl+C notice line appears → growth takes the reclaim path, which cleared both buffers to "force a full redraw" — but physically only the banked rows are blank; the rest is still the old frame. With prev lying about being fully empty, the diff believes the screen is empty: the new frame's **empty lines don't clear old text underneath** (`❯ Hi`/reply adjacent duplicates, card bottom-border fragments surviving) and **shorter lines don't clear their tails** (input-box border `────` residue to the right of the notice text). Fix: reclaim no longer clears the buffers; `retarget_buffers` gains a signed offset (shrink +shift bottom-anchored, downward expansion/scrolled growth 0 top-anchored, reclaim growth −reclaimed bottom-anchored with blank rows added on top — exactly mirroring the physically blank banked rows), so **prev truthfully mirrors the physical screen at all times** becomes a driver invariant (resize/clear_visible clear the buffers only because they physically clear the screen; the law holds). The regression test `grow_after_shrink_repaints_over_every_stale_row` precisely reproduces the screenshot artifact under the old implementation (`["eeee","bbbb","ffff","ggdd"]`). The trigger surface is broader than Ctrl+C: any "shrink → grow" sequence (send a message / open help / surface a suggestion after every turn end) hits it.
  2. **The rmcp builder overrides stderr** (the redirect from follow-up fix #1 never took effect): rmcp 3.1.0's `TokioChildProcess::new` internally goes through a builder whose default is `stderr: Stdio::inherit()`, and its `spawn()` unconditionally calls `.stderr(self.stderr)`, **overriding** the value already set on the Command — our log-file sink set on the Command is silently discarded. Fix: go explicitly through `TokioChildProcess::builder(command).stderr(stderr_sink(name)).spawn()`. The timing matches "stably appears after sending a message": MCP lazy-connects when assemble_tools spawns on the first message, so the banner lands exactly at the cursor (input box).
  Verification: 478 tests all green, clippy clean.
- **inline ctrl+o = expand/collapse toggle; expansion replays the entire transcript** (CC's non-fullscreen ctrl+o, named by the user): write-once scrollback means "expand in place" doesn't exist inline — the old scheme only allowed toggling collapse on the last unsettled message (the `last_message_dynamic` gate) and left flushed content alone. Switched to CC semantics, both directions:
  - **Expand** (collapsible items or flushed content exist): `Chat::expand_transcript` expands **all** collapsible items in the full history (activities + collapse groups), `reset_flushed` rewinds the flush cursor, sets `dump_transcript` + `force_redraw`; the app layer **clears the visible screen first** during replay (same as resize — without the clear, the relative position of the old frame and the replayed lines depends on viewport history, and short content would duplicate on the same screen), then rebuilds the whole document from the welcome card, takes the **last** settlement checkpoint, and freezes the whole volume into scrollback with one `flush_items` + `advance_flushed_upto`: replayed content lays out from the top of the screen with chrome right below, anything beyond the screen rolls naturally into scrollback, and the user scrolls up to see it all. The dynamic tail (streaming messages/permission dialogs/transient slash lines, all outside checkpoints) stays in the viewport as usual. No-op when the whole picture is on screen with nothing expandable.
  - **Collapse** (`transcript_fully_expanded`: collapsible items exist and all are expanded): `Chat::collapse_transcript` folds the full history back into aggregates, then takes the **same shrink path as resize** — undo the unrendered replay, `force_redraw` clears the visible screen, `rehydrate` refills the window at the collapsed height. The expanded replay lines on screen stay only in scrollback (write-once; without a clear they'd coexist on screen with the collapsed window). The predicate is always false when nothing is collapsible, so ctrl+o degrades to pure replay.
  Collapsed old copies stay above in scrollback — duplicates accepted (same trade-off as rehydration). Zero new driver primitives: a combination of rewind (same as /clear, /resume) + full freeze (same as lazy flush) + clear-and-rehydrate (same as resize). The `last_message_dynamic` gate is deleted; fullscreen ctrl+o remains an in-place collapse toggle (it can redraw there). 478 tests all green (2 gate tests rewritten as replay/collapse semantics).

### D28. Activity-row icon vocabulary: shape encodes category, color encodes state

- **Problem** (the user said "too ugly"): every activity row shows the same `⏺`, MCP tools expose their full raw name `mcp__server__tool(...)`, and Skills fall back to k=v `args="doc.md"` — categories indistinguishable, noisy.
- **Vocabulary**: `⏺` built-in tools (the CC anchor stays; group rows/reply dots/Update are the same family) · `◆` MCP (external components; display name `server:tool`, permission rules still use the full `mcp__` name) · `✦` Skill (same family as the ✢✻✽ starburst spinners; summary becomes `skill name args`) · `◉` subagent Watch line (a core within a ring = session within session; Agent is a hidden tool whose only visible line is the watch). Colors still encode state only (dim running/green success/red failure) — one role, one color.
- **Implementation**: `activities.rs tool_glyph`/`display_tool_name` + the `watch_header` label-prefix decision; `summarize_input` gains a Skill arm. All display-layer, zero behavior change. All four glyphs are unicode_width single-width (◆/◉ are EA=Ambiguous, same class as the existing ○/◇, already accepted). 480 tests green. (The label-prefix decision moves to a WatchKind contract field with D29.)

### D29. Named agents + instance continuation (first cut: hub-and-spoke)

Three rounds of design discussion (the user deleted concepts round by round) converged on the multi-agent roadmap: this entry is step one (non-experimental); step two is channel messaging (experimental flag, design frozen below).

- **Named definitions** (`src/agents.rs`): `~/.config/bingo/agents/*.md` + `.bingo/agents/*.md` flat files (the project layer near cwd overrides the user layer with the same name); frontmatter `name/description/model/provider/thinking`, body = the subagent's system prompt (replaces the parent system; empty inherits); precedence: explicit args > definition > inheritance (model/provider/thinking independent of each other; thinking inherits a snapshot of the parent session's current level). **Cross-provider boundary (#19 fix)**: when forking to an endpoint different from the parent session's current provider, neither the parent model nor the thinking level is inherited — `model` must be explicit (missing → early failure "provider X requires a model", avoiding the parent model name hitting the wrong endpoint as "model not found"), `thinking` defaults to off (no parameter sent; compatible with DeepSeek/Ollama endpoints); `provider` "default"/unspecified = shared parent endpoint (follows parent switches); same provider (including unspecified) keeps inheriting the parent session's current-level snapshot. Explicit/definition thinking values are validated (off + THINKING_LEVELS; invalid → error, not silent downgrade). Frontmatter parsing is generalized into `skills::parse_frontmatter_pairs` (arbitrary keys + folded/literal scalars), shared by skills and agent definitions. Definitions number in single digits; no mtime caching.
- **Instance registry** (`AgentRegistry`, shared at Session level): every Agent spawn registers a named instance (`name` arg defaults to the definition name/agent; name collisions auto-suffix -2/-3), state machine Running/Idle/Stopped. On turn completion, **the full message history returned by run_query** is stored in the entry — continuation = old history + new instructions back into run_query, zero context loss.
- **Continuation and lifecycle** (only assembled at depth==0; hub-and-spoke): `SendMessage(agent, message)` queues while busy (the same background task chain auto-runs the next turn when the current one ends), wakes with history while idle (new spawn); `AgentControl(list|stop|delete)` lists/stops (aborts the current turn + sets the watch line to Cancelled, history kept)/deletes (removes the entry, name released). Multiple queued instructions are combined into one prompt in order. Subagents can still spawn further (depth cap 3) but don't manage siblings.
- **Display**: `WatchKind` threads through Watchable → WatchEvent → UiEvent → WatchCall (a contract field replacing D28's label-prefix decision); subagent watch lines show `◉ name · task`, continued turns `◉ name #N · instruction summary` (one line per turn; label unique so the TUI doesn't collide rows by label).
- **Leftover**: a synchronous (background:false) subagent whose whole turn is interrupted by the user may stay in Running (no driver) — AgentControl stop/delete can clean it up; same class as the old orphaned watch rows; no new failure surface added.
- **Step two (channel messaging, experimental flag `experimental.agentChannels`) is implemented**: design principles — capabilities universal (everyone can listen and speak), choice autonomous (silence is an agent's decision after waking; not calling Post is silence, zero cost, no wake propagation); the engine has zero game/scenario knowledge; discipline like addressing lives entirely in prompts. Implementation:
  1. **`src/channels.rs` (pure state)**: a channel = member list + serial|free + monotonic seq + full log + per-member seen cursor + message counts; `post()` does exactly three things — stamp (from is taken by the caller from `Session.instance`; the model can't forge it), serial staleness check (seen < seq → `Stale{missed}` bounced back with the delta marked as read), budget gate (50 per agent per channel / 500 per channel; over-limit freezes + one hub_mail warning). The hub member is always named `main` (`claim_name` reserved).
  2. **Delivery wake-up (orchestrated at the tool layer)**: the `AgentRegistry` mailbox generalizes into `InboxItem::Direct|Channel`, one lock for atomic "deposit + claim" so no wake is lost; the Sent branch of Post `deposit`s to every member — Idle immediately `spawn_agent_loop`s (with `absorb_inbox` formatting `[#channel #N] sender: content` and advancing the seen cursor), Running accumulates in the mailbox for batch injection at turn boundaries, Stopped silently drops. The hub goes through `hub_mail`: before each reasoning round in the query loop, `<channel-messages>` is injected; when the TUI is idle, a `◇` row's WatchEvent triggers submit_auto (with a TurnEnd check as fallback).
  3. **A serial bounce is a tool result, not an error**: `Post` returns "not sent + delta + please re-decide"; within the same turn's tool loop the model reads the delta and decides on its own to send as-is / modify / abandon — count-based ordering emerges from competition + retry (every contested round lands exactly one, convergence guaranteed).
  4. **Tool surface**: the hub (depth 0 and flag on) gets `Channel` (create/invite/kick/list; members limited to direct subagents at depth==1; late joiners get no backlog, seen set to the current head) + `Post`; cohort members (depth 1 with an instance name) only get `Post`; deeper levels and the flag-off state assemble neither. AgentControl delete also clears the member out of all channels.
  5. **Watching**: one `◇ #name` watch line per channel (WatchKind::Channel); post updates detail (N messages · latest speaker) and payload (last 50 log lines; ctrl+o expands to see the whole chat); Running-state updates don't produce notification noise.
  6. **v1 has no engine-side waiting**: round_robin/gather were both deleted in discussion (ordering = scheduling/protocol, not transport), so no timeout mechanism is needed; the budget is the only stop-loss. settings merge per field (any layer's flag on ⇒ on).

### D30. Entity view: bottom selector + alternate-screen modals (agent conversations / WeChat-style channel rooms)

- **Requirement** (named by the user, modeled on CC's multi-agent display): agents are shown at the bottom; ↑↓ selects, Enter opens that agent's conversation; channels are in the list too, opening forced-fullscreen with a WeChat-style layout (others left-aligned, mine right-aligned).
- **Selector** (`chat.rs`): a bottom entity area (chrome, above the input box) — collapsed state is a one-line dim summary (`◉ scout(running) · ◇ #table(3) — ctrl+g to view`); after ctrl+g focus, per-line `❯` selection (window scrolls, cap 6 lines), Enter sets `open_entity`, Esc collapses; the snapshot refreshes via `refresh_entities` on tick (every 15 ticks) and on WatchEvent, and only dirties when content changes. Selector keys take precedence over the global Esc semantics.
- **Modals** (`src/tui/entity.rs`): write-once scrollback means inline has no "swap content in place" — both views run on the **alternate screen** (a fullscreen host already on the alternate screen doesn't nest in/out). Self-drawn loop (ratatui Terminal paints the full frame each time), bottom-pinned scrolling + ↑↓ offset, Esc/ctrl+c returns; during a modal, `chat.tick + drain_all` continue as usual (background agent events aren't lost; the hub's auto turns still get pulled up). **Return takes a deterministic redraw**: inline sets pending_resize (the existing resize pipeline of clear + rehydrate + force_redraw, not betting on the alternate screen's restore fidelity), fullscreen sets force_redraw.
- **Agent conversation view**: registry history (❯ instructions subtle / body plain text wrapped / `⏺ tool(summary)` dim; tool_result and thinking stay out of the view) + **streaming live tail** — `AgentRegistry.live` holds an output Arc shared per turn with subagent_hooks (attached at turn start, detached at end), so opening while running shows the text being generated + `✻ generating…`.
- **Channel room**: `user` becomes a third reserved member (auto-seated like main, immovable, budget-exempt, `claim_name` reserved); bubble layout — others left (name tag + code_block_bg, consecutive same-sender merges the name tag), user right (user_message_bg, right-aligned padding; SegStyle per-segment backgrounds, not a whole-row Row.bg); the bottom input line's Enter speaks as user via `deliver_post` (the same delivery/wake path as the Post tool); **rendering = read** (mark_seen to the log tail every frame, so the person on screen is always current against serial checks and never bounces back).
- 506 tests green (bubble layout/agent view/selector state-machine pure-function tests; the modal loop and host wiring are thin coverage, same layer as fullscreen).

### D31. agent team: project-scoped roster + cross-session memory (key = project path + branch)

Requirement (named by the user): ① teams are fixed to a project (committable), and the project reads and starts them by default at launch; ② teams keep memory, bound to "current project path + current branch". Two full brainstorming rounds in `#dev-room` converged (dev-ex/ui-ux/dev/qa, 28 entries).

- **Mental model**: the team is the blueprint (persistent definition), the room is the construction site (volatile runtime state). team = a declarative roster layer reusing every existing primitive, returning to the hub-and-spoke control plane when done.
- **Config** `.bingo/team.json` (camelCase, committed, same layer as settings.json): `name` + `channel{ mode: serial|free, messageLimit: positive integer }` + `members[{ name, agent }]`. Members reference AgentDefs rather than inlining personas — the single source of truth for personas stays in `.bingo/agents/<name>.md`, and one persona can join multiple teams. Separation of concerns: settings governs "whether to start" (`team.autoStart`), the team file governs "what to start".
- **Default load at startup**: `autoStart` defaults true (the user's literal "read by default"), with two opt-outs: settings off + `--no-team` CLI. **Starting ≠ waking**: only spawn members + create the room, going to the existing Idle standby (zero tokens, zero turns); it only runs when SendMessage/channel messages arrive; acceptance asserts "token=0, no turn logs before a task arrives". Startup completes with a one-line notice `[team] dev-room ready · 3/3 standing by (/team status · /team stop)`, escalating to a warning on anomalies.
- **Implemented as three thin layers, no new runtime**: team.json parsing (the validate function shares its source with start: if validate passes, start must succeed) → `spawn_team` orchestration (existing Agent spawn + `ChannelRegistry` create-if-not-exists + member dedup) → the `/team` command family. Idempotency key = instance name: repeated start reuses (wording `spawned ×3` vs `reused ×3`, same `[team]` prefix, different verb).
- **One command line**: `/team list` (definition area / runtime area, two screens) → `start` → `status` (standing by/busy/anomaly/offline, four states, color+char dual encoding ●green ◐yellow ✗red ○gray) → `assign` (= SendMessage) → `stop`; `/team new` interactive scaffolding (output must pass validate; unreachable AgentDefs are intercepted immediately when picking members) → `/team validate`.
- **AgentDef gains `source: AgentDefSource`** (Project/User/Unknown): `load_agent_defs` records the first origin at load time (project layer loads first, first-wins dedup → same-name cross-layer overrides mark source=Project); legacy data without a source defaults to `Unknown` without error, and the UI silently omits the badge. AgentDef has no serialization path today, so adding a field is zero-break.
- **Edge cases (finalized by qa)**: missing AgentDef reports a three-part message "missing name + lookup paths + field path"; duplicate member names in config = reject; empty members = config error, single member is legal; member-level failure isolation (keep starting the rest; failed ones are marked anomalous and can be re-spawned individually); half-started states stay recognizable (`partially started · 2/3`), no auto-rollback; three-layer settings merging gets autoStart override-order tests; unknown fields are ignored (backward compatibility).
- **Memory persistence** (second convergence round):
  - **Key = project path (project_hash) + branch**: worktree scenarios work naturally (the main repo's main and `.bingo/worktrees/agent-team` differ in both path and branch, so memories never cross-pollute); absolute paths are avoided; a project_hash validation assertion prevents a copy from mounting on the wrong machine.
  - **Content layering**: full history on disk (fidelity on restore) + decision records (append-only, zero model cost; the `sources` pipe-separated field reuses `parse_frontmatter_pairs`; `type` drops to entry level).
  - **Storage**: user layer, per project+branch directories (not committed by default); collaborative export via `export` with zero transformation (frontmatter is the schema fields); `project_hash` included in the export file header.
  - **Fragment cleanup**: the `/team memory list|show|gc|merge|export` command family; gc has a TTL; corrupted/orphaned files use the same visual language as config errors (no new styles).
  - **Restore timing**: auto-restore on startup launch, one unobtrusive summary line; restore and launch both go through spawn_team; missing files silently fall back to empty history.
- **Acceptance assertion chain**: scaffold output → validate passes → start doesn't fail on config; memory roundtrip (save → restart → restore equivalent); source cross-layer override; behavior fully unchanged for old projects without a team section.
### D32. Multi-platform support: shell / process tree / TTY platform abstraction (removing the pre-D25 unix-only legacy)

Requirement (issue #1): native Windows support + official prebuilt binaries on GitHub Releases; the user further specified multi-platform (Windows / macOS / Linux).

- **`src/platform.rs` single platform layer**: `init_shell/shell` (process-level OnceLock, injected from `settings.shell` at main startup), `shell_command` (Unix `-c` + `process_group(0)`; Windows PowerShell family `-NoProfile -NonInteractive -ExecutionPolicy Bypass -Command`, other configured shells fall back to `-c`), `kill_process_tree` (Unix `/bin/sh kill -pgid`, staying within AGENTS.md's no-unsafe constraint; Windows `taskkill /PID /T /F`), `open_tty` (Unix `/dev/tty` O_NONBLOCK; Windows returns None = theme detection degrades safely).
- **Default shell per platform**: macOS `/bin/zsh`, other Unix `/bin/bash` (removing the implicit Linux dependency on zsh), Windows `powershell.exe`; `settings.shell` can override (e.g. Git Bash). Shared by the Bash tool and hooks.
- **Process-tree termination order fix**: the timeout path now does `kill_process_tree` before `child.kill()` — on Windows `taskkill /T` needs the root process alive to traverse the tree; killing the root first would orphan grandchildren; the order doesn't matter on Unix.
- **Behavior changes**: Linux's shell changes from zsh to bash (determinism first); `interactive_command_reason`'s REPL list gains `powershell/pwsh/cmd`.
- **CI/Release**: `.github/workflows/ci.yml` three-platform matrix (ubuntu/macos/windows-latest, check+clippy+test, no API key needed); `release.yml` tag-triggered, four targets (linux x64 / win x64 / mac arm64 / mac x64 cross-compiled), ZIP/tar.gz + `checksums.txt` SHA-256.
- **Verification limits**: on macOS the local run is 628 tests green + zero clippy warnings; a Windows source cross-check is blocked by `aws-lc-sys` (a C dependency needing windows.h); the Windows side is verified by the CI native runner.

### D33. Provider protocol layer + OAuth access (multi-provider protocol abstraction)

Requirement (user-named): bingo supports OAuth access to multiple AI providers (Codex/ChatGPT subscription, opencode go subscription, etc.) + a protocol abstraction layer (Anthropic as one implementation, plus the OpenAI Responses protocol). `#provider-oauth` full-team alignment + main's ruling (design doc notes/design/provider-oauth.md §10).

- **Contract-first trio** (AGENTS.md public-boundary rule): ① settings v2 — `ProviderConfig` gains optional `protocol` (values anthropic|openai, default anthropic, zero migration for existing configs), `apiBaseUrl` optional (empty → protocol default endpoint), `oauth`/`capabilities` left for P2; ② `api::contract` neutral types (NeutralRequest/SystemBlock/StreamEvent/ThinkingLevel/Capabilities) + the `ProviderClient` trait (stream→BoxStream / complete_text / list_models / count_tokens / auth_status) — consumers never see wire JSON; ③ the auth.json format (P2).
- **Client becomes a facade**: the provider table becomes `Arc<dyn ProviderClient>` + display info (key/url); `set_provider`/`with_provider`/`provider_endpoint`/`supports_images` (reading the current adapter's capabilities) keep their API; the error type `ClientError` moves into the contract, gaining `Unsupported` (e.g. openai has no count_tokens endpoint → local-estimation degradation, in the spirit of D6) and `Config` (unknown protocol → `CONFIG_INVALID` at startup).
- **Anthropic absorbed, not rewritten**: client.rs's internals move flat into `api::providers::anthropic`; retries/backoff/timeouts/400-overflow recompute/SSE/error mapping stay byte-identical (baseline 636 green → only after the move is 639 green does new code start).
- **OpenAI Responses adapter** (`api::providers::openai`, POST `{base}/v1/responses`, default base api.openai.com, `Authorization: Bearer`): system→instructions (join), messages→input items (text/image/function_call/function_call_output; thinking not replayed; the tool_result error flag encoded into the output string), tools→function tools (input_schema→parameters), thinking→`reasoning.effort` (xhigh/max converge to high) + `include:["reasoning.summary_text"]`, max_tokens→max_output_tokens. SSE mapping: output_item.added (message/reasoning/function_call) → Text/Thinking/ToolUseStart, output_text.delta→TextDelta, reasoning_summary_text.delta→ThinkingDelta, function_call_arguments.delta→InputJsonDelta (output_item.done's authoritative arguments backstop empty arguments), completed/incomplete (max_output_tokens→max_tokens, the queryLoop continuation semantics) → StopReason, failed/error→ApiError; **the two-level index (output_item + content part) flattens into a single block index** (ignored item types don't take a slot).
- **Registry `build_provider`** dispatches by `protocol`, the only place that knows "config → adapter"; the default provider still goes through the top-level apiKey/apiBaseUrl/env (anthropic).
- **OAuth (P2, main's mandatory item)**: the only hard requirement = Codex/ChatGPT (both device flow and loopback PKCE implemented; client_id `app_EMoamEEZ73f0CkXaXp7hrann`, issuer auth.openai.com; endpoints/refresh/revoke source-verified, see notes/research-oauth-cli.md); tokens stored in `~/.local/share/bingo/auth.json` (0600, opencode-compatible shape), never in project settings (rooting out apiKey leaking into committed config); lazy refresh + 401-triggered + single-flight lock; permanent failures clear the login and prompt re-login; **P2 opens with a 0.5-day spike**: does the subscription bearer work against the public /v1/responses (Path 1, reusing the P1 adapter) or is it the private chatgpt.com/backend-api codex protocol (Path 2, a third adapter).
- **opencode-go subscription correction** (research fix): it's actually an API-key subscription, not OAuth → lands as a named provider + protocol openai + apiKey, zero OAuth code; endpoints verified at P3.
- **Capability negotiation v1 static declaration** (protocol defaults + config overrides, no runtime negotiation); `cacheControl` is anthropic-only (the openai side doesn't take caching in v1); reasoning summaries map to the thinking UI and aren't replayed verbatim.
- **Verification**: cargo build + clippy -D warnings + test --bin bingo all green (P0 639 / P1 650); the mock server asserts the same StreamEvent sequence for a same-turn dual-protocol fixture (§9 contract tests); two commits, only into feat/provider-oauth (♻️ refactor + ✨ feat), never touching dev/main.
### D35. Release integrity and executable acceptance gates

- **Problem**: release publication previously built directly from any `v*` tag without proving that the tag matched `Cargo.toml`; CI skipped integration-test targets and formatting; archives were uploaded without executing the packaged binary. This allowed a tag/package identity mismatch and left the real CLI process boundary outside the gate.
- **Identity contract**: a release tag must be exactly `v<package.version>`. `scripts/check_release_version.py` reads the manifest with the Python standard library and rejects malformed or mismatched tags before any release build. The current `v0.3.1` tag therefore requires package version `0.3.1` in both `Cargo.toml` and `Cargo.lock`.
- **CI gate**: every supported host runs `cargo check --locked --all-targets`, `cargo clippy --locked --all-targets -- -D warnings`, and `cargo test --locked --all-targets`; a separate rustfmt job runs `cargo fmt --all -- --check`. CI and release workflows default to read-only repository permissions.
- **Release gate**: publication depends on a quality job that repeats identity, formatting, check, clippy, and all-target tests. Each platform archive is then unpacked by `scripts/smoke_release_archive.py` on its native architecture (including the dedicated Intel macOS runner); it must contain exactly one `bingo`/`bingo.exe`, exit successfully for `--version`, write exactly `bingo <tag-version>` to stdout, and keep stderr empty. Workflows install a pinned Python runtime before invoking the standard-library-only gate scripts. Only the final publication job receives `contents: write`.
- **CLI acceptance seam**: `tests/cli_black_box.rs` executes the Cargo-built binary in an isolated HOME/config directory. It asserts that `--version` and `--help` bypass invalid settings and that a representative non-TTY configuration failure has a non-zero exit, empty stdout, one stable `[error] code=CONFIG_INVALID msg=...` stderr line, and no ANSI escapes.
- **Scope**: these gates do not replace focused unit/component tests; they prove the packaging and process boundaries that in-process tests cannot cover. Release publication remains tag-triggered, but a bad tag, failing quality check, malformed archive, wrong binary version, extra binary, non-zero exit, or stderr output blocks publication.

### D36. Fullscreen is the default interactive host

- **Decision**: `bingo` starts the ratatui alternate-screen fullscreen host by default. It keeps the input docked at the bottom and uses in-app scrolling and mouse interaction. This supersedes D20's default-mode choice, not the inline driver itself.
- **Opt-out and compatibility**: `--inline` explicitly selects the write-once terminal-scrollback host. The existing `--fullscreen` flag remains accepted as an explicit, backward-compatible selection of the default; clap rejects using both flags together.
- **Boundary**: headless `--print` and subcommands are unchanged. Mode selection is resolved once at the CLI boundary and passed to `run_tui_session`; the two renderers keep their existing behavior and tests. (The image-capability trade-off originally noted here was unified in the same batch by the D37 placement layer.)
- **Acceptance**: CLI parser tests cover the default, explicit inline, compatible fullscreen, and mutual exclusion. Binary help output and the English/Chinese README plus bundled guide document the same contract.

### D37. Live-viewport kitty placements (diff-synced image layer)

- **Decision**: loaded, fully visible image blocks in the live viewport render immediately as kitty graphics placements in both interactive hosts (inline and fullscreen), instead of waiting for the scrollback flush or degrading to `#[image]` rows. Supersedes D36's "existing image-capability trade-offs" clause and the earlier stance that the fullscreen per-frame diff repaint cannot carry kitty images.
- **Mechanism**: `app::desired_placements` reads the assembled frame and yields one placement per fully visible loaded block (instance id = 24-bit hash of url + doc row, anchored to a screen cell); `gfx::PlacementLayer` diffs desired against active and emits converging writes — transmit once per instance (`a=T`), place-only for moves, deletes (`a=d,d=I`) for gone instances. The scrollback flush keeps its separate id space (`image_id_for`), so frozen copies never collide with viewport instances.
- **Protocol constraints** (review fix, 2026-08-08): kitty `x=`/`y=` are source-crop keys, not screen coordinates — an image is placed at the cursor cell, so positioning lives in `term::write_gfx` (one synchronized-update batch: DECSC, CUP per positioned op, DECRC, single flush) and gfx payloads carry no cursor escapes, preserving D26's term.rs-only rule. Every put names `p=1`, so a re-put with the same (image id, placement id) replaces the placement — an id-less put accumulates a second copy instead of moving. `C=1` keeps the cursor parked; every command (puts and deletes) carries `q=2` — `q=1` still sends error replies, and those APC replies arrive on stdin as typed input (main saw `ENOENT: image not found` flood the input box and drive a redraw feedback loop after a resize).
- **Terminal amnesia on resize** (main-reported fix, 2026-08-08): a resize purges the terminal's placements and image data (ratatui's autoresize also clears the screen), so both hosts route resize through the force-redraw path — clear the layer, drop the transmit cache, retransmit what is visible. Fullscreen previously only marked dirty, leaving the layer convinced its transmissions still existed: images vanished after a resize until a doc-row shift happened to change the instance id and retransmit.
- **Boundary**: Direct mode only — tmux placeholder mode keeps `#[image]` rows in the live viewport (its transfer path on scrollback flush is unchanged, D27).
- **Acceptance**: gfx diff/byte-contract tests, term positioning-wrapper test, app placement-geometry tests. Real-terminal visual verification is still pending alongside the other live-host checks.

### D38. Declarative composition layer: element tree + Static blocks (Ink's shape, not its runtime)

- **Problem**: after D26 the driver was sound, but view assembly stayed imperative — `build_rows` (~370 lines) and the app.rs chrome builders hand-threaded click row numbers, the caret's `prompt_row` offset and the settle bookkeeping. Every assembly-class rendering bug (chrome undercount, caret drift) traces to a hand-maintained offset with a second source of truth.
- **Decision**: add back the layer the iocraft removal deleted, but as pure data, not a runtime. `el.rs` — an Ink-shaped element tree (Col/Line/Rows/Lines leaves; Click/Caret/Annotated annotations) with a single pre-order render walk producing rows + absolute click ranges + the caret cell. `chrome.rs` — every section below the transcript as component functions composed into one tree. `statics.rs` — Ink `<Static>` formalized: the transcript is a `Block` list (welcome / per message / dialog / transient), `layout` renders it into the shared `Doc`, settled marks are prefix-only, and `pick_flush_mark` lives beside the mark semantics. Explicitly **no VDOM, no hooks, no retained component state** (the D16/D25 failure class); `term.rs` is untouched — the driver's write-once scrollback already was the Static guarantee, this change gives it a composition-layer shape that cannot drift from it.
- **Invariants formalized**: heights are measured by rendering (`el::height`), never predicted; annotations are offset-free (the walk computes absolutes — the Page/Field error row used to park the caret one row high because the hand count skipped it; fixed by construction); settlement is prefix-monotone (marks stop at the first unsettled block; transient blocks are counted for the lazy-flush window).
- **Migration surface**: `Row`/`ClickTarget`/`ClickRange` move to el.rs, `Doc`/`SettledMark` to statics.rs (both re-exported from chat for compatibility); `push_text` becomes `text_el`; per-message settlement is precomputed in one linear pass; shared test fixtures (`test_session`/`chat_at`) move to test_util. The flush cursor (`flushed_segments`/`tail_start`/`mark_base`) stays on `Chat` next to the state that drives it; text wrapping stays in markdown/`wrap_words` — components own text layout, the tree owns structure.
- **Acceptance**: the full suite (757 tests, including exact visible-output and click-targeting assertions) passes with behavior unchanged; fmt/check/clippy `-D warnings` clean. Real-terminal smoke of both hosts is the remaining manual check.


### D39. UI/UX audit remediation: identity, feedback tiers, modality, scoped settings

- **Scope**: the five-batch remediation of the 2026-08-08 UI/UX audit (~70 findings, 16 P0; report archived as a claude.ai artifact). Batches land as five dev commits, each gated by fmt/check/clippy `-D warnings`/full tests.
- **Batch 1 — correctness**: sixteen audited defects (merge() dropping shell/motion with a completeness fixture; update banner version comparison; atomic settings writes; layer-named parse errors; non-NotFound layer reads no longer silently skipped; ask-dialog modifier passthrough; short-sync errors keeping busy; fullscreen Page/Field error rows + short-terminal chrome guard; Esc-dismissable page errors; bash-turn interrupt reset; notice TTL; /theme BAD_ARGUMENT; preset-aware 401 hints; -p TTY fail-fast).
- **Batch 2 — identity**: provider+model is one atomic selection resolved by a single `switch_provider` (session last-used → provider default → keep+warn, mirroring the sub-agent rule; mid-turn switches refused); credentials are live (shared `TokenProvider` per provider between adapters and /provider login|logout; `AuthSource::StoredKey` reads auth.json per request; --manual tokens usable without a refresh they cannot perform); a missing top-level key degrades the default provider to a fail-fast Unconfigured adapter so the TUI (and the login command inside it) stays reachable, with onboarding on the welcome card; `api::models` supplies per-model context windows (budget/status/compaction) and a thinking gate (wire + /think warning).
- **Batch 3 — feedback tiers**: see feedback-states.md v1.24 (info tier, pinned panels, error-tier rewiring, startup notes into the TUI).
- **Batch 4 — modality**: one `close_menus()` mutual-exclusion point; render priority equals key-dispatch priority (menus before the slash dropdown); unclaimed printable keys close the menu; all pickers swallow out-of-range digits; Esc clears the residual slash query and closes the tasks panel; keys.rs regains its missing bindings and cross-links /help; reverse search gets inline keys, a failed-state, and Esc-cancels; ctrl+s announces; the bare `/` dropdown windows over the full command list.
- **Batch 5 — scoped settings**: `upsert_scoped_settings` writes each key to the layer where it takes effect (defined-layer update, else the USER layer — `.bingo/` is no longer conjured in arbitrary directories; /permissions and /mcp disable stay project-scoped by intent); `remove_from_union_lists` fixes /mcp enable against union-merged layers; `/config` is the interpreter for the five config sources (per-key winner, endpoint, credential store, unknown-key hints backed by `KNOWN_KEYS` + fixture-sync test); startup lints unknown keys and invalid enum values into TUI-visible notes. `Session.user_config_dir` threads the once-resolved XDG path so library code never re-reads the env (test hermeticity).
- **Deliberately not done**: the five menu Option fields were not collapsed into one enum — the modality invariants (exclusion, priority, leak-proofing) are enforced and tested at the dispatch/render seams instead; the enum remains a possible later simplification (106 call sites of pure mechanical churn for no additional behavior).
### D40. Experience evolution loop: explicit observed outcomes, no self-promotion

- **Problem**: Experience retrieval and recommit counts did not distinguish an entry that was merely found or rewritten from one that was actually applied and verified. Using that count as ranking evidence creates a closed self-confirming loop.
- **Decision**: keep retrieval explicit through `ExperienceQuery` and add `ExperienceOutcome { id, outcome, evidence }`. The tool is permission-gated, accepts only the full stored id, and records `helpful` or `harmful` only after the agent actually applied the entry and can cite external evidence. Retrieval or ordinary task completion never implies success.
- **Persistence**: existing Markdown entries remain valid; optional `helpful`, `harmful`, and `outcome_history` frontmatter fields default to zero/empty. Each confirmed result appends a timestamped record with concrete evidence and its SHA-256 digest; counters are derived only from records whose non-empty evidence matches the digest, so hand-edited evidence cannot silently retain ranking weight. Both outcomes preserve `verified_at` and `active`/`degraded`/`stale`. Recommit preserves outcome metadata.
- **Ranking**: lifecycle status remains the first gate. Within the same status, explicit helpful evidence ranks first, harmful evidence lowers rank, and the legacy recommit count is only a final compatibility signal. The startup index uses the same evidence ordering and a full-id tie-break before truncating to ten.
- **Governance boundary**: the model may propose a lifecycle change after harmful evidence, but it cannot promote or degrade an entry through `ExperienceOutcome`; status changes still use `ExperienceCommit` through the existing permission gate. No query-loop interception, inferred usage, embeddings, telemetry, or automatic retrieval is introduced.
- **Acceptance**: focused persistence/tool tests cover old-entry compatibility, helpful/harmful accumulation, required evidence, malformed evidence-less history, unknown ids, lifecycle-status and `verified_at` preservation, query visibility, recommit preservation, deterministic ranking, and the permission-mode matrix. The generic permission gate remains the write-authorization contract.
### D41. Subagent parity: borrowed prompt surface, shared handles, addressed notifications, batched messaging

- **Problem**: a subagent's capability gap was not the tool list (only SendMessage/AgentControl/Channel were depth-gated) but two seams that silently degraded it. The `UiHooks` the Agent tool built answered permission prompts with a constant — Ask decisions became `user denied <tool>` without the user ever seeing anything, so in Default mode a subagent could read but not write, while under `bypassPermissions` the bypass-immune `safety_check` gate was auto-approved instead. And `build_sub_session` constructed a fresh `Runtime`, whose `McpManager` starts empty: subagents had zero MCP tools, with the failure warning swallowed by a no-op `on_warning`. Alongside these, `AskUserQuestion` was assembled for subagents but wired to a hook that always returns "unanswered"; the base prompt promised subagents two things that are not true for them (rendering images to the user, being woken by background-task notifications); and a named definition replaced the whole system block vector, silently dropping environment info, CLAUDE.md/AGENTS.md, project memory and the experience index.
- **Decision — prompt surface is borrowed, not faked**: the session that owns the UI attaches its `AskFn` to the `AgentRegistry` (`attach_ask`, mirroring the existing `attach_share`), because the registry is the one place every spawn path can reach — the Agent tool, channel delivery, and the TUI channel room alike, the last of which has no `ToolContext` to thread a parameter through. Forwarded prompts are stamped with the instance name and serialized through a process-wide gate: several background instances can reach one user, and both surfaces (TUI modal, headless stdin) answer one question at a time. `AskUserQuestion` is no longer assembled below depth 0 — permission prompts are involuntary and must be forwarded, questions are voluntary and belong in the return value (hub-and-spoke).
- **Decision — shared handles, not snapshots**: MCP connections and the permission-rule table are shared `Arc`s with the parent, so a subagent sees the same MCP tools without a second handshake and `/permissions` edits reach instances already running. The MCP failure drain is gated to depth 0 so a subagent's no-op `on_warning` cannot consume the user's only report.
- **Decision — say what is actually true**: every sub-session gets an appended `SUBAGENT_NOTE` block (uncached; a short tail is not worth another cache breakpoint) stating that its text is a tool result rather than user-visible output, that it cannot question the user, and that its turn ends when it stops calling tools. `AgentDef` gains `inherit_system` (default true = append); wholesale replacement is now opt-in.
- **Decision — notifications are addressed**: `WatchRegistry` entries and notifications carry an owner (the registering session's instance name, None = hub); `consume_notifications`/`has_wake_notifications` filter by it and leave everyone else's in order. The registry is shared by hub and subagents, so an unaddressed global queue let a running subagent consume a completion meant for the hub.
- **Decision — messages are queued, batched, and acknowledged**: `deliver`/`deposit` only enqueue. `flush_pending` claims every idle instance with a non-empty inbox at a turn boundary (`query_loop`, plus explicit flushes for the surfaces with no turn behind them: channel posts and `/team assign`) and folds the whole inbox into one prompt — waking on the first message made a burst arrive one per turn. Each direct message gets a `MsgId` and an `Ack` (`Queued` → `Delivered { run }`, or `Dropped { reason }`), bounded at 64 per instance; `SendMessage` returns the id and `AgentControl(action=messages)` reports state plus age, since "queued" was previously the only signal and it is not a receipt. `stop`/`remove` record the discarded inbox instead of clearing it silently and report the count. A run chain that dies with messages queued no longer strands them: the instance is left Idle and the next boundary flush retries the batch.
- **Decision — images cross the boundary**: the attachment table moves from `ChatState` to `Session` (`api::image::Attachments`), so `#[image N]` markers — which the model already sees in its own message text — resolve anywhere, and the hub forwards an image to a subagent by repeating the marker (images ride along with a queued instruction). Separately, `ToolResult.content` may now be a block array: `Read` returns image files as image blocks and MCP image results pass through instead of flattening to a size note. `result_block` passes arrays through (clipping only their text blocks) — the previous unconditional re-stringify is what would have turned a screenshot into base64 text. Everything that reads rather than transmits (compaction, memory extraction, share HTML, the Responses adapter, which has no image tool results) goes through `api::types::tool_result_text`, which collapses image blocks to a size note.
- **Boundary**: depth semantics, `MAX_AGENT_DEPTH`, hub-and-spoke tool gating, and the permission gate's decision table are unchanged. Subagents still do not write the transcript (their history lives in the registry and the TUI instance view) and are still not auto-woken by notifications — the note now says so instead of the base prompt implying otherwise.
- **Acceptance**: 794 tests pass with fmt/check/clippy `-D warnings` clean. Focused regressions cover forwarded-vs-absent prompt surfaces, shared MCP/permission handles, the depth-0 AskUserQuestion gate, system-block composition under both `inherit_system` values, notification ownership, one-batch delivery of three messages sent in one turn, stop recording undelivered messages, retry after a failed run, marker-resolved images surviving a queued instruction, and image blocks reaching the wire uneclipsed.

### D42. Images unified on kitty Unicode placeholders; the C=1 direct path is deleted

- **Trigger**: 2026-08-08 field report — inside tmux neither host showed images (fullscreen never could: no scrollback flush exists there and `desired_placements` returned empty for the tmux mode; inline only materialized them once a block scrolled past the window top). A byte-level reproduction (passthrough `a=T,U=1` transmit + placeholder text written straight to the pane tty, verified by screenshot) proved the Ghostty 1.3.1 + tmux 3.6b chain renders placeholders perfectly — the gap was ours, not the environment's.
- **Decision**: one placement scheme everywhere — kitty Unicode placeholders (`U=1`). Placement is the text itself: each image row renders as placeholder cells (placeholder char + row/column diacritics, image id in the 24-bit foreground) through the ordinary buffer path (`view::to_line`), in the live viewport, fullscreen, and scrollback flush alike. The image data travels once per `image_id_for(url)` (`gfx::transmit_bytes`), bare or wrapped in tmux passthrough (`Transport::Bare|Tmux` — the only remaining mode axis). Transmission is position- and order-independent; nothing tracks placements, so redraws, scrolling, partial visibility and tmux repaints are correct by construction.
- **Deleted**: `PlacementLayer` (diff of active placements), `Placement`, `placement_id`, `kitty_image_bytes` (C=1), `image_print_bytes`, `GfxWrite` cursor-parking, `HistoryItem::Raw` (scrollback is Lines again), `image_block_head`, `Frame.doc_start`. `ImageRef` gained `row` (its position inside the block) so any row is self-describing.
- **Support matrix change**: WezTerm/Konsole answered the kitty query but never rendered `U=1`; with C=1 gone they drop from image support entirely (excluded at detection, one-time notice, `#[image]` fallback). Accepted deliberately — the C=1 machinery existed only for them and had already cost one repair cycle (q=2 flood, resize retransmit, move/delete diffing).
- **Transmit bookkeeping**: `gfx::Transmits` (a HashSet of sent ids) is the entire terminal-side state; reset on resize/ctrl+l (store may be purged), never deleted otherwise — scrollback placeholders keep referencing their id for the session's lifetime.
- **Verified**: 773 unit tests (pre-merge branch); headless tmux end-to-end (detached session + injected probe reply + local mock provider): both hosts render 3×15 placeholder cells with the id fg the moment the reply streams in, no probe warning, no `#[image]` fallback; the real-pixel half (Ghostty rendering placeholders through tmux passthrough) verified visually by screenshot. Known limits: the startup probe still needs a focused pane inside tmux; images in already-written scrollback cannot retransmit after a store purge (write-once).
- **Interplay with D41**: orthogonal image axes — D41 moves attachment *sending* (session attachment table, image blocks to the wire, vision subagent routing); D42 owns *rendering* what comes back. The `#[image N]` input-marker text and the transcript `#[image]` fallback are different placeholders and share no code.


### D43. Slack-shaped workspace: rail + sidebar + message pane (supersedes D30's single-conversation modal)

- **Requirement** (named by the user): research what Slack's interface looks like and replicate it as closely as practical for the team / DM / channel views. Sources consulted: Slack's own help pages on the consolidated desktop tabs and custom sidebar sections, and the published brand palette (aubergine `#4A154B`; blue/green/yellow/red accents).
- **Mapping — nothing new was invented**: workspace = the team (`.bingo/team.json` name, falling back to the project directory); channel = a `channels` room; DM = a subagent instance; app/bot messages = agent turns, with tool calls read as Slack app attachments. The Slack shape fits because the domain already had these three things; only the skin is new.
- **Structure** (`src/tui/slack.rs`, pure row builders; `src/tui/entity.rs`, the host loop): three panes laid out as `Rect`s rather than through the D38 element tree, because `el` is a vertical stack with no column concept and each pane is a flat row list — the sibling view already composed at the `Rect` level. Responsive: the rail drops below 64 columns, the sidebar below 44, and the conversation goes full-bleed.
  - **Rail** (5 cols): workspace chip + home / DMs / activity tabs. Activity lists what is unread *plus* whatever is open, so reading a conversation cannot yank it out from under the cursor.
  - **Sidebar**: workspace name, quick-switcher hint, collapsible channels / DMs sections, `#` prefixes and presence dots keyed to `AgentState`, unread rows bold-white with a red badge, the open row on Slack's blue bar. A frozen channel is struck through rather than given a glyph — strikethrough costs no columns and cannot misalign on a terminal that renders ambiguous-width glyphs at two cells.
  - **Conversation pane**: header (name + mode/count/members) → message list (day dividers, avatar chip in a per-sender colour, bold sender + `AGENT` badge + `HH:MM`, consecutive messages inside 5 minutes grouped under one name row, a red unread rule pinned where reading started, tool calls as attachments, a running instance's live tail as a typing indicator) → a rounded composer.
- **The composer is the point**: the previous agent view was read-only. A channel post goes out as `user` through the existing `deliver_post` (rendering still counts as read, so serial never bounces you); a DM goes through `agents.deliver` + `flush_agent_inbox` and renders as a pending message (`pending_of`) until the turn boundary folds it into the history — otherwise a message you just sent would vanish for a whole turn.
- **Palette — Slack's layout, bingo's colours**: aubergine shipped first and was rejected on sight. A saturated purple slab is a brand costume, and in a terminal it reads muddy against everything else the app draws. Three candidates were rendered side by side (Slack's own dark theme / neutral slate / theme-native) and the theme-native one won: chrome greys are warm neutrals, but every accent — active row, badge, presence, unread, typing — comes from `Theme`, so the workspace moves with the rest of the app instead of pinning a second brand on top of it. The sidebar stays dark under a light terminal (Slack's default does the same); only the conversation pane turns over. Non-truecolor terminals bring the whole skin down through the shared `theme::to_ansi256`.
- **No pictographs in the chrome**: the rail's icon-over-label tabs were cut for label-only tabs, and the magnifier, send arrow and slashed-circle went with them. At one cell an icon is unreadable, and the terminal substitutes whatever glyph its font carries — usually a two-cell colour emoji that also breaks the column. Structural geometry (`#`, `●○·`, `▾▸`, `▎▏`, box drawing) stays; a test asserts the rail carries nothing from the emoji/dingbat/arrow blocks.
- **Two real bugs surfaced by looking at rendered frames**, not by tests: (1) `Buffer::set_line` makes ratatui `reset()` the continuation cell of every double-width grapheme, wiping the row background — which had been punching a hole behind every CJK character on any coloured row, the user-message bubble included; `view::render_rows` now repaints what came back untouched. (2) The quick-switcher overlay repainted backgrounds but not symbols, so the message list showed through it; overlay rows are now opaque across the full pane.
- **Timestamps**: `ChannelMessage` gains `at` (unix seconds, `#[serde(default)]`, `0` = unknown so pre-D43 share documents still parse and simply render without a time). `chrono` becomes a direct dependency — it was already in the tree with the `clock` feature, so this costs no build weight and beats hand-rolling local-time conversion.
- **State lifetime**: the `Workspace` (open conversation, read cursors, collapsed sections, tab) lives on `Chat`, not the modal, so leaving and re-entering the view is continuous. Instances seen for the first time are seeded as read — a workspace you have never opened should not greet you with an unread badge for every turn that already happened.
- **Keys**: Tab cycles the panes that are actually on screen, alt+↑↓ switches conversation from anywhere (Slack's own binding), Ctrl+K is the quick switcher, ←→ fold sidebar sections, Esc returns. The view opens focused on the composer.
- **Superseded**: D30's WeChat-style bubble room and read-only agent view. The ctrl+g bottom entity selector is unchanged — it is still the way in.
- **Acceptance**: 819 tests pass with fmt/check/clippy `-D warnings` clean. Focused coverage: responsive layout shedding, full-column painting (rail/sidebar/badge, asserted against a real `Buffer`), section collapse removing rows from navigation, message grouping and divider placement, tool/queued/typing post kinds, composer geometry and caret, switcher filtering and opacity, key routing, and channel-vs-DM sending. A `#[cfg(test)]` preview harness (`slack_preview.rs`, opt-in via `BINGO_PREVIEW_DIR`) renders frames to HTML for screenshot review — the visual round that produced both bugs above.

### D44. Reply chasing: an ack is an answer, not a delivery

- **Problem**: D41 gave every direct message a `MsgId` and an `Ack`, but the mechanism was pull-only — `AgentControl(action=messages)` never pushes, so the hub has to remember to look — and, worse, the strongest thing it could report was `Delivered`, which proves only that the text was folded into a prompt. A receiver can read a message, run a turn and end it without a word; from the sender's side that is indistinguishable from a hang, and nothing in the system ever said so. Automating the check by prompting ("remember to poll") would put a correctness property in the least reliable layer available.
- **Decision — the acknowledgement is the reply**: `AckState` gains `Answered { run }`, set in `finish` when a turn produced text for the hub, and `AckState::is_outstanding` makes `Queued` and `Delivered` the same thing from the sender's side: still owed. Replying is a turn-level act, not a per-message one, so a turn that speaks answers everything the instance has read so far — a message first read during a silent run is answered by the run that finally speaks, and `Answered { run }` names that run rather than the one that carried it. Anything still queued is untouched, having not been read at all.
- **Decision — on by default, off only on request**: `SendMessage(ack_timeout: <seconds>)` tunes the wait (clamped to 5–3600) and `0` switches the check off, but omitting it arms a 300s watchdog rather than nothing. An opt-in parameter would have reproduced the very problem stated above one layer up — the sender still has to remember, and the case that most needs catching is the one nobody anticipated. Five minutes is long enough not to chase an instance for working quietly. A per-message tokio task (`spawn_ack_watchdog`) sleeps the wait and re-reads the same record the tool reports, so the automatic check and the manual one can never disagree.
- **Decision — chase, then stop**: while the sender is still owed an answer, each round appends `InboxItem::FollowUp` to the receiver's inbox and re-runs `flush_agent_inbox`. The flush repairs the stranded-at-a-boundary case (an idle instance nobody claimed); the follow-up reaches a receiver that is running or was silent, riding along with the message it names, since queueing is the only channel to a busy instance. It carries `delivered` so the prompt can name which silence it is — nobody picked it up, versus you read it and ended the turn saying nothing — and asks for a reply rather than repeating the instruction. `MAX_FOLLOW_UPS = 3` bounds it: a retry budget that never runs out is not a mechanism, it is a loop. `AgentRegistry::follow_up` reads and enqueues under the single registry lock, so a turn ending mid-check cannot produce a nudge for a message that was just answered.
- **Decision — silence only when nothing went wrong**: the watchdog registers no watch line up front. On the first missed deadline it registers one, and every non-clean outcome ends terminally — `Done` when the follow-ups drew a reply out, `Failed` when the message was dropped, the instance was removed, or the budget ran out. Terminal watch states are already the path into the hub's next turn (D41 addressed notifications), so the report costs no new plumbing. An answer inside the wait leaves no line, no notification, and no trace beyond the ack record.
- **Boundary**: an answer means the turn produced text for the hub, not that the work is done or correct — the mechanism detects silence, not bad work. Follow-ups take no `MsgId` of their own (they are not the sender's messages), never re-send the original text, and are excluded from `pending_of` so the D43 DM view does not render the harness's nudge as something the user typed. Hub-and-spoke gating is unchanged: `SendMessage` stays depth-0, so only the hub can arm a watchdog, and channel deposits and `/team assign` deliver with no wait to name.
- **Acceptance**: 826 tests pass with fmt/check/clippy `-D warnings` clean. Focused regressions cover the bounded chase (three follow-ups then `Exhausted`, follow-ups batched with the original, the ack recording its wait and round count), a silent turn leaving what it read unanswered and chaseable with `delivered: true` and then being answered by a later run, settling on a dropped message and a removed instance, and both driver halves under a paused clock — a receiver that reads and says nothing chased three times and reported to the hub, and an answered message leaving no notification and no watch line. A separate case pins the default: a send that names no wait still records one, and only an explicit `0` records none.

### D45. Channel silence is a prompt, in the system block: stopping the acknowledgement storm

- **Trigger** (reported by the user with a screenshot): one "hello" posted into a team room produced thirteen messages and no work. Every member answered the greeting; every answer woke every other member; each of those then answered the answers — "morning, X here" → "received, roles complete" → "received, QA will…" — a room of agents being polite at each other while the task sat untouched.
- **Mechanism**: D29's primitive 3 (wake-up follows delivery) is doing exactly what it promises. A post reaches every member's inbox and `flush_agent_inbox` starts a turn for each. The engine is not at fault; what was missing is that the model's default social reflex is to answer whatever just arrived, and nothing told it that here, answering is the expensive act.
- **Decision — fix it in the prompt, not the engine**: a `CHANNEL_NOTE` system block appended by `build_sub_session`, gated on `experimental.agentChannels` so a solo subagent that will never see a room doesn't carry room etiquette. It names the failure mode instead of asking for brevity: do not post to greet, introduce yourself, acknowledge, agree, say you understood, or restate the plan; if your draft would be just as true before you read the message you are answering, don't send it; post when you have something only you can supply. The `Post` tool description carries the same rule in one line — a tool description is re-sent every turn and is the last thing read before the call.
- **Why a system block and not the wake-up payload**: compaction rewrites the message history, so anything delivered as a message eventually summarises away — precisely on the long-running members most at risk of drifting back into chatter. `compact::maybe_compact` takes `&Session`, so it cannot touch `Session::system` at all, and builds its summary request with `system: Vec::new()`; the rule is guaranteed present on turn fifty by the borrow checker, not by convention. (This was the user's question, and it is why the note lives where it does.)
- **Boundary against D44**: the two rules point opposite ways on purpose and must not be collapsed. A *direct* message from the hub is owed an answer — that is what D44's `Answered` ack and its follow-up chase enforce. A *channel broadcast* is owed nothing; answering it is the failure. `CHANNEL_NOTE` says so in as many words, because "an ack is an answer" read without that boundary becomes pressure to reply to everything, which is the storm this record exists to stop. D44 already keeps its half of the line: channel deposits arm no watchdog.
- **Not done, deliberately**: making delivery selective — waking only `@`-mentioned members, the way a real Slack channel pings. That would damp storms structurally rather than by persuasion, but it revises D29's third primitive and changes what "a channel" means for every existing team. Left as a lever to pull if the prompt proves insufficient in practice.
- **Acceptance**: focused test asserts the note is absent when channels are off, present when on, and that its text names the acknowledgement failure mode rather than merely asking for concision. The compaction property is asserted by the type system and documented rather than restated as a tautological test.

### D46. The team tool: the agent proposes the crew, the user hires it

Requirement (named by the user): D31 gave teams to the `/team` command family only, so the model could not manage the crew at all — but team management is a major decision, so it must either come from the user directly or, when the model wants a change, get the user's permission first.

- **Two halves, decided separately**: *can* the model manage the team (yes — a `Team` tool at depth 0, alongside SendMessage/AgentControl in the hub-and-spoke set), and *may* it do so unattended (no). Conflating them is what would have produced either a useless read-only tool or an agent that hires three subagents while the user is looking away.
- **Consent is a tool property, not a permission rule**: new `Tool::confirm_reason(input) -> Option<String>`, consulted at the head of `safety_check` — the bypass-immune position. `Some(reason)` prompts in every mode and cannot be pre-authorized by an `allow` rule; only an explicit `deny` outranks it. The permission ladder was the wrong lever on purpose: `bypassPermissions` means "I trust you with this repo's files", which is not evidence that the user meant to staff a crew, and a standing `Team` allow rule would silently be that consent forever.
- **Actions**: `status` (blueprint + each member's runtime state + the agent definitions available to draft with — one read, so the model never has to guess a roster), `validate`, `start`, `stop`, `save`. Only the first two are read-only; the other three ask. No `assign`: dispatch is SendMessage, which the hub already has, and a second door onto the same delivery path would just be a second thing to keep true.
- **`save` is whole-document**, matching the file it writes: the roster in the call is the roster on disk, and a partial-merge dialect (`add_member`/`remove_member`) would be a second format to keep honest against `.bingo/team.json` itself. The cost — a model that forgets to re-send an existing member drops them — is paid where it belongs: the confirmation line carries the **delta**, not the document (`Rewrite .bingo/team.json · dev-room · 4 members (-ui +qa)`), so the user reviews the change rather than proofreading JSON. What a whole-document write leaves *out* is a change too, so an omitted `mode` reverting the room to serial is named as `· channel free → serial` rather than happening quietly. The prompt is one `Line`, so the reason is one line by construction: names collapse to a count past four.
- **The side door is shut**: `.bingo/team.json` joins the safety check as a sensitive target, so `Write`/`Edit` on the blueprint ask the same question in `acceptEdits` too. Without it the consent gate would have been one `Write` call wide. `Bash` writing the file is still only gated by Bash's own prompt — the same limit the existing `.git`/`.claude` check has always had, and not worth a shell parser to close.
- **Format contract**: `TeamDef` gains `Serialize` and `write_team_file` validates structurally before writing, so parse and write share one struct — what the tool saves is what the loader returns, asserted by a round-trip test. `/team new` was rebuilt on it, deleting its hand-built `json!` document.
- **Depth 0 only**: a member that could restart or rewrite the team it belongs to is a loop with the user's consent in the middle of it.
- **Acceptance**: the save → validate → start chain from the tool side (artifact passes validation, start does not fail on config, a second start reuses); a bad definition reference is refused before anything reaches the disk; the permission matrix asserts ask under all five modes plus an allow rule, and deny still wins; the `save` confirmation names added, removed, and unchanged rosters distinctly.

### D47. The workspace loses its chrome: one pane, no surfaces, portraits for avatars

Requirement (named by the user, with a screenshot): drop the background colours, keep only the conversation pane on the right, and — if inline image rendering works — give each agent a Notion-style avatar.

- **Answering the image question first**, because the rest depends on it: yes. D42's placement scheme is Unicode placeholders, whose cells are *ordinary styled text* — the placeholder character, its row/column diacritics, the image id in the foreground colour. Nothing about it is tied to the inline host: any code that can push a `Line` into a buffer can place an image, the alternate-screen workspace included. So an avatar needed no new mechanism, only a small one: `src/tui/avatar.rs` transmits a portrait once per id and hands the message list a styled segment.
- **4×2 cells, not 1×1**: a cell is about twice as tall as it is wide, so 4×2 is roughly square — the terminal's version of Slack's 36-pixel chip. One cell tall would put a face in ~18×19 pixels, which is a smudge. The second row rides in the gutter of the message's **first body row**, which the layout was already spending on indentation, so the portrait costs no rows: the image and text skins have identical row counts and differ only in the gutter (asserted). Terminals that cannot place images keep the initial-on-colour chip, and the gutter narrows from 5 to 4 to match.
- **Assets**: eight [DiceBear Notionists](https://www.dicebear.com/styles/notionists/) portraits (CC0, by Zoish), fetched once and committed under `assets/avatars/` (~55 KB, `include_bytes!`), never downloaded at runtime. Chosen framing — `radius=50`, `scale=140`, `translateY=8`, one explicit background colour per file — came from looking at the candidates rendered at their real 36×38 pixels: the default crop spends 40% of an avatar on shoulders, and at that size the background tint separates senders faster than the face does.
- **The rail and the sidebar are gone**, and with them `Tab`, `Focus::{Rail,Sidebar}`, sections and section folding. Navigation is Ctrl+K and alt+↑/↓, which already reached every conversation; a list that costs 31 columns to save one keystroke is a bad trade in a terminal. What the sidebar was genuinely carrying moved rather than died: the workspace name to the header's right edge, the unread badges into the switcher (the only place the whole workspace is visible at once).
- **No surfaces**: `Palette` lost `rail_bg`/`side_bg`/`main_bg` and every row-level background with them. The remaining backgrounds are marks, not chrome — the avatar chip, the switcher's selected row. The header also collapsed from three rows to two, its metadata trailing the title as a subtitle instead of taking a heading of its own.
- **The one thing that broke, and is worth remembering**: ratatui *patches* styles, so a span carrying no background leaves the cell's existing one intact. With no pane background repainting the column, the switcher overlay's spaces erased the glyphs underneath but not their colours, and the avatar chip beneath it glowed through the box. Fixed by marking overlay rows `bg: Some(Color::Reset)` — not a colour the view chose, but an explicit erase to the terminal's own. Caught by looking at a screenshot, not by a test; the test came after.
- **Preview harness**: `slack_preview` now paints a stand-in terminal background behind each frame, because with `Reset` on almost every cell the old grey filler was judging contrast against a colour no terminal shows. It renders the text chip, not the portrait — a browser cannot show a kitty placement, and a preview that silently dropped the gutter would be measuring a layout the terminal never draws.
- **Not verified here**: the portraits themselves, in a real terminal. The placement contract is asserted (cell width, shared id across both rows, transmit-once, gutter alignment) and the sequences are the same ones D42 ships, but a kitty placement cannot be photographed from a test.

### D48. The room re-balanced: who spoke decides, and only Post reaches the room

Trigger (reported by the user with three screenshots): they asked the hub to greet the team in `#dev-team`. The hub posted; all four members woke; the channel ended with two messages, both from `main`. The members had each written a reply — `devex` even wrote "standing by; no extra channel reply needed" — but those replies went to the hub as turn results and nobody in the room ever saw them. From the room's side the crew ignored a greeting from the person running it.

- **Two failure modes, not one.** D45 fixed the storm (one "hello" → thirteen courtesies) with a note that says silence is the default and lists greetings and acknowledgements as things never to post. It over-damped: the same rule that stops members answering *each other* also stops them answering *the human*. Both have to be held at once, so the rule can no longer be about how a message reads.
- **The axis is who spoke.** A person answers their manager and ignores their colleagues' hellos. `user`/`main` addressing the room is owed one short line, in the room; another member is owed nothing unless they named you or you can unblock them; and **never answer an answer** — the storm is replies to replies, not replies. That last clause is what makes the first one safe.
- **The mechanical gap was the real bug.** Nothing told the model that a turn woken by a channel message reports to the hub, so its text never reaches the channel. Believing it had already answered, it stayed silent *on purpose* — `devex`'s "no extra channel reply needed" is the model narrating a decision it made on a false premise. `CHANNEL_NOTE` now opens with the fact, and the `Post` description repeats it: only Post puts words in the room.
- **Where it lives** is unchanged from D45 and for the same reason: the system block, so compaction cannot summarise it away on turn fifty.
- **Acceptance**: the note's test now pins all three load-bearing clauses (the storm, the mechanism, the answer-the-human rule) so a future edit cannot quietly drop one and re-open either failure mode.

### D49. The view stops quoting the runtime at you; the crew gets a fixed cast

Two user reports from the same screenshots: the DM view rendered a wall of injected system text under a "You" name row, and team members were `room-member-1/2/3` wearing hash-picked faces.

- **Scaffolding is not a message.** `absorb_inbox` composes wake-up prompts (`[#chan #N] from: …`, follow-up chases) and `maybe_inject_task_reminder` pushes a reminder — all as user-role messages, so the view quoted them in full with an avatar and a name, as if the user had typed a page of English instructions to their own agent. They now collapse to one dim `▏` line each (`#dev-team · main: …`, `system reminder · task tools`), owning no name row, no avatar, and not breaking the grouping of real messages around them. Whatever a person actually wrote stays a message: the classifier keys on the runtime's own bracket shapes, line by line, so a multi-line instruction from the hub is untouched.
- **A crew is a standing cast.** `TeamMember` gains `avatar`, pinned in `.bingo/team.json`, because hashing an instance name collides across four short role names and changes the moment a member is renamed. `/team new` hands out portraits in roster order (distinct by construction), the Team tool's `status` lists the ids to draft with, and its schema now says a member's name is the name shown on its messages — so give it a person's name, not `dev` or `member-2`. The chip colour is keyed to the same index, so a member's identity is one thing in both skins.
- **Portraits switched to [Lorelei](https://www.dicebear.com/styles/lorelei/)** (CC0, Lisa Wischofsky) at the user's request for an anime style. Chosen by rendering eight candidate styles at their real 36×38 pixels and looking: at that size silhouette and background tint identify a sender, the face does not, so the set is picked for silhouette contrast — glasses, a beard, a pale crop, a bob, long straight hair.
- **Side effect worth keeping**: the workspace name and the pin map are now read once when the view opens instead of `load_team_file` running on every one of 30 frames a second.
### D50. The faces come to the main chat, and the crew gets to pick its engine

- **Overhead, not beside.** Putting portraits in the main transcript the way the workspace does — a gutter — would have meant moving every body: markdown re-wrapped 5 columns narrower, tool rows, `⎿` rows, diffs and thinking all re-indented, and new rows seamed against the differently-indented ones already written into scrollback. A band *above* the message costs none of that: `text_el` and the body width are untouched, and the two rows sit where nothing else was. The vertical cost is smaller than it looks, too — `TurnStart` pushes one `UiMessage` per turn with the round's tool activities interleaved into it, so a band is +2 rows per *turn*, not per activity block, and the workspace's `GROUP_WINDOW` merging has no counterpart to solve here.
- **The names are the room's own.** `main` for the hub and `You` for the human — the second already what `message_rows` writes for `post.you`. A display-name layer was rejected: these strings are addresses (channel members, SendMessage targets, `AgentControl list` rows), so a band naming the speaker something you cannot address would be a translation the reader has to hold in their head, maintained twice. `main` reading a little bare is the price, and the cheapest thing to revisit — a `display_name()` in `slack.rs` would fix both views at once.
- **Two transmit paths stay two.** The obvious cleanup — fold portraits into the `image_transmits` sweep — does not survive `view::to_line`, which *replaces* a line carrying an `ImageRef` with that image's placeholder cells and drops its segments. A portrait composed beside text on one line therefore cannot carry an `ImageRef` to be found by a sweep. Instead the row builders record the indices they drew into `Chat.faces` and both render loops transmit that set: no per-frame `O(messages × activities)` rescan, and a store purge resends exactly what is still referenced.
- **Known degradation, accepted.** `image_transmits` only reaches rows in the live document, so after a purge (any resize) messages already flushed to scrollback keep their placeholder cells with no image behind them. Markdown images have always had this; the difference is one of frequency, since a band is on every message rather than on the occasional picture. It fails soft — the four cells go blank, the name stays — so it is documented rather than defended against.
- **`⏺` stays.** It looked redundant under a band until the interleaving was read properly: a message is prose → tool → prose → tool, and each prose segment takes its own `⏺`. The band marks the message, `⏺` marks where speech resumes inside it. Dropping it would glue post-tool prose onto the tool's output.
- **The hub and the human are dealt out of the deck.** They wear the faces `main` and `user` hash to (kenji, mika), so `/team new` now deals a crew from the other six. Derived from the hash rather than named, so the reservation follows if either the hash or the portrait list changes.
- **A member pins its own engine.** `TeamMember` gains `model`/`provider`/`thinking`; `spawn_team` was already calling `build_sub_session` with three `None`s in exactly those slots. Which model does which job is a property of the formation — a reviewer on a cheap fast endpoint, an architect on the expensive one — so it belongs in the committed blueprint rather than being re-decided per spawn. Precedence is unchanged and shared with `Agent`: member, then definition, then session.
- **`validate` had to grow with it, or stop being true.** team.rs claims validate passing means start succeeds. An unknown provider and an invalid thinking level only failed per member inside `build_sub_session`, which start reaches and validate did not, so the claim would have quietly become a slogan. `validate` now takes the session and mirrors that function's rules rather than inventing stricter ones — including judging cross-provider against the session's *current* endpoint, so switching provider can change the verdict exactly as it changes what start would do.
- **`AgentStatus` reports the engine** (`model`, `provider`), surfaced by `/team list` and `AgentControl list`. A crew that mixes engines otherwise makes "which member is on which model" invisible until the bill arrives — and it is the observable that lets a test prove the blueprint's fields actually reach the spawned session, rather than being dropped somewhere in between.
- **Found by looking, not by the tests**: rendering the band through the preview harness showed a filled first prose line running two columns past the frame. `text_el` prepends the `⏺ ` marker *after* `wrap_segs` has already hard-wrapped the markdown to the full width, so every filled first line measured `width + 2`: the viewport clipped it (two characters gone, with nothing to show they had existed) and scrollback, which does not clip, would have wrapped it onto a second physical row and desynced the one-document-row-per-terminal-row invariant the write-once design rests on. Pre-existing and unrelated to the band; fixed by wrapping to `width - 2`, which costs continuation lines two columns they were not reliably getting anyway. The regression test asserts no row exceeds the width it was built for, and was checked against the unfixed code first.
- **Watch rows too, on the same terms.** A subagent's watch row is the one place in the transcript with many named speakers (`◉ 林夏 · UI review`, the label being `{instance} · {description}`), and it is already a header plus a result row — exactly the height a portrait wants. So the face spans both and the `⎿` connector on those rows is what it costs; the portrait's second row occupies the same gutter columns, so the body still hangs where the eye expects it. The user chose that trade over keeping the glyph. Only where images place: a chip skin has no face to spend and keeps `◉` and the connector untouched. `activities.rs` stays free of palettes and image capabilities — it is handed two finished cells, the same way it is handed a finished markdown renderer.
- **Not verified here**: the bands in a real terminal. Row counts, placeholder cells, name text and the face set are asserted, and the escape sequences are the ones D42/D40 already ship, but a kitty placement cannot be photographed from a test.

### D51. A crew member is told where its past is, not handed it

The user noticed that closing a session and opening a new one left members still able to see the old conversation, and asked whether that was a good idea at all.

- **What it actually cost, measured before deciding.** `persist_team_memory` wrote every member's *entire* history at exit and `spawn_team` read it back whole into the instance. No cap of any kind — not messages, not bytes, not age. On the reporting user's disk: 15 files, 847 messages, ~346k tokens; the worst single member (`ui_ux` on main) was 252 messages ≈ 132k tokens on its own. One `/team start` on `dev` preloaded ≈127k tokens across five members before any of them had read an instruction, and `/team memory gc`'s 30-day TTL is manual and keyed on file mtime, so a team touched occasionally is never pruned.
- **The distilled half of D31 never ran.** `decisions.md` — the append-only, zero-model-cost record that was supposed to be the compressed memory — has exactly one writer, `/team assign`, a command this user does not use (they drive the crew from SendMessage and the workspace composer). There were zero `decisions.md` files on disk. Only the raw-transcript half was running, and it was the unbounded one.
- **The decision: point, don't preload.** A member spawns with an empty context and one system block naming its transcript. Three reasons, in order of weight: the hub itself starts every session clean and you opt into the old one with `/resume`, so a crew member starting with 250 invisible messages is the inconsistency, not the fresh start; relevance decays much faster than the file grows; and the cost was silent — nothing at `/team start` said what had just been attached.
- **A pointer is only honest if the file can be read.** The record is serialized `Message` structs — content blocks, tool_use/tool_result envelopes, base64 image payloads — so "go read it yourself" would have failed on contact. `save_member_history` now writes `<member>.md` beside `<member>.json` from one function, so the two cannot drift: the JSON stays the exact record (which is what keeps this decision reversible), the markdown is the view the note names. Prose is verbatim, tool calls collapse to one line each, images are named rather than inlined, and thinking blocks are dropped — reasoning is not a decision and does not survive as one.
- **Migration is not a no-op.** Histories written before this are JSON only, and a note naming a file that does not exist is worse than no note, so `ensure_transcript` renders the transcript the first time a member with an older past spawns.
- **The note tells it when *not* to read**, too. An instruction that only says "your history is over there" invites a speculative 130k-token read on every first turn, which would have been the same bill by a longer route.
- **Fallout kept**: `AgentRegistry::set_history` had no other caller and is gone.
- **Considered and rejected**: capping the preload (bounds the toll but keeps it silent and still pays for stale context), and distilling each history into a summary at exit (best quality, but it puts model calls — and their failure modes — on the process-exit path). The second is the natural thing to grow into if pointers prove too passive.

### D52. A running turn is shown the way a finished one is

Reported from a DM screenshot: an instance mid-run showed no tool calls and one wall of prose with sentences butting together — `…set up task tracking and verify the current state.Now let me verify the current state:State confirmed…`.

- **The finished path was never the problem.** `dm_posts` has always turned a stored history into prose posts plus `PostKind::Tool` attachments. What the screenshot showed was the *live tail*, which `subagent_hooks` accumulated as a single `String` from `TextDelta` events alone: `on_tool_ready` and `on_round_end` were no-ops. So a five-round turn arrived as five rounds of prose concatenated with nothing between them and no sign of the tools that separated them — and the view only stopped lying about it once the turn ended and the real history took over.
- **Structure, not parsing.** The live tail is now `Vec<LiveBlock>` (`Text` per round, `Tool` per call, already rendered the way the transcript renders one), so the view maps blocks to posts instead of re-deriving structure from a string it just flattened. `output` keeps its old job unchanged — it is the flat reply the spawn returns and what `spoke` is judged on — and `live` is the same turn as the view needs it. One reader (`entity.rs`), so the type change stayed contained.
- **"Typing" belongs to the tail, and only when the tail is prose.** Marking the last *text* block would have put the indicator halfway up the conversation, directly above the tool call the agent is actually waiting on. Mid-tool (or between rounds, where `on_round_end` leaves an empty block) the indicator becomes a post of its own at the end: still working, rather than going quiet exactly when there is most to report.
- **Channels were already right** and stay untouched: `channel_posts` maps `ChannelMessage` to prose, so a room shows what was said and not how it was arrived at. That asymmetry is the point — a DM is where you watch someone work, a channel is where you read what they concluded.

### D53. The crew is the default workforce, a hire is temporary, and both work to a written agreement

Reported as issue #17: the pinned crew had weak presence. Day-to-day work went to freshly spawned ad-hoc agents while five named members sat idle in the channel; a temp spawn was indistinguishable from a member; and nothing the team was supposed to agree on was written down anywhere.

- **The crew was invisible at the moment it mattered.** The hub's only view of "who can do this" was the Agent tool's list of *definitions* — which is a menu of things to spawn, not a roster of people already standing by. So `crew_note` is a system block naming the members, what each is for, and the rule between them: give the work to a member with SendMessage; spawn only for what no member covers. A system block rather than tool copy, for D48's reason — compaction rewrites the message history and never touches `Session::system`, so the roster is still there on turn fifty. The Agent tool's description carries one pointer to it anyway, because that description is the exact place a second `dev` gets spawned beside the `dev` already idle.
- **"Temporary" had to become a lifetime, not a label.** `AgentKind::{Crew, Hire}` is set at insert — `spawn_team` makes members, the Agent tool makes hires, and nothing converts one into the other, so the blueprint cannot grow members the user never confirmed. `release_hires` then takes a finished hire away: idle, empty inbox, no message still owed an answer, at least one run behind it. The lease is two sweeps, not one, because a hire finishing in hub round N has its result reported in N+1 — releasing on the first sweep would remove the instance in the same round its result arrived. Two gives the hub exactly one round to follow up, and a follow-up refills the inbox and renews the lease.
- **Only the hub sweeps, and only where there is a crew.** Every instance shares one registry, so a subagent's own loop running the sweep would have hires releasing each other and themselves; the guard is `session.instance.is_none()`. And the sweep is a no-op unless a crew member is actually up: in a project with no crew, an ad-hoc subagent is the ordinary way to work, and deleting those would break the hub's own follow-ups. This is what keeps a behavioural change this sharp from reaching projects that never asked for a team.
- **A release is announced, not silent.** Without the notification line the hub's next SendMessage to a released hire fails with `no subagent named …`, which reads as a bug rather than as the lifetime it agreed to.
- **The agreement is prose in `.bingo/team-norms.md`**, beside the blueprint, in version control. Not a schema: it is read by models and reviewed by people, and neither wants a config format. It reaches every member *and every hire* as a system block, and the load-bearing part is the precedence clause — a direct instruction outranks it, that exception covers the point the instruction actually makes, and every other norm still holds. Norms that outrank an instruction would make the crew unusable; norms an instruction silently voids are decoration.
- **`/team new` scaffolds the agreement with real rules in it.** An empty template is one nobody edits and a file nobody scaffolds is a feature that never runs — D51 already caught the distilled half of D31 having exactly zero writers on disk. An existing file is never overwritten.
- **Hires are listed apart from the crew** in `/team list` and the Team tool's `status`, and `AgentControl list` prefixes each row with `crew`/`hire`. Each hire is also appended to the crew's `decisions.md` under `type: hire` — the same append-only log `/team assign` writes to — so "who was brought in from outside, and for what" survives the context window it happened in.
- **Not verified here**: whether the hub actually routes to members in practice. Everything mechanical is asserted (the roster reaches the prompt, the agreement reaches every member, a hire is a hire and is released on schedule, the blueprint is byte-identical across a hire), but "the model reads the rule and obeys it" is a prompt-level claim no test in this repo can make.

### D54. The team becomes a tree, and a team holds rooms rather than being one

Reported as issue #19: an organisation splits into departments pinned in different directories — engineering in `repos/ai-marketing-review-mvp/.bingo/team.json`, strategy at the project root — but a session is bound to one working directory, so from the root there was no way to see, start or manage anything but the team beside it. Asked for during the work: a team should also be able to hold several rooms with different rosters, the way a department has a standup, a release channel and a design review.

- **Two things stopped being the same thing.** D31 folded team, room and namespace into one name: the channel was called after the team and held everyone in it. `channels[]` splits them — a team declares its rooms, each with its own mode, budget and roster — and a team declaring none still gets the one room named after it, holding everybody. Explicit wins with nothing left behind it: a team with `channels` gets no extra room named after it, because a room nobody asked for is a room nobody reads. Every existing blueprint keeps working untouched, which is the whole reason the shorthand survives as a shorthand.
- **`teams[]` is the chart, and a path is how you name a team.** A reference is relative to the declaring team's own directory (a path starting at a filesystem root is refused — a committed org chart has to travel with the repo; the check is `is_absolute() || has_root()`, because those disagree exactly where it matters: on Windows `/etc/team.json` is rooted but not absolute, and a rule that held on one platform and not the other for the same committed file would be no rule at all) and may name either the directory holding a blueprint or the blueprint file itself. Each node keeps *its own* everything: agent definitions from its `.bingo/agents/`, working agreement from its `.bingo/team-norms.md`, git branch from its directory, memory partition keyed by that directory. So reaching a department from the root gives the same crew as opening a session inside it — the property that makes a subtree a thing you can move, and the one that would have been quietly lost by resolving everything against the session's cwd.
- **Addressing stays flat, and uniqueness is what pays for it.** `SendMessage("Linh")` reaches a member three levels down with no team prefix. The alternative — qualified `engineering/Linh` — would have rippled through channel rosters, pinned portraits, memory filenames and `/team assign`, and changed what a name means in every session that already exists. So teams, members and rooms are unique across the whole tree, checked at load with both files named. This is not merely a convenience: the agent registry and the channel registry are flat maps, so two teams claiming one name would silently *be* one entry.
- **A room reaches its own team and the teams below it.** Not a parent, not a sibling: a manager may convene their subtree, a peer may not conscript another department. The rule is one sentence and it buys the subtree property above — a department that could name a sibling's member would fail to validate the moment you opened a session inside it. Pre-order is what makes it cheap: a node's subtree is the contiguous run that follows it, so "in reach" is a slice check rather than a graph walk.
- **Spawn is two phases across the whole chart, not one phase per team.** Every member in the tree spawns, and only then does any room open — because a parent's room may hold a child's members, and a room opened before its occupants exist comes up missing them. Whole-tree validation runs first, so a chart with a bad reference anywhere spawns nothing at all: `validate` and `start` still share one source, now at tree scope. `autoStart` brings up the entire chart for the same reason a chart is declared in one file — it is one formation.
- **A member of a department is told where it is.** Tool paths resolve against the *session's* working directory, which is the root's; a member of a team pinned two directories down would otherwise read and write in the wrong place while believing it was at home. One system block naming its team's directory, only for nodes below the root. The alternative — a per-instance cwd — is a much larger change to the tool context for a problem a sentence solves.
- **`Team save` writes the roster and carries the chart.** The whole-document rule (D46) is right for members: the roster in the call is the roster on disk. It is wrong for `teams`, which points at other repositories and is set up by hand — a roster edit is no reason to re-decide the org, and an omitted field silently dissolving it is the worst possible reading. So `teams` travels every save intact and the confirmation line says `· 1 child team(s) kept`, because what a write leaves *out* is a change the user is agreeing to (the same reason D46 names a reverted channel mode). Rooms are editable — `channels` in the call replaces them all, omitted keeps them — and sending `mode`/`message_limit` to a team that declares its own rooms is refused rather than guessed at, since honouring it would delete every room it cannot describe. Before anything is written, the prospective chart is built and checked, so a rewrite that would break a room is refused rather than persisted and then complained about.
- **Acceptance**: from one session at the root, `/team status` shows both departments and the team under one of them, `/team start` brings up all five members and opens every room at once, and a second start reuses; a cross-department room comes up complete; a broken reference, a cycle, a duplicate name and a room reaching sideways are each refused with the file that holds the mistake named; a subtree loads standalone; `save` leaves `teams` byte-identical across a roster change.
- **Not verified here**: nothing asserts that a large chart stays *usable* — the crew note grows with the tree, and a fifty-person org would spend real context on a roster the hub mostly does not need. The cap is on depth (8), not on width, and no evidence in this repo says where the useful limit is.

### D55. Managing subagents folds like everything else, and an answer ends the message it interrupted

Two transcript-ordering defects, reported together. Managing subagents produced one two-line block per call, every one of them reading `AgentControl(action="messages")`; and answering a mid-turn question left the answer pinned to the bottom of the transcript for the rest of the turn (issue #28).

- **The target was never on the row, for a whole family of tools.** `summarize_input`'s fallback takes the input map's first key and `serde_json` orders keys alphabetically — `action` wins over `agent`, `channel` and `name` every time. So three calls aimed at three different instances rendered identically, and the fix is not a branch per tool but a rule: a call carrying a string `action` is summarised as the action plus what it is aimed at (`messages scout`). One rule covers `AgentControl`, `Channel` and `Team`, and `hint_for` inherits it through the same fallback.
- **A fold that hides a stop is worse than no fold.** The user asked for all four actions to fold. They do, but a look and a change are counted apart (`agent_checks` / `agent_stops` / `agent_deletes`), so the summary says `Checked 3 subagents, stopped 1 subagent` rather than reporting a killed run as a glance. An `AgentControl` call with no `action` at all stays a standalone row — a malformed call should not be counted as one of anything.
- **Unknown tools were closing open groups.** `classify_tool` returning `None` marks the open group inactive, so a subagent check in the middle of file work split one fold into three blocks. Classifying `AgentControl` fixes that streak as a side effect; the group model already mixes kinds on one line and needed no notion of a group family.
- **A failure inside a fold was invisible.** The summary counts a call as if it had worked and only ctrl+o shows the error row. This was tolerable when folds only held reads; it is not once a stop can fold, so the summary row now carries `· N failed` in the error colour. This applies to every group, not just subagent ones.
- **The answer's position was a symptom of `stream_msg`, not of the answer.** D22/v1.20 made an answer an ordinary user message pushed at the tail, but `TurnStart` had aimed `stream_msg` at the assistant message *before* it, and nothing moved it — so every delta, tool row and thinking block after the answer rendered above what the user had just said. The fix is to end that message and open a fresh one, the way a turn boundary would: a mid-turn answer is the user speaking, and the model's reply to it belongs underneath.
- **Two traps came with the split.** `AskUserQuestion` is a hidden tool, so `ToolStart` returns before closing the running thinking block — left running, the abandoned message could never satisfy `message_static_settled`, and with it the settle prefix and every flush after it would have stalled for the rest of the session. And `thinking_buf` carried over would make the next reasoning delta merge into a block the new message does not have, dropping it silently. Both are closed at the split, which is why `close_running_thinking` is now shared by the three callers that end a segment.
- **What the split refuses to do.** A tool still in flight owns activity indices in the current message (`pending_tools`), so the stream stays put until it lands. And a continuation the turn never filled is dropped at TurnEnd — tracked by `continuation_msg` rather than inferred from "empty assistant message", because that shape also belongs to messages this code did not open.
- **Acceptance**: 892 tests pass with fmt/clippy `-D warnings` clean. New coverage: the action+target summary across three tools (and the k=v fallback surviving for everything else), the classifier's look/stop/delete split, the summary wording, a three-call streak folding into one group with the ⎿ row naming its target, an `AgentControl` call no longer breaking a file group, the `· N failed` marker, the continuation landing below the answer, the interrupted message settling without waiting for TurnEnd, the unused continuation being dropped, and a tool in flight pinning the stream.
- **Not verified here**: no real-terminal run — both changes are asserted through the row model, not by looking at a live TUI.

### D56. The working directory belongs to the session, not the process

Issue #32: a running session was permanently bound to the directory in which bingo started, because each tool turn re-read the process cwd and the TUI kept a separate startup snapshot.

- **One shared path, no process mutation.** `Session` owns an `Arc<Mutex<PathBuf>>`; the hub, turn-derived session copies, and sub-sessions share it. `/cd <dir>` is refused while the hub has an active turn, then canonicalizes an existing directory and updates that cell. `std::env::set_current_dir` is deliberately not used: process cwd is global, so two concurrent sessions or background tasks could redirect one another. Background subagents that outlive the switch share the updated session path by design; their next turn follows the new directory.
- **Resolution follows the session at the boundary.** Every turn snapshots the session cwd into `ToolContext`; Bash and all built-in file/search tools resolve relative paths from it, permission path rules use the same base, Team and Agent load the blueprint beside it, and tool assembly reloads project skills/agent definitions there. Experience already keys from `ToolContext.cwd`; memory extraction and the TUI's `/team`, scoped settings, history, image paths, and workspace blueprint now read the same session value.
- **The switch is not a blueprint edit.** No `.bingo/team.json` or other project artifact is modified. Existing agents and channels remain in the runtime registries; subsequent dispatch and newly assembled tool descriptions use the new project, rather than silently stopping the old crew.
- **Boundary.** Startup-only work still reads process cwd: initial settings/system memory/transcript/team auto-start in `main` and standalone `bingo share`. `/cd` changes subsequent session resolution and hook launch cwd; it does not rebuild the already-sent system prompt, reload project-scoped settings/MCP configuration, or restart session lifecycle hooks.

### D57. The compact entity strip is presence; the workspace is conversation (supersedes D43/D52 entry and DM-tool rendering)

Issue #33 separates two jobs that had drifted together. The compact strip above the prompt is now a presence surface: it lists running agents and channels only, shows each running agent's model/thinking/state, and Ctrl+G opens the full workspace directly instead of entering an inline selector. Idle and stopped agents remain available in the workspace switcher; they no longer consume the scarce prompt-adjacent row.

The DM workspace is a conversation surface, not a second execution transcript. Stored `ToolUse` blocks and live `LiveBlock::Tool` blocks do not become posts; user messages and agent prose remain. Their bodies reuse the main transcript's `user_message_rows` and assistant markdown/`text_rows` path, including bubbles, prefixes, styles, wrapping, and row attributes. The workspace still owns the surrounding DM name row and existing avatar gutter, so body alignment does not replace its identity skin. The generic working indicator is deliberately kept while the live tail is mid-tool or between rounds, because hiding tool detail must not make a long operation look idle. Channel posts are unchanged.

`AgentStatus` exposes the runtime thinking level beside model/provider so both surfaces read one snapshot. DM headers show model/thinking/state. Channel headers list model/thinking for the hub and members when the complete names fit, and use one bounded aggregate otherwise; this preserves D47's compact header and keeps the composer visible on short terminals.

### D58. Running agents remain reachable from the main chat

Issue #36 restores the direct path that D57 accidentally removed without taking Ctrl+G away from the full workspace. When the input is empty, plain ↑/↓ enters a bounded list of running agents, moves the selection, and Enter opens that instance's DM; channels and idle/stopped agents stay out of this selector. Ctrl+G still opens the workspace at its last conversation, so the two entrances are complementary rather than competing.

Ctrl+B opens a main-view background-agent manager modeled on Claude Code's BackgroundTasksDialog and narrowed to bingo's hub-and-spoke model: running agents only, ↑/↓ selection, Enter detail, x stop, Esc close, and no foreground action. The detail view reports the current prompt, running state, elapsed time, cumulative output tokens, tool-use count, and the five most recent tool activities. Progress is sampled in the existing subagent UI hooks and exposed through `AgentStatus`; stopping delegates to the same registry/watch state transition as AgentControl, rather than adding a second lifecycle.

### D59. Provider rejection is the second compaction trigger

Issue #37: local token estimation is deliberately heuristic, so the provider's 400/413 response is the authoritative fallback when the estimate misses.

- **Recognition lives in the neutral client contract.** Both Anthropic Messages and OpenAI Responses pass non-success status/body pairs through the same classifier. A 400/413 plus a context-size message feature becomes `ClientError::ContextOverflow`; other errors retain their existing mapping. The stable exit code is `CONTEXT_OVERFLOW`.
- **Recovery lives in `query_loop`.** When any provider request overflows, the loop compacts without consulting the proactive threshold and immediately retries that exact request once. A second overflow is terminal. The direct retry does not run turn-boundary inbox, notification, reminder, or proactive-compaction work, and the one-retry guard is scoped to that rejected request rather than the whole tool loop.
- **The existing breaker remains authoritative.** A failed overflow compaction increments `compact_failures`; a second overflow after a successful compact increments it as a failed recovery; the existing `MAX_COMPACT_FAILURES` cap prevents further overflow compaction attempts. A successful summary resets the consecutive-failure count, matching proactive compaction.
- **Scope.** The proactive 90% threshold is unchanged. Main sessions and subagents share the same `query_loop`, so no agent-specific path exists.

### D60. SendMessage dispatches on enqueue and running agents absorb mail between tool rounds (supersedes D41 messaging cadence)

Issue #35 made D41's sender-turn boundary visible as latency: an idle crew member could wait through the hub's entire long turn, and a running member could not see a message until its whole query ended. Delivery now keeps the same registry and acknowledgement contract but changes who defines the batch boundary. `deliver` enqueues and immediately dispatches idle recipients; running recipients subscribe to inbox generations and drain everything waiting before their next model/tool round. The receiver's atomic drain is the batch boundary, not the sender's turn end.

The registry lock still owns claim/state transitions, and a claimed run is revalidated before its background task executes so stop/delete wins a dispatch race. Running-query drains keep the existing run number rather than inventing a watch round. Each delivered receipt records the amount of reply text already produced when it entered the query, so earlier prose cannot acknowledge a later instruction; only subsequent text promotes it to `Answered`. A query failure restores its claimed batch to the front of the inbox and resets those receipts to `Queued`, preserving D41's retry guarantee. The query-loop flush remains a recovery sweep, and direct delivery renews a temporary hire's D53 lease.

Channel timing stays on its existing `deposit` + explicit dispatcher path; issue #35 changes direct SendMessage delivery only. Direct inbox events use a shared watch signal; an unrelated running agent may wake, but a wake only causes a constant-time keyed registry check and never cancels or duplicates an in-flight API request.

### D61. In-stream transient API errors restart the uncommitted response

Issue #39 supersedes D1's original 2–3-attempt cap for errors received after a stream has opened. Long agent turns retry normalized `429`/`5xx`/overloaded/`server_error` events up to 10 times because losing a multi-hour turn to a transient provider event is more costly than waiting through bounded backoff. A provider-supplied retry delay wins; otherwise the delay starts at 500ms, doubles, applies ±10% jitter, and never exceeds 32s. Quota, plan, invalid-prompt, and context-overflow errors remain terminal. Short synchronous operations retain their 10s/15s feedback-layer budgets and do not enter this loop.

Retry restarts the entire still-uncommitted model response. The renderer-neutral boundary therefore carries an explicit retry reset: TUI and subagent live views discard failed-attempt deltas and tool rows before consuming the replacement stream. Headless stdout cannot retract bytes already written, so a mid-output reconnect may leave the failed prefix visible; persisted history and the result returned to the agent still contain only the successful attempt.

### D62. One classifier, one backoff for retryable stream errors

Review of the issue #39 change consolidated its three retry surfaces. Message-based transient
classification is owned by the contract layer (`StreamApiErrorKind::from_message`); the openai
adapter's code table and the query loop's `Unknown` fallback both defer to it, so the retryable /
terminal pattern lists can no longer drift apart. A bare 5xx number in a message counts as an HTTP
status only when it opens the message or follows a status marker (`http`/`status`/`code`) — prose
like "512 characters" is not transient. The exponential-backoff shape lives once in
`api::providers::backoff_delay` (500ms·2^(n−1), ±10% jitter, capped at 32s); the adapters' connect
loops and the in-stream retry loop share it, which changes connect jitter from additive +0–50% to
±10% with the cap applied after jitter.

In-stream pacing is a `StreamRetryPolicy` value: a server-directed delay wins but is clamped at
60s, because an absurd `retry_after` behind the suppressed first notice reads as a hang. Test
builds shrink only the policy's delay data, so the loop and its delay selection run the same code
in tests instead of forking control flow on `cfg(test)`. The `Reconnecting... ` progress-notice
prefix that TUI and subagent views key their replacement logic on is a shared constant
(`query::RECONNECT_WARNING_PREFIX`), not a repeated literal. Retry-after metadata additionally
accepts string forms (`"3s"`, `"250ms"`, bare numeric strings) and the `retry_delay` key.

### D63. A direct message is a private lane, and the notes say which surface reads the turn text

A member DM'd by the user would answer in the crew channel. The routing was never wrong — a DM
delivers to one inbox, and the reply is the turn text the DM window renders — the member's model
of the surfaces was: `SUBAGENT_NOTE` claimed the turn text "is not displayed to the user", and
`CHANNEL_NOTE` mentioned `user` only as a room speaker to be answered with Post. From inside that
description, the one imaginable way to reach the human is a channel Post, so private questions
were answered in front of the room. The fix is words, not plumbing (the intent-layer/code-layer
rule): `SUBAGENT_NOTE` now states the DM window exists and that its prose is exactly what the
user reads there, and `CHANNEL_NOTE` gains the medium rule — *where* a message arrived decides
where the answer goes. Channel traffic is recognizable by its `[#channel msg #N]` tag; untagged
text was sent to you alone, is answered in turn text, never with Post, and its content stays out
of channels unless the message itself says otherwise.

DMs stay sender-anonymous on purpose: the hub's `SendMessage` and the user's composer feed the
same `deliver`, and distinguishing them would add plumbing (an `InboxItem` field, UI scaffold
filtering) that the reply medium does not need — both senders read the same turn text. If tone
ever warrants it, tagging the sender is the follow-up, not a prerequisite.

### D64. The user's direct messages arrive named (amends D63's anonymity)

D63 kept DM senders anonymous to avoid plumbing; the user decided members should know when the
human is the one talking. `InboxItem::Direct` now carries `from`, and `absorb_inbox` renders the
asymmetry: the hub stays untagged — it is the default voice of direct instructions, so the common
SendMessage path is byte-identical — while the user's messages (DM window, `/team assign`) arrive
under a `[DM from user]` line of their own. Both system notes teach the tag, which also repairs a
display defect: batched user DMs previously took the `[follow-up instruction]` label, whose first
line the DM view collapsed into a "follow-up" note, eating the message's opening line.

The marker is transport scaffolding, so the DM view drops the line instead of rendering it — the
bubble already says who spoke — but still splits at it, keeping two batched DMs two bubbles.
Senders stay a closed set (`main`, `user`): SendMessage is hub-only, so no member-to-member case
exists to design for.

### D65. Models are configuration, not discovery; bingo filters nothing

The model list was discovered, never declared: every session's `/model` menu paid a round trip to
the endpoint, per-model metadata was a three-entry prefix table nobody could extend, and a preset
could ship a `model_allowlist` that hid everything a subscription offered but one. Three problems,
one shape — the user had no way to say what their endpoint serves.

Settings gains `models`, at the top level (the "default" provider) and under
`providers.<name>`. Entries are model ids or objects (`{id, display?, contextWindow?, thinking?}`)
— four fields, no cost or modality data bingo does not consume. **Declaring is authoritative**:
the menu shows exactly that list, in that order, with zero network. Metadata resolves in three
tiers, field by field: declaration → prefix table → conservative default, so declaring only a
window does not silently reset the thinking gate.

The catalog is a value on `Client`, not a process global. `Client::models()` hands out a
`ModelResolver` already bound to the current provider, and every measuring site (`budget`,
compaction, `/status`, `/context`, the footer, the thinking gate) takes one — a second session on a
second provider gets a second ruler, and the compiler finds any site that forgets, which is how #40
(disagreeing window measurements) is prevented rather than re-fixed.

Filtering is removed outright: `ProviderPreset.model_allowlist` and the openai adapter's
`ModelAllowlist` are gone, so opencode-go pulls `/v1/models` like any other endpoint. Narrowing a
subscription is now the user's `models` declaration. The codex static list stays — it is a
*fallback* for a failed dynamic fetch, not a filter, and the difference is the whole point.

Undeclared providers still pull, but no longer once per session: results land in
`~/.local/share/bingo/models-cache.json` with a 24h TTL, keyed by provider **and** base URL
(repointing an endpoint must not serve the old list). An expired entry is not discarded — it rides
along with the fetch, so a failure shows the last known list with the reason attached instead of an
empty menu ("degraded and visible"), and `r` in the level-two menu forces a re-ask. Every cache read
and write degrades silently to "no cache"; a corrupt file must never cost a startup.

`envKey` completes the picture on the credential side: a provider may name the environment variable
holding its key (`apiKey` > `envKey` > auth.json stored key / OAuth). It resolves in
`Client::build`, before the adapter, so `auth_status`, `is_configured` and the JSON protocol's
`providers.result` cannot disagree about whether a provider is usable (#43's failure mode).

### D66. One output budget per model, and compaction that reports itself

A review of the auto-compaction path found six defects that share two roots.

**Root one: one number stood for every model's output budget.** `DEFAULT_MAX_TOKENS` (64k) was both
sent on the wire and subtracted from every context window to get the effective input window. For
Claude that is right by construction. For anything else it is a guess that fails in both
directions: DeepSeek's real ceiling is 8k, so 56k of its input window was reserved for output it
cannot produce; and a model declared with `contextWindow: 32768` (D65 made that declarable, so the
guess became reachable) had an effective window of exactly 0 — threshold 0, compaction on every
turn past `KEEP_RECENT`, and a request that 400d anyway for asking 64k of output from a 32k model.

`ModelMeta` therefore carries `max_tokens` alongside `context_window`, resolved by the same three
tiers, field by field (`maxTokens` in a `models` entry → prefix table → default). `budget::max_tokens_for`
clamps the resolved value to **half the window** and is the single source for both the wire
parameter and the reserved headroom, so `effective >= window / 2` holds for any declaration a user
can write and the threshold hierarchy cannot collapse again. The clamp, not the table, is what
makes this safe: the table can only ever be wrong about models it lists.

**Root two: the compactor talked to a terminal instead of to a surface.** Compaction wrote
`eprintln!`, mostly gated on `!quiet` — which is precisely the TUI and JSON modes, so a GUI client
never learned that its context had been rewritten, and the one ungated line wrote stderr underneath
a TUI that owns the screen. The notices now go through `UiHooks::on_warning`, the one channel all
three front ends implement (TUI warning row / headless stderr / the existing JSON `warning` event).
Success is information rather than a warning and would prefer its own channel, but adding one means
a new protocol event type for one line of text; borrowing the existing channel is the cheaper trade
and is recorded here so the next reader knows it was a choice.

Four smaller findings, same review:

- The token gate's exact-count anchor survived overflow compaction. Projection floors at the anchor
  (`saturating_sub` eats a negative delta), so the shrunken history kept reading at its old size and
  the next turn compacted what had just been compacted. `compact_after_overflow` now takes the gate,
  which puts the invariant in the type rather than in a caller's memory.
- Images were estimated at one unit per base64 character — a 1MB attachment read as ~350k tokens.
  Since the image lives inside `KEEP_RECENT`, compaction could never bring that number down: on any
  endpoint without `count_tokens`, an image meant one wasted summary request per turn forever. An
  image now costs a flat 1600 tokens, Anthropic's cap for a full-size one.
- The CJK weight stays at 1 token/char even though BPE tokenizers pack CJK tighter. It is what
  Anthropic's tokenizer does, and the two error directions are not symmetric: overestimating
  compacts early, underestimating overflows the window and costs a failed turn plus recovery. The
  estimate only decides anything where `count_tokens` is unavailable, so it takes the conservative
  side — and per-model `max_tokens` gave those endpoints back the headroom the flat reservation ate,
  which shrinks the price of being early (#40 is the underestimate this replaced).
- The summary itself was the quality ceiling: ~120k tokens of history compressed to 300 characters
  and 8 recent messages, with `summary_prompt` dropping the `tool_use` blocks that hold the very
  commands the prompt asked it to keep. The prompt is now sectioned and sized to its content,
  the request carries 4096 output tokens, tool inputs contribute one bounded line each, and
  `KEEP_RECENT` is 12 — tool turns spend messages two at a time. And because the overflow path hands
  the summarizer a history the model has already refused, the prompt is trimmed into the model's own
  budget before it is sent: whole messages from the oldest end, then the head of what remains, with
  a line saying how many were left out. Local and deterministic, because a recovery path that needs
  a request to discover it failed is not a recovery path.

### D67. The audience decides the lane: venue selection for initiated messages

D45/D48/D63 all govern *replies* — who owes one, where it goes, what stays private. Initiated
messages had no rule, which leaves two symmetric failures: a member that discovers something
team-wide (a contract change, a shared blocker) reports it only to the hub as turn text and the
team works on stale ground; and a member that narrates personal progress into the room re-creates
the D45 flood through a door the reply rules do not watch. The venue rule closes both with one
criterion — the audience decides the lane — stated on all three surfaces that choose a lane: the
member's CHANNEL_NOTE (a proactive Post duty plus a status-stays-out half, guarded as a pair by
tests), the hub's SendMessage description (private lane for what concerns the receiver alone; a
channel Post for what every member should act on, because per-member private copies drift apart),
and the Post description (what concerns one agent alone goes to them directly, not into everyone's
context). Like the reply rules, it lives in system-prompt/tool-description text rather than wake-up
payloads: compaction never touches either, so the rule survives long sessions.

### D68. The terminal's own cursor is the caret; nothing is drawn where it stands

The input box carried two carets at once. `prompt_lines` sliced the text at the cursor and
*inserted* a `▋` cell, while `chrome::prompt` attached `El::Caret` at the same column and
`Frame::set_cursor_position` put the real terminal cursor there — so the terminal's blinking block
sat on top of a static glyph. The insertion is the part that hurt: every character after the caret
shifted one cell right, so editing mid-line made the tail of the line jump as the caret moved, and
on empty input the placeholder lost its first character to make room (`Try …` rendered as
`▋ry …`). The `▋` also overrode whatever cursor the user had configured — a bar or underline
cursor came out as a block, and blink was faked by not blinking at all.

Codex's ChatComposer solves this by not drawing a cursor: it computes the caret's display column
(width-aware, so a CJK glyph counts two) and calls `Frame::set_cursor_position`, and that is the
whole mechanism. bingo already had that half — `caret_cell` → `El::Caret` → `Frame::cursor` →
`set_cursor_position`, with `input::cursor_cell` measuring in display cells — so the fix is
subtraction, not addition: delete the glyph insertion and let the surviving pipeline be the only
authority. The cursor now *overlays* the cell it occupies instead of displacing it, which is what
every other terminal editor does, and shape and blink stay whatever the user's terminal is set to
(no `SetCursorStyle` is issued anywhere, matching codex's non-vim `DefaultUserShape`).

Three notes on the edges.

- The slack workspace composer (`slack::composer_rows` → `entity.rs`) was already glyph-free and
  already drove `set_cursor_position`; it needed no change, which is the shape the main input box
  now matches.
- The ask block's free-text field (`ask_el`) also drew a trailing `▋`, and it loses its cursor
  outright rather than gaining a real one: the block renders into the *transcript*, and only
  chrome-declared carets reach `Frame::cursor`, so the terminal cursor stays in the input box while
  a permission ask is open. Trading a lying caret for no caret is the right direction — a glyph in
  the transcript that looks like a cursor but is not one is worse than an unmarked field — but a
  real caret for the ask field means letting the doc tree declare one, which is a separate change.
- Visibility semantics are unchanged and deliberately so: the caret is declared on every frame
  except the Full error screen (`Frame::assemble` returns `cursor: None`, so `term.rs` hides it),
  which means it stays visible while a menu is open and while a turn is running even though input
  is not accepted then. That predates this change; it is recorded here rather than fixed, because
  the fix belongs to whatever decides what "input is unavailable" should look like.

### D69. Start re-reads a member's definition; deleting it was never the point

A member's `AgentDef` was read exactly once, at spawn. `spawn_members` keyed idempotency on the
instance name and stopped there, so a user who rewrote a member's `.md` — its system prompt, its
model, its thinking level — had one way to make the edit take: delete the instance and let the next
start build it fresh. That deletion took the member's whole conversation with it. The cost of a
one-line prompt fix was everything the member had done for the crew, which is exactly backwards:
the persona is the cheap, editable half and the history is the part that took real turns to earn.

The seam is smaller than it looks. `AgentRegistry::Entry` holds `session: Arc<Session>` **beside**
`history`, `stamps`, `inbox`, `acks` and `runs`, and the session is where a definition ends up
baked: `system` blocks, `runtime.model/provider/thinking`, the forked provider client, the cwd. A
turn takes that session by clone at wake (`flush_pending`), never before. So refreshing a definition
is one assignment — replace the `Arc<Session>` on the entry — and everything a member remembers is
on the fields next to it, untouched. No hot path, no parallel copy of the definition, no new
lifecycle. `AgentRegistry::refresh` does it under the registry lock; the next wake picks up the new
session, and `list()` reports the new engine immediately because it reads the session too.

Four rules fall out of "the session is what a turn takes":

- **Mid-turn is off limits.** A running member's turn is already holding the old `Arc`; swapping the
  entry's copy would change nothing for that turn and would change the persona mid-sentence if it
  did. `Refresh::Busy` leaves it alone and the next start catches it — bounded staleness beats a
  member that changes character between two tool calls.
- **A stopped member comes back idle.** `/team stop` has always said "history kept; /team start
  brings it back", and start never brought it back: `is_in_project` counted a stopped entry as
  reuse, so the member stayed stopped and refused mail forever. Since the whole documented loop is
  stop → edit → start, reviving is part of the same fix rather than a separate one.
- **A hire holding the name is left alone.** The idempotency check is "is there an instance under
  this name in this project", which a temporary hire (D53) can satisfy by having taken the name
  first. Reuse always conflated the two; a refresh would have gone further and rewritten the hire's
  persona out from under the task it was spawned for, so `Refresh::Hired` stops at the door and the
  start reads as the reuse it always was.
- **What "changed" means is decided over the built session, not the file.** The file is one input
  among several — the blueprint's per-member overrides, the crew's working agreement, the parent
  session's own model when the member pins none. Comparing `system` block texts plus
  model/provider/thinking/cwd asks the only question that matters: does the member face the model
  with different words on a different engine? A stored fingerprint of the `.md` would have answered
  a narrower question and needed a new field on every insert path to carry it.

Start therefore reports three outcomes, not two: `spawned ×N` for new instances, `refreshed ×N` for
definitions that moved, `reused ×N` for the ones that did not. Keeping `refreshed` distinct from
`reused` is what makes the feature checkable — "I edited the prompt and started" now has an
observable answer, and a start that silently reported reuse would be indistinguishable from the bug
this replaces.

Known coupling, recorded rather than papered over: the member's memory pointer (D51) is a system
block, so if that pointer changes the comparison sees a changed definition and reports a refresh.
Today it cannot move mid-session — `save_member_history` runs once at session end — so a start after
a member has worked still reports `reused`. Were history persisted per turn, every start would
report `refreshed` instead. The refresh itself stays correct in that world (history is preserved
either way); only the wording would get noisy, and the fix would be to exclude the pointer block
from the comparison rather than to reach for a fingerprint.

### D70. The agent owes its judgment, not just its labor

The base prompt's task rules were all restraint — do what was asked, nothing beyond — with no
counterweight, so complying silently with a request the agent knows is worse was doctrine-conformant
behavior. A "Your own judgment" section adds the progressive half: a materially better solution
found while planning is raised before building (trade-off, recommendation, question), an
apparent best-practice gap is pointed out briefly (inform, don't lecture), and two rails keep it
from degrading — "materially" bars taste-level questions, and a user who has heard the alternative
and still wants their way gets it without relitigating. Members inherit the section through the
parent system; their existing "report to the hub" redirect already reroutes the ask they cannot
make. Both halves are guarded as a pair by a test, like the venue rule (D67).

### D71. The shell contract names the real executor (#42)

Three layers disagreed about what runs a Bash tool command: `platform.rs` resolves the executor
(PowerShell on native Windows), the tool schema names itself `Bash` with a "local shell"
description, and the environment block said only `OS: windows`. The tool name is the strongest
prior the model sees, so it generated POSIX commands that PowerShell then executed — failing
outright or, worse, meaning something else (same-named aliases differ between the two).

The fix aligns the three layers on one resolved value instead of renaming the tool. `platform.rs`
gains `ShellDialect` (posix / powershell / cmd / unknown, classified by executable basename; an
unrecognized shell like fish is honestly `unknown` rather than assumed POSIX). The environment
block and the Bash tool description both name the executor, and a non-POSIX dialect carries an
explicit syntax directive — the description matters because a weak environment hint does not
outrank the tool-name prior. `session.ready` metadata reports `shell`/`shellDialect` as effective
values so JSON clients render the real executor without guessing platform defaults.

The wire tool name stays `Bash`: permission rules (`Bash(git push:*)`), hooks, stored transcripts,
and provider-side tool-call history all key on it, and a rename would break every one of them for
a cosmetic gain. The dialect strings are wire format now (tested), not display text.

### D72. The sidecar `.lock` is the whole claim; data files are never locked

Session storage locked two files per transcript: the sidecar `<stem>.jsonl.lock` and the
`<stem>.jsonl` data file itself, the second held open for the session's whole lifetime. Unix file
locks are advisory, so every other reader — `load_messages`, `/resume`, `/share`, `/compact` —
opened a second handle and read straight through the lock without noticing it. Windows locks are
mandatory: `LockFileEx` fails any read or write through *any* other handle, including handles in
the same process, so those same readers came back `Os { code: 33 }` and eleven tests failed on
`windows-latest` only. The lock that was invisible on one platform was load-bearing on the other.

The fix is one invariant instead of a platform special case: **the sidecar is the whole claim, and
a data file is never locked anywhere**. Cross-process exclusion is unchanged — a second bingo still
finds the sidecar locked and still gets "transcript is active in another process" — because the
sidecar alone always expressed that contract; the data-file lock only ever duplicated it. The
data-file handle stays open for appends, just unlocked. Applied uniformly: `transcript.rs`
(lifetime lock), `storage.rs` cleanup (both removal paths), and `tui/history.rs` save, where the
mandatory lock could have failed another process's lock-free `load` into a silently empty history.

Two Windows semantics were checked rather than assumed, since neither is visible in the code.
Renaming and deleting an open file works: Rust's `OpenOptions` opens with `FILE_SHARE_DELETE`, so
`/rename`'s data-file-then-sidecar rename dance and `delete` succeed with the session's handles
still open. And cleanup's own removal of a stale sidecar is `let _ = remove_file(..)`, so a Windows
sharing violation there degrades to a leftover `.lock` — re-lockable on the next pass — rather than
failing the cleanup. Cleanup's data-file open is now read-only: it exists to compare mtime, not to
claim anything.

Tests carried two more POSIX assumptions that the executor no longer matches (D71). `sh -c 'exit 7'`
under PowerShell is a missing command (exit 1), not an exit-7 process, so the non-zero-exit test
selects its command by `cfg`. And grep's "files only, no `path:line:text` coordinates" assertion
searched for a colon in absolute paths — which every Windows drive letter carries; it now compares
below the fixture root.

### D73. Family defaults live in a two-owner catalog file

The compiled prefix table (D65) is where per-family research lands — window, output ceiling,
thinking support — but it was invisible: the only way to see what bingo assumed about a family was
to read the source, and the only way to correct it was a per-provider, per-model settings
declaration. The `dnf` drama session showed the cost of invisible defaults: the table's 8k DeepSeek
output ceiling silently strangled a long-reasoning proxy model mid-thought, and nothing on disk
said so.

`~/.config/bingo/model-catalog.json` (created at startup, next to settings.json) makes the table a
file with two sections split by owner. `builtin` is bingo's mirror of the compiled table, rewritten
whenever an upgrade changes it — which is exactly how corrected research reaches users who never
tuned anything, the failure mode an add-only or write-once file would have baked in (today's wrong
8k would have shadowed tomorrow's fix forever). `overrides` is the user's and is never written by
bingo. Keys are id prefixes, a full id being just the longest prefix, so one mechanism covers both
"this family" and "this one model". Resolution is per field through the tiers — settings
declaration, then overrides (longest prefix first), then the compiled table, then the conservative
default — matching the D65 rule that declaring one field must not reset another.

Failure doctrine follows feedback-states: a file that fails to parse degrades to built-ins with a
startup note and is *never* rewritten — the broken content may hold the user's overrides mid-edit,
and a repair would destroy their work to save a cache. `deny_unknown_fields` turns a typo'd field
into that same visible note rather than a key that silently does nothing (the settings-lint
doctrine, applied at parse time because this file has a schema and settings.json's layers do not).
`Client` re-derives the config dir itself (XDG, then home) rather than threading a new parameter
through both construction paths; maintenance runs once in main, construction only reads.

The seed data itself (29 families across nine vendors) was researched against each vendor's official
API docs on 2026-08-13 — windows, output ceilings, thinking support, and the prefix-shadowing pairs
are recorded with per-claim sources in notes/research/model-catalog-params.md. Two rules carried
into the table: no number enters it that a primary source did not publish (families with no
documented output ceiling — kimi-k2.x, mistral — keep the conservative default), and
`supports_thinking` describes bingo's wire parameters, not the model's ability (DeepSeek reasons by
default server-side while the gate stays off). The old table's DeepSeek entry (128k/8k) had gone
stale enough to strangle a real session mid-thought; the corrected 1M/384k reaching users through
the file's `builtin` refresh is D73's mechanism doing its job on day one.

### D74. Compaction is a marker line; canonical history is never rewritten

Auto-compaction spliced its summary into the in-memory list and stopped there, while every user
turn reloads history from the transcript. Above the threshold that meant a fresh summary request
per turn — and since a summary is not deterministic, a byte-different request prefix per turn,
resetting any provider prefix cache (DeepSeek bills cache hits at a fraction of misses; the
Anthropic API caches explicitly) on every single turn. Manual `/compact` was worse: it rewrote the
whole session file, destroying the only full record of the conversation.

The transcript stays append-only and grows one new line kind: `{"type":"compact","summary":…,
"kept":N}`. Message lines above a marker are canonical and permanent; `load_messages` projects
through the *last* marker (synthetic summary message + the last `kept` message lines before it +
everything after), so a reload replays exactly the bytes the in-memory splice produced — the
summary is written once and reused until the next threshold crossing, making compaction the only
point where the request prefix changes. `/share` reads `load_canonical` and still exports the full
original conversation; a later marker supersedes an earlier one, which is also `/compact`'s repair
path for a bad summary. Old bingo versions reading a new file hit the existing skip-bad-lines
doctrine (a warning, summary lost, nothing corrupted); `kept` counts physical message lines, and a
projection whose tail would begin with an orphan tool_result advances past it — the same invariant
`safe_split` maintains — so a drifted count degrades to a slightly shorter tail, never a 400 loop.

Two prefix-stability fixes ride along. Everything the model sees now goes through `record`: the
task reminder, task notifications, channel mail, the max_tokens resume prompt and the stop-hook
message were pushed to memory only, so the next turn's reload diverged from the provider's cached
prefix at the injection point (and would have thrown off `kept`). And the Anthropic wire collapses
`cache_control` to the last cacheable system block: a breakpoint caches the whole prefix before
it, and the API rejects more than 4 — memory + crew + experience blocks together already crossed
that line whenever `cacheControl` was on.

### D75. Recall is BM25 over the stores we already have

Experience retrieval was substring-or-4-char-prefix matching against trigger keywords only — no
relevance score, and effectively blind to Chinese (a CJK sentence tokenizes as one giant "word",
so nothing short of the exact trigger phrase inside the query ever matched). Memory had no
retrieval at all: the whole file rides the system prompt.

`src/bm25.rs` is a ~150-line zero-dependency scorer — the corpora are dozens of entries and a
200-line memory file, scored in microseconds per query, so tantivy's index files and dependency
tree would buy nothing (the same instinct that hand-rolled `civil_from_days`). Tokenization
carries the semantics: ASCII words prefix-stem to 4 chars (exactly the old matcher's ≥4-prefix
rule, so "migrate" still finds "migration") and CJK runs become character bigrams, which is what
makes Chinese queries work at all. idf is the Lucene form (always positive) because in a
three-entry corpus a token every entry shares is normal, not a stopword; the noise guard is
`rank`'s relative floor instead.

Two consumers: ExperienceQuery ranks by BM25 over trigger/summary/steps/notes (weighted 3/2/1/1),
breaking ties with the old status-then-outcomes order, entry schema untouched. And each real user
turn auto-recalls up to 3 active experiences + memory facts relevant to what the user just said,
appended to the tail of the user message as a system-reminder — the tail is the one position that
never disturbs the cached request prefix (D74), and the recalled text is recorded with the turn,
so the canonical transcript stays exactly what the model saw. Stale/degraded entries are never
auto-injected; they remain reachable through explicit ExperienceQuery.

### D76. An interrupted turn is recorded, not discarded

Pressing Esc mid-stream threw the whole turn away: `query_loop`'s aborted branch returned
without `record()`, so the partial reply stayed on the user's screen while the model's history
denied it had said anything. The next turn then answered as if the interrupted attempt had never
happened — a split-brain the harness institutionalized. The only signal was a 10s warning toast,
and the tool rows that never ran closed with the green Done glyph.

Both interrupt paths now close the turn honestly. Mid-stream: whatever the model managed to say is
recorded as an assistant message, followed by a user-role message carrying CC's exact marker
`[Request interrupted by user]`. During tool execution (assistant and filled orphan tool_results
already in history) only the marker is appended, in CC's tool variant
`[Request interrupted by user for tool use]`. The strings are model-facing and verbatim CC, so a
bingo transcript reads the same as a Claude Code one. `QueryOutcome` carries which marker it wrote;
the TUI echoes that exact string instead of inventing its own wording.

Three edges decide themselves once the rule is "record what can be replayed". A partial reply is
trimmed to text and *signed* thinking: an unfinished `tool_use` has no result and an unsigned
thinking block fails signature verification, and either one would 400 every later request in the
session — the same permanent corruption `fill_missing_tool_results` exists to prevent. Nothing
accumulated means no assistant message at all: an empty message is a second lie, and endpoints
reject it. And the marker follows every user-initiated stop, including the cancel that lands
between a tool finishing and the next round, and the interrupted `!` command — where the tool row
is now also closed as Interrupted, because a row left Running keeps its message from ever settling
and freezes the session's whole flush prefix.

On screen the marker is the record: the transient warning is deleted (it expired while the fact it
reported stayed in the history), and a user message equal to either marker renders as a single
error-coloured line rather than a `❯` bubble — the harness wrote it, not the user, and it carries
no send time. `ToolStatus` finally has an `Interrupted` arm of its own: amber glyph, result line
`Interrupted`, no borrowed output and nothing to expand. Interrupted rows are still not counted as
failures inside a collapse summary, and `interrupted` auto-continue suppression is unchanged.

### D77. The terminal is handed back on the way out, and the compaction warning goes where the user is

Three ways the harness talked past the user, all of them about the last moment before something
goes wrong.

**The panic hook.** Everything outside the turn task ran with no safety net. A panic there left raw
mode on, the alternate screen up and the cursor hidden — and printed its message into a terminal
that could no longer render it, so the user got a frozen frame and a shell that answered nothing.
`std::panic::set_hook` (installed once, at TUI setup) now restores the terminal first and delegates
to the previous hook after, so the message still prints, into a terminal that can show it. The hook
does nothing but emit fixed escape sequences: no allocation, no formatting, no lock. `TUI_ACTIVE` is
claimed after `enable_raw_mode` and *swapped* on release, so the clean teardown and the hook cannot
both restore — whichever gets there first wins and the other is a no-op — and `TUI_FULLSCREEN` says
whether the teardown owes the alternate-screen steps. The setup-failure path takes the same release,
which is also how it stops leaking the alternate screen when `execute!` fails halfway.

One condition beyond the blueprint's `AtomicBool`: the restore also requires the panicking thread to
be the one that claimed the terminal (a `Cell<bool>` in thread-local storage, no destructor, so the
hook can read it at any point in a thread's life). The blueprint gates on "TUI active" alone, but a
panic *inside a spawned task* is not the session's death — v1.31 built `supervise_turn` precisely to
turn it into the recoverable `TURN_LOST` state, and pulling the screen out from under a session that
is about to offer retry / go back would trade one broken terminal for another. The host is driven by
the runtime's root future, which `block_on` polls on the calling thread and never migrates, so
"panicked on the claiming thread" is exactly "this panic is unwinding out of `main`". Panics
elsewhere keep the screen as it is, which is what they did before this batch.

**The pre-compaction warning.** It was `eprintln!` under `!session.quiet`, and `quiet` is true for
everything except `--print` — so the warning existed for the one host that did not need it and was
silent for the TUI and the JSON client. D66 already moved compaction's own report onto
`UiHooks::on_warning` for exactly this reason; the warning that precedes it now takes the same
channel and reads `context at {tokens} tokens; auto-compact at {threshold}`. Headless behaviour is
unchanged in substance: the default hooks are an `eprintln!`. The per-turn `context: N tokens`
progress line stays on stderr and stays `quiet`-gated — the TUI already carries that number in the
footer every turn, and a warning row repeating it would be the same fact twice, in the tier reserved
for things that need attention.

**The bands.** The footer coloured at 70% and 90% of the raw context window while auto-compaction
fires at 90% of the *effective* window (the window minus that model's own output budget). The two
denominators differ by a number that varies enormously between models, so the bands described no
model correctly: 78% of the window for a current Claude model, 55% for DeepSeek v4 — where the
danger band opened at 90%, thirty-five points after compaction had already run. Bands are now
measured as a distance to the trigger, in percentage points of the window the label prints: warning
within 20 points, danger within 5. `ContextUsage` therefore carries the trigger alongside used and
window, and `ContextUsage::for_model` builds all three from one resolver, so the number on screen
and the number the compactor obeys cannot drift apart. `UiHooks::on_context_usage` passes that
measurement whole rather than two loose integers — a receiver rebuilding the window or the trigger
from its own model handle would be the second ruler this replaces. Label, bar and percentage are
untouched.

### D78. A folded tool result is still a result

The transcript folds a run of reads and searches into one line (`Read 3 files (ctrl+o to
expand)`), and that fold was where their output went to die. `ToolDone` stored the result text on
the row only in the `!in_group` branch: a folded member got its one-line summary (`Read 173 lines`)
and nothing else, and because `Activity::expandable()` is content-based, the row was not even
offered as expandable. Opening the group revealed three summary rows over three empty bodies — the
output of a call that had really run was unreachable for the rest of the session, and nothing else
in the UI keeps it. The model's history holds the `tool_result`; the user's only recourse was to
ask for the same file to be read again.

Materialization is now one function for both branches (`result_content`): the fold is a display
state, not a different kind of result, so it cannot be the thing that decides whether a result is
kept. Same blank-line filter, same Bash `$ cmd` / `[Exited with code N]` strip, same budget. The
budget is the one the model already lives under — `MAX_RESULT_CHARS`, applied to every result on
its way to the UI by `clipped_result` — now also applied at the row, so the paths that build their
own output without passing through the clip (a tool error string, a denied call) are bounded too.
No new key, and no per-row cost the standalone path did not already pay: a group of N members
retains exactly what N standalone rows retained.

Three things this deliberately does not change. The collapsed row is untouched — the summary
wording, the `· N failed` tally and the `⎿` hint under a running fold all read group state and
never member content, so a fold looks exactly as it did. An interrupted member still keeps
nothing: D76's early return sits above the capture, and a call stopped before it produced output
must not borrow a body — its result line says `Interrupted` and there is nothing to open. And
`Skill`'s summary rewrite and standalone Bash's auto-expand stay standalone-only; the first is
about a pointer path that a folded row never shows, the second about `!` commands, which do not
fold.

One behaviour beyond storage. Every row of an *open* group is wrapped in that group's click target
(`El` emits enclosing ranges first and click resolution takes the first match), so a member row
cannot be clicked open on its own — with the content stored but the members left collapsed, the
mouse could open a group and still not reach a single line of output. Members therefore follow the
group: opening it opens them, folding it folds them. `ctrl+o` already expanded every activity and
every group in one pass, so the keyboard route needed nothing.

The tests went into a new `chat_tests_c.rs`: `chat_tests_b.rs` stood at 3785 lines against the
discipline gate's 4000-line cap, and the split is what the gate asks for. `chat_tests_a` /
`chat_tests_b` were already split by size alone.

### D79. Feedback for the user who is not looking

bingo had no way of reaching a user who had switched away: no bell, no desktop notification, no
terminal title. A permission prompt could block a turn for as long as it took someone to glance
back at the window, and a ten-minute turn finished into an empty room. Every feedback state the
project had specified assumed eyes on the screen — which contradicts the first principle of
`feedback-states.md`, that feedback must not depend on the environment. The terminal *is* an
environment where the user is routinely somewhere else.

**The channel.** One settings key, `notifications`, with the usual three-layer merge and a `/config`
line: `auto` (default), `bell`, `iterm2`, `kitty`, `ghostty`, `off`. `auto` reads the terminal —
`TERM_PROGRAM=iTerm.app` → `OSC 9`, kitty (`TERM_PROGRAM` or `TERM=xterm-kitty`) → the three-part
`OSC 99`, `TERM_PROGRAM=ghostty` → `OSC 777 ; notify` — and falls back to the terminal bell, which
every terminal has. There is deliberately no probe. The image layer can afford one because the kitty
graphics protocol answers a query; a notification protocol has none, so a wrong guess is not a
failed handshake but silence, and the only safe default for an unknown terminal is the sequence that
predates all of them. `off` silences the title too: a user who turned notifications off did not ask
for their tab to be renamed either.

**Who writes.** `notify.rs` builds bytes and writes nothing, the same split `gfx.rs` already has
with the image transport. The inline host's rendering invariant is that `term.rs` is the single
owner of escape-sequence writes, and a bell emitted from the state machine would land in the middle
of a viewport diff. So the term layer grew one narrow pair — `write_attention` beside
`write_transmits`, both over the same out-of-band helper — and the two hosts collect
`chat.notify.take()` after their frame (the fullscreen host through the crossterm backend behind its
`Terminal`, its equivalent single write point). Attention bytes are position-independent by the same
contract the transmits have: no cursor moves, no cell is painted, so they need no synchronized-update
bracket.

**tmux.** A notification OSC goes in the passthrough envelope, reusing `gfx`'s `tmux_passthrough`:
tmux has never heard of 9/99/777 and drops them. The bell does not — tmux acts on a bell it can
see, and wrapping it would hide it from `monitor-bell`, which is the whole point of falling back to
a bell inside a multiplexer. The **title** is bare for the mirror-image reason: `OSC 2` is a
sequence tmux *does* understand, sets as the pane title, and propagates to the window title under
`set-titles on`; passthrough would set the outer terminal's title behind tmux's back and tmux would
overwrite it on its next redraw. The spec's "wrap OSC, not bell" rule sits under the notification
sequences, and this is that rule read at the level it was written for — the title's correct
envelope is decided by whether tmux understands the sequence, and it does.

**When.** Three triggers, and nothing else: a permission ask accepted by `drain_asks` (the turn is
blocked and only the user can unblock it), a `TurnEnd` whose wall time reached 10s, and a
flow-level `UiEvent::Error`. A queued ask does not ring — the modal it would announce is already on
screen. A short turn does not ring — it was watched. A page-level error does not ring — it is a hint
beside a session that carries on, and a channel that fires for those is a channel users turn off.
The bodies are one line each and say only the state: `Waiting for permission`, `Turn complete`,
`Turn failed`. A body like `Turn complete · 214 tests passed` would put information in a
notification that exists nowhere else, and notifications are not a surface anything can be read
back from.

**The title** carries the same three states continuously, which is the part that works while the
user is looking *at* the tab bar: `✳ bingo — working…` at `TurnStart`, `✳ bingo — waiting for
permission` when an ask lands, `bingo — {directory}` when idle. It repeats nothing (an unchanged
title emits no bytes) and is handed back on the way out — an empty `OSC 2` from `restore_terminal`,
so D77's panic hook covers it for free, and it was trivially reachable there because that function
is already a list of fixed sequences that allocates nothing. It is gated on a `TUI_TITLE` latch set
only when the channel is enabled: a session that never took the title must not clear whatever the
shell had put there.

**Environment sealing.** Library code does not read the environment — it is what keeps the test
suite from depending on the shell that launched it, the same rule `Session::user_config_dir`
follows for the config directory. `TerminalEnv` is read once in `tui::run_tui_session` and handed
down resolved, so the auto-detection matrix is a table test over a struct rather than a test that
mutates process state. `Notifier::default()` is the disabled channel, which makes every existing
`Chat` in the suite silent by construction and makes "did this trigger fire?" a question about
bytes rather than about a terminal.

No focus tracking in this batch. A notification fires whether or not the terminal is in front, which
is what CC does, and the alternative — a focus-tracking mode enabled with `CSI ?1004h` and drained
out of the event stream — is a second piece of terminal state to own for a refinement nobody has
asked for yet.

### D80. Esc is one ordered stack, and a dialog does not outlive its turn

Three findings from the interaction audit, all of them the same shape: a key's meaning was
decided by the *order handlers happened to be written in*, and that order was wrong.

**The stack.** `on_key_at` was a chain of nine `if handler(...) { return true }` calls, each with
its own `KeyCode::Esc` arm, and `escape()` tested `self.busy` first. So Esc with a slash dropdown
open over a running turn killed the turn — the user was looking at the dropdown, reached for the
key that closes things, and lost the work instead. Esc now belongs to no handler. It is judged at
the top of `on_key_at` against one list, `EscLayer::ORDER`, walked top-down; the first open layer
is dismissed and nothing below it is consulted. The order, verbatim:

```
AskDialog › Menu › AgentManager › SlashDropdown › Search › ErrorRow › InfoLines
          › HelpPanel › TaskPanel › Interrupt › BashMode › ClearInput
```

`Interrupt` sits *inside* the list, and that placement is the decision. Everything transient
closes before the turn does; with nothing open, Esc still interrupts on the first press, exactly
as before. `ClearInput` below it keeps esc-esc on a non-empty draft. With no layer at all — idle,
empty input — Esc does nothing, which is the slot D91 will fill with rewind rather than a new key.
`AgentManager` and `ErrorRow` are not in the original sketch: both were Esc-dismissable overlays
already, and a stack that omits them is not a single source. `BashMode` moved the other way, below
the interrupt instead of above it: the `!` prefix is sticky, so a running bash command *always* sits
under an empty bash-mode composer, and a bash-mode-above-interrupt order would have meant Esc
exiting the prompt prefix while `!sleep 100` ran on. `Ctrl+C` deliberately skips the stack
and interrupts unconditionally — the layers are a refinement of "close what I'm looking at", not a
shield around a running turn, and a user who wants out needs one key that always means out.

The stack decides *which* layer hears the key; the layer keeps its own close semantics in its own
handler (the model picker returns provider-list → menu one press at a time, search cancels without
adopting its hit, the dropdown takes a bare `/` query with it). `esc_dismiss` delegates rather than
reimplements, so D39's `close_menus` mutual-exclusion invariant and the digit-swallowing guards are
untouched. The hint text reads the same list: the running-status row asks `esc_busy_hint()` and
prints `(esc to close · Ns)` while a layer is stacked over the turn, `(esc to interrupt · Ns)`
otherwise. A status row that promises an interrupt Esc will not perform is the original bug wearing
a different hat.

**The dialog's lifetime.** `pending_ask` was set by `drain_asks` and cleared only by answering it.
An interrupt aborted the task awaiting the answer and left the question on screen: the footer went
on saying `Waiting for permission…`, and 1-9 or Enter sent a `DialogAction` into a dropped oneshot
— a dialog that had been dead for minutes and still took keys. Both ends of a turn now settle it
through one mechanism, `cancel_asks`: an explicit `Cancel` on the wire (the receiver already reads
`Cancel` and a closed channel identically, so this is fail-closed either way), the requests still
queued in the mpsc emptied with it, and one dim line where the dialog was —
`(pending permission dialog cancelled with the turn)`, display-only, because the tool call that
asked went down with the turn and the model's history must not learn of an answer nobody gave. It
is a user-role state line like D76's interrupt marker, rendered without the `❯` bubble and without
a send stamp, through the same special case.

Turn end and interrupt do not use the same rule, because subagents share this modal queue
(`attach_ask` points them at it). A dialog on screen when a *foreground* turn ends normally belongs,
by construction, to something else — the turn that owned it could not have finished while blocked
on it. So `TurnEnd` cancels only requests whose receiver is already gone (`Sender::is_closed`,
which is the fact rather than a proxy for it: the task was aborted, panicked, or was lost), and
puts live ones back in order. An explicit interrupt takes everything, because that is what the
user asked for. This is a deliberate deviation from "cancel any pending ask on TurnEnd": that rule
would have denied a background agent's question every time an unrelated foreground turn happened
to end first.

**The arrows.** `entity_key` claimed plain ↑/↓ on an empty composer to open an inline
running-agent selector, which cost the composer its prompt history the moment any agent was up —
the two keys a terminal user reaches for first, taken by a feature reachable two other ways. The
selector is deleted: `entity_focus`, its bounded list rendering, `ENTITY_ROWS_MAX` and the
`↑↓ select agent` hint go with it, and the strip is a one-line presence summary again. Its one
capability that had no other home, "open this agent's DM", moved onto the ctrl+b manager's detail
view (Enter, which previously just closed the panel), so `EntityOpen::Agent` stays alive on the
surface that replaced the selector. Ctrl+G still opens the workspace; it now rides in `control_key`
with the rest of the ctrl chords instead of in a handler of its own.

### D81. Approval is a decision about an act, not about a tool name

The permission dialog was `⏺ Allow running Bash` over `Allow` / `Deny`. Three things were wrong
with it, and they compound: the user approved a *category* while the actual command or file change
stayed off screen; the only way to stop being asked was to quit and edit `settings.json` by hand;
and a refusal reached the model as a wall (`permission denied: Bash (…)`) with no way to say what
to do instead, so the model's next move was a guess. This batch replaces the dialog with CC's
three-option shape and widens the contract underneath it enough to carry the extra meaning.

**The verdict widens.** `AskFn` was `Fn(&str name, &str reason) -> bool`. A bool cannot express
"allow, and stop asking", and two `&str` cannot describe what is about to happen. It is now
`Fn(&AskContext<'_>) -> AskOutcome`, where `AskOutcome` is `Allow` / `AllowSession` /
`Deny { feedback: Option<String> }` and `AskContext` carries `{ tool, reason, input, cwd, scope,
diff }`. A struct rather than more positional arguments because both new fields are *derived*, not
passed through: the preview needs the cwd to resolve a relative `file_path`, and the session option
needs a rule the prompt surface has no business inventing. All four implementations moved with it —
`modal_ask` (TUI), `stdin_ask` (headless, `y` / `s` / anything-else), the subagent forwarder in
`tool/agent.rs`, and the JSON host.

**Scope is the permission engine's answer, not the UI's.** `permission::session_allow_rule` derives
the narrowest rule that matches this exact call — `Bash(<first word>:*)`, `Edit(<parent dir>/)`,
`WebFetch(domain:<host>)`, or the bare tool name — and then *verifies* it with `rule_matches` under
the same `MatchMode::All` the gate uses. Derivation and matching are two readings of one grammar and
they are allowed to disagree; the verification is what keeps `cd /tmp && rm -rf /` from being scoped
to `Bash(cd:*)`, which would have matched neither sub-command and silently promised nothing.

The harder question is what to do when a session rule *cannot* work. `can_use_tool` consults `ask`
rules (step 2) and the bypass-immune safety check (step 4) before allow rules (step 7), so for a
write into `.git`, a `confirm_reason` tool (D46's Team confirmations), or the user's own
`ask` rule, a rule pushed into `allow` is dead text. Two bad options were available: install it
anyway (the option lies), or route around the safety check (a rule table consents to something D46
decided a rule table must never consent to). Chosen instead: `session_allow_rule` returns `None`
for those cases and the option is **not rendered** — the dialog shows `Yes` / `No…` and nothing
promises anything. Deviation from the dispatch, which specified exactly three options; an option
that cannot keep its word is worse than an option that is not there.

On `AllowSession`, `gate_tool` pushes the rule into `session.runtime.permissions` before the tool
runs, so the call that asked is covered by the rule it created. That table is the runtime one — the
same handle `/permissions` edits and subagents share — and is never persisted: "this session" is
what the user was offered, so this session is exactly how long it lives. The UI touches no file.

**A refusal carries a direction.** Option 3 does not resolve the dialog; it opens a feedback row
under itself (reusing the existing `free_text` / `DialogAction::Answer` mechanics, so nothing new
was invented for the input). The text arrives as `Deny { feedback }`, travels through `GateDecision`
alongside — not inside — the reason, and lands in both deny sites as
`permission denied: {name} ({reason}). User guidance: {text}`. Without feedback the wording is
byte-identical to before. `GateDecision` replaces the `(behavior, reason, input)` tuple `gate_tool`
returned: guidance belongs *after* the parenthesised reason in the sentence the model reads, so it
could not be folded into `reason` without corrupting a string other code formats. The `!` command's
deny site has no `tool_result` to wrap; it gets the same sentence in the place it already put the
denial, inside `<bash-stderr>`.

**The preview is the tool's own dry run.** `Tool::preview_diff(input, cwd)` defaults to `None`;
`EditTool` and `WriteTool` implement it by reading the file and computing the same replacement
`call` would, through a shared `replace()` and a shared `resolve_path()` — one source of truth, so
the diff shown is the diff that lands. It never writes. `gate_tool` calls it only when the decision
is `Ask`. Bash needs no hook: its `command` field is the preview. Rendering bounds both (12 diff
rows, 6 command rows) with `… N more lines`; `ctrl+e` lifts the bound and additionally prints
`session rule: <rule>`, which is where the promise option 2 makes gets spelled out in full.
`ctrl+e` is claimed only while a dialog is open and only when there is a preview, so it stays
readline end-of-line everywhere else.

**Two guards.** Enter and digits are inert for `ASK_CONFIRM_GUARD` (400ms) after a permission
prompt appears, so a keystroke already in flight when the dialog rendered cannot approve anything;
`ask_opened_at` is stamped in `drain_asks` and read against the `now` that `on_key_at` already
threads, which is the whole test hook — no clock injection was needed. Esc, shift+tab and ctrl+e
are not delayed: none of them is a key anyone types ahead. The guard is deliberately limited to
permission prompts; a mistyped AskUserQuestion option costs a round trip, a mistyped approval runs
a command.

**The receipt.** The dialog is chrome and disappears with the answer, which left a transcript where
a turn simply carried on with no record of who let it. Each resolution now leaves one dim state
line where the dialog was — `> yes` · `> yes, don't ask again this session` · `> no` ·
`> no — <feedback>` — extending D80's `is_state_line` rather than adding a parallel mechanism.
Matching is exact for the three fixed lines and by the `> no — ` prefix for the fourth; a bare
`> ` test would have turned every pasted markdown quote into a state line. Display only: the model
learns the verdict from the gate, not from a message the user did not write.

**Housekeeping.** The dialog's key handling and rendering moved out of `chat.rs`/`chat_tail.rs`
into `src/tui/ask.rs` (`impl super::Chat`, same pattern as `chat_tail`). Both files were within a
few hundred lines of the 4000-line cap and this batch adds to both; the move is what keeps the cap
honest without touching the pre-existing debt. `cancel_asks`, `drain_asks`, `ask_click` and the
answer-message helpers went with it, so the dialog's whole lifetime reads in one file.

AskUserQuestion is untouched: same options, same numbered `Other` row, same decline message, no
confirm guard, and `AskKind` on the request is what keeps the two shapes apart. The JSON host keeps
protocol v1's two-option prompt — its reply is a `bool` on the wire, and widening that is a protocol
version rather than a rendering change (recorded here so D8x does not mistake it for an oversight).

### D82. The transcript view is what makes "ctrl+o to expand" true

Every collapsed row has advertised `(ctrl+o to expand)`, and in the inline host that promise could
not be kept. Inline is write-once scrollback: a settled row is printed into the terminal's own
buffer and belongs to the terminal from that moment, so there is no row left to rewrite. What
ctrl+o actually did was **reprint** — `expand_transcript` opened every fold, rewound the flush
cursor to zero and set `dump_transcript`, and the next frame froze the entire session into
scrollback again. The old collapsed copies stayed where they were, above the new expanded ones: one
duplicate transcript per press, an accepted trade-off (D27) that got worse the longer the session
ran. The fullscreen host, meanwhile, had a different key under the same name: `toggle_transcript`
folded and unfolded the **last message only**, in place. Two behaviours, one binding, and neither
of them was "show me the output".

**The decision is Claude Code's**: `ctrl+o` opens a transcript view — an alternate-screen pager over
the whole session — and `ctrl+e` inside it toggles show-all. The alternate screen is the entire
point. Scrollback cannot be rewritten, but a screen that is discarded on exit can be redrawn as
often as we like, and leaving it puts the previous screen back byte for byte. The compensation for
write-once is not a cleverer write; it is a second surface.

**The row builder is `Chat::build_rows`, borrowed.** The pager does not re-implement rendering: it
sets the flush cursor to zero (so the document covers the whole session and not just the unflushed
tail), opens every fold when `show_all`, calls the same builder both hosts draw from, and gives the
fold state and the cursor straight back, setting `dirty` so the host rebuilds its own document
before it draws again. Markdown, diffs, image placeholder rows, the CJK width machinery and the
collapse summaries are therefore identical to the main screen by construction, and D78's retained
group content is what the view has to show. A test asserts the borrow is returned — leaving it out
would collapse the user's open rows and reprint the session, which is the bug this batch deletes.

**Shape follows `entity.rs`** (the ctrl+G workspace): a self-drawing modal loop that owns the
terminal while it is open, with `already_alt` so the fullscreen host does not nest a second
alternate screen inside its own, and the same guarded enter/leave. One thing is added there:
`AltScreenClaim` flips D77's `TUI_FULLSCREEN` latch for the lifetime of the modal, so a panic
inside the pager over the *inline* host restores the alternate screen instead of leaving the user
in it. The pager also declares `pub(super)` on `app::image_transmits` rather than copying it —
the images the rows address were transmitted on the main screen, and a resize can purge the
terminal's store.

**Split for testability**: `TranscriptState` is pure state (rows, offset, viewport, show-all,
query, matches, current) with pure transitions, `transcript_rows` is the builder over the session,
`on_key` maps a key to `None` / `Rebuild` / `Close`, and the loop is the thin shell that owns the
terminal. Fifteen tests cover the behaviour without one.

**Details worth recording.** Show-all defaults **on**: a reader who opened the transcript came for
what the fold hid, and CC's transcript shows detail. Toggling it re-anchors the reading position
*proportionally* — expanding every fold moves every row number below the first one, so no absolute
anchor survives, and the position in the session is what the reader cares about. Search folds case
per character (`to_lowercase().next()`) so a hit found in the folded copy is still a byte range of
the original; the highlight splits segments on the hit boundaries and leaves every other colour
alone. `q` typed into an open search input is a letter, not an exit. With a permission dialog on
screen ctrl+o is **inert**: the pager would bury the question blocking the turn, and the answer is
one keystroke away.

**Deviations from the dispatch.** (a) The dispatch left "refresh on close of search / on demand"
optional; it is not built. The content is a snapshot at open — a running turn's events queue in
their channels and drain after close (the `entity.rs` precedent) — and unlike `entity.rs` the loop
does not tick, because draining would mutate the session behind rows the pager has already laid
out. (b) `Chat::toggle_transcript` had become the convenient "open every fold" call in a dozen
render tests. Rather than rewrite them all against `doc_click`, it is replaced by a `#[cfg(test)]`
`expand_all_folds` with the rationale in its doc comment; the four tests that specifically covered
the *collapse* direction of the old toggle now round-trip through the mouse click target, which is
the surviving in-place fold surface, and one that had become a duplicate of another is deleted.
(c) The `(ctrl+o to expand)` copy is left exactly as it was: it was never wrong about what the key
does, only about where it did it.

### D83. Steering is a message that arrives while the work is still changeable

Enter while busy queued the message; `submit_queued` drained the queue at TurnEnd. So a correction
typed thirty seconds into a five-minute turn was read only after the work it was correcting had
finished. The user's real choice was to wait it out or press Esc and lose the turn — and the queue,
which looks like a way to speak mid-turn, was in fact a way to speak *after* it. Claude Code's
`messageQueueManager` injects queued messages at the running turn's next tool barrier (its `next`
priority), and that is the alignment target: the model reads the correction while it is still
deciding what to do, without anything being cancelled.

**The barrier is a place, not a moment we invent.** `query_loop` already has it: after
`execute_calls`, once every `tool_use` has a paired `tool_result` in `blocks` and before those
blocks are recorded as the next user message. The request has not gone out; the results are
complete. Steered text is appended to that same message as extra `Text` blocks, *after* the
tool_results — the API rejects a user message whose tool_result blocks are not first, and there is
no second message to put them in without inventing a turn the model never asked for.

The drain is guarded by "is this turn going to ask again": `!interrupted && !stop_after_tools &&
!is_cancelled(&cancel_rx)`. Each of those three ends the turn a few lines below the record, and a
message folded into a request that is never sent is a message swallowed. A reply with no tool call
never reaches the barrier at all; TurnEnd's queue covers it exactly as before.

**The marker.** The text arrives beside tool output, where an unlabelled paragraph reads as more
tool output. It goes under `[Message from user, sent while you were working]` — the family already
in use (`[DM from user]`, D64; `[Request interrupted by user]`, D76): a bracketed statement of fact
with no instruction attached. Not XML: the codebase's user-interjection convention is a marker
line, and inventing a tag here would have been a second convention for one caller.

**The channel is a projection of the queue, not a second copy.** `SteerQueue` (new `src/steer.rs`)
holds the eligible prefix of `Chat::queued` and is re-armed from it on every change — enqueue,
pull-back, absorption, turn boundary. A *prefix*, deliberately: a slash command is dispatched on the
client side and cannot travel to the turn, and a message queued behind one that jumped into the turn
would run the two in the opposite order from the one they were typed in. Images take the same
answer, for a smaller reason — mounting attachments is `start_turn`'s path and nothing at the
barrier can do it — and rather than build a second attachment path for a case the user can trivially
re-send, such an item stays queued and everything after it waits with it. The channel belongs to one
turn and is `reset` at every turn start, so a message the previous turn declined is never folded
into a turn the user never typed it at.

**The race has one winner, by construction.** `take` is atomic and records the ids it took;
`tui_hooks` takes and announces (`UiEvent::Steered`) in the same closure, so there is no window in
which an item is in the request and still pending on screen. `↑` pull-back asks `reclaim` first: an
item the turn already took answers `Absorbed`, and the pull-back does nothing — the event, already
in flight, is what removes it from the queue. The taken-id ledger is what makes re-arming safe: the
composer re-arms from a queue that still holds the absorbed item until that event lands, and without
the ledger it would offer the same message to the next barrier.

**On screen.** One turn renders as one assistant message, so a line merely pushed after it would
sink below everything the turn still had to say. `absorb_steered` closes the reply block and opens a
continuation — `open_continuation_message`, the move an AskUserQuestion answer already makes — so
the line sits between the reply written without it and the reply written with it, which is the order
the history holds. It renders as one dim line under `↪`, no `❯` bubble, but it keeps its send stamp:
unlike D76's and D80's state lines, the user did write it and it did reach the model, so
`is_steer_line` is deliberately *not* folded into `is_state_line`. The queued rows are unchanged and
gain CC's verbatim `Press up to edit queued messages` beneath them, only while a turn is running.

**Deviations and consequences.** (a) The steer line is marked by its `↪ ` text prefix rather than by
a field on `UiMessage`, which has seventeen literal construction sites and no constructor; this is
the convention `is_interrupt_marker` and `is_ask_receipt` already established, and the false
positive it admits (a user message that itself begins with `↪ `) costs one bubble. (b) The three
query-side barrier tests live in `src/query_steer_tests.rs` rather than inline: they are `query`'s
loop tests and borrow that suite's mock-server helpers (four of which become `pub(super)`), but
`query.rs` was at 3877 lines and adding them inline left thirteen lines of headroom under the cap.
(c) `QueuedInput` gains an `id`. Matching absorbed items by text would have merged two identical
messages into one. (d) Scope holds: subagents keep their inbox mechanics, and headless, `--print`
and JSON protocol v1 pass `no_steer()`, which is `Vec::new` — those hosts have no composer, and the
turn runs byte-identically to before.

### D84. A running command is evidence, not a spinner — and ctrl+b is the exit

A foreground `Bash` call was a silent `⎿ Running…` from the moment it started until the moment it
exited. The tool already streamed its output incrementally — `spawn_output_readers` has fed a shared
buffer since the beginning — but nobody read that buffer until the command was done, so a two-minute
`cargo build` and a hung `ssh` looked exactly alike, and the only key that reached a running command
was Esc, which kills it. Claude Code shows a rolling tail under the tool row and lets ctrl+b move the
running command to the background mid-flight; both are the alignment targets.

**One command at a time, by construction.** `Bash::is_concurrency_safe` is always false, and
`execute_calls` runs non-safe tools serially, so a session has at most one foreground command in
flight. The whole feature is built on that: `src/live.rs`'s `LiveBash` has one slot, one promote
signal, one tail. A second `arm()` would be a bug, and `debug_assert` says so; if it ever happened,
the newcomer keeps its own sender inside its guard rather than evicting the incumbent, because an
evicted (dropped) sender must never be read as "the user pressed ctrl+b". `promote_requested` parks
forever on a closed channel for the same reason — the same shape as `executor::cancel_requested`.

**The tail is its own buffer, not the result buffer.** `BoundedOutput` stores bytes verbatim and
stops growing at `bashOutputMaxChars` (48k), which makes it useless as a tail twice over: a
`\r` progress bar would paint hundreds of lines, and a long build would freeze the tail at the cap
while the command kept running. `TailBuffer` applies terminal semantics as the bytes arrive —
`\r` rewrites the current line, `\n` commits it, five lines are kept, escape sequences are dropped
rather than handed to a renderer that would write them straight to the terminal (`line::sanitize`
deliberately preserves ESC), and a newline-free stream is capped per line. The result the model
reads is byte-identical to before.

**Coalescing is a rule about the wire, so it is testable as one.** `TailCoalescer::admit` takes the
clock as an argument: at most one event per 100ms, and never an event that would repaint rows the
host already shows. A ticker samples every 50ms — faster than the floor on purpose, so the last
write of a burst still lands once the floor passes rather than waiting for output that never comes.
A thousand writes in one interval is one event.

**Promotion moves the audience, not the process.** `promote_to_background` registers the watchable
the background path already uses, feeds it the same `BashCell` that has been counting lines since the
command started (so the task panel reports the elapsed time it really has), swaps the sink's tail for
the task id, and spawns a task that owns the *same* `Child` and the *same* reader handles. Nothing
is killed, nothing is restarted, the buffer is not lost, and the timeout is dropped because a
background task is not bounded by the foreground call's budget. The model gets the exact shape
`background: true` returns, plus a note saying the user did this and it did not.

**Deviations and consequences.** (a) The tail event carries no `tool_use_id`. The TUI's activity
model has no id key at all — `ToolReady` explicitly discards it and `ToolDone` finds its row by
scanning for the first running call with a matching name — so an id would have been a field nothing
could resolve; the renderer finds the running `Bash` row the same way `ToolDone` does, which the
serial invariant makes unambiguous. (b) The tail rows are rendered in `chat_tail.rs`, not in
`layout_activity`: a Bash call is usually *folded*, and a folded activity is never laid out at all —
its only row is the group's `⎿` hint row — so both render paths need the rows, and only the caller
knows the width they must be clipped to. (c) Output written before a promotion is not replayed into
the notify conditions. It has already been on screen, and the completion payload still carries all of
it; replaying it would fire a notification for an error the user just watched scroll past. (d) The
blueprint's "line counter on the `⎿` row" is a `… N lines` row above the tail instead of a suffix on
the row itself, which `activities.rs` renders from `ToolCall` alone — and it appears only when there
is something to count, i.e. when the tail is not the whole output. (e) `ToolContext` gains a `live`
field (37 literal construction sites, all tests, defaulting to a detached handle) rather than a
task-local: `ask_question` already crosses from `UiHooks` into `ToolContext` this way, and hidden
control flow was not worth saving 37 lines of mechanical diff. (f) Scope holds: headless, `--print`,
JSON protocol v1 and every subagent hold `LiveBash::detached()` — no tail is produced, nothing can be
promoted, no new wire event exists, and those hosts run byte-identically to before.

### D85. Completion is one surface, and its candidates come from the command's own data

The composer could complete exactly one thing: a slash command's **name**. `/model `, `/theme `,
`/think `, `/resume `, `/provider login ` all take arguments from a small, enumerable, *already
existing* set, and every one of them had to be typed blind — with a trailing space the dropdown even
kept offering the command the user had just finished typing. There was no `@` at all: no way to
name a file to read or an agent to talk to without typing the path by hand. Claude Code and Codex
both have `@` and both complete arguments; this batch adds them, on one mechanism.

**One scorer.** `src/tui/complete.rs` holds a hand-rolled fuzzy matcher (no new dependency): a
case-insensitive subsequence match with a consecutive-run bonus (8), a word/path-boundary bonus
(10), a base point per matched character and a capped "started late" penalty, ties broken lexically.
Two details are load-bearing. (a) The match is found in **two passes** — forward for the earliest
end position, then backward from that end for the tightest positions reaching it. A single greedy
pass matching `ch` against `src/chat.rs` takes the `c` of `src` and scores a scattered match; the
backward pass finds `ch` in `chat` and scores the run, which is the difference between a useful
ranking and a random one. (b) Folding is **ASCII-only**: `char::to_lowercase` may expand one
character into several and would break the 1:1 index alignment the scorer needs between the folded
and the original candidate, so non-ASCII simply matches case-sensitively. Model ids, identifiers and
paths — everything this ranks — are ASCII. An empty query matches everything with score 0 and
`fuzzy_rank` then does **not sort at all**: a catalog lists its preferred model first and the
session list lists the most recent session first, and re-sorting an unfiltered list would destroy
information the source deliberately carried.

**One registry.** `Chat::arg_candidates` is a single `match` on `(command, already-typed arguments)`
— one arm per command, arity falling out of the tuple. Every arm reads the data its own handler
validates against, never a copy: `/model` the D73 declared catalog (`client.declared_models`) and,
failing that, the same two synchronous fallbacks the `/model` picker's level two uses, in the same
order — this session's fetched list, then a *fresh* disk cache; `/theme` `THEME_LEVELS`; `/think`
`THINK_LEVELS`; `/resume` the untruncated `transcript::list` that `/resume <keyword>` itself
searches; `/provider` `provider_order()` plus its two subcommands, and after `login`/`logout` the
strictly smaller set that `slash_provider_login` accepts (configured providers ∪ presets — pointedly
*not* `default`, which login rejects). The picker's fourth tier is a network fetch and is
deliberately absent: a dropdown rebuilt on every keystroke must not block on an endpoint. `None`
from the registry means the argument is free-form, and nothing opens.

**One dropdown.** `update_slash_suggestions` is now the composer's single completion funnel and
picks exactly one surface per edit: an `@` token under the caret, else the argument phase, else the
command-name phase. Because it is one funnel, every edit path that already refreshed the old
dropdown refreshes the new ones for free, and the mention and slash dropdowns can never be open
together — which is why `EscLayer::MentionDropdown` sits *adjacent* to `SlashDropdown` in D80's
order rather than above or below it in any meaningful sense, and why the peel-order test walks the
stack a second time instead of walking a longer one.

**`@` mentions.** Opening is anchored at the caret and at a word boundary — start of input or after
whitespace — which is what keeps `user@example.com` an email address rather than a mention of
`example.com`. Files come from `git ls-files --cached --others --exclude-standard -z` inside a
repository, so `.gitignore` is honoured for free and the list matches what the user means by "the
project"; outside one, a bounded walk (depth ≤ 6, no hidden directories, no `target/`
`node_modules/` …). Both are capped at 5000 entries and the cap is stated in the dropdown footer
rather than silently swallowed. The gather happens **once per open**, not per keystroke: the
snapshot lives in `MentionState::all` and the query only re-filters it. Selecting inserts a file as
its relative path and an agent as `@name` — the agent keeps its `@` because that token is exactly
what D90's routing will read.

**Deviations from the blueprint sketch, and why.** (a) Tab is *accept*, not "longest common prefix
then accept": with a live fuzzy-ranked list the top row is already the answer, and an LCP step would
insert a prefix that matches nothing the user can see. (b) No directory entries with a trailing `/`:
`git ls-files` yields files, and synthesising the directory set would roughly double the list to
support a navigation gesture that fuzzy matching makes unnecessary. Both are recorded here rather
than silently dropped; neither is hard to add later. (c) Line-leading `@agent` **routing** is
untouched — this batch builds the completion surface only, per the D90 scope guard.

**Two things fixed in passing, both inside the code this batch rewrites.** Enter with an argument
dropdown open used to take the name-phase shortcut ("complete and execute"), which would have
dispatched `/deepseek-chat` as a command; it is now gated on the name phase. And
`apply_slash_suggestion` never moved the caret, so Tab-completing `/mo` into `/model ` left the
cursor at column 3 — it now follows the text it completed.

### D86. The composer is a readline, and a prompt worth writing is worth writing in your editor

Four gaps, one surface. (a) A prompt longer than a sentence had to be composed in a single-line-ish
box with `\`+Enter for newlines, while every shell and every git commit has had `$EDITOR` for
decades. (b) `shift+enter` was already wired to insert a newline and could never fire: no terminal
sends it distinguishably unless someone asks, and nobody asked. (c) The paste-burst heuristic
protected Enter and nothing else, so a pasted `@` or `/` opened a dropdown mid-paste — and that
dropdown then claimed the very Enter the rest of the paste needed as a newline. (d) The readline
set was half there: `ctrl+a/e/k/u/w/y` and `alt+b/f` existed, but the kills all overwrote one
`String` slot, so two kills meant the first was gone, and `ctrl+p/n`, `alt+d`, `alt+backspace` and
`alt+y` were not bound at all.

**`$EDITOR` (ctrl+g, and the readline chord `ctrl+x ctrl+e`).** `$VISUAL` first, then `$EDITOR`;
neither set is not a dead key press but an info line naming the variable to set. The draft goes to a
pid-and-counter-tagged temp file, the terminal is handed over in full, the editor runs as a child
inheriting it, and the file comes back as the draft with one trailing newline trimmed. A non-zero
exit **keeps** the draft — an abandoned editor is the one case where replacing the prompt would be
unrecoverable — and says so. The replacement is one undo step, so `ctrl+_` returns to what was typed
before the editor opened.

Two things about the hand-over are not obvious and are the reason this is not three lines in
`chat_tail.rs`. First, **the terminal goes through D77's claim protocol**, not through an ad-hoc
`disable_raw_mode`: `suspend_terminal` performs the same release the clean teardown does, and the
guard's `Drop` performs the resume, so a panic while the editor holds the screen restores exactly
what a clean return would have. This also forced the setup sequence out of `run_tui_session` into
`setup_terminal(fullscreen)` — the resume needs the same answer to "what does this host have
switched on", and a second copy of it would have been a second answer. Second, **crossterm's
`EventStream` has to be dropped first**. It parks a background thread inside `poll_internal`, which
*reads* the terminal; an editor sharing that descriptor loses keystrokes to it. Dropping the stream
wakes that thread and ends it, and the host continues on the fresh stream left in its place.

The chord is armed by `ctrl+x` for exactly one key. Taking it at the top of `on_key_at` rather than
inside `control_key` is what makes "anything else clears it" mean *anything* — a plain character,
Esc, a dialog key — instead of only the control keys that reach the same handler. `ctrl+e` keeps
being end-of-line everywhere else, and D81's dialog `ctrl+e` and D82's transcript `ctrl+e` are
untouched, because neither ever sees an armed chord.

**Kitty keyboard enhancement.** At setup, if `supports_keyboard_enhancement()` says yes, push
`DISAMBIGUATE_ESCAPE_CODES` and nothing else — `REPORT_EVENT_TYPES` would add release events and
change what every existing binding sees. That one flag is what makes `shift+enter` arrive as its own
key rather than as a bare `\r`, so the newline binding that was already written finally fires. The
pair is a **latch, not a counter**: the push is skipped if already pushed, the pop writes nothing if
nothing is pushed, and every teardown path calls the pop blind. There is exactly one pop site —
inside `restore_terminal` — which is how the clean exit, the panic hook, the setup-failure path and
the `$EDITOR` suspend are all covered by construction rather than by four remembered calls.

**Paste-burst hardening — what was already there and what is new.** Already handled: Enter inside a
detected burst inserted a newline instead of submitting, and bracketed `Event::Paste` was one edit
rather than one per character. New: during a burst *and* during a bracketed paste, `after_edit`
**closes** the completion surfaces instead of recomputing them. A dropdown is an answer to typing,
and pasted text containing `@` or `/` is not asking the question. This also removes a per-keystroke
file walk and `load_skills` call from the middle of a large paste. The end of a burst is only
observable from the next event, so the re-evaluation happens on the first keystroke that is not part
of it — which is the honest reading of "re-evaluate once at the end". The empty-input meanings of
`!` and `?` are suppressed the same way.

**Known limit, stated rather than papered over**: the heuristic cannot see a paste's first
`PASTE_BURST_KEYS` characters — they are indistinguishable from typing — so a burst-pasted payload
whose *first* character is `!` still enters shell mode on terminals with no bracketed paste. That is
what bracketed paste exists to fix and why it is the primary path; the burst heuristic remains the
fallback it was documented as.

**Readline motions and the kill ring.** `ctrl+p`/`ctrl+n` route through `vertical()` — the same
function `↑`/`↓` use — so history browsing, multi-line caret movement and D83's queue pull-back
(including losing the race to a turn that already took the message) are identical by construction
rather than by parallel implementation. The kill ring is bounded at 10 and coalesces
readline-style: consecutive kills in the same direction rebuild one entry in **text** order, so
`ctrl+w ctrl+w` yanks both words back the way they were typed. `ctrl+y` inserts the top; `alt+y`
immediately after rotates in place and wraps, and anywhere else does nothing at all rather than
inserting something unasked. Both "immediately after" rules are counted in **keys, not seconds**: a
key that went to a dialog or a dropdown still ticks the counter and so breaks the chain, which is
exactly what readline means by an intervening command.

Word motion splits in two, which is readline's own distinction and happens to be what a path needs.
`alt+b`/`alt+f`/`alt+d`/`alt+backspace` use a new `subword_*` boundary — alphanumeric runs, so `/`,
`-`, `_` and `.` are all stops and `src/tui/chat_tail.rs` is six stops rather than one. `ctrl+w`
keeps the whitespace word, exactly as a shell does. `word_right` had no caller left afterwards and
was deleted.

**Key-conflict resolutions.** `ctrl+g` was the agents/channels workspace and is now the editor. The
workspace keeps a door — the ctrl+b manager, Enter on an agent — and D89 retires the modal anyway,
so `EntityOpen::Workspace` survives as an `#[allow(dead_code)]` variant with the batch that removes
it named on it, rather than being deleted here as D89's work done early. `ctrl+k` was **already**
bound to kill-to-end-of-line, so the "leave it unbound for D90" guard could not be honoured
literally; its meaning is unchanged and it now feeds the ring, because leaving it writing to a slot
nothing reads would have broken `ctrl+y` after a `ctrl+k`. `ctrl+x`, `ctrl+p`, `ctrl+n`, `alt+d` and
`alt+y` were free; `alt+backspace` was a plain backspace. `ctrl+x` inside the ctrl+b agent manager
still stops an agent — the manager is judged before the composer, so the chord never sees it.

### D87. Motion is a layer with one gate, not a habit each surface picked up

bingo's animation was almost all absence. The only continuously moving thing in the app was the
welcome card's update banner, breathing on a properly specified sine curve; everything else either
did not move or moved without anyone deciding it should. The `motion` setting — the app's opt-out,
and the determinism knob tests lean on — was resolved independently in three places and consulted at
five, none of which was the spinner. So `motion: "off"` stilled the banner and the tok/s glyph while
the loudest moving thing on screen kept spinning, which is worse than not having the setting: it
answers the user's request with a partial no.

And the spinner's own timing was an accident. `spinner(chat.tick)` indexed the 8-glyph star cycle by
the raw frame counter, so at TICK_MS=33 a glyph lasted 33ms and a full cycle 264ms — about 3.6×
Claude Code's rate. Nobody chose 33ms; it is what you get when the animation's clock *is* the render
loop's clock. That is the shape of the whole problem, so the fix is structural rather than a set of
new effects: **`src/tui/motion.rs` owns the frame interval, and nothing animated may read the tick
again.** Every moving surface asks a `Motion` — one bool, built once from settings, copied to render
sites — for a token, and each token converts ticks to milliseconds through `TICK_MS` so a cadence is
stated in the unit it was designed in. A grep for tick reads afterwards leaves only the increment
itself, the elapsed-time stopwatches (`duration_ms`, which now multiply by `motion::TICK_MS` instead
of a literal 33), the 15-tick registry poll, and the two call sites that pass the tick *into* a
token. There is one deliberate survivor: `token_rate.rs` picks its glyph from a wall clock on
per-band cadences that v1.37 specified, so it keeps its own frame math and now takes only the gate
from `Motion` — the drift that mattered was two copies of the gate, not two clocks.

**The seven tokens.** `pulse` advances one glyph per 120ms through the unchanged sequence. `beam` is
a 6-cell glimmer sweeping the running verb and its ellipsis once per 2s, returning a half-open
character window that the render layer paints in a lighter tint of the same base — the sweep enters
from the left and leaves off the right, so there is a dark beat between passes rather than a
permanent bright spot. `stall` is 3s with no event of any kind reaching the TUI. `settle` is one
120ms window at turn end. `breath` is the banner curve moved here verbatim, stops and all, with its
wave test left where it was. `title_glyph` alternates `✳ ⠂ ✳ ⠐` on 960ms boundaries. `Meter` eases a
jumped number to its target over 300ms, ease-out, and is wired to the status row's `↓ N tokens`.
Every one of them takes the frame number as an argument and reads no clock, which is what makes
animation testable without a timer and keeps the demand-gated loop deterministic.

**The gate rests decoration and not information.** `motion: "off"` freezes the spinner on `✻`, kills
the sweep, stills the banner and the title, and makes numbers snap. It deliberately does *not*
silence `stall` or `settle`: those change a colour in order to say something, and a user who asked
for stillness asked for less movement, not for less to be told. This is the same boundary the
feedback spec already drew for `prefers-reduced-motion` ("the loading indicator itself must not be
removed"), applied one level up — and it is why the two tokens are gate-independent by construction
rather than by a comment asking callers to remember.

**Stall says nothing new, because nothing new is known.** At three seconds the spinner and verb turn
warning-coloured and the glimmer stops. The verb, the elapsed seconds and the Esc hint are
untouched. The blueprint sketch had a second tier at 6s adding `(stalled?)` to the copy; that is a
claim about a cause the TUI cannot see — an endpoint thinking hard and an endpoint hung look
identical from here — so this batch ships the one tier the colour can honestly carry, and the
question mark stays unwritten. Progress is recorded in `drain_all`, where every stream delta, tool
event and ask already funnels through: one assignment covers the whole surface, and a hook per
event variant would have been five copies of the same fact.

**`settle` versus write-once, which is the interesting one.** The spec asks the completion row's `✻`
to wear the accent for one frame-window and then rest. Printed scrollback rows are final, so a
post-print colour change is impossible; the fallback offered was the live status row's last repaint,
but that row is gone the instant `busy` clears, and painting the completion text there too would
have shown the same line twice for 120ms. The resolution inverts the problem: **do not print the row
until it has finished blinking.** `settle_at` is stamped at `TurnEnd`, the finished turn's last
message is held out of the settled prefix for the window, and it freezes at rest. Nothing printed
ever changes — the discipline is strengthened, not bent: a row whose colour is still moving is by
definition not final. The cost is honest and visible: six existing tests asserted "everything
settles after the turn ends" *immediately* after `TurnEnd`, and they now tick past the window first
through a shared `past_settle` helper, which is exactly what the host does 120ms later.

**The running verb is now pinned per turn.** It was `thinking_stage(self.messages.len())`, sampled
afresh at each of the three sites that open a reasoning segment. In practice the message count
rarely changed mid-turn so it usually held, but "usually" is not a property: a turn that pushed a
message could change its mind about what it was doing. The verb is sampled once at `TurnStart` and
reused. The tables themselves are unchanged — Claude Code carries ~180 words, bingo carries 12
running and 8 completion, and a curated dozen that all read like the same voice is better than 180
that do not. Expanding them is a copy decision, not a motion one.

The title animation reuses D79's machinery entirely: `Title::Busy` carries the glyph, `set_title`
already drops an unchanged title, so a busy turn costs about one `OSC 2` write per second and a
`notifications: off` session still emits nothing. A pending permission prompt outranks the animation
— the waiting title is never animated over, because it is the more urgent thing to say.

### D88. Every conversation is the same shape, and the hub is the first one

bingo has three kinds of conversation and three unrelated implementations of "conversation". The hub
is `Vec<UiMessage>` rendered by `build_rows` straight into scrollback. A DM or a channel is a
`Conv`, sampled per frame into a `Snapshot` of `ChannelItem`/`DmItem`, rendered by `message_rows`
inside a modal that owns its own composer, its own key map and its own palette. The team has no
conversation at all: `/team` prints strings into the hub transcript, spawn/done/ack go to the watch
registry and are then forgotten. Phase 4 retires the modal and puts all of them on one surface, so
the first thing it needs is one shape. This batch builds it and moves nothing: **`src/tui/buffer.rs`
is the engine, the hub is buffer 0, and the screen is bit-identical** — the 1255 tests that passed
at D87 pass unchanged, and the 25 new ones are all the batch's own.

**A buffer holds what is *about* a conversation, never what is *in* one.** Id, how far the source
has got, how far you have read, whether it wants you, the draft you left in it, when it last moved.
`BufferId::source()` names the store the transcript actually lives in — `ChannelLog`,
`AgentHistory`, `TeamLog`, `HubFlow` — and that naming is the point: it is a key, so there is no
second copy of any message to fall out of step with the first. The registry's ordering is the
derived `Ord` on the enum (hub, `#team`, channels by name, DMs by name), which means the order is
the declaration and there is no comparator to keep in agreement with it.

**Unread is a subtraction, not a counter — the batch spec asked for event tees and the code was
already right.** The workspace has never incremented anything: `entity.rs::snapshot` recomputes
`seq - read_cursor` on every frame, against `ChannelStatus.seq` for a room and `history.len()` for a
DM. The engine keeps that and gains its robustness for free: a counter fed by an event stream can
drift from the thing it counts (a dropped event, a double delivery, a lagging broadcast receiver),
and a cursor read from the source cannot. So "shadow accounting" is a *poll*, teed into
`refresh_entities` — which already reads both registries on the same 15-tick gate D87 named. One
read, one clock, no second timer to disagree with the first. Two rules come with it. A conversation
seen for the first time starts read, because opening bingo on an hour-old session should not badge
every turn that already happened; that is the workspace's rule, kept for the workspace's reason. And
a cursor is clamped to the sequence, because a source can *shrink*: compacting a subagent rewrites
its history shorter, and a cursor parked past the end would read as "nothing new" for the rest of
the session.

**Mention is per-source, because "addressed to you" means different things in a room and a DM.** A
DM is addressed to you by construction — there is no other kind of message in one — so anything new
in it is a mention. A channel is a room, and chatter in a room is not a summons: it wants you only
when an unread post contains `@user`. The log is only read when the subtraction says something is
unread, so the common case costs nothing.

**The board is the one buffer that had to store something, and that is a fact about the domain.**
Every other source already keeps its own transcript. Lifecycle events do not: they are broadcast
through `WatchRegistry` and retained nowhere, so a board bound to "the lifecycle stream" has nothing
to bind *to*. `Buffers` therefore keeps a bounded log of its own (200 events, oldest dropped) fed
from the `UiEvent::WatchEvent` arm — and only from `WatchKind::Agent`, because a channel event
belongs to that channel's buffer and a command event is the hub's own tool, and neither is team
news. The board also materializes on its first event rather than standing empty in a session that
never spawned anyone, and that first event counts as unread: "first sight is read" exists to avoid
badging history that predates you, and a board born a moment ago has none.

**Rehydrate produces `UiMessage`, because the replay path the spec told it to reuse does not
exist.** `/resume` looks like a replay and is not: it clears `self.messages`, prints one line, and
uses the loaded history only for a count and a token estimate. Nothing anywhere converts a stored
`api::types::Message` into a transcript row. So the instruction "do not write a second renderer" was
honoured one layer down instead: `rehydrate` extracts through `slack::dm_posts` and
`slack::channel_posts` — which is where the D64 `[DM from user]` stripping and the batched-message
splitting already live — and emits `UiMessage`, the unit `build_rows` already consumes. A test pins
that equivalence by rendering the same history both ways. The consequence is a claim on D89: those
two functions are the part of `slack.rs` worth keeping, and the `Snapshot`/`Workspace`/`Switcher`
shell is the part that dies. One honest shortfall: `budget` counts messages, not rows. Rows exist
only after a layout at a known width, which is the host's business; a parameter that said rows and
meant messages would have been worse than one that says what it is.

**Routing is data, and the marker stays where it was.** `route_submit` maps an id to a
`SubmitTarget`; `deliver` performs it with the same two calls the workspace composer makes today, in
the same order. The DM target carries `from = user` and *nothing else* — the `[DM from user]` line
is added downstream in `absorb_inbox`, derived from that name, and adding it here too would double
it. A test asserts the full round trip at the domain level: route, deliver, drain the inbox, absorb,
and read the marker off the prompt the instance would actually see. `#team` returns a typed refusal;
it is a record of what happened, not a room to speak in.

**Persistence: none, deliberately.** Unread marks and drafts are session-local and in memory. They
describe your attention in this sitting, and a badge restored from disk would be a claim about a
conversation you may have read in another window. Buffers rebuild from the domain on the next start,
which is the same reason the workspace never persisted its cursors either.

Nothing in this batch renders, so `feedback-states.md` is unchanged and the guide and READMEs are
untouched: there is no new state to document because there is no new state on screen. `chat.rs`
gained seven lines (3912 → 3919) and needed no extraction pre-step. The module carries one
`#![cfg_attr(not(test), allow(dead_code))]`, because an engine complete before its caller is exactly
what a foundation batch produces — **D89 deletes that line**, and anything still unused afterwards
is genuinely dead.

### D89. The workspace retires: a conversation is entered, not opened in another screen

D88 built the shape and moved nothing. This batch is the move, and it is a retirement: the
alternate-screen workspace modal (`src/tui/entity.rs`, 835 lines) and the Slack skin it wore
(`src/tui/slack.rs`, 2680) are gone, along with the test-only preview harness that shot frames of it
(`src/tui/slack_preview.rs`, 439). What replaces them is not a smaller modal. It is the absence of
one: **one terminal, one write-once flow, one active conversation**, with the hub as an ordinary
member of the set rather than the place the real application lives.

The modal was a second application reached with one key. It had its own composer, its own key map,
its own row builder, its own palette and its own quick switcher — which meant that everything the
program had learned in D76–D87 stopped at its border. The approval dialog, the steer queue, the
readline and kill ring, the `$EDITOR` round trip, the motion tokens, the transcript view: all hub
only. Every one of those is now available in a DM because a DM is not a different screen.

**The hard part was not deletion, it was the flow.** `Chat::messages` is simultaneously the hub's
transcript store and the list `build_rows` prints. Left alone, a hub turn completing while the user
reads a DM appends to that list and prints into the DM — precisely the interleaving the design
forbids. Three options presented themselves: buffer rows for inactive conversations (a holding pen,
explicitly ruled out), give each conversation its own renderer (a second renderer, ruled out), or
make the printed flow a *projection* of the one store. The third is what shipped.
`Chat::flow_order` walks the store and returns the print order: hub messages up to the point where
the user left, then that conversation's rows, and — while it is still open — nothing after them, so
the hub's tail sits unprinted in the hub's own store until the return prints it under a `── hub ──`
rule. Two properties make this the cheap answer rather than the clever one. It is **append-only**:
an emitted position never moves, so `flushed_segments` keeps meaning what it meant and scrollback is
never rewritten. And an excursion's rows are `UiMessage`s in the same list the hub's are, indexed the
same way, so `assistant_el(i, …)` renders a replayed DM message with the code that renders a live hub
reply — **there is no second renderer, and no imitation to keep in step**. A conversation's
decoration (the rule, the speaker's name) is a property of the *flow position*, not of the message,
which is why `UiMessage` gained no field and its nineteen construction sites went untouched.

**Symmetry is the rule, including for the hub.** Switching stashes the outgoing draft and restores
the incoming one, prints the rule, replays the last 30 messages, and makes that conversation the only
one that prints live. Returning to the hub is the same operation. One deviation, recorded because it
is a deviation: the hub's replay is **its unprinted tail rather than a cloned budget-bounded tail**.
The hub's store *is* the flow, so its tail has never been printed and printing it is the rehydration;
cloning the last thirty on top would print a third copy of rows still physically on the same screen,
a couple of hundred milliseconds of scrolling away. The budget is meaningful exactly for the
conversations whose store is somewhere else, and that is where it is applied.

**Cadence.** D88's accounting rides the fifteen-tick registry poll, which is right for a presence
strip and wrong for a message you are waiting on. The active conversation is polled **every** tick
instead — one conversation's worth of work, and it is the one on screen — which is also what removes
the window in which a landing message would have shown twice, once in the live tail and once in the
flow. The retired modal did this much work per *frame*, for every conversation.

**Esc is navigation before interruption.** `EscLayer::BackToHub` sits directly above `Interrupt` in
D80's stack: transient layers over a conversation still peel first, then the conversation returns
home, and only at the hub does Esc reach the turn. A turn running behind a DM survives the press, and
the status row stops promising otherwise (`esc to hub`). Ctrl+C keeps the unconditional interrupt.
The reasoning is the same one D80 used to put the interrupt *inside* the list rather than above it:
Esc acts on the thing the user is looking at.

**What the composer does** is decided before `busy` is consulted, because `busy` is the hub's state.
In a conversation the text is delivered, not queued and not steered — D83 offers the running turn the
hub's submissions only, which is now pinned by a test rather than true by accident. Slash commands
fall through to the unchanged path from every conversation, so `/model` in a DM is still `/model`.

**Access, interim.** `/open <@agent|#channel|#team|hub>` with argument completion from D85's registry
(candidates are the buffer registry itself, so an offered name is a conversation that exists), Enter
on an agent in the ctrl+b manager, and Esc home. The conversation bar, `ctrl+k`, `@`/`#` line-leading
routing and the `#team` board's own rendering are D90 and were deliberately not built here.

**Two further deviations from the brief, both for the same reason — no second renderer.** The brief
said to leave `ctrl+o` showing the hub session alone. It shows the flow instead, excursions and
their rules included, because D82 built the pager on `build_rows` precisely so it could never
disagree with the screen, and `build_rows` now prints the flow; filtering the conversations back out
would mean a second row builder in exchange for showing the reader less than their terminal actually
printed. Which conversation the pager should be scoped to is a real question and belongs beside
D90's bar, where there is a way to say "this one"; the behaviour is pinned by a test rather than
left accidental. Second, **portraits did not follow DM and channel rows into the flow**. The gutter
that carried them was the modal's message-list layout, and rebuilding it here would have been new
avatar machinery, which the brief rules out. The plumbing survives where it was already used — the
`experimental.chatAvatars` band and the watch row — and what a DM or channel row gets instead is the
sender's name heading each run of messages, which is the part that was load-bearing: with more than
two speakers in a room the name is not decoration.

**Accepted costs, named.** Repeated excursions leave a conversation in scrollback more than once —
write-once is what makes that unavoidable and the rules are what make it legible. Two capabilities
died with the modal rather than being quietly carried: the per-instance
context-usage meter, which only its composer footer displayed (`AgentRegistry::context_usage`, its
setter and the field behind it are deleted; the `on_context_usage` hook stays wired so a later
surface needs no re-plumbing), and `ChannelRegistry::seen_of`, whose only reader was the sidebar
badge that D88's own accounting replaced. The instance's live token rate did *not* die — it moved to
the `⠙ name is replying…` row, which is the same fact at the same moment.

**D88's `#![cfg_attr(not(test), allow(dead_code))]` is deleted, as it said it would be.** Living up
to the rest of that sentence — "anything still unused afterwards is genuinely dead" — cost `Source`
and `BufferId::source()`, which named where a transcript lives but which nothing consults now that
every store is reached through `rehydrate`, plus four accessors D90 can re-add as three-line getters
when its bar reads them. In their place `BufferId::rule()` earned its keep: the replay and the
hand-back to the hub both format the divider through it, so the two can never drift.

`slack.rs`'s survivors moved rather than died. `Post`/`PostKind`/`channel_posts`/`dm_posts` and their
private helpers went to `buffer.rs`, beside the engine that was already their only non-modal caller —
D64's `[DM from user]` rules, the scaffolding collapse and the live-turn tail travel with them
untouched. `Palette`, `sender_band`, `gutter_cell` and the chip fallback went to `avatar.rs`, which
is what they are; `stamp` went to `buffer.rs`, because a send time is a conversation's. Net: 1979
lines added against 4206 removed — **−2227** — across 21 files, one of them new (`bufferview.rs`,
1056 lines, half of it tests); 34 tests deleted with the code they tested and 19 added, for
1266 + 13 green; and both hosts render every conversation through the one builder they already
shared, because there was never a second one to make agree.

### D90. Conversation chrome: a bar, a switcher, and a way to speak without moving

**Problem.** D88 gave every conversation one shape and D89 made it reachable, but nothing on
screen ever said what existed. `/open` could enter a conversation and never named one, so a DM
filling up behind you was invisible until you thought to ask; the only roster was the ctrl+b
manager, which lists running agents rather than conversations. Three more gaps came with it:
`/team` printed the formation's own report into the hub's info tier — everywhere except the one
buffer that exists to hold it, where the board sat empty; the board's rows showed the detail an
event carried and dropped the lifecycle word, so a finished run and a running one were told apart
only by what the agent happened to say; and nothing on the composer said which conversation the
next Enter would reach. The blueprint's D90 also wanted `ctrl+k`, which D86 had already spent on
kill-to-end-of-line.

**Decision.**

1. **The bar** (`convbar.rs`, new) is one row directly on the composer, in `BufferId` order:
   `hub  #team  #build (3)  ●@scout (2)  ○@zoe`. Presence (`●` running / `○` idle) is a DM's fact
   and nobody else's — a channel has no pulse, so it gets no glyph rather than one that means
   nothing — and it comes from the agent state the ctrl+b manager already reads, in one pass
   rather than a registry lookup per entry. Unread is D88's derived `seq − read`; a conversation
   that named you gets the accent instead of the plain unread colour, which is free because the
   active conversation never carries a badge at all. **The bar renders only when there is more
   than one conversation**: a session that never spawned an agent pays nothing, and a bar reading
   `hub` alone would be a row spent saying that the only thing on screen is the thing on screen.
   Overflow elides to `…` around a run grown outward from the active entry (forward first, so the
   registry still reads left to right) — a pure function of widths, active index and budget, so
   there is no scroll state to get stuck in and nothing to animate. Below the width of one entry
   the active entry survives clamped and unmarked, because a chrome row that wrapped would throw
   off every height the frame assembler measured.
2. **`ctrl+k` is the switcher** (`switcher.rs`, new), an EscLayer in the Menu stratum. Ordered by
   **recency** with the hub pinned on top — the bar answers "what exists" and must not move under
   a glance, while a reader reaching for ctrl+k is asking "what just happened" — filtered through
   D85's single scorer, `↑/↓` clamping like the manager's list, `Enter` switching through D89's
   own `switch_to`, `Esc` peeling only the switcher. **The ctrl+b manager stays.** The blueprint
   allows the switcher to absorb it eventually; this batch does not, because the two answer
   different questions (which conversation am I in, versus what are my agents doing) and the
   manager carries per-agent stats and prompts this list has no room for. What must not fork is
   the stop action, so `ctrl+x` calls the manager's own `stop_agent_from_manager` — same warning,
   same watch transition, and the same absence of a confirmation step. A stopped agent's
   conversation stays listed, idle: it is still worth reading back.
3. **`alt+k` takes the kill.** `ctrl+k` no longer edits text in any state. Same kill, same ring,
   same `KillDir::Forward` as `alt+d`, so consecutive forward kills still coalesce in text order.
4. **Line-leading routing, in the hub only.** A hub submit opening with a known conversation's
   name delivers the rest there, does not switch and does not start a turn — placed beside D89's
   non-hub branch and above the busy branch for the same reason: a delivery must neither queue
   behind a running turn nor start one. The flow keeps a dim `→ @scout: …` receipt, a state line
   like the interrupt marker and the dialog receipts — no `❯` bubble putting the envelope in the
   user's mouth, no send stamp, nothing in the model's history. **An unresolved name is prose**,
   not an error: `@nobody hi` opens an ordinary turn, verbatim. **In a conversation there is no
   such rule**, and the asymmetry is the point: the buffer already *is* the target, so a leading
   `@name` there is a person being addressed inside a sentence, and reading it as an envelope
   would silently redirect a message meant for whoever you were talking to.
5. **The `#team` board renders and receives.** `TeamEvent::state` becomes `Option<WatchState>`:
   `Some` is a lifecycle event and renders as `state · detail`, `None` is output posted to the
   board, which has no transition to name and must not claim one. `/team` writes there and the
   board's own unread carries it; when the board is not what you are looking at, one info line
   says `→ #team`, so the command is never apparently silent. The board stays read-only.
6. **Teammate tinting.** While a DM is active the composer border, the `❯` glyph and the
   `is replying…` row take that agent's colour from the palette the avatar machinery already
   assigns the name — no new colours, both themes covered by the palette's own branch. Bash mode
   still wins the border: what a surface *does* outranks who is on the other end of it.

**Deviations, and why.** (a) The brief asked for bare `x` to stop an agent in the switcher. The
switcher filters as you type, so `x` would have to be either a letter or an irreversible kill;
it is `ctrl+x`, and the overlay's own hint row names it. (b) A running hub turn used to dim the
composer's prompt unconditionally; in a DM it no longer does, because a DM submit is a delivery
rather than a turn (D89) and the dim promised a wait that was not happening. (c) `alt+↑/↓` and
`alt+1..9` positional switching, which the blueprint lists under D90, are not in this batch — the
brief's scope did not include them and unrequested keybindings are cheap to add later and
expensive to take back. (d) The board reuses the conversation row builder rather than getting a
bespoke dim renderer, because D89's ruling is that there is no second renderer.

**Consequences.** `chat_tail.rs` was at 3838 of the 4000-line cap before this batch, so the
provider and think pickers moved to `chat_menus.rs` first as a mechanical no-behavior-change
commit (639 lines; only the two key handlers widened to `pub(super)`). Two new modules, both
carrying their own tests. `ctrl+k` changes meaning for anyone with the muscle memory, which is
the one user-visible regression here and is why the `?` panel, both READMEs and the guide all
name `alt+k` beside the kills. The receipt predicate matches `→ ` followed by a sigil, so a line
of prose in exactly that shape would render as a state line — the same tradeoff `is_ask_receipt`
already accepts. 1266 + 13 tests before, 1295 + 13 after.

### D91. Rewind: a turn you can take back

**Problem.** bingo had no checkpoint, no undo, and no fork. A turn that edited the wrong files
left the user reverting them by hand — with no record of which files it had touched — and the
conversation that produced the wrong turn stayed in history forever, priming every turn after it.
The audit filed this as a P0 against Claude Code's Rewind (esc-esc → pick a past user message →
choose what to restore), and D80 had already left the slot open: Esc on an idle empty composer
did nothing, "reserved for rewind, D91".

**Decision.**

*Identity.* A checkpoint is a turn-opening user message, addressed by **the transcript line it was
written on**. `Message` is `{role, content}` and carries no id, so position is the only identity
history offers; it is a stable one, because the transcript is append-only and rewind is the single
operation that ever shortens it. Turn-openness is **recorded, not inferred**: `record_turn_open`
appends a `{"type":"turn","at":…}` marker line before the user message, in the same idiom as D74's
compact marker, and every projection skips it — so it changes no request bytes and no compact
`kept` accounting. The alternative was sniffing message text, and the harness's own user-role
injections (task reminders, `<task-notifications>`, `<channel-messages>`, the max-tokens resume
prompt, Stop-hook blocks, D76 interrupt markers, D83 steered blocks) are indistinguishable from a
real turn by content. The cost is that sessions recorded before this batch offer no rewind points,
which is a better failure than offering the wrong ones.

*Truncation.* `Transcript::truncate_at_line` copies the surviving prefix **byte for byte** — never
re-serializing — so the request prefix the provider has cached is the one it gets back, and the
replacement is atomic (temp + rename). Exactly one line can ever be new. The compaction marker's
meaning is positional (`kept` counts physical message lines *before* it), so a cut that lands in
its kept tail takes the marker with it, and the same summary is re-emitted with `kept` narrowed to
the part of its window that survived; without that, a cut into the tail would resurrect verbatim
the messages the summary already stands for. A cut inside a span the summary covers is refused
outright — the projection never offers one, since a folded message is not in the projection at
all, and that is the whole guarantee that rewind cannot cut across a compact boundary. Cutting at
a turn-opening user message keeps every `tool_use`/`tool_result` pair intact by construction, the
invariant `safe_split` and `project`'s orphan-skip loop exist to protect.

*The store.* Pre-images live under `~/.local/share/bingo/rewind/<session>/<checkpoint>/`, one
directory per checkpoint, `<hash>.pre` beside `<hash>.path`. Bytes are written first and the
`.path` sidecar second, which makes the sidecar the commit: a crash between them leaves an orphan
`.pre` that no restore reads, never a file wrongly marked as created-by-a-tool. The sidecar is
also the once-per-`(checkpoint, path)` record — the *first* pre-image of a turn is the state the
turn began in, and a second edit of the same file must not overwrite it. Restores replay
checkpoints **newest first** so that where two turns edited one file the oldest pre-image is
written last and wins. Bounded at 50 MB / 200 checkpoints per session, evicted oldest-first at
checkpoint open; eviction names its own files and then `remove_dir`s, never a recursive delete.
Git is not involved: a repository may not exist, and the working tree is not ours to commit.

*Wiring.* The recorder hangs off `Runtime` (so no `Session` literal changed) and is handed to
every tool through `ToolContext`, which is already built once per turn — so the TUI, `--print` and
the JSON host record identically even though only the TUI can rewind this batch. `Edit` and
`Write` are the only tools that write user files (there is no MultiEdit and no NotebookEdit here);
each snapshots immediately before it changes anything.

**Consequences.**

- `chat.rs` was at 3960 of the 4000-line cap, so `/rename`, `/resume`, `/gc`, `/share` and the
  resume picker moved to `chat_session.rs` first as a mechanical no-behavior-change commit
  (386 lines; the four command entry points widened to `pub(super)`, `ResumeMenu` re-exported).
- **Snapshot failures are disclosed at the decision, not as a toast.** The spec asked for
  warn-tier, and tool code has no warning channel: `on_warning` lives on `UiHooks`, which is
  deliberately not passed to tools, and converting it to a shareable `Arc` would have rippled
  through `assemble_tools` and compaction's notify plumbing — unrelated debt, in a batch whose
  load-bearing parts are elsewhere. A failure is instead recorded as a `.miss` sidecar and shown
  in the selector as `3 files (+1 unsnapshotted)`, where the user is choosing whether to rely on
  it. The edit always goes ahead either way, which was the requirement that mattered.
- **Summarize from here shipped rather than being disabled.** It cuts *before* the chosen turn and
  appends `(summary of the turns rewound from here)` after the previous message — deliberately not
  compaction's `(summary of the earlier conversation, from automatic compaction)`, which is a byte
  contract about the *prefix* of a request reproduced identically by the in-memory splice and by
  every projection. Borrowing it for a tail would make a reload unable to tell the two apart. The
  model call reuses compaction's prompt and budget via a new `compact::summarize_slice`, factored
  out of `compact()` with no behavior change; the transcript surgery is `rewind::write_summary`,
  synchronous and directly tested, so the async wrapper is thin.
- **The ctrl+o transcript view does not shrink after a conversation restore** — verified, not
  assumed: it is built from `chat.messages`, what this terminal printed, not from the session file.
  Write-once scrollback makes the rows above the terminal's property anyway. What the model sees
  and what a reload replays are truncated exactly; what the screen remembers is what happened.
- Out of scope by construction and documented: `Bash` mutations (no pre-image is takeable), and
  directories `Write` created on the way to a file (deleting a directory we may not have made is a
  larger wrong than leaving an empty one).
- `EscLayer::ORDER` grows to 16. `esc_at` is shared with `ClearInput`, so arming on a non-empty
  draft and then emptying it within the second lets the next Esc open rewind — harmless, and
  cheaper than a second timer.
- 1295 + 13 tests before, 1327 + 13 after (32 new: 18 store/truncation/summary, 13 selector,
  1 Esc-stack walk).

### D92. The dark theme grows up: a palette of our own, coloured code, numbered diffs

**Problem.** Four faults, one root: the display layer had been finished everywhere except where
colour lives.

1. `Theme::dark()` spelled **21 of its slots as named ANSI** (`Cyan`, `DarkGray`, `Gray`, `Yellow`,
   `Blue`, `Reset`, …) while `Theme::light()` was fully RGB. A named colour is the *terminal's*
   palette, not ours: every markdown heading, list bullet, quote bar, link, table rule, thinking
   block, tool-output gutter and diff line moved with whatever scheme the user had loaded — and
   `to_ansi256` passes non-RGB through untouched, so the downgrade path could not save them
   either. Most terminal users run dark, so the strictly worse half was the one almost everybody
   saw.
2. `markdown.rs`'s fenced-code arm opened with `let _ = lang;`. Every code block in every reply
   rendered in one grey.
3. Diffs had no line numbers, on any surface.
4. The dim vocabulary was two slots and a habit: `inactive` at 75 call sites, and `subtle` — which
   had **no accessor and zero readers in the whole repository**. "Dim" was a look, not a tier.

**Decision.**

*The palette is ours.* Both presets are RGB, end to end, and `both_presets_are_fully_rgb` asserts
it over the struct so a new slot cannot quietly opt out. The enumeration that makes that possible
is `Theme::slots_mut`, one list of `(name, &mut Color)` that `downgrade_to_256` now walks instead
of the 35 hand-copied assignments it used to be — a field missing from the list is a field that
opts out of *both* the downgrade and the test, which is the kind of omission that announces
itself. The dark scheme is built around the brand orange on the terminal's own near-black, with a
warm neutral ladder and desaturated structural tokens; the light preset, already RGB, moved
exactly one slot.

| Slot | Dark | Was | Why |
|---|---|---|---|
| `text` | `#EBE7E2` | `#FFFFFF` | warm off-white; pure white on near-black is a glare, not a contrast |
| `text_secondary` | `#9A948D` | `#999999` (`inactive`) | same rung, warmed to the ladder |
| `text_muted` | `#6B6660` | `#505050` (`subtle`) | the tier now carries text; 2.2:1 was not a tier, it was a hairline |
| `claude` family | unchanged | — | the brand; nothing in this batch earns the right to move it |
| `code_fg` | `#D9A05B` | `Yellow` | inline code as a warm sibling of the accent |
| `link`, `headings[2]` | `#7FA7D9` | `Blue` | one blue in the palette, not three |
| `math` | `#C08AD1` | `Magenta` | muted mauve, distinct from link and accent |
| `headings[0]`, `table_header` | `#EBE7E2` | `White`, `Reset` | h1 and a table header *are* primary text, plus bold |
| `headings[1]`, `tool_running`, `diff_hunk` | `#7FBFB4` | `Cyan` | one teal, in the `plan_mode` family |
| `headings[3]`, `quote`, `list_marker`, `tool_output`, `diff_context` | `#9A948D` | `DarkGray`/`Gray`/`Cyan` | all tier 2 by meaning, so all tier 2 by colour |
| `quote_bar`, `task_open`, `table_border`, `hr`, `footnote`, `thinking` | `#6B6660` | `Blue`/`DarkGray` | furniture, tier 3 |
| `task_done` | `#4EBA65` | `Green` | the `success` value; one green |
| `text_muted` (light) | `#8C8C8C` | `#AFAFAF` | the one light change: 2.3:1 on white is not readable |
| `diff_edit` | *deleted* | `Yellow` | dead since it was written; the blueprint said use it or delete it |

Every dark slot clears 3.1:1 against a `#0D0D0D` ground; the three tiers land at 15.8 / 6.5 / 3.4:1,
and light mirrors them at 21 / 5.7 / 3.4:1.

*Three tiers, named and employed.* `text` / `text_secondary` / `text_muted`, with `text()` /
`dim()` / `muted()`. `inactive` was renamed rather than aliased — it named a *state* and was used
for a *tier*, and 75 sites of honest name beat one line of indirection. `dim()` keeps its name
because it is the tier-2 accessor at ~110 call sites and "secondary" is exactly what it meant.
`muted()` is new, and the sweep gave it work: the expand hints (`(ctrl+o to expand)`, `… +N
lines`), the send-time stamp under every message, the approval dialog's key hints, the transcript
pager's footer (position, search and key rows), the conversation bar's separators and ellipses,
the `manager_box` frame, the welcome-card border, the `(url)` trailing every link, the footer's
`·` between token rate and context usage, and the new diff gutter. Not swept, and named rather
than quietly skipped: the menu-gated hint rows (`/model`, `/provider`, `/resume`, the `@`
completion dropdown, the ctrl+b agent manager, the rewind list) still sit on tier 2.

*Highlighting: `synoptic`, and two narrowings around it.* Chosen over `syntect` because the
comparison is not close — synoptic is one 1,900-line pure-Rust file over four small crates
(`char_index`, `if_chain`, `nohash-hasher`, and the `regex` we already depend on: **four new lock
entries**), ships grammars for every language the batch required, and offers `run`/`append` line
APIs that fit a renderer. `syntect` with `regex-fancy` still drags in `plist`, `bincode`,
`yaml-rust`, `walkdir` and either megabytes of Sublime grammar dumps or runtime `.sublime-syntax`
parsing — for a feature whose entire job is to tint a code fence. The two narrowings live in
`src/tui/highlight.rs`: `from_extension` answers `Some` for *every* string (an unknown extension
yields a highlighter that tokenizes nothing), so `language_for` is an explicit allowlist and an
unrecognised fence stays monochrome — **guessing is worse than not colouring**; and synoptic's ~30
grammar-varying token names fold into eight `Class`es whose colours are existing theme tokens.
That last choice is the one that pays: the dark and light highlight palettes are not two more
tables to maintain, they are whatever the two presets already say `text_muted`, `success`,
`claude`, `math`, `link`, `tool_running`, `code_fg`, `text_secondary` and `code_block_fg` are, so
`/theme` moves them for free and a test asserts no class collides with the code background or with
another class in either preset.

*The cost seam is the memo, not the settled-line rule.* Rows are built once for scrollback (the
hub's `MarkdownRenderer` caches per block, keyed on block source), but the DM tail in
`bufferview.rs` builds a fresh renderer every frame on purpose. A "highlight only settled lines"
rule would have needed the renderer to know whether a fence was still open, which the AST does not
say. A bounded memo keyed on `(language, source)` gets the same result at the cheap seam: a block
re-rendered without changing costs a hash lookup, so a live tail pays nothing per frame, and a
block that *is* growing pays one pass per change — the same order as the markdown re-parse it
arrives with. 256 entries, oldest-first eviction, thread-local rather than locked (rows are built
on the UI thread and each test thread gets a clean memo). Blocks past 96 KB or 4,000 lines are
returned unhighlighted rather than allowed to make a frame late. Tabs expand to four spaces in
*both* paths, because synoptic expands before tokenizing and a fallback that passed `\t` through
would put highlighted and monochrome fences on different grids.

*The gutter goes in the one diff builder.* `Hunk` gains `old_start`/`new_start` (parsed from
`@@ -a,b +c,d @@`; an unparsable header falls back to 1/1 rather than refusing to render), and
`Hunk::numbered` walks the arithmetic — context advances both sides, an addition only the new, a
removal only the old. Width comes from the largest number in the **whole diff**, so a hunk crossing
99 → 100 does not shift the code column mid-block. `diff_lines` gained a `width`, which bought the
second half: long lines now **wrap instead of being clipped**, with a blank gutter and a blank
marker on continuations so the code column stays a straight edge. Code wraps on columns, never on
words — a break mid-identifier is honest, a break that reflows indentation is not. The `@@` header
stays flush left: it is a statement *about* the numbers, and indenting it into their column would
say otherwise. Because `diff_lines` has exactly two callers, the gutter reached the approval
preview, the completed-edit rows and the transcript view in one edit;
`both_diff_surfaces_render_the_same_gutter` renders two of them and compares, so a future second
diff renderer fails a test rather than drifting.

**Consequences.**

- `theme.inactive` → `theme.text_secondary` at 75 sites, mechanical; `subtle` → `text_muted`;
  `diff_edit` deleted. `Theme::slots_mut` is now the single palette enumeration.
- `diff_lines(d, theme)` → `diff_lines(d, theme, width)`. The dialog passes the live width and
  re-renders each frame; the transcript path bakes at edit time with `width - RESULT_INDENT`, so a
  **resize does not re-wrap rows already built** — a narrower terminal clips them, exactly as it
  did before D92. Rebuilding on resize would mean either re-rendering baked activity content or
  moving diff rendering into `layout_activity`; both are larger than this batch and neither is
  needed while scrollback is written once.
- `/theme` now re-renders the diff rows still in the live region (`rebuild_diff_rows`), which are
  baked when the edit lands and would otherwise keep the old palette. Rows already in scrollback
  keep theirs — the standing write-once contract, not a gap here.
- One new dependency (`synoptic 2.2`), four new lock entries. `src/tui/highlight.rs` is new;
  the blueprint suggested `src/highlight.rs`, but it reads `Theme` and produces spans for
  `markdown.rs`, so it belongs beside them.
- A stale intra-doc link in `theme.rs` pointing at `crate::tui::slack` (retired in D89) now points
  at `crate::tui::avatar`, which carries the skin palette and comes down the same `to_ansi256`.
- 1327 + 13 tests before, 1354 + 13 after (27 new: 6 palette/tier, 9 highlighter, 5 markdown
  fence, 5 diff gutter, 2 cross-surface).

### D93. Six things a real terminal found

**Problem.** The D76–D92 program shipped and was run, for the first time, on a real device against
real work. Six faults came back — none of them visible from a test suite, and one of them expensive.

1. **Images reached models that cannot read them.** `supports_vision` fed the system prompt and
   nothing else, so pasting a screenshot while a text-only model was active put the whole base64
   block on the wire. A single image is a large slice of a context window spent on bytes the model
   discards. Worse, the only gate that existed (`user_message_with_images`, on the *endpoint*-wide
   `supportsImages`) covered the input box alone: a screenshot a tool read off disk bypassed it
   entirely.
2. **A buffer switch left the viewport behind.** The rule and replay a switch prints land at the
   end of the document, and the fullscreen host stayed where it was — the user had to scroll down
   to find the conversation they had just opened.
3. **Three surfaces for one agent.** The pre-blueprint presence strip above the composer (D80's
   reduced `entity_rows`), the D90 conversation bar, and a `#team` entry nobody asked for — D88
   materialized the board on the first agent lifecycle event, so hiring one subagent for one task
   lit all three.
4. **A clock under every message.** The issue-41 stamps rendered as standalone rows: one terminal
   line per message spent on five characters, and down a transcript a column of times reading
   louder than the words between them.
5. **The D87 glimmer was invisible.** Six cells painted one step along the accent ramp
   (`claude → claude_strong`, ~18/255 per channel). On a dark terminal there was nothing to see.
6. **`$EDITOR` lost an edit** — reported as 改完没回填, "edited, not filled back".

**Decision.**

*Vision is a wire gate, not just a prompt fact.* The projection sits in `Client::stream` /
`Client::complete_text` — the one seam every request funnels through — and not at the point history
is built. Same philosophy as the D74 cache markers: **the history is the record and the request is a
view of it.** For a model without vision, every `ContentBlock::Image` and every `{"type":"image"}`
inside a tool result's untyped `content` array becomes
`[image omitted: <model-id> has no vision]`. Two shapes are matched because images reach a request
by two roads, and missing the second is what let a `Read` of a screenshot cost a window. Being at
the seam means the turn loop, retries and the compaction summarizer inherit it without knowing it
exists. `count_tokens` projects with the same function, because the contract says the count must
measure the payload it predicts. `Cow` return: no image, no clone. The session file and the
in-memory history are untouched, so switching to a model that can see shows the image again.

*A rebuild reconciles against the document it just built.* `rebuild()` ran `reconcile_scroll` and
then `build_rows`, so `max_scroll` was computed from the previous frame's document — any batch of
rows arriving in one frame landed below the fold **even for a viewer who had never scrolled**. That
is the root cause, and it is more general than the switch that exposed it; the two calls are now in
the other order. `switch_to` additionally re-arms `auto_scroll`, which brings along a reader who
*had* scrolled up: opening a conversation puts you at its tail, the way opening a chat anywhere does.

*The bar is the presence strip's successor, so the strip goes.* `entity_rows`, the `entities`
snapshot and `EntityRow` are deleted. `refresh_entities` is renamed `refresh_conversations`, which
is all it still does — it existed to snapshot the strip and happened to also poll the conversation
registry — and it takes its dirty signal from `bar_entries()`, so there is one answer to "did
anything change" rather than two that can disagree. `AgentStatus::thinking` went with it: the strip
was its only reader.

*The board is a team's board.* `Buffers::refresh` lists `#team` when the agent registry holds an
`AgentKind::Crew` — the discriminator the domain already keeps, and one a `Task`-spawned hire can
never satisfy (`tool/agent.rs` hardcodes `Hire`). `note_watch_event` and `note_team_output` still
fill the bounded log unconditionally and no longer materialize the buffer, so a crew formed later
opens onto the history it missed rather than onto a board that starts when it was listed.

*The stamp sits beside the message.* One helper, `push_right`, appends a trailer flush to a width
with a minimum gap and reports whether it fit; one caller, `hang_stamp`, hangs the stamp on the
first row of the message's element tree via `El::first_content_line_mut` (blank spacing rows
skipped). Both stamp sites — the user bubble and the assistant reply — go through it, which is why
DMs, channels and the `#team` board changed with them: they were always the same builder. Widths run
through `text_width`, so CJK lands the stamp on the column ASCII does, and a bubble's reserved right
column is subtracted rather than overrun. **Too narrow, and the stamp is the thing that goes** —
nothing is wrapped or truncated to fit a clock.

*The glimmer is eight cells, bold, and derived from the palette.* `beam_color(theme, base)` lerps
the verb's colour 55% toward `theme.text` — the highest-contrast ink a theme owns, so a dark preset
brightens and a light one darkens, both moving *away* from the base rather than one step along the
same ramp. Bold carries it where colour cannot (256-colour terminals). Period and gate unchanged.

*The reported `$EDITOR` cause was wrong, and the real one is worse.* The rename hypothesis — vim's
`backupcopy=auto` writing a sibling and renaming it over the path, read back through a stale inode —
was reproduced first and **does not exist**: `edit_draft` reads by path after the child exits, and a
rename-style save has always round-tripped. There is also no mtime/content classification to
misclassify; `a_saved_edit_is_the_only_silent_outcome` only ever tested `note()`. A test now pins the
rename case so the belief cannot come back. What *does* lose work is an editor that opens its own
window and returns immediately — `code`/`zed`/`subl` without their wait flag. It exits zero before
the user has typed, the file is read back unchanged and removed, and the edit they then save has
nowhere to land. That was classified `Edited(original)` and therefore **silent**. An unchanged file
is now `EditorOutcome::Unchanged` and says so, naming the cure. The draft still stands, and the
non-zero-exit and unset-editor paths are untouched.

**Consequences.**

- `supports_vision` is now a wire gate; the comments in `api/models.rs` and `model_families.rs`
  that said it was prompt-only were wrong and are corrected. `estimate_tokens` in `compact.rs` still
  charges `IMAGE_UNITS` for a block the request may drop — a local over-estimate that errs toward
  compacting early, left as-is rather than duplicating the resolver into the estimator.
- Deleted with the presence strip, and where their coverage went: `entity_area_filters_idle_agents`
  (the strip's whole subject; its running/idle distinction is now `presence_marks_dms_and_only_dms`
  in `convbar.rs`, its channel listing `the_bar_lists_the_registry_in_its_own_order`, and its
  trailing ctrl+g assertion is verbatim `ctrl_g_requests_the_editor_unless_a_dialog_is_up`), and the
  strip paragraph of `running_agents_leave_the_arrows_to_history` (a negative assertion about a hint
  string that no longer renders; the test's real subject — ↑ recalls history, ctrl+b opens the DM —
  is untouched). `list_reports_the_runtime_engine_and_thinking` lost its `thinking` assertion with
  the field and is now `list_reports_the_runtime_engine`.
- Four tests seeded an `AgentKind::Hire` and expected `#team`; they seed a `Crew` now, which is what
  they always meant. `the_board_hears_agents_and_nothing_else` keeps its subject and gains a
  sibling, `a_solo_hire_writes_the_log_without_raising_a_board`.
- `rebuild`'s reordering changes scroll behaviour for *every* batch of rows, not only switches. It
  is the more correct order and the render helper in `chat_tests_a.rs` already used it; the whole
  suite was green without further change.
- 1354 + 13 tests before, 1365 + 13 after (12 new: 2 vision projection, 3 scroll, 1 solo-hire board,
  3 stamp placement, 1 beam, 2 editor round trip; 1 deleted).

### D94. The hub stops being the message bus

**Problem.** Every agent lifecycle event in the session wrote into the user's conversation with the
main agent. `UiEvent::WatchEvent` has one consumer (`chat.rs`), and when it could not find an
existing row for the label it built one and hung it off `stream_msg` — or, with no turn running,
off *the last assistant message it could find*. So a background hire finishing forty seconds after
its turn ended, a continuation run opening under a new label (`scout #3 · …`, a new label every
run), and an ack watchdog reporting a chase all appended `◉ name · task` + `⎿ done` under a reply
that had nothing to do with them. The hub is supposed to be a 1v1 conversation; it was the system's
message bus with the conversation mixed in.

**The inventory**, taken before anything was changed, and where each item went:

| What printed in the hub | Site | Now |
|---|---|---|
| `◉ name · task` + `⎿ done`, manufactured when no row matched the label | `chat.rs` else-branch | Not printed when no turn is running; bar presence + lifecycle log + the agent's DM carry it |
| The same row updated in place (status flip, and the agent's whole final text stuffed into its ctrl+o content) | `chat.rs` found-branch | Unchanged — it is the running turn's own row, created by the turn that called `Agent` |
| One new row **per continuation run** (`flush_agent_inbox` registers `{name} #{run} · {excerpt}`) | `agent.rs` | Gone from the hub with the manufacture branch |
| Ack-retry scaffolding (`waiting for a reply, chased 1/3`, `not delivered: …`, `3 follow-ups and scout still has not replied`) — all `WatchKind::Agent` | `agent.rs` `spawn_ack_watchdog` | Same |
| An auto hub turn on every terminal state (`submit_auto`) | `chat.rs` | **Untouched.** This is how `<task-notifications>` reaches the model; D94 changes what the user sees, not what any model reads |

**Decision.**

*The rule is about **when**, not about **what**.* The obvious change — drop agent watch rows — is
wrong, because `Agent` is a hidden tool (`is_hidden_tool`): it renders no tool row of its own, so
the watch row **is** the row for the Task call the user just watched the model make. Deleting it
would delete the hub turn's only evidence of its own tool use. The discriminator is therefore
`stream_msg`: a turn is running, so this event answers something the user did here, and the row
stays; no turn is running, so this is the bus, and the row is not built. That is the same sentence
the delivery matrix uses — "events that are answers to something the user did in hub within the
current turn" — expressed in the one piece of state that already means it. `Command` and `Channel`
watches keep the old walk-back untouched: a background shell command is the hub's own tool.

*The event still falls through.* Suppressing the row must not suppress `submit_auto`, so the
early-`return` the old code used for "no message to hang a row on" is preserved **only** for
non-agent kinds, and an agent event with nowhere to draw carries on to the terminal-state handling.
The model-side contracts — task notifications, D64 markers, D63 privacy, ack-retry — are byte-identical.

*Nothing was needed to make a completion bump the DM.* This was verified rather than assumed: the
final report already lands in the instance's history (`AgentRegistry::finish`), the DM's sequence is
that history's length (`Buffers::refresh`), and `mention` is hardcoded true for every DM, so the
badge and the `wants you` accent follow for free. The lifecycle event's own registry sweep is what
re-reads it. A test now pins the whole chain end to end.

*`notify_user` is the road that replaces the flood.* One tool, subagents only — the main agent holds
the hub already, so a tool for "reaching the user" there would be a second and worse way to say
something it can simply say. `assemble_tools` gains it in the `else` of the `depth == 0` gate,
which is the first thing that branch has ever been used for. The description spends most of its
words on when *not* to call it, because an agent that narrates progress through it turns the user's
one quiet surface into a log.

*The rate limit lives with the decision, not with the drawing.* `Relay::notify` returns
`delivered` or `queued`, so the model learns the window exists and is told not to resend; the
`queued` copy names where the text still is. One line per agent per 60s; extras are counted and
reported once as `🔔 @name: N more — see the DM` when the window rolls, flushed on the host's tick
so a rolled window pays what it owes even if the agent has gone quiet. `urgent` bypasses coalescing
— a blocker is worth a line whenever it arrives — but not the attention ceiling: the D79 notifier
fires at most once per agent per window, which is why `notifier: bool` is decided in the relay and
merely obeyed by the renderer. **Nothing is lost by counting**: the text was written *as a tool
call*, so it is in that agent's transcript and reaches the user through its DM regardless.

*Time is a parameter.* Every entry point takes `now: Instant`, the `token_rate` pattern, so the
window is driven by tests instead of slept through.

*The relay is session-scoped, like `rewind`.* It lives on `Runtime` and `build_sub_session`
inherits the parent's handle, which is what makes the rate-limit table the session's rather than the
spawn's — an agent restarted in a loop cannot buy itself a fresh window. It also avoided threading a
handle through `spawn_agent_loop`, `flush_agent_inbox` and their nine callers. The sink is
interior-mutable because the session outlives the surface: the session is built first, the channel a
notice travels on only exists once the host builds its screen, so the host `attach`es then.

*The hub gets an unread count for the first time.* It is the one buffer with no domain sequence
behind it — nothing could previously arrive in it that the user had not asked for, so there was
nothing to count. A relay can arrive unasked, which is the entire point of it, so `Buffers` counts
relays and observes the hub against them.

*A relay is a state line that keeps its stamp.* It joins the dim, bubble-less family (interrupt
marker, dialog receipts, the D90 route receipt) because the user did not write it — but it is the
one member that is a real message, sent by someone, at a moment that matters: "the build broke"
reads differently at 09:02 and at 17:40. The other members describe *now* and have nothing to stamp.

**Consequences.**

- A fourth D79 trigger, `Attention::AgentNotice` → `An agent needs you`. It is the first trigger the
  *model* side can pull, which is why the ceiling is at the source rather than here. No name and no
  count, by the same rule as the other three: the line is already in the hub.
- Headless attaches a stderr sink, so a relay is not silent in a pipe (general principle 1). The
  JSON protocol host takes a detached relay and **no new wire event this batch** — inventing a shape
  before a client has asked for one would freeze it; the rate limiting still runs there, so the
  model's contract does not vary by host.
- Two D97-era avatar tests were pinning the row's appearance with no running turn; they now set
  `stream_msg`, which is what a real spawn does. What they test — how the row looks — is unchanged.
- `Buffers::team_log` is `#[cfg(test)]` for now: `rehydrate` is still the only production reader.
  D95's directory is the second and un-gates it.
- **Known limits, named rather than papered over.** (a) `set_state(Done)` broadcasts *before*
  `finish()` stores the history, so a completion's DM badge can wait for the next 15-tick sweep;
  the ordering was left alone because fixing it means restructuring the continuation match with no
  test that can prove the race. (b) `release_hires()` can delete an instance before any sweep
  observes the grown history, freezing that DM's badge — it only fires in a project with a live
  crew, and it is a pre-existing hole rather than one D94 opened. (c) The DM's sequence counts raw
  history entries, not rendered posts, so one report reads as `(2)` rather than `(1)`. (d) The
  coalesce flush rides the tick, so a fully idle loop pays what it owes when it next wakes.
- 1365 + 13 tests before, 1390 + 13 after (25 new: 7 relay arithmetic, 6 tool surface, 11 hub
  routing and rendering, 1 tool registration).

### D95. Rooms as first-class citizens, and the team as a directory

**Problem.** Two things were wearing each other's clothes. A *channel* was a group chat whose
roster the domain silently filled in — `create` seated `main` and `user` no matter what was asked
for — so every room was the user's room by construction, and "an arbitrary subset of the team" was
a sentence in the design that the code contradicted on line one. Meanwhile `#team` was a
*conversation* you could open, that carried an unread badge, that sat in the bar and the switcher
and `/open`, and that you could type into — where it refused politely, because there was nobody to
refuse. The team is the organization. You cannot say anything to it. A read-only buffer with a
badge was a conversation-shaped hole where a roster belonged.

**The channel-domain inventory**, taken at `ebf632e` before anything changed:

| Piece | Where | What D95 did |
|---|---|---|
| `ChannelRegistry` — members, mode, seq, log, seen, sent, frozen, watch id, share sync | `src/channels.rs` | Extended in place. This *is* the room domain; a parallel one was never on the table. |
| `create(name, members, mode)` — force-seated `HUB_NAME` + `USER_NAME` | `src/channels.rs:247` | Seats exactly what it is given. Policy moved to the tool layer, which knows the caller. |
| `invite` / `kick` — roster edits, silent; `kick` refused `user` and `main` | `src/channels.rs:306` | Both write a membership event; only `main` is still irremovable. |
| `post` — stamping, serial staleness, budget gate, hub_mail | `src/channels.rs:368` | Staleness now reads speech only; the non-member error names the cure. |
| `remove_member_everywhere` — called on AgentControl delete | `src/channels.rs:347` | Writes a `left` event per room it touches. |
| `log_of` / `info` / `list` / `row_snapshot` — read surface for the TUI and the watch row | `src/channels.rs` | Unchanged, plus `is_member` and `rooms_of` for the display side. |
| `ChannelTool` (create/invite/kick/list), **hub-only** | `src/tool/channel.rs:225`, `src/tools.rs:79` | Also registered for depth-1 sub-agents; description rewritten to explain rooms. |
| `PostTool` (hub + depth-1), `deliver_post` shared by the tool and the TUI composer | `src/tool/channel.rs:80` | Unchanged mechanically; description says "room". |
| Blueprint rooms `.bingo/team.json` | `src/team.rs:1292` | Names `main` + `user` explicitly, so declared rooms behave exactly as before. |

**Vocabulary mapping.** Domain `channel` → UI/docs **room**; `ChannelRegistry` → the room domain;
`BufferId::Channel(name)` → `#name`; `ChannelMessage{kind: Membership}` → the dim `· name joined ·`
line. The domain keeps its own name deliberately: renaming a persisted share schema, a settings key
(`experimental.agentChannels`), a tool (`Channel`) and a `WatchKind` to say the same word would be
churn with a migration attached. The rule is that nothing a *user or an agent reads* says "channel".

**Membership and its event schema.** `ChannelMessage` gained `kind: MessageKind` (`Said` |
`Membership`), `#[serde(default)]` so a pre-D95 share document still reads as all speech. A
membership entry is `{seq, from: <member>, text: "joined" | "left", at, kind: Membership}` and takes
a real sequence number, because a reader has to be able to tell whether somebody spoke before or
after they arrived. Three things it deliberately is **not**: delivered to anybody's inbox (waking N
agents because a roster changed is the flooding D94 removed — an agent that wants the roster asks
`Channel list`); counted by the serial commit check, which now filters `kind == Said`, so a join can
never bounce a post already being drafted; or the "latest" in a room's watch-row detail, since a
room whose last entry is a join has not gone quiet.

**Membership is what the bar means.** `Buffers::refresh` lists a room while the user is a member and
*removes* it when they are not. That is the one place D89's "never remove a buffer" rule is broken,
and the reason is the distinction the rule was about: a stopped agent's DM is still your
conversation, while a room you are not in was never yours. Rooms with no user are absent from the
bar, the `Ctrl+K` switcher and `/open` — the directory is the only door.

**Observing.** Opening a non-member room needs no new state: the active buffer is simply an id with
no registry entry, and everything else derives from `channels.is_member`. The rule becomes
`── #parser · observer · read-only ──` (via `Buffers::rule_for`, so the framing is a fact about the
conversation rather than about the host), `route_submit` returns `Refused(OBSERVER_HINT)`, and the
same sentence stands under the composer via `Chat::observer_hint` — one wording, so the answer to
"why can't I type here" does not depend on whether you tried. **Esc goes to the hub**, unchanged:
`BackToHub` already means "take me home" from every conversation, and a second meaning ("back to
the directory") would have made Esc's destination depend on how you arrived.

**The directory** (`src/tui/directory.rs`, new) is the second stop of `Ctrl+T`: tasks → team →
closed. Roster with presence and each member's rooms, every room with its members and a
`you're not in` mark, the last ten feed entries newest-first — all rebuilt from live sources every
draw, because the roster is a dozen rows and a cached roster is a roster that can be wrong. ↑/↓ walk
the *selectable* rows only, Enter opens a DM or a room, `j` joins the room under the cursor and
leaves the panel open so the mark flipping is the confirmation. It is modal for bare keys (`j` must
not also type a `j`) and transparent to chords (`Ctrl+T` has to close what it opened).

**Deliberate scoping.** The directory navigates and informs; it does not stop, restart or inspect
agents. Those verbs live in the `Ctrl+B` manager with their warning and their one stop path, which
the `Ctrl+K` switcher already routes through rather than reimplementing. A second surface that could
stop an agent would be a second place for "stop" to mean something slightly different.

**Why `EscLayer::Directory` is its own layer** rather than a second meaning for `TaskPanel`: the two
are one *gesture* but not one surface — different state, different dismissal, both reachable from
the same key — and `ORDER` is the single place that says which one Esc closes. A shared slot would
have had to answer that question somewhere else. It sits immediately above `TaskPanel`, and the walk
test asserts both the adjacency and that `ORDER.len()` grew by exactly one (a variant in the enum
and in both matches but missing from `ORDER` is a layer Esc can never reach — the one thing the
compiler does not catch here).

**Deviations from the dispatch, with reasons.**
- *The join key is `/join` plus `j` in the directory, not a bare `J` in the composer.* The composer
  stays live in observer mode so `/help`, `Ctrl+K` and Esc keep working; a bare letter there would
  have to be either a letter or an action. `ctrl+j` was rejected outright — it is Enter on many
  terminals. `/leave` came along for symmetry and to give the user a way out of a room they joined.
- *The observer frame is the rule plus the composer hint, not a drawn border.* Scrollback is written
  once (D38/D82), so a box around a flow that is still growing cannot be drawn without rewriting it.
- *`PostKind::Note` now renders as a dim line everywhere*, not only for membership. Its own doc had
  always said "one dim line instead of a quoted block with a name over it", and the flow rendered it
  as a named message anyway; `Replay::Note` makes the code match the sentence, and DM wake-up
  scaffolding gets the treatment it was documented to have.
- *The D93 crew gate is deleted rather than moved.* It existed so a solo hire would not raise a
  badge; a column in a panel the user opens has no badge to withhold.

**Deleted tests, and where their claims went.**
| Deleted | Disposition |
|---|---|
| `buffer::the_board_hears_agents_and_nothing_else` | → `the_feed_hears_agents_and_nothing_else_and_raises_no_conversation`: the `WatchKind` filter is kept, the buffer assertions become "no conversation is raised". |
| `buffer::a_solo_hire_writes_the_log_without_raising_a_board` | Subject gone with the crew gate. Its surviving claim (the log fills for a hire) is asserted in the test above. |
| `buffer::the_board_bounds_what_it_remembers` | → `the_feed_bounds_what_it_remembers`, unchanged but reading through `team_log()`. |
| `buffer::the_board_replays_its_lifecycle_log` | → `the_feed_keeps_what_happened_and_what_was_reported` (the `state · detail` shape) + `directory::the_directory_shows_the_roster_the_rooms_and_what_just_happened` (the rows). |
| `buffer::the_board_refuses_to_be_spoken_in` | → `a_room_you_are_watching_refuses_to_be_spoken_in`, which also asserts nothing was posted and that joining flips it. |
| `bufferview::the_board_refuses_to_be_spoken_in` | → `a_room_you_are_not_in_opens_read_only_and_says_why`. |
| `bufferview::the_board_renders_its_lifecycle_log` | → `directory::the_directory_shows_…`; the rows moved, so the test did. |
| `bufferview::team_output_lands_on_the_board_and_says_so` | → `team_output_lands_in_the_feed_and_says_where` (pointer copy + the directory-open case). |
| `#team` arms inside `an_id_names_its_conversation_in_one_vocabulary`, `conversations_materialize_from_the_domain_in_one_order`, `every_conversation_routes_to_its_own_path`, `open_reaches_every_conversation_…`, `convbar::the_bar_lists_the_registry_in_its_own_order` | Arms removed; the enum no longer has the variant. |
| `tools::channel_tools_gated_by_experimental_flag`'s "channel management is hub-only" | Inverted: a direct sub-agent now gets `Channel`. |

**Named limits.** While observing, the bar shows no active entry — the room is by definition not one
of the user's conversations, and inventing a bar slot for it would contradict the rule the bar
states. A membership line carries its clock inside its own text, because the row shape it renders as
(the rule's) has nowhere to hang a right-aligned stamp. Agent↔agent rooms are only as discoverable
as the directory: nothing announces that one was formed, which is the correct default for a room the
user is not in, but it does mean a room can exist for a while before anyone looks.

- 1390 + 13 tests before, 1405 + 13 after (15 new: 3 domain — roster subsets, join-never-stales,
  pre-D95 share compatibility; 3 buffer — membership listing, membership notes, observer rule;
  2 bufferview — observer mode, join/leave round trip; 6 directory — contents, feed order, Enter
  navigation, `j` join, the scoping guard, the empty case; plus the switcher's non-member room and
  three chat-tests for the `EscLayer` slot, the `Ctrl+T` cycle and the directory's key modality).

### D96. The perspective page: every agent is the protagonist of its own record

**Problem.** The model says an agent's communications are a thing you can look at — a grouped,
read-only dossier, one thread per counterpart, with the agent's own work shown inside each. The code
had no way to answer the question that page asks, because the page asks *who said this*, and by the
time anything reaches an agent's history nobody knows. `AgentRegistry::deliver` takes a real `from`
and `InboxItem::Direct` stores it; then `absorb_inbox` renders the batch into **one flat prompt
string** and the name is gone. What survives is a handful of literal markers, and only some of them
name anybody.

**The attribution inventory**, taken at `c4f6fc2` before anything changed. Everything an agent's
`Vec<Message>` can contain is produced by a `record()` call in `src/query.rs` (plus the compaction
splice), and `Entry.history` is replaced wholesale by `AgentRegistry::finish` — there is no
incremental push and no `from` field on a `Message`. So this table is the whole universe:

| Shape in a user-role message | Composed at | Attributed to | Why |
|---|---|---|---|
| `[DM from user]` heading a line | `tool::agent::direct_text` (agent.rs:616) | **the user** | D64's one observable difference between "your manager" and "the human" |
| `[Message from user, sent while you were working]` block | `steer::SteerItem::block_text` | **the user** | a real message from a real person that arrived beside a tool result |
| unmarked prose | `direct_text`, single item | **the hub** | the hub is the one sender `direct_text` deliberately leaves unmarked |
| `[follow-up instruction] …` | `direct_text`, batched | **the hub** | the label is added only when a batch makes boundaries ambiguous; the text beside it is a real instruction |
| `[#{room} msg #{seq}] {from}: {text}` | `absorb_inbox` (agent.rs:666) | **timeline only** | the one marker that kept a sender's name — and the room's own log is the authoritative copy |
| `[follow-up {n}/{m}] …` | `absorb_inbox` | **intake** | a chase; carries no instruction, only the fact that somebody is waiting |
| `[SYSTEM NOTIFICATION - TASK REMINDER]` | `query::maybe_inject_task_reminder` | **intake** | a block, not a line: everything after it belongs to it |
| `<task-notifications>` | `query.rs:988` | **intake** | owner-scoped, so a subagent really does receive these |
| the first user message | the `Agent` tool's prompt | **intake** | unmarked prose, and still not the hub making conversation — it is the task that created the instance |
| `[Request interrupted by user]` / `…for tool use` | `query::record_interrupt` | **timeline only** | nobody wrote it |
| `(summary of the earlier conversation, from automatic compaction)` | `transcript::summary_message` | **timeline only** | ditto |
| `(Stop hook blocked continuation)`, the max-tokens resume, `<channel-messages>` | `query` | **timeline only** | ditto |
| `notify_user` tool_use in the agent's own turn | `tool::notify_user` | **the user's lane**, as a message | it is the agent speaking to the user, in the one tool that can |

The last four rows are the load-bearing ones. They are recognised **not** because the page wants to
show them but because the fallback rule is "unmarked prose is the hub" — so anything unrecognised
would be filed as the hub speaking, and a page that puts the runtime's words in somebody's mouth is
worse than a page that omits them.

**What the domain cannot say.** Every production caller of `deliver` passes `main` or `user`:
`SendMessageTool` (agent.rs:1497, hardcoded `HUB_NAME`), the DM composer (buffer.rs:598,
`USER_NAME`), `/team assign` (team_cmd.rs:306, `USER_NAME`). And `SendMessage` is assembled only at
depth 0 (`tools.rs:73`), pinned by `hub_agent_tools_only_at_depth_zero`. **Agent→agent direct
messages do not exist**; agents reach each other through rooms, which arrive as `InboxItem::Channel`
and never as `Direct`. The delivery matrix's "agent ↔ agent DM → both perspective pages" row
therefore describes a capability the code cannot yet express, and this batch does not invent one.
The counterpart lane is keyed by **name** rather than by an enum precisely so that the day `deliver`
carries a real sender, the projection needs no change to show it.

**One parser, two readers.** The markers were already half-parsed, in `buffer::scaffold_note` and
`buffer::user_posts`, whose own header says a second parser beside them "was the one thing worth
avoiding". So the shapes are now recognised once, in `buffer::line_source` → `LineSource`, and each
caller takes what it needs: the DM view collapses them to dim notes and throws the source away (a
pair conversation has two parties and the bubble already says which), while the page keeps the
source and files by it. `scaffold_note` became a pure function of a `LineSource`, and `user_posts`
walks the same enum — its output is byte-identical, which is what "the user's `@X` view is
unchanged" means and why every pre-existing `dm_posts` test was left untouched rather than adjusted.

This is also where a latent bug became visible and was deliberately **not** fixed: `[follow-up
instruction] …` and `[follow-up {n}/{m}] …` share a prefix, and `scaffold_note` matched both, so a
batched hub instruction rendered in the DM as `follow-up · waiting for a reply` with its text
dropped. Pair purity was an acceptance constraint for this batch, so the DM keeps that behaviour;
`line_source` distinguishes the two, and the perspective page shows the instruction correctly. The
fix for the DM is a one-line change on top of the enum whenever it is wanted.

**Structure, and one deviation.** The dispatch proposed `src/perspective.rs` (domain) plus
`src/tui/perspective_ui.rs`. The projection landed at **`src/tui/perspective.rs`** instead, because
what it produces is `Post` — a presentation type that lives in `tui::buffer` beside `dm_posts` and
`channel_posts`, the two projections this one is a sibling of. A domain module would have had to
either duplicate `Post` or invert the dependency, and inverting it to avoid a directory name is a
worse trade than the name. `dossier()` is pure regardless: it takes a history, its stamps and the
room logs as data, so every test builds a page without a `Session`.

**The rules the walk applies**, each stated because each is a judgement:
- **Room lanes come from the channel logs, never from history.** A room thread is `channel_posts` of
  `log_of(room)` — the whole room with the agent's own rows marked (`you`), because a thread that
  showed one voice would not be a thread. The relay lines in the agent's history are recorded in the
  timeline and left out of the room lane, so a lane never disagrees with itself about its own count.
- **The agent's turns attach to the counterpart it last heard from.** Where interleaving makes exact
  reply-attribution impossible this is best effort by construction, and it is the reason the
  timeline exists: completeness lives there, threads are the readable approximation.
- **Counts are messages, not rows.** Process rows are the agent's work; an index reading `@main (47)`
  because one turn made forty-five tool calls would be measuring the wrong thing.
- **Empty lanes are dropped.** A page is a record of what happened.
- **The timeline is a superset of the history-derived lanes, and deliberately not of the room lanes** —
  it is complete about the *agent*, and a room's log contains speech the agent may never have been
  woken for.

**The modal** is the D82 transcript's shape: a self-driving alt-screen loop that owns every key while
it is up and therefore takes **no `EscLayer` slot** — the same call `run_transcript_modal` makes, and
the reason `EscLayer::ORDER` did not grow this batch. Both levels live in **one loop**: they share a
snapshot, a theme and a frame, and two modals would have meant two claims on the terminal and a
snapshot rebuilt on every Enter, which is exactly the live-ness the page does not want. The thread
pager **is** `TranscriptState`, unchanged, so `j`/`k`, `g`/`G`, `/` and `n`/`N` mean there what they
mean under `Ctrl+O`. The cursor at the index walks **lanes, not rows**, so it cannot land on a group
heading (the D95 directory's rule). `q` closes from either depth and Esc walks one level — except
while the search input is open, which owns every key, because there `q` is a letter and Esc cancels
the search.

**`ctrl+e` is unbound, and that is the omission worth naming.** In the transcript it forces
`Activity::expanded` and `CollapseGroup::expanded` on the hub's messages and rebuilds; a thread's
rows are `Post`s, which carry a tool call as one collapsed line and carry no output at all. There is
no second state to show, so binding the key would have meant inventing one.

**One renderer, still.** `Chat::tail_post_rows` was split: the three settled kinds (a message, a
note, a step of the work) moved to a free `bufferview::settled_post_rows`, and the two live-only
kinds — the typing indicator and a queued send, which need the running instance's clock and colour —
stayed with the host. The page and the DM tail therefore print an agent's `⏺ Bash(git status)` with
the same code rather than with two that agree today.

**Deliberate scoping.** The page navigates and reads. It has no composer, no submit path and no verb:
a test walks every key it handles and asserts none of them produces anything but movement, opening
and closing. Live-ness is a snapshot on purpose (D82's precedent) — reopening is the refresh.

**Named limits.** A page today has at most two counterpart lanes, because the domain has two senders.
A compaction clears an instance's stamps outright (`agents.rs:936`), so a compacted agent's lanes
sort by a clock that reads zero and the index shows no time beside them. The index has no windowing:
an agent in more rooms than the terminal is tall scrolls off the bottom, which the thread level's
pager would solve and the index's does not have. And the page reads the live registry only — an
agent whose instance is gone has no history to show, while its DM buffer survives.

- 1405 + 13 tests before, 1423 + 13 after (18 new: 9 projection — the attribution catalog, runtime
  scaffolding staying out of threads, the protagonist rule, `notify_user` as a message, the room
  thread, the timeline superset, lane ordering, counts, the empty page; 9 modal — the level walk,
  `q` from either depth, the search input owning `q`/Esc, the lane cursor, snapshot semantics, the
  read-only guard, the index's groups and counts, the thread's rule, the footer per level).

### D97. Presentation: a gutter for faces, a floor for the bar, a door out for pictures

**Three debts, one batch.** They share no code and one theme: the conversation model was complete and
did not yet *look* like itself. The batch's rule was that none of the three may introduce a second
renderer, a second convention or a new dependency.

#### Inventory 1 — the avatar machinery

`src/tui/avatar.rs` survived D89 intact: eight bundled portraits (`include_bytes!`, keyed by portrait
rather than by sender so two members sharing a face share one transmit), `placeholder`/`transmits`
over the D42 kitty path, `Palette`, `gutter_cell`, `sender_band`, and a private `gutter(images)`
returning 5 (image skin: `COLS + 1`) or 4 (chip skin). What actually *rendered* before this batch was
only two things: the `experimental.chatAvatars` sender band above hub messages (`chat_tail.rs:2689`),
and a subagent watch row's portrait replacing `◉`/`⎿` where images place (`chat_tail.rs:2718`).
Everything else was colour without a picture — `pal.avatars[…]` for the bar's teammate tint, `pal.unread`
in the switcher. **DM, room, perspective and transcript rows carried no gutter and no face at all.**
The retired shape is recoverable at `82bf32a^:src/tui/slack.rs` (`indent_rows`/`gutter_line`), and it
is the shape this batch mirrors.

Two obstacles the inventory turned up. First, the three post-row builders — `settled_post_rows`
(free fn), `Chat::tail_post_rows` (`&self`), `perspective_ui::thread_rows` (free fn) — take neither a
sender nor an indent, and `Post.from` was being discarded. Second, `Chat::faces` (the transmit sweep's
source) is only writable from `&mut self`, which none of those three has.

#### Inventory 2 — the chrome stack

`chrome::chrome` is one function pushing ~20 conditional regions in order; `fullscreen` changes only
where the suggestion area goes. Heights are never predicted (`el::height` renders and counts), and
both hosts — `Frame::assemble` inline, `fullscreen_frame` — treat chrome as one opaque bottom-anchored
block and truncate it **from the top**. So moving the bar is a one-line move inside `chrome()` and no
host change at all. The states that touch the bottom rows: the busy status row (top of chrome, far
above), the ask dialog (in the *document*, above all chrome, leaving only a `Waiting for permission…`
row behind), the picker menus (all `return` early and replace the suggestion area, never stack), the
D80 esc hints (text inside the status row, no rows of their own), and the D84 bash tail (inside the
assistant message, not chrome at all). Nothing competes for the last row.

#### Inventory 3 — where images enter

Two disjoint worlds that never met. **Wire images** (`ImageAttachment`, base64) go to the model and
into the transcript and are never drawn. **Display images** (`ImageMeta` + kitty) are decoded from
markdown `![](url)` in message text, drawn, and never sent. Producers: the clipboard paste
(`register_image`, macOS), a path in the composer (`expand_image_paths` — the `PathBuf` was known and
thrown away one line later), the Read tool (`read.rs:79`, which produced an image block and registered
nothing anywhere), MCP results, and `load_message_images` → `UiEvent::ImageReady`. The D93 vision
projection is a `Cow` view taken at the send seam (`client.rs:407`); the history keeps its image
blocks, so a registry built on the session's own data is untouched by it — confirmed rather than
assumed. Click plumbing is `ClickTarget`/`ClickRange` resolved by `Chat::doc_click`, and clicks reach
the fullscreen host only. The one existing detached spawn is `share::open_in_browser` (`cfg!` three-way,
`spawn` not `status`, no test seam); the testable-process pattern is `composer.rs`, where the command
is simply a parameter. Windows is CI-enforced on all three gates.

**Piece 1 — the gutter.** Added `El::Gutter { cells, blank, child }`: the child renders first and the
rows it produced are indented afterwards, so the *row count* is unchanged and every click range and
the caret keep the offsets the walk computed. That is the whole argument for a wrapper over a second
row builder, and a test asserts it directly. `avatar::Gutter` is the value threaded through all three
surfaces — width, palette, the pinned table, `index_for`, `cells(index, name, lead)`, `apply` — so
"how wide", "who gets a face" and "which skin" are decided once. `settled_post_rows` gained an
`Option<&Sender>`; `sender_runs` marks which posts open a run. **The width comes out before anything
wraps**, which is the failure the CJK test pins: a body wrapped at the full width and then indented
overruns the terminal by exactly the gutter.

Placement rules, all asserted: the portrait on the first row of a sender's run only (a work step does
not break a run — a tool call is inside its own turn); blank gutter on continuation rows and on
process/note rows, so the message column is one straight edge and only somebody who spoke gets a
picture; and **no gutter in the hub**, keyed off `Decor::Said`, which is set by the conversation
replay and by nothing else. The `faces` problem was solved by recording up front in `build_rows`
(`&mut self`, before the loop takes its borrows) and by transmitting all eight portraits once when the
perspective modal opens — the alternate screen is short-lived and knowing which faces a thread will
show would mean laying out every lane before drawing one.

**Piece 2 — the bar.** Moved from directly above the composer to the last row of the chrome. The old
argument was that the bar is *about* the composer; the better reading is that it is this window's
status area — where you are, what is unread — and a status area belongs at the bottom edge. One line
moved in `chrome()`; no host change, as the inventory predicted. A new test drives busy + an open
picker + a second conversation through both hosts and asserts the bar is last, the composer appears
exactly once, and the busy row is above it all.

**Piece 3 — the registry.** `src/tui/images.rs`. Entries are `(id, source, at, bytes, format, marker,
origin)`, newest-first to every reader, deduplicated by source plus a hash of the head of the content
so a repaint does not grow the list. `Origin` is the load-bearing distinction: an image **already on
disk** is addressed where it lives — never copied, never removed — and an image that exists **only in
memory** is written into a pid-tagged temp dir on first open and is the only thing eviction deletes,
by the exact path it wrote. Bounds 100 entries / 50 MB, oldest first.

Three tees, chosen to match the spec's own definition (a picture that *renders*): `UiEvent::ImageReady`
on success (every markdown image — an agent's chart, a URL in the model's prose), `register_image`/
`register_image_file` at the composer (clipboard and attached paths, where the source label was
already being discarded), and `ToolDone` for the Read tool. That last one needed a contract: `read.rs`
now owns `image_result_line` with `image_result_path` beside it, one formatter and one reader in one
place, rather than the TUI guessing at prose. Avatars register nowhere, and the rule holds at the tee.

Three doors, one action. A click resolves **before** the click ranges — an image inside a tool's
output would otherwise be swallowed by the enclosing collapse group — and resolves either the row's
own `ImageRef` URL or the `#[image N]` marker in a bubble (`api::image::first_marker`, one regex).
`/images` is the `/theme` shell verbatim, which is why it costs no `EscLayer`: the existing `Menu`
slot already covers it. `o` in the transcript opens the first image row in the window, because a pager
has no cursor and the top of the window is where the reader's eye is. The open spawns detached with
the platform triple `share.rs` already settled on, with the program as a value so the acceptance tests
point it at a recording script — `composer.rs`'s pattern, no trait and no mock.

**Deviations.**

1. *Gutter width.* The dispatch asked for "a fixed 2-3 cells". The gutter is `avatar::gutter_width()`
   — 5 with images, 4 with the chip — because `COLS` is the portrait's own width and anything narrower
   would put body text over the image cells. `a_placeholder_row_measures_exactly_the_chip` has guarded
   that number since D50.
2. *Picker line.* Specified as `N. <source> · <stamp> · <WxH or size>`. It shows size: nothing in the
   codebase retains an image's pixel dimensions (`ImageMeta` carries *cell* cols/rows, `prepare_image`
   discards everything), and decoding a header per row to print `1920x1080` would be a new cost for a
   field the spec offered an alternative to.
3. *Transcript `o`.* Specified as "`o` on an image row". The pager has no cursor to be on a row with,
   so it acts on the first image row in view and is a no-op when there is none.
4. *Bar position.* Placed after the footer row, so it is the window's true last row rather than
   second-to-last. "The LAST row of the chrome" is the dispatch's own wording, and a status area under
   a hint row would read as a hint.
5. *Read-tool images and the fullscreen click.* A Read-produced image registers and is openable by
   `/images`, but it renders as a tool *result line* rather than as an image block, so there is no
   picture on screen to click. That is a property of how the tool reports, not of the registry.

**Named limits.** A row carrying `line.image` renders as placeholder cells with its text segments
discarded (`view::to_line`), so an image block inside a gutter draws at column 0 rather than indented —
rare in a DM and unchanged from the pre-D89 behaviour. The live tail starts its sender runs fresh
rather than reaching back across the settled seam, because everything above it is frozen. And the
perspective page transmits all eight portraits on open rather than the ones it will use.

- 1423 + 13 tests before, 1447 + 13 after (24 new: 6 registry — newest-first labels, dedup, bounded
  eviction that removes only its own file, on-disk materialization to itself, the platform opener and
  detached spawn, size labels; 7 gutter — the run rule in a real DM, the hub's absence of one,
  process/note taking the indent and no face, runs unbroken by tool rows, CJK width, the image skin's
  placeholder cells, the perspective thread; 1 `El::Gutter` invariant — clicks and the caret unmoved;
  1 chrome — every bottom state composing around the bar; 9 image flows — content vs avatars, a failed
  load registering nothing, `/images` listing and Enter opening, the `Menu` Esc layer, the empty case,
  the click target on both row shapes, an ordinary row not being one, transcript `o` and its footer,
  and a failed open landing on the info tier).

### D98. The quiet console, and one verb for speaking

**Problem.** D94 removed the lifecycle *lines* from the hub and stopped there. Underneath, the bus
was intact: `chat.rs`'s `WatchEvent` arm fired `submit_auto()` on every terminal state, trigger-blind,
so the user typing into an agent's DM woke the main agent to digest their private exchange — D63's
privacy line drawn everywhere except on the wake path — and every room post with `main` as a member
bought its own woken turn, so three agents talking for a minute bought three digests of a conversation
that had not finished happening. Meanwhile three tools meant "say something": `SendMessage` (main→sub),
`Post` (→room), `notify_user` (sub→user). Three verbs, three vocabularies, one act.

**The inventory**, taken at `e7df0e3` before anything changed:

| Piece | Where | Now |
|---|---|---|
| `SendMessageTool` — main-only, input field `agent`, sender hardcoded `HUB_NAME` | `tool/agent.rs` | The one speech tool: `to` is the conversation namespace, assembled at every depth, sender stamped from `session.instance` |
| `PostTool` — the room wrapper around `deliver_post` | `tool/channel.rs` | Deleted. `deliver_post` is untouched and is still the single path a post takes |
| `NotifyUserTool` + `notify_user::{Relay, Notice, Verdict, NotifyLevel}` + `UiEvent::NotifyUser` + `RELAY_PREFIX`/`is_relay_line` + `Buffers::note_relay` + `Runtime.notify_user` + `ToolContext.notify_user` + the headless stderr sink | seven files | All deleted. Nothing else called any of it |
| `submit_auto()` on every terminal `WatchEvent` | `chat.rs` | Gated on `has_wake_notifications(None)` — the same question TurnEnd already asked |
| `has_hub_mail() → submit_auto()` at the channel row and at TurnEnd | `chat.rs` ×2 | One tick-driven debounce, `chat_tail::digest_mail` |
| `<channel-messages>` injection, drained by **every** session | `query.rs` | `<messages>`, drained by the main session only |

**Decision.**

*One tool, because addressing is the thing that was actually being enforced.* Hub-and-spoke never
needed a withheld tool; it needed a rule about who may be named. `to` is read by `parse_address` into
`Agent(name)` or `Room(name)` — `#name` is a room, anything else is an agent and may wear the `@` the
bar shows — and `check_target` narrows by caller: main reaches any instance and any room it is in, a
subagent reaches `main` and the rooms it is a member of. The refusal names what the caller *may*
address instead, because a refusal that only says no teaches nothing. The `channels_on` gate and the
depth-1-named-instance cohort that governed the retired `Post`'s assembly now govern room *addressing*
inside the tool, so the experimental feature's blast radius is unchanged.

*The description is built from the session.* A subagent and the main agent read different tools with
the same name — different reach, different lane advice, and the room clause missing entirely when
rooms are off. `PostTool` already did this with `sender_of`; it is the one place the tool layer knows
who is calling, and a static string would have had to describe both callers to each of them.

*`urgent` is refused, not ignored, outside subagent→main.* It rings the user's attention channel, and
main writing to a subagent, or anyone writing to a room, has nobody on the other end to interrupt. A
silently-dropped flag is a contract the model cannot learn.

*One inbox, one drain, one injection.* A direct message to main rides `hub_mail` — the store room
relays already used — rather than getting a sibling store, so the query layer keeps exactly one
drain-and-inject seam. What tells the two apart is the marker on the line: `[message from @scout]`,
on its own line above the text, which is `[DM from user]`'s shape carrying the one thing that marker
never had to (the human is the only human; `main` hears from many). The wrapper is renamed
`<messages>` because it is no longer only channel messages. `line_source` gained `Agent { name }` and
stays the single recognizer; the perspective page unwraps the block rather than collapsing it to one
note, so D96's projection can put the message in the sender's lane — the seam D100 needs for main's
own page. **A real bug fell out of the inventory**: `drain_hub_mail` was unguarded and the registry is
shared, so a subagent's own turn boundary could eat mail addressed to main. Now gated on
`session.instance.is_none()`, the same guard `release_hires` carries three lines above it.

*The trigger decides whether a run's end is main's business, and it is decided at registration.*
`wakes_owner(&items)` reads the batch that woke the run: empty (a dispatch — the `Agent` call itself
is the trigger) or containing anything that is not a user-origin `Direct` ⇒ main's business. The
answer is stamped into the watch entry as `notify_owner`, and a `false` entry enqueues no
`Notification` on `set_state` or `emit_signal` — so `has_wake_notifications` is false, nothing is
injected, and nothing wakes. **The suppression is at the queue, not at the wake site**, which is why
one flag covers both the auto turn and the `<task-notifications>` line, and why the broadcast is
untouched: the team directory's feed rides the broadcast and still records every run. `chat.rs`'s
wake then gates on `has_wake_notifications(None)` — a question the code already asked at TurnEnd, so
the two wake paths now say one thing, and a nested subagent's completion stops waking main as a
side effect of asking it properly.

*Bad news is the asymmetric case.* `Done` and `Cancelled` can wait for the main agent to narrate them,
because the dispatch row's own state already says so and a narration that never comes costs nothing.
A crash cannot: the turn that would have narrated it may never run. So `Failed` — and only `Failed` —
draws `⚠ @scout · subagent failed: …` in the theme's error tier and rings D79. The instance name is
`label.split_whitespace().next()`, which is the first token of every label shape the run watches
produce (`scout · task`, `scout #3 · …`, `scout #7 receipt`). It keeps its send stamp, inheriting the
exception D94 wrote for the relay: it is news, about someone, at a moment that matters.

*The debounce runs on the tick, which is what makes it need nothing else.* `digest_mail` reads a
length the domain already keeps; a burst is exactly a length that keeps changing, and every change
restarts a 2s quiet window under a 15s ceiling. Constants in ticks (`MAIL_QUIET_TICKS`,
`MAIL_DEADLINE_TICKS`) at the 33ms frame: two seconds is a room's round trip — agents answering each
other land inside it — and fifteen is short enough that a room which never goes quiet is still read.
`needs_tick` gained `has_hub_mail()`, because mail landing in a fully idle session is the one thing
that has to wake the clock rather than ride an event. The urgent flag is read **before** the emptiness
test: the drain and the ring are different readers on different clocks, and a turn already running can
absorb the message before the tick ever sees it, so a bell owed must survive the drain that beat it.

**Consequences.**

- The three prompt notes were rewritten rather than patched: `SUBAGENT_NOTE` gains the deliberate road
  to main (with what *not* to send), and `CHANNEL_NOTE`'s "only `Post` puts words in the room" — the
  sentence that exists because the model cannot infer that turn text never reaches the room —
  becomes "only a message addressed to the room". Its tests assert phrases, and one of them broke on a
  line wrap rather than on a meaning; the phrase was moved off the fold rather than the assertion
  weakened.
- `AgentControl`, `Channel`, `deliver_post` and the whole ack-watchdog chain are byte-identical.
- **Old assertions rewritten, not weakened.** `notify_user_is_a_subagent_tool_only` became
  `a_subagent_gets_send_message_and_neither_retired_tool` — same question (what does the *other*
  direction get), new answer. `channel_tools_gated_by_experimental_flag` dropped its `Post` clauses and
  keeps every `Channel` one. The two `tool/channel.rs` delivery tests now drive `SendMessage(to:
  "#room")` and assert the same outcomes, which is the point: the machinery did not change, only the
  door. `terminal_watch_event_triggers_auto_turn_when_idle` and
  `signal_triggers_auto_turn_even_while_typing` were synthesizing a `UiEvent` with no registered
  watch behind it; they now register one and drive it, which is what production does — and the second
  half of the rule got a test of its own beside them.
- `perspective::a_notice_is_a_message_in_the_user_s_lane` became
  `a_direct_message_to_main_lands_in_its_sender_s_lane`: the tool it was about is gone, and the thing
  it was really pinning — an agent's words reaching a lane that is not the one it was working in — is
  now the marker's job.
- 1447 + 13 tests before, 1444 + 13 after (19 removed with the relay: 7 arithmetic, 6 tool surface, 6
  hub rendering; 16 added: 6 addressing and delivery, 1 notification suppression, 1 unaddressed
  terminal, 3 alert-line rendering, 4 debounce, 1 marker attribution).

**Named limits.**

1. *@main loses the unread count D94 gave it.* That counter existed only to count relays, so it
   retired with them, and an alert line raises no badge this batch. D99 gives @main a real unread.
2. *No wire event for a direct message on the JSON protocol host*, and no tick loop there or in
   headless — mail waits for the main agent's next turn boundary, which is where it was already read.
3. *The failure alert fires for every `WatchKind::Agent` failure*, ack-watchdog give-ups included. That
   is deliberate (a chase that gave up is news), but it means one instance can produce two alerts for
   one bad run: the run's own failure and its receipt's.
4. *`wakes_owner` treats a room relay as main-relevant.* A room post that wakes an agent still wakes
   main when that run ends. Narrowing it is a question about rooms, not about the user's DM, and this
   batch did not open it.

### D99. The pure pair, and a face for the console

**Problem.** Two of the three surfaces the v3 model names were already right; the DM was not. `dm_posts`
rendered an agent's *whole context* flat — the prompt the instance was spawned with, main's
instructions, room relays, chases and the task reminder, collapsed to dim notes and interleaved with
the user's own conversation — and its work as N flat `⏺ Tool(…)` lines that the settled replay then
dropped altogether. Beside it, three pieces of accounting were measuring the wrong things: a DM's
badge counted the history's length (a turn with forty tool calls read as forty unread messages), every
DM change set `mention`, and @main had no unread at all since D98 retired the relay it used to count.

**Decision.**

*The DM is a lane of the projection, not a second reader of the record.* D96 built the machinery that
answers "who said this" and left it four keys deep on a read-only page; D99 makes the pair view its
first production consumer. `perspective::split_user_text` and the per-message loop that used to sit
inside `dossier` are now one function, `walk(agent, history, stamps) -> Vec<Filed>`, which files every
post in record order. `dossier` keeps every lane it files; `pair_lane` keeps `Dm(user)` and drops the
rest, and `buffer::dm_posts` renders that. **`buffer::user_posts` and `scaffold_note` are deleted**:
their whole job was collapsing somebody else's traffic into dim lines the DM should not have been
showing. `line_source` is untouched and is still the single recognizer — the point of the batch is
that there is now one *walk* over it as well as one parser.

What falls out of the attribution rule, and is the batch's one behavioural surprise: **an agent that
main spawned and the user never spoke to has an empty `@agent` view.** Its first user message is the
task (intake), so `active` is `None` and its report attaches to no counterpart. That is the model's
own answer — the report is main's news, and main's dispatch row already carries it — and it is named
here because it is the thing a reader will notice first.

*Work renders through the console's collapse machinery, which meant carrying the call and not the
line.* `tool_call_line` throws the tool name and input away, and `classify_tool` needs both, so the
walk carries `Work::{Tool{name, input}, Thinking}` beside each process post. `buffer::pair_replay`
turns a run into **one `UiMessage`** — prose concatenated, each call an `Activity` at the char offset
it happened at, groups opened and extended by the same rules `on_tool_ready` applies — and
`Replay::Message` already routes that through `assistant_el`. So `⏺ Searched for 1 pattern, read 2
files (ctrl+o to expand)` in a DM is `collapse_summary` itself, not an imitation of it. Tool names are
**interned** rather than `Box::leak`ed per call, because a replay re-reads the same names on every
switch and the live path's leak-once-per-call is only sound when the call happens once.

*A run ends where anything at all stood between two of the agent's rows in the full walk.* This is the
rule that keeps the flow append-only, and it is why `PairPost` carries `contiguous` rather than the
consumer computing adjacency in the filtered lane: every continuation is triggered by an inbox item,
every inbox item files *something* in the walk (main's prose, a `[#room …]` relay, a `[follow-up N/M]`
chase), so a continuation can never extend a message the flow has already printed. Adjacency measured
in the filtered lane would have merged across exactly those, and `poll_active_conversation` — which
appends by count — would have shown the reader nothing.

*The live tail is gated on whose run it is, and the messages beside it on whose they are.* D98 already
computes main-relevance per run (`wakes_owner` over the drained batch) and stamps it on the watch
entry; the same answer is now stamped on the instance (`set_run_trigger`/`run_is_the_users`) so the
view can ask it. A run that is not the user's shows no stream **and no typing row** — the indicator is
a promise of a reply, and none is owed. That alone is not enough, because `in_flight` and `pending`
would still have drawn main's message as the user's bubble and kept the indicator alive through it, so
`Entry.in_flight` gained the sender it was already being handed (`AgentView`'s fourth element is
`(from, text)` now) and `dm_state` filters both to `user`. Two filters, because they answer two
different questions: whose run, and whose message.

*Main's portrait is reserved by removing one from circulation, not by adding a ninth.* The requirement
is that main's face never move and never be a teammate's. A hash reserved at index 0 with the id still
pinnable would have been probabilistic; bundling a ninth portrait would have meant authoring an asset
to match eight that already agree. So `MAIN_INDEX = 0`, `index_of` hashes over `1..COUNT`, `ids()`
returns the seven a blueprint may pin, `index_of_id` refuses main's, and `Gutter::index_for` answers
`main` **before** the pinned table — a reservation a pin could override would not be one. The cost is
one face out of eight and one retired pin id (`emi`); a `team.json` that pinned it falls through to
the hash, which is what it already did for a typo. This also surfaced a latent bug in
`team_cmd::crew_portraits`, which filtered by *position in `ids()`* against *portrait indices* — the
same number until D99, not after.

*The sender band retires with the console's gutter.* D97 put the band overhead with an explicit
premise: "the main chat has no gutter — its bodies run the full width — so the face goes overhead."
D99 removes the premise, and with both in place `experimental.chatAvatars` drew the same speaker's
portrait twice on one message. `avatar::sender_band` and `Chat::sender_band_el` are deleted; the switch
keeps the one job the gutter does not do, the portrait on a subagent's watch row.

*`speaker_of` names @main's two speakers — and refuses to name a state line.* The console's
participants were never written down (the role *was* the name), so the gutter had nothing to key on;
`speaker_of(item, role, text)` answers `main`/`user` for `Decor::Hub`, and the run rule
(`spoke != previous`) then works in the console for free. But not every user-role row in the console
is the user's: the D98 failure alert, a route receipt, an ask receipt, the interrupt marker and a
rewind line are the runtime reporting, and the first cut hung the human's portrait on all of them —
`⚠ @scout · connection reset` with the user's chip beside it says the human wrote it. So a
`Decor::Hub` row that satisfies `is_state_line` answers `None`, which costs it the face and takes it
out of the run; main speaking after an alert therefore re-leads with its own, which is the visual
break the interruption already is. **The gutter stopped being decided by the speaker**: every
non-rule row takes the column and only a speaker takes the cells, because a state that gave up the
indentation as well would make the message column jog around it — rules span, states align. This is
the ruling the DM tail's live-only states have carried since D97, applied to the surface that just
grew a gutter. A steered message (`↪ …`) is not a state line and is untouched: the user typed it.

*Unread is measured where the measure was already stated.* `Lane::messages` has said "process rows are
work, not messages" since D96; the bar never read it. `pair_measure` returns `(Said count, an agent
Said after the read cursor)` from the pair lane, memoized per instance on the history's length —
sound because a history is replaced wholesale at a run's end and never edited in place, and the one
rewrite that exists (compaction) makes it shorter. @main's own counter is pushed rather than polled
(`Buffers::note_console`), because @main is the one conversation with no domain store behind it: its
record is the flow. Main's prose at `TurnEnd` counts; the D98 alert counts **and** mentions, which is
the one line here nobody chose to say.

**Consequences.**

- `REPLAY_BUDGET` 30 → 8, with the reasoning restated: thirty was sized when a replay was the only way
  back into a conversation, and it has not been since D82/D96.
- Room mention detection is one predicate, `buffer::names`, case-insensitive with word boundaries on
  both sides of the token. `@User` and `@USER,` reach the person; `@username` and `mail@user.example`
  do not.
- **Old assertions rewritten, not weakened**, each because the contract under it changed:
  `the_hub_flow_wears_no_gutter` → `the_console_wears_the_same_gutter_every_conversation_does` (plus a
  both-skins layout test, the D97 invariant extended); `without_the_switch_the_transcript_wears_no_face`
  → `…_no_band` (the gutter is not the switch's any more);
  `sender_band_names_the_speaker_and_records_its_face` and
  `sender_band_costs_a_second_row_only_where_portraits_place` → one test that the console names its
  speakers in the gutter and not above them; `a_dm_is_addressed_to_you_by_construction` →
  `a_dm_wants_you_when_the_agent_answers_and_not_when_you_speak`, which is the opposite claim and the
  right one; `unread_counts_one_per_message_and_moves_the_stamp`, both `a_replay_keeps_to_its_budget`
  tests, `a_switch_opens_the_conversation_under_a_rule`,
  `an_excursion_holds_the_hubs_tail_until_you_come_back`, `arrivals_print_here_and_count_there`,
  `a_dm_wears_a_face_on_the_first_row_of_each_run`, `a_completion_bumps_the_dm_instead_of_the_hub` and
  `the_pager_covers_the_conversations_the_flow_printed` all had histories of bare `assistant(…)`
  replies, which now belong to nobody's lane; they were given the user message the reply answers,
  which is what a pair conversation is. Four row-prefix assertions in `chat_tests_b` read through a new
  `test_util::body`, which takes the gutter off a row rather than asserting around it.
- 1445 + 13 tests before, 1459 + 13 after (19 added: 4 pair-lane projection — the filter, reply
  attribution, the work it carries, the run break; 3 replay — activity groups and their wording, the
  standalone call closing a group, the budget; 6 gutter — the console's gutter, both skins laying out
  alike, main's reserved face, the console naming its speakers in the gutter and not above them, a
  state line taking the indentation and nobody's face in both skins with the run re-leading after it,
  and a steered message keeping the user's; 3 accounting — Said-only counting, mention on an agent's
  Said, @main's unread; 1 the live tail gated by run trigger; 1 the room mention predicate; 1 the
  switch keeping the watch row and losing the band.
  5 removed: the two band tests, the hub's absence of a gutter, `a_dm_is_addressed_to_you_by_construction`,
  and the no-face-without-the-switch claim — each renamed or replaced above rather than dropped).

**Named limits.**

1. *A collapse group cannot span two of the agent's runs.* In @main a turn is one message and a streak
   of reads across four rounds is one group; here a run boundary is a message boundary. Within a run
   (which is where a streak actually happens) the grouping is the console's exactly.
2. *A replayed group has no output to expand.* The record kept the call; the result went to the model.
   `ctrl+o` on one shows the calls it folded and nothing under them.
3. *An agent whose lane is empty shows an empty DM.* See above — deliberate, and the reason D100's door
   to the observation page matters more than it did.
4. *`pending`/`in_flight` are filtered by sender, not by run.* A message main queued while the user's
   run is in flight is correctly absent from the DM, but a *chase* the harness queued has no sender at
   all and is already excluded by `pending_of`'s own rule, which predates this batch.

### D100. The record's doors, and a page for the console

**Problem.** D96 built the observation page and D99 made its walk the pair view's only reader, which
left two gaps the model names and the code did not fill. The page existed for *subagents* only —
`perspective_ui::snapshot` reads `agents.view_of(name)`, and main is not in that registry, so the one
participant whose whole job is coordination had no record of its own coordination. And the page was
four keys deep: `ctrl+b` → ↑/↓ → Enter → tab, from a manager whose other verbs are about stopping
things. D99 also shipped an honest consequence with no way out of it: an agent main spawned and the
user never wrote to opens an **empty** `@agent`, because its task is intake and its report answers
main — a blank screen under a rule, with the thing you actually wanted one key away and unmentioned.

**Decision.**

*The unmarked default is a property of the protagonist, not a constant.* Everything the page does
already worked for main except one line: `split_user_text` filed unmarked user-role prose to
`HUB_NAME`, which is right in a subagent's record (the hub is the one sender `direct_text` leaves
unmarked) and exactly wrong in main's, where unmarked prose is the human typing into the console.
Nothing else writes plain prose into a session transcript. The second flip rides with it: the
first-user-message-is-intake rule exists because the `Agent` tool's prompt is the task that created
the instance, and nobody dispatched main, so the first thing ever typed into the console is a
message. Both are `Protagonist { name, default, spawned }`, resolved by `Protagonist::of(name)` —
`main` is a reserved member name (`channels::HUB_NAME`), so no instance can answer to it and the
resolution cannot mistake a teammate for the console. `LineSource::HubBatched` follows the same
default rather than a second hardcoded name: it *is* the unmarked sender, wearing a batch label
because the batch made its boundaries ambiguous.

`buffer::line_source` was **not** extended. Everything main's transcript can hold was already a
recognised shape — the `<messages>` envelope and its pre-D98 `<channel-messages>` predecessor unwrap,
`[message from @X]` lands in the sender's lane (the seam D98 built for exactly this), `[#room msg #N]`
stays timeline-only because the room's log is authoritative, `<task-notifications>` and the task
reminder are intake, the steer block is the user, interrupt/compaction/stop-hook/max-tokens are
timeline-only. The main-specific shape a reader might expect — a marker naming the user — does not
exist and must not be invented: in main's record the *absence* of a marker is what names them.

*The clock is the turn marker, and that is the whole answer.* A subagent's history is stamped per
message (`AgentRegistry::finish`). A transcript is not: `Transcript::append` writes bare messages, and
the one wall clock on disk is D91's `{"type":"turn","at":…}` line, written by `record_turn_open`
before the message that *opens* a turn. Everything recorded inside that turn — the
`<task-notifications>` block, the `<messages>` inbox, every assistant reply — goes down through plain
`record` and carries nothing. So `main_record` reads `load_projection()` (which already surfaces the
marker as `Entry.opens_turn`) and **carries the turn's stamp forward** across the messages recorded
inside it. That is a turn clock, not a message clock, and it is named as one: the messages of a turn
did belong to that turn, and the alternative — stamping only the opener — would have left main's
agent lanes reading zero and sorting by nothing, since mail and notifications are never turn openers.
A transcript written before D91 has no markers at all and reads 0 throughout, which is the
projection's documented "no clock": lanes sort by a zero and the index shows no time beside them,
exactly as a compacted agent's page already does. Only the index's trailing stamp and the lane
ordering read `at`; `settled_post_rows` never did.

*Three doors, one page, and the composer decides which key.* `tab` on an **empty** composer opens the
active conversation's record: `BufferId::Dm(name)` → that agent, `BufferId::Hub` → main,
`BufferId::Channel(_)` → nothing at all, because a room has no single protagonist and giving the key
a second meaning there would be inventing one. Tab survived unchanged as completion because it never
had to be taken from anything: the slash dropdown and the `@` mention dropdown are judged far above
the editing keys and both require text, `KeyCode::Tab if self.bash_mode` keeps history completion, and
before this batch a bare `Tab` on an empty composer fell through to `_ => false` — an unbound key. So
the two readings cannot compete, and the ctrl+g fallback the dispatch offered was not needed. The
door is inert behind `pending_ask`, the rule the switcher and the directory already carry (D81): a
full-screen surface must not open over a question holding up a turn. In the directory, `o` opens the
member under the cursor and closes the panel behind it, the way Enter already hands the screen over;
on a room it does nothing and the panel stays open. `ctrl+b` detail → `tab` is byte-identical.

*Main is a row on the roster, and the roster is where it always belonged.* Its presence is the **host
turn** (`chat.busy`) rather than a registry state, its label is `main · console · idle` in the
`kind.label()` grammar the other rows use, and its rooms come from `channels::rooms_of(HUB_NAME)` like
anyone's. Enter on it is the one special case: `BufferId::Dm("main")` is not a conversation that
exists, and `BufferId::Hub` *is* the pair view of the user and main, so Enter goes home. It gains no
manage verbs, which costs nothing to enforce — the directory has none, by D95's ruling.

*The empty pair's note is furniture, not replay.* Putting it in `Buffers::rehydrate` would have made
it a replay *item*, and `Excursion::seen` counts items: the note would have set the cursor to one and
`poll_active_conversation` would have read the pair's first real message as already printed. So it is
emitted in `open_conversation` beside the rule, in the rule's own row shape, and counted nowhere —
the same exclusion `replay_items` makes for the divider, for the same reason. Idempotence is read off
the flow rather than stored: switching out and back prints `── hub ──` and `── @scout ──` again, so
the check walks the message store backwards *past the rules* and prints nothing if the first real
line it finds is the note itself. No new state, and the one thing that could go stale — whether the
note is still the last thing that conversation said — is the question being asked.

**Consequences.**

- **Old assertions rewritten, not weakened.** `directory::enter_opens_a_member_dm_and_a_room` asserted
  the target list is `[scout, parser]`; it is `[main, scout, parser]` now, so the list assertion states
  the new contract and both Enters step past main's row. `j_joins_the_room_under_the_cursor` moved the
  cursor down one before pressing `j`, because the row it was sitting on is main's now and `j` on a
  member has never done anything. Every pre-existing `perspective` and `dm_posts` test passes
  untouched, which is what "the flip does not regress a subagent's page" means.
- The directory footer gains `o record`, and the module doc gains main's row and its reasoning.
- `walk` and `split_user_text` take a `Protagonist`; `dossier` and `pair_lane` resolve it from the
  name they were already given, so no caller outside this module changed.
- 1459 + 13 tests before, 1471 + 13 after (12 added: 2 projection — main's page reading prose as the
  user with mail in its sender's lane and notifications as intake, and the flip moving no marker;
  2 snapshot — main's record from the transcript with the turn clock on every lane, and a session
  with no transcript answering with an empty page rather than a panic; 4 doors — tab in each of the
  three conversation kinds, tab with a draft still completing, tab inert behind a permission ask,
  and `o` in the directory on main/an agent/a room; 1 roster — main first, its presence following the
  host turn, Enter going to the console; 1 footer; 2 the empty-pair note — printed once across a
  round trip and gone once the pair has content, and not swallowing the first message the poll
  appends).

**Named limits.**

1. *Main's page is as live as its transcript.* The snapshot reads the file, so a turn in flight is not
   on it (D82's semantics, stated) — but neither is anything the current turn has recorded and not yet
   flushed through `record`. In practice `record` persists before it pushes, so the file leads the
   in-memory history rather than trailing it; the gap is a turn's streaming text, which lands at the
   turn's end.
2. *The turn clock is a turn's clock.* Two messages sixty seconds apart inside one long turn read the
   same time. The honest alternative was zero.
3. *The empty-pair note's idempotence is textual.* It compares the flow's last non-rule message against
   the note, and a rule is recognised as `── … ──`. A user message that is itself exactly that shape
   would be mistaken for furniture — a cost of not adding state, and the same shape the flow already
   treats as a rule everywhere else.
4. *`o` is a directory key, not a global one.* There is no door to a *non-member's* record, because
   there is no surface that lists one: an agent whose instance is gone has no history to show, which is
   D96's limit, unchanged.

### D101. The rename: hub retires, @main is the floor

**Problem.** The floor of the terminal was labelled `hub`, and by D100 the word named nothing. Its
three historical meanings had already resolved separately: the bus died in D94 and D98 (no agent
lifecycle line writes into the console at all), the pair view became one view type among many in
D99, and D100 gave main a directory row, an observation page and a `console` label to go with them.
What was left was a word the user read in the bar, in every rule the flow prints, in `/open`'s
grammar and in the `?` panel — one participant addressed by a name the address grammar does not use,
in a session where every other conversation is `@name` or `#name`. `HUB_NAME`'s value had been
`"main"` since channels.rs was written (channels.rs:37), so the constant had been lying about
itself for the whole program.

**Decision.**

*One rule, applied everywhere: where a name says "hub" and the thing it names is main-the-participant
or the home conversation, it says main.* `BufferId::label()` returns `format!("@{MAIN_NAME}")` rather
than a literal, so the label, `Display`, `rule()` (`── @main ──`), the bar, `ctrl+k`, `/open`'s
completion and the conversation-bar entry all flip from one line — D88's "one vocabulary" property
paying for itself. The constant is `channels::MAIN_NAME`, value unchanged. The mail path main drains
is `main_mail` / `drain_main_mail` / `has_main_mail` / `take_main_mail_urgent` / `main_mail_len`, and
the room relay it formats is `format_main_line`. `EscLayer::BackToHub` → `BackToMain`,
`LineSource::HubBatched` → `MainBatched`, `Chat::route_from_hub` → `route_from_main`,
`switcher::pin_hub` → `pin_main`, `flow_order`'s `push_hub_upto` → `push_main_upto`.

*`BufferId::Hub` stays, and its doc comment now says why.* The design doc's explicit ruling, and it
holds up under reading: home is the one buffer whose mechanics are genuinely different — it owns the
turn loop, it has no sequence to read to, `rehydrate` returns nothing for it, it is never closable,
and it sorts first by declaration order. A variant named for that is worth more than a variant named
to match its label, and the label is one `format!` away. `Decor::Hub` did **not** earn the same
exemption and became `Decor::Home` (with `FlowItem::hub` → `FlowItem::home`): it names a *rendering
property* — "this position is the home conversation's own two-speaker message" — and "home" is
precisely the property the design doc says survives the retirement. So "hub" appears in exactly one
identifier in the tree, at the one place that carries the paragraph explaining it.

*`hub-and-spoke` is kept, everywhere, unrephrased.* It names a **topology**, not a participant: main
may address any instance and any room it is in, a subagent only main and its own rooms, and that
shape is what the phrase means in every architecture text that has one. The v3 design doc itself
still uses it after declaring hub retired (conversation-model-v3.md:138), which settles it. Kept
verbatim in tools.rs, agents.rs, tool/agent.rs, guide.md and both READMEs rather than kept in some
places and rephrased in others.

*`/open` gains `main` and loses `hub`.* `resolve_target` had a hardcoded `eq_ignore_ascii_case("hub")`
branch; it now resolves `@main` and a bare `main` through the same sigil grammar every other target
uses — accepted, not required — while `#main` still reads as a room, so a room may carry that name
without shadowing the floor. `hub` is refused outright rather than kept as an alias: the completion
dropdown reads the registry (so it stopped offering the word the moment `label()` changed), and a
spelling that survives only in the parser is exactly the second name this batch existed to remove.
`/open hub` answers `no conversation called hub · /open lists what is open`, which is the same
refusal every unknown target gets and says the word is gone rather than merely unlisted.

*The word leaves what the model reads, too.* SUBAGENT_NOTE, CHANNEL_NOTE, `crew_note`, `hire_note`,
the ack-chase follow-up line (`[follow-up n/3] Main sent you message …`) and the `Agent` /
`SendMessage` / `AgentControl` descriptions all say main, so the address language in the prompt is
the address language in the bar — the same argument D98 made when it merged the speech tools. The
SUBAGENT_NOTE's opening gloss ("The main agent (the hub) spawned you") lost its parenthetical
outright: it existed to introduce the name the rest of the note used, and the rest of the note now
uses the name in the sentence.

**The wire was investigated and says nothing.** `src/json_events.rs`, `src/share.rs`,
`src/share_html.rs`, `tests/cli_black_box.rs` and `notes/gui-json-events-legacy-check.md` contain
zero occurrences of "hub", case-insensitive. The share document's channel rosters and message
senders are literal participant names — `share.rs:450`, `share.rs:484`, `share.rs:807` all read
`"main"` — because `HUB_NAME` has always been `"main"` and the projection has always written the
value, never the constant's spelling. `BufferId::label()` is a TUI function with no serializer on it.
So there is no compatibility divergence to document and none was introduced: an external consumer
sees byte-identical output before and after this batch.

**Consequences.**

- **Assertions rewritten to the new contract, never relaxed.** Every literal that named the bar's
  first entry, a rule, a completion candidate or a switcher label moved from `"hub"` to `"@main"` and
  still asserts equality. Seventeen test names renamed with their subject
  (`a_lone_hub_shows_no_bar` → `a_lone_console_shows_no_bar`,
  `the_hub_is_there_before_anything_else_is` → `main_is_there_before_anything_else_is`, and so on).
  Two assertions that the mechanical pass would have turned tautological were rewritten by hand
  instead: `"main is reserved for main"` became a sentence about what `claim_name` does, and
  `"the main is listed"` became `"@main is listed"`.
- **Three new assertions and one new test.** `an_id_names_its_conversation_in_one_vocabulary` now
  states `BufferId::Hub.rule() == "── @main ──"` literally rather than deriving it from `label()` —
  the point of a rename is the exact glyphs — and asserts no label contains "hub".
  `open_completes_from_the_registry` asserts the retired word is absent from the dropdown.
  `open_reaches_every_conversation_and_reports_the_ones_it_cannot` covers bare `main` resolving and `hub` being refused by
  name. `the_bar_opens_with_main_and_keeps_it_first` (new) checks the first bar entry reads `@main`
  and stays first after activity elsewhere and a switch away, and that the row contains no "hub".
- `/open`'s description for the home candidate reads **`the console`** rather than "the conversation
  with the model", so the completion dropdown, D100's directory row (`main · console · idle`) and the
  observation page's title use one word for the surface. It is the only candidate described by what it
  *is* rather than by what is waiting in it, and that was already true before the rename.
- 1471 + 13 tests before, 1472 + 13 after.
- `feedback-states.md` gains v1.69 and its stale header stamp (v1.65, three entries behind since
  D98) is corrected to match. The guide's two duplicated capability blocks, `README.md` and
  `README.zh-CN.md` flip the vocabulary wherever they describe the current UI; changelog and
  historical entries stay as written, including the ones that say "hub" about what used to be true.

**Named limits.**

1. *Two spellings survive in the tree, both deliberately.* `BufferId::Hub` (doc-pinned, with the
   reasoning in its doc comment) and `hub-and-spoke` (topology). A reader who greps for "hub" finds
   them and finds the paragraph that says why; a reader who greps expecting zero hits will be
   surprised, which is the cost of the design doc's ruling and not a defect of it.
2. *`hub` is unreachable, including from muscle memory.* Anyone who typed `/open hub` gets a refusal
   rather than a redirect. Judged correct — a retired word kept in the grammar is a second name — but
   it is a real, if small, one-time cost paid by existing users, and no deprecation path exists.
3. *Bare `main` now beats a room called `main`.* Under the old grammar a bare word preferred a channel
   when one existed by that name; the home conversation now takes a bare `main` first. `#main` still
   reaches the room, so the room is not unreachable, only un-defaulted.
4. *`README.zh-CN.md` is behind D98, and this batch did not fix it.* It still documents `notify_user`
   as a tool and `Channel` / `Post` as the room pair — both retired in D98, both already corrected in
   the English README. The vocabulary flip was applied to those lines because the rule applies to
   them; their *content* is a D98 sync debt and correcting it is that batch's call, not a rename's.

### D102. The silence contract: two endings for a turn nobody asked for

**Problem.** D98 gave main a second kind of turn and no rule about how to end it. A dispatch
finishing, an agent's `SendMessage`, a room going quiet — each of those wakes main into a turn the
user never typed into, and every one of those turns ended in prose, because prose is what a turn
ends in. So a run whose dispatch row already said `⎿ done · fixed the parser` bought a paragraph
saying the same thing again, and the console D98 made quiet filled back up with narration instead of
lifecycle lines. The design doc's ruling: *a digest turn ends either in prose — which renders in
@main as main speaking — or in a silent acknowledgement marker that renders as nothing.*

**The flush question, answered before anything was written.** The write-once doctrine says the marker
must never reach scrollback, not even for a frame, and the batch prompt allowed for two possible
mechanisms. The existing pipeline already has the guard: `message_static_settled` opens with `if
Some(i) == self.stream_msg { return false }`, prefix settlement is monotone, and `build_rows` puts
only the settled prefix in `doc.settled` — which is the only thing `flush_items` ever hands to
`InlineTerm::insert_history`. `stream_msg` is set in the `TurnStart` arm and cleared in `TurnEnd`, so
**the message a turn is streaming into cannot settle, and therefore cannot flush, until the turn is
over**; `streaming_content_is_not_flushed_until_settled` (chat_tests_b.rs) has pinned exactly this
since the inline rewrite. So the intervention is at settle: `TurnEnd` reads the answer once, before
`stream_msg` is cleared, and everything downstream branches on it. No transient-region holding was
needed, and the invariant is proved rather than argued —
`the_marker_never_reaches_flushed_scrollback` drives a turn frame by frame, flushing at the *last*
settled mark every frame (more aggressive than production, which waits for the window top), and
asserts that no captured row ever contains even a half-streamed prefix and that the cursor does not
move for the turn at all.

**Decision.**

*The marker is bracketed, not tag-shaped, and that was forced by the renderer.* `<quiet/>` is the
obvious spelling — the injected envelopes are `<messages>` and `<task-notifications>` — and it is
wrong here, for a reason that only shows up when the contract fails. Assistant text is rendered as
markdown; a bare `<quiet/>` alone on a line parses as an HTML block and `render_block` emits **zero
rows** for it. That is harmless where the marker is meant to vanish and fatal where it is not: the
same string at the end of a turn the user typed into is the model misfiring, and the rule says a
misfire must be *visible*. A tag would have made "renders literally" depend on a second special case
in the renderer. `[[quiet]]` renders verbatim in every position tested (alone, padded, mid-prose, in
a list item), keeps the marker in the family the delivery path already uses (`[DM from user]`,
`[message from @scout]`, `[follow-up n/m]`), and is doubled so the single-bracket link-reference
syntax — which turns `[quiet]` into `quiet()` — cannot claim it. One definition,
`query::QUIET_MARKER`, read by the prompt, the renderer and the projection.

*Tag, don't infer.* The digest fact is stamped in `submit_auto`, the one door a turn nobody submitted
comes through, into `Chat::digest_turn`; `TurnStart` takes it with `mem::take` and writes it onto the
reply as `UiMessage::digest`. Three reasons for that shape rather than reading the empty prompt at
render time: an empty user submission and a woken turn are different facts that happen to produce the
same prompt string today; `mem::take` means a stamp cannot outlive the turn it was set for, and
`busy` is latched between the two points so no other turn can read it by mistake; and putting it on
the *message* rather than on `Chat` means it survives `/clear`, rewind and the transcript pager
without a single invalidation rule. `open_continuation_message` copies it from the message it closes,
because a continuation is the same turn.

*One predicate, and it asks both halves.* `Chat::is_quiet(i)` is `digest && text.trim() ==
QUIET_MARKER`, and `flow_order` — the single answer to what the message store looks like on screen —
never gives such a message a position. That is the whole render rule: no block, no rows, and so
nothing for the settled prefix or the flush cursor to see. Append-only survives it, because the only
message that can answer true is the one the stream is writing, which by the paragraph above cannot
have flushed; every position it would shift is above the cursor. Suppressing at `flow_order` rather
than deleting the message is also what keeps `excursions[].at`, `exc.rows[].index` and `stream_msg`
valid — a `Vec::remove` in the middle would corrupt all three, and the stream message is genuinely
not always last (a failure alert or a mid-turn conversation switch pushes after it).

*The accounting is three refusals, not one.* A quiet turn does not call `note_console` (which carries
the unread, the mention accent **and** the conversation's `last_activity` — one call, three
consequences, so the guard has to be in front of it), does not ring `Attention::TurnComplete` even
when the turn ran past `LONG_TURN` (the user never walked away from a turn they never started), and
does not arm the D87 settle blink: nothing settled, and an armed blink would hold the *previous*
message live and re-accent a completion row that finished minutes ago. A prose digest turn keeps
D99's behaviour byte for byte.

*Tool calls go with it — the preferred ruling, taken without a fallback.* A digest turn's calls are
activities *on the streaming message*, so suppressing the message suppresses its work for free, and
the same `stream_msg` guard means none of those rows could have flushed either. The work is in the
record and the dispatch row already said the run was done; repeating it on screen is the narration
the contract exists to stop.

*The record keeps what the flow drops.* The marker is written to the transcript by `record()` in the
query layer, untouched by any of this. On main's observation page it would otherwise land in the lane
of whichever agent happened to have woken main — raw protocol inside a conversation — so `walk` files
an assistant text block equal to the marker as a `TimelineOnly` `Note`, verbatim: the same place
`runtime_only` puts every other piece of scaffolding, and the symmetric arm to it.

*The contract is main's alone, enforced by a drop.* It is a system block (`system::DIGEST_HEADING`,
`# Digest turns`) built second in `build_system`, where it reads as a continuation of the base
prompt's turn behaviour. But a subagent assembles from `parent.system.clone()`, so "main-only" cannot
be a matter of where it is pushed: `build_sub_session` does
`system.retain(|b| !b.text.starts_with(DIGEST_HEADING))` before appending `SUBAGENT_NOTE` — the same
find-by-heading trick `with_model_capabilities` already uses. Nothing wakes a subagent with an
injected notification, and an instance taught a marker that renders as nothing could only use it to
disappear its own report.

*No reminder inside the envelope, deliberately.* The batch left it to judgment and the answer is no,
on evidence: both `<task-notifications>` and `<messages>` are rebuilt and `record`ed **every round**
of every turn, so a reminder line becomes per-round repetition in the transcript and in the cached
prefix; and `perspective::split_user_text` recognises the mail envelope by exact `strip_prefix` /
`strip_suffix`, so a line after `</messages>` breaks D98's attribution outright while a line inside it
gets filed as a message by `line_source`. The system prompt is the contract, and it is static, cached
and read on every turn.

**Consequences.**

- `README.zh-CN.md`'s D98 debt, named as limit 4 of D101, is paid: `notify_user` leaves the tool
  table, `SendMessage` and `AgentControl` become two rows with the real addressing rules,
  `Channel` / `Post` becomes `Channel` (room management), the sub-agent section's "反方向只有一个
  工具" paragraph is rewritten around addressing, and the channels section says
  `SendMessage(to: "#房间")`. The register is the file's own; this is a translation catching up, not
  new prose. Both READMEs and both of the guide's capability blocks gain the contract itself.
- 1472 + 13 tests before, 1485 + 13 after (13 added, none removed, none weakened). Two in `system.rs`
  (the contract says all four of its things; exactly one block carries the heading), one in
  `tool/agent.rs` (a spawned instance does not inherit it, and inherits everything else), one in
  `perspective.rs` (the marker is a timeline note, not speech in a lane), nine in `chat_tests_f.rs`
  as its fourth part.
- `build_system` gained a doc comment saying it builds the *main* session's blocks, because that is
  now load-bearing rather than incidental.

**Named limits.**

1. *The contract is model discipline, and nothing enforces it.* A digest turn that ends in
   `[[quiet]] done!` renders as prose, which is the safe failure; a turn that says nothing and
   emits no marker renders as an empty assistant message, which is the same blank the pipeline has
   always had for an empty reply. The floor is what the design doc named: the dispatch row is always
   visible, `@main` badges, failures alert, the chase machinery still runs.
2. *A quiet reply stays in `Chat::messages` forever, invisible.* Cheap (one `UiMessage` with a short
   string per digest turn) and deliberate: suppressing at `flow_order` is what keeps every index into
   the store valid. A session of thousands of digests would carry thousands of hidden messages.
3. *The `ctrl+o` transcript pager is a flow view, so it is quiet too.* It renders through
   `Chat::build_rows`, so the marker is absent there as well; the complete record is the session
   transcript on disk and main's observation page, both of which keep it.
4. *Only the TUI has digest turns.* `submit_auto` is a TUI method and the headless and JSON-events
   hosts have no tick loop, so the contract sits in the main session's prompt for a turn shape those
   hosts never open. Harmless, and it is the same asymmetry D98 documented for mail.

### D103. The single transcript, and one line that can leave it

**Problem.** Real use rejected v3's view layer. D89 made every conversation a *place* the terminal
could be pointed at, D90 gave the places a bar and a switcher, and the result was a program where
"where am I" was a question the user had to keep answering. The evidence tipped it: Claude Code has
no in-app conversation switching at all, and its answer to many agents in one terminal is one
transcript plus a status layer plus a temporary zoom. `notes/design/conversation-model-v4.md` is the
program; this is its first batch, and it is almost entirely a retirement.

**What went.** The conversation engine's *view* half: `switch_to`, `Excursion`, `flow_order`,
`FlowItem`/`Decor`, the replay budget and `replay_items`, `open_conversation` and its dividers, the
empty-pair note, `poll_active_conversation`, `send_to_active`, the live-tail call site, `/open` with
its resolver and its completion source, the `→ @name: …` route receipt and `is_route_receipt`, the
`tab` door (`open_conversation_record`), `EscLayer::BackToMain`, `Buffers::rehydrate`/`rule_for`/
`route_submit` and the drafts a switch stashed, `SubmitTarget::{Turn, Refused}` with `OBSERVER_HINT`
and `observer_hint`, `Delivery::Turn`, `BufferId::rule`, the whole of `convbar.rs` (600 lines) and
the whole of `switcher.rs` (546). D102's silence contract went with them, on the user's parity
ruling: `QUIET_MARKER`, `is_quiet`, the `# Digest turns` system block and the spawn-path drop that
kept it main-only, the projection's marker arm, and the `digest` tag on `UiMessage` — checked for a
second consumer, which it did not have, so the tag went too. In code: **1124 added against 5009
removed** — **−3885** — across 25 source files, two of them deleted whole. `chat.rs` fell from 3948
to 3885 of its 4000-line cap and `chat_tail.rs` from 3483 to 3362.

**`build_rows` got shorter, which is the proof the retirement was real.** The flow was a projection
of one store through `flow_order`; it is the store, in order. The loop lost the divider arm, the
sender-name row, the `Decor` match and the per-frame question of which conversation the gutter
belongs to. `speaker_of` went from five arms over a `FlowItem` to three over a role — the
transcript has two participants and they are never written down anywhere, which is exactly what D99
said when it gave the console a gutter. Segment numbering is unchanged in meaning and simpler in
fact: flow positions *were* message indices for every session that never left `@main`, and now they
always are, so `flushed_segments` keeps meaning what it meant and write-once is untouched.

**The composer keeps exactly one way to address somebody else, and it is CC's.**
`parseDirectMemberMessage` (2.1.88 `utils/directMemberMessage.ts`) is `/^@([\w-]+)\s+(.+)$/s`, read
at submit, ahead of the empty check and ahead of the suggestion guard; on an unknown recipient
`sendDirectMemberMessage` answers `unknown_recipient` and `PromptInput.tsx:1055` falls through to a
normal prompt with the comment *"This allows e.g. `@utils explain this code` to be sent as a
prompt"*. bingo's D90 line-leading routing was already this shape, so the batch kept the placement
(below `/` and `!`, above `busy`) and changed three things: the target resolves against the **domain
registries** rather than the accounting store, because an agent spawned two frames ago is already
addressable and the store is refreshed on a poll; `#room` **joins first** when the user is not a
member, which is the v3 ruling about speaking being participation and is why the domain writes the
membership line where every member sees it; and the receipt moved off the flow.

*The receipt is transient, and that is the whole difference from D90.* CC posts a 3s notification
reading `Sent to @scout`; bingo puts the same sentence on the slash-info tier, which lives until the
user acts. The old `→ @scout: look at…` was a `UiMessage` with a state-line predicate behind it — it
settled into scrollback, it needed `is_route_receipt` in two renderers, and a session of ten
deliveries carried ten lines of envelope in a transcript that never sent them. Nothing was said to
the model, so nothing belongs in the model's history *or* in the record of it.

*The sigil reads differently at the start of a line, and only there.* `mention_token` returns the
sigil now, `#` opens only at offset zero, and `gather_mentions` takes the position. At line start
`@` lists **every** instance with `send message · running` / `· idle` / `· stopped` — stopped
included, because a message resumes one (CC `SendMessageTool.ts:808-866`, and already bingo's
deliver path), so a list that hid them would refuse to offer something the send can do — and `#`
lists the rooms with `post to room`, plus `· joins you` where the user is not a member, which is the
one thing about the grammar a user could not guess.

**One deviation from the brief, and the reason.** The brief said line-start `@` offers agent names.
It offers agent names *and the project files under them*, because the two grammars do not collide:
the send fires only on a name that resolves to an instance, so `@src/lexer.rs why does this loop?`
is an ordinary prompt that happens to start with the file sigil, and dropping files there would take
a reference away to settle a conflict that does not exist. `#` at line start is rooms and nothing
else — that sigil has one meaning. Mid-line both are unchanged: `@` is D85's file-and-agent
reference (running only, no note), `#` is a hash in a sentence.

**Keys and doors, settled.** `ctrl+k` is readline's kill-to-end-of-line again — D90 spent it on the
switcher and moved the kill to `alt+k`; the switcher is gone and a dead key is worse than a
duplicated one, so `alt+k` stays an alias and nobody loses a binding. `Enter` in the `ctrl+b`
manager's detail and in the `ctrl+t` directory is **unbound**: both opened a conversation and there
is no conversation to open, and D105 says Enter means *zoom*, so binding it to something else for
one batch would be a promise to break. The record is still one key away from both — `tab` in the
detail, `o` in the directory — which is why nothing was invented to fill the gap.

**What survives untouched, and what survives unused.** The domain and delivery layer is
byte-identical: registry, inbox, `deliver`, `deliver_post`, `SendMessage`, channels, wake and
debounce, the D98 failure alert, the D96/D99/D100 projection. `Buffers` survives as the **accounting
store** — `observe`/`refresh`/`note_console`/`mark_read`, unread and mention and `last_activity` per
`BufferId` — and its readers are marked `#[allow(dead_code)] // D104 consumes these` rather than
deleted, because D104's footer pills and agent tree are the surface that reads them. Four more
pieces carry `// D105 consumes this`: `Buffers::set_active` (the zoomed conversation is the one being
read), `Replay` and `pair_replay` (the zoom's body), and `conversation_tail_el` with `dm_state` and
`tail_post_rows` (the zoom's live tail, whose D99 filters — the user's own in-flight messages, the
user's own run — are the part that was hard to get right). Everything else that fell out of use was
deleted.

**Old assertions rewritten to the new contract, never relaxed.** **Eighty-nine tests were deleted
with the machinery they pinned** and none was weakened on the way out: 17 with the two files that
went whole (`convbar.rs` 8, `switcher.rs` 9), 30 in `bufferview.rs` (the switch and its rule, the
replay budget, the draft round trip, switching to the conversation you are in, main's held tail, the
append-only print order, the empty-pair note ×2, the tab door ×3, the DM and channel submits, the
observed room, the join announcement, the slash-through-a-conversation path, the live tail ×2, the
arrivals poll, the leading-name routing ×5, `/open` ×2, the DM face run), 11 in `buffer.rs`
(`rehydrate` ×7, the submit router ×2, the drafts ×1, the observer refusal ×1), 9 in `chat_tests_f.rs`
(D102's whole part), 6 in `chat_tests_c.rs` (Esc going home, Esc peeling the conversation, Esc
peeling the switcher, the ctrl+k switch, the dialog inside a conversation, the DM-does-not-steer
pair), 4 in `chrome.rs` (the teammate tint, the bar ×2, the switcher overlay), 2 each in `app.rs`
(scroll on switch), `directory.rs` (Enter's two destinations), `keys.rs` (the panel's door
inventory) and `system.rs` (the contract's wording and its heading), and 1 each in `tool/agent.rs`
(the spawn-path drop), `perspective.rs` (the marker as a timeline note) and `transcript.rs` (the
pager covering excursions).

**Eleven of those were rewritten rather than dropped**, each because the thing it was really about
survives. `the_console_counts_what_main_says_while_you_are_elsewhere` drives the accounting store
directly instead of through `switch_to` — the store was always what it was testing.
`a_dm_submission_never_steers_mains_turn` → `a_direct_send_never_steers_mains_turn`: same question,
new door. `ctrl_k_switches_and_alt_k_kills` → `both_kill_keys_kill_to_the_end_of_the_line`.
`enter_opens_a_member_dm_and_a_room` → `the_cursor_walks_destinations_and_enter_opens_nothing`,
which *states* the new contract rather than deleting the old one, and `the_footer_names_the_record_door`
now asserts the footer no longer promises the key that was unbound.
`mention_lists_running_agents` → `mention_lists_agents_by_what_the_position_can_reach`, which pins
**both** readings of the sigil, so the line-start change cannot silently take the mid-line one with
it. `an_id_names_its_conversation_in_one_vocabulary` kept every label assertion and lost only the
retired `rule()`. `the_bottom_states_compose_around_the_bar` → `…_around_the_composer`, asserting
the bar is *absent*. `running_agents_leave_the_arrows_to_history` walks ctrl+b → Enter → `tab` and
asserts Enter opens nothing. `esc_peels_the_directory_in_the_slot_above_the_task_panel` and
`esc_peels_the_rewind_selector_one_stage_at_a_time` restate their neighbours in the shortened stack.
`a_digest_turn_that_speaks_still_speaks` → `a_woken_turn_renders_its_prose_as_main_speaking`.

**Twenty-five were added.** Eight in `bufferview.rs`: the send reaches the inbox as the user and
writes nothing into main's history, the receipt is transient and never a flow line, the room send
posts and joins first, an unknown name falls through to a normal turn, the grammar's edges as one
table (bare name, missing sigil, wrong case, `@main`, a newline body, an unknown room), a slash line
is never a direct send, the flow is main's own messages in order with no rule reachable, and `/team`'s
feed receipt (restored — it was collateral in the block that held the retired routing tests, and it
pins surviving behaviour). Four in `buffer.rs` for the machinery kept dead: `pair_replay` driven
directly rather than through the retired `rehydrate`, so the D99 run-folding rules D105 depends on
stay under test, plus `channel_posts`'s membership notes. Three in `chat_tests_f.rs` (a woken turn's
prose renders, the retired marker is ordinary prose, a woken turn counts on the console). Two each in
`chat_tests_c.rs`, `chat_tests_d.rs` (the room typeahead and what it says a post will do),
`directory.rs` and `keys.rs`, and one each in `complete.rs` (`#` opens only at line start) and
`chrome.rs`. **1485 + 13 tests before, 1421 + 13 after.**

**Named limits.**

1. *There is no way to view an agent's conversation until D105.* Accepted before the batch started
   and stated here as the cost it is: the pair view retired with the buffer it lived in, and the
   observation page — which is the *record*, not the conversation — is what is left. No stopgap was
   invented, because a stopgap would be a surface to retire again three batches later.
2. *`@main` is not addressable, and that is correct rather than a gap.* `main` is a reserved name no
   instance can claim, so `@main hello` resolves to nothing and opens an ordinary turn — which is
   what talking to main already is.
3. *The name matches exactly, case included.* `@Scout` is prose. Inherited from D90 and unchanged;
   the typeahead is where discovery belongs, and a case-insensitive match would make the parser
   guess.
4. *A direct send is invisible afterwards.* The receipt is gone at the next keystroke and nothing on
   screen remembers it, because the conversation that would have held it is not on screen. That is
   the interim state of limit 1, not a separate decision.
5. *The composer no longer tints.* D90 coloured the border and the `❯` for the teammate you were
   talking to; there is nobody on the other end of the transcript's composer but main. D105 gives the
   colour back to the zoom.
6. *`refresh_conversations` no longer sets `dirty`.* Nothing on screen reads the accounting store
   between this batch and D104, so a sweep cannot change a row; the fifteen-tick poll now costs a
   registry read and no repaint. D104 restores the fingerprint when it has a surface to repaint.

### D104. The status layer: a tree that says who is working, and a line that says it in one row

**Problem.** D103 left the terminal with one transcript and nothing else. That was the right
retirement and it cost the screen its only answer to "who is working": the bar was gone, the
directory answered a different question two keys away, and an agent spawned three turns ago existed
only as a row in scrollback that had already scrolled. CC's answer to many agents in one terminal is
not a switcher — it is **one conversation plus a persistent status layer** — and this batch is the
status layer.

**What the tree is, and where it lives.** `src/tui/tree.rs`, 1053 lines, one new module: `@main`
first, then one row per registry instance in name order, then a `hide` row while the cursor is in
it. It is **chrome** — rebuilt from `session.agents.list()` on every draw, never settled, never one
byte of scrollback (`term.rs`'s doctrine is not a habit here, it is the reason the panel can carry a
second counter at all). Nothing is cached, for the directory's reason: a dozen rows drawn only while
the panel is open, and a cached roster is a roster that can be wrong.

```
    ╒═ @main: Idle · shift + ↑/↓ to select
    ├─ @scout: reading src/tui/chat.rs… · 12 tool uses · 8.3k tokens
   ❯╞═ @writer: Idle for 14s · shift + ↑/↓ to select
    │   drafted the section
    │   and checked the numbers
    ├─ @zoe: cargo test --all… · 1 tool use · 143k tokens
    └─ hide
```

Every glyph in that block is CC's. The stem is `┌─ ├─ └─`, doubling to `╒═ ╞═ ╘═` on the highlighted
row (`TeammateSpinnerTree.tsx:69`, `TeammateSpinnerLine.tsx:83`); the cursor is `figures.pointer` in
column four with a space in its place otherwise (`TeammateSpinnerTree.tsx:57`); the separator is
` · ` and never a dash; the stats read ` · 12 tool uses · 8.3k tokens`, singular on `use` and never
on `tool` (`TeammateSpinnerLine.tsx:130`); the select hint is `shift + ↑/↓ to select` with the spaces
around the `+` (`teammateSelectHint.ts:1`); the closing row is the bare word `hide`
(`TeammateSpinnerTree.tsx:244`). The status column is CC's ladder too — `[stopping]` first,
then `Idle for 14s`, then the activity with an ellipsis (`TeammateSpinnerLine.tsx:171-194`) — and so
is the responsive fold, which drops the select hint before the stats and never lets the activity
column below 25 cells (`TeammateSpinnerLine.tsx:141`, `:151-153`). Every segment goes through one
`push_fit`, so a row cannot overrun the canvas whatever the arithmetic above it decided; a test walks
20/40/60/80 columns and asserts it.

**Selection is state, and that is the whole safety property.** `AgentTree` holds one field. `None` is
not row zero — it is *not selecting*, which is what `ctrl+t` opens the tree into, and while it holds
every key still belongs to the composer. The index space is CC's exactly
(`useBackgroundTaskNavigation.ts:26-58`): `-1` is `@main`, `0..n-1` the instances, `n` the hide row,
wrapping at both ends. `shift+↑/↓` with the tree **closed** opens it and parks on `@main` without
moving — CC's comment says so in as many words — and with it open steps and wraps. `k` fires only on
a selected instance row, through `stop_agent_from_manager`, so there is one stop path, one warning
and one watch transition; **no confirmation**, which is CC's ruling
(`useBackgroundTaskNavigation.ts:228-241`), and `k` with nothing selected is a character in the
draft. `@main` has no `k` (index `-1` is excluded upstream and main is not stoppable here anyway) and
the hide row indexes past the end, so both are no-ops rather than special cases. `esc` is a new
`EscLayer::AgentTree`, slotted where the directory's was — immediately above `TaskPanel`, the panel
it cycles with — and it **peels twice**: the cursor first, the panel second. The first half is CC's
(`:166-175` leaves selection mode and leaves the tree expanded); the second is D80's one-press-one-
level rule, and without it the panel would have had no exit but the key that opened it, because
`enter` is unbound.

**`enter` stays unbound, and two strings were left out to keep that honest.** CC's rows offer
` · enter to view` and ` · enter to collapse`; D105 is what Enter means, so a row that promised it now
would be a promise to break in one batch. Same interim ruling D103 made for the directory and the
manager's detail.

**`ctrl+shift+o` previews the record, not the activity feed.** CC's `getMessagePreview`
(`TeammateSpinnerLine.tsx:26-70`) walks the teammate's *messages* newest-first, taking text lines
from the end of each block and a tool call's own `description`/`prompt`/`command`/`query`/`pattern`
before falling back to `Using <name>…`, cuts each to 80 columns and reverses the three into reading
order. That is ported verbatim over `agents.view_of`, and the choice matters: `recent_activity` is
the same string the row already shows in its activity column, so sourcing the preview from it would
have printed the row three more times. Three lines, dim, hung under the stem (`│  ` or `   `). The
binding is CC's (`defaultBindings.ts:48`, `app:toggleTeammatePreview`), judged before the plain
control keys so `ctrl+o` keeps the transcript; on a terminal without the kitty keyboard protocol
both arrive as `0x0F` and only the transcript is reachable, which is stated rather than papered over.

**The pills take the row D103 vacated.** `@main @scout @writer · shift + ↓ to expand` — `@main`
first and never sorted, running instances ahead of resting ones, the separator one space, the tail
verbatim from `BackgroundTaskStatus.tsx`. Identity colours, dim where an instance is idle or stopped,
`→` where the window is too narrow to name them all (CC scrolls a window with `←`/`→`; with no pill
selection to chase, the window is always left-anchored, which is CC's own behaviour at index 0).
Pills and tree are **exclusive**, which is CC's gate and its reason: the tree says everything a pill
would. Bold is wired to `Chat::zoomed()`, a method that returns `None` and is marked `// D105
consumes this` — the one place the zoom has to fill in for the pills, the tree's stem and the tree's
highlight all to follow.

**No badge, and the accounting store is re-marked rather than consumed.** The brief said the
`Buffers` readers survived D103 "exactly to feed this". They do not, and CC is why: `AgentPill`
renders `@name` and nothing else, and a tree row is `@name: <what it is doing> · <what that cost>` —
no counter, no dot, no accent. Both surfaces take their dim and bold from the agent's **state**,
which is the registry's answer, not the store's. So `Buffer::{id,unread,mention,last_activity}` and
`Buffers::iter` moved their markers to `// D107 absorbs these`: the background dialog is where "three
unread from @scout" is the question being asked, and it is the surface absorbing the directory those
numbers already fed. D103's limit 6 stands for the same reason — `refresh_conversations` still does
not set `dirty`, because the tree reads the registry directly. It keeps its own clock instead, in
`has_dynamic_rows`: an open tree with anybody on it counts seconds, so `Idle for 14s` ticks.

**Two additions to the task panel, both display-only.** ` (@scout)` in the owner's identity colour
when the owner is still on the roster — an owner who was stopped or never existed is simply not
named, which is CC's `ownerActive` gate (`TaskListV2.tsx:268`) — and ` › blocked by #3` listing only
the blockers whose tasks are not done, with the blocked row dimmed (`TaskListV2.tsx:322`, `:334`).
The glyph is `figures.pointerSmall`, `›`, not the `▸` the brief guessed; CC wins on presentation.
`TodoItem` grew `id`, `owner` and `blocked_by` to carry the answers, because a row cannot state what
the snapshot did not bring. **No assignment protocol and no claiming**, which is the design's
explicit ruling and is why nothing here writes.

**The directory left the cycle and has no door at all.** `ctrl+t` is `none → tasks → agents → none`,
collapsing to `none ↔ tasks` with an empty roster — CC's cycle exactly
(`useGlobalKeybindings.tsx:65-86`). `directory.rs` stays whole: its rows, its keys and its Esc layer
are kept for D107's dialog, and only `open_directory` lost its caller and wears
`#[allow(dead_code)] // D107 absorbs this`. **The interim record door was verified and still works**:
`ctrl+b` → Enter (list) → `tab` (detail) opens the agent's perspective page, under test in
`chat_tests_b.rs`, and the manager's detail footer stopped advertising `Enter opens DM`, which D103
unbound and left in the copy.

**`/team` had to be repaired, not just re-worded.** D95 filed its answer in the lifecycle feed and
printed `→ team (ctrl+t)` pointing at the key that opened the column. Take the key away and the
command answers into a room nobody can enter. So the lines print on the info tier — the tier every
other slash command answers on — and the feed entry stays, because that is what D107's dialog will
show. Its test is rewritten to assert the tier and the feed carry the same text, which is a stronger
claim than the pointer's wording was.

**Deviations from CC, each with its reason.**

1. *The leader row is `@main`, not `team-lead`.* v4 forbids the teammate vocabulary outright, and CC
   itself calls the same entity `@main` in its footer pills — so this picks the one of CC's two
   strings the design mandates rather than inventing a third.
2. *Main's row always states its own condition* (`: Idle` / `: <verb>…`). CC shows it only while
   something else has the screen, because `Spinner.tsx:231-237` prints `✻ Idle · teammates running`
   above the tree otherwise. bingo has no such row when idle, so the state lives on the row that has
   the name on it.
3. *Stopped instances stay on the roster*, rendered `[stopped]` in the slot CC gives `[stopping]`.
   bingo's stop is synchronous, so there is no stopping state to show — and a stopped instance is not
   gone: the registry keeps it and `@name <message>` resumes it, which is exactly why D103's typeahead
   lists it. A roster that hid what the composer can reach would disagree with the row below it.
4. *No per-row spinner glyph.* The brief asked for one through D87's motion layer; CC's rows have
   none (`TeammateSpinnerLine.tsx:191-193` emits dim text and nothing else) because one spinner on
   screen is the rule. CC wins.
5. *The stats segment is empty at zero.* CC prints it unconditionally on an instance row, but its
   progress is the teammate's whole life where bingo's is the current **run**, so every idle row
   would have carried `0 tool uses · 0 tokens`. The gate is CC's own, taken from its leader row
   (`TeammateSpinnerTree.tsx:111`) and applied one row down.
6. *`1k`, not `1.0k`.* `context_usage::compact_tokens` was generalized with a threshold parameter
   rather than duplicated — the footer's meter keeps compacting from 100k and the tree compacts from
   1k, one rounding rule between them. The trailing `.0` is dropped, which is CC's own `formatTokens`
   convention (`format.ts:133`) where its `formatNumber` keeps it.
7. *`shift+↑/↓` is left alone with an empty roster.* CC swallows it; bingo's arrows belong to the
   composer, and a key that visibly does nothing is worse than a key that does what it always did.
8. *`[awaiting approval]` and the all-idle past-tense verb are not ported.* Both belong to the
   teammate machinery — plan approval and the always-alive idle loop — that v4 names as explicitly
   not copied.
9. *The pills sit on the window's last row*, where the conversation bar was, rather than stacked
   above the footer's other parts as CC stacks them. Same region; bingo's footer row is a single
   composed line rather than CC's byline, so there was nothing to stack them on top of.

**Tests: sixteen added, five rewritten, none deleted and none weakened.** Twelve new in `tree.rs`
(the row shape and the hide row's arrival; what a running, idle and stopped row says; the idle row's
copy and the duration formatter; the selection walk and its wrapping; the empty roster leaving the
chord alone; `k`'s four cases; the preview walk; the rows a preview hangs; `ctrl+shift+o` not taking
`ctrl+o`'s meaning; the pills' content, order and exclusivity; the pills windowing; the fit at four
widths).
One in `chrome.rs` (the tree takes the task slot, the pills take the last row, never both). One in
`chat_tests_b.rs` (owner and blocked-by, including an owner nobody answers to and a blocker already
done). One in `chat_tests_c.rs` (`an_empty_roster_leaves_ctrl_t_a_two_stop_toggle`). One in
`keys.rs` (`the_panel_names_the_status_layer`). The five rewritten, each because the thing it was
about survives with a new occupant: `ctrl_t_cycles_the_tasks_then_the_team_then_away` →
`…_then_the_agents_then_away`, `esc_peels_the_directory_in_the_slot_above_the_task_panel` →
`esc_peels_the_agent_tree_…` (which keeps both adjacency assertions and adds the two-stage peel),
`the_panel_names_no_retired_surface` (which gained `team directory` to the list rather than losing
anything), `team_output_lands_in_the_feed_and_says_where` →
`team_output_lands_on_the_info_tier_and_in_the_feed`, and `task_lines_use_checkbox_glyphs` (unchanged
assertions, new fields on the fixtures). **1421 + 13 before, 1437 + 13 after.**

**Named limits.**

1. *`enter` still opens nothing, anywhere.* The tree's hide row is selectable and inert, and that is
   deliberate: it is the stem's closing corner and the selection's terminus, and D105 is what gives
   it a verb.
2. *`ctrl+shift+o` needs the kitty keyboard protocol.* Elsewhere the chord is `ctrl+o` and opens the
   transcript, so the preview is unreachable there. No fallback binding was invented; a second key
   for one feature is how a keymap rots.
3. *The team directory is unreachable.* Not deprecated, not deleted — 663 lines with their tests
   still green, waiting for D107's dialog. `/team`'s answer moved to the info tier so the command
   still answers in the meantime.
4. *The pill window is left-anchored.* CC scrolls it to keep a selected pill in view; bingo's pills
   have no selection until D105 gives them one, and at index 0 CC's own window starts at the left.
5. *No badge anywhere in the status layer.* The unread and mention counts are still measured on every
   poll and read by nothing. That is the honest state, marked as such, and D107 is the surface for it.

### D105. The zoomed view: the screen becomes one agent, and gives itself back

**Problem.** D103 left one transcript and D104 put a status layer around it, and between them they answered
"who is working" without answering "what is it *saying*". An agent's own conversation had no surface at all:
its record was two keys away on the observation page, the tree's `ctrl+shift+o` preview showed three
condensed lines of it, and D103's own limit 1 named the gap as the price of the retirement. This is the batch
that pays it back — CC's third surface, the one that swaps the screen to one agent and swaps it back.

**In code: 2573 added against 658 removed across 19 files**, one of them new (`src/tui/zoom.rs`, 1553 lines
including its tests). Everything removed is machinery a shorter road made unnecessary; the list is below.

**Why the alternate screen, and what that buys.** The inline host prints each settled row into the terminal's
scrollback exactly once and never touches it again (`term.rs`); a view that *replaces the body* cannot exist
there, and every attempt to build one would be an attempt to un-print. So the zoom takes the road the
transcript pager (D82) and the perspective page (D96) already take — a self-driving alt-screen loop that owns
the terminal while it is open, the same guarded enter, the same D77 panic-hook claim — and differs from both
in the two ways that matter: it is **live** (a ticker, not a blocking `events.next()`) and it has a **live
composer**.

**The write-once round trip, and precisely how it is guaranteed.** The riskiest seam of the batch was
inline-vs-alt: whatever the transcript would have printed while the zoom is open must be neither lost nor
printed twice. The guarantee is not a flush protocol, it is a **restriction on what the loop may write**. The
loop draws from the domain — `agents.view_of`, `agents.list`, `channels.log_of` — read fresh on every frame,
through functions that take `&self`. The only `Chat` state it mutates is: the composer draft (through
`Chat::on_key`, the console's own editor), the tree's cursor, the zoom pointer, the accounting store's active
conversation, and `chat.tick`. It never calls `build_rows`, never pushes a `UiMessage`, never touches
`flushed_segments`/`tail_start`/`mark_base`, and — deliberately — **never calls `Chat::tick`**, which drains,
debounces and polls on behalf of a transcript that is not on screen. So messages that arrive during a zoom
arrive after it, in one batch, in order, exactly as if the zoom had never happened; that is the same freeze
the pager and the perspective page already impose, with a redraw clock added on top. Two tests pin it: one
drives the loop's *whole* write surface (`enter_zoom`, six keys, a paste, a tick bump, `leave_zoom`) with two
segments already flushed and asserts the document rebuilds identically and the cursor did not move; the other
appends a message mid-zoom and asserts it prints once on the way out.

**What the body is: the whole record, through one projection and one renderer.** The user's ruling was
everything the agent saw and did, so the body is `walk(Protagonist::of(name), …)` keeping *every* lane —
the task that created it, main's instructions, the user's messages, room relays, reminders and chases, its
process, its answers — where `pair_lane` keeps one. Runs of collapsible tool calls fold through the console's
own `classify_tool` and `collapse_summary` into `⏺ Searched for 1 pattern, read 2 files`, and a call the
console does not collapse breaks the run and prints its own line, as it does in `@main`. Everything is drawn
by `settled_post_rows` and `zoom_post_rows` with `conversation_gutter` — the same builder the observation
page draws with, so a message looks like itself wherever it is read.

*Intake takes the furniture tier, which is a decision the single-column view forced.* `walk` files a spawn
prompt as a `Said` post with an empty sender, which the perspective page can afford because it has a lane
called `intake` around it. Rendered into one column that post arrived wearing an avatar and a `⏺`, as though
the agent had said the thing it was asked. It is a `Note` here: dim, one line, no face, no name.

**`Replay` and `pair_replay` were not consumed — they were retired, and the fold moved.** D103 kept them for
this batch on the belief that the zoom would print `UiMessage`s. It cannot: the console's message renderer
(`assistant_el`) is indexed into `Chat::messages` and threaded through the streaming state, so printing a
replayed message would mean either pushing it into the transcript's own store — the thing this whole batch
exists not to do — or a second renderer beside the one D96/D99 spent two batches making singular. So the
same fold now produces a `Post`, `zoom_posts` replaces `dm_posts` (whose last reader went with the pair view),
`PairPost` collapses to `Post` and `pair_lane` returns the lane itself. **`dm_posts` (90 lines), `Replay`,
`pair_replay`, `push_work`, `blank_message`, `interned_tool`, `PairPost` and `conversation_tail_el`'s
settled-vs-all diff are gone**; the diff went because an alt screen redraws whole and has no printed prefix
to subtract. `Buffers::active`/`set_active`, `conversation_gutter` and `tail_post_rows` (now `zoom_post_rows`)
*were* consumed, and their `// D105 consumes this` markers came off.

**CC, key by key.** The header is `TeammateViewHeader.tsx:31-70` verbatim: `Viewing ` plain, `@name` bold in
the identity colour, ` · esc to return` dim, the task prompt dim on its own line, one blank row under it
(`marginBottom={1}`). `esc` is `useBackgroundTaskNavigation.ts:151-165` — running aborts the current turn
only and stays (CC's `currentWorkAbortController`, not its `abortController`; bingo's `registry.stop()` is
exactly that: turn aborted, history kept, instance on the roster), otherwise it returns. `shift+tab` is
`PromptInput.tsx:1410-1447`, which cycles the **teammate's** mode through `getNextPermissionMode` and returns
before any leader-side effect; `PermissionMode`'s ladder was made a free function so both subjects walk one
ladder. The mode shows where CC shows it, in the footer, swapped for the viewed agent's
(`PromptInput.tsx:342-351`). `enter` on the tree is `:206-225`: leader → leave the view, hide row → collapse
the panel, instance → open it; and it is gated on selection mode, which is why `enter` with a merely-open
tree is still the composer's. Typing routes at `PromptInput.tsx:1086-1097` → `REPL.tsx:3548-3578`, which has
**no `/` branch at all** — slash and `!` lines go to the teammate as text, and here that is true by
construction because nothing but the zoom's own submit reads this draft. Auto-return is
`useTeammateViewAutoExit.ts:35`; `:8-9` says in as many words that a *completed* teammate's view stays open.
The two `enter to …` strings D104 held back are back with the key that earns them, on CC's gates
(`TeammateSpinnerLine.tsx:134/151`, `TeammateSpinnerTree.tsx:128/253`): the selected row only, and only where
it is not already the row on screen.

**Divergences from CC, each with its reason.**

1. *`shift+↑/↓` inside a zoom moves the view.* CC's handler steps the tree and flips back to `selecting-agent`
   (`:181-189`, `:26-59`), which drops you out of the zoom into selection while the transcript still shows the
   agent — because CC's tree is on screen underneath. bingo draws the tree in the zoom too, and it does exactly
   CC's thing **while the tree is open**; with the tree closed there is no cursor to move, so the chord walks
   the roster and the view follows. Landing on `@main` is not in that ring: `esc` is the way out, and a chord
   that could fall out of the view by one press would be a trap.
2. *The body is the whole record, not a 50-message tail.* CC caps `task.messages` at
   `TEAMMATE_MESSAGES_UI_CAP = 50` for memory (`types.ts:47`, `:88-99`: 36.8 GB in a 292-agent session) and
   drops the rest with no marker at all. bingo reads the registry's history live rather than mirroring it into
   UI state, so the cap buys nothing here — and the user's ruling was the full transcript.
3. *`ctrl+c` is not the kill-everything-and-exit key.* CC binds it to `killAllAgentsAndNotify` + exit while
   viewing (`useCancelRequest.ts:190-203`). bingo's `ctrl+c` is the interrupt/double-press-to-quit contract
   the whole app is built on, and rebinding it inside one view is how a quit key becomes unpredictable. What
   the view owes it instead: the window it arms is announced on this screen too, and the loop breaks on
   `chat.exit` so the second press quits at once rather than on the way back out.
4. *Five chords are inert here.* `ctrl+o`, `ctrl+b`, `ctrl+t`, `ctrl+g`, `ctrl+r`. CC leaves them all bound
   and its `ctrl+o` opens the **leader's** transcript from inside the teammate view; in bingo those keys set
   flags the *host* consumes, so pressing one would do nothing visible and then spring a modal the moment you
   pressed `esc`. A key that does nothing beats a key that does something later.
5. *A room zoom's footer advertises only `esc`.* It has no permission mode and no roster position, so the two
   other hints would name keys that do nothing there. Same reason D104 left `enter to view` off.
6. *No swarm banner.* CC replaces the composer's border with two coloured rules carrying an inverse-text
   `@name` label (`useSwarmBanner.ts:92-100`, `PromptInput.tsx:2250-2268`). bingo tints the border and the `❯`
   with the same colour instead: the name is already bold in the header two rows up, and a second label for it
   is the decoration D93 spent a batch removing.
7. *No `Message @scout…` placeholder* (`usePromptInputPlaceholder.ts:38-44`). bingo's composer placeholder is
   the `Try "fix a bug"` hint, which is a first-run affordance rather than an addressing cue; the addressing
   cue is the colour and the header.

**One correction to the record, and it is a real gap.** D103 wrote that D105's typeahead lists stopped
instances "because a message resumes one (CC `SendMessageTool.ts:808-866`, and already bingo's deliver path)",
and v4's member model says the same. **It is not bingo's deliver path.** `AgentRegistry::deliver` answers
`"<name> is stopped and no longer accepts instructions"` for any stopped instance, so the resume half of the
subagent semantics is unimplemented. That is a domain write path this batch is fenced out of, so nothing was
changed: the claim is corrected everywhere it was written (`tree.rs`'s status ladder, D103's typeahead test,
the guide, the README), the zoom reports the refusal on the warning tier rather than swallowing it, and a test
pins the current truth by name. The fix belongs to whichever batch is allowed to touch `deliver`.

**Tests: twenty-nine added, ten rewritten, one folded away, none weakened. Net +28.**

*Twenty-six new in `zoom.rs`*: the header's copy and its bold name; the body as the whole record; the live
tail following a run; a room's log; the send reaching the inbox as the user with nothing in main; the echo on
the next frame and no receipt; a stopped agent's refusal on the warning tier; a slash line arriving as text; a
room post joining first; `esc`'s two meanings and the tree cursor peeled before both; `shift+tab` scoped to
the viewed agent with main's and the other instances' modes untouched; `shift+↑/↓` walking the roster and
wrapping; `enter`'s three tree answers; `enter` belonging to the composer with nothing selected; the five
held-back chords arming nothing and typing nothing; an ordinary key still editing the draft; entry clearing
unread and mention and exit restoring the pointer; the auto-return's two halves — gone versus merely done —
for an agent and for a room; the composer's tint; the hint row's two forms and the room's third; and the round
trip in two parts; and `ctrl+c` keeping its own contract with the view standing aside. *One in `tree.rs`* (the inline host's `enter`, all four of its cases). *Two in `buffer.rs`*
(the live tail keeping every sender, and a run the user did not start — the two things the D99 pair filters
used to remove and a full-record view must not).

*Ten rewritten, each because the thing it was about survives.* Four in `buffer.rs`, the tests D103 wrote to
keep `pair_replay` honest, moved onto what replaced it — `the_pair_replay_says_who_said_what` and
`…says_what_dm_posts_says_about_the_same_history` **merge** into
`the_zoom_keeps_every_lane_and_the_pair_view_keeps_one`, which is a stronger claim than either (it pins the
*difference* between the two projections rather than their agreement); `the_pair_replays_work_as_activity_groups…`
→ `the_zoom_folds_work_the_way_the_console_folds_it`; `a_standalone_call_closes_the_group…` → `…_the_fold_…`.
Two in `perspective.rs`: `the_pair_lane_carries_the_work_of_its_own_turns` →
`the_walk_carries_the_call_and_not_only_the_line`, and `a_run_breaks_on_what_the_lane_does_not_show`, both
moved onto `walk` because that is where the claim lives now that `PairPost`'s `work` and `contiguous` are
gone. One in `chat_tests_b.rs` (`running_agents_leave_the_arrows_to_history`: `Enter` in the `ctrl+b` detail
opens the zoom, `tab` still opens the record, and the panel closes behind either — it used to assert Enter
opened *nothing*). One in `tree.rs` (`the_tree_leads_with_main_and_closes_with_hide`, which asserted the two
`enter to …` strings **absent** and now asserts them present on the selected row and absent everywhere else).
Two in `keys.rs` (`the_panel_names_the_status_layer` gained Enter and the viewing row;
`ctrl_b_help_names_both_of_its_meanings` gained its two doors). And one doc-only correction in
`chat_tests_d.rs`, where the typeahead's rationale said a message resumes a stopped agent — the assertions are
untouched, the reasoning is now true.

*Nothing was deleted that was not replaced*, and the one that vanished by name
(`the_pair_replay_says_what_dm_posts_says_about_the_same_history`) was folded into the merge above with its
question intact. **1437 + 13 before, 1465 + 13 after.**

**Named limits.**

1. *A stopped instance cannot be messaged.* See the correction above. It is the one place the view can be
   used and answer "no".
2. *The room zoom has no door.* CC has no rooms anywhere in 2.1.88, so there was no key to copy and no
   invented global binding was added. The view, its body, its send-with-join and its `esc` are implemented and
   tested; D107's dialog has a Rooms section and an `f`-to-zoom key (`BackgroundTasksDialog.tsx:290-299`) and
   is the door. Marked `// D107 opens this`.
3. *The task prompt shows twice* — once in the header, once as the first (dim, furniture) row of the record.
   CC has the same duplication for the same reason: the header names the task and the record contains it.
   Dropping the row would make the record incomplete, which is the one thing the user's ruling forbade.
4. *There is no zoom-side scrollback search.* `PgUp`/`PgDn` and the wheel, following the tail at the bottom —
   CC's set exactly (it inherits the global scroll handler and binds nothing of its own). `/` and `g`/`G`
   belong to the composer here, so the pager's search would have had to steal them.
5. *A mid-word wrap can still happen in the body.* It is the markdown renderer's, shared with the transcript,
   and not this batch's to change.
6. *`README.zh-CN.md` is a batch behind.* It has no status-layer section at all and still names the team
   directory's retired `o` door — both inherited from D104. Nothing D105 did made it newly false, and D108
   owns the rewrite.

### D105a. Review fixup: a message resumes a stopped instance

D105's finding, closed by the reviewer. v4's member model (the CC subagent
semantics the whole program replicates) says a message sent after a stop
resumes the instance; `AgentRegistry::deliver` refused one instead, and every
surface downstream had to carry the refusal as a named limit.

The fix is four lines where the refusal was: a delivery that finds the entry
`Stopped` flips it to `Idle` and lets the flush that already follows every
delivery respawn the run — the registry never dropped the instance's session
or history, so waking a stopped instance is literally the move `flush_pending`
makes for an idle one. Nothing else resumes: `follow_up` pushes chase items
without touching state and `deliver_channel` skips stopped members, so an
automatic retry or a room broadcast cannot undo a stop the user asked for —
only somebody addressing the instance on purpose can.

Swept with it: the `AgentState::Stopped` doc, the tree's status comment, the
guide's two grammar paragraphs and its named-limits row, feedback-states
v1.74, and the zoom's pin test — `a_message_to_a_stopped_agent_says_what_did_
not_happen` becomes `a_message_to_a_stopped_agent_resumes_it`, the same pin
facing the other way. Both READMEs already claimed the resume worked (the
D103-era text was written to the design, not the code) and are true now
without an edit.

### D106. What the transcript shows of agent life

**Problem.** D103 left one transcript, D104 put a status layer around it and D105 gave one agent the
whole screen for a while. Between them they answered who is working and what it is saying — but the
*transcript*, the surface a user actually reads, still showed agent life the way v3 left it: one
watch row per dispatch reading `◉ scout · fix the parser` with `⎿ produced 1234 chars` under it, and
then nothing at all. A message an agent sent main rendered **zero rows** (D98's ruling: the wake was
invisible, main narrated). A completion rendered zero rows. The design's tiering table is CC's answer
to the same question, row by row, and this batch is that table. **In code: 1362 added against 59
removed across 12 files**, none new — every tier lands in the module that already owned its
neighbour.

**The dispatch row is a name and a task, because CC's named spawns are.** The brief said
`⏺ Agent(<description>)`, which is CC's row for an *anonymous* subagent
(`AssistantToolUseMessage.tsx:200-210` composing `userFacingName` with the parenthesised
`renderToolUseMessage`, and `AgentTool/UI.tsx:411-421` returning the description). bingo has no
anonymous subagents: every instance is named and addressable, which is the whole of v4's member
model. CC has a row for that case too — `AgentTool/UI.tsx:687` makes a named spawn's `agentType`
`@name`, and `AgentProgressLine`'s `hideType` branch renders `<Text bold>{name}</Text><Text
dimColor>: {description}</Text>`. So the row is **`◉ @scout: fix the parser`**, which is also, letter
for letter, the shape D104 gave the tree — the same run now reads the same way in the flow, in the
panel and in the zoom header. The `◉` stays: it is D97's subagent glyph and the avatar gutter's
connector arithmetic is built on it, and CC's own dot is platform-dependent anyway
(`constants/figures.ts:4`).

*One bug fell out of the parse.* The face a dispatch row wears was keyed on everything before the
first ` · `, so a continuation run labelled `scout #3 · look again` wore **a different face** from
`scout · fix the parser`. `watch_instance` — the first whitespace token, which is what every label
shape opens with — is now the single answer for the row's text, the row's face and the notice line,
and a test walks the four label shapes and the gutter consequence.

**Live progress is the last three things the agent did, and it is not stored anywhere it could
settle.** CC keeps the tail of a subagent's progress messages (`MAX_PROGRESS_MESSAGES_TO_SHOW = 3`,
`UI.tsx:33`, `:510`) and renders each in condensed style. bingo's `recent_activity` entries already
*are* that line — `⏺ Read(src/lexer.rs)`, built by the same `tool_glyph` / `display_tool_name` /
`summarize_input` the console builds its own tool headers with — so the port is the tail, not a new
renderer. **CC's grouping of consecutive read/search calls is deliberately not ported**: its own
comment at `UI.tsx:501` marks that path *ants only*, so the shipped renderer prints the rows as they
come, and so does this one.

The short-window fallback is CC's too (`UI.tsx:469`, `:495-503`): when the terminal cannot hold the
rows, one `In progress… · 4 tool uses · 8.3k tokens` takes their place. The arithmetic is CC's —
in-progress dispatches × lines-per-dispatch + `TERMINAL_BUFFER_LINES`, the buffer verbatim at 7
(`:182`) — and the per-dispatch figure is bingo's own 4, because a dispatch row here is a header plus
at most three progress lines rather than a full tool rendering.

**Where the write-once line falls, and why it falls there by construction.** `message_static_settled`
asks `Activity::is_running`, and a dispatch row answers yes for as long as its run does. So a message
holding a running dispatch **cannot settle**, its rows stay in the redrawn tail, and the live progress
is therefore transient whatever it is stored in. That is the licence to keep it in the `WatchCall`
itself and refresh it on the tick — beside the thinking clock, which has updated a stored
`duration_ms` per frame since D87 for exactly the same reason. What reaches scrollback is the row the
terminal event leaves behind: `Done (12 tool uses · 8.3k tokens · 1m 4s)`, CC's completion line
(`UI.tsx:376-377`) built from D104's `stats_body` and `duration_label` rather than a second pair of
formatters. Two tests pin the pair: the running message does not settle and shows the three rows; the
finished one settles, shows the cost, and shows none of the rows it used to.

*The numbers had to be kept because the domain throws them away.* `spawn_agent_loop` calls
`set_progress(&name, None)` **one line before** it reports `Done`, so a renderer that read the
registry at the terminal event would read zeroes. The row keeps its own copy, sampled per tick and
merged with `max` — within one run the counts are monotone, so the merge is exact and immune to the
frame in which the cell went empty, and a new run gets a new label and therefore a new row. No domain
write was needed, which was the constraint: the `Done` detail string is model-facing (it is the body
of the task notification, `watch.rs`'s `notification_body`) and was not touched.

**Several agents from one round draw one tree.** CC groups the `Agent` calls of one assistant message
into a single block (`renderGroupedAgentToolUse`, `UI.tsx:649-762`); the analogue here is a run of
adjacent watch rows sharing an insert point, which is what "the model made these calls in one round"
looks like once the rows are hung off the message's text.

```
   ⏺ Running 2 agents…
      ├─ @scout: fix the parser · 1 tool use · 2.1k tokens
      │  ⎿  ⏺ Read(a.rs)
      └─ @zoe: run the tests
         ⎿  Initializing…
```

Every glyph is CC's `AgentProgressLine`: `paddingLeft={3}`, the stem `├─ ` / `└─ `, the status row
`│  ⎿  ` / `   ⎿  `, the stats appended with ` · `, and one word — `Done` — where the ungrouped row
prints the whole cost. The headings are `UI.tsx:745-752`: `Running N agents…` while any is
unresolved, `N agents finished` when none is; `commonType` never fills in, because named instances
are never all one type. **A group anybody has opened is not a group**: an expanded member falls back
to the individual rows, which is how the folded content stays reachable by click and how the `ctrl+o`
pager (which opens every activity before it builds) sees the full thing. Rows inside a group wear no
portrait and claim no face — `Chat::faces` is what the transmit sweep sends, and sending a picture
nothing draws is paying for a hole that never appears.

**The completion's own line, and the one place it is gated.** CC renders a task notification arriving
in the leader's context as `<BLACK_CIRCLE> <summary>`, the glyph coloured by status and the summary
plain (`UserAgentNotificationMessage.tsx:55-81`, over the `<summary>` its `LocalAgentTask` writes at
`:246`: `Agent "<description>" completed`). bingo's is `● @scout completed · fix the parser`, glyph in
the done colour, text on the furniture tier. Three departures: `BLACK_CIRCLE` is `⏺` on macOS and `●`
elsewhere (`constants/figures.ts:4`) and bingo already spends `⏺` on tool rows *and* on main's prose,
so the other of CC's two glyphs is the one that does not collide; the summary names the **instance**,
because bingo's agents are addressable and `@scout` is what a reader would type next; and the text is
dim, which is the tier every line nobody said has settled into since D98 and what the design asked
for in as many words.

*It fires only when a notification really is main's.* The registry is the only thing that knows: a run
the user started inside an agent's own conversation registers with `notify_owner: false` and reports
its end to nobody (D98), and a run owned by a subagent reports it to that subagent. So `WatchEvent`
gained one field, `notifies_main`, set where the registry already decides whether to enqueue —
`has_wake_notifications(None)` would have answered "somebody's notification is pending", which is a
different question and would have printed a line for the wrong run. **Failure keeps its alert and gets
no second line** (`⚠ @scout · connection reset`, D98, untouched), and **cancellation prints nothing**,
which is D94's ruling and still right: the user just did it.

**A message from an agent is one visible line again.** This is the one row where v4 reverses v3. D98
made an agent→main message render nothing at all and let the woken turn speak for it; CC renders
`@name❯ <summary>` in the sender's colour and keeps the body for transcript mode
(`UserTeammateMessage.tsx:150-204`, the glyph `figures.pointer`). The wake, the debounce, the marker
and the `<messages>` envelope are **byte-identical** — only the screen changed.

*The summary is CC's own fallback.* CC's `SendMessage` requires a 5-10 word `summary` field
(`SendMessageTool.ts:76-80`) and falls back to `truncate(input.message, 50)` when it is missing
(`:765`). bingo's `SendMessage` has no such field, so the fallback is always the path taken: the first
line, fifty columns, through the house's `one_line`. Adding a `summary` parameter would have been a
tool-schema change in a rendering batch, and the fallback is CC's answer for exactly the case where
one is absent.

*The body lives in `ctrl+o`, through CC's own gate.* `Chat::transcript_mode` is `isTranscriptMode`
(`UserTeammateMessage.tsx:139`, `:186`): `transcript_rows` sets it for one build and restores it with
the fold state it already saves and restores, so the inline document is never built with it true and
nothing it flushed can disagree with what it flushes next. The `@name❯` line therefore occupies one
row in scrollback, forever, and three in a pager that rebuilds from zero.

*The arrival is read from a mirror, not from the mail.* `main_mail` is a byte contract with the
model — the marker on each line is what `buffer::line_source` parses back — so the renderer does not
un-format it. `ChannelRegistry` gained `main_arrivals`, a bounded queue (256, oldest dropped) that
`deliver_to_main` fills and the tick drains, which also means a `-p` run with no flow cannot grow it.
**Room relays are deliberately not mirrored.** The design's rooms paragraph offers a `#room❯` line and
the tiering table does not, and the table wins here for the reason the debounce exists: a room is a
conversation between agents that main overhears, and one flow line per post is exactly the flood D98
was written to stop. The room's surfaces are its zoom and the digest main writes after reading it.

*Where it lands, and what it wears.* Appended, like the alert, without splitting main's running reply.
D83 splits a turn for a *steered* message because the user's words demonstrably entered that turn's
context; mail may be drained by the running turn's next round or may sit until the debounce wakes a
new one, and a renderer cannot know which at arrival. So the flow states when the message arrived,
which is the thing it can be sure of — and it keeps a **send stamp** for the reason the alert keeps
one: it is news, from somebody, at a moment that reads differently at 09:02 and at 17:40. It wears
**no gutter face**, and that falls out of D99 rather than being decided again: `speaker_of` gives a
state line nobody, the identity is carried by the colour on `@scout❯` exactly as CC carries it, and a
portrait is two rows tall where this line is one.

**State changes still write nothing, and there is now a test that says so.** Running, Idle, `mark_idle`
and `stop` across the registry, plus a `refresh_conversations` and a tick: the flow is byte-identical
before and after. The roster's surfaces are the tree row and the pill, as D104 left them.

**Tests: twelve added, three rewritten, none deleted and none weakened.** Two in `activities.rs` (the
four label shapes and what each yields; the dispatch row's three forms — progress, condensed, cost —
plus a failure keeping its reason). Two in `bufferview.rs` (the teammate line's shape and the six
strings that are ordinary prose despite looking close; the notice line's two forms and its
non-overlap with the alert). Eight in `chat_tests_f.rs`'s new part E (the last three activity lines
and the unsettled message that makes them safe; the short window's condensation; the settled cost and
the settle itself; the grouped tree, its two headings, its one-word status and its dissolution on
expand; the notice line, its gate and the failure that gets no second one; the teammate line, its
50-column summary, its body reachable only through `ctrl+o` and its mail still sitting unread in main's
inbox; the lifecycle transitions that write nothing; the continuation run wearing the first run's
face). *The three rewritten* are the label assertions the row shape changed under —
`agent_watch_rows_wear_the_instance_face_only_where_images_place`,
`without_the_switch_the_transcript_wears_no_band` and
`the_running_turn_keeps_the_row_for_its_own_task_call` — each keeping its own question (the face
replacing the glyph, the absent band, the row belonging to the turn) and changing only the copy it
looks for. **1466 + 13 before, 1478 + 13 after.**

**Named limits.**

1. *Both new lines are recognised by their text.* `● ` at the start of a line, or `@name❯` with a
   plain-identifier name — so a user who types either gets the rendering. It is the same textual
   convention `is_agent_alert` has carried since D98 and the same exposure; the parser is strict about
   the name charset so that `@src/lexer.rs❯ why` is prose, and a test pins six near-misses.
2. *The teammate line's summary cannot be better than the message's first fifty columns.* CC's is,
   because its sender writes one. Giving bingo's `SendMessage` a `summary` field is a domain change
   and belongs to whichever batch is allowed to touch the tool schema.
3. *A room post still renders nothing in the flow.* Stated above as a decision rather than an
   oversight; if D108 finds the design's `#room❯` line is wanted after all, the mirror is where it
   goes and the debounce is the argument it has to beat.
4. *The grouped tree has no `ctrl+o to expand` hint of its own.* CC prints one on the group header;
   here the affordance is a click on the member row, and a hint promising a key that opens *one* of
   several rows would be a half-truth. The pager's `a` opens all of them.
5. *`In progress…` is decided per message, from the dispatches in that message.* CC counts every
   in-progress tool call in the session. One turn's dispatches all live in one message, so the two
   agree wherever it matters and differ only when two turns are somehow in flight at once.

### D107. The background dialog: one modal over everything that is not the conversation

**Problem.** Three surfaces were left over from three different answers to the same question. The
`ctrl+b` manager listed *running agents* and nothing else — no shells, though `ctrl+b`'s other
meaning is what puts a shell in the background; no rooms, though rooms are where half the formation
talks. The D95 team directory answered "who is here, what rooms exist, what just happened" and had
had **no door at all** since D104 took its `ctrl+t` stop. And the accounting store had been counting
unread and mentions for two batches with nobody reading them: D103 kept its readers alive for D104,
D104 declined them (CC puts no badge on a pill or a tree row) and moved the markers here. This batch
is CC's `BackgroundTasksDialog` — one modal, one cursor, four verbs — and it takes all three.
**In code: 2084 added against 1436 removed across 18 files**, one new (`src/tui/background.rs`, 1798
lines including its tests) and one deleted whole (`src/tui/directory.rs`, 676).

**The dialog is CC's, string for string.** Title `Background tasks` and the ` · `-joined running
counts under it — `2 agents · 1 active shell` (`BackgroundTasksDialog.tsx:425`, `:404-413`); the
empty state `No tasks currently running` (`:426`); headings `  Agents (2)` with two leading spaces,
the word bold inside a dim line and the count outside the bold (`:429`, `:439`); a blank row between
sections (`marginTop={1}`, `:438`); the pointer `❯ ` in the cell before the row and `  ` otherwise
(`:571`); the status chip in parentheses, dim, coloured by state — `done` in the success colour,
`error` in the error colour, `stopped` in the warning colour, `running` plain
(`ShellProgress.tsx:21`, `:39-80`); a teammate row as `@name` in the identity colour with a dim
`: <activity>` after it (`BackgroundTask.tsx:149-215`); and the key row as a `Byline` of
`<key> to <action>` hints joined by ` · ` (`design-system/KeyboardShortcutHint.tsx:16`,
`design-system/Byline.tsx:10`):

```
↑/↓ to select · Enter to view · f to foreground · x to stop · ←/Esc to close
```

**The design's key row lost to CC's, which is the 1:1 rule working.** `conversation-model-v4.md` §4
wrote `↑/↓ select · Enter detail · f zoom · x stop · Esc close`. Every verb differs and CC wins on
all of them: `Enter to view` (its detail *is* the view), `f to foreground` (its verb for the screen
D105 calls the zoom), `←/Esc to close` (`←` closes the list, not just the detail). Two of CC's
conditionals came with the strings: `f` appears only where the row has something to foreground and
`x` only where the row is running (`:414`), so the bottom line never names a key that would do
nothing.

*The blank rows are load-bearing and were found by looking.* CC separates sections with one
(`:438`); rendering the thing showed that the header and the key row want the same treatment, so the
box reads as three kinds of block — the title and its counts, then one block per section, then the
byline — rather than as eleven rows in a rectangle. Four blank rows, no other decoration.

**A heading only appears where there is something to tell it apart from.** CC renders the `Agents`
and `Shells` headings only when another kind is on screen (`:428`, `:438`) — a label over the only
list there is, is noise — and an empty section renders nothing rather than "no rooms yet". That is
the one place the directory's convention lost: D95 printed all three empty sections on the argument
that a blank panel is a bug report, and CC answers the same worry with one line for the whole modal.

**Rooms are bingo's third section, inside CC's grammar rather than beside it.** `#build: 12 messages
· main, scout, user` with the same chip carrying `you're not in` where the user is not a member —
which is D95's mark, kept for D95's reason: the mark is on the rooms where speaking would mean
something different, and a tick on every room you are in would be a column saying nothing. The
directory's other question, *which rooms is each member in*, is the same fact transposed: it is
answered by reading the members off the room rather than the rooms off the member, in a section that
has to exist anyway.

**`f` is the room zoom's first door.** D105 built the room view whole — body, send-with-join,
`esc` — and shipped it with `#[allow(dead_code)] // D107 opens this` on `ZoomTarget::Room`, because
CC has no rooms anywhere in 2.1.88 and inventing a global binding for one surface is how a keymap
rots. The dialog is the door, and it is the *same* door an agent uses: one key, one verb, two kinds
of conversation.

**The cursor is on a thing, not on a position.** CC keeps a selected id and re-finds it every render
(`:184-192` sorts, `Item` compares ids); `BackgroundDialog::selected` is a `DialogTarget` for that
reason and one more that is bingo's own — the rows re-sort as work moves, and an index would let `x`
stop whatever slid under the cursor between two frames. `None` means "the first row there is",
resolved at draw time, so opening the dialog needs to know nothing about the roster and a row that
leaves takes the cursor back to the top instead of off the end. A test drives exactly that: two
instances, the running one leading, the cursor moved onto the second, the first stopped — and the
cursor is still on the row it was on, which is now the first.

**The order is CC's, with one substitution.** Running first, then youngest first (`:184-192`, over
`startTime`). Shells have that clock — ids are handed out in sequence, so a higher id is a younger
command — and conversations do not: an agent or a room is not a task that only begins. So the second
key for those two sections is **the accounting store's own clock**, which is what `Buffer::
last_activity` has been recording since D88. It is an **order and not a duration**, stated as such
where it is read: `Chat::needs_tick` stops the frame counter when nothing is happening, so two of
these compare and neither subtracts from the wall clock. Durations on screen come from sources that
carry one — `AgentStatus::last_active` through the tree's `status_label`, and `WatchSnapshot::
elapsed_ms`, which this batch added beside `kind` so a row can say how long a command has been up.

**The badge is the store, finally spent.** `Buffer::{id,unread,mention,last_activity}` and
`Buffers::iter` lose their `#[allow(dead_code)]` markers to one function, `dialog_badges`, which is
the dialog's single read of the store. The count lands in CC's chip — `@scout: reading src/lib.rs…
(3 unread)` — and **the mention lands in the chip's colour**, which is D90's rule brought forward
verbatim: a conversation that said your name is worth more than one that merely moved, so the accent
means "wants you" and the plain unread colour means "moved". Entering a conversation reads it, and
it does so through the door D105 already built: `f` arms the zoom, `enter_zoom` calls
`Buffers::set_active`, the badge is gone on the way back. Two tests pin the pair — the count on an
agent row and a room row with the two styles differing, and the count cleared by a round trip
through the view.

**`x` is one stop path with one warning, and it says so where it cannot act.** A running instance
goes through `stop_agent` — renamed from `stop_agent_from_manager` now that three surfaces share it
and none of them is called the manager — so the tree's `k`, the zoom's `esc` and the dialog's `x`
still produce one warning and one watch transition, with no confirmation, which is CC's ruling
(`useBackgroundTaskNavigation.ts:228-241`). A **shell** is where bingo cannot follow CC: `tool/bash.rs`
hands a promoted command's child to a spawned waiter and keeps no handle, so there is nothing here to
kill. `x` on a running shell answers on the warning tier — *a background command cannot be stopped
from here; it reports when it exits* — because a key that appears dead is worse than a refusal, and
the key row does not offer `x` on that row in the first place.

**The detail replaces the list, which is why Esc closes.** CC's detail dialogs are separate screens
(`:396-398`), and their own footer is `← to go back · Esc/Enter/Space to close · x to stop · f to
foreground` (`InProcessTeammateDetailDialog.tsx:198`, `ShellDetailDialog.tsx:167`). bingo's manager
peeled detail → list on Esc; the dialog does what CC does, and `EscLayer::BackgroundDialog` is
therefore **one layer rather than two levels** — `←` is the way back, so nothing is stranded by a
press that closes the modal. The two retired layers (`AgentManager`, `Directory`) collapse into it
and `ORDER` drops from 16 to 15.

*Three details, one per section.* The agent's is CC's `InProcessTeammateDetailDialog`: `@scout
(reading src/lib.rs…)` as the title (`:126-150`), the run's cost as the subtitle (`:160-183`), then
`Progress` with the newest activity marked `› ` and `Prompt` (`:209`, `:218`). Both halves of the
subtitle are gated on having something to say — D104's rule for the same numbers, because bingo's
progress is the *current run* and an instance between runs would otherwise carry `0s · 0 tool uses ·
0 tokens`. The shell's is `ShellDetailDialog`: `Shell details`, then `Status:` / `Runtime:` /
`Command:` / `Output:` (`:177`, `:193`, `:223`, `:253`), the output being the completion payload's
tail with `No output available` (`:317`) where there is none and `Showing N lines` (`:371`) where
there is — a *running* command has no output here, and that is honest rather than missing: the tail
the user watched belonged to the foreground and the registry only ever holds the finished text. The
room's is bingo's own in the same grammar: `Members:`, `Messages:`, `Recent messages:`.

**The lifecycle feed retired, and it had to.** D104 kept `Buffers`' team log because "that is what
D107's dialog will show". It does not: CC's dialog has no recent-events column, and the two answers
the feed carried are both on screen elsewhere now — a run's start and end are the flow's own dispatch
and completion rows (D106), and `/team`'s output has printed on the info tier since D104. So the feed
went: `TeamEvent`, `team_line`, `state_word`, `team_log`, `note_team_output`, `note_watch_event`,
`push_team`, `TEAM_LOG_MAX` and the `team` field. A store nobody reads is not a store, and keeping
one alive behind a marker for a third batch would have been a promise nobody was going to keep.
`AgentRegistry::run_is_the_users` went the same way with its whole chain (`user_run`,
`set_run_trigger`, two call sites): D105 removed the filter that asked whose run it was, and a
roster row that answered "yours" would be answering a question the dialog does not ask.

**What the record page's retirement would cost, and why it is D108's.** The v4 table says the
D96/D100 observation page "retires as a surface; projection reused", and this batch owns its last
door. It is **descoped deliberately**, with the door kept: `tab` in the dialog's agent detail still
opens it, and the detail's footer names the key. The inventory, so D108 does not have to redo it:

| Would be deleted | Would survive, and who reads it |
|---|---|
| `src/tui/perspective_ui.rs` whole (933 lines, 12 tests) | `perspective::walk` — the zoom's body (D105) |
| `perspective::dossier` and `Dossier` / `Lane` / `LaneId` (~150 lines, 11 tests) | `Protagonist` and its two defaults — `walk`'s attribution |
| `Chat::open_perspective` and the two `run_perspective_modal` call sites in `app.rs` | `Filed` / `Target` / `Work` — the zoom reads `Target::Intake` for the furniture tier and `work` for the fold |
| the `tab` arm and its footer hint | `pair_lane` — `Buffers::pair_measure`, which is what the badge above is counted in |

The split is clean at `dossier`: everything the *page* needs is above it, everything the zoom and the
accounting need is below. What is not clean is the tests — three of the eleven dossier tests pin
`walk`'s marker handling through the lanes it fills (`a_page_groups_every_counterpart_the_markers_can_
name`, `mains_page_reads_unmarked_prose_as_the_user`, `the_flipped_default_does_not_move_any_marker`)
and would have to be rewritten onto `walk` rather than deleted with it. That is a batch's tail, not a
batch's afternoon, and D108 already owns the README rewrite that has to travel with it.

**Divergences from CC, each with its reason.**

1. *The dialog is on `ctrl+b`.* CC opens it from the footer pill, `shift+↓` and `/tasks`, and spends
   `ctrl+b` on `task:background` — which is exactly bingo's *first* meaning for the key (D84). So the
   key does CC's thing when there is a foreground command to background, and opens CC's dialog when
   there is not. The design's ruling, and it costs nothing: nothing else was bound to `ctrl+b`.
2. *`ctrl+b` closes it again.* CC's dialog has no toggle. With the modal open the chord was inert —
   nothing else binds it — and the ctrl+t panels' rule is that the key that opened a panel closes it.
   A dead chord on an open surface is worse than a second way out.
3. *`f` is offered on any agent, not only a running one.* CC gates it on `status === 'running'`
   (`:414`) because a finished task has nothing to foreground. bingo's `f` opens a *conversation*: a
   stopped instance still has its whole record, and a message to it resumes it (D105a). The gate that
   survives is the one that is true here — the row has somewhere to point.
4. *`x` on a shell refuses out loud.* CC kills; bingo has no kill path for a background command at
   all (see above). Stated rather than silently swallowed.
5. *Esc closes from the detail.* CC's, against D80's one-press-one-level habit — see the layer note
   above; the detail is a mode of one surface rather than a surface over another.
6. *`@main` is not on the roster.* D95's directory led with it (D100's ruling: main is a participant).
   The dialog lists what can be *managed in the background*, and main is neither stoppable nor
   foregroundable — its conversation is the screen you pressed the key on. The tree still leads with
   `@main`, which is where "who is here" is answered in full.
7. *`j` does not join a room here.* The directory's join key retires with it: `/join #room` is the
   command for joining without speaking, and posting from the room's zoom joins first anyway. One
   fewer key on a row, and no capability lost.
8. *The unread badge is a count, and CC's is a boolean.* CC writes `, unread` inside the chip
   (`BackgroundTask.tsx:127`) because its tasks are not conversations. bingo's store measures a
   count, in Said posts (D99), and this is the surface D104 named for it.

**Tests: nineteen added, twelve rewritten, thirteen deleted with their machinery, none weakened.**

*Nineteen new in `background.rs`*: the three sections from three live sources with the directory's
two questions among them; the heading rule and the empty dialog's one line; the subtitle's counts and
what a stop does to them; the order (running first, then what moved last); the cursor walking every
section and wrapping; the cursor following its row when the order changes under it; `f` on an agent
**and on a room**; `f` inert on a shell and absent from its key row; `x`'s four cases (running
instance, stopped instance, running shell's refusal, room); the badges and the mention's colour;
the badge cleared by a round trip through the zoom; the key row's exact copy and its conditionals;
`Enter` opening the detail with `←`, Esc and Space as the ways out; the agent detail's five parts;
`tab` reaching the record and doing nothing on a room; the shell detail's labels, its `No output
available` and its finished form; the room detail; the fit at four widths with a section past its
window; and a pending question keeping the dialog shut.

*Twelve rewritten, each because the thing it was about survives.* `agent_manager_lists_opens_details_
and_stops_agents` → `the_background_dialog_lists_opens_details_and_stops_agents`, keeping every
question it asked and asking it through the key that carries it now. `running_agents_leave_the_arrows_
to_history` walks `ctrl+b` → `tab` and `ctrl+b` → `f` where it used to walk Enter → `tab` and
Enter → Enter. `the_directory_swallows_its_own_keys_and_passes_the_chords_through` →
`the_dialog_swallows_…`, plus the one chord that is now the exception. `team_output_lands_on_the_info_
tier_and_in_the_feed` → `team_output_lands_on_the_info_tier`, and it got *stronger* on the way: it
compares the tier's lines against `team_cmd::run`'s own output rather than against a copy the feed
filed. `a_terminal_event_with_no_notification_for_main_wakes_nothing` keeps its rule and asserts the
surviving half — nothing in `@main` — where it used to assert the feed's copy.
`the_lifecycle_log_keeps_what_the_console_no_longer_prints` →
`the_lifecycle_signal_reaches_the_dialog_and_not_the_console`, which is the same reroute pointed at
its new destination. `esc_peels_the_agent_tree_in_the_slot_above_the_task_panel` asserts the dialog
above the tree and 15 layers. `ctrl_t_cycles_the_tasks_then_the_agents_then_away` asserts the cycle
reaches no modal. `ctrl_b_backgrounds_the_running_command_before_it_opens_the_manager` and the zoom's
held-back-chords test follow the field's rename. `the_panel_names_the_status_layer` and
`ctrl_b_help_names_both_of_its_meanings` follow the help copy, and the record door's advertisement
moved from the fixed help line to the detail's own footer, where it can be conditional on the row.

*Thirteen deleted with the machinery they pinned*: ten in `directory.rs`, which went whole, and three
in `buffer.rs` for the lifecycle feed (what it hears, what it bounds, what it remembers).
**1478 + 13 before, 1484 + 13 after.**

**Named limits.**

1. *The observation page is still reachable, and v4 says it should not be.* Descoped on purpose, with
   the inventory above. `tab` is its one door and the detail's footer names it.
2. *A background shell cannot be stopped.* The refusal is honest and the fix is a domain change —
   `tool/bash.rs` would have to keep the child handle a kill needs — which this batch was fenced out
   of.
3. *A running shell's detail shows no output.* The registry holds the completion payload and nothing
   before it; the live tail belongs to the foreground surface that has already scrolled by. Reading a
   running background command's output would need a domain store nothing has asked for yet.
4. *`@main` has no row.* Limit 6 of the divergences, restated as a cost: a reader looking for the
   console in a list of background work will not find it there.
5. *The sections are windowed at eight rows each.* Past that a section counts the rest (`… 3 more
   agents`) and the cursor cannot reach what is not drawn — the same bound the manager had, now
   applied three times. A user with nine agents and a stopped one to find has to stop one first.
6. *The lifecycle feed is gone rather than moved.* If a "what just happened" column turns out to be
   wanted, it is a new store: nothing keeps that history now except the flow itself.

### D108. The last surface closes, the sender writes its own line, and the prose catches up

**Problem.** Five batches built v4 and each one left something for the last. D107 owned the
observation page's retirement and descoped it with an inventory. D106 named the `@scout❯` line's
summary as a limit it could not fix without touching a tool schema. D105 named `README.zh-CN.md` as a
batch behind, and D104 and D107 named their own copy debts. And the prose everywhere — module docs,
test names, assertion messages, both READMEs, the bundled guide — still described surfaces that had
been gone for up to five batches. This is the closing batch: no new mechanism except one tool field.
**In code: 729 added against 1851 removed across 26 files**, one deleted whole.

**The observation page is gone, and the split was exactly where D107 said it was.**
`src/tui/perspective_ui.rs` (933 lines, 12 tests), `dossier`, `Dossier`, `Lane` and `LaneId` (~150
lines), `Chat::open_perspective` and its two `app.rs` call sites, the dialog's `tab` arm and the
`tab to open the record` hint in its detail footer. What the zoom reads survives untouched: `walk`,
`Protagonist`, `Filed`/`Target`/`Work`, `pair_lane`. The sweep after the cut found one more thing
reachable only from the page — `ChannelRegistry::rooms_of`, whose doc said in as many words that "the
directory prints it beside the member; nothing else needs it", and whose two assertions were replaced
with the `is_member` ones on either side of them so the membership round trip stays pinned.

*Nine of the twelve dossier tests were rewritten onto `walk` rather than deleted with it, which is
six more than D107 budgeted for.* Its inventory named three that pin marker handling *through* the
lanes; reading them showed the same is true of four more — the scaffolding rule, the protagonist's
own process, mail filed under its sender, the pre-D98 envelope — and of the timeline's superset
property, which is the statement that the targets *partition* the walk in order. All of those are
claims about `walk`, so they are asserted about `walk`: `said_to`/`filed_to` over `Vec<Filed>` where
the tests used to ask a `Dossier` for a lane by name. **Three were genuinely the machinery's and went
with it** — `lanes_are_ordered_by_last_activity` and `a_lane_s_count_is_its_thread_s_messages` are
`Lane`'s, and `a_room_thread_is_the_whole_room_with_the_agent_in_it` is `channel_posts`', which has
its own test in `buffer.rs`; the one thing that test said and the `buffer.rs` one did not — the `you`
flag flipping with the reader — was added there rather than lost.

*One rewritten assertion got a fact right that the lane grouping had hidden.* `a_page_groups_…`
became `the_walk_names_every_counterpart_the_markers_can_name`, and stating the `TimelineOnly` set
directly showed that the reply a spawn prompt draws is filed there too — intake is not a counterpart,
so nothing was said back to it. The dossier put that post in the timeline and in no lane, which is
the same fact; the lane view just never had to name it.

**`SendMessage` gains `summary`, and the interesting half is where it does *not* go.** CC's field is
`'A 5-10 word summary shown as a preview in the UI (required when message is a string)'`
(`SendMessageTool.ts:76-81`) and its readers prefer it over `truncate(input.message, 50)` (`:765`,
`:782`). The brief asked whether the envelope carries it. **CC's two runtimes disagree, and v4
replicates the one that does not.** Its *teammate* path writes `summary="…"` as an attribute on the
`<teammate-message>` tag (`utils/teammateMailbox.ts:386`), which becomes the recipient's own prompt
text and which `UserTeammateMessage.tsx:25` then regex-parses back out — one string doing double duty
as context and as UI. Its *subagent* path calls `queuePendingMessage(agentId, input.message, …)`
(`SendMessageTool.ts:810-814`) and drops the summary before the recipient sees it. v4's member model
is the subagent, so `main_mail` is **byte-identical**: the model reads exactly what was said to it,
`buffer::line_source` parses what it always parsed, and the preview rides `MainArrival`, the mirror
D106 built beside the mail for exactly this reason.

*The field is left off main's own schema.* `summary` is drawn in two places and both read a
subagent's send — the `@name❯` line (D106) and the tree's `ctrl+shift+o` preview (D104). Main's sends
have no such surface, so `input_schema` removes the property at depth 0 rather than advertising a
parameter the model cannot use. `SendMessageInput` still *accepts* it at every depth, because
`deny_unknown_fields` would otherwise turn a harmless word into an error — the standard shape:
the schema advertises what is useful, the parser tolerates.

*Both sources go through the same fifty-column cut.* CC does not bound its summary, because its
schema requires 5-10 words; bingo's is optional and a model can write a paragraph into it, and the
`@name❯` line is one row that `parse_teammate_line` has to read back. A real summary passes through
`one_line` untouched, and one that is not a summary cannot overrun the row.

*The tree preview gets `summary` at the head of CC's key list.* `["description", "prompt", "command",
"query", "pattern"]` matched nothing on a `SendMessage` call, so a send fell all the way to
`Using SendMessage…`; `summary` is the one input field whose whole purpose is to be a preview, so it
goes first. Without one the fallback is exactly what it was.

**The sweep, and the shape of what was stale.** Retired vocabulary was still asserted as current in
eleven source files, both READMEs and the bundled guide. Three clusters:

| Where | What it still claimed |
|---|---|
| `perspective.rs` (header + 5 doc sites), `buffer.rs` (9), `bufferview.rs` (3), `avatar.rs`, `chrome.rs`, `zoom.rs` | the observation page / the pair view as live readers, "the bar" as the thing an accounting rule serves |
| `channels.rs` (3), `watch.rs` (2), `settings.rs` (2), `rewind_ui.rs` (2), `chat_tail.rs` (2), `chat_tests_f.rs` (2) | the team directory, the lifecycle feed, the switcher, the agent manager, the `Post` tool, "the workspace views (DM, channel, team)" |
| `tool/agent.rs` (5, three of them **model-facing**) | "your private direct-message window" in `SUBAGENT_NOTE` and `CHANNEL_NOTE`, "a channel Post", "what the bar shows" |

The model-facing ones mattered most: a subagent's system prompt named a surface that has not existed
since D103, and the note's own test asserted the phrase. The claim underneath survives whole — the
user has a private line to every instance and reads its turns — so the note says that instead, and
`subagent_note_knows_the_dm_window_exists` became
`subagent_note_knows_the_user_can_write_to_it`, asserting the sentence that carries the claim now.
`keys.rs`'s retired-surface guard, which is the standing defence against this, gained `the record`
and `perspective` to its list.

*Two things that looked stale were left alone, each for a reason.* "hub-and-spoke" is a topology, not
a surface, and describes the addressing rules exactly. `BufferId::Hub`'s spelling is D101's explicit
ruling and its comment already says why. And `share.rs`'s legacy-derivation table keeps `Post`,
because it reads transcripts written before D98 — it just says `(pre-D98)` now.

**The guide's duplication was an accident, and deleting it removed the worst copy in the tree.**
`guide.md`'s capability map appeared **twice**: bullets 224-440 and a strict-subset copy at 441-583,
introduced by a parallel-entry merge (`a4b4d51`, "guide.md oauth detail merge"). Later batches patched
only the first, so the second still explained **D102's silence contract to the model as a live rule**
— the single most harmful line in the tree, since D103 removed the marker and a model told to emit it
would be writing `[[quiet]]` into prose. The duplication is not structural: there is no heading
between them and no bullet in the second that is not in the first, except two paragraphs — **Sharing**
and **Updates** — that describe `bingo share` and `bingo update` and exist nowhere else. Those moved
into the surviving `Sessions` bullet and the copy went. **178 lines out of one file.**

*The surviving copy's interaction story was then rewritten as one voice, in one order*: the transcript
and its grammar → the status layer → what the transcript shows of an agent's life → the zoomed view →
the background dialog → rooms → the team → attribution → unread → avatars. Two paragraphs were new
rather than edited: **the D106 tiering**, which the guide had never described (the dispatch row, its
live progress, the settled cost, the grouped tree, the `●` notice, the `@name❯` line, the `⚠` alert,
and the list of what writes nothing), and **attribution**, which replaces the perspective page's own
paragraph with the walk it was a view of. A stray fragment that opened `↑/↓ belong to the composer's
prompt history` and then repeated the background dialog verbatim was folded into its neighbours.

**Both READMEs describe v4 whole, and zh-CN is a peer again.** English lost the perspective-page
section and the `tab` door and gained the same two paragraphs the guide did. zh-CN was two batches
behind and is now section-for-section with English: `**状态层**` (D104) and `**放大视图**` (D105) are
new, the background dialog is expanded to the full key row and detail, and the tiering and attribution
paragraphs are translated rather than summarised. The `## 会话（Conversations）` heading — a v3 name,
plural — became `## 一条 transcript，以及能离开它的那一行`, which is what the English heading says.

**The line cap, and how close it came.** `src/tool/agent.rs` was at 3899 of its 4000 and the batch's
own additions took it to 4021 — the discipline gate caught it. Nothing unrelated was touched to make
room: the new test was tightened (the same five claims, driven through one fewer registry insert —
the "main may still pass one" half is a `serde_json::from_value` on the input type, which is what that
claim is actually about) and two doc comments were compressed. **3994.** The file is a standing debt
this batch did not create and did not pay.

**Tests: three added, nine rewritten onto `walk`, four strengthened in place, sixteen deleted with
their machinery, none weakened.** Added: two in `chat_tests_f.rs` (the line prefers the sender's
summary and the body and the mail are untouched by it; an oversized summary is cut to the same
budget) and one in `tool/agent.rs` (the schema offers it to a subagent and omits it for main, the
arrival carries it, `main_mail` does not, and the parser still accepts it at depth 0). Strengthened
without renaming: `a_rooms_log_reads_as_messages_with_membership_changes_as_notes` (the `you` flag,
inherited from the deleted room-lane test), `the_preview_shows_the_last_three_lines_of_the_record`
(the `summary` key and the unchanged fallback), `the_panel_names_no_retired_surface` (two more
retired words), and `the_agent_detail_says_what_it_is_doing_and_what_it_was_asked` (which now asserts
`tab` is **absent** where it used to assert it present). Deleted: the twelve in `perspective_ui.rs`,
three `Lane`/`Dossier` tests whose subject is gone, and
`tab_opens_the_record_of_the_instance_under_the_cursor`. `running_agents_leave_the_arrows_to_history`
lost its `tab` arm and keeps every other claim through `f`, the one door that survives.
**1484 + 13 before, 1471 + 13 after.**

**Deviations from the brief, each with its reason.**

1. *Nine dossier tests rewritten, not three.* D107's inventory named three; four more and the
   timeline-superset property turned out to be claims about `walk` as well. Rewriting them is
   strictly stronger than deleting them.
2. *`summary` is off main's schema.* The brief said "the subagent-assembled variant at minimum". It
   is exactly that, and the gate is in `input_schema` rather than in a second input type, because a
   second type would be two things to keep in step for one absent field.
3. *`guide.md`'s duplicate block was deleted rather than kept consistent.* The brief said to
   understand the duplication before touching it and keep it consistent if it is structural. It is
   not: it is a merge artifact with no heading, no unique bullet, and a five-batch-stale copy of the
   worst kind of content. Its two genuinely unique paragraphs were moved before it went.

---

**The program's remaining limits, gathered.** Everything v4 (D103–D108) leaves open, in one place.
D109 (pane mode) is the only batch still to come and owns none of these.

*Exposure, and the one thing deliberately not built:*

1. **The transcript's own line shapes are recognised by their text.** A user who types `● something`
   or `@scout❯ something` gets the rendering, because `is_agent_notice` and `parse_teammate_line` are
   textual conventions — the same exposure class `is_agent_alert` has carried since D98. The parsers
   are strict about the name charset (`@src/lexer.rs❯ why` is prose, and six near-misses are pinned),
   but they cannot tell a user's keystrokes from the harness's. **No escaping mechanism was built**,
   on purpose: an escape would be a second grammar in the composer for a case nobody has hit, and the
   worst outcome today is a line that looks like news.

*Things bingo cannot do, where the fix is a domain change:*

2. **A background shell cannot be stopped.** `tool/bash.rs` hands a promoted command's child to a
   spawned waiter and keeps no handle, so there is nothing to kill. `x` on a running shell answers on
   the warning tier rather than appearing dead, and the dialog's key row does not offer `x` there.
3. **A running shell's detail shows no output.** The registry holds the completion payload and
   nothing before it; the tail the user watched belonged to the foreground surface, which has already
   scrolled. Reading it live needs a store nothing has asked for yet.

*Chords and doors:*

4. **`ctrl+shift+o` needs the kitty keyboard protocol.** Elsewhere the chord arrives as `ctrl+o` and
   opens the transcript, so the tree's message preview is unreachable there. No second binding was
   invented.
5. **Five chords are inert inside a zoom** (`ctrl+o`, `ctrl+b`, `ctrl+t`, `ctrl+g`, `ctrl+r`): they
   set flags the *host* consumes, so binding them would spring a modal on the way out.
6. **The room zoom's only door is the background dialog's `f`.** CC has no rooms in 2.1.88, so there
   was no key to copy and none was invented.
7. **`@main` has no row in the background dialog.** It lists what can be managed in the background,
   and main is neither stoppable nor foregroundable. The tree is where "who is here" is answered in
   full.

*Bounds and shapes a reader might expect otherwise:*

8. **The dialog's sections are windowed at eight rows each.** Past that a section counts the rest
   (`… 3 more agents`) and the cursor cannot reach what is not drawn.
9. **A room post renders nothing in the flow.** The design's `#room❯` line lost to the tiering table:
   a room is a conversation main overhears, and a line per post is the flood the D98 debounce exists
   to stop. If it is ever wanted, `MainArrival` is where it goes and the debounce is the argument it
   has to beat.
10. **The grouped dispatch tree has no expand hint of its own.** A hint naming a key that opens *one*
    of several rows would be a half-truth; the pager's `a` opens all of them.
11. **`In progress…` counts one message's dispatches**, not the session's in-flight calls. The two
    agree wherever it matters and differ only if two turns are somehow in flight at once.
12. **The zoom has no scrollback search.** `/` and `g`/`G` belong to the composer there, so the
    pager's search would have had to steal them.
13. **The task prompt shows twice in a zoom** — the header names the task, the record contains it.
    CC has the same duplication for the same reason.
14. **A mid-word wrap can still happen in a zoom body.** It is the markdown renderer's, shared with
    the transcript.
15. **The pill window is left-anchored.** CC scrolls it to keep a selected pill in view; bingo's
    pills have no selection, and at index 0 CC's own window starts at the left.
16. **`@main` is not addressable and the name match is exact.** `@main hello` opens an ordinary turn,
    which is what talking to main already is; `@Scout` is prose, because the typeahead is where
    discovery belongs and a case-insensitive parser would be guessing.

*Debts this batch touched but did not clear:*

17. **`src/tool/agent.rs` is 3994 of its 4000-line cap.** Pre-existing, and now six lines from the
    gate. The next thing to add there has to split the file.

### D108a. Smoke fixup: a short message no longer cuts the portrait at the waist

Found on the first real-terminal run of v4, by the user, in the console and
the room zoom alike: a lead message one row tall drew a two-row portrait with
nowhere to put its second row — `gutter_rows` walks the rows it is given and
silently drops the cells past the end, so the kitty placeholder's bottom row
never reached the screen and the face rendered clipped.

The rule now stated once at both entry points (`Gutter::apply` and the
`El::Gutter` render arm): every gutter cell must have a row to ride, and a
child shorter than its cells is padded with blank rows at the end. `cells()`
stopped listing the chip skin's second row — it was the blank cell by another
name — so only the image skin ever pads and the chip skin keeps its heights.
That amends the D97 doctrine "the row count is identical either way" to
"identical everywhere but a lead message shorter than the portrait", and the
cross-skin layout test now pins the difference at exactly the pad row.

### D110. One switch for every avatar

The user's ruling, delivered on the first real-terminal day of v4:
**`experimental.chatAvatars` governs every avatar in the interface.** D99 had
narrowed the switch to the watch row and let the conversation gutter run
unconditionally; that inversion is now reversed — off (the default) means no
gutter, no chips, no watch-row portrait, no face transmissions, anywhere. On
means everything D97/D99 built, unchanged. The default therefore *is* CC's own
look — no avatars — and the switch is where bingo's flavor lives, which reads
as the arrangement this program should have arrived at itself.

Mechanically: `Gutter` gained a `faces` dimension ahead of `images`. With it
off, `width()` is zero, `cells()` is empty, and every consumer — the console's
`El::Gutter` blocks, the zoom's `settled_post_rows`, the wrap arithmetic that
subtracts the gutter from the body width — degrades through the one value it
already read. The two drawing sites (the console's `conversation_gutter` and
`Chat::conversation_gutter`) pass `chat_avatars`; the five colour-only
constructions (tree, pills' addressee tint, zoom header, task owners) pass
`false` and lose nothing, because `index_for` answers regardless — identity
colours are not avatars, by the ruling's own line. Face recording and the two
transmit sweeps (inline and the zoom's alt-screen) are gated at the same
switch, so an off session sends no image bytes at all.

Tests: the off-state pin (`without_the_switch_the_transcript_wears_no_band`)
rewrote its claim for the second time, honestly both times, and now asserts
the full absence — no placeholder cells, `Chat::faces` empty. Seven tests
whose subject is the gutter itself opted in with one line each. D108a's pad
row rides along untouched: with faces off there are no cells to pad for.

D109 remains reserved for pane mode.

### D111. The orchestration verbs fold, and arrivals queue up

The user's ruling, same real-terminal day: the coordination tools —
AgentControl, SendMessage, Channel — and the arrival lines should group the
way everything else in the flow does, instead of each claiming a standalone
block. AgentControl already folded; this batch brings the other two verbs and
the arrival tier level with it.

**The verbs.** `classify_tool` gained `Send(target)` (sigil normalised, so a
lone send can say who it reached) and the three Channel counters — creates,
roster changes and list-looks counted apart, on the same argument that keeps
a stopped subagent out of "checked": a summary must never report a change as
a glance. `collapse_summary` words them as `Messaged @scout` / `Messaged
@scout 3 times` / `Messaged 2 recipients`, `Created 1 room`, `Changed 2
rosters`, `Looked at the rooms`. Malformed calls (no `to`, no `action`) stay
standalone. The zoom's `tally` folds through the same classifier, so both
surfaces read one ruling.

**The arrivals.** Consecutive `●` notices and `@name❯` lines now share one
block — the blank row between messages is skipped when this message and the
one before are both arrival-tier — so three agents reporting in read as one
batch, which is the tool groups' own argument applied to the other direction
of traffic. The `⚠` alert deliberately never joins: bad news keeps its own
block. The decision reads nothing but the previous message's settled text,
so a block renders the same on every frame and write-once holds.

**The split.** The fold machinery left `chat.rs` for a new `tui::collapse`
module when the file hit the 4000-line cap mid-batch (D108 predicted the
next addition would have to split something; it was this one), re-exported
from `chat` so every consumer keeps its path. The pure classifier tests
moved with it — `chat_tests_a.rs` had crossed the cap too, and a test should
live beside what it pins.

### D112. The room learns when not to speak

The user's ruling on the greeting storm: members should judge for themselves
when to speak, and the judgment should be taught by prompt — not enforced by
machinery. The analysis that led here: one "Hi there" in #dev-team became
~25 runs, but the members' *discipline* mostly held (the second-order wakes
ended silent, exactly as the channel note teaches). What failed was the
race: five members each woke on msg #1 alone, could not see each other, and
all five answered the same broadcast. The room's serial mode even caught
them — every late greeting bounced with the peers' greetings attached — but
the bounce copy offered resend/edit/drop as three equal choices, and five
models chose resend.

Two prompt edits, both at the point where the information is:

- **The channel note's broadcast rule** gains the covering clause: a
  broadcast is owed one *covered* answer, not one answer each; a member who
  can see a colleague already answering adds a line only if it carries
  something theirs did not.
- **The stale bounce flips its default to drop**: when what landed already
  covers your message, dropping is the answer — resend is the exception
  that has to justify itself.

Not done, deliberately: the member-side wake debounce, the busy-turn watch
stapling and the `●` gate stay as analyzed and unbuilt — the user chose the
prompt lever for the speech question, and the view questions are separate
rulings not yet made.

### D113. A speaker's run opens with their name

Found by the user the same hour, with avatars off (D110): a room's log was a
wall of anonymous prose, because identity had ridden entirely on the gutter
face/chip that D110 turned off. The ruling: an agent is identified by
avatar + name, and by name alone when avatars are off.

`settled_post_rows` now opens every speaking run of somebody-else's posts
with a name row — `@dev`, identity colour, bold — before the gutter is
applied, so with avatars on the portrait's first cell rides the name row,
which is the geometry D97's module doc had described all along ("row 0
rides the name line"), and the D108a pad row is mostly no longer needed
(a one-line message under a name row is already two rows tall). The user's
own bubble keeps its `❯` and gets no label — the glyph already names the
one person who never needs naming — and furniture (membership lines, work
steps) names nobody. Both zoom kinds inherit, agent and room alike: main's
instructions in an agent's record now read as `@main`'s, which the guide
and README had (prematurely) promised since D105.

### D114. The inbox turn — the flow's whitelist closes

The three-survey research pass (CLI subagent rendering; orchestration
mission-control; group-chat paradigm — record in
notes/design/conversation-model-v5.md) converged on one sentence: the main
flow is an inbox, not a monitor. v4's skeleton is exactly the industry's
shape; what leaked was the social layer pushing rows into a write-once
scrollback. Three leaks, closed at the same seam:

- **Arrival lines retired.** `SendMessage(to: "main")` draws nothing;
  `absorb_arrivals` now feeds `Chat::agent_mail` (per-sender count, the
  status layer's dot) instead of printing `@name❯`. The renderer, parser,
  50-column budget and streak membership went with the producer
  (bufferview.rs, chat_tail.rs); `●` notices keep D111's coalescing.
- **A `dispatch` bit on watch registrations** (watch.rs `Entry`/`WatchEvent`,
  threaded through `register_run_watch`/`spawn_agent_loop`): true only for
  the run an `Agent` call itself asked for — `launch_background`, the sync
  path — false for `flush_agent_inbox` deliveries and loop continuations.
  The streaming turn's staple and the `●` notice both gate on it, so a
  member woken by a room post mid-turn no longer lands under main's prose
  as a "Running N agents" tree, and its completion is the tree's business.
- **Perception is not presentation.** The inbox, the wake, the debounce,
  `wakes_owner` and the notification queue are untouched — every cut is in
  the view layer, which is the answer to the user's question ("main 能感知
  到吗": yes, byte-identical).

Prerequisite housekeeping: `tool/agent.rs` sat at exactly 4000 lines, so the
two NOTE constants moved verbatim to `tool/agent_notes.rs` (3902 after).
Docs: feedback-states v1.81, README/zh, guide.md. Tests: the four arrival
pins rewritten (`a_message_from_an_agent_writes_no_line_and_counts_as_mail`,
`a_streak_of_notices_reads_as_one_batch`) plus two new gate pins
(`a_delivery_triggered_run_completes_without_a_notice`,
`a_streaming_turn_staples_only_its_own_dispatches`).

### D115. The status layer is the summons

The user's ruling narrowed ctrl+t — "ctrl+t 只和 task 展示有关" — and the
pull model needed its bell; one batch, because both are the same layer.

- **ctrl+t toggles the task panel, full stop.** D104's second stop retired:
  the tree's real door was always `shift+↑/↓` (which opens selecting), the
  pills name it every frame, and a cycle stop that duplicated a named door
  was spent on nothing. The panels stay exclusive both ways.
  `open_agent_tree` is `#[cfg(test)]` now — only tests want an unselected
  open.
- **Member rooms join the tree and the pills** (index space: `-1` main,
  instances, rooms, hide). A room row is `#dev-team: 3 members`; enter zooms
  the room; non-member rooms stay in the ctrl+b dialog (D95's membership
  rule worn by the switcher, and Slack's own: no badges for rooms you never
  joined — one post joins you, and the join starts the badge).
- **Two-tier badges everywhere the store counts** (`badge_of` +
  `push_badge`): unread = bare dot in text colour; mention (D99's `names`
  accounting) = `•N` in the accent, bold — the ctrl+b dialog's `Tone`
  grammar spread to rows and pills. Agent unread folds in `agent_mail`
  (D114's mirror). Entering reads: `enter_zoom`'s `set_active` clears the
  buffer and the mail dot. A badge fingerprint on the slow poll
  (`observe_badges`) dirties the frame when one moves.

Docs: feedback-states v1.82, README/zh status-layer sections, guide.md.
Tests: ctrl+t cycle tests rewritten as `ctrl_t_toggles_the_task_panel_alone`;
keys panel test updated; new pins `member_rooms_join_the_tree_and_enter_zooms
_one`, `a_rooms_unread_is_a_dot_and_a_mention_counts`,
`mail_to_main_lights_the_senders_dot_until_its_zoom_is_visited`.

### D116. The needs-you tier

The inbox program's third act: D114 closed the flow, D115 hung the badges,
and this batch builds the whitelist's interrupts — the three things allowed
to come to the user.

- **The `⚑` mention line.** A member-room post naming the user (the D99
  `names` accounting, now `pub(crate)`) draws one
  `⚑ #dev-team @qa: <excerpt>` flow line — flag and room in the accent,
  author in their colour, stamped like the alert — and rings D79. The edge
  detector is `observe_badges`' fingerprint: a Channel buffer's mention bit
  turning on pushes the line once; further mentions wait behind the lit
  badge until the room is read, and the active conversation never flags
  (the store cannot set mention on what is being looked at).
- **Waiting on you is a state.** `asking_instance` parses the pending ask's
  `{instance} · ` reason prefix against the roster (the subagent prompt
  surface's own format; main's asks match nobody): the row turns to
  `waiting on you (permission)` in the accent, the pill takes the flag
  tier. The ask dialog already named its source in its question line.
- The `⚠` alert and the permission dialog were already whitelisted; nothing
  else interrupts.

Docs: feedback-states v1.83 (+ the Room-relays row's one exception),
README/zh, guide.md. Tests:
`a_room_post_naming_the_user_leaves_one_flag_line` (once per turn-on,
re-armed by reading, ordinary posts never),
`a_pending_subagent_ask_marks_the_row_waiting`. 1480 + 13 green.

### D117. The wake gate — delivery and waking come apart

v6's first engine batch (design: notes/design/conversation-model-v6.md, the
@-mention system the user ruled). The old room contract woke every idle
member on every post — one "Hi" was N model calls — and v5 explicitly left
member-side debounce unbuilt. The @ decides now: delivery is untouched
(every member's inbox still receives every line, in total order, budgets
and serial checks byte-identical), but *waking* is earned.

- **Mentions resolve at commit.** `channels::post` scans the text with
  `mention_tokens` — same `part_of_a_word` boundaries as `names`, one
  predicate, two readers — against the roster plus `@all` (`ALL_NAME`,
  now reserved in `claim_name`). `PostOutcome::Sent` carries
  `Vec<RoomDelivery { member, msg, mentioned }>` and the
  `unknown_mentions` that resolved to nobody; the sender's tool result
  names those, and names a mentioned member whose copy a stop dropped —
  a needs-you-now promise silently unkept is worse than a bounce.
- **One gate, three doors.** `inbox_wakes(entry, now)` is the single
  predicate: Direct/FollowUp always pass; a mentioned Channel line passes;
  unmentioned lines pass in bulk (`ROOM_UNREAD_WAKE` = 5) or on age
  (`ROOM_UNREAD_MAX_AGE` = 120s — above main's 15s digest deadline, below
  the 300s ack default). `flush_pending` consults it (and drains whole:
  once woken, read everything), `finish` consults it before continuing (a
  lone unmentioned line no longer chains runs), and `take_direct_inbox`
  became `take_interrupting_inbox`: a mention releases *all* queued room
  lines in order, because injecting msg #7 while #5–6 stay queued would
  push the seen cursor past lines the context never held.
- **A mention pulses; a batch waits.** `deposit` now signals `inbox_tx`
  for mentioned items only, so a running member absorbs a mention at its
  next tool boundary; unmentioned traffic never interrupts a turn. The
  age half is enforced by one per-registry sweeper (`ensure_room_sweeper`,
  CAS-armed on first delivery, `Weak<Session>`, 15s cadence): it re-runs
  the boundary flush and wakes nobody whose inbox does not pass — an
  empty inbox never wakes, so a quiet room costs one lock scan and zero
  model calls. The user's ruling, verbatim: no new messages, no read.
- **Prompt patched for the mechanics only.** CHANNEL_NOTE gained one
  paragraph — named lines reach you at once, unnamed lines in batches,
  `@` what needs someone now — so the note stops implying a timeliness
  the engine no longer grants. The who-spoke reply doctrine itself is
  rewritten in D119; between the two batches the machine is honest even
  where the etiquette is dated.
- Deliberate non-changes: a mention does not revive a Stopped member
  (D105a's one door stands, the tool result says so instead); `@all` has
  no rate limiter (the 50/500 budgets and the fire-alarm prompt rule in
  D119 govern it); the sweeper's wake attribution keeps the recovery-flush
  owner quirk (query.rs's per-round flush has always had it).
- Prerequisite commit: the address grammar (`Address`/`parse_address`/
  `check_target`/`rooms_allowed`) moved out of `tool/agent.rs` into
  `tool/address.rs` (the file sat at 3936/4000), and `names` moved from
  `tui/buffer.rs` into `channels.rs` where the mention engine lives.

Docs: guide.md (channel section states the gate), tool descriptions.
Tests: `unmentioned_room_lines_wake_in_bulk_or_on_age`,
`mention_tokens_share_names_word_boundaries`,
`post_resolves_mentions_against_the_roster`,
`a_mention_interrupts_and_a_misfire_is_reported`; the accumulate/stamp
tests updated to the gate with their v6 reasons inline. 1484 + 13 green.

### D118. Main joins its own team — the pen

The user's ruling closed the gap D117 left open: the wake gate governed
every member except the one with no registry entry. `channels::post` had
relayed every room line into `main_mail` unconditionally since D29, which
after D117 made main the one member a room could still spam awake — and
v5's law 5 ("perception is not presentation") said so proudly. Reversed,
deliberately: main is a member under the same @-rules as everyone else.

- **The pen.** `Inner::main_pen` holds unnamed room lines per room
  (`MainPen { lines, first_at }`). `post` routes through
  `pen_or_release`: a line naming main (`@main`, `@all`) releases the
  room's pen ahead of itself — order within a room is preserved — and
  goes straight to `main_mail`; an unnamed line pens up and bulk-releases
  at `ROOM_UNREAD_WAKE`. The age half is `pump_main_gate(max_age)`
  (parameterized so tests can force expiry), called from `digest_mail` on
  the frame clock and from the query loop's main-guarded drain — so a
  running main picks up aged lines at its own turn boundary, exactly
  where a member absorbs its batch. `main_gate_waiting` keeps
  `needs_tick` honest: a held pen keeps the frame loop ticking toward
  the release; an empty pen keeps main asleep, same as an empty inbox.
- **What was never a relay is never penned.** The frozen-budget `⚠`
  lands directly (a runtime warning, not room speech); DM mail
  (`deliver_to_main`, arrivals mirror, `urgent`) is byte-untouched; the
  2s/15s digest debounce still shapes delivery — of what the gate has
  released. Main's serial `seen` cursor keeps its old semantics (a pen
  release is not a read; the stale bounce remains main's catch-up).
- v5's delivery table row "room post → member deposit, debounced digest"
  is now "room post → member deposit behind the wake gate; main_mail
  behind the pen". The screen side of that row (badges, no flow lines)
  is untouched until the view batches.

Docs: guide.md (digest paragraph), feedback-states v1.84 (+ header
desync v1.80→v1.84 repaired — v1.81..83 shipped without bumping it).
Tests: `main_hears_a_room_through_the_gate` (mention releases in order,
@all passes, bulk at five, own posts never relay),
`an_unnamed_room_line_waits_in_the_pen_and_release_starts_the_clock`
(no quiet window on penned mail; the clock starts at release);
`post_fans_out…` and `post_stamps…` updated to the gate with reasons
inline. 1486 + 13 green.

### D119. The @ decides what you owe

The doctrine catches up with the machine. D112's reply rule — *who spoke
decides* — was the best available reading while every post woke every
member: obligation had to be inferred from rank because timeliness was
uniform. D117 made timeliness a bit the sender spends, so obligation now
follows the same bit, and the pair of prompts is rewritten around it.

- **CHANNEL_NOTE, the member half.** "Who spoke decides" is replaced by
  "**The `@` decides what you owe**": a line naming you needs you now
  (act or answer, in the room, this turn); `@all` keeps D112's *covered*
  answer clause — the anti-chorus rule survives on the one broadcast form
  left; a line naming nobody is FYI whoever wrote it — the sender who
  wanted an answer had the `@` and chose not to spend it. The batch rule
  says what waking on unnamed backlog means: read, and if nothing changes
  what you are doing, end the turn without posting. D48's lesson survives
  as the one exception — a question the batch shows still unanswered, the
  user's especially, deserves its answer from whoever holds it. Sender
  discipline is now explicit: `@` what needs someone *now*, leave FYI
  unnamed, `@all` is a fire alarm. "Never answer an answer", the venue
  rule (D67) and the DM privacy lane (D63) stand verbatim.
- **MAIN_CHANNEL_NOTE, the new half.** v5 deferred "main's room-digest
  narration discipline (prompt layer, extends D112) — observe D112
  first"; this is its due date, with D118 the forcing function: main's
  room lines arrive inside the `<messages>` envelope with nothing
  anywhere explaining them, and the base prompt's instinct — talk to the
  user — is exactly the narration flood v5 cut from the screen. The note
  names main a member, states its two wake tiers, points its answers at
  `SendMessage(to: "#room")`, forbids narrating room traffic at the user,
  and binds it to the same sender discipline. Injected in `main.rs`
  beside the crew note, same `agent_channels` gate, same system-block
  reasoning (compaction never touches `Session::system`).
- **SUBAGENT_NOTE untouched, deliberately**: nothing in it is about
  rooms, and its one adjacent claim — background tasks do not wake you —
  stays true: a room wake is a delivery, not a background task.
- Anchor churn: the retired `` `user` or `main` addressed the room ``
  assert is replaced by "The `@` decides what you owe" / "one *covered*
  answer" / "still unanswered" / "fire alarm", plus the MAIN_CHANNEL_NOTE
  set ("needs you now", "Do not narrate room traffic",
  `SendMessage(to: "#room")`, "fire alarm"). Two anchor phrases were
  caught straddling a hard line break while writing this batch — the
  exact failure the anchor tests exist for — and rewrapped.
- Known limit: the MAIN_CHANNEL_NOTE injection point lives in `main.rs`'s
  binary assembly, which no unit test exercises (the crew note has the
  same shape); the anchor tests pin the words, the gate is one `if` beside
  a proven one.

Docs: guide.md (doctrine paragraph rewritten, main's half added),
SendMessage room description states the send-side discipline.
Tests: anchor set reworked as above. 1486 + 13 green.

### D120. An agent's page is main's page

The v6 headline, and the batch the wake gate was built for. The user's
ruling, verbatim: enter an agent and it is *exactly like main* — same
rendering, same conversation logic; main is just a slightly specialized
agent. v4/v5's answer was the alt-screen zoom: a second renderer over
flat post rows, no scrollback, no trace. Retired whole.

- **One pipeline.** `conv.rs` builds an away page's messages from the
  domain — `perspective::walk` for attribution (the same single walk the
  pair lane uses), `group_ready_tool` (extracted from the live ToolReady
  path) for collapse groups, so an agent's settled record folds exactly
  the way the console folds its own work. The build swaps the page's
  messages into `Chat::messages` for the duration of one `build_rows`
  call: main's fields always describe main, main's events land in main's
  store while any page is up, and the render/flush/scroll/click layers
  needed no changes at all — the doc and its cursors simply describe
  whichever page is active. A `speaker` field on `UiMessage` carries what
  the marker walk cannot (a room's members); `None` falls back to
  `speaker_of`, so main's flow is byte-identical.
- **Switching is a page turn.** `term::page_break()` returns from
  fix/page-switch@cf22b59 (D98's primitive, ported with its tests): rows
  above the viewport bank into the terminal's own scrollback, the
  viewport is erased, the next page starts at the top. Coming home parks
  the flush cursor at the end and `rehydrate`s a windowful — the resize
  machinery, D27's accepted duplicate. Fullscreen just swaps the doc.
- **The page is live.** A fingerprint over the domain (history length,
  live block sizes, in-flight, pending, state) rebuilds the page on
  change; the streaming run rides after the settled messages as a
  volatile block (prose as markdown, one dim row per tool call), never
  flushed. The stable prefix is a pure function of append-only history,
  which is what lets the flush cursor trust it.
- **Same conversation logic.** Typing on a page is prose to its subject
  through the `@name` grammar's own delivery (a `/` line is a message, a
  stopped agent resumes, a room seats you before it speaks). Esc grew
  two rungs on the one ladder: `AwayStop` (stop the page's run — main's
  turn is out of reach while a page is up; ctrl+c keeps the override)
  and `AwayHome` last. shift+tab cycles the viewed agent's mode and the
  footer badge follows it. The room page is **speech only** — the v6
  ruling — membership lines stay in the log and off the page.
- **Deleted:** the zoom modal loop and its keys, `zoom_posts`/
  `record_posts`/`channel_posts`/`settled_post_rows`/`sender_runs` (the
  whole second renderer), `PostKind::{Queued,Typing}`, the
  `open_zoom`/`close_zoom` signal fields, `zoom_chrome`/`zoom_footer`.
  Net −1,200 lines while gaining full-parity pages.
- Known limits, recorded honestly: history-derived Done rows carry no
  per-call duration or token counts (the record never had them — D99's
  limit, unchanged); the live tail is flat rows rather than the activity
  tree (the registry publishes strings); ctrl+o's pager follows the
  active page via `build_rows` but its fold toggles act on main's
  messages while away. guide.md's zoom/keys sections are rewritten in
  the next batch together with the roster's key changes — the two
  batches rewrite the same sections and land adjacent.

Docs: this record (guide/feedback-states fold into D121's sync).
Tests: the zoom's modal suite retired with the modal; the page suite
replaces it (`a_page_is_the_transcripts_own_pipeline`,
`a_room_page_is_speech_only`,
`typing_on_a_page_reaches_the_agent_as_the_user`,
`a_message_to_a_stopped_agent_resumes_it`,
`a_room_page_joins_before_it_speaks`,
`esc_stops_the_run_first_and_comes_home_second`,
`shift_tab_cycles_the_viewed_agents_mode_and_not_mains`,
`entering_reads_the_conversation_and_leaving_gives_it_back`,
`the_page_closes_when_its_subject_is_gone_and_stays_when_done`,
`a_switch_owes_a_page_turn_and_home_reprints_the_tail`,
`enter_on_a_tree_row_switches_comes_home_or_collapses`), plus
`page_break_banks_the_page_and_erases_the_tail` in term.rs.
1463 + 13 green.

### D121. The roster — the rows under the composer

The v6 view's second half, and the user's screenshot made literal: the
conversations line up under the composer the way Claude Code's own agent
list does — `● main` first, then every agent and every room you are in,
at most three rows with the cursor scrolling the window — and there is
no key to learn.

- **`roster.rs`.** Rows are resolved from the stores each frame (the
  registry for state, `status_label` for the wording, `badge_of` for the
  two tiers, `asking_instance` for the waiting-on-you accent) and drawn
  flat: presence dot (`●` running / `○` idle / `·` stopped), the name in
  its identity colour (bold where the page is open or the cursor is),
  the badge, the status copy, `↑/↓ N more` on the window's edges. Zero
  conversations, zero furniture.
- **The fallthrough is the door** (the user's ruling: no new chord).
  `↓` in the composer walks the draft, then history, and at history's
  end falls onto the rows — CC's own three-level fallthrough. `↓/↑`
  move, `↑` off the top returns to the draft, `Enter` opens the row's
  page (main's row comes home), `Esc` drops the cursor, `k` stops a
  selected running instance through the one stop path, and any printable
  character gives the keyboard straight back to the draft (CC's
  type-to-exit). The cursor is one `EscLayer::Roster` rung — the rows
  themselves are furniture and never close.
- **Retired with it:** the agent tree (shift+↑/↓, the `-1..hide` index
  space, the panel), the footer pills, ctrl+shift+o's per-row preview,
  and — the user's second ruling — D116's `⚑` flow line: a mention now
  lights the badge in constant view and rings once per turn-on
  (`observe_badges` keeps the edge detector and the bell, drops the
  line), re-armed by reading the room, silent for the room you are
  standing in. `tree.rs` shrinks to the shared helpers every surface
  reads.
- ctrl+t loses its last coupling (the tree no longer yields a slot);
  keys.rs's panel rows say the new grammar; guide.md's status-layer,
  zoom and quick-start sections are rewritten for pages + roster (the
  D120 sync folded in here, as recorded there).

Docs: guide.md (quick start, conversation rows, pages, dialog, team
paragraphs), feedback-states v1.85 (+ rows: room relays lose their one
exception, page send/round-trip, lifecycle surfaces). Tests:
`the_rows_lead_with_main_and_wear_the_status_copy`,
`the_window_is_three_rows_and_follows_the_cursor`,
`down_falls_in_up_comes_back_and_typing_leaves`,
`k_stops_only_the_running_row_under_the_cursor`,
`enter_on_a_roster_row_switches_and_main_comes_home`,
`a_room_post_naming_the_user_rings_once_and_writes_nothing`; the tree's
suite reduces to the helpers'. 1453 + 13 green.

### D122. The v6 file, and the shelf cleared

The documentation batch the user authorized in the same breath as the
refactor: "过时的 Decision, 也可以删掉" — outdated decisions may go.

- **`notes/design/conversation-model-v6.md` is the authority**: the model
  (user / main / rooms), the laws (delivery ≠ waking; main is a member;
  the @ decides what you owe; a page is main's page; the roster; @user is
  a badge), the byte-contract inventory (v6 added none), the per-batch
  global rules migrated verbatim from the interaction blueprint (their
  only other home), the batch table with commit hashes, and the
  deliberate non-builds.
- **Deleted**: conversation-model-v2/v3/v4/v5.md and
  interaction-blueprint.md. Each declared its successor in prose; none
  had a machine-readable header; all of it is git history and D-records.
  `notes/research.md` stays append-only — the records are the history the
  design files never were.
- **Synced**: AGENTS.md's decision-record range ("D1-D75" had been stale
  since August 12) now points at the v6 file for the model and the global
  rules; both READMEs' status-layer and zoomed-view chapters rewritten
  for the roster and the pages (keys tables included); the one dead link
  in feedback-states' v1.81 changelog entry annotated rather than
  rewritten — a changelog is history too.

Docs: this record. Tests: unchanged — 1453 + 13 green.

### D123. The briefing duty — D119's narration ban reversed

One day after D119 shipped, the user ruled against its second paragraph in
as many words: "main 应该向我转述房间内的情况" — main should relay what is
happening in the rooms. D119 had banned narration fearing v5's flood ("they
can open every room themselves"); the ruling reads the same facts the other
way: the roster and the pages are pull surfaces, and a main that reads its
digests in silence leaves the user watching a team that looks idle unless
they go door to door themselves.

- **The machine is untouched.** The ban was one paragraph of
  `MAIN_CHANNEL_NOTE`; the pen, the gate, the debounce and every byte
  contract are exactly as D117/D118 left them. What made the reversal safe
  is that the flood guard was never really the silence — it is the pen:
  room lines reach main at most once per mention/5-lines/120s release, so
  a briefing inherits that cadence mechanically and per-post prose cannot
  come back.
- **What the paragraph says now**: main is the user's eyes on the team —
  when room lines reach it, its reply briefs the user on what moved (who
  did what, decisions, results, blockers, anything needing the user
  themselves). The surviving discipline is form, not volume: **a briefing
  is not a transcript** — own words, compressed (one sentence can cover
  five lines), verbatim record on the room's page — and a batch is not to
  be sat on. The posting half (answer a room in the room; the `@`
  discipline; `@all` the fire alarm) is unchanged.
- **Anchors**: `Do not narrate room traffic` retires from the D119 anchor
  test; `Keep the user posted on their rooms` and `A briefing is not a
  transcript` replace it, each sitting on one line (the hard-wrap trap,
  third sighting).
- **Docs synced same-batch**: the v6 file's D119 law bullet and batch
  table (title range now D117–D123), guide.md's rooms paragraph,
  AGENTS.md's decision range. feedback-states.md is untouched on purpose:
  the briefing is main's own turn prose, not a feedback state — the "room
  relays render nothing" rows legislate the *flow*, and still hold.

Docs: this record; conversation-model-v6.md law bullet. Tests: the
reworked anchor assertions in `tool/agent.rs` — 1453 + 13 green.

### D124. Silence is a turn — the empty-response guard, and D73's leftover

Reported from a live session: four crew turns in a row died with
`subagent failed: stream protocol error: the model returned no response after
the stream ended`. Nothing was wrong with the stream. `dev #2`, `ui-ux #2` and
their two chase rounds had each woken on `[#dev-team msg #3] qa: @user Hi!…` —
room traffic naming nobody, with their own greetings already posted — and did
exactly what `CHANNEL_NOTE` tells a member to do with a batch it owes nothing:
ended the turn without saying anything. `devex`, on the same model and the same
batch, wrote "No reply needed." and passed. It was a coin flip on phrasing.

- **The engine and the prompt disagreed.** `query_loop`'s classifier reads a
  turn with no tool calls and no readable text as a malformed response: retry
  once, then `QueryError::Protocol`. That is right for a session answering the
  user and wrong for every wake-driven turn — a member draining an FYI batch,
  or main pumped by an aged pen with nothing to brief. The second silence now
  ends the turn instead: `QueryEndReason::EmptyResponseRetried`, neither
  attempt recorded (history stays as clean as the first retry left it), the
  inbox **not** restored — silence is the absorbing state `channels.rs` always
  claimed it was — and the subagent's result reads `[subagent returned no
  text]`, the sentence `non_empty` already held for it.
- **The repeat was the restore.** Failing the turn put the drained batch back;
  the chase redelivered it; the same lines produced the same silence, once per
  round. Completing the turn ends that loop by construction.
- **The error named the transport for a decision the model made.**
  `QueryError::Protocol` renders as `stream protocol error: …` and is also what
  a genuinely dead stream returns (`query_turn.rs`), so main read the wording
  and told the user it was transient. The real ladder — 429/5xx/overloaded,
  reconnect notices, ten attempts — is untouched; it was never in this path.
- **The retry was invisible.** `model returned an empty response; retrying
  once` is gated on `!session.quiet`, and every subagent inherits the TUI's
  quiet, so the user saw the verdict and never the first attempt. Left as is:
  the surviving warning is the main session's, and a crew member's private
  retry is not the user's business.
- **D73's leftover, closed in the same edit.** A turn the output budget cut off
  mid-thought has thinking as its only block and reads empty — so the
  classifier discarded it *before* the `max_tokens` recovery written for
  exactly that turn (the dnf incident, August 13; the fix was agreed then and
  never authorized). `stop_reason == "max_tokens"` now leaves the empty branch
  untouched: the truncated content enters history and the resume prompt
  continues it, thinking-only included.
- **The prompt half.** "End the turn without posting" meant *do not put words
  in the room*; models read it as *produce nothing at all*. `CHANNEL_NOTE` now
  says which silence it means — **Silence belongs in the room, not in your turn
  text** (one line, the anchor trap's fourth sighting) — because turn text is
  the only thing that reaches main, and closes with the line the note asks for.
  `stop calling tools` gained `and say so in a line`.

Docs: this record; conversation-model-v6.md's FYI law; feedback-states v1.86
(§4 empty-turn recovery, §7 acceptance anchor). Tests: two new in `query.rs`
(silence completes and records nothing; a thinking-only `max_tokens` turn
recovers), one new `CHANNEL_NOTE` anchor assertion, and the old
`SERVER_ERROR` expectation reshaped to the silent outcome — 1455 + 13 green.

### D125. Reasoning has two event names on the Responses wire

User report: the same DeepSeek model shows its thinking through the official
`api.deepseek.com/anthropic` endpoint and shows nothing through a
Responses-protocol proxy. Not a capability difference and not a missing
request parameter — the tokens arrived and the adapter dropped them.

- **The wire, verified against the endpoint**: the `reasoning` output item
  opens (`output_item.added` → `ThinkingStart`, mapped since D33), a
  `content_part.added` announces `reasoning_text`, and every token then comes
  as **`response.reasoning_text.delta`**. D33's mapping reads
  `response.reasoning_summary_text.delta` — the *summary* stream, which is
  what models that keep their reasoning hidden emit. Same `output_index`,
  same string `delta`, different name; the second name fell through to the
  ignore arm. The result was the worst shape of all: a thinking block opened
  and never filled, so the affordance rendered empty rather than absent.
- **The fix is the alias**: one match arm now reads both names. The anthropic
  adapter never had this problem — `thinking_delta` is parsed unconditionally,
  which is why the official endpoint always worked.
- **Not built, on the user's call**: `supports_thinking` stays false for the
  deepseek family, so bingo still sends neither `reasoning.effort` nor the
  `include` on the Responses path. The endpoint accepts both (probed: 200 with
  reasoning either way) and DeepSeek reasons regardless, so the only cost is
  that `thinkingLevel` cannot move its depth there. The flag answers "may I
  send the Anthropic-shape thinking parameter?" and the Responses path borrows
  it for a different question; splitting it per protocol is a real change and
  waits for a reason beyond this one.

Docs: this record (research.md is append-only — D33's mapping line stands as
written; this is the amendment). Tests: one new SSE case in
`api/providers/openai.rs` — 1456 + 13 green.

### D126. DeepSeek takes the thinking parameters — the gate was an assumption

D125 left `supports_thinking` false for the deepseek family and said splitting
it per protocol would wait for a reason. The user asked for the level to work,
so the assumption underneath got tested instead of split: **both endpoints take
bingo's parameters**, and the flag is now true.

- **Probed on the wire, not reasoned about.** The Anthropic-compatible endpoint
  (`api.deepseek.com/anthropic/v1/messages`) answers 200 to bingo's exact pair —
  `thinking:{"type":"adaptive"}` + `output_config:{"effort":"max"}` — and
  returns its thinking block as before. The Responses shape behind the
  openai-protocol proxy deserializes `reasoning.effort` into a typed enum and
  says so when it rejects one: *unknown variant `bogus`, expected one of `none`,
  `minimal`, `low`, `medium`, `high`, `xhigh`, `max`*. Every level bingo can
  send is in that set. The old comment's claim — "their endpoints take
  DeepSeek-shaped thinking parameters, not the ones bingo sends" — was never
  checked against either endpoint.
- **What the user gets**: `thinkingLevel` (and `/think`) now reach a DeepSeek
  model instead of being nulled in `query_turn` before the request is built.
  `effort_for` lets the deepseek prefix carry `xhigh`/`max` verbatim, as gpt-5.6
  already did — capping them at `high` would have thrown away the two tiers the
  endpoint explicitly accepts.
- **What it does not buy, measured**: eight runs across unset/low/high/max on
  the Responses path showed no depth signal — reasoning length varied more
  within a level (386–877 chars) than between levels. The parameter is parsed
  and validated upstream; whether DeepSeek acts on it is not visible from here,
  and is not claimed. On the Anthropic shape the pair is accepted and probably
  ignored (DeepSeek documents `reasoning.effort`, not `output_config`), which
  costs two ignored fields per request and no behaviour.
- **The blast radius is the family, not the endpoint**: any deepseek-prefixed
  model now gets the parameters on any provider. An endpoint that refuses them
  says so where it always could — `model-catalog.json`'s `overrides`, or a
  per-provider `models` declaration, both of which outrank the compiled table.
- **Pinned**: the table test that asserted the denial now asserts the reverse
  with the reason; `qwen-max` inherits the "family that takes no thinking
  parameter" role in it, in the resolver's re-enable case, and in the system
  capability block's `Thinking: no` case — where DeepSeek now demonstrates the
  two capabilities are independent (it reasons and cannot see).

Docs: this record; guide.md's `thinkingLevel` row (the `off` line no longer
calls itself DeepSeek-compatible); the `wire_thinking` comment carrying the
retired assumption. Tests: 1456 + 13 green.

### D127. A room page names its speakers (v7 batch 0)

Reported while reviewing the conversation model: "房间界面 里面不同的人说话分不
清" — a room page's messages are indistinguishable. Confirmed, and it is not a
perception problem.

- **Half of D113 was never built.** The ruling was "avatar and name, or name
  alone". `Gutter::cells` returns nothing at all when `faces` is false, and the
  row builder's `Some(_) => Vec::new()` arm meant a message with a known
  speaker and no portrait drew *nothing* — no name, no colour, no initial.
  `experimental.chatAvatars` is off by default, so the default rendering of a
  room was one anonymous voice, with `UiMessage::speaker` sitting there unread.
- **The name is the identity when no portrait is**: one row per run, `@dev` in
  the colour the roster gives that same name (`Palette::avatars` through
  `Gutter::index_for`, the identity-colour path, not the avatar path).
- **Only an explicit speaker takes one.** Room and agent pages carry
  `UiMessage::speaker`; main's own flow leaves it unset and derives its two
  participants from the text (`speaker_of` answers `main` for every assistant
  message), so gating on the *explicit* field keeps the console byte-identical
  and off the write-once tests' toes. The user's own bubble is exempt: `❯`
  already says who they are.
- **The pager's folds followed the page.** `transcript_rows` saved, expanded
  and restored fold state on `Chat::messages` — which always describes main —
  while `build_rows` underneath swapped the away page's document in for the
  draw. The content was right all along; `a` (expand everything) was a dead key
  on a page. Both now go through `active_messages`.

**Correction to the same session's review**: the claim that `ctrl+o` shows
main's transcript while a page is open was wrong — it renders the active page,
and always did. What was broken is the fold bookkeeping around it. And the
inline console has no scroll of its own *by design* (the rows belong to the
terminal, D27/D98); the pager is the scrollable surface, and a dead wheel is
the host's mouse mode, not bingo's.

Docs: conversation-model-v7.md (proposed) records this as batch 0. Tests: a
room page names both speakers and the name leads its run; main's flow grows no
speaker rows — 1458 + 13 green.

### D128. The duties: obligation stops being an inference (v7 batch 1)

The user's reading of the room prompt, after watching members get it wrong:
"感觉模型分不清什么时候该说什么时候不该说". They are right, and the fault is the
note, not the models. D119's `@` rule was sound; the clauses around it asked for
a judgement nothing in the model's input can answer — *"if nothing in them
changes what you are doing, end the turn without posting"* and *"a question the
batch shows still unanswered"*. A member has no way to know whether a line
changes what it is doing until it has already decided. D124 is what one of those
calls cost.

**Obligation now has two sources, both observable.** An `@` on your name, and a
question whose sender is `user` — the second keeps D48's lesson (a room where
nobody answers the human is worse than one that chatters) while replacing its
inference with a `from` field. A line naming nobody owes nothing at all, and the
note bans the judgement outright: *"Never work out what you owe by judging
whether a line matters or changes what you are doing. You cannot see enough to
judge that; the member who wrote it could, and if they needed you they had the
`@`."*

**Three rules protect the sigil**, each replacing an appeal to restraint with a
statement about the wire:

- an acknowledgement does not discharge an `@` — *"if the answer is 'already
  doing it', that sentence is the answer"*;
- an answer never `@`s the person it is answering. This is "never answer an
  answer" made mechanical: a ping-pong needs the sigil to keep going, so the
  rule names the sigil instead of asking for restraint;
- a name being quoted is written without the `@`, so a recap does not put the
  whole room on the hook.

**Main's half became four tiers** rather than one duty: quote what names the
user (a question is not activity to be compressed), one line for a state change,
**hold pure progress** — *"say nothing, and know it"*, the row that makes main
worth waking at all — and nothing for discussion. `@user` is main's to carry:
verbatim to the user, the answer owed back to the *room*, and answering on the
user's behalf is allowed where main already knows, with one hard stop —
**never state a position the user has not taken**. That is the sentence guarding
the place a model is most fluent and most wrong.

**Deliberately prompt-only.** The v6 wake machine is untouched, and every timing
sentence still describes it truthfully ("names you — reaches you at once; does
not — in batches, later"). v7's wake rule lands in a later batch; a note
promising immediacy the runtime does not have would be a lie the model would
plan against. The batch is ordered this way on purpose: the gates are cheap to
keep and expensive to restore, so the duties get observed under load before
anything is deleted.

Docs: conversation-model-v7.md (batch 1 of the table), guide.md's room
paragraph. Tests: five new `CHANNEL_NOTE` anchors and two `MAIN_CHANNEL_NOTE`
anchors, replacing D119's two — 1458 + 13 green.

### D129. The wake rule: a non-empty inbox, and nothing else (v7 batch 2)

The observation window batch 1 was ordered for closed itself in one afternoon.
The user posted a greeting into `#dev-team` at 14:04 and screenshotted a roster
of idle members 44 seconds later. The transcript says the gate worked exactly as
built: all five members woke at **t+135s** — `ROOM_UNREAD_MAX_AGE` (120s) plus
one `ROOM_WAKE_SWEEP` tick — read the line, and correctly posted nothing, in the
new duties' own words ("no one named, nothing owed", "owing it nothing, staying
out of the room"). D128 converged; only the two-minute lag was wrong.

- **`inbox_wakes` is `!entry.inbox.is_empty()`.** The count and age gates were
  proxies for a question the sender now answers with the `@`. The count was also
  an amplifier: in a room of six, one round where everyone speaks leaves five
  unread in every inbox and re-crosses the threshold for all of them — a knife
  edge, not a margin (a five-member room is one line short of oscillating). Two
  properties are kept: an empty inbox never wakes, so nothing polls and a quiet
  room is free, and the predicate is still one function behind every door.
- **A running member absorbs everything at its tool boundary.**
  `take_interrupting_inbox` retires into `drain_inbox`: v6 took only
  mention-bearing batches and then had to take every queued line with them
  anyway, to keep the seen cursor honest. Taking all of it is the same behaviour
  with the special case removed — and it is what the user asked for in one word:
  *steer*.
- **Main's pen is gone.** `MainPen`, `pen_or_release`, `release_pen`,
  `pump_main_gate`, `main_gate_waiting` and the three pump points delete; `post`
  pushes the relay line into `main_mail` the moment it lands. Main was a special
  case in four places for a job every member does with none. The 2s/15s digest
  debounce stays — it coalesces a burst into one turn and holds nothing back,
  which is the opposite of a gate.
- **The sweeper and its arming CAS delete** (`ensure_room_sweeper`,
  `room_sweeper_armed`, `ROOM_WAKE_SWEEP`), and `InboxItem::Channel::arrived_at`
  with them: it existed to drive the age half and nothing else read it.
- **Not built, and named**: no per-member debounce. An idle member woken by the
  first line of a burst absorbs the rest for free at its tool boundaries, so a
  burst already costs about one wake; a second coalescer would be machinery for
  a case the architecture handles. `max_awake` is batch 3's.

Net: −5 constants, −5 methods, −1 background task, −1 struct field, −1 atomic.
The behaviour it buys is the one the user asked for: post, and the room moves.

**D128 patched in the same batch.** Main narrated three near-identical lines
about five members reading a greeting, because the four tiers live in
`MAIN_CHANNEL_NOTE` and a task notification is not a room line. The tiers now
say what they always meant — they cover *everything that reaches main about
somebody else*, and "a turn ended with nothing to report" is named as pure
progress: *five members each reporting that they read a greeting is five lines
that say nothing happened.*

Docs: conversation-model-v7.md (status, batch table), v6's two superseded law
headers, guide.md's two gate paragraphs, both module docs. Tests: the bulk/age
test replaced by v7's wake rule, main's pen test by "hears every room line at
once", the interrupt test by "both lines ride the same boundary", the pen-clock
test by the debounce alone — 1458 + 13 green.

## D130 — an agent's page shows what came back, not only what was asked

The user reported that an agent's page "looks different from main's" and could
not say how. It does, and the difference was not in the renderer: both pages run
the same `UiMessage → Block → Doc` pipeline (v6's ruling), but they are fed from
two different places. Main's activities are filled by the live events —
`UiEvent::ToolDone` carries status, output and duration; the reasoning stream
carries the text. An agent's page is *rebuilt* from its API history by
`perspective::walk`, and that walk matched four block kinds. `ToolResult` was not
one of them.

Rendered side by side (the scratch that became the tests in `chat_tests_f`), the
same run gave:

| | main | agent's page |
|---|---|---|
| reasoning | `✻ Thinking … +2 lines (ctrl+o to expand)` | `✻ Thinking` |
| a call that worked | `⎿ Done (ctrl+o to expand)` | `⎿ Done` |
| **a call that failed** | `⎿ Failed` | **`⎿ Done`** |

The third row is the one that matters: on the page whose whole job is telling
you what an agent did, a failure was drawn as a success. The answers were in the
record the whole time — one `tool_result` block per call, sitting in the very
history the page is built from.

- **`walk` collects results in a pre-pass** and hands each to its call
  (`Work::Tool { result }`). A call and its answer live in two different
  messages, so a forward pass would have to patch a row it had already emitted;
  one pre-pass keeps the walk a walk. `Work::Thinking` carries its text the same
  way.
- **`RunBuilder` fills what the console's `ToolDone` fills**: status from
  `is_error`, expandable content from `result_content`, the counted
  `result_summary` inside a fold and the `✦` line for Skill outside one, and the
  same summary source main uses (`summarize_input`, not `hint_for` — the two
  disagreed, so the row read `WebFetch(url="…")` on one page and `WebFetch(…)` on
  the other).
- **A call with no answer in the record reads `Interrupted`.** A committed
  history is written when the run ends, so a call still unanswered in it never
  got an answer. Borrowing `Done` would report a completion that never happened.
- **Prose closes the open fold**, exactly as `UiEvent::TextDelta` does. Without
  it a sentence between two tool streaks was swallowed and one summary spanned
  the change of subject.
- **Work rows carry their message's stamp.** They used to carry zero — a process
  row has no send time of its own — but a run takes its clock from its *first*
  row and most turns open with reasoning, so the whole block rendered stamped
  zero, which `buffer::stamp` draws as no clock at all.
- **`Thinking::timed`.** Duration is the one fact that is genuinely not in the
  history, and `✻ Thinking for 0.0s` would be a measurement nobody took. The
  flag says whether a clock was taken rather than letting zero mean two things;
  a fast turn on main still reports, because main always takes one.

What still cannot be reconstructed, and is left alone deliberately: per-call
duration (never enters the record) and the `Diff` activity for edits (the unified
diff rides `ToolResult::diff` to the UI, not into the protocol block). Both
degrade to the honest thing — no duration tail, and an ordinary tool row.

`EXPAND_HINT` and `skill_result_summary` were lifted out of `chat.rs` rather than
copied: a row on one page advertising a different key than the same row on the
other is the exact class of drift this record is about.

**`query.rs` split (same commit).** `scripts/check_discipline.sh` had been red
since **D124** — the empty-turn tests pushed the file from 3941 to 4063 lines,
past the 4000 cap — and D124 through D129 were reported as five-gate green when
four had been. The tests move to `query_tests.rs` behind `#[path]`, which is what
`query_steer_tests.rs` already does and for the same reason. `mod tests` is still
`query::tests` and `use super::*` still reaches the loop; nothing changed but the
file the lines live in.

## D131 — the `@` becomes a debt the runtime keeps (v7 batch 3)

Ordered with `max_awake` explicitly cut ("我感觉那个没必要"), which is the right
call and worth recording as a ruling rather than a scope trim: a cap on how many
members may wake is the last survivor of the gating instinct v7 reverses. The
storm was never everyone *reading* — it was everyone *speaking*, and R1 is what
stops that. Queueing members behind a bound would also make the one real latency
worse, since a room's delay is a member's own turn.

**What was missing.** The `@` is the room's only obligation (R1) and it was the
only obligation nothing recorded. A direct message has carried a delivery record
since D44; a mention in a room ran bare. So a silent member was four situations
wearing one face, and only one of them is fine:

| | before | now |
|---|---|---|
| has not read it | invisible | `owes #build #5 · unread` |
| read it, working | invisible | `owes #build #5 · Reading…` |
| read it, not answering | invisible | `owes #build #5 · Idle for 2m` — a bug you can see |
| its turn died | invisible | the state says so, and the chase says so again |

D124 was row four: four crew turns died, main reported a transient stream error,
and it took a screenshot to find out.

**The ledger.** `channels::Mention { seq, from, to, at, answered }`, opened by
the sigil in `post` and closed by the named member's next post to that room.
Two decisions worth their reasons:

- **Speaking is the answer.** No judgement of substance, because R2 already tells
  the model an acknowledgement is not one, and a runtime that second-guessed the
  wording would be making exactly the inference v7 removed from the prompt.
- **Close before open, in one pass.** A member that answers and asks in the same
  breath settles the old debt and opens the new one; a post can never settle the
  question it is itself asking.
- **`@all` is one debt against the room** (R4), closed by the first answer from
  anybody but the asker — not one debt per member, which is the shape that would
  have turned one greeting into five obligations.

**The cursor, finally shown.** `Channel::seen` has existed since the beginning
with exactly one reader, the serial staleness bounce, and has never been on
screen. `standing_of` pairs it with the debt, and that pairing is the whole
difference between rows one and three above: `unread` means the line is not even
in the member's context yet.

**The surfaces.** The roster, not the room page: a page header is a settled row
under write-once and a debt is volatile, so the live answer belongs on the rows
that redraw every frame — which is where v7's own mock put it.

```
○ dev        owes #build #5 · unread
# build      waiting on @dev · 3m 12s
```

The accent stays reserved for *you* being the holdup, so a room waiting on `main`
or `user` colours and a room waiting on a member does not: news, not a prompt.

**The chase** (`spawn_mention_watchdog`) mirrors D44's, and is a parallel
mechanism rather than a rekeyed `AckState` — the direct record is per-instance on
a `MsgId`, the room's is per-room on a sequence, and forcing one into the other
would couple two lifetimes with no reason to move together. Five minutes, three
rounds, the same owner-addressed watch line back to the sender. `@all` is chased
without nudging anybody: the sigil deliberately did not pick a member, so neither
does the chase — the sender is told and the room's row says what it is waiting on.

**Two things deleted on the way.**

- `deposit`'s `if mentioned { notify_inbox() }` — v6 pulsed only for a mention
  because an unmentioned line was on a batch clock it must not jump. D129 deleted
  that clock and left the condition: the last place the runtime still read the
  `@` as a wake bit instead of an obligation. Every deposit pulses now.
- `InboxItem::Channel::mentioned` — its own doc said the bit "is what the
  obligation ledger will key on". The ledger keys on the room's record instead,
  which is strictly better: it outlives the inbox item, and one record answers
  both the chase and the row so the two can never disagree.

**A prompt that had gone false.** `CHANNEL_NOTE` still said *"room traffic that
does not name you reaches you in batches, later"* — written true under v6, made a
lie by D129, and a member that believes a line is still in flight has a reason
not to answer it. Both notes now say every line arrives at once and the `@`
decides only what is owed. A member is also told the debt is recorded and chased:
that is a fact about the world, not a new rule, and a model that does not know it
is being watched cannot factor it in.

## D132 — one builder for a page, running or not

The user's correction, and it was the same one from the start of the thread: *an
agent's page should be exactly main's page, except main talks to the user by
default.* D130 made the two agree about **history**; they still disagreed about
**now**, because a page had two renderers and only one of them had been fixed.

- **Settled half** — `walk` → `RunBuilder` → activities: results, folds, status,
  stamps.
- **Moving half** — `LiveBlock` → `live_block_rows`, twenty lines with none of
  them.

Rendered, the moving half read:

```
✻ Thinking…                 ← no content, no size, no key
⏺ ⏺ Read(a.rs)              ← glyph applied twice; no result row, ever
partway through the answer
```

The doubled glyph is the whole bug in one row: `on_tool_ready` pre-rendered
`⏺ Read(a.rs)` into a string and `live_block_rows` prefixed `⏺ ` to it again.
Nobody had a reason to notice, because nothing else looked at that string.

**Why it read as lag rather than as ugliness.** Nothing was late — the
fingerprint is compared every frame and the deltas land per event. What
happened is that the *same content* was drawn twice: poorly while it streamed,
then replaced wholesale by the rich rebuild when `finish` wrote the history. A
result that had been sitting in `ToolCallDone` for a minute appeared only when
the run ended. Meanwhile the page had no status row at all — `chrome` read
`chat.away.is_none()`, correctly refusing to describe main's turn on somebody
else's page, and then drew nothing in its place. No spinner, no clock, no token
count, no key to stop with. A screen with nothing moving on it reads as stalled
however fresh its rows are.

**The fix is a deletion.** `LiveBlock::Tool` carries the call —
`LiveTool { id, name, input, answer }` — instead of a rendering of it, and
`live_message` runs the whole tail through the same `RunBuilder` the settled
half uses. `live_block_rows`, `away_live_blocks` and `AwayBuild::live` delete
with their drawing branch; the running turn is just another volatile message
past `stable`, which is what the queued echoes have always been.

Three things fall out of having one builder:

- `on_tool_done` fills the answer **in the round it arrives**, matched on the
  protocol's own id rather than on position, because a round runs tools
  concurrently and they do not come back in call order.
- `RunBuilder::tool` takes what silence *means* from its caller: `Interrupted`
  in a committed history (written at run end, so a call still unanswered never
  got one) and `Running` in a live tail (it simply has not come back). The two
  callers mean opposite things by the same absence, and now they say so.
- `perspective::ToolOutcome` and the live tail's answer collapse into one
  `agents::ToolAnswer`. One type for "what a call returned" is what makes
  drawing the two halves differently impossible rather than merely unintended.

**And the row that says something is happening.** `page_running_status` gives an
open agent page the status row main gets, from numbers the registry has carried
all along (`elapsed`, `output_tokens`, `recent_activity`). The token figure is
the registry's count, not the animated meter — the meter tracks main's stream,
and borrowing it would show main's numbers under somebody else's name. `esc` says
which of its two meanings it has here (D39's rule, applied to the new surface).

Same run, after:

```
✻ Thinking … +2 lines (ctrl+o to expand)
⏺ WebFetch(url="https://x.dev/a")
  ⎿  Done (ctrl+o to expand)
⏺ WebFetch(url="https://x.dev/b")
  ⎿  Running…
```

**`agent.rs` split** (same commit, same reason as D130's `query.rs`): 4002 lines
against a 4000 cap. The tests move to `agent_tests.rs` behind `#[path]`, which is
what `agent_notes.rs` was carved out for in D114 and what `query_tests.rs` did in
D130.

## D133 — a conversation becomes a thing, instead of a privilege of the console

The user's ruling, and it is the same one D132 was answering: *我们切换 agent 只是
相同的 chrome 切换不同的数据来源* — switching agents is the same chrome pointed at a
different source, and main differs in exactly one way, that it talks to the user
by default. The code says the opposite. Main is a set of fields on `Chat`, fed by
`UiEvent` as its turn runs; every other conversation is an `AwayPage` rebuilt
from the registry whenever a fingerprint moves, and swapped into those same
fields for the length of one `build_rows`.

That asymmetry is where the last two records came from. D130: the rebuild lost
tool results, so a failed call rendered as a success. D132: the rebuild's live
half had a renderer of its own, so a running call drew twice and differently.
Both were fixed where they showed. The composer is still doing it — `/`, `!` and
`@name` branch on `away.is_some()` and are dead on an agent page.

D133 is the first of four steps and runs nothing new: `Conversation` is lifted
out of `Chat` and `Chat` holds one of them. No test's assertions moved; the only
edits to test bodies are the field paths they name.

**Where the line is: what there is one of.** One terminal, one composer, one
input history, one theme, one tick — the console. One transcript and the turn
writing into it — a conversation. Eighteen fields cross:

- the transcript and what is waiting to enter it — `messages`, `queued`,
  `next_queue_id`;
- where the turn is writing — `stream_msg`, `stream_attempt_checkpoint`,
  `continuation_msg`, `thinking_buf`, `thinking_seg_open`;
- whether it is running, and how it ended — `busy`, `interrupted`;
- what it has spent — `output_tokens`, `output_round_tokens`, `token_rate`,
  `context_usage`;
- and its clocks — `turn_start_tick`, `turn_started`, `settle_at`, `turn_verb`.

Each is the transcript, an index into it, a byte produced into it, or a clock the
turn writing it started. 702 sites become `self.conv.<field>`, 390 of them in the
six `chat_tests_*` files — which is itself the measurement: the console reaches
into one conversation's state that often and until now had no name for what it
was reaching into.

**No `Deref`.** It compiles and it would have made the diff a tenth the size, and
it would have borrowed the whole `Chat` at every one of those 702 sites while
hiding the split this record exists to make visible. The point is that the reader
can see which half of the state a line touches.

**What stayed, and why.**

- `token_meter` — the *display's* travel toward `output_tokens` (D87). D132
  already ruled that an agent's page shows the registry's count rather than this
  meter, because a meter borrowed across pages puts main's numbers under somebody
  else's name. It eases one status row and there is one status row.
- `steer`, `cancel_tx`, `interrupt_at`, `bash_tail`, `live` — wires into the turn
  that is running now, not a record of it. Who holds them once more than one turn
  can run is the question D135 answers when submit stops branching on `away`;
  moving them today would be guessing at that answer.
- `last_progress_tick`, `warnings`, `last_error`, `last_prompt` — each measured
  from "the last event that reached the TUI" and rendered in exactly one place.
  They stay console until a second conversation can produce one, which is D134.
- `pending_tools` — the honest miss. It holds activity indices into
  `conv.messages[stream_msg]`, awaiting `ToolReady`; it fails the console test the
  same way `thinking_buf` does and belongs beside it. It stayed because D133's
  contract was the eighteen fields the plan named, and because cross-talk here is
  impossible while one stream exists. It becomes a real bug the moment D134 puts a
  second conversation's events on the same channel, so it moves there.

**One widening.** `stream_attempt_checkpoint`, `output_round_tokens`,
`turn_start_tick` and `turn_started` were private to `chat.rs`, which `chat_tail`
and the test modules could still read as descendants of it. In `conversation.rs`
they are `pub(super)`, i.e. `pub(in crate::tui)` — the smallest visibility that
keeps those readers compiling, and the only visibility this change loosens.

`conversation.rs` is a sibling of `chat`, not a child of it: `chrome`, `roster`
and `chat_menus` already read these fields, and a conversation is not an
implementation detail of the state machine that happens to hold one. It sits
beside `conv.rs`, which is the projection D134 deletes.

No file crossed the 4000-line cap and nothing needed splitting; `chat.rs` fell to
3729. `chat_tests_b.rs` is the one to watch at 3993.

**Completed in a follow-up.** `pending_tools` failed the split's own test and was
left behind: it is a FIFO of activity indices into `messages[stream_msg]`, so it
belongs to the same conversation those two do. It moved, and `mail_wake` — the
one borderline field the record did not rule on — is recorded as console state
with its reason: it gates *when the console starts a turn*, not what any
conversation contains, and it retires in D136 when main's mail becomes an
ordinary inbox. D134 is now purely about event routing.


## D134 — the console stops having a favourite

The user's ruling, in their words: *"我们切换 agent 只是相同的 chrome 切换不同的数据来源"* — switching
agents is the same chrome pointed at a different data source, and main differs in exactly one way:
it talks to the user by default. The architecture said otherwise. Main was the source and everybody
else was a **projection**:

| | main | an agent |
|---|---|---|
| how its rows arrive | `UiEvent`, pushed per delta | rebuilt from `history` + `live` |
| when | as it happens | every frame, if a fingerprint moved |
| how much | the delta | the whole record, from scratch |

Three decision records were spent on symptoms of that one fact before it was named: **D130** (the
projection matched four block kinds and `tool_result` was not one, so a failed call rendered as a
success), **D132** (the projection's moving half had a renderer of its own, with no result rows, no
folds and no status), and the composer, which still branches on `away.is_some()` — D135's problem.

**One channel, addressed.** `UiEvent` travels as `Addressed { to: ConvKey, event }`, and producers
hold an `EventSink` bound to a conversation: *the producer says what happened, the sink says whose
turn it happened in, and nothing downstream has to guess.* `subagent_hooks` stops accumulating into
`entry.live` and emits onto the console's own channel, so an agent's stream reaches the very handler
main's does. `EventSink::detached()` is the headless case — a run with no screen drops its events
the way a closed channel does, so no producer branches on having an audience.

**One store per conversation, the active one inline.** `Chat` holds `conv` (on screen) plus
`parked: HashMap<ConvKey, Conversation>`. Main sits in `parked` like anybody else while the screen
is elsewhere. Keeping the active one inline rather than behind a lookup is what makes `Chat::conv`
infallible: the renderer, the composer and the status row read it every frame, and a lookup that
could miss would be a panic path in all three.

**First sight is an event, not a page opening.** An instance streams from the moment it is spawned;
`detach` opens a store on the first event addressed to it, so a page opened later shows what
happened meanwhile. Cold start — a conversation the console never saw stream, restored from a
session — is `agent_history`, one `walk` at open time, and then pure append. The per-frame full
re-walk is gone; so are `AwayPage`, `AwayBuild`, `fingerprint`, `agent_messages`, `live_message`,
and the swap-in/swap-out around `build_rows`.

**`LiveBlock` dies with the projection it fed** (`LiveBlock`, `LiveTool`, `push_text`,
`push_thinking`, `answer_tool`, `set_live`, and `AgentView`'s live and in-flight halves). It existed
to carry a running turn across the gap between the domain and a view that could not see events.
There is no gap now.

**Write-once, which was the risk.** An agent page had its own settled boundary (`AwayBuild::stable`);
it now uses the console's, `message_static_settled`, the same one main's rows have always been
flushed by. One rule for every page, and no second place for a half-finished row to slip into
scrollback from.

**Two facts a store cannot infer, so it carries them.** `intake_seen`: the first user-role text in an
agent's record is the task it was dispatched with and every one after is somebody talking to it, but
"first" cannot be read off the transcript — `TurnStart` opens the turn's own message before the
prompt arrives, so a store guessing from emptiness would file a spawn task as main speaking, and the
same run would render one way live and another way re-read from history. `projected`: how far a
room's log has been copied in, because a room is the one conversation with no turn loop.

**Rooms stay a projection, deliberately.** A room is a log, not a turn loop — there is no stream to
push. It keeps a cursor (`projected`) and appends its tail, so even that path stopped rebuilding.

Net for `src/`: +2036/−1387 across 32 files, and the tests grew 1473 → 1481. It is not the deletion
the shape promises; that arrives in D135, when `submit` and the command paths stop branching and the
second input path retires with them.

**Provenance.** The implementation is an Opus 5 (max effort) agent's, run under this session's
orchestration; it completed and passed all five gates but timed out before writing this record and
bumping `AGENTS.md`. Both were written here, after reading the diff — a record nobody read the code
for would be the wrong kind of record. It was then reviewed adversarially by Fable 5 (xhigh).

## D134a — what the review caught

D134's review (Fable 5, xhigh) found one blocking defect and three real ones. The
blocking one is worth the space, because it is the same class D134 exists to
close and it walked straight back in through the new door.

**The answer rendered above the question.** The runtime's event order is fixed by
construction: `TurnBrackets::open` sends `TurnStart` before `run_query` hands the
prompt to `on_inbound`. So by the time an agent's intake arrives, `TurnStart` has
already opened the turn's own message — and the handler appended. Every agent
turn watched live read reply-first, and because it is the *live* half, write-once
banked the inversion into scrollback permanently. Re-read from history it came
out in the right order, which is exactly the live-versus-settled drift D130 and
D132 were spent on.

The suite was green because the new tests asserted the inbound lines were
*present* and never that they were *in order*. A reviewer reproduced it with a
scratch render rather than reasoning about it, which is why it was found.

`Conversation::absorb_inbound` now decides by what the turn has said:

- **Nothing yet** — splice above the open message, shifting `stream_msg` and
  `continuation_msg`. "Nothing yet" has to count the placeholder reasoning block
  `TurnStart` opens, or the mail lands under an empty row that renders above the
  question anyway. (`pending_tools` holds *activity* indices inside one message
  and is untouched by a message splice; `stream_attempt_checkpoint` is a clone,
  not an index.)
- **Something already** — mail absorbed at a tool barrier belongs below what was
  said and above what comes next, so append and open a continuation. That is the
  shape `absorb_steered` has given main since D83; `open_continuation_message`
  moved onto `Conversation` and both paths call the one implementation.

**Three more, all of them D134 making a console-wide assumption reachable from
every conversation:**

- A call still running when a turn ends never got a `ToolDone` — the run was
  aborted under it. It read `Running…` for the rest of the session *and pinned
  the flush cursor with it*, because `message_static_settled` refuses a running
  activity and settlement is prefix-monotone: an unbounded redrawable tail on
  that page. Before D134 a stop was survivable because the page re-read the
  committed history and D130's rule gave `Interrupted`; the live store is
  authoritative now and has to correct itself. `TurnEnd` closes running tool
  calls as `Interrupted`, beside the running thinking blocks it already closed.
- `bash_tail` is the console's one foreground `!` command. Its two clears were
  written when only main could send those events; an instance running Bash, or
  merely ending a turn, blanked the tail under the user's own running command.
  Both gate on `is_main` now.

**Left open, deliberately, and named here so it is not lost:** the cold-start
race (`open_conversation` walks the registry at page-open time without draining
the channel first, so a turn committed to history and still queued as events can
render twice, permanently) and the loss of the pending/in-flight echo for
main-originated sends. Both need a ruling rather than a patch; D135 is where the
input paths merge and is the honest place for them.

**A second review, independently.** The D134 commit was reviewed twice — the
first pass caught the three defects above while the tree was still uncommitted;
a second, pinned in an isolated worktree at `7895023`, reproduced all three with
compiled probes rather than by reading, and traced the write-once boundary end to
end: settlement is prefix-monotone over `message_static_settled`, read from the
active store, and the abort corner fails *safe* — it starves the flush rather
than banking a volatile row. It added two:

- **A warning had no sender.** An instance's `Reconnecting… 2/10` reached the
  console's shared warning tier unattributed, so it read as main's stream while
  the user watched main. It wears `@name` now, and the reconnect dedupe keys on
  the sender as well as the prefix — keyed on the prefix alone it collapsed
  main's retry and an instance's into one line that alternated between them.
- **A test was weakened that did not need to be.** `✻ Thinking` had been relaxed
  to `✻`; the stronger assertion passes unchanged. Restored. That is the exact
  failure mode the review was asked to look for, and it was found by diffing the
  test files rather than by trusting the suite.

And it read the record against the code, which is what a second-hand record is
for: `pending_of`'s doc claimed D134 replaced the away page's echo "with the
console's own echo at send time". Only the *user's* sends are echoed. Main's
dispatches and mail are not, so a user watching a busy instance cannot see what
main just asked it. The doc says so now, plainly, instead of overstating the
replacement.


## D135 — one input path, and the one command that follows the page

The placeholder said `/ for commands · ! for shell` on every page, and on every
page but main's it was a lie. `Chat::submit` opened with

```rust
if self.away.is_some() { self.submit_to_zoom(); return; }
```

and `submit_to_zoom` read the whole line as prose. So on an agent's page `/` did
nothing, `!` did nothing, `@name` and `#room` were dead, and the three output
tiers (`slash_lines`, `slash_error_lines`, `slash_info_lines`) were gated on the
page being main's — the display half of the same split, which would now swallow
the answer to a command the user had just run.

v6's reasoning was that a page's composer addresses the page, and CC's teammate
route does return before any slash handling. It addresses the **console**. A
terminal command and shell mode are not properties of what you happen to be
reading; a page turn is not a modal, and D132 already stopped treating it as
one. **One `submit`**: take the draft, the `@name`/`#room` grammar, the queue
behind the console's turn, `!`, `/` — the same steps in the same order wherever
the screen is — and one last step that reads `active`, because where *prose*
goes is the only thing a page decides. `submit_to_zoom` and `ZoomTarget::direct`
retire into it; `zoom.rs` keeps the target vocabulary and loses the composer.

**What the merge made visible, and what it cost.**

- **Whose `busy`.** `self.conv.busy` is the *active* conversation's since D134,
  and a `/model` typed on scout's page must not read scout's turn. `waits_for_main`
  names the rule instead of leaving it in the shape of a branch: a command, a
  shell line, or prose to main waits behind main's turn; prose to anybody else
  never queues here at all, because their queue is their inbox and the domain
  already runs it. `slash_cd` and `set_model` were reading the same wrong field
  and now read main's; `slash_clear` was clearing the store on screen.
- **The queue rows are main's page.** A command queued from somebody else's page
  joined a queue nobody could see, which is a keystroke that did nothing. It
  says `queued behind main's turn` on the tier that does show.
- **`/theme` walks every store**, not just the one on screen: diff rows are baked
  at edit time, and a parked conversation the reader comes back to would still be
  wearing the old palette.
- **Shell mode stopped being cancelled by a page turn** (`switch_to` cleared it),
  which is the same complaint one level down, and `EscLayer::BashMode` lost its
  `is_main` gate with it — a mode you turned on is a mode you close before the
  page under it closes.

**The `/compact` ruling.** The user's, verbatim: *大多数照旧作用于控制台会话（它们
本来就是控制台的设置），但 `/compact` 在 agent 页上应该压缩这个 agent 的上下文 —— 因为
shift+tab 已经立了这个先例，而 compact 是唯一一个"作用错对象会真的有损失"的.*
Implemented as stated. Most commands keep acting on the console because acting on
the console *is* acting on what the user meant — they are settings, and a setting
has one owner. Compaction is not a setting: it rewrites a context, and rewriting
the wrong one destroys work that cannot be got back. `shift+tab` has cycled the
viewed agent's permission mode since D105, so the precedent was already paid for;
this is the one command worth spending it on again.

An instance's context is not a transcript file — it is `Entry::history` in the
registry — so the rewrite is read, summarise, write:

- The summarising call runs on the **instance's own session**, which is what
  opened `AgentRegistry::session_of` to production. Through the console's session
  it would have appended the compact marker to the *console's* transcript (D74's
  `append_compact`), which is the wrong record; the instance has no transcript, so
  the marker is correctly a no-op.
- `replace_history` **refuses while the instance is running**, and the refusal is
  the design rather than a gap: a turn in flight holds its own copy of the history
  and writes it back at `finish`, so a summary spliced in underneath would be
  overwritten by the next round. The state is read under the same lock the write
  takes, so a run that starts in between loses the race instead of the work. The
  error says `esc to stop it, then retry`, which is a ladder the user already has.
- A **room** answers that there is nothing to compact. It is a log, not a turn
  loop; falling back to the console's context there would be exactly the
  wrong-target loss this ruling exists to prevent.
- The `✓`/`⏳` lines take the console's tiers (they answer the console's command,
  on whatever page is up); the new window figure takes a sink bound to the
  instance, because it is the instance's footer that reports it.

**The cold-start race** (deferred by both of D134's reviews). The suggested fix
was to drain the channel before opening a store. Drained alone, it does not
work — and the test written first is what said so. The drain routes the queued
events, `detach` opens the store for the first of them, and `detach` walked too:
the walk simply moved, and read the same already-committed history.

The walk's real precondition was never "no store yet", it is **"nothing is
waiting to replay what I am about to read"**. It reads the registry as it stands
*now*; an event in hand describes something that happened *then*. So the walk
leaves the event path entirely — `detach` opens a **blank** store — and survives
only behind a full drain, where the channel is empty and no store means the
console has genuinely never heard of the conversation. Blank costs nothing: both
production `insert` call sites (`tool::agent`, `team::spawn`) create an instance
with an empty history, so walking at first sight was walking nothing in every
healthy case and walking a duplicate in the broken one. `echo_direct` claims its
store the same way, because a store opened there was the same cold start racing
the same events.

**Left undone, and named so it is not lost:** main's own sends to an instance are
still invisible on that instance's page until the run absorbs them at a barrier.
The user's sends are echoed at send time (`echo_direct`); `Agent` dispatches show
up as intake the moment the run opens, so the gap is `SendMessage` mail to a
*running* instance. Closing it means echoing from `AgentRegistry::deliver` — the
one point every sender passes through — and dropping the absorb-time replay for
`Target::Dm(MAIN_NAME)` the way `inbound_messages` already drops the user's. That
is a domain-layer emit and a second dedupe rule, and it belongs with D136, where
main becomes a registry entry and `deliver` stops having a special sender at all.

**Changed assertions.** One: `typing_on_a_page_reaches_the_agent_as_the_user`
asserted that `/help is not a command here` reached scout's inbox and that the
console's parser never saw the draft. That is the behaviour this record reverses,
so the test keeps the half that is still true (prose reaches the subject as the
user) and the `/` half becomes its own test asserting the opposite. Nothing else
in 1490 tests moved.

**`chat.rs` split.** 4070 against the 4000 cap. `/compact` moved to
`chat_session.rs`, which already owns `/rename`, `/resume`, `/gc` and `/share` —
the commands that act on a session's record on disk — rather than to a new file
for three functions.
