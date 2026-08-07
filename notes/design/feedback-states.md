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
| Explicit `GENERIC` path | `GENERIC` (no stable code assigned yet) | Follow the copy's guidance |

Code-value semver: once published, only grow, never modify or reuse; a new code = new variant mapping + an added assertion in
the `src/error.rs` drift-guard unit tests (missing either turns CI red).

**Short-sync operation failure = degrade visibly, never swallow silently (policy after #18 lands)**:
- When a short sync operation (`list_models` / `count_tokens`) fails, the TUI emits a page-level `UiEvent::Error` (`level: Page, context: ShortSync`) → an error-colored error line at the end of the content area (the user perceives "what happened + what they can do").
- **Behavior degradation is preserved**: the /model menu still shows known models (usable when the list is empty), /status still shows budget 0 — the error line is a hint; it doesn't block interaction or escalate to a full-screen state.
- Comparison: silently swallowing errors (`unwrap_or_default()` / `unwrap_or(0)` with no perception) violates general principle 1 "feedback states must not depend on the environment"; corrected to "degrade + stay visible".

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
- **State reset**: all four reset actions asserted one by one (aria-busy, aria-invalid, aria-live content, focus transfer); focus happens after rendering completes; stale-response races are ignored.
- **Structured errors**: non-TTY output follows the single-line `[error] code=... msg=...` contract; assertions use error codes, not copy.
- **Accessibility**: toast `aria-live` / error `role="alert"` readable by screen readers; under `prefers-reduced-motion`, motion is disabled but the loading indicator remains.

## Changelog


- v0.1 (2026-08-07): draft, covering Loading/Toast/the three error levels and acceptance anchors.
- v1.0 (2026-08-07): merged devex's three items — structured error protocol (4.1), TTY/non-TTY degradation (4.2), four state-machine reset actions (state machine section); added GUI-side feedback: collapsible error-code presentation, the "feedback states must not depend on the environment" general principle.
- v1.1 (2026-08-07): merged main's implementation-side two items — async focus after render (state-machine reset item 4 / section 5 "focus timing"), CLI error-code contract format `[error] code=... msg=...` (4.1); merged qa's six boundary classes — tiered timeouts and timer cancellation (Loading section "Timeout"), error-code → user-action mapping (3.1), mixed state (3), action-granularity de-dup (Loading section "Duplicate prevention"), stale-response race (state machine section), Toast quantification (2); testability conventions (6); loading indicator kept under reduced-motion, aria-live write-empty-string without node deletion.
- v1.2 (2026-08-07): merged devex's msg escaping conventions (newlines normalized to spaces, primary msg truncated to 200 chars, multi-line stacks via `detail=`, see 4.1); merged dev's current-state findings — error-code implementation path A/B/C pending main's decision (4.3); this spec constrains only the exit contract, not the internal path.
- v1.3 (2026-08-07): merged devex's three guardrails for path C — anti-downcast (module errors implement the `ErrorCode` trait returning `&'static str`; exits only call, never branch on types), registration is the contract (unregistered → `GENERIC` fallback + debug-build warning), drift-guard unit tests (4.3); `SCREAMING_SNAKE` error-code naming convention (4.1); added 4.4 "scenario → error code" sample table for dev/qa reference.
- v1.4 (2026-08-07): merged qa/devex's three items — the fallback code is the published stable `GENERIC`, and the unregistered→fallback landing is assertable (guardrail 2); code values only grow, never modify or reuse (guardrail 3); dual-exit consistency made structural: GUI/CLI share the same `map_error` function, single code-table file, assertions as fallback (guardrail 4).
- v1.5 (2026-08-07): dev's final call — path is C, exit mapping (4.3); the contract centralized in the single file `src/error.rs` (ErrorCode trait / GENERIC fallback + debug warning macro + map_error / drift-guard unit tests); ErrorCode trait implementation model: each module's enum exhaustively matches to return stable codes (an unhandled new variant fails to compile) + a tail `_ => missing_code + GENERIC` debug warning.
- v1.6 (2026-08-07): qa clarified "exhaustive match contradicts the `_` fallback arm"; dev corrected the implementation model — **the `_` fallback arm is removed; a truly exhaustive match is compile-enforced** (unhandled new variant fails to compile); `GENERIC` becomes an **explicit return** (explicit behavior + debug warning), with known release semantics of "no stable code assigned yet", recorded honestly; the drift-guard unit test becomes **enumerating every variant of every module, asserting non-GENERIC** (CI-period missed-registration blocking).
- v1.7 (2026-08-07): merged the **GENERIC allowlist landing details** (qa proposed, dev finalized; guardrail 5) — `const GENERIC_ALLOWLIST: &[&str]` in `src/error.rs` with locatable paths (e.g. `"tool::bash::Error::NonZeroExit"`); unit test asserts "any variant not in the allowlist is non-GENERIC"; entries must carry a `TODO(generic-allow): <issue>/<date> <reason>` comment; review convention: new allowlist entries must have a reason; no reason, no entry.
- v1.8 (2026-08-07): dev review landed — section 5 labeled "Web DOM/ARIA stance", added the **bingo TUI mapping table** (chat.busy→aria-busy, error-line highlight→red outline, status-area update→aria-live write-empty-string, error-line scroll-into-view+highlight→focus transfer, spinner rate reduction→reduced-motion etc.; normative values unchanged); section 6 adds `tokio::time::pause/advance` for Rust-side timing tests (zero new dependencies); 4.3 adds implementation integration points: `src/ui.rs` is the renderer-agnostic contract layer (`map_error`'s natural mount point), `UiEvent::Error(String)` appears in only 3 places and refactoring to `{ code, msg }` is the ideal entry point, `chat.busy` is already a turn state-machine precedent.
- v1.9 (2026-08-07): DX review corrections (devex) — 4.3 "landing decision" fixes v1.5's stale wording ("`GENERIC` fallback" → "`GENERIC` explicit return + debug warning", "representative error variants of each module" → "enumerate every variant of every module"), eliminating the self-contradiction with guardrail 5 / the v1.6 correction; v1.7 changelog attribution fixed (GENERIC allowlist proposed by qa, finalized by dev); the document enters the reference network — `notes/research.md` "References" section gains a link, `AGENTS.md` "built-in skills sync" section gains the rule "changes touching user-visible feedback states must be checked against this file".
- v1.10 (2026-08-07): dev re-review wrap-up — fixes the same-source auto-fallback residue in guardrail 2 "registration is the contract" and the 4.4 sample table ("unregistered → `GENERIC` fallback" / "unregistered path → lands on `GENERIC`" → "every variant explicitly returns a stable code; not-yet-assigned ones explicitly return `GENERIC`; `_` implicit fallback forbidden" / "explicit `GENERIC` path → lands on `GENERIC`"), making the whole document fully consistent with the v1.6 model.
- v1.11 (2026-08-07): dev settled AC-15 timeout layering — §1 "Timeout" row and §7 anchors gain **per-operation-type breakdown**: short sync operations (list_models/count_tokens/complete_text etc.) apply the feedback layer's read 10s / write 15s, timeout's primary action is retry; **long agent turns don't apply 10s/15s** (continuous progress feedback already exists; go through the transport layer 120s/60s + user interruption), **long-turn failures escalate to flow-level errors**; cancellation mechanism = the feedback layer drops the future at the deadline (tokio `timeout()`); the sequence-number check is only a fallback.
- v1.12 (2026-08-07): dev implementation-period backfill — §4.4 scenario table upgraded from "sample" to the **complete registration-is-the-contract code table**: added `RATE_LIMITED` (429 rate limiting), `STORAGE_ERROR` (local storage), `TOOL_FAILED`, `HOOK_FAILED`; `AUTH_REQUIRED`'s meaning extended to "login expired/key missing/invalid key/401"; `SERVER_ERROR` covers stream protocol and MCP connection failures; landing points: all 10 module error enums implement `ErrorCode` (exhaustive match, no `_` arm), `UiEvent::Error` structured (`{ code, msg }`), top-level CLI exit `[error] code=... msg=...` (non-TTY), feedback-layer tiered timeouts (read 10s / write 15s) landed. Code values only grow, never modify or reuse.
- v1.13 (2026-08-07): devex post-implementation review backfill — guardrail 1 clarifies downcast forms: **matching by type name / string** forbidden; `downcast_ref::<$t>()` **compile-time type references** allowed (refactors error out); guardrail 4 adds the **dual-exit implementation stance**: TUI via `map_error`, CLI boxed via `error_code_boxed` + the macro registry (the second "registration is the contract" site, with a full-coverage registry test); mapping logic stays a single source (ErrorCode impls) for consistency; the implementation model notes **current explicit GENERIC paths = 0** (`missing_code` dormant; future additions must call it).
- v1.14 (2026-08-07): qa regression evidence supplement — the implementation model strengthens the `missing_code` warning responsibility: future explicit `GENERIC` returns (including the **boxed-exit macro-registry miss that lands on `GENERIC`** branch) must call `missing_code` to warn (loud in debug / known semantics in release); dormancy can't be documented only without a code-side warning tail.
- v1.15 (2026-08-07): qa #69 contract alignment + main's final call — §3.1 sample table's `AUTH_EXPIRED` becomes **`AUTH_REQUIRED`** (single source aligned with §4.4/implementation; no new code; login-expired/key-missing/invalid-key/401 semantics carried by msg + user action); §4.4's `TIMEOUT` row gains a **dual-presentation-level note** (short-sync = page-level; long turn = flow-level, AC-53 — the presented level is determined by the trigger context, not inferred from the code alone).
- v1.16 (2026-08-07): #18 presentation-layer minimal implementation backfill (dev #86 + #92) — `UiEvent::Error` extended to `{ code, msg, level, context }` (level/context explicitly carried by the producer at emission; chat.rs emits Full+LongTurn at turn level); §5 adds the TUI-side note "level is carried by the producer; the render layer only consumes, never derives; duplicating a code→level mapping in the render layer or test side is forbidden"; §3.1 adds the "error level = typical slot, context can override" note (TIMEOUT dual-slot, PERMISSION_DENIED dual-slot taking the flow-level slot). Presentation driven by `last_error` (chat.rs): Full = full-screen state (title + code + explanation + action hint; Enter retries / Esc returns / Ctrl+C exits), Page/Field = error-colored error-line highlight. **#92 short-operation degrade-visibly**: §4.4 adds the "short-sync operation failure = degrade visibly, never swallow silently" policy — list_models/count_tokens failures emit a Page+ShortSync error line (behavior degradation preserved: empty menu / budget 0 still usable); TurnStart resets the error state; `ErrorLevel::Page`/`ErrorContext::ShortSync` producers move from dead_code to real emission sources.
- v1.17 (2026-08-07): todo task-area completed-state closure (ui/ux proposal) — the task area distinguishes **auto-opened** (TaskCreate signal, `tasks_auto`) from **manually opened** (Ctrl+T); an auto-opened panel hides automatically once all its tasks are Completed (closed via refresh_tasks), and reuses the §2 transient-row mechanism to push `✓ N/N tasks completed · ctrl+t to view` (2s TTL, not flushed) for closure and a way back; a manually opened panel stays when all tasks complete (a state the user explicitly asked to see; no transient row pushed); an explicit `/tasks` request is temporarily exempt and unaffected (no false "no background tasks" report).
- v1.19 (2026-08-07): non-fatal warning lifecycle closed (issue reported live by main) — the `⚠` warning line above the input box (MCP connection failure / image load failure etc.) changes from **persistent until /clear** to **auto-expiring with a 10s TTL** (`Chat::WARNING_TTL`; stale entries pruned on push + render-time filtering; de-dup semantics kept); same spirit as §2's Toast "light notices auto-disappear" (difference: the warning is a static line above the input box, not the Toast channel). MCP connections now run in the background (never blocking turn input); failures are reported once, delayed to the next turn via `drain_unreported_failures`; a new failure after `/mcp reconnect` can be reported again.
- v1.18 (2026-08-07): AskUserQuestion answer-feedback block lifecycle closed (issue reported live by main) — the answer-result block (`⏺ User answered the questions:`) changes from **persistent until /clear** to **transient within the turn**: cleared at TurnEnd (including zeroing the `flushed_ask_rows` cursor, avoiding the next block skipping rendering); kept during the answering process and the turn (multi-question intermediate states visible, answers echoed), gone at turn end — the block renders at the document tail / above the input box, doesn't participate in the message stream, and persisting would look like residue; same spirit as §2's transient rows ("completion feedback doesn't persist") (difference: the block stays visible for the whole turn, no TTL). The answer content is already fed back to the model via the tool, so no UI persistence is needed.
