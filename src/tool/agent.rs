use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Deserialize;

use crate::agents::{AgentDef, AgentRegistry, InboxItem};
use crate::api::contract::SystemBlock;
use crate::api::types::Message;
use crate::channels::ChannelRegistry;
use crate::query::{Session, UiHooks};
use crate::tool::{Tool, ToolContext, ToolError, ToolResult, parse_input};
use crate::watch::{NotifyCondition, WatchId, WatchKind, WatchRegistry, WatchState};

const MAX_AGENT_DEPTH: usize = 3;

/// Appended to every sub-agent's system prompt. The base prompt is written for the session that
/// owns the terminal, and two of its promises do not hold here: rendering images for the user,
/// and being woken by background-task notifications. Say so rather than letting the model plan
/// against a surface it does not have.
const SUBAGENT_NOTE: &str = "\
# You are a subagent

- The main agent (the hub) spawned you for one task. Your final text is returned to the hub
  as its tool result; it is not displayed to the user, and markdown image blocks are not
  rendered for anyone. Put conclusions in the text itself.
- You cannot question the user: AskUserQuestion is not available here. Permission prompts do
  reach the user, but anything else you need must be reported back to the hub.
- Your turn ends when you stop calling tools, and background tasks you started will NOT wake
  you afterwards. Finish what needs finishing within this turn, or state what is still
  pending — the hub can resume you with a follow-up message.";

/// Appended when agent channels are on. Delivery wakes every member, so a room of polite agents
/// deadlocks into acknowledging each other's acknowledgements — one greeting turns into a dozen
/// "got it" messages, each of which wakes everyone again. The Post tool's description already
/// says silence is free; that was not enough, because the model's default social reflex is to
/// answer whatever just arrived. This states the failure mode by name.
///
/// It lives in the system prompt rather than in the wake-up payload deliberately: compaction
/// rewrites the message history but never touches `Session::system`, so the rule is still there
/// on turn fifty, when a long-running member has forgotten everything else about the room.
const CHANNEL_NOTE: &str = "\
# Speaking in a channel

Every channel message is delivered to every member, and delivery wakes them. So silence is the
default, not a failure — post only when your message changes what someone else will do.

Do NOT post to greet, introduce yourself, acknowledge, agree, say you understood, or restate the
plan. Each of those wakes every other member, who then feel the same pull to answer, and the room
fills with courtesies while the work stands still. A useful test: if your draft would be just as
true before you read the message you are answering, don't send it.

Post when you have something only you can supply — a decision someone is blocked on, a
disagreement, a result, a question that must be answered before you can continue. Name the person
you mean.

When you have nothing to add, just stop calling tools. Silence costs nothing and wakes nobody.";

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct AgentInput {
    #[schemars(description = "Independent task instructions for the subagent")]
    prompt: String,
    /// Background mode: returns async_launched immediately and notifies the main agent when done.
    #[serde(default)]
    #[schemars(
        description = "Async execution (default true): returns the instance name immediately without waiting; set false to wait synchronously for the result"
    )]
    background: Option<bool>,
    /// Notification condition: notify the main agent when the sub-agent output contains any of these strings.
    #[serde(default)]
    #[schemars(
        description = "Notify condition: notify when the subagent's output contains any of these strings"
    )]
    notify_on: Option<Vec<String>>,
    /// Short task description (optional), shown in the header.
    #[serde(default)]
    #[schemars(description = "Short task description (optional)")]
    description: Option<String>,
    /// Sub-agent model (optional): defaults to the named definition or parent session model.
    #[serde(default)]
    #[schemars(
        description = "Model for the subagent (optional; inherits the named definition / parent session by default); required when crossing providers — the parent model is not inherited"
    )]
    model: Option<String>,
    /// Sub-agent provider (optional, from the `providers` section of settings.json): when set, the sub-agent
    /// uses that provider's endpoint and key (independent of the parent session's current provider).
    #[serde(default)]
    #[schemars(
        description = "Provider for the subagent (optional; the providers section of settings; \"default\" or omitted = shared parent endpoint; specify model when crossing providers). Also the way to get an image looked at when this endpoint cannot receive one: fork onto an image-capable provider and repeat the `#[image N]` marker in the prompt — the attachment table is shared, so the subagent receives the real image."
    )]
    provider: Option<String>,
    /// Sub-agent thinking level (optional): off | low | medium | high | xhigh | max.
    #[serde(default)]
    #[schemars(
        description = "Thinking level for the subagent (optional): off/low/medium/high/xhigh/max; invalid values are rejected; defaults to off when crossing providers, otherwise inherits the named definition / parent session's current level"
    )]
    thinking: Option<String>,
    /// Instance name (optional): address used by SendMessage/AgentControl.
    #[serde(default)]
    #[schemars(
        description = "Instance name (optional): used to address it later via SendMessage/AgentControl; defaults to the named definition name or agent, with -2/-3 suffixes on name collisions"
    )]
    name: Option<String>,
    /// Named definition (optional): `.bingo/agents/<name>.md` or `~/.config/bingo/agents/<name>.md`.
    #[serde(default)]
    #[schemars(
        description = "Named agent definition (optional): uses that definition's system prompt and default model/provider"
    )]
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

/// Serializes permission prompts forwarded by subagents: several background instances can
/// reach the single user at once, and both prompt surfaces (TUI modal, headless stdin) answer
/// one question at a time. Queue them instead of interleaving.
fn ask_gate() -> &'static tokio::sync::Mutex<()> {
    static GATE: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    GATE.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Sub-agent UI: captures text, renders nothing, and forwards permission prompts to the
/// session that owns the UI. The cell tracks the number of characters produced (for interval
/// progress checks of background agents).
fn subagent_hooks(
    output: Arc<Mutex<String>>,
    cell: Arc<AgentCell>,
    watch: Arc<WatchRegistry>,
    id: WatchId,
    instance: String,
    ask: Option<Arc<crate::query::AskFn>>,
) -> UiHooks {
    UiHooks {
        on_event: Box::new(move |event| {
            if let crate::api::contract::StreamEvent::TextDelta { text, .. } = event
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
        // A subagent has no prompt surface of its own, so its Ask decisions are forwarded to
        // the session that owns the UI, stamped with the instance name. Auto-denying here
        // would fail the tool call as "user denied" without the user ever being asked — and
        // auto-allowing under bypassPermissions would silently clear the safety-check gate
        // that is supposed to survive bypass.
        ask: std::sync::Arc::new(move |tool_name, reason| {
            // No prompt surface attached: both real entry points (TUI, headless) attach one at
            // startup, so this is the embedded/test path — fall back to denying.
            let Some(ask) = ask.clone() else {
                return Box::pin(async { false });
            };
            let request = format!("{instance} · {reason}");
            let tool_name = tool_name.to_string();
            Box::pin(async move {
                let _serialized = ask_gate().lock().await;
                ask(&tool_name, &request).await
            })
        }),
        // AskUserQuestion is not assembled for subagents (see `assemble_tools`); if one ever
        // reaches here, treat it as unanswered rather than blocking on a modal.
        ask_question: std::sync::Arc::new(|_title, _question, _options| Box::pin(async { None })),
    }
}

/// Turn-boundary delivery: start a run for every instance that has messages waiting.
///
/// This is the only place a queued message becomes a running turn. Holding delivery until the
/// boundary is what makes a batch a batch — messages sent during one turn arrive together
/// instead of one per turn — and it doubles as the recovery path for a run chain that died
/// with messages still in its inbox.
pub(crate) fn flush_agent_inbox(session: &Arc<Session>, watch: &Arc<WatchRegistry>) {
    for wake in session.agents.flush_pending() {
        let (prompt, images) = absorb_inbox(&session.channels, &wake.name, &wake.items);
        let label = format!("{} #{} · {}", wake.name, wake.run, excerpt(&prompt));
        spawn_agent_loop(
            session.agents.clone(),
            watch.clone(),
            wake.name,
            wake.session,
            wake.history,
            prompt,
            images,
            label,
            Vec::new(),
            session.instance.clone(),
        );
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

/// Inbox → turn prompt plus the images those instructions carried: a single hub instruction is
/// kept verbatim; mixed or multiple entries are annotated with their sources in order. Channel
/// entries also advance the member's read cursor (messages enter its context with this turn).
pub(crate) fn absorb_inbox(
    channels: &Arc<ChannelRegistry>,
    name: &str,
    items: &[InboxItem],
) -> (String, Vec<crate::api::types::ImageAttachment>) {
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
    let images: Vec<crate::api::types::ImageAttachment> = items
        .iter()
        .filter_map(|item| match item {
            InboxItem::Direct { images, .. } => Some(images.clone()),
            InboxItem::Channel { .. } => None,
        })
        .flatten()
        .collect();
    let prompt = match items {
        [InboxItem::Direct { text, .. }] => text.clone(),
        _ => items
            .iter()
            .map(|item| match item {
                InboxItem::Direct { text, .. } => format!("[追加指令] {text}"),
                InboxItem::Channel {
                    channel,
                    from,
                    text,
                    seq,
                } => format!("[#{channel} 第{seq}条] {from}: {text}"),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    };
    (prompt, images)
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
    owner: Option<String>,
) -> WatchId {
    watch.register_with_conditions(
        Box::new(AgentWatch {
            cell,
            label,
            interval: Some(std::time::Duration::from_secs(5)),
        }),
        conditions,
        owner,
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
    images: Vec<crate::api::types::ImageAttachment>,
    first_label: String,
    conditions: Vec<NotifyCondition>,
    owner: Option<String>,
) -> WatchId {
    let cell = Arc::new(AgentCell::new());
    let first_id = register_run_watch(&watch, first_label, cell.clone(), conditions, owner.clone());
    registry.set_run_watch(&name, first_id);
    let loop_registry = registry.clone();
    let loop_name = name.clone();
    let handle = tokio::spawn(async move {
        let name = loop_name;
        let mut history = history;
        let mut prompt = prompt;
        let mut images = images;
        let mut run = (first_id, cell);
        loop {
            let output = Arc::new(Mutex::new(String::new()));
            loop_registry.set_live(&name, Some(output.clone()));
            let mut ui = subagent_hooks(
                output.clone(),
                run.1.clone(),
                watch.clone(),
                run.0,
                name.clone(),
                loop_registry.ask_fn(),
            );
            match crate::query::run_query(&session, history, &prompt, &images, &mut ui, None).await
            {
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
                        Some(next) => {
                            history = next.history;
                            (prompt, images) = absorb_inbox(&session.channels, &name, &next.items);
                            let cell = Arc::new(AgentCell::new());
                            let label = format!("{name} #{} · {}", next.run, excerpt(&prompt));
                            let id = register_run_watch(
                                &watch,
                                label,
                                cell.clone(),
                                Vec::new(),
                                owner.clone(),
                            );
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
            self.session.attachments.resolve(&params.prompt),
            format!("{name} · {description}"),
            conditions,
            ctx.instance.clone(),
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

/// Normalize a thinking level (explicit parameter / named definition entry): `off` → `None`
/// (no thinking parameter); valid levels pass through; anything else is an error — silently
/// degrading an invalid value to off would let the user believe thinking is on when it isn't,
/// so sub-agent spawn must surface it immediately. Inherited values skip this check
/// (consistent with the main session after `/think`, see [`build_sub_session`]).
fn normalize_thinking(level: &str) -> Result<Option<String>, String> {
    if level == "off" {
        return Ok(None);
    }
    if crate::api::contract::THINKING_LEVELS.contains(&level) {
        return Ok(Some(level.to_string()));
    }
    Err(format!(
        "无效思考级别 \"{level}\"（可用：off/low/medium/high/xhigh/max）"
    ))
}

/// Build a sub-agent session (shared by AgentTool and team spawn, D31):
/// the named definition provides the system prompt and default model/provider; explicit parameters
/// take precedence over the definition, which takes precedence over inheritance. A named provider
/// forks an independent-endpoint client so the parent session is unaffected; "default" or no
/// provider shares the parent endpoint and follows the parent session's switches.
pub(crate) fn build_sub_session(
    parent: &Arc<Session>,
    model: Option<String>,
    provider: Option<String>,
    thinking: Option<String>,
    def: Option<&AgentDef>,
    instance: &str,
) -> Result<Arc<Session>, ToolError> {
    let model = model.or_else(|| def.and_then(|d| d.model.clone()));
    // provider："default" 与未指定等价（共享父端点，跟随父切换）；
    // 仅命名 provider fork 独立端点。未知名字在此报错（即时反馈）。
    let named_provider = provider
        .or_else(|| def.and_then(|d| d.provider.clone()))
        .filter(|p| p != "default");
    let client = match &named_provider {
        Some(name) => parent
            .client
            .with_provider(name)
            .map_err(ToolError::failed)?,
        None => parent.client.clone(),
    };
    let provider_name = named_provider
        .clone()
        .unwrap_or_else(|| parent.runtime.provider.borrow().clone());
    // 跨 provider 判定：fork 到与父当前 provider 不同的端点才跨（未指定
    // provider = 共享父端点，恒同 provider）。跨 provider 时父会话的模型与
    // 思考级别都不可用——模型名会发到错误端点（如 claude-sonnet-5 发到
    // DeepSeek 必然 "model not found"），thinking 参数则可能被端点拒绝。
    let cross_provider = match &named_provider {
        Some(name) => name != parent.runtime.provider.borrow().as_str(),
        None => false,
    };
    let model = match model {
        Some(m) => m,
        None if cross_provider => {
            let parent_provider = parent.runtime.provider.borrow().clone();
            return Err(ToolError::failed(format!(
                "provider \"{}\" 需要 model：跨 provider 不继承父会话模型 \
                 （当前父 provider = \"{parent_provider}\"），请显式指定 model 或去掉 provider",
                named_provider.as_deref().unwrap_or("")
            )));
        }
        None => parent.runtime.model.borrow().clone(),
    };
    // 思考级别：显式参数/定义校验（off→不发参数，非法值报错而非静默失效）；
    // 两者皆无时：跨 provider 缺省 off（不带 thinking 参数，兼容 ds/ollama 端点），
    // 同 provider 继承父会话当前级别快照（与主会话一致的宽松语义）。
    let thinking = match thinking.or_else(|| def.and_then(|d| d.thinking.clone())) {
        Some(level) => normalize_thinking(&level).map_err(ToolError::failed)?,
        None if cross_provider => None,
        None => parent.runtime.thinking.borrow().clone(),
    };
    let cache = parent.settings.cache_control.unwrap_or(false);
    let persona = |text: &str| SystemBlock {
        text: text.to_string(),
        cache,
    };
    let mut system = match def {
        Some(d) if d.system.trim().is_empty() => parent.system.clone(),
        // Replacing wholesale also drops the environment info, CLAUDE.md/AGENTS.md and project
        // memory — rarely what a persona wants, so appending is the default.
        Some(d) if d.inherit_system => {
            let mut blocks = parent.system.clone();
            blocks.push(persona(&d.system));
            blocks
        }
        Some(d) => vec![persona(&d.system)],
        None => parent.system.clone(),
    };
    // Uncached on purpose: a short tail block is not worth another cache breakpoint.
    system.push(SystemBlock {
        text: SUBAGENT_NOTE.to_string(),
        cache: false,
    });
    // Only when the feature is on: channel etiquette is noise for a solo subagent that will
    // never see a room.
    if parent.settings.experimental.agent_channels {
        system.push(SystemBlock {
            text: CHANNEL_NOTE.to_string(),
            cache: false,
        });
    }
    let mut runtime = crate::query::Runtime::new(model, None, Default::default());
    // Share the parent's permission table and MCP connections rather than snapshotting them:
    // `/permissions` edits reach instances that are already running, and a subagent reuses the
    // parent's MCP handshake instead of starting from an empty manager (i.e. no MCP tools).
    runtime.permissions = parent.runtime.permissions.clone();
    runtime.mcp = parent.runtime.mcp.clone();
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
        user_config_dir: parent.user_config_dir.clone(),
        quiet: parent.quiet,
        compact_failures: parent.compact_failures.clone(),
        watch: parent.watch.clone(),
        tasks: parent.tasks.clone(),
        expand_tasks: parent.expand_tasks.clone(),
        agents: parent.agents.clone(),
        channels: parent.channels.clone(),
        instance: Some(instance.to_string()),
        attachments: parent.attachments.clone(),
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
        self.chars.fetch_add(n, std::sync::atomic::Ordering::SeqCst);
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
        let mut desc = "Spawn a subagent for an independent task (depth-limited). Async by default: returns the instance name and task id immediately without waiting; a completion notification is injected when the subagent finishes; background:false waits synchronously for the result; notify_on also notifies when the subagent's output matches. The instance name is addressable: SendMessage sends follow-up instructions (context preserved), AgentControl manages (list/stop/delete). The `agent` argument uses a named definition (preset system prompt and model); model/provider/thinking can be set per instance (defaulting to the named definition or parent session)."
            .to_string();
        if !self.defs.is_empty() {
            desc.push_str("\n\nAvailable named definitions:");
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
            ctx.instance.clone(),
        );
        self.session.agents.set_run_watch(&name, id);
        let output = Arc::new(Mutex::new(String::new()));
        self.session.agents.set_live(&name, Some(output.clone()));
        let mut ui = subagent_hooks(
            output.clone(),
            cell.clone(),
            ctx.watch.clone(),
            id,
            name.clone(),
            self.session.agents.ask_fn(),
        );
        let images = self.session.attachments.resolve(&params.prompt);
        let sync_run = crate::query::run_query(
            &sub_session,
            Vec::new(),
            &params.prompt,
            &images,
            &mut ui,
            None,
        )
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
                if let Some(next) = self.session.agents.finish(&name, outcome.messages) {
                    let (prompt, images) = absorb_inbox(&sub_session.channels, &name, &next.items);
                    spawn_agent_loop(
                        self.session.agents.clone(),
                        ctx.watch.clone(),
                        name.clone(),
                        sub_session,
                        next.history,
                        prompt.clone(),
                        images,
                        format!("{name} #{} · {}", next.run, excerpt(&prompt)),
                        Vec::new(),
                        ctx.instance.clone(),
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
    #[schemars(
        description = "Target subagent instance name (the name returned by the Agent tool; see AgentControl list)"
    )]
    agent: String,
    #[schemars(description = "Follow-up instruction/message to send")]
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
        "Send a follow-up instruction to a spawned subagent instance (a continuation that keeps its context). Returns a message_id: the message is queued and delivered at the end of this turn, batched with any other message sent to the same instance in this turn, so the receiver reads them together rather than one per turn. Queued is not yet an acknowledgement — AgentControl(action=messages) reports whether it was delivered, is still queued, or was dropped because the instance stopped. The instance name comes from the Agent tool's return value or AgentControl list.".to_string()
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
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: SendMessageInput = parse_input(&input)?;
        let images = self.session.attachments.resolve(&params.message);
        let id = self
            .session
            .agents
            .deliver(&params.agent, &params.message, images)
            .map_err(ToolError::failed)?;
        Ok(ToolResult {
            content: serde_json::json!({
                "status": "queued",
                "message_id": id.0,
                "agent": params.agent,
                "note": "本回合结束时与同一收件人的其他消息合并成一批送达；\
                         久未确认可用 AgentControl(action=messages, agent=…) 复查",
            })
            .to_string()
            .into(),
            is_error: false,
            diff: None,
        })
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AgentAction {
    /// List all instances (name/definition/status/pending message count).
    List,
    /// Delivery records for one instance: which messages were delivered, are still queued, or
    /// were dropped.
    Messages,
    /// Stop: abort the current run and stop accepting messages; history is kept and can be listed.
    Stop,
    /// Delete: stop and remove the instance (name released).
    Delete,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct AgentControlInput {
    #[schemars(
        description = "Action: list all instances / check message delivery / stop one / delete one"
    )]
    action: AgentAction,
    #[serde(default)]
    #[schemars(description = "Target instance name (required for messages/stop/delete)")]
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
            .ok_or_else(|| ToolError::failed("messages/stop/delete 需要 agent 参数（实例名）"))
    }
}

#[async_trait]
impl Tool for AgentControlTool {
    fn name(&self) -> String {
        "AgentControl".to_string()
    }
    fn description(&self) -> String {
        "Manage subagent instances: list all (name/definition/status/queued-instruction count), check messages sent to one (per-message delivered/queued/dropped plus how long it has been waiting — use this when a SendMessage has gone unacknowledged for a while), stop one (aborts the current run, stops accepting instructions; history kept), delete one (stops and removes it; the name is released).".to_string()
    }
    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<AgentControlInput>()
    }
    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }
    fn is_read_only(&self, input: &serde_json::Value) -> bool {
        matches!(
            input.get("action").and_then(|a| a.as_str()),
            Some("list") | Some("messages")
        )
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
                            let unacked = if s.unacked > 0 {
                                format!("，{} 条未确认", s.unacked)
                            } else {
                                String::new()
                            };
                            format!(
                                "- {}（{}{def}{pending}{unacked}）：{}",
                                s.name,
                                s.state.label(),
                                s.description
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            AgentAction::Messages => {
                let name = Self::require_agent(&params)?;
                let acks = registry
                    .acks_of(name)
                    .ok_or_else(|| ToolError::failed(format!("没有名为 {name} 的子代理")))?;
                if acks.is_empty() {
                    format!("还没有发给 {name} 的消息")
                } else {
                    acks.iter()
                        .map(|a| {
                            let detail = match &a.state {
                                crate::agents::AckState::Queued => format!(
                                    "queued（等待 {}s，将在下一个回合边界成批送达）",
                                    a.queued_at.elapsed().as_secs()
                                ),
                                crate::agents::AckState::Delivered { run } => {
                                    format!("delivered（第 {run} 轮已进入上下文）")
                                }
                                crate::agents::AckState::Dropped { reason } => {
                                    format!("dropped（{reason}，未送达）")
                                }
                            };
                            format!("- #{} {detail}：{}", a.id, a.excerpt)
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            AgentAction::Stop => {
                let name = Self::require_agent(&params)?;
                let (watch_id, dropped) = registry.stop(name).map_err(ToolError::failed)?;
                let lost = if dropped > 0 {
                    format!("，{dropped} 条未送达指令已丢弃")
                } else {
                    String::new()
                };
                match watch_id {
                    Some(id) => {
                        ctx.watch.set_state(
                            id,
                            WatchState::Cancelled,
                            Some("已停止".to_string()),
                            None,
                        );
                        format!("已停止 {name}（当前回合中止，历史保留{lost}）")
                    }
                    None => format!("{name} 已停止（无进行中的回合{lost}）"),
                }
            }
            AgentAction::Delete => {
                let name = Self::require_agent(&params)?;
                self.session.channels.remove_member_everywhere(name);
                let (watch_id, dropped) = registry.remove(name).map_err(ToolError::failed)?;
                // The ack trail is removed with the instance, so this count is the sender's
                // last chance to learn that queued instructions never landed.
                let lost = if dropped > 0 {
                    format!("，{dropped} 条未送达指令已丢弃")
                } else {
                    String::new()
                };
                match watch_id {
                    Some(id) => {
                        ctx.watch.set_state(
                            id,
                            WatchState::Cancelled,
                            Some("已删除".to_string()),
                            None,
                        );
                        format!("已删除 {name}（回合中止，名字释放{lost}）")
                    }
                    None => format!("已删除 {name}（名字释放{lost}）"),
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
            // Explicitly opted out: models a compat proxy that speaks the protocol but rejects
            // image blocks. Image support is otherwise the default.
            send_images: Some(false),
            ..Default::default()
        };
        settings.providers.insert(
            "ds".to_string(),
            crate::settings::ProviderConfig {
                api_key: Some("sk-ds".into()),
                api_base_url: "https://api.deepseek.com".into(),
                supports_images: None,
                protocol: None,
                oauth: None,
            },
        );
        // An image-capable endpoint next to a text-only default: the shape that lets a text-only
        // session delegate an attachment to a subagent.
        settings.providers.insert(
            "vision".to_string(),
            crate::settings::ProviderConfig {
                api_key: Some("sk-v".into()),
                api_base_url: "https://vision.example".into(),
                supports_images: Some(true),
                protocol: None,
                oauth: None,
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
            user_config_dir: std::env::temp_dir().join(".config"),
            quiet: true,
            compact_failures: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            expand_tasks: tokio::sync::watch::channel(false).0,
            agents: AgentRegistry::new(),
            channels: crate::channels::ChannelRegistry::new(Default::default()),
            instance: None,
            attachments: crate::api::image::Attachments::new(),
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
            inherit_system: true,
            source: crate::agents::AgentDefSource::Unknown,
        }
    }

    /// 提取 build_sub_session 错误文本（Arc<Session> 无 Debug，unwrap_err 不可用）。
    fn sub_err(r: Result<Arc<Session>, ToolError>) -> String {
        match r {
            Err(e) => e.to_string(),
            Ok(_) => panic!("expected build_sub_session error"),
        }
    }

    #[test]
    fn sub_session_inherits_model_and_shared_endpoint() {
        let (session, client) = parent_session();
        let _ = session.runtime.thinking_tx.send(Some("medium".into()));
        let tool = AgentTool::new(session.clone(), Vec::new());
        let sub = tool
            .build_sub_session(&params("do it"), None, "sub")
            .unwrap();
        assert_eq!(*sub.runtime.model.borrow(), "parent-model");
        assert_eq!(
            sub.client.current_endpoint(),
            (
                Some("sk-parent".to_string()),
                "https://parent.example".to_string()
            )
        );
        assert_eq!(sub.system[0].text, "父 system", "无定义时继承父 system");
        assert_eq!(
            sub.runtime.thinking.borrow().as_deref(),
            Some("medium"),
            "无显式/定义时继承父会话当前思考级别"
        );
        // No provider specified: shares the parent endpoint (follows the parent's provider switch).
        client.set_provider("ds").unwrap();
        assert_eq!(
            sub.client.current_endpoint().0.as_deref(),
            Some("sk-ds"),
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
            (
                Some("sk-ds".to_string()),
                "https://api.deepseek.com".to_string()
            )
        );
        assert_eq!(
            sub.runtime.thinking.borrow().as_deref(),
            Some("xhigh"),
            "显式思考级别生效"
        );
        // Forked independent endpoint: the parent session is unaffected.
        assert_eq!(
            session.client.current_endpoint().0.as_deref(),
            Some("sk-parent")
        );
    }

    #[test]
    fn named_def_supplies_system_and_defaults() {
        let (session, _client) = parent_session();
        let d = def("reviewer");
        let tool = AgentTool::new(session.clone(), vec![d.clone()]);
        // The definition supplies system/model/provider/thinking defaults.
        let sub = tool
            .build_sub_session(&params("审查"), Some(&d), "sub")
            .unwrap();
        // Default is append: 父 system + 人设 + 子代理说明块。
        let texts: Vec<&str> = sub.system.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(
            texts,
            ["父 system", "你是评审。", SUBAGENT_NOTE],
            "具名定义默认追加而非替换"
        );
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
            None,
            "显式 off 归一化为不发参数"
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
    fn sub_session_cross_provider_requires_model() {
        // 父 provider = "default"（parent_session 缺省）。
        let (session, _client) = parent_session();
        // 仅指定 provider、无 model → 早失败：不继承父模型（避免 claude-sonnet-5
        // 发到 DeepSeek 端点 "model not found"）。
        let tool = AgentTool::new(session.clone(), Vec::new());
        let mut p = params("do it");
        p.provider = Some("ds".into());
        let err = sub_err(tool.build_sub_session(&p, None, "sub"));
        assert!(
            err.contains("需要 model") && err.contains("ds"),
            "跨 provider 需要显式 model：{err}"
        );
        // 定义提供 provider 但无 model → 同样报错。
        let mut d = def("reviewer");
        d.model = None;
        let tool = AgentTool::new(session.clone(), vec![d.clone()]);
        let err = sub_err(tool.build_sub_session(&params("审查"), Some(&d), "sub"));
        assert!(
            err.contains("需要 model"),
            "定义侧跨 provider 同样报错：{err}"
        );
        // 同 provider（父当前就是 ds）→ 继承模型，不报错。
        let _ = session.runtime.provider_tx.send("ds".into());
        let tool = AgentTool::new(session.clone(), Vec::new());
        let mut p = params("do it");
        p.provider = Some("ds".into());
        let sub = tool.build_sub_session(&p, None, "sub").unwrap();
        assert_eq!(
            *sub.runtime.model.borrow(),
            "parent-model",
            "同 provider 继承父模型"
        );
    }

    #[test]
    fn sub_session_cross_provider_defaults_thinking_off() {
        let (session, _client) = parent_session();
        let _ = session.runtime.thinking_tx.send(Some("xhigh".into()));
        let tool = AgentTool::new(session.clone(), Vec::new());
        // 跨 provider 且无显式/定义 thinking → 缺省 off（不带 thinking 参数，
        // 兼容 DeepSeek/Ollama 端点）。
        let mut p = params("do it");
        p.provider = Some("ds".into());
        p.model = Some("ds-model".into());
        let sub = tool.build_sub_session(&p, None, "sub").unwrap();
        assert_eq!(
            sub.runtime.thinking.borrow().as_deref(),
            None,
            "跨 provider 缺省 off"
        );
        // 跨 provider 显式 thinking 仍生效。
        let mut p = params("do it");
        p.provider = Some("ds".into());
        p.model = Some("ds-model".into());
        p.thinking = Some("high".into());
        let sub = tool.build_sub_session(&p, None, "sub").unwrap();
        assert_eq!(sub.runtime.thinking.borrow().as_deref(), Some("high"));
    }

    #[test]
    fn sub_session_same_provider_inherits_thinking() {
        let (session, _client) = parent_session();
        let _ = session.runtime.thinking_tx.send(Some("xhigh".into()));
        let _ = session.runtime.provider_tx.send("ds".into());
        let tool = AgentTool::new(session.clone(), Vec::new());
        let mut p = params("do it");
        p.provider = Some("ds".into());
        let sub = tool.build_sub_session(&p, None, "sub").unwrap();
        assert_eq!(
            sub.runtime.thinking.borrow().as_deref(),
            Some("xhigh"),
            "同 provider 维持继承快照"
        );
    }

    #[test]
    fn sub_session_default_provider_aliases_parent_endpoint() {
        let (session, client) = parent_session();
        let tool = AgentTool::new(session.clone(), Vec::new());
        // 显式 "default"：共享父端点，不 fork、不报错。
        let mut p = params("do it");
        p.provider = Some("default".into());
        let sub = tool.build_sub_session(&p, None, "sub").unwrap();
        assert_eq!(sub.runtime.provider.borrow().as_str(), "default");
        assert_eq!(
            sub.client.current_endpoint(),
            (
                Some("sk-parent".to_string()),
                "https://parent.example".to_string()
            )
        );
        // 共享端点跟随父切换（"default" 与未指定等价）。
        client.set_provider("ds").unwrap();
        let _ = session.runtime.provider_tx.send("ds".into());
        assert_eq!(sub.client.current_endpoint().0.as_deref(), Some("sk-ds"));
        // AgentDef frontmatter provider: default 同路径（跟随父当前 provider 名）。
        let mut d = def("reviewer");
        d.provider = Some("default".into());
        let tool = AgentTool::new(session.clone(), vec![d.clone()]);
        let sub = tool
            .build_sub_session(&params("审查"), Some(&d), "sub")
            .unwrap();
        assert_eq!(sub.runtime.provider.borrow().as_str(), "ds");
    }

    #[test]
    fn sub_session_rejects_invalid_thinking() {
        let (session, _client) = parent_session();
        let tool = AgentTool::new(session.clone(), Vec::new());
        for bad in ["auto", "super", "HIGH"] {
            let mut p = params("do it");
            p.thinking = Some(bad.into());
            let err = sub_err(tool.build_sub_session(&p, None, "sub"));
            assert!(err.contains("无效思考级别"), "非法档位 {bad:?} 报错：{err}");
        }
        // 定义侧非法值同样报错。
        let mut d = def("reviewer");
        d.thinking = Some("bogus".into());
        let tool = AgentTool::new(session.clone(), vec![d.clone()]);
        let err = sub_err(tool.build_sub_session(&params("审查"), Some(&d), "sub"));
        assert!(err.contains("无效思考级别"), "定义侧非法值报错：{err}");
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
            instance: None,
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
            .call(
                serde_json::json!({"action": "stop", "agent": "scout"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.content.as_str().unwrap().contains("已停止"), "stop");
        // After stopping, SendMessage rejects delivery.
        let send = SendMessageTool::new(session.clone());
        let err = send
            .call(serde_json::json!({"agent": "scout", "message": "hi"}), &ctx)
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
            .call(
                serde_json::json!({"action": "stop", "agent": "ghost"}),
                &ctx,
            )
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
            instance: None,
        };
        let out = send
            .call(
                serde_json::json!({"agent": "worker", "message": "补充"}),
                &ctx,
            )
            .await
            .unwrap();
        // The receipt carries the message id; delivery itself waits for the turn boundary.
        let receipt: serde_json::Value =
            serde_json::from_str(out.content.as_str().unwrap_or_default())
                .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(receipt["status"], "queued");
        assert_eq!(receipt["message_id"], 1);
        let status = &session.agents.list()[0];
        assert_eq!(status.pending, 1);
        assert_eq!(status.unacked, 1, "queued 还不是回执");
        // Unknown instance: the error lists the existing instance names.
        let err = send
            .call(serde_json::json!({"agent": "nobody", "message": "x"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("worker"), "{err}");
    }

    /// The hub forwards an image to a subagent by repeating its `#[image N]` marker: the
    /// attachment table is shared with the sub-session, and the resolved images ride along with
    /// the queued instruction so a busy instance still receives them.
    #[test]
    fn image_markers_resolve_for_spawn_and_follow_up() {
        let (session, _client) = parent_session();
        let png = {
            let img = image::RgbaImage::from_pixel(4, 2, image::Rgba([255u8, 0, 0, 255]));
            let mut out = Vec::new();
            image::DynamicImage::ImageRgba8(img)
                .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                .unwrap_or_else(|_| unreachable!());
            out
        };
        assert_eq!(session.attachments.register(&png), Some(1));

        // Spawn: markers in the prompt resolve against the session table.
        let images = session.attachments.resolve("看这张 #[image 1] 再判断");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].media_type, "image/png");
        // Sub-sessions share the table, so a nested spawn can resolve the same marker.
        let sub = build_sub_session(&session, None, None, None, None, "worker").unwrap();
        assert_eq!(sub.attachments.resolve("#[image 1]").len(), 1);

        // Follow-up: a queued instruction keeps its images until it is delivered.
        session
            .agents
            .insert("worker", None, "d".into(), sub.clone());
        let id = session
            .agents
            .deliver("worker", "对比 #[image 1]", images.clone())
            .unwrap_or_else(|e| panic!("{e}"));
        let (prompt, carried) = match session.agents.finish("worker", Vec::new()) {
            Some(next) => absorb_inbox(&sub.channels, "worker", &next.items),
            None => unreachable!("排队消息应在回合结束时取出"),
        };
        let acks = session
            .agents
            .acks_of("worker")
            .unwrap_or_else(|| unreachable!());
        assert_eq!(acks[0].id, id);
        assert_eq!(prompt, "对比 #[image 1]");
        assert_eq!(carried.len(), 1, "图片随排队指令一同送达");
        assert_eq!(carried[0].data, images[0].data);
    }

    /// A text-only main session can still get an image looked at: the attachment table is
    /// session-scoped and independent of endpoint capability, so a subagent forked onto an
    /// image-capable provider resolves the same `#[image N]` marker and actually receives it.
    #[test]
    fn text_only_parent_can_hand_an_image_to_a_vision_subagent() {
        let (parent, _client) = parent_session();
        let png = {
            let img = image::RgbaImage::from_pixel(4, 2, image::Rgba([9u8, 9, 9, 255]));
            let mut out = Vec::new();
            image::DynamicImage::ImageRgba8(img)
                .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                .unwrap_or_else(|_| unreachable!());
            out
        };
        assert_eq!(parent.attachments.register(&png), Some(1));
        assert!(
            !parent.client.supports_images(),
            "父端点不收图（本用例的前提）"
        );

        // Markers resolve regardless of what the parent endpoint can carry.
        let images = parent.attachments.resolve("描述 #[image 1]");
        assert_eq!(images.len(), 1, "解析不受端点能力影响");

        // Forked onto the vision provider, the sub-session is the one whose capability decides.
        let sub = build_sub_session(
            &parent,
            Some("vision-model".into()),
            Some("vision".into()),
            None,
            None,
            "looker",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(sub.client.supports_images(), "子会话端点收图");
        assert!(
            Arc::ptr_eq(&sub.attachments, &parent.attachments),
            "附件表共享，子代理复述占位即可命中"
        );
        assert!(
            parent
                .client
                .image_capable_providers()
                .contains(&"vision".to_string()),
            "提示里指的路是可发现的：{:?}",
            parent.client.image_capable_providers()
        );
    }

    /// `inherit_system: false` opts back into wholesale replacement; the subagent note is still
    /// appended, because it describes the runtime rather than the persona.
    #[test]
    fn inherit_system_false_replaces_parent_blocks() {
        let (session, _client) = parent_session();
        let mut d = def("reviewer");
        d.inherit_system = false;
        let tool = AgentTool::new(session, vec![d.clone()]);
        let sub = tool
            .build_sub_session(&params("审查"), Some(&d), "sub")
            .unwrap();
        let texts: Vec<&str> = sub.system.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts, ["你是评审。", SUBAGENT_NOTE]);
    }

    /// Channel etiquette rides in the system prompt, and only when channels are on.
    ///
    /// The placement is the point: it outlives compaction. That is not asserted here because it
    /// cannot fail — `compact::maybe_compact` takes `&Session`, so the borrow checker forbids it
    /// from touching `Session::system` at all; it splices `messages` and builds its summary
    /// request with `system: Vec::new()`. A test that re-stated that would prove nothing.
    #[test]
    fn channel_note_is_gated_by_the_flag() {
        let (off, _c1) = parent_session();
        assert!(!off.settings.experimental.agent_channels, "缺省关");
        let sub = build_sub_session(&off, None, None, None, None, "solo")
            .unwrap_or_else(|e| panic!("派生: {e}"));
        assert!(
            !sub.system.iter().any(|b| b.text == CHANNEL_NOTE),
            "频道关闭时不该塞频道礼仪"
        );

        let (mut on, _c2) = parent_session();
        let session = Arc::get_mut(&mut on).unwrap_or_else(|| panic!("独占"));
        session.settings.experimental.agent_channels = true;
        let sub = build_sub_session(&on, None, None, None, None, "member")
            .unwrap_or_else(|e| panic!("派生: {e}"));
        assert!(sub.system.iter().any(|b| b.text == CHANNEL_NOTE));
        assert!(
            CHANNEL_NOTE.contains("wakes them") && CHANNEL_NOTE.contains("acknowledge"),
            "规则要点名那个失败模式，别只说「保持简洁」"
        );
    }

    /// No named definition: the parent's system carries over, plus the note.
    #[test]
    fn plain_subagent_inherits_parent_system_plus_note() {
        let (session, _client) = parent_session();
        let sub = build_sub_session(&session, None, None, None, None, "worker").unwrap();
        let texts: Vec<&str> = sub.system.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts, ["父 system", SUBAGENT_NOTE]);
        assert!(
            !sub.system.last().map(|b| b.cache).unwrap_or(true),
            "说明块不占 cache breakpoint"
        );
    }

    /// MCP connections and the permission table are shared handles, not snapshots: a subagent
    /// sees the parent's MCP tools, and `/permissions` edits reach instances already running.
    #[test]
    fn sub_session_shares_parent_mcp_and_permissions() {
        let (parent, _) = parent_session();
        let sub = build_sub_session(&parent, None, None, None, None, "worker").unwrap();
        assert!(
            Arc::ptr_eq(&sub.runtime.mcp, &parent.runtime.mcp),
            "MCP 管理器应共享，否则子代理拿不到任何 MCP 工具"
        );
        assert!(
            Arc::ptr_eq(&sub.runtime.permissions, &parent.runtime.permissions),
            "权限表应共享，否则 spawn 后的 /permissions 改动传不到"
        );
    }

    /// A subagent's Ask decision is forwarded to the attached prompt surface, stamped with the
    /// instance name — never silently auto-denied (or auto-allowed under bypass).
    #[tokio::test]
    async fn subagent_ask_forwards_to_attached_prompt() {
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorder = seen.clone();
        let ask: Arc<crate::query::AskFn> = Arc::new(move |tool, reason| {
            recorder
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(format!("{tool}|{reason}"));
            Box::pin(async { true })
        });
        let watch = crate::watch::WatchRegistry::new();
        let id = register_run_watch(
            &watch,
            "l".into(),
            Arc::new(AgentCell::new()),
            Vec::new(),
            None,
        );
        let ui = subagent_hooks(
            Arc::new(Mutex::new(String::new())),
            Arc::new(AgentCell::new()),
            watch.clone(),
            id,
            "worker".into(),
            Some(ask),
        );
        assert!((ui.ask)("Write", "Write needs permission").await);
        assert_eq!(
            seen.lock().unwrap_or_else(|e| e.into_inner()).as_slice(),
            ["Write|worker · Write needs permission"]
        );

        // Nothing attached (embedded/test path): deny rather than block on a modal nobody shows.
        let ui = subagent_hooks(
            Arc::new(Mutex::new(String::new())),
            Arc::new(AgentCell::new()),
            watch,
            id,
            "worker".into(),
            None,
        );
        assert!(!(ui.ask)("Write", "Write needs permission").await);
    }
}
