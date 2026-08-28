# Delivery plan: milestones, porting checklist, verification, conventions (2026-08-28)

> Source: subagent report, archived verbatim. Facts were verified on the date in the title; re-verify before depending on a version.

I have what I need. Summary of the evidence base before the plan: the old repo is 148,754 LOC in one binary crate with 413 locked dependencies, `target/` at 14 GB, rustc 1.96; the new repo is an empty `main` with no commits. I read the survey, `AGENTS.md`, the discipline script, CI, `Cargo.toml`, the gap analysis, and extracted behaviors (with line numbers) from bash, permission, compact, budget, contract, providers, auth, transcript, storage, memory, hooks, executor, query loop, rewind, gfx, term, notify, keys, schema drift guard, the black-box tests, and the ACP/GUI research notes.

---

# bingo rewrite — delivery plan (build order, porting checklist, verification, conventions)

## 0. Ground truth that shapes the plan

- Old: 148.7K LOC / 208 files / one crate / 413 deps / 14 GB target / 1,844 tests (44% TUI). Social layer ≈27K LOC; five P0 correctness gaps still open (`notes/cc-gap-analysis.md:105-113`).
- Disease #4 (four event enums `StreamEvent→EngineEvent→AppEventPayload→UiEvent`, `src/engine/events.rs:31-114` vs `src/app/event.rs:364-403`) came from building the TUI against internal enums first and retrofitting the protocol in a 16-decision campaign (D140–D155). The build order below makes that impossible: the JSON event stream is the first consumer, the TUI is the third.
- Target for the rewrite: ~35–40K LOC across ~20 crates, ≤260 deps, `cargo check -p bingo-core` must not compile reqwest/ratatui/rmcp.

## 1. The arc — where I depart from the suggested order and why

| Suggested | Proposed | Why |
|---|---|---|
| M1 Anthropic … M7 OpenAI | OpenAI Responses (API-key, no OAuth) moves to **M2**, right after Anthropic | Risk #1 is trait churn. The `Provider` trait is only real once two wires with opposite shapes sit behind it (usage at `message_start` vs at end; `system[]` vs `instructions` string; count_tokens vs none). The Codex quirks are already known (`src/api/providers/openai.rs:985-1004`), so it is ~1.5K LOC. OAuth stays late (M9): it is an `AuthSource` capability, orthogonal to the wire. |
| M4 TUI before M6 RPC | **RPC surface (M5) before TUI (M6)** | "GUI-ready from day one" is only true if a non-TUI client exists before the TUI. `--print --json` (M0) is the first client; the stdio JSON-RPC server is the second; the TUI is then forced to be a client. Old `notes/research/acp-protocol-fit.md` and `gui-event-protocols.md` already picked the shape (Thread→Turn→Item, completed item authoritative). |
| M3 hooks/MCP/skills/memory together | Memory moves to **M4** with compaction; hooks+skills+MCP become **M7** after the TUI | Memory's P0 bugs are context-management bugs and share the `complete_text` brick with summaries. Hooks/skills/MCP are low-risk plugins; landing them after a usable TUI means they are dogfooded on day one instead of via `--print`. |
| M5 sub-agents before RPC | **Sub-agents M10**, after RPC, TUI, hooks, OAuth | A sub-agent is "a second session actor driven by a tool"; it needs session-scoped events, which the RPC surface forces. Doing it earlier is how the old repo grew the social layer before the kernel was correct. |
| M6 RPC + ACP together | **ACP is its own late milestone (M12)** | Old research decided: native RPC is the parity contract, ACP is a sibling adapter (`acp-protocol-fit.md:9-13`). Separate so RPC is not shaped by ACP's narrower session model. |
| TUI polish "later" | TUI MVP (M6) deliberately excludes inline mode, images, pager, themes; those are **M11** with a virtualization benchmark first | `cc-gap-analysis.md:159` says profile 1k/5k messages before virtualizing. |

Every milestone remains usable from the terminal: M0–M5 via `--print`/`--json`, M6+ via the TUI.

## 2. Milestones

Assumed crate map (names adjusted to one convention: `bingo-<kind>-<name>`; coordinate with the kernel/gateway architects, ADR-0001):

```
crates/bingo-sdk            types + traits (serde, schemars, async-trait only)
crates/bingo-core           loop state machine, session actor, registry, event log, permission gate, executor, context policy
crates/bingo-provider-fake  scripted provider (in-process + optional loopback SSE server, feature "loopback")
crates/bingo-provider-anthropic | bingo-provider-openai
crates/bingo-tool-fs | bingo-tool-bash | bingo-tool-web
crates/bingo-surface-print | bingo-surface-rpc | bingo-surface-tui
crates/bingo-hooks | bingo-skills | bingo-mcp | bingo-memory | bingo-subagents
crates/bingo-acp | bingo-channel-<im>            (late)
crates/bingo                the binary; the only crate that depends on every plugin
schema/   docs/adr/   docs/plans/   scripts/
```

### M0 — Walking skeleton
- **Goal:** `bingo --print --provider fake "hi"` streams a scripted reply through the real loop, executing one tool round, with `--json` emitting the canonical event stream.
- **Contents:** `bingo-sdk` (Message/ContentBlock incl. `Image` and `Thinking{signature}`, `StreamEvent`, `Event` with durable/ephemeral split, `Tool`, `Provider`, `PermissionDecision`, `Interrupt{reason}`/`InterruptBehavior`, `ErrorCode`), `bingo-core` (accumulator, executor, loop as a pure state machine `step(input) -> effects`, session actor driving effects, in-memory event log), `bingo-provider-fake`, `bingo-tool-fs` with `Read` only, `bingo-surface-print`, `bingo` binary, CI.
- **Brick:** the pure loop machine and the single `Event` enum. Everything after is a client or an effect handler.
- **Exit:** `cargo test --workspace` green; loop tests (text-only turn; tool round; interrupt mid-stream keeps text/drops tool_use and fills orphan results; in-stream retry restarts the response; empty-response retry once; max rounds); JSON round-trip fixtures for every `Event`/`ContentBlock` variant; black-box: `--print` stdout is prose only, `--json` is one `Event` per line, `[error] code=… msg=…` on non-TTY stderr with exit 1.
- **Size:** ~2.5K LOC, ~20 files.

### M1 — Real provider, real tools, permission gate
- **Goal:** Claude Code-shaped coding turns against Anthropic with `Read/Glob/Grep/Edit/Write/Bash/AskUserQuestion`, gated by the five modes and rule tables.
- **Contents:** `bingo-provider-anthropic` (SSE parser, retry ladder, context-limit recompute, thinking, cache_control), `bingo-tool-fs` complete (+ rewind snapshot capability), `bingo-tool-bash` (interactive rejection, periodic auto-background, process tree, truncation, live tail events), `bingo-core::permission` (typed decision, 7-step order, shell splitting), settings (3 layers with tri-state), system prompt (CLAUDE.md/AGENTS.md/env block), interaction brick (permission prompt and AskUserQuestion answered on stderr/stdin under `--print`).
- **Brick:** `PermissionDecision` with `DecisionReason::{Rule,Mode,Hook,Safety,User}` resolved in exactly one function; no `Ask` arm anywhere else (kills `src/query.rs:1303,1819` `unreachable!`).
- **Exit:** permission table ported as a case file (every test in `src/permission.rs:490-1090` has a row); shell-split property tests; bash rejection table tests; Edit uniqueness/Write-overwrite tests; Anthropic adapter wire tests from recorded SSE fixtures; loopback fake speaks Anthropic wire so the real adapter is black-box tested; one manual live smoke recorded in the plan file.
- **Size:** ~5K LOC.

### M2 — Second provider proves the trait; model catalog; web tools
- **Goal:** `--provider openai` works with the Responses API (API key); model windows resolve through declared > family file > prefix table > default > learned clamp.
- **Contents:** `bingo-provider-openai` (default variant only; Codex variant's body isolation kept behind an enum so M9 is additive), model catalog + `model-catalog.json` two-owner file + 24h `/v1/models` cache + learned windows, vision gating, `bingo-tool-web` (WebFetch with preapproved domains, WebSearch).
- **Brick:** `ModelResolver` as a pure function of (declared, overrides, prefix table, clamps).
- **Exit:** request-body assertions for both variants (`openai.rs:932-1004` as spec); threshold hierarchy property test (`src/budget.rs:78-136`); `sdk` unchanged or changed once with an ADR listing every plugin touched.
- **Size:** ~2K LOC.

### M3 — Sessions: durable log, resume, typed interrupt, turn budget
- **Goal:** `--continue` / `--resume <stem>` replay a session exactly; Esc/Ctrl-C semantics are typed and per-tool; runaway loops stop.
- **Contents:** transcript as the durable subset of the event log (records: message, turn-open, compact marker, interrupt), sidecar lock, GC policy, `TurnBudget{max_rounds, max_retries}`, `InterruptBehavior` honored by the executor (`Cancel` drops, `Block` awaits with the tool's own timeout), session-owned cwd, asset store for images.
- **Brick:** `project(records) -> Vec<Message>` as a pure function with an invariant test "every projection is API-valid" (paired tool_use/tool_result, no empty assistant, no unsigned thinking).
- **Exit:** resume fixtures (torn line, marker, interrupt marker); Windows-safe lock test; `--max-turns` under `--print` exits with a named event; Bash mid-flight interrupt returns a real result, Read is cancelled.
- **Size:** ~2K LOC. ADR: "transcript = durable event records; messages are a projection" (decide with kernel architect).

### M4 — Context: compaction ladder, microcompact, observability, memory
- **Goal:** long sessions never 400 on overflow and never pay for useless summaries; memory files are correct.
- **Contents:** token estimate + `TokenGate`, auto-compact at 90% effective window, overflow ladder (summary → truncate oversized → drop oldest), marker-first persistence, request-time microcompact projection, `CompactOutcome` event + rapid-refill breaker, `bingo-memory` (memdir with recency-first extraction input, line+byte caps, no silent drops, git-common-root key).
- **Brick:** every rung a pure function over `&[Message]` with the fake provider supplying summaries.
- **Exit:** ladder tests with a fake `Overflow` step; "summary that does not shrink is discarded and charged" test; microcompact keeps ids and recent N pairs; memory: 300-line file gains a new fact and evicts oldest; worktree A/B share memory.
- **Size:** ~2K LOC.

### M5 — RPC surface (the GUI contract)
- **Goal:** `bingo rpc` serves sessions over JSON-RPC 2.0/NDJSON on stdio with a committed, drift-checked schema.
- **Contents:** `bingo-surface-rpc`: initialize/capabilities, session list/create/resume/close, submit (one path), interactions answered by id, snapshot + gapless event sequence + resync, bounded per-client channel with `CLIENT_TOO_SLOW`; `schema/` bundle generation; black-box harness spawning the binary with the loopback fake.
- **Brick:** `Event` is already the wire; the surface adds envelopes and ids only. Zero private mirror types (CI greps the surface crate for `enum .*Event`).
- **Exit:** 15–20 black-box scenarios (handshake refusal, stdout purity, interrupt reaching a run, permission via interaction, retry visible, resume with marker); schema drift test; camelCase property test.
- **Size:** ~3K LOC.

### M6 — TUI MVP (a client)
- **Goal:** daily-driver terminal UI: fullscreen only, composer with history, streaming transcript, tool rows with verbs, permission dialog (Yes/session/No+feedback, Ctrl-E diff), Esc ordered stack, Ctrl-C twice, `/model /provider /think /permission /compact /resume /clear /help`, `?` key table, context bar, title/bell.
- **Contents:** `bingo-surface-tui` consuming the same in-process `Event` channel the RPC uses (bounded; resync via snapshot), `TestBackend` Recorder, tmux smoke script.
- **Brick:** render is a pure function `(snapshot, viewport) -> rows`; the event loop never awaits the core.
- **Exit:** TestBackend tests for every dialog and the permission flow; tmux smoke green on macOS+Linux; the TUI crate has no dependency on any provider/tool crate (CI-verified).
- **Size:** ~4K LOC.

### M7 — Extension plugins: hooks, skills, MCP
- **Contents:** `bingo-hooks` (10 old events + PermissionRequest/PostToolUseFailure/CwdChanged, `BINGO_ENV_FILE`, hook `ask` goes through the M1 gate), `bingo-skills` (SKILL.md, layers, cache, bundled `guide`), `bingo-mcp` (rmcp stdio+HTTP, boot-time concurrent dial, 5s timeout, stderr to log file, readOnlyHint untrusted, `/mcp`).
- **Exit:** hook `ask` → prompt → allow (the P0 regression test); 64KB-stdin no-deadlock test; MCP tool folds under server key; `cargo tree -p bingo-core` still free of rmcp.
- **Size:** ~2.5K LOC (7a hooks+skills, 7b MCP; parallelizable).

### M8 — Diagnostics & config UX
- `/hooks /memory /doctor /mcp` panels, single command registry with argument completion sources, scoped settings writes, error-detail folding. ~1K LOC. (Small; can merge into M7 or M11.)

### M9 — OAuth + Codex variant
- `AuthSource` capability, `auth.json` 0600 atomic, loopback PKCE (1455, fallback), device flow, eager refresh 300s single-flight, permanent-failure classes, `/provider login codex`, Codex body isolation + dynamic model list. Exit: flow tests against a fake issuer on loopback; `codex_request_params_isolation` ported. ~1.5K LOC.

### M10 — Sub-agents
- `Agent` tool, `.bingo/agents/*.md` definitions (model/provider/thinking/inherit_system), one session actor per instance, completion notifications injected at round boundaries, `SendMessage` to a running instance, TUI/RPC show instances as conversations. Explicitly not: teams, rooms, hires, @-debt, ack chasing, avatars. ~2K LOC. Cap enforced by risk #5.

### M11 — TUI depth
- Inline mode (write-once scrollback, DECSTBM push), kitty images (probe + tmux passthrough), markdown/highlighting, Ctrl-O pager, themes, OSC notifications, rewind UI (Esc Esc), background dialog, block-level virtualization (after a 1k/5k benchmark), `Task` tool + panel. ~6–8K LOC, several plan files.

### M12 — ACP adapter (`bingo-acp`, v1 stable; v2 behind negotiation). ~1.5K LOC.
### M13 — IM channel plugin (one; Telegram or Feishu) as a surface that maps messages to submits and events to replies. Greenfield; speculative. ~1.5K LOC.
### Later, each behind an ADR with a user story: teams/rooms, experience library, self-update. Never: `share`.

## 3. Porting-knowledge checklist (behaviors to re-implement; consult the pointer, do not copy)

Paths are under `/Users/yexrob/Episodes/Projects/bingo-inc/bingo/`.

### M0
- Stream vocabulary with `input_tokens` at both `MessageStart` and `StopReason`; accumulator folds both — `src/api/contract.rs:343-396`, tests `:932-957`.
- Accumulator `finish()` rules: unclosed text keeps `max_tokens`; unclosed tool_use dropped and marked truncated; unclosed thinking truncated; out-of-order block is an error — `src/api/contract.rs:605-640`, tests `:841-960`.
- Tool trait with fail-closed defaults; schema from schemars; no `$schema` key — `src/tool/mod.rs:107-150, 171-188`, test `:296`.
- Executor: consecutive concurrency-safe calls parallel ≤10, unsafe serial, insertion order; `FuturesUnordered` not `join_all`; completed results kept on cancel; acknowledge an already-set signal between batches — `src/tool/executor.rs:6, 43-86, 88-137`.
- Interrupt markers verbatim CC (`[Request interrupted by user]`, `…for tool use`); keep text + signed thinking only; fill orphan tool_results with an `is_error` placeholder — `src/query.rs:766-859`.
- In-stream retry restarts the whole response and discards the uncommitted attempt; 10 retries; server `retry_after` capped at 60s; retry policy injected, not `cfg!(test)` — `src/query_turn.rs:17-63`.
- Empty-response guard (retry once, named end reason) — `src/query.rs:44-47`, D124 `notes/research.md:5317`.
- Print contract: stdout = prose only; everything else stderr; prompt read from stdin only under `--print`; `--` for prompts starting with a subcommand name; non-TTY `[error] code= msg=` — `src/print.rs:16-18, 221-226`, `src/main.rs:106-108, 159-165, 226-233`.
- Stable error codes, exhaustive match with no `_` arm, drift tests — `src/error.rs:14-80, 165-180, 247-360`; `src/api/contract.rs:111-132`.
- **P0 typed interrupt (fixed here in sdk, verified M3):** `InterruptReason::{NewInput,UserCancel,Shutdown}`, `Tool::interrupt_behavior() -> {Cancel,Block}` default `Block`; replaces `watch<bool>` (`src/query_turn.rs:105,155`; `cc-gap-analysis.md:48,110`).

### M1
- Anthropic: `x-api-key`/`anthropic-version` headers `src/api/providers/anthropic.rs:432-436`; thinking is `{"type":"adaptive"}` only, `budget_tokens` 400s on Claude 5 `:189-204, 889-910`; ≤4 `cache_control: ephemeral` system blocks, off by default `:141, 165-183`, `src/system.rs:134`; HTTP retry 5×, retryable = 5xx|429, `retry-after` header `:455-475`; 400 `"A + B > C"` → recompute `max_tokens = clamp(C−A−1000, 3000..)` once `:481-497`; 60s idle guard per chunk `:36, 519`; `complete_text` streams underneath (proxies cut idle connections at 60–100s) `:577`, D171; Anthropic usage sums cache read/write, OpenAI does not `:226` vs `openai.rs:625-626`.
- Backoff: 500ms·2^(n−1)·[0.9,1.1], cap 32s, exponent ≤6 — `src/api/providers/mod.rs:255-290`. Broken pipe retries like a 5xx — D182 `notes/research.md:10065`.
- SSE: `\n\n` and `\r\n\r\n` boundaries across chunks, partial tail kept, 8MB cap is a protocol error — `src/api/sse.rs:3-100`.
- Error classification: overflow = 400/413 + phrase list; retryable/non-retryable message lists; "512 characters" must not read as 5xx; envelope message extraction — `src/api/contract.rs:47-103, 259-339`.
- Read: 20K chars, byte-bounded read, image files return a real image block, line ranges — `src/tool/read.rs:10-13, 78-125`. Edit: unique `old_string` or refuse (D174), `replace_all`, snapshot before write — `src/tool/edit.rs:21-27, 78-102`. Write: refuse to overwrite an unreadable file — `src/tool/write.rs:60-73`. Diff preview via `similar` (deadline-bounded Myers, D176) — `Cargo.toml:39-41`.
- Rewind recorder limits (50MB/200 checkpoints/8MB file; Bash writes uncovered) — `src/rewind.rs:1-40, 61-110`.
- Bash: constants `src/tool/bash.rs:11-18`; interactive rejection table with wrapper unwrapping, sudo flag rules, monitors (`-b` allowed), editors, file managers, TUI tools, gdb, REPLs, DB clients, ssh (-t / bare host), `docker exec -it`/attach — `:25-331`; periodic detection `:335-357`; input schema `:361-389`; description names the real shell dialect (#42/D71) `:420-439`; call order reject→periodic→background→foreground `:454-505`; result `$ cmd\n…\n[Exited with code N]`; foreground: `kill_on_drop`, process-tree kill on timeout, stdin null, drain readers 2s, Ctrl-B promotion keeps the same process/buffer `:535-654`; default notify condition `Errors` `:693-705`; live tail 5 lines / 100ms floor `src/live.rs:24-31`.
- Platform: shell dialect table, `shell_command`, `kill_process_tree`, `open_tty` — `src/platform.rs:56-93, 125, 161`.
- Permission (the spec): splitter separators/quotes/`$(` `src/permission.rs:71-118`; deny/ask any-match vs allow all-match + trusted `:123-135`; path normalization without fs lookups, trailing separator kept `:140-175`; `Skill(name:*)`, `prefix:`, `:*` stripped whole `:181-242`; `mcp__server` prefix rule `:246-270`; sensitive dirs `.git .claude .vscode .idea` + `confirm_reason` bypass-immune `:286-322`; 7-step order (deny → ask rules even in bypass → WebFetch preapproved → read-only except WebFetch/mcp__ → safety → bypass → acceptEdits → allow rules → mode fallthrough, Plan allows `Task*`) `:325-408`; session-scope rule only when it could silence the prompt, none for compound commands `:418-440, 567-612`. Gate: hook → rules → prompt with scope+diff; user feedback appended to the denial — `src/query.rs:490-560`.
- **P0 typed `PermissionDecision`** replaces `(behavior, String, input)` tuples — `cc-gap-analysis.md:62-63, 113`.
- Settings: layer paths and precedence `src/settings.rs:361-389`; merge rules (permissions accumulate, providers by name, mcpServers whole-replace, experimental OR-merge, hook lists concatenate) `:389-530`; scoped writes and union-list removal pitfalls `:558-600`. **Tri-state gap** (`false` cannot override `true`, `[]` cannot clear) `cc-gap-analysis.md:67` → explicit `Tri` and nullable lists from day one.
- System prompt: instruction files (`CLAUDE.md`, `.claude/CLAUDE.md`, `AGENTS.md`, `.agents/AGENTS.md`, `~/.claude/CLAUDE.md`), env block, model capability block — `src/system.rs:112-120, 137-160, 254-261`.
- AskUserQuestion shape (1–4 questions, 2–4 options, header ≤12 chars) — `src/tool/ask.rs:9-75`.
- Black-box CLI harness (isolated HOME/XDG, keys removed, `CARGO_BIN_EXE_bingo`) — `tests/cli_black_box.rs:1-80`.

### M2
- OpenAI Responses: variant enum and paths `src/api/providers/openai.rs:62-131`; Codex isolation (`store:false`, no `max_output_tokens`, stream-only, `include: reasoning.encrypted_content` vs `reasoning.summary_text`) `:239-284, 985-1004`; system → single `instructions` string `:248-251`; thinking → `reasoning.effort` levels `none…max` `:45-48`; two reasoning delta event names `:557-565`; `retry_after` ms vs s keys `:400-436`; `ChatGPT-Account-Id` from JWT `:166-174`; model list tolerant of `data[]`/`models[]` `:372`.
- Catalog tiers and learned clamps `src/api/models.rs:3, 497-534`; two-owner `model-catalog.json` `src/model_families.rs:27-31, 95-150`; 24h models cache `src/model_cache.rs:20-46`; learned windows parse/clamp `src/api/learned.rs:16-20, 101-140`; vision gating `src/api/client.rs:422-435`, `src/api/types.rs:117-141`.
- Budget hierarchy: `max_tokens ≤ window/2`, effective = window − max_tokens, compact at 90%, warn at −20K, keep tail ≤ effective/4 — `src/budget.rs:12-55`, tests `:78-136`.
- WebFetch limits/cache/https-upgrade `src/tool/webfetch.rs:10-23, 186-230`; preapproved domains `src/preapproved.rs:5-83`; WebSearch via DDG HTML, 20s, 8 results `src/tool/websearch.rs:10-12, 123-166`.

### M3
- Sidecar `.jsonl.lock` is the whole claim; data file never locked (Windows mandatory locks); in-process per-path lock map; rename moves the lock — `src/transcript.rs:28-56, 149-232`, D72 `notes/research.md:994`.
- Records `Message | Compact | Turn`; compact marker appended, canonical never rewritten (D74); projection through the latest marker; torn-line count surfaced only at entry boundaries (D175) — `src/transcript.rs:239-317, 503-610`.
- GC: 30 days / latest 100 / 24h grace; never delete a file that changed under you — `src/storage.rs:10-13, 178-340`. Data dirs `:62-124`.
- Turn-open marker is the rewind checkpoint; marker first, then message — `src/query.rs:867-887`.
- `--continue/--resume <stem>`; exit hint names the way back (D184) — `src/main.rs:93, 625-630`.
- Image asset store: sha256-addressed, `#[image N]` marker, 2000px/3.75MB/32MB limits — `src/api/image.rs:11-42, 110-167`.
- **New:** `TurnBudget` (`cc-gap-analysis.md:53, 90`).

### M4
- Constants and prompt headings — `src/compact.rs:16-75`; split = max(count cap 12, token cap) advanced past tool_result boundaries `:81-119`; ladder with target `min(effective, gate)·¾`, breaker skips only the summary rung, gate reset after change `:257-307`; a summary that does not shrink is discarded and charged (D172) `:412-430`; marker-first rewrite `:446-452`; idempotent middle-elision (output lands at half cap) `:496-539`; drop-oldest cannot fail and folds the carried summary `:544-590`; CJK 1 token/char, image = 1600 tokens even inside tool results, tool schemas counted `:605-668`; `TokenGate` every 5 turns or +20K, anchored on provider-reported input `:675-724`; count_tokens warning once; breaker pause message; warning notice goes to the surface not stderr `:730-794`. Circuit breaker 3 `src/budget.rs:52`; first-injection result cap 50K `:55`. Decision history D169–D172 `notes/research.md:9510-9748`.
- **P0 compaction observability + rapid-refill breaker; P1 microcompact projection** — `cc-gap-analysis.md:50-52, 112, 120`.
- **P0 memory lifecycle:** prefix-not-tail truncation bug `src/memory.rs:80-86`; silent drop after 200 lines `:157`; no byte cap; cwd-hash key splits worktrees `:30-57` → recency-first, line+byte caps, evict-oldest, git common root key (`cc-gap-analysis.md:66, 109`).

### M5
- Design and invariants: `notes/design/gui-app-server.md` §Resource model (230), §One submission path (451), §Server-initiated interactions (617), §Snapshots and recovery (662), §Lifecycle and ordering (703), §Errors/load/security (787). Item lifecycle started → deltas → completed-authoritative — `notes/research/gui-event-protocols.md:25-33`.
- Bounded per-attachment channel (1024) and `CLIENT_TOO_SLOW`; attachment is a view, never an owner — `src/app/mod.rs:58-70`.
- Deterministic Draft-7 bundle, committed, drift test names the regeneration command, camelCase and ref-resolution tests — `src/app_server/schema.rs:1-30, 392-580`.
- Black-box harness with a loopback scripted provider — `tests/app_server_black_box.rs:1-70, 865-1040`.
- Protocol error codes — `src/error.rs:41-77`.

### M6
- Kitty keyboard-protocol push/pop, pop safe from the panic path — `src/tui/term.rs:32-78`; out-of-band writes for bell/OSC/title `:135-160`; terminal handed back on every exit incl. panic (D77) `notes/research.md:1149`.
- Frame is measured not predicted; settled rows written once; resize quiet 120ms — `src/tui/app.rs:1-31, 65`.
- Single keybinding table drives `?`, footer, docs — `src/tui/keys.rs:9-17`; single command registry — `src/app/action.rs:348, 885`, `src/tui/slash.rs:36`. (Closes `cc-gap-analysis.md:68`.)
- Esc is one ordered stack (D80) `notes/research.md:1309`; approval dialog Yes/session/No+feedback, Ctrl-E diff — `src/tui/ask.rs`.
- Active-verb table (P1#9) — `src/tui/activities.rs`; live tail rendering.
- Test infra: Recorder over `TestBackend` with raw-byte sink and style assertions — `src/tui/test_util.rs:203-320`; timing tiers (`now: Instant` injection vs fake timers) — `notes/design/tui-test-infra.md:6-12, 60-72`.

### M7
- Hooks: 60s timeout, SessionEnd 1.5s, stdin write shares the timeout and runs concurrently (64KB deadlock) — `src/hooks.rs:10-13, 110-170, 651-670`; exit 2 blocks with stderr, other codes ignored; `updatedInput` accumulates; hook `ask` — `:178-258`. **P0 hook-ask `unreachable!`** — `src/query.rs:500-504, 1303, 1819`. New events and `BINGO_ENV_FILE` — `cc-gap-analysis.md:64-65, 131`.
- Skills: frontmatter/args, layer order, on-disk overrides bundled, mtime cache — `src/skills.rs:4-14, 117-266`; bundled `guide` sync rule `AGENTS.md:24-31`.
- MCP: 5s connect timeout, dial at boot (D165), concurrent handshakes without holding the manager lock (D167), child stderr to a log file, tools fold under the server key (D166), double-dial guard — `src/mcp.rs:41-61, 185-211, 299-350`.

### M9
- Codex client id/issuer, refresh lead 300s, device wait 15min, loopback port 1455, permanent refresh-failure classes, no-expiry → refresh on first use — `src/api/auth.rs:11-90, 152-162, 197-320`; `auth.json` 0600 atomic, non-reentrant store — `src/auth.rs:1-25, 96-141, 221-232`; design `notes/design/provider-oauth.md`.

### M10
- Definition frontmatter and precedence — `src/agents.rs:20-50`; notification injection as a recorded user message (`<task-notifications>`) — `src/query.rs:969-985`; wake rule D129 `notes/research.md:5536`. Do not port: hires (D53), rooms (D95), @-debt (D131), ack chasing (D44), avatars (D110).

### M11
- Kitty probe bytes, 400ms timeout, tmux passthrough and focused-pane rule, WezTerm/Konsole exclusion, env fast path, size bounds — `src/tui/gfx.rs:23-57, 101-260`; DECSTBM scrollback push (not CSI S), two-row minimum — `src/tui/term.rs:100-121, 465-470`; write-once/lazy-freeze block model — `src/tui/statics.rs:1-27`; notification OSC 9/99/777, tmux wrapping rules — `src/tui/notify.rs:1-23, 85-93`; highlighter choice rationale `Cargo.toml:33-36`; `zune-jpeg` breaks on rustc ≥1.96 without the patch `Cargo.toml:45-53` (prefer dropping the `image` jpeg feature or a different decoder); virtualization only after profiling `cc-gap-analysis.md:78, 119, 159`.

### Explicitly not ported (from `cc-gap-analysis.md:138-150` and the survey)
StreamingToolExecutor, precompute compaction, remote flags, agent/http/prompt hook types, distributed team memory, React/Yoga port, cache-edit microcompact, external vim/pager takeover, default fallback model, `share`, four generations of conversation views, `Session` god object (27 fields, 10 handles), the 575-line `query_loop` (`src/query.rs:915-1490`) touching inbox/hires/mail/reminders.

## 4. Test and verification strategy

**Per-crate unit tests, layered by what they may compile.**
- `bingo-sdk`: fixture round-trips for every serde type under `crates/bingo-sdk/fixtures/*.json`; `deny_unknown_fields` where the type is a wire type.
- `bingo-core`: loop machine tests are pure (`step` in, effects out, no runtime); actor tests use `bingo-provider-fake` in-process. CI asserts `cargo tree -p bingo-core -e normal` contains none of `reqwest ratatui crossterm rmcp image syntect synoptic`.
- Plugin crates: unit tests only, no integration test binaries (link cost); one exception each for `bingo-surface-rpc` (protocol fixtures) and `bingo-surface-tui` (TestBackend).
- `bingo` binary: exactly two integration test binaries, `tests/cli.rs` and `tests/rpc.rs`.

**`bingo-provider-fake`.** `Step = Text | Events(Vec<StreamEvent>) | ToolCall{name,input} | Error{kind, retry_after} | Overflow{body} | Hang(Duration) | CountTokens(u64)`; records every `NeutralRequest` for assertions; validates request shape like the API would (rejects orphan tool_results, empty assistant messages). Feature `loopback`: a hand-rolled HTTP/1.1 SSE responder on `127.0.0.1:0` speaking the Anthropic wire (model: `tests/app_server_black_box.rs:865-1040`) so black-box tests exercise the real adapter. Scriptable from JSON via `BINGO_FAKE_SCRIPT` for CLI/RPC/tmux tests. Always compiled (it is the demo provider), no heavy deps.

**Contract tests.** Schema bundle in `schema/` generated from schemars; drift test compares in-memory generation to the committed bundle and prints the regeneration command; camelCase and ref-resolution checks (`src/app_server/schema.rs:392-580` is the template). Error codes: exhaustive `ErrorCode` impls with no wildcard arm plus a drift test per enum.

**Black-box CLI/RPC.** Temp HOME + `XDG_CONFIG_HOME` + `XDG_DATA_HOME`, provider keys removed, `BINGO_PROVIDER=fake`; assert exit status, stdout purity, stderr `[error]` lines, NDJSON validity, resume across two invocations.

**TUI.** Recorder over ratatui `TestBackend` (screen rows, scrollback rows, raw byte sink for OSC/kitty bytes, draw/clear counters, `assert_row_styled`); drive with synthetic `KeyEvent`s and injected `Event`s; time via `now: Instant` parameters, never sleeps. `scripts/tui-smoke.sh`: `tmux -L bingo -x 120 -y 40` with the fake provider, `send-keys`, poll `capture-pane -p` up to 5s, assert reply text, permission dialog, Esc interrupt, Ctrl-C exit, pane title restored, exit code via a wrapper writing `$?` to a file. Runs on macOS and Linux in CI from M6.

**CI matrix (`.github/workflows/ci.yml`).** Jobs: `fmt` (rustfmt check + `scripts/check_discipline.sh`); `test` on `ubuntu-latest, macos-latest` (`cargo check --workspace --all-targets --locked`, `clippy … -D warnings`, `cargo test --workspace --locked`); `windows-latest` as `continue-on-error: true` from M1, promoted to required at M6; `budget` (below); `tui-smoke` from M6. `Swatinem/rust-cache`, `--locked` everywhere.

**Build-time budget (`scripts/budget.sh`, run in CI, thresholds in `scripts/budget.toml`, baseline recorded at M0/M1).** Measure and print in the job summary; fail on hard limits:
- dependency count `cargo tree --workspace -e normal --prefix none | sort -u | wc -l` (hard ≤ 260; any increase needs an ADR line);
- per-crate forbidden-dependency lists (kernel purity, plugin-to-plugin absence via `cargo metadata` — a plugin's `dependencies` may name only `bingo-sdk` and external crates);
- `cargo build --timings` artifact; wall time of cold `cargo test --workspace` (hard ≤ 2× baseline) and warm `cargo check -p bingo-core` (hard ≤ 20s);
- the relink test: `touch crates/bingo-surface-tui/src/lib.rs && time cargo check -p bingo-core` must be a no-op;
- `du -sh target/debug` after tests (report; soft ≤ 5 GB);
- number of test binaries (report; ≤ crates + 4).
Workspace profile: `[profile.dev] debug = "line-tables-only"`, `split-debuginfo = "unpacked"`, `[profile.dev.package."*"] debug = false`.

**File-size discipline that measures cohesion (`scripts/check_discipline.sh`).** Lints live in `[workspace.lints]` (`unsafe_code = "forbid"`, `clippy::unwrap_used`/`expect_used = "deny"`, allowed in `#[cfg(test)]` via `clippy.toml` `allow-unwrap-in-tests = true`). The script measures, per crate: (1) for each type, the number of files containing an inherent `impl T` block (≤ 2) and the total inherent-impl lines across files (≤ 1200) — this is what `Chat` across 9 files would have tripped; (2) struct field count (≤ 16); (3) non-test lines per file (warn 700, fail 1000; `#[cfg(test)] mod tests` excluded); (4) module fan-out in `bingo-core` (`use crate::` distinct siblings ≤ 12 — the `query_loop` counter-example). Implement with `ast-grep` patterns when available, grep/awk fallback; thresholds in `scripts/discipline.toml`; exemptions only via `// discipline: allow(<rule>) ADR-NNNN`.

**Milestone gate.** A milestone is done when its plan file's exit-criteria checkboxes are ticked with the command output pasted (or the CI run linked), including the manual live-provider smoke where one is required. Failures are pasted as-is.

## 5. Repo conventions

**`AGENTS.md` outline (≤ 90 lines).**
1. What bingo is; where things live (`ARCHITECTURE.md` crate map, `docs/adr/`, `docs/plans/`).
2. Language and style: edition 2024; `thiserror`; no `unwrap`/`expect` (lint-enforced; tests exempt); no `unsafe` (forbidden); comments say why; English for model-facing text, UI copy, docs, tests; no dependency without an ADR line and a budget run.
3. Architecture rules: layering `sdk ← core ← plugins ← bin`; **the kernel never imports a plugin**; **no plugin depends on another plugin except through an sdk capability trait registered in core**; **one event stream** — `bingo_sdk::Event` is the only event type, surfaces are clients and derive views at render time, no private mirror enums; **one fact, one representation** (the old wording, `AGENTS.md:21`); **contracts first** for anything consumed independently (trait, wire, persisted record) — fixture/schema test before implementation; **bricks first** — pure function → primitive → component → feature; a feature PR without a pure brick underneath is suspect; **subtract by default**; tool properties fail closed.
4. Verification gates: the four cargo commands with `--workspace --locked`, `scripts/check_discipline.sh`, `scripts/budget.sh`; user-visible CLI/RPC behavior needs black-box coverage; terminal-byte changes need a TestBackend test and the tmux smoke; unverified is not done; failures reported as-is.
5. Decision records (below). 6. Plans (below). 7. Commits (below). 8. Forbidden: unsafe; unwrap/expect; a surface holding session state; a plugin importing another plugin; a second representation of an existing fact.

**ADRs (`docs/adr/NNNN-slug.md`).** One per boundary decision only: a trait shape, a wire format, a persisted format, a dependency, a crate split, a threshold family. Template: Context (≤ 10 lines) / Decision (≤ 15) / Consequences (≤ 10) / Supersedes. Hard cap 120 lines; anything longer is a design note in `docs/design/` linked from a short ADR. Bug fixes are commit bodies, not ADRs. `docs/adr/README.md` is the index, one line each. The 856 KB `research.md` is the anti-pattern: 189 entries where ~25 were boundary decisions.

**Plan files (`docs/plans/M<n>-slug.md`, ≤ 150 lines, written before code).** Sections: Goal (one line) / Bricks in build order / Crates and files to touch / Exit criteria as checkboxes with exact commands / Non-goals / Risks touched. At the end of the milestone append "Verified" with outputs. The plan is the milestone's ledger; ADRs it produced are listed.

**Commits.** Conventional Commits, imperative, literal subject ≤ 60 chars, English. Scopes are crate short names: `sdk core provider-fake provider-anthropic provider-openai tool-fs tool-bash tool-web print rpc tui hooks skills mcp memory subagents acp bin ci docs adr`. Examples: `feat(core): add typed interrupt with per-tool behavior`, `fix(tool-bash): reject bare sudo before spawn`, `test(rpc): cover initialize refusal on version mismatch`, `build: cap dependency count at 260`. Body only when it carries information; footers `Refs: ADR-0007`, `Plan: M3`. No literary titles — `git log --grep` and `git log -- crates/bingo-tui` must find things.

## 6. Risk register

| # | Risk | Mitigation | Early signal |
|---|---|---|---|
| 1 | sdk trait churn before 3 implementations exist | Fake + Anthropic + OpenAI by M2; print + rpc + tui by M6; Read + Bash + MCP-adapted tools by M7. After M2 a sdk change needs an ADR listing every plugin touched. | A sdk PR touching > 2 plugin crates more than once per milestone. |
| 2 | TUI async client model (ordering, backpressure, latency) | TUI consumes the same bounded `Event` channel as RPC with snapshot resync; render is pure; the loop never awaits the core; TestBackend tests inject events. | TUI code holding session state, calling a provider, or sharing a `Mutex` with core. |
| 3 | Compile time / target bloat returns | Budget job with hard limits; per-crate forbidden deps; profile settings; ≤ 2 integration test binaries. | `cargo check -p bingo-core` warm > 20s; target > 5 GB; a crate pulling `reqwest` that is not a provider/web/mcp crate. |
| 4 | Provider quirk regressions (Codex include/store, reasoning delta names, retry-after units, context-limit recompute, cache-usage summing) | Port the old wire tests as request-body and SSE-fixture tests in M1/M2/M9; live-verified matrix recorded in an ADR. | A provider crate change without a fixture change. |
| 5 | Scope creep back into the social layer | Teams/rooms/experience are outside M0–M13 and need an ADR with a user story and test plan; M10 capped at Agent tool + notifications. | `bingo-subagents` > 2K LOC; the nouns "room", "team", "hire", "ack" appearing in sdk or core. |
| 6 | Permission gate fail-open regression | Table-driven case file ported from `permission.rs` tests; property tests for the splitter; invariants (deny beats all, ask survives bypass, sensitive dirs prompt in every mode) as separate tests. | Any change to gate order without a case-file diff. |
| 7 | Resume/compaction produce API-invalid history | Projection validity property test; fake provider rejects invalid requests; every dogfood 400 body becomes a fixture. | A 400 during dogfooding; a compaction change without a ladder test. |
| 8 | The three architects' plans diverge and M0 becomes a big-bang merge | Before code: ADR-0001 crate map and ADR-0002 event stream reconciled across all three plans; M0 is the integration proof. | Two plans naming the same concept differently past ADR-0001. |

Also watched: Windows drift (cfg(unix) creep in plugins; Windows CI job promoted at M6), `image`/`zune-jpeg` toolchain breakage.

## 7. First session after approval (ends with `cargo test` green on the walking skeleton)

1. Read the kernel and gateway plans; write `docs/adr/0001-crate-map.md` (names, layering, dependency rules) and `docs/adr/0002-event-stream.md` (single `Event`, durable vs ephemeral variants, ids). Commit `docs: add crate map and event stream ADRs`.
2. Scaffold: root `Cargo.toml` with `[workspace] members = ["crates/*"] resolver = "3"`, `[workspace.package] edition = "2024" rust-version = "1.96" license = "MIT"`, pinned `[workspace.dependencies]` (tokio, serde, serde_json, schemars, thiserror, async-trait, futures-util, clap; nothing else yet), `[workspace.lints]` (`unsafe_code = "forbid"`, `clippy::unwrap_used = "deny"`, `clippy::expect_used = "deny"`), the dev profile above; `rust-toolchain.toml` (stable, rustfmt, clippy); `rustfmt.toml`; `clippy.toml` (`allow-unwrap-in-tests = true`); `.gitignore`. Create `crates/{bingo-sdk,bingo-core,bingo-provider-fake,bingo-tool-fs,bingo-surface-print,bingo}` with `lints.workspace = true`. Commit `chore: init workspace`.
3. `AGENTS.md`, `ARCHITECTURE.md` (crate map, 40 lines), `docs/plans/M0-walking-skeleton.md` with exit criteria. Commit `docs: add AGENTS.md, architecture map and M0 plan`.
4. `scripts/check_discipline.sh` v1 (lint pass, kernel-purity `cargo tree` check, plugin-to-plugin check via `cargo metadata`, cohesion counters), `scripts/budget.sh` v1, `scripts/budget.toml`, `.github/workflows/ci.yml`. Commit `ci: add fmt/check/clippy/test matrix, discipline and budget gates`.
5. Bricks, each with tests, each its own commit: `feat(sdk): add message, stream and event contracts` (with fixtures) → `feat(sdk): add tool, provider and permission traits` → `feat(provider-fake): add scripted provider` → `feat(core): add assistant accumulator` → `feat(core): add tool executor with typed interrupt` → `feat(core): add loop state machine` → `feat(core): add session actor and event log` → `feat(tool-fs): add Read tool` → `feat(print): add --print and --json surface` → `feat(bin): compose walking skeleton`.
6. Verify and paste into the M0 plan: `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `scripts/check_discipline.sh`, `scripts/budget.sh` (records the baseline), `bingo --print --provider fake "hello"`, `bingo --print --json --provider fake "read Cargo.toml"`. Commit `docs(plans): mark M0 verified`.

### Critical Files for Implementation
- `/Users/yexrob/Episodes/Projects/bingo-inc/bingo/src/permission.rs` — the gate order, shell splitter and its tests are the spec for M1's most safety-critical brick.
- `/Users/yexrob/Episodes/Projects/bingo-inc/bingo/src/compact.rs` — the overflow ladder, token gate and estimate rules for M4 (with `src/budget.rs`).
- `/Users/yexrob/Episodes/Projects/bingo-inc/bingo/src/api/contract.rs` — stream vocabulary, accumulator finish rules and error classification for M0/M1.
- `/Users/yexrob/Episodes/Projects/bingo-inc/bingo/src/tool/bash.rs` — interactive rejection table, periodic detection, foreground/background process handling for M1.
- `/Users/yexrob/Episodes/Projects/bingo-inc/bingo/notes/cc-gap-analysis.md` — the open P0/P1 list every milestone above maps to.
