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

- [x] A run started on a dark terminal that turns light redraws in the
      light palette within one focus or one `RE_ASK`, and back.
- [x] `BINGO_THEME` pins the look and nothing is asked.
- [x] The measured table: which of kitty / Ghostty / tmux answer 2031,
      and whether crossterm passes `CSI ? 997` (in `late.rs` and the
      design doc). **Partly**: crossterm is measured, the terminals are
      not — see below.
- [x] All gates; `TestBackend` test for the swap; ~~Windows cross-check
      for `bingo-surface-tui`~~ — `aws-lc-sys` blocks it on this box.
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

## Verified

### What was measured, and what is only documented

The hard prohibition on this milestone was that no terminal emulator may be
launched. Everything below was measured inside a pty the harness owns
(`crates/bingo/tests/pty/`), read out of crossterm 0.29's own source, or is
marked **not measured**.

| question | answer | how |
|---|---|---|
| does crossterm 0.29 pass `CSI ? 997 ; 1 n` on? | **no — and it does not drop it either: it holds the report and every key struck after it** | measured in a pty: `look::a_theme_report_holds_crossterms_parser_and_swallows_what_follows`. `\x1b[?997;1n`, then `held`, then `c`, then `typed`: only `typed` reaches the composer. Read in `crossterm-0.29.0/src/event/sys/unix/parse.rs` (`parse_csi`, the `b'?'` arm → `Ok(None)` for every final byte but `u` and `c`) and `event/source/unix/tty.rs` (`Parser::advance` keeps the buffer on `Ok(None)`); the `c` that frees it parses as a DA1 reply, which crossterm keeps to itself, so the whole buffer goes with it |
| does a terminal that answers `OSC 11` again get followed, ink and all? | **yes** | measured in a pty: `look::a_terminal_whose_ground_turns_light_is_followed_within_one_focus`. The fake terminal answers `rgb:1e1e/1e1e/2e2e`, the answer row is drawn in pale ink; the test turns its ground light, sends `CSI I`, and the same row is near-black — then dark again after the second flip. The ink is read off `vt100`'s own cells, and the assertion is which side of the middle it falls on, so no palette value is spelled twice |
| does `BINGO_THEME` stop every question? | **yes** | measured in a pty: `look::a_named_look_is_never_asked_what_ground_the_terminal_has`. With `BINGO_THEME=dark` and `COLORTERM=truecolor`, `\x1b]11;?` never appears in the child's output at all — not the probe's, not a focus's |
| do kitty / Ghostty / foot / WezTerm / iTerm2 ≥ 3.5 / Contour report mode 2031? | documented to | **not measured.** Measuring it means driving a real terminal emulator, which this milestone did not do. It does not matter to the code: the row above shuts that door whatever they report |
| does tmux answer `OSC 11` for its own pane, without passthrough? | documented to, from the ground it believes the outer terminal has | **not measured**, and not attempted: a tmux the harness could start here would have no client attached, and an unattached tmux answers this question from its own options rather than by relaying it to a terminal — a number for the case nobody runs, which is worse than none. The risk the plan named — a tmux that answers with a *default* colour rather than the current one — is therefore still open, and if that is where the reported screenshot came from, this milestone does not cure it |

### What landed

1. **The slot** (`theme.rs`). `Ask` lost its `light` field: what the
   environment says is settled once in a `OnceLock`, and the ground the terminal
   said is a `static AtomicU8` (`UNASKED` / `LIGHT_GROUND` / `DARK_GROUND`, with
   `ground`/`number` as the pair between them). `current()` — read on every span
   — is now one relaxed load and `choose` on top of the `Theme` copy it already
   made; nothing else about a draw changed. `choose(ask, light)` takes the two
   apart, so the changing fact has exactly one home. `swap(light) -> bool`
   answers whether the look *changed*, which makes a named `BINGO_THEME`, eight
   colours and `NO_COLOR` all no-ops without a second rule to remember, and
   `follows()` is the one gate on asking — the probe used to spell that
   condition itself, and `detect()` now shares it.
   Pure bricks beside it: `answered(reply)` reads an `OSC 11` reply as
   `xparsecolor` reads a colour string (`rgb:` with one to four hex digits a
   channel, the older `#` form, either terminator), and `background_is_light`
   is `terminal_colorsaurus::Color::perceived_lightness() > 0.5` — the crate's
   own maths, so the first answer and every later one agree about one colour.
   The threshold differs from `theme_mode` in one way, deliberately and in the
   doc comment: the probe has the ink beside the ground and compares the two,
   while a reply to this one question carries only the ground, so the pivot is
   the middle grey the crate itself falls back to.
2. **The two memos of a drawing** (`blocks.rs`, `highlight.rs`) — *the part the
   plan under-counted, and the part the bug was really in.* A `Line` carries its
   styles, so the transcript's block cache and the highlighter's warm blocks
   would have kept the old palette on every row nobody happened to redraw. The
   block's look now sits in its `Revision` (not beside the width) so the entry
   and the landing it is in the middle of survive and only the drawing is made
   again; a warm code block resumes only in the look it was drawn in.
3. **The ear** (`late.rs`) — no code, one row of its table and the reason.
4. **The asks** (`run/look.rs`, ~120 lines, free functions over `&mut Run`).
   One field on `Run` (`look: look::Owed`, taking it to the sixteen the
   discipline script allows): a question owed, and when the last one went out.
   `ask` on `Term::FocusGained`; `wait`/`asking` are a sixth `select!` arm that
   sleeps to `asked + RE_ASK` (30 s) and is `pending` while a turn runs, while a
   question is already owed, or while nothing follows the terminal — so it never
   spins and an idle run wakes twice a minute at most. `pay` writes the question
   between frames where the title and the clipboard go (`Screen::ask`, the one
   new trait method). `answered` swaps.
5. **No redraw was needed** — the plan asked for the whole screen to be marked
   dirty; ratatui's diff includes the style, so every cell whose token is worth
   another colour is rewritten on the next frame and every palette token
   *does* differ between the two. What a full clear would have added is
   repainting cells that did not change, at the cost of `Terminal::clear()`,
   which queries the cursor position — a blocking read of the tty that races the
   event stream. Subtracted instead of written.

### Gates

```
$ cargo fmt --all -- --check                       (no output)
$ cargo check --workspace --all-targets --locked -j 2
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.15s
$ cargo clippy --workspace --all-targets --locked -j 2 -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.25s
$ cargo test --workspace --locked -j 2 --no-fail-fast
    85 binaries, 4183 passed, 0 failed, 3 ignored
$ cargo test -p bingo --test pty --locked -j 2
    test result: ok. 16 passed; 0 failed; 0 ignored          (run three times)
$ cargo test -p bingo-surface-tui --locked -j 2
    test result: ok. 1030 passed; 0 failed; 3 ignored         (run three times)
$ scripts/check_discipline.sh
    dependency direction ok / kernel names no tool / cohesion ok / discipline ok
$ scripts/budget.sh
    dependencies (unique, normal): 334 (max 334) ... budget ok
    (target/debug 10 GB over its 5 GB soft limit, as it was before this branch)
$ cargo deny check
    advisories ok, bans ok, licenses ok, sources ok
```

No new dependency: the reply parser is this crate's, and the lightness is
`terminal-colorsaurus`, already a direct dependency for the probe.

Three files crossed a discipline line and were split rather than excused:
`fn drive` (a sixth arm took it past 60 lines — the keys and replies arms became
`Run::keyed` and `Run::replied`, which answer with the wake they are);
`screens.rs` (the swap's `TestBackend` proof moved to `screens/colours.rs`,
where colour is pinned); and `crates/bingo/tests/pty.rs`, which became
`tests/pty/main.rs` + `tests/pty/look.rs` the way `tests/cli/` already is —
`cargo test --test pty` still names it, and the four live references to the old
path were updated (older plans still name `tests/pty.rs`; they are records).

### Not done, and not verified

- **Windows.** `cargo check -p bingo-surface-tui --all-targets --target
  x86_64-pc-windows-msvc` fails in `aws-lc-sys` 0.44's C build (`cc-rs`: no
  Windows C toolchain on this box), exactly as the plan warned, so the crate
  itself was never reached. Nothing here is platform-shaped — an `AtomicU8`, a
  `OnceLock`, `tokio::time::sleep_until`, bytes to stdout, and
  `terminal_colorsaurus::Color`, which is compiled on every platform — and no
  `cfg` was added. What is *unknown* on Windows is whether a terminal's `OSC 11`
  reply reaches crossterm's console-API event source as the same chords M60's ear
  reads on unix; if it does not, the look simply stops following there, as it
  stops following on a terminal that never answers. CI's `windows` job is the
  backstop.
- **The 30-second clock end to end.** The gate (`asking`) and the wake
  (`wait`) are unit-tested, and the *ask → answer → swap* path is measured in a
  pty through a focus event. The clock itself firing after 30 s is not driven:
  the run loop's tests cannot hold a thread-local look across an `await`, and a
  pty scene would have to sit for half a minute.
- **`scripts/tui-smoke.sh` was not run** (the brief forbade it).
- **No real terminal was driven**, so the two "not measured" rows above stay
  that way, and with them the plan's tmux risk.
