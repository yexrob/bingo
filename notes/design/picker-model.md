# Generic Picker component model (picker-model.md)

> Status: draft (to be finalized via Post after #slash-ux alignment) · 2026-08-07 · Team A (feat/slash-ux)
> Task origin: main item #18 — user feedback: "the /think picker optimization is great; apply the picker model to all similar slash interaction scenarios".
> This file = the design (step one, no code). Interaction model led by: ui/ux; review: devex; implementation-surface assessment: dev (this file).
> Regression floor: **the abstraction layer must not change ThinkMenu behavior; the 644 tests pass as-is**.
> Cross-references: `slash-command-ux.md` (contract v0.4, /think picker spec), `slash-ux.md` (dev implementation plan), `research.md` D4/D16/D26/D30.

## 1. Generalized abstraction: data-driven generic Picker

### 1.1 Interaction model distilled from ThinkMenu (generalization of contract v0.4 §3)

| Element | ThinkMenu (existing instance) | Generic Picker contract |
|---|---|---|
| Data | `items` (static 6 items: label+desc) | `Vec<PickerItem { label, description }>` (static or async-filled) |
| Selection | `selected: usize` (pure index; browsing is side-effect-free) | same — **effects are written only on the confirm key**; Esc naturally needs no rollback |
| In-effect state | `current: usize` (● fixed) | `current: Option<usize>` (None = no in-effect concept) |
| Browse keys | ↑↓ wrap | same |
| Direct-jump keys | 1-6 (`1..=items.len()`) | optional `number_jump`; **only `1..=min(items.len(), 9)`** (>9 items: ↑↓ only, not enough digit slots) |
| Confirm key | Enter = apply + persist | Enter = apply (whether to persist is decided by the scenario semantics) |
| Session-level key | `s` = session-only (no settings write) | optional `session_only` (aligns with CC `/model`'s `s`) |
| Preview | footer `think {level} ▸` (live while browsing) | optional `preview`: while browsing, the footer's corresponding segment shows `{label} ▸` |
| Row rendering | `  {❯\|●}{label:<w}  {desc}` + dim hint row | same (marker(2) + name column + desc column + optional hint row) |
| Hint row | `↑↓ select · Enter confirm and save · s session-only · 1-6 jump · Esc cancel` | optional `hint_row` (copy assembled from the scenario's keys) |

### 1.2 Implementation-surface assessment (dev) — two landing options

**Option A (composition thin shell; recommended first step)**: new pure core `src/tui/picker.rs`:

```rust
// src/tui/picker.rs (pure logic, zero rendering deps, unit-testable)
pub struct PickerItem {
    pub label: String,        // display name (render column)
    pub value: String,        // applied value (written on Enter/s landing; separate from label, e.g. /resume's session name)
    pub description: String,  // description column (optional empty)
}
pub struct PickerModel {
    pub items: Vec<PickerItem>,
    pub selected: usize,
    pub current: Option<usize>,
}
impl PickerModel {
    pub fn move_selection(&mut self, step: isize) -> bool;  // wrap
    pub fn jump(&mut self, n: usize) -> bool;               // 1..=min(len,9)
    pub fn row(&self, index: usize, width: usize) -> Row;   // ●/❯/name/desc rendering (reuses contract §3.3 layout)
    pub fn hint_row(&self, width: usize, keys: &PickerKeys) -> Row;
}
pub struct PickerKeys { pub session_only: bool, pub number_jump: bool }
```

- `ThinkMenu`/`ModelMenu` become thin shells: hold a `PickerModel` + the scenario differences (two-level structure / async state / preview text / confirm action); **the Chat field types and public API stay unchanged** → existing tests unchanged, 644 pass as-is.
- Key events are still dispatched by `Chat.on_key` (the priority order stays the single source of truth); the shell calls core pure functions + scenario actions.
- Rendering: the `suggestion_rows` think/model arms read the shell's `rows()` (the width-budget logic moves into the core; the contract §3.3 layout invariants are preserved).
- Footer preview: the shell exposes `preview_text()`; `footer_row` reads it (ThinkMenu instance = `think {label} ▸`; scenarios without preview return None).

**Option B (unified replacement)**: merge `Chat.think_menu/model_menu` into `Option<Picker>`, changing the Chat API — large blast radius (tests, chrome, on_key all move); **not the first step**; only evaluate on demand after Option A stabilizes.

**Two-level/async differences** (/model): PickerModel manages "the current level's list + selection"; level switching (Enter into level two, Esc back to level one) and async loading (loading/empty hint rows) stay in the ModelMenu shell — the core doesn't know about them. Rationale: two-level + async is a model-selection-specific shape; pushing it into the core would pollute single-level scenarios (over-engineering).

### 1.3 Interactions not migrated (explicitly excluded, reasons on record)

- **Slash dropdown completion** (`/` + filter + Tab complete): it's "filter-complete", not "cycle-select"; a different interaction pattern — not migrated.
- **Entity selector** (D30, ctrl+g focus + window slide + Enter opens modal): window sliding and entity semantics are special — not migrated.
- **Permission dialog** (1-9 number selection, D22/D2): an independent modal channel — not migrated.

## 2. Candidate scenario list (assessed one by one)

| # | Scenario | Fit value | Adaptation differences | Priority |
|---|---|---|---|---|
| 0 | **/think** | existing instance; the abstraction layer must not change its behavior (regression floor) | — | commit A (pure refactor) |
| 1 | **/theme** | high: 3 static items (dark/light/auto), current value can be marked ●, 1-3 direct jump | ⚠ **behavior change point**: today `/theme` with no arg = directly switches to auto; picker-ized, no arg = opens the picker (aligns with CC "no arg → opens a picker"). Needs ui/ux ruling: keep the auto shortcut (e.g. `/theme auto`) or change the semantics outright. Footer has no theme segment → no preview; no `s` (theme persistence is the design); hint row = `↑↓ select · Enter confirm · Esc cancel` | ★★★★★ commit B |
| 2 | **/provider** | high: single-level static (`default` + settings.providers), current marked ●, footer already has a provider segment → preview meaningful | `s` = session-only (today `/provider` always persists; same semantics as /think's `s`); options can exceed 9 (number jump degrades); no async | ★★★★☆ commit C |
| 3 | **/resume** | high: session list (`transcript::list` synchronous disk read = static snapshot), Enter switches session — CC's /resume is exactly a picker | current = the current transcript (mark ● when present); no preview (footer has no session segment); no `s`; option description = session name/message count | ★★★★☆ commit D |
| 4 | **/model** | medium-high: two-level (provider → models) + async + loading/empty hint | biggest difference: two-level switching + async state stay in the shell (§1.2); footer preview of limited value (list is async) — optional, can skip; `s` key stays deferred (main ruling #6); number jump 1..N per level | ★★★☆☆ commit E (last, highest risk) |
| 5 | **/mcp** | low: subcommand text interface (list/enable/disable/reconnect/status badges), not option selection | enable/disable target selection could use a picker, but the overall shape doesn't fit — **not migrated** (reason on record) | — |
| 6 | **/permissions** | none: rule list + addition, not a selection scenario | — | — |
| 7 | **/skills** | medium: `/skill-name` direct execution is already fast; picker-izing is "browse-then-execute", medium value — not migrated this round (on record) | dynamic (skills directories) | — |
| 8 | **/team** | none: subcommand family dispatch (G1 subcommand completion is a deferred item), not value selection | — | — |

**Assessment criterion**: fit value = whether the "option list + confirm" interaction naturally holds; scenarios that don't fit are not forced (main boundary 3).

## 3. Migration strategy (one scenario at a time; each step an independent, revertable commit)

1. **Commit A (`refactor:`, zero behavior change)**: create `src/tui/picker.rs` (PickerModel pure core + rendering + key-transfer pure functions); ThinkMenu becomes a thin shell; **regression floor = the 644 tests pass as-is** (Chat's public API unchanged → ThinkMenu's existing tests are kept verbatim). New acceptance tests (§5 devex additions): core pure-function tests (wrap/jump boundaries/row layout/hint assembly), empty-items defense, value≠label landing, shell confirm actions.
2. **Commit B: /theme picker** — 3 static levels, lowest cost; incidentally fixes the "no-arg = silently switches to auto" behavior gap; the behavior change needs ui/ux's finalized no-arg semantics + guide.md sync.
3. **Commit C: /resume picker** — the highest DX value (removes "look at the list → type the name by hand"), dynamic single-level (disk scan injects items); guide.md sync.
4. **Commit D: /provider picker** — static single-level, list info columns (URL/key) merged into description; `s` semantics optional; guide.md sync.
5. **Commit E: /model two-level migration** — the Picker manages level-one providers; the level-two model list stays independent (async shell); done last.
6. Each step is an independent, individually revertable commit; stop when a scenario doesn't fit (no forced migration).

## 4. Boundaries

- **Zero new dependencies**; no crate beyond ratatui.
- **feedback-states spec unchanged**: picker confirm = transient ✓ row (success 2s), error/usage = error bucket (≥8s + cleared on next input), no new motion, full state-machine reset (Esc close clears all transient state); new scenarios reuse the error-code format (e.g. `BAD_ARGUMENT`).
- **Docs sync**: every migrated scenario's behavior changes sync to `guide.md` (slash quick reference) + `feedback-states.md` changelog.
- **Regression floor**: 644 tests all green (commit A as-is); `cargo build` / `clippy -- -D warnings` clean.
- **No touching dev/main**; only feat/slash-ux.

## 5. Alignment record (devex review #21 passed; 4 questions finalized)

1. **/theme no-arg semantics: open the picker (CC-aligned)** — incidentally fixes the hidden gap of "no-arg silently switches to auto with no feedback"; the explicit `/theme auto` shortcut stays; guide.md syncs the behavior change.
2. **/provider's `s` key: do it** — PickerKeys.session_only config bit costs ≈0; consistent with /think; no conflict with /model's `s` defer (model switching has a confirmation-cost flow; provider is lightweight).
3. **Commit A's core includes hint_row: yes** — copy assembled from the keys config; the row-count budget keeps the narrow-screen drop rule (think's 6+1 rows already sit inside that rule).
4. **/model's footer preview: deferred** — the async list gains little, and it would mean touching the footer_row model segment.

### devex testability additions (merged into commit A acceptance in §3)

1. PickerModel pure-core tests: move_selection wraps at both ends / jump boundaries (`1..=min(len,9)`, out-of-range returns false without panicking) / row rendering (●❯ overlapping row, name-column width, desc truncation per contract §3.3) / hint_row assembled from PickerKeys.
2. **Empty-items defense (needed per scenario after generalization)**: empty items → the menu doesn't open, falls back to the usage line — test-covered.
3. **value ≠ label landing test**: /resume's label=display name, value=session key — assert the landed value uses `value`, not `label`.
4. Each scenario shell gets at least one "confirm action lands correctly" test (theme switches theme / resume switches session / provider switches provider + persistence semantics).

### devex DX additions

1. **resume option-count cap**: items truncated (most recent 20 + the hint row notes it), desc carries session name + message count/time.
2. **provider desc keeps the info columns** (URL/masked key reusing the existing 4-char key display logic).
3. **Commit A marked `refactor:`** (extracting the PickerModel pure core, zero behavior change), distinguished from the later feat commits.

## 6. Acceptance anchors (ui/ux additions; qa assertion basis)

**Commit A (abstraction extraction, regression floor)**
- [ ] the 644 tests pass as-is (the existing /think test assertions have **zero changes** — they are the acceptance suite for the abstraction layer, not an equivalent rewrite)
- [ ] `PickerModel` pure logic unit-testable: `move_selection` wrap upper/lower bounds, `jump` boundaries (1..=min(len,9), out-of-range returns false without panicking), out-of-range clamp
- [ ] **empty-items defense (devex addition)**: needed per scenario after generalization — empty items → the menu doesn't open, falls back to the usage line; test-covered (contract §3.6 previously only had it for think)
- [ ] **value ≠ label landing test (devex addition)**: /resume's label=display name, value=session key — assert the landed value uses `value`, not `label`
- [ ] `/think` behavior word-for-word unchanged: dual markers (●/❯ overlapping row), 1-6 direct jump, `s` session-level, footer ▸ preview, Esc revert, narrow-screen truncation, hint row
- [ ] chrome row-budget invariant preserved (marker(2) + name column + desc column + hint row; whole-row truncation on narrow screens)
- [ ] commit marked "refactor: extract PickerModel pure core (zero behavior change)", distinguished from the later feat commits

**Commits B-D (per scenario)**
- [ ] /theme: no-arg opens the 3-item picker (per the finalized semantics), `/theme dark` fast path unchanged, Enter persists + ✓ row, Esc no side effects
- [ ] /resume: no-arg opens the session picker, Enter switches session, Esc doesn't change the current session, empty-list hint row; **option truncation cap (e.g. most recent 20 + hint row notes it, devex DX addition)**
- [ ] /provider: no-arg opens the picker (● marks current, URL + masked key into the desc column — masking logic follows along, existing 4-char key display), `/provider <name>` unchanged, `s` session-level (no settings write), footer preview `{provider} ▸`
- [ ] each scenario shell has at least one "confirm action lands correctly" test (theme switches theme / resume switches session / provider switches provider + persistence semantics)
- [ ] each scenario: error/usage lines go to the error bucket (`[error] code=…` format reused), success ✓ rows 2s TTL

**Commit E (/model, optional)**
- [ ] two-level: Enter into level two, Esc back to level one then Esc closes; loading/empty hint rows kept; selected/current clamped after async reload

**Common**
- [ ] `cargo build` / `clippy -- -D warnings` / `cargo test --bin bingo` green per commit
- [ ] each commit syncs the guide.md quick reference + feedback-states changelog (AGENTS.md sync rule)
- [ ] feat/slash-ux only, no touching dev/main

## 7. Three-party output merge record

- **dev main body** (this file): PickerModel shape + options A/B + scenario matrix (0-8) + migration strategy + boundaries
- **devex input** (items #19/21): /mcp judged ❌ (status display, not value selection), /resume has the highest DX value (promoted to commit C), review floor (core = single-level, `s` off by default), /theme hidden behavior-gap argument (into §5.1), empty-items defense / value≠label test / per-scenario confirm-action test / resume truncation cap / provider desc masking (into §6)
- **ui/ux finalization** (interaction model): §6 acceptance anchors + §8 finalized positions

## 8. Interaction model finalized (ui/ux · on the four §5 questions)

1. **/theme no-arg = open the picker** (CC-aligned + fixes the "no-arg silently switches to auto" hidden gap); the `/theme auto|dark|light` fast paths stay as explicit shortcuts. guide.md syncs the behavior change.
2. **/provider's `s` key: do it**: PickerKeys.session_only costs ≈0, consistent with /think; no conflict with /model's `s` defer (model switching has a confirmation-cost flow; provider is lightweight).
3. **hint_row assembled by the core** (copy driven by the keys config), not copied per scenario; the row-count budget keeps the narrow-screen drop rule.
4. **/model footer preview deferred**: the async list gains little, and it would mean touching the footer_row model segment.
