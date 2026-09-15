# 0057 — A plugin is switched, not deleted

## Context

Every plugin this build ships is registered on every start (`crates/bingo/src/main.rs::plugins`), and the only way a plugin is ever off is an unmet requirement (`Registry::load`, the `standing` fixpoint) or, for the demo, a flag. A person who does not want the web tools, the schedule, or an external `plugin.json` process on this machine has no way to say so: the bridge spawns every directory it finds, and the settings have no word for "off". `disabledMcpServers` and `/mcp enable|disable` exist for MCP servers, `experience.enabled` for one plugin, each spelled by its owner. The user asked (2026-09-15) for one place — a command and a settings key — that switches any plugin, built in or external.

The kernel door this opens: an external process is a plugin the kernel has never seen — it is the bridge's after I/O (ADR-0009 §1, ADR-0015 §3). For one listing and one switch to cover both, the bridge must read the kernel's switches and the kernel must list the bridge's plugins. Refusing either door would force the bridge to keep a second list of what is off, which is the second representation the ratchet forbids.

## Decision

1. **One kernel key, `enabledPlugins`**, the seventh (ADR-0003 §2): an object from a plugin's name to a boolean. A built-in plugin's name is its manifest id (`bingo.tools.web`); an external plugin's is its directory name (`wordcount`), so `bingo.*` is reserved for the build's own. Objects merge field by field (ADR-0003 §3), so a project turns one plugin back on that a person turned off, and a `null` in a JSON layer clears the word below it. Absent is `true`. The spelling is Claude Code's `enabledPlugins`, a map to booleans, because a person coming from there writes it unprompted.
2. **The registry reads it.** `standing` takes the switches beside the manifests: a plugin switched off is disabled with the reason `switched off in the settings`, and the fixpoint cascades as it always has — whoever required what it provided goes down naming the requirement. A switch is never fatal.
3. **What the binary cannot run without stays on.** A plugin that provides a `store:` or a `surface:` capability ignores its switch with a `PLUGIN_NEEDED` notice, and the command and the subcommand refuse to write one; one function, `bingo_core::plugins::needed`, is the rule for all three. Everything else is the person's to switch, the permission policy and every provider included: the gate without a policy asks (fail closed), and a run without a provider says so.
4. **The bridge reads the switches through the registrar** — `Registrar::switched_off()` — and never spawns a process whose name is off: a switched-off plugin is a directory that is read and a process that is not started. The kernel hands every plugin the set; a plugin that ignores it loses nothing.
5. **The bridge's plugins are listed through a source.** `bingo_sdk::PluginSource { id, plugins() -> Vec<PluginStatus> }`, `Contribution::Plugins`, read where a listing is drawn; `PluginStatus { id, version, enabled, reason, from }` moves from the registry into the sdk, `from` naming the source (`built in`, or the source's id). Answering with nothing before discovery is never wrong (ADR-0009 §1).
6. **`/plugins` (alias `/modules`) is a kernel built-in** (ADR-0008 §4), instant. Bare, it draws one `View::Table` — plugin, version, state, reason, from — of the registry's statuses followed by every source's. `enable <name>` and `disable <name>` write the **user** layer (ADR-0003 §5) and answer with the change and the words *at the next start*: a plugin registers at boot, and a switch that pretended to take a running plugin's tools, hooks, store or surface away mid-session would be a lie in one direction and a half-truth in the other. A name nobody lists is refused; a needed plugin is refused by §3. **`bingo plugins list|enable|disable`** is the headless twin (ADR-0050 §4's pattern), before any host: `list` prints what the next start will do, one line per plugin — the same `standing` verdicts on the same composition, and the bridge's discovery without a spawn.

## Consequences

- sdk: `PluginStatus`, `PluginSource`, `Contribution::Plugins`, `Registrar::switched_off`. core: `KernelSettings.enabled_plugins`, `plugins::needed`, `standing` takes switches, `commands/plugins.rs`. bridge: skips a switched-off directory, contributes a `PluginSource`. bin: `plugins` subcommand. No new dependency.
- `/mcp enable|disable` and `disabledMcpServers` stay: an MCP server is a server this machine dials, not a plugin. `experience.enabled` stays as the plugin's own finer switch; `enabledPlugins["bingo.experience"] = false` is the coarser one and wins by not registering it at all.
- A switch takes effect at the next start. A live switch is a later ADR, if a case for it appears: it would need every contribution to remember which plugin made it.
- `bingo.provider.fake` is off unless `BINGO_FAKE_SCRIPT` names a script and never appears in a listing that did not set it; `bingo.demo-ui` keeps its flag and its setting and is listed only when composed.

## Supersedes

—
