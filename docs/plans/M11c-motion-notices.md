# M11c — Motion and notices: rhythm, not animation

## Goal

Every row of `docs/design/tui.md` §6 and the ergonomic rules of §7 hold, each with a timing test on an injected clock: a fast turn never flashes an activity line; a dialog's keyboard guard is seen as dim rows that brighten; thinking has a clock and decays; a waiting child pulses in the footer; the window being unfocused turns a completion or a question into a system notification; an idle surface draws nothing; `BINGO_MOTION=off` freezes everything that moves. Esc is one ordered stack and `ctrl+c` says what it does.

## Bricks, in build order (owner)

1. **`clock.rs`** (worker) — `Now` gains what §6 needs: `since(instant)` and a `motion: bool` read once from `BINGO_MOTION`; every animated cue is a pure function of `Now` and the state (the spinner already is).
2. **Activity line** (worker) — shown only when the turn has run 300 ms: `⠹ <verb> · 4s · esc to interrupt`; the verb is the running tool's name, `thinking` during reasoning, `waiting` otherwise; the clock ticks once a second, never faster.
3. **Guard settle** (worker) — dialog rows drawn `dim` while `now < guard_until`, `text` after; the bell rings at open as today; the first frame after the guard is forced (the tick makes it at most 100 ms late).
4. **Caret and streaming** (worker) — a dim `▌` after the growing edge of streaming text and of the composer; deltas are folded per frame, never drawn per delta.
5. **Pulse** (worker) — the footer's `N needs you` alternates dim/text at 1 Hz while a child waits; off under `BINGO_MOTION=off`.
6. **Idle** (worker) — the loop's tick is armed only while something animates (spinner, clock, pulse, guard); with nothing on screen moving, no frame is drawn until a key or a kernel frame; a frame counter test proves 0 draws over 2 s idle.
7. **Focus and notifications** (worker) — crossterm focus events (`CSI ?1004h`); when unfocused, `InteractionOpened` and a root `TurnCompleted` emit an OSC 777 (or OSC 9 where 777 is unknown) notification `bingo · needs you` / `bingo · done`, wrapped for tmux (`DCS tmux; … ST`); the bell stays; the title marks attention as today.
8. **Notices** (worker) — 5 s on the status row, dim after 3 s; a notice about a rejected intent names the intent's text in dim.
9. **Esc and ctrl+c** (worker) — the stack `overlay → dialog → dropdown → interrupt` and the two-press exit are tables in `keys.rs` with one test each; the help panel prints the stack.

## Files

`crates/bingo-surface-tui/src/{clock,run,view,dialog,keys,input,terminal,ui}.rs`, `test_support.rs` (a `Recorder` that captures OSC bytes and counts draws), snapshots for the dim guard and the pulse.

## Dependencies

None (crossterm has focus events; OSC is bytes).

## Exit criteria

- [ ] a scripted turn that completes in 200 ms produces no activity line; one that runs 400 ms produces it at 300 ms (clock-injected test)
- [ ] the dialog's rows are dim at `guard_until - 1ms` and plain at `guard_until` (two snapshots)
- [ ] thinking shows `· 3s` at 3 s and `thought for 3s` after the answer starts
- [ ] the pulse alternates on the 1 s boundary; `BINGO_MOTION=off` holds it at `text`
- [ ] idle: 0 draws in 2 s; streaming: ≤ 1 draw per 16 ms under a 1 kHz delta storm
- [ ] the `Recorder` sees exactly one OSC 777 when a question opens unfocused and none when focused; the tmux wrapping is byte-asserted
- [ ] Esc stack and ctrl+c tables have a test per row; `scripts/tui-smoke.sh` gains the focus scene (tmux `select-pane` toggles focus)

## Non-goals

Easing, sliding, fades beyond dim/text. Sound beyond the bell. Notifications while focused.

## Risks

Terminals that echo focus events as keys — gated on the kitty/xterm capability query, off otherwise. Notification OSCs that a terminal renders as text — sent only to terminals that answered the capability probe or are named in a small allow-list; the fallback is the bell.
