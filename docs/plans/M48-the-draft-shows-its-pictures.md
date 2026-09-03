# M48 — The draft shows its pictures

## Goal

Two things a person sees before a picture is sent, and one they must
never see. On a terminal that draws pictures, the composer shows a
thumbnail strip of the pictures its line still names — `[image 1]`
stays in the words (it is the record and the anchor in the sentence;
M45), the strip is that record made visible. A terminal that answers
the graphics query but never draws a placeholder cell (the research
below names them) gets the chip, not a picture-shaped hole. And a
picture is scaled to the cells it will cover before it is sent, so a
thumbnail costs kilobytes, not the screenshot.

## Bricks

1. **The named list** — the protocol can be asked for, the placeholders
   cannot: the spec's `a=q` answer is `OK` or an error and carries no
   feature bit, and no DA/XTVERSION field names it (research of
   2026-09-04, pasted in Verified). So after a kitty `OK` the terminal
   must also be one *known* to draw `U=1` placeholders, and the list is
   an allow list: **kitty ≥ 0.28.0** (placeholders landed there),
   **Ghostty ≥ 1.0.0** (PR #2015, 2024-07-31), **iTerm2 ≥ 3.5.6**
   (commit 4fe5b21, stable 2024-11-02), **Rio ≥ 0.5.27** (fix #1893,
   2026-08-24). Every other terminal that answers `OK` — WezTerm
   (issue #986 open since 2021), Konsole (parses `U`, never reads it;
   bug 523718), Warp (issue #6210, rejects placeholders), VS Code's
   xterm.js (issue #5711) — draws tofu, and WezTerm and Konsole drop a
   stray image at the cursor besides; the chip is what they get. The
   name comes from XTVERSION: `CSI > 0 q` joins the probe's one write
   (before DA1, so DA1 still ends the read) and its reply
   `DCS > | <name> <version> ST` (`kitty(0.46.2)`, `ghostty 1.3.1`,
   `iTerm2 3.6.11`, `Rio 0.5.27`) is parsed beside the others —
   `probe::parse` yields `Probe { kitty, cell, terminal:
   Option<Named { name, version }> }`. `graphics::draws_placeholders(
   &Named) -> bool` is the pure allow list with the version floors,
   its doc comment carrying the four sources; a terminal that says
   `OK` but no XTVERSION, or an unnamed one, is `Off`: silence is not
   a yes. XTVERSION is preferred over `TERM_PROGRAM` because it
   survives `ssh` and foot strips the env anyway. Tests: each of the
   four at and below its floor, each of the four false positives, an
   absent reply, a malformed one.
2. **Scaling before the send** — `bingo_pictures::scaled(&Png, (w, h))
   -> Png` (the `image` crate's resize, Lanczos3 or Triangle — measure
   once, pick the faster that is not visibly worse at thumbnail size).
   `Stored::sending` asks for the pixels at `cols × cell.width` by
   `rows × cell.height` instead of the original, for every picture — a
   transcript block is at most `IMAGE_ROWS` tall too. A picture whose
   pixels are already within the box is sent as it is. The decode
   memo keys on `(id, cols, rows)` now, since the bytes depend on the
   box; a fold that opens a block re-sends at the larger size (the
   `place`-only path goes: the terminal was never given the large one).
3. **The draft's pictures** — `graphics::picture::Picture` grows a
   `Source`: `Journal { item, part }` (today's) or `Draft { token }`
   (a held picture, by its `[image N]`); `id()` for a draft is a hash
   of the token under a distinct prefix byte so it never collides with
   a journal id; `image_in` reads a draft from `ui.pictures` — `Stored`
   asks for pixels through one closure that knows both. `Ui.pictures`
   (M45's `Held`) is the store; nothing new remembers a thing.
4. **The strip** — `composer/strip.rs`: from `Held::carried(line)` in
   token order, at most `STRIP_SHOWN` (4) thumbnails, each `fit` into
   `STRIP_ROWS` (3) rows and `STRIP_COLS` (12) columns, side by side
   with one column between, `+N` in dim after the last when more were
   cut; the placeholder lines ride as the first rows *inside* the box,
   above the prompt row, only under `Graphics::Kitty` and only when the
   line names a picture that is held — otherwise the box is exactly
   what it is today. `frame::composer_rows` and `view::composer_rows`
   count the strip; `COMPOSER_MAX` grows by `STRIP_ROWS` when a strip
   is up, so the ten text rows are not eaten. The strip's `Picture`s
   join the frame's `placed` list, so `Stored` sends and forgets them
   like any other; a submit clears `Held` and the next frame's
   catch-up deletes them from the terminal.
5. **Tests.** `draws_placeholders` per name; `scaled` keeps the aspect
   and never upsizes; `Stored` sends the scaled bytes and re-sends on a
   larger box; `TestBackend`: a held picture under `drawing()` puts
   three placeholder rows above the prompt and the box is three rows
   taller; deleting the token from the line drops the rows and the
   next catch-up carries `delete`; five held pictures show four and
   `+1`; under `Graphics::Off` the box is untouched; a room's prompt
   (`#design > `) keeps its row under the strip. PTY: the kitty scene
   in `crates/bingo/tests/pty.rs` pastes (inject `Held` through the
   harness's seam, or a `@shot.png` mention — whichever the harness
   reaches) and asserts a transmit sized to the strip's box.

## Files

`bingo-surface-tui/src/graphics/{mod.rs,probe.rs,picture.rs,stored.rs,
decoded.rs}`, `composer.rs` + new `composer/strip.rs`, `frame.rs`,
`view.rs`, `bingo-pictures/src/lib.rs` (`scaled`), `crates/bingo/tests/
pty.rs`, `docs/design/tui.md` §4 (the strip, dated) and §5 (the named
terminals), the plan's Verified.

## Exit criteria

- [x] The four named terminals draw at or above their floors; WezTerm,
  Konsole, Warp and xterm.js draw the chip; an `OK` with no XTVERSION
  is the chip.
- [x] A 4000×3000 PNG in a 12-row block is sent at the block's pixel
  size; a thumbnail is sent at ≤ 3 rows' pixels.
- [x] The strip appears, sizes, cuts at four, and leaves with its token;
  the terminal is told to forget it after submit.
- [x] Every AGENTS.md gate; budget 331; `tui-smoke` and the PTY kitty
  scene; Windows cross-check is CI's (ADR-0041 note).

## Non-goals

Removing `[image N]` from the words (it is the record). A strip on a
terminal without graphics. Thumbnails of `@path` mentions before
submit (they are read at submit, M47). Clicking a thumbnail. Any
change to how the transcript's own blocks are placed.

## Risks

- Ten text rows plus three of strip plus the borders leaves a short
  terminal little transcript: the strip yields first when
  `frame::composer_rows` runs out of room.
- The allow list is a list: a fifth terminal that learns placeholders
  next year draws the chip until it is added. That is the honest
  cost of a feature nobody can query; the doc comment says where to
  look before adding one (its parser must read `U`, not just store it).
- A draft id colliding with a journal id is made impossible by the
  prefix byte, not by luck; the test says so.
- Scaling on the draw thread: a 4000×3000 decode + resize is tens of
  milliseconds once per `(id, box)`; the memo keeps it once. Recorded,
  not cured, like M46's decode.

## Verified

### What landed

The five bricks, with six shapes different from the plan (below).

1. **The named list** — `graphics/probe.rs` and a new
   `graphics/placeholders.rs`. `QUERY` is four questions in one write, with
   XTVERSION (`CSI > 0 q`) between the cell reply and DA1, so DA1 still ends
   the read. `probe::parse` yields `Probe { kitty, cell, terminal:
   Option<Named { name, version }> }`, reading `DCS > | text ST` in both
   shapes the wild uses — `kitty(0.46.2)` and `ghostty 1.3.1`.
   `graphics::draws_placeholders(&Named)` is the allow list with its four
   floors, its doc comment carrying the four sources and the four false
   positives. `Graphics::from(Probe)` is `Kitty` only when all three answers
   came back. 6 tests in `probe.rs`, 5 in `placeholders.rs`, 4 in
   `graphics/mod.rs`, and a pty scene (below).
2. **Scaling before the send** — `bingo_pictures::scaled(&Png, (w, h)) ->
   Png`, `Triangle` (the measurement is below). `Decoded::thumbnail` is what
   `Stored` asks through, so what goes over the wire is the pixels of the
   cells and nothing more; a picture already inside its box is the bytes that
   came in, untouched. `kitty::place` is **deleted**: with the bytes cut to
   the rectangle, a rectangle that grew is a picture the terminal was never
   given, so it is transmitted again.
3. **The draft's pictures** — `graphics::picture::Source` is `Journal { item,
   part }` or `Draft { token }`; `Picture` is a `Source` and a rectangle.
   `Picture::image_in(state, held)` is the one lookup that knows both places,
   and `run.rs`'s `placing` closure is the only caller. `Held` gained `shown`
   (the line's pictures under their tokens) and `under` (one token's picture);
   `carried` now derives from `shown`.
4. **The strip** — `composer/strip.rs`. `strip::rows(held, line, graphics,
   decoded, width) -> Strip { lines, pictures }`, at most `SHOWN` (4)
   thumbnails each fitted into `ROWS` (3) by `COLS` (12) with a `GAP` of one
   column, `+N` dim on the floor row when more were cut. `frame::Demand`
   gained a `strip` field and `frame::composer_rows` adds it *after* the
   `COMPOSER_MAX` clamp, so the ten rows of draft are still ten.
   `view::render_composer` split into `render_strip` (the band, and the rows
   left under it) and `render_draft`. `Painted::placed()` is the frame's list:
   the blocks' pictures, then the strip's.
5. **Tests.** 9 in `strip.rs`, 6 new in `view.rs` (`TestBackend`: the band
   above the prompt and the box three rows taller, the token deleted taking it
   with it, five showing four and `+1`, `Graphics::Off` untouched, a room's
   `#design >` keeping its row under the band, a five-row screen keeping the
   prompt and dropping the band), 1 new in `run.rs` (the bytes go out at
   `c=8,r=3` and the payload decodes to 80×60, then `a=d,d=I` when the token
   leaves), 4 new in `pictures.rs`/`picture.rs`, 5 new in `bingo-pictures`,
   2 new in `frame.rs`, 3 new in `decoded.rs`, 1 new pty scene. Workspace
   total 3493 → 3527.

### The research, of 2026-09-04

| Terminal | `U=1` placeholders | answers `a=q` | XTVERSION reply payload | source |
|---|---|---|---|---|
| kitty ≥ 0.28.0 | yes | yes | `kitty(0.46.2)` | spec "Unicode placeholders" versionadded 0.28.0; PR kovidgoyal/kitty#5664 |
| Ghostty ≥ 1.0.0 | yes | yes | `ghostty 1.3.1` | ghostty-org/ghostty#2015 (2024-07-31) |
| iTerm2 ≥ 3.5.6 | yes | yes | `iTerm2 3.6.11` | gnachman/iTerm2 commit 4fe5b21 (2024-08-21), stable 3.5.6 (2024-11-02) |
| Rio ≥ 0.5.27 | yes | yes | `Rio 0.5.27` | raphamorim/rio#1893 (2026-08-24) fixing #1891 |
| WezTerm | **no** — tofu + a stray image at the cursor | yes → OK | `WezTerm 20240203-110809-5046fc22` | wezterm/wezterm#986 (open since 2021-07-28), #7807, PR #7924 unmerged |
| Konsole | **no** — coloured tofu + a stray image | yes → OK | `Konsole 26.08.0` | Vt102Emulation.cpp stores `U` and never reads it; bugs.kde.org 523718 |
| Warp | **no** — tofu, placement rejected silently under `q=2` | yes → OK | `Warp(v0.2026.06…)` (framing unconfirmed) | warpdotdev/warp#6210 (open 2025-03-28) |
| VS Code / xterm.js | **no** | yes → OK when the image addon is loaded | none | xtermjs/xterm.js#5711 |
| foot, Alacritty, Windows Terminal | no graphics protocol at all | no | `foot(1.28.0)` / none / none | — |
| tmux | placeholders pass through with `allow-passthrough on` (tmux ≥ 3.4) | n/a | n/a | tmux.1; kitty icat `--passthrough` implies `--unicode-placeholder` |

Protocol-level detection does not exist: the `a=q` reply is `OK` or an error
with no feature bits, and no DA/XTVERSION field names placeholders. Probing by
side effect is destructive — WezTerm and Konsole ignore `U` and draw a real
placement — so the list is the only honest answer.

### The resize, measured once

A 4000×3000 PNG fixture (`bingo_pictures::testing::png_bytes`, 249 287 bytes),
release build, this machine, decode and resize timed separately:

```
decode 4000x3000 png: 63.949958ms
Triangle -> 320x240 in 29.993708ms, 2839 png bytes
Triangle ->   80x60 in 26.305000ms,  379 png bytes
Lanczos3 -> 320x240 in 81.008625ms, 2896 png bytes
Lanczos3 ->   80x60 in 76.116667ms,  379 png bytes
```

Triangle is 2.7–2.9× faster and writes a PNG of the same size to within 2 %;
at 320×240 and below the two are indistinguishable, and this runs on the draw
thread. Triangle it is. (The measurement was taken with a throwaway test that
was then deleted: a 12-megapixel fixture in the debug suite would cost every
run seconds for a number that does not change.)

### What the plan got wrong

- **A prefix byte does not make a collision impossible.** It separates the
  hash's *inputs*, and two different inputs may still hash to the same 24-bit
  number. The id space is partitioned instead: a journal picture is
  `hash & 0x7f_ffff` and a draft is `0x80_0000 | hash & 0x7f_ffff`, so no
  draft's number can be a journal picture's whatever the hash does. The test
  the plan asked for now asserts something that is true by construction.
- **The memo keys on pixels, and keeps two answers per picture.** The plan
  said `(id, cols, rows)`, which would replace the whole picture with the
  scaled one — but a frame has to measure the *whole* to know how many cells
  it takes, and only then knows the box. So `Decoded` holds both: `png(id)`
  is what a frame measures, `thumbnail(id, within)` is what the wire carries,
  and the cap is `2 * KEPT` for it. The key is pixels rather than cells
  because the cell is a constant of the run, so the two are the same key, and
  pixels is what `scaled` takes.
- **`draws_placeholders` is a module, not a function in `graphics/mod.rs`.**
  It is a list with a version comparison under it and four sources in its doc
  comment; `mod.rs` owns "what this terminal can draw" and would have owned
  two things. `pub use` keeps the name the plan spells.
- **The strip's height does not vary with what is in it.** `view::demand` is
  handed the frame's width and `render_composer` the composer region's, which
  differ by the rail — an inconsistency `composer_rows` already had. A band
  whose height depended on how many thumbnails fit could therefore disagree
  between the two, and the box would be drawn taller or shorter than the rows
  the frame cut for it. So `Strip::height()` is `ROWS` or nothing, and
  `fitting(width)` never cuts below one thumbnail: *whether* there is a band
  is a question about the line alone. The thumbnails stand on the band's last
  row, so their base is the prompt row whatever their heights are.
- **`Demand.strip` is a field of its own.** Folding the band into `composer`
  would have made `COMPOSER_MAX` mean "ten rows of draft, or seven and a
  band", and a picture pasted into a full draft would push a line of it off.
- **The pty cannot paste a picture.** `Effect::PasteImage` reads the *system*
  clipboard through `osascript`/`wl-paste`/PowerShell (`clipboard.rs`); a pty
  cannot set it, and a test that did would be writing to the machine it runs
  on. `@shot.png` is not the draft either — a mention is read at submit and
  becomes a journal picture (M47). So the strip is not driven through a pty,
  and the pty gained instead the two things it *can* prove: brick 1's chip on
  a terminal that says `OK` under a name not on the list, and brick 2's shrink
  end to end — a 1200×900 file, the transmitted PNG decoded back out of the
  APC and asserted smaller than the block and under a quarter of the file.
- **`scaled` answers a `Png`, not a `Result`.** The bytes decoded once to
  become a `Png`; if they will not decode now, the picture a person can see is
  worth more than the bytes it costs, so the original is handed back — which
  is exactly M46's behaviour.

### What is not verified

- **No real terminal was driven, again.** Every terminal in these tests is one
  this repository wrote: the XTVERSION replies are spelled from each project's
  documented format and the pty harness plays the terminal's part. What is
  proven is that the right bytes go out and the right cells are drawn.
- **The floors are read, not run.** Each of the four came from a PR, a commit
  or a spec note (the table names them); nobody here installed kitty 0.27.9 to
  watch it fail. iTerm2's is the weakest link — the commit is dated and the
  release it first shipped in is inferred from the 3.5.6 release date.
- **The list will go stale.** A fifth terminal that learns placeholders draws
  the chip until it is added. That is the cost the plan accepted; the doc
  comment says where to look first (its parser must *read* `U`, not merely
  store it).
- **A token's number can come back.** `Held::hold` mints one past the highest
  token *in the line* (M45), so emptying the line and pasting again reuses
  token 1 — and with it that draft's id. Between the two, a frame is drawn
  with no strip and the terminal is told to forget the old picture, so the
  reuse is safe; but a delete and a paste inside one frame's tick would leave
  the terminal holding the old bytes under that id and drawing them. One tick
  wide, not tested, not fixed.
- **The 4000×3000 case is measured, not asserted.** The suite pins the same
  rule at the sizes it can afford: 2000×1500 into a 12-row block and into the
  strip's three rows (`bingo-pictures`), 400×300 through `Stored` and the
  recorder (`run.rs`), 1200×900 through the pty. A twelve-megapixel fixture in
  the debug suite would cost every run seconds for a number the numbers above
  already give.
- **"The terminal is told to forget it after submit"** is proven against the
  state a submit leaves — the line taken and `Held` cleared — not by driving
  `Effect::Submit` through the reducer, which on an `idle()` run would write a
  history file into the working directory.
- **The resize is on the draw thread**, like M46's decode: tens of
  milliseconds once per `(picture, box)`, kept once by the memo. Recorded,
  not cured — and the numbers above are release, not the debug build a
  developer runs.
- **The Windows cross-check could not be run for the TUI**, for the reason
  ADR-0041's note records: `reqwest` → `rustls` → `aws-lc-sys`, whose build
  script compiles C against `windows.h` for the target, and there is no
  Windows SDK on this machine. This milestone adds no `cfg`, no signal, no
  process, no path and no clock; the one platform-gated thing it touches is
  `probe::QUERY`, which keeps its `cfg(any(unix, test))` and is asserted by a
  test that runs everywhere. CI's `windows` job is the backstop.
- **`run.rs` is 966 → 972 non-test lines.** Still 28 from the failure. The
  next change there should split it, as M47 said.

### Gates, all from the worktree

```
$ cargo fmt --all -- --check                                    # clean
$ cargo check --workspace --all-targets --locked                # Finished
$ cargo clippy --workspace --all-targets --locked -- -D warnings   # Finished
$ cargo test --workspace --locked                # 3527 passed, 0 failed
$ scripts/check_discipline.sh                                   # discipline ok
$ scripts/budget.sh    # dependencies (unique, normal): 331 (max 331); budget ok
$ cargo deny check                 # advisories ok, bans ok, licenses ok, sources ok
$ scripts/tui-smoke.sh                                          # tui-smoke ok
$ cargo test -p bingo --locked --test pty              # 5 passed, 0 failed
$ cargo check -p bingo-surface-tui --all-targets --locked \
      --target x86_64-pc-windows-msvc
                    # FAILS in aws-lc-sys' build script (ADR-0041's note)
$ cargo check -p bingo-sdk --all-targets --locked \
      --target x86_64-pc-windows-msvc                           # Finished
```
No known flake was hit. No crate joined the tree: the budget is 331 before and
after, and `bingo-pictures` gained a function, not a dependency.
