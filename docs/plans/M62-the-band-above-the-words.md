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

- [ ] A `>` block with pictures: band of thumbnails above the words,
      row-wise, `SHOWN` then `+N`; snapshots at 80×24 and 120×40.
- [ ] A tool result's picture is exactly where and how big it was.
- [ ] A click on a thumbnail in the band opens the picture (the M56
      `run.rs` test re-aimed at the band's rectangle).
- [ ] The composer's strip is byte-identical (its snapshots unchanged).
- [ ] All gates.
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
