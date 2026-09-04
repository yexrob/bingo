# M64 — One fact, one file

## Goal

User, 2026-09-05: give bingo a memory shaped like Claude Code's — a
directory of small markdown files, one fact each, with an index the
model reads every session and files it opens when it needs the whole
fact. Today (ADR-0006 §7, `bingo-context/src/memory.rs`) memory is
**one file per project**, 300 lines, whole file in every prompt,
written only by an extractor hook the person never sees. Three
things are wrong with it at once: the whole file is paid for on
every turn whether or not a line matters; the model cannot write a
memory itself (it has no idea the file exists); and a fact has no
name, so it cannot be corrected, only appended beside.

## Shape (the contract, fixture-tested first)

```
<data_dir>/memory/
  user/                      facts about the person, every project
    MEMORY.md                the index
    <slug>.md                one fact
  <project-key>/             keyed as today (git common root, digest)
    MEMORY.md
    <slug>.md
```

A memory file:

```markdown
---
name: <kebab-case slug, = the file name>
description: <one line, used to decide relevance>
type: user | feedback | project | reference
---

<the fact; feedback and project carry **Why:** and **How to apply:**
lines; [[other-slug]] links a related memory>
```

`MEMORY.md` is one line per memory: `- [Title](slug.md) — hook`.
The index is the fact "what memories exist"; the file is the fact
itself. Nothing is stored twice: the index carries no body, the body
carries no index.

## Bricks

1. **`memory::file`** (pure): parse and print a memory file
   (frontmatter + body), refusing a name that is not the file name;
   `memory::index`: parse/print `MEMORY.md`, add a line, drop a line,
   sorted stable. Fixtures for both, round-trip tests. This is the
   contract, before anything reads it.
2. **The contribution.** `MemoryContributor` contributes, as system
   blocks after the instructions: the **user** index and the
   **project** index (each capped at `INDEX_LINES` = 200 lines, the
   newest kept and the cut said, as today's file does), and one
   teaching paragraph in `bingo-core`'s identity or the contributor's
   own block (whichever ADR-0009 puts prose in — check): where the
   directory is (the absolute path, so `Write`/`Edit`/`Read` reach it
   with no new tool), the file format above, and the rules a good
   memory keeps — one fact per file; check the index for an existing
   file before adding; update rather than duplicate; delete a memory
   that turns out wrong; do not save what the repo already records;
   add the index line after writing the file. A memory's body is read
   by the model with `Read` when it wants it; nothing here loads
   bodies into the prompt.
3. **The extractor writes the same shape.** The end-of-turn hook
   (`hook.rs`) keeps its job but writes each fact as a file of type
   `project` with a slug derived from its first words (deduped on the
   slug; an existing slug is left alone), and appends the index line.
   `context.memory = false` still turns the hook off; the
   contribution stays on (the model may still write by hand).
4. **Migration, once.** The first time a project's contributor finds
   the old `<key>.md` and no `<key>/`, it moves the file to
   `<key>/imported.md` under a frontmatter of type `project` with
   description `what the old memory file held`, indexes it, and
   removes the old file. Test with a fixture of the old shape.
5. **`/memory`.** A bare command that answers a `View::Table` of both
   indexes (name, type, description, scope) — the same rows the model
   sees, for the person. Nothing more; editing is `Read`/`Edit` on
   the file, or the person's own editor.

## Files

`bingo-context/src/memory/{mod.rs,file.rs,index.rs,migrate.rs}`
(split of `memory.rs`), `hook.rs`, `lib.rs` (the command), the
identity or contributor prose, `docs/adr/0044-one-fact-one-file.md`
(≤80 lines: why files, why the index in the prompt and bodies on
demand, why the extractor stays), ADR-0006 §7 dated amendment,
settings fixture if a key is added.

## Exit criteria

- [ ] Fixture round-trips for a memory file and an index; a name that
      is not the file name is refused.
- [ ] The prompt carries both indexes and the teaching paragraph
      (snapshot), never a body.
- [ ] The extractor writes one file per fact and one index line;
      a repeated fact writes nothing.
- [ ] The old single file migrates once and is gone afterwards.
- [ ] `/memory` lists both scopes.
- [ ] All gates; the Windows cross-check for `bingo-context`.
- [ ] Hands-on: appended by the parent.

## Non-goals

Relevance ranking of memories into the prompt (the index is the
recall); a memory tool (the file tools are the tool); syncing memory
between machines; a cap on the number of files (the index cap is the
bound that matters).

## Risks

The teaching paragraph is model-facing text: it costs tokens on
every turn — keep it under ~120 words and snapshot it. Two sessions
of one project writing the index at once: append-with-rename as the
picture cache does, and a re-read before each write. The extractor
may produce a slug that collides with a hand-written memory of a
different meaning: the extractor never overwrites, so the worst case
is a lost extraction, which is the cheap side.
