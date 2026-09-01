# M31 — Stored children in sight: the switcher and the mid-run line

## Goal

Resume is per-session by design (ADR-0005 §7) and what a project
declares is re-seated by the start hooks; what was spawned ad hoc —
an agent, a `/room` — stays in the store, addressable but invisible.
Two visibility fixes, no change to resume's semantics: the TUI's
switcher lists the root's stored descendants so a person can see and
reopen them, and a resumed root is told, one line per child, when a
child was mid-turn at the end of the last process — the report its
spawner was waiting for will not arrive.

## Bricks, in build order

1. **The merged roster, pure** (`tree.rs` or a sibling) — a function
   from the live states and a stored listing to one row list: a
   session both live and stored is one row and the live one wins;
   a stored-only row carries its own status kind (`Stored`), its
   title, and reopens by id. Order pinned by test, not assumed.
2. **The switcher reads the store when it opens** (`input.rs`,
   `ui.rs`, `view.rs`, `run.rs`) — opening the switcher spawns one
   `sessions` read through the loop's existing host-call lane
   (`run.rs` keeps results, never awaits in the loop); the stored
   descendants of the attached root (their parent chain reaches it)
   land in the `Switcher` card and render dimmed with a `stored`
   mark. Enter on a stored row opens it `ById` (the root keeps the
   tree stream; the row turns live when the child's head frames
   arrive) and switches the view. One read per opening; nothing
   watches the store.
3. **The mid-run line** (`host/resume.rs` neighbourhood) — after a
   root is resumed, list its stored children; for each whose journal
   ends inside a turn — replayed and folded by the one reducer, and
   asked `busy()`; no new stored field, no second representation —
   publish one `Notice` on the root naming the child and saying its
   turn was lost with the process and it can be woken to continue.
   The child is not reopened and its turn is not closed here:
   `recover()` owns that at the child's own reopen, and the words
   must not promise what did not happen. A `Log` session is never
   busy and costs nothing.
4. **Black-box** — a binary scenario (beside the M8/M9 agent
   scenarios): a background child is mid-turn when the process ends;
   `--continue` brings the root back with one notice naming the
   child, and the child's journal is untouched. TUI `TestBackend`
   snapshots: a stored row in the switcher; enter reopens it and the
   row turns live. Paused clocks; nothing waits wall.

## Files

`crates/bingo-surface-tui/src/{tree,view,input,ui,run}.rs`,
`crates/bingo-core/src/host/resume.rs` (and its tests),
`crates/bingo/tests/` (the resume scenario where the agent
scenarios live), TUI snapshot tests. No new dependencies; budget
unchanged; sdk untouched unless a fact is genuinely missing — then
stop and report before adding one.

## Exit criteria

- [ ] the switcher lists the root's stored descendants, marked, one
      row per session, live wins over stored; enter reopens and the
      row turns live — snapshots prove both states
- [ ] a resumed root carries one line per stored child that was
      mid-turn; the child's journal is not rewritten and the child
      is not reopened; a root with no such child says nothing
- [ ] the merged roster's order and dedup are pinned by unit test
- [ ] every gate green (fmt, check, clippy, test, discipline,
      budget unchanged, deny)

## Non-goals

Respawning or re-running an interrupted child turn; delivering the
lost report; watching the store live from the switcher; a `busy`
field on `SessionSummary` (a second representation of the journal's
tail); changes to `recover()`, to seating, or to resume's
one-session semantics; ACP.

## Risks

R-two-sources — the roster merges the tree's states with a store
listing: live is the truth and the merge must never show one session
twice; the unit test is the pin. R-cost — brick 3 replays every
stored child of one root once at resume; journals are small and
children few, but if the black-box scenario drags, say so rather
than cache. R-async — the switcher's store read lands after the
card opens; the card fills when it does and the snapshot pins the
filled state, not the race. R-words — the notice reports a fact
(the turn was lost) and an option (wake it); it must not claim the
child was resumed or the work rerun.
