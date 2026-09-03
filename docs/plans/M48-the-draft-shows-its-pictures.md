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

- [ ] The four named terminals draw at or above their floors; WezTerm,
  Konsole, Warp and xterm.js draw the chip; an `OK` with no XTVERSION
  is the chip.
- [ ] A 4000×3000 PNG in a 12-row block is sent at the block's pixel
  size; a thumbnail is sent at ≤ 3 rows' pixels.
- [ ] The strip appears, sizes, cuts at four, and leaves with its token;
  the terminal is told to forget it after submit.
- [ ] Every AGENTS.md gate; budget 331; `tui-smoke` and the PTY kitty
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
