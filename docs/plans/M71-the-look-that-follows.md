# M71 — The look that follows

## Goal

User, 2026-09-04, with a screenshot: a dark terminal, and bingo drawing
its **light** palette on it — body text a near-black ink on a near-black
ground, only the bold rows readable. The look is chosen once, before the
first frame (`theme::detect`, OSC 11 through `terminal_colorsaurus`, a
400 ms probe), and never again. A terminal that follows the system's
appearance changes its ground under a running bingo, and bingo keeps the
ink it chose an hour ago. Wanted: the look follows the terminal for as
long as the run lasts — a theme flip is a redraw, not a restart.

## Shape

The look stays one fact read at render time (`theme::current()`), but
the fact may change: the `OnceLock` becomes a slot that a later answer
may replace, and every draw after that reads the new one. Nothing that
draws changes; nothing caches a `Style`. `BINGO_THEME=light|dark` still
pins the look and turns the following off — a person who named a look
is not asked again.

Two ways to hear a change, both cheap, both optional per terminal:

1. **The terminal says so.** Mode 2031 (`CSI ? 2031 h`): a terminal that
   knows it reports `CSI ? 997 ; 1 n` (dark) / `CSI ? 997 ; 2 n` (light)
   whenever its scheme changes (kitty, foot, Ghostty, WezTerm, iTerm2
   3.5+, Contour; **measure which**, and what crossterm 0.29 makes of the
   report — its parser may drop an unknown `CSI … n`, as it drops
   `CSI 6;h;w t` (M60). If it drops it, this way is closed and the plan
   says so; if it reaches the key stream as chords, `late::Late` hears it
   as it hears an OSC reply). Set on enter, reset on leave, beside the
   other modes `terminal::Tui` sets.
2. **bingo asks again.** OSC 11 written to the terminal — the same
   question `detect` asked — at moments a change is likely and the cost
   is nothing: on `Event::FocusGained` (enable focus reporting on enter;
   a person who flipped the system theme did it in another window), and
   on a slow clock while the run is idle (`RE_ASK`, 30 s; not while a
   turn runs, when the screen is busy anyway). The answer arrives in the
   key stream and `late::Late` already spells an OSC 11 reply back into
   bytes (`ESC ] 11 ; rgb:rrrr/gggg/bbbb ESC \` or `BEL`); `run::answered_late`
   parses the colour, decides light or dark by the luminance the
   colorsaurus crate uses (so the two doors agree), and swaps the look.
   Under tmux the question needs no passthrough (tmux answers OSC 11 for
   its pane) — measure it.

A swap that changes nothing is nothing: same look, no redraw. A swap that
changes the look marks the whole screen dirty (the reconciler keeps the
pictures; the dim pass reads the new tokens), and the status line says
nothing — the screen itself is the message.

## Bricks

1. **`theme::swap(look: bool)`** (light or dark, truecolor only; under
   `NO_COLOR` or ANSI there is nothing to follow): the slot, the read,
   and a test that a draw after a swap wears the other palette
   (`theme::with` already fakes a look for tests; the slot must not
   break it). Pure: `theme::background_is_light(rgb) -> bool` on the
   parsed OSC 11 colour, fixture-tested on the colorsaurus threshold.
2. **The ear.** `late::Late` hears `CSI ? 997 ; n n` if crossterm lets
   it through (measure first; write the table row in `late.rs`'s doc
   comment either way). `run::answered_late` gains the OSC 11 arm and
   the 997 arm; each ends in `theme::swap` and a full redraw.
3. **The asks.** `EnableFocusChange` on enter; the `RE_ASK` clock in the
   run loop's `tick` (only while idle, only when the look is
   `Look::Terminal` and truecolor); mode 2031 set/reset in `terminal.rs`
   beside the kitty keyboard flags.
4. **Proof.** A `TestBackend` test: fold a dark screen, swap, draw, the
   body text wears `LIGHT.text`; a pty scene under `scripts/tui-smoke.sh`
   is optional (the smoke terminal's ground does not change) — the
   answer path is unit-tested by feeding `late` the bytes.

## Files

`bingo-surface-tui/src/{theme.rs,late.rs,run.rs,terminal.rs}` (+ a
`run/look.rs` if `run.rs` grows past its cap — it is near it),
`docs/design/tui.md` §4 (the look follows; the two doors; what is
measured) + dated line, `docs/design/tui.md` ledger if a token moves.

## Exit criteria

- [ ] A run started on a dark terminal that turns light redraws in the
      light palette within one focus or one `RE_ASK`, and back.
- [ ] `BINGO_THEME` pins the look and nothing is asked.
- [ ] The measured table: which of kitty / Ghostty / tmux answer 2031,
      and whether crossterm passes `CSI ? 997` (in `late.rs` and the
      design doc).
- [ ] All gates; `TestBackend` test for the swap; Windows cross-check
      for `bingo-surface-tui` if it builds on the box (`aws-lc-sys` may
      block it; say so).
- [ ] Hands-on: appended by the parent.

## Non-goals

A theme setting in `settings.json` (the env var stays the override);
a palette beyond the two; following the *system* appearance directly
(the terminal is the one that knows; bingo asks it); ANSI palettes.

## Risks

An OSC 11 asked while a person types: the reply lands between their
keystrokes and `late` must give the keys back untouched when the reply
is not one — it already does for the picture probe, and `RE_ASK` is
idle-only to keep the case rare. A terminal that answers OSC 11 with
the *default* colour rather than the current one (some do under tmux):
the measured table says which, and the focus-time ask is the fallback.
