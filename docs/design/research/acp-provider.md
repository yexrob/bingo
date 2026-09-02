# ACP as a model `Provider` — research (verified 2026-09-02)

Question: bring Claude Code, Codex and everything else that speaks ACP into bingo as a
`Provider` implementation (horizons.md §2). Don't reinvent: find the library.

**Answer up front.** The wheel exists and is Rust: the official `agent-client-protocol`
crate, v2.0.0, Apache-2.0, ~1.84M downloads in 90 days, both sides implemented. It is
*not* the whole answer, because it drags a second async runtime (smol) and yields `!Send`
futures. Recommendation in §7. And the shape itself is already shipped by someone else:
**goose ships "ACP providers"** — claude-acp, codex-acp, amp-acp, pi-acp as model
providers, goose's own extensions handed to the agent as MCP servers. That is exactly
horizons §2, in production, in Rust, today.

## 1. The protocol

- ACP (agentclientprotocol.com), started by Zed, now its own GitHub org
  `agentclientprotocol/`. JSON-RPC 2.0 over stdio (an agent is a child process; the
  editor is the client). HTTP/SSE and WebSocket transports exist as a separate crate.
- **`protocolVersion` is a single integer MAJOR version; v1 = `1`.** v2 was published in
  **Draft** on 2026-07-20 and is behind `unstable_protocol_v2` in the SDKs. v1 is what
  ships. <https://agentclientprotocol.com/protocol/initialization>
- Agent side: `initialize`, `authenticate`, `session/new`, `session/prompt` (baseline);
  `session/load`, `session/resume`, `session/list`, `session/delete`, `logout`,
  `session/set_mode` (optional); `session/cancel` (notification).
- Client side — **what bingo must implement**: `session/request_permission` (baseline);
  `fs/read_text_file`, `fs/write_text_file`, `terminal/*`, `elicitation/create`
  (optional); receives `session/update` and `elicitation/complete` notifications.
  <https://agentclientprotocol.com/protocol/overview>
- `session/new { cwd, mcpServers }`. MCP transports: **stdio mandatory**, http/sse gated
  by `agentCapabilities.mcpCapabilities`. `agentCapabilities` also carries `loadSession`,
  `promptCapabilities {image, audio, embeddedContext}`, `sessionCapabilities
  {resume, close, additionalDirectories}`, `agentInfo`, `authMethods`.
- Recent v1 stabilizations (<https://agentclientprotocol.com/updates>): **`usage_update`
  session notification** (context-window used/size + cumulative cost) June 2026;
  `messageId` on streamed chunks; `session/delete`; `additionalDirectories`;
  Elicitation (structured form + URL modes) and `$/cancel_request` July 2026;
  `session/list` and `session_info_update` March 2026.
- Maturity: SDKs hit 1.0.0 on 2026-06-25 ("stable foundations"); JetBrains + Zed launched
  an **ACP Agent Registry** in Jan 2026; GitHub Copilot CLI shipped ACP public preview
  2026-01-28. This is no longer a Zed side-project.

## 2. Rust: the official crate

`agent-client-protocol` **2.0.0**, published **2026-07-23**, Apache-2.0,
repo `agentclientprotocol/rust-sdk`, 4.10M downloads total / 1.84M in 90d,
**MSRV 1.88.0** (we are on 1.96 — fine).

- **Both sides.** `Client`, `Agent`, plus `Proxy` and a `Conductor` for proxy chains;
  `Client.builder()` / `Agent.builder()` are the stable v1 APIs, `.v2()` under
  `unstable_protocol_v2`. Client trait methods: `session_notification`,
  `request_permission`, `read_text_file`/`write_text_file`, terminal methods.
  Client pattern: spawn the agent as a child with piped stdio →
  `ClientSideConnection::new(...)` → drive `handle_io` → `initialize` → `new_session` →
  `PromptRequest`. (<https://deepwiki.com/agentclientprotocol/rust-sdk/5-usage-guides>)
- **Sibling crates**: `agent-client-protocol-schema` (pinned `=1.5.0` by the core crate —
  pure types), `-derive`, `-http` (HTTP/SSE + WebSocket), `-rmcp` (bridge to the `rmcp`
  MCP SDK — the same crate bingo already uses at 3.1). `agent-client-protocol-tokio`
  exists but is **stale: 0.11.1, 2026-04-21**, four months behind core 2.0.0. Do not
  build on it.
- **Dependencies** (crates.io, 2.0.0, normal): `agent-client-protocol-schema =1.5.0`,
  `-derive ^2`, `async-io ^2`, `async-process ^2`, `blocking ^1`, `futures ^0.3.32`,
  `futures-concurrency ^7.6.3`, `rustc-hash`, `schemars ^1.0`, `serde`, `serde_json`,
  `shell-words`, `tracing`, `uuid ^1.18`, `rustix` (unix), `windows-sys ^0.61` (win).
  **No reqwest** in the core crate (reqwest lives in `-http` only). **`schemars ^1.0`
  matches our 1.2.2**, and `serde_json` wants `preserve_order` + `raw_value` — we already
  set `preserve_order`. The rust-crates.md divergence worry is resolved: goose's tree
  (reqwest 0.13 + schemars 1.x) is ours, and ACP asks for nothing else.
- **Measured cost** (`cargo add` in a scratch crate, `cargo tree --edges normal`
  diffed against this workspace's 301):
  - full `agent-client-protocol` 2.0.0 → **+27 crates**, of which **14 are the smol
    async stack** (`async-io async-process async-channel async-lock async-signal
    async-task blocking polling piper parking concurrent-queue event-listener
    event-listener-strategy futures-lite`) plus `serde_with(+macros)`, `uuid`,
    `shell-words`, `futures-concurrency`, `pin-project(+internal)`, `fixedbitset`,
    `anyhow`, `unicode-xid`, and the three acp crates.
  - `agent-client-protocol-schema` **1.5.0 alone → +5 crates**
    (`agent-client-protocol-schema`, `anyhow`, `serde_with`, `serde_with_macros`,
    `unicode-xid`). Everything else it wants (schemars, serde, serde_json, strum,
    derive_more, diffy-optional) is already in our tree.
- **Known client-side friction**: the SDK's futures are **`!Send`**; you must run them on
  a `tokio::task::LocalSet` and pass `spawn_local` as the spawner. bingo's
  `Provider: Send + Sync` returning a `ModelStream: Send` cannot hold an ACP connection
  directly — it needs a dedicated thread with a current-thread runtime + LocalSet and a
  channel to the outside. That is a real design cost, not a detail.
- Other client users: **Zed** itself; **goose** (`agent-client-protocol 2.0.0` +
  `-http 2.0.0` + `-schema =1.5.0`, `[patch]`ed to a git rev) as its ACP *providers*;
  JetBrains IDEs and a VS Code ACP client extension.

## 3. Adapters (what we would actually talk to)

| Agent | Spawn | Runtime | Notes |
|---|---|---|---|
| **Claude Code / Claude Agent** | `npx -y @agentclientprotocol/claude-agent-acp` (legacy `@zed-industries/claude-agent-acp` 0.23.1; `@zed-industries/claude-code-acp` 0.16.2 is **deprecated**) | node | built on the official Claude Agent SDK. Supports permissions (+ permission extension), client MCP servers, custom slash commands, images, @-mentions, edit review, TODO lists, nested subagent transcripts, interactive+background terminals, goal extension. Owns its own auth/billing (`ANTHROPIC_API_KEY`/`ANTHROPIC_BASE_URL` env, or Claude subscription login). |
| **Codex** | `npx -y @agentclientprotocol/codex-acp` | node | **`zed-industries/codex-acp` (Rust) was archived 2026-07-22**; the successor at `agentclientprotocol/codex-acp` is **TypeScript** on the Codex App Server. Auth: ChatGPT login, API key, custom gateway. Slash commands `/status /mcp /skills /goal /review`; native ACP subagent sessions. |
| **Gemini CLI** | `gemini --experimental-acp` (also `--acp`) | node (its own) | built in, no adapter. Known hang bugs in tty mode (gemini-cli #22782, PR #10089). |
| **Cursor** | `cursor-agent acp` | native | |
| **OpenCode / Copilot / mux / fast-agent** | npm or `uvx` adapters | node / python | |
| **OpenClaw** | `openclaw acp` | native | |
| Others in the registry | Amp, Pi, Goose, Cline, Junie, Kimi, Kiro, Qwen, Qoder, iFlow, Trae, Droid, OpenHands, Mistral Vibe, Docker cagent, Poolside… | mixed | registry: <https://agentclientprotocol.com/get-started/agents> |

Custom-agent config is uniformly `{ command, args, env }` — the same three fields bingo's
`bingo-plugin-rpc` and `bingo-mcp` already spawn children with.

## 4. Other languages

- TypeScript: `@agentclientprotocol/sdk` (renamed from `@zed-industries/agent-client-protocol`),
  1.0.0 on 2026-06-25. It is the reference implementation the adapters are written in.
- Python: `agent-client-protocol` on PyPI, 0.12.1 — pydantic models + asyncio transports;
  still 0.x.
- Kotlin/Java: `agentclientprotocol/java-sdk`, plus JetBrains' Koog × ACP integration
  (Feb 2026); Kotlin SDK active as of 2026-08-31.

**No port is warranted.** The Rust crate is the most-downloaded implementation, is at 2.0,
implements the client side that Zed and goose ship on, and tracks the schema first. The
only thing another language does better is nothing we need.

## 5. Mapping: `Provider`/`ModelRequest` ↔ ACP

| bingo | ACP | Notes |
|---|---|---|
| `Provider::id()` / `family()` | one provider instance per configured adapter | `acp/claude`, `acp/codex` |
| `Provider::stream(request, cancel)` | one `session/prompt`, held open | the whole turn is one call |
| `ModelRequest.messages` | **only the last user turn** → `PromptRequest.prompt: ContentBlock[]` | ACP sessions are stateful; `session/prompt` "carries the new user message only". See decision D1. |
| `ModelRequest.system` | dropped, or prepended to the first prompt | the agent has its own system prompt. Nothing maps. |
| `ModelRequest.tools` | **not sent as tools** — exposed via `session/new { mcpServers }` | goose's exact move: hand our tools to the agent as an MCP server. bingo already has an MCP client (`rmcp`); it would need to also *serve* MCP over stdio, or use the `-rmcp` bridge / `unstable_mcp_over_acp`. |
| `ContentPart::Image` | `ContentBlock::image` if `promptCapabilities.image` | else degrade |
| `ModelRequest.max_tokens`, `reasoning: Effort` | nothing standard; `session/set_mode` / config options | not mapped |
| `session/update` `agent_message_chunk` | `TextStart`/`TextDelta`/`TextEnd` keyed by `messageId` | |
| `session/update` `agent_thought_chunk` | `ReasoningStart`/`Delta`/`End` | |
| `session/update` `tool_call` / `tool_call_update` | **decision D2** — see below | `toolCallId, title, kind(read/edit/delete/move/search/execute/think/fetch/other), status(pending/in_progress/completed/failed), content(content\|diff\|terminal), locations, rawInput/rawOutput` |
| `session/update` `plan`, `available_commands_update`, `current_mode_update` | no `ModelEvent` | notices, or `_meta`-carried; not mapped in v1 |
| `session/request_permission` (client→) | `InteractionKind::Permission` via `Prompter::ask` | `PermissionOption{optionId, name, kind: allow_once\|allow_always\|reject_once\|reject_always}` ↔ `AnswerSpec`; outcome `selected{optionId}` / `cancelled`. **`Provider::stream` has no prompter** — only `Provider::login` takes `Arc<dyn Prompter>`. Decision D3. |
| `elicitation/create` | `InteractionKind::Question` | same door |
| `fs/*`, `terminal/*` client capabilities | declare **false** in v1 | the agent uses its own machine; bingo's `tool-fs`/`tool-bash` are not in this loop |
| `cancel: CancellationToken` | `session/cancel` notification, then await `stop_reason: cancelled` | clean 1:1 |
| `PromptResponse.stop_reason` | `ModelEvent::Finish { finish_reason }` | `end_turn`→`Stop`, `max_tokens`→`Length`, `max_turn_requests`→`Other`, `refusal`→`ContentFilter`, `cancelled`→`Other`. **Never `ToolCalls`** — the agent ran its own tools. |
| `Usage` | `PromptResponse.usage` (input/output/total/cached/thought) — **behind `unstable_end_turn_token_usage`**; `usage_update` notification (context used/size + cost) is **stable** | claude-agent-acp populates `usage`; codex-acp does not (goose #8132). Fill what arrives, zero otherwise. `usage.cost` has no home in `Usage`. |
| `EndpointCapabilities` | `images` ← `promptCapabilities.image`; `count_tokens` false; `caching` false | |
| `ModelFacts` (core catalogue) | `context_window` ← `usage_update.size` when it arrives, else the `UNKNOWN` fallback in `models/resolve.rs`; `reasoning: true`; `images` from `promptCapabilities` | a models.dev row cannot describe "Claude Code". |
| `Provider::models()` | ACP has no model list (v1) | `Vec::new()`, or the adapter's own config options |
| `Provider::auth()` / `login()` | `initialize.authMethods` + `authenticate`; mostly **the adapter's own login** | `AuthStatus::NotApplicable` is honest for claude-agent-acp. |
| `count_tokens` | none | `Unsupported` |

**Does not map**, either way: our `system` blocks and prompt caching; `ToolSpec`s as
model-visible tools; `Effort`; ACP's `plan`, modes, slash commands, `session/list`,
`fs/*`, `terminal/*`, cost-in-currency, and `messageId` grouping.

## 6. Cost and risk

- **Dependencies**: +27 crates for the full SDK (budget is `max_dependencies = 302`,
  we sit at 301 → 328, needs an ADR line). +5 for schema-only. Fourteen of the 27 are a
  second async runtime we would carry for one plugin.
- **Version conflicts: none found.** schemars ^1.0 ✓ (we have 1.2.2), serde_json ^1 ✓,
  futures 0.3 ✓, tracing ✓, no reqwest, MSRV 1.88 < 1.96 ✓. The old rust-crates.md worry
  does not survive contact with 2.0.0.
- **`!Send` is the real cost**, not the crates. An ACP connection cannot live inside a
  `Send` provider future; it needs an owning thread + `LocalSet` + channels.
- **Process lifecycle**: one adapter child **per bingo session** (an ACP session is
  stateful and holds the history). A provider instance owns a pool keyed by
  `SessionId`; the child dies with the session; `session/load` or `session/resume`
  reattaches after a bingo restart if `loadSession` is advertised. Every first-tier
  adapter needs **node** on PATH.
- **Auth is theirs.** Claude Code logs in with its own subscription/API key, Codex with
  ChatGPT login. bingo's OAuth (`bingo-auth-oauth`) is not involved. Upside: no per-token
  API cost for a subscriber — goose's stated reason for shipping ACP providers.
- **Licensing**: Apache-2.0 throughout (crate, adapters, protocol). `cargo deny` must
  learn Apache-2.0 if it does not already allow it.
- **Churn**: the protocol stabilized eight RFDs in six months and has a v2 Draft; the
  adapters renamed npm scopes twice in 2026 (`@zed-industries/claude-code-acp` →
  `claude-agent-acp` → `@agentclientprotocol/*`). Pin nothing in code; the command lives
  in config.

## 7. Recommendation

1. **Use the official Rust crate — `agent-client-protocol` 2.0.0, client side, stdio,
   default features only** (no `unstable_*` except `unstable_end_turn_token_usage`, which
   is the only way to get per-turn tokens). Do not port anything. Do not build on
   `agent-client-protocol-tokio` (stale). Do not take `-http` in v1.
   *The alternative worth costing in the ADR*: `agent-client-protocol-schema` alone
   (+5 crates) over our own line-framed JSON-RPC — `bingo-plugin-rpc/{codec,connection,
   wire}.rs` is 2.3k lines of exactly this, already tokio, already `Send`. It buys the
   `!Send` problem away and 22 crates back, and costs us the reconnect/proxy machinery
   and the duty to track the protocol by hand. Given that the *types* are the contract
   and the transport is 200 lines we have written twice, this is a genuine coin-flip;
   my lean is **schema-only**, precisely because the `!Send` thread-hop is the kind of
   structure that infects the whole plugin.
2. **Adapters, in order**: `@agentclientprotocol/claude-agent-acp` (Claude Code) first —
   best ACP coverage, populates usage. Then `@agentclientprotocol/codex-acp`. Then
   `gemini --experimental-acp`. Everything else is config: `{command, args, env}`,
   no code.
3. **Read goose before writing a line.** `aaif-goose/goose` ships this exact feature
   (docs: "ACP Providers"), including the extensions-as-MCP-servers trick and the known
   holes (usage dropped, no fork/resume, session-id mismatch). It is the reference.

### The decisions the ADR must make

- **D1 — session identity.** One ACP session per bingo session, sending only the new user
  turn (stateful, cheap, matches ACP's design), versus stateless replay of the folded
  context each turn (matches bingo's "journal is truth" and `ContextView::fold`, but no
  agent supports being re-fed its own history). **Take the stateful mapping** and accept
  that for an ACP provider the *agent* owns the context — which means bingo's compaction,
  ruler and `TurnUsage` are advisory here, and a resumed session depends on
  `session/load`. This is the decision that shapes everything else.
- **D2 — the agent's own tool calls.** Three options: (a) reasoning text — lossy, but
  zero new vocabulary; (b) notices via `ToolHost::record` — not reachable from a
  provider; (c) **first-class**: emit `ToolInputStart/Delta/End` + `ToolCall` and a
  synthetic `ToolResult`, so the TUI renders them like any tool. (c) is the only one that
  makes `diff`/`terminal`/`locations` visible, but it puts calls in the journal that
  bingo never executed and cannot re-run — an honest second representation risk that the
  ADR must rule on. Recommend (c) with an explicit `provider_options` marker
  (`acp.external: true`) so the loop never tries to execute them.
- **D3 — the provider's prompter door.** `Provider::stream` has no `Prompter`;
  `ToolHost: Prompter` is the tool-side door and `Provider::login` already takes
  `Arc<dyn Prompter>`. `session/request_permission` needs one *during a turn*. Either add
  an optional `Arc<dyn Prompter>` to `ModelRequest`/`stream` (a kernel change, touching
  every provider) or hand the ACP provider a prompter at construction from the plugin's
  registration (no kernel change, but the provider then holds session-scoped state).
  Per horizons' rule — point at a door the kernel already has — start with the second and
  mint the trait change only if a second provider needs it.

Secondary, cheaper: whether tools go over as MCP at all in v1 (recommend **no** — the
agent brings its own tools; horizons §2 says "running its own tools on its own side"),
and whether `bingo-acp` the *surface* (gateway-and-surfaces.md, bingo as agent) and this
*provider* share a crate. They should not: same protocol, opposite roles, and ADR-0001
forbids a plugin importing a plugin. Two crates, `bingo-acp` (surface) and
`bingo-provider-acp`, sharing only the third-party types.

### Sources

crates.io API (2026-09-02) for `agent-client-protocol` 2.0.0 / `-schema` 1.5.0 /
`-tokio` 0.11.1 · <https://github.com/agentclientprotocol/rust-sdk> ·
<https://agentclientprotocol.com/protocol/overview>, `/initialization`, `/session-setup`,
`/prompt-turn`, `/tool-calls`, `/updates`, `/get-started/agents`, `/rfds/session-usage` ·
<https://deepwiki.com/agentclientprotocol/rust-sdk/5-usage-guides> ·
<https://goose-docs.ai/docs/guides/acp-providers/> ·
<https://github.com/aaif-goose/goose/issues/8132> ·
<https://github.com/agentclientprotocol/claude-agent-acp>,
<https://github.com/agentclientprotocol/codex-acp>,
<https://github.com/zed-industries/codex-acp> (archived 2026-07-22) ·
<https://www.npmjs.com/package/@zed-industries/claude-agent-acp> ·
<https://docs.openclaw.ai/tools/acp-agents> · <https://zed.dev/docs/ai/external-agents> ·
<https://blog.jetbrains.com/ai/2026/01/acp-agent-registry/> ·
<https://github.blog/changelog/2026-01-28-acp-support-in-copilot-cli-is-now-in-public-preview/> ·
local measurement: `cargo tree --edges normal` diffed against this workspace (301 crates).
