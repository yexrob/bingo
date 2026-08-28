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

- [x] `cargo fmt --all -- --check`
- [x] `cargo check --workspace --all-targets --locked`
- [x] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [x] `cargo test --workspace --locked`
- [x] `scripts/check_discipline.sh` and `scripts/budget.sh` (records the baseline)
- [x] `cargo deny check`
- [x] Loop tests: text-only turn; tool round; interrupt mid-stream keeps text, drops unpaired tool_use, fills orphan results; in-stream retry restarts the response; empty-response retry once; turn budget stops a runaway loop.
- [x] Reducer tests: every `Event` variant has a JSON round-trip fixture; `apply` over a scripted frame list reproduces the expected `SessionState`.
- [x] Black-box: `bingo --print --provider fake "hello"` prints only prose to stdout; `--output-format json` is one valid `Frame` per line; non-TTY errors are `[error] code=… msg=…` with exit 1.

## Non-goals

Real providers, persistence on disk, permissions beyond the fail-closed default, TUI, MCP, hooks, sub-agents.

## Risks touched

R1 (trait churn — the sdk changes freely until M2), R8 (the three design proposals disagree on names; ADR-0001/0002 are the reconciliation).

## Verified (2026-08-29, commit 574710d)

```
$ cargo fmt --all -- --check                                  exit 0
$ cargo check --workspace --all-targets --locked              exit 0
$ cargo clippy --workspace --all-targets --locked -- -D warnings   exit 0
$ cargo test --workspace --locked                             exit 0
  bingo (tests/cli.rs) 6 · bingo-core 44 · bingo-provider-fake 19 · bingo-sdk 15
  bingo-surface-print 31 · bingo-tool-fs 12                   = 127 passed
$ scripts/check_discipline.sh                                 exit 0
  warn session.rs 764, host.rs 783, turn.rs 773 non-test lines (>700); cohesion ok
$ scripts/budget.sh                                           exit 0
  dependencies (unique, normal): 75 (max 260)                 ← M0 baseline
$ cargo deny check                                            advisories ok, bans ok, licenses ok, sources ok
$ bingo --print --provider fake hello
Hello from the fake provider.
$ bingo --print --output-format json --provider fake hello
{"seq":2,…,"event":{"type":"itemCompleted",…}}   … one Frame per line, seq 2..12, last turnCompleted
```

Exit criteria, item by item:

- Loop tests (`bingo-core::turn::tests`, 11): text-only turn; tool round; interrupt mid-stream keeps text, drops the unpaired tool_use, fills the orphan result; in-stream retry restarts the response; empty-response retry once; `TurnBudget` stops a runaway loop.
- Actor tests (`bingo-core::session::tests`, 9): submit opens a turn; a busy session queues and the queue opens the next turn; a permission is answered once by id and late answers are `INTERACTION_CLOSED`; a lagging subscriber gets `Lagged` and resyncs from its last applied seq; the replay is the durable journal only and folds to the same view; close ends the journal; a panicking turn is `TURN_LOST`, not a hang.
- Host tests (`bingo-core::host::tests`, 5): load order and unmet requirements disable without crashing; a second policy is a conflict; create/find/delete with gateway events; sub-sessions carry a parent and hit the depth limit.
- Reducer: `bingo-sdk::event::tests::every_event_variant_has_a_pinned_wire_form` (insta snapshot); `state::tests` cover deltas until completion, stale frames, interactions, rewind, and that a `Lagged` marker leaves `seq` alone.
- Black-box (`crates/bingo/tests/cli.rs`, 6): stdout is prose only; `--output-format json` is one valid `Frame` per line ending in `turnCompleted`; a tool round runs `Read` and reports on stderr; a failed turn is one `[error] code=… msg=…` line with exit 1; no prompt / unknown provider fail before any turn.

Deviations from the brick list, recorded rather than hidden:

- No `bingo-sdk::testing` module yet. The scripted provider, echo tool and no-op tool host live in `bingo-core::test_support`; `bingo-provider-fake` is the demo provider. The sdk module moves in when a second plugin crate needs the fakes (M1).
- `bingo-core::turn` is an async driver with explicit phases, not a pure `step(input) -> effects` function. The loop is tested through the scripted provider instead; revisit if a second driver (replay, dry-run) ever needs the pure form.
- `bingo-provider-fake` steps are `Text | Reasoning | ToolCall | Error | Delay`; `Overflow` is `Error(ContextOverflow)` and `Hang` is a long `Delay`. No loopback SSE server yet (M1).
- The interaction guard (400 ms) and the queue preview are kernel constants in `session.rs`; the `TurnBudget` defaults in `turn.rs`.
