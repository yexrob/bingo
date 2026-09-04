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

- [ ] Every frame is bracketed by mode 2026 (TestBackend + PTY).
- [ ] `⏎` sweeps the border; a tool's completion sweeps its name; an
  error flares; the breath's period follows the state; the card
  breathes; progress has its gradient and sheen — each with a test on
  the injected clock and each at rest under `BINGO_MOTION=off`.
- [ ] `NO_COLOR` loses no fact (each cue's degrade named in §6).
- [ ] Every AGENTS.md gate; budget 331; tui-smoke; pty; no new crate.
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
