# M3 — Sessions on disk: journal, resume, budget

## Goal

A session outlives the process. `bingo --print --continue "…"` and `--resume <id>` reopen the exact journal a previous run wrote and the next turn folds its context from it; two processes cannot own one session; old sessions are collected; a runaway turn stops on a named error. The context a resumed session sends is API-legal by construction, proved on random journals.

## Bricks, in build order (owner)

1. **ADR-0005 + sdk** (kernel) — the persisted format is a contract consumed independently (a person reads it, a migrator will), so it is decided first. `SessionStore` gains `acquire(session)` / `release(session)` with `Ok` defaults: the lock is the store's, the moment is the kernel's. `ErrorCode::Storage` for a disk that fails, distinct from a kernel bug. One sdk change.
2. **`bingo-store-jsonl`** (worker A) — `<data_dir>/sessions/<id>/journal.jsonl`: one header line `{"format":"bingo-journal","version":1,"session":…}` then one durable `Frame` per line in seq order, appended and flushed per frame; `replay(since)` drops a torn last line and refuses a corrupt middle one or a newer version with `Storage`; `.lock` sidecar taken with an advisory exclusive lock (`fs4`) on `acquire`, held until `release` or process exit, a second holder is `SessionLocked` — data files are never locked; `summary.json` is the latest `SessionSummary`, rewritten on `create` and on each `SessionUpdated` frame, `updated_at` stamped from the journal's mtime on `list`, rebuilt from the journal when missing (deleting every one loses nothing — a test); `list` sorts by `updated_at` descending, honours `cwd`, `parent`, `limit`; `delete` removes the directory; GC in `start()` at most once a day (`gc.stamp`): sessions untouched for 30 days, then the oldest beyond 100, never one that is locked. Plugin `bingo.store.jsonl`, `provides: ["store:jsonl"]`.
3. **Resume** (kernel) — `open(ById | ByKey | Latest{cwd})` for a session that is not live: `store.acquire` → `replay(ZERO)` → `session::resume(frames, …)` spawns the actor with the frames as its journal, `seq` at the last frame, the state folded by `SessionState::apply`, the generation from the state; the actor publishes a fresh `SessionUpdated` as the head of the new segment. The model is the stored summary's provider and model, capabilities re-resolved (ADR-0004); tools, policy, hooks are the running host's. `Latest{cwd}` and `sessions(filter)` read the store as well as the live set. `delete` releases and removes.
4. **Fold invariants** (kernel) — proptest over random item journals (user, assistant, reasoning, tool calls with and without output, interruptions, compaction): every `ToolUse` has exactly one `ToolResult` in the next user message; no message is empty; the first message is a user message; a resumed journal folds to the same messages as the live one. A golden `fixtures/journal-v1.jsonl` → `ContextView::items` → `insta` snapshot pins what version 1 means to the kernel.
5. **`bingo`** (kernel) — `--continue` (`Latest{cwd}`), `--resume <id>` (`ById`), `--max-turns N` (`TurnBudget.max_rounds`); register `JsonlStorePlugin`; `tests/cli.rs`: a second `--print --continue` run in the same `HOME`/cwd carries the first run's session id and continues its seq; `--resume` of an unknown id is `SESSION_NOT_FOUND`; a second process on a session another holds is `SESSION_LOCKED`; `--max-turns 1` on a tool loop exits 1 with `TURN_BUDGET_EXHAUSTED`.

Already in place from M0–M2 and not redone: `TurnBudget`, `Interrupt::{Cancel, Block}` in the executor, `--session-id` as an arbitrary `host/<key>`, a session's own cwd.

## Files

`docs/adr/0005-session-persistence.md`, `crates/bingo-sdk/src/{store,error}.rs`, `crates/bingo-store-jsonl/src/{lib,layout,journal,lock,summary,gc}.rs` + `fixtures/*.jsonl`, `crates/bingo-core/src/{session,host,context}.rs`, `crates/bingo-core/fixtures/journal-v1.jsonl`, `crates/bingo/src/main.rs`, `crates/bingo/tests/cli.rs`.

## Dependencies (verify on crates.io; `scripts/budget.sh` and `cargo deny check` after)

`fs4` — advisory file locks on every platform (the lock is the only thing the store needs an OS for). No sqlite: the list is small after GC and nothing searches it yet; the FTS index comes with the first consumer that searches (`/resume` in M6).

## Exit criteria

- [ ] `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `scripts/check_discipline.sh`, `scripts/budget.sh`, `cargo deny check`
- [ ] Store: fixtures for a clean journal, a torn last line (kept up to it), a corrupt middle line (`STORAGE` naming the line), a newer version (refused); two store instances on one directory — the second `acquire` is `SESSION_LOCKED`, `release` frees it; `summary.json` deleted → `list` still answers; GC removes the old and the surplus and skips the locked, and runs once a day
- [ ] Resume: a host on a shared store reopens a session another host created, the snapshot holds its items, the next request to the provider carries the old messages, seq continues; `Latest{cwd}` picks the most recently updated; a resumed session keeps its provider and model
- [ ] Fold: the four invariants hold on random journals; the v1 golden snapshot
- [ ] Black-box: `--continue`, `--resume <id>`, unknown id, locked session, `--max-turns`
- [ ] sdk changed exactly once, ADR-0005 lists what it touched

## Non-goals

Blobs for images and large outputs (a version-2 format change, when the first journal is too big to read — noted in ADR-0005). Rewind and the `Checkpointer` service (M11, with the UI that shows it). The sqlite index and search (M6). Bash background mode, the `!` command and the command dispatcher (M5, with the RPC surface that gives commands a second client; the catalogue refresh and a notice path out of `Plugin::register` go with it). Resuming a session's children (M8). Compaction persistence beyond the `Compacted` frame that already exists (M4). fsync per frame (a crash loses at most the last line, which replay already tolerates).

## Risks touched

R7 resume produces API-illegal history — the invariants are proptests, not examples; the fake provider already refuses an orphan tool result. R8 format drift — the version is in the header from the first byte and the golden snapshot is the kernel's reading of it. R1 — one sdk change, made first.
