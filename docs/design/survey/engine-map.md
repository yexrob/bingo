# Engine and domain map of the old bingo (survey, 2026-08-28)

> Source: subagent report, archived verbatim. Facts were verified on the date in the title; re-verify before depending on a version.

I have what I need. Here's the map.

---

# bingo engine map (read-only survey)

Repo: `/Users/yexrob/Episodes/Projects/bingo-inc/bingo` — 148,754 Rust LOC, 456 commits, 2026-08-08 → 2026-08-28.

## 1. Layering as-built

Grep of `crate::<mod>` references (not just `use`) across non-test files. Intended layering is roughly:

```
platform / storage / error            (leaf infra)
  ↑
api::{types,contract,sse}  →  api::providers::{anthropic,openai}  →  api::client
  ↑
tool::{mod,executor,*}   permission   hooks   compact   transcript   settings
  ↑
query{,_turn,_session}                 ← the agentic loop
  ↑
engine::{events,runner,actions}        ← host boundary + Engine impl
  ↑
app::* (AppCore actor)  →  tui / app_server / print   (frontends)
```

**Actual violations (production code, not tests):**

| Edge | Where | Note |
|---|---|---|
| `tool → tui` | `/Users/yexrob/Episodes/Projects/bingo-inc/bingo/src/tool/team.rs:329` calls `crate::tui::avatar::ids()` | a tool's prompt text reads the terminal's avatar table |
| `tool → ui` | `src/tool/agent.rs` (`crate::ui::…`) | subagent tool reaches the frontend event vocabulary |
| `compact → ui` | `src/compact.rs:357` `fn conversation_of(..) -> crate::ui::ConvKey` | compactor names a *conversation* (a frontend/core concept) |
| `permission → team` | `src/permission.rs:311` `crate::team::TEAM_FILE` | the gate hardcodes knowledge of the multi-agent feature |
| `system → team`, `system → tool` | `src/system.rs:222,235,241` | prompt assembly pulls crew note, `agent_notes::MAIN_CHANNEL_NOTE`, `tool::experience::session_index` |
| `settings → api`, `settings → channels` | `src/settings.rs:967,1177` | config layer reaching up (mostly test-adjacent, but `ChannelLimits::from_settings` is real coupling in the other direction) |
| `error → ~14 modules` | `src/error.rs:190-222` `error_code_boxed` / `downcast_error_code` | the code registry downcasts against every concrete error type: a leaf that depends on the whole tree |
| `storage → transcript/api` | `src/storage.rs` (retention sweep) | infra leaf knows record formats |
| `app/projection.rs → tui` | `src/app/projection.rs` | core projection reaches the terminal |
| `query → 20 modules` | `src/query.rs:1-21` + inline | `agents api app bm budget channels compact context_usage engine error experience hooks live memory permission print query_session query_turn rewind settings tool tools transcript` |

**Cycles.** Three real ones:
- `query ⇄ engine` — `query_turn.rs` imports `engine::events::{EngineEvent,EngineHost}`; `engine/runner.rs` calls `query::run_query`. The "boundary" is mutual.
- `query ⇄ app` — `Session` (in `query_session.rs:69`) holds 10 `app::*` handles; `app/turn.rs`, `app/interaction.rs`, `app/projection.rs` all import `query`.
- `tool ⇄ query` — `tool/mod.rs:56` `ToolContext.ask_question: Arc<crate::query::AskQuestionFn>`; `query.rs` imports `tool::*`.

**Bottom line:** `Session` is the god-object that makes the graph acyclic-on-paper and cyclic-in-fact. It is defined in `query_session.rs` but ~half its fields are `app::` actor handles.

## 2. Domain model

### Wire/protocol layer (clean, single representation)

| Type | File:line | Role |
|---|---|---|
| `Role` | `src/api/types.rs:8` | User \| Assistant |
| `ContentBlock` | `src/api/types.rs:15` | Text \| ToolUse \| ToolResult \| Thinking \| Image — Anthropic-shaped, serde `tag="type"` |
| `Message` | `src/api/types.rs:216` | `{role, content: Vec<ContentBlock>}` — **the** canonical unit, also the transcript line format |
| `ImageSource` / `ImageAttachment` | `src/api/types.rs:45,206` | base64 image payload |
| `SystemBlock` | `src/api/contract.rs:237` | `{text, cache}` — one cacheable prompt segment |
| `NeutralRequest` | `src/api/contract.rs:246` | `{model, max_tokens, system, messages, tools: Vec<Value>, stream, thinking}` — the unified request |
| `StreamEvent` | `src/api/contract.rs:343` | the unified event: `MessageStart / TextStart / ThinkingStart / ToolUseStart / TextDelta / ThinkingDelta / SignatureDelta / InputJsonDelta / BlockStop / StopReason / Done / ApiError` |
| `ProviderClient` | `src/api/contract.rs:424` | the provider trait |
| `AssistantAccumulator` | `src/api/contract.rs:455` | folds `StreamEvent`s → one `Message` + stop_reason + input_tokens |
| `ClientError` | `src/api/contract.rs:16` | MissingApiKey / Api / ContextOverflow / Stream / Transport / Timeout / Auth / Unsupported / Config |
| `Capabilities` | `src/api/contract.rs:189` | static per-protocol declaration |
| `ThinkingLevel` | `src/api/contract.rs:146` | Low..Max |
| `Client` | `src/api/client.rs:66` | provider *facade*: table of adapters, current-provider switch, model resolver, learned windows |

### Loop layer

| Type | File:line | Role |
|---|---|---|
| `Session` | `src/query_session.rs:69` | everything a run reads: client, `Runtime`, settings, system, cwd, depth + 10 actor handles |
| `Runtime` | `src/query_session.rs:16` | the mutable-by-slash-command bits as `watch` channels (model, provider, thinking, transcript, permissions, mcp, rewind) |
| `QueryOutcome` / `QueryEndReason` / `QueryError` | `src/query.rs:51,44,24` | run result |
| `Turn` (private) | `src/query_turn.rs:68` | one model response: assistant msg + tool_uses + stop_reason + input_tokens + aborted |
| `ToolCallDone` / `ToolCallStatus` | `src/query.rs:209,222` | one finished tool call, for display |
| `AskOutcome` / `AskContext` / `AskFn` | `src/query.rs:230,258,276` | permission prompt request+verdict |
| `AskAnswer` / `AskQuestionFn` | `src/query.rs:282,291` | AskUserQuestion |
| `SteerFn` | `src/query.rs:311` | tool-barrier steering pull |
| `GateDecision` | `src/query.rs:441` | gate result (behavior, reason, rewritten input, guidance) |
| `TokenGate` | `src/compact.rs:675` | exact-count anchor + estimate extrapolation |

### Tool layer

| Type | File:line |
|---|---|
| `Tool` trait | `src/tool/mod.rs:107` |
| `ToolResult` | `src/tool/mod.rs:77` — `{content: serde_json::Value, is_error: bool, diff: Option<String>}` |
| `ToolContext` | `src/tool/mod.rs:34` — 13 fields incl. `ask_question`, `watch`, `live`, `rewind`, `tasks`, `hooks` |
| `ToolError` | `src/tool/mod.rs:84` — one variant, `Failed(String)` |
| `PendingCall` / `ExecOutcome` | `src/tool/executor.rs:29,35` |

### Permission layer

`PermissionMode` (`src/permission.rs:5`), `PermissionBehavior` (`:31`), `PermissionResult` (`:38`), `can_use_tool` (`:325`), `session_allow_rule` (`:418`), `safety_check` (`:288`).

### Core/host layer (newer, added 2026-08-18)

| Type | File:line |
|---|---|
| `EngineEvent` | `src/engine/events.rs:31` — the run's one-way report |
| `EngineEvents` / `EngineRequests` / `EngineHost` | `src/engine/events.rs:172,251,271` |
| `Run` (work handed to an engine) | `src/app/engine.rs:28` — Turn / Shell / Promote / Wake / Posted / Interrupt / Act |
| `Engine` trait | `src/app/engine.rs:101` — `fn run(&self, run: Run)`, one method, fire-and-forget |
| `SessionEngine` | `src/engine/runner.rs:25` — the only real impl |
| `AppCore` | `src/app/mod.rs:305` — the single actor |
| `Item` / `ItemBody` | `src/app/snapshot.rs:500,515` — the *persisted/projected* conversation record |
| `AppEventPayload` | `src/app/event.rs:364` — 44 variants, 1:1 with app-server JSON-RPC notifications |
| `UiEvent` | `src/ui.rs:153` — the terminal's own event vocabulary |

### ⚠ Same concept, multiple representations

1. **Four event enums for one stream.** `StreamEvent` (wire) → `EngineEvent` (run report, `EngineEvent::from_stream` at `src/engine/events.rs:120`) → `AppEventPayload` (core, 44 variants) → `UiEvent` (terminal, `src/ui.rs:153`). The module doc at `src/engine/events.rs:21` admits: *"the frontends translate them into `UiEvent` at the three adapters, which are marked as the shims they are."*
2. **`PermissionMode` twice** — `src/permission.rs:5` and `src/app/snapshot.rs:112`, converted by hand in `src/main.rs:435-445` and `src/app_server/session.rs:349-355`.
3. **`ThinkingLevel` twice** — `src/api/contract.rs:146` and `src/app/snapshot.rs:78`, bridged in `mirror_config` (`src/engine/runner.rs:303`).
4. **`ShellDialect` twice** — `src/platform.rs:56` and `src/app/snapshot.rs`.
5. **Message vs Item.** `Message` is the model's truth (transcript JSONL); `Item`/`ItemBody` is the client's truth (11 body variants). Nothing derives one from the other automatically — `src/app/projection.rs` (1251 lines) reconciles them.
6. **`ConvKey`** is *not* duplicated: `src/ui.rs:108` re-exports `src/app/conversation.rs:32`. Good.
7. **`tool_result_text` / `render_result`** — `src/api/types.rs:80` and re-wrapped at `src/query.rs:572`.
8. **`permission_mode_str`** duplicated verbatim in `src/query.rs:562` and `src/compact.rs:796`.

## 3. Main loop walkthrough (one user turn)

1. Frontend writes `AppRequest` → `AppCore` actor (`src/app/mod.rs:305`). The actor mints a `TurnId`, records the user item, and hands `Run::Turn { turn, text }` to the attached `Engine` (`src/app/engine.rs:28`).
2. `SessionEngine::run` (`src/engine/runner.rs:323`) → `spawn(turn, Work::Prompt(text))` (`:83`). It resets the cancel `watch`, builds the `EngineHost` via `host_for` (`:239`) — wiring `ask`/`ask_question` to `app::interaction::permission_ask/question_ask` and `steer` to `queue.absorb` — and binds it to the turn (`EngineHost::bound`).
3. Inside the spawned task: a `TurnGuard` (`src/app/turn.rs:221`) is taken so `Drop` closes the turn exactly once even under abort; the body runs inside `AssertUnwindSafe` + `catch_unwind` so a panic closes with `TURN_LOST`.
4. `SessionEngine::history` (`:57`) loads `Vec<Message>` from `transcript.load_messages()`.
5. `query::run_query` (`src/query.rs:1492`): `claim_run` (one host = one run), `tools::assemble_tools` (`src/tools.rs:23` — builtins + depth-gated tools + MCP), `tool_context` (`src/query.rs:734`).
6. `run_user_prompt_submit` hook (`src/hooks.rs:326`) — exit 2 aborts the submission.
7. `recall_context` (`src/query.rs:1558`) — BM25 (`src/bm25.rs`) over project experiences + memory facts; appended to the tail of the user text.
8. `record_turn_open` (`:867`) writes a `{"type":"turn","at":…}` marker then the user `Message` to the transcript, and `EngineEvent::Inbound` to the host.
9. **`query_loop`** (`src/query.rs:915`). Per iteration, in order:
   - drain agent inbox (`InboxWake::take` → `agent::absorb_inbox`), record as a user message;
   - `compact::check_and_compact` (`src/compact.rs:730`) — see §"compaction" below;
   - `maybe_inject_task_reminder` (`:170`);
   - `flush_agent_inbox`, `release_hires`, `watch.consume_notifications` → `<task-notifications>` block;
   - main-only `channels.drain_main_mail()` → `<messages>` block;
   - `report_context_usage` → `EngineEvent::ContextUsage`.
10. `query_turn::one_turn_with_stream_retries` (`src/query_turn.rs:340`) → `one_turn` (`:145`): builds `NeutralRequest` (model/thinking/max_tokens from `budget::max_tokens_for`, system refreshed via `system::with_model_capabilities`, `tools: tool_params(tools)`), calls `session.client.stream(&request)`.
11. The stream read loop `tokio::select!`s three ways: next event / `cancel_requested` / inbox wake. Every event goes to `acc.push()` and `host.events.emit_stream()`. On `BlockStop` of a `ToolUse` block it emits `EngineEvent::ToolReady` with the complete input.
12. Retries: `ApiError` or `ClientError::{Stream,Transport}` → `StreamApiErrorKind::retryable` decides; up to 10 attempts, `backoff_delay` or server `retry_after` clamped to 60s; emits `EngineEvent::StreamRetry { discarded_output }` so the UI can withdraw the live tail.
13. Back in `query_loop`: `gate.record_exact(turn.input_tokens, estimate)` anchors the token gate on the server's own count (D172); the compact circuit breaker decays by one.
14. Branches before tools: `aborted` → record interrupt marker, return; all-empty assistant → one silent retry then `EmptyResponseRetried`; `stop_reason == "max_tokens"` → inject a resume prompt (≤3×); no tool_uses → run Stop hooks (exit 2 injects stderr and loops once), else return.
15. **Phase 1 — gate** (`src/query.rs:1272`): for each `ToolUse`, `find_tool`, then `gate_tool` (`:490`): `run_pre_tool_use` hook (may rewrite input or deny) → `can_use_tool` (deny > ask > allow, with `safety_check` and sub-command splitting for Bash) → if `Ask`, compute `session_allow_rule` scope + `tool.preview_diff`, `await (host.requests.ask)(&AskContext)`. `AllowSession` installs a session-only rule.
16. **Phase 2 — execute** (`src/tool/executor.rs:48`): consecutive `is_concurrency_safe` calls run as a `FuturesUnordered` batch of ≤10; anything else runs serially. Each batch races the cancel watch; already-finished results are kept.
17. Each outcome → `EngineEvent::ToolDone(ToolCallDone)` + a `ContentBlock::ToolResult` via `result_block` (`src/query.rs:576`, arrays pass through so images survive, text is clipped at `MAX_RESULT_CHARS` = 50k). `run_post_tool_use` exit 2 sets `stop_after_tools`.
18. `fill_missing_tool_results` (`:850`) guarantees every `tool_use` has a paired `tool_result` — otherwise every future history replay 400s.
19. **Tool barrier / steering** (`:1400`): if not interrupted/stopped/cancelled, `(host.requests.steer)()` → `queue.absorb(ConvKey::Main, turn)` returns `Vec<SteerItem>`; each appended as a `ContentBlock::Text` **after** the tool_results in the same user message.
20. `record(...)` the user message (transcript append + `host` notification), emit `RoundEnd`, loop. On return, `SessionEngine` closes the turn via the guard with `Completed`/`Interrupted`/`Failed`.

**Where interrupt/steer/queue live:** interrupt = a per-session `watch::Sender<bool>` on `SessionEngine` (`src/engine/runner.rs:32`), fired by `Run::Interrupt`; steering = `app::queue::QueueHandle::absorb` behind `SteerFn`; the queue itself is actor state (`src/app/queue.rs`, `STEER_MARKER` at `:37`).

**Compaction:** `check_and_compact` (`src/compact.rs:730`) runs `count_tokens` every 5 turns or on +20k estimate growth, otherwise extrapolates. Threshold = `budget::autocompact_threshold_for` = 90% of (window − max_tokens). On trip → `maybe_compact` → summarize the head with a fixed prompt (`COMPACT_PROMPT`, `:50`), keep `KEEP_RECENT`=12 messages capped at window/4 tokens, append a `{"type":"compact","summary","kept"}` transcript marker. A `ContextOverflow` 400 mid-loop triggers `compact_after_overflow` (`:257`) with deterministic rungs: truncate oversized blocks → drop oldest → retry once; the rejection body also feeds `client.learn_window` (`src/api/learned.rs`).

## 4. Provider layer

**Yes, it's a trait.** `ProviderClient` (`src/api/contract.rs:424`), 5 methods:

```rust
fn capabilities(&self) -> Capabilities;
fn auth_status(&self) -> AuthStatus;
async fn stream(&self, req: &NeutralRequest) -> Result<BoxStream, ClientError>;
async fn complete_text(&self, req: &NeutralRequest) -> Result<String, ClientError>;
async fn list_models(&self) -> Result<Vec<String>, ClientError>;
async fn count_tokens(&self, model, system, messages, tools) -> Result<u64, ClientError>;
```

`BoxStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, ClientError>> + Send>>` (`:398`).

- **Wire protocols: exactly two.** `anthropic` = Messages API (`src/api/providers/anthropic.rs`, `API_BASE`/`API_VERSION` `2023-06-01`). `openai` = **Responses API only** (`src/api/providers/openai.rs:1` — "OpenAI Responses protocol adapter"). No chat-completions adapter. `build_provider` (`src/api/providers/mod.rs:83`) rejects anything else at startup.
- **Two OpenAI variants:** `OpenAiVariant::{Default, Codex}` (`openai.rs:67`). Codex = `chatgpt.com/backend-api/codex/responses` + `ChatGPT-Account-Id` header + its own model-list route.
- **Auth:** `AuthSource::{ApiKey, StoredKey, OAuth}` (`providers/mod.rs:20`). OAuth is `TokenProvider` (`src/api/auth.rs:577`) — Codex device flow + loopback PKCE, lazy refresh, single-flight; tokens in `auth.json` (0600, opencode-compatible shape). Presets in `presets.rs`: `codex` and `opencode-go`.
- **`Unconfigured` placeholder adapter** (`providers/mod.rs:~168`) so the TUI still boots with no credentials.
- **`Client`** (`src/api/client.rs:66`) is a *facade*, not a provider: holds the adapter table, the current-provider switch, `ModelResolver`, `WindowClamps`. `with_provider` forks a `Client` for subagents.
- **`contract.rs`** = the neutral vocabulary + error taxonomy + retry classification (`StreamApiErrorKind::from_message`, `:268` — keyword matching on provider prose) + `AssistantAccumulator`. It is the seam.
- **`learned.rs`** (215 lines) = *learned context windows*. When a provider 400s with "prompt is too long", `parse_context_limit` extracts the real ceiling and `WindowClamps::set` persists `provider:model → window` to a flat JSON map next to the catalog, so the next session compacts before the 400 instead of rediscovering it. Applied under the user's explicit declaration but over the family table.

**Provider-specific leakage into "neutral" code:**
- `effort_for` (`openai.rs:49`) hardcodes `gpt-5.6` / `deepseek` prefixes.
- `ContentBlock` (`api/types.rs:15`) *is* the Anthropic wire shape — `ToolResult.content` is untyped `serde_json::Value` carrying Anthropic protocol blocks; the OpenAI adapter has to flatten it (`tool_output_wire`, `openai.rs:363`).
- `StreamApiErrorKind::from_message` (`contract.rs:268`) is a hand-written keyword list of provider error prose.
- `ClientError::from_response` sniffs 12 English substrings to classify context overflow (`contract.rs:80`).
- `src/api/models.rs` has a compiled model-family prefix table; `model_families.rs` makes it user-overridable via `~/.config/bingo/model-catalog.json`.

## 5. Tool protocol

```rust
#[async_trait]
pub trait Tool: Send + Sync {                      // src/tool/mod.rs:107
    fn name(&self) -> String;
    fn description(&self) -> String;
    fn input_schema(&self) -> serde_json::Value;
    fn is_concurrency_safe(&self, input) -> bool { false }   // fail-closed defaults
    fn is_read_only(&self, input) -> bool { false }
    fn is_destructive(&self, input) -> bool { false }
    fn is_edit_tool(&self, input) -> bool { false }
    fn confirm_reason(&self, input) -> Option<String> { None }   // forces the prompt, unbypassable
    fn preview_diff(&self, input, cwd: &Path) -> Option<String> { None }  // dry run
    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, ToolError>;
}
```

- **Schemas** from `schemars` derive on a per-tool input struct → `schema_for::<T>()` (`:171`), which strips `$schema` and hoists `definitions` so `$ref`s don't dangle. Single source of truth; a drift test lives in the same file. `tool_params` (`:195`) emits `{name, description, input_schema}` for the request.
- **Registry is a `Vec<Box<dyn Tool>>`** assembled per turn by `tools::assemble_tools` (`src/tools.rs:23`) — no static registry; `find_tool` is a linear scan by name. Depth gates which tools exist (AskUserQuestion / AgentControl / Team only at depth 0).
- **Permissions integrate *outside* the trait.** The gate reads the trait's predicates but the decision is `query::gate_tool` → `permission::can_use_tool` → `AskFn`. Tools never see the verdict.
- **Results:** `ToolResult { content: serde_json::Value, is_error, diff }`. `content` is either a JSON string (text) or an array of Anthropic protocol blocks (text + images) — `result_block` (`src/query.rs:576`) passes arrays through and clips text blocks. `diff` is UI-only and never reaches the model.
- **MCP tools** (`src/mcp.rs:397`) implement the same trait; name is `mcp__{server}__{tool}` after `normalize_mcp_name` (`:415`); `readOnlyHint` maps to `is_concurrency_safe`/`is_read_only` but is explicitly *not* trusted by the permission gate.
- **Tool inventory** (18 builtin + MCP + depth-gated): Bash, Read, Glob, Grep, Edit, Write, WebFetch, WebSearch, Agent, TaskCreate/Update/Get/List, Skill, ExperiencePropose/Commit/Query/Outcome/Forget, SendMessage; +AskUserQuestion, AgentControl, Team, Channel at depth 0.

## 6. Persistence

Base: **`~/.local/share/bingo/`** (`storage::data_dir`, `src/storage.rs:62`) — not `~/.bingo`. Config: `~/.config/bingo/`.

| Path | Format | Canonical? |
|---|---|---|
| `transcripts/<project-slug>-<unix-ts>.jsonl` | **JSONL, one `Message` per line**, plus two marker line types: `{"type":"compact","summary","kept"}` and `{"type":"turn","at"}` | ✅ **This is the canonical record.** |
| `transcripts/<…>.jsonl.lock` | empty sidecar, `try_lock` held for the session | session mutex (D72 — the data file itself is never locked, because Windows locks are mandatory) |
| `attachments/<session>.assets.jsonl` | per-session `#[image N]` index | sidecar |
| `assets/<sha256>` | content-addressed image blobs, machine-shared, swept at 30d | sidecar |
| `session-assets/<epoch>/` | protocol working bytes, deleted at session close | ephemeral |
| `rewind/<session>/<checkpoint>/` | pre-images of files Edit/Write touched, hashed filenames; caps 50MB / 200 checkpoints / 8MB per file | sidecar |
| `rooms/<session>.rooms.jsonl` | room log | sidecar |
| `tasks/<key>/<id>.json` | one file per task | sidecar |
| `shares/<session>.json` | subagent/channel snapshot for `bingo share` | sidecar |
| `history/` | input history | sidecar |
| `logs/mcp-<name>.log`, `logs/panic.log` | plain text | ops |
| `models-cache.json` | fetched model lists, 24h TTL | cache |
| `~/.config/bingo/model-catalog.json` | `builtin` (rewritten on upgrade) + `overrides` (never written) | config |
| `~/.config/bingo/memdir/<dirname>-<fnv1a64>.md` | extracted project memory | sidecar |
| `auth.json` (0600) | OAuth tokens / stored keys, opencode-compatible | secrets |

- **Append-only.** `Transcript::append` seeks to end and writes one line (`src/transcript.rs:272`). Compaction *appends a marker*; canonical lines are never rewritten. Loading projects through the latest marker (`load_messages` → `project`, `:566`).
- **The one shortening op is rewind** — `truncate_at_line` (`:365`), which rewrites via `.jsonl.rewind` tmp + rename, then reopens and re-installs the lock.
- **Retention:** 30-day TTL / 100 sessions / 100 history files (`storage.rs:10-12`), `cleanup()` at `:178`.
- **Settings layering** (`settings.rs:362`): `$XDG_CONFIG_HOME/bingo/settings.json` → `<project>/.bingo/settings.json` → `<project>/.bingo/local.json`, field-wise `merge` (`:389`). Writes go to *the highest layer that already defines the key* (`upsert_scoped_settings`, `:558`).

## 7. Reusable vs tangled

**[clean — lift nearly as-is]**
- `src/api/contract.rs` (968) — the neutral seam is genuinely neutral; `NeutralRequest`/`StreamEvent`/`ProviderClient`/`AssistantAccumulator` have no upward deps except `crate::error::ErrorCode`. Its only impurity is the English-keyword error classifier.
- `src/api/sse.rs` (193) — incremental SSE parser with an 8MB guard and O(n) rescan. Zero deps, self-contained.
- `src/api/types.rs` (335) — but see caveat: it's the Anthropic wire format wearing a neutral name.
- `src/tool/mod.rs` (303) — the `Tool` trait is the best-designed thing in the repo: fail-closed defaults, schema derived from one struct, `preview_diff` as a dry run.
- `src/tool/executor.rs` (650) — safe-prefix batching + cancel-preserving `FuturesUnordered`; correct and small.
- `src/bm25.rs` (254) — zero-dep BM25 with CJK bigram tokenization. Self-contained.
- `src/context_usage.rs` (166), `src/budget.rs` (147) — pure functions over a `ModelResolver`; one ruler for display and compaction.
- `src/api/learned.rs` (215) — small, orthogonal, and a genuinely good idea (learn the window from the server's rejection).
- `src/platform.rs` (264) — narrow OS abstraction: shell selection, process-group/tree kill, TTY probe.
- `src/api/models.rs` (872) / `model_families.rs` (321) / `model_cache.rs` (199) — three-tier metadata resolution, catalog as a value not a global.
- `src/transcript.rs` (966) — append-only JSONL with a well-argued sidecar-lock design; the projection through compact markers is clean.
- `src/api/providers/{mod,presets}.rs` — the registry is the only place config→adapter is decided.

**[salvageable — good ideas, messy code]**
- `src/query_turn.rs` (427) — the retry policy and `AssistantAccumulator` drive are right, but `one_turn` has a **4-arm `tokio::select!` combinatorial expansion repeated three times** over `(cancel, inbox)` optionality. Collapse to always-present channels and it halves.
- `src/permission.rs` (1092, 472 prod) — rule semantics (deny/ask = any-subcommand, allow = all-subcommands, untrusted split never allows) are well-reasoned; but the shell splitter is hand-rolled and `:311` hardcodes `team::TEAM_FILE`.
- `src/compact.rs` (1329, ~806 prod) — `TokenGate` and the deterministic overflow rungs are good; but it imports `ui::ConvKey`, duplicates `permission_mode_str`, and mixes the summarizer, the token estimator, the gate, and the conversation-addressing all in one file.
- `src/hooks.rs` (770) — 10 hook events with a regex matcher cache and per-event timeouts; but every event is a separate `pub async fn run_*` with copy-pasted spawn/timeout/parse. One `run(event, payload)` would delete half of it.
- `src/mcp.rs` (1064, 447 prod) — lifecycle (lazy connect, `spawn_connect`, `drain_unreported_failures`, per-server stderr log) is thought through; the manager is `Arc<Mutex<…>>` reached through `Runtime`, which is why it needs the "drain only at depth 0" hack (`src/tools.rs:106`).
- `src/skills.rs` (813) — frontmatter parse + dir scan + budget-bounded listing; fine, but `expand_skill`/`substitute_arguments`/`replace_word_boundary` is a hand-rolled templating engine.
- `src/engine/events.rs` (400) — the `EngineEvent`/`EngineRequests` split is the right idea and the module doc argues it well; it's salvageable-not-clean only because it's the middle of a 4-enum chain that shouldn't exist.
- `src/api/client.rs` (1242, ~18 prod before tests… actually ~560 prod) — the facade is reasonable but has grown 15 accessors (`image_capable_providers`, `is_preset`, `learn_window`, …) that are `/provider`-menu concerns leaking into the API layer.
- `src/settings.rs` (1294, 663 prod) — three-layer merge is right; `merge` is 116 lines of hand-written field-by-field `if let Some`.

**[rewrite — tangled or over-built]**
- `src/query.rs` (1962 prod) — **the single biggest problem.** `query_loop` (`:915`–`:1490`) is a 575-line function that does inbox drain, compaction, task reminders, hire release, notification injection, mail drain, model call, five distinct end-branches, permission gating, tool execution, steering, and interrupt bookkeeping. It reaches 20 modules. This is where a rewrite pays most.
- `src/query_session.rs:69` `Session` — 27 fields, 10 of them actor handles. It is passed as `&Arc<Session>` to everything, which is what creates the `query ⇄ app ⇄ tool` cycles.
- `src/error.rs` (624) — the error-code *registry* is a good idea, but `error_code_boxed`/`downcast_error_code` (`:190-222`) downcasts against ~14 concrete error types, inverting the dependency graph. Use a trait object with `ErrorCode` supertrait instead.
- **The `query` / `engine` / `app` triangle.** `app` (595 in mod + `controller.rs` 3050 + `snapshot.rs` 1563 + `projection.rs` 1251 + `action.rs` 2012 + `turn.rs` 1755 …) is the newer layer (2026-08-18, 10 days after `query.rs`); `engine` is a 3-file shim between them (`mod` 11, `events` 400, `runner` 408, `actions` 1110). Nothing was deleted when `app` landed — `query.rs` kept its own loop, `ui::UiEvent` kept existing, and `engine` was added to bridge. Three loop-adjacent vocabularies now coexist.
- `src/engine/actions.rs` (1110) — "13 of 28 actions need a model/network/rewrite" — it's a giant `match` dispatching compact/mcp-reconnect/team/share/login/rewind/rename/reset, each with its own ad-hoc `Said { tier, text }` return. This is a command bus wearing a function's clothes.
- `src/app/action.rs` (2012) + `src/app/command.rs` (638) + `src/app/snapshot.rs` (1563) — the app-server protocol surface is fully built out (44 notification kinds, JSON-schema-derived) for a GUI that isn't the product yet.
- `src/tool/agent.rs` (2089) + `tool/team.rs` (1220) + `tool/channel.rs` (907) + `team.rs` (3151) + `channels.rs` (2571) + `agents.rs` (2498) — out of scope per your brief, but they are the reason `Session` has 10 handles and `query_loop` has an inbox drain, a mail drain, a hire release and a notification pass in its hot path. **The multi-agent feature is not separable from the engine as written.**
- `src/tool/bash.rs` (1922, 1126 prod) — a tool file that also owns interactive-command rejection heuristics, periodic-command interval detection, output-tail sampling, and background promotion. Split it.

## 8. Sizes (lines; `_tests.rs` files listed separately)

**api/** — `client.rs` 1242 · `auth.rs` 1230 · `contract.rs` 968 · `models.rs` 872 · `image.rs` 456 · `types.rs` 335 · `learned.rs` 215 · `sse.rs` 193 · `mod.rs` 9 · `providers/openai.rs` 1906 · `providers/anthropic.rs` 1150 · `providers/mod.rs` 315 · `providers/presets.rs` 47 — **8,938**

**query/** — `query.rs` 1962 · `query_turn.rs` 427 · `query_session.rs` 157 — **2,546** (+ `query_tests.rs` 2852, `query_steer_tests.rs` 129)

**engine/** — `actions.rs` 1110 · `runner.rs` 408 · `events.rs` 400 · `mod.rs` 11 — **1,929**

**tool/** — `agent.rs` 2089 · `bash.rs` 1922 · `team.rs` 1220 · `channel.rs` 907 · `experience.rs` 879 · `task.rs` 762 · `executor.rs` 650 · `grep.rs` 563 · `read.rs` 552 · `webfetch.rs` 499 · `websearch.rs` 413 · `ask.rs` 408 · `glob.rs` 386 · `mod.rs` 303 · `edit.rs` 258 · `agent_notes.rs` 250 · `skill.rs` 236 · `diff.rs` 201 · `write.rs` 175 · `address.rs` 134 — **12,807** (+ `agent_tests.rs` 2482) · `tools.rs` (assembly) 462

**gate/hooks/mcp/skills** — `permission.rs` 1092 · `mcp.rs` 1064 · `skills.rs` 813 · `hooks.rs` 770 · `preapproved.rs` 166 — **3,905**

**context/budget** — `compact.rs` 1329 · `token_rate.rs` 359 · `context_usage.rs` 166 · `budget.rs` 147 — **2,001**

**memory** — `memory.rs` 221 · `bm25.rs` 254 — **475**

**persistence** — `transcript.rs` 966 · `storage.rs` 666 · `rewind.rs` 514 — **2,146** (+ `rewind_tests.rs` 568)

**config/prompt/infra** — `settings.rs` 1294 · `error.rs` 624 · `system.rs` 545 · `model_families.rs` 321 · `platform.rs` 264 · `model_cache.rs` 199 — **3,247**

**support the engine drags in** — `watch.rs` 1647 · `tasks.rs` 637 · `live.rs` 524 · `ui.rs` 474 — **3,282**

**Engine-side total surveyed ≈ 41,700 lines** (excluding `app/` 14,077, `app_server/` 6,500, `tui/` ~40,000, `team/channels/agents` ~8,200, `share*` ~2,300).

---

### One-paragraph verdict for the rewrite

The provider seam (`api/contract.rs` + two adapters), the `Tool` trait, the executor, the transcript format, and the budget/context math are all keepers — that's roughly 8–10k lines that could move nearly verbatim. Everything above them is one 575-line loop function and a god-`Session`, wrapped in a three-layer event chain (`StreamEvent → EngineEvent → AppEventPayload → UiEvent`) that exists because `app/` was added on day 10 without deleting what it replaced. The highest-leverage rewrite is: (a) split `query_loop` into a state machine over an explicit `TurnState`, (b) shrink `Session` to what a run actually reads and pass the actor handles as a separate `Host`, (c) collapse the event chain to two enums (wire + one application event), and (d) make the multi-agent registries a *consumer* of the loop rather than five injection points inside it.
