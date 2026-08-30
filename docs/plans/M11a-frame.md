# M11a — The frame: a full-screen surface that owns its scrolling

## Goal

`bingo` at a terminal is a full-screen application laid out as `docs/design/tui.md` §3: a header of presence, session tabs, model and context meter; a virtualised transcript we scroll ourselves — smoothly, by line, with the wheel, `pgup/pgdn` and `ctrl+f` search — with selection and copy through OSC 52; a rail past 120 columns; cards and sheets as layers over it; the composer on a fixed baseline. Quitting prints the last screenful of the transcript into the shell so the conversation is still there after `exit`. A 5 000-block transcript scrolls at 30 fps with a draw under 4 ms.

## Bricks, in build order (owner)

1. **Regions** (worker) — `frame.rs`: the pure layout `regions(size, ui) -> Regions{header, transcript, rail, activity, composer, footer}`; the rail exists at ≥ 120 columns or when toggled; every region has a minimum and the composer and footer are never dropped; a table test at 80×24, 100×30, 120×40, 200×60.
2. **Block cache** (worker) — `blocks.rs`: one rendered block per item, keyed by `(item id, width, revision)`; a completed item renders once; a streaming item re-renders its last block only; the cache is dropped on width change. Measured: 5 000 items, 200 ms to warm, then zero re-renders on scroll.
3. **Scrolling** (worker) — `scroll.rs`: the pure state `Scroll{offset, target, since}`; `pgup/pgdn`, wheel, `home/end`, follow-the-tail while at the bottom; ease-out over 100 ms on an injected clock; a viewport that never shows a torn block boundary.
4. **Search** (worker) — `ctrl+f` opens a one-row search in the footer's place; matches highlighted with `structure`, `n/N` steps, the transcript scrolls to the match; `esc` closes.
5. **Selection and copy** (worker) — `v` starts a keyboard selection from the focused block, arrows extend, `y` copies; a mouse drag selects cells; copy writes OSC 52 (base64, chunked under 100 KiB) and falls back to a toast naming the size when the terminal refused; the selection is drawn with `raised`.
6. **Mouse** (worker) — crossterm mouse capture on; wheel scrolls; a click on a header tab switches; a click on a card row answers; a click in the transcript focuses a block; nothing needs the mouse.
7. **Layers** (worker) — `layers.rs`: `Card` and `Sheet` as the two layer kinds with their reveal state (frame 0..3) on the clock; the dim backdrop is a style pass over the regions beneath; the reveal frames are pure functions of `now`; the M6 dialog and the switcher become cards, help and the picker become sheets.
8. **Leaving** (worker) — on exit, after leaving the alternate screen, print the last `rows - 2` lines of the transcript as plain text through the block cache's degrade; `--no-print-on-exit` skips it.
9. **Header and footer** (worker) — `✻ bingo` presence mark, session tabs from the tree with their glyphs, model, the context meter as a sparkline over the last eight turns' `ContextUsage`; the footer per §4.

## Files

`crates/bingo-surface-tui/src/{run,view,ui,tree,terminal,input,keys}.rs`, new `frame.rs`, `blocks.rs`, `scroll.rs`, `search.rs`, `select.rs`, `layers.rs`, `header.rs`, `footer.rs`, `tests/pty.rs`, `scripts/tui-smoke.sh`, `Cargo.toml` (dev-deps), `scripts/budget.toml`.

## Dependencies

`portable-pty`, `vt100` (dev) for the pty harness. Nothing at runtime.

## Exit criteria

- [ ] regions table test at four sizes; the composer and footer survive 20×5
- [ ] block cache: 5 000 items warm in under 200 ms; scrolling re-renders nothing (counter); a width change drops everything once
- [ ] scroll eases over 100 ms (three sampled offsets on an injected clock); follow-the-tail holds while streaming and releases on `pgup`
- [ ] search finds across blocks, highlights, steps, scrolls; `esc` restores the footer
- [ ] selection: keyboard and mouse; OSC 52 bytes asserted by the `Recorder`; the refusal toast
- [ ] mouse: wheel, tab click, card click, block focus — each a test with a synthetic event
- [ ] cards and sheets: reveal frames 0-3 snapshotted; the backdrop is all dim; `esc` reverses
- [ ] leaving: the pty test sees the last screenful in the normal screen after exit, and nothing with `--no-print-on-exit`
- [ ] header and footer snapshots at 80 and 120; the meter's colour at 41 % and at the trigger
- [ ] a full draw at 120×40 under 4 ms (timed test, release profile noted); idle draws 0 in 2 s
- [ ] `scripts/tui-smoke.sh` and the pty test green on macOS and Linux

## Non-goals

Inline mode or the terminal's scrollback (set aside 2026-08-30). Reflow of old blocks on resize beyond the cache drop. Panes the user resizes. Tabs the user reorders.

## Risks

OSC 52 is refused by some terminals and by tmux without `set-clipboard on` — the toast says so and names the size. Mouse capture steals the terminal's own selection: `BINGO_MOUSE=off` returns it. The block cache and streaming: the last block is the only mutable one, by construction. A 30 fps tick and battery — the tick runs only while something animates (M11c).
