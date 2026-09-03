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

- [x] An unknown-kind JSON value deserializes to `Custom` and folds
  to its `fold` field; absent, to `[<kind>]`.
- [x] Every pre-existing `View` fixture parses unchanged.
- [x] The TUI renders a `Custom` node as its fold text
  (`TestBackend`, 80x24).
- [x] The demo board shows one custom element beside the named ones.
- [x] Every AGENTS.md gate; no new dependency.

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

## Verified

2026-09-03, worker U, `m41-custom-view` off `dev` at 59f5378.

**The spelling.** A custom node rides the reserved `custom` tag, the
plugin's own word beside it: `{"kind": "custom", "customKind":
"demo.sparkline", "data": …, "fold": "3 5 8 13"}`. The tag stays a
word the sdk knows, so an explicit `Custom` reads back as itself
instead of being caught by the catch-all and nested a second time;
any other `kind` is caught with the whole object as `data` and its
`fold` (else `[<kind>]`) as the fold. Both spellings are read, only
the reserved one is written. ADR-0038 §1 records it.

**The fence.** `bingo_sdk__view__tests__views.snap` is untouched by
the change, and its sweep — byte-identical re-serialisation of every
known node, plus "a malformed known kind is still an error" — was run
against the unchanged `View` first (c84038f), then again after
(8983cbf). The known reading is the derived one, reached through
serde's `remote = "Self"` wrapper, so there is no second spelling of
the vocabulary; `KNOWN` is pinned to the variants by
`the_known_kinds_are_the_vocabulary`, which reads them out of
`schema_for!(View)`.

**The arms.** The compiler named one crate: `bingo-surface-tui`
(`views::marked`, and `named` in its tests). `--print`, the channels
and the ACP bridge already read every node through `fold()`.

```
$ cargo fmt --all -- --check                       # clean
$ cargo clippy --workspace --all-targets --locked -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 20.24s
$ cargo test --workspace --locked                  # 40 binaries, 0 failed
test result: ok. 582 passed; …  (bingo-surface-tui)
test result: ok. 154 passed; …  (bingo cli, black box)
test result: ok. 134 passed; …  (bingo-plugin-rpc)
test result: ok.  39 passed; …  (bingo-sdk)
$ scripts/check_discipline.sh
kernel names no tool / cohesion ok / discipline ok
$ scripts/budget.sh
dependencies (unique, normal): 310 (max 310)
relink isolation: touching the TUI recompiled 0 crates for core
budget ok
$ scripts/tui-smoke.sh
  a button on a pinned board fires its command and the table changes
tui-smoke ok
```

Two `bingo --test cli` ACP cases (`an_adapter_that_died_between_turns…`,
`a_row_that_keeps_its_servers_home…`) failed once each while a second
cargo ran beside them and passed alone and in every uncontended run;
they are adapter-process timing, and nothing here touches a process or
a clock. `target/debug` exceeds budget's soft size limit on this
machine — a warning, pre-existing, not a gate.

Beyond the plan: ADR-0038 gained the wire spelling, what the catch-all
is *not* for, and the panel lane's consequence — a journalled payload
carrying any `kind` string is now read as that node's fold rather than
the generic record dump (the lane already read `kind` that way for the
fourteen known words; this widens it to every word).
