# M82 — Waiting for agents

## Goal

When the session on screen is idle and agents it started are still at
work, the activity row says so: `✻ Waiting for 2 background agents to
finish`. Today the row leaves with `TurnCompleted`, so a parent that
ended its turn to be woken (M81) looks finished — an empty composer over
a dim `2 running` in the status line, which the eye does not find. The
wake that ends the wait opens a turn, and that turn takes the row over.
Claude Code's shape, taken whole (`✻ Waiting for 3 background agents to
finish`).

## Bricks, in build order

1. **A pure count** of the viewed session's descendants that are running,
   from the tree the surface already folds (`tree.rs` rows with
   `Status::Running`, minus the viewed session itself). No state: it is
   derived at draw time, as `status::count` is.
2. **The row's words**: `Waiting for 1 background agent to finish` /
   `… 2 background agents …`, one function, tested for both numbers.
3. **The activity row draws it** when the viewed session has no turn and
   the count is above zero. The sparkle cycles and breathes at the pace a
   tool holds a turn (2.2 s): the session is waiting on something else,
   which is what that pace already says. No `esc to interrupt` — there is
   no turn to end; no clock, no token count — they belong to a turn. The
   input box border stays dim: nothing is arriving here.
4. **Precedence**: a running turn's row outranks the wait (the wake opens
   one). `Needs you` in the status line is unchanged.
5. **`TestBackend`** tests: idle root with one running child draws the row;
   with none it draws nothing; a turn starting replaces it; the singular
   and plural. Plus a snapshot in `screens.rs` if the surrounding screens
   have one.
6. **`docs/design/tui.md`**: one row in the activity-row entry saying the
   above, dated, so the next change starts from it.

## Files

- `crates/bingo-surface-tui/src/activity.rs` (the row), `tree.rs` or
  `status.rs` (the count, wherever `count` lives), `screens.rs`
- `docs/design/tui.md`

## Exit criteria

- [ ] a parent idle over running children shows the wait row; a wake's
      turn replaces it; no children, no row
- [ ] `TestBackend` tests for the four states; snapshot updated
- [ ] a tmux hands-on drive before release (TUI-visible)
- [ ] every gate green

## Non-goals

- print, RPC, ACP and channels: the wait is the TUI's to show.
- `esc` while waiting: it does nothing today and keeps doing nothing.
  Ending the children from the parent is `KillAgent`-shaped work that
  does not exist and is not asked for.
- The status line's `2 running` stays: furniture counts, the row speaks.

## Risks

- R-flicker: a child's `TurnCompleted` reaches the surface before the
  wake's `TurnStarted` on the parent; the row would blink to nothing
  for a frame. Acceptable — the frames are ordered and the gap is one
  round trip; if it shows, fold the two states in the draw, not in state.
