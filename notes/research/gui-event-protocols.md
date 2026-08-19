# GUI Event Protocols: Codex and Claude Code

Research snapshot: 2026-08-18. This note uses only OpenAI and Anthropic documentation and source code. Source-code observations are pinned to commits so that later protocol changes do not silently alter the evidence.

## Conclusion

Codex's app-server is the stronger direct reference for a GUI contract: it exposes a bidirectional, typed protocol built around threads, turns, and items, with lifecycle notifications, streaming deltas, approvals, capability negotiation, and schema generation. Codex `exec --json` is a deliberately smaller, one-way automation projection and cannot represent the complete interactive experience.

Claude reaches comparable interactivity through the Agent SDK. Its public API yields a rich message union while a control channel carries interrupts, permission decisions, hook callbacks, MCP traffic, and other request/reply operations. The underlying Claude CLI uses newline-delimited `stream-json`, but applications should prefer the versioned SDK contract over copying its private transport details.

| Interface | Wire/API shape | Appropriate role |
| --- | --- | --- |
| Codex app-server | Bidirectional JSON-RPC-like messages; JSONL over stdio by default | Full interactive client and GUI |
| Codex `exec --json` | One-way JSONL event projection | Automation, logging, CI |
| Claude Agent SDK | Async message stream plus bidirectional control operations | Full interactive client and GUI |
| Claude CLI `stream-json` | NDJSON carrying messages and, in SDK use, control envelopes | SDK transport or carefully version-pinned integration |

## Codex

### App-server is the rich-client boundary

The app-server explicitly powers rich clients such as the Codex VS Code extension. Its protocol is JSON-RPC 2.0-like but omits the `"jsonrpc":"2.0"` field; stdio JSONL is the default transport, while WebSocket support is described as experimental and unsupported. Diagnostics go to stderr, and the server documents bounded queues plus a retryable overload error. ([app-server overview and transport](https://github.com/openai/codex/blob/e2eea071405a4d312ca9eabeed91b7e7cb9685c3/codex-rs/app-server/README.md#L1-L55))

The conversation model is `Thread -> Turn -> Item`. Clients initialize once, start or resume a thread, start a turn, and render item events until terminal turn completion. The initial handshake also negotiates client capabilities and notification opt-outs. ([core primitives and initialization](https://github.com/openai/codex/blob/e2eea071405a4d312ca9eabeed91b7e7cb9685c3/codex-rs/app-server/README.md#L66-L105))

The server distinguishes durable thread/turn/item history from ephemeral realtime events. For each item, the normal lifecycle is `item/started`, zero or more deltas, then `item/completed`; the completed item is authoritative. `turn/completed` is a terminal turn signal, not a replacement for the canonical item stream. ([event semantics](https://github.com/openai/codex/blob/e2eea071405a4d312ca9eabeed91b7e7cb9685c3/codex-rs/app-server/README.md#L1532-L1599))

The item union covers user and agent messages, plans, reasoning, command executions, file changes, MCP calls, collaboration operations, web search, image operations, review mode, sleeps, and compaction. Delta semantics are type-specific: text and command output append, while some updates are snapshots. ([item kinds and deltas](https://github.com/openai/codex/blob/e2eea071405a4d312ca9eabeed91b7e7cb9685c3/codex-rs/app-server/README.md#L1601-L1657))

Approvals and user-input prompts are server-initiated requests scoped to thread, turn, and item. Their lifecycle is observable around the request: item start, request/reply, request resolution, and authoritative item completion. Dynamic tools use the same server-request pattern. ([approvals and input requests](https://github.com/openai/codex/blob/e2eea071405a4d312ca9eabeed91b7e7cb9685c3/codex-rs/app-server/README.md#L1683-L1716), [dynamic tools](https://github.com/openai/codex/blob/e2eea071405a4d312ca9eabeed91b7e7cb9685c3/codex-rs/app-server/README.md#L1796-L1832))

The protocol separates stable features from experimental ones. Experimental methods require an initialization capability, and the server can generate TypeScript declarations or JSON Schema for the exact running Codex version. ([stability policy](https://github.com/openai/codex/blob/e2eea071405a4d312ca9eabeed91b7e7cb9685c3/codex-rs/app-server/README.md#L2458-L2509), [schema generation](https://github.com/openai/codex/blob/e2eea071405a4d312ca9eabeed91b7e7cb9685c3/codex-rs/app-server/README.md#L57-L64))

### `exec --json` is a projection

`codex exec --json` emits JSONL, but its public event enum contains only thread start, turn start/completion/failure, item start/update/completion, and error. ([CLI flag](https://github.com/openai/codex/blob/e2eea071405a4d312ca9eabeed91b7e7cb9685c3/codex-rs/exec/src/cli.rs#L44-L52), [exec event union](https://github.com/openai/codex/blob/e2eea071405a4d312ca9eabeed91b7e7cb9685c3/codex-rs/exec/src/exec_events.rs#L7-L49))

The event processor maps only selected core events and drops unrepresented variants; for example, hook lifecycle and model-verification events are ignored. ([projection mapping](https://github.com/openai/codex/blob/e2eea071405a4d312ca9eabeed91b7e7cb9685c3/codex-rs/exec/src/event_processor_with_jsonl_output.rs#L142-L312), [ignored hook events](https://github.com/openai/codex/blob/e2eea071405a4d312ca9eabeed91b7e7cb9685c3/codex-rs/exec/src/event_processor_with_jsonl_output.rs#L462-L496))

Therefore, `exec --json` is useful evidence for a compact automation format, but it is not a sufficient source of truth for GUI/CLI behavioral parity.

## Claude Code

### Agent SDK is the supported application boundary

The TypeScript Agent SDK's `query()` accepts either a prompt string or an async iterable of user messages and returns an async stream of `SDKMessage`. The returned `Query` also exposes control methods including interrupt, model and permission-mode changes, rewind, MCP management, task stopping, context usage, and continued input streaming. ([`query()`](https://code.claude.com/docs/en/agent-sdk/typescript#query), [`Query` controls](https://code.claude.com/docs/en/agent-sdk/typescript#query-object))

`SDKMessage` is a broad discriminated union. It includes complete assistant and user messages, terminal results, initialization, raw partial stream events, status and compaction events, hook lifecycle, tool progress, task/background-task state, command changes, permission denial, rate limiting, retry, and conversation reset. ([message union](https://code.claude.com/docs/en/agent-sdk/typescript#sdkmessage))

The initialization message reports discoverable state such as tools, MCP servers, model, permission mode, slash commands, skills, plugins, and capabilities. Capabilities are an open set, so clients are instructed to feature-detect and ignore unknown values. ([system initialization](https://code.claude.com/docs/en/agent-sdk/typescript#sdksystemmessage))

Complete assistant and user messages carry session and parent-tool-use identifiers. Partial assistant events expose raw Anthropic streaming events only when requested and cover the main session; complete messages, optionally with forwarded subagent text, are required to represent subagent output. ([assistant messages](https://code.claude.com/docs/en/agent-sdk/typescript#sdkassistantmessage), [partial messages](https://code.claude.com/docs/en/agent-sdk/typescript#sdkpartialassistantmessage), [headless subagent behavior](https://code.claude.com/docs/en/headless#stream-responses))

Terminal result messages distinguish success from several failure classes and include duration, stop reason, permission denials, and usage. In streaming sessions, `modelUsage` includes subagents and internal calls and is cumulative, so consumers should read the latest value rather than sum successive results. ([result messages and usage](https://code.claude.com/docs/en/agent-sdk/typescript#sdkresultmessage))

Hooks are first-class events with correlated start, progress, and response messages. The documented hook surface spans tool, prompt, session, subagent, compaction, permission, task, and other lifecycle points. ([hook messages](https://code.claude.com/docs/en/agent-sdk/typescript#sdkhookstartedmessage), [hook event catalog](https://code.claude.com/docs/en/agent-sdk/typescript#hookevent))

Some updates are explicitly full snapshots rather than patches. For example, `background_tasks_changed` replaces the cached live-task set, and `commands_changed` replaces the cached command list. ([background-task snapshots](https://code.claude.com/docs/en/agent-sdk/typescript#sdkbackgroundtaskschangedmessage), [command snapshots](https://code.claude.com/docs/en/agent-sdk/typescript#sdkcommandschangedmessage))

### Stream JSON and control messages

The headless CLI can emit one JSON event per line with `--output-format stream-json`; partial messages and hook events are opt-in flags, and the final line is a result event. Anthropic warns consumers to drain stdout promptly because backpressure can terminate a slow consumer. ([streaming output](https://code.claude.com/docs/en/headless#stream-responses), [CLI flags](https://code.claude.com/docs/en/cli-reference#cli-flags))

The official Python SDK implementation launches the CLI with verbose `stream-json` output and `stream-json` input, then parses stdout as newline-delimited JSON with a bounded line buffer. ([CLI invocation](https://github.com/anthropics/claude-agent-sdk-python/blob/90ab9578f92ea6ca49616bcbc22ac6f9a32724df/src/claude_agent_sdk/_internal/transport/subprocess_cli.py#L562-L566), [stream input flag](https://github.com/anthropics/claude-agent-sdk-python/blob/90ab9578f92ea6ca49616bcbc22ac6f9a32724df/src/claude_agent_sdk/_internal/transport/subprocess_cli.py#L781-L783), [NDJSON reader](https://github.com/anthropics/claude-agent-sdk-python/blob/90ab9578f92ea6ca49616bcbc22ac6f9a32724df/src/claude_agent_sdk/_internal/transport/subprocess_cli.py#L1081-L1127))

That implementation multiplexes ordinary SDK messages with `control_request`, `control_response`, and cancellation messages. Requests are correlated by request ID; server-originated permission, hook, and MCP requests must receive matching responses while stdin remains open. ([message demultiplexing](https://github.com/anthropics/claude-agent-sdk-python/blob/90ab9578f92ea6ca49616bcbc22ac6f9a32724df/src/claude_agent_sdk/_internal/query.py#L308-L346), [control request correlation](https://github.com/anthropics/claude-agent-sdk-python/blob/90ab9578f92ea6ca49616bcbc22ac6f9a32724df/src/claude_agent_sdk/_internal/query.py#L595-L640), [bidirectional callback requirement](https://github.com/anthropics/claude-agent-sdk-python/blob/90ab9578f92ea6ca49616bcbc22ac6f9a32724df/src/claude_agent_sdk/_internal/query.py#L960-L970))

Initialization and reconnection can surface pending permission requests; callback handling should be idempotent by request ID. Interrupt acknowledgements can also report requests that remain queued. ([initialize response](https://code.claude.com/docs/en/agent-sdk/typescript#sdkcontrolinitializeresponse), [interrupt response](https://code.claude.com/docs/en/agent-sdk/typescript#sdkcontrolinterruptresponse))

These raw envelopes are useful implementation evidence, but they are below the public SDK abstraction. A direct CLI integration should pin a Claude Code/SDK version and contract-test observed frames rather than assume this transport is permanently stable.

## Lessons for bingo

1. **Define one parity-grade engine protocol.** Both terminal and GUI clients should consume the same complete domain events and controls. Any human-readable or compact JSONL output should be derived from it, with omissions documented.
2. **Keep three interaction classes distinct.** Model client requests/replies, server notifications, and server-initiated requests/replies explicitly. Approvals, user questions, hooks, and dynamic tools cannot be faithfully represented as notifications alone.
3. **Use stable correlation and hierarchy.** Every session/thread, turn, item, request, and client submission needs an ID; nested agents or tools need an explicit parent ID. This supports concurrent work, deduplication, replay, and targeted cancellation.
4. **Standardize lifecycle semantics.** Prefer `started -> delta/update -> completed/failed`, with the terminal object authoritative. State whether an update appends, patches, or replaces a snapshot; do not make clients infer that from payload shape.
5. **Negotiate capabilities during initialization.** Return current tools, commands, models, modes, integrations, and supported optional features. Clients should ignore unknown additive fields and gate experimental operations explicitly.
6. **Treat interactive controls as part of parity.** Interrupt, steer/input, permission response, model/mode change, session resume/rewind, MCP state, tasks, authentication, hooks, and context usage belong in the protocol, not in terminal-only shortcuts.
7. **Make server requests recoverable.** Give each request an idempotency key, scope, optional deadline, cancellation/resolution event, and reconnect behavior. Re-send or enumerate pending requests after reconnection.
8. **Separate durable history from live state.** Persist canonical completed items. Mark progress, partial text, hook progress, queue state, and similar signals as ephemeral; provide replacement snapshots for reconnectable live collections.
9. **Bound and isolate the transport.** Use one frame per line or an equivalently explicit framing rule, cap frame/buffer sizes, apply backpressure, and reserve stderr for diagnostics so protocol output remains parseable.
10. **Generate and test the boundary.** Maintain a serde-owned contract, generate JSON Schema for clients, keep fixtures for every variant, and run the same scenario traces against terminal and GUI adapters. Compatibility tests should cover unknown fields, duplicate frames, reconnects, delayed approvals, cancellation, and truncated streams.

The central design inference is simple: protocol completeness should be measured against every user-observable CLI state and action, not against the events that are convenient to print. Codex app-server demonstrates the complete boundary; Codex exec and Claude headless output demonstrate useful projections and transports.
