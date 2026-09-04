# M66 — The wake the model sets

## Goal

User, 2026-09-05: Claude Code's `/loop` lets the model pace itself —
at the end of a turn it says "wake me in N seconds with this
prompt", polls whatever it is waiting for, and stops when the goal
is met. bingo's schedules (ADR-0019) are the person's: `every`,
`daily at`, `once at`, each firing on a session of its own. Wanted:
**a mechanism the model calls itself**, on its own session, so a
turn can hand work to a later turn of the same conversation.

## Bricks

1. **`Wake` tool** in `bingo-schedule`: `{ after: "<n>s|m|h", note:
   string, stop?: bool }`. `after` is clamped to `[WAKE_LEAST,
   WAKE_MOST]` (10 s, 1 h; named constants, the clamp said in the
   result); `note` is what the next turn opens with — the model's
   own words to itself, delivered as a person-role line marked as a
   wake (`Origin` says `wake`, so the transcript row wears a dim `wake`
   label in the gutter where `>` would be — no emoji, the theme's
   own glyph table as everywhere); `stop: true` cancels the pending wake and schedules
   nothing. **One pending wake per session**: a second call replaces
   the first. The wake fires only after the turn that set it has
   ended, and it fires on **this** session (a schedule entry with
   `session: Some(id)` instead of one of its own — extend `Entry`
   with that field, the runner honours it, the file format gains one
   optional key). A session that is busy when the wake comes due
   waits for the barrier as any delivery does.
2. **The discipline in the description.** The tool's model-facing
   text teaches what the harness's own loop rules say: decide the
   evidence of success, the budget and the stop condition before the
   first wake; every wake is bounded and idempotent; a failed check
   goes back to diagnosis, not a shorter interval; when the budget is
   spent, stop and report. Snapshot the text.
3. **The person sees it and can end it.** The status line shows
   `wake in 4m` while a wake is pending (derived from the schedules list,
   nothing stored twice); `/wake` bare shows the pending note and
   when; `/wake off` cancels. `esc` during the woken turn drops it as
   any turn.
4. **Bounds a person sets.** `schedule.wakes = false` in settings
   refuses the tool with a message; a wake never outlives the session
   (a closed session's pending wake is forgotten on close).

## Files

`bingo-schedule/src/{tools/wake.rs (new), tools.rs, entry.rs,
runner.rs, schedules.rs, command.rs, render.rs}`, `bingo-sdk` if
`Origin` needs a `wake` spelling (check what `Origin` carries), the
TUI status line + transcript row for the origin, ADR-0019 dated
amendment (a fourth form: the model's own `once`, bound to a
session), `docs/design/tui.md` dated line.

## Exit criteria

- [x] `Wake` schedules one entry on the calling session; a second
      call replaces it; `stop` cancels; clamp said.
- [x] The runner delivers the note to the same session after the
      turn ends (integration on the fake provider with an injected
      clock).
- [x] Status line `wake in …` and `/wake off` (snapshot + test).
- [x] The description snapshot.
- [x] All gates; the file format fixture for the new key.
- [ ] Hands-on: appended by the parent.

## Non-goals

Cron; a wake on another session (that is `SendMessage` + schedules);
a wake that survives the session; a per-wake token budget enforced
by the kernel (the discipline is the model's, the person's `esc` and
`/wake off` are the bound).

## Risks

A model that wakes itself forever: `WAKE_MOST` bounds the interval,
not the count — the status line makes it visible and `/wake off`
ends it; say so in the ADR and do not build a counter until someone
needs one. Delivery to a session whose surface is not attached (a
TUI that closed): the entry fires into the journal as schedules do
today; check what the runner does for a session nobody holds.

## Verified

2026-09-04. Four bricks; `Origin` needed no new spelling — `surface:
"schedule"` already says a schedule delivered a turn, and a wake wears
`surface: "wake"` beside it, so `bingo-sdk` is untouched.

The shape, in one line each: `Wake{after?, note?, stop?}` (both optional
so `{"stop": true}` is a call); the entry file's one new optional key is
`session`; the runner branches on it and delivers `Delivery::Wake` to
that `SessionId` instead of opening `schedule/<id>`; the status line
says `wake in 4m` from `extensions["bingo.schedule"]["wake"].at`.

```
$ cargo fmt --all -- --check          # silent
$ cargo check --workspace --all-targets --locked -j 2
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 31.10s
$ cargo clippy --workspace --all-targets --locked -j 2 -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.06s
$ cargo test --workspace --locked -j 2 --no-fail-fast
passed: 4002 failed: 0        # 83 binaries, twice, both clean
$ scripts/check_discipline.sh
dependency direction ok / kernel names no tool / cohesion ok / discipline ok
$ scripts/budget.sh
dependencies (unique, normal): 333 (max 333) … budget ok
$ cargo deny check
advisories ok, bans ok, licenses ok, sources ok
$ cargo test -p bingo --test pty -j 2
test result: ok. 11 passed; 0 failed
$ cargo check -p bingo-schedule -p bingo-sdk --all-targets --locked -j 2 \
    --target x86_64-pc-windows-msvc
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 10.60s
```

The clock is a fixture, never a wait: a wake's entry is written `once at`
a moment already past (the store is hand-editable, ADR-0019 §1), so
`Runner::tick` fires it on the pass it is asked for. `cargo test -p bingo
--test cli -- schedule::` is 11 tests in 2.7s.

One thing seen once and not reproduced: a single unnamed failure in
`-p bingo --test cli` during one `--workspace` run under load. Three
`--test cli` runs and two whole-workspace runs afterwards were clean
(4002/4002 each), and the name was lost to the filter, so it is recorded
rather than explained.
