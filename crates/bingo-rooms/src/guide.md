# Rooms

A room is a conversation every member reads. It is a session nobody answers,
titled `#name`, hanging under the session that opened it; a post is written
once into its journal and copied nowhere.

**A room is read, not delivered.** At the head of each of your turns you are
handed everything each of your rooms has said since you last read it, under
`[#design — the purpose, since you last read]`, one line per post under the
name that wrote it. Nothing of a room reaches you between turns; what you have
read is exactly where your cursor stands.

Post into a room with `SendMessage` to `#name`.

## Opening one

`OpenRoom` takes `name` (one word, no slashes — `close` is not one, it is the
word that ends a room), `purpose`, `members`, `listeners` and `shared`.

`purpose` is required and is the whole discipline: every member reads it above
every reading, and **when the work moves on you open another room rather than
reseating this one**. A name that already stands is refused, and the refusal
names the verb that does what was meant.

By default the room hangs under you, so the agents you started read it; with
`shared: true` it hangs under the agent that started you, so your peers do.
That placement is the whole of who will ever hear it.

Members are names, not sessions: a name nobody holds yet is kept on the roster
and skipped until somebody does, and names match whatever their case. Name
`parent` among the members to read the room yourself and to owe an answer to a
post that says `@parent`.

## The roster of a room that stands

- `Seat` — the roster it has plus the names you give, published whole. A new
  seat starts reading at the room's head, so what was said before it is not a
  backlog it owes. A name already seated keeps its place.
- `Unseat` — the roster without them. Each is told once who unseated it from
  where; a name the room does not seat is refused rather than passed over.
- `CloseRoom` — everyone reads one last line, with your `why` if you give one,
  and after that the room takes no post, wakes nobody, owes nothing and is
  never reopened under that name. Nothing is deleted: what was said stays
  where it was said.

Only the session a room hangs under and whoever opened it may seat, unseat or
close. A peer of a shared room reads it and posts in it; to convene work of
its own it opens a room of its own.

Each of these is a post the room reads, signed by the caller, so a change is
something every member reads rather than something that happened to them.

## Ears

A seat's ear is one number, `patience_s`, declared on the roster
(`{"name": "scout", "patience_s": 0}`) and retuned by its owner with `Listen`.

- **Patient** (the default, 300 seconds): posts wait for your next turn, and
  you are woken once when the room has stood unread that long.
- **Live** (`0`): every post wakes you as it lands.
- Between 1 and 29 seconds is refused in words rather than rounded — it
  describes a live ear the long way round.

`Listen` changes your own seat and nobody else's: who is in a room is the
seater's to say.

## What a mention owes

`@name` calls on that member whatever its ear, and opens a debt its next post
in the room closes — speaking is the answer, and what was said is not judged.
`@all` calls on every member but the one who wrote it, and any other member's
post closes it.

Being called on is owed an answer: post it back to the room so whoever is next
can carry it on, and say `@name` when it falls to someone in particular. When
what a post names is not yours, end your turn without posting rather than
answering for someone else.

A debt nobody answers is chased: after 300 seconds the member is nudged, at
most three times, and after that the debt simply stands where a person can see
it — the `owed` column of `/room`, and a card on the room's parent. A nudge is
not a post: it says there is something to read, opens no debt of its own, and
counts as nothing read.

## Every room is serial

A post must follow everything its author could have seen. A post written
behind the room's head does not land: it comes back as an error quoting what
was missed, in order. That bounce itself counts as having seen them, so the
next attempt lands — `SendMessage` with `again: true` posts the same draft
again without rewriting it. Posts that landed before your session existed are
not counted against you.

A person is never bounced: a person watches the room live and outranks the
protocol.

## `/room`

`/room` lists the rooms under this session: `room`, `purpose`, `members`,
`owed`. `/room design reviewer scout` opens `#design` under it — or resets who
is in the one that stands, which is the person's lever and no agent's — with
`name:120` for a seat's patience and `name:0` for a live one. `/room close
design` ends a room of the person's own.

## `.bingo/team.json`

A project's `rooms` declares rooms that are seated when the person's own
session opens: `name`, `purpose`, `members` and `listeners`, read from the
nearest such file at or above the working directory.
