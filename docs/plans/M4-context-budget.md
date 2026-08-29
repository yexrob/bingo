# M4 — Context: one ruler, cheap cuts, a paid summary, memory

## Goal

A long session never 400s and never pays for a summary that does not shrink anything: the kernel measures against the server's own count, elides stale tool results on the wire before it ever summarises, accepts a summary only when it helps, and stops asking for one after three useless tries. Instruction files and a project memory reach the prompt with their newest lines intact, and the model leaves facts behind at the end of a working turn.

## Bricks, in build order (owner)

1. **ADR-0006 + sdk** (kernel) — the threshold family and the acceptance rule are boundary decisions. `Compactor::threshold` goes (the kernel's `ContextUsage.trigger` is the one number); `CompactContext{failures, keep_budget}`; `Compaction.usage`; `HookContext{provider, model}` so a turn-end hook can ask the model. One sdk change.
2. **Ruler** (kernel, `bingo-core::context::budget`) — pure `Thresholds::of(window, max_tokens)` → `{effective, micro, warn, trigger, keep}`; `Anchor{server, estimate}` folded into `ContextUsage.used = server + (estimate − estimate_at_anchor)`, set from every response's `input_total()`; `count_tokens` through the provider every 5 rounds or +20 000 estimated since the last exact count, when the endpoint counts; `CONTEXT_WARNING` once per turn past the warn line.
3. **Microcompact** (kernel, `context::elide`) — pure `elide_old_results(messages, keep_recent, min_chars) -> Option<Vec<Message>>`: older `ToolResult` parts become `[tool result elided: N chars]`, ids and `is_error` kept; applied in `assemble()` past the micro line with `keep_recent = 10`, and with `4` on the retry after an overflow; measured after.
4. **Breaker and acceptance** (kernel) — `TurnConfig.compaction: Arc<Breaker>` (per session): `after < before` accepts, splices and resets; otherwise discards, bills `usage`, notices `COMPACTION_USELESS`, counts; at three, `Threshold` compactions notice `COMPACTION_SKIPPED` and `Overflow` ones pass `failures` on. Learned windows persisted in `<data_dir>/learned-windows.json` (`Learned::load` at build, `save` on record).
5. **`bingo-context`** (worker A) — `Compactor`: split point = the later of "keep the last 12 items" and "keep the tail that fits `keep_budget`", moved forward past any tool result so no pair is cut; the summary request is the old prompt (`src/compact.rs:32-75` headings, `SUMMARY_MAX_TOKENS 4 096`, tool inputs echoed to 200 chars, `SUMMARY_PROMPT_RESERVE 2 000`), streamed through `cx.provider` and drained; `before`/`after` from the estimate of the items replaced versus the summary; `Overflow` with `failures ≥ 3`, or a summary that came back empty, takes the no-model rung: boundary at the split, summary `[earlier conversation dropped]`. Contributors: `InstructionsContributor` (System, order −10) reads `<config_dir>/AGENTS.md` then `AGENTS.md` | `CLAUDE.md` per directory from the git common root (`git rev-parse --git-common-dir`'s parent, else cwd) down to cwd, each as one block with its path; `MemoryContributor` (System, order −5) reads `<data_dir>/memory/<name>-<hash>.md`; both apply the cap (300 lines, 32 KB, newest kept, a trailing line saying `[… N earlier lines not shown]`). `MemoryHook` (Turn, End): when the turn's items include a completed tool call and `cx.provider` is present, sends the extraction prompt (`src/memory.rs:11-19`) over the turn's items capped at 60 000 chars, appends the returned lines that are not already present, evicts the oldest beyond 300, writes atomically; every failure is a `tracing::warn!`, never an error. Config claim `context` (`memory: bool`, default true).
6. **`bingo`** (kernel) — register `ContextPlugin`; `tests/cli.rs`: a scripted overflow followed by success shows `Compacted` on the wire and a `Compaction` item; a turn with `AGENTS.md` in cwd sends it (visible in the fake provider's recorded request through `--output-format json`? no — through a core host test instead); `--print` warns once at the warn line with a tiny declared window.

## Files

`docs/adr/0006-context-budget.md`, `crates/bingo-sdk/src/{compactor,hook}.rs`, `crates/bingo-core/src/context.rs` + `context/{budget,elide}.rs`, `crates/bingo-core/src/{turn,turn/config,turn/stream,host}.rs`, `crates/bingo-core/src/models/learned.rs`, `crates/bingo-context/src/{lib,compact,split,prompt,instructions,memory,files}.rs`, `crates/bingo/src/main.rs`, `crates/bingo/tests/cli.rs`.

## Dependencies

None new. (`bm25` recall waits for a memory larger than its 300-line prompt.)

## Exit criteria

- [ ] `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `scripts/check_discipline.sh`, `scripts/budget.sh`, `cargo deny check`
- [ ] Ruler: thresholds from a declared 50k window / 10k max_tokens are 20k / 16k / 36k / 10k; `used` never drops below the server's last count; an exact count replaces the anchor on the 5th round and on +20k
- [ ] Microcompact (proptest): ids and `is_error` unchanged, the last N results untouched, anything under `min_chars` untouched, the projection is idempotent, the journal items unchanged; the turn sends the elided form past the micro line and the full form below it
- [ ] Breaker: a compaction with `after ≥ before` is discarded, billed and counted; the third trips it; a `Threshold` under a tripped breaker is skipped with a notice; a success resets; `Overflow` passes `failures`
- [ ] Ladder (scripted): overflow → `Compacted` + `Compaction` item → retry succeeds; overflow → useless summary → retry still fails → `CONTEXT_OVERFLOW`, one turn; learned window on disk after the first overflow
- [ ] Context plugin: split never cuts a tool pair (proptest); the summary request body snapshot; instruction files in root-to-cwd order with paths; a 400-line memory contributes lines 101–400 and says so; the hook appends new facts, skips repeats, evicts the oldest past 300, skips a turn without a tool call, survives a failing provider; worktree and main checkout resolve to one memory file
- [ ] Black-box: scripted overflow-then-success prints the answer and shows `compacted` in `--output-format json`; `CONTEXT_WARNING` once with a tiny declared window
- [ ] sdk changed exactly once, ADR-0006 lists what it touched

## Non-goals

BM25 recall over memory (M6+). Persisting compaction beyond the `Compacted` frame (already durable). The `/compact` command (M5, with the dispatcher). Compaction of sub-session children (M8). Prompt caching of the summary block (the provider's, M-later). An LSP diagnostics contributor (later plugin).

## Risks touched

R7 — every cut is a pure function with a proptest; the fake provider still refuses an orphan tool result. R3 — nothing new to compile. R4 — the summary prompt is snapshotted so a wording change is a deliberate diff.
