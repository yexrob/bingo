# 0006 — Context budget: the kernel measures and cuts, the plugin summarises and remembers

## Context

A long session must never 400 and must not pay for a summary that does not shrink anything. The old project had two rulers (a fixed 200k and the model's), four thresholds spread over three files, a summary step that could fail three times running and still be paid for, and a memory file that lost its newest lines first. Compaction is a strategy (what to say in the summary) but the numbers it fires on, the acceptance of its result, and the cheap cuts that need no model are the kernel's — one ruler, one breaker (plan §2.2, D169–D172).

## Decision

1. **One threshold family, in the kernel**, from `effective = window − max_tokens` (ADR-0004): microcompact at 50 %, warning at `trigger − 20 000`, compaction trigger at 90 %, kept tail 25 %. `Compactor::threshold` is removed; `ContextUsage.trigger` is the kernel's number.
2. **The ruler anchors on the server.** `used` = the last input total the provider reported for this session + the local estimate of what was added since. When the provider counts tokens, an exact count replaces the anchor every 5 rounds or after 20 000 estimated tokens of growth. The estimate alone never decides a compaction of a session the server has already measured.
3. **Microcompact is a projection, not a record.** Once `used` passes the micro line, tool results older than the last 10 and longer than 1 000 chars go to the provider as `[tool result elided: N chars]`, ids kept, journal untouched, transcript untouched; measured after projection. After an overflow the retry keeps only the last 4.
4. **A compaction is accepted only if it shrinks.** The plugin returns `Compaction{summary, boundary, kept, before, after, usage}`; `after ≥ before` discards it, bills `usage` to the turn, and counts one failure. Three consecutive failures trip the breaker: `Threshold` compactions are skipped with a notice until a compaction succeeds; `Overflow` compactions still run and `CompactContext.failures` tells the plugin to take its no-model rung (drop the oldest). A success resets the count.
5. **Overflow ladder.** First overflow: learn the window (ADR-0004), compact with `Overflow`, retry once with the forced microcompact. Second overflow in one turn fails the turn with `CONTEXT_OVERFLOW`.
6. **Observability is the journal.** `Item::Compaction{summary, replaced, before, after, duration_ms}` and `Event::Compacted` for every accepted cut; notices `CONTEXT_WARNING` (once per turn), `COMPACTION_USELESS`, `COMPACTION_SKIPPED`; `TurnUsage.context` every round.
7. **Memory is the plugin's**, in `bingo-context`: instruction files (`<config_dir>/AGENTS.md`, then `AGENTS.md` | `CLAUDE.md` in every directory from the git common root down to cwd) and one project memory file `<data_dir>/memory/<name>-<hash>.md` keyed by the git common root so worktrees share it, both contributed as system blocks. A file over 300 lines or 32 KB contributes its newest lines and says what was left out — never the oldest, never silently. A hook at turn end asks the model for facts worth keeping when the turn ran a tool, appends the new ones, drops exact repeats, evicts the oldest past 300 lines. `context.memory = false` turns the hook off.
   *(Amended 2026-09-04, M64/ADR-0044: the one project file is a directory of one-fact markdown files, and there are two scopes — `<data_dir>/memory/user/` and `<data_dir>/memory/<key>/`, each with a `MEMORY.md` index. The prompt carries the two indexes, capped at 200 lines with the newest kept and the cut said, plus one teaching paragraph; a body reaches the model only when it opens the file with the tools it already has, so the model can now write and correct a memory itself. The hook keeps its job and writes the same shape, one file per fact. `context.memory = false` still turns only the hook off. The old single file migrates once into `<key>/imported.md`.)*
8. **Learned windows persist** in `<data_dir>/learned-windows.json`, written on each lesson, read at host build.

## Consequences

- sdk changes, one round: `Compactor::threshold` removed; `CompactContext` gains `failures` and `keep_budget`; `Compaction` gains `usage`; `HookContext` gains `provider` and `model` so a hook can ask the model. Touched: `bingo-core`, the new `bingo-context`; no other plugin implements these traits.
- Every cut the kernel makes is a pure function on `&[Message]` or `&[Item]`, tested on random journals like the fold (ADR-0005).
- The transcript never loses a byte to microcompact; only the wire does.
- A memory line is at most one extraction request per tool-using turn away; a provider without credentials means no memory, not a failed turn.

## Supersedes

—
