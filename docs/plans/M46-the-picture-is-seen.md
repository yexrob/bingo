# M46 — The picture is seen

## Goal

Design §5's image row, first column: on a terminal that speaks the
kitty graphics protocol, a PNG in the transcript is drawn as pixels —
a pasted screenshot under the person's line, a `Read` of a `.png`
under its tool row. Everywhere else the row's degrade stands
(`[image: image/png]`). Whether the terminal speaks it is *asked*, once,
at enter, the way the background colour is asked (theme.rs) — never
assumed from `TERM`. No new crate: kitty takes PNG bytes as they are,
so nothing decodes; `terminal-trx` (already in the tree under
colorsaurus, +0 in the budget) is the raw tty the probe reads from.

## Bricks

Pure first; the terminal last.

1. **The probe's parser** — `tui/src/graphics/probe.rs`. The terminal
   is sent, in one write: the kitty query
   `ESC _ G i=31,s=1,v=1,a=q,t=d,f=24;AAAA ESC \`, the cell-size query
   `CSI 16 t`, and DA1 `CSI c`. `fn parse(answer: &[u8]) -> Probe`
   reads what came back before the DA1 reply (`CSI ? … c`, which every
   terminal answers and which ends the read): `ESC _ G i=31;OK ESC \`
   means kitty; `CSI 6 ; h ; w t` is the cell in pixels. Tests on
   captured answers: kitty, WezTerm, Ghostty (OK + cell size), iTerm2
   and Apple Terminal (DA1 only), an empty answer (a pipe).
   `Probe { kitty: bool, cell: Option<Cell { width, height }> }`;
   `Graphics::from(Probe)` is `Kitty { cell }` only when both are there.
2. **The read** — `graphics::detect()`: `BINGO_GRAPHICS=off` or a
   multiplexer (`terminal::multiplexed`) skips the ask, as theme.rs
   skips its probe where the answer would change nothing; else write
   the three queries through `terminal_trx::terminal()`, read until
   the DA1 reply or `PROBE` (theme.rs's 400 ms) elapses, parse. Called
   from `Tui::enter` beside `theme::detect()`, before raw mode; the
   result rides `SurfaceOptions`-free into `Ui.graphics: Graphics`
   (default `Off`; tests set it).
3. **The encoder** — `graphics/kitty.rs`, byte-exact tests on each:
   `fn png_size(bytes) -> Option<(u32, u32)>` (IHDR at offset 16);
   `fn fit(px, cell, max_cols, max_rows) -> (cols, rows)` keeping the
   aspect ratio, never zero; `fn transmit(id, png, cols, rows) -> Vec<u8>`
   — `a=T,f=100,q=2,U=1,i=<id>,c=<cols>,r=<rows>`, base64 in 4096-byte
   chunks with `m=1` on all but the last; `fn delete(id) -> Vec<u8>`;
   `fn placeholder(id, row, cols) -> Line<'static>` — `cols` cells of
   `U+10EEEE`, the row diacritic on the first cell (the column ones on
   every cell, so a partially covered line still resolves), the id in
   the foreground as `Color::Rgb(id>>16, id>>8, id)` (ids stay under
   2^24). The id of a picture is a stable hash of item id and part
   index, so a redraw never re-sends.
4. **The block in the transcript** — `transcript.rs`: a user item's
   image parts and a tool output's image parts become one block each,
   after the words. Kitty and `image/png`: `fit` into the row's width
   and `IMAGE_ROWS` (12) in a peek, the whole height when open; the
   placeholder lines carry it. Anything else: one dim line
   `[image: <media type>]` — the §5 degrade — for a tool result; a
   user item's picture draws nothing extra when it cannot be pixels,
   because the words already name it (M45). The lines the transcript
   emits also record what the frame needs sent: a `RefCell<Placed>`
   on `Ui` beside `painted` (the same pattern — render time is when it
   is known), holding `(id, cols, rows, &png)` per block drawn.
5. **The send** — `Screen` grows `fn place(&mut self, bytes: &[u8])`;
   after `terminal.draw`, `Tui::draw` walks `ui.placed`, transmits
   each id it has not sent (a set on `Tui`), and deletes ids that fell
   out of the last `KEPT_IMAGES` (32) drawn, oldest first, so a long
   session does not fill the terminal's memory. Out-of-band bytes, as
   the title and the clipboard already go.
6. **Tests.** `TestBackend`: a tool result holding a PNG, `Ui.graphics`
   kitty with a 10×20 cell, draws `fit`'s rows of placeholder cells
   with the id's colour and `ui.placed` names the block; the same
   under `Off` draws the chip; a JPEG under kitty draws the chip; a
   user item's PNG draws its block under the line. The recorder screen
   in tests collects `place` bytes, and one run test sees the transmit
   go out once across two frames. `scripts/tui-smoke.sh` gains a scene
   whose PTY answers the probe with kitty's OK and cell size and
   asserts the placeholder row, and one whose PTY answers only DA1 and
   asserts the chip — the probe must end on DA1, not on the clock.

## Files

`bingo-surface-tui/src/graphics/{mod.rs,probe.rs,kitty.rs}` (new),
`terminal.rs`, `ui.rs`, `transcript.rs`, `theme.rs` (nothing but the
`PROBE` constant shared), `Cargo.toml` (`terminal-trx`),
`scripts/budget.toml` (a comment line: +0), `scripts/tui-smoke.sh`,
`docs/design/tui.md` (the §5 row's first column, dated).

## Exit criteria

- [ ] `parse` on the six captured answers; `png_size`, `fit`,
  `transmit`, `placeholder` byte-exact.
- [ ] Kitty on: a PNG tool result and a pasted PNG draw as placeholder
  blocks and are transmitted once; JPEG and kitty-off draw the chip.
- [ ] The probe ends on DA1; a terminal that answers nothing costs at
  most `PROBE`; `BINGO_GRAPHICS=off` and tmux ask nothing.
- [ ] tui-smoke's two new scenes; every AGENTS.md gate; budget +0;
  Windows cross-check for `bingo-surface-tui` (a tty is read).

## Non-goals

iTerm2 and sixel (the row's other columns); half-block cells (needs a
decoder); the full-size sheet (§5 "full-size in a sheet" — the block
is the fold's peek and open; the sheet is its own slice); JPEG, GIF,
WebP as pixels (kitty wants PNG or raw; no decoder); tmux passthrough;
images in `--print` and channels (the degrade is theirs).

## Risks

- ratatui 0.30's diff may skip a placeholder cell whose glyph and
  colour did not change while the terminal lost the image (a redraw
  after `leave`/`enter`): `Tui` clears its sent-set on enter.
- A terminal that answers the kitty query but not `CSI 16 t` gives no
  cell size; `Graphics` stays `Off` rather than guessing 8×16.
- The probe shares the tty with colorsaurus's; both run before raw
  mode, one after the other, never interleaved.
