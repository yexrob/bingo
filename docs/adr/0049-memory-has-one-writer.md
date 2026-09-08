# ADR-0049 — Memory has one writer

Status: accepted · 2026-09-08 · Plan: M83

## Context

ADR-0044 made a memory a file the model can write and correct, and kept the
extractor (§5) beside it: a side question at the end of every tool-using
turn, writing one `project` file per line it answered. The extractor never
reads. It is handed one turn, not the index it adds to, not the tree, not a
clock; it can append and nothing else, and its dedup is a slug of the fact's
first words, which a paraphrase defeats. One project's scope on a real
machine after three days: 101 files, 97 from one morning, none by hand, six
wordings of one rejected direction beside two of the direction it rejected,
27 KB of it in the system prompt on every turn. When the project was deleted
and begun again on the same path the key was the path, so it all came back.
Across every scope on that machine, 540 files were the extractor's and 3 the
model's. The teaching the model reads is Claude Code's, and it is right; the
writer that cannot read it is the defect.

No kernel door is asked for; two are closed.

## Decision

1. **The model is the one writer.** The extractor hook, its `context.memory`
   switch and the plugin's config claim are gone. A memory is written by
   the model with `Write` and `Edit`, under the teaching it already carries,
   when the person says who they are or how they want it to work, corrects
   it, or decides something the tree does not record. Nothing writes a
   memory the model did not decide to write.
2. **The teaching says how a memory is read.** A memory is background, not
   an instruction: it says what was true when it was written; the
   conversation and the tree outrank it; one they contradict is fixed or
   deleted, not followed. Under 200 words, cached, snapshotted.
3. **A project is the commit it began with.** `<data_dir>/memory/<name>-
   <root commit[..16]>/` for a repository — the lowest of `git rev-list
   --max-parents=0 HEAD`, so a history with two roots answers one thing —
   and `<name>-<fnv(path)>`, the key of ADR-0044, for a directory outside
   git or before its first commit. A checkout deleted and begun again is a
   new project with an empty memory; a worktree or a second clone is the
   same one. Root and commit are asked once, in one place, for the
   contributor and `/memory` alike.
4. **The index is a hint.** Each index block carries at most 60 lines, the
   newest kept and the cut said. Lines that do not fit are lines to merge.
5. **The write side of the store goes with the writer.** The crate reads
   memories (parse, list, index text) and prints one only in tests, as the
   fixture the parser is checked against. The index parser, the slug maker,
   the atomic swap and the turn transcript the extractor was fed are
   deleted; `stream::drain` goes and `stream::summary` stays. The one-time
   migration of ADR-0044 §6 goes too: every project that had a file has
   been through it.

## Consequences

- The path-keyed scopes already on disk are not migrated and not deleted. A
  repository opens on an empty scope; a person who wants an old one moves
  the directory to the new key. Given what the old scopes hold, nobody will.
- A memory written in a repository before its first commit sits under the
  path key and is not seen after the commit. Rare; the model is told to
  check before relying.
- A shallow clone's root is its grafted boundary, so it keys apart from the
  full clone. Memory is local; accepted.
- A `context.memory` left in a settings file is reported as an unknown key.
- `bingo-context` drops `schemars`; budget unchanged otherwise.
- The fake provider's `side` deck stays: compaction is a side question too.

## Supersedes

Amends ADR-0044 §5 and §6, ADR-0006 §7 (the hook and its switch), and the
word "extractor" in ADR-0014 §1.
