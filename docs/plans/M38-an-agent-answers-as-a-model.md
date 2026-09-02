# M38 — An agent answers as a model

## Goal

ADR-0035 built: a `bingo-provider-acp` plugin whose every configured
ACP adapter (`acp/claude`, `acp/codex`, …) is a `Provider` instance.
Types from `agent-client-protocol-schema`; the ndjson JSON-RPC client
loop is ours, tokio, `Send`. Sessions are stateful on the agent's side
with the `sessionId` journaled once as a pointer; restore climbs
resume → load (replay swallowed) → fresh session whose first prompt
names a transcript file. The agent's own tool calls stream first-class
wearing `acp.external: true`; `session/request_permission` reaches the
person through a `Prompter` handed at registration. The kernel does
not change.

## Bricks, in build order

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
   the same fake drives the loop, the ladder and the black-box.
4. **The mapping, pure.** `events.rs`: `SessionUpdate → Vec<ModelEvent>`
   — message chunk → `Text*`, thought chunk → `Reasoning*`,
   `tool_call`/`tool_call_update` → `ToolInput*`/`ToolCall` plus the
   synthetic `ToolResult`, `acp.external: true` in `provider_options`;
   `stop_reason` → `Finish`; usage folded from `usage_update` and the
   end-turn field when present, zero otherwise. Fixture tests per
   update kind.
5. **The session pool.** `pool.rs`: children keyed by bingo session;
   `session/new` journals the extension (`bingo.acp`,
   `session:<instance>`) through the sdk doors the plugin already has.
   The restore ladder: `session/resume`; else `session/load` behind a
   loading flag so the replay never journals; else `transcript.rs`
   renders the journal to a file at that moment and the first prompt
   names it, with a notice saying which step was taken. Tests against
   the fake agent advertising each capability set.
6. **The provider.** `provider.rs`, `config.rs`, registration: one
   instance per `[providers.acp.<name>]` row; `stream()` holds one
   `session/prompt`, forwards mapped events, `cancel` → the
   `session/cancel` notification then awaits the cancelled stop;
   capabilities from `initialize`; `models()` empty; auth
   `NotApplicable`. The registration hands every instance one
   `Arc<dyn Prompter>`; `request_permission` / `elicitation/create`
   become Interactions and answer with the chosen option id.
7. **The words.** Config docs; AGENTS.md commit scopes gain
   `provider-acp`; black-box `crates/bingo/tests/cli/acp.rs` (a
   `--print` turn, an interrupt, the ladder's three restores); a live
   `claude-agent-acp` smoke behind an env guard beside the other live
   smokes — needs node and a login, never CI.

## Files

`crates/bingo-provider-acp/src/{lib,method,wire,connection,child,
events,pool,transcript,provider,config}.rs` + the fake-agent bin;
`crates/bingo/tests/cli/acp.rs`; `scripts/budget.sh` (the number);
`Cargo.{toml,lock}`; `AGENTS.md` (one scope); docs for config.

## Exit criteria

- [ ] Every message used has a fixture round-trip; `cargo deny check`
  green; budget 307 with the ADR line.
- [ ] `--print` through the fake agent: text, thought and an external
  tool call land in valid NDJSON, marked `acp.external: true`, and
  nothing was executed by the loop.
- [ ] Esc mid-turn sends `session/cancel` and the turn ends with the
  interrupt wording; the child is gone with the session.
- [ ] `--continue` against a fake advertising resume / only load /
  neither: each step proven; on the last, the prompt names the file
  and the file holds the prior turns; the load replay journaled
  nothing twice.
- [ ] A scripted `request_permission` reaches a Prompter and the
  answered option id reaches the fake.
- [ ] `cargo check -p bingo-provider-acp --all-targets --target
  x86_64-pc-windows-msvc` (the child spawns, both spellings); every
  gate in AGENTS.md.

## Non-goals

`bingo-acp` the surface (opposite role, its own milestone). Handing
our tools over MCP. ACP plans, modes, slash commands, `fs/*`,
`terminal/*`, the `-http` transport, protocol v2. Shipping default
adapter rows — configuration belongs to the person.

## Risks

- The `=1.5.0` pin: a schema bump is deliberate and the compiler
  walks the mapping; recorded fixtures catch silent shape drift.
- Adapters are node children; a dropped turn must kill the whole
  process group or the npx tree lingers (M34-D's lesson).
- The replay-swallow flag is load-bearing: a test pins that a
  `session/load` restore emits no second copy of history.
- codex-acp reports no usage today: zeros are honest, and the ruler
  reads them as unknown, not as free.
