# M39 — Shared tools cross as MCP

## Goal

ADR-0036 built: an ACP member can act, not only read. Bingo's shared
tools are served to the agent over MCP — injected at `session/new`,
executed by the running turn, the offer derived from the tools catalog
so it syncs itself when a tool is added later.

## Bricks, in build order

**Kernel words (`bingo-sdk`, `bingo-core`) — worker Q**

1. The catalog's `tools()` meta gains `inputSchema` and the traits
   (catalog.rs:98 already copies description) — the bridge's
   bootstrap-before-the-first-prompt reads it. No new trait: the offer
   is `ModelRequest.tools`, which the kernel already assembles per
   session (ADR-0036 §1). Regenerate the plugin-rpc committed schema
   if the wire shape moved. Fixture: a catalog entry shows schema and
   traits.
2. The door: one `HostApi` verb (worker names it) delivering a
   `ToolCall` into a session's running turn. Executed via the existing
   executor with the turn's gate; a real tool item journaled under the
   turn; the cancel token a child of the turn's; the outcome handed
   back to the caller, never to the provider's messages. Refused with
   a reason when no turn is in flight. Served WHILE the stream is open
   — the agent is blocked on the answer; the seam is the session
   actor, beside the turn loop, not inside it. Tests: executed and
   journaled mid-turn; refused idle; interrupt drops it and the caller
   is told; a gate-denied call reports the denial; a call for a tool
   the turn's request did not offer is refused — the request is the
   authority (ADR-0036 §2).

**Transport (`bingo-provider-acp`, `bingo` bin) — worker R, parallel**

3. Token and listener: mint/verify (pure brick), unix socket + named
   pipe behind one cfg'd module, one accepted stream per token, gone
   with the instance.
4. `acp-mcp-proxy` hidden bin mode: a stdio↔socket pump, no logic,
   token from env. Windows spelling in the same change.
5. The rmcp server loop against a faked pair of doors
   (`offer() -> Vec<ToolSpec>`, `call(ToolCall) -> Outcome`):
   initialize, tools/list, tools/call, tools/list_changed. Feature
   flip: rmcp `server`, `transport-io`; `budget.sh` answers.

**The joint — worker S, after Q and R**

6. Real doors over `HostHandle`. The offer: `ModelRequest.tools` of
   the turn being served, minus the exclusion const (fs, bash, web
   tool names + `SpawnAgent`, `AskUserQuestion` — the bridge's
   `NOT_A_CHILDS` shape), minus source tools when they are forwarded;
   bootstrap from the catalog minus the same, converging on the first
   request; a changed offer → `tools/list_changed`. Injection at
   `session/new`: the bridge row (token in env), and the `mcp.servers`
   rows verbatim (stdio and http; sse skipped with a notice) under
   `forwardMcp`, default true — `false` keeps the rows home and
   their tools on the bridge instead (ADR-0036 §4). Adapter row grows
   `tools` (explicit offer, replaces the derived one) and
   `forwardMcp`.
   The preamble names the bridge's tools. `CatalogChanged` →
   `tools/list_changed` to live sessions.
7. Black-box (`bingo/tests/cli/acp.rs`; the scripted fake agent grows
   an `mcp` capability: dial the injected row, call one tool). The
   fake agent posts to a room over the bridge — the post is in the
   room's journal, the tool item under the member's turn. A stray call
   after the turn ends is refused. Esc mid-call. A `forwardMcp`
   adapter sees the forwarded row verbatim. The live-smoke runbook
   gains the codex bridge check.

## Files

`bingo-sdk/src/tool.rs`, `bingo-sdk/src/host.rs`, `bingo-core/src/
host/catalog.rs`, `bingo-core/src/{host,session,turn,executor}.rs` as
the seam demands; declarations in `bingo-agents`, `bingo-rooms`,
`bingo-tasks`, `bingo-skills`; `bingo-provider-acp/src/{bridge (new),
socket (new), session, config, provider}.rs`; `bingo/src/main.rs`
(hidden mode); `bingo-plugin-rpc/schema/plugin.json`; `bingo/tests/
cli/acp.rs`, `bingo-provider-acp/tests/`; `scripts/acp-smoke.md`.

## Exit criteria

- [x] The scripted agent posts over the bridge; the post is in the
  target's journal; the tool item sits under the member's turn, marked
  external. The target is the member's parent rather than a room: the
  same `SendMessage` door, one fewer fixture.
- [x] A call with no turn in flight is refused with a reason.
- [x] Esc during a bridged call: the turn ends, the call is dropped,
  the MCP answer is an error, the child lives to serve the next turn.
- [x] The offer is derived, not listed: the only tool-name list in
  `bingo-provider-acp` is the exclusion const. A tool registered by a
  test plugin appears on the bridge with no provider-acp edit; a
  session spawned with a restricted `tools` list offers only the
  restriction — both asserted.
- [x] `mcpServers` rows are forwarded verbatim by default and their
  tools leave the bridge offer — nothing is served twice; under
  `forwardMcp: false` nothing is forwarded and the sourced tools ride
  the bridge (gated, untrusted) instead; a row this agent cannot take
  is skipped and said. All pinned by tests. **Not an sse row**: an sse
  row never becomes a row — `bingo-mcp` refuses the transport where it
  reads the key, before anything can be forwarded (see Verified).
- [x] Every AGENTS.md gate; Windows cross-check for socket/pipe, the
  proxy mode and the child work
  (`cargo check -p bingo-provider-acp --all-targets --target
  x86_64-pc-windows-msvc`). The `bingo` binary's cross-check does not
  run on this machine, pre-existingly (see Verified).

## Non-goals

`SpawnAgent` / `AskUserQuestion` crossing. A gate UI for bridged calls
beyond the existing modes. Sse bridging. Any endpoint but the
per-run socket — bingo serves MCP to its own ACP children only.

## Risks

- The turn serving a call while its stream is open: `Turn`'s borrow
  shape may fight; the seam is the session actor, not the loop. If it
  fights hard, the door may hold the executor pieces (gate, journal
  writer, token) rather than the turn itself.
- `WaitAgent` over the bridge is a long call inside a turn: bounded by
  the turn's interrupt, not a timer — said in the preamble.
- A double-dialled MCP server (bingo's client and the agent both):
  recorded in ADR-0036 §4, not cured.
- rmcp's server features may pull transitive weight; `budget.sh`
  decides, the cap moves only with the measured number.
- A tool item lands under a turn whose assistant item is still
  streaming — surfaces must fold that interleaving; a `TestBackend`
  look before done.
- The bridge's peer may reconnect (its MCP client respawns a dead
  proxy): a token is re-usable after its stream closed; only a second
  concurrent stream is refused.
