# M25 — The ear on every seat (ADR-0029)

## Goal

Seats gain ears: patience 0 (live — today's seat, the default) or ≥30 s
(patient — posts land `Hold`, a deadline nudge bounds the wait). The
roster declares initial ears in all three doors, `Listen` retunes your
own, and obligation pierces every ear.

## Bricks, in build order

1. **The ear brick, pure first** (`room.rs`, `roster.rs`) — the
   membership payload grows `listeners: [{name, patience_s?}]` beside
   `members`; a flat array from an old journal reads as all-live
   (back-compat pinned by fixture). One reader `ear_of(room, name) ->
   Ear{Live, Patient(Duration)}` applies the default (the chaser's
   300 s, one constant) and folds **EAR deltas**: a second extension
   kind carrying `{name, patience_s}`, applied over the last membership
   payload, cleared by the next one. Table-tested: declared ears,
   deltas, delta-after-reseat gone, unknown name ignored.
2. **Delivery reads the ear** (`post.rs`) — a live seat `Wake`, a
   patient one `Hold`; the author guard and exactly-once unchanged and
   re-pinned with mixed ears. `@name` pierces: a post that mentions a
   patient seat is delivered `Wake` to that seat (the fold's own
   matcher via the roster, NOT a resurrected `calls_on` — the check is
   "is this seat among the post's mentions", asked of `mentions::named`
   with the roster); `@all` pierces nothing. Pinned in the delivery
   table.
3. **The deadline** (`hook.rs` + a small `ear.rs` if it wants a home) —
   the hook watches `QueueChanged`: entries with `origin.surface ==
   "room"` held longer than the seat's patience arm one bounded timer
   per session (earliest deadline wins); firing re-checks the live
   snapshot and delivers one nudge (`principal: None`, worded like the
   chaser's) `Wake`. Chaser discipline throughout: timers die with the
   process, announce re-derives from `SessionState.queue`, overdue
   nudged once. **The standby-brief guard is pinned by test**: a held
   entry with surface `agent` never arms anything (ADR-0027 lives).
   Paused-clock tests drive the window; nothing waits wall-clock.
4. **`Listen`** (`listen.rs`, new tool) — `{room, patience_s}`: resolve
   the room (child-then-sibling), the caller's own seat by its title
   (`parent` for a holder-caller is NOT a case — a holder is not on its
   own roster's reach from inside; the tool is for members), refuse in
   words when not seated, when the room is not one, or for the dead
   band (0, 30). `patience_s: 0` = live. Appends the EAR delta;
   receipt says the ear it now wears. Manifest line, traits fail-closed
   like `OpenRoom`'s neighbours, `Listen` added to the discipline tool
   regex (the one `scripts/check_discipline.sh` line you may touch).
5. **The doors** — `/room` grammar `~name[:secs]` (bare `~name` =
   default), `OpenRoom.listeners` (string or `{name, patience_s}`),
   `team.json` `listeners` (same serde). Receipts and `/room` listing
   show a patient ear as `~name(300s)` or similar — derived from the
   one reader. Words in `OpenRoom` + `/room` + `Listen` teach the dial
   in a sentence each.
6. **Black-box** (`tests/cli/rooms.rs`) — a `~parent` room: posts land
   without waking the root (journal: no turn, entries held), the
   person's next message absorbs them in order (the M23 scenario
   returns, now as the patient ear's); `@parent` still wakes it at
   once and opens the debt; `Listen` from a member retunes and the
   room's journal shows the EAR delta. The deadline itself stays on
   the paused clock in-crate (a 30 s wall wait is not a cli test).

## Files

`crates/bingo-rooms/src/{room,roster,post,seat,hook,tool,command,lib}.rs`,
new `{listen,ear}.rs` as needed, crate tests, `crates/bingo/tests/cli/rooms.rs`,
the tool-regex line of `scripts/check_discipline.sh`. No new
dependencies; budget unchanged; no kernel or sdk change.

## Exit criteria

- [ ] ears fold from roster + EAR deltas, one reader, one constant;
      old flat rosters read all-live (fixture)
- [ ] delivery: live Wake, patient Hold, `@name` pierces, `@all` does
      not, author guard + exactly-once with mixed ears
- [ ] deadline: paused-clock proof of one nudge per patience, backlog
      absorbed first, standby brief never arms it, announce re-derives
      and nudges an overdue backlog once
- [ ] `Listen`: own seat only, dead band refused in words, EAR delta
      journaled, reseat clears it
- [ ] three doors declare ears; receipts and `/room` show them
- [ ] black-box scenarios green; every gate green (fmt, check, clippy,
      test, discipline, budget unchanged, deny)

## Non-goals

Join/leave verbs; sliding windows or stored inboxes; changes to
serial, standby delivery, the person's exemption, or off-roster
holders; per-message priorities beyond the mention.

## Risks

R-adr27 — the deadline discriminator is the whole safety of standby:
surface `room` only, pinned, or zero-cost seating dies. R-fold — EAR
deltas must be folded by the roster's ONE reader; a second reader is
the debt ADR-0011 forbids. R-pierce — the mention-pierce check must
reuse `mentions::named` against the real roster, not a one-name
roster (the `calls_on` mistake M24 deleted; do not rebuild it).
R-syntax — `~name:secs` collides with nothing today but the parser
must refuse `~parent:15` with the dead-band words, not clamp.
