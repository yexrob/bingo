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
