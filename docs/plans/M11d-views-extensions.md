# M11d — Views and extensions: UI as data

## Goal

A plugin puts something rich, live or interactive on the screen by publishing a `View` (ADR-0013) and never learns what a terminal is. A tool's output carries a `Diff` or a `Table` a person reads in columns while the model reads text; a plugin's journaled panel comes back after `--continue`; a build's progress bar updates ten times a second without touching the journal; a board's `[ Approve ]` fires a command. The TUI draws every node once; `--print` prints its fold; the RPC wire carries it verbatim. A demo plugin proves all three lanes end to end.

## Bricks, in build order (owner)

1. **sdk** (kernel, once) — `View` gains `Markdown`, `Code{lang, text}`, `Diff{unified}`, `KeyValue{rows}`, `Progress{value, total, label}`, `Badge{text, tone}`, `Tree{nodes}`, `Stack`, `Columns`, `Panel{title, child}`, `Actions{items}` with `ActionItem{label, action: Action, key: Option<char>}`; `View::text()` folds every node; `ToolOutput.display: Option<View>` replaces `Display`; `Event::Signal{plugin, kind, payload}` (ephemeral); `SessionState.signals` folded latest-per-kind, `Null` removes; `HostApi::signal`; `bingo_sdk::testing::NoHost` gains it. A fixture test pins the JSON of every node and the fold of each.
2. **Kernel** (kernel) — `Host::signal` publishes without journaling (`is_durable` false); the reducer test for `signals`; `session/signal` on the wire, `schema/rpc.json` regenerated; `bingo-tool-fs` writes `View::Diff` and `View::Text` where it wrote `Display`; the print surface prints `display.text()` under a tool row when present.
3. **`view.rs` → `views/`** (worker) — one renderer per node in `crates/bingo-surface-tui/src/views/{text,markdown,code,diff,list,table,keyvalue,progress,badge,tree,stack,columns,panel,actions}.rs`, each `fn lines(node, width, theme) -> Vec<Line>`, composed by `views::render(view, width)`; `Columns` splits the width evenly and stacks below 60 cells; `Panel` is a title row and an indented child; `block.rs` and `panel.rs` become callers of it.
4. **Lanes in the frame** (worker) — a block (`ToolOutput.display`) is drawn under its tool row, folded like output (three rows, `… +N`), fully in a sheet; a panel (`Extension` that parses as a `View`) is drawn in `ctrl+t`; pinning is the TUI's, not the plugin's — `⏎` on a panel row in the panel sheet pins it into the rail (or under the running rows below 120 columns), remembered per session in `ui`; a signal is a rail card that updates in place, newest last, at most 8 rows per plugin, the rest folded.
5. **Actions** (worker) — an `Actions` row draws `[ 1 Approve ] [ 2 Next ]`; when the card holding it has focus (`tab` cycles rail cards; a click focuses one), digits fire `Effect::Submit(Input::Action)`; the row shows `…` until the ack; a `Rejected` ack is a notice.
6. **Demo plugin** (worker) — `crates/bingo-demo-ui` (plugin id `bingo.demo.ui`, off by default, `--demo-ui` or a setting enables it): a `DemoProgress` tool that signals a `Progress` every 200 ms for 3 s and returns a `Code` display; a `/board` command that extends a `Panel{Table + Actions}` and a `board.tick` command the action fires; used by the tests and the tmux drive, and the reference for plugin authors.
7. **Black-box** (kernel) — `--print --output-format json` carries `display` verbatim; RPC sees `Signal` frames and not in `history`; `--continue` shows the board and not the progress.

## Files

`crates/bingo-sdk/src/{command,event,state,host,testing}.rs`, `crates/bingo-core/src/{host,session,journal}.rs`, `crates/bingo-surface-rpc/src/{methods,server,client}.rs`, `schema/rpc.json`, `crates/bingo-tool-fs/src/{edit,write}.rs`, `crates/bingo-surface-print/src/render.rs`, `crates/bingo-surface-tui/src/views/*`, `src/{rail,panel,block,input,keys}.rs`, new `crates/bingo-demo-ui/**`, `crates/bingo/src/main.rs`, `crates/bingo/tests/{cli/views.rs,views.rs}`.

## Dependencies

None new for the vocabulary. `pulldown-cmark` is already in; `Code` highlighting is M11e's, `Code` renders plain here.

## Exit criteria

- [ ] every node has a JSON fixture, a fold test, and one `TestBackend` snapshot at 80 columns; `Columns` also at 50
- [ ] `ToolOutput.display` round-trips through the wire and the journal; `Display` is gone from the workspace
- [ ] a `Signal` is on the live stream and absent from the journal and from `history`; the reducer keeps the latest per kind and drops on `Null`
- [ ] the demo: the progress bar moves in tmux and is gone after `--continue`; the board is back after `--continue`; `1` on the focused board fires `board.tick` and the table changes
- [ ] a plugin author's example in `docs/design/tui.md` §8 compiles as a doc test against the demo crate
- [ ] sdk changed once; ADR-0013 lists what it touched; `check_discipline.sh` unchanged and green

## Non-goals

Forms as an interaction. Node-level styling (colour, alignment) chosen by plugins — the tone of a `Badge` is the one styling hook. Images in views (`Asset` items are images). Mouse targets.

## Risks

Vocabulary creep — a node enters only with its fold and its snapshot; ten nodes is the cap for M11. A signal storm from a careless plugin — the reducer coalesces; a plugin publishing faster than 20 Hz gets a `Lagged` and a notice naming it.
