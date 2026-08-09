# Slash Command UX — dev implementation plan (aligned with slash-command-ux.md)

> Status: alignment draft (ui-ux's `notes/design/slash-command-ux.md` v0.1 is the **design contract**; this file is the **dev implementation plan**: code touchpoints, gaps filled in, tests, and commit split).
> 2026-08-07 · Team A (feat/slash-ux) · cross-referenced against `notes/research.md` D4/D16/D20/D26/D30/D22.

## 0. Role split

- **Interaction design contract**: `notes/design/slash-command-ux.md` (maintained by ui-ux) — `/think` picker (●/❯ dual markers, 1-6 direct jump, footer ▸ preview, busy whitelist, no-match hint row), `/model` deferred items, state machine §3.1, key map §3.2, layout §3.3, acceptance anchors §7.
- **This file (dev)**: fills in the engineering surface the contract doesn't cover — structured arg hints (G1; the contract's §4 dropdown section was missing this, dev proposed it and it was accepted), code touchpoints, safe busy-time dispatch in submit, guide.md sync, commit split.

## 1. Current-state essentials (research findings, consistent with contract §1.3)

- `SLASH_COMMANDS: &[(&str, &str)]` flat table (chat.rs:198), 18 built-ins; desc embeds the arg hint (not structured). Consumers: `slash_help`, `update_slash_suggestions` (merges skills), `submit` completion check.
- Execution chain: `submit()` → busy branch (**everything queues, slash commands included**) → `run_slash()` dispatches on (cmd,arg) → `push_slash_output` transient rows.
- `submit_queued()` → `start_turn(text, true)`: **queued slash commands go to the model as plain text** (not re-routed through `run_slash`) — this is the pit to fix alongside aligning with CC's "busy commands queue, run after the turn" semantics (see §3.2).
- thinking: settings `thinkingLevel` three-layer merge + `/think` persists via `upsert_project_settings`; `THINKING_LEVELS = [low..max]` (api/types.rs:139), UI table `THINK_LEVELS` (chat.rs:267) = off + five levels, same order, test-guarded.
- Menus = suggestion rows above the input (app.rs `suggestion_rows`); fullscreen puts them after chrome, inline after the prompt; on_key priority: error > ask > model > think > search > entity > ctrl+c/esc > slash > editing (correct today, unchanged).
- `footer_row` (app.rs:144) reads `runtime.thinking` → `model_footer_label`; the theme already has `claude` / `permission` / `inactive` tokens (contract §3.4 styling is feasible).

## 2. Claude Code / Codex research findings (consistent with contract §1.1/1.2)

- CC: `/` full list + filter; menu row = command name + arg hint (`<arg>` / `[arg]`) + description; Tab accepts; `/model` with no arg → picker (↑/↓ cycles, `s` session-only, Enter saves, confirmation when there's prior output); `/effort` levels low..max in the same order as bingo; commands queue while busy but /status /tasks run immediately.
- Codex: `/` popup keeps filtering; busy-time slash + Tab queues for the next turn.
- Verified: bingo's picker skeleton (↑/↓ wrap + preselect current level + Enter/Esc) is isomorphic to CC's — **the gap is in feedback and hierarchy, not the skeleton** (contract §2).

## 3. dev additions (engineering surface not covered / needing dev's call)

### 3.1 G1: structured SLASH_COMMANDS arg hints (patch for contract §4)

- Constant shape: `pub const SLASH_COMMANDS: &[(&str, &str, &str)]` (name, hint, desc) — a 3-tuple, zero new types.
  - hint is extracted from the usage fragment embedded in desc and normalized: `help`→`""`, `model`→`[name]`, `think`→`[off|low|medium|high|xhigh|max]`, `resume`→`[name or keyword]`, `permissions`→`[allow|deny|ask] [rule]`, `mcp`→`[enable|disable|reconnect]`, `team`→`start|status|assign|stop|list`, `provider`→`[name]`, `share`→`[--open]`, `rename`→`[name]`, `theme`→`[dark|light|auto]`, the rest `""`.
  - desc drops the `(/xxx [arg])` prefix, keeping a clean description.
- Consumers: `SlashSuggestion` gains `hint: String` (skills entries hint = empty); `update_slash_suggestions` keeps the existing name/desc matching logic; `suggestion_rows` slash arm renders a `/{name} {hint}` column + desc column (name_col computed including the hint); `slash_help` same format.
- Impact: purely display + help layout; behavior unchanged; tests involved: `slash_menu_lists_commands_and_hides_with_args` (chat.rs:6474) etc. need their assertions updated for the new row format.

### 3.2 Busy whitelist (contract §4.2/§5 anchor 5) — dev verification results

- **submit() dispatch safety**: all whitelist handlers are synchronous + fire-and-forget:
  - `slash_think` (sync: channel send + upsert), `slash_theme`, `slash_provider`, `slash_status`, `slash_context`, `slash_tasks` (sync; refresh_tasks is a snapshot refresh), `slash_help`, `slash_skills` (sync read-only; load_skills reads disk) — safe;
  - `slash_model`'s no-arg path `open_model_models` is an async fetch (background fetch, doesn't block the event loop) — safe; the with-arg path is sync.
  - Implementation: in the busy branch, before `queued.push`, check whether `text.strip_prefix('/')` hits the whitelist → if so go through `run_slash` (reuses the existing dispatch, no new path); **do not touch the busy state afterwards** (whitelist commands don't reset the turn). Test: after running a whitelist command while busy, `busy` is still true.
  - Whitelist = the seven from contract §4.2 + `help` + `skills` (added by devex; purely read-only, zero side effects); `resume` with no arg lists from disk, stays queued.
- **Mandatory follow-on fix**: `submit_queued` currently sends queued text straight to the model via `start_turn` — a non-whitelist slash command queued while busy (e.g. typing `/clear`) would be sent to the model as a prompt (wrong semantics). Change to: when dequeueing, if the text starts with `/` → go through `run_slash`; otherwise `start_turn`. Aligns with CC's "busy commands queue, execute as commands after the turn".

### 3.3 footer ▸ preview (contract §3.5/anchor 3) — dev verification

- `footer_row` single branch: `if let Some(menu) = &chat.think_menu` → the think segment renders `THINK_LEVELS[menu.selected].0` + `▸` (theme.claude); otherwise current behavior (runtime.thinking, inactive). Testable.
- Note: `model_footer_label` currently takes `(model, thinking: Option<&str>)` — the preview branch bypasses it and constructs the segment text directly, so preview values never mix into the generic path.

### 3.4 `s` key (session-only): **deferred in v1**, consistent with contract §5's defer

- The contract lists `s` (/model session-only) as deferred; /think's `s` is deferred for the same reason: /think persistence is its existing design (three-layer settings), session-only is a low-frequency need; v1 adds no key and no persist parameter.
- Recorded as a v1.1 candidate (add it if users report "I want to adjust the level without writing project config").

### 3.5 Key-hint row (pending ui-ux confirmation; merged by default)

- Append one dim row to the think menu tail: `↑↓/1-6 select · Enter confirm · Esc cancel` (7 rows total; the frame budget and narrow-terminal drop rules are unaffected — chrome rows aren't predicted up front, D26/D27 invariants).
- Merge into contract §3.3 if ui-ux has no objection; cut if they do (not a hard requirement).

### 3.6 Default-level wording fix (devex G2, P0)

Three "default" claims currently contradict each other — an AGENTS.md doc-drift violation:

| Location | Current wording | Fact |
|---|---|---|
| `settings.thinkingLevel` | absence sends no param (= off) | settings default `None`; off isn't serialized |
| `THINK_LEVELS` high row | `(default level)` | **misleading** — the default is off; high is not the default |
| `toggle_thinking` (Alt+T) | restores default `medium` | fallback when there's no last_thinking, not a global default |

**Unified story (into guide.md + THINK_LEVELS)**:
1. settings absence = `off` (no thinking param, DeepSeek-compatible endpoints);
2. `THINK_LEVELS` high row drops the "default level" wording (the highlight/recommendation can stay as "(recommended)", wording up to ui-ux);
3. Alt+T = quick toggle between off ↔ the last non-off level; restores `medium` when never enabled — this goes into the guide.md shortcut section.
- Incidental fact fix: **bingo already has the Alt+T toggle** (chat.rs `toggle_thinking`, persists via `slash_think`) — it's already the counterpart of CC's Alt+T; this round does **not build or change it**, only unifies the wording with G2.

### 3.7 Slash output TTL grading (devex G3, main ruling: **do it this round**)

- Today: `push_slash_output` has a uniform 2s TTL (`slash_at`). Unknown-command/usage errors vanish after 2s and can't be expanded after landing in the scrollback.
- Plan: success-type feedback keeps 2s; **error/usage rows ≥8s or persistent until the next input**. Implementation: `push_slash_output` gains an `error: bool` parameter (explicit marker, not content sniffing), `slash_lines` records the type, TTL render filtering uses a different window per type; complements the "no-match dim hint row" (the hint row is chrome, not an error).
- Boundary: error rows don't open a new channel beyond slash_lines; /clear still clears everything.

### 3.8 Structured error codes (devex G4, main ruling: **do it this round**)

- Today: unknown command/invalid argument are plain text (`unknown command: /xxx` / `usage: /think [...]`); feedback-states §4.1 requires error output to carry `code=`.
- Plan (minimal surface): unknown command → `[error] code=UNKNOWN_COMMAND` + the original text; invalid argument → `[error] code=BAD_ARGUMENT` + usage line. The codes are registered in the `src/error.rs` table (add-only); the TUI transient rows render the code prefix.
- Scope control: only the two most-asserted categories — unknown command and invalid argument; other slash error texts stay unchanged (avoid scope creep). qa acceptance anchors can hang on these two codes.

### 3.9 Dispatch completeness tests (devex G5)

- Every `SLASH_COMMANDS` name has a `run_slash` branch (aliases included), and every `run_slash` branch is in the table (aliases map to primary names) — prevents missing dispatch for new commands or dead table entries.
- `arg_hint`/`/help` output consistency test: help rendering = table hints assembled from a single source.
- Picker state-machine tests: wrap cycle / digit direct-jump / confirm / cancel / current-level preselect — five cases extending the existing pattern.

### 3.10 To be aligned later (not this round, recorded)

- **Q1 persistence layer**: `/model` `/think` write the project-layer `.bingo/settings.json` (git noise); CC writes the user layer. Model/thinking level feel more like personal preference — **switch to the user layer later; not this round**.
- **Q2 no-match hint**: beyond the `/zzz` dim hint (contract §4.1), invalid values for "menu opens only with no arg" commands (`/think foo`) keep current behavior: usage line + no menu + state unchanged (= CC's keep-current semantics, already holds).

## 4. State machine (/think picker, same source as contract §3.1)

```text
Idle ──"/think" no-arg Enter──▶ MenuOpen{ selected = current }   (current = runtime.thinking or "off")
MenuOpen ──↑/↓──▶ MenuOpen{ selected ±1 mod 6 }                (wrap)
MenuOpen ──1..6──▶ MenuOpen{ selected = n-1 }                  (direct jump)
MenuOpen ──Enter──▶ apply(persist) ──▶ Idle                    (runtime write + upsert + "✓ …" transient row)
MenuOpen ──Esc──▶ Idle                                          (no runtime write, footer reverts)
While busy: whitelist commands bypass the busy queue and run immediately; the rest queue, and after TurnEnd are dequeued and dispatched as command/text (§3.2).
```

- A pure `selected` index; effects are written to runtime only on Enter — browsing is side-effect-free and Esc naturally needs no rollback (contract §3.1 principle, dev agrees).

## 5. Implementation touchpoints (file × change)

| File | Change |
|---|---|
| `src/tui/chat.rs` | `SLASH_COMMANDS` 3-tuples; `SlashSuggestion.hint`; `update_slash_suggestions`/`slash_help` format; `ThinkMenu.current` (● data source); `think_menu_key` gains `1..6`; `submit` busy whitelist dispatch (incl. help/skills); `submit_queued` command/text dispatch; `push_slash_output` error/success TTL grading (§3.7); unknown command/invalid argument carry `[error] code=` (§3.8); THINK_LEVELS high row wording (§3.6, one line) |
| `src/tui/app.rs` | `suggestion_rows`: think arm ●/❯/name/desc/hint row; slash arm hint column; `footer_row` ▸ preview branch |
| `src/error.rs` | register `UNKNOWN_COMMAND` / `BAD_ARGUMENT` (§3.8, add-only) |
| `src/skills/bundled/guide.md` | `/think` 6 levels + one picker line + default-level story (§3.6) + Alt+T explanation |
| `notes/design/feedback-states.md` | changelog backfill (v1.21): transient `✓` row semantics unchanged; busy whitelist instant feedback; error-row TTL grading; no-match hint is a hint, not an error |

## 6. Test plan (contract §7 anchors + dev additions + main ruling)

1. browse then Esc → `runtime.thinking` unchanged (extends the existing pattern);
2. `1..6` direct-jump selects the right row; wrap boundary regression; **while the menu is open, 1-6 are consumed by the menu and never enter the input**;
3. footer shows `▸` preview while the menu is open, reverts after Esc/Enter; **during preview ↑/↓ only switches the footer data source, doesn't dirty the document into re-render**;
4. busy + `/think xhigh` takes effect immediately (not queued) and **busy stays true**; busy + `/clear` queues → after TurnEnd executes as a **command** (not sent to the model);
5. `/zzz` shows the no-match hint row, cleared by further typing;
6. `●` marks the runtime level, `❯` marks the selection (two different markers when separated; when overlapping, ❯ takes the prefix slot);
7. slash dropdown rows render with the hint column; `slash_help` new format; **dispatch completeness + arg_hint/help consistency (main's mandatory acceptance item, §3.9)**;
8. unknown command/invalid argument rows carry `[error] code=UNKNOWN_COMMAND|BAD_ARGUMENT` (qa asserts on the code);
9. error rows TTL ≥8s, success rows 2s (clock-advance/injected-now tests);
10. regression: all existing slash tests pass; `cargo build` / `clippy -- -D warnings` / `cargo test --bin bingo` all green.

## 7. Commit split (dependency order, all on feat/slash-ux; post-main-ruling scope = P0 six items + G3/G4)

1. `refactor(tui): structured SLASH_COMMANDS arg hints (G1, pure display)` — includes hint rendering + dispatch-completeness tests + arg_hint/help consistency tests;
2. `feat(tui): /think picker ●/❯ dual markers + 1-6 direct jump + footer ▸ preview` — contract §3 core;
3. `feat(tui): busy slash whitelist instant execution + queued-command dispatch fix` — contract §4.2 + §3.2;
4. `feat(tui): /zzz no-match hint + error-row TTL grading + structured error codes` — contract §4.1 + §3.7/3.8;
5. `docs: unify default-level wording (THINK_LEVELS one line + guide.md) + feedback-states changelog` — §3.6 + AGENTS.md sync rule.

## 8. Explicitly not doing (recorded, anti-over-engineering)

- Model-switch second confirmation (bingo has no CC cache contract); `s` session-only (v1.1 candidate); ←/→ effort adjustment inside /model (/think already has its own first-class picker; keep the separation); subcommand secondary completion (`/mcp e → enable`, devex G1, small but deferrable — this round only does arg_hint display); j/k menu navigation (swallows input; the current comment already argues for it); a generic Menu component abstraction (the three menus differ in shape).
- **Alt+T not built, not changed**: bingo already has it (`toggle_thinking`, off ↔ last level, the counterpart of CC's Alt+T); only the wording is unified with §3.6.
