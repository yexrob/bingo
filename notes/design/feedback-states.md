# Feedback States Specification

> Version: v1.18 · Status: in effect (2026-08-07)
> Scope: the unified design conventions for every user-visible feedback state in bingo. The GUI (TUI) and CLI (headless) sides share a single source; qa acceptance anchors are at the end.

## General principles

1. **Feedback states must not depend on the environment**: the same operation must produce perceivable feedback in the TUI, in a pipe, in CI, and on a weak-network narrow screen, and feedback information must not live only in the interactive UI (see the CLI-side conventions).
2. **State machine design, not one-shot events**: every async operation goes through `idle → loading → success / error → idle`; it must fully reset, and getting stuck in an intermediate state is forbidden.
3. **Error copy = what happened + what the user can do**: dead-end copy like "operation failed" alone is forbidden.
4. **No motion for motion's sake**: feedback animation is only used to draw attention or bridge state changes; timing and amplitude follow each section's conventions.
5. **Feedback is granular per action, not per control**: duplicate prevention, loading, timeout, and reset all hang off the submit action rather than only off the button click (see the "duplicate prevention" note in the Loading section and 3.1 in the error section).

## State machine

```text
idle ── action ──▶ loading ── success ──▶ success ──┐
                      │                            │
                      ├── timeout ──▶ error ───────┴──▶ idle (must reset)
                      └── failure ──▶ error
```

Four reset actions (qa can assert each one; see "Acceptance anchors"):

1. Clear the `aria-busy` / loading state
2. Remove error styling and `aria-invalid` (no leftover red outline)
3. Remove the error message from the `aria-live` region (by **writing an empty string / updating the copy**, not by deleting the node — most browsers don't announce deleted nodes)
4. Focus transfer: on success → next operation; on failure → focus the error element (field-level) or the retry button (page-level); **the focus must be set asynchronously after the error element finishes rendering** (async focus after render, e.g. requestAnimationFrame or a component effect; implementation-specific), otherwise the focus fails

**Stale-response race**: after a retry/new request is issued, late failure/success responses from the old request must be ignored (abort or sequence-number check); the new success must not be flashed over by an old error. Test this together with the reset.

## 1. Loading (in-progress state)

| Item | Convention |
|---|---|
| Trigger threshold | Appears when an async operation hasn't completed within >200ms |
| Form | Local operations: inline spinner in the button, replacing the button icon slot in place; page-level: skeleton screen in the content area |
| Forbidden | Fullscreen overlay blocking; silent waiting with no feedback |
| Duplicate prevention | De-dup at the **submit action** granularity: a disabled button blocks the mouse but not the Enter key / form onSubmit — the submit action granularity intercepts uniformly; the same action is not submitted twice during loading (idempotency guarantee) |
| Copy | Local buttons don't change copy (keep "Submit" → spinner in place); page-level shows "Loading…" |
| Timeout | Tiered timeouts: **10s for reads, 15s for writes, applied to short synchronous operations** (list_models / count_tokens / complete_text etc.); on timeout, enter the corresponding error level with a retry hint, **primary action = retry**; **long agent turns (streaming + multi-round tools) do not apply 10s/15s** — continuous progress feedback already exists within a turn (status line + activity rows + `chat.busy`), so timeouts are backed by the transport layer (120s/60s) + user interruption; **if a long turn fails (transport timeout/interruption), it escalates to a flow-level error** (retry or return, never a silent local hint); **the timeout timer must be cancelled on success/failure/cancel** — otherwise a late error arriving after success will overwrite the success state. Cancellation mechanism: at the deadline the feedback layer **drops the future** (wrapped in tokio `timeout()`; the reqwest connection is cancelled with it); the sequence-number check is only a defensive fallback |

## 2. Toast (light notification)

| Item | Convention |
|---|---|
| Trigger | When the operation result needs no user decision (saved, copied) |
| Duration | Auto-dismiss after 3s by default; **hovering or keyboard-focusing a toast pauses the timer, and leaving/unfocusing resumes with the remaining time** (not reset); manually closable |
| Stacking | No more than 2 at once; **only when the slots are full is the oldest evicted** (if A has 0.5s left and B arrives while there's room, B queues until A leaves) |
| De-duplication | Repeated triggers of the same kind (5 rapid clicks of "Copied") → **replaced by a single toast with the timer reset**, never stacked |
| Copy | One sentence stating the result + an action entry when needed (e.g. "Copied ✓ / Undo") |
| Accessibility | Container `aria-live="polite"`; readable by screen readers |

## 3. Error states (3 escalating levels + mixed state)

| Level | Scenario | Form | Focus |
|---|---|---|---|
| Field-level | Input validation failure | Inline below the input: icon + specific reason ("Email format is invalid"); red outline marks only the erroneous field | Focus the corresponding input |
| Page-level | Single request failure, dependency unavailable | Error card/placeholder + retry button | Focus the retry button |
| Flow-level | API down, insufficient permissions, session failure | Full-page state + a way back (never a dead end) | Focus the primary action |
| **Mixed state** | Batch operation partially succeeded | "Succeeded 2/3, 1 failed" + a list of failed items, each individually retryable | Focus the failed-items list |

### 3.1 Error code → user action mapping (mandatory)

Besides "how to report", the error-code contract must provide a **user action**: the UI action for a given error code must be consistent.

| Error code (example) | Meaning | User action | Error level |
|---|---|---|---|
| `AUTH_REQUIRED` / 401 | Login expired / key missing / invalid key | Log in again / configure the key | Flow-level |
| `PERMISSION_DENIED` / 403 | No permission | Go back / request permission | Page/flow-level |
| `SERVER_ERROR` / 500 | Server-side error | Retry later | Page-level |
| `OFFLINE` | No network | Check the network and retry | Page-level |

The mapping table uses the thiserror error types as its single source of truth; a new error code must register its mapping at the same time.

**Error level = typical slot; context can override**: the "error level" column gives the code's **typical level**; the actual presented level is **explicitly carried by the producer based on the trigger context** (not derived by the render layer, not a duplicated mapping in tests) — e.g. `TIMEOUT` short-sync = page-level / long turn = flow-level; when `PERMISSION_DENIED`'s multi-slot codes take the flow-level slot, it renders full-page. The presentation layer and assertions go by the `level` carried on the event.

General:
- Error copy must state "what happened + what the user can do"; the copy may change, but the carried information must not.
- Error codes are presented in the UI as collapsible "advanced details": ordinary users see only the human-readable copy by default; expanding reveals `code=...`.
- After a field-level error is focused, a successful retry/fix must return to idle (see the state-machine reset).

## 4. CLI-side conventions (headless / pipe / CI)

### 4.1 Structured error protocol

- Errors are defined with stable codes/categories (e.g. `CONFIG_INVALID`, `PERMISSION_DENIED`); the thiserror error types in code are the single source of truth; the render layer only consumes, never re-implements.
- UI rendering, CLI output, logs, and qa assertions share the same error definition:
  - Assertions rely on the error code, not string matching of copy; copy changes don't break tests.
- **Stable output contract** (this exact format for all non-TTY output, greppable):

```text
[error] code=CONFIG_INVALID msg=config file could not be parsed: line 3
```

  `key=value` on a single line; `code` is the stable error code, `msg` is the human-readable copy (changeable). qa assertions depend only on `code`.
- **msg escaping conventions** (to protect key=value parsing; stable for qa/log grep):
  - Newlines/tabs normalized to spaces;
  - The primary `msg` is truncated to 200 chars (truncation beyond that is expected);
  - Multi-line stacks go in a separate `detail=` field; the primary `msg` stays single-line.
- **Error code naming convention**: always `SCREAMING_SNAKE` (e.g. `CONFIG_INVALID`); new codes register accordingly.

### 4.2 TTY / non-TTY degradation

- Interactive TTY: spinner / progress bar allowed.
- Pipe / CI / scripted calls: no spinner; progress and errors must land on **greppable single-line logs** (`[error] code=...`, `[progress] ...`), never visible only in the interactive UI.
- The same operation gives equivalent feedback in both environments (no information loss, only different presentation).

### 4.3 Error code implementation path (decided: C, exit mapping; final call by dev)

Current state: bingo's thiserror errors are defined scattered across modules (src/api/client.rs, settings.rs, tasks.rs, mcp.rs — 10+ places), the Display copy is the only error information, and there is no unified error-code layer yet. Three implementation paths were considered (C selected):

- **A. Top-level AppError aggregation**: a unified enum absorbs every module's errors, code table centralized — clearest contract, largest change surface.
- **B. Module-level `code()`**: existing enums expose ErrorCode + a centralized code table — keeps module isolation, small change, requires maintaining a cross-module table.
- **C. Exit mapping (selected)**: low-level errors stay as-is; only the CLI/UI exits do one "error → stable code" mapping — minimal intrusion, aligns with AGENTS.md's "default to subtracting"; can evolve to A once requirements mature.

**C's landing guardrails** (devex gates + qa owns acceptance; the contract drifts if any is missing):

1. **Downcast fragility guard**: downcasts based on **matching by type name / string** are forbidden (renames in refactors silently break). C lands as = **every module error implements a lightweight `ErrorCode` trait (returning a `&'static str` code); exits only call it, never branch on types** — compile-time exhaustive, discoverable, not fragile (B's interface + C's intrusion surface). Boxed exits use **`downcast_ref::<$t>()` compile-time type references** (not name matching; refactors fail at compile time), which is an allowed form.
2. **Registration is the contract**: hard rule — a new error path must **explicitly return a stable code for every variant**; variants not yet assigned a stable code **explicitly return `GENERIC`** (debug builds warn; **`_` implicit fallback is forbidden**). `GENERIC` is a **published stable code** (not a temporary value); qa assertions explicitly cover "explicit `GENERIC` path → lands on `GENERIC`". The code table lives in one file, the single lookup point for dev/qa.
3. **Codes only grow: never modified, never reused (semver-style)**: once an error code is published, its meaning can't change and its number can't be reused, or historical logs and existing assertions all break. Single-file code table + append-only additions make review obvious at a glance; the file header comment states the semver rule.
4. **Single exit function + single code-table file** (structural consistency): the mapping logic's single source = each module's `ErrorCode` impl; **the TUI exit goes through `map_error`, the CLI boxed exit through `error_code_boxed` + a macro registry** — both functions only consume ErrorCode impls and don't implement mapping themselves; consistency is guaranteed this way, with dual-exit comparison assertions kept as a fallback. **The macro registry is "registration is the contract" site #2**: types newly implementing ErrorCode must be registered, otherwise the CLI exit silently lands on `GENERIC` (the TUI exit stays correct) — a test asserting that the macro registry covers every ErrorCode-implementing type is required. The code table is centralized in the same file.
5. **Drift-guard unit tests**: **enumerate every variant of every module's error enum**, asserting a mapping to a **non-`GENERIC`** stable code — upgraded from "prevent missed registration" to "CI blocks temporary GENERIC misses"; copy changes don't break, code deletions do. The unit test itself must be maintained as variants are added; accept this cost (it's the CI-period fallback beyond compile time).
   - **GENERIC allowlist** (qa suggestion + dev finalized): `const GENERIC_ALLOWLIST: &[&str]` in `src/error.rs`, entries using locatable paths (e.g. `"tool::bash::Error::NonZeroExit"`); the unit test asserts "any variant not in the allowlist is non-GENERIC".
   - **Temporary annotation**: every allowlist entry must carry a `TODO(generic-allow): <issue>/<date> <reason>` comment, preventing permanent exemption.
   - **Review convention**: new allowlist entries must have a reason (why no stable code is registered yet); entries without a reason are not allowed.

**Landing decision (dev)**: the contract is centralized in **`src/error.rs`**, a single file containing three things:
1. The `ErrorCode` trait: `fn error_code(&self) -> &'static str` (SCREAMING_SNAKE)
2. Explicit `GENERIC` return + debug warning + the shared `map_error` exit function
3. Drift-guard unit tests (enumerate **every variant** of every module's error enum; non-allowlist variants must be non-`GENERIC`; including "explicit `GENERIC` path assertions" and dual-exit comparison, mirroring the qa AC table)

**Implementation integration points (dev confirmed against the code; nothing new invented)**:
- `src/ui.rs` is the renderer-agnostic contract layer (TUI/GUI/test harness share `UiEvent`) — the natural mount point for the "single `map_error` exit" already exists.
- `UiEvent::Error` is currently a plain string, used in only 3 places project-wide (chat.rs 1439 consumes; 2748/2789 produce `e.to_string()`) — upgrading it to `UiEvent::Error { code, msg }` is a minimal change surface and the ideal entry point for the error-code contract; TUI error rows then naturally carry stable codes, and qa can assert them.
- The TUI state-machine reset already has a prototype: `chat.busy` is de facto a turn state machine, and chat.rs already has many pure-logic busy-state tests (no runtime); qa assertions reuse that pattern.

**ErrorCode trait implementation model (compile-time enforcement + explicit GENERIC, dev's corrected version)**:
- Each module implements the trait on its own enum; the match **exhaustively covers every variant with no `_` arm** and returns a stable code — a new variant left unhandled **fails at compile time**, so "compile-time exhaustive" holds.
- Variants "not yet registered" are given an **explicit `GENERIC` return by the developer** (explicit behavior, not silently falling into `_`); explicit `GENERIC` warns via `eprintln!` in debug builds, reminding to register.
- **In release, explicit `GENERIC` semantics are known**: it means "this path has no stable code assigned yet" (error semantics degraded to generic) — not an accidental loss; a known trade-off, recorded honestly, with no promise that compile time catches it.
- **Current explicit `GENERIC` paths = 0**: the `missing_code` warning mechanism is dormant; when future explicit `GENERIC` returns are added (including the **boxed-exit macro-registry miss that lands on `GENERIC`** branch), **`missing_code` must be called to warn** — preventing silence: a loud `eprintln!` warning in debug, known semantics in release; the dormancy can't be documented only and leave no code-side warning tail.
- Exits only call `error_code()`, never branch on concrete types.

This spec constrains only the **exit contract** (CLI/UI output stable error codes); the internal path is decided as C; exit output is consistent. TTY detection uses `std::io::IsTerminal` (zero new dependencies).

### 4.4 Scenario → error code table

dev implementation and qa assertions both reference this table (**registration is the contract**; after implementation, every variant is explicitly assigned a
stable code and `GENERIC_ALLOWLIST` is empty):

| Scenario | Error code | User action |
|---|---|---|
| Read/write timeout (short sync ops); long-turn transport timeout | `TIMEOUT` | Retry later (short-sync = page-level; long turn = flow-level, AC-53) |
| Login expired / key missing / invalid key / 401 | `AUTH_REQUIRED` | Log in again / configure the key |
| No permission / 403 | `PERMISSION_DENIED` | Go back / request permission |
| Rate limited / 429 | `RATE_LIMITED` | Retry later |
| Server error / stream protocol / MCP connection failure | `SERVER_ERROR` | Retry later |
| No network (transport-layer error) | `OFFLINE` | Check the network and retry |
| Invalid config (settings / team.json read, write, or validation) | `CONFIG_INVALID` | Fix the config and retry |
| Local storage read/write failure (tasks / transcript / experience) | `STORAGE_ERROR` | Check disk / permissions and retry |
| Tool execution failure | `TOOL_FAILED` | Check the tool output and retry |
| Hook execution failure | `HOOK_FAILED` | Check the hook config |
| The turn's task ended without reporting an outcome (panic inside the spawn) | `TURN_LOST` | Retry the turn or go back (flow-level) |
| Explicit `GENERIC` path | `GENERIC` (no stable code assigned yet) | Follow the copy's guidance |

Code-value semver: once published, only grow, never modify or reuse; a new code = new variant mapping + an added assertion in
the `src/error.rs` drift-guard unit tests (missing either turns CI red).

**Short-sync operation failure = degrade visibly, never swallow silently (policy after #18 lands)**:
- When a short sync operation (`list_models` / `count_tokens`) fails, the TUI emits a page-level `UiEvent::Error` (`level: Page, context: ShortSync`) → an error-colored error line at the end of the content area (the user perceives "what happened + what they can do").
- **Behavior degradation is preserved**: the /model menu still shows known models (usable when the list is empty), /status still shows budget 0 — the error line is a hint; it doesn't block interaction or escalate to a full-screen state.
- Comparison: silently swallowing errors (`unwrap_or_default()` / `unwrap_or(0)` with no perception) violates general principle 1 "feedback states must not depend on the environment"; corrected to "degrade + stay visible".

**Empty long-turn recovery = retry once, never complete silently**:
- A completed upstream response with no committed assistant content and no tool calls is an abnormal long-turn result, including a stream that ends with an unclosed thinking/text/tool block. It must not be treated as an ordinary TurnEnd.
- Because this attempt has no committed assistant output or tool side effects, bingo retries it automatically once. The malformed empty assistant is not persisted or fed back to the model.
- If the retry succeeds, the TUI may show a non-fatal warning that the empty response was retried; headless mode emits equivalent stderr feedback. If the retry is also empty, the turn escalates through the existing full-flow long-turn error path with copy stating what happened and that the user can retry or go back.

## 5. DOM / style / ARIA conventions

> This section is the web-frontend (DOM/ARIA) stance; **bingo's current stack is ratatui TUI + headless CLI — no DOM/aria/CSS animation/prefers-reduced-motion/rAF**. The normative values (states, timing, reset) are unchanged; the TUI side implements them per the mapping below.

| Norm item (Web) | bingo TUI mapping |
|---|---|
| `aria-busy` / loading | `chat.busy` + status line (exists) |
| `aria-invalid` red outline | error-line highlight styling |
| `aria-live` write-empty-string update | status-area content update (not row deletion) |
| Focus transfer (async focus after render) | after the error line renders, scroll it into view + highlight |
| `prefers-reduced-motion` | no such concept in TUI: spinner animation rate may degrade; the indicator itself is never removed |
| requestAnimationFrame | TUI's frame loop is naturally post-render; not an issue |
| Animation durations (150ms/120ms/100ms) | not applicable to TUI; expressed in frames (e.g. 1-2 frame fade-in), no exaggerated displacement |

**TUI-side supplement (landed with #18)**: error **level/context is explicitly carried by the producer** (`UiEvent::Error { code, msg, level, context }`; chat.rs knows the trigger path when emitting), the render layer only consumes and never derives — level is not an inherent property of the code (`TIMEOUT` dual-slot, `PERMISSION_DENIED` dual-slot); the render layer and tests share the same event contract; duplicating a "code → level" mapping in the render layer or test side is forbidden.

Web-side conventions (for a future web frontend to reuse):

- **Loading button**: `aria-busy="true"` + disabled; spinner via `<span role="status">`.
- **Toast**: container `aria-live="polite"`; when it contains an action entry, use `role="status"`, not `role="alert"` (non-blocking notification).
- **Field-level error**: error copy attached via `role="alert"` or `aria-describedby` associated with the input; input `aria-invalid="true"`.
- **Page/flow-level error**: `role="alert"`; the retry button reachable and focused.
- **Error code advanced details**: collapsible region hidden by default, expand button semantic (`aria-expanded`).
- **Focus timing**: reset item 4's focus must happen after the error element finishes rendering (async focus after render); skip rather than block on failure.
- **Motion**: spinner loop animation; toast enters with fade-in + slight upward shift (150ms, cubic-bezier ease-out), exits with fade-out (120ms); error blocks complete within 100ms of appearing, no exaggerated displacement. All durations short and disableable via `prefers-reduced-motion`.
- **Reduced-motion boundary**: `prefers-reduced-motion` only disables motion (skeleton shimmer may be disabled); **the loading indicator itself must not be removed** — slow-load perception is a state, not decoration.

## 6. Testability conventions

- **Timing assertions**: timings like 200ms / 3s use **fake timers / virtual time** (ms-level determinism); E2E keeps only smoke tests, never timing assertions (avoids flakiness). On the Rust side use `tokio::time::pause/advance` (tokio already includes the time feature; zero new dependencies); on the Web side use component-level fake timers.
- **Test hooks**: components/commands must expose injectable hooks — injectable delay (to trigger the loading state), injectable failure responses (to trigger each error level) — so each state is stably reproducible.
- Assertions always go by the error code (`[error] code=...`), never by matching copy.

## 7. Acceptance anchors for qa

- **Loading**: 200ms threshold triggers/hides correctly; local button in-place spinner; fullscreen overlay forbidden; **action-granularity de-dup (including Enter/form onSubmit)**.
- **Toast**: 3s auto-dismiss, closable; hover/keyboard-focus pauses and resumes with the remaining time; max 2, oldest evicted only when full; same-kind de-dup replaces + resets the timer.
- **Error states**: three levels + mixed state each trigger correctly; 401/403/500/offline map to the correct actions; field-level focuses the corresponding input; copy contains "what happened + what the user can do"; state resets after retry.
- **Timeout**: read 10s / write 15s (short sync ops) time out into the corresponding error level; long turns go through the transport layer (120s/60s) + user interruption, escalating to flow-level on failure; **a late error arriving after success is cancelled**; the timeout timer is cancelled on success/failure.
- **Empty long turn**: an upstream completion with no committed assistant content and no tool calls is never accepted silently; an unclosed in-flight block marks the attempt malformed; bingo retries once without recording that empty attempt, then either completes normally or exposes a full-flow retry/back error.
- **State reset**: all four reset actions asserted one by one (aria-busy, aria-invalid, aria-live content, focus transfer); focus happens after rendering completes; stale-response races are ignored.
- **Structured errors**: non-TTY output follows the single-line `[error] code=... msg=...` contract; assertions use error codes, not copy.
- **Accessibility**: toast `aria-live` / error `role="alert"` readable by screen readers; under `prefers-reduced-motion`, motion is disabled but the loading indicator remains.

## Changelog

- v0.1 (2026-08-07): draft, covering the three error tiers (Loading/Toast/error states) and acceptance anchors.
- v1.0 (2026-08-07): merged devex's three additions — structured error protocol (4.1), TTY/non-TTY degradation (4.2), four state-machine resets (state-machine section); plus GUI-side feedback: error codes presented collapsible, and the general principle that feedback states must not depend on the environment.
- v1.1 (2026-08-07): merged main's two implementation-side additions — async focus after render (state-machine reset #4 / §5 "focus timing"), and the CLI error-code contract format `[error] code=... msg=...` (4.1); merged qa's six boundary categories — tiered timeouts with timer cancellation (Loading §"timeout"), error-code → user-action mapping (3.1), mixed state (3), action-granularity duplicate prevention (Loading §"duplicate prevention"), stale-response race (state-machine section), Toast quantification (2); testability conventions (6); under reduced-motion the loading indicator is retained, aria-live writes an empty string instead of deleting the node.
- v1.2 (2026-08-07): merged devex's msg escaping conventions (newlines normalized to spaces, primary msg truncated to 200 chars, multi-line stacks go to `detail=`, see 4.1); merged dev's current-state findings — error-code implementation path A/B/C pending main's ruling (4.3); this spec constrains only the exit contract and does not depend on the internal path.
- v1.3 (2026-08-07): merged devex's three C-path guardrails — downcast prevention (module errors implement the `ErrorCode` trait returning `&'static str`; exits only call it, never branch on types), registration is the contract (unregistered falls to `GENERIC` + debug-build warning), drift-guard unit tests (4.3); error-code naming convention `SCREAMING_SNAKE` (4.1); added the §4.4 "scenario → error code" example table for dev/qa reference.
- v1.4 (2026-08-07): merged qa/devex's three additions — the fallback code is a published stable code `GENERIC` and the unregistered-fallback path is assertable (guardrail 2); code values only grow, never modified/reused — semver rule (guardrail 3); dual-exit consistency enforced structurally: GUI/CLI share the same `map_error` function, a single code-table file, assertions as fallback (guardrail 4).
- v1.5 (2026-08-07): dev's ruling — path C exit mapping (4.3); the contract centralized in the single file `src/error.rs` (ErrorCode trait / GENERIC fallback + debug warning macro + map_error / drift-guard tests); ErrorCode trait implementation model: each module enum has an exhaustive match returning stable codes (unhandled new variants fail compilation) + a tail `_ => missing_code + GENERIC` debug warning.
- v1.6 (2026-08-07): qa clarified that "exhaustive match" and "a `_` fallback arm" contradict; dev corrected the implementation model — **drop the `_` fallback arm, compile-enforce true exhaustive match** (unhandled new variants fail compilation); `GENERIC` becomes **explicit return** (explicit behavior + debug warning), with the release-mode semantics honestly recorded as "no stable code assigned yet"; drift-guard tests change to **enumerating every variant of every module, asserting non-GENERIC** (blocks missed registration during the CI period).
- v1.7 (2026-08-07): merged the **GENERIC allowlist landing details** proposed by qa and finalized by dev (guardrail 5) — `const GENERIC_ALLOWLIST: &[&str]` in `src/error.rs` using locatable paths (e.g. `"tool::bash::Error::NonZeroExit"`); unit tests assert "any variant not in the allowlist is non-GENERIC"; entries must carry a `TODO(generic-allow): <issue>/<date> <reason>` comment; review convention: new allowlist entries must have a reason, entries without one are not allowed.
- v1.8 (2026-08-07): dev review landed — §5 annotated as the "Web DOM/ARIA stance", with the **bingo TUI mapping table** added (chat.busy→aria-busy, error-line highlight→red outline, status-area update→aria-live writes empty string, error line scrolled into view + highlighted→focus transfer, spinner rate degraded→reduced-motion, etc.; normative values unchanged); §6 adds Rust-side timing tests using `tokio::time::pause/advance` (zero new deps); 4.3 gains implementation touchpoints: `src/ui.rs` is the renderer-agnostic contract layer (the natural mount point for map_error), `UiEvent::Error(String)` has only 3 touchpoints to change into `{ code, msg }` and is the ideal entry point, and `chat.busy` is already a turn state machine precedent.
- v1.9 (2026-08-07): DX review corrections (devex) — §4.3 "landing ruling" fixes v1.5's stale wording ("`GENERIC` fallback" → "`GENERIC` explicit return + debug warning", "representative error variants per module" → "enumerate every variant of every module"), removing the self-contradiction with guardrail 5 / the v1.6 correction; v1.7 changelog attribution fixed (GENERIC allowlist proposed by qa, finalized by dev); the doc enters the reference network — `notes/research.md` "references" section gains a link, `AGENTS.md` "built-in skills sync" gains the rule "changes touching user-visible feedback states must be checked against this file".
- v1.10 (2026-08-07): dev re-review wrap-up — fixed guardrail 2 "registration is the contract" and §4.4 example table's **same-source automatic-fallback leftovers** ("unregistered → `GENERIC` fallback" / "unregistered path → lands on `GENERIC`" → "every variant explicitly returns a stable code; those not yet assigned explicitly return `GENERIC`; `_` implicit fallback is forbidden" / "explicit `GENERIC` path → lands on `GENERIC`"); the whole document is now fully self-consistent with the v1.6 model.
- v1.11 (2026-08-07): dev ruled on AC-15 timeout tiering — §1 "timeout" row and §7 anchors gain **per-operation-type granularity**: short synchronous operations (list_models/count_tokens/complete_text etc.) get the feedback layer's read 10s / write 15s, with retry as the primary action on timeout; **agent long turns don't get 10s/15s** (continuous progress feedback already exists; the transport layer 120s/60s + user interruption applies), **long-turn failures escalate to flow-level errors**; cancellation = the feedback layer drops the future at the deadline (tokio `timeout()`), the sequence-number check is only a fallback.
- v1.12 (2026-08-07): dev implementation-period backfill — §4.4 scenario table upgraded from "example" to the **complete registration-is-the-contract code table**: added `RATE_LIMITED` (429 throttling), `STORAGE_ERROR` (local storage), `TOOL_FAILED`, `HOOK_FAILED`; `AUTH_REQUIRED` semantics extended to "login expired / key missing / invalid key / 401"; `SERVER_ERROR` covers stream protocol and MCP connection failures; landing points: all 10 module error enums implement `ErrorCode` (exhaustive match with no `_` arm), `UiEvent::Error` structured (`{ code, msg }`), top-level CLI exit `[error] code=... msg=...` (non-TTY), feedback-layer timeout tiering (read 10s / write 15s). Code values only grow, never modified or reused.
- v1.13 (2026-08-07): devex post-implementation re-review backfill — guardrail 1 clarifies the downcast form: **matching by type name / string is forbidden**; `downcast_ref::<$t>()` **compile-time type references** are allowed (refactor-time errors); guardrail 4 gains the **dual-exit implementation story**: the TUI goes through `map_error`, the CLI boxed exit through `error_code_boxed` + a macro registry (registration-is-the-contract site #2, with a full-coverage test for the registry); the mapping logic stays single-sourced (ErrorCode impl) for consistency; the implementation model notes **current explicit GENERIC paths = 0** (missing_code dormant; future additions must call it).
- v1.14 (2026-08-07): qa regression-evidence addition — the implementation model hardens `missing_code`'s warning duty: any future explicit `GENERIC` return (including the **boxed-exit macro-registry miss landing on `GENERIC`** branch) must call `missing_code` to warn (loud in debug / known semantics in release); the dormancy cannot be documented alone with no code-side warning tail.
- v1.15 (2026-08-07): qa #69 contract alignment + main's ruling — §3.1 example table changes `AUTH_EXPIRED` to **`AUTH_REQUIRED`** (single-source alignment with §4.4/implementation, no new code; the login-expired/key-missing/invalid-key/401 semantics ride on msg + user action); §4.4 `TIMEOUT` row gains a **dual-presentation-level note** (short-sync = page-level; long turn = flow-level, AC-53 — the presentation level is decided by the trigger context, not inferred from the code alone).
- v1.16 (2026-08-07): #18 presentation-layer minimal implementation backfill (dev #86 + #92) — `UiEvent::Error` extended to `{ code, msg, level, context }` (level/context explicitly carried by the producer at emit time; chat.rs lands Full+LongTurn at turn level); §5 gains the TUI-side note "level is carried by the producer; the render layer only consumes, never derives; render-layer/test-side code→level mapping copies are forbidden"; §3.1 gains the "error level = typical slot, context can override" note (TIMEOUT dual-tier, PERMISSION_DENIED dual-tier takes the flow-level slot). The presentation layer is driven by `last_error` (chat.rs): Full = full-screen state (title + code + explanation + action hint, Enter retries / Esc returns / Ctrl+C exits), Page/Field = error line highlighted in the error color. **#92 short-op degradation stays visible**: §4.4 gains the "short-sync operation failure = degrade visibly, never swallow silently" story — list_models/count_tokens failures emit a Page+ShortSync error line (behavior degradation preserved: the menu stays empty/usable, budget 0), TurnStart resets the error state; production `ErrorLevel::Page`/`ErrorContext::ShortSync` move from dead_code to real emit sources.
- v1.17 (2026-08-07): todo task-area completion state tightened (ui/ux design) — the task area distinguishes **auto-opened** (TaskCreate signal, `tasks_auto`) from **manually opened** (Ctrl+T); an auto-opened panel auto-hides when all tasks are Completed (refresh_tasks closes the loop), reusing the §2 transient-row mechanism to push `✓ N/N tasks done · ctrl+t to view` (2s TTL, not persisted) for closure and a way back; a manually opened panel stays when everything completes (a state the user explicitly asked to see; no transient row); `/tasks` explicit requests are unaffected (no false "no background tasks" report).
- v1.19 (2026-08-07): non-fatal warning lifecycle tightened (main on-site report) — the `⚠` warning line above the input (MCP connection failure / image load failure etc.) changes from **persisting until /clear** to **auto-expiring after a 10s TTL** (`Chat::WARNING_TTL`, expiring entries cleaned on push + render filtering, dedup semantics kept); same spirit as §2 Toast "light notifications auto-dismiss" (difference: the warning is a static line above the input, not the Toast channel). MCP connections run in the background (not blocking turn input); failures are deferred and reported once in the next turn via `drain_unreported_failures`; new failures after `/mcp reconnect` can be reported again.
- v1.18 (2026-08-07): AskUserQuestion answer feedback block lifecycle tightened (main on-site report) — the answer-result block (`⏺ User answered the questions:`) changes from **persisting until /clear** to **transient within the turn**: cleared at TurnEnd (including zeroing the `flushed_ask_rows` cursor so the next block isn't skipped in rendering); kept during the answering process and the turn (intermediate states of multi-question flows visible, answers echoed back), gone when the turn ends — the block renders at the document tail / above the input, outside the message flow, and persistence would read as residue; same spirit as §2 transient rows' "completed feedback doesn't persist" (difference: the block is visible throughout the turn, no TTL). The answer content is already fed back to the model via tools, so no UI persistence is needed. **Superseded by v1.20**.
- v1.20 (2026-08-07): AskUserQuestion answers become **ordinary user messages in the message flow** (main on-site report: after v1.18 the block still clung above the input) — root cause: the result block rendered at the document tail (after messages, above the input), and only entered the settled/persisted state when "all preceding messages settled + no pending_ask + last message settled"; during the turn's model streaming that condition never held → the block clung above the input. Fix: delete the AskResult struct / `ask_result` field / `flushed_ask_rows` cursor / `SettledMark.ask_rows` (uproot the block's special logic); on answer submit, option confirm, or Esc decline (free_text requests) push a **User message** directly (content preserved: `User answered the questions:
  · question → answer`; declines are `User declined to answer questions`), rendered like user input (bubble), settled like it, persisted into scrollback like it, and kept with the session (no longer cleared at TurnEnd). **Ordered-settle guard**: a mid-turn answer message sits after the streaming assistant message; if the preceding messages aren't settled (streaming/tool running/image loading) this message must not settle either — otherwise persistence would jump past streaming rows and bake intermediate state into scrollback (the same invariant as "streaming content doesn't hit disk"). Difference from v1.18: answers are no longer turn-transient; they stay in the session like ordinary messages (visible on scroll-up, cleared only by `/clear`).

- v1.21 (2026-08-07): slash command interaction alignment landed (Team A · feat/slash-ux) —
  busy-time whitelist instant commands (think/model/provider/theme/status/context/tasks/help/skills run
  immediately while busy and busy stays unchanged; other slash commands queue and dispatch per command
  after TurnEnd, no longer sent to the model as plain text);
  `/think` level picker dual markers (●=in effect, fixed; ❯=browsing selection) + 1-6 direct jump +
  footer `think {level} ▸` preview state (Enter commits / Esc reverts); slash completion rows gain the
  arg_hint parameter hint;
  no-match hint row (`/zzz` → dim row, chrome-level hint not error-level, no error code);
  structured slash errors (UNKNOWN_COMMAND / BAD_ARGUMENT, `[error] code=… msg=…` single line, qa asserts on code only);
  slash output TTL grading (success 2s / error and usage ≥8s, cleared on the next input — spec in design contract §4.4);
  defer records: subcommand secondary completion, /model session-only `s`, model/thinking persistence layer (pending Q1).
- v1.25 (2026-08-09): image rendering unified on kitty Unicode placeholders (D42) — images now appear in the live viewport of both hosts the moment they load, tmux included (previously tmux fullscreen never showed images and tmux inline only after the block scrolled into scrollback); the loading state is unchanged (`#[image]` until loaded, `#[image ✗ load failed]` on failure). New one-time warning for WezTerm/Konsole (any environment): "This terminal does not support kitty Unicode placeholders (WezTerm/Konsole); images display as #[image]" — these terminals answer the kitty graphics query but cannot render U=1 placeholders, and with the C=1 direct path deleted they drop from image support (previously they displayed images outside tmux). The tmux passthrough probe warning is unchanged.
- v1.24 (2026-08-08): feedback tiers wired end to end (audit batch 3). Four output tiers: transient confirmations keep 2s; errors/usage move to the 8s error tier at every site (a dozen spoke through the success channel in plain color); a new INFO tier (`slash_info_lines`) holds explicitly requested reading — /help /status /context /config, listings, share URLs — persisting until the next input or Esc; pinned panels (`UiEvent::PinPanel/Unpin`) carry flows that must outlive any TTL: OAuth device codes stay for their full validity, the loopback flow shows the auth URL itself, and long operations (/compact, stats, MCP check/reconnect, share upload) hold a visible progress line until resolution. Startup notes (invalid provider fallback, transcript failures) reach the TUI via the info tier — the alt screen wiped stderr. Page/Field errors no longer reset a running turn's busy state, render in the fullscreen host (pinned above the prompt), and dismiss with Esc.
- v1.23 (2026-08-08): image feedback hardening after main's real-terminal reports — a failed image load renders a distinct `#[image ✗ load failed]` marker row (previously identical to the still-loading placeholder), with the warning line keeping the url; network fetches send a User-Agent and reject non-2xx bodies instead of failing later at decode; all kitty graphics commands use `q=2` so terminal error replies can no longer land in the input box as typed garbage (`ENOENT: image not found` flood), and both hosts retransmit placements after a resize purges the terminal-side image store (fullscreen previously lost images on resize until the next message).
- v1.22 (2026-08-08): fix feedback-lifecycle drift on the real host paths — the alternate-screen `run_fullscreen` now shares the Full error-state semantics with inline frames (title, stable code, explanation, Enter retries / Esc returns, input box and caret hidden) instead of bypassing `Frame::assemble`'s error branch; idle scheduling for slash error/usage lines feeds `slash_error_at` into `needs_tick`, so they clear after the 8s TTL and return to true idle even with no further input. Adds regressions for the real fullscreen frame and host/state idle TTL, covering "what the user sees" and "when feedback disappears". Public-share feedback is tightened in the same batch: local export is the default and only an explicit `--public` goes online; the "anyone can access + may contain sensitive content" warning shows before any byte is uploaded.

- v1.26 (2026-08-09): a confirmation tier the permission mode cannot silence (D46). The Team tool's changes — `start` / `stop` / `save` — and any Write/Edit aimed at `.bingo/team.json` prompt through the existing permission modal in **every** mode, including `bypassPermissions` and `acceptEdits`, and an `allow` rule cannot pre-authorize them (only `deny` outranks it); the mechanism is `Tool::confirm_reason`, sitting where the sensitive-path safety check already sat. Nothing new is drawn: the surface is the same `⏺ Allow Team` block with allow/deny. What changes is the copy contract for that tier — the description line states **the change, not the call** (`Rewrite .bingo/team.json · dev-room · 4 members (-ui +qa)`, `Pull up dev-room · 3 members (dev-ex, ui, qa) into #dev-room`), because a user approving a crew change is deciding about the crew, not about a file write. It stays one line by construction (the modal renders `request.question` as a single `Line`): rosters longer than four names collapse to `a, b, c, and N more`. Read actions (`status`/`validate`) are ordinary read-only tools and prompt for nothing.

- v1.27 (2026-08-09): the workspace view sheds its chrome (D47). The rail and sidebar are gone — Ctrl+K (now listing every conversation with its unread count) and alt+↑↓ carry navigation, Tab moves between the message list and the composer. **No surface colours**: the view paints foregrounds only, so the terminal's own background is the background in both themes; the only backgrounds left are marks (the avatar chip, the switcher's selected row) and one explicit erase (`Color::Reset` on overlay rows — ratatui patches styles, so an overlay of plain spaces would let the colours underneath show through). The header is two rows instead of three, with the team's name at the right edge. **Avatars**: where the terminal can place kitty images, each sender wears one of eight bundled portraits (4×2 cells, transmitted once per portrait, re-sent after a resize purges the image store); everywhere else the initial-on-colour chip stays, and the row count is identical either way — the fallback changes the gutter, never the layout.

- v1.28 (2026-08-09): the workspace stops quoting the runtime at the user (D49). Wake-up scaffolding written into an instance's history — a relayed channel message, a follow-up chase, the `[SYSTEM NOTIFICATION - TASK REMINDER]` block — used to render as a full quoted message under a "You" name row and avatar, as if the user had typed it. It now collapses to a single dim `▏` line (`#dev-team · main: …`, `system reminder · task tools`) that owns no name row and does not split the grouping of the real messages around it; text a person actually wrote is untouched. Same tier and same visual language as a tool attachment: this is context, not conversation.

- v1.29 (2026-08-10): the crew and its hires read as two different things (D53). `/team list` keeps the blueprint's roster and the runtime zone as the crew's alone, and adds a separate `temporary hires (N) · not in .bingo/team.json · released when their task is done` block below it — same row shape (`name · state · what for`), different heading, so a temp is never mistaken for a member. `AgentControl list` prefixes every row with `crew`/`hire` for the same reason. A released hire is announced rather than swept silently: the hub gets a task-notification line naming what went, because otherwise its next `SendMessage` to that name fails with `no subagent named …`, which reads as a bug instead of the agreed lifetime. New read command `/team norms` prints `.bingo/team-norms.md` under a `▸ team norms · <path>` header with the precedence line ("carried by every member and every hire; a direct instruction outranks it") above the file's own text; with no file it says so and points at `/team new`, which now scaffolds one beside a fresh blueprint (never overwriting an existing one).

- v1.30 (2026-08-10): empty long-turn completion no longer looks like success (#15). An upstream completion with no committed assistant content and no tool calls — including an unclosed thinking/text/tool block — is classified as malformed, retried automatically once while side-effect-free, and kept out of history; successful recovery may produce a non-fatal warning, while a second empty result enters the existing Full+LongTurn retry/back state. §7 adds the regression anchor that the empty attempt is never silently accepted.

- v1.31 (2026-08-10): a turn can no longer disappear and take the session with it (follow-on to v1.30). v1.30 stopped *producing* content-free assistant turns, but transcripts written before it still carry `content: []`, and history normalization indexed the first block of every message unguarded — resuming such a session panicked inside the spawned turn task. Tokio swallows that panic (nothing joins the handle) and the alternate screen repaints over its message within a frame, so the only symptom was a turn stuck on `✻ …ing… (esc to interrupt · Ns)` forever, with `busy` latched and gating Esc, Ctrl+C, submission and `/quit` alike — the session answered only to `kill`. Three changes: normalization reads blocks through `first()`; a lost turn is reported as the new flow-level `TURN_LOST` (retry / go back) instead of latching `busy`; and Ctrl+C force-quits once an interrupt has gone unhonoured for `INTERRUPT_GRACE` (3s), announced by the `Interrupting… press ctrl-c again to force quit` notice — a healthy interrupt is untouched, since the next turn clears the stamp. Loading history also drops messages that carry nothing (content-free messages and blank text blocks), so transcripts already poisoned are resumable rather than permanently rejected by the endpoints.

- v1.32 (2026-08-10): the transcript stops lying about order and about subagent work (D55, issue #28). **Order**: an `AskUserQuestion` answer is pushed as an ordinary user message (v1.20) but `stream_msg` still pointed at the assistant message above it, so everything the model did next rendered on top of the answer and the answer stayed pinned to the bottom until TurnEnd. The answer now ends that message and opens a fresh one (`open_continuation_message`) — the interrupted message settles and flushes immediately instead of waiting for the turn, a continuation the turn never filled is dropped at TurnEnd (tracked by `continuation_msg`), and a tool still in flight pins the stream where it is. **Folding**: `AgentControl` joins the existing collapse groups instead of producing one two-line block per call (and instead of closing whatever group was open); a look and a change are counted apart, so the summary reads `Checked 3 subagents, stopped 1 subagent` and never reports a killed run as a glance. The ⎿ row under a running group already showed the latest call — it now shows *which instance* it was aimed at, because `summarize_input`'s k=v fallback (first key, alphabetical) always landed on `action` and hid the target for `AgentControl`, `Channel` and `Team` alike. Every collapse group (not only subagent ones) now carries `· N failed` in the error colour when something inside it failed: a fold that can hide a stop must not count a refused stop as a success.
