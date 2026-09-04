# M56 — The picture where the eye is

## Goal

Two asks from the user after seeing M51 draw (2026-09-04): a picture
always sits at the left edge of its block, which reads as dumped
rather than placed — it should sit where the words it belongs to sit;
and a click on a picture should open it in the system's viewer
(Preview on macOS), the way a click on a URL row opens the browser.

## Bricks

1. **The picture's column.** Today a picture's cells start at the
   block's own indent (`Hangs::Said`/`Returns` in `transcript/
   pictured.rs`). Give `Hangs` the column its picture stands in: a
   picture in an answer's words stands at the words' indent (the
   `⏺` speaks-indent, as now) **plus** the indent of the markdown
   construct it was written in — a list item's marker width, a
   quote's bar — read off the chip line's own leading spaces
   (`markdown::Linked` gains `indent: usize`, measured by the
   renderer; pure, tested for a plain paragraph, a bullet, a nested
   bullet, a quote). A picture under `⎿` keeps the returns indent.
   A picture wider than the room left of the right margin is fitted
   to that room, as now, and never pushed past it.
2. **A picture is a thing to click.** `Painted` already keeps where
   each block's lines landed (`Painted::{line_at,row_of}`) and each
   block's `pictures`; add the cells each picture occupies (row range,
   column range) to `Block`/`Painted` — derived at draw, the way the
   strip's thumbnails are remembered — and let `pointer.rs` answer a
   left click inside one with `Effect::OpenPicture(Source)`. The
   strip's thumbnails answer the same way.
3. **Opening it.** `Effect::OpenPicture` resolves the source to a
   path — a journal picture or a draft has bytes only: write them once
   to `data_dir/pictures/<id>.png` (through `bingo_pictures::to_png`)
   and open that; a linked picture with a local path opens the path
   itself; a linked URL opens the URL — through the one browser/file
   opener (`bingo-loopback::browser::open` after M54 lands; if M54
   has not merged, wait for it rather than adding a second opener).
   `BINGO_NO_BROWSER` keeps it shut in tests. A notice names what was
   opened; failure to open is a `Warn` notice with the path.
4. **The hint.** The status line's `? for shortcuts` sheet gains one
   line: `click a picture · open it`. Design §5's image row gets a
   dated line for both.

## Files

`bingo-surface-tui/src/{markdown.rs,transcript/pictured.rs,painted.rs,
pointer.rs,effect.rs,run.rs (one arm; at 886 non-test lines),
composer/strip.rs,welcome.rs or help}`, `docs/design/tui.md` §5.

## Exit criteria

- [ ] A picture in a bulleted answer stands at the bullet's text
  column; in a plain paragraph at the words' column; under `⎿` where
  it is today (snapshots).
- [ ] A click inside a drawn picture opens it (test: the effect is
  produced with the right source; the opener is exercised with
  `BINGO_NO_BROWSER`, asserting the path it would have opened).
- [ ] Every AGENTS.md gate; budget unchanged; tui-smoke.
- [ ] Hands-on (main session with the user): click a Read picture and
  a markdown picture; each opens in Preview.

## Non-goals

Centring pictures (a transcript is left-ranged type). Dragging,
zooming or a full-size sheet inside the terminal. Copying the picture
to the clipboard.

## Risks

- Mouse reporting inside tmux: a click reaches bingo only when the
  mouse is on (`set -g mouse on`); otherwise nothing happens, which is
  tmux's, not ours — say so in the hint's design note.
- Writing journal pictures to `data_dir` leaves files behind; bound it
  by id (one file per picture, overwritten) and note the directory in
  the design doc.
