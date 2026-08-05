//! 聊天状态机：消息/活动/折叠组的增量模型 + 文档行构建。
//!
//! 移植自旧 `tui.rs` 的 `BingoChat`（ratatui 版）：事件处理语义、
//! 折叠判定、展开切换原样保留；`draw` 换成 [`Chat::build_rows`]，
//! 产出显示无关的样式化行文档，由 UI 层映射为 iocraft 元素。
//! 事件从通道（`UiEvent` / `AskRequest`）流入，键盘/鼠标经
//! [`Chat::on_key`] / [`Chat::doc_click`] 流入。

use std::collections::HashMap;
use std::sync::Arc;

use iocraft::prelude::{Color, KeyCode, KeyModifiers};
use rsmarkdown_core::{MarkdownProcessor, Renderer};
use tokio::sync::{mpsc, oneshot};

use crate::permission::PermissionMode;
use crate::query::{run_query, Session};
use crate::tui::activities::{
    activities_path_get_mut, diff_lines, layout_activity, Activity, ActivityKind, Diff,
    Thinking, ThinkingState, TodoItem, TodoStatus, ToolCall, ToolStatus, WatchCall,
    WatchStatus,
};
use crate::tui::line::{text_width, Line, SegStyle};
use crate::tui::markdown::MarkdownRenderer;
use crate::tui::theme::Theme;
use crate::tui::UiEvent;

/// 强制整屏清除重绘信号：doc 行数变化（内容整体位移）或回合收尾时置位，
/// UI 层 hook 消费（绕开 iocraft 行 diff 在行号位移时的残留）。
pub(crate) static FORCE_FULL_REDRAW: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 权限询问：请求 + 结果回执。
pub type AskRequest = (PermissionRequest, oneshot::Sender<DialogAction>);

/// 权限对话框结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogAction {
    /// 选项 `index`（0 起）被确认。
    Confirm(usize),
    /// 对话框被 Esc 取消。
    Cancel,
}

/// 要展示的权限请求。
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    /// 标题，如 `允许执行 Bash`。
    pub title: String,
    /// 标题下的说明。
    pub question: String,
    /// 编号选项（数字自动添加）。
    pub options: Vec<String>,
}

impl PermissionRequest {
    pub fn new(title: impl Into<String>, question: impl Into<String>, options: Vec<String>) -> Self {
        Self {
            title: title.into(),
            question: question.into(),
            options,
        }
    }
}

/// 文档中一行：样式化行 + 整行背景（用户气泡用）。
#[derive(Debug, Clone)]
pub struct Row {
    pub line: Line,
    /// 整行背景。
    pub bg: Option<Color>,
    /// 行内右侧留白（CC 用户气泡 paddingRight=1）。
    pub padding_right: usize,
}

impl Row {
    pub fn new(line: Line) -> Self {
        Self {
            line,
            bg: None,
            padding_right: 0,
        }
    }
}

/// 文档行的点击目标。
#[derive(Debug, Clone)]
pub enum ClickTarget {
    /// 折叠组行（折叠/展开组）。
    Group { message: usize, group: usize },
    /// 活动头部行（折叠/展开活动）。
    Activity { message: usize, path: Vec<usize> },
    /// 权限选项（按索引确认）。
    AskOption(usize),
}

/// 可点击行的文档坐标范围。
#[derive(Debug, Clone)]
pub struct ClickRange {
    pub start: usize,
    pub end: usize,
    pub target: ClickTarget,
}

/// 滚动文档：全部行 + 点击范围 + sticky 提示。
#[derive(Debug, Clone)]
pub struct Doc {
    pub rows: Vec<Row>,
    pub click_ranges: Vec<ClickRange>,
    /// 滚动离开底部时 sticky 提示文本（CC StickyPromptHeader）。
    pub sticky: Option<String>,
}

/// 一条会话消息（用户或 assistant 文本 + assistant 活动提示）。
#[derive(Debug, Clone)]
pub struct UiMessage {
    pub role: Role,
    pub text: String,
    pub activities: Vec<Activity>,
    /// activities[i] 创建时 text 的字符数：渲染时 text 与活动按模型输出顺序交错。
    pub insert_points: Vec<usize>,
    /// 连续 Read/Search 折叠组。
    pub groups: Vec<CollapseGroup>,
    /// activities[i] 所属折叠组索引（None = 独立活动）。
    pub group_of: Vec<Option<usize>>,
}

/// Read/Search 连续操作的折叠组：折叠为一行规则摘要（`Read 3 files`）。
#[derive(Debug, Clone)]
pub struct CollapseGroup {
    /// 组内活动索引（顺序）。
    pub activities: Vec<usize>,
    /// 搜索操作数。
    pub search: usize,
    /// Read 文件路径（去重计数）。
    pub read_paths: Vec<String>,
    /// 无路径的读取操作数。
    pub read_ops: usize,
    /// 列举操作数（ls/tree/du）。
    pub list: usize,
    /// 普通 Bash 操作数。
    pub bash: usize,
    /// 组仍开放（进行中 → 摘要用进行时 + …）。
    pub active: bool,
    /// ctrl+o / 点击展开组内逐工具。
    pub expanded: bool,
    /// 组内最近一个工具的输入 hint（执行中显示在 ⎿ 行）。
    pub last_hint: Option<String>,
}

/// 工具的可折叠分类（Claude Code isSearchOrReadCommand）。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CollapseKind {
    Search,
    /// Read 或读取类 Bash：携带文件路径（Bash 类为 None）。
    Read(Option<String>),
    List,
    /// 非搜索/读/列举的普通 Bash。
    Bash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// Read/Search 类工具判定。
pub fn classify_tool(name: &str, input: &serde_json::Value) -> Option<CollapseKind> {
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
                Some(CollapseKind::Bash)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// 命令是否含非中性段（纯 echo/printf 等不折叠）。
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

/// Bash 命令分类（按 && / || / | / ; 分段，跳过量词/重定向目标与中性命令，
/// 所有段都必须属于搜索/读取/列举集合；混合时按 list > search > read 归位）。
pub fn classify_bash_command(command: &str) -> Option<CollapseKind> {
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

/// 折叠组执行中的 hint：组内最近工具的输入。
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

/// 折叠组摘要文本：`Searched for 2 patterns, read 3 files`；
/// 进行中用进行时 + 末尾 …。
pub fn collapse_summary(g: &CollapseGroup, in_progress: bool) -> String {
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
            format!(
                " {} {}",
                g.search,
                if g.search == 1 { "pattern" } else { "patterns" }
            ),
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
            format!(
                " {} {}",
                read_count,
                if read_count == 1 { "file" } else { "files" }
            ),
        );
    }
    if g.list > 0 {
        push(
            "listed",
            "listing",
            format!(
                " {} {}",
                g.list,
                if g.list == 1 { "directory" } else { "directories" }
            ),
        );
    }
    if g.bash > 0 {
        push(
            "ran",
            "running",
            format!(
                " {} bash {}",
                g.bash,
                if g.bash == 1 { "command" } else { "commands" }
            ),
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

/// 展开态 1 行结果摘要（CC renderToolResultMessage）。
pub fn result_summary(name: &str, output: &str) -> Option<String> {
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

/// bingo 聊天组件状态：消息流 + 活动提示 + 输入 + 权限请求。
pub struct Chat {
    pub session: Arc<Session>,
    pub(super) events: mpsc::UnboundedSender<UiEvent>,
    pub asks: mpsc::UnboundedSender<AskRequest>,
    events_rx: mpsc::UnboundedReceiver<UiEvent>,
    asks_rx: mpsc::UnboundedReceiver<AskRequest>,
    pub messages: Vec<UiMessage>,
    pub input: String,
    pub typing: bool,
    pub busy: bool,
    /// 当前 assistant 消息索引。
    pub stream_msg: Option<usize>,
    thinking_buf: String,
    output_tokens: u64,
    pub tick: u64,
    /// TurnStart 时的 tick：运行态 thinking 的相对计时基准。
    turn_start_tick: u64,
    pub warnings: Vec<String>,
    pub user: String,
    pub cwd: String,
    pub pending_ask: Option<(PermissionRequest, oneshot::Sender<DialogAction>)>,
    /// 任务列表磁盘快照缓存（tick 周期刷新）。
    tasks_cache: Vec<TodoItem>,
    processor: MarkdownProcessor,
    renderer: MarkdownRenderer,
    reply_cache: HashMap<String, Vec<Line>>,
    /// 文档是否待重建（事件/tick/展开等写入后置位；布局层消费后清除）。
    pub dirty: bool,
    /// 上次 tick 时的文档行数（行号位移检测）。
    prev_doc_len: usize,
    pub width: usize,
    /// 视口行数（布局层写入；reconcile_scroll 用它钳制滚动）。
    pub viewport_height: usize,
    pub scroll: usize,
    pub auto_scroll: bool,
    /// 上次 build_rows 的文档（点击定位 + sticky）。
    pub doc: Doc,
    /// 等待 ToolReady（完整 input）归类的工具活动索引（FIFO）。
    pending_tools: Vec<usize>,
    pub theme: Theme,
    /// 任务区展开信号（Task 工具调用 → 展示任务列表）。
    pub tasks_visible: bool,
    /// 中断信号：busy 时 Ctrl+C / Esc → send(true)，回合内流读取立即中止。
    cancel_tx: tokio::sync::watch::Sender<bool>,
}

impl Chat {
    pub fn new(
        session: Arc<Session>,
        events: mpsc::UnboundedSender<UiEvent>,
        events_rx: mpsc::UnboundedReceiver<UiEvent>,
        asks: mpsc::UnboundedSender<AskRequest>,
        asks_rx: mpsc::UnboundedReceiver<AskRequest>,
        theme: Theme,
    ) -> Self {
        // Watchable 事件转发：registry 广播 → UiEvent 通道（跨回合常驻）。
        // 测试环境无 tokio runtime 时跳过。
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let watch_events = events.clone();
            let mut rx = session.watch.subscribe();
            handle.spawn(async move {
                loop {
                    let ev = match rx.recv().await {
                        Ok(ev) => ev,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    };
                    if watch_events
                        .send(UiEvent::WatchEvent {
                            label: ev.label,
                            status: match ev.state {
                                crate::watch::WatchState::Running => WatchStatus::Running,
                                crate::watch::WatchState::Idle => WatchStatus::Idle,
                                crate::watch::WatchState::Done => WatchStatus::Done,
                                crate::watch::WatchState::Failed => WatchStatus::Failed,
                                crate::watch::WatchState::Cancelled => WatchStatus::Cancelled,
                            },
                            detail: ev.detail,
                            duration_ms: ev.elapsed_ms,
                            payload: ev.payload,
                            signal: ev.signal,
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
            tasks_cache: Vec::new(),
            processor: MarkdownProcessor::default(),
            renderer: MarkdownRenderer::with_theme(80, theme.clone()),
            reply_cache: HashMap::new(),
            dirty: true,
            prev_doc_len: 0,
            width: 80,
            viewport_height: 24,
            scroll: 0,
            auto_scroll: true,
            doc: Doc {
                rows: Vec::new(),
                click_ranges: Vec::new(),
                sticky: None,
            },
            pending_tools: Vec::new(),
            theme,
            tasks_visible: false,
            cancel_tx: tokio::sync::watch::channel(false).0,
        }
    }

    /// 消费通道里所有待处理事件。返回是否处理了任何事件。
    pub fn drain_events(&mut self) -> bool {
        let mut handled = false;
        while let Ok(event) = self.events_rx.try_recv() {
            handled = true;
            self.handle(event);
        }
        handled
    }

    /// 消费权限通道（一次一个：新请求只在无待处理请求时接收）。
    pub fn drain_asks(&mut self) -> bool {
        if self.pending_ask.is_none()
            && let Ok(request) = self.asks_rx.try_recv()
        {
            self.pending_ask = Some(request);
            return true;
        }
        false
    }

    /// 消费所有通道。返回是否有任何新状态。
    pub fn drain_all(&mut self) -> bool {
        let mut changed = self.drain_events();
        changed |= self.drain_asks();
        if changed {
            self.dirty = true;
        }
        changed
    }

    fn handle(&mut self, event: UiEvent) {
        match event {
            UiEvent::TurnStart => {
                self.thinking_buf.clear();
                self.pending_tools_clear();
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
                // 运行态行立即可见。
                let mut hint = Activity::new(ActivityKind::Thinking(Thinking {
                    state: ThinkingState::Running,
                    duration_ms: 0,
                    stage: thinking_stage(self.messages.len()),
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
                    self.messages[i].text.push_str(&text);
                    if let Some(g) = self.messages[i].groups.last_mut() {
                        g.active = false;
                    }
                }
            }
            UiEvent::ThinkingDelta(thinking) => {
                if let Some(i) = self.stream_msg {
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
                        let dup = thinking == self.thinking_buf
                            || self.messages[i]
                                .activities
                                .iter()
                                .rev()
                                .find(|a| matches!(a.kind, ActivityKind::Thinking(_)))
                                .is_some_and(|a| {
                                    a.content.first().is_some_and(|l| l.plain_text() == thinking)
                                });
                        if dup {
                            return;
                        }
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
                            stage: thinking_stage(self.messages.len()),
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
            UiEvent::OutputTokens(tokens) => {
                self.output_tokens = tokens;
            }
            UiEvent::ToolStart { name } => {
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
                    self.pending_tools_push(idx);
                }
            }
            UiEvent::ToolReady { name, input } => {
                let Some(i) = self.stream_msg else { return };
                let Some(idx) = self.pending_tools_pop() else {
                    return;
                };
                if let ActivityKind::Tool(call) = &mut self.messages[i].activities[idx].kind {
                    call.summary = crate::query::summarize_input(&name, &input);
                }
                let kind = classify_tool(&name, &input);
                let Some(kind) = kind else {
                    if let Some(g) = self.messages[i].groups.last_mut() {
                        g.active = false;
                    }
                    return;
                };
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
                signal,
            } => {
                let found = self.messages.iter_mut().find_map(|m| {
                    m.activities.iter_mut().find(|a| {
                        matches!(&a.kind, ActivityKind::Watch(w) if w.label == *label)
                    })
                });
                if let Some(hint) = found {
                    if let ActivityKind::Watch(w) = &mut hint.kind {
                        w.status = status;
                        w.duration_ms = duration_ms;
                        if let Some(d) = &detail {
                            w.detail = Some(d.clone());
                        }
                    }
                    if let Some(text) = &payload.and_then(|p| p.as_str().map(str::to_string)) {
                        let content: Vec<Line> = text
                            .lines()
                            .filter(|l| !l.trim().is_empty())
                            .map(|l| Line::plain(l.to_string()))
                            .collect();
                        hint.set_content(content);
                    }
                } else {
                    let target = match self.stream_msg {
                        Some(i) => i,
                        None => match self
                            .messages
                            .iter()
                            .rposition(|m| m.role == Role::Assistant)
                        {
                            Some(i) => i,
                            None => return,
                        },
                    };
                    let mut hint = Activity::new(ActivityKind::Watch(WatchCall {
                        label: label.clone(),
                        status,
                        detail: detail.clone(),
                        duration_ms,
                    }));
                    hint.expand_hint = Some("ctrl+o to expand".to_string());
                    let text_len = self.messages[target].text.chars().count();
                    self.messages[target].activities.push(hint);
                    self.messages[target].insert_points.push(text_len);
                    self.messages[target].group_of.push(None);
                }
                let terminal = matches!(
                    status,
                    WatchStatus::Done | WatchStatus::Failed | WatchStatus::Cancelled
                );
                if terminal || signal.is_some() {
                    if let Some(sig) = &signal
                        && let Some(hint) = self.messages.iter_mut().find_map(|m| {
                            m.activities.iter_mut().find(|a| {
                                matches!(&a.kind, ActivityKind::Watch(w) if w.label == *label)
                            })
                        })
                        && let ActivityKind::Watch(w) = &mut hint.kind
                    {
                        w.detail = Some(sig.clone());
                    }
                    self.submit_auto();
                }
            }
            UiEvent::RoundEnd => {
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
                    return;
                }
                let group_of = self.messages[i].group_of.clone();
                for (hint_idx, hint) in self.messages[i].activities.iter_mut().enumerate() {
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
                            call.result_summary = result_summary(&done.name, &done.output);
                        } else {
                            let content: Vec<Line> = done
                                .output
                                .lines()
                                .filter(|l| !l.trim().is_empty())
                                .map(|l| Line::plain(l.to_string()))
                                .collect();
                            hint.set_content(content);
                        }
                        break;
                    }
                }
            }
            UiEvent::TurnEnd => {
                self.busy = false;
                self.output_tokens = 0;
                FORCE_FULL_REDRAW.store(true, std::sync::atomic::Ordering::Relaxed);
                if self.session.watch.has_wake_notifications() {
                    self.submit_auto();
                }
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
                        let old_to_new: HashMap<usize, usize> = keep
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

    #[cfg(test)]
    fn apply_turn_start(&mut self) {
        self.handle(UiEvent::TurnStart);
    }

    #[cfg(test)]
    fn apply_event(&mut self, event: UiEvent) {
        self.handle(event);
    }

    fn pending_tools_clear(&mut self) {
        self.pending_tools.clear();
    }
    fn pending_tools_push(&mut self, idx: usize) {
        self.pending_tools.push(idx);
    }
    fn pending_tools_pop(&mut self) -> Option<usize> {
        let first = self.pending_tools.first().copied();
        if first.is_some() {
            self.pending_tools.remove(0);
        }
        first
    }

    /// thinking 内容走 markdown streaming 渲染（代码块/列表随流更新）。
    /// 每次以完整文本重渲染（thinking 增量不大）。
    fn render_thinking(&mut self, text: &str) -> Vec<Line> {
        if text.is_empty() {
            return Vec::new();
        }
        self.renderer.set_width(self.width);
        let doc = self.processor.process_streaming(text);
        self.renderer.render(&doc);
        self.renderer.lines().to_vec()
    }

    /// 点击（doc 行号）命中的行 → 折叠/展开 / 权限选项确认。
    /// 返回是否处理了点击。
    pub fn doc_click(&mut self, doc_row: usize) -> bool {
        let Some(range) = self
            .doc
            .click_ranges
            .iter()
            .find(|r| doc_row >= r.start && doc_row < r.end)
        else {
            return false;
        };
        match &range.target {
            ClickTarget::Group { message, group } => {
                let Some(msg) = self.messages.get_mut(*message) else {
                    return false;
                };
                let Some(g) = msg.groups.get_mut(*group) else {
                    return false;
                };
                g.expanded = !g.expanded;
                self.auto_scroll = false;
                self.dirty = true;
                true
            }
            ClickTarget::Activity { message, path } => {
                let Some(msg) = self.messages.get_mut(*message) else {
                    return false;
                };
                if let Some(act) = activities_path_get_mut(&mut msg.activities, path) {
                    act.toggle();
                    self.auto_scroll = false;
                    self.dirty = true;
                    return true;
                }
                false
            }
            ClickTarget::AskOption(index) => {
                self.choose_ask_option(*index);
                true
            }
        }
    }

    /// ctrl+o：全局展开/折叠 transcript（CC app:toggleTranscript）。
    /// 优先级：展开的组先折叠回聚合态；否则有折叠项 → 全部展开；否则全部折叠。
    pub fn toggle_transcript(&mut self) -> bool {
        let Some(i) = self.messages.len().checked_sub(1) else {
            return false;
        };
        if self.messages[i].groups.iter().any(|g| g.expanded) {
            for g in &mut self.messages[i].groups {
                g.expanded = false;
            }
            self.auto_scroll = false;
            self.dirty = true;
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
        self.dirty = true;
        true
    }

    pub fn submit(&mut self) {
        let text = std::mem::take(&mut self.input);
        if text.trim().is_empty() || self.busy {
            self.input = text;
            return;
        }
        self.start_turn(text, true);
    }

    /// 系统触发回合：watchable 信号/终态通知唤醒主 agent。
    /// 无用户输入（通知在 run_query 首轮注入）；不管用户状态。
    fn submit_auto(&mut self) {
        if self.busy {
            return;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        self.start_turn(String::new(), false);
    }

    fn start_turn(&mut self, text: String, show_user: bool) {
        if show_user {
            self.messages.push(UiMessage {
                role: Role::User,
                text: text.clone(),
                activities: Vec::new(),
                insert_points: Vec::new(),
                groups: Vec::new(),
                group_of: Vec::new(),
            });
        }
        self.busy = true;
        // 新一轮开始前复位中断信号（同一 Sender 跨轮复用）。
        let _ = self.cancel_tx.send(false);

        let session = self.session.clone();
        let events = self.events.clone();
        let asks = self.asks.clone();
        let cancel_rx = self.cancel_tx.subscribe();
        tokio::spawn(async move {
            let _ = events.send(UiEvent::TurnStart);
            let mut ui = crate::tui::tui_hooks(events.clone(), asks);
            // 多轮连续性：加载 transcript 历史作为本轮上下文（每轮独立 run_query）。
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
                run_query(&session, history, &text, &mut ui, Some(cancel_rx)).await;
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

    /// 权限键盘输入：1..9 确认选项，Esc 取消。返回是否消费。
    pub fn ask_key(&mut self, code: KeyCode) -> bool {
        if self.pending_ask.is_none() {
            return false;
        }
        match code {
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                let index = (c as u8 - b'1') as usize;
                self.choose_ask_option(index);
                true
            }
            KeyCode::Esc => {
                if let Some((_, tx)) = self.pending_ask.take() {
                    let _ = tx.send(DialogAction::Cancel);
                }
                true
            }
            _ => false,
        }
    }

    fn choose_ask_option(&mut self, index: usize) {
        if let Some((request, tx)) = self.pending_ask.take() {
            if index < request.options.len() {
                let _ = tx.send(DialogAction::Confirm(index));
            } else {
                let _ = tx.send(DialogAction::Cancel);
            }
        }
    }

    /// 键盘事件（与旧 event() 语义一致；busy 时 Esc/Ctrl+C 中断回合）。
    /// 按键处理完强制整屏重绘：打字与流式渲染交错时，iocraft 行 diff
    /// 可能残留旧行（thinking/输入框双行）；全清重绘绕开。
    pub fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        FORCE_FULL_REDRAW.store(true, std::sync::atomic::Ordering::Relaxed);
        if self.ask_key(code) {
            return true;
        }
        if self.busy
            && (code == KeyCode::Esc
                || (code == KeyCode::Char('c')
                    && modifiers.contains(KeyModifiers::CONTROL)))
        {
            let _ = self.cancel_tx.send(true);
            return true;
        }
        if self.typing {
            match code {
                KeyCode::Char(c)
                    if !c.is_control() && !modifiers.contains(KeyModifiers::CONTROL) =>
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
        match code {
            KeyCode::Esc => {
                self.typing = !self.typing;
                true
            }
            KeyCode::Char('o') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_transcript();
                true
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.auto_scroll = false;
                self.scroll = self.scroll.saturating_add(1);
                self.reconcile_scroll(self.viewport_height);
                true
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.auto_scroll = false;
                self.scroll = self.scroll.saturating_sub(1);
                self.reconcile_scroll(self.viewport_height);
                true
            }
            KeyCode::PageDown => {
                self.auto_scroll = false;
                self.scroll = self.scroll.saturating_add(10);
                self.reconcile_scroll(self.viewport_height);
                true
            }
            KeyCode::PageUp => {
                self.auto_scroll = false;
                self.scroll = self.scroll.saturating_sub(10);
                self.reconcile_scroll(self.viewport_height);
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

    /// tick：spinner 帧与运行态 thinking 独立计时。
    pub fn tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        self.dirty = true;
        // 文档行数变化 → 内容行号位移（markdown wrap、消息增删）——
        // iocraft 行 diff 在此类位移下可能残留旧行，置位强制全清。
        let len = self.doc.rows.len();
        if len != self.prev_doc_len {
            self.prev_doc_len = len;
            FORCE_FULL_REDRAW.store(true, std::sync::atomic::Ordering::Relaxed);
        }
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

    /// 任务区数据源：磁盘 store 实时快照。
    pub fn tasks(&self) -> Vec<TodoItem> {
        self.session
            .tasks
            .list_ui()
            .into_iter()
            .map(|t| {
                let status = match t.status {
                    crate::tasks::TaskStatus::Pending => TodoStatus::Pending,
                    crate::tasks::TaskStatus::InProgress => TodoStatus::InProgress,
                    crate::tasks::TaskStatus::Completed => TodoStatus::Done,
                };
                TodoItem {
                    text: t.subject,
                    status,
                }
            })
            .collect()
    }

    /// 刷新任务缓存（磁盘快照；tick 周期 + 事件排空时调用）。
    pub fn refresh_tasks(&mut self) {
        self.tasks_cache = self.tasks();
    }

    /// 已完成项最多保留尾部几条，更老的折叠进 `… N done`。
    const DONE_SHOWN: usize = 3;
    /// 活动项窗口大小，超出折叠进 `… +N more`。
    const TODO_SHOWN: usize = 5;

    /// 任务区行（CC TaskListV2 位置：输入框上方）。
    /// 有展开信号且存在任务时显示；完成后自动隐藏。
    pub fn task_lines(&self) -> Vec<Line> {
        if !self.tasks_visible {
            return Vec::new();
        }
        let t = &self.tasks_cache;
        if t.is_empty() {
            return Vec::new();
        }
        let theme = &self.theme;
        let mut out = Vec::new();
        // 头部：`{spinner}todo · N/M tasks`
        let mut header = Line::empty();
        if t.iter().any(|i| i.status == TodoStatus::InProgress) {
            header.push_styled(
                format!("{} ", crate::tui::activities::spinner(self.tick)),
                theme.tool_running(),
            );
        }
        header.push_styled("todo".to_string(), theme.tool_running());
        let done = t.iter().filter(|i| i.status == TodoStatus::Done).count();
        header.push_styled(
            format!(" · {done}/{} tasks", t.len()),
            SegStyle::fg(theme.inactive),
        );
        out.push(header);
        let done_indices: Vec<usize> = t
            .iter()
            .enumerate()
            .filter(|(_, i)| i.status == TodoStatus::Done)
            .map(|(i, _)| i)
            .collect();
        let shown_done = done_indices.len().min(Self::DONE_SHOWN);
        let hidden_done = done_indices.len() - shown_done;
        if hidden_done > 0 {
            out.push(Line::styled(
                format!("… {} done", hidden_done),
                SegStyle::fg(theme.inactive),
            ));
        }
        for &idx in done_indices.iter().skip(hidden_done) {
            let mut line = Line::styled("[x] ", theme.task_done());
            line.push_styled(t[idx].text.clone(), theme.task_done());
            out.push(line);
        }
        let active: Vec<&TodoItem> = t
            .iter()
            .filter(|i| i.status != TodoStatus::Done)
            .collect();
        for item in active.iter().take(Self::TODO_SHOWN) {
            let (marker, style) = match item.status {
                TodoStatus::Pending => {
                    ("[ ] ".to_string(), theme.task_open())
                }
                TodoStatus::InProgress => (
                    format!("[{}] ", crate::tui::activities::spinner(self.tick)),
                    theme.tool_running(),
                ),
                TodoStatus::Done => unreachable!("filtered"),
            };
            let mut line = Line::styled(marker, style);
            line.push_styled(item.text.clone(), style);
            out.push(line);
        }
        if active.len() > Self::TODO_SHOWN {
            out.push(Line::styled(
                format!("… +{} more", active.len() - Self::TODO_SHOWN),
                SegStyle::fg(theme.inactive),
            ));
        }
        out
    }

    /// 权限模式标签（footer 徽标）。
    pub fn permission_mode_label(&self) -> &'static str {
        match self.session.permission_mode {
            PermissionMode::Default => "default",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::BypassPermissions => "bypassPermissions",
            PermissionMode::DontAsk => "dontAsk",
            PermissionMode::Plan => "plan",
        }
    }

    /// 滚动与文档一致性：clamp 滚动到文档末尾，auto_scroll 贴底。
    pub fn reconcile_scroll(&mut self, viewport: usize) {
        self.viewport_height = viewport;
        let total = self.doc.rows.len();
        let max_scroll = total.saturating_sub(viewport);
        if self.auto_scroll {
            self.scroll = max_scroll;
        }
        let scroll = self.scroll.min(max_scroll);
        self.scroll = scroll;
        if scroll == max_scroll {
            self.auto_scroll = true;
        }
    }

    /// 构建滚动文档：欢迎卡片 + 消息（text 与活动按插入点交错）+
    /// 权限请求块。
    pub fn build_rows(&mut self, width: usize) -> &Doc {
        let mut rows: Vec<Row> = Vec::new();
        let mut click_ranges: Vec<ClickRange> = Vec::new();
        let theme = self.theme.clone();

        rows.extend(welcome_card_rows(
            &theme,
            &self.user,
            &self.session.model,
            self.permission_mode_label(),
            &self.cwd,
            width,
        ));
        // 消息块间距（CC marginTop=1）：欢迎卡片后与每条消息前留一行。
        for i in 0..self.messages.len() {
            rows.push(Row::new(Line::empty()));
            match self.messages[i].role {
                Role::User => {
                    let mut line = Line::styled("❯ ", SegStyle::fg(theme.text));
                    line.push_styled(self.messages[i].text.clone(), SegStyle::fg(theme.text));
                    rows.push(Row {
                        line,
                        bg: Some(theme.user_message_bg),
                        padding_right: 1,
                    });
                }
                Role::Assistant => {
                    // markdown 渲染闭包：只借用互不相交的字段，避免与
                    // `self.messages` 的只读借用冲突。
                    let mut render = {
                        let processor = &mut self.processor;
                        let renderer = &mut self.renderer;
                        let cache = &mut self.reply_cache;
                        move |reply: &str| -> Vec<Line> {
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
                    let msg = &self.messages[i];
                    let text = &msg.text;
                    let char_bounds: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
                    let mut rendered_chars = 0usize;
                    let mut rendered_bytes = 0usize;
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
                            push_text(&theme, &mut rows, reply);
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
                            continue;
                        }
                        if let Some(g) = group_idx
                            && !msg.groups[g].expanded
                        {
                            // 折叠组：一行规则摘要（`Read 3 files (ctrl+o to expand)`）。
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
                            let spinner = crate::tui::activities::spinner(self.tick);
                            let mut line = Line::empty();
                            if msg.groups[g].active {
                                line.push_styled(
                                    format!("{spinner} "),
                                    SegStyle::fg(theme.thinking),
                                );
                            }
                            line.push_styled(summary, SegStyle::fg(theme.text));
                            line.push_styled(
                                " (ctrl+o to expand)".to_string(),
                                SegStyle::fg(theme.inactive),
                            );
                            let row = rows.len();
                            rows.push(Row::new(line));
                            click_ranges.push(ClickRange {
                                start: row,
                                end: row + 1,
                                target: ClickTarget::Group { message: i, group: g },
                            });
                            // 执行中的折叠组下方显示最近工具的输入（CC ⎿ 行）。
                            if in_progress
                                && let Some(hint) = &msg.groups[g].last_hint
                            {
                                rows.push(Row::new(Line::styled(
                                    format!("  ⎿  {hint}"),
                                    SegStyle::fg(theme.inactive),
                                )));
                            }
                            continue;
                        }
                        let (lines, mut local) = layout_activity(
                            act,
                            &[idx],
                            rows.len() as u16,
                            crate::tui::activities::spinner(self.tick),
                            &theme,
                            &mut |reply: &str| render(reply),
                        );
                        // 组展开态：组首工具行同时是聚合行的位置——点击它折叠回组。
                        if let Some(g) = group_idx
                            && let Some(first) = local.first()
                        {
                            click_ranges.push(ClickRange {
                                start: first.start as usize,
                                end: first.end as usize,
                                target: ClickTarget::Group { message: i, group: g },
                            });
                        }
                        for line in lines {
                            rows.push(Row::new(line));
                        }
                        for range in &mut local {
                            click_ranges.push(ClickRange {
                                start: range.start as usize,
                                end: range.end as usize,
                                target: ClickTarget::Activity {
                                    message: i,
                                    path: range.path.clone(),
                                },
                            });
                        }
                    }
                    if rendered_bytes < text.len() {
                        let reply = render(&text[rendered_bytes..]);
                        push_text(&theme, &mut rows, reply);
                    }
                }
            }
        }

        // 权限请求块：标题 + 说明 + 编号选项（每行可点击）。
        if let Some((request, _)) = &self.pending_ask {
            let mut title = Line::styled("⏺ ", SegStyle::fg(theme.text));
            title.push_styled(request.title.clone(), theme.permission());
            rows.push(Row::new(title));
            rows.push(Row::new(Line::styled(
                format!("  {}", request.question),
                SegStyle::fg(theme.inactive),
            )));
            for (opt_idx, option) in request.options.iter().enumerate() {
                let mut line = Line::styled(
                    format!("  [{}] ", opt_idx + 1),
                    SegStyle::fg(theme.text),
                );
                line.push_styled(option.clone(), SegStyle::fg(theme.text));
                let row = rows.len();
                rows.push(Row::new(line));
                click_ranges.push(ClickRange {
                    start: row,
                    end: row + 1,
                    target: ClickTarget::AskOption(opt_idx),
                });
            }
        }

        // sticky：滚动离开底部且存在用户消息时，显示首条用户消息文本。
        let sticky = if self.scroll > 0 {
            self.messages
                .iter()
                .find(|m| m.role == Role::User && !m.text.trim().is_empty())
                .map(|m| m.text.split_whitespace().collect::<Vec<_>>().join(" "))
        } else {
            None
        };
        let sticky = sticky.map(|s| crate::tui::markdown::truncate(&s, width));

        self.doc = Doc {
            rows,
            click_ranges,
            sticky,
        };
        // 行数变化 → 内容行号位移（markdown wrap、消息增删）——iocraft
        // 行 diff 在此类位移下可能残留旧行。tick 里的检测读到的是上一帧
        // doc，事件驱动的重建（ThinkingDelta/TextDelta）会先于 tick 用
        // diff 渲染残留帧；这里在重建现场立即置位，堵住该窗口。
        let len = self.doc.rows.len();
        if len != self.prev_doc_len {
            self.prev_doc_len = len;
            FORCE_FULL_REDRAW.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        &self.doc
    }

}

/// text 段折叠：段 >2 行时折叠为首 2 行 + 提示（CC `… +N lines`）。
fn push_text(theme: &Theme, rows: &mut Vec<Row>, reply: Vec<Line>) {
    let claude = theme.claude;
    for (j, line) in reply.into_iter().enumerate() {
        if j == 0 {
            let mut styled = Line::styled("⏺ ", SegStyle::fg(claude));
            styled.segs.extend(line.segs);
            rows.push(Row::new(styled));
        } else {
            rows.push(Row::new(line));
        }
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
    left: Option<(String, Color)>,
    right: Option<(String, Color)>,
) -> Line {
    let mut line = Line::empty();
    let (l_text, l_color) = left.unwrap_or_else(|| (String::new(), theme.text));
    let l_len = text_width(&l_text);
    line.push_styled(l_text, SegStyle::fg(l_color));
    line.push_styled(
        format!("{}│", " ".repeat(left_w.saturating_sub(l_len))),
        SegStyle::fg(theme.inactive),
    );
    let r_width = if let Some((r_text, r_color)) = &right {
        line.push_styled(r_text.clone(), SegStyle::fg(*r_color));
        text_width(r_text)
    } else {
        0
    };
    line.push_styled(
        " ".repeat(right_w.saturating_sub(r_width)),
        SegStyle::fg(theme.inactive),
    );
    line
}

/// 欢迎面板（启动横幅）：左栏 logo/欢迎/身份，右栏 Tips 与 What's new。
fn welcome_rows(
    theme: &Theme,
    user: &str,
    model: &str,
    mode: &str,
    cwd: &str,
    width: usize,
) -> Vec<Line> {
    let left_w = width * 3 / 5;
    let right_w = width.saturating_sub(left_w + 1);
    let accent = theme.claude;
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
        Some((center(&format!("Welcome back {user}!"), left_w), accent)),
        Some(("Tips for getting started".to_string(), accent)),
    ));
    rows.push(column_row(theme, left_w, right_w, None, None));
    rows.push(column_row(
        theme,
        left_w,
        right_w,
        Some((center(&format!("{model} · {mode}"), left_w), theme.inactive)),
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
        Some((center(cwd, left_w), theme.inactive)),
        Some(("MCP 服务配置在 settings.json".to_string(), theme.text)),
    ));
    rows.push(column_row(
        theme,
        left_w,
        right_w,
        None,
        Some(("─".repeat(right_w).to_string(), theme.inactive)),
    ));
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

/// 欢迎卡片行（带 ╭╮ 边框），作为滚动内容的一部分。
fn welcome_card_rows(
    theme: &Theme,
    user: &str,
    model: &str,
    mode: &str,
    cwd: &str,
    width: usize,
) -> Vec<Row> {
    let gray = SegStyle::fg(theme.inactive);
    let title = format!(" bingo v0.1.0 · {model} ");
    let title_len = title.chars().count();
    let mut rows = Vec::new();
    rows.push(Row::new(Line::styled(
        format!(
            "╭{}{}╮",
            title,
            "─".repeat(width.saturating_sub(title_len + 2))
        ),
        gray,
    )));
    let inner_w = width.saturating_sub(2);
    for line in welcome_rows(theme, user, model, mode, cwd, inner_w) {
        let mut styled = Line::styled("│", gray);
        styled.segs.extend(line.segs);
        styled.push_styled("│", gray);
        rows.push(Row::new(styled));
    }
    rows.push(Row::new(Line::styled(
        format!("╰{}╯", "─".repeat(width.saturating_sub(2))),
        gray,
    )));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 测试用 Chat：独立通道 + 完整 Session。
    pub(super) fn test_chat() -> Chat {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (asks_tx, asks_rx) = mpsc::unbounded_channel();
        let session = Arc::new(Session {
            client: crate::api::client::Client::new(
                "test-key".to_string(),
                "https://example.com".to_string(),
            ),
            model: "test-model".to_string(),
            permission_mode: PermissionMode::Default,
            settings: crate::settings::Settings::default(),
            system: Vec::new(),
            transcript: None,
            depth: 0,
            home: std::env::temp_dir(),
            quiet: true,
            compact_failures: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(
                &std::env::temp_dir(),
                &std::env::temp_dir(),
            )),
            last_task_reminder_turn: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            expand_tasks: tokio::sync::watch::channel(false).0,
        });
        Chat::new(session, events_tx, events_rx, asks_tx, asks_rx, Theme::dark())
    }

    fn tool_activity() -> Activity {
        let mut hint = Activity::new(ActivityKind::Tool(ToolCall::running("Bash", "")));
        hint.set_content(vec![
            Line::plain("output line 1"),
            Line::plain("output line 2"),
        ]);
        hint.expand_hint = Some("ctrl+o to expand".to_string());
        hint
    }

    fn msg(role: Role, text: &str) -> UiMessage {
        UiMessage {
            role,
            text: text.to_string(),
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        }
    }

    /// 模拟组件层：build_rows + 滚动 + 视口切片 → 可见文本。
    fn visible(chat: &mut Chat, width: usize, height: usize) -> String {
        chat.build_rows(width);
        chat.reconcile_scroll(height.saturating_sub(3));
        let scroll = chat.scroll;
        let rows: Vec<String> = chat
            .doc
            .rows
            .iter()
            .skip(scroll)
            .take(height.saturating_sub(3))
            .map(|r| r.line.plain_text())
            .filter(|l| !l.trim().is_empty())
            .collect();
        rows.join("\n")
    }

    fn start_group(chat: &mut Chat) {
        chat.messages.push(msg(Role::Assistant, ""));
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

    fn finish_turn(chat: &mut Chat) {
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::TurnEnd);
        chat.drain_events();
        chat.stream_msg = None;
    }

    /// start_group + 工具完成（带显式摘要，如旧 build_group_chat(true)）。
    fn start_group_done(chat: &mut Chat) {
        start_group(chat);
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

    #[tokio::test]
    async fn chat_tasks_reflect_store_changes() {
        // TUI 任务区数据源 = 磁盘 store 实时快照（tick 广播链路的数据层）。
        let mut chat = test_chat();
        assert!(chat.tasks().is_empty());
        let store = chat.session.tasks.clone();
        let id = store
            .create(&crate::tasks::Task {
                id: String::new(),
                subject: "fix flicker".into(),
                description: String::new(),
                active_form: None,
                status: crate::tasks::TaskStatus::Pending,
                owner: None,
                blocks: Vec::new(),
                blocked_by: Vec::new(),
                metadata: Default::default(),
            })
            .await
            .unwrap();
        chat.refresh_tasks();
        assert_eq!(chat.tasks_cache.len(), 1);
        assert_eq!(chat.tasks_cache[0].text, "fix flicker");
        store
            .update(
                &id,
                &crate::tasks::TaskPatch {
                    status: Some(crate::tasks::TaskStatus::InProgress),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        chat.refresh_tasks();
        assert_eq!(chat.tasks_cache[0].status, TodoStatus::InProgress);
        store.delete(&id).await.unwrap();
        chat.refresh_tasks();
        assert!(chat.tasks_cache.is_empty());
    }

    #[test]
    fn click_toggles_tool_activity() {
        let mut chat = test_chat();
        chat.messages.push(UiMessage {
            activities: vec![tool_activity()],
            ..msg(Role::Assistant, "reply")
        });
        chat.build_rows(100);
        assert!(!chat.doc.click_ranges.is_empty(), "build_rows populates ranges");

        let start = {
            let range = &chat.doc.click_ranges[0];
            assert!(matches!(
                &range.target,
                ClickTarget::Activity { path, .. } if path == &vec![0]
            ));
            range.start
        };
        assert!(chat.doc_click(start), "click on header expands");
        assert!(chat.messages[0].activities[0].expanded);
        assert!(chat.doc_click(start), "click collapses again");
        assert!(!chat.messages[0].activities[0].expanded);
    }

    #[test]
    fn click_outside_ranges_is_noop() {
        let mut chat = test_chat();
        chat.messages.push(UiMessage {
            activities: vec![tool_activity()],
            ..msg(Role::Assistant, "reply")
        });
        chat.build_rows(100);
        assert!(!chat.doc_click(999), "no range -> no toggle");
    }

    fn thinking_text(hint: &Activity) -> String {
        hint.content
            .iter()
            .map(|l| l.plain_text().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_string()
    }

    /// 多轮 thinking：工具轮后的 delta 必须开新块，后续 delta 续写到新块。
    #[test]
    fn tool_turn_thinking_blocks_stay_separate() {
        let mut chat = test_chat();
        chat.apply_turn_start();
        chat.apply_event(UiEvent::ThinkingDelta("plan the fetch".into()));
        chat.apply_event(UiEvent::ToolStart { name: "WebFetch".into() });
        chat.apply_event(UiEvent::ThinkingDelta("got it".into()));
        chat.apply_event(UiEvent::ThinkingDelta(", summarizing".into()));

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
        chat.apply_turn_start();
        chat.apply_event(UiEvent::ThinkingDelta("a".into()));
        chat.apply_event(UiEvent::ThinkingDelta("b".into()));

        let acts = &chat.messages[0].activities;
        assert_eq!(acts.len(), 1);
        assert_eq!(thinking_text(&acts[0]), "ab");
    }

    /// 交错渲染：text 与活动按插入点交叉（模型输出 text → tool → text 顺序）。
    #[test]
    fn interleaves_text_and_activities_in_order() {
        let mut chat = test_chat();
        chat.messages.push(UiMessage {
            text: "hello world".to_string(),
            activities: vec![tool_activity()],
            insert_points: vec![5],
            ..msg(Role::Assistant, "")
        });
        let joined = visible(&mut chat, 100, 40);
        let hello = joined.find("hello").expect("first text before tool");
        let tool = joined.find("Bash").expect("tool row");
        let world = joined.find("world").expect("trailing text after tool");
        assert!(hello < tool, "text before tool: {joined}");
        assert!(tool < world, "tool before trailing text: {joined}");
    }

    // ------------------------------------------------------------------
    // 折叠分类与摘要（原 fold_tests）
    // ------------------------------------------------------------------

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
        assert_eq!(classify_bash_command("ls -la ."), Some(CollapseKind::List));
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
        assert_eq!(
            classify_tool("Grep", &json!({"pattern": "x"})),
            Some(CollapseKind::Search)
        );
        assert_eq!(
            classify_tool("Glob", &json!({"glob": "**/*.rs"})),
            Some(CollapseKind::Search)
        );
        assert_eq!(
            classify_tool("Bash", &json!({"command": "git log"})),
            Some(CollapseKind::Bash)
        );
        assert_eq!(classify_tool("Bash", &json!({"command": "echo hi"})), None);
        assert_eq!(
            classify_tool("Bash", &json!({"command": "cargo test && echo done"})),
            Some(CollapseKind::Bash)
        );
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

    // ------------------------------------------------------------------
    // 折叠渲染（原 fold_render_tests / fold_toggle_tests / 部分 live）
    // ------------------------------------------------------------------

    #[test]
    fn parallel_reads_collapse_to_one_line() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
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
        let joined = visible(&mut chat, 120, 20);
        assert!(joined.contains("Reading 2 files"), "active summary: {joined}");
        assert!(joined.contains("ctrl+o to expand"), "fold hint: {joined}");
        assert!(!joined.contains("a.md"), "paths hidden when collapsed: {joined}");
    }

    #[test]
    fn group_done_uses_past_tense() {
        let mut chat = test_chat();
        start_group(&mut chat);
        finish_turn(&mut chat);
        let joined = visible(&mut chat, 120, 20);
        assert!(joined.contains("Read 2 files"), "past tense: {joined}");
    }

    #[test]
    fn ctrl_o_expands_group_to_individual_tools() {
        let mut chat = test_chat();
        start_group(&mut chat);
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
        assert!(chat.toggle_transcript());
        let joined = visible(&mut chat, 120, 30);
        assert!(joined.contains("Read a.md"), "expanded first tool: {joined}");
        assert!(joined.contains("Read b.md"), "expanded second tool: {joined}");
        assert!(joined.contains("Read 3 lines"), "result summary row: {joined}");
        assert!(!joined.contains("Reading 2 files"), "no collapse line: {joined}");
    }

    #[test]
    fn non_collapsible_tool_breaks_group() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
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
        let joined = visible(&mut chat, 120, 20);
        assert!(joined.contains("Read 1 file"), "group rendered: {joined}");
        assert!(joined.contains("WebSearch"), "websearch independent: {joined}");
        assert!(!joined.contains("Reading"), "group closed by websearch: {joined}");
    }

    #[test]
    fn tool_after_thinking_placeholder_groups_without_panic() {
        // 回归：TurnStart 占位 thinking 后接工具——group_of 必须与 activities 同步。
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        chat.apply_turn_start();
        let _ = chat.events.send(UiEvent::ToolStart { name: "Read".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Read".into(),
            input: json!({"file_path": "a.md"}),
        });
        chat.drain_events();
        let joined = visible(&mut chat, 120, 30);
        assert!(joined.contains("Reading 1 file"), "group row: {joined}");
    }

    #[test]
    fn interleaved_group_keeps_text_position() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
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
        let joined = visible(&mut chat, 120, 20);
        let text_pos = joined.find("let me read").expect("text");
        let group_pos = joined.find("Reading 1 file").expect("group line");
        assert!(text_pos < group_pos, "text before group: {joined}");
    }

    #[test]
    fn ctrl_o_round_trip_collapses_group_back() {
        let mut chat = test_chat();
        start_group_done(&mut chat);
        finish_turn(&mut chat);
        assert!(visible(&mut chat, 120, 40).contains("Read 2 files"), "collapsed first");
        assert!(chat.toggle_transcript());
        let expanded = visible(&mut chat, 120, 40);
        assert!(expanded.contains("Read a.md"), "expanded: {expanded}");
        assert!(!expanded.contains("Read 2 files"), "no collapse line: {expanded}");
        assert!(chat.toggle_transcript());
        let collapsed = visible(&mut chat, 120, 40);
        assert!(collapsed.contains("Read 2 files"), "collapsed again: {collapsed}");
        assert!(!collapsed.contains("Read a.md"), "tools hidden: {collapsed}");
    }

    #[test]
    fn click_group_then_ctrl_o_collapses() {
        let mut chat = test_chat();
        start_group_done(&mut chat);
        finish_turn(&mut chat);
        chat.build_rows(120);
        // 点击组折叠行展开
        let row = chat
            .doc
            .click_ranges
            .iter()
            .find(|r| matches!(r.target, ClickTarget::Group { .. }))
            .map(|r| r.start)
            .expect("group fold row");
        assert!(chat.doc_click(row), "click expands group");
        let expanded = visible(&mut chat, 120, 40);
        assert!(expanded.contains("Read a.md"), "click expanded: {expanded}");
        // ctrl+o 折叠回
        assert!(chat.toggle_transcript());
        let collapsed = visible(&mut chat, 120, 40);
        assert!(collapsed.contains("Read 2 files"), "ctrl+o collapsed: {collapsed}");
    }

    #[test]
    fn running_tool_shows_input_summary_after_ready() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::ToolStart { name: "Agent".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Agent".into(),
            input: json!({"description": "读取项目说明并总结", "prompt": "..."}),
        });
        chat.drain_events();
        let joined = visible(&mut chat, 120, 30);
        let flat = joined.replace(' ', "");
        assert!(
            flat.contains("description=\"读取项目说明并总结\""),
            "running header shows input summary: {joined}"
        );
        // 完成后 duration 用真实值
        let _ = chat.events.send(UiEvent::ToolDone(crate::query::ToolCallDone {
            name: "Agent".into(),
            summary: "Agent description=\"读取项目说明并总结\"".into(),
            output: "line".into(),
            is_error: false,
            diff: None,
            duration_ms: 3210,
        }));
        chat.drain_events();
        let joined = visible(&mut chat, 120, 30);
        assert!(joined.contains("3210ms"), "real duration: {joined}");
    }

    #[tokio::test]
    async fn terminal_watch_event_triggers_auto_turn_when_idle() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "Agent: 长任务".into(),
            status: WatchStatus::Running,
            detail: None,
            duration_ms: 0,
            payload: None,
            signal: None,
        });
        chat.drain_events();
        assert!(!chat.busy);
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "Agent: 长任务".into(),
            status: WatchStatus::Done,
            detail: Some("完成".into()),
            duration_ms: 30000,
            payload: Some(serde_json::json!("结果")),
            signal: None,
        });
        chat.drain_events();
        tokio::task::yield_now().await;
        chat.drain_events();
        assert!(chat.busy, "auto turn started");
        assert_eq!(chat.messages.len(), 2, "new message for auto turn");
    }

    #[tokio::test]
    async fn signal_triggers_auto_turn_even_while_typing() {
        let mut chat = test_chat();
        chat.input = "我还在打字".to_string();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "tail -f app.log".into(),
            status: WatchStatus::Running,
            detail: None,
            duration_ms: 0,
            payload: None,
            signal: None,
        });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "tail -f app.log".into(),
            status: WatchStatus::Running,
            detail: Some("发现 1 个错误".into()),
            duration_ms: 12000,
            payload: None,
            signal: Some("发现错误：ERROR boom".into()),
        });
        chat.drain_events();
        tokio::task::yield_now().await;
        chat.drain_events();
        assert!(chat.busy, "signal wakes despite typing");
        assert_eq!(chat.input, "我还在打字", "input preserved");
    }

    /// 测试用 watchable：状态恒 Running。
    struct FakeWatchable;

    impl crate::watch::Watchable for FakeWatchable {
        fn label(&self) -> String {
            "fake".to_string()
        }
        fn poll(&self) -> crate::watch::WatchPoll {
            crate::watch::WatchPoll {
                state: crate::watch::WatchState::Running,
                detail: None,
                payload: None,
                signal: None,
            }
        }
        fn check_interval(&self) -> Option<std::time::Duration> {
            None
        }
    }

    #[tokio::test]
    async fn turn_end_triggers_auto_turn_when_wake_notification_pending() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        chat.busy = true;
        let watch = chat.session.watch.clone();
        let id = watch.register_with_conditions(Box::new(FakeWatchable), Vec::new());
        watch.set_state(id, crate::watch::WatchState::Done, Some("完成".into()), None);
        assert!(watch.has_wake_notifications(), "notification queued");
        chat.drain_events();
        assert!(chat.busy, "still busy, no auto turn mid-turn");
        let _ = chat.events.send(UiEvent::TurnEnd);
        chat.drain_events();
        tokio::task::yield_now().await;
        chat.drain_events();
        assert!(chat.busy, "auto turn started after TurnEnd");
        assert_eq!(chat.messages.len(), 2, "new message for wake turn");
    }

    #[tokio::test]
    async fn draw_with_long_cjk_stream_and_activities_does_not_panic() {
        let mut chat = test_chat();
        chat.apply_turn_start();
        let big = "clippy 基线在后台跑（任务 2）。以下是汇总与优化清单。\n\n---\n\n## 项目概览（子代理汇总）\n\n**bingo** 是 Rust 实现的本地 agent CLI。\n\n- **两种运行方式**：交互式 TUI 与 headless `--print`\n- **9 个内置工具** + MCP（stdio）适配；权限门五模式\n- **核心分层**：`api/`、`tool/`、`query.rs`、`tui.rs`\n- **watch 机制**：后台命令/子代理状态机\n";
        for chunk in big.chars().collect::<Vec<_>>().chunks(120) {
            let t: String = chunk.iter().collect();
            let _ = chat.events.send(UiEvent::TextDelta(t));
            chat.drain_events();
        }
        let _ = chat.events.send(UiEvent::ToolStart { name: "Bash".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Bash".into(),
            input: json!({"command": "cargo clippy"}),
        });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "Agent: 核查".into(),
            status: WatchStatus::Running,
            detail: Some("已产出 100 字符".into()),
            duration_ms: 5000,
            payload: None,
            signal: None,
        });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::TextDelta("后续正文，还有中文，继续。".into()));
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolDone(crate::query::ToolCallDone {
            name: "Bash".into(),
            summary: "$ cargo clippy".into(),
            output: "ok".into(),
            is_error: false,
            diff: None,
            duration_ms: 3000,
        }));
        chat.drain_events();
        let _ = chat.events.send(UiEvent::TurnEnd);
        chat.drain_events();
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "Agent: 核查".into(),
            status: WatchStatus::Done,
            detail: Some("完成".into()),
            duration_ms: 30000,
            payload: None,
            signal: None,
        });
        chat.drain_events();
        visible(&mut chat, 120, 40);
        assert_eq!(chat.messages.len(), 1, "single message rendered");
    }

    #[test]
    fn watch_event_updates_across_messages_in_place() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "Agent: 探索".into(),
            status: WatchStatus::Running,
            detail: None,
            duration_ms: 0,
            payload: None,
            signal: None,
        });
        chat.drain_events();
        assert_eq!(chat.messages[0].activities.len(), 1);
        let _ = chat.events.send(UiEvent::TurnEnd);
        chat.drain_events();
        chat.stream_msg = None;
        chat.messages.push(msg(Role::Assistant, ""));
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "Agent: 探索".into(),
            status: WatchStatus::Done,
            detail: Some("完成".into()),
            duration_ms: 40000,
            payload: None,
            signal: None,
        });
        chat.drain_events();
        assert_eq!(chat.messages[0].activities.len(), 1, "updated in place");
        assert_eq!(chat.messages[1].activities.len(), 0, "no new row at bottom");
        let w = match &chat.messages[0].activities[0].kind {
            ActivityKind::Watch(w) => w,
            _ => unreachable!(),
        };
        assert_eq!(w.status, WatchStatus::Done, "in-place status change");
    }

    #[test]
    fn idle_round_notification_does_not_trigger_auto_turn() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "watch ls".into(),
            status: WatchStatus::Idle,
            detail: Some("第 1 轮".into()),
            duration_ms: 5000,
            payload: None,
            signal: None,
        });
        chat.drain_events();
        assert!(!chat.busy, "idle round does not wake");
        assert_eq!(chat.messages.len(), 1);
    }

    #[test]
    fn watch_event_renders_inline_and_updates() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "watch -n 2 ls".into(),
            status: WatchStatus::Running,
            detail: None,
            duration_ms: 0,
            payload: None,
            signal: None,
        });
        chat.drain_events();
        assert_eq!(chat.messages[0].activities.len(), 1);
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "watch -n 2 ls".into(),
            status: WatchStatus::Idle,
            detail: Some("第 2 轮".into()),
            duration_ms: 4000,
            payload: None,
            signal: None,
        });
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "watch -n 2 ls".into(),
            status: WatchStatus::Done,
            detail: None,
            duration_ms: 9000,
            payload: Some(serde_json::json!("done output")),
            signal: None,
        });
        chat.drain_events();
        assert_eq!(chat.messages[0].activities.len(), 1, "updates in place");
        let joined = visible(&mut chat, 120, 30);
        assert!(joined.contains("watch -n 2 ls"), "header: {joined}");
        assert!(joined.contains("✓"), "done glyph: {joined}");
        assert!(chat.toggle_transcript());
        let joined = visible(&mut chat, 120, 30);
        assert!(joined.contains("done output"), "expanded: {joined}");
    }

    #[test]
    fn bash_folds_into_group_with_count() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
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
        let joined = visible(&mut chat, 120, 30);
        assert!(
            joined.contains("Read 1 file, ran 2 bash commands"),
            "final summary: {joined}"
        );
    }

    #[test]
    fn running_group_shows_hint_line_then_hides_when_done() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::ToolStart { name: "Read".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Read".into(),
            input: json!({"file_path": "package.json"}),
        });
        chat.drain_events();
        let joined = visible(&mut chat, 120, 30);
        assert!(
            joined.contains("⎿") && joined.contains("package.json"),
            "running group shows hint: {joined}"
        );
        let _ = chat.events.send(UiEvent::ToolDone(crate::query::ToolCallDone {
            name: "Read".into(),
            summary: "Read package.json".into(),
            output: "l1".into(),
            is_error: false,
            diff: None,
            duration_ms: 3,
        }));
        chat.drain_events();
        let joined = visible(&mut chat, 120, 30);
        assert!(joined.contains("Read 1 file"), "past tense: {joined}");
        assert!(!joined.contains("⎿"), "hint hidden when group done: {joined}");
    }

    #[test]
    fn round_end_starts_new_group_next_round() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::ToolStart { name: "Grep".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Grep".into(),
            input: json!({"pattern": "nomatch"}),
        });
        chat.drain_events();
        assert_eq!(chat.messages[0].groups.len(), 1, "round 1 group");
        let _ = chat.events.send(UiEvent::RoundEnd);
        chat.drain_events();
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
        let idx = chat.messages[0].activities.len() - 1;
        assert_eq!(chat.messages[0].group_of[idx], Some(1));
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
        assert!(visible(&mut chat, 120, 40).contains("Reading 2 files"), "running fold");
        assert!(chat.toggle_transcript());
        assert!(!visible(&mut chat, 120, 40).contains("Reading 2 files"), "expanded");
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
        finish_turn(&mut chat);
        assert!(chat.toggle_transcript());
        let collapsed = visible(&mut chat, 120, 40);
        assert!(collapsed.contains("Read 2 files"), "collapsed after turn: {collapsed}");
    }

    #[test]
    fn click_expanded_group_head_collapses_back() {
        let mut chat = test_chat();
        start_group(&mut chat);
        chat.build_rows(120);
        let fold_row = chat
            .doc
            .click_ranges
            .iter()
            .find(|r| matches!(r.target, ClickTarget::Group { .. }))
            .map(|r| r.start)
            .expect("group fold row");
        assert!(chat.doc_click(fold_row), "click expands");
        chat.build_rows(120);
        let head_row = chat
            .doc
            .click_ranges
            .iter()
            .find(|r| matches!(r.target, ClickTarget::Group { .. }))
            .map(|r| r.start)
            .expect("group head row");
        assert!(head_row >= fold_row, "head row after fold row");
        assert!(chat.doc_click(head_row), "click head collapses");
        let collapsed = visible(&mut chat, 120, 40);
        assert!(collapsed.contains("Reading 2 files"), "collapsed again: {collapsed}");
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
        for _ in 0..3 {
            assert!(chat.toggle_transcript());
            assert!(!visible(&mut chat, 120, 40).contains("Read 2 files"), "expanded state");
            assert!(chat.toggle_transcript());
            assert!(
                visible(&mut chat, 120, 40).contains("Read 2 files"),
                "collapsed state"
            );
        }
    }

    #[test]
    fn user_message_has_bubble_background() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::User, "hello"));
        chat.build_rows(100);
        let row = chat.doc.rows.iter().find(|r| r.line.plain_text().starts_with("❯"));
        assert!(row.is_some(), "user row rendered");
        assert_eq!(row.unwrap().bg, Some(chat.theme.user_message_bg));
    }

    #[test]
    fn permission_request_renders_with_clickable_options() {
        let mut chat = test_chat();
        let (tx, _rx) = oneshot::channel();
        chat.pending_ask = Some((
            PermissionRequest::new("允许执行 Bash", "cargo build", vec!["允许".into(), "拒绝".into()]),
            tx,
        ));
        chat.build_rows(100);
        let joined: String = chat
            .doc
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("允许执行 Bash"), "title: {joined}");
        assert!(joined.contains("[1] 允许"), "option row: {joined}");
        let ask_rows: Vec<(usize, usize)> = chat
            .doc
            .click_ranges
            .iter()
            .filter_map(|r| match r.target {
                ClickTarget::AskOption(i) => Some((r.start, i)),
                _ => None,
            })
            .collect();
        assert_eq!(ask_rows.len(), 2, "two clickable options");
    }

    #[test]
    fn sticky_appears_when_scrolled() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::User, "first question"));
        chat.messages.push(msg(Role::Assistant, "answer"));
        // 把内容撑高到需要滚动
        chat.messages[1].text = "answer\n".repeat(60);
        chat.build_rows(100);
        chat.scroll = 10;
        chat.build_rows(100);
        let sticky = chat.doc.sticky.as_deref().expect("sticky when scrolled");
        assert!(sticky.contains("first"), "sticky shows user text: {sticky}");
        chat.scroll = 0;
        chat.build_rows(100);
        assert!(chat.doc.sticky.is_none(), "no sticky at top");
    }
}
