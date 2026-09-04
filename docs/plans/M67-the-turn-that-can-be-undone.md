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

- [ ] `host.rewind` appends the item, the fold drops the items, the
      context builder sends none of them; refused mid-turn.
- [ ] A Write and an Edit in one turn snapshot the file once; a file
      created in the turn is `absent` and removed on rewind.
- [ ] `/rewind <turn>` restores files across two turns and drops both
      turns' items (integration test on the fake provider).
- [ ] The TUI `esc esc` picker now opens and its `⏎` rewinds
      (existing picker tests re-aimed; one pty scene).
- [ ] All gates; Windows cross-check for the new crate (paths).
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
