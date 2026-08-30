# M11 — TUI depth: a full-screen surface that is alive

## Goal

The terminal surface described in `docs/design/tui.md`: a full-screen application that owns every cell — header, virtualised transcript, rail, cards and sheets as layers, a fixed composer — and feels alive: every motion reports a state; the eye lands on the answer or on what wants you; content is rich — highlighted code, column diffs, tables, progress, images — and a plugin puts live, interactive things on screen by publishing a `View` (ADR-0013), never by touching the TUI. Motion is rhythm: nothing still flickers, nothing fast flashes, everything that waits has a clock.

## Five plans, and their order

| plan | owner | content | starts |
|---|---|---|---|
| **M11a** `M11a-frame.md` | worker | the frame: regions, a block cache, smooth scrolling, search, selection and OSC 52 copy, mouse, cards and sheets as layers, print-on-exit, header and footer | now |
| **M11b** `M11b-hierarchy-tokens.md` | worker | §2 and §4 of the design: gutter, indents, receipts folded, thinking decay, paths, footer, borderless composer, tabs, measure, the raised tint; every snapshot redone | now, in parallel with M11a (different files) |
| **M11c** `M11c-motion-notices.md` | worker | §6 and §7: the animation clock, breath, comet tail, flips and rises, reveals, toasts, pulse, meter, activity delay, idle no-redraw, focus + OSC 9/777, reduced motion | after M11a and M11b |
| **M11d** `M11d-views-extensions.md` | kernel (sdk + reducer + wire), then worker (rendering) | ADR-0013: the `View` vocabulary, `ToolOutput.display`, `Signal`, actions; rendering in the TUI, print and RPC; a demo plugin | sdk now; rendering after M11b |
| **M11e** `M11e-content-kinds.md` | worker | §5: markdown tables, syntax highlighting, word-level diffs, images, `@` completion, the pager sheet, background detection and the truecolor palettes, rewind picker, reasoning sheet | after M11a and M11d |

Two workers run at once at most: M11a and M11b touch different files and run together; M11c and M11e follow. The sdk changes once, in M11d, before its rendering.

## Shared exit criteria

- [ ] every plan's own criteria ticked with output pasted
- [ ] `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `scripts/check_discipline.sh`, `scripts/budget.sh`, `cargo deny check`, `scripts/tui-smoke.sh`
- [ ] every region and layer in `docs/design/tui.md` §3 and every node in §5 has a `TestBackend` snapshot at 80×24 and 120×40; `assert_row_styled` covers §4's placement rule
- [ ] every row of §6 has a frame-by-frame test with an injected `now`; idle draws 0 frames in 2 s
- [ ] the tmux drives (smoke + card + sheet + toast + signal + action + mouse) pass on macOS and Linux; a PTY run under `portable-pty` + `vt100` asserts the print-on-exit screen
- [ ] `bingo-surface-tui` still depends on no provider or tool crate; no crate but it depends on ratatui/crossterm
- [ ] `docs/design/tui.md` §10 records every taste decision taken while building

## Dependencies (each verified on crates.io in its own plan, each with a `scripts/budget.sh` run and an ADR-0013 or plan line)

`syntect` with `default-fancy` (pure Rust) + `two-face` (M11e); `ratatui-image` (M11e; pulls `image` — the largest single addition, measured before accepted); `nucleo-matcher` (M11e); `terminal-colorsaurus` (M11e); `portable-pty` + `vt100` as dev-dependencies (M11a). Budget cap 268 is raised per plan as each lands; the estimate for all of M11 is ≈ +45.

## Non-goals

A mouse-only path (the mouse scrolls, clicks and selects; it is never required). Themes beyond terminal / light / dark truecolor. A GUI. Per-plugin widget crates (ADR-0013 §6 keeps the hatch shut). Forms as an interaction kind (own ADR when a plugin needs one). Inline mode or the terminal's own scrollback (set aside 2026-08-30 for full control). Panes the user resizes; tabs the user reorders.

## Risks

R-frame — owning scrolling, selection and copy means owning their bugs: the block cache, OSC 52 refusals and mouse capture each have a switch (`BINGO_MOUSE=off`) and a measured budget in M11a. R-images — `ratatui-image` + `image` may not fit the budget; images then stay `[image: name]` with an overlay via the terminal's own viewer. R-taste — five plans by three hands drift, and "alive" slides into "busy"; §6's rule (a cue exists only with a state row) and §10 of the design doc are the ratchet, and every plan's review reads moving screens in tmux, not diffs.
