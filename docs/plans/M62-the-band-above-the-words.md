# M62 — The band above the words

## Goal

User, 2026-09-04: a line a person sent with pictures shows them in the
transcript **stacked one under another below the words**, twelve rows
each — a wall, where the composer had shown the same pictures as a
three-row band of thumbnails side by side. Wanted: the `>` block reads
the way the box read before `⏎` — **the band of thumbnails above the
words, in a row**. The record and its preview are the same shape.

Tool results (`⎿` with a screenshot) and pictures an answer's words
named (M51) are not in this: a picture a tool returned *is* the result
and is read at its size; a picture the model wrote a link to stands on
its chip line. Only the person's own pictures change.

## Bricks

1. **One band brick.** `composer/strip.rs::banded` (thumbnails at
   `COLS`×`ROWS`, `GAP` between, `SHOWN` then `+N`) is the band. It
   moves out from under the composer into a place both can use —
   `graphics/band.rs` or `pictures/band.rs`, pure over `&[Picture]` —
   and the composer's strip becomes one caller of it. Same constants,
   same row count, same overflow count: one representation.
2. **A person's block wears it.** `transcript/pictured.rs::
   under_the_words` gains a third `Hangs` shape, or better a split:
   for an item a person sent (`Hangs::Said`, the `>` block) the
   pictures go **above** the words as one band; for a tool result
   (`Hangs::Returned`) nothing changes. The band's thumbnails are
   sources `Source::Journal { item, part }` as today, so the click that
   opens one (M56), the reconciler, and the decode memo all see the
   same picture at a new rectangle and nothing else. The band stands
   in the `>` block's own column (the `⏺`/`>` indent), after the
   block's first row is decided — look at how `item_lines` opens a
   person's block and put the band before its first words row, not
   before the mark.
3. **Fold.** `Shut` hides the band as it hides everything; `Peek` and
   `Open` both show the band at `ROWS` — a thumbnail has no taller
   form, `ctrl+o` on the block opens nothing about it. The click opens
   the picture in the viewer, which is where the whole picture is.
4. **Screens.** `screens.rs` catalogue: a `>` block with one picture,
   with three, with six (the `+2`), at both sizes; on a terminal
   drawing nothing the block keeps exactly the chip lines it has today
   (snapshot unchanged — the band is a graphics-only shape, as the
   composer's is).

## Files

`bingo-surface-tui/src/composer/strip.rs` (thinner), the new band
module, `transcript/pictured.rs`, `transcript.rs` (where a person's
block is opened), `screens.rs` + snapshots, `theme.rs` ledger if the
band module spends a token, `docs/design/tui.md` §5 (the image row
entry) + a dated line.

## Exit criteria

- [x] A `>` block with pictures: band of thumbnails above the words,
      row-wise, `SHOWN` then `+N`; snapshots at 80×24 and 120×40.
- [x] A tool result's picture is exactly where and how big it was.
- [x] A click on a thumbnail in the band opens the picture (the M56
      click test re-aimed at the band's rectangle — it is in `input.rs`,
      not `run.rs`).
- [x] The composer's strip is byte-identical (its snapshots unchanged).
- [x] All gates.
- [ ] Hands-on: appended by the parent.

## Non-goals

Changing the tool result's picture; a hover or a larger thumbnail on
the cursor; a band for M51's linked pictures.

## Risks

M61 is on `pictured.rs`'s `drawn` (decode off the frame) at the same
time — start from a `dev` that has M61 merged, or expect a conflict
there. A `>` block's first row also carries the `>` mark and the
person's first words; the band above them must not move the mark off
the block's first row in the plain (no graphics) case, which is the
`--print` shape and the channels' shape.

## Verified

*2026-09-04, worktree `.claude/worktrees/m62` on `m62-band`, base
`b26f1002` (M61 merged, so the decode seam was already there and the
risk section's remark about it was moot).*

### What landed

Four commits, the plan's four bricks.

1. **One band brick** (`855c2478`). `graphics/band.rs`: `Band { lines,
   pictures }` with `height()`, and one entry point
   `band::of(&[(Source, &Image)], cell, &Decoded, width) -> Band` — at
   most `fitting(width)` of them (`SHOWN` 4, never fewer than one), each
   `thumbnail`ed into `COLS`×`ROWS` (12×3) with `GAP` 1 between and `+N`
   dim on the floor row for the rest. `composer/strip.rs` keeps only the
   reading of the line — `held.shown(line)` mapped to `Source::Draft` —
   and calls it; `Strip` is gone, `Band` is what `view.rs` cuts rows for
   and draws. 7 tests in `band.rs` (the band's own rules), 5 left in
   `strip.rs` (the line's).
2. **A person's block wears it** (`d6ff20a1`). `under_the_words` splits
   on the item rather than on a `Hangs` arm: `returned` is what a tool
   answered with, unchanged to the row; `above_the_words` is the band,
   its thumbnails `Source::Journal { item, part }` at the band's
   rectangle, spliced in at `0..0` through the existing `at_column` at
   the `⏺`/`>` indent. `Hangs` itself is **deleted** — see below.
3. **Fold.** `Shut` returns before any of it, as it always did; `Peek`
   and `Open` are the same three rows and the same `Picture`s, which is
   what `the_fold_hides_the_band_and_never_grows_it` asserts by
   comparing the two blocks outright.
4. **Screens** (`c826a6e0`, and the click in `805579b4`).
   `screens/pictures.rs`: one picture, three, six (`+2`), and the same
   line with graphics off, each at 80×24 and 120×40 — eight new
   snapshots, read line by line before they were kept. The click test is
   `input::tests::a_click_on_the_band_above_a_persons_line_opens_it`.

916 lib tests in `bingo-surface-tui`, +14 net: 7 new in `band.rs`, 4 in
`pictured.rs`, 4 screens and 1 click, less the 2 that left `strip.rs`
with the rows they were about. 3863 in the workspace.

### What the plan got wrong

- **The click test is `input.rs`'s, not `run.rs`'s.** M56's five click
  tests live beside `on_mouse`; `run.rs`'s `OpenPicture` tests are about
  the opener, and neither of them moved.
- **`Hangs` did not gain a third shape; it lost its reason to exist.**
  With the band above and the result below, `Hangs::Said`'s `under` and
  `chip` arms had no caller left, and `Returned`'s were the tool path's
  alone. What the two still shared was one line of arithmetic, so the
  enum became `room(columns) -> u16` and `chip(&Image) -> Line`, and the
  `Where { item, part, hangs }` carrier went with it. A picture an
  answer's words named (M51) spells its own column at its one call site,
  which is where the `indent` it uses comes from anyway.
- **`screens.rs` could not hold the scenes.** It is at 975 non-test
  lines against a hard 1000 and every line of it counts (it has no
  `#[cfg(test)]`), so the catalogue is `screens/pictures.rs`. The
  snapshots still land in `src/snapshots/…__screens__<name>_<size>.snap`,
  because the `insta` macro's call site is `both()` in `screens.rs` —
  which is how `acp` and `thinking` already work.
- **A thumbnail is three rows tall and rarely twelve columns wide.** The
  fixtures are a wide screenshot, a tall shot and a square; the row
  limit binds for all three, so they draw 9, 4 and 6 columns. `COLS` is
  a ceiling for a picture taller than it is wide, not a width.

### The band, as it draws (three pictures, 80×24)

```
  􎻮̅̅􎻮̅̍􎻮̅̎􎻮̅̐􎻮̅̒􎻮̅̽􎻮̅̾􎻮̅̿􎻮̅͆ 􎻮̅̅􎻮̅̍􎻮̅̎􎻮̅̐ 􎻮̅̅􎻮̅̍􎻮̅̎􎻮̅̐􎻮̅̒􎻮̅̽
  􎻮̍̅􎻮̍̍􎻮̍̎􎻮̍̐􎻮̍̒􎻮̍̽􎻮̍̾􎻮̍̿􎻮̍͆ 􎻮̍̅􎻮̍̍􎻮̍̎􎻮̍̐ 􎻮̍̅􎻮̍̍􎻮̍̎􎻮̍̐􎻮̍̒􎻮̍̽
  􎻮̎̅􎻮̎̍􎻮̎̎􎻮̎̐􎻮̎̒􎻮̎̽􎻮̎̾􎻮̎̿􎻮̎͆ 􎻮̎̅􎻮̎̍􎻮̎̎􎻮̎̐ 􎻮̎̅􎻮̎̍􎻮̎̎􎻮̎̐􎻮̎̒􎻮̎̽
> which of these has the right margin? [image 1] [image 2] [image 3]
```

With six, the floor row ends `… +2`. With graphics off, the four rows
above are not there at all and the `>` row is the whole block — the
snapshot `sent_pictures_undrawn_80x24` is that block beside the same
`⏺` answer, and it is the shape `--print` and a channel see.

### Gates, all from the worktree, `-j 2`

```
$ cargo fmt --all -- --check                                 # silent, exit 0
$ cargo check --workspace --all-targets --locked              # Finished
$ cargo clippy --workspace --all-targets --locked -- -D warnings
                                                              # Finished
$ cargo test --workspace --locked --no-fail-fast
    exit 0; 81 result lines, 0 with a failure; 3863 passed, 0 failed
$ scripts/check_discipline.sh
    dependency direction ok / kernel names no tool / cohesion ok / discipline ok
    (pre-existing warns only; screens.rs 973 → 975, no other TUI file moved)
$ scripts/budget.sh
    dependencies (unique, normal): 332 (max  332)
    warm cargo check -p bingo-core: 0s (max  20s)
    relink isolation: touching the TUI recompiled 0 crates for core (must be 0)
    budget ok            (`target/debug` over its soft limit, as before)
$ cargo test -p bingo --test pty                     # 11 passed, 0 failed
```

No crate joined the tree: 332 before and after. The known
`bingo_plugin_rpc::connection` flake was not hit.
`scripts/tui-smoke.sh` was not run (the brief says not to).

### What is not verified

- **No real terminal drew any of this.** As in M46, M48, M49, M51, M56
  and M61, every terminal in these tests is one this repository wrote.
  What is proven is which cells carry which picture's number, where
  those cells are, and what a click on them produces. Whether a band of
  thumbnails above a `>` line *reads* well is the hands-on line's, and
  it is the one this milestone was asked for.
- **Nothing here drove the wire.** The band changes a rectangle, not a
  reconciler: the same `Source::Journal` at a new size goes through
  `Stored` and `Decoded` exactly as M61 left them, and no test in this
  milestone watches the bytes. What says the seam is honoured is
  structural — `band::thumbnail` calls `Decoded::size`, which is a
  header read, and nothing here calls `pixels` or `fitted` at all.
- **A room's own post with a picture was not looked at.** A post
  (`Driver::Log`) and a delivery from a quiet surface are `ItemBody::
  User` too, so they take the band as well. That matches what they did
  before (they took the pictures under their words), and the case where
  the rows are dropped altogether (`rooms_machinery`) still draws a band
  with nothing above it, as it drew a picture with nothing above it
  before. Unchanged, untested, and not in this plan.
- **The Windows cross-check for the TUI cannot run here**, for
  ADR-0041's recorded reason (`reqwest` → `rustls` → `aws-lc-sys`, whose
  build script wants `windows.h`). This milestone adds no `cfg`, no
  signal, no clock, no process and no path: it is `Line`s and `u16`s.
  CI's `windows` job is the backstop.
- **`screens.rs` is 25 non-test lines from the hard cap.** It grew by
  two here and the scenes went to a submodule for that reason; the next
  change to it should split it rather than find another two.
