# M89 — The measure is the person's

## Goal

User, 2026-09-10, with a screenshot of a 200-column terminal whose
transcript stopped at half the width: "bingo貌似不会把文字铺满整个终端 是有什么
限制吗". It is `wrap::MEASURE = 100` (design §7, "prose is read, not
scanned"), a cap every prose row wears whatever the terminal's width. The
user's decision: **the default fills the terminal; the cap stays as a
setting for whoever wants a narrower line** — "默认tui占满吧 不替用户做决定 可以
保留这个设置 让有需求的用户来设置".

After this milestone: prose wraps at the transcript's own width unless
`tui.measure` in the settings says a number, and then at `min(width, that)`.

## Bricks, in build order

1. `bingo-surface-tui/src/settings.rs` (new): the surface's claimed slice —
   `tui.measure: Option<u32>` beside the `update` key it already claims —
   one schema for both (`ConfigClaim.keys` gains `("tui", Replace)`); `0`
   and absent both mean the terminal's width; `measure(settings) ->
   Option<usize>` is the one reading. Schema fixture test.
2. `bingo-core/src/settings.rs` (or the bin's own reading, whichever the
   `update_wanted` path uses — follow it exactly): `tui_measure(layers)`,
   highest layer that names `tui` wins; `crates/bingo/src/main.rs` hands it
   over as `"measure"` in the surface args, as `pictureCacheDays` and
   `updateCheck` are.
3. `wrap::measure(width, cap: Option<usize>)`: the width, or the smaller of
   the two. `MEASURE` is deleted — the number is the person's now, and
   nothing in the crate names one. `Rows` carries `measure: Option<usize>`
   (read off the run's args once, held on `Ui`); every `wrap::measure`
   caller passes it (`transcript.rs`, `pager.rs`).
4. `docs/design/tui.md` §7's "Measure for prose `min(width, 100)`" becomes
   the new rule, dated, with the user's words.

## Files

- `bingo-surface-tui/src/{settings,wrap,transcript,pager,lib,ui,run}.rs`,
  `view.rs` where `Rows` is built.
- `bingo-core/src/settings.rs` or `bingo/src/main.rs` (reading + args).
- `docs/design/tui.md` §7.

## Exit criteria

- [ ] a 200-column `TestBackend` draw of a long paragraph fills the
      transcript width; the same with `measure: 100` wraps at 100
- [ ] `tui.measure` absent, `0`, `100` read as None, None, Some(100); a
      misspelled key under `tui` is reported as unknown like `update`'s
- [ ] snapshots at 80 and 120 columns unchanged (the transcript there is
      already narrower than 100)
- [ ] every gate green; PTY smoke; Windows check for `bingo-surface-tui`
      if it cross-builds here (aws-lc-sys may block it, as in M86/M87)

## Non-goals

- No per-kind measure (thoughts, results, prose alike take the one cap).
- Tables, rules and cards already take the full width; unchanged.
- No `/measure` command; a setting is enough for a value set once.

## Risks

- A row measured at 200 cells and a markdown renderer written for ≤100 may
  differ in how they place a long code span; the TestBackend draw at 200
  is the check.
