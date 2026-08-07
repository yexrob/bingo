//! Incremental model for the chat state machine: messages/activities/collapse groups + document row construction.
//!
//! Ported from the old `tui.rs` `BingoChat` (ratatui edition): event handling semantics,
//! collapse detection, and expand/collapse toggling are preserved as-is; `draw` is replaced by [`Chat::build_rows`],
//! which produces display-agnostic styled row documents, mapped to terminal rows by [`crate::tui::view`].
//! Events arrive from channels (`UiEvent` / `AskRequest`); keyboard/mouse come in via
//! [`Chat::on_key`] / [`Chat::doc_click`].

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
    /// Full-row background.
    pub bg: Option<Color>,
    /// Right padding inside the row (CC user bubble paddingRight=1).
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

    /// Bubble row with a full-row background (user messages; CC paddingRight=1).
    pub fn bubble(line: Line, bg: Color) -> Self {
        let mut row = Row::new(line);
        row.bg = Some(bg);
        row.padding_right = 1;
        row
    }
}

/// Click target of a document row.
#[derive(Debug, Clone)]
pub enum ClickTarget {
    /// Collapse-group row (collapses/expands the group).
    Group { message: usize, group: usize },
    /// Activity header row (collapses/expands the activity).
    Activity { message: usize, path: Vec<usize> },
    /// Permission option (confirm by index).
    AskOption(usize),
}

/// Document coordinate range of a clickable row.
#[derive(Debug, Clone)]
pub struct ClickRange {
    pub start: usize,
    pub end: usize,
    pub target: ClickTarget,
}

/// Scrollable document: all rows + click ranges.
///
/// In inline mode the document only covers the "not yet flushed" part (messages after [`Chat::flushed_segments`]),
/// so row numbers are not global — click targeting and scrolling are only used in fullscreen mode.
#[derive(Debug, Clone)]
pub struct Doc {
    pub rows: Vec<Row>,
    pub click_ranges: Vec<ClickRange>,
    /// Number of leading "settled" rows: rows that no longer change and can be printed
    /// into the terminal scrollback in one go (the print boundary for REPL mode; unused in fullscreen).
    /// Production uses `settled_marks` checkpoints; this aggregate is kept as the test-facing
    /// "settled prefix row count" handle.
    #[cfg_attr(not(test), allow(dead_code))]
    pub settled: usize,
    /// 定稿检查点（欢迎卡 / 每条定稿消息各一个，行号递增）：
    /// 懒落盘按检查点整段冻结，resize 回灌按检查点整段取回。
    pub settled_marks: Vec<SettledMark>,
    /// Number of transient rows at the end of the document (slash output, gone after TTL): lazy-flush
    /// window math must exclude them — a transient list shrinking the window is no reason to freeze live content.
    pub transient_rows: usize,
}

/// 一个定稿检查点：`row_end` 之前的行全部定稿。`segments` 是构建内
/// 累计值，跨多次 [`Chat::advance_flushed_upto`] 的增量由
/// `Chat::mark_base` 消化。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettledMark {
    /// Row count covered by this checkpoint (exclusive end within doc.rows).
    pub row_end: usize,
    /// Message segments covered (build-internal accumulation, including the welcome card).
    pub segments: usize,
}

/// Current error state (#18 presentation layer): `code`/`msg`/`level`/`context` come from structured
/// `UiEvent::Error`; the level is decided by the triggering context (short sync = page-level, long turn = full-flow).
#[derive(Debug, Clone)]
pub struct ErrorState {
    pub code: &'static str,
    pub msg: String,
    pub level: crate::error::ErrorLevel,
    /// Triggering context (contract field: the event chain "producer → event → state" keeps
    /// context alive for auditing and future short-op integration; the render branch uses `level`).
    #[allow(dead_code)]
    pub context: crate::error::ErrorContext,
}

/// A session message (user or assistant text + assistant activity notices).
#[derive(Debug, Clone)]
pub struct UiMessage {
    pub role: Role,
    pub text: String,
    pub activities: Vec<Activity>,
    /// Char count of text at activities[i] creation: rendering interleaves text and activities in model output order.
    pub insert_points: Vec<usize>,
    /// Collapse groups for consecutive Read/Search operations.
    pub groups: Vec<CollapseGroup>,
    /// Index of the collapse group activities[i] belongs to (None = standalone activity).
    pub group_of: Vec<Option<usize>>,
}

/// Collapse group for consecutive Read/Search operations: collapses into a one-line rule summary (`Read 3 files`).
#[derive(Debug, Clone)]
pub struct CollapseGroup {
    /// Activity indices in the group (in order).
    pub activities: Vec<usize>,
    /// Number of search operations.
    pub search: usize,
    /// Read file paths (deduplicated count).
    pub read_paths: Vec<String>,
    /// Number of read operations without a path.
    pub read_ops: usize,
    /// Number of list operations (ls/tree/du).
    pub list: usize,
    /// Number of plain Bash operations.
    pub bash: usize,
    /// Group still open (in progress → summary uses the -ing form + …).
    pub active: bool,
    /// ctrl+o / click expands the group into individual tools.
    pub expanded: bool,
    /// Input hint of the group's most recent tool (shown on the ⎿ line while running).
    pub last_hint: Option<String>,
}

/// Collapsible classification of a tool (isSearchOrReadCommand).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CollapseKind {
    Search,
    /// Read or read-like Bash: carries a file path (None for Bash).
    Read(Option<String>),
    List,
    /// Plain Bash that is neither search, read, nor list.
    Bash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// Tool calls not shown in the transcript (renderToolUseMessage = null):
/// the Task tool family (shown in the task panel) and AskUserQuestion (shown in the dialog).
pub fn is_hidden_tool(name: &str) -> bool {
    matches!(
        name,
        "TaskCreate"
            | "TaskUpdate"
            | "TaskGet"
            | "TaskList"
            | "AskUserQuestion"
            // Agent aligns with Task renderToolUseMessage=null: no tool row is rendered,
            // progress is carried solely by the Watch activity row (`Agent: <description> · N chars produced`).
            | "Agent"
    )
}

/// Built-in slash command table (single source shared by /help and the dropdown suggestions).
pub const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("help", "显示可用命令"),
    ("clear", "清空对话，开始新会话（别名 /reset /new）"),
    ("compact", "压缩上下文（旧消息 → 摘要）"),
    ("model", "显示/切换模型（/model [名称]）"),
    ("resume", "恢复历史会话（/resume [名称或关键词]）"),
    ("rename", "重命名当前会话（/rename [名称]）"),
    ("share", "导出当前会话为 HTML 分享页（/share [--open]）"),
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

/// `/share` 参数解析：是否包含指定 flag（--local / --open）。
fn parse_share_arg(arg: &str, flag: &str) -> bool {
    arg.split_whitespace().any(|t| t == flag)
}

/// Slash dropdown suggestion item (/name + description).
#[derive(Debug, Clone, PartialEq)]
pub struct SlashSuggestion {
    pub name: String,
    pub description: String,
}

/// Footer model badge: `{model} · think {level}` (off = no level shown, keeps it concise).
pub fn model_footer_label(model: &str, thinking: Option<&str>) -> String {
    match thinking {
        Some(level) if level != "off" => format!("{model} · think {level}"),
        _ => model.to_string(),
    }
}

/// `/model` two-level selector state: level one = endpoint list, level two = that endpoint's models
/// (fetched async from `/v1/models`; shows known models + loading until the fetch completes).
#[derive(Clone)]
pub struct ModelMenu {
    /// Level-one list: `default` (top-level config) + settings.providers names.
    pub providers: Vec<String>,
    pub provider_selected: usize,
    /// Level-two model list (None = still on level one).
    pub models: Option<ModelMenuModels>,
}

#[derive(Clone)]
pub struct ModelMenuModels {
    pub provider: String,
    /// Loaded models (filled in asynchronously; may be incomplete).
    pub models: Vec<String>,
    pub loading: bool,
    pub selected: usize,
}

/// `/think` single-level selector state (level table = off + [`crate::api::types::THINKING_LEVELS`]).
#[derive(Clone)]
pub struct ThinkMenu {
    pub selected: usize,
}

/// `/think` selector entries: level name + description (everything past off corresponds one-to-one with
/// THINKING_LEVELS, in the same order; consistency is guaranteed by a test).
pub const THINK_LEVELS: &[(&str, &str)] = &[
    ("off", "不发 thinking 参数（兼容 DeepSeek 等端点）"),
    ("low", "adaptive thinking · effort low"),
    ("medium", "adaptive thinking · effort medium"),
    ("high", "adaptive thinking · effort high（默认档位）"),
    ("xhigh", "adaptive thinking · effort xhigh（编码/agentic 推荐）"),
    ("max", "adaptive thinking · effort max（最深推理）"),
];

/// Max visible rows in the dropdown (OVERLAY_MAX_ITEMS = 5).
pub const SLASH_SUGGESTIONS_MAX: usize = 5;

/// Max rows rendered for the input area (longer input scrolls to the caret's line).
pub const INPUT_ROWS_MAX: usize = 10;
/// Max rows shown for queued messages (more collapse into `… +N more`).
pub const QUEUE_ROWS_MAX: usize = 3;
/// Max entities shown one per line while the entity selector is focused.
pub const ENTITY_ROWS_MAX: usize = 6;
/// Undo stack depth (ctrl+_).
pub const UNDO_MAX: usize = 20;
/// Exit-confirmation window between two Ctrl+C presses.
pub const CTRL_C_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);
/// Clear-confirmation window between two Esc presses.
pub const ESC_WINDOW: std::time::Duration = std::time::Duration::from_secs(1);
/// Paste-burst detection: key intervals shorter than this count as one batch of input.
///
/// Terminals with bracketed paste go through [`Chat::on_paste`] (real `Event::Paste`);
/// this heuristic is only a fallback for terminals without it. Its limitations are documented here
/// because they define the experience boundary on those terminals:
/// - very fast typists (<10ms/key for more than [`PASTE_BURST_KEYS`] presses in a row) are
///   misjudged as pasting: Enter inserts a newline instead of sending — press Esc or pause to recover;
/// - automated char-by-char replay (tmux send-keys, expect) is misjudged the same way;
/// - conversely, slow pastes (SSH jitter) look like typing, so Enter sends directly.
pub const PASTE_BURST_GAP: std::time::Duration = std::time::Duration::from_millis(10);
/// Number of consecutive "fast" keys before it counts as a paste (below this is normal typing).
pub const PASTE_BURST_KEYS: usize = 4;
/// Pastes longer than this many lines collapse into a placeholder.
pub const PASTE_COLLAPSE_LINES: usize = 10;

/// Image placeholder reference (`#[image N]` → the Nth attachment, 1-based).
static IMAGE_MARKER_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"#\[image (\d+)\]").expect("static regex"));

/// Image placeholder text: `#[image N]`.
fn image_marker(id: usize) -> String {
    format!("#[image {id}]")
}

/// Expands a `~` prefix to the home directory (returns unchanged when there is no home).
fn expand_home(path: &str) -> String {
    if let (Some(rest), Ok(home)) = (path.strip_prefix("~/"), std::env::var("HOME")) {
        return format!("{home}/{rest}");
    }
    path.to_string()
}

/// An image path on its own line: path signature (`~` prefix or contains `/`) + image extension.
fn standalone_image_path(s: &str) -> Option<String> {
    // Windows paths use backslashes: accept either separator (plus `~` home expansion).
    if !(s.starts_with('~') || s.contains('/') || s.contains('\\')) {
        return None;
    }
    let ext = std::path::Path::new(s)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)?;
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp")
        .then(|| s.to_string())
}

/// Path of a whole `![alt](path)` line (no spaces in path; unwraps `<path>`).
fn markdown_image_path(s: &str) -> Option<String> {
    let rest = s.strip_prefix("![")?;
    let close = rest.find("](")?;
    let rest = &rest[close + 2..];
    let end = rest.find(')')?;
    let p = &rest[..end];
    let p = p.strip_prefix('<').and_then(|p| p.strip_suffix('>')).unwrap_or(p);
    (!p.is_empty() && !p.contains(' ')).then(|| p.to_string())
}

/// Load timeout for a single image (a timeout counts as a load failure).
pub const IMAGE_LOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Lifetime of slash transient hints: they disappear from above the input after the timeout (never flushed).
pub const SLASH_OUTPUT_TTL: std::time::Duration = std::time::Duration::from_secs(2);

/// User message text entering the message flow when AskUserQuestion is declined
/// (Esc / empty Other submit) — an ordinary message, persistent with the flow.
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

/// Whether the command contains a non-neutral segment (pure echo/printf etc. do not collapse).
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

/// Bash command classification (split on && / || / | / ;, skipping quantifiers, redirection targets,
/// and neutral commands; every segment must belong to the search/read/list sets; when mixed, place by list > search > read).
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

/// Hint shown while a collapse group runs: the input of the group's most recent tool.
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

/// Collapse-group summary text: `Searched for 2 patterns, read 3 files`;
/// uses the -ing form plus a trailing … while in progress.
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

/// One-line result summary for the expanded state (CC renderToolResultMessage).
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

/// Bash tool result preview: strips the `$ cmd` echo and the `[Exited with code N]` footnote,
/// leaving only the command output (the bare output shown for BashModeProgress).
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

/// Playful words for the thinking stage.
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

/// Random completion words (`TURN_COMPLETION_VERBS`, all fit `for Xs`).
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

/// Completion word: sampled from creation-time nanoseconds (a different source than the running words, `✻ Churned for 40s`).
fn thinking_done_verb() -> &'static str {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0);
    COMPLETION_WORDS[nanos % COMPLETION_WORDS.len()]
}

/// Edit action classification: consecutive same-kind micro-edits (char-by-char insert/delete) merge
/// into one undo step; whole replacements (kill / yank / newline / history fill) are their own steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Insert,
    Delete,
    Bulk,
}

/// Bottom running status: `✻ {verb}… (esc to interrupt · {N}s · ↓ {tokens} tokens)`.
#[derive(Debug, Clone, PartialEq)]
pub struct RunningStatus {
    /// Current verb (tool summary / thinking word / `Working`).
    pub verb: String,
    /// Seconds elapsed in this turn.
    pub elapsed: f64,
    /// Tokens produced this turn (0 = segment omitted).
    pub tokens: u64,
}

/// ctrl+r reverse search state: query string + current hit (the classic inline edition).
#[derive(Debug, Clone, Default)]
pub struct HistorySearch {
    /// The filter string typed by the user.
    pub query: String,
    /// The matched history entry (None = no match).
    pub hit: Option<String>,
    /// Index of the hit in history (pressing ctrl+r again keeps searching older from it).
    pub index: Option<usize>,
}

/// bingo chat component state: message stream + activity notices + input + permission requests.
pub struct Chat {
    pub session: Arc<Session>,
    pub(super) events: mpsc::UnboundedSender<UiEvent>,
    pub asks: mpsc::UnboundedSender<AskRequest>,
    events_rx: mpsc::UnboundedReceiver<UiEvent>,
    asks_rx: mpsc::UnboundedReceiver<AskRequest>,
    pub messages: Vec<UiMessage>,
    pub input: String,
    /// Byte position of the caret in `input` (always on a char boundary).
    pub cursor: usize,
    /// Text last deleted with ctrl+k/u/w (ctrl+y pastes it back).
    kill: String,
    /// Edit undo stack (text + caret), capped at [`UNDO_MAX`].
    undo: Vec<(String, usize)>,
    /// Type of the last edit (consecutive same-kind edits merge in the undo stack).
    last_edit: Option<EditKind>,
    /// Thinking level before Alt+T disabled it (pressing again restores it).
    last_thinking: Option<String>,
    /// Input stashed with ctrl+s (text + caret).
    stash: Option<(String, usize)>,
    /// Submitted prompts (persisted per cwd; falls back to in-session on write failure).
    pub history: crate::tui::history::History,
    /// Whether the history file is writable (after one failure, never retry — avoid hitting the same error on every submit).
    history_writable: bool,
    /// Messages queued while busy (submitted one by one after TurnEnd).
    pub queued: Vec<String>,
    /// Whether the `?` shortcut panel is expanded.
    pub help_visible: bool,
    /// Bottom transient notice (`Press ctrl-c again to exit` etc.).
    pub notice: Option<&'static str>,
    /// Time of the most recent Ctrl+C on empty input (a second press within [`CTRL_C_WINDOW`] exits).
    ctrl_c_at: Option<std::time::Instant>,
    /// Time of the most recent Esc (a second press within [`ESC_WINDOW`] clears the input).
    esc_at: Option<std::time::Instant>,
    /// Time of the last key press and the count of consecutive "fast" keys (paste-burst heuristic).
    last_key_at: Option<std::time::Instant>,
    burst_keys: usize,
    /// Collapsed paste blocks: placeholder `[Pasted text #N +M lines]` → real content.
    pastes: Vec<(String, String)>,
    /// Image attachments mounted in the message box (`#[image N]` placeholder → N = index here + 1).
    attachments: Vec<crate::api::types::ImageAttachment>,
    /// `!` commands run in this session (prefix completion for Tab in bash mode).
    bash_history: Vec<String>,
    /// ctrl+r reverse search state (None = not active).
    pub search: Option<HistorySearch>,
    /// Current permission mode (cycled with shift+tab). `Session` is immutable inside an `Arc`,
    /// so this holds the one actually in effect: each turn derives a `Session` copy from it.
    pub permission_mode: PermissionMode,
    /// ctrl+l requests a full-screen repaint (cleared after the render layer consumes it).
    pub force_redraw: bool,
    /// inline ctrl+o requests a full-transcript replay: everything expanded, the flush cursor
    /// rewound; the app freezes the settled part into scrollback on the next frame (cleared after consumption).
    pub dump_transcript: bool,
    /// bash mode (`!` prefix): input executes directly, bypassing the model.
    pub bash_mode: bool,
    pub busy: bool,
    /// Esc/Ctrl+C interrupted the current turn: background-task completion no longer auto-starts
    /// a new turn (interrupt semantics: wait for the user to submit), reset in start_turn.
    pub interrupted: bool,
    /// Index of the current assistant message.
    pub stream_msg: Option<usize>,
    thinking_buf: String,
    /// Whether the current thinking segment is open for continuation: closed after ToolStart/TextDelta
    /// (segment boundaries); deltas in the same segment continue without paragraph breaks; new segments (fresh reasoning after a tool) are aggregated with \n\n.
    thinking_seg_open: bool,
    output_tokens: u64,
    pub tick: u64,
    /// Tick at TurnStart: the relative timing baseline for running-state thinking.
    turn_start_tick: u64,
    /// Real clock at TurnStart (baseline for the status-row elapsed time; cleared at TurnEnd).
    turn_started: Option<std::time::Instant>,
    /// Non-fatal warnings (timestamp + text): entries past `WARNING_TTL` expire
    /// automatically; rendering shows only valid entries (pruned on push).
    pub warnings: Vec<(std::time::Instant, String)>,
    /// Current error state (#18 presentation layer): drives the error-row highlight and the full-flow full-screen state.
    /// Recorded when `UiEvent::Error` arrives; cleared by the reset action (AC-03's four resets).
    /// The render side branches on `level`: Field/Page → error-row highlight, Full → full-screen error state.
    pub last_error: Option<ErrorState>,
    /// Input of the last submitted model turn (#18 full-screen error state reruns it on Enter=retry).
    pub last_prompt: String,
    pub cwd: String,
    /// Permission prompt: request + result receipt.
    pub pending_ask: Option<(PermissionRequest, oneshot::Sender<DialogAction>)>,
    /// Dialog focus row (0..=options.len(); == options.len() = Other input).
    ask_focus: usize,
    /// Buffer for Other free-form input.
    ask_other: String,
    /// 任务列表磁盘快照缓存（tick 周期刷新）。
    tasks_cache: Vec<TodoItem>,
    processor: MarkdownProcessor,
    renderer: MarkdownRenderer,
    reply_cache: HashMap<String, Vec<Line>>,
    /// Terminal image capability (kitty protocol; detected in inline mode, None in fullscreen).
    pub image_cap: Option<ImageCap>,
    /// Loaded image cache (url → PNG bytes + cell dimensions).
    pub images: HashMap<String, Arc<ImageMeta>>,
    /// Image urls currently being fetched (prevents duplicate loads).
    images_pending: HashSet<String>,
    /// Image cache version (bumped on load completion → invalidates the render cache).
    images_version: u64,
    /// Whether the document needs rebuilding (set after writes like events/tick/expand; cleared after the layout layer consumes it).
    pub dirty: bool,
    /// Width of the last build_rows (markdown cache invalidated by width).
    prev_build_width: usize,
    pub width: usize,
    /// Viewport row count (written by the layout layer; reconcile_scroll clamps scrolling with it).
    pub viewport_height: usize,
    /// Total terminal rows (written by the layout layer; the `?` panel budgets rows with it).
    pub height: usize,
    pub scroll: usize,
    pub auto_scroll: bool,
    /// Document from the last build_rows (click targeting).
    pub doc: Doc,
    /// Tool activity indices waiting to be classified on ToolReady (full input) (FIFO).
    pending_tools: Vec<usize>,
    pub theme: Theme,
    /// Detected terminal background color (used by /theme to rebuild the theme).
    detected_background: Option<bool>,
    /// Slash command output lines (/help /status etc.): rendered after messages, settled when idle.
    pub slash_lines: Vec<String>,
    /// When the slash output appeared (auto-dismissed by tick timeout).
    pub slash_at: Option<std::time::Instant>,
    /// /exit requested quitting (component layer consumes → system.exit).
    pub exit: bool,
    /// inline: segments of the document prefix already flushed to scrollback — 0 = none, 1 = welcome card,
    /// 1+k = welcome card + first k messages. The flush cursor counts by **message boundary**, not row number,
    /// so re-layout after a width change (all row numbers change) never reprints.
    pub flushed_segments: usize,
    /// inline：当前 doc 中已落盘的行数（canvas 尾部起点）；每次
    /// build_rows 归零——重建后落盘部分已不在文档里。
    pub tail_start: usize,
    /// Baseline that absorbs checkpoint accumulators: prevents double-counting when the
    /// flush cursor advances multiple times within one build (reset by build_rows).
    mark_base: usize,
    /// slash 下拉建议（输入 `/` 且无参数时非空；组件层渲染）。
    pub slash_suggestions: Vec<SlashSuggestion>,
    /// Selected index in the dropdown.
    pub slash_selected: usize,
    /// `/model` two-level selector (level-one endpoint → level-two model list; None = inactive).
    pub model_menu: Option<ModelMenu>,
    /// `/think` level selector (None = inactive).
    pub think_menu: Option<ThinkMenu>,
    /// Menu-level model-list cache (provider → latest `/v1/models` result):
    /// validates `/model <name>` direct sets against the known list; avoids
    /// re-fetching when re-entering level two (P2-G cache, per-session).
    pub models_cache: std::collections::HashMap<String, Vec<String>>,
    /// 任务区展开信号（Task 工具调用 → 展示任务列表）。
    pub tasks_visible: bool,
    /// Whether the task area was auto-opened by TaskCreate (not manually via ctrl+t): hides automatically when everything is done.
    pub tasks_auto: bool,
    /// Snapshot of the bottom entity area (agent instances + channels; refreshed on tick/WatchEvent).
    pub entities: Vec<EntityRow>,
    /// Entity selector focus (Some(i) = selection mode: ↑↓/Enter/Esc are captured).
    pub entity_focus: Option<usize>,
    /// Entity view pending open (app layer consumes → enters the fullscreen modal).
    pub open_entity: Option<EntityOpen>,
    /// Interrupt signal: Ctrl+C / Esc while busy → send(true), aborting stream reads in the turn immediately.
    cancel_tx: tokio::sync::watch::Sender<bool>,
}

/// One row of the bottom entity area: a subagent instance or a channel.
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

/// Entity view to open after selecting with Enter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityOpen {
    Agent(String),
    Channel(String),
}

impl Chat {
    /// Display TTL for non-fatal warnings: expired entries are no longer
    /// rendered (pruned on push).
    const WARNING_TTL: std::time::Duration = std::time::Duration::from_secs(10);

    /// Record a non-fatal warning (de-duped + stale entries pruned).
    pub(crate) fn push_warning(&mut self, message: String) {
        self.warnings.retain(|(t, _)| t.elapsed() < Self::WARNING_TTL);
        if !self.warnings.iter().any(|(_, w)| w == &message) {
            self.warnings.push((std::time::Instant::now(), message));
        }
    }

    /// The warning currently displayed (`None` when nothing is
    /// unexpired).
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
        // Watchable event forwarding: registry broadcast → UiEvent channel (persists across turns).
        // Skipped when there is no tokio runtime (tests).
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
            models_cache: HashMap::new(),
            tasks_visible: false,
            tasks_auto: false,
            entities: Vec::new(),
            entity_focus: None,
            open_entity: None,
            interrupted: false,
            cancel_tx: tokio::sync::watch::channel(false).0,
        }
    }

    /// Drains all pending events from the channel. Returns whether any event was handled.
    pub fn drain_events(&mut self) -> bool {
        let mut handled = false;
        while let Ok(event) = self.events_rx.try_recv() {
            handled = true;
            self.handle(event);
        }
        handled
    }

    /// Drains the permission channel (one at a time: a new request is only accepted when none is pending).
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

    /// Drains all channels. Returns whether there is any new state.
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
                // Cache this result (/model <name> validation + no re-fetch on re-entry).
                self.models_cache.insert(provider.clone(), models.clone());
                // 二级菜单补充异步拉取结果；列表仍空说明拉取失败（保留 loading
                // 不阻塞：用户可直接输入模型名或 Esc 退出）。
                if let Some(menu) = &mut self.model_menu
                    && let Some(m) = &mut menu.models
                    && m.provider == provider
                {
                    m.models = models;
                    m.loading = false;
                    // P1-F：当前 provider 且当前模型在列表中时预选它——
                    // 与 /think 菜单预选当前档位对等，避免浏览即误切。
                    let current_provider = self.session.runtime.provider.borrow().clone();
                    let current_model = self.session.runtime.model.borrow().clone();
                    let selected = if m.provider == current_provider {
                        m.models.iter().position(|name| *name == current_model)
                    } else {
                        None
                    };
                    m.selected = selected
                        .unwrap_or(0)
                        .min(m.models.len().saturating_sub(1));
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
                // Bump the cache version: the renderer's per-block cache and reply_cache invalidate together.
                self.images_version = self.images_version.wrapping_add(1);
                self.reply_cache.clear();
                self.dirty = true;
            }
            UiEvent::TurnStart => {
                // A new turn resets the error state (AC-03): page-level error rows vanish with the new turn
                // (full-screen Full is already dismissed in error_screen_key; this is a fallback).
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
                // Placeholder thinking: when the endpoint delays deltas (DeepSeek often by tens of seconds),
                // the running row is visible immediately.
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
                    // Text is a segment boundary: thinking after text opens a new block (no more aggregation),
                    // and the running thinking block closes with it (same closing semantics as ToolStart).
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
                        // Aggregation: when text has not interrupted (thinking_buf still holds this stage's text),
                        // new reasoning merges into the last thinking block. Same-segment continuation (segment open)
                        // appends directly; a new segment (after a tool/text) is separated by a blank line and counted.
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
                // Tool start = reasoning segment boundary: subsequent deltas aggregate into a new segment.
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
                // `!` commands: standalone activities (output preview expanded directly), not part of collapse groups.
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
                // Agent/channel lifecycle events also refresh the bottom entity area.
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
                    // After the user interrupted a turn, never auto-run again (wait for an explicit submit).
                    if !self.interrupted {
                        self.submit_auto();
                    }
                }
                // A channel row updated and the hub is idle with mail: start a turn to digest it (when a subagent posts,
                // the hub is usually not in a turn — without this wake-up the message would sleep until the user speaks).
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
                    // Collapse groups are bounded by text: model rounds do not split a group, nor does thinking —
                    // only text (TextDelta) and non-collapsible tools close the group.
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
                            // Standalone Bash (`!` commands): preview = the output itself (stripped of the
                            // `$ cmd` echo and the `[Exited with code N]` footnote),
                            // expanded by default (BashModeProgress shows the output directly).
                            // Skill: the result row shows only `✦ <skill name>` (same family as the activity header
                            // `✦ Skill(input)`); the pointer path stays only in tool_result.
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
                // AskUserQuestion answers are ordinary user messages (in the message flow,
                // settled/flushed with it) — nothing to clean at turn end, they persist with the session.
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
                    // Remove synchronously: the empty placeholder thinking and its insert point.
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
                    // Text is settled → asynchronously load its images (reply with ImageReady when done).
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
                // #18: structured error-state record (code/msg/level/context); the render side uses it to
                // produce the error row (Page/Field) or the full-screen state (Full) — independent of message-text
                // replacement and doc-rebuild timing, so nothing renders twice.
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

    /// Scans message text for markdown image references and asynchronously loads urls not cached
    /// or already in flight (data:/http(s)/local paths), replying with `ImageReady` when done.
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

    /// Thinking content renders with markdown streaming (code blocks/lists update as the stream flows).
    /// Re-renders with the full text each time (thinking deltas are small).
    fn render_thinking(&mut self, text: &str) -> Vec<Line> {
        if text.is_empty() {
            return Vec::new();
        }
        self.renderer.set_width(self.width);
        let doc = self.processor.process_streaming(text);
        self.renderer.render(&doc);
        self.renderer.lines().to_vec()
    }

    /// A click (doc row number) hitting a row → collapse/expand / permission-option confirm.
    /// Returns whether the click was handled.
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

    /// Click on a dialog option: the Other row → enter input mode; anything else confirms immediately.
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

    /// ctrl+o: globally expand/collapse the transcript (CC app:toggleTranscript).
    /// Priority: expanded groups collapse back first; otherwise, if anything is collapsible → expand all; else collapse all.
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

    /// inline ctrl+o expand direction (CC non-fullscreen transcript): rows already printed to scrollback
    /// cannot change (write-once), so instead of a collapse toggle it does a **full replay** — every collapsible item
    /// in all of history expands and the flush cursor rewinds; the app then freezes the whole transcript
    /// into scrollback in one go, where the user can scroll back to read it. Old collapsed copies already
    /// in scrollback cannot be retracted — duplicates are accepted (the same trade-off as rehydration). When everything
    /// is already on screen with nothing to expand, it is a no-op: the replay adds no information.
    ///
    /// The replay frame uses `force_redraw` (clear the visible screen): same as resize, clear first then write,
    /// replay content starts from the top of the screen with chrome right below — without clearing, the old frame and
    /// the replay rows would align by viewport history, so short content appears twice on screen.
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

    /// ctrl+o toggle direction: true only when the transcript has collapsible items and they are **all**
    /// expanded (the next press collapses). Always false with nothing collapsible — ctrl+o then
    /// degrades to a pure replay: pressing it repeatedly just reprints, never entering the collapse branch.
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

    /// inline ctrl+o collapse direction: all history folds back to the default aggregate state. Only the fold state
    /// changes; the caller's display layer closes it up via the same path as resize (clear-redraw + rehydration), because
    /// the expanded replay rows on screen are also write-once printed content — without clearing, they would
    /// coexist on screen with the collapsed window.
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
        // Turn in progress: queue it, submitted one by one after TurnEnd (CC message queueing).
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
            // Enter with a partial prefix and dropdown suggestions: apply the selection and run it
            // (handleEnter: with suggestions present, Enter = complete + execute).
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

    /// Large pastes collapse into a placeholder: the input keeps `[Pasted text #N +M lines]`,
    /// with the real content in [`Chat::pastes`], restored by [`Chat::expand_pastes`] on submit.
    /// Only called during a paste burst (detection limits see [`PASTE_BURST_GAP`]).
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
    /// When the clipboard holds an image (macOS), prefer mounting it: the `#[image N]` placeholder
    /// goes in at the caret and the text payload is ignored (terminals send no text for pure-image pastes).
    /// The burst heuristic ([`PASTE_BURST_GAP`]) stays as the fallback for
    /// terminals that do not report bracketed paste.
    pub fn on_paste(&mut self, text: &str) {
        // Tests must not read the system clipboard: an image in it would turn
        // a text paste into an image placeholder (the bracketed_paste test was
        // once flaky because the host clipboard held an image).
        if !cfg!(test)
            && let Some(id) = self.paste_clipboard_image()
        {
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

    /// Clipboard with an image (macOS): osascript reads the PNG → compress → register the attachment → placeholder id.
    fn paste_clipboard_image(&mut self) -> Option<usize> {
        let bytes = crate::tui::gfx::clipboard_image_png()?;
        self.register_image(&bytes)
    }

    /// Swaps placeholders back to their real content (at submit time).
    fn expand_pastes(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (token, body) in &self.pastes {
            out = out.replace(token.as_str(), body);
        }
        out
    }

    /// An image path in the input (a standalone path line, or a whole `![alt](path)` line) → read the file
    /// → compress and register → replace with the `#[image N]` placeholder. Unrecognized/unreadable lines stay as-is.
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

    /// Resolves `#[image N]` references in text → attachments (deduped, in order); unknown ids are ignored.
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

    /// Raw image bytes → compress (within the API limit) → register the attachment → placeholder id.
    fn register_image(&mut self, bytes: &[u8]) -> Option<usize> {
        let prepared = crate::api::image::prepare_image(bytes)?;
        self.attachments.push(crate::api::types::ImageAttachment {
            media_type: prepared.media_type,
            data: prepared.data,
        });
        Some(self.attachments.len())
    }

    /// Image file → register the attachment (read failure / non-image → None).
    fn register_image_file(&mut self, path: &std::path::Path) -> Option<usize> {
        let bytes = std::fs::read(path).ok()?;
        self.register_image(&bytes)
    }

    /// The `Session` in effect for this turn: `Session` is immutable inside `Arc`, and shift+tab must
    /// switch permission modes — so each turn derives a copy carrying the current mode (the other fields are shared
    /// handles: Runtime's watch channel, task store, and watch registry still point at the same state).
    fn session_for_turn(&self) -> Arc<Session> {
        if self.permission_mode == self.session.permission_mode {
            return self.session.clone();
        }
        let mut session = (*self.session).clone();
        session.permission_mode = self.permission_mode;
        Arc::new(session)
    }

    /// Queues slash output lines (transient hints: rendered after messages and above the input, gone after TTL).
    fn push_slash_output(&mut self, text: String) {
        for line in text.lines() {
            self.slash_lines.push(line.to_string());
        }
        self.slash_at = Some(std::time::Instant::now());
        self.dirty = true;
    }

    /// Slash command dispatch. Returns true = consumed.
    fn run_slash(&mut self, line: &str) -> bool {
        // Any slash run closes the dropdown (Enter on a full input skips submit's clear-menu branch,
        // otherwise suggestion rows like `+ /model …` would linger below the input forever).
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
            "share" => self.slash_share(arg),
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
                // Skill name (prompt Command: skills share the registry with built-in commands; typing
                // /skill-name runs it; the full body never enters the context, see the marker comment below).
                let skills = crate::skills::load_skills(
                    &self.session.home,
                    &std::path::PathBuf::from(&self.cwd),
                );
                if let Some(skill) = skills.iter().find(|s| s.name == other) {
                    // Progressive disclosure: only the `✦ <skill name> [args]` marker is submitted; the model
                    // reads the body on demand via the Skill tool pointer (`✦ name — read <path>`) + Read.
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

    /// Switches the runtime model and persists it as the default (same path as /theme /think: writes the project layer).
    fn set_model(&mut self, model: String) {
        // P1-E：已知列表校验——当前 provider 有缓存且未命中时附一句提示
        // （advisory 不阻塞；端点可能刚发布新模型/缓存未拉过，直接输入仍是
        // 合法路径）。与成功提示合并为单行，避免「⚠ 与 ✓ 并存」观感矛盾。
        let provider = self.session.runtime.provider.borrow().clone();
        let unknown = self
            .models_cache
            .get(&provider)
            .is_some_and(|known| !known.is_empty() && !known.contains(&model));
        let _ = self.session.runtime.model_tx.send(model.clone());
        self.persist_model(&model);
        let out = if unknown {
            format!("✓ 模型已切换: {model}（⚠ 不在 {provider} 已知列表，若请求失败用 /model 核对）")
        } else {
            format!("✓ 模型已切换: {model}")
        };
        self.push_slash_output(out);
    }

    /// Writes the model choice back to `.bingo/settings.json` (used as the default on next start; --model can still override).
    fn persist_model(&self, model: &str) {
        let cwd = std::path::PathBuf::from(&self.cwd);
        let _ = crate::settings::upsert_project_settings(
            &cwd,
            &serde_json::json!({ "model": model }),
        );
    }

    /// Enters the `/model` two-level selector: level one = current endpoint + configured providers.
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

    /// Level-one Enter: asynchronously fetches the model list from that provider endpoint (forks the
    /// endpoint, without switching the current one); results arrive via the ModelsLoaded event. The
    /// level-one list (providers + provider_selected) is kept as-is: Esc back to level one doesn't lose it.
    fn open_model_models(
        &mut self,
        provider: String,
        providers: Vec<String>,
        provider_selected: usize,
    ) {
        let session = self.session.clone();
        let events = self.events.clone();
        let provider_for_spawn = provider.clone();
        tokio::spawn(async move {
            let client = match session.client.with_provider(&provider_for_spawn) {
                Ok(c) => c,
                // default: clone the current endpoint directly.
                Err(_) => session.client.clone(),
            };
            let models = match client.list_models().await {
                Ok(m) => m,
                Err(e) => {
                    // #18/main #91: short-op failures must be visible (page-level error row, error color),
                    // behavior keeps degrading gracefully (menu still shows empty/known models) — "degraded + visible".
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
        // The menu was taken out by the Enter branch — rebuild the level-two state here (level-one list kept).
        self.model_menu = Some(ModelMenu {
            providers,
            provider_selected,
            models: Some(ModelMenuModels {
                provider,
                models: Vec::new(),
                loading: true,
                selected: 0,
            }),
        });
    }

    /// Model menu keys: ↑↓ move, Enter goes to level two / confirms, Esc exits. Returns whether consumed.
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
                    // Level one: go to level two and fetch the model list asynchronously (level-one list kept).
                    let provider = menu
                        .providers
                        .get(menu.provider_selected)
                        .cloned()
                        .unwrap_or_default();
                    self.open_model_models(provider, menu.providers, menu.provider_selected);
                    return true;
                };
                // Level two: confirm the selected model. Keep the menu when the list is empty (fetch failed/none returned).
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
                // 模型 + provider 一并持久化（P0-A）：下次启动恢复同一端点
                // 上的模型，避免「default 端点 + deepseek 模型」错配。
                let cwd = std::path::PathBuf::from(&self.cwd);
                let _ = crate::settings::upsert_project_settings(
                    &cwd,
                    &serde_json::json!({
                        "model": model.clone(),
                        "provider": provider.clone(),
                    }),
                );
                self.push_slash_output(format!("✓ 模型已切换: {provider} · {model}"));
                true
            }
            KeyCode::Esc => {
                // Level two → back to level one; level one → exit entirely (returns one level at a time).
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
        // The renderer baked in theme styles and reply_cache holds old-theme rows — rebuild them in sync.
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

    /// `/share`：导出当前会话分享页。默认上传官网分享服务（与 `bingo share`
    /// 子命令一致）并显示公网链接；`/share --local` 本地文件模式（保留）。
    fn slash_share(&mut self, arg: &str) {
        let local = parse_share_arg(arg, "--local");
        let open = parse_share_arg(arg, "--open");
        let Some(transcript) = self.session.runtime.transcript.borrow().clone() else {
            self.push_slash_output("尚无会话可导出（新会话未落盘，先发一条消息）。".to_string());
            return;
        };
        let messages = match transcript.load_messages() {
            Ok(m) => m,
            Err(e) => {
                self.push_slash_output(format!("读取会话失败: {e}"));
                return;
            }
        };
        let stem = transcript.name();
        let share_path = crate::share::shares_dir(&self.session.home).join(format!("{stem}.json"));
        let doc = match crate::share::ShareStore::load_or_create(&share_path) {
            Ok(store) => store.snapshot(),
            Err(e) => {
                self.push_slash_output(format!(
                    "无法读取 share 文档（{e}）；仅导出对话视图。"
                ));
                crate::share::ShareDoc::new(stem.clone())
            }
        };
        // 旧会话回退：无 share 文档时从主 transcript 推导 Team/DM/频道数据。
        let doc = if doc.agents.is_empty() && doc.channels.is_empty() {
            crate::share::derive_share_doc(&stem, &messages)
        } else {
            doc
        };
        let html = crate::share_html::render(&doc, &messages);
        let out = std::path::PathBuf::from(&self.cwd).join(format!("{stem}.html"));

        // 本地模式：写文件（覆盖提示 + 隐私警告），可选打开。
        if local {
            let overwritten = out.exists();
            if let Err(e) = crate::share::write_html_atomic(&out, &html) {
                self.push_slash_output(format!("写入失败: {e}"));
                return;
            }
            let mut lines = vec![format!(
                "✓ 已导出: {}{}",
                out.display(),
                if overwritten { "（覆盖）" } else { "" }
            )];
            if open {
                match crate::share::open_in_browser(&out.display().to_string()) {
                    Ok(_) => lines.push("已在浏览器中打开。".to_string()),
                    Err(e) => lines.push(format!("无法打开浏览器: {e}")),
                }
            }
            lines.push(
                "注意：此文件包含完整对话与工具输出（可能含敏感信息），分享前请自行审阅。"
                    .to_string(),
            );
            self.push_slash_output(lines.join("\n"));
            return;
        }

        // 上传模式：settings.share.baseUrl（缺省官网基址；服务公开无 token）。
        // 必须异步上传——reqwest::blocking 在 TUI async 事件循环内调用会 tokio
        // panic（Cannot block the current thread from within a runtime）。结果经
        // events.send(UiEvent::SlashOutput) 推送（同 slash_compact 模式）。
        let user_dir = std::env::var("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| self.session.home.join(".config"));
        let settings =
            crate::settings::load_settings(&user_dir, &std::path::PathBuf::from(&self.cwd))
                .unwrap_or_default();
        let base = settings
            .share
            .base_url
            .unwrap_or_else(|| crate::share::DEFAULT_SHARE_BASE.to_string());
        let id = crate::share::share_id(&stem);
        let events = self.events.clone();
        self.push_slash_output("⏳ 正在发布分享页…".to_string());
        tokio::spawn(async move {
            match crate::share::upload_share(&base, &id, &html).await {
                Ok(url) => {
                    let mut lines = vec![format!("✓ 已发布: {url}")];
                    if open {
                        match crate::share::open_in_browser(&url) {
                            Ok(_) => lines.push("已在浏览器中打开。".to_string()),
                            Err(e) => lines.push(format!("无法打开浏览器: {e}")),
                        }
                    }
                    lines.push(
                        "注意：任何人可公开访问此链接；分享页含完整对话与工具输出（可能含敏感信息），传播前请自行审阅。"
                            .to_string(),
                    );
                    let _ = events.send(UiEvent::SlashOutput(lines.join("\n")));
                }
                Err(e) => {
                    // 上传失败回退本地文件 + 提示（与 bingo share 子命令一致）。
                    let mut lines = vec![format!("上传失败（{e}）；回退本地文件。")];
                    let overwritten = out.exists();
                    match crate::share::write_html_atomic(&out, &html) {
                        Ok(()) => lines.push(format!(
                            "✓ 已导出: {}{}",
                            out.display(),
                            if overwritten { "（覆盖）" } else { "" }
                        )),
                        Err(write_err) => lines.push(format!("写入失败: {write_err}")),
                    }
                    if open
                        && crate::share::open_in_browser(&out.display().to_string()).is_ok()
                    {
                        lines.push("已在浏览器中打开。".to_string());
                    }
                    lines.push(
                        "注意：此文件包含完整对话与工具输出（可能含敏感信息），分享前请自行审阅。"
                            .to_string(),
                    );
                    let _ = events.send(UiEvent::SlashOutput(lines.join("\n")));
                }
            }
        });
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

    /// Async stats shared by /status and /context: message count + token count.
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
                    // #18/main #91: short-op failures must be visible (page-level error row),
                    // behavior keeps degrading gracefully (budget still shows 0).
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
        let provider = session.runtime.provider.borrow().clone();
        let thinking = session.runtime.thinking.borrow().clone();
        let thinking_shown = thinking.unwrap_or_else(|| "off".to_string());
        let transcript = session.runtime.transcript.borrow().clone();
        let transcript_name = transcript
            .as_ref()
            .map(|t| t.name())
            .unwrap_or_else(|| "无".to_string());
        let mode = session.permission_mode_str().to_string();
        self.slash_stats_async(move |msg_count, tokens| {
            format!(
                "模型: {model}\nProvider: {provider}\n思考级别: {thinking_shown}\n权限模式: {mode}\n会话: {transcript_name}\n消息数: {msg_count}\n上下文: {tokens} tokens / {}（{}%）",
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
            // 列表：default 打头（顶层端点），其后命名 provider 各带 URL
            // （/provider 信息量不足修复：一眼看清每个端点的去向）。
            let mut lines = vec![format!("当前 provider: {current}")];
            let mut names = vec!["default".to_string()];
            names.extend(session.client.provider_names());
            for name in names {
                let (key, url) = session
                    .client
                    .provider_endpoint(&name)
                    .unwrap_or_else(|| ("?".to_string(), "?".to_string()));
                // key 脱敏：仅显示前 4 字符；短 key（≤4）不加省略号。
                let mut key_shown: String = key.chars().take(4).collect();
                if key.chars().count() > 4 {
                    key_shown.push('…');
                }
                let mark = if name == current { "●" } else { " " };
                lines.push(format!("{mark} {name} @ {url}（key {key_shown}）"));
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
                // 与 /model /think 同路径持久化：重启恢复当前 provider。
                let cwd = std::path::PathBuf::from(&self.cwd);
                let _ = crate::settings::upsert_project_settings(
                    &cwd,
                    &serde_json::json!({ "provider": name }),
                );
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

    /// Sets the thinking level (runtime + persisted). Level table = off + THINKING_LEVELS:
    /// off sends no parameter; the rest send adaptive thinking + output_config.effort.
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

    /// Enters the `/think` level selector: preselects the current level (off when unset).
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

    /// Think level menu keys: ↑↓ move (wraps), Enter confirms, Esc exits. Returns whether consumed.
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
        // task_lines is gated by task-area visibility — /tasks explicitly asks for them, so bypass it temporarily.
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

    /// `/team <subcommand>` (D31 project-level formation): dispatched to team_cmd, multi-line output queued at once.
    fn slash_team(&mut self, arg: &str) {
        let lines = crate::team_cmd::run(&self.session, &std::path::PathBuf::from(&self.cwd), arg);
        self.push_slash_output(lines.join("\n"));
    }

    /// Rebuilds the slash dropdown suggestions (called when the input changes):
    /// shown when the input starts with `/` and has no args; an empty query lists everything (built-ins + skills),
    /// otherwise prefix/substring matching (a simplified generateCommandSuggestions).
    fn update_slash_suggestions(&mut self) {
        self.slash_suggestions.clear();
        let input = self.input.trim_end();
        let Some(query) = input.strip_prefix('/') else {
            return;
        };
        if query.contains(char::is_whitespace) {
            return; // has args: do not show
        }
        let mut items: Vec<SlashSuggestion> = SLASH_COMMANDS
            .iter()
            .map(|(name, desc)| SlashSuggestion {
                name: (*name).to_string(),
                description: (*desc).to_string(),
            })
            .collect();
        // Merge skills in (the / menu includes skills). Description truncation:
        // overlong lines do not wrap — the terminal wraps them itself, breaking the frame-height math;
        // capped at MAX_LISTING_DESC_CHARS.
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
            // Prefix matches first (shorter first), then substring matches; built-ins stay ahead.
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

    /// Dropdown key handling: ↑↓ move the selection, Tab completes (without running), Esc closes.
    /// No j/k navigation: while the menu is open, j/k would be typed as input chars (e.g. /thin → think),
    /// swallowing keys and truncating the command. Returns true = consumed.
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

    /// Applies the selected suggestion (applyCommandSuggestion): fills `/name ` back into the input.
    fn apply_slash_suggestion(&mut self) {
        if let Some(s) = self.slash_suggestions.get(self.slash_selected) {
            self.input = format!("/{} ", s.name);
        }
        self.slash_suggestions.clear();
    }

    /// Submits the next queued message after a turn (one at a time: the next turn continues).
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

    /// System-triggered turn: a watchable signal/terminal notification wakes the main agent.
    /// No user input (the notification is injected in run_query's first round); user state is irrelevant.
    fn submit_auto(&mut self) {
        if self.busy {
            return;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        self.start_turn(String::new(), false);
    }

    /// Multi-turn continuity: loads transcript history as this turn's context (each turn runs its own run_query).
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

    /// Post-turn handling: send TurnEnd first (busy resets / the completion row appears immediately),
    /// memory extraction is deferred — it is a non-streaming model call (seconds) and the wrap-up should not block
    /// the turn-end UI; extraction runs fine in parallel with the next turn (e.g. a watch wake-up).
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
        // Subscribe first, then reset: tokio watch's send does not update the value with no receivers —
        // after the previous spawn ends, all receivers are dropped; sending false first would silently
        // fail (the value stays true) and the new turn would be misread as interrupted during connection.
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
                        // Turn-level error = long-turn failure → full-flow full-screen state (AC-53).
                        level: crate::error::ErrorLevel::Full,
                        context: crate::error::ErrorContext::LongTurn,
                    });
                }
            }
        });
    }

    /// bash-mode turn (processBashCommand): `!` commands execute directly,
    /// output shown as a tool activity; with respondToBashCommands on, the model replies afterwards.
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
        // Same as start_turn: subscribe first, then reset (send does not update with no receivers).
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
                        // Turn-level error = long-turn failure → full-flow full-screen state (AC-53).
                        level: crate::error::ErrorLevel::Full,
                        context: crate::error::ErrorContext::LongTurn,
                    });
                }
            }
        });
    }

    /// Dialog key input (Select semantics):
    /// digits/Enter confirm, ↑/↓ move the focus, Esc cancels; typing goes directly when the focus is on Other.
    /// Returns whether it was consumed.
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
    /// AskUserQuestion answer text: header + one `· question → answer` line. Enters the
    /// message flow as an ordinary user message (no longer a transient block rendered above the input).
    fn ask_answer_text(question: &str, answer: &str) -> String {
        format!("User answered the questions:\n  · {question} → {answer}")
    }

    /// Records an answer/decline as an ordinary user message: rendered like user input
    /// (bubble), settled and flushed into scrollback, persistent with the session — no transient residue.
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

    /// Confirms option `index` (0-based; out of range = cancel).
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

    /// Keyboard events. Real-clock version; semantics in [`Chat::on_key_at`].
    pub fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        self.on_key_at(code, modifiers, std::time::Instant::now())
    }

    /// Resets the error state (one of AC-03's four resets: clears the error row / full-screen error state).
    fn dismiss_error(&mut self) {
        self.last_error = None;
    }

    /// #18 full-flow full-screen error-state keys (AC-26/53: the way back is not a dead end):
    /// Enter = retry (reruns the last input), Esc = back, Ctrl+C = quit, the rest ignored.
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

    /// Keyboard events (`now` is injectable: the Ctrl+C double-press window and paste-burst detection both need a clock).
    ///
    /// Priority, top to bottom: dialog → `/model` menu → history search → interrupt/quit semantics
    /// → editing keys. Returns whether it was consumed.
    pub fn on_key_at(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        now: std::time::Instant,
    ) -> bool {
        let pasting = self.track_burst(now);
        // #18 full-flow full-screen error state: primary actions Enter=retry / Esc=back, the rest ignored.
        if let Some(err) = &self.last_error
            && err.level == crate::error::ErrorLevel::Full
        {
            return self.error_screen_key(code, modifiers, now);
        }
        if self.ask_key(code) {
            return true;
        }
        // `/model` `/think` selectors take priority over input (↑↓/Enter/Esc fully consumed).
        if self.model_menu_key(code, modifiers) {
            return true;
        }
        if self.think_menu_key(code, modifiers) {
            return true;
        }
        if self.search.is_some() {
            return self.search_key(code, modifiers);
        }
        // Entity selector (ctrl+g / ↑↓ Enter Esc while focused) precedes the global Esc semantics.
        if self.entity_key(code, modifiers) {
            return true;
        }
        // Interrupt (busy) and quit (idle) both live on Ctrl+C / Esc, judged before editing keys.
        if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
            return self.ctrl_c(now);
        }
        if code == KeyCode::Esc {
            return self.escape(now);
        }
        self.notice = None;
        // Slash dropdown keys (Tab completes / Esc closes / ↑↓ navigate) take priority over input.
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
            // Shift+Tab: cycle the permission mode (CC app:cyclePermissionMode).
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
                // Empty-input backspace in bash mode exits shell mode (CC).
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
            // Shift+Enter (available when the terminal reports enhanced keyboards) and pasted Enter are both newlines.
            KeyCode::Enter
                if pasting
                    || modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                self.insert_newline();
                // Only pasted newlines can pile up large text → fold into a placeholder at the threshold.
                if pasting {
                    self.collapse_paste();
                }
                true
            }
            KeyCode::Enter => {
                // `\` + Enter: the newline every terminal can type (CC).
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
            // `?` on empty input toggles the shortcut panel; with text it is an ordinary character.
            KeyCode::Char('?') if self.input.is_empty() && !self.bash_mode => {
                self.help_visible = !self.help_visible;
                true
            }
            // `!` on empty input enters shell mode (`!` itself never enters the input).
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

    /// Paste-burst detection: [`PASTE_BURST_KEYS`] consecutive key presses with intervals under
    /// [`PASTE_BURST_GAP`] count as a paste (limitations in that constant's comment).
    fn track_burst(&mut self, now: std::time::Instant) -> bool {
        let fast = self
            .last_key_at
            .is_some_and(|last| now.duration_since(last) < PASTE_BURST_GAP);
        self.burst_keys = if fast { self.burst_keys + 1 } else { 0 };
        self.last_key_at = Some(now);
        self.burst_keys >= PASTE_BURST_KEYS
    }

    /// Ctrl+C: interrupts when busy; with text while idle, clears it (into history, retrievable with ↑);
    /// first press on idle empty input shows a hint, a second press within [`CTRL_C_WINDOW`] quits.
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

    /// Esc: interrupts when busy; closes menus/suggestions; double-press with text while idle clears (into history).
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

    /// Interrupts the current turn (Esc / Ctrl+C while busy).
    fn interrupt(&mut self) {
        self.interrupted = true;
        self.cancel_tx.send_replace(true);
    }

    /// Ctrl+<char> editing commands (readline semantics).
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
                // Empty-input ctrl+u in bash mode exits shell mode (CC).
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
            // ctrl+d deletes the char after the caret only when there is text (empty input never quits).
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
                    // Manually opened: keep the panel even when everything is done (the user explicitly wants to see it).
                    self.tasks_auto = false;
                    self.refresh_tasks();
                }
                self.dirty = true;
                true
            }
            // Ctrl+_ arrives as byte 0x1F, which crossterm reports as Ctrl+7; terminals with the enhanced
            // keyboard protocol report `_` or `/` — all three count as undo.
            '7' | '_' | '/' => {
                self.undo_edit();
                true
            }
            _ => false,
        }
    }

    /// Alt+<char>: word movement and the thinking toggle.
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

    /// ↑/↓: move within a multi-line input first, then switch history at the first/last row;
    /// ↑ while busy with a queue pulls back the last queued message.
    fn vertical(&mut self, down: bool) -> bool {
        // Pulling back a queued message only happens on empty input: what is being typed should not be clobbered.
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

    /// Available input width (terminal width - 2 prefix columns - right padding).
    pub fn input_width(&self) -> usize {
        self.width.saturating_sub(4).max(8)
    }

    /// Newline insertion (`\`+Enter / Ctrl+J / Shift+Enter / Enter inside a paste).
    fn insert_newline(&mut self) {
        self.snapshot(EditKind::Bulk);
        crate::tui::input::insert(&mut self.input, &mut self.cursor, "\n");
        self.after_edit();
    }

    /// Replaces the whole input and puts the caret at the end.
    pub fn set_input(&mut self, text: impl Into<String>) {
        self.input = text.into();
        self.cursor = self.input.len();
        self.update_slash_suggestions();
    }

    /// Clears the input and records it in history (Ctrl+C / double Esc: retrievable with ↑).
    fn clear_input_into_history(&mut self) {
        let text = std::mem::take(&mut self.input);
        self.cursor = 0;
        self.undo.clear();
        self.record_history(&text);
        self.update_slash_suggestions();
    }

    /// Wrap-up after every edit: refresh the dropdown suggestions, leave history-browsing mode.
    fn after_edit(&mut self) {
        self.history.detach();
        self.update_slash_suggestions();
    }

    /// Records and persists a prompt. A write failure only degrades to in-session history (once,
    /// no repeated retries).
    fn record_history(&mut self, text: &str) {
        if !self.history.record(text) || !self.history_writable {
            return;
        }
        let path = std::path::PathBuf::from(&self.cwd);
        if crate::tui::history::save(&self.session.home, &path, self.history.entries()).is_err() {
            self.history_writable = false;
        }
    }

    /// Undo stack: consecutive inserts merge into one step; deletes/whole replacements are their own steps.
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

    /// Ctrl+_: returns to the previous step's text and caret.
    fn undo_edit(&mut self) {
        let Some((text, cursor)) = self.undo.pop() else {
            return;
        };
        self.input = text;
        self.cursor = cursor.min(self.input.len());
        self.last_edit = None;
        self.update_slash_suggestions();
    }

    /// Ctrl+S: with text, stash and clear it; on empty input, restore (including the caret).
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
    /// bypassPermissions / dontAsk stay in the cycle only when the session started in that mode
    /// (dangerous modes must not be reachable by one mispress).
    fn cycle_permission_mode(&mut self) {
        self.permission_mode = match self.permission_mode {
            PermissionMode::Default => PermissionMode::AcceptEdits,
            PermissionMode::AcceptEdits => PermissionMode::Plan,
            PermissionMode::Plan => PermissionMode::Default,
            // Started in bypass/dontAsk: toggle between it and default, never introducing a new dangerous mode.
            PermissionMode::BypassPermissions | PermissionMode::DontAsk => {
                PermissionMode::Default
            }
        };
        // From default, switch back to the startup mode (an edge that only bypass/dontAsk sessions have).
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

    /// Alt+T: thinking toggle (off ↔ the last non-off level, default medium).
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

    /// bash-mode Tab: prefix-completes from the `!` commands run in this session.
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

    /// Ctrl+R: enters reverse history search (an empty query hits the most recent entry first).
    fn open_search(&mut self) {
        let mut search = HistorySearch::default();
        if let Some((index, hit)) = self.history.search("", None) {
            search.index = Some(index);
            search.hit = Some(hit);
        }
        self.search = Some(search);
        self.slash_suggestions.clear();
    }

    /// Search-mode keys: typing filters, Ctrl+R takes an older hit, Tab/Esc adopt and keep editing,
    /// Enter adopts and submits, Ctrl+C cancels and restores. Returns consumed (always true).
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

    /// tick: independent timing for spinner frames and running-state thinking.
    ///
    /// Only set dirty when some row changes with the tick: rebuilding the whole document on idle
    /// equals a 30fps full re-layout, wasting CPU and forcing the host to repaint the viewport every frame.
    pub fn tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        if self.has_dynamic_rows() {
            self.dirty = true;
        }
        // The bottom entity area follows the registry (agent states/channel counts); dirty only on change.
        if self.tick.is_multiple_of(15) {
            self.refresh_entities();
        }
        // Slash transient hints expire (operation confirmations leave no permanent placeholder).
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

    /// Whether any row changes with the tick (spinner frames / elapsed time / status rows).
    /// false when idle — the tick neither rebuilds the doc nor wakes the component.
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

    /// Whether the host's tick loop has work to do. Returns false when idle so the host skips the whole frame —
    /// with no animation and no pending events, not a single byte is written.
    pub fn needs_tick(&self) -> bool {
        self.has_dynamic_rows()
            || self.slash_at.is_some()
            || !self.events_rx.is_empty()
            || !self.asks_rx.is_empty()
    }

    /// Task-area data source: live snapshot of the on-disk store.
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

    /// Refreshes the task cache (disk snapshot; called on the tick cadence and after draining events).
    /// Only set dirty when the snapshot changes — row-count changes alter the canvas height, and the render
    /// layer's shape detection triggers a full repaint.
    pub fn refresh_tasks(&mut self) {
        let next = self.tasks();
        if next != self.tasks_cache {
            self.tasks_cache = next;
            self.dirty = true;
        }
        // Auto-opened task area: hide once everything is done (work over, panel leaves),
        // push a 2s transient line for closure + a way back; manually opened panels stay.
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

    /// Keep at most this many trailing done items; older ones fold into `… N done`.
    const DONE_SHOWN: usize = 3;
    /// Active-item window size; overflow folds into `… +N more`.
    const TODO_SHOWN: usize = 5;

    /// Task-area rows (CC TaskListV2 placement: above the input).
    /// Shown when the expand signal is set and tasks exist; auto-opened lists hide when everything is done
    /// (wrapped up in `refresh_tasks`); manually opened ones stay.
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
        // Header: `{spinner}todo · N/M tasks`
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
            // `☒` + struck-through text (real strikethrough + dim, see Theme::strikethrough).
            let mut line = Line::styled("☒ ", theme.task_done());
            line.push_styled(t[idx].text.clone(), theme.strikethrough());
            out.push(line);
        }
        let active: Vec<&TodoItem> = t
            .iter()
            .filter(|i| i.status != TodoStatus::Done)
            .collect();
        for item in active.iter().take(Self::TODO_SHOWN) {
            // `☐` not done; in-progress items use the primary accent color for the whole row (CC's active-item highlight).
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

    /// Permission-mode label (footer badge).
    pub fn permission_mode_label(&self) -> &'static str {
        match self.permission_mode {
            PermissionMode::Default => "default",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::BypassPermissions => "bypassPermissions",
            PermissionMode::DontAsk => "dontAsk",
            PermissionMode::Plan => "plan",
        }
    }

    /// Running status row (ActivityIndicator): when busy, returns the verb + elapsed time + tokens
    /// produced — preferring the running tool (summary/name), then the running
    /// thinking (whimsical word), falling back to "Working". Returns None when idle (row hidden).
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
                    // Running background task/subagent (ActivityIndicator shows the agent activeForm):
                    // the label is `Agent: <description>`.
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

    /// Input-area rendered rows (with the ▋ caret) — the single source for the row-count model and rendering:
    /// chrome height is counted from it and assembly emits rows from it.
    ///
    /// Empty input gets a one-line dim placeholder; multi-line input beyond [`INPUT_ROWS_MAX`] shows only
    /// the screen around the caret (tail-aligned), so the row count always has an upper bound.
    pub fn prompt_lines(&self) -> Vec<Line> {
        let style = SegStyle::fg(self.theme.text);
        // Search mode: the input row shows the current hit; the query sits in the hint line below.
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
                // Draw ▋ at the caret; text after it renders normally.
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
            // Each row must occupy exactly one line: history-filled text may contain tabs (folded to spaces),
            // otherwise the column-width math and canvas height would both drift.
            .map(|mut line| {
                line.sanitize();
                line
            })
            .collect()
    }

    /// Refreshes the bottom entity-area snapshot (agent instances + channels). Dirty only on change.
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
            // Clamp the selection when the list shrinks.
            if let Some(i) = self.entity_focus
                && i >= fresh.len()
            {
                self.entity_focus = fresh.len().checked_sub(1);
            }
            self.entities = fresh;
            self.dirty = true;
        }
    }

    /// Bottom entity area: collapsed = a one-line summary (dim); focused = a per-row list with `❯` selection +
    /// action hints. Takes no rows when there are no entities.
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
        // Keep the selection visible: the window slides around selected.
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

    /// Entity selector keys: ctrl+g toggles focus; while focused, ↑↓ move, Enter opens,
    /// Esc closes. Returns whether consumed.
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

    /// `?` panel rows (single source for the shortcut table). The row budget comes from the terminal height:
    /// the panel must not push the viewport above the terminal height.
    pub fn help_lines(&self) -> Vec<String> {
        if !self.help_visible {
            return Vec::new();
        }
        // Reserve: input 3 rows + footer 1 + a 4-row margin for status/suggestions + 1 safety row.
        let budget = self.height.saturating_sub(9);
        crate::tui::keys::help_lines(self.width.saturating_sub(2), budget)
    }

    /// Queued-message rows (dim `> {text}` below the input); overflow folds into one row.
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

    /// ctrl+r search hint line (`(reverse-i-search)`query': hit`).
    pub fn search_line(&self) -> Option<String> {
        let search = self.search.as_ref()?;
        let hit = search.hit.as_deref().unwrap_or("");
        Some(one_line(
            &format!("(reverse-i-search)`{}': {hit}", search.query),
            self.width.saturating_sub(2),
        ))
    }

    /// Scroll/doc consistency: clamp the scroll to the doc end; auto_scroll sticks to the bottom.
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

    /// A message's own static settlement condition (independent of predecessors):
    /// streaming stopped, no running activities, no images loading.
    fn message_static_settled(&self, i: usize) -> bool {
        if Some(i) == self.stream_msg {
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

    /// Whether a message is "settled": its rows no longer change (stream stopped, no running activities).
    /// REPL mode: settled messages print into scrollback in one go; unsettled ones stay in the
    /// dynamic tail for in-place redraws. Settling is one-way — once true, the rows never change.
    ///
    /// Sequential settlement: an answer message inserted mid-turn sits after the streaming
    /// assistant message; if a predecessor isn't settled (still streaming / tool running /
    /// image loading), this message must not settle either — flushing past a streaming row
    /// would print an intermediate state into scrollback as unchangeable residue (same
    /// invariant as `streaming_content_is_not_flushed_until_settled`; today's message model
    /// always has predecessors settled, this guard only constrains new scenarios).
    ///
    /// Prefix settlement is monotone (0..=i all settled ⟺ 0..i-1 all settled and i itself
    /// static), so recursing from the previous message is linear — do NOT recurse into every
    /// predecessor: with everything settled that is exponential (freezes the hot path on
    /// every build_rows).
    fn message_settled(&self, i: usize) -> bool {
        (i == 0 || self.message_settled(i - 1)) && self.message_static_settled(i)
    }

    /// 构建滚动文档：欢迎卡片 + 消息（text 与活动按插入点交错）+
    /// 权限请求块。`doc.settled` = 前置定稿行数（欢迎卡片 + 全部
    /// 已定稿消息；权限请求块永远不定稿）。
    ///
    /// In inline mode, segments already flushed ([`Chat::flushed_segments`]) are skipped wholesale:
    /// the doc only covers the dynamic tail, so more flushing means cheaper rebuilds.
    pub fn build_rows(&mut self, width: usize) -> &Doc {
        // The markdown render cache is not width-aware — clear it when the width changes,
        // otherwise message text keeps wrapping at the old width after a resize.
        if self.prev_build_width != width {
            self.prev_build_width = width;
            self.reply_cache.clear();
        }
        let mut rows: Vec<Row> = Vec::new();
        let mut click_ranges: Vec<ClickRange> = Vec::new();
        let theme = self.theme.clone();
        // Segment numbering: 0 = welcome card, i+1 = messages[i]. The clamp is defensive: if the message set
        // is replaced wholesale (/clear, /resume) without the cursor resetting, better to re-render
        // than leave a blank screen.
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
        // Message block spacing (CC marginTop=1): one blank row after the welcome card and before each message.
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
                    // Markdown render closure: borrows only disjoint fields to avoid conflicting with
                    // the shared read borrow of `self.messages`.
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
                            // Image cache version changed → sync the renderer (clears its per-block cache).
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
                            // Collapse group: a one-line rule summary (`Read 3 files (ctrl+o to expand)`).
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
                            // The group row is a static `⏺ …`: the spinner only lives in the bottom status row.
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
                            // Below a running collapse group, show the most recent tool's input (the CC ⎿ row).
                            // The hint may be a multi-line bash command: single-line it and truncate by width,
                            // otherwise the row balloons into multiple lines and the row model drifts from the canvas.
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
                        // Expanded group: the group-head tool row is also the summary row's spot — clicking it collapses back.
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
                    // Thinking completion row (CC SystemTextMessage `✻ Churned for 40s`):
                    // rendered at the end of the message (after text and all tools), from the last completed
                    // real thinking block (empty placeholder blocks produce no completion row).
                    // Only rendered after the turn ends: while running, `✻ Baked for 0.4s` would appear
                    // while tools are still running, contradicting the bottom running-status row.
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

        // Permission/ask block (PermissionDialog / AskUserQuestion):
        // title (permission bold) + description (dim) + numbered options (Select:
        // `❯ n. label` focus marker, desc sub-row dim, Other free input) + shortcut hints.
        if let Some((request, _)) = &self.pending_ask {
            let mut title = Line::styled("⏺ ", SegStyle::fg(theme.text));
            title.push_styled(request.title.clone(), theme.permission());
            rows.push(Row::new(title));
            rows.push(Row::new(Line::styled(
                format!("  {}", request.question),
                SegStyle::fg(theme.text),
            )));
            // CC Select: one blank row between the question and the options.
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

        // Slash command output (/help /status /compact etc.): transient hints — rendered after messages and
        // above the input, **never settled or flushed**, auto-dismissed after the tick timeout (SLASH_OUTPUT_TTL).
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

    /// Resets the flush cursor: after the message set is replaced wholesale (/clear, /resume), segment numbers
    /// are invalid, so the doc rebuilds from the welcome card (new content flushes into scrollback again).
    fn reset_flushed(&mut self) {
        self.flushed_segments = 0;
        self.tail_start = 0;
        self.mark_base = 0;
        self.dirty = true;
    }

    /// After flushing `doc.rows[tail_start..settled]`, advance the cursor: the next rebuild skips
    /// those segments and the current doc's tail start moves up (the canvas stops drawing them before a rebuild).
    // Production advances partially by checkpoints (lazy flush); full advance stays as a test-facing primitive.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn advance_flushed(&mut self) {
        if let Some(mark) = self.doc.settled_marks.last().copied() {
            self.advance_flushed_upto(mark);
        }
    }

    /// After flushing `doc.rows[tail_start..mark.row_end]`, advance the cursor to that checkpoint.
    /// Callable multiple times within one build (`mark_base` absorbs the build-internal accumulators,
    /// preventing double-counting); safe across width-change re-layouts — segment counts are row-number independent.
    pub fn advance_flushed_upto(&mut self, mark: SettledMark) {
        self.flushed_segments += mark.segments.saturating_sub(self.mark_base);
        self.mark_base = mark.segments;
        self.tail_start = mark.row_end;
    }

    /// After a resize the window can hold more: pull the most recently flushed content back into the live doc
    /// to refill it. Old copies in scrollback cannot be physically retracted — accept seeing a duplicate
    /// at the old width when scrolling up (an explicitly accepted trade-off, see research.md D27). Rehydration is
    /// purely bookkeeping (writes nothing to the terminal), bounded by "no more than `doc_budget` rows"; beyond that it rolls back,
    /// guaranteeing no conflict with lazy flushing (after rehydration no settled segment crosses the window top).
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

/// User message rows: a `❯ ` prefix + body wrapped to the width (multi-line pasted messages split into rows).
/// One bubble Row per line — stuffing the whole message into a single height=1 View would clip
/// everything after the first newline and detach the canvas height from the row model.
fn user_message_rows(text: &str, width: usize, theme: &Theme) -> Vec<Row> {
    // 2 prefix columns + 1 column of right padding inside the bubble.
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

/// Single-line + truncate: summary/hint text may contain newlines (multi-line bash commands),
/// while every Row must be exactly one line.
pub(crate) fn one_line(text: &str, width: usize) -> String {
    let flat = crate::tui::line::sanitize(text);
    crate::tui::markdown::truncate(flat.as_ref(), width.max(1))
}

/// Text segment folding: segments >2 lines fold into the first 2 lines + a hint (CC `… +N lines`).
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

/// Welcome card rows (with the ╭╮ border), part of the scrollable content.
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

    /// Test Chat: independent channels + a full Session.
    pub(super) fn test_chat() -> Chat {
        test_chat_home(std::env::temp_dir())
    }

    /// Segments covered by the latest settled checkpoint (checkpoint-equivalent read of the old aggregate field).
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

    /// Simulates the component layer: build_rows + scroll + viewport slice → visible text.
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

    /// start_group + tool completion (with explicit summaries, like the old build_group_chat(true)).
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

    /// Task-family / AskUserQuestion calls are not shown in the transcript
    /// (renderToolUseMessage = null; the task panel / dialog shows them).
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
        // Visible tools still render normally.
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
        // The TUI task area's data source = live snapshot of the disk store (the data layer of the tick broadcast chain).
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

    /// Creates a task and returns its id (writes to the temp store).
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

    /// Auto-opened task area (TaskCreate signal semantics): all done → hide + transient line;
    /// new task → reappears; all done again → hides again; once hidden, idle writes nothing.
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

        // Create another task (the expand signal reopens the panel) → reappears; all done again → hides again.
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

    /// Panel opened manually with ctrl+t: kept even when everything is done (the user explicitly wants to see it), no transient line.
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

    /// `/tasks` explicit request: outputs the ☒ list even when everything is done, never falsely reports "no background tasks".
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

    /// Running status-row data (ActivityIndicator): None when idle;
    /// when busy, prefer the running tool's summary, then a thinking word, fall back to Working.
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

        // A running Watch (subagent/background task) verb = its label (CC ActivityIndicator
        // shows the agent activeForm): after tools, before thinking.
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

        // A Done Watch no longer claims the verb (falls through to thinking/Working).
        if let ActivityKind::Watch(w) = &mut chat.messages[0].activities[0].kind {
            w.status = WatchStatus::Done;
        }
        let verb = chat.running_status().expect("busy status").verb;
        assert_ne!(verb, "Agent: 列出桌面目录内容", "Done 的 Watch 不占动词");

        chat.messages[0].activities.clear();
        chat.apply_turn_start();
        // TurnStart appends a new message (index 1): the placeholder thinking lives there.
        let stage = match &chat.messages[1].activities[0].kind {
            ActivityKind::Thinking(t) => t.stage,
            _ => unreachable!(),
        };
        let verb = chat.running_status().expect("busy status").verb;
        assert_eq!(verb, stage, "thinking 俏皮词");
    }

    /// bash-mode toggle: `!` on empty input enters, `!` never enters the input,
    /// `!` inserts normally when the input is non-empty, backspace on empty input exits.
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

    /// `!` commands (standalone tool activity): not part of collapse groups, expanded by default when done,
    /// preview = the output itself (stripped of the `$ cmd` echo and the `[Exited with code N]` footnote).
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

    /// Model-driven Bash (standalone=false) still folds into a group as before.
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

    /// bash-mode submit: the user message carries the `!` prefix, the command runs as a tool activity and finishes normally
    /// (respondToBashCommands=false → no model call; the turn ends and busy resets).
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

    /// Thinking between tool rounds merges into one block when text has not interrupted (segments split by blank lines),
    /// with later deltas continuing into the merged block.
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

    /// Thinking after text interrupts opens a new block, no longer merging.
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

    /// The thinking completion row (CC SystemTextMessage `✻ Churned for 40s`) renders at the end of the message:
    /// after text and all activities; empty placeholder thinking (no content) produces no completion row.
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

        // Empty placeholder thinking (no content) → no completion row.
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

    /// The completion row only appears after the turn ends: with thinking Done but tools still running,
    /// `✻ Baked for 0.4s` is not rendered, avoiding a contradiction with the bottom running-status row.
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

    /// Consecutive deltas within one turn continue the same block.
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

    /// Interleaved rendering: text and activities cross by insert point (model output in text → tool → text order).
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
    // Slash commands (/help /model /clear /exit /theme /rename /resume
    // /permissions /skills /tasks /compact)
    // ------------------------------------------------------------------

    /// Input-layer interception: a leading / never starts a turn; /help lists commands, unknown ones get a hint.
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

    /// /model: with an arg, switch the runtime model (effective next turn) and persist as default; without, open the selector.
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

    /// /exit sets the quit flag (component layer consumes → system.exit).
    #[test]
    fn slash_exit_requests_shutdown() {
        let mut chat = test_chat();
        chat.input = "/exit".to_string();
        chat.submit();
        assert!(chat.exit);
    }

    /// /clear: clears the UI messages and swaps in a new transcript (task keys stay per-session; M0 does not follow).
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

    /// /theme: rebuilds the theme (dark → light render difference) + persists to .bingo/settings.json.
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

    /// /rename: renames the transcript file and updates the runtime reference.
    #[test]
    fn slash_rename_renames_transcript() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-rename", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        let t = crate::transcript::create(&home, &tmp).unwrap();
        // create only makes the directory; drop a message first so the file exists.
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

    /// /resume: without args, list all sessions; with an arg, switch the runtime transcript by keyword.
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

    /// /share --local：导出当前会话 HTML 分享页（文件存在、路径输出、覆盖提示）。
    #[test]
    fn slash_share_exports_current_session() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-share", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        let t = crate::transcript::create(&home, &tmp).unwrap_or_else(|e| panic!("{e}"));
        let _ = t.append(&crate::api::types::Message::user_text("hi"));
        let mut chat = test_chat_home(home.clone());
        let _ = chat.session.runtime.transcript_tx.send(Some(t));
        chat.input = "/share --local".to_string();
        chat.submit();
        let stem = chat.session.runtime.transcript.borrow().clone().unwrap().name();
        let joined = chat.slash_lines.join("\n");
        assert!(joined.contains("已导出"), "{joined}");
        assert!(joined.contains(&stem), "路径含 stem: {joined}");
        assert!(joined.contains("注意：此文件包含完整对话"), "隐私警告");
        // 输出目录 = chat.cwd（test_chat_home 设为 home）。
        let out = home.join(format!("{stem}.html"));
        assert!(out.exists(), "产物存在: {}", out.display());
        let html = std::fs::read_to_string(&out).unwrap_or_else(|e| panic!("{e}"));
        assert!(html.contains("hi"), "产物含消息文本");
        assert!(html.contains("data-view=\"conv\""), "产物为 share 页");
        // 二次导出 → 覆盖提示。
        chat.input = "/share --local".to_string();
        chat.submit();
        assert!(
            chat.slash_lines.join("\n").contains("覆盖"),
            "覆盖提示: {}",
            chat.slash_lines.join("\n")
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// /share：无 transcript（新会话未落盘）时提示不可导出。
    #[test]
    fn slash_share_without_transcript_hints() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-noshare", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mut chat = test_chat_home(tmp.join("home"));
        chat.input = "/share".to_string();
        chat.submit();
        assert!(
            chat.slash_lines.join("\n").contains("尚无会话可导出"),
            "{}",
            chat.slash_lines.join("\n")
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// /share 参数解析（纯逻辑，不触发浏览器/上传）。
    #[test]
    fn parse_share_arg_flags() {
        assert!(parse_share_arg("--open", "--open"));
        assert!(parse_share_arg("--local --open", "--open"));
        assert!(parse_share_arg("  --local  ", "--local"));
        assert!(!parse_share_arg("", "--open"));
        assert!(!parse_share_arg("--local", "--open"));
        assert!(!parse_share_arg("--output x", "--local"));
    }

    /// /share 默认上传模式：mock 服务器接收 POST，输出公网链接 + 公开提示。
    /// 上传为异步（tokio::spawn + UiEvent::SlashOutput），断言前 drain 事件。
    #[tokio::test]
    async fn slash_share_uploads_by_default() {
        use std::io::{BufRead, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if line.to_ascii_lowercase().starts_with("content-length:") {
                    content_length = line
                        .split_once(':')
                        .map(|(_, v)| v.trim().parse().unwrap_or(0))
                        .unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            reader.read_exact(&mut body).unwrap();
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            (request_line, String::from_utf8(body).unwrap())
        });
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-upshare", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        // settings.share.baseUrl → 本地 mock 服务器。
        std::fs::create_dir_all(home.join(".config/bingo")).unwrap();
        std::fs::write(
            home.join(".config/bingo/settings.json"),
            format!("{{\"share\": {{\"baseUrl\": \"http://{addr}\"}}}}"),
        )
        .unwrap();
        let t = crate::transcript::create(&home, &tmp).unwrap_or_else(|e| panic!("{e}"));
        let _ = t.append(&crate::api::types::Message::user_text("hi"));
        let mut chat = test_chat_home(home.clone());
        let _ = chat.session.runtime.transcript_tx.send(Some(t));
        chat.input = "/share".to_string();
        chat.submit();
        // current_thread runtime 下 spawn 任务需让出才能执行；sleep 轮询
        // 直到 mock 服务器线程完成（收到请求并回应），并行负载下也稳定。
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while !handle.is_finished() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(handle.is_finished(), "mock 服务器未收到上传请求");
        let (request_line, body) = handle.join().unwrap();
        chat.drain_events();
        let joined = chat.slash_lines.join("\n");
        assert!(joined.contains("已发布"), "{joined}");
        assert!(joined.contains(&format!("http://{addr}/share/u/")), "{joined}");
        assert!(joined.contains("任何人可公开访问此链接"), "{joined}");
        assert!(request_line.starts_with("POST /share/u/"), "{request_line}");
        assert!(body.contains("hi"), "上传 body 为完整 HTML");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// /share --local：保留本地文件模式（不触发上传）。
    #[test]
    fn slash_share_local_keeps_file() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-locshare", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        let t = crate::transcript::create(&home, &tmp).unwrap_or_else(|e| panic!("{e}"));
        let _ = t.append(&crate::api::types::Message::user_text("hi"));
        let mut chat = test_chat_home(home.clone());
        let _ = chat.session.runtime.transcript_tx.send(Some(t));
        chat.input = "/share --local".to_string();
        chat.submit();
        let joined = chat.slash_lines.join("\n");
        assert!(joined.contains("已导出"), "{joined}");
        assert!(!joined.contains("已发布"), "本地模式不上传");
        let stem = chat.session.runtime.transcript.borrow().clone().unwrap().name();
        assert!(home.join(format!("{stem}.html")).exists(), "本地文件存在");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// /permissions: lists rules; adding a rule → runtime table + settings.json persistence.
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

    /// /skills: loads and lists the project-level skills directory.
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

    /// /tasks: lists the task area (Todo list). Uses a dedicated home to avoid polluting the shared test store.
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

    /// Slash output is transient: rendered after messages and above the input, never settled (not flushed).
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

        // After the TTL, a tick clears it: the transient hint disappears.
        chat.slash_at = Some(std::time::Instant::now() - SLASH_OUTPUT_TTL - std::time::Duration::from_millis(1));
        chat.tick();
        assert!(chat.slash_lines.is_empty(), "超时后 slash 输出消失");
        assert!(chat.slash_at.is_none());
    }

    /// Built-in/disk skills submit a `✦ <skill name> [args]` marker via `/skill-name` (progressive disclosure;
    /// the model reads the full body on demand via the Skill tool + Read, never into the context).
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

    /// Unknown slash commands still point to /help (no mis-consumption when the skill name does not match).
    #[test]
    fn slash_unknown_still_guides() {
        let mut chat = test_chat();
        chat.input = "/nope-skill".to_string();
        chat.submit();
        let joined = chat.slash_lines.join("\n");
        assert!(joined.contains("未知命令: /nope-skill"), "{joined}");
        assert!(chat.messages.is_empty(), "未知命令不启动回合");
    }

    /// P1-E：/provider 列表 key 脱敏——短 key（≤4 字符）不追加省略号。
    #[test]
    fn slash_provider_list_masks_short_keys() {
        let mut chat = test_chat();
        let settings = crate::settings::Settings {
            api_key: Some("main".into()),
            ..Default::default()
        };
        Arc::get_mut(&mut chat.session).unwrap().client =
            crate::api::client::Client::from_settings(&settings).unwrap();
        chat.input = "/provider".to_string();
        chat.submit();
        let out = chat.slash_lines.join("\n");
        assert!(out.contains("default @ https://api.anthropic.com"), "{out}");
        assert!(out.contains("（key main）"), "短 key 无省略号: {out}");
        assert!(!out.contains("main…"), "{out}");
    }

    #[test]
    fn slash_provider_lists_and_switches() {
        let mut chat = test_chat();
        chat.input = "/provider".to_string();
        chat.submit();
        let out = chat.slash_lines.join("\n");
        assert!(out.contains("当前 provider: default"), "{out}");

        // Configure a named provider, then switch.
        let providers = std::collections::HashMap::from([(
            "deepseek".to_string(),
            crate::settings::ProviderConfig {
                api_key: "sk-ds".into(),
                api_base_url: "https://api.deepseek.com".into(),
                supports_images: None,
            },
        )]);
        Arc::get_mut(&mut chat.session).unwrap().client =
            crate::api::client::Client::new("sk-main".into(), "https://main.example".into());
        // set_provider needs a providers table — constructing via from_settings is more direct.
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

        // No arg → open the level selector (preselects off = first item).
        chat.input = "/think".to_string();
        chat.submit();
        let menu = chat.think_menu.as_ref().expect("菜单已打开");
        assert_eq!(THINK_LEVELS[menu.selected].0, "off", "未设置时预选 off");
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(chat.think_menu.is_none(), "Esc 退出菜单");

        // New level xhigh: runtime effect + persistence.
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
    // /mcp: list / enable|disable (persisted list) / reconnect
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
        // Persisted to .bingo/settings.json
        let saved: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.join(".bingo/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["disabledMcpServers"], serde_json::json!(["files"]));
        // The list shows disabled
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
        // Reconnect a failing server: the failure detail shows through
        chat.input = "/mcp reconnect files".to_string();
        chat.submit();
        let out = slash_mcp_wait(&mut chat).await;
        assert!(out.contains("files"), "{out}");
        assert!(out.contains("握手失败") || out.contains("✗"), "{out}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ------------------------------------------------------------------
    // Slash dropdown suggestions (pop up on /; Tab completes / ↑↓ navigate / Enter runs / Esc closes)
    // ------------------------------------------------------------------

    // ------------------------------------------------------------------
    // Slash dropdown suggestions (pop up on /; Tab completes / ↑↓ navigate / Enter runs / Esc closes)
    // ------------------------------------------------------------------

    /// Typing `/` → suggestions list the built-in commands; gone once args follow.
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

    /// Prefix filtering + skills merged in (project-level skills directory).
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

        // Overlong descriptions are truncated (MAX_LISTING_DESC_CHARS):
        // a NoWrap overlong row would push the canvas past the terminal width → stale diff residue.
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

    /// ↑/↓ move the selection (keys consumed, no scroll); Tab completes `/name ` without running it.
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

        // Tab applies the selection (/help) → `/help ` with suggestions cleared and nothing run.
        chat.input = "/".to_string();
        chat.update_slash_suggestions();
        chat.slash_selected = 0;
        assert!(chat.slash_menu_key(KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(chat.input, "/help ");
        assert!(chat.slash_suggestions.is_empty());
        assert!(chat.slash_lines.is_empty(), "Tab 不执行");

        // Esc closes.
        chat.input = "/".to_string();
        chat.update_slash_suggestions();
        assert!(chat.slash_menu_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(chat.slash_suggestions.is_empty());
    }

    /// Enter: partial prefix → apply the selection and run; full command → run as-is.
    #[tokio::test]
    async fn slash_menu_enter_applies_and_executes() {
        let mut chat = test_chat();
        // Full command: run directly; the suggestion menu must close (no leftover placeholder row).
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
        // Esc exits the menu.
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(chat.model_menu.is_none(), "Esc 退出菜单");

        // Partial prefix `/sta`: Enter applies the selection (status first) and runs it.
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

    /// `/model` two-level selector: Enter opens the menu (level-one endpoint list),
    /// move the selection → Enter goes to level two (loading) → Esc exits level by level.
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
        // Enter goes to level two: async fetch in progress (loading).
        assert!(chat.on_key(KeyCode::Enter, KeyModifiers::empty()));
        let m = &chat.model_menu.as_ref().unwrap().models;
        assert!(m.is_some(), "已进入二级");
        assert!(m.as_ref().unwrap().loading, "拉取中");
        // Esc returns level by level: two → one → exit.
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(
            chat.model_menu.as_ref().is_some_and(|m| m.models.is_none()),
            "二级 Esc 回一级"
        );
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(chat.model_menu.is_none(), "一级 Esc 整体退出");
    }

    /// Level-two confirm: the model is picked → switch the runtime model and exit the menu.
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

    /// 多 provider 时二级 Esc 回一级：一级 provider 列表与选中必须保留
    /// （回归：open_model_models 曾把 providers 重建为单元素，Esc 后列表丢失）。
    #[tokio::test]
    async fn model_menu_esc_back_keeps_provider_list() {
        let home =
            std::env::temp_dir().join(format!("bingo-model-esc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let mut chat = test_chat_home(home.clone());
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            ..Default::default()
        };
        for (name, key, url) in [
            ("deepseek", "sk-ds", "https://api.deepseek.com"),
            ("local", "sk-local", "http://127.0.0.1:11434"),
        ] {
            settings.providers.insert(
                name.to_string(),
                crate::settings::ProviderConfig {
                    api_key: key.into(),
                    api_base_url: url.into(),
                    supports_images: None,
                },
            );
        }
        Arc::get_mut(&mut chat.session).unwrap().client =
            crate::api::client::Client::from_settings(&settings).unwrap();

        chat.input = "/model".to_string();
        chat.submit();
        let providers = chat.model_menu.as_ref().unwrap().providers.clone();
        assert_eq!(providers, vec!["default", "deepseek", "local"]);
        assert_eq!(chat.model_menu.as_ref().unwrap().provider_selected, 0);

        // ↓ 两次选中 local → Enter 进二级（loading）→ Esc 回一级。
        chat.on_key(KeyCode::Down, KeyModifiers::empty());
        chat.on_key(KeyCode::Down, KeyModifiers::empty());
        assert_eq!(chat.model_menu.as_ref().unwrap().provider_selected, 2);
        chat.on_key(KeyCode::Enter, KeyModifiers::empty());
        assert!(chat.model_menu.as_ref().unwrap().models.is_some(), "进入二级");
        chat.on_key(KeyCode::Esc, KeyModifiers::empty());
        let menu = chat.model_menu.as_ref().expect("一级仍在");
        assert_eq!(menu.providers, vec!["default", "deepseek", "local"], "列表保留");
        assert_eq!(menu.provider_selected, 2, "选中保留");
        assert!(menu.models.is_none(), "回到一级");

        let _ = std::fs::remove_dir_all(&home);
    }

    /// P0-A：/provider <名> 持久化 provider；/model 菜单确认时
    /// provider + model 一并写入 `.bingo/settings.json`（重启恢复同端点模型）。
    #[tokio::test]
    async fn provider_switch_persists_provider_and_model_menu_persists_both() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-provpersist", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mut chat = test_chat_home(tmp.join("home"));
        chat.cwd = tmp.display().to_string();
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

        // /provider deepseek：切换 + 持久化。
        chat.input = "/provider deepseek".to_string();
        chat.submit();
        let saved: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.join(".bingo/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["provider"], "deepseek", "provider 持久化");
        assert_eq!(*chat.session.runtime.provider.borrow(), "deepseek");

        // /model 菜单：当前 provider=deepseek（预选）→ 二级确认模型
        // → provider + model 一并持久化。
        chat.input = "/model".to_string();
        chat.submit();
        assert_eq!(
            chat.model_menu.as_ref().unwrap().provider_selected,
            1,
            "一级预选当前 provider"
        );
        chat.on_key(KeyCode::Enter, KeyModifiers::empty());
        if let Some(m) = &mut chat.model_menu.as_mut().unwrap().models {
            m.models = vec!["deepseek-v4".to_string()];
            m.loading = false;
        }
        chat.on_key(KeyCode::Enter, KeyModifiers::empty());
        assert!(chat.model_menu.is_none(), "确认后关闭菜单");
        let saved: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.join(".bingo/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["model"], "deepseek-v4", "模型持久化");
        assert_eq!(saved["provider"], "deepseek", "provider 随模型一并持久化");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// P1-E：/model <名> 直接设置——有缓存且未命中 → 提示不阻塞；
    /// 无缓存/未拉过 → 直接切换不提示。
    #[test]
    fn slash_model_validates_against_cached_list() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-modelval", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mut chat = test_chat_home(tmp.clone());

        // 无缓存：直接切换，无校验提示。
        chat.input = "/model custom-new".to_string();
        chat.submit();
        let out = chat.slash_lines.join("\n");
        assert!(out.contains("✓ 模型已切换: custom-new"), "{out}");
        assert!(!out.contains("不在"), "{out}");

        // 当前 provider 有缓存且模型不在其中：成功提示内附一句 ⚠ 提示，
        // 单行输出（advisory 不阻塞，仍切换）。
        chat.slash_lines.clear();
        chat.models_cache.insert(
            "default".to_string(),
            vec!["claude-sonnet-5".to_string(), "deepseek-v4".to_string()],
        );
        chat.input = "/model unknown-xyz".to_string();
        chat.submit();
        let out = chat.slash_lines.join("\n");
        assert!(
            out.contains("✓ 模型已切换: unknown-xyz（⚠ 不在 default 已知列表"),
            "{out}"
        );
        assert_eq!(out.lines().count(), 1, "单行输出，⚠ 与 ✓ 不并存");
        assert_eq!(*chat.session.runtime.model.borrow(), "unknown-xyz");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// P1-F：ModelsLoaded 后当前 provider 的当前模型被预选（与 /think 菜单
    /// 预选当前档位对等），避免浏览即误切；当前模型不在列表时回 0；结果
    /// 写入 models_cache（/model <名> 校验用）。
    #[tokio::test]
    async fn models_loaded_preselects_current_model_and_caches() {
        let mut chat = test_chat();
        chat.input = "/model".to_string();
        chat.submit();
        chat.on_key(KeyCode::Enter, KeyModifiers::empty());
        // 当前 provider=default，当前模型=test-model（test_chat 初始值）。
        chat.handle(UiEvent::ModelsLoaded {
            provider: "default".into(),
            models: vec!["m0".into(), "test-model".into(), "m2".into()],
        });
        let m = chat.model_menu.as_ref().unwrap().models.as_ref().unwrap();
        assert_eq!(m.selected, 1, "预选当前模型");
        assert_eq!(m.models[m.selected], "test-model");
        assert_eq!(
            chat.models_cache.get("default").map(Vec::as_slice),
            Some(&["m0".to_string(), "test-model".to_string(), "m2".to_string()][..]),
            "加载结果入缓存"
        );

        // 当前模型不在列表中：选中回 0。
        chat.handle(UiEvent::ModelsLoaded {
            provider: "default".into(),
            models: vec!["m0".into(), "m1".into()],
        });
        let m = chat.model_menu.as_ref().unwrap().models.as_ref().unwrap();
        assert_eq!(m.selected, 0, "未命中回 0");
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
        // ↑ to medium, Enter confirms: runtime effect + persistence + menu closes.
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
        // Reopen the menu: Esc exits directly; off clears the level.
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

    /// THINK_LEVELS (selector) matches the API layer's THINKING_LEVELS: off + all levels, in the same order.
    #[test]
    fn think_levels_match_api_levels() {
        assert_eq!(THINK_LEVELS[0].0, "off");
        let menu: Vec<&str> = THINK_LEVELS[1..].iter().map(|(n, _)| *n).collect();
        assert_eq!(menu, crate::api::types::THINKING_LEVELS.to_vec());
    }

    /// Footer badge: shows `model · think level` when a level is set; off shows only the model name.
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
    // Collapse classification & summaries (formerly fold_tests)
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
    // Collapse rendering (formerly fold_render_tests / fold_toggle_tests / part of live)
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
        // Regression: a tool right after the TurnStart placeholder thinking — group_of must stay in sync with activities.
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
        // Clicking the group fold row expands
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
        // ctrl+o collapses back
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
        // After completion, duration uses the real value
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
        // CC two-line form: elapsed time merges into the result row, and only slow commands (>2s) show it.
        // Skill uses the ✦ icon (category icons: ⏺ built-in / ◆ MCP / ✦ Skill).
        assert!(joined.contains("✦ Skill(pdf doc.md)"), "头行: {joined}");
        assert!(joined.contains("✦ pdf"), "结果行只显示 ✦ 技能名: {joined}");
        assert!(
            !joined.contains("read /tmp/skills/SKILL.md"),
            "指针路径不进 TUI 结果行: {joined}"
        );
        assert!(joined.contains("Ran in 3.2s"), "结果行带耗时: {joined}");
        assert!(!joined.contains("3210ms"), "毫秒不再进头行: {joined}");
    }

    /// Agent aligns with Task renderToolUseMessage=null: ToolStart creates no tool activity row,
    /// the message area is carried solely by the Watch progress row (the only display, updated in place).
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

        // The Watch activity row is created normally (the only Agent display).
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

        // Later events with the same label update in place, creating no new row.
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

    /// Test watchable: state always Running.
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

    /// Collapse groups are bounded by text: neither RoundEnd (model rounds) nor thinking splits them,
    /// tools across rounds merge into one group; only text (TextDelta) opens a new one.
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
        // Text appears: the group closes and later tools open a new one.
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

    /// User messages with newlines (multi-line pastes) must split into single-line Rows: a Row always
    /// occupies one line; mixing in newlines would detach the row model from the actual viewport height.
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
        // Continuation lines align with indentation, never repeating the prefix.
        assert!(bubbles[1].line.plain_text().starts_with("  second line"));
    }

    /// Overlong (newline-free) user messages wrap to the terminal width instead of spilling off screen.
    #[test]
    fn long_user_message_wraps_to_width() {
        let mut chat = test_chat();
        let text = "word ".repeat(40);
        chat.messages.push(msg(Role::User, text.trim()));
        chat.build_rows(30);
        let bubbles: Vec<&Row> = chat.doc.rows.iter().filter(|r| r.bg.is_some()).collect();
        assert!(bubbles.len() > 1, "长消息折成多行");
        for row in bubbles {
            // 2 prefix columns + body ≤ width-1 (1 column of right padding inside the bubble).
            assert!(
                text_width(&row.line.plain_text()) <= 29,
                "行宽超限: {:?}",
                row.line.plain_text()
            );
        }
    }

    /// A collapse group's `⎿ hint` row may hold a multi-line bash command: it must be single-lined + truncated.
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

    /// The flush cursor counts by message boundary: re-layout after a width change (all row numbers change) never re-flushes.
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

        // Rebuild after a width change: already-flushed segments no longer appear in the doc.
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

        // A new message only builds its own segment.
        chat.messages.push(msg(Role::User, "第二条"));
        chat.build_rows(40);
        assert!(
            chat.doc.rows.iter().any(|r| r.line.plain_text().contains("第二条")),
            "新消息进入文档"
        );
        assert_eq!(settled_segments(&chat), 1, "只新增 1 段");
    }

    /// Streaming (unsettled) content is not flushed: a full markdown re-parse rewrites earlier rows,
    /// which would be frozen in scrollback as an unchangeable intermediate state.
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

    /// `/clear` (and `/resume`) replace the message set wholesale → segment numbers become invalid, so the flush
    /// cursor must reset, otherwise the new session's doc is skipped wholesale (blank screen).
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

    /// 顺序守卫必须是线性的：全量定稿（数百条消息）下 build_rows 的
    /// 定稿判定不得指数爆炸（回归：逐前缀递归求值在 ~40 条时即卡死）。
    #[test]
    fn message_settled_guard_is_linear_for_large_settled_sessions() {
        let mut chat = test_chat();
        for _ in 0..400 {
            chat.messages.push(msg(Role::User, "hi"));
            chat.messages.push(msg(Role::Assistant, "ok"));
        }
        // 全量静态定稿：build_rows 会对每条消息做定稿判定。
        chat.build_rows(80);
        assert_eq!(chat.doc.settled, chat.doc.rows.len(), "全部定稿");
        for i in 0..chat.messages.len() {
            assert!(chat.message_settled(i), "消息 {i} 定稿");
        }
    }

    /// Simulates the inline component's flush loop: rebuild → flush the settled prefix → advance the cursor.
    fn flush_frame(chat: &mut Chat, width: usize, printed: &mut Vec<String>) {
        chat.build_rows(width);
        if chat.doc.settled > chat.tail_start {
            for row in &chat.doc.rows[chat.tail_start..chat.doc.settled] {
                printed.push(row.line.plain_text());
            }
            chat.advance_flushed();
        }
    }

    /// Full-flow regression: streaming + mid-turn resize + settling — no row in the scrollback
    /// is ever repeated (the old row-number cursor re-printed everything after a resize re-layout).
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
        // Mid-turn resize: all row numbers change after re-layout.
        flush_frame(&mut chat, 60, &mut printed);
        chat.handle(UiEvent::TextDelta("结尾。".into()));
        chat.handle(UiEvent::TurnEnd);
        flush_frame(&mut chat, 60, &mut printed);
        // Idling a few frames must print nothing more.
        let after = printed.len();
        for _ in 0..3 {
            flush_frame(&mut chat, 60, &mut printed);
        }
        assert_eq!(printed.len(), after, "无新增落盘");

        // The welcome card itself has repeated padded rows; deduping by content would false-positive — check only the message part.
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

    /// inline ctrl+o replay: a no-op with nothing new; with flushed content or expandable items,
    /// it expands everything, rewinds the cursor, and requests a full freeze.
    #[test]
    fn expand_transcript_rewinds_and_expands_everything() {
        let mut chat = test_chat();
        // Empty session, everything on screen → no-op (the replay adds no information).
        assert!(!chat.expand_transcript());
        assert!(!chat.dump_transcript);
        assert!(!chat.force_redraw);

        // After a message flushed → replay: the cursor rewinds and the rebuilt doc contains all segments;
        // clear the screen first, then write (top-aligned, same as resize).
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

        // Historical messages with collapse groups → everything expands before the replay.
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

        // Fully expanded → the second press goes the collapse direction: back to aggregates (the app layer
        // handles the clear-redraw + rehydration to close it up).
        assert!(chat.transcript_fully_expanded());
        assert!(chat.collapse_transcript());
        assert!(
            chat.messages.iter().flat_map(|m| &m.groups).all(|g| !g.expanded),
            "折叠组全部闭合"
        );
        assert!(!chat.transcript_fully_expanded(), "闭合后回到展开方向");
        assert!(!chat.collapse_transcript(), "已全闭合，再闭合无变化");
    }

    /// The tick does not set dirty when idle (no doc rebuild); it does when dynamic elements exist.
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
        // Pending events must also wake it up (otherwise they would never drain).
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
        // Turn start: streaming message + placeholder thinking → must not settle.
        chat.handle(UiEvent::TurnStart);
        chat.build_rows(100);
        assert_eq!(chat.doc.settled, welcome, "streaming message not settled");
        assert!(chat.doc.rows.len() > welcome, "streaming message rendered");
        // Turn end: the message settles, all rows enter settled.
        chat.handle(UiEvent::TurnEnd);
        chat.build_rows(100);
        assert_eq!(chat.doc.settled, chat.doc.rows.len(), "all rows settled after turn");
        // Settling is one-way: a second message (streaming) does not move existing boundaries.
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
        // A message with a running tool.
        let mut m = msg(Role::Assistant, "");
        m.activities.push(tool_activity());
        chat.messages.push(m);
        chat.build_rows(100);
        assert_eq!(chat.doc.settled, welcome, "running tool keeps message dynamic");
        // Tool done → settles.
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
        // Streaming turn (dynamic message).
        chat.handle(UiEvent::TurnStart);
        chat.build_rows(100);
        assert_eq!(chat.doc.settled, welcome, "streaming message dynamic");
        // A permission block appears → the boundary stays put (ask blocks never settle).
        let (tx, _rx) = tokio::sync::oneshot::channel();
        chat.pending_ask = Some((
            PermissionRequest::new("允许执行 Bash", "cargo build", vec!["允许".into()]),
            tx,
        ));
        chat.build_rows(100);
        assert_eq!(chat.doc.settled, welcome, "ask block not settled");
        // Turn end + request resolved → everything settles.
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

    /// Esc (while busy) sets the interrupt flag: background-task completion no longer auto-starts a turn;
    /// a new turn (start_turn) resets it.
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

    /// start_turn's reset order: subscribe first, then send_replace — after the previous turn's receivers are all
    /// dropped (send does not update with no receivers), the new turn still sees false.
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

    /// TurnEnd → asynchronously load the data-URL image → ImageReady reply → the image block appears in the doc.
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

    /// A message with images still loading never settles — otherwise the `#[image]` fallback rows would flush
    /// into scrollback, and since the kitty sequence is only emitted at flush time, the picture could never appear.
    #[test]
    fn message_waits_for_pending_images_before_settling() {
        let mut chat = test_chat();
        chat.image_cap = Some(ImageCap::default_cells());
        let url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(tiny_png())
        );
        chat.messages.push(msg(Role::Assistant, &format!("![图]({url})")));
        // Load in flight (the effect of load_message_images).
        chat.images_pending.insert(url.clone());
        chat.build_rows(100);
        assert_eq!(
            settled_segments(&chat), 1,
            "只有欢迎卡定稿，含在途图片的消息不定稿"
        );

        // Load succeeds → the message settles, and flushed rows carry an ImageRef (the block head emits the kitty sequence).
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

    /// A failed load (including None from a timeout) also releases the block: it settles with the `#[image]` placeholder.
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

    /// Without image capability, nothing enters the in-flight set and messages settle immediately (unchanged behavior).
    #[test]
    fn without_image_capability_messages_settle_immediately() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, "![图](a.png)"));
        chat.build_rows(100);
        assert!(chat.images_pending.is_empty());
        assert_eq!(settled_segments(&chat), 2, "无能力不等图片");
    }

    // ---- Interactions (CC feel): caret editing / history / multiline / double-press semantics / queueing ----

    /// Chat with a dedicated home: history files are split per home, so tests never cross-contaminate.
    fn chat_with_history(tag: &str) -> Chat {
        let home = std::env::temp_dir().join(format!("bingo-chat-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        test_chat_home(home)
    }

    thread_local! {
        static KEY_TICK: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }

    /// Test key clock: every key advances 50ms — far above the paste-burst threshold, so
    /// "rapid typing in tests" is never misjudged as a paste (same as real typing).
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

    /// Caret editing: ←/→ move, ctrl+a/e line start/end, alt+b/f word movement,
    /// insertion lands at the caret, not the line end.
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
        // CJK moves by character and renders by display width.
        chat.set_input("中文");
        press(&mut chat, KeyCode::Left);
        assert_eq!(chat.cursor, 3, "一次退一个汉字（3 字节）");
    }

    /// ctrl+k/u/w delete into the kill buffer, ctrl+y pastes back; ctrl+d deletes after the caret.
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

    /// History: submitted entries go into history and persist; ↑/↓ navigate, back at the bottom restores the draft;
    /// consecutive identical prompts are recorded once.
    #[test]
    fn prompt_history_persists_and_navigates() {
        let mut chat = chat_with_history("history");
        chat.record_history("first");
        chat.record_history("second");
        chat.record_history("second");
        assert_eq!(chat.history.entries(), ["first", "second"], "连续重复只记一条");
        // Persisted: a new session with the same home + cwd can read it.
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

    /// Multi-line input: `\`+Enter and ctrl+j insert newlines, Enter submits the whole;
    /// rendered as multiple rows (each height=1, not a single row stuffed with \n).
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
        // ↑ moves along visual rows within a multi-line input before switching history.
        chat.record_history("older");
        press(&mut chat, KeyCode::Up);
        assert_eq!(chat.input, "first\nsecond\nthird", "行内移动不动文本");
        press(&mut chat, KeyCode::Up);
        press(&mut chat, KeyCode::Up);
        assert_eq!(chat.input, "older", "到首行才切历史");
        let _ = std::fs::remove_dir_all(&chat.session.home);
    }

    /// The input area has a row cap: long input only shows the screen around the caret.
    #[test]
    fn prompt_rows_are_capped() {
        let mut chat = chat_with_history("caprows");
        chat.width = 40;
        chat.set_input((0..30).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n"));
        assert_eq!(chat.prompt_lines().len(), INPUT_ROWS_MAX);
    }

    /// Ctrl+C (CC semantics): interrupts when busy; with text, clears it first (into history);
    /// on empty input, first press hints and a second within the window quits; the counter resets on timeout.
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

        // The counter restarts after the window expires.
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

    /// Esc: interrupts when busy; closes suggestions/panels layer by layer; double-press with text clears and saves to history.
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

    /// Shift+Tab cycles the permission mode, and it really applies to the next turn's Session.
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
        // The turn's Session carries the current mode (Session is immutable in Arc → derive a copy).
        press(&mut chat, KeyCode::BackTab);
        assert_eq!(chat.session_for_turn().permission_mode, PermissionMode::AcceptEdits);
        assert_eq!(chat.session.permission_mode, PermissionMode::Default, "原 Session 不变");

        // A session started in bypass only toggles between bypass ↔ default (never introduces a new dangerous mode).
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

    /// Enter while busy is no longer a no-op: messages queue and show below the input; ↑ pulls back the last one.
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
        // While busy, ↑ pulls back the last queued message for further editing.
        press(&mut chat, KeyCode::Up);
        assert_eq!(chat.input, "second queued");
        assert_eq!(chat.queued.len(), 1);
    }

    /// Bottom entity area: ctrl+g focuses the selector, ↑↓ move, Enter opens, Esc closes;
    /// collapsed state is a one-line summary; no entities → no rows and ctrl+g gives a hint.
    #[test]
    fn entity_selector_picks_agent_and_channel() {
        let mut chat = test_chat();
        chat.width = 80;
        // No entities: takes no rows; ctrl+g shows a hint.
        assert!(chat.entity_rows(80).is_empty());
        assert!(chat.on_key(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert!(chat.notice.is_some(), "空态提示");
        assert!(chat.entity_focus.is_none());
        // Create one agent instance + one channel.
        chat.session
            .agents
            .insert("scout", None, "调研".into(), chat.session.clone());
        chat.session
            .channels
            .create("table", vec![], crate::channels::ChannelMode::Serial)
            .unwrap_or_else(|e| panic!("{e}"));
        chat.refresh_entities();
        assert_eq!(chat.entities.len(), 2);
        // Collapsed: one summary line containing both.
        let rows = chat.entity_rows(80);
        assert_eq!(rows.len(), 1);
        let summary = rows[0].plain_text();
        assert!(
            summary.contains("◉ scout(running)") && summary.contains("◇ #table(0)"),
            "{summary}"
        );
        // Focused: per-row list + ❯ selection + hint row.
        assert!(chat.on_key(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert_eq!(chat.entity_focus, Some(0));
        let rows = chat.entity_rows(80);
        let joined: Vec<String> = rows.iter().map(|l| l.plain_text()).collect();
        assert!(joined[0].starts_with("❯ ◉ scout"), "{joined:?}");
        assert!(joined.last().unwrap_or(&String::new()).contains("enter 打开"));
        // ↓ to the channel, Enter opens it.
        assert!(chat.on_key(KeyCode::Down, KeyModifiers::empty()));
        assert!(chat.on_key(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(
            chat.open_entity,
            Some(EntityOpen::Channel("table".into())),
            "选中频道"
        );
        assert!(chat.entity_focus.is_none(), "打开后退出聚焦");
        // After refocusing, Esc only closes the selector (does not trigger global Esc semantics).
        let _ = chat.on_key(KeyCode::Char('g'), KeyModifiers::CONTROL);
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(chat.entity_focus.is_none());
    }

    /// Queues beyond the cap fold into one row (row count feeds chrome, so it must be bounded).
    #[test]
    fn queue_lines_are_capped() {
        let mut chat = chat_with_history("queuecap");
        chat.queued = (0..10).map(|i| format!("m{i}")).collect();
        assert_eq!(chat.queue_lines().len(), QUEUE_ROWS_MAX + 1);
        assert!(chat.queue_lines().last().is_some_and(|l| l.contains("more queued")));
    }

    /// `?`: toggles the panel on empty input; an ordinary character otherwise.
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

    /// Help panel rows are bounded by the terminal height (the canvas must never exceed it).
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

    /// ctrl+s stash/restore (with the caret), ctrl+_ undo, ctrl+t task area, ctrl+l repaint.
    #[test]
    fn stash_undo_tasks_and_redraw() {
        let mut chat = chat_with_history("t2");
        type_text(&mut chat, "stashed");
        chat.cursor = 3;
        assert!(ctrl(&mut chat, 's'));
        assert_eq!(chat.input, "", "ctrl+s 暂存并清空");
        assert!(ctrl(&mut chat, 's'));
        assert_eq!((chat.input.as_str(), chat.cursor), ("stashed", 3), "恢复含光标");

        // Undo: a bulk edit (kill) steps back one.
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

    /// bash mode: empty-input Esc/backspace/ctrl+u exit; Tab completes from this session's `!` history.
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

    /// Paste burst: Enter inside a burst is a newline, not send; ≥10 lines fold into a placeholder,
    /// with the real content expanded at submit time.
    #[test]
    fn paste_burst_inserts_newlines_and_collapses() {
        let mut chat = chat_with_history("paste");
        let mut now = std::time::Instant::now();
        let fast = std::time::Duration::from_millis(1);
        // "Paste" 12 lines character by character.
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

        // Normal typing (wide intervals): Enter submits as usual instead of inserting a newline.
        let mut chat = chat_with_history("paste2");
        chat.busy = true; // queueing path: no tokio runtime needed
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

    /// Bracketed paste: the whole chunk inserts at the caret as one undo step; ≥10 lines fold into a placeholder,
    /// with the real content expanded at submit time. CR newlines (what terminals paste) are normalized first.
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

        // Short chunks do not fold (below the threshold).
        let mut chat = chat_with_history("paste-short");
        chat.on_paste("line1\nline2");
        assert_eq!(chat.input, "line1\nline2");
        assert!(chat.pastes.is_empty(), "未到阈值不折叠");

        // ≥ PASTE_COLLAPSE_LINES lines fold; CR and CRLF both count as newlines.
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

        // An empty paste does nothing (no undo-stack write).
        let mut chat = chat_with_history("paste-empty");
        chat.on_paste("");
        assert!(chat.input.is_empty());
        assert!(chat.undo.is_empty());
    }

    /// Generates a test PNG and returns its path.
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

    /// A standalone image path line at submit time → register the attachment + `#[image N]` placeholder (text kept).
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

    /// A whole `![alt](path)` line is recognized too; non-image paths/missing files stay as-is.
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

    /// resolve_images: picks attachments by placeholder number (deduped, out-of-range ignored).
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

    /// ctrl+r reverse search: filter hits, press again for older, Tab adopts and keeps editing,
    /// ctrl+c cancels and restores.
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
        // In search mode, the input row shows the hit.
        assert_eq!(chat.prompt_lines()[0].plain_text(), "cargo test");
        press(&mut chat, KeyCode::Tab);
        assert!(chat.search.is_none(), "Tab 采纳并退出搜索");
        assert_eq!(chat.input, "cargo test");

        // ctrl+c cancels: the input restores to its pre-search content.
        chat.set_input("keep");
        ctrl(&mut chat, 'r');
        ctrl(&mut chat, 'c');
        assert!(chat.search.is_none(), "ctrl+c 退出搜索");
        assert_eq!(chat.input, "keep", "取消不改输入");
        let _ = std::fs::remove_dir_all(&chat.session.home);
    }

    /// Alt+T thinking toggle: off ↔ the previous level.
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

    /// Task area (CC glyphs): `☐`/`☒`, completed items dimmed + strikethrough semantics.
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

    /// Empty-input placeholder hint (CC placeholder); gone once there is input.
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

    /// A 4×2 solid-color PNG (for tests).
    fn tiny_png() -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(4, 2, image::Rgba([255u8, 0, 0, 255]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    // ---- #18 presentation-layer minimal implementation: error-row highlight + full-screen state + retry/back ----

    /// #18 full-flow full-screen error state: inject a Full-level fixture → `last_error` recorded →
    /// `Frame::assemble` produces the full-screen error rows (title/stable code/actions) → Esc returns and clears the error state
    /// (AC-26/53: the way back is not a dead end).
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
        // Esc returns: not a dead end.
        chat.on_key(KeyCode::Esc, KeyModifiers::empty());
        assert!(chat.last_error.is_none(), "Esc 返回清除错误态");
    }

    /// #18 page-level error-row highlight: inject a Page-level fixture → the `[error]` row uses the error color
    /// (A zone; theme.error = (255,107,128) color baseline).
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

    /// #18 full-screen state: Enter retries the last input (AC-15/53 retry-path skeleton).
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

    // ---- QA assertion side (delivery 3/3): AC-53 / AC-29 / presentation styling ----

    /// AC-53 long-turn failure escalates: FX-11 (TIMEOUT + LongTurn) → full-flow full-screen state,
    /// versus FX-01 (TIMEOUT + ShortSync, page-level): **same code, different level**, distinguished by context.
    /// The full-screen state shows the stable code + retry/back paths (AC-53 F3) and hides the caret.
    #[test]
    fn qa_ac53_long_turn_timeout_escalates_to_full_screen() {
        use crate::error::ErrorContext;
        use crate::error::ErrorLevel;
        use crate::tui::app::Frame;
        use crate::tui::test_util::error_fixtures;
        use ratatui::layout::Size;
        // Long-turn transport timeout → full-flow level.
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
        // Same code, short sync (FX-01) → page-level error row, not full-screen — the two TIMEOUT levels are told apart by context.
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

    /// AC-29 per-code matrix: inject all 11 fixtures from error_fixtures(), asserting that
    /// "the level is carried explicitly by the producer and the render shape matches it" — Full → full screen,
    /// Page/Field → error row. The assertion anchor is the stable code, never the msg text.
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

    /// Presentation styling (A zone): after the page-level error row renders into the Buffer via `render_rows`,
    /// **the real cells use the error color (255,107,128)** (not just at the SegStyle layer) — asserting that
    /// the "highlight the user sees" lands on the final picture, anchored in both style and text.
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
        // Text anchor (assertions only anchor on the code).
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

    /// FX-01 **real-path** assertion (main #91 / dev #92 invite): the `/model` level-two menu
    /// fetch (`open_model_models`, the production emission source) emits
    /// `UiEvent::Error { level: Page, context: ShortSync }` when list_models times out (10s) — no fixture
    /// injection, verifying the **production trigger source** wiring (AC-12/13/14 page-level contracts have a real landing).
    /// Degraded behavior is preserved: the error row is visible, non-full-screen, non-blocking.
    #[tokio::test(start_paused = true)]
    async fn qa_fx01_real_path_model_menu_failure_emits_page_error() {
        use crate::api::client::test_hooks;
        use crate::error::ErrorContext;
        use crate::error::ErrorLevel;
        use crate::tui::app::Frame;
        use ratatui::layout::Size;
        let _guard = test_hooks::hang_guard(60_000); // hangs list_models for 60s, > the 10s read timeout
        let mut chat = test_chat();
        chat.open_model_models("test".into(), vec!["test".into()], 0); // 触发真实生产拉取路径（fork provider）
        // 先让 spawn 任务启动并注册超时 timer（start_paused 下需 poll 才推进）。
        tokio::task::yield_now().await;
        // The 10s read timeout fires → emits UiEvent::Error (page-level).
        tokio::time::advance(std::time::Duration::from_secs(11)).await;
        tokio::task::yield_now().await; // let the spawned task finish sending the event
        chat.drain_events();
        let err = chat.last_error.as_ref().expect("生产发射源已记录错误态");
        assert_eq!(err.code, "TIMEOUT", "list_models 读超时落 TIMEOUT");
        assert_eq!(err.level, ErrorLevel::Page, "短同步=页面级（真实路径）");
        assert_eq!(err.context, ErrorContext::ShortSync, "上下文=短同步");
        // Render: the page-level error row is visible, non-full-screen (degraded behavior preserved).
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
