# M81 — The wait is the turn's end

## Goal

Delete `WaitAgent`. A parent that has started a background agent waits
for it by ending its turn: the watcher `SpawnAgent` left behind wakes it
with the reply (`watch::report`, ADR-0024 §2). A tool that holds the
turn instead holds the person too — every message they type queues
behind a join the model did not need — and, offered beside `SpawnAgent`,
it is what the model reaches for first. Claude Code's harness ships the
same shape: sub-agents notify, and the blocking read is not in the
visible tool list.

M23 examined removing `WaitAgent` and kept it, against the survey's
verdict that a missing join was the old tree's top gap. That verdict was
made while `background: false` blocked the whole turn and nothing woke a
parent. Both changed in M8 and M20; the gap the join filled is gone.

## Bricks, in build order

1. **`wait.rs` deleted**, with its registration, manifest line, doc
   header, the two lists in `lib.rs`'s tests, and `watch::last_reply`
   (its only reader). `watch::replied` becomes private.
2. **`SpawnAgent`'s description** stops naming `WaitAgent`; the sentence
   that teaches ending the turn stays, and its test pins it.
3. **`scripts/check_discipline.sh`** drops the name from the tool-name
   regex; ADR-0024 §3 drops the clause that `WaitAgent` resolves names.
4. **Black-box** (`tests/cli/agents.rs`): the two join tests go with the
   tool. The M31 "child left mid-turn" run, which used a two-second wait
   to end the root's turn before the child's, addresses the root's last
   response with `when: contains` instead — race-free where the wait was
   a margin. The ADR-0027 seated-member run keeps its journal assertions
   (no turn, no journalled brief) and loses the wait.

## Files

- `crates/bingo-agents/src/{wait.rs,lib.rs,spawn.rs,watch.rs}`
- `crates/bingo/tests/cli/agents.rs`
- `scripts/check_discipline.sh`, `docs/adr/0024-peer-messages.md`

## Exit criteria

- [x] `rg WaitAgent crates scripts docs/adr` finds nothing; plans and the
      survey keep their history.
- [x] `SpawnAgent`'s description says the reply wakes you and there is
      nothing to poll; the description test pins it.
- [x] the M31 and ADR-0027 black-box runs pass without a wait.
- [x] every gate green (fmt, check, clippy, test, discipline, budget).

## Non-goals

- `--print` exits on the root's `TurnCompleted`, so a wake that lands
  after it is lost there — for a background agent and, since ADR-0018,
  for a background command alike. `background: false` remains the
  in-turn read. Defining a print run's end as "the tree is quiet" is its
  own milestone, if wanted.
- A deferred-tool mechanism (a tool listed by name until fetched) is
  researched separately; nothing here depends on it.

## Risks

- R-fanout: a parent that started three agents is woken three times and
  must end its turn twice more. The description says so; the cost is two
  short turns on a stable prefix (M80).
- R-teammate: reading what an idle teammate last said had no door but
  `WaitAgent`. ADR-0027 §3 already routes that by room or message.

## Verified (2026-09-07)

- `rg WaitAgent crates scripts docs/adr` → no matches.
- `cargo fmt --all -- --check` ok; `cargo check --workspace --all-targets
  --locked` clean; `cargo clippy --workspace --all-targets --locked --
  -D warnings` clean.
- `cargo test --workspace --locked`: every crate `ok`, no failures
  (bingo-agents 143, cli 200 among them).
- `cargo test -p bingo --test cli -- agents::a_resumed_root_is_told…
  agents::an_unwoken_member_has_no_turn… agents::a_finished_background_agent_wakes…`
  → `3 passed`.
- `scripts/check_discipline.sh` → `discipline ok` (the `session.rs:129`
  warning predates this); `scripts/budget.sh` → `budget ok`.
