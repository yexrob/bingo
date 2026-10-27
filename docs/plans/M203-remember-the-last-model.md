# M203 — Remember the last model

## Goal

A successful root-session `/model` choice is the next startup's fallback,
without rewriting settings or remembering one-run CLI overrides (ADR-0099).

## Bricks

- A versioned provider/model JSON pair with tolerant reads and unique atomic writes.
- Whole-pair precedence: CLI > explicit settings > project local > project >
  remembered pair > user settings; other keys keep their existing merge.
- Record only after the core command successfully switches a root session.

## Files

- `bingo-core`: model selection record, host startup/choice, core model command, tests.
- Model picker copy and CLI regression tests.
- ADR-0099 amends ADR-0085 §1; no new dependencies or SDK doors.

## Exit criteria

- [x] Record contract, restart, precedence, ignored invalid state and write safety tests.
- [x] Successful commands alone remember; children, reconfigure, resume and CLI do not.
- [x] `--keep` still writes settings; ordinary choices preserve settings bytes.
- [x] Formatting, relevant tests/checks and repository checks run; failures recorded.
- [ ] Full workspace acceptance (environment/broader test failures below).

## Non-goals

Remembering thinking; changing running-session defaults; provider network probes;
settings migration; new flags, dependencies or public APIs.

## Risks

Unknown model IDs are deliberately supported; offline startup can reject missing
or unsigned-in providers, not determine whether an unlisted model still exists
on its server. Concurrent successful writers use atomic last-writer-wins pairs.

## Verified

Windows GNU, Rust 1.99.1; Winlibs compiler first on PATH. Cargo/rustup homes,
temporary files and build artifacts were explicitly kept under `D:/Apps/Bingo`.
No new dependencies or live-install changes.

```text
cargo fmt --all -- --check
exit 0
cargo check --workspace --all-targets --locked
Finished `dev` profile ... in 19.82s (exit 0)
cargo clippy --workspace --all-targets --locked -- -D warnings
Finished `dev` profile ... in 18.19s (exit 0)
cargo test -p bingo-core -p bingo-mod-models --locked --quiet -- --test-threads=2
733 passed; 0 failed (core); 23 passed; 0 failed (model picker)
cargo test -p bingo-surface-tui --locked --quiet model_picker -- --test-threads=1
4 passed; 0 failed
cargo test -p bingo --test pty --locked -- --test-threads=1
8 passed; 0 failed
bash scripts/check_discipline.sh
... discipline ok (exit 0; existing advisory/size warnings)
```

The discipline script used bundled Python with `-X utf8 -B` through an exported
`python3` Bash function. Initial missing GNU assembler/dlltool failures were
resolved by the parent-provided D-drive Winlibs installation before tests ran.

Additional runs, initial failures and broader checks:

- `cargo test -p bingo --test cli --locked models::`: 12 passed.
- `cargo test --workspace --locked`: stopped at CLI, 289 passed / 4 ACP timeouts.
  All 41 ACP tests subsequently passed with `--test-threads=1`.
- `cargo test --workspace --locked -- --test-threads=2`: binary 24 passed,
  CLI 293 passed, kernel 33 passed; plugin-kit 33 passed / 1 failed. The
  `wordcount` fixture's native subprocess invokes unavailable `python3`; its
  initialization ended. A shell function does not provide that native command.
- `cargo test -p bingo --test rpc --locked -- --test-threads=2`: 99 passed /
  1 MCP fixture timeout during a nested 72-second initial build. All model-picker
  and sign-in tests passed. The sole MCP test passed on a warm targeted retry
  (`rpc::an_mcp_server_from_mcp_config_offers_its_tool_through_the_gate`: 1 passed).
- The entire `bingo-surface-tui` suite was attempted with two test threads. Its
  process eventually crashed with `0xc0000005 STATUS_ACCESS_VIOLATION` during
  the Ember sweep; it did not finish. The focused model snapshots and PTY pass.
- `bash scripts/budget.sh`: exit 1, Windows normal dependency count 432 exceeds
  budget 423; no manifests or lockfile changed. Warm core check 2s (max 20s),
  touching TUI recompiles 0 crates. Its artifact counter assumes local `target/`
  and reports 0 because artifacts intentionally live in the external D directory.
- Initial `cargo deny check` was unavailable. The parent's portable cargo-deny
  check then failed identically on this branch and clean `origin/dev`:
  RUSTSEC-2026-0386 (AsyncFd I/O safety); bans/licenses/sources pass. Logs:
  `D:/Apps/Bingo/temp/model-deny.log` and `baseline-deny.log`. No dependencies or
  advisory exceptions were changed.

Final controlled workspace run after the parent's native `python3` shim and warm
builds (`cargo test --workspace --locked -- --test-threads=2`, exit 101):

```text
binary: 24 passed; CLI: 293 passed; kernel: 33 passed; plugin-kit: 34 passed
PTY: 8 passed; RPC: 100 passed; session inspection: 4 passed
bingo-auth-oauth: 121 passed; 1 failed
challenge::tests::a_server_nothing_is_listening_on_is_a_transport_failure
challenge.rs:267: unexpected Ok(Ok) instead of a transport failure
```

Log: `D:/Apps/Bingo/temp/model-workspace-final.log`. The earlier Python and RPC
fixture failures are resolved; this unrelated OAuth test stops later workspace
crates. Focused final acceptance log (all pass):
`D:/Apps/Bingo/temp/model-focused-final.log`; discipline:
`D:/Apps/Bingo/temp/model-discipline-final.log`. The parent also reproduced the
432-dependency Windows budget and the same exact OAuth failure on clean
`origin/dev` (the isolated test also returns `Ok(Ok)`). Independent final checks
also pass: core 733 tests, picker 23 tests, and `cargo check -p bingo-core --lib
--locked` for `x86_64-unknown-linux-gnu` and `aarch64-apple-darwin` (compile checks,
not native runtime tests).

Full acceptance is not claimed; broader failures remain for review.
Startup tests also cover late provider publication, in-flight pair consistency,
default-scope preservation and selections made inside plugin startup. A boot
candidate is read once; root choices during startup affect only the next process.
