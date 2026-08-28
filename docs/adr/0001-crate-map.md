# ADR-0001 — Crate map and dependency direction

Status: accepted (2026-08-29). Plan: M0. Source: `docs/design/kernel-and-sdk.md` §6, `docs/design/gateway-and-surfaces.md` §2.1, `docs/design/delivery.md` §2.

## Context

The previous bingo was one 148K-line binary crate. Its tool layer imported the TUI (`tool/team.rs` read the avatar table, `tool/diff.rs` used the TUI's diff parser), integration tests could only spawn the binary, and any edit relinked everything (13 GB `target/`). The seams between kernel, plugins and frontends existed in the design but never became compilation boundaries.

## Decision

A Cargo workspace with four layers and one direction of dependency:

```
bingo (bin)          → bingo-core + every plugin and surface crate
plugins / surfaces   → bingo-sdk only
bingo-core           → bingo-sdk only
bingo-sdk            → serde, serde_json, schemars, thiserror, async-trait, tokio(sync), tokio-util, futures, ulid, jiff — nothing heavier
```

Crate naming: `bingo-sdk`, `bingo-core`, `bingo-provider-<name>`, `bingo-tool-<name>`, `bingo-surface-<name>`, and feature crates by noun (`bingo-permissions`, `bingo-hooks-shell`, `bingo-store-jsonl`, `bingo-context`, `bingo-skills`, `bingo-mcp`, `bingo-agents`, `bingo-teams`, `bingo-rooms`, `bingo-tasks`, `bingo-acp`, `bingo-channels`).

Forbidden edges, asserted by `scripts/check_discipline.sh` over `cargo metadata`:

1. a plugin or surface crate depends on `bingo-core`;
2. `bingo-core` depends on a plugin or surface crate;
3. any crate other than `bingo-surface-tui` depends on `ratatui` or `crossterm`;
4. `bingo-core` or `bingo-sdk` resolve `reqwest`, `rmcp`, `ratatui`, `crossterm`, `image` or `syntect` anywhere in their normal dependency tree;
5. a plugin crate depends on another plugin crate. Cross-plugin needs go through a service trait registered via the sdk `Service` contribution; if a third consumer of a service trait appears, the trait moves to a small `bingo-services` crate.

The bin composes `Vec<Box<dyn Plugin>>` explicitly. No self-registration crate until plugins must load without the bin naming them.

## Consequences

- `bingo-core` tests run in-process against `bingo_sdk::testing` fakes; no subprocess is needed to test the kernel.
- Touching a tool crate never relinks the TUI; `touch crates/bingo-surface-tui/src/lib.rs && cargo check -p bingo-core` is a no-op (budget script asserts it).
- The sdk is what an external plugin author downloads; it cannot pull ratatui or reqwest.
- The TUI cannot reach the engine, so the four walls of the old D149–D152 migration cannot recur.
