# bingo 重写计划

## 〇、背景（Context）

源项目 `/Users/yexrob/Episodes/Projects/bingo-inc/bingo` 是一个 Rust 写的本地 coding-agent harness（类 Claude Code 的 CLI/TUI）。20 天、456 次提交、148,754 行、单一 binary crate。它能用，但规划是「边做边想」长出来的：一个 575 行的 `query_loop` 触达 20 个模块，同一个事实有 4 层事件枚举、5 种消息形状、7 个会话文件；27K 行的团队/房间/经验库社交层先于内核的 P0 缺口完成；工具层反向依赖 TUI，任何改动都全量重链（13 GB target）。

目标：在 `/Users/yexrob/Episodes/Projects/bingo-inc/bingo-improve`（空仓库）**重新设计**一个同类产品。旧代码只作行为与坑的参考，架构不照搬。

用户已定的约束：
- **Rust**，edition 2024，thiserror，无 unwrap/expect（测试除外），无 unsafe。
- **core 之外一切皆插件**：工具（含 Bash/Read/Edit）、provider、界面（TUI/print/GUI 协议/IM 频道）、hooks、权限策略、存储、压缩、记忆、skills、MCP、子代理、团队——全部经**稳定 trait** 注册。
- **有一个事件中心**：所有事件经过它，RPC/TUI/GUI/IM 彼此解耦。
- **GUI-ready**，并考虑 OpenClaw / Hermes 那类 IM 网关生态，后续可方便接入；可被外部宿主（ACP）驱动。
- **协作与子代理是 first-class**（排进主线里程碑；仍以插件实现，内核只提供原语）。
- 旧功能除 `share` 外都要，但内核不认识团队/房间/经验库。
- 工程哲学：为道日损；守一（一个事实一种表示）；三生万物（先砖后墙）；契约只立在被独立消费的边界。

---

## 一、现状调研摘要（2026-08-28）

### 规模
148,754 行 / 208 文件 / 单 crate（无 lib.rs、无 workspace）；1,844 测试（44% 在 TUI）；`notes/research.md` 10,423 行 189 条决策记录。按目录：tui 54.9K (37%) · app 21.7K · tool 15.3K · api 8.9K · app_server 7.8K · engine 1.9K · 其余 ~38K。

### 功能清单（旧项目有什么）
- **入口**：TUI（全屏默认 / `--inline`）、`--print` 无头（stdout 只出正文，非 TTY `[error] code= msg=` 契约）、`bingo app-server`（JSON-RPC 23 方法/39 通知，零消费者）、`share`（砍）、`update`、`--continue/--resume`
- **Provider**：Anthropic Messages（SSE、count_tokens）、OpenAI Responses（+Codex 变体）、OAuth（PKCE/device/manual，`auth.json` 0600）、预设 codex/opencode-go、模型目录三层解析、`/v1/models` 缓存、从 400 学真实窗口、vision 门控、10 次抖动重试
- **工具 24 个**：编码 10（Bash：进程组/120s/48K/交互式拒绝表/后台/周期检测/live tail；Read 返回真实图片块；Glob；Grep；Edit/Write dry-run diff + rewind 快照；WebFetch 40 个预批准域名；WebSearch 抓 DDG；AskUserQuestion；Skill），协作记账 14（Agent、SendMessage、AgentControl、Team、Channel、Task×4、Experience×5），MCP 工具同一 trait
- **权限**：5 模式、allow/deny/ask 规则、Bash 拆子命令（deny/ask 任一命中、allow 须全覆盖）、敏感目录任何模式都问、MCP readOnlyHint 不信任、审批弹窗（Yes/本会话/No+反馈回传模型）
- **Hooks** 10 事件（JSON stdin/stdout，exit 2 阻断，PreToolUse 可改写输入）；**MCP** stdio + streamable HTTP；**Skills** SKILL.md 两层 + 内置 guide
- **会话**：JSONL 追加写 + compact marker + sidecar 锁；自动压缩 90%/保留 12 条/熔断 3 次/溢出阶梯；memdir 记忆 + CLAUDE.md/AGENTS.md + BM25 召回；Rewind（Esc Esc，Bash 写的文件不覆盖）；图片内容寻址；GC 30 天/100 个
- **多智能体 ~27K 行**：子代理（命名定义、独立 model、异步、完成通知）→ 团队（team.json、Crew/Hire、norms、组织树、按分支团队记忆）→ 房间（serial/free、`@` 债务 + 5 分钟看门狗）→ ack 追踪 300s×3 → 经验库（5 工具 + BM25 + 生命周期）→ 任务
- **TUI 54.9K 行**：readline 编辑器 + kill ring + 外部编辑器、34 快捷键、24 斜杠命令 + 参数补全、`@` 补全、主题、19 种高亮、kitty 图片、Ctrl+O 分页器、代理/房间页面、roster、后台对话框、选择器、状态动画、OSC 通知、头像
- **配置** 23 键三层合并

### 病灶
1. 社交层先于内核完成；`cc-gap-analysis.md` 的 P0（中断无类型、记忆 bug、hook ask `unreachable!`、压缩不可观测）仍开着；最后 5 条决策全是社交层 bug
2. 575 行 `query_loop`；`Session` 上帝对象 27 字段 10 句柄；query⇄engine⇄app、tool⇄query 三个环
3. 同一事实多种表示：事件 4 层、PermissionMode×2、ThinkingLevel×2、消息 5 形、会话 7 文件、转录 5 格式
4. 四代会话显示的残骸 4,134 行未清
5. 单 crate；tool→tui 反向依赖；4000 行上限被 `_tail`/`_a..g` 规避
6. 文档反超代码，文学化提交标题不可检索

### 值得保留的思想（不是代码）
provider 中立层（NeutralRequest/StreamEvent/accumulator）、Tool trait 的失败关闭默认值与 dry-run preview、执行器的安全前缀批处理、追加写 + marker 的转录、预算数学、权限规则语义、hooks 事件设计、AppCore「一份真相三个客户端」、D140–D155 的迁移纪律（先删旧、先立契约、撞墙记录、一批回滚）、错误码注册表、黑盒测试风格。

---

## 二、架构

### 2.1 一句话
**一个最小内核 = 会话 actor + 有序事件日志（事件中心）+ turn 状态机 + 权限门 + 插件宿主；其余一切是插件；每个界面都只是事件的订阅者与提交入口的调用者。**

### 2.2 内核边界（什么在 core，为什么不能是插件）

| # | 在内核 | 理由 |
|---|---|---|
| K1 | **领域词汇**（`bingo-sdk`）：ids、`Message/ContentPart`、`Item`、`Event`、`Interaction`、`Input`、`SessionState` | 两个插件要能对话，名词必须不属于任何一方。放在 sdk crate 而非 core：插件对着它编译，core 只是消费者之一 |
| K2 | **会话 actor**：id 铸造、有序日志（seq）、item/turn/interaction/queue 注册表、快照 | 顺序是全局性质。谁分配 seq、谁裁决两个竞争写谁赢，谁就是内核 |
| K3 | **turn 状态机 + 工具执行器** | 状态机是「插件被咨询的顺序」；插件不能定义插件的顺序。它不含任何功能名词（无收件箱、无雇员释放、无任务提醒——那些是插件注册的 contributor） |
| K4 | **权限门**（不是策略）：`hooks.before_tool → policy.decide → interaction.ask → policy.on_verdict → execute`，失败关闭默认值 | 「用户说不」在此唯一执行；策略（模式、规则表、敏感目录）是插件；无策略时门拒绝一切非只读 |
| K5 | **插件宿主**：manifest 校验、capability 拓扑排序、`register → start → stop`、配置分层与命名空间、service 定位、`HostApi` | 插件注册进的那个东西；也拥有唯一提交入口与唯一订阅入口 |

诱惑与拒绝：持久化不是内核（`SessionStore` 插件，actor 先持久化后发布；空存储时内核不变）；Bash 不特殊（`!` 是 Bash 插件注册的 Command，live tail 是 `ToolContext::progress()`）；子代理不需要内核注册表（只需 `open(Create{parent})` + `submit`；收件箱就是内核队列）；压缩策略是插件，但**尺子**（ContextUsage、服务端锚定估算、learned windows）在内核，因为两把尺子是旧 bug（D172）；权限模式是策略插件的配置，内核不枚举模式（旧代码 PermissionMode 有两份）。

### 2.3 事件中心（用户建议的落地）

不是一条全局无类型总线（那会重新引入「谁先谁后」问题），而是：
- **每个会话一条有序日志** `Journal`：`Frame{seq, ts, session, event}`，seq 由 actor 在一把锁下分配，无缝；durable 帧落盘，ephemeral 帧（`ItemDelta/Notice/Lagged`）只走内存。
- **每个订阅者一条有界通道**：溢出时内核发 `Lagged{from,to}`，客户端回读日志；**内核永不阻塞在客户端上**。
- **快照切口**：`subscribe` 返回 `(SessionState @ seq, stream of frames > seq)`，客户端视图 = `fold(snapshot, frames)`，用与内核同一个 reducer `SessionState::apply`。
- **网关级流** `GatewayEvent`：会话创建/删除、目录变更。
- **唯一入口**：`submit / interrupt / answer` 三个写操作，**同步、不返回回执**，结果以 `Event::IntentAck{intent}` 回来。这条规则使旧项目 D151 那堵墙（同步按键处理器等异步回执）无法表达。
- 所有生产者（turn loop、工具经 ctx、插件经 `Extension`）都经 actor 发布；所有消费者（TUI、print、RPC、ACP、IM）都经 `HostApi::subscribe`。TUI 进程内直接调 trait；跨进程走同一契约的 JSON-RPC 镜像。

### 2.4 插件机制（已定）

| 方案 | 结论 |
|---|---|
| (a) 进程内静态 crate，`fn plugin() -> Box<dyn Plugin>`，bin 组装 | **主机制，v1** |
| (b) 跨进程 JSON-RPC（stdio/WS） | MCP 与 shell hooks 是 day-one 的两个实例（各自已有契约）；**通用插件桥 `bingo-plugin-rpc`**（`plugin.json` 发现 + SDK 类型 JSON 镜像）等 trait 经 TUI/RPC/agents 三个插件验证后再加（M14） |
| (c) WASM | 不规划；seam 相同，将来加不改 SDK |
| (d) dlopen/abi_stable | 拒绝（unsafe） |

```rust
pub struct PluginManifest { id: &'static str, version, sdk: &'static str /* semver req */,
    provides: &'static [&'static str] /* "tool:Bash" "command:!" "service:bingo.checkpoint" "provider:anthropic" */,
    requires: &'static [&'static str] /* 缺失 → 插件禁用 + Notice{PLUGIN_UNMET}，不崩 */,
    config: Option<ConfigClaim> /* 认领的顶层 settings 键 + Merge{Replace|Accumulate|ByName} + schemars schema */ }
#[async_trait] pub trait Plugin: Send + Sync + 'static {
    fn manifest(&self) -> &'static PluginManifest;
    fn register(&self, reg: &mut Registrar<'_>) -> Result<(), PluginError>;   // 同步、依赖序、只注册不 I/O
    async fn start(&self, host: HostHandle) -> Result<(), PluginError> { Ok(()) } // 全部注册后；可 spawn（MCP 拨号、OAuth 刷新）
    async fn stop(&self, deadline: Duration) -> Result<(), PluginError> { Ok(()) }
}
#[non_exhaustive] pub enum Contribution { Tool(Arc<dyn Tool>), Provider(Arc<dyn Provider>), Policy(Arc<dyn PermissionPolicy>),
    Hook(Arc<dyn Hook>), Context(Arc<dyn ContextContributor>), Command(Arc<dyn Command>), Surface(Arc<dyn Surface>),
    Store(Arc<dyn SessionStore>), Compactor(Arc<dyn Compactor>), Service { key: &'static str, value: Box<dyn Any + Send + Sync> } }
```
跨插件依赖只经 **service**（提供方 crate 导出 trait，如 `Checkpointer`、`SkillSource`、`Background`），消费方 `host.service::<Arc<dyn T>>()`；这些 trait 不进 SDK（只被同族插件消费）。配置：内核加载三层，按认领键与 `Merge` 合并，按 schema 校验，切片交给插件；未认领键报 unknown-key；tri-state（`Option<T>` + 显式 null 清空）从第一天做。

### 2.5 稳定 trait 集（`bingo-sdk`）

通则：全部 object-safe，`Arc<dyn T>` 持有；async 用 `#[async_trait]`（dyn-compatible AFIT 稳定后去宏不改签名）；取消用 `CancellationToken`；扩展靠 `#[non_exhaustive]` + 默认方法 + `meta: Map`（即 ACP 透传的 `_meta`）。

**Provider**（流事件模型镜像 Vercel `@ai-sdk/provider` V4 的 `StreamPart`——它是 30+ provider 归一化后的形状，带每块 id、工具输入流、审批请求、`{unified, raw}` 结束原因、嵌套用量；litellm 的 OpenAI-delta 形状丢块 id 与推理签名，不用）
```rust
pub struct ModelRequest { model, max_tokens, system: Vec<SystemBlock{text, cache}>, messages: Vec<Message>, tools: Vec<ToolSpec>, reasoning: Option<Effort>, provider_options: Map /* 按 provider 键控 */ }
#[non_exhaustive] pub enum ModelEvent {                    // 只在 loop 内，永不发布
    StreamStart{warnings}, ResponseMetadata{id, model, ts},
    TextStart{id, meta}, TextDelta{id, delta}, TextEnd{id, meta},
    ReasoningStart{id}, ReasoningDelta{id, delta}, ReasoningEnd{id, meta /* Anthropic signature / OpenAI encrypted 等 provider 私有数据 */},
    ToolInputStart{id, name}, ToolInputDelta{id, delta}, ToolInputEnd{id},
    ToolCall{id, name, input: String /* 原始 JSON，边界处解析一次 */}, ToolResult{id, result, is_error} /* provider 侧执行的工具 */,
    File{media_type, data}, Source{..}, Raw(Value),
    Finish{usage: Usage{input:{total,no_cache,cache_read,cache_write}, output:{total,text,reasoning}}, finish_reason:{unified: Stop|Length|ToolCalls|ContentFilter|Error|Other, raw}},
    Error{message, retryable, retry_after?} }
// Message/ContentPart 每条、每块都带 provider_options(入) / provider_metadata(出)，按 provider id 键控；fold 时只回传给同一 provider——签名、加密推理由此往返，不需要 Opaque 变体
#[async_trait] pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self, model: &str) -> ModelCapabilities;   // window, max_output, images, thinking, count_tokens, caching
    async fn stream(&self, req: &ModelRequest, cancel: CancellationToken) -> Result<ModelStream, ProviderError>;
    async fn count_tokens(&self, req: &ModelRequest) -> Result<u64, ProviderError> { Err(Unsupported) }
    async fn models(&self) -> Result<Vec<ModelInfo>, ProviderError> { Ok(vec![]) }
    fn auth(&self) -> AuthStatus { NotApplicable }
    async fn login(&self, cx: &LoginContext) -> Result<(), ProviderError> { Err(Unsupported) }  // OAuth 经 cx.prompt()/open_url()
}
```
无 `complete_text`：就是 `stream` 排干（旧 D171：非流式在代理处被掐）。`ProviderError` 保留稳定错误码。

**Tool**
```rust
#[non_exhaustive] pub struct ToolTraits { concurrency_safe: bool /*false*/, read_only: bool /*false*/, destructive: bool, edit: bool,
    interrupt: Interrupt /* Block: 让进行中的调用跑完，跳过其余; Cancel: 丢弃 */, result_limit: ResultLimit /* Global | SelfBounded */,
    trusted_traits: bool /* MCP 为 false：readOnlyHint 永不被门信任 */ }
#[non_exhaustive] pub enum Subject { Path(PathBuf), Command(String), Url(String), Name(String) }   // 规则匹配的对象
#[async_trait] pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;                                            // name, description, input_schema(schemars), meta
    fn traits(&self, input: &Value) -> ToolTraits { default }              // 失败关闭
    fn subjects(&self, input: &Value, cwd: &Path) -> Vec<Subject> { vec![] } // Bash→[Command]，Edit→[Path]，WebFetch→[Url]，Skill→[Name]
    fn confirm(&self, input: &Value) -> Option<String> { None }            // 只有人能做的决定；任何模式都弹窗，规则不能预授权
    fn preview(&self, input: &Value, cwd: &Path) -> Option<Preview> { None } // dry-run：Diff | Command | Text
    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError>;
}
pub struct ToolOutput { parts: Vec<ContentPart>, is_error: bool, display: Option<Display>, meta }  // parts 与模型消息同一类型
```
相对旧 trait 的改进：`subjects()` 让策略不再 `match` 工具名；`traits()` 合并四个布尔并补上 `interrupt`/`result_limit` 两个 P0；输出与消息同型。

**ToolContext**（工具能触达的一切；旧的 13 个字段里任务存储/hooks 配置/权限模式串/TUI 信号/头像表全部删掉，改为 service）
```rust
pub struct ToolContext { call, session, turn, item, cwd, env: Arc<Env> /* dirs, reqwest, shell dialect */, cancel: CancellationToken, session_info, host: HostHandle }
impl ToolContext {
    pub async fn ask(&self, prompt: Prompt) -> Verdict;                       // Question/Confirmation，走交互注册表
    pub fn progress(&self, tail: Progress);                                   // ItemUpdated（替换语义）= live tail
    pub async fn record(&self, body: ItemBody) -> ItemId;                     // 调用之外写一条 item（后台完成）
    pub async fn spawn_session(&self, spec: SessionSpec) -> Result<SessionHandle>;  // 子代理原语
    pub fn submit(&self, to: SessionId, intent: IntentId, input: Input);      // 对等消息原语（同步，ack 走事件）
    pub fn service<T>(&self) -> Option<T>;  pub fn events(&self) -> EventStream;
}
```

**PermissionPolicy**（同一时刻只有一个生效；想插一嘴的用 `Hook::before_tool`）
```rust
pub struct PolicyInput<'a> { call, traits: &ToolTraits, subjects: &[Subject], confirm: Option<&str>, session: &SessionInfo, cwd }
#[non_exhaustive] pub enum Decision { Allow{reason}, Deny{reason}, Ask{reason, scope: Option<Scope> /* 经核实的「本会话不再问」规则 */, preview: bool} }
#[non_exhaustive] pub enum Reason { Rule(String), Mode(String), Hook(String), Safety(String), ReadOnly, Confirm(String), Default }
#[async_trait] pub trait PermissionPolicy { fn id(&self) -> &str; async fn decide(&self, PolicyInput) -> Decision;
    async fn on_verdict(&self, PolicyInput, verdict: &Verdict, scope: Option<&Scope>) {} /* 装会话级规则，内核永不持久化 */ }
```
`Ask` 由**门**而非策略解决 → 旧 `unreachable!`（P0）不可表达；类型化 `Reason` 关掉 P0 #5。

**Hook**（类型化生命周期拦截；shell hooks 是其中一个插件）
```rust
#[non_exhaustive] pub enum HookOutcome { Continue, Deny{reason}, Ask{reason}, Block{reason}, Redirect{session} }
#[async_trait] pub trait Hook { fn id(&self) -> &str; fn matcher(&self) -> HookMatcher;  // 点位 + 可选工具名正则
    async fn on_submit(&self, sub: &mut Input, cx) -> HookOutcome;        // agents 插件在此把 @name 重定向
    async fn before_tool(&self, call: &mut ToolCall, cx) -> HookOutcome;  // 可改写输入
    async fn after_tool(&self, call: &ToolCall, out: &ToolOutput, cx) -> HookOutcome;
    async fn on_stop(&self, cx) -> HookOutcome;                          // Block → 再循环一次
    async fn on_turn(&self, phase: Start|End, turn, items, cx) {}        // 记忆抽取在 End
    async fn on_compact(&self, phase, cx) {}  async fn on_session(&self, phase, cx) {}
    async fn on_event(&self, event: &Event, cx) {}                       // 被动观察日志：TaskCreated/CwdChanged/PermissionRequest 免费得到
}
```

**ContextContributor**（掏空旧 575 行 loop 的那个 trait：收件箱、任务提醒、后台通知、团队规范、经验召回、模型能力块——全是 contributor）
```rust
#[non_exhaustive] pub enum Placement { System{order: i32}, RoundStart, Barrier }
#[non_exhaustive] pub enum ContextPiece { System(SystemBlock), User{parts, label} }   // User 片段落为 Item::User{origin: Contributor(id)}，转录与缓存前缀一致
#[async_trait] pub trait ContextContributor { fn id(&self) -> &str; fn placement(&self) -> Placement;
    async fn contribute(&self, q: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError>; }
```

**Command**（一张表服务分发、`action/list`、补全、`?` 面板；选择器 = `cx.ask(Question{options})`，GUI 免费得到）
```rust
pub struct CommandSpec { name, aliases, hint, args: ArgSpec, instant: bool /* 忙时可即时执行 */, family }
#[non_exhaustive] pub enum CommandOutcome { Applied{message?}, View(View /* Text|Table|List */), Prompt(String) /* 变成一轮 */, Action(ItemId) /* 长操作 = Item::Action */ }
#[async_trait] pub trait Command { fn spec(&self) -> CommandSpec; fn complete(&self, partial, cx) -> Vec<Completion> { vec![] };
    async fn run(&self, args: &str, cx: &CommandContext) -> Result<CommandOutcome, CommandError>; }
```

**Surface**（界面 = 客户端；内核除 `run` 外从不调用界面；IM 频道也是 Surface——`Channel` 不另立 SDK trait，IM 适配器抽象放在 `bingo-channels` 插件内）
```rust
#[async_trait] pub trait Surface { fn id(&self) -> &str; fn kind(&self) -> SurfaceKind /* Exclusive(占终端/stdio) | Concurrent */;
    async fn run(&self, host: HostHandle, opts: SurfaceOptions) -> Result<Exit, SurfaceError>; }
```

**SessionStore / Compactor**
```rust
#[async_trait] pub trait SessionStore { async fn create(&self, meta) -> SessionId; async fn append(&self, id, frame: &Frame);  // actor 先持久化后发布
    async fn replay(&self, id, since: Seq) -> FrameStream; async fn list(&self, filter) -> Vec<SessionSummary>; async fn delete(&self, id); }
#[async_trait] pub trait Compactor { fn threshold(&self, model: &ModelCapabilities) -> u64;
    async fn compact(&self, cx: CompactContext<'_>, reason: Threshold|Overflow{server_message}|Manual{instructions}) -> Result<Compaction, CompactError>; }
```
内核持尺子（估算锚定服务端 input_tokens、每 5 轮或 +20K 校正、learned windows）和熔断器（3 次），插件持策略（摘要提示词、保留尾巴、溢出阶梯）。观测性进 `Item::Compaction{before, after, replaced, duration}`。

**故意不做 SDK trait 的**：Memory（= System contributor + `on_turn(End)` hook）、SkillSource（`bingo-skills` 导出的 service）、SubagentRuntime（= `spawn_session` + `submit` + 队列）。

### 2.6 事件模型与会话日志（守一）

**一个会话 = 一份持久上下文 = 一条日志 = 一个 actor。** 子代理是带 `parent` 链接的子会话；没有 `ConversationId`。

```rust
pub struct Frame { seq: Seq, ts, session: SessionId, cause: Option<Cause>, event: Event }
#[non_exhaustive] pub enum Event {
    SessionUpdated(SessionSummary), SessionClosed{reason},
    TurnStarted{turn, inputs: Vec<ItemId>, origin}, TurnRetrying{turn, attempt, max, delay_ms, dropped: Vec<ItemId>},
    TurnUsage{turn, usage, context: ContextUsage}, TurnCompleted{turn, status: Completed|Failed{err}|Interrupted{reason}},
    ItemStarted(Item), ItemDelta{item, n, kind: Text|Reasoning|Tail, data} /* 瞬态 */, ItemUpdated(Item), ItemCompleted(Item) /* 权威 */,
    QueueChanged{revision, entries},
    InteractionOpened(Interaction), InteractionResolved{id, answer: AnswerSummary, by: ResolvedBy}, InteractionCancelled{id, reason},
    IntentAck{intent, outcome: TurnStarted{turn}|Queued{position}|Applied{result}|Rejected{error}},
    Compacted{generation, boundary: ItemId, summary: ItemId, kept: Vec<ItemId>}, Rewound{generation, to_turn, dropped, files_restored},
    ConfigChanged(ConfigView), CatalogChanged{kind},
    Notice{level, code, text} /* 瞬态 */, Extension{plugin, kind, payload: Value} /* 插件自有资源：roster/room/task */,
    Lagged{from, to} /* 瞬态，传输层 */ }
#[non_exhaustive] pub enum ItemBody { User{parts, origin: Origin}, Assistant{text}, Reasoning{text, signature?},
    ToolCall{call, name, input, output?, progress?, child_session?, duration_ms?}, Action{name, args, status, result?} /* login/mcp reconnect/team start */,
    Compaction{summary, replaced, before, after, duration_ms}, Rewind{..}, Interruption{marker}, Notice{code, level, text},
    QuestionAnswer{interaction, question, answer}, PermissionReceipt{interaction, tool, decision, feedback?}, Asset{asset, label?} }
pub struct Origin { surface: &'static str, principal: Option<String> /* 谁说的 */, conversation: Option<String> }
```
- ids 全部 ULID，只由 actor 铸造一次、持久化、重启不重铸。
- **日志即真相；模型上下文是派生**：`ContextView::fold(frames) -> Vec<Message>`（纯函数）：收 User/Assistant/Reasoning/ToolCall+result/QuestionAnswer；一轮的 assistant items 合并成一条 assistant message，工具结果与 steer 进来的 user items 合成下一条 user message（tool_result 在前）；`Compacted` 丢 boundary 之前的、保留 `kept`、插入 summary；`Rewound` 排除 `dropped`（仍在日志里，`history_generation` +1）。日志头带 `version`；格式变更 = 迁移器重 fold，永不原地改写。每个版本一组 golden fixture（fold 固定日志得固定消息）。
- 客户端视图是另一个纯 reducer：`SessionState::apply(&Frame)`，内核与所有客户端共用（GUI 用它的 TS 生成孪生，fixture 保证等价）。`SessionState{seq, summary, config, history_generation, tail: Vec<Item>, turn, queue, interactions, children, attention /* 派生：有开放交互或轮已结束未读 */}`。
- provider 私有回放数据（Anthropic 思考签名、OpenAI 加密推理）走 `ContentPart.provider_metadata`（按 provider id 键控），fold 时只回传给同一 provider。
- 磁盘：`sessions/<ulid>/{journal.jsonl, blobs/<blake3>, .lock}`；`.lock` 是唯一的占有声明（数据文件永不加锁，Windows 强制锁）；大输出/图片/diff 进 blobs，事件持 preview + BlobRef。默认实现是 `bingo-store-jsonl` 插件。
- **派生索引**（codex 的模式）：`sessions/index.sqlite`（rusqlite + FTS5）只存派生数据——会话列表、标题、cwd、git 分支、父子关系、全文检索。**删掉它不丢任何东西**，可从各 journal 头重建；这条不变量是它与 goose「只剩 SQLite、无法回放」的本质区别。
- **fork**：不在事件流里做 DAG（pi 的做法），而是新开一个会话，头里记 `forked_from: {session, seq}`；`/tree` 就是会话列表的树状视图。线性 seq 日志保住 GUI 的快照 + 无缝流语义。

### 2.7 客户端契约与 wire

```rust
#[async_trait] pub trait HostApi: Send + Sync {
    async fn sessions(&self, f: SessionFilter) -> Vec<SessionSummary>;
    async fn open(&self, sel: SessionSelector /* Create{cwd, key?, parent?, opts} | ById | ByKey | Latest{cwd} */, who: ClientIdentity) -> Result<Attachment, OpenError>;
    async fn close(&self, id, reason); async fn delete(&self, id);
    fn catalog(&self, k: CatalogKind /* Models|Providers|Tools|Commands|Skills|Plugins */) -> Catalog;
    fn gateway_events(&self) -> BoxStream<GatewayEvent>;
    fn service_any(&self, key: TypeId) -> Option<&(dyn Any + Send + Sync)>;
}
pub struct Attachment { session, snapshot: SessionState, events: BoxStream<Frame> /* 全部 seq > snapshot.seq */, handle: SessionHandle }
impl SessionHandle {   // 写操作同步、不阻塞、不返回回执（I3）
    pub fn submit(&self, intent: IntentId, input: Input /* Text{text, attachments, origin} | Action(Action) */);   // 内核解析 "/" "!" "@"，决定 turn/queue/steer
    pub fn interrupt(&self, intent, scope: Turn(TurnId)|Head);
    pub fn answer(&self, intent, id: InteractionId, answer: Answer, activation: Keyboard|Pointer|Programmatic);
    pub async fn history(&self, page) -> HistoryChunk;  pub async fn events_since(&self, seq) -> BoxStream<Frame>;
}
```
`IntentId` 是客户端铸造的 ULID，也是幂等键。

**Wire = JSON-RPC 2.0**，NDJSON over stdio 与 WebSocket 字节相同；13 方法 + 2 通知：
`initialize` · `session/{list,open,close,delete,events,history,submit,interrupt,answer}` · `catalog/read` · `shutdown` · 通知 `event{session, seq, ts, ...Event}` / `gateway/event`。
选 JSON-RPC 的理由：ACP 就是 JSON-RPC（`bingo-acp` 与 `bingo-surface-rpc` 共用编解码）；Codex app-server / Hermes TUI gateway 都是这个形状；OpenClaw 的 `req/res/event` 只是改名（一个 20 行的信封映射就能做 `AgentHarness`）。保留 OpenClaw 两个好主意：`initialize` 先行带能力协商、幂等键。
`bingo serve --stdio`（宿主、IDE）与 `bingo serve --ws 127.0.0.1:0 --token-file`（GUI）。`RemoteKernel` 在 wire 之上实现 `HostApi`，TUI 换一个构造器即可远程。schema 从 Rust 类型生成（JSON Schema + TS），提交并做漂移检查。M5 时评估直接复用 `agent-client-protocol` crate 内置的 JSON-RPC/传输机制（它已实现 stdio/Lines/Channel 传输且运行时无关），否则手写约 400 行——`jsonrpsee` 无 stdio 传输且一年未发版，不用。

**`--print --output-format stream-json` 是一个有损的兼容编码器**（surface-print 内），信封与 Claude Code 的 `stream-json` 一致：`system/init`（含 `session_id`、tools、mcp_servers）、`assistant`/`user`（带 `parent_tool_use_id`，子代理树由此重建）、`stream_event`、`result`（含 usage/cost）、`compact_boundary`；`--input-format stream-json` 从 stdin 收后续提交与回答。理由：OpenClaw 的 `claude-cli` 后端只认 `claude-stream-json`/`gemini-stream-json` 两种方言，兼容它 = 零插件接入；Hermes 也导入 Claude 格式转录。内核内部**不**采用 Anthropic 原始事件做事件模型——兼容编码器只是投影。`--session-id <任意冒号安全字符串>` 原样接受并映射为 `SessionKey host/<key>`（OpenClaw 的键形如 `agent:main:telegram:group:-100:topic:77`，不可强制 UUID）。`--mcp-config` 可加载宿主提供的 MCP 配置（OpenClaw `bundleMcp` 依赖它）。

**SessionKey**（路由索引，非身份）：`owner/path`，owner = 铸造它的插件 id：`tg/chat/-100123/topic/77`、`feishu/oc_abc/om_thread`、`acp/<client>/<id>`、`host/<任意>`、`agent/<team>/<name>`。内核强制首段 = 铸造插件 id。`/new` = rebind。

### 2.8 交互（权限 / 提问 / 确认 / 登录）——一个资源，不是一次调用
```rust
pub struct Interaction { id, session, turn?, item?, opened_at, guard_until: Option<Ts> /* 绝对时间；此前的 Keyboard 批准被拒 NOT_READY */,
    expires_at?, kind: Permission{tool, summary, preview: Option<Diff|Command|Url>, session_scope?} | Question{question, options, free_text, multi} | Confirm{title, detail} | Login{provider, flow: Browser{url}|Device{url, code}|Paste},
    answers: Vec<AnswerSpec> /* 内核会接受的答案，精确列出 */ }
pub enum Answer { AllowOnce, AllowSession{scope}, Deny{feedback?}, Choice{ids}, Text(String), Confirm, Cancel }
pub enum ResolvedBy { Client(ClientIdentity), Kernel /* OAuth 回调落地 */, Policy /* 规则/hook 自动 */ }
```
顺序由内核保证：ToolCall item 可见 → `InteractionOpened` 扇出到所有附着者 → 第一个有效答案赢，其余 `IntentAck{Rejected(INTERACTION_CLOSED)}` → 回执 item → `InteractionResolved{by}` → 执行或失败关闭。轮结束/中断/关闭前，内核先对每个开放交互发 `InteractionCancelled{reason}`；取消的权限 = 拒绝。`AllowSession` 装运行时规则并发 `ConfigChanged`，永不持久化。Login 的 token 永不进事件（`Redacted`）。「需要你」不是一种 kind，是 `SessionState.attention` 派生。

### 2.9 Turn 状态机（内核，无功能名词）
```
Idle ─submit→ Opening   : Hook::on_submit(Deny/Redirect/改写) ; 落 Item::User ; 开 Turn(guard) ; Hook::on_turn(Start)
Assembling              : Contributors{System, RoundStart} ; 量 ContextUsage ; ≥ compactor.threshold → Compacting(Threshold)
Streaming               : provider.stream ; fold ModelEvent → Items ; 重试阶梯(10×抖动, retry_after≤60s ; TurnRetrying 按 id 撤回)
  ├ ContextOverflow → learn window ; Compacting(Overflow) ; 再 Streaming 一次 ; 二次溢出 → Closing(Failed)
  ├ cancel → Interrupted{保留已签名文本/思考, 丢未配对 tool_use, Item::Interruption}
  └ Stop → Deciding     : 空回复→重试 1 次 ; MaxTokens 且 recoveries<3 → 注入续写 → Assembling ; 无工具 → Hook::on_stop(Block 一次) → Closing ; 有工具 → Gating
Gating                  : 逐个串行：before_tool → policy.decide(traits, subjects, confirm) → Ask ⇒ InteractionOpened → Verdict → on_verdict ; Deny ⇒ Item Failed
Executing               : 连续 concurrency_safe 并行(≤10)，其余串行 ; Interrupt::Block 跑完 / Cancel 丢弃 ; 已完成结果保留 ; 每个 tool_use 必有 tool_result
Barrier                 : after_tool(Block ⇒ 本轮后结束) ; Contributors{Barrier} ; queue.absorb(可 steer 前缀) → User items ; TurnRound → Assembling
Compacting              : on_compact(Before) ; compactor.compact ; Item::Compaction ; Event::Compacted ; on_compact(After) ; 熔断计数
Closing                 : 恰好一次 TurnCompleted（guard Drop 兜底 = Failed{TURN_LOST}）; on_turn(End) 在终态事件之后 ; queue.drain_front → Opening | Idle
```
留在内核的常量：中断标记、续写提示、空回复重试 1、max-tokens 恢复 3、结果裁剪 50K（`SelfBounded` 豁免）、并发 10、`TurnBudget{max_rounds, max_retries}`。

### 2.10 协作原语（first-class，仍是插件）

**为什么「first-class」与「在插件里」不矛盾**：内核里没有「主会话 / 子代理 / 房间」三种东西，只有一种——会话。子代理是带 `parent` 链接的会话，房间是没有模型的会话；journal 格式、`SessionState::apply`、TUI 的 `draw()`、GUI 的 store **全是同一份代码**。TUI「切到子代理」= `open(ById(child))` 换一个 session id，不存在第二条渲染路径（旧项目把 Main/Agent/Room 做成三种 `ConvKey`，才长出四代会话视图、七个文件）。所以 first-class 的**名词**在内核（会话树、parent 链接、`origin` 谁说的、向任意会话 submit、子会话事件回流根订阅者、深度/并发上限），放在插件里的只是**动词**（`Agent` 工具怎么 spawn、角色定义文件、`SendMessage`、`@name` 路由、花名册）——内核不该认识工具名，正如它不认识 `Bash`。

内核只多三件事，且单代理也需要：
1. **子会话**：`open(Create{parent: Some{session, item}, key, model, system_extra, toolset})`；父的 `ToolCall` item 带 `child_session`；`SubscribeOptions.children=true` 时根订阅者收到子会话事件（`session` 字段不同）。
2. **向任意会话提交**：`submit(any, Input{origin: Peer{from}})`；队列语义（空闲→开轮，忙→下个 barrier 吸收）**就是收件箱**——旧的 `InboxWake / drain_main_mail / flush_agent_inbox / MailWake` 坍缩成一件事。
3. **`Extension` 事件 + service**：插件自有资源（roster、任务、房间状态）。
另加：`SessionKey` 命名寻址（`agent/<team>/<name>`）、`Origin.principal` 记谁说的、`ResolvedBy` 记谁批的、**内核硬上限**（子会话深度、并发数——goose 与 codex 都在注册表层强制，Claude Code 默认深度 1 / 并发 20）、子会话默认隔离上下文、只经消息传递。wire 上每条子会话消息带 `parent`（= Claude Code 的 `parent_tool_use_id`），任何宿主都能重建任意深度的树；代理间消息作为 journal 里的 `User{origin: Peer}` item 持久化（codex 的 `InterAgentCommunication`）。

在此之上的插件：
- **`bingo-agents`（M8）**：借 codex 的五个协作动词而非一个万能 `Task`：`spawn_agent`、`send_message`（入队不触发轮）、`followup_task`（入队并触发）、`wait_agent`（阻塞到终态）、`list_agents`——这是唯一支持父代理「推一把正在跑的子代理」的设计，也是团队协作的基础；`.bingo/agents/*.md` 命名定义（frontmatter：description / `mode: primary|subagent|all` / model / provider / thinking / tools / permission，正文 = system prompt，取 opencode 的作者面）；完成通知作为 RoundStart contributor 注入父会话；`@name` 经 `Hook::on_submit` 重定向；roster 作 `Extension`。Guardian（高风险操作前的 reviewer 子代理）是后续插件，不进内核。
- **`bingo-teams`（M9）**：`.bingo/team.json` 常驻角色 = `start()` 时打开的子会话；`team-norms.md` = System contributor；团队记忆（project-hash + branch）= contributor + `on_turn(End)`；`/team`。
- **`bingo-rooms`（M9）**：推荐设计——**房间本身是一个没有模型的会话**（`SessionSpec{driver: None}`：只有日志与队列，提交直接落为 `User{origin: Peer}` item，不开轮）。房间日志就是它的 journal，成员订阅它，发言 = submit 到房间会话，房间插件扇出到成员队列。这样房间页面用与任何会话相同的 reducer 渲染，不需要新 widget（守一）。serial/free、`@` 追问是插件策略，从最简（free、无债务台账）起步，加功能各出一份 ADR。M9 开工前用 ADR 确认「无模型会话」这一内核扩展。
- **`bingo-tasks`（M9，小）**：Task×4 工具 + 提醒 contributor + `Extension` 面板。
- **`bingo-experience`（M14）**：5 工具 + 召回 contributor。
- TUI 侧插件资源的渲染：`bingo-tui` 提供 `View`（Text|Table|List）通用渲染与一个 `Widget` 扩展点；插件的专用 widget 放独立 crate（如 `bingo-teams-tui`）由 bin 组装——工具层永远看不到 TUI。

### 2.11 Crate 布局与依赖方向
```
bingo-improve/
  Cargo.toml                 workspace, resolver 3, edition 2024; [workspace.lints] unsafe_code=forbid, clippy::unwrap_used/expect_used=deny (tests 豁免)
  crates/
    bingo-sdk                稳定 API：ids/model/event/item/state(reducer)/traits/plugin/host/errors/schema; feature "testing"(ScriptedProvider, FakeHost, RecordingSurface)
                             deps: serde, serde_json, schemars, thiserror, async-trait, tokio(sync), tokio-util, futures-core —— 无 reqwest 无 ratatui
    bingo-core               内核：session actor, journal/hub, id mint, turn 状态机, gate, executor, accumulator, ContextUsage 尺子+熔断, ContextView::fold, config 分层, plugin host
    bingo-provider-fake      脚本化 provider（进程内 + feature "loopback" 起 Anthropic-wire SSE 回环服务器）；永远编译，是 demo provider
    bingo-provider-anthropic | bingo-provider-openai(Responses + Codex 变体) | bingo-auth-oauth
    bingo-tool-fs (Read/Glob/Grep/Edit/Write/AskUserQuestion) | bingo-tool-bash (+ "!" command + Background service) | bingo-tool-web
    bingo-permissions | bingo-hooks-shell | bingo-store-jsonl (journal + lock + blobs + rewind Checkpointer + GC) | bingo-context (compactor + memory)
    bingo-skills | bingo-mcp | bingo-agents | bingo-teams | bingo-rooms | bingo-tasks | bingo-experience(晚)
    bingo-surface-print | bingo-surface-rpc (JSON-RPC stdio/WS + schema gen + RemoteKernel) | bingo-surface-tui | bingo-acp(晚) | bingo-channels(+ loopback 测试频道；Telegram/Feishu 晚)
    bingo-plugin-rpc(晚)     通用跨进程插件桥
    bingo                    二进制：clap、组装 Vec<Box<dyn Plugin>>、选 surface、update 子命令
  schema/  docs/adr/  docs/plans/  docs/design/  scripts/
```
依赖方向：`bin → {core, plugins}`；`plugins → sdk`（跨插件只经 service trait；出现第三个消费者就把 trait 挪进小 crate `bingo-services`）；`core → sdk`。**禁止** `plugin → core`、`anything → tui`、`tui → 任何 provider/tool crate`。CI 用 `cargo metadata` 断言。

### 2.11b 架构图
设计稿页面：`/private/tmp/claude-501/-Users-yexrob-Episodes-Projects-bingo-inc-bingo-improve/0d880e40-0c39-4e6a-beb7-8c6fcbad26e2/scratchpad/bingo-architecture.html`（七张图：分层总览、事件中心、一轮时序、turn 状态机、会话=日志、crate 依赖、路线图；已用无头 Chrome 双主题渲染核对）。批准后随设计稿一并归档到 `docs/design/architecture.html` 并发布为可分享链接。

### 2.12 旧功能落位表
| 旧功能 | 去处 |
|---|---|
| NeutralRequest/StreamEvent/accumulator/错误码注册表 | sdk + core（思想移植） |
| AppCore actor、turn guard、交互注册表（400ms 守卫、只答一次）、队列 barrier/reclaim | core |
| query_loop / query_turn | core 状态机（功能分支全部拆成 contributor/hook） |
| 执行器 | core（+ 每工具 Interrupt） |
| can_use_tool 语义、5 模式、规则、Bash 拆分、敏感目录、会话级规则 | bingo-permissions（按 Subject 匹配） |
| 审批弹窗 | 语义在 core Interaction；渲染在 tui/GUI/IM |
| Hooks 10 事件 + exit-2 契约 | bingo-hooks-shell |
| Anthropic/OpenAI/Codex/OAuth/预设/目录/缓存/learned windows/vision | provider 与 auth 插件（learned windows 存储在插件、使用在 core 尺子） |
| Bash 全部子功能、`!`、live tail、后台 watch 注册表 | bingo-tool-bash（watch = service + Barrier contributor） |
| Read/Glob/Grep/Edit/Write/diff/AskUserQuestion | bingo-tool-fs（rewind 快照经 Checkpointer service） |
| WebFetch 预批准域名、WebSearch | bingo-tool-web（预批准 = 插件播种的策略 allow 规则） |
| Skills、MCP | 各自插件 |
| 转录/resume/rename/GC/Rewind/图片资产 | bingo-store-jsonl（记录 = 事件帧；compact/rewind 是事件） |
| 压缩（节奏/90%/保留 12/熔断/阶梯）| 尺子+熔断 core；策略 bingo-context |
| 记忆（memdir/CLAUDE.md/AGENTS.md/BM25）| bingo-context（顺手修 P0：最近优先截断、行+字节上限、git common root 键） |
| 子代理、SendMessage、AgentControl | bingo-agents |
| 团队/norms/团队记忆 | bingo-teams |
| 房间 | bingo-rooms（无模型会话） |
| 任务 | bingo-tasks |
| 经验库 | bingo-experience |
| TUI 全部 | bingo-surface-tui（选择器经 Question 交互；页面 = 按会话过滤；roster/room 经 View/Widget） |
| 头像 | bingo-teams-tui（晚） |
| `--print` | bingo-surface-print |
| app-server | bingo-surface-rpc（方法 = HostApi 1:1，通知 = Event） |
| ACP | bingo-acp（有损投影，`_meta.bingo.*`） |
| OpenClaw/Hermes 接入 | (i) `serve --stdio` 作 CLI backend；(ii) ACP；(iii) 发布 schema 让对方写 AgentHarness；(iv) 自托管 IM 频道晚 |
| update | bin |
| 配置 23 键三层 | core loader + 各插件 ConfigClaim |
| **share**、四代会话视图残骸、UiEvent/EngineEvent/AppEventPayload、第二份 PermissionMode/ThinkingLevel/ShellDialect | **删除** |

### 2.13 开源库选型与参考实现（2026-08-29 逐个在 crates.io / GitHub 核实）

原则：成熟库优先；Rust 没有的，找别的语言的成熟实现移植**类型与算法**而非代码；两个同类 Rust 项目（openai/codex、aaif-goose/goose）都手写的东西，我们也手写。版本对齐 `rmcp 3.1` 的要求：**reqwest 0.13 + schemars 1.x + oauth2 5（关默认特性）**（codex 还在 reqwest 0.12 / schemars 0.8，goose 已在新线上）。

| 子系统 | 选型 | 备注 |
|---|---|---|
| LLM provider | **手写两个 adapter**（Anthropic Messages、OpenAI Responses） | 无官方 Anthropic Rust SDK，社区四个全是单人停更；`genai` 0.7-beta 每版破坏；`rig-core` 是 RAG 框架层级不对；codex/goose/Claude Code 都手写。事件模型镜像 AI SDK V4 |
| 模型目录 | **models.dev**（MIT，TOML，204 provider / 7,439 模型，`limit.context/output`、`cost`、`reasoning`、`tool_call`）内嵌快照 + 可刷新；litellm 的价格表只作交叉核对 | 替代旧项目手写的家族前缀表；两者数据有分歧，以厂商文档为准 |
| HTTP / SSE / 重试 | `reqwest` 0.13（rustls 默认；`query`/`form` 现为可选特性）+ `sse-stream` 0.2（rmcp 已带入）+ `backon` 1.6 | `reqwest-eventsource` 钉 reqwest 0.12，弃 |
| JSON-RPC | 手写 ~400 行（serde_json + `tokio_util::codec::LinesCodec`）或复用 `agent-client-protocol` 内置机制；WS 用 `axum::extract::ws`（若已有 axum）否则 `tokio-tungstenite`，二选一避免双版本 | `jsonrpsee` 无 stdio、一年未发版 |
| ACP | **`agent-client-protocol` 2.0**（v1 稳定、v2 特性门控）+ `-tokio` + `-rmcp` | Zed 的做法值得抄：`AgentConnection` trait 有原生/外部两个实现，自家 UI 也走 ACP 形状——即我们的 `HostApi` / `RemoteKernel` |
| MCP | **`rmcp` 3.1** | 客户端 + 服务端都有；`agent-client-protocol-rmcp` 桥接 |
| OAuth / 密钥 | `oauth2` 5.0（`default-features=false`，自实现 AsyncHttpClient）+ 自写回环监听 + `keyring` 4.1 + `secrecy` | Anthropic Pro/Max 与 Codex 的 OAuth 流程**没有任何 Rust 实现**，移植 pi-ai `auth/oauth/*` |
| 配置 | 手写三层合并（~150 行，需要精确的 Claude-Code 优先级语义）+ `jsonc-parser` 0.33（注释保留、可回写）+ `schemars` 1.2 + `ts-rs` 12 | `figment` 两年未动，弃；`specta` 永久 RC |
| 日志存储 | JSONL 手写（`tokio::fs` + serde_json）+ **`rusqlite` 0.40 `bundled` + FTS5 作可删除索引** + `fs4` 锁 + `ulid` 3 / `uuid` v7 + `blake3` + `etcetera` | codex 的 JSONL 真相 + SQLite 索引；goose 只剩 SQLite 无法回放，opencode 双真相——都不学 |
| 记忆检索 | `bm25` 2.3.2（codex-core 正是这个版本）；落盘检索用 FTS5 | `tantivy` 过重 |
| shell 解析（权限拆分） | **`tree-sitter-bash` 0.25** 拆 `&& ; | $()` 与重定向 + `shlex` 2.0 分词，ERROR 节点一律失败关闭 | codex `codex-shell-command` 同法；`yash-syntax` GPL 弃；Claude-Code 规则语义无任何开源实现——权限引擎是我们真正原创的部分 |
| 进程 / 沙箱 | `tokio::process` + `process-wrap` 10（进程组、Windows job object、kill-on-drop）+ `portable-pty`；沙箱 Linux `landlock` 0.4.7 + `seccompiler`，macOS `sandbox-exec` SBPL（无 crate），后置 | `birdcage` 已归档且 GPL |
| diff / patch | `similar` 3.2（`Algorithm::Histogram`）+ `diffy` 0.5（unified 解析/应用）；Edit 工具只需精确串替换 | codex `apply-patch` V4A 格式（模糊上下文）可后续移植 |
| 文件搜索 | `ignore` + `globset` + `grep-searcher`/`grep-regex`（ripgrep 内核），不 shell 出 `rg` | 模糊选择用 `nucleo-matcher`（MPL-2.0，链接无碍，cargo-deny 标记） |
| WebFetch / 搜索 | `dom_smoothie` 0.18（Readability 移植）→ `htmd` 0.5（Turndown 移植）；Brave/Tavily/Exa 各 ~80 行手写在一个 `SearchProvider` trait 后 | `html2md` GPL 弃；无成熟搜索客户端 |
| Skills | frontmatter 自己切（15 行）+ **`serde-saphyr` 1.1**（纯 Rust、forbid unsafe、无标签实例化）；`$1/$ARGUMENTS` 手写替换，仅当需要条件/循环才引 `minijinja` | **`serde_yml` 不健全且已归档（RUSTSEC-2025-0068），`serde_yaml` 已弃**——两个参考项目仍在用后者 |
| tokenizer | `tiktoken-rs` 0.12（OpenAI 侧）；Anthropic 无离线 tokenizer → `count_tokens` API + 字符估算 + 每次响应 usage 校正 | |
| 插件注册 | 静态 crate 显式 `register()`；需要免声明自注册时用 `inventory`（codex 用）；`extism`（WASM）留作不可信插件层 | |
| 可观测 | `tracing` + `tracing-subscriber` + `tracing-appender`；OTel 0.32 / `tracing-opentelemetry` 0.33 整体钉版并放在 `otel` 特性后 | |
| CLI / 发布 / 质量 | `clap` 4.6；`cargo-dist`（现名 dist）；`self_update` 0.44；**`cargo-deny`**（GPL/MPL/不健全 crate 必须拦）；`cargo-nextest`；`cargo-machete` | |
| 测试 | `insta`（journal 事件与提示词快照）、`wiremock`（provider HTTP 假件含 SSE）、`assert_cmd`+`predicates`、`proptest`（权限匹配器与拆分器）、`rstest`、`tempfile` | |
| 异步 | `tokio-util` 0.7（CancellationToken、LinesCodec、TaskTracker）；`async-trait`（dyn 兼容）；`dynosaur` 到 0.5+ 再看 | |
| TUI | `ratatui` 0.30（`scrolling-regions`、`Viewport::Inline` + `insert_before` 原生支持 write-once scrollback）+ `crossterm` 0.29（kitty 键盘协议、bracketed paste）+ `ratatui-textarea` 0.9（多行、undo、emacs 键；kill ring/历史/reverse-i-search/粘贴折叠自写）+ `tui-input` + `tui-markdown` 0.3（`highlight-code`）+ `pulldown-cmark` + `syntect` 5 + `two-face` + `similar` + **`ratatui-image` 11（kitty Unicode placeholder + tmux 透传 + sixel/iTerm2/halfblocks 全有）** + `nucleo-matcher` + `tui-popup`/`tui-scrollview`/`tui-widget-list` + `throbber-widgets-tui` + `terminal-colorsaurus`（OSC 10/11 深浅色探测）+ `supports-color`；测试 `TestBackend` + `insta`，PTY 冒烟 `portable-pty` + `vt100`（不必依赖 tmux） | 无 diff 组件、无 OSC 通知 crate、无流式 markdown 提交逻辑——从 codex TUI 移植 `diff_render.rs`、`osc9.rs`、`markdown_stream.rs`、`terminal_probe.rs`、`pager_overlay.rs`（Apache-2.0，可 vendor；codex TUI 未发 crate） |
| IM | `teloxide` 0.17、`slack-morphism` 2.25、`serenity` 0.12 成熟；**Feishu 无可用 crate**（`openlark` 0.20 单人生成代码、6k 下载）→ 从官方 Go/Python OAPI SDK 移植租户 token、`im/v1/messages`、卡片回调、WS 长连接四件 | |
| 杂项 | `regex`、`unicode-width` 0.2、`unicode-segmentation`、**`jiff`**（自家逻辑；`chrono` 经 rmcp 传递接受）、`thiserror` 2 + `anyhow`（仅二进制边缘）、`miette`（仅配置/skill 解析错误的源码定位）、`semver`、`base64` 0.23 | journal 时间戳存 RFC-3339 字符串，不绑 crate |

**参考实现要借的设计**（已核实到 2026-08-28 HEAD）：
- **codex**（119k★，Rust，无 crate 发布，只能 vendor）：`Op`/`EventMsg` 提交队列⇄事件队列；`thread/turn/item` v2 JSON-RPC + schemars/ts-rs 双 schema；JSONL rollout + SQLite 索引；`ExecApprovalRequirement{Forbidden, NeedsApproval{proposed_amendment}, Skip}` 决策格；`.codex-plugin/plugin.json` + marketplace；五个协作动词；`execpolicy` 的 Starlark 规则自带 `match/not_match` 自测（规则语言不抄——权限路径里放 Starlark 攻击面太大，但「规则自带测试」这个点要）。**不学**：80+ 个 `codex-utils-*` crate、v1/v2 协议与 `multi_agents`/`multi_agents_v2` 并存。
- **goose**（53.6k★，Rust）：`ToolInspector` 链（permission / security / egress / adversary / repetition → Allow|Deny|RequireApproval）——映射到我们的 `Hook::before_tool` 链 + 唯一 `PermissionPolicy`；ACP 既作服务端也作客户端；recipes / sub-recipes。**不学**：`goose-agent` 0.1.0-alpha.7（最新版 15 次下载、3% 文档、钉死 `agent-client-protocol-schema =1.5.0`）——是陷阱；SQLite 独存会话。
- **pi**（原 pi-mono，98.7k★，TS；OpenClaw 的 agent runtime 建于其上）：pi-ai 12 个流事件与 provider 怪癖表（`compat` + `thinkingLevelMap`）；steering / follow-up 两个队列；统一的 `ExtensionAPI`（工具、命令、快捷键、旗标、UI、hook、会话状态一个对象注册）——**最接近我们「一切皆插件」的参考**；~35 个 hook 点含 `before_provider_request` 与 `agent_settled`（重试与压缩都完成后才触发，IM 网关判断「可以发了」的正确信号）；`CompactionEntry{summary, firstKeptEntryId, tokensBefore, retainedTail}` 压缩即日志条目。
- **opencode**（202k★，TS）：SSE 总线是 UI 唯一契约；12 种 `Part`（含 `SnapshotPart`、`PatchPart`、`RetryPart`、`CompactionPart`）；agent 即 markdown frontmatter（`mode: primary|subagent|all`）；`permission.ask` hook 返回 allow|deny|ask。**不学**：SQLite + JSON 文件双真相。
- **Claude Agent SDK**：~30 个 hook 事件与 `hookSpecificOutput.permissionDecision` 契约；`plugin.json` 组件路径 + `userConfig` 类型化选项 + `${PLUGIN_ROOT}`；`stream-json` 信封（见 §2.7）。
- **OpenClaw**：`CliBackendConfig` 是我们要满足的契约（`sessionArgs`、`jsonlDialect`、`bundleMcp`、`ownsNativeCompaction`）；插件 `activation{onStartup, onCommands, onChannels, …}` 惰性激活（插件上百个时才需要，manifest 预留字段）。
- **crush**（Go）：LSP 诊断作为上下文——编辑后 `waitForLSPDiagnostics` 阻塞到诊断发布，模型同一轮看到编译器意见（后续 `bingo-lsp` 插件）。**aider**（已停更）：`repomap.py` 的 tree-sitter 标签 → PageRank → 二分找能塞进 token 预算的定义数（后续 contributor 插件）。

**移植清单（按价值排序）**：① AI SDK V4 类型代数（1 周）；② pi-ai 的 Anthropic Pro/Max 与 Codex OAuth 流程 + 模型目录（1 周，**最高价值**——无任何 Rust 实现）；③ codex `shell-command` 的 tree-sitter 拆分（安全关键）与 `apply-patch` V4A；④ codex TUI 的五个模块；⑤ Mozilla Readability 评分启发式（`dom_smoothie` 单人维护，必要时 vendor）；⑥ litellm 的错误码→规范错误映射表（只取数据）；⑦ larksuite-oapi-sdk-go 的鉴权与 WS 事件循环。

**Rust 生态空白（我们必须自己写的）**：Anthropic SDK；agent 类 OAuth 流程；JSON-RPC over stdio；健全的 YAML（`serde-saphyr` 刚过 1.1）；macOS/Windows 沙箱；Brave/Tavily/Exa 客户端；**Claude-Code 式权限规则引擎（任何语言都没有开源实现）**；Anthropic 离线 tokenizer；frontmatter 解析（`gray_matter` 停更）。

---

## 三、里程碑（每个都能从终端用；每个以验证收尾）

| M | 目标 | 内容 | 砖 | 退出标准 | 量级 |
|---|---|---|---|---|---|
| **M0** 行走骨架 | `bingo --print --provider fake "hi"` 经真实 loop 流式回复并跑一轮工具；`--json` 输出规范事件流 | sdk（全部类型 + trait；`ModelEvent` 镜像 AI SDK V4）、core（accumulator、executor、纯状态机 `step(input)->effects`、actor、内存日志、hub、子会话深度/并发上限）、provider-fake、tool-fs 仅 Read、surface-print、bin、CI、`cargo-deny` 配置（拦 GPL/MPL/不健全 crate） | 纯状态机 + 单一 `Event` | loop 测试（纯文本轮/工具轮/流中中断保文本丢 tool_use 补孤儿结果/流内重试重开/空回复重试/最大轮数）；每个 Event/ContentPart 变体 JSON 往返 fixture；黑盒 `--print` stdout 只有正文、`--json` 一行一帧、非 TTY `[error]` 契约 | ~2.5K |
| **M1** 真 provider + 真工具 + 权限门 | 对 Anthropic 做 Claude-Code 形状的编码轮，Read/Glob/Grep/Edit/Write/Bash/AskUserQuestion，5 模式 + 规则表 | provider-anthropic（SSE、重试阶梯、context-limit 重算、thinking、cache_control）、tool-fs 全、tool-bash、permissions、settings 三层+tri-state、system prompt、交互砖（`--print` 下 stderr/stdin 应答） | 类型化 `Decision`，唯一解决 `Ask` 的地方在门 | 权限用例表逐行移植；拆分属性测试；bash 拒绝表；Edit 唯一/Write 覆盖；Anthropic 录制 SSE fixture；loopback fake 让真 adapter 被黑盒测；一次人工 live smoke 记入 plan | ~5K |
| **M2** 第二个 provider 证明 trait；目录；web 工具 | `--provider openai` Responses 可用（API key）；窗口按 声明 > 家族文件 > 前缀表 > 默认 > learned clamp 解析 | provider-openai（Codex 请求体隔离留在 enum 里，M10 只加 OAuth）、目录 + 24h 缓存 + learned windows、vision 门控、tool-web | `ModelResolver` 纯函数 | 两变体请求体断言；预算层级属性测试；sdk 要么不变要么一次变更并附 ADR 列出触及的插件 | ~2K |
| **M3** 会话：落盘日志、resume、类型化中断、轮预算 | `--continue/--resume` 精确回放；Esc/Ctrl-C 语义类型化且每工具可定；失控循环会停 | store-jsonl（journal + lock + blobs + GC + **rusqlite 可删除索引**）、`ContextView::fold`、`TurnBudget`、`Interrupt::{Cancel,Block}` 由执行器执行、会话自有 cwd、图片资产、`--session-id` 任意字符串键 | `fold` 纯函数 + 「任何投影都 API 合法」不变量测试（tool_use/tool_result 成对、无空 assistant、无未签名 thinking） | resume fixtures（撕裂行/marker/中断标记）；Windows 安全锁测试；`--max-turns` 以命名事件退出；Bash 中途中断返回真实结果而 Read 被取消；journal 版本 golden 测试 | ~2K |
| **M4** 上下文：压缩阶梯、microcompact、可观测、记忆 | 长会话永不 400、不为无用摘要付费；记忆文件正确 | 估算 + TokenGate、90% 自动压缩、溢出阶梯、事件化落盘、请求时 microcompact 投影、`Item::Compaction` + 快速回填熔断、bingo-context 记忆（最近优先、行+字节上限、不静默丢、git common root 键） | 每一级阶梯是 `&[Item]` 上的纯函数 | 阶梯测试（fake `Overflow`）；「不缩小的摘要被丢弃并计费」；microcompact 保 id 与最近 N 对；记忆 300 行文件加新事实淘汰最旧；worktree A/B 共享记忆 | ~2K |
| **M5** RPC 界面（GUI 契约，事件中心上线） | `bingo serve --stdio/--ws` 用 JSON-RPC 服务会话，schema 提交并防漂移；`--print --output-format stream-json` 兼容 Claude 信封（OpenClaw `claude-cli` 后端零插件接入） | surface-rpc：initialize/能力、13 方法、快照 + 无缝 seq + resync、每客户端有界通道 + `Lagged`、`RemoteKernel`（先评估复用 `agent-client-protocol` 的 JSON-RPC 机制）；schema 生成；surface-print 的 stream-json 兼容编码器（`parent_tool_use_id`、`session_id`、`result`）；黑盒 harness（spawn 二进制 + loopback fake） | `Event` 就是 wire，界面只加信封与 id；零私有镜像类型（CI grep 界面 crate 无 `enum .*Event`） | 15–20 个黑盒场景（握手拒绝、stdout 纯净、中断到达运行中的轮、经交互批准、重试可见、带 marker resume、两客户端同会话）；schema 漂移测试；camelCase 属性测试 | ~3K |
| **M6** TUI MVP（一个客户端） | 日常可用：全屏、带历史的 composer、流式转录 + markdown/高亮基础、工具行、审批弹窗（Yes/本会话/No+反馈、Ctrl-E diff）、Esc 有序栈、Ctrl-C 两次、`/model /provider /think /permission /compact /resume /clear /help`、`?` 键表、上下文条、标题/铃 | surface-tui 消费与 RPC 相同的进程内 Frame 通道；`on_key/draw` 纯函数 + `LocalUi` 乐观状态；TestBackend Recorder；tmux smoke | 渲染 = `(state, ui, viewport) -> rows` 纯函数；事件循环永不 await 内核 | 每个对话框与权限流的 TestBackend 测试；tmux smoke macOS+Linux 绿；tui crate 不依赖任何 provider/tool crate（CI 断言） | ~4K |
| **M7** hooks / skills / MCP | 三个低风险插件，在可用 TUI 上 dogfood | hooks-shell（10 旧事件 + PermissionRequest/PostToolUseFailure/CwdChanged，`BINGO_ENV_FILE`，hook `ask` 走 M1 的门）、skills（SKILL.md、两层、缓存、内置 guide）、mcp（rmcp stdio+HTTP、启动并发拨号 5s、stderr 落日志、readOnlyHint 不信任、`/mcp`） | — | hook `ask`→弹窗→允许（P0 回归）；64KB stdin 不死锁；MCP 工具折叠到服务器键；`cargo tree -p bingo-core` 无 rmcp | ~2.5K |
| **M8** 子代理（first-class 之一） | 五个协作动词（spawn / send_message / followup_task / wait_agent / list_agents）生成并驱动子会话；命名定义；完成通知；TUI 切换到子代理视图与主视图**同一份渲染代码** | bingo-agents；`SubscribeOptions.children`；TUI 会话切换器（读会话树）；wire 上 `parent` 标记 | 子会话 = 会话；收件箱 = 队列 | 父子事件流断言；异步完成注入到父的下一轮；`background:false` 阻塞返回；权限提示从子会话扇出到根界面；ACP 风格扁平化 `_meta` | ~2K |
| **M9** 协作（first-class 之二）：团队 + 房间 + 任务 | `.bingo/team.json` 常驻角色自动就位；房间群聊；任务面板 | ADR「无模型会话」；bingo-teams（角色 = 子会话、norms contributor、团队记忆、`/team`）；bingo-rooms（房间 = 无模型会话、扇出、free 模式）；bingo-tasks；tui `View`/`Widget` 扩展点 + roster | 房间用同一 reducer 渲染 | 团队启动零 token 空闲；房间发言到达每个成员队列且忙时在 barrier 吸收；任务 hook 事件；内核无 room/team/hire/ack 名词（CI grep） | ~4K |
| **M10** OAuth + Codex | `/provider login codex` 等；订阅制零配置 | bingo-auth-oauth（PKCE 1455/device/paste、`auth.json` 0600 原子、提前 300s 单飞刷新、永久失败类）、Codex 动态模型表 | — | 对 loopback 假 issuer 的流程测试；`codex_request_params_isolation` 移植 | ~1.5K |
| **M11** TUI 深度 | inline 模式、kitty 图片（探测 + tmux 透传）、Ctrl-O 分页器、主题、OSC 通知、Rewind UI（Esc Esc）、后台对话框、块级虚拟化（先做 1k/5k 基准）、`@` 补全 | 多个 plan 文件 | — | 每项 TestBackend + tmux | ~6–8K |
| **M12** ACP | `bingo-acp`（`agent-client-protocol` 2.0 + `-tokio`；v1 稳定，v2 特性门控） | 映射表见 §2.7 与设计稿；子代理扁平为 tool call + `_meta.bingo.child_session` | — | Zed/Hermes/OpenClaw acpx 任一真实宿主跑通 | ~1.5K |
| **M13** IM 频道 | `bingo-channels` 宿主 + `Deliverer` 纯 reducer（帧 → 编辑/分块/typing/按钮）+ 一个真频道（**Feishu 优先**，其次 Telegram）；审批经按钮/关键词回答 | `ChannelPlugin{id, caps, run, send}`（channels 插件内部 trait）；会话键 `feishu/...` | Deliverer 用与 TUI 相同的帧 fixture 测 | 群聊 `origin.principal` 记谁说；两界面同时批准只有一个赢 | ~2.5K |
| **M14** 经验库 + 通用跨进程插件桥 | bingo-experience；`bingo-plugin-rpc`（plugin.json 发现、SDK 类型 JSON 镜像） | — | — | 第三方用非 Rust 写一个 Tool 与一个 Command 跑通 | ~3K |
| 永不 | `share` | | | | |

顺序理由：RPC（M5）在 TUI（M6）之前——「GUI-ready」只有在 TUI 之前就存在一个非 TUI 客户端时才是真的；旧项目正是 TUI 先对着内部枚举写，补协议时撞了四堵墙。第二个 provider 紧跟第一个（M2）——trait 只有在两种相反形状的 wire 后面才算真的。子代理与协作紧跟 hooks/MCP（M8/M9），早于 OAuth 与 TUI 深度，因为它们是 first-class。

---

## 四、验证策略

- **按 crate 分层的单元测试**：sdk 每个 serde 类型 fixture 往返（wire 类型 `deny_unknown_fields`）；core 状态机测试纯粹（`step` 进、effects 出、无 runtime），actor 测试用 provider-fake 进程内；插件 crate 只做单元测试（少链接）；bin 恰好两个集成测试二进制 `tests/cli.rs`、`tests/rpc.rs`。
- **`bingo-provider-fake`**：`Step = Text | Events(..) | ToolCall{..} | Error{kind, retry_after} | Overflow{body} | Hang | CountTokens`；记录每个 `ModelRequest`；像 API 一样校验请求形状（拒绝孤儿 tool_result、空 assistant）；feature `loopback` 起 Anthropic-wire SSE 回环服务器；`BINGO_FAKE_SCRIPT` 从 JSON 脚本化，供 CLI/RPC/tmux 测试。
- **契约测试**：`schema/` 从 schemars 生成并提交，漂移测试打印再生成命令；camelCase 与 ref 解析检查；错误码枚举无通配 arm + 漂移测试。
- **黑盒 CLI/RPC**：临时 HOME + XDG，清除密钥，`BINGO_PROVIDER=fake`；断言退出码、stdout 纯净、stderr `[error]`、NDJSON 合法、跨两次调用 resume。
- **TUI**：ratatui `TestBackend` 上的 Recorder（屏幕行、scrollback、原始字节 sink 抓 OSC/kitty、draw 计数、`assert_row_styled`）；合成 `KeyEvent` + 注入 `Frame`；时间用 `now: Instant` 参数，永不 sleep。`scripts/tui-smoke.sh`：`tmux -L bingo -x 120 -y 40` + fake provider，`send-keys`，轮询 `capture-pane`，断言回复、弹窗、Esc 中断、Ctrl-C 退出、标题恢复。
- **CI**（`.github/workflows/ci.yml`）：`fmt`（rustfmt + `scripts/check_discipline.sh`）；`test` on ubuntu+macos（`cargo check --workspace --all-targets --locked`、`clippy -D warnings`、`cargo test --workspace --locked`）；windows `continue-on-error` 自 M1，M6 起必须；`budget`；`tui-smoke` 自 M6。
- **构建预算**（`scripts/budget.sh` + `budget.toml`，M0/M1 记基线，硬限失败）：依赖数 ≤ 260；每 crate 禁依赖表（core 无 reqwest/ratatui/crossterm/rmcp/image；插件依赖只许 sdk + 外部 crate）；`cargo build --timings`；冷 `cargo test --workspace` ≤ 2× 基线，热 `cargo check -p bingo-core` ≤ 20s；重链测试：`touch tui/src/lib.rs && cargo check -p bingo-core` 必须 no-op；`target/debug` 软限 5 GB。profile：`debug="line-tables-only"`, `split-debuginfo="unpacked"`, `[profile.dev.package."*"] debug=false`。
- **量对东西的文件纪律**（`scripts/check_discipline.sh`）：每类型 inherent impl 所在文件数 ≤ 2 且总行 ≤ 1200（旧 `Chat` 9 个文件会被抓）；struct 字段 ≤ 16；每文件非测试行 warn 700 / fail 1000；core 内模块扇出 `use crate::` ≤ 12。豁免只经 `// discipline: allow(<rule>) ADR-NNNN`。
- **里程碑闸门**：plan 文件退出标准逐项打勾并贴命令输出（或 CI 链接），含需要的人工 live smoke；失败原样贴。

---

## 五、仓库约定

- **`AGENTS.md`**（≤ 90 行）：产品一句话与地图（`ARCHITECTURE.md`、`docs/adr/`、`docs/plans/`）；语言风格（edition 2024、thiserror、无 unwrap/expect 由 lint 执行、无 unsafe、注释只说 why、模型面/UI/文档/测试英文、新依赖需 ADR 一行 + budget 跑）；架构规则（分层 `sdk ← core ← plugins ← bin`；**内核永不 import 插件**；**插件不依赖插件，除非经 sdk/service trait**；**一条事件流**——`bingo_sdk::Event` 是唯一事件类型，界面是客户端，渲染时派生，无私有镜像枚举；**一个事实一种表示**；**契约先行**（trait/wire/持久格式先 fixture/schema 测试）；**先砖后墙**；**默认做减法**；工具属性失败关闭）；验证闸门；ADR/plan/commit 约定；禁止项（unsafe；unwrap/expect；界面持有会话状态；插件 import 插件；已有事实的第二种表示）。
- **ADR**（`docs/adr/NNNN-slug.md`）：只为边界决策写一份（trait 形状、wire、持久格式、依赖、crate 拆分、阈值族）；模板 Context ≤10 行 / Decision ≤15 / Consequences ≤10 / Supersedes；硬上限 120 行，更长的进 `docs/design/` 由短 ADR 链接；bug 修复写在 commit body。旧项目 856 KB 的 research.md 是反面教材：189 条里约 25 条是边界决策。
- **Plan 文件**（`docs/plans/M<n>-slug.md`，≤150 行，动手前写）：Goal / 砖（按构建序）/ 触及 crate 与文件 / 退出标准（带精确命令的勾选框）/ 非目标 / 触及的风险；结束时追加 Verified 与输出。
- **Commit**：Conventional Commits，祈使句，主题 ≤ 60 字符，英文；scope = crate 短名（`sdk core provider-anthropic tool-bash print rpc tui hooks skills mcp context store agents teams rooms tasks acp channels bin ci docs adr`）；body 只在有信息时写；footer `Refs: ADR-0007` / `Plan: M3`；**不用文学化标题**——`git log --grep` 与 `git log -- crates/bingo-surface-tui` 必须能找到东西。
- **设计稿存档**：本计划批准后第一件事，把三份设计提案（内核与 SDK / 网关与界面 / 交付与验证）、四份选型调研（Rust crate 版图 / TUI 生态 / 参考实现 / provider 事件模型）与架构图整理进 `docs/design/`，作为后续 ADR 的来源。
- **子代理**：调研/探索/核实类子代理一律 `model: "opus"`；Fable 只用于架构设计与 review（用户 2026-08-28 规定）。

---

## 六、风险与待决

| # | 风险 | 缓解 | 早期信号 |
|---|---|---|---|
| 1 | 三个实现出现前 sdk trait 抖动 | M2 前有 fake+Anthropic+OpenAI；M6 前有 print+rpc+tui；M7 前有 Read+Bash+MCP 工具。M2 后改 sdk 需 ADR 列出每个触及插件 | 一个 sdk PR 每里程碑不止一次触及 >2 个插件 crate |
| 2 | TUI 异步客户端模型（顺序/背压/延迟） | 与 RPC 同一有界 Frame 通道 + 快照 resync；渲染纯函数；写操作不返回回执；TestBackend 注入帧 | TUI 代码持有会话状态、调 provider、与 core 共享 Mutex |
| 3 | 编译时间/target 膨胀回潮 | budget 硬限；每 crate 禁依赖；profile；≤2 个集成测试二进制 | `cargo check -p bingo-core` 热 >20s；target >5 GB |
| 4 | provider 怪癖回归（Codex include/store、两种推理 delta 名、retry-after 单位、context-limit 重算、cache 用量求和） | 旧 wire 测试作为请求体/SSE fixture 移植；live 验证矩阵记入 ADR | provider crate 改动不带 fixture 改动 |
| 5 | 协作层再次先于内核正确性 | 协作在 M8/M9 才开始，且只用三个原语；内核禁出现 room/team/hire/ack 名词（CI grep）；每个协作功能一份带用户故事的 ADR | `bingo-agents` >2K 行；名词漏进 sdk/core |
| 6 | 权限门失败开放回归 | 用例表逐行移植；拆分器属性测试；不变量（deny 压倒一切、ask 在 bypass 下仍问、敏感目录任何模式都问）单独成测 | 改门序不带用例表 diff |
| 7 | resume/压缩产出 API 非法历史 | `fold` 合法性属性测试；fake provider 拒绝非法请求；每次 dogfood 的 400 body 变 fixture | dogfood 出 400；压缩改动不带阶梯测试 |
| 8 | `ContextView::fold` 跨版本漂移 | journal 头带 version；每版本 golden；`ContentPart::Opaque` 保 provider 私有数据 | 老会话 resume 上下文不同 |
| 9 | TUI 进程 + 进程锁下「这里 TUI、那里 GUI」 | 每个 TUI 进程在回环临时端口起 WS 服务并把 `{pid, ws, token}` 写进 `.lock`；第二个客户端读锁附着；v1 无守护进程 | — |
| 10 | 会话键归属冲突（自家 `feishu/…` 与宿主 `host/…` 指同一聊天） | 内核强制首段 = 铸造插件 id；文档规定路由 IM 的宿主不得同时跑 bingo 的同 provider 频道插件 | — |

待决（在对应里程碑的 ADR 里定）：`async_trait` vs AFIT（先 async_trait，sdk 0.x 期间可换）；`bingo-rooms` 的「无模型会话」内核扩展（M9 前）；子会话在 store 里按父目录还是平铺（M3 前定，M8 用）；ACP v1 `session/prompt` 保持打开 vs 我们的队列优先（v1 接受，v2 稳定后映射）。

---

## 七、批准后第一个会话（以骨架 `cargo test` 全绿收尾）

1. 把三份设计提案整理进 `docs/design/`；写 `docs/adr/0001-crate-map.md`（名称、分层、依赖规则）与 `docs/adr/0002-event-stream.md`（单一 Event、durable/ephemeral、ids、reducer）。`docs: add crate map and event stream ADRs`
2. 脚手架：根 `Cargo.toml`（`[workspace] members=["crates/*"] resolver="3"`；`[workspace.package] edition="2024" rust-version="1.96" license="MIT"`；`[workspace.dependencies]` 只钉 tokio/serde/serde_json/schemars/thiserror/async-trait/tokio-util/futures-util/clap/ulid；`[workspace.lints]`；dev profile）；`rust-toolchain.toml`、`rustfmt.toml`、`clippy.toml`（`allow-unwrap-in-tests=true`）、`.gitignore`；建 `crates/{bingo-sdk,bingo-core,bingo-provider-fake,bingo-tool-fs,bingo-surface-print,bingo}`，各 `lints.workspace=true`。`chore: init workspace`
3. `AGENTS.md`、`ARCHITECTURE.md`（40 行 crate 图）、`docs/plans/M0-walking-skeleton.md`。`docs: add AGENTS.md, architecture map and M0 plan`
4. `scripts/check_discipline.sh` v1、`scripts/budget.sh` + `budget.toml`、`.github/workflows/ci.yml`。`ci: add fmt/check/clippy/test matrix, discipline and budget gates`
5. 砖，每块带测试、单独提交：`feat(sdk): add ids, message and content contracts`（fixtures）→ `feat(sdk): add event, item and session state reducer` → `feat(sdk): add plugin, tool, provider and permission traits` → `feat(provider-fake): add scripted provider` → `feat(core): add assistant accumulator` → `feat(core): add tool executor with typed interrupt` → `feat(core): add turn state machine` → `feat(core): add session actor, journal and hub` → `feat(core): add plugin host and registry` → `feat(tool-fs): add Read tool` → `feat(print): add --print and --json surface` → `feat(bin): compose walking skeleton`
6. 验证并贴进 M0 plan：`cargo fmt --all -- --check`、`cargo check --workspace --all-targets --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked`、`scripts/check_discipline.sh`、`scripts/budget.sh`（记基线）、`bingo --print --provider fake "hello"`、`bingo --print --json --provider fake "read Cargo.toml"`。`docs(plans): mark M0 verified`

---

## 附：移植知识清单（行为，非代码；路径在旧仓库 `bingo/` 下）

**M0**：流事件 `input_tokens` 在 MessageStart 与 StopReason 都出现、accumulator 两处都折叠 `src/api/contract.rs:343-396`；`finish()` 规则（未闭合文本保 max_tokens、未闭合 tool_use 丢并标截断、乱序块报错）`:605-640`；Tool trait 失败关闭 + schema 去 `$schema` `src/tool/mod.rs:107-188`；执行器连续安全并行 ≤10、`FuturesUnordered`、取消保留已完成、批间确认已置位信号 `src/tool/executor.rs:43-137`；中断标记原文（`[Request interrupted by user]` / `…for tool use`）、保文本+签名思考、孤儿 tool_result 补 `is_error` 占位 `src/query.rs:766-859`；流内重试重开整个回复、10 次、retry_after ≤60s、重试策略注入而非 `cfg!(test)` `src/query_turn.rs:17-63`；空回复守卫 `src/query.rs:44-47`；print 契约 `src/print.rs:16-18,221-226`、`src/main.rs:106-108,159-165,226-233`；错误码穷尽 match 无 `_` + 漂移测试 `src/error.rs:14-80,165-180,247-360`；**P0 类型化中断** `cc-gap-analysis.md:48,110`。

**M1**：Anthropic 头 `providers/anthropic.rs:432-436`；thinking 只用 `{"type":"adaptive"}`，Claude 5 上 budget_tokens 400 `:189-204,889-910`；≤4 个 cache_control 块默认关 `:141,165-183`；HTTP 重试 5×、retry-after 头 `:455-475`；400 `A + B > C` 重算 max_tokens 一次 `:481-497`；每 chunk 60s 空闲守卫 `:36,519`；`complete_text` 底下走流 `:577`；Anthropic 用量求和 cache 读写、OpenAI 不 `:226` vs `openai.rs:625-626`；退避 500ms·2^(n−1)·[0.9,1.1] 上限 32s `providers/mod.rs:255-290`；broken pipe 当 5xx 重试 D182；SSE 边界/尾巴/8MB `src/api/sse.rs:3-100`；错误分类（溢出短语表、"512 characters" 不得读作 5xx）`contract.rs:47-103,259-339`；Read 20K/字节界/图片块/行范围 `src/tool/read.rs:10-13,78-125`；Edit 唯一或拒绝、replace_all、写前快照 `edit.rs:21-27,78-102`；Write 拒绝覆盖不可读文件 `write.rs:60-73`；diff 用 `similar`（D176）；rewind 上限 50MB/200/8MB `src/rewind.rs:1-40,61-110`；**Bash**：常量 `bash.rs:11-18`、交互拒绝表（wrapper 展开、sudo 旗标、监视器 `-b`、编辑器、文件管理器、TUI、gdb、REPL、DB 客户端、ssh、docker attach）`:25-331`、周期检测 `:335-357`、描述报真实 shell 方言 `:420-439`、调用序 reject→periodic→background→foreground `:454-505`、`$ cmd\n…\n[Exited with code N]`、前台 kill_on_drop/进程树/stdin null/2s 排干/Ctrl-B 提升同进程 `:535-654`、默认通知条件 Errors `:693-705`、live tail 5 行/100ms `src/live.rs:24-31`；平台 shell 方言/`kill_process_tree`/`open_tty` `src/platform.rs:56-93,125,161`；**权限（规范）**：拆分器 `permission.rs:71-118`、deny/ask 任一 vs allow 全部+trusted `:123-135`、路径规范化不查 fs `:140-175`、`Skill(name:*)`/`prefix:`/`:*` `:181-242`、`mcp__server` 前缀 `:246-270`、敏感目录 + confirm 免 bypass `:286-322`、七步序 `:325-408`、会话级规则只在能消音时、复合命令无 `:418-440,567-612`；门：hook→规则→带 scope+diff 弹窗、反馈附到拒绝 `src/query.rs:490-560`；设置分层/合并规则/scoped 写 `src/settings.rs:361-600`，tri-state 缺口 `cc-gap-analysis.md:67`；system prompt 指令文件/env 块/能力块 `src/system.rs:112-160,254-261`；AskUserQuestion 形状 `src/tool/ask.rs:9-75`；黑盒 harness `tests/cli_black_box.rs:1-80`。

**M2**：OpenAI 变体与路径 `openai.rs:62-131`；Codex 隔离（`store:false`、无 max_output_tokens、只流、`include` 差异）`:239-284,985-1004`；system→单 `instructions` `:248-251`；effort 档 `:45-48`；两种推理 delta 事件名 `:557-565`；retry_after ms/s `:400-436`；`ChatGPT-Account-Id` 取自 JWT `:166-174`；模型表容忍 `data[]`/`models[]` `:372`；目录三层与 learned clamp `src/api/models.rs:497-534`、`model_families.rs:27-31,95-150`、`model_cache.rs:20-46`、`learned.rs:16-20,101-140`；vision 门控 `client.rs:422-435`、`types.rs:117-141`；预算层级 `src/budget.rs:12-55`；WebFetch 限制/缓存/https 升级 `webfetch.rs:10-23,186-230`；预批准域名 `preapproved.rs:5-83`；WebSearch `websearch.rs:10-12,123-166`。

**M3**：sidecar 锁是全部声明、数据文件不锁、进程内按路径锁表、rename 带锁 `src/transcript.rs:28-56,149-232` D72；记录类型与 marker 追加、经最新 marker 投影、撕裂行只在入口报 `:239-317,503-610`；GC 30 天/100/24h、不删动过的文件 `src/storage.rs:10-13,178-340`；数据目录 `:62-124`；turn-open marker 即 rewind 检查点、先 marker 后消息 `src/query.rs:867-887`；退出提示回来的路 D184 `src/main.rs:625-630`；图片资产 sha256/`#[image N]`/尺寸上限 `src/api/image.rs:11-42,110-167`；新增 `TurnBudget` `cc-gap-analysis.md:53,90`。

**M4**：常量与提示词 `src/compact.rs:16-75`；分割点 = max(12 条, token 上限) 且越过 tool_result 边界 `:81-119`；阶梯目标 `min(effective, gate)·¾`、熔断只跳摘要级 `:257-307`；不缩小的摘要丢弃并计费 D172 `:412-430`；幂等中段省略 `:496-539`；丢最旧不会失败并折叠已有摘要 `:544-590`；CJK 1 token/字、图片 1600、工具 schema 计入 `:605-668`；TokenGate 每 5 轮或 +20K `:675-724`；熔断 3 `src/budget.rs:52`；决策史 D169–D172；**P0 压缩可观测 + 快速回填熔断；P1 microcompact** `cc-gap-analysis.md:50-52,112,120`；**P0 记忆**：截前不截尾 bug `src/memory.rs:80-86`、200 行后静默丢 `:157`、无字节上限、cwd-hash 分裂 worktree `:30-57`。

**M5**：`notes/design/gui-app-server.md` §Resource model(230) §One submission path(451) §Server-initiated interactions(617) §Snapshots and recovery(662) §Lifecycle and ordering(703) §Errors/load/security(787)；item 生命周期 `notes/research/gui-event-protocols.md:25-33`；有界通道 1024 + `CLIENT_TOO_SLOW`、附着是视图非拥有者 `src/app/mod.rs:58-70`；schema 生成/漂移/camelCase/ref `src/app_server/schema.rs:1-30,392-580`；黑盒 harness + 回环 provider `tests/app_server_black_box.rs:1-70,865-1040`；协议错误码 `src/error.rs:41-77`；旧 Interaction 形状 `src/app/snapshot.rs:730-830`。

**M6**：kitty 键盘协议 push/pop、panic 路径可 pop `src/tui/term.rs:32-78`；带外写铃/OSC/标题 `:135-160`；终端在任何退出（含 panic）交还 D77；帧是量出来的不是算出来的、settled 行只写一次、resize 静默 120ms `src/tui/app.rs:1-31,65`；单一键表 `keys.rs:9-17`；单一命令表 `app/action.rs:348,885`；Esc 一个有序栈 D80；审批弹窗 `src/tui/ask.rs`；动词表 `activities.rs`；测试基建 Recorder/样式断言 `test_util.rs:203-320`、计时分层 `notes/design/tui-test-infra.md:6-12,60-72`。

**M7**：hooks 60s/1.5s、stdin 写并发共享超时（64KB 死锁）`src/hooks.rs:10-13,110-170,651-670`；exit 2 阻断、`updatedInput` 累加、hook ask `:178-258`；**P0 hook-ask `unreachable!`** `src/query.rs:500-504,1303,1819`；新事件与 `BINGO_ENV_FILE` `cc-gap-analysis.md:64-65,131`；skills 层序/参数/mtime 缓存 `src/skills.rs:4-14,117-266`；MCP 5s 连接、启动拨号 D165、并发握手不持锁 D167、stderr 落日志、按服务器折叠 D166 `src/mcp.rs:41-61,185-211,299-350`。

**M8/M9**：定义 frontmatter 与优先级 `src/agents.rs:20-50`；通知作为记录的 user 消息注入 `src/query.rs:969-985`；唤醒规则 D129；房间语义（serial/free、读游标）`src/channels.rs:59-99,275-306` 只作参考；**不移植**：hires D53、`@` 债务 D131、ack 追踪 D44、头像 D110、递归组织树。

**M10**：Codex client id/issuer、刷新提前 300s、device 等待 15min、回环端口 1455、永久失败类 `src/api/auth.rs:11-90,152-162,197-320`；`auth.json` 0600 原子、非重入 `src/auth.rs:1-25,96-141,221-232`；设计 `notes/design/provider-oauth.md`。

**M11**：kitty 探测字节/400ms/tmux 透传/焦点窗格/WezTerm-Konsole 排除/尺寸界 `src/tui/gfx.rs:23-57,101-260`；DECSTBM 推 scrollback、两行最小 `term.rs:100-121,465-470`；写一次/惰性冻结块模型 `statics.rs:1-27`；OSC 9/99/777 与 tmux 包裹 `notify.rs:1-23,85-93`；高亮器选型 `Cargo.toml:33-36`；`zune-jpeg` 在 rustc ≥1.96 需 patch `Cargo.toml:45-53`（改用别的解码器）；虚拟化先剖析 `cc-gap-analysis.md:78,119,159`。

**明确不移植**：StreamingToolExecutor、预计算压缩、远程旗标、agent/http/prompt 类 hook、分布式团队记忆、React/Yoga 移植、cache-edit microcompact、外部 vim/pager 接管、默认回退模型、`share`、四代会话视图、`Session` 上帝对象、575 行 `query_loop`。
