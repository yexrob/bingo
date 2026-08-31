# M21 — Ongoing notify conditions, debounced (ADR-0018 §8)

## Goal

A background job's `notify_on`/`notify_regex` can outlive their first hit:
`notify_all: true` turns them into an ongoing watch with a leading-edge
throttle — first hit wakes at once, matches inside the thirty-second quiet
window are counted, and the count rides the next wake or the completion as
"…and N more lines matched since the last notice". The default stays
first-hit-once, byte-identical to today.

## Bricks, in build order

1. **The tally** — `Conditions` learns to count: matches in a window of
   text as (count, last matching line), the existing first-hit read built
   on it. Pure, table-tested.
2. **The throttle** — `Scan` grows the mode: `Once { fired }` unchanged;
   `All { last_wake, suppressed, last }` wakes on a hit when the quiet
   window (30 s, a named constant with its why) has passed since the last
   wake, else counts; time is taken so tokio's paused clock can drive it.
   The messages: `notify::matched` gains the optional "and N more" clause;
   `notify::finished` carries a pending tally the way it carries a late
   hit today. Unit tests on the paused clock: leading edge immediate, a
   burst inside the window is one wake, the window's end alone flushes
   nothing, the count resets on wake.
3. **The flag** — `notify_all: Option<bool>` on `BashArgs`, threaded to
   the scan; `true` without a condition is a worded refusal (the bad-regex
   precedent); the tool description gains one sentence. The schema test
   that pins the arg names gains the new one.
4. **Black-box** (`tests/cli/jobs.rs`) — a job that matches, bursts and
   exits: the first hit wakes, the completion carries "and N more…"; a
   `notify_all` call without conditions is refused with the worded error;
   a default call still notifies exactly once (regression).

## Files

`crates/bingo-tool-bash/src/{notify,supervise,lib}.rs`,
`crates/bingo/tests/cli/jobs.rs`. No new dependencies; budget unchanged.

## Exit criteria

- [x] default behaviour byte-identical: one hit, one wake, silence after
- [x] `notify_all`: first hit immediate; a burst in the window is counted,
      not delivered; the count rides the next wake or the completion
- [x] paused-clock coverage of the window; no wall-clock waits
- [x] `notify_all` without conditions refused with a worded error
- [x] black-box scenarios green; every gate green (fmt, check, clippy,
      test, discipline, budget unchanged, deny)

## Non-goals

A trailing flush timer; per-call window lengths; carrying more than one
line per wake (the log is the representation, `BashOutput` the reader);
any change to completion or growth semantics; kernel or sdk changes.

## Risks

R-clock — the throttle must read time through something tokio's paused
clock drives, or the tests wait thirty real seconds; the chase timers are
the precedent. R-race — a hit landing between the last scan and process
exit must be neither lost nor doubled: the end-of-job look already runs
after the readers drain, and the pending tally folds into `finished`.

## Verified (2026-09-01)

- Worker J merged `b910d5d`, clean: `Conditions::tally` under both
  readings (the first-hit read is the tally stopped at one match, so the
  two cannot disagree), `Scan`'s `Mode::{Once, All}` with the 30 s
  `QUIET` window on `tokio::time::Instant`, `Notice { line, more }`
  making "a count with no line" unrepresentable, the held tally riding
  the completion via `last_look`.
- Four paused-clock tests drive the window with `tokio::time::advance`;
  nothing waits wall-clock. Deviations accepted on review: the mode
  carries `held: Option<Notice>` rather than two drifting fields, and a
  post-window wake shows the newest match of its scan window with the
  older ones counted.
- `bingo-tool-bash` 133 tests; the three black-box scenarios gate their
  determinism on files the test creates, never on scan-tick races. The
  worktree's own gate run was fully green; the integrated run's table
  sits in M22's plan.

## Carried

- A gone session's "nobody was told…" note lands in the job's own log; an
  ongoing pattern that happens to match that text re-matches it, bounded
  to one further note per quiet window. Pre-existing in shape; left.
