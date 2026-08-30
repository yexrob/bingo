# M11 — TUI depth: the tape, made rich

## Goal

The terminal surface described in `docs/design/tui.md`: the conversation lives in the terminal's own scrollback and the surface owns a live region at the bottom; the eye lands on the answer or on what wants you; content is rich — highlighted code, column diffs, tables, progress, images — and a plugin puts live, interactive things on screen by publishing a `View` (ADR-0013), never by touching the TUI. Motion is rhythm: nothing still flickers, nothing fast flashes, everything that waits has a clock.

## Five plans, and their order

| plan | owner | content | starts |
|---|---|---|---|
| **M11a** `M11a-tape-viewport.md` | kernel (spike), then worker | the inline viewport: settled rows written once above a live region; overlays on the alternate screen; one tape for the tree; resize, tmux, four terminals | now — a one-week spike decides the shape |
| **M11b** `M11b-hierarchy-tokens.md` | worker | §2 and §4 of the design: gutter, indents, receipts folded, thinking decay, paths, footer, borderless composer, band, measure; every snapshot redone | now, in parallel with the spike (holds on today's screen too) |
| **M11c** `M11c-motion-notices.md` | worker | §6 and §7: activity delay, guard settle, caret, pulse, idle no-redraw, focus events + OSC 9/777, reduced motion, ASCII fallback | after M11b |
| **M11d** `M11d-views-extensions.md` | kernel (sdk + reducer + wire), then worker (rendering) | ADR-0013: the `View` vocabulary, `ToolOutput.display`, `Signal`, actions; rendering in the TUI, print and RPC; a demo plugin | sdk now; rendering after M11b |
| **M11e** `M11e-content-kinds.md` | worker | §5: markdown tables, syntax highlighting, word-level diffs, images, `@` completion, the pager overlay, theme detection, rewind UI, reasoning expansion | after M11a and M11d |

Two workers run at once at most: M11a's spike and M11b are independent; M11c and M11e follow. The sdk changes once, in M11d, before its rendering.

## Shared exit criteria

- [ ] every plan's own criteria ticked with output pasted
- [ ] `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `scripts/check_discipline.sh`, `scripts/budget.sh`, `cargo deny check`, `scripts/tui-smoke.sh`
- [ ] every screen in `docs/design/tui.md` §3 and every node in §5 has a `TestBackend` snapshot; `assert_row_styled` covers §4's placement rule
- [ ] every row of §6 has a timing test with an injected `now`
- [ ] the tmux drives (smoke + settle + overlay + signal + action) pass on macOS and Linux; a PTY run under `portable-pty` + `vt100` asserts scrollback content
- [ ] `bingo-surface-tui` still depends on no provider or tool crate; no crate but it depends on ratatui/crossterm
- [ ] `docs/design/tui.md` §10 records every taste decision taken while building

## Dependencies (each verified on crates.io in its own plan, each with a `scripts/budget.sh` run and an ADR-0013 or plan line)

`syntect` with `default-fancy` (pure Rust) + `two-face` (M11e); `ratatui-image` (M11e; pulls `image` — the largest single addition, measured before accepted); `nucleo-matcher` (M11e); `terminal-colorsaurus` (M11e); `portable-pty` + `vt100` as dev-dependencies (M11a). Budget cap 268 is raised per plan as each lands; the estimate for all of M11 is ≈ +45.

## Non-goals

A mouse-driven UI (the wheel scrolls overlays; that is all). Themes beyond terminal / light / dark truecolor. A GUI. Per-plugin widget crates (ADR-0013 §6 keeps the hatch shut). Forms as an interaction kind (own ADR when a plugin needs one). Virtualising the transcript (the tape is scrollback; the live region is small by construction). A pager over anything but the latest output.

## Risks

R-inline — ratatui's inline viewport is fixed-height by construction and `insert_before` interacts with resize and with tmux's scroll regions; the spike's verdict is binding and its fallback (alternate screen with write-once emulation) is written down before code. R-images — `ratatui-image` + `image` may not fit the budget; images then stay `[image: name]` with an overlay via the terminal's own viewer. R-taste — five plans by three hands drift; §10 of the design doc is the ratchet, and every plan's review reads screens, not diffs.
