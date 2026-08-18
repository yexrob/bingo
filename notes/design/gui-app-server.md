# GUI App-Server Protocol

Status: adopted 2026-08-18 (with the amendments below), not implemented.
Implementation plan: [`gui-app-server-plan.md`](gui-app-server-plan.md) (aggressive
final-form sequencing, ruled by the user; it supersedes this document's phased
"Implementation plan" section).

## Amendments (adopted 2026-08-18)

Rulings and revisions from the adoption review; where they conflict with the
body below, the amendments win.

1. No consumer of `--json-events` exists; delete-first (Phase 0) stands.
2. Aggressive sequencing replaces the phased plan: contract first, then core,
   then both adapters against the final shape. See the plan document.
3. `EventMeta` gains `ts` (unix ms, stamped at actor sequencing); items and
   turns carry authoritative `startedAt`/`completedAt`.
4. `turn/retrying` carries `attempt`, `maxAttempts`, `delayMs` in addition to
   the checkpoint replacement (`removedItemIds`).
5. The agent resource shape explicitly includes `model`, `provider`,
   `thinking`, `cwd`, `def`, `kind`, `state` — each agent is its own Session
   with its own engine; `AgentStatus` grows thinking/cwd accordingly.
6. Room log + attention-cursor persistence is IN scope for 1.0 (per-session
   sidecar, replayed on resume). `conversation/read` for a room after resume
   returns real history, not an empty log.
7. `catalog/read` is available before `session/start` (replaces `--inspect`'s
   discovery role).
8. `--print` becomes a third thin AppCore client; no frontend-specific
   capability gates anywhere in startup.
9. Core observability prerequisites are part of the campaign, not assumed:
   context/usage plumbed into `turn/usageUpdated`; `compact.rs` produces
   `CompactOutcome{before,after,replaced,duration}` for the compaction item.
10. Main wake behavior: the digest debounce moves into the core and main
    auto-starts turns in every frontend (`origin: auto`) — the previously
    ruled 乙案.

This document designs the application boundary required by a GUI with functional
parity with bingo's CLI. It replaces the development-only `--json-events`
protocol. No compatibility surface, adapter, or migration window is retained.

The primary-source comparison behind the protocol choice is in
[`notes/research/gui-event-protocols.md`](../research/gui-event-protocols.md).

## Decision summary

Build a new bidirectional app-server protocol and move bingo's application state
machine below both frontends.

- The internal boundary is a concrete `AppCore` that accepts typed actions,
  exposes authoritative snapshots, and publishes typed domain events.
- The TUI and GUI call the same actions and consume the same domain state. TUI
  rows, dialogs, key bindings, and page transitions remain a TUI projection.
- The external boundary is JSON-RPC 2.0 framed as one JSON object per line on
  stdin/stdout. Stderr remains diagnostics-only.
- The resource graph is rooted at a session. Conversations own ordered items;
  runnable conversations may also own turns whose items reference that turn.
  Operations, interactions, agents, rooms, tasks, and assets are first-class.
- Server-owned opaque IDs identify resources within a server epoch. JSON-RPC
  IDs correlate wire requests; the initial protocol does not promise mutation
  replay across a broken stdio connection.
- A completed item is authoritative over its deltas. Every started turn reaches
  exactly one terminal `turn/completed`, including failure and cancellation.
- Permissions and questions are persistent server-initiated interactions. The
  client answers them through a typed request, so snapshot recovery does not
  depend on a stale transport request ID.
- Slow-changing resources publish replacement snapshots. Text and reasoning use
  append-only deltas; a command live tail is a bounded replacement snapshot and
  large final output is an artifact. There is no generic JSON Patch protocol.
- Delete `--json-events`, `JsonSession`, `CliEvent`, `json_hooks`, probe/inspect
  modes, and their old tests. Add `bingo app-server` as the only JSON frontend.

This is a complete architecture refactor delivered incrementally, not a big-bang
wire rewrite.

## Why the current JSON protocol must be replaced

The current development-only JSON path is the wrong layer for a full frontend.

| Current shape | Consequence |
| --- | --- |
| `JsonSession` owns a second active-turn and prompt state machine | TUI and JSON behavior can diverge even when their event fields look similar. |
| `json_hooks` forwards text, tool completion, warnings, and reduced prompts | Thinking, retry, context, round boundaries, inbound messages, steering, and live Bash are absent by construction. |
| Permission replies are reduced to allow/deny | D81's session allow, resolved preview, scope, and denial feedback cannot be represented. |
| One main turn is the whole conversation model | Agents, rooms, direct sends, mention debt, per-conversation runs, and D137 peer messages have no identity or lifecycle. |
| The JSON startup path skips share attachment, team auto-start, startup feedback, and team-memory persistence | The host changes product behavior instead of only changing presentation. |
| `Chat::submit`, `run_slash`, and `handle_event` own routing and reduction | Serializing `UiEvent` would still leave the GUI to reimplement the actual application. |
| A failed query emits an error without a terminal turn event | A client cannot close the turn state machine deterministically. |
| Unbounded adapter channels and a flush per delta | A slow GUI can consume unbounded memory or turn token granularity into transport overhead. |
| No authoritative state read | Resume and recovery require parsing transcript paths and guessing live state. |

The current protocol landed before D81, D83, D84, and the current conversation
model. Since it has no released compatibility obligation, preserving those
omissions would only fossilize an obsolete application boundary.

Extending the event enum does not repair the deeper split: application behavior
still lives in the TUI. The first move must therefore be extraction of the
shared application controller.

## Meaning of parity

GUI parity means that every user-observable CLI action and domain state has one
of two explicit homes:

1. It is an `AppCore` action/state/event available to both frontends.
2. It is frontend-local presentation behavior with no effect on bingo state.

Parity does not mean reproducing terminal layout or key bindings over the wire.
Cursor editing, scroll position, collapse state, roster focus, page breaks,
terminal image cell geometry, and modal layout stay local. The following do not:

- which conversation receives submitted text;
- whether input starts, queues, steers, or delivers;
- session, model, provider, thinking, permission, MCP, and team mutations;
- turns, retries, context usage, tools, diffs, live command output, and errors;
- agents, rooms, messages, read cursors, mentions, obligations, and tasks;
- pending permissions/questions and their exact available decisions;
- transcript/history, compaction, rewind, images, sharing, and background work.

The project should maintain a parity ledger in tests. A new CLI action or state
cannot land without being classified as shared or frontend-local.

## Alternatives considered

Three deliberately different interfaces were designed against the current
codebase.

| Design | Strength | Failure mode |
| --- | --- | --- |
| Minimal `attach + dispatch + state deltas` | Very small public surface and a deep core | Generic entity patches and raw command lines make clients learn too much reducer and command-language detail. |
| Exhaustive resource RPC | Excellent discovery and independent evolution of each resource | Publishes dozens of methods, exposes implementation taxonomy, and makes the client orchestrate submission semantics. |
| GUI-shaped facade | Makes attach, open, submit, prompt, and interrupt hard to misuse | Risks encoding one GUI's view model and duplicating common typed methods with a generic command escape hatch. |

The selected design uses the useful part of each at a different layer:

- a small typed action/snapshot/event interface inside the process;
- explicit lifecycle and read methods on the wire;
- one deep conversation submission path for routing, queueing, and steering;
- a typed action union for uncommon CLI operations;
- semantic events rather than generic entity patches or terminal rows.

## Architecture

```text
TUI keys/composer                         GUI
       |                                  |
       v                                  v
 TUI adapter                       JSON-RPC adapter
       |                                  |
       +----------- AppCore --------------+
                  /    |     \
                 /     |      \
        query/tool   session   collaboration
          engine     storage   agents/rooms/tasks
```

`AppCore` is the seam. It owns application truth and ordering. Provider streams,
tool execution, transcript markers, registry locks, room wake rules, slash
parsing, and prompt oneshots stay behind it.

The TUI owns rendering and interaction mechanics only. The JSON adapter owns
framing, JSON-RPC correlation, schema negotiation, and serialization only.

A concrete internal shape is sufficient; a public trait is not needed before a
third implementation exists. Attachment establishes the atomic snapshot/stream
cut and one ordered frame channel carries replies, snapshots, interactions, and
events:

```rust
pub struct AppCore { /* private actor state */ }

impl AppCore {
    pub async fn attach(&self, request: AttachRequest) -> Result<AppLink, AppError>;
}

pub struct AppLink {
    pub requests: tokio::sync::mpsc::Sender<AppRequest>,
    pub frames: tokio::sync::mpsc::Receiver<AppFrame>,
}
```

The actor assigns event sequence numbers when it mutates state, not when a
transport happens to serialize an event. A snapshot frame and the stream that
follows it are created under one actor barrier. Events already buffered at or
before the snapshot cursor are suppressed for that attachment.

All mutation is serialized through one session actor. Provider calls, tools,
model-list fetches, authentication, and other external work may run concurrently,
but their results re-enter that actor before changing state or publishing an
event.

### Internal event layers

The current names blur three separate layers. They should become explicit:

- `EngineEvent`: private query/tool/provider progress entering `AppCore`.
- `AppEvent`: semantic application state consumed by both frontends.
- `TuiEvent`: optional TUI-only effects such as a page break, pinned panel, or
  terminal image measurement.

`UiHooks` may remain temporarily as the engine adapter during extraction, but it
must not be the public GUI contract. Delete `json_hooks`; app-server consumes
`AppEvent` directly.

## Resource model

### Session

A session is the persisted harness context. It owns configuration selection,
conversations, active work, collaboration registries, pending interactions, and
operations.

`sessionId` is server-owned and immutable within one `serverEpoch`. Rename
changes display metadata and the storage locator, not the open session's
identity. A restarted process creates a new epoch and may assign new resource
IDs; persisted session selection uses an explicit transcript locator. The
initial protocol does not add a session metadata sidecar solely to preserve
opaque IDs across restart.

Only one user session needs to be open per app-server connection initially. The
protocol does not prebuild concurrent multi-session control. Agents may still
run concurrently inside that session.

### Conversation

A conversation has an opaque `conversationId` and a kind:

```rust
pub enum ConversationKind {
    Main,
    Agent { agent_id: AgentId },
    Room { room_id: RoomId },
}
```

Main uses the same conversation projection and rendering contract as other
conversations. It is still the console/session target, not an invented
`AgentRegistry` participant: app-server must preserve the current main mail/digest and
addressing behavior until a separate product decision changes it. Its special
capabilities are session actions, shell input, and the model turn started by
user prose.

The active page is frontend-local. Every input carries its origin conversation,
which preserves D135 and D135a even if the user switches pages while a command
is queued. Command scope is decided by `AppCore`, not the frontend. In
particular, compact follows the originating page while most session commands
act on main/session state.

A conversation summary includes attention state the GUI cannot safely infer
from text: unread and mention counts, read cursors, outstanding obligations,
run state, and whether the user is a room member.

### Turn and round

A turn is one model or standalone shell run attached to a runnable conversation.
At most one turn writes a conversation at a time; different conversations may
run concurrently. Rooms receive posts but do not own model turns.

A round is one model request within the tool loop. A retry is a new attempt of
the same round. Failed-attempt output is not canonical history: retry publishes
an authoritative replacement of the turn's live tail at its pre-attempt
checkpoint before the next attempt starts. Clients never guess which text to
roll back, and discarded rows do not survive in transcript snapshots.

### Item

Items are ordered semantic content, not terminal rows and not raw provider
events. The initial union should cover:

- user, assistant, peer, and room messages;
- reasoning;
- tool calls with resolved input, summary, output, diff, duration, and status;
- standalone/local commands, their replacement live tail, and final output;
- compaction, rewind, interruption, and structured notices;
- question answers and permission decision receipts;
- images and other assets by opaque reference.

An item has `pending | streaming | completed | failed | cancelled`. Items
discarded by a transparent retry are removed by the retry checkpoint
replacement rather than retained as terminal history. `item/completed` carries
the authoritative final item. A frontend can therefore repair coalesced or
missed deltas without parsing prior text.

Items belong directly to a conversation and may carry an optional `turnId`.
Room posts are completed message items with no turn; there is no synthetic room
turn and no second `room/messageAdded` representation.

A user/inbound item that opens a turn has no `turnId`; the subsequent turn
snapshot references it as an input item. This preserves question-before-answer
ordering without making the input a turn-produced event.

Interaction resolution creates an ordered semantic item before execution
continues. Question answers enter the model context through the existing answer
path. Permission receipts are display/audit state only and are explicitly
excluded from model input. Both survive `conversation/read`; the transient
interaction resource itself disappears after resolution.

### Operation and interaction

An operation represents accepted asynchronous work that is not naturally a
turn, such as provider authentication, sharing, MCP reconnection, team startup,
or garbage collection. It has a server ID, progress, and exactly one terminal
state.

An interaction is a pending permission, question, or explicit destructive
confirmation. It has a stable `interactionId`. Pending interactions appear in
session and conversation snapshots and are answered by
`interaction/respond`, not by a connection-local reverse-request correlation.

## Wire protocol

### Framing

- UTF-8 NDJSON: exactly one JSON-RPC message per line.
- Stdout contains protocol frames only; diagnostics use stderr.
- Retain the current 1 MiB client-frame and 8 MiB server-frame ceilings unless
  measurements justify changing them.
- Images, full command output, and other large artifacts are referenced by ID,
  not inlined into events. Reads are chunked below the frame ceiling.
- EOF closes the client connection. The shutdown policy then interrupts active
  work and resolves permissions fail-closed before persistence and exit.

A malformed but bounded JSON line receives the standard JSON-RPC parse error and
does not mutate application state. Oversized, non-UTF-8, or otherwise unframeable
input closes the transport.

Stdio is the only required initial transport. A local socket can be added later as a
shallow adapter if live reconnect becomes a real requirement. WebSocket and
multi-controller arbitration are deliberately out of scope.

### Envelope and initialization

Use standard JSON-RPC 2.0 envelopes instead of repeating `protocolVersion` and
`commandId` inside every payload.

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol":{"major":1,"minMinor":0,"maxMinor":0},"client":{"name":"bingo-gui","version":"0.1.0"},"capabilities":{"interactionResponse":true}}}
{"jsonrpc":"2.0","id":1,"result":{"protocol":{"major":1,"minor":0},"server":{"name":"bingo","version":"...","epoch":"epoch_1"},"limits":{"maxClientFrameBytes":1048576,"maxServerFrameBytes":8388608},"capabilities":{"multiConversation":true,"reasoning":true,"images":true,"teams":true}}}
{"jsonrpc":"2.0","method":"initialized","params":{}}
```

No session events are emitted before `initialized`. The negotiated major and
capabilities remain fixed for the connection.

The server reports feature capabilities rather than making clients infer them
from model/provider names. Experimental methods require an explicit client
opt-in during initialization. A controlling client must support
`interaction/respond`; initialization otherwise fails rather than silently
auto-denying a future prompt.

Within major 1, unknown object fields are additive and ignored. A new method,
notification, required field, or discriminated-union variant is available only
when the selected minor version or an opted-in capability defines it. Breaking
semantics require a new major version.

### Client request taxonomy

The stable app-server surface should remain smaller than the CLI's command list.

| Area | Methods |
| --- | --- |
| Connection | `initialize`, `shutdown` |
| Session lifecycle | `session/list`, `session/start`, `session/resume`, `session/read`, `session/close`, `session/delete` |
| Conversation state | `conversation/list`, `conversation/read`, `conversation/markRead`, `conversation/submit` |
| Turn and queue | `turn/interrupt`, `queue/read`, `queue/reclaimTail` |
| Interaction | `interaction/respond` |
| Actions/state | `action/list`, `action/execute`, `config/read`, `catalog/read`, `resource/read` |
| Assets | `asset/registerPath`, `asset/readChunk` |

`session/read` and `conversation/read` are authoritative resynchronization
surfaces, not just startup conveniences. Historical conversation items use a
stable item cursor plus `historyGeneration`. Ordinary streaming and append-only
messages do not change that generation; structural rewrites such as rewind or
compaction do. A continuation from a stale generation returns `STALE_PAGE`
instead of mixing pre- and post-rewrite history. Live state is always included.

`conversation/read` has no attention side effect. A frontend calls
`conversation/markRead` only after content is actually visible, carrying the
last observed item or room sequence plus the expected conversation revision.
Prefetch and resynchronization therefore cannot clear unread or mention state.

`catalog/read` is a tagged read for models, providers, skills, MCP servers, and
available images. `resource/read` pages runtime collections such as agents,
rooms, tasks, deliveries, and background commands. If a resource later needs
independent mutation semantics, it can earn a dedicated method then.

Because the initial protocol is local-only, `asset/registerPath` accepts a local path plus
optional expected MIME type and SHA-256, validates the file, and returns an
opaque asset record. Successful registration snapshots the bytes into
server-owned storage; it never deletes or later borrows the caller's path. A GUI
with clipboard bytes writes a private temporary file first and may remove it
after registration succeeds. A remote upload protocol is not prebuilt.
`asset/readChunk` takes an
asset ID, byte offset, and bounded length and returns base64 plus the next offset
and EOF flag. Session-owned temporary assets have explicit cleanup; transcript
assets remain reconstructable from their existing durable content. Tool or
command output too large for an item carries a bounded preview and an artifact
ID for the same chunked read path.

### One submission path

The most important deep method is `conversation/submit`:

```rust
pub struct ConversationSubmit {
    pub conversation_id: ConversationId,
    pub input: Submission,
}

pub enum Submission {
    Composer {
        mode: ComposerMode,
        text: String,
        attachments: Vec<AssetId>,
    },
    SendProse { text: String, attachments: Vec<AssetId> },
}

pub enum ComposerMode { Normal, Shell }

pub enum SubmitDisposition {
    TurnStarted { turn_id: TurnId },
    Queued { queue_id: QueueId, position: usize, steer_eligible: bool },
    Delivered { message_id: ItemId },
    Applied { result: ActionResult },
    OperationStarted { operation_id: OperationId },
}
```

The TUI adapter first resolves terminal-only paste placeholders and image-path
shortcuts into text/assets, then uses `Composer`. A GUI's human composer uses
the same variant, so leading slash, shell mode, and direct-address syntax cannot
silently bypass CLI semantics. `SendProse` is an explicit programmatic delivery
that never parses those forms. Both normalize to the same internal `AppCommand`
before routing.

The caller never selects queue versus steer. `AppCore` applies the existing
rules:

- prose on main starts work when idle and queues while busy;
- prose on an agent or room is delivered to that conversation;
- direct sends do not wait behind main's busy state;
- shell and session actions always execute in the console/main context even when
  submitted from another page;
- the origin is still retained for the few page-sensitive actions such as
  compact;
- only the eligible FIFO prefix of plain, attachment-free input may steer at a
  tool barrier;
- a command or attachment blocks later queue entries from overtaking it;
- queue absorption and tail reclamation are one race with one winner;
- a queued action keeps the conversation on which it was submitted.

A busy submission is only `Queued`; it cannot promise that steering will occur.
If a later tool barrier absorbs it, `queue/itemAbsorbed` and the corresponding
conversation item record that transition. If the turn ends first, normal FIFO
drain starts the next turn. `queue/reclaimTail` matches the CLI's pull-back of
the newest entry and returns `AlreadyAbsorbed` if the barrier won.

### Typed actions

`action/execute` requires `originConversationId` and a tagged `Action` enum. The
composer's slash/shell parser produces the same enum. There are never separate
TUI and GUI handlers, and an action cannot accidentally retarget itself to the
page visible when a queued action eventually drains.

```rust
pub struct ActionExecute {
    pub origin_conversation_id: ConversationId,
    pub precondition: Option<ResourceRevision>,
    pub action: Action,
}
```

Mutations of revisioned config/team/task resources carry the relevant expected
revision; stale writes fail instead of overwriting a concurrently refreshed
view.

The initial action families map the existing CLI behavior:

- session reset/rename/garbage collection/share/change-directory;
- conversation compact and rewind;
- model/provider/thinking selection and provider login/logout;
- permission-rule mutation;
- MCP enable/disable/reconnect;
- skill invocation;
- team start/assign/stop and room join/leave;
- foreground-command promotion; foreground interruption remains
  `turn/interrupt`.

Pure views such as help, status, context, config, skills, tasks, and available
images read structured state/catalogs. Their TUI strings are projections, not
protocol contracts. Theme choice, opening an image, and exit chrome are
frontend-local, while persisted settings changes still use a typed action.

`action/list` returns stable action IDs, availability, argument schemas, and
short English labels. The command menu and `/help` derive from the same registry
that dispatches `Action`; a completeness test keeps metadata and handlers in
lockstep.

### Server notifications

Notifications use explicit method names and carry a shared event header:

```rust
pub struct EventMeta {
    pub seq: u64,
    pub session_id: SessionId,
    pub caused_by: Option<OperationId>,
}
```

Required families:

| Family | Notifications and semantics |
| --- | --- |
| Session | `session/updated`, `session/closed`, `session/deleted`; payload replaces the session summary. |
| Conversation | `conversation/created`, `conversation/updated`, `conversation/removed`; update replaces the summary including attention/obligation state. |
| Turn | `turn/started`, `turn/roundStarted`, `turn/retrying`, `turn/roundCompleted`, `turn/usageUpdated`, `turn/completed`. `turn/retrying` replaces the live tail with its checkpoint. |
| Item | `item/started`, `item/textDelta`, `item/reasoningDelta`, `item/commandTailUpdated`, `item/updated`, `item/completed`. Text/reasoning deltas append; tail/update/completion replace. |
| Input queue | `queue/itemAdded`, `queue/itemRemoved`, `queue/itemAbsorbed`; each carries bounded items/IDs and the queue revision. |
| Interaction | `interaction/opened`, `interaction/resolved`, `interaction/cancelled`. |
| Collaboration | `agent/changed`, `room/changed`, `task/changed`, `task/removed`, `delivery/changed`; room posts use the ordinary item lifecycle. |
| Operations | `operation/started`, `operation/progress`, `operation/completed`. |
| Runtime state | `config/changed`, `catalog/changed`, `asset/available`, `feedback/raised`, `feedback/cleared`. |

Specialized delta names are intentional. A generic `item/delta` would require
clients to infer whether a payload appends, patches, or replaces.

`item/commandTailUpdated` is D84's terminal-semantics tail snapshot: bounded
lines plus total-line count, after carriage-return handling and escape removal.
It never appends to final output. Only the console's single foreground command
publishes/promotes this slot. Subagent Bash runs remain detached and
non-promotable; after promotion the command is background watch state only, with
no cancel or re-foreground capability invented by app-server.

Unbounded collections are paginated and changed by keyed upsert/removal events.
Only intrinsically bounded state, such as one conversation summary or one
operation, is replaced wholesale. `queue/read` pages an immutable queue revision;
append, tail reclaim, front drain, and prefix absorption publish bounded changes
with positions rather than a complete ordered ID list. Absorption emits one
`queue/itemAbsorbed` per entry in contiguous sequence order. This keeps every frame
under the negotiated limit without imposing a new hidden CLI queue limit.

Direct-message delivery state is structured rather than inferred from watch
labels. It includes sender, private-lane target, message/ack IDs, and
`queued | delivered | read | answered | dropped` plus follow-up state. A peer's
turn prose does not settle the sender's acknowledgment; only an observable
message back does, preserving D137.

Example turn flow:

```json
{"jsonrpc":"2.0","id":7,"method":"conversation/submit","params":{"conversationId":"conv_main","input":{"type":"composer","mode":"normal","text":"Run the tests","attachments":[]}}}
{"jsonrpc":"2.0","id":7,"result":{"disposition":{"type":"turnStarted","turnId":"turn_9"}}}
{"jsonrpc":"2.0","method":"turn/started","params":{"event":{"seq":101,"sessionId":"sess_1"},"conversationId":"conv_main","turn":{"id":"turn_9","status":"running"}}}
{"jsonrpc":"2.0","method":"item/started","params":{"event":{"seq":102,"sessionId":"sess_1"},"conversationId":"conv_main","turnId":"turn_9","item":{"id":"item_12","type":"assistantMessage","status":"streaming","text":""}}}
{"jsonrpc":"2.0","method":"item/textDelta","params":{"event":{"seq":103,"sessionId":"sess_1"},"conversationId":"conv_main","turnId":"turn_9","itemId":"item_12","deltaSeq":1,"delta":"I will run "}}
```

### Server-initiated interactions

Permission and question prompts are persistent application requests, announced
as server notifications and answered by the client's `interaction/respond`
request. This is intentionally not a transport-level reverse request: a prompt
recovered from a snapshot must remain answerable without its original JSON-RPC
correlation ID.

```json
{"jsonrpc":"2.0","method":"interaction/opened","params":{"event":{"seq":110,"sessionId":"sess_1"},"interaction":{"id":"perm_3","conversationId":"conv_main","turnId":"turn_9","itemId":"tool_2","remainingGuardMs":400,"prompt":{"type":"permission","title":"Allow running Bash","reason":"Run the test suite","tool":{"name":"Bash","input":{"command":"cargo test"}},"preview":{"type":"command","command":"cargo test"},"decisions":["allowOnce","allowSession","deny"],"sessionScope":{"id":"scope_8","label":"Bash: cargo test"},"allowsFeedback":true}}}}
{"jsonrpc":"2.0","id":8,"method":"interaction/respond","params":{"interactionId":"perm_3","activation":"pointer","decision":{"type":"allowSession","scopeId":"scope_8"}}}
{"jsonrpc":"2.0","id":8,"result":{"status":"accepted"}}
```

The server advertises exactly which decisions are valid. `allowSession` is
rejected when no advertised scope exists. Denial feedback is structured and
travels to the model through the existing permission-error path.

The core records the permission-open instant and enforces D81's confirmation
guard. `remainingGuardMs` is recomputed for an event or snapshot so the frontend
can disable premature keyboard confirmation. `interaction/respond` carries an
activation kind. Only keyboard allow-confirmation during the guard fails with
`INTERACTION_NOT_READY`; pointer approval, denial/cancel, and non-confirmation
keys remain immediate, matching D81 rather than imposing a blanket delay.

`allowSession` installs only the permission engine's derived and verified rule
in `session.runtime.permissions`. It is visible while that session remains open
but is never written to settings or transcript state and disappears on close or
process restart. Persistent permission-rule actions are a separate capability
with their own explicit storage semantics.

Interaction ordering is:

1. the tool/item becomes visible;
2. `interaction/opened` is sequenced into application state;
3. the client sends `interaction/respond`;
4. the first valid, non-premature response wins;
5. the ordered question-answer or permission-receipt item is committed;
6. `interaction/resolved` or `interaction/cancelled` closes the state;
7. execution proceeds or fails closed.

Interrupt and session close cancel their outstanding interactions before the
turn reaches its terminal event. A late or repeated response returns a stable
`INTERACTION_CLOSED` error and cannot affect a later prompt.

## Snapshots and recovery

`session/read` returns an atomic session snapshot containing:

- session metadata, cwd, selected provider/model/thinking/permission mode;
- a conversation-collection revision/count plus bounded summaries for main and
  currently active/attention-bearing conversations; `conversation/list` pages
  the complete collection;
- active turns, queue revision/count summaries, pending interactions, and
  operations;
- collection revisions plus bounded active summaries for agents, rooms, tasks,
  background commands, MCP state, and capability state; unbounded remainder is
  read through paginated resource/catalog methods;
- an `eventCursor` through which the snapshot is valid.

`conversation/read` returns an atomic conversation snapshot containing:

- conversation identity, `conversationRevision`, and `historyGeneration`;
- a paginated ordered item page whose cursor is bound only to
  `historyGeneration`;
- active turn/round/items, a bounded queue page/revision, pending interactions,
  and context usage;
- an `eventCursor` through which the snapshot is valid.

The response is enqueued before events caused after its snapshot cut. The first
subsequent event has `seq > eventCursor`. This closes the subscribe-then-read
race without a durable event log.

Event sequence is scoped to `serverEpoch` and gapless. A client that detects a
gap or resource-revision mismatch calls the relevant read method and replaces
local state. The initial protocol does not add a replay journal or promise live turn
recovery across process death. A restarted app-server advertises a new epoch;
all prior live resource IDs are invalid and disappear when the persisted session
is resumed. A hard-dead process cannot emit or persist a terminal event for its
last in-memory turn, so the protocol does not claim one. Clean EOF/shutdown still
interrupts and persists through the normal core path.

Completed item snapshots are authoritative. Transcript persistence remains the
durable source already used by the CLI; the protocol does not turn transport
events into a second event-sourced database.

## Lifecycle and ordering invariants

These are protocol requirements, not implementation notes.

1. `initialize` completes before any session notification.
2. Server resource IDs are opaque, non-empty, and unique within their resource
   type and server epoch. Clients never choose turn, item, prompt, or operation
   IDs.
3. An accepted request response is written before the first event caused solely
   by that request.
4. Events have strictly increasing, gapless `seq` values within a server epoch.
5. `turn/started` precedes all turn-produced round, live-item, usage, and
   completion events. The completed input items referenced by the turn precede
   `turn/started`. Exactly one `turn/completed` follows with status
   `completed | failed | cancelled |
   interrupted`; an error never substitutes for the terminal event.
6. `item/started` precedes its deltas. Delta sequence is contiguous per item.
   Each item either receives exactly one terminal snapshot or is removed by an
   explicit retry checkpoint replacement; removed IDs are never reused.
7. A failed stream attempt emits `turn/retrying` with an authoritative
   checkpoint replacement; discarded attempt items do not remain in the
   conversation, and the next attempt uses new live item IDs.
8. User/inbound message items are ordered before the turn or round that consumes
   them. Frontends never implement transcript splice rules.
9. Queue absorption and tail reclamation are atomic. Whichever event is sequenced
   first wins; absorbed input cannot be pulled back.
10. Queue order is FIFO, page origin is immutable, and ineligible input prevents
    later input from steering past it.
11. Turn interruption is idempotent and targets a server-issued turn ID. A late
    interrupt cannot cancel the next turn.
12. A pending interaction is answerable exactly once with an advertised answer.
    The permission guard applies only to keyboard approval; denial and
    cancellation remain immediate. Cancellation fails closed.
13. Bounded resource updates replace their named resource. Unbounded
    collections use paginated reads plus keyed changes. Text and reasoning
    deltas append; command-tail snapshots replace. No other update semantics are
    implicit.
14. Reading or prefetching a conversation never marks it read; only an explicit
    `conversation/markRead` advances attention cursors.
15. Conversation selection and display state never change server routing.

## Errors, load, and security

JSON-RPC standard codes cover parse, invalid request, unknown method, invalid
params, and internal errors. Application errors add structured data:

```json
{
  "code": -32010,
  "message": "The turn is no longer active.",
  "data": {
    "bingoCode": "TURN_CLOSED",
    "recoverable": true,
    "scope": "turn",
    "sessionId": "sess_1",
    "conversationId": "conv_main",
    "turnId": "turn_9",
    "suggestedAction": "refreshConversation"
  }
}
```

Human-readable messages are English and sanitized; clients branch on
`bingoCode`, never on text. Request validation errors terminate only that
request. Async failures close their turn/item/operation before raising
structured feedback. Only framing corruption, incompatible initialization,
stdout failure, or unrecoverable core corruption closes the connection.

Both inbound and outbound queues are bounded. Adjacent append deltas for the
same item may be coalesced before a sequence number is assigned. Lifecycle,
interaction, replacement snapshot, and terminal events are never selectively
dropped while the connection remains healthy. If bounded backpressure and a
write timeout cannot recover, the transport is already unusable: a
`CLIENT_TOO_SLOW` error is best-effort only. The server closes the transport,
interrupts active work through the normal shutdown path, and persists what it
can; it does not claim that the blocked client received the final frames.

The app-server is local-only initially. Events and snapshots never contain API
keys, OAuth tokens, or raw credential values. Provider state exposes presence,
source, and status only. File paths, command previews, diffs, transcripts, and
share output are sensitive application data and must not be copied to stderr
or telemetry by default.

## CLI parity ledger

The following table is the minimum acceptance inventory, not a future-feature
wish list.

| CLI behavior | Shared contract |
| --- | --- |
| Text on main, agent, or room page | `conversation/submit`; core decides turn versus delivery. |
| Direct-address syntax and stopped-agent revival | Raw composer parser -> the same typed delivery action. |
| Busy input queue and pull-back | queued disposition, keyed queue events, `queue/reclaimTail`. |
| Mid-turn steering at the tool barrier | core queue transaction plus item/turn events. |
| Shell input, live tail, foreground interruption, promotion | command items, replacement tail snapshots, `turn/interrupt`, promotion action, background-command state. |
| Text and thinking streams | assistant/reasoning items and append-only deltas. |
| Stream retry and round boundaries | explicit round lifecycle plus authoritative retry checkpoint replacement. |
| Tool input, summary, output, status, diff, duration | authoritative tool item snapshots. |
| Context usage, output tokens, status | turn usage and conversation/session snapshots. |
| Permission previews, allow once/session, denial feedback | server interaction request and terminal interaction event. |
| AskUserQuestion | typed question interaction and answer. |
| Images in input and tool output | asset registration/reference/read; no terminal cell geometry. |
| Clear/new, compact, resume, rename, delete, close, cwd, GC, share | session lifecycle/read methods or typed actions. |
| Rewind | preview/apply action, operation state, authoritative conversation refresh. |
| Model/provider/thinking and provider authentication | catalogs, typed actions, operation events. |
| Config/status/help/skills/images/tasks views | structured reads and action metadata; each frontend renders its own view. |
| Permission rules and MCP management | config/catalog state plus typed actions. |
| Team start/status/assign/stop | team state and typed actions. |
| Agents, rooms, join/leave, DMs, peer messages | conversation/resource snapshots and message items. |
| Direct-message delivery/read/answer/chase state | typed delivery snapshots/events; peer prose alone does not settle an acknowledgment. |
| Unread, mentions, read cursors, waiting-on-user obligations | conversation summaries; never inferred from prose. |
| Agent/task/command watch transitions | typed resource or operation updates, not label-only strings. |
| Warnings, notices, errors, loading/progress | feedback and operation state with stable codes. |
| Theme, scroll, folds, roster focus, key bindings | frontend-local; persisted preference changes remain typed config actions. |

`PinPanel`, `Unpin`, `SlashInfo`, `SlashOutput`, and terminal-specific
`ImageMeta` are not wire events. Their underlying operation, feedback, asset,
or catalog state is.

## Implementation plan

### Phase 0: delete the obsolete boundary and measure

- Delete `src/json_events.rs`, the `--json-events`, `--probe`, and `--inspect`
  flags, exact-session-only arguments, JSON-specific error plumbing, and all
  old JSON unit and black-box tests. Keep no wire fixtures or compatibility
  adapter.
- Remove `json_events` startup forks so share attachment, team auto-start,
  startup feedback, and team-memory persistence use the normal session path.
- Add the parity ledger as a checked table covering every slash command,
  submission branch, and `UiEvent` variant.
- Record normalized scenario traces for text, tool, permission, retry, steer,
  live Bash, direct delivery, room traffic, tasks, and interruption.

### Phase 1: extract the application core

- Move non-visual conversation state and `Chat::handle_event` reduction into
  `AppCore`.
- Move submit routing, queue/steer arbitration, direct delivery, and command
  scope into `AppCore`.
- Introduce `EngineEvent`, `AppCommand`, `AppEvent`, and snapshot types.
- Keep TUI-only cursor, layout, collapse, picker, page-break, and theme state in
  `src/tui`.
- Put the TUI on `AppCore` before adding app-server transport. This proves the
  shared core against the product with the broadest existing behavior.

Suggested ownership, allowed to evolve with the extraction:

```text
src/app/
  mod.rs
  controller.rs
  command.rs
  event.rs
  snapshot.rs
  catalog.rs
src/app_server/
  mod.rs
  protocol.rs
  stdio.rs
  schema.rs
```

### Phase 2: normalize application actions

- Move slash-command behavior out of `Chat` one family at a time.
- Make raw CLI parsing and typed GUI actions converge on the same enum and
  handler.
- Move session/config/provider/MCP/team/task/room operations behind `AppCore`.
- Keep app-server startup on the normal session path; do not reintroduce
  frontend-specific capability gates.

### Phase 3: add app-server protocol 1.0

- Add `bingo app-server` with JSON-RPC/NDJSON stdio transport.
- Derive the serde/schemars contract from Rust types.
- Add `bingo app-server generate-schema --out <dir>` and commit a deterministic
  Draft 7 schema bundle. Its manifest maps every method/notification to
  direction, params, result, declared application errors, and stable `$id`
  references; shape-only enum schemas are not a complete RPC contract.
- Generate TypeScript from that schema in the GUI build rather than maintaining
  handwritten duplicate interfaces.
- Start experimental until the parity ledger and black-box scenarios are green.

### Phase 4: declare GUI parity

- Run the same controller scenarios through the TUI adapter and JSON adapter.
- Complete the capability ledger with no unexplained omissions.
- Update `src/skills/bundled/guide.md`, README capability documentation, and
  `notes/design/feedback-states.md` in the implementation batches that change
  user-visible behavior.
- Only then advertise app-server protocol 1.0 as stable.

## Verification strategy

### Core behavior tests

Test the shared controller once, at its public interface:

- submission disposition on main, agent, and room conversations;
- page-origin preservation for queued commands;
- FIFO steering barrier and tail-reclaim race;
- retry checkpoint replacement and authoritative final items;
- exactly-one terminal turn/operation/interaction state, and terminal-or-
  explicitly-removed item state;
- prompt scope validation, denial feedback, cancellation, and late replies;
- direct/peer/room message attribution and mention obligations;
- session resume, compaction, rewind, team, task, MCP, and provider actions.

These tests replace duplicated frontend state-machine tests rather than layering
another copy on top.

### Protocol contract tests

- one fixture for every request, response, notification, interaction, and error
  variant;
- schema generation is deterministic and CI fails on unreviewed drift;
- unknown additive fields are accepted within a major version;
- unsupported required capabilities and major versions fail initialization;
- duplicate request IDs/tokens, malformed frames, oversized frames, and EOF;
- response-before-causal-event and snapshot-cursor ordering;
- slow-reader backpressure and delta coalescing without lifecycle loss.

### Black-box parity scenarios

Run a fake provider/tool harness through a real `bingo app-server` process and
assert exit status, stdout-only framing, stderr separation, and normalized event
traces for:

- text + reasoning + usage;
- tool call + diff + permission + denial feedback;
- retry + round transition;
- queue + steer + interrupt;
- live shell output + backgrounding;
- images/assets;
- agent/room/peer messages + tasks + obligations;
- startup warnings, provider authentication, MCP, sharing, and session close.

All normal project gates still apply to every implementation batch:
`cargo fmt --all -- --check`, `cargo check --locked --all-targets`,
`cargo clippy --locked --all-targets -- -D warnings`, and
`cargo test --locked --all-targets`.

## Explicit non-goals for 1.0

- simultaneous control by multiple GUI/TUI clients;
- a durable wire-event journal or event-sourced transcript replacement;
- WebSocket or remote network exposure;
- exposing provider-native stream frames;
- terminal layout, cursor, scroll, fold, or pixel state;
- redesigning the conversation model, permission policy, team model, or tools.

These omissions keep the protocol at the existing product boundary. They can be
added when an independent consumer creates a real seam.
