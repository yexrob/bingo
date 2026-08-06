use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Deserialize;

use crate::agents::{AgentDef, AgentRegistry, Delivery, InboxItem};
use crate::api::types::{Message, SystemBlock};
use crate::channels::ChannelRegistry;
use crate::permission::PermissionMode;
use crate::query::{Session, UiHooks};
use crate::tool::{parse_input, Tool, ToolContext, ToolError, ToolResult};
use crate::watch::{NotifyCondition, WatchId, WatchKind, WatchRegistry, WatchState};

const MAX_AGENT_DEPTH: usize = 3;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct AgentInput {
    #[schemars(description = "子代理的独立任务指令")]
    prompt: String,
    /// 后台化：立即返回 async_launched，完成时通知主 agent。
    #[serde(default)]
    #[schemars(description = "异步执行（默认 true）：立即返回实例名，主 agent 不等待；设 false 则同步等待结果")]
    background: Option<bool>,
    /// 通知条件：子 agent 产出内容出现任一字样即通知主 agent。
    #[serde(default)]
    #[schemars(description = "通知条件：子 agent 产出内容命中任一字样即通知")]
    notify_on: Option<Vec<String>>,
    /// 任务简述（可选），随 header 显示。
    #[serde(default)]
    #[schemars(description = "任务简述（可选）")]
    description: Option<String>,
    /// 子代理模型（可选）：缺省继承具名定义或父会话模型。
    #[serde(default)]
    #[schemars(description = "子代理使用的模型（可选，缺省继承具名定义/父会话）")]
    model: Option<String>,
    /// 子代理 provider（可选，settings.json 的 providers 段）：指定后子代理
    /// 使用该 provider 的端点与 key（独立于父会话的当前 provider）。
    #[serde(default)]
    #[schemars(description = "子代理使用的 provider（可选，settings 的 providers 段；缺省继承具名定义/父会话）")]
    provider: Option<String>,
    /// 实例名（可选）：SendMessage/AgentControl 的地址。
    #[serde(default)]
    #[schemars(description = "实例名（可选）：后续 SendMessage/AgentControl 用它寻址；缺省取具名定义名或 agent，重名自动加 -2/-3 后缀")]
    name: Option<String>,
    /// 具名定义（可选）：`.bingo/agents/<名>.md` 或 `~/.config/bingo/agents/<名>.md`。
    #[serde(default)]
    #[schemars(description = "具名 agent 定义（可选）：使用该定义的 system prompt 与缺省模型/provider")]
    agent: Option<String>,
}

/// 子代理工具（D14/D29）：递归 queryLoop，独立消息历史，结果文本回填父模型。
/// 每次派生登记为注册表实例（名字可寻址），完成后历史保留，
/// 主 agent 经 SendMessage 续话（hub-and-spoke）。
pub struct AgentTool {
    session: Arc<Session>,
    defs: Vec<AgentDef>,
}

impl AgentTool {
    pub fn new(session: Arc<Session>, defs: Vec<AgentDef>) -> Self {
        Self { session, defs }
    }
}

/// 子代理 UI：捕获文本、无交互（写工具在非 bypass 模式下被拒）。
/// cell 记录已产出字符数（后台 agent 的 interval 进度检查）。
fn subagent_hooks(
    output: Arc<Mutex<String>>,
    cell: Arc<AgentCell>,
    permission_mode: PermissionMode,
    watch: Arc<WatchRegistry>,
    id: WatchId,
) -> UiHooks {
    let bypass = permission_mode == PermissionMode::BypassPermissions;
    UiHooks {
        on_event: Box::new(move |event| {
            if let crate::api::types::StreamEvent::TextDelta { text, .. } = event
                && let Ok(mut output) = output.lock()
            {
                output.push_str(text);
                cell.record_chars(text.chars().count());
                // 产出文本进条件引擎（notify_on 命中 → 信号通知）。
                watch.feed_content(id, text);
            }
        }),
        on_tool_ready: Box::new(|_name, _input, _standalone| {}),
        on_tool_done: Box::new(|_| {}),
        on_round_end: Box::new(|| {}),
        on_warning: Box::new(|_| {}),
        ask: std::sync::Arc::new(move |_tool_name, _reason| Box::pin(async move { bypass })),
        // 子代理无 UI 可问：AskUserQuestion 视为未回答（模型应避免在子代理中询问）。
        ask_question: std::sync::Arc::new(|_title, _question, _options| {
            Box::pin(async { None })
        }),
    }
}

/// 单行摘要（label 用）：截到换行/40 字符。
pub(crate) fn excerpt(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    let cut: String = line.chars().take(40).collect();
    if cut.chars().count() < text.chars().count() {
        format!("{cut}…")
    } else {
        cut
    }
}

/// 信箱 → 回合提示：单条 hub 指令保持原文；混合/多条按序标注来源。
/// 频道条目同时推进该成员的已读游标（消息随本回合进入其上下文）。
pub(crate) fn absorb_inbox(
    channels: &Arc<ChannelRegistry>,
    name: &str,
    items: &[InboxItem],
) -> String {
    let mut latest: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
    for item in items {
        if let InboxItem::Channel { channel, seq, .. } = item {
            let cursor = latest.entry(channel.as_str()).or_insert(0);
            if *cursor < *seq {
                *cursor = *seq;
            }
        }
    }
    for (channel, seq) in latest {
        channels.mark_seen(name, channel, seq);
    }
    match items {
        [InboxItem::Direct(m)] => m.clone(),
        _ => items
            .iter()
            .map(|item| match item {
                InboxItem::Direct(m) => format!("[追加指令] {m}"),
                InboxItem::Channel {
                    channel,
                    from,
                    text,
                    seq,
                } => format!("[#{channel} 第{seq}条] {from}: {text}"),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// 空产出占位。
fn non_empty(text: String) -> String {
    if text.trim().is_empty() {
        "[subagent returned no text]".to_string()
    } else {
        text
    }
}

/// 注册一个回合的 watch 行（◉ `{label}` · 已产出 N 字符）。
fn register_run_watch(
    watch: &Arc<WatchRegistry>,
    label: String,
    cell: Arc<AgentCell>,
    conditions: Vec<NotifyCondition>,
) -> WatchId {
    watch.register_with_conditions(
        Box::new(AgentWatch {
            cell,
            label,
            interval: Some(std::time::Duration::from_secs(5)),
        }),
        conditions,
    )
}

/// 后台驱动一个实例的回合链：run_query → 历史落注册表 → 信箱非空则
/// 同任务续跑下一回合（新 watch 行），排空转 Idle。abort 句柄挂到注册表
/// （stop/delete 可中止）。返回首回合的 watch id。
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_agent_loop(
    registry: Arc<AgentRegistry>,
    watch: Arc<WatchRegistry>,
    name: String,
    session: Arc<Session>,
    history: Vec<Message>,
    prompt: String,
    first_label: String,
    conditions: Vec<NotifyCondition>,
) -> WatchId {
    let cell = Arc::new(AgentCell::new());
    let first_id = register_run_watch(&watch, first_label, cell.clone(), conditions);
    registry.set_run_watch(&name, first_id);
    let permission_mode = session.permission_mode;
    let loop_registry = registry.clone();
    let loop_name = name.clone();
    let handle = tokio::spawn(async move {
        let name = loop_name;
        let mut history = history;
        let mut prompt = prompt;
        let mut run = (first_id, cell);
        loop {
            let output = Arc::new(Mutex::new(String::new()));
            loop_registry.set_live(&name, Some(output.clone()));
            let mut ui = subagent_hooks(
                output.clone(),
                run.1.clone(),
                permission_mode,
                watch.clone(),
                run.0,
            );
            match crate::query::run_query(&session, history, &prompt, &mut ui, None).await {
                Ok(outcome) => {
                    let text = output.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    loop_registry.set_live(&name, None);
                    watch.set_state(
                        run.0,
                        WatchState::Done,
                        Some("完成".to_string()),
                        Some(serde_json::json!(non_empty(text))),
                    );
                    match loop_registry.finish(&name, outcome.messages) {
                        Some((next_history, items)) => {
                            history = next_history;
                            prompt = absorb_inbox(&session.channels, &name, &items);
                            let cell = Arc::new(AgentCell::new());
                            let n = loop_registry.next_run(&name);
                            let label = format!("{name} #{n} · {}", excerpt(&prompt));
                            let id = register_run_watch(&watch, label, cell.clone(), Vec::new());
                            loop_registry.set_run_watch(&name, id);
                            run = (id, cell);
                        }
                        None => break,
                    }
                }
                Err(e) => {
                    loop_registry.set_live(&name, None);
                    watch.set_state(
                        run.0,
                        WatchState::Failed,
                        Some(format!("subagent failed: {e}")),
                        None,
                    );
                    loop_registry.mark_idle(&name);
                    break;
                }
            }
        }
    });
    registry.set_abort(&name, handle.abort_handle());
    first_id
}

impl AgentTool {
    /// 解析具名定义（agent 参数）。
    fn resolve_def(&self, params: &AgentInput) -> Result<Option<&AgentDef>, ToolError> {
        let Some(want) = &params.agent else {
            return Ok(None);
        };
        self.defs
            .iter()
            .find(|d| &d.name == want)
            .map(Some)
            .ok_or_else(|| {
                let known: Vec<&str> = self.defs.iter().map(|d| d.name.as_str()).collect();
                ToolError::failed(if known.is_empty() {
                    format!("unknown agent definition: {want}（没有任何具名定义）")
                } else {
                    format!(
                        "unknown agent definition: {want}；可用：{}",
                        known.join(", ")
                    )
                })
            })
    }

    /// 实例落地：认领名字 → 构造子会话（携带实例名，Post 盖戳用）→
    /// 注册表登记。返回 (实例名, 描述, 子会话)。
    fn spawn_instance(
        &self,
        params: &AgentInput,
        def: Option<&AgentDef>,
    ) -> Result<(String, String, Arc<Session>), ToolError> {
        let base = params
            .name
            .clone()
            .or_else(|| def.map(|d| d.name.clone()))
            .unwrap_or_else(|| "agent".to_string());
        let name = self.session.agents.claim_name(&base);
        let sub_session = self.build_sub_session(params, def, &name)?;
        let description = params
            .description
            .clone()
            .unwrap_or_else(|| excerpt(&params.prompt));
        self.session.agents.insert(
            &name,
            def.map(|d| d.name.clone()),
            description.clone(),
            sub_session.clone(),
        );
        Ok((name, description, sub_session))
    }

    fn launch_background(
        &self,
        params: &AgentInput,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let def = self.resolve_def(params)?;
        let (name, description, sub_session) = self.spawn_instance(params, def)?;
        let _ = self.session.agents.next_run(&name);
        let conditions = params
            .notify_on
            .clone()
            .map(|p| vec![NotifyCondition::Contains(p)])
            .unwrap_or_default();
        let id = spawn_agent_loop(
            self.session.agents.clone(),
            ctx.watch.clone(),
            name.clone(),
            sub_session,
            Vec::new(),
            params.prompt.clone(),
            format!("{name} · {description}"),
            conditions,
        );
        Ok(ToolResult {
            content: serde_json::Value::String(serde_json::json!({
                "status": "async_launched",
                "name": name,
                "task_id": id.0,
                "note": "子代理已在后台执行，完成通知会注入下一轮上下文；SendMessage 可发后续指令，AgentControl 可 list/stop/delete",
            })
            .to_string()),
            is_error: false,
            diff: None,
        })
    }

    /// 构造子代理会话：具名定义提供 system prompt 与缺省模型/provider，
    /// 显式参数优先于定义、定义优先于继承（provider 指定时 fork 独立端点
    /// Client，互不影响父会话的当前 provider）。
    fn build_sub_session(
        &self,
        params: &AgentInput,
        def: Option<&AgentDef>,
        instance: &str,
    ) -> Result<Arc<Session>, ToolError> {
        build_sub_session(
            &self.session,
            params.model.clone(),
            params.provider.clone(),
            def,
            instance,
        )
    }
}

/// 构造子代理会话（AgentTool 与 team spawn 共用，D31）：
/// 具名定义提供 system prompt 与缺省模型/provider，显式参数优先于定义、
/// 定义优先于继承（provider 指定时 fork 独立端点 Client，互不影响父会话）。
pub(crate) fn build_sub_session(
    parent: &Arc<Session>,
    model: Option<String>,
    provider: Option<String>,
    def: Option<&AgentDef>,
    instance: &str,
) -> Result<Arc<Session>, ToolError> {
    let model = model
        .or_else(|| def.and_then(|d| d.model.clone()))
        .unwrap_or_else(|| parent.runtime.model.borrow().clone());
    let provider = provider.or_else(|| def.and_then(|d| d.provider.clone()));
    let client = match &provider {
        Some(name) => parent.client.with_provider(name).map_err(ToolError::failed)?,
        None => parent.client.clone(),
    };
    let provider_name = provider.unwrap_or_else(|| parent.runtime.provider.borrow().clone());
    let system = match def {
        Some(d) if !d.system.trim().is_empty() => vec![SystemBlock {
            text: d.system.clone(),
            cache: parent.settings.cache_control.unwrap_or(false),
        }],
        _ => parent.system.clone(),
    };
    let runtime = crate::query::Runtime::new(
        model,
        None,
        parent
            .runtime
            .permissions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone(),
    );
    let _ = runtime.provider_tx.send(provider_name);
    Ok(Arc::new(Session {
        client,
        runtime,
        permission_mode: parent.permission_mode,
        settings: parent.settings.clone(),
        system,
        depth: parent.depth + 1,
        home: parent.home.clone(),
        quiet: parent.quiet,
        compact_failures: parent.compact_failures.clone(),
        watch: parent.watch.clone(),
        tasks: parent.tasks.clone(),
        last_task_reminder_turn: parent.last_task_reminder_turn.clone(),
        expand_tasks: parent.expand_tasks.clone(),
        agents: parent.agents.clone(),
        channels: parent.channels.clone(),
        instance: Some(instance.to_string()),
    }))
}

/// 后台 agent 进度：已产出字符数（interval poll 用）。
struct AgentCell {
    chars: std::sync::atomic::AtomicUsize,
}

impl AgentCell {
    fn new() -> Self {
        Self {
            chars: std::sync::atomic::AtomicUsize::new(0),
        }
    }
    fn record_chars(&self, n: usize) {
        self.chars
            .fetch_add(n, std::sync::atomic::Ordering::SeqCst);
    }
    fn poll(&self) -> crate::watch::WatchPoll {
        crate::watch::WatchPoll {
            state: WatchState::Running,
            detail: Some(format!(
                "已产出 {} 字符",
                self.chars.load(std::sync::atomic::Ordering::SeqCst)
            )),
            payload: None,
            signal: None,
        }
    }
}

struct AgentWatch {
    cell: Arc<AgentCell>,
    label: String,
    interval: Option<std::time::Duration>,
}

impl crate::watch::Watchable for AgentWatch {
    fn label(&self) -> String {
        self.label.clone()
    }
    fn poll(&self) -> crate::watch::WatchPoll {
        self.cell.poll()
    }
    fn check_interval(&self) -> Option<std::time::Duration> {
        self.interval
    }
    fn kind(&self) -> WatchKind {
        WatchKind::Agent
    }
}

#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> String {
        "Agent".to_string()
    }

    fn description(&self) -> String {
        let mut desc = "派生子代理执行独立任务（深度受限）。默认异步执行：立即返回实例名与任务 id，主 agent 不等待，子代理完成时自动通知；background:false 可同步等待结果；notify_on 条件命中子代理产出内容时也会通知。实例名可寻址：SendMessage 发后续指令（上下文保留），AgentControl 管理（list/stop/delete）。`agent` 参数使用具名定义（预设 system prompt 与模型）。"
            .to_string();
        if !self.defs.is_empty() {
            desc.push_str("\n\n可用具名定义：");
            for def in &self.defs {
                desc.push_str(&format!("\n- {}: {}", def.name, def.description));
            }
        }
        desc
    }

    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<AgentInput>()
    }

    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }

    fn is_read_only(&self, _input: &serde_json::Value) -> bool {
        false
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: AgentInput = parse_input(&input)?;
        if self.session.depth >= MAX_AGENT_DEPTH {
            return Err(ToolError::failed(format!(
                "max agent depth ({MAX_AGENT_DEPTH}) exceeded"
            )));
        }
        // 默认异步：主 agent 不等待子 agent，完成通知注入下一轮。
        if params.background.unwrap_or(true) {
            return self.launch_background(&params, ctx);
        }

        let def = self.resolve_def(&params)?;
        let (name, description, sub_session) = self.spawn_instance(&params, def)?;
        let _ = self.session.agents.next_run(&name);

        // 前台子 agent 同样可 watch：Running（产出字符量）→ Done/Failed。
        let cell = Arc::new(AgentCell::new());
        let conditions = params
            .notify_on
            .clone()
            .map(|p| vec![NotifyCondition::Contains(p)])
            .unwrap_or_default();
        let id = register_run_watch(
            &ctx.watch,
            format!("{name} · {description}"),
            cell.clone(),
            conditions,
        );
        self.session.agents.set_run_watch(&name, id);
        let output = Arc::new(Mutex::new(String::new()));
        self.session.agents.set_live(&name, Some(output.clone()));
        let mut ui = subagent_hooks(
            output.clone(),
            cell.clone(),
            sub_session.permission_mode,
            ctx.watch.clone(),
            id,
        );
        let sync_run =
            crate::query::run_query(&sub_session, Vec::new(), &params.prompt, &mut ui, None)
                .await;
        self.session.agents.set_live(&name, None);
        match sync_run {
            Ok(outcome) => {
                let text = output.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let content = non_empty(text);
                ctx.watch.set_state(
                    id,
                    WatchState::Done,
                    Some("完成".to_string()),
                    Some(serde_json::json!(content.clone())),
                );
                // 同步路径工具串行执行，排队指令实际到不了这里；万一有，
                // 交给后台环继续（同一续跑机制）。
                if let Some((history, items)) = self.session.agents.finish(&name, outcome.messages)
                {
                    let prompt = absorb_inbox(&sub_session.channels, &name, &items);
                    let n = self.session.agents.next_run(&name);
                    spawn_agent_loop(
                        self.session.agents.clone(),
                        ctx.watch.clone(),
                        name.clone(),
                        sub_session,
                        history,
                        prompt.clone(),
                        format!("{name} #{n} · {}", excerpt(&prompt)),
                        Vec::new(),
                    );
                }
                Ok(ToolResult {
                    content: serde_json::Value::String(content),
                    is_error: false,
                    diff: None,
                })
            }
            Err(e) => {
                ctx.watch.set_state(
                    id,
                    WatchState::Failed,
                    Some(format!("subagent failed: {e}")),
                    None,
                );
                self.session.agents.mark_idle(&name);
                Err(ToolError::failed(format!("subagent failed: {e}")))
            }
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct SendMessageInput {
    #[schemars(description = "目标子代理实例名（Agent 工具返回的 name；AgentControl list 可查）")]
    agent: String,
    #[schemars(description = "要发送的后续指令/消息")]
    message: String,
}

/// 主→子续话通道（hub-and-spoke，仅主会话可用）：空闲实例带完整历史
/// 唤醒续跑；忙碌实例排队、回合结束自动送达。
pub struct SendMessageTool {
    session: Arc<Session>,
}

impl SendMessageTool {
    pub fn new(session: Arc<Session>) -> Self {
        Self { session }
    }
}

#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> String {
        "SendMessage".to_string()
    }
    fn description(&self) -> String {
        "向已派生的子代理实例发送后续指令（上下文保留的续话）。实例空闲：立即唤醒续跑并在完成时通知；实例忙碌：排队，当前回合结束自动送达。实例名来自 Agent 工具返回值或 AgentControl list。".to_string()
    }
    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<SendMessageInput>()
    }
    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }
    fn is_read_only(&self, _input: &serde_json::Value) -> bool {
        false
    }
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: SendMessageInput = parse_input(&input)?;
        let registry = self.session.agents.clone();
        match registry.deliver(&params.agent, &params.message) {
            Ok(Delivery::Queued) => Ok(ToolResult {
                content: serde_json::Value::String(format!(
                    "{} 正在执行，指令已排队（当前回合结束自动送达）",
                    params.agent
                )),
                is_error: false,
                diff: None,
            }),
            Ok(Delivery::Start {
                session,
                history,
                items,
            }) => {
                let prompt = absorb_inbox(&session.channels, &params.agent, &items);
                let n = registry.next_run(&params.agent);
                spawn_agent_loop(
                    registry,
                    ctx.watch.clone(),
                    params.agent.clone(),
                    session,
                    history,
                    prompt.clone(),
                    format!("{} #{n} · {}", params.agent, excerpt(&prompt)),
                    Vec::new(),
                );
                Ok(ToolResult {
                    content: serde_json::Value::String(format!(
                        "{} 已唤醒（历史保留），完成通知会注入下一轮上下文",
                        params.agent
                    )),
                    is_error: false,
                    diff: None,
                })
            }
            Err(e) => Err(ToolError::failed(e)),
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AgentAction {
    /// 列出全部实例（名字/定义/状态/待送指令数）。
    List,
    /// 停止：中止当前回合，不再接收指令；历史保留可 list。
    Stop,
    /// 删除：停止并移除实例（名字释放）。
    Delete,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct AgentControlInput {
    #[schemars(description = "操作：list 列出全部实例 / stop 停止 / delete 停止并移除")]
    action: AgentAction,
    #[serde(default)]
    #[schemars(description = "目标实例名（stop/delete 必填）")]
    agent: Option<String>,
}

/// 子代理生命周期管理（hub-and-spoke，仅主会话可用）。
pub struct AgentControlTool {
    session: Arc<Session>,
}

impl AgentControlTool {
    pub fn new(session: Arc<Session>) -> Self {
        Self { session }
    }

    fn require_agent(input: &AgentControlInput) -> Result<&str, ToolError> {
        input
            .agent
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::failed("stop/delete 需要 agent 参数（实例名）"))
    }
}

#[async_trait]
impl Tool for AgentControlTool {
    fn name(&self) -> String {
        "AgentControl".to_string()
    }
    fn description(&self) -> String {
        "管理子代理实例：list 列出全部（名字/定义/状态/排队指令数），stop 停止（中止当前运行、不再接收，历史保留），delete 停止并移除（名字释放）。".to_string()
    }
    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<AgentControlInput>()
    }
    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }
    fn is_read_only(&self, input: &serde_json::Value) -> bool {
        input.get("action").and_then(|a| a.as_str()) == Some("list")
    }
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: AgentControlInput = parse_input(&input)?;
        let registry = &self.session.agents;
        let text = match params.action {
            AgentAction::List => {
                let statuses = registry.list();
                if statuses.is_empty() {
                    "当前没有子代理实例".to_string()
                } else {
                    statuses
                        .iter()
                        .map(|s| {
                            let def = s
                                .def
                                .as_deref()
                                .map(|d| format!("，定义 {d}"))
                                .unwrap_or_default();
                            let pending = if s.pending > 0 {
                                format!("，{} 条指令排队", s.pending)
                            } else {
                                String::new()
                            };
                            format!(
                                "- {}（{}{def}{pending}）：{}",
                                s.name,
                                s.state.label(),
                                s.description
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            AgentAction::Stop => {
                let name = Self::require_agent(&params)?;
                match registry.stop(name).map_err(ToolError::failed)? {
                    Some(id) => {
                        ctx.watch.set_state(
                            id,
                            WatchState::Cancelled,
                            Some("已停止".to_string()),
                            None,
                        );
                        format!("已停止 {name}（当前回合中止，历史保留）")
                    }
                    None => format!("{name} 已停止（无进行中的回合）"),
                }
            }
            AgentAction::Delete => {
                let name = Self::require_agent(&params)?;
                self.session.channels.remove_member_everywhere(name);
                match registry.remove(name).map_err(ToolError::failed)? {
                    Some(id) => {
                        ctx.watch.set_state(
                            id,
                            WatchState::Cancelled,
                            Some("已删除".to_string()),
                            None,
                        );
                        format!("已删除 {name}（回合中止，名字释放）")
                    }
                    None => format!("已删除 {name}（名字释放）"),
                }
            }
        };
        Ok(ToolResult {
            content: serde_json::Value::String(text),
            is_error: false,
            diff: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::{Runtime, Session};

    fn parent_session() -> (Arc<Session>, Arc<crate::api::client::Client>) {
        let mut settings = crate::settings::Settings::default();
        settings.api_key = Some("sk-parent".into());
        settings.api_base_url = Some("https://parent.example".into());
        settings.providers.insert(
            "ds".to_string(),
            crate::settings::ProviderConfig {
                api_key: "sk-ds".into(),
                api_base_url: "https://api.deepseek.com".into(),
            },
        );
        let client = Arc::new(crate::api::client::Client::from_settings(&settings).unwrap());
        let mut runtime = Runtime::new("parent-model".into(), None, Default::default());
        runtime.mcp = Arc::new(tokio::sync::Mutex::new(crate::mcp::McpManager::new(
            Default::default(),
            Default::default(),
        )));
        let session = Arc::new(Session {
            client: (*client).clone(),
            runtime,
            permission_mode: crate::permission::PermissionMode::Default,
            settings,
            system: vec![SystemBlock {
                text: "父 system".into(),
                cache: false,
            }],
            depth: 0,
            home: std::env::temp_dir(),
            quiet: true,
            compact_failures: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            last_task_reminder_turn: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            expand_tasks: tokio::sync::watch::channel(false).0,
            agents: AgentRegistry::new(),
            channels: crate::channels::ChannelRegistry::new(Default::default()),
            instance: None,
        });
        (session, client)
    }

    fn params(prompt: &str) -> AgentInput {
        AgentInput {
            prompt: prompt.into(),
            background: None,
            notify_on: None,
            description: None,
            model: None,
            provider: None,
            name: None,
            agent: None,
        }
    }

    fn def(name: &str) -> AgentDef {
        AgentDef {
            name: name.into(),
            description: format!("{name} 描述"),
            model: Some("def-model".into()),
            provider: Some("ds".into()),
            system: "你是评审。".into(),
            source: crate::agents::AgentDefSource::Unknown,
        }
    }

    #[test]
    fn sub_session_inherits_model_and_shared_endpoint() {
        let (session, client) = parent_session();
        let tool = AgentTool::new(session.clone(), Vec::new());
        let sub = tool.build_sub_session(&params("do it"), None, "sub").unwrap();
        assert_eq!(*sub.runtime.model.borrow(), "parent-model");
        assert_eq!(
            sub.client.current_endpoint(),
            ("sk-parent".to_string(), "https://parent.example".to_string())
        );
        assert_eq!(sub.system.len(), 1, "无定义时继承父 system");
        assert_eq!(sub.system[0].text, "父 system");
        // 不指定 provider：共享父端点（切换父 provider 子跟随）。
        client.set_provider("ds").unwrap();
        assert_eq!(
            sub.client.current_endpoint().0,
            "sk-ds",
            "共享端点跟随父会话切换"
        );
    }

    #[test]
    fn sub_session_overrides_model_and_provider() {
        let (session, _client) = parent_session();
        let tool = AgentTool::new(session.clone(), Vec::new());
        let mut p = params("do it");
        p.model = Some("sub-model".into());
        p.provider = Some("ds".into());
        let sub = tool.build_sub_session(&p, None, "sub").unwrap();
        assert_eq!(*sub.runtime.model.borrow(), "sub-model");
        assert_eq!(sub.runtime.provider.borrow().as_str(), "ds");
        assert_eq!(
            sub.client.current_endpoint(),
            ("sk-ds".to_string(), "https://api.deepseek.com".to_string())
        );
        // fork 独立端点：父会话不受影响。
        assert_eq!(session.client.current_endpoint().0, "sk-parent");
    }

    #[test]
    fn named_def_supplies_system_and_defaults() {
        let (session, _client) = parent_session();
        let d = def("reviewer");
        let tool = AgentTool::new(session.clone(), vec![d.clone()]);
        // 定义提供 system/model/provider 缺省。
        let sub = tool.build_sub_session(&params("审查"), Some(&d), "sub").unwrap();
        assert_eq!(sub.system.len(), 1);
        assert_eq!(sub.system[0].text, "你是评审。", "定义正文替换 system");
        assert_eq!(*sub.runtime.model.borrow(), "def-model");
        assert_eq!(sub.runtime.provider.borrow().as_str(), "ds");
        // 显式参数优先于定义。
        let mut p = params("审查");
        p.model = Some("explicit".into());
        let sub = tool.build_sub_session(&p, Some(&d), "sub").unwrap();
        assert_eq!(*sub.runtime.model.borrow(), "explicit");
        // resolve_def：未知定义报错并列出可用项。
        let mut p = params("x");
        p.agent = Some("nope".into());
        let err = tool.resolve_def(&p).unwrap_err().to_string();
        assert!(err.contains("nope") && err.contains("reviewer"), "{err}");
    }

    #[test]
    fn sub_session_unknown_provider_errors() {
        let (session, _client) = parent_session();
        let tool = AgentTool::new(session, Vec::new());
        let mut p = params("do it");
        p.provider = Some("nope".into());
        assert!(
            tool.build_sub_session(&p, None, "sub").is_err(),
            "未知 provider 报错"
        );
    }

    #[test]
    fn schema_exposes_name_and_agent() {
        let (session, _client) = parent_session();
        let tool = AgentTool::new(session, vec![def("reviewer")]);
        let schema = tool.input_schema();
        let props = schema["properties"].as_object().unwrap();
        for key in ["model", "provider", "name", "agent"] {
            assert!(props.contains_key(key), "schema 含 {key}");
        }
        assert!(
            tool.description().contains("- reviewer: reviewer 描述"),
            "描述列出具名定义"
        );
    }

    #[test]
    fn excerpt_is_single_line_and_bounded() {
        assert_eq!(excerpt("短任务"), "短任务");
        assert_eq!(excerpt("第一行\n第二行"), "第一行…");
        let long = "长".repeat(50);
        let cut = excerpt(&long);
        assert!(cut.chars().count() <= 41, "{cut}");
        assert!(cut.ends_with('…'));
    }

    #[tokio::test]
    async fn agent_control_list_stop_delete() {
        let (session, _client) = parent_session();
        session
            .agents
            .insert("scout", None, "调研".into(), session.clone());
        let ctl = AgentControlTool::new(session.clone());
        let ctx = crate::tool::ToolContext {
            cwd: std::path::PathBuf::from("/tmp"),
            watch: session.watch.clone(),
            http: reqwest::Client::new(),
            tasks: session.tasks.clone(),
            hooks: crate::settings::HooksConfig::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
        };
        assert!(ctl.is_read_only(&serde_json::json!({"action": "list"})));
        assert!(!ctl.is_read_only(&serde_json::json!({"action": "stop", "agent": "scout"})));
        let out = ctl
            .call(serde_json::json!({"action": "list"}), &ctx)
            .await
            .unwrap();
        let text = out.content.as_str().unwrap();
        assert!(text.contains("scout") && text.contains("running"), "{text}");
        let out = ctl
            .call(serde_json::json!({"action": "stop", "agent": "scout"}), &ctx)
            .await
            .unwrap();
        assert!(out.content.as_str().unwrap().contains("已停止"), "stop");
        // 停止后 SendMessage 拒收。
        let send = SendMessageTool::new(session.clone());
        let err = send
            .call(
                serde_json::json!({"agent": "scout", "message": "hi"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("已停止"), "{err}");
        let out = ctl
            .call(
                serde_json::json!({"action": "delete", "agent": "scout"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.content.as_str().unwrap().contains("已删除"));
        assert!(session.agents.list().is_empty());
        // 未知实例：stop 报错。
        let err = ctl
            .call(serde_json::json!({"action": "stop", "agent": "ghost"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("ghost"), "{err}");
    }

    #[tokio::test]
    async fn send_message_queues_on_running_instance() {
        let (session, _client) = parent_session();
        session
            .agents
            .insert("worker", None, "干活".into(), session.clone());
        let send = SendMessageTool::new(session.clone());
        let ctx = crate::tool::ToolContext {
            cwd: std::path::PathBuf::from("/tmp"),
            watch: session.watch.clone(),
            http: reqwest::Client::new(),
            tasks: session.tasks.clone(),
            hooks: crate::settings::HooksConfig::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
        };
        let out = send
            .call(
                serde_json::json!({"agent": "worker", "message": "补充"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.content.as_str().unwrap().contains("排队"), "queued");
        assert_eq!(session.agents.list()[0].pending, 1);
        // 未知实例：报错并列出现有实例名。
        let err = send
            .call(serde_json::json!({"agent": "nobody", "message": "x"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("worker"), "{err}");
    }
}
