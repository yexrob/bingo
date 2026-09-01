# ADR-0029 — The ear on every seat

Status: accepted · 2026-09-01 · Plan: M25

## Context

M24 made every rostered seat wake alike — right for the working member
(the relay lives on plain posts waking it), and it bills the convener
for chatter: a supervisor wants to be *informed*, not *interrupted*.
The journey found two stances toward a room, not two identities;
pushing further, the stances turned out to be one dial. The old tree
solved the same tension with a stored inbox and two timers (QUIET 2 s,
DEADLINE 15 s); we can have the deadline without the inbox, because a
turn boundary already coalesces and a queue already holds.

## Decision

1. **Every seat has an ear: a patience, in seconds.** Patience 0 is a
   live ear — every post `Wake`s it; today's seat, and the default. A
   patience of 30 s or more is a patient ear — posts land `Hold`, read
   whole at the seat's next turn, whoever opens it. The band (0, 30) is
   refused in words: under thirty seconds of patience, take the live
   seat you are describing. There is no seat-kind enum; working and
   listening are readings of one number.
2. **The roster declares the initial ear.** `/room design scout
   ~parent` seats a patient ear at the default; `~parent:120` a custom
   one; `OpenRoom { listeners: [...] }` (a name, or `{name,
   patience_s}`) and `team.json`'s `listeners` say the same. A bare
   name is live. The membership payload becomes `{members, listeners}`;
   a flat array read from an old journal is all-live. `patience_s`
   stores what was asked — absent means default, applied by one reader,
   so the constant lives in one place: the chaser's own 300 s.
3. **The patience deadline.** Held mail whose origin surface is
   `room` — only that; a standby brief (surface `agent`) must never
   trip this, or ADR-0027's zero-cost seat dies — older than the
   seat's patience wakes the seat once, by a nudge (`principal: None`:
   not a post, no debt, no serial count). The woken turn absorbs the
   backlog first, queue order. Timers keep the chaser's discipline
   (ADR-0022 §3): bounded, die with the process, re-derived from the
   session snapshot's queue on announce, an overdue backlog nudged
   once.
4. **`Listen { room, patience_s }` retunes the caller's own ear** — an
   EAR delta appended to the room's journal, folded over the last
   membership payload: no read-modify-write, journal order settles
   every race, and a reseat re-declares the roster whole and clears
   the deltas (replace-whole, the standing rule). A caller not on the
   roster is refused; joining and leaving are still not verbs — the
   formation is the seater's, the stance is the seat's.
5. **Obligation pierces every ear.** `@name` delivers `Wake` to that
   seat whatever its patience, and opens the ordinary debt; nudges
   chase as ever, so demotion dodges nothing. `@all` pierces nothing —
   its debt is the room's, not any seat's.

## Consequences

- The storm bound returns, per seat and self-chosen: a live ear bills
  per post (its job), a patient one at most one turn per patience,
  floored at 30 s. Retuning to live is a deliberate, journaled act —
  the R-shadow precedent — and the reseat is the reset lever.
- The serial rule needs nothing: a patient seat that posts while
  behind is bounced with the missed posts quoted — the repair lane
  already covers the ear. The pending area (ADR-0028) already hides
  held room mail; the person can always watch the room live.
- The roster's one reader grows one arm (the EAR fold); the mention
  fold counts every seat, patient ones included.
- A standby member seated with a patient ear on an active room will be
  started by the deadline — that is the ear its seater asked for, and
  the words say so.
- A listener is at most one patience behind the room, and an unanswered
  mention is chased on the same 300 s clock: one constant, two duties.

Refs: ADR-0021, ADR-0022, ADR-0025, ADR-0027, ADR-0028.
Non-goals: join/leave verbs, sliding quiet windows, stored inboxes or
digests, per-room notification config beyond the seat's own number.
