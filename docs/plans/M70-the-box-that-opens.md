# M70 — The box that opens

## Goal

User, 2026-09-04, after the M69 storyboard: the opening plays **inside
the welcome box**, not on the whole screen; a **higher
resolution** than one glyph per cell; **more depth** — a world a person
can see into. It plays on **every start**, in that box, and ends as the
box the transcript has always had (`✻ Welcome to bingo!`, the help line,
the cwd). The box is 12 rows tall while it plays (the parent's call:
resolution is rows, and 12 is the most the composer can give up) and
settles to its usual height on the last cut.

M69's bricks stay — the marcher, the distance fields, the lights, the
fog, the `t`-pure frame, the shot table — and its storyboard is
replaced: this milestone is the new picture, the new shots, and the
wiring.

## Shape

**Half-block truecolor.** A cell is `▀` with its foreground the upper
pixel and its background the lower one: a 180×12 box is 180×24 pixels,
every one a full truecolor sample. That is what a terminal has for
pictures without a graphics protocol, and it is what makes shading read
as shading rather than as a ramp. `shade::render` gains this mode as a
second output beside the glyph ramp; the ramp stays the fallback for
`BINGO_ASCII`, ANSI and `NO_COLOR` — there the box does not play at all
and the static box is drawn (a dithered ramp at 12 rows is not worth
the wait). Colours stay the theme's own: the world in `dim`→`text`, the
block in `presence`→`glow`, as `theme::lit` already mixes them; both
palettes, since the box may be on paper (M71 may swap the look mid-play
— read the tokens per frame, hold nothing).

**Depth, four ways, all in the scenes.** A floor with a perspective
grid that vanishes into the fog; a camera that moves (dolly, then
orbit); near things large and fast, far things small and slow; a
shadow the block casts on the floor (the marcher's soft shadow already
exists — spend it on the floor). Fog alone is not depth.

**Three shots, ~4 s, hard cuts.**

1. **0.0–1.4 The floor.** Black. A grid floor recedes to a vanishing
   point; the emissive block stands at mid-distance, turning slowly,
   its shadow on the floor, a faint halo in the fog. The camera dollies
   in, low.
2. **1.4–2.8 The field.** Cut. The camera orbits the block a half
   turn at a slight tilt; a field of dark blocks floats around it at
   three depths, so the near ones sweep past and the far ones barely
   move. The block is the only light; the field's near faces catch it.
   She is here (the user's call, 2026-09-04, reversing the "no mascot"
   above): the mascot as M69's billboard, sampled through
   `bingo_pictures::pixels` into the half-block grid, cropped to the
   head, the block hanging before her face as the orbit ends. At 24 px
   she is a hooded, cat-eared head in profile or she is a silhouette in
   the far field — the frame decides, and Verified says which.
3. **2.8–4.0 The hand-off.** Cut. The camera settles frontal. The
   block descends to the box's top-left corner and becomes the `✻` mark
   (one mark on screen at every instant — M69's `descending`); the
   border draws from that corner along both edges; the box shrinks
   from 12 rows to its resting height as the world fades into the
   ground; the greeting, the help line and the cwd light up in order.
   The last frame is byte-identical to `welcome::lines` (test).

**Wiring.** The welcome block is derived at render time from
`SessionState` (`welcome::lines`); the opening is a `Ui` fact —
`ui.opening: Option<Opening { started: Instant }>` — set when the
surface opens a fresh top-level model session on a terminal that
qualifies, and read by the welcome block, which draws the frame for
`t = now - started` instead of the static box while it stands. Frames
are rendered **off the draw thread**: a `spawn_blocking` per frame
returning `Reply::Opening(Rendered)`, the draw taking the newest one it
has and never waiting (M69 measured 41 ms debug at 120×40; the box is
smaller, and the release build is the one that ships). `animating()`
is true while it plays. **Any key skips** to the end state (the key is
consumed, not typed); `esc`, `ctrl+c` too. It does not play under
`--print`, on `--resume`/`--continue`, in a sub-session or a room, when
`BINGO_MOTION` says off, when the terminal is under 80 columns or 16
rows, or without truecolor. The welcome box's M63 update row is drawn
on the end state as today.

## Bricks

1. **`shade::pixels`**: the marcher's grid at 2× vertical, and
   `shade::halves` turning two pixel rows into one row of `▀` spans
   with fg/bg from the tokens. Pure; a 4×2 pixel fixture pins the
   packing and the colours in both palettes.
2. **`sdf::floor` + `scenes`**: the plane with a grid (a repeated
   thin-box union, or a texture on the plane's hit — cheaper), the
   field of floating blocks with three depth bands, the orbit camera.
   The three shots in `SHOTS`. Storyboard snapshots at 100×12 pixels
   rows (`t` = 0.0, 0.7, 1.4, 2.1, 2.8, 3.4, 4.0), as text with the
   half-block glyph, and the PNG previews at 8×16 px a cell so the
   parent can look; the `play` test kept, playing in the box's
   dimensions.
3. **`welcome` + `run`**: `Opening` on `Ui`, the qualifying rule
   (`welcome::opens(state, screen, env) -> bool`, pure, tested case by
   case), the off-thread frame, the skip, the settle to resting height,
   the end-state identity test.
4. **Subtract.** Only what the rewrite leaves dead: the glyph-ramp
   silhouette in `end.rs` if nothing draws it, and the §11 paragraphs
   the new storyboard replaces. `intro/mascot.rs`, `assets/mascot.png`
   and `bingo_pictures::pixels` **stay** (the user's calls, 2026-09-04).

## Files

`bingo-surface-tui/src/intro/{shade.rs,sdf.rs,scenes.rs,storyboard.rs,
mod.rs}`, `welcome.rs`, `run.rs` (+ `run/opening.rs`), `ui.rs`,
`keys.rs` (the skip), `docs/design/tui.md` §11 rewritten to this
storyboard + dated line, `docs/plans/M69-the-opening-shot.md` gets a
one-line pointer to this plan under Verified.

## Exit criteria

- [ ] Half-block frames: fixture for the packing; storyboard snapshots
      at seven `t`s; PNG previews under `target/intro/`.
- [ ] The last frame equals `welcome::lines` for the same state, both
      palettes.
- [ ] It plays on a qualifying fresh session and not otherwise (table
      of cases, tested); any key skips; `--print` and resume never see
      it (black-box: the print run's stdout has no `▀`).
- [ ] The draw never waits on a frame (the render is off-thread; a
      test on the run with a slow frame still paints).
- [ ] Measured: ms per frame at 180×12 in debug and release.
- [ ] All gates; tui-smoke by the parent; hands-on by the user.

## Non-goals

A daily short form (every start plays, the user's
call); sound; a settings key (`BINGO_MOTION` is the switch);
graphics-protocol pictures for the opening.

## Risks

A 12-row box on a 24-row terminal leaves 12 for the composer and the
status line — enough; under 16 rows it does not play. A key pressed to
skip that is also a command (`/`) must not be lost into the composer:
the skip consumes exactly the one key. tmux: half-block truecolor is
fine; the frame rate is the pane's, and a late frame is skipped, never
queued.
