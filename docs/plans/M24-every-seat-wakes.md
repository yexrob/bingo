# M24 — Every seat wakes, and the queue shows only yours (ADR-0028 as amended)

## Goal

A room post is a steer, never a pending message: every rostered seat —
the holder included — is woken alike (busy reads it at the next
barrier, idle opens a turn that drains what queued), the mention goes
back to being obligation only, and the composer's pending area shows
nothing but what the person themselves queued.

## Bricks, in build order

1. **One delivery** (`bingo-rooms/src/post.rs`) — `delivered` loses
   the text/mode axis: every member but the author `Wake`, a rostered
   holder exactly the same (the signing-name author guard stays, its
   R-shadow comment with it). `mentions::calls_on` loses its one
   caller and is deleted; the fold's own matcher is untouched. The
   nine-row table shrinks to what is left and says so.
2. **The words** — `/room`'s listing and `OpenRoom`'s description stop
   promising quiet ("costing you no turn"): a rostered holder hears
   every post as it lands, mid-run or as a turn of its own; `@parent`
   is owed an answer. `SpawnAgent`'s clause ("and `parent` among them
   when you want to hear the room yourself") is already true and
   stands. Description pins updated.
3. **The pending area** (`bingo-surface-tui/src/view.rs`) —
   `activity()` renders only queue entries whose origin is a person's:
   the M22 quiet set (`transcript.rs::QUIET_SURFACES`, exposed through
   its existing `quiet(origin)` shape, never a second list) marks the
   boundary; a subsystem entry renders as nothing — it is a steer in
   flight, visible in the transcript as a quiet notice when absorbed —
   and an unknown surface fails to the loud, person side. `TestBackend`
   proves person-only, subsystem-only (nothing drawn), mixed, and the
   fail-loud default.
4. **Black-box reshaped** (`tests/cli/rooms.rs`) — the quiet-holder
   scenario becomes the live-holder scenario: a member's post opens the
   idle holder's turn with the post as its first item, a burst drains
   as one queue rather than a turn per post where the run can pin it;
   the `@parent` debt still opens and the seat's post still closes it;
   the deaf-without-`parent` regression stands byte-identical.

## Files

`crates/bingo-rooms/src/{post,mentions,tool,command}.rs` and their
tests; `crates/bingo-surface-tui/src/{view,transcript}.rs` and their
tests; `crates/bingo/tests/cli/rooms.rs`. No new dependencies; budget
unchanged. One worker; no kernel or sdk change.

## Exit criteria

- [x] a rostered holder is woken like any member; no `Hold` remains in
      `bingo-rooms`; `calls_on` is gone
- [x] the author guard still keeps every seat from hearing itself;
      exactly-once fan-out pinned with the holder counted
- [x] the composer's pending area draws person entries only; a
      subsystem entry draws nothing; unknown surfaces stay person-loud
      — all four proven on `TestBackend`
- [x] the live-holder black-box: a post opens the idle holder's turn,
      `@parent`'s debt opens and closes, a parentless room unchanged
- [x] the words stop promising quiet; pins updated
- [x] every gate green (fmt, check, clippy, test, discipline, budget
      unchanged, deny)

## Non-goals

A second inbox or any digest timer; changes to the person's exemption
(ADR-0025 §4), to `serial.rs`, to standby delivery (a held brief is
`bingo-agents`' and stays `Hold`), or to off-roster holders (still
deaf, still posting blind); rendering changes beyond the pending area.

## Risks

R-noise — a chatty room now spends the holder's tokens and context;
accepted by decision (the roster is the dial) and said in the words.
R-race — "one turn per burst" is scheduler-shaped: assert the
never-lost invariant (every post lands in the holder's journal, in
order) and pin turn *behaviour* only where the script makes it
deterministic, the M20 lesson. R-window — between Wake and the
barrier a subsystem entry sits in the queue; brick 3 is what keeps
that window invisible, so land it with brick 1 in one commit.

## Verified (2026-09-01)

- Worker P merged `92c8660` (`15dbf4d` rooms, `564cf20` tui):
  `delivered` lost the text axis (seven rows over roster x holder x
  author); `calls_on` deleted; `rg Hold` and `rg calls_on` over
  `bingo-rooms/src` both empty; the pending area filters through
  `transcript::quiet` exposed pub(crate) — one list, two readers — and
  the filter was proven by removing it.
- Deviations accepted on review: `A_BURST` puts the burst in one model
  response so the fake provider's run-wide cursor cannot race the
  holder's woken turn (the deaf test keeps `TWO_POSTS` untouched);
  `until_the_room_settles` waits for root-and-scout quiescence for the
  same reason; the burst's turn count is deliberately unpinned (the
  scheduler's business) and the never-lost invariant pinned instead.
- Gates on the worker's tree, which is byte-identical to this merge
  (base `c11b6ea`, quiet machine, load 5.6): fmt / check / clippy OK;
  bingo-rooms 96, bingo-surface-tui 474; workspace 69 targets, 0
  failures; discipline / budget (302 unchanged) / deny OK; the PTY
  smoke's 14 scenes green (terminal bytes changed: the steer rows left
  the pending band).

## Carried

- The relay black-box (`peers.rs::one_kickoff_post_runs_a_relay_…`)
  now flakes on a QUIET machine — 2/6 red at base `c11b6ea` with P's
  work absent, two variants (the parent posts the members' counts
  itself; the relay stops at the kickoff) — both shapes of the fake
  provider's run-wide cursor being taken by the wrong waker. M22's
  Carried said "widen only if it ever flakes quiet"; it has. Filed to
  its own worker: fix the test's determinism (or teach provider-fake
  addressed responses), never the assertion.
