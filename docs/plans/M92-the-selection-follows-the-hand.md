# M92 — The selection follows the hand

## Goal

User, 2026-09-10: "能不能增加一种选中模式，可以选中即复制，另外选中模式下可以滚动复
制，复制不在当前屏幕内的内容". The surface owns the alternate screen (design
§3), so the terminal's own selection reaches only what is on it; that is why
bingo has a selection of its own (`select.rs`): `v` or a drag starts a run,
the arrows extend it, `y`/`ctrl+c` copy it through OSC 52. Its cells are
lines of the *whole* rendered transcript, not of the screen, so a run over
what has scrolled away is already representable — and nothing helps a hand
reach it: the view stays where it was when the far end walks off it, a drag
past the transcript's edge is dropped, and the copy is a second gesture.

After this milestone: the view follows the run's far end; a drag held past
the edge scrolls until it comes back; releasing the button copies; the keys
that do all this are on the `?` table and the `guide-tui` page. The
selection stays what it is — a run of rendered cells, the terminal's
clipboard, the 100 KiB cap refused out loud.

## Bricks, in build order

1. `scroll.rs`: `Scroll::reveal(line, total, rows, now)` — the smallest move
   that puts `line` on the screen: nothing when it is on it, `line` at the
   top when it is above, `line` at the bottom when it is below. `show`
   (a third of the way down, for search) is untouched. Unit tests at both
   edges and in the middle.
2. `select.rs`: `Run::empty()` — anchor and head the same cell; `Select::
   edge(row, region_rows) -> Option<Edge>` where `Edge::Above(n)` /
   `Edge::Below(n)` is how many rows past the transcript a screen row is.
   Pure, tested.
3. `input.rs` `selecting`: after every `walk`, `ui.scroll.reveal(head.line,
   …)`. Each of the four arrows keeps the run on the screen.
4. `pointer.rs`: `drag` on a row inside the region extends as today; on a
   row past it (`Edge`), scrolls by that many lines and extends the run to
   the line now at that edge, and records `ui.select.dragging = Some(edge)`.
   `MouseEventKind::Up(Left)`: `dragging = None`; a run that is not empty is
   copied (`Effect::Copy`, the same `copy` the keys use) and let go, and a
   notice says `copied N lines`; an empty run — a click — copies nothing and
   changes nothing. `MouseEventKind::Moved` and a drag back inside clear
   `dragging`. The wheel while a run is held keeps the run (test).
5. `run.rs`: `animating()` is true while `dragging` is some; on each frame
   tick past `select::EDGE_PACE` (50 ms) since the last edge step, scroll
   one line toward the edge and extend the run to the line there — one
   `Ui::drag_step(now)` in `ui.rs`, pure over `painted`, tested with a
   scripted clock. Stops on its own at the transcript's first or last line.
6. `keys.rs`: two rows — `v` "select from the focused block · ↑↓←→ extend ·
   y or ctrl+c copy" and `drag` "select · release copies · past the edge
   scrolls" — placed after `pgup/pgdn`. The `?` panel draws them; the
   `guide-tui` page gains both (its owner test already requires every
   binding's keys in backticks, so it fails until the page says them).
7. `docs/design/tui.md`: §3's transcript line says the new rule; a dated
   §10 entry with the user's words.

## Files

- `bingo-surface-tui/src/{scroll,select,input,pointer,run,ui,keys}.rs`,
  `guide.md`, the `?` panel snapshot.
- `docs/design/tui.md`.

## Exit criteria

- [ ] `Scroll::reveal`: a line above the top becomes the top, below the
      bottom becomes the bottom, on the screen moves nothing; `Tail` stays
      `Tail` when the line is on the last screen
- [ ] `TestBackend`: `v` then `↓` past the last screen row advances
      `painted.top` by one and the run's head is on the last row; `↑` from
      the first row the mirror
- [ ] `TestBackend`: press on a row, drag to a row above the region, the
      view scrolls by the overshoot and the run reaches the new top line;
      a further drag inside stops the scrolling; holding past the edge
      scrolls one line per 50 ms under the scripted clock and stops at
      line 0
- [ ] `TestBackend`: press, drag, release yields exactly one `Effect::Copy`
      whose text includes a line that was off the screen at the press, and
      a `copied N lines` notice; press and release on one cell yields no
      copy and keeps the click's meaning (focus, fold cycle)
- [ ] the wheel during a held run scrolls and keeps the run; `esc` still
      lets it go; a typed letter lets it go and is typed
- [ ] `?` shows the two rows; `guide-tui`'s owner test passes with them
- [ ] every gate green, `scripts/tui-smoke.sh`, 80×24 and 120×40 snapshots
      unchanged except the `?` panel's

## Non-goals

- No selection by word or line on a double or triple click; no rectangular
  selection; no selection inside a sheet or a card.
- The clipboard stays OSC 52 with the 100 KiB refusal; no system clipboard
  crate (`bingo-surface-tui` reads the clipboard for pictures only where it
  already does).
- No change to what a run's text is: rendered cells, trailing spaces
  trimmed, lines joined by `\n`.

## Risks

- Terminals differ in whether a drag past the window's bottom row arrives
  at all; the row the terminal reports is the last one it has, and the
  `Edge` from that row is the design's answer. The pace is a constant to
  tune by hand, not a setting.
- A drag that ends outside the region delivers `Up` with a row past it;
  the run is copied as it stands, which is what the hand meant.
- Mouse reporting inside tmux needs `set -g mouse on`; without it there is
  no drag and nothing here changes, as today.
