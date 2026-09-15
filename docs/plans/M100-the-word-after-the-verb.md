# M100 — The word after the verb

## Goal

User, 2026-09-15: "`/plugins enable` 后面怎么不会给出全部的 plugins". Typing
`/plugins enable ` in the TUI offers nothing, because `ArgSpec` describes
a command's first word only and `commands::arguments` returns nothing once
the partial has a space in it; `/mcp <verb> <server>` has the same hole.
`CatalogKind::Plugins` exists but the TUI never fetches it, and the
catalogue lists the registry's plugins without the bridge's. ADR-0008 §6a
decides: `ArgSpec::Words` gains `then: Option<Box<ArgSpec>>` (serde
default, so the wire is unchanged for every existing spec), a surface
completes the second word from it, and the plugins catalogue answers the
sources too.

## Bricks, in build order

1. sdk `command.rs` — `ArgSpec::Words { values: Vec<String>, #[serde(default, skip_serializing_if = "Option::is_none")] then: Option<Box<ArgSpec>> }`.
   Every `ArgSpec::Words { values }` construction and pattern in the tree
   gains `then: None` / `..`. Schema: `BINGO_UPDATE_SCHEMA=1 cargo test -p bingo-plugin-rpc`
   and `-p bingo-surface-rpc`, and the drift tests pass. Serde test: a
   `Words` without `then` round-trips byte-identical to before.
2. core `host/catalog.rs` — `plugins` becomes async and appends every
   `sources.plugins` answer after the registry's, in order; `meta` gains
   `from`. Test with a fake `PluginSource`.
3. core `commands/plugins.rs` — the spec's `then` is
   `ArgSpec::Catalog { source: "plugins" }`. mcp `command.rs` — `then` is
   `ArgSpec::Words { values: <the manager's names>, then: None }`, so a
   server completes without a catalogue kind of its own. Spec tests.
4. tui `run.rs` — `CATALOGUES` gains `("plugins", CatalogKind::Plugins)`.
   tui `commands.rs` — `arguments` walks the words: the first word is
   ranked against `values` as today; with a space after a word that is
   exactly one of `values`, the second partial is ranked against `then`
   (a catalogue's ids or its words); a `then` of `None`, a word that is
   not one of `values`, or a third word offers nothing. The suggestion's
   `value` is `/{name} {word} {id}`. Tests: `/plugins enable bin` offers
   the plugin ids that match; `/plugins enabl` still offers the verb;
   `/plugins enable x y` offers nothing; `/mcp reconnect fi` offers the
   server; a spec without `then` behaves exactly as before.
5. A TUI `TestBackend` test that the `/` menu draws the plugin ids under
   `/plugins disable ` (the composer's completion popup), if one exists
   for `/model`; else the unit tests of brick 4 carry it.

## Files

- `crates/bingo-sdk/src/command.rs`, `schema/{plugin,rpc}.json`
- `crates/bingo-core/src/host/catalog.rs`, `crates/bingo-core/src/commands/plugins.rs`
- `crates/bingo-mcp/src/command.rs`
- `crates/bingo-surface-tui/src/{run.rs,commands.rs}` and any `ArgSpec::Words` site the compiler names
- `docs/adr/0008-commands.md` (§6a, already written)

## Exit criteria

- [ ] `/plugins enable ` and `/plugins disable ` complete the plugin
      names, built-in and external, in the TUI.
- [ ] `/mcp <verb> ` completes the configured server names.
- [ ] a `Words` spec without `then` serialises exactly as before; both
      schemas regenerated and drift-clean.
- [ ] every gate green (fmt, check, clippy, test, discipline, budget: no
      new dependency).

## Non-goals

- A third word, or per-verb `then` (every verb of one command takes the
  same kind of name today).
- Completion in the rpc or channels surfaces: they carry the spec to a
  client and complete nothing themselves.

## Risks

- R-sites: `ArgSpec::Words` is constructed in several plugins and doubles;
  the compiler lists them, and `..` in patterns keeps the diff small.
- R-catalogue: the plugins catalogue is fetched at the TUI's start, after
  the bridge's discovery in `Plugin::start`, so the external names are
  there; nothing emits `CatalogChanged { Plugins }` later, and nothing
  needs to yet.
