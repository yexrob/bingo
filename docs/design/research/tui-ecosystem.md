# Rust TUI ecosystem, verified 2026-08-28

> Source: subagent report, archived verbatim. Facts were verified on the date in the title; re-verify before depending on a version.

## Rust TUI ecosystem for bingo — verified 2026-08-28

Verification sources: crates.io API (version/date/downloads/license), `gh api` (stars, last push, archived), docs.rs, GitHub source, deepwiki + raw source for openai/codex. Maturity: **A** = widely used + active + stable API; **B** = maintained but small/young/niche; **C** = stale, experimental, or license-problematic.

### Summary table

| Need | Recommended | Version (date) | Maturity | Gap / port note |
|---|---|---|---|---|
| Core TUI | `ratatui` (+ `scrolling-regions`) | 0.30.2 (2026-06-19), 46.8M dl, 22.4k★ | A | Inline viewport + `insert_before` covers "write-once scrollback"; Codex still forked `Terminal` for reflow/hyperlinks |
| Terminal I/O | `crossterm` (`event-stream`, `bracketed-paste`) | 0.29.0 (2025-04-05), 180M dl, 4.2k★ | A | Kitty flags yes; no OSC 9/99/777, no OSC 10/11 query (hand-write, ~50 lines) |
| Composer | `ratatui-textarea` | 0.9.2 (2026-06-12), 480k dl, 96★ | B | No kill ring, no history/reverse-i-search, Ctrl-U=undo not kill; wrap it, remap keys |
| Single-line inputs (pickers) | `tui-input` | 0.15.4 (2026-08-10), 1.9M dl | A | Single-line only, no undo |
| Readline semantics reference | `reedline` (core types only) | 0.51.0 (2026-08-22), 3.1M dl | A | Drives its own painter; single cut buffer. Port ideas, not code |
| Markdown → ratatui Text | `tui-markdown` (`highlight-code`) | 0.3.9 (2026-07-23), 437k dl, 116★ | B | Self-labelled "experimental PoC"; no streaming API → add Codex-style newline-commit collector |
| Markdown parser | `pulldown-cmark` | 0.13.4 (2026-05-20), 142M dl | A | — |
| Syntax highlighting | `syntect` (`default-fancy` or onig) + `two-face` | 5.3.0 (2025-09-27), 24.7M / 0.5.2+bat-0.26.1 (2026-08-07), 6.7M | A | Same stack as Codex |
| Diff algorithm | `similar` (or `diffy` for unified-patch parsing) | 3.2.0 (2026-08-17), 185M / 0.5.1 (2026-07-19), 14.6M | A | **No diff widget exists**; `ratatui-diff` is a reserved name only. Port Codex `diff_render.rs` (~gutter/wrap/limits) |
| Images | `ratatui-image` | 11.0.6 (2026-06-25), 750k dl, 387★ | B+ | Kitty via Unicode placeholders (U=1) + tmux DCS passthrough, sixel, iTerm2, halfblocks — all in |
| Fuzzy picker | `nucleo-matcher` | 0.3.1 (2024-02-20), 3.7M dl, 1.5k★, MPL-2.0 | A | Helix's matcher; UI is yours |
| Scroll/popup/prompt widgets | `tui-scrollview`, `tui-popup`, `tui-widget-list` | 0.6.7 / 0.7.6 (2026-06) / 0.15.3 (2026-07) | B | Fine, small; transcript pager still custom |
| Spinner/animation | `throbber-widgets-tui` / `tachyonfx` | 0.11.1 (2026-06-19, Zlib) / 0.25.1 (2026-07-05), 1.3k★ | B / B+ | — |
| Live command output | `tui-term` (vt100 + portable-pty) | 0.3.4 (2026-04-07), 229★ | B | README says WIP; depth milestone |
| Dark/light detect | `terminal-colorsaurus` | 1.0.3 (2025-12-28), 3.6M dl | A- | OSC 10/11 + DA1 trick + timeout; run before raw mode/probe batching like Codex |
| Color level | `supports-color` | 3.0.2 (2024-11-26), 61.6M | A | Codex uses it for truecolor/256/16 diff palettes |
| Title / bell | `crossterm::terminal::SetTitle`, `\x07` | — | A | — |
| OSC notifications | none mature — hand-write | (`peal` 0.1.0, 17 dl) | C | Port Codex `notifications/osc9.rs` (OSC 9 + tmux wrap + BEL fallback) |
| Snapshot tests | `ratatui::TestBackend` + `insta` | 1.48.0 (2026-06-11), 93M | A | TestBackend has `scrollback()`/`assert_scrollback` for inline mode |
| PTY smoke | `portable-pty` + `vt100` (+ `expectrl`/`rexpect`) | 0.9.0 (2025-02) / 0.16.2 (2025-07) / 0.9.0, 0.7.1 (2026-05) | A | Codex pattern: spawn binary in PTY, feed `vt100::Parser`, assert screen |
| App architecture | ratatui `component`/`event-driven-async` template + Codex structure | templates repo 419★ (2026-08-14) | A | Codex TUI is **not** a crate (`codex-tui` 404 on crates.io); Apache-2.0, vendorable |

---

### 1. Core

**ratatui 0.30.2** — MIT, released 2026-06-19 (0.30.0 series; repo pushed 2026-08-27). 0.30 split into `ratatui-core` 0.1.2 / `ratatui-widgets` 0.3.2 / `ratatui-crossterm` 0.1.2 / `ratatui-termion`, `ratatui-termwiz` 0.1.2 / `ratatui-macros` 0.7.2; MSRV 1.86, edition 2024; `Backend` trait now has an `Error` assoc type + `clear_region`; `ratatui::run()` helper; selectable `crossterm_0_28`/`crossterm_0_29` features and a new `termina` backend feature (not verified further). Cargo features that matter here: `scrolling-regions` (optional), `unstable-backend-writer`, `unstable-rendered-line-info`, `unstable-widget-ref` — exactly the four Codex enables. https://ratatui.rs/highlights/v030/ , https://docs.rs/crate/ratatui/latest/features

**`Terminal::insert_before(height, draw_fn)`** (docs.rs): no effect unless `Viewport::Inline`; with `scrolling-regions` "can be done without clearing and redrawing the viewport", otherwise it clears the viewport for the next draw; content beyond screen "will go directly into the terminal's scrollback buffer"; viewport is pushed to the bottom, then prior lines scroll up. `TestBackend` mirrors this with `scrollback()`, `assert_scrollback[_lines|_empty]`, `scroll_region_up/down` (feature-gated). So the "write-once scrollback" pattern is first-class in ratatui now. https://docs.rs/ratatui/latest/ratatui/struct.Terminal.html

**crossterm 0.29.0** — 2025-04-05 (17 months, but master pushed 2026-08-21; unreleased: MSRV 1.85, edition 2024, drops `IsTty`, parser underflow fixes). `KeyboardEnhancementFlags`: `DISAMBIGUATE_ESCAPE_CODES`, `REPORT_EVENT_TYPES`, `REPORT_ALTERNATE_KEYS`, `REPORT_ALL_KEYS_AS_ESCAPE_CODES` (alternate keys/Unicode codepoints "not yet supported"); `Push/PopKeyboardEnhancementFlags`, `supports_keyboard_enhancement()` query (0.29 also added OSC 52 clipboard). `EnableBracketedPaste`, mouse capture, `EventStream` (feature `event-stream`), `SetTitle`. Missing: OSC notifications, OSC 10/11 colour queries. https://docs.rs/crossterm/latest/crossterm/event/struct.KeyboardEnhancementFlags.html

**Alternatives**: `termwiz` 0.23.3 (2025-03-20, wezterm 28.6k★) — has `lineedit` (emacs keys, `History` trait) but it owns the terminal loop; ratatui-image says its termwiz backend is non-functional → not for this. `iocraft` 0.8.5 (2026-08-20, 1.5k★, Taffy flexbox, React-like, inline `.print()`/`render_loop()`) — no Ink-`Static` equivalent found, and would mean abandoning the ratatui widget ecosystem. `cursive` 0.21.1 (2024-08-03, 4.8k★) — retained view tree, no inline mode. `tuirealm` 4.1.0 (2026-05-02, 992★) — Elm/React on ratatui; usable but adds its own component/event vocabulary.

### 2. Text input / editor widgets

| Crate | Multi-line | Undo | Kill/yank | Emacs keys | History/rev-search | Soft-wrap | Custom keys | Status |
|---|---|---|---|---|---|---|---|---|
| `ratatui-textarea` 0.9.2 | yes | yes (50 default, `set_max_histories`) | single yank buffer (Ctrl-K/J/W + Ctrl-Y) | yes but Ctrl-U=**undo**, Ctrl-J=kill-to-head, Ctrl-R=redo | no | yes, char+word | `input_without_shortcuts()` + public methods | ratatui-org fork of tui-textarea, 2026-06-12 |
| `tui-textarea` 0.7.0 | yes | yes | yank | same table | no | **no** | same | last push 2024-12-01 → C |
| `tui-input` 0.15.4 | **no** | no | `Yank` | `GoToPrev/NextWord`, `DeletePrevWord`, `DeleteLine`, `DeleteTillEnd`, `DeleteFromStart` | no | n/a | `InputRequest` enum | active 2026-08 |
| `rat-text` 3.1.0 (rat-salsa) | yes (ropey) | yes | clipboard trait | **CUA** keys (Ctrl-Z undo, Ctrl-X/V, Ctrl/Alt-Backspace word, Ctrl-Left/Right) | no | yes | own `ct_event!` handlers | 64★, pulls rat-event/rat-focus/rat-scrolled |
| `edtui` 0.11.7 | yes | yes | vim registers | vim modes + modeless emacs-ish alt | no | yes | `KeyEventHandler` | 2026-08-16, 154★; syntect highlighting, `$EDITOR` via Ctrl+e feature |
| `reedline` 0.51.0 | yes | yes | single cut buffer (`CutFromStart`, `CutToLineEnd`, `CutWordLeft`, `KillLine`, `PasteCutBuffer*`) | yes | yes (SQLite/file, Ctrl-R) | n/a | `EditCommand` | `Editor`/`LineBuffer` public but painter is its own; not a ratatui widget |
| Codex in-tree `TextArea` | yes | **no** | single kill buffer, Ctrl-A/E/W/U/K, Alt-B/F, Alt-Backspace, Ctrl-Y | yes | separate `chat_composer_history.rs` | yes (own `wrapping` module) | — | vim mode + atomic "elements" (pills) |

Verdict: **`ratatui-textarea`** is the only maintained ratatui-native multi-line editor with emacs keys + undo + soft-wrap. Remap Ctrl-U/J to readline kill semantics via `input_without_shortcuts` + methods. **Gaps to write yourself** (nobody has them as a widget): multi-entry kill ring (Ctrl-Y/Alt-Y), prompt history + reverse-i-search, bracketed-paste collapse (Codex `paste_burst.rs`), `$EDITOR` compose (trivial: suspend, spawn, reload; Codex `external_editor.rs`). Use `reedline`'s `EditCommand` list and prompt_toolkit's key tables as the spec. `tui-input` for picker filter lines.

### 3. Markdown

- **`tui-markdown` 0.3.9** (joshka; MIT/Apache; pushed 2026-08-24): pulldown-cmark → `ratatui::Text`; `highlight-code` feature = syntect (default theme Base16OceanDark); headings, quotes, GFM alerts, lists, code, tables with borders/alignment, task lists, links, images-as-text, footnotes, math, definition lists; `from_str` / `from_str_with_options(Options{code theme, StyleSheet})`. README still says "experimental Proof of Concept". No streaming API.
- **`ratatui-markdown` 0.3.6** (celestia-island, 42★, targets ratatui 0.29, tree-sitter highlighting, mermaid): GitHub license is "Synthetic Source License (SySL) 1.0" while crates.io metadata says MIT OR Apache-2.0 — conflicting licence → **avoid**.
- **`termimad` 0.35.2** (2026-08-21, 6.9M dl, 1.2k★): the closest Glamour analogue (MadSkin, Hjson skins, tables, wrapping) but renders to crossterm directly, no syntax highlighting, no links — not a ratatui `Text` producer. Its skin model is worth copying for theming.
- Parsers: `pulldown-cmark` 0.13.4 (142M) — use; `comrak` 0.54.0 (BSD-2, GFM AST) and `markdown` 1.0.0 (2025-04) are fine but tui-markdown/Codex already standardise on pulldown-cmark.
- **Streaming**: Codex `markdown_stream.rs` `MarkdownStreamCollector`: `push_delta`, `commit_complete_source() -> Option<Range>` (only up to last `\n`), `finalize_and_take_source()` (newline-terminated). Committed source is rendered once and inserted into history; only the open tail block is re-rendered each frame ("completed top-level blocks retained, final block mutable"). Port this; it's ~200 lines.

### 4. Syntax highlighting

- **`syntect` 5.3.0** (MIT, 24.7M dl). Features: default = `default-onig` (oniguruma, C build via `onig_sys`); `default-fancy` = pure-Rust `fancy-regex` (slower on some grammars, no C toolchain); `dump-load` loads the embedded binary syntax/theme dump (fast startup — no YAML parse). `two-face` 0.5.2+bat-0.26.1 adds bat's extra syntaxes/themes; features `syntect-onig` (default) / `syntect-fancy` / `syntect-default-onig` / `syntect-default-fancy`. Codex: `syntect = "5"`, `two-face { default-features=false, features=["syntect-default-onig"] }`, and skips highlighting above a size limit ("avoiding thousands of parser initializations").
- `tree-sitter-highlight` 0.26.13 (2026-08-23): one C grammar crate per language → 20 grammars = 20 C compiles and larger binary; better fidelity but no gain for a chat renderer.
- `inkjet` 0.11.1 — repo **archived** (2025-09). `synoptic` 2.2.9 — last activity 2024-11-30, 35★.
- **Recommend**: `syntect` + `two-face`, load `SyntaxSet` once lazily (`OnceLock`), `default-fancy` if you want a pure-Rust build, else onig (Codex's choice, faster).

### 5. Diff rendering

No usable widget: `ratatui-diff` is a 0.0.0 placeholder ("Reserved for Ratatui unified and side-by-side diff viewers", 2026-06-12) with no public repo; search of showcase/awesome-ratatui finds only apps (ftdv, diff-tui). Use `similar` 3.2.0 (Apache-2.0, `TextDiff`, unified hunks, inline word diff) or `diffy` 0.5.1 (`Patch::from_str` for incoming unified diffs — Codex's choice). Port Codex `diff_render.rs`: `line_number_width = digits(max_line)`, GitHub-like backgrounds (`#213A2B`/`#4A221D` dark, `#dafbe1`/`#ffebe9` light, 256-colour equivalents, fg-only on 16-colour), continuation rows without line numbers, tab=4 hard wrap by display width, `max_rows` truncation with "… Diff preview limited" footer, summary header "Edited N files (+X -Y)".

### 6. Images

**`ratatui-image` 11.0.6** (MIT, ratatui org). Verified in source (`src/protocol/kitty.rs`, `src/picker.rs`, branch `master`): kitty is implemented **with Unicode placeholders** — `transmit_virtual` sends `_Gq=2,i={id},a=T,U=1,f=32,t=d,...` (virtual placement), cells are `U+10EEEE` + row/column diacritics (297-row cap), so images live in ordinary ratatui cells and can be emitted via `insert_before`; tmux is detected from env and every sequence is DCS-wrapped and chunked ("tmux seems to only allow a limited amount of data in each passthrough sequence"). WezTerm/Konsole are blacklisted for kitty because "neither implement the placeholder part of kitty correctly"; iTerm2 mis-detection issues (#158/#159) open. `Picker::from_query_stdio()` queries font size + capabilities (falls back to ioctl/env, #187); `ThreadProtocol` offloads resize+encode. Sixel via `icy_sixel` 0.6.0 (2026-08-19); iTerm2; halfblocks fallback. `viuer` 0.11.0 (2025-12) prints directly to stdout — only useful for plain-print mode.

### 7. Widgets

`tui-widgets` monorepo (ratatui org, 229★, pushed 2026-08-26): `tui-popup` 0.7.6, `tui-scrollview` 0.6.7, `tui-prompts` 0.6.7, `tui-big-text` 0.8.9, `tui-scrollbar`, `tui-cards`, `tui-qrcode`, `tui-bar-graph`. `tui-tree-widget` 0.24.1 (2026-08-09). `tui-logger` 0.18.3 (2026-07-04, 318★). `throbber-widgets-tui` 0.11.1 (Zlib licence — check policy). `tui-widget-list` 0.15.3 (2026-07-18) — heterogeneous stateful lists (good for the transcript). `tachyonfx` 0.25.1 (1.3k★) — effects; overkill for a status spinner. `tui-term` 0.3.4 (vt100 + portable-pty, README: "work in progress") — embed live command output later. `tui-menu` 0.3.1 (2025-12-30) — nestable menus. `ratatui-explorer` 0.3.0 (2026-03-06). `nucleo` 0.5.0 / `nucleo-matcher` 0.3.1 — crate releases are 2024 but repo active (2026-06-24), used by Helix; MPL-2.0 (file-level copyleft, fine for linking). Codex builds all pickers itself (`list_selection_view.rs`, `multi_select_picker.rs`, `file_search_popup.rs`, `selection_popup_common.rs`) with its own `codex-utils-fuzzy-match`.

### 8. Terminal capabilities

- Dark/light: `terminal-colorsaurus` 1.0.3 (OSC 10/11, DA1 sentinel, mio timeout, `theme_mode()`; 3.6M dl); `terminal-light` 1.9.1 (2026-08-23, Canop); `termbg` 0.6.2 (2025-01). Codex instead batches one startup probe: `CSI 6n` + `OSC 10;? OSC 11;?` + `CSI ?u` + `CSI c` (DA1 fallback), 100 ms deadline, replays consumed bytes into crossterm's parser — the right design if you also need cursor row for the inline viewport.
- Kitty keyboard in practice (Codex `keyboard_modes.rs`): flags `DISAMBIGUATE_ESCAPE_CODES | REPORT_ALTERNATE_KEYS`, add `REPORT_EVENT_TYPES` except on Ghostty/iTerm2 and tmux `extended-keys-format=xterm`; in tmux read `tmux show -gv extended-keys-format` and enable modifyOtherKeys-2 only for `csi-u`.
- OSC notifications: **no mature crate** (`peal` 0.1.0 has 17 downloads). Codex `osc9.rs`: `ESC ] 9 ; msg BEL`, tmux-wrapped `ESC P tmux ; ESC ESC ] 9 ; msg BEL ESC \` with ESC doubled; used only for Ghostty/iTerm2/Kitty/Warp/WezTerm, else BEL. OSC 99/777 not used by Codex.
- Title: `crossterm::terminal::SetTitle`; Codex animates a braille spinner in the title (`terminal_title.rs`). Colour depth: `supports-color` 3.0.2; `anstyle` 1.0.14 for style types if you emit plain ANSI in print mode. `console` 0.16.4 / `termion` 4.0.6 — not needed.
- Hyperlinks: Codex patched OSC 8 into its forked terminal (`custom_terminal.rs`) — ratatui has no hyperlink cell attribute.

### 9. Testing

`TestBackend` (Display impl → `insta::assert_snapshot!`, `assert_buffer_lines`, `assert_cursor_position`, scrollback asserts, `resize`) + `insta` 1.48.0; `ratatui-macros` 0.7.2 for `line!`/`span!` fixtures. PTY: `portable-pty` 0.9.0 + `vt100` 0.16.2 (`Parser::process`, `screen().contents()`), or `expectrl` 0.9.0 / `rexpect` 0.7.1 for expect-style. `vte` 0.15.0 if you want a raw parser. Codex has an in-tree `VT100Backend` (`test_backend.rs`) and `PtyCodex` harness — both worth copying; tmux-based testing is unnecessary given vt100.

### 10. Architecture / Codex reference

Codex TUI (Apache-2.0, 119k★ repo, `codex-rs/tui`, **not published as a crate**): `App` (event bus, `AppEvent`) → `ChatWidget` (history cells, streaming, `StatusIndicatorWidget`) + `BottomPane` (stack of `BottomPaneView`: `ChatComposer`, `ApprovalOverlay`, `ListSelectionView`, `MultiSelectPicker`, `CommandPopup`, `FileSearchPopup`, `CustomPromptView`, `McpServerElicitation`, `footer`). `Tui` = forked `custom_terminal.rs` (inline viewport, `last_known_cursor_pos`, `visible_history_rows`, `draw_with_size`, OSC 8) + `insert_history.rs` (DECSTBM `CSI top;bottom r` region above the viewport, `\r\n` writes, `ScrollbackStrategy::{Standard,Zellij,FullScreen}`, URL-preserving wrap policy) + `tui/{event_stream,frame_requester,frame_rate_limiter,keyboard_modes,scrollback,screen_size}` (crossterm `EventStream` + tokio broadcast, DEC 2026 `sync_update`, alt-screen only for `pager_overlay` Ctrl+T transcript — which has no search). Streaming markdown as in §3; diff as in §5; probes as in §8. Crates to adopt from it: ratatui (4 features above), crossterm (`bracketed-paste`,`event-stream`), pulldown-cmark, syntect+two-face, diffy, textwrap 0.16.2, unicode-width, supports-color, image, insta, vt100. Ratatui's own guidance (ratatui.rs/concepts/application-patterns): Elm / Component / Flux; `templates` repo has `component` and `event-driven-async` (tokio + EventStream + action channel).

### 11. Non-Rust references

- **Ink 7.1.1** (39.8k★): `<Static>` = items rendered once above the dynamic area into scrollback; ratatui `insert_before` (or Codex's DECSTBM writer) is the direct equivalent — no port needed, only the discipline "finalized cells are immutable".
- **Bubble Tea v2.0.9** (2026-08-19, 44.6k★) / **Glamour v2.0.1** (2026-06-12, 3.7k★; goldmark + chroma; JSON `StyleConfig` of per-element `StylePrimitive` — prefix/suffix/colour/bg/bold/margin/indent; dark/light/auto). Rust ports exist but are immature: `bubbletea-rs` 0.0.9 (2025-11, 278★), `lipgloss` 0.1.1 (2025-11), `charmed-glamour` 0.2.3 (2026-08-25, repo 37★, days old). Port **Glamour's stylesheet schema** (as a serde struct feeding tui-markdown's `StyleSheet`) — not its code.
- **prompt_toolkit 3.0.53** (2024-12-19): spec for emacs key table, multi-line prompt, reverse-i-search UI, completion menus. Nothing to port besides behaviour tables.

### Gaps in Rust → what to port

| Gap | Port from |
|---|---|
| Kill ring, prompt history + reverse-i-search, paste-burst collapse, `$EDITOR` compose around `ratatui-textarea` | reedline `EditCommand` set + prompt_toolkit key tables; Codex `chat_composer_history.rs`, `paste_burst.rs`, `external_editor.rs` |
| Streaming markdown commit logic | Codex `markdown_stream.rs` (newline-boundary collector) |
| Diff widget with gutter/wrap/limits | Codex `diff_render.rs` (on `similar` or `diffy`) |
| OSC 9/99/777 notifications + tmux wrap | Codex `notifications/osc9.rs` (+ add OSC 99/777 variants from kitty/wezterm docs) |
| Startup capability probe (cursor, OSC 10/11, `CSI ?u`, DA1) | Codex `terminal_probe.rs` (or `terminal-colorsaurus` if only theme is needed) |
| Markdown theme schema | Glamour `StyleConfig` → serde struct → tui-markdown `StyleSheet` / termimad-style skin |
| Transcript pager with search | Codex `pager_overlay.rs` (scroll/reflow/live-tail cache) + your own search |
| OSC 8 hyperlinks | Codex `custom_terminal.rs` diff/flush hook (needs `unstable-backend-writer`) |

### Recommended stack

**v1 (MVP)**: `ratatui 0.30` (`scrolling-regions`, `unstable-widget-ref`) with `Viewport::Inline` + `insert_before` for history and `Viewport::Fullscreen` toggle; `crossterm 0.29` (`event-stream`, `bracketed-paste`) + tokio; `ratatui-textarea` wrapped by a `Composer` that owns kill ring/history/paste-collapse; `tui-input` for filters; `tui-markdown` (`highlight-code`) + own newline-commit collector; `syntect` + `two-face`; `similar` + ported diff renderer; `nucleo-matcher` pickers; `tui-popup`/`tui-scrollview`/`tui-widget-list`; `throbber-widgets-tui`; `terminal-colorsaurus` + `supports-color`; `SetTitle`, BEL, OSC 9 hand-written; tests: `TestBackend` + `insta`, one `portable-pty`+`vt100` smoke test.

**Depth milestone**: `ratatui-image` (kitty placeholders, tmux, sixel/iTerm2); fork-or-vendor Codex `custom_terminal.rs`/`insert_history.rs` only if `insert_before` proves insufficient (resize reflow, Zellij, OSC 8); Codex-style batched startup probe replacing colorsaurus; `tui-term` for live command panes; `tachyonfx` for status animation; Glamour-schema theme files; OSC 99/777 backends; `expectrl` scripted PTY suites.

Sources (primary): crates.io API pages for each crate above; https://docs.rs/ratatui/latest/ratatui/struct.Terminal.html ; https://docs.rs/crate/ratatui/latest/features ; https://ratatui.rs/highlights/v030/ ; https://docs.rs/crossterm/latest/crossterm/event/struct.KeyboardEnhancementFlags.html ; https://github.com/crossterm-rs/crossterm/blob/master/CHANGELOG.md ; https://github.com/ratatui/ratatui-textarea ; https://github.com/rhysd/tui-textarea ; https://github.com/preiter93/edtui ; https://docs.rs/rat-text ; https://docs.rs/tui-input/latest/tui_input/enum.InputRequest.html ; https://docs.rs/reedline ; https://github.com/joshka/tui-markdown ; https://github.com/celestia-island/ratatui-markdown ; https://github.com/Canop/termimad ; https://docs.rs/crate/syntect/latest/features ; https://docs.rs/crate/two-face/latest/features ; https://github.com/ratatui/ratatui-image (src/protocol/kitty.rs, src/picker.rs) ; https://github.com/tautropfli/terminal-colorsaurus ; https://github.com/a-kenji/tui-term ; https://github.com/ratatui/tui-widgets ; https://github.com/ratatui/templates ; https://ratatui.rs/concepts/application-patterns/ ; https://ratatui.rs/showcase/third-party-widgets/ ; https://docs.rs/ratatui/latest/ratatui/backend/struct.TestBackend.html ; https://github.com/openai/codex/tree/main/codex-rs/tui (Cargo.toml, insert_history.rs, custom_terminal.rs, tui.rs, tui/keyboard_modes.rs, tui/scrollback.rs, markdown_stream.rs, diff_render.rs, terminal_probe.rs, notifications/osc9.rs, pager_overlay.rs, bottom_pane/textarea.rs) ; https://deepwiki.com/openai/codex ; https://github.com/vadimdemedes/ink ; https://github.com/charmbracelet/glamour ; https://github.com/Dicklesworthstone/charmed_rust ; https://github.com/whit3rabbit/bubbletea-rs ; https://pypi.org/pypi/prompt_toolkit/json ; https://registry.npmjs.org/ink/latest
