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

- [x] `/plugins enable ` and `/plugins disable ` complete the plugin
      names, built-in and external, in the TUI.
- [x] `/mcp <verb> ` completes the configured server names.
- [x] a `Words` spec without `then` serialises exactly as before; both
      schemas regenerated and drift-clean.
- [x] every gate green (fmt, check, clippy, test, discipline, budget: no
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

## Verified (2026-09-15)

All four exit criteria ticked.

The first word is unchanged and the second is read from the verb's own
`then`: `/plugins enable bin` offers the two ids that match in catalogue
order, `/plugins disable ` the whole listing, `/plugins enabl` the verb
still, and `/plugins enable x y`, `/plugins toggle bin` and a spec whose
`then` is `None` all offer nothing. A `TestBackend` snapshot draws the
three plugin ids under `/plugins disable `.

`/mcp` is covered in its two halves rather than end to end: the spec
carries the manager's configured names (`bingo-mcp`), and the dropdown
completes from a `then` of words (`bingo-surface-tui`). Nothing in the
tree wires the mcp plugin into the TUI, so no test drives that seam.

One representation of the listing: `crate::plugins::listing` is what
`/plugins` draws its table from and what the catalogue answers with, so
the name a person completes cannot drift from the name the table shows.

`schema/rpc.json` did not change — `ArgSpec` is reached only through the
plugin document — and `schema/plugin.json` gained `then` as an optional
`$ref` to `ArgSpec` itself, `required` untouched. Both drift tests pass
without `BINGO_UPDATE_SCHEMA`. The `acp_bridge` token test did not hang.

```
$ cargo fmt --all -- --check                                    exit 0

$ cargo check --workspace --all-targets --locked
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.60s

$ cargo clippy --workspace --all-targets --locked -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 43.77s

$ cargo test --workspace --locked   4795 passed; 0 failed; 2 ignored; 91 binaries
test commands::tests::the_word_after_a_verb_comes_from_the_catalogue_the_verb_named ... ok
test commands::tests::a_verb_and_a_space_offers_every_name_the_catalogue_holds ... ok
test commands::tests::a_half_typed_verb_is_still_a_verb_and_a_third_word_is_nobody_s ... ok
test commands::tests::a_verb_may_name_its_own_words_for_the_word_after_it ... ok
test commands::tests::a_second_word_is_not_completed_from_the_first_word_s_list ... ok
test commands::tests::a_catalogue_that_has_not_arrived_yet_offers_nothing ... ok
test view::tests::the_word_after_a_verb_is_drawn_under_the_verb ... ok
test command::tests::a_word_list_crosses_the_wire_as_its_kind_and_its_values ... ok
test command::tests::a_verb_carries_the_spec_of_the_word_after_it ... ok
test host::catalog::tests::the_plugins_catalogue_lists_the_sources_after_the_registry ... ok
test commands::plugins::tests::the_spec_says_both_verbs_and_the_catalogue_the_name_comes_from ... ok
test command::tests::the_word_after_a_verb_is_a_configured_server ... ok
test schema::tests::the_committed_schema_is_this_document ... ok

$ scripts/check_discipline.sh
dependency direction ok
kernel names no tool
cohesion ok
discipline ok

$ scripts/budget.sh
dependencies (unique, normal): 342 (max  342)
relink isolation: touching the TUI recompiled 0 crates for core (must be 0)
budget ok
```
