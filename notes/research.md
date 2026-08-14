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
