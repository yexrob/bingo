# M96 — The window follows the stream

## Goal

User, 2026-09-14, session `notify`: `tail -F` a log with `notify_all`
and `notify_regex: ".+"`, append a line, and nothing arrives. The line
had landed inside the thirty-second quiet window of the previous notice,
and M21 chose no trailing flush: a held line only rides the next hit or
the job's end. For a job that never ends and a stream that has gone
quiet, that is never. Every line in that session waited 30 s to 2 min
and was reported only because another line happened to follow it. The
model, reading "at most once every thirty seconds", told the person the
delay was bounded by thirty seconds. It was not bounded at all.

The user's ask: no fixed thirty seconds; real time while the stream is
slow; back off when it floods; recover when it calms; and every notice
carries when it arrived and how often lines are coming, so the model can
perceive the pace and pin a window of its own when it wants one.

After this milestone: the quiet window starts at zero and follows the
observed rate — each line per second that arrived while it was quiet
buys two seconds of quiet, capped at thirty; a held line goes out when
the window ends, so the delay is bounded by the window it names; every
notice says the clock time, the lines and seconds since the last one,
and how soon the next can come; `notify_quiet` pins the window when the
model would rather choose. M21's non-goals "a trailing flush timer" and
"per-call window lengths" are superseded here.

## Bricks, in build order

1. `supervise.rs` — `next_window(lines, since) -> Duration`: pure.
   One line is not a rate: `lines ≤ 1` is zero. Otherwise
   `PACE × (lines − 1) / since`, `since` no shorter than a scan tick,
   capped at `CAP` (30 s). Table test.
2. `supervise.rs` — `Quiet::{Adaptive(Duration), Pinned(Duration)}`
   inside `Mode::All`; the scan keeps its start `Instant`. A tick with
   something to say — a fresh hit, or a held notice — wakes when the
   window since the last wake has passed (the first wake always), else
   holds. A wake observes `(more + 1, since)` into an adaptive window
   and leaves a pinned one alone. Paused-clock tests: a slow stream
   wakes on every line; a burst grows the window and the window's end
   flushes what it held; a pinned window holds a burst and flushes at
   its end; the held count still rides the job's end.
3. `notify.rs` — `Wake { notice, cadence: Option<Cadence> }`,
   `Cadence { since, window }`, `clock()` for the stamp. `matched` says
   the clock time, "…and N more lines matched since the last notice,
   12 s ago", and either "the next notice comes no sooner than 8 s after
   this one" or "the next matching line wakes you at once"; `finished`
   says its clock time too. Tests on the text.
4. `lib.rs` — `notify_quiet: Option<u64>` (milliseconds) on `BashArgs`,
   into `Conditions`; given without `notify_all` it is refused in words,
   the `notify_all`-without-conditions precedent. The description and
   the field docs say what the window does now. Schema test gains the
   field; a refusal test.
5. ADR-0018 §8 rewritten and dated.
6. Black-box (`tests/cli/jobs.rs`): the burst scenario pins its window
   (`notify_quiet`) so it stays deterministic against the scan tick and
   now also covers the pin; a new gated run has two lines wake two turns
   under the default.

## Files

- `crates/bingo-tool-bash/src/{supervise.rs,notify.rs,lib.rs}`
- `crates/bingo/tests/cli/jobs.rs`
- `docs/adr/0018-background-commands.md`

## Exit criteria

- [x] paused clock: a line every 10 s wakes every time; three lines in a
      tick grow the window; the window's end flushes the held line;
      `notify_quiet` pins.
- [x] `matched` names the clock time, the span and the next window.
- [x] `notify_quiet` without `notify_all` is an error result naming both.
- [x] black-box: two gated lines are two wakes under the default; the
      pinned burst is one count on the completion.
- [x] every gate green (fmt, check, clippy, test, discipline, budget).

## Non-goals

- Changing a running job's window: `notify_quiet` is set when the job
  starts. A job that never ends is a second's `KillShell` and restart;
  a verb that reaches into a running scan waits for a case the
  adaptive window does not cover.
- A clock time on every input the kernel delivers: the bash notice says
  its own time; the kernel's rendering of inputs is another ADR.
- Coalescing wakes in the kernel's queue: the scan folds lines into one
  notice per tick already, and the window bounds the rest.

## Risks

- R-rate: two lines in one tick read as eight a second and buy 16 s of
  quiet; the flush at the window's end bounds the harm, and the next
  single-line notice resets to zero.
- R-turns: a stream at one line a second under the default is a wake a
  second. That is what the model asked for with `notify_all`, the notice
  says so in its cadence line, and `notify_quiet` is the way out.
- R-black-box: the M21 burst scenario raced the scan tick under a
  zero window; pinning it is the fix, not a wall-clock wait.

## Verified (2026-09-14)

- `cargo test -p bingo-tool-bash --locked`: 145 passed, 0 failed —
  `next_window` table; a line every 10 s wakes every time; three lines
  in a tick buy 16 s and the window's end flushes the held line at
  `since: 16 s, window: 0`; the pinned window holds a burst and flushes
  it at 30 s; the held count rides the job's end; `notify_quiet`
  without `notify_all` is an error result naming both.
- `cargo test -p bingo --test cli -- jobs::`: 11 passed — two gated
  lines are two wakes and "The next matching line wakes you at once";
  the pinned burst is "…and 2 more" on the completion and the notice
  says "no sooner than 1m 0s".
- `cargo fmt --check`, `cargo check --workspace --all-targets`,
  `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test --workspace --locked`: 89 suites ok, 0 failed.
- `scripts/check_discipline.sh`: discipline ok (plan-length warnings are
  earlier milestones'). `scripts/budget.sh`: budget ok, no new
  dependency (`jiff` was already the job clock).
