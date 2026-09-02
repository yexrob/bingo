# M37 — The room is read

## Goal

ADR-0034 built: a post exists once, in the room's journal; every seat
keeps a cursor; a wake is an empty nudge; reading happens at the head of
a turn through a contributor; the default ear is patient; a member's
transcript — the parent's included — shows none of the room. Plus one
surface change the user asked for at the same time: the session list
(M36) is **one column** again, grouped under labels — `Agents` first,
then `Rooms`, Slack's sidebar — not two columns side by side.

## Bricks, in build order

**Rooms (`bingo-rooms`, `bingo-agents` words, tests in `bingo`)**

1. **The cursor.** Pure: `Cursor { room, seq }`; extension kind
   `cursor:<room>` on the member's session; `cursors_of(state) ->
   BTreeMap<room, Seq>`; fixture test for the persisted shape (a
   contract). Seating writes the initial cursor at the room's head.
2. **The fan-out copies nothing.** `post.rs`: append to the room, then
   for each seat `wake_or_not(seat, mentioned, ear, cursor, head)` — a
   pure decision returning `Wake | Leave`; a `Wake` is the empty nudge
   (ADR-0029 §3's), routed through the existing nudge path. The
   patience timer is re-derived from cursor-vs-head instead of held
   mail. Tests: named → woken whatever the ear; live ear → woken; patient
   at the head → left; patient behind + elapsed → woken once.
3. **The reader.** A `ContextContributor` (the `bingo-rooms` plugin
   already contributes? check; else register one via the sdk) that, at
   turn start, for every room the session sits in, reads the room's
   journal after the cursor (through `HostHandle::events_since` /
   `history` on the room session — find the door `mentions` already
   uses), emits one `ContextPiece::User` labelled with the room, and
   advances the cursor by publishing the extension. Order: briefing
   first (ADR-0027), then rooms. Tests: a woken member's first request
   carries the unread posts and nothing older; the cursor moved.
4. **The serial rule reads the cursor** (`serial.rs`): the bounce says
   what it missed from cursor to head; the count of *seen* is the
   cursor. Re-aim every ADR-0025 test; the black-box relay in
   `crates/bingo/tests/cli/peers.rs` must pass unchanged in wording.
5. **The default ear is patient.** `ear.rs`, `room.rs`: bare name →
   `patience_s: 300`; `name:0` → live; `OpenRoom` / `/room` /
   `team.json` docs and the `SpawnAgent` room-shape text say it; the
   sub-agent NOTE says "a room wakes you when you are named, or when
   your patience runs out with something unread".
6. **Amend in place**: ADR-0025 §2, ADR-0028 §2, ADR-0029 §1 each get
   one sentence pointing at ADR-0034.

**Surface (`bingo-surface-tui`)**

7. **One column, two labels.** `roster.rs`: rows are `Agents` (dim
   label), the sessions that answer a model, then `Rooms` (dim label),
   the rooms; one `window::around` over the whole; `←`/`→` retire from
   the list; a member's row gains `· 3 unread` from its cursor when the
   room is behind. Snapshots re-aimed; the M36 §10 line gets a dated
   follow-up (the user chose one column after seeing two).

## Files

`bingo-rooms/src/{cursor (new),post,serial,ear,room,contributor (new),
hook}.rs`, `bingo-agents/src/{spawn,message}.rs` words, `bingo/tests/
cli/{peers,rooms,mentions}.rs`; `bingo-surface-tui/src/{roster,seats,
input,pointer,keys}.rs` + snapshots; `docs/adr/{0025,0028,0029}.md`;
`docs/design/tui.md`.

## Exit criteria

- [ ] A member's journal after ten room posts holds no `User { origin:
  room }` item; the same member's next request carries the ten posts
  under one `[#design, since you last read]` label.
- [ ] Four patient members and one poster: one turn each per patience,
  none before; an `@` wakes at once.
- [ ] The relay black-box (`peers.rs`) passes with its assertions
  unchanged; the bounce still says what was missed.
- [ ] A resumed member (`--continue`) reads only what its cursor left.
- [ ] `roster_80x24`: `Agents` / `Rooms` labels, one column, cursor
  visible past the room's end; `unread` on a lagging member's row.
- [ ] Every gate in AGENTS.md; no new dependency.

## Non-goals

Budgets or freezes (refused by the user). A digest timer. Hiding room
posts in a surface — there is nothing to hide.

## Risks

- The contributor reads another session's journal; the door must be
  the host's (`history`/`events_since`), never the store — check the
  room-parent attachment already used by `mentions`.
- `bingo-experience` recall no longer sees copied posts; recorded.
- Old journals with copied posts: replay is untouched; the cursor line
  at seating keeps a resumed member from re-reading history.
