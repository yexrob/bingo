# M11c — Motion: every motion reports a state

## Goal

Every row of `docs/design/tui.md` §6 exists and has a frame-by-frame test on an injected clock: the presence mark breathes while a turn runs, streaming text carries a comet tail, a finished tool flips with one bold frame, a new block rises into place, a card dims the world and reveals top-down through the kernel's guard, a sheet slides, a toast enters and fades, a waiting child pulses, the context meter warms, scrolling eases; a fast turn never flashes an activity row; an idle surface draws nothing; the window being unfocused turns a completion or a question into a system notification; `BINGO_MOTION=off` stills all of it. Esc is one ordered stack and `ctrl+c` says what it does.

## Bricks, in build order (owner)

1. **`clock.rs`** (worker) — `Now` gains `motion: bool` (from `BINGO_MOTION`) and `Anim{started, len}` with `progress(now) -> f32` and the two easings (`ease_out`, `ease_in_out`); every cue is a pure function of `Now` and state. The loop's tick is 33 ms while any `Anim` or spinner is live and absent otherwise.
2. **Breath** (worker) — the header `✻` at `70 % + 30 % · ease_in_out(t/1.6 s)` brightness, drawn through a five-step brightness ramp of `structure` (truecolor) or `dim ↔ text` (ANSI); still when no turn runs.
3. **Comet tail** (worker) — the last eight cells of a streaming block styled on a ramp from `structure` to `text` by age (0-150 ms), the caret riding the edge; deltas folded per frame.
4. **Flips and rises** (worker) — a tool's completion draws one frame bold before settling; a new block enters offset two rows up and eases to place over three frames; both off under `motion: false`.
5. **Layers' reveals** (worker) — a card's rows are dim until the guard lifts and brighten in one frame with the bell; its reveal (M11a `layers.rs`) is 0..3 frames top-down; a sheet slides four frames; closing reverses; frames snapshotted at each step.
6. **Toasts** (worker) — notices enter the header's right edge over four frames, hold 4 s, dim for 1 s, leave; at most three stacked; a rejected intent's toast names the intent text in dim.
7. **Pulse and meter** (worker) — the `needs you` badge and a waiting child's tab alternate `text`/`attention` at 1 Hz; the context meter's colour interpolates `dim → attention` across the last 20 % before the trigger.
8. **Activity row** (worker) — shown only when the turn has run 300 ms: `⠹ <verb> · 4s · esc to interrupt`; the verb is the running tool's name, `thinking`, or `waiting`; the clock ticks once a second.
9. **Focus and notifications** (worker) — crossterm focus events; unfocused, `InteractionOpened` and a root `TurnCompleted` emit OSC 777 (OSC 9 where 777 is unknown) `bingo · needs you` / `bingo · done`, tmux-wrapped; the bell stays; the title marks attention as today.
10. **Esc and ctrl+c** (worker) — the stack `sheet → card → dropdown → interrupt` and the two-press exit are tables in `keys.rs` with one test each; help prints the stack.

## Files

`crates/bingo-surface-tui/src/{clock,run,view,header,footer,layers,transcript,dialog,keys,input,terminal,ui}.rs`, `test_support.rs` (a `Recorder` that captures OSC bytes and counts draws; a `frames_at(times)` helper), snapshots per animation step.

## Dependencies

None (crossterm has focus and mouse events; OSC is bytes).

## Exit criteria

- [ ] a scripted turn that completes in 200 ms produces no activity row; one that runs 400 ms produces it at 300 ms
- [ ] breath: five brightness samples across 1.6 s match the easing; still when idle; `BINGO_MOTION=off` holds it at `structure`
- [ ] comet tail: samples at 0, 75 and 150 ms show the ramp; `--print` never sees it (it is style, not text)
- [ ] a completion flips bold for exactly one frame; a block rises through offsets 2, 1, 0
- [ ] card rows dim at `guard_until - 1ms`, plain at `guard_until`; reveal frames 0-3 snapshotted; the backdrop all dim
- [ ] a toast's four entry frames and its fade; three stacked; a fourth waits
- [ ] the pulse alternates on the 1 s boundary; the meter's colour at 79 %, 90 % and 100 % of the trigger
- [ ] idle: 0 draws in 2 s; under a 1 kHz delta storm ≤ 1 draw per 33 ms
- [ ] the `Recorder` sees exactly one OSC 777 when a question opens unfocused and none when focused; tmux wrapping byte-asserted
- [ ] Esc stack and ctrl+c tables have a test per row; `scripts/tui-smoke.sh` gains the focus and toast scenes

## Non-goals

Motion for decoration (a cue without a state row in §6 is not added). Sound beyond the bell. Notifications while focused. Easing curves beyond the two named.

## Risks

Terminals that echo focus or mouse reports as keys — gated on the capability probe, off otherwise. Brightness ramps on ANSI-only terminals collapse to two steps — accepted and snapshotted. Battery: the 33 ms tick lives only while something animates; the idle counter test is the guard.
