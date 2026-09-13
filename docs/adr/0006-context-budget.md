# 0006 — Context budget: the kernel measures and cuts, the plugin summarises and remembers

## Context

A long session must never 400 and must not pay for a summary that does not shrink anything. The old project had two rulers (a fixed 200k and the model's), four thresholds spread over three files, a summary step that could fail three times running and still be paid for, and a memory file that lost its newest lines first. Compaction is a strategy (what to say in the summary) but the numbers it fires on, the acceptance of its result, and the cheap cuts that need no model are the kernel's — one ruler, one breaker (plan §2.2, D169–D172).

## Decision

1. **One threshold family, in the kernel**, from `effective = window − max_tokens` (ADR-0004): warning at `trigger − 20 000`, compaction trigger at 90 %, kept tail 25 %. `Compactor::threshold` is removed; `ContextUsage.trigger` is the kernel's number. *(M80/ADR-0048 removes normal microcompaction at 50 % to preserve request prefixes.)*
2. **The ruler anchors on the server.** *(ADR-0055: where the endpoint holds the context, its own reading replaces anchor and estimate, and no line is drawn.)* `used` = the last input total the provider reported for this session + the local estimate of what was added since. When the provider counts tokens, an exact count replaces the anchor every 5 rounds or after 20 000 estimated tokens of growth. The estimate alone never decides a compaction of a session the server has already measured.
3. **Overflow elision is a projection, not a record.** After an overflow the retry keeps the last 4 tool results whole; older results longer than 1 000 chars go to the provider as `[tool result elided: N chars]`, ids kept, journal untouched, transcript untouched; measured after projection. Normal requests preserve their results rather than continually shifting a cutoff through cached history (M80/ADR-0048).
4. **A compaction is accepted only if it shrinks.** The plugin returns `Compaction{summary, boundary, kept, before, after, usage}`; `after ≥ before` discards it, bills `usage` to the turn, and counts one failure. Three consecutive failures trip the breaker: `Threshold` compactions are skipped with a notice until a compaction succeeds; `Overflow` compactions still run and `CompactContext.failures` tells the plugin to take its no-model rung (drop the oldest). A success resets the count.
5. **Overflow ladder.** First overflow: learn the window (ADR-0004), compact with `Overflow`, retry once with the forced microcompact. Second overflow in one turn fails the turn with `CONTEXT_OVERFLOW`.
6. **Observability is the journal.** `Item::Compaction{summary, replaced, before, after, duration_ms}` and `Event::Compacted` for every accepted cut; notices `CONTEXT_WARNING` (once per turn), `COMPACTION_USELESS`, `COMPACTION_SKIPPED`; `TurnUsage.context` every round.
7. **Memory is the plugin's**, in `bingo-context`: instruction files (`<config_dir>/AGENTS.md`, then `AGENTS.md` | `CLAUDE.md` in every directory from the git common root down to cwd) and two directories of one-fact markdown files — `<data_dir>/memory/user/` and `<data_dir>/memory/<key>/`, the key taken from the git common root so worktrees share it — each behind a `MEMORY.md` index. Instructions and the two indexes are System content, captured in context-owned journal extensions and reused between lifecycle boundaries: they refresh on reopen, accepted compaction or rewind, not on each file edit, and internal runtime context is never drawn as conversation. The prompt carries the two indexes, capped at 200 lines, plus one teaching paragraph; a body reaches the model only when it opens the file with the tools it already has, so the model can write and correct a memory itself. A file over 300 lines or 32 KB contributes its newest lines and says what was left out — never the oldest, never silently. A hook at turn end asks the model for facts worth keeping when the turn ran a tool, writes one file per fact and drops exact repeats. `context.memory = false` turns only the hook off. The one project file of the first cut migrates once into `<key>/imported.md`.
   *(Amended 2026-09-04, M64/ADR-0044: was one project memory file `<data_dir>/memory/<name>-<hash>.md` contributed whole; two scoped directories of one-fact files behind an index replaced it, and the model can now write a memory itself.)*
   *(Amended 2026-09-07, M80/ADR-0048: was rebuilt into every request; instructions and indexes now sit in context-owned journal extensions and refresh only at lifecycle boundaries.)*
   *(Amended 2026-09-08, M83/ADR-0049: the turn-end hook, `context.memory` and the migration are gone; the model is the one writer, the project key is the root commit, and an index is capped at 60 lines.)*
   *(Amended 2026-09-08, M84: the plugin publishes where the two memory directories are as journal state, `_bingo.context`/`memory`, once per session start, so the TUI can draw a call on a memory file as `Recall from memory(…)`. A turn-end recap hook and its `context.recap` key were built and taken away the same day on the user's word; the plugin claims no settings.)*
8. **Learned windows persist** in `<data_dir>/learned-windows.json`, written on each lesson, read at host build.

## Consequences

- sdk changes, one round: `Compactor::threshold` removed; `CompactContext` gains `failures` and `keep_budget`; `Compaction` gains `usage`; `HookContext` gains `provider` and `model` so a hook can ask the model. Touched: `bingo-core`, the new `bingo-context`; no other plugin implements these traits.
- Every cut the kernel makes is a pure function on `&[Message]` or `&[Item]`, tested on random journals like the fold (ADR-0005).
- The transcript never loses a byte to overflow elision; only the wire does.
- A memory line is at most one extraction request per tool-using turn away; a provider without credentials means no memory, not a failed turn.

## Supersedes

—
