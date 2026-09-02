# ADR-0025 — The serial room

Status: accepted · 2026-08-31 · Plan: M20 · amended 2026-09-03 (M37)

## Context

Two members of a room can compose answers from stale state — both speaking
after post N while post N+1 already landed — and it has happened: duplicate
and conflicting proposals written at the same moment. The old tree ran
rooms in two modes, `serial` (a post must have read the head; a stale one
bounces, carrying what it missed) and `free`. In this architecture the
room's journal is already one ordered stream; what is missing is only the
same discipline on the writer: a post must follow everything its author
could have seen.

The facts are in place. Every post fans out at landing, `Delivery::Wake`,
to every member but its author, and is absorbed into the member's own
journal carrying `origin { conversation: "#name", principal: Some(author) }`
(`post.rs`); a nudge carries `principal: None` (`chase.rs`), so posts and
nudges are distinguishable in sdk vocabulary alone. And journals are
append-only: a landed post cannot be retracted, so any refusal must happen
before landing.

## Decision

1. **Every room is serial; there is no mode.** A parallel dump costs at
   most one bounce and a same-turn retry — accepted, and the retry is a
   genuine re-decision point ("half of what I meant to say was just said").
2. **The checkpoint is `SendMessage`'s room arm, before `deliver`.** Two
   derived ledgers, no stored watermark: the room's journal (posts by
   others) against what the caller has *seen* — reading only the caller's
   journal **before the assistant item that issued this call** (`cx.item`
   is the cut; what was absorbed at this turn's barriers after the model
   spoke was not seen by it). Behind → bounce; even → land.
   The room's ledger starts at the caller's own `created_at`: a post that
   landed before the session existed was fanned out to nobody, so no
   author can be behind on it, and a member spawned into a running room
   is level with it rather than behind its whole history.
   (Amended 2026-09-03, ADR-0034 §5: a post is copied into no member's
   journal any more, so "seen" is the caller's cursor into the room —
   still derived, still no stored watermark of its own, and the quoted
   bounce still counts beside it.)
3. **Seen = absorbed or quoted.** The bounce is a worded tool error that
   quotes the missed posts, and a journaled bounce counts toward "seen" on
   the next attempt: seen(room) = max(posts absorbed before the cut,
   posts quoted by a bounce journaled before the cut). So a bounce always
   unlocks the very next attempt — even when a fan-out was lost (a frame
   nobody observed is never re-delivered), the bounce itself is the
   repair, arriving through the tool-result lane instead of the input
   lane. In the normal case the fan-out copies absorb at the same barrier
   and the quote merely stands beside them, clearly labelled.
4. **A person is never bounced.** The check lives in the tool; whatever
   posts without the tool — a person's own composer — is not checked. A
   person watches the room live and outranks the protocol.
5. **Exactly-once fan-out is pinned by a test**: one delivery per post per
   member. The count comparison of §2 leans on it.

## Consequences

- Mentions are untouched: a bounced post never landed, so it neither
  answers a debt nor opens one, and the chaser never sees it.
- Cost: one extra tool round-trip per stale post, worst case; at
  single-digit member counts this is noise, and it buys the property that
  every landed post was written in full knowledge of the room's head.
- The rule rides entirely on `Origin` fields, `created_at` and journal
  order — sdk vocabulary; `bingo-agents` still imports nothing of
  `bingo-rooms`, and knows nothing of who a room's members are.
- The session a room hangs under is never fanned out to (`post.rs`: a room
  reaches into the tree, not up out of it), so its own model posts blind
  and is bounced once whenever a member has spoken since. That bounce is
  the only reading of the room it gets, which is the repair of §3 doing
  its work rather than an exception to it. (Narrowed by ADR-0028: a
  roster that names `parent` seats the holder — its posts are then
  delivered to it and absorbed like any member's, and only an
  off-roster holder still posts blind.)
- Restart-safe by construction: both ledgers are re-derived from journals;
  process death loses timers, never the discipline.
