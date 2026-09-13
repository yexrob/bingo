# ADR-0053 — A room has one purpose, and four verbs

Status: accepted · 2026-09-10 · Plan: M88

## Context

A room is a name and a roster (ADR-0021), and `OpenRoom` on a standing name
replaces the roster whole — the tool's own description says so, and the
person's `/room` does the same. So an agent that wanted a smaller
discussion had one verb for it, and it was "reopen with fewer names": three
seats were off the room and not one of them was told (M88, the Nilbo
session). The room carried two jobs — an evidence dump by three explorers
and a design debate by two — and every post of the one bounced the drafts
of the other (ADR-0025): 52 bounces on 35 posts, the slowest writer starved,
each bounce a regeneration of a multi-kilobyte draft. Nothing in the record
says what a room is *for*, so nothing told the model that the second job
was a second room.

## Decision

1. **A room is opened for one purpose.** `OpenRoom { name, purpose,
   members?, listeners?, shared? }`; `purpose` is required. It is published
   into the room's journal as the kind `opened` — `{ "purpose", "by" }`,
   `by` the opener's signing name — beside `members` (ADR-0011 §2). A
   member reads it in the header of every reading, `[#design — <purpose>,
   since you last read]`; `/room` lists it; the protocol says a new phase
   is a new room. A person's `/room` and a `team.json` entry may give none.
2. **A standing name is not reopened by an agent.** `OpenRoom` on a name
   that stands is refused with its purpose and roster and the verb that
   does what was meant. `/room <name> [member…]` keeps the person's reset
   lever: a person outranks the protocol (ADR-0025 §4), and a restart
   reseats declared rooms through it (ADR-0034 §8).
3. **Membership moves by its own verbs.** `Seat { room, members?,
   listeners? }` and `Unseat { room, members }` derive the next roster from
   the standing one and publish it whole — the roster stays one frame — and
   say so in the room by a post the caller signs (`parent seated scout`,
   `parent unseated scout`). A seat joins at the head (ADR-0034 §2). A seat
   that leaves has its retuning cleared and is nudged, once, with who
   unseated it from which room: nothing in a tree is taken away in silence.
4. **A room closes.** `CloseRoom { room, why? }` posts a last line the
   caller signs, then publishes `closed` — `{ "at", "by", "why" }`. A closed
   room is read by each seat to its end and then never; takes no post
   (`SendMessage` refuses, reading `closed` as data); owes nothing and is
   chased for nothing; is listed by `/room` as closed; and its name is not
   reopened by any door. The session is not ended or deleted: the journal
   is the record. `/room close <name>` is the person's spelling, so `close`
   is not a room name.
5. **The holder and the opener are the only hands on a roster.** `Seat`,
   `Unseat` and `CloseRoom` are refused unless the caller is the session
   the room hangs under or signs `opened.by`. A peer of a shared room posts
   and reads; it does not reseat what it did not open.
6. **A bounce is cheap.** The draft a bounce hands back is already in the
   caller's journal as the bounced call's own input, so nothing keeps a
   copy. `SendMessage { to: "#room", again: true }` posts the latest bounced
   draft for that room, found before the cut (ADR-0025 §2), under the same
   serial check; `text` beside `again`, or `again` with nothing to repeat,
   is an input error. The bounce says so: "Post it again with `again: true`
   if it still applies."

## Consequences

- The narrowing that cost three silent evictions is now `CloseRoom` on the
  first room and `OpenRoom` for the second, each a post everybody reads.
- Two jobs in two rooms do not bounce each other; a bounce that still
  happens costs one short call, not a regeneration, so the window in which
  a slow writer can be bounced again shrinks to seconds.
- `bingo-agents` still imports nothing of `bingo-rooms`: it reads one more
  kind by name (`closed`), as it reads `cursor:` (ADR-0025 §2).
- The TUI reads `members` by kind and is untouched by the new kinds.
- Every `OpenRoom` caller says a purpose; a model that omits it is refused
  with the schema, which is the input error it corrects.

## Supersedes

ADR-0021 §3's "same idempotent reset of a standing room" for the agent's
door (the person's keeps it); ADR-0021's non-goal "invite/kick verbs".
