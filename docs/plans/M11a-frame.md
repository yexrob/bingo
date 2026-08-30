# M11a — The frame: a full-screen surface that owns its scrolling

## Goal

`bingo` at a terminal is a full-screen application laid out as `docs/design/tui.md` §3: nothing above the transcript, which starts at the top edge; a virtualised transcript we scroll ourselves — smoothly, by line, with the wheel, `pgup/pgdn` and `ctrl+f` search — with selection and copy through OSC 52; a rail past 120 columns; cards and sheets as layers over it; the composer on a fixed baseline with the status line's three slots under it. Quitting prints the last screenful of the transcript into the shell so the conversation is still there after `exit`. A 5 000-block transcript scrolls at 30 fps with a draw under 4 ms.

## Bricks, in build order (owner)

1. **Regions** (worker) — `frame.rs`: the pure layout `regions(size, ui) -> Regions{transcript, rail, activity, composer, status}`; the rail exists at ≥ 120 columns; every region has a minimum and the composer and status line are never dropped; a table test at 80×24, 100×30, 120×40, 200×60.
2. **Block cache** (worker) — `blocks.rs`: one rendered block per item, keyed by `(item id, width, revision)`; a completed item renders once; a streaming item re-renders its last block only; the cache is dropped on width change. Measured: 5 000 items, 200 ms to warm, then zero re-renders on scroll.
3. **Scrolling** (worker) — `scroll.rs`: the pure state `Scroll{offset, target, since}`; `pgup/pgdn`, wheel, `home/end`, follow-the-tail while at the bottom; ease-out over 100 ms on an injected clock; a viewport that never shows a torn block boundary.
4. **Search** (worker) — `ctrl+f` opens a one-row search in the status line's place; matches highlighted with `structure`, `n/N` steps, the transcript scrolls to the match; `esc` closes.
5. **Selection and copy** (worker) — `v` starts a keyboard selection from the focused block, arrows extend, `y` copies; a mouse drag selects cells; copy writes OSC 52 (base64, chunked under 100 KiB) and falls back to a toast naming the size when the terminal refused; the selection is drawn with `raised`.
6. **Mouse** (worker) — crossterm mouse capture on; wheel scrolls; a click on a child's row steps into it; a click on a card row answers; a click in the transcript focuses a block; nothing needs the mouse.
7. **Layers** (worker) — `layers.rs`: `Card` and `Sheet` as the two layer kinds with their reveal state (frame 0..3) on the clock; the dim backdrop is a style pass over the regions beneath; the reveal frames are pure functions of `now`; the M6 dialog and the switcher become cards, help and the picker become sheets.
8. **Leaving** (worker) — on exit, after leaving the alternate screen, print the last `rows - 2` lines of the transcript as plain text through the block cache's degrade; `--no-print-on-exit` skips it.
9. **Status line** (worker) — `status.rs`: the three slots of §4 — mode left; notices middle (`N needs you (ctrl+g)`, `N running`, `context N%` from 70 % of the trigger, the latest notice, else `? for shortcuts` while the composer is empty); place right (`in <session> · <model>`); the middle truncates first; a notice queue `push(text)` holding each 4 s on the clock (its fade is M11c's); the context sparkline over the last eight turns' `ContextUsage` draws in the `/status` sheet.

## Files

`crates/bingo-surface-tui/src/{run,view,ui,tree,terminal,input,keys}.rs`, new `frame.rs`, `blocks.rs`, `scroll.rs`, `search.rs`, `select.rs`, `layers.rs`, `status.rs`, `tests/pty.rs`, `scripts/tui-smoke.sh`, `Cargo.toml` (dev-deps), `scripts/budget.toml`.

## Dependencies

`portable-pty`, `vt100` (dev) for the pty harness. Nothing at runtime.

## Exit criteria

- [x] regions table test at four sizes; the composer and status line survive 20×5
- [x] block cache: 5 000 items warm in under 200 ms; scrolling re-renders nothing (counter); a width change drops everything once
- [x] scroll eases over 100 ms (three sampled offsets on an injected clock); follow-the-tail holds while streaming and releases on `pgup`
- [x] search finds across blocks, highlights, steps, scrolls; `esc` restores the status line
- [x] selection: keyboard and mouse; OSC 52 bytes asserted by the `Recorder`; the refusal toast
- [x] mouse: wheel, child-row click, card click, block focus — each a test with a synthetic event
- [x] cards and sheets: reveal frames 0-3 snapshotted; the backdrop is all dim; `esc` reverses
- [x] leaving: the pty test sees the last screenful in the normal screen after exit, and nothing with `--no-print-on-exit`
- [x] status line snapshots at 80 and 120 with every slot filled and every slot empty; `context` absent at 41 %, `dim` at 70 %, `bad` at 90 %; the middle slot truncates before the others
- [x] a full draw at 120×40 under 4 ms (timed test, release profile noted); idle draws 0 in 2 s
- [ ] `scripts/tui-smoke.sh` and the pty test green on macOS and Linux

## Non-goals

Inline mode or the terminal's scrollback (set aside 2026-08-30). Reflow of old blocks on resize beyond the cache drop. Panes the user resizes.

## Risks

OSC 52 is refused by some terminals and by tmux without `set-clipboard on` — the toast says so and names the size. Mouse capture steals the terminal's own selection: `BINGO_MOUSE=off` returns it. The block cache and streaming: the last block is the only mutable one, by construction. A 30 fps tick and battery — the tick runs only while something animates (M11c).

## Verified

2026-08-31, worker A, on macOS 27 (aarch64), rustc stable, debug profile
unless noted.

### The bricks

```
$ cargo test -p bingo-surface-tui --lib -- frame:: blocks:: scroll:: search:: select:: layers:: status::
test frame::tests::the_four_sizes_lay_out_the_same_frame ... ok
test frame::tests::the_composer_and_the_status_line_survive_the_smallest_screens ... ok
test frame::tests::the_rail_appears_at_a_hundred_and_twenty_columns_and_not_before ... ok
test blocks::tests::a_finished_item_is_drawn_once_and_scrolling_draws_nothing ... ok
test blocks::tests::a_width_change_drops_every_block_once ... ok
test blocks::tests::a_window_is_an_exact_slice_of_the_whole_transcript ... ok
test scroll::tests::a_page_back_eases_over_a_tenth_of_a_second ... ok
test scroll::tests::a_held_transcript_keeps_its_line_while_more_arrives ... ok
test search::tests::a_committed_query_finds_every_occurrence_in_reading_order ... ok
test select::tests::osc_52_carries_the_selection_as_base64 ... ok
test select::tests::a_selection_too_large_for_the_terminal_is_refused_by_name ... ok
test layers::tests::a_card_comes_down_over_three_frames ... ok
test layers::tests::esc_runs_the_same_frames_backwards ... ok
test status::tests::the_context_notice_appears_at_seventy_percent_of_the_trigger ... ok
test status::tests::every_slot_filled_and_every_slot_empty ... ok
test result: ok. 54 passed; 0 failed; 0 ignored; 0 measured; 185 filtered out
```

### The frame, the mouse, the clock

```
$ cargo test -p bingo-surface-tui --lib -- input::tests::a_click view::tests::a_card run::tests::an_idle run::tests::leaving view::tests::a_full_draw
test input::tests::a_click_on_a_card_row_answers_it ... ok
test input::tests::a_click_in_the_transcript_focuses_the_block_it_landed_on ... ok
test input::tests::a_click_on_a_child_row_steps_into_it ... ok
test view::tests::a_card_comes_down_from_its_top_edge ... ok
test run::tests::an_idle_surface_draws_nothing_at_all ... ok
test run::tests::leaving_hands_the_last_screenful_of_the_transcript_back ... ok
test view::tests::a_full_draw_of_a_long_transcript_is_inside_the_frame_budget ... ok
test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 221 filtered out
```

A warm draw of a 5 000-block transcript at 120×40: **0.80 ms in release**,
3.2 ms in debug, against §6's 4 ms. The test holds release to the budget
and debug to four times it; the number above was read off the assertion.

### The terminal

```
$ cargo test -p bingo --test pty
test no_print_on_exit_leaves_the_shell_as_it_was ... ok
test leaving_prints_the_last_screenful_into_the_shells_own_screen ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo build && scripts/tui-smoke.sh
  a reply reaches the transcript
  esc interrupts a turn that is still waiting
  a page up releases the tail, and the foot takes it back
  the wheel scrolls the transcript
  ctrl+f searches the transcript and esc gives the status line back
  the help sheet opens on ? and closes on esc
  a permission dialog answered y runs the tool
tui-smoke ok
```

### The gates

```
cargo fmt --all -- --check                                    0
cargo check --workspace --all-targets --locked                0
cargo clippy --workspace --all-targets --locked -- -D warnings 0
cargo test --workspace --locked                               0   (1 678 passed, 54 binaries)
scripts/check_discipline.sh                                   0
scripts/budget.sh                                             0   (268 unique normal deps, max 268)
cargo deny check                                              0
cargo build && scripts/tui-smoke.sh                           0
```

`portable-pty` 0.9 and `vt100` 0.16 are dev-dependencies of the `bingo`
crate, where the binary under test lives: they do not enter
`cargo tree -e normal`, so the dependency count is unmoved at 268 and the
budget's cap stands unraised.

### Not verified

- Linux. Everything above ran on macOS only; the pty harness and the tmux
  drive use nothing platform-specific, but neither has been run on Linux.
