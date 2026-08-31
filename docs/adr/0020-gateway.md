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
   marked as what `start` launches. **`install`, `start` and `restart`
   preflight the channels first** (user-directed 2026-09-01): unparseable
   settings, no configured channel, or a channel that cannot sign is refused
   with the doctor's own lines — a configuration that cannot run is never
   handed to a supervisor to crash-loop under KeepAlive.
3. **The pidfile** `<data_dir>/gateway/gateway.pid` holds pid, binary version
   and start time; written with `create_new` (the channels claim shape), given
   back on drop. Liveness is probed with `kill -0 <pid>` as a subprocess —
   `libc` and `unsafe` stay banned. A dead pid in the file is reported, never
   trusted; `run` finding one replaces it with a log note — a supervisor's
   respawn after a crash must not wedge on the corpse's file.
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
7. **Installation (user-directed 2026-08-31).** `install` writes a per-user
   service for the default data dir and loads it: macOS
   `~/Library/LaunchAgents/com.bingo.gateway.plist` via `launchctl bootstrap
   gui/$UID`; Linux `~/.config/systemd/user/bingo-gateway.service` via
   `systemctl --user enable --now`. The service runs `<current exe> gateway
   run`, keeps it alive (`KeepAlive` / `Restart=on-failure`), and carries
   **no secrets**. `uninstall` unloads and removes it. While installed, the
   verbs delegate to the supervisor — start = load, stop = unload, restart =
   `launchctl kickstart -k` / `systemctl --user restart` — so launchd and a
   hand-spawned process never fight over one pidfile; `status` and `doctor`
   name which mode is in force. launchd refuses to bootstrap a service it
   already holds (error 5), and `install` loads it — so a `start` that finds
   the service loaded kicks it (`launchctl kickstart`) instead of
   bootstrapping twice; `systemctl start` is idempotent and needs no probe.
8. **Secrets get a disk home.** A boot-started gateway has no exported env,
   so a channel secret may live in `auth.json` (0600, ADR-0012's store)
   under `channels.<id>`, written by `bingo channels secret <id>` — hidden
   paste, the M15 pattern. The env variable, when set, still wins; `doctor`
   knows both sources and says which one the running mode actually sees.

## Consequences

- Schedules fire while the terminal is closed: the resident process holds the
  runner claim by arriving first. ADR-0019 is unchanged — schedules still
  *require* no daemon; they benefit from one that exists for channels.
- One gateway per data dir by pidfile; per-credential channel locks still
  guard against a second listener from any process.
- A Linux user unit stops at logout unless lingering is on; `doctor` has a
  row for `loginctl enable-linger` and says the command rather than running
  it.
- Windows is out of scope, as everywhere in this tree.
- Deeper `status` (which channels are connected right now) needs a control
  socket the process answers on; deferred until wanted — `doctor` stays
  static, `status` stays process-level, and neither pretends otherwise.

Refs: ADR-0012, ADR-0016, ADR-0019. Non-goals: system-level (root) services,
credential vaulting, a control socket, multi-gateway coordination, installing
for a non-default data dir.
