# M52 — The beat on the person's side

## Goal

The user wants the TUI to feel young and alive ("年轻的有活力的感觉").
The research of 2026-09-04 (`docs/design/effects-research.md`, taken
by the user as recommended, item 7 dropped) says that is not more
animation — §6's table is nearly full — but three things bingo
under-spends: a beat on the *person's* gestures (today the model moves
and the person never does), a rhythm that tracks the work, and frames
that do not tear. No library: tachyonfx would be +5 crates against a
full budget of 331 for an engine `clock.rs`/`theme.rs` already have;
ttfx is a static-text CLI. Everything here is a style ramp over rows
that already redraw, written with `Anim`, `ease_out` and `theme::mix`,
and every one reports a state change §6 would sign off on. Nothing new
appears on screen; nothing moves a row.

## Bricks

0. **Synchronized output.** Every draw is wrapped in DEC mode 2026:
   `crossterm::terminal::BeginSynchronizedUpdate` before the frame's
   bytes and `EndSynchronizedUpdate` after, in `terminal.rs`'s `draw`
   (one place; `Stored`'s graphics bytes go inside the same bracket so
   a picture lands with its cells). Under tmux the two sequences go
   bare — tmux understands 2026 itself (verify in tmux's `tty.c` /
   `CHANGES`, say so in Verified). Unknown to a terminal → ignored.
   `TestBackend` test: the frame's byte stream starts with `CSI ?2026h`
   and ends with `CSI ?2026l`; the PTY smoke sees them once per frame.
1. **Send ignition.** On `⏎` that submits, the input box's border
   runs one sweep left→right, `presence` → glow, over 200 ms (6 frames
   of the 33 ms tick), and the new `>` block's `raised` ground fades in
   over 3 frames. One `Anim` started at submit, kept on `Ui` beside the
   other motion state (find where the completion flash's instant lives
   and sit beside it), read by `view::border` and by the transcript's
   ground for that block. `BINGO_MOTION=off` → the block appears at
   rest; `NO_COLOR` → nothing (colour never carries a fact alone, §4).
   Tests in `motion.rs` with the injected clock: the border's style at
   0, 100, 200 ms; a second `⏎` restarts it; off → rest.
2. **Tool-done sweep.** The one-frame bold flash (`transcript.rs`
   ~583, `motion::a_completion_flashes_bold_for_exactly_one_frame`)
   becomes `good` sweeping left→right across the row's name over
   200 ms and settling. Same brick as 1 — one `sweep(anim, column,
   width) -> Style` function in `theme` or `motion`, used by both.
   Motion off → straight to `good`, as today. The flash test is
   re-aimed, not deleted: it becomes "a completion sweeps for six
   frames and is at rest on the seventh".
3. **The breath tracks the work.** `clock::breath(now, period)`
   already takes a period; `view::breathing` passes a period derived
   from what the session is doing: output tokens arriving → 0.9 s,
   a tool blocking → 2.2 s, thinking → 1.6 s (today's constant). The
   facts come from `SessionState` (what the activity row already reads
   to pick its verb) — no new state. Test: the three periods from three
   states; the phase is continuous across a change (no jump: derive
   the phase from an accumulated clock, or accept and document a
   ≤1-frame discontinuity — measure and say which).
4. **The card's border breathes.** The permission card's `presence`
   border takes `theme::attention(now)` while unanswered; answered or
   dismissed → rest. Motion off → `presence` (attention already does
   this). `TestBackend` test on the card's border style across a beat.
5. **Progress finished.** `views/progress.rs`: the bounded fill ramps
   `presence` → glow across its lit run (§4's second sanctioned
   gradient); the unbounded bar's 3-cell `SHEEN` walks the track from
   `clock::phase` at ~4 fps. Motion off → sheen parked at the head,
   gradient stays (it is not motion). Snapshot tests with the injected
   clock at two phases, and under `Colors::Plain`.
6. **An error flares and cools.** A failed tool or turn's row text
   starts at `bad` and ramps to `text` over 400 ms (`theme::fading`
   with its ends swapped — make it one function with a direction, not
   two). No shake, no jitter (§3 "nothing jumps", the 2026-09-02 rise
   withdrawal). Motion off → today's `bad` bullet and plain text.

Order: 0, 3, 5, 4 first (no taste risk, each a small commit), then 1,
2, 6 (the new beats). One commit per brick.

## Files

`bingo-surface-tui/src/{terminal.rs,view.rs,clock.rs,theme.rs,
motion.rs,transcript.rs,views/progress.rs,permission.rs,ui.rs}`;
`run.rs` only if the submit needs one line to start the `Anim` — it is
at 976 non-test lines and fails at 1000, and another worker is editing
it (M51: `Reply` seam, markdown pictures) — keep out of it if any other
seam works; `docs/design/tui.md` §6's motion table gains the six rows
(dated), §10 a decision entry naming the research and what was refused.

## Exit criteria

- [x] Every frame is bracketed by mode 2026 — on a bare terminal, and
  deliberately *not* through a multiplexer (Verified). A `TestBackend` sees
  cells and never bytes, so the bracket is pinned by a unit test against
  crossterm's own spelling and by a real pty.
- [x] `⏎` sweeps the border; a tool's completion sweeps its name; an
  error flares; the breath's period follows the state; the card
  breathes; progress has its gradient and sheen — each with a test on
  the injected clock and each at rest under `BINGO_MOTION=off`. The `>`
  block's ground fade did not land (Verified).
- [x] `NO_COLOR` loses no fact (each cue's degrade named in §6).
- [x] Every AGENTS.md gate; budget 331; tui-smoke; pty; no new crate.
- [ ] Hands-on (main session with the user): the seven, seen.

## Non-goals

Item 7 (session-switch wipe). Any library. Springs, typewriters,
character-rewriting reveals, hue cycling, particles, a second spinner,
anything that moves a row — refused in the research §3 and §10.
A three-level `BINGO_MOTION` (textual's `basic`): every cue here
reports a state, so there is no middle tier yet.

## Risks

- Sweeps run at the 33 ms tick for 200 ms on beats that already
  redraw; cost is bounded by `animating()`. Measure a full draw at
  120×40 before and after (§9's <4 ms) and paste it.
- Mode 2026 through tmux: if tmux does not honour it for a pane, the
  sequences are inert and nothing breaks; if it mis-parses them (old
  tmux), garbage. Floor it at tmux ≥ 3.4 (where 2026 support landed —
  verify) using M49's `Named`, else omit.
- Brick 3's phase continuity: a period change with a phase derived
  from wall-clock modulo jumps. Say which cure was taken.

## Verified

2026-09-04, on `m52-beat` off dev `c0e3fd1`. One commit per brick, in the
plan's order: `973ac1e` (0), `d09c7cf` (3), `5bf5831` (5), `80d296d` (4),
`e82f7c8` (1), `8729d52` (2 and 6 together — both are the same `Landing` on
the same row and could not be unpicked into two honest commits).

### What landed

**0 — synchronized output.** `terminal.rs`'s `draw` writes `CSI ?2026h`
before the frame's bytes and `CSI ?2026l` after, out of band like the title
and the bell, and the closing half goes out whatever the draw did (a terminal
left holding an update that never ends shows nothing at all). **Not through a
multiplexer** — `terminal::synchronizes(term, tmux)`, settled once in
`Tui::enter`; the reason is the tmux finding below. `Stored`'s picture bytes
are *not* inside the bracket: they go out through `Screen::place`, which
`run.rs` calls after `draw`, so bracketing them would mean one bracket
spanning two `Screen` calls — an API change in a file another worker holds
this week. The cells a picture is drawn into are inside the bracket; only the
terminal's copy of the pixels lands just after it.

**1 — send ignition.** `Ui::sent` (an `Instant`, beside `switched`) is set in
`input::submit` — the `⏎` that takes the line, whatever the line turns out to
be; `Ui::sending(now)` is how far the light has come; `view::ignite` re-styles
the box's outline cell by cell through `clock::sweep` and `theme::pulse`. It
*patches* what the border already drew, so it comes back to dim or to the
breath without knowing which. One line in `run.rs`'s `animating()` — the seam
the plan allowed — so a light is never frozen mid-border when the line started
no turn (`/help`, `/clear`, a post into a room). The `>` block's ground fade
did not land; see below.

**2 — tool-done sweep.** `transcript::Landing` says what a row that has just
landed is doing; `theme::landing` is the ramp and `clock::sweep` the geometry,
one span per cell of the name while the light is on it and one span again when
it has passed. `live_bullet` lost its bold frame entirely — the sweep replaces
it, as the plan says. `motion::a_completion_flashes_bold_for_exactly_one_frame`
became `a_completion_sweeps_its_name_for_six_frames_and_rests_on_the_seventh`:
re-aimed, not deleted, and it now also asserts the bullet carries no weight of
its own.

**3 — the breath tracks the work.** `view::breath_of(state)`: 0.9 s while an
assistant item is still arriving, 2.2 s while a tool call is running, 1.6 s
otherwise. No new state — both facts are `SessionState`'s.

**4 — the card breathes.** `layers::card` takes `now` and its border wears
`theme::attention`. A card is on the screen exactly while it is unanswered, so
there is no "answered → rest" branch to write: it stops asking by going.

**5 — progress.** The bounded fill ramps `presence` → glow across its lit run
(`theme::pulse` per cell — the same ramp a live bullet wears, so no new token);
the unbounded bar's three cells walk the ten-cell track at four steps a second
and wrap. The beat travels as `views::Marks::beat`, which is what `Marks` is
for: what the frame knows that a node does not.

**6 — the flare.** `theme::cooling` (`bad` → `text`) over twelve frames on the
row's name and on its `(about)`. The bullet stays `bad`, so what cools is how
fresh the failure is and never whether there was one. No shake, no jitter.

### What the plan got wrong

1. **"Under tmux the two sequences go bare — tmux understands 2026 itself…
   floor it at tmux ≥ 3.4 (where 2026 support landed)."** Both halves are
   wrong; the finding is below. The bracket is omitted under a multiplexer
   instead.
2. **The `>` block's `raised` ground fading in over three frames.** Not done,
   on purpose. A block's ground can only be dated by when the *block* arrived
   (`blocks::Motion::since`), which is when the kernel's frame lands and not
   when the key was pressed — so riding it would fade every user band of a
   transcript being replayed (and would fail three standing truecolor
   assertions that a `>` row is a band from its first frame). Riding
   `Ui::sent` instead would need the block cache to redraw for a fact that is
   not in its `Revision` — a second clock for the same block. The border's
   light is the whole of the send's answer, and the exit criteria never asked
   for the ground.
3. **"one `sweep(anim, column, width) -> Style` … used by both."** It is
   `clock::sweep(t, column, width) -> f32` — the geometry — with each row
   naming its own token (`theme::pulse` for the border, `theme::landing` for a
   name). The two sweeps have different ends, and `theme.rs`'s own rule is
   that a view names a token and never a colour, so one function returning a
   `Style` would have had to take its ends from the caller.
4. **"`theme::fading` with its ends swapped — make it one function with a
   direction, not two."** `fading` runs `dim` → the level's colour; the flare
   runs `bad` → `text`. They share no end, so a direction flag would have been
   a worse function than the file's own grain: one named ramp per pair
   (`pulse`, `comet`, `warming`, `fading`, and now `landing`, `cooling`).
5. **"A failed tool *or turn*'s row text."** Only the tool's. A failed turn's
   line is derived from `last_turn` and belongs to no item, so it has no
   arrival instant to flare from; giving it one means a `Motion` for a block
   that is not an item's, in `blocks.rs`. It is already `bad` from end to end,
   which is the half of the cue that matters.
6. **Files.** `views/progress.rs` could not be reached without `views/mod.rs`
   (`Marks::beat`), `ui.rs` (`Ui::marks` takes `now`) and `panel.rs` (the
   `ctrl+t` sheet's live cards walk on the same beat as the rail's).
   `blocks.rs` owned `FLIP`, so brick 2 had to go through it, and
   `input.rs::submit` was a better seam for the ignition than `run.rs`.
   `motion.rs` passed the discipline script's thousand-line cap on the way in,
   so the block's own landings are `motion/landing.rs` beside it — the shape
   `screens.rs` already uses.

### The tmux finding (mode 2026), from primary sources

- tmux is a terminal of its own: it **consumes** the sequence and never
  forwards it. `input.c`'s `input_csi_dispatch_sm_private` on master has
  `case 2026: screen_write_start_sync(ictx->wp); break;` — and the same file
  at tags 3.6, 3.5a and 3.2 does not contain `2026` at all.
- It landed in **tmux 3.7** (released 2026-06-26), not 3.4. `CHANGES`, under
  `CHANGES FROM 3.6b TO 3.7`: *"Add support for applications to use
  synchronized output mode (DECSET 2026) to prevent screen tearing during
  rapid updates (from Chris Lloyd in issue 4744)."* Commit `1c7e164c22a3`.
- What tmux 3.4 did (commit `1a14d6d2e1c2`, *"Use SM 2026 for Sync which is
  more widely supported now"*) is the **other** direction: its own `Sync`
  capability, which is how tmux wraps what *it* repaints in the outer
  terminal's synchronized update. That is what the plan's "≥ 3.4" was reading,
  and it is a reason for this surface to write nothing, not a reason to write.
- **3.7 and 3.7a are actively harmful**: `RM ?2026` stopped sync mode without
  asking for a redraw, so a frame written between the two halves could stay
  invisible until something unrelated repainted the pane or a one-second
  timeout fired. `CHANGES FROM 3.7a TO 3.7b`: *"Fix so that the end of a
  synchronized update again triggers a redraw."* (commit `e802909de060`).
- On every tmux ≤ 3.6 an unknown private mode falls to `default: log_debug(…)`
  — silently dropped, no garbage on the screen, as far back as 2.6.

So: writing the bracket under tmux buys nothing on ≤ 3.6, risks a frozen
frame on 3.7/3.7a, and duplicates work tmux already does on ≥ 3.7b. It is
omitted, and the pty test asserts it is omitted.

### The draw at 120×40

Same harness before and after — a temporary test in `motion.rs` (not
committed) drawing a 60-item transcript with a running tool and a streaming
answer into a `TestBackend`, 300 warm frames, `view::draw` through the real
`Ui` so the block cache behaves as it does in the loop:

| profile | before (`c0e3fd1`) | after |
|---|---|---|
| release | 152.662 µs / frame | 153.0–155.5 µs / frame |
| debug | 1.37426 ms / frame | 1.379–1.387 ms / frame |

+0.4 % in debug, and inside the run-to-run spread in release. A frame with the
border's light actually running measured 164 µs at its lowest — below the
plain scene's own lowest reading in the same conditions, so the ignition's
extra work (two rows of cells re-styled) is under the noise floor. Both are
~25× inside §9's 4 ms budget, and the repo's own
`view::tests::a_full_draw_of_a_long_transcript_is_inside_the_frame_budget`
(5 000 blocks at 120×40) still passes. Caveat: three workers shared this
machine and the load average sat above 60 for much of the session; readings
taken under load ranged to 575 µs for *both* scenes alike, which is why the
minimum of five solo runs is quoted.

### Not verified

- **The hands-on criterion is untouched** — left for the main session with the
  user, as the brief says. Nothing below is a substitute for seeing the seven.
- **Windows.** The TUI cannot be cross-checked locally (ADR-0041's note:
  `cargo check -p bingo-surface-tui --target x86_64-pc-windows-msvc` needs a
  Windows host for the terminal crates). Nothing here is platform-shaped: no
  process, no path, no signal; the one new escape sequence is written through
  the same `out_of_band` every other one already uses, and CI's `windows` job
  is the backstop.
- **Mode 2026 on a real terminal.** Verified as bytes through a pty and as a
  parse against crossterm's own spelling; that a given terminal *composites*
  the frame is its own business and is what the hands-on pass is for.
- **The breath's phase steps where the period changes.** The plan offered two
  cures; this is the second (accept and document). The phase stays the wall
  clock's own turn of the period, so at a change the breath's level can jump
  by up to the whole of its range — measured over a 22-second grid at 10 ms,
  the worst single-frame step is 1.00 of the ramp and the mean is 0.47 (all
  six transitions between 0.9 / 1.6 / 2.2 s alike). It is drawn in five steps
  between 65 % and 100 % of `presence`, so the worst case a person sees is
  four steps of brightness on the sparkle and the box's border, at most twice
  in a turn, at the instant the work itself changed — which is the one moment
  §6 allows a cue to move. An accumulated phase would need per-frame state on
  `Ui` and would cost `motion.rs` the property the whole file stands on: a cue
  is a pure function of `Now` and state.
- **A name's resting colour.** The light across a name ramps `text` → `good`,
  and a name at rest carries no foreground of its own (it is `bold` alone, as
  it always was), so the last frame of the light hands off from the palette's
  `text` to the terminal's default foreground. Under either shipped palette
  those are the same colour by design; on a terminal whose default foreground
  is far from it, the hand-off would be visible for one frame.
- **An unbounded bar with nothing else moving.** The sheen walks while the
  loop ticks, and the loop ticks while any session is busy — which is when a
  progress bar is on the screen in practice. A plugin that published one with
  no turn running would leave it parked until something else redrew, which is
  the resting frame and not a wrong one.

### Gates

```
$ cargo fmt --all -- --check
(no output; exit 0)

$ cargo check --workspace --all-targets --locked
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 00s

$ cargo clippy --workspace --all-targets --locked -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 35.79s

$ cargo test --workspace --locked            # tee'd to target/m52-test.log
EXIT: 0
   1 test result: ok. 747 passed; 0 failed; ... (bingo-surface-tui)
   1 test result: ok. 181 passed; 0 failed; ... (bingo, tests/cli)
   ... 47 binaries, 0 failed
# The first run hit the known flake
# `mentions::a_question_left_unanswered_is_chased_when_the_next_process_opens_the_room`;
# it passed alone and passed in the whole suite on the rerun quoted here.

$ scripts/check_discipline.sh
dependency direction ok
kernel names no tool
cohesion ok
warn crates/bingo-core/src/session.rs:129 fn handle is 66 lines (>60)   # not ours
discipline ok

$ scripts/budget.sh
dependencies (unique, normal): 331 (max  331)
warm cargo check -p bingo-core: 0s (max  20s)
relink isolation: touching the TUI recompiled 0 crates for core (must be 0)
target/debug: 5 GB (soft max  5)
test binaries: 3
budget ok

$ cargo deny check
advisories ok, bans ok, licenses ok, sources ok

$ scripts/tui-smoke.sh
  ... 17 drives, ending
  BINGO_ASCII=1 and NO_COLOR leave a terminal nothing it cannot draw
  a live signal moves in the rail and leaves nothing behind
  a button on a pinned board fires its command and the table changes
tui-smoke ok

$ cargo test -p bingo --locked --test pty
running 8 tests
test every_frame_is_written_inside_a_synchronized_update ... ok
... 8 passed; 0 failed
```

No snapshot changed, and none needed to: every new cue is style, and a
snapshot is text. The one standing test that had to be re-aimed is the
completion flash, which the plan asked for by name; the token ledger in
`theme.rs` gained `landing` and `cooling` and moved `presence` → `pulse` for
the progress fill and the card's border → `attention`, which is the ledger
doing its job.
