# M68 — The line that waits

## Goal

User, 2026-09-05, two asks about a line typed while a turn runs:

1. **A queued line cannot be edited.** Once sent behind a running
   turn it sits in the queue and there is no way back into the box.
   Wanted: `↑` in the TUI walks into the queued lines and puts one
   back in the composer to edit.
2. **`⏎` steers, `tab` queues.** Today every line sent mid-turn is
   queued and the running turn *absorbs* it at its next barrier
   (ADR-0008 §2) — that is a steer, and the only thing a person can
   do. Wanted: `⏎` keeps that meaning; `tab` sends a line that waits
   for the turn to end and opens the next one.

## Shape

- `Input::Text` gains `delivery: Delivery` (`Wake` | `Hold`, serde
  default `Wake`, skipped when default — old frames byte-identical).
  `Delivery` is the sdk's own word already (`HostApi::deliver`), so
  no new noun. The queue's barrier absorbs only `Wake` prose;
  `QueueEntry.steerable` becomes what it was meant to say: not a
  command **and** not held. A `Hold` line waits behind the turn and
  opens the next one; two held lines are one turn's inputs as today.
- `HostApi::withdraw(session, intent) -> Result<Input, KernelError>`:
  removes a queued entry that has not been taken, and only one the
  caller's own surface put there (the entry's origin; another
  surface's line is `PERMISSION_DENIED`); `QueueChanged` follows.
  `NOT_FOUND` for an intent no longer queued (taken, or never there).
  Contract first: the sdk method (default body), the host test.

## Bricks

1. **The wire.** `Input::Text.delivery`, schemas regenerated, the
   frames fixture unchanged and one frame added; `queue.rs`'s
   `take_prose` stops at the first held line as it stops at a
   command; `steerable` derived from both. Tests on the queue brick.
2. **The verb.** `withdraw` on the actor: find the intent, check the
   origin's surface, remove, publish `QueueChanged`, return the
   `Input`. Host test through a real session with a fake provider
   mid-turn.
3. **The keys.** In the TUI, with a turn running: `⏎` submits as
   today (`Wake`); `tab` submits `Hold` (with the composer non-empty
   — `tab` on an empty box keeps whatever it does now, and `tab`
   with a dropdown open still completes). The pending rows under the
   activity say which is which: a steer row as today, a held row
   with a dim `waits` tag. The composer's placeholder while a turn
   runs reads `ask anything · ⏎ steers · tab queues` (one string in
   `keys.rs`, snapshot). Help sheet row for `tab`.
4. **`↑` walks into the queue.** On an empty composer with the
   person's own entries queued, `↑` withdraws the **newest** one into
   the composer (text and pictures — the tokens and `ui.pictures`
   come back as they were before the send; `run/submit.rs` knows how
   they went out) and the row leaves the pending area. `↑` again
   withdraws the next; history recall resumes only when nothing of
   the person's is queued. `⏎`/`tab` send it again. `esc` on a
   withdrawn line clears the box as it clears any line (the queue
   does not get it back — the person has it).
5. **Other surfaces.** `--print --input-format stream-json` and RPC
   carry `delivery` through; nothing else changes there.

## Files

`bingo-sdk/src/{event.rs,host.rs}`, `schema/*.json`, `bingo-core/src/
session/{queue.rs,inputs.rs}` + `session.rs` + host tests,
`bingo-surface-tui/src/{input.rs,keys.rs,view.rs,run/submit.rs}` +
snapshots, `docs/adr/0008-commands.md` §2 dated amendment,
`docs/design/tui.md` §6/§7 dated line.

## Exit criteria

- [ ] A held line is not absorbed at the barrier and opens the next
      turn; a `Wake` line is absorbed as before (host tests).
- [ ] `withdraw` returns the input, refuses another surface's entry,
      publishes `QueueChanged`.
- [ ] TUI: `tab` mid-turn queues, `⏎` steers, both rows drawn
      (snapshot); `↑` on an empty box brings the newest queued line
      back with its pictures; a second `↑` the next; history only
      after.
- [ ] All gates; Windows cross-check for sdk/core.
- [ ] Hands-on: appended by the parent.

## Non-goals

Reordering the queue; editing a queued command (`/x` lines withdraw
too, that is free — but no special UI); a held line surviving the
session.

## Risks

`tab` already means complete in the dropdowns and the switcher — the
new meaning holds only with no layer open and a turn running; write
the precedence down in `keys.rs` and test the three cases. A withdraw
racing the barrier: the actor is single-threaded per session, so
either the entry is still there or it was taken — `NOT_FOUND` is the
honest answer and the TUI then leaves the box empty and says
`already sent` in the status line.
