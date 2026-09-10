# Rewind

A checkpoint is what a file held before the turn that changed it. Every turn
that edits a file leaves one directory of them, and `/rewind` puts the files
and the conversation back to a turn you name. Nothing expires a checkpoint but
the end of its session: they are swept at startup, when the session they
belong to is no longer one the host lists.

## What is kept

Before `Write` or `Edit` runs, the bytes at its `file_path` are copied — one
snapshot per file per turn, because what the file was before the turn is one
fact and the turn's second edit of it is not a second one. A relative path
hangs off the session's own directory; a path outside the working tree is kept
all the same, because the fact is the file, not where it is.

- A path with nothing at it is recorded `absent`, and going back removes
  whatever is there by then.
- A file over 8 MiB, and anything that is not a file, is recorded `skipped`:
  nothing was copied, nothing is put back, and the reply says so rather than
  pretending otherwise.
- **A shell line is not tracked.** `Bash` names no path this could read, so
  what a command wrote is never snapshotted. Only `Write` and `Edit` are
  mapped to the field of their input that names the file they write; a tool
  outside that table — an MCP server's, a plugin's — leaves nothing to undo.

The snapshots live under `<data_dir>/checkpoints/<session>/<turn>/`. One that
cannot be taken is a log line, never a refused edit: a checkpoint is what
makes an edit undoable, not what makes it allowed.

## `/rewind`

Bare, it is a table of this session's turns, newest first: the turn's id, the
line that opened it, and the files that turn touched.

`/rewind <turn>` takes an id from that table and goes back to it:

1. The files of that turn **and of every turn after it** go back to what they
   were before the first of them — the oldest snapshot per file wins — and a
   file those turns created is removed.
2. Only then the conversation: that turn's first item and every item after it
   in transcript order are dropped, a notice recorded between turns included.

The reply is `rewound to <the line that opened it>, N items dropped`, then
`put back <files>`, `removed <files>`, and `left as it is, never kept (over
8 MiB): <files>` for whatever was skipped. Turns that changed no file say so
rather than leaving the question open.

That order is the promise. Every snapshot is read before a byte is written, so
a plan with an unreadable snapshot is refused whole and nothing moves; a write
that fails stops before the journal is touched, because a transcript saying a
turn was undone while the files still hold it is the one state nothing
recovers from.

`/rewind` is not instant: asked while a turn is running it waits in the queue
for that turn rather than being refused for asking. A turn this session never
had is refused, and nothing moves.

## What a rewind does not do

- It does not rewrite the journal. The item recording the rewind is the record
  of it, so reopening the session replays what happened, the undoing included.
- It restores no file a shell line wrote, none a tool outside `Write` and
  `Edit` wrote, and none that was skipped for its size.
- It touches nothing outside this session's files and transcript: no git
  state, no running process, no other session's work.

## From the terminal

`esc esc` on an empty composer opens the same turns as a card, newest first;
`⏎` on a row submits `/rewind <turn>` for it. The card is offered wherever a
`rewind` command is registered, and nowhere else.
