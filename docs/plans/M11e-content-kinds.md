# M11e — Content kinds and the deep items

## Goal

The tape holds everything `docs/design/tui.md` §5 lists, each with its degrade: markdown with tables and highlighted code, diffs with word-level emphasis, images where the terminal can show them, a pager for the latest long output, `@` completion for paths, a truecolor palette once the terminal's background is known, a rewind picker on `esc esc`, and reasoning that opens on request. Each item is its own brick with its own snapshot and tmux scene; none changes the sdk.

## Bricks, in build order (owner)

1. **Markdown tables** (worker) — `markdown.rs` renders GFM tables as ruled `Table` nodes (the M11d renderer), right-aligning numeric columns; over-wide tables scroll in the pager.
2. **Highlighting** (worker) — `syntect` (`default-fancy`, pure Rust) + `two-face` syntaxes; a theme that maps scopes to the eight ANSI colours (`structure` for keywords, `dim` for comments, `text` for the rest — no rainbow); applied to fenced code and to `View::Code`; cached per block by hash so a streaming block re-highlights only its last line.
3. **Word-level diffs** (worker) — `similar` (already in) at the word level inside changed line pairs; emphasis by `bold` on the changed words, colour stays by column; applies to `View::Diff` and permission previews.
4. **Pager overlay** (worker) — `ctrl+o` opens the latest tool output, `Code` or `Diff` block in the alternate screen: `j/k`, `pgup/pgdn`, `g/G`, `/` search, `esc`; the tape is untouched.
5. **Images** (worker) — `ratatui-image`: kitty and iTerm2 inline, sixel where probed, half-block cells otherwise; an `Asset` item draws a thumbnail (≤ 12 rows) in the tape and opens full-size in an overlay; the probe runs once at start with a 400 ms cap; tmux passthrough where kitty is behind it. Measured against the budget before it is accepted; the fallback is `[image: name]` + `open` in the terminal's viewer.
6. **`@` completion** (worker) — `@` in the composer opens a dropdown over the `ignore` walk of the cwd, fuzzy-ranked by `nucleo-matcher`, capped at 8 rows; `⏎` inserts the path; an image path is added to `Input::Text.attachments` so it reaches the model.
7. **Theme detection** (worker) — `terminal-colorsaurus` reads the background once; `theme` setting `terminal | light | dark`; the truecolor tables from the design doc §4 for light and dark; `NO_COLOR` wins over everything.
8. **Rewind picker** (worker) — `esc esc` on an empty composer opens an overlay listing turns newest first; `⏎` submits `/rewind <turn>`; requires the store's rewind command (ADR-0005) — if absent the picker is not offered and the plan says so.
9. **Reasoning** (worker) — `✻ thought for 3s` opens the reasoning text in the pager on `⏎` when the row is focused via `ctrl+o` history.

## Files

`crates/bingo-surface-tui/src/{markdown,transcript,composer,input,keys,theme,terminal}.rs`, new `highlight.rs`, `images.rs`, `complete.rs`, `overlay/{pager,rewind}.rs`, `Cargo.toml`, `scripts/budget.toml`, `deny.toml` if a licence needs listing.

## Dependencies (verify each on crates.io; `scripts/budget.sh` after each)

`syntect` (`default-fancy`), `two-face`, `ratatui-image` (+ `image` with only `png jpeg` features), `nucleo-matcher`, `terminal-colorsaurus`. Estimate ≈ +40 crates; the cap moves in this plan with the measured number, one line per crate in the Verified section.

## Exit criteria

- [ ] a markdown table renders ruled at 80 columns and overflows to the pager at 40
- [ ] highlighted Rust, Python, JSON and a diff fenced block each have a snapshot; a streaming block re-highlights in under 1 ms per delta (timed test)
- [ ] a word-level diff snapshot; the permission preview uses it
- [ ] the pager: open, search, close; the tape byte-identical after (pty)
- [ ] an image: kitty path asserted by the `Recorder`'s raw bytes; half-block fallback snapshot; probe under 400 ms with no answer
- [ ] `@Car` completes to `Cargo.toml`; `@shot.png` attaches
- [ ] the light truecolor palette snapshot and the dark one differ only in the token values; `NO_COLOR` snapshot has none
- [ ] `esc esc` lists turns and rewinds (or the plan records the missing command)
- [ ] every new dependency named in the Verified section with its crate count

## Non-goals

A themes gallery. Syntax highlighting of the composer. Images in `--print`. Rendering PDFs.

## Risks

`image`'s size and build time — measured first, features minimal; if it breaks the budget or the warm-check budget, images stay a fallback in M11. `syntect` regex engine — `default-fancy` only; `onig` is banned in `deny.toml`. Terminal probes that hang — every probe has a cap and a default.
