# ADR-0044 — One fact, one file

Status: accepted · 2026-09-04 · Plan: M64

## Context

Memory is one file per project (ADR-0006 §7): 300 lines, all of it in every
prompt, written only by an extractor the person never sees. Three things are
wrong at once. The whole file is paid for on every turn whether or not a line
matters. The model cannot write a memory, because nothing tells it the file
exists. And a fact has no name, so it can be appended beside but never
corrected — a wrong line stays until age evicts it.

No kernel door is asked for: no `HostApi` verb, no field on a kernel type, no
trait verb. `memory` stays a noun of `bingo-context`, and the prose that
teaches it is the plugin's own `System` block (ADR-0009 §3), never the
kernel's identity.

## Decision

1. **A memory is a file.** `<data_dir>/memory/user/` holds what is true of the
   person in every project, `<data_dir>/memory/<project-key>/` what is true of
   one project, keyed as before by the git common root so worktrees share it.
   Each directory holds `MEMORY.md` and one `<slug>.md` per fact.
2. **A file is frontmatter and a fact**: `name` (the file's own name — a file
   that answers to another is refused, not corrected), `description` (one
   line), `type` (`user | feedback | project | reference`), then the body.
   Three keys and one line each, read by hand: a model writing with `Write`
   must parse here, and a parser is a smaller promise than a dependency.
3. **The index is a projection of the files.** A line is
   `- [Title](slug.md) — hook`, the title the slug read as words and the hook
   the file's own `description`, so a writer invents nothing that can later
   disagree with the file. Writing replaces the line of that slug and leaves
   every other line as it was; the newest entry is the last.
4. **Indexes reach the prompt, bodies never.** Both are contributed as system
   blocks under headings naming their absolute directory, each capped at 200
   lines with the newest kept and the cut said. A body reaches the model when
   it opens the file with the tools it already has — which is why the paths
   are in the prompt and why there is no memory tool. One teaching paragraph,
   cached and snapshotted, gives the format and the rules in under 120 words.
5. **The extractor keeps its job in the new shape**: one file of type
   `project` per fact, named from the fact's first words, the fact its own
   description. A name the directory already holds is left alone — whoever
   wrote that file knew more than a line, and a lost extraction is the cheap
   side. `context.memory = false` still turns the hook off and leaves the
   contribution on: the model may write by hand.
6. **Migration happens once.** A project whose directory is absent and whose
   old `<key>.md` is present gets `<key>/imported.md`, type `project`,
   described as what the old memory file held and indexed; then the old file
   is removed. A directory that exists has been through this.
7. **`/memory`** answers a `View::Table` of both scopes — scope, name, type,
   description — read from the files, for the person who cannot read the
   prompt. Correcting a memory is `Read` and `Edit`, or their own editor.
8. **Budget unchanged at 332.** `thiserror` joins `bingo-context` from the
   workspace tree; no crate is added.

## Consequences

- A turn pays for two index blocks and the teaching instead of a 300-line
  file, and pays for a body only when it asks for one.
- Two sessions of one project may write an index at once: each memory file and
  each index is written under a name of its own and renamed into place, and
  the index is re-read in the moment before it is rewritten. The worst case is
  a lost index line, never a lost file and never half of one.
- A slug is ASCII-safe, is cut to 48 bytes, and is moved aside when it would
  collide with `MEMORY.md` on a case-insensitive filesystem or with a Windows
  device name (`con`, `nul`, `com1`…).
- `files::block` takes its line cap from the caller: an index is not a file
  and says so with a smaller one.

## Supersedes

Amends ADR-0006 §7: the project memory file becomes a directory, gains a user
scope, and stops putting its bodies in the prompt. Refs: ADR-0009 §3.
