# The terminal surface — design language

> The record M11 and every later TUI change starts from. Decisions are dated at the end; a change to this file is a change of taste, made on purpose. The comparison that led here is the 2026-08-30 proposal (three directions: tape, bench, editorial); this is the tape, made rich.

## 1. Anchor

bingo in a terminal is a counterpart you work beside, not a dashboard you watch. It is looked at for hours and spends most of them waiting — on a model, on a tool, on you. So the surface is **quiet, scannable, and out of the way**: nothing that is not moving glows; one glance tells apart what it said, what it did, and what it wants; the surface's own furniture — borders, hints, badges — earns its place or goes, and width and brightness are spent on content.

Three named references, one trait each: `git log`'s gutter discipline (one column of glyphs says everything), `less`'s restraint (one line of chrome), and the conversation living in the terminal's own scrollback (the history is yours; it survives exit, it selects, it greps). Density is middle: 80 columns must be good; a second column exists only past 120.

## 2. Three glances

| glance | sees | how |
|---|---|---|
| first | what it **wants from you** (a dialog, `needs you`); else the **latest answer** | the one warm colour is reserved for wanting; the answer is the brightest text, flush left, its own block |
| second | what it is **doing / did**: thinking, tool rows, output tails, receipts | indented two, dim; state is one gutter glyph `○ ⠹ ✓ ✗ ⊘` |
| third | furniture: mode, model, context, keys | one dim footer line; the composer is one `❯` |

## 3. Layout: a tape with overlays

The transcript is the terminal's scrollback. The surface owns only a **live region** at the bottom, drawn with an inline viewport whose height follows its content (capped at 60 % of the screen):

```
  ─ scrollback (settled, written once) ──────────────────────────
  ❯ what is in this workspace?
    ✻ thought for 2s
    ✓ Read Cargo.toml
      ⎿  [package] · name = "demo" · version = "0.1.0"
  The workspace is one package, demo 0.1.0. …
  ─ live region ──────────────────────────────────────────────────
  project ● · reviewer ⠹ · #design            ← band, only when ≥ 2 sessions
    ⠹ Write note.txt · 4s                     ← unsettled items: streaming text, running tools
    ▸ bingo.build · progress ████████░░ 80 %   ← live signals (ADR-0013 lane 3)
  ────────────────────────────────────────
  Permission · Write note.txt                 ← dialog
    ❯ 1  Yes  …
  ────────────────────────────────────────
  ❯ ▌                                         ← composer, borderless
  default · ? help          fake-1 · ctx 2% · 1 agent · 1 needs you
```

- **Settling.** An item is drawn in the live region while it is not final and is written above, once, when it completes: a user line on submit, a tool row on its result, assistant text on `ItemCompleted`. Settled rows are not reflowed on resize — that is what scrollback means.
- **One tape for the tree.** Children and rooms settle into the same tape with their prefix (`↳ reviewer`, `#design ❯ reviewer:`); the switcher changes which session the composer talks to and whose live region shows, never the history. Scrollback cannot be swapped, and a single timeline is what a person wants anyway.
- **Overlays** take the alternate screen for as long as they are open, then hand the tape back untouched: help (`?`), the session picker, a long tool output (`ctrl+o` on the latest), a diff larger than the live region (`ctrl+e` in a permission dialog), an image, the full plugin panel (`ctrl+t`). An overlay is the only thing that may fill the screen.
- **Nothing jumps.** The composer never moves up or down while you type; new content pushes the tape up. A dialog opens above the composer, not over it.

## 4. Tokens

Colours are the terminal's **named ANSI colours**, so they follow the user's theme in light and dark alike. A truecolor palette is applied only once the terminal's background is known (OSC 10/11); its dark-side reference values are given here, the light side is derived by the same roles.

| token | ANSI | truecolor (dark) | where, and only where |
|---|---|---|---|
| `text` | default | `#d6d8de` | answers, what you type, option labels |
| `dim` | DIM | `#737884` | work rows, receipts, rules, footer, hints, the caret |
| `structure` | cyan | `#6cc3d5` | the `❯` and `!` prompts, the running spinner, the selected row's number, links |
| `attention` | yellow | `#dcae4b` | dialog titles, `needs you`, `⊘`, the bypass badge — the one warm colour, only for wanting |
| `good` / `bad` | green / red | `#8fc98f` / `#e07b7b` | `✓` `✗` and a failed turn's line; prose is never coloured |
| `bold` | BOLD | — | dialog titles, markdown headings; never emphasis in prose |
| background | none | — | no REVERSED, no fills; selection is `❯` plus `structure` |

Colour never carries a fact alone: every state also has a glyph, so a monochrome terminal and `NO_COLOR` lose nothing.

### Glyphs and words

| element | form |
|---|---|
| your line | `❯ ` flush left; in a room `❯ reviewer: …` |
| work row | indent 2: `  ✓ Read Cargo.toml`; states `○` pending `⠹` running `✓` done `✗` failed `⊘` stopped; `●` retired |
| tool output | indent 4 + `⎿  `, at most three tail rows while running, `… +N lines` after; a short result joins one row with ` · ` |
| receipt | joins the row: `✓ Write note.txt · allowed`, `· denied — <feedback>` |
| answer | flush left, `text`, a blank line above and below, measure `min(width, 100)` |
| thinking | `✻ thinking · 3s` while it lasts, `✻ thought for 3s` after; dim; expandable in an overlay |
| paths | relative inside the cwd, `~` for home, middle-elided `…` beyond 48 cells |
| dialog | a dim rule above and below; title `attention` + `bold`; options `❯ 1  Yes`; one hint line |
| composer | no border; `❯ ` + text + dim `▌`; continuation lines indent 2; in a room the prompt is `#design ❯` |
| footer | one dim line: left `mode · ? help`, right `model · ctx N% · N agents · N needs you`; no hint is repeated in the composer |
| band | only with ≥ 2 live sessions: `project ● · reviewer ⠹ · #design`, the current one bold |
| rules | `─` for dialog bounds and compaction; nothing else draws a line |

Spacing has one base: 2 cells. Indents are 0 / 2 / 4 (+ the 3-cell `⎿  `); a blank line separates turns and frames an answer; nothing else adds vertical space.

## 5. Content kinds

What the tape can hold, each with one degrade so `--print` and an IM channel never lose information:

| kind | drawn as | degrade |
|---|---|---|
| markdown | headings bold, lists `•`, quotes `│`, tables ruled, links underlined with the url in dim | the text |
| code | fenced, syntax-highlighted (ANSI-16 palette classes), line numbers when > 8 lines | the text |
| diff | unified, coloured by column, word-level emphasis inside changed lines, `ctrl+e` to expand | the unified text |
| table / key-value | ruled by dim rules, right-aligned numbers, `–` for a missing cell | rows joined by ` · ` |
| progress | `████████░░ 80 % · label`, or a spinner when unbounded | `label 80 %` |
| badge | `[ text ]` in the tone's colour | `[text]` |
| tree | `├─ └─` with per-node glyphs and badges | indented lines |
| image | kitty / iTerm2 / sixel, else half-block cells, else `[image: name]`; opened full-size in an overlay | `[image: name]` |
| view (plugin) | ADR-0013's vocabulary, any nesting | `View::text()` |

## 6. Motion

A terminal can do three things: change one cell's glyph, add or remove a row, change brightness. The principles: **what is still does not flicker; what is fast is not shown; what waits has a clock.**

| moment | cue | rhythm |
|---|---|---|
| text streaming | grows in place; a dim `▌` marks the growing edge; deltas coalesced per frame | ≤ 60 Hz, no per-character delay |
| thinking | `✻ thinking · 3s` ticking, decays to `✻ thought for 3s` when the answer starts | 1 s |
| tool running | braille spinner in the gutter, three dim tail rows, folds into the output row on completion | 80 ms per frame |
| activity line | appears only after 300 ms of a turn: `⠹ Write note.txt · 4s · esc to interrupt`; a fast turn never flashes it | 300 ms delay, 1 s clock |
| dialog opening | rows drawn dim during the kernel's 400 ms keyboard guard, then plain, with the bell; the guard is seen, not suffered | once |
| answer settling | none: the answer is the last bright block, the work rows stay dim — decay is the transition | — |
| needs you | the footer badge pulses dim/plain while a child waits | 1 Hz |
| notices | on the status row for 5 s, dim after 3 s; an OSC 9/777 notification when the window is unfocused | 5 s |
| switching sessions | the live region changes, the tape does not; the band's bold moves | — |
| idle | no redraw without a frame, a key, or a spinner on screen | 0 Hz |
| reduced motion | `NO_COLOR` strips colour; `BINGO_MOTION=off` freezes spinners to `•`, stops clocks and pulses | — |

Frame budget: a draw of the live region under 4 ms; the tape costs nothing after settling. Smoothness is the absence of flicker, jumps and dropped keys, not the presence of animation.

## 7. Ergonomics

- **Hands stay on the keys.** Every dialog answers to one key (`1-9`, `y/a/n`, `⏎`, `esc`); every overlay closes with `esc`; `?` shows the whole table. The mouse is the terminal's: with the tape in scrollback, selection and search are native; the wheel scrolls overlays.
- **One focus, always visible.** The `❯` marks what the keyboard talks to: the composer, a dialog row, an overlay's row. Focus moves only by opening and closing, never by ambient events; a child that asks does not steal the composer — it pulses in the footer until you go to it.
- **No hidden state.** Mode, model, context, who is waiting: all on the footer. A queued line shows `> ` above the composer. A running child is a spinner in the band.
- **Esc is one ordered stack**: overlay → dialog (cancel/deny) → dropdown → interrupt the turn; `ctrl+c` clears the composer, twice exits, and says so.
- **Readable widths**: prose wraps at `min(width, 100)`; tables and code scroll sideways in an overlay rather than wrapping.
- **Predictable geometry**: the composer never moves; the live region grows downward from a fixed baseline; the tape only ever pushes up.
- **Latency**: a keystroke echoes within one frame; the loop never awaits the kernel; a frame is drawn at most once per 16 ms.
- **Accessibility**: colour is never the only signal; `NO_COLOR` and `BINGO_MOTION=off` are honoured; every glyph has an ASCII fallback (`BINGO_ASCII=1`: `>` `*` `+` `x` `-` `|`).

## 8. Extension by plugins (ADR-0013)

A plugin never sees the TUI. It describes what to show as a `View` and chooses a lane by how long it should last:

| lane | call | lifetime | drawn |
|---|---|---|---|
| block | `ToolOutput.display = Some(view)` | with the item, in the tape | under the tool row, folded like any output |
| panel | `host.extend(session, plugin, kind, view.into())` | journaled; back after `--continue` | `ctrl+t`, and in the live region while pinned |
| live | `host.signal(session, plugin, kind, view.into())` | until replaced or `Null`; gone on resume | the live region, under the running rows |

Interaction: a `View::Actions` row (`[ Approve all ] [ Next hunk ]`) fires `Input::Action{name, args}` into the plugin's command; a question that must stop the turn is `ToolHost::ask` as always. Examples: a build tool signals a `Progress` every second and settles a `Code` block of the last twenty lines; a review plugin extends a `Panel{Table + Actions}` board that survives a restart; a diff tool displays `Diff` in its output so the person reads columns while the model reads the unified text.

The TUI renders each node exactly once, tested once; a plugin's UI is a value asserted with `assert_eq!`.

## 9. Measures

What a change is checked against, in this order: `TestBackend` snapshots for every screen in §3 and every node in §5; `assert_row_styled` proving colour lands only where §4 says; timing tests with an injected `now` for every row of §6; the tmux scenes (`scripts/tui-smoke.sh` plus the M11 drives: settle, overlay, signal, action); a 5 000-row insert into the tape under 200 ms with no dropped frame.

## 10. Decisions

- **2026-08-30** — Direction: the tape (A), not the bench or the editorial layout; the bench's band is borrowed only past one live session, the editorial measure (≤ 100) for prose. The warm colour is reserved for wanting. The composer loses its border; hints live in the footer alone. One tape for the whole tree. Extension is UI as data (ADR-0013), not widget crates.
