# ADR-0036 — Shared tools cross as MCP

Status: accepted · 2026-09-03 · Plan: M39

## Context

ADR-0035 §6 kept our tools out of the ACP conversation: the agent
brings its own. The room made the cost visible: an ACP member reads
what the contributors serve (they are provider-blind), but cannot act —
no `SendMessage`, no task, no ear. It is mute in a house where speech
is a tool call. The protocol left a door open for exactly this:
`session/new.mcpServers` hands the agent MCP servers to dial, and the
stdio transport is mandatory for every agent. The workspace already
pins `rmcp` 3.1.4 for its MCP client; the server side is a feature
flag on the same crate. This ADR amends ADR-0035 §6: the second half
of its sentence — no MCP handover — is repealed for shared tools.

## Decision

1. **Whether a tool crosses is the tool's own word.** `ToolTraits`
   gains `shared: bool`, default false — the fail-closed rule the
   traits already live by. A shared tool acts on bingo's shared state
   (journal, rooms, tasks, the catalog), not on the machine (the agent
   has its own hands) and not on the user. First declarations:
   `SendMessage`, `ListAgents`, `WaitAgent`, `OpenRoom`, `Listen`,
   `TaskCreate/Get/List/Update`, `Skill`, `ListModels`. Not shared, on
   purpose: fs, bash and web (own capability), `SpawnAgent` and
   `AskUserQuestion` (the child rule — an ACP member holds what a
   sub-agent holds). The bridge keeps no list: the tools catalog's
   entries now carry `inputSchema` and the traits — facts the registry
   already held — and the offer is derived. A new tool that says the
   word appears on the bridge with no bridge edit, and appears live:
   `CatalogChanged` becomes MCP's `tools/list_changed`.
2. **A bridged call is the turn's call.** It can only arrive while a
   `session/prompt` is in flight — an agent calls tools mid-turn — so
   the kernel door is one verb: deliver this call into the session's
   running turn. The turn's own machinery serves it: the same gate, a
   real tool item journaled under the turn, a cancel token that is a
   child of the turn's — one `esc` drops it where it stands and the
   MCP answer says so. No turn in flight → refused, fail closed. The
   outcome returns over MCP only; it never enters the provider's
   message stream (the agent already holds it — a copy would be a
   second representation). And it is served beside the stream, not
   after it: the agent is blocked on the answer before it speaks on.
3. **The rendezvous is ours.** The plugin listens on a per-run unix
   socket — a named pipe on Windows, written in the same change — and
   mints one token per ACP session: the token is the address, the
   session is the authority, and it rides the server row's `env`, not
   argv. The row is stdio, the one transport every agent must speak:
   `{command: <current exe>, args: ["acp-mcp-proxy", …]}` — a hidden
   bin mode that pumps stdio↔socket and owns no logic. `rmcp`'s server
   half speaks MCP on the accepted stream.
4. **Third-party MCP crosses natively, not through the bridge.** The
   `mcp.servers` rows ride the same `mcpServers` list — stdio and http
   rows verbatim, an sse row skipped with a notice — so the agent
   dials them itself: one hop, their own env and auth. Per adapter
   row: `forwardMcp`, default true. Recorded, not cured: a server both
   sides use is dialled twice.
5. **The first prompt says so.** The preamble that names the transcript
   now also names the bridge: what can be called, what will not be
   answered. A tool in the hand is no tool if nobody says it is there.
6. **A row may still choose.** `tools: [...]` on the adapter row is an
   explicit offer list replacing the shared set — the person's own
   word on their own machine. Absent, the shared set is the offer and
   syncs itself.

## Consequences

- An ACP member aligns with a sub-agent: the same shared nouns, less
  the two child refusals, plus its own hands for fs/bash under its own
  permission words (ADR-0035 §5). The gate and the journal see bridged
  calls as they see any call; surfaces render them with no new word.
- The kernel changes by one trait field, two catalog lines and one
  door into the running turn. No tool machinery is duplicated, and no
  tool list exists twice.
- The turn must serve a bridged call while its stream is open; a door
  that waits for the stream's end deadlocks the agent against itself.
- Budget: rmcp grows the `server` and `transport-io` features on a
  crate already pinned; `scripts/budget.sh` is the referee.

Refs: ADR-0035 §§5–6, ADR-0001; Plan: M39
