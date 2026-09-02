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

- [x] `roster_80x24` / `_120x40`: two columns, a member's row with its
  room, ear and debt; the cursor visible in either column with `…` at
  a cut end; `←`/`→` cross columns in a TestBackend test.
- [x] `↓` on an empty composer and `ctrl+g` open byte-identical lists.
- [x] Walking switches the view; `esc` restores the opening session;
  `⏎` keeps the walked-to one. TestBackend tests.
- [x] `cycle.rs` is gone; `grep -rn cycling crates` is empty.
- [x] Every gate in AGENTS.md; no new dependency.

## Non-goals

A room's roster shown without opening the list (sketch A, refused:
teaching the rail to read a sibling's state). The `owed` card's folded
clock (carried). Actions from the list (kick, listen) — a later plan.

## Risks

- The list covers the transcript's tail like every dropdown (layers,
  not reflows, §3); a room with many seats scrolls under the cursor.
- `screens.rs` is near its size fail; new scenes go in a submodule.

## Verified

2026-09-02, worker I, on macOS (aarch64).

```
$ cargo fmt --all -- --check
$ cargo clippy --workspace --all-targets --locked -- -D warnings
    Finished `dev` profile
$ cargo test -p bingo-surface-tui --locked
test result: ok. 548 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
$ cargo test --workspace --locked
69 binaries, all `test result: ok`, 0 failed
$ scripts/check_discipline.sh
discipline ok
$ scripts/budget.sh
dependencies (unique, normal): 302 (max  302)
relink isolation: touching the TUI recompiled 0 crates for core (must be 0)
budget ok
$ scripts/tui-smoke.sh
  ctrl+g opens the one list of sessions and esc closes it
tui-smoke ok
$ cargo check -p bingo-surface-tui --all-targets --target x86_64-pc-windows-msvc
    Finished `dev` profile
$ grep -rn cycling crates
(no output)
```

Exit criteria, with what proves each:

- **`roster_80x24` / `_120x40`** — `screens::teams::the_roster`, plus
  `roster_in_the_rooms` with the cursor crossed into the rooms column.
  The member row reads `~ watcher   idle · in #design · listening ·
  300s` and the room's `#design  2 seats · 1 owed`. The cut end and the
  cursor on the last row are `screens::windows::the_switcher_scrolls_…`
  and `roster::tests::a_column_longer_than_its_room_keeps_the_row_…`.
- **Two doors, byte-identical** —
  `screens::teams::the_two_doors_open_byte_identical_lists` draws both
  at 80×24 and 120×40 and compares the screens.
- **Walk, `esc`, `⏎`** — `input::tests::walking_the_list_switches_the_view_…`
  and `…::esc_gives_back_where_the_walk_started_and_enter_keeps_where_it_ended`.
- **`cycle.rs` gone** — the grep above; `Ui.cycling` and
  `tree::switcher_lines` went with it.

Decided while building, and recorded in `tui.md` §10:

- The row reads **doing · where it sits · what it hears · what it owes ·
  needs you · tools and tokens**, so a narrow column cuts the glanceable
  tail and never the actionable head. A rail card takes 24 columns from
  the transcript and the list is a dropdown of the transcript's width,
  so a row is cut harder at 120 *with* a rail than at 80 without one —
  which is what forced the order.
- A session that has not run a turn says **what it was asked** instead
  of a status.
- A listening seat wears `~` in the dot's place and keeps the dot's own
  colour: the glyph adds a fact rather than spending one.
- Headings are drawn only where there are two columns.

**What the roster payload lacked:** the age of a debt. `bingo.rooms`
signals `owed` as a `View::Table` whose `asked` column is a local
`%H:%M` clock time (`owed::asked`), with no date beside it — `owed::column`
computes the age but is `/room`'s text, not a published fact. A surface
cannot turn `14:02` into `22m` without inventing a day and a timezone, so
a row says `owes an answer since 14:02`. **No plugin change was made**: a
`since_s` (or a timestamp) on that table would be the cure, and it would
also touch the `owed` rail card, whose three columns are already one
wider than the rail (§10, M34-E). Everything else the design asked for
was already in the payloads: the membership, the declared listeners'
`patience_s`, each seat's own `ear:<name>` register, and who owes in
which room.

Also carried: `input.rs` crossed `check_discipline.sh`'s 1000-line fail,
so the pointer came out into `pointer.rs`. The walk itself has no PTY
scene — the smoke's fixture has one session — so `ctrl+g` drives the
layer there and the walk stays a `TestBackend` test.
