# Collaboration layer, app-server protocol and architecture health of the old bingo (survey, 2026-08-28)

> Source: subagent report, archived verbatim. Facts were verified on the date in the title; re-verify before depending on a version.

# bingo — architecture survey (read-only)

**Scale:** 148,754 lines Rust across 208 files, **single binary crate** (no `lib.rs`, no `[workspace]`, empty `[dev-dependencies]`). 456 commits. `src/tui/` 54,860 (37%) · `src/app/` 21,731 (15%) · `src/tool/` 15,289 · `src/api/` 8,938 · `src/app_server/` 7,808 · `src/engine/` 1,929.

---

## 1. Collaboration domain

### Entity model (one sentence, one owner)

| Entity | Owner | What it is |
|---|---|---|
| `AgentDef` | `src/agents.rs:32` | On-disk persona template (`.bingo/agents/<name>.md`, frontmatter + body prompt), layered project > user. |
| `AgentDefSource` | `agents.rs:23` | Which layer a def came from, for the `/team list` badge. |
| Instance (`Entry`) | `agents.rs` (~410, private) | One live child session: history, stamps, inbox, acks, abort handle, run counter, watch id, progress. |
| `AgentRegistry` | `agents.rs:559` | The actor-owned table of instances; every mutation is an `AgentMsg` (`agents.rs:1733`) answered via `Answer`. |
| `AgentKind` | `agents.rs:172` | `Crew` (pinned in `team.json`, permanent) vs `Hire` (model-spawned, released after `HIRE_LEASE = 2` sweeps). |
| `AgentState` | `agents.rs:145` | `Running` / `Idle` / `Stopped` — a stopped instance keeps history and revives on a DM. |
| `MsgId`/`Ack`/`AckState`/`FollowUp` | `agents.rs:250/292/263/325` | Per-direct-message delivery ledger: `Queued → Delivered{run} → Answered{run}` or `Dropped`, chased 3× by `spawn_ack_watchdog`. |
| `InboxItem` | `agents.rs:338` | Five kinds of thing that can land in an instance's inbox: `Direct`, `Channel`, `FollowUp`, `Unanswered`. |
| `Wake` | `agents.rs:395` | A claim ticket: an idle instance with a non-empty inbox, already flipped to Running so two flushes can't double-start it. |
| `Continuation` | `agents.rs:405` | The inbox that refilled *during* a run, folded into the next round without a new spawn. |
| `Roster` / `AgentProgress` | `agents.rs:536/458` | Display projections of the registry. |
| `TeamDef` / `TeamMember` / `Room` / `ChannelSpec` / `TeamRef` | `src/team.rs:171/154/135/77/126` | The blueprint in `.bingo/team.json`: named members pointing at `AgentDef`s, rooms, and child teams in other directories. |
| `TeamTree` / `TeamNode` | `team.rs:380/367` | The org chart — a blueprint can name child blueprints; names are unique tree-wide so `@bare-name` addresses from anywhere. |
| Team memory | `team.rs:976–1530` | Per-(project-path-hash + git branch + team) directory holding member history, a readable transcript, and a decisions log. |
| Norms | `.bingo/team-norms.md`, `team.rs:829` | Prose working agreement injected into every member's system prompt. |
| `ChannelRegistry` / `ChannelMessage` / `ChannelMode` | `src/channels.rs:506/99/59` | Rooms: a member list, `serial`\|`free` staleness check, runtime sender stamping, budget freeze. |
| `Mention` / `MemberStanding` | `channels.rs:275/306` | The `@` debt ledger (v7 batch 3 / D131) and the per-member read cursor. |
| `main_mail` / `main_arrivals` | `channels.rs:452/467` | Main's inbox: the byte-contract copy the model sees, and the display copy the UI draws. |
| `ConvKey` | `src/app/conversation.rs:32` | `Main` \| `Agent(name)` \| `Room(name)` — the process-internal conversation name. |
| `ConversationId` / `Conversations` | `app/conversation.rs` | The opaque client-facing id and the map where the two meet; carries `revision` + `history_generation`. |
| `Item` / `ItemBody` (14 variants) | `app/snapshot.rs:500/515` | One completed unit in a conversation log — prose, tool call, peer message, compaction… |
| `Turn` / `TurnGuard` | `app/turn.rs:221` | One run, closed exactly once, even when its task is aborted (`Drop`). |
| `QueuedInput` / `SteerItem` | `app/queue.rs:64/94` | A line typed while busy; the eligible **prefix** may be absorbed at a tool barrier. |
| `Attention` (cursors) / `Obligation` | `app/attention.rs` | Per-conversation read cursor; unread is *derived by subtraction*, never counted. |
| `MailWake` | `app/mail.rs` | The digest debounce: 2s quiet window, 15s deadline; urgent bypasses both. |
| `Interaction` | `app/interaction.rs` | A run stopped on a permission prompt or a question; survives reconnect, answered once. |
| `Operation` | `app/operation.rs` | Async non-turn work (team start, provider login, MCP reconnect) with one terminal state. |
| `Run` / `Act` / `trait Engine` | `app/engine.rs:28/64/101` | The one-way seam: work the actor accepts but cannot do (`Turn`, `Shell`, `Wake`, `Posted`, `Promote`, `Interrupt`, `Act`). |
| Room sidecar | `app/roomlog.rs` | Append-only `{stem}.rooms.jsonl` so rooms and unread marks survive a restart. |
| Attribution walk | `app/projection.rs` (1,251) | Recovers "who said what" from one agent's own transcript — the one walker both frontends use. |
| `Task` / `TaskStore` | `src/tasks.rs:34/64` | One JSON file per task, keyed by project. |
| `ExperienceEntry` | `src/experience.rs:50` | Cross-session lesson library with `active/degraded/stale` status and BM25 query. |

### Walkthrough: main dispatches a subagent → subagent replies → main wakes

1. `tui::chat::Chat::submit` sends an `AppRequest::Submit` to the actor; no frontend decides what the line means.
2. `Controller::serve_submit` (`app/controller.rs:2182`) calls `app::submit::compose` (slash / shell / `@name` / `#room` / prose) then `route`.
3. Prose on main while idle → `perform_turn_from` opens a `TurnId`, mints an `ItemId` for the user prose, publishes `turn/started` + `item/*`, and hands `Run::Turn{turn,text}` to the attached `Engine`.
4. `engine::runner::SessionEngine::run` (`runner.rs:324`) spawns the query loop; **nothing** comes back through the return value — only `EngineEvent`s into the turn.
5. Model emits `tool_use` for `Agent`; `permission::can_use_tool` gates it; `tool::agent::AgentTool` (`tool/agent.rs:84`) runs.
6. `spawn_instance` (`agent.rs:947`) builds a child `Session` (own model/provider/thinking/cwd) and registers an `Entry` as `AgentKind::Hire` — the tool can never create a `Crew` member.
7. `spawn_agent_loop` (`agent.rs:770`) spawns the child's own query loop plus a `watch.rs` line so the roster can render `● name #1 · <excerpt>`.
8. `background:false` blocks the parent tool call on `task.wait()`; `background:true` returns at once and the parent hears back only via mail.
9. The child's `EngineEvent`s carry `ConvKey::Agent(name)`, so they enter the *same* actor and become items on the instance's own conversation — an instance has a page exactly like main's (D134).
10. On success `AgentRegistry::finish` commits the messages to `Entry.history` and returns a `Continuation` if the inbox refilled mid-run; otherwise the loop breaks and the instance goes `Idle`.
11. On failure `mark_failed` sets `Entry.held` (D187) so the same poisoned batch cannot re-wake the instance; only new mail lifts the hold.
12. To answer, the child calls `SendMessage(to: "main")` → `to_main` (`agent.rs:1663`) → `ChannelRegistry::deliver_to_main` (`channels.rs:1394`), which pushes the byte-contract line into `main_mail` **and** a display copy into `main_arrivals`, and moves the sender's `Ack` to `Answered`.
13. The actor calls `consider_mail()` (`controller.rs:2708`) after every message; `MailWake::observe` debounces (2s quiet / 15s deadline) and raises a `feedback/raised` "mail waiting" notice in the meantime.
14. When `mail.due()` **and** `free_to_wake()` (no running main turn, empty main queue) → `wake_main()` (`controller.rs:2699`) opens an ordinary turn with **empty prose** and `TurnOrigin::Auto`.
15. That turn's query loop drains `channels.drain_main_mail()` at the top of the round (`query.rs:1010`) and injects the batch as a `<messages>` envelope — waking is *reading your inbox as context*, not a special code path.
16. If main was already running, the same drain happens at the next tool boundary: input tokens, zero extra model calls. That is v7's "a running agent never wakes."
17. Meanwhile `spawn_ack_watchdog` (`agent.rs:470`) chases an unanswered DM up to `MAX_FOLLOW_UPS = 3`, injecting `InboxItem::FollowUp`; `channels::Mention` does the same for an unanswered `@` in a room after 5 minutes.
18. The inbound counterpart is `flush_agent_inbox` (`agent.rs:430`): `flush_pending().now()` atomically claims every idle instance with mail — this is what `Run::Wake` (`runner.rs:331`) triggers.
19. Rendering: `tui/store.rs` folds the `AppEvent` stream into a `View`; `tui/roster.rs` draws one row per conversation with badges; `tui/conv.rs` + `app/projection.rs` attribute the instance's page.
20. Persistence: main → `transcript.rs` JSONL; rooms → `app/roomlog.rs` sidecar; crew member history/transcript/decisions → `team_memory_dir` (`team.rs:1021`).

### Judgment: essential vs. social simulation

**Collaboration domain size:** ~21,100 lines (`agents.rs` + tests 4,053 · `team.rs`/`team_cmd.rs` 4,123 · `channels.rs` 2,571 · `tool/{agent,agent_tests,agent_notes,address,channel,team}` 7,082 · `experience` 1,893 · `tasks` 1,399) — **~14% of the crate**, plus the parts of `app/` (`mail`, `attention`, `roomlog`, `projection` ≈ 2,400) that exist only for it, plus `tui/{roster,buffer,bufferview,zoom,tree,avatar}` ≈ 3,900. Call it **~27,000 lines, 18% of the codebase.** Of 24 tools, **14 are collaboration/bookkeeping** (Agent, AgentControl, SendMessage, Channel, Team, 5× Experience, 4× Task) and only 10 do coding work.

**Essential** (any real coding agent needs these, ~6,000 lines):
- Spawning a child session with its own context window and getting a string back. That is `AgentTool` + `build_sub_session` + the child query loop.
- A named-definition file format so a subagent has a persona and a tool subset.
- Task list (Claude Code's TodoWrite equivalent).
- Permission inheritance and MCP inheritance into the child (both were bugs; see research.md:484).

**Elaborate social simulation** (the rest, ~20,000 lines):
- **Rooms/channels with serial-vs-free optimistic locking, staleness bounce, per-member read cursors, budget freeze.** This is Slack. `channels.rs` is 2,571 lines to give LLMs a group chat with total ordering. The design doc itself admits the primitive count is four and "everything else is prompting."
- **The `@` obligation ledger.** `conversation-model-v7.md` spends a full page defining seven duties (R1–R7) that are *prompt text*, backed by `Mention` records, a 5-minute chase timer, and a follow-up budget. This is HR policy encoded as a distributed-systems protocol. R3 ("never `@` the person you are answering") exists to stop model ping-pong — a problem created by the room, not solved by it.
- **Crew vs. Hire lifetimes**, `HIRE_LEASE = 2` sweeps, `Refresh::{Refreshed,Unchanged,Busy,Hired,Missing}`, team memory keyed on project-hash + git branch, `team-norms.md` as a checked-in prose contract, `TeamTree` org charts spanning directories. `team.rs` is 3,151 lines to describe five names in a JSON file.
- **`@user` relay semantics (R7a/b/c)** — main holds a debt to a human on behalf of a room, must quote verbatim, may answer for the user but must disclose it. Three clauses of protocol for something a coding agent could do by printing the line.
- **Avatars** (`tui/avatar.rs`, 544 lines) — and `src/tool/team.rs:329` reaches into `crate::tui::avatar::ids()` so the *model* can pick a face for a teammate.
- **The experience library** (1,893 lines, 5 tools, BM25 index, active/degraded/stale lifecycle, outcome recording). A speculative memory system with no evidence of payoff in the notes.

**The tell:** the last five decisions in `research.md` (D185–D189, all 2026-08-27/28) are *all* bugs in this layer — a failed subagent batch retried 19 times in one minute (D187), 19 identical `⚠` alerts with a bell each (D186), Esc dead for the rest of a main turn once it dispatched an Agent (D188), a whole-session store resync per dispatch (D189). The social layer is where the defects live, and it is still producing them 20 days in.

**Rewrite recommendation:** keep sub-agent spawn + named defs + task list (~6K lines). Cut rooms, the `@` ledger, teams/norms/org-charts, the ack watchdog, avatars, and the experience library. If group coordination is genuinely wanted later, it is a *product* to build on a stable base, not a foundation.

---

## 2. App-server protocol

**What it is.** JSON-RPC 2.0, one JSON object per line (NDJSON) on stdin/stdout, stderr for diagnostics only. `bingo app-server` (`src/main.rs:140`). Spec: `notes/design/gui-app-server.md` (53KB) + `gui-app-server-plan.md` (46KB).

**Surface** (from `schema/app-server/manifest.json`, protocol 1.0, `bundleVersion: 1`):

- **23 methods:** `initialize` · `shutdown` · `session/{list,start,resume,read,close,delete}` · `conversation/{list,read,markRead,submit}` · `turn/interrupt` · `queue/{read,reclaimTail}` · `interaction/respond` · `action/{list,execute}` · `config/read` · `catalog/read` · `resource/read` · `asset/{registerPath,readChunk}`
- **39 notifications:** `session/{updated,closed,deleted}` · `conversation/{created,updated,removed}` · `turn/{started,roundStarted,retrying,roundCompleted,usageUpdated,completed}` · `item/{started,textDelta,reasoningDelta,commandTailUpdated,updated,completed}` · `queue/{itemAdded,itemRemoved,itemAbsorbed}` · `interaction/{opened,resolved,cancelled}` · `agent/{changed,removed}` · `room/changed` · `task/{changed,removed}` · `delivery/changed` · `command/changed` · `operation/{started,progress,completed}` · `config/changed` · `catalog/changed` · `asset/available` · `feedback/{raised,cleared}`
- **19 declared errors**, 5 envelopes, 90-file Draft-7 bundle generated deterministically from the Rust types (`app_server/schema.rs`), CI-checked for drift.

**Design properties that are genuinely good:** server-owned opaque ids; gapless `seq` with `coalescedFrom` spans; snapshot-cut + event-stream recovery (a hole triggers a re-read, never a patch); one submission path (`conversation/submit` decides turn/queue/steer/deliver — the client never chooses); server-initiated interactions that outlive the call and survive reconnect; credentials never cross the boundary (`app/catalog.rs` drops the plaintext key on the floor); `AppEvent → ServerNotification` is total in both directions with a test pinning it.

**Built for:** a GUI. `notes/json-events-gap-analysis.md` §0 states the benchmark explicitly — Codex's `app-server` tier (rich client: deltas + approval round-trips + full observability), *not* Codex's `exec --json` tier.

**Complete?** As a contract, yes: all 23 methods are implemented, 55 functions / 23 `#[test]`s in `tests/app_server_black_box.rs` (1,770 lines) drive a real subprocess through a scripted loopback Anthropic endpoint, including `the_print_client_runs_one_turn_through_the_same_core` and `a_client_can_walk_the_whole_session_free_surface_in_one_connection`. Excluded from 1.0 by the spec: two clients on one session, a durable event journal, network transports, provider-native frames.

**But it has zero consumers.** README (line ~899): "it has no released consumer yet, so it carries no compatibility promise." `bingo-site/` is a Next.js marketing + share-page site — grep for `app-server|jsonrpc|app_server` there returns nothing. **7,808 lines of protocol code + a 90-file schema bundle serving no client.**

**How much of `app/` exists only for it?** Almost none, and that is the redeeming fact. `AppCore` became the *only* place session truth lives — the TUI (`store.rs`), `--print` (`print.rs`), and the app-server are three clients of the same actor. `app/parity.rs` (620 lines, 137 checked rows) enforces that. The honest accounting: `app/` (21,731) would be needed at roughly 70% of its size for the TUI alone; `app_server/` (7,808) is pure speculative surface. **The app-server did not bloat the core — it justified extracting one.** That extraction is the single best thing in the codebase.

---

## 3. The in-flight migration (D140 → D155)

- **Goal.** Delete the old `--json-events` protocol v1 and replace the whole frontend boundary with an `AppCore` session actor that owns application truth, publishing a sequenced event stream that the TUI, a GUI, and `--print` all consume as *projections with no rules of their own*. Nine batches, B0–B8.
- **B0 / D140:** deleted `src/json_events.rs` (1,836 lines) whole, plus 4 flags and 18 tests, **before** building the replacement. The stated reason: nothing consumed it, and keeping it alive for a 9-batch rewrite means a second contract every batch must keep true. It also removed six `if !cli.json_events` startup gates that had quietly made the JSON frontend a *different product* (no share store, no team auto-start, no team-memory persistence).
- **B1 / D141:** the contract first — 16 id types, 39 events, 28 actions, 23 methods, ~170 exact-JSON round-trip fixtures, mounted and uncalled with `allow(dead_code)` naming the batch that removes it. Caught two real bugs by generating the schema (a flattened `ItemBody::ToolCall.status` colliding with the item's own `status`; serde/schemars disagreeing on `rename_all_fields`).
- **B2–B3 / D142–D144:** the actor, the id mint, the snapshot barrier, then turns, queue, routing, prompts.
- **B4 / D145:** the collaboration domain moves in — items, attention cursors, and rooms that survive restart (`app/roomlog.rs`).
- **B5 / D146:** five hand-kept copies of the command table collapse into one (`app/action.rs`, 2,012 lines). They had drifted: `/?` and `/quit` dispatched but were in no table; `/team` advertised 5 of 9 subcommands; the argument dropdown knew 5 of 24 commands.
- **B6–B7a / D147–D148:** the stdio transport, then the engine reaching the wire.
- **Wall #1 (D149) — the test runtime.** "570 of the console's ~640 tests are `#[test]`, not `#[tokio::test]`." `AppCore::attach` spawned a forwarder on `Handle::current()`, so a runtime-less test attached to nothing and read an empty projection. The read-face swap was inseparable from a test migration, and the migration was not a `sed` (several tests were written *because* there is no runtime). Resolved by ruling ② — make `attach` runtime-free.
- **Wall #2 (D150) — the console had no engine.** One forced chain: no engine → no `item/*` published → cannot render from `AppEvent` → `tui_hooks`/`subagent_hooks` stay → console runs the loop itself → 15 `Answer::now()` sites stay → `/model` writes `runtime.model_tx` and the core is never told → the config double mirror stays.
- **Wall #3 (D151) — the submission path's shape.** Three real blockers: a slash command's *view* is the frontend's so the console can't use `serve_submit`; a key handler is `fn` and a receipt is `async` (10 of the 15 `.now()` sites are writes from a keypress needing the answer to draw the next line — removing them means an intent queue, and ~130 `chat.submit()` sites assume immediacy); and the digest wake had no door (`compose` reads an empty line as `Composed::Empty`).
- **Wall #4 (D152) — the half of the action table that never moved.** `apply_action` implemented **14 of 28** actions; the 13 needing a model, a transcript rewrite or a network round-trip lived in `tui::chat::run_command`. So `/compact` queued behind a running turn would drain into a silent `ActionUnavailable` the moment the engine attached. Work was carried far enough to prove this, then **reverted**. Three exits were costed; the user chose (a): move them into the core.
- **D153 (B7d-2):** the 13 actions move into `engine/actions.rs` (1,110 lines). `Availability::engine_attached` stops being a constant.
- **D154:** the write face lands. Shims **gone**: `Answer::now()` in `src/tui/` production (verified: 0 outside `#[cfg(test)]`), the console's run loop, `tui_hooks`/`subagent_hooks`, the config double mirror, `Chat::permission_mode` as a field, `Chat::live`/`cancel_tx`, the dual queue drain.
- **D155 (B8) — the campaign closed.** `app/parity.rs` ledger (137 rows, 5 inventories, 2 checked by exhaustive `match` = compile errors); `--print` becomes the third client (`print.rs`); 10 small accounts closed. **`rg "B7 removes this"` returns 0. `rg -i shim` in `src/` returns 4 hits, all comments explaining why something is *not* a shim.**
- **Status: the migration finished.** This is the unusual part — a 15-batch architectural rewrite executed inside 20 days, walls named in writing, one batch reverted rather than forced, and the endpoint verified. What remains is by ruling, not debt: `Answer::now` still has ~19 production sites in `tool/` and `team*.rs` (blocking calls from `spawn_blocking` workers into the actor, documented and asserted safe in `app/answer.rs`); `ItemBody::Command` for a standalone `!` run is a known issue; `tui/store.rs` keeps `#![allow(dead_code)]` as a read-face inventory.

**Gap analyses.** `notes/cc-gap-analysis.md` (Chinese, 16KB) rates bingo *ahead* of Claude Code on the error-code contract and roughly isomorphic on the query loop, tool contract, concurrency partitioning, and permission gate. P0 gaps it identified: memory lifecycle bugs (tail truncation comment inverted; facts silently dropped after 200 lines), no typed interrupt reason / per-tool `InterruptBehavior` (a remote write dropped mid-flight is in an unknown state — a real safety hole), a `unreachable!` panic path in the hook `ask` route, no compaction observability, no `microcompact`, no turn budget, no fullscreen virtualization. **Most of these are still open** — the 15 batches went into the frontend boundary, not into the agent loop. `notes/json-events-gap-analysis.md` is superseded; its matrix was absorbed into the parity ledger.

---

## 4. Duplication list

| Concept | Representations | Verdict |
|---|---|---|
| **Events** | `api::contract::StreamEvent` (`api/contract.rs:343`) → `engine::events::EngineEvent` (12 variants) → `app::event::AppEventPayload` (39) → **two** sinks: `ServerNotification` (39, `app_server/protocol/notifications.rs`) and `ui::UiEvent` (27, `src/ui.rs`) | 4 layers. The first three are justified (provider → engine → application). `UiEvent` is a 27-variant terminal render vocabulary translated from `AppEvent` in `tui/chat_feed.rs`. The parity ledger classifies only **9 of 27** as legitimately frontend-local — the other 18 are re-encodings. |
| **Background-command / agent / task state** | `watch.rs` (`WatchState`, `WatchSnapshot`, broadcast) **and** `command/changed` + `agent/changed` + `task/changed` on the wire | D155 names this itself: "Shared state, two read paths." The console reads the registry broadcast; a GUI reads the store. |
| **Unread / attention** | `app/attention.rs` (cursors, derived subtraction) **and** `tui/buffer.rs::Buffers::refresh` (935 lines) | **Live duplication.** `Buffers::refresh` reads `session.channels.list()`, `session.channels.log_of()` and `session.agents.list()` *directly from the registries* (comment at `buffer.rs` ~line 520 admits it: "The one roster read that stays on the registry (B7c)"). Two answers to "is this unread." |
| **Conversations** | `app/conversation.rs` (`ConvKey`, `Conversations`) · `app/snapshot.rs::ConversationSummary` · `tui/store.rs::View` + `Transcript` · `tui/conversation.rs` (285) · `tui/conv.rs` (497) · `tui/buffer.rs` (935) · `tui/bufferview.rs` (798) | Seven files touching one noun. `conv.rs`/`conversation.rs`/`buffer.rs`/`bufferview.rs` are four successive design generations (D88 → D103 → D130/132 → D134) whose survivors were kept rather than merged; each header explains what it *used to be*. |
| **Message** | `api::types::Message` · `app::snapshot::Item`/`ItemBody` (14) · `channels::ChannelMessage` · `agents::InboxItem` (4) · `tui::chat::UiMessage` | Five shapes for "a thing someone said." |
| **Transcript** | `transcript.rs` (session JSONL) · `app/roomlog.rs` (room sidecar JSONL) · `team.rs:1384 fn transcript()` + `member_transcript_path` (per-member readable dump) · `tui/transcript.rs` (the `ctrl+o` pager) · `share.rs` + `share_html.rs` (2,264 lines, HTML export) | Five formats. The team member transcript is a *third* on-disk record of the same conversations. |
| **Directory helpers** | `transcript.rs:79 transcripts_dir` **and** `storage.rs:66 transcripts_dir` | Two functions, same name, same purpose. |
| **Conversation address** | `ui::ConvKey` is a `pub use` re-export of `app::conversation::ConvKey` (`ui.rs:108`) | Not a real duplicate, but it means `compact.rs` and `tool/agent.rs` import a core type *through the UI module*. |
| **Command table** | `app/action.rs::COMMANDS`/`ACTIONS` and `tui/slash.rs` (147 lines) | **Fixed** by D146 — `slash.rs` is now ranking only. Worth noting as the one duplication that was successfully collapsed (from five copies). |

---

## 5. Layering violations

Real ones (module boundary crossed against the stated direction):

- **`src/tool/team.rs:329` → `crate::tui::avatar::ids()`** — a model-facing tool reaches into the terminal renderer to list avatar names. Domain → UI.
- **`src/tool/diff.rs:176,187,198` → `crate::tui::activities::Diff::parse_unified`** — the diff tool parses unified diffs using the TUI's activity renderer. Domain → UI.
- **`src/tool/agent_tests.rs:1985` → `crate::tui::store::Store::open`** — the agent tool's tests instantiate the terminal's projection to assert domain behaviour.
- **`src/compact.rs:357,1278,1282` → `crate::ui::ConvKey`** — compaction imports its conversation key through the renderer-agnostic UI module rather than from `app::conversation`. Same at `src/tool/agent.rs:14`.
- **`src/app/parity.rs:36` → `crate::ui::UiEvent`** — the core imports a terminal enum. This one is deliberate (the ledger must enumerate frontend-local events) but it makes `app/` un-compilable without `ui.rs`.
- **`src/app/projection.rs:28,370,479`** — doc-comments in the core reference `crate::tui::conv` and `crate::tui::buffer::stamp` as authorities for behaviour.
- **`src/tui/buffer.rs`, `activities.rs`, `background.rs`, `roster.rs`, `chat.rs`, `chat_tail.rs`, `conv.rs`** import `crate::{query,channels,agents,watch,permission,transcript}` directly (non-test: 7 `query`, 9 `channels`, 6 `agents`, 4 `watch`, 6 `permission`). Post-migration these should all go through `store.rs`. Counted per target module (non-test TUI files): `tui` 118 · `app` 33 · `ui` 9 · `channels` 9 · `query` 7 · `permission` 6 · `agents` 6 · `api` 5 · `watch` 4 · `engine` 4.

**Verdict:** the TUI is a client of `app::AppCore` via `tui/store.rs` — that is the dominant path (33 `app` imports, all `.now()` gone from TUI production). But ~30 non-test import sites still reach the registries and `query::Session` directly. The violations that would concern me most in a rewrite are the **`tool/` → `tui/`** ones, because they mean the tool layer cannot be extracted into a crate without dragging ratatui along.

---

## 6. Test story

- **1,844 tests**: 1,481 `#[test]` + 363 `#[tokio::test]`. Distribution: `src/tui/` **807 (44%)** · `src/tool/` 176 · `src/app/` 161 · `src/api/` 112 · `src/app_server/` 61 · `tests/` 36 · `src/engine/` 5.
- **Structure:** almost everything is an inline `#[cfg(test)] mod` in the production file. `src/tui/chat_tests_a..g.rs` (**14,507 lines, ~10% of the crate**) are `#[path]`-mounted modules inside `chat.rs:3896–3920` — the `a..g` suffixes carry no meaning, they exist purely to stay under the 4,000-line cap in `scripts/check_discipline.sh`. `chat.rs` also has `#[cfg(test)]` methods interleaved inside production impls (lines 1303, 1315, 2327, 2334, 2342, 2470, 2643, 2672…).
- **Integration tests are black-box by force.** There is **no `lib.rs`**, so `tests/*.rs` cannot `use bingo::…`; both files spawn `env!("CARGO_BIN_EXE_bingo")` as a subprocess (`tests/app_server_black_box.rs:73,1165`; `tests/cli_black_box.rs:34`). `app_server_black_box.rs` (1,770 lines, 23 tests, 55 helper fns) runs a scripted Anthropic-protocol TCP endpoint on loopback with an isolated `HOME`. Genuinely high-value tests — and structurally the only kind available.
- **Quality is high.** Test names are English sentences (`a_stream_retry_withdraws_the_attempt_it_lost_and_re_enters_the_round`, `the_catalogs_answer_before_a_session_and_never_carry_a_key`). ~170 exact-JSON round-trip fixtures. Schema-drift guard. 137-row parity ledger, 2 inventories checked by exhaustive `match` (compile errors). Real-terminal tmux smoke runs recorded in `research.md` for every batch.
- **Runtime estimate for `cargo test --locked --all-targets`:** test *execution* is probably 30–90 s (71 `sleep()` calls, longest 800 ms; ~640 TUI tests each spawn an actor OS thread; two black-box files spawn real processes). **Compilation dominates.** A single 148K-line binary crate with ratatui + reqwest + rmcp + image + synoptic + schemars means: cold ≈ 5–10 min on Apple Silicon; incremental after touching any `src/app/` file ≈ 1.5–4 min, because **every test in the crate relinks with it**. The 13 GB `target/debug` is consistent with that. There is no way to test `app/` without compiling `tui/`.
- **The gate is currently red.** `src/tui/chat_tests_b.rs` is **4,144 lines**, over the 4,000-line cap `scripts/check_discipline.sh` enforces. `chat.rs` (3,921), `chat_tests_a.rs` (3,915) and `chat_tail.rs` (3,645) are all pressed against it. D155 recorded account (h) as "**measured**, cap holds" — it no longer does.

---

## 7. Top 5: where the vibe went wrong

**1. A social-simulation layer was built before the coding agent was finished.**
~27,000 lines (18%) go to teams, rooms, the `@` obligation ledger, ack chasing, avatars, norms, org charts and an experience library. Meanwhile `notes/cc-gap-analysis.md` lists as **P0, still open**: memory lifecycle correctness bugs, no typed interrupt reason or per-tool `InterruptBehavior` (a remote write dropped mid-flight is in an unknown state — the doc calls it "一个安全缺口"), a `unreachable!` panic path in the hook `ask` route, no compaction observability, no `microcompact`, no turn budget. `channels.rs` (2,571 lines) got a serial/free optimistic-locking protocol with staleness bounce before `compact.rs` learned to report before/after token counts. And the last five decision records (D185–D189, both of the final two days) are *all* bugs in the social layer — 19 identical failure alerts in one minute (D186/D187), Esc dead after a dispatch (D188), a full-session store resync per dispatch (D189).

**2. The design documents outgrew the code they describe.**
`notes/research.md` is **10,423 lines / 856 KB** — 189 decision records, one per batch, each a full essay with problem/decision/consequences/verification/"what stayed deliberately"/"not verified". `notes/design/feedback-states.md` is **210 KB**. `gui-app-server.md` + its plan are **99 KB**. `conversation-model-v7.md` defines seven duties (R1–R7) with three sub-clauses on R7 for how main relays a question to a human. This is extraordinary discipline *and* the clearest symptom: when a design doc needs 14 KB to explain why `@` means "I need your answer," the feature is too clever. The commit titles are literary (`the clock moves the chrome, not the transcript`) — which reads beautifully and makes `git log` unsearchable by subsystem.

**3. The whole product is one binary crate.**
No `lib.rs`, no `[workspace]`, no `[dev-dependencies]`. Consequences, all load-bearing: integration tests must spawn the binary as a subprocess (that's *why* both `tests/` files are "black box"); touching `src/app/` relinks all 148K lines and all 1,844 tests; `app/` cannot compile without `ui.rs` (`parity.rs:36`); `tool/` cannot compile without `tui/` (`tool/team.rs:329`, `tool/diff.rs:176`); a 13 GB debug target. The `AppCore` extraction did the *conceptual* work of separating core from frontend, then left them in the same compilation unit. **The single highest-leverage rewrite decision is a workspace:** `bingo-core` (app + engine + api + tools), `bingo-tui`, `bingo-app-server`, `bingo-cli`. The seams already exist in the design; they just aren't crate boundaries.

**4. A 4,000-line file-size cap became a naming scheme instead of a refactor.**
`scripts/check_discipline.sh` enforces it, and the response was mechanical splitting rather than decomposition: `chat.rs` (3,921) + `chat_tail.rs` (3,645) is one type's methods cut in half — `chat_tail.rs`'s own header says "Owns no state; `impl super::Chat`". `chat_tests_a` through `chat_tests_g` (14,507 lines) are alphabet soup `#[path]`-mounted back into `chat.rs`. `tool/agent.rs` shed `agent_notes.rs` and `address.rs` with headers reading "split out of `agent.rs`, which sits against the file-size cap; the words and their reasons are unchanged." And the cap is currently **violated** — `chat_tests_b.rs` is 4,144 lines, so the discipline gate fails today. A cap that produces `_tail` and `_a..g` suffixes is measuring the wrong thing; `Chat` is one 7,500-line god object wearing nine filenames.

**5. Every design generation was kept as a sedimentary layer.**
Read the module headers: `tui/zoom.rs` — "This module used to be the alt-screen zoom (D105) … v6 retired the modal … what remains here is the part that was always about meaning." `tui/tree.rs` — "D104 built the agent tree here … v6 replaced the panel with the roster … what stays is what both bodies always shared." `tui/buffer.rs` — "D89 built a view layer on top of this and D103 retired it whole. What is left is the book-keeping half." `tui/conv.rs` — "The page itself is no longer here." Four generations of the same conversation-display idea (D88 → D103 → D130/132 → D134) survive as `buffer.rs` (935) + `bufferview.rs` (798) + `conv.rs` (497) + `conversation.rs` (285) + `zoom.rs` (747) + `tree.rs` (253) + `roster.rs` (619) = **4,134 lines whose headers are mostly obituaries**. Same pattern in the docs: `conversation-model-v6.md` and `v7.md` both live, with v7 declaring "where this file and v6 disagree on waking, obligation or main's place, this one wins; v6 still holds for everything else." A reader must diff two specs to know the rules. **The v2–v5 docs were deleted (D122) — the code's equivalents never were.**

**One thing worth defending in the rewrite:** the D140–D155 campaign is the best work here. Delete-the-old-protocol-first, contract-before-implementation, walls named in writing, one batch (D152) *reverted* rather than forced, and a checked parity ledger so the two frontends cannot silently diverge. `src/app/` is a genuinely good session actor. Keep `app/`, `engine/`, `api/`, the error-code registry (`src/error.rs`), the schema-generation discipline, and the black-box test style. Cut the social layer, split the crate, and collapse the seven conversation modules into one.
