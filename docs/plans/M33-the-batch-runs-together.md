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

- [ ] the rendezvous test proves two safe allowed calls overlap and
      fails (times out) when execution is serialized
- [ ] a batch splits at a non-safe or non-allowed call, pinned
- [ ] Bash carries `concurrency_safe: true` with the rationale in
      its doc comment; background jobs behave as before
- [ ] the TUI lane shows two pulsing presence bullets in one step
- [ ] black-box: a one-step two-bash turn completes with both
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
