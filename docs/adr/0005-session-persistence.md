# 0005 — Session persistence: a JSONL journal per session, a sidecar lock, a derived summary

## Context

The journal is the session (ADR-0002): a session on disk is that journal written down. People read the file, a future migrator reads it, another process must not write it at the same time, and `--continue` has to find the right one without opening every file. The old project kept seven files per session and locked data files, which Windows turned into hard failures; goose keeps only SQLite and cannot replay; opencode keeps two truths.

## Decision

1. **Layout.** `<data_dir>/sessions/<session-id>/journal.jsonl`, `.lock`, `summary.json`. The id is the session's ULID as minted by the kernel; nothing is renamed.
2. **Journal.** Line 1 is the header `{"format":"bingo-journal","version":1,"session":"<id>"}`. Every following line is one durable `bingo_sdk::Frame` exactly as it serialises (every property camelCase, variant fields included, since ADR-0007), in seq order, appended and flushed per frame, never rewritten. Ephemeral frames (`ItemDelta`, `Notice`, `Lagged`) are never written. A format change is a new `version` and a migrator that re-folds; version 1 is never edited in place.
3. **Reading.** A torn last line (a crash mid-write) is dropped; replay ends at the last whole frame. An unreadable line anywhere else is a `STORAGE` error naming the line — corruption is reported, not skipped. A header version newer than the reader's is refused with `STORAGE`.
4. **Lock.** `.lock` is the only claim of ownership: an advisory exclusive lock on that file (`std::fs::File::try_lock`, stable since Rust 1.89; no crate), taken by `SessionStore::acquire` when the kernel creates or resumes a session and released by `release` or process exit. Data files are never locked. A second holder gets `SESSION_LOCKED`.
5. **Summary.** `summary.json` is the latest `SessionSummary`, rewritten on create and on each `SessionUpdated` frame, with `updated_at` read from the journal's mtime on `list`. It is derived: a missing one is rebuilt from the journal, and deleting every one loses nothing. It exists so `list` never reads a journal body. The sqlite index the plan foresaw is deferred to the first consumer that searches.
6. **Collection.** On store start, at most once a day: sessions untouched for 30 days, then the oldest beyond 100, never a locked one.
7. **Resume is the kernel's.** The store replays; the kernel folds the frames with the one reducer, continues the seq, re-resolves the model from the stored provider and model (ADR-0004), and publishes a fresh `SessionUpdated` as the head of the new segment. What the store returns is what the live session had — there is no second reader.
8. **Blobs are version 2.** Images and large outputs stay inline in version 1. When a journal grows past what a replay should read, `ContentPart` gains a blob reference and the store a `blobs/<hash>` directory — a format change with a migrator, not a patch.

## Consequences

- sdk change: `SessionStore::{acquire, release}` with `Ok` defaults (the in-memory store needs neither) and `ErrorCode::Storage`. Touched: `bingo-core` (`MemoryStore`, the host's open path), the new `bingo-store-jsonl`.
- A session directory is portable: copy it, and `--resume` reads it.
- `rm -r summary.json` across the store is a valid repair; `rm journal.jsonl` is data loss.
- A crash loses at most the last line; there is no fsync per frame.
- Two bingo processes in one cwd get two sessions; `--continue` picks the most recently updated one that nobody holds.

## Supersedes

—
