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

- [x] Under tmux ≥ 3.4 with passthrough on and an allowed outer
  terminal, the probe says `Kitty` with `Transport::Tmux` and every
  APC chunk goes out wrapped (pty test).
- [x] Passthrough off, tmux too old, or an outer terminal off the list
  → `Off`; the passthrough-off case raises the one notice.
- [x] Every AGENTS.md gate; budget 331; tui-smoke; the pty test.
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

## Verified

### What landed

Bricks 1–4. Brick 5 is the main session's, with the user, after the merge:
its lines are left empty below.

1. **The envelope, one function** — `graphics/tmux.rs`. It owns the whole of
   "the terminal in front of the terminal": `Transport { Bare, Tmux }`,
   `wrapped(sequence, transport)` (the `DCS tmux; … ST` with every inner
   `ESC` doubled), `transport(term, tmux)` (which multiplexer, or `None` for
   one this cannot reach through), `named` and `carries_pictures` (the
   ≥ 3.4 floor). `terminal.rs`'s own `wrapped` is gone; `notification` calls
   the shared one and keeps today's behaviour exactly, screen included
   (`terminal::envelope` says why in three lines). `kitty::apc` wraps each
   sequence it builds, so a two-chunk picture is two envelopes and a delete
   is one — asserted byte for byte in `kitty.rs` and again through `Stored`.
2. **The probe through tmux** — `probe::query(transport)`. `Probe.terminal`
   became `Probe.terminals: Vec<Named>`, every XTVERSION reply in arrival
   order, and `Graphics::from(probe, transport)` wants a kitty `OK`, a cell,
   an entry that `draws_placeholders` and — under tmux — a `tmux` entry at or
   above 3.4. `asked()` no longer short-circuits on `multiplexed`; it
   short-circuits on `tmux::transport` answering `None`, which is screen and
   anything else that is not tmux.
3. **The notice** — `graphics::PASSTHROUGH`, decided by `graphics::unheard`
   and raised by a new `opening.rs` from `run::attach`, beside
   `fetch_catalogs`. `run.rs` gained two lines, not a notice.
4. **The cell under tmux** — the wrapped `CSI 16 t` is the outer terminal's,
   and it is the only one sent, for the reason under "what the plan got
   wrong". `the_first_cell_reply_is_the_one_taken` pins the parser's rule
   either way.

Tests: 4 new in `graphics/tmux.rs`, 5 in `probe.rs`, 6 in `graphics/mod.rs`,
1 in `kitty.rs`, 1 in `stored.rs`, 2 in `opening.rs`, 2 pty scenes, and one
moved out of `placeholders.rs`. Workspace total 3527 → 3547.

### tmux's own source, read 2026-09-04

`input.c` at `master`, fetched rather than recalled:

```
case 16:
        if (w == NULL)
                break;
        input_reply(ictx, 1, "\033[6;%u;%ut", w->ypixel, w->xpixel);
        break;
```

**tmux does answer `CSI 16 t`** — and `CSI 14 t`, `15`, `18`, `19`, DA1
(`input_csi_dispatch`) and XTVERSION (`input_reply(ictx, 1, "\033P>|tmux
%s\033\\", …)`). So brick 4's "if tmux ever answers it" is not a
hypothetical: the only reason the cell that comes back is the outer
terminal's is that no unwrapped `CSI 16 t` is ever sent.

### What the plan got wrong

- **An unwrapped DA1 cannot end the read under tmux.** The plan had DA1 go
  last and bare, "and that still ends the read". It ends it far too early.
  `input_reply` writes straight back to the pane, while a passthrough goes
  through `screen_write_rawstring` → `tty_write` and only reaches the outer
  terminal on the next flush — and tmux's request queue (`input_requests`,
  `INPUT_REQUEST_QUEUE`) holds a reply back only for OSC 4 and OSC 52, never
  for a passthrough. So tmux's DA1 would land before the outer terminal had
  said one byte, and `parse` cuts everything after the DA1 reply: the answer
  would be empty every time. **DA1 travels inside the envelope instead**, so
  the DA1 that ends the read is the outer terminal's, and it is the last of
  the four it answers. The one bare question left is XTVERSION, placed
  *first* for the same ordering reason: tmux answers it before it has
  forwarded anything, so its name is always in front of the outer
  terminal's and can never be cut off by the DA1 behind it.
- **The cost of that is a slow start-up when the passthrough is off.**
  Nothing comes out of the envelope, so nothing ends the read and the probe
  spends the whole `theme::PROBE` (400 ms) before it gives up. That is once,
  at start-up, on a run under tmux with `allow-passthrough off` — and it is
  the run that then gets the notice telling it which setting to change. The
  alternative (a bare DA1 to end it quickly) is the bug above.
- **`Graphics::from` is an inherent function of two arguments**, and the
  `From<Probe>` impl is gone: the transport is not derivable from the answer
  and a one-argument `from` would have had to guess it.
- **The notice cannot be derived from `Graphics`.** "tmux answered and the
  outer terminal did not" is a fact about the *answer*, not about the
  decision — `Graphics::Off` also means WezTerm, and telling a WezTerm user
  to turn on `allow-passthrough` would be wrong. So the `OnceLock` holds
  `Settled { graphics, notice }`, both minted from the one `Probe`. One
  global, not two, and neither half can go stale against the other.
- **Version parsing moved.** `placeholders::version` was private to the allow
  list, and tmux needs the same reading for its own floor. Rather than tmux
  depending on placeholders for something that is not about placeholders, it
  is `Named::number()` in `probe.rs` — the reply's own shape, read by
  whoever holds a floor. `3.6b` reads as `[3, 6, 0]`, which is what a running
  tmux gives.
- **The envelope is one per APC, not one per `catch_up`.** The plan said as
  much; worth recording that it is `kitty::apc` that wraps, so nothing above
  it has to remember to.

### What is not verified

- **No real tmux, and no real terminal behind one.** Every terminal and every
  multiplexer in these tests is one this repository wrote. tmux's *replies*
  are spelled from `input.c`'s own format strings (above) and its ordering is
  argued from that source, not measured: the pty harness writes tmux's name
  first because that is what the code says will happen, not because a tmux
  was watched doing it. **This is brick 5's whole job.**
- **The notice is not driven through a pty.** A notice lives 4 seconds
  (`ui::NOTICE`) and the pty polls against a 30-second limit; asserting it on
  a screen would pin the machine the test was written on (AGENTS.md). It is
  asserted at both ends instead — `unheard` as a pure function over five
  answers, and `opening::notices` putting it on a `Ui` — and the pty scene
  next to it proves the same run draws the chip and transmits nothing.
- **A pane switched away from mid-send** still loses the picture, as the
  plan's risk 2 says. Nothing here cures it and nothing here detects it: the
  cure (`allow-passthrough all`, or a re-send on focus) is a later slice.
- **A pane that is not focused at start** reads nothing and gets the chip
  plus the notice. That is the plan's risk 1, accepted, and the notice's
  words are the whole of the mitigation.
- **The 400 ms above is arithmetic, not a measurement** — the deadline is
  `theme::PROBE` and the loop's exit condition is `probe::answered`, both of
  which have their own tests; how long a real tmux takes to turn a
  passthrough round was not timed.
- **The Windows cross-check for the TUI cannot run here**, for the reason
  ADR-0041's note records: `reqwest` → `rustls` → `aws-lc-sys`, whose build
  script compiles C against `windows.h`, and there is no Windows SDK on this
  machine. The output is pasted below. The one platform-gated thing this
  milestone touches is `probe::query`/`exchange`, whose non-unix arm is
  written in the same change and whose `test` arm is asserted by tests that
  run everywhere; `Transport`, `wrapped` and the floors are platform-free.
  CI's `windows` job is the backstop.
- **`run.rs` is 972 → 976 non-test lines.** Twenty-four from the failure at
  1000. The next change there must split it, as M47 and M48 both said; this
  one put its new lines in `opening.rs` and still could not shrink it.

### Hands-on, in the user's tmux under Ghostty (brick 5, main session)

*To be filled by the main session after the merge: the strip on a paste, the
transcript block on a send, the block scrolled half off and back, and
`allow-passthrough off` in a fresh session showing the chip and the notice —
including anything wrong.*

### Gates, all from the worktree, `-j 2`

```
$ cargo fmt --all -- --check                                    # clean
$ cargo check --workspace --all-targets --locked                # Finished
$ cargo clippy --workspace --all-targets --locked -- -D warnings   # Finished
$ cargo test --workspace --locked                # 3547 passed, 0 failed
$ scripts/check_discipline.sh                                   # discipline ok
$ scripts/budget.sh    # dependencies (unique, normal): 331 (max 331); budget ok
$ cargo deny check                 # advisories ok, bans ok, licenses ok, sources ok
$ scripts/tui-smoke.sh                                          # tui-smoke ok
$ cargo test -p bingo --locked --test pty              # 7 passed, 0 failed
$ cargo check -p bingo-surface-tui --all-targets --locked \
      --target x86_64-pc-windows-msvc
                    # FAILS in aws-lc-sys' build script (ADR-0041's note)
$ cargo check -p bingo-sdk --all-targets --locked \
      --target x86_64-pc-windows-msvc                           # Finished
```
No known flake was hit. No crate joined the tree: the budget is 331 before and
after.

### Hands-on (main session with the user, 2026-09-04)

In the user's tmux 3.6b under Ghostty, and again in Ghostty without
tmux, `target/debug/bingo` at dev `75cb4d3`: a pasted screenshot shows
in the strip; asked to show a `.png`, the model reads it and the
transcript draws it; a `WebFetch` of an image URL draws too. The user
reported both routes working in both places ("在tmux和直接在终端我测试
可以Read展示图片了 webfetch也可以"). One earlier run showed a Read
picture as bare placeholder cells (tofu) — not reproduced after the
rebuild; cause not established. Scroll and the passthrough-off notice
were not driven. Same session, at the user's word after seeing it: the
strip moved out of the box onto its top border and `[image N]` became
one thing to the editor (`75cb4d3`).

- [x] Hands-on in the user's tmux: strip, block — seen; scroll and the
  off case not driven.

**M60 follows this one** (`docs/plans/M60-the-late-answer.md`): the 400 ms window
above is too short for tmux's own round trip, and the answers that arrived after
it landed in crossterm's key stream as typed characters. M60 asks tmux whether the
passthrough is on, waits three times as long when it is, and eats whatever is
still late off the key stream.
