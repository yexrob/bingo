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

- [x] The march brick renders a sphere, a box and a torus that read
      as such in a 60×20 snapshot; the light and fog tests pin one
      cell each.
- [x] Five scenes, ten storyboard snapshots, the preview files.
- [x] 120×40 frame under budget: a step-count assertion and the
      measured time in Verified. (The step assertion holds; the debug
      *time* does not meet the 8 ms the Goal guessed — see Verified.)
- [x] The end-state welcome box snapshots, both palettes, ASCII.
- [x] All gates; no dependency added.
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

## Verified

2026-09-04, worktree `m69-intro`, branch `m69-intro` off `dev` `3f0de865`.

### Commands

```
cargo fmt --all -- --check                                    # clean
cargo check --workspace --all-targets --locked -j 2           # Finished, no warnings
cargo clippy --workspace --all-targets --locked -j 2 -- -D warnings   # clean
cargo test --workspace --locked -j 2 --no-fail-fast           # 4 105 passed, 0 failed, 3 ignored
scripts/check_discipline.sh                                   # discipline ok (no new warning)
scripts/budget.sh                                             # budget ok, 334 crates (unchanged)
```

### The frame, measured

`intro::storyboard::a_frame_of_a_large_terminal_stays_inside_its_march_budget`,
sampling every tenth of a second of the piece at 120×40:

```
intro: worst frame at 120x40 is t=2.3s, 181518 march steps, slowest wall time 41.15ms   # debug
intro: worst frame at 120x40 is t=2.3s, 181518 march steps, slowest wall time  6.59ms   # release
```

The test asserts **181 518 steps against a budget of 240 000**, as the plan
asks: a step is the same number on every machine and a millisecond is not.

**The Goal's "under 8 ms at 120×40 in debug" is not met and is not reachable.**
`[profile.dev]` is `opt-level = 0`; the same frame is 6.6 ms optimised and
41 ms unoptimised, a factor of six that is the build profile and not the
algorithm. The horizon cut (a ray stops where the fog has taken all but 2 % of
whatever it could still find) and skipping shadow walks that cannot change a
cell took the worst frame from 217 687 steps to 181 518, ~16 %; the rest is
the march itself. What M70 needs:

```
intro::storyboard::play, 80×24, the whole five seconds at clock::FRAME:
  debug    152 frames, 9.43 ms a frame drawing
  release  152 frames, 1.61 ms a frame drawing
```

So at a normal terminal size the piece plays inside a 33 ms frame in either
profile, and at 120×40 it needs the release build — or M70's off-thread
render, which is the plan either way.

### The previews

`cargo test -p bingo-surface-tui -- --ignored intro::storyboard::preview`
writes, under `target/intro/`:

- `shot_{0_0,0_5,1_0,1_6,2_2,2_8,3_2,3_9,4_5,5_0}.txt` — the ten frames as text
- the same ten as `.png` (dark) and `_light.png` (light), 8×16 px a cell

`cargo test -p bingo-surface-tui --release -- --ignored intro::storyboard::play
--nocapture` plays the whole piece in the terminal it is run from, at the
surface's own frame clock.

### What was compared, and what was chosen

- **Two ramps.** ` .:-=+*#%@` against ` .:-=+*#%@` with the shade blocks
  `░▒▓█` on top. The shade ramp won for the world — `#` and `%` leave holes a
  lit face reads as noise, `▓` and `█` fill evenly — and the punctuation ramp
  is what `BINGO_ASCII=1` gets. Both ship; `theme::glyphs()` picks.
- **Two camera plans for the lattice.** A tunnel through the frames' holes
  against flying the diagonal shaft between four columns of blocks. The
  tunnel won: the shaft reads as four slabs closing in, the tunnel reads as
  *distance*, and the same geometry gives the third shot its best frame —
  the block glowing inside a receding square window. The hole was then
  widened (`half` 0.68 → 0.97 against a 3.0 period) because at the first
  size the lens saw wall at both edges and the shot read as a room.
- **Two lightings.** A warm/cool duotone spending `mode` on the ambient
  against warm/neutral spending only `dim`→`text` and `presence`→glow. The
  neutral won on the source's own evidence: the mascot is a monochrome warm
  picture, a blue ambient fights it, and §4's "one warm colour, one cool, each
  with a job" is left alone. No new token is spent.
- **The block's halo, round against tall.** Round made an upright caret read
  as a ball. It is now an ellipse, 2.8× taller than it is wide.
- **Her grading, measured not guessed.** `intro::mascot::probe::histogram`
  prints the crop's own luminance percentiles: p25 0.045, p50 0.049, p70
  0.055, p93 0.216, p99 0.438, p100 0.809 — seven tenths of the picture is one
  flat dark value. The ramp is spent on the window 0.048 → 0.45 with a 0.8
  curve, so the dark comes out as air and the rim reaches the top.

### Does the mascot read?

**At the sizes the shots draw her, yes; in the welcome box, as a figure and
not as a face.** In shot four she is ~30 cells wide and the two cat ears, the
hood and the lit profile all read (`target/intro/shot_3_9.png`). In the end
state at 20×13 the ears and the profile edge survive and the face does not:
she reads as a hooded, cat-eared head in profile. Below 20×13 — the sizes
first tried, 16×9 and 20×11 — she is a warm smudge and should not ship.

Three things bought most of that: a crop tightened onto the head rather than
the whole square picture, sampling **nine** points of the field per cell with
55 % of the weight on the brightest of them (a plain average buries a rim one
sample wide), and the measured grading window above.

**If the box is judged too tall at thirteen rows**, the plan's own fallback
stands and should be taken rather than shrinking her: the real picture through
the pictures seam on a terminal that draws pictures, and the ASCII silhouette
— which is already built and snapshotted — everywhere else.

### What became of it

M70 (`docs/plans/M70-the-box-that-opens.md`) is the wiring, and it replaced the
storyboard while it was there: the piece now plays in half-block pixels inside
the welcome box, in three shots rather than five. The brick — the marcher, the
distance fields, the lights, the fog, the `t`-pure frame, the shot table —
stands.

### Not done, not verified

- Nothing is wired: `mod intro` carries `#[allow(dead_code)]` and no screen
  draws it. M70's.
- `scripts/tui-smoke.sh` was not run (out of scope by instruction).
- Windows was not cross-checked: nothing here touches a process, a path, a
  signal or a clock — the module's only clock is the `f32` second its caller
  hands it.
- The light palette is checked in the storyboard PNGs and reads correctly, but
  it is a much fainter picture than the dark one: the world is ink on paper and
  there is simply less of it. Nothing is wrong; it is worth a look before M70.
