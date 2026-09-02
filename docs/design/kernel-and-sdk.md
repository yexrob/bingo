# Kernel and plugin SDK design proposal (2026-08-28)

> Source: subagent report, archived verbatim. Facts were verified on the date in the title; re-verify before depending on a version.

# bingo-improve: kernel + plugin SDK design proposal

Sources read: the survey, and from the old tree `src/tool/mod.rs`, `tool/executor.rs`, `api/contract.rs`, `api/types.rs`, `permission.rs`, `hooks.rs`, `query.rs`/`query_turn.rs`/`query_session.rs`, `engine/{events,runner}.rs`, `app/{engine,event,ids,snapshot,turn,queue,submit,interaction,operation,command}.rs`, `agents.rs`, `channels.rs`, `team.rs`, `compact.rs`, `memory.rs`, `system.rs`, `tools.rs`, `transcript.rs`, `rewind.rs`, `watch.rs`, `mcp.rs`, `skills.rs`, `settings.rs`, `notes/research/{acp-protocol-fit,gui-event-protocols}.md`, `notes/cc-gap-analysis.md`. Toolchain on this machine: rustc 1.96.0.

Conventions below: `sdk::` = `bingo-sdk`, `core::` = `bingo-core`. All sketches omit derives (`Debug, Clone, Serialize, Deserialize, JsonSchema`) which every wire type carries. Every SDK struct/enum that the kernel produces is `#[non_exhaustive]`.

---

## 1. Kernel boundary

The kernel is what remains when every *feature* is removed and the thing still runs one model turn with zero tools, zero storage and a scripted provider, and publishes a gapless event stream about it. Five things survive:

| # | In kernel | Why it cannot be a plugin |
|---|---|---|
| K1 | **Domain vocabulary** (`sdk::model`, `sdk::event`): ids, `Message`/`ContentPart`, `Item`, `Turn`, `Interaction`, `Event`, `Submission`, `Disposition`, `ContextUsage`, `Verdict` | Two plugins can only talk if the nouns are owned by neither. This is the "one fact, one representation" contract; it lives in the SDK crate, not in core, because plugins compile against it and core is one consumer among them. |
| K2 | **Session actor**: id mint, ordered event log (`seq`), item registry, turn registry (exactly one terminal state, guard-on-drop), interaction registry (advertised decisions, answered once, confirm guard, fail-closed), input queue (barrier/reclaim race arbitrated inside the actor), `items -> Vec<Message>` projection | Ordering is a global property. Whoever assigns `seq` and decides which of two racing writes won *is* the kernel. The old code got this right (`AppCore`, D142/D144) and the design keeps it. |
| K3 | **Turn loop state machine** (§5) + tool executor (safe-batch parallel / unsafe serial, cancel keeps completed results, tool_use/tool_result pairing) | The loop is the *sequence* in which plugins are consulted. A plugin cannot define the order in which plugins run. It contains no feature: no inbox, no hire release, no task reminder, no team norms. Those are contributors it calls. |
| K4 | **Permission gate** (not policy): `hooks.before_tool → policy.decide → interaction.ask → policy.on_verdict → execute`, plus the fail-closed defaults (`ToolTraits::default()` = not safe, not read-only, interrupt=Block) | The gate is the one place where "the user said no" is enforced. If a plugin could bypass it, permissions are advisory. The *policy* (modes, rule tables, sensitive dirs) is a plugin. With no policy registered the gate denies everything not read-only. |
| K5 | **Host/registry + plugin lifecycle**: manifest check, capability resolution, `register → start → stop`, config layering and per-plugin namespaces, service locator, `HostApi` (subscribe/submit/respond/interrupt) | The host is what plugins register *into*. It also owns the one submission entry and the one subscription entry, so every surface is a client of the same door. |

Everything else is a plugin, including Bash/Read/Edit, every provider, every surface, the transcript store, the compactor, memory, skills, MCP, sub-agents, teams, rooms, tasks, experience.

### The temptation, and why not

1. *"Persistence must be core; a session that forgets is broken."* No. The kernel's log is in-memory and gapless; the JSONL store is a `SessionStore` plugin the actor calls persist-first-then-publish (old `record()` order, kept). With a null store (`--print` in tests, sub-agent scratch sessions) the kernel is unchanged. Making it core would make every test spin a disk.
2. *"Bash is special: `!` shell mode, hooks, the live tail, the background registry all need it."* `!` is a `Command` named `!` that the Bash plugin registers; the live tail is `ToolContext::progress()` (replace-semantics item update); background runs are a service the Bash plugin owns and the agents plugin reuses. Nothing in core spells "Bash".
3. *"Sub-agents need a registry in core to be addressable."* Only `HostApi::open_session(SessionSpec{parent})` and `submit(session, …)` are core. A child session's *inbox* is the kernel input queue (idle → starts a turn; busy → absorbed at the next barrier — exactly the v7 room semantics). Names, acks, chases, crews are a plugin reading events.
4. *"Compaction is a loop concern; it must be core or the loop overflows."* The loop has two hook points (`Threshold`, `Overflow`) and calls whatever `Compactor` is registered. With none, an overflow fails the turn with `CONTEXT_OVERFLOW`. The *measurement* (one ruler: `ContextUsage`, server-anchored estimate, learned windows) is core because two rulers was the old bug (D172).
5. *"Permission modes are user-visible; the TUI footer shows them."* Modes are the policy plugin's config; it claims the `permissionMode` key, publishes `ConfigChanged{scope: Plugin("bingo.permissions")}`, and registers `/permission-mode`. The TUI reads the plugin's namespace. The kernel never enumerates modes (old code had `PermissionMode` twice).

---

## 2. The plugin mechanism in Rust

| | (a) in-process static crates | (b) out-of-process JSON-RPC (stdio/WS) | (c) WASM (extism / component model) | (d) dlopen / abi_stable |
|---|---|---|---|---|
| Safety | safe Rust, compile-time typed | safe; process isolation | sandboxed | **unsafe**; ABI fragile |
| Type fidelity | full (`Arc<dyn Tool>`) | serde-level (JSON) | serde-level + WIT | full but ABI-locked |
| Language | Rust only | any | any that targets wasm | Rust only |
| Latency | ns | ms per call (pipe) | µs | ns |
| Access to OS (spawn, TTY, sockets) | yes | yes | no (WASI, no process spawn) | yes |
| Hot reload | rebuild | yes | yes | yes |
| Fits | tools, providers, surfaces, policy, store, compactor | MCP tools, shell hooks, IM gateways, external agents (ACP), CLI backends | pure tools (formatters, parsers), text hooks | – |

**Recommendation.** Primary: **(a)**. Every bundled feature is a workspace crate exposing `pub fn plugin() -> Box<dyn Plugin>`; the `bingo` bin composes a `Vec<Box<dyn Plugin>>`. Secondary: **(b)**, delivered as *one* bundled bridge plugin (`bingo-rpc`) that speaks a JSON mirror of the SDK types (`Contribution`, `Event`, `ToolCall`, …) and re-registers an external process's contributions into the same `Registrar`. MCP and shell hooks are the two day-one instances of (b) — MCP through `rmcp`, hooks through the CC stdin/stdout contract — each as its own plugin using the same bridging idea. **(c)** deferred: worth it only for a marketplace of pure tools; the seam is the same `Registrar`, so nothing in the SDK changes when it arrives. **(d)** rejected (unsafe, and it buys nothing (a) lacks except hot reload).

What each contribution kind can use: `Tool` a/b/c · `Provider` a/b (b = "CLI backend": a process that streams `ModelEvent` JSONL) · `Surface` a (external clients are not plugins; they attach through the app-server surface) · `Hook` a/b/c · `ContextContributor` a/b · `Command` a/b · `PermissionPolicy` a only (latency and trust) · `SessionStore`, `Compactor` a only.

### Manifest, capabilities, config, dependencies

```rust
// sdk::plugin
pub struct PluginManifest {
    pub id: &'static str,                 // "bingo.tools.bash"
    pub version: &'static str,
    pub sdk: &'static str,                // semver req on bingo-sdk, checked at boot
    pub provides: &'static [&'static str],// "tool:Bash", "command:!", "service:bingo.background", "provider:anthropic"
    pub requires: &'static [&'static str],// missing → plugin disabled with Notice{PLUGIN_UNMET}, never a crash
    pub config: Option<ConfigClaim>,
}
pub struct ConfigClaim {
    pub keys: &'static [(&'static str, Merge)], // top-level settings keys this plugin owns
    pub schema: fn() -> schemars::Schema,        // schemars::schema_for!(T)
}
pub enum Merge { Replace, Accumulate, ByName }   // user < project < local layering per key
```

- Capabilities are plain strings with a `kind:name` shape. The host topologically orders `register` by `requires`/`provides`, refuses duplicate `provides` unless the kind allows many (`tool:*` no, `hook:*` yes), and records unmet requirements as events.
- Config: the kernel loads the three layers once, merges per claimed key with the claimed `Merge`, validates against the claimed schema, and hands `Registrar::config::<T: DeserializeOwned>()` the plugin's slice. Unclaimed top-level keys produce the old "unknown key" lint. Kernel keys: `model`, `provider`, `thinking`, `cwd`-independent ones only.
- Discovery: (a) static list in the bin; (b) `~/.config/bingo/plugins/<id>/plugin.json` and `.bingo/plugins/` read by `bingo-rpc` (Hermes layout, manifest is the JSON form of `PluginManifest`).
- Cross-plugin dependencies go through **services**: `Contribution::Service(Box<dyn Any + Send + Sync>)` holding an `Arc<dyn SomeTrait>`; consumers call `host.service::<Arc<dyn SomeTrait>>()`. The trait itself is exported by the providing plugin's crate (e.g. skills plugin exports `SkillSource`) — a contract at a boundary consumed independently, not an SDK trait.

---

## 3. The stable trait set

Decisions that apply to all: every contribution trait is **object-safe** and held as `Arc<dyn T>` (registries are heterogeneous). Async methods use `#[async_trait]` (rejected: native `async fn` — not dyn-compatible on 1.96; `dynosaur` — 0.x macro on the whole ABI). If dyn-compatible AFIT stabilises, the macro goes and no signature changes. Cancellation is `tokio_util::sync::CancellationToken` (rejected: `watch<bool>`, whose version semantics produced the two subtle bugs documented in old `executor.rs`). Extension without breaking: `#[non_exhaustive]` + builders on structs, default method bodies, `Capabilities` bitflags, and a `meta: serde_json::Map` on `Item`, `Event`, `ToolSpec`, `SessionSpec` (this is the `_meta` ACP passes through).

### 3.1 `Plugin` — entry and lifecycle

```rust
#[async_trait]
pub trait Plugin: Send + Sync + 'static {
    fn manifest(&self) -> &'static PluginManifest;
    /// Synchronous, at boot, in dependency order. Only registers; does no I/O.
    fn register(&self, reg: &mut Registrar<'_>) -> Result<(), PluginError>;
    /// After every plugin registered. May spawn tasks (MCP dial, OAuth refresh).
    async fn start(&self, host: HostHandle) -> Result<(), PluginError> { Ok(()) }
    async fn stop(&self, deadline: Duration) -> Result<(), PluginError> { Ok(()) }
}

pub struct Registrar<'a> { /* host-owned */ }
impl Registrar<'_> {
    pub fn add(&mut self, c: Contribution);
    pub fn config<T: DeserializeOwned>(&self) -> Result<T, PluginError>;
    pub fn plugin_id(&self) -> &str;
}
#[non_exhaustive]
pub enum Contribution {
    Tool(Arc<dyn Tool>),
    Provider(Arc<dyn Provider>),
    Policy(Arc<dyn PermissionPolicy>),
    Hook(Arc<dyn Hook>),
    Context(Arc<dyn ContextContributor>),
    Command(Arc<dyn Command>),
    Surface(Arc<dyn Surface>),
    Store(Arc<dyn SessionStore>),
    Compactor(Arc<dyn Compactor>),
    Service { key: &'static str, value: Box<dyn Any + Send + Sync> },
}
```
Registered by: the bin (static) or `bingo-rpc` (external). Called by: the host, once. Versioning: `manifest.sdk` gate; `Contribution` is `#[non_exhaustive]`. One enum rather than one `Registrar` method per kind so the RPC bridge and the in-process path share one representation.

### 3.2 `Provider` — model streaming

```rust
pub struct ModelRequest {                       // provider-neutral; adapters map to wire
    pub model: String, pub max_tokens: u32,
    pub system: Vec<SystemBlock>,               // {text, cache: bool}
    pub messages: Vec<Message>,                 // sdk::model
    pub tools: Vec<ToolSpec>,
    pub thinking: Option<Effort>,               // Low|Medium|High|Xhigh|Max; None = no param
    pub meta: Map,
}
#[non_exhaustive]
pub enum ModelEvent {                           // ephemeral; never leaves the turn loop
    Start { id: String, model: String, input_tokens: Option<u64> },
    BlockStart { index: u32, kind: BlockKind }, // Text | Thinking | ToolUse{id, name}
    Delta { index: u32, delta: BlockDelta },    // Text(String) | Thinking(String) | Signature(String) | InputJson(String)
    BlockStop { index: u32 },
    Stop { reason: StopReason, usage: Usage },  // EndTurn | ToolUse | MaxTokens | StopSequence | Other(String)
    Error { message: String, retryable: Retryable, retry_after: Option<Duration> },
}
pub type ModelStream = Pin<Box<dyn Stream<Item = Result<ModelEvent, ProviderError>> + Send>>;

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;                                       // "anthropic", "openai", "codex"
    fn capabilities(&self, model: &str) -> ModelCapabilities;   // context_window, max_output, images, thinking, count_tokens, caching
    async fn stream(&self, req: &ModelRequest, cancel: CancellationToken) -> Result<ModelStream, ProviderError>;
    async fn count_tokens(&self, req: &ModelRequest) -> Result<u64, ProviderError> { Err(ProviderError::Unsupported) }
    async fn models(&self) -> Result<Vec<ModelInfo>, ProviderError> { Ok(Vec::new()) }
    fn auth(&self) -> AuthStatus { AuthStatus::NotApplicable }
    async fn login(&self, cx: &LoginContext) -> Result<(), ProviderError> { Err(ProviderError::Unsupported) } // OAuth flows use cx.prompt()/cx.open_url()
}
```
Registered by provider plugins (one per protocol; named endpoints in settings are instances). Called by the turn loop, the compactor, memory extraction. `complete_text` is deliberately absent: it is `stream` drained (old D171 finding: non-streaming dies at proxies). `ProviderError` keeps the old stable code mapping (`AUTH_REQUIRED`, `RATE_LIMITED`, `CONTEXT_OVERFLOW{body}`, `OFFLINE`, `TIMEOUT`, `SERVER_ERROR`). Kernel-side `Accumulator` folds `ModelEvent` into `Item`s directly; old `AssistantAccumulator` logic is ported verbatim.

### 3.3 `Tool` — improved contract

```rust
pub struct ToolSpec { pub name: String, pub description: String, pub input_schema: Value, pub aliases: Vec<String>, pub meta: Map }
#[non_exhaustive]
pub struct ToolTraits {                          // fail-closed defaults
    pub concurrency_safe: bool,                  // false
    pub read_only: bool,                         // false
    pub destructive: bool,                       // false
    pub edit: bool,                              // false  (acceptEdits target)
    pub result_limit: ResultLimit,               // Global | SelfBounded (P0 gap: per-tool result policy)
    pub trusted_traits: bool,                    // false for MCP: readOnlyHint is never trusted by the gate
}
#[non_exhaustive]
pub enum Subject { Path(PathBuf), Command(String), Url(String), Name(String) } // what a rule matches against
#[non_exhaustive]
pub enum Preview { Diff(String), Command(String), Text(String) }

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn traits(&self, input: &Value) -> ToolTraits { ToolTraits::default() }
    /// The things a permission rule may match on. Bash → [Command], Edit → [Path], WebFetch → [Url], Skill → [Name].
    fn subjects(&self, input: &Value, cwd: &Path) -> Vec<Subject> { Vec::new() }
    /// A decision only a person may take; forces a prompt in every mode, no allow rule pre-authorises it.
    fn confirm(&self, input: &Value) -> Option<String> { None }
    /// Dry run for the approval prompt. Reads, never writes.
    fn preview(&self, input: &Value, cwd: &Path) -> Option<Preview> { None }
    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError>;
}
pub struct ToolOutput { pub parts: Vec<ContentPart>, pub is_error: bool, pub display: Option<Display>, pub meta: Map }
pub enum Display { Diff(String), Artifact(AssetId), Summary(String) }
```
Improvements over the old trait: `subjects()` removes the tool-name `match` from the policy (old `permission.rs:181-242` hard-coded Bash/Read/Edit/Write/Grep/Glob/WebFetch/Skill); `traits()` collapses four booleans into one struct with the two missing fields from the gap analysis (`interrupt`, `result_limit`); `ToolOutput.parts` is the same `ContentPart` the model message uses (old: `serde_json::Value` that was sometimes a string, sometimes an array of untyped blocks). Registered by tool plugins and by MCP (one `Tool` per discovered remote tool, `trusted_traits: false`). Called by the executor only.

### 3.4 `ToolContext` — what a tool may reach

```rust
pub struct ToolContext {
    pub call: ToolCallId, pub session: SessionId, pub turn: TurnId, pub item: ItemId,
    pub cwd: PathBuf,
    pub env: Arc<Env>,                    // home, data_dir, config_dir, cache_dir, shared reqwest::Client, shell dialect
    pub cancel: CancellationToken,        // child of the turn's token; honoured per `Interrupt` trait
    pub session_info: SessionInfo,        // depth, parent, model, provider
    host: HostHandle,
}
impl ToolContext {
    pub async fn ask(&self, prompt: Prompt) -> Verdict;                 // Question/Confirmation via the interaction registry
    pub fn progress(&self, tail: Progress);                             // Event::ItemUpdated (replace semantics) — the live tail
    pub async fn record(&self, body: ItemBody) -> ItemId;               // an item into this session's log outside the call (background completion)
    pub async fn spawn_session(&self, spec: SessionSpec) -> Result<SessionHandle, HostError>; // THE sub-agent brick
    pub async fn submit(&self, to: SessionId, s: Submission) -> Result<Disposition, HostError>; // peer messaging brick
    pub fn service<T: Clone + Send + Sync + 'static>(&self) -> Option<T>; // e.g. Arc<dyn Checkpointer>, Arc<dyn Background>
    pub fn events(&self) -> EventStream;                                // subscribe (a tool that waits on another session)
}
```
Not in the context: task store, hooks config, permission mode string, TUI expand signal, avatar tables — every one of these in the old `ToolContext` (`tool/mod.rs:34-64`) was a plugin reaching into another plugin. They become services.

### 3.5 `PermissionPolicy`

```rust
pub struct PolicyInput<'a> { pub call: &'a ToolCall, pub traits: &'a ToolTraits, pub subjects: &'a [Subject],
                             pub confirm: Option<&'a str>, pub session: &'a SessionInfo, pub cwd: &'a Path }
#[non_exhaustive]
pub enum Decision {
    Allow { reason: Reason },
    Deny  { reason: Reason },
    Ask   { reason: Reason, scope: Option<Scope>, preview: bool },   // scope = the verified "don't ask again" rule
}
#[non_exhaustive]
pub enum Reason { Rule(String), Mode(String), Hook(String), Safety(String), ReadOnly, Confirm(String), Default }
#[async_trait]
pub trait PermissionPolicy: Send + Sync {
    fn id(&self) -> &str;
    async fn decide(&self, input: PolicyInput<'_>) -> Decision;
    /// Install the session-scoped rule the user accepted. Never persisted by the kernel.
    async fn on_verdict(&self, input: PolicyInput<'_>, verdict: &Verdict, scope: Option<&Scope>) {}
}
```
Exactly one policy is active (last registered wins with a Notice; rejected: chaining, because "which policy's Allow wins" is a second policy). Plugins that want a say use `Hook::before_tool`. The bundled `bingo-permissions` ports old `can_use_tool` semantics unchanged (deny→ask→preapproved→read-only→safety→bypass→acceptEdits→allow→mode default; Bash sub-command splitting with any/all; `:*` suffix; `mcp__server` prefix) but matches on `Subject`s. Typed `Reason` closes P0 #5; the `Ask` arm carrying `scope` closes the `unreachable!` path (P0 #3) because the gate, not the policy, resolves `Ask`.

### 3.6 `Hook` — typed lifecycle interceptors

```rust
#[non_exhaustive]
pub enum HookOutcome { Continue, Deny { reason: String }, Ask { reason: String }, Block { reason: String }, Redirect { session: SessionId } }
pub struct HookContext { pub session: SessionInfo, pub turn: Option<TurnId>, pub cwd: PathBuf, host: HostHandle }

#[async_trait]
pub trait Hook: Send + Sync {
    fn id(&self) -> &str;
    fn matcher(&self) -> HookMatcher;   // which points + optional tool-name regex, so the kernel skips non-matching hooks cheaply
    async fn on_submit(&self, sub: &mut Submission, cx: &HookContext) -> HookOutcome { HookOutcome::Continue }
    async fn before_tool(&self, call: &mut ToolCall, cx: &HookContext) -> HookOutcome { HookOutcome::Continue } // may rewrite input
    async fn after_tool(&self, call: &ToolCall, out: &ToolOutput, cx: &HookContext) -> HookOutcome { HookOutcome::Continue }
    async fn on_stop(&self, cx: &HookContext) -> HookOutcome { HookOutcome::Continue }                  // Block{reason} → loop continues once
    async fn on_turn(&self, phase: Phase, turn: &TurnInfo, items: &[Item], cx: &HookContext) {}        // Start | End — memory extraction lives here
    async fn on_compact(&self, phase: Phase, cx: &HookContext) {}
    async fn on_session(&self, phase: Phase, cx: &HookContext) {}
    /// Passive observer of the sequenced log; the generic form of TaskCreated/TaskCompleted/CwdChanged/PermissionRequest.
    async fn on_event(&self, event: &Event, cx: &HookContext) {}
}
```
Shell hooks become one plugin (`bingo-hooks-shell`) that reads `hooks` from settings and implements each method by spawning the command with the CC JSON contract (exit 2, `updatedInput`, decision `deny|ask|block`, concurrent stdin write + wait, kill-on-drop, 60s/1.5s timeouts). `TaskCreated`/`TaskCompleted` are `on_event` matches on `Event::Extension{plugin:"bingo.tasks"}`; `PermissionRequest`, `PostToolUseFailure`, `CwdChanged` come free from `on_event`. Ordering: registration order; first non-`Continue` wins, `before_tool` input rewrites accumulate.

### 3.7 `ContextContributor` — how features enter the prompt

```rust
#[non_exhaustive]
pub enum Placement { System { order: i32 }, RoundStart, Barrier }
pub struct ContextQuery<'a> { pub session: &'a SessionInfo, pub turn: &'a TurnInfo, pub round: u32,
                              pub items: &'a [Item], pub usage: &'a ContextUsage, pub model: &'a ModelCapabilities, host: HostHandle }
#[non_exhaustive]
pub enum ContextPiece { System(SystemBlock), User { parts: Vec<ContentPart>, label: String } }
#[async_trait]
pub trait ContextContributor: Send + Sync {
    fn id(&self) -> &str;
    fn placement(&self) -> Placement;
    async fn contribute(&self, q: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError>;
}
```
This is the trait that empties the old 575-line `query_loop`. Old lines `query.rs:933-1023` are seven contributors: inbox drain, task reminder, agent-inbox flush, hire release, background notifications, main mail, model-capability block. `System` pieces are recomputed per request (so `/model` changes the capability block, old `with_model_capabilities`); `User` pieces become `Item::User{origin: Contributor(id)}` — recorded, so transcript and provider cache prefix agree (old bug noted at `query.rs:984-987`).

### 3.8 `Surface` — frontends, and why `Channel` is not a second trait

```rust
#[async_trait]
pub trait Surface: Send + Sync {
    fn id(&self) -> &str;                               // "tui", "print", "app-server", "acp", "telegram"
    fn kind(&self) -> SurfaceKind;                      // Exclusive (owns the terminal/stdio) | Concurrent
    async fn run(&self, host: HostHandle, opts: SurfaceOptions) -> Result<Exit, SurfaceError>;
}
```
A surface is a *client*: it calls `HostApi` and reads `Event`s. The kernel never calls into a surface except `run`. An IM channel is a `Concurrent` surface whose `run` connects to a platform, maps inbound messages to `HostApi::open_session(SessionSpec{key: "agent:main:telegram:group:-100:topic:77"})` + `submit`, and projects events back. Rejected: a `Channel{connect,disconnect,send}` trait — it would be a second name for "a client that submits and projects"; the platform adapter abstraction belongs inside a gateway plugin, where OpenClaw/Hermes put it. Permission prompts from a channel session are answered through the same `respond()`; whether a channel may auto-approve is that surface's policy against advertised decisions.

### 3.9 `HostApi` — the two entries (submission, subscription) plus controls

```rust
#[async_trait]
pub trait HostApi: Send + Sync {
    fn subscribe(&self, session: SessionId, opts: SubscribeOptions) -> Result<(Snapshot, EventStream), HostError>; // snapshot cut + gapless from cut.seq
    async fn open_session(&self, spec: SessionSpec) -> Result<SessionId, HostError>;       // New | Resume(locator); parent for children
    async fn close_session(&self, id: SessionId, reason: CloseReason) -> Result<(), HostError>;
    async fn submit(&self, session: SessionId, s: Submission) -> Result<Disposition, HostError>;
    async fn respond(&self, id: InteractionId, decision: InteractionDecision, activation: Activation) -> Result<(), HostError>;
    async fn interrupt(&self, session: SessionId, turn: Option<TurnId>, reason: InterruptReason) -> Result<Interrupted, HostError>;
    async fn reclaim(&self, session: SessionId, queue: QueueId) -> Result<Reclaim, HostError>;
    async fn set_config(&self, scope: ConfigScope, value: Value) -> Result<(), HostError>;
    fn catalog(&self, kind: CatalogKind) -> Catalog;                                        // Tools|Commands|Models|Providers|Skills|Plugins
    fn sessions(&self) -> Vec<SessionInfo>;
    fn service_any(&self, key: TypeId) -> Option<&(dyn Any + Send + Sync)>;
}
pub struct HostHandle(Arc<dyn HostApi>);   // adds the generic `service::<T>()` over service_any
pub enum Submission { Composer { text: String, attachments: Vec<AssetId> }, Raw { parts: Vec<ContentPart>, origin: Origin } }
pub enum Disposition { Turn(TurnId), Queued { id: QueueId, position: u32, steerable: bool }, Applied(CommandOutcome), Operation(OperationId), Redirected(SessionId) }
```
`Composer` parses `/x` → `Command`, `!x` → `Command("!")`, then `Hook::on_submit` may `Redirect` (the agents plugin resolves `@name`); `Raw` parses nothing (old `SendProse`, and what a contributor/peer uses). The app-server surface is these methods 1:1 as JSON-RPC, and `Event` serialised as the notification payload — no second protocol enum.

### 3.10 `SessionStore`

```rust
#[non_exhaustive]
pub enum Record { Item(Item), TurnOpened { turn: TurnInfo }, Compacted { boundary: ItemSeq, summary: ItemId }, Rewound { to: ItemSeq } }
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn create(&self, meta: &SessionMeta) -> Result<SessionLocator, StoreError>;
    async fn append(&self, loc: &SessionLocator, rec: &Record) -> Result<(), StoreError>;   // called by the actor before publish
    async fn load(&self, loc: &SessionLocator) -> Result<Vec<Record>, StoreError>;
    async fn list(&self, filter: &ListFilter) -> Result<Vec<SessionSummary>, StoreError>;
    async fn rename(&self, loc: &SessionLocator, name: &str) -> Result<SessionLocator, StoreError>;
    async fn delete(&self, loc: &SessionLocator) -> Result<(), StoreError>;
}
```
The record is the **item**, not the provider message. `core::project::to_messages(&[Record]) -> Vec<Message>` is the single projection (groups a round's assistant items into one assistant message, tool outputs + steered/contributed user items into the following user message with tool_results first, honours `Compacted`/`Rewound` boundaries, drops `PermissionReceipt`). This kills "one message: five shapes / transcript: five formats" and persists the compact boundary (gap P1 #8). Bundled store: JSONL append + sidecar lock (old `transcript.rs`, ported).

### 3.11 `Compactor`

```rust
#[non_exhaustive]
pub enum CompactReason { Threshold, Overflow { server_message: String }, Manual { instructions: Option<String> } }
pub struct CompactContext<'a> { pub items: &'a [Item], pub usage: &'a ContextUsage, pub model: &'a ModelCapabilities,
                                pub provider: Arc<dyn Provider>, pub request_for: &'a dyn Fn(&[Item]) -> ModelRequest, pub cancel: CancellationToken }
pub struct Compaction { pub summary: ItemBody /* Item::Compaction{...} */, pub keep_from: ItemSeq, pub before: u64, pub after: u64 }
#[async_trait]
pub trait Compactor: Send + Sync {
    fn threshold(&self, model: &ModelCapabilities) -> u64;   // when Threshold fires
    async fn compact(&self, cx: CompactContext<'_>, reason: CompactReason) -> Result<Compaction, CompactError>;
}
```
Kernel owns the ruler (`ContextUsage`: local estimate anchored on server `input_tokens`, `count_tokens` every 5 rounds or +20K, learned windows from 400 bodies) and the breaker (3 consecutive failures, decays per accepted request). The plugin owns the strategy (summary prompt, keep-12 + token-capped tail, orphan-safe split, overflow ladder summary→truncate blocks→drop oldest). Observability (`before/after/replaced/duration`) is in `Item::Compaction` (P0 #4).

### 3.12 `Command` — slash commands

```rust
pub struct CommandSpec { pub name: String, pub aliases: Vec<String>, pub hint: String, pub args: ArgSpec, pub instant: bool /* runs during a busy turn */, pub family: String }
#[non_exhaustive]
pub enum CommandOutcome { Applied { message: Option<String> }, View(View), Prompt(String) /* becomes a turn */, Operation(OperationId) }
#[async_trait]
pub trait Command: Send + Sync {
    fn spec(&self) -> CommandSpec;
    fn complete(&self, partial: &str, cx: &CommandContext) -> Vec<Completion> { Vec::new() }
    async fn run(&self, args: &str, cx: &CommandContext) -> Result<CommandOutcome, CommandError>;
}
```
One registry serves dispatch, `action/list`, completion and the `?` panel (old had three drifting tables). Pickers (`/model`, `/provider`, `/think`, `/resume`) are `cx.ask(Prompt::Question{options})` — the same interaction registry every surface already renders — so a GUI gets them for free. `View` is a small structured type (`Text | Table | List`) rendered by each surface.

### 3.13 Memory, `SkillSource`, `SubagentRuntime` — deliberately not SDK traits

- **Memory** = `ContextContributor{Placement::System}` (recall: CLAUDE.md/AGENTS.md, memdir, BM25) + `Hook::on_turn(End)` (extract). A `Memory` trait would restate those two.
- **SkillSource** is exported by `bingo-skills` (`trait SkillSource { fn skills(&self, cwd) -> Vec<Skill> }`) and registered as a service; the `Skill` tool and `/skills` consume it; a marketplace plugin implements it. Not SDK because only skills-family plugins consume it.
- **SubagentRuntime** is `ToolContext::spawn_session` + `submit` + the queue. Running a sub-agent as an external ACP process is a *different `Agent` tool*, not a runtime abstraction.

---

## 4. The one event model

```rust
// sdk::event — the only thing that crosses kernel → client. Serde + schemars; the app-server wire *is* this.
#[non_exhaustive]
pub struct Event { pub seq: u64, pub ts: UnixMillis, pub session: SessionId, pub cause: Option<Cause>, pub kind: EventKind, pub meta: Map }
pub enum Cause { Submission(RequestId), Operation(OperationId), Interaction(InteractionId), Plugin(String) }

#[non_exhaustive] #[serde(tag = "type", rename_all = "camelCase")]
pub enum EventKind {
    SessionStarted { info: SessionInfo },         // parent: Option<SessionId>, key, cwd, title, locator, origin: Option<ItemId> (the tool call that spawned it)
    SessionUpdated { info: SessionInfo },
    SessionEnded   { reason: CloseReason },
    TurnStarted    { turn: TurnInfo },            // id, origin: Composer|Queue|Peer|Contributor|Shell|Auto
    TurnRound      { turn: TurnId, round: u32 },
    TurnRetrying   { turn: TurnId, round: u32, attempt: u32, max: u32, delay_ms: u64, removed: Vec<ItemId>, reason: Option<String> },
    TurnUsage      { turn: TurnId, usage: Usage, context: ContextUsage },
    TurnEnded      { turn: TurnId, status: TurnStatus },   // Completed | Failed{code,msg} | Interrupted{reason} | Cancelled
    ItemStarted    { item: Item },
    ItemDelta      { item: ItemId, delta_seq: u64, delta: Delta },   // append: Text | Reasoning
    ItemUpdated    { item: Item },                 // replace (status change, progress tail)
    ItemCompleted  { item: Item },                 // authoritative over every delta before it
    QueueAdded     { entry: QueueEntry },
    QueueRemoved   { id: QueueId, reason: QueueRemoval },  // Drained{turn} | Reclaimed | Cleared | Absorbed{turn, item}
    InteractionOpened { interaction: Interaction },        // Permission | Question | Confirmation, with advertised decisions + remaining_guard_ms
    InteractionClosed { id: InteractionId, outcome: InteractionOutcome },  // Resolved{decision, receipt: Option<ItemId>} | Cancelled{reason}
    OperationStarted { op: Operation }, OperationProgress { id: OperationId, progress: Progress }, OperationEnded { op: Operation },
    ConfigChanged  { scope: ConfigScope, value: Value },   // Kernel | Plugin(id)
    CatalogChanged { kind: CatalogKind, catalog: Catalog },
    Notice         { level: Level, code: String, text: String, item: Option<ItemId> },   // transient; history notices are Item::Notice
    Extension      { plugin: String, kind: String, payload: Value },   // plugin resources: rooms, tasks, deliveries, roster
}

#[non_exhaustive] #[serde(tag = "type")]
pub enum ItemBody {
    User        { parts: Vec<ContentPart>, origin: Origin },
    Assistant   { text: String },
    Reasoning   { text: String, signature: Option<String> },
    ToolCall    { call: ToolCallId, name: String, input: Value, output: Option<ToolOutput>, progress: Option<Progress>, duration_ms: Option<u64> },
    Compaction  { summary: String, replaced: u32, before: u64, after: u64, duration_ms: u64 },
    Rewind      { to: ItemSeq, mode: RewindMode, removed: u32 },
    Interruption{ marker: String },
    Notice      { code: String, level: Level, text: String },
    QuestionAnswer    { interaction: InteractionId, question: String, answer: String },
    PermissionReceipt { interaction: InteractionId, tool: String, decision: DecisionKind, feedback: Option<String> },
    Asset       { asset: AssetId, label: Option<String> },
}
pub struct Item { pub id: ItemId, pub seq: ItemSeq, pub turn: Option<TurnId>, pub round: u32, pub status: ItemStatus, pub started_at, pub completed_at, pub body: ItemBody, pub meta: Map }
```

Ids (`SessionId`, `TurnId`, `ItemId`, `InteractionId`, `OperationId`, `QueueId`, `AssetId`) are opaque prefixed strings minted only by the actor (old `IdMint`, kept). `ToolCallId` is the provider's string. There is **no `ConversationId`**: a conversation is a session; a sub-agent is a child session; the root's subscribers receive children's events (`session` field differs) when `SubscribeOptions.children = true`. Rooms are `Extension` resources plus submissions to member sessions.

Why this is not the old four-enum chain: `ModelEvent` is an *input* to the loop and never published; `Event` is the only output; the TUI, `--print`, app-server, ACP and channels all consume `Event` (no `UiEvent`, no `EngineEvent`, no `AppEventPayload`). Every fact appears once: a text delta is `ItemDelta` and nothing else; a tool's success is `Item.status`, not a second status beside it.

How the five cases appear:

| Case | Sequence |
|---|---|
| Tool call | `ItemStarted{ToolCall, status: Pending}` → (gate) `ItemUpdated{status: Running}` → `ItemUpdated{progress: tail}`* (replace) → `ItemCompleted{output, status: Completed\|Failed\|Interrupted}`; denied: `ItemCompleted{status: Failed, output: permission_error}` |
| Permission request | after `ItemStarted{ToolCall}`: `InteractionOpened{Permission{tool, preview, decisions:[AllowOnce,AllowSession?,Deny], scope?, remaining_guard_ms}}` → client `respond()` → `InteractionClosed{Resolved{decision, receipt}}` + `ItemCompleted{PermissionReceipt}` → tool proceeds or fails. Sub-agent prompts carry the child `session`; the root's interactive surface answers; the registry serialises them (one open head). |
| Sub-agent run | parent: `ItemStarted{ToolCall Agent}`; child: `SessionStarted{parent, origin: item}` → its own `TurnStarted/Item*/TurnEnded` → parent `ItemCompleted{ToolCall}` (sync) or `Extension{bingo.agents, "finished"}` + a contributed `User` item in the parent at the next round (async). ACP flattens the child under the parent tool call with `_meta.bingo.session`. |
| Compaction | `Notice{COMPACTING}`? no — `OperationStarted{Compact}` only when manual; always `ItemCompleted{Compaction{before, after, replaced}}` + `TurnUsage{context}`; the store gets `Record::Compacted{boundary}`. |
| Queued / steered message | `submit` while busy → `QueueAdded{entry, steerable}`; at the barrier → `QueueRemoved{Absorbed{turn, item}}` + `ItemCompleted{User{origin: Steer{queue}}}`; at turn end → `QueueRemoved{Drained{turn}}` + `TurnStarted{origin: Queue}`; pull-back → `QueueRemoved{Reclaimed}` or `Reclaim::Absorbed` error. |

Transport rules the app-server surface keeps from the old spec: `seq` gapless per epoch, `coalesced_from` when a transport merges deltas, bounded per-attachment channel (`CLIENT_TOO_SLOW`), snapshot cut on attach, stderr for diagnostics.

---

## 5. The turn loop as a state machine

```
Idle
 └─ submit(Composer|Raw) ──► Opening
Opening        : Hook::on_submit (Deny→Notice, Redirect→submit elsewhere, rewrite) ; commit Item::User ; open Turn (guard) ; Hook::on_turn(Start) ; Record::TurnOpened
 └──────────────────────────► Assembling
Assembling     : Contributors{System, RoundStart} ; measure ContextUsage (anchor, count_tokens cadence) ; if usage ≥ compactor.threshold → Compacting(Threshold)
 └──────────────────────────► Streaming
Streaming      : provider.stream(req, cancel) ; fold ModelEvent → Items (delta/complete) ; retry ladder (10× jittered, server retry_after clamped 60s; TurnRetrying withdraws items by id)
 ├─ Error(ContextOverflow) ─► learn window ; Compacting(Overflow) ; back to Streaming once ; second overflow → Closing(Failed)
 ├─ cancel ─────────────────► Interrupted{keep signed text/thinking, drop unpaired tool_use, Item::Interruption marker}
 └─ Stop ───────────────────► Deciding
Deciding       : empty assistant & no tools → retry once, then Closing(Completed, EmptyResponse)
                 no tools & MaxTokens & recoveries<3 → inject continue piece → Assembling
                 no tools → Hook::on_stop ; Block once → inject reason → Assembling ; else Closing(Completed)
                 tools → Gating
Gating         : per call, serial: Hook::before_tool (rewrite/Deny/Ask) → policy.decide(traits, subjects, confirm) → Ask ⇒ InteractionOpened(preview, scope) → Verdict → policy.on_verdict ; Deny ⇒ Item Failed{permission_error + guidance} ; unknown tool ⇒ Item Failed
 └──────────────────────────► Executing
Executing      : executor: consecutive concurrency_safe calls in parallel (≤10), others serial; child cancel tokens; an interrupt drops every call in flight (a tool has no say); completed results kept; every tool_use answered (placeholder for unanswered)
 ├─ cancel ─────────────────► Barrier(interrupted=true)
 └──────────────────────────► Barrier
Barrier        : Hook::after_tool (Block ⇒ end after this round) ; if continuing: Contributors{Barrier} ; queue.absorb(steerable prefix) → User items ; commit tool-result user message ; TurnRound
 ├─ interrupted | blocked | cancelled ─► Closing(Interrupted | Completed)
 └──────────────────────────► Assembling
Compacting     : Hook::on_compact(Before) ; compactor.compact(reason) ; Item::Compaction ; Record::Compacted ; Hook::on_compact(After) ; breaker
Closing        : exactly one TurnEnded (guard Drop fallback = Failed{TURN_LOST, panic text}) ; Hook::on_turn(End) *after* the terminal event ; queue.drain_front → Opening | Idle
```

Kernel constants that stay in core because they are loop mechanics, not features: interrupt markers, `MAX_TOKENS_RESUME_PROMPT`, empty-response retry (1), max-tokens recovery (3), result clip (50K global; `SelfBounded` tools exempt), `MAX_CONCURRENCY`. Everything that names a feature (inbox, mail, hires, task reminder, team norms, experience recall, model capability block) is a contributor or hook registered by a plugin. The loop signature: `core::turn::run(session: &SessionCtx, turn: TurnId, registry: &Registry, cancel: CancellationToken) -> TurnStatus`.

---

## 6. Crate layout

```
bingo-improve/
  Cargo.toml                      workspace; edition = "2024"; [workspace.lints] unsafe_code = "forbid", clippy::unwrap_used/expect_used = "deny" (allowed in cfg(test))
  crates/
    bingo-sdk/                    STABLE. ids, model (Message/ContentPart/SystemBlock), event, item, traits (§3), Registrar/Contribution, HostApi/HostHandle,
                                  Submission/Disposition, errors (+ stable error codes), schema helpers, `testing` feature (FakeHost, ScriptedProvider, RecordingSurface).
                                  deps: serde, serde_json, schemars, thiserror, async-trait, tokio(sync), tokio-util(sync), futures-core. No reqwest, no ratatui.
    bingo-core/                   kernel: session actor, id mint, turn loop, gate, executor, accumulator, context ruler, projection items→messages, config layering, plugin host.
                                  deps: sdk, tokio, tokio-util. Tests drive it with sdk::testing fakes; no network, no disk.
    plugins/
      bingo-providers/            Provider impls: anthropic (SSE, count_tokens), openai-responses (+codex), OAuth (PKCE/device/manual, auth.json 0600, 5-min refresh),
                                  presets, model catalog + /v1/models cache, learned windows, vision gating. deps: reqwest.
      bingo-tools-fs/             Read (image blocks), Glob, Grep, Edit, Write (preview diff, checkpoint service use), AskUserQuestion. Exports `Checkpointer` service? no → see bingo-session.
      bingo-tools-shell/          Bash (process group, timeout, truncation, interactive deny list, periodic auto-background, notify conditions, live tail), Command "!",
                                  Background service (watch registry: state machine, conditions, notifications as a Barrier contributor).
      bingo-tools-web/            WebFetch (preapproved domains via subjects+policy), WebSearch. deps: reqwest, html2md.
      bingo-permissions/          the CC-compatible PermissionPolicy, modes, rule tables, sensitive dirs, /permissions, /permission-mode, session rules.
      bingo-hooks-shell/          settings `hooks` → Hook impl over subprocesses.
      bingo-session-jsonl/        SessionStore (JSONL + lock), rewind checkpoints (exports `Checkpointer` service), assets store (content-addressed, #[image N]), GC, /resume /rename /rewind.
      bingo-context/              compactor (thresholds, keep-tail, overflow ladder), memory (CLAUDE.md/AGENTS.md, memdir, BM25 recall contributor + extract hook), /compact.
      bingo-skills/               SkillSource trait + loaders (user/project/bundled), Skill tool, /skills, `guide` bundled skill.
      bingo-mcp/                  rmcp client; one Tool per remote tool (trusted_traits=false); /mcp; concurrent dial at start.
      bingo-agents/               Agent tool (spawn_session), named defs (.bingo/agents), SendMessage (submit to session), AgentControl, roster Extension events, @name on_submit Redirect.
      bingo-rpc/                  (phase 2) external plugin bridge: plugin.json discovery, JSON mirror of Contribution/Event/ToolCall over stdio.
      bingo-tui/                  Surface "tui" (ratatui, crossterm). Reads Event only. Big; must be its own crate.
      bingo-print/                Surface "print".
      bingo-app-server/           Surface "app-server": JSON-RPC/NDJSON, methods = HostApi 1:1, notifications = Event, schema generation from sdk types.
      bingo-acp/                  (later) Surface "acp" on the official Rust SDK; lossy projection per the old research note.
      bingo-teams/, bingo-rooms/, bingo-tasks/, bingo-experience/   (later; see §7)
  bin/bingo/                      clap CLI, settings paths, composes Vec<Box<dyn Plugin>>, picks the surface (--print | tui | app-server | acp), `update` subcommand.
```

Dependency direction: `bin → {core, plugins}`; `plugins → sdk` only (a plugin may depend on another plugin crate solely to import an exported service trait, e.g. tools-fs → session-jsonl for `Checkpointer`; prefer moving such traits to a tiny `bingo-services` crate if a third consumer appears); `core → sdk`. Never `plugin → core`, never `anything → tui`.

What forces the split: (1) the old tool→tui dependency (`tool/team.rs:329`, `tool/diff.rs`) is impossible by construction; (2) 55K lines of TUI and `reqwest`/`rmcp`/`ratatui` each compile in parallel and are not relinked when a tool changes (old: 13 GB `target`, full relink on any `app/` edit); (3) integration tests run in-process against `bingo-core` with `sdk::testing` fakes instead of spawning the binary; (4) the SDK crate is what an external plugin author downloads — it must not pull ratatui.

---

## 7. Where each old feature lands

| Feature (survey) | Lands | Note |
|---|---|---|
| Domain ids, `NeutralRequest`/`StreamEvent`/accumulator, error-code registry | kernel / `bingo-sdk` | ported; `ModelEvent` is the old `StreamEvent` |
| `AppCore` actor, turn guard, interaction registry (confirm guard 400 ms, answered-once), queue barrier/reclaim | kernel `bingo-core` | port the invariants, drop `ConversationId`/`ConvKey` |
| query_loop / query_turn (stream retries, overflow retry, interrupt markers, pairing fill) | kernel | as the state machine; feature branches removed |
| Tool executor (safe batching, cancel keeps completed) | kernel | + typed `Interrupt` per tool |
| `can_use_tool` semantics, 5 modes, rules, Bash splitting, sensitive dirs, MCP hint distrust, session allow rule | bundled `bingo-permissions` | matching over `Subject`s |
| Permission dialog (yes / session / no+feedback, Ctrl+E diff) | `bingo-tui` (render) + kernel interaction (semantics) | GUI gets the same via `InteractionOpened` |
| Hooks 10 events, exit-2 contract | bundled `bingo-hooks-shell` | TaskCreated/Completed via `on_event` |
| Anthropic / OpenAI Responses / Codex, OAuth, presets, providers config, model catalog, `/v1/models` cache, learned windows, vision gating, retry ladder | bundled `bingo-providers` | learned windows *storage* here, *use* in kernel ruler |
| Bash (all sub-features), `!` shell mode, live tail, background/watch registry, periodic auto-bg | bundled `bingo-tools-shell` | watch registry becomes a service + Barrier contributor |
| Read/Glob/Grep/Edit/Write, dry-run diff, AskUserQuestion | bundled `bingo-tools-fs` | rewind snapshot via `Checkpointer` service |
| WebFetch (40 preapproved domains), WebSearch | bundled `bingo-tools-web` | preapproval = the policy's allow rules seeded by the plugin |
| Skill tool, SKILL.md layers, args, bundled `guide` | bundled `bingo-skills` | |
| MCP stdio/http, concurrent dial, `/mcp` | bundled `bingo-mcp` | |
| Transcript JSONL, compact marker, sidecar lock, `--continue/--resume`, rename, GC | bundled `bingo-session-jsonl` | records are Items; boundary persisted |
| Rewind (Esc Esc, code/conversation/both, pre-image snapshots) | bundled `bingo-session-jsonl` | `Record::Rewound`; Bash writes still uncovered (documented) |
| Images: content-addressed assets, `#[image N]`, kitty rendering | assets in `bingo-session-jsonl`; rendering in `bingo-tui` | `ContentPart::Image{asset}` |
| Compaction (cadence, 90 %, keep 12, breaker, overflow ladder) | ruler+breaker kernel; strategy `bingo-context` | observable via `Item::Compaction` |
| Memory: memdir extract, CLAUDE.md/AGENTS.md, BM25 recall | bundled `bingo-context` | fix P0 #1 (recent-first truncation, byte cap, git common root) while porting |
| Sub-agents: Agent tool, named defs, model/provider/thinking override, async + notify, completion injection | bundled `bingo-agents` | on `spawn_session` + queue + contributor |
| SendMessage, AgentControl, ack tracking (queued/delivered/answered/dropped, 300 s × 3 chase) | `bingo-agents` (later phase) | ack = derived from `QueueRemoved{Absorbed}` / `TurnEnded` events; chase = a Hook::on_event timer |
| Teams: team.json, crew/hire, norms, org tree, team memory | later plugin `bingo-teams` | crew = child sessions opened at `start()`; norms = a System contributor; memory = a contributor |
| Rooms: serial/free, expiry, cursors, budget, `@debt` watchdog | later plugin `bingo-rooms` | room log = plugin state + `Extension` events; posts = `submit(member, Raw{origin: Peer})`; wake = queue semantics |
| Experience library (5 tools, BM25, lifecycle, evidence hashes) | later plugin `bingo-experience` | tools + a recall contributor |
| Tasks ×4, Ctrl+T panel, reminders | later plugin `bingo-tasks` | reminder = RoundStart contributor; panel = TUI reading `Extension` |
| TUI (editor, keybindings, slash completion, `@` completion, themes, markdown/highlight, diff, pager, pages, roster, background dialog, pickers, status line, motion, OSC notify, title) | bundled `bingo-tui` | pickers via `Prompt::Question`; pages = per-session filters; roster/rooms rendered from `Extension` by feature-specific TUI modules that ship with those plugins' crates (`bingo-tui` exposes a small `Widget` extension point — a later decision, see §8) |
| Avatars (experimental) | later, inside `bingo-teams`'s TUI module | not in tools, not in kernel |
| `--print` | bundled `bingo-print` | |
| `bingo app-server` (JSON-RPC, 23 methods/39 notifications, schema gen) | bundled `bingo-app-server` | methods = `HostApi`; notifications = `Event`; schema from sdk |
| ACP | later `bingo-acp` | lossy projection; `_meta.bingo.*` |
| OpenClaw / Hermes integration | later: a `Concurrent` surface per gateway, or run `bingo app-server` as their CLI backend | no kernel change |
| `bingo update` | bin | |
| Config: 23 keys, 3 layers | kernel loader + per-plugin `ConfigClaim` with `Merge` | tri-state via `Option<T>` and explicit `null`-clears in the loader |
| `bingo share`, share HTML/upload | **dropped** | |
| Old app/ sediment (buffer/bufferview/conv/zoom/tree), `UiEvent`, `EngineEvent`, `AppEventPayload`, second `PermissionMode`/`ThinkingLevel`/`ShellDialect` | **dropped** | replaced by `Event` and single enums in sdk |

**The minimal kernel primitive for teams/rooms.** Three bricks, all already required for single-agent use: (1) `open_session(SessionSpec{parent, key, toolset, system_extra, model})` returning a session whose events flow to the root's subscribers; (2) `submit(any_session, Raw{origin: Peer{from}})` with the queue's idle-starts/busy-absorbs semantics (this *is* the inbox and the mail block, so `InboxWake`, `drain_main_mail`, `flush_agent_inbox` collapse to one thing); (3) `Extension` events + services for plugin-owned resources. Rooms, acks, chases, crews, hires, org trees are then pure plugin state derived from the event log — no kernel edit is needed for any of them.

---

## 8. Risks and open questions (with recommendations)

1. **Items as the durable record may lose provider-private replay data** (Anthropic thinking signatures are covered by `Reasoning.signature`; OpenAI Responses encrypted reasoning / future item kinds are not). *Recommendation:* add `ContentPart::Opaque { provider: String, payload: Value }` now; the projection passes it back only to the same provider id. Cheap, and it keeps "one record".
2. **`async_trait` vs native AFIT for a public plugin ABI.** Macro-generated `Pin<Box<dyn Future>>` signatures are stable but leak into docs and error messages. *Recommendation:* ship with `async_trait`; the SDK is 0.x until the first external plugin exists, so switching later is a minor bump, and no method shape changes.
3. **Where TUI rendering of plugin resources lives** (roster, rooms, task panel). Either the TUI knows every `Extension` kind (kernel-of-the-TUI grows like the old one), or plugins ship TUI widgets (tools→tui dependency returns, inverted). *Recommendation:* `bingo-tui` exposes one `Widget` trait for `Extension` kinds and plugins provide their widget in a *separate* crate (`bingo-tasks-tui`) that depends on both; the bin composes. Tools never see the TUI.
4. **Out-of-process plugin protocol timing.** Defining the JSON mirror early risks freezing SDK types before they settle; late means MCP and shell hooks are written twice. *Recommendation:* MCP and shell hooks are ordinary in-process plugins in phase 1 (they already bridge foreign processes with their own contracts); `bingo-rpc` waits until `Event`/`Contribution` have survived the TUI, app-server and agents plugins.
5. **Sub-agents as child sessions changes the `--resume` shape** (a root session now has children with their own locators) and the ACP mapping (one ACP session = one root session; children flatten). *Recommendation:* the store persists children under the root's directory keyed by `SessionInfo.key`; `load` returns the tree; ACP flattens with `_meta`. Decide before writing `bingo-session-jsonl`, not after.

Sequencing an implementer can start from: `bingo-sdk` (model, event, traits, testing fakes) → `bingo-core` (actor, projection, loop, gate, executor) with scripted-provider tests → `bingo-providers` + `bingo-print` (first real turn) → `bingo-tools-fs`/`shell` + `bingo-permissions` + `bingo-session-jsonl` → `bingo-tui` → `bingo-app-server` → `bingo-context`, `bingo-hooks-shell`, `bingo-skills`, `bingo-mcp` → `bingo-agents` → later plugins.

### Critical Files for Implementation

New workspace (to be created):
- `/Users/yexrob/Episodes/Projects/bingo-inc/bingo-improve/crates/bingo-sdk/src/event.rs` — `Event`, `EventKind`, `Item`, `ItemBody`, ids (the one representation everything projects from)
- `/Users/yexrob/Episodes/Projects/bingo-inc/bingo-improve/crates/bingo-sdk/src/plugin.rs` — `Plugin`, `PluginManifest`, `Registrar`, `Contribution`, `HostApi`/`HostHandle`
- `/Users/yexrob/Episodes/Projects/bingo-inc/bingo-improve/crates/bingo-sdk/src/tool.rs` — `Tool`, `ToolTraits`, `Subject`, `ToolContext`, `PermissionPolicy`, `Hook`, `ContextContributor`
- `/Users/yexrob/Episodes/Projects/bingo-inc/bingo-improve/crates/bingo-core/src/turn.rs` — the state machine of §5 (gate, executor, barrier, closing)
- `/Users/yexrob/Episodes/Projects/bingo-inc/bingo-improve/crates/bingo-core/src/session.rs` — actor: seq mint, turn/interaction/queue registries, `items -> messages` projection, persist-then-publish

Old-tree references to port behaviour from (not architecture): `/Users/yexrob/Episodes/Projects/bingo-inc/bingo/src/api/contract.rs` (accumulator, error classification), `/Users/yexrob/Episodes/Projects/bingo-inc/bingo/src/permission.rs` (rule semantics), `/Users/yexrob/Episodes/Projects/bingo-inc/bingo/src/tool/executor.rs` (batching/cancel), `/Users/yexrob/Episodes/Projects/bingo-inc/bingo/src/app/interaction.rs` (guard, answered-once), `/Users/yexrob/Episodes/Projects/bingo-inc/bingo/src/query.rs:1238-1484` (gate/execute/barrier ordering).
