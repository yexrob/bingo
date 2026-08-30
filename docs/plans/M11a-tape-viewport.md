# M11a — The tape: settled rows in scrollback, a live region below

## Goal

`bingo` at a terminal writes each transcript item into the terminal's own scrollback exactly once, when it settles, and draws only what is not final — streaming text, running tools, live signals, a dialog, the composer, the footer — in an inline viewport at the bottom. Quitting leaves the conversation on screen; the terminal's selection and search work on it; `--continue` replays the last hundred settled items above. An overlay that needs the whole screen (help, picker, pager, a big diff, an image, the full panel) takes the alternate screen and gives the tape back untouched.

## Bricks, in build order (owner)

1. **Spike** (kernel, one week, binding) — `Viewport::Inline(h)` + `insert_before` with ratatui's `scrolling-regions`; grow and shrink the viewport between frames (recreate the terminal at the new height, or `clear` + redraw — measure flicker); resize while streaming; tmux and screen (DECSTBM inside a pane); Terminal.app, iTerm2, kitty, ghostty, WezTerm; a 5 000-row insert timed. Verdict written into this plan's Verified section with the fallback spelled out: alternate screen with a settled-row cache that emulates write-once (the M6 model plus a cache), if inline fails on any of the five terminals.
2. **`settle`** (worker) — the pure brick: `settle(state, ui, width) -> (Vec<Line>, Cursor)` returns the rows of every item that completed since the last settle point and advances the point; the live region is `transcript::item_lines` over the unsettled items only. Settled rows are rendered at the width of that moment and never again.
3. **The live region** (worker) — `view::live()` composes band · unsettled items · signals · dialog · composer · footer; its height is its content, capped at 60 % of the screen; when a dialog or preview exceeds the cap the dialog opens as an overlay instead.
4. **Overlays** (worker) — one `Overlay` enum (`Help`, `Picker`, `Pager{item}`, `Preview{interaction}`, `Image{asset}`, `Panel`) drawn on the alternate screen; enter and leave are the terminal module's; `esc` returns; the tape is not touched while one is open; frames keep folding underneath and settle when the overlay closes.
5. **One tape for the tree** (worker) — a child's or a room's item settles with its prefix (`↳ reviewer`, `#design ❯ reviewer:`) in seq order across sessions; the switcher (`ctrl+g`) changes the composer's target and which live region is drawn; the band names every live session.
6. **Replay on open** (worker) — `--continue` and `/resume` write the last 100 settled items above the live region in one `insert_before`, preceded by a dim rule `─── earlier: N items ───` when more exist; a fresh session writes nothing.
7. **PTY harness** (worker) — `portable-pty` + `vt100` dev-dependencies: a test drives the real binary in a pty, asserts scrollback rows after a reply, after an overlay, after a resize; `scripts/tui-smoke.sh` gains the settle scene.

## Files

`crates/bingo-surface-tui/src/{terminal,run,view,ui,tree}.rs`, new `settle.rs`, `live.rs`, `overlay.rs`, `overlay/{help,picker,pager,preview,image,panel}.rs`, `tests/pty.rs`, `scripts/tui-smoke.sh`, `Cargo.toml` (dev-deps), `scripts/budget.toml`.

## Dependencies

`portable-pty`, `vt100` (dev). Nothing at runtime beyond ratatui's `scrolling-regions` feature.

## Exit criteria

- [ ] spike verdict recorded with timings on five terminals and tmux; the fallback written even if unused
- [ ] a settled row is written once: the pty test counts bytes for a row across ten later frames and finds it written zero more times
- [ ] the live region never exceeds 60 % of the screen; a taller dialog is an overlay
- [ ] every overlay opens and closes with the tape byte-identical before and after (pty assertion)
- [ ] a child's reply and a room post settle into the tape in seq order with their prefixes; `ctrl+g` changes only the live region
- [ ] `--continue` replays 100 items and the rule; scrolling the terminal shows them
- [ ] resize while streaming: the live region redraws at the new width, the tape is untouched, no key is lost
- [ ] 5 000 rows insert in under 200 ms; idle draws nothing (frame counter over 2 s = 0)
- [ ] `scripts/tui-smoke.sh` and the pty test green on macOS and Linux

## Non-goals

Reflowing settled rows on resize. A per-session tape. Mouse scrolling of the tape (the terminal's). Virtualisation. Themes.

## Risks

Inline viewports on tmux — the spike's first day. `insert_before` with a growing viewport — flicker, measured. A 100-item replay on a slow terminal — batched into one write.
