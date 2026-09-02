# M36 — The roster stands up

## Goal

The user chose sketch C of `tui.md` §3 "Teams", turned upright: the
quick cycle becomes a **vertical list** of every session in the tree,
each row saying what Claude Code's task list says — status, what it is
doing, tools · tokens · time, whether it needs you — and a room's row
opens into its seats: who is in it, who is listening and for how long,
who owes an answer and since when. There is already a vertical list of
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
   a room, `seats(tree, room) -> Vec<Seat>` composed from the room's
   `members` extension (`View::Tree` payload, M34-E) joined with each
   member's live `SessionState` in the tree and the parent's `owed`
   signal — surface-side composition, ADR-0013 §4's own decision. A
   `Seat { name, status, ear: Option<patience>, owed: Option<since> }`.
   Unit tests on fixtures.
2. **`roster.rs` — the list, pure.** Rows in the cycle's order (agents,
   then rooms, each room followed by its seats indented, open by
   default), one `Line` per row:
   ```
   ❯ ⏺ project     what is in this workspace?
     ⏺ reviewer    running · 3 tools · 1.2k tokens · 40s · needs you
     ⏺ scout       done · 4 tools · 8.1k tokens
     # design      3 seats · 1 owed
       ⏺ reviewer    busy · owes an answer · 22m
       ⏺ scout       idle
       ~ parent      listening · 300s
   ```
   Name column padded to the longest, the rest dim, `needs you` and
   `owes an answer` in `attention`, a seat's cursor enters that member's
   session. `←` folds a room's seats, `→` opens them (the `Tree` view's
   own gesture, §5). Drawn through `window::around` with the room the
   frame has above the composer; snapshot at 80×24 and 120×40 with more
   rows than room and the cursor last.
3. **One door replaces two.** `Open::Switcher` is the list; `↓` on an
   empty composer opens it with the cursor on the viewed session,
   `ctrl+g` the same. `cycle.rs` (the strip) and `Ui.cycling` are
   deleted; `status.rs` loses the strip branch; the `↓`-strip snapshot
   and its byte-identical-status-line proof go with it, replaced by the
   list's. `tree::switcher_lines` becomes `roster::lines` (one renderer).
4. **Live walk, settled by `⏎`.** Moving the cursor calls `tree.show`
   as the strip did; `esc` shows the session the list was opened from
   (remembered on `Switcher` — a fact about the gesture, like
   `esc_armed`); `⏎` closes the list where it is. A click on a row does
   what the cursor there does. The `mark_read` on switch is unchanged.
5. **Keys and words.** `keys.rs`: `↓` "walk the sessions (empty box)",
   `ctrl+g` the same list, `←/→` fold a room; the `?` sheet follows;
   `tui.md` §3 "Teams" records the built shape and retires the strip,
   §4's switcher row is rewritten, §10 gains the dated line (the user's
   choice, and why one list rather than the strip plus a dropdown).

## Files

`bingo-surface-tui/src/{tree,roster (new),cycle (deleted),ui,input,
view,status,keys,screens}.rs`, snapshots, `docs/design/tui.md`.

## Exit criteria

- [ ] `roster_80x24` / `_120x40`: agents then rooms, seats under each
  room with ear and debt, cursor visible with `…` at a cut end.
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
