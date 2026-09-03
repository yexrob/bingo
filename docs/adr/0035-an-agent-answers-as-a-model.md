# ADR-0035 — An agent answers as a model

Status: accepted · 2026-09-02 · Plan: M38

## Context

horizons.md §2 decided that an ACP agent — Claude Code, Codex, anything
in the registry — joins bingo as a `Provider`, running its own tools on
its own side. The research (docs/design/research/acp-provider.md,
2026-09-02) found the protocol grown up: JSON-RPC 2.0, newline-delimited
over the stdio of a child process spawned from `{command, args, env}`;
a registry of adapters; goose ships this exact shape in Rust today. The
official crate `agent-client-protocol` 2.0.0 implements both sides but
rides the smol runtime: its futures are `!Send`, which a `Send`
`ModelStream` cannot hold without a dedicated thread and a `LocalSet`,
and it brings 27 crates, 14 of them a second async runtime. Its sibling
`agent-client-protocol-schema` (=1.5.0) carries every message type with
serde, stands alone, and costs 5. The workspace has written the
transport itself twice (`bingo-plugin-rpc`: line-framed JSON-RPC,
tokio, `Send`).

## Decision

1. **One plugin, many instances.** `bingo-provider-acp` registers one
   `Provider` per configured adapter (`acp/claude`, `acp/codex`, …).
   An instance is three fields of config; its capabilities come from
   `initialize`, never from code. A new agent is a new row, no code.
2. **The types are the contract; the transport is ours.** Message types
   come from `agent-client-protocol-schema`; the ndjson JSON-RPC client
   loop and a ~15-row method table are written here, in tokio, `Send`.
   The full SDK is refused: a second runtime and a `!Send` thread-hop
   would buy ~300 lines of codec this workspace has twice already.
   Budget: `max_dependencies` 302 → 308 (measured; the member crate
   counts as one).
3. **One ACP session per bingo session** (stateful — how the protocol
   is consumed everywhere: Zed keeps one session per thread and
   replays nothing). The boundary learns one word: `ModelRequest`
   gains `session: Option<SessionId>` — the host names the session it
   streams for, a fact it already knew; a stateless provider ignores
   it, and any stateful wire (OpenAI's Responses API as much as ACP)
   reads the same field. The instance maps it to the agent's
   `sessionId`, journaled once as an extension (`bingo.acp`,
   `session:<instance>`): a pointer to the agent's own state, never a
   copy. `session/prompt` carries only the new turn; everything that
   crosses the wire is journaled as `ModelEvent`s — the journal stays
   the one record every surface and `bingo-experience` read. Restore
   climbs: `session/resume` (no replay — the journal already holds
   the history); else `session/load`, whose replay is swallowed, not
   journaled twice; else a fresh session whose first prompt names a
   transcript file rendered from the fold at that moment. Children
   are kept the way plugin processes are kept: spawned lazily,
   `initialize`d once, respawned on death with a notice, killed at
   `stop()`.
4. **The agent's tool calls arrive whole, not as calls** (amended in
   the building): a `ModelEvent::ToolCall` is an instruction — the
   loop would execute it and answer with a second `session/prompt`,
   wrong twice for a call the agent already ran itself. So the call,
   its status and its output ride the reasoning item's provider
   metadata, marked `acp.external: true`: journaled whole, executed
   never. A surface that wants tool rows for them reads that metadata
   — a surface slice, no kernel word. The TUI does, since 2026-09-03
   (`bingo-surface-tui`'s `acp` module): a finished call draws as the
   tool row it was, a call still arriving carries no metadata yet and
   reads as the thought's record, and `--print` reads that record
   still.
5. **Permissions are the agent's own.** The adapter is a whole agent,
   permission machinery included; the row that spawns it says what it
   may do in the adapter's own words (args or env — Claude Code's
   permission modes, Codex's approval policy). A
   `session/request_permission` that arrives anyway is answered with
   its reject option — fail closed — and a notice names the row to
   configure; `elicitation/create` is declined the same way. No
   prompter reaches a provider and no kernel door opens for one: the
   need is recorded here, not built.
6. **Not mapped, on purpose**: our tools do not cross (no MCP handover
   — the agent brings its own); `system`, `Effort`, caching and token
   counting do not cross either. (ADR-0037 amends this: `Effort` and the
   model now cross as `session/set_config_option` between turns; the rest
   of the list stands.) ACP's plans, modes and slash commands
   stay unmapped; `fs/*` and `terminal/*` are declared unsupported.
   `cancel` → the `session/cancel` notification; usage fills from what
   the adapter reports and is zero otherwise, honestly.
7. **Two crates, opposite roles**: `bingo-acp` (bingo as the agent, a
   surface, future work) and `bingo-provider-acp` share only the
   third-party types (ADR-0001: no plugin imports a plugin).

## Consequences

- For an ACP session the working context lives with the agent:
  compaction and the ruler shape only the fallback transcript, and a
  lost agent-side session degrades one rung with a notice. The kernel
  changes by one declaration, not by machinery: the request names its
  session. No prompter door opens; none of this reaches any other
  provider.
- The first-tier adapters need `node` on PATH; auth is the adapter's
  own (subscription or key), `AuthStatus::NotApplicable`.
- Protocol churn arrives as a schema-crate bump the compiler walks us
  through; conformance is ours to test — a scripted fake agent drives
  the contract tests, and a live `claude-agent-acp` smoke needs a
  login, so it stays behind an env guard like the other live smokes.
- `cargo deny` already admits Apache-2.0; the budget line moves once.

Refs: docs/design/horizons.md §2, docs/design/research/acp-provider.md
