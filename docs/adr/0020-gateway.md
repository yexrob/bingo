# ADR-0020 — The gateway: a resident bingo, managed like a service

Status: accepted · 2026-08-31 · Plan: M17

## Context

An IM channel is an inbound door: people write at any hour, the Feishu
long-connection has no replay (ADR-0016 §6), and a dead process is a missed
conversation. `bingo channels` runs that door in the foreground and dies with
the terminal. Schedules (ADR-0019) chose "no daemon" deliberately — a missed
fire degrades gracefully into one overdue fire — and that ruling stands; an
inbound door has no such grace. Hermes-class gateways (`start stop restart
doctor`) are the settled UX for this.

## Decision

`bingo gateway <verb>` manages one resident bingo process per data dir.

1. **The gateway is a whole host, not a bridge.** `gateway run` assembles the
   ordinary plugin host on the existing `Work::Channels` path (headless, the
   channels surface listening). Sessions, transcripts, schedules and locks are
   the normal ones in the normal places. Nothing is proxied.
2. **Verbs.** `start` spawns `bingo gateway run` detached (process-wrap's
   `ProcessSession`, already in the tree — no `unsafe`, no new dependency),
   stdio to `<data_dir>/gateway/gateway.log`, and waits for the pidfile.
   `stop` sends TERM and waits for the pid to leave. `restart` = stop, start.
   `status` reports pid liveness, version, uptime, log path. `logs` prints the
   log path and its tail. `doctor` diagnoses (below). `run` is public but
   marked as what `start` launches.
3. **The pidfile** `<data_dir>/gateway/gateway.pid` holds pid, binary version
   and start time; written with `create_new` (the channels claim shape), given
   back on drop. Liveness is probed with `kill -0 <pid>` as a subprocess —
   `libc` and `unsafe` stay banned. A dead pid in the file is reported, never
   trusted.
4. **Graceful end.** `run` handles TERM: surfaces stop, `Plugin::stop` runs
   (the schedule runner and channel locks are given back), then exit 0.
5. **`doctor`** checks, read-only: settings parse; each configured channel's
   credential present (env/auth.json — named, never printed); the pidfile and
   every known lock (`gateway.pid`, `schedules/runner.lock`, the channels
   credential locks) against a live pid; running version vs binary version;
   log writable. `doctor --fix` removes locks whose pid is dead, and only
   those.
6. **The gateway log is a real tracing sink.** `run` installs a subscriber
   writing to the log file — the first process in the tree where `warn!`
   lands somewhere (the M16 carried item). Dependency rule: try
   `tracing-subscriber` (`default-features = false, features = ["fmt"]`),
   measure with `scripts/budget.sh`; if it costs more than +5 unique crates,
   hand-roll a line-writing `Subscriber` instead and record that.

## Consequences

- Schedules fire while the terminal is closed: the resident process holds the
  runner claim by arriving first. ADR-0019 is unchanged — schedules still
  *require* no daemon; they benefit from one that exists for channels.
- One gateway per data dir by pidfile; per-credential channel locks still
  guard against a second listener from any process.
- Boot persistence (launchd/systemd installation) is v2: a daemon started
  from a shell inherits the credentials in that shell's environment; an
  installed service needs its own credential story first.
- Windows is out of scope, as everywhere in this tree.
- Deeper `status` (which channels are connected right now) needs a control
  socket the process answers on; deferred until wanted — `doctor` stays
  static, `status` stays process-level, and neither pretends otherwise.

Refs: ADR-0016, ADR-0019. Non-goals: OS service files, credential vaulting,
a control socket, multi-gateway coordination.
