# M17 — The gateway (ADR-0020)

## Goal

`bingo gateway start|stop|restart|status|logs|doctor|run`: one resident bingo
per data dir, detached from the terminal, running the channels surface
headless with schedules riding along; a pidfile in the channels-claim shape;
a doctor that reads the locks and says what a person must do; and the first
real tracing sink in the tree.

## Bricks, in build order

1. **The pidfile brick** — pure: render/parse `{pid, version, started}` +
   `create_new` claim with Drop give-back (`gateway/pidfile.rs`); liveness by
   `kill -0` subprocess behind a small probe trait so tests fake it.
2. **`run`** — assemble the host exactly as `Work::Channels` does, write the
   pidfile, install the tracing subscriber to `gateway.log` (ADR-0020 §6
   dependency rule: measure `tracing-subscriber` fmt-only; >+5 unique crates
   → hand-roll), TERM → stop surfaces → `Plugin::stop` → exit 0.
3. **`start` / `stop` / `restart` / `status` / `logs`** — start spawns `run`
   detached (process-wrap `ProcessSession`, stdio to the log), waits bounded
   for the pidfile; stop TERMs and waits bounded for it to clear; status and
   logs read the pidfile and the file tail; every verb's words name the file
   they read.
4. **`doctor` (+ `--fix`)** — the checks of ADR-0020 §5 as a table of named
   rows (check, verdict, what to do); `--fix` removes exactly the dead-pid
   locks it just reported. Secrets never printed — names and locations only.
5. **Black-box** (`crates/bingo/tests/cli/gateway.rs`, temp HOME) — start →
   status alive → stop → status gone, pidfile cleaned; a second start while
   one runs refuses naming the pid; a stale pidfile is reported dead and
   doctor --fix clears it; a schedule entry fires under the gateway (fake
   provider, the M16 schedule test pattern); doctor names a missing
   credential without printing any.
6. **`install` / `uninstall` + mode-aware verbs (ADR-0020 §7)** — render the
   plist/unit (current exe, `gateway run`, keep-alive, log paths, no
   secrets) for the platform, load/unload through the supervisor; while
   installed, start/stop/restart delegate (`launchctl bootstrap/bootout/
   kickstart -k` on darwin, `systemctl --user` on linux). Hermetic tests:
   a fake `launchctl`/`systemctl` script first on PATH records its argv —
   no test touches the real supervisor; the rendered file is asserted
   byte-wise (exe path, label, no secret strings).
7. **The secret's disk home (ADR-0020 §8)** — in `bingo-channels`: the
   adapter's secret source becomes env-else-auth.json (`channels.<id>`,
   0600, ADR-0012 store); `bingo channels secret <id>` pastes it hidden
   (the M15 stty pattern, restore guard); doctor rows for source-in-force
   and (linux) lingering. `BINGO_FEISHU_APP_SECRET` behaviour with the var
   set is byte-identical to today.

## Files

`crates/bingo/src/main.rs` (subcommand), `crates/bingo/src/gateway/*.rs`,
`crates/bingo/tests/cli/gateway.rs`, possibly `Cargo.toml`/`scripts/budget.toml`
(+`tracing-subscriber`, measured, reason line); for brick 7 only,
`crates/bingo-channels` (secret source + the paste command). Nothing else.

## Exit criteria

- [x] start/stop/restart/status/logs round-trip on a real detached process
- [x] TERM is graceful: schedule runner claim and channel locks given back
- [x] doctor reports settings/credentials/locks/version; --fix removes only dead-pid locks
- [x] `warn!` from any plugin lands in `gateway.log` while the gateway runs — construction-evidenced: the bin's own WARN+INFO with targets pinned black-box; no plugin warn reachable without contriving one
- [x] a schedule fires with no terminal attached
- [x] install writes a secret-free plist/unit naming the current exe; verbs delegate while installed (PATH-shim proven); uninstall unloads and removes
- [x] a channel secret pasted via `bingo channels secret` lands in auth.json 0600; env still wins; doctor names the source in force
- [x] every gate green (fmt, check, clippy, test, discipline, budget measured, deny)

## Non-goals

launchd/systemd install; control socket; live per-channel status; Windows;
credential vaulting; log rotation beyond the size cap pattern.

## Risks

R-detach — daemonising without `unsafe` rests on process-wrap's session
wrapper; if stdio redirect fights it, the worker reports rather than adding
libc. R-liveness — `kill -0` races pid reuse; the pidfile's version+start
row is the tiebreak and stop refuses a pid whose start time disagrees.
R-env — credentials are captured at `start`; doctor must say which env vars
the run was and was not started with (names only). R-supervisor — launchd's
KeepAlive and a hand-spawned `run` are two masters; mode detection (is the
service file present and loaded) is the single switch, and the PATH-shim
tests pin every delegated argv.

## Verified (2026-09-01)

- Worker D merged `6ee08d5`: two commits — the gateway, and a separate
  four-fault fix for the live Feishu wedge (`finalize` decoupled the answer
  from the question it carried, so a refused card edit no longer drops a
  permission question and wedges the session; the inbox hand-off armed
  against cancel and the read deadline; HTTP timeouts 30s/10s; the dedupe
  ring outlives reconnects; a close frame before the socket drops — the
  connection budget's refusal is fatal). Both wedge regressions were
  revert-verified against the original code.
- Integrated gates on the quiet machine (1-min load 5.7): `GATES_EXIT=0`,
  **2509 tests passed, 0 failed**, `dependencies (unique, normal): 302
  (max 302)` — tracing-subscriber measured +4, inside §6's +5 rule —
  `discipline ok`, `advisories/bans/licenses/sources ok`.
- 10 black-box scenarios on temp HOMEs with a kill-on-drop guard (zero
  leaked processes): detached round-trip; graceful TERM with the schedule
  claim and pidfile given back; a stale pidfile reported dead and cleared
  by `--fix` alone; a schedule firing with no terminal; install/uninstall
  with byte-wise plist/unit assertions and PATH-shim `launchctl`/
  `systemctl` argv recordings; the secret paste to auth.json 0600 with env
  precedence.
- Taste calls reviewed and kept: mode switch = the service file's presence
  (install rolls the file back if the supervisor refused, so file-on-disk
  ⟺ loaded); the liveness tiebreak is `ps -o comm=` (start time is not
  portably readable) and stop refuses to signal a live pid that is not a
  bingo; stop-while-installed always tells the supervisor; SIGINT handled
  beside SIGTERM.

## Carried

- CardKit's 10-minute auto-close is survived, not detected: no text is
  lost, but a long streaming turn stops updating live. Treating a rejected
  sequence as recoverable (open a new card) is a design change owed.
- `Runner::asked` and `Deliverer::asked` are two representations of one
  open question — surfaced by the wedge diagnosis; the collapse is a real
  refactor, owed.
- Supervisor behaviour is PATH-shim-proven only; nothing ran against a
  real launchd/systemd. The live Feishu confirmation steps are with the
  user (secret → start → answer a permission → long turn → logs → doctor
  → stop).
- Wished seams: a plugin's credential wants as a registry contribution
  (doctor knows bingo-channels specifically today); `Plugin::claims()` for
  lock discovery (doctor finds locks by shape); `settings::load` returns
  the same file twice when cwd == $HOME (cosmetic, pre-existing).
