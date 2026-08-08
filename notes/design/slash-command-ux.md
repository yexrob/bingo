# Slash Command UX — Interaction Design Spec (v0.4, merged)

> Status: **final for implementation** — ui/ux v0.1 + parallel dev draft
> (`slash-ux.md`) + devex reviews + main ruling (scope: G12/G13 in, subcommand
> completion / `s` session-only / Q1 persistence layer deferred) · Scope: the `/`
> dropdown, the `/think` level picker, the `/model` picker · Alignment baseline:
> Claude Code (interactive pickers) and Codex CLI (slash popup) · Normative feedback
> conventions: `notes/design/feedback-states.md` v1.18. This document is the design
> contract; dev lands it, qa accepts against the anchors in §7. The parallel draft
> (`slash-ux.md`) is superseded; its unique content is folded in below.

## 1. Baseline facts

### 1.1 Claude Code (official docs, code.claude.com/docs)

- `/` at input start opens the command menu; typing letters filters it. Menu rows =
  command name + argument hint (`/add-dir <path>`, `/model [alias|name]`) + purpose.
  Built-ins, skills, plugin and MCP prompts share the menu. Fullscreen adds mouse
  hover/click. Tab accepts completions. A command is recognized only at message start;
  trailing text is its argument.
- **Picker pattern — "no argument → opens a picker"** is the canonical shape:
  `/model`, `/effort`, `/advisor`, `/autocompact`, `/clear`. A picker = list +
  keyboard select + confirm:
  - `/model` picker: ↑/↓ (or Tab) cycle; ←/→ adjust effort on supporting models;
    `Enter` switches **and saves as default**; `s` switches **session-only** (no save);
    picker has a `Default` row; when the conversation already has output it asks for
    confirmation (the next response re-reads full history without cached context —
    bingo is a local harness with no such cache contract, **not adopted**).
  - `/effort` (reasoning effort) = `low | medium | high | xhigh | max` — **identical
    to bingo's `THINKING_LEVELS`**. Org-level effort caps hide out-of-range rows.
  - Pickers can carry auxiliary keys (`/copy` picker: `w` = write to file).
- `Alt+T` toggles extended thinking on/off (binary — not applicable to bingo's 6-level
  continuum, **not adopted**). `/config thinking=true|false` sets directly.
- Commands typed while Claude is responding **queue and run after the current turn**;
  a few (`/status`, `/tasks`, `/usage`) run immediately. `Esc` closes dialogs;
  Left/Right cycle dialog tabs.
- `/skills` picker: type to filter, `t` sort, `Space` cycle visibility, `Enter` save.

### 1.2 Codex CLI (OpenAI developers docs)

- `/` in the composer opens the slash-command popup; keep typing to filter.
- While a task runs, a slash command + `Tab` **queues it for the next turn**.
- `/model` picks the active model **and reasoning effort where available**; `/fast`
  toggles fast mode; `/statusline` configures footer items.

### 1.3 bingo today (code facts, feat/slash-ux)

- **Single source**: `SLASH_COMMANDS: &[(&str, &str)]` in `src/tui/chat.rs` (18 built-ins);
  consumed by `slash_help()`, `update_slash_suggestions()` (merges skills), and the
  Enter-completion check in `submit()`. Descriptions embed the arg hint as text
  (`（/model [名称]）`) — **not structured** (G1).
- **Dispatch**: `submit()` → `/`-prefixed → `run_slash(line)` splits `(cmd, arg)` →
  match → `slash_*`. Unknown → skills (`✦ name [args]` marker) → `未知命令` output.
  Output via `push_slash_output` (transient TTL rows above the input, never flushed).
  Dropdown hidden when the query has args (`/think xhigh` = fast path).
- **Thinking chain**: settings key `thinkingLevel` (user/project/local merge; `/think`
  persists via `upsert_project_settings` to `.bingo/settings.json`); runtime
  `Session.runtime.thinking` + `thinking_tx` watch channel. Level table single source:
  `src/api/types.rs::THINKING_LEVELS = [low..max]`; UI table `THINK_LEVELS` (chat.rs) =
  `off` + those five, **test-guarded same order**. `off` sends no param (DeepSeek
  compatible); the rest send `{"type":"adaptive"}` + `output_config.effort`.
  Footer badge: `{model} · think {level}` (off hides the level, P1-D).
- **Existing menus** (no overlay component: all menus render as suggestion rows above
  the input via `app.rs::suggestion_rows`; fullscreen moves them above the prompt,
  inline below — unchanged placement):
  | Menu | State | Keys |
  |---|---|---|
  | slash dropdown | `slash_suggestions` + `slash_selected` | ↑/↓ wrap · Tab=`/name ` · Esc=close · Enter=complete+run · max 5 rows, prefix-then-substring · **no j/k** |
  | /model | `model_menu: Option<ModelMenu>` | ↑/↓ · Enter=level2/confirm · Esc=back/out · two levels (provider → models, async `/v1/models`, loading/empty hint rows) |
  | /think | `think_menu: Option<ThinkMenu>` | ↑/↓ wrap · Enter=confirm+persist · Esc=cancel (state unchanged, tested) |
  | entity selector | entities + focus index | ctrl+g · ↑/↓ · Enter=open · Esc=collapse (D30, 6-row window) |
  `on_key` priority (existing, unchanged): error screen → ask → model → think → search →
  entity → Ctrl+C/Esc → slash dropdown → edit keys. Menus are mutually exclusive.
- **Busy behavior (verified)**: `submit()` pushes *any* text (slash commands included)
  onto `queued` while busy; `submit_queued()` runs them via `start_turn(text, true)` —
  **they are sent to the model as plain user messages, never executed as commands**
  (G7). `/think xhigh` typed mid-turn therefore never runs; the model receives the
  literal string.
- **Docs drift**: `guide.md` quick reference lists `/think [off|low|medium|high]`
  (4 levels; real table has 6 — G5). `feedback-states.md` (v1.18) requires changelog
  backfill for user-visible feedback changes.

## 2. Gap table (merged)

| # | Gap | Claude Code | bingo plan | Land at |
|---|---|---|---|---|
| G1 | arg hint not structured | row = `/model [alias\|name]` + purpose | `SLASH_COMMANDS` → `(name, hint, desc)`; hint renders in grey after the name (`/mcp [enable\|disable\|reconnect]`), desc in the second column; skills entries hint = `""` | chat.rs const + `update_slash_suggestions` + `suggestion_rows` + `slash_help` + tests |
| G2 | current level not visually marked | picker marks current | **`●` marker in front of the in-effect row** (ui/ux) — the `❯` row is the browse selection; two separate marks (§3.4) | `suggestion_rows` think arm |
| G3 | no key hints | rows carry shortcuts | one dim hint row at the list tail: `↑↓ 选择 · Enter 确认并保存 · s 仅本次会话 · 1-6 直达 · Esc 取消` | `suggestion_rows` think arm |
| G4 | `/think` always persists | Enter=save, `s`=session-only | `set_think_level(level, persist: bool)`; Enter=persist=true (status quo), `s`=persist=false (no settings write). **P2: this round optional** (devex suggests defer; CC precedent + low cost, dev's call) | `think_menu_key` + `slash_think` |
| G5 | guide.md level drift | — | `/think [off\|low\|medium\|high\|xhigh\|max]` + one line on the picker | guide.md |
| G6 | state machine only in comments | — | this spec §3.1 is the single explanation; `on_key` priority untouched | doc |
| G7 | busy slash commands are swallowed as user messages (verified bug) | commands queue, some run immediately | `submit()` busy branch: instant whitelist (`think model provider theme status context tasks help skills`) runs immediately **without resetting busy**; other `/`-commands queue with an `is_slash` marker, `submit_queued` dispatches via `run_slash` (§4.2) | `submit()` + `submit_queued` |
| G8 | no live preview while browsing | picker rewrites the prompt (WYSIWYG) | footer badge previews the browsed level: `think xhigh ▸` (`▸` = preview state), committed badge has no suffix (§3.5) | `footer_row` |
| G9 | no feedback on empty match | — | one dim hint row for `/zzz`-style no-match (§4.1) | `update_slash_suggestions` / render |
| G10 | no direct jump | — | `1..6` jumps to off=1 … max=6 (fixed 6-item table, one keystroke) | `think_menu_key` |
| G11 | doc "default" contradiction (P0) | — | guide.md states `thinkingLevel` default = off/no param; `high` row drops the `默认档位` wording (or becomes `推荐档位`); Alt+T default level written into guide — one consistent story (§4.3) | guide.md |
| G12 | slash output TTL not graded | errors stay readable | **in scope (main ruling)** — success rows keep 2s TTL; error/usage rows ≥8s (preferred: stay until the next input; 8s is the floor) (§4.4) | `push_slash_output` TTL param |
| G13 | slash errors have no stable code | — | **in scope (main ruling)** — `未知命令` → `[error] code=UNKNOWN_COMMAND msg=…`; bad arg → `code=BAD_ARGUMENT` (feedback-states §4.1 format; qa asserts on code) (§4.5) | `push_slash_output` / error path |
| G14 | dispatch completeness untested (P1) | — | test: every `SLASH_COMMANDS` name has a handler and every handler name is in the table (§7) | chat.rs tests |

**Explicitly NOT doing** (over-engineering guard, both drafts agree):
- CC's model-switch confirmation (no prompt-cache contract; switching is the intent);
- `Alt+T` binary thinking toggle (6-level continuum; the picker is the shortest path —
  opens preselected on the current level, ↑/↓/1-6 one step, Enter confirms);
- `j/k` menu navigation (swallows input chars, existing argument stands);
- a generic Menu component abstraction (three menus differ: two-level async / single
  static / completion list; shared component is not worth it);
- `←/→` effort adjust inside `/model` (thinking has its own first-class picker; keep
  the separation);
- `s` session-only for `/model` (deferred; `/model` persists by design).

## 3. `/think` level picker — interaction design (core)

### 3.1 State machine

```text
             input `/think` (no args) + Enter
Idle ────────────────────────────────────────────────▶ PickerOpen {selected = current}
                                                        current = runtime.thinking or "off"
PickerOpen ── ↑/↓ ──▶ PickerOpen {selected = (selected ± 1) mod 6}     (wrap)
PickerOpen ── 1..6 ─▶ PickerOpen {selected = n - 1}                    (direct jump, G10)
PickerOpen ── Enter ─▶ apply(persist=true) ──▶ Idle                    (set + save + "✓ 思考级别已设置: X")
PickerOpen ── s ────▶ apply(persist=false) ──▶ Idle                    (session-only, no settings write, G4)
PickerOpen ── Esc ──▶ Idle                                             (cancel; state unchanged)
Idle ── `/think <level>` + Enter ──▶ apply(persist=true) ──▶ Idle      (fast path, status quo)
Idle ── `/think <invalid>` ──▶ usage line ──▶ Idle                      (state unchanged, tested)
```

- The menu owns a pure `selected` index; the **effect (`runtime.thinking`) is written
  only on Enter/`s`**. Browsing never touches runtime state — Esc needs no rollback.
- `apply(persist)` is synchronous (channel send + optional settings upsert); `Applying`
  exists only as a future-proofing step and may never block Esc.
- **Mutual exclusion** (unchanged): opening the picker clears the dropdown and other
  menus; `on_key` priority stays as-is.

### 3.2 Key map

| Key | Behavior | Notes |
|---|---|---|
| `↑` / `↓` | cycle selection, wraps at both ends | existing |
| `1`..`6` | jump to off=1 … max=6 | G10, new |
| `s` | apply **session-only** (no settings write), close | G4, new (CC precedent) |
| `Enter` | apply + persist + close, `✓ 思考级别已设置: {level}` | existing |
| `Esc` | cancel: close, state unchanged | existing, test-covered |
| `Tab` | n/a — menu is modal; consumed, no-op | avoids confusion with dropdown Tab |

`j`/`k` stay excluded (documented: letters would be swallowed as input chars).

### 3.3 Layout (inline chrome, below the input; fullscreen above it — unchanged placement)

```text
  ● off       不发 thinking 参数（兼容 DeepSeek 等端点）
  ❯ low       adaptive thinking · effort low
    medium    adaptive thinking · effort medium
    high      adaptive thinking · effort high（推荐档位）
    xhigh     adaptive thinking · effort xhigh（编码/agentic 推荐）
    max       adaptive thinking · effort max（最深推理）
  ↑↓ 选择 · Enter 确认并保存 · s 仅本次会话 · 1-6 直达 · Esc 取消     ← G3 hint row (dim)
```

Columns: marker(2) · name (left-aligned, width = max name width) · 2 · description
(dim, truncated per existing rules). Hint row: dim (`theme.inactive`), one row.

### 3.4 Visual emphasis (hierarchy, G2)

| Element | Style | Meaning |
|---|---|---|
| `❯` prefix + name | `theme.permission` (existing selected color) | the row under the finger |
| `●` marker + name | `theme.claude` | the level **in effect** (runtime), fixed while browsing |
| description column | `theme.inactive` | context, deliberately dimmed |
| hint row | `theme.inactive` | affordance, not interactive |

Key rule: **selection (`❯`) and effect (`●`) are two separate marks.** Browsing moves
`❯`; `●` stays put — the user always sees both "where I am" and "what is active".
When `❯` lands on the `●` row both marks share the line (`❯` keeps the prefix slot,
`●` stays in front of the name).

### 3.5 Footer live preview (G8)

While `ThinkMenu` is open, the footer badge shows the **browsed** level in preview
state instead of the committed one:

```text
  ? for shortcuts · ctrl+o to expand        claude-sonnet-4-5 · think xhigh ▸
```

- `▸` suffix marks the preview ("would-be"); committed badge has no suffix.
- Preview segment renders in `theme.claude`; committed in `theme.inactive` (existing).
- On Enter/`s`, the badge lands on the committed level (no suffix). On Esc it reverts.
- Implementation: while `think_menu.is_some()`, the think segment reads
  `menu.selected`; otherwise `runtime.thinking` (single render branch, testable).

### 3.6 Edge and error states

| Case | Behavior |
|---|---|
| No candidates | impossible (fixed 6-item table); defensive: empty table → picker never opens, falls to usage line |
| Wrap at top/bottom | cycles (existing, tested) |
| Enter/`s` while busy | applies immediately (G7 instant path), see §4.2; persist write is fire-and-forget (existing semantics) |
| Esc mid-browse | picker closes, `runtime.thinking` untouched, footer reverts |
| Invalid arg `/think bogus` | usage line, state unchanged (existing, tested) |
| Narrow terminal | rows truncated (existing); 7 picker rows + chrome over budget → Frame drops top rows, input+footer survive (existing invariant) |
| Stale runtime value | picker always preselects from `runtime.thinking` at open time |
| Picker open + resize | chrome rebuilt each frame (existing invariant) |

## 4. `/` dropdown alignment

### 4.1 Structured command table (G1) + no-match hint (G9)

- `SLASH_COMMANDS` becomes `&[SlashCommandDef { name, hint, desc }]` (or a 3-tuple —
  dev's call, zero new types preferred). Descriptions drop the embedded `（/xxx [arg]）`
  prefix; the hint renders in grey after the name (`/mcp [enable|disable|reconnect]`).
  Skills entries get `hint = ""`.
- Hint participates in rendering only, not in matching.
- `slash_help` renders `/{name} {hint}  {desc}`.
- **No-match hint**: when the query yields zero suggestions (`/zzz`), show one dim
  row `（无匹配命令 · 输入 /help 查看可用命令）` instead of an empty gap. Triggered
  only when input starts with `/`, has no args, and the filter is empty. It is a hint
  row (chrome), not an error: no `[error]`, no level, no focus transfer — this is not
  a failed operation. Disappears on the next keystroke (same refresh path).

### 4.3 Doc "default" story unified (G11, P0)

Three places currently contradict each other: `thinkingLevel` doc default = off /
no param; `high` row says `默认档位`; Alt+T (guide line 35) implies a default level.
Unified story for guide.md + code:
- settings absence = `off` (no param sent) — unchanged;
- **`THINK_LEVELS` high description drops `（默认档位）`** (one-line change, zero
  risk; there is no single default level — the label misleads);
- guide.md: `thinkingLevel` default = off / no param; **Alt+T restores the last
  non-off level, defaulting to `medium` when none is recorded**;
- the picker (this spec §3) is the primary thinking entry; Alt+T is a toggle on the
  current level and is documented as such.

### 4.4 Error/usage rows stay readable — TTL grading (G12, in scope by main ruling)

Today every transient slash output row shares one 2s TTL — an error or usage line is
gone before the user can read "what happened + what you can do" (feedback-states §3).
Grading:
- **success rows** (`✓ …`): keep 2s (status quo);
- **error/usage rows** (`未知命令…`, `用法: …`, `未找到 provider…` etc.): **≥8s,
  preferred: stay until the next input** (dev evaluates the cost of a
  "clear on next input" lifecycle; 8s is the floor — the user needs time to act);
- implementation: `push_slash_output` gains a TTL parameter (or a success/error
  marker); the per-row expiry currently shared by `slash_lines` becomes per-row.

### 4.5 Stable error codes on slash errors (G13, in scope by main ruling)

Slash errors are plain text today; qa cannot assert on them. Landing (feedback-states
§4.1 format, same single-line contract as the CLI side):
- unknown command → `[error] code=UNKNOWN_COMMAND msg=未知命令: /xxx。输入 /help 查看可用命令。`
- bad argument (`/think bogus`, `/theme bogus`, …) → `[error] code=BAD_ARGUMENT msg=用法: …`
- rendered as a transient row in the error color (existing error styling); qa asserts
  on `code=` only, copy stays changeable.

### 4.2 Busy dispatch fix (G7, verified bug)

- Today: busy + `/think xhigh` → queued → `start_turn` sends the literal string to the
  model. Never executes.
- Landing: the busy branch in `submit()` checks the `/` prefix first:
  - **instant whitelist** (`think`, `model`, `provider`, `theme`, `status`, `context`,
    `tasks`, `help`, `skills`) → run immediately via `run_slash` (CC semantics:
    settings knobs apply before the next turn; read-only status commands run
    mid-turn). **The whitelist path must not reset `busy`** — it is a side-channel
    dispatch, not a turn transition. Test: busy stays true after an instant command.
  - other `/`-commands (e.g. `resume`, `share`) → queue with an `is_slash` marker;
    `submit_queued` dispatches them through `run_slash` after TurnEnd instead of
    `start_turn`;
  - plain messages → queue as today.

## 5. `/model` picker (deferred)

- Cost confirmation when the conversation has prior output — **deferred**: no
  prompt-cache contract; "✓ 模型已切换" already reports the result.
- `s` session-only for `/model` — **deferred**: `/model` persists by design (and the
  `/think` `s` proves the pattern first).
- `←/→` effort adjust inside the model menu — **deferred**: thinking has its own
  first-class picker; keep the separation.

## 6. Motion

No animation is added. TUI frame timing applies (feedback-states §5 mapping): the
selection marker swaps colors within one frame; the picker opens/closes in one frame;
the `▸` preview appears/disappears with the badge redraw. No fade, no displacement.

## 7. Landing anchors (dev + qa)

1. `ThinkMenu` gains `current: usize` (the in-effect level at open time) — `●` reads
   it; `selected` stays independent; open sets `selected = current`.
2. `suggestion_rows` think arm renders `●`/`❯`/name/desc per §3.3–3.4 + the hint row.
3. `footer_row` preview branch per §3.5.
4. `think_menu_key` accepts `1`..`6` (G10) and `s` (G4; `set_think_level(level, persist)`).
5. `submit()`/`submit_queued` busy dispatch fix per §4.2.
6. `SLASH_COMMANDS` structured (G1); `update_slash_suggestions`/`slash_help`/dropdown
   render update; no-match hint row (G9).
7. Tests:
   - browsing then Esc → `runtime.thinking` unchanged; footer reverts;
   - `1`..`6` selects the right row; **while the picker is open, `1..6` are consumed by
     the menu and never enter the input buffer** (other digits/letters fall through to
     the input, consistent with the menu's existing non-strictly-modal edit semantics;
     devex review wording: "1-6 不进输入框"); `s` applies session-only (settings.json
     **not** written, runtime switched); Enter still persists (regression);
   - footer shows `▸` while the picker is open, clears after Enter/Esc; **↑/↓ during
     preview does not dirty the document — only the footer data source switches**
     (D20i render-storm lesson);
   - busy + `/think xhigh` applies immediately (not queued) **and `busy` stays true**;
     busy + non-instant `/foo` queues and executes via `run_slash` after the turn
     (not sent to the model);
   - `/zzz` shows the hint row; typing clears it;
   - `●` marks the runtime level, `❯` marks `selected` (distinct rows when apart);
   - dropdown renders `/{name} {hint}` + desc; `slash_help` shows the same;
   - **dispatch completeness (G14): every `SLASH_COMMANDS` name has a `run_slash`
     handler and every handler name is in the table**;
   - error/usage rows carry `code=UNKNOWN_COMMAND` / `code=BAD_ARGUMENT` and their TTL
     is ≥8s (success rows stay 2s) (G12/G13);
   - `THINK_LEVELS` ↔ `THINKING_LEVELS` same-order test stays (G5 guard);
   - every existing slash test keeps passing.
8. Docs: guide.md `/think` → 6 levels + one picker line (G5); guide.md default story
   unified (G11, §4.3: absence = off, Alt+T restores last non-off / defaults medium,
   `THINK_LEVELS` high drops `（默认档位）`); feedback-states.md changelog backfill
   (transient `✓` line semantics unchanged; `s` path same transient line; error/usage
   rows now carry stable codes — new error-level-free code lines, qa asserts on code).
9. Verification: `cargo build`, `cargo clippy -- -D warnings`, `cargo test --bin bingo`
   green; PTY smoke: `/think` opens picker → ↑/↓ → `s` → footer changes, settings.json
   untouched; Enter path writes settings.json.

## 8. Consistency with feedback-states.md v1.18

- State machine discipline: picker open/apply/cancel is a pure state machine with full
  reset; no stuck intermediate state.
- Success: `✓ 思考级别已设置: {level}` (existing) — granular per action.
- Errors: invalid arg → usage line with `[error] code=BAD_ARGUMENT` (§4.5); unknown
  command → `code=UNKNOWN_COMMAND`; error/usage rows outlive success rows (§4.4);
  the no-match hint is not an error level (no code, no focus transfer).
- TTY/non-TTY: all interactive-only chrome; headless `/think xhigh` keeps working and
  printing the same ✓ line (no info lives only in the TUI).
- No animation added (principle 4).

## Sources

- Claude Code docs: interactive mode, commands (code.claude.com/docs/en/interactive-mode, /commands)
- Codex docs: slash commands (developers.openai.com/codex/cli/slash-commands)
