# M72 — The line that draws itself

## Goal

User, 2026-09-05, after seeing M70: the opening stays inside the welcome
box, but it is **redesigned from nothing** — every element new, no 3D
asked for, "make it what you think is beautiful and in the product's
own key". The parent's design, chosen from three (below), is this: the
box's own furniture arriving in the product's own motion vocabulary. No
world, no camera, no pixels. The box is drawn by one point of warm
light, the mark ignites, the words come in on a beam, the border
breathes once, and it is the welcome box. About 2.4 s, at the box's
resting height — nothing on the screen moves but the box's own cells,
so the composer never jumps (design §3, *nothing jumps*).

## The anchor, and what was compared

Design §4: one warm colour, and it is the only one that moves; §6:
motion reports a state change, stillness is the default, when motion
stops the words remain. The product already has three motions and this
piece is made of exactly those, so it looks like bingo and not like a
demo: the **comet** (`theme::comet`, the tail on streaming text), the
**beam** (`view::beamed`, the light across the working word), the
**breath** (`theme::breath`, the input border while the model works),
and the **sparkle** (`theme::sparkle`, `✻ ✢ ✶ ✽`).

Compared and set aside: *embers converging into the mark, words
resolving out of random glyphs* — the decode effect is a generative
cliché and random glyphs are unreadable frames; *a bare breath and a
fade* — too quiet to hold the eye for even a second. The chosen one
has the strongest first eye (a moving light on a dark ground), a second
eye that is the product's own mark, and a third that is the words.

## The shots (one continuous take, no cuts)

Times in seconds from `started`; `p` is progress within a beat; every
beat is a pure function of `t`.

1. **0.0–0.9 The line.** The box's border does not exist yet. A point
   of light (`glow`) leaves the top-left corner and runs the perimeter
   clockwise; behind it a tail of ~12 cells fades `glow → presence →
   dim` (`theme::comet` by age), and behind the tail the border stays
   as the resting hairline in `dim`. Ease-in-out over the lap
   (`clock::ease_in_out`). Interior: empty. At 0.9 the head is home.
2. **0.9–1.3 The mark.** Where the head stopped, one cell inside the
   corner on the greeting row, the mark ignites: `✻ ✢ ✶ ✽` at 80 ms
   each (`theme::sparkle` from `t − 0.9`), ending on the resting `✻` in
   `presence`. The head's light is spent on it — one light on screen at
   every instant, never two.
3. **1.1–2.0 The words.** The greeting, the help line, the cwd — each
   row arrives under a beam that sweeps left to right (`clock::sweep`
   with `theme::comet(1 − sweep)`, exactly `view::beamed`'s recipe):
   ahead of the beam the row is blank, under it the glyphs are lit
   `glow`, behind it they settle to their resting style (`text`, `dim`).
   Rows start 150 ms apart; the M63 update row, when there is one,
   arrives last the same way.
4. **2.0–2.4 The breath.** The border takes one breath
   (`theme::breath` over `clock::ease_in_out` of the beat) and rests.
   The last frame is byte-identical to `welcome::lines` (test).

Skip on any key (kept from M70): the key is consumed, the end state is
drawn. Reduced motion (`BINGO_MOTION`), `--print`, resume, a
sub-session, a room, ANSI-only or `NO_COLOR`: the static box, as M70's
rule already decides. ASCII glyph table: the sparkle's ASCII frames
and `-`/`|`/`+` border — the piece still plays, in the table's own
glyphs. Width: the lap and the beam scale with the box's width, so it
plays at 80 columns and at 200.

## Bricks

1. **`opening::lap(t, width, height) -> Vec<(usize, usize, f32)>`**
   (pure): the perimeter cells with the age of the light at each, for
   the head at progress `p`; fixture at 10×3. **`opening::beat(t) ->
   Beat`**: which beat and its `p`; table test on the boundaries.
2. **`opening::frame(t, width, state) -> Vec<Line>`**: the four beats
   composed over `welcome::lines`' own rows — the words are *those*
   rows with each span's glyphs revealed and styled by the beam, so
   the resting frame is the same function's output at `t ≥ END`
   (identity test both palettes, both glyph tables). Storyboard
   snapshots at `t` = 0.3, 0.9, 1.1, 1.5, 2.0, 2.4 at 80 and 120
   columns.
3. **Wiring**: M70's `Ui::intro`, `welcome::opens`, the skip and the
   run-loop hooks stay; the off-thread frame goes (this frame is
   microseconds — it is drawn in the draw). `run/opening.rs` shrinks
   accordingly. The `play` test stays for the user.
4. **Subtract.** The whole of `intro/` — marcher, sdf, shade, scenes,
   embers, grid, settle, mascot, storyboard — and `assets/mascot.png`
   go; the theme ledger rows and `theme::lit`/`theme::half` with them;
   design §11 is rewritten to this piece. `bingo_pictures::pixels`
   stays (the user's call, M70). Nothing ships dead, and nothing of the
   old piece is kept "for later".

## Files

`bingo-surface-tui/src/opening/{mod.rs,lap.rs,beat.rs,frame.rs}` (new
module in place of `intro/`), `run/opening.rs`, `welcome.rs`, `ui.rs`,
`theme.rs` (ledger), `docs/design/tui.md` §11 + dated line, this plan.

## Exit criteria

- [ ] `lap` and `beat` fixtures; six storyboard snapshots at two
      widths; the end frame equals `welcome::lines` (both palettes,
      both glyph tables).
- [ ] Plays in ANSI (`presence` yellow, `DIM` for the tail) and in
      ASCII; not under `NO_COLOR`.
- [ ] Any key skips; `--print`/resume never see it (M70's tests kept).
- [ ] `intro/` and the mascot asset are gone; no dead code.
- [ ] All gates; tui-smoke by the parent; hands-on by the user.

## Non-goals

A daily short form; a settings key; sound; anything the box does not
already contain when it rests.

## Risks

The beam over wrapped rows: `welcome::lines` wraps the cwd at narrow
widths; the sweep runs per *row*, so a wrapped continuation is its own
row 150 ms later — fine, and the fixture at 80 columns pins it. The
comet tail on the corners: the age function walks the perimeter as one
path, so a corner is just another cell.
