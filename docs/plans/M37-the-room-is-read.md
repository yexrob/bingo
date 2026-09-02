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

- [x] A member's journal after ten room posts holds no `User { origin:
  room }` item; the same member's next request carries the ten posts
  under one `[#design, since you last read]` label.
- [x] Four patient members and one poster: one turn each per patience,
  none before; an `@` wakes at once.
- [x] The relay black-box (`peers.rs`) passes with its assertions
  unchanged; the bounce still says what was missed. †
- [x] A resumed member (`--continue`) reads only what its cursor left.
- [x] `roster_80x24`: `Agents` / `Rooms` labels, one column, cursor
  visible past the room's end; `unread` on a lagging member's row.
- [x] Every gate in AGENTS.md; no new dependency. (Bricks 1–6; brick 7
  and its `roster_80x24` criterion belong to the surface worker.)

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

## Verified — bricks 1–6 (rooms), 2026-09-03

Gates, in the worktree: `fmt --check` clean; `clippy --workspace
--all-targets -D warnings` no diagnostic; `test --workspace` every
target `ok, 0 failed` (`bingo-rooms` 156, `bingo-agents` 136, `bingo
--test cli` 138, `--test rooms` 2); `check_discipline.sh` → direction /
cohesion / discipline ok; `budget.sh` → `budget ok`, `dependencies
(unique, normal): 302 (max 302)` — nothing added. The CLI suite was then
run eight times over to shake out timing: clean.

Criteria: no copy, one label — `ten_posts_are_read_as_one_piece_under_
one_label`, `peers::a_post_reaches_each_member_exactly_once`, and
`tests/rooms.rs`'s JSON-RPC one, each asserting the reading is there and
the copy is not. Storm bound — `four_patient_members_and_one_poster_
cost_one_wake_each_per_patience` (nothing at 119 s, four wakes at
120 s, none in the next 600 s, `@alpha` at once). Resumed —
`a_resumed_member_reads_only_what_its_cursor_left`.

† The relay's own assertions are unchanged — one post per round to
`count 3`, the parent dispatching none — and the bounce test is
untouched. But two assertions that read a member's journal for a *copy*
of a post had to change, because §1 is that there is no copy:
`fanned_out` became `readings` (filtering `contributor:rooms`), and the
per-member check finds `parent: start the count` inside the reading.
`ONE_POST` and `RELAY` also declare `patience_s: 0` seats, since a relay
on unnamed posts cannot run on patient ears.

Decided beyond the plan:

- **The cursor lives in the room's journal, keyed by member**
  (`cursor:<member>`), not on the member's session keyed by room. The
  latter is wrong: reading a member's session is `host.open` plus a
  dropped attachment, which makes the reader its last client and closes
  an idle seat, losing the wake just sent — it regressed the chase in
  `mentions`. ADR-0034 §2 amended. `cursors_of` was not built;
  `Unread::of` is the brick.
- **A reseat moves no cursor** — only names the room was not already
  seating start at its head — or every restart would sweep away every
  backlog. Cost: the first run after this change reads each room's
  history once. In ADR-0034's consequences.
- **A reseat reads the room once**: three questions, one snapshot,
  where `seat.rs` opened the same room three times per call.
- **The `~` sigil is gone**, not re-defaulted: it meant "patient", which
  is what a bare name means now. ADR-0029 §2 amended.
- `tests/rooms.rs` was outside the plan's files but had to be re-aimed:
  it seated `reviewer` bare and asserted the copied post.
- A black-box asserting a *reading* has to await one: a reading is
  journaled only once the woken member's turn starts, where a delivery
  landed at once, so `an_agent_opens_a_shared_room_and_its_peer_reads_
  the_post` raced the exit. It is host-driven and gated now.

Not done here: brick 7 (`bingo-surface-tui`), untouched — and its
`seats.rs::declared()` still falls back to `Ear::Live` for a seat with
no `listeners` entry, the reversed default, now wrong. Flagged for the
surface worker. No Windows cross-check: nothing here touches a process,
path, signal or clock.

## Verified — brick 7 and the integration, 2026-09-03

Brick 7 landed in two merges: the one-column labelled list with
`unread` (built to the plan's member-side cursor before bricks 1–6
moved it), then the reconciliation slice re-aiming the surface at what
the plugin ships — `unread` counts the room's `User` items past the
room-journal `cursor:<member>` watermark; `Ear::default()` is the
patient ear; `~` re-aimed to mark the live ear (the exception now);
the `SendMessage` room receipt says "at each seat's next turn; an @
wakes now". `roster_80x24` shows both labels, `3 unread` on the
lagging row and `live` on the live one; silences stay silent (no mark,
foreign shape, watermark gone).

On the merged line, all of it together: `cargo test --workspace
--locked` 69 suites ok, `clippy -D warnings` clean, `fmt --check`
clean, `check_discipline.sh` ok, `budget.sh` ok (302/302),
`tui-smoke.sh` ok. Recorded, not cured: a seat's own unpublished-read
post counts in its row's `unread` until its next turn (the surface
does not judge authorship); the live row gives up its tail sooner at
120-with-rail (§10's stated give).
