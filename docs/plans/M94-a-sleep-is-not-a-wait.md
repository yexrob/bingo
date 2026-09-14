# M94 — A sleep is not a wait

## Goal

User, 2026-09-14: with a background job running and nothing else to do,
the model reaches for a foreground `sleep` to wait for it. The harness
has the mechanism it needs — a job's completion opens a turn
(ADR-0018 §4) — but only `SpawnAgent` says the words that use it
("or end your turn — the reply wakes you", M81). `Bash`'s description
says "wait for its completion notification" without saying how, its
start receipt says "no reason to poll", and nothing refuses `sleep 60`,
which is not polling. Claude Code's harness ships both halves: the
prompt says work that finishes re-invokes you, and a foreground `sleep`
is blocked.

After this milestone: the words say ending the turn is the wait, in the
description and in every receipt that starts a job; a foreground command
that is nothing but `sleep` is refused with the way round it; a `sleep`
the model wants as a timer runs in the background and wakes it like any
job.

## Bricks, in build order

1. `idle.rs` — `reason(command) -> Option<String>`: pure, words via
   `reject::tokenise`; the first word is `sleep` and every other word is
   a duration (`30`, `0.5`, `2m`), so `sleep 2 && curl …`,
   `echo x; sleep 2` and `sleep "$n"` are left alone, the way `endless`
   leaves a shape it is unsure of. The reason names the command, says a
   foreground sleep only holds the turn and the person, and gives the
   three ways round: end the turn for a job or agent that will wake you;
   `background: true` on the same sleep to be woken after a delay; the
   check in the same call when a pause must precede it. Table test, one
   row per shape; a test that the reason offers each way round.
2. `lib.rs` — `call` refuses on `idle::reason` in the foreground branch
   only: a background `sleep` is a timer and stays. `started()` and
   `promoted()` say ending the turn is how you wait. The description's
   background paragraph says the same, and that a bare `sleep` is
   refused. The spec test pins "end your turn" and "wakes you"; the
   background-call test pins the receipt; a new test refuses `sleep 30`
   and lets `sleep 30` with `background: true` through.
3. ADR-0018 §9: the rule, dated.
4. Black-box: `sessions.rs` and `agents.rs` held a turn open with a bare
   `sleep`; they say `echo waiting; sleep N`, which is what they meant.
   `jobs.rs` gains one run: a scripted bare `sleep` comes back as an
   error result that says "end your turn", and no log is written.

## Files

- `crates/bingo-tool-bash/src/{idle.rs,lib.rs}`
- `crates/bingo/tests/cli/{jobs.rs,sessions.rs,agents.rs}`
- `docs/adr/0018-background-commands.md`

## Exit criteria

- [x] `Bash{command: "sleep 30"}` answers an error naming the command and
      "end your turn"; `background: true` starts it as a job.
- [x] `Bash`'s description and both receipts contain "end your turn".
- [x] the black-box run sees the refusal and no log file.
- [x] every gate green (fmt, check, clippy, test, discipline, budget).

## Non-goals

- Refusing `sleep N; true` or `sleep N && true`: the table stays narrow
  on purpose; a model that dodges the rule has read the reason.
- A `--print` one-shot still exits on the root's `TurnCompleted` (M81
  non-goals): a wake sent after that is lost there, and this milestone
  does not change what a print run waits for.
- A kernel-level harness prompt block. The words live in the tool that
  owns the mechanism, as `SpawnAgent`'s do.

## Risks

- R-timer: a model told to background its `sleep` may do so for
  readiness waits that belonged in the same call. The reason lists the
  same-call shape first for that case.
- R-tests: any script that held a turn with a bare `sleep` now gets a
  refusal instead; `rg` over `crates` found the two named above.

## Verified (2026-09-14)

- `cargo test -p bingo-tool-bash --locked` → `140 passed; 0 failed`
  (`a_bare_sleep_is_refused_in_the_foreground_and_a_timer_in_the_background`,
  `idle::tests::*`, the spec pins among them).
- `cargo test -p bingo --test cli --locked -- jobs:: sessions::a_session_another_process
  agents::a_wake_that_finds` → `12 passed; 0 failed`, the new
  `jobs::a_bare_foreground_sleep_is_refused_and_starts_nothing` among them.
- `cargo fmt --all -- --check` ok; `cargo check --workspace --all-targets
  --locked` clean; `cargo clippy --workspace --all-targets --locked --
  -D warnings` clean.
- `cargo test --workspace --locked` → exit 0, 89 suites `ok`, no failures.
- `scripts/check_discipline.sh` → `discipline ok` (four plan-length
  warnings predate this); `scripts/budget.sh` → `budget ok`.
