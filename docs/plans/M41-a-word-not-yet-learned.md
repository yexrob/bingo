# M41 — A word not yet learned

## Goal

ADR-0038 built: `View::Custom { kind, data, fold }` — a plugin can put
up an element the sdk has no word for, every surface that has not
learned it reads the fold, and an unknown `kind` deserializes into
`Custom` instead of failing, so a newer speaker never breaks an older
reader again.

## Bricks, in build order

1. **The word** (`bingo-sdk/src/view.rs`): the `Custom` variant;
   `fold()` returns its fold; a hand-written `Deserialize` for `View`
   that tries the known kinds and catches the rest into `Custom`
   (whole raw object as `data`, its `fold` field else `[<kind>]`).
   Serialize stays derived for known kinds; `Custom` serializes as
   `{kind, data, fold}` flat enough to round-trip. Fixture tests: a
   custom node round-trips; an unknown-kind object from "the future"
   lands as `Custom` with the right fold; every existing fixture
   still parses byte-identically.
2. **The arms**: every crate that matches on `View` gains its
   `Custom` arm rendering the fold — the compiler lists them (TUI,
   print, channels, wherever else). No surface learns any kind in
   this milestone; the TUI arm is fold-only.
3. **The schema**: regenerate the committed plugin-rpc schema if
   `View` rides it (`bingo-plugin-rpc::schema` test says).
4. **The example**: `bingo-demo-ui` puts one `Custom` on its board
   (e.g. `demo.sparkline`, data: a few numbers, fold: the numbers as
   text) — the crate exists to be read, so the porch is shown next to
   the house. Snapshot re-aim if the board's snapshot moves.
5. **The proof**: sdk fixtures (brick 1); a TUI `TestBackend` case —
   a `Custom` panel renders its fold at 80x24; a black-box or unit
   case proving an unknown kind arriving over the plugin-rpc wire is
   folded, not an error.

## Files

`bingo-sdk/src/view.rs`; every `View` match the compiler names
(`bingo-surface-tui`, `bingo-surface-print`, `bingo-channels`?);
`bingo-plugin-rpc/schema/plugin.json` if it moves;
`bingo-demo-ui/src/board.rs` + snapshots; `docs/design/tui.md` §8 if
the example belongs there too.

## Exit criteria

- [ ] An unknown-kind JSON value deserializes to `Custom` and folds
  to its `fold` field; absent, to `[<kind>]`.
- [ ] Every pre-existing `View` fixture parses unchanged.
- [ ] The TUI renders a `Custom` node as its fold text
  (`TestBackend`, 80x24).
- [ ] The demo board shows one custom element beside the named ones.
- [ ] Every AGENTS.md gate; no new dependency.

## Non-goals

Any surface learning any kind richly (that is each surface's own
later work). A kind registry. Changing any existing variant.

## Risks

- The hand-written `Deserialize` must not change how known kinds
  parse: the byte-identical fixture sweep is the fence.
- `Data`-flat vs nested spelling of `Custom` on the wire: brick 1
  owns it; the round-trip fixture is the contract.
- M40 runs in parallel in `bingo-provider-acp` — no shared files; if
  the plugin-rpc schema moves in both, the integrator resolves.
