//! Agent activities (thinking / tool / diff / watch).
//!
//! Ported from rsmarkdown-tui `activities.rs`: an activity is a collapsible
//! panel — one header row when collapsed, header + content when expanded.
//! Only the kinds bingo uses are kept (Thinking / Tool / Diff / Watch);
//! SubAgent is presented by bingo as a Tool.

use crate::tui::line::Line;
use crate::tui::theme::Theme;
use crate::watch::WatchState;

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

/// Tool-call lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    /// Running.
    Running,
    /// Finished successfully.
    Done,
    /// Failed.
    Error,
    /// Cut short by the user's interrupt: it neither finished nor failed, and the row must
    /// not claim either.
    Interrupted,
}

/// One tool call: `✓ bash · cargo test · 12ms`.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Tool name (e.g. `bash`, `Edit`).
    pub name: &'static str,
    /// Lifecycle status.
    pub status: ToolStatus,
    /// Command/argument summary.
    pub summary: String,
    /// Milliseconds elapsed.
    pub duration_ms: u64,
    /// Single-line output preview for the header.
    pub output: Option<String>,
    /// Result summary shown when expanded (CC `⎿ Read 173 lines`), rendered
    /// before the content.
    pub result_summary: Option<String>,
}

impl ToolCall {
    /// Create a tool call that is still running.
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

/// A watched entity (command/agent): header + round detail + expandable
/// content.
#[derive(Debug, Clone)]
pub struct WatchCall {
    /// Description (e.g. `watch -n 2 ls`, `reviewer · review commits`).
    pub label: String,
    /// Category (icon: ⏺ command / ◉ subagent).
    pub kind: crate::watch::WatchKind,
    /// Lifecycle status.
    pub status: WatchState,
    /// Current round/progress description.
    pub detail: Option<String>,
    /// Milliseconds elapsed.
    pub duration_ms: u64,
}

/// Whether a thinking block is still running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingState {
    /// Still reasoning.
    Running,
    /// Reasoning finished.
    Done,
}

/// A thinking block: `✻ Thinking` (running/done share the header; the running
/// verb and the elapsed time live only in the bottom status line).
/// The completion line `✻ Churned for 1.4s` is rendered by
/// [`thinking_completion_line`] at the end of the message.
#[derive(Debug, Clone)]
pub struct Thinking {
    /// Running/done.
    pub state: ThinkingState,
    /// Milliseconds elapsed.
    pub duration_ms: u64,
    /// Distinguishes consecutive reasoning phases (running/done updates
    /// replace the correct block).
    pub stage: &'static str,
    /// Random verb for the done state (`✻ Churned for 40s`);
    /// `None` falls back to `stage`.
    pub done_verb: Option<&'static str>,
    /// Host tick (per-block independent timing start).
    pub start_tick: u64,
    /// Number of aggregated reasoning segments (several thinking segments
    /// between body text merge into one block; the folded row shows
    /// `✻ Thinking · N segments`).
    pub segments: usize,
}

/// Task lifecycle (pending → in_progress → completed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    /// Not started.
    Pending,
    /// In progress.
    InProgress,
    /// Completed.
    Done,
}

/// A task item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoItem {
    /// Task text.
    pub text: String,
    /// Lifecycle status.
    pub status: TodoStatus,
}

/// One line of a unified diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    /// Unchanged context line.
    Context(String),
    /// Removed line (`-`).
    Removed(String),
    /// Added line (`+`).
    Added(String),
}

/// One hunk of a unified diff.
#[derive(Debug, Clone)]
pub struct Hunk {
    /// `@@ -a,b +c,d @@`
    pub header: String,
    /// Context / removed / added lines.
    pub lines: Vec<DiffLine>,
}

/// One file edit, presented as a git-style unified diff.
#[derive(Debug, Clone)]
pub struct Diff {
    /// File path.
    pub path: String,
    /// Hunks in order.
    pub hunks: Vec<Hunk>,
}

impl Diff {
    /// Count added/removed lines across all hunks.
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

    /// Parse a unified diff (git format: `---` / `+++` / `@@` / `-` `+` ` `).
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

/// Render a diff as styled lines: `@@` hunk headers, `-` red, `+` green.
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

/// Activity kind.
#[derive(Debug, Clone)]
pub enum ActivityKind {
    /// Thinking block.
    Thinking(Thinking),
    /// Tool call.
    Tool(ToolCall),
    /// File edit (unified diff).
    Diff(Diff),
    /// A watched entity.
    Watch(WatchCall),
}

/// A collapsible activity: one header row + optional expandable content. All
/// kinds share the expand/collapse capability; only the presentation differs.
#[derive(Debug, Clone)]
pub struct Activity {
    /// Activity kind.
    pub kind: ActivityKind,
    /// Collapsed (false) or expanded (true).
    pub expanded: bool,
    /// Content shown when expanded (reasoning text / tool I/O).
    pub content: Vec<Line>,
    /// Whether this came from an auto-expand rule (activity still active)
    /// rather than a user click.
    pub auto_expanded: bool,
    /// Expand hint text (e.g. `"ctrl+o to expand"`).
    pub expand_hint: Option<String>,
}

impl Activity {
    /// Create a collapsed activity.
    pub fn new(kind: ActivityKind) -> Self {
        Self {
            kind,
            expanded: false,
            content: Vec::new(),
            auto_expanded: false,
            expand_hint: None,
        }
    }

    /// Expand or collapse (a manual toggle takes over the auto-expand rule).
    pub fn toggle(&mut self) {
        self.expanded = !self.expanded;
        self.auto_expanded = false;
    }

    /// Whether expanding reveals any content. Production reads the content
    /// directly (the layout decides row by row); this is the predicate the
    /// retention tests assert on.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn expandable(&self) -> bool {
        !self.content.is_empty()
    }

    /// Set the expandable content.
    pub fn set_content(&mut self, content: Vec<Line>) {
        self.content = content;
    }

    /// Whether the activity is still changing (row content updates with
    /// ticks/events): a running thinking block, a running tool, a running
    /// watch. REPL mode uses this to decide whether a message can be
    /// finalized.
    pub fn is_running(&self) -> bool {
        match &self.kind {
            ActivityKind::Thinking(t) => t.state == ThinkingState::Running,
            ActivityKind::Tool(t) => t.status == ToolStatus::Running,
            ActivityKind::Watch(w) => w.status == WatchState::Running,
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

/// Thinking-block header: running/done share the shape `✻ Thinking` (dim
/// italic). The running verb, spinner and elapsed time appear only in the
/// bottom status line, to avoid repetition.
fn thinking_header(theme: &Theme) -> Line {
    Line::styled("✻ Thinking", theme.thinking())
}

/// Thinking completion line: `✻ {done_verb} for 40.0s`, rendered at the end
/// of the message (after the body and all tools).
pub fn thinking_completion_line(t: &Thinking, theme: &Theme) -> Line {
    let verb = t.done_verb.unwrap_or(t.stage);
    Line::styled(
        format!("✻ {verb} for {:.1}s", t.duration_ms as f64 / 1000.0),
        theme.thinking(),
    )
}

/// Status colour of the leading marker: running is muted, done is green,
/// failure is red, an interrupted call is amber (it did not fail — it was stopped).
fn dot_style(status: ToolStatus, theme: &Theme) -> crate::tui::line::SegStyle {
    match status {
        ToolStatus::Running => theme.dim(),
        ToolStatus::Done => theme.tool_done(),
        ToolStatus::Error => theme.tool_error(),
        ToolStatus::Interrupted => theme.tool_interrupted(),
    }
}

/// Tool-category icon: built-in `⏺` / MCP `◆` / Skill `✦`. Shape encodes
/// category, colour encodes status (dot_style unchanged); agents have no tool
/// row, their watch-row icon lives in [`watch_header`].
pub fn tool_glyph(name: &str) -> &'static str {
    if name.starts_with("mcp__") {
        "◆ "
    } else if name == "Skill" {
        "✦ "
    } else {
        "⏺ "
    }
}

/// The MCP full name `mcp__server__tool` displays as `server:tool`;
/// permission rules still use the full name.
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
        // The state is the whole result: an interrupted call has no output to summarize,
        // and borrowing one from a half-finished run would read as completion.
        ToolStatus::Interrupted => "Interrupted".to_string(),
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
    let style = match t.status {
        ToolStatus::Error => theme.tool_error(),
        ToolStatus::Interrupted => theme.tool_interrupted(),
        _ => theme.dim(),
    };
    let mut line = Line::styled(format!("{RESULT_CONNECTOR}{body}"), style);
    if let Some(hint) = expand_hint(act) {
        line.push_styled(format!(" ({hint})"), theme.dim());
    }
    line
}

/// `⏺ watch -n 2 ls` — same shape as a tool, driven by the watch lifecycle.
/// A subagent's watch row (`WatchKind::Agent`) uses `◉`: a core inside a
/// ring, a session inside a session.
fn watch_header(w: &WatchCall, theme: &Theme, portrait: Option<&Portrait>) -> Line {
    let style = match w.status {
        WatchState::Running | WatchState::Idle => theme.dim(),
        WatchState::Done => theme.tool_done(),
        WatchState::Failed => theme.tool_error(),
        WatchState::Cancelled => theme.dim(),
    };
    // A subagent's row names a speaker, so it wears that speaker's face where one
    // can be drawn; a channel and a command are not people and keep their glyph.
    let mut line = match portrait {
        Some(p) => p.top.clone(),
        None => {
            let glyph = match w.kind {
                crate::watch::WatchKind::Agent => "◉ ",
                crate::watch::WatchKind::Channel => "◇ ",
                crate::watch::WatchKind::Command => "⏺ ",
            };
            Line::styled(glyph, style)
        }
    };
    line.push_styled(w.label.clone(), theme.text());
    line
}

fn watch_result(w: &WatchCall, act: &Activity, theme: &Theme, portrait: Option<&Portrait>) -> Line {
    let mut body = match (&w.detail, w.status) {
        (Some(detail), _) => detail.clone(),
        (None, WatchState::Running) => "Running…".to_string(),
        (None, WatchState::Idle) => "Waiting…".to_string(),
        (None, WatchState::Done) => "Done".to_string(),
        (None, WatchState::Failed) => "Failed".to_string(),
        (None, WatchState::Cancelled) => "Cancelled".to_string(),
    };
    if w.status != WatchState::Running && w.duration_ms > SLOW_TOOL_MS {
        body.push_str(&format!(" · Ran in {:.1}s", w.duration_ms as f64 / 1000.0));
    }
    let style = if w.status == WatchState::Failed {
        theme.tool_error()
    } else {
        theme.dim()
    };
    // The portrait's second row stands in for the `⎿` connector: it occupies the
    // same gutter columns, so the body still hangs where the eye expects it.
    let mut line = match portrait {
        Some(p) => {
            let mut line = p.bottom.clone();
            line.push_styled(body, style);
            line
        }
        None => Line::styled(format!("{RESULT_CONNECTOR}{body}"), style),
    };
    if let Some(hint) = expand_hint(act) {
        line.push_styled(format!(" ({hint})"), theme.dim());
    }
    line
}

fn header_for(h: &Activity, theme: &Theme, portrait: Option<&Portrait>) -> Line {
    match &h.kind {
        ActivityKind::Thinking(_) => thinking_header(theme),
        ActivityKind::Tool(t) => tool_header(t, theme),
        ActivityKind::Diff(d) => diff_header(d, theme),
        ActivityKind::Watch(w) => watch_header(w, theme, portrait),
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
/// (thinking): `… +N lines (ctrl+o to expand)`; aggregated blocks show
/// `· N segments (ctrl+o to expand)`.
fn fold_tail(act: &Activity) -> Option<String> {
    if act.expanded || act.content.is_empty() {
        return None;
    }
    match &act.kind {
        ActivityKind::Thinking(t) => {
            let mut tail = if t.segments > 1 {
                format!("· {} segments", t.segments)
            } else {
                format!("… +{} lines", act.content.len())
            };
            if let Some(hint) = &act.expand_hint {
                tail.push_str(&format!(" ({hint})"));
            }
            Some(tail)
        }
        _ => None,
    }
}

/// The two gutter cells a named speaker's portrait occupies, already built by the
/// caller. This module stays free of palettes and image capabilities: it is handed
/// finished cells, exactly as it is handed a finished markdown renderer.
///
/// A watch row is a header plus a result row, which is the height a portrait wants,
/// so the face fits the block it already had. The cost is the `⎿` connector on
/// those rows — the portrait spans both and says the same thing by other means.
#[derive(Debug, Clone)]
pub struct Portrait {
    pub top: Line,
    pub bottom: Line,
}

/// The clickable row range of one activity (document coordinates).
#[derive(Debug, Clone)]
pub struct ActivityRowRange {
    /// First row (inclusive).
    pub start: u16,
    /// Last row (exclusive).
    pub end: u16,
    /// Activity path (`[i]` message-level activity; always a single element
    /// without nested subagents).
    pub path: Vec<usize>,
}

/// Lay out a single activity: header + result line + (when expanded) content,
/// returning the rows and the clickable range.
///
/// No spinner parameter: activity rows are static — the running animation
/// lives only in the bottom status line (CC semantics), so a finalized
/// activity row never changes again.
///
/// `render_reply` renders a subagent's markdown reply (this module stays
/// display-independent; bingo presents SubAgent as a Tool, so no recursion is
/// needed — the parameter is kept to match the original contract).
pub fn layout_activity(
    act: &Activity,
    path: &[usize],
    base_row: u16,
    theme: &Theme,
    portrait: Option<&Portrait>,
    render_reply: &mut dyn FnMut(&str) -> Vec<Line>,
) -> (Vec<Line>, Vec<ActivityRowRange>) {
    let mut header = header_for(act, theme, portrait);
    if let Some(tail) = fold_tail(act) {
        header.push_styled(format!(" {tail}"), theme.dim());
    }
    let mut rows = vec![header];
    // Result line (CC `  ⎿  …`): tools, edits and watches always carry one —
    // it is where status, timing and the expand hint live.
    match &act.kind {
        ActivityKind::Tool(t) => rows.push(tool_result(t, act, theme)),
        ActivityKind::Diff(d) => rows.push(diff_result(d, theme)),
        ActivityKind::Watch(w) => rows.push(watch_result(w, act, theme, portrait)),
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

/// Locate an activity along a path (which may contain nested subagents),
/// mutably.
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
            segments: 1,
        }));
        if state == ThinkingState::Done {
            h.set_content(vec![Line::plain("reasoning line")]);
        }
        h
    }

    fn render_lines(h: &Activity) -> Vec<Line> {
        render_lines_with(h, None)
    }

    fn render_lines_with(h: &Activity, portrait: Option<&Portrait>) -> Vec<Line> {
        let mut render = |_: &str| Vec::new();
        let (rows, _) = layout_activity(h, &[0], 0, &Theme::dark(), portrait, &mut render);
        rows
    }

    #[test]
    fn thinking_collapsed_and_expanded() {
        // Running/done share the `✻ Thinking` header; the completion line is
        // rendered separately by thinking_completion_line.
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
        // Reasoning body is italic grey (CC italic gray).
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
    fn aggregated_thinking_shows_segment_count() {
        // Aggregated blocks show the segment count on the folded row; single
        // segments keep the line-count hint.
        let mut h = thinking("understand", ThinkingState::Done);
        if let ActivityKind::Thinking(t) = &mut h.kind {
            t.segments = 3;
        }
        let lines = render_lines(&h);
        assert_eq!(text(&lines[0]), "✻ Thinking · 3 segments");
        let single = thinking("understand", ThinkingState::Done);
        let lines = render_lines(&single);
        assert_eq!(text(&lines[0]), "✻ Thinking … +1 lines");
    }

    #[test]
    fn completion_line_uses_random_verb_and_duration() {
        // `✻ Churned for 40.0s` (random past-tense verb).
        let t = Thinking {
            state: ThinkingState::Done,
            duration_ms: 40_000,
            stage: "Churning",
            done_verb: Some("Churned"),
            start_tick: 0,
            segments: 1,
        };
        let line = thinking_completion_line(&t, &Theme::dark());
        assert_eq!(text(&line), "✻ Churned for 40.0s");
        // None falls back to stage.
        let t2 = Thinking {
            stage: "Churning",
            done_verb: None,
            ..t
        };
        let line2 = thinking_completion_line(&t2, &Theme::dark());
        assert_eq!(text(&line2), "✻ Churning for 40.0s");
    }

    /// CC two-line structure: `⏺ Bash(cmd)` + `  ⎿  {result}
    /// (ctrl+o to expand)`.
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
            "result rows carry an expand hint; quick commands show no timing"
        );
        assert_eq!(lines.len(), 2, "collapsed: header + result");

        h.toggle();
        let lines = render_lines(&h);
        assert_eq!(
            text(&lines[1]),
            "  ⎿  54 passed",
            "no longer hints after expansion"
        );
        assert!(text(&lines[2]).starts_with("     $ cargo test -p core"));
        assert!(text(&lines[3]).contains("54 passed; 0 failed"));
    }

    /// Running: the header row appears immediately, the result row reads
    /// `Running…` (the spinner lives in the bottom status line).
    #[test]
    fn running_tool_shows_running_result_line() {
        let h = Activity::new(ActivityKind::Tool(ToolCall::running("Read", "src/main.rs")));
        let lines = render_lines(&h);
        assert_eq!(text(&lines[0]), "⏺ Read(src/main.rs)");
        assert_eq!(text(&lines[1]), "  ⎿  Running…");
        // The running dot uses the weak colour and turns green when done.
        assert_eq!(lines[0].segs[0].style.fg, Some(Theme::dark().inactive));
    }

    /// Slow commands (>2s) fold the duration into the result line; error
    /// lines use the error colour.
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

    /// D76: a call the user stopped is neither done nor failed. It reads `Interrupted` in
    /// the warning colour — the green completion glyph used to claim a result that was
    /// never produced.
    #[test]
    fn interrupted_tool_is_amber_and_says_interrupted() {
        let stopped = Activity::new(ActivityKind::Tool(ToolCall {
            name: "Bash",
            status: ToolStatus::Interrupted,
            summary: "sleep 30".into(),
            duration_ms: 0,
            // Even with output on hand, the state is the result.
            output: Some("partial output".into()),
            result_summary: None,
        }));
        let lines = render_lines(&stopped);
        assert_eq!(text(&lines[0]), "⏺ Bash(sleep 30)");
        assert_eq!(text(&lines[1]), "  ⎿  Interrupted");
        let warning = Some(Theme::dark().warning);
        assert_eq!(lines[0].segs[0].style.fg, warning, "glyph");
        assert_eq!(lines[1].segs[0].style.fg, warning, "result line");
        assert_ne!(
            lines[0].segs[0].style.fg,
            Some(Theme::dark().success),
            "never wears the completion colour"
        );
    }

    /// Edit/Write: `⏺ Update(path)` + `  ⎿  Updated path with N additions…`.
    #[test]
    fn diff_renders_update_header_and_result() {
        let diff =
            Diff::parse_unified("--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,2 @@\n a\n-b\n+c\n+d\n");
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
        assert_eq!(
            spinner(SPINNERS.len() as u64),
            a,
            "cycles after full rotation"
        );
        // Starburst glyph (CC): no longer braille.
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

    /// Category icons: ⏺ built-in / ◆ MCP (displayed as server:tool) /
    /// ✦ Skill / ◉ Agent watch. Shape encodes category, colour keeps
    /// encoding status.
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
            "MCP full names show as server:tool"
        );
        // The status colour stays on the icon: Done turns green.
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
            label: "reviewer · organizing notes".into(),
            kind: crate::watch::WatchKind::Agent,
            status: WatchState::Running,
            detail: None,
            duration_ms: 0,
        }));
        assert_eq!(
            text(&render_lines(&agent_watch)[0]),
            "◉ reviewer · organizing notes"
        );

        let channel_watch = Activity::new(ActivityKind::Watch(WatchCall {
            label: "#table".into(),
            kind: crate::watch::WatchKind::Channel,
            status: WatchState::Running,
            detail: Some("3 msgs · latest a: report".into()),
            duration_ms: 0,
        }));
        let lines = render_lines(&channel_watch);
        assert_eq!(text(&lines[0]), "◇ #table");
        assert_eq!(text(&lines[1]), "  ⎿  3 msgs · latest a: report");
    }

    #[test]
    fn watch_header_states() {
        let w = WatchCall {
            label: "watch -n 2 ls".into(),
            kind: crate::watch::WatchKind::Command,
            status: WatchState::Done,
            detail: Some("round 2".into()),
            duration_ms: 9000,
        };
        let h = Activity::new(ActivityKind::Watch(w));
        let lines = render_lines(&h);
        assert_eq!(text(&lines[0]), "⏺ watch -n 2 ls");
        assert_eq!(text(&lines[1]), "  ⎿  round 2 · Ran in 9.0s");
    }
}
