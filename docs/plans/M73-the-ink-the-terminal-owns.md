# M73 — The ink the terminal owns

## Goal

User, 2026-09-05: "look at how Claude Code and Codex do it; your switch
is not real-time." Researched from their sources (Codex `codex-rs/tui`
at `728cb12`; Claude Code 2.1.260's binary and docs), the answer is
not detection at all. **Neither colours body text.** Codex draws prose
with no foreground SGR (`Color::Reset` → SGR 39), secondary text with
real SGR 2, accents in ANSI-16 — and bans `Color::Rgb` in `clippy.toml`
("use ANSI colors, which work better in various terminal themes").
Claude Code draws prose as a bare Ink `<Text>` (default foreground) and
only its chrome in explicit colour. So when the terminal flips its
palette, every line of prose — scrollback included — is remapped by
the terminal itself, instantly, with no query. Detection is spent only
on what cannot be "default": backgrounds, tints, blends. Codex detects
once at start and never again (it removed its focus re-query after it
froze under Zellij); Claude Code enables DEC 2031 and re-asks OSC 11 on
the report, but only under `theme: auto`, which is not its default.

bingo does the opposite: `text` is an explicit truecolor
(`#ece7df` / `#24201a`) and `dim` an explicit grey, so a flip leaves
the ink wrong until M71's next ask (focus, or 30 s idle). That is the
delay the user sees. The cure is the one both products already have:
**body text on the terminal's own foreground; secondary text on the
terminal's own dim; colour only where colour is the meaning.**

## Shape

- `theme::text()` is `Color::Reset` in every mode — no palette entry.
  The `text` token leaves `Palette`; the design's warm off-white was a
  hue the terminal's own foreground is now asked to carry, and on a
  warm terminal theme it will, on a cold one it will not, and that is
  the terminal's choice as it is for Codex and Claude Code.
- `theme::dim()` is `Modifier::DIM` in every mode (the ANSI arm already
  is). The `dim` token leaves `Palette` **for text**. Where `dim` was
  spent as a *colour* rather than as secondary text — the hairline
  border, the rail, the comet tail's cold end, `theme::lit` if it
  survives M72 — each site is read and given the right thing: a
  border in `Reset` + `DIM` reads as a hairline on both grounds; a
  colour ramp's cold end becomes `raised`. The ledger test lists every
  site, so this is a walk down one list.
- `presence`/`glow` stay as they are: `#d97757` is the same brand
  orange Claude Code fixes in both themes, legible on both grounds.
  `good`/`bad`/`mode` and the tints stay palette-chosen (they *are*
  the detected part), so M71's slot and re-ask stay for them and for
  `raised`. Nothing about M71 is undone; it now governs only what
  detection can govern.
- `NO_COLOR`/`Plain` is unchanged (already `Style::new()`); ANSI is
  unchanged for text (already `Reset`); the ASCII table is untouched.

## Bricks

1. **`theme`**: `text()` → `Reset`; `dim()` → `DIM`; `Palette` loses
   `text` and `dim`; the ledger and its test updated; a test that
   `text()` and `dim()` are palette-free in all three modes.
2. **The walk.** Every ledger site of `dim` that meant a colour, fixed
   by hand, with the snapshot diff read: the box borders, the rail's
   lines, the status line's counts (DIM is right there), the diff
   hunk header, the comet ramp's tail (`comet(age)` fades
   `glow → presence → raised`, not `→ dim`), the picture band's frame.
   The commit body names each site and why.
3. **Contrast proof.** A test that draws the transcript screen in both
   palettes and asserts no span's foreground is a palette `Rgb` unless
   it is one of `presence glow good bad mode` — i.e. prose and
   secondary text carry no colour of their own anywhere. Both
   `screens::*` snapshot sets re-read (they will change wholesale in
   style, not in text — say so in the commit).
4. **Docs.** Design §4 rewritten: the palette is now the *accent*
   palette; the terminal owns the ink and the dim; why (the research
   above, two lines); the M71 slot governs accents and tints. Dated
   log line. The M71 plan gets a one-line pointer.

## Files

`bingo-surface-tui/src/theme.rs` (+ every file the ledger names, one
line each), `screens/*` snapshots, `docs/design/tui.md` §4 + log,
`docs/plans/M71-the-look-that-follows.md` (pointer).

## Exit criteria

- [ ] `theme::text()` and `theme::dim()` carry no palette colour in
      any mode (test).
- [ ] No screen span's fg is a palette Rgb outside the five accents
      (test over both palettes).
- [ ] Every former `dim`-as-colour site reads right in both palettes
      (snapshots re-read; the comet tail ends on `raised`).
- [ ] All gates; tui-smoke by the parent; hands-on by the user: flip
      the system theme with bingo open — the prose follows at once,
      the accents within one focus.

## Non-goals

DEC 2031 (closed by crossterm 0.29, M71); ANSI-16 accents (the brand
orange is fixed on purpose, as Claude Code's is); a `theme` setting.

## Risks

A terminal whose default foreground is low-contrast is the user's own
choice, as it is for every other program. `DIM` renders differently
across terminals (some lighten, some grey); Codex ships on it 600
times over, and bingo already does in ANSI mode. The snapshot churn is
large and mechanical: every `text`/`dim` span changes style; the
worker must read a sample of each screen rather than accept blindly.

## Verified

Exit criteria:

- [x] `theme::text()` and `theme::dim()` carry no palette colour in any mode.
      `text()` is `Style::new().fg(Color::Reset)` and `dim()` is
      `Style::new().add_modifier(DIM)` unconditionally — no `current()` read
      at all — and `theme::tests::the_ink_and_the_dim_are_the_terminals_own_in_every_look`
      asserts both in `Plain`, `Ansi`, `True(DARK)` and `True(LIGHT)`.
      `Palette` has no `text` and no `dim` field; the palette snapshot lost
      its two rows.
- [x] No screen span's fg is a palette Rgb outside the accents.
      `screens::colours::no_screen_paints_prose_in_a_colour_of_its_own`
      draws seven scenes (an answered question, a turn at work, a card over
      a diff, a form with a mockup, the switcher, a shell line, a channel's
      report) at 80×24 and 120×40 in **both** palettes and asserts every
      foreground is `Reset` or a member of `theme::spendable()`.
      `spendable()` is the closure of §4's own table — the five accents and
      every point the sanctioned ramps reach between them, sampled at
      1/1000 — derived from the token functions rather than from the
      palette, so a ramp added to the table is in it without anyone
      remembering, and a colour no token can produce fails.
- [x] Every former `dim`-as-colour site reads right in both palettes. Each
      was read off a `TestBackend` frame as a per-cell style map, in both
      palettes; the list and what each became is in `904cf871`'s body. **One
      deviation**: the comet's tail ends on the terminal's ink, not on
      `raised`. `theme::comet` is spent on streaming *prose* and on the
      working word (`transcript`, `view::beamed`); §4 says `raised` is never
      text, and a tail landing one step from the terminal's own background
      would leave aged prose unreadable. It runs `glow` → `presence` over
      the warm half of the tail and hands to `text()` at `SETTLES`, which is
      the design's own "cools to `text`" with the ink where the ink now
      lives.
- [x] All gates below. `scripts/tui-smoke.sh` was not run (the brief forbade
      it) and no terminal emulator was launched. **Hands-on by the user is
      not done**: flipping the system theme with bingo open is the one
      measure a worker cannot take.

Not in the plan, and done anyway:

- `fading`, `warming`, `landing` and `cooling` also passed through the grey
  or ended in the ink. Each now takes its two ends in every look, which is
  what the eight colours always drew them as, and §4 sanctions gradients in
  two places only (the progress fill and the comet) — so this is the table
  being obeyed rather than a loss. `warming` lands `bad` at exactly 90 %,
  where `· /compact` joins it, so hue and words say one thing.
- `theme::picture` keeps the opening's two neutral greys, private to
  `theme.rs` and used only by `lit`: a half-block picture carries its
  brightness in its colour and has no ink to borrow. `intro/` was not
  touched otherwise and its snapshots did not move — the piece and these
  greys go together in M72.
- `storyboard::ground` decided a preview's ground by asking whether the ink
  was pale. It cannot ask that any more; it reads the tint instead.

What the plan expected and did not happen: **the snapshots did not change
wholesale.** The suite fixes the look to the ANSI table, where `text` was
already `Reset` and `dim` already `DIM`, so not one screen's bytes moved.
The only snapshot that changed is the palette table itself. The styles were
read by hand instead, in both palettes, screen family by screen family, and
only styles changed.

```
$ cargo fmt --all -- --check
FMT OK

$ cargo check --workspace --all-targets --locked -j 2
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 06s

$ cargo clippy --workspace --all-targets --locked -j 2 -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 38.79s

$ cargo test --workspace --locked -j 2 --no-fail-fast
passed 4208 failed 0 ignored 4   (85 suites)

$ cargo test -p bingo --test pty --locked -j 2
test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ scripts/check_discipline.sh
kernel names no tool
cohesion ok
warn crates/bingo-core/src/session.rs:129 fn handle is 72 lines (>60)
discipline ok

$ scripts/budget.sh
dependencies (unique, normal): 334 (max  334)
relink isolation: touching the TUI recompiled 0 crates for core (must be 0)
warn: target/debug exceeds the soft limit
budget ok

$ cargo deny check
advisories ok, bans ok, licenses ok, sources ok
```

Not verified: Windows (`cargo check --target x86_64-pc-windows-msvc` still
fails in `aws-lc-sys`'s C build on this box, as M71 recorded — nothing here
is platform-shaped: no process, path, signal or clock is touched); a real
terminal's own rendering of SGR 2, which is the one thing `DIM` rests on and
which differs between terminals by design.
