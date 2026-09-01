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

- [x] the switcher lists the root's stored descendants, marked, one
      row per session, live wins over stored; enter reopens and the
      row turns live — snapshots prove both states
- [x] a resumed root carries one line per stored child that was
      mid-turn; the child's journal is not rewritten and the child
      is not reopened; a root with no such child says nothing
- [x] the merged roster's order and dedup are pinned by unit test
- [x] every gate green (fmt, check, clippy, test, discipline,
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

## Verified

Gates, on the merge base `5713d10` (load 25.71 before, 33.99 after —
no timing-sensitive family failed, and the whole suite was run twice
with the same result):

```
cargo fmt --all -- --check                      (silent)
cargo check --workspace --all-targets --locked  (silent)
cargo clippy --workspace --all-targets --locked -- -D warnings  (silent)
cargo test --workspace --locked                 passed: 2866 failed: 0
scripts/check_discipline.sh                     discipline ok
scripts/budget.sh                               dependencies 302 (max 302) · budget ok
cargo deny check                                advisories ok, bans ok, licenses ok, sources ok
```

What each criterion rests on:

- **The switcher lists them, marked, one row each, live winning.**
  `tree::roster` (`crates/bingo-surface-tui/src/tree.rs`) with
  `screens::the_switcher_lists_what_is_only_in_the_store` (both sizes)
  and `view::a_stored_row_turns_live_in_place_when_its_frames_arrive`,
  whose two snapshots are the same row before and after: `scout ⏺
  stored` becomes `scout ⏺ idle` in place. The wiring end to end is
  `run::opening_the_switcher_reads_the_store_and_the_card_fills`;
  the key is `input::enter_on_a_stored_row_steps_into_the_session_it_names`.
- **Order and dedup pinned.** `tree::the_roster_puts_the_tree_first_and_
  the_stored_rows_after_it_by_id`, `..._is_one_row_and_the_live_one`,
  `..._whose_parent_chain_reaches_the_root`, and a circular chain that
  must still terminate.
- **One line per mid-turn child, journal untouched, child not reopened.**
  `host::tests::resume::a_resumed_session_is_told_which_child_was_mid_turn`
  (three planted children — busy, finished, `Log` — one line, the busy
  child's journal compared byte for byte, and `session_state` on it
  still `Err`), its negative twin `..._with_no_lost_child_says_nothing`,
  and the black-box
  `cli::agents::a_resumed_root_is_told_which_child_was_mid_turn_when_
  the_process_ended` (2.5s).

Decisions the plan left open:

- **A durable `ItemBody::Notice`, not an ephemeral `Event::Notice`.**
  The notice is published while the session opens, which is before any
  client attaches; `attach` hands frames after the snapshot's `seq`, so
  an ephemeral one would reach nobody. As an item it is in the snapshot
  the TUI folds and in the journal, which is what "the root carries it"
  has to mean. `Host::notice` already records items this way.
- **It is awaited inside `resume_frames`.** The `record` is queued
  before the client's `attach`, so the line is in the journal and in the
  snapshot deterministically. `Host::open` already waited on the start
  hooks through `attach`, so this adds no new wait on that path.
- **Every reopen reports, not only a root.** A root is a session with no
  parent; the rule is the same one. A session with no stored children
  pays one `list`. This means `deliver`/`extend` reopening a persisted
  target (ADR-0011 §3) also pays it — the peers, rooms, channels and
  plugin-rpc suites are the evidence that it neither deadlocks nor drags.
- **Liveness is not checked before reporting.** A role the start hooks
  are re-seating concurrently may be both reported and reopened. The
  words hold either way: the turn *was* lost, and "wake it" is true of a
  live session too. Checking would have been a race, not a fix.
- **`ctrl+g` no longer refuses a lone root, and `NO_AGENTS` is gone.**
  Whether there is anywhere to switch to is now the store's answer and
  it has not arrived when the key is pressed; refusing would have hidden
  exactly the stored children this milestone is for.
- **`Tree::show` keeps a session the tree has not heard of, and `view()`
  answers it.** Otherwise a line typed in the gap between `⏎` and the
  child's head frames would go to the root. Now `writer()` finds no
  mailbox and the existing "still opening that session" refusal fires.
- **The listing is unfiltered.** A child need not work where its root
  does, so `SessionFilter::default()` and the parent chain decides.
- **The black-box kills the process.** A graceful exit closes its
  sessions, and a closed session's turn is completed — so only a kill
  leaves a journal mid-turn, which is what `recover` was written for.

## Carried

- `crates/bingo-surface-tui/src/run.rs` crossed the discipline warn
  line (700 → 725 non-test lines). Still a warning, not a failure; the
  loop is the right home for a host call and its reply, but the file is
  now a candidate for a split of its own.
- The line reaches a person through the transcript, so `--print` and
  `--output-format json` never show it: they print stream frames, and
  this item is folded into the snapshot before any client attaches. The
  TUI, the RPC surface and the journal all carry it. Putting it on the
  stream would need the session to know when a client has attached.
- The switcher's read is one `sessions` call over the whole store on
  every opening. At the store's GC ceiling (100 sessions) that is 100
  `summary.json` reads; nothing here is paged or cached.
- `activity()` carries an unreachable `Status::Stored` arm: `Status::of`
  reads a live state and cannot answer it. The alternative was a second
  marker beside the status, which is worse.
