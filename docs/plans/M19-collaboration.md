# M19 — Collaboration: mentions and the board (ADR-0022, ADR-0023)

## Goal

What a room post owes becomes visible and chased: `@name` opens a debt the
member's next post closes, one bounded chaser nudges the silent, `/room`
and a rail signal show who owes what. And the room becomes a workplace: its
own task list is the shared board — the four task tools and `/tasks` gain
`in: "#name"`, claiming stamps the caller's runtime title, and a gone
owner is marked at read time, never silently rewritten.

## Bricks, in build order

**Worker F — mentions (`bingo-rooms`):**

1. **The fold** — pure: `mentions(posts, members) -> Vec<Mention>` over the
   room's journal items (word-boundary, case-insensitive against members;
   `@all` one debt closed by any other member; a non-member `@` opens
   nothing; closure = the named member's next post). Fixture-style tests
   the way the old tree's parser table reads: `@User,` hits, `mail@user`
   does not.
2. **The chaser** — in the hook that already watches journals: one bounded
   timer per open mention (300 s × 3), nudge = `deliver(member, Wake)`
   carrying the room, the asker and the post's head; a nudge is not a post
   and opens nothing; timers die with the process and the next hook run
   re-derives and chases overdue **once** (the schedule rule).
3. **Showing it** — `/room` gains an `owed` column (member, oldest age);
   the hook publishes signal `owed` (`View::Table`) on the room's parent
   while debts are open, `Null` when the last closes.
4. **Black-box** (`tests/cli/`, fake script) — a post with `@member`, the
   member silent → the nudge lands in its journal; the member posts → the
   debt closes and no further nudge; restart re-chases once; `@all` never
   nudges; `/room` shows the column.

**Worker G — the board (`bingo-tasks`):**

5. **Resolution brick** — pure-ish: `board(host, caller, "#name")` walks
   child-then-sibling rooms of the caller (the post address rule); an
   unreachable name is a worded error result the model corrects.
6. **The tools** — `in: "#name"` on all four + `/tasks`; `claim: true` on
   `TaskUpdate` sets owner to the caller's own resolved title (runtime-
   stamped; explicit `owner` stays for assignment); without `in`,
   behaviour is byte-identical to today and the existing tests prove it
   untouched.
7. **The gone mark** — render-time: `TaskList`/`/tasks` mark an owner with
   no live session of that title as `owner: x (gone)`; nothing is written.
8. **Black-box** — parent opens a room and puts tasks on its board; a
   spawned member lists the board, claims one (owner = its title without
   stating it), completes it; the parent's `/tasks in #room` shows it;
   after the member is gone its remaining task reads `(gone)`; a bogus
   `in` is a worded error.

## Files

F: `crates/bingo-rooms/src/{mentions,hook,command}.rs` + one cli test file.
G: `crates/bingo-tasks/src/{board,…}.rs` (tools, command, render) + one cli
test file. Shared: one `mod` line each in `tests/cli/main.rs` (union).
No new dependencies; budget unchanged.

## Exit criteria

- [x] an unanswered `@` nudges the member at 300 s, at most three times; answering stops it — the cadence is paused-clock-verified; a real five-minute wall wait is not
- [x] the debt is derived: restart re-derives and chases overdue once
- [x] `/room` shows `owed`; the parent's rail carries the signal while debts are open and drops it after
- [x] the four tools + `/tasks` work against `in: "#room"`; without `in` byte-identical to today
- [x] `claim: true` stamps the caller's runtime title; a gone owner renders `(gone)` with nothing rewritten
- [x] black-box scenarios of bricks 4 and 8 green
- [x] every gate green (fmt, check, clippy, test, discipline, budget unchanged, deny)

## Non-goals

Obligations on direct `SendMessage`; auto-flipping a crashed owner's tasks
(the wake makes the parent the janitor); write attribution beyond claiming;
read receipts; escalation past three nudges; teams changes; kernel changes.

## Risks

R-noise — a chaser must never storm: the fold is the single source, a
mention nudges at most three times ever, and the restart chase is once.
R-race — two board writers last-write-wins on the whole list; accepted at
this scale, said in the tool prompt (ADR-0023 §4). R-resolve — two plugins
now walk the tree for `#name`; duplication accepted, recorded for the sdk
sweep. R-title — claiming needs the caller's title from its session id; if
the walk cannot find it (a root has none), `claim` is a worded error, not a
guess.

## Verified (2026-09-01)

- Worker F merged `ad969db` (mentions: the fold, the one chaser, `owed`);
  worker G merged `adf0c0a` (the board). Both merges clean; the shared
  `tests/cli/main.rs` mod lines unioned.
- Integrated gates on the quiet machine (1-min load 5.2), together with the
  day's fixes that rode the same run (the suite's isolated-HOME floor and
  the quiet held-lock skip `cf7f77d`; the registry requires-fixpoint
  `3f7a31e`): `GATES_EXIT=0`, **2575 tests passed, 0 failed**,
  `dependencies (unique, normal): 302 (max 302)`, discipline ok, deny ok.
- F's black-box: an aged journal's restart nudges exactly once and quotes
  the right post; answering closes the debt, drops the rail card to `Null`,
  and nobody is nudged; `@all` shows in `owed` and chases nobody. F found
  and revert-verified its own cross-room timer-cancel bug.
- G's black-box: a spawned member lists, claims (`— worker` stamped from
  the runtime, never stated) and completes on the parent's board; the gone
  mark appears across two `--continue` runs and writes nothing; a bogus
  `in` is a worded error; without `in`, byte-identical — zero tree reads,
  every pre-existing test untouched.
- Taste calls reviewed and kept: the nudge budget is per process — three
  for a debt heard asked, exactly one for one found already overdue (a fold
  cannot know what a dead process sent; N restarts cost N single nudges,
  never storms). Board tools stay `read_only: true` on the SendMessage
  precedent, pinned by a test so any future change moves both together.
  The gone mark reads presence, not liveness — what the sdk can honestly
  say today. `@all` closure excludes the asker; the room's holder counts
  as claimable presence.

## Carried

- A room this process never opens is never chased: declared rooms re-seat
  at start and `/room` covers the rest, but an ad-hoc room from a dead
  process waits for something to open it — chosen over sweeping the store
  at boot, which would resurrect sessions and take locks.
- The `owed` rail signal is published but unseen: no bingo-surface-tui
  change was made, so how the card actually looks on screen is open.
- Wished seams, again: a `Clock` (chase timing is tested only through
  tokio's paused clock), `SessionFilter { id }` (claim + `in` walks the
  tree twice), liveness on `SessionSummary` (gone = presence).
- R-resolve stands — a third tree-walk now exists; the projection registry
  proposed in `docs/design/research/plugin-openness.md` is the recorded
  fix, pending the M20 scope decision.
