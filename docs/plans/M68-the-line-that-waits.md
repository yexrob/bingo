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

- [x] A held line is not absorbed at the barrier and opens the next
      turn; a `Wake` line is absorbed as before (host tests).
- [x] `withdraw` returns the input, refuses another surface's entry,
      publishes `QueueChanged`.
- [x] TUI: `tab` mid-turn queues, `⏎` steers, both rows drawn
      (snapshot); `↑` on an empty box brings the newest queued line
      back with its pictures; a second `↑` the next; history only
      after.
- [x] All gates; Windows cross-check for sdk/core.
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

## Verified

2026-09-04, from the worktree root, `-j 2` throughout.

```
$ cargo fmt --all -- --check
fmt ok

$ cargo check --workspace --all-targets --locked -j 2
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 18.32s

$ cargo clippy --workspace --all-targets --locked -j 2 -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 23.77s

$ cargo test --workspace --locked -j 2 --no-fail-fast
60 test binaries, every one `test result: ok` (no `FAILED`, no `failures:`).
Among them: bingo-core 300 passed, bingo-surface-tui 941 passed,
bingo-surface-rpc 23 passed.

$ cargo test -p bingo --test pty -j 2
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ scripts/check_discipline.sh
dependency direction ok / kernel names no tool / cohesion ok / discipline ok
(warnings only: files over 700 non-test lines, and `session.rs` `fn handle`
at 72 lines — a dispatch match that already warned at 67 before this.)

$ scripts/budget.sh
dependencies (unique, normal): 334 (max  334)
relink isolation: touching the TUI recompiled 0 crates for core (must be 0)
budget ok    (warn: target/debug over its soft limit, as it was)

$ cargo deny check
advisories ok, bans ok, licenses ok, sources ok

$ cargo check -p bingo-sdk -p bingo-core --all-targets --locked -j 2 \
    --target x86_64-pc-windows-msvc
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.78s
```

The named tests behind the criteria:

- `bingo_core::host::tests::queue::a_held_line_waits_out_the_barrier_and_the_next_turn_takes_it`
  — a real session, a gate holding the turn open before its barrier, a
  `Wake` and a `Hold` line queued behind it: the second request carries
  the steer and not the held line, and the third turn carries the held
  line.
- `…::queue::a_queued_line_comes_back_out_to_the_surface_that_sent_it` —
  the input comes back, another surface's is `PERMISSION_DENIED`, an
  intent never queued and one already taken are `NOT_FOUND`, and the
  fold loses the row.
- `bingo_core::session::queue::tests` — the barrier stops at a held line
  as at a command; the next turn takes two held lines as one turn's
  inputs; `steerable` is false for both; `queued`/`take` find and remove
  one entry once.
- `bingo_surface_tui::keys::tests::tab_completes_then_queues_then_walks_the_cards`
  and `…::input::tests::tab_completes_before_it_queues_and_queues_only_while_a_turn_runs`
  — the precedence, in the table and at the keyboard.
- `…::input::tests::tab_mid_turn_sends_a_line_that_waits_and_enter_sends_one_that_steers`.
- `…::input::tests::up_on_an_empty_box_asks_for_the_newest_line_of_ours_that_is_queued`,
  `…::up_walks_past_another_surfaces_queued_line_into_the_history`,
  `…::up_with_an_empty_queue_recalls_the_prompt_history`.
- `…::run::tests::a_withdrawn_line_comes_back_to_the_composer_with_its_pictures`
  and `…::a_line_the_turn_took_first_leaves_the_box_empty_and_says_so`.
- `…::view::tests::a_line_that_will_not_steer_wears_the_tag_and_the_busy_box_says_why`,
  plus 16 screen snapshots re-read line by line: every diff is the busy
  placeholder or the `tab` help row, and nothing else moved.
- `bingo_surface_rpc` wire: `a_line_may_ask_on_the_wire_to_wait_for_the_turn_to_end`.

Not verified here: `cargo check -p bingo-surface-tui --target
x86_64-pc-windows-msvc` does not build on this machine — `aws-lc-sys`
wants a native Windows C toolchain, which is a pre-existing limit of the
box and not of this change; the TUI edits touch no process, path, signal
or clock, and CI's `windows` job is the backstop. `scripts/tui-smoke.sh`
was not run, as directed.
