# AGENTS.md — bingo

An agent CLI implemented in Rust (a local agent harness).

> Architecture and selection decisions live in [`notes/research.md`](./notes/research.md) (decision records D1-D24); check against it before changing the architecture.

## Language and style

- Use the Rust 2024 edition; use thiserror for error handling, avoid unwrap/expect (except in tests and unreachable code).
- Write code the way the surrounding code is written; prefer no comments, self-documenting names; comments explain only the "why".
- Don't add unneeded dependencies; before reinventing the wheel, check whether a mature wheel already exists on crates.io.
- English for everything the model side sees: code comments (//, ///, //!), documentation (notes/, README.md, AGENTS.md itself), tool input schemas (`#[schemars(description = …)]`), tool `description()` text, and model prompts (compaction, memory extraction, etc.).
- Chinese is kept only on the user side: UI copy, error messages, and test data/assertion messages. Keep this split when touching existing strings: translate model-side strings, leave user-side strings as they are.
- `README.zh-CN.md` stays as the Chinese-language documentation entry point.

## Architecture rules

- Core layering follows agent-harness conventions: the Tool protocol (the Zod equivalent, i.e. serde schema), a unified permission gate, a streaming main loop, and Hooks extension points. Do not treat intent-layer problems with code-layer medicine.
- Default to subtracting: delete code, dependencies, and features that can be deleted. Adding things requires a reason.
- When a boundary is consumed independently (public API, cross-process protocol, persistence format), define the contract first (trait/serde schema) and have all implementations check against the same table; internal refactors don't establish contracts.

## Built-in skills sync

- When a change touches bingo's user-visible behavior (config options / slash commands / tools / error messages / capability map),
  check whether the built-in skills in `src/skills/bundled/` describe that behavior; if so, update them in the same batch
  (currently guide.md: config table, examples, command quick reference, diagnostic guide, capability map).
- When a change touches user-visible feedback states (loading / error hints / toast behaviors and output formats),
  cross-check against [`notes/design/feedback-states.md`](./notes/design/feedback-states.md) (the feedback-states specification),
  stay consistent, and backfill that document's changelog.

## Verification

- Run `cargo build` and `cargo clippy -- -D warnings` for every change; related logic must carry tests (`cargo test`).
- Unverified work is not called complete; failures are presented as-is.

## Committing

- Conventional Commits, imperative mood, short. Write the body and issue footnotes only when they carry real information.
- Write commit messages in English.
- Commit only what the user asked for; never commit secrets.

## Forbidden

1. Using unsafe
2. unwrap or expect; every error case must be handled
