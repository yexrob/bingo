# M34 — The surface answers the person

## Goal

Six things a person felt at the keyboard on 2026-09-02, each traced
to its cause. After this milestone: the transcript does not shuffle
by a row for every block that arrives; a thought is readable where
it happened, and `/think` tells the truth when a model cannot; a
folded block opens on a click, whatever it holds; `/skill` reads as
the thing it is, and a skill knows its own directory; one `esc`
ends a tool instead of waiting for it; and a room's roster, its
posts and the message tools look designed, not dumped. The traces
that found each cause are the commit bodies.

## Bricks, in build order

Five slices, one worker each on its own branch; A merges before B.

**A — the transcript holds still** (`bingo-surface-tui`)

1. The "rise" (`blocks.rs:30-32, 242-260, 460-466`) appends two
   blank rows to the transcript's *end*; a bottom-anchored viewport
   turns that into the whole screen jumping up two rows and walking
   back, once per block. Delete the rise — §3's "nothing jumps"
   outranks §6's cue. `motion.rs`'s rise test goes with it; `tui.md`
   §6 and §10 record the reversal.
2. A wheel notch adds to the scroll's *target*; it never restarts the
   ease from the interpolated position (`scroll.rs:56-60`). Test: ten
   notches in 20 ms land ten notches away after one ease, not one.
3. Terminal events paint through the same 33 ms gate as frames
   (`run.rs:317`); a key still echoes on the next frame.

**B — a thought is readable, and the truth about it**

1. Inline thinking: `✻ Thinking…` while it lasts; done, the row is
   `✻ Thought for 2s` with the text under it, dim, folded at
   `OUTPUT_ROWS` like a result, `… +N lines (ctrl+o to expand)`.
   Empty text (redacted) draws the row alone and nothing to open.
   `blocks.rs:61` stops revising a block whose output cannot change
   while it streams.
2. `ctrl+o`'s `latest()` reaches a thought; `⏎` on a focused block
   still opens the pager.
3. **A click opens a folded block** — any fold: a result, a thought,
   a notice — and a second click folds it (`input.rs:150-163` sets
   focus today and stops). One key never means two directions;
   a click on the same row is one gesture that toggles.
4. `/think <level>` on a session whose model resolved
   `reasoning: false` (`host.rs:501`) answers
   `thinking: high — but <model> does not declare reasoning, so no
   turn asks for it; models.<model>.reasoning = true in settings says
   otherwise`. Bare `/think` says the same. The level is kept, so it
   takes effect when the model changes.

**C — a skill reads as a skill** (`bingo-core`, `bingo-skills`,
`bingo-surface-tui`, ADR-0008)

1. `inputs.rs:102-104`: a `Prompt` a command produced re-enters with
   `Origin { surface: "command", principal: Some(name), .. }` — not the
   surface's own. The intent is unchanged. ADR-0008 §3's "same intent
   and origin" becomes "same intent; the origin names the command".
   `mint_title` skips a command-originated user item.
2. `transcript.rs:228` adds `"command"` to the quiet set: the row is
   `⏺ /guide` with the body under `⎿`, folded — what `Skill(guide)`
   already looks like. `view.rs:434` follows. `rewind.rs:76` restores
   `/name args` for such an item, not the body — the command's own
   text is `principal` + the args the origin carries.
3. `expand()` prepends one line: `Base directory for this skill:
   <dir>` — both paths, one place. A body's relative `scripts/x`
   is then a path the model can resolve. `${BINGO_SKILL_DIR}` stays.

**D — one esc ends the turn** (`bingo-sdk`, `bingo-core`,
`bingo-surface-tui`; user-directed 2026-09-02: as Claude Code and
Codex do, no second stage)

1. The executor races every tool call against the turn's `cancel`
   token; `Interrupt::Block` ("await it to completion") is gone, and
   with it the trait, its field, its fail-closed test and the line in
   AGENTS.md. Dropping the future is the end: Bash's `KillOnDrop`
   takes the process group, an MCP or plugin-rpc call is abandoned
   (`tool/cancel` still sent first); the item is `Interrupted`. A
   background job (ADR-0018) is still never killed by an interrupt.
2. No new event: `TurnCompleted { Interrupted }` is the truth and
   now arrives at once.
3. The activity row answers on the keypress's own frame — a
   surface-local "asked to stop" flag, the `exit_armed` pattern —
   reading `✻ Stopping…` until `TurnCompleted` clears it. A fact about
   the key, not the session.

**E — the room looks like a room** (`bingo-rooms`, `bingo-agents`,
`bingo-surface-tui`, `tui.md`)

1. `SendMessage`, `OpenRoom`, `ListAgents` return `display:
   Some(View)` — a `KeyValue` receipt, a `Tree` of seats with
   status badges — the block lane, zero surface change.
2. The `members` extension is published as a `View::Tree` (members
   with an `ear` badge, listeners with patience), so `ctrl+t` and the
   rail stop showing `members: ["reviewer","scout"]`.
3. `headline()` draws `origin.conversation`: in a member's own
   transcript a post reads `⏺ reviewer in #design: …`; in the room's
   own transcript the name alone.
4. The `owed` signal gets its snapshot at 120×40 — first time seen.
5. `tui.md` gains §3 "Teams": what is on screen for a room today, and
   two or three sketched options for a members pane (rail card vs
   sheet vs status-line chips) with the trade-offs — sketches, not
   code; the pane itself is M35 after a choice.

## Files

`bingo-surface-tui/src/{blocks,scroll,input,run,transcript,view,
rewind,status,keys,motion,screens}.rs` and snapshots;
`bingo-core/src/session/inputs.rs`, `commands/think.rs`,
`session.rs`, `executor.rs`, `turn.rs`; `bingo-skills/src/expand.rs`;
`bingo-sdk/src/{event,state,host}.rs`; `bingo-rooms/src/{tool,room,
owed}.rs`, `bingo-agents/src/{message,list}.rs`; `schema/rpc.json`;
`docs/adr/0008-commands.md`; `docs/design/tui.md`.

## Exit criteria

- [x] A: snapshot of three blocks arriving over three frames — every
  row above the newest is byte-identical across the frames.
- [x] A: ten wheel notches in a burst scroll thirty lines.
- [x] B: `reasoning_inline_80x24` snapshot; `ctrl+o` on a thought
  opens the pager; click toggles a fold in a TestBackend test.
- [x] B: `/think high` on `Fake` with `reasoning: false` answers with
  the warning; the level survives a `/model` switch.
- [x] C: black-box `/guide` shows `⏺ /guide` folded; rewind restores
  `/guide`; the expansion's first line names the directory.
- [x] D: executor test — a tool that ignores its token ends on one
  `cancel` within the test's own timeout; `esc` on a running Bash
  `sleep 30` ends the turn and the process is gone (unix probe).
- [x] E: snapshots for the receipts, the roster tree, the `in #room`
  headline, the `owed` card; `tui.md` §3 Teams written.
- [x] Every gate in AGENTS.md; `cargo check --target
  x86_64-pc-windows-msvc` for D (a process is touched).

## Non-goals

Withdrawing a queued input (no dequeue exists; its own ADR).
A members pane (M35). A
`Prompt { echo }` contract change. Images.

## Risks

- A: snapshots move for every scene that showed a rise; re-aim, do
  not loosen.
- D: dropping a plugin-rpc future leaves the plugin's own process
  running its call; `tool/cancel` is still sent first. Recorded, not
  solved.
- C: a client that read `origin.surface == "tui"` to mean "typed"
  now sees `command`; grep every consumer before merging.

## Verified (2026-09-02)

Five worktree branches, merged A→B→C→G→E→F(M35)→D; each ran its
crate's fmt/clippy/tests before merging, the integrator re-ran the
crates each merge touched (tui 522, core 233, rooms 142, agents 135,
bin 137 black-box + suites) and the workspace gates once at the end.
- A: `a_new_block_takes_its_own_room_and_walks_nowhere_after_it`;
  `a_burst_of_notches_lands_the_whole_of_itself` (30 lines, not ~10);
  `tui-smoke.sh` 14/14.
- B: `reasoning_inline_80x24`; `ctrl_o_lifts_a_thoughts_fold_and_then_
  opens_it`; `a_click_opens_a_fold_and_a_second_click_folds_it`;
  `think_owns_up_when_the_model_will_not_reason_and_keeps_the_level`.
- C: `skill_command_80x24` (`⏺ /guide the wire format` folded);
  `a_turn_a_command_opened_is_named_by_the_line_that_was_typed`;
  black-box asserts `origin.surface == "command"` and the base line.
- D: `an_interrupt_drops_the_call_in_flight_and_skips_the_rest`;
  `rpc::one_interrupt_ends_the_turn_and_the_command_it_was_running`
  (grandchild `tick` loop stops, unix); `screens::stopping`; Windows
  cross-check of core and tool-bash clean.
- E: `owed_120x40` (first time drawn — the clock column folds at 22
  cells, recorded); `a_post_names_its_room_everywhere_but_in_the_room`;
  tui.md §3 "Teams" with three sketched panes.
- Found on the way, fixed by G (bug, commit body): every picker and
  the permission card lost the cursor past their room; `window.rs`.
Carried: `esc` does not fold a click-expanded block (only the pager's
does); three wall-clock budget tests in the TUI fail under machine
load and pass on a quiet box; the rail's pinned cards can stack past
the visible column.
