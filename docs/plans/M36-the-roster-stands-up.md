# M36 — The roster stands up

## Goal

The user chose sketch C of `tui.md` §3 "Teams", turned upright: the
quick cycle becomes a **vertical list** of every session in the tree,
each row saying what Claude Code's task list says — status, what it is
doing, tools · tokens · time, whether it needs you. It is **two columns side by side** (user-directed
2026-09-02, after two flat sketches were refused): sessions on the left
— agents, a room's members among them, never nested under the room —
and rooms on the right; `↑`/`↓` walk a column, `←`/`→` cross between
them. A member's row says which room it is in, what it owes and whether
it is listening; a room's row says how many seats and how many owed. There is already a vertical list of
sessions (`ctrl+g`, `tree::switcher_lines`) and a horizontal one (`↓`,
`cycle::strip`); after this milestone there is **one list, two doors**
— `↓` on an empty composer and `ctrl+g` anywhere — and the strip is
gone. Walking the list switches the view live, as the strip did; `⏎`
settles on it, `esc` returns to where you were. The cursor never leaves
the screen: the list draws through `window::around`.

## Bricks, in build order

1. **`tree::Row` grows what a row says.** Pure: `activity(state)` exists
   (`Running… 3 tools · 1.2k tokens · 40s`, `Done (…)`, `Needs you`);
   add `brief(state)` (the first ask, from the summary's title) and, for
   a member session, `seat(tree, session) -> Option<Seat>`: which room
   it sits in, its ear (`listening · 300s`) and its debt (`owes an
   answer · 22m`), composed from the room's `members` extension
   (`View::Tree` payload, M34-E) and the parent's `owed` signal —
   surface-side composition, ADR-0013 §4's own decision. For a room,
   `seats · owed` counts. Unit tests on fixtures.
2. **`roster.rs` — the list, pure.** Two columns under dim headings,
   the sessions that answer a model on the left, the rooms on the right:
   ```
     Sessions                                  Rooms
   ❯ ⏺ project   what is in this workspace?    # design   3 seats · 1 owed
     ⏺ reviewer  running · 3 tools · 40s        # ops      2 seats
     ⏺ scout     done · 8.1k tokens · in #design
     ~ watcher   listening · 300s · in #design
   ```
   Each column is its own list through `window::around` (the cursor's
   column scrolls under it; the other shows its head); the left column
   takes what the right does not need, and each row is clipped to its
   column with `…`. Below 80 columns the right column is still drawn —
   a room row is short. Name column padded to the longest in its
   column, the rest dim, `needs you` and `owes an answer` in
   `attention`. Snapshot at 80×24 and 120×40 with more rows than room
   and the cursor last in each column.
3. **One door replaces two.** `Open::Switcher` is the list; `↓` on an
   empty composer opens it with the cursor on the viewed session,
   `ctrl+g` the same. `cycle.rs` (the strip) and `Ui.cycling` are
   deleted; `status.rs` loses the strip branch; the `↓`-strip snapshot
   and its byte-identical-status-line proof go with it, replaced by the
   list's. `tree::switcher_lines` becomes `roster::lines` (one renderer).
4. **Live walk, settled by `⏎`.** `↑`/`↓` move within a column, `←`/`→`
   cross to the other at the nearest row. Moving the cursor calls `tree.show`
   as the strip did; `esc` shows the session the list was opened from
   (remembered on `Switcher` — a fact about the gesture, like
   `esc_armed`); `⏎` closes the list where it is. A click on a row does
   what the cursor there does. The `mark_read` on switch is unchanged.
5. **Keys and words.** `keys.rs`: `↓` "walk the sessions (empty box)",
   `ctrl+g` the same list; the `?` sheet follows;
   `tui.md` §3 "Teams" records the built shape and retires the strip,
   §4's switcher row is rewritten, §10 gains the dated line (the user's
   choice, and why one list rather than the strip plus a dropdown).

## Files

`bingo-surface-tui/src/{tree,roster (new),cycle (deleted),ui,input,
view,status,keys,screens}.rs`, snapshots, `docs/design/tui.md`.

## Exit criteria

- [ ] `roster_80x24` / `_120x40`: two columns, a member's row with its
  room, ear and debt; the cursor visible in either column with `…` at
  a cut end; `←`/`→` cross columns in a TestBackend test.
- [ ] `↓` on an empty composer and `ctrl+g` open byte-identical lists.
- [ ] Walking switches the view; `esc` restores the opening session;
  `⏎` keeps the walked-to one. TestBackend tests.
- [ ] `cycle.rs` is gone; `grep -rn cycling crates` is empty.
- [ ] Every gate in AGENTS.md; no new dependency.

## Non-goals

A room's roster shown without opening the list (sketch A, refused:
teaching the rail to read a sibling's state). The `owed` card's folded
clock (carried). Actions from the list (kick, listen) — a later plan.

## Risks

- The list covers the transcript's tail like every dropdown (layers,
  not reflows, §3); a room with many seats scrolls under the cursor.
- `screens.rs` is near its size fail; new scenes go in a submodule.
