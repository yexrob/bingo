# Design documents

These are the sources the ADRs draw from. They were written before the first line of code and are archived verbatim; ADRs and plans are authoritative where they disagree.

- [plan.md](plan.md) — the approved rewrite plan (Chinese): survey, architecture, milestones, verification, conventions, porting checklist.
- [architecture.html](architecture.html) — the architecture page (seven figures: layers, event hub, one turn, turn state machine, session = journal, crate graph, roadmap).
- [kernel-and-sdk.md](kernel-and-sdk.md) — kernel boundary, plugin mechanism, the stable trait set, the event model, the turn state machine, crate layout, where each old feature lands.
- [gateway-and-surfaces.md](gateway-and-surfaces.md) — invariants, session and addressing, the client contract and wire protocol, how each surface is a client, multi-client and durability, ecosystem paths, interactions.
- [delivery.md](delivery.md) — milestones, porting-knowledge checklist with old-code pointers, test and verification strategy, repo conventions, risk register, first session.
- `survey/` — what the old bingo had and where it hurt: [feature inventory](survey/feature-inventory.md), [engine map](survey/engine-map.md), [collaboration layer, protocol and health](survey/collab-protocol-health.md).
- `research/` — verified library and reference-implementation research: [Rust crates](research/rust-crates.md), [TUI ecosystem](research/tui-ecosystem.md), [reference architectures](research/reference-architectures.md), [provider stream model](research/provider-stream-model.md).
