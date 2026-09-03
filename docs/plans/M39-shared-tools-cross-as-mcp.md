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

## Verified

2026-09-03, macOS 15 (aarch64), branch `m39-joint`.

### The gates

```
$ cargo fmt --all -- --check
(no output)

$ cargo clippy --workspace --all-targets --locked -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 16.77s

$ cargo test --workspace --locked   # exit 0
75 result lines: 3214 passed; 0 failed; no FAILED, no failures:

$ cargo test -p bingo --test cli acp::
test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 138 filtered out

$ scripts/check_discipline.sh
dependency direction ok
kernel names no tool
cohesion ok
discipline ok

$ scripts/budget.sh
dependencies (unique, normal): 310 (max  310)
warm cargo check -p bingo-core: 0s (max  20s)
relink isolation: touching the TUI recompiled 0 crates for core (must be 0)
target/debug: 11 GB (soft max  5)
warn: target/debug exceeds the soft limit
test binaries: 72
budget ok

$ cargo deny check
advisories ok, bans ok, licenses ok, sources ok

$ cargo check -p bingo-provider-acp --all-targets --target x86_64-pc-windows-msvc
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 18.72s

$ scripts/tui-smoke.sh
15 scenes, ending "a button on a pinned board fires its command…"
tui-smoke ok
```

The joint added no dependency: 310 is the number the transport half
measured, unmoved. The `target/debug` warn is this worktree's build
cache, not the tree — it warns on a clean checkout too, and `budget.sh`
does not fail on it.

`check_discipline.sh` prints its usual pre-existing file-length warns
(TUI, core, `bingo/tests`); none of them is a file this milestone wrote,
and the two new test files land under the ceiling that first caught them
— `bingo/tests/cli/acp.rs` is 666 lines with the bridge scenarios moved
into `bingo/tests/cli/acp/bridge.rs`.

### The Windows cross-check of the binary

`cargo check -p bingo --all-targets --target x86_64-pc-windows-msvc`
does not run on this machine, and did not before this milestone:

```
error occurred in cc-rs: command did not execute successfully …
  aws-lc-sys-0.44.0/aws-lc/third_party/jitterentropy/…/jitterentropy-timer.c
```

`aws-lc-sys` (reached through `bingo-auth-oauth`) compiles C against the
Windows SDK headers, which a developer's macOS box does not have. It
fails in a build script, before a line of bingo is compiled, and it fails
the same way on an untouched crate — worker R checked that on the
transport half. CI's `windows` job is the backstop. Everything M39 added
to the binary is one hidden subcommand that is the same code on both
platforms; the platform-shaped half (socket / named pipe, the proxy's
stream bounds) lives in `bingo-provider-acp`, whose cross-check is green
above.

### What the criteria were ticked on

Every scenario runs the shipped binary against the scripted agent, which
dials the injected row through the real `acp-mcp-proxy` and speaks MCP by
hand — no library on the client side, so what is proven is the protocol
(`crates/bingo/tests/cli/acp/bridge.rs`):

- `a_bridged_call_posts_to_the_parent_and_is_journaled_under_the_turn`
- `a_call_with_no_turn_in_flight_is_refused_with_a_reason`
- `an_interrupt_drops_a_bridged_call_and_the_child_serves_the_next_turn`
- `the_bridge_offers_the_sessions_tools_and_not_the_agents_own_hands`
- `a_tool_a_plugin_registered_reaches_the_bridge_with_no_edit_here`
- `a_row_that_names_its_tools_is_offered_only_those`
- `a_persons_own_servers_are_forwarded_verbatim_and_leave_the_offer`
- `a_row_that_keeps_its_servers_home_serves_their_tools_on_the_bridge`
- `a_row_this_agent_cannot_take_is_skipped_and_named`

And the risk the plan asked for a look at:
`bingo-surface-tui` `a_call_that_lands_while_the_assistant_is_still_writing`
— a tool item under a turn whose assistant item is still being written
reads as itself, at both sizes. No rendering change was needed.

### Two findings, recorded rather than cured

1. **An sse row cannot be "skipped and said".** ADR-0036 §4 says an sse
   row rides no further and is named. It cannot get that far: `bingo-mcp`
   refuses the transport where it reads `mcpServers`, so an sse row stops
   that plugin at boot and there is no row for anyone to forward —
   `[error] code=INTERNAL msg=plugin bingo.mcp failed to register:
   configuration: unknown variant `sse`, expected `stdio` or `http``.
   Making it forwardable would mean weakening a fail-fast that is right,
   so the translation keeps its skip-and-name arm for any transport it
   does not know (unit-tested against an sse row in
   `servers::theirs`), and the live path is pinned by the case that *can*
   happen: an http row to an agent whose handshake did not claim http.
2. **The rows are read through a service, not a second settings key.**
   The kernel refuses two plugins one key, so `bingo-mcp` registers
   `mcp.servers` (ADR-0031) and answers the rows it holds *now* — a
   `/mcp disable` takes a server out of the answer the same moment it
   takes it out of bingo's hands. `bingo-provider-acp` declares no
   `requires:` on it: a build without `bingo-mcp` forwards nothing and
   serves the bridge alone.
   The service opens **no wire face**. A row carries a person's `env`
   and `headers` — forwarding those to the agent is what ADR-0036 §4
   chose, and it is a narrow door: the child this session spawned. A
   wire face is a wide one, reachable by `service/call` from every
   out-of-process plugin, and nothing asks for it. `wire: None`, pinned
   by `the_plugin_registers_a_tool_source_a_command_and_the_rows`.
