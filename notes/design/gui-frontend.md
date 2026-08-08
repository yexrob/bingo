# GUI frontend design

> Status: proposal (D33). Not yet implemented. This document is the basis for
> future GUI development; the TUI remains the default frontend.

## 1. Positioning: one engine, multiple frontends

bingo is a local agent harness: the model only emits intent (tool_use); the
harness owns permissions, parallelism, side effects, compaction, memory, and
the UI (round-trip principle). A GUI is a **second frontend on the same core**,
not a rewrite.

The seam already exists on purpose: `src/ui.rs` defines a renderer-agnostic
contract (`UiEvent` / `AskRequest` / `UiHooks`) — "a TUI, a GUI or a test
harness all consume" it. The TUI is one adapter (`tui_hooks`); the GUI adds a
second adapter (`web_hooks`) that pushes the same event stream over a socket.

### Reusable assets (no rework needed)

| Asset | Location | GUI use |
|---|---|---|
| UiEvent stream (TextDelta/ThinkingDelta/ToolStart/ToolReady/ToolDone/RoundEnd/WatchEvent/Error…) | `src/ui.rs` | serialized and streamed verbatim |
| AskRequest = (PermissionRequest, oneshot\<DialogAction\>) | `src/ui.rs` | permission/question dialogs; the oneshot is already transport-agnostic |
| Self-contained share HTML (Claude Code app look) | `src/share_html.rs` | visual reference; its templates become the live-view component base |
| Stable error codes + feedback-states spec | `src/error.rs`, `notes/design/feedback-states.md` | error presentation contract (page-level highlight vs whole-flow state) |
| Task store / agent & channel snapshots (JSON on disk) | `src/tasks.rs`, `src/share.rs` | sidebar panels with zero backend work |
| Session list / resume source | `src/transcript.rs` | session history sidebar |

## 2. Tech selection

| Option | Form | Pros | Cons |
|---|---|---|---|
| **A. Local web server + browser (recommended v1)** | `bingo web` starts an axum server on 127.0.0.1, opens the browser | best rendering fidelity (markdown/code/diff/images are a mature browser ecosystem); fastest to build; cross-platform; reuses the share-page stack | weaker system integration than native (shortcuts, tray) |
| B. Tauri v2 desktop shell | Rust core + WebView | native feel, tray, global shortcuts | packaging/signing cost, WebView platform variance |
| C. Pure-Rust native (egui/iced) | single binary | no web stack | streaming markdown/diff rendering cost is very high; hard to match modern agent UX |

**Recommendation: A first, evolve into B.** In v1, upgrade the share-page
template from snapshot to live view — the share page and the live GUI share the
same frontend components, halving the work.

## 3. Process model & protocol

```text
┌─ bingo process (core untouched) ────────────────────────┐
│  queryLoop ──UiHooks──> web_hooks (mirror of tui_hooks)  │
│       │                    │                            │
│  Session/Runtime      broadcast::<UiEvent> fan-out       │
│       ▲                    │                            │
│       │ actions (REST)     ▼                            │
│  slash-equivalent ops ◄── axum (127.0.0.1:random) ◄── browser SPA │
└─────────────────────────────────────────────────────────┘
```

- **Downstream (WebSocket)**: serialized `UiEvent` stream; on connect, the
  server first sends the full transcript snapshot (same source as `/resume`).
- **Upstream (WebSocket + REST)**:
  - Send a message → `run_query` (same entry point the TUI uses).
  - Permission decisions → `DialogAction::Confirm/Answer/Cancel` fed back into
    the `AskRequest` oneshot.
  - Slash commands / settings → **reuse the existing slash handlers**
    (/model /permissions /theme /mcp /team /resume …). Today they live inside
    `src/tui/chat.rs`; extracting them into a renderer-independent core service
    is a GUI prerequisite (see issue: slash command core extraction) and
    overlaps with the chat.rs split (issue #8).
- **Errors**: keep the stable `[error] code=… msg=…` contract + `ErrorLevel`
  presentation tiers from the feedback-states spec; no new error vocabulary.

## 4. UI layout (benchmarked against Cursor / Claude Code / opencode agent panels)

```text
┌─────────┬────────────────────────────────┬────────────────┐
│ sidebar  │ main (conversation)            │ context panel   │
│ sessions │ message stream (streaming md)  │ task list       │
│ teams    │ tool activity timeline (fold)  │ agent instances │
│ channels │ thinking blocks (collapsible)  │ channel rooms   │
│ MCP      │ permission cards (inline)      │ context/tokens  │
│ share    │ code / diff / images           │ MCP status      │
├─────────┴────────────────────────────────┴────────────────┤
│ top bar: model · provider · thinking level · permission mode · theme │
│ composer: multi-line · paste · image attach · `/` menu · `!` bash mode │
└───────────────────────────────────────────────────────────┘
```

Key interactions reuse existing TUI semantics, no new behavior invented:

- Tool fold groups (`Read 3 files`, click to expand) — same semantics as
  `CollapseGroup` in the chat document model.
- Permission cards confirmable by number key — the TUI's 1/2 semantics.
- `/` opens the slash menu — the `SLASH_COMMANDS` table shipped to the client.
- Edit/Write diffs — `ToolResult.diff` already exists; dual-pane diff view.
- Session history — transcript dir scan, same source as `/resume`.
- **Images are a native win for the GUI**: browser `<img>` replaces the whole
  kitty-graphics/tmux-passthrough terminal machinery.

## 5. Change surface

| Module | Action |
|---|---|
| query.rs main loop / tool/* / permission.rs / mcp.rs / agents / team / channels / tasks / experience / hooks | **untouched** |
| `src/ui.rs` | add `web_hooks` adapter (mapping mirrors `tui_hooks`); derive serde on UiEvent/AskRequest + version the event envelope |
| slash commands | extract from chat.rs into a reusable core service (do together with the chat.rs split, issue #8) |
| new | `src/web/` (axum server, WS protocol, static hosting) + `web/` (React SPA) |
| share page | `share_html.rs` templates become the live-view component base |

## 6. Security

- Bind `127.0.0.1` + random port + one-time token in the startup URL
  (Jupyter/ghostty pattern).
- Transcripts contain tool output: keep the existing sensitive-info warning;
  surface it in the page itself.
- Remote access (`--serve`) is opt-in and a later milestone (TLS/password).

## 7. Milestones

- **M1 (~2-3 weeks)**: `bingo web` boots; streaming chat + tool timeline +
  permission cards + task sidebar + session list/resume + model/permission
  settings; reuse share-page styles.
- **M2**: diff view + agent/team/channel panels + images + slash palette +
  keyboard shortcuts + one-click share.
- **M3**: Tauri desktop shell (tray/global shortcuts); concurrent sessions;
  remote mode.

## 8. Risks and red lines

- **Do not feed the TUI's terminal row model to the browser** — the row/styled
  document in `tui/chat.rs` is terminal-oriented (most of its 8,906 lines are
  terminal rendering detail). The GUI consumes the `UiEvent` semantic stream and
  renders markdown with frontend tooling (react-markdown + shiki).
- Streaming connections must be robust: heartbeat + reconnect + snapshot
  restore.
- The GUI must not change core behavior: permissions, concurrency, compaction,
  and memory stay harness-owned (round-trip principle).
