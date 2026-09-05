# M74 — The list under the verb

## Goal

User, 2026-09-05: "align the task display in the TUI with Claude Code;
the dedicated board is not intuitive." Today the list `bingo-tasks`
publishes is a row of the `ctrl+t` panel sheet, drawn as a generic
table, pinnable into the rail — a board a person has to go and open.
Claude Code 2.1.261, driven in a harness-owned tmux and read frame by
frame (`scratchpad/b-*.txt`, `c-*.txt`, `d-*.txt`, `styled-*.txt`):

```text
✻ Writing the plan… (9s · ↓ 652 tokens · thinking)     ← the verb is the task
  ⎿  ✔ Write the plan                                  ← green ✔, subject dim + struck
     ◼ Ship it                                         ← orange ◼, subject bold
     ◻ Celebrate                                       ← plain

⏺ Ok.                                                  ← after the turn:
  3 tasks (1 done, 1 in progress, 1 open)              ← dim, counts bold, where the verb row was
  ✔ Write the plan
  ◼ Ship it
  ◻ Celebrate
```

Twelve tasks draw five rows and `… +6 pending, 1 completed`; `ctrl+t`
hides the whole block and shows it again; the four task calls draw no
row in the transcript at all — the list is the row. bingo does the
same, in its own grammar, and the panel sheet loses the one kind the
band now draws.

## Bricks

1. **`theme`**: `Glyphs.task: [&str; 3]` — `◻ ◼ ✔` / ASCII `- * x` —
   behind `theme::tasks()`; `tasks.rs` joins the spending table for
   `text dim presence good`.
2. **`tasks.rs`** (new, the surface's whole contract with `bingo.tasks`,
   as `seats.rs` is with `bingo.rooms`): `of(state)` reads
   `extensions["bingo.tasks"]["tasks"]` as data — a record that is not a
   task is left out; `doing(&tasks)` is the first in-progress task's
   `activeForm`, else its subject; `summary(&tasks)`;
   `rows(&tasks, width)` — everything in list order when it fits in
   five rows, else the open ones in list order to five and one dim line
   counting the rest; `hung(rows)` under a `⎿`, `standing(rows)` at
   the transcript's indent; `is_list(plugin, kind)`; `quiet(item)` —
   one of the four calls, on the session's own list (no `in`), that did
   not come back wrong.
3. **`view`**: the band is air + one row + the list + the queue. The
   row is the verb row while the turn shows one, else the summary
   while there are tasks, else blank as today; the verb is `doing`,
   `Stopping` still outranks it. `Ui.tasks_hidden` keeps the rows and
   the summary off the band and nothing else.
4. **`transcript`**: a quiet call is no block — a failed one, or one
   sent to a board, draws as any tool row, because its effect is not
   on this screen.
5. **`panel`**: `rows` leaves the list out; `ctrl+p` opens the sheet.
   **`keys`/`input`**: `ctrl+t` toggles the list; the `?` table says so.
6. **Screens**: `screens/tasks.rs` — at work, between turns, cut to
   five, hidden, ASCII — plus unit tests for every brick above.
7. **Docs**: design §3 sketch, §4 rows (`activity row`, `tasks`,
   `the list`), §7, §8, a dated §10 line; the `ctrl+t` mentions in
   `panel.rs`, `views/mod.rs`, `window.rs`.

## Files

`crates/bingo-surface-tui/src/{tasks.rs (new), activity.rs (new: the
band, out of view.rs, which the list pushed past the 1000-line fail),
theme.rs, view.rs, transcript.rs, panel.rs, input.rs, keys.rs, ui.rs,
lib.rs, motion.rs, rail.rs (fixtures), screens.rs, screens/tasks.rs
(new), screens/windows.rs}`, their snapshots, `scripts/tui-smoke.sh`
(`C-p`), `docs/design/tui.md`. `bingo-tasks` is untouched: the payload
is already Claude Code's shape.

## Exit criteria

- [x] A turn with a task in progress reads `✻ <activeForm>…` with the
      list under a `⎿`; between turns the summary stands where the verb
      row stood, the rows under it (snapshots, both sizes).
- [x] Twelve tasks are five rows and `… +N pending, M completed`; a
      list that fits keeps its completed rows, struck.
- [x] `ctrl+t` hides and shows the block; `ctrl+p` opens the panel
      sheet, which lists no `bingo.tasks · tasks` row.
- [x] `TaskCreate/Update/Get/List` on the own list draw no row; one
      that failed or was sent `in #room` draws as before.
- [x] `cargo fmt/check/clippy/test`, `check_discipline.sh`,
      `budget.sh` pass; each new snapshot read, not accepted blind.

## Non-goals

A `View` node for checklists (one consumer; ADR-0038's custom kind
stays available); a room's board in the parent's band (the room's own
view shows it, the same code); wrapping a long subject (cut with `…`);
Claude Code's `/tasks` (bingo's stays the table).

## Risks

The band grows with the list, so a `TaskCreate` shifts the transcript
by a row, as it does in Claude Code; the cap of five bounds it. The
surface names four tool names and one kind — the same contract shape
`seats.rs` and `skill.rs` already carry, recorded in §10.

## Verified

2026-09-05, Fable's own slice; no worker. Claude Code 2.1.261 was driven
in a harness-owned tmux (`--model haiku`, socket `bingoprobe`) and its
frames kept in the session scratchpad; the styled frames gave the
weights (`✔` 38;5;114 + SGR 9 dim, `◼` 38;5;174 + bold, summary 246
with bold counts).

- [x] At work: `✻ Shipping it… (esc to interrupt` over `⎿  ✔ Write the
      plan` / `◼ Ship it` / `◻ Celebrate`; between turns `  3 tasks (1
      done, 1 in progress, 1 open)` in the slot, rows standing under it
      (`screens::tasks::{tasks_at_work,tasks_between_turns}`, 80×24 and
      120×40, each read). Real binary on the fake provider in tmux
      (`scratchpad/drive/*.txt`): `✢ Writing the plan… (esc to interrupt
      · 2s · ↓ 0.0k tokens)` with `◼ Write the plan` under the `⎿`, then
      `✻ Shipping it…` with `✔ Write the plan`, then the summary and the
      rows with no `TaskCreate`/`TaskUpdate` row in the transcript.
- [x] Twelve tasks: five rows and `… +6 pending, 1 completed`, the done
      one counted not drawn (`tasks_cut_to_five`); a list that fits keeps
      `✔` struck (`tasks::tests::a_list_that_fits_is_every_row_in_list_order`).
- [x] `ctrl+t` hides and shows the block (`input::tests::ctrl_t_hides_…`,
      `screens::tasks::tasks_hidden`, and in tmux); `ctrl+p` opens the
      sheet, which says `nothing to show` for a session whose only
      extension is the list (tmux frame `5-panel.txt`;
      `panel::tests::every_plugin_and_kind_is_a_row_of_its_own`).
- [x] The four calls draw no row; `TaskUpdate{id: 9}` came back
      `No task #9` and drew as a tool row with its error, in tmux and in
      `transcript::tests::a_task_call_draws_no_row_unless_…`; one sent
      `in #design` draws (same test).
- [x] Gates, final run after the `activity.rs` split:

```text
== fmt        fmt exit 0 (after `cargo fmt --all`; the check first caught
              one over-long tuple in theme.rs)
== check      Finished `dev` profile — check exit 0
== clippy     clippy exit 0 (-D warnings)
== test       cargo test --workspace --locked — test exit 0
              (bingo-surface-tui alone: 1024 passed; 0 failed; 2 ignored)
== discipline discipline ok  (view.rs 1044 → 787 non-test lines; was 992 at HEAD)
== budget     budget ok — dependencies 334 (max 334)
== smoke      tui-smoke ok — 17 steps, the panel step on C-p
```

Unverified: no real Claude Code session compared side by side with
bingo in the same terminal (frames only); a board `in #room` in a room's
own view is covered by `view::tests::a_room_s_board_is_drawn_…` and not
driven by hand; the help sheet at 80×30 now cuts `/clear` off its last
row (a key was added, the sheet does not scroll). Hands-on by the user
owed: the band in their own terminal and under tmux.
