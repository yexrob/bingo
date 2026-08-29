# M5 — The RPC surface: the GUI contract, the event hub on the wire

## Goal

`bingo serve --stdio` serves sessions over JSON-RPC to any process: open, snapshot, an unbroken seq of events, submit/interrupt/answer with client-minted intents, resync after a lag, the catalogue — thirteen methods that are `HostApi` and `SessionPort` with an envelope, a schema committed and guarded against drift. `bingo --print --output-format stream-json` speaks Claude Code's envelope so a host that already drives that CLI drives bingo without a plugin. A remote kernel proves a surface can run on the far side of the wire.

## Bricks, in build order (owner)

1. **ADR-0007** (kernel) — the wire is decided before the crate: transport, the thirteen methods, verbatim events, write-returns-nothing, the error mapping, the committed schema, `RemoteKernel`, the compatibility encoder. No sdk change is planned; a gap a worker reports is fixed on main and listed here.
2. **`bingo-surface-rpc`** (worker A) — `codec`: NDJSON framing over `tokio_util::codec::LinesCodec`, JSON-RPC request/response/notification envelopes with `serde_json::Value` params and ids (number or string); `server`: a `Surface` (`id: "rpc"`, `Exclusive`) that reads requests from stdin, dispatches them against `HostHandle` on one task, writes responses and notifications through one writer so ordering holds, keeps one forwarding task per opened session (an `Attachment` stream → `event` notifications; `session/close` or reopening drops it; `session/events` replaces it with `events_since`), a `gateway/subscribe` forwarder, `initialize`-first with `NOT_INITIALIZED`, `shutdown` that ends the loop after the response is flushed; unknown methods and bad params as -32601/-32602; `KernelError` as -32000 with `data.code`; `client`: `RemoteKernel` implementing `HostApi` + a `RemoteSession` implementing `SessionPort` over any `AsyncRead + AsyncWrite` pair (a spawned child's stdio in the tests), correlating responses by id and routing `event` notifications to the open session's stream; `schema`: `document()` → the JSON Schema with `$defs` (schemars `SchemaGenerator` over `Frame`, `SessionState`, `SessionSummary`, `Input`, `Answer`, `Activation`, `InterruptScope`, `SessionSelector`, `SessionFilter`, `HistoryPage`, `HistoryChunk`, `Catalog`, `CatalogKind`, `GatewayEvent`, `ClientIdentity`, `ErrorCode`, and the params/result structs), `methods` and `notifications` tables; `schema/rpc.json` at the repo root, a drift test that regenerates and diffs (update with `BINGO_UPDATE_SCHEMA=1 cargo test -p bingo-surface-rpc`), and a test that walks every `properties` key and asserts camelCase.
3. **stream-json** (worker B, in `bingo-surface-print`) — `Mode::StreamJson` for `--output-format stream-json`: `{"type":"system","subtype":"init",…}` at open with `session_id`, `cwd`, `tools`, `model`; `{"type":"assistant","message":{…"content":[text | tool_use]},"session_id","parent_tool_use_id":null}` per completed assistant text and per started tool call; `{"type":"user","message":{"content":[tool_result]},…}` per completed tool call; `{"type":"result","subtype":"success"|"error_max_turns"|"error_during_execution","is_error","result","session_id","duration_ms","num_turns","usage"}` at `TurnCompleted`; stdout carries those lines only, stderr keeps the prose diagnostics; verified against the current Claude Code / Agent SDK documentation of the format, with the date recorded in the module doc.
4. **`bingo serve`** (kernel) — a clap subcommand `serve --stdio` beside the print flags; registers `RpcPlugin`; `SurfaceOptions.args = {"transport": "stdio"}`; exit code 0 on `shutdown`, 1 on a broken pipe.
5. **`tests/rpc.rs`** (kernel) — the second and last integration binary: `RemoteKernel` over a spawned `bingo serve --stdio` with the fake provider; scenarios: a method before `initialize` is `NOT_INITIALIZED`; an unknown method is -32601; stdout is JSON-RPC lines only across a whole turn; open → submit → events to `TurnCompleted`, seq unbroken and the snapshot's seq first; `IntentAck` carries the client's intent; an interrupt reaches a running turn (fake `Delay`) and the turn ends `Interrupted`; a permission `InteractionOpened` is answered through `session/answer` and the tool runs; a retry is visible as `TurnRetrying`; a session written by a `--print` run reopens `ById` with its items in the snapshot; `session/history` pages backwards; `catalog/read` lists the fake provider and the tools; `session/delete` removes the directory; `shutdown` ends the process with 0.

## Files

`docs/adr/0007-rpc-wire.md`, `crates/bingo-surface-rpc/src/{lib,codec,server,session,client,schema}.rs` + `tests/wire.rs`, `schema/rpc.json`, `crates/bingo-surface-print/src/{render,stream_json}.rs`, `crates/bingo/src/main.rs`, `crates/bingo/tests/rpc.rs`, `scripts/check_discipline.sh` (no `enum …Event` in a surface crate).

## Dependencies

None new: `tokio-util` (`codec`) is already a workspace dependency; the client and server share `serde_json`. The `agent-client-protocol` crate's JSON-RPC machinery was considered and passed over: it brings its own schema types and a runtime abstraction for ~400 lines of framing we can read in one sitting; ACP itself is M12.

## Exit criteria

- [x] `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `scripts/check_discipline.sh`, `scripts/budget.sh`, `cargo deny check`
- [x] Wire (unit, scripted `HostApi`): every method's params and result round-trip; `initialize` first; responses and notifications interleave in order; `session/open` then frames; `Lagged` → `session/events` resends from `since`; errors carry `data.code`; a parse error answers -32700 with `id: null`; `RemoteKernel` against the in-process server over a duplex pipe folds to the same `SessionState` as a direct attachment
- [x] Schema: `schema/rpc.json` matches `document()`; every property camelCase; every method in the table has both refs resolving to a `$defs` entry
- [x] stream-json: fixture frames → the exact lines (snapshot); a tool round is `assistant(tool_use)` then `user(tool_result)`; a failed turn is `result{is_error:true}`; stdout has no prose
- [x] Black-box: nine `tests/rpc.rs` scenarios covering the twelve listed facts; `--print --output-format stream-json` on a tool round parses line by line with `type` in `{system, assistant, user, result}`
- [x] sdk changed once, for two corrections the wire exposed (ADR-0007 consequences): `rename_all_fields` on the tagged enums, `HostApi::catalog` async and fallible; touched `bingo-core`, `bingo-surface-print`, `bingo-surface-rpc`

## Non-goals

WebSocket transport and a token file (with the first browser or multi-client GUI). The command dispatcher, `!` and `/compact` (M6, with the TUI that types them; `Input::Action` stays `Rejected` on the wire until then). `--input-format stream-json` (M6). ACP (M12). A post-turn hook point for the memory extractor and `HostHandle::env()` (with the next sdk change that needs one). The loopback SSE server in `provider-fake` (wiremock covers the two real adapters).

## Risks touched

R2 — the RPC client and the TUI to come share the bounded channel + snapshot + resync design; the harness proves it before any TUI code exists. R1 — no sdk change planned. Ordering on one writer is the whole correctness of the server; it is one task by construction.

## Verified (2026-08-29, commit 3c74314)

```
$ cargo fmt --all -- --check                                        exit 0
$ cargo check --workspace --all-targets --locked                    exit 0
$ cargo clippy --workspace --all-targets --locked -- -D warnings    exit 0
$ cargo test --workspace --locked                                   exit 0, three runs in a row
  bin (cli 26 + rpc 9) 35 · core 106 · sdk 19 · rpc 35 · print 50 · context 66 · store-jsonl 34 · provider-fake 19
  provider-anthropic 68 · provider-openai 95 · tool-fs 69 · tool-bash 51 · tool-web 77 · permissions 92 = 816 passed
$ scripts/check_discipline.sh                                       exit 0 (no warnings; surface crates hold no event mirror)
$ scripts/budget.sh                                                 dependencies 216 (max 260); no crate added
$ cargo deny check                                                  advisories ok, bans ok, licenses ok, sources ok
$ printf '…initialize…shutdown…' | bingo serve --stdio              NOT_INITIALIZED, the handshake, {}; exit 0, stderr empty
```

Exit criteria, item by item:

- Wire: `tests/wire` (rpc) drives `serve` over a duplex pair through raw JSON-RPC lines — every method round-trips, `initialize` first, the open response precedes the first event, `session/events` resends from `since`, `Lagged` is forwarded, -32700 with `id: null`, -32601, -32602, -32000 with `data.code`, `shutdown` ends `serve`; `RemoteKernel` against the in-process server folds to the scripted host's own state.
- Schema: `schema/rpc.json` (71 `$defs`, 13 methods, 2 notifications) equals `document()`; every property camelCase with no allowance; every method ref resolves.
- stream-json: four snapshots (text, tool round, failed, max-turns); every line typed; the error arm carries `errors`, not `result`, as the documented union says.
- Black-box `tests/rpc.rs` over a spawned `bingo serve --stdio`: refused before `initialize`; stdout is JSON-RPC lines only across a turn, -32601 and -32700 with `id: null`; seq unbroken from the snapshot with the client's intent acknowledged; an interrupt into a running `Delay` ends `Interrupted` within seconds; a `Write` permission answered over the wire runs the tool; a 529 is a visible `TurnRetrying`; a `--print` run's session reopens `ById` with its items and pages `history`; the catalogue lists `fake` and `Read`; `delete` removes the directory; `shutdown` exits 0. `tests/cli/stream_json.rs`: a tool round is `system, assistant, assistant, user, assistant, result`; a failed turn is `result{is_error, errors}` and exit 1.

Found while integrating (each is a commit body too):

- The wire is the sdk, so building it audited the sdk: variant fields were snake_case on the wire (`rename_all_fields`, recorded frames and snapshots regenerated, journal stays version 1 — none had left a developer's machine); `HostApi::catalog` was synchronous (`be1c582`).
- A kernel race the harness caught under `--workspace` load: the provider and the turn watch the same cancel token, and when the provider ended its stream first the turn read a completion; a cancelled token now makes the end an interruption, and a stream that ends without a finish otherwise is a retryable stream error (`3c74314`).
- The shared flags (`--cwd`, `--settings`, provider/model, permissions, `--max-turns`) are global so `bingo serve --stdio --cwd X` parses.
- `RemoteKernel` ignores `open`'s `who` (the `initialize` identity is the client) and `close`'s reason (a detach is a detach); both are said at the impl.

Open, carried forward:

- WebSocket transport, a token file and concurrent clients of one server — with the first browser or multi-client GUI.
- `--input-format stream-json`, the command dispatcher (`Input::Action` is still `Rejected` on the wire), `HostHandle::env()`, a post-turn hook point — M6 with the TUI.
- `SessionState` drops the finished `LiveTurn` at `TurnCompleted`, so the stream-json encoder keeps its own round count and start time; a `last_turn` carrying the finished `LiveTurn` would delete that state. `ConfigView` is never populated, so `permissionMode` in the init line is the constant `default`.
- Protocol mismatch is not refused at `initialize`; the result carries `protocol` and the client decides.
- Live smokes against Anthropic and OpenAI (M1, M2) — still need keys.
