# Tasks

A task list is what a session has to do, kept in that session's own journal.
It is not a file and not a setting: `--continue` and `--resume` read back the
list the last run wrote, and every surface draws it from the same record. One
task per unit of work someone would tick off, so record what is worth coming
back to.

Every tool here reads the whole list, changes it and writes the whole of it
back. They are read-only and trusted — nothing outside this process is
touched — so the person is not asked before one runs. None of them is
concurrency-safe: two calls at once each write a list without the other's
change, so make one call at a time.

## The tools

- `TaskCreate` — one task, and the id the list gives it comes back. `subject`
  in the imperative ("write the plan"), `activeForm` the present-continuous
  form shown while it runs ("writing the plan"), `description` for anything
  the doer would otherwise have to ask about, `owner`, `blockedBy` (ids that
  must finish first), `blocks` (ids waiting on this one), `metadata`. You
  never pick an id.
- `TaskUpdate` — `id`, and only the fields you name; the rest stay as they
  are. `status` moves it, `addBlockedBy` and `addBlocks` add ids, `metadata`
  merges by key, and `claim: true` writes your own name as the owner (the
  runtime knows which session you are — do not pass `owner` as well, which is
  for giving the task to somebody else). A session with no name of its own
  cannot claim, and is told to name the doer with `owner` instead.
- `TaskGet` — one task in full, as JSON, by id.
- `TaskList` — every task, one per line. Read it before writing to an id you
  are unsure of.

An id the list does not have is answered, not failed: `No task #N. TaskList
shows what there is.`

## Statuses

`pending`, `in_progress`, `completed`. Mark a task `in_progress` when you
start it and `completed` the moment it is done; one task in progress at a time
reads best. A completed task keeps its number — ids are never reused.

One task on one line reads
`#3 [in_progress] write the plan — reviewer (blocked by #1, #2)`.

## What reaches the prompt

A `# Tasks` block, recomputed each round, listing the **open** tasks of this
session's own list — a board's is never in the prompt, because a list nobody
asked for in every request is a tax. So a task you put on a board is one you
must go and read.

## `/tasks`

The same list as a table for the person: `id`, `status`, `subject`, `owner`.
`/tasks in #room` shows a room's board instead. It runs while a turn is busy.

## A room's board

All four tools take an optional `in: "#room"`, and then the list they work on
is that room's own — one shared board everyone in the room reads and writes.
`#design` and `design` name the same room. The rooms you can reach are the
ones you opened and the ones beside you; a name outside that reach comes back
as a sentence to correct, not a failed call.

Two writers at once overwrite each other, so put a task on a board once and
let its owner move it on, saying so in the room rather than racing.

In a board listing, an owner no session in that room answers to any more reads
`owner (gone)`. Nobody rewrote the task: it is said at read time, and no
machinery ever reassigns a crashed owner's work.
