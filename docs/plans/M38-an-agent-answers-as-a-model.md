# M38 — An agent answers as a model

## Goal

ADR-0035 built: a `bingo-provider-acp` plugin whose every configured
ACP adapter (`acp/claude`, `acp/codex`, …) is a `Provider` instance.
Types from `agent-client-protocol-schema`; the ndjson JSON-RPC client
loop is ours, tokio, `Send`. One bingo session is one ACP session
(Zed's shape): `ModelRequest` gains `session: Option<SessionId>`, the
instance maps it to the agent's `sessionId` journaled once as an
extension, and restore climbs resume → load (replay swallowed) →
fresh session + transcript file. The adapter children are kept the
way plugin processes are kept — spawned lazily, `initialize`d once,
restarted on death, killed at `stop()`. The agent's tool calls stream
first-class wearing `acp.external: true`; permissions are the
adapter's own, configured on its row, and a stray
`session/request_permission` is refused closed. The kernel changes by
one field, no door.

## Bricks, in build order

0. **The request names its session.** `bingo-sdk` `ModelRequest`
   gains `session: Option<SessionId>` (serde default, absent
   serializes to nothing, so recorded fixtures stand); core stamps it
   at the one place requests are built. A test on the stamping; every
   provider compiles untouched. Nothing else in sdk or core moves.
1. **The contract.** `agent-client-protocol-schema = "=1.5.0"`
   (budget 302 → 307, the ADR line; `cargo deny check`). `method.rs`:
   a pure table, type ↔ method string, for the ~15 methods used both
   ways. Fixture tests: each message round-trips serde against a
   recorded JSON body — the contract before any transport.
2. **The loop.** `wire.rs`, `connection.rs`: newline-framed JSON-RPC
   over `AsyncRead`/`AsyncWrite` — id counter, pending map, responses
   matched by id, incoming requests routed to a handler, notifications
   to a stream. Generic over the transport: unit tests drive it over
   an in-memory duplex. `child.rs`: spawn from `{command, args, env}`,
   kill-on-drop by process group / job object (tool-bash's `Group`
   lesson, cfg both platforms in the same change).
3. **The fake agent.** A test-helper binary in the crate (a scripted
   ACP agent on stdin/stdout, script from an env path like
   `BINGO_FAKE_SCRIPT`), found via `CARGO_BIN_EXE_…` — rust, so CI
   needs no node. It advertises whatever capabilities its script says:
   the same fake drives the loop, the permission refusal and the
   black-box.
4. **The mapping, pure.** `events.rs`: `SessionUpdate → Vec<ModelEvent>`
   — message chunk → `Text*`, thought chunk → `Reasoning*`,
   `tool_call`/`tool_call_update` → `ToolInput*`/`ToolCall` plus the
   synthetic `ToolResult`, `acp.external: true` in `provider_options`;
   `stop_reason` → `Finish`; usage folded from `usage_update` and the
   end-turn field when present, zero otherwise. Fixture tests per
   update kind.
5. **The session map and the ladder.** `pool.rs`: the kept children
   (lazy spawn, `initialize` once with capabilities cached, respawn
   on death with a notice, `stop()` kills all, kill-on-drop the
   backstop) and the map bingo session → agent session. `session/new`
   journals the extension (`bingo.acp`, `session:<instance>`) through
   the plugin's host handle; restore climbs `session/resume`, else
   `session/load` behind a swallowing flag, else a fresh session
   whose first prompt names the file `transcript.rs` renders from the
   request's fold at that moment. Tests against the fake advertising
   each capability set; one pins that a load replay journals nothing.
6. **The provider.** `provider.rs`, `config.rs`, registration: one
   instance per `[providers.acp.<name>]` row. `stream()` resolves
   `request.session` through the map (a request without one gets a
   one-shot session), holds one `session/prompt`, forwards mapped
   events; `cancel` → the `session/cancel` notification, then awaits
   the cancelled stop — the child and the agent session outlive the
   esc. `models()` empty; auth `NotApplicable`. `request_permission` is answered with its reject
   option and a notice naming the row — the adapter's own permission
   config (args/env on the row) is where yes is said;
   `elicitation/create` declines the same way.
7. **The words.** Config docs; AGENTS.md commit scopes gain
   `provider-acp`; black-box `crates/bingo/tests/cli/acp.rs` (a
   `--print` turn, an interrupt, the ladder's three restores); a live
   `claude-agent-acp` smoke behind an env guard beside the other live
   smokes — needs node and a login, never CI.

## Files

`crates/bingo-provider-acp/src/{lib,method,wire,connection,child,
events,pool,transcript,provider,config}.rs` + the fake-agent bin;
`crates/bingo-sdk/src/model.rs` and the one core request-build site;
`crates/bingo/tests/cli/acp.rs`; `scripts/budget.sh` (the number);
`Cargo.{toml,lock}`; `AGENTS.md` (one scope); docs for config.

## Exit criteria

- [ ] Every message used has a fixture round-trip; `cargo deny check`
  green; budget 307 with the ADR line.
- [ ] `--print` through the fake agent: text, thought and an external
  tool call land in valid NDJSON, marked `acp.external: true`, and
  nothing was executed by the loop.
- [ ] Esc mid-turn sends `session/cancel` and the turn ends with the
  interrupt wording; the child and the agent session survive to
  serve the next turn.
- [ ] Two turns of one bingo session ride one agent session on one
  child (the fake counts its spawns and its `session/new`s); the
  `bingo.acp` extension is journaled exactly once.
- [ ] `--continue` against a fake advertising resume / only load /
  neither: each rung proven; on the last, the prompt names the file
  and the file holds the prior fold; a load replay journals nothing.
- [ ] A scripted `request_permission` is answered with its reject
  option, one notice names the config row, and the turn goes on.
- [ ] `cargo check -p bingo-provider-acp --all-targets --target
  x86_64-pc-windows-msvc` (the child spawns, both spellings); every
  gate in AGENTS.md.

## Non-goals

`bingo-acp` the surface (opposite role, its own milestone). Handing
our tools over MCP. ACP plans, modes, slash commands, `fs/*`,
`terminal/*`, the `-http` transport, protocol v2. A prompter door
for providers — recorded in the ADR, not built. Shipping default
adapter rows — configuration belongs to the person.

## Risks

- The `=1.5.0` pin: a schema bump is deliberate and the compiler
  walks the mapping; recorded fixtures catch silent shape drift.
- Adapters are node children; when one goes (stop, death, fallback)
  the whole process group must go with it or the npx tree lingers
  (M34-D's lesson).
- An adapter may mishandle many sessions across one connection's
  life: when a child misbehaves at `session/new`, the instance falls
  back to a fresh child for that turn and says so.
- The swallowing flag on a `session/load` replay is load-bearing;
  its test is the ladder's most important line.
- codex-acp reports no usage today: zeros are honest, and the ruler
  reads them as unknown, not as free.
