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

- [x] `lap` and `beat` fixtures; six storyboard snapshots at two
      widths; the end frame equals `welcome::lines` (both palettes,
      both glyph tables).
- [ ] Plays in ANSI (`presence` yellow, `DIM` for the tail) and in
      ASCII; not under `NO_COLOR`. — **not done, on purpose**; see below.
- [x] Any key skips; `--print`/resume never see it (M70's tests kept).
- [x] `intro/` and the mascot asset are gone; no dead code.
- [x] All gates but the Windows cross-check; tui-smoke by the parent;
      hands-on by the user.

## Non-goals

A daily short form; a settings key; sound; anything the box does not
already contain when it rests.

## Risks

The beam over wrapped rows: `welcome::lines` wraps the cwd at narrow
widths; the sweep runs per *row*, so a wrapped continuation is its own
row 150 ms later — fine, and the fixture at 80 columns pins it. The
comet tail on the corners: the age function walks the perimeter as one
path, so a corner is just another cell.

## Verified

2026-09-05, worker M72 on `m72-opening` (cut from `dev` at `0598f67a`).

### What was built

`crates/bingo-surface-tui/src/opening/` — `beat.rs` (where a second stands
in each of the four beats, which overlap), `lap.rs` (the perimeter as one
path, and the age of the light on each cell), `cells.rs` (a row taken apart
into cells and put back with what shares a style in one span), `frame.rs`
(the beats composed over the box `welcome::lines` drew), `storyboard.rs`
(twelve snapshots, the PNG preview, and `play`) — plus `opening.rs`, which
holds `Playing`, the whole of which is the instant the piece began. The
run's own start-up notices moved to `run/notices.rs` to free the name.

Deleted: `intro/` entire (3 519 lines), `assets/mascot.png`, `theme::lit`,
`theme::half`, their ledger rows, their nine snapshots, `Reply::Opening`,
`opening::Rendered` and the `spawn_blocking` path. Net −2 408 lines.

Added to the vocabulary: `theme::hairline` (a border with light on it:
the hairline it rests as → `presence` → `glow`, keeping `through`, which
`lit` used to own), `clock::swept` (whether a sweep's light has reached a
cell at all, which is what a *revealed* run needs and `sweep` cannot say)
and `clock::swell` (the one shape of a breath, now shared by the
wall-clock `breath` and by a beat with a beginning).

### Deviations, and why

1. **The lap runs at an even speed**, not through `clock::ease_in_out`
   (plan, shot 1). Cubic ease-in-out peaks at three times the average
   speed, so with the tail's own overshoot the head was home at 0.55 s of
   a 0.9 s beat and the last third of it had nothing moving on it. Seen in
   the PNG previews at 0.6 s and 0.7 s. The surface has only two easings
   and the other one is worse here, so the light is spent evenly.
2. **The tail is a share of the perimeter (22 %), not ~12 cells** (plan,
   shot 1). At 120 columns the head travels eleven cells between two
   frames; a twelve-cell tail is one frame long and reads as a strobe.
3. **The tail runs through `theme::hairline`, not `theme::comet`** (plan,
   shot 1). `comet` cools to `text`, which is what streaming words rest as;
   a border rests as `dim`, so a comet tail on one ends bright and then
   steps down to grey. `hairline` is the same three stops the plan names —
   `glow` → `presence` → the hairline — and the breath uses it too, so the
   border's last beat starts and ends exactly where it rests. The beam over
   the words *is* `theme::comet`, exactly `view::beamed`'s recipe.
4. **The mark's ignition is one turn of the sparkle at the surface's own
   150 ms** (0.9–1.5 s), not 80 ms a frame (plan, shot 2). The plan names
   `theme::sparkle`, whose frame is `SPARKLE_MS`; at 80 ms it would be a
   second rhythm for the same glyph, and at 400 ms the beat ends on `✶`
   rather than on the resting `✻`.
5. **The beam crosses each row's own glyphs**, not the box's width (plan,
   shot 3). `cwd: /tmp/project` is 17 cells of a 76-cell box, so a
   box-wide beam revealed it whole in the first quarter of its beat. Rows
   still start 150 ms apart, and close the gap rather than run past the
   beat when a box has more rows than the beat has room for.
6. **The breath ends at 2.35 s and the piece at 2.40 s.** A breath that
   ended with the piece left the border one part in eighty warmer than
   `dim` on the last frame before the box landed — a step of colour on the
   hand-off. It now rests for two frames first, and the frame before the
   last is cell-for-cell the box.
7. **`frame(t, boxed)`, not `frame(t, width, state)`** (plan, brick 2).
   The width is the box's own rows; asking for it twice is a second
   representation of one fact, and it is wrong for a box too narrow to
   hold its own padding.
8. **`lap` answers with the box's cells, not a list of lit ones** (plan,
   brick 1): `Vec<Edge>`, one per cell — `Interior`, `Dark`, `Lit(age)`.
   A list would have made the caller ask a second time which cells are on
   the border at all, and that predicate and the walk must agree.

### What is not done

- **The exit criterion "plays in ANSI and in ASCII" is not met**, and the
  plan contradicts itself on it: *The shots* says ANSI gets the static box
  "as M70's rule already decides", and the parent's brief says to keep
  `welcome::opens` as it is. That rule gates the piece on twenty-four bits
  *and* on more than ASCII, so the piece plays in neither. It is kept as it
  stands. What was done instead: `frame` is correct in every look — the
  last frame is the box in the ASCII table as well as in both palettes
  (`the_last_frame_is_the_welcome_box_and_nothing_else`), and
  `theme::hairline` collapses to `dim`/`presence`/`glow` on eight colours
  (`a_border_with_light_on_it_runs_from_its_own_hairline_to_the_glow`), so
  opening the door is a one-line change to `opens` if the user wants it.
- **The Windows cross-check did not run**: `cargo check -p
  bingo-surface-tui --target x86_64-pc-windows-msvc` fails building
  `aws-lc-sys`, which needs a Windows toolchain this machine has not got —
  a pre-existing limit of the environment, not of this change. Nothing here
  touches a process, a path, a signal or a clock: the piece is a pure
  function of an `f32` and the only platform call in the module is
  `crossterm::terminal::size()` inside the `#[ignore]`d `play` test.
- **`scripts/tui-smoke.sh` was not run** (the parent's, per the brief), and
  neither was `opening::storyboard::play`, which takes over a terminal.
- **The piece has not been seen in motion.** It was judged from the twelve
  text snapshots and from PNG renderings of the frames (`--ignored
  opening::storyboard::preview`, temporarily widened to every 0.1 s while
  judging). Whether 2.4 s is the right length, and whether the beam's
  150 ms tread is right, are the user's to say.

### How it reads (asked for, and answered honestly)

At **80 columns** the light reads as a moving point with a tail: the head
is a single bright cell of `glow`, the ~37 cells behind it fall through
`presence` to the hairline, and the border already drawn is a pale rule.
Turning a corner reads as one motion, because the corner is one cell of the
same path. At **120 columns** the same is true and the tail is ~55 cells,
which is the point of making it a share — at a fixed twelve cells it broke
into dashes. In the light palette the head is a rust orange on paper and
reads as clearly.

Every frame is legible: nothing is ever drawn as a glyph that is not the
box's own. A row under its beam shows a *prefix* of its own text (`cwd`,
`/help for help · /login codex to use a`) rather than scrambled characters,
so there is no frame a person could mistake for garbage. The two frames
that are almost empty — the light on the top edge alone at 0.3 s, the mark
alone inside a finished border at 0.9–1.1 s — read as the piece pausing on
purpose, not as a missing draw. The one thing a still cannot show is
whether the border's closing breath is felt at all: it is a warm flush
across a hairline for a third of a second, and it may prove too quiet.

### Gates

```text
$ cargo fmt --all -- --check
(no output)

$ cargo check --workspace --all-targets --locked -j 2
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.37s

$ cargo clippy --workspace --all-targets --locked -j 2 -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.96s

$ cargo test --workspace --locked -j 2 --no-fail-fast
85 suites, 4156 passed, 0 failed, 2 ignored
(the two ignored are `opening::storyboard::preview` and `::play`)

$ cargo test -p bingo-surface-tui --locked -j 2 opening::
test result: ok. 32 passed; 0 failed; 2 ignored; 0 measured; 970 filtered out

$ scripts/check_discipline.sh
dependency direction ok
kernel names no tool
cohesion ok
discipline ok
(no warning names a file this milestone wrote or changed)

$ scripts/budget.sh
dependencies (unique, normal): 334 (max  334)
warm cargo check -p bingo-core: 0s (max  20s)
relink isolation: touching the TUI recompiled 0 crates for core (must be 0)
target/debug: 8 GB (soft max  5)
warn: target/debug exceeds the soft limit
test binaries: 57
budget ok

$ cargo deny check
advisories ok, bans ok, licenses ok, sources ok

$ cargo test -p bingo --test pty --locked -j 2
test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo check -p bingo-surface-tui --all-targets --target x86_64-pc-windows-msvc
error: failed to run custom build command for `aws-lc-sys v0.44.0`
(pre-existing: no Windows toolchain on this machine)
```

No dependency was added, so `budget.sh`'s count is unchanged. `target/debug`
over its soft limit is this worktree's build output and predates the change.
