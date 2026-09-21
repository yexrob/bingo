# M106 — Scheduler lock recovery

## Goal

User, 2026-09-21: a dead process left `schedules/runner.lock` behind,
and no already-running process could recover. Keep one scheduler per data
store without making a file's existence proof of a live runner. Use the
standard-library file-lock mechanism already used by `bingo-store-jsonl`;
no external dependency, kernel door, or channel-lock change. The gateway
composition gains a workspace dependency on `bingo-schedule` to read its
lock protocol through its own probe rather than duplicate the marker.

## Bricks, in build order

1. Contract tests before implementation: real processes exclude one
   another; an already-started standby takes over after normal exit or
   forced termination; a released modern lock file remains reusable.
2. A permanent `runner.lock` inode, held with `File::try_lock()`. Publish
   a complete, nonnumeric protocol marker atomically via a same-directory
   staging file and hard link, so a crash during initialization cannot
   publish an empty modern file. A claim owns the open file, never unlinks
   the published path, and distinguishes contention, legacy files, and I/O
   errors. Diagnostic reads probe the OS lock, not a recorded PID.
3. Legacy PID-only, empty, or unrecognized files fail closed without being
   changed: stop all binaries sharing the store, remove their sentinel with
   one repair invocation, then restart. Concurrent legacy repairs can hold
   cached path deletions; old binaries cannot coordinate with OS locks. The modern marker also prevents old
   `gateway doctor --fix` from treating this inode as a removable PID file.
4. A standby supervisor retries acquisition on a short bounded interval.
   Acquiring immediately scans overdue work. It owns the claim through the
   whole runner future; normal shutdown cancels and joins before returning.
   In-flight dispatch finishes under the claim rather than being abandoned
   between spending an occurrence and delivering its turn.
5. One holder line distinguishes this process, another runner, no owner,
   and storage/migration trouble. Update the guide and dated ADR-0019 note.
   Teach gateway doctor to recognize this persistent lock and never offer
   to remove it; do not change channel-lock or PID-file semantics.

## Files

- `crates/bingo-schedule/src/{lock.rs,schedules.rs,supervisor.rs,runner.rs,lib.rs}`
- `crates/bingo-schedule/src/wakes.rs` (joined shutdown cancellation boundary)
- Small lock/supervisor test or helper modules where responsibility splits
- `crates/bingo-schedule/src/{guide.md,command/schedule.rs,render.rs}`
- `crates/bingo-schedule/src/tools/create.rs` (holder wording tests only)
- `crates/bingo/tests/cli/schedule.rs` and narrowly scoped test submodules
- `crates/bingo-gateway/src/doctor.rs` and its tests
- `docs/adr/0019-schedules.md`

## Exit criteria

- [x] Two processes cannot dispatch the same due entry concurrently.
- [x] Existing standby takes ownership and dispatches after clean exit and
      forced termination, with no lock-file deletion or process restart.
- [x] An unlocked modern file survives release and is reclaimed; legacy
      numeric/empty files are refused without mutation or automatic theft.
- [x] A stop holds the claim until in-flight dispatch and runner exit finish;
      a cancelled standby never starts dispatching.
- [x] `/schedule` and tool receipts distinguish waiting from a failed store;
      real I/O errors are not described as another owner.
- [x] Gateway doctor never removes the modern lock, held or free.
- [x] Targeted tests and full workspace fmt/check/clippy/test pass, plus
      discipline, budget, cargo deny and Windows cross-checks where possible.

## Non-goals

- Changing channel locks, gateway PID ownership, or starting a new daemon.
- Exactly-once external effects, replaying a spent failed occurrence, or
  changing the existing schedule entry schema and delivery policy.
- Accepting expired `once at` requests or adding relative one-shot syntax:
  the separately identified creation-time bug remains a follow-up.
- Automatically upgrading a legacy sentinel while old binaries may run.

## Risks

- OS locks coordinate cooperating processes on local filesystems; deleting
  or replacing a held lock externally breaks that contract. Modern code
  and its doctor must never unlink it.
- A retained descriptor must not be inherited by executed child processes.
  Standard-library file opening supplies the platform handle semantics.
- Shutdown may wait for an in-flight host operation; releasing early would
  allow another runner to dispatch concurrently.
- Process tests use isolated homes and readiness handshakes with bounded
  waits, not assumptions about startup time or PID reuse.

## Verified (2026-09-21)

Cargo commands used `env -u RUSTC_WRAPPER` and
`DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`: this session had
an obsolete `sccache` wrapper in its environment. No user configuration changed.

- Before implementation, `cargo test -p bingo --test cli schedule::ownership
  --locked -- --nocapture`: `0 passed; 6 failed`. Both takeover contracts
  timed out; the old lock disappeared on clean stop and reported stale PID
  ownership. The additional pre-cancelled wake contract also failed first.
- `cargo test -p bingo-schedule -p bingo-gateway --locked`:
  `123 passed; 0 failed` and `51 passed; 0 failed`.
- `cargo test -p bingo --test cli schedule:: --locked`:
  `17 passed; 0 failed`; `gateway::stopping_gives_back`: `1 passed; 0 failed`.
  Real child processes cover normal shutdown, forced termination, automatic
  takeover, legacy refusal, and doctor preservation of held/free modern locks.
- `cargo fmt --all -- --check`: exit 0. `git diff --check`: exit 0.
- `cargo check --workspace --all-targets --locked`: exit 0.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: exit 0.
  Targeted schedule/gateway clippy was repeated after migration wording changes.
- `cargo test --workspace --locked --no-fail-fast`: exit 0; every target
  passed. CLI: `245 passed; 0 failed`; PTY smoke: `16 passed; 0 failed`.
  The TUI target retains its two existing ignored tests.
- `scripts/check_discipline.sh`: `discipline ok` (size warnings only).
- `scripts/budget.sh`: `budget ok`; `dependencies (unique, normal): 342
  (max 342)`; `warm cargo check -p bingo-core: 18s (max 20s)`;
  `relink isolation: touching the TUI recompiled 0 crates for core`.
- `cargo deny check`: `advisories ok, bans ok, licenses ok, sources ok`;
  existing duplicate-version and unused-license allowances remain warnings.
- `cargo check -p bingo-schedule --all-targets --locked --target
  x86_64-pc-windows-msvc`: exit 0, including lock and lifecycle tests.
- Gateway and CLI Windows cross-checks were attempted but stopped in existing
  `aws-lc-sys 0.44.0`: this macOS machine has no Windows SDK headers
  (`fatal error: 'stdlib.h' file not found`, `'windows.h' file not found`).
  No Windows execution was claimed; the release CI remains that backstop.

No change was made to channel lock ownership, schedule timing grammar, the
60-second cross-process store rescan, or the separately identified expired
one-shot creation bug. No binary was installed or live process restarted.
