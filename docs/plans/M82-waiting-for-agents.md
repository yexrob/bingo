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

- [x] a parent idle over running children shows the wait row; a wake's
      turn replaces it; no children, no row
- [x] `TestBackend` tests for the four states; snapshot updated
- [x] a tmux hands-on drive before release (TUI-visible)
- [x] every gate green

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

## Verified (2026-09-07)

Brick 1 moved rather than grew: `status.rs`'s `count`/`Wants` are now
`tree::count`/`tree::Wants`, taking a `tree::Scope`. One count, two
senses — `Scope::Others` is every session but the one in view, which is
what the status line's `2 running` has always meant and still does;
`Scope::Under(&id)` is what hangs under a session, however deep, by the
parent links the tree already holds. The row uses `Under(viewed)`,
because a child looking up at a running parent is waiting on nothing of
its own. `covered` resolves the scope once per count and `reaches` — the
roster's own walk, now over `&[&SessionSummary]` — is the one reading of
"hangs under", so the depth rule is not written twice.

`activity::lines` takes the `Tree` (it took the viewed `SessionState`),
so `waiting` reads the count at draw time and nothing is stored.
`breathing` split into `breathing_at(period)`, which is how the row
takes the 2.2 s pace with no turn to read one from. The input box needed
no change: `view::border` reads `busy()`, which a waiting session is not.

Precedence is the plan's literal condition — the wait draws only where
the viewed session has no turn — so the 300 ms a young turn is silent
for is silent here too, which is the blink R-flicker already allows.

Five `TestBackend` tests in `activity.rs` (singular and plural; nothing
running, and an agent that finished; a turn taking the row back; no
`(`, so no key, clock or token count; and an idle child under a running
root, which draws no row while the status line still says `1 running` —
red before the scope, green after), one scope unit test in `tree.rs`
over a root, a child and a grandchild, and one §6 cue test in
`motion.rs` (the sparkle on the 2.2 s breath, the border still `dim`).
Snapshots: `child_running_80x24`, `child_running_120x40` and
`a_tool_call_that_spawned_an_agent_says_what_it_is_doing` each changed
by exactly one row — the blank activity row became `✢ Waiting for 1
background agent to finish`; nothing else moved, which is what the
band's two held rows are for.

```text
== fmt / check / clippy (-D warnings)   exit 0
== test       86 suites, 4282 passed, 0 failed, 2 ignored (--no-fail-fast)
== discipline discipline ok (pre-existing warns only)
== budget     budget ok — dependencies unchanged (334)
== deny       advisories ok, bans ok, licenses ok, sources ok
```

Gates above are on the branch rebased onto `dev` at `c43cb29d`, which
moved five commits while this was being written. An earlier workspace
run hung in `acp_bridge` for 22 minutes and was killed; that is the
flake memory already names (its address keys on `std::process::id()`),
and every run since has passed it. The tmux hands-on drive is the
parent session's and is not ticked here.

- tmux drive (2026-09-07, 120×40, fake provider, harness-owned server): after
  `review the plan` the root's turn ends and the pane reads
  `⏺ reviewer(…)` / `⎿  Running… 0 tools · 0 tokens` / the root's line /
  `✶ Waiting for 1 background agent to finish`, status line `1 running`, no
  parentheses on the row. Eight seconds later the wake's turn has taken the
  row: `⎿  Done (0 tools · 10.0k tokens · 8s)`, `⏺ reviewer: finished.`,
  the root's answer, status line back to `? for shortcuts`.
