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
    /// Background mode: returns async_launched immediately and notifies the main agent when done.
    #[serde(default)]
    #[schemars(description = "异步执行（默认 true）：立即返回实例名，主 agent 不等待；设 false 则同步等待结果")]
    background: Option<bool>,
    /// Notification condition: notify the main agent when the sub-agent output contains any of these strings.
    #[serde(default)]
    #[schemars(description = "通知条件：子 agent 产出内容命中任一字样即通知")]
    notify_on: Option<Vec<String>>,
    /// Short task description (optional), shown in the header.
    #[serde(default)]
    #[schemars(description = "任务简述（可选）")]
    description: Option<String>,
    /// Sub-agent model (optional): defaults to the named definition or parent session model.
    #[serde(default)]
    #[schemars(description = "子代理使用的模型（可选，缺省继承具名定义/父会话）")]
    model: Option<String>,
    /// Sub-agent provider (optional, from the `providers` section of settings.json): when set, the sub-agent
    /// uses that provider's endpoint and key (independent of the parent session's current provider).
    #[serde(default)]
    #[schemars(description = "子代理使用的 provider（可选，settings 的 providers 段；缺省继承具名定义/父会话）")]
    provider: Option<String>,
    /// Sub-agent thinking level (optional): off | low | medium | high | xhigh | max.
    #[serde(default)]
    #[schemars(description = "子代理思考级别（可选）：off/low/medium/high/xhigh/max；缺省继承具名定义/父会话当前级别")]
    thinking: Option<String>,
    /// Instance name (optional): address used by SendMessage/AgentControl.
    #[serde(default)]
    #[schemars(description = "实例名（可选）：后续 SendMessage/AgentControl 用它寻址；缺省取具名定义名或 agent，重名自动加 -2/-3 后缀")]
    name: Option<String>,
    /// Named definition (optional): `.bingo/agents/<name>.md` or `~/.config/bingo/agents/<name>.md`.
    #[serde(default)]
    #[schemars(description = "具名 agent 定义（可选）：使用该定义的 system prompt 与缺省模型/provider")]
    agent: Option<String>,
}

/// Sub-agent tool (D14/D29): recursive query loop with its own message history; result text is fed back
/// to the parent model. Each spawn is registered as a registry instance (addressable by name); history
/// is kept after completion and the main agent resumes the conversation via SendMessage (hub-and-spoke).
pub struct AgentTool {
    session: Arc<Session>,
    defs: Vec<AgentDef>,
}

impl AgentTool {
    pub fn new(session: Arc<Session>, defs: Vec<AgentDef>) -> Self {
        Self { session, defs }
    }
}

/// Sub-agent UI: captures text, no interaction (write tools are rejected unless in bypass mode).
/// The cell tracks the number of characters produced (for interval progress checks of background agents).
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
                // Feed produced text into the condition engine (notify_on hit → signal notification).
                watch.feed_content(id, text);
            }
        }),
        on_tool_ready: Box::new(|_name, _input, _standalone| {}),
        on_tool_done: Box::new(|_| {}),
        on_round_end: Box::new(|| {}),
        on_warning: Box::new(|_| {}),
        ask: std::sync::Arc::new(move |_tool_name, _reason| Box::pin(async move { bypass })),
        // Sub-agents have no UI to ask: AskUserQuestion is treated as unanswered (models should avoid asking inside sub-agents).
        ask_question: std::sync::Arc::new(|_title, _question, _options| {
            Box::pin(async { None })
        }),
    }
}

/// Single-line excerpt (for labels): cut at newline / 40 characters.
pub(crate) fn excerpt(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    let cut: String = line.chars().take(40).collect();
    if cut.chars().count() < text.chars().count() {
        format!("{cut}…")
    } else {
        cut
    }
}

/// Inbox → turn prompt: a single hub instruction is kept verbatim; mixed or multiple entries are
/// annotated with their sources in order. Channel entries also advance the member's read cursor
/// (messages enter its context with this turn).
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

/// Placeholder for empty output.
fn non_empty(text: String) -> String {
    if text.trim().is_empty() {
        "[subagent returned no text]".to_string()
    } else {
        text
    }
}

/// Register a watch line for a run (◉ `{label}` · produced N chars).
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

/// Drive an instance's run chain in the background: run_query → history saved to the registry → if
/// the inbox is non-empty, continue with the next run of the same task (new watch line); once drained,
/// transition to Idle. The abort handle is attached to the registry (stop/delete can abort).
/// Returns the watch id of the first run.
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
            match crate::query::run_query(&session, history, &prompt, &[], &mut ui, None).await {
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
    /// Resolve the named definition (agent parameter).
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

    /// Spawn an instance: claim a name → build a sub-session (carrying the instance name for Post
    /// stamps) → register in the registry. Returns (instance name, description, sub-session).
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

    /// Build a sub-agent session: the named definition provides the system prompt and default
    /// model/provider; explicit parameters take precedence over the definition, which takes
    /// precedence over inheritance (when a provider is set, fork an independent-endpoint client
    /// so the parent session's current provider is unaffected).
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
            params.thinking.clone(),
            def,
            instance,
        )
    }
}

/// Build a sub-agent session (shared by AgentTool and team spawn, D31):
/// the named definition provides the system prompt and default model/provider; explicit parameters
/// take precedence over the definition, which takes precedence over inheritance (when a provider is
/// set, fork an independent-endpoint client so the parent session is unaffected).
pub(crate) fn build_sub_session(
    parent: &Arc<Session>,
    model: Option<String>,
    provider: Option<String>,
    thinking: Option<String>,
    def: Option<&AgentDef>,
    instance: &str,
) -> Result<Arc<Session>, ToolError> {
    let model = model
        .or_else(|| def.and_then(|d| d.model.clone()))
        .unwrap_or_else(|| parent.runtime.model.borrow().clone());
    let provider = provider.or_else(|| def.and_then(|d| d.provider.clone()));
    let thinking = thinking
        .or_else(|| def.and_then(|d| d.thinking.clone()))
        .or_else(|| parent.runtime.thinking.borrow().clone());
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
    let _ = runtime.thinking_tx.send(thinking);
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

/// Background agent progress: characters produced (for interval polling).
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
        let mut desc = "派生子代理执行独立任务（深度受限）。默认异步执行：立即返回实例名与任务 id，主 agent 不等待，子代理完成时自动通知；background:false 可同步等待结果；notify_on 条件命中子代理产出内容时也会通知。实例名可寻址：SendMessage 发后续指令（上下文保留），AgentControl 管理（list/stop/delete）。`agent` 参数使用具名定义（预设 system prompt 与模型）；model/provider/thinking 参数可逐实例指定（缺省继承具名定义或父会话）。"
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
        // Async by default: the main agent does not wait for the sub-agent; the completion
        // notification is injected into the next turn.
        if params.background.unwrap_or(true) {
            return self.launch_background(&params, ctx);
        }

        let def = self.resolve_def(&params)?;
        let (name, description, sub_session) = self.spawn_instance(&params, def)?;
        let _ = self.session.agents.next_run(&name);

        // Foreground sub-agents can also be watched: Running (characters produced) → Done/Failed.
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
            crate::query::run_query(&sub_session, Vec::new(), &params.prompt, &[], &mut ui, None)
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
                // On the synchronous path tools run serially, so queued messages never reach here;
                // if one somehow does, hand it to the background loop (same continuation mechanism).
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

/// Main→sub continuation channel (hub-and-spoke, main session only): an idle instance is woken
/// with its full history to continue; a busy instance queues the message and it is delivered
/// when the turn ends.
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
    /// List all instances (name/definition/status/pending message count).
    List,
    /// Stop: abort the current run and stop accepting messages; history is kept and can be listed.
    Stop,
    /// Delete: stop and remove the instance (name released).
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

/// Sub-agent lifecycle management (hub-and-spoke, main session only).
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
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-parent".into()),
            api_base_url: Some("https://parent.example".into()),
            ..Default::default()
        };
        settings.providers.insert(
            "ds".to_string(),
            crate::settings::ProviderConfig {
                api_key: "sk-ds".into(),
                api_base_url: "https://api.deepseek.com".into(),
                supports_images: None,
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
            thinking: None,
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
            thinking: Some("high".into()),
            system: "你是评审。".into(),
            source: crate::agents::AgentDefSource::Unknown,
        }
    }

    #[test]
    fn sub_session_inherits_model_and_shared_endpoint() {
        let (session, client) = parent_session();
        let _ = session.runtime.thinking_tx.send(Some("medium".into()));
        let tool = AgentTool::new(session.clone(), Vec::new());
        let sub = tool.build_sub_session(&params("do it"), None, "sub").unwrap();
        assert_eq!(*sub.runtime.model.borrow(), "parent-model");
        assert_eq!(
            sub.client.current_endpoint(),
            ("sk-parent".to_string(), "https://parent.example".to_string())
        );
        assert_eq!(sub.system.len(), 1, "无定义时继承父 system");
        assert_eq!(sub.system[0].text, "父 system");
        assert_eq!(
            sub.runtime.thinking.borrow().as_deref(),
            Some("medium"),
            "无显式/定义时继承父会话当前思考级别"
        );
        // No provider specified: shares the parent endpoint (follows the parent's provider switch).
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
        p.thinking = Some("xhigh".into());
        let sub = tool.build_sub_session(&p, None, "sub").unwrap();
        assert_eq!(*sub.runtime.model.borrow(), "sub-model");
        assert_eq!(sub.runtime.provider.borrow().as_str(), "ds");
        assert_eq!(
            sub.client.current_endpoint(),
            ("sk-ds".to_string(), "https://api.deepseek.com".to_string())
        );
        assert_eq!(
            sub.runtime.thinking.borrow().as_deref(),
            Some("xhigh"),
            "显式思考级别生效"
        );
        // Forked independent endpoint: the parent session is unaffected.
        assert_eq!(session.client.current_endpoint().0, "sk-parent");
    }

    #[test]
    fn named_def_supplies_system_and_defaults() {
        let (session, _client) = parent_session();
        let d = def("reviewer");
        let tool = AgentTool::new(session.clone(), vec![d.clone()]);
        // The definition supplies system/model/provider/thinking defaults.
        let sub = tool.build_sub_session(&params("审查"), Some(&d), "sub").unwrap();
        assert_eq!(sub.system.len(), 1);
        assert_eq!(sub.system[0].text, "你是评审。", "定义正文替换 system");
        assert_eq!(*sub.runtime.model.borrow(), "def-model");
        assert_eq!(sub.runtime.provider.borrow().as_str(), "ds");
        assert_eq!(
            sub.runtime.thinking.borrow().as_deref(),
            Some("high"),
            "定义提供思考级别缺省"
        );
        // Explicit parameters take precedence over the definition.
        let mut p = params("审查");
        p.model = Some("explicit".into());
        p.thinking = Some("off".into());
        let sub = tool.build_sub_session(&p, Some(&d), "sub").unwrap();
        assert_eq!(*sub.runtime.model.borrow(), "explicit");
        assert_eq!(
            sub.runtime.thinking.borrow().as_deref(),
            Some("off"),
            "显式思考级别覆盖定义"
        );
        // resolve_def: an unknown definition errors out and lists the available ones.
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
        for key in ["model", "provider", "thinking", "name", "agent"] {
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
            home: std::env::temp_dir(),
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
        // After stopping, SendMessage rejects delivery.
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
        // Unknown instance: stop errors out.
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
            home: std::env::temp_dir(),
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
        // Unknown instance: the error lists the existing instance names.
        let err = send
            .call(serde_json::json!({"agent": "nobody", "message": "x"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("worker"), "{err}");
    }
}
