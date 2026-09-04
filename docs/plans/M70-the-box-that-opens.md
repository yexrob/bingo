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

- [x] Half-block frames: fixture for the packing; storyboard snapshots
      at seven `t`s; PNG previews under `target/intro/`.
- [x] The last frame equals `welcome::lines` for the same state, both
      palettes.
- [x] It plays on a qualifying fresh session and not otherwise (table
      of cases, tested); any key skips; `--print` and resume never see
      it (black-box: the print run's stdout has no `▀`).
- [x] The draw never waits on a frame (the render is off-thread; a
      test on the run with a slow frame still paints).
- [x] Measured: ms per frame at 180×12 in debug and release.
- [x] All gates. (`scripts/tui-smoke.sh` was not run — out of scope by
      instruction; hands-on by the user is still owed.)

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

## Verified

2026-09-05, worktree `m70-opening`, branch `m70-opening` off `dev` `8c5d8814`.

### Commands

```
cargo fmt --all -- --check                                             # clean
cargo check --workspace --all-targets --locked -j 2                    # Finished, no warnings
cargo clippy --workspace --all-targets --locked -j 2 -- -D warnings    # clean
cargo test --workspace --locked -j 2 --no-fail-fast                    # 4 189 passed, 0 failed, 4 ignored
scripts/check_discipline.sh                                            # discipline ok (no new warning)
scripts/budget.sh                                                      # budget ok, 334 crates (unchanged)
cargo deny check                                                       # advisories ok, bans ok, licenses ok, sources ok
cargo test -p bingo --test pty --locked -j 2                           # 13 passed
```

### The frame, measured

`intro::storyboard::a_frame_of_a_wide_terminal_stays_inside_its_march_budget`,
sampling every tenth of a second of the piece at **180×12** — a wide terminal,
and the twelve rows the box gives up:

```
intro: 180x12 — worst frame t=1.8s at 139162 march steps; 27.0ms a frame over 41, slowest 61.4ms   # debug
intro: 180x12 — worst frame t=1.8s at 139162 march steps;  2.9ms a frame over 41, slowest  6.6ms   # release
```

The test asserts **139 162 steps against a budget of 400 000**: a step is the
same number on every machine and a millisecond is not.

So the release build draws the piece three times over inside a 33 ms frame, and
the debug build draws the worst of it (the orbit through the field) in about
two frames' worth. Neither costs a keystroke: the render is a `spawn_blocking`
per frame, one in flight at a time, and the draw takes whichever frame it has —
in debug that is roughly every second frame of the field shot, skipped rather
than queued, which is what the piece being a pure function of `t` buys.

### The previews

`cargo test -p bingo-surface-tui --lib -- --ignored intro::storyboard::preview`
writes, under `target/intro/`:

- `shot_{0_0,0_7,1_4,2_1,2_8,3_4,4_0}.txt` — the seven frames as text
- the same seven as `.png` (dark) and `_light.png` (light), 8×16 px a cell,
  the half blocks drawn as their actual halves

`cargo test -p bingo-surface-tui --lib -- --ignored intro::mascot::probe::sizes`
writes `her_{14x18,18x24,24x32,36x48}.png` — her billboard alone, at the pixel
sizes the shots draw her, so the sizing decision is made by looking.

`cargo test -p bingo-surface-tui --release -- --ignored intro::storyboard::play
--nocapture` plays the whole piece in the terminal it is run from. **Not run
here** — it takes the terminal over, and it exists for the user.

### What the frames look like

- **The floor (0.0–1.4).** A dark room. A ruled floor converges on a vanishing
  point a little above the middle of the box; the block hangs over it as a
  glowing bar with a halo, its shadow reading as a break in the rules running
  away to the lower right; a few motes of dust rise through its light. The
  camera dollies in and comes down over the shot, and the rules slide under it.
  This is the shot that carries the milestone's "more depth": the converging
  lines and the shadow are what say *there is a room here*, and fog on its own
  never did.
- **The field (1.4–2.8).** Black but for the block. The camera swings half a
  turn from behind it to nearly square on; dark blocks at two depths swing
  through, catching the light on the faces turned towards it, the near ones
  fast and the far ones barely at all. She comes in from the left of frame and
  ends to the right of the block, with the block hanging before her face.
- **The hand-off (2.8–4.0).** The world goes out, the block walks down to the
  box's corner and becomes its `✻`, the border walks out along both edges and
  closes at the far corner, the three rows light in the order they are read,
  and the box settles from twelve rows to six. The frame one tick before the
  end is already, exactly, the welcome box.

### Does the mascot read?

**At twenty-four pixel rows she is a hooded, cat-eared head in profile — a
figure, not a face.** The same finding as M69's, at the same size: the ears and
the lit profile survive, the eye and the mouth do not. `her_18x24.png` is the
frame to argue with; `her_36x48.png` is what she is at twice that, where the
hood, the jaw and both ears are unmistakable.

Her size is fixed by the geometry, not by the box: filling twelve rows means
twenty-four square pixels tall, and her crop's own shape then makes her
eighteen wide. Making her bigger means letting the ears leave the frame, which
trades the one thing that identifies her for detail that still would not show
an eye — so she stays whole and she is a figure. She is **not** a far
silhouette: she is the subject of the second shot and reads as a head.

Two things bought that, and both were fixed here:

- **`theme::lit` reaching the ground.** It bottomed out at `dim`, a mid grey on
  the dark palette, because M69's glyph ramp carried the brightness. With half
  blocks the colour is all there is, so her hood, the unlit field and the floor
  between its rules all came out as a milky wash and nothing read. Three stops
  — ground, half, whole — with `raised` as the ground fixed the whole picture at
  once, not only her.
- **One reduction, not two.** The picture is now filtered straight to about the
  size she is drawn at (56×72), keeping some of the brightest pixel under each
  sample, and graded at both ends before they are mixed. Reducing twice averaged
  her one-pixel rim away; mixing before grading pushed every sample that touched
  anything lit through the top of the window and flattened her.

### Snapshot changes

- The ten M69 storyboard frames, the four end-state boxes and the two mascot
  rectangles are **gone with the code that drew them**: the glyph ramp, its two
  tables, `intro/end.rs` (the piece lands on the real welcome box now) and the
  mascot's straight-on rectangle and silhouette. So are `Shape::Sphere` and
  `Shape::Torus` (a ball is a block with no extent and a round — one primitive,
  both surfaces) and solid subtraction, which the new shots do not use.
- Seven half-block frames and a packing fixture in both palettes replace them,
  plus seven screens of the box playing inside a whole frame
  (`screens/opening.rs`).
- Nothing outside the opening changed a byte: the token ledger moved
  `intro/end.rs` out and `intro/settle.rs`, `intro/shade.rs` and the new `half`
  in.

### Not done, not verified

- `scripts/tui-smoke.sh` was not run (out of scope by instruction), and neither
  was `intro::storyboard::play` — the user's own hands-on is still owed.
- **Windows was not cross-checked.** `cargo check -p bingo-surface-tui
  --all-targets --target x86_64-pc-windows-msvc` fails in this environment
  building `aws-lc-sys`'s C, which is a cross-toolchain limitation and not this
  change. Nothing here touches a process, a path, a signal or a clock beyond
  `std::time::Instant` and `tokio::task::spawn_blocking`, both portable; CI's
  `windows` job is the backstop.
- A session that was *resumed but has no items yet* would still be entered:
  freshness is read off the transcript, which is the one fact a surface has,
  and an empty session is one nothing has happened in.
- `crate::opening` (what a run has to say when it opens) and
  `crate::run::opening` (the piece) are two modules of the same word. The plan
  named the second; the first is M49's and untouched.
