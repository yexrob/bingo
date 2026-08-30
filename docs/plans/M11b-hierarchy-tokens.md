# M11b — Hierarchy and tokens: the eye lands where it should

## Goal

Every screen reads by `docs/design/tui.md` §2 and §4: the answer is the brightest block flush left; work rows are indented two and dim with one gutter glyph; the one warm colour is reserved for wanting; the composer is a bare `❯`; the footer is the only line of hints; paths are short; thinking decays; receipts join their rows; a band appears only with a second live session. Every `TestBackend` snapshot is redone, and `assert_row_styled` proves colour lands only where the token table allows. This plan holds on today's alternate screen as much as on the tape, so it starts before M11a's spike ends.

## Bricks, in build order (owner)

1. **`theme.rs`** (worker) — the token functions become the table in §4: `text`, `dim`, `structure`, `attention`, `good`, `bad`, `bold`; `selected` and `caution` retire (REVERSED is gone; `caution` was `attention`); glyph constants per the glyph table (`○` pending, `●` retired, `▌` caret); an `Ascii` fallback table selected by `BINGO_ASCII=1`; `NO_COLOR` maps every colour token to `plain`. A test enumerates the tokens and their sole call sites.
2. **`transcript.rs`** (worker) — work rows indented two; output under `⎿  ` at indent 4 with three tail rows; short results joined on the row with ` · `; receipts folded into the tool row (`· allowed`, `· denied — feedback`); the answer flush left with a blank line above and below; `✻ thinking · Ns` / `✻ thought for Ns` from the item's timestamps; `[Request interrupted…]` stays dim; a failed turn's `✗` line stays the one red line.
3. **`paths.rs`** (worker) — pure: relative inside the cwd, `~` for home, middle-elided beyond 48 cells; used by tool summaries, previews and receipts; a table test.
4. **Composer and footer** (worker) — the box goes; `❯ ` + text + dim `▌`; continuation lines indent 2; the placeholder is empty; the room prompt is `#design ❯`; the footer is `mode · ? help` left and `model · ctx N% · N agents · N needs you` right; `1 agent` leaves the transcript; the context badge turns `attention` at the trigger, not red.
5. **Dialog** (worker) — a dim rule above and below; title `attention` + `bold`; options `❯ 1  Yes` with `structure` on the selected number only; the hint line stays; the permission summary uses `paths`.
6. **Band and switcher** (worker) — the band `project ● · reviewer ⠹ · #design` only when the tree has two live sessions, current bold; the switcher rows share the glyph table.
7. **Measure and help** (worker) — prose wraps at `min(width, 100)`; the help table goes two-column at 100 columns (the cell width shrinks by abbreviating the four longest lines).
8. **Snapshots and styles** (worker) — every snapshot regenerated and read as a screen, not a diff; `assert_row_styled` cases: an answer row has no colour; a tool row's glyph is the only coloured cell; a dialog title is `attention`; the footer is all dim but the badge.

## Files

`crates/bingo-surface-tui/src/{theme,transcript,view,dialog,keys,tree,composer,ui}.rs`, new `paths.rs`, `src/snapshots/*`, `test_support.rs`.

## Dependencies

None.

## Exit criteria

- [ ] the token table in the design doc and `theme.rs` list the same names; a test asserts no `Color::`/`Modifier::` literal outside `theme.rs`
- [ ] snapshots: idle, streaming, tool running, tool done with output, permission (collapsed, expanded, feedback row), question (single, multi), confirm, login (three flows), error turn, interrupted, room transcript, child transcript, switcher, band, help at 80 and 100, dropdown, `ctrl+t` panel — each read and accepted, not just regenerated
- [ ] `assert_row_styled` cases from brick 8 pass; `NO_COLOR` and `BINGO_ASCII=1` snapshots exist for idle and permission
- [ ] no hint text appears twice on a screen; `1 agent` is in the footer, not the transcript
- [ ] paths in dialogs and summaries fit one row at 80 columns for a path 120 cells long
- [ ] `scripts/tui-smoke.sh` green; the M9 and M10 tmux drives green with their needles updated

## Non-goals

The inline viewport (M11a). Motion (M11c). New content kinds (M11e). Theme detection.

## Risks

Taste drift between a worker's eye and the design doc — the review of this plan is done on rendered screens at 80 and 100 columns, and §10 of the design doc gets a line for every choice the doc did not already make.
