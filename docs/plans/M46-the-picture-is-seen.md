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

- [x] `parse` on the six captured answers; `png_size`, `fit`,
  `transmit`, `placeholder` byte-exact.
- [x] Kitty on: a PNG tool result and a pasted PNG draw as placeholder
  blocks and are transmitted once; JPEG and kitty-off draw the chip.
  (Scope changed mid-flight: a JPEG is now *decoded* and drawn — only a
  payload no decoder reads takes the chip.)
- [x] The probe ends on DA1; a terminal that answers nothing costs at
  most `PROBE`; `BINGO_GRAPHICS=off` and tmux ask nothing.
- [x] tui-smoke's two new scenes; every AGENTS.md gate; budget 310 → 331
  by the measured 21 (ADR-0041); Windows cross-check for
  `bingo-surface-tui` and `bingo-pictures`.

## Non-goals

iTerm2 and sixel (the row's other columns); half-block cells (a
terminal that draws no pictures at all); the full-size sheet (§5
"full-size in a sheet" — the block is the fold's peek and open; the
sheet is its own slice); tmux passthrough; images in `--print` and
channels (the degrade is theirs). *JPEG, GIF and WebP as pixels left
the non-goals mid-flight*: the user took the dependency, so every
format a decoder reads is drawn (ADR-0041).

## Risks

- ratatui 0.30's diff may skip a placeholder cell whose glyph and
  colour did not change while the terminal lost the image (a redraw
  after `leave`/`enter`): `Tui` clears its sent-set on enter.
- A terminal that answers the kitty query but not `CSI 16 t` gives no
  cell size; `Graphics` stays `Off` rather than guessing 8×16.
- The probe shares the tty with colorsaurus's; both run before raw
  mode, one after the other, never interleaved.

## Verified

### What landed

The six bricks, plus a seventh the scope change brought (below).

1. **The probe's parser** — `bingo-surface-tui/src/graphics/probe.rs`.
   `QUERY` is the three questions in one write; `parse` reads what came
   back before the DA1 reply and `answered` says when that reply has
   landed. Tested on six answers **written from the protocol, not
   captured off six machines**: kitty, WezTerm and Ghostty (`OK` and a
   cell size, three different cell sizes), iTerm2 and Apple Terminal
   (DA1 alone), and an empty answer. Also: an `OK` for another image id,
   a refusal, an unterminated APC, a cell reply of zero pixels and one
   with a single number — every one of them fails closed.
2. **The read** — `graphics::detect()`, called from `Tui::enter` right
   after `theme::detect()`. `BINGO_GRAPHICS=off` and a multiplexer skip
   the ask. Three deviations, each with its reason under "What the plan
   got wrong".
3. **The encoder** — `graphics/kitty.rs`, byte-exact: `transmit`,
   `place`, `delete`, `placeholder`, the 128-entry diacritic table
   (copied from kitty's own `gen/rowcolumn-diacritics.txt`, fetched and
   diffed rather than recalled). `png_size` and the geometry moved:
   `png_size` is `bingo-pictures`' (it is the PNG fast path) and `fit`
   is `graphics/picture.rs`'.
4. **The block in the transcript** — `transcript/pictured.rs`. A tool's
   picture hangs under its `⎿`, a person's under their own line with no
   mark. `IMAGE_ROWS` (12) at a peek, the whole height when open,
   nothing when shut. The chip is `[image: <media type>]` under a tool
   row and nothing under a person's line (their words already name it).
5. **The send** — `Screen::place`, `Run::hand_pictures` after the draw,
   `graphics/stored.rs` as the state machine. Its invariant is one
   sentence: *the terminal holds exactly the last `KEPT` (32) pictures
   the transcript holds*. New picture → `transmit`; same picture, new
   rectangle → `place` (no bytes twice); gone → `delete` with `d=I`, so
   the memory goes with it.
6. **Tests.** 9 in `transcript/pictured.rs` (cells, colour, chip,
   person's line, the three folds, a whole `TestBackend` frame, and a
   block kept between frames keeping its picture), 7 in `stored.rs`,
   3 in `decoded.rs`, 7 in `probe.rs`, 6 in `kitty.rs`, 8 in
   `picture.rs`, 2 in `run.rs` (the picture goes out once across two
   frames; a terminal that draws none is handed none), 6 in
   `bingo-pictures`. Two new pty tests and two new smoke scenes.

### What the scope change brought

Mid-flight the user took the dependency: **every format a decoder reads
is drawn**, not PNG only. `crates/bingo-pictures` (library tier) is the
one place that knows a decoder — `to_png` passes a PNG through
untouched (size off the header, no decode, no re-encode) and decodes
anything else, a GIF as its first frame, re-encoding at
`CompressionType::Fast` because these bytes are going to a terminal on
the same machine. `graphics/decoded.rs` keeps the answers (the failures
too, or an undecodable 5 MiB payload would be re-decoded every frame),
capped at the same 32.

### What the plan got wrong

- **The pictures cannot ride a `RefCell<Placed>` on `Ui`.** The block
  cache draws an item once and clones it ever after, so a collector
  filled at render time is empty on the second frame — and the send
  would then delete a picture whose cells are still on the screen. They
  ride *with the lines*, in the block's `Entry`, and `Blocks::pictures()`
  derives the frame's list from them. `item_lines` became `item_block`
  and answers with `Block { lines, pictures }`. A test pins exactly this
  (`a_block_kept_between_frames_keeps_its_picture`).
- **`terminal-trx` is not used, and is not in the tree.** Its `lock()`
  takes `stdout().lock()` for the terminal it opens, so a probe thread
  abandoned at the deadline would deadlock every later frame's write.
  The probe opens `/dev/tty` itself with `O_NONBLOCK` and polls to a
  deadline on the calling thread — no thread to abandon, no lock to
  hold, and it works with stdout redirected. That costs `libc` as a
  `cfg(unix)` dependency for the one constant `O_NONBLOCK` (no call, no
  `unsafe`); it was already in the lockfile, so the count did not move
  for it.
- **Windows asks nothing.** No Windows console host speaks the kitty
  protocol, and a console that will never answer would cost every
  start-up the whole 400 ms. `exchange()` is `cfg`-split with its unix
  counterpart in the same function pair, and `QUERY`/`answered` carry
  `cfg(any(unix, test))` so they stay compiled and asserted on Windows
  CI without being dead code there. **WezTerm on Windows does speak the
  protocol and will get the chip** — a stated limitation, not an
  oversight.
- **`a=d,d=i` frees nothing.** Lowercase `d=i` deletes placements and
  leaves the image data in the terminal's memory, which is the opposite
  of the eviction's purpose; `d=I` is what the code sends.
- **Every cell carries both diacritics**, not "the row on the first
  cell". The protocol's diacritics are positional — the first is the
  row, the second the column — so there is no way to spell a column
  without a row. Both on every cell also means a half-drawn row still
  resolves, which is what the plan wanted.
- **`Ui.graphics` would have been a second copy** of a run-wide fact.
  `graphics::chosen()` is a `OnceLock` with a thread-local override for
  tests, exactly as `theme.rs` settles the look — so no field, no
  plumbing, and no way for a stale copy to exist. It never probes
  lazily: a run that did not call `detect()` draws no pictures.
- **tmux cannot answer the probe**, so the plan's kitty smoke scene is
  impossible: under a multiplexer the ask is skipped by design. The
  smoke gained the two scenes it *can* prove on a real terminal (the
  chip, and `BINGO_GRAPHICS=off`, both asserting that no byte of
  graphics protocol reaches a terminal that was never asked), and the
  kitty half moved to `crates/bingo/tests/pty.rs`, whose harness now
  answers the probe: it sees the query in the child's output and writes
  kitty's `OK` + `CSI 6;20;10t` + DA1 back. That test reads a real PNG
  through the `Read` tool and asserts one `ESC _ G a=T,f=100` went out,
  `U=1` with it, and no chip on the screen; its twin answers DA1 only
  and asserts the chip with not one byte of protocol.
- **Two ends the plan does not mention, both found by reading the exit
  path.** A picture's placeholder cells are not text: printed back into
  the shell's own screen on the way out (design §3) they would be a row
  of glyphs no font has, so `Blocks::tail` leaves them behind with the
  alternate screen. And a picture the terminal is holding for this
  surface would outlive the run, so `Run::leave` hands it back — one
  `catch_up(&[])` against an empty frame, which is the same reconciler
  saying "hold nothing". Each has a test.
- **`png_size`'s "IHDR at offset 16"** is right about the size but says
  nothing about the signature or the chunk type; both are checked, so a
  JPEG cannot be mistaken for a PNG with an absurd size.

### What is not verified

- **No real kitty, WezTerm or Ghostty was driven.** Every terminal in
  these tests is one this repository wrote: the probe answers are
  spelled from the protocol document (fetched from
  `sw.kovidgoyal.net/kitty/graphics-protocol/`), and the pty harness
  plays the terminal's part. What is proven is that the right bytes go
  out and the right cells are drawn — not that a real kitty paints a
  picture. That needs a person at a kitty window.
- **"The probe ends on DA1 rather than on the clock"** is proven
  structurally (the loop's condition is `probe::answered`, which has its
  own test) and behaviourally (a DA1-only pty comes up and draws), not
  by timing: a wall-clock assertion would pin the machine it was written
  on (AGENTS.md).
- **The decode runs on the draw thread.** A 5 MiB JPEG is decoded and
  re-encoded inside `terminal.draw`, once per picture; on a slow machine
  that frame may trip the surface's own "slow draw" notice. Bounded by
  `Image::MAX_BYTES` and by the 32-entry memo, but not measured.
- **Memory.** Up to 32 decoded pictures are held beside the journal's
  own copies. Worst case with 5 MiB pictures that is ~160 MB on top of
  what the journal already holds. Not measured on a real conversation.
- The **eviction and the re-place path** (a picture scrolled past 32
  others, a fold opened under a picture the terminal already holds) are
  covered by `stored.rs`'s unit tests, not by a driven terminal.

### Gates, all from the worktree

```
$ cargo fmt --all -- --check                                   # clean
$ cargo check --workspace --all-targets --locked               # Finished
$ cargo clippy --workspace --all-targets --locked -- -D warnings  # Finished
$ cargo test --workspace --locked                              # 3454 passed, 0 failed
$ scripts/check_discipline.sh                                  # discipline ok
$ scripts/budget.sh            # dependencies (unique, normal): 331 (max 331); budget ok
$ cargo deny check                        # advisories ok, bans ok, licenses ok, sources ok
$ scripts/tui-smoke.sh                                         # tui-smoke ok
$ cargo check -p bingo-surface-tui --all-targets --locked \
      --target x86_64-pc-windows-msvc                          # Finished, no warnings
$ cargo check -p bingo-pictures --all-targets --locked \
      --target x86_64-pc-windows-msvc                          # Finished
```
No known flake was hit; the suite was run twice and passed both times.
