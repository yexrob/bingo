# ACP Protocol Fit for Bingo

Research snapshot: 2026-08-18. This note uses only the official ACP
specification and repositories in the verified `agentclientprotocol`
organization. Schema observations are pinned to repository commit
[`983b864`](https://github.com/agentclientprotocol/agent-client-protocol/tree/983b864cdc27a93953cc3a8f4c27aacd194944e0).

## Decision

Keep bingo's native app-server protocol as the full-parity GUI contract. Add ACP
later as a sibling adapter over `AppCore`; do not make ACP plus a large private
extension surface the native GUI protocol.

ACP is directly relevant, but at a different boundary. It standardizes an
editor/client talking to a coding agent. Bingo's app-server replicates the state
of the whole bingo application: one persisted harness session, multiple kinds of
conversation, explicit turns and rounds, queues and steering, durable
interactions, agents, rooms, tasks, attention state, operations, catalogs, and
assets. ACP's standard surface intentionally does not model most of that product
state.

As of this snapshot, the current stable ACP wire protocol is **v1**
([official versioning](https://github.com/agentclientprotocol/agent-client-protocol#versioning)).
ACP v2 is **Draft** and its maintainers say to gate it behind version negotiation
and feature flags rather than ship it by default
([draft announcement](https://agentclientprotocol.com/announcements/acp-v2-draft#draft-status)).

## Where ACP fits

| Concern | ACP | Bingo app-server | Fit |
| --- | --- | --- | --- |
| Transport | JSON-RPC 2.0, UTF-8 newline-delimited stdio, protocol-only stdout, logs on stderr ([transport](https://agentclientprotocol.com/protocol/v1/transports#stdio)) | Same transport shape | Strong; reuse the discipline, not the wire identity. |
| Initialization | Integer `protocolVersion` plus role capabilities ([v1 initialization](https://agentclientprotocol.com/protocol/v1/initialization#protocol-version)) | Major/minor range, server epoch, limits, and `initialized` barrier | Similar purpose, incompatible messages. |
| Session meaning | One conversation/thread with its own context and history; one connection may host several sessions ([session model](https://agentclientprotocol.com/protocol/v1/session-setup)) | One persisted harness context owns main, agent, and room conversations plus shared resources | No lossless one-to-one mapping. |
| Prompt lifecycle | v1 keeps `session/prompt` open until a stop reason; v2 acknowledges acceptance and reports foreground state asynchronously ([v1](https://agentclientprotocol.com/protocol/v1/prompt-turn#4-check-for-completion), [v2](https://agentclientprotocol.com/protocol/v2/prompt-lifecycle#2-prompt-accepted)) | Submission immediately reports started, queued, delivered, applied, or operation-started; explicit turn lifecycle follows | v2 validates bingo's asynchronous direction, but neither version carries bingo's disposition model. |
| Messages and tools | Content blocks, message chunks, reasoning, tool status/content, diffs, terminals, plans, and usage ([v1 overview](https://agentclientprotocol.com/protocol/v1/overview#client), [v2 tool calls](https://agentclientprotocol.com/protocol/v2/tool-calls)) | Typed items with authoritative completion plus specialized append/replace updates | Strong projection target; many item types translate naturally. |
| Queue and steering | v2 explicitly permits queueing and steering, but the v2 schema has no standard queue identity, position, reclaim, or absorption method/event ([v2 rationale](https://agentclientprotocol.com/announcements/acp-v2-draft#moving-beyond-the-turn), [v2 schema](https://github.com/agentclientprotocol/agent-client-protocol/blob/983b864cdc27a93953cc3a8f4c27aacd194944e0/schema/v2/schema.json)) | Queue state, FIFO barriers, absorption, and tail reclaim are first-class | Semantic gap, not a naming gap. |
| Permissions and questions | Connection-scoped reverse requests: `session/request_permission` and `elicitation/create` ([permissions](https://agentclientprotocol.com/protocol/v1/tool-calls#requesting-permission), [elicitation](https://agentclientprotocol.com/protocol/v1/elicitation#creating-an-elicitation)) | Snapshot-visible `Interaction` resources answered by ID, with guard timing, session scope, receipts, and late-response rules | An adapter can bridge active requests; ACP is not the authoritative recovery model. |
| Recovery | v1 `session/load` replays history; v2 `session/resume` optionally replays from a cursor, currently from the start ([v1 replay](https://agentclientprotocol.com/protocol/v1/session-setup#loading-a-session), [v2 replay](https://agentclientprotocol.com/protocol/v2/session-setup#resuming-sessions)) | Atomic session/conversation reads, revisions, history generation, gapless event sequence, and resynchronization | ACP history replay does not replace bingo's live-state snapshots. |
| Product resources | Session list/delete/close, config options, slash commands, agent auth | Conversations, agents, rooms, tasks, deliveries, unread/mentions, provider auth, MCP/team actions, background commands, operations, and assets | The first-party GUI contract would be dominated by private extensions. |
| Extensibility | Custom data belongs in `_meta`; custom methods must begin with `_` and advertise capabilities ([extension rules](https://agentclientprotocol.com/protocol/v1/extensibility)) | These extra resources are required for CLI parity, not optional decoration | Extensions are useful for an ACP adapter, but a poor foundation for the native contract. |

The method-name overlap must not be mistaken for compatibility. For example,
both protocols have `initialize`, `session/list`, `session/resume`,
`session/close`, and `session/delete`, but their parameter shapes, capability
rules, and session semantics differ. Bingo must not advertise its native
app-server as ACP, and an ACP endpoint must use the exact ACP schemas.

## Official adapter precedent

The ACP organization's own Codex integration follows the recommended split.
`codex-acp` starts `codex app-server`, translates ACP requests into app-server
operations, and maps app-server events back to ACP
([README](https://github.com/agentclientprotocol/codex-acp/blob/47b57da5641a04df9aeeedc254a3aef53a9497da/README.md#L5-L18),
[process launch](https://github.com/agentclientprotocol/codex-acp/blob/47b57da5641a04df9aeeedc254a3aef53a9497da/src/CodexJsonRpcConnection.ts#L15-L35),
[translation boundary](https://github.com/agentclientprotocol/codex-acp/blob/47b57da5641a04df9aeeedc254a3aef53a9497da/src/CodexAcpClient.ts#L99-L104)).
Codex therefore keeps its richer native app-server even though both layers use
JSON-RPC-like messages.

The official Claude adapter shows how product-specific hierarchy degrades
gracefully at the ACP boundary. ACP has no standard nested-subagent relation, so
Claude exposes subagents as ordinary tool calls and adds the opt-in
`_meta.claudeCode.parentToolUseId` and `_meta.claudeCode.subagent` fields. Clients
without the capability receive a flattened standard fallback
([Claude adapter README](https://github.com/agentclientprotocol/claude-agent-acp/blob/50a95434e94318456f2d07c3d21aaf3595c3407d/README.md#L24-L33)).
The Codex adapter uses the same pattern with namespaced
`_meta.codex.subagent` metadata
([Codex adapter features](https://github.com/agentclientprotocol/codex-acp/blob/47b57da5641a04df9aeeedc254a3aef53a9497da/README.md#L9-L18)).

These are evidence for **native protocol plus ACP translation**, not for making
the richer product protocol an ACP extension bundle.

## Recommended boundary

```text
                         +-> TUI adapter
Engine/storage -> AppCore+-> native app-server -> first-party GUI (full parity)
                         +-> ACP adapter        -> ACP editors (standard subset)
```

The ACP adapter should contain translation only:

- Map one ACP session to a bingo user session's primary coding conversation;
  represent spawned agents as tool calls, with namespaced metadata only when an
  ACP client opts in.
- Map prompts, text/reasoning, tools, diffs, terminal output, usage, permission
  requests, elicitation, cancellation, session history, config options, and
  advertised slash commands where semantics are honest.
- Do not present rooms, team/task state, attention cursors, queue control, retry
  checkpoints, or general operations as standard ACP concepts. Either omit
  them with a documented fallback or expose narrowly scoped `_bingo/...`
  extensions behind `_meta` capabilities.
- Target stable ACP v1 first. Track v2 because its accepted-prompt lifecycle,
  stable message IDs, upserts, background updates, and display terminals are a
  closer projection; do not make the draft the default contract.
- Use the official Rust ACP SDK when implementing the independent endpoint
  rather than maintaining hand-written ACP envelopes
  ([official Rust library](https://agentclientprotocol.com/libraries/rust)).

## Effect on the obsolete JSON boundary

The former development-only `--json-events` boundary covered a small ACP-like
subset: start/cancel a turn, stream text and tool results, answer prompts, list
models/providers, and close/delete a session. ACP would be a better public
contract for that narrow editor-integration role, but retrofitting the old
`JsonSession` would preserve its duplicate state machine and missing CLI
behavior.

Delete it as planned. Build both the native app-server and any future ACP
endpoint from the same `AppCore`. The native protocol remains the parity source
of truth; ACP is a deliberately lossy, standards-compliant projection for
interoperability.
