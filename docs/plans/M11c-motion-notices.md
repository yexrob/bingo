# M11c — Motion: every motion reports a state

## Goal

Every row of `docs/design/tui.md` §6 exists and has a frame-by-frame test on an injected clock: the presence mark sparkles and the input box glows while a turn runs, a live bullet pulses, streaming text carries a comet tail, a finished tool flips with one bold frame, a new block rises into place, a card dims the world and reveals top-down through the kernel's guard, a sheet slides, a toast enters and fades, a waiting child pulses, the context notice warms, scrolling eases; a fast turn never flashes an activity row; an idle surface draws nothing; the window being unfocused turns a completion or a question into a system notification; `BINGO_MOTION=off` stills all of it. Esc is one ordered stack and `ctrl+c` says what it does.

## Bricks, in build order (owner)

1. **`clock.rs`** (worker) — `Now` gains `motion: bool` (from `BINGO_MOTION`) and `Anim{started, len}` with `progress(now) -> f32` and the two easings (`ease_out`, `ease_in_out`); every cue is a pure function of `Now` and state. The loop's tick is 33 ms while any `Anim` or spinner is live and absent otherwise.
2. **Sparkle and glow** (worker) — the activity row's `✻` cycles `✻ ✢ ✶ ✽` every 150 ms and breathes `65 % + 35 % · ease_in_out(t/1.6 s)` through a five-step ramp of `presence` (truecolor) or `dim ↔ presence` (ANSI); the input box border glows on the same clock; neither exists when no turn runs.
3. **Comet tail** (worker) — the last eight cells of a streaming block styled on a ramp from `presence`'s glow to `text` by age (0-180 ms); deltas folded per frame.
4. **Pulse, flips and rises** (worker) — a live tool's `⏺` pulses `presence` ↔ glow at 1.2 s; its completion draws one bold frame in `good`/`bad` before settling; a new block enters offset two rows up and eases to place over three frames; all off under `motion: false`.
5. **Layers' reveals** (worker) — a card's rows are dim until the guard lifts and brighten in one frame with the bell; its reveal (M11a `layers.rs`) is 0..3 frames top-down; a sheet slides four frames; closing reverses; frames snapshotted at each step.
6. **Notices** (worker) — a notice fades into the status line's middle slot over two frames (`dim → text`), holds 4 s, fades to dim over two and leaves; one at a time, the next waits; a rejected intent's notice names the intent text in dim.
7. **Needs-you pulse and the context notice** (worker) — the `needs you` notice, a waiting child's `⎿  Needs you` and its switcher row alternate `text`/`presence` at 1 Hz; the `context N%` notice appears at 70 % of the trigger and interpolates `dim → bad` across the last 20 %.
8. **Activity row** (worker) — shown only when the turn has run 300 ms: `✻ <Verb>… (esc to interrupt · 4s · ↓ 1.2k tokens)`; the verb is drawn once per turn from bingo's list (§4), the sparkle cycles, the clock ticks once a second, the token count is the turn's output so far.
9. **Focus and notifications** (worker) — crossterm focus events; unfocused, `InteractionOpened` and a root `TurnCompleted` emit OSC 777 (OSC 9 where 777 is unknown) `bingo · needs you` / `bingo · done`, tmux-wrapped; the bell stays; the title marks attention as today.
10. **Esc and ctrl+c** (worker) — the stack `sheet → card → dropdown → interrupt` and the two-press exit are tables in `keys.rs` with one test each; help prints the stack.

## Files

`crates/bingo-surface-tui/src/{clock,run,view,header,footer,layers,transcript,dialog,keys,input,terminal,ui}.rs`, `test_support.rs` (a `Recorder` that captures OSC bytes and counts draws; a `frames_at(times)` helper), snapshots per animation step.

## Dependencies

None (crossterm has focus and mouse events; OSC is bytes).

## Exit criteria

- [x] a scripted turn that completes in 200 ms produces no activity row; one that runs 400 ms produces it at 300 ms
- [x] sparkle and glow: the glyph at 0, 150, 300, 450 ms; five brightness samples across 1.6 s match the easing; still when idle; `BINGO_MOTION=off` holds `✻` at `presence`
- [x] comet tail: samples at 0, 75 and 150 ms show the ramp; `--print` never sees it (it is style, not text)
- [x] a live bullet's pulse at 0, 0.6 and 1.2 s; a completion flips bold for exactly one frame; a block rises through offsets 2, 1, 0
- [x] card rows dim at `guard_until - 1ms`, plain at `guard_until`; reveal frames 0-3 snapshotted; the backdrop all dim
- [x] a notice's two entry frames and its fade; a second waits until the first leaves
- [x] the pulse alternates on the 1 s boundary; `context` absent at 69 %, `dim` at 79 %, between at 90 %, `bad` at 100 % of the trigger
- [x] idle: 0 draws in 2 s; under a 1 kHz delta storm ≤ 1 draw per 33 ms
- [x] the `Recorder` sees exactly one notification when a question opens unfocused and none when focused; the OSC 777 bytes and the tmux wrapping are byte-asserted in `terminal.rs`
- [x] Esc stack and ctrl+c tables have a test per row; `scripts/tui-smoke.sh` gains the focus and notice scenes

## Non-goals

Motion for decoration (a cue without a state row in §6 is not added). Sound beyond the bell. Notifications while focused. Easing curves beyond the two named.

## Risks

Terminals that echo focus or mouse reports as keys — gated on the capability probe, off otherwise. Brightness ramps on ANSI-only terminals collapse to two steps — accepted and snapshotted. Battery: the 33 ms tick lives only while something animates; the idle counter test is the guard.

## Verified

2026-08-31, on `worker-c-m11c-motion`.

`motion.rs` is to §6 what `screens.rs` is to §3: one test per row, sampled at
the instants the row names. Every cue is a pure function of `Now` — which now
carries `motion: bool` — so nothing here sleeps.

```
$ cargo test -p bingo-surface-tui --locked motion::
test motion::the_sparkle_walks_its_four_glyphs_at_a_hundred_and_fifty_milliseconds ... ok
test motion::the_presence_mark_breathes_between_two_thirds_and_all_of_itself ... ok
test motion::the_input_box_glows_on_the_same_breath_and_is_dim_when_idle ... ok
test motion::nothing_of_the_presence_is_on_screen_while_no_turn_runs ... ok
test motion::a_comet_tail_cools_from_the_glow_to_the_text_behind_it ... ok
test motion::the_tail_is_style_and_never_text ... ok
test motion::a_live_bullet_pulses_between_presence_and_its_glow ... ok
test motion::a_completion_flashes_bold_for_exactly_one_frame ... ok
test motion::a_new_block_rises_two_rows_into_place ... ok
test motion::a_turn_that_answers_at_once_never_flashes_a_row ... ok
test motion::the_activity_row_says_a_verb_a_clock_and_what_the_turn_has_said ... ok
test motion::every_verb_is_one_of_bingos_own ... ok
test motion::a_cards_rows_are_dim_until_the_guard_lifts_and_plain_the_moment_it_does ... ok
test motion::a_card_reveals_top_down_and_a_sheet_slides_up ... ok
test motion::esc_runs_a_sheet_back_down_the_way_it_came ... ok
test motion::a_notice_arrives_out_of_dim_and_leaves_into_it ... ok
test motion::a_refused_line_is_named_after_the_reason_that_refused_it ... ok
test motion::what_wants_a_person_alternates_on_the_second ... ok
test motion::a_waiting_childs_row_and_its_switcher_line_pulse_with_it ... ok
test motion::the_context_notice_appears_at_seventy_and_warms_across_the_last_fifth ... ok
test motion::the_transcript_crossfades_through_dim_into_the_session_stepped_into ... ok
test motion::motion_off_holds_every_cue_at_its_resting_frame ... ok
test motion::motion_off_puts_a_layer_up_whole_and_takes_it_away_at_once ... ok
test motion::a_still_notice_is_said_at_once_and_still_leaves ... ok
test motion::a_finished_turn_takes_the_whole_of_the_presence_with_it ... ok
test motion::an_idle_frame_is_the_same_frame_a_second_later ... ok
test motion::the_rhythms_are_the_ones_the_design_names ... ok
test result: ok. 27 passed; 0 failed; 0 ignored; 0 measured; 331 filtered out
```

The loop's own budget, the notifications and the two key tables:

```
$ cargo test -p bingo-surface-tui --locked run:: keys::
test run::tests::an_idle_surface_draws_nothing_at_all ... ok
test run::tests::a_storm_of_deltas_costs_one_draw_a_frame_and_no_more ... ok
test run::tests::a_question_that_opens_on_a_window_nobody_watches_says_so ... ok
test run::tests::a_turn_that_finishes_on_a_window_nobody_watches_says_so ... ok
test keys::tests::esc_closes_the_innermost_thing_that_is_open ... ok
test keys::tests::ctrl_c_stops_a_turn_clears_a_line_and_then_leaves ... ok
test keys::tests::the_help_prints_the_escape_stack ... ok
test terminal::tests::a_notification_is_one_osc_sequence_in_the_dialect_the_terminal_takes ... ok
test terminal::tests::a_multiplexer_is_passed_through_with_the_escape_doubled ... ok
```

The gates, each by exit code:

```
$ cargo fmt --all -- --check                                  EXIT 0
$ cargo check --workspace --all-targets --locked              EXIT 0
$ cargo clippy --workspace --all-targets --locked -- -D warnings   EXIT 0
$ cargo test --workspace --locked                             EXIT 0
    bingo-surface-tui: 358 passed; 0 failed
$ scripts/check_discipline.sh                                 EXIT 0  (discipline ok)
$ scripts/budget.sh                                           EXIT 0  (budget ok)
$ cargo deny check                                            EXIT 0  (advisories ok, bans ok, licenses ok, sources ok)
$ cargo build && scripts/tui-smoke.sh                         EXIT 0
  …
  a notice holds the status line and then leaves it
  a question on a window nobody watches notifies and nothing else
  BINGO_ASCII=1 and NO_COLOR leave a terminal nothing it cannot draw
  tui-smoke ok
```

Not done here: nothing in the plan's bricks is outstanding. Two notes for
whoever reads this next. The loop test asserts *that* one notification is
written and that it carries the words, not which dialect it is in — the test
process's own `TERM_PROGRAM` and `TMUX` decide that, so the bytes of both
dialects and the tmux passthrough are asserted in `terminal::tests` instead.
And the 33 ms tick is a deadline measured from the last frame painted: a fresh
`sleep` per pass of the loop was cancelled by every arriving delta, which made
the surface go still exactly when it had the most to say.
