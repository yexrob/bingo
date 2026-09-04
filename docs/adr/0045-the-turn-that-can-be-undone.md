# 0045 — The turn that can be undone: one kernel verb, the files a plugin's

Status: accepted (2026-09-04). Plan: M67. Supersedes nothing.

## Context

`ItemBody::Rewind { to_turn, dropped }` and `Event::Rewound` have been in the
sdk since ADR-0002 §3, `ContextView::items` and `SessionState::apply` have
dropped what they name since M0, and the TUI's `esc esc` picker has built the
rows since M11e — but nothing ever emitted one, because no plugin registered
`/rewind` and no plugin could. A person could ask to go back and be answered
with silence.

The kernel door this opens is one verb. **Would refusing it force a second
representation of a kernel-owned fact somewhere else?** Yes. The journal, the
`seq` that orders it and the two folds over it are the kernel's alone (ADR-0002
§2). A plugin that wanted to undo a turn without this verb would have to keep a
transcript of its own beside the journal and teach every surface to prefer it —
exactly the second representation this repository forbids.

The files are the other half, and they are nobody's in the kernel: it does no
file I/O for a turn, and a snapshot is a plugin's business (ADR-0001).

## Decision

1. **One kernel verb.** `HostApi::rewind(session, to_turn) -> Result<u32, _>`:
   the actor records `ItemBody::Rewind { to_turn, dropped }` as its own item
   and publishes `Event::Rewound { generation, to_turn, dropped, .. }`, whose
   `dropped` is the turn's first item and every item after it in transcript
   order — a notice recorded between turns happened after the line being taken
   back, so it goes too. The journal is never rewritten; the item that undid a
   turn is the record of the undoing. Refused `NOT_READY` while a turn runs (a
   child agent runs inside its parent's turn, so that covers it too) and
   `INVALID_INPUT` for a turn the session does not have. The cut is a pure
   function, `bingo_core::rewind::dropped`. No new error code; not on the
   JSON-RPC wire — a client rewinds by submitting the command.
   `Event::Rewound.files_restored` stays empty: the kernel restores nothing, so
   it names nothing.
2. **The snapshot, before the edit.** `bingo-checkpoints` (plugin tier) hooks
   `BeforeTool` and copies the file's bytes as they are *now* under
   `<data_dir>/checkpoints/<session>/<turn>/`: `<n>.snap` beside one `index`
   line `<n> <present|absent|skipped> <path>` — the path is last because a path
   may hold a space and a state may not. One snapshot per file per turn: the
   pre-turn state is one fact, and the second tool to reach for it finds it
   already kept. A file over `MOST` (8 MiB), and anything that is not a file,
   is recorded `skipped` and the rewind says so. The tool a call names is
   mapped to the field of its input that names the file it writes, in one table
   (`hook::WRITERS`, today `Write`/`Edit` → `file_path`); the matcher takes one
   name or one prefix and this table has two names, so the filtering is the
   hook's. **What a shell line wrote is not tracked** — a `Bash` call names no
   path this could read — and the reply never claims otherwise. A snapshot that
   fails is a log line, never a refused edit: a checkpoint makes an edit
   undoable, not allowed.
3. **`/rewind`, the plugin's.** Bare: a `View::Table` of turns, newest first —
   id, what was asked, the files that turn touched. `/rewind <turn>`: the files
   of that turn and every later one go back to what they were before the
   *first* of them (the oldest snapshot per file wins; `absent` means removed),
   and only then `host.rewind`. Read every snapshot before writing any, so a
   plan that cannot be read is refused whole; a write that fails stops **before
   the journal is touched**, because a transcript saying a turn was undone
   while the files still hold it is the one state nothing can recover from. Not
   `instant`: it waits in the queue for a running turn rather than being
   refused for asking.
4. **Collection.** A checkpoint outlives nothing but its session. The plugin
   sweeps at `start`, where the store's own GC runs (ADR-0005): every
   checkpoint directory whose session the host no longer lists is removed. A
   host that lists no session at all collects nothing — silence is a host with
   no store, not a host whose sessions were all deleted. A session deleted
   mid-run keeps its snapshots until the next start. Nothing else expires them.

## Consequences

- sdk touched once: `HostApi::rewind`, defaulted, so no double breaks and the
  wire is unchanged. Kernel: `Msg::Rewind`, `Mailbox::rewind`, `Host::rewind`,
  `rewind.rs`.
- The TUI's `esc esc` picker lights up with **no change to the surface**: it
  has always been offered exactly when a `rewind` spec is in the catalogue
  (M11e brick 8), which is what that data-driven test was for.
- `bingo-checkpoints` brings no dependency: 333 → 334 is the member alone.
- Recorded, not cured: "the turns of a transcript, and the line that opened
  each" is now derived in two places — `bingo-surface-tui/src/rewind.rs` for
  the card and `bingo-checkpoints/src/turns.rs` for the table. The cure is one
  brick in the sdk; it is not taken here because this milestone's sdk change is
  one verb.
