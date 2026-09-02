# ADR-0028 — The holder on the roster

Status: accepted · 2026-09-01 · Plan: M23 · amended 2026-09-01 (M24), 2026-09-03 (M37)

## Context

A room deliberately never fans up: the session it hangs under hears
nothing of it (`post.rs`), posts blind, and is bounced once as its only
reading (ADR-0025). Live use shows what that costs: the convener of a
room is deaf to it — results reach it only if a member remembers to DM
`parent`, and the person's root model cannot report on work it never
heard. The user asked for main in the room.

The old bingo judged both extremes for us (survey
`docs/design/survey/collaboration-mechanisms.md`). Its user and main
were ordinary roster members (D95) — and per-post wake of main was a
production failure, recorded as the epitaph of `app/mail.rs`: "three
agents talking in a room for a minute bought the user three digests of
a conversation that had not finished happening." Its fix was a digest
(2 s quiet / 15 s deadline) with an urgent bypass for mail that called
on main by name. Our kernel already holds that distinction as plain
vocabulary: `Delivery::Hold` and `Delivery::Wake`.

## Decision

1. **`parent` may be named on a room's roster.** It means the session
   the room hangs under — the name its members already call it, and the
   name a root holder's posts already sign. No other holder address is
   introduced.
2. **A post fans to a rostered holder as `Wake`, like to any member.**
   A busy holder reads it at its next barrier — a steer, not a
   conversation — and an idle one opens a turn that drains whatever
   queued behind it: the kernel's own batching, no timers. (Amended
   2026-09-01: the first cut delivered `Hold` and woke only on
   `@parent` — a digest that priced liveness away, and a mention read
   for routing, the thing ADR-0022 refuses. The noise the old tree's
   debounce fought is already carried by M22's quiet notices; what
   remains is tokens and context, and the roster itself is that dial —
   a room that should not spend the holder's attention leaves `parent`
   off it.)
   (Amended 2026-09-03, ADR-0034 §7: nothing fans to a holder at all —
   a rostered `parent` is a cursor and an ear like any other seat, and
   reads the room at the head of its own turn; its transcript shows no
   post.)
3. **`@parent` opens an ordinary mention debt** (ADR-0022) against the
   seat, closed by the seat's next post — obligation only, never a
   delivery mode. One delivery per post; the exactly-once pin stands.
4. **Explicit, never default.** A roster without `parent` is today's
   room, byte-identical: the holder stays blind, `@parent` opens
   nothing. The old tree's own rule — create seats only the caller,
   everyone else named — and the holder's context spend should be a
   choice, not a tax.
5. **A holder never hears its own post.** The guard is the seat's
   signing name: the fan-out already excludes the author, and the
   holder's seat is one author — the person at the composer and the
   holder's model sign alike, share a conversation already, and neither
   is delivered the other's posts.

## Consequences

- ADR-0025's blind-holder consequence narrows to holders off the
  roster; for a rostered one, absorbed posts count toward *seen* at the
  cut and its posts land like any member's. The serial module changes
  not at all — the rule already reads only `Origin` and journal order.
- The chaser learns one address: a debt owed by `parent` is nudged at
  the room's parent session, `Wake` like every nudge. The `owed` card
  and `/room` show the seat by the name everyone uses for it.
- A queued subsystem input is a steer in flight, not a pending message:
  the composer's pending area renders only what the person themselves
  queued, with M22's quiet set as the boundary and an unknown surface
  failing to the loud, person side. Nothing lingers behind that rule —
  a woken seat drains its queue.
- The tools' words teach it where the pattern lives: `SpawnAgent`'s
  room shape names `parent` among the members when the caller wants to
  hear the room itself; `/room` and `OpenRoom` say what a rostered
  holder gets.
- A member deliberately titled with its holder's signing name would
  shadow the holder's authorship; sibling naming already forbids
  duplicates beside the room, the residue is noted in the plan and
  accepted at this scale.
- An *agent* holder's `@parent` debt does not close itself: the fold
  matches debts by roster name, and an agent holder signs its title,
  not `parent`. It is still delivered, woken and nudged; the debt just
  outlives its answer in `owed`. Closing it would mean threading the
  holder's title into the pure fold — machinery deferred until an
  agent-held room actually leans on mention debts (found in M23
  review, recorded in the plan's Carried).

Refs: ADR-0011 §1, ADR-0022, ADR-0025, ADR-0027; the collaboration
survey (D95, D98, the mail.rs digest).
Non-goals: rostering the holder by default, digest timers or any
stored inbox, any change to the person's exemption (ADR-0025 §4).
