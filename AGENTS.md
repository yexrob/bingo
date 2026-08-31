# AGENTS.md — bingo

A local coding-agent harness in Rust: a minimal kernel, everything else a plugin, one ordered event stream that every surface (TUI, `--print`, JSON-RPC, ACP, IM channels) consumes as a client. Map: `ARCHITECTURE.md` (crate map), `docs/adr/` (boundary decisions), `docs/plans/` (one plan per milestone), `docs/design/` (the design and research the plan came from).

## Language and style

- Rust 2024, `thiserror` for library errors, `anyhow` only at the binary edge. `unwrap`/`expect` are lint errors outside tests; `unsafe` is forbidden.
- Write code the way the surrounding code is written. Names carry meaning; comments say only *why*.
- **One responsibility per function, one per module.** A function does one thing at one level of abstraction: a match arm that grows a body becomes a function, a loop body that decides and acts becomes two. A module owns one noun; when it owns two, split it. Split eagerly — a small function with a good name costs nothing, a long one hides its second job. `scripts/check_discipline.sh` warns at 60 lines per function and fails at 120.
- Model-facing text, UI copy, docs, tests and commit messages are English.
- No new dependency without a line in the ADR or plan that justifies it and a `scripts/budget.sh` run. `cargo deny check` must pass.

## Architecture rules

- Layering `sdk ← core ← plugins/surfaces ← bin`. **The kernel never imports a plugin. No plugin imports another plugin** except through a service trait registered via the sdk. No crate but the TUI surface depends on ratatui/crossterm. `scripts/check_discipline.sh` asserts these (ADR-0001).
- **One event stream.** `bingo_sdk::Event` is the only event type. Surfaces are clients: they fold frames with `SessionState::apply` and derive their views at render time. No private mirror enums (ADR-0002).
- **One fact, one representation.** Never carry a value alongside the thing it derives from. If a fix requires "remember to update it everywhere", it is debt, not a fix; prefer the change that makes the mistake unrepresentable.
- **Contracts first** for anything consumed independently: a trait, a wire format, a persisted record gets its fixture or schema test before its implementation.
- **Bricks first.** Pure function → primitive → component → feature. A feature without a pure brick underneath is suspect.
- **Subtract by default.** Deleting needs no reason; adding does.
- Tool properties fail closed: an unknown tool is not concurrency-safe, not read-only, and its interrupt behaviour is Block.
- The kernel owns no feature nouns. `room`, `team`, `hire`, `task`, `experience` do not appear in `bingo-sdk` or `bingo-core`.

## Verification

- Every change passes `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `scripts/check_discipline.sh`, `scripts/budget.sh`.
- User-visible CLI/RPC behaviour has black-box coverage (exit status, stdout purity, NDJSON validity). Terminal-byte changes have a `TestBackend` test and the PTY smoke.
- A milestone is done when its plan's exit criteria are ticked with command output pasted. Unverified work is not called complete; failures are reported as they are.

## Records

- ADR: one per boundary decision, ≤120 lines, template in `docs/adr/README.md`. Bug fixes are commit bodies.
- Plan: `docs/plans/M<n>-<slug>.md`, ≤150 lines, written before code: Goal / Bricks / Files / Exit criteria / Non-goals / Risks; a Verified section is appended at the end.

## Commits

Conventional Commits, imperative, subject ≤60 characters, English, no literary titles. Scopes are crate short names: `sdk core provider-fake provider-anthropic provider-openai tool-fs tool-bash tool-web print rpc tui permissions hooks store context skills mcp agents teams rooms tasks demo-ui experience plugin-rpc schedule acp channels bin ci docs adr`. Body only when it carries information; footers `Refs: ADR-0002`, `Plan: M0`.

## Forbidden

`unsafe`; `unwrap`/`expect` outside tests; a surface holding session state; a plugin importing another plugin or the kernel; a second representation of an existing fact; a feature noun in the kernel.
