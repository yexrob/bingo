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

- [x] a background job: start → two cursor pulls → kill; the log file holds everything, the cap is honest
- [x] `tail -f` backgrounds itself with the note; a plain `cargo build` does not
- [x] exit and `notify_regex` hits wake the owning session on a **headless** surface; growth does not
- [x] `ctrl+b` (and the action, as `/bash.promote` — same kernel door) promotes a running command: same process, early return, job listed in the rail signal
- [x] `every 45s` fires a real scripted turn on `schedule/<id>`; `once at` disables itself; overdue fires once after a restart
- [x] a second process leaves schedules dormant with the notice; `/schedule` names the holder
- [x] every gate green (fmt, check, clippy, test, discipline, budget 297→298 measured, deny, tui-smoke)

## Non-goals

Cron expressions; an OS daemon / launchd; jobs surviving the process (the log survives, the process does not); pausing/resuming jobs; per-job resource limits; a schedules UI beyond the table.

## Risks

R-wake — `deliver` opening turns from plugins is the load-bearing seam; if a session is closed the delivery must fail into the log, never panic a reader task. R-double-fire — the lock file is the only guard; the dormant path needs its test. R-clock — `daily at` across DST is why the spec brick is pure and property-tested. R-scope — promotion touches the executor's assumptions about a call returning; if the seam is missing in `bingo-tool-bash` alone, the worker reports rather than reaching into the kernel.

## Verified (2026-08-31)

- Worker A merged `3cd2e57` (bricks 1–7, 4 commits); worker B merged `ef89ae4` (bricks 8–12,
  7 commits). Both textual merges clean; `tests/cli/main.rs` unioned.
- Integrated gates on main after both merges, quiet machine (1-min load 5.9):
  fmt / check / clippy / test / discipline / budget / deny — `GATES_EXIT=0`,
  **2407 tests passed, 0 failed**, `dependencies (unique, normal): 298 (max 298)`,
  `discipline ok`, `advisories ok, bans ok, licenses ok, sources ok`.
- Black-box: `tests/cli/jobs.rs` 5 scenarios (cursor pulls across turns + kill; tail -f
  auto-backgrounds, plain command does not; headless stream-json wake — a second `result`
  line with no further input; mid-turn promotion returns the call early; bad id is a
  correctable error result). `tests/cli/schedule.rs` 7 scenarios (entry file readable;
  `/schedule` folds under --print; a real fired turn on `schedule/<id>`; overdue-once;
  once-at self-disables; per-entry permission mode — mutation-checked by removing the mode
  and watching the test time out; dormant second process names the holder).
- R-wake held: a gone session lands in the job's log, never a panic. R-double-fire has its
  dormant test. R-scope resolved without kernel changes (early return from `Tool::call`).
- Taste calls reviewed and kept: the rail card carries `since`, not a frozen `age` (one
  fact, one representation); job ids are `job_`-prefixed; `every` has no `d` unit (a day is
  civil — `daily at` owns it); `ls | tail -f` is NOT auto-backgrounded (the table fails
  open — backgrounding something a caller meant to wait for is the worse surprise).
- Beside the merge: `7489c5f`+`caab444` fixed `tests/plugin_rpc.rs` — the sixth test file
  leaning on the fake-provider fallback f251b1f deleted (found by worker B, confirmed by
  worker A); `6818fff` added `schedule` to the discipline noun list + AGENTS/ARCHITECTURE.

## Carried

- **No tracing subscriber exists in the tree**: every `tracing::warn!` (jobs, schedule,
  channels, core) is a no-op. Both workers routed failures to other doors (the job's log;
  `/schedule`'s trouble line). The real fix is a subscriber at the binary edge, or the
  `HostApi::notice` owed since M14.
- `SessionSpec` carries no permission mode; the schedule runner submits `/permission <mode>`
  on the attachment before delivering. Safe direction, but the lose-the-race path has no
  test. Wished: `SessionSpec.permission_mode`, or the policy as a `service:` key.
- `HostApi::deliver` never parses commands (`session/inputs.rs:184` routes to prose): a
  schedule cannot run a `/command` yet; deciding whether it should is a design line owed.
- No open-or-create selector: the runner hand-rolls ByKey → Create; every keyed plugin will.
- `kill -9` leaves `runner.lock` and later processes stay dormant (the line names the pid
  and the file). An sdk advisory-lock primitive would fix schedule and channels together.
- No single test drives ctrl+b from a real terminal to the plugin (two halves covered);
  `input.rs` at 885 non-test lines against the 1000 fail line.
