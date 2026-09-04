# M67 — The turn that can be undone

## Goal

User, 2026-09-05: Claude Code's checkpoints — every file edit is
snapshotted before it happens, and `/rewind` puts both the files and
the conversation back to the turn a person picks. bingo has the
picker (`esc esc`, `bingo-surface-tui/src/rewind.rs`, derives turns
from the transcript) and the journal item (`ItemBody::Rewind {
to_turn, dropped }`, folded by `SessionState::apply` and by the
context builder), but **no plugin registers `/rewind`**, so the chord
is deliberately silent and nothing ever emits the item. Wanted: a
plugin that does both halves.

## Bricks

1. **The kernel's one new verb.** `HostApi::rewind(session, to_turn)
   -> Result<u32, KernelError>`: append `ItemBody::Rewind { to_turn,
   dropped }` to the session's journal as the kernel's own item —
   every item after the turn's opening is dropped from the model's
   view (`context.rs` already does this on the item) and from every
   surface's fold (`state.rs` already does this). Refused while a
   turn is running (`Busy`), refused for a turn the session does not
   have. That is the whole of the kernel's part; `rewind` is already
   an sdk word, not a feature noun. Contract first: the host test
   that emits it and reads the fold.
2. **The snapshot, before the edit.** New plugin crate
   `bingo-checkpoints` (plugin tier): a `BeforeTool` hook on every
   tool that is not `read_only` and whose input carries a path the
   tool declares it writes — read the fs tool's specs to find the
   field (`path`/`file_path`) and put the mapping in one place. It
   stores the file's bytes as they are *before* the call under
   `<data_dir>/checkpoints/<session>/<turn>/<n>.snap` with a one-line
   index `<n> <absolute path> <present|absent>`; a file already
   snapshotted in this turn is not snapshotted again (the pre-turn
   state is the fact). Bounded: a file over `MOST` (say 8 MiB) is
   recorded as `skipped` and the rewind says so. Bash-made changes are
   not tracked (Claude Code's own limit; say it in the ADR).
3. **`/rewind`.** Registered by the plugin (so the TUI's chord lights
   up with no change there). Bare: a `View::Table` of turns — id,
   what was asked, files touched. `/rewind <turn>`: restore every file
   touched in that turn and every later one to its pre-turn bytes
   (absent → removed; the oldest snapshot per file wins), then
   `host.rewind(session, turn)`. The reply says what was restored and
   how many items dropped; a restore that fails on one file stops
   before the journal is touched and says which file. The TUI's
   picker sends exactly this line.
4. **GC.** Checkpoints of a session go when the session is deleted
   (`SessionStore` GC hook or the delete path — find where the
   store's own GC runs and hang it there); nothing else expires them.

## Files

`bingo-sdk/src/host.rs` (+ the `Busy`/not-found codes if new),
`bingo-core/src/host.rs` + `session.rs`, `crates/bingo-checkpoints/`
(new: `hook.rs`, `store.rs`, `restore.rs`, `command.rs`, `lib.rs`),
`crates/bingo/src/main.rs` (register), `Cargo.toml`/`Cargo.lock`,
`scripts/budget.toml` (+1 member), `docs/adr/0045-the-turn-that-can-
be-undone.md` (≤80 lines), M11e plan pointer, `docs/design/tui.md`
dated line (the chord is live).

## Exit criteria

- [x] `host.rewind` appends the item, the fold drops the items, the
      context builder sends none of them; refused mid-turn.
- [x] A Write and an Edit in one turn snapshot the file once; a file
      created in the turn is `absent` and removed on rewind.
- [x] `/rewind <turn>` restores files across two turns and drops both
      turns' items (integration test on the fake provider).
- [x] The TUI `esc esc` picker now opens and its `⏎` rewinds
      (existing picker tests re-aimed; one pty scene).
- [x] All gates; Windows cross-check for the new crate (paths).
- [ ] Hands-on: appended by the parent.

## Non-goals

Snapshotting what Bash wrote; git-based snapshots; rewinding a
child session or a room; a size cap on the checkpoint dir beyond
per-file `MOST`.

## Risks

A rewind while a child agent is running: refuse (`Busy`) as for the
session's own turn. A path outside cwd (the tool allows it): snapshot
it all the same — the fact is the file, not where it is. Two edits of
one file in one turn by two concurrent tools: the hook runs before
each, the first wins, which is the pre-turn state either way.

## Verified — 2026-09-04

Every gate green, by exit code, on `m67-checkpoints` (from `dev` at
`d601a6f2`):

```
cargo fmt --all -- --check                                        0
cargo check --workspace --all-targets --locked -j 2               0
cargo clippy --workspace --all-targets --locked -j 2 -D warnings  0
cargo test --workspace --locked -j 2 --no-fail-fast               0  85 binaries, 3968 passed, 0 failed
scripts/check_discipline.sh                                       0  discipline ok
scripts/budget.sh                                                 0  dependencies (unique, normal): 334 (max 334)
cargo deny check                                                  0  advisories ok, bans ok, licenses ok, sources ok
cargo test -p bingo --test pty --locked -j 2                      0  12 passed
cargo check -p bingo-checkpoints -p bingo-sdk -p bingo-core \
  --all-targets --locked -j 2 --target x86_64-pc-windows-msvc     0
```

### The bricks, and where each is proved

1. **`HostApi::rewind(session, to_turn) -> Result<u32, KernelError>`**, one
   method with a default body, so no `HostApi` double breaks and the wire is
   unchanged. The actor records `ItemBody::Rewind { to_turn, dropped }` and
   publishes `Event::Rewound`; the cut is `bingo_core::rewind::dropped`, pure.
   `NOT_READY` under a running turn (`self.running.is_some()`, as `/compact`
   reads it), `INVALID_INPUT` for a turn the session never had. No new error
   code. `crates/bingo-core/src/host/tests/rewind.rs` drives a real host: the
   item is appended, the client's fold loses the turn, and the *next* turn's
   provider request carries the first answer and neither line of the rewound
   one.
2. **`bingo-checkpoints`**, six modules — `store` (the directory), `hook` (the
   tool-to-field table and `BeforeTool`), `turns` (a transcript's turns, pure),
   `restore` (the plan and its application), `command` (`/rewind`), `lib` (the
   plugin). The plan named five; `turns` is the sixth because listing turns and
   running a command are two jobs. `<data_dir>/checkpoints/<session>/<turn>/`
   holds `<n>.snap` and one `index` line per file — `<n>
   <present|absent|skipped> <path>`, **path last**, because a path may hold a
   space and a state may not. One snapshot per file per turn; over 8 MiB (or
   not a file) is `skipped`; a path that is not UTF-8 is not kept at all,
   because `Display` would not write it back byte for byte.
3. **`/rewind`**, registered, not `instant`. Bare: a `View::Table` of turns
   newest first — id, what was asked, the files touched. With a turn: the files
   of it and every later one go back (oldest snapshot per file wins, `absent` →
   removed), then `host.rewind`. Every snapshot is read before a byte is
   written, and a failure returns before the journal is touched —
   `command::tests::a_restore_that_cannot_be_read_leaves_the_journal_alone`
   proves the transcript is untouched. `crates/bingo/tests/cli/checkpoints.rs`
   is the same across four real processes on the fake provider.
4. **Collection** at `start`, where the store's own GC runs: a checkpoint
   directory whose session the host no longer lists is removed. An empty
   listing is an answer; only an `Err` is "cannot tell".

The TUI changed only in its tests' words and one fixture field: the picker has
always been offered exactly when a `rewind` spec is in the catalogue.

### What was not done, or not cured

- **A shell line's changes are not snapshotted** — the plan's non-goal, said in
  the reply's own words rather than passed over in silence.
- **`Event::Rewound.files_restored` stays empty.** The verb takes no paths, as
  the plan wrote it, and the plugin's reply names the files. The field now has
  no producer at all, which is a subtraction for whoever next touches that
  enum.
- **A session deleted while the process runs** keeps its snapshots until the
  next start.
- **The turns of a transcript are derived twice** — the TUI's card and the
  plugin's table. The cure is one brick in the sdk, not taken while this
  milestone's sdk change is one verb.
- `scripts/tui-smoke.sh` was not run (out of scope by the brief).
- `crates/bingo-core/src/session.rs`'s `fn handle` warned at 66 lines before
  this branch and warns at 67 after it: the one-line `Msg::Rewind` arm joined
  the dispatch table it belongs to rather than splitting somebody else's
  function.
- `scripts/budget.sh` warns that `target/debug` is 11 GB, which is this
  worktree holding a host build, a Windows cross-check and every test binary at
  once; it is a soft limit and not this change's.
