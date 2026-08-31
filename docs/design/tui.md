# The terminal surface — design language

> The record M11 and every later TUI change starts from. Decisions are dated at the end; a change to this file is a change of taste, made on purpose. The 2026-08-30 comparison (tape / bench / editorial) led to the tape; the same evening the user chose the other road — a full-screen application that owns every cell — and asked for life, not restraint. This is that.

## 1. Anchor

bingo in a terminal is a counterpart you work beside for hours, and it should feel **alive**: it breathes while it thinks, its work arrives with a pulse, what it wants from you comes forward and the rest recedes. Alive is not busy — every motion says something true about state (thinking, arriving, waiting, done), nothing moves for decoration, and stillness is the default when nothing is happening. Three rules hold the energy: **hierarchy first** (the eye is led, never searched), **content over furniture** (chrome is thin and dim; colour and motion are spent on what matters), and **full control** (the surface owns the whole screen, so nothing is left to the terminal's mercy — scrolling, selection, layout and every transition are ours to make good).

The **grammar is Claude Code's**, taken whole because a person who knows it should feel at home: `⏺` for what the model says and does, `⎿` for what came back, `>` for what you said, a rounded box around the input, `✻` that sparkles through a verb while it works (`✻ Simmering… (esc to interrupt · 8s · ↓ 1.2k tokens)`), `… +12 lines (ctrl+o to expand)`, a bordered card asking `Do you want to…?` with numbered answers, a mode line `⏵⏵ accept edits on (shift+tab to cycle)`, a welcome box. On top of that grammar, what Claude Code does not do: warm truecolor, an input box that glows while the model works, bullets that pulse while live and flip when done, a comet tail on streaming text, blocks that rise into place, translucent diff backgrounds, child sessions as rows you can step into, a rail, and a full screen we own. And one thing Claude Code does not do that we keep not doing: a header — nothing sits above the transcript; the one line of furniture is under the input box. Other references, one trait each: `btop` — live meters that never flicker; Charm's `gum` — motion with wit and restraint. Density is middle-high: 80 columns must be good, 120 earns a rail.

## 2. Three glances

| glance | sees | how |
|---|---|---|
| first | what it **wants from you** (a card, `needs you`); else the **latest answer** | a card has the only bright border on screen and dims what is behind it; `needs you` pulses; the answer is the brightest text after a white `⏺`, its own block |
| second | what it is **doing / did**: thinking, tool rows, output tails, live signals | `⏺ Tool(args)` rows whose bullet is orange and pulsing while live, green when done, red when failed; results under `⎿` in dim |
| third | furniture: mode, notices, where you are, model | one status line under the input box, dim; nothing above the transcript; the input box's border is dim until the model works |

## 3. Layout: the frame

The surface owns the alternate screen for the whole run. Regions, top to bottom — nothing above the transcript — with a rail past 120 columns:

```text
                                                       ┌ rail ─────────────┐
  > what is in this workspace?                         │ Todos             │
                                                       │ ☐ write the plan  │
  ⏺ I'll read the manifest first.                      │                   │
                                                       │ cargo test        │
  ⏺ Read(Cargo.toml)                                   │ ▬▬▬▬▬▬▬▬░░ 24/33  │
    ⎿  Read 3 lines                                    └───────────────────┘

  ⏺ reviewer(review the manifest)                                        ← a child session is a row; ⏎ steps in
    ⎿  Done (4 tools · 8.1k tokens · 40s)

  ⏺ The workspace is one package, demo 0.1.0. …

  > write me a note and run the tests

  ✻ Thought for 2s

  ⏺ Write(note.txt)                                                       ← bullet orange, pulsing
    ⎿  Wrote 27 bytes to note.txt

  ✻ Tinkering… (esc to interrupt · 4s · ↓ 0.4k tokens)                   ← activity row, sparkling
  ╭──────────────────────────────────────────────────────────────────╮
  │ > ▌                                                              │    ← input box, glowing while it works
  ╰──────────────────────────────────────────────────────────────────╯
  ⏵⏵ accept edits on (shift+tab to cycle)   1 needs you · context 84%   gpt-5.4  ← status line: mode · notices · place
```

- **Transcript**: a virtualised list of blocks (one per item), scrolled by us — smooth, by line, with the mouse wheel and `pgup/pgdn`; `ctrl+f` searches it; `v` or a mouse drag selects and `y`/`ctrl+c` copies through OSC 52; a long block folds to its tail and opens in place on `⏎` when focused. The frame around it is a hairline in the raised tint, not a bright box.
- **Sessions**: a child or peer is a row in the transcript where it began — `⏺ reviewer(review the manifest)` with `⎿  Running… 3 tools · 1.2k tokens` while it works, `⎿  Done (4 tools · 8.1k tokens · 40s)` after, `⎿  Needs you` pulsing — and `⏎` on that row steps into its transcript; the status line's right slot then reads `in reviewer`, and `ctrl+g` (the switcher: a dropdown above the input box, like the `/` menu) steps anywhere. A room's messages are `> reviewer: …` lines. **The quick cycle** (user-directed, 2026-08-31): `↓` on an empty composer turns the status line into a chip strip below the box — `❯ ⏺ project  ⏺ reviewer  #design`, agents then rooms, the roster's own dots — and `←`/`→` switch the view live as the cursor moves, wrapping at the ends; `↑`, `esc` or typing gives the line back. It appears only on the gesture, so the one-line-of-furniture rule holds; there is still no tab strip: a session that is doing nothing is not on screen until you ask to walk them.
- **Rail** (≥ 120 columns): pinned panels and live signals (ADR-0013), each a small card, from the top row down; below 120 columns the same cards draw in the transcript under the running rows; without them the rail is not drawn.
- **Cards** are the dialog form: a permission, a question, a sign-in, the switcher, the rewind picker. A card is a bordered box under the `⎿` of the row that asked — Claude Code's `Do you want to…?` — with the only bright border on screen; what is behind it dims, it reveals top-down over three frames, and `esc` closes it in reverse. Focus moves into a card and back out — never anywhere else.
- **Sheets** take the whole frame for a moment — help, the pager for a long output or diff, an image, the full panel list — and slide up from the composer over four frames; `esc` slides them down.
- **Status line**: the one line of furniture, under the input box, three slots. Left, the mode: `⏵⏵ accept edits on (shift+tab to cycle)`. Middle, notices — only what is true now: `1 needs you (ctrl+g)` pulsing, `2 running`, `context 84%` from 70 % of the trigger warming toward `bad`, the latest notice for 4 s; `? for shortcuts` while the box is empty and nothing else is true. Right, place: `in reviewer · gpt-5.4` (`gpt-5.4` alone at the root). It is vim's line — status and message in one row.
- **Notices** are toasts without a corner: a notice fades into the status line's middle slot over two frames, holds 4 s, fades to dim and goes; the next waits its turn.
- **Nothing jumps.** The input box never moves; the transcript grows upward from a fixed baseline; a card's reveal never shifts what is behind it. Resize re-lays the frame in one draw. The **activity band is reserved**: its two rows (air + the verb row) are held even while idle, so a turn starting or ending never reflows the transcript — the verb appears and leaves in place.
- **Leaving** prints the last screenful of the transcript, plain, into the shell's normal screen so the conversation is still on the terminal after `exit` — the one thing the alternate screen would otherwise take.

## 4. Tokens

Truecolor is the native look, chosen after the terminal's background is read (OSC 10/11) and derived for light and dark alike; the eight ANSI colours are the fallback and every rule below holds in both. The body background is never painted — the terminal's stays — but cards, sheets and the rail sit on a **raised tint** one step from it, which is what gives the frame depth.

| token | ANSI | truecolor (dark) | where, and only where |
|---|---|---|---|
| `text` | default | `#ece7df` | answers, what you type, option labels — warm off-white |
| `dim` | DIM | `#8a847a` | results under `⎿`, thinking, hints, the status line, everything behind a card |
| `raised` | none | `#211d17` (bg + one step) | the bar behind a `>` line, rail cards, the surface of a sheet; never text |
| `presence` | yellow | `#d97757`, glow `#f2a07c` | bingo's own colour: the `✻` sparkle, a live `⏺`, the glowing input border, the welcome mark, progress fills — the one warm colour, and the only one that moves |
| `good` / `bad` | green / red | `#8fcf8a` / `#e0655a` | a finished `⏺` and a failed one; a failed turn's line; diff tints |
| `mode` | blue | `#8fb4de` | the `⏵⏵` on the status line and links — the one cool colour |
| `bold` | BOLD | — | tool names in `⏺ Read(…)`, card titles, markdown headings; the model's own `⏺` is bold white |

Gradients exist in two places only: a progress bar's fill (`presence` → its glow) and the comet tail of streaming text (§6). Diff lines sit on translucent `good`/`bad` tints. Colour never carries a fact alone: every state has a glyph, so `NO_COLOR` and a monochrome terminal lose nothing.

### Glyphs and words

| element | form |
|---|---|
| presence | `✻` on the activity row and in the welcome box, `presence`; the activity row's sparkles `✻ ✢ ✶ ✽` and breathes while a turn runs; when idle none of it is on screen |
| your line | `> what is in this workspace?` on a `raised` bar the width of the transcript; in a room `> reviewer: …` |
| the model speaks | `⏺` bold white, then the text; a blank line before and after |
| tool row | `⏺ Read(Cargo.toml)` — the name bold, the argument plain; the bullet `presence` and pulsing while live, `good` when done, `bad` when failed |
| result | `  ⎿  Read 3 lines` in dim; three tail rows while running; then `… +12 lines (ctrl+o to expand)` |
| receipt | joins the result: `⎿  Wrote 27 bytes to note.txt`, `⎿  denied — <feedback>` |
| thinking | `✻ Thinking…` dim italic while it lasts, `✻ Thought for 2s` after; `⏎` opens it in a sheet |
| activity row | `✻ Simmering… (esc to interrupt · 8s · ↓ 1.2k tokens)`: the sparkle cycles, the verb is one of bingo's own (Simmering, Noodling, Tinkering, Rummaging, Mulling, Weaving, Sketching, Percolating — one per turn, at random), the clock ticks each second |
| card | a bordered box (`╭ ╮ ╰ ╯` in `presence`) under the asking row's `⎿`: title bold, a diff or command preview on its tints, `Do you want to create note.txt?`, `❯ 1. Yes` / `2. Yes, allow Edit(src/) during this session` (the scope is the kernel's answer spec; no `(shift+tab)` — here shift+tab cycles the mode and does not answer) / `3. No, and tell bingo what to do differently (esc)`; no hint row under the options — every hint lives in the thing it acts on |
| input box | `╭─╮ │ > ▌ │ ╰─╯` the width of the transcript, one to ten rows; the border `dim`, `presence` and glowing while the model works; a dim placeholder until the first keystroke |
| child row | `⏺ reviewer(review the manifest)` — the row *is* the call that spawned it (not `⏺ SpawnAgent(…)`), the name bold, the brief plain, the bullet by state as a tool's; `⎿  Starting…` before its first turn, `⎿  Running… 3 tools · 1.2k tokens` live, `⎿  Done (4 tools · 8.1k tokens · 40s)`, `⎿  Needs you` (an open question, not merely unread) pulsing; a room's row is its name alone; `⏎` steps in |
| switcher | `ctrl+g` (as since M8; `ctrl+t` stays the panel sheet, Claude Code's todo key): a dropdown above the input box like the `/` menu — one session per row, `❯ reviewer ⏺ running · needs you`, the coloured `⏺` for the glance and the word for `NO_COLOR`; no title, no hint row — `⏎` enters, `esc` closes |
| status line | left `⏵⏵ accept edits on (shift+tab to cycle)` in `mode` (`default` shows nothing but the hint); middle, notices only while true — `1 needs you (ctrl+g)` pulsing `presence`, `2 running`, `context 84%` (`dim` from 70 %, `bad` from 90 % with ` · /compact`), the latest notice for 4 s, else `? for shortcuts` while the box is empty; right `in reviewer · gpt-5.4`, dim |
| welcome | a bordered box on a fresh session: `✻ Welcome to bingo!`, `/help for help · /login codex to use a subscription`, `cwd: ~/…`; scrolls away like anything else |
| todos | `☐ ☒` rows in a rail card (`bingo.tasks` through ADR-0013) |
| paths | relative inside the cwd, `~` for home, middle-elided `…` beyond 48 cells |

Spacing: `⏺` at column 0, its text at 2; `⎿` at 2, its text at 5; a blank line between blocks; cards and boxes pad by one cell. Measure for prose `min(width, 100)`.

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
| meter | `▁▂▃▅▇` sparkline; the context meter in the `/status` sheet | `42 %` |
| view (plugin) | ADR-0013's vocabulary, any nesting, in a block, a rail card or a signal | `View::text()` |

## 6. Motion

A terminal moves in three ways — a glyph changes, a row appears or leaves, brightness shifts — and at 30 frames a second that is enough for motion with intent. Principles: **every motion reports a state change; stillness is the default; nothing moves for decoration; a person may switch all of it off.**

| moment | cue | rhythm |
|---|---|---|
| presence | the activity row's `✻` sparkles `✻ ✢ ✶ ✽` and breathes between 65 % and 100 % while a turn runs; the input box border glows in step; neither exists when idle | 150 ms per glyph, 1.6 s breath |
| thinking | `✻ Thinking…` dim italic, then decays to `✻ Thought for 2s` as the next block starts | — |
| streaming | text after a `⏺` grows in place with a **comet tail**: the last eight cells fade from `presence`'s glow to `text` | per frame, tail 180 ms |
| tool running | the row's `⏺` pulses `presence` ↔ glow; three dim `⎿` tail rows that scroll up as they arrive | 1.2 s pulse |
| tool done | the `⏺` turns `good` (or `bad`) with one bold frame; the tail folds into `⎿ … +N lines (ctrl+o to expand)` | 33 ms flash |
| block arriving | a new block rises two rows into place | 3 frames, ease-out |
| activity row | appears only after 300 ms of a turn: `✻ Tinkering… (esc to interrupt · 4s · ↓ 0.4k tokens)`; one verb per turn | 300 ms delay, 1 s clock |
| card opening | the world dims; the card reveals top-down; rows are dim through the kernel's 400 ms guard and brighten as it lifts, with the bell | 3 frames + guard |
| card closing | the reverse; the world brightens | 3 frames |
| sheet | slides up from the composer; `esc` slides it down | 4 frames |
| notice | fades into the status line's middle slot; holds 4 s; fades to dim and goes | 2 frames in, 2 out |
| needs you | the `1 needs you` notice, the child row's `⎿  Needs you` and its switcher row pulse `presence` | 1 Hz |
| scroll | by line with ease-out; the wheel, `pgup/pgdn`, `ctrl+f` hits | 100 ms |
| session switch | the transcript crossfades through dim; the status line's right slot renames | 2 frames |
| context | the `context 84%` notice appears at 70 % of the trigger and warms from `dim` to `bad` across the last 20 %; absent below | per turn |
| idle | no redraw at all | 0 Hz |
| reduced motion | `BINGO_MOTION=off`: spinners freeze to `•`, no breath, no tail, no reveals, no pulse; `NO_COLOR` strips colour | — |

Budget: an animation tick of 33 ms while anything animates, none otherwise; a full draw under 4 ms at 120×40; a keystroke echoes on the next frame. Smoothness is the absence of flicker, tearing, jumps and dropped keys — the motion above sits on top of that, never instead of it.

## 7. Ergonomics

- **Hands on the keys, the mouse welcome.** Every card answers to one key (`1-9`, `y/a/n`, `⏎`, `esc`); every sheet closes with `esc`; `?` shows the whole table; `ctrl+o` expands the latest result; `ctrl+g` opens the switcher; `shift+tab` cycles the mode. The mouse scrolls, clicks a child's row or a card row, drags a selection; it is never required.
- **One focus, always visible.** `❯` marks what the keyboard talks to: the composer, a card row, a sheet, a focused block. Focus moves only by opening and closing, never by ambient events; a child that asks pulses until you go to it.
- **No hidden state, no idle furniture.** Nothing lives above the transcript. What is true now is on the status line: the mode; `N needs you`, `N running` and the context percentage for as long as they are true; where you are and the model. A queued line shows dim above the input box; `/status` opens the sheet with the rest.
- **Esc is one ordered stack**: sheet → card (cancel/deny) → dropdown → interrupt the turn; `ctrl+c` clears the composer, twice exits, and says so.
- **Readable widths**: prose wraps at `min(width, 100)`; tables and code never wrap — they fold to the width and open in a sheet.
- **Predictable geometry**: the input box never moves; the transcript baseline is fixed; cards and sheets are layers, not reflows.
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

What the surface decides, and a plugin never asks about: a panel is pinned into the rail with `⏎` on its row in the `ctrl+t` sheet, and the pin is remembered per session; a signal is a rail card the moment it arrives and needs no pinning; `tab` walks the cards and `❯` marks the one the keyboard is talking to; a key on that card fires the button it names — the plugin's `key` hint, else the button's position — and the button wears `…` until the session's stream answers. One plugin gets eight rows in the rail before the rest folds to `… +N lines`; below 120 columns the same cards draw under the running rows instead.

The worked example is `crates/bingo-demo-ui` — read it whole; this is its middle, and `cargo test --doc -p bingo-demo-ui` compiles it:

```rust
use bingo_demo_ui::Board;
use bingo_sdk::{ContentPart, HostHandle, KernelError, SessionId, ToolOutput, View};

/// The block lane: a person reads the board, the model reads the text.
fn block() -> ToolOutput {
    ToolOutput {
        parts: vec![ContentPart::text("3 rows")],
        is_error: false,
        display: Some(Board::default().view()),
    }
}

/// The panel lane: journaled, and back after `--continue`.
async fn panel(host: &HostHandle, session: &SessionId) -> Result<(), KernelError> {
    let view = serde_json::to_value(Board::default().view()).unwrap_or_default();
    host.extend(session, "bingo.demo.ui", "board", view).await
}

/// The live lane: never journaled, gone on a resume, `Null` removes it.
async fn live(host: &HostHandle, session: &SessionId, step: u64) -> Result<(), KernelError> {
    let bar = View::Progress { value: step, total: Some(15), label: Some("cargo test".into()) };
    let payload = serde_json::to_value(bar).unwrap_or_default();
    host.signal(session, "bingo.demo.ui", "progress", payload).await
}
```

## 9. Measures

What a change is checked against, in this order: `TestBackend` snapshots for every region and layer in §3 and every node in §5, at 80×24 and 120×40; `assert_row_styled` proving colour lands only where §4 says; a frame-by-frame test with an injected clock for every row of §6 (the reveal at frames 1, 2, 3; the tail at 0, 75 and 150 ms); the tmux drives (`scripts/tui-smoke.sh` plus card, sheet, toast, signal, action, mouse); a 5 000-block transcript scrolling at 30 fps with a draw under 4 ms; an idle frame count of 0 over 2 s.

## 10. Decisions

- **2026-08-30** — Direction: the tape (A) was proposed and then set aside for a **full-screen application** at the user's call: full control over layout and motion is worth more than the terminal's native scrollback, which the frame replaces (virtualised, smooth, searchable, selectable, printed back on exit). The bench's band becomes the header's tabs; the editorial measure (≤ 100) stays for prose. The anchor is **alive**, with every motion reporting a state. Extension is UI as data (ADR-0013), unchanged by the road taken.
- **2026-08-30, later** — Grammar: **Claude Code's, taken whole** (`⏺` `⎿` `>`, the rounded input box, the sparkle and its verbs, the bordered `Do you want to…?` card, `ctrl+o to expand`, `⏵⏵ accept edits on`), at the user's call, made sexier rather than different: warm truecolor, the glowing box, pulsing bullets, the comet tail, rising blocks, diff tints. Two earlier rules are reversed on purpose: the input box keeps its border (it is the grammar's signature and the glow needs it), and the warm colour is bingo's *presence*, not a reservation for wanting — wanting is the one bright border plus the pulse. `✓ ✗ ⊘ ○ ⠹` retire in favour of a coloured `⏺`.
- **2026-08-30, night** — **No header.** The session tabs and the context bar came out at the user's call: a header is a row of furniture that is always on, and Claude Code never spends one. What it carried goes where a terminal keeps such things — a child session is a **row in the transcript** where it began (`⏺ reviewer(…)` / `⎿  Running…`, `⏎` to step in) and `ctrl+g` is a switcher dropdown; **context is a notice** (`context 84%`, from 70 %, warming to `bad`), not a meter; the model and where you are sit at the right of the **status line**, one row under the input box — vim's status and message line in one — the only furniture on screen. The rail's toggle goes with the header: below 120 columns its cards draw inline.
- **2026-08-31** — **M11b landed** (worker B, merged `ecd87c9`), with the choices the doc had not made: the look is chosen once from `NO_COLOR` / `COLORTERM` / `BINGO_ASCII` and fixed to the ANSI table under test, so no snapshot depends on the terminal that ran it; `NO_COLOR` keeps DIM and BOLD (weights, not hues); a token may be spent only in the files a test names (`theme::tests::every_token_is_spent_where_the_design_says` — a new call site is argued in that table first); `Warn` wears `presence`, `Error` `bad`, `bypassPermissions` `bad`; the permission question is `Do you want to ` + the kernel's summary lowered, its paths shortened; the ASCII table adds `...` for `…` and `-` for `•`, and keeps `·` (Latin-1); diff hunk headers are `dim`, added/removed rows sit on tints only 24 bits can draw and the `+`/`-` column carries the fact elsewhere; the welcome box spans the transcript's full width (it agrees with the input box, not the prose measure) and is drawn only for a session this surface opened; the room prompt is `#design > ` with the placeholder `post to the room`; `Pending` and `Interrupted` bullets are `dim`; a receipt names its tool only when the row above did not.
- **2026-08-31, later** — **M11a landed and the two were joined** (worker A merged `3257d59`, integration `206e38b`). A's choices: the middle slot of the status line sits against the left margin when there is no mode, else centred; `v` starts a selection only while the transcript is held; a committed search owns the keyboard until `esc`; search is case-insensitive; a card **covers** the tail of the transcript rather than pushing it up (§3: layers, not reflows), and so do the `/` dropdown and a command's block; help and the panel do not capture the keyboard, the picker does; the old context badge and sparkline are gone — the context notice and `/status` are what remain; `esc` closes a layer over its frames and it stays until the last one. Integration choices: a card is anchored only under a row of the transcript on screen (a child's item ids are its own); a card taller than its room keeps its title and its newest rows — the preview gives way, never the answers; a card pads by one cell like every box; the switcher is a dropdown above the input box like the `/` menu, with no border and no dimming; the backdrop dims through `dim` (fg on 24 bits, DIM on eight); the current search hit is `presence` + bold, other hits bold on the tint, a selection `presence` on the tint — every mark visible on eight colours; `ctrl+o` opens the latest result whole (or the focused block's) and again folds it; a tab in a result runs to the next stop of eight.
- **2026-08-31, later still** — **M11d landed** (worker D, merged `d2a0693`), with its choices: the panel sheet now captures the keyboard (pinning needs `↑/↓ ⏎`; M11a's "the panel does not capture" is reversed on purpose); a rail card is not a bordered box — a bold title on the `raised` tint, one blank row between cards, no frame; a card is titled by its `Panel`'s own title, else by the kind it was published under; the live segment below 120 columns carries no tint; `Stack` puts nothing between its children (air is a `Text{""}`); a `Tree` draws whole (no fold cursor for a value handed afresh each frame); `Code` wears markdown's fence and a line-number gutter past eight rows; `KeyValue` wears no rule, dim keys, numbers right-aligned; an unbounded `Progress` is a three-cell lit run and its count; a table's rule spans the columns and `–` marks a missing cell; a fired button reads `[ 1 Approve… ]` until the stream moves; `tab` cycles rail cards and back round to none; a digit fires only while the composer is empty and a card has focus.
- **2026-08-31, at last light** — **M11c landed and the whole of M11's motion is joined** (worker C, merged `dd6bb3a`). C's choices: one wall clock for every free-running cue (a breath, sparkle or pulse takes the clock's own turn of its period — nothing holds an origin); the breath is a triangle, eased at both ends; a ramp collapses to its two ends on eight colours, changing over halfway; **esc closes a sheet before a card** (dismissing help must never deny a permission — the order §7 always said); a blank row above the activity row; the verb is seeded from the turn id (a turn never changes its mind, a test can name it); the token count is in thousands and absent until the turn has spoken; a guarded card is dim in its rows, bright in its border; the rise is room beneath the block, never an overlap — a resize is not an arrival and what a person arrived to never rises; **the oldest notice is the one being said** and the next waits; stillness is drawn, not skipped (`BINGO_MOTION=off` holds every cue at its resting frame); the desktop hears about the root's turn only (OSC 9 for iTerm2/Terminal.app, 777 elsewhere, tmux passthrough); the kernel's pace is not the screen's — frames coalesce to one draw per 33 ms and the loop draws once more on the way out. Two loop bugs its tests caught: an arriving delta used to cancel the animation sleep forever, and the loop measured with one clock while sleeping on another. Joined with M11d at the merge, plus two fixes the join forced: **the answer to a key is never a notice** — `press ctrl+c again to exit` is derived from the armed window on the status line's middle slot, ahead of every queued notice (a queued answer arrived after its own exit window had lapsed); and an armed window counts as animating, so its hint leaves the line on its own frame when it lapses.
- **2026-08-31, evening** — **M11e landed and M11 is whole** (worker E, merged `d7d5c59`). E's choices: a fence in an answer goes through `views::code` and a GFM table through `views::table` — a fence and a plugin's block are one rendering, gutter, fold and all; **three inks and no rainbow** (`keyword`→`mode`, `comment`→`dim`, the rest `text`), and operators stay plain — punctuation is not vocabulary, and colouring `=` and `&&` would put the cool colour on every other cell; a fence whose language is `diff` or `patch` **is a diff**: it goes to the one unified-diff renderer and wears its tints, not the inks; a table's alignment comes from its cells, not GFM's delimiter row — which column reads as numbers is a fact about the values; **`ctrl+o` only ever opens further** (fold → expanded → pager) and `esc` folds — M11a's "again to fold" reversed on purpose, one key never means two directions; the pager's keys are hinted on its own title row, not a hint row (§4: the hint lives in the thing it acts on); the rewind card is a dropdown above the input box like the switcher, but **with** a bold title — unlike the switcher's, its rows do not say what they are; `esc esc` needs no clock — any other key is what says the two presses were not one gesture; a completed `@` mention keeps its `@`, so the line itself says which of its words are paths and the attachments are derived from it rather than remembered beside it; the `@` walk is sorted by path — the filesystem's order is nobody's; `BINGO_THEME` is an env var beside `BINGO_MOTION`/`BINGO_ASCII`, not a settings key — the look is settled before the kernel is up; the light palette is §4's eight at the other end of their own lightness, with `raised` one step *down* from the ground; the background is probed only where the answer would change something (24 bits on, `NO_COLOR` off, no look named). Refused: **images** — `image` + `ratatui-image` measured +33 crates against the ~25 the plan allowed (the sixel quantiser is most of it); `ItemBody::Asset` keeps §5's `[label]` degrade, and an `@`-mentioned image still becomes an attachment. Recorded: no `/rewind` command exists yet, so the picker is offered only when a `rewind` spec is in the session's catalogue — silent today, lit the day a store command lands with no change to the surface.
- **2026-08-31, after M15** — **The activity band is reserved** (user-reported: "流式开始和结束的时候不稳定"). Frame-by-frame capture showed the whole bottom-anchored transcript bouncing two rows at each end of every stream: the verb row and its air arriving reflowed everything above, and the 300 ms delay made fast turns bounce for nothing. The verb and the easing were innocent. Fix: the band's two rows are held even while idle (`demand.activity.max(2)`), so the verb appears and leaves **in place**; idle screens wear the two rows as air above the box. Every screen snapshot moved up by two; four row-math tests were re-aimed. §3's "nothing jumps" now includes the band. Also decided, user-directed: the **quick cycle** — `↓` on an empty composer turns the status line into a chip strip (agents then rooms), `←`/`→` live-switch, `↑`/`esc`/typing returns the line; `ctrl+g` stays as the full tree card. Implementation queued behind M16's TUI-touching worker. **Landed** (worker C, merged `b17bcc9`): `cycle.rs` is pure — the marked chip *is* `tree.view()`, the only new fact is `cycling: bool`; the sketch's `●/○` gave way to the roster's `⏺` vocabulary (a room wears no dot; its `#` is its name); the mark is a `❯` cursor plus text-vs-dim weight, so `NO_COLOR` loses nothing; walking wraps; a second `↓` holds it open rather than closing; `↓` with one session keeps its old job silently; the strip's `esc` answers before the escape stack (the `ctrl+f` precedent) so dismissal never interrupts a turn; a mouse click switches the view and re-marks without dismissing — the keys own dismissal. Overflow scrolls the strip under the chip you are on, `…` at each cut end. Proof of §3: a snapshot asserts every row but the status line's is byte-identical with the strip open. Carried: the 80-column help sheet, already one line over, now drops `/exit` too — the sheet wants a scroll or bottom-anchor decision; the strip has no PTY scene (the smoke fixture has one session, where `↓` is rightly inert).
