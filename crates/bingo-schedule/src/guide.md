# Schedule

Two different things live here. A **schedule** is a file that fires a turn on
a session of its own, later or over and over. A **wake** is this session
handing work to a later turn of itself. A schedule is written down and outlives
the process; a wake is held in memory and does not.

Neither is a daemon. **Schedules fire only while a bingo process is running**,
and every listing says whether one is. A resident `bingo gateway` process is
the usual way to have one.

## Schedules

`ScheduleCreate` writes one entry:

- `spec` — `every <n>s|m|h`, `daily at HH:MM` in the machine's own zone, or
  `once at <RFC3339>`. Days are not a unit of `every` — a day is what DST
  makes longer or shorter, and `daily at` is where it belongs. **Cron
  expressions are not a schedule here.**
- `text` — a prompt, or a `/command` line, delivered as the turn. It reads
  nothing of this conversation and nobody is watching it, so write one that
  stands on its own and needs no answer.
- `cwd` — the directory that turn works in; this session's by default.
- `permissionMode` — `default`, `acceptEdits`, `plan`, `bypassPermissions` or
  `dontAsk` for the scheduled turn. Nobody is there to answer a prompt, so
  `default` declines whatever would have asked.

The person sees the file the call would write before it is written, and the
receipt gives the id.

`ScheduleList` is every entry — `id`, `spec`, `next fire`, `enabled`, `text` —
and the line saying whether any process runs them. `ScheduleForget` deletes one
by a unique prefix of its id; turns it has already run are transcripts of their
own and are untouched. There is no cap, no expiry and no other pruning.

Every id is a unique prefix. One that names nothing, or several, comes back as
a sentence to act on rather than a failed call.

### What a fire is

The entry's session is opened by the key `schedule/<id>` — one key per entry,
so every one of its turns lands in one transcript that `--resume` reads like
any other. The clock moves before the turn is asked for, so a session that
cannot be opened costs one occurrence, not a retry every pass.

A missed window is **one** fire, not the backlog: `every` counts from the last
fire, so a process that was down for three intervals owes one turn. A `once at`
is spent by firing and leaves the entry disabled.

### Who runs them

One runner per store, claimed by `runner.lock` in the schedule directory. A
second process over the same store leaves the schedules dormant and names the
pid holding them rather than firing everything twice. The claim is proof a
process took the store, not proof it still runs: a bingo that was killed leaves
the file behind for a person to remove.

`/schedule` (`/schedules`) is the same table for a person, with that holder
line under it and any file that could not be read named.

## Wakes

`Wake` sets the one wake this session has:

- `after` — how long to wait: `30s`, `5m`, `1h`. Held between 10 seconds and
  one hour; anything outside is clamped and the answer says so. A model that
  wants tomorrow wants `ScheduleCreate`.
- `note` — the line the next turn opens with. Nothing else of this turn is
  repeated for you, so write what you will need.
- `stop: true` — cancel the wake that stands and set none.

One wake stands at a time: setting another replaces it. It arrives only once
this turn has ended, on this same session, so finish what you are doing. It is
held by the process running the session and never written to the store, so it
does not survive a restart.

Before the first wake, decide three things and put them in the note: what
evidence would prove the work done, what the budget is (how many wakes, or by
when), and what happens when it is spent. Every wake is bounded and
idempotent — it checks and reports, it does not start the work again. A check
that fails goes back to diagnosis, not to a shorter interval. When the evidence
is there or the budget is gone, say what you found and set no further wake.

`/wake` shows the person when it comes and what it will say; `/wake off` ends
it, at any time, including while the turn that set it is still running.

```jsonc
{ "schedule": { "wakes": false } }
```

turns wakes off: the `Wake` tool is then not offered at all, and the schedules
are unaffected. `wakes` is true until a person says otherwise, and a typo in
the block is a startup failure rather than a silence.
