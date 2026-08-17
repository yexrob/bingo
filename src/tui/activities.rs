//! Agent activities (thinking / tool / diff / watch).
//!
//! Ported from rsmarkdown-tui `activities.rs`: an activity is a collapsible
//! panel — one header row when collapsed, header + content when expanded.
//! Only the kinds bingo uses are kept (Thinking / Tool / Diff / Watch);
//! SubAgent is presented by bingo as a Tool.

use crate::tui::line::Line;
use crate::tui::theme::Theme;
use crate::watch::WatchState;

/// Result connector under a tool header (CC `  ⎿  `). Continuation lines line
/// up with the text after it.
pub const RESULT_CONNECTOR: &str = "  ⎿  ";
/// Indentation of the lines that continue a result block.
pub const RESULT_INDENT: &str = "     ";

/// Narrowest code column a diff row will wrap to. Below this the gutter would
/// be eating the content it exists to index, so the row is allowed to overrun
/// and be clipped instead — the same thing every other over-wide row does.
const MIN_DIFF_BODY: usize = 16;
/// Only commands slower than this get a duration on their result line —
/// a millisecond count on every row is noise.
pub const SLOW_TOOL_MS: u64 = 2_000;

/// Progress lines shown under a running dispatch row — CC's
/// `MAX_PROGRESS_MESSAGES_TO_SHOW` (`tools/AgentTool/UI.tsx:33`).
pub const PROGRESS_LINES: usize = 3;

/// What a dispatch row says before its instance has done anything —
/// CC `INITIALIZING_TEXT` (`tools/AgentTool/UI.tsx:443`).
pub const INITIALIZING: &str = "Initializing…";

/// The opening of the condensed progress line CC falls back to when the window
/// is too short to carry the per-tool rows (`tools/AgentTool/UI.tsx:497`).
pub const IN_PROGRESS: &str = "In progress…";

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
    /// The protocol's own id for this call, recorded at `ToolReady` so the
    /// answer can find the call that made it.
    ///
    /// A round runs its tools concurrently and they do not come back in call
    /// order, so matching an answer to "the first running call with this name"
    /// puts one `Read`'s output under another `Read`'s row. `None` for calls
    /// the protocol never named — the `!` command's synthesized rows — which
    /// fall back to that name match.
    pub id: Option<String>,
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
            id: None,
        }
    }
}

/// What a dispatched run has cost so far: the two numbers CC's `Done (…)` line
/// and its condensed progress line are both built from
/// (`tools/AgentTool/UI.tsx:376`, `:497-499`).
///
/// Sampled from the registry while the run is alive and **frozen at the
/// terminal event**, because the registry drops a run's progress the instant it
/// finishes (`spawn_agent_loop` clears it one line before it reports `Done`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunStats {
    pub tool_uses: usize,
    pub tokens: u64,
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
    /// Live progress under a running dispatch row (D106): the last
    /// [`PROGRESS_LINES`] activity lines of the instance, oldest first, exactly
    /// as CC shows the last three of a subagent's progress messages
    /// (`tools/AgentTool/UI.tsx:33`, `:510`).
    ///
    /// Transient by construction: a message holding a *running* watch row never
    /// settles ([`crate::tui::chat::Chat::message_settled`] asks
    /// `Activity::is_running`), so these rows can never reach scrollback. What
    /// does reach it is the settled form built from [`WatchCall::run_stats`].
    pub progress: Vec<String>,
    /// The run's cost. Live while the run is, frozen at the terminal event —
    /// which is the row that settles.
    pub run_stats: Option<RunStats>,
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
    /// Whether a clock was taken (D130). The console times every block it
    /// streams, even one that finishes inside a tick; a page rebuilt from
    /// history has no measurement to report, because none is in the record.
    /// [`thinking_completion_line`] is that report, so `false` suppresses it —
    /// a zero would otherwise read as "measured, and instant".
    pub timed: bool,
}

/// Task lifecycle (pending → in_progress → completed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TodoStatus {
    /// Not started.
    #[default]
    Pending,
    /// In progress.
    InProgress,
    /// Completed.
    Done,
}

/// A task item.
///
/// Carries the store's `id`, `owner` and `blocked_by` since D104: the panel
/// names an owner who is a live instance and marks what a task is waiting on,
/// and both are answers the row can only give if the snapshot brought them.
/// **Display only** — there is no assignment protocol and no claiming here.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TodoItem {
    /// Store id, which is what `blocked_by` names.
    pub id: String,
    /// Task text.
    pub text: String,
    /// Lifecycle status.
    pub status: TodoStatus,
    /// Who the store says is on it. Rendered only when it resolves to an
    /// instance that is still on the roster.
    pub owner: Option<String>,
    /// Ids this task is waiting on. Rendered as those of them that are not done.
    pub blocked_by: Vec<String>,
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
    /// First line number on the old side (`a`), 1-based.
    pub old_start: usize,
    /// First line number on the new side (`c`), 1-based.
    pub new_start: usize,
    /// Context / removed / added lines.
    pub lines: Vec<DiffLine>,
}

impl Hunk {
    /// Walk the hunk, pairing each line with the numbers it carries: context
    /// advances both sides, an addition only the new one, a removal only the
    /// old one. This is the whole arithmetic behind the gutter.
    fn numbered(&self) -> Vec<(Option<usize>, Option<usize>, &DiffLine)> {
        let mut old = self.old_start;
        let mut new = self.new_start;
        let mut out = Vec::with_capacity(self.lines.len());
        for line in &self.lines {
            let entry = match line {
                DiffLine::Context(_) => {
                    let e = (Some(old), Some(new), line);
                    old += 1;
                    new += 1;
                    e
                }
                DiffLine::Removed(_) => {
                    let e = (Some(old), None, line);
                    old += 1;
                    e
                }
                DiffLine::Added(_) => {
                    let e = (None, Some(new), line);
                    new += 1;
                    e
                }
            };
            out.push(entry);
        }
        out
    }
}

/// Parse the two starting line numbers out of `@@ -a,b +c,d @@`.
///
/// A malformed or absent header falls back to 1/1 rather than refusing to
/// render: a diff we cannot number is still a diff worth showing.
fn hunk_starts(header: &str) -> (usize, usize) {
    let number = |part: Option<&str>, sigil: char| -> usize {
        part.and_then(|p| p.strip_prefix(sigil))
            .map(|p| p.split(',').next().unwrap_or(p))
            .and_then(|p| p.parse::<usize>().ok())
            .unwrap_or(1)
    };
    let mut parts = header.trim_start_matches('@').split_whitespace();
    let old = number(parts.next(), '-');
    let new = number(parts.next(), '+');
    (old, new)
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
    /// Digits the line-number gutter needs: the widest number anywhere in the
    /// diff, so the code column sits at the same place in every hunk. A diff
    /// that crosses from line 99 to line 100 does not shift under the reader.
    fn gutter_digits(&self) -> usize {
        let mut max = 1;
        for hunk in &self.hunks {
            for (old, new, _) in hunk.numbered() {
                max = max.max(old.unwrap_or(0)).max(new.unwrap_or(0));
            }
        }
        max.to_string().len()
    }

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
                let (old_start, new_start) = hunk_starts(&header);
                hunks.push(Hunk {
                    header,
                    old_start,
                    new_start,
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

/// Render a diff as styled lines: `@@` hunk headers, a muted old/new line-number
/// gutter, then `-` red / `+` green.
///
/// `width` is the display width the rows have to live in *after* whatever the
/// caller indents them by. Code lines are wrapped rather than clipped, and a
/// continuation row gets a blank gutter and a blank marker column, so the code
/// column stays a straight edge no matter how long a line is.
///
/// The hunk header stays flush left. It is a statement about the numbers, not a
/// numbered line, and indenting it into the gutter would say otherwise.
pub fn diff_lines(d: &Diff, theme: &Theme, width: usize) -> Vec<Line> {
    let digits = d.gutter_digits();
    // "{old:>digits} {new:>digits} " — two columns, one space between, one after.
    let gutter_width = digits * 2 + 2;
    // …and one more column for the -/+/space marker.
    let body = width.saturating_sub(gutter_width + 1).max(MIN_DIFF_BODY);
    let blank_gutter = " ".repeat(gutter_width);
    let mut out = Vec::new();
    for hunk in &d.hunks {
        out.push(Line::styled(hunk.header.clone(), theme.diff_hunk()));
        for (old, new, line) in hunk.numbered() {
            let (marker, style) = match line {
                DiffLine::Context(_) => (' ', theme.diff_context()),
                DiffLine::Removed(_) => ('-', theme.diff_removed()),
                DiffLine::Added(_) => ('+', theme.diff_added()),
            };
            let (DiffLine::Context(text) | DiffLine::Removed(text) | DiffLine::Added(text)) = line;
            let cell = |n: Option<usize>| match n {
                Some(n) => format!("{n:>digits$}"),
                None => " ".repeat(digits),
            };
            let gutter = format!("{} {} ", cell(old), cell(new));
            for (i, chunk) in wrap_columns(text, body).into_iter().enumerate() {
                let mut row = Line::styled(
                    if i == 0 {
                        gutter.clone()
                    } else {
                        blank_gutter.clone()
                    },
                    theme.muted(),
                );
                row.push_styled(
                    format!("{}{chunk}", if i == 0 { marker } else { ' ' }),
                    style,
                );
                out.push(row);
            }
        }
    }
    out
}

/// Hard-wrap by display columns (CJK-aware).
///
/// Code does not wrap on words — a break mid-identifier is honest, a break that
/// silently reflows indentation is not — so this splits on columns and nothing
/// else. Always returns at least one chunk, so an empty line still gets a row.
fn wrap_columns(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut col = 0usize;
    for ch in text.chars() {
        let w = crate::tui::line::char_width(ch);
        if col + w > width && !current.is_empty() {
            out.push(std::mem::take(&mut current));
            col = 0;
        }
        current.push(ch);
        col += w;
    }
    out.push(current);
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
///
/// `settling` is the [`crate::tui::motion::Motion::settle`] token: for one
/// 120 ms window after the turn ends the line carries the accent, then rests.
/// The blink happens while the row is still live — it freezes into scrollback
/// at rest, so write-once is never broken by it.
pub fn thinking_completion_line(t: &Thinking, theme: &Theme, settling: bool) -> Line {
    let verb = t.done_verb.unwrap_or(t.stage);
    let text = format!("✻ {verb} for {:.1}s", t.duration_ms as f64 / 1000.0);
    if settling {
        return Line::styled(text, crate::tui::line::SegStyle::fg(theme.claude));
    }
    Line::styled(text, theme.thinking())
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
        line.push_styled(format!(" ({hint})"), theme.muted());
    }
    line
}

/// Who a run's watch label addresses. Every shape a dispatch label takes —
/// `scout · fix the parser`, `scout #3 · look again`, `scout #7 receipt` —
/// opens with the instance name, and that is the only part of it any surface
/// needs in order to find the agent's colour, its face or its row.
pub fn watch_instance(label: &str) -> &str {
    label.split_whitespace().next().unwrap_or(label)
}

/// What a run's watch label says the run is *for*: everything after the first
/// ` · `. A label without one (the ack watchdog's `scout #7 receipt`) has no
/// description, and the row says nothing rather than repeating the address.
pub fn watch_description(label: &str) -> &str {
    label.split_once(" · ").map(|(_, rest)| rest).unwrap_or("")
}

/// `⏺ watch -n 2 ls` — same shape as a tool, driven by the watch lifecycle.
/// A subagent's watch row (`WatchKind::Agent`) uses `◉`: a core inside a
/// ring, a session inside a session.
///
/// **A dispatch row is written `@scout: fix the parser`** (D106), which is CC's
/// own shape for a *named* spawn: `AgentProgressLine`'s `hideType` branch is
/// `<Text bold>{name}</Text><Text dimColor>: {description}</Text>`
/// (`components/AgentProgressLine.tsx` sourcesContent, and
/// `tools/AgentTool/UI.tsx:687` where a named spawn's `agentType` becomes
/// `@name`). It is also, letter for letter, the shape D104 gave the agent
/// tree's rows — so the same run reads the same way wherever it is drawn.
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
    if w.kind == crate::watch::WatchKind::Agent {
        line.push_styled(format!("@{}", watch_instance(&w.label)), theme.text());
        let description = watch_description(&w.label);
        if !description.is_empty() {
            line.push_styled(format!(": {description}"), theme.dim());
        }
    } else {
        line.push_styled(w.label.clone(), theme.text());
    }
    line
}

/// `Done (12 tool uses · 8.3k tokens · 1m 4s)` — CC
/// `tools/AgentTool/UI.tsx:376-377`, whose three parts are joined by ` · ` and
/// whose duration is `formatDuration`. The stats formatter is D104's, shared
/// rather than forked; the duration formatter is D104's too.
pub fn dispatch_done_line(stats: Option<RunStats>, duration_ms: u64) -> String {
    let stats = stats.unwrap_or_default();
    format!(
        "Done ({} · {})",
        crate::tui::tree::stats_body(stats.tool_uses, stats.tokens),
        crate::tui::tree::duration_label(std::time::Duration::from_millis(duration_ms))
    )
}

/// `In progress… · 3 tool uses · 8.3k tokens` — CC's condensed progress line
/// (`tools/AgentTool/UI.tsx:495-503`), the one it falls back to when the window
/// is too short to carry a row per tool.
pub fn dispatch_condensed_line(stats: Option<RunStats>) -> String {
    let stats = stats.unwrap_or_default();
    format!(
        "{IN_PROGRESS} · {}",
        crate::tui::tree::stats_body(stats.tool_uses, stats.tokens)
    )
}

/// What hangs under a dispatch row (D106). One entry per row, first on the `⎿`
/// connector and the rest on [`RESULT_INDENT`].
///
/// - **running**, room to spare: the instance's last [`PROGRESS_LINES`]
///   activity lines, oldest first — CC keeps the tail of its progress messages
///   and renders each in condensed style (`tools/AgentTool/UI.tsx:510`,
///   `:553-556`); bingo's `recent_activity` entries are already exactly that
///   line (`⏺ Read(src/lexer.rs)`), built by the same `tool_glyph` /
///   `display_tool_name` / `summarize_input` the console uses. CC's grouping of
///   consecutive read/search calls is not ported: its own comment marks it
///   *ants only* (`:501`), so the shipped renderer prints the rows as they come.
/// - **running**, short window: one [`dispatch_condensed_line`].
/// - **finished**: one [`dispatch_done_line`], which is the form that settles.
fn dispatch_body(w: &WatchCall, narrow: bool) -> Vec<String> {
    match w.status {
        WatchState::Done => vec![dispatch_done_line(w.run_stats, w.duration_ms)],
        WatchState::Running | WatchState::Idle => {
            if w.progress.is_empty() {
                return vec![INITIALIZING.to_string()];
            }
            if narrow {
                return vec![dispatch_condensed_line(w.run_stats)];
            }
            w.progress
                .iter()
                .rev()
                .take(PROGRESS_LINES)
                .rev()
                .cloned()
                .collect()
        }
        // A failure keeps D98's wording and its colour: the reason the run gave
        // is the only useful thing left to say, and the `⚠` alert line below
        // repeats the name for a reader who is not looking here.
        WatchState::Failed => vec![w.detail.clone().unwrap_or_else(|| "Failed".to_string())],
        WatchState::Cancelled => vec![w.detail.clone().unwrap_or_else(|| "Cancelled".to_string())],
    }
}

/// The rows under a watch row's header. Commands and channels keep their single
/// state line; a dispatch gets [`dispatch_body`], which is one row while it is
/// finished or the window is short and up to [`PROGRESS_LINES`] while it works.
fn watch_result(
    w: &WatchCall,
    act: &Activity,
    theme: &Theme,
    portrait: Option<&Portrait>,
    narrow: bool,
) -> Vec<Line> {
    let agent = w.kind == crate::watch::WatchKind::Agent;
    let mut bodies = if agent {
        dispatch_body(w, narrow)
    } else {
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
        vec![body]
    };
    if bodies.is_empty() {
        bodies.push(INITIALIZING.to_string());
    }
    let style = if w.status == WatchState::Failed {
        theme.tool_error()
    } else {
        theme.dim()
    };
    let mut rows: Vec<Line> = Vec::with_capacity(bodies.len());
    for (i, body) in bodies.into_iter().enumerate() {
        // The portrait's second row stands in for the `⎿` connector: it occupies
        // the same gutter columns, so the body still hangs where the eye expects
        // it. Only the first row can take it — a face is two cells tall.
        let mut line = match (i, portrait) {
            (0, Some(p)) => {
                let mut line = p.bottom.clone();
                line.push_styled(body, style);
                line
            }
            (0, None) => Line::styled(format!("{RESULT_CONNECTOR}{body}"), style),
            _ => Line::styled(format!("{RESULT_INDENT}{body}"), style),
        };
        if i == 0
            && let Some(hint) = expand_hint(act)
        {
            line.push_styled(format!(" ({hint})"), theme.muted());
        }
        rows.push(line);
    }
    rows
}

fn header_for(h: &Activity, theme: &Theme, portrait: Option<&Portrait>) -> Line {
    match &h.kind {
        ActivityKind::Thinking(_) => thinking_header(theme),
        ActivityKind::Tool(t) => tool_header(t, theme),
        ActivityKind::Diff(d) => diff_header(d, theme),
        ActivityKind::Watch(w) => watch_header(w, theme, portrait),
    }
}

/// How a folded activity says it can be opened. One string, because a row that
/// advertised a different key than the transcript binds would be worse than a
/// row that advertised none.
pub const EXPAND_HINT: &str = "ctrl+o to expand";

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
///
/// `narrow` is CC's short-window fallback for dispatch rows
/// (`tools/AgentTool/UI.tsx:469`): the caller measures the window, this decides
/// what a row may spend on it.
pub fn layout_activity(
    act: &Activity,
    path: &[usize],
    base_row: u16,
    theme: &Theme,
    portrait: Option<&Portrait>,
    narrow: bool,
    render_reply: &mut dyn FnMut(&str) -> Vec<Line>,
) -> (Vec<Line>, Vec<ActivityRowRange>) {
    let mut header = header_for(act, theme, portrait);
    if let Some(tail) = fold_tail(act) {
        header.push_styled(format!(" {tail}"), theme.muted());
    }
    let mut rows = vec![header];
    // Result line (CC `  ⎿  …`): tools, edits and watches always carry one —
    // it is where status, timing and the expand hint live.
    match &act.kind {
        ActivityKind::Tool(t) => rows.push(tool_result(t, act, theme)),
        ActivityKind::Diff(d) => rows.push(diff_result(d, theme)),
        ActivityKind::Watch(w) => rows.extend(watch_result(w, act, theme, portrait, narrow)),
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
            timed: true,
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
        let (rows, _) = layout_activity(h, &[0], 0, &Theme::dark(), portrait, false, &mut render);
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
            timed: true,
        };
        let line = thinking_completion_line(&t, &Theme::dark(), false);
        assert_eq!(text(&line), "✻ Churned for 40.0s");
        // None falls back to stage.
        let t2 = Thinking {
            stage: "Churning",
            done_verb: None,
            ..t
        };
        let line2 = thinking_completion_line(&t2, &Theme::dark(), false);
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
            id: None,
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
        assert_eq!(
            lines[0].segs[0].style.fg,
            Some(Theme::dark().text_secondary)
        );
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
            id: None,
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
            id: None,
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
            id: None,
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

    /// The gutter's arithmetic, on a hunk that has all three line kinds:
    /// context advances both sides, an addition only the new one, a removal
    /// only the old one.
    #[test]
    fn diff_gutter_numbers_a_mixed_hunk() {
        let d = Diff::parse_unified(
            "--- a/f.rs\n+++ b/f.rs\n@@ -10,4 +10,4 @@\n keep\n-gone\n+new\n tail\n",
        );
        let rows: Vec<String> = diff_lines(&d, &Theme::dark(), 40)
            .iter()
            .map(text)
            .collect();
        assert_eq!(
            rows,
            vec![
                "@@ -10,4 +10,4 @@".to_string(),
                "10 10  keep".to_string(),
                "11    -gone".to_string(),
                "   11 +new".to_string(),
                "12 12  tail".to_string(),
            ],
        );
    }

    /// The gutter is sized from the largest number in the whole diff, not per
    /// hunk: a diff that crosses 99 → 100 must not shift its code column
    /// halfway down.
    #[test]
    fn diff_gutter_width_is_stable_across_digit_widths() {
        let d = Diff::parse_unified("+++ b/f.rs\n@@ -1,1 +1,1 @@\n a\n@@ -99,2 +99,2 @@\n b\n+c\n");
        let rows: Vec<String> = diff_lines(&d, &Theme::dark(), 40)
            .iter()
            .map(text)
            .collect();
        // Three digits, because the new side reaches 100.
        assert_eq!(rows[1], "  1   1  a");
        assert_eq!(rows[3], " 99  99  b");
        assert_eq!(rows[4], "    100 +c");
        // Same marker column in every row, first hunk and last alike: three
        // digits, a space, three digits, a space, then the marker.
        for row in rows.iter().filter(|r| !r.starts_with("@@")) {
            assert!(
                matches!(row.chars().nth(8), Some(' ' | '+' | '-')),
                "the marker must sit at column 8: {row:?}"
            );
        }
    }

    /// A line too long for the width wraps instead of being clipped, and the
    /// continuation carries a blank gutter and a blank marker — the code column
    /// stays a straight edge.
    #[test]
    fn diff_wrapped_lines_get_blank_gutters() {
        let long = "x".repeat(45);
        let d = Diff::parse_unified(&format!("+++ b/f.rs\n@@ -1,1 +1,1 @@\n+{long}\n"));
        // digits=1 → the gutter "  1 " is 4 wide and the marker 1, so 25 columns
        // leave a 20-column body: 45 characters need three rows.
        let rows: Vec<String> = diff_lines(&d, &Theme::dark(), 25)
            .iter()
            .map(text)
            .collect();
        assert_eq!(rows.len(), 4, "header + three wrapped rows, got {rows:?}");
        assert_eq!(rows[1], format!("  1 +{}", "x".repeat(20)));
        assert_eq!(
            rows[2],
            format!("     {}", "x".repeat(20)),
            "blank gutter, blank marker"
        );
        assert_eq!(rows[3], format!("     {}", "x".repeat(5)));
    }

    /// The gutter is muted furniture; the markers keep the colours that carry
    /// the meaning.
    #[test]
    fn diff_gutter_is_muted_and_markers_keep_their_colours() {
        let theme = Theme::dark();
        let d = Diff::parse_unified("+++ b/f.rs\n@@ -1,1 +1,2 @@\n a\n+b\n");
        let rows = diff_lines(&d, &theme, 40);
        for row in &rows[1..] {
            assert_eq!(
                row.segs[0].style.fg,
                Some(theme.text_muted),
                "gutter is tier 3"
            );
        }
        assert_eq!(rows[1].segs[1].style.fg, Some(theme.diff_context));
        assert_eq!(rows[2].segs[1].style.fg, Some(theme.success));
    }

    /// A diff with no `@@` header still renders: the numbers start at 1 rather
    /// than the rows disappearing.
    #[test]
    fn diff_without_a_parsable_header_still_numbers_from_one() {
        assert_eq!(hunk_starts("@@ -7,3 +9,4 @@"), (7, 9));
        assert_eq!(hunk_starts("@@ -7 +9 @@"), (7, 9));
        assert_eq!(hunk_starts("@@ garbage @@"), (1, 1));
        assert_eq!(hunk_starts(""), (1, 1));
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
            id: None,
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
            id: None,
        }));
        assert_eq!(text(&render_lines(&skill)[0]), "✦ Skill(review doc.md)");

        let agent_watch = Activity::new(ActivityKind::Watch(WatchCall {
            label: "reviewer · organizing notes".into(),
            kind: crate::watch::WatchKind::Agent,
            status: WatchState::Running,
            detail: None,
            duration_ms: 0,
            progress: Vec::new(),
            run_stats: None,
        }));
        assert_eq!(
            text(&render_lines(&agent_watch)[0]),
            "◉ @reviewer: organizing notes"
        );

        let channel_watch = Activity::new(ActivityKind::Watch(WatchCall {
            label: "#table".into(),
            kind: crate::watch::WatchKind::Channel,
            status: WatchState::Running,
            detail: Some("3 msgs · latest a: report".into()),
            duration_ms: 0,
            progress: Vec::new(),
            run_stats: None,
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
            progress: Vec::new(),
            run_stats: None,
        };
        let h = Activity::new(ActivityKind::Watch(w));
        let lines = render_lines(&h);
        assert_eq!(text(&lines[0]), "⏺ watch -n 2 ls");
        assert_eq!(text(&lines[1]), "  ⎿  round 2 · Ran in 9.0s");
    }

    /// Every shape a run's label takes opens with the instance name, and says
    /// what the run is for after the first ` · ` — or says nothing, which is
    /// better than repeating the address.
    #[test]
    fn a_label_names_the_instance_and_then_the_task() {
        for (label, instance, description) in [
            ("scout · fix the parser", "scout", "fix the parser"),
            ("scout #3 · look again", "scout", "look again"),
            ("scout #7 receipt", "scout", ""),
            ("scout", "scout", ""),
            ("林夏 · UI review", "林夏", "UI review"),
            // A description may carry the separator itself; only the first
            // one divides the label.
            (
                "zoe · run tests · then report",
                "zoe",
                "run tests · then report",
            ),
        ] {
            assert_eq!(watch_instance(label), instance, "{label:?}");
            assert_eq!(watch_description(label), description, "{label:?}");
        }
    }

    /// D106's dispatch row: the last three activity lines while it works, one
    /// condensed line when the window is short, and what the run cost once it
    /// is over — which is the only one of the three that ever settles.
    #[test]
    fn a_dispatch_row_says_progress_then_cost() {
        let mut w = WatchCall {
            label: "scout · fix the parser".into(),
            kind: crate::watch::WatchKind::Agent,
            status: WatchState::Running,
            detail: Some("produced 200 chars".into()),
            duration_ms: 0,
            progress: Vec::new(),
            run_stats: None,
        };

        let rows = |w: &WatchCall, narrow: bool| -> Vec<String> {
            let act = Activity::new(ActivityKind::Watch(w.clone()));
            let mut render = |_: &str| Vec::new();
            layout_activity(&act, &[0], 0, &Theme::dark(), None, narrow, &mut render)
                .0
                .iter()
                .map(text)
                .collect()
        };

        assert_eq!(
            rows(&w, false),
            vec![
                "◉ @scout: fix the parser".to_string(),
                format!("  ⎿  {INITIALIZING}"),
            ],
            "a run with nothing behind it yet"
        );

        w.progress = vec![
            "⏺ Grep(fn main)".into(),
            "⏺ Read(src/lexer.rs)".into(),
            "⏺ Bash(cargo test)".into(),
            "⏺ Edit(src/lexer.rs)".into(),
        ];
        w.run_stats = Some(RunStats {
            tool_uses: 4,
            tokens: 8_300,
        });
        assert_eq!(
            rows(&w, false),
            vec![
                "◉ @scout: fix the parser",
                "  ⎿  ⏺ Read(src/lexer.rs)",
                "     ⏺ Bash(cargo test)",
                "     ⏺ Edit(src/lexer.rs)",
            ],
            "the last three, oldest first, the first on the connector"
        );

        assert_eq!(
            rows(&w, true),
            vec![
                "◉ @scout: fix the parser",
                "  ⎿  In progress… · 4 tool uses · 8.3k tokens",
            ],
            "a short window trades the rows for the numbers"
        );

        w.status = WatchState::Done;
        w.duration_ms = 64_000;
        assert_eq!(
            rows(&w, false),
            vec![
                "◉ @scout: fix the parser",
                "  ⎿  Done (4 tool uses · 8.3k tokens · 1m 4s)",
            ],
            "and the settled form is the one that reaches scrollback"
        );

        // The `detail` a failure carries is its reason, and it keeps saying it.
        w.status = WatchState::Failed;
        w.detail = Some("connection reset".into());
        assert_eq!(rows(&w, false)[1], "  ⎿  connection reset");
    }
}
