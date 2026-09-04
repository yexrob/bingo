# M69 — The opening shot

## Goal

User, 2026-09-05: an entrance for bingo — a five-second story told in
**cuts**, in a **three-dimensional world rendered as characters** (the
genre of the spinning torus and the ray-marched ASCII scene, where a
person really sees depth), ending on the mascot — the hooded cat-eared
girl in profile from `../bingo-site/public/mascot.png`, facing a
glowing block — and on the new welcome box, where that glowing block
becomes the composer's cursor. The cursor she looks at is yours.

This milestone is the **brick and the storyboard**, reviewed with the
user before anything is wired into the welcome box (M70 will be the
wiring, the skip, the short form and the settings). Nothing here
changes a shipped screen.

## Bricks

1. **A renderer that sees depth.** `bingo-surface-tui/src/intro/`
   (a module of the TUI, no new crate, no dependency): a ray-marcher
   over signed-distance fields — sphere, box, torus, plane, repeated
   lattice, union/subtract — with one directional light, one point
   light (the block, emissive), soft shadow by march, a fog for depth.
   Output is a cell grid: a luminance ramp (` .:-=+*#%@`, ASCII; a
   half-block/shade ramp where the glyph table allows) and a colour
   per cell from the theme's own tokens (`presence`, `glow`, `dim`,
   `text`) so it obeys `NO_COLOR`, ANSI, truecolor and light/dark.
   Cells are 1:2, so the camera's aspect corrects for it. Pure: `frame(
   scene, camera, t, width, height) -> Vec<Line>`; deterministic on
   `t`; budget **under 8 ms at 120×40 in debug** (measured, not
   pinned: the test asserts a step count, the plan records the time).
2. **The scenes, five, hard cuts.** One `Scene` value each; the cut
   times are constants in one table (`SHOTS`):
   - **0.0–1.0 Dark.** A black world. One emissive block (the cursor)
     rotating slowly at centre; six embers rising (points, the only
     particles). Establishes the one character.
   - **1.0–2.2 The lattice.** Cut. The camera is inside an endless
     repeated lattice of dark blocks — the codebase — dollying forward
     fast, the blocks' edges catching the light, fog eating the far
     end. The cursor block is not here yet.
   - **2.2–3.2 The find.** Cut. The dolly stops dead. One block in the
     lattice lights from inside (the emissive cursor), the lattice
     around it falls into fog. Half a second of stillness.
   - **3.2–4.5 Her.** Cut. A billboard quad in the world carrying the
     mascot (the PNG decoded once through `bingo-pictures`, sampled as
     luminance + a warm/cool split so the ramp and the theme's two
     colours draw her), camera in a slow parallax drift; the cursor
     block hangs before her face at the picture's own distance; the
     embers rise between them; the block breathes once.
   - **4.5–5.0 The hand-off.** The camera settles frontal; the world
     flattens into the welcome box: the border draws from the block
     outward, the block descends to where the composer's cursor will
     be, the wordmark `bingo` lights beneath her. Last frame = the
     new welcome box, still.
3. **The storyboard.** `intro::storyboard()` renders t = 0.0, 0.5,
   1.0, 1.6, 2.2, 2.8, 3.2, 3.9, 4.5, 5.0 at 100×30 into insta
   snapshots (`src/snapshots/…__intro__shot_<t>.snap`), and a
   `#[ignore]`d test writes the ten frames to `target/intro/*.txt`
   plus one animated preview the parent can look at (`target/intro/
   preview.gif` if a GIF encoder is already in the tree — check;
   otherwise ten PNGs through `bingo-pictures` at 8×16 px per cell,
   which is also what proves the frames read as depth).
4. **The end state.** The new welcome box, static: the mascot on the
   left (the character rendering on any terminal; the real picture on
   one that draws pictures — through the pictures seam, not a new
   route), greeting, help, cwd on the right; both palettes; ASCII
   fallback is a silhouette. Snapshot at 80×24 and 120×40. Not yet
   wired into `welcome.rs` — that is M70's.

## Files

`bingo-surface-tui/src/intro/{mod.rs,march.rs,sdf.rs,shade.rs,
scenes.rs,mascot.rs,storyboard.rs}`, `theme.rs` ledger if a token is
spent, `docs/design/tui.md` (a new §11 "The opening", the shot table,
the constraints M70 will hold: skip on any key, motion off = end
state, width < 60 = end state, never under `--print` or on resume),
the mascot PNG copied to `crates/bingo-surface-tui/assets/mascot.png`
(with the site's licence line if it has one — check).

## Exit criteria

- [ ] The march brick renders a sphere, a box and a torus that read
      as such in a 60×20 snapshot; the light and fog tests pin one
      cell each.
- [ ] Five scenes, ten storyboard snapshots, the preview files.
- [ ] 120×40 frame under budget: a step-count assertion and the
      measured time in Verified.
- [ ] The end-state welcome box snapshots, both palettes, ASCII.
- [ ] All gates; no dependency added.
- [ ] Reviewed by the user from the preview: appended by the parent.

## Non-goals

Wiring into the welcome box, the skip, the daily short form, the
settings (M70); sound; a 3D mascot (she is a billboard — a face in
characters needs the picture's own pixels, not a mesh); scenes wider
than the shot table.

## Risks

A ray-marcher in a debug build on CI: the budget test asserts steps,
not milliseconds. Light theme: the ramp inverts (dark ink on a light
ground) — the storyboard must be checked in both. The mascot at 24×12
cells is a suggestion of a face, not a face: the parallax and the
warm split are what sell it; if the preview does not read, the
fallback is the real picture only and a silhouette elsewhere, and the
plan says so rather than shipping a smear.
