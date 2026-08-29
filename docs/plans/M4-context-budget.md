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

- [x] `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `scripts/check_discipline.sh`, `scripts/budget.sh`, `cargo deny check`
- [x] Ruler: thresholds from a declared 50k window / 10k max_tokens are 20k / 16k / 36k / 10k; `used` never drops below the server's last count; an exact count replaces the anchor on the 5th round and on +20k
- [x] Microcompact (proptest): ids and `is_error` unchanged, the last N results untouched, anything under `min_chars` untouched, the projection is idempotent, the journal items unchanged; the turn sends the elided form past the micro line and the full form below it
- [x] Breaker: a compaction with `after ≥ before` is discarded, billed and counted; the third trips it; a `Threshold` under a tripped breaker is skipped with a notice; a success resets; `Overflow` passes `failures`
- [x] Ladder (scripted): overflow → `Compacted` + `Compaction` item → retry succeeds; overflow → useless summary → retry still fails → `CONTEXT_OVERFLOW`, one turn; learned window on disk after the first overflow
- [x] Context plugin: split never cuts a tool pair (proptest); the summary request body snapshot; instruction files in root-to-cwd order with paths; a 400-line memory contributes lines 101–400 and says so; the hook appends new facts, skips repeats, evicts the oldest past 300, skips a turn without a tool call, survives a failing provider; worktree and main checkout resolve to one memory file
- [x] Black-box: scripted overflow-then-success prints the answer and shows `compacted` in `--output-format json`; `CONTEXT_WARNING` once with a tiny declared window
- [x] sdk changed once for the contracts (ADR-0006 lists what it touched), plus one additive module `bingo_sdk::tokens` and `BREAKER_TRIP` so no plugin restates the ruler

## Non-goals

BM25 recall over memory (M6+). Persisting compaction beyond the `Compacted` frame (already durable). The `/compact` command (M5, with the dispatcher). Compaction of sub-session children (M8). Prompt caching of the summary block (the provider's, M-later). An LSP diagnostics contributor (later plugin).

## Risks touched

R7 — every cut is a pure function with a proptest; the fake provider still refuses an orphan tool result. R3 — nothing new to compile. R4 — the summary prompt is snapshotted so a wording change is a deliberate diff.

## Verified (2026-08-29, commit 9d8e41d)

```
$ cargo fmt --all -- --check                                        exit 0
$ cargo check --workspace --all-targets --locked                    exit 0
$ cargo clippy --workspace --all-targets --locked -- -D warnings    exit 0
$ cargo test --workspace --locked                                   exit 0
  bin (cli) 24 · core 105 · sdk 19 · context 66 · store-jsonl 34 · print 34 · provider-fake 19
  provider-anthropic 68 · provider-openai 95 · tool-fs 69 · tool-bash 51 · tool-web 77 · permissions 92 = 753 passed
$ scripts/check_discipline.sh                                       exit 0 (no warnings)
$ scripts/budget.sh                                                 dependencies 215 (max 260); no crate added
$ cargo deny check                                                  advisories ok, bans ok, licenses ok, sources ok
```

Exit criteria, item by item:

- Ruler: `Thresholds::of(50_000, 10_000)` = 20k / 16k / 36k / 10k; proptest `used_never_drops_below_the_servers_count`; a turn whose provider reports 5 000 input tokens measures ≥ 5 000 on the next round; the recount rule on rounds and growth (`Anchor::recount_due`).
- Microcompact: proptest on ids, `is_error`, the untouched tail and idempotence; a turn with twelve 2 000-char results sends ten whole and two elided past the micro line, all twelve whole below it.
- Breaker: a `9 000 → 8 000` cut is discarded, billed (`output_tokens` +20), counted and noticed; three trip it and a `Threshold` is `COMPACTION_SKIPPED`; a shrinking cut splices, records `Compaction`, emits `Compacted`, resets; `Overflow` passes `failures` and the retry succeeds.
- Ladder: scripted overflow → learned window on disk → forced microcompact retry (kernel-only, `an_overflow_is_retried_once…`); fourteen tool rounds → overflow → the summary the strategy asks for → `Compacted` + `Compaction{replaced ≥ 2, after < before}` → the retry answers (`an_overflow_after_many_rounds…`).
- Context plugin: the split proptest never cuts a tool pair; the summary request snapshot; instruction files root-to-cwd with paths and the user file first; a real `git worktree add` resolves to the main checkout's root; a 400-line memory contributes the last 300 and says so; the hook appends, skips repeats, evicts past 300, skips a turn without a tool call and a session without a provider, survives a failing provider.
- Black-box: `CONTEXT_WARNING` exactly once across two rounds with a 30k declared window; a tool-using turn writes two facts into `~/.bingo/data/memory/<key>.md` and a turn without a tool leaves it as it was.

Decisions taken while integrating (each is a commit body too):

- The kernel owns the threshold family; `Compactor::threshold` is gone. `ContextUsage.window` is now the *effective* window (input side), so a percentage means what a person expects.
- The estimate is `bingo_sdk::tokens`, one copy, used by the kernel and the plugin; `BREAKER_TRIP` sits beside `CompactContext::failures`. Both were duplications the worker reported.
- A summary that came back empty still bills the request that produced it; only the rung that made no request bills nothing.
- Overflow retries once whether or not a compactor is registered: the forced microcompact is the kernel's own rung.
- The turn loop grew past 700 lines and was split by noun: `turn/ruler.rs`, `turn/contributors.rs`, `session/spawn.rs`.

Open, carried forward:

- `ContextView::fold` is the kernel's; the plugin's `before` counts an item's own content without the fold's wrappers, a few tokens under the kernel's reading. Harmless for acceptance; visible in `Item::Compaction.before`.
- Live smokes against Anthropic and OpenAI (M1, M2) — still need keys; a compaction and a memory extraction against a real model have never run.
- The memory hook's extraction request runs before `TurnCompleted`, bounded at 30 s; a `--print` run on a slow model waits for it. Moving it after the outcome needs a post-turn hook point (M5, with the dispatcher).
- `Plugin::start` still cannot see `Env` (M3 carry-over); `HostHandle::env()` with M5's sdk change.
- `files::capped` drops a single line longer than 32 KB rather than keeping it; untested shape.
- No caching of `git rev-parse` (two subprocesses per round); revisit if it shows in a profile.
