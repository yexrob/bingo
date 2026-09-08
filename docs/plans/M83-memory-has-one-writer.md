# M83 — Memory has one writer

## Goal

User, 2026-09-08: bingo reused a dead project's memory. The `dnf` scope
held 101 files, 97 written in one morning, none by hand: the extractor
hook (ADR-0044 §5) is blind — it sees one turn, never the index it adds
to, never the tree, never a clock — so it can only append. A change of
direction became a sixth wording of the same fact beside the first,
`Three.js 2.5D` on line 1 and `no 2.5D` on line 34, all of it in the
system prompt every turn (27 KB), and when the project was deleted and
begun again at the same path the key was the same, so the old prompt
came back whole. The teaching the model reads is right; the writer that
does not read it is the bug. Across every scope on that machine: 540
files, 3 by hand.

Three changes. **The extractor goes**: the model is the one writer, under
the teaching it already has, as Claude Code does it. **The project is
its root commit**: a repository begun again is a new project with an
empty memory; a worktree or a second clone is the same one. **The index
is a hint, not a document**: 60 lines, the newest kept and the cut said.

## Bricks, in build order

1. **`root::commit(root)`** (pure over one `git` call): the lowest of
   `git rev-list --max-parents=0 HEAD`, so a repository with two roots
   still answers one thing; `None` outside git or before the first
   commit. Tests on a real repository: a worktree answers as its checkout,
   two repositories answer differently, an empty one answers nothing.
2. **`dir::key(root, commit)`**: `<name>-<commit[..16]>` when there is a
   commit, `<name>-<fnv(path)>` when there is not — one shape, and the
   path key is the key of today, so a directory outside git keeps its
   memory. `memory::project_dir(data_dir, cwd)` composes root, commit and
   key in one place for the contributor and the command.
3. **Subtraction**: `hook.rs` (the extractor), `migrate.rs` (ADR-0044 §6
   ran once for every project that had a file; the two left are
   orphans of the old key anyway), the `context` config claim and
   `context.memory`. An old `context.memory` in a settings file is
   reported as an unknown key, which is what it is. The fake provider's
   `side` deck stays: compaction is a side question too.
4. **`INDEX_LINES` 200 → 60.** The cut line already says what was left
   out; sixty lines that will not fit are sixty lines to merge.
5. **The teaching** gains the two sentences the extractor's absence needs
   — when to write (a fact about the person, a correction, a decision the
   tree does not record) and how to read (background, outranked by the
   conversation and the tree; a memory they contradict is fixed, not
   followed) — and stays under 200 words. Snapshot updated.
6. **Black box**: `/memory` on an empty project says where the directory
   is; a scripted turn writes a file and its index line there with the
   tools it has; the next `/memory` lists it. The extractor's black box
   goes with the extractor.
7. **Records**: ADR-0049; dated amendments in ADR-0006 §7, ADR-0044 §5,
   ADR-0014 §1; sdk and fake-provider comments that name the extractor.

## Files

- `crates/bingo-context/src/{root,lib}.rs`, `src/memory/{mod,dir,teach,command}.rs`;
  deleted: `src/hook.rs`, `src/memory/migrate.rs`
- `crates/bingo/tests/cli/{context,prefix,compaction_prefix}.rs`
- `crates/bingo-sdk/src/hook.rs`, `crates/bingo-provider-fake/src/lib.rs` (comments)
- `docs/adr/0049-memory-has-one-writer.md`, `README.md`, `0006`, `0044`, `0014`

## Exit criteria

- [x] no hook writes memory; the plugin registers five contributions
- [x] a repository's scope is keyed by its root commit; a worktree shares
      it; a directory outside git keeps today's key
- [x] the index block carries at most 60 lines and says the cut
- [x] the teaching snapshot changed by the two sentences and no more
- [x] black box: write by hand, read back by `/memory`
- [x] every gate green

## Non-goals

- Migrating the path-keyed scopes on disk. They are 540 extractor lines
  and 3 by hand; a person who wants one moves the directory to the new
  key. Nothing is deleted.
- A memory tool. The paths are in the prompt; `Read`/`Write`/`Edit` reach
  them (ADR-0044 §4).
- Dating memories in frontmatter. `mtime` is the date and the index is
  newest-last; a second clock would be a second representation.

## Risks

- R-window: a memory written in a repository before its first commit sits
  under the path key and is not seen after the commit. Rare, cheap, and
  the model is told to check before relying; named in the ADR.
- R-shallow: a shallow clone's root is its grafted boundary, so it keys
  apart from the full clone. Memory is local; accepted.
- R-quiet: with no extractor a project the model never writes about has
  no memory. That is the point; the cost of a missed fact is one line,
  the cost of a flood was a project steered by its own past.

## Verified (2026-09-08)

The subtraction went further than the plan's brick 3 named, because the
write side of the store had exactly one caller. `hook.rs`, `migrate.rs`,
`transcript.rs` (the turn as the extractor read it), `memory/index.rs`
(parsing and rewriting `MEMORY.md`), `file::slug` and its Windows-name
guard, `store::{save, holds, swap, beside}` and `stream::drain` are gone;
`file::print` and a plain `store::save` survive under `cfg(test)` as the
fixture writers the parser is checked against. `bingo-context` drops
`schemars` (the config claim was its one use); the lock file loses that
edge and nothing else. 458 lines out, 270 in, most of the 270 records.

`root::commit` is one more `git` call beside `--git-common-dir`, through
one `git()` helper both now share. The test that two repositories begin
differently first failed: two checkouts seeded with the same bytes in the
same second are, to `git`, the same commit — so `Repo::begun_with(seed)`
seeds them apart, and the comment on it says why.

The teaching is 193 words; the snapshot diff is the three sentences the
plan named and the reflow around them.

```text
== fmt / check / clippy (-D warnings)   exit 0
== bingo-context                         88 passed
== cli context:: prefix:: compaction_prefix::   8 passed
== test --workspace --no-fail-fast       88 suites, 4277 passed, 0 failed, 2 ignored
== discipline                            ok (pre-existing plan-length warns only)
== budget                                ok
== check --target x86_64-pc-windows-msvc bingo-context   ok
```

Not done here: the site's settings tables lose their `context.memory`
row in `../bingo-site` (two lines, committed there, not pushed).
