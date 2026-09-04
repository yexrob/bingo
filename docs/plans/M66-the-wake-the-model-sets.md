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
   wake (`Origin` says `wake`, so the transcript shows `⏰ note` and
   not `>`); `stop: true` cancels the pending wake and schedules
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
   `⏰ 4m` while a wake is pending (derived from the schedules list,
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

- [ ] `Wake` schedules one entry on the calling session; a second
      call replaces it; `stop` cancels; clamp said.
- [ ] The runner delivers the note to the same session after the
      turn ends (integration on the fake provider with an injected
      clock).
- [ ] Status line `⏰` and `/wake off` (snapshot + test).
- [ ] The description snapshot.
- [ ] All gates; the file format fixture for the new key.
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
