# Feedback States Specification · TUI Presentation Acceptance Checklist

> Version: v1.8 · Deliverable: ui/ux (2026-08-07) · Corresponds to: `notes/design/feedback-states.md` v1.16 §5 + §7
> Purpose: the **presentation-layer acceptance** ui/ux owns in the #14 TUI component-level regression; qa owns the assertion side (AC-15/26/53 state-machine behavior + test infrastructure).
> Lens: **what the user sees, how they operate, whether feedback is timely**. Every acceptance item is an "observable, decidable" pass criterion, independent of implementation internals.

## Cross-reference

| Acceptance area | Corresponding AC | TUI mapping (§5) |
|---|---|---|
| A. Error-line highlight | AC-26 field/page-level presentation | `aria-invalid` red outline → error-line highlight styling |
| B. Scroll-into-view + highlight | AC-26/53 | focus transfer → after the error line renders, scroll into view + highlight |
| C. Status-area update | AC-03/26 | `aria-live` write-empty-string → status-area content update (not row deletion) |
| D. Retry action reachability | AC-15/26/53 | focus lands on the primary action item → retry item reachable/selected |
| E. Spinner rate reduction, indicator kept | AC-47/49 | reduced-motion → spinner rate may drop, indicator never removed |
| F. Flow-level full-screen state | AC-26/53 | page-level error → full-screen state + a way back |
| G. Retry idempotent interaction | AC-15 | action-level de-dup → retry doesn't re-persist, no flicker |
| H. Error-code advanced details | AC-48 | collapsible region → TUI key toggles expand/collapse |
| I. Error copy | AC-28 | "what happened + what you can do" readability |

## A. Error-line highlight

| # | Acceptance item | Pass criterion (observable) |
|---|---|---|
| A1 | Error line distinguishable | The error line is clearly visually distinct from normal content lines (highlight styling: color/inversion/symbol prefix, one of these, distinguishable under a normal terminal color scheme) |
| A2 | Highlight marks only the erroneous object | Field-level errors highlight only the corresponding input line, not the whole interface (corresponding to "red outline marks only the erroneous field") |
| A3 | Error-line content | Contains the specific reason + stable code (`[error] code=...`); the user can perceive "what went wrong" |

## B. Scroll-into-view + highlight

| # | Acceptance item | Pass criterion |
|---|---|---|
| B1 | Error visible when it appears | After the error line renders, it auto-scrolls into view (the user doesn't need to scroll manually to see the error) |
| B2 | Highlight guides attention | After scrolling, the error line is in a highlighted state, a natural visual landing point |
| B3 | No frame skip | Scrolling happens after render, in one step (no user-perceivable second jump); the frame loop naturally satisfies this; acceptance confirms no anomaly |

## C. Status-area update

| # | Acceptance item | Pass criterion |
|---|---|---|
| C1 | Content update, not row deletion | The status area reflects state changes via **content updates** (loading→idle updates to normal status text, not delete-then-re-add) |
| C2 | No leftover error styling | After reset, the status area/error-line highlight is cleared; no "previous round's error styling lingering" |

## D. Retry action reachability

| # | Acceptance item | Pass criterion |
|---|---|---|
| D1 | Retry item selectable | In the error state, the retry item can be selected via keyboard/shortcut (the TUI's "focus" semantics) |
| D2 | Primary action default target | When the flow-level error state appears, the selection defaults to the primary action (retry/return); the user can trigger it directly with Enter |
| D3 | Return path not a dead end | The flow-level error state offers a return action (exit/back one level); no "stuck on the error screen" |

## E. Spinner rate reduction, indicator kept

| # | Acceptance item | Pass criterion |
|---|---|---|
| E1 | Indicator kept | In any loading state, the loading indicator (spinner/status-line mark) exists and doesn't disappear due to rate reduction |
| E2 | Rate reduction perceivable | The animation rate can drop (e.g. slower rotation) but the status-line text clearly says "working" |

## F. Flow-level full-screen state

| # | Acceptance item | Pass criterion (AC-26/53) |
|---|---|---|
| F1 | Full-screen state | A failed long agent turn (transport timeout/interruption) presents a **full-screen error state**, not a small local hint |
| F2 | Error level correct | Long-turn failure → flow-level; short-sync operation timeout → page-level (the two tiers don't mix, distinguishable) |
| F3 | Content complete | The full-screen state includes: error explanation + a retry-or-return path (AC-53), not just "operation failed" |

## G. Retry idempotent interaction

| # | Acceptance item | Pass criterion (AC-15) |
|---|---|---|
| G1 | Retry triggerable | In the timeout error state, the retry action works (actually triggering a retry on top of D1) |
| G2 | No duplicate-persistence appearance | After a timed-out write → retry, the user side shows no duplicate result/duplicate state (action-level idempotency; drop is best-effort) |
| G3 | State-machine reset | Successful retry → back to idle (loading cleared, no error residue); the user can continue (AC-02/03 presentation side) |

## H. Error-code advanced details

| # | Acceptance item | Pass criterion |
|---|---|---|
| H1 | Collapsed by default | By default only human-readable copy is shown; ordinary users aren't disturbed |
| H2 | Expandable by key | A clear key toggles expand/collapse; expanding shows the stable code, collapsible again |

## I. Error copy

| # | Acceptance item | Pass criterion |
|---|---|---|
| I1 | Formula holds | Copy contains "what happened + what the user can do" (e.g. "request timed out, retry" rather than "operation failed") |
| I2 | Terminal-width fit | Copy isn't truncated into an unreadable half-sentence by terminal width (wraps/indents sensibly when long) |

## Infrastructure dependency notes (against dev #59 assessment)

> **#14 infrastructure is landed (dev #86, 561 tests all passing / clippy zero warnings)**: R1 fixtures (`ErrorFixture` 6 fields + `inject()`), R2 Recorder promoted to shared + `assert_row_styled` + `visible_row_containing`, R3b fake-timers + option A (client.rs `maybe_hang` hang hook), #18 presentation-layer minimal implementation (`last_error`-driven: Full=full-screen state / Page/Field=error-line highlight) all ready. The notes below are the historical basis from project kickoff, kept for traceability.
>
> Per dev's assessment: existing = no-runtime pure-logic tests / `TermDriver`+`Recorder` render assertions / UiEvent channel injection; gaps = tokio fake-timers (0 uses) / injectable delayed-failure hooks (AC-50).

| Area | Assertable with existing infra | Needs infra additions (#14) |
|---|---|---|
| A Error-line highlight | ✅ `Recorder` row-level render assertions (highlight/color/symbol) | — |
| B Scroll-into-view + highlight | ⚠️ `TermDriver` scroll counter exists; `Recorder` needs promotion to a shared helper to assert across modules | Recorder promoted to shared |
| C Status-area update | ✅ Render assertions can cover (update not row deletion, no residue) | — |
| D Retry reachability | ⚠️ Selection-state rendering is assertable; depends on Recorder promotion | Recorder promoted to shared |
| E Spinner rate reduction | ⚠️ Indicator existence is render-assertable; the rate-reduction animation needs manual verification | Manual item |
| F Flow-level full-screen state | ⚠️ `TermDriver.screen()` full-screen assertions can cover; but long-turn failure needs **injected failure** to trigger | Test hooks (gap 5) + fixture |
| G Retry idempotency | ⚠️ Timeout-state triggering goes through **parameterized `now` injection** (light; dev #61: chat.rs timing is a `now: Instant` injection pattern), no need to actually wait 10s; retry injection needs test hooks | Test hooks/fixtures (area G no longer depends on fake-timers) |
| H Error-code advanced details | ⚠️ Key toggling is injectable via UiEvent; expanded-state rendering is assertable | Hooks/fixtures (optional) |
| I Error copy | ⚠️ Content is render-assertable; terminal-width fit needs manual verification | Manual item |

**Timing infrastructure tiers (dev #61 pre-study, aligned)**:
- **TUI logic timing (light)**: loading 200ms / toast triggering etc. go through chat.rs's **parameterized `now: Instant` injection** (`on_key_at`/`track_burst`/`ctrl_c` patterns already exist), **no tokio fake-timers needed**; pure logic, compatible with existing no-runtime tests
- **client timeout timing (heavy)**: `tokio::time::timeout` deadline behavior (AC-12/13/14's 10s/15s, AC-54's drop-future cancellation) needs `tokio::time::pause` + runtime — qa decides coverage per the AC stance; both are zero new dependencies
- **Boundary**: toast functionality is **not landed yet**; AC-16/18/19/20/21 (toast 3s timing/pause/de-dup) presentation acceptance is **not in #14's current scope**; schedule separately once toast lands

**Recommendation**: adopt devex's fixture suggestion — **fixture = error-state definition (code+msg+level) → inject → render assertion**, merged with gap 5 (test hooks) into one mechanism; shared by qa assertions and presentation-layer acceptance, and usable as dev's "error-state local preview".

## Acceptance method and boundaries

- **Method**: TUI component-level (once render-assertion capability is ready) + manual visual acceptance (checking each pass criterion under a real terminal color scheme).
- **Division of labor with the qa assertion side**: this checklist verifies "presentation effect" (what the user sees); qa verifies "behavioral correctness" (state-machine transitions, idempotency, timeout mechanics). Both complement each other on the same AC: e.g. AC-26 is qa-asserted as "error enters the flow-level state" + this checklist's F1/F2 verifies "full-screen presentation is correct".
- **Infrastructure dependency**: B1 (scroll into view) and D1 (selection state) need component tests that can assert the selected/scrolled attributes of rendered output; once qa's infrastructure requirement list lands, this checklist can execute.
- **Trigger scenarios**: A/B/C/I can be triggered with short-sync operation errors; F needs injected long-turn failure (transport timeout/interruption); G needs a post-timeout retry injection.

## Fixture error-code coverage list (aligned with qa #63)

> For dev to land fixtures and for dev's local preview; qa assertions and presentation-layer acceptance share the same carrier. Against the complete §4.4 code table (v1.15) **covering all 10 injectable stable codes** — `GENERIC` excluded (main's decision: no actual return point, invisible on normal user paths; the guardrails are covered by error.rs unit tests, not fixtures); items outside #14 scope are marked. **The presented level is decided by the trigger context, not inferred from the code alone** (qa #69 / main #71 increment 2): fixtures must carry a context field (short-sync vs long turn); `TIMEOUT` is a dual-level code.

| FX | code | Level | Scenario | User action | Presentation expectation |
|---|---|---|---|---|---|
| FX-01 | `TIMEOUT` | page-level (context=short-sync) | short-op read timeout | retry | error-line highlight + retry reachable (AC-15/29) |
| FX-02 | `SERVER_ERROR` | page-level | server error | retry later | error-line highlight |
| FX-03 | `OFFLINE` | page-level | no network | check network and retry | error-line highlight |
| FX-04 | `AUTH_REQUIRED` | flow-level | login expired/key missing | log in again | full-screen state + primary action (AC-29) |
| FX-05 | `PERMISSION_DENIED` | flow-level | no permission | go back / request permission | full-screen state + return path |
| FX-06 | `CONFIG_INVALID` | field-level | config validation failure | fix the config | field-level highlight (marks only the object, A2) |
| FX-07 | `RATE_LIMITED` | page-level | rate limited / 429 | retry later | error-line highlight |
| FX-08 | `TOOL_FAILED` | page-level | tool execution failure | check output and retry | error-line highlight |
| FX-09 | `HOOK_FAILED` | page-level | hook execution failure | check the hook config | error-line highlight |
| FX-10 | `STORAGE_ERROR` | page-level | local storage failure | check disk/permissions | error-line highlight |
| FX-11 | `TIMEOUT` (transport) | flow-level (context=long turn) | long agent turn failed | retry or return | full-screen state (AC-53) |
| FX-12 | mixed state (AC-27) | — | batch partial failure | — | **not covered by #14**, pending the mixed-state feature |
| FX-13 | error-code advanced details | collapsed state | H1/H2 | key expand/collapse | hidden by default, expandable to the stable code |

**Sample color baseline**: error color `(255,107,128)`, contrast-distinguishable from normal colors (R2 style-assertion acceptance baseline).
**Priority**: FX-01/04/05/06/11 are the AC-15/26/53/29 core; FX-02/03/07/08/09/10 give full code-table coverage (§4.4 ten codes one by one, AC-29); FX-13 verifies area H. FX-01 and FX-11 share the `TIMEOUT` code; the level is distinguished by context.

## Acceptance record (#20, 2026-08-07)

> Baseline: **564 tests all passing / 0 failures** (dev 561 + qa assertions 3) + test targets compile clean (clippy being confirmed). Fixture carrier = `error_fixtures()` (10 codes + FX-11), shared by qa assertions and presentation-layer acceptance.

**Automated verification passed (fixture inject → `last_error` → Frame::assemble → assertion)**:

| Acceptance area | Verdict | Assertion basis |
|---|---|---|
| A1 Error line distinguishable | ✅ | SegStyle error color + **real cell** error color `(255,107,128)` (`qa_page_error_row_paints_error_color_in_buffer`) |
| A2 Field-level marks only the object | ✅ baseline | AC-29 matrix: Field goes the error-line branch, not full-screen; "the corresponding input line" anchors as an appended line (approximation), noted |
| A3 Error line contains the stable code | ✅ | Matrix asserts `[error] code=...` |
| B1 Error visible | ✅ main path | The error line appends at the end of the content area, visible after submission; the scroll-up scenario doesn't auto-scroll, noted |
| C2 No residue | ✅ | `dismiss_error` clears (Esc/Enter/retry → last_error=None) |
| D2 Primary action | ✅ | Full-screen state Enter=retry |
| D3 Return not a dead end | ✅ | Esc=return (`full_error_shows_full_screen_and_esc_returns`) |
| F1 Full-screen state | ✅ | Full-screen test + AC-29 matrix Full branch |
| F2 Two tiers don't mix | ✅ | `qa_ac53_long_turn_timeout_escalates_to_full_screen`: FX-11 full-screen **vs** FX-01 error-line comparison assertion |
| F3 Content complete | ✅ | title + stable code + explanation + "Enter retry · Esc return" |
| G1 Retry reachable | ✅ | `full_error_enter_retries_last_prompt` (Enter retries the last input, clears the error state, starts a new turn) |
| G3 State-machine reset | ✅ | Enter/Esc clears + busy reset |
| E1/E2 Spinner kept | ⏳ manual | real-terminal visual confirmation (indicator kept / rate reduction perceivable) |
| H Collapsed details | ⚠️ not covered | AC-48 P1; FX-13 not in the injection set (known out-of-scope) |
| I1 Copy formula | ✅ fixtures | fixture msgs all "what happened + what you can do"; production `e.to_string()` copy pending manual spot-check |
| I2 Width fit | ⏳ manual | — |

**Recorded items (non-blocking)**:
1. ~~No production emission path for ShortSync/Field/Page~~ → **resolved (dev #92)**: main #91 decided option ①, dev landed it — `list_models`/`count_tokens` failures now emit `UiEvent::Error { level: Page, context: ShortSync }` (chat.rs:1972/2236), degradation behavior kept (empty menu / budget 0 still usable) + error line visible; TurnStart resets the error state (chat.rs:1031). Production `ErrorLevel::Page`/`ErrorContext::ShortSync` move from dead_code to real emission sources. **FX-01 is now verifiable through the real path** (qa can add a /model-failure → page-level error-line assertion). Field-level stays un-added per main #88 (a conversational CLI has no form fields).
2. Area H collapsed details (AC-48 P1), FX-13, and the FX-12 mixed state: not in #14 scope; schedule later.
3. Manual items (E/I2/A1 visuals under a real terminal color scheme) await manual visual acceptance.

**Conclusion**: **presentation-layer acceptance passed** — the FX-01…11 inject → render chain is fully verified (A/C/D/F/G core + B main path automated coverage); the three parties' stances are consistent (fixture carrier from one source).

## Changelog

- v1.0 (2026-08-07): produced a 9-area acceptance checklist from the feedback-states spec v1.14 §5 TUI mapping table + AC-15/26/53; aligned the division of labor with the qa assertion side (main item 57 scheduled the start).
- v1.1 (2026-08-07): against dev item 59's infrastructure assessment, added "infrastructure dependency notes" — marking per area what existing infra can assert (Recorder row-level assertions, TermDriver scroll/screen, UiEvent injection) and what #14 needs (fake-timers, test hooks, Recorder promotion to shared, manual items); recommended adopting the fixture mechanism (fixture = error-state definition → inject → render assertion, merged with gap 5).
- v1.2 (2026-08-07): against dev item 61's timing-tier pre-study — area G downgraded to "parameterized `now` injection" for triggering the timeout state (no fake-timers needed); added "timing infrastructure tiers": TUI logic timing goes through lightweight now-injection, only client timeout timing needs tokio fake-timers (qa decides coverage); noted toast functionality not landed and AC-16/18/19/20/21 presentation acceptance out of #14 scope.
- v1.3 (2026-08-07): against qa #63's infrastructure requirement list — added the "**fixture error-code coverage list**" (FX-01…13, against the complete §4.4 code table; including the sample color baseline error `(255,107,128)`; marking mixed-state FX-12 out of #14 scope); the fixture carrier is shared by qa assertions and dev's local preview.
- v1.4 (2026-08-07): self-check against the §4.4 v1.14 code table — added FX-14 `GENERIC` (missed in the previous list; qa R1 has it, §4.4 lists it as a published stable code, so it must be renderable and assertable); the list now covers all 11 stable codes in §4.4.
- v1.5 (2026-08-07): main decided three items (qa #69 / main #71) — **withdrew FX-14 `GENERIC`** (no actual return point, invisible on normal paths; guardrails covered by error.rs unit tests, not fixtures; the §4.4 table includes it but fixtures don't); **FX-01/FX-11 gain context fields** (`TIMEOUT` dual level: short-sync = page-level, long turn = flow-level; level decided by context, not inferred from the code alone); the list covers all 10 injectable stable codes in §4.4, consistent with AC table v1.9/v1.9.1 section C (verified consistent in practice by qa #76).
- v1.6 (2026-08-07): #14 infrastructure + presentation-layer minimal implementation landed (dev #86) — all prerequisites for executing this checklist ready: R1 fixtures with 6 fields (`{ code, msg, context, level, action, expect_style }`, level/context use the production contracts `error::ErrorLevel`/`error::ErrorContext`, FX-05 at flow-level slot), `inject()` → `UiEvent::Error { code, msg, level, context }` → `last_error`-driven rendering (Full=full-screen state / Page/Field=error-line highlight); doc §5 "level carried by the producer" + §3.1 "typical level overridable" synced (feedback-states.md v1.16). Presentation-layer acceptance (#20) executable.
- v1.7 (2026-08-07): **#20 presentation-layer acceptance record** — 564 tests all passing / 0 failures; A/C/D/F/G core + B main path automated verification passed (qa AC-29 matrix + AC-53 dual-tier comparison + real-cell style assertions); 3 recorded items (no production ShortSync emission path / area H collapsed details P1 / manual items). Conclusion: presentation-layer acceptance passed.
- v1.8 (2026-08-07): recorded item 1 updated — dev #92 landed production Page+ShortSync emission sources (list_models/count_tokens + TurnStart reset; main #91 decided option ①), **FX-01 is verifiable through the real path**; doc sync complete (feedback-states.md v1.16 including §4.4's "short-op degrade-visibly" stance). Remaining recorded items: area H collapsed details P1 + manual items.
