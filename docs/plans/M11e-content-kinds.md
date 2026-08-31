# M11e — Content kinds and the deep items

## Goal

The transcript holds everything `docs/design/tui.md` §5 lists, each with its degrade: markdown with tables and highlighted code, diffs with word-level emphasis, images where the terminal can show them, a pager sheet for any long block, `@` completion for paths, a truecolor palette once the terminal's background is known, a rewind picker on `esc esc`, and reasoning that opens on request. Each item is its own brick with its own snapshot and tmux scene; none changes the sdk.

## Bricks, in build order (owner)

1. **Markdown tables** (worker) — `markdown.rs` renders GFM tables as ruled `Table` nodes (the M11d renderer), right-aligning numeric columns; over-wide tables scroll in the pager.
2. **Highlighting** (worker) — `syntect` (pure Rust: `parsing` + `regex-fancy`, no themes — a scope maps onto §4's table, so `default-themes`/`plist-load`/`html` stay off) + `two-face` syntaxes; three inks and no rainbow (`mode` for keywords, `dim` for comments, `text` for the rest, and operators plain); applied to fenced code and to `View::Code`, which a fence now goes through; cached per block so a streaming block re-highlights only its last line.
3. **Word-level diffs** (worker) — `similar` (already in) at the word level inside changed line pairs; emphasis by `bold` on the changed words, colour stays by column; applies to `View::Diff` and permission previews.
4. **Pager sheet** (worker) — `⏎` on a focused block, or `ctrl+o` for the latest, opens it as a sheet: `j/k`, `pgup/pgdn`, `g/G`, `/` search, `esc`; the frame beneath is untouched.
5. **Images** (worker) — **refused on the budget.** `image` (png + jpeg only) and `ratatui-image` were measured at **+33 crates** against a cap that allowed ~25 (`image` +9, `ratatui-image` +24: its sixel quantiser drags in `icy_sixel`, `quantette`, `palette`, `moxcms`, `bitvec`, `wide`, `rand_chacha` and the rest). Images stay at design §5's own degrade: `ItemBody::Asset` draws `[label]` in the transcript. Nothing else in this plan depends on it, and the `@` completion still puts an image path into `Input::Text.attachments`.
6. **`@` completion** (worker) — `@` in the composer opens a dropdown over the `ignore` walk of the cwd, fuzzy-ranked by `nucleo-matcher`, capped at 8 rows; `⏎` inserts the path; an image path is added to `Input::Text.attachments` so it reaches the model.
7. **Background detection and palettes** (worker) — `terminal-colorsaurus` reads the background once; `theme` setting `terminal | light | dark`; the light truecolor set derived from §4's dark one (the `raised` tint one step from the read background); `NO_COLOR` wins over everything.
8. **Rewind picker** (worker) — `esc esc` on an empty composer opens a card listing turns newest first; `⏎` submits `/rewind <turn>`. **No `/rewind` command exists**: the kernel registers `model think compact login logout status` and the plugins `agents team room mcp permission tasks board`, none of them a rewind (ADR-0005's rewind is `Event::Rewound` in the reducer, never a command). So the picker is offered exactly when a `rewind` spec is in the session's catalogue — data-driven, so it lights up the day one lands and offers nothing that could not be done today. The card, its rows and its `⏎` are built and tested against an injected catalogue.
9. **Reasoning** (worker) — `⏎` on a focused `✻ thought for 3s` row opens the reasoning text in a sheet.

## Files

`crates/bingo-surface-tui/src/{markdown,transcript,preview,input,keys,theme,terminal,ui,view,lib}.rs` and `views/{mod,code}.rs`, new `highlight.rs`, `complete.rs`, `pager.rs`, `rewind.rs`, `Cargo.toml`, `scripts/budget.toml`, `scripts/tui-smoke.sh`, `deny.toml`. No `images.rs` (brick 5), and the two sheets are flat modules rather than a `sheets/` directory — a module owns a noun, and `pager` and `rewind` are two.

## Dependencies (verify each on crates.io; `scripts/budget.sh` after each)

`syntect`, `two-face`, `nucleo-matcher`, `terminal-colorsaurus`. `ratatui-image` and `image` were measured and refused. Measured total **+13**; `scripts/budget.toml` moves 269 → 282 with that number and a reason line.

## Exit criteria

- [x] a markdown table renders ruled at 80 columns and overflows to the pager at 40
- [x] highlighted Rust, Python, JSON and a diff fenced block each have a snapshot; a streaming block re-highlights in under 1 ms per delta (timed test)
- [x] a word-level diff snapshot; the permission preview uses it
- [x] the pager sheet: open, search, close; the frame beneath byte-identical after
- [~] an image: **not built** — the budget refused `image` + `ratatui-image` at +33 crates (brick 5). Images keep design §5's degrade.
- [x] `@Car` completes to `Cargo.toml`; `@shot.png` attaches
- [x] the light truecolor palette snapshot and the dark one differ only in the token values; `NO_COLOR` snapshot has none
- [x] `esc esc` lists turns and rewinds (the plan records the missing command: brick 8)
- [x] every new dependency named in the Verified section with its crate count

## Non-goals

A themes gallery. Syntax highlighting of the composer. Images in `--print`. Rendering PDFs.

## Risks

`image`'s size and build time — measured first, features minimal; if it breaks the budget or the warm-check budget, images stay a fallback in M11. `syntect` regex engine — `default-fancy` only; `onig` is banned in `deny.toml`. Terminal probes that hang — every probe has a cap and a default.

## Verified — 2026-08-31

Every gate green, by exit code, on `worktree-agent-a3d8ffbafe1d3ccc0`:

```
cargo fmt --all -- --check                                   0
cargo check --workspace --all-targets --locked               0
cargo clippy --workspace --all-targets --locked -- -D warnings  0
cargo test --workspace --locked                              0   (37 binaries ok; bingo-surface-tui 444 passed)
scripts/check_discipline.sh                                  0   discipline ok
scripts/budget.sh                                            0   dependencies (unique, normal): 282 (max 282)
cargo deny check                                             0   advisories ok, bans ok, licenses ok, sources ok
cargo build && scripts/tui-smoke.sh                          0   tui-smoke ok (14 scenes, two of them new)
```

### Dependencies

| crate | version | licence | measured delta | why |
|---|---|---|---|---|
| `syntect` | 5.3.0 | MIT | +9 together with `two-face` (`adler2`, `bincode`, `crc32fast`, `fancy-regex`, `flate2`, `miniz_oxide`, `simd-adler32` and the two) | the parser behind brick 2, on `fancy-regex`; `onig` and `onig_sys` are banned in `deny.toml` |
| `two-face` | 0.5.2+bat-0.26.1 | MIT OR Apache-2.0 | (in the +9 above) | bat's syntax set, so TOML and the rest of what a person opens are known |
| `nucleo-matcher` | 0.3.1 | MPL-2.0 | +1 | the `@` ranking; its default features are required — without `unicode-normalization` the crate does not compile |
| `terminal-colorsaurus` | 1.0.3 | MIT OR Apache-2.0 | +3 (`terminal-trx`, `xterm-color`) | OSC 10/11, once, capped at 400 ms |
| `ignore`, `similar` | in the tree | — | +0 | the `@` walk and the word diff |
| `image` + `ratatui-image` | 0.25 / 11.0 | — | **+33, refused** | over the ~25 the brief allowed; images keep §5's degrade |

269 → 282 in `scripts/budget.toml`, with the measured total and a reason line.

`deny.toml` gains two bans (`onig`, `onig_sys`) and one advisory ignore:
**RUSTSEC-2025-0141**, `bincode` 1.3.3 unmaintained. It is not a defect — the
authors' own announcement calls 1.3.3 complete — and it reaches the tree only
under syntect's dump loader, which deserializes a syntax set compiled into the
binary. Nothing from the network, a session or a person's file reaches it.

### What was not built

- **Brick 5, images.** Refused on the measured budget (above). `ItemBody::Asset`
  still draws `[label]`, and an `@`-mentioned image still becomes an attachment.
- **The rewind action.** No `/rewind` command exists anywhere in the tree
  (brick 8). The card, its rows and its `⏎` are built and tested; it is offered
  only when a `rewind` spec is in the session's catalogue, so today `esc esc` is
  silent.

### Observed, not caused

`cargo test --workspace` hung once, in
`run::tests::a_copied_selection_reaches_the_terminals_own_clipboard`, under an
exceptionally loaded machine; a `sample` put the thread in the tokio io driver
with no timer. The mechanism is M11a's harness, not this plan: `drive` enters
its `select!` **before the first paint**, so a key that arrives within the
double's 5 ms can be answered against `Painted::default()` — `ui.page()` is then
1 against a transcript of height 0, `Scroll::hold` keeps `Tail`, `reading()` is
false, `v` is typed as a letter, and `ctrl+d` on a non-empty composer never
exits. It did not reproduce in three later runs. The fix, if it recurs, is one
line: paint once before the loop's first `select!` — a key is answered against
what was drawn (§7) — and `an_idle_surface_draws_nothing_at_all`'s count moves
with it.
