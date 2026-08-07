//! agent 活动（thinking / tool / diff / watch）。
//!
//! 移植自 rsmarkdown-tui `activities.rs`：活动是一个可折叠面板——折叠
//! 时一行头部，展开时头部 + 内容。这里只保留 bingo 用到的种类
//! （Thinking / Tool / Diff / Watch），SubAgent 由 bingo 以 Tool 呈现。

use crate::tui::line::Line;
use crate::tui::theme::Theme;

/// Spinner frames: a star that grows and shrinks (`·` → `✻`/`✽` → `·`),
/// driven by the host tick. The sequence is a there-and-back cycle, so the
/// glyph never jumps between sizes.
pub const SPINNERS: [char; 8] = ['·', '✢', '*', '✻', '✽', '✻', '*', '✢'];

/// Spinner frame for a given tick.
pub fn spinner(frame: u64) -> char {
    SPINNERS[(frame as usize) % SPINNERS.len()]
}

/// Result connector under a tool header (CC `  ⎿  `). Continuation lines line
/// up with the text after it.
pub const RESULT_CONNECTOR: &str = "  ⎿  ";
/// Indentation of the lines that continue a result block.
pub const RESULT_INDENT: &str = "     ";
/// Only commands slower than this get a duration on their result line —
/// a millisecond count on every row is noise.
pub const SLOW_TOOL_MS: u64 = 2_000;

/// 工具调用生命周期。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    /// 执行中。
    Running,
    /// 成功完成。
    Done,
    /// 失败。
    Error,
}

/// 一次工具调用：`✓ bash · cargo test · 12ms`。
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// 工具名（如 `bash`、`Edit`）。
    pub name: &'static str,
    /// 生命周期状态。
    pub status: ToolStatus,
    /// 命令/参数摘要。
    pub summary: String,
    /// 已运行毫秒。
    pub duration_ms: u64,
    /// 头部单行输出预览。
    pub output: Option<String>,
    /// 展开态显示的结果摘要（CC `⎿ Read 173 lines`），渲染在 content 前。
    pub result_summary: Option<String>,
}

impl ToolCall {
    /// 创建一个执行中的工具调用。
    pub fn running(name: &'static str, summary: impl Into<String>) -> Self {
        Self {
            name,
            status: ToolStatus::Running,
            summary: summary.into(),
            duration_ms: 0,
            output: None,
            result_summary: None,
        }
    }
}

/// Watchable 生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchStatus {
    /// 执行中。
    Running,
    /// 一轮完成（周期命令轮次边界）。
    Idle,
    /// 成功终态。
    Done,
    /// 失败终态。
    Failed,
    /// 取消终态。
    Cancelled,
}

/// 一个被 watch 的实体（命令/agent）：header + 轮次 detail + 可展开内容。
#[derive(Debug, Clone)]
pub struct WatchCall {
    /// 描述（如 `watch -n 2 ls`、`reviewer · 审查提交`）。
    pub label: String,
    /// 类别（图标：⏺ 命令 / ◉ 子代理）。
    pub kind: crate::watch::WatchKind,
    /// 生命周期状态。
    pub status: WatchStatus,
    /// 当前轮次/进度描述。
    pub detail: Option<String>,
    /// 已运行毫秒。
    pub duration_ms: u64,
}

/// 思考块是否仍在运行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingState {
    /// 仍在推理。
    Running,
    /// 推理完成。
    Done,
}

/// 一个思考块：`✻ Thinking`（运行/完成同头；运行词与耗时只在底部状态行）。
/// 完成行的 `✻ Churned for 1.4s` 由 [`thinking_completion_line`] 在消息末尾渲染。
#[derive(Debug, Clone)]
pub struct Thinking {
    /// 运行/完成。
    pub state: ThinkingState,
    /// 已运行毫秒。
    pub duration_ms: u64,
    /// 区分连续推理阶段（运行/完成更新替换正确的块）。
    pub stage: &'static str,
    /// 完成态随机词（`✻ Churned for 40s`）；
    /// None 回落 stage。
    pub done_verb: Option<&'static str>,
    /// 宿主 tick（块级独立计时起点）。
    pub start_tick: u64,
}

/// 任务生命周期（pending → in_progress → completed）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    /// 未开始。
    Pending,
    /// 进行中。
    InProgress,
    /// 已完成。
    Done,
}

/// 一个任务项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoItem {
    /// 任务文本。
    pub text: String,
    /// 生命周期状态。
    pub status: TodoStatus,
}

/// unified diff 的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    /// 未变上下文行。
    Context(String),
    /// 删除行（`-`）。
    Removed(String),
    /// 新增行（`+`）。
    Added(String),
}

/// unified diff 的一个 hunk。
#[derive(Debug, Clone)]
pub struct Hunk {
    /// `@@ -a,b +c,d @@`
    pub header: String,
    /// 上下文 / 删除 / 新增行。
    pub lines: Vec<DiffLine>,
}

/// 一次文件编辑，呈现为 git 风格 unified diff。
#[derive(Debug, Clone)]
pub struct Diff {
    /// 文件路径。
    pub path: String,
    /// 按序 hunks。
    pub hunks: Vec<Hunk>,
}

impl Diff {
    /// 统计所有 hunk 的新增/删除行数。
    pub fn stats(&self) -> (usize, usize) {
        let mut added = 0;
        let mut removed = 0;
        for hunk in &self.hunks {
            for line in &hunk.lines {
                match line {
                    DiffLine::Added(_) => added += 1,
                    DiffLine::Removed(_) => removed += 1,
                    DiffLine::Context(_) => {}
                }
            }
        }
        (added, removed)
    }

    /// 解析 unified diff（git 格式：`---` / `+++` / `@@` / `-` `+` ` `）。
    pub fn parse_unified(text: &str) -> Self {
        let mut path = String::new();
        let mut hunks: Vec<Hunk> = Vec::new();
        for line in text.lines() {
            if let Some(p) = line.strip_prefix("+++ b/") {
                path = p.to_string();
                continue;
            }
            if let Some(p) = line.strip_prefix("+++ ") {
                path = p.to_string();
                continue;
            }
            if let Some(header) = line.strip_prefix("@@") {
                let header = format!("@@{header}");
                hunks.push(Hunk {
                    header,
                    lines: Vec::new(),
                });
                continue;
            }
            if line.starts_with("--- ") {
                continue;
            }
            if let Some(hunk) = hunks.last_mut() {
                if let Some(rest) = line.strip_prefix('-')
                    && !rest.starts_with('-')
                {
                    hunk.lines.push(DiffLine::Removed(rest.to_string()));
                    continue;
                }
                if let Some(rest) = line.strip_prefix('+')
                    && !rest.starts_with('+')
                {
                    hunk.lines.push(DiffLine::Added(rest.to_string()));
                    continue;
                }
                if let Some(rest) = line.strip_prefix(' ') {
                    hunk.lines.push(DiffLine::Context(rest.to_string()));
                } else if !line.is_empty() {
                    hunk.lines.push(DiffLine::Context(line.to_string()));
                }
            }
        }
        Self { path, hunks }
    }
}

/// 渲染 diff 为样式化行：`@@` hunk 头、`-` 红、`+` 绿。
pub fn diff_lines(d: &Diff, theme: &Theme) -> Vec<Line> {
    let mut out = Vec::new();
    for hunk in &d.hunks {
        out.push(Line::styled(hunk.header.clone(), theme.diff_hunk()));
        for line in &hunk.lines {
            let (prefix, style) = match line {
                DiffLine::Context(_) => (" ", theme.diff_context()),
                DiffLine::Removed(_) => ("-", theme.diff_removed()),
                DiffLine::Added(_) => ("+", theme.diff_added()),
            };
            let text = match line {
                DiffLine::Context(t) | DiffLine::Removed(t) | DiffLine::Added(t) => t.clone(),
            };
            out.push(Line::styled(format!("{prefix}{text}"), style));
        }
    }
    out
}

/// 活动种类。
#[derive(Debug, Clone)]
pub enum ActivityKind {
    /// 思考块。
    Thinking(Thinking),
    /// 工具调用。
    Tool(ToolCall),
    /// 文件编辑（unified diff）。
    Diff(Diff),
    /// 被 watch 的实体。
    Watch(WatchCall),
}

/// 可折叠的活动：头部一行 + 可选展开内容。所有种类共享
/// 展开/折叠能力，只有呈现不同。
#[derive(Debug, Clone)]
pub struct Activity {
    /// 活动种类。
    pub kind: ActivityKind,
    /// 折叠（false）或展开（true）。
    pub expanded: bool,
    /// 展开时显示的内容（reasoning 文本 / 工具 I/O）。
    pub content: Vec<Line>,
    /// 是否来自自动展开规则（活动仍活跃）而非用户点击。
    pub auto_expanded: bool,
    /// 展开提示文本（如 `"ctrl+o to expand"`）。
    pub expand_hint: Option<String>,
}

impl Activity {
    /// 创建一个折叠的活动。
    pub fn new(kind: ActivityKind) -> Self {
        Self {
            kind,
            expanded: false,
            content: Vec::new(),
            auto_expanded: false,
            expand_hint: None,
        }
    }

    /// 展开或折叠（手动切换接管自动展开规则）。
    pub fn toggle(&mut self) {
        self.expanded = !self.expanded;
        self.auto_expanded = false;
    }

    /// 展开是否揭示任何内容。
    pub fn expandable(&self) -> bool {
        !self.content.is_empty()
    }

    /// 设置展开内容。
    pub fn set_content(&mut self, content: Vec<Line>) {
        self.content = content;
    }

    /// 活动是否仍在变化（行内容随 tick/事件更新）：运行中的思考块、
    /// 运行中的工具、运行中的 watch。REPL 模式以此判定消息可否定稿。
    pub fn is_running(&self) -> bool {
        match &self.kind {
            ActivityKind::Thinking(t) => t.state == ThinkingState::Running,
            ActivityKind::Tool(t) => t.status == ToolStatus::Running,
            ActivityKind::Watch(w) => w.status == WatchStatus::Running,
            ActivityKind::Diff(_) => false,
        }
    }
}

/// Edit/Write header: `⏺ Update(path)` (the `✻` marker stays with thinking).
fn diff_header(d: &Diff, theme: &Theme) -> Line {
    let mut line = Line::styled("⏺ ", theme.tool_done());
    line.push_styled(format!("Update({})", d.path), theme.text());
    line
}

/// Edit/Write result: `  ⎿  Updated path with N additions and M removals`.
fn diff_result(d: &Diff, theme: &Theme) -> Line {
    let (added, removed) = d.stats();
    let plural = |n: usize, word: &str| {
        if n == 1 {
            format!("{n} {word}")
        } else {
            format!("{n} {word}s")
        }
    };
    Line::styled(
        format!(
            "{RESULT_CONNECTOR}Updated {} with {} and {}",
            d.path,
            plural(added, "addition"),
            plural(removed, "removal"),
        ),
        theme.dim(),
    )
}

/// 思考块头：运行/完成同形 `✻ Thinking`（dim italic）。
/// 运行词、spinner 与耗时只出现在底部状态行，避免重复。
fn thinking_header(theme: &Theme) -> Line {
    Line::styled("✻ Thinking", theme.thinking())
}

/// 思考完成行：`✻ {done_verb} for 40.0s`，
/// 渲染在消息末尾（正文与全部工具之后）。
pub fn thinking_completion_line(t: &Thinking, theme: &Theme) -> Line {
    let verb = t.done_verb.unwrap_or(t.stage);
    Line::styled(
        format!("✻ {verb} for {:.1}s", t.duration_ms as f64 / 1000.0),
        theme.thinking(),
    )
}

/// Status colour of the leading marker: running is muted, done is green,
/// failure is red.
fn dot_style(status: ToolStatus, theme: &Theme) -> crate::tui::line::SegStyle {
    match status {
        ToolStatus::Running => theme.dim(),
        ToolStatus::Done => theme.tool_done(),
        ToolStatus::Error => theme.tool_error(),
    }
}

/// 工具类别图标：内建 `⏺` / MCP `◆` / Skill `✦`。形状表类别、颜色表状态
/// （dot_style 不变）；Agent 无工具行，其 Watch 行图标见 [`watch_header`]。
pub fn tool_glyph(name: &str) -> &'static str {
    if name.starts_with("mcp__") {
        "◆ "
    } else if name == "Skill" {
        "✦ "
    } else {
        "⏺ "
    }
}

/// MCP 全名 `mcp__server__tool` 显示为 `server:tool`；权限规则仍用全名。
pub fn display_tool_name(name: &str) -> String {
    match name.strip_prefix("mcp__") {
        Some(rest) => rest.replacen("__", ":", 1),
        None => name.to_string(),
    }
}

/// Tool header: `⏺ Bash(git status)` — no timing, no output; those belong on
/// the result line below.
fn tool_header(t: &ToolCall, theme: &Theme) -> Line {
    let mut line = Line::styled(tool_glyph(t.name), dot_style(t.status, theme));
    let shown = display_tool_name(t.name);
    if t.summary.is_empty() {
        line.push_styled(shown, theme.text());
    } else {
        line.push_styled(format!("{shown}({})", t.summary), theme.text());
    }
    line
}

/// Tool result: `  ⎿  Read 173 lines (ctrl+o to expand)`. Running tools get
/// `Running…` — the spinner itself lives in the bottom status line.
fn tool_result(t: &ToolCall, act: &Activity, theme: &Theme) -> Line {
    let mut body = match t.status {
        ToolStatus::Running => "Running…".to_string(),
        _ => t
            .result_summary
            .clone()
            .or_else(|| t.output.clone())
            .unwrap_or_else(|| match t.status {
                ToolStatus::Error => "Failed".to_string(),
                _ => "Done".to_string(),
            }),
    };
    if t.status != ToolStatus::Running && t.duration_ms > SLOW_TOOL_MS {
        body.push_str(&format!(" · Ran in {:.1}s", t.duration_ms as f64 / 1000.0));
    }
    let style = if t.status == ToolStatus::Error {
        theme.tool_error()
    } else {
        theme.dim()
    };
    let mut line = Line::styled(format!("{RESULT_CONNECTOR}{body}"), style);
    if let Some(hint) = expand_hint(act) {
        line.push_styled(format!(" ({hint})"), theme.dim());
    }
    line
}

/// `⏺ watch -n 2 ls` — same shape as a tool, driven by the watch lifecycle.
/// 子代理的 Watch 行（WatchKind::Agent）用 `◉`：环中有核，会话中套会话。
fn watch_header(w: &WatchCall, theme: &Theme) -> Line {
    let style = match w.status {
        WatchStatus::Running | WatchStatus::Idle => theme.dim(),
        WatchStatus::Done => theme.tool_done(),
        WatchStatus::Failed => theme.tool_error(),
        WatchStatus::Cancelled => theme.dim(),
    };
    let glyph = match w.kind {
        crate::watch::WatchKind::Agent => "◉ ",
        crate::watch::WatchKind::Channel => "◇ ",
        crate::watch::WatchKind::Command => "⏺ ",
    };
    let mut line = Line::styled(glyph, style);
    line.push_styled(w.label.clone(), theme.text());
    line
}

fn watch_result(w: &WatchCall, act: &Activity, theme: &Theme) -> Line {
    let mut body = match (&w.detail, w.status) {
        (Some(detail), _) => detail.clone(),
        (None, WatchStatus::Running) => "Running…".to_string(),
        (None, WatchStatus::Idle) => "Waiting…".to_string(),
        (None, WatchStatus::Done) => "Done".to_string(),
        (None, WatchStatus::Failed) => "Failed".to_string(),
        (None, WatchStatus::Cancelled) => "Cancelled".to_string(),
    };
    if w.status != WatchStatus::Running && w.duration_ms > SLOW_TOOL_MS {
        body.push_str(&format!(" · Ran in {:.1}s", w.duration_ms as f64 / 1000.0));
    }
    let style = if w.status == WatchStatus::Failed {
        theme.tool_error()
    } else {
        theme.dim()
    };
    let mut line = Line::styled(format!("{RESULT_CONNECTOR}{body}"), style);
    if let Some(hint) = expand_hint(act) {
        line.push_styled(format!(" ({hint})"), theme.dim());
    }
    line
}

fn header_for(h: &Activity, theme: &Theme) -> Line {
    match &h.kind {
        ActivityKind::Thinking(_) => thinking_header(theme),
        ActivityKind::Tool(t) => tool_header(t, theme),
        ActivityKind::Diff(d) => diff_header(d, theme),
        ActivityKind::Watch(w) => watch_header(w, theme),
    }
}

/// The result line of a collapsed activity advertises how to open it.
fn expand_hint(act: &Activity) -> Option<&str> {
    if act.expanded || act.content.is_empty() {
        return None;
    }
    act.expand_hint.as_deref()
}

/// Fold hint for activities that have no result line of their own
/// (thinking): `… +N lines (ctrl+o to expand)`.
fn fold_tail(act: &Activity) -> Option<String> {
    if act.expanded || act.content.is_empty() {
        return None;
    }
    match &act.kind {
        ActivityKind::Thinking(_) => {
            let mut tail = format!("… +{} lines", act.content.len());
            if let Some(hint) = &act.expand_hint {
                tail.push_str(&format!(" ({hint})"));
            }
            Some(tail)
        }
        _ => None,
    }
}

/// 一个活动的可点击行范围（文档坐标）。
#[derive(Debug, Clone)]
pub struct ActivityRowRange {
    /// 首行（含）。
    pub start: u16,
    /// 尾行（不含）。
    pub end: u16,
    /// 活动路径（`[i]` 消息级活动；无嵌套 subagent 时恒为单元素）。
    pub path: Vec<usize>,
}

/// 布局单个活动：头部 + 结果行 + （展开时）内容，返回行与可点击范围。
///
/// 没有 spinner 参数：活动行是静态的，运行中的动画只在底部状态行
/// （CC 语义），于是已定稿的活动行永远不再变化。
///
/// `render_reply` 渲染 subagent 的 markdown 回复（本模块保持显示无关，
/// bingo 的 SubAgent 以 Tool 呈现，无需递归——保留参数以对齐原契约）。
pub fn layout_activity(
    act: &Activity,
    path: &[usize],
    base_row: u16,
    theme: &Theme,
    render_reply: &mut dyn FnMut(&str) -> Vec<Line>,
) -> (Vec<Line>, Vec<ActivityRowRange>) {
    let mut header = header_for(act, theme);
    if let Some(tail) = fold_tail(act) {
        header.push_styled(format!(" {tail}"), theme.dim());
    }
    let mut rows = vec![header];
    // Result line (CC `  ⎿  …`): tools, edits and watches always carry one —
    // it is where status, timing and the expand hint live.
    match &act.kind {
        ActivityKind::Tool(t) => rows.push(tool_result(t, act, theme)),
        ActivityKind::Diff(d) => rows.push(diff_result(d, theme)),
        ActivityKind::Watch(w) => rows.push(watch_result(w, act, theme)),
        ActivityKind::Thinking(_) => {}
    }
    let thinking = matches!(act.kind, ActivityKind::Thinking(_));
    if act.expanded {
        for line in &act.content {
            let mut styled = Line::styled(RESULT_INDENT, theme.tool_output());
            // Reasoning text is italic grey (CC), tool output keeps its colours.
            let body = if thinking {
                line.clone().styled_all(theme.thinking())
            } else {
                line.clone()
            };
            styled.segs.extend(body.segs);
            styled.image = line.image.clone();
            rows.push(styled);
        }
    }
    let cursor = base_row + rows.len() as u16;
    let _ = render_reply;
    let ranges = vec![ActivityRowRange {
        start: base_row,
        end: cursor,
        path: path.to_vec(),
    }];
    (rows, ranges)
}

/// 沿路径（可含嵌套 subagent）定位活动，可变引用。
pub fn activities_path_get_mut<'a>(
    acts: &'a mut [Activity],
    path: &[usize],
) -> Option<&'a mut Activity> {
    let (head, rest) = path.split_first()?;
    let act = acts.get_mut(*head)?;
    if rest.is_empty() {
        return Some(act);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line) -> String {
        line.plain_text()
    }

    fn thinking(stage: &'static str, state: ThinkingState) -> Activity {
        let mut h = Activity::new(ActivityKind::Thinking(Thinking {
            state,
            duration_ms: 2300,
            stage,
            done_verb: None,
            start_tick: 0,
        }));
        if state == ThinkingState::Done {
            h.set_content(vec![Line::plain("reasoning line")]);
        }
        h
    }

    fn render_lines(h: &Activity) -> Vec<Line> {
        let mut render = |_: &str| Vec::new();
        let (rows, _) = layout_activity(h, &[0], 0, &Theme::dark(), &mut render);
        rows
    }

    #[test]
    fn thinking_collapsed_and_expanded() {
        // 块头运行/完成同形 `✻ Thinking`；完成行由 thinking_completion_line 单独渲染。
        let mut h = thinking("understand", ThinkingState::Done);
        assert!(h.expandable());
        let lines = render_lines(&h);
        assert_eq!(text(&lines[0]), "✻ Thinking … +1 lines");
        assert_eq!(lines.len(), 1, "collapsed: header only");

        h.toggle();
        assert!(h.expanded);
        let lines = render_lines(&h);
        assert_eq!(text(&lines[0]), "✻ Thinking");
        assert_eq!(lines.len(), 2, "expanded: header + content");
        assert!(text(&lines[1]).contains("reasoning line"));
        // 思考正文 italic + thinking 灰（CC italic gray）。
        let body = lines[1].segs.last().expect("content seg");
        assert!(body.style.italic, "italic reasoning");
        assert_eq!(body.style.fg, Some(Theme::dark().thinking));

        h.toggle();
        assert!(!h.expanded);
    }

    #[test]
    fn running_hint_is_not_expandable() {
        let h = thinking("understand", ThinkingState::Running);
        assert!(!h.expandable());
        let lines = render_lines(&h);
        assert_eq!(text(&lines[0]), "✻ Thinking");
    }

    #[test]
    fn completion_line_uses_random_verb_and_duration() {
        // `✻ Churned for 40.0s`（随机过去式动词）。
        let t = Thinking {
            state: ThinkingState::Done,
            duration_ms: 40_000,
            stage: "Churning",
            done_verb: Some("Churned"),
            start_tick: 0,
        };
        let line = thinking_completion_line(&t, &Theme::dark());
        assert_eq!(text(&line), "✻ Churned for 40.0s");
        // None 回落 stage。
        let t2 = Thinking {
            stage: "Churning",
            done_verb: None,
            ..t
        };
        let line2 = thinking_completion_line(&t2, &Theme::dark());
        assert_eq!(text(&line2), "✻ Churning for 40.0s");
    }

    /// CC 双行结构：`⏺ Bash(cmd)` + `  ⎿  {结果} (ctrl+o to expand)`。
    #[test]
    fn tool_collapsed_and_expanded() {
        let mut h = Activity::new(ActivityKind::Tool(ToolCall {
            name: "Bash",
            status: ToolStatus::Done,
            summary: "cargo test -p core".into(),
            duration_ms: 12,
            output: Some("54 passed".into()),
            result_summary: None,
        }));
        h.expand_hint = Some("ctrl+o to expand".to_string());
        h.set_content(vec![
            Line::plain("$ cargo test -p core"),
            Line::plain("54 passed; 0 failed"),
        ]);
        let lines = render_lines(&h);
        assert_eq!(text(&lines[0]), "⏺ Bash(cargo test -p core)");
        assert_eq!(
            text(&lines[1]),
            "  ⎿  54 passed (ctrl+o to expand)",
            "结果行带展开提示，快命令不显示耗时"
        );
        assert_eq!(lines.len(), 2, "collapsed: header + result");

        h.toggle();
        let lines = render_lines(&h);
        assert_eq!(text(&lines[1]), "  ⎿  54 passed", "展开后不再提示");
        assert!(text(&lines[2]).starts_with("     $ cargo test -p core"));
        assert!(text(&lines[3]).contains("54 passed; 0 failed"));
    }

    /// 运行中：头行立即出现，结果行 `Running…`（spinner 在底部状态行）。
    #[test]
    fn running_tool_shows_running_result_line() {
        let h = Activity::new(ActivityKind::Tool(ToolCall::running("Read", "src/main.rs")));
        let lines = render_lines(&h);
        assert_eq!(text(&lines[0]), "⏺ Read(src/main.rs)");
        assert_eq!(text(&lines[1]), "  ⎿  Running…");
        // 运行中的点用弱色，完成后转绿。
        assert_eq!(lines[0].segs[0].style.fg, Some(Theme::dark().inactive));
    }

    /// 慢命令（>2s）把耗时并进结果行；错误行用 error 色。
    #[test]
    fn slow_and_failed_tools_annotate_the_result_line() {
        let slow = Activity::new(ActivityKind::Tool(ToolCall {
            name: "Bash",
            status: ToolStatus::Done,
            summary: "cargo build".into(),
            duration_ms: 2_300,
            output: Some("Compiling".into()),
            result_summary: None,
        }));
        assert_eq!(
            text(&render_lines(&slow)[1]),
            "  ⎿  Compiling · Ran in 2.3s"
        );
        let failed = Activity::new(ActivityKind::Tool(ToolCall {
            name: "Bash",
            status: ToolStatus::Error,
            summary: "false".into(),
            duration_ms: 5,
            output: None,
            result_summary: None,
        }));
        let lines = render_lines(&failed);
        assert_eq!(text(&lines[1]), "  ⎿  Failed");
        assert_eq!(lines[0].segs[0].style.fg, Some(Theme::dark().error));
        assert_eq!(lines[1].segs[0].style.fg, Some(Theme::dark().error));
    }

    /// Edit/Write：`⏺ Update(path)` + `  ⎿  Updated path with N additions…`。
    #[test]
    fn diff_renders_update_header_and_result() {
        let diff = Diff::parse_unified(
            "--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,2 @@\n a\n-b\n+c\n+d\n",
        );
        let h = Activity::new(ActivityKind::Diff(diff));
        let lines = render_lines(&h);
        assert_eq!(text(&lines[0]), "⏺ Update(f.txt)");
        assert_eq!(
            text(&lines[1]),
            "  ⎿  Updated f.txt with 2 additions and 1 removal"
        );
    }

    #[test]
    fn spinner_cycles() {
        let a = spinner(0);
        let b = spinner(1);
        assert_ne!(a, b);
        assert_eq!(spinner(SPINNERS.len() as u64), a, "cycles after full rotation");
        // 星芒字形（CC）：不再是 braille。
        assert_eq!(SPINNERS[0], '·');
        assert!(SPINNERS.contains(&'✻'));
        assert!(!SPINNERS.contains(&'⠋'));
    }

    #[test]
    fn diff_parse_and_stats() {
        let d = Diff::parse_unified("--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,2 @@\n a\n-b\n+c\n");
        assert_eq!(d.path, "f.txt");
        assert_eq!(d.hunks.len(), 1);
        let (added, removed) = d.stats();
        assert_eq!(added, 1);
        assert_eq!(removed, 1);
    }

    /// 类别图标：⏺ 内建 / ◆ MCP（显示名 server:tool）/ ✦ Skill / ◉ Agent watch。
    /// 形状表类别，颜色继续表状态。
    #[test]
    fn category_icons_and_mcp_display_name() {
        let mcp = Activity::new(ActivityKind::Tool(ToolCall {
            name: "mcp__dokploy__application-deploy",
            status: ToolStatus::Done,
            summary: "applicationId=\"x\"".into(),
            duration_ms: 5,
            output: None,
            result_summary: None,
        }));
        assert_eq!(
            text(&render_lines(&mcp)[0]),
            "◆ dokploy:application-deploy(applicationId=\"x\")",
            "MCP 全名显示为 server:tool"
        );
        // 状态色仍在图标上：Done 转绿。
        assert_eq!(
            render_lines(&mcp)[0].segs[0].style.fg,
            Some(Theme::dark().success)
        );

        let skill = Activity::new(ActivityKind::Tool(ToolCall {
            name: "Skill",
            status: ToolStatus::Running,
            summary: "review doc.md".into(),
            duration_ms: 0,
            output: None,
            result_summary: None,
        }));
        assert_eq!(text(&render_lines(&skill)[0]), "✦ Skill(review doc.md)");

        let agent_watch = Activity::new(ActivityKind::Watch(WatchCall {
            label: "reviewer · 整理笔记".into(),
            kind: crate::watch::WatchKind::Agent,
            status: WatchStatus::Running,
            detail: None,
            duration_ms: 0,
        }));
        assert_eq!(text(&render_lines(&agent_watch)[0]), "◉ reviewer · 整理笔记");

        let channel_watch = Activity::new(ActivityKind::Watch(WatchCall {
            label: "#table".into(),
            kind: crate::watch::WatchKind::Channel,
            status: WatchStatus::Running,
            detail: Some("3 条 · 最近 a: 报数".into()),
            duration_ms: 0,
        }));
        let lines = render_lines(&channel_watch);
        assert_eq!(text(&lines[0]), "◇ #table");
        assert_eq!(text(&lines[1]), "  ⎿  3 条 · 最近 a: 报数");
    }

    #[test]
    fn watch_header_states() {
        let w = WatchCall {
            label: "watch -n 2 ls".into(),
            kind: crate::watch::WatchKind::Command,
            status: WatchStatus::Done,
            detail: Some("第 2 轮".into()),
            duration_ms: 9000,
        };
        let h = Activity::new(ActivityKind::Watch(w));
        let lines = render_lines(&h);
        assert_eq!(text(&lines[0]), "⏺ watch -n 2 ls");
        assert_eq!(text(&lines[1]), "  ⎿  第 2 轮 · Ran in 9.0s");
    }
}
