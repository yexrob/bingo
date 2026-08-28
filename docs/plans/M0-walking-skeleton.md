# M0 — Walking skeleton

## Goal

`bingo --print --provider fake "hi"` streams a scripted reply through the real turn loop, executes one `Read` tool round, and `--output-format json` emits the canonical frame stream one per line.

## Bricks, in build order

1. `bingo-sdk::ids` — `SessionId`, `TurnId`, `ItemId`, `InteractionId`, `IntentId`, `Seq`; ULID-backed, serde as strings.
2. `bingo-sdk::model` — `Message`, `Role`, `ContentPart` (text, image, tool_use, tool_result, reasoning; `provider_metadata`), `SystemBlock`, `ModelRequest`, `ModelEvent` (AI-SDK-V4-shaped), `Usage`, `FinishReason`, `ModelCapabilities`, `Effort`.
3. `bingo-sdk::event` — `Frame`, `Event`, `Item`, `ItemBody`, `ItemStatus`, `Interaction`, `Answer`, `IntentOutcome`, `QueueEntry`, `SessionSummary`; durable/ephemeral classification.
4. `bingo-sdk::state` — `SessionState` + `apply(&Frame) -> Applied`, the one reducer.
5. `bingo-sdk::plugin` — `Plugin`, `PluginManifest`, `Registrar`, `Contribution`; `bingo-sdk::traits` — `Provider`, `Tool`, `ToolTraits`, `Subject`, `ToolContext`, `ToolOutput`, `PermissionPolicy`, `Hook`, `ContextContributor`, `Command`, `Surface`, `SessionStore`, `Compactor`; `bingo-sdk::host` — `HostApi`, `Attachment`, `SessionHandle`, `Input`, `Origin`, `SessionSelector`.
6. `bingo-sdk::testing` (feature) — `ScriptedProvider`, `RecordingSurface`, fixture helpers.
7. `bingo-provider-fake` — `Step = Text | Events | ToolCall | Error | Overflow | Hang`; records every request; validates request shape like an API would.
8. `bingo-core::accumulator` — folds `ModelEvent` into `Item`s; the finish rules from `docs/design/delivery.md` §3 M0.
9. `bingo-core::executor` — consecutive concurrency-safe calls in parallel (≤10), the rest serial; cancel keeps completed results; every tool_use gets a tool_result.
10. `bingo-core::turn` — the state machine as pure `step(input) -> effects` plus a driver; retry ladder; empty-response retry; max-tokens continuation; interrupt markers.
11. `bingo-core::session` — actor: seq mint, journal (in memory for M0), broadcast with bounded channels + `Lagged`, `SessionState` maintained by the same reducer, interaction/queue registries; `ContextView::fold`.
12. `bingo-core::host` — plugin registry, capability ordering, services, config claims; `HostApi` impl; permission gate with a fail-closed default policy.
13. `bingo-tool-fs::read` — `Read` only (20K char cap, line ranges, image files as image parts).
14. `bingo-surface-print` — `--print` (stdout prose only, everything else stderr, `[error] code=… msg=…` on non-TTY) and `--output-format json` (one `Frame` per line).
15. `bingo` — clap, plugin composition, surface selection.

## Files

`crates/bingo-sdk/src/{ids,model,event,state,plugin,traits,host,testing}.rs`, `crates/bingo-sdk/fixtures/*.json`, `crates/bingo-core/src/{accumulator,executor,turn,session,journal,host,gate,context}.rs`, `crates/bingo-provider-fake/src/lib.rs`, `crates/bingo-tool-fs/src/{lib,read}.rs`, `crates/bingo-surface-print/src/lib.rs`, `crates/bingo/src/main.rs`, `crates/bingo/tests/cli.rs`.

## Exit criteria

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo check --workspace --all-targets --locked`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `cargo test --workspace --locked`
- [ ] `scripts/check_discipline.sh` and `scripts/budget.sh` (records the baseline)
- [ ] `cargo deny check`
- [ ] Loop tests: text-only turn; tool round; interrupt mid-stream keeps text, drops unpaired tool_use, fills orphan results; in-stream retry restarts the response; empty-response retry once; turn budget stops a runaway loop.
- [ ] Reducer tests: every `Event` variant has a JSON round-trip fixture; `apply` over a scripted frame list reproduces the expected `SessionState`.
- [ ] Black-box: `bingo --print --provider fake "hello"` prints only prose to stdout; `--output-format json` is one valid `Frame` per line; non-TTY errors are `[error] code=… msg=…` with exit 1.

## Non-goals

Real providers, persistence on disk, permissions beyond the fail-closed default, TUI, MCP, hooks, sub-agents.

## Risks touched

R1 (trait churn — the sdk changes freely until M2), R8 (the three design proposals disagree on names; ADR-0001/0002 are the reconciliation).
