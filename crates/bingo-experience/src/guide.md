# Experience

A playbook is procedure a project taught you: *when this happens (trigger), do
this (steps), check it worked (verify)*. One markdown file per playbook, kept
per project rather than per session, so what one session learned the next one
starts with. A **fact** about the project — where a thing lives, what a name
means — is memory's, not this library's; the two never share a file.

Write one down only after you have done the thing and seen it work.

## The files

`<config_dir>/experience/<project>/<id>.md`, YAML frontmatter (`status`,
`summary`, `trigger`, `steps`, `verify`, `created`, `outcomes`) and a free body
that is the entry's notes. The `<project>` is the git remote when there is one,
so the library follows a checkout to another machine; else the repository root,
else the directory itself.

The directory is the index: it is read afresh every time, `grep` and `rm` work
on it, and a person may edit a file by hand. A file that cannot be parsed costs
that one entry and is named out loud — in `/experience` — rather than silently
skipped. A frontmatter key this plugin does not know is left alone.

## The tools

- `ExperienceCommit` — write a playbook down or revise one. `trigger` (the
  words that would be used at the time), `summary` (one line), `steps` (in
  order), and optionally `verify`, `notes`, `status`. Without `id`, an entry
  that already has the same trigger, summary and steps is revised rather than
  forked; with `id` (a unique prefix of one is enough) that entry is revised
  and keeps its outcomes and the day it was first written. The person sees the
  diff of the file this would write before it is written.
- `ExperienceQuery` — search, with the words of the task as the `query`, and
  `limit` (five when absent). Read-only. Every entry is ranked, retired ones
  included and marked, and there is no relevance floor: the weakest answer to
  a question somebody asked is still the answer.
- `ExperienceOutcome` — one record of what happened when you followed an
  entry: `id`, `outcome` (`helpful` or `harmful`), and `evidence` that is
  required and must be something a person could check — the command that went
  green, the error that came back, the file that changed. One record per time
  you actually followed it. **It never changes an entry's status**: nothing
  promotes itself here.
- `ExperienceForget` — delete one entry and every outcome it has been given,
  for a playbook that was *wrong*. One that merely stopped applying is
  `status: retired` through `ExperienceCommit`, which keeps what was learned.
  There is no expiry, no cap and no collection; this is the whole of it.

An id is always a unique prefix. One that names nothing, or several, comes
back as a sentence to act on rather than a failed call.

## How a library reaches a turn

Two blocks, both at the start of a round:

- The `# Experience` index — the active entries, most useful first, at most
  ten lines and then `… N more`. It is a pointer, not the playbook: the steps
  come from `ExperienceQuery`.
- Recall — at most three lines, appended as a user item after what the person
  just said, when the ranking clears a floor and the library has anything in
  it. It lands in the transcript, so what you were shown is what the person
  can read back, and nothing is recalled twice for one thing said.

Both order entries the same way `/experience` does: most `helpful` first, most
`harmful` last, retired after active, ties by id — so the same library never
reads differently in two places. Ranking is BM25 over four fields, and a
trigger word weighs three times what a note does: a trigger says what an entry
is *for*.

## `/experience`

The library as a table — `id`, `status`, `summary`, `outcomes`, `age` — in
that same order, with a line for any file that was meant to be an entry and
could not be read. It runs while a turn is busy.

## Turning it off

```jsonc
{ "experience": { "enabled": false } }
```

`enabled` is true until a project says otherwise. Off, the plugin contributes
nothing at all: no tools, no index, no recall, no `/experience`. A typo in the
block is a startup failure rather than a silence.
