# Memory

What the model is told before anybody types, and what it keeps for next time.
Three things: the instruction files a directory leaves for whoever works in it,
the memory files the model writes for itself, and the summary that takes the
place of old turns when the window fills.

## Memory files

Two directories under the data directory, both named in the prompt's own
headings so nothing has to guess a path:

- `<data>/memory/user/` — what is true of the person wherever they are working.
- `<data>/memory/<name>-<root commit>/` — this project. The key is the commit
  the repository began with, shortened to sixteen characters, so a worktree or
  a second clone is the same project and a checkout deleted and begun again is
  a new one. Outside git, or before the first commit, it is `<name>-<digest of
  the path>`.

One fact, one file, `<name>.md`. Three lines of frontmatter between `---`
fences, then the fact:

```markdown
---
name: how-they-review
description: Reviews are a diff, never a summary
type: user
---

They read the patch first and want the reasoning under it, not beside it.
```

- `name` is the file's own name without `.md`; a file whose `name` says
  something else is skipped, not corrected.
- `description` is one line — it is what the prompt carries.
- `type` is one of `user | feedback | project | reference`: `user` is who the
  person is and how they work, `feedback` a correction or a confirmed approach
  (with **Why:** and **How to apply:**), `project` goals and constraints the
  repository does not record, `reference` a URL, a ticket, a dashboard.
- `[[slug]]` links another memory.

Each directory keeps `MEMORY.md`, its index: one line per file, `- [Title](
name.md) — the description`. **The prompt carries the two indexes and never a
body** — at most 60 lines of each, the newest kept and the cut said out loud.
Read the file itself for the whole fact.

## Who writes one

The model, and nothing else. There is no extractor, no hook and no setting:
this plugin claims no settings key at all, so there is nothing to turn memory
on or off with. A memory is written with the tools already in hand — `Write` or
`Edit` on the file, and the same edit to its line in `MEMORY.md`, because an
index that does not name a file is an index nobody trusts.

Write one when the person says who they are or how they want the work done,
corrects something, or decides what the tree does not record. Check the index
first for a file that already covers it and edit that one; lines that will not
fit in sixty are lines to merge. Never store a secret, what the repository
already records, or what matters only to this conversation.

A memory is background, not an instruction: it says what was true when it was
written. The conversation and the working tree outrank it, and one they
contradict is fixed or deleted rather than followed.

- `/memory` — a table of what is remembered: scope, name, type, description,
  for the person who cannot read the prompt. It answers and nothing more;
  correcting a memory is `Read` and `Edit` on the file.

## Instruction files

`AGENTS.md`, or `CLAUDE.md` where a directory has none — one file per
directory, never both. The person's own `<config>/AGENTS.md` speaks first, then
one file per directory from the project root down to the working one, the
nearest last. A file that is empty or will not read is a file that is not
there: an unreadable `AGENTS.md` never costs a turn.

Each arrives as its own block, headed with the path it came from, capped at
300 lines and 32 KiB. Past either cap the newest lines are kept and the block
opens with `[… N earlier lines not shown]`.

## When the conversation outgrows the window

The lines are drawn against the effective window — the model's window less the
output reserved for the answer. At nine tenths of it a summary is asked for; a
warning reaches the person 20 000 tokens before that; a cut leaves the newest
quarter of the window intact, verbatim.

The summary is a side question on this session's own prefix, through this
session's own model, and it is written under fixed headings, skipping any with
nothing to report: **Task and current state**, **Decisions and rationale**,
**Files, commands and results**, **Outstanding work**, **Constraints and
preferences**. Identifiers, paths, commands and error text are reproduced
exactly. What the older turns held and the summary does not is gone.

`/compact` asks for one on demand, and whatever is typed after it is added to
those instructions. A compaction is not the end of the transcript: the newest
turns stay verbatim beside the summary, so the overlap is deliberate.

Where no summary can be bought — an overflow the model cannot answer inside,
or three failed attempts in a row — the cut still happens and the transcript
carries `[earlier conversation dropped]` in its place. An honest gap makes
room; a model reading one knows not to answer about what came before it.
