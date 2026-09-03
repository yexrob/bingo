# M49 — The picture through tmux

## Goal

Inside tmux, a picture is drawn the way it is drawn outside: the
probe and the transmit travel in tmux's passthrough envelope to the
outer terminal, the placeholder cells stay ordinary text tmux scrolls
and repaints, and nothing else changes. The research of 2026-09-04
(M48 Verified) says this is the one image route tmux carries: kitty's
own `icat --passthrough` implies `--unicode-placeholder`, and yazi
drops direct placement under a multiplexer. `allow-passthrough on`
(tmux ≥ 3.3; ≥ 3.4 for its combining-character rewrite) is the
person's setting; when it is off the chip stands and one notice says
which setting. The user's own box is tmux 3.6b under Ghostty with the
setting on — the first real terminal any of M46–M48 is driven on.

## Bricks

1. **The envelope, one function.** `terminal::wrapped(bytes, tmux)`
   already builds `DCS tmux; <bytes with ESC doubled> ST` for
   notifications (§6). Move it — with its test — to a module both the
   notification and the graphics can name (`terminal::passthrough`, or
   `graphics::tmux`), and give `Graphics` a `transport: Transport {
   Bare, Tmux }` the send reads: `Stored::catch_up`'s bytes are wrapped
   per APC chunk (each chunk is its own DCS; a chunk is ≤ 4096 bytes of
   base64 plus keys, under any DCS limit tmux has). Pure; byte-exact
   test of one wrapped chunk with the doubled `ESC`.
2. **The probe through tmux.** `probe::QUERY` becomes `probe::query(
   transport) -> Vec<u8>`: under tmux, the kitty query, `CSI 16 t` and
   XTVERSION are each wrapped (the outer terminal answers, and tmux
   delivers the answers to the pane that asked — the *active* pane, so
   the probe is only sound when bingo starts in the focused pane; the
   plan accepts that and says it in the notice), a second XTVERSION
   goes *unwrapped* (tmux answers it itself: `DCS > | tmux 3.6b ST`),
   and DA1 goes last, unwrapped (tmux answers DA1 and that still ends
   the read). `probe::parse` collects every XTVERSION reply into
   `Probe.terminals: Vec<Named>`; `Graphics::from(Probe, transport)`
   under tmux wants: a kitty `OK`, a cell, a `tmux` entry with version
   ≥ 3.4, and another entry that `draws_placeholders`. Tests on spelled
   answers: Ghostty-under-tmux (all four), tmux with passthrough off
   (only tmux's XTVERSION and DA1 come back → `Off` + the notice),
   tmux 3.3 under kitty (→ `Off`, too old), an outer WezTerm (→ `Off`).
   `asked()` stops short-circuiting on `multiplexed`; `screen` (no
   passthrough) still does.
3. **The notice.** One `Level::Info` notice on the first frame when
   tmux answered but the outer terminal did not:
   `tmux: pictures need `set -g allow-passthrough on` and the focused
   pane at start` — through the existing `ui.notify`, raised once from
   the run's opening (find where the theme's or the session's first
   notices are raised and sit beside them; never from a draw).
4. **The cell size under tmux.** The wrapped `CSI 16 t` answers the
   outer terminal's cell, which is the pane's cell; keep it. If tmux
   ever answers `CSI 16 t` itself first, the parser takes the first
   reply — write the test that says which it takes, and read tmux's
   `input.c` to know whether it answers (say so in Verified).
5. **Real-terminal Verified.** Beyond the gates: in the user's tmux
   under Ghostty, `target/debug/bingo`, paste a screenshot — the strip
   shows; send — the transcript block shows; scroll it half off and
   back; `tmux set -g allow-passthrough off` in a fresh session — the
   chip and the notice. Record what was seen, including anything
   wrong, in the plan's Verified as the first hands-on observation of
   M46–M49. The worker cannot do this (no tty); the main session does
   it with the user, after the merge.

## Files

`bingo-surface-tui/src/{terminal.rs,graphics/{mod.rs,probe.rs,
stored.rs},run.rs}` — `run.rs` is at 972 non-test lines and fails at
1000: whatever the notice needs there must be a call into a new small
module, not lines in `run.rs`; `docs/design/tui.md` §5 (tmux, dated);
`scripts/tui-smoke.sh` runs under a PTY, not tmux — the pty test in
`crates/bingo/tests/pty.rs` gains the tmux-shaped exchange (the
harness plays tmux *and* the outer terminal: answers the unwrapped
XTVERSION as tmux, the wrapped ones as Ghostty, DA1 as tmux, and
asserts the transmit arrives wrapped).

## Exit criteria

- [ ] Under tmux ≥ 3.4 with passthrough on and an allowed outer
  terminal, the probe says `Kitty` with `Transport::Tmux` and every
  APC chunk goes out wrapped (pty test).
- [ ] Passthrough off, tmux too old, or an outer terminal off the list
  → `Off`; the passthrough-off case raises the one notice.
- [ ] Every AGENTS.md gate; budget 331; tui-smoke; the pty test.
- [ ] Hands-on in the user's tmux (main session, after merge): strip,
  block, scroll, and the off case — observations recorded.

## Non-goals

GNU screen (no passthrough). Zellij (its own passthrough rules — not
researched; treat as a multiplexer that gets the chip until it is).
Detecting a pane that is not focused at start (the notice tells the
person). `allow-passthrough all` semantics beyond what `on` gives.

## Risks

- tmux delivers the outer terminal's answers to the active pane: a
  bingo started in a background pane reads nothing and gets the chip
  plus the notice — honest, and the notice says why.
- A wrapped DCS inside tmux's `allow-passthrough on` is dropped while
  the pane is invisible; a transmit sent for a frame nobody sees is
  lost, and the next frame's `Stored` believes the terminal has it.
  `Stored` must not mark a picture held until… it cannot know. Accept:
  a pane switched away from during a send shows a hole until the
  picture is re-sent; record the shape and leave the cure (`all`, or a
  re-send on focus) to a later slice.
