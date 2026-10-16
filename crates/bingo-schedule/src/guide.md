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
- `text` — prose delivered to the model, including lines beginning with `/`
  or `!`. It reads nothing of this conversation, so write something that
  stands on its own and needs no answer.
- `cwd` — the directory that turn works in; this session's by default.
- `permissionMode` — `default`, `acceptEdits`, `plan`, `bypassPermissions` or
  `dontAsk` for the scheduled turn. A stored mode can only keep or tighten
  the session's permission policy; it cannot grant more authority. Configure
  any needed permissions yourself; a turn that asks can wait for a person.

The person sees the file the call would write before it is written, and the
receipt gives the id.

`ScheduleList` is every entry — `id`, `spec`, `next fire`, `enabled`, `by`,
`text` — and the line saying whether any process runs them. `ScheduleForget` deletes one
by a unique prefix of its id; turns it has already run are transcripts of their
own and are untouched. There is no cap, no expiry and no other pruning.

Every id is a unique prefix. One that names nothing, or several, comes back as
a sentence to act on rather than a failed call.

### Whose entry it is

The `by` column says who asked for the entry, and that decides what a fire
may do:

- `you` — written by the person's own `/schedule add` or made theirs by
  `/schedule confirm`. Its text runs as the person typed it: a `/command`
  line runs as that command, a `!` line runs in the shell, and its stored
  mode is applied as the person's own `/permission` would be, so it may
  widen the session's mode.
- `model` — written by `ScheduleCreate`. Its text is prose for the model,
  even a line that begins with `/` or `!`, and its mode only keeps or
  tightens.
- `unconfirmed` — an entry from before this column existed. It is treated
  like the model's until the person confirms it.

`/schedule add <spec> [as <mode>] -- <text>` writes an entry as the
person's, working in this session's directory: `/schedule add every 30m --
/wake`, `/schedule add daily at 09:00 as acceptEdits -- tidy the docs`.
`/schedule confirm <id> [digest]` makes an entry the person has read theirs.
The table offers a digest that binds its exact text and mode; a changed entry
is refused. The panel supplies it automatically. Without it, the typed verb
confirms the current file. Control and bidi characters are shown as escapes;
the panel requires typed confirmation for such text. Both verbs require the person's asked line. The built-in panel can ask the client
to submit a line after the person confirms; a model, a mod acting alone,
an arbitrary button or a chat correspondent cannot authorize it.

A fire that is refused — a command line or a wider mode on an entry that is
not the person's — is never silent. The `/schedule` listing names the entry
and what was refused under its table, and the first such refusal in a
process also raises a warning notice. An unconfirmed command line reaches the model as prose. A rejected
command belonging to the person remains unspent and reports its refusal.

### What a fire is

The entry's session is opened by the key `schedule/<id>` — one key per entry,
so every one of its turns lands in one transcript that `--resume` reads like
any other. The clock moves only after delivery is acknowledged. A failed attempt
remains due and retries after a rescan; each entry has its own bounded
attempt, so a stalled entry does not hold up the others.

A missed window is **one** fire, not the backlog: `every` counts from the last
fire, so a process that was down for three intervals owes one turn. A `once at`
is spent by firing and leaves the entry disabled.

### Who runs them

One runner per store, holding an OS file lock on `runner.lock` in the schedule
directory. The file is permanent, not a PID sentinel: **never remove or replace
it while modern bingo processes are running**. Exiting, even after a crash,
releases the OS lock. Other processes stand by and retry once a second, so an
already-running process takes over without a restart. Shutdown waits for any
dispatch in flight before releasing ownership.

The holder line distinguishes this process, another runner, an unowned store,
and a storage error. It does not infer ownership from a PID. A free modern lock
file needs no cleanup, and `bingo gateway doctor --fix` leaves it alone.

An older PID-only, empty, or unrecognized `runner.lock` is a migration barrier,
not permission to start a second scheduler. Stop **all** bingo processes sharing
this store, run a single `bingo gateway doctor --fix` to remove a dead PID
sentinel, then start the upgraded binaries. Do not run concurrent repair commands:
an older doctor may still have a cached decision to delete the old path. Inspect
other legacy files before removing them once. Modern files carry a nonnumeric
version marker, so an older doctor reading one will not mistake it for a dead PID
sentinel. A downgrade likewise requires stopping every modern runner before
removing its marker; never mix the old and new lock protocols.

`/schedule` (`/schedules`) opens the built-in panel. With the mod disabled,
it shows the table, its holder line, and any file that could not be read.

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

The terminal `/schedule` panel includes the pending wake; `/wake` selects it.
Text clients keep the wake time and note. `/wake off` ends it at any time,
including while the turn that set it is still running.

```toml
[schedule]
wakes = false
```

turns wakes off: the `Wake` tool is then not offered at all, and the schedules
are unaffected. `wakes` is true until a person says otherwise, and a typo in
the block is a startup failure rather than a silence.

## Managing schedules

`/schedule add every 2h -- review the build` creates a schedule with this
session's working directory and no stored mode. Add `as <mode>` before
` -- ` to store an explicit mode. The other forms are
`daily at HH:MM` and `once at <RFC3339>`; ` -- ` separates spec and text.
`/schedule pause <id>` disables it; `/schedule resume <id>` enables it.
`/schedule run <id>` fires now, including from a dormant process, and refuses
a disabled entry: resume first. A fire in another process may add one extra turn.
`/schedule forget <id>` removes the entry while keeping its session. Unique
id prefixes work; ambiguous prefixes are refused. All these lines require a
person's input; a model or a mod cannot run them on its own.

With the built-in schedules mod on, the bare command opens a panel. With it
off, the same table and lines remain. `/status` shows a pending wake counting
down; Enter on that row prefills `/wake off` for you to send.
