use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use rsmarkdown_core::{MarkdownProcessor, Renderer};
use rsmarkdown_tui::activities::{
    activities_path_get_mut, diff_lines, Activity, ActivityKind, Diff, Thinking,
    ThinkingState, ToolCall, ToolStatus,
};
use rsmarkdown_tui::app::App;
use rsmarkdown_tui::Component;
use rsmarkdown_tui::permission::{DialogAction, PermissionRequest};
use rsmarkdown_tui::renderer::theme::Theme;
use rsmarkdown_tui::renderer::StreamMarkdownRenderer;
use rsmarkdown_tui::{FooterBadge, run_tui};
use tokio::sync::{mpsc, oneshot};

use crate::api::types::StreamEvent;
use crate::permission::PermissionMode;
use crate::query::{Session, ToolCallDone, UiHooks};

/// agent task → 组件的事件通道。
#[derive(Debug, Clone)]
pub enum UiEvent {
    TurnStart,
    /// 一批 toolcall 全部执行完（query loop 一轮收口）。
    RoundEnd,
    TextDelta(String),
    ThinkingDelta(String),
    /// message_delta 的输出 token 累计值。
    OutputTokens(u64),
    ToolStart { name: String },
    /// 工具 block 流式接收完整（含 input）：折叠判定时机。
    ToolReady { name: String, input: serde_json::Value },
    ToolDone(ToolCallDone),
    /// Watchable 状态事件（命令/agent 生命周期，转发自 registry）。
    WatchEvent {
        label: String,
        status: rsmarkdown_tui::activities::WatchStatus,
        detail: Option<String>,
        duration_ms: u64,
        payload: Option<serde_json::Value>,
    },
    TurnEnd,
    /// 非致命警告（如 MCP 连接失败），显示在边框与分隔线之间。
    Warning(String),
    Error(String),
}

/// 权限询问：请求 + 结果回执。
pub type AskRequest = (PermissionRequest, oneshot::Sender<DialogAction>);

/// 把 query 的 UiHooks 接到 TUI 通道上。
pub fn tui_hooks(
    events: mpsc::UnboundedSender<UiEvent>,
    asks: mpsc::UnboundedSender<AskRequest>,
) -> UiHooks {
    let tool_events = events.clone();
    let ready_events = events.clone();
    let round_events = events.clone();
    let warn_events = events.clone();
    UiHooks {
        on_event: Box::new(move |event| match event {
            StreamEvent::TextDelta { text, .. } => {
                let _ = events.send(UiEvent::TextDelta(text.clone()));
            }
            StreamEvent::ThinkingDelta { thinking, .. } => {
                let _ = events.send(UiEvent::ThinkingDelta(thinking.clone()));
            }
            StreamEvent::ToolUseStart { name, .. } => {
                let _ = events.send(UiEvent::ToolStart { name: name.clone() });
            }
            StreamEvent::StopReason { output_tokens: Some(tokens), .. } => {
                let _ = events.send(UiEvent::OutputTokens(*tokens));
            }
            _ => {}
        }),
        on_tool_ready: Box::new(move |name, input| {
            let _ = ready_events.send(UiEvent::ToolReady { name, input });
        }),
        on_tool_done: Box::new(move |done| {
            let _ = tool_events.send(UiEvent::ToolDone(crate::query::ToolCallDone {
                name: done.name.clone(),
                summary: done.summary.clone(),
                output: done.output.clone(),
                is_error: done.is_error,
                diff: done.diff.clone(),
                duration_ms: done.duration_ms,
            }));
        }),
        on_round_end: Box::new(move || {
            let _ = round_events.send(UiEvent::RoundEnd);
        }),
        on_warning: Box::new(move |message| {
            let _ = warn_events.send(UiEvent::Warning(message));
        }),
        ask: Box::new(move |tool_name, reason| {
            let request = PermissionRequest::new(
                format!("允许执行 {tool_name}"),
                reason,
                vec!["允许".to_string(), "拒绝".to_string()],
            );
            let (tx, rx) = oneshot::channel();
            if asks.send((request, tx)).is_err() {
                return Box::pin(async { false });
            }
            Box::pin(async move {
                matches!(rx.await, Ok(DialogAction::Confirm(0)))
            })
        }),
    }
}

/// 一条会话消息（用户或 assistant 文本 + assistant 活动提示）。
struct UiMessage {
    role: Role,
    text: String,
    activities: Vec<Activity>,
    /// activities[i] 创建时 text 的字符数：渲染时 text 与活动按模型输出顺序交错。
    insert_points: Vec<usize>,
    /// 连续 Read/Search 折叠组（Claude Code collapseReadSearchGroups）。
    groups: Vec<CollapseGroup>,
    /// activities[i] 所属折叠组索引（None = 独立活动）。
    group_of: Vec<Option<usize>>,
}

/// Read/Search 连续操作的折叠组：折叠为一行规则摘要（`Read 3 files`）。
struct CollapseGroup {
    /// 组内活动索引（顺序）。
    activities: Vec<usize>,
    /// 搜索操作数（Grep/Glob/搜索类 Bash）。
    search: usize,
    /// Read 文件路径（去重计数，Claude Code 用 Set.size）。
    read_paths: Vec<String>,
    /// 无路径的读取操作数（如 Bash cat 管道）。
    read_ops: usize,
    /// 列举操作数（ls/tree/du）。
    list: usize,
    /// 普通 Bash 操作数（CC fullscreen bash 类）。
    bash: usize,
    /// 组仍开放（进行中 → 摘要用进行时 + …）。
    active: bool,
    /// ctrl+o / 点击展开组内逐工具。
    expanded: bool,
    /// 组内最近一个工具的输入 hint（CC latestDisplayHint，执行中显示在 ⎿ 行）。
    last_hint: Option<String>,
}

/// 工具的可折叠分类（Claude Code isSearchOrReadCommand）。
#[derive(Clone, PartialEq, Eq, Debug)]
enum CollapseKind {
    Search,
    /// Read 或读取类 Bash：携带文件路径（Bash 类为 None）。
    Read(Option<String>),
    List,
    /// 非搜索/读/列举的普通 Bash（CC fullscreen isBash 类）。
    Bash,
}

/// 折叠组行的点击动作。携带消息索引：点击旧消息的行时目标必须是
/// 那一条消息，而不是最后一条。
#[derive(Clone, Copy)]
enum ClickAction {
    Group { message: usize, group: usize },
}

/// Read/Search 类工具判定（Claude Code isSearchOrReadCommand 的最小面）。
/// Bash 走命令分类；Read/Grep/Glob 固定可折叠。
fn classify_tool(name: &str, input: &serde_json::Value) -> Option<CollapseKind> {
    match name {
        "Read" => input
            .get("file_path")
            .and_then(|p| p.as_str())
            .map(|p| CollapseKind::Read(Some(p.to_string()))),
        "Grep" | "Glob" => Some(CollapseKind::Search),
        "Bash" => {
            let kind = input
                .get("command")
                .and_then(|c| c.as_str())
                .and_then(classify_bash_command);
            if kind.is_some() {
                kind
            } else if input
                .get("command")
                .and_then(|c| c.as_str())
                .is_some_and(bash_has_work)
            {
                // 非搜索/读/列举的普通命令：折叠为 bash 类（CC fullscreen）。
                Some(CollapseKind::Bash)
            } else {
                // 纯中性命令（echo hi）不折叠。
                None
            }
        }
        _ => None,
    }
}

/// 命令是否含非中性段（CC hasNonNeutralCommand）：纯 echo/printf 等不折叠。
fn bash_has_work(command: &str) -> bool {
    const NEUTRAL: &[&str] = &["echo", "printf", "true", "false", ":"];
    let mut skip_next = false;
    for part in command.split(['&', '|', ';']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if skip_next {
            skip_next = false;
            continue;
        }
        if part.starts_with('>') {
            skip_next = true;
            continue;
        }
        let base = part.split_whitespace().next().unwrap_or("");
        if !NEUTRAL.contains(&base) {
            return true;
        }
    }
    false
}

/// Bash 命令分类（Claude Code isSearchOrReadBashCommand 简化版）：
/// 按 && / || / | / ; 分段，跳过量词/重定向目标与语义中性命令，
/// 所有段都必须属于搜索/读取/列举集合；混合时按 list > search > read 归位。
fn classify_bash_command(command: &str) -> Option<CollapseKind> {
    const SEARCH: &[&str] = &[
        "find", "grep", "rg", "ag", "ack", "locate", "which", "whereis",
    ];
    const READ: &[&str] = &[
        "cat", "head", "tail", "less", "more", "wc", "stat", "file", "strings",
        "jq", "awk", "cut", "sort", "uniq", "tr",
    ];
    const LIST: &[&str] = &["ls", "tree", "du"];
    const NEUTRAL: &[&str] = &["echo", "printf", "true", "false", ":"];
    let mut seen = false;
    let mut list = false;
    let mut search = false;
    let mut read = false;
    let mut skip_next = false;
    for part in command.split(['&', '|', ';']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if skip_next {
            skip_next = false;
            continue;
        }
        if part.starts_with('>') {
            skip_next = true;
            continue;
        }
        let base = part.split_whitespace().next().unwrap_or("");
        if NEUTRAL.contains(&base) {
            continue;
        }
        seen = true;
        if LIST.contains(&base) {
            list = true;
        } else if SEARCH.contains(&base) {
            search = true;
        } else if READ.contains(&base) {
            read = true;
        } else {
            return None;
        }
    }
    if !seen {
        return None;
    }
    if list {
        Some(CollapseKind::List)
    } else if search {
        Some(CollapseKind::Search)
    } else if read {
        Some(CollapseKind::Read(None))
    } else {
        None
    }
}

/// 折叠组执行中的 hint（CC latestDisplayHint）：组内最近工具的输入。
/// Read → 裸路径、Grep/Glob → "pattern"、Bash → $ cmd。
fn hint_for(name: &str, input: &serde_json::Value) -> String {
    let map = input.as_object();
    match name {
        "Bash" => map
            .and_then(|m| m.get("command"))
            .and_then(|c| c.as_str())
            .map(|c| format!("$ {c}"))
            .unwrap_or_else(|| crate::query::summarize_input(name, input)),
        "Read" => map
            .and_then(|m| m.get("file_path"))
            .and_then(|p| p.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| crate::query::summarize_input(name, input)),
        "Grep" | "Glob" => map
            .and_then(|m| m.get("pattern"))
            .and_then(|p| p.as_str())
            .map(|p| format!("\"{p}\""))
            .unwrap_or_else(|| crate::query::summarize_input(name, input)),
        _ => crate::query::summarize_input(name, input),
    }
}

/// 折叠组摘要文本（Claude Code getSearchReadSummaryText 的对应物）：
/// `Searched for 2 patterns, read 3 files`；进行中（组未关闭且还有 Running
/// 工具）用进行时 + 末尾 …。
fn collapse_summary(g: &CollapseGroup, in_progress: bool) -> String {
    let active = in_progress;
    let mut parts: Vec<String> = Vec::new();
    let mut push = |verb_done: &str, verb_ing: &str, body: String| {
        if parts.is_empty() {
            let v = if active { verb_ing } else { verb_done };
            parts.push(format!("{}{body}", capitalize(v)));
        } else {
            let v = if active { verb_ing } else { verb_done };
            parts.push(format!("{v}{body}"));
        }
    };
    if g.search > 0 {
        push(
            "searched for",
            "searching for",
            format!(" {} {}", g.search, if g.search == 1 { "pattern" } else { "patterns" }),
        );
    }
    let read_count = if g.read_paths.is_empty() {
        g.read_ops
    } else {
        g.read_paths.iter().collect::<std::collections::HashSet<_>>().len()
    };
    if read_count > 0 {
        push(
            "read",
            "reading",
            format!(" {} {}", read_count, if read_count == 1 { "file" } else { "files" }),
        );
    }
    if g.list > 0 {
        push(
            "listed",
            "listing",
            format!(" {} {}", g.list, if g.list == 1 { "directory" } else { "directories" }),
        );
    }
    if g.bash > 0 {
        push(
            "ran",
            "running",
            format!(" {} bash {}", g.bash, if g.bash == 1 { "command" } else { "commands" }),
        );
    }
    let text = parts.join(", ");
    if active {
        format!("{text}…")
    } else {
        text
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// 展开态 1 行结果摘要（Claude Code renderToolResultMessage 的对应物）。
fn result_summary(name: &str, output: &str) -> Option<String> {
    let lines = output.lines().filter(|l| !l.trim().is_empty()).count();
    match name {
        "Read" => Some(format!("Read {lines} lines")),
        "Grep" => Some(format!(
            "Found {} {}",
            lines,
            if lines == 1 { "match" } else { "matches" }
        )),
        "Glob" => Some(format!(
            "Found {} {}",
            lines,
            if lines == 1 { "file" } else { "files" }
        )),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    User,
    Assistant,
}

/// bingo 的聊天组件：消息流 + 活动提示 + 输入框 + 权限模态。
pub struct BingoChat {
    session: Arc<Session>,
    pub(super) events: mpsc::UnboundedSender<UiEvent>,
    asks: mpsc::UnboundedSender<AskRequest>,
    events_rx: mpsc::UnboundedReceiver<UiEvent>,
    asks_rx: mpsc::UnboundedReceiver<AskRequest>,
    messages: Vec<UiMessage>,
    input: String,
    typing: bool,
    busy: bool,
    /// 当前 assistant 消息索引。
    stream_msg: Option<usize>,
    thinking_buf: String,
    output_tokens: u64,
    tick: u64,
    /// TurnStart 时的 tick：运行态 thinking 的相对计时基准。
    turn_start_tick: u64,
    warnings: Vec<String>,
    user: String,
    cwd: String,
    pending_ask: Option<(PermissionRequest, oneshot::Sender<DialogAction>)>,
    processor: MarkdownProcessor,
    renderer: StreamMarkdownRenderer,
    reply_cache: HashMap<String, Vec<Line<'static>>>,
    width: usize,
    scroll: u16,
    auto_scroll: bool,
    /// 消息区顶（组件局部坐标）：点击行 → doc 行的偏移。
    msg_top: u16,
    /// 上次 draw 的可展开活动行范围（doc 坐标），供鼠标点击折叠/展开。
    activity_ranges: Vec<rsmarkdown_tui::activities::ActivityRowRange>,
    /// 折叠组行的点击范围（doc 坐标）：这些行不属于单个 Activity。
    click_ranges: Vec<(u16, u16, ClickAction)>,
    /// 等待 ToolReady（完整 input）归类的工具活动索引（FIFO）。
    pending_tools: Vec<usize>,
    theme: Theme,
    /// 中断信号：busy 时 Ctrl+C / Esc → send(true)，回合内流读取立即中止。
    cancel_tx: tokio::sync::watch::Sender<bool>,
}

impl BingoChat {
    pub fn new(
        session: Arc<Session>,
        events: mpsc::UnboundedSender<UiEvent>,
        events_rx: mpsc::UnboundedReceiver<UiEvent>,
        asks: mpsc::UnboundedSender<AskRequest>,
        asks_rx: mpsc::UnboundedReceiver<AskRequest>,
    ) -> Self {
        // Watchable 事件转发：registry 广播 → UiEvent 通道（跨回合常驻）。
        // 测试环境无 tokio runtime 时跳过（转发只在线程模型下有意义）。
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let watch_events = events.clone();
            let mut rx = session.watch.subscribe();
            handle.spawn(async move {
            loop {
                // Lagged：消费者落后丢事件——重同步继续，不退出转发。
                let ev = match rx.recv().await {
                    Ok(ev) => ev,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                if watch_events
                    .send(UiEvent::WatchEvent {
                        label: ev.label,
                        status: match ev.state {
                            crate::watch::WatchState::Running => {
                                rsmarkdown_tui::activities::WatchStatus::Running
                            }
                            crate::watch::WatchState::Idle => {
                                rsmarkdown_tui::activities::WatchStatus::Idle
                            }
                            crate::watch::WatchState::Done => {
                                rsmarkdown_tui::activities::WatchStatus::Done
                            }
                            crate::watch::WatchState::Failed => {
                                rsmarkdown_tui::activities::WatchStatus::Failed
                            }
                            crate::watch::WatchState::Cancelled => {
                                rsmarkdown_tui::activities::WatchStatus::Cancelled
                            }
                        },
                        detail: ev.detail,
                        duration_ms: ev.elapsed_ms,
                        payload: ev.payload,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        }
        Self {
            session,
            events,
            asks,
            events_rx,
            asks_rx,
            messages: Vec::new(),
            input: String::new(),
            typing: true,
            busy: false,
            stream_msg: None,
            thinking_buf: String::new(),
            output_tokens: 0,
            tick: 0,
            turn_start_tick: 0,
            warnings: Vec::new(),
            user: std::env::var("USER").unwrap_or_else(|_| "user".to_string()),
            cwd: std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            pending_ask: None,
            processor: MarkdownProcessor::default(),
            renderer: StreamMarkdownRenderer::new(80),
            reply_cache: HashMap::new(),
            width: 80,
            scroll: 0,
            auto_scroll: true,
            msg_top: 0,
            activity_ranges: Vec::new(),
            click_ranges: Vec::new(),
            pending_tools: Vec::new(),
            theme: Theme::dark(),
            cancel_tx: tokio::sync::watch::channel(false).0,
        }
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.events_rx.try_recv() {
            match event {
                UiEvent::TurnStart => {
                    self.thinking_buf.clear();
                    self.pending_tools.clear();
                    self.messages.push(UiMessage {
                        role: Role::Assistant,
                        text: String::new(),
                        activities: Vec::new(),
                        insert_points: Vec::new(),
                        groups: Vec::new(),
                        group_of: Vec::new(),
                    });
                    self.stream_msg = Some(self.messages.len() - 1);
                    self.busy = true;
                    self.turn_start_tick = self.tick;
                    // 占位 thinking：端点延迟推送 delta 时（DeepSeek 常达数十秒），
                    // 运行态行立即可见，用户能感知"正在思考"。
                    let mut hint = Activity::new(ActivityKind::Thinking(Thinking {
                        state: ThinkingState::Running,
                        duration_ms: 0,
                        digest: None,
                        stage: thinking_stage(self.messages.len()),
                        tokens: None,
                        start_tick: self.tick,
                    }));
                    hint.expand_hint = Some("ctrl+o to expand".to_string());
                    if let Some(i) = self.stream_msg {
                        self.messages[i].activities.push(hint);
                        self.messages[i].insert_points.push(0);
                        self.messages[i].group_of.push(None);
                    }
                }
                UiEvent::TextDelta(text) => {
                    if let Some(i) = self.stream_msg
                        && !text.is_empty()
                    {
                        // assistant 正文打断连续 Read/Search 组（CC isTextBreaker）。
                        self.messages[i].text.push_str(&text);
                        if let Some(g) = self.messages[i].groups.last_mut() {
                            g.active = false;
                        }
                    }
                }
                UiEvent::ThinkingDelta(thinking) => {
                    if let Some(i) = self.stream_msg {
                        // 多轮 thinking 各自成块：末尾是运行态 thinking 则续写它，
                        // 否则（末尾是工具行或已完成的旧块）开新块。
                        let last_is_running_thinking = self.messages[i]
                            .activities
                            .last()
                            .is_some_and(|a| {
                                matches!(&a.kind, ActivityKind::Thinking(t)
                                    if t.state == ThinkingState::Running)
                            });
                        if last_is_running_thinking {
                            self.thinking_buf.push_str(&thinking);
                            let buf = self.thinking_buf.clone();
                            let content = self.render_thinking(&buf);
                            if let Some(hint) = self.messages[i]
                                .activities
                                .iter_mut()
                                .rev()
                                .find(|a| matches!(a.kind, ActivityKind::Thinking(_)))
                            {
                                hint.set_content(content);
                            }
                        } else {
                            // 工具轮后的新一段思考。DeepSeek 兼容层偶发把
                            // 同一段 thinking 在 tool_use 前后各发一遍：
                            // 内容与上一轮相同则视为重复，不新开块。
                            let dup = thinking == self.thinking_buf
                                || self.messages[i]
                                    .activities
                                    .iter()
                                    .rev()
                                    .find(|a| matches!(a.kind, ActivityKind::Thinking(_)))
                                    .is_some_and(|a| {
                                        a.content.first().is_some_and(|l| l.to_string() == thinking)
                                    });
                            if dup {
                                continue;
                            }
                            // 清掉从未收到 delta 的空占位，然后新开一块（排在工具行之后）。
                            self.thinking_buf = thinking.clone();
                            self.messages[i].activities.retain(|a| {
                                !(matches!(a.kind, ActivityKind::Thinking(_))
                                    && a.content.is_empty())
                            });
                            let buf = self.thinking_buf.clone();
                            let content = self.render_thinking(&buf);
                            let mut hint = Activity::new(ActivityKind::Thinking(Thinking {
                                state: ThinkingState::Running,
                                duration_ms: self.tick.saturating_sub(self.turn_start_tick) * 33,
                                digest: None,
                                stage: thinking_stage(self.messages.len()),
                                tokens: None,
                                start_tick: self.tick,
                            }));
                            hint.set_content(content);
                            hint.expand_hint = Some("ctrl+o to expand".to_string());
                            self.messages[i].activities.push(hint);
                            let text_len = self.messages[i].text.chars().count();
                            self.messages[i].insert_points.push(text_len);
                            self.messages[i].group_of.push(None);
                        }
                    }
                }
                UiEvent::OutputTokens(_tokens) => {}
                UiEvent::ToolStart { name } => {
                    // 模型转向工具调用：正在进行的 thinking 段到此结束，
                    // 块转完成态（新段到达时再开新块）。
                    if let Some(i) = self.stream_msg {
                        for hint in &mut self.messages[i].activities {
                            if let ActivityKind::Thinking(t) = &mut hint.kind
                                && t.state == ThinkingState::Running
                            {
                                t.state = ThinkingState::Done;
                                t.duration_ms = self
                                    .tick
                                    .saturating_sub(t.start_tick)
                                    .saturating_mul(33);
                            }
                        }
                    }
                    let name: &'static str = Box::leak(name.into_boxed_str());
                    let mut hint = Activity::new(ActivityKind::Tool(ToolCall::running(
                        name, "",
                    )));
                    hint.expand_hint = Some("ctrl+o to expand".to_string());
                    if let Some(i) = self.stream_msg {
                        let idx = self.messages[i].activities.len();
                        let text_len = self.messages[i].text.chars().count();
                        self.messages[i].activities.push(hint);
                        self.messages[i].insert_points.push(text_len);
                        self.messages[i].group_of.push(None);
                        self.pending_tools.push(idx);
                    }
                }
                // 工具 block 完整（含 input）：判定 Read/Search 折叠归组。
                UiEvent::ToolReady { name, input } => {
                    let Some(i) = self.stream_msg else { return };
                    let Some(idx) = self.pending_tools.first().copied() else {
                        return;
                    };
                    self.pending_tools.remove(0);
                    // 执行中的 header 就带输入摘要（CC：`⏺ Agent description="…"`）。
                    if let ActivityKind::Tool(call) = &mut self.messages[i].activities[idx].kind {
                        call.summary = crate::query::summarize_input(&name, &input);
                    }
                    let kind = classify_tool(&name, &input);
                    let Some(kind) = kind else {
                        // 非 Read/Search 工具：打断进行中的折叠组。
                        if let Some(g) = self.messages[i].groups.last_mut() {
                            g.active = false;
                        }
                        return;
                    };
                    // 入组：末尾有开放组则续组，否则开新组。
                    let open = self.messages[i]
                        .groups
                        .last()
                        .is_some_and(|g| g.active && !g.activities.is_empty());
                    let g = if open {
                        self.messages[i].groups.len() - 1
                    } else {
                        self.messages[i].groups.push(CollapseGroup {
                            activities: Vec::new(),
                            search: 0,
                            read_paths: Vec::new(),
                            read_ops: 0,
                            list: 0,
                            bash: 0,
                            active: true,
                            expanded: false,
                            last_hint: None,
                        });
                        self.messages[i].groups.len() - 1
                    };
                    self.messages[i].group_of[idx] = Some(g);
                    self.messages[i].groups[g].activities.push(idx);
                    self.messages[i].groups[g].last_hint = Some(hint_for(&name, &input));
                    match kind {
                        CollapseKind::Search => self.messages[i].groups[g].search += 1,
                        CollapseKind::Read(path) => match path {
                            Some(p) => self.messages[i].groups[g].read_paths.push(p),
                            None => self.messages[i].groups[g].read_ops += 1,
                        },
                        CollapseKind::List => self.messages[i].groups[g].list += 1,
                        CollapseKind::Bash => self.messages[i].groups[g].bash += 1,
                    }
                }
                UiEvent::WatchEvent {
                    label,
                    status,
                    detail,
                    duration_ms,
                    payload,
                } => {
                    let Some(i) = self.stream_msg else { return };
                    // 同 label 的 watch 活动原地更新（终态后保留，快照定格）。
                    let found = self.messages[i].activities.iter_mut().find(|a| {
                        matches!(&a.kind, rsmarkdown_tui::activities::ActivityKind::Watch(w)
                            if w.label == *label)
                    });
                    if let Some(hint) = found {
                        if let rsmarkdown_tui::activities::ActivityKind::Watch(w) = &mut hint.kind
                        {
                            w.status = status;
                            w.duration_ms = duration_ms;
                            if let Some(d) = &detail {
                                w.detail = Some(d.clone());
                            }
                        }
                        if let Some(text) = &payload.and_then(|p| p.as_str().map(str::to_string)) {
                            let content: Vec<Line<'static>> = text
                                .lines()
                                .filter(|l| !l.trim().is_empty())
                                .map(|l| Line::from(l.to_string()))
                                .collect();
                            hint.set_content(content);
                        }
                    } else {
                        let mut hint = Activity::new(rsmarkdown_tui::activities::ActivityKind::Watch(
                            rsmarkdown_tui::activities::WatchCall {
                                label: label.clone(),
                                status,
                                detail: detail.clone(),
                                duration_ms,
                            },
                        ));
                        hint.expand_hint = Some("ctrl+o to expand".to_string());
                        let text_len = self.messages[i].text.chars().count();
                        self.messages[i].activities.push(hint);
                        self.messages[i].insert_points.push(text_len);
                        self.messages[i].group_of.push(None);
                    }
                }
                UiEvent::RoundEnd => {
                    // 一批 toolcall 收口：清 active，下一轮模型响应的 Read/Search 开新组
                    // （跨轮聚合曾导致"上一轮没找到 → 思考 → 下一轮工具挂进旧组"）。
                    if let Some(i) = self.stream_msg
                        && let Some(g) = self.messages[i].groups.last_mut()
                    {
                        g.active = false;
                    }
                }
                UiEvent::ToolDone(done) => {
                    let Some(i) = self.stream_msg else {
                        return;
                    };
                    // 编辑类工具：Running 工具行 → 原位替换为 unified diff 活动。
                    if let Some(diff_text) = &done.diff
                        && let Some(pos) = self.messages[i].activities.iter().position(|h| {
                            matches!(&h.kind, ActivityKind::Tool(c)
                                if c.name == done.name.as_str() && c.status == ToolStatus::Running)
                        })
                    {
                        let diff = Diff::parse_unified(diff_text);
                        let content = diff_lines(&diff, &self.theme);
                        let mut hint = Activity::new(ActivityKind::Diff(diff));
                        hint.expand_hint = Some("ctrl+o to expand".to_string());
                        hint.set_content(content);
                        self.messages[i].activities[pos] = hint;
                        continue;
                    }
                    let group_of = self.messages[i].group_of.clone();
                    for (hint_idx, hint) in self.messages[i].activities.iter_mut().enumerate()
                    {
                        if let ActivityKind::Tool(call) = &mut hint.kind
                            && call.name == done.name.as_str()
                            && call.status == ToolStatus::Running
                        {
                            call.status = if done.is_error {
                                ToolStatus::Error
                            } else {
                                ToolStatus::Done
                            };
                            call.summary = done.summary.clone();
                            call.duration_ms = done.duration_ms;
                            let in_group = group_of
                                .get(hint_idx)
                                .copied()
                                .flatten()
                                .is_some();
                            if in_group {
                                // 组内工具：展开态只显示 1 行结果摘要（CC verbose）。
                                call.result_summary = result_summary(&done.name, &done.output);
                            } else {
                                // 独立工具：展开显示全部输出（真实展开）。
                                let content: Vec<Line<'static>> = done
                                    .output
                                    .lines()
                                    .filter(|l| !l.trim().is_empty())
                                    .map(|l| Line::from(l.to_string()))
                                    .collect();
                                hint.set_content(content);
                            }
                            // 只更新第一个匹配的 Running 工具（并行同名工具按序消费）。
                            break;
                        }
                    }
                }
                UiEvent::TurnEnd => {
                    self.busy = false;
                    self.output_tokens = 0;
                    // 原位收尾：thinking 在它发生的位置转完成态（不重排到回复之后）；
                    // 从未收到 delta 的空占位直接移除（避免出现无内容的空行）。
                    if let Some(i) = self.stream_msg {
                        if let Some(g) = self.messages[i].groups.last_mut() {
                            g.active = false;
                        }
                        // 同步移除：空占位 thinking 与它的插入点。
                        let mut keep = Vec::new();
                        for (idx, a) in self.messages[i].activities.iter().enumerate() {
                            if matches!(a.kind, ActivityKind::Thinking(_)) && a.content.is_empty() {
                                continue;
                            }
                            keep.push(idx);
                        }
                        if keep.len() != self.messages[i].activities.len() {
                            let old_to_new: std::collections::HashMap<usize, usize> = keep
                                .iter()
                                .enumerate()
                                .map(|(new, old)| (*old, new))
                                .collect();
                            for g in &mut self.messages[i].groups {
                                g.activities = g
                                    .activities
                                    .iter()
                                    .filter_map(|a| old_to_new.get(a).copied())
                                    .collect();
                            }
                            self.messages[i].activities =
                                keep.iter().map(|&k| self.messages[i].activities[k].clone()).collect();
                            self.messages[i].insert_points = keep
                                .iter()
                                .map(|&k| self.messages[i].insert_points[k])
                                .collect();
                            self.messages[i].group_of = keep
                                .iter()
                                .map(|&k| self.messages[i].group_of[k])
                                .collect();
                        }
                        for hint in &mut self.messages[i].activities {
                            if let ActivityKind::Thinking(t) = &mut hint.kind
                                && t.state == ThinkingState::Running
                            {
                                t.state = ThinkingState::Done;
                                t.duration_ms = self
                                    .tick
                                    .saturating_sub(t.start_tick)
                                    .saturating_mul(33);
                                hint.expanded = false;
                            }
                        }
                    }
                    self.stream_msg = None;
                }
                UiEvent::Warning(message) => {
                    if !self.warnings.iter().any(|w| w == &message) {
                        self.warnings.push(message);
                    }
                }
                UiEvent::Error(message) => {
                    self.busy = false;
                    self.stream_msg = None;
                    if let Some(msg) = self.messages.pop() {
                        self.messages.push(UiMessage {
                            role: Role::Assistant,
                            text: format!("[error] {message}"),
                            activities: msg.activities,
                            insert_points: msg.insert_points,
                            groups: msg.groups,
                            group_of: msg.group_of,
                        });
                    }
                }
            }
        }
    }

    fn drain_asks(&mut self) {
        if self.pending_ask.is_none()
            && let Ok(request) = self.asks_rx.try_recv()
        {
            self.pending_ask = Some(request);
        }
    }

    /// thinking 内容走 markdown streaming 渲染（代码块/列表随流更新，
    /// 换行按渲染结果自然成行）。每次以完整文本重渲染（thinking 增量不大）。
    fn render_thinking(&mut self, text: &str) -> Vec<Line<'static>> {
        if text.is_empty() {
            return Vec::new();
        }
        self.renderer.set_width(self.width);
        let doc = self.processor.process_streaming(text);
        self.renderer.render(&doc);
        self.renderer.lines().to_vec()
    }

    /// 点击（doc 坐标）命中的行 → 折叠/展开（鼠标点击）。
    /// 折叠组行、组展开态组首工具行优先（点击折叠组）；其次活动行。
    fn toggle_at(&mut self, doc_row: u16) -> bool {
        if let Some((_, _, action)) = self
            .click_ranges
            .iter()
            .find(|(start, end, _)| doc_row >= *start && doc_row < *end)
            && let ClickAction::Group { message, group } = action
            && let Some(msg) = self.messages.get_mut(*message)
        {
            msg.groups[*group].expanded = !msg.groups[*group].expanded;
            self.auto_scroll = false;
            return true;
        }
        if let Some(range) = self
            .activity_ranges
            .iter()
            .find(|r| doc_row >= r.start && doc_row < r.end)
        {
            let path = range.path.clone();
            let Some(msg) = self.messages.get_mut(range.message) else {
                return false;
            };
            if let Some(act) = activities_path_get_mut(&mut msg.activities, &path) {
                act.toggle();
                self.auto_scroll = false;
                return true;
            }
            return false;
        }
        false
    }

    /// ctrl+o：全局展开/折叠 transcript（Claude Code app:toggleTranscript）。
    /// 优先级：展开的组先折叠回聚合态（点击展开组后 ctrl+o 必须能回去）；
    /// 否则有折叠项 → 全部展开；否则全部折叠。
    fn toggle_transcript(&mut self) -> bool {
        let Some(i) = self.messages.len().checked_sub(1) else {
            return false;
        };
        if self.messages[i].groups.iter().any(|g| g.expanded) {
            for g in &mut self.messages[i].groups {
                g.expanded = false;
            }
            self.auto_scroll = false;
            return true;
        }
        let any_collapsed = self.messages[i]
            .activities
            .iter()
            .any(|a| !a.expanded && a.expandable())
            || self.messages[i]
                .groups
                .iter()
                .any(|g| !g.expanded && !g.activities.is_empty());
        for act in &mut self.messages[i].activities {
            act.expanded = any_collapsed;
        }
        for g in &mut self.messages[i].groups {
            g.expanded = any_collapsed;
        }
        self.auto_scroll = false;
        true
    }

    fn submit(&mut self) {
        let text = std::mem::take(&mut self.input);
        if text.trim().is_empty() || self.busy {
            self.input = text;
            return;
        }
        self.messages.push(UiMessage {
            role: Role::User,
            text: text.clone(),
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        self.busy = true;
        // 新一轮开始前复位中断信号（同一 Sender 跨轮复用）。
        let _ = self.cancel_tx.send(false);

        let session = self.session.clone();
        let events = self.events.clone();
        let asks = self.asks.clone();
        let cancel_rx = self.cancel_tx.subscribe();
        tokio::spawn(async move {
            let _ = events.send(UiEvent::TurnStart);
            let mut ui = tui_hooks(events.clone(), asks);
            // 多轮连续性：加载 transcript 历史作为本轮上下文（每轮独立 run_query）。
            // 新 session 的文件尚未创建（首次 append 才落盘）→ NotFound 视为空历史。
            let history = match session.transcript.as_ref() {
                Some(t) => match t.load_messages() {
                    Ok(msgs) => msgs,
                    Err(crate::transcript::TranscriptError::Io(e))
                        if e.kind() == std::io::ErrorKind::NotFound =>
                    {
                        Vec::new()
                    }
                    Err(e) => {
                        (ui.on_warning)(format!("transcript load failed: {e}"));
                        Vec::new()
                    }
                },
                None => Vec::new(),
            };
            let result =
                crate::query::run_query(&session, history, &text, &mut ui, Some(cancel_rx))
                    .await;
            match result {
                Ok(outcome) => {
                    if outcome.aborted {
                        let _ = events.send(UiEvent::Warning("回合已中断".to_string()));
                    }
                    let cwd = std::env::current_dir().unwrap_or_default();
                    crate::memory::extract_memory(&session, &outcome.messages, &session.home, &cwd)
                        .await;
                    let _ = events.send(UiEvent::TurnEnd);
                }
                Err(e) => {
                    let _ = events.send(UiEvent::Error(e.to_string()));
                }
            }
        });
    }

}

fn center(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len >= width {
        return text.to_string();
    }
    let pad = (width - len) / 2;
    format!("{}{}", " ".repeat(pad), text)
}

fn column_row(
    theme: &Theme,
    left_w: usize,
    right_w: usize,
    left: Option<(String, ratatui::style::Color)>,
    right: Option<(String, ratatui::style::Color)>,
) -> Line<'static> {
    let mut spans = Vec::new();
    let (l_text, l_color) = left.unwrap_or_else(|| (String::new(), theme.dim().fg.unwrap_or(theme.text)));
    let l_len = l_text.chars().count();
    spans.push(Span::styled(l_text, ratatui::style::Style::default().fg(l_color)));
    spans.push(Span::styled(
        format!("{}│", " ".repeat(left_w.saturating_sub(l_len))),
        theme.dim(),
    ));
    let mut r_len = 0;
    if let Some((r_text, r_color)) = right {
        r_len = r_text.chars().count();
        spans.push(Span::styled(r_text, ratatui::style::Style::default().fg(r_color)));
    }
    spans.push(Span::styled(
        " ".repeat(right_w.saturating_sub(r_len)),
        theme.dim(),
    ));
    Line::from(spans)
}

/// 欢迎面板（启动横幅，1:1 对齐 Claude Code）：左栏 logo/欢迎/身份，
/// 右栏 Tips 与 What's new。
fn welcome_rows(
    theme: &Theme,
    user: &str,
    model: &str,
    mode: &str,
    cwd: &str,
    width: usize,
) -> Vec<Line<'static>> {
    let left_w = width * 3 / 5;
    let right_w = width.saturating_sub(left_w + 1);
    let accent = theme.tool_running;
    let mut rows = Vec::new();

    let logo = ["    ▐▛█▜▌", "   ▝▜███▛▘", "     ▘ ▘"];
    for line in logo {
        rows.push(column_row(
            theme,
            left_w,
            right_w,
            Some((center(line, left_w), theme.text)),
            None,
        ));
    }
    rows.push(column_row(theme, left_w, right_w, None, None));
    rows.push(column_row(
        theme,
        left_w,
        right_w,
        Some((center(&format!("Welcome back {user}!"), left_w), theme.text)),
        Some(("Tips for getting started".to_string(), accent)),
    ));
    rows.push(column_row(theme, left_w, right_w, None, None));
    rows.push(column_row(
        theme,
        left_w,
        right_w,
        Some((center(&format!("{model} · {mode}"), left_w), theme.dim().fg.unwrap_or(theme.text))),
        Some(("Enter 发送 · Esc 切换输入".to_string(), theme.text)),
    ));
    rows.push(column_row(
        theme,
        left_w,
        right_w,
        Some((center(user, left_w), theme.text)),
        Some(("ctrl+o 展开/折叠工具输出".to_string(), theme.text)),
    ));
    rows.push(column_row(
        theme,
        left_w,
        right_w,
        Some((center(cwd, left_w), theme.dim().fg.unwrap_or(theme.text))),
        Some(("MCP 服务配置在 settings.json".to_string(), theme.text)),
    ));
    rows.push(column_row(theme, left_w, right_w, None, Some(("─".repeat(right_w).to_string(), theme.dim().fg.unwrap_or(theme.text)))));
    rows.push(column_row(
        theme,
        left_w,
        right_w,
        None,
        Some(("What's new".to_string(), accent)),
    ));
    rows.push(column_row(
        theme,
        left_w,
        right_w,
        None,
        Some(("流式主循环 · Tool 协议 · 权限门".to_string(), theme.text)),
    ));
    rows.push(column_row(
        theme,
        left_w,
        right_w,
        None,
        Some(("Hooks · MCP · 子代理 · 自动记忆".to_string(), theme.text)),
    ));
    rows.push(column_row(
        theme,
        left_w,
        right_w,
        None,
        Some(("transcript 持久化 · --continue".to_string(), theme.text)),
    ));
    rows
}

/// Claude Code 风格的思考阶段俏皮词。
const THINKING_WORDS: [&str; 12] = [
    "Bootstrapping",
    "Razzle-dazzling",
    "Hashing",
    "Pondering",
    "Wrangling",
    "Synthesizing",
    "Mulling",
    "Churning",
    "Digesting",
    "Concocting",
    "Scheming",
    "Weaving",
];

fn thinking_stage(seed: usize) -> &'static str {
    THINKING_WORDS[seed % THINKING_WORDS.len()]
}

/// 欢迎卡片行（带 ╭╮ 边框），作为滚动内容的一部分——消息增长时随流上移。
fn welcome_card_rows(
    theme: &Theme,
    user: &str,
    model: &str,
    mode: &str,
    cwd: &str,
    width: usize,
) -> Vec<Line<'static>> {
    let gray = ratatui::style::Style::default().fg(ratatui::style::Color::Gray);
    let title = format!(" bingo v0.1.0 · {model} ");
    let title_len = title.chars().count();
    let mut rows = Vec::new();
    rows.push(Line::from(Span::styled(
        format!(
            "╭{}{}╮",
            title,
            "─".repeat(width.saturating_sub(title_len + 2))
        ),
        gray,
    )));
    let inner_w = width.saturating_sub(2);
    for line in welcome_rows(theme, user, model, mode, cwd, inner_w) {
        let mut spans = vec![Span::styled("│", gray)];
        spans.extend(line.spans);
        spans.push(Span::styled("│", gray));
        rows.push(Line::from(spans));
    }
    rows.push(Line::from(Span::styled(
        format!("╰{}╯", "─".repeat(width.saturating_sub(2))),
        gray,
    )));
    rows
}

fn permission_mode_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Default => "default",
        PermissionMode::AcceptEdits => "acceptEdits",
        PermissionMode::BypassPermissions => "bypassPermissions",
        PermissionMode::DontAsk => "dontAsk",
        PermissionMode::Plan => "plan",
    }
}

impl Component for BingoChat {
    fn title(&self) -> &str {
        "bingo"
    }

    fn draw(&mut self, area: Rect, buf: &mut Buffer) {
        self.drain_events();
        self.drain_asks();
        self.width = area.width as usize;
        let mut click_ranges: Vec<(u16, u16, ClickAction)> = Vec::new();
        let theme_claude = self.theme.claude;
        let theme_dim = self.theme.dim();
        let theme_text = self.theme.text();
        let theme_thinking = self.theme.thinking();

        let warn_height = if self.warnings.is_empty() { 0 } else { 1 } as u16;
        // 警告行固定在最顶部（不随消息滚动）
        let warn_y = area.y;
        if warn_height > 0 {
            let warn = self.warnings.first().cloned().unwrap_or_default();
            buf.set_string(
                area.x,
                warn_y,
                format!(" ⚠ {warn}"),
                ratatui::style::Style::default().fg(self.theme.warning),
            );
        }

        let msg_top = warn_y + warn_height;
        self.msg_top = msg_top;
        // 消息区：欢迎卡片 + 消息作为同一滚动流（卡片随消息增长上移滚出）
        let msg_bottom_limit = area.height.saturating_sub(2); // 分隔线 + 输入
        let spinner = rsmarkdown_tui::activities::spinner(self.tick);
        let mut rows: Vec<Line<'static>> = Vec::new();
        rows.extend(welcome_card_rows(
            &self.theme,
            &self.user,
            &self.session.model,
            permission_mode_label(self.session.permission_mode),
            &self.cwd,
            area.width as usize,
        ));
        let mut activity_ranges: Vec<rsmarkdown_tui::activities::ActivityRowRange> = Vec::new();
        for i in 0..self.messages.len() {
            match self.messages[i].role {
                Role::User => {
                    rows.push(Line::from(vec![
                        Span::styled("❯ ", self.theme.tool_running()),
                        Span::styled(self.messages[i].text.clone(), self.theme.text),
                    ]));
                }
                Role::Assistant => {
                    let mut render = {
                        let processor = &mut self.processor;
                        let renderer = &mut self.renderer;
                        let cache = &mut self.reply_cache;
                        let width = self.width;
                        move |reply: &str| {
                            if reply.is_empty() {
                                return Vec::new();
                            }
                            if let Some(lines) = cache.get(reply) {
                                return lines.clone();
                            }
                            renderer.set_width(width);
                            let doc = processor.process_streaming(reply);
                            renderer.render(&doc);
                            let lines = renderer.lines().to_vec();
                            cache.insert(reply.to_string(), lines.clone());
                            lines
                        }
                    };
                    // text 与活动按模型输出顺序交错：活动创建时记录的 text
                    // 字符位置（insert_points）决定它插入在哪段正文之间
                    // （Claude Code：正文与工具调用逐段交叉，而非聚合）。
                    let msg = &self.messages[i];
                    let text = &msg.text;
                    let char_bounds: Vec<usize> = text
                        .char_indices()
                        .map(|(b, _)| b)
                        .collect();
                    let mut rendered_chars = 0usize;
                    let mut rendered_bytes = 0usize;
                    // text 段折叠：段 >2 行时折叠为首 2 行 + 提示（CC `… +N lines`）。
                    // 返回折叠提示行行号（None = 未折叠）。
                    let push_text = |rows: &mut Vec<Line<'static>>,
                                         reply: Vec<Line<'static>>| {
                        for (j, line) in reply.into_iter().enumerate() {
                            if j == 0 {
                                let mut spans = vec![Span::styled(
                                    "⏺ ",
                                    ratatui::style::Style::default().fg(theme_claude),
                                )];
                                spans.extend(line.spans);
                                rows.push(Line::from(spans));
                            } else {
                                rows.push(line);
                            }
                        }
                    };
                    for (idx, act) in msg.activities.iter().enumerate() {
                        let pos_chars = msg
                            .insert_points
                            .get(idx)
                            .copied()
                            .unwrap_or(rendered_chars)
                            .min(text.chars().count());
                        if pos_chars > rendered_chars {
                            let seg_end = char_bounds
                                .get(pos_chars)
                                .copied()
                                .unwrap_or(text.len());
                            let reply = render(&text[rendered_bytes..seg_end]);
                            push_text(&mut rows, reply);
                            rendered_chars = pos_chars;
                            rendered_bytes = seg_end;
                        }
                        let group_idx = msg.group_of.get(idx).copied().flatten();
                        let group_collapsed = group_idx.is_some_and(|g| {
                            !msg.groups[g].expanded
                        });
                        let is_group_head = group_idx.is_some_and(|g| {
                            msg.groups[g].activities.first() == Some(&idx)
                        });
                        if group_collapsed && !is_group_head {
                            // 组折叠：只有组首渲染折叠行，其余活动跳过。
                            continue;
                        }
                        if let Some(g) = group_idx
                            && !msg.groups[g].expanded
                        {
                            // 折叠组：一行规则摘要（`Read 3 files (ctrl+o to expand)`）。
                            // 时态按组内是否还有 Running 工具实时判定：工具全部
                            // 完成后立即转过去时（CC isActiveGroup 语义）。
                            let in_progress = msg.groups[g].active
                                && msg.groups[g].activities.iter().any(|&ai| {
                                    matches!(
                                        msg.activities.get(ai),
                                        Some(a) if matches!(
                                            &a.kind,
                                            ActivityKind::Tool(t)
                                                if t.status == ToolStatus::Running
                                        )
                                    )
                                });
                            let summary = collapse_summary(&msg.groups[g], in_progress);
                            let row = rows.len() as u16;
                            let mut spans = Vec::new();
                            if msg.groups[g].active {
                                spans.push(Span::styled(
                                    format!("{spinner} "),
                                    theme_thinking,
                                ));
                            }
                            spans.push(Span::styled(summary, theme_text));
                            spans.push(Span::styled(
                                " (ctrl+o to expand)".to_string(),
                                theme_dim,
                            ));
                            rows.push(Line::from(spans));
                            click_ranges.push((
                                row,
                                row + 1,
                                ClickAction::Group { message: i, group: g },
                            ));
                            // 执行中的折叠组下方显示最近工具的输入（CC ⎿ 行）；
                            // 组完成后该行消失（hint 只在进行时渲染）。
                            if in_progress
                                && let Some(hint) = &msg.groups[g].last_hint
                            {
                                rows.push(Line::from(Span::styled(
                                    format!("  ⎿  {hint}"),
                                    theme_dim,
                                )));
                            }
                            continue;
                        }
                        let (lines, mut local) = rsmarkdown_tui::activities::layout_activity(
                            act,
                            &[idx],
                            i,
                            rows.len() as u16,
                            spinner,
                            &self.theme,
                            &mut render,
                        );
                        // 组展开态：组首工具行同时是聚合行的位置——点击它折叠回组
                        // （组内工具无独立内容，toggle 工具无意义）。
                        if let Some(g) = group_idx
                            && let Some(first) = local.first()
                        {
                            click_ranges.push((
                                first.start,
                                first.end,
                                ClickAction::Group { message: i, group: g },
                            ));
                        }
                        rows.extend(lines);
                        activity_ranges.append(&mut local);
                    }
                    if rendered_bytes < text.len() {
                        let reply = render(&text[rendered_bytes..]);
                        push_text(&mut rows, reply);
                    }
                }
            }
        }

        self.activity_ranges = activity_ranges;
        self.click_ranges = click_ranges;

        // 消息区高度：跟随内容，不超过可用空间
        let needed = rows.len() as u16;
        let max_msg_h = msg_bottom_limit.saturating_sub(msg_top);
        let msg_h = needed.min(max_msg_h).max(1);
        let msg_rect = Rect {
            x: area.x,
            y: msg_top,
            width: area.width,
            height: msg_h,
        };

        let sep_y = msg_rect.y + msg_h;
        for x in 0..area.width {
            buf.set_string(
                area.x + x,
                sep_y,
                "─",
                ratatui::style::Style::default().fg(ratatui::style::Color::Gray),
            );
        }

        let caret = if self.typing { '▋' } else { ' ' };
        let input_line = Line::from(vec![
            Span::styled("❯ ", self.theme.tool_running()),
            Span::styled(self.input.clone(), self.theme.text),
            Span::styled(caret.to_string(), self.theme.tool_running()),
        ]);
        buf.set_line(area.x, sep_y + 1, &input_line, area.width);

        // 消息滚动（内容超出消息区时）
        let total = rows.len() as u16;
        let max_scroll = total.saturating_sub(msg_h);
        if self.auto_scroll {
            self.scroll = max_scroll;
        }
        let scroll = self.scroll.min(max_scroll);
        self.scroll = scroll;
        if scroll == max_scroll {
            self.auto_scroll = true;
        }
        for (y, line) in rows
            .iter()
            .skip(scroll as usize)
            .take(msg_h as usize)
            .enumerate()
        {
            buf.set_line(msg_rect.x, msg_rect.y + y as u16, line, msg_rect.width);
        }
    }

    fn event(&mut self, event: Event) -> bool {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                // 回合运行中：Ctrl+C / Esc 中断当前回合（工具照常跑完，流中止）。
                if self.busy
                    && (key.code == KeyCode::Esc
                        || (key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL)))
                {
                    let _ = self.cancel_tx.send(true);
                    return true;
                }
                if self.typing {
                    match key.code {
                        KeyCode::Char(c)
                            if !c.is_control()
                                && !key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            self.input.push(c);
                            return true;
                        }
                        KeyCode::Backspace => {
                            self.input.pop();
                            return true;
                        }
                        KeyCode::Enter => {
                            self.submit();
                            return true;
                        }
                        _ => {}
                    }
                }
                match key.code {
                    KeyCode::Esc => {
                        self.typing = !self.typing;
                        true
                    }
                    KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.toggle_transcript();
                        true
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        self.auto_scroll = false;
                        self.scroll = self.scroll.saturating_add(1);
                        true
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        self.auto_scroll = false;
                        self.scroll = self.scroll.saturating_sub(1);
                        true
                    }
                    KeyCode::PageDown => {
                        self.auto_scroll = false;
                        self.scroll = self.scroll.saturating_add(10);
                        true
                    }
                    KeyCode::PageUp => {
                        self.auto_scroll = false;
                        self.scroll = self.scroll.saturating_sub(10);
                        true
                    }
                    KeyCode::Char('g') => {
                        self.auto_scroll = false;
                        self.scroll = 0;
                        true
                    }
                    KeyCode::Char('G') => {
                        self.auto_scroll = true;
                        true
                    }
                    _ => false,
                }
            }
            Event::Mouse(m) => match m.kind {
                MouseEventKind::ScrollUp => {
                    self.scroll = self.scroll.saturating_sub(3);
                    self.auto_scroll = false;
                    true
                }
                MouseEventKind::ScrollDown => {
                    self.scroll = self.scroll.saturating_add(3);
                    self.auto_scroll = false;
                    true
                }
                // 点击可展开活动的标题行 → 折叠/展开
                MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                    if m.row >= self.msg_top {
                        let doc_row = self.scroll + (m.row - self.msg_top);
                        self.toggle_at(doc_row);
                    }
                    true
                }
                _ => false,
            }
            _ => false,
        }
    }

    fn on_tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        // 运行态 thinking 每块独立计时（相对各自 start_tick），不依赖 delta 到达。
        for msg in &mut self.messages {
            for act in &mut msg.activities {
                if let ActivityKind::Thinking(t) = &mut act.kind
                    && t.state == ThinkingState::Running
                {
                    t.duration_ms = self
                        .tick
                        .saturating_sub(t.start_tick)
                        .saturating_mul(33);
                }
            }
        }
    }

    fn status(&self) -> String {
        if self.busy {
            "working…".to_string()
        } else {
            "idle".to_string()
        }
    }

    fn hints(&self) -> &'static str {
        "Enter to send · Esc toggles input · ctrl+o expand"
    }

    fn footer_badges(&self) -> Vec<FooterBadge> {
        vec![FooterBadge::new(
            self.session.model.clone(),
            self.theme.tool_running(),
        )]
    }

    fn on_ask(&mut self) -> Option<PermissionRequest> {
        self.pending_ask
            .as_ref()
            .map(|(request, _)| request.clone())
    }

    fn on_dialog_closed(&mut self, action: DialogAction) {
        if let Some((_, tx)) = self.pending_ask.take() {
            let _ = tx.send(action);
        }
    }

    fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }
}

/// 启动 TUI 会话。draw/event 崩溃时恢复终端并报告（不裸退）。
pub fn run_tui_session(session: Arc<Session>) -> Result<(), Box<dyn std::error::Error>> {
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let (asks_tx, asks_rx) = mpsc::unbounded_channel();

    let mut app = App::new(vec![Box::new(BingoChat::new(
        session,
        events_tx,
        events_rx,
        asks_tx,
        asks_rx,
    ))]);
    app.set_status_bar(false);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = run_tui(&mut app);
    }));
    if let Err(payload) = result {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::event::DisableMouseCapture
        );
        let message = payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "unknown panic".to_string());
        eprintln!("[bingo] TUI panicked: {message}");
        eprintln!(
            "[bingo] backtrace:\n{}",
            std::backtrace::Backtrace::force_capture()
        );
    }
    Ok(())
}

#[allow(dead_code)]
fn _assert_send(_: Pin<Box<dyn std::future::Future<Output = bool> + Send>>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use rsmarkdown_tui::activities::ToolCall;

    pub(super) fn test_chat() -> BingoChat {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (asks_tx, asks_rx) = mpsc::unbounded_channel();
        let session = Arc::new(Session {
            client: crate::api::client::Client::new("test-key".to_string(), "https://example.com".to_string()),
            model: "test-model".to_string(),
            permission_mode: crate::permission::PermissionMode::Default,
            settings: crate::settings::Settings::default(),
            system: Vec::new(),
            transcript: None,
            depth: 0,
            home: std::env::temp_dir(),
            quiet: true,
            compact_failures: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
        });
        BingoChat::new(session, events_tx, events_rx, asks_tx, asks_rx)
    }

    fn tool_activity() -> Activity {
        let mut hint = Activity::new(ActivityKind::Tool(ToolCall::running("Bash", "")));
        hint.set_content(vec![Line::from("output line 1"), Line::from("output line 2")]);
        hint.expand_hint = Some("ctrl+o to expand".to_string());
        hint
    }

    #[test]
    fn click_toggles_tool_activity() {
        let mut chat = test_chat();
        chat.messages.push(UiMessage {
            role: Role::Assistant,
            text: "reply".to_string(),
            activities: vec![tool_activity()],
            insert_points: vec![0],
            groups: Vec::new(),
            group_of: vec![None],
        });
        let area = Rect { x: 0, y: 0, width: 100, height: 30 };
        let mut buf = Buffer::empty(area);
        chat.draw(area, &mut buf);
        assert!(!chat.activity_ranges.is_empty(), "draw must populate ranges");

        let range = &chat.activity_ranges[0];
        assert_eq!(range.path, vec![0]);
        let header_row = range.start;
        let doc_row = header_row + chat.scroll;
        assert!(chat.toggle_at(doc_row));
        let act = &chat.messages[0].activities[0];
        assert!(act.is_expanded(), "click on header expands");
        let _ = chat;
    }

    #[test]
    fn click_collapses_again() {
        let mut chat = test_chat();
        chat.messages.push(UiMessage {
            role: Role::Assistant,
            text: "reply".to_string(),
            activities: vec![tool_activity()],
            insert_points: vec![0],
            groups: Vec::new(),
            group_of: vec![None],
        });
        let area = Rect { x: 0, y: 0, width: 100, height: 30 };
        let mut buf = Buffer::empty(area);
        chat.draw(area, &mut buf);
        let range = &chat.activity_ranges[0];
        let doc_row = range.start + chat.scroll;
        assert!(chat.toggle_at(doc_row));
        assert!(chat.toggle_at(doc_row));
        assert!(!chat.messages[0].activities[0].is_expanded());
    }

    #[test]
    fn click_outside_ranges_is_noop() {
        let mut chat = test_chat();
        chat.messages.push(UiMessage {
            role: Role::Assistant,
            text: "reply".to_string(),
            activities: vec![tool_activity()],
            insert_points: vec![0],
            groups: Vec::new(),
            group_of: vec![None],
        });
        let area = Rect { x: 0, y: 0, width: 100, height: 30 };
        let mut buf = Buffer::empty(area);
        chat.draw(area, &mut buf);
        assert!(!chat.toggle_at(999), "no range -> no toggle");
    }

    fn thinking_text(hint: &Activity) -> String {
        hint.content
            .iter()
            .map(|l| l.to_string().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_string()
    }

    /// 多轮 thinking：工具轮后的 delta 必须开新块，后续 delta 续写到新块
    /// （不得写到已完成的旧块——回归：iter_mut().find() 错写首个块）。
    #[test]
    fn tool_turn_thinking_blocks_stay_separate() {
        let mut chat = test_chat();
        let (tx, rx) = mpsc::unbounded_channel();
        chat.events_rx = rx;
        tx.send(UiEvent::TurnStart).unwrap();
        chat.drain_events();
        tx.send(UiEvent::ThinkingDelta("plan the fetch".into())).unwrap();
        chat.drain_events();
        tx.send(UiEvent::ToolStart { name: "WebFetch".into() }).unwrap();
        chat.drain_events();
        tx.send(UiEvent::ThinkingDelta("got it".into())).unwrap();
        tx.send(UiEvent::ThinkingDelta(", summarizing".into())).unwrap();
        chat.drain_events();

        let acts = &chat.messages[0].activities;
        assert_eq!(acts.len(), 3, "thinking + tool + new thinking");
        let (first, tool, second) = (&acts[0], &acts[1], &acts[2]);
        assert!(matches!(&first.kind, ActivityKind::Thinking(t) if t.state == ThinkingState::Done));
        assert!(matches!(tool.kind, ActivityKind::Tool(_)));
        assert_eq!(thinking_text(first), "plan the fetch");
        assert_eq!(thinking_text(second), "got it, summarizing");
        assert!(matches!(&second.kind, ActivityKind::Thinking(t) if t.state == ThinkingState::Running));
    }

    /// 单轮内连续 delta 续写同一块。
    #[test]
    fn single_turn_thinking_accumulates() {
        let mut chat = test_chat();
        let (tx, rx) = mpsc::unbounded_channel();
        chat.events_rx = rx;
        tx.send(UiEvent::TurnStart).unwrap();
        tx.send(UiEvent::ThinkingDelta("a".into())).unwrap();
        tx.send(UiEvent::ThinkingDelta("b".into())).unwrap();
        chat.drain_events();

        let acts = &chat.messages[0].activities;
        assert_eq!(acts.len(), 1);
        assert_eq!(thinking_text(&acts[0]), "ab");
    }

    /// 交错渲染：text 与活动按插入点交叉（模型输出 text → tool → text 顺序）。
    #[test]
    fn interleaves_text_and_activities_in_order() {
        let mut chat = test_chat();
        chat.messages.push(UiMessage {
            role: Role::Assistant,
            text: "hello world".to_string(),
            activities: vec![tool_activity()],
            insert_points: vec![5],
            groups: Vec::new(),
            group_of: vec![None],
        });
        let area = Rect { x: 0, y: 0, width: 100, height: 40 };
        let mut buf = Buffer::empty(area);
        chat.draw(area, &mut buf);
        let lines: Vec<String> = (0..40)
            .map(|y| {
                (0..100)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .filter(|l| !l.trim().is_empty())
            .collect();
        let joined = lines.join("\n");
        let hello = joined.find("hello").expect("first text before tool");
        let tool = joined.find("Bash").expect("tool row");
        let world = joined.find("world").expect("trailing text after tool");
        assert!(hello < tool, "text before tool: {joined}");
        assert!(tool < world, "tool before trailing text: {joined}");
    }
}

#[cfg(test)]
mod fold_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bash_classifier_collapsible_commands() {
        assert_eq!(
            classify_bash_command("cat README.md"),
            Some(CollapseKind::Read(None))
        );
        assert_eq!(
            classify_bash_command("grep -rn foo src/"),
            Some(CollapseKind::Search)
        );
        assert_eq!(
            classify_bash_command("ls -la ."),
            Some(CollapseKind::List)
        );
        assert_eq!(
            classify_bash_command("cat a | grep foo"),
            Some(CollapseKind::Search)
        );
        assert_eq!(
            classify_bash_command("ls dir && echo \"---\" && ls dir2"),
            Some(CollapseKind::List)
        );
        assert_eq!(
            classify_bash_command("head -20 file > /tmp/out"),
            Some(CollapseKind::Read(None))
        );
    }

    #[test]
    fn bash_classifier_other_commands_not_collapsible() {
        assert_eq!(classify_bash_command("git log --oneline -10"), None);
        assert_eq!(classify_bash_command("npm install"), None);
        assert_eq!(classify_bash_command("echo hello"), None);
        assert_eq!(classify_bash_command("ls && git status"), None);
        assert_eq!(classify_bash_command(""), None);
    }

    #[test]
    fn tool_classifier_read_grep_glob() {
        assert_eq!(
            classify_tool("Read", &json!({"file_path": "a.md"})),
            Some(CollapseKind::Read(Some("a.md".to_string())))
        );
        assert_eq!(classify_tool("Read", &json!({})), None);
        assert_eq!(classify_tool("Grep", &json!({"pattern": "x"})), Some(CollapseKind::Search));
        assert_eq!(classify_tool("Glob", &json!({"glob": "**/*.rs"})), Some(CollapseKind::Search));
        // 普通 Bash（含 git log）折叠为 bash 类；纯中性命令不折叠。
        assert_eq!(classify_tool("Bash", &json!({"command": "git log"})), Some(CollapseKind::Bash));
        assert_eq!(classify_tool("Bash", &json!({"command": "echo hi"})), None);
        assert_eq!(classify_tool("Bash", &json!({"command": "cargo test && echo done"})), Some(CollapseKind::Bash));
        // 非折叠工具（CC 语义：WebSearch/WebFetch 不参与）断组。
        assert_eq!(classify_tool("WebFetch", &json!({"url": "x"})), None);
        assert_eq!(classify_tool("WebSearch", &json!({"query": "x"})), None);
    }

    #[test]
    fn summary_past_tense_counts() {
        let mut g = CollapseGroup {
            activities: vec![0, 1, 2],
            search: 1,
            read_paths: vec!["a.md".into(), "b.md".into(), "c.md".into()],
            read_ops: 0,
            list: 0,
            bash: 0,
            active: false,
            expanded: false,
            last_hint: None,
        };
        assert_eq!(collapse_summary(&g, false), "Searched for 1 pattern, read 3 files");
        g.search = 2;
        assert_eq!(collapse_summary(&g, false), "Searched for 2 patterns, read 3 files");
        g.active = true;
        assert_eq!(
            collapse_summary(&g, true),
            "Searching for 2 patterns, reading 3 files…"
        );
    }

    #[test]
    fn summary_read_paths_dedupe_and_ops_fallback() {
        let g = CollapseGroup {
            activities: vec![0, 1],
            search: 0,
            read_paths: vec!["a.md".into(), "a.md".into()],
            read_ops: 0,
            list: 0,
            bash: 0,
            active: false,
            expanded: false,
            last_hint: None,
        };
        assert_eq!(collapse_summary(&g, false), "Read 1 file");
        let g = CollapseGroup {
            activities: vec![0],
            search: 0,
            read_paths: vec![],
            read_ops: 2,
            list: 1,
            bash: 0,
            active: false,
            expanded: false,
            last_hint: None,
        };
        assert_eq!(collapse_summary(&g, false), "Read 2 files, listed 1 directory");
    }

    #[test]
    fn result_summaries() {
        assert_eq!(
            result_summary("Read", "line1\nline2\n\nline3"),
            Some("Read 3 lines".to_string())
        );
        assert_eq!(result_summary("Grep", "a:1:x\nb:2:y"), Some("Found 2 matches".to_string()));
        assert_eq!(result_summary("Glob", "a.rs\nb.rs"), Some("Found 2 files".to_string()));
        assert_eq!(result_summary("Bash", "out"), None);
    }
}

#[cfg(test)]
mod fold_render_tests {
    use super::*;
    use serde_json::json;
    use tests::test_chat;

    fn render_lines(chat: &mut BingoChat, height: u16) -> Vec<String> {
        let area = Rect { x: 0, y: 0, width: 120, height };
        let mut buf = Buffer::empty(area);
        chat.draw(area, &mut buf);
        (0..height)
            .map(|y| {
                (0..120)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .filter(|l| !l.trim().is_empty())
            .collect()
    }

    #[test]
    fn parallel_reads_collapse_to_one_line() {
        let mut chat = test_chat();
        chat.messages.push(UiMessage {
            role: Role::Assistant,
            text: String::new(),
            activities: vec![],
            insert_points: vec![],
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        chat.stream_msg = Some(0);
        // 两个并行 Read：ToolStart 建活动，ToolReady 入组。
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolStart { name: "Read".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Read".into(),
            input: json!({"file_path": "a.md"}),
        });
        let _ = chat.events.send(UiEvent::ToolStart { name: "Read".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Read".into(),
            input: json!({"file_path": "b.md"}),
        });
        chat.drain_events();
        let lines = render_lines(&mut chat, 20);
        let joined = lines.join("\n");
        assert!(joined.contains("Reading 2 files"), "active summary: {joined}");
        assert!(joined.contains("ctrl+o to expand"), "fold hint: {joined}");
        assert!(!joined.contains("a.md"), "paths hidden when collapsed: {joined}");
    }

    #[test]
    fn group_done_uses_past_tense() {
        let mut chat = test_chat();
        chat.messages.push(UiMessage {
            role: Role::Assistant,
            text: String::new(),
            activities: vec![],
            insert_points: vec![],
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::ToolStart { name: "Read".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Read".into(),
            input: json!({"file_path": "a.md"}),
        });
        chat.drain_events();
        chat.drain_events();
        let _ = chat.events.send(UiEvent::TurnEnd);
        chat.drain_events();
        let lines = render_lines(&mut chat, 20);
        let joined = lines.join("\n");
        assert!(joined.contains("Read 1 file"), "past tense: {joined}");
    }

    #[test]
    fn ctrl_o_expands_group_to_individual_tools() {
        let mut chat = test_chat();
        chat.messages.push(UiMessage {
            role: Role::Assistant,
            text: String::new(),
            activities: vec![],
            insert_points: vec![],
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        chat.stream_msg = Some(0);
        for path in ["a.md", "b.md"] {
            let _ = chat.events.send(UiEvent::ToolStart { name: "Read".into() });
            chat.drain_events();
            let _ = chat.events.send(UiEvent::ToolReady {
                name: "Read".into(),
                input: json!({"file_path": path}),
            });
            chat.drain_events();
        }
        let _ = chat.events.send(UiEvent::ToolDone(crate::query::ToolCallDone {
            name: "Read".into(),
            summary: "Read a.md".into(),
            output: "l1\nl2\nl3".into(),
            is_error: false,
            duration_ms: 0,
        diff: None,
        }));
        let _ = chat.events.send(UiEvent::ToolDone(crate::query::ToolCallDone {
            name: "Read".into(),
            summary: "Read b.md".into(),
            output: "x\ny".into(),
            is_error: false,
            duration_ms: 0,
        diff: None,
        }));
        chat.drain_events();
        // ctrl+o 全局展开
        assert!(chat.toggle_transcript());
        let lines = render_lines(&mut chat, 30);
        let joined = lines.join("\n");
        assert!(joined.contains("Read a.md"), "expanded first tool: {joined}");
        assert!(joined.contains("Read b.md"), "expanded second tool: {joined}");
        assert!(joined.contains("Read 3 lines"), "result summary row: {joined}");
        assert!(!joined.contains("Reading 2 files"), "no collapse line: {joined}");
    }

    #[test]
    fn non_collapsible_tool_breaks_group() {
        let mut chat = test_chat();
        chat.messages.push(UiMessage {
            role: Role::Assistant,
            text: String::new(),
            activities: vec![],
            insert_points: vec![],
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::ToolStart { name: "Read".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Read".into(),
            input: json!({"file_path": "a.md"}),
        });
        let _ = chat.events.send(UiEvent::ToolStart { name: "WebSearch".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "WebSearch".into(),
            input: json!({"query": "rust"}),
        });
        chat.drain_events();
        let lines = render_lines(&mut chat, 20);
        let joined = lines.join("\n");
        assert!(joined.contains("Read 1 file"), "group rendered: {joined}");
        assert!(joined.contains("WebSearch"), "websearch independent: {joined}");
        assert!(
            !joined.contains("Reading"),
            "group closed by websearch: {joined}"
        );
    }

    #[test]
    fn tool_after_thinking_placeholder_groups_without_panic() {
        // 回归：TurnStart 占位 thinking 后接工具——group_of 必须与 activities 同步，
        // 否则 ToolReady 用 activities 索引写 group_of 时越界。
        let mut chat = test_chat();
        chat.stream_msg = Some(0);
        chat.messages.push(UiMessage {
            role: Role::Assistant,
            text: String::new(),
            activities: vec![],
            insert_points: vec![],
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        // 模拟 TurnStart 占位 thinking
        let mut hint = Activity::new(ActivityKind::Thinking(Thinking {
            state: ThinkingState::Running,
            duration_ms: 0,
            digest: None,
            stage: "Thinking",
            tokens: None,
            start_tick: 0,
        }));
        hint.expand_hint = Some("ctrl+o to expand".to_string());
        chat.messages[0].activities.push(hint);
        chat.messages[0].insert_points.push(0);
        chat.messages[0].group_of.push(None);
        // 工具事件
        let _ = chat.events.send(UiEvent::ToolStart { name: "Read".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Read".into(),
            input: json!({"file_path": "a.md"}),
        });
        chat.drain_events();
        let lines = render_lines(&mut chat, 30);
        let joined = lines.join("\n");
        assert!(joined.contains("Reading 1 file"), "group row: {joined}");
    }

    #[test]
    fn interleaved_group_keeps_text_position() {
        let mut chat = test_chat();
        chat.messages.push(UiMessage {
            role: Role::Assistant,
            text: "let me read".to_string(),
            activities: vec![],
            insert_points: vec![],
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::TextDelta("let me read".into()));
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolStart { name: "Read".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Read".into(),
            input: json!({"file_path": "a.md"}),
        });
        chat.drain_events();
        let lines = render_lines(&mut chat, 20);
        let joined = lines.join("\n");
        let text_pos = joined.find("let me read").expect("text");
        let group_pos = joined.find("Reading 1 file").expect("group line");
        assert!(text_pos < group_pos, "text before group: {joined}");
    }
}

#[cfg(test)]
mod fold_toggle_tests {
    use super::*;
    use serde_json::json;
    use tests::test_chat;

    fn build_group_chat(chat: &mut BingoChat, finish_tools: bool) {
        chat.messages.push(UiMessage {
            role: Role::Assistant,
            text: String::new(),
            activities: vec![],
            insert_points: vec![],
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        chat.stream_msg = Some(0);
        for path in ["a.md", "b.md"] {
            let _ = chat.events.send(UiEvent::ToolStart { name: "Read".into() });
            chat.drain_events();
            let _ = chat.events.send(UiEvent::ToolReady {
                name: "Read".into(),
                input: json!({"file_path": path}),
            });
            chat.drain_events();
        }
        if finish_tools {
            for (summary, out) in [("Read a.md", "l1\nl2\nl3"), ("Read b.md", "x\ny")] {
                let _ = chat.events.send(UiEvent::ToolDone(crate::query::ToolCallDone {
                    name: "Read".into(),
                    summary: summary.into(),
                    output: out.into(),
                    is_error: false,
                    duration_ms: 0,
                diff: None,
                }));
            }
            chat.drain_events();
        }
        chat.stream_msg = None;
    }

    fn finish_turn(chat: &mut BingoChat) {
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::TurnEnd);
        chat.drain_events();
        chat.stream_msg = None;
    }

    fn visible(chat: &mut BingoChat) -> String {
        let area = Rect { x: 0, y: 0, width: 120, height: 40 };
        let mut buf = Buffer::empty(area);
        chat.draw(area, &mut buf);
        (0..40)
            .map(|y| {
                (0..120)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn ctrl_o_round_trip_collapses_group_back() {
        let mut chat = test_chat();
        build_group_chat(&mut chat, true);
        finish_turn(&mut chat);
        // 初始：折叠组（完成态过去时）
        assert!(visible(&mut chat).contains("Read 2 files"), "collapsed first");
        // 展开
        assert!(chat.toggle_transcript());
        let expanded = visible(&mut chat);
        assert!(expanded.contains("Read a.md"), "expanded: {expanded}");
        assert!(!expanded.contains("Read 2 files"), "no collapse line: {expanded}");
        // 再折叠
        assert!(chat.toggle_transcript());
        let collapsed = visible(&mut chat);
        assert!(
            collapsed.contains("Read 2 files"),
            "collapsed again: {collapsed}"
        );
        assert!(!collapsed.contains("Read a.md"), "tools hidden: {collapsed}");
    }

    #[test]
    fn active_group_round_trip_uses_present_tense() {
        let mut chat = test_chat();
        build_group_chat(&mut chat, false);
        // 未 TurnEnd 且工具 Running：组 active，折叠行用进行时
        assert!(
            visible(&mut chat).contains("Reading 2 files"),
            "active collapsed: {}",
            visible(&mut chat)
        );
        assert!(chat.toggle_transcript());
        let expanded = visible(&mut chat);
        assert!(expanded.contains("Read"), "expanded shows tools: {expanded}");
        assert!(!expanded.contains("Reading 2 files"), "no collapse line");
        assert!(chat.toggle_transcript());
        let collapsed = visible(&mut chat);
        assert!(
            collapsed.contains("Reading 2 files"),
            "active collapsed again: {collapsed}"
        );
    }

    #[test]
    fn click_group_then_ctrl_o_collapses() {
        let mut chat = test_chat();
        build_group_chat(&mut chat, true);
        finish_turn(&mut chat);
        let area = Rect { x: 0, y: 0, width: 120, height: 40 };
        let mut buf = Buffer::empty(area);
        chat.draw(area, &mut buf);
        // 点击组折叠行展开
        let row = chat
            .click_ranges
            .iter()
            .find(|(_, _, a)| matches!(a, ClickAction::Group { .. }))
            .map(|(start, ..)| *start)
            .expect("group fold row");
        assert!(chat.toggle_at(row), "click expands group");
        let expanded = visible(&mut chat);
        assert!(expanded.contains("Read a.md"), "click expanded: {expanded}");
        // ctrl+o 折叠回
        assert!(chat.toggle_transcript());
        let collapsed = visible(&mut chat);
        assert!(
            collapsed.contains("Read 2 files"),
            "ctrl+o collapsed: {collapsed}"
        );
    }
}

#[cfg(test)]
mod fold_roundtrip_live_tests {
    use super::*;
    use serde_json::json;
    use tests::test_chat;

    fn visible(chat: &mut BingoChat) -> String {
        let area = Rect { x: 0, y: 0, width: 120, height: 40 };
        let mut buf = Buffer::empty(area);
        chat.draw(area, &mut buf);
        (0..40)
            .map(|y| {
                (0..120)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn start_group(chat: &mut BingoChat) {
        chat.messages.push(UiMessage {
            role: Role::Assistant,
            text: String::new(),
            activities: vec![],
            insert_points: vec![],
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        chat.stream_msg = Some(0);
        for path in ["a.md", "b.md"] {
            let _ = chat.events.send(UiEvent::ToolStart { name: "Read".into() });
            chat.drain_events();
            let _ = chat.events.send(UiEvent::ToolReady {
                name: "Read".into(),
                input: json!({"file_path": path}),
            });
            chat.drain_events();
        }
    }

    #[test]
    fn running_tool_shows_input_summary_after_ready() {
        // 回归：子 agent 执行中 header 就带 description（CC 同款），
        // 而不是等 ToolDone 才显示。
        let mut chat = test_chat();
        chat.messages.push(UiMessage {
            role: Role::Assistant,
            text: String::new(),
            activities: vec![],
            insert_points: vec![],
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::ToolStart { name: "Agent".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Agent".into(),
            input: json!({"description": "读取项目说明并总结", "prompt": "..."}),
        });
        chat.drain_events();
        let area = Rect { x: 0, y: 0, width: 120, height: 30 };
        let mut buf = Buffer::empty(area);
        chat.draw(area, &mut buf);
        let joined: String = (0..30)
            .map(|y| {
                (0..120)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let flat = joined.replace(' ', "");
        assert!(
            flat.contains("description=\"读取项目说明并总结\""),
            "running header shows input summary: {joined}"
        );
        // 完成后 duration 用真实值（不再硬编码 0ms）
        let _ = chat.events.send(UiEvent::ToolDone(crate::query::ToolCallDone {
            name: "Agent".into(),
            summary: "Agent description=\"读取项目说明并总结\"".into(),
            output: "line".into(),
            is_error: false,
            diff: None,
            duration_ms: 3210,
        }));
        chat.drain_events();
        let area = Rect { x: 0, y: 0, width: 120, height: 30 };
        let mut buf = Buffer::empty(area);
        chat.draw(area, &mut buf);
        let joined: String = (0..30)
            .map(|y| {
                (0..120)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("3210ms"), "real duration: {joined}");
    }

    #[test]
    fn watch_event_renders_inline_and_updates() {
        // Watchable 生命周期：Running 行 → 同 label 原地更新 → 终态含时长，
        // payload 可展开。
        let mut chat = test_chat();
        chat.messages.push(UiMessage {
            role: Role::Assistant,
            text: String::new(),
            activities: vec![],
            insert_points: vec![],
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "watch -n 2 ls".into(),
            status: rsmarkdown_tui::activities::WatchStatus::Running,
            detail: None,
            duration_ms: 0,
            payload: None,
        });
        chat.drain_events();
        assert_eq!(chat.messages[0].activities.len(), 1);
        // 轮次 Idle + 终态 Done：同 label 原地更新，不新增活动。
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "watch -n 2 ls".into(),
            status: rsmarkdown_tui::activities::WatchStatus::Idle,
            detail: Some("第 2 轮".into()),
            duration_ms: 4000,
            payload: None,
        });
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "watch -n 2 ls".into(),
            status: rsmarkdown_tui::activities::WatchStatus::Done,
            detail: None,
            duration_ms: 9000,
            payload: Some(serde_json::json!("done output")),
        });
        chat.drain_events();
        assert_eq!(chat.messages[0].activities.len(), 1, "updates in place");
        let area = Rect { x: 0, y: 0, width: 120, height: 30 };
        let mut buf = Buffer::empty(area);
        chat.draw(area, &mut buf);
        let joined: String = (0..30)
            .map(|y| {
                (0..120)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("watch -n 2 ls"), "header: {joined}");
        assert!(joined.contains("✓"), "done glyph: {joined}");
        // 展开：payload 作为内容渲染。
        assert!(chat.toggle_transcript());
        let mut buf = Buffer::empty(area);
        chat.draw(area, &mut buf);
        let joined: String = (0..30)
            .map(|y| {
                (0..120)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("done output"), "expanded: {joined}");
    }

    #[test]
    fn bash_folds_into_group_with_count() {
        // CC fullscreen：普通 Bash 折叠为 bash 类，摘要 "Ran 2 bash commands"，
        // 且不与 Read/Search 互断。
        let mut chat = test_chat();
        chat.messages.push(UiMessage {
            role: Role::Assistant,
            text: String::new(),
            activities: vec![],
            insert_points: vec![],
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        chat.stream_msg = Some(0);
        for (name, input) in [
            ("Bash", json!({"command": "cargo test"})),
            ("Read", json!({"file_path": "a.md"})),
            ("Bash", json!({"command": "npm run build"})),
        ] {
            let _ = chat.events.send(UiEvent::ToolStart { name: name.into() });
            chat.drain_events();
            let _ = chat.events.send(UiEvent::ToolReady { name: name.into(), input });
            chat.drain_events();
        }
        assert_eq!(chat.messages[0].groups.len(), 1, "all fold into one group");
        let g = &chat.messages[0].groups[0];
        assert_eq!(g.bash, 2);
        assert_eq!(g.read_ops, 0);
        assert_eq!(g.read_paths, vec!["a.md".to_string()]);
        assert_eq!(collapse_summary(g, false), "Read 1 file, ran 2 bash commands");
        assert_eq!(collapse_summary(g, true), "Reading 1 file, running 2 bash commands…");
        // 全部工具完成后：hint 消失、摘要转过去时、bash 计数保留。
        for (summary, out) in [
            ("Bash $ cargo test", "ok"),
            ("Read a.md", "l1"),
            ("Bash $ npm run build", "done"),
        ] {
            let _ = chat.events.send(UiEvent::ToolDone(crate::query::ToolCallDone {
                name: summary.split(' ').next().unwrap().into(),
                summary: summary.into(),
                output: out.into(),
                is_error: false,
                diff: None,
                duration_ms: 1,
            }));
            chat.drain_events();
        }
        let area = Rect { x: 0, y: 0, width: 120, height: 30 };
        let mut buf = Buffer::empty(area);
        chat.draw(area, &mut buf);
        let joined: String = (0..30)
            .map(|y| {
                (0..120)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("Read 1 file, ran 2 bash commands"),
            "final summary: {joined}"
        );
    }

    #[test]
    fn running_group_shows_hint_line_then_hides_when_done() {
        // CC latestDisplayHint：执行中折叠组下方显示最近工具的输入（⎿ 行），
        // 组完成后消失。
        let mut chat = test_chat();
        chat.messages.push(UiMessage {
            role: Role::Assistant,
            text: String::new(),
            activities: vec![],
            insert_points: vec![],
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::ToolStart { name: "Read".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Read".into(),
            input: json!({"file_path": "package.json"}),
        });
        chat.drain_events();
        let area = Rect { x: 0, y: 0, width: 120, height: 30 };
        let mut buf = Buffer::empty(area);
        chat.draw(area, &mut buf);
        let joined: String = (0..30)
            .map(|y| {
                (0..120)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("⎿") && joined.contains("package.json"),
            "running group shows hint: {joined}"
        );
        // 工具完成 → 组不再 in_progress → hint 行消失，只剩过去时摘要。
        let _ = chat.events.send(UiEvent::ToolDone(crate::query::ToolCallDone {
            name: "Read".into(),
            summary: "Read package.json".into(),
            output: "l1".into(),
            is_error: false,
            diff: None,
            duration_ms: 3,
        }));
        chat.drain_events();
        let mut buf = Buffer::empty(area);
        chat.draw(area, &mut buf);
        let joined: String = (0..30)
            .map(|y| {
                (0..120)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("Read 1 file"), "past tense: {joined}");
        assert!(
            !joined.contains("⎿"),
            "hint hidden when group done: {joined}"
        );
    }

    #[test]
    fn round_end_starts_new_group_next_round() {
        // 回归：上一轮 Searching 没找到 → 下一轮（新一批 toolcall）的 Read
        // 不得聚合进旧组。RoundEnd 收口后新工具开新组。
        let mut chat = test_chat();
        chat.messages.push(UiMessage {
            role: Role::Assistant,
            text: String::new(),
            activities: vec![],
            insert_points: vec![],
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        chat.stream_msg = Some(0);
        // 轮 1：Searching（Grep 入组）
        let _ = chat.events.send(UiEvent::ToolStart { name: "Grep".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Grep".into(),
            input: json!({"pattern": "nomatch"}),
        });
        chat.drain_events();
        assert_eq!(chat.messages[0].groups.len(), 1, "round 1 group");
        // 轮 1 收口（工具执行完，下一轮模型响应前）
        let _ = chat.events.send(UiEvent::RoundEnd);
        chat.drain_events();
        // 轮 2：thinking（不断组语义不受影响）后新 Grep——必须开新组
        let _ = chat.events.send(UiEvent::ThinkingDelta("hmm".into()));
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolStart { name: "Grep".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Grep".into(),
            input: json!({"pattern": "another"}),
        });
        chat.drain_events();
        assert_eq!(chat.messages[0].groups.len(), 2, "round 2 opens new group");
        // 组 2 归属正确
        let idx = chat.messages[0].activities.len() - 1;
        assert_eq!(chat.messages[0].group_of[idx], Some(1));
        // 同轮内连续 Read 仍聚合（既有语义不破坏）
        let _ = chat.events.send(UiEvent::ToolStart { name: "Read".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Read".into(),
            input: json!({"file_path": "a.md"}),
        });
        chat.drain_events();
        assert_eq!(chat.messages[0].groups.len(), 2, "same-round Read joins group 2");
        let idx = chat.messages[0].activities.len() - 1;
        assert_eq!(chat.messages[0].group_of[idx], Some(1));
    }

    #[test]
    fn expand_running_then_complete_then_collapse_back() {
        let mut chat = test_chat();
        start_group(&mut chat);
        // 执行中：聚合行进行时
        assert!(visible(&mut chat).contains("Reading 2 files"), "running fold");
        // 执行中展开
        assert!(chat.toggle_transcript());
        assert!(!visible(&mut chat).contains("Reading 2 files"), "expanded");
        // 工具完成 + turn 结束（模拟真实顺序）
        for (summary, out) in [("Read a.md", "l1\nl2\nl3"), ("Read b.md", "x\ny")] {
            let _ = chat.events.send(UiEvent::ToolDone(crate::query::ToolCallDone {
                name: "Read".into(),
                summary: summary.into(),
                output: out.into(),
                is_error: false,
                duration_ms: 0,
            diff: None,
            }));
        }
        chat.drain_events();
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::TurnEnd);
        chat.drain_events();
        chat.stream_msg = None;
        // 折叠回：聚合行（过去时）
        assert!(chat.toggle_transcript());
        let collapsed = visible(&mut chat);
        assert!(
            collapsed.contains("Read 2 files"),
            "collapsed after turn: {collapsed}"
        );
    }

    #[test]
    fn click_expanded_group_head_collapses_back() {
        let mut chat = test_chat();
        start_group(&mut chat);
        fn group_rows(chat: &mut BingoChat) -> Vec<(u16, u16)> {
            let area = Rect { x: 0, y: 0, width: 120, height: 40 };
            let mut buf = Buffer::empty(area);
            chat.draw(area, &mut buf);
            chat.click_ranges
                .iter()
                .filter(|(_, _, a)| matches!(a, ClickAction::Group { .. }))
                .map(|(start, end, _)| (*start, *end))
                .collect()
        }
        let first_rows = group_rows(&mut chat);
        let fold_row = first_rows.first().expect("group fold row").0;
        assert!(chat.toggle_at(fold_row), "click expands");
        let head_row = group_rows(&mut chat).first().expect("group head row").0;
        assert!(head_row >= fold_row, "head row after fold row");
        assert!(chat.toggle_at(head_row), "click head collapses");
        let collapsed = visible(&mut chat);
        assert!(
            collapsed.contains("Reading 2 files"),
            "collapsed again: {collapsed}"
        );
    }

    #[test]
    fn collapse_after_expand_then_expand_again() {
        let mut chat = test_chat();
        start_group(&mut chat);
        chat.stream_msg = Some(0);
        for (summary, out) in [("Read a.md", "l1"), ("Read b.md", "x")] {
            let _ = chat.events.send(UiEvent::ToolDone(crate::query::ToolCallDone {
                name: "Read".into(),
                summary: summary.into(),
                output: out.into(),
                is_error: false,
                duration_ms: 0,
            diff: None,
            }));
        }
        chat.drain_events();
        chat.stream_msg = None;
        // 展开 → 折叠 → 再展开 → 再折叠（多次往返）
        for _ in 0..3 {
            assert!(chat.toggle_transcript());
            assert!(!visible(&mut chat).contains("Read 2 files"), "expanded state");
            assert!(chat.toggle_transcript());
            assert!(
                visible(&mut chat).contains("Read 2 files"),
                "collapsed state: {}",
                visible(&mut chat)
            );
        }
    }
}
