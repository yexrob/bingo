# AGENTS.md — bingo

An agent CLI implemented in Rust (a local agent harness).

> Architecture and selection decisions live in [`notes/research.md`](./notes/research.md) (decision records D1-D64, with superseded decisions retained as history); check the latest applicable decision before changing the architecture.

## Language and style

- Use the Rust 2024 edition; use thiserror for error handling, avoid unwrap/expect (except in tests and unreachable code).
- Write code the way the surrounding code is written; prefer no comments, self-documenting names; comments explain only the "why".
- Don't add unneeded dependencies; before reinventing the wheel, check whether a mature wheel already exists on crates.io.
- Newly added or modified model-facing text must be English: tool input schemas (`#[schemars(description = …)]`), tool `description()` text, model prompts (compaction, memory extraction, etc.), and documentation intended for model consumption.
- Newly added or modified code comments and English documentation (`notes/`, `README.md`, and AGENTS.md itself) must be English.
- User-facing UI copy, error messages, and test data/assertion messages must be English.
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

- Every change must pass `cargo fmt --all -- --check`, `cargo check --locked --all-targets`, `cargo clippy --locked --all-targets -- -D warnings`, and `cargo test --locked --all-targets`; related behavior needs focused regression tests.
- User-visible CLI behavior must include black-box coverage for process exit status and stdout/stderr contracts when practical.
- Release tags must match the Cargo package version exactly; packaged archives must be unpacked and their binary version smoke-tested before publication.
- Unverified work is not called complete; failures are presented as-is.

## Committing

- Conventional Commits, imperative mood, short. Write the body and issue footnotes only when they carry real information.
- Write commit messages in English.
- Commit only what the user asked for; never commit secrets.
- Every commit made by or on behalf of bingo appends the co-author trailer `Co-authored-by: bingo <EMAIL>`
  so GitHub renders bingo's avatar next to the author. EMAIL is the dedicated bingo GitHub account's
  `<id>+bingo@users.noreply.github.com` address — register that account, upload the avatar, then use its
  noreply email here (single source of truth for the trailer).

## Forbidden

1. Using unsafe
2. unwrap or expect; every error case must be handled
