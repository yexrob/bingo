# The terminal surface — design language

> The record M11 and every later TUI change starts from. Decisions are dated at the end; a change to this file is a change of taste, made on purpose. The 2026-08-30 comparison (tape / bench / editorial) led to the tape; the same evening the user chose the other road — a full-screen application that owns every cell — and asked for life, not restraint. This is that.

## 1. Anchor

bingo in a terminal is a counterpart you work beside for hours, and it should feel **alive**: it breathes while it thinks, its work arrives with a pulse, what it wants from you comes forward and the rest recedes. Alive is not busy — every motion says something true about state (thinking, arriving, waiting, done), nothing moves for decoration, and stillness is the default when nothing is happening. Three rules hold the energy: **hierarchy first** (the eye is led, never searched), **content over furniture** (chrome is thin and dim; colour and motion are spent on what matters), and **full control** (the surface owns the whole screen, so nothing is left to the terminal's mercy — scrolling, selection, layout and every transition are ours to make good).

Named references, one trait each: `btop` — live meters that are smooth, dense and never flicker; Charm's `gum`/`glow` — motion with wit and restraint (spinners, progress, reveals); `lazygit` — keyboard flow where every pane answers at once. Density is middle-high: 80 columns must be good, 120 earns a rail.

## 2. Three glances

| glance | sees | how |
|---|---|---|
| first | what it **wants from you** (a card, `needs you`); else the **latest answer** | the one warm colour is reserved for wanting; a card dims everything behind it; the answer is the brightest text, flush left, its own block |
| second | what it is **doing / did**: thinking, tool rows, output tails, live signals | indented two, dim; state is one gutter glyph that moves while it is live `○ ⠹ ✓ ✗ ⊘` |
| third | furniture: sessions, mode, model, context, keys | a one-row header and a one-row footer, dim; the composer is one `❯` |

## 3. Layout: the frame

The surface owns the alternate screen for the whole run. Regions, top to bottom, with a rail past 120 columns:

```
  ✻ bingo   project ● · reviewer ⠹ · #design         gpt-5.4  ▁▂▃▅ 42%   ← header: presence, sessions, model, context meter
  ┌ transcript ─────────────────────────────────────┐┌ rail ───────────┐
  │ ❯ what is in this workspace?                     ││ tasks           │
  │   ✻ thought for 2s                               ││ 1 ○ write plan  │
  │   ✓ Read Cargo.toml                              ││                 │
  │     ⎿  [package] · name = "demo" · 0.1.0         ││ build           │
  │ The workspace is one package, demo 0.1.0. …      ││ ████████░░ 80%  │
  │ ❯ write me a note                                ││                 │
  │   ⠹ Write note.txt · 4s                          ││                 │
  └──────────────────────────────────────────────────┘└─────────────────┘
  ⠹ Write note.txt · 4s · esc to interrupt                                ← activity row
  ❯ ▌                                                                     ← composer
  default · ? help · / commands · ! shell                    1 needs you  ← footer
```

- **Transcript**: a virtualised list of blocks (one per item), scrolled by us — smooth, by line, with the mouse wheel and `pgup/pgdn`; `ctrl+f` searches it; `v` or a mouse drag selects and `y`/`ctrl+c` copies through OSC 52; a long block folds to its tail and opens in place on `⏎` when focused. The frame around it is a hairline in the raised tint, not a bright box.
- **Rail** (≥ 120 columns, `ctrl+t` toggles it below that): pinned panels and live signals (ADR-0013), each a small card; without them the rail is not drawn.
- **Cards** are the dialog form: a permission, a question, a sign-in, the switcher, the rewind picker. A card opens over the transcript's lower third, everything behind it dims, and it reveals top-down over three frames; `esc` closes it the same way in reverse. Focus moves into a card and back out — never anywhere else.
- **Sheets** take the whole frame for a moment — help, the pager for a long output or diff, an image, the full panel list — and slide up from the composer over four frames; `esc` slides them down.
- **Toasts** are notices: they enter from the right edge of the header row over four frames, hold 4 s, fade to dim, and go.
- **Nothing jumps.** The composer never moves; the transcript grows upward from a fixed baseline; a card's reveal never shifts what is behind it. Resize re-lays the frame in one draw.
- **Leaving** prints the last screenful of the transcript, plain, into the shell's normal screen so the conversation is still on the terminal after `exit` — the one thing the alternate screen would otherwise take.

## 4. Tokens

Truecolor is the native look, chosen after the terminal's background is read (OSC 10/11) and derived for light and dark alike; the eight ANSI colours are the fallback and every rule below holds in both. The body background is never painted — the terminal's stays — but cards, sheets and the rail sit on a **raised tint** one step from it, which is what gives the frame depth.

| token | ANSI | truecolor (dark) | where, and only where |
|---|---|---|---|
| `text` | default | `#dfe2e8` | answers, what you type, option labels |
| `dim` | DIM | `#6f7684` | work rows, receipts, rules, footer, hints, the caret, everything behind a card |
| `raised` | none | `#1a1e26` (bg + one step) | the surface of a card, a sheet, the rail; never text |
| `structure` | cyan | `#5ee0c0` | the `❯` and `!` prompts, spinners, the selected row's number, links, the presence mark |
| `attention` | yellow | `#ffb454` | card titles, `needs you`, `⊘`, the bypass badge, the context meter near its trigger — the one warm colour, only for wanting |
| `good` / `bad` | green / red | `#8fd694` / `#ff7b72` | `✓` `✗` and a failed turn's line; prose is never coloured |
| `bold` | BOLD | — | card titles, markdown headings; never emphasis in prose |

Gradients exist in two places only: a progress bar's fill (`structure` → brighter `structure`) and the comet tail of streaming text (§6). Colour never carries a fact alone: every state has a glyph, so `NO_COLOR` and a monochrome terminal lose nothing.

### Glyphs and words

| element | form |
|---|---|
| presence | `✻` in the header, `structure`; breathes while a turn runs (§6); a child's turn shows as `⠹` on its tab |
| your line | `❯ ` flush left; in a room `❯ reviewer: …` |
| work row | indent 2: `  ✓ Read Cargo.toml`; states `○` pending `⠹` running `✓` done `✗` failed `⊘` stopped; `●` retired |
| tool output | indent 4 + `⎿  `, three tail rows while running, `… +N lines · ⏎ to open` after; a short result joins one row with ` · ` |
| receipt | joins the row: `✓ Write note.txt · allowed`, `· denied — <feedback>` |
| answer | flush left, `text`, a blank line above and below, measure `min(width, 100)` |
| thinking | `✻ thinking · 3s` while it lasts, `✻ thought for 3s` after; dim; `⏎` opens it in a sheet |
| paths | relative inside the cwd, `~` for home, middle-elided `…` beyond 48 cells |
| card | raised tint, one blank cell of padding, title `attention` + `bold`, options `❯ 1  Yes`, one dim hint line; the world behind it dim |
| composer | no box; `❯ ` + text + `▌` caret that blinks at 1 Hz; continuation lines indent 2; in a room the prompt is `#design ❯` |
| header | `✻ bingo` · session tabs (current bold, a running one with `⠹`, a waiting one pulsing `attention`) · model · context meter `▁▂▃▅ 42%` |
| footer | `mode · ? help · / commands · ! shell` left, `N needs you` right; nothing repeated in the composer |
| rules | hairlines in `raised` for the transcript frame and cards; `─` in `dim` for compaction; nothing else draws a line |

Spacing has one base: 2 cells. Indents are 0 / 2 / 4 (+ the 3-cell `⎿  `); a blank line separates turns and frames an answer; cards and sheets pad by one cell.

## 5. Content kinds

Each kind has a degrade so `--print` and an IM channel never lose information:

| kind | drawn as | degrade |
|---|---|---|
| markdown | headings bold, lists `•`, quotes `│`, tables ruled, links underlined with the url dim | the text |
| code | fenced, syntax-highlighted in the palette's classes, line numbers past 8 lines, opens in a sheet | the text |
| diff | unified, coloured by column, word-level emphasis, `ctrl+e`/`⏎` to open | the unified text |
| table / key-value | hairline rules, right-aligned numbers, `–` for a missing cell | rows joined by ` · ` |
| progress | gradient fill `████████░░ 80 % · label`; a moving sheen when unbounded | `label 80 %` |
| badge | `[ text ]` in the tone's colour | `[text]` |
| tree | `├─ └─` with glyphs and badges, folds on `←` | indented lines |
| image | kitty / iTerm2 / sixel, else half-block cells, else `[image: name]`; full-size in a sheet | `[image: name]` |
| meter | `▁▂▃▅▇` sparkline; the context meter in the header | `42 %` |
| view (plugin) | ADR-0013's vocabulary, any nesting, in a block, a rail card or a signal | `View::text()` |

## 6. Motion

A terminal moves in three ways — a glyph changes, a row appears or leaves, brightness shifts — and at 30 frames a second that is enough for motion with intent. Principles: **every motion reports a state change; stillness is the default; nothing moves for decoration; a person may switch all of it off.**

| moment | cue | rhythm |
|---|---|---|
| presence | the header `✻` breathes between 70 % and 100 % brightness while a turn runs; still when idle | 1.6 s cycle, ease in-out |
| thinking | `✻ thinking · 3s` ticking, then decays to `✻ thought for 3s` as the answer starts | 1 s clock |
| streaming | text grows in place with a **comet tail**: the last eight cells fade from `structure` to `text`; the caret rides the edge | per frame, tail 150 ms |
| tool running | gutter spinner; three dim tail rows that scroll up as they arrive | 80 ms per frame |
| tool done | the glyph flips `⠹ → ✓` with one bold frame, then settles; the tail folds into the output row | 33 ms flash |
| block arriving | a new block rises two rows into place | 3 frames, ease-out |
| activity row | appears only after 300 ms of a turn: `⠹ Write note.txt · 4s · esc to interrupt` | 300 ms delay, 1 s clock |
| card opening | the world dims; the card reveals top-down; rows are dim through the kernel's 400 ms guard and brighten as it lifts, with the bell | 3 frames + guard |
| card closing | the reverse; the world brightens | 3 frames |
| sheet | slides up from the composer; `esc` slides it down | 4 frames |
| toast | enters from the right of the header; holds 4 s; fades | 4 frames in, 2 out |
| needs you | the footer badge and the child's tab pulse `attention` | 1 Hz |
| scroll | by line with ease-out; the wheel, `pgup/pgdn`, `ctrl+f` hits | 100 ms |
| session switch | the transcript crossfades through dim; the header tab's bold moves | 2 frames |
| context meter | the sparkline grows per turn; its colour warms from `dim` to `attention` across the last 20 % before the trigger | per turn |
| idle | no redraw at all | 0 Hz |
| reduced motion | `BINGO_MOTION=off`: spinners freeze to `•`, no breath, no tail, no reveals, no pulse; `NO_COLOR` strips colour | — |

Budget: an animation tick of 33 ms while anything animates, none otherwise; a full draw under 4 ms at 120×40; a keystroke echoes on the next frame. Smoothness is the absence of flicker, tearing, jumps and dropped keys — the motion above sits on top of that, never instead of it.

## 7. Ergonomics

- **Hands on the keys, the mouse welcome.** Every card answers to one key (`1-9`, `y/a/n`, `⏎`, `esc`); every sheet closes with `esc`; `?` shows the whole table. The mouse scrolls, clicks a tab or a card row, drags a selection; it is never required.
- **One focus, always visible.** `❯` marks what the keyboard talks to: the composer, a card row, a sheet, a focused block. Focus moves only by opening and closing, never by ambient events; a child that asks pulses until you go to it.
- **No hidden state.** Sessions and who is waiting in the header; mode, keys and `needs you` in the footer; the context meter always on. A queued line shows `> ` above the composer.
- **Esc is one ordered stack**: sheet → card (cancel/deny) → dropdown → interrupt the turn; `ctrl+c` clears the composer, twice exits, and says so.
- **Readable widths**: prose wraps at `min(width, 100)`; tables and code never wrap — they fold to the width and open in a sheet.
- **Predictable geometry**: the composer never moves; the transcript baseline is fixed; cards and sheets are layers, not reflows.
- **Latency**: a keystroke echoes within one frame; the loop never awaits the kernel.
- **Accessibility**: colour never the only signal; `NO_COLOR`, `BINGO_MOTION=off`, `BINGO_ASCII=1` (`>` `*` `+` `x` `-` `|`) honoured; every card readable at 80×24.

## 8. Extension by plugins (ADR-0013)

A plugin never sees the TUI. It describes what to show as a `View` and chooses a lane by how long it should last:

| lane | call | lifetime | drawn |
|---|---|---|---|
| block | `ToolOutput.display = Some(view)` | with the item, in the transcript | under the tool row, folded like any output |
| panel | `host.extend(session, plugin, kind, view.into())` | journaled; back after `--continue` | a rail card, or the panel sheet |
| live | `host.signal(session, plugin, kind, view.into())` | until replaced or `Null`; gone on resume | a rail card that updates in place; under the running rows below 120 columns |

Interaction: a `View::Actions` row (`[ 1 Approve ] [ 2 Next hunk ]`) fires `Input::Action{name, args}` into the plugin's command; a question that must stop the turn is `ToolHost::ask` as always. A live `Progress` gets the gradient and the sheen for free; a `Table` in a rail card gets the hairlines; a `Badge` with tone `attention` pulses like everything that wants you. The TUI renders each node exactly once, tested once; a plugin's UI is a value asserted with `assert_eq!`.

## 9. Measures

What a change is checked against, in this order: `TestBackend` snapshots for every region and layer in §3 and every node in §5, at 80×24 and 120×40; `assert_row_styled` proving colour lands only where §4 says; a frame-by-frame test with an injected clock for every row of §6 (the reveal at frames 1, 2, 3; the tail at 0, 75 and 150 ms); the tmux drives (`scripts/tui-smoke.sh` plus card, sheet, toast, signal, action, mouse); a 5 000-block transcript scrolling at 30 fps with a draw under 4 ms; an idle frame count of 0 over 2 s.

## 10. Decisions

- **2026-08-30** — Direction: the tape (A) was proposed and then set aside for a **full-screen application** at the user's call: full control over layout and motion is worth more than the terminal's native scrollback, which the frame replaces (virtualised, smooth, searchable, selectable, printed back on exit). The bench's band becomes the header's tabs; the editorial measure (≤ 100) stays for prose. The warm colour is reserved for wanting. The composer loses its box; hints live in the footer alone. The anchor is **alive**, with every motion reporting a state. Extension is UI as data (ADR-0013), unchanged by the road taken.
