# 0015 — The cross-process plugin bridge

## Context

`Contribution` was written as "one enum so the in-process path and a future out-of-process bridge share one representation" (sdk `plugin.rs`), and the plan gated that bridge on the traits being proven by three in-process consumers — done since M11. Two out-of-process instances already exist, each on a foreign contract: MCP (tools only, a protocol with its own runtime and no notion of our commands, previews or views) and shell hooks (lifecycle shell-outs on Claude Code's contract). Neither lets a third party ship a bingo-native contribution — a `/command` with completion and an instant flag, a tool whose `ToolOutput.display` is a `View` — without writing Rust in this repository. The exit the plan names: a third party writes one Tool and one Command in a language that is not Rust, and both run.

## Decision

1. **One plugin, `bingo-plugin-rpc`**, hosting external plugin processes. Discovery: a `plugin.json` per directory under `<config_dir>/plugins/<name>/` and `.bingo/plugins/<name>/` from the project (the project's wins a name). Manifest: `{name, version, entry: {command, args?, env?}, config?}`; `${PLUGIN_ROOT}` in `command`, `args` and `env` values resolves to the manifest's directory.
2. **The wire is the SDK's own types as JSON.** JSON-RPC 2.0, NDJSON over stdio. `ToolSpec`, `CommandSpec`, `ToolOutput`, `CommandOutcome`, `Completion`, `View` are already `Serialize + JsonSchema`; the bridge adds envelopes, never shapes. `schema/plugin.json` is generated from those types (schemars, the ADR-0007 pattern), committed, drift-tested — it is the document a non-Rust author writes against.
3. **Handshake registers, calls execute.** At `Plugin::start` the host spawns each entry and sends `initialize {protocol, plugin_root, config, env}`; the process answers `{name, version, tools: [ToolSpec], commands: [CommandSpec]}`. The host registers one `ToolSource` and one `CommandSource` answering from that cache (ADR-0009 §1: answering with nothing is never wrong). Calls: `tool/call {call_id, name, input, cwd, session, turn}` → `{output: ToolOutput}`; `command/run {name, args, cwd, session}` → `{outcome: CommandOutcome}`; `command/complete {name, partial, cwd}` → `{completions}`. Notifications: plugin→kernel `tool/progress {call_id, tail}`; kernel→plugin `tool/cancel {call_id}`. A JSON-RPC error is a `ToolError::Failed` / `KernelError`, named for the plugin.
4. **Everything a process says about itself is a claim.** Bridge tools wear `ToolTraits::default()` — untrusted, fail closed, the gate asks (the MCP stance, ADR-0009 §2). Tool names are `plugin__<name>__<tool>` so the permission grammar reads them; command names collide by the registry's existing later-duplicate-dropped rule. A `CommandOutcome::Prompt` submits with the command's own origin, like any command.
5. **A process is allowed to die.** stderr goes to `<data_dir>/logs/plugin-<name>.log`, never the terminal; a dead process makes its sources answer nothing and surfaces one notice; the next source read respawns it with backoff. No health checks, no supervisor.
6. **v1 mirrors Tool and Command only.** Hooks, contributors, providers, surfaces and stores stay in-process; the `Contribution` enum is the map of what a later version may cross, each gated on a real need. MCP remains the road for tool-only servers speaking MCP; the bridge is for bingo-native plugins. (Superseded as of PROTOCOL 3: contributors and compaction strategies cross by ADR-0030 (M26) and providers by the same ADR (M27) — a stream crossing as `provider/stream` plus `provider/delta` notifications keyed by call; services and hooks follow (M28–M29, ADR-0031/0032), and the fixed method count of §3 became a pin derived from the committed schema.)
7. **The exit criterion ships in the repo**: `examples/plugins/wordcount/` — Python 3, stdlib only — one tool and one command, driven end-to-end by a black-box test through the real binary (skipped where `python3` is absent).

## Consequences

- New crate `bingo-plugin-rpc` (plugin tier). No new dependencies: framing is serde_json over lines, the process is `tokio::process`, both in the tree.
- The bridge's NDJSON framing consciously repeats the ~100-line envelope loop the RPC surface has; sharing it would make a plugin import a plugin. Two copies of a codec are mechanism, not a second representation of a fact; a third copy forces the module into the sdk.
- The wire schema is versioned by `protocol` in `initialize`; an unknown major refuses the handshake with a notice rather than guessing.
- A bridge plugin's config slice lives under the host plugin's claim (`plugins.<name>` inside `bingo.plugin-rpc`'s key), typed by the manifest's `config` schema; the loader validates it like any other slice.

## Supersedes

—
