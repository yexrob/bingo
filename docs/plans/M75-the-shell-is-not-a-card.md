# M75 — The shell is not a card

## Goal

User, 2026-09-05, straight after M74: "jobs 也优化一下 … 现在是在右上角
很难看" — the running jobs, too; a card in the top-right corner is ugly.
Today `bingo-tool-bash` signals its running set as a `Table` (ADR-0018
§7) and the rail draws it as a card: `jobs` / `job  command…` /
`job_3fxcfqm4  npm run…`, three columns clipped into twenty-two cells.
Claude Code 2.1.261, driven in a harness-owned tmux and read frame by
frame (`scratchpad/j-*.txt`, `jstyled-*.txt`):

```text
⏺ Bash(sleep 45; echo finished)
  ⎿  Running in the background (↓ to manage)         ← dim; the same words after it ends
⏺ Started.
✻ Baked for 3s · done 10:22 AM · 1 shell still running
  ⏵⏵ bypass permissions on · 1 shell                  ← the count on the mode line, an accent
…
⏺ Background command "…" completed (exit code 0)      ← a turn the completion opened
```

No card anywhere: the row that started the shell says where it went,
the footer counts what is still running, and the completion is a turn.
bingo does the same in its own grammar — and, deriving the row from the
signal, says `Ran` once the shell has, which Claude Code's row never does.

## Bricks

1. **`bingo-tool-bash`**: the answer to a call that went to the
   background — by its flag, by the table of commands that never end,
   or by a person's `ctrl+b` — carries `display: View::Custom{kind:
   "job", data: {id, command}, fold: "Started in the background as
   job_…"}` (ADR-0038: the plugin names the element and writes the
   fold). The text the model reads is unchanged; `--print` and every
   surface that has not learned the kind show the fold.
2. **`shells.rs`** (new, the surface's whole contract with
   `bingo.tools.bash`, as `tasks.rs` is with `bingo.tasks`): `running
   (state)` reads the signal's table by its headers — the ids in the
   `job` column, a row that does not parse left out; `started(output)`
   is the id a call's display names; `row(id, running)` is `Running in
   the background` while the id is in the set and `Ran in the
   background` when it is not; `counted(state)` is `1 shell` / `2
   shells`; `is_set(plugin, kind)`.
3. **`transcript`**: `Rows` carries the ids the session still has
   running; a finished call whose output names a job draws `row` under
   its `⎿` instead of the folded display. **`status`**: the middle slot
   gains the count after `2 running`, dim — only while true.
4. **`rail`**: `cards` leaves the set out, as the panel sheet leaves the
   task list out.
5. **Screens**: `screens/shells.rs` — running (80×24; 120×40 with no
   card), ran — plus unit tests for every brick.
6. **Docs**: design §4 (`shell` row), §8 (the one signal the rail
   leaves out), a dated §10 line; ADR-0018 §7 amended.

## Files

`crates/bingo-tool-bash/src/{jobs.rs, lib.rs}`,
`crates/bingo-surface-tui/src/{shells.rs (new), transcript.rs,
status.rs, rail.rs, theme.rs, lib.rs, screens.rs, screens/shells.rs
(new)}`, their snapshots, `docs/design/tui.md`,
`docs/adr/0018-background-commands.md`.

## Exit criteria

- [x] A backgrounded `Bash` reads `⎿  Running in the background` under
      its row while the signal lists it and `⎿  Ran in the background`
      after; the status line says `1 shell` while it runs and nothing
      after (snapshots, both sizes).
- [x] At 120 columns no `jobs` card is drawn; the demo's progress card
      still is.
- [x] `--print` shows `Started in the background as job_…` under the
      call's verdict (the fold).
- [x] `cargo fmt/check/clippy/test`, `check_discipline.sh`, `budget.sh`
      pass; a hand drive in tmux on the real binary shows the row, the
      count and the completion turn.

## Non-goals

A page of shell details, or ending a shell from the surface —
`KillShell` is the model's verb and a person asks for it; the shells as
rows of the `↓` list; Claude Code's `✻ Baked for 3s · done 10:22 AM`
turn summary; the agent count on its footer.

## Risks

A finished row now changes after its item did (`Running` → `Ran`) —
the child row already does, read from the child's state. The signal is
gone on resume, so every job row reads `Ran` there, which is true: a
job lives exactly as long as the process (ADR-0018). The surface names
one signal kind and one custom kind, the contract shape `seats.rs` and
`tasks.rs` already carry.

## Verified

2026-09-05, Fable's own slice; no worker. Claude Code 2.1.261 driven in
a harness-owned tmux (`jobs.sh`, frames `j-*.txt`, styled
`jstyled-*.txt`): `⎿  Running in the background (↓ to manage)` in 246,
`· 1 shell` in 44 on the mode line, the same row after the shell ended,
the completion as `⏺ Background command "…" completed (exit code 0)`.

- [x] `screens::shells::{shell_running, shell_ran}` at 80×24 and 120×40,
      each read: `⎿  Running in the background` / `1 shell`, then `⎿  Ran
      in the background`, no count, the completion turn. Real binary on
      the fake provider in tmux (`drive2.sh`, frames `drive2/*.txt`):
      `Running in the background` + `⎿  allowed` + `1 shell` at 130 and
      at 80 columns; nine seconds later `Ran in the background`, the
      `⏺ Background job job_33wd9k1s (…) exited with code 0 after 8s.`
      turn, `? for shortcuts` back. The first drive caught the row
      *not* flipping: the block memo's revision did not know the set —
      `Revision.shell` and `blocks::tests::a_shell_that_ends_makes_its_
      block_draw_again` are the fix.
- [x] 120×40 draws no card and the transcript keeps all 120 columns
      (`shell_running_120x40`); `rail::tests::the_running_shells_are_
      the_one_signal_that_is_not_a_card` keeps the demo's progress card.
- [x] `--print --dangerously-skip-permissions` on the fake provider:
      `[tool] Bash ok (1ms)` / `  Started in the background as
      job_r6qk92rh` on stderr, `Started it.` alone on stdout.
- [x] Gates:

```text
== fmt        fmt exit 0
== check      Finished `dev` profile — check exit 0
== clippy     clippy exit 0 (-D warnings)
== test       cargo test --workspace --locked — test exit 0
              (bingo-tool-bash 134 passed; bingo-surface-tui 1035 passed, 2 ignored)
== discipline discipline ok (one pre-existing warn: core session.rs handle 72 lines)
== budget     budget ok — dependencies unchanged
== smoke      tui-smoke ok — 17 steps
```

Unverified: a promoted command (`ctrl+b`) was not driven by hand — it
takes the same `answered()` path as a flagged one; a session resumed
with `--continue` reads every job row as `Ran`, which is true and not
driven; Windows cross-check not run (nothing here touches a process, a
path or a clock). Hands-on by the user owed: the row and the count in
their own terminal.
