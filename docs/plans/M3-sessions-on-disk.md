# M3 — Sessions on disk: journal, resume, budget

## Goal

A session outlives the process. `bingo --print --continue "…"` and `--resume <id>` reopen the exact journal a previous run wrote and the next turn folds its context from it; two processes cannot own one session; old sessions are collected; a runaway turn stops on a named error. The context a resumed session sends is API-legal by construction, proved on random journals.

## Bricks, in build order (owner)

1. **ADR-0005 + sdk** (kernel) — the persisted format is a contract consumed independently (a person reads it, a migrator will), so it is decided first. `SessionStore` gains `acquire(session)` / `release(session)` with `Ok` defaults: the lock is the store's, the moment is the kernel's. `ErrorCode::Storage` for a disk that fails, distinct from a kernel bug. One sdk change.
2. **`bingo-store-jsonl`** (worker A) — `<data_dir>/sessions/<id>/journal.jsonl`: one header line `{"format":"bingo-journal","version":1,"session":…}` then one durable `Frame` per line in seq order, appended and flushed per frame; `replay(since)` drops a torn last line and refuses a corrupt middle one or a newer version with `Storage`; `.lock` sidecar taken with an advisory exclusive lock (`std::fs::File::try_lock`) on `acquire`, held until `release` or process exit, a second holder is `SessionLocked` — data files are never locked; `summary.json` is the latest `SessionSummary`, rewritten on `create` and on each `SessionUpdated` frame, `updated_at` stamped from the journal's mtime on `list`, rebuilt from the journal when missing (deleting every one loses nothing — a test); `list` sorts by `updated_at` descending, honours `cwd`, `parent`, `limit`; `delete` removes the directory; GC in `start()` at most once a day (`gc.stamp`): sessions untouched for 30 days, then the oldest beyond 100, never one that is locked. Plugin `bingo.store.jsonl`, `provides: ["store:jsonl"]`.
3. **Resume** (kernel) — `open(ById | ByKey | Latest{cwd})` for a session that is not live: `store.acquire` → `replay(ZERO)` → `session::resume(frames, …)` spawns the actor with the frames as its journal, `seq` at the last frame, the state folded by `SessionState::apply`, the generation from the state; the actor publishes a fresh `SessionUpdated` as the head of the new segment. The model is the stored summary's provider and model, capabilities re-resolved (ADR-0004); tools, policy, hooks are the running host's. `Latest{cwd}` and `sessions(filter)` read the store as well as the live set. `delete` releases and removes.
4. **Fold invariants** (kernel) — proptest over random item journals (user, assistant, reasoning, tool calls with and without output, interruptions, compaction): every `ToolUse` has exactly one `ToolResult` in the next user message; no message is empty; the first message is a user message; a resumed journal folds to the same messages as the live one. A golden `fixtures/frames-v1.jsonl` (the frames of a recorded tool round) → `ContextView::items` → `insta` snapshot pins what version 1 means to the kernel.
5. **`bingo`** (kernel) — `--continue` (`Latest{cwd}`), `--resume <id>` (`ById`), `--max-turns N` (`TurnBudget.max_rounds`); register `JsonlStorePlugin`; `tests/cli.rs`: a second `--print --continue` run in the same `HOME`/cwd carries the first run's session id and continues its seq; `--resume` of an unknown id is `SESSION_NOT_FOUND`; a second process on a session another holds is `SESSION_LOCKED`; `--max-turns 1` on a tool loop exits 1 with `TURN_BUDGET_EXHAUSTED`.

Already in place from M0–M2 and not redone: `TurnBudget`, `Interrupt::{Cancel, Block}` in the executor, `--session-id` as an arbitrary `host/<key>`, a session's own cwd.

## Files

`docs/adr/0005-session-persistence.md`, `crates/bingo-sdk/src/{store,error}.rs`, `crates/bingo-store-jsonl/src/{lib,layout,journal,lock,summary,gc}.rs` + `fixtures/*.jsonl`, `crates/bingo-core/src/{session,host,context}.rs`, `crates/bingo-core/fixtures/frames-v1.jsonl`, `crates/bingo/src/main.rs`, `crates/bingo/tests/cli.rs`.

## Dependencies (verify on crates.io; `scripts/budget.sh` and `cargo deny check` after)

None. `std::fs::File::try_lock` (stable since 1.89) is the advisory lock; `fs4` 1.1 only mirrors it and its trait import is shadowed into a dead-import error. No sqlite: the list is small after GC and nothing searches it yet; the FTS index comes with the first consumer that searches (`/resume` in M6).

## Exit criteria

- [x] `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `scripts/check_discipline.sh`, `scripts/budget.sh`, `cargo deny check`
- [x] Store: fixtures for a clean journal, a torn last line (kept up to it), a corrupt middle line (`STORAGE` naming the line), a newer version (refused); two store instances on one directory — the second `acquire` is `SESSION_LOCKED`, `release` frees it; `summary.json` deleted → `list` still answers; GC removes the old and the surplus and skips the locked, and runs once a day
- [x] Resume: a host on a shared store reopens a session another host created, the snapshot holds its items, the next request to the provider carries the old messages, seq continues; `Latest{cwd}` picks the most recently updated; a resumed session keeps its provider and model
- [x] Fold: the four invariants hold on random journals; the v1 golden snapshot
- [x] Black-box: `--continue`, `--resume <id>`, unknown id, locked session, `--max-turns`
- [x] sdk changed exactly once, ADR-0005 lists what it touched

## Non-goals

Blobs for images and large outputs (a version-2 format change, when the first journal is too big to read — noted in ADR-0005). Rewind and the `Checkpointer` service (M11, with the UI that shows it). The sqlite index and search (M6). Bash background mode, the `!` command and the command dispatcher (M5, with the RPC surface that gives commands a second client; the catalogue refresh and a notice path out of `Plugin::register` go with it). Resuming a session's children (M8). Compaction persistence beyond the `Compacted` frame that already exists (M4). fsync per frame (a crash loses at most the last line, which replay already tolerates).

## Risks touched

R7 resume produces API-illegal history — the invariants are proptests, not examples; the fake provider already refuses an orphan tool result. R8 format drift — the version is in the header from the first byte and the golden snapshot is the kernel's reading of it. R1 — one sdk change, made first.

## Verified (2026-08-29, commit 48fbffd)

```
$ cargo fmt --all -- --check                                        exit 0
$ cargo check --workspace --all-targets --locked                    exit 0
$ cargo clippy --workspace --all-targets --locked -- -D warnings    exit 0
$ cargo test --workspace --locked                                   exit 0
  bin (cli.rs) 20 · core 90 · sdk 16 · store-jsonl 34 · print 34 · provider-fake 19 · provider-anthropic 68
  provider-openai 95 · tool-fs 69 · tool-bash 51 · tool-web 77 · permissions 92          = 665 passed
$ scripts/check_discipline.sh                                       exit 0 (no warnings)
$ scripts/budget.sh                                                 dependencies 214 (max 260), unchanged: no crate added
$ cargo deny check                                                  advisories ok, bans ok, licenses ok, sources ok
```

Exit criteria, item by item:

- Store: `fixtures/{clean,torn,corrupt,version2}.jsonl` with one replay test each and `since`; the clean fixture is asserted byte-for-byte against what the writer produces, so the file format is a contract; two stores on one root — the second `acquire` is `SESSION_LOCKED`, `release` frees it; `summary.json` deleted → `list` rebuilds it from the last `SessionUpdated`; GC old / surplus / locked-kept / once a day, on an injected clock.
- Resume: a second host on a shared store reopens by id, the snapshot holds the old items, the next provider request carries them, seq continues; `Latest{cwd}` from the store; unknown id and empty directory are `SESSION_NOT_FOUND`; a journal cut inside a turn resumes with `TURN_LOST` said first; a journal ending in `SessionClosed` resumes open.
- Fold: `every_projection_is_legal_for_the_api` and `a_replayed_journal_folds_exactly_like_the_live_session` (proptest); the v1 golden `frames-v1.jsonl` → snapshot.
- Black-box: `--continue` and `--resume <id>` reopen the same session and continue its seq while a flagless run is a new one; unknown id; a held session is `SESSION_LOCKED` from a second process; `--max-turns 2` on a tool loop is `TURN_BUDGET_EXHAUSTED`.
- sdk changed once (`09c5e48`).

Found by the tests while integrating (each is a commit body too):

- The proptest caught a real fold bug: two tool calls of one response were split into two rounds; they now share one assistant message keyed by `(turn, round)`, and a journal opening on the assistant's words gets `[The conversation begins here.]` (`e22553e`).
- `create` never took the store's lock, so a second process could continue a running session (`8cfb599`); a journal ending in `SessionClosed` folded to a closed state (same commit).
- The print surface waited forever on a rejected submit (`09fd6c1`).
- The lock is `std::fs::File::try_lock` (stable since 1.89), not `fs4`: the crate's trait now merely mirrors std and its import is a dead-import error under `-D warnings`. ADR-0005 §4 amended.
- `summary.json` is rebuilt from the *last* `SessionUpdated`, not the first, so a later title survives the rebuild.

Open, carried forward:

- `Plugin::start` cannot see `Env`; the store carries its root from `register` in a `OnceLock`. `HostHandle::env()` goes with the next sdk change that needs one.
- No cross-process lock test on Windows (the CI leg is `continue-on-error` until M6); the in-process test is the stricter `flock` case.
- A session deleted between `create` and its head frame has no summary to rebuild from — unreachable in practice, noted in the store.
- Blobs (journal version 2), rewind checkpoints (M11), the sqlite index and search (M6), the command dispatcher (M5), children resume (M8), learned windows persistence (with the store now present, a small M4 item).
