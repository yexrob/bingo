//! 聊天状态机：消息/活动/折叠组的增量模型 + 文档行构建。
//!
//! 移植自旧 `tui.rs` 的 `BingoChat`（ratatui 版）：事件处理语义、
//! 折叠判定、展开切换原样保留；`draw` 换成 [`Chat::build_rows`]，
//! 产出显示无关的样式化行文档，由 [`crate::tui::view`] 映射为终端行。
//! 事件从通道（`UiEvent` / `AskRequest`）流入，键盘/鼠标经
//! [`Chat::on_key`] / [`Chat::doc_click`] 流入。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::style::Color;
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
use crate::tui::line::{text_width, wrap_words, Line, SegStyle};
use crate::tui::markdown::MarkdownRenderer;
use crate::tui::theme::{Theme, ThemeSetting};
use crate::ui::{AskRequest, DialogAction, PermissionRequest, UiEvent};

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
    /// Every row is exactly one canvas line: the constructor is the single
    /// choke point that enforces it (see [`crate::tui::line::sanitize`]).
    pub fn new(line: Line) -> Self {
        let mut line = line;
        line.sanitize();
        Self {
            line,
            bg: None,
            padding_right: 0,
        }
    }

    /// 整行背景的气泡行（用户消息；CC paddingRight=1）。
    pub fn bubble(line: Line, bg: Color) -> Self {
        let mut row = Row::new(line);
        row.bg = Some(bg);
        row.padding_right = 1;
        row
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

/// 滚动文档：全部行 + 点击范围。
///
/// inline 模式下文档只覆盖"尚未落盘"的部分（[`Chat::flushed_segments`]
/// 之后的消息），行号因此不是全局的——点击定位与滚动只在全屏模式用。
#[derive(Debug, Clone)]
pub struct Doc {
    pub rows: Vec<Row>,
    pub click_ranges: Vec<ClickRange>,
    /// 前置"定稿"行数：不再变化、可一次性打印进终端 scrollback 的行
    /// （REPL 模式的打印边界；全屏模式不用）。
    /// 生产路径经 `settled_marks` 取检查点；此聚合值保留为测试面的
    /// 「定稿前缀行数」句柄。
    #[cfg_attr(not(test), allow(dead_code))]
    pub settled: usize,
    /// 定稿检查点（欢迎卡 / 每条定稿消息各一个，行号递增）：
    /// 懒落盘按检查点整段冻结，resize 回灌按检查点整段取回。
    pub settled_marks: Vec<SettledMark>,
    /// 文档尾部的瞬态行数（slash 输出，TTL 后消失）：懒落盘的窗口计算
    /// 必须剔除它们——临时列表把窗口挤小不构成冻结活内容的理由。
    pub transient_rows: usize,
}

/// 一个定稿检查点：`row_end` 之前的行全部定稿。`segments` 是构建内
/// 累计值，跨多次 [`Chat::advance_flushed_upto`] 的增量由
/// `Chat::mark_base` 消化。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettledMark {
    /// 该检查点覆盖的行数（doc.rows 内的排他终点）。
    pub row_end: usize,
    /// 覆盖的消息段数（构建内累计，含欢迎卡）。
    pub segments: usize,
}

/// 当前错误态（#18 呈现层）：`code`/`msg`/`level`/`context` 来自结构化
/// `UiEvent::Error`，级别由触发上下文决定（短同步=页面级，长回合=全流程级）。
#[derive(Debug, Clone)]
pub struct ErrorState {
    pub code: &'static str,
    pub msg: String,
    pub level: crate::error::ErrorLevel,
    /// 触发上下文（契约字段：事件链「生产者→事件→状态」上下文存活，
    /// 供审计与未来短操作接入；渲染分支用 `level`）。
    #[allow(dead_code)]
    pub context: crate::error::ErrorContext,
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

/// 工具的可折叠分类（isSearchOrReadCommand）。
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

/// 不在 transcript 展示的工具调用（renderToolUseMessage = null）：
/// Task 工具族（任务区面板即展示）、AskUserQuestion（对话框即展示）。
pub fn is_hidden_tool(name: &str) -> bool {
    matches!(
        name,
        "TaskCreate"
            | "TaskUpdate"
            | "TaskGet"
            | "TaskList"
            | "AskUserQuestion"
            // Agent 对齐 Task renderToolUseMessage=null：不渲染工具行，
            // 进度由 Watch 活动行（`Agent: 描述 · 已产出 N 字符`）一处承载。
            | "Agent"
    )
}

/// 内置 slash 命令表（/help 与下拉建议共用单一来源）。
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
    ("think", "设置思考级别（/think [off|low|medium|high|xhigh|max]）"),
    ("skills", "列出可用技能"),
    ("tasks", "列出后台任务"),
    ("team", "管理项目团队（/team start|status|assign|stop|list）"),
    ("exit", "退出会话"),
];

/// slash 下拉建议项（/name + 描述）。
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

/// `/think` 单级选择器状态（等级表 = off + [`crate::api::types::THINKING_LEVELS`]）。
#[derive(Clone)]
pub struct ThinkMenu {
    pub selected: usize,
}

/// `/think` 选择器条目：等级名 + 说明（off 之外与 THINKING_LEVELS 一一对应，
/// 顺序一致；一致性由测试保证）。
pub const THINK_LEVELS: &[(&str, &str)] = &[
    ("off", "不发 thinking 参数（兼容 DeepSeek 等端点）"),
    ("low", "adaptive thinking · effort low"),
    ("medium", "adaptive thinking · effort medium"),
    ("high", "adaptive thinking · effort high（默认档位）"),
    ("xhigh", "adaptive thinking · effort xhigh（编码/agentic 推荐）"),
    ("max", "adaptive thinking · effort max（最深推理）"),
];

/// 下拉最大可见行数（OVERLAY_MAX_ITEMS = 5）。
pub const SLASH_SUGGESTIONS_MAX: usize = 5;

/// 输入区最多渲染的行数（更长的输入滚动到光标所在行）。
pub const INPUT_ROWS_MAX: usize = 10;
/// 排队消息最多显示的行数（更多的折叠为 `… +N more`）。
pub const QUEUE_ROWS_MAX: usize = 3;
/// 实体选择器聚焦时最多逐行显示的实体数。
pub const ENTITY_ROWS_MAX: usize = 6;
/// 撤销栈深度（ctrl+_）。
pub const UNDO_MAX: usize = 20;
/// 两次 Ctrl+C 之间的退出确认窗口。
pub const CTRL_C_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);
/// 两次 Esc 之间的清空确认窗口。
pub const ESC_WINDOW: std::time::Duration = std::time::Duration::from_secs(1);
/// 粘贴突发判定：按键间隔短于此值即视为同一批输入。
///
/// 支持 bracketed paste 的终端走 [`Chat::on_paste`]（真实 `Event::Paste`），
/// 这套启发式只是不支持的终端上的兜底。它的局限（写在这里，因为它决定了
/// 那些终端上的体验边界）：
/// - 打字极快的人（<10ms/键，连续 [`PASTE_BURST_KEYS`] 次以上）会被误判为
///   粘贴，此时 Enter 变成换行而不是发送——按 Esc 或停顿一下即恢复；
/// - 逐字符重放的自动化输入（tmux send-keys、expect）同样会被误判；
/// - 反过来，慢速粘贴（SSH 抖动）会被当作打字，此时 Enter 直接发送。
pub const PASTE_BURST_GAP: std::time::Duration = std::time::Duration::from_millis(10);
/// 连续多少个"快"按键之后才认定是粘贴（低于此值是正常快打）。
pub const PASTE_BURST_KEYS: usize = 4;
/// 粘贴超过这么多行时折叠为占位符。
pub const PASTE_COLLAPSE_LINES: usize = 10;

/// 图片占位引用（`#[image N]` → 附件表第 N 张，1 起）。
static IMAGE_MARKER_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"#\[image (\d+)\]").expect("static regex"));

/// 图片占位符文本：`#[image N]`。
fn image_marker(id: usize) -> String {
    format!("#[image {id}]")
}

/// 展开 `~` 前缀为 home 目录（无 home 时原样返回）。
fn expand_home(path: &str) -> String {
    if let (Some(rest), Ok(home)) = (path.strip_prefix("~/"), std::env::var("HOME")) {
        return format!("{home}/{rest}");
    }
    path.to_string()
}

/// 独立成行的图片路径：路径特征（`~` 开头或含 `/`）+ 图片扩展名。
fn standalone_image_path(s: &str) -> Option<String> {
    if !(s.starts_with('~') || s.contains('/')) {
        return None;
    }
    let ext = std::path::Path::new(s)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)?;
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp")
        .then(|| s.to_string())
}

/// `![alt](path)` 整行的路径（path 无空格；`<path>` 包裹解包）。
fn markdown_image_path(s: &str) -> Option<String> {
    let rest = s.strip_prefix("![")?;
    let close = rest.find("](")?;
    let rest = &rest[close + 2..];
    let end = rest.find(')')?;
    let p = &rest[..end];
    let p = p.strip_prefix('<').and_then(|p| p.strip_suffix('>')).unwrap_or(p);
    (!p.is_empty() && !p.contains(' ')).then(|| p.to_string())
}

/// 单张图片的加载上限（超时按加载失败处理）。
pub const IMAGE_LOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// slash 临时提示存活时长：超时后从输入框上方消失（不落盘）。
pub const SLASH_OUTPUT_TTL: std::time::Duration = std::time::Duration::from_secs(2);

/// AskUserQuestion 被用户拒绝（Esc / 空 Other 提交）时进入消息流的
/// 用户消息文本（普通消息，随流持久）。
pub const ASK_DECLINED_TEXT: &str = "User declined to answer questions";

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
/// 只留命令输出（BashModeProgress 的裸输出展示）。
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

/// 思考阶段俏皮词。
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

/// 思考完成态随机词（`TURN_COMPLETION_VERBS`，均适配 `for Xs`）。
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

/// 编辑动作分类：连续的同类微编辑（逐字插入/逐字删除）在撤销栈里合并
/// 为一步，整体替换（kill / yank / 换行 / 历史回填）各自成步。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Insert,
    Delete,
    Bulk,
}

/// 底部运行状态：`✻ {verb}… (esc to interrupt · {N}s · ↓ {tokens} tokens)`。
#[derive(Debug, Clone, PartialEq)]
pub struct RunningStatus {
    /// 当前动词（工具摘要 / thinking 俏皮词 / `Working`）。
    pub verb: String,
    /// 本回合已耗时（秒）。
    pub elapsed: f64,
    /// 本回合已产出的 token 数（0 = 该段省略）。
    pub tokens: u64,
}

/// ctrl+r 反向搜索态：查询串 + 当前命中（inline 经典版）。
#[derive(Debug, Clone, Default)]
pub struct HistorySearch {
    /// 用户输入的过滤串。
    pub query: String,
    /// 命中的历史条目（None = 无匹配）。
    pub hit: Option<String>,
    /// 命中条目在历史中的下标（再按 ctrl+r 从它继续往旧找）。
    pub index: Option<usize>,
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
    /// 光标在 `input` 中的字节位置（恒落在字符边界上）。
    pub cursor: usize,
    /// 最近一次 ctrl+k/u/w 删除的文本（ctrl+y 粘回）。
    kill: String,
    /// 编辑撤销栈（文本 + 光标），封顶 [`UNDO_MAX`]。
    undo: Vec<(String, usize)>,
    /// 上一次编辑的类型（连续同类编辑在撤销栈里合并）。
    last_edit: Option<EditKind>,
    /// Alt+T 关闭思考前的等级（再按恢复它）。
    last_thinking: Option<String>,
    /// ctrl+s 暂存的输入（文本 + 光标）。
    stash: Option<(String, usize)>,
    /// 提交过的 prompt（按 cwd 持久化，写失败降级为会话内）。
    pub history: crate::tui::history::History,
    /// 历史文件是否可写（一次失败后不再重试，避免每轮提交都撞同一个错）。
    history_writable: bool,
    /// busy 期间排队的消息（TurnEnd 后逐条提交）。
    pub queued: Vec<String>,
    /// `?` 快捷键面板是否展开。
    pub help_visible: bool,
    /// 底部临时提示（`Press ctrl-c again to exit` 等）。
    pub notice: Option<&'static str>,
    /// 最近一次空输入 Ctrl+C 的时刻（[`CTRL_C_WINDOW`] 内再按即退出）。
    ctrl_c_at: Option<std::time::Instant>,
    /// 最近一次 Esc 的时刻（[`ESC_WINDOW`] 内再按即清空输入）。
    esc_at: Option<std::time::Instant>,
    /// 上一次按键时刻与连续"快"按键计数（粘贴突发启发式）。
    last_key_at: Option<std::time::Instant>,
    burst_keys: usize,
    /// 折叠的粘贴块：占位符 `[Pasted text #N +M lines]` → 真实内容。
    pastes: Vec<(String, String)>,
    /// 消息框挂载的图片附件（`#[image N]` 占位 → N 对应此表下标+1）。
    attachments: Vec<crate::api::types::ImageAttachment>,
    /// 本会话执行过的 `!` 命令（bash 模式 Tab 前缀补全）。
    bash_history: Vec<String>,
    /// ctrl+r 反向搜索态（None = 未激活）。
    pub search: Option<HistorySearch>,
    /// 当前权限模式（shift+tab 循环）。Session 在 Arc 里不可变，
    /// 这里持有真正生效的那份：每回合以它派生 Session 副本。
    pub permission_mode: PermissionMode,
    /// ctrl+l 请求整屏重画（渲染层消费后清除）。
    pub force_redraw: bool,
    /// inline ctrl+o 请求整卷 transcript 重放：全部展开、游标已回卷，
    /// app 层下一帧把定稿部分一次冻结进 scrollback（消费后清除）。
    pub dump_transcript: bool,
    /// bash 模式（`!` 前缀）：输入直接执行，不经模型。
    pub bash_mode: bool,
    pub busy: bool,
    /// Esc/Ctrl+C 中断过当前回合：后台任务完成通知不再自动拉起新回合
    /// （interrupt 语义：等待用户主动提交才继续），start_turn 时复位。
    pub interrupted: bool,
    /// 当前 assistant 消息索引。
    pub stream_msg: Option<usize>,
    thinking_buf: String,
    /// 当前 thinking 段是否开放续写：ToolStart/TextDelta（段边界）后关闭，
    /// 同一段内的 delta 续写不加段落分隔；新段（工具后的新推理）\n\n 聚合。
    thinking_seg_open: bool,
    output_tokens: u64,
    pub tick: u64,
    /// TurnStart 时的 tick：运行态 thinking 的相对计时基准。
    turn_start_tick: u64,
    /// TurnStart 的真实时钟（状态行耗时基准；TurnEnd 清空）。
    turn_started: Option<std::time::Instant>,
    /// 非致命警告（时间戳 + 文案）：超过 WARNING_TTL 自动过期，
    /// 渲染只显示有效条目（push 时顺带清理）。
    pub warnings: Vec<(std::time::Instant, String)>,
    /// 当前错误态（#18 呈现层）：驱动错误行高亮与全流程级整屏态。
    /// `UiEvent::Error` 到达时记录；复位动作（AC-03 复位四项）清除。
    /// 渲染端按 `level` 分支：Field/Page → 错误行高亮，Full → 整屏错误态。
    pub last_error: Option<ErrorState>,
    /// 最近一次模型回合提交的输入（#18 整屏错误态 Enter=重试时重跑）。
    pub last_prompt: String,
    pub cwd: String,
    /// 权限询问：请求 + 结果回执。
    pub pending_ask: Option<(PermissionRequest, oneshot::Sender<DialogAction>)>,
    /// 对话框焦点行（0..=options.len()；== options.len() = Other 输入）。
    ask_focus: usize,
    /// Other 自由输入缓冲。
    ask_other: String,
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
    /// 上次 build_rows 的宽度（markdown 缓存按宽度失效）。
    prev_build_width: usize,
    pub width: usize,
    /// 视口行数（布局层写入；reconcile_scroll 用它钳制滚动）。
    pub viewport_height: usize,
    /// 终端总行数（布局层写入；`?` 面板按它算行数预算）。
    pub height: usize,
    pub scroll: usize,
    pub auto_scroll: bool,
    /// 上次 build_rows 的文档（点击定位）。
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
    /// inline：已落盘进 scrollback 的文档前缀段数——0 = 无，1 = 欢迎卡，
    /// 1+k = 欢迎卡 + 前 k 条消息。落盘游标按**消息边界**而非行号计，
    /// 于是宽度变化后重排（行号全变）也不会重复打印。
    pub flushed_segments: usize,
    /// inline：当前 doc 中已落盘的行数（canvas 尾部起点）；每次
    /// build_rows 归零——重建后落盘部分已不在文档里。
    pub tail_start: usize,
    /// 检查点累计值的消化基线：同一次构建内多次推进落盘游标时防止
    /// 重复累加（build_rows 归零）。
    mark_base: usize,
    /// slash 下拉建议（输入 `/` 且无参数时非空；组件层渲染）。
    pub slash_suggestions: Vec<SlashSuggestion>,
    /// 下拉选中索引。
    pub slash_selected: usize,
    /// `/model` 二级选择器（一级 endpoint → 二级模型列表；None = 未激活）。
    pub model_menu: Option<ModelMenu>,
    /// `/think` 等级选择器（None = 未激活）。
    pub think_menu: Option<ThinkMenu>,
    /// 任务区展开信号（Task 工具调用 → 展示任务列表）。
    pub tasks_visible: bool,
    /// 任务区是否由 TaskCreate 自动打开（非 ctrl+t 手动）：全部完成后自动隐藏。
    pub tasks_auto: bool,
    /// 底部实体区快照（agent 实例 + 频道；tick/WatchEvent 时刷新）。
    pub entities: Vec<EntityRow>,
    /// 实体选择器焦点（Some(i) = 选择模式：↑↓/Enter/Esc 被捕获）。
    pub entity_focus: Option<usize>,
    /// 待打开的实体视图（app 层消费 → 进入全屏模态）。
    pub open_entity: Option<EntityOpen>,
    /// 中断信号：busy 时 Ctrl+C / Esc → send(true)，回合内流读取立即中止。
    cancel_tx: tokio::sync::watch::Sender<bool>,
}

/// 底部实体区的一行：子代理实例或频道。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityRow {
    Agent {
        name: String,
        state: &'static str,
        description: String,
    },
    Channel {
        name: String,
        seq: u64,
        frozen: bool,
    },
}

/// 选中回车后要打开的实体视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityOpen {
    Agent(String),
    Channel(String),
}

impl Chat {
    /// 非致命警告的展示时限：过期条目不再渲染（push 时顺带清理）。
    const WARNING_TTL: std::time::Duration = std::time::Duration::from_secs(10);

    /// 记录一条非致命警告（去重 + 清理过期）。
    pub(crate) fn push_warning(&mut self, message: String) {
        self.warnings.retain(|(t, _)| t.elapsed() < Self::WARNING_TTL);
        if !self.warnings.iter().any(|(_, w)| w == &message) {
            self.warnings.push((std::time::Instant::now(), message));
        }
    }

    /// 当前应显示的警告（无过期条目时 None）。
    pub fn visible_warning(&self) -> Option<&str> {
        self.warnings
            .iter()
            .find(|(t, _)| t.elapsed() < Self::WARNING_TTL)
            .map(|(_, w)| w.as_str())
    }
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
                            kind: ev.kind,
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
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let history = crate::tui::history::History::new(crate::tui::history::load(
            &session.home,
            std::path::Path::new(&cwd),
        ));
        let permission_mode = session.permission_mode;
        Self {
            session,
            events,
            asks,
            events_rx,
            asks_rx,
            messages: Vec::new(),
            input: String::new(),
            cursor: 0,
            kill: String::new(),
            undo: Vec::new(),
            last_edit: None,
            last_thinking: None,
            stash: None,
            history,
            history_writable: true,
            queued: Vec::new(),
            help_visible: false,
            notice: None,
            ctrl_c_at: None,
            esc_at: None,
            last_key_at: None,
            burst_keys: 0,
            pastes: Vec::new(),
            attachments: Vec::new(),
            bash_history: Vec::new(),
            search: None,
            permission_mode,
            force_redraw: false,
            dump_transcript: false,
            bash_mode: false,
            busy: false,
            stream_msg: None,
            thinking_buf: String::new(),
            thinking_seg_open: false,
            output_tokens: 0,
            tick: 0,
            turn_start_tick: 0,
            turn_started: None,
            warnings: Vec::new(),
            last_error: None,
            last_prompt: String::new(),
            cwd,
            pending_ask: None,
            ask_focus: 0,
            ask_other: String::new(),
            tasks_cache: Vec::new(),
            processor: MarkdownProcessor::default(),
            renderer: MarkdownRenderer::with_theme(80, theme.clone()),
            reply_cache: HashMap::new(),
            image_cap: None,
            images: HashMap::new(),
            images_pending: HashSet::new(),
            images_version: 1,
            dirty: true,
            prev_build_width: 0,
            width: 80,
            viewport_height: 24,
            height: 24,
            scroll: 0,
            auto_scroll: true,
            doc: Doc {
                rows: Vec::new(),
                click_ranges: Vec::new(),
                settled: 0,
                settled_marks: Vec::new(),
                transient_rows: 0,
            },
            pending_tools: Vec::new(),
            theme,
            detected_background,
            slash_lines: Vec::new(),
            slash_at: None,
            exit: false,
            flushed_segments: 0,
            tail_start: 0,
            mark_base: 0,
            slash_suggestions: Vec::new(),
            slash_selected: 0,
            model_menu: None,
            think_menu: None,
            tasks_visible: false,
            tasks_auto: false,
            entities: Vec::new(),
            entity_focus: None,
            open_entity: None,
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
                        self.push_warning(format!("图片加载失败: {url}"));
                    }
                }
                // 缓存版本递增：渲染器逐块缓存与 reply_cache 一并失效。
                self.images_version = self.images_version.wrapping_add(1);
                self.reply_cache.clear();
                self.dirty = true;
            }
            UiEvent::TurnStart => {
                // 新回合开始 = 错误态复位（AC-03）：页面级错误行随新回合消失
                // （整屏级 Full 在 error_screen_key 已 dismiss，此处兜底）。
                self.last_error = None;
                self.thinking_buf.clear();
                self.thinking_seg_open = false;
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
                    segments: 1,
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
                    // 正文是阶段边界：正文后的 thinking 新开块（不再聚合），
                    // 运行中的思考块随之收尾（同 ToolStart 的收尾语义）。
                    self.thinking_buf.clear();
                    self.thinking_seg_open = false;
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
                        // 聚合：正文未打断时（thinking_buf 仍持有本阶段文本），
                        // 新推理并入最后一个 thinking 块。同段续写（段未关）
                        // 直接追加；新段（工具/正文之后）空行分隔并计数。
                        if !self.thinking_buf.is_empty() {
                            let was_open = self.thinking_seg_open;
                            if was_open {
                                self.thinking_buf.push_str(&thinking);
                            } else {
                                self.thinking_buf.push_str("\n\n");
                                self.thinking_buf.push_str(&thinking);
                            }
                            self.thinking_seg_open = true;
                            let buf = self.thinking_buf.clone();
                            let content = self.render_thinking(&buf);
                            let merged = self.messages[i]
                                .activities
                                .iter_mut()
                                .rev()
                                .find(|a| matches!(a.kind, ActivityKind::Thinking(_)));
                            if let Some(hint) = merged {
                                if let ActivityKind::Thinking(t) = &mut hint.kind {
                                    t.state = ThinkingState::Running;
                                    if !was_open {
                                        t.segments += 1;
                                    }
                                    t.duration_ms = self
                                        .tick
                                        .saturating_sub(t.start_tick)
                                        .saturating_mul(33);
                                }
                                hint.set_content(content);
                            }
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
                            segments: 1,
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
                // 工具开始 = 推理段边界：后续 delta 聚合为新段。
                self.thinking_seg_open = false;
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
                kind,
                status,
                detail,
                duration_ms,
                payload,
                signal,
            } => {
                // agent/频道的生命周期事件顺带刷新底部实体区。
                self.refresh_entities();
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
                        kind,
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
                // 频道行更新且 hub 空闲有邮件：拉起回合消化（子代理发言时
                // hub 多半不在回合中——没有这次唤醒，消息会一直睡到用户开口）。
                if kind == crate::watch::WatchKind::Channel
                    && !self.interrupted
                    && self.queued.is_empty()
                    && self.session.channels.has_hub_mail()
                {
                    self.submit_auto();
                }
            }
            UiEvent::RoundEnd => {
                if let Some(i) = self.stream_msg {
                    // 折叠组以正文为边界：模型轮次（round）不拆组，thinking
                    // 也不拆组——正文（TextDelta）与非折叠工具才关组。
                    // Warm the image cache a round early: by TurnEnd the message
                    // settles and flushes, and an image that only starts loading
                    // then would miss the flush (see `message_settled`).
                    let text = self.messages[i].text.clone();
                    self.load_message_images(&text);
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
                            // 默认展开（BashModeProgress 直接展示输出）。
                            // Skill：结果行只显示 `✦ 技能名`（与活动头
                            // `✦ Skill(输入)` 同族），指针路径只留在 tool_result。
                            if done.name == "Skill" {
                                call.result_summary = done.output.lines().next().and_then(|l| {
                                    l.strip_prefix("✦ ")
                                        .and_then(|rest| rest.split(" — ").next())
                                        .map(|name| format!("✦ {name}"))
                                });
                            }
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
                self.thinking_seg_open = false;
                // AskUserQuestion 回答是普通用户消息（进入消息流、随流落盘），
                // 回合结束无需清理——它已按消息定稿/落盘，会随会话持久。
                // 用户中断后不再因后台任务完成自动拉起新回合；
                // 有排队消息时先让用户的消息走（下面统一提交）。
                if (self.session.watch.has_wake_notifications()
                    || self.session.channels.has_hub_mail())
                    && !self.interrupted
                    && self.queued.is_empty()
                {
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
                self.submit_queued();
            }
            UiEvent::Warning(message) => {
                self.push_warning(message);
            }
            UiEvent::SlashOutput(message) => {
                self.push_slash_output(message);
            }
            UiEvent::Error { code, msg, level, context } => {
                self.busy = false;
                self.stream_msg = None;
                // #18：错误态结构化记录（code/msg/level/context），渲染端据此
                // 生成错误行（Page/Field）或整屏态（Full）——不依赖消息文本
                // 替换与 doc 重建时机，无双显。
                self.last_error = Some(ErrorState {
                    code,
                    msg: msg.clone(),
                    level,
                    context,
                });
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
                // A hung URL must not keep the message unsettled forever
                // (unsettled = never flushed to the scrollback): a timeout
                // reports as a failed load and the placeholder settles.
                let meta: Option<ImageMeta> = tokio::time::timeout(
                    IMAGE_LOAD_TIMEOUT,
                    gfx::load_image(&url, std::path::Path::new(&cwd), &cap),
                )
                .await
                .unwrap_or_default();
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

    /// inline ctrl+o 展开方向（CC 非全屏 transcript）：已打印进 scrollback
    /// 的行改不动（write-once），所以不折叠切换，而是**整卷重放**——全历史
    /// 所有可折叠项展开、落盘游标回卷，app 层随即把整卷 transcript 一次
    /// 冻结进 scrollback，用户用终端滚动翻看。scrollback 里已有的折叠
    /// 旧拷贝收不回，接受重复（与回灌同一取舍）。全部已在屏上且无可
    /// 展开项时 no-op：重放不会增加任何信息。
    ///
    /// 重放帧走 `force_redraw`（清可见屏）：与 resize 同款，先清后写，
    /// 重放内容从屏幕顶部铺起、chrome 紧随其下——不清屏的话旧画面与
    /// 重放行的相对位置取决于视口历史，短内容时会出现同屏重复。
    pub fn expand_transcript(&mut self) -> bool {
        let mut changed = false;
        for message in &mut self.messages {
            for act in &mut message.activities {
                if !act.expanded && act.expandable() {
                    act.expanded = true;
                    changed = true;
                }
            }
            for group in &mut message.groups {
                if !group.expanded && !group.activities.is_empty() {
                    group.expanded = true;
                    changed = true;
                }
            }
        }
        if !changed && self.flushed_segments == 0 {
            return false;
        }
        self.reset_flushed();
        self.dump_transcript = true;
        self.force_redraw = true;
        true
    }

    /// ctrl+o 的切换方向判定：transcript 里存在可折叠项且**全部**处于
    /// 展开态才为真（下一按走闭合）。无可折叠项时恒为假——此时 ctrl+o
    /// 退化为纯重放，反复按也只是重印，不会误入闭合分支。
    pub fn transcript_fully_expanded(&self) -> bool {
        let mut any = false;
        for message in &self.messages {
            for act in &message.activities {
                if act.expandable() {
                    if !act.expanded {
                        return false;
                    }
                    any = true;
                }
            }
            for group in &message.groups {
                if !group.activities.is_empty() {
                    if !group.expanded {
                        return false;
                    }
                    any = true;
                }
            }
        }
        any
    }

    /// inline ctrl+o 闭合方向：全历史折回默认聚合态。只改折叠状态；
    /// 展示层由调用方走 resize 同款路径收拢（清屏重画 + 回灌），因为
    /// 屏上的展开重放行同样属于 write-once 的已打印内容，不清屏就会
    /// 与折叠后的窗口同屏并存。
    pub fn collapse_transcript(&mut self) -> bool {
        let mut changed = false;
        for message in &mut self.messages {
            for act in &mut message.activities {
                if act.expanded {
                    act.expanded = false;
                    changed = true;
                }
            }
            for group in &mut message.groups {
                if group.expanded {
                    group.expanded = false;
                    changed = true;
                }
            }
        }
        if changed {
            self.dirty = true;
        }
        changed
    }

    pub fn submit(&mut self) {
        let text = std::mem::take(&mut self.input);
        self.cursor = 0;
        self.undo.clear();
        self.last_edit = None;
        if text.trim().is_empty() {
            self.set_input(text);
            return;
        }
        // 回合进行中：入队，TurnEnd 后逐条提交（CC 消息排队）。
        if self.busy {
            let text = self.expand_pastes(&text);
            let text = self.expand_image_paths(&text);
            self.queued.push(text);
            self.update_slash_suggestions();
            return;
        }
        let text = self.expand_pastes(&text);
        let text = self.expand_image_paths(&text);
        self.record_history(&text);
        if self.bash_mode {
            let command = text.trim().to_string();
            self.bash_history.push(command.clone());
            self.start_bash_turn(command);
            return;
        }
        if let Some(cmd) = text.strip_prefix('/') {
            // Enter 时输入是部分前缀且有下拉建议：应用选中项并执行
            //（handleEnter：suggestions 存在时 Enter = 补全 + 执行）。
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
        self.last_prompt = text.clone();
        self.start_turn(text, true);
    }

    /// 大段粘贴折叠为占位符：输入框里留 `[Pasted text #N +M lines]`，
    /// 真实内容存在 [`Chat::pastes`]，提交时由 [`Chat::expand_pastes`] 还原。
    /// 只在粘贴突发中调用（判定局限见 [`PASTE_BURST_GAP`]）。
    fn collapse_paste(&mut self) {
        let lines = self.input.lines().count();
        if lines < PASTE_COLLAPSE_LINES {
            return;
        }
        let body = std::mem::take(&mut self.input);
        let token = format!("[Pasted text #{} +{lines} lines]", self.pastes.len() + 1);
        self.pastes.push((token.clone(), body));
        self.input = token;
        self.cursor = self.input.len();
    }

    /// Bracketed paste (`Event::Paste`): insert the payload at the cursor as a
    /// single undo step, then fold it away when it is large enough to swamp
    /// the prompt. Terminals send bare CR for the line breaks inside a paste,
    /// so they are normalised first — the fold threshold counts lines.
    ///
    /// 剪贴板含图片（macOS）时优先挂载图片：`#[image N]` 占位插到光标处，
    /// 文本 payload 忽略（终端对纯图片剪贴板的 paste 事件没有文本内容）。
    /// The burst heuristic ([`PASTE_BURST_GAP`]) stays as the fallback for
    /// terminals that do not report bracketed paste.
    pub fn on_paste(&mut self, text: &str) {
        if let Some(id) = self.paste_clipboard_image() {
            self.snapshot(EditKind::Bulk);
            crate::tui::input::insert(&mut self.input, &mut self.cursor, &image_marker(id));
            self.after_edit();
            self.dirty = true;
            return;
        }
        if text.is_empty() {
            return;
        }
        let text = if text.contains('\r') {
            text.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            text.to_string()
        };
        self.snapshot(EditKind::Bulk);
        crate::tui::input::insert(&mut self.input, &mut self.cursor, &text);
        self.after_edit();
        if text.lines().count() >= PASTE_COLLAPSE_LINES {
            self.collapse_paste();
        }
        self.dirty = true;
    }

    /// 剪贴板含图片（macOS）：osascript 读 PNG → 压缩 → 注册附件 → 占位 id。
    fn paste_clipboard_image(&mut self) -> Option<usize> {
        let bytes = crate::tui::gfx::clipboard_image_png()?;
        self.register_image(&bytes)
    }

    /// 把占位符换回真实内容（提交时）。
    fn expand_pastes(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (token, body) in &self.pastes {
            out = out.replace(token.as_str(), body);
        }
        out
    }

    /// 输入中的图片路径（独立成行的路径，或 `![alt](path)` 整行）→ 读文件
    /// → 压缩注册 → 替换为 `#[image N]` 占位。无法识别/读取的行原样保留。
    fn expand_image_paths(&mut self, text: &str) -> String {
        let cwd = self.cwd.clone();
        let mut out: Vec<String> = Vec::new();
        for line in text.lines() {
            let trimmed = line.trim();
            let path = markdown_image_path(trimmed).or_else(|| standalone_image_path(trimmed));
            if let Some(p) = path {
                let expanded = expand_home(&p);
                let path_buf = if std::path::Path::new(&expanded).is_absolute() {
                    std::path::PathBuf::from(&expanded)
                } else {
                    std::path::PathBuf::from(&cwd).join(&expanded)
                };
                if let Some(id) = self.register_image_file(&path_buf) {
                    out.push(image_marker(id));
                    continue;
                }
            }
            out.push(line.to_string());
        }
        out.join("\n")
    }

    /// 解析文本中的 `#[image N]` 引用 → 附件（去重保序）；未知 id 忽略。
    fn resolve_images(&self, text: &str) -> Vec<crate::api::types::ImageAttachment> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for cap in IMAGE_MARKER_RE.captures_iter(text) {
            if let Ok(n) = cap[1].parse::<usize>()
                && n >= 1
                && n <= self.attachments.len()
                && seen.insert(n)
            {
                out.push(self.attachments[n - 1].clone());
            }
        }
        out
    }

    /// 原始图片字节 → 压缩（API 上限内）→ 注册附件 → 占位 id。
    fn register_image(&mut self, bytes: &[u8]) -> Option<usize> {
        let prepared = crate::api::image::prepare_image(bytes)?;
        self.attachments.push(crate::api::types::ImageAttachment {
            media_type: prepared.media_type,
            data: prepared.data,
        });
        Some(self.attachments.len())
    }

    /// 图片文件 → 注册附件（读失败/非图片 → None）。
    fn register_image_file(&mut self, path: &std::path::Path) -> Option<usize> {
        let bytes = std::fs::read(path).ok()?;
        self.register_image(&bytes)
    }

    /// 本回合生效的 Session：`Session` 在 `Arc` 里不可变，而 shift+tab 要
    /// 换权限模式——每回合派生一份带当前模式的副本（其余字段是共享句柄：
    /// Runtime 的 watch 通道、任务存储、watch 注册表都仍指向同一份状态）。
    fn session_for_turn(&self) -> Arc<Session> {
        if self.permission_mode == self.session.permission_mode {
            return self.session.clone();
        }
        let mut session = (*self.session).clone();
        session.permission_mode = self.permission_mode;
        Arc::new(session)
    }

    /// slash 输出行入队（临时提示：渲染在消息之后、输入框上方，TTL 后消失）。
    fn push_slash_output(&mut self, text: String) {
        for line in text.lines() {
            self.slash_lines.push(line.to_string());
        }
        self.slash_at = Some(std::time::Instant::now());
        self.dirty = true;
    }

    /// slash 命令分发。返回 true = 已消费。
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
            "team" => self.slash_team(arg),
            other => {
                // 技能名（prompt Command：技能与内置命令同注册表，输入
                // /技能名 即执行；全量正文不进上下文，见下方 marker 注释）。
                let skills = crate::skills::load_skills(
                    &self.session.home,
                    &std::path::PathBuf::from(&self.cwd),
                );
                if let Some(skill) = skills.iter().find(|s| s.name == other) {
                    // 渐进披露：只提交 `✦ 技能名 [参数]` 标记，正文由模型经
                    // Skill 工具指针（`✦ name — read <path>`）+ Read 按需读取。
                    let marker = if arg.is_empty() {
                        format!("✦ {}", skill.name)
                    } else {
                        format!("✦ {} {}", skill.name, arg)
                    };
                    self.start_turn(marker, true);
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
        let mut lines = vec!["可用命令：".to_string()];
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
        self.warnings.clear();
        self.reset_flushed();
        self.push_slash_output("✓ 已清空对话，开始新会话。".to_string());
    }

    fn slash_model(&mut self, arg: &str) {
        if arg.is_empty() {
            self.open_model_menu();
            return;
        }
        self.set_model(arg.to_string());
    }

    /// 切换运行时模型并持久化为默认（与 /theme /think 同路径：写 project 层）。
    fn set_model(&mut self, model: String) {
        let _ = self.session.runtime.model_tx.send(model.clone());
        self.persist_model(&model);
        self.push_slash_output(format!("✓ 模型已切换: {model}"));
    }

    /// 模型选择写回 `.bingo/settings.json`（下次启动作为默认；--model 仍可覆盖）。
    fn persist_model(&self, model: &str) {
        let cwd = std::path::PathBuf::from(&self.cwd);
        let _ = crate::settings::upsert_project_settings(
            &cwd,
            &serde_json::json!({ "model": model }),
        );
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
            let models = match client.list_models().await {
                Ok(m) => m,
                Err(e) => {
                    // #18/main #91：短操作失败可见（页面级错误行，error 色），
                    // 行为降级不变（菜单仍显示空/已知模型）——「降级 + 可见」。
                    let _ = events.send(UiEvent::Error {
                        code: crate::error::map_error(&e),
                        msg: e.to_string(),
                        level: crate::error::ErrorLevel::Page,
                        context: crate::error::ErrorContext::ShortSync,
                    });
                    Vec::new()
                }
            };
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
                self.persist_model(&model);
                self.push_slash_output(format!("✓ 模型已切换: {provider} · {model}"));
                true
            }
            KeyCode::Esc => {
                // 二级 → 回一级；一级 → 整体退出（逐级返回）。
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
        self.reset_flushed();
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
            let tokens = match session
                .client
                .count_tokens(&model, &session.system, &msgs)
                .await
            {
                Ok(t) => t,
                Err(e) => {
                    // #18/main #91：短操作失败可见（页面级错误行），
                    // 行为降级不变（预算仍显示 0）。
                    let _ = events.send(UiEvent::Error {
                        code: crate::error::map_error(&e),
                        msg: e.to_string(),
                        level: crate::error::ErrorLevel::Page,
                        context: crate::error::ErrorContext::ShortSync,
                    });
                    0
                }
            };
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
        if arg.is_empty() {
            self.open_think_menu();
            return;
        }
        self.set_think_level(arg);
    }

    /// 设置思考级别（运行时 + 持久化）。等级表 = off + THINKING_LEVELS：
    /// off 不发参数，其余发 adaptive thinking + output_config.effort。
    fn set_think_level(&mut self, arg: &str) {
        let level = if arg == "off" {
            None
        } else if crate::api::types::THINKING_LEVELS.contains(&arg) {
            Some(arg.to_string())
        } else {
            self.push_slash_output(
                "用法: /think [off|low|medium|high|xhigh|max]".to_string(),
            );
            return;
        };
        let _ = self.session.runtime.thinking_tx.send(level.clone());
        let saved = level.as_deref().unwrap_or("off");
        let cwd = std::path::PathBuf::from(&self.cwd);
        let _ = crate::settings::upsert_project_settings(
            &cwd,
            &serde_json::json!({ "thinkingLevel": saved }),
        );
        self.push_slash_output(format!("✓ 思考级别已设置: {saved}"));
    }

    /// 进入 `/think` 等级选择器：预选当前等级（未设置 = off）。
    fn open_think_menu(&mut self) {
        let current = self.session.runtime.thinking.borrow().clone();
        let current = current.as_deref().unwrap_or("off");
        let selected = THINK_LEVELS
            .iter()
            .position(|(name, _)| *name == current)
            .unwrap_or(0);
        self.think_menu = Some(ThinkMenu { selected });
        self.slash_suggestions.clear();
    }

    /// 思考等级菜单键盘：↑↓ 移动（循环）、Enter 确认、Esc 退出。返回已消费。
    fn think_menu_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let Some(menu) = &mut self.think_menu else {
            return false;
        };
        match code {
            KeyCode::Down if !modifiers.contains(KeyModifiers::CONTROL) => {
                menu.selected = (menu.selected + 1) % THINK_LEVELS.len();
                true
            }
            KeyCode::Up if !modifiers.contains(KeyModifiers::CONTROL) => {
                menu.selected = menu
                    .selected
                    .checked_sub(1)
                    .unwrap_or(THINK_LEVELS.len() - 1);
                true
            }
            KeyCode::Enter => {
                let selected = menu.selected.min(THINK_LEVELS.len() - 1);
                self.think_menu = None;
                self.set_think_level(THINK_LEVELS[selected].0);
                true
            }
            KeyCode::Esc => {
                self.think_menu = None;
                true
            }
            _ => false,
        }
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

    /// `/team <子命令>`（D31 项目级编队）：分发到 team_cmd，输出多行一次入队。
    fn slash_team(&mut self, arg: &str) {
        let lines = crate::team_cmd::run(&self.session, &std::path::PathBuf::from(&self.cwd), arg);
        self.push_slash_output(lines.join("\n"));
    }

    /// 重建 slash 下拉建议（输入变化时调用）：
    /// `/` 开头且无参数时显示；空 query 列全部（内置命令 + 技能），
    /// 否则前缀/包含匹配（generateCommandSuggestions 的简化版）。
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
        // 技能并入（/ 菜单含 skills）。描述截断：
        // 超长行不折行，会被终端自己折成两行、把帧高度算错；
        // 上限 MAX_LISTING_DESC_CHARS。
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

    /// 应用选中建议（applyCommandSuggestion）：`/name ` 回填输入。
    fn apply_slash_suggestion(&mut self) {
        if let Some(s) = self.slash_suggestions.get(self.slash_selected) {
            self.input = format!("/{} ", s.name);
        }
        self.slash_suggestions.clear();
    }

    /// 回合结束后提交下一条排队消息（每次一条：下一轮结束再接着走）。
    fn submit_queued(&mut self) {
        if self.busy || self.queued.is_empty() {
            return;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let text = self.queued.remove(0);
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

    /// 回合结束后处理：先发 TurnEnd（busy 复位 / 完成行立即出现），
    /// 记忆抽取后置——它是一次非流式模型调用（数秒），收尾不应阻塞
    /// 回合结束的 UI 表现；抽取与下一回合（如 watch 唤醒）并行无碍。
    async fn finish_turn(
        events: &mpsc::UnboundedSender<UiEvent>,
        session: &Arc<Session>,
        outcome: &crate::query::QueryOutcome,
    ) {
        if outcome.aborted {
            let _ = events.send(UiEvent::Warning("回合已中断".to_string()));
        }
        let _ = events.send(UiEvent::TurnEnd);
        let cwd = std::env::current_dir().unwrap_or_default();
        crate::memory::extract_memory(session, &outcome.messages, &session.home, &cwd).await;
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
        let session = self.session_for_turn();
        let events = self.events.clone();
        let asks = self.asks.clone();
        let images = self.resolve_images(&text);
        // 先订阅再复位：tokio watch 的 send 在无 receiver 时不更新值——
        // 上一轮 spawn 结束后 receiver 已全部 drop，若先 send(false) 会静默
        // 失效（值保持 true），新回合会在连接阶段被误判为中断。
        let cancel_rx = self.cancel_tx.subscribe();
        self.cancel_tx.send_replace(false);
        tokio::spawn(async move {
            let _ = events.send(UiEvent::TurnStart);
            let mut ui = crate::ui::tui_hooks(events.clone(), asks);
            let history = Self::load_history(&session, &mut ui.on_warning);
            let result = run_query(&session, history, &text, &images, &mut ui, Some(cancel_rx)).await;
            match result {
                Ok(outcome) => {
                    Self::finish_turn(&events, &session, &outcome).await;
                }
                Err(e) => {
                    let _ = events.send(UiEvent::Error {
                        code: crate::error::map_error(&e),
                        msg: e.to_string(),
                        // 回合级错误 = 长回合失败 → 全流程级整屏态（AC-53）。
                        level: crate::error::ErrorLevel::Full,
                        context: crate::error::ErrorContext::LongTurn,
                    });
                }
            }
        });
    }

    /// bash 模式回合（processBashCommand）：`!` 命令直接执行，
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
        let session = self.session_for_turn();
        let events = self.events.clone();
        let asks = self.asks.clone();
        // 与 start_turn 相同：先订阅再复位（send 无 receiver 时不更新值）。
        let cancel_rx = self.cancel_tx.subscribe();
        self.cancel_tx.send_replace(false);
        tokio::spawn(async move {
            let _ = events.send(UiEvent::TurnStart);
            let mut ui = crate::ui::tui_hooks(events.clone(), asks);
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
                    let _ = events.send(UiEvent::Error {
                        code: crate::error::map_error(&e),
                        msg: e.to_string(),
                        // 回合级错误 = 长回合失败 → 全流程级整屏态（AC-53）。
                        level: crate::error::ErrorLevel::Full,
                        context: crate::error::ErrorContext::LongTurn,
                    });
                }
            }
        });
    }

    /// 对话框键盘输入（Select 语义）：
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
                        self.push_ask_message(ASK_DECLINED_TEXT.to_string());
                    }
                    let _ = tx.send(DialogAction::Cancel);
                }
                true
            }
            _ => false,
        }
    }

    /// AskUserQuestion 回答消息：header + 一行 `· 问题 → 答案`。作为
    /// 普通用户消息进消息流（不再是指渲染在输入框上方的瞬态结果块）。
    fn ask_answer_text(question: &str, answer: &str) -> String {
        format!("User answered the questions:\n  · {question} → {answer}")
    }

    /// 把一条回答/拒绝记录为普通用户消息：与用户输入同样渲染（气泡）、
    /// 定稿、落盘进 scrollback，并随会话持久——不回合瞬态残留。
    fn push_ask_message(&mut self, text: String) {
        self.messages.push(UiMessage {
            role: Role::User,
            text,
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        });
    }

    /// 提交 Other 自由输入（CC SelectInputOption onSubmit：空文本 = 取消）。
    fn submit_ask_answer(&mut self, text: String) {
        if text.trim().is_empty() {
            let free_text = self
                .pending_ask
                .as_ref()
                .is_some_and(|(r, _)| r.free_text);
            if free_text {
                self.push_ask_message(ASK_DECLINED_TEXT.to_string());
            }
            if let Some((_, tx)) = self.pending_ask.take() {
                let _ = tx.send(DialogAction::Cancel);
            }
            return;
        }
        if let Some((request, tx)) = self.pending_ask.take() {
            let question = request.question.clone();
            let answer = text.clone();
            self.push_ask_message(Self::ask_answer_text(&question, &answer));
            let _ = tx.send(DialogAction::Answer(text));
        }
    }

    /// 确认选项 `index`（0 起；越界 = 取消）。
    fn choose_ask_option(&mut self, index: usize) {
        if let Some((request, tx)) = self.pending_ask.take() {
            if index < request.options.len() {
                if request.free_text {
                    let question = request.question.clone();
                    let answer = request.options[index].clone();
                    self.push_ask_message(Self::ask_answer_text(&question, &answer));
                }
                let _ = tx.send(DialogAction::Confirm(index));
            } else {
                let _ = tx.send(DialogAction::Cancel);
            }
        }
    }

    /// 键盘事件。真实时钟版本；语义见 [`Chat::on_key_at`]。
    pub fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        self.on_key_at(code, modifiers, std::time::Instant::now())
    }

    /// 复位错误态（AC-03 复位四项之一：错误行/整屏错误态清除）。
    fn dismiss_error(&mut self) {
        self.last_error = None;
    }

    /// #18 全流程级整屏错误态按键（AC-26/53：返回路径非死路）：
    /// Enter = 重试（重跑最近输入）、Esc = 返回、Ctrl+C = 退出，其余忽略。
    fn error_screen_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        now: std::time::Instant,
    ) -> bool {
        match code {
            KeyCode::Enter => {
                self.dismiss_error();
                if !self.last_prompt.is_empty() {
                    self.start_turn(self.last_prompt.clone(), true);
                }
                true
            }
            KeyCode::Esc => {
                self.dismiss_error();
                true
            }
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => self.ctrl_c(now),
            _ => true,
        }
    }

    /// 键盘事件（`now` 可注入：Ctrl+C 双击窗口与粘贴突发检测都依赖时钟）。
    ///
    /// 优先级自上而下：对话框 → `/model` 菜单 → 历史搜索 → 中断/退出语义
    /// → 编辑键。返回是否消费。
    pub fn on_key_at(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        now: std::time::Instant,
    ) -> bool {
        let pasting = self.track_burst(now);
        // #18 全流程级整屏错误态：首要动作 Enter=重试 / Esc=返回，其余忽略。
        if let Some(err) = &self.last_error
            && err.level == crate::error::ErrorLevel::Full
        {
            return self.error_screen_key(code, modifiers, now);
        }
        if self.ask_key(code) {
            return true;
        }
        // `/model` `/think` 选择器优先于输入（↑↓/Enter/Esc 全消费）。
        if self.model_menu_key(code, modifiers) {
            return true;
        }
        if self.think_menu_key(code, modifiers) {
            return true;
        }
        if self.search.is_some() {
            return self.search_key(code, modifiers);
        }
        // 实体选择器（ctrl+g / 聚焦时 ↑↓ Enter Esc）先于全局 Esc 语义。
        if self.entity_key(code, modifiers) {
            return true;
        }
        // 中断（busy）与退出（空闲）都挂在 Ctrl+C / Esc 上，先于编辑键判定。
        if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
            return self.ctrl_c(now);
        }
        if code == KeyCode::Esc {
            return self.escape(now);
        }
        self.notice = None;
        // slash 下拉键盘（Tab 补全 / Esc 关闭 / ↑↓ 导航）优先于输入。
        if !self.bash_mode && self.slash_menu_key(code, modifiers) {
            return true;
        }
        if modifiers.contains(KeyModifiers::CONTROL)
            && let KeyCode::Char(c) = code
        {
            return self.control_key(c);
        }
        if modifiers.contains(KeyModifiers::ALT)
            && let KeyCode::Char(c) = code
        {
            return self.alt_key(c);
        }
        match code {
            // Shift+Tab：权限模式循环（CC app:cyclePermissionMode）。
            KeyCode::BackTab => {
                self.cycle_permission_mode();
                true
            }
            KeyCode::Left => {
                self.cursor = crate::tui::input::prev_char(&self.input, self.cursor);
                true
            }
            KeyCode::Right => {
                self.cursor = crate::tui::input::next_char(&self.input, self.cursor);
                true
            }
            KeyCode::Home => {
                self.cursor = crate::tui::input::line_start(&self.input, self.cursor);
                true
            }
            KeyCode::End => {
                self.cursor = crate::tui::input::line_end(&self.input, self.cursor);
                true
            }
            KeyCode::Up => self.vertical(false),
            KeyCode::Down => self.vertical(true),
            KeyCode::Backspace => {
                // bash 模式下空输入退格退出 shell 模式（CC）。
                if self.bash_mode && self.input.is_empty() {
                    self.bash_mode = false;
                    return true;
                }
                self.snapshot(EditKind::Delete);
                crate::tui::input::backspace(&mut self.input, &mut self.cursor);
                self.after_edit();
                true
            }
            KeyCode::Delete => {
                self.snapshot(EditKind::Delete);
                crate::tui::input::delete(&mut self.input, &mut self.cursor);
                self.after_edit();
                true
            }
            KeyCode::Tab if self.bash_mode => {
                self.complete_bash_history();
                true
            }
            // Shift+Enter（终端上报增强键盘时可得）与粘贴中的 Enter 都是换行。
            KeyCode::Enter
                if pasting
                    || modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                self.insert_newline();
                // 粘贴中的换行才可能堆出大段文本 → 到阈值就折叠为占位符。
                if pasting {
                    self.collapse_paste();
                }
                true
            }
            KeyCode::Enter => {
                // `\` + Enter：所有终端都能打出的换行（CC）。
                if self.input.ends_with('\\') && self.cursor == self.input.len() {
                    self.snapshot(EditKind::Bulk);
                    self.input.pop();
                    self.cursor = self.input.len();
                    self.insert_newline();
                    return true;
                }
                self.submit();
                true
            }
            // 空输入的 `?` 开关快捷键面板；有文本时是普通字符。
            KeyCode::Char('?') if self.input.is_empty() && !self.bash_mode => {
                self.help_visible = !self.help_visible;
                true
            }
            // 空输入的 `!` 进入 shell 模式（`!` 本身不入输入）。
            KeyCode::Char('!') if self.input.is_empty() && !self.bash_mode => {
                self.bash_mode = true;
                true
            }
            KeyCode::Char(c) if !c.is_control() => {
                self.snapshot(EditKind::Insert);
                let mut buf = [0u8; 4];
                crate::tui::input::insert(
                    &mut self.input,
                    &mut self.cursor,
                    c.encode_utf8(&mut buf),
                );
                self.after_edit();
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
            _ => false,
        }
    }

    /// 粘贴突发检测：连续 [`PASTE_BURST_KEYS`] 个间隔小于
    /// [`PASTE_BURST_GAP`] 的按键即判定为粘贴（局限见该常量的注释）。
    fn track_burst(&mut self, now: std::time::Instant) -> bool {
        let fast = self
            .last_key_at
            .is_some_and(|last| now.duration_since(last) < PASTE_BURST_GAP);
        self.burst_keys = if fast { self.burst_keys + 1 } else { 0 };
        self.last_key_at = Some(now);
        self.burst_keys >= PASTE_BURST_KEYS
    }

    /// Ctrl+C：busy 中断；空闲且有文本清空（进历史，↑ 可找回）；
    /// 空闲空输入第一次提示，[`CTRL_C_WINDOW`] 内第二次退出。
    fn ctrl_c(&mut self, now: std::time::Instant) -> bool {
        if self.busy {
            self.interrupt();
            return true;
        }
        if !self.input.is_empty() {
            self.clear_input_into_history();
            self.notice = None;
            self.ctrl_c_at = None;
            return true;
        }
        let armed = self
            .ctrl_c_at
            .is_some_and(|at| now.duration_since(at) <= CTRL_C_WINDOW);
        if armed {
            self.exit = true;
            return true;
        }
        self.ctrl_c_at = Some(now);
        self.notice = Some("Press ctrl-c again to exit");
        true
    }

    /// Esc：busy 中断；菜单/建议关闭；空闲且有文本双击清空（存入历史）。
    fn escape(&mut self, now: std::time::Instant) -> bool {
        if self.busy {
            self.interrupt();
            return true;
        }
        if !self.slash_suggestions.is_empty() {
            self.slash_suggestions.clear();
            return true;
        }
        if self.help_visible {
            self.help_visible = false;
            return true;
        }
        if self.bash_mode && self.input.is_empty() {
            self.bash_mode = false;
            return true;
        }
        if self.input.is_empty() {
            self.notice = None;
            return false;
        }
        let armed = self
            .esc_at
            .is_some_and(|at| now.duration_since(at) <= ESC_WINDOW);
        if armed {
            self.clear_input_into_history();
            self.esc_at = None;
            self.notice = None;
            return true;
        }
        self.esc_at = Some(now);
        self.notice = Some("Press esc again to clear");
        true
    }

    /// 中断当前回合（Esc / Ctrl+C on busy）。
    fn interrupt(&mut self) {
        self.interrupted = true;
        self.cancel_tx.send_replace(true);
    }

    /// Ctrl+<char> 编辑命令（readline 语义）。
    fn control_key(&mut self, c: char) -> bool {
        match c {
            'a' => {
                self.cursor = crate::tui::input::line_start(&self.input, self.cursor);
                true
            }
            'e' => {
                self.cursor = crate::tui::input::line_end(&self.input, self.cursor);
                true
            }
            'k' => {
                self.snapshot(EditKind::Bulk);
                self.kill = crate::tui::input::kill_to_end(&mut self.input, &mut self.cursor);
                self.after_edit();
                true
            }
            'u' => {
                // bash 模式空输入：ctrl+u 退出 shell 模式（CC）。
                if self.bash_mode && self.input.is_empty() {
                    self.bash_mode = false;
                    return true;
                }
                self.snapshot(EditKind::Bulk);
                self.kill = crate::tui::input::kill_to_start(&mut self.input, &mut self.cursor);
                self.after_edit();
                true
            }
            'w' => {
                self.snapshot(EditKind::Bulk);
                self.kill = crate::tui::input::kill_word(&mut self.input, &mut self.cursor);
                self.after_edit();
                true
            }
            'y' => {
                if self.kill.is_empty() {
                    return true;
                }
                self.snapshot(EditKind::Bulk);
                let kill = std::mem::take(&mut self.kill);
                crate::tui::input::insert(&mut self.input, &mut self.cursor, &kill);
                self.kill = kill;
                self.after_edit();
                true
            }
            // ctrl+d 只在有文本时删光标后字符（空输入不退出会话）。
            'd' => {
                if self.input.is_empty() {
                    return true;
                }
                self.snapshot(EditKind::Delete);
                crate::tui::input::delete(&mut self.input, &mut self.cursor);
                self.after_edit();
                true
            }
            'j' => {
                self.insert_newline();
                true
            }
            'l' => {
                self.force_redraw = true;
                self.dirty = true;
                true
            }
            'o' => {
                self.toggle_transcript();
                true
            }
            'r' => {
                self.open_search();
                true
            }
            's' => {
                self.toggle_stash();
                true
            }
            't' => {
                self.tasks_visible = !self.tasks_visible;
                if self.tasks_visible {
                    // 手动打开：全部完成也保留面板（用户显式要看的态）。
                    self.tasks_auto = false;
                    self.refresh_tasks();
                }
                self.dirty = true;
                true
            }
            // Ctrl+_ 到达时的字节是 0x1F，crossterm 报成 Ctrl+7；开启增强
            // 键盘协议的终端才报 `_` 或 `/`——三者都当撤销。
            '7' | '_' | '/' => {
                self.undo_edit();
                true
            }
            _ => false,
        }
    }

    /// Alt+<char>：词间移动与思考开关。
    fn alt_key(&mut self, c: char) -> bool {
        match c {
            'b' => {
                self.cursor = crate::tui::input::word_left(&self.input, self.cursor);
                true
            }
            'f' => {
                self.cursor = crate::tui::input::word_right(&self.input, self.cursor);
                true
            }
            't' => {
                self.toggle_thinking();
                true
            }
            _ => false,
        }
    }

    /// ↑/↓：先在多行输入内移动，到首/末行再切换历史；
    /// busy 且有队列时 ↑ 取回最后一条排队消息。
    fn vertical(&mut self, down: bool) -> bool {
        // 取回排队消息只在输入为空时发生：正在写的内容不该被顶掉。
        if !down && self.busy && self.input.is_empty() && !self.queued.is_empty() {
            if let Some(text) = self.queued.pop() {
                self.set_input(text);
            }
            return true;
        }
        let width = self.input_width();
        if let Some(cursor) =
            crate::tui::input::move_row(&self.input, self.cursor, width, down)
        {
            self.cursor = cursor;
            return true;
        }
        let next = if down {
            self.history.newer()
        } else {
            self.history.older(&self.input)
        };
        match next {
            Some(text) => {
                self.snapshot(EditKind::Bulk);
                self.input = text;
                self.cursor = self.input.len();
                self.update_slash_suggestions();
                true
            }
            None => true,
        }
    }

    /// 输入区可用宽度（终端宽 - 前缀 2 列 - 右侧留白）。
    pub fn input_width(&self) -> usize {
        self.width.saturating_sub(4).max(8)
    }

    /// 换行插入（`\`+Enter / Ctrl+J / Shift+Enter / 粘贴中的 Enter）。
    fn insert_newline(&mut self) {
        self.snapshot(EditKind::Bulk);
        crate::tui::input::insert(&mut self.input, &mut self.cursor, "\n");
        self.after_edit();
    }

    /// 替换整个输入并把光标放到末尾。
    pub fn set_input(&mut self, text: impl Into<String>) {
        self.input = text.into();
        self.cursor = self.input.len();
        self.update_slash_suggestions();
    }

    /// 清空输入并把它记进历史（Ctrl+C / 双击 Esc：可用 ↑ 找回）。
    fn clear_input_into_history(&mut self) {
        let text = std::mem::take(&mut self.input);
        self.cursor = 0;
        self.undo.clear();
        self.record_history(&text);
        self.update_slash_suggestions();
    }

    /// 每次编辑之后的收尾：刷新下拉建议、离开历史浏览态。
    fn after_edit(&mut self) {
        self.history.detach();
        self.update_slash_suggestions();
    }

    /// 记录并持久化一条 prompt。写盘失败只降级为会话内历史（记一次，
    /// 不反复重试）。
    fn record_history(&mut self, text: &str) {
        if !self.history.record(text) || !self.history_writable {
            return;
        }
        let path = std::path::PathBuf::from(&self.cwd);
        if crate::tui::history::save(&self.session.home, &path, self.history.entries()).is_err() {
            self.history_writable = false;
        }
    }

    /// 撤销栈：连续插入合并为一步，删除/整体替换各自成步。
    fn snapshot(&mut self, kind: EditKind) {
        let coalesce = kind != EditKind::Bulk
            && self.last_edit == Some(kind)
            && !self.undo.is_empty();
        self.last_edit = Some(kind);
        if coalesce {
            return;
        }
        self.undo.push((self.input.clone(), self.cursor));
        if self.undo.len() > UNDO_MAX {
            self.undo.remove(0);
        }
    }

    /// Ctrl+_：回到上一步的文本与光标。
    fn undo_edit(&mut self) {
        let Some((text, cursor)) = self.undo.pop() else {
            return;
        };
        self.input = text;
        self.cursor = cursor.min(self.input.len());
        self.last_edit = None;
        self.update_slash_suggestions();
    }

    /// Ctrl+S：有文本则暂存并清空，空输入则恢复（含光标位）。
    fn toggle_stash(&mut self) {
        if self.input.is_empty() {
            if let Some((text, cursor)) = self.stash.take() {
                self.input = text;
                self.cursor = cursor.min(self.input.len());
                self.update_slash_suggestions();
            }
            return;
        }
        self.stash = Some((std::mem::take(&mut self.input), self.cursor));
        self.cursor = 0;
        self.last_edit = None;
        self.update_slash_suggestions();
    }

    /// Shift+Tab：default → acceptEdits → plan → default。
    /// bypassPermissions / dontAsk 只有启动时就在该模式才留在循环里
    /// （危险模式不能靠一次误按进入）。
    fn cycle_permission_mode(&mut self) {
        self.permission_mode = match self.permission_mode {
            PermissionMode::Default => PermissionMode::AcceptEdits,
            PermissionMode::AcceptEdits => PermissionMode::Plan,
            PermissionMode::Plan => PermissionMode::Default,
            // 启动即 bypass/dontAsk：在它与 default 之间切换，不引入新的危险模式。
            PermissionMode::BypassPermissions | PermissionMode::DontAsk => {
                PermissionMode::Default
            }
        };
        // 从 default 切回启动模式（bypass/dontAsk 会话才有这条边）。
        if self.permission_mode == PermissionMode::AcceptEdits
            && matches!(
                self.session.permission_mode,
                PermissionMode::BypassPermissions | PermissionMode::DontAsk
            )
        {
            self.permission_mode = self.session.permission_mode;
        }
        self.dirty = true;
    }

    /// Alt+T：思考开关（off ↔ 上一次的非 off 等级，默认 medium）。
    fn toggle_thinking(&mut self) {
        let current = self.session.runtime.thinking.borrow().clone();
        let next = match current.as_deref() {
            None | Some("off") => self.last_thinking.clone().unwrap_or_else(|| "medium".into()),
            Some(level) => {
                self.last_thinking = Some(level.to_string());
                "off".to_string()
            }
        };
        self.slash_think(&next);
    }

    /// bash 模式 Tab：用本会话执行过的 `!` 命令做前缀补全。
    fn complete_bash_history(&mut self) {
        let prefix = self.input.clone();
        let Some(hit) = self
            .bash_history
            .iter()
            .rev()
            .find(|cmd| cmd.starts_with(&prefix) && cmd.as_str() != prefix)
            .cloned()
        else {
            return;
        };
        self.set_input(hit);
    }

    /// Ctrl+R：进入历史反向搜索（空查询先命中最近一条）。
    fn open_search(&mut self) {
        let mut search = HistorySearch::default();
        if let Some((index, hit)) = self.history.search("", None) {
            search.index = Some(index);
            search.hit = Some(hit);
        }
        self.search = Some(search);
        self.slash_suggestions.clear();
    }

    /// 搜索态键盘：打字过滤、Ctrl+R 取更旧命中、Tab/Esc 采纳继续编辑、
    /// Enter 采纳并提交、Ctrl+C 取消还原。返回是否消费（恒 true）。
    fn search_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let Some(mut search) = self.search.take() else {
            return false;
        };
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Char('r') if ctrl => {
                if let Some((index, hit)) = self.history.search(&search.query, search.index) {
                    search.index = Some(index);
                    search.hit = Some(hit);
                }
                self.search = Some(search);
            }
            KeyCode::Char('c') if ctrl => {}
            KeyCode::Char(c) if !c.is_control() && !ctrl => {
                search.query.push(c);
                match self.history.search(&search.query, None) {
                    Some((index, hit)) => {
                        search.index = Some(index);
                        search.hit = Some(hit);
                    }
                    None => {
                        search.index = None;
                        search.hit = None;
                    }
                }
                self.search = Some(search);
            }
            KeyCode::Backspace => {
                search.query.pop();
                match self.history.search(&search.query, None) {
                    Some((index, hit)) => {
                        search.index = Some(index);
                        search.hit = Some(hit);
                    }
                    None => {
                        search.index = None;
                        search.hit = None;
                    }
                }
                self.search = Some(search);
            }
            KeyCode::Enter => {
                if let Some(hit) = search.hit {
                    self.set_input(hit);
                    self.submit();
                }
            }
            KeyCode::Tab | KeyCode::Esc => {
                if let Some(hit) = search.hit {
                    self.set_input(hit);
                }
            }
            _ => self.search = Some(search),
        }
        true
    }

    /// tick：spinner 帧与运行态 thinking 独立计时。
    ///
    /// 只有存在随 tick 变化的行时才置 dirty：空闲时无条件重建整个文档
    /// 等于 30fps 全量重排，既费 CPU 又让宿主每帧重画一次视口。
    pub fn tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        if self.has_dynamic_rows() {
            self.dirty = true;
        }
        // 底部实体区随注册表变化（agent 状态/频道条数）；变化才置 dirty。
        if self.tick.is_multiple_of(15) {
            self.refresh_entities();
        }
        // slash 临时提示超时消失（操作确认类不留永久占位）。
        if let Some(at) = self.slash_at
            && at.elapsed() > SLASH_OUTPUT_TTL
        {
            self.slash_lines.clear();
            self.slash_at = None;
            self.dirty = true;
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

    /// 是否存在随 tick 变化的行（spinner 帧 / 运行时长 / 状态行）。
    /// 空闲时为 false —— tick 不重建文档，宿主 tick 循环也不唤醒组件。
    pub fn has_dynamic_rows(&self) -> bool {
        self.busy
            || self.messages.iter().any(|m| {
                m.groups.iter().any(|g| g.active)
                    || m.activities.iter().any(|a| a.is_running())
            })
            || (self.tasks_visible
                && self
                    .tasks_cache
                    .iter()
                    .any(|t| t.status == TodoStatus::InProgress))
    }

    /// 宿主 tick 循环是否需要做事。空闲时返回 false，宿主据此整帧跳过——
    /// 无动画、无待处理事件时一个字节都不写。
    pub fn needs_tick(&self) -> bool {
        self.has_dynamic_rows()
            || self.slash_at.is_some()
            || !self.events_rx.is_empty()
            || !self.asks_rx.is_empty()
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
    /// 快照变化才置 dirty——任务区行数变化会改 canvas 高度，交给渲染层
    /// 的形状检测触发全量重画。
    pub fn refresh_tasks(&mut self) {
        let next = self.tasks();
        if next != self.tasks_cache {
            self.tasks_cache = next;
            self.dirty = true;
        }
        // 自动打开的任务区：全部完成即隐藏（工作结束，面板离场），
        // 推一行 2s 瞬态提示给闭合感 + 找回路径；手动打开的保留。
        if self.tasks_auto
            && self.tasks_visible
            && !self.tasks_cache.is_empty()
            && self.tasks_cache.iter().all(|t| t.status == TodoStatus::Done)
        {
            self.tasks_visible = false;
            self.tasks_auto = false;
            let total = self.tasks_cache.len();
            self.push_slash_output(format!("✓ {total}/{total} tasks 完成 · ctrl+t 查看"));
        }
    }

    /// 已完成项最多保留尾部几条，更老的折叠进 `… N done`。
    const DONE_SHOWN: usize = 3;
    /// 活动项窗口大小，超出折叠进 `… +N more`。
    const TODO_SHOWN: usize = 5;

    /// 任务区行（CC TaskListV2 位置：输入框上方）。
    /// 有展开信号且存在任务时显示；自动打开的列表全部完成即隐藏
    /// （`refresh_tasks` 收口），手动打开的保留。
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
                SegStyle::fg(theme.claude),
            );
        }
        header.push_styled("todo".to_string(), theme.text());
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
            // `☒` + 划掉的文本（真实删除线 + dim，见 Theme::strikethrough）。
            let mut line = Line::styled("☒ ", theme.task_done());
            line.push_styled(t[idx].text.clone(), theme.strikethrough());
            out.push(line);
        }
        let active: Vec<&TodoItem> = t
            .iter()
            .filter(|i| i.status != TodoStatus::Done)
            .collect();
        for item in active.iter().take(Self::TODO_SHOWN) {
            // `☐` 未完成；进行中的项整行用主强调色（CC 的活动项高亮）。
            let style = match item.status {
                TodoStatus::Pending => theme.task_open(),
                TodoStatus::InProgress => SegStyle::fg(theme.claude).bold(),
                TodoStatus::Done => unreachable!("filtered"),
            };
            let mut line = Line::styled("☐ ", style);
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
        match self.permission_mode {
            PermissionMode::Default => "default",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::BypassPermissions => "bypassPermissions",
            PermissionMode::DontAsk => "dontAsk",
            PermissionMode::Plan => "plan",
        }
    }

    /// 运行状态行（ActivityIndicator）：busy 时返回动词 + 已耗时 + 已产出
    /// token 数——优先运行中的工具（summary/名字）、其次运行中的
    /// thinking（俏皮词）、兜底 "Working"。空闲返回 None（状态行隐藏）。
    pub fn running_status(&self) -> Option<RunningStatus> {
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
                    // 运行中的后台任务/子代理（ActivityIndicator 显示
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
        Some(RunningStatus {
            verb,
            elapsed,
            tokens: self.output_tokens,
        })
    }

    /// 输入区渲染行（含 ▋ 光标）——行数模型与渲染的单一来源：
    /// chrome 高度按它计数，组装按它出行。
    ///
    /// 空输入给一行 dim 占位提示；多行输入超过 [`INPUT_ROWS_MAX`] 时只显示
    /// 光标所在的那一屏（末尾对齐），行数因此恒有上界。
    pub fn prompt_lines(&self) -> Vec<Line> {
        let style = SegStyle::fg(self.theme.text);
        // 搜索态：输入行显示当前命中，查询串在下方提示行。
        if let Some(search) = &self.search {
            let hit = search.hit.clone().unwrap_or_default();
            return vec![Line::styled(one_line(&hit, self.input_width()), style)];
        }
        if self.input.is_empty() {
            // Block caret sits ON the placeholder's first cell (CC-style):
            // the hint reads as text under the cursor, not glued after it.
            let mut hint = crate::tui::keys::INPUT_PLACEHOLDER.chars();
            hint.next();
            let mut line = Line::styled("▋", style);
            line.push_styled(hint.as_str().to_string(), self.theme.dim());
            return vec![line];
        }
        let width = self.input_width();
        let lines = crate::tui::input::visual_lines(&self.input, width);
        let (row, col) = crate::tui::input::cursor_cell(&self.input, &lines, self.cursor);
        let start = row.saturating_sub(INPUT_ROWS_MAX - 1);
        lines
            .iter()
            .enumerate()
            .skip(start)
            .take(INPUT_ROWS_MAX)
            .map(|(i, line)| {
                if i != row {
                    return Line::styled(line.text.clone(), style);
                }
                // 光标处画 ▋，其后文字照常显示。
                let mut at = 0usize;
                let mut w = 0usize;
                for ch in line.text.chars() {
                    if w >= col {
                        break;
                    }
                    w += crate::tui::line::char_width(ch);
                    at += ch.len_utf8();
                }
                let mut out = Line::styled(line.text[..at].to_string(), style);
                out.push_styled("▋", style);
                out.push_styled(line.text[at..].to_string(), style);
                out
            })
            // 每行恰占一行：历史里回填的文本可能带制表符（折成空格），
            // 否则列宽核算与 canvas 高度都对不上。
            .map(|mut line| {
                line.sanitize();
                line
            })
            .collect()
    }

    /// 刷新底部实体区快照（agent 实例 + 频道）。内容变化才置 dirty。
    pub fn refresh_entities(&mut self) {
        let mut fresh: Vec<EntityRow> = self
            .session
            .agents
            .list()
            .into_iter()
            .map(|s| EntityRow::Agent {
                name: s.name,
                state: s.state.label(),
                description: s.description,
            })
            .collect();
        fresh.extend(self.session.channels.list().into_iter().map(|c| {
            EntityRow::Channel {
                name: c.name,
                seq: c.seq,
                frozen: c.frozen,
            }
        }));
        if fresh != self.entities {
            // 选中项随列表收缩钳制。
            if let Some(i) = self.entity_focus
                && i >= fresh.len()
            {
                self.entity_focus = fresh.len().checked_sub(1);
            }
            self.entities = fresh;
            self.dirty = true;
        }
    }

    /// 底部实体区：收起 = 一行摘要（dim）；聚焦 = 逐行列表 + `❯` 选中 +
    /// 操作提示。没有实体时不占行。
    pub fn entity_rows(&self, width: usize) -> Vec<Line> {
        if self.entities.is_empty() {
            return Vec::new();
        }
        let glyph = |e: &EntityRow| match e {
            EntityRow::Agent { .. } => "◉",
            EntityRow::Channel { .. } => "◇",
        };
        let brief = |e: &EntityRow| match e {
            EntityRow::Agent { name, state, .. } => format!("◉ {name}({state})"),
            EntityRow::Channel { name, seq, frozen } => format!(
                "◇ #{name}({seq}{})",
                if *frozen { "❄" } else { "" }
            ),
        };
        let Some(selected) = self.entity_focus else {
            let summary = self
                .entities
                .iter()
                .map(brief)
                .collect::<Vec<_>>()
                .join(" · ");
            return vec![Line::styled(
                one_line(&format!("  {summary} — ctrl+g 查看"), width),
                SegStyle::fg(self.theme.inactive),
            )];
        };
        let mut rows = Vec::new();
        // 选中项保持可见：窗口围绕 selected 滑动。
        let cap = ENTITY_ROWS_MAX;
        let start = selected.saturating_sub(cap.saturating_sub(1));
        for (i, e) in self.entities.iter().enumerate().skip(start).take(cap) {
            let focused = i == selected;
            let detail = match e {
                EntityRow::Agent {
                    name,
                    state,
                    description,
                } => format!("{} {name} · {state} · {description}", glyph(e)),
                EntityRow::Channel { name, seq, frozen } => format!(
                    "{} #{name} · {seq} 条{}",
                    glyph(e),
                    if *frozen { " · 已冻结" } else { "" }
                ),
            };
            let style = if focused {
                SegStyle::fg(self.theme.permission)
            } else {
                SegStyle::fg(self.theme.inactive)
            };
            let prefix = if focused { "❯ " } else { "  " };
            rows.push(Line::styled(
                one_line(&format!("{prefix}{detail}"), width),
                style,
            ));
        }
        if self.entities.len() > cap {
            rows.push(Line::styled(
                format!("  … 共 {} 个", self.entities.len()),
                SegStyle::fg(self.theme.inactive),
            ));
        }
        rows.push(Line::styled(
            "  ↑↓ 选择 · enter 打开 · esc 关闭".to_string(),
            SegStyle::fg(self.theme.inactive),
        ));
        rows
    }

    /// 实体选择器按键：ctrl+g 开关聚焦；聚焦时 ↑↓ 移动、Enter 打开、
    /// Esc 关闭。返回是否消费。
    pub fn entity_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        if code == KeyCode::Char('g') && ctrl {
            self.refresh_entities();
            if self.entities.is_empty() {
                self.notice = Some("没有子代理实例或频道（Agent 工具派生后出现）");
            } else if self.entity_focus.is_some() {
                self.entity_focus = None;
            } else {
                self.entity_focus = Some(0);
            }
            self.dirty = true;
            return true;
        }
        let Some(i) = self.entity_focus else {
            return false;
        };
        match code {
            KeyCode::Up => {
                self.entity_focus = Some(i.saturating_sub(1));
                self.dirty = true;
                true
            }
            KeyCode::Down => {
                self.entity_focus =
                    Some((i + 1).min(self.entities.len().saturating_sub(1)));
                self.dirty = true;
                true
            }
            KeyCode::Enter => {
                self.open_entity = self.entities.get(i).map(|e| match e {
                    EntityRow::Agent { name, .. } => EntityOpen::Agent(name.clone()),
                    EntityRow::Channel { name, .. } => EntityOpen::Channel(name.clone()),
                });
                self.entity_focus = None;
                self.dirty = true;
                true
            }
            KeyCode::Esc => {
                self.entity_focus = None;
                self.dirty = true;
                true
            }
            _ => false,
        }
    }

    /// `?` 面板的行（快捷键表单一来源）。行数预算由终端高度决定：
    /// 面板不能把视口顶到终端高度以上。
    pub fn help_lines(&self) -> Vec<String> {
        if !self.help_visible {
            return Vec::new();
        }
        // 预留：输入框 3 行 + footer 1 行 + 状态/建议等 4 行余量 + 1 行安全边。
        let budget = self.height.saturating_sub(9);
        crate::tui::keys::help_lines(self.width.saturating_sub(2), budget)
    }

    /// 排队消息行（输入框下方 dim `> {text}`），超出上限折叠为一行。
    pub fn queue_lines(&self) -> Vec<String> {
        if self.queued.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<String> = self
            .queued
            .iter()
            .take(QUEUE_ROWS_MAX)
            .map(|text| format!("> {}", one_line(text, self.width.saturating_sub(4))))
            .collect();
        if self.queued.len() > QUEUE_ROWS_MAX {
            out.push(format!("… +{} more queued", self.queued.len() - QUEUE_ROWS_MAX));
        }
        out
    }

    /// ctrl+r 搜索提示行（`(reverse-i-search)`query': hit`）。
    pub fn search_line(&self) -> Option<String> {
        let search = self.search.as_ref()?;
        let hit = search.hit.as_deref().unwrap_or("");
        Some(one_line(
            &format!("(reverse-i-search)`{}': {hit}", search.query),
            self.width.saturating_sub(2),
        ))
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
        // 顺序定稿：回合中插入的回答消息排在流式 assistant 消息之后，
        // 若前置消息未定稿（正在流式/工具运行中/图片加载中）本消息也不得
        // 定稿——否则落盘会越过流式行，把中间态打印进 scrollback 成为
        // 改不掉的残留（与 `streaming_content_is_not_flushed_until_settled`
        // 同一不变量；现状消息模型前置消息恒已定稿，此守卫只约束新场景）。
        if self.messages[..i]
            .iter()
            .enumerate()
            .any(|(j, _)| !self.message_settled(j))
        {
            return false;
        }
        let m = &self.messages[i];
        // Images load asynchronously. Settling (and therefore flushing) a
        // message whose images are still in flight would print the
        // `#[image]` fallback rows into the scrollback for good: the kitty
        // sequence is only emitted at flush time, and `build_rows` skips
        // flushed segments, so the picture could never appear. Loads that
        // fail drop out of `images_pending` and settle as the placeholder,
        // which is the intended failure display.
        if !self.images_pending.is_empty()
            && gfx::extract_image_urls(&m.text)
                .iter()
                .any(|url| self.images_pending.contains(url))
        {
            return false;
        }
        !m.groups.iter().any(|g| g.active)
            && !m.activities.iter().any(|a| a.is_running())
    }

    /// 构建滚动文档：欢迎卡片 + 消息（text 与活动按插入点交错）+
    /// 权限请求块。`doc.settled` = 前置定稿行数（欢迎卡片 + 全部
    /// 已定稿消息；权限请求块永远不定稿）。
    ///
    /// inline 模式下已落盘的段（[`Chat::flushed_segments`]）整段跳过：
    /// 文档只覆盖动态尾部，落盘越多重建越省。
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
        // 段编号：0 = 欢迎卡，i+1 = messages[i]。clamp 是防御：消息集合
        // 若被整体替换（/clear、/resume）而游标没跟着复位，宁可重复渲染
        // 也不要整屏空白。
        let skip = self.flushed_segments.min(self.messages.len() + 1);
        self.tail_start = 0;
        self.mark_base = 0;

        if skip == 0 {
            rows.extend(welcome_card_rows(
                &theme,
                &self.session.runtime.model.borrow(),
                self.permission_mode_label(),
                &self.cwd,
                width,
            ));
        }
        let mut settled = rows.len();
        let mut settled_segments = 1usize.saturating_sub(skip);
        let mut settled_marks: Vec<SettledMark> = Vec::new();
        if settled_segments > 0 {
            settled_marks.push(SettledMark {
                row_end: settled,
                segments: settled_segments,
            });
        }
        // 消息块间距（CC marginTop=1）：欢迎卡片后与每条消息前留一行。
        for i in 0..self.messages.len() {
            if skip >= i + 2 {
                continue;
            }
            rows.push(Row::new(Line::empty()));
            match self.messages[i].role {
                Role::User => {
                    rows.extend(user_message_rows(
                        &self.messages[i].text,
                        width,
                        &theme,
                    ));
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
                            // 组行是静态的 `⏺ …`：spinner 只在底部状态行。
                            let mut line = Line::styled(
                                "⏺ ",
                                if in_progress {
                                    theme.dim()
                                } else {
                                    theme.tool_done()
                                },
                            );
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
                            // hint 可能是多行 bash 命令：单行化 + 按宽截断，
                            // 否则该行会撑成多行，行数模型与 canvas 脱节。
                            if in_progress
                                && let Some(hint) = &msg.groups[g].last_hint
                            {
                                rows.push(Row::new(Line::styled(
                                    one_line(&format!("  ⎿  {hint}"), width),
                                    SegStyle::fg(theme.inactive),
                                )));
                            }
                            continue;
                        }
                        let (lines, mut local) = layout_activity(
                            act,
                            &[idx],
                            rows.len() as u16,
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
                    // 只在回合结束后渲染：进行中展示会让 `✻ Baked for 0.4s` 在
                    // 工具还在跑时就出现，与底部运行状态行互相矛盾。
                    let show_done_line = i == self.messages.len() - 1 && self.stream_msg.is_none()
                        || self.message_settled(i);
                    if show_done_line
                        && let Some(line) = self.messages[i].activities.iter().rev().find_map(
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
                settled_segments = (i + 2).saturating_sub(skip);
                settled_marks.push(SettledMark {
                    row_end: settled,
                    segments: settled_segments,
                });
            }
        }

        // 权限/提问块（PermissionDialog / AskUserQuestion）：
        // 标题（permission bold）+ 说明（dim）+ 编号选项（Select：
        // `❯ n. label` 焦点指示、desc 副行 dim、Other 自由输入）+ 快捷键提示。
        if let Some((request, _)) = &self.pending_ask {
            let mut title = Line::styled("⏺ ", SegStyle::fg(theme.text));
            title.push_styled(request.title.clone(), theme.permission());
            rows.push(Row::new(title));
            rows.push(Row::new(Line::styled(
                format!("  {}", request.question),
                SegStyle::fg(theme.text),
            )));
            // CC Select：问题与选项之间留一行空白。
            rows.push(Row::new(Line::empty()));
            let focus_color = theme.permission;
            for (opt_idx, option) in request.options.iter().enumerate() {
                let focused = opt_idx == self.ask_focus;
                let mut line = Line::empty();
                let style = if focused {
                    SegStyle::fg(focus_color)
                } else {
                    SegStyle::fg(theme.inactive)
                };
                line.push_styled(if focused { "❯ " } else { "  " }, style);
                line.push_styled(format!("{}. {option}", opt_idx + 1), style);
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
                let style = if focused {
                    SegStyle::fg(focus_color)
                } else {
                    SegStyle::fg(theme.inactive)
                };
                line.push_styled(if focused { "❯ " } else { "  " }, style);
                line.push_styled(format!("{}. Other", other_idx + 1), style);
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
                "enter to submit · esc to cancel"
            } else {
                "enter to select · ↑/↓ to navigate · esc to cancel"
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
                    one_line(line, width),
                    SegStyle::fg(theme.text),
                )));
            }
        }

        self.doc = Doc {
            rows,
            click_ranges,
            settled,
            settled_marks,
            transient_rows: self.slash_lines.len(),
        };
        &self.doc
    }

    /// 复位落盘游标：消息集合被整体替换（/clear、/resume）后段编号失效，
    /// 文档从欢迎卡开始重建（新内容重新落盘进 scrollback）。
    fn reset_flushed(&mut self) {
        self.flushed_segments = 0;
        self.tail_start = 0;
        self.mark_base = 0;
        self.dirty = true;
    }

    /// 落盘 `doc.rows[tail_start..settled]` 之后推进游标：下次重建跳过
    /// 这些段，当前 doc 的尾部起点同步前移（重建前 canvas 就不再画它们）。
    // 生产路径按检查点部分推进（懒落盘）；全量推进保留为测试面原语。
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn advance_flushed(&mut self) {
        if let Some(mark) = self.doc.settled_marks.last().copied() {
            self.advance_flushed_upto(mark);
        }
    }

    /// 落盘 `doc.rows[tail_start..mark.row_end]` 之后推进游标到该检查点。
    /// 同一次构建内可多次调用（`mark_base` 消化检查点里的构建内累计值，
    /// 防止重复累加）；宽度变化后重排安全——段计数与行号无关。
    pub fn advance_flushed_upto(&mut self, mark: SettledMark) {
        self.flushed_segments += mark.segments.saturating_sub(self.mark_base);
        self.mark_base = mark.segments;
        self.tail_start = mark.row_end;
    }

    /// resize 后窗口容量变大：把最近落盘的内容取回活文档重新渲染填满
    /// 窗口。scrollback 里的旧拷贝物理上收不回来——接受上滑时看到一份
    /// 旧宽度的重复（明确认可的取舍，见 research.md D27）。回灌是纯记
    /// 账（不写终端），以「不超出 `doc_budget` 行」为界，超出即回退，
    /// 保证不会与懒落盘互相打架（回灌完不存在越过窗口顶的定稿段）。
    pub fn rehydrate(&mut self, width: usize, doc_budget: usize) {
        loop {
            if self.flushed_segments == 0 {
                break;
            }
            if self.build_rows(width).rows.len() >= doc_budget {
                break;
            }
            self.flushed_segments -= 1;
            if self.build_rows(width).rows.len() > doc_budget {
                self.flushed_segments += 1;
                break;
            }
        }
        self.dirty = true;
    }
}

/// 用户消息行：`❯ ` 前缀 + 按宽折行的正文（含换行的粘贴消息拆成多行）。
/// 每行一个气泡 Row——整条塞进单个 height=1 的 View 会把换行之后的内容
/// 全部裁掉，且让 canvas 实际高度与行数模型脱节。
fn user_message_rows(text: &str, width: usize, theme: &Theme) -> Vec<Row> {
    // 前缀 2 列 + 气泡右侧留白 1 列。
    let body_width = width.saturating_sub(3).max(1);
    let style = SegStyle::fg(theme.text);
    wrap_words(text, body_width)
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            let mut line = Line::styled(if i == 0 { "❯ " } else { "  " }, style);
            line.push_styled(text, style);
            Row::bubble(line, theme.user_message_bg)
        })
        .collect()
}

/// 单行化 + 截断：摘要/提示类文本可能含换行（多行 bash 命令），
/// 而每个 Row 必须恰好一行。
pub(crate) fn one_line(text: &str, width: usize) -> String {
    let flat = crate::tui::line::sanitize(text);
    crate::tui::markdown::truncate(flat.as_ref(), width.max(1))
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

/// Welcome card body (CC WelcomeBox): a starred greeting, the two commands
/// worth knowing, the cwd, and a dim identity line. `bingo` stays `bingo` —
/// this is homage, not impersonation.
fn welcome_rows(
    theme: &Theme,
    model: &str,
    mode: &str,
    cwd: &str,
    width: usize,
) -> Vec<Line> {
    let mut rows = Vec::new();
    let mut greeting = Line::styled(" ✻ ", SegStyle::fg(theme.claude));
    greeting.push_styled("Welcome back!", theme.text());
    rows.push(greeting);
    rows.push(Line::empty());
    rows.push(Line::styled(
        one_line(
            "   /help for help · /status for your current setup",
            width,
        ),
        theme.dim(),
    ));
    rows.push(Line::empty());
    rows.push(Line::styled(
        one_line(&format!("   cwd: {cwd}"), width),
        theme.dim(),
    ));
    rows.push(Line::styled(
        one_line(&format!("   bingo v0.1.0 · {model} · {mode}"), width),
        theme.dim(),
    ));
    rows
}

/// 欢迎卡片行（带 ╭╮ 边框），作为滚动内容的一部分。
fn welcome_card_rows(
    theme: &Theme,
    model: &str,
    mode: &str,
    cwd: &str,
    width: usize,
) -> Vec<Row> {
    let gray = SegStyle::fg(theme.inactive);
    let inner_w = width.saturating_sub(2);
    let mut rows = vec![Row::new(Line::styled(
        format!("╭{}╮", "─".repeat(inner_w)),
        gray,
    ))];
    for line in welcome_rows(theme, model, mode, cwd, inner_w) {
        let mut styled = Line::styled("│", gray);
        let pad = inner_w.saturating_sub(text_width(&line.plain_text()));
        styled.segs.extend(line.segs);
        styled.push_styled(" ".repeat(pad), gray);
        styled.push_styled("│", gray);
        rows.push(Row::new(styled));
    }
    rows.push(Row::new(Line::styled(
        format!("╰{}╯", "─".repeat(inner_w)),
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

    /// 最新定稿检查点覆盖的段数（旧聚合字段的检查点等价读法）。
    fn settled_segments(chat: &Chat) -> usize {
        chat.doc.settled_marks.last().map_or(0, |m| m.segments)
    }

    /// 自建 home 的 Chat（slash 测试用唯一目录，避免与其他测试共享
    /// transcript/task 存储）。cwd 同指 home：/model /think /theme 等
    /// 持久化路径写 `{cwd}/.bingo`，不得污染仓库真实配置。
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
            agents: crate::agents::AgentRegistry::new(),
            channels: crate::channels::ChannelRegistry::new(Default::default()),
            instance: None,
        });
        let mut chat =
            Chat::new(session, events_tx, events_rx, asks_tx, asks_rx, Theme::dark(), None);
        chat.cwd = home.display().to_string();
        chat
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
    /// （renderToolUseMessage = null；任务区面板 / 对话框即展示）。
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

    /// 建任务并返回 id（写入临时 store）。
    async fn create_task(chat: &Chat, subject: &str) -> String {
        chat.session
            .tasks
            .create(&crate::tasks::Task {
                id: String::new(),
                subject: subject.into(),
                description: String::new(),
                active_form: None,
                status: crate::tasks::TaskStatus::Pending,
                owner: None,
                blocks: Vec::new(),
                blocked_by: Vec::new(),
                metadata: Default::default(),
            })
            .await
            .unwrap()
    }

    /// 自动打开的任务区（TaskCreate 信号语义）：全部完成 → 隐藏 + 瞬态行；
    /// 再建任务 → 重现；再全完成 → 再隐藏；隐藏后空闲零写入。
    #[tokio::test]
    async fn auto_todo_hides_when_all_done() {
        let mut chat = chat_with_history("todo-auto");
        let store = chat.session.tasks.clone();
        let id = create_task(&chat, "t1").await;
        chat.tasks_visible = true;
        chat.tasks_auto = true;
        chat.refresh_tasks();
        assert!(chat.tasks_visible, "有活动项时自动面板显示");
        assert!(!chat.task_lines().is_empty());

        store
            .update(
                &id,
                &crate::tasks::TaskPatch {
                    status: Some(crate::tasks::TaskStatus::Completed),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        chat.refresh_tasks();
        assert!(!chat.tasks_visible, "自动面板全部完成后隐藏");
        assert!(!chat.tasks_auto);
        assert!(chat.task_lines().is_empty());
        assert!(
            chat.slash_lines.iter().any(|l| l.contains("✓ 1/1 tasks 完成")),
            "隐藏瞬间推瞬态行: {:?}",
            chat.slash_lines
        );
        assert!(!chat.has_dynamic_rows(), "隐藏后任务区不驱动 tick");

        // 再建任务（expand 信号重开面板）→ 重现；再全完成 → 再隐藏。
        let id2 = create_task(&chat, "t2").await;
        chat.tasks_visible = true;
        chat.tasks_auto = true;
        chat.refresh_tasks();
        assert!(chat.tasks_visible, "新任务后自动面板重现");
        store
            .update(
                &id2,
                &crate::tasks::TaskPatch {
                    status: Some(crate::tasks::TaskStatus::Completed),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        chat.refresh_tasks();
        assert!(!chat.tasks_visible, "再次全完成再次隐藏");
    }

    /// ctrl+t 手动打开的面板：全部完成也保留（用户显式要看的态），且不推瞬态行。
    #[tokio::test]
    async fn manual_todo_stays_when_all_done() {
        let mut chat = chat_with_history("todo-manual");
        let id = create_task(&chat, "t1").await;
        chat.session
            .tasks
            .update(
                &id,
                &crate::tasks::TaskPatch {
                    status: Some(crate::tasks::TaskStatus::Completed),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        ctrl(&mut chat, 't');
        assert!(chat.tasks_visible, "手动打开显示");
        assert!(!chat.tasks_auto, "手动打开非自动");
        chat.refresh_tasks();
        let lines = chat.task_lines();
        let joined: Vec<String> = lines.iter().map(|l| l.plain_text()).collect();
        assert!(joined[0].contains("todo · 1/1 tasks"), "{joined:?}");
        assert!(joined.iter().any(|l| l.starts_with("☒ ")), "{joined:?}");
        assert!(
            chat.slash_lines.is_empty(),
            "手动面板常驻即反馈，不推瞬态行: {:?}",
            chat.slash_lines
        );
    }

    /// `/tasks` 显式请求：全部完成也输出 ☒ 列表，不误报「没有后台任务」。
    #[tokio::test]
    async fn slash_tasks_shows_done_list() {
        let mut chat = chat_with_history("todo-slash");
        let id = create_task(&chat, "t1").await;
        chat.session
            .tasks
            .update(
                &id,
                &crate::tasks::TaskPatch {
                    status: Some(crate::tasks::TaskStatus::Completed),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        chat.slash_tasks();
        let joined = chat.slash_lines.join("\n");
        assert!(joined.contains("☒ t1"), "{joined:?}");
        assert!(!joined.contains("当前没有后台任务"), "{joined:?}");
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

    /// 运行状态行数据（ActivityIndicator）：空闲 None；
    /// busy 时优先运行中工具 summary、其次 thinking 俏皮词、兜底 Working。
    #[test]
    fn running_status_verb_priority() {
        let mut chat = test_chat();
        assert_eq!(chat.running_status(), None, "空闲无状态行");

        chat.busy = true;
        chat.turn_started = Some(std::time::Instant::now());
        let verb = chat.running_status().expect("busy status").verb;
        assert_eq!(verb, "Working", "无活动时兜底");

        let mut tool = tool_activity();
        if let ActivityKind::Tool(t) = &mut tool.kind {
            t.summary = "$ cargo test".to_string();
        }
        chat.messages.push(UiMessage {
            activities: vec![tool],
            ..msg(Role::Assistant, "")
        });
        let verb = chat.running_status().expect("busy status").verb;
        assert_eq!(verb, "$ cargo test", "运行中工具 summary 优先");

        // 运行中的 Watch（子代理/后台任务）动词 = label（CC ActivityIndicator
        // 显示 agent activeForm）：工具之后、thinking 之前。
        chat.messages[0].activities.clear();
        chat.messages[0].activities.push(Activity::new(ActivityKind::Watch(
            WatchCall {
                label: "scout · 列出桌面目录内容".into(),
                kind: crate::watch::WatchKind::Agent,
                status: WatchStatus::Running,
                detail: Some("已产出 43 字符".into()),
                duration_ms: 0,
            },
        )));
        let verb = chat.running_status().expect("busy status").verb;
        assert_eq!(verb, "scout · 列出桌面目录内容", "Watch Running 动词 = label");

        // Done 的 Watch 不再占用动词（落到 thinking/Working）。
        if let ActivityKind::Watch(w) = &mut chat.messages[0].activities[0].kind {
            w.status = WatchStatus::Done;
        }
        let verb = chat.running_status().expect("busy status").verb;
        assert_ne!(verb, "Agent: 列出桌面目录内容", "Done 的 Watch 不占动词");

        chat.messages[0].activities.clear();
        chat.apply_turn_start();
        // TurnStart 追加新消息（索引 1）：占位 thinking 在其中。
        let stage = match &chat.messages[1].activities[0].kind {
            ActivityKind::Thinking(t) => t.stage,
            _ => unreachable!(),
        };
        let verb = chat.running_status().expect("busy status").verb;
        assert_eq!(verb, stage, "thinking 俏皮词");
    }

    /// bash 模式切换：空输入按 `!` 进入、`!` 不插入输入、
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
            agents: crate::agents::AgentRegistry::new(),
            channels: crate::channels::ChannelRegistry::new(Default::default()),
            instance: None,
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

    /// 正文未打断时工具轮之间的 thinking 聚合到同一块（段间空行分隔），
    /// 后续 delta 续写到聚合块。
    #[test]
    fn tool_turn_thinking_blocks_merge_until_text() {
        let mut chat = test_chat();
        chat.apply_turn_start();
        chat.apply_event(UiEvent::ThinkingDelta("plan the fetch".into()));
        chat.apply_event(UiEvent::ToolStart { name: "WebFetch".into() });
        chat.apply_event(UiEvent::ThinkingDelta("got it".into()));
        chat.apply_event(UiEvent::ThinkingDelta(", summarizing".into()));

        let acts = &chat.messages[0].activities;
        assert_eq!(acts.len(), 2, "thinking merged + tool");
        let (first, tool) = (&acts[0], &acts[1]);
        assert!(matches!(&first.kind, ActivityKind::Thinking(t)
            if t.state == ThinkingState::Running && t.segments == 2));
        assert!(matches!(tool.kind, ActivityKind::Tool(_)));
        let text = thinking_text(first);
        assert!(text.contains("plan the fetch"), "first segment: {text}");
        assert!(text.contains("got it, summarizing"), "merged segment: {text}");
    }

    /// 正文打断后 thinking 新开块，不再聚合。
    #[test]
    fn thinking_after_text_opens_new_block() {
        let mut chat = test_chat();
        chat.apply_turn_start();
        chat.apply_event(UiEvent::ThinkingDelta("plan".into()));
        chat.apply_event(UiEvent::TextDelta("正文…".into()));
        chat.apply_event(UiEvent::ThinkingDelta("reflect".into()));

        let acts = &chat.messages[0].activities;
        assert_eq!(acts.len(), 2, "two thinking blocks");
        let (first, second) = (&acts[0], &acts[1]);
        assert!(matches!(&first.kind, ActivityKind::Thinking(t) if t.segments == 1));
        assert!(matches!(&second.kind, ActivityKind::Thinking(t) if t.segments == 1));
        assert_eq!(thinking_text(first), "plan");
        assert_eq!(thinking_text(second), "reflect");
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
        chat.apply_event(UiEvent::TurnEnd);
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
        chat2.apply_event(UiEvent::TurnEnd);
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

    /// 完成行只在回合结束后出现：thinking 已 Done 但工具仍在运行时
    /// 不渲染 `✻ Baked for 0.4s`，避免与底部运行状态行互相矛盾。
    #[test]
    fn thinking_completion_line_waits_for_turn_end() {
        let mut chat = test_chat();
        chat.apply_turn_start();
        chat.apply_event(UiEvent::ThinkingDelta("plan".into()));
        chat.apply_event(UiEvent::ToolStart { name: "Bash".into() });
        chat.build_rows(100);
        let rows: Vec<String> = chat
            .doc
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect();
        assert!(
            !rows.iter().any(|l| l.starts_with("✻ ") && l.contains(" for ")),
            "回合进行中不得有完成行: {rows:?}"
        );
        chat.apply_event(UiEvent::TurnEnd);
        chat.build_rows(100);
        let rows: Vec<String> = chat
            .doc
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect();
        assert!(
            rows.iter().any(|l| l.starts_with("✻ ") && l.contains(" for ")),
            "回合结束后应有完成行: {rows:?}"
        );
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

    /// /model：带参切换运行时模型（下一轮生效）并持久化默认；无参进入选择器。
    #[test]
    fn slash_model_switches_runtime_model() {
        let home = std::env::temp_dir().join(format!("bingo-model-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let mut chat = test_chat_home(home.clone());
        chat.input = "/model deepseek-v4".to_string();
        chat.submit();
        assert_eq!(*chat.session.runtime.model.borrow(), "deepseek-v4");
        assert!(chat.slash_lines.join("\n").contains("deepseek-v4"));
        let saved: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".bingo/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["model"], "deepseek-v4", "选择写回 project settings");
        chat.input = "/model".to_string();
        chat.submit();
        assert!(chat.model_menu.is_some(), "无参进入选择器");
        let _ = std::fs::remove_dir_all(&home);
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

    /// 内置/磁盘技能经 `/技能名` 提交 `✦ 技能名 [参数]` 标记（渐进披露，
    /// 全量正文由模型经 Skill 工具 + Read 按需读取，不进上下文）。
    #[tokio::test]
    async fn slash_skill_submits_marker_not_full_content() {
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
        assert_eq!(
            chat.messages[0].text, "✦ guide",
            "只提交 ✦ 标记: {}",
            &chat.messages[0].text[..chat.messages[0].text.len().min(80)]
        );
        assert!(
            !chat.messages[0].text.contains("诊断指南"),
            "全量正文不再进上下文"
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
                supports_images: None,
            })]);
        Arc::get_mut(&mut chat.session).unwrap().client =
            crate::api::client::Client::new("sk-main".into(), "https://main.example".into());
        // set_provider 需要 providers 表——通过 from_settings 构造更直接。
        drop(providers);
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            ..Default::default()
        };
        settings.providers.insert(
            "deepseek".to_string(),
            crate::settings::ProviderConfig {
                api_key: "sk-ds".into(),
                api_base_url: "https://api.deepseek.com".into(),
                supports_images: None,
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

        // 无参进入等级选择器（预选 off = 首项）。
        chat.input = "/think".to_string();
        chat.submit();
        let menu = chat.think_menu.as_ref().expect("菜单已打开");
        assert_eq!(THINK_LEVELS[menu.selected].0, "off", "未设置时预选 off");
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(chat.think_menu.is_none(), "Esc 退出菜单");

        // 新档位 xhigh：运行时生效 + 持久化。
        chat.input = "/think xhigh".to_string();
        chat.submit();
        let out = chat.slash_lines.join("\n");
        assert!(out.contains("✓ 思考级别已设置: xhigh"), "{out}");
        assert_eq!(
            chat.session.runtime.thinking.borrow().as_deref(),
            Some("xhigh")
        );
        let saved: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.join(".bingo/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["thinkingLevel"], "xhigh");

        chat.input = "/think off".to_string();
        chat.submit();
        let out = chat.slash_lines.join("\n");
        assert!(out.contains("✓ 思考级别已设置: off"), "{out}");
        assert_eq!(chat.session.runtime.thinking.borrow().as_deref(), None);

        chat.input = "/think bogus".to_string();
        chat.submit();
        let out = chat.slash_lines.join("\n");
        assert!(out.contains("用法: /think"), "{out}");
        assert_eq!(
            chat.session.runtime.thinking.borrow().as_deref(),
            None,
            "无效参数不改状态"
        );
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
            "下拉最多 5 行（OVERLAY_MAX_ITEMS）"
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

        // 超长描述截断（MAX_LISTING_DESC_CHARS）：
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
            m.models = vec!["deepseek-v4".to_string(), "deepseek-r1".to_string()];
            m.loading = false;
            m.selected = 1;
        }
        assert!(chat.on_key(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(
            *chat.session.runtime.model.borrow(),
            "deepseek-r1",
            "选中的模型生效"
        );
        assert!(chat.model_menu.is_none(), "确认后关闭菜单");
        assert!(
            chat.slash_lines.join("\n").contains("模型已切换"),
            "确认提示"
        );
    }

    /// /think 无参进入等级选择器：预选当前档位，↑↓ 移动，Enter 确认，Esc 退出。
    #[test]
    fn think_menu_navigates_and_confirms() {
        let home =
            std::env::temp_dir().join(format!("bingo-think-menu-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let mut chat = test_chat_home(home.clone());
        let _ = chat.session.runtime.thinking_tx.send(Some("high".into()));
        chat.input = "/think".to_string();
        chat.submit();
        let menu = chat.think_menu.as_ref().expect("菜单已打开");
        assert_eq!(THINK_LEVELS[menu.selected].0, "high", "预选当前档位");
        // ↑ 到 medium，Enter 确认：运行时生效 + 持久化 + 关闭菜单。
        assert!(chat.on_key(KeyCode::Up, KeyModifiers::empty()));
        assert!(chat.on_key(KeyCode::Enter, KeyModifiers::empty()));
        assert!(chat.think_menu.is_none(), "确认后关闭菜单");
        assert_eq!(
            chat.session.runtime.thinking.borrow().as_deref(),
            Some("medium")
        );
        let saved: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".bingo/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["thinkingLevel"], "medium", "选择持久化");
        // 再开菜单：Esc 直接退出；off 清空等级。
        chat.input = "/think".to_string();
        chat.submit();
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(chat.think_menu.is_none(), "Esc 退出");
        chat.input = "/think off".to_string();
        chat.submit();
        assert_eq!(
            chat.session.runtime.thinking.borrow().as_deref(),
            None,
            "off 清空等级"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// THINK_LEVELS（选择器）与 API 层 THINKING_LEVELS 一致：off + 全档位，顺序一致。
    #[test]
    fn think_levels_match_api_levels() {
        assert_eq!(THINK_LEVELS[0].0, "off");
        let menu: Vec<&str> = THINK_LEVELS[1..].iter().map(|(n, _)| *n).collect();
        assert_eq!(menu, crate::api::types::THINKING_LEVELS.to_vec());
    }

    /// footer 徽标：带思考等级时显示 `模型 · think 等级`，off 只显示模型名。
    #[test]
    fn footer_model_label_shows_thinking_level() {
        assert_eq!(
            model_footer_label("deepseek-v4", Some("high")),
            "deepseek-v4 · think high"
        );
        assert_eq!(model_footer_label("deepseek-v4", None), "deepseek-v4");
        assert_eq!(
            model_footer_label("deepseek-v4", Some("off")),
            "deepseek-v4"
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
            input: json!({"skill": "pdf", "args": "doc.md"}),
            standalone: false,
        });
        chat.drain_events();
        let joined = visible(&mut chat, 120, 30);
        assert!(
            joined.contains("pdf doc.md"),
            "running header shows input summary: {joined}"
        );
        // 完成后 duration 用真实值
        let _ = chat.events.send(UiEvent::ToolDone(crate::query::ToolCallDone {
            name: "Skill".into(),
            summary: "pdf doc.md".into(),
            output: "✦ pdf — read /tmp/skills/SKILL.md".into(),
            is_error: false,
            diff: None,
            duration_ms: 3210,
        }));
        chat.drain_events();
        let joined = visible(&mut chat, 120, 30);
        // CC 双行：耗时并入结果行，且只有慢命令（>2s）才显示。
        // Skill 用 ✦ 图标（类别图标：⏺ 内建 / ◆ MCP / ✦ Skill）。
        assert!(joined.contains("✦ Skill(pdf doc.md)"), "头行: {joined}");
        assert!(joined.contains("✦ pdf"), "结果行只显示 ✦ 技能名: {joined}");
        assert!(
            !joined.contains("read /tmp/skills/SKILL.md"),
            "指针路径不进 TUI 结果行: {joined}"
        );
        assert!(joined.contains("Ran in 3.2s"), "结果行带耗时: {joined}");
        assert!(!joined.contains("3210ms"), "毫秒不再进头行: {joined}");
    }

    /// Agent 对齐 Task renderToolUseMessage=null：ToolStart 不创建工具活动行，
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
            kind: crate::watch::WatchKind::Agent,
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
            kind: crate::watch::WatchKind::Agent,
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
            kind: crate::watch::WatchKind::Agent,
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
            kind: crate::watch::WatchKind::Agent,
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
            kind: crate::watch::WatchKind::Command,
            status: WatchStatus::Running,
            detail: None,
            duration_ms: 0,
            payload: None,
            signal: None,
        });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "tail -f app.log".into(),
            kind: crate::watch::WatchKind::Command,
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
            kind: crate::watch::WatchKind::Agent,
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
            kind: crate::watch::WatchKind::Agent,
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
            kind: crate::watch::WatchKind::Agent,
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
            kind: crate::watch::WatchKind::Agent,
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
            kind: crate::watch::WatchKind::Command,
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
            kind: crate::watch::WatchKind::Command,
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
            kind: crate::watch::WatchKind::Command,
            status: WatchStatus::Idle,
            detail: Some("第 2 轮".into()),
            duration_ms: 4000,
            payload: None,
            signal: None,
        });
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "watch -n 2 ls".into(),
            kind: crate::watch::WatchKind::Command,
            status: WatchStatus::Done,
            detail: None,
            duration_ms: 9000,
            payload: Some(serde_json::json!("done output")),
            signal: None,
        });
        chat.drain_events();
        assert_eq!(chat.messages[0].activities.len(), 1, "updates in place");
        let joined = visible(&mut chat, 120, 30);
        assert!(joined.contains("⏺ watch -n 2 ls"), "header: {joined}");
        assert!(joined.contains("  ⎿  第 2 轮"), "结果行: {joined}");
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

    /// 折叠组以正文为边界：RoundEnd（模型轮次）与 thinking 都不拆组，
    /// 跨轮次的工具并入同一组；正文（TextDelta）才开新组。
    #[test]
    fn group_survives_rounds_and_thinking_until_text() {
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
        assert_eq!(chat.messages[0].groups.len(), 1, "round 2 joins same group");
        let idx = chat.messages[0].activities.len() - 1;
        assert_eq!(chat.messages[0].group_of[idx], Some(0));
        let _ = chat.events.send(UiEvent::ToolStart { name: "Read".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Read".into(),
            input: json!({"file_path": "a.md"}),
            standalone: false,
        });
        chat.drain_events();
        assert_eq!(chat.messages[0].groups.len(), 1, "same-group Read joins group");
        // 正文出现：组关闭，后续工具开新组。
        let _ = chat.events.send(UiEvent::TextDelta("结论…".into()));
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolStart { name: "Grep".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Grep".into(),
            input: json!({"pattern": "post-text"}),
            standalone: false,
        });
        chat.drain_events();
        assert_eq!(chat.messages[0].groups.len(), 2, "text opens new group");
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

    /// 含换行的用户消息（粘贴多行）必须拆成多个单行 Row：一个 Row 恒占
    /// 一行，混进换行就会让行数模型与实际视口高度脱节。
    #[test]
    fn multiline_user_message_wraps_into_single_line_rows() {
        let mut chat = test_chat();
        chat.messages
            .push(msg(Role::User, "first line\nsecond line\nthird"));
        chat.build_rows(40);
        let bubbles: Vec<&Row> = chat.doc.rows.iter().filter(|r| r.bg.is_some()).collect();
        assert_eq!(bubbles.len(), 3, "每行一个气泡 Row");
        for row in &bubbles {
            for seg in &row.line.segs {
                assert!(
                    !seg.text.contains(['\n', '\r']),
                    "Row 必须单行: {:?}",
                    seg.text
                );
            }
        }
        assert!(bubbles[0].line.plain_text().starts_with("❯ first line"));
        // 续行缩进对齐，不重复前缀。
        assert!(bubbles[1].line.plain_text().starts_with("  second line"));
    }

    /// 超长（无换行）用户消息按终端宽度折行，不再撑出屏幕。
    #[test]
    fn long_user_message_wraps_to_width() {
        let mut chat = test_chat();
        let text = "word ".repeat(40);
        chat.messages.push(msg(Role::User, text.trim()));
        chat.build_rows(30);
        let bubbles: Vec<&Row> = chat.doc.rows.iter().filter(|r| r.bg.is_some()).collect();
        assert!(bubbles.len() > 1, "长消息折成多行");
        for row in bubbles {
            // 前缀 2 列 + 正文 ≤ width-1（气泡右侧留白 1 列）。
            assert!(
                text_width(&row.line.plain_text()) <= 29,
                "行宽超限: {:?}",
                row.line.plain_text()
            );
        }
    }

    /// 折叠组的 `⎿ hint` 行可能是多行 bash 命令：必须单行化 + 截断。
    #[test]
    fn multiline_hint_stays_one_row() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::ToolStart { name: "Bash".into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Bash".into(),
            input: json!({"command": "grep -rn foo \\\n  --include='*.rs' .\nls -la"}),
            standalone: false,
        });
        chat.drain_events();
        chat.build_rows(60);
        let hint = chat
            .doc
            .rows
            .iter()
            .find(|r| r.line.plain_text().contains('⎿'))
            .expect("hint row rendered");
        assert!(!hint.line.plain_text().contains('\n'), "hint 单行化");
        assert!(text_width(&hint.line.plain_text()) <= 60, "hint 按宽截断");
    }

    /// 落盘游标按消息边界：宽度变化后重排（行号全变）仍不重复落盘。
    #[test]
    fn flush_cursor_survives_width_change() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::User, "第一条消息"));
        chat.messages.push(msg(Role::Assistant, "回复正文"));
        chat.build_rows(100);
        assert_eq!(chat.doc.settled, chat.doc.rows.len(), "空闲全部定稿");
        assert_eq!(
            settled_segments(&chat), 3,
            "欢迎卡 + 2 条消息 = 3 段"
        );
        chat.advance_flushed();
        assert_eq!(chat.flushed_segments, 3);
        assert_eq!(chat.tail_start, chat.doc.rows.len());

        // 宽度变化重建：已落盘的段不再出现在文档里。
        chat.build_rows(40);
        assert_eq!(chat.tail_start, 0, "重建后尾部从头算");
        assert!(chat.doc.rows.is_empty(), "已落盘内容不重复构建");
        let text: String = chat
            .doc
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect();
        assert!(!text.contains("第一条消息"), "不重复打印");

        // 新消息只构建自己那一段。
        chat.messages.push(msg(Role::User, "第二条"));
        chat.build_rows(40);
        assert!(
            chat.doc.rows.iter().any(|r| r.line.plain_text().contains("第二条")),
            "新消息进入文档"
        );
        assert_eq!(settled_segments(&chat), 1, "只新增 1 段");
    }

    /// 流式（未定稿）内容不落盘：markdown 全量重解析会改写早先的行，
    /// 落进 scrollback 就成了改不掉的中间态。
    #[test]
    fn streaming_content_is_not_flushed_until_settled() {
        let mut chat = test_chat();
        chat.build_rows(80);
        chat.advance_flushed();
        let welcome_segments = chat.flushed_segments;
        assert_eq!(welcome_segments, 1, "欢迎卡是第 0 段");

        chat.handle(UiEvent::TurnStart);
        chat.handle(UiEvent::TextDelta("| a | b |".into()));
        chat.build_rows(80);
        assert_eq!(chat.doc.settled, 0, "流式内容不定稿");
        assert!(!chat.doc.rows.is_empty(), "但仍渲染在动态尾部");
        chat.advance_flushed();
        assert_eq!(chat.flushed_segments, welcome_segments, "游标不动");

        chat.handle(UiEvent::TurnEnd);
        chat.build_rows(80);
        assert_eq!(chat.doc.settled, chat.doc.rows.len(), "回合结束后定稿");
        chat.advance_flushed();
        assert_eq!(chat.flushed_segments, welcome_segments + 1, "消息落盘");
    }

    /// `/clear`（与 `/resume`）整体替换消息集合 → 段编号失效，落盘游标
    /// 必须复位，否则新会话的文档被整段跳过（空白界面）。
    #[test]
    fn clear_resets_flush_cursor() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::User, "hi"));
        chat.build_rows(80);
        chat.advance_flushed();
        assert!(chat.flushed_segments > 0);
        chat.input = "/clear".to_string();
        chat.submit();
        assert_eq!(chat.flushed_segments, 0, "游标复位");
        assert!(chat.dirty, "复位后重建");
        chat.build_rows(80);
        assert!(
            chat.doc.rows.iter().any(|r| r.line.plain_text().contains("bingo")),
            "欢迎卡重新出现"
        );
    }

    /// AskUserQuestion 回答是普通用户消息：进入消息流、按普通消息定稿
    /// 落盘（段数推进），不再是指渲染在输入框上方的瞬态块。
    #[test]
    fn ask_answer_message_flushes_like_normal_message() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::User, "hi"));
        chat.build_rows(80);
        chat.advance_flushed();
        assert_eq!(chat.flushed_segments, 2, "欢迎卡 + 用户输入");

        // 回答一条问题（走真实事件路径）。
        let (tx, _rx) = oneshot::channel();
        let mut request =
            PermissionRequest::new("技术选型", "用哪个库？", vec!["A".into(), "B".into()]);
        request.free_text = true;
        chat.pending_ask = Some((request, tx));
        chat.ask_focus = 0;
        assert!(chat.ask_key(KeyCode::Enter), "Enter 选 A");
        assert!(chat.pending_ask.is_none(), "对话框已关闭");

        // 回答作为一条用户消息进入消息流。
        let answer = chat.messages.last().expect("回答消息已入流");
        assert_eq!(answer.role, Role::User, "回答是用户消息");
        assert!(
            answer.text.contains("User answered the questions:"),
            "{}",
            answer.text
        );
        assert!(answer.text.contains("· 用哪个库？ → A"), "{}", answer.text);
        // 与普通消息一样定稿与落盘：游标按消息段推进。
        chat.build_rows(80);
        assert_eq!(chat.doc.settled, chat.doc.rows.len(), "回答消息已定稿");
        chat.advance_flushed();
        assert_eq!(chat.flushed_segments, 3, "欢迎卡 + hi + 回答消息全部落盘");
    }

    /// 回答消息随会话持久：TurnEnd 不再清除（此前是回合内瞬态块，
    /// 回合结束即消失；现在是消息流的一部分）。
    #[test]
    fn ask_answer_message_persists_across_turn_end() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::User, "hi"));
        let (tx, _rx) = oneshot::channel();
        let mut request =
            PermissionRequest::new("技术选型", "用哪个库？", vec!["A".into(), "B".into()]);
        request.free_text = true;
        chat.pending_ask = Some((request, tx));
        chat.ask_focus = 1;
        assert!(chat.ask_key(KeyCode::Enter), "Enter 选 B");

        chat.handle(UiEvent::TurnEnd);
        let answer = chat.messages.last().expect("回答消息仍在");
        assert_eq!(answer.role, Role::User, "回合结束不清除回答消息");
        assert!(answer.text.contains("· 用哪个库？ → B"), "{}", answer.text);
        chat.build_rows(80);
        let joined: String = chat
            .doc
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("User answered the questions:"),
            "回答仍渲染在消息流: {joined}"
        );
    }

    /// 回合中回答的消息排在流式 assistant 消息之后：顺序守卫——流式
    /// 未结束前回答消息不得定稿（否则落盘会越过流式行把中间态打进
    /// scrollback）；回合结束后两者一并定稿落盘。
    #[test]
    fn ask_answer_after_streaming_message_settles_only_after_turn_end() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::User, "hi"));
        chat.handle(UiEvent::TurnStart);
        chat.handle(UiEvent::TextDelta("思考中…".into()));

        // 回合中回答（模型提问 → 用户回答，模型仍在流式）。
        let (tx, _rx) = oneshot::channel();
        let mut request = PermissionRequest::new("技术选型", "用哪个库？", vec!["A".into()]);
        request.free_text = true;
        chat.pending_ask = Some((request, tx));
        chat.ask_focus = 0;
        assert!(chat.ask_key(KeyCode::Enter), "选 A");
        assert_eq!(chat.messages.len(), 3, "hi + 流式 assistant + 回答");

        // 流式未结束：回答消息与流式消息都不定稿，定稿停在第一条用户消息。
        chat.build_rows(80);
        assert!(chat.message_settled(0), "前置用户消息已定稿");
        assert!(!chat.message_settled(1), "流式消息不定稿");
        assert!(!chat.message_settled(2), "回答消息在流式结束前不定稿");
        assert_eq!(chat.doc.settled_marks.len(), 2, "欢迎卡 + 第一条用户消息");

        // 回合结束：全部定稿并落盘（含回答消息，顺序正确）。
        chat.handle(UiEvent::TurnEnd);
        chat.build_rows(80);
        assert_eq!(chat.doc.settled, chat.doc.rows.len(), "回合结束后全部定稿");
        chat.advance_flushed();
        assert_eq!(chat.flushed_segments, 4, "欢迎卡 + hi + 流式 + 回答全部落盘");
    }

    /// 错误路径不经过 TurnEnd（start_turn 的 `Err(e)` 只发 UiEvent::Error）：
    /// 回答消息仍在消息流中——旧瞬态块在错误路径下无人清理、悬挂到
    /// /clear（24ba4d9 前旧 bug 的回归路径）；普通消息无状态可清，天然修复。
    #[test]
    fn ask_answer_message_survives_error_path() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::User, "hi"));
        let (tx, _rx) = oneshot::channel();
        let mut request =
            PermissionRequest::new("技术选型", "用哪个库？", vec!["A".into(), "B".into()]);
        request.free_text = true;
        chat.pending_ask = Some((request, tx));
        chat.ask_focus = 0;
        assert!(chat.ask_key(KeyCode::Enter), "选 A");

        chat.handle(UiEvent::Error {
            code: "SERVER_ERROR",
            msg: "回合失败".to_string(),
            level: crate::error::ErrorLevel::Full,
            context: crate::error::ErrorContext::LongTurn,
        });
        // 回答消息仍在消息流中且照常渲染。
        let answer = chat.messages.last().expect("回答消息仍在");
        assert_eq!(answer.role, Role::User);
        assert!(answer.text.contains("· 用哪个库？ → A"), "{}", answer.text);
        chat.build_rows(80);
        let joined: String = chat
            .doc
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("User answered the questions:"),
            "错误后回答仍渲染: {joined}"
        );
    }

    /// 模拟 inline 组件的落盘循环：重建 → 落盘定稿前缀 → 推进游标。
    fn flush_frame(chat: &mut Chat, width: usize, printed: &mut Vec<String>) {
        chat.build_rows(width);
        if chat.doc.settled > chat.tail_start {
            for row in &chat.doc.rows[chat.tail_start..chat.doc.settled] {
                printed.push(row.line.plain_text());
            }
            chat.advance_flushed();
        }
    }

    /// 全流程回归：流式 + 中途 resize + 定稿，scrollback 里任何一行都
    /// 不重复（旧实现的行号游标在 resize 重排后会重打一遍）。
    #[test]
    fn streaming_with_resize_never_prints_a_row_twice() {
        let mut chat = test_chat();
        let mut printed = Vec::new();
        flush_frame(&mut chat, 100, &mut printed);
        let welcome = printed.len();
        assert!(welcome > 0, "欢迎卡落盘");

        chat.messages.push(msg(Role::User, "请解释一下这段代码"));
        flush_frame(&mut chat, 100, &mut printed);
        chat.handle(UiEvent::TurnStart);
        for chunk in ["第一段文字。\n\n", "## 标题\n\n", "- 列表项一\n", "- 列表项二\n"] {
            chat.handle(UiEvent::TextDelta(chunk.into()));
            flush_frame(&mut chat, 100, &mut printed);
        }
        // 回合中途 resize：重排后行号全变。
        flush_frame(&mut chat, 60, &mut printed);
        chat.handle(UiEvent::TextDelta("结尾。".into()));
        chat.handle(UiEvent::TurnEnd);
        flush_frame(&mut chat, 60, &mut printed);
        // 空转几帧不应再打印任何东西。
        let after = printed.len();
        for _ in 0..3 {
            flush_frame(&mut chat, 60, &mut printed);
        }
        assert_eq!(printed.len(), after, "无新增落盘");

        // 欢迎卡自身含多行相同的分栏留白，按内容去重会误报——只查消息部分。
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for line in &printed[welcome..] {
            if line.trim().is_empty() {
                continue;
            }
            *seen.entry(line.as_str()).or_default() += 1;
        }
        for (line, count) in &seen {
            assert_eq!(*count, 1, "行重复落盘 {count} 次: {line:?}");
        }
        let joined = printed.join("\n");
        assert!(joined.contains("请解释一下这段代码"), "用户消息落盘");
        assert!(joined.contains("结尾。"), "定稿正文落盘");
        assert!(chat.doc.rows.is_empty(), "全部落盘后尾部为空");
    }

    /// inline ctrl+o 重放：无新信息时 no-op；有已落盘内容或可展开项时
    /// 展开一切、游标回卷并请求整卷冻结。
    #[test]
    fn expand_transcript_rewinds_and_expands_everything() {
        let mut chat = test_chat();
        // 空会话且全部在屏 → no-op（重放不增加信息）。
        assert!(!chat.expand_transcript());
        assert!(!chat.dump_transcript);
        assert!(!chat.force_redraw);

        // 消息落盘后 → 重放：游标回卷，重建的文档重含全部段；
        // 先清屏再写（置顶，与 resize 同款）。
        chat.messages.push(msg(Role::Assistant, "回复"));
        chat.build_rows(80);
        chat.advance_flushed();
        chat.build_rows(80);
        assert!(chat.doc.rows.is_empty(), "全部落盘后尾部为空");
        assert!(chat.expand_transcript());
        assert!(chat.dump_transcript);
        assert!(chat.force_redraw, "重放帧先清可见屏");
        chat.build_rows(80);
        let text: String = chat
            .doc
            .rows
            .iter()
            .map(|row| row.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("回复"), "重放文档含已落盘消息: {text}");

        // 有折叠组的历史消息 → 重放前全部展开。
        chat.dump_transcript = false;
        start_group(&mut chat);
        let _ = chat.events.send(UiEvent::ToolDone(crate::query::ToolCallDone {
            name: "Read".into(),
            summary: "Read a.md".into(),
            output: "l1\nl2\nl3".into(),
            is_error: false,
            duration_ms: 0,
            diff: None,
        }));
        chat.drain_events();
        assert!(chat.expand_transcript());
        assert!(chat.dump_transcript);
        assert!(
            chat.messages
                .iter()
                .flat_map(|m| &m.groups)
                .all(|g| g.expanded || g.activities.is_empty()),
            "全部折叠组已展开"
        );

        // 全展开态 → 第二按走闭合方向：折回聚合态（展示层由 app 层
        // 走清屏重画 + 回灌收拢）。
        assert!(chat.transcript_fully_expanded());
        assert!(chat.collapse_transcript());
        assert!(
            chat.messages.iter().flat_map(|m| &m.groups).all(|g| !g.expanded),
            "折叠组全部闭合"
        );
        assert!(!chat.transcript_fully_expanded(), "闭合后回到展开方向");
        assert!(!chat.collapse_transcript(), "已全闭合，再闭合无变化");
    }

    /// 空闲时 tick 不置 dirty（不重建文档）；有动态元素时才置位。
    #[test]
    fn tick_marks_dirty_only_when_dynamic() {
        let mut chat = test_chat();
        chat.dirty = false;
        chat.tick();
        assert!(!chat.dirty, "空闲不重建");
        assert!(!chat.needs_tick(), "空闲不唤醒组件");
        chat.busy = true;
        chat.tick();
        assert!(chat.dirty, "busy 时重建（spinner/耗时行）");
        assert!(chat.needs_tick());
        // 待处理事件也要唤醒（否则事件永远排不出去）。
        chat.busy = false;
        chat.dirty = false;
        let _ = chat.events.send(UiEvent::Warning("w".into()));
        assert!(chat.needs_tick(), "有待处理事件需唤醒");
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
            joined.contains("enter to select · ↑/↓ to navigate · esc to cancel"),
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
        assert!(joined.contains("enter to submit · esc to cancel"), "input hint: {joined}");
        for c in ['s', 'e', 'r', 'd', 'e'] {
            assert!(chat.ask_key(KeyCode::Char(c)), "type {c}");
        }
        assert!(chat.ask_key(KeyCode::Enter), "submit");
        assert!(chat.pending_ask.is_none(), "dialog closed");
        assert_eq!(rx.try_recv(), Ok(DialogAction::Answer("serde".to_string())));
        // 回答进入消息流：一条普通用户消息（Q&A 回显）。
        let answer = chat.messages.last().expect("回答消息已入流");
        assert_eq!(answer.role, Role::User);
        assert_eq!(
            answer.text,
            "User answered the questions:\n  · 用哪个库？ → serde"
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
            joined.contains("User answered the questions:"),
            "result header: {joined}"
        );
        assert!(
            joined.contains("· 用哪个库？ → serde"),
            "result line: {joined}"
        );
        assert!(joined.contains("❯ "), "回答以用户气泡渲染: {joined}");
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
        // 拒绝同样进入消息流（一条普通用户消息）。
        let declined = chat.messages.last().expect("拒绝消息已入流");
        assert_eq!(declined.role, Role::User);
        assert_eq!(declined.text, ASK_DECLINED_TEXT);
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
        let answer = chat.messages.last().expect("回答消息已入流");
        assert_eq!(answer.role, Role::User);
        assert!(
            answer.text.contains("· 用哪个库？ → B"),
            "选项文本作为回答: {}",
            answer.text
        );
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
        let chat = test_chat();
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
        assert!(chat.warnings.iter().any(|(_, w)| w.contains("a.png")), "警告提示");
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

    /// 图片仍在加载时消息不定稿——否则 `#[image]` 回落行会被落盘进
    /// scrollback，而 kitty 序列只在落盘那一刻输出，图片永远出不来。
    #[test]
    fn message_waits_for_pending_images_before_settling() {
        let mut chat = test_chat();
        chat.image_cap = Some(ImageCap::default_cells());
        let url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(tiny_png())
        );
        chat.messages.push(msg(Role::Assistant, &format!("![图]({url})")));
        // 加载在途（load_message_images 的效果）。
        chat.images_pending.insert(url.clone());
        chat.build_rows(100);
        assert_eq!(
            settled_segments(&chat), 1,
            "只有欢迎卡定稿，含在途图片的消息不定稿"
        );

        // 加载成功 → 消息定稿，且落盘行携带 ImageRef（块首行输出 kitty 序列）。
        let meta = ImageMeta { cols: 4, rows: 2, bytes: tiny_png() };
        chat.handle(UiEvent::ImageReady { url: url.clone(), meta: Some(meta) });
        chat.build_rows(100);
        assert_eq!(settled_segments(&chat), 2, "图片就绪后消息定稿");
        let image_rows: Vec<&Row> = chat
            .doc
            .rows
            .iter()
            .take(chat.doc.settled)
            .filter(|r| r.line.image.is_some())
            .collect();
        assert!(!image_rows.is_empty(), "定稿行里有图片块");
    }

    /// 加载失败（含超时回报的 None）同样解除阻塞：以 `#[image]` 占位定稿。
    #[test]
    fn failed_image_load_settles_with_placeholder() {
        let mut chat = test_chat();
        chat.image_cap = Some(ImageCap::default_cells());
        chat.messages.push(msg(Role::Assistant, "![图](missing.png)"));
        chat.images_pending.insert("missing.png".to_string());
        chat.build_rows(100);
        assert_eq!(settled_segments(&chat), 1, "在途时不定稿");
        chat.handle(UiEvent::ImageReady {
            url: "missing.png".to_string(),
            meta: None,
        });
        chat.build_rows(100);
        assert_eq!(settled_segments(&chat), 2, "失败后照常定稿");
        let text: String = chat
            .doc
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("#[image]"), "占位文本落稿: {text}");
    }

    /// 无图片能力时不进在途集合，消息照常立即定稿（行为不变）。
    #[test]
    fn without_image_capability_messages_settle_immediately() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, "![图](a.png)"));
        chat.build_rows(100);
        assert!(chat.images_pending.is_empty());
        assert_eq!(settled_segments(&chat), 2, "无能力不等图片");
    }

    // ---- 交互（CC 手感）：光标编辑 / 历史 / 多行 / 双击语义 / 排队 ----

    /// 独立 home 的 Chat：历史文件按 home 分家，测试之间互不串。
    fn chat_with_history(tag: &str) -> Chat {
        let home = std::env::temp_dir().join(format!("bingo-chat-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        test_chat_home(home)
    }

    thread_local! {
        static KEY_TICK: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }

    /// 测试按键时钟：每次按键推进 50ms——远大于粘贴突发阈值，于是
    /// "测试里连打" 不会被粘贴启发式误判（真实打字同理）。
    fn key_time() -> std::time::Instant {
        let n = KEY_TICK.with(|c| {
            let v = c.get() + 1;
            c.set(v);
            v
        });
        std::time::Instant::now() + std::time::Duration::from_millis(50 * n)
    }

    fn press(chat: &mut Chat, code: KeyCode) -> bool {
        chat.on_key_at(code, KeyModifiers::empty(), key_time())
    }

    fn ctrl(chat: &mut Chat, c: char) -> bool {
        chat.on_key_at(KeyCode::Char(c), KeyModifiers::CONTROL, key_time())
    }

    fn type_text(chat: &mut Chat, text: &str) {
        for c in text.chars() {
            press(chat, KeyCode::Char(c));
        }
    }

    fn alt(chat: &mut Chat, c: char) -> bool {
        chat.on_key_at(KeyCode::Char(c), KeyModifiers::ALT, key_time())
    }

    /// 光标编辑：←/→ 移动、ctrl+a/e 行首行尾、alt+b/f 词间、
    /// 插入落在光标处而非行尾。
    #[test]
    fn cursor_moves_and_inserts_at_position() {
        let mut chat = chat_with_history("cursor");
        type_text(&mut chat, "hello world");
        assert_eq!(chat.cursor, chat.input.len());
        assert!(ctrl(&mut chat, 'a'));
        assert_eq!(chat.cursor, 0, "ctrl+a 行首");
        assert!(press(&mut chat, KeyCode::Right));
        press(&mut chat, KeyCode::Char('i'));
        assert_eq!(chat.input, "hiello world", "插入在光标处");
        assert!(ctrl(&mut chat, 'e'));
        assert_eq!(chat.cursor, chat.input.len(), "ctrl+e 行尾");
        assert!(alt(&mut chat, 'b'));
        assert_eq!(chat.cursor, "hiello ".len(), "alt+b 退一个词");
        assert!(alt(&mut chat, 'f'));
        assert_eq!(chat.cursor, chat.input.len(), "alt+f 前进一个词");
        // CJK 按字符移动、按显示宽渲染。
        chat.set_input("中文");
        press(&mut chat, KeyCode::Left);
        assert_eq!(chat.cursor, 3, "一次退一个汉字（3 字节）");
    }

    /// ctrl+k/u/w 删除进 kill 缓冲，ctrl+y 粘回；ctrl+d 删光标后字符。
    #[test]
    fn kill_ring_round_trip() {
        let mut chat = chat_with_history("kill");
        type_text(&mut chat, "alpha beta");
        assert!(ctrl(&mut chat, 'w'));
        assert_eq!(chat.input, "alpha ");
        assert!(ctrl(&mut chat, 'y'));
        assert_eq!(chat.input, "alpha beta", "ctrl+y 粘回");
        assert!(ctrl(&mut chat, 'a'));
        assert!(ctrl(&mut chat, 'k'));
        assert_eq!(chat.input, "", "ctrl+k 删到行尾");
        assert!(ctrl(&mut chat, 'y'));
        assert_eq!(chat.input, "alpha beta");
        assert!(ctrl(&mut chat, 'u'));
        assert_eq!(chat.input, "", "ctrl+u 删到行首");
        chat.set_input("abc");
        chat.cursor = 1;
        assert!(ctrl(&mut chat, 'd'));
        assert_eq!(chat.input, "ac", "ctrl+d 删光标后字符");
    }

    /// 历史：提交入历史并落盘；↑/↓ 切换，回到底部恢复 draft；
    /// 连续相同 prompt 只记一条。
    #[test]
    fn prompt_history_persists_and_navigates() {
        let mut chat = chat_with_history("history");
        chat.record_history("first");
        chat.record_history("second");
        chat.record_history("second");
        assert_eq!(chat.history.entries(), ["first", "second"], "连续重复只记一条");
        // 落盘：同一 home + cwd 的新会话读得到。
        let reloaded = crate::tui::history::load(
            &chat.session.home,
            std::path::Path::new(&chat.cwd),
        );
        assert_eq!(reloaded, vec!["first".to_string(), "second".to_string()]);

        chat.set_input("draft");
        press(&mut chat, KeyCode::Up);
        assert_eq!(chat.input, "second");
        press(&mut chat, KeyCode::Up);
        assert_eq!(chat.input, "first");
        press(&mut chat, KeyCode::Down);
        assert_eq!(chat.input, "second");
        press(&mut chat, KeyCode::Down);
        assert_eq!(chat.input, "draft", "回到底部恢复 draft");
        let _ = std::fs::remove_dir_all(&chat.session.home);
    }

    /// 多行输入：`\`+Enter 与 ctrl+j 插入换行，Enter 提交整体；
    /// 渲染为多行（每行 height=1，不靠单行塞 \n）。
    #[test]
    fn multiline_input_renders_as_multiple_rows() {
        let mut chat = chat_with_history("multiline");
        chat.width = 80;
        type_text(&mut chat, "first\\");
        assert!(press(&mut chat, KeyCode::Enter), "\\+Enter 换行");
        type_text(&mut chat, "second");
        assert!(ctrl(&mut chat, 'j'), "ctrl+j 换行");
        type_text(&mut chat, "third");
        assert_eq!(chat.input, "first\nsecond\nthird");
        let rows = chat.prompt_lines();
        assert_eq!(rows.len(), 3, "三行输入 = 三个 Row");
        for row in &rows {
            assert!(!row.plain_text().contains('\n'), "行内不含换行");
        }
        assert!(rows[2].plain_text().contains('▋'), "光标画在末行");
        // ↑ 在多行里先走视觉行，不切历史。
        chat.record_history("older");
        press(&mut chat, KeyCode::Up);
        assert_eq!(chat.input, "first\nsecond\nthird", "行内移动不动文本");
        press(&mut chat, KeyCode::Up);
        press(&mut chat, KeyCode::Up);
        assert_eq!(chat.input, "older", "到首行才切历史");
        let _ = std::fs::remove_dir_all(&chat.session.home);
    }

    /// 输入区行数有上限：长输入只显示光标所在的一屏。
    #[test]
    fn prompt_rows_are_capped() {
        let mut chat = chat_with_history("caprows");
        chat.width = 40;
        chat.set_input((0..30).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n"));
        assert_eq!(chat.prompt_lines().len(), INPUT_ROWS_MAX);
    }

    /// Ctrl+C（CC 语义）：busy 中断；有文本先清空（进历史）；
    /// 空输入第一次提示，窗口内第二次退出，超时重新计数。
    #[test]
    fn ctrl_c_interrupt_clear_then_exit() {
        let mut chat = chat_with_history("ctrlc");
        let t0 = std::time::Instant::now();
        chat.busy = true;
        chat.on_key_at(KeyCode::Char('c'), KeyModifiers::CONTROL, t0);
        assert!(chat.interrupted, "busy → 中断");
        assert!(!chat.exit);

        chat.busy = false;
        chat.set_input("draft");
        chat.on_key_at(KeyCode::Char('c'), KeyModifiers::CONTROL, t0);
        assert_eq!(chat.input, "", "有文本先清空");
        assert!(!chat.exit, "清空不退出");
        assert_eq!(chat.history.entries().last().map(String::as_str), Some("draft"));

        chat.on_key_at(KeyCode::Char('c'), KeyModifiers::CONTROL, t0);
        assert_eq!(chat.notice, Some("Press ctrl-c again to exit"));
        assert!(!chat.exit, "第一次只提示");
        chat.on_key_at(KeyCode::Char('c'), KeyModifiers::CONTROL, t0 + CTRL_C_WINDOW);
        assert!(chat.exit, "窗口内第二次退出");

        // 超窗后重新计数。
        let mut chat = chat_with_history("ctrlc2");
        chat.on_key_at(KeyCode::Char('c'), KeyModifiers::CONTROL, t0);
        chat.on_key_at(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            t0 + CTRL_C_WINDOW + std::time::Duration::from_millis(1),
        );
        assert!(!chat.exit, "超窗不退出，只重新提示");
        assert_eq!(chat.notice, Some("Press ctrl-c again to exit"));
        let _ = std::fs::remove_dir_all(&chat.session.home);
    }

    /// Esc：busy 中断；建议/面板逐层关闭；有文本双击清空并存历史。
    #[test]
    fn esc_closes_layers_then_clears_input() {
        let mut chat = chat_with_history("esc");
        let t0 = std::time::Instant::now();
        chat.busy = true;
        chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0);
        assert!(chat.interrupted, "busy → 中断");

        chat.busy = false;
        chat.set_input("/");
        assert!(!chat.slash_suggestions.is_empty());
        chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0);
        assert!(chat.slash_suggestions.is_empty(), "先关下拉");
        assert_eq!(chat.input, "/", "输入还在");

        chat.set_input("hello");
        chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0);
        assert_eq!(chat.input, "hello", "第一次只预备");
        assert_eq!(chat.notice, Some("Press esc again to clear"));
        chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0);
        assert_eq!(chat.input, "", "双击清空");
        assert_eq!(chat.history.entries().last().map(String::as_str), Some("hello"));
        let _ = std::fs::remove_dir_all(&chat.session.home);
    }

    /// Shift+Tab 循环权限模式，且真正作用于下一回合的 Session。
    #[test]
    fn shift_tab_cycles_permission_mode() {
        let mut chat = chat_with_history("mode");
        assert_eq!(chat.permission_mode, PermissionMode::Default);
        press(&mut chat, KeyCode::BackTab);
        assert_eq!(chat.permission_mode, PermissionMode::AcceptEdits);
        assert_eq!(chat.permission_mode_label(), "acceptEdits", "footer 徽标同源");
        press(&mut chat, KeyCode::BackTab);
        assert_eq!(chat.permission_mode, PermissionMode::Plan);
        press(&mut chat, KeyCode::BackTab);
        assert_eq!(chat.permission_mode, PermissionMode::Default, "循环回默认");
        // 回合用的 Session 带当前模式（Session 在 Arc 里不可变 → 派生副本）。
        press(&mut chat, KeyCode::BackTab);
        assert_eq!(chat.session_for_turn().permission_mode, PermissionMode::AcceptEdits);
        assert_eq!(chat.session.permission_mode, PermissionMode::Default, "原 Session 不变");

        // 启动即 bypass 的会话只在 bypass ↔ default 之间切（不引入新危险模式）。
        let mut chat = chat_with_history("mode-bypass");
        chat.permission_mode = PermissionMode::BypassPermissions;
        let mut session = (*chat.session).clone();
        session.permission_mode = PermissionMode::BypassPermissions;
        chat.session = Arc::new(session);
        press(&mut chat, KeyCode::BackTab);
        assert_eq!(chat.permission_mode, PermissionMode::Default);
        press(&mut chat, KeyCode::BackTab);
        assert_eq!(chat.permission_mode, PermissionMode::BypassPermissions);
    }

    /// busy 时 Enter 不再无效：消息入队、显示在输入框下方，↑ 取回最后一条。
    #[test]
    fn messages_queue_while_busy() {
        let mut chat = chat_with_history("queue");
        chat.busy = true;
        chat.set_input("first queued");
        chat.submit();
        assert_eq!(chat.queued, vec!["first queued".to_string()]);
        assert_eq!(chat.input, "", "入队后输入清空");
        chat.set_input("second queued");
        chat.submit();
        assert_eq!(chat.queued.len(), 2);
        let lines = chat.queue_lines();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("> first queued"), "{lines:?}");
        // busy 时 ↑ 取回最后一条排队消息继续编辑。
        press(&mut chat, KeyCode::Up);
        assert_eq!(chat.input, "second queued");
        assert_eq!(chat.queued.len(), 1);
    }

    /// 底部实体区：ctrl+g 聚焦选择，↑↓ 移动、Enter 打开、Esc 关闭；
    /// 收起态一行摘要，无实体时不占行且 ctrl+g 给提示。
    #[test]
    fn entity_selector_picks_agent_and_channel() {
        let mut chat = test_chat();
        chat.width = 80;
        // 无实体：不占行，ctrl+g 提示。
        assert!(chat.entity_rows(80).is_empty());
        assert!(chat.on_key(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert!(chat.notice.is_some(), "空态提示");
        assert!(chat.entity_focus.is_none());
        // 造一个 agent 实例 + 一个频道。
        chat.session
            .agents
            .insert("scout", None, "调研".into(), chat.session.clone());
        chat.session
            .channels
            .create("table", vec![], crate::channels::ChannelMode::Serial)
            .unwrap_or_else(|e| panic!("{e}"));
        chat.refresh_entities();
        assert_eq!(chat.entities.len(), 2);
        // 收起态：一行摘要含两者。
        let rows = chat.entity_rows(80);
        assert_eq!(rows.len(), 1);
        let summary = rows[0].plain_text();
        assert!(
            summary.contains("◉ scout(running)") && summary.contains("◇ #table(0)"),
            "{summary}"
        );
        // 聚焦：逐行 + ❯ 选中 + 提示行。
        assert!(chat.on_key(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert_eq!(chat.entity_focus, Some(0));
        let rows = chat.entity_rows(80);
        let joined: Vec<String> = rows.iter().map(|l| l.plain_text()).collect();
        assert!(joined[0].starts_with("❯ ◉ scout"), "{joined:?}");
        assert!(joined.last().unwrap_or(&String::new()).contains("enter 打开"));
        // ↓ 到频道，Enter 打开。
        assert!(chat.on_key(KeyCode::Down, KeyModifiers::empty()));
        assert!(chat.on_key(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(
            chat.open_entity,
            Some(EntityOpen::Channel("table".into())),
            "选中频道"
        );
        assert!(chat.entity_focus.is_none(), "打开后退出聚焦");
        // 再次聚焦后 Esc 只关选择器（不触发全局 Esc 语义）。
        let _ = chat.on_key(KeyCode::Char('g'), KeyModifiers::CONTROL);
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(chat.entity_focus.is_none());
    }

    /// 队列超出上限时折叠为一行（行数进 chrome，必须有上界）。
    #[test]
    fn queue_lines_are_capped() {
        let mut chat = chat_with_history("queuecap");
        chat.queued = (0..10).map(|i| format!("m{i}")).collect();
        assert_eq!(chat.queue_lines().len(), QUEUE_ROWS_MAX + 1);
        assert!(chat.queue_lines().last().is_some_and(|l| l.contains("more queued")));
    }

    /// `?`：空输入开关面板；有文本时是普通字符。
    #[test]
    fn question_mark_toggles_help_panel() {
        let mut chat = chat_with_history("help");
        chat.width = 100;
        chat.height = 40;
        press(&mut chat, KeyCode::Char('?'));
        assert!(chat.help_visible);
        assert!(!chat.help_lines().is_empty(), "面板有内容");
        assert!(chat.input.is_empty(), "? 不入输入");
        press(&mut chat, KeyCode::Char('?'));
        assert!(!chat.help_visible, "再按关闭");
        assert!(chat.help_lines().is_empty());
        type_text(&mut chat, "why");
        press(&mut chat, KeyCode::Char('?'));
        assert_eq!(chat.input, "why?", "有文本时是普通字符");
        assert!(!chat.help_visible);
    }

    /// 帮助面板行数受终端高度约束（canvas 不得超过终端高度）。
    #[test]
    fn help_panel_shrinks_on_short_terminals() {
        let mut chat = chat_with_history("helpshort");
        chat.width = 100;
        chat.help_visible = true;
        chat.height = 40;
        let tall = chat.help_lines().len();
        chat.height = 14;
        let short = chat.help_lines().len();
        assert!(short < tall, "矮终端面板更短: {short} vs {tall}");
        assert!(short + 9 <= 14, "面板 + 其余 chrome 不超过终端高度");
        chat.height = 6;
        assert!(chat.help_lines().is_empty(), "极矮终端不显示面板");
    }

    /// ctrl+s 暂存/恢复（含光标位）、ctrl+_ 撤销、ctrl+t 任务区、ctrl+l 重画。
    #[test]
    fn stash_undo_tasks_and_redraw() {
        let mut chat = chat_with_history("t2");
        type_text(&mut chat, "stashed");
        chat.cursor = 3;
        assert!(ctrl(&mut chat, 's'));
        assert_eq!(chat.input, "", "ctrl+s 暂存并清空");
        assert!(ctrl(&mut chat, 's'));
        assert_eq!((chat.input.as_str(), chat.cursor), ("stashed", 3), "恢复含光标");

        // 撤销：整体编辑（kill）回退一步。
        chat.set_input("undo me");
        chat.cursor = chat.input.len();
        assert!(ctrl(&mut chat, 'w'));
        assert_eq!(chat.input, "undo ");
        assert!(ctrl(&mut chat, '7'), "ctrl+_ 到达时是 ctrl+7");
        assert_eq!(chat.input, "undo me", "撤销回到删除前");

        assert!(!chat.tasks_visible);
        assert!(ctrl(&mut chat, 't'));
        assert!(chat.tasks_visible, "ctrl+t 显示任务区");
        assert!(ctrl(&mut chat, 't'));
        assert!(!chat.tasks_visible);

        assert!(ctrl(&mut chat, 'l'));
        assert!(chat.force_redraw, "ctrl+l 请求整屏重画");
    }

    /// bash 模式：空输入 Esc/退格/ctrl+u 退出；Tab 从会话内 `!` 历史补全。
    #[test]
    fn bash_mode_exits_and_completes() {
        let mut chat = chat_with_history("bash");
        chat.bash_history.push("cargo test --all".to_string());
        press(&mut chat, KeyCode::Char('!'));
        assert!(chat.bash_mode);
        press(&mut chat, KeyCode::Esc);
        assert!(!chat.bash_mode, "空输入 Esc 退出 shell 模式");
        press(&mut chat, KeyCode::Char('!'));
        assert!(ctrl(&mut chat, 'u'));
        assert!(!chat.bash_mode, "空输入 ctrl+u 退出");
        press(&mut chat, KeyCode::Char('!'));
        type_text(&mut chat, "cargo");
        press(&mut chat, KeyCode::Tab);
        assert_eq!(chat.input, "cargo test --all", "Tab 前缀补全");
    }

    /// 粘贴突发：突发中的 Enter 是换行而不是发送；≥10 行折叠为占位符，
    /// 提交时展开真实内容。
    #[test]
    fn paste_burst_inserts_newlines_and_collapses() {
        let mut chat = chat_with_history("paste");
        let mut now = std::time::Instant::now();
        let fast = std::time::Duration::from_millis(1);
        // 逐字符“粘贴” 12 行。
        for i in 0..12 {
            for c in format!("line{i}").chars() {
                now += fast;
                chat.on_key_at(KeyCode::Char(c), KeyModifiers::empty(), now);
            }
            now += fast;
            chat.on_key_at(KeyCode::Enter, KeyModifiers::empty(), now);
        }
        assert!(!chat.busy, "粘贴中的 Enter 不发送");
        assert!(chat.input.starts_with("[Pasted text #1 +"), "占位符: {}", chat.input);
        assert_eq!(chat.pastes.len(), 1);
        assert!(chat.expand_pastes(&chat.input).contains("line11"), "提交时展开真实内容");

        // 正常打字（间隔大）时 Enter 照常提交，不再变成换行。
        let mut chat = chat_with_history("paste2");
        chat.busy = true; // 走排队路径：不需要 tokio runtime
        let slow = std::time::Duration::from_millis(50);
        let mut now = std::time::Instant::now();
        for c in "hi".chars() {
            now += slow;
            chat.on_key_at(KeyCode::Char(c), KeyModifiers::empty(), now);
        }
        now += slow;
        chat.on_key_at(KeyCode::Enter, KeyModifiers::empty(), now);
        assert_eq!(chat.input, "", "Enter 提交而不是换行");
        assert_eq!(chat.queued, vec!["hi".to_string()]);
    }

    /// bracketed paste：整段插到光标处、只占一步撤销；≥10 行折叠为占位符，
    /// 提交时展开真实内容。CR 换行（终端粘贴用的就是 CR）先归一。
    #[test]
    fn bracketed_paste_inserts_and_collapses() {
        let mut chat = chat_with_history("paste-real");
        chat.set_input("ab");
        chat.cursor = 1;
        chat.on_paste("X");
        assert_eq!(chat.input, "aXb", "插在光标处");
        assert_eq!(chat.cursor, 2);
        chat.undo_edit();
        assert_eq!(chat.input, "ab", "一次粘贴 = 一步撤销");

        // 短段不折叠（阈值以下）。
        let mut chat = chat_with_history("paste-short");
        chat.on_paste("line1\nline2");
        assert_eq!(chat.input, "line1\nline2");
        assert!(chat.pastes.is_empty(), "未到阈值不折叠");

        // ≥ PASTE_COLLAPSE_LINES 行折叠；CR 与 CRLF 都算换行。
        let mut chat = chat_with_history("paste-fold");
        let body: String = (0..PASTE_COLLAPSE_LINES)
            .map(|i| format!("line{i}\r"))
            .collect();
        chat.on_paste(&body);
        assert!(
            chat.input.starts_with("[Pasted text #1 +"),
            "占位符: {}",
            chat.input
        );
        assert_eq!(chat.cursor, chat.input.len());
        assert!(
            chat.expand_pastes(&chat.input).contains("line9"),
            "提交时展开真实内容"
        );
        assert!(!chat.expand_pastes(&chat.input).contains('\r'), "CR 已归一");

        // 空粘贴什么也不做（不写撤销栈）。
        let mut chat = chat_with_history("paste-empty");
        chat.on_paste("");
        assert!(chat.input.is_empty());
        assert!(chat.undo.is_empty());
    }

    /// 生成一张测试 PNG，返回路径。
    fn test_png_path(dir: &std::path::Path, name: &str, w: u32, h: u32) -> std::path::PathBuf {
        let path = dir.join(name);
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([255u8, 0, 0, 255]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(
                &mut std::fs::File::create(&path).unwrap(),
                image::ImageFormat::Png,
            )
            .unwrap();
        path
    }

    /// 提交时独立成行的图片路径 → 注册附件 + `#[image N]` 占位（文本保留）。
    #[test]
    fn image_path_line_becomes_marker_on_submit() {
        let mut chat = chat_with_history("img-path");
        let dir = std::env::temp_dir().join(format!("bingo-img-dir-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = test_png_path(&dir, "a.png", 8, 8);
        chat.set_input(format!("看一下这张图\n{}", png.display()));
        chat.busy = true; // 走排队路径：不需要 tokio runtime
        chat.submit();
        assert_eq!(chat.queued.len(), 1);
        assert_eq!(
            chat.queued[0],
            format!("看一下这张图\n#[image 1]"),
            "路径行替换为占位：{}",
            chat.queued[0]
        );
        assert_eq!(chat.attachments.len(), 1);
        assert_eq!(chat.attachments[0].media_type, "image/png");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `![alt](path)` 整行同样识别；非图片路径/不存在的文件原样保留。
    #[test]
    fn markdown_image_syntax_and_non_image_lines() {
        let mut chat = chat_with_history("img-md");
        let dir = std::env::temp_dir().join(format!("bingo-img-md-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = test_png_path(&dir, "b.png", 4, 4);
        let txt = dir.join("note.txt");
        std::fs::write(&txt, "hi").unwrap();
        chat.set_input(format!("![图]({})\n{}", png.display(), txt.display()));
        chat.busy = true;
        chat.submit();
        assert_eq!(chat.queued[0], format!("#[image 1]\n{}", txt.display()));
        assert_eq!(chat.attachments.len(), 1, "txt 不注册");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// resolve_images：按占位序号取附件（去重、越界忽略）。
    #[test]
    fn resolve_images_extracts_attachments_in_order() {
        let mut chat = chat_with_history("img-resolve");
        let dir = std::env::temp_dir().join(format!("bingo-img-rs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = test_png_path(&dir, "a.png", 4, 4);
        let b = test_png_path(&dir, "b.png", 6, 6);
        let id1 = chat.register_image_file(&a).unwrap();
        let id2 = chat.register_image_file(&b).unwrap();
        let text = format!("看 #[image {id1}] 和 #[image {id2}] 再看 #[image {id1}] 和 #[image 99]");
        let imgs = chat.resolve_images(&text);
        assert_eq!(imgs.len(), 2, "去重 + 越界忽略");
        assert_eq!(imgs[0].data, chat.attachments[id1 - 1].data);
        assert_eq!(imgs[1].data, chat.attachments[id2 - 1].data);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ctrl+r 反向搜索：过滤命中、再按取更旧、Tab 采纳继续编辑、
    /// ctrl+c 取消还原。
    #[test]
    fn reverse_search_walks_history() {
        let mut chat = chat_with_history("search");
        for entry in ["cargo test", "git status", "cargo build"] {
            chat.record_history(entry);
        }
        chat.set_input("keep");
        assert!(ctrl(&mut chat, 'r'));
        assert!(chat.search.is_some(), "进入搜索态");
        assert_eq!(chat.search_line().as_deref(), Some("(reverse-i-search)`': cargo build"));
        type_text(&mut chat, "cargo");
        assert_eq!(
            chat.search.as_ref().and_then(|s| s.hit.clone()).as_deref(),
            Some("cargo build")
        );
        assert!(ctrl(&mut chat, 'r'), "再按取更旧命中");
        assert_eq!(
            chat.search.as_ref().and_then(|s| s.hit.clone()).as_deref(),
            Some("cargo test")
        );
        // 搜索态的输入行显示命中。
        assert_eq!(chat.prompt_lines()[0].plain_text(), "cargo test");
        press(&mut chat, KeyCode::Tab);
        assert!(chat.search.is_none(), "Tab 采纳并退出搜索");
        assert_eq!(chat.input, "cargo test");

        // ctrl+c 取消：输入还原为搜索前的内容。
        chat.set_input("keep");
        ctrl(&mut chat, 'r');
        ctrl(&mut chat, 'c');
        assert!(chat.search.is_none(), "ctrl+c 退出搜索");
        assert_eq!(chat.input, "keep", "取消不改输入");
        let _ = std::fs::remove_dir_all(&chat.session.home);
    }

    /// Alt+T 思考开关：off ↔ 上一次的等级。
    #[test]
    fn alt_t_toggles_thinking() {
        let mut chat = chat_with_history("think");
        let _ = chat.session.runtime.thinking_tx.send(Some("high".to_string()));
        alt(&mut chat, 't');
        assert_eq!(*chat.session.runtime.thinking.borrow(), None, "关闭思考");
        alt(&mut chat, 't');
        assert_eq!(
            chat.session.runtime.thinking.borrow().as_deref(),
            Some("high"),
            "恢复上次等级"
        );
    }

    /// 任务区（CC 字形）：`☐`/`☒`，已完成项弱化 + 删除线语义。
    #[test]
    fn task_lines_use_checkbox_glyphs() {
        let mut chat = chat_with_history("todo");
        chat.tasks_visible = true;
        chat.tasks_cache = vec![
            TodoItem { text: "done one".into(), status: TodoStatus::Done },
            TodoItem { text: "doing".into(), status: TodoStatus::InProgress },
            TodoItem { text: "later".into(), status: TodoStatus::Pending },
        ];
        let lines = chat.task_lines();
        let joined: Vec<String> = lines.iter().map(|l| l.plain_text()).collect();
        assert!(joined[0].contains("todo · 1/3 tasks"), "{joined:?}");
        assert!(joined.iter().any(|l| l == "☒ done one"), "{joined:?}");
        assert!(joined.iter().any(|l| l == "☐ doing"), "{joined:?}");
        assert!(joined.iter().any(|l| l == "☐ later"), "{joined:?}");
        assert!(!joined.iter().any(|l| l.contains("[x]") || l.contains("[ ]")));
        let done_text = lines
            .iter()
            .find(|l| l.plain_text() == "☒ done one")
            .and_then(|l| l.segs.last())
            .expect("done seg");
        assert!(done_text.style.strikethrough, "已完成项带删除线语义");
        assert_eq!(done_text.style.fg, Some(chat.theme.inactive), "并弱化呈现");
    }

    /// 空输入的占位提示（CC placeholder），有输入即消失。
    #[test]
    fn empty_prompt_shows_placeholder() {
        let mut chat = chat_with_history("placeholder");
        let lines = chat.prompt_lines();
        assert_eq!(lines.len(), 1);
        let text = lines[0].plain_text();
        // Caret sits ON the first placeholder cell: `▋` replaces the first
        // char instead of being glued in front of the full hint.
        let mut rest = crate::tui::keys::INPUT_PLACEHOLDER.chars();
        rest.next();
        assert_eq!(text, format!("▋{}", rest.as_str()), "{text}");
        chat.set_input("x");
        let text = chat.prompt_lines()[0].plain_text();
        assert_eq!(text, "x▋", "有输入即无占位");
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

    // ---- #18 呈现层最小实现：错误行高亮 + 整屏态 + 重试/返回 ----

    /// #18 全流程级整屏错误态：注入 Full 级 fixture → `last_error` 记录 →
    /// `Frame::assemble` 出整屏错误行（标题/稳定码/动作）→ Esc 返回清错误态
    /// （AC-26/53 返回路径非死路）。
    #[test]
    fn full_error_shows_full_screen_and_esc_returns() {
        use crate::error::ErrorLevel;
        use crate::tui::app::Frame;
        use crate::tui::test_util::error_fixtures;
        use crossterm::event::{KeyCode, KeyModifiers};
        use ratatui::layout::Size;
        let mut chat = test_chat();
        let fx = error_fixtures()
            .into_iter()
            .find(|f| f.code == "AUTH_REQUIRED")
            .expect("FX-04 在清单中");
        fx.inject(&chat.events);
        chat.drain_events();
        let err = chat.last_error.as_ref().expect("错误态已记录");
        assert_eq!(err.code, "AUTH_REQUIRED");
        assert_eq!(err.level, ErrorLevel::Full);
        let frame = Frame::assemble(&chat, Size::new(80, 24));
        let joined: String = frame
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("出错了"), "整屏错误态标题: {joined}");
        assert!(joined.contains("code=AUTH_REQUIRED"), "稳定码可见: {joined}");
        assert!(joined.contains("重试"), "首要动作提示: {joined}");
        assert!(frame.cursor.is_none(), "整屏态输入光标隐藏");
        // Esc 返回：非死路。
        chat.on_key(KeyCode::Esc, KeyModifiers::empty());
        assert!(chat.last_error.is_none(), "Esc 返回清除错误态");
    }

    /// #18 页面级错误行高亮：注入 Page 级 fixture → `[error]` 行叠加 error 色
    /// （A 区，theme.error = (255,107,128) 样色基线）。
    #[test]
    fn page_error_row_is_highlighted_with_error_color() {
        use crate::error::ErrorLevel;
        use crate::tui::app::Frame;
        use crate::tui::test_util::{error_fixtures, ErrorContext};
        use ratatui::layout::Size;
        use ratatui::style::Color;
        let mut chat = test_chat();
        let fx = error_fixtures()
            .into_iter()
            .find(|f| f.code == "TIMEOUT" && f.context == ErrorContext::ShortSync)
            .expect("FX-01 在清单中");
        fx.inject(&chat.events);
        chat.drain_events();
        assert_eq!(
            chat.last_error.as_ref().unwrap().level,
            ErrorLevel::Page
        );
        let frame = Frame::assemble(&chat, Size::new(80, 24));
        let error_row = frame
            .rows
            .iter()
            .find(|r| r.line.plain_text().starts_with("[error]"))
            .expect("错误行存在");
        assert!(
            error_row
                .line
                .segs
                .iter()
                .any(|s| s.style.fg == Some(Color::Rgb(255, 107, 128))),
            "错误行高亮用 error 色 (255,107,128): {:?}",
            error_row.line.segs
        );
    }

    /// #18 整屏态 Enter 重试最近输入（AC-15/53 重试路径骨架）。
    #[tokio::test]
    async fn full_error_enter_retries_last_prompt() {
        use crate::error::ErrorLevel;
        use crate::tui::test_util::error_fixtures;
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut chat = test_chat();
        chat.last_prompt = "为什么天是蓝的".into();
        let fx = error_fixtures()
            .into_iter()
            .find(|f| f.code == "PERMISSION_DENIED")
            .expect("FX-05 在清单中");
        fx.inject(&chat.events);
        chat.drain_events();
        assert_eq!(
            chat.last_error.as_ref().unwrap().level,
            ErrorLevel::Full
        );
        chat.on_key(KeyCode::Enter, KeyModifiers::empty());
        assert!(chat.last_error.is_none(), "Enter 清除错误态");
        assert!(chat.busy, "Enter 重试启动新回合");
    }

    // ---- qa 断言侧（交付 3/3）：AC-53 / AC-29 / 呈现层样式 ----

    /// AC-53 长回合失败升级：FX-11（TIMEOUT + LongTurn）→ 全流程级整屏态，
    /// 与 FX-01（TIMEOUT + ShortSync，页面级）**同码不同级**，由 context 区分。
    /// 整屏态含稳定码 + 重试/返回路径（AC-53 F3），光标隐藏。
    #[test]
    fn qa_ac53_long_turn_timeout_escalates_to_full_screen() {
        use crate::error::ErrorContext;
        use crate::error::ErrorLevel;
        use crate::tui::app::Frame;
        use crate::tui::test_util::error_fixtures;
        use ratatui::layout::Size;
        // 长回合传输层超时 → 全流程级。
        let mut chat = test_chat();
        let fx = error_fixtures()
            .into_iter()
            .find(|f| f.code == "TIMEOUT" && f.context == ErrorContext::LongTurn)
            .expect("FX-11 在清单中");
        fx.inject(&chat.events);
        chat.drain_events();
        let err = chat.last_error.as_ref().expect("错误态已记录");
        assert_eq!(err.code, "TIMEOUT");
        assert_eq!(err.level, ErrorLevel::Full, "长回合 TIMEOUT 升级全流程级（AC-53）");
        let frame = Frame::assemble(&chat, Size::new(80, 24));
        let joined: String = frame
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("code=TIMEOUT"), "整屏态含稳定码: {joined}");
        assert!(
            joined.contains("重试") || joined.contains("返回"),
            "AC-53 含「可重试或返回」路径: {joined}"
        );
        assert!(frame.cursor.is_none(), "整屏态输入光标隐藏");
        // 同码短同步（FX-01）→ 页面级错误行，非整屏态——TIMEOUT 双级别由上下文区分。
        let mut short = test_chat();
        let fx_short = error_fixtures()
            .into_iter()
            .find(|f| f.code == "TIMEOUT" && f.context == ErrorContext::ShortSync)
            .expect("FX-01 在清单中");
        fx_short.inject(&short.events);
        short.drain_events();
        let frame_short = Frame::assemble(&short, Size::new(80, 24));
        let joined_short: String = frame_short
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined_short.contains("[error] code=TIMEOUT"),
            "短同步 TIMEOUT = 页面级错误行: {joined_short}"
        );
        assert!(!joined_short.contains("出错了"), "短同步不整屏: {joined_short}");
    }

    /// AC-29 逐码矩阵：error_fixtures() 全部 11 个 fixture 注入，断言
    /// 「级别由生产者显式携带 + 渲染形态与 level 匹配」——Full → 整屏态，
    /// Page/Field → 错误行。断言锚 = 稳定码，不匹配 msg 文案。
    #[test]
    fn qa_ac29_fixture_matrix_renders_by_level() {
        use crate::error::ErrorLevel;
        use crate::tui::app::Frame;
        use crate::tui::test_util::error_fixtures;
        use ratatui::layout::Size;
        for fx in error_fixtures() {
            let mut chat = test_chat();
            fx.inject(&chat.events);
            chat.drain_events();
            let err = chat.last_error.as_ref().expect("错误态已记录");
            assert_eq!(err.code, fx.code, "错误码已记录: {}", fx.code);
            assert_eq!(
                err.level, fx.level,
                "级别由生产者显式携带（不复制映射表）: {}",
                fx.code
            );
            let frame = Frame::assemble(&chat, Size::new(80, 24));
            let joined: String = frame
                .rows
                .iter()
                .map(|r| r.line.plain_text())
                .collect::<Vec<_>>()
                .join("\n");
            match fx.level {
                ErrorLevel::Full => {
                    assert!(joined.contains("出错了"), "全流程级整屏态标题: {} / {joined}", fx.code);
                    assert!(
                        joined.contains(&format!("code={}", fx.code)),
                        "整屏态含稳定码: {} / {joined}",
                        fx.code
                    );
                    assert!(frame.cursor.is_none(), "整屏态光标隐藏: {}", fx.code);
                }
                ErrorLevel::Page | ErrorLevel::Field => {
                    assert!(
                        joined.contains(&format!("[error] code={}", fx.code)),
                        "页面/字段级错误行含稳定码: {} / {joined}",
                        fx.code
                    );
                    assert!(!joined.contains("出错了"), "页面/字段级不整屏: {} / {joined}", fx.code);
                }
            }
        }
    }

    /// 呈现层样式（A 区）：页面级错误行经 `render_rows` 渲染到 Buffer 后，
    /// **真实 cell 用 error 色 (255,107,128)**（非仅 SegStyle 层）——断言
    /// 「用户看到的高亮」落在最终画面，样式与文本双锚。
    #[test]
    fn qa_page_error_row_paints_error_color_in_buffer() {
        use crate::error::ErrorContext;
        use crate::tui::app::Frame;
        use crate::tui::test_util::error_fixtures;
        use ratatui::buffer::Buffer;
        use ratatui::layout::{Rect, Size};
        use ratatui::style::Color;
        let mut chat = test_chat();
        let fx = error_fixtures()
            .into_iter()
            .find(|f| f.code == "TIMEOUT" && f.context == ErrorContext::ShortSync)
            .expect("FX-01 在清单中");
        fx.inject(&chat.events);
        chat.drain_events();
        let frame = Frame::assemble(&chat, Size::new(80, 24));
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        let area = buf.area;
        crate::tui::view::render_rows(&frame.rows, Color::White, &mut buf, area);
        let err_color = Color::Rgb(255, 107, 128);
        let has_err_color = (0..buf.area.height).any(|y| {
            (0..buf.area.width).any(|x| buf[(x, y)].fg == err_color)
        });
        assert!(has_err_color, "错误行真实渲染 error 色 (255,107,128) 到 cell");
        // 文本锚（断言只锚 code）。
        let joined: String = frame
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("[error] code=TIMEOUT"),
            "错误行文本含稳定码: {joined}"
        );
    }

    /// FX-01 **真实路径**断言（main #91 / dev #92 邀请）：`/model` 二级菜单
    /// 拉取（`open_model_models` 生产发射源）在 list_models 读超时（10s）时
    /// 发射 `UiEvent::Error { level: Page, context: ShortSync }`——不经 fixture
    /// 注入，验证**生产触发源**接线（AC-12/13/14 页面级契约有真实落点）。
    /// 降级行为保留：错误行可见、非整屏态、不阻断。
    #[tokio::test(start_paused = true)]
    async fn qa_fx01_real_path_model_menu_failure_emits_page_error() {
        use crate::api::client::test_hooks;
        use crate::error::ErrorContext;
        use crate::error::ErrorLevel;
        use crate::tui::app::Frame;
        use ratatui::layout::Size;
        let _guard = test_hooks::hang_guard(60_000); // list_models 挂起 60s，> 读档 10s
        let mut chat = test_chat();
        chat.open_model_models("test".into()); // 触发真实生产拉取路径（fork provider）
        // 先让 spawn 任务启动并注册超时 timer（start_paused 下需 poll 才推进）。
        tokio::task::yield_now().await;
        // 读档 10s 超时到点 → 发射 UiEvent::Error（页面级）。
        tokio::time::advance(std::time::Duration::from_secs(11)).await;
        tokio::task::yield_now().await; // 让 spawn 任务完成事件发送
        chat.drain_events();
        let err = chat.last_error.as_ref().expect("生产发射源已记录错误态");
        assert_eq!(err.code, "TIMEOUT", "list_models 读超时落 TIMEOUT");
        assert_eq!(err.level, ErrorLevel::Page, "短同步=页面级（真实路径）");
        assert_eq!(err.context, ErrorContext::ShortSync, "上下文=短同步");
        // 渲染：页面级错误行可见，非整屏态（降级行为保留）。
        let frame = Frame::assemble(&chat, Size::new(80, 24));
        let joined: String = frame
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("[error] code=TIMEOUT"),
            "真实路径错误行可见: {joined}"
        );
        assert!(!joined.contains("出错了"), "页面级不整屏: {joined}");
    }
}
