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
   Budget: `max_dependencies` 302 → 307.
3. **The session is the agent's** (stateful): one ACP session per bingo
   session; `session/prompt` carries only the new turn. Everything that
   crosses the wire is journaled as `ModelEvent`s — the journal stays
   the one record every surface and `bingo-experience` read. The
   agent's `sessionId` is journaled once as an extension (`bingo.acp`,
   `session:<instance>`): a pointer to the agent's own state, never a
   copy of it. Restore prefers `session/resume` (no replay — the
   journal already holds the history); falls back to `session/load`,
   whose replay is swallowed, not journaled twice. Where neither door
   exists, a fresh session whose first prompt names a file for the
   agent to read with its own tools — the transcript so far, rendered
   from the journal at that moment, never maintained alongside it.
4. **The agent's tool calls are first-class**: `ToolInputStart/…/
   ToolCall` and a synthetic `ToolResult`, marked `acp.external: true`
   in `provider_options`; the loop never executes what wears the mark;
   a surface draws it like any tool row, diffs and terminals included.
5. **The permission door is handed at registration**: the plugin gives
   its providers one `Arc<dyn Prompter>`; `session/request_permission`
   and `elicitation/create` become Interactions through it. The
   `Provider` trait is untouched until a second provider needs the
   same door.
6. **Not mapped, on purpose**: our tools do not cross (no MCP handover
   — the agent brings its own); `system`, `Effort`, caching and token
   counting do not cross either. ACP's plans, modes and slash commands
   stay unmapped; `fs/*` and `terminal/*` are declared unsupported.
   `cancel` → the `session/cancel` notification; usage fills from what
   the adapter reports and is zero otherwise, honestly.
7. **Two crates, opposite roles**: `bingo-acp` (bingo as the agent, a
   surface, future work) and `bingo-provider-acp` share only the
   third-party types (ADR-0001: no plugin imports a plugin).

## Consequences

- For an ACP instance the context lives with the agent: compaction and
  the ruler are advisory there; a lost agent-side session degrades to
  the transcript-file path, and the degradation is said in a notice.
- The first-tier adapters need `node` on PATH; auth is the adapter's
  own (subscription or key), `AuthStatus::NotApplicable`.
- Protocol churn arrives as a schema-crate bump the compiler walks us
  through; conformance is ours to test — a scripted fake agent drives
  the contract tests, and a live `claude-agent-acp` smoke needs a
  login, so it stays behind an env guard like the other live smokes.
- `cargo deny` already admits Apache-2.0; the budget line moves once.

Refs: docs/design/horizons.md §2, docs/design/research/acp-provider.md
