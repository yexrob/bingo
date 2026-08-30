# M11b — Hierarchy and tokens: the eye lands where it should

## Goal

Every screen reads by `docs/design/tui.md` §2 and §4 in Claude Code's grammar: `⏺` for what the model says and does, `⎿` for what came back, `>` on a raised bar for what you said, the rounded input box, `✻` and a verb on the activity row, the bordered `Do you want to…?` card, `… +N lines (ctrl+o to expand)`, the `⏵⏵` mode line, a welcome box; the warm colour is bingo's presence and the only one that moves; paths are short; thinking decays; receipts join their `⎿`; a child session is a row you can step into; the status line says only what is true now. Every `TestBackend` snapshot is redone, and `assert_row_styled` proves colour lands only where the token table allows. It runs beside M11a on different files; where the two meet (the status line, the composer's baseline) M11a's regions are the frame and this plan fills them.

## Bricks, in build order (owner)

1. **`theme.rs`** (worker) — the token functions become the table in §4: `text`, `dim`, `raised`, `presence` (with its glow step), `good`, `bad`, `mode`, `bold`; `accent`, `caution`, `selected` retire (REVERSED is gone); a `Palette` with the ANSI set and the two truecolor sets (dark now, light when M11e reads the background), every token a function of the palette; glyph constants per the glyph table (`⏺`, `⎿`, `>`, `✻ ✢ ✶ ✽`, `☐ ☒`, `⏵⏵`; `✓ ✗ ⊘ ❯` retire from the transcript, `❯` stays as the card's cursor); an `Ascii` fallback table selected by `BINGO_ASCII=1`; `NO_COLOR` maps every colour token to `plain`. A test enumerates the tokens and their sole call sites.
2. **`transcript.rs`** (worker) — the model's text as `⏺ ` bold white then text at indent 2; a tool row `⏺ Name(args)` with the bullet coloured by state; results `  ⎿  ` at indent 5, three tail rows while running, then `… +N lines (ctrl+o to expand)`; receipts join the result; `✻ Thinking…` / `✻ Thought for Ns`; `[Request interrupted…]` stays dim; a failed turn is a `bad` `⏺` line.
3. **`paths.rs`** (worker) — pure: relative inside the cwd, `~` for home, middle-elided beyond 48 cells; used by tool summaries, previews and receipts; a table test.
4. **Input box and status line** (worker) — the rounded box the width of the transcript, one to ten rows, `> ` inside, a dim placeholder until the first keystroke, border `dim` (its glow is M11c's); the room prompt is `#design >`; the status line is `⏵⏵ <mode> (shift+tab to cycle)` left, notices middle (`N needs you (ctrl+t)`, `N running`, `context N%`, else `? for shortcuts` while the box is empty), place right (`in reviewer · gpt-5.4`); `1 agent` leaves the transcript for the `1 running` notice; the model leaves the transcript for the right slot.
5. **Cards** (worker) — the dialog as a bordered box under the asking row's `⎿`, border `presence`; title bold, the preview on its tints, the question line, `❯ 1. Yes` with `presence` on the selected row; the hints in the options themselves (`(shift+tab)`, `(esc)`); the permission summary uses `paths`; everything behind a card is drawn `dim`.
6. **Child rows and the switcher** (worker) — a child or peer is a row where it began, `⏺ reviewer(brief)` with the bullet by state and `⎿  Running… 3 tools · 1.2k tokens` / `⎿  Done (4 tools · 8.1k tokens · 40s)` / `⎿  Needs you`; `⏎` on it steps in; the switcher is a dropdown above the input box on `ctrl+t` (`❯ project ● · reviewer ⠹ needs you · #design`) sharing the glyph table; the window title keeps naming the session.
7. **Measure and help** (worker) — prose wraps at `min(width, 100)`; the help table goes two-column at 100 columns (the cell width shrinks by abbreviating the four longest lines).
8. **Welcome, snapshots and styles** (worker) — the welcome box on a fresh session; every snapshot regenerated and read as a screen, not a diff; `assert_row_styled` cases: an answer row is white after its `⏺`; a tool row's bullet is the only coloured cell; a card's border is `presence`; the status line is `mode` and dim only.

## Files

`crates/bingo-surface-tui/src/{theme,transcript,view,dialog,keys,tree,composer,ui}.rs`, new `paths.rs`, `src/snapshots/*`, `test_support.rs`.

## Dependencies

None.

## Exit criteria

- [ ] the token table in the design doc and `theme.rs` list the same names; a test asserts no `Color::`/`Modifier::` literal outside `theme.rs`
- [ ] snapshots: idle, streaming, tool running, tool done with output, permission (collapsed, expanded, feedback row), question (single, multi), confirm, login (three flows), error turn, interrupted, room transcript, child row (running, done, needs you), child transcript, switcher dropdown, help at 80 and 100, dropdown, the panel sheet — at 80×24 and 120×40, each read and accepted, not just regenerated
- [ ] `assert_row_styled` cases from brick 8 pass; `NO_COLOR` and `BINGO_ASCII=1` snapshots exist for idle and permission
- [ ] no hint text appears twice on a screen; `1 running` is on the status line, not in the transcript
- [ ] paths in dialogs and summaries fit one row at 80 columns for a path 120 cells long
- [ ] `scripts/tui-smoke.sh` green; the M9 and M10 tmux drives green with their needles updated

## Non-goals

The frame's scrolling, layers and mouse (M11a). Motion (M11c). New content kinds (M11e). Theme detection.

## Risks

Taste drift between a worker's eye and the design doc — the review of this plan is done on rendered screens at 80 and 100 columns, and §10 of the design doc gets a line for every choice the doc did not already make.
