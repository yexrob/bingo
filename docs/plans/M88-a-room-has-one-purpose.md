# M88 — A room has one purpose

## Goal

User, 2026-09-10, from session `ses_01M24RQNXJ52Q60V2D526EK9RC` (Nilbo): the
parent called `OpenRoom` twice with the same name, the second time with two
of the five members, and three explorers were off the room with nothing said
to any of them. `OpenRoom` on a standing name *is* a reseat, its description
says so, and there is no other verb — so "narrow the discussion" had exactly
one spelling, and it was the wrong one. The same room carried a five-member
evidence dump and a two-member design debate: 35 posts, 52 bounces, the
slowest writer (Fable xhigh, 7 kB drafts) bounced 28 times for 8 landed
posts, each bounce a full regeneration. After this milestone (ADR-0053):

- **a room is opened for one purpose**, said when it is opened and read by
  every member; a new phase is a new room, and `OpenRoom` refuses a name
  that stands;
- **who is in it changes by its own verbs** — `Seat`, `Unseat`, `CloseRoom`
  — each a post the room reads, and an unseat is told to the seat that left;
- **only the holder and the opener** change a roster or close a room;
- **a bounce is cheap**: the draft is the bounced call's own input, and
  `SendMessage { to, again: true }` posts it again without a regeneration.

## Bricks, in build order

1. `room.rs`: two new kinds beside `members` — `opened { purpose, by }` and
   `closed { at, by, why }` — read back by `opened_of` / `closed_of`; `Room`
   gains `purpose: Option<String>` and `closed: bool`; `roster.rs` folds
   both. Pure, fixture-tested first.
2. `seat.rs`: `seat` (the person's door, the reset lever) refuses a closed
   room and writes `opened` when it opens one; `open` (the agent's door)
   refuses a standing or closed name with what stands and which verb does
   what was meant; `join`, `leave`, `close` each derive the next roster from
   the standing one, publish it whole, and say so in the room by a post the
   caller signs (`post::say`). `leave` clears the seat's retuning and nudges
   the seat that left; `close` posts last, then publishes `closed`.
3. `door.rs`: the room a `room` argument names — under the caller, else
   beside it — and `may`: the caller is the holder or signs `opened.by`.
4. Tools: `tool.rs` becomes `tool/{open,join,leave,close}.rs`; `OpenRoom`
   gains a required `purpose`; `Seat { room, members?, listeners? }`,
   `Unseat { room, members }`, `CloseRoom { room, why? }`. Cards name the
   room and the seats. Manifest and discipline tool list gain the three.
5. `command.rs`: `/room close <name>` (`close` is reserved by `name::check`);
   the table gains a `purpose` column and a closed room's row says so.
6. `reader.rs`: the reading's header carries the purpose; a closed room is
   read to its end and no further; the protocol says a room has one purpose
   and a new phase is a new room.
7. `hook.rs` / `owed.rs` / `chase.rs`: a closed room owes nothing and is
   chased for nothing.
8. `bingo-agents`: `rooms.rs` holds the read-only contract (`bingo.rooms`,
   `cursor:`, `closed`) `serial.rs` and `message.rs` read as data; a post
   into a closed room is refused; `SendMessage` takes `text` or
   `again: true`, and `serial::draft` finds the latest bounced draft for that
   room in the caller's own journal before the cut. The bounce says how to
   post again.
9. `team.rs`: an `Entry` may carry `purpose`.

## Files

- `bingo-rooms/src/{room,roster,seat,door,command,reader,hook,owed,chase,team,name,lib,tests}.rs`,
  `bingo-rooms/src/tool/{mod,open,join,leave,close}.rs`.
- `bingo-agents/src/{rooms,serial,message}.rs`.
- `bingo/tests/cli/rooms.rs` (the convene script names a purpose; a
  standing name is refused end to end; `again` lands a bounced draft).
- `scripts/check_discipline.sh` tool names.
- `docs/adr/0053-the-room-s-verbs.md`, ADR-0021 status line, ADR README.

## Exit criteria

- [ ] `OpenRoom` on a standing name is refused, names the room's purpose and
      members, and opens nothing; the same for a closed name
- [ ] `Seat` seats at the head and the room reads `<by> seated scout`;
      `Unseat` publishes the roster without the name, the room reads it, the
      seat that left is nudged with who and why, its retuning is cleared
- [ ] `CloseRoom`: a last post, then `closed`; a member reads it to the end
      and then nothing; `SendMessage` to it is refused; `/room` says closed
- [ ] a sibling of a shared room may post and may not `Seat`/`Unseat`/close
- [ ] the reading header carries the purpose; `/room` lists it
- [ ] `SendMessage { to, again: true }` posts the latest bounced draft for
      that room and is judged by the same serial rule; `text` and `again`
      together, or `again` with no bounce to repeat, are refused as input
- [ ] black-box: the convene script still runs; a standing name refused;
      `again` lands after a bounce
- [ ] every gate green; Windows check for `bingo-rooms`, `bingo-agents`

## Non-goals

- No TUI change: the roster view reads `members` by kind and ignores the
  new kinds; showing a purpose there is a later milestone.
- No invite/accept handshake, no ACLs beyond holder-or-opener.
- A closed room is not deleted and its session is not ended: the journal
  is the record, and the `closed` frame is the whole of what closing means.
- `/room <name> [member…]` keeps the person's reset lever: a person
  outranks the protocol (ADR-0025 §4), and a restart reseats declared rooms
  through it.

## Risks

- Every `OpenRoom` caller must now say a purpose: the fake-provider scripts
  in `bingo/tests` are the only ones in the repo; a model that omits it is
  refused with the schema, which is the input error it can correct.
- `again` re-posts the draft of the *latest* bounce for that room; a model
  that bounced twice with different drafts gets the later one, which is the
  one it last decided to send.
- A closed room's seats keep their cursors; a reseat of the same name is
  refused, so nothing reads them again. Accepted: the journal is history.
