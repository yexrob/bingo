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

The facts are in place. Every post is written once into the room's
journal carrying `origin { conversation: "#name", principal: Some(author) }`
(`post.rs`), and each seat keeps a cursor there saying how far it has read
(ADR-0034 §2); a nudge carries `principal: None` (`chase.rs`), so posts and
nudges are distinguishable in sdk vocabulary alone. And journals are
append-only: a landed post cannot be retracted, so any refusal must happen
before landing.

## Decision

1. **Every room is serial; there is no mode.** A parallel dump costs at
   most one bounce and a same-turn retry — accepted, and the retry is a
   genuine re-decision point ("half of what I meant to say was just said").
2. **The checkpoint is `SendMessage`'s room arm, before `deliver`.** Two
   derived ledgers, no watermark of the rule's own: the room's journal
   (posts by others) against what the caller has *seen* — its cursor into
   the room (ADR-0034 §2), read **before the assistant item that issued
   this call** (`cx.item` is the cut; what the cursor took at this turn's
   barriers after the model spoke was not seen by it). Behind → bounce;
   even → land.
   The room's ledger starts at the caller's own `created_at`: a post that
   landed before the session existed reached nobody, so no author can be
   behind on it, and a member spawned into a running room is level with
   it rather than behind its whole history.
   *(Amended 2026-09-03, ADR-0034 §5: "seen" was counted from the posts
   copied into the caller's own journal; a post is copied nowhere now, so
   it is counted from the caller's cursor.)*
3. **Seen = read or quoted.** The bounce is a worded tool error that
   quotes the missed posts, and a journaled bounce counts toward "seen" on
   the next attempt: seen(room) = max(posts before the seat's cursor,
   posts quoted by a bounce journaled before the cut)
   (`bingo-agents/src/serial.rs`). So a bounce always unlocks the very
   next attempt: the cursor moves only when the seat reads the room at
   the head of a turn (ADR-0034 §4), and mid-turn the quote is the
   repair, arriving through the tool-result lane.
   *(Amended 2026-09-07: was "absorbed or quoted", counting fan-out
   copies absorbed into the member's journal; ADR-0034 §2 replaced the
   copies with one cursor per seat.)*
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
  roster that names `parent` seats the holder — it then reads the room
  by a cursor like any member (ADR-0034 §7), and only an off-roster
  holder still posts blind.)
- Restart-safe by construction: both ledgers are re-derived from journals;
  process death loses timers, never the discipline.
