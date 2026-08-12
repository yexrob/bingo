# Feedback States Specification · Acceptance Assertion Table (AC)

> Version: v1.10 (qa deliverable + devex contract alignment) · Corresponding design doc: `notes/design/feedback-states.md` **v1.35** (§3.1/§4.4 contract alignment plus real fullscreen/idle-host feedback regressions)
> Purpose: dev implements against the AC table; qa regresses against the AC table. **Assertions always anchor on the error code (`[error] code=...`), never on msg copy**.
> Priority: P0 = must pass the release gate; P1 = should pass, schedulable as follow-up.
> Assertion methods: unit (fake timers / tokio `pause`+`advance`, ms-level determinism) ｜ integration (spawn a real CLI, non-TTY) ｜ component/E2E (TUI, smoke-only, no timing assertions) ｜ SR audit (manual screen reader, web frontend only).

## dev implementation cross-check (2026-08-07, doc v1.12)

The following assertions are landed in the dev implementation; qa can verify them directly during regression (assertion anchors unchanged):

- **AC-30/31/32**: the top-level CLI exit is wired — `main` catches the top-level `Box<dyn Error>` and, when non-TTY (`stderr().is_terminal()` is false), outputs `[error] code=<SCREAMING_SNAKE> msg=<single line ≤200>`; msg goes through `src/error.rs::sanitize_msg` (newline/tab normalization + 200-char truncation). Verified in practice: `[error] code=AUTH_REQUIRED msg=missing API key...`.
- **AC-36 dual-exit consistency**: GUI (TUI `UiEvent::Error { code, msg }`) and CLI (`report_error` → `error_code_boxed`) share the `src/error.rs` code table; the TUI producer side goes through `map_error`, the CLI through `error_code_boxed` (downcasting the cause chain to a concrete type before taking the code); both sides share one source.
- **AC-38/40/41/43/44**: all 10 module error enums implement `ErrorCode` (exhaustive match, no `_` arm; a new variant fails to compile); drift-guard unit tests enumerate every variant of every module asserting non-`GENERIC`; `GENERIC_ALLOWLIST` is currently empty (all variants registered with stable codes); the contract is centralized in `src/error.rs`.
- **AC-45**: `TIMEOUT`/`AUTH_REQUIRED`/`PERMISSION_DENIED`/`SERVER_ERROR`/`OFFLINE`/`CONFIG_INVALID` all implemented per §4.4; newly registered `RATE_LIMITED` (429), `CONTEXT_OVERFLOW` (terminal provider context rejection), `STORAGE_ERROR`, `TOOL_FAILED`, `HOOK_FAILED` (see doc §4.4).
- **AC-12/13/14/53/54**: feedback-layer tiered timeouts landed — `SHORT_READ_TIMEOUT=10s` (list_models/count_tokens), `SHORT_WRITE_TIMEOUT=15s` (complete_text, wrapping the entire operation including retries), long-turn streams keep the transport layer 120s/60s; the feedback layer's `tokio::time::timeout` drops the future at the deadline, cancelling the underlying request. client.rs has the `feedback_timeout_tiers_are_read_10s_write_15s` constant assertion.
- **AC-52**: `UiEvent::Error { code, msg }` structured (was `Error(String)`); chat.rs's consumer renders `[error] code=... msg=...`, the producer side takes the code via `map_error` — not `to_string()` concatenation.
- **AC-50**: timing tests reuse chat.rs's existing no-runtime pure-logic test pattern with `tokio::time::pause/advance` infrastructure; the concrete fake-timer cases are added by qa against this table.
- **AC-33 not applicable for now**: the `detail=` output channel (JSON-escaped multi-line stacks) is **only triggered by `--verbose`**; with no `--verbose` today there's no trigger point, so it's not implemented yet; `sanitize_msg` already keeps the primary msg single-line (covered by AC-31/32), so there's no gap on the code-table side. **Add this assertion when `--verbose` is implemented** (msg stays single-line + `detail=<JSON-escaped>`, see §F).

Component-level follow-up is closed for AC-15/26/53/29: retry/reset, long-turn escalation, full-screen presentation, per-code actions, and the real short-sync path are covered by the regression record below. The 2026-08-08 follow-up additionally exercises the alternate-screen host seam and idle slash-error TTL; no item in this paragraph remains a release backlog.

## TUI mapping baseline (dev review item 21, merged into doc section 5 by ui/ux)

bingo's stack is **ratatui TUI + headless CLI** — no DOM / aria-* / CSS animation / prefers-reduced-motion / rAF. Web-side terms in this table map per the table below; **assertions go by observable TUI behavior**:

| Norm item (Web) | bingo TUI mapping |
|---|---|
| aria-busy / loading | `chat.busy` + status line (exists) |
| aria-invalid red outline | error-line highlight styling |
| aria-live write-empty-string (not node deletion) | status-area content update (not row deletion) |
| Focus transfer (async after render) | after the error line renders, **scroll into view + highlight**; the TUI frame loop is naturally post-render |
| prefers-reduced-motion | no such concept in TUI: spinner animation rate may simply degrade; **the indicator is never removed** |
| role="alert" / aria-describedby | error-line highlight + visible together with the associated input line |

(If a web frontend exists in the future, follow doc section 5 as-is; AC-22/46's aria items are marked "web-side conventions; bingo TUI asserts via the mapping".)

---

## A. State-machine reset and races (design doc: state-machine section + §7)

| ID | Trigger | Expected (quantifiable) | Assertion method | Priority |
|---|---|---|---|---|
| AC-01 | Any async operation completes (success) | Full `idle→loading→success→idle` loop; `chat.busy` cleared, status line updates back to normal | Unit (fake timers) | P0 |
| AC-02 | Any async operation fails | Full `idle→loading→error→idle` loop; after the error shows, back to idle, triggerable again | Unit (fake timers) | P0 |
| AC-03 | The four reset actions (TUI mapping) | (1) `chat.busy`/loading cleared, status line updated; (2) error line **highlight removed** (no leftover error styling); (3) status area **content updated** (not row deletion); (4) success → next operation visible; failure → error line/retry item **scrolled into view + highlighted** | Unit + component | P0 |
| AC-04 | Focus-transfer timing | The TUI frame loop is naturally post-render (no rAF issue); assert the error line **after rendering** scrolls into view + highlights; skip without blocking if rendering fails | Unit + component | P0 |
| AC-05 | Stale-response race | After a retry/new request, late success/failure responses from the old request are ignored; **the new success must not be flashed over by a stale error**. **Implementation mechanism (qa regression confirmed, ui/ux v1.14 aligned)**: the race protection is structural — **drop future + cancel channel** (query.rs `cancel_requested`/`aborted`); **there is no standalone sequence-number counter** — structural cancellation takes precedence; sequence-number checks were not separately implemented | Unit (injected delayed response sequences) | P0 |
| AC-06 | Timeout timer cancellation | The timeout timer is **synchronously cancelled** on success/failure/cancel; no late error enters the error state within the delayed window after success | Unit (fake timers; advance past the timeout point, then assert no error) | P0 |

---

## B. Loading (design doc §1)

| ID | Trigger | Expected (quantifiable) | Assertion method | Priority |
|---|---|---|---|---|
| AC-07 | Async operation takes >200ms | Loading state appears after 200ms (±50ms); completing in <200ms does **not flicker** (no appear-then-disappear) | Unit (fake timers) | P0 |
| AC-08 | Local operation | Spinner **replaces the icon slot in place** at the operation site (button/action row); copy unchanged (keeps "Submit") | Component | P0 |
| AC-09 | Any loading state | **Fullscreen blocking forbidden**; page-level = skeleton/placeholder in the content area + status line (reusing `chat.busy`) | Component | P0 |
| AC-10 | Submit action triggered during loading | De-dup at the **submit action** granularity: command input/Enter/shortcut submission paths intercepted uniformly (`isSubmitting` gates the onSubmit equivalent); no second request is produced | Unit + component | P0 |
| AC-11 | Same action rapid-clicked | Idempotency guarantee: only 1 request for the same submit action during loading (injectable counting hook verifies call count = 1) | Unit (test hooks) | P0 |

---

## C. Timeout (design doc §1 timeout row + §7; dev item 31 settled stance)

> **Timeout layering (settled by dev, 2026-08-07)**:
> - **`TIMEOUT`'s presented level is determined by the trigger context**: short-sync = page-level (AC-12/13/14), long turn = flow-level (AC-53); fixtures/assertions must not infer the level from the code alone.
> - **Short sync operations** (`list_models`/`count_tokens`/`complete_text` etc.): the feedback layer applies read 10s / write 15s; timeout means failure (`TIMEOUT` code), **primary action = retry** (page-level).
> - **Long agent turns (streaming + multi-round tools)**: **do not apply 10s/15s** — continuous progress feedback already exists within a turn (status line + activity rows + `chat.busy`); timeouts are backed by the transport layer (120s/60s) + user interruption.
> - **A genuinely failed long turn (transport timeout/interruption) → escalates to a flow-level error** (AC-26), offering a "retry or return" path, never a silent local hint.
> - **Cancellation mechanism**: existing client requests are already wrapped in tokio `timeout()` / droppable streams — the feedback-layer timeout **wraps one more timeout and drops the future at the deadline**, cancelling the underlying reqwest connection; **the sequence-number check is only a fallback for late responses on non-timeout paths** (doesn't truly cancel).
> - **Write-path defense**: dropping is best-effort for "the server already applied the write"; on timeout→retry, action-level idempotency is still recommended as a fallback (preventing duplicate persistence under extreme races).

| ID | Trigger | Expected (quantifiable) | Assertion method | Priority |
|---|---|---|---|---|
| AC-12 | **Short-sync** read not done in 10s | Enters the corresponding error level after 10s; error code `TIMEOUT`; retry hint shown | Unit (fake timers) | P0 |
| AC-13 | **Short-sync** write not done in 15s | Enters the corresponding error level after 15s; error code `TIMEOUT`; retry hint shown | Unit (fake timers) | P0 |
| AC-14 | Read/write tiers correct | Read 10s, write 15s; the two tiers never mix (a read must report before 11s; a write must not report before 14s) | Unit (fake timers) | P0 |

> **`TIMEOUT` presented level (qa #72 aligned, doc §4.4 v1.15)**: `TIMEOUT`'s presented level is decided by the **trigger context** — short-sync (AC-12/13/14) = page-level (error-line highlight + retry reachable); long-turn transport timeout (AC-53) = flow-level (full-screen state). Assertions must not infer the level from the code alone.
| AC-15 | Retry after timeout | The timeout error state is retryable; a successful retry runs the state-machine reset (AC-02/AC-03); **a timed-out write retried doesn't persist twice** (action-level idempotency defense; drop is best-effort) | Unit + integration | P1 |
| AC-53 | Long-turn failure escalation | Long agent turns (streaming + multi-round tools) **don't apply 10s/15s**; when a transport timeout/interruption fails the turn, the error is **flow-level** (not a local hint), with a "retry or return" path | Unit + component | P0 |
| AC-54 | Timeout cancellation mechanism | The feedback-layer timeout **drops the future** at the deadline (underlying request cancelled, verifiable via hook counting / no further network activity); the sequence-number check is only a fallback for late non-timeout responses (AC-05) | Unit (test hooks) | P0 |

---

## D. Toast (design doc §2)

| ID | Trigger | Expected (quantifiable) | Assertion method | Priority |
|---|---|---|---|---|
| AC-16 | Toast appears | Auto-dismisses after 3s (±500ms) | Unit (precise fake-timer assertions) | P0 |
| AC-17 | User manually | Manually closable (the close action is key-triggerable) | Component | P0 |
| AC-18 | Hover over toast | **Pauses** the timer; after leaving, resumes with the **remaining time** (not reset to 3s) | Unit (fake timers: advance 1s → hover → advance 5s → fail if already gone) | P0 |
| AC-19 | Keyboard focus on toast | Same as hover: pauses and resumes with the remaining time; resumes after blur | Unit + component | P0 |
| AC-20 | A 3rd toast triggers | At most 2 at once; **oldest evicted only when full**; when not full, the new toast queues | Unit (fake timers) + component | P0 |
| AC-21 | Same-kind repeated trigger (rapid "Copied" clicks) | **Replaced by a single toast with the timer reset**, never stacked (same-toast identity always ≤1) | Unit + component | P0 |
| AC-22 | Toast accessibility | **Web-side convention**: container `aria-live="polite"`; when it has an action entry, `role="status"` (not `role="alert"`). **bingo TUI side**: toast shows in the status area, content update perceivable (mapping: status-area content update, not row deletion) | Component + SR audit (web only) | P0 |
| AC-23 | Copy | One-sentence result + action entry when needed ("Copied ✓ / Undo") | SR audit / manual | P1 |

---

## E. Three error levels + mixed state (design doc §3)

| ID | Trigger | Expected (quantifiable) | Assertion method | Priority |
|---|---|---|---|---|
| AC-24 | Field-level validation failure | Error line inline below the input area: icon + specific reason; **highlight marks only the erroneous input line**; error line and the associated input line visible together (mapping `aria-invalid`+`aria-describedby`); cursor/highlight positioned on the corresponding input line | Component | P0 |
| AC-25 | Page-level failure | Error card/placeholder + retry item; error line highlighted + scrolled into view; retry item reachable and selected (mapping focus on the retry button) | Component | P0 |
| AC-26 | Flow-level failure | Full-screen error state + a way back (never a dead end); focus lands on the primary action item | Component | P0 |
| AC-27 | Batch partial failure | Mixed state: "Succeeded n/m, k failed" + a list of failed items; failed items **individually retryable**; the failed-items list scrollable into view/navigable | Unit + component | P0 |
| AC-28 | Any error copy | Must contain "what happened + what the user can do"; dead-end copy like "operation failed" forbidden | Manual / test-hook sampling | P0 |
| AC-29 | Error code → user action mapping | `AUTH_REQUIRED→log in again/configure key`, `PERMISSION_DENIED→go back/request`, `SERVER_ERROR→retry later`, `OFFLINE→check network`; the same error code gives a consistent UI action | Component (inject each error code) | P0 |

---

## F. CLI structured error contract (design doc §4.1/4.3 + §7)

| ID | Trigger | Expected (quantifiable) | Assertion method | Priority |
|---|---|---|---|---|
| AC-30 | Any error under non-TTY | stderr outputs a single line `[error] code=<SCREAMING_SNAKE> msg=<single line>`, greppable | Integration (spawn CLI, non-TTY) | P0 |
| AC-31 | msg contains newlines/tabs | Normalized to spaces; the single line isn't broken | Integration | P0 |
| AC-32 | msg too long | Truncated to 200 chars (length ≤200) | Integration | P0 |
| AC-33 | Multi-line stack | Carried in `detail=<JSON-escaped>`, **only output with `--verbose`**; primary msg stays single-line | Integration | P0 |
| AC-34 | Assertion stability | Same error: changing msg copy → tests don't break; changing the code → tests must break | Integration (equivalence classes) | P0 |
| AC-35 | Naming convention | All codes match `^[A-Z][A-Z0-9_]*$` (SCREAMING_SNAKE) | Integration + unit (code-table scan) | P0 |
| AC-36 | Dual-exit consistency | GUI and CLI produce the **same code** for the same underlying error (shared `map_error`); cross-side comparison assertions | Unit (construct each representative error → call map_error on both sides and compare) + integration | P0 |
| AC-37 | TTY/non-TTY information parity | The same operation loses no feedback information in either environment (only presentation differs: spinner vs log line) | Integration + manual | P1 |
| AC-52 | TUI error line structured | TUI error lines render from the structured `UiEvent::Error { code, msg }` (dev: the current `UiEvent::Error(String)` needs only 3 call-site changes), **not `to_string()` concatenation**; the TUI render layer naturally carries stable codes, assertable | Unit (construct UiEvent, assert render input) + component | P0 |

---

## G. Error-code infrastructure (design doc §4.3)

> Note: AC-38/40/43 follow the **corrected v1.6/v1.7 stance** (exhaustive match without `_` arm + unit tests enumerating every variant of every module + **explicit GENERIC paths, no automatic fallback**), aligned with devex P1 and dev items 24/26's finding of "§4.3/guardrail 2 stale wording residue" — when implementing, **don't refer to stale wording like "unregistered falls back" or "representative error variants"**.

| ID | Trigger | Expected (quantifiable) | Assertion method | Priority |
|---|---|---|---|---|
| AC-38 | `ErrorCode` trait implementation | Each module's match **exhaustively covers every variant with no `_` arm** — an unhandled new variant **fails to compile** | Compile-time (cargo build) + unit | P0 |
| AC-39 | Explicit `GENERIC` | A loud `eprintln!` warning (with the missing_code marker) in debug builds, **assertable**; release semantics known (no stable code assigned yet) | Unit (debug assertion) + integration | P0 |
| AC-40 | Drift-guard unit tests | **Enumerate every variant of every module's error enum**, asserting a mapping to a **non-`GENERIC`** stable code | Unit (full variant enumeration) | P0 |
| AC-41 | GENERIC allowlist | `const GENERIC_ALLOWLIST: &[&str]`: variants not in the list are all non-`GENERIC`; entries are locatable paths (e.g. `"tool::bash::Error::NonZeroExit"`); each carries a `TODO(generic-allow): <issue>/<date> <reason>` comment | Unit + review | P0 |
| AC-42 | Code-value lifecycle | Code values **only grow: never modified, never reused** (semver): meaning frozen once published; single-file append-only code table, visible to review | Unit (code-table uniqueness) + review | P0 |
| AC-43 | Explicit GENERIC path | Variants without a stable code yet **explicitly return `GENERIC`** (a published stable code, not a temporary value); **there is no "unregistered auto-lands on GENERIC"** (exhaustive match, no `_` arm; v1.6+ stance, confirmed by dev item 26) | Unit | P0 |
| AC-44 | Contract file | The contract is centralized in `src/error.rs`: `ErrorCode` trait / `GENERIC` + debug warning macro / shared `map_error` exit function / drift-guard unit tests; **single exit + single code table** | Structural review + unit | P0 |
| AC-45 | Scenario → code consistency | Implementation matches the "scenario → error code" sample table: timeout→`TIMEOUT`, login expired→`AUTH_REQUIRED`, no permission→`PERMISSION_DENIED`, server→`SERVER_ERROR`, no network→`OFFLINE`, invalid config→`CONFIG_INVALID` | Unit + integration | P0 |

---

## H. Accessibility (design doc §5 + §7; bingo TUI follows the "TUI mapping baseline")

| ID | Trigger | Expected (quantifiable) | Assertion method | Priority |
|---|---|---|---|---|
| AC-46 | Screen-reader reading | **Web-side convention**: toast (`aria-live`) and error (`role="alert"`) readable by SR. **bingo TUI side**: status-area error/toast content readable (mapping: status-area content update, not row deletion); if SR support is limited, degrade to manual confirmation | SR audit (web only) + manual | P1 |
| AC-47 | reduced-motion | **Web-side convention**: under `prefers-reduced-motion`, motion disabled, loading indicator kept. **bingo TUI side**: no such concept in TUI — spinner animation rate may simply degrade; **the loading indicator itself is kept** (slow-load perception is a state, not decoration) | Manual (TUI) / component (Web) | P0 |
| AC-48 | Error-code advanced details | Collapsible region hidden by default; **TUI side**: key toggles expand/collapse, expandable state toggleable and perceivable (mapping `aria-expanded`) | Component | P1 |
| AC-49 | Loading indicator | **Web side**: `aria-busy="true"` + disabled; spinner `<span role="status">`. **bingo TUI side**: `chat.busy` + status-line spinner indicator | Component | P0 |

---

## I. Testability infrastructure (design doc §6)

| ID | Trigger | Expected (quantifiable) | Assertion method | Priority |
|---|---|---|---|---|
| AC-50 | Test hooks | Components/commands expose injectable hooks: **injectable delay** (trigger loading/timeout), **injectable failure responses** (trigger each error level); each state stably reproducible | Structural review + smoke | P0 |
| AC-51 | Timing test strategy | 200ms/3s/10s/15s timings **all go through fake timers** (ms-level); E2E has no timing assertions (avoids flakiness) | Review (test inventory) | P0 |

---

## Shared parsing helper (test-side contract)

```rust
/// Parses a single-line error contract. Only parses `[error]` lines; `[progress]` etc. don't apply.
/// Assertions depend only on code; msg/detail are for display and debugging, never asserted.
pub struct ParsedError {
    pub code: String,      // SCREAMING_SNAKE, normative
    pub msg: Option<String>,     // single line ≤200, newlines normalized to spaces
    pub detail: Option<String>,  // JSON-escaped, only present with --verbose
}

/// Syntax: `[error] code=<CODE> msg=<single line>[ detail=<json>]`
/// msg is cut at ` detail=` (the normalized msg must not contain that sequence); without detail, take to end of line.
pub fn parse_error_line(line: &str) -> Option<ParsedError>;

/// Assertion helper: assert_code!(line, "TIMEOUT") etc. Never asserts msg text.
```

- The helper mirrors dev's drift-guard unit tests: the unit tests guarantee "every variant → stable code", the helper guarantees "CLI output line → assertable code".
- On the Rust side, timing tests use `tokio::time::pause/advance` (tokio already includes the time feature, zero new dependencies; confirmed by dev item 21); chat.rs's existing no-runtime pure-logic test pattern can be reused.
- Landing point: once `src/error.rs` is in place, the helper lands with the integration tests (test-side code, maintained by qa).

---

## Regression checklist (ordered by release gate)

P0 gate: AC-01/02/03/04/05/06 (state machine) → AC-07/10/11 (loading) → AC-12/13/14/53/54 (timeout + long turn + cancellation) → AC-16/18/19/20/21 (toast) → AC-24/27/29 (error states + mixed state + mapping) → AC-30/32/34/35/36/52 (CLI contract + UiEvent structured) → AC-38/39/40/41/42/43/44/45 (error-code infrastructure) → AC-47/49 (accessibility) → AC-50/51 (test infrastructure).

P1 follow-up: AC-15, AC-23, AC-28 (manual), AC-37, AC-46, AC-48.

## qa regression record (v1.3.1, 2026-08-07)

Verification baseline: `cargo build` / `cargo clippy --all-targets` zero warnings; `cargo test` **553 passed 0 failed**; two CLI error-code exits tested in practice.

**Verified passing (P0 mainline):**
- **AC-30/45**: non-TTY `[error] code=AUTH_REQUIRED msg=...` exit=1 (clean HOME without key); `[error] code=CONFIG_INVALID msg=...` exit=1 (bad settings.json) — assertion anchors on code ✅
- **AC-12/13/14**: `SHORT_READ_TIMEOUT=10s` / `SHORT_WRITE_TIMEOUT=15s` + constant assertion tests ✅
- **AC-31/32**: `sanitize_msg` normalization + 200-char truncation (including per-character Chinese) unit tests pass ✅
- **AC-35/38/44**: SCREAMING_SNAKE assertions, 10 modules' ErrorCode exhaustive match without `_` arm (compile-enforced), contract centralized in `src/error.rs` ✅
- **AC-36**: TUI via `map_error`, CLI via `error_code_boxed`, same code table source ✅
- **AC-41/43**: `GENERIC_ALLOWLIST` empty, all variants stable codes (zero explicit GENERIC paths) ✅
- **AC-52**: `UiEvent::Error { code, msg }` structured; TUI renders `[error] code=... msg=...` + `busy=false` reset ✅
- **AC-53/54**: long turns keep the transport layer 120s/60s; feedback-layer timeout drops the future at the deadline to cancel ✅ (AC-53's TUI presentation assertion awaits component-level coverage)

**Remediation items (need dev confirmation/handling):**
1. **[P1] AC-40 drift-coverage gap**: of `TeamError`'s 3 variants, **only `Invalid` is constructed in unit tests**; `Io`/`Parse` aren't enumerated — if they change to explicit GENERIC, the drift test won't catch it (the "representative error variants" pattern devex once flagged, still lingering). Fix: `error.rs` unit tests add `TeamError::Io(io::Error::other)` + `TeamError::Parse(serde_json err)` assertions. Full-variant coverage for the other modules confirmed ✅.
2. **[P1] `error_code_boxed` implicit GENERIC + downcast registry drift**: the CLI exit (main.rs:279) goes through the boxed path, which returns `GENERIC` directly at the tail — **no debug warning** (`missing_code` is dead code, never called), and `downcast_error_code!` is a hand-maintained registry; a new ErrorCode type missing registration → **silent GENERIC**. Fix suggestions: (a) `error_code_boxed` calls `missing_code` when landing on GENERIC (debug builds); (b) add a test "every ErrorCode type is reachable through `error_code_boxed` as boxed and non-GENERIC" (currently only QueryError + unknown io::Error are tested).
3. **[P2] AC-39 has no executable assertion**: `missing_code` currently has no calls/tests (no explicit GENERIC paths). Suggest a cfg(test) scenario asserting the debug warning is triggerable.
4. **[P2] AC-05 mechanism annotation**: the race protection is actually structural — **drop future + cancel channel** (query.rs `cancel_requested`/`aborted`), with no standalone sequence-number counter — stronger than sequence-number checks; suggest AC-05's note explicitly states "sequence-number checks not separately implemented; structural cancellation takes precedence".
5. **[Info] AC-33**: without `--verbose` there's no trigger point; dev v1.4 marked it "not applicable for now" — accepted.

**Regression conclusion**: P0 mainline **passed, conditionally released** — remediation 1/2 (P1) recommended this iteration (low cost, 2 tests); remediation 3/4 (P2) can follow with doc annotations; AC-15 retry idempotency, AC-53 long-turn failure TUI presentation, and AC-26 flow-level full-screen state are component-level regression, covered once the TUI component test infrastructure is in place.

### Re-verification record (v1.6.1, 2026-08-07, responding to devex's landing fix #48)

Re-verified after devex landed fixes for P1-P3 + the missing_code code (clippy zero warnings, cargo test 553 passed 0 failed):

- ✅ **Remediation 1 (AC-40 TeamError drift coverage)**: `config_and_storage_errors` now enumerates all 3 TeamError variants (Invalid/Io/Parse → `CONFIG_INVALID`, through `assert_stable_codes`).
- ✅ **Remediation 2a (missing_code code landed)**: the `error_code_boxed` GENERIC fallback branch calls `missing_code` to warn in debug builds (ui/ux v1.14 requirement + #47 scope); registry misses/unimplemented types are no longer silent.
- ⏳ **Remediation 2b (macro-registry coverage test)**: still awaiting dev's decision on the boxed technique's necessity — if confirmed necessary, add "10 types asserted non-GENERIC through the boxed path".
- ✅ **Remediation 3 (AC-39)**: `missing_code` is now called by the boxed GENERIC branch; in debug, the `boxed_error_walks_cause_chain` (unknown io::Error → GENERIC) path already exercises the warning (the eprintln output isn't asserted; a formal eprintln assertion can be added later).
- ✅ **Remediation 4 (AC-05 mechanism annotation)**: the AC-05 row is annotated (v1.6).
- ⏳ **P2 (boxed exit technical necessity)**: awaiting dev's decision.

### Re-verification record 2 (v1.7.1, 2026-08-07, responding to dev #49 landing fix)

Re-verified after dev's P2 decision + the macro-registry coverage test landed (`cargo test error::` 7 passed; full run 553 passed 0 failed; clippy zero warnings):

- ✅ **Remediation 2b (macro-registry coverage test)**: `boxed_export_covers_all_registered_modules` exists and passes — 10 types each boxed-asserted non-GENERIC + `samples.len()==10` dual registration comparison (one-to-one with the `downcast_error_code` macro list).
- ✅ **P2 (boxed technical necessity)**: dev formally decided "for boxed scenarios, `map_error`'s static generics can't cover the CLI's top-level `&dyn Error`; `error_code_boxed` + the macro registry are necessary"; "unify into a single entry point" was assessed as a purely formal refactor and not adopted per default-to-subtracting; guardrail 4's dual-exit stance needs no rollback.

**Re-verification checklist fully closed**: remediation 1/2a/2b/3/4 + P2 necessity all green; the regression conclusion stays "P0 mainline passed".

### Component-level regression record (v1.9.2, 2026-08-07, #14 TUI component-level regression)

**qa assertions landed (565 tests all passing + clippy zero warnings, 4 `qa_*` tests in chat.rs)**:
- **AC-15 (timeout retry idempotency)** ✅: asserted at the TUI layer (full-screen state Enter=retry reachable, state resets after retry) + client layer (timeout lands on TIMEOUT).
  **Server-side "no duplicate persistence" boundary (dev #99 / qa #98, main's option ① finalized)**: the short-sync write path = `complete_text` (pure generation for compact/memory, **no persistence side effects**), so retry-overwrite is harmless; **server-side idempotency depends on API idempotency capability (current LLM APIs have no idempotency headers), and client-side structural guarantees (drop future + cancel channel) are the only defense**; idempotency keys are unnecessary (no side-effecting write surface); when an idempotency-key API is adopted later, that's a "capability upgrade", not "filling a gap".
- **AC-26 (flow-level full-screen state)** ✅: full-screen state (title + code + explanation + action + cursor hidden) + Esc returns + Enter retries + focus on the primary action.
- **AC-53 (long-turn failure escalation)** ✅: FX-11 (TIMEOUT+LongTurn) → flow-level full-screen state, contrasted with FX-01 (same code, short-sync = page-level) — **same code, different level** (TIMEOUT's dual levels distinguished by context, verified in practice).
- **AC-29 (error code → action)** ✅: `qa_ac29` full 11-fixture per-code matrix (level explicitly carried by the producer + render form matches level).
- **Real path** ✅: `qa_fx01_real_path` (/model fetch timeout → page-level error line, **real production emitter** list_models, not a fixture one-leg).
- **Presentation-layer acceptance (ui/ux #20)**: FX-01…11 injection → render chain all passed (A1/A3/C2/D2/D3/F1/F2/F3/G1/G3); H-section collapse (AC-48 P1) + manual items pending.
- **DX re-review (devex #15)**: level surviving the event chain + emitter/reset/degrade-preservation verified, closed.
- **Short-operation emitters (main #91 option ①)**: list_models/count_tokens failures emit Page+ShortSync error lines, degradation behavior preserved, no Field-level addition — #14 full chain closed.

---

## Changelog

- v1.0 (2026-08-07): produced a 51-item assertion table + the parsing-helper contract against `feedback-states.md` v1.7; flagged the api/client.rs timeout inconsistency finding (AC-15 note).
- v1.1 (2026-08-07): revised per dev review item 21 and main's directive "assertions go by TUI behavior" — introduced the "TUI mapping baseline" table; ARIA/DOM/rAF/prefers-reduced-motion class assertions (AC-03/04/08/09/10/17/22/24/25/26/27/46/47/48/49) changed to observable TUI behavior; timing tests noted to use `tokio::time::pause/advance` (zero new dependencies); added AC-52 (`UiEvent::Error` structured); section G notes the corrected v1.6/v1.7 stance (aligned with devex P1).
- v1.2 (2026-08-07): aligned with dev re-review item 26 — AC-43 wording changed to "explicit `GENERIC` path" (removing the stale "unregistered auto-lands on GENERIC" semantics); the section G note extends to guardrail 2's stance.
- v1.3 (2026-08-07): synced timeout layering per dev item 31's decision — AC-12/13/14 scoped to "short sync operations"; added AC-53 (long turns don't apply 10s/15s; failures escalate to flow-level) and AC-54 (feedback timeout drops the future to cancel); AC-15 closed its original finding note and adds write-path idempotency defense; 54 items total. Header pins the corresponding doc **v1.11** (ui/ux item 34 already landed the timeout refinement ahead of time, not an implementation-period backfill).
- v1.4 (2026-08-07): dev implementation-period backfill — the "dev implementation cross-check" section adds the **AC-33 not applicable for now** note (no `--verbose` means no trigger point; add the `detail=` assertion when `--verbose` is implemented); §4.4 title cross-checked via ui/ux, changed to "scenario → error code table", consistent with the code-mapping/drift assertions, no behavior-stance change.
- v1.5 (2026-08-07): qa regression record section — verified 553 tests passing + two CLI code exits; P0 mainline conditionally released; 5 remediation items (P1: AC-40 TeamError drift-coverage gap, error_code_boxed implicit GENERIC/downcast registry drift; P2: AC-39 no executable assertion, AC-05 mechanism annotation; Info: AC-33).
- v1.6 (2026-08-07): AC-05 row mechanism note landed (drop future + cancel channel, sequence-number checks not separately implemented, confirmed by ui/ux v1.14); section G's AC-39/43 await devex's P1-P3 landing fixes, then sync the missing_code warning stance (including the boxed-exit macro-registry miss branch).
- v1.7 (2026-08-07): re-verification record (v1.6.1) — after devex's landing fixes: TeamError 3-variant assertions ✅, missing_code boxed-branch code landed ✅, empty test deleted ✅, clippy/test all green ✅; backlog narrowed to 2 items (macro-registry coverage test, boxed-exit necessity decision; both awaiting dev).
- v1.8 (2026-08-07): re-verification record 2 (v1.7.1) — after dev #49 landed the macro-registry coverage test and decided the boxed necessity, **the re-verification checklist is fully closed** (remediation 1/2a/2b/3/4 + P2 all green); the regression conclusion stays "P0 mainline passed".
- v1.9 (2026-08-07): qa #69 + main's contract alignment — AC-29's `AUTH_EXPIRED` becomes **`AUTH_REQUIRED`** (aligned with the implementation and doc §4.4's single source; no new code); header version fixed (previously v1.3 lagging the actual v1.8) and corresponding doc version (v1.15); added the **`TIMEOUT` presented-level note** after AC-12/13/14 (decided by trigger context: short-sync = page-level / long turn = flow-level, qa #72).
- v1.9.1 (2026-08-07): ui/ux added section C's **explicit TIMEOUT-level stance** per qa #72 — "`TIMEOUT`'s presented level is decided by the trigger context: short-sync = page-level (AC-12/13/14), long turn = flow-level (AC-53)" pinned at the top of the timeout-layering block, so qa/presentation don't assert `TIMEOUT`'s level against each other (qa #76 verified both places consistent in practice ✅).
- v1.9.2 (2026-08-07): qa backfilled the **#14 component-level regression record** — AC-15/26/53/29 assertions landed (4 `qa_*` tests, 565 all passing); AC-15 server-side idempotency boundary finalized (main's option ①: short-sync writes = pure generation with no side effects, structural guarantees are the only defense, idempotency keys unnecessary; dev #99 boundary note).
- v1.9.3 (2026-08-08): AC-26/53 regression moved to the real alternate-screen assembly seam (`fullscreen_frame`), preventing `run_fullscreen` from bypassing the Full error screen; assertion verifies title/stable code, no prompt border, and no caret. Added idle host/state coverage for slash-error TTL: `slash_error_at` keeps ticking while otherwise idle, expires after the 8s floor, then stops ticking.
- v1.10 (2026-08-11): issue #37 registers `CONTEXT_OVERFLOW` for a provider-rejected context window after the single compact retry; recovery tests cover Anthropic and OpenAI fixtures plus terminal repeated-overflow and compaction-failure breaker increments.
