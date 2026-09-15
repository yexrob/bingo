# M97 — A plugin is switched, not deleted

## Goal

User, 2026-09-15: a command "like `/modules`" that says which plugins are on
and which are off — the build's own and the external ones — and a settings
key that says the same. Today the only way a plugin is off is an unmet
requirement (`Registry::load`, `standing`), and the bridge spawns every
`plugin.json` directory it finds. ADR-0057 decides the shape: one kernel key
`enabledPlugins` (name → bool, objects merge field by field), the registry
reads it and cascades as it already does, the bridge reads it through the
registrar and never spawns an off process, the bridge's plugins are listed
through a `PluginSource`, and `/plugins` (alias `/modules`) plus
`bingo plugins` are the two faces. A switch takes effect at the next start
and says so. A plugin the binary cannot run without — a `store:` or a
`surface:` provider — ignores its switch with a notice and is refused a
written one; one function is the rule.

## Bricks, in build order

1. sdk `plugin.rs` — `PluginStatus { id, version, enabled, reason: Option<String>, from: String }`
   moved here from `core::host::registry` (`from` = `"built in"` or a
   source id); `PluginSource { fn id(&self) -> &str; async fn plugins(&self) -> Vec<PluginStatus> }`;
   `Contribution::Plugins(Arc<dyn PluginSource>)` with its `Debug` arm;
   `Registrar::switched_off(&self) -> &BTreeSet<String>` and a `new` that
   takes the set (the sdk's testing fakes and every `Registrar::new` caller
   follow). Unit test: a registrar built with a set answers it.
2. core `settings` — `enabledPlugins` joins `KERNEL_KEYS`;
   `KernelSettings.enabled_plugins: BTreeMap<String, bool>` read by
   `kernel_settings` through `typed`; `KernelSettings::switched_off()` is the
   set of names mapped to `false`. Merge tests: a project flips one name
   back on; a JSON `null` clears; a non-object or non-boolean is a `Type`
   error naming the layer.
3. core `plugins.rs` (new, kernel: the noun is the kernel's own) —
   `needed(manifest) -> Option<&'static str>`: the reason a plugin cannot be
   switched off, for one that provides `store:*` or `surface:*`; and
   `NEEDED` / `SWITCHED_OFF` notice codes. Table test.
4. core `registry.rs` — `standing(plugins, switched_off)`: an off plugin
   that `needed` does not protect is disabled with reason
   `switched off in the settings` before the fixpoint runs, so dependants
   cascade naming the requirement; a protected one stays with a
   `PLUGIN_NEEDED` notice collected on the registry and surfaced by
   `Host::notices`. `Registry::load` passes the set into every registrar and
   keeps `sources.plugins`. Tests: off → disabled with the reason; a
   requirer goes down naming what went missing; the store ignores its
   switch and the notice names it; a plugin off is neither registered nor
   started nor stopped.
5. core `commands/plugins.rs` — `/plugins` (`aliases: ["modules"]`,
   instant, family `session`). Bare: `View::Table` headers
   `plugin | version | state | reason | from`, the registry's statuses then
   every `sources.plugins` answer, in order. `enable <name>` /
   `disable <name>`: refuse a name nobody lists (`INVALID_INPUT`, the message
   says `/plugins` lists them) and a needed one (`needed`'s reason); else
   read the user document, set `enabledPlugins.<name>`, write, answer
   `"<name> is off at the next start."` / on. Parse tests; a host test that
   the table shows a cascaded plugin's reason; a host test that `disable`
   writes the user layer and the reply names the next start.
6. bridge — `Manager::new` takes the set; `discovery::discover` is unchanged,
   `Manager::start` skips a name in the set without spawning and remembers it
   as `PluginStatus { enabled: false, reason: "switched off in the settings" }`;
   `PluginPlugins` in `source.rs` contributes `Contribution::Plugins`,
   answering every discovered name with its live state (`enabled` = a bridge
   that connected; a dead one's reason is its last notice text). Make
   `discovery` `pub` for brick 7. Tests on the manager with a fake dir.
7. bin `plugins.rs` (new) — `bingo plugins list|enable|disable <name>`,
   before any host (`before_any_host`). `list`: compose `plugins(false)` (the
   demo when the setting says so), read the layers, run `standing` with the
   switches, then `discovery::discover` without a spawn; one line per
   plugin, `<name>\t<version>\t<on|off>\t<reason>`, built-ins then
   external. `enable|disable`: the same refusals as brick 5 (the composition
   and the discovery are the "lists"), the same write. Stdout carries the
   answer and nothing else.
8. Black-box `tests/cli/plugins.rs`: `bingo plugins disable bingo.tools.web`
   writes the user layer and `list` shows `off`; a run with the fake provider
   then has no `WebFetch` in its tool list (the fake's request record) and
   the `/plugins` table (rpc or `--print` with `Input::Action`) shows the
   reason; `disable bingo.store.jsonl` is refused with exit 1 and the
   reason on stderr; a seeded `enabledPlugins` for `wordcount` keeps the
   example plugin's tool out of the model's tools and the table shows it
   off (skipped without `python3`, as `plugin_rpc.rs` does).
9. ADR-0003 §2 amended to seven keys; ADR-0015 §Consequences gains one line
   (a switched-off name is never spawned).

## Files

- `crates/bingo-sdk/src/{plugin.rs,testing.rs,lib.rs}`
- `crates/bingo-core/src/{settings.rs,settings/merge.rs,plugins.rs,lib.rs,host.rs,host/registry.rs,commands/mod.rs,commands/plugins.rs}`
- `crates/bingo-plugin-rpc/src/{lib.rs,manager.rs,source.rs}`
- `crates/bingo/src/{main.rs,plugins.rs}`, `crates/bingo/tests/cli/{main.rs,plugins.rs}`
- `docs/adr/{0003-settings.md,0015-plugin-rpc.md}`

## Exit criteria

- [ ] `enabledPlugins` merges field by field; a higher layer turns one name
      back on; `null` clears; a wrong type names the layer.
- [ ] a switched-off plugin is disabled with its reason, cascades to its
      dependants, and is neither registered, started nor stopped.
- [ ] a `store:`/`surface:` provider ignores its switch with `PLUGIN_NEEDED`.
- [ ] `/plugins` and `/modules` draw one table of built-ins and the bridge's;
      `enable|disable` write the user layer and say *at the next start*.
- [ ] `bingo plugins list|enable|disable` black-box: exit codes, stdout
      purity, the refusal, the wordcount case.
- [ ] `cargo check -p bingo --all-targets --target x86_64-pc-windows-msvc`
      (the bin gained a file that reads directories).
- [ ] every gate green (fmt, check, clippy, test, discipline, budget: no new
      dependency, count unchanged).

## Non-goals

- A live switch: nothing is unregistered mid-run; the reply says so.
- MCP servers: `/mcp enable|disable` and `disabledMcpServers` stay theirs.
- Removing `experience.enabled` or `demoUi`: finer switches keep their
  owners; the coarse one wins by not registering.
- A warning for a name in `enabledPlugins` that matches nothing: the two
  listings are where a person looks; a name the bridge has not installed on
  this machine is not a typo.

## Risks

- R-registrar: every `Registrar::new` caller — the sdk's testing fakes, the
  bridge's tests, core's tests — takes one more argument. Mechanical.
- R-order: `PluginStatus` moving crates touches `core::lib.rs` re-exports
  and `host/catalog.rs`, `host/tests.rs`. Keep the names.
- R-parallel: M98 rewrites `settings.rs`'s read/write half at the same time;
  this milestone touches only `KERNEL_KEYS`, `KernelSettings` and
  `merge.rs`. The bin's `main.rs` gains a subcommand arm; M98 gains a call
  at the top of `run`. Both merge on `dev`.
