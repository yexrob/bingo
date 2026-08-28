# Session, gateway, surfaces and ecosystem design proposal (2026-08-28)

> Source: subagent report, archived verbatim. Facts were verified on the date in the title; re-verify before depending on a version.

I have what I need. The old project's research already settled several things correctly (native protocol + lossy ACP projection, Interaction-as-resource not reverse-request, one submission path); the four walls all trace to one root cause — the TUI had the engine in reach and the kernel didn't. The design below makes that unreachable by crate boundary, and makes writes structurally unable to block.

---

# bingo-improve: Session/Gateway, Surfaces, Ecosystem — Design Proposal

## 0. Invariants (the design is these; everything else is consequence)

| # | Invariant | What it kills |
|---|---|---|
| I1 | **The kernel never blocks on a client.** Per-attachment outbound channel is bounded; overflow → kernel sends `Lagged{from,to}`, client re-reads the journal. | frontend backpressure into the actor |
| I2 | **A client's view is `fold(snapshot, events since snapshot.seq)`**, using the one reducer `SessionState::apply` that the kernel itself uses for its in-memory state. | 4-layer event enums, projection reconcilers |
| I3 | **Writes return nothing.** `submit / interrupt / answer` take a client-minted `IntentId` and return `()`. The outcome is `Event::IntentAck`. There is no receipt to await, so a sync key handler cannot wait for one. | D151 (`Answer::now()` ×15) |
| I4 | **Every action has exactly one handler, and it is in the kernel.** `match action` in kernel dispatch is exhaustive, no `_ => unavailable`. Surfaces depend on `bingo-contract` only; Cargo makes the engine unreachable from a surface. | D150 (console-has-no-engine), D152/D153 (half the action table in the TUI) |
| I5 | **A surface is testable without a runtime.** Key handling is `fn(Key, &SessionState, &mut LocalUi) -> Vec<Outgoing>`; drawing is `fn(&SessionState, &LocalUi) -> Frame`. Only the run loop is async and it is ~50 lines. | D149 (570 `#[test]`s that could not attach) |
| I6 | **The journal is the truth; the model context is derived.** One `.jsonl` of `Event`s per session; `ContextView::fold(journal)` produces the provider messages. Compaction and rewind are events, never rewrites. | transcript/projection dual truth |
| I7 | **An `Interaction` is a resource, not a call.** Opened by event, visible in snapshot, answered once by id from any attachment, resolved/cancelled by event with `by`. | reverse-request correlation dying on reconnect |
| I8 | **Ids are minted once, persisted, never re-minted on restart.** `SessionId`, `TurnId`, `ItemId`, `InteractionId` are ULIDs in the journal. No "epoch"; no "transcript locator vs sessionId" duality. | 7 files per session, epoch-scoped ids |
| I9 | **One wire, one vocabulary.** stdio and WebSocket carry byte-identical JSON-RPC 2.0 frames; `--print --output-format stream-json` is that server with a preset, not a second format. | `exec --json`-style lossy sibling formats |

**Rejected explicitly**

- *Session as a container of conversations (main/agent/room)* — the old model. Rejected: it is the reason a session was 7 files, why ACP had "no lossless mapping", and why the TUI needed a page model the kernel had to mirror. Here a session is one context; sub-agents are child sessions; "rooms"/teams, if they ever return, are a plugin that owns several sessions.
- *ACP as the native protocol* — reaffirmed from `acp-protocol-fit.md`: queue/steer, interactions-as-resources, and history generations have no honest ACP form. ACP is a surface plugin.
- *Daemon-first (TUI always over the wire)* — rejected for v1: adds process lifecycle to the 95% case. In-process and wire share one trait so the wire path is a flag, not a fork.
- *Journaling text deltas* — rejected: `ItemCompleted` is authoritative; deltas are live-only frames.

---

## 1. Session & addressing

**A session = one persisted context = one ordered journal = one actor.** Nothing else is "in" a session.

```rust
pub struct SessionId(Ulid);                 // identity, minted at create, immutable
pub struct SessionKey(String);              // optional routing index, unique across the store

pub struct SessionHeader {                  // journal line 0
    pub id: SessionId,
    pub key: Option<SessionKey>,            // at most one; rebindable (see below)
    pub cwd: PathBuf, pub project_root: Option<PathBuf>,
    pub parent: Option<ParentLink>,         // sub-agent: which session+item spawned me
    pub created_at: Ts,
}
pub struct ParentLink { pub session: SessionId, pub item: ItemId }  // the Agent tool-call item
```

**Key grammar** (convention enforced only for uniqueness and first-segment ownership):

```
key   := owner "/" path
owner := plugin id that minted it   ("tg", "slack", "feishu", "acp", "openclaw", "host")
path  := "/"-separated provider-native ids, most-general first

tg/chat/-100123/topic/77          Telegram supergroup topic
slack/T0AB/C0CD/1712345.6789      Slack workspace/channel/thread ts
feishu/oc_abc/om_thread           Feishu chat / thread root
acp/<client-name>/<their-session-id>   (only if the ACP client asks to bind)
host/<anything the host chose>    OpenClaw/Hermes passing --session-key
```

| Case | How it addresses a session |
|---|---|
| Local TUI | `SessionSelector::Latest{cwd}` (`--continue`), `ById` (`--resume <id or prefix>`), or `Create{cwd}`. No key. |
| `--print` | `Create{cwd}` or `ById`; `--session-key <k>` for hosts that want stable rebinding. |
| GUI attaching to existing | `ById`; lists via `sessions(filter)`. |
| IM chat/thread | `ByKey(k)` then `Create{key: Some(k)}` on miss. `/new` in chat = `rebind(k → fresh session)`; old session keeps its id, loses its key. |
| Sub-agent child | `Create{parent: Some(link)}`. No key. Parent's tool item carries `child: SessionId`. |

One representation: `SessionId` is identity; `SessionKey` is an index held in one place (the kernel's session index, `~/.local/share/bingo/sessions/index.jsonl`). Plugins never keep their own mapping table.

**Children are not merged into the parent stream.** The parent's `Item::ToolCall{kind: Agent, child, status, summary}` gets bounded `Updated` events (last tool line, tokens). A surface that wants detail opens the child session — same contract, same reducer. This is exactly how ACP and Claude's adapter flatten sub-agents (tool call + `_meta` hint), so the projection is free.

---

## 2. The client contract

### 2.1 Workspace shape (the "by construction" part)

```
crates/
  bingo-contract   Event, SessionState (+apply), Input, Action, Interaction, ids,
                   trait Kernel, ScriptedKernel (feature "test-support"). serde + schemars. No engine types.
  bingo-kernel     actor, journal, engine loop, tools, permissions, compaction, MCP, hooks. impl Kernel for LocalKernel.
  bingo-wire       JSON-RPC codec; Server<K: Kernel>; RemoteKernel (impl Kernel over stdio/WS).
  bingo-tui        surface. depends on bingo-contract ONLY.
  bingo-acp        surface (agent-client-protocol crate). contract only.
  bingo-channels   channel host + trait ChannelPlugin. contract only.
  bingo-cli        the binary: picks a Kernel (Local or Remote) and a surface.
```

`bingo-tui` → `bingo-kernel` is a forbidden edge (Cargo, plus a `cargo-deny`/workspace-lint test). That single rule is what makes D150/D152 unrepresentable.

### 2.2 Rust trait

```rust
#[trait_variant::make(Send)]
pub trait Kernel: Clone + Send + Sync + 'static {
    // ---- gateway (process-wide) ----
    async fn sessions(&self, f: SessionFilter) -> Vec<SessionSummary>;
    async fn open(&self, sel: SessionSelector, who: ClientIdentity) -> Result<Attachment, OpenError>;
    async fn delete(&self, id: SessionId) -> Result<(), KernelError>;
    async fn catalog(&self, k: CatalogKind) -> Catalog;         // models, providers, skills, mcp, actions
    fn gateway_events(&self) -> BoxStream<GatewayEvent>;        // SessionCreated/Removed, CatalogChanged
}

pub struct Attachment {
    pub session: SessionId,
    pub snapshot: SessionState,             // valid through snapshot.seq
    pub events: BoxStream<Frame>,           // every frame has seq > snapshot.seq; see §4 for Lagged
    pub handle: SessionHandle,
}

impl SessionHandle {                        // Clone; all writes are sync, non-blocking (I3)
    pub fn submit(&self, intent: IntentId, input: Input);
    pub fn interrupt(&self, intent: IntentId, scope: InterruptScope);   // Turn(TurnId) | Head
    pub fn answer(&self, intent: IntentId, id: InteractionId, answer: Answer, activation: Activation);
    pub async fn history(&self, page: HistoryPage) -> Result<HistoryChunk, KernelError>;   // read
    pub async fn events_since(&self, seq: Seq) -> BoxStream<Frame>;                        // resume
}

pub enum Input {                            // ONE submission entry
    Text { text: String, attachments: Vec<AssetRef>, origin: Origin },   // kernel parses "/", "!", "@"
    Action(Action),                                                       // typed; GUI buttons, hosts
}
pub struct Origin { pub surface: &'static str, pub principal: Option<String>, pub conversation: Option<String> }
```

`SessionSelector = Create{cwd, key?, parent?, opts} | ById(SessionId) | ByKey(SessionKey) | Latest{cwd}`.

`IntentId` is a client-minted ULID and the idempotency key: a duplicate `submit` with the same intent within the session's dedupe window returns the same `IntentAck` and does nothing.

### 2.3 Frame catalogue (keep it under ~20)

```rust
pub struct Frame { pub seq: Seq, pub ts: Ts, pub session: SessionId, pub body: Event }

pub enum Event {
    // session
    SessionUpdated(SessionSummary),          // title, model, mode, cwd, usage, warnings — replace
    SessionClosed { reason },
    // turn
    TurnStarted { turn: TurnId, inputs: Vec<ItemId>, origin: Origin },
    TurnRetrying { turn, dropped: Vec<ItemId> },
    TurnCompleted { turn, status: Completed|Failed{err}|Interrupted, usage },
    // item  (Delta is ephemeral: not journaled)
    ItemStarted(Item), ItemDelta { item: ItemId, n: u32, kind: Text|Reasoning|Tail, data: String },
    ItemUpdated(Item), ItemCompleted(Item),
    // queue (bounded → replace)
    QueueChanged { revision: u64, entries: Vec<QueueEntry> },
    // interactions
    InteractionOpened(Interaction),
    InteractionResolved { id, answer: AnswerSummary, by: ResolvedBy },
    InteractionCancelled { id, reason },
    // intents
    IntentAck { intent: IntentId, outcome: IntentOutcome },
    // history structure
    Compacted { generation: u64, boundary: ItemId, summary: ItemId, kept: Vec<ItemId> },
    Rewound   { generation: u64, to_turn: TurnId, dropped: Vec<ItemId>, files_restored: Vec<PathBuf> },
    ConfigChanged(ConfigView),               // model/mode/permission rules incl. session grants
    // transport
    Lagged { from: Seq, to: Seq },           // kernel tells you; you never infer gaps
}
pub enum IntentOutcome { TurnStarted{turn}, Queued{position}, Applied{result: ActionResult}, Rejected{error: KernelError} }
```

Everything that *happens in the transcript order* is an `Item` (user/assistant/reasoning/tool call/agent call/command/compaction marker/rewind marker/notice/permission receipt/question answer). Long-running actions (login, share, MCP reconnect) are `Item::Action{status}` — no separate "operation" resource. Item kinds carry their own delta semantics (`Text|Reasoning` append, `Tail` replace); nothing is inferred from payload shape.

`SessionState` (the snapshot and the reducer):

```rust
pub struct SessionState {
    pub seq: Seq, pub summary: SessionSummary, pub config: ConfigView,
    pub history_generation: u64,
    pub tail: Vec<Item>,                 // last N completed + every non-terminal item (streaming text included)
    pub turn: Option<LiveTurn>, pub queue: Vec<QueueEntry>,
    pub interactions: Vec<Interaction>,  // open ones, in order
    pub children: Vec<ChildSummary>,
    pub attention: Attention,            // DERIVED: open interactions || turn ended since last mark
}
impl SessionState { pub fn apply(&mut self, f: &Frame) -> Applied /* what changed, for renderers */ }
```

### 2.4 Wire: JSON-RPC 2.0 over NDJSON-stdio and WebSocket

**Recommendation: JSON-RPC 2.0.** Reasons: (1) ACP is JSON-RPC 2.0 and uses the same stdio discipline, so `bingo-acp` and `bingo-wire` share one codec; (2) Codex app-server, Hermes' TUI gateway and Claude's control channel are all JSON-RPC-shaped, so hosts already have parsers; (3) OpenClaw's `req/res/event` is a 1:1 renaming (`req`↔request, `res`↔response, `event`↔notification) — if OpenClaw wants an `AgentHarness`, the shim is a 20-line envelope map, not a second protocol. Keep OpenClaw's two good ideas: `connect`-first with capabilities, and idempotency keys (= `IntentId`).

```
--> initialize          { protocol: 1, client: {name, version}, capabilities: {answers: true, deltas: true} }
<-- result              { protocol: 1, server: {name, version}, features: {...}, limits: {frameBytes, ...} }
--> session/list        { filter }
--> session/open        { selector, since?: Seq }      → { snapshot }   (since: kernel may skip snapshot and replay)
--> session/close       { session }                    // detach this connection only
--> session/delete      { session }
--> session/events      { session, since: Seq }        // replay; notifications follow
--> session/history     { session, page }              → { items, next, generation }
--> session/submit      { session, intent, input }     → { accepted: true }
--> session/interrupt   { session, intent, scope }     → { accepted: true }
--> session/answer      { session, intent, interaction, answer, activation } → { accepted: true }
--> catalog/read        { kind }                       → { catalog }
--> shutdown
<-- event               { session, seq, ts, ... Event }        (notification)
<-- gateway/event       { ... GatewayEvent }                    (notification)
```

Thirteen methods, two notifications. Multi-session per connection is allowed (a GUI shows several; a host multiplexes). Errors: stable codes (`SESSION_LOCKED`, `INTERACTION_CLOSED`, `NOT_READY`, `STALE_GENERATION`, `NOT_INITIALIZED`). Schema generated from the Rust types (`bingo schema` → JSON Schema + TS), committed. **Transports:** `bingo serve --stdio` (hosts, IDEs, ACP spawn) and `bingo serve --ws 127.0.0.1:0 --token-file` (GUI, daemon). Same `Server<K>`; stderr is never protocol.

`RemoteKernel` implements `Kernel` over either transport, so the TUI, tests, and any Rust surface can point at a remote process by changing one constructor.

---

## 3. Each surface is just a client

### (a) TUI — in-process by default (`bingo`), wire by flag (`bingo --attach <ws|stdio-cmd>`)

Local types (never on the wire):

```rust
struct LocalUi { composer, scroll, theme, pending: BTreeMap<IntentId, Optimistic>, hidden_interactions: HashSet<InteractionId> }
enum Optimistic { UserRow{text}, Answered{interaction}, Interrupting }
enum Outgoing   { Submit{intent, input}, Interrupt{intent, scope}, Answer{intent, id, answer, activation}, Fetch(HistoryPage) }

fn on_key(k: Key, st: &SessionState, ui: &mut LocalUi) -> Vec<Outgoing>;   // pure, sync
fn draw(st: &SessionState, ui: &LocalUi, area) -> Frame;                      // pure, sync
```

Walkthrough — Enter on "run the tests" while a turn is busy:

1. `on_key(Enter)`: mint `intent`; `ui.pending.insert(intent, UserRow{text})`; clear composer; return `[Submit{intent, Input::Text{..}}]`. **Draw next tick** already shows the row (dim, "…").
2. Loop: `handle.submit(intent, input)` — sync send, returns `()`.
3. Kernel: appends user `Item` (with `intent` on it), decides queue, emits `ItemCompleted(user item)`, `QueueChanged`, `IntentAck{intent, Queued{position: 1}}`.
4. Reducer folds; `Applied` says "intent acked" → `ui.pending.remove(intent)`. The real row replaced the optimistic one (dedupe key = `item.intent`).
5. If `Rejected` → restore composer text, show error line. No second code path.

Walkthrough — permission `y`:

1. `on_key('y')`: if `now < guard_until` → no-op (locally disabled, also kernel-rejected). Else `ui.hidden_interactions.insert(id)`, emit `Answer{.., Keyboard}`. Modal disappears this tick.
2. `InteractionResolved{id, by}` or `Cancelled` arrives → remove from hidden set (the state no longer has it anyway). If `IntentAck{Rejected(InteractionClosed)}` — someone else answered first; state already shows the result. `by` renders as "approved via Telegram".

TUI-local slash commands (`/theme`, `/keys`, `/help` overlay) are in a `LocalCommand` enum in `bingo-tui`. `/help` = `catalog(Actions)` ∪ `LocalCommand::all()`. Everything else is `Input::Text` and the kernel parses. There is no place in `bingo-tui` where an `Action` could be executed.

Tests: `ScriptedKernel` (contract crate) records `Outgoing`s and replays `Frame`s from a fixture; 90% of TUI tests are `#[test]` over `apply`/`on_key`/`draw`. The same fixture files are the wire contract tests and the kernel black-box oracle (§4).

### (b) `--print` (headless)

In-process surface, ~200 lines. `open(Create|ById)`, `submit(prompt)`, then fold frames:

- `ItemDelta{Text}` on the assistant item → stdout. Everything else → stderr (`[tool] Bash cargo test … ok 2.1s`).
- `InteractionOpened(Permission|Question)`: if stdin is a TTY → render on stderr, read one line, `answer`. If not a TTY → `answer(Deny{feedback: "non-interactive"})` immediately (fail closed, but *through the same door*). `--permission-mode` is kernel policy, not this surface's.
- `TurnCompleted` → usage line to stderr, exit code from status; `Failed{err}` → `[error] code=… msg=…` (old stable contract kept).

`--print --output-format stream-json` is **not** this surface: it is `bingo serve --stdio` with a preset that auto-runs `session/open` + `session/submit` and prints every `event` notification. `--input-format stream-json` makes stdin accept JSON-RPC requests (answers, follow-up submits). One vocabulary (I9).

### (c) GUI over WebSocket

```
GUI ─connect ws://127.0.0.1:PORT?token=… ─▶ Server<LocalKernel>
--> initialize                              <-- {features, limits}
--> session/list {cwd}                      <-- [...]
--> session/open {ById(s1)}                 <-- {snapshot @ seq 4181}
<-- event {s1, seq 4182, ItemDelta ...}     (only > 4181; cut is atomic in the actor)
--> session/submit {s1, intent i9, Text}    <-- {accepted}
<-- event ItemCompleted(user, intent i9) · event IntentAck{i9, TurnStarted t7} · event TurnStarted
<-- event InteractionOpened(perm p3, guard_until, preview: Diff)
--> session/answer {s1, i10, p3, AllowSession{scope}, Pointer}   <-- {accepted}
<-- event InteractionResolved{p3, by: {client: "gui-2", surface:"gui"}} · event ConfigChanged (session grant visible to all)
   (socket drops)
--> session/open {ById(s1), since: 4210}    <-- {snapshot: null, replay: true}; events 4211.. follow from journal
```

The GUI's store is the generated-TS twin of `SessionState::apply` (fixtures guarantee equivalence). It never computes busy/attention itself.

### (d) IM channel plugin (Telegram as the example)

```rust
pub trait ChannelPlugin: Send + Sync {
    fn id(&self) -> &'static str;                                   // "tg" — owns the key prefix
    fn caps(&self) -> OutboundCaps;                                  // edit, thread, typing, buttons, max_len
    async fn run(&self, inbound: Sender<Inbound>, ctl: ChannelCtl) -> Result<()>;   // connect + pump
    async fn send(&self, to: ConversationRef, msg: Outbound) -> Result<MessageRef>; // Outbound::{Text, Edit{ref,text}, Typing, Buttons{..}}
}
pub struct Inbound { conversation: ConversationRef, sender: Principal, text, media: Vec<MediaFact>, reply_to: Option<MessageRef>, callback: Option<Callback> }
```

The **channel host** (`bingo-channels`, in-process, one per process) is the only code that talks to the kernel:

1. `Inbound` → policy (allowlist / DM pairing — host-owned, kernel-ignorant) → key `tg/chat/-100123/topic/77` → `open(ByKey)` else `open(Create{key, cwd: configured})`.
2. `/new` → `rebind`; `/stop` → `interrupt(Head)`; anything else → `submit(Input::Text{ text, attachments: media→AssetRef, origin: {surface:"tg", principal:"u:42", conversation:key} })`.
3. Host keeps one attachment per active session and runs `Deliverer`: a pure reducer `fn(&Frame, &mut DraftState, &OutboundCaps) -> Vec<Outbound>`:
   - `TurnStarted` → `Typing`.
   - assistant `ItemDelta`: coalesce (≥800ms or ≥300 chars) → if `caps.edit` then `Edit{ref}` else buffer until `ItemCompleted` → `Text` (split at `max_len`).
   - tool item `Started` → one status line, edited in place ("▶ Bash cargo test"), `Completed` → "✓ … 2.1s".
   - `InteractionOpened(Permission)` → `Buttons{ [Allow][Allow for session][Deny] , data: interaction_id }`; no buttons → text "reply **yes** / **always** / **no**".
   - `Callback{data}` or keyword reply-in-thread while the head interaction is open → `answer(id, .., Programmatic)`.
   - `InteractionResolved{by: other surface}` → edit the prompt message to "approved in TUI".
   - `TurnCompleted` → final edit, stop typing; `Failed` → error line.
   
The `Deliverer` reducer is in `bingo-channels`, provider-agnostic, tested from the same frame fixtures. A Slack/Feishu plugin is `caps` + transport only. Group chats: every user item records `origin.principal`, so the transcript shows who spoke and the ACP/GUI see the same.

---

## 4. Multi-client and durability

**Journal layout**

```
~/.local/share/bingo/sessions/index.jsonl                 {id, key, cwd, parent, title, updated}  (rebuilt from headers if lost)
~/.local/share/bingo/sessions/<ulid>/journal.jsonl        Frame per line, seq gapless, durable kinds only
~/.local/share/bingo/sessions/<ulid>/blobs/<sha256>       large tool output, images, diffs; events hold preview + BlobRef
~/.local/share/bingo/sessions/<ulid>/state.<seq>.json     OPTIONAL fold checkpoint — a cache, deletable
~/.local/share/bingo/sessions/<ulid>/.lock                one owning process
```

Rules: `ItemDelta` is the only ephemeral event (plus `Lagged`); it takes a live `seq` but is not written. Replay is therefore a subsequence; the client rule is *monotonic*, never *gapless* — gaps are the kernel's to announce (`Lagged`), not the client's to infer. Old "append compact marker, never rewrite" survives as `Compacted`/`Rewound` events.

**Model context is derived** by a second pure reducer in the kernel:

```rust
impl ContextView { pub fn fold(frames: impl Iterator<Item=&Frame>) -> Vec<NeutralMessage> }
```

- includes: user/assistant/tool-call+result/question-answer items; excludes: permission receipts, notices, action items.
- `Compacted{boundary, summary, kept}`: drop everything before `boundary` except `kept`, insert `summary`. The kernel lists `kept` explicitly so the reducer re-derives nothing.
- `Rewound{to_turn, dropped}`: items in `dropped` are excluded from context and marked `rewound` in `SessionState` (still in journal; `history_generation` bumps; a stale `history` page gets `STALE_GENERATION`). File restore is the tool's snapshot mechanism as before; the event records the paths.
- Journal has a `version` in the header; a format change is a re-fold with a migrator, never an in-place edit.

**Two clients on one session (TUI + GUI, TUI + Telegram)** — what v1 needs, all of it falls out of the above:

| Need | Provided by |
|---|---|
| gapless/monotonic seq, snapshot cut | actor assigns seq under one lock; snapshot is the actor's own `SessionState` |
| interaction fan-out | `InteractionOpened` to every attachment; first valid `answer` wins; `Resolved{by}` to all |
| "answered by whom" | `ResolvedBy::Client(ClientIdentity) \| Kernel \| Policy` |
| concurrent submits | kernel serializes, queue decides; both see `QueueChanged` + their own `IntentAck` |
| who said what | `Origin` on every user item |

Defer (additive later): per-client read cursors / unread counts, presence ("typing in GUI"), `Interaction.audience` (restrict who may answer), two *processes* on one session (lock says "attach to pid N instead").

---

## 5. Ecosystem paths, ranked

| Rank | Path | Cost | Benefit | Ship |
|---|---|---|---|---|
| 1 | **(ii) CLI backend / stream-json** = `serve --stdio` preset | ~0 beyond the wire | OpenClaw `registerCliBackend`, Hermes, CI, scripts | v1 |
| 2 | **(i) ACP surface** (`bingo-acp`, official Rust SDK) | medium (translation only) | Zed/IDEs, Hermes, OpenClaw `acpx` — three hosts for one adapter, and the first *external* consumer of the contract (the old app-server died of having none) | v1 |
| 3 | **(iii) native WS** for an OpenClaw `AgentHarness` | ~0 (server + published schema) | only when someone writes the harness | v1 server; harness not ours |
| 4 | **(iv) hosting IM channels** | high per provider | standalone product | v1: delegate to OpenClaw/Hermes; `bingo-channels` trait + `Deliverer` reducer land in v1 with a **loopback/test channel** only; Telegram in v2 |

**ACP mapping (v1 stable):**

| ACP | bingo |
|---|---|
| `initialize` | `initialize` (capabilities: fs/terminal from client ignored in v1) |
| `session/new{cwd}` | `open(Create{cwd, key: acp/<client>/<id>?})` |
| `session/load` | `open(ById)` + `history` replayed as `session/update` chunks |
| `session/prompt` | `submit(Text)`; hold the RPC until `TurnCompleted` (v1 semantics); a second prompt during a turn → `submit` (queued) but respond only when *its* turn completes |
| `session/cancel` | `interrupt(Head)` |
| `session/update` ← | `ItemDelta{Text}`→`agent_message_chunk`; `Reasoning`→`agent_thought_chunk`; tool item→`tool_call`/`tool_call_update` (status, content, `locations`, diff from preview/result); `Compacted`→ update with `_meta.bingo.compacted`; usage on `TurnCompleted` |
| `session/request_permission` → | one reverse request per `Interaction::Permission`; options = advertised `answers`; reply → `answer(.., Programmatic)`; `Cancelled` → ACP cancelled outcome |
| `elicitation/create` → | `Interaction::Question` |
| sub-agents | tool call + `_meta.bingo.child_session`; a client may `_bingo/session/open` it (extension method, gated by capability) |
| not mapped | queue positions, `history_generation`, session grants, `Login` (return ACP `auth_required` with a `_bingo/login` extension) |

**What the plugin traits must anticipate now** (cheap fields, expensive retrofits): `Origin{surface, principal, conversation}` on inputs; `SessionKey` + `rebind`; `ResolvedBy` on interactions; `OutboundCaps`-driven `Deliverer`; `Attention` derived in the reducer; `IntentId` as idempotency key; `ClientIdentity` on `open`.

---

## 6. Interactions over the wire

```rust
pub struct Interaction {
    pub id: InteractionId, pub session: SessionId,
    pub turn: Option<TurnId>, pub item: Option<ItemId>,       // the tool call waiting on it
    pub opened_at: Ts,
    pub guard_until: Option<Ts>,        // absolute; only Keyboard+approve before this is NOT_READY
    pub expires_at: Option<Ts>,         // device-code expiry etc.; kernel cancels with Expired
    pub kind: InteractionKind,
    pub answers: Vec<AnswerSpec>,       // exactly what the kernel will accept
}
pub enum InteractionKind {
    Permission { tool: String, summary: String, preview: Option<Preview>, session_scope: Option<Scope> },
    Question   { question: String, options: Vec<Option>, free_text: bool, multi: bool },
    Confirm    { title: String, detail: String },
    Login      { provider: String, flow: Browser{url} | Device{url, code} | Paste },
}
pub enum Preview { Diff{ files: Vec<FileDiff> /* bounded, else BlobRef */ }, Command{ cmd, cwd }, Url(String) }
pub enum Answer { AllowOnce, AllowSession{scope: ScopeId}, Deny{feedback: Option<String>}, Choice{ids: Vec<String>}, Text(String), Confirm, Cancel }
pub enum Activation { Keyboard, Pointer, Programmatic }
pub enum ResolvedBy { Client(ClientIdentity), Kernel /* OAuth poll/loopback landed */, Policy /* auto-allow rule, hook */ }
pub enum CancelReason { TurnEnded, Interrupted, SessionClosed, Expired, Superseded }
```

Lifecycle (kernel-enforced ordering):

1. Tool item `Started` (visible) → 2. `InteractionOpened` → 3. any attachment `answer(intent, id, ..)` → 4. first valid, non-premature wins; others get `IntentAck{Rejected(INTERACTION_CLOSED)}` → 5. receipt/answer `Item` committed (`ItemCompleted`) → 6. `InteractionResolved{by}` → 7. execution proceeds or fails closed.

- **Guard**: `guard_until` is absolute so snapshots and events need no recomputation; clients disable locally; kernel rejects `Keyboard` approvals before it with `NOT_READY`. `Pointer`/`Programmatic`/deny are immediate.
- **Scope**: `AllowSession` valid only if `session_scope` advertised; kernel installs a runtime rule and emits `ConfigChanged` so every client's `/permissions` agrees. Never persisted.
- **Late answers**: `INTERACTION_CLOSED`, stable, cannot touch a later interaction (ids are ULIDs).
- **Turn end / interrupt / close**: kernel emits `InteractionCancelled{reason}` for every open one *before* `TurnCompleted`; a cancelled permission is a deny.
- **Login**: `answers` is `[Cancel]` for Browser/Device (kernel resolves itself → `ResolvedBy::Kernel`), `[Text, Cancel]` for Paste. The token never appears in any event (`AnswerSummary::Redacted`).
- **"Needs you"**: not a kind. `SessionState.attention` is derived (open interactions, or turn ended since `mark_read`), and surfaces decide the bell/OSC/IM ping on `InteractionOpened` and `TurnCompleted`. One fewer representation.

---

## 7. Risks / open questions for the owner

1. **Journal-as-truth makes `ContextView::fold` the most load-bearing function in the system.** If it drifts across versions, old sessions resume with a different context. *Recommend:* items are stored provider-neutral (keep the old `NeutralRequest`/`StreamEvent` shapes); journal `version` in header; a golden test per version that folds a fixture journal to a fixed message list.
2. **In-process TUI + process lock means "TUI here, GUI there" needs the TUI process to be the server.** *Recommend:* every `bingo` TUI process starts the WS server on an ephemeral loopback port and writes `sessions/<id>/.lock` = `{pid, ws, token}`; a second client reads the lock and attaches. No daemon in v1; `bingo serve` is the same binary for hosts.
3. **Sub-agent = child session = journal per agent.** Hundreds of small directories for agent-heavy sessions. *Recommend:* accept; GC by parent; the alternative (embedding child items in the parent journal) reintroduces the container model.
4. **ACP v1 holds `session/prompt` open; our model is queue-first.** Hosts that send a second prompt mid-turn will wait on the queue. *Recommend:* accept in v1; when ACP v2 stabilizes, map `Queued` → v2's accepted-prompt lifecycle. Do not ship v2 draft by default.
5. **Key ownership across plugins** (`tg/…` minted by a future in-house Telegram plugin vs. `host/…` from OpenClaw for the same chat) can produce two sessions for one conversation. *Recommend:* kernel enforces "first segment == minting plugin id" and the docs state that a host that routes IM must not also run bingo's channel plugin for the same provider. Cheap now, painful later.

---

### Critical Files for Implementation

Proposed (new repo is empty; these are the load-bearing modules of the design):
- `/Users/yexrob/Episodes/Projects/bingo-inc/bingo-improve/crates/bingo-contract/src/event.rs` — `Frame`, `Event`, `Item`, `Interaction`, `IntentOutcome`
- `/Users/yexrob/Episodes/Projects/bingo-inc/bingo-improve/crates/bingo-contract/src/state.rs` — `SessionState::apply`, the one reducer (plus its fixture-driven tests, which double as the wire contract tests)
- `/Users/yexrob/Episodes/Projects/bingo-inc/bingo-improve/crates/bingo-contract/src/kernel.rs` — `trait Kernel`, `Attachment`, `SessionHandle`, `Input`, `SessionSelector`, `ScriptedKernel`
- `/Users/yexrob/Episodes/Projects/bingo-inc/bingo-improve/crates/bingo-kernel/src/journal.rs` — append/replay, `ContextView::fold`, `Compacted`/`Rewound` handling
- `/Users/yexrob/Episodes/Projects/bingo-inc/bingo-improve/crates/bingo-wire/src/server.rs` — `Server<K: Kernel>` and `RemoteKernel` over stdio/WS

Old-project references worth reading before writing those (learned semantics, not code to port):
- `/Users/yexrob/Episodes/Projects/bingo-inc/bingo/src/app/snapshot.rs` (lines 730–830, the old `Interaction`/`InteractionDecision` — closest ancestor of §6)
- `/Users/yexrob/Episodes/Projects/bingo-inc/bingo/notes/design/gui-app-server.md` (lines 703–744, the ordering invariants; most survive verbatim)
