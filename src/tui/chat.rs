//! 聊天状态机：消息/活动/折叠组的增量模型 + 文档行构建。
//!
//! 移植自旧 `tui.rs` 的 `BingoChat`（ratatui 版）：事件处理语义、
//! 折叠判定、展开切换原样保留；`draw` 换成 [`Chat::build_rows`]，
//! 产出显示无关的样式化行文档，由 UI 层映射为 iocraft 元素。
//! 事件从通道（`UiEvent` / `AskRequest`）流入，键盘/鼠标经
//! [`Chat::on_key`] / [`Chat::doc_click`] 流入。

use std::collections::{HashMap, HashSet};
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
use crate::tui::gfx::{self, ImageCap, ImageMeta};
use crate::tui::line::{text_width, Line, SegStyle};
use crate::tui::markdown::MarkdownRenderer;
use crate::tui::theme::{Theme, ThemeSetting};
use crate::tui::UiEvent;

/// 强制整屏清除重绘信号：doc 行数变化（内容整体位移）或回合收尾时置位，
/// UI 层 hook 消费（绕开 iocraft 行 diff 在行号位移时的残留）。
pub(crate) static FORCE_FULL_REDRAW: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 权限询问：请求 + 结果回执。
pub type AskRequest = (PermissionRequest, oneshot::Sender<DialogAction>);

/// 权限对话框结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogAction {
    /// 选项 `index`（0 起）被确认。
    Confirm(usize),
    /// AskUserQuestion 的 Other 自由输入被提交。
    Answer(String),
    /// 对话框被 Esc 取消。
    Cancel,
}

/// 要展示的权限/提问块。
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    /// 标题，如 `允许执行 Bash` 或 AskUserQuestion 的 header。
    pub title: String,
    /// 标题下的说明。
    pub question: String,
    /// 编号选项（数字自动添加）。
    pub options: Vec<String>,
    /// options[i] 的说明（CC Select 副行，dim 渲染）。
    pub descriptions: Vec<Option<String>>,
    /// AskUserQuestion：末尾自动附加 "Other" 自由输入（CC 行为）。
    pub free_text: bool,
}

impl PermissionRequest {
    pub fn new(title: impl Into<String>, question: impl Into<String>, options: Vec<String>) -> Self {
        Self {
            title: title.into(),
            question: question.into(),
            options,
            descriptions: Vec::new(),
            free_text: false,
        }
    }
}

/// AskUserQuestion 回答结果块（CC `User answered Claude's questions` 消息）。
#[derive(Debug, Clone, Default)]
pub struct AskResult {
    /// (问题, 答案) 已答条目。
    pub answered: Vec<(String, String)>,
    /// 用户 Esc 拒绝回答（free_text 请求）。
    pub declined: bool,
}

impl AskResult {
    fn is_empty(&self) -> bool {
        self.answered.is_empty() && !self.declined
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
    /// 前置"定稿"行数：不再变化、可一次性打印进终端 scrollback 的行
    /// （REPL 模式的打印边界；全屏模式不用）。
    pub settled: usize,
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

/// 不在 transcript 展示的工具调用（对标 CC renderToolUseMessage = null）：
/// Task 工具族（任务区面板即展示）、AskUserQuestion（对话框即展示）。
pub fn is_hidden_tool(name: &str) -> bool {
    matches!(
        name,
        "TaskCreate"
            | "TaskUpdate"
            | "TaskGet"
            | "TaskList"
            | "AskUserQuestion"
            // Agent 对齐 CC Task renderToolUseMessage=null：不渲染工具行，
            // 进度由 Watch 活动行（`Agent: 描述 · 已产出 N 字符`）一处承载。
            | "Agent"
    )
}

/// 内置 slash 命令表（/help 与下拉建议共用单一来源，对齐 CC 命令注册表）。
pub const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("help", "显示可用命令"),
    ("clear", "清空对话，开始新会话（别名 /reset /new）"),
    ("compact", "压缩上下文（旧消息 → 摘要）"),
    ("model", "显示/切换模型（/model [名称]）"),
    ("resume", "恢复历史会话（/resume [名称或关键词]）"),
    ("rename", "重命名当前会话（/rename [名称]）"),
    ("context", "显示上下文用量"),
    ("status", "显示会话状态（模型/权限/会话/上下文）"),
    ("permissions", "列出/添加权限规则"),
    ("theme", "切换主题（/theme [dark|light|auto]）"),
    ("mcp", "管理 MCP 服务器（/mcp [enable|disable|reconnect]）"),
    ("provider", "列出/切换 API provider（/provider [名称]）"),
    ("think", "设置思考级别（/think [off|low|medium|high]）"),
    ("skills", "列出可用技能"),
    ("tasks", "列出后台任务"),
    ("exit", "退出会话"),
];

/// slash 下拉建议项（对齐 CC SuggestionItem：/name + 描述）。
#[derive(Debug, Clone, PartialEq)]
pub struct SlashSuggestion {
    pub name: String,
    pub description: String,
}

/// footer 模型徽标：`{model} · think {level}`（off = 不显示等级，保持简洁）。
pub fn model_footer_label(model: &str, thinking: Option<&str>) -> String {
    match thinking {
        Some(level) if level != "off" => format!("{model} · think {level}"),
        _ => model.to_string(),
    }
}

/// `/model` 二级选择器状态：一级 = endpoint 列表，二级 = 该 endpoint 的模型
/// （异步拉取 `/v1/models` 补充；拉取完成前显示已知模型 + loading）。
#[derive(Clone)]
pub struct ModelMenu {
    /// 一级列表：`default`（顶层配置）+ settings.providers 名字。
    pub providers: Vec<String>,
    pub provider_selected: usize,
    /// 二级模型列表（None = 停在一级）。
    pub models: Option<ModelMenuModels>,
}

#[derive(Clone)]
pub struct ModelMenuModels {
    pub provider: String,
    /// 已加载的模型（异步补充；可能未拉完）。
    pub models: Vec<String>,
    pub loading: bool,
    pub selected: usize,
}

/// 下拉最大可见行数（对齐 CC OVERLAY_MAX_ITEMS = 5）。
pub const SLASH_SUGGESTIONS_MAX: usize = 5;

/// slash 临时提示存活时长：超时后从输入框上方消失（不落盘）。
pub const SLASH_OUTPUT_TTL: std::time::Duration = std::time::Duration::from_secs(2);

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

/// Bash 工具结果预览：去掉 `$ cmd` 回显与 `[Exited with code N]` 尾注，
/// 只留命令输出（对齐 CC BashModeProgress 的裸输出展示）。
fn bash_output_preview(lines: &[String]) -> Vec<String> {
    let mut out: Vec<String> = lines.to_vec();
    if out.first().is_some_and(|l| l.starts_with("$ ")) {
        out.remove(0);
    }
    if out.last().is_some_and(|l| l.starts_with("[Exited with code")) {
        out.pop();
    }
    out
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

/// 思考完成态随机词（对标 CC `TURN_COMPLETION_VERBS`，均适配 `for Xs`）。
const COMPLETION_WORDS: [&str; 8] = [
    "Baked",
    "Brewed",
    "Churned",
    "Cogitated",
    "Cooked",
    "Crunched",
    "Sautéed",
    "Worked",
];

fn thinking_stage(seed: usize) -> &'static str {
    THINKING_WORDS[seed % THINKING_WORDS.len()]
}

/// 完成态词：按创建时刻纳秒随机取样（与运行词不同源，`✻ Churned for 40s`）。
fn thinking_done_verb() -> &'static str {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0);
    COMPLETION_WORDS[nanos % COMPLETION_WORDS.len()]
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
    /// bash 模式（对标 CC `!` 前缀）：输入直接执行，不经模型。
    pub bash_mode: bool,
    pub typing: bool,
    pub busy: bool,
    /// Esc/Ctrl+C 中断过当前回合：后台任务完成通知不再自动拉起新回合
    /// （对齐 CC interrupt：等待用户主动提交才继续），start_turn 时复位。
    pub interrupted: bool,
    /// 当前 assistant 消息索引。
    pub stream_msg: Option<usize>,
    thinking_buf: String,
    output_tokens: u64,
    pub tick: u64,
    /// TurnStart 时的 tick：运行态 thinking 的相对计时基准。
    turn_start_tick: u64,
    /// TurnStart 的真实时钟（状态行耗时基准；TurnEnd 清空）。
    turn_started: Option<std::time::Instant>,
    pub warnings: Vec<String>,
    pub user: String,
    pub cwd: String,
    /// 权限询问：请求 + 结果回执。
    pub pending_ask: Option<(PermissionRequest, oneshot::Sender<DialogAction>)>,
    /// 对话框焦点行（0..=options.len()；== options.len() = Other 输入）。
    ask_focus: usize,
    /// Other 自由输入缓冲。
    ask_other: String,
    /// AskUserQuestion 已答结果块（CC 结果消息；跨请求累积）。
    pub ask_result: Option<AskResult>,
    /// 任务列表磁盘快照缓存（tick 周期刷新）。
    tasks_cache: Vec<TodoItem>,
    processor: MarkdownProcessor,
    renderer: MarkdownRenderer,
    reply_cache: HashMap<String, Vec<Line>>,
    /// 终端图片能力（kitty 协议；inline 模式检测，fullscreen 为 None）。
    pub image_cap: Option<ImageCap>,
    /// 已加载图片缓存（url → PNG 字节 + 单元格尺寸）。
    pub images: HashMap<String, Arc<ImageMeta>>,
    /// 拉取中的图片 url（防重复加载）。
    images_pending: HashSet<String>,
    /// 图片缓存版本（加载完成递增 → 渲染缓存失效）。
    images_version: u64,
    /// 文档是否待重建（事件/tick/展开等写入后置位；布局层消费后清除）。
    pub dirty: bool,
    /// 上次 tick 时的文档行数（行号位移检测）。
    prev_doc_len: usize,
    /// 上次 build_rows 的宽度（markdown 缓存按宽度失效）。
    prev_build_width: usize,
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
    /// 终端背景色探测结果（/theme 重建主题用）。
    detected_background: Option<bool>,
    /// slash 命令输出行（/help /status 等）：渲染在消息之后、空闲时定稿。
    pub slash_lines: Vec<String>,
    /// slash 输出出现时间（tick 超时自动消失）。
    pub slash_at: Option<std::time::Instant>,
    /// /exit 请求退出（组件层消费 → system.exit）。
    pub exit: bool,
    /// 组件层每帧写入的落盘边界（doc.rows 索引，printed 推进的下一帧目标）；
    /// FlushRows hook 消费（先清屏后打印，保证 iocraft 相对定位正确）。
    pub flush_up_to: usize,
    /// 组件层每帧写入的 resize 重放标志（打印 printed 之前的视口行）。
    pub replay_pending: bool,
    /// slash 下拉建议（输入 `/` 且无参数时非空；组件层渲染）。
    pub slash_suggestions: Vec<SlashSuggestion>,
    /// 下拉选中索引。
    pub slash_selected: usize,
    /// `/model` 二级选择器（一级 endpoint → 二级模型列表；None = 未激活）。
    pub model_menu: Option<ModelMenu>,
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
        detected_background: Option<bool>,
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
            bash_mode: false,
            typing: true,
            busy: false,
            stream_msg: None,
            thinking_buf: String::new(),
            output_tokens: 0,
            tick: 0,
            turn_start_tick: 0,
            turn_started: None,
            warnings: Vec::new(),
            user: std::env::var("USER").unwrap_or_else(|_| "user".to_string()),
            cwd: std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            pending_ask: None,
            ask_focus: 0,
            ask_other: String::new(),
            ask_result: None,
            tasks_cache: Vec::new(),
            processor: MarkdownProcessor::default(),
            renderer: MarkdownRenderer::with_theme(80, theme.clone()),
            reply_cache: HashMap::new(),
            image_cap: None,
            images: HashMap::new(),
            images_pending: HashSet::new(),
            images_version: 1,
            dirty: true,
            prev_doc_len: 0,
            prev_build_width: 0,
            width: 80,
            viewport_height: 24,
            scroll: 0,
            auto_scroll: true,
            doc: Doc {
                rows: Vec::new(),
                click_ranges: Vec::new(),
                sticky: None,
                settled: 0,
            },
            pending_tools: Vec::new(),
            theme,
            detected_background,
            slash_lines: Vec::new(),
            slash_at: None,
            exit: false,
            flush_up_to: 0,
            replay_pending: false,
            slash_suggestions: Vec::new(),
            slash_selected: 0,
            model_menu: None,
            tasks_visible: false,
            interrupted: false,
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
            self.ask_focus = 0;
            self.ask_other.clear();
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
            UiEvent::ModelsLoaded { provider, models } => {
                // 二级菜单补充异步拉取结果；列表仍空说明拉取失败（保留 loading
                // 不阻塞：用户可直接输入模型名或 Esc 退出）。
                if let Some(menu) = &mut self.model_menu
                    && let Some(m) = &mut menu.models
                    && m.provider == provider
                {
                    m.models = models;
                    m.loading = false;
                    m.selected = m.selected.min(m.models.len().saturating_sub(1));
                }
            }
            UiEvent::ImageReady { url, meta } => {
                self.images_pending.remove(&url);
                match meta {
                    Some(meta) => {
                        self.images.insert(url.clone(), Arc::new(meta));
                    }
                    None => {
                        self.images.remove(&url);
                        let warning = format!("图片加载失败: {url}");
                        if !self.warnings.iter().any(|w| w == &warning) {
                            self.warnings.push(warning);
                        }
                    }
                }
                // 缓存版本递增：渲染器逐块缓存与 reply_cache 一并失效。
                self.images_version = self.images_version.wrapping_add(1);
                self.reply_cache.clear();
                self.dirty = true;
            }
            UiEvent::TurnStart => {
                self.thinking_buf.clear();
                self.pending_tools_clear();
                self.turn_started = Some(std::time::Instant::now());
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
                    done_verb: Some(thinking_done_verb()),
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
                            done_verb: Some(thinking_done_verb()),
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
                if is_hidden_tool(&name) {
                    return;
                }
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
            UiEvent::ToolReady {
                name,
                input,
                standalone,
            } => {
                let Some(i) = self.stream_msg else { return };
                if is_hidden_tool(&name) {
                    return;
                }
                let Some(idx) = self.pending_tools_pop() else {
                    return;
                };
                if let ActivityKind::Tool(call) = &mut self.messages[i].activities[idx].kind {
                    call.summary = crate::query::summarize_input(&name, &input);
                }
                // `!` 命令：独立活动（输出预览直接展开），不参与折叠组。
                if standalone {
                    return;
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
                    // 用户中断过回合后不自动续跑（等主动提交）。
                    if !self.interrupted {
                        self.submit_auto();
                    }
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
                            // 独立 Bash（`!` 命令）：预览 = 输出本身（去掉
                            // `$ cmd` 回显与 `[Exited with code N]` 尾注），
                            // 默认展开（对标 CC BashModeProgress 直接展示输出）。
                            let lines: Vec<String> = done
                                .output
                                .lines()
                                .map(str::to_string)
                                .collect();
                            let preview: Vec<String> = if done.name == "Bash" {
                                bash_output_preview(&lines)
                            } else {
                                lines
                            };
                            let content: Vec<Line> = preview
                                .into_iter()
                                .filter(|l| !l.trim().is_empty())
                                .map(Line::plain)
                                .collect();
                            hint.set_content(content);
                            if done.name == "Bash" && !hint.expanded {
                                hint.expanded = true;
                            }
                        }
                        break;
                    }
                }
            }
            UiEvent::TurnEnd => {
                self.busy = false;
                self.turn_started = None;
                self.output_tokens = 0;
                FORCE_FULL_REDRAW.store(true, std::sync::atomic::Ordering::Relaxed);
                // 用户中断后不再因后台任务完成自动拉起新回合。
                if self.session.watch.has_wake_notifications() && !self.interrupted {
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
                    // 文本已定稿 → 异步加载其中的图片（完成后回发 ImageReady）。
                    let text = self.messages[i].text.clone();
                    self.load_message_images(&text);
                }
                self.stream_msg = None;
            }
            UiEvent::Warning(message) => {
                if !self.warnings.iter().any(|w| w == &message) {
                    self.warnings.push(message);
                }
            }
            UiEvent::SlashOutput(message) => {
                self.push_slash_output(message);
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

    /// 扫描消息文本中的 markdown 图片引用，异步加载未缓存/未在途的
    /// url（data:/http(s)/本地路径），完成后回发 `ImageReady`。
    fn load_message_images(&mut self, text: &str) {
        let Some(cap) = self.image_cap else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let urls = gfx::extract_image_urls(text);
        for url in urls {
            if self.images.contains_key(&url) || self.images_pending.contains(&url) {
                continue;
            }
            self.images_pending.insert(url.clone());
            let events = self.events.clone();
            let cwd = self.cwd.clone();
            handle.spawn(async move {
                let meta = gfx::load_image(&url, std::path::Path::new(&cwd), &cap).await;
                let _ = events.send(UiEvent::ImageReady { url, meta });
            });
        }
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
                self.ask_click(*index);
                true
            }
        }
    }

    /// 点击对话框选项：Other 行 → 进入输入模式；其余立即确认。
    fn ask_click(&mut self, index: usize) {
        let Some((request, _)) = &self.pending_ask else {
            return;
        };
        let options_len = request.options.len();
        let free_text = request.free_text;
        if index >= options_len && free_text {
            self.ask_focus = index;
            return;
        }
        self.choose_ask_option(index);
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
        if self.bash_mode {
            self.start_bash_turn(text.trim().to_string());
            return;
        }
        if let Some(cmd) = text.strip_prefix('/') {
            // Enter 时输入是部分前缀且有下拉建议：应用选中项并执行
            //（对齐 CC handleEnter：suggestions 存在时 Enter = 补全 + 执行）。
            if !self.slash_suggestions.is_empty()
                && !self
                    .slash_suggestions
                    .iter()
                    .any(|s| s.name == cmd.trim_end())
            {
                let selected = self.slash_suggestions.get(self.slash_selected).cloned();
                self.slash_suggestions.clear();
                if let Some(s) = selected
                    && self.run_slash(&s.name)
                {
                    return;
                }
            }
            if self.run_slash(cmd) {
                return;
            }
        }
        self.start_turn(text, true);
    }

    /// slash 输出行入队（临时提示：渲染在消息之后、输入框上方，TTL 后消失）。
    fn push_slash_output(&mut self, text: String) {
        for line in text.lines() {
            self.slash_lines.push(line.to_string());
        }
        self.slash_at = Some(std::time::Instant::now());
        self.dirty = true;
    }

    /// slash 命令分发（对齐 Claude Code 常用命令）。返回 true = 已消费。
    fn run_slash(&mut self, line: &str) -> bool {
        // 任何 slash 执行都关闭下拉建议（完整输入 Enter 时不走 submit 的清菜单分支，
        // 否则 `+ /model …` 建议行永久残留在输入框下方）。
        self.slash_suggestions.clear();
        let (cmd, arg) = match line.split_once(char::is_whitespace) {
            Some((c, a)) => (c, a.trim()),
            None => (line, ""),
        };
        match cmd {
            "help" | "?" => self.slash_help(),
            "exit" | "quit" => self.exit = true,
            "clear" | "reset" | "new" => self.slash_clear(),
            "model" => self.slash_model(arg),
            "theme" => self.slash_theme(arg),
            "rename" => self.slash_rename(arg),
            "resume" => self.slash_resume(arg),
            "compact" => self.slash_compact(),
            "status" => self.slash_status(),
            "context" => self.slash_context(),
            "permissions" => self.slash_permissions(arg),
            "mcp" => self.slash_mcp(arg),
            "provider" => self.slash_provider(arg),
            "think" => self.slash_think(arg),
            "skills" => self.slash_skills(),
            "tasks" => self.slash_tasks(),
            other => {
                // 技能名：展开为提示词并作为用户消息提交（对齐 CC prompt Command：
                // 技能与内置命令同注册表，输入 /技能名 即执行）。
                let skills = crate::skills::load_skills(
                    &self.session.home,
                    &std::path::PathBuf::from(&self.cwd),
                );
                if let Some(skill) = skills.iter().find(|s| s.name == other) {
                    let expanded = crate::skills::expand_skill(skill, arg);
                    self.start_turn(expanded, true);
                    return true;
                }
                self.push_slash_output(format!(
                    "未知命令: /{other}。输入 /help 查看可用命令。"
                ))
            }
        }
        true
    }

    fn slash_help(&mut self) {
        let mut lines = vec!["可用命令（对齐 Claude Code）：".to_string()];
        for (name, description) in SLASH_COMMANDS {
            lines.push(format!("  /{name:<12} — {description}"));
        }
        self.push_slash_output(lines.join("\n"));
    }

    fn slash_clear(&mut self) {
        let session = self.session.clone();
        let cwd = std::path::PathBuf::from(&self.cwd);
        let new_transcript = crate::transcript::create(&session.home, &cwd).ok();
        let _ = session.runtime.transcript_tx.send(new_transcript);
        self.messages.clear();
        self.stream_msg = None;
        self.slash_lines.clear();
        self.ask_result = None;
        self.warnings.clear();
        self.push_slash_output("✓ 已清空对话，开始新会话。".to_string());
    }

    fn slash_model(&mut self, arg: &str) {
        if arg.is_empty() {
            self.open_model_menu();
            return;
        }
        let _ = self.session.runtime.model_tx.send(arg.to_string());
        self.push_slash_output(format!("✓ 模型已切换: {arg}"));
    }

    /// 进入 `/model` 二级选择器：一级 = 当前 endpoint + 配置 providers。
    fn open_model_menu(&mut self) {
        let mut providers = vec!["default".to_string()];
        providers.extend(self.session.client.provider_names());
        let current = self.session.runtime.provider.borrow().clone();
        let selected = providers
            .iter()
            .position(|p| *p == current)
            .unwrap_or(0);
        self.model_menu = Some(ModelMenu {
            providers,
            provider_selected: selected,
            models: None,
        });
        self.slash_suggestions.clear();
    }

    /// 一级 Enter：以该 provider 端点异步拉取模型列表（fork 端点，不切换当前），
    /// 拉取完成经 ModelsLoaded 事件补充进菜单。
    fn open_model_models(&mut self, provider: String) {
        let session = self.session.clone();
        let events = self.events.clone();
        let provider_for_spawn = provider.clone();
        tokio::spawn(async move {
            let client = match session.client.with_provider(&provider_for_spawn) {
                Ok(c) => c,
                // default：直接克隆当前端点。
                Err(_) => session.client.clone(),
            };
            let models = client.list_models().await.unwrap_or_default();
            let _ = events.send(UiEvent::ModelsLoaded { provider: provider_for_spawn, models });
        });
        // 菜单已由 Enter 分支 take 出来——这里重建二级状态。
        self.model_menu = Some(ModelMenu {
            providers: vec![provider.clone()],
            provider_selected: 0,
            models: Some(ModelMenuModels {
                provider,
                models: Vec::new(),
                loading: true,
                selected: 0,
            }),
        });
    }

    /// 模型菜单键盘：↑↓ 移动、Enter 进入二级/确认、Esc 退出。返回已消费。
    fn model_menu_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let Some(menu) = &mut self.model_menu else {
            return false;
        };
        match code {
            KeyCode::Down if !modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(m) = &mut menu.models {
                    if !m.models.is_empty() {
                        m.selected = (m.selected + 1) % m.models.len();
                    }
                } else {
                    menu.provider_selected =
                        (menu.provider_selected + 1) % menu.providers.len();
                }
                true
            }
            KeyCode::Up if !modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(m) = &mut menu.models {
                    if !m.models.is_empty() {
                        m.selected = m.selected.checked_sub(1).unwrap_or(m.models.len() - 1);
                    }
                } else {
                    menu.provider_selected = menu
                        .provider_selected
                        .checked_sub(1)
                        .unwrap_or(menu.providers.len() - 1);
                }
                true
            }
            KeyCode::Enter => {
                let Some(menu) = self.model_menu.take() else {
                    return true;
                };
                let Some(m) = menu.models else {
                    // 一级：进入二级并异步拉取模型列表。
                    let provider = menu
                        .providers
                        .get(menu.provider_selected)
                        .cloned()
                        .unwrap_or_default();
                    self.open_model_models(provider);
                    return true;
                };
                // 二级：确认选中的模型。列表为空（拉取失败/未返回）时保留菜单。
                let provider = m.provider.clone();
                let model = m.models.get(m.selected).cloned().unwrap_or_default();
                if model.is_empty() {
                    self.model_menu = Some(ModelMenu {
                        providers: menu.providers,
                        provider_selected: menu.provider_selected,
                        models: Some(m),
                    });
                    return true;
                }
                if provider != self.session.runtime.provider.borrow().clone()
                    && let Err(e) = self.session.client.set_provider(&provider)
                {
                    self.model_menu = Some(ModelMenu {
                        providers: menu.providers,
                        provider_selected: menu.provider_selected,
                        models: Some(m),
                    });
                    self.push_slash_output(e);
                    return true;
                }
                let _ = self.session.runtime.model_tx.send(model.clone());
                let _ = self.session.runtime.provider_tx.send(provider.clone());
                self.push_slash_output(format!("✓ 模型已切换: {provider} · {model}"));
                true
            }
            KeyCode::Esc => {
                // 二级 → 回一级；一级 → 整体退出（对齐 CC 逐级返回）。
                if self
                    .model_menu
                    .as_mut()
                    .is_some_and(|m| m.models.is_some())
                {
                    self.model_menu.as_mut().expect("菜单必在").models = None;
                } else {
                    self.model_menu = None;
                }
                true
            }
            _ => false,
        }
    }

    fn slash_theme(&mut self, arg: &str) {
        let setting = if arg.is_empty() {
            ThemeSetting::Auto
        } else {
            ThemeSetting::parse(Some(arg))
        };
        let name = match setting {
            ThemeSetting::Dark => "dark",
            ThemeSetting::Light => "light",
            ThemeSetting::Auto => "auto",
        };
        self.theme = Theme::for_terminal(setting, self.detected_background);
        // renderer 烘焙了主题样式、reply_cache 缓存了旧主题行——同步重建。
        self.renderer = crate::tui::markdown::MarkdownRenderer::with_theme(
            self.width,
            self.theme.clone(),
        );
        self.reply_cache.clear();
        self.dirty = true;
        let cwd = std::path::PathBuf::from(&self.cwd);
        let _ = crate::settings::upsert_project_settings(
            &cwd,
            &serde_json::json!({ "theme": name }),
        );
        self.push_slash_output(format!("✓ 主题已切换: {name}"));
    }

    fn slash_rename(&mut self, arg: &str) {
        let Some(t) = self.session.runtime.transcript.borrow().clone() else {
            self.push_slash_output("当前会话无 transcript，无法重命名。".to_string());
            return;
        };
        match t.rename(arg) {
            Ok(new_t) => {
                let name = new_t.name();
                let _ = self.session.runtime.transcript_tx.send(Some(new_t));
                self.push_slash_output(format!("✓ 会话已重命名: {name}"));
            }
            Err(e) => self.push_slash_output(format!("重命名失败: {e}")),
        }
    }

    fn slash_resume(&mut self, arg: &str) {
        let home = self.session.home.clone();
        let transcripts = match crate::transcript::list(&home) {
            Ok(t) => t,
            Err(e) => {
                self.push_slash_output(format!("无法读取会话列表: {e}"));
                return;
            }
        };
        if arg.is_empty() {
            if transcripts.is_empty() {
                self.push_slash_output("没有历史会话。".to_string());
                return;
            }
            let mut lines = vec!["历史会话（/resume [名称或关键词] 恢复）：".to_string()];
            for t in &transcripts {
                lines.push(format!("  {}", t.name()));
            }
            self.push_slash_output(lines.join("\n"));
            return;
        }
        let Some(found) = transcripts.iter().find(|t| t.name().contains(arg)) else {
            self.push_slash_output(format!("未找到包含 '{arg}' 的会话。"));
            return;
        };
        let count = found.load_messages().unwrap_or_default().len();
        let _ = self.session.runtime.transcript_tx.send(Some(found.clone()));
        self.messages.clear();
        self.slash_lines.clear();
        self.ask_result = None;
        self.push_slash_output(format!(
            "✓ 已切换到会话 {}（{count} 条消息），下一轮回复使用其历史。",
            found.name()
        ));
    }

    fn slash_compact(&mut self) {
        let session = self.session.clone();
        let events = self.events.clone();
        self.push_slash_output("⏳ 正在压缩上下文…".to_string());
        tokio::spawn(async move {
            let transcript = session.runtime.transcript.borrow().clone();
            let mut messages = match &transcript {
                Some(t) => t.load_messages().unwrap_or_default(),
                None => Vec::new(),
            };
            if messages.len() <= 8 {
                let _ = events.send(UiEvent::SlashOutput(
                    "对话太短，无需压缩。".to_string(),
                ));
                return;
            }
            let old_len = messages.len();
            let compacted =
                crate::compact::maybe_compact(&session, &mut messages, u64::MAX).await;
            if !compacted {
                let _ = events.send(UiEvent::SlashOutput(
                    "压缩失败（模型调用异常）。".to_string(),
                ));
                return;
            }
            let summary = messages
                .first()
                .map(|m| {
                    m.content
                        .iter()
                        .filter_map(|b| match b {
                            crate::api::types::ContentBlock::Text { text } => {
                                Some(text.clone())
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            if let Some(t) = transcript {
                let _ = t.replace_messages(&messages);
            }
            let _ = events.send(UiEvent::SlashOutput(format!(
                "✓ 已压缩 {old_len} 条消息 → 摘要 + 最近 8 条。\n摘要: {summary}"
            )));
        });
    }

    /// /status 与 /context 共用的异步统计：消息数 + token 数。
    fn slash_stats_async(
        &mut self,
        format: impl Fn(usize, u64) -> String + Send + 'static,
    ) {
        let session = self.session.clone();
        let events = self.events.clone();
        self.push_slash_output("⏳ 正在统计…".to_string());
        tokio::spawn(async move {
            let model = session.runtime.model.borrow().clone();
            let transcript = session.runtime.transcript.borrow().clone();
            let msgs = transcript
                .map(|t| t.load_messages().unwrap_or_default())
                .unwrap_or_default();
            let tokens = session
                .client
                .count_tokens(&model, &session.system, &msgs)
                .await
                .unwrap_or(0);
            let _ = events.send(UiEvent::SlashOutput(format(msgs.len(), tokens)));
        });
    }

    fn slash_status(&mut self) {
        let session = self.session.clone();
        let model = session.runtime.model.borrow().clone();
        let transcript = session.runtime.transcript.borrow().clone();
        let transcript_name = transcript
            .as_ref()
            .map(|t| t.name())
            .unwrap_or_else(|| "无".to_string());
        let mode = session.permission_mode_str().to_string();
        self.slash_stats_async(move |msg_count, tokens| {
            format!(
                "模型: {model}\n权限模式: {mode}\n会话: {transcript_name}\n消息数: {msg_count}\n上下文: {tokens} tokens / {}（{}%）",
                crate::budget::CONTEXT_WINDOW,
                tokens * 100 / crate::budget::CONTEXT_WINDOW
            )
        });
    }

    fn slash_context(&mut self) {
        self.slash_stats_async(|_msg_count, tokens| {
            let window = crate::budget::CONTEXT_WINDOW;
            let pct = tokens * 100 / window;
            let bar_len = 40usize;
            let filled = ((pct as usize * bar_len) / 100).min(bar_len);
            let bar = format!(
                "{}·{}",
                "#".repeat(filled),
                "·".repeat(bar_len - filled)
            );
            format!(
                "上下文: [{bar}] {pct}%\n已用 {tokens} / {window} tokens\n自动压缩阈值: {}%",
                crate::budget::AUTOCOMPACT_THRESHOLD * 100 / window
            )
        });
    }

    fn slash_permissions(&mut self, arg: &str) {
        let rules = self
            .session
            .runtime
            .permissions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if arg.is_empty() {
            let mut lines = vec!["权限规则（.bingo/settings.json）：".to_string()];
            for (name, list) in
                [("allow", &rules.allow), ("deny", &rules.deny), ("ask", &rules.ask)]
            {
                if list.is_empty() {
                    lines.push(format!("  {name}: （无）"));
                } else {
                    lines.push(format!("  {name}:"));
                    for rule in list {
                        lines.push(format!("    {rule}"));
                    }
                }
            }
            lines.push("用法: /permissions [allow|deny|ask] [规则，如 Skill(review:*)]".into());
            self.push_slash_output(lines.join("\n"));
            return;
        }
        let Some((kind, rule)) = arg.split_once(char::is_whitespace) else {
            self.push_slash_output("用法: /permissions [allow|deny|ask] [规则]".to_string());
            return;
        };
        if !["allow", "deny", "ask"].contains(&kind) || rule.is_empty() {
            self.push_slash_output("用法: /permissions [allow|deny|ask] [规则]".to_string());
            return;
        }
        let mut rules = self
            .session
            .runtime
            .permissions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let list = match kind {
            "allow" => &mut rules.allow,
            "deny" => &mut rules.deny,
            _ => &mut rules.ask,
        };
        if !list.iter().any(|r| r == rule) {
            list.push(rule.to_string());
        }
        *self
            .session
            .runtime
            .permissions
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = rules.clone();
        let cwd = std::path::PathBuf::from(&self.cwd);
        let patch = serde_json::json!({
            "permissions": {
                "allow": rules.allow,
                "deny": rules.deny,
                "ask": rules.ask,
            }
        });
        match crate::settings::upsert_project_settings(&cwd, &patch) {
            Ok(()) => self.push_slash_output(format!(
                "✓ 已添加 {kind} 规则: {rule}（运行时生效 + 已写入 .bingo/settings.json）"
            )),
            Err(e) => self.push_slash_output(format!(
                "✓ 已添加 {kind} 规则: {rule}（运行时生效）；持久化失败: {e}"
            )),
        }
    }

    fn slash_mcp(&mut self, arg: &str) {
        use crate::mcp::McpStatus;
        let session = self.session.clone();
        let cwd = std::path::PathBuf::from(&self.cwd);
        let events = self.events.clone();
        let parts: Vec<&str> = arg.split_whitespace().collect();
        match parts.first().copied() {
            None => {
                self.push_slash_output("⏳ 正在检查 MCP 服务器…".to_string());
                tokio::spawn(async move {
                    let mgr = session.runtime.mcp.lock().await;
                    let names = mgr.configured();
                    if names.is_empty() {
                        let _ = events.send(UiEvent::SlashOutput(
                            "未配置 MCP 服务器。\n在 .bingo/settings.json 或 \
                             ~/.config/bingo/settings.json 的 mcpServers 中添加。"
                                .to_string(),
                        ));
                        return;
                    }
                    let mut lines = vec![format!("MCP 服务器（{} 个）：", names.len())];
                    for name in names {
                        let line = match mgr.status(&name) {
                            McpStatus::Connected { tool_count } => {
                                format!("  ✓ {name}  connected · {tool_count} tools")
                            }
                            McpStatus::Failed { detail } => {
                                format!("  ✗ {name}  failed: {detail}")
                            }
                            McpStatus::Disabled => format!("  ○ {name}  disabled"),
                            McpStatus::NotConnected => format!("  · {name}  not connected"),
                        };
                        lines.push(line);
                    }
                    lines.push("用法: /mcp enable|disable [name|all] · /mcp reconnect <name>".into());
                    let _ = events.send(UiEvent::SlashOutput(lines.join("\n")));
                });
            }
            Some(action @ ("enable" | "disable")) => {
                let target = parts.get(1).copied().unwrap_or("all").to_string();
                let enabled = action == "enable";
                self.push_slash_output(format!(
                    "⏳ 正在{}{target}…",
                    if enabled { "启用 " } else { "禁用 " }
                ));
                tokio::spawn(async move {
                    let mut mgr = session.runtime.mcp.lock().await;
                    let targets: Vec<String> = if target == "all" {
                        mgr.configured()
                    } else if mgr.configured().contains(&target.to_string()) {
                        vec![target.to_string()]
                    } else {
                        Vec::new()
                    };
                    if targets.is_empty() {
                        let _ = events.send(UiEvent::SlashOutput(format!(
                            "未找到 MCP 服务器 \"{target}\"。"
                        )));
                        return;
                    }
                    for name in &targets {
                        mgr.set_enabled(name, enabled);
                    }
                    let list = mgr.disabled();
                    let _ = crate::settings::upsert_project_settings(
                        &cwd,
                        &serde_json::json!({ "disabledMcpServers": list }),
                    );
                    let verb = if enabled { "已启用" } else { "已禁用" };
                    let _ = events.send(UiEvent::SlashOutput(format!(
                        "{verb} {} 个 MCP 服务器: {}",
                        targets.len(),
                        targets.join(", ")
                    )));
                });
            }
            Some("reconnect") => {
                let Some(name) = parts.get(1).copied() else {
                    self.push_slash_output("用法: /mcp reconnect <服务器名>".to_string());
                    return;
                };
                let name = name.to_string();
                self.push_slash_output(format!("⏳ 正在重连 {name}…"));
                tokio::spawn(async move {
                    let mut mgr = session.runtime.mcp.lock().await;
                    if !mgr.configured().contains(&name) {
                        let _ = events.send(UiEvent::SlashOutput(format!(
                            "未找到 MCP 服务器 \"{name}\"。"
                        )));
                        return;
                    }
                    if mgr.is_disabled(&name) {
                        let _ = events.send(UiEvent::SlashOutput(format!(
                            "{name} 已禁用，先 /mcp enable {name} 再重连。"
                        )));
                        return;
                    }
                    match mgr.reconnect(&name).await {
                        Ok(()) => {
                            let count = match mgr.status(&name) {
                                McpStatus::Connected { tool_count } => tool_count,
                                _ => 0,
                            };
                            let _ = events.send(UiEvent::SlashOutput(format!(
                                "✓ {name} 已重连 · {count} tools"
                            )));
                        }
                        Err(e) => {
                            let _ = events.send(UiEvent::SlashOutput(format!("✗ {e}")));
                        }
                    }
                });
            }
            _ => self.push_slash_output(
                "用法: /mcp [enable|disable [name|all]] · /mcp reconnect <name>".to_string(),
            ),
        }
    }

    fn slash_provider(&mut self, arg: &str) {
        let session = self.session.clone();
        if arg.is_empty() {
            let current = session.runtime.provider.borrow().clone();
            let (key, url) = session.client.current_endpoint();
            let mut lines = vec![format!(
                "当前 provider: {current}\n  {key} @ {url}",
            )];
            for name in session.client.provider_names() {
                lines.push(format!("  {name}"));
            }
            lines.push("用法: /provider <名称>（settings.json 的 providers 段）".into());
            self.push_slash_output(lines.join("\n"));
            return;
        }
        let name = arg.to_string();
        match session.client.set_provider(&name) {
            Ok(()) => {
                let (_, url) = session.client.current_endpoint();
                let _ = session.runtime.provider_tx.send(name.clone());
                self.push_slash_output(format!("✓ provider 已切换: {name}（{url}）"));
            }
            Err(e) => self.push_slash_output(e),
        }
    }

    fn slash_think(&mut self, arg: &str) {
        let session = self.session.clone();
        let cwd = std::path::PathBuf::from(&self.cwd);
        if arg.is_empty() {
            let current = session.runtime.thinking.borrow().clone();
            let shown = current.as_deref().unwrap_or("off");
            self.push_slash_output(format!(
                "当前思考级别: {shown}\n用法: /think [off|low|medium|high]\n\
                 low=2048 · medium=8192 · high=16384 budget tokens"
            ));
            return;
        }
        let level = match arg {
            "off" => None,
            "low" | "medium" | "high" => Some(arg.to_string()),
            _ => {
                self.push_slash_output("用法: /think [off|low|medium|high]".to_string());
                return;
            }
        };
        let _ = session.runtime.thinking_tx.send(level.clone());
        let saved = level.as_deref().unwrap_or("off");
        let _ = crate::settings::upsert_project_settings(
            &cwd,
            &serde_json::json!({ "thinkingLevel": saved }),
        );
        self.push_slash_output(format!("✓ 思考级别已设置: {saved}"));
    }

    fn slash_skills(&mut self) {
        let home = self.session.home.clone();
        let cwd = std::path::PathBuf::from(&self.cwd);
        let skills = crate::skills::load_skills(&home, &cwd);
        if skills.is_empty() {
            self.push_slash_output(
                "当前没有可用的技能。\n技能放在 .bingo/skills/<name>/SKILL.md 或 $XDG_CONFIG_HOME/bingo/skills/<name>/SKILL.md。"
                    .to_string(),
            );
            return;
        }
        let listing =
            crate::skills::format_listing(&skills, crate::skills::DEFAULT_CHAR_BUDGET);
        self.push_slash_output(format!("可用技能：\n{listing}"));
    }

    fn slash_tasks(&mut self) {
        self.refresh_tasks();
        // task_lines 受任务区可见性门控——/tasks 显式请求，临时放行。
        let was_visible = self.tasks_visible;
        self.tasks_visible = true;
        let lines = self.task_lines();
        self.tasks_visible = was_visible;
        if lines.is_empty() {
            self.push_slash_output("当前没有后台任务。".to_string());
            return;
        }
        let text: Vec<String> = lines.iter().map(|l| l.plain_text()).collect();
        self.push_slash_output(text.join("\n"));
    }

    /// 重建 slash 下拉建议（输入变化时调用）：
    /// `/` 开头且无参数时显示；空 query 列全部（内置命令 + 技能），
    /// 否则前缀/包含匹配（对齐 CC generateCommandSuggestions 的简化版）。
    fn update_slash_suggestions(&mut self) {
        self.slash_suggestions.clear();
        let input = self.input.trim_end();
        let Some(query) = input.strip_prefix('/') else {
            return;
        };
        if query.contains(char::is_whitespace) {
            return; // 已带参数：不显示
        }
        let mut items: Vec<SlashSuggestion> = SLASH_COMMANDS
            .iter()
            .map(|(name, desc)| SlashSuggestion {
                name: (*name).to_string(),
                description: (*desc).to_string(),
            })
            .collect();
        // 技能并入（对齐 CC：/ 菜单含 skills）。描述截断：
        // NoWrap 超长行会把 canvas 撑出终端宽（iocraft 不截断），
        // 行 diff 错位 → 帧残留；上限对齐 CC MAX_LISTING_DESC_CHARS。
        let home = self.session.home.clone();
        let cwd = std::path::PathBuf::from(&self.cwd);
        for skill in crate::skills::load_skills(&home, &cwd) {
            let mut description = skill.description;
            if description.chars().count() > crate::skills::MAX_LISTING_DESC_CHARS {
                let cut: String = description
                    .chars()
                    .take(crate::skills::MAX_LISTING_DESC_CHARS - 1)
                    .collect();
                description = format!("{cut}…");
            }
            items.push(SlashSuggestion {
                name: skill.name,
                description,
            });
        }
        let q = query.to_lowercase();
        if !q.is_empty() {
            // 前缀优先（短者在前），其次包含匹配；保持内置在前。
            items.retain(|s| {
                let n = s.name.to_lowercase();
                n.starts_with(&q) || n.contains(&q)
            });
            items.sort_by(|a, b| {
                let pa = a.name.to_lowercase().starts_with(&q);
                let pb = b.name.to_lowercase().starts_with(&q);
                pb.cmp(&pa).then(a.name.len().cmp(&b.name.len()))
            });
        }
        self.slash_suggestions = items.into_iter().take(SLASH_SUGGESTIONS_MAX).collect();
        self.slash_selected = self.slash_selected.min(self.slash_suggestions.len().saturating_sub(1));
    }

    /// 下拉键盘消费：↑↓ 移动选中、Tab 补全（不执行）、Esc 关闭。
    /// 不用 j/k 导航：菜单打开时 j/k 会被当作输入字符（如 /thin → think），
    /// 吞键导致命令被截断。返回 true = 已消费。
    fn slash_menu_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if self.slash_suggestions.is_empty() {
            return false;
        }
        match code {
            KeyCode::Down if !modifiers.contains(KeyModifiers::CONTROL) => {
                self.slash_selected = (self.slash_selected + 1) % self.slash_suggestions.len();
                true
            }
            KeyCode::Up if !modifiers.contains(KeyModifiers::CONTROL) => {
                self.slash_selected = self
                    .slash_selected
                    .checked_sub(1)
                    .unwrap_or(self.slash_suggestions.len() - 1);
                true
            }
            KeyCode::Tab => {
                self.apply_slash_suggestion();
                true
            }
            KeyCode::Esc => {
                self.slash_suggestions.clear();
                true
            }
            _ => false,
        }
    }

    /// 应用选中建议（对齐 CC applyCommandSuggestion）：`/name ` 回填输入。
    fn apply_slash_suggestion(&mut self) {
        if let Some(s) = self.slash_suggestions.get(self.slash_selected) {
            self.input = format!("/{} ", s.name);
        }
        self.slash_suggestions.clear();
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

    /// 多轮连续性：加载 transcript 历史作为本轮上下文（每轮独立 run_query）。
    fn load_history(
        session: &Session,
        on_warning: &mut (dyn FnMut(String) + Send),
    ) -> Vec<crate::api::types::Message> {
        let Some(t) = session.runtime.transcript.borrow().clone() else {
            return Vec::new();
        };
        match t.load_messages() {
            Ok(msgs) => msgs,
            Err(crate::transcript::TranscriptError::Io(e))
                if e.kind() == std::io::ErrorKind::NotFound =>
            {
                Vec::new()
            }
            Err(e) => {
                on_warning(format!("transcript load failed: {e}"));
                Vec::new()
            }
        }
    }

    /// 回合结束后处理：记忆抽取 + TurnEnd。
    async fn finish_turn(
        events: &mpsc::UnboundedSender<UiEvent>,
        session: &Arc<Session>,
        outcome: &crate::query::QueryOutcome,
    ) {
        if outcome.aborted {
            let _ = events.send(UiEvent::Warning("回合已中断".to_string()));
        }
        let cwd = std::env::current_dir().unwrap_or_default();
        crate::memory::extract_memory(session, &outcome.messages, &session.home, &cwd).await;
        let _ = events.send(UiEvent::TurnEnd);
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
        self.interrupted = false;
        let session = self.session.clone();
        let events = self.events.clone();
        let asks = self.asks.clone();
        // 先订阅再复位：tokio watch 的 send 在无 receiver 时不更新值——
        // 上一轮 spawn 结束后 receiver 已全部 drop，若先 send(false) 会静默
        // 失效（值保持 true），新回合会在连接阶段被误判为中断。
        let cancel_rx = self.cancel_tx.subscribe();
        self.cancel_tx.send_replace(false);
        tokio::spawn(async move {
            let _ = events.send(UiEvent::TurnStart);
            let mut ui = crate::tui::tui_hooks(events.clone(), asks);
            let history = Self::load_history(&session, &mut ui.on_warning);
            let result = run_query(&session, history, &text, &mut ui, Some(cancel_rx)).await;
            match result {
                Ok(outcome) => {
                    Self::finish_turn(&events, &session, &outcome).await;
                }
                Err(e) => {
                    let _ = events.send(UiEvent::Error(e.to_string()));
                }
            }
        });
    }

    /// bash 模式回合（对标 CC processBashCommand）：`!` 命令直接执行，
    /// 输出展示为工具活动；respondToBashCommands 开启时模型随后回应。
    fn start_bash_turn(&mut self, command: String) {
        self.messages.push(UiMessage {
            role: Role::User,
            text: format!("!{command}"),
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        self.busy = true;
        let session = self.session.clone();
        let events = self.events.clone();
        let asks = self.asks.clone();
        // 与 start_turn 相同：先订阅再复位（send 无 receiver 时不更新值）。
        let cancel_rx = self.cancel_tx.subscribe();
        self.cancel_tx.send_replace(false);
        tokio::spawn(async move {
            let _ = events.send(UiEvent::TurnStart);
            let mut ui = crate::tui::tui_hooks(events.clone(), asks);
            let history = Self::load_history(&session, &mut ui.on_warning);
            let result = crate::query::run_bash_command(
                &session,
                &command,
                history,
                &mut ui,
                Some(cancel_rx),
            )
            .await;
            match result {
                Ok(outcome) => {
                    Self::finish_turn(&events, &session, &outcome).await;
                }
                Err(e) => {
                    let _ = events.send(UiEvent::Error(e.to_string()));
                }
            }
        });
    }

    /// 对话框键盘输入（对标 CC Select）：
    /// 数字/Enter 确认、↑/↓ 移动焦点、Esc 取消；焦点在 Other 时直接打字。
    /// 返回是否消费。
    pub fn ask_key(&mut self, code: KeyCode) -> bool {
        let Some((request, _)) = &self.pending_ask else {
            return false;
        };
        let options_len = request.options.len();
        let free_text = request.free_text;
        let total = options_len + usize::from(free_text);
        let in_other = free_text && self.ask_focus >= options_len;
        match code {
            KeyCode::Char(c) if in_other && !c.is_control() => {
                self.ask_other.push(c);
                true
            }
            KeyCode::Backspace if in_other => {
                self.ask_other.pop();
                true
            }
            KeyCode::Enter if in_other => {
                let text = std::mem::take(&mut self.ask_other);
                self.submit_ask_answer(text);
                true
            }
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                let index = (c as u8 - b'1') as usize;
                if index < total {
                    self.ask_focus = index;
                    if !(index == options_len && free_text) {
                        self.choose_ask_option(index);
                    }
                }
                true
            }
            KeyCode::Up => {
                if self.ask_focus > 0 {
                    self.ask_focus -= 1;
                }
                true
            }
            KeyCode::Down => {
                if self.ask_focus + 1 < total {
                    self.ask_focus += 1;
                }
                true
            }
            KeyCode::Enter => {
                let focus = self.ask_focus;
                if focus >= options_len && free_text {
                    let text = std::mem::take(&mut self.ask_other);
                    self.submit_ask_answer(text);
                } else {
                    self.choose_ask_option(focus);
                }
                true
            }
            KeyCode::Esc => {
                if let Some((request, tx)) = self.pending_ask.take() {
                    if request.free_text {
                        self.ask_result
                            .get_or_insert_with(AskResult::default)
                            .declined = true;
                    }
                    let _ = tx.send(DialogAction::Cancel);
                }
                true
            }
            _ => false,
        }
    }

    /// 提交 Other 自由输入（CC SelectInputOption onSubmit：空文本 = 取消）。
    fn submit_ask_answer(&mut self, text: String) {
        if text.trim().is_empty() {
            let free_text = self
                .pending_ask
                .as_ref()
                .is_some_and(|(r, _)| r.free_text);
            if free_text {
                self.ask_result.get_or_insert_with(AskResult::default).declined = true;
            }
            if let Some((_, tx)) = self.pending_ask.take() {
                let _ = tx.send(DialogAction::Cancel);
            }
            return;
        }
        if let Some((request, tx)) = self.pending_ask.take() {
            self.ask_result
                .get_or_insert_with(AskResult::default)
                .answered
                .push((request.question.clone(), text.clone()));
            let _ = tx.send(DialogAction::Answer(text));
        }
    }

    /// 确认选项 `index`（0 起；越界 = 取消）。
    fn choose_ask_option(&mut self, index: usize) {
        if let Some((request, tx)) = self.pending_ask.take() {
            if index < request.options.len() {
                if request.free_text {
                    self.ask_result
                        .get_or_insert_with(AskResult::default)
                        .answered
                        .push((request.question.clone(), request.options[index].clone()));
                }
                let _ = tx.send(DialogAction::Confirm(index));
            } else {
                let _ = tx.send(DialogAction::Cancel);
            }
        }
    }

    /// 键盘事件（与旧 event() 语义一致；busy 时 Esc/Ctrl+C 中断回合）。
    /// busy（流式渲染中）打字强制整屏重绘：打字与流式内容交错时行 diff
    /// 可能残留旧行；空闲打字走行 diff——全量重写会把每帧历史累积进
    /// scrollback（slash 菜单/普通打字场景不应产生帧堆积）。
    pub fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if self.busy {
            FORCE_FULL_REDRAW.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if self.ask_key(code) {
            return true;
        }
        // `/model` 二级选择器优先于输入（↑↓/Enter/Esc 全消费）。
        if self.model_menu_key(code, modifiers) {
            return true;
        }
        if self.busy
            && (code == KeyCode::Esc
                || (code == KeyCode::Char('c')
                    && modifiers.contains(KeyModifiers::CONTROL)))
        {
            self.interrupted = true;
            self.cancel_tx.send_replace(true);
            return true;
        }
        if self.typing {
            // slash 下拉键盘（Tab 补全 / Esc 关闭 / ↑↓ 导航）优先于输入。
            if !self.bash_mode && self.slash_menu_key(code, modifiers) {
                return true;
            }
            // bash 模式切换（对标 CC）：输入为空时按 `!` 进入 shell 模式
            //（`!` 本身不插入输入）；bash 模式下空输入按退格退出。
            if !self.bash_mode
                && self.input.is_empty()
                && code == KeyCode::Char('!')
                && !modifiers.contains(KeyModifiers::CONTROL)
            {
                self.bash_mode = true;
                return true;
            }
            if self.bash_mode
                && self.input.is_empty()
                && code == KeyCode::Backspace
            {
                self.bash_mode = false;
                return true;
            }
            match code {
                KeyCode::Char(c)
                    if !c.is_control() && !modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    self.input.push(c);
                    self.update_slash_suggestions();
                    return true;
                }
                KeyCode::Backspace => {
                    self.input.pop();
                    self.update_slash_suggestions();
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
        // slash 临时提示超时消失（操作确认类不留永久占位）。
        if let Some(at) = self.slash_at
            && at.elapsed() > SLASH_OUTPUT_TTL
        {
            self.slash_lines.clear();
            self.slash_at = None;
        }
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

    /// 运行状态行（对标 CC ActivityIndicator）：busy 时返回
    /// `(动词, 已耗时秒)`——优先运行中的工具（summary/名字）、
    /// 其次运行中的 thinking（俏皮词）、兜底 "Working"。
    /// 空闲返回 None（状态行隐藏）。
    pub fn running_status(&self) -> Option<(String, f64)> {
        if !self.busy {
            return None;
        }
        let verb = self
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
            .and_then(|m| {
                m.activities.iter().find_map(|a| match &a.kind {
                    ActivityKind::Tool(t) if t.status == ToolStatus::Running => {
                        Some(if t.summary.is_empty() {
                            t.name.to_string()
                        } else {
                            t.summary.clone()
                        })
                    }
                    // 运行中的后台任务/子代理（对齐 CC ActivityIndicator 显示
                    // agent activeForm）：label 即 `Agent: 描述`。
                    ActivityKind::Watch(w) if w.status == WatchStatus::Running => {
                        Some(w.label.clone())
                    }
                    ActivityKind::Thinking(t) if t.state == ThinkingState::Running => {
                        Some(t.stage.to_string())
                    }
                    _ => None,
                })
            })
            .unwrap_or_else(|| "Working".to_string());
        let elapsed = self
            .turn_started
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        Some((verb, elapsed))
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

    /// 消息是否"定稿"：行内容不再变化（流式停止、无运行中活动）。
    /// REPL 模式：定稿消息的行一次性打印进 scrollback；未定稿的留在
    /// 动态尾部原地重绘。定稿是单向的——一旦为 true，其行永不变。
    fn message_settled(&self, i: usize) -> bool {
        if Some(i) == self.stream_msg {
            return false;
        }
        let m = &self.messages[i];
        !m.groups.iter().any(|g| g.active)
            && !m.activities.iter().any(|a| a.is_running())
    }

    /// 最后一条消息是否仍可变化。REPL 模式据此决定 ctrl+o 折叠是否
    /// 安全：只有仍在动态区（未打印进 scrollback）的消息才能折叠。
    pub fn last_message_dynamic(&self) -> bool {
        self.messages
            .last()
            .is_some_and(|_| !self.message_settled(self.messages.len() - 1))
    }

    /// 构建滚动文档：欢迎卡片 + 消息（text 与活动按插入点交错）+
    /// 权限请求块。`doc.settled` = 前置定稿行数（欢迎卡片 + 全部
    /// 已定稿消息；权限请求块永远不定稿）。
    pub fn build_rows(&mut self, width: usize) -> &Doc {
        // markdown 渲染缓存不区分宽度——宽度变化时清空，
        // 否则 resize 后消息文本沿用旧宽度折行。
        if self.prev_build_width != width {
            self.prev_build_width = width;
            self.reply_cache.clear();
        }
        let mut rows: Vec<Row> = Vec::new();
        let mut click_ranges: Vec<ClickRange> = Vec::new();
        let theme = self.theme.clone();

        rows.extend(welcome_card_rows(
            &theme,
            &self.user,
            &self.session.runtime.model.borrow(),
            self.permission_mode_label(),
            &self.cwd,
            width,
        ));
        let mut settled = rows.len();
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
                        let images = &self.images;
                        let image_cap = self.image_cap;
                        let images_version = self.images_version;
                        move |reply: &str| -> Vec<Line> {
                            if reply.is_empty() {
                                return Vec::new();
                            }
                            if let Some(lines) = cache.get(reply) {
                                return lines.clone();
                            }
                            renderer.set_width(width);
                            // 图片缓存版本变化 → 同步渲染器（清逐块缓存）。
                            if renderer.images_version() != images_version {
                                renderer.set_images(image_cap, images, images_version);
                            }
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
                    // 思考完成行（CC SystemTextMessage `✻ Churned for 40s`）：
                    // 渲染在消息末尾（正文与全部工具之后），取最后一个已完成的
                    // 真实思考块（空占位块不产生完成行）。
                    if let Some(line) = self.messages[i].activities.iter().rev().find_map(
                        |a| match &a.kind {
                            ActivityKind::Thinking(t)
                                if t.state == ThinkingState::Done && !a.content.is_empty() =>
                            {
                                Some(crate::tui::activities::thinking_completion_line(
                                    t, &theme,
                                ))
                            }
                            _ => None,
                        },
                    ) {
                        rows.push(Row::new(line));
                    }
                }
            }
            if self.message_settled(i) {
                settled = rows.len();
            }
        }

        // AskUserQuestion 结果块（CC `● User answered Claude's questions:`）：
        // 对话框答毕后定稿；无待答对话框时随最后一条消息一起定稿。
        if let Some(result) = &self.ask_result
            && !result.is_empty()
        {
            let mut header = Line::styled("⏺ ", SegStyle::fg(theme.text));
            if result.declined && result.answered.is_empty() {
                header.push_styled("User declined to answer questions", theme.text());
            } else {
                header.push_styled("User answered Claude's questions:", theme.text());
            }
            rows.push(Row::new(header));
            for (question, answer) in &result.answered {
                rows.push(Row::new(Line::styled(
                    format!("  · {question} → {answer}"),
                    SegStyle::fg(theme.inactive),
                )));
            }
            if self.pending_ask.is_none()
                && self
                    .messages
                    .last()
                    .is_some_and(|_| self.message_settled(self.messages.len() - 1))
            {
                settled = rows.len();
            }
        }

        // 权限/提问块（对标 CC PermissionDialog / AskUserQuestion）：
        // 标题（permission bold）+ 说明（dim）+ 编号选项（CC Select：
        // `❯ n. label` 焦点指示、desc 副行 dim、Other 自由输入）+ 快捷键提示。
        if let Some((request, _)) = &self.pending_ask {
            let mut title = Line::styled("⏺ ", SegStyle::fg(theme.text));
            title.push_styled(request.title.clone(), theme.permission());
            rows.push(Row::new(title));
            rows.push(Row::new(Line::styled(
                format!("  {}", request.question),
                SegStyle::fg(theme.inactive),
            )));
            let focus_color = theme.permission;
            for (opt_idx, option) in request.options.iter().enumerate() {
                let focused = opt_idx == self.ask_focus;
                let mut line = Line::empty();
                line.push_styled(
                    if focused { "❯ " } else { "  " },
                    if focused {
                        SegStyle::fg(focus_color)
                    } else {
                        SegStyle::fg(theme.text)
                    },
                );
                line.push_styled(
                    format!("{}. {option}", opt_idx + 1),
                    if focused {
                        SegStyle::fg(focus_color)
                    } else {
                        SegStyle::fg(theme.text)
                    },
                );
                let row = rows.len();
                rows.push(Row::new(line));
                click_ranges.push(ClickRange {
                    start: row,
                    end: row + 1,
                    target: ClickTarget::AskOption(opt_idx),
                });
                if let Some(desc) = request
                    .descriptions
                    .get(opt_idx)
                    .and_then(|d| d.as_deref())
                    .filter(|d| !d.is_empty())
                {
                    rows.push(Row::new(Line::styled(
                        format!("   {desc}"),
                        if focused {
                            SegStyle::fg(focus_color)
                        } else {
                            SegStyle::fg(theme.inactive)
                        },
                    )));
                }
            }
            if request.free_text {
                let other_idx = request.options.len();
                let focused = self.ask_focus >= other_idx;
                let mut line = Line::empty();
                line.push_styled(
                    if focused { "❯ " } else { "  " },
                    if focused {
                        SegStyle::fg(focus_color)
                    } else {
                        SegStyle::fg(theme.text)
                    },
                );
                line.push_styled(
                    format!("{}. Other", other_idx + 1),
                    if focused {
                        SegStyle::fg(focus_color)
                    } else {
                        SegStyle::fg(theme.text)
                    },
                );
                let row = rows.len();
                rows.push(Row::new(line));
                click_ranges.push(ClickRange {
                    start: row,
                    end: row + 1,
                    target: ClickTarget::AskOption(other_idx),
                });
                let placeholder = if focused {
                    if self.ask_other.is_empty() {
                        "Type something.".to_string()
                    } else {
                        format!("{}{}", self.ask_other, '▋')
                    }
                } else {
                    "Type something.".to_string()
                };
                rows.push(Row::new(Line::styled(
                    format!("   {placeholder}"),
                    if focused {
                        SegStyle::fg(focus_color)
                    } else {
                        SegStyle::fg(theme.inactive)
                    },
                )));
            }
            let hint = if request.free_text && self.ask_focus >= request.options.len() {
                "Enter to submit · Esc to cancel"
            } else {
                "Enter to select · ↑/↓ to navigate · Esc to cancel"
            };
            rows.push(Row::new(Line::styled(
                format!("  {hint}"),
                SegStyle::fg(theme.inactive),
            )));
        }

        // slash 命令输出（/help /status /compact 等）：临时提示——渲染在消息之后、
        // 输入框上方，**不定稿不落盘**，tick 超时（SLASH_OUTPUT_TTL）后自动消失。
        if !self.slash_lines.is_empty() {
            for line in &self.slash_lines {
                rows.push(Row::new(Line::styled(
                    line.clone(),
                    SegStyle::fg(theme.text),
                )));
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
            settled,
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
            styled.image = line.image.clone();
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
    use base64::Engine;
    use serde_json::json;

    /// 测试用 Chat：独立通道 + 完整 Session。
    pub(super) fn test_chat() -> Chat {
        test_chat_home(std::env::temp_dir())
    }

    /// 自建 home 的 Chat（slash 测试用唯一目录，避免与其他测试共享
    /// transcript/task 存储）。
    fn test_chat_home(home: std::path::PathBuf) -> Chat {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (asks_tx, asks_rx) = mpsc::unbounded_channel();
        let session = Arc::new(Session {
            client: crate::api::client::Client::new(
                "test-key".to_string(),
                "https://example.com".to_string(),
            ),
            runtime: crate::query::Runtime::new("test-model".to_string(), None, Default::default()),
            permission_mode: PermissionMode::Default,
            settings: crate::settings::Settings::default(),
            system: Vec::new(),
            depth: 0,
            home: home.clone(),
            quiet: true,
            compact_failures: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(
                &home,
                "test",
            )),
            last_task_reminder_turn: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            expand_tasks: tokio::sync::watch::channel(false).0,
        });
        Chat::new(session, events_tx, events_rx, asks_tx, asks_rx, Theme::dark(), None)
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
                standalone: false,
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

    /// Task 工具族 / AskUserQuestion 的调用不在 transcript 展示
    /// （对标 CC renderToolUseMessage = null；任务区面板 / 对话框即展示）。
    #[test]
    fn hidden_tools_produce_no_activities() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        for name in ["TaskCreate", "TaskUpdate", "TaskGet", "TaskList", "AskUserQuestion"] {
            let _ = chat.events.send(UiEvent::ToolStart { name: name.into() });
            chat.drain_events();
            let _ = chat.events.send(UiEvent::ToolReady {
                name: name.into(),
                input: json!({}),
                standalone: false,
            });
            chat.drain_events();
        }
        assert!(
            chat.messages[0].activities.is_empty(),
            "hidden tools leave no activities: {:?}",
            chat.messages[0].activities
        );
        assert!(chat.pending_tools.is_empty(), "pending FIFO 不失配");
        // 可见工具仍正常展示。
        let _ = chat.events.send(UiEvent::ToolStart { name: "Bash".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Bash".into(),
            input: json!({"command": "ls"}),
            standalone: false,
        });
        chat.drain_events();
        assert_eq!(chat.messages[0].activities.len(), 1, "Bash 正常展示");
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

    /// 运行状态行数据（对标 CC ActivityIndicator）：空闲 None；
    /// busy 时优先运行中工具 summary、其次 thinking 俏皮词、兜底 Working。
    #[test]
    fn running_status_verb_priority() {
        let mut chat = test_chat();
        assert_eq!(chat.running_status(), None, "空闲无状态行");

        chat.busy = true;
        chat.turn_started = Some(std::time::Instant::now());
        let (verb, _) = chat.running_status().unwrap();
        assert_eq!(verb, "Working", "无活动时兜底");

        let mut tool = tool_activity();
        if let ActivityKind::Tool(t) = &mut tool.kind {
            t.summary = "$ cargo test".to_string();
        }
        chat.messages.push(UiMessage {
            activities: vec![tool],
            ..msg(Role::Assistant, "")
        });
        let (verb, _) = chat.running_status().unwrap();
        assert_eq!(verb, "$ cargo test", "运行中工具 summary 优先");

        // 运行中的 Watch（子代理/后台任务）动词 = label（CC ActivityIndicator
        // 显示 agent activeForm）：工具之后、thinking 之前。
        chat.messages[0].activities.clear();
        chat.messages[0].activities.push(Activity::new(ActivityKind::Watch(
            WatchCall {
                label: "Agent: 列出桌面目录内容".into(),
                status: WatchStatus::Running,
                detail: Some("已产出 43 字符".into()),
                duration_ms: 0,
            },
        )));
        let (verb, _) = chat.running_status().unwrap();
        assert_eq!(verb, "Agent: 列出桌面目录内容", "Watch Running 动词 = label");

        // Done 的 Watch 不再占用动词（落到 thinking/Working）。
        if let ActivityKind::Watch(w) = &mut chat.messages[0].activities[0].kind {
            w.status = WatchStatus::Done;
        }
        let (verb, _) = chat.running_status().unwrap();
        assert_ne!(verb, "Agent: 列出桌面目录内容", "Done 的 Watch 不占动词");

        chat.messages[0].activities.clear();
        chat.apply_turn_start();
        // TurnStart 追加新消息（索引 1）：占位 thinking 在其中。
        let stage = match &chat.messages[1].activities[0].kind {
            ActivityKind::Thinking(t) => t.stage,
            _ => unreachable!(),
        };
        let (verb, _) = chat.running_status().unwrap();
        assert_eq!(verb, stage, "thinking 俏皮词");
    }

    /// bash 模式切换（对标 CC）：空输入按 `!` 进入、`!` 不插入输入、
    /// 输入非空时 `!` 正常插入、空输入退格退出。
    #[test]
    fn bang_toggles_bash_mode() {
        let mut chat = test_chat();
        assert!(!chat.bash_mode);
        assert!(chat.on_key(KeyCode::Char('!'), KeyModifiers::empty()));
        assert!(chat.bash_mode, "! 进入 bash 模式");
        assert!(chat.input.is_empty(), "! 本身不插入输入");
        assert!(chat.on_key(KeyCode::Char('l'), KeyModifiers::empty()));
        assert_eq!(chat.input, "l");
        assert!(chat.on_key(KeyCode::Char('!'), KeyModifiers::empty()));
        assert_eq!(chat.input, "l!", "输入非空时 ! 正常插入");
        assert!(chat.bash_mode, "输入非空不退出 bash 模式");
        assert!(chat.on_key(KeyCode::Backspace, KeyModifiers::empty()));
        assert!(chat.on_key(KeyCode::Backspace, KeyModifiers::empty()));
        assert!(chat.on_key(KeyCode::Backspace, KeyModifiers::empty()));
        assert!(!chat.bash_mode, "空输入退格退出 bash 模式");
    }

    /// `!` 命令（standalone 工具活动）：不参与折叠组，完成后默认展开，
    /// 预览 = 输出本身（去掉 `$ cmd` 回显与 `[Exited with code N]` 尾注）。
    #[test]
    fn bash_preview_expands_with_output() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::ToolStart { name: "Bash".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Bash".into(),
            input: json!({"command": "ls"}),
            standalone: true,
        });
        chat.drain_events();
        assert!(chat.messages[0].groups.is_empty(), "standalone 不折叠");
        let _ = chat.events.send(UiEvent::ToolDone(crate::query::ToolCallDone {
            name: "Bash".into(),
            summary: "$ ls".into(),
            output: "$ ls\nREADME.md\nsrc\n[Exited with code 0]".into(),
            is_error: false,
            duration_ms: 5,
            diff: None,
        }));
        chat.drain_events();
        let a = &chat.messages[0].activities[0];
        assert!(a.expanded, "输出预览默认展开");
        let text: Vec<String> = a.content.iter().map(|l| l.plain_text()).collect();
        assert_eq!(
            text,
            vec!["README.md", "src"],
            "预览去掉回显与退出码: {text:?}"
        );
    }

    /// 模型驱动的 Bash（standalone=false）仍按原样折叠成组。
    #[test]
    fn model_bash_still_folds_into_group() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::ToolStart { name: "Bash".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Bash".into(),
            input: json!({"command": "cargo test"}),
            standalone: false,
        });
        chat.drain_events();
        assert_eq!(chat.messages[0].groups.len(), 1, "模型驱动照常折叠");
    }

    /// bash 模式提交：用户消息带 `!` 前缀，命令经工具活动执行并正常收尾
    /// （respondToBashCommands=false → 无模型调用，回合结束 busy 复位）。
    #[tokio::test]
    async fn bash_submit_runs_command_and_ends_turn() {
        let session = Arc::new(Session {
            client: crate::api::client::Client::new("k".into(), "http://127.0.0.1:9".into()),
            runtime: crate::query::Runtime::new("m".into(), None, Default::default()),
            permission_mode: PermissionMode::BypassPermissions,
            settings: crate::settings::Settings {
                respond_to_bash_commands: Some(false),
                ..Default::default()
            },
            system: Vec::new(),
            depth: 0,
            home: std::env::temp_dir(),
            quiet: true,
            compact_failures: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(
                &std::env::temp_dir(),
                "test",
            )),
            last_task_reminder_turn: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            expand_tasks: tokio::sync::watch::channel(false).0,
        });
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (asks_tx, asks_rx) = mpsc::unbounded_channel();
        let mut chat = Chat::new(session, events_tx, events_rx, asks_tx, asks_rx, Theme::dark(), None);
        chat.bash_mode = true;
        chat.input = "echo hello".to_string();
        chat.submit();
        assert!(chat.bash_mode, "提交后保持 bash 模式");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            chat.drain_all();
            if !chat.busy && !chat.messages.is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "回合未在超时内结束"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(chat.messages[0].text, "!echo hello", "用户消息带 ! 前缀");
        let done_tool = chat.messages[1].activities.iter().any(|a| {
            matches!(&a.kind, ActivityKind::Tool(t)
                if t.name == "Bash" && t.status == ToolStatus::Done)
        });
        assert!(done_tool, "Bash 工具活动收口为 Done");
        let preview = &chat.messages[1].activities[0];
        assert!(preview.expanded, "! 命令输出预览展开");
        assert!(
            preview
                .content
                .iter()
                .any(|l| l.plain_text() == "hello"),
            "预览含命令输出: {:?}",
            preview.content.iter().map(|l| l.plain_text()).collect::<Vec<_>>()
        );
        assert!(!chat.busy, "回合结束");
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

    /// 思考完成行（CC SystemTextMessage `✻ Churned for 40s`）渲染在消息末尾：
    /// 正文与全部活动之后；空占位思考（无内容）不产生完成行。
    #[test]
    fn thinking_completion_line_renders_at_message_end() {
        let mut chat = test_chat();
        chat.apply_turn_start();
        chat.apply_event(UiEvent::ThinkingDelta("plan".into()));
        let mut done = chat.messages[0].activities[0].clone();
        if let ActivityKind::Thinking(t) = &mut done.kind {
            t.state = ThinkingState::Done;
            t.duration_ms = 3300;
            t.done_verb = Some("Baked");
        }
        chat.messages[0].activities[0] = done;
        chat.messages[0].text = "你好！".to_string();
        chat.build_rows(100);
        let joined: Vec<String> = chat
            .doc
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect();
        let lines: Vec<&str> = joined.iter().map(String::as_str).collect();
        let thinking = lines
            .iter()
            .position(|l| l.contains("✻ Thinking"))
            .expect("thinking block header");
        let reply = lines
            .iter()
            .position(|l| l.contains("你好"))
            .expect("reply text");
        let done_line = lines
            .iter()
            .position(|l| l.contains("✻ Baked for 3.3s"))
            .expect("completion line");
        assert!(
            thinking < reply && reply < done_line,
            "完成行在消息末尾: {lines:?}"
        );

        // 空占位思考（无内容）→ 无完成行。
        let mut chat2 = test_chat();
        chat2.apply_turn_start();
        let mut ph = chat2.messages[0].activities[0].clone();
        if let ActivityKind::Thinking(t) = &mut ph.kind {
            t.state = ThinkingState::Done;
            t.duration_ms = 400;
        }
        chat2.messages[0].activities[0] = ph;
        chat2.build_rows(100);
        let joined2: String = chat2
            .doc
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!joined2.contains("for 0.4s"), "空占位无完成行: {joined2}");
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
    // slash 命令（/help /model /clear /exit /theme /rename /resume
    // /permissions /skills /tasks /compact）
    // ------------------------------------------------------------------

    /// 输入层拦截：/ 开头不启动回合；/help 输出清单、未知命令给提示。
    #[test]
    fn slash_intercepts_and_help_lists_commands() {
        let mut chat = test_chat();
        chat.input = "/help".to_string();
        chat.submit();
        assert!(!chat.busy, "slash 不启动回合");
        let joined = chat.slash_lines.join("\n");
        for cmd in ["/clear", "/model", "/resume", "/rename", "/compact", "/exit"] {
            assert!(joined.contains(cmd), "缺少 {cmd}: {joined}");
        }

        chat.input = "/nope".to_string();
        chat.submit();
        assert!(
            chat.slash_lines.iter().any(|l| l.contains("未知命令")),
            "{joined}"
        );
    }

    /// /model：无参显示当前模型；带参切换运行时模型（下一轮生效）。
    #[test]
    fn slash_model_switches_runtime_model() {
        let mut chat = test_chat();
        chat.input = "/model deepseek-v4".to_string();
        chat.submit();
        assert_eq!(*chat.session.runtime.model.borrow(), "deepseek-v4");
        chat.input = "/model".to_string();
        chat.submit();
        assert!(chat.slash_lines.join("\n").contains("deepseek-v4"));
    }

    /// /exit 置退出标志（组件层消费 → system.exit）。
    #[test]
    fn slash_exit_requests_shutdown() {
        let mut chat = test_chat();
        chat.input = "/exit".to_string();
        chat.submit();
        assert!(chat.exit);
    }

    /// /clear：清空 UI 消息并换新 transcript（任务列表键随会话隔离，M0 不跟随）。
    #[test]
    fn slash_clear_resets_session() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::User, "hi"));
        chat.input = "/clear".to_string();
        chat.submit();
        assert!(chat.messages.is_empty(), "UI 消息清空");
        assert!(
            chat.session.runtime.transcript.borrow().is_some(),
            "新 transcript"
        );
    }

    /// /theme：重建主题（dark → light 渲染差异）+ 持久化到 .bingo/settings.json。
    #[test]
    fn slash_theme_switches_and_persists() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-theme", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mut chat = test_chat();
        chat.cwd = tmp.display().to_string();
        let dark_text = chat.theme.text;
        chat.input = "/theme light".to_string();
        chat.submit();
        assert_ne!(chat.theme.text, dark_text, "主题已切换");
        let saved = std::fs::read_to_string(tmp.join(".bingo/settings.json")).unwrap();
        assert!(saved.contains("\"theme\": \"light\""), "{saved}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// /rename：重命名 transcript 文件并更新运行时引用。
    #[test]
    fn slash_rename_renames_transcript() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-rename", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        let t = crate::transcript::create(&home, &tmp).unwrap();
        // create 只建目录；先落一条消息让文件存在。
        let _ = t.append(&crate::api::types::Message::user_text("hi"));
        let mut chat = test_chat();
        let _ = chat.session.runtime.transcript_tx.send(Some(t));
        chat.input = "/rename my-session".to_string();
        chat.submit();
        let t = chat.session.runtime.transcript.borrow().clone().unwrap();
        assert!(t.name().contains("my-session"), "{}", t.name());
        assert!(t.path().exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// /resume：无参列出全部会话；带参按关键词切换运行时 transcript。
    #[test]
    fn slash_resume_lists_and_switches() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-resume", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        let t_a = crate::transcript::create(&home, &tmp).unwrap();
        let _ = t_a.append(&crate::api::types::Message::user_text("a"));
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let t_b = crate::transcript::create(&home, &tmp).unwrap();
        let _ = t_b.append(&crate::api::types::Message::user_text("b"));
        let mut chat = test_chat_home(home.clone());
        let _ = chat.session.runtime.transcript_tx.send(Some(t_a));
        let name_b = t_b.name();
        chat.input = "/resume".to_string();
        chat.submit();
        let joined = chat.slash_lines.join("\n");
        assert!(joined.contains(&name_b), "列出会话: {joined}");

        chat.input = format!("/resume {name_b}");
        chat.submit();
        let current = chat.session.runtime.transcript.borrow().clone().unwrap();
        assert_eq!(current.name(), name_b, "切换到目标会话");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// /permissions：列出规则；添加规则 → 运行时表 + settings.json 持久化。
    #[test]
    fn slash_permissions_adds_and_lists() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-perms", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mut chat = test_chat();
        chat.cwd = tmp.display().to_string();
        chat.input = "/permissions".to_string();
        chat.submit();
        assert!(chat.slash_lines.join("\n").contains("allow: （无）"));

        chat.input = "/permissions allow Skill(review:*)".to_string();
        chat.submit();
        let rules = chat
            .session
            .runtime
            .permissions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert!(rules.allow.iter().any(|r| r == "Skill(review:*)"));
        let saved = std::fs::read_to_string(tmp.join(".bingo/settings.json")).unwrap();
        assert!(saved.contains("Skill(review:*)"), "{saved}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// /skills：项目层技能目录加载并列出。
    #[test]
    fn slash_skills_lists_project_skills() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-skills", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let skill = tmp.join(".bingo/skills/pdf/SKILL.md");
        std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
        std::fs::write(
            &skill,
            "---\ndescription: Converts documents to PDF\n---\nbody\n",
        )
        .unwrap();
        let mut chat = test_chat();
        chat.cwd = tmp.display().to_string();
        chat.input = "/skills".to_string();
        chat.submit();
        assert!(
            chat.slash_lines.join("\n").contains("- pdf: Converts documents to PDF"),
            "{}",
            chat.slash_lines.join("\n")
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// /tasks：列出任务区（Todo 列表）。用独立 home 避免污染共享测试 store。
    #[tokio::test]
    async fn slash_tasks_lists_todos() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-tasks", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mut chat = test_chat_home(tmp.join("home"));
        chat.input = "/tasks".to_string();
        chat.submit();
        let empty = chat.slash_lines.join("\n");
        assert!(empty.contains("没有后台任务"), "{empty}");

        let store = chat.session.tasks.clone();
        let id = store
            .create(&crate::tasks::Task {
                id: String::new(),
                subject: "do things".into(),
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
        chat.input = "/tasks".to_string();
        chat.submit();
        let listed = chat.slash_lines.join("\n");
        let _ = store.delete(&id).await;
        assert!(listed.contains("do things"), "{listed}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// slash 输出渲染为消息之后的定稿行（inline 落盘边界）。
    #[test]
    /// slash 输出是临时提示：渲染在消息之后、输入框上方，不定稿（不落盘）。
    #[test]
    fn slash_output_rows_render_transient() {
        let mut chat = test_chat();
        chat.input = "/help".to_string();
        chat.submit();
        chat.build_rows(100);
        assert_ne!(
            chat.doc.settled,
            chat.doc.rows.len(),
            "slash 输出不定稿（不落盘）"
        );
        let joined: Vec<String> = chat
            .doc
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect();
        assert!(joined.iter().any(|l| l.contains("/model")), "{joined:?}");

        // TTL 后 tick 清空：临时提示消失。
        chat.slash_at = Some(std::time::Instant::now() - SLASH_OUTPUT_TTL - std::time::Duration::from_millis(1));
        chat.tick();
        assert!(chat.slash_lines.is_empty(), "超时后 slash 输出消失");
        assert!(chat.slash_at.is_none());
    }

    /// 内置/磁盘技能经 `/技能名` 展开为提示词提交（对齐 CC prompt Command）。
    #[tokio::test]
    async fn slash_skill_expands_and_submits() {
        let mut chat = test_chat();
        chat.input = "/guide".to_string();
        chat.submit();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            chat.drain_all();
            if !chat.busy && !chat.messages.is_empty() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "技能回合未结束");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            chat.messages[0].text.contains("诊断指南"),
            "消息为展开后的技能内容: {}",
            &chat.messages[0].text[..chat.messages[0].text.len().min(80)]
        );
        assert!(
            !chat.messages[0].text.starts_with("/guide"),
            "技能名被展开而非原样发送"
        );
    }

    /// 未知 slash 命令仍提示 /help（技能名不匹配时不误消费）。
    #[test]
    fn slash_unknown_still_guides() {
        let mut chat = test_chat();
        chat.input = "/nope-skill".to_string();
        chat.submit();
        let joined = chat.slash_lines.join("\n");
        assert!(joined.contains("未知命令: /nope-skill"), "{joined}");
        assert!(chat.messages.is_empty(), "未知命令不启动回合");
    }

    #[test]
    fn slash_provider_lists_and_switches() {
        let mut chat = test_chat();
        chat.input = "/provider".to_string();
        chat.submit();
        let out = chat.slash_lines.join("\n");
        assert!(out.contains("当前 provider: default"), "{out}");

        // 配置一个命名 provider 后切换。
        let providers = std::collections::HashMap::from([(
            "deepseek".to_string(),
            crate::settings::ProviderConfig {
                api_key: "sk-ds".into(),
                api_base_url: "https://api.deepseek.com".into(),
            },
        )]);
        Arc::get_mut(&mut chat.session).unwrap().client =
            crate::api::client::Client::new("sk-main".into(), "https://main.example".into());
        // set_provider 需要 providers 表——通过 from_settings 构造更直接。
        drop(providers);
        let mut settings = crate::settings::Settings::default();
        settings.api_key = Some("sk-main".into());
        settings.providers.insert(
            "deepseek".to_string(),
            crate::settings::ProviderConfig {
                api_key: "sk-ds".into(),
                api_base_url: "https://api.deepseek.com".into(),
            },
        );
        Arc::get_mut(&mut chat.session).unwrap().client =
            crate::api::client::Client::from_settings(&settings).unwrap();

        chat.input = "/provider".to_string();
        chat.submit();
        let out = chat.slash_lines.join("\n");
        assert!(out.contains("deepseek"), "{out}");

        chat.input = "/provider deepseek".to_string();
        chat.submit();
        let out = chat.slash_lines.join("\n");
        assert!(out.contains("✓ provider 已切换: deepseek"), "{out}");
        assert_eq!(
            *chat.session.runtime.provider.borrow(),
            "deepseek",
            "runtime provider 同步"
        );

        chat.input = "/provider nope".to_string();
        chat.submit();
        let out = chat.slash_lines.join("\n");
        assert!(out.contains("未找到 provider"), "{out}");
    }

    #[test]
    fn slash_think_sets_level_and_persists() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-think", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mut chat = test_chat_home(tmp.join("home"));
        chat.cwd = tmp.display().to_string();

        chat.input = "/think".to_string();
        chat.submit();
        let out = chat.slash_lines.join("\n");
        assert!(out.contains("当前思考级别: off"), "{out}");

        chat.input = "/think high".to_string();
        chat.submit();
        let out = chat.slash_lines.join("\n");
        assert!(out.contains("✓ 思考级别已设置: high"), "{out}");
        assert_eq!(
            chat.session.runtime.thinking.borrow().as_deref(),
            Some("high")
        );
        let saved: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.join(".bingo/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["thinkingLevel"], "high");

        chat.input = "/think off".to_string();
        chat.submit();
        let out = chat.slash_lines.join("\n");
        assert!(out.contains("✓ 思考级别已设置: off"), "{out}");
        assert_eq!(chat.session.runtime.thinking.borrow().as_deref(), None);

        chat.input = "/think bogus".to_string();
        chat.submit();
        let out = chat.slash_lines.join("\n");
        assert!(out.contains("用法: /think"), "{out}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ------------------------------------------------------------------
    // /mcp：列出 / enable|disable（持久化名单）/ reconnect
    // ------------------------------------------------------------------

    async fn slash_mcp_wait(chat: &mut Chat) -> String {
        let start = chat.slash_lines.len();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            chat.drain_all();
            let output: Vec<String> = chat.slash_lines[start..]
                .iter()
                .filter(|l| !l.starts_with('⏳'))
                .map(|l| l.to_string())
                .collect();
            if !output.is_empty() {
                return output.join("\n");
            }
            assert!(std::time::Instant::now() < deadline, "slash 输出超时");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn slash_mcp_lists_unconfigured() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-mcp1", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mut chat = test_chat_home(tmp.join("home"));
        chat.cwd = tmp.display().to_string();
        chat.input = "/mcp".to_string();
        chat.submit();
        let out = slash_mcp_wait(&mut chat).await;
        assert!(out.contains("未配置 MCP 服务器"), "{out}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn slash_mcp_enable_disable_persists_and_lists() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-mcp2", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mut chat = test_chat_home(tmp.join("home"));
        chat.cwd = tmp.display().to_string();
        Arc::get_mut(&mut chat.session).unwrap().runtime.mcp =
            Arc::new(tokio::sync::Mutex::new(crate::mcp::McpManager::new(
                std::collections::HashMap::from([(
                    "files".to_string(),
                    crate::settings::McpServerConfig {
                        kind: None,
                        command: Some("/bin/echo".to_string()),
                        args: Vec::new(),
                        env: Default::default(),
                        url: None,
                        headers: Default::default(),
                    },
                )]),
                Default::default(),
            ),
        ));
        chat.input = "/mcp".to_string();
        chat.submit();
        let out = slash_mcp_wait(&mut chat).await;
        assert!(out.contains("MCP 服务器（1 个）"), "{out}");
        assert!(out.contains("files"), "{out}");

        chat.input = "/mcp disable files".to_string();
        chat.submit();
        let out = slash_mcp_wait(&mut chat).await;
        assert!(out.contains("已禁用 1 个 MCP 服务器: files"), "{out}");
        // 持久化到 .bingo/settings.json
        let saved: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.join(".bingo/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["disabledMcpServers"], serde_json::json!(["files"]));
        // 列表显示 disabled
        chat.input = "/mcp".to_string();
        chat.submit();
        let out = slash_mcp_wait(&mut chat).await;
        assert!(out.contains("files  disabled"), "{out}");

        chat.input = "/mcp enable all".to_string();
        chat.submit();
        let out = slash_mcp_wait(&mut chat).await;
        assert!(out.contains("已启用 1 个 MCP 服务器: files"), "{out}");
        chat.input = "/mcp".to_string();
        chat.submit();
        let out = slash_mcp_wait(&mut chat).await;
        assert!(!out.contains("disabled"), "{out}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn slash_mcp_reconnect_unknown_server() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-mcp3", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mut chat = test_chat_home(tmp.join("home"));
        chat.cwd = tmp.display().to_string();
        Arc::get_mut(&mut chat.session).unwrap().runtime.mcp =
            Arc::new(tokio::sync::Mutex::new(crate::mcp::McpManager::new(
                std::collections::HashMap::from([(
                    "files".to_string(),
                    crate::settings::McpServerConfig {
                        kind: None,
                        command: Some("/bin/echo".to_string()),
                        args: Vec::new(),
                        env: Default::default(),
                        url: None,
                        headers: Default::default(),
                    },
                )]),
                Default::default(),
            ),
        ));
        chat.input = "/mcp reconnect nope".to_string();
        chat.submit();
        let out = slash_mcp_wait(&mut chat).await;
        assert!(out.contains("未找到 MCP 服务器 \"nope\""), "{out}");
        // 重连失败服务器：失败详情透出
        chat.input = "/mcp reconnect files".to_string();
        chat.submit();
        let out = slash_mcp_wait(&mut chat).await;
        assert!(out.contains("files"), "{out}");
        assert!(out.contains("握手失败") || out.contains("✗"), "{out}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ------------------------------------------------------------------
    // slash 下拉建议（/ 输入弹出，Tab 补全 / ↑↓ 导航 / Enter 执行 / Esc 关闭）
    // ------------------------------------------------------------------

    // ------------------------------------------------------------------
    // slash 下拉建议（/ 输入弹出，Tab 补全 / ↑↓ 导航 / Enter 执行 / Esc 关闭）
    // ------------------------------------------------------------------

    /// 输入 `/` → 建议列出内置命令；带参数后消失。
    #[test]
    fn slash_menu_lists_commands_and_hides_with_args() {
        let mut chat = test_chat();
        chat.input = "/".to_string();
        chat.update_slash_suggestions();
        assert_eq!(
            chat.slash_suggestions.len(),
            SLASH_SUGGESTIONS_MAX.min(SLASH_COMMANDS.len()),
            "下拉最多 5 行（对齐 CC OVERLAY_MAX_ITEMS）"
        );
        assert!(chat.slash_suggestions.iter().any(|s| s.name == "model"));

        chat.input = "/model deepseek".to_string();
        chat.update_slash_suggestions();
        assert!(chat.slash_suggestions.is_empty(), "带参数不显示");

        chat.input = "hi".to_string();
        chat.update_slash_suggestions();
        assert!(chat.slash_suggestions.is_empty(), "非 / 开头不显示");
    }

    /// 前缀过滤 + 技能并入（项目层技能目录）。
    #[test]
    fn slash_menu_filters_by_prefix_and_includes_skills() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-menu", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let skill = tmp.join(".bingo/skills/pdf/SKILL.md");
        std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
        std::fs::write(&skill, "---\ndescription: PDF tool\n---\nbody\n").unwrap();

        let mut chat = test_chat();
        chat.cwd = tmp.display().to_string();
        chat.input = "/p".to_string();
        chat.update_slash_suggestions();
        assert!(
            chat.slash_suggestions.iter().any(|s| s.name == "pdf"),
            "技能并入建议"
        );

        chat.input = "/mo".to_string();
        chat.update_slash_suggestions();
        let names: Vec<&str> = chat
            .slash_suggestions
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(names, vec!["model"], "前缀过滤: {names:?}");

        // 超长描述截断（对齐 CC MAX_LISTING_DESC_CHARS）：
        // NoWrap 超长行会把 canvas 撑出终端宽 → 行 diff 错位残留。
        let long = "x".repeat(400);
        std::fs::write(
            &skill,
            format!("---\ndescription: {long}\n---\nbody\n"),
        )
        .unwrap();
        chat.input = "/p".to_string();
        chat.update_slash_suggestions();
        let desc = chat
            .slash_suggestions
            .iter()
            .find(|s| s.name == "pdf")
            .map(|s| s.description.clone())
            .expect("pdf 技能在建议中");
        assert!(
            desc.chars().count() <= crate::skills::MAX_LISTING_DESC_CHARS,
            "描述截断: {} 字符",
            desc.chars().count()
        );
        assert!(desc.ends_with('…'), "截断带省略号: {desc}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// ↑/↓ 移动选中（消费按键，不触发滚动）；Tab 补全 `/name ` 不执行。
    #[test]
    fn slash_menu_navigation_and_tab_completion() {
        let mut chat = test_chat();
        chat.input = "/".to_string();
        chat.update_slash_suggestions();
        assert_eq!(chat.slash_selected, 0);

        assert!(chat.slash_menu_key(KeyCode::Down, KeyModifiers::empty()));
        assert_eq!(chat.slash_selected, 1);
        assert!(chat.slash_menu_key(KeyCode::Up, KeyModifiers::empty()));
        assert_eq!(chat.slash_selected, 0);
        assert!(chat.slash_menu_key(KeyCode::Up, KeyModifiers::empty()));
        assert_eq!(
            chat.slash_selected,
            chat.slash_suggestions.len() - 1,
            "顶部回卷"
        );

        // Tab 应用选中（/help）→ `/help ` 且建议清空、未执行。
        chat.input = "/".to_string();
        chat.update_slash_suggestions();
        chat.slash_selected = 0;
        assert!(chat.slash_menu_key(KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(chat.input, "/help ");
        assert!(chat.slash_suggestions.is_empty());
        assert!(chat.slash_lines.is_empty(), "Tab 不执行");

        // Esc 关闭。
        chat.input = "/".to_string();
        chat.update_slash_suggestions();
        assert!(chat.slash_menu_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(chat.slash_suggestions.is_empty());
    }

    /// Enter：部分前缀 → 应用选中并执行；完整命令 → 原样执行。
    #[tokio::test]
    async fn slash_menu_enter_applies_and_executes() {
        let mut chat = test_chat();
        // 完整命令：直接执行，建议菜单必须关闭（不留占位行）。
        chat.input = "/model".to_string();
        chat.update_slash_suggestions();
        assert!(
            !chat.slash_suggestions.is_empty(),
            "输入 /model 时有建议: {:?}",
            chat.slash_suggestions
        );
        chat.submit();
        assert!(
            chat.model_menu.is_some(),
            "/model 进入二级选择器（一级 endpoint 列表）"
        );
        assert!(chat.slash_suggestions.is_empty(), "菜单模式无 slash 建议");
        assert!(!chat.busy);
        // Esc 退出菜单。
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(chat.model_menu.is_none(), "Esc 退出菜单");

        // 部分前缀 `/sta`：Enter 应用选中（status 在前）并执行。
        chat.input = "/sta".to_string();
        chat.update_slash_suggestions();
        assert!(
            chat.slash_suggestions.iter().any(|s| s.name == "status"),
            "有建议: {:?}",
            chat.slash_suggestions
        );
        chat.submit();
        assert!(
            chat.slash_lines.join("\n").contains("⏳"),
            "status 已执行（异步统计提示）"
        );
        assert!(chat.slash_suggestions.is_empty(), "部分前缀执行后菜单关闭");
    }

    /// `/model` 二级选择器：Enter 进入菜单（一级 endpoint 列表），
    /// 移动选中 → Enter 进二级（loading）→ Esc 逐级退出。
    #[tokio::test]
    async fn model_menu_two_stage_navigation() {
        let mut chat = test_chat();
        chat.input = "/model".to_string();
        chat.submit();
        let Some(menu) = &chat.model_menu else {
            panic!("菜单未打开");
        };
        assert_eq!(menu.providers, vec!["default"], "一级列表含当前 endpoint");
        assert!(menu.models.is_none(), "停在一级");
        assert!(
            chat.on_key(KeyCode::Down, KeyModifiers::empty()),
            "↓ 移动选中"
        );
        assert_eq!(
            chat.model_menu.as_ref().unwrap().provider_selected,
            0,
            "单元素列表循环回 0"
        );
        // Enter 进入二级：异步拉取中（loading）。
        assert!(chat.on_key(KeyCode::Enter, KeyModifiers::empty()));
        let m = &chat.model_menu.as_ref().unwrap().models;
        assert!(m.is_some(), "已进入二级");
        assert!(m.as_ref().unwrap().loading, "拉取中");
        // Esc 逐级返回：二级 → 一级 → 退出。
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(
            chat.model_menu.as_ref().is_some_and(|m| m.models.is_none()),
            "二级 Esc 回一级"
        );
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(chat.model_menu.is_none(), "一级 Esc 整体退出");
    }

    /// 二级确认：模型被选中 → 切换运行时模型并退出菜单。
    #[tokio::test]
    async fn model_menu_picks_model_and_switches() {
        let mut chat = test_chat();
        chat.input = "/model".to_string();
        chat.submit();
        chat.on_key(KeyCode::Enter, KeyModifiers::empty());
        if let Some(m) = &mut chat.model_menu.as_mut().unwrap().models {
            m.models = vec!["claude-sonnet-5".to_string(), "claude-opus-4".to_string()];
            m.loading = false;
            m.selected = 1;
        }
        assert!(chat.on_key(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(
            *chat.session.runtime.model.borrow(),
            "claude-opus-4",
            "选中的模型生效"
        );
        assert!(chat.model_menu.is_none(), "确认后关闭菜单");
        assert!(
            chat.slash_lines.join("\n").contains("模型已切换"),
            "确认提示"
        );
    }

    /// footer 徽标：带思考等级时显示 `模型 · think 等级`，off 只显示模型名。
    #[test]
    fn footer_model_label_shows_thinking_level() {
        assert_eq!(
            model_footer_label("claude-sonnet-5", Some("high")),
            "claude-sonnet-5 · think high"
        );
        assert_eq!(model_footer_label("claude-sonnet-5", None), "claude-sonnet-5");
        assert_eq!(
            model_footer_label("claude-sonnet-5", Some("off")),
            "claude-sonnet-5"
        );
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
                standalone: false,
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
            standalone: false,
        });
        let _ = chat.events.send(UiEvent::ToolStart { name: "WebSearch".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "WebSearch".into(),
            input: json!({"query": "rust"}),
            standalone: false,
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
            standalone: false,
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
            standalone: false,
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
        let _ = chat.events.send(UiEvent::ToolStart { name: "Skill".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Skill".into(),
            input: json!({"name": "pdf", "arguments": "doc.md"}),
            standalone: false,
        });
        chat.drain_events();
        let joined = visible(&mut chat, 120, 30);
        let flat = joined.replace(' ', "");
        assert!(
            flat.contains("arguments=\"doc.md\""),
            "running header shows input summary: {joined}"
        );
        // 完成后 duration 用真实值
        let _ = chat.events.send(UiEvent::ToolDone(crate::query::ToolCallDone {
            name: "Skill".into(),
            summary: "arguments=\"doc.md\"".into(),
            output: "line".into(),
            is_error: false,
            diff: None,
            duration_ms: 3210,
        }));
        chat.drain_events();
        let joined = visible(&mut chat, 120, 30);
        assert!(joined.contains("3210ms"), "real duration: {joined}");
    }

    /// Agent 对齐 CC Task renderToolUseMessage=null：ToolStart 不创建工具活动行，
    /// 消息区只由 Watch 进度行承载（唯一显示，原地更新）。
    #[test]
    fn agent_tool_start_creates_no_tool_activity() {
        assert!(is_hidden_tool("Agent"), "Agent 是隐藏工具");
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::ToolStart { name: "Agent".into() });
        chat.drain_events();
        assert!(
            chat.messages[0].activities.iter().all(|a| !matches!(
                a.kind,
                ActivityKind::Tool(_)
            )),
            "Agent 不创建 Tool 活动: {:?}",
            chat.messages[0]
                .activities
                .iter()
                .map(|a| format!("{:?}", a.kind))
                .collect::<Vec<_>>()
        );

        // Watch 活动行正常创建（唯一 Agent 显示）。
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "Agent: 列出桌面目录内容".into(),
            status: WatchStatus::Running,
            detail: Some("已产出 0 字符".into()),
            duration_ms: 0,
            payload: None,
            signal: None,
        });
        chat.drain_events();
        let watch_rows = chat.messages[0]
            .activities
            .iter()
            .filter(|a| matches!(a.kind, ActivityKind::Watch(_)))
            .count();
        assert_eq!(watch_rows, 1, "Watch 行唯一");

        // 同 label 后续事件原地更新，不新建行。
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "Agent: 列出桌面目录内容".into(),
            status: WatchStatus::Running,
            detail: Some("已产出 43 字符".into()),
            duration_ms: 0,
            payload: None,
            signal: None,
        });
        chat.drain_events();
        let watch_rows = chat.messages[0]
            .activities
            .iter()
            .filter(|a| matches!(a.kind, ActivityKind::Watch(_)))
            .count();
        assert_eq!(watch_rows, 1, "同 label 事件不新建行");
        let detail = chat.messages[0]
            .activities
            .iter()
            .find_map(|a| match &a.kind {
                ActivityKind::Watch(w) => w.detail.clone(),
                _ => None,
            });
        assert_eq!(detail.as_deref(), Some("已产出 43 字符"), "原地更新 detail");
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
            standalone: false,
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
            let _ = chat.events.send(UiEvent::ToolReady {
                name: name.into(),
                input,
                standalone: false,
            });
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
            standalone: false,
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
            standalone: false,
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
            standalone: false,
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
            standalone: false,
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
    fn settled_tracks_streaming_message() {
        let mut chat = test_chat();
        chat.build_rows(100);
        let welcome = chat.doc.settled;
        assert!(welcome > 0, "welcome card rows are settled");
        assert_eq!(chat.doc.settled, chat.doc.rows.len(), "empty session fully settled");
        // 回合开始：流式消息 + 占位 thinking → 不得定稿。
        chat.handle(UiEvent::TurnStart);
        chat.build_rows(100);
        assert_eq!(chat.doc.settled, welcome, "streaming message not settled");
        assert!(chat.doc.rows.len() > welcome, "streaming message rendered");
        // 回合结束：消息定稿，所有行进入 settled。
        chat.handle(UiEvent::TurnEnd);
        chat.build_rows(100);
        assert_eq!(chat.doc.settled, chat.doc.rows.len(), "all rows settled after turn");
        // 定稿单向：第二条消息（流式）不动已有边界。
        let after_turn = chat.doc.settled;
        chat.handle(UiEvent::TurnStart);
        chat.build_rows(100);
        assert_eq!(chat.doc.settled, after_turn, "new turn keeps prior settled boundary");
    }

    #[test]
    fn settled_stops_at_running_activity() {
        let mut chat = test_chat();
        chat.build_rows(100);
        let welcome = chat.doc.settled;
        // 一条带运行中工具的消息。
        let mut m = msg(Role::Assistant, "");
        m.activities.push(tool_activity());
        chat.messages.push(m);
        chat.build_rows(100);
        assert_eq!(chat.doc.settled, welcome, "running tool keeps message dynamic");
        // 工具完成 → 定稿。
        let a = &mut chat.messages[0].activities[0];
        match &mut a.kind {
            ActivityKind::Tool(t) => t.status = ToolStatus::Done,
            _ => panic!("tool activity expected"),
        }
        chat.build_rows(100);
        assert_eq!(chat.doc.settled, chat.doc.rows.len(), "settled after tool done");
    }

    #[test]
    fn settled_stops_before_permission_block() {
        let mut chat = test_chat();
        chat.build_rows(100);
        let welcome = chat.doc.settled;
        // 流式回合（动态消息）。
        chat.handle(UiEvent::TurnStart);
        chat.build_rows(100);
        assert_eq!(chat.doc.settled, welcome, "streaming message dynamic");
        // 权限请求块出现 → 边界不动（请求块不定稿）。
        let (tx, _rx) = tokio::sync::oneshot::channel();
        chat.pending_ask = Some((
            PermissionRequest::new("允许执行 Bash", "cargo build", vec!["允许".into()]),
            tx,
        ));
        chat.build_rows(100);
        assert_eq!(chat.doc.settled, welcome, "ask block not settled");
        // 回合结束 + 请求解决 → 全部定稿。
        chat.pending_ask = None;
        chat.handle(UiEvent::TurnEnd);
        chat.build_rows(100);
        assert_eq!(chat.doc.settled, chat.doc.rows.len(), "all settled after ask done");
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
        assert!(joined.contains("❯ 1. 允许"), "focused first option: {joined}");
        assert!(joined.contains("2. 拒绝"), "option row: {joined}");
        assert!(
            joined.contains("Enter to select · ↑/↓ to navigate · Esc to cancel"),
            "hint: {joined}"
        );
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
    fn ask_question_renders_other_and_answers_free_text() {
        let mut chat = test_chat();
        let (tx, mut rx) = oneshot::channel();
        let mut request =
            PermissionRequest::new("技术选型", "用哪个库？", vec!["A".into(), "B".into()]);
        request.free_text = true;
        request.descriptions = vec![None, Some("更快".to_string())];
        chat.pending_ask = Some((request, tx));
        chat.build_rows(100);
        let joined: String = chat
            .doc
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("1. A"), "option: {joined}");
        assert!(joined.contains("2. B"), "option: {joined}");
        assert!(joined.contains("  更快"), "desc dim row: {joined}");
        assert!(joined.contains("3. Other"), "other option: {joined}");
        assert!(joined.contains("Type something."), "placeholder: {joined}");
        assert!(chat.ask_key(KeyCode::Char('3')), "digit 3 → Other");
        chat.build_rows(100);
        let joined: String = chat
            .doc
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("❯ 3. Other"), "other focused: {joined}");
        assert!(joined.contains("Enter to submit · Esc to cancel"), "input hint: {joined}");
        for c in ['s', 'e', 'r', 'd', 'e'] {
            assert!(chat.ask_key(KeyCode::Char(c)), "type {c}");
        }
        assert!(chat.ask_key(KeyCode::Enter), "submit");
        assert!(chat.pending_ask.is_none(), "dialog closed");
        assert_eq!(rx.try_recv(), Ok(DialogAction::Answer("serde".to_string())));
        let result = chat.ask_result.as_ref().expect("result recorded");
        assert_eq!(
            result.answered,
            vec![("用哪个库？".to_string(), "serde".to_string())]
        );
        chat.build_rows(100);
        let joined: String = chat
            .doc
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("User answered Claude's questions:"),
            "result header: {joined}"
        );
        assert!(
            joined.contains("· 用哪个库？ → serde"),
            "result line: {joined}"
        );
    }

    #[test]
    fn ask_other_empty_submit_cancels() {
        let mut chat = test_chat();
        let (tx, mut rx) = oneshot::channel();
        let mut request =
            PermissionRequest::new("技术选型", "用哪个库？", vec!["A".into(), "B".into()]);
        request.free_text = true;
        chat.pending_ask = Some((request, tx));
        chat.ask_focus = 2;
        assert!(chat.ask_key(KeyCode::Enter), "空 Other 提交");
        assert!(chat.pending_ask.is_none());
        assert_eq!(rx.try_recv(), Ok(DialogAction::Cancel));
        assert!(
            chat.ask_result.as_ref().is_some_and(|r| r.declined),
            "declined recorded"
        );
        chat.build_rows(100);
        let joined: String = chat
            .doc
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("User declined to answer questions"), "{joined}");
    }

    #[test]
    fn ask_arrow_keys_move_focus() {
        let mut chat = test_chat();
        let (tx, mut rx) = oneshot::channel();
        let mut request =
            PermissionRequest::new("技术选型", "用哪个库？", vec!["A".into(), "B".into()]);
        request.free_text = true;
        chat.pending_ask = Some((request, tx));
        assert!(chat.ask_key(KeyCode::Down), "↓ 到 B");
        assert_eq!(chat.ask_focus, 1);
        assert!(chat.ask_key(KeyCode::Down), "↓ 到 Other");
        assert_eq!(chat.ask_focus, 2);
        assert!(chat.ask_key(KeyCode::Down), "↓ 到底部不再移动");
        assert_eq!(chat.ask_focus, 2);
        assert!(chat.ask_key(KeyCode::Up), "↑ 回 B");
        assert_eq!(chat.ask_focus, 1);
        assert!(chat.ask_key(KeyCode::Enter), "Enter 选 B");
        assert_eq!(rx.try_recv(), Ok(DialogAction::Confirm(1)));
        assert_eq!(
            chat.ask_result.as_ref().unwrap().answered,
            vec![("用哪个库？".to_string(), "B".to_string())]
        );
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

    /// Esc（busy 时）置中断标志：后台任务完成通知不再自动拉起新回合；
    /// 新回合（start_turn）复位。
    #[test]
    fn esc_sets_interrupted_and_start_turn_resets() {
        let mut chat = test_chat();
        chat.busy = true;
        assert!(
            chat.on_key(KeyCode::Esc, KeyModifiers::empty()),
            "busy Esc 中断"
        );
        assert!(chat.interrupted, "Esc 置 interrupted");
        assert!(
            *chat.cancel_tx.borrow(),
            "中断信号已发送（send_replace 无条件生效）"
        );
        chat.busy = false;
        chat.interrupted = false;
        chat.busy = true;
        let _ = chat.cancel_tx.send_replace(true);
        let cancel_rx = chat.cancel_tx.subscribe();
        chat.cancel_tx.send_replace(false);
        assert!(
            !*cancel_rx.borrow(),
            "新一轮开始前复位：receiver 读到 false"
        );
        drop(cancel_rx);
    }

    /// start_turn 的复位顺序：先订阅再 send_replace——上一轮的 receiver 全部
    /// drop 后（tokio watch 无 receiver 时 send 不更新值），新回合仍能看到 false。
    #[test]
    fn cancel_reset_works_after_all_receivers_dropped() {
        let mut chat = test_chat();
        chat.cancel_tx.send_replace(true);
        drop(chat.cancel_tx.subscribe());
        let cancel_rx = chat.cancel_tx.subscribe();
        chat.cancel_tx.send_replace(false);
        assert!(
            !*cancel_rx.borrow(),
            "receiver 全 drop 后 send_replace 仍复位（send 则失效）"
        );
    }

    #[test]
    fn image_ready_updates_cache_and_invalidates_render_cache() {
        let mut chat = test_chat();
        chat.reply_cache.insert("x".to_string(), vec![Line::plain("old")]);
        let meta = ImageMeta {
            cols: 5,
            rows: 3,
            bytes: vec![1, 2, 3],
        };
        chat.handle(UiEvent::ImageReady {
            url: "a.png".to_string(),
            meta: Some(meta.clone()),
        });
        assert!(chat.images.contains_key("a.png"), "加载成功写入缓存");
        assert_eq!(chat.images["a.png"].cols, 5);
        assert_eq!(chat.images_version, 2, "版本递增（初始 1）");
        assert!(chat.reply_cache.is_empty(), "reply_cache 失效");

        chat.handle(UiEvent::ImageReady {
            url: "a.png".to_string(),
            meta: None,
        });
        assert!(!chat.images.contains_key("a.png"), "失败移除缓存");
        assert!(chat.warnings.iter().any(|w| w.contains("a.png")), "警告提示");
    }

    #[test]
    fn turn_end_without_capability_skips_image_loading() {
        let mut chat = test_chat();
        chat.apply_turn_start();
        chat.handle(UiEvent::TextDelta(
            "![图](https://example.com/i.png)".to_string(),
        ));
        chat.handle(UiEvent::TurnEnd);
        assert!(chat.images.is_empty(), "无能力不加载");
        assert!(chat.images_pending.is_empty());
    }

    /// TurnEnd → 异步加载 data URL 图片 → ImageReady 回传 → 文档出现图片块。
    #[tokio::test]
    async fn turn_end_loads_images_and_renders_image_block() {
        let mut chat = test_chat();
        chat.image_cap = Some(ImageCap::default_cells());
        let png = tiny_png();
        let url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&png)
        );
        chat.apply_turn_start();
        chat.handle(UiEvent::TextDelta(format!("![图]({url})")));
        chat.handle(UiEvent::TurnEnd);
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        while !chat.images.contains_key(&url) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "image load timed out"
            );
            chat.drain_all();
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(chat.images_pending.is_empty(), "在途集合已清空");
        chat.build_rows(100);
        let image_rows = chat
            .doc
            .rows
            .iter()
            .filter(|r| r.line.image.is_some())
            .count();
        assert!(image_rows > 0, "文档出现图片块行");
        let meta = &chat.images[&url];
        assert_eq!(image_rows, meta.rows, "块行数 = meta.rows");
    }

    /// 4×2 纯色 PNG（测试用）。
    fn tiny_png() -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(4, 2, image::Rgba([255u8, 0, 0, 255]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }
}
