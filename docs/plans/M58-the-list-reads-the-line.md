# M58 — The list reads the line

## Goal

M55 gave the `ctrl+g` list a query line of its own at its head. The
user, having used it (2026-09-04): no — one rule for every list in
the TUI: **what you type goes in the input box, the list narrows on
it, `tab` completes it, `⏎` takes it.** That is already how the `/`
and `@` dropdowns work; the session list should be the same shape,
not a list with a search box bolted on.

## Bricks

1. **The query is the line.** `Switcher.query` is deleted. On open
   the composer's draft is set aside (`Switcher.draft: String`, taken
   with `Composer::take`) and the box is empty; whatever is typed lands
   in the box as anywhere else, and the list is ranked by
   `ui.composer.text()` through the one matcher (`matching::rank`).
   On close — `⏎`, `esc`, a click — the draft is put back exactly as
   it was (`Composer::set`, caret at its end, as history recall does).
   The `⌕` query row and its glyphs go (the `Glyphs` fields M55 added
   are removed again; `search.rs`'s caret keeps its glyph if it still
   needs one — check).
2. **`tab` completes.** With the list open, `tab` puts the row under
   the cursor's name into the box (a session's name; a room's `#name`)
   — the way `tab` on the `/` dropdown completes the command — and the
   list narrows to it. `⏎` then, as now, keeps the session the walk
   landed on and closes; with a name completed and nothing else, `⏎`
   switches to that session. `esc` with text in the box clears the
   box first (the query), then closes and restores the draft and the
   view it was opened from.
3. **`↓` on an empty box** still opens the list; with text in the box
   `↓` walks it. The typed-into list is the same list either door
   opens (§3 "one list, two doors").

## Files

`bingo-surface-tui/src/{ui.rs,input.rs,roster.rs,view.rs,theme.rs}`
(the M55 files, minus the query row), `docs/design/tui.md` §3/§4/§7
(the M55 dated line is amended, dated again), the two
`switcher_query_*` snapshots (re-read: the query row gone, the box
holding the text).

## Exit criteria

- [x] `ctrl+g`, typing `rev`: the text is in the input box, the list
  shows `reviewer`'s row; `tab` completes the name into the box; `⏎`
  switches; the draft that was in the box before is back after.
- [x] `esc` twice: clears, then closes and restores view and draft.
- [x] No query row anywhere; `Glyphs` back to its M55-minus-two
  fields — minus *one*, in fact: see Verified.
- [x] Every AGENTS.md gate; tui-smoke.
- [ ] Hands-on (main session with the user).

## Non-goals

Completing a session name into a `@mention` for sending (that is the
`@` dropdown's). Fuzzy-matching the transcript.

## Risks

- A draft set aside and a crash between: the draft is in memory only,
  as it is today while typing. Accept.

## Verified

2026-09-04, on `m58-list-line` off dev `a8bf9e0e`. One commit per brick, in the
plan's order, after one preparatory move: `8d1872f` (the switcher's keys lifted
into a module of their own), `795da6b` (1, the box is the query), `8af1cdd`
(2, `tab` completes), `789a0dc` (3, `↓`'s two doors asserted with the box
holding a query).

### What landed

**0 — room to work in.** `input.rs` was at 937 non-test lines (fail at 1000) and
the brief pinned the growth to the switcher's keys, so they moved first, whole
and unchanged, into `crates/bingo-surface-tui/src/input/switcher.rs`:
`opens_the_roster`, `toggle_switcher` and `switcher` are `switcher::{opens,
toggle, keys}`, `walk_to` is re-exported from `input` so `pointer.rs` still
reaches it by the name it always used, and the tests stayed with `on_key` in
`input.rs`, which is what they press. `input.rs` 937 → 788, and 795 after the
three bricks.

**1 — the query is the line.** `Switcher.query` is gone. `Switcher.draft` holds
the line the box had when the list opened (`Composer::take`), and one function —
`switcher::put_away` — is the way out: it puts the draft back (`Composer::set`,
caret at its end, as history recall does) and closes the layer. `⏎`, `esc` on an
empty box and `ctrl+g` all go through it. The list is ranked by
`ui.composer.text()` at every read: `roster::lines`, `walked`, `placed` and
`Switcher::session` take the query as an argument, and nothing keeps a copy.
Typing is the box's own editing: `switcher::edits` inserts, backspaces, deletes
and moves the caret, and hands an `alt` chord to `super::alt` so a word chord is
spelled in one table only. After every edit the cursor is re-placed on the
session it was on (M55's rule, unchanged) and the view follows it, so `⏎` still
keeps what a person is looking at.

Two things the plan did not name had to hold with it:

- **A chord takes the list away.** `ctrl+t`, `ctrl+f`, `ctrl+o` and `ctrl+b`
  open something else over the list, and `ctrl+g` closes it; with the query in
  the box, any of them leaving the list up or the query behind would leave a
  line `⏎` sends. So `layered` puts the list away before answering a chord that
  is not the list's own (`switcher::CHORD`), and `put_away` is a no-op unless
  the list is what is showing.
- **The box's line is the list's, so it offers nothing else.** `Ui::listing()`
  is true while the list captures the keyboard, and `Ui::suggestions` answers
  nothing then — one fact in one place, so the `/` and `@` dropdown is neither
  drawn over the list nor answering its keys when a query happens to start with
  `/` or `@`.

`roster::asked` and `roster::headed` are deleted with the glyphs they spelled,
and the window asks for the whole of the room it has again (`roster.rs` is 29
non-test lines shorter).

**2 — `tab` completes.** `Switcher::name` is the second thing a row is asked
for, over the same one walk of the list as `Switcher::session`
(`Switcher::of`). `switcher::completed` puts that name in the box — `reviewer`,
`#design`, whatever the row is called — re-places the cursor and narrows the
list to it; `⏎` after it keeps the session the name named, because the walk the
narrowing ended in had already shown it. A list the query left empty has no
name to complete and `tab` leaves the line alone.

**3 — `↓`'s two doors.** No code changed. `down_opens_the_list_and_then_walks_
what_the_query_left` now also asserts the box is holding the query while the
arrow walks, and still holds it after — the walk is the list's, the line is the
box's.

**The glyph table gave one field back, not two.** `find` (`⌕` / `/`) is gone
with the row it marked. `caret` (`▌` / `|`) stayed: `search.rs` reads it for the
transcript search's own row, which is the check the plan asked for. `Glyphs` is
at 15 fields (cap 16), and the ASCII ledger test lost its `find` line and keeps
`caret`. `theme.rs`'s ASCII doc comment now says six characters, and design §7's
ASCII list says the `/` a person still sees is `search::PROMPT`, in every look.

### What the plan got wrong

1. **The Files list is short by three.** `pointer.rs` (one line: a click
   resolves against the list the *box* narrowed — `open.session(tree,
   ui.composer.text(), cursor)`), `screens.rs` (the `switcher_query` scene sets
   `ui.composer` instead of a field that no longer exists) and
   `input/switcher.rs` (new). Both of the first two are files other workers hold
   this hour; each change is the smallest that compiles.
2. **"`Glyphs` back to its M55-minus-two fields"** was one field too many, and
   the plan's own parenthetical is why (`caret` is the transcript search's now).
3. **"On close — `⏎`, `esc`, a click — the draft is put back."** A click never
   closed the list and still does not: a click on a row walks, exactly as the
   cursor there does (§3), and the list stays up. The third way out is a chord,
   which the plan did not mention and which needed the rule above.

### Snapshots

The two `switcher_query_*` were re-read, and no other snapshot changed. The
query row is gone and the box holds the text:

```
"⏺ reviewer(review the diff)                                                     "
"  ⎿  Running… 3 tools · 1.2k tokens                                             "
"                                                                                "
"❯ ⏺ reviewer  running · 3 tools · 1.2k tokens                                   "
"╭──────────────────────────────────────────────────────────────────────────────╮"
"│ > rev                                                                        │"
"╰──────────────────────────────────────────────────────────────────────────────╯"
```

### Tests re-aimed, and what they say now

No test was weakened; each M55 assertion about `Switcher.query` became the same
assertion about the box.

- `input::a_typed_query_narrows_the_list_and_the_view_follows_it` — the query is
  read off `ui.composer`.
- `input::esc_takes_the_query_back_before_it_closes_the_list` — the first `esc`
  empties the **box**.
- `input::backspace_gives_the_rows_back_letter_by_letter` — renamed reason: a
  backspace on an empty box is the box's own no-op and never reaches the `esc`
  stack (M55 answered that with `queried` returning `None`; now only `⏎` and
  `esc` reach `settle` at all).
- `input::the_list_opens_with_nothing_typed_into_it` — the box, not the field.
- `input::down_opens_the_list_and_then_walks_what_the_query_left` — brick 3's
  two assertions added.
- `roster::a_query_narrows_the_column_and_says_what_was_typed` →
  `a_query_narrows_the_column_to_what_it_matches`: one row, no line of its own.
- `roster::a_query_nothing_matches_is_the_line_alone` →
  `a_query_nothing_matches_draws_no_list`.
- `roster::the_query_line_takes_a_row_of_the_lists_own_room` →
  `a_narrowed_list_keeps_every_row_of_its_own_room` — the row the line used to
  take is a row of the list again.
- `roster::a_room_and_the_seats_in_it_answer_to_the_rooms_name`, `theme::the_
  ascii_table_spells_every_glyph_in_one_cell`, `screens::the_switcher_dropdown_
  narrowed_by_a_query` — the `⌕` line dropped from what they assert.

New: `the_line_being_written_is_set_aside_and_given_back` (all three ways out,
`esc` counted as the two presses §7's stack asks for),
`a_chord_that_opens_something_else_puts_the_list_away`,
`the_query_offers_no_command_of_its_own`,
`tab_completes_the_name_the_cursor_is_on`,
`tab_completes_a_rooms_name_with_its_sigil`,
`tab_completes_nothing_where_the_query_left_no_row`.

### Not verified

- **Hands-on** (left for the main session, as briefed).
- **Windows.** Not cross-checked: the TUI cannot be checked against another
  target locally (ADR-0041's note), and nothing here is platform-shaped — no
  process, path, signal or clock is touched.
- **The editing control chords leave the cursor a frame stale.** `ctrl+w`,
  `ctrl+u`, `ctrl+k` and `ctrl+j` are answered by `input::control` (they fall
  past `layered`'s chord table, as they did before M58), so they edit the query
  without re-placing the cursor: until the next key, a row-index past the end of
  a list they shortened draws no `❯`. Pre-existing in shape — the same keys
  edited the invisible draft under M55 — and the fix wants the cursor to name a
  session rather than an index, which is its own decision.
- **`run.rs::attach` closes the layer without the draft.** A `/resume` or
  `/clear` landing while the list is up would drop the set-aside line. Not
  reachable by hand today (the list owns the keyboard and neither command can be
  typed while it does); left alone rather than given a second closing path.
- **`keys.rs`'s binding table says nothing about typing into the list.** `tab`
  still reads "complete the command under the caret" and `ctrl+g` "the same
  list · ↑↓ to walk it". Truthful but thin; the copy sits under the hands-on
  pass, and changing it re-reads five help snapshots.

### Gates

```
$ cargo fmt --all -- --check
=== fmt exit 0

$ cargo check -j 2 --workspace --all-targets --locked
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 57.79s
=== check exit 0

$ cargo clippy -j 2 --workspace --all-targets --locked -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 35.40s
=== clippy exit 0

$ cargo test -j 2 --workspace --locked   # tee'd to target/m58-test.log
81 `test result: ok` lines, 3742 passed, 0 failed — no known flake hit, no rerun
=== test exit 0

$ scripts/check_discipline.sh
dependency direction ok
warn crates/bingo-surface-tui/src/input.rs: 795 non-test lines (>700)
kernel names no tool
cohesion ok
warn crates/bingo-core/src/session.rs:129 fn handle is 66 lines (>60)
discipline ok
=== discipline exit 0

$ scripts/budget.sh
dependencies (unique, normal): 332 (max  332)
warm cargo check -p bingo-core: 0s (max  20s)
relink isolation: touching the TUI recompiled 0 crates for core (must be 0)
target/debug: 7 GB (soft max  5)
warn: target/debug exceeds the soft limit
test binaries: 55
budget ok
=== budget exit 0

$ cargo deny check
advisories ok, bans ok, licenses ok, sources ok
=== deny exit 0

$ TMUX_TMPDIR=$(mktemp -d) scripts/tui-smoke.sh
  ...
  ctrl+g opens the one list of sessions and esc closes it
  ...
tui-smoke ok
=== smoke exit 0
```

The private `TMUX_TMPDIR` is M55's finding, unchanged: the script's socket and
session names are fixed, so two workers running it at once drive one pane.
