# M11 — TUI depth: a full-screen surface that is alive

## Goal

The terminal surface described in `docs/design/tui.md`: a full-screen application that owns every cell — a virtualised transcript from the top edge, a rail, cards and sheets as layers, a fixed composer, one status line under it — and feels alive: every motion reports a state; the eye lands on the answer or on what wants you; content is rich — highlighted code, column diffs, tables, progress, images — and a plugin puts live, interactive things on screen by publishing a `View` (ADR-0013), never by touching the TUI. Motion is rhythm: nothing still flickers, nothing fast flashes, everything that waits has a clock.

## Five plans, and their order

| plan | owner | content | starts |
|---|---|---|---|
| **M11a** `M11a-frame.md` | worker | the frame: regions, a block cache, smooth scrolling, search, selection and OSC 52 copy, mouse, cards and sheets as layers, print-on-exit, the status line | now |
| **M11b** `M11b-hierarchy-tokens.md` | worker | §2 and §4 of the design: the `⏺ ⎿ >` grammar, receipts folded, thinking decay, paths, the input box and status line, child rows and the switcher, cards, measure, the raised tint; every snapshot redone | now, in parallel with M11a (different files) |
| **M11c** `M11c-motion-notices.md` | worker | §6 and §7: the animation clock, breath, comet tail, flips and rises, reveals, toasts, pulse, meter, activity delay, idle no-redraw, focus + OSC 9/777, reduced motion | after M11a and M11b |
| **M11d** `M11d-views-extensions.md` | kernel (sdk + reducer + wire), then worker (rendering) | ADR-0013: the `View` vocabulary, `ToolOutput.display`, `Signal`, actions; rendering in the TUI, print and RPC; a demo plugin | sdk now; rendering after M11b |
| **M11e** `M11e-content-kinds.md` | worker | §5: markdown tables, syntax highlighting, word-level diffs, images, `@` completion, the pager sheet, background detection and the truecolor palettes, rewind picker, reasoning sheet | after M11a and M11d |

Two workers run at once at most: M11a and M11b touch different files and run together; M11c and M11e follow. The sdk changes once, in M11d, before its rendering.

## Shared exit criteria

- [x] every plan's own criteria ticked with output pasted
- [x] `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `scripts/check_discipline.sh`, `scripts/budget.sh`, `cargo deny check`, `scripts/tui-smoke.sh`
- [x] every region and layer in `docs/design/tui.md` §3 and every node in §5 has a `TestBackend` snapshot at 80×24 and 120×40; `assert_row_styled` covers §4's placement rule
- [x] every row of §6 has a frame-by-frame test with an injected `now`; idle draws 0 frames in 2 s
- [~] the tmux drives (smoke + card + sheet + toast + signal + action + mouse) pass on macOS; **Linux has never run** (carried); a PTY run under `portable-pty` + `vt100` asserts the print-on-exit screen
- [x] `bingo-surface-tui` still depends on no provider or tool crate; no crate but it depends on ratatui/crossterm
- [x] `docs/design/tui.md` §10 records every taste decision taken while building

## Dependencies (each verified on crates.io in its own plan, each with a `scripts/budget.sh` run and an ADR-0013 or plan line)

`syntect` with `default-fancy` (pure Rust) + `two-face` (M11e); `ratatui-image` (M11e; pulls `image` — the largest single addition, measured before accepted); `nucleo-matcher` (M11e); `terminal-colorsaurus` (M11e); `portable-pty` + `vt100` as dev-dependencies (M11a). Budget cap 268 is raised per plan as each lands; the estimate for all of M11 is ≈ +45.

## Non-goals

A mouse-only path (the mouse scrolls, clicks and selects; it is never required). Themes beyond terminal / light / dark truecolor. A GUI. Per-plugin widget crates (ADR-0013 §6 keeps the hatch shut). Forms as an interaction kind (own ADR when a plugin needs one). Inline mode or the terminal's own scrollback (set aside 2026-08-30 for full control). Panes the user resizes.

## Risks

R-frame — owning scrolling, selection and copy means owning their bugs: the block cache, OSC 52 refusals and mouse capture each have a switch (`BINGO_MOUSE=off`) and a measured budget in M11a. R-images — `ratatui-image` + `image` may not fit the budget; images then stay `[image: name]` with an overlay via the terminal's own viewer. R-taste — five plans by three hands drift, and "alive" slides into "busy"; §6's rule (a cue exists only with a state row) and §10 of the design doc are the ratchet, and every plan's review reads moving screens in tmux, not diffs.

## Verified — 2026-08-31

Each plan carries its own Verified section with output. The shared gates, on
main at the M11e merge (`d7d5c59`, all five workers in):

```
cargo fmt --all -- --check                                       0
cargo check --workspace --all-targets --locked                   0
cargo clippy --workspace --all-targets --locked -- -D warnings   0
cargo test --workspace --locked          1911 passed, 0 failed
cargo test -p bingo-surface-tui --lib     444 passed (0.76 s)
scripts/check_discipline.sh              0  discipline ok
scripts/budget.sh                        0  dependencies (unique, normal): 282 (max 282)
cargo deny check                         0  advisories, bans, licenses, sources ok
scripts/tui-smoke.sh                     0  14 scenes (macOS, tmux 3.6b)
```

The two wall-clock tests (`highlight::…_under_a_millisecond`,
`view::…_frame_budget`) fail deterministically on a machine whose 1-minute
load is ~30+ and pass with room at load < 6; the numbers above are the quiet
run. A loaded dev machine is not a regression signal for them.

Reviewed on the real binary (tmux, truecolor): the three inks on a Rust
fence, the ruled table with right-hugging numbers, the pager round trip with
`/` search, `@Car` → `@Cargo.toml`, `BINGO_THEME=light` wearing the light
inks with no dark token leaking, the progress card animating, the two-press
exit.

Dependencies: the estimate was ≈ +45; the spend was **+14** (268 → 269 for
`bingo-demo-ui`, → 282 for M11e's four crates) because images were refused
at their measured +33 (`M11e-content-kinds.md` brick 5).

### Carried out of M11

- Linux: the tmux drives and the PTY smoke have run on macOS only.
- Keyboard block focus: `⏎` into a child row is mouse/switcher-only; `ctrl+↑/↓` would give it.
- No `/rewind` command exists; `esc esc` is silent until a store command lands (M11e brick 8).
- `ItemBody::Asset` has no dimensions or mime; a future image brick wants them.
- `SessionState.signals` is a BTreeMap with no arrival order — "newest last" is not derivable.
- `Input::Action` records no item; a pending button is `(action, seq)` in `Ui`.
- The 36 old `view__tests__*` snapshots duplicate `screens.rs`; `raised()` is truecolor-only.
- The comet tail counts chars, not display cells; `motion.rs` is 749 lines (> 700 warn).
- `↓ 0.0k tokens` shows on zero-usage turns; the theme ratchet's scan stops at a file's first `#[cfg(test)]`.
- Observed once, not reproduced: a key answered against `Painted::default()` before the first paint (M11e plan, "Observed, not caused"); the one-line fix is named there.
