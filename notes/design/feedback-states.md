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

- v0.1（2026-08-07）：草案，含 Loading/Toast/错误态三级与验收锚点。
- v1.0（2026-08-07）：并入 devex 三条——结构化错误协议（4.1）、TTY/非 TTY 降级（4.2）、状态机复位四条（状态机节）；补 GUI 侧反哺：错误码折叠呈现、反馈态不依赖环境总原则。
- v1.1（2026-08-07）：并入 main 实现侧两条——焦点渲染后异步聚焦（状态机复位第 4 条 / 第 5 节「焦点时序」）、CLI 错误码契约格式 `[error] code=... msg=...`（4.1）；并入 qa 边界六类——分档超时与计时器取消（Loading 节「超时」）、错误码→用户动作映射（3.1）、混合态（3）、动作粒度防重（Loading 节「防重复」）、陈旧响应竞态（状态机节）、Toast 量化（2）；可测试性约定（6）；reduced-motion 下 loading 指示保留、aria-live 写空串不删节点。
- v1.2（2026-08-07）：并入 devex 的 msg 转义约定（换行归一化空格、主 msg 截断 200 字符、多行堆栈走 `detail=`，见 4.1）；并入 dev 现状发现——错误码实现路径 A/B/C 待 main 拍板（4.3），本规范只约束出口契约不依赖内部路径。
- v1.3（2026-08-07）：并入 devex 的 C 路径三护栏——防 downcast（模块错误实现 `ErrorCode` trait 返回 `&'static str`，出口只调用不判断类型）、登记即契约（未登记走 `GENERIC` 兜底 + debug 构建告警）、防漂移单测（4.3）；错误码命名规范 `SCREAMING_SNAKE`（4.1）；新增 4.4「场景 → 错误码」示例表供 dev/qa 对照。
- v1.4（2026-08-07）：并入 qa/devex 三条——兜底码定为已发布稳定码 `GENERIC` 且未登记落兜底可断言（护栏 2）；码值只增不改不重用 semver 规则（护栏 3）；双出口一致性结构强制：GUI/CLI 共用同一 `map_error` 函数、单码表文件，断言作兜底（护栏 4）。
- v1.5（2026-08-07）：dev 拍板——路径定为 C 出口映射（4.3）；契约集中 `src/error.rs` 单文件（ErrorCode trait / GENERIC 兜底 + debug 告警宏 + map_error / 防漂移单测）；ErrorCode trait 实现模型：各模块 enum 穷尽 match 返回稳定码（新增 variant 未处理编译报错）+ 尾部 `_ => missing_code + GENERIC` debug 告警。
- v1.6（2026-08-07）：qa 澄清「穷尽 match 与 `_` 兜底臂矛盾」，dev 修正实现模型——**去掉 `_` 兜底臂、真穷尽 match 编译强制**（新增 variant 未处理编译报错）；`GENERIC` 改为**显式返回**（显式行为 + debug 告警），release 下语义已知「暂未分配稳定码」如实记录；防漂移单测改为**枚举每模块每 variant 断言非 GENERIC**（CI 期挡漏登记）。
- v1.7（2026-08-07）：并入 qa 提出、dev 定稿的 **GENERIC allowlist 落地细节**（护栏 5）——`src/error.rs` 里 `const GENERIC_ALLOWLIST: &[&str]` 用可定位路径（如 `"tool::bash::Error::NonZeroExit"`）；单测断言「不在 allowlist 的 variant 一律非 GENERIC」；条目必带 `TODO(generic-allow): <issue>/<日期> <理由>` 注释；review 约定：新增 allowlist 条目必须有理由，无理由不允许。
- v1.8（2026-08-07）：dev 评审落地——第 5 节标注「Web DOM/ARIA 口径」，补 **bingo TUI 映射表**（chat.busy→aria-busy、错误行高亮→红框、状态区更新→aria-live 写空串、错误行滚动可见+高亮→焦点转移、spinner 降频→reduced-motion 等，规范值不变）；第 6 节补 Rust 侧时序测试用 `tokio::time::pause/advance`（零新依赖）；4.3 补实现接入点：`src/ui.rs` 即 renderer-agnostic 契约层（map_error 天然挂载点）、`UiEvent::Error(String)` 仅 3 处改造为 `{ code, msg }` 是理想切入口、`chat.busy` 已是回合状态机先例。
- v1.9（2026-08-07）：DX 评审修正（devex）——4.3「落地拍板」修正 v1.5 旧口径残留（「`GENERIC` 兜底」→「`GENERIC` 显式返回 + debug 告警」、「各模块代表错误变体」→「枚举每模块每 variant」），消除与护栏 5 / v1.6 修正版的自相矛盾；v1.7 变更记录归属修正（GENERIC allowlist 为 qa 提出、dev 定稿）；文档进入引用网络——`notes/research.md`「参考」节加链接、`AGENTS.md`「内置技能同步」节补「改动涉及用户可见反馈态须对照本文件」规则。
- v1.10（2026-08-07）：dev 复评收尾——修正护栏 2「登记即契约」与 4.4 示例表的**同源自动兜底残留**（「未登记走 `GENERIC` 兜底」/「未登记路径 → 落 `GENERIC`」→「每个 variant 显式返回稳定码，暂未分配的显式返回 `GENERIC`，禁止 `_` 隐式兜底」/「显式 `GENERIC` 路径 → 落 `GENERIC`」），全篇口径与 v1.6 模型完全自洽。
- v1.11（2026-08-07）：dev 定夺 AC-15 超时分层——§1「超时」行与 §7 锚点补**按操作类型细分**：短同步操作（list_models/count_tokens/complete_text 等）套反馈层读 10s/写 15s、超时首要动作重试；**agent 长回合不套用 10s/15s**（持续进度反馈已有，走传输层 120s/60s + 用户中断），**长回合失败升级全流程级错误**；取消机制 = 反馈层到点 drop future（tokio `timeout()`），序号校验仅兜底。
- v1.12（2026-08-07）：dev 实现期回填——§4.4 场景表从「示例」升级为**登记即契约的完整码表**：新增 `RATE_LIMITED`（429 限流）、`STORAGE_ERROR`（本地存储）、`TOOL_FAILED`、`HOOK_FAILED`，`AUTH_REQUIRED` 语义扩展为「登录过期/缺 key/key 非法/401」，`SERVER_ERROR` 覆盖流协议与 MCP 连接失败；实现落地点：10 个模块错误 enum 全部实现 `ErrorCode`（match 穷尽无 `_` 臂）、`UiEvent::Error` 结构化（`{ code, msg }`）、CLI 顶层出口 `[error] code=... msg=...`（非 TTY）、反馈层超时分档（读 10s/写 15s）落地。码值只增不改不重用。
- v1.13（2026-08-07）：devex 实现后复评回填——护栏 1 澄清 downcast 形态：禁止**按类型名/字符串匹配**，允许 `downcast_ref::<$t>()` **编译期类型引用**（重构报错）；护栏 4 补**双出口实现口径**：TUI 走 `map_error`、CLI boxed 走 `error_code_boxed` + 宏登记表（登记即契约第二处，加登记表全覆盖测试），映射逻辑仍单一来源（ErrorCode impl）保证一致；实现模型注明**当前显式 GENERIC 路径 = 0**（missing_code 休眠，未来新增须调用）。
- v1.14（2026-08-07）：qa 回归实证补充——实现模型强化 `missing_code` 告警责任：未来新增显式 `GENERIC` 返回时（含 **boxed 出口宏登记表漏登记落入 `GENERIC`** 的分支）必须调用 `missing_code` 告警（debug 醒目 / release 语义已知），不能只在文档标注休眠而无代码告警尾巴。
- v1.15（2026-08-07）：qa #69 契约对齐 + main 拍板——§3.1 示例表 `AUTH_EXPIRED` 改为 **`AUTH_REQUIRED`**（单一来源对齐 §4.4/实现，不新增码；登录过期/缺 key/key 非法/401 语义由 msg + 用户动作承载）；§4.4 `TIMEOUT` 行补**双呈现级别注记**（短同步=页面级；长回合=全流程级，AC-53——呈现级别由触发上下文决定，不单由 code 推断）。
- v1.16（2026-08-07）：#18 呈现层最小实现落地回填（dev #86 + #92）——`UiEvent::Error` 扩展为 `{ code, msg, level, context }`（级别/上下文由生产者发射时显式携带，chat.rs 回合级落 Full+LongTurn）；§5 补 TUI 侧注「级别由生产者携带、渲染层只消费不推导，禁止渲染层/测试侧复制码→级别映射」；§3.1 补「错误级别 = 典型档位，上下文可覆盖」注（TIMEOUT 双档、PERMISSION_DENIED 双档取全流程档）。呈现层按 `last_error`（chat.rs）驱动：Full=整屏态（标题+码+说明+动作提示，Enter 重试/Esc 返回/Ctrl+C 退出）、Page/Field=错误行 error 色高亮。**#92 短操作降级可见**：§4.4 补「短同步操作失败 = 降级可见，不静默吞错」口径——list_models/count_tokens 失败发 Page+ShortSync 错误行（行为降级保留：菜单空/预算 0 仍可用），TurnStart 复位错误态；生产 `ErrorLevel::Page`/`ErrorContext::ShortSync` 从 dead_code 转为真实发射源。
- v1.17（2026-08-07）：todo 任务区完成态收口（ui/ux 方案）——任务区区分**自动打开**（TaskCreate 信号，`tasks_auto`）与**手动打开**（Ctrl+T）；自动打开的面板全部任务 Completed 即自动隐藏（refresh_tasks 收口），并复用 §2 瞬态行机制推 `✓ N/N tasks 完成 · ctrl+t 查看`（2s TTL 不落盘）给闭合感与找回路径；手动打开的面板全部完成保留（用户显式要看的态，不推瞬态行）；`/tasks` 显式请求临时放行不受影响（不误报「没有后台任务」）。
- v1.19（2026-08-07）：非致命警告生命周期收口（main 现场报障）——输入框上方 `⚠` 警告行（MCP 连接失败/图片加载失败等）从**常驻到 /clear** 改为 **10s TTL 自动过期**（`Chat::WARNING_TTL`，push 时清理过期条目 + 渲染过滤，去重语义保留）；与 §2 Toast「轻提示自动消失」同一精神（区别：警告是输入框上方的静态行，不走 Toast 通道）。MCP 连接改为后台执行（不阻塞回合输入），失败延迟到下一回合经 `drain_unreported_failures` 报告一次，`/mcp reconnect` 后的新失败可再报告。
- v1.18（2026-08-07）：AskUserQuestion 回答反馈块生命周期收口（main 现场报障）——回答结果块（`⏺ User answered the questions:`）从**常驻到 /clear** 改为**回合内瞬态**：TurnEnd 清除（含 `flushed_ask_rows` 游标归零，避免下次块跳过渲染）；回答过程与回合中保留（多问题中间态可见、答案回显），回合结束即消失——块渲染在文档尾部/输入框上方、不参与消息流，常驻会像残留物；与 §2 瞬态行「完成反馈不常驻」同一精神（区别：块在回合内全程可见，不走 TTL）。答案内容本就经工具回填给模型，无需 UI 常驻。**已被 v1.20 取代**。
- v1.20（2026-08-07）：AskUserQuestion 回答改为**普通用户消息进消息流**（main 现场报障：v1.18 后块仍吸在输入框上方）——根因：结果块渲染在文档尾部（消息之后、输入框上方），且只在「前置消息全定稿 + 无 pending_ask + 末消息定稿」时进定稿/落盘，回合内模型流式期间条件不满足 → 块常驻输入框上方。修复：删除 AskResult 结构 / `ask_result` 字段 / `flushed_ask_rows` 游标 / `SettledMark.ask_rows`（连根拔掉块特殊逻辑）；回答提交、选项确认、Esc 拒绝（free_text 请求）时直接 push 一条 **User 消息**（内容保留：`User answered the questions:\n  · 问题 → 答案`；拒绝为 `User declined to answer questions`），与用户输入同渲染（气泡）、同定稿、同落盘进 scrollback、随会话持久（TurnEnd 不再清除）。**顺序定稿守卫**：回合中回答的消息排在流式 assistant 消息之后，若前置消息未定稿（流式/工具运行中/图片加载中）本消息也不得定稿——否则落盘会越过流式行把中间态打进 scrollback（与「流式内容不落盘」同一不变量）。与 v1.18 的差别：回答不再回合瞬态，而是像普通消息一样留在会话里（上滑可见、`/clear` 才清）。

- v1.21（2026-08-07）：slash command 交互对齐落地（Team A · feat/slash-ux）——
  忙时白名单即时命令（think/model/provider/theme/status/context/tasks/help/skills 忙时立即执行且 busy 不变；
  其余 slash 命令入队，TurnEnd 后按命令分派，不再作为纯文本发模型）；
  `/think` 档位选择器双标记（●=当前生效固定、❯=浏览选中）+ 1-6 直达 + footer `think {level} ▸` 预览态
  （Enter 落地 / Esc 还原）；slash 补全行 arg_hint 参数提示；
  无匹配提示行（`/zzz` → dim 行，属 chrome 提示非 error 级，不走错误码）；
  slash 错误结构化（UNKNOWN_COMMAND / BAD_ARGUMENT，`[error] code=… msg=…` 单行，qa 只断言 code）；
  slash 输出 TTL 分级（成功 2s / 错误与用法 ≥8s 且下次输入清除——规格见设计契约 §4.4）；
  defer 记案：子命令二级补全、/model 的 s session-only、模型/思考持久化层（Q1 待议）。
- v1.25 (2026-08-09): image rendering unified on kitty Unicode placeholders (D42) — images now appear in the live viewport of both hosts the moment they load, tmux included (previously tmux fullscreen never showed images and tmux inline only after the block scrolled into scrollback); the loading state is unchanged (`#[image]` until loaded, `#[image ✗ 加载失败]` on failure). New one-time warning for WezTerm/Konsole (any environment): "此终端不支持 kitty Unicode 占位符（WezTerm/Konsole），图片以 #[image] 显示" — these terminals answer the kitty graphics query but cannot render U=1 placeholders, and with the C=1 direct path deleted they drop from image support (previously they displayed images outside tmux). The tmux passthrough probe warning is unchanged.
- v1.24 (2026-08-08): feedback tiers wired end to end (audit batch 3). Four output tiers: transient confirmations keep 2s; errors/usage move to the 8s error tier at every site (a dozen spoke through the success channel in plain color); a new INFO tier (`slash_info_lines`) holds explicitly requested reading — /help /status /context /config, listings, share URLs — persisting until the next input or Esc; pinned panels (`UiEvent::PinPanel/Unpin`) carry flows that must outlive any TTL: OAuth device codes stay for their full validity, the loopback flow shows the auth URL itself, and long operations (/compact, stats, MCP check/reconnect, share upload) hold a visible progress line until resolution. Startup notes (invalid provider fallback, transcript failures) reach the TUI via the info tier — the alt screen wiped stderr. Page/Field errors no longer reset a running turn's busy state, render in the fullscreen host (pinned above the prompt), and dismiss with Esc.
- v1.23 (2026-08-08): image feedback hardening after main's real-terminal reports — a failed image load renders a distinct `#[image ✗ 加载失败]` marker row (previously identical to the still-loading placeholder), with the warning line keeping the url; network fetches send a User-Agent and reject non-2xx bodies instead of failing later at decode; all kitty graphics commands use `q=2` so terminal error replies can no longer land in the input box as typed garbage (`ENOENT: image not found` flood), and both hosts retransmit placements after a resize purges the terminal-side image store (fullscreen previously lost images on resize until the next message).
- v1.22 (2026-08-08): fix feedback-lifecycle drift on the real host paths — the alternate-screen `run_fullscreen` now shares the Full error-state semantics with inline frames (title, stable code, explanation, Enter retries / Esc returns, input box and caret hidden) instead of bypassing `Frame::assemble`'s error branch; idle scheduling for slash error/usage lines feeds `slash_error_at` into `needs_tick`, so they clear after the 8s TTL and return to true idle even with no further input. Adds regressions for the real fullscreen frame and host/state idle TTL, covering "what the user sees" and "when feedback disappears". Public-share feedback is tightened in the same batch: local export is the default and only an explicit `--public` goes online; the "anyone can access + may contain sensitive content" warning shows before any byte is uploaded.

- v1.26 (2026-08-09): a confirmation tier the permission mode cannot silence (D46). The Team tool's changes — `start` / `stop` / `save` — and any Write/Edit aimed at `.bingo/team.json` prompt through the existing permission modal in **every** mode, including `bypassPermissions` and `acceptEdits`, and an `allow` rule cannot pre-authorize them (only `deny` outranks it); the mechanism is `Tool::confirm_reason`, sitting where the sensitive-path safety check already sat. Nothing new is drawn: the surface is the same `⏺ 允许执行 Team` block with 允许/拒绝. What changes is the copy contract for that tier — the description line states **the change, not the call** (`改写 .bingo/team.json · dev-room · 4 名成员（-ui +qa）`, `拉起 dev-room · 3 名成员（dev-ex、ui、qa）进入 #dev-room`), because a user approving a crew change is deciding about the crew, not about a file write. It stays one line by construction (the modal renders `request.question` as a single `Line`): rosters longer than four names collapse to `a、b、c、等 N 名`. Read actions (`status`/`validate`) are ordinary read-only tools and prompt for nothing.

- v1.27 (2026-08-09): the workspace view sheds its chrome (D47). The rail and sidebar are gone — Ctrl+K (now listing every conversation with its unread count) and alt+↑↓ carry navigation, Tab moves between the message list and the composer. **No surface colours**: the view paints foregrounds only, so the terminal's own background is the background in both themes; the only backgrounds left are marks (the avatar chip, the switcher's selected row) and one explicit erase (`Color::Reset` on overlay rows — ratatui patches styles, so an overlay of plain spaces would let the colours underneath show through). The header is two rows instead of three, with the team's name at the right edge. **Avatars**: where the terminal can place kitty images, each sender wears one of eight bundled portraits (4×2 cells, transmitted once per portrait, re-sent after a resize purges the image store); everywhere else the initial-on-colour chip stays, and the row count is identical either way — the fallback changes the gutter, never the layout.
