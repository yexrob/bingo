# 0019 — Schedules: deferred and recurring turns

## Context

Neither the old project nor this one can defer or repeat work: no "run this every morning", no "try again at nine". The old `/tasks` command even advertised "background tasks" and returned an empty page — scaffolding for a scheduler nobody built. The pieces this tree has make one cheap: a session is durable and addressable by key, `HostApi::open` reopens or creates one, `deliver(Delivery::Wake)` opens a turn on it from any plugin, and the channels plugin already proved the one-runner-per-store lock. The kernel owns no feature nouns, so a schedule is a plugin's.

## Decision

1. **One plugin, `bingo-schedule`.** An entry is one JSON file under `<data_dir>/schedules/<id>.md`-less plain `<id>.json` — rebuilt on read, hand-editable, one entry per file (the experience store's discipline): `{spec, text, cwd, permission_mode?, enabled, created, last_fired}`. The id is minted, short, and is the filename.
2. **The spec is a small grammar, not cron**: `every <duration>` (`every 30m`, `every 2h`), `daily at <HH:MM>` (local time), `once at <RFC3339>`. Parsing and next-fire computation are one pure brick with its own tests (`jiff` is in the tree). Cron expressions are a later dependency the day someone needs one.
3. **A fire is a turn on the schedule's own session.** At fire time the runner opens-or-continues the session keyed `schedule/<id>` at the entry's `cwd` and delivers `text` — a prompt or a `/command` line — with `Delivery::Wake`. The transcript is the record: results, failures and costs land where every other turn's do, and `--resume` reads them. A `once at` entry disables itself after firing.
4. **Unattended is degraded, not stuck.** The scheduled session runs under the entry's `permission_mode` (default `default`); a question nobody answers is declined the way `--print` already declines them, and the turn continues or fails honestly. A person who wants hands-free runs writes `acceptEdits` or narrows with rules — their call, recorded in the entry.
5. **One runner per store.** `Plugin::start` spawns the timer loop only after taking a lock file under `<data_dir>/schedules/` (the channels credential-lock pattern); a second bingo process runs with schedules dormant and one notice saying who holds them. On start, an overdue entry fires **once**, never once per missed interval.
6. **Tools and a command**: `ScheduleCreate{spec, text, cwd?, permissionMode?}` (its `preview` shows the entry to be written — the card is the proposal), `ScheduleList`, `ScheduleForget{id}` (destructive; id prefixes accepted); `/schedule` (instant) renders the table — id, spec, next fire, enabled, text head.
7. **Schedules fire only while a bingo process runs** — the TUI, `serve`, or `bingo channels`. There is no OS daemon, no launchd unit, and no pretence otherwise: `/schedule` says "held by this process" or "dormant — no runner". Sleeping through a fire time is the overdue case of §5.

## Consequences

- New crate `bingo-schedule` (plugin tier): one workspace member, no external dependency; `scripts/budget.toml` moves by exactly one with a reason line.
- The kernel stays noun-free: `schedule/` is a session-key prefix minted by this plugin under the existing first-segment rule; everything else is `open` + `deliver`.
- A schedule's turns cost real tokens unattended. The entry's `text` and mode are shown at creation (the preview) and in `/schedule`, which is the audit surface; there is no cap on entries — `ScheduleForget` and the visible table are the discipline, as with the experience store.
- Restart behaviour is honest: entries persist, in-flight turns do not; the next process picks the store up through the lock.

## Supersedes

—
