# M11b — Hierarchy and tokens: the eye lands where it should

## Goal

Every screen reads by `docs/design/tui.md` §2 and §4: the answer is the brightest block flush left; work rows are indented two and dim with one gutter glyph; the one warm colour is reserved for wanting; the composer is a bare `❯`; the footer is the only line of hints; paths are short; thinking decays; receipts join their rows; the header's tabs name the sessions. Every `TestBackend` snapshot is redone, and `assert_row_styled` proves colour lands only where the token table allows. It runs beside M11a on different files; where the two meet (the header, the composer's baseline) M11a's regions are the frame and this plan fills them.

## Bricks, in build order (owner)

1. **`theme.rs`** (worker) — the token functions become the table in §4: `text`, `dim`, `raised`, `structure`, `attention`, `good`, `bad`, `bold`; `selected` and `caution` retire (REVERSED is gone; `caution` was `attention`); a `Palette` with the ANSI set and the two truecolor sets (dark now, light when M11e reads the background), every token a function of the palette; glyph constants per the glyph table (`○` pending, `●` retired, `▌` caret); an `Ascii` fallback table selected by `BINGO_ASCII=1`; `NO_COLOR` maps every colour token to `plain`. A test enumerates the tokens and their sole call sites.
2. **`transcript.rs`** (worker) — work rows indented two; output under `⎿  ` at indent 4 with three tail rows; short results joined on the row with ` · `; receipts folded into the tool row (`· allowed`, `· denied — feedback`); the answer flush left with a blank line above and below; `✻ thinking · Ns` / `✻ thought for Ns` from the item's timestamps; `[Request interrupted…]` stays dim; a failed turn's `✗` line stays the one red line.
3. **`paths.rs`** (worker) — pure: relative inside the cwd, `~` for home, middle-elided beyond 48 cells; used by tool summaries, previews and receipts; a table test.
4. **Composer and footer** (worker) — the box goes; `❯ ` + text + dim `▌`; continuation lines indent 2; the placeholder is empty; the room prompt is `#design ❯`; the footer is `mode · ? help` left and `model · ctx N% · N agents · N needs you` right; `1 agent` leaves the transcript; the context badge turns `attention` at the trigger, not red.
5. **Cards** (worker) — the dialog on the `raised` tint with one cell of padding; title `attention` + `bold`; options `❯ 1  Yes` with `structure` on the selected number only; the hint line stays; the permission summary uses `paths`; everything behind a card is drawn `dim`.
6. **Tabs and switcher** (worker) — the header's tabs `project ● · reviewer ⠹ · #design`, current bold, a waiting one in `attention`; the switcher card's rows share the glyph table.
7. **Measure and help** (worker) — prose wraps at `min(width, 100)`; the help table goes two-column at 100 columns (the cell width shrinks by abbreviating the four longest lines).
8. **Snapshots and styles** (worker) — every snapshot regenerated and read as a screen, not a diff; `assert_row_styled` cases: an answer row has no colour; a tool row's glyph is the only coloured cell; a dialog title is `attention`; the footer is all dim but the badge.

## Files

`crates/bingo-surface-tui/src/{theme,transcript,view,dialog,keys,tree,composer,ui}.rs`, new `paths.rs`, `src/snapshots/*`, `test_support.rs`.

## Dependencies

None.

## Exit criteria

- [ ] the token table in the design doc and `theme.rs` list the same names; a test asserts no `Color::`/`Modifier::` literal outside `theme.rs`
- [ ] snapshots: idle, streaming, tool running, tool done with output, permission (collapsed, expanded, feedback row), question (single, multi), confirm, login (three flows), error turn, interrupted, room transcript, child transcript, switcher, tabs, help at 80 and 100, dropdown, the panel sheet — at 80×24 and 120×40, each read and accepted, not just regenerated
- [ ] `assert_row_styled` cases from brick 8 pass; `NO_COLOR` and `BINGO_ASCII=1` snapshots exist for idle and permission
- [ ] no hint text appears twice on a screen; `1 agent` is in the footer, not the transcript
- [ ] paths in dialogs and summaries fit one row at 80 columns for a path 120 cells long
- [ ] `scripts/tui-smoke.sh` green; the M9 and M10 tmux drives green with their needles updated

## Non-goals

The frame's scrolling, layers and mouse (M11a). Motion (M11c). New content kinds (M11e). Theme detection.

## Risks

Taste drift between a worker's eye and the design doc — the review of this plan is done on rendered screens at 80 and 100 columns, and §10 of the design doc gets a line for every choice the doc did not already make.
