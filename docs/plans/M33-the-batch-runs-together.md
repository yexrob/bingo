# M33 — The batch runs together

## Goal

The machinery for parallel tool calls exists end to end — the
executor batches consecutive concurrency-safe allowed calls
(`executor.rs`, cap 10), the read-family tools and web fetch declare
`concurrency_safe`, both providers fold a multi-call step, and the
prompt asks for independent calls together — yet nothing pins that a
two-call step actually overlaps, Bash refuses to join a batch by
trait, and the prompt's one line is too weak for models to obey.
After this milestone a model that emits independent calls in one
step sees them run together, Bash included, and a test would fail
the day that stops being true.

## Bricks, in build order

1. **Bash joins the batch.** `bingo-tool-bash`'s main tool flips to
   `concurrency_safe: true`, aligning with Claude Code: two commands
   that would race are the model's own judgment, exactly as two
   racing `Edit`s would be; the gate still serializes anything not
   `Gate::Allowed`, and background jobs (ADR-0018) are untouched.
   The pinned trait test flips with it; the trait's doc comment
   carries the why.
2. **The pin that cannot flake.** An executor-level test with two
   fake tools that rendezvous on a barrier — each waits for the
   other to have started before finishing. It completes only if the
   batch truly runs together and deadlocks-to-timeout if execution
   is serial. No wall-clock assertions (machine-load lore: timing
   tests lie under load). A companion test pins the boundary: a
   non-safe call between two safe ones splits the batch.
3. **The prompt says it like it means it.** `prompt.rs`'s line
   becomes instruction-grade: independent calls go in the same
   step, dependent calls wait. One or two sentences, English,
   no essay.
4. **The surface shows it.** A TestBackend lane with one step
   holding two live tool calls: two rows, both bullets in
   `presence`, pulsing at once — the visible fact that two things
   are running. Snapshot pinned.
5. **Black-box smoke.** A fake-provider script emits one step with
   two bash calls; both receipts land, the turn completes, NDJSON
   stays valid. Behavioural only — no timing.

## Files

`crates/bingo-tool-bash/src/lib.rs` (trait + its test),
`crates/bingo-core/src/executor.rs` (tests beside it),
`crates/bingo-core/src/prompt.rs`,
`crates/bingo-surface-tui/src/` (one lane + snapshot),
`crates/bingo/tests/cli/` (smoke). No new dependencies; schemas
untouched (no wire change).

## Exit criteria

- [x] the rendezvous test proves two safe allowed calls overlap and
      fails (times out) when execution is serialized
- [x] a batch splits at a non-safe or non-allowed call, pinned
- [x] Bash carries `concurrency_safe: true` with the rationale in
      its doc comment; background jobs behave as before
- [x] the TUI lane shows two pulsing presence bullets in one step
- [x] black-box: a one-step two-bash turn completes with both
      receipts; every gate green (fmt, check, clippy, test,
      discipline, budget unchanged, deny)

## Non-goals

Reordering calls to widen batches (consecutive-only stays — order
is the model's); parallelism across steps or sessions; a
concurrency setting; per-command safety analysis for Bash (the
trait is the decision); provider changes (they already fold
multi-call steps).

## Risks

R-race — two parallel bash commands can genuinely race on shared
state: accepted deliberately, as Claude Code accepts it; the model
owns the judgment, the plan records the decision. R-flake — any
timing-based proof would join the load-flake lore; the rendezvous
barrier is the design answer, wall-clock asserts are forbidden in
this milestone. R-interrupt — a parallel batch under interrupt must
keep completed results and mark the rest per executor's existing
contract; the rendezvous test runs once more under a cancelled
token to pin it.

## Verified

2026-09-01, worktree on 6f30958. Bash: `16bd995`; executor pins:
`f7d5544`; prompt: `2468c09`; TUI lane: `f10fd15`; smoke: `50926be`;
the two file-size splits M33's additions forced: `fe7ef16`, `c0dc528`.

The pins bite. Forcing `let safe = false` in `execute` (serial
execution) fails exactly the three overlap tests, at the barrier's
bound rather than by a clock:

```
failures:
    executor::tests::an_interrupt_keeps_what_a_parallel_batch_already_finished
    executor::tests::results_come_back_in_input_order_whichever_finishes_first
    executor::tests::two_safe_allowed_calls_are_in_flight_at_the_same_moment
test result: FAILED. 7 passed; 3 failed; ... finished in 10.01s
```

Dropping the `concurrency_safe` and `gate` conjuncts from the forward
scan (batch everything) fails the other side:

```
failures:
    executor::tests::a_call_that_cannot_run_together_splits_the_batch_around_it
    executor::tests::a_denied_call_splits_the_batch_around_it_too
    executor::tests::an_interrupt_keeps_what_a_parallel_batch_already_finished
```

The TUI pin bites too: give the second call a finished status and
`every_row_of_a_batch_wears_the_live_mark_at_once` fails on the row it
carries. No test in this milestone asserts a duration.

Gates, all green (load average 9-15 throughout, 47 at the peak of the
test run; no rerun needed):

```
cargo fmt --all -- --check                      clean
cargo check --workspace --all-targets --locked  Finished
cargo clippy ... -- -D warnings                 Finished
cargo test --workspace --locked                 2895 passed; 0 failed
                                                (69 targets, 0 FAILED)
scripts/check_discipline.sh                     discipline ok
scripts/budget.sh                               budget ok (302 deps, max 302)
cargo deny check                                advisories/bans/licenses/sources ok
scripts/tui-smoke.sh                            tui-smoke ok
```

Two calls the plan left open, taken here. The interrupt pin is two
tests, not one: under an *already*-cancelled token nothing runs at all,
so "completed results kept" cannot be exercised there — that half is
pinned by cancelling from `on_done` once the parallel batch has landed.
And the black-box smoke is behavioural only, as the plan asks, so it
does not itself prove overlap; that proof is the rendezvous test, and
the trait test is what joins Bash to it.

Not proved here: that two real shell commands overlap end to end. The
only black-box proof would be a filesystem rendezvous inside the two
commands, which runs into Bash's own timeout and auto-background
heuristics; the executor pin plus the trait pin compose to the same
fact.

Integrated on main at `ae445fc` (2026-09-01): the whole gate suite
green on the merge — `cargo test --workspace --locked --no-fail-fast`
exit 0 with an empty `failures:` capture across all targets,
fmt/check/clippy clean, discipline ok (pre-existing warns only),
budget 302, deny ok, tui-smoke ok. Load 8.4 → 12.5; no rerun needed.
