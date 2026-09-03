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

1. **The offer is the request's own tool list.** The agent stands as
   the session's model, and the kernel already assembles what a model
   of this session is offered: `ModelRequest.tools` — child-filtered
   for a sub-agent seat, whole for a top-level one. The bridge serves
   that list minus one short, spelled-out exclusion: the machine's
   hands the agent already owns (the fs, bash and web tools) and the
   two the child rule names (`SpawnAgent`, `AskUserQuestion`) — a
   const beside the bridge, the same shape as `NOT_A_CHILDS`. No new
   trait, no declaration, no second selection anywhere: a tool reaches
   the agent the same turn it reaches any model, and a tool kept from
   the session's models is kept from the bridge by the same fact.
   Before the first prompt the offer is bootstrapped from the tools
   catalog (whose entries now carry `inputSchema` and the traits)
   minus the same exclusions, and converges on the first request — a
   changed offer becomes MCP's `tools/list_changed`.
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
   A call for a tool the turn's request did not offer is refused the
   same way — the request is the authority on the offer.
3. **The rendezvous is ours.** The plugin listens on a per-run unix
   socket — a named pipe on Windows, written in the same change — and
   mints one token per ACP session: the token is the address, the
   session is the authority, and it rides the server row's `env`, not
   argv. The row is stdio, the one transport every agent must speak:
   `{command: <current exe>, args: ["acp-mcp-proxy", …]}` — a hidden
   bin mode that pumps stdio↔socket and owns no logic. `rmcp`'s server
   half speaks MCP on the accepted stream.
4. **Third-party MCP crosses on its own wire by default.** The
   `mcp.servers` rows ride `session/new.mcpServers` verbatim (stdio
   and http; an sse row is skipped with a notice) — the agent dials
   them itself: one hop, their own env and auth — and the forwarded
   tools leave the bridge offer so nothing is served twice.
   `forwardMcp: false` on the adapter row keeps a person's rows home:
   then a `ToolSource`'s tools cross the bridge instead, gated and
   untrusted as ever (ADR-0009 §2), credentials never leaving bingo's
   hands. Recorded, chosen (the user's word): a forwarded row's env
   and headers may carry credentials into a foreign agent whose logs
   and model context we do not govern — the off-switch is the answer
   for rows that matter.
5. **The first prompt says so.** The preamble that names the transcript
   now also names the bridge: what can be called, what will not be
   answered. A tool in the hand is no tool if nobody says it is there.
6. **A row may still choose.** `tools: [...]` on the adapter row is an
   explicit offer list replacing the shared set — the person's own
   word on their own machine. Absent, the shared set is the offer and
   syncs itself.

## Consequences

- An ACP member holds what a sub-agent holds by construction — the
  same list, assembled by the same kernel path — less the machine's
  hands, which it brings itself under its own permission words
  (ADR-0035 §5). The gate and the journal see bridged calls as they
  see any call; surfaces render them with no new word.
- The kernel changes by two catalog lines and one door into the
  running turn. No trait is added, no tool machinery duplicated, and
  no tool list exists twice — the one list the bridge owns is an
  exclusion, not an offer.
- The turn must serve a bridged call while its stream is open; a door
  that waits for the stream's end deadlocks the agent against itself.
- Budget: rmcp grows the `server` and `transport-io` features on a
  crate already pinned — measured 308 → 310 (`uuid`, `pastey`, both
  the server feature's own). Token bytes come from `getrandom`
  (already in the tree, +0) rather than `aws-lc-rs`, whose C build
  breaks the Windows cross-check of the very crate that spells the
  named pipe.

Refs: ADR-0035 §§5–6, ADR-0001; Plan: M39
