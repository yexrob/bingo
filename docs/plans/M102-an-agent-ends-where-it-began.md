# M102 — An agent ends where it began

## Goal

User, 2026-09-16: "我发现现在bingo对agent管理的工具不健全". Four retired
roles stood on a roster with no verb to remove them. The kernel has every
verb (`interrupt`, `delete`, `deliver` to a persisted child) and the
agents plugin exposes none of them. After this milestone the session that
started an agent can stop its turn, dismiss it, call it back by name with
its memory, and read a roster that says what each one is running on and
what it has cost — and a person can do the first two from `/agents`.
ADR-0060.

## Bricks, in build order

1. `tests.rs` — the `Fleet` double learns the two verbs it was told it
   would never see: `delete` removes the session and records the id;
   the attachment's port records `interrupt(session, scope)` instead of
   panicking, and a `turn_interrupted()` event joins the script helpers.
2. `stop.rs` — `StopAgentTool { agent }`: `names::child` (own children
   only), `watch::follow`, refuse in words when the snapshot is idle,
   `handle.interrupt(IntentId::mint(), Head)`, then read the stream to
   the `TurnCompleted` (receipt names the status) or to a `Rejected` ack
   for that intent (receipt says it had already ended). The caller's
   cancel token ends the wait, never the child. Traits `crate::traits()`.
   Tests: a busy child is interrupted and the receipt says `interrupted`;
   an idle child is refused; a name nobody has says who is here; a
   sibling's name is refused (not yours to stop).
3. `dismiss.rs` — `DismissAgentTool { agent }`: own child, refused while
   busy, `host.delete`. Traits destructive, not read-only, trusted.
   Tests: an idle child is deleted and gone from `sessions`; a busy one
   is refused naming `StopAgent`; a sibling is refused; the traits.
4. `spawn.rs` — `reopen: Option<bool>`: when set and an own child of
   `base` stands, `watch::follow` it and deliver, no `Create`; the three
   arms unchanged. Test: a reopened child gets the prompt, nothing is
   spawned, the receipt names the same session; without such a child the
   spawn is ordinary.
5. `list.rs` — `HEADERS` gains `model`, `messages`, `tokens`; `row`
   reads them off the summary (`-` for none). Tests updated; the tree
   view keeps its badge.
6. `command.rs` — `/agents [stop <name> | dismiss <name>]`, `ArgSpec::Free`;
   the table when bare. Shares `stop::stop` and `dismiss::dismiss`.
7. `lib.rs`, `guide.md`, `guide.rs` — manifest `provides`, registration,
   the crate doc's "every tool is read-only" becomes "every tool but
   `DismissAgent`", the page gains a `## Ending one` section and the
   `reopen` word; the page test pins every tool as it does now.
8. Black-box `tests/cli/agents.rs`: a script that spawns in the
   foreground, then `DismissAgent`s under `--dangerously-skip-permissions`;
   the child's journal dir is gone and the receipt says so. A second
   run: `StopAgent` on an idle child is an error result naming `idle`.
9. ADR-0060 (written first), ADR-0010 §6 annotated, `ARCHITECTURE.md`
   untouched unless it lists tools.

## Files

- `crates/bingo-agents/src/{stop.rs,dismiss.rs}` (new)
- `crates/bingo-agents/src/{spawn.rs,list.rs,command.rs,lib.rs,tests.rs,guide.md,guide.rs}`
- `crates/bingo/tests/cli/agents.rs`
- `docs/adr/{0060-an-agent-ends-where-it-began.md,0010-sub-sessions.md,README.md}`

## Exit criteria

- [x] `StopAgent` on a busy child ends its turn; on an idle one the
      result is an error the model can read.
- [x] `DismissAgent` on an idle child deletes it (`session/list` and the
      data dir no longer have it); on a busy one it is refused.
- [x] `SpawnAgent { reopen: true }` delivers to the standing child and
      mints nothing.
- [x] `ListAgents` rows carry model, messages and tokens; `/agents` the
      same columns.
- [x] `/agents stop|dismiss <name>` work from the composer.
- [x] every gate green (fmt, check, clippy, test, discipline, budget: no
      new dependency).

## Non-goals

- Ending a teammate (a sibling): its parent's business.
- A budget or deadline per spawn; `SetModel`; a rename; a wait with a
  timeout; a `done`/`failed` word in the roster (needs a kernel fact).
- `esc` on a waiting parent ending its children (M82 non-goal stands).
- A TUI key for stop/dismiss: `/agents` is the person's lever this round.

## Risks

- R-race: a child whose turn ends between the snapshot and the interrupt
  gets a `Rejected` ack; the stop reads it as "already ended", never
  waits on a `TurnCompleted` that will not come.
- R-watcher: a background spawn's watcher reports the interrupted turn as
  cut short; a `DismissAgent` afterwards finds the child idle. Dismissing
  first is refused while the turn runs, so the watcher never loses its
  session mid-turn.
- R-columns: the black-box roster test reads `ses_` words only, so three
  more columns move nothing it pins.

## Verified (2026-09-16)

- `cargo test -p bingo-agents --locked`: 172 passed. New: `stop.rs` (6),
  `dismiss.rs` (4), `names::mine`, `spawn::reopen_*` (2), the roster row
  with the three columns, `/agents stop|dismiss` (2).
- Black-box, `crates/bingo/tests/cli/agents.rs`: `DismissAgent` under
  `--dangerously-skip-permissions` deletes the child's
  `.bingo/data/sessions/<id>` and the root's stays; without the bypass,
  off a tty, the call is an error result and the directory stands;
  `StopAgent` on an idle child is an error result saying `scout is idle`.
- `cargo fmt --all -- --check`, `cargo check --workspace --all-targets
  --locked`, `cargo clippy --workspace --all-targets --locked -- -D
  warnings`, `cargo test --workspace --locked --no-fail-fast` (91 test
  binaries, 0 failed), `scripts/check_discipline.sh` (ok),
  `scripts/budget.sh` (ok, no new dependency).
- The `/agents stop|dismiss` composer path is covered by the command's own
  tests over the fleet double; no tmux drive, the change draws nothing new.
- Found in the user's first drive (2026-09-16 16:19, root
  `ses_01M2FBBSK7Y2XM6GG68KN92HH3` resumed on the new build): the four
  `DismissAgent` calls succeeded and the directories were gone, but the
  switcher kept the four rows as `stored`. A child a resume replays from
  the store has no stream to carry `SessionClosed { Deleted }`; the host's
  `SessionRemoved` was the one word, and the TUI ignored it. Fixed in
  `run.rs` (`removed`: the tree row, the handle, the switcher's stored
  list); unit test `a_stored_child_the_host_removed_leaves_the_tree`;
  ADR-0060 Consequences corrected. The running process needs the rebuilt
  binary to show it; a fresh `--resume` lists from the store and is clean
  either way.
