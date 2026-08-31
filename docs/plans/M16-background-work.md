# M16 — Background work: jobs, promotion, schedules

## Goal

Async by default, whole in both directions (ADR-0018, ADR-0019): a shell command runs detached with its output pullable and its process killable; a running command is promoted with one key; commands that can never finish background themselves; completion wakes the owning session on every surface; and a schedule fires prompts as turns on its own durable session — `every 30m`, `daily at 09:00`, `once at …` — while any bingo process runs.

## Bricks, in build order

**Worker A — jobs (`bingo-tool-bash` + one TUI keybinding):**

1. **The job table** — one module owning the noun: minted id, `Child` + process group, log writer to `<data_dir>/bash/<id>.log` (size-capped, cap noted in-log), state (`Running/Exited{code}/Killed`), the reader task; jobs die with the process and that is documented on the type.
2. **Three verbs** — `Bash{background: true}` returns `{id, log}` at once (description leans async per ADR-0018 §1); `BashOutput{id, cursor?}` → chunk + cursor + state, read-only, `SelfBounded`, id prefixes accepted; `KillShell{id}` → TERM the group, KILL after a grace, report the exit.
3. **Deliver on exit and on condition** — `notify_on: [String]` / `notify_regex` watched by the reader; exit and hits send one concise notification via `cx.host.deliver(…, Delivery::Wake)` (the agents' followup door); growth never wakes. Notification text: command head, state, matched line if any, `BashOutput <id>` as the pointer.
4. **Auto-backgrounding** — the existing parse recognises `watch`, `tail -f`, unbounded `while`/`for`, trailing `&`: started backgrounded with a note, whatever the flag said.
5. **Promotion** — the in-flight foreground call selects on a promote flag beside the child; an `Input::Action{name: "bash.promote", args:{call}}` flips it; the process, pipes and buffer move to the table (no restart, foreground timeout dropped); the call returns early with the id. TUI: `ctrl+b` on the running block fires the action (keys.rs row + one handler; the rail already draws the plugin's signal).
6. **The live signal** — one `HostApi::signal` per change listing running jobs (id, head, age); `Null` when none.
7. **Black-box** — a background job's output pulled across two turns by cursor; kill ends it and the transcript says so; a `tail -f` is backgrounded unbidden; the completion notification wakes a `--print --input-format stream-json` host session (the headless wake the old project could not do); promotion mid-run via the action over RPC.

**Worker B — schedules (new `bingo-schedule`):**

8. **The spec brick** — pure parse + next-fire for the three forms, local-time `daily`, DST-sane via `jiff`; property tests around midnight and month ends.
9. **The store** — one JSON file per entry under `<data_dir>/schedules/`, minted short id = filename, unreadable entries surfaced not skipped; the runner lock file beside them (channels' pattern), overdue fires once.
10. **The runner** — `Plugin::start` takes the lock else notes dormancy; sleeps to the nearest next-fire; fire = `host.open(schedule/<id> at cwd)` + `deliver(text, Wake)`; `once at` disables itself; failures land in the schedule's own transcript.
11. **Tools + `/schedule`** — Create (preview = the entry file, the card is the proposal), List, Forget (destructive, prefix ok); `/schedule` instant → `View::Table` with id, spec, next fire, enabled, holder note.
12. **Black-box** — create → the file; a short `every` fires a real turn on `schedule/<id>` (fake provider) and the transcript holds it; a second process is dormant with the notice; overdue-once on restart; `/schedule` folds in `--print`.

## Files

A: `crates/bingo-tool-bash/src/{jobs,…}.rs`, `crates/bingo-surface-tui/src/{keys,input}.rs` (one bind + one handler), `crates/bingo/tests/cli/jobs.rs`. B: `crates/bingo-schedule/src/{lib,spec,store,runner,tools,command}.rs`, `crates/bingo/src/main.rs`, workspace/bin `Cargo.toml`, `scripts/budget.toml` (+1 member, reason line), `crates/bingo/tests/cli/schedule.rs`. AGENTS.md scopes gain `schedule`.

## Exit criteria

- [ ] a background job: start → two cursor pulls → kill; the log file holds everything, the cap is honest
- [ ] `tail -f` backgrounds itself with the note; a plain `cargo build` does not
- [ ] exit and `notify_regex` hits wake the owning session on a **headless** surface; growth does not
- [ ] `ctrl+b` (and the action over RPC) promotes a running command: same process, early return, job listed in the rail signal
- [ ] `every 45s` fires a real scripted turn on `schedule/<id>`; `once at` disables itself; overdue fires once after a restart
- [ ] a second process leaves schedules dormant with the notice; `/schedule` names the holder
- [ ] every gate green (fmt, check, clippy, test, discipline, budget 297→298 measured, deny, tui-smoke)

## Non-goals

Cron expressions; an OS daemon / launchd; jobs surviving the process (the log survives, the process does not); pausing/resuming jobs; per-job resource limits; a schedules UI beyond the table.

## Risks

R-wake — `deliver` opening turns from plugins is the load-bearing seam; if a session is closed the delivery must fail into the log, never panic a reader task. R-double-fire — the lock file is the only guard; the dormant path needs its test. R-clock — `daily at` across DST is why the spec brick is pure and property-tested. R-scope — promotion touches the executor's assumptions about a call returning; if the seam is missing in `bingo-tool-bash` alone, the worker reports rather than reaching into the kernel.
