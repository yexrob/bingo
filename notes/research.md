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
### D20f. 频繁 resize panic（落盘游标越界）

- **症状**：连续 resize 时 `thread 'main' panicked at components.rs: range end index 28 out of range for slice of length 26`。
- **根因**：resize 宽度变化 → `build_rows` 重排后 `doc.rows` 行数收缩（窄宽度折行更少），但落盘游标 `printed` 仍是旧值（> 新 `doc.rows.len()`）；落盘 slice `rows[*p..live_start]` 与重播 slice `rows[*p-replay_n..*p]` 越界。`live_start = max(printed, ...)` 也依赖未 clamp 的 printed。
- **修复**：inline 分支开头（`live_start` 计算**之前**）clamp 落盘游标 `*p = min(*p, doc.rows.len())`——全部 slice 恢复安全。
- 验证：随机宽度（40-120）快速切换 25 次 × 5 轮 + 期间发消息，无 panic；178 测试全过。

### D20g. 输入真实光标（iocraft 无光标 API 的布局等价）

- **诉求**：输入时终端真实光标应停在输入文本后（参考体验），而非 `▋` 假光标"在最后飘"。
- **机制**：渲染循环输出后把真实光标定位到组件声明位置（输入框的 cursorOffset）；视口外才冻结。
- **iocraft 0.8.4 无此 API**（渲染后光标固定停在 canvas 末行末尾；TextInput 也是假光标——absolute 定位色块）。等价实现：**输入行做成 canvas 最后一行**——iocraft 渲染后真实光标自然停在输入文本末尾。inline 模式 chrome 顺序调整：tasks/warn/waiting/footer/上边框/**输入行（最后）**；输入行去掉 `▋` 假光标。全屏模式保持原布局（▋ 假光标）。
- **连带排查（未修，环境特异）**：pty 测试环境下键盘事件偶发不达——iocraft `Terminal::new` 同步调用 `crossterm::supports_keyboard_enhancement()`（发 `\x1b[?u` 等响应，~2.5s 超时），无响应的 pty 里该查询可能吞掉窗口期输入；HEAD 基线（全屏）同样复现 → 非本改动引入；真实终端有响应、无感（用户实际打字正常）。

### D20h. 输入框回归完整框 + 真实光标方案否决

- **症状**：D20g 的"输入行最后"布局（无下边框、footer 上移）被用户否——"输入框不对 并且无法输入"。
- **复盘**：用户截图 ❯ 后文字可见（输入实际工作）——"无法输入"实为光标感知问题：D20g 去掉 ▋ 假光标后，iocraft 渲染完的真实光标停在 canvas 末行（不跟随输入），用户看不到输入位置。真实光标跟随（ink frame.cursor）在 iocraft 0.8.4 不可实现（无组件声明光标 API）。
- **修复**：恢复完整输入框（上边框 + `❯ {input}▋` + 下边框 + footer 下方）；inline 模式输出 `\x1b[?25l` 隐藏真实终端光标（避免 footer 行尾真光标与 ▋ 混淆），退出时 iocraft 自动 Show。验证：上下边框/▋/光标隐藏序列齐全，178 测试全过。

### D20i. 事件全断根因：渲染期间写 State 的渲染风暴（clamp 每帧 write）

- **症状**：真实终端（用户 + tmux 复现）打字/Ctrl+C/resize 全部失效；pty 环境显示启动输出膨胀到 122KB（每帧全量重画）。
- **根因**：D20f 的落盘游标 clamp `*p = (*p).min(doc.rows.len())` **每帧执行**——iocraft `State::write`（DerefMut）**无条件**标记 `did_change` 并 `waker.wake()`（use_state.rs:149-163，即使值相同）——组件**渲染期间写 state → 唤醒 → 立即再渲染** → 渲染风暴 → 渲染循环的 `select(root.wait(), term.wait())` 里 `root.wait()` 永远 ready → **`term.wait()` 饿死 → 终端事件（键盘/resize）全部不达**。Ctrl+C 退出、resize 重播（依赖 Resize 事件触发 replay）随之失效——用户"resize 功能没了"实为同一根因。
- **修复**：clamp 只在值变化时写（`if clamped != *p { *p = clamped; }`）。排查了全部渲染期 state 写点：落盘/replay/cursor_hidden/last_size 均有条件守卫，唯 clamp 遗漏。
- **验证**（tmux 真实终端，查询有响应可完整复现）：打字 `❯ zz▋` ✓；Ctrl+C 退出（pgrep 0）✓；resize 100→60 视口重播（welcome 60 宽重排）✓。178 测试全过。

### D21. `!` 命令（bash 模式）

- **输入面**：输入为空时按 `!` 进入 shell 模式（`!` 本身不插入输入）；bash 模式下空输入按退格退出；输入非空时 `!` 正常插入。模式是**粘性**的（提交后保留）。UI：输入前缀 `!` + 输入框边框换 `bashBorder` 色；footer 提示 `! for shell mode`。
- **执行面**：**不经模型、不经 UserPromptSubmit hooks**——命令经统一权限门（PreToolUse hook + canUseTool + 用户确认）+ Bash 工具 + PostToolUse hook 执行；UI 复用现有工具活动行（ToolUseStart/ToolReady/ToolDone）。历史按真实工具轮形状写入（`<bash-input>` 用户文本 → 合成 assistant ToolUse → 用户 ToolResult——**API 要求 tool_result 必须与同请求内 tool_use 配对**），输出 HTML 实体转义（`& < >`）后包裹 `<bash-stdout>`。
- **模型回应**（`respondToBashCommands`，settings 键同名，默认 true）：true → 执行后照常进入 queryLoop（模型可见输出并可继续）；false → 纯执行，并在历史前注入 caveat（`<local-command-caveat>` "DO NOT respond to these messages…"）防模型把输出当指令。中断/PostToolUse 阻断同样走"不查模型 + caveat"路径。
- **实现落点**：`run_query` 拆出 `query_loop`（循环体）+ `tool_context`，`run_bash_command` 与 `run_query` 共用；TUI `Chat` 新增 `bash_mode` 状态与 `start_bash_turn`。
- **取舍**：stdout/stderr 不再分离（双标签）——bingo 的 Bash 工具本就合并输出且含 `$ cmd` 回显与退出码，模型信息不缺失；周期/后台命令（`!watch …`）直接复用工具的后台化语义。权限拒绝也回填模型（与 run_query 的 `<permission_error>` 惯例一致，失败也查模型）。
- **验证**：184 测试全过（新增：`!` 切换、bash 提交收尾、执行消息形状与转义、组件渲染前缀/边框/提示、settings 合并）；PTY 冒烟：`!` 前缀 + `! for shell mode` 提示 + `!echo hello` 直接执行（`✓ Bash $ echo hello · 9ms … +3 lines`）后保持 bash 模式。

### D21b. 交互式/TTY 命令拒绝（`!` 与 Bash 工具共用）

- **动机**：bingo 子进程 stdin/stdout 均为管道但**继承控制终端**——全屏 TUI（top/htop/vim）输出乱码、ssh/fzf/sudo 直连 `/dev/tty` 抢占终端（raw mode 下画面被撕毁）、裸 shell/REPL 无输入即退出（无意义）。参考实现侧只做提示（"Interactive terminal apps can't be driven by an agent's bash tool" + tmux 包装惯例），bingo 直接执行前拒绝。
- **实现**：`tool/bash.rs::interactive_command_reason`（`!` 与 Bash 工具共用）——解包 sudo/env/nohup/command/exec/doas 包装后按命令名与参数判定：
  - **恒拒**：系统监控（top/htop/btop…，`-b/--batch` 快照放行）、编辑器（vim/nano/emacs…）、文件管理器（ranger/yazi/mc…）、TUI 工具（lazygit/tig/fzf/k9s/screen…）、`docker/kubectl attach` 与 `exec/run -it`、`tmux` 前台（`new -d`/send-keys/capture-pane 等脚本用法放行）、gdb（`-batch` 放行）。
  - **裸拒**：shell/REPL（bash/python/node…，带参数放行）；DB 客户端（sqlite3/psql/mysql/mongosh/redis-cli——无 `-c/-e/--eval` 等执行旗标、无 `<` 重定向、无 SQL/脚本位置参数即交互提示符；`--version/-l` 等非交互用法放行）。
  - **ssh**：`-t` 强制 tty 或"仅主机无远程命令"（口令/远程 shell 占 `/dev/tty`）→ 拒；`ssh host 'cmd'` 与 `-N/-f`（端口转发/后台）放行。`sudo -i/-s` 与裸 sudo → 拒。
- **落点**：`BashTool::call` 顶部（模型路径生效，`<tool_use_error>` 回填）；`run_bash_command` 在**权限门之前**预检（`!top` 不弹无意义的权限询问；respond 开启时模型可见拒绝原因并可提示替代，如 `top -b -n 1`）并直接发 `Warning` 事件——折叠行在 inline 模式落盘后无法展开，拒绝原因必须以警告行呈现。工具描述同步声明交互式命令被拒。
- **验证**：187 测试全过（新增拒绝/放行正反例 ~60 条 + 工具层与 `!` 路径拒绝断言）；PTY 冒烟：`!top` → `⚠ interactive command not allowed: top 是全屏交互监控程序（需要 TTY），已拒绝。一次性快照可用 \`top -b -n 1\``，命令未执行、回到 bash 模式提示。

### D21c. `!` 命令输出预览（BashModeProgress）

- **症状**：`!pwd`/`!ls` 执行后输出被折叠组吞掉——折叠摘要行（"Ran 1 bash command"）不含输出，且 inline 模式落盘后无法展开，用户看不到命令结果。
- **机制**：bash 模式进度 = `<bash-input>` 行 + 与普通工具结果同款渲染器的 fullOutput——输出直接展示在命令下方（长输出折叠 "+N lines"）。
- **实现**：`UiHooks.on_tool_ready` 与 `UiEvent::ToolReady` 增 `standalone: bool`——`!` 命令的 Bash 活动标记 standalone：只设摘要、**不参与折叠组**（模型驱动路径传 false，折叠行为不变）；ToolDone 时非折叠 Bash 活动**默认展开**，内容 = 输出去掉 `$ cmd` 回显与 `[Exited with code N]` 尾注（`bash_output_preview`），复用 layout_activity 既有的 `⎿` 连接行渲染。
- **验证**：189 测试全过（新增 standalone 折叠判定正反例、预览展开与剥离断言）；PTY 冒烟：`!pwd` → `⏺ ✓ Bash $ pwd · 8ms` + `⎿ /Users/yexrob/Episodes/Projects/bingo`；`!ls` → `⎿ AGENTS.md` + 缩进续行，多行输出完整可见。

### D22. AskUserQuestion 工具（问用户选择题）

- **契约**：`questions[1..=4]`，每题 `{question, header?（≤12 字符）, options[2..=4] {label, description?}, multiSelect?}`；问题文本与 option label 各自唯一。v1 实现：`multiSelect: true` 报错引导改单选；`preview` 字段不纳入 schema（UI 无预览面板）；header 缺失时以「问题 N」为题。
- **执行**：`src/tool/ask.rs`——`is_concurrency_safe=false`（阻塞等待回答，串行）；逐题调用 `ToolContext.ask_question`（None = Esc 跳过，后续问题不再问）；结果 `The user answered: "q"="a", ...` 或 `The user did not answer the questions.`；模型端输入校验失败（数量/唯一性）以 `tool_use_error` 回填。
- **UI 复用**：AskUserQuestion 走既有权限模态（`PermissionRequest` + 1-9 键 + Esc）——`UiHooks.ask_question` 与 `ask` 同通道，`Confirm(i) → Some(i)`、`Cancel → None`。**不经权限门**（对话框本身即审批；run_query 门内按名短路）。工具行摘要显示问题文本。子代理（Agent 工具）的 ask_question 恒 None（无 UI 可问）。
- **连带修复**：`schema_for` 此前丢弃 schemars generator 的 definitions——嵌套类型（`Vec<AskQuestion>` 经 `$ref` 引用）发给模型的 schema 悬空。现把 `generator.definitions()` 合并进根 schema（对扁平工具无影响）。
- **取舍**：多选/Other 自由文本/预览面板为后续项（多选组件与文本输入需扩展模态协议）；超时不做（默认 never，无限等待）。
- **验证**：196 测试全过（新增：schema 形状含 definitions、输入校验反例、回答/跳过/多选拒绝、执行队列串行化、TUI 挂钩 Confirm/Cancel 映射）。

### D24. MCP 管理：McpManager 连接缓存 + /mcp 命令

- **机制**：启动并行连接全部 server（batch，失败不阻塞）；连接状态 connected/failed/needs-auth/pending/disabled 进 AppState；SSE 断线指数退避重连（5 次，1s→30s）；`mcp__{server}__{tool}` 前缀进 ToolRegistry，ToolListChangedNotification 动态刷新；enable/disable 持久化 `disabledMcpServers`/`enabledMcpServers` 名单（project config）；`/mcp` immediate 命令 = 交互 UI（状态徽标 + fork/重连/日志/删除菜单）+ `/mcp enable|disable [name|all]` + `/mcp reconnect <name>` 快速路径。
- **bingo 现状改造**：原 `connect_servers` 每次回合 spawn 子进程重连（浪费）。新增 **`McpManager`**（挂 `Session.runtime.mcp`）：**懒连接**（首个回合 `connect_all`，之后复用缓存）、失败记录不自动重试（`/mcp reconnect` 手动——stdio 子进程退出即彻底失败，自动重连无意义）、`disconnect`/`set_enabled` 立即生效。
- **/mcp 命令**（argumentHint `[enable|disable [server-name]]`）：无参数列出（✓ connected · N tools / ✗ failed: 详情 / ○ disabled / · not connected）；`enable|disable [name|all]` 更新名单并**持久化 `.bingo/settings.json` 顶层 `disabledMcpServers`**（同名机制）；`reconnect <name>`（disabled 时拦截提示先启用）。
- **配置契约**：`McpServerConfig` 增 `type` 字段（TransportSchema）；**stdio**（`command`/`args`/`env`）与 **http**（`url` + 可选 `headers`，streamable HTTP）落地；sse/ws 连接时报错提示（rmcp 3.1 无 legacy SSE；OAuth 未做，静态头先覆盖）。`command` 改可选（http 无命令）。
- **权限**：MCP 工具复用统一权限门（Box<dyn Tool> 已有）；is_concurrency_safe=false（串行，保守策略）。
- **验证**：244 测试全过（新增 McpManager 状态矩阵/失败不重试/reconnect 清失败 + /mcp 列表/enable-disable 持久化/reconnect 拦截）；tmux 实测（无依赖 Node stdio server）：懒连接 2 tools、badsrv failed + 警告行、disable 断开+持久化跨会话、disabled reconnect 拦截、enable 后下回合自动连接。

### D25. 运行状态行（ActivityIndicator）

- **机制全景**（让用户知道 agent 在运行）：
  1. **状态行（ActivityIndicator）**：transcript 底部、输入框上方一行——spinner（100ms 帧）+ 动词消息（`{verb}…`）+ thinking 计时（`(thinking for 12s)`）+ 工具计时（`running tool for 3.2s`）+ 输出 token 计数（`↓ N tokens`）+ 总耗时。动词 = 运行中工具的 activeForm/subject > thinking 俏皮词 > 兜底 "Working"。**无论模型在想、在等、在跑工具，这行永远存在**。
  2. **thinking 占位**：`⠋ Mulling for 1.4s`（~150 俏皮词表）。
  3. **工具行**：输出预览——无输出时 `Running…` + elapsed，有输出时尾 5 行 + `~N lines`/`+N lines` + `(timeout 2m)`。
  4. **工具心跳**：30s 无输出心跳 progress，长任务 elapsed 持续刷新。
  5. **stall 检测**：距上次 token 10s/45s/300s 阈值，spinner 降强度/变 warning 色；429 显示 `Waiting for API response · will retry in X · check your network`。
  6. **spinner tips**：运行 >30s 提示 `/btw`、>30min 提示 `/clear`；有任务时 `Next: {subject}`。
- **bingo 实现**：`Chat::running_status()`（busy 时返回 `(动词, 耗时)`——运行中工具 summary > thinking 俏皮词 > "Working"；`turn_started` Instant 由 TurnStart/TurnEnd 设置）+ `status_row` 渲染在输入框上方（chrome 一行，inline/全屏均可见；任务区与警告行之间）。动词优先工具 summary（`$ sleep 2`），与 activeForm 语义一致。
- **验证**：198 测试全过（新增 `running_status` 动词优先级、状态行渲染断言）；PTY 实测 `!sleep 2`：`⠼ $ sleep 2 for 0.2s → … → 2.0s` 逐帧跳动，回合结束消失。
- **遗留（上游 iocraft 问题，非本改动引入）**：API 完全挂起（无任何事件）时，tick 驱动的渲染链会在 ~1s 内饿死——spinner/计时冻结在提交瞬间（基线同现；探针时序可复现/可绕过）。事件流正常时（含真实 API 的流式往返）无此问题。状态行至少在冻结前给出"Working"可见提示；彻底修复需在 iocraft 渲染循环的唤醒链上动手（`select(root.wait(), term.wait())` 对自驱动动画的唤醒竞态）。（已随 D26 重写消亡）

### D26. TUI 渲染层重写：iocraft → ratatui 0.30 + 自研 inline 驱动

- **动因**：iocraft inline 模式每帧重画整个 canvas（含永不变化的内容）、每行空格补齐到满终端宽、光标相对 diff 在终端 resize reflow 下失同步、canvas ≥ 终端高触发 `Clear(All)+Purge` 清空 scrollback。多轮补偿（chrome 记账、shrink_deficit、reflow 白名单）治标不治本——resize 楼梯残骸的根源是「重画已定稿内容」这个前提本身。
- **新架构**（codex-rs / Claude Code 同款）：定稿行经滚动区域**一次性写入终端 scrollback，永不重绘**；只有底部视口（未定稿尾部 + chrome）被重画。resize 时旧内容由终端自然 rewrap（与普通 shell 输出一致），残骸从结构上不可能累积。
- **分层**：`src/ui.rs` 渲染无关契约（UiEvent/AskRequest/PermissionRequest/DialogAction/tui_hooks，零渲染依赖——未来 GUI 对同一契约实现另一 `run_*_session`）；`tui/term.rs` 全 crate 唯一碰终端的模块（视口双缓冲 diff + insert_history + CSI ?2026 同步更新包裹，镜像 ratatui `insert_before` scrolling-regions 路径与 codex `custom_terminal`）；`tui/app.rs` 显式 `select!` 事件循环 + Frame 组装（帧高 = 实测行数，无第二套 chrome 公式可漂移）；`tui/view.rs` 纯转换（crate `Line` → ratatui text）。
- **迁移面**：chat.rs 4,100 行逻辑 + 3,300 行测试仅换 import 全量存活（iocraft 的 KeyCode/KeyModifiers 本就是 crossterm 再导出）；line/theme 换 Color 类型；components.rs（2,122 行）整体删除；iocraft 依赖移除，新增仅 ratatui（`scrolling-regions` feature 需显式开启，0.30.2 非默认；crossterm 加 `event-stream`）。
- **驱动语义要点**：视口未贴底先 `scroll_region_down` 下推（不耗 scrollback），贴底后对上方区域 `scroll_region_up` 分块入 scrollback；行尾 `Clear(UntilNewLine)` 不填空格（resize 无折行垃圾的关键）；判空用 `Cell::EMPTY` 全等——带背景色的空格是内容（用户气泡尾巴靠此保活）；`HistoryItem::Raw`（kitty 图片字节）按 rows 记账、其行永不清除；视口增高在物理底行写真实换行（唯一全终端保 scrollback 的滚动）。
- **顺带改进**：bracketed paste 真事件（突发启发式降为兜底）；删除线真实渲染（CROSSED_OUT）；终端硬光标落在 `▋` 位（D20g 的意图，iocraft 无光标 API 的约束消失）；极矮终端从顶部丢行保住输入框 + footer（旧行为触发 Purge）；D25 遗留的 tick 饿死随渲染循环消亡（tokio interval 无唤醒竞态）。
- **验证**：468 测试全绿（chat.rs 115 个既有测试零改动通过；驱动 23 个 TestBackend 场景测试，含「50 行历史 + 缩窄 resize 后无任何重复行」回归；app/view 移植 25 个策略测试——tail window、chrome 完整性、flush 跨宽度不双打、ctrl+o 门控、建议行数同源）；`cargo clippy -- -D warnings` 干净。真机烟囱测试（Ghostty 长回复 + 拖拽 resize、tmux+Ghostty 图片、Terminal.app、BCE 终端的 EL 清行）待实测。
- **首轮真机回修**（2026-08-06 Ghostty 实测反馈四项）：
  1. 回合后大片空白——settle 顺序错了：先把定稿行滚进 scrollback（视口还高），再收缩视口，收缩腾出的行成了永久空白带（高度=刚才回复行数）。改为驱动 `gap_above` 银行 + `frame()` 单批次：先收缩（腾出的行记账入银行）、定稿行直接写进这些行、再 diff 视口——settle 零空白且常态零滚动（测试断言 `scrolled_up` 为空）；银行未耗尽的部分由下一次增高回收（grow 先收银行再滚动）。
  2. MCP stderr 写穿 TUI——scrollback 永不重绘后，stdio 子进程日志一旦落屏就固定（旧架构被每帧全量重画掩盖）。spawn 时 stderr 重定向 `~/.local/share/bingo/logs/mcp-<名>.log`（每次连接截断重写；开不了文件则丢弃，绝不继承终端）。
  3. tmux 图片从未显示过的真因——raw mode 下 `placeholder_rows` 每行以裸 `\n` 结尾：LF 只下移不回车，占位符第二行起从上一行末列开始，网格整体斜切，kitty 按占位单元格放像素自然全乱（iocraft 时代同病，故「一直没显示过」）。`kitty_image_bytes` 尾部推进同改 `\r\n`。
  4. 拖拽 resize 边框残影——chrome 的满宽行（边框/气泡）被终端 reflow 折行、逃出视口清理区。加 120ms 防抖：风暴中只记录最新尺寸、不作画（旧宽作画只会叠更多错宽行），静默后一次应用+重画——残留从每步一组降到每次拖拽至多一组（Ctrl+L 可清）。彻底解法见 D27。
  验证：473 测试全绿（驱动 +4：settle 零滚动、银行分块滚动、grow 回收、resize/clear 重置银行），clippy 干净。

### D27. 懒落盘 + resize 回灌：大活视口

- **需求**（用户明确提出）：视口范围内的内容不要冻结进终端——窗口内一律保持可重排；容量变大（拉高窗口、缩小字体）时，连**已经落盘**的内容也要取回重新渲染填满屏幕，「我可以接受这种情况上滑的时候有重复」。
- **策略**：
  1. **懒落盘**：定稿段只要仍完整落在可见窗口（终端高 − 1 − chrome）内就留在活文档里，每帧参与 diff 重画（宽度变化随 `build_rows` 重排、ctrl+o 折叠照常可用）。段的起始行越过窗口顶端才冻结，且**整段冻结**——半冻结会让隐藏部分既不在屏上也不在 scrollback，无处翻看；整段冻结时其可见尾部从视口绘制转为 scrollback，逐像素不动（gap 银行吸收，见 D26 回修 1）。
  2. **回灌（rehydrate）**：resize 防抖静默后，若窗口容量大于活文档行数，按检查点从最近落盘的段往回取（先问答块行、再消息段），`build_rows` 试渲染，超出预算即回退——保证回灌完不存在越过窗口顶的定稿段，与懒落盘互不打架。回灌是**纯记账**（flushed 游标回退，不写终端）；scrollback 里的旧拷贝物理上收不回，上滑会看到一份旧几何的重复——用户明确认可的取舍。
- **机制**：`Doc.settled_marks: Vec<SettledMark{row_end, segments, ask_rows}>`——build_rows 在每个定稿点（欢迎卡/每条定稿消息/问答块）记检查点（构建内累计值，`Chat::mark_base` 消化跨次推进的增量）；app 层 `pick_flush_mark` 选「所属段起始行 < 窗口顶」的最远检查点，`advance_flushed_upto` 部分推进。旧聚合字段 `settled_segments/settled_ask_rows` 删除（检查点的纯重复）。
- **副作用剖析**：resize 后首帧视口大幅增高会把屏上旧几何残留推进 scrollback（append_lines），新渲染的拷贝随后填满屏幕——正是「接受重复」的语义；图片块回灌后再冻结会重发 kitty 传输，U=1 同 id 更新既有放置、Direct 无 id 累积新实例但旧放置像素不动，均无害。
- **验证**：476 测试全绿（新增：pick_flush_mark 策略矩阵、装得下不冻结、回灌填满/超预算回退；9 处既有断言改读检查点）；clippy -D warnings 干净。
- **真机回修二（/resume 大列表触发逐行重复 + 拼接行）**：三个叠加成因，一并根治——
  1. **DECSTBM 单行区域非法**（元凶）：懒落盘后视口高达 H−1、`vp.top==1` 成常态，make_room 逐行发 `CSI 1;1r`+`CSI S`——DECSTBM 要求区域 ≥2 行，非法参数被终端忽略、区域退回**全屏**，每写一行全屏滚一次，视口内容与写入交错上移 → 逐行相邻重复 + 新行盖旧行的拼接尾巴（TestBackend 对单行区域语义正确，故 27 个驱动测试全绿而真机全错）。修复：视口上限统一改 **H−2**（driver clamp / Frame 预算 / tail_window 三处同源），`vp.top ≥ 2` 恒成立、区域恒合法。
  2. **`CSI S` 不进 scrollback**（kitty/Ghostty 明确语义：SU 滚出的行进 bit bucket）：落盘滚动换成 codex 同款原语——`RawWrite::scroll_into_scrollback`：DECSTBM 顶锚区域 + 光标停区域底行 + n 个 LF + 复位（LF 是唯一全终端进 scrollback 的滚动）；测试端 Recorder 映射回 `scroll_region_up` 保持 TestBackend 语义等价，27 个驱动测试零改动。
  3. **瞬态 slash 输出驱逐活内容**（策略错误）：/resume 无参列表是文档尾部的 TTL 瞬态行，把窗口挤小导致活对话被误冻结（且 2s 后窗口空半截）。`Doc.transient_rows` 标记瞬态行数，懒落盘的窗口计算剔除之——临时列表只是暂时盖住，不是驱逐。
  验证：477 测试全绿（新增瞬态不冻结回归测试；3 个 H−1 期望值随上限改 H−2），clippy 干净。
- **真机回修三（resize 后视口跳底、旧画面成多宽度残骸堆叠）**：`term.resize()` 原本无条件把视口重锚到屏幕底部——内容未占满屏时（如仅欢迎卡在屏幕上部），每次宽度拖拽都把视口甩到底部重画，旧位置的画面留在上方永不清除，一次拖拽堆出一摞不同宽度的卡片。改为**内容锚定**（CC 同款行为，用户点名）：resize 保持视口原行、仅在新屏装不下时上移到恰好容纳；清除从视口原位到屏末，把终端 reflow 从旧视口折出的碎片一并抹掉。视口从内容起点开始渲染，与刚启动的会话一致。477 测试全绿（`resize_taller_keeps_the_viewport_content_anchored` 取代旧的跳底断言）。
- **真机回修四（缩宽后视口原点之上仍残留一行折行碎片）**：宽度缩小时终端 reflow 先于 resize 事件发生，旧满宽行折行使内容整体下移、顶到屏底还会整屏上滚——我们记录的视口原点与物理现实的位移**协议上不可知**，「从原点向下清」必然漏掉被推到原点之上的碎片。终极策略=不猜几何：resize 静默后走 Ctrl+L 通道（`force_redraw` → `clear_visible`）清掉整个可见屏、按新宽从头重画窗口；回灌已把内容拉回填满屏幕，画面无损，旧几何拷贝只留在 scrollback（用户接受的取舍）。自此 resize 的屏上表现完全确定性，不依赖任何终端 reflow 行为。
- **真机回修五（Ctrl+C 后整屏错位重影 + MCP stderr 仍写穿）**：两个独立成因——
  1. **reclaim 增长清空 diff buffer**（重影元凶）：回合结束 status 行消失 → 视口收缩 1 行（底锚，origin 下移、顶行清空进银行）；Ctrl+C notice 行出现 → 增长走 reclaim 路径，该路径把两个 buffer 清空「强制全量重画」——但物理屏只有银行那几行是空白，其余仍是旧帧。prev buffer 谎报全空后，diff 认定屏幕已空：新帧的**空行**不清底下的旧字（`❯ Hi`/回复相邻重复、卡片底边框碎片存活）、**变短的行**不清行尾（notice 文本右侧残留输入框边框 `────`）。修复：reclaim 不清 buffer，`retarget_buffers` 加带符号 offset（收缩 +shift 底锚、下扩/滚动增长 0 顶锚、reclaim 增长 −reclaimed 底锚下移、顶部补空行——恰好镜像物理上真空白的银行行），**prev buffer 任何时刻如实镜像物理屏**成为驱动不变量（resize/clear_visible 清 buffer 是因为它们同时物理清屏，不破此律）。回归测试 `grow_after_shrink_repaints_over_every_stale_row` 在旧实现下精确复现截图 artifact（`["eeee","bbbb","ffff","ggdd"]`）。触发面不止 Ctrl+C：任何「收缩→增长」序列（每个回合结束后再发消息/开 help/出建议）都踩中。
  2. **rmcp builder 覆盖 stderr**（上次回修一的重定向从未生效）：rmcp 3.1.0 `TokioChildProcess::new` 内部走 builder，builder 默认 `stderr: Stdio::inherit()` 且 `spawn()` 无条件 `.stderr(self.stderr)` **覆盖** Command 上已设置的值——我们在 Command 上设的日志文件 sink 被静默丢弃。修复：显式走 `TokioChildProcess::builder(command).stderr(stderr_sink(name)).spawn()`。触发时机与「发消息后稳定出现」吻合：MCP 懒连接在首条消息的 assemble_tools 时 spawn，banner 恰好打在光标（输入框）处。
  验证：478 测试全绿，clippy 干净。
- **inline ctrl+o = 展开/闭合切换，展开即整卷 transcript 重放**（CC 非全屏 ctrl+o，用户点名）：scrollback write-once 决定了 inline 下不存在「原地展开」——旧方案只对未落盘的最后一条消息放行折叠切换（`last_message_dynamic` 门），已落盘内容按不动。改为 CC 语义，两个方向：
  - **展开**（存在折叠项或已落盘内容）：`Chat::expand_transcript` 把**全历史**所有可折叠项（活动 + 折叠组）展开、`reset_flushed` 回卷落盘游标、置 `dump_transcript` + `force_redraw`；app 层重放帧**先清可见屏**（与 resize 同款——不清屏的话旧画面与重放行的相对位置取决于视口历史，短内容会同屏重复），再从欢迎卡全量重建文档，取**最后一个**定稿检查点一次 `flush_items` + `advance_flushed_upto` 整卷冻结进 scrollback：重放内容从屏幕顶部铺起、chrome 紧随其下，超屏部分自然滚入 scrollback，用户上滑翻看全貌。动态尾部（流式消息/权限对话/瞬态 slash 行，均在检查点之外）照常留在视口。屏上已是全貌且无可展开项时 no-op。
  - **闭合**（`transcript_fully_expanded`：存在可折叠项且全部展开）：`Chat::collapse_transcript` 全历史折回聚合态，随后走 **resize 同款收拢**——撤销未渲染的重放、`force_redraw` 清可见屏、`rehydrate` 按折叠后的高度回灌填满窗口。屏上的展开重放行只留在 scrollback（write-once，不清屏就会与折叠窗口同屏并存）。无可折叠项时判定恒为假，ctrl+o 退化为纯重放。
  折叠旧拷贝留在 scrollback 上方，接受重复（与回灌同一取舍）。零新驱动原语：回卷（/clear、/resume 同款）+ 全量冻结（懒落盘同款）+ 清屏回灌（resize 同款）的组合。`last_message_dynamic` 门随之删除；fullscreen 的 ctrl+o 仍是就地折叠切换（那里可以重绘）。478 测试全绿（重写 2 个门测试为重放/闭合语义）。

### D28. 活动行图标词汇表：形状表类别，颜色表状态

- **问题**（用户点名「太丑」）：所有活动行统一 `⏺`，MCP 工具裸露全名 `mcp__server__tool(...)`，Skill 显示 k=v 兜底 `args="doc.md"`——类别不可辨、噪声重。
- **词汇表**：`⏺` 内建工具（CC 锚点不动，组行/回复点/Update 同族）· `◆` MCP（外接件，显示名 `server:tool`，权限规则仍用 `mcp__` 全名）· `✦` Skill（与 ✢✻✽ 星芒 spinner 同族，摘要改 `技能名 参数`）· `◉` 子代理 Watch 行（环中有核=会话套会话；Agent 是隐藏工具，唯一可见行是 watch）。颜色继续只表状态（dim 运行/绿成/红败），一职一色。
- **实现**：`activities.rs tool_glyph`/`display_tool_name` + `watch_header` label 前缀判定；`summarize_input` 加 Skill 臂。全部显示层，零行为变化。四个字形均为 unicode_width 单宽（◆/◉ EA=Ambiguous，与既有 ○/◇ 同类已被接受）。480 测试全绿。（label 前缀判定随 D29 换成 WatchKind 契约字段。）

### D29. 具名 agent + 实例续话（第一刀：hub-and-spoke）

三轮设计讨论（用户逐轮删概念）收敛出的多 agent 路线图：第一步 = 本条（非实验）；第二步 = 频道互发（实验开关，见下方设计冻结）。

- **具名定义**（`src/agents.rs`）：`~/.config/bingo/agents/*.md` + `.bingo/agents/*.md` 平铺文件（近 cwd 项目层同名覆盖用户层），frontmatter `name/description/model/provider/thinking`、正文 = 子代理 system prompt（替换父 system，空则继承）；优先级 显式参数 > 定义 > 继承（模型/provider/思考各自独立，thinking 继承父会话当前级别快照）。frontmatter 解析泛化为 `skills::parse_frontmatter_pairs`（任意键 + 折叠/字面标量），技能与 agent 定义共用。定义个位数，不做 mtime 缓存。
- **实例注册表**（`AgentRegistry`，Session 级共享）：每次 Agent 派生登记一个具名实例（`name` 参数缺省取定义名/agent，重名自动 -2/-3），状态机 Running/Idle/Stopped。回合完成把 **run_query 返回的完整消息历史**存进条目——续话 = 旧历史 + 新指令再进 run_query，上下文零丢失。
- **续话与生命周期**（仅 depth==0 装配，hub-and-spoke）：`SendMessage(agent, message)` 忙碌排队（回合结束由同一后台任务链自动续跑下一回合）、空闲带历史唤醒（新 spawn）；`AgentControl(list|stop|delete)` 列表/停止（abort 当前回合 + watch 行置 Cancelled，历史保留）/删除（移除条目，名字释放）。多条排队指令按序并成一个提示。子代理仍可继续派生（深度上限 3）但不管理兄弟。
- **展示**：`WatchKind` 贯穿 Watchable → WatchEvent → UiEvent → WatchCall（契约字段，替代 D28 的 label 前缀判定），子代理 watch 行 `◉ 名字 · 任务`，续跑回合 `◉ 名字 #N · 指令摘要`（每回合独立行，label 唯一避免 TUI 按 label 撞行）。
- **遗留**：同步（background:false）子代理若整回合被用户中断，条目可能停留 Running（无驱动方）——AgentControl stop/delete 可清理；与旧版 watch 行孤儿同类，未新增失败面。
- **第二步（频道互发，实验开关 `experimental.agentChannels`）已落地**：设计原则——能力普遍（人人可听可说）、选择自主（沉默是 agent 醒后的决定，不调用 Post 即沉默、零成本不传播唤醒）；引擎零游戏/场景知识，点名纪律等全在提示词。实现：
  1. **`src/channels.rs`（纯状态）**：频道 = 成员名单 + serial|free + 单调 seq + 全量 log + 每成员 seen 游标 + 发言计数；`post()` 只做三件事——盖戳（from 由调用方从 `Session.instance` 取，模型无法指定）、serial 陈旧校验（seen < seq → `Stale{missed}` 弹回并把增量计入已读）、预算闸（每 agent 每频道 50 / 每频道 500，超限冻结 + hub_mail 一次警示）。hub 成员名恒 `main`（claim_name 保留）。
  2. **投递唤醒（工具层编排）**：`AgentRegistry` 信箱泛化为 `InboxItem::Direct|Channel`，单锁原子"投递+认领"无丢失唤醒；Post 的 Sent 分支对每个成员 `deposit`——Idle 立即 `spawn_agent_loop`（`absorb_inbox` 格式化 `[#频道 第N条] 发件人: 内容` 并推进 seen 游标）、Running 信箱累积回合边界批量注入、Stopped 静默丢弃。hub 走 `hub_mail`：query loop 每轮推理前 `<channel-messages>` 注入；TUI 空闲时由 ◇ 行的 WatchEvent 触发 submit_auto（TurnEnd 检查兜底）。
  3. **serial 弹回是工具结果不是错误**：`Post` 返回"未送出+增量+请重新决定"，模型在同回合 tool loop 内阅读增量后自判照发/改发/放弃——报数式顺序从竞争+重试中涌现（每轮竞争必有一人落地，收敛有保证）。
  4. **工具面**：hub（depth 0 且开关开）得 `Channel`（create/invite/kick/list，成员限 depth==1 直接子代理，迟入无 backlog、seen 置当前头）+ `Post`；cohort 成员（depth 1 有实例名）只得 `Post`；更深层与关着开关时不装配。AgentControl delete 顺带清出全部频道。
  5. **观战**：每频道一条 `◇ #名字` watch 行（WatchKind::Channel），post 时更新 detail（N 条 · 最近发言）与 payload（日志尾 50 条，ctrl+o 展开看全群聊）；Running 态更新不产生通知垃圾。
  6. **v1 无引擎等待**：round_robin/gather 均已在讨论中删除（顺序=调度/协议，非传输），故无需超时机制；预算是唯一止损。settings 逐字段合并（开关任一层开启即开）。

### D30. 实体视图：底部选择器 + 交替屏模态（agent 对话 / 微信式频道房间）

- **需求**（用户点名，参照 CC 多 agent 展现）：agent 在底部展示，↑↓ 选择、回车进入该 agent 的对话；频道也在列表里，打开时强制全屏、微信式布局（他人靠左、我发的靠右）。
- **选择器**（`chat.rs`）：底部实体区（chrome，输入框上方）——收起态一行 dim 摘要（`◉ scout(running) · ◇ #table(3) — ctrl+g 查看`），ctrl+g 聚焦后逐行 `❯` 选择（窗口滑动、上限 6 行）、Enter 置 `open_entity`、Esc 收起；快照经 `refresh_entities` 在 tick（每 15 tick）与 WatchEvent 时刷新，内容变化才 dirty。选择器按键先于全局 Esc 语义。
- **模态**（`src/tui/entity.rs`）：write-once scrollback 决定 inline 没有"就地换内容"——两个视图都跑**交替屏**（fullscreen 宿主本就在交替屏则不嵌套进出）。自绘循环（ratatui Terminal 每帧全画）、贴底滚动 + ↑↓ 偏移、Esc/ctrl+c 返回；模态期间照常 `chat.tick + drain_all`（后台 agent 事件不丢，hub 的自动回合照常拉起）。**返回走确定性重画**：inline 置 pending_resize（清屏 + 回灌 + force_redraw 的既有 resize 管道，不赌交替屏恢复的保真度），fullscreen 置 force_redraw。
- **agent 对话视图**：注册表历史（❯ 指令 subtle / 正文纯文本折行 / `⏺ 工具(摘要)` dim；tool_result 与 thinking 不进视图）+ **流式活尾**——`AgentRegistry.live` 持每回合与 subagent_hooks 共享的输出 Arc（回合始挂终摘），运行中打开能看到正在生成的文本 + `✻ 生成中…`。
- **频道房间**：`user` 成为第三个保留成员（与 main 同样自动入席、不可移出、预算豁免、claim_name 保留）；气泡布局——他人靠左（名签 + code_block_bg，连续同发件人合并名签），user 靠右（user_message_bg，右对齐 pad；SegStyle 分段 bg，不用整行 Row.bg）；底部输入行 Enter 经 `deliver_post`（Post 工具同一投递/唤醒路径）以 user 身份发言；**渲染即已读**（每帧 mark_seen 到日志尾，serial 校验对着屏幕的人恒为最新，不弹回）。
- 506 测试全绿（气泡布局/agent 视图/选择器状态机纯函数测试；模态循环与宿主接线为薄覆盖，与 fullscreen 同层）。

### D31. agent team：项目级编队 + 跨会话记忆（键 = 项目路径 + 分支）

需求（用户点名）：① team 固定到项目（可入库），项目启动默认读取并拉起；② team 保留记忆，记忆与「当前项目路径 + 当前分支」绑定。`#dev-room` 全员两轮头脑风暴收敛（dev-ex/ui-ux/dev/qa，28 条）。

- **心智模型**：team 是图纸（持久定义），room 是工地（易失运行态）。team = 声明式编队层，复用一切既有原语，跑完回到 hub-and-spoke 控制面。
- **配置** `.bingo/team.json`（camelCase、进版本库、与 settings.json 同层）：`name` + `channel{ mode: serial|free, messageLimit: 正整数 }` + `members[{ name, agent }]`。成员引用 AgentDef 而非内联人格——人格单一事实来源仍在 `.bingo/agents/<名>.md`，一人格可入多 team。职责分离：settings 管「要不要拉起」（`team.autoStart`），team 文件管「拉起什么」。
- **启动默认加载**：`autoStart` 缺省 true（用户需求字面「默认读取」），双 opt-out：settings 关 + `--no-team` CLI。**拉起 ≠ 唤醒**：只派生成员 + 建房间，走现有 Idle 待命态（零 token、零回合），等 SendMessage/频道消息才开跑；验收断言「未收任务前 token=0、无回合日志」。拉起完成一行提示 `[team] dev-room 就绪 · 3/3 待命（/team status · /team stop）`，异常升级警示。
- **实现为三块薄层，不引入新运行时**：team.json 解析（校验函数 validate 与 start 同源：validate 能过 start 必成）→ `spawn_team` 编排（现有 Agent spawn + `ChannelRegistry` create-if-not-exists + 成员去重）→ `/team` 命令族。幂等键 = 实例名：重复 start 复用（事件措辞 `spawned ×3` vs `reused ×3`，同 `[team]` 前缀不同动词）。
- **命令一条线**：`/team list`（定义区/运行区两屏）→ `start` → `status`（待命/忙碌/异常/离线四态，字符+颜色双编码 ●绿 ◐黄 ✗红 ○灰）→ `assign`（= SendMessage）→ `stop`；`/team new` 交互脚手架（产物必过 validate，选成员时对引用不到的 AgentDef 即时拦截）→ `/team validate`。
- **AgentDef 加 `source: AgentDefSource`**（Project/User/Unknown）：`load_agent_defs` 加载时记第一出处（项目层先加载、first-wins 去重 → 跨层同名覆盖 source=项目层）；无 source 的旧数据缺省 `Unknown` 不报错，UI 静默省略徽标。AgentDef 当前无序列化路径，加字段零破坏。
- **边界（qa 定稿）**：缺失 AgentDef 报「缺失名 + 查找路径 + 字段路径」三段式；配置内成员重名=拒绝；空成员=配置错误、单成员合法；成员级失败隔离（继续拉起其余，失败者标异常可单独 re-spawn）；半启动态保留可辨认（`部分拉起 · 2/3`），不自动回滚；三层 settings 合并加 autoStart 的覆盖顺序测试；未知字段忽略（旧版本兼容）。
- **记忆持久化**（第二轮收敛）：
  - **键 = 项目路径（project_hash）+ 分支**：worktree 场景天然成立（主仓库 main 与 `.bingo/worktrees/agent-team` 路径+分支不同，记忆互不污染）；避免嵌入绝对路径，project_hash 校验断言防拷贝到异机误挂。
  - **内容分层**：完整历史落盘（恢复保真）+ 决策记录（append-only、零模型成本，`sources` 管道分隔字段复用 `parse_frontmatter_pairs`，`type` 下沉条目级）。
  - **存储**：user 层按项目+分支分目录（默认不进版本库）；协作导出 `export` 零转换（frontmatter 即 schema 字段）；`project_hash` 含于导出文件头。
  - **碎片清理**：`/team memory list|show|gc|merge|export` 命令族，gc 带 TTL；损坏/孤儿文件与配置错误同一视觉语言（不另造样式）。
  - **恢复时机**：启动拉起时自动恢复，一行摘要不打扰；恢复与拉起同走 spawn_team，缺文件静默回落空历史。
- **验收断言链**：脚手架产物 → validate 通过 → start 不因配置失败；记忆 roundtrip（存→重启→恢复等值）；source 跨层覆盖；无 team 段旧项目行为完全不变。

### D32. 多平台支持：shell / 进程树 / TTY 平台抽象（D25 前遗留 unix-only 消除）

需求（issue #1）：原生 Windows 支持 + GitHub Releases 官方预编译二进制；用户进一步明确为多平台（Windows / macOS / Linux）。

- **`src/platform.rs` 单一平台层**：`init_shell/shell`（进程级 OnceLock，main 启动时从 `settings.shell` 注入）、`shell_command`（Unix `-c` + `process_group(0)`；Windows PowerShell 系 `-NoProfile -NonInteractive -ExecutionPolicy Bypass -Command`，其他配置 shell 回退 `-c`）、`kill_process_tree`（Unix `/bin/sh kill -pgid`，沿用 AGENTS.md 禁 unsafe 约束；Windows `taskkill /PID /T /F`）、`open_tty`（Unix `/dev/tty` O_NONBLOCK；Windows 返回 None = 主题检测安全降级）。
- **默认 shell 按平台**：macOS `/bin/zsh`、其他 Unix `/bin/bash`（消除 Linux 无 zsh 的隐式依赖）、Windows `powershell.exe`；`settings.shell` 可覆盖（如 Git Bash）。Bash 工具与 hooks 共用。
- **进程树终止顺序修正**：超时路径先 `kill_process_tree` 再 `child.kill()`——Windows 的 `taskkill /T` 需要根进程存活才能遍历树，先杀根会导致孙进程遗留；Unix 两侧顺序无碍。
- **行为变化**：Linux 上 shell 从 zsh 变为 bash（确定性优先）；`interactive_command_reason` REPLS 名单加 `powershell/pwsh/cmd`。
- **CI/Release**：`.github/workflows/ci.yml` 三平台 matrix（ubuntu/macos/windows-latest，check+clippy+test，无需 API key）；`release.yml` 标签触发，四目标（linux x64 / win x64 / mac arm64 / mac x64 交叉编译），ZIP/tar.gz + `checksums.txt` SHA-256。
- **验证局限**：macOS 本机 628 测试全绿 + clippy 零告警；Windows 源码交叉检查被 `aws-lc-sys`（C 依赖需 windows.h）阻断，Windows 侧由 CI 原生 runner 验证。

### D33. Provider 协议层 + OAuth 接入（多 provider 协议抽象）

需求（用户点名）：bingo 支持多 AI provider OAuth 接入（Codex/ChatGPT 订阅、opencode go 订阅等）+ 协议抽象层（Anthropic 为一个实现，另支持 OpenAI Responses 协议）。`#provider-oauth` 全员对齐 + main 裁决（设计稿 notes/design/provider-oauth.md §10）。

- **契约先行三件套**（AGENTS.md 公共边界规则）：① settings v2——`ProviderConfig` 加可选 `protocol`（值域 anthropic|openai，缺省 anthropic，存量配置零迁移）、`apiBaseUrl` 可缺省（空 → 协议默认端点）、`oauth`/`capabilities` 留待 P2；② `api::contract` 中立类型（NeutralRequest/SystemBlock/StreamEvent/ThinkingLevel/Capabilities）+ `ProviderClient` trait（stream→BoxStream / complete_text / list_models / count_tokens / auth_status）——消费者永远不见 wire JSON；③ auth.json 格式（P2）。
- **Client 门面化**：provider 表改为 `Arc<dyn ProviderClient>` + 展示信息（key/url），`set_provider`/`with_provider`/`provider_endpoint`/`supports_images`（读当前 adapter capabilities）API 不变；错误类型 `ClientError` 移入 contract，新增 `Unsupported`（如 openai 无 count_tokens 端点 → 本地估算降级，D6 精神）与 `Config`（未知 protocol → 启动即 CONFIG_INVALID）。
- **Anthropic 收编 = 吸收不改写**：client.rs 内部平移为 `api::providers::anthropic`，重试/退避/超时/400 溢出重算/SSE/错误映射 byte-identical（基线 636 全绿 → 平移后 639 全绿才动新代码）。
- **OpenAI Responses adapter**（`api::providers::openai`，POST `{base}/v1/responses`，默认 base api.openai.com，`Authorization: Bearer`）：system→instructions（join）、messages→input items（text/image/function_call/function_call_output；thinking 不回放、tool_result 错误标志编码进 output 字符串）、tools→function tools（input_schema→parameters）、thinking→`reasoning.effort`（xhigh/max 收敛 high）+ `include:["reasoning.summary_text"]`、max_tokens→max_output_tokens。SSE 映射：output_item.added（message/reasoning/function_call）→ Text/Thinking/ToolUseStart、output_text.delta→TextDelta、reasoning_summary_text.delta→ThinkingDelta、function_call_arguments.delta→InputJsonDelta（output_item.done 权威 arguments 兜底空参数）、completed/incomplete（max_output_tokens→max_tokens，queryLoop 延续语义）→StopReason、failed/error→ApiError；**双层 index（output_item+content part）压平成单 block index**（忽略型 item 不占槽）。
- **注册表 `build_provider`** 按 `protocol` 分发，唯一知道「配置 → adapter」的地方；默认 provider 仍走顶层 apiKey/apiBaseUrl/env（anthropic）。
- **OAuth（P2，main 裁决强制项）**：唯一硬需求 = Codex/ChatGPT（device flow + loopback PKCE 双实现，client_id `app_EMoamEEZ73f0CkXaXp7hrann`、issuer auth.openai.com，端点/refresh/revoke 已源码核实，见 notes/research-oauth-cli.md）；token 存 `~/.local/share/bingo/auth.json`（0600、opencode 兼容 shape）绝不进项目 settings（根治 apiKey 进被提交配置）；懒刷新+401 触发+单飞锁，永久失效清登录提示重登；**P2 起手 0.5 天 spike**：订阅 bearer 打公开 /v1/responses（Path 1，复用 P1 adapter）还是私有 chatgpt.com/backend-api codex 协议（Path 2，第三 adapter）。
- **opencode-go 订阅修正确认**（调研修正）：实为 API-key 订阅非 OAuth → 落地 = 命名 provider + protocol openai + apiKey，零 OAuth 代码；端点 P3 时核实。
- **能力协商 v1 静态声明**（协议默认 + config 覆盖，无运行时协商）；`cacheControl` 为 anthropic-only（openai 侧 v1 不接缓存）；reasoning 摘要映射 thinking UI、不回放 verbatim。
- **验证**：cargo build + clippy -D warnings + test --bin bingo 全绿（P0 639 / P1 650）；mock server 双协议同回合 fixture 断言同一 StreamEvent 序列（§9 契约测试）；提交两枚只进 feat/provider-oauth（♻️ refactor + ✨ feat），不碰 dev/main。

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
  - **Rail** (5 cols): workspace chip + 主页 / 私信 / 动态 tabs. Activity lists what is unread *plus* whatever is open, so reading a conversation cannot yank it out from under the cursor.
  - **Sidebar**: workspace name, quick-switcher hint, collapsible 频道 / 私信 sections, `#` prefixes and presence dots keyed to `AgentState`, unread rows bold-white with a red badge, the open row on Slack's blue bar. A frozen channel is struck through rather than given a glyph — strikethrough costs no columns and cannot misalign on a terminal that renders ambiguous-width glyphs at two cells.
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
- **Decision — the sender names the wait, the harness keeps the clock**: `SendMessage(ack_timeout: <seconds>)`, clamped to 5–3600, is the whole opt-in surface. A per-message tokio task (`spawn_ack_watchdog`) sleeps that long and re-reads the same record the tool reports, so the automatic check and the manual one can never disagree. Omitting the parameter leaves the previous path untouched, watchdog and all.
- **Decision — chase, then stop**: while the sender is still owed an answer, each round appends `InboxItem::FollowUp` to the receiver's inbox and re-runs `flush_agent_inbox`. The flush repairs the stranded-at-a-boundary case (an idle instance nobody claimed); the follow-up reaches a receiver that is running or was silent, riding along with the message it names, since queueing is the only channel to a busy instance. It carries `delivered` so the prompt can name which silence it is — nobody picked it up, versus you read it and ended the turn saying nothing — and asks for a reply rather than repeating the instruction. `MAX_FOLLOW_UPS = 3` bounds it: a retry budget that never runs out is not a mechanism, it is a loop. `AgentRegistry::follow_up` reads and enqueues under the single registry lock, so a turn ending mid-check cannot produce a nudge for a message that was just answered.
- **Decision — silence only when nothing went wrong**: the watchdog registers no watch line up front. On the first missed deadline it registers one, and every non-clean outcome ends terminally — `Done` when the follow-ups drew a reply out, `Failed` when the message was dropped, the instance was removed, or the budget ran out. Terminal watch states are already the path into the hub's next turn (D41 addressed notifications), so the report costs no new plumbing. An answer inside the wait leaves no line, no notification, and no trace beyond the ack record.
- **Boundary**: an answer means the turn produced text for the hub, not that the work is done or correct — the mechanism detects silence, not bad work. Follow-ups take no `MsgId` of their own (they are not the sender's messages), never re-send the original text, and are excluded from `pending_of` so the D43 DM view does not render the harness's nudge as something the user typed. Hub-and-spoke gating is unchanged: `SendMessage` stays depth-0, so only the hub can arm a watchdog, and channel deposits and `/team assign` deliver with no wait to name.
- **Acceptance**: 824 tests pass with fmt/check/clippy `-D warnings` clean. Focused regressions cover the bounded chase (three follow-ups then `Exhausted`, follow-ups batched with the original, the ack recording its wait and round count), a silent turn leaving what it read unanswered and chaseable with `delivered: true` and then being answered by a later run, settling on a dropped message and a removed instance, and both driver halves under a paused clock — a receiver that reads and says nothing chased three times and reported to the hub, and an answered message leaving no notification and no watch line.

### D45. Channel silence is a prompt, in the system block: stopping the acknowledgement storm

- **Trigger** (reported by the user with a screenshot): one `你好` posted into a team room produced thirteen messages and no work. Every member answered the greeting; every answer woke every other member; each of those then answered the answers — "早上好，X 已就位" → "收到，角色已齐" → "收到，届时 QA 会…" — a room of agents being polite at each other while the task sat untouched.
- **Mechanism**: D29's primitive 3 (wake-up follows delivery) is doing exactly what it promises. A post reaches every member's inbox and `flush_agent_inbox` starts a turn for each. The engine is not at fault; what was missing is that the model's default social reflex is to answer whatever just arrived, and nothing told it that here, answering is the expensive act.
- **Decision — fix it in the prompt, not the engine**: a `CHANNEL_NOTE` system block appended by `build_sub_session`, gated on `experimental.agentChannels` so a solo subagent that will never see a room doesn't carry room etiquette. It names the failure mode instead of asking for brevity: do not post to greet, introduce yourself, acknowledge, agree, say you understood, or restate the plan; if your draft would be just as true before you read the message you are answering, don't send it; post when you have something only you can supply. The `Post` tool description carries the same rule in one line — a tool description is re-sent every turn and is the last thing read before the call.
- **Why a system block and not the wake-up payload**: compaction rewrites the message history, so anything delivered as a message eventually summarises away — precisely on the long-running members most at risk of drifting back into chatter. `compact::maybe_compact` takes `&Session`, so it cannot touch `Session::system` at all, and builds its summary request with `system: Vec::new()`; the rule is guaranteed present on turn fifty by the borrow checker, not by convention. (This was the user's question, and it is why the note lives where it does.)
- **Boundary against D44**: the two rules point opposite ways on purpose and must not be collapsed. A *direct* message from the hub is owed an answer — that is what D44's `Answered` ack and its follow-up chase enforce. A *channel broadcast* is owed nothing; answering it is the failure. `CHANNEL_NOTE` says so in as many words, because "an ack is an answer" read without that boundary becomes pressure to reply to everything, which is the storm this record exists to stop. D44 already keeps its half of the line: channel deposits arm no watchdog.
- **Not done, deliberately**: making delivery selective — waking only `@`-mentioned members, the way a real Slack channel pings. That would damp storms structurally rather than by persuasion, but it revises D29's third primitive and changes what "a channel" means for every existing team. Left as a lever to pull if the prompt proves insufficient in practice.
- **Acceptance**: focused test asserts the note is absent when channels are off, present when on, and that its text names the acknowledgement failure mode rather than merely asking for concision. The compaction property is asserted by the type system and documented rather than restated as a tautological test.
