# TUI Component-Level Regression Test Infrastructure Requirements (#14)

> Status: v3.1 (2026-08-08, complete — the four component assertions remain green; the AC-15 server-side boundary was finalized as non-applicable because short-sync writes are pure generation with no persistence side effects, as recorded in `feedback-states-ac.md` v1.9.2).
> Related: `notes/design/feedback-states.md` (design v1.22), `notes/design/feedback-states-ac.md` (AC table v1.9.3),
> `notes/design/feedback-states-presentation.md` (presentation acceptance checklist v1.8, ui/ux)
> Roles: qa provides requirements → dev implements infrastructure → qa asserts + ui/ux accepts → devex DX re-review (#15, closed)

## 0. Alignment conclusions (three-party consensus)

- **Fixtures adopted** (devex #58 / ui/ux #60): error-state fixtures and test hooks merge into one mechanism
- **Timing tiers adopted** (dev #61 / ui/ux #62): TUI logic timing goes through parameterized `now: Instant` injection (light); client timeout timing goes through tokio fake-timers (heavy)
- **Toast moved out of #14** (ui/ux #62): AC-16/18/19/20/21 have no implementation target; schedule separately once toast lands
- **qa increment**: style-aware assertions (`Recorder` row-level text assertions don't include cell style; highlight assertions need an addition)

## 1. Infrastructure requirements (ordered by ui/ux priority; P0 = required by #14)

### R1 Test hooks + error-state fixtures (priority 1)

**Requirements**
- `ErrorFixture { code, msg, level, context, action, expect_style }` data layer (**6 fields; level restored per ui/ux #80 ②**)
- **`level` field (explicit anchor for the presented level)**: for non-TIMEOUT codes the level is an **inherent property of the code** — CONFIG_INVALID=field-level, AUTH_REQUIRED/PERMISSION_DENIED=flow-level, the rest=page-level; `context` can't express that ("short operation" would wrongly derive page-level). Explicitly carrying it is directly assertable and reviewable, without duplicating the §4.4 mapping logic
- **`context` field (qa #69 alignment increment 2; a hard input to the R1 design)**: the presented level can't be inferred from the code alone — for the same `TIMEOUT` code, a short-sync read timeout = page-level (FX-01), a long-turn transport timeout = flow-level (FX-11; client.rs:67 confirmed to land on TIMEOUT). **level/context must survive to the render path (ui/ux #80 ③)**: `UiEvent::Error` currently only carries `{ code, msg }`, so the render layer can't get the level — the #18 production change must extend it to carry level/context (known at emission in chat.rs:2748/2792), otherwise the same TIMEOUT code can't distinguish FX-01/FX-11
- Injected through the mpsc events channel (reusing `test_chat()`, no new injection point)
- Render assertion chain: inject → `Frame::assemble(&chat, size)` → assert
- Also serves as dev's "error-state local preview" + the ui/ux acceptance carrier
- Injectable delay/failure responses (client mock or a start_turn injection point — **the only design point with a large change surface; dev must propose a plan first**)
- AC-15 write idempotency needs "delayed write-response injection + persistence call counting"

**Error-code coverage (finalized against ui/ux #68 FX-01…13)**
`TIMEOUT` (dual context: page-level + flow-level) / `SERVER_ERROR` / `OFFLINE` / `AUTH_REQUIRED` / `PERMISSION_DENIED` / `CONFIG_INVALID` / `RATE_LIMITED` / `TOOL_FAILED` / `HOOK_FAILED` / `STORAGE_ERROR` + long-turn transport timeout (FX-11)
- **`AUTH_EXPIRED` contract drift (qa #69 increment 1; main already decided option ①)**: the design's §3.1/AC-29 originally used `AUTH_EXPIRED`, but the implementation doesn't have it (client.rs lands 401 on `AUTH_REQUIRED`). main adopted option ① (align the doc to the implementation; no new code) — §3.1/AC-29 already changed to `AUTH_REQUIRED` (doc v1.15 / AC v1.9); fixtures can just use `AUTH_REQUIRED`
- **`GENERIC` doesn't enter fixtures**: no actual return point, invisible on normal user paths; the explicit-GENERIC guardrail assertions are covered by error.rs unit tests

**Acceptance assertions**
- Injecting any fixture failure → the corresponding error level renders (code text + expect_style)
- Injecting a 9s delay → loading state visible → `TIMEOUT` triggers at 10s
- Write retry persistence count = 1 (idempotent)

### R2 Recorder promoted to shared + style awareness (priority 2)

**Requirements**
- Promote `Recorder` from term.rs's test module to `pub(crate)` (screen/scrollback/counters)
- New style-aware assertion: `assert_row_styled(y, fg, bg, contains)` (cell fg/bg colors)
- New viewport locator: `visible_rows()` (for "error line scrolls into view" assertions)

**Acceptance assertions**
- Both the same-row text assertion and the style assertion pass
- Style assertions can distinguish the error color `(255,107,128)` from normal colors

### R3 Timing infrastructure (priority 3)

**R3a Parameterized now injection (light)**
- Reuse the `now: Instant` parameter pattern from `on_key_at` / `track_burst` / `ctrl_c` to add coverage
- Covers: AC-07 loading 200ms, area-G timeout-state triggering
- No fake-timers dependency; compatible with existing no-runtime `#[test]`

**R3b tokio fake-timers (client timeout timing only)**
- Covers: **AC-12/13/14 deadline behavior assertions (P0; currently constants-only assertions)**
- P1 enhancement: AC-54 drop-future cancellation (structural guarantee already passed; add a "drop at deadline" hook counter)
- A 20s long turn doesn't trigger the short timeout (AC-53)

### Explicitly out of #14
- Toast timing (AC-16/18/19/20/21) — no implementation target

## 2. AC cross-reference

| AC | Content | Infrastructure |
|---|---|---|
| AC-15 | Timeout retry idempotency + reset | R1(delay+persistence count)+R3a+R2 |
| AC-26 | Full-screen error state + return path | R2(style+viewport)+R1 |
| AC-53 | Long-turn failure escalation | R3b+R2+R1 |
| AC-12/13/14 | Short-timeout tier behavior | R3b |
| AC-07 | Loading 200ms | R3a |
| AC-29 | Error code → action TUI | R1(fixture per code) |

## 3. Implementation order

1. **dev**: R1 injection plan design → R2 promotion → R3a → R3b → fixtures finalized
2. **ui/ux**: fixture error-code coverage list aligned with qa; acceptance checklist v1.2 ready
3. **qa**: after R1 injection plan confirmation, sync assertion details; once infrastructure is ready, assert per AC against the table above
4. **devex**: DX re-review wrap-up (#15)

## 4. ⚠️ Presentation-layer gap warning (awaiting main's decision)

`UiEvent::Error` (chat.rs:1439) currently only replaces the last assistant message text with
`[error] code=... msg=...` — **no error-line highlight, no full-screen error state, no retry/return path**.
ui/ux's acceptance checklist areas A/F/D/G have no implementation target. Once infrastructure is ready, assertions will expose this gap.

Recommendation: infrastructure first → assertions expose the gap → this iteration adds the minimal presentation-layer implementation
(error-line highlight styling + full-screen error-state skeleton + retry/return path) → ui/ux accepts.
If not added this iteration, AC-15/26/53 stay in the "gap exposed" state.

## 5. qa assertion spec draft (deliverable 3/3 prerequisite; pure contract semantics; mapped once the injection API lands)

> Status: draft (2026-08-07). Contract anchors finalized (`AUTH_REQUIRED` as the single code; `TIMEOUT` level decided by context).
> Dependencies: the R2 render-assertion side is ready (`src/tui/test_util.rs`: shared Recorder + `assert_row_styled`
> + `visible_row_containing`, including error-color self-tests ✅); the R1 injection side (fixture data layer / injectable delay /
> persistence counting) is in progress; the presentation-layer minimal implementation (#18) pending — **highlight/full-screen assertions depend on #18**.
> **Fixture 6 fields (ui/ux #80 ②③)**: `{ code, msg, level, context, action, expect_style }`;
> `level` explicitly carried (asserting "presented level correct" anchors directly); `level/context` must survive to the render path
> (#18 must extend `UiEvent::Error` to carry level/context, otherwise the TIMEOUT code can't distinguish page/flow level).

### AC-15 Timeout retry idempotency (short-sync write timeout)
Prerequisite: inject a 9s-delayed write response → virtual time advances to 10s (R3b).
- ① Error-state rendering: the visible area contains `[error] code=TIMEOUT` (`visible_row_containing`)
- ② Error-line highlight: `assert_row_styled(y, fg=error color (255,107,128), None, "TIMEOUT")` (depends on #18)
- ③ Retry reachable: the retry item is selectable (selection-state render assertion, depends on #18)
- ④ Successful retry → state-machine reset: busy=false, no error residue, loading cleared (AC-02/03)
- ⑤ Write idempotency: persistence count = 1 (R1 persistence-count hook)

### AC-26 Flow-level full-screen state
Prerequisite: inject flow-level fixtures (AUTH_REQUIRED / PERMISSION_DENIED / long-turn TIMEOUT).
- ① Full-screen state presented: error title + explanation (what happened + what you can do) + primary action + exit action (`screen()`)
- ② Return path not a dead end: the exit/return action is selectable (D3)
- ③ Selection lands on the primary action (D2)

### AC-53 Long-turn failure escalation
Prerequisite: inject a long-turn transport timeout (context=long turn).
- ① Virtual time 20s doesn't trigger the short timeout (10s/15s, R3b)
- ② On failure, a flow-level full-screen state (not a local hint; F1/F2)
- ③ Contains a retry-or-return path (F3)

### AC-12/13/14 Short-timeout tiers
- ① A read must report at 10s before 11s (R3b)
- ② A write must not report at 15s before 14s (R3b)
- ③ TIMEOUT code + page-level presentation (distinct from flow-level; decided by context)

### AC-29 Error code → action (per code)
- FX-01~11 injected per code → code text + action reachable + presented level (decided by context)

### AC-07 Loading 200ms
- R3a now injection: the loading state is visible within 200ms and fades afterward.

### Execution dependency matrix
| Assertion | R1 injection | R2 rendering | R3a | R3b | #18 presentation layer |
|---|---|---|---|---|---|
| AC-15 | needed (delay+count) | needed | — | needed | needed (highlight+retry) |
| AC-26 | needed (flow-level fixture) | needed (style+viewport) | — | — | needed (full-screen state) |
| AC-53 | needed (long-turn fixture) | needed | — | needed | needed (full-screen state) |
| AC-12/13/14 | needed (delay) | needed | — | needed | partial (page-level highlight) |
| AC-29 | needed (per-code fixture) | needed | — | — | needed |
| AC-07 | — | needed | needed | — | — |
