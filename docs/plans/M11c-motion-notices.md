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

- [ ] a scripted turn that completes in 200 ms produces no activity row; one that runs 400 ms produces it at 300 ms
- [ ] sparkle and glow: the glyph at 0, 150, 300, 450 ms; five brightness samples across 1.6 s match the easing; still when idle; `BINGO_MOTION=off` holds `✻` at `presence`
- [ ] comet tail: samples at 0, 75 and 150 ms show the ramp; `--print` never sees it (it is style, not text)
- [ ] a live bullet's pulse at 0, 0.6 and 1.2 s; a completion flips bold for exactly one frame; a block rises through offsets 2, 1, 0
- [ ] card rows dim at `guard_until - 1ms`, plain at `guard_until`; reveal frames 0-3 snapshotted; the backdrop all dim
- [ ] a notice's two entry frames and its fade; a second waits until the first leaves
- [ ] the pulse alternates on the 1 s boundary; `context` absent at 69 %, `dim` at 79 %, between at 90 %, `bad` at 100 % of the trigger
- [ ] idle: 0 draws in 2 s; under a 1 kHz delta storm ≤ 1 draw per 33 ms
- [ ] the `Recorder` sees exactly one OSC 777 when a question opens unfocused and none when focused; tmux wrapping byte-asserted
- [ ] Esc stack and ctrl+c tables have a test per row; `scripts/tui-smoke.sh` gains the focus and notice scenes

## Non-goals

Motion for decoration (a cue without a state row in §6 is not added). Sound beyond the bell. Notifications while focused. Easing curves beyond the two named.

## Risks

Terminals that echo focus or mouse reports as keys — gated on the capability probe, off otherwise. Brightness ramps on ANSI-only terminals collapse to two steps — accepted and snapshotted. Battery: the 33 ms tick lives only while something animates; the idle counter test is the guard.
