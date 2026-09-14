# M95 — The foot is a row away

## Goal

User, 2026-09-14: when the transcript is not showing its latest lines —
scrolled back, held — put a button above the input box that takes it to
the foot on a click. Today the only ways back are `end` on an empty
composer, `pgdn` until the foot, or sending a line; nothing on the
screen says the foot is off it, and nothing there answers a mouse.

After this milestone: while the transcript is held, the activity band's
first row — its air, which is always there (§3: the band is reserved) —
carries a centred pill (right-aligned at first; the user asked for the middle the same day), `↓ 37 lines below · end or click to
follow`, on the raised tint; a click on it follows the tail again, as
`end` does; the moment the foot is on the screen the row is air again.
Nothing else moves: the pill takes a row the frame already holds, so the
transcript is the same height held or following.

## Bricks, in build order

1. `foot.rs` — pure: `label(below, width) -> Option<String>`, the long
   form, the short form (`↓ 37 lines below`) when the hint does not fit,
   the arrow alone when nothing else does, and no count when `below` is
   zero (a resize can hold a transcript whose foot is on the screen);
   `pill(below, width) -> Option<Pill { line, x, width }>`: the label on
   its bar, centred, the count in `text` and the hint in `dim`, both
   on `raised`; `below(ui, now) -> Option<usize>`: `None` while the
   scroll follows, else the lines under the frame's last row. Table test
   per width; one for the styles.
2. `activity::lines` — the foot row is the band's first row while there
   is one: it replaces the air when the band has rows and is the band's
   one row when it has none, so the demand's `max(2)` is untouched and
   the geometry never changes on a scroll.
3. `Painted.foot: Option<Rect>` — where this frame drew the pill; cleared
   by `begin`, set by `render_activity`. `pointer::pressed` answers a
   click inside it with `scroll.end()`, after the cards and the rail and
   before a picture or a transcript cell, and not under a layer.
4. Screens: `held_back` at both sizes; a styles assertion for the pill's
   row; a click test (on the pill → `Tail`; beside it → still held); a
   frame test that the transcript region is the same height held and
   following.
5. `docs/design/tui.md` §3 (under "Nothing jumps"), §4 (a `foot` row),
   §10 (dated); `guide.md`'s keys line names the click.

## Files

- `crates/bingo-surface-tui/src/{foot.rs,activity.rs,view.rs,ui.rs,pointer.rs,lib.rs,screens.rs,input.rs,guide.md}`
- `docs/design/tui.md`

## Exit criteria

- [x] `pgup` on a long transcript draws the pill on the band's first
      row; `end` or a click on it draws the tail and no pill.
- [x] the transcript region is the same `Rect` held and following.
- [x] snapshots at 80×24 and 120×40; the styles assertion; the click
      test; the PTY smoke still green.
- [x] every gate green (fmt, check, clippy, test, discipline, budget).

## Non-goals

- A count of *new* items since the hold: the lines below the frame are
  the one fact the scroll already has; a second counter is a second
  representation.
- The rail, a sheet or a card: the pill is the band's and nothing
  else's; a layer over the band covers it, and a click there is the
  layer's.
- A row in the help table: the sheet fills 80×24 (M92) and the pill
  already says `end`.

## Risks

- R-air: the air row is what separates the transcript from the verb row
  (§3). The pill is centred and on its own tint, so the eye reads
  it as furniture at the foot, not as the transcript's last line.
- R-stale: at demand time the count comes from the last frame; only the
  words depend on it, never the rows.

## Verified (2026-09-14)

- `cargo test -p bingo-surface-tui --locked` → `1138 passed; 0 failed`
  (`foot::tests::*`, `screens::held::held_back_from_the_foot`,
  `screens::colours::the_foot_row_is_a_bar_with_the_count_in_text_and_the_hint_in_dim`,
  `input::tests::a_click_on_the_foot_row_follows_the_tail_again`,
  `view::tests::the_foot_row_costs_the_transcript_nothing` among them; two
  scroll snapshots moved by the one row, both read and accepted).
- `cargo test -p bingo --test pty --locked` → `16 passed`.
- tmux drive, 80×24, fake provider, a forty-paragraph answer: `pgup` twice
  drew `↓ 36 lines below · end or click to follow` centred on the band's
  first row with the box unmoved; an SGR click at column 40 of that row
  drew the tail and no pill; `pgup` then `end` did the same. (The first
  cut was right-aligned; the user asked for the middle and the drive was
  run again.)
- `cargo fmt --all -- --check` ok; `cargo check --workspace --all-targets
  --locked` clean; `cargo clippy --workspace --all-targets --locked --
  -D warnings` clean.
- `cargo test --workspace --locked --no-fail-fast` → exit 0, 89 suites
  `ok`. One earlier run failed `bingo-auth-oauth`'s
  `the_named_port_is_taken_when_it_is_free_and_given_up_when_it_is_not`:
  it rebinds a port it has just freed, and something on the machine took
  it in between — a machine flake, green alone and green on the rerun.
- `scripts/check_discipline.sh` → `discipline ok` (the scene lives in
  `screens/held.rs` because `screens.rs` was at the 1000-line line);
  `scripts/budget.sh` → `budget ok`.
