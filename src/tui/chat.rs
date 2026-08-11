//! Incremental model for the chat state machine: messages/activities/collapse groups + document row construction.
//!
//! Ported from the old `tui.rs` `BingoChat` (ratatui edition): event handling semantics,
//! collapse detection, and expand/collapse toggling are preserved as-is; `draw` is replaced by [`Chat::build_rows`],
//! which builds transcript blocks ([`crate::tui::statics::Block`]) laid out by
//! [`crate::tui::statics::layout`] into display-agnostic styled row documents, mapped to
//! terminal rows by [`crate::tui::view`].
//! Events arrive from channels (`UiEvent` / `AskRequest`); keyboard/mouse come in via
//! [`Chat::on_key`] / [`Chat::doc_click`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::style::Color;
use rsmarkdown_core::{MarkdownProcessor, Renderer};
use tokio::sync::{mpsc, oneshot};

use crate::permission::PermissionMode;
use crate::query::{Session, run_query};
use crate::tui::activities::{
    Activity, ActivityKind, Diff, Portrait, Thinking, ThinkingState, TodoItem, TodoStatus,
    ToolCall, ToolStatus, WatchCall, activities_path_get_mut, diff_lines, layout_activity,
};
use crate::tui::avatar;
use crate::tui::gfx::{self, ImageCap};
use crate::tui::line::{Line, SegStyle, text_width, wrap_words};
use crate::tui::markdown::MarkdownRenderer;
use crate::tui::theme::{Theme, ThemeSetting};
use crate::ui::{AskRequest, DialogAction, ImageMeta, PermissionRequest, UiEvent};
use crate::watch::WatchState;

pub use crate::tui::el::{ClickTarget, Row};
pub use crate::tui::statics::{Doc, SettledMark};

use crate::tui::el::{El, LocalClick};
use crate::tui::statics::Block;

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
#[derive(Debug, Clone, Default)]
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
    /// Number of read-only subagent inspections (AgentControl list/messages).
    pub agent_checks: usize,
    /// Number of subagents stopped (AgentControl stop).
    pub agent_stops: usize,
    /// Number of subagents deleted (AgentControl delete).
    pub agent_deletes: usize,
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
    /// Looking a subagent up (AgentControl list/messages).
    AgentCheck,
    /// Stopping a subagent (AgentControl stop).
    AgentStop,
    /// Deleting a subagent (AgentControl delete).
    AgentDelete,
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

pub use crate::tui::slash::{
    COMMANDS as SLASH_COMMANDS, INSTANT_COMMANDS as INSTANT_SLASH_COMMANDS, SlashSuggestion,
};

/// `/share` flag parser (`--public` / `--open`).
fn parse_share_arg(arg: &str, flag: &str) -> bool {
    arg.split_whitespace().any(|t| t == flag)
}

/// One queued input, submitted after TurnEnd: a slash command (dispatched through
/// `run_slash`) or a plain message (`start_turn`). The marker keeps the two apart —
/// a queued slash must never reach the model as literal text.
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedInput {
    pub text: String,
    pub is_slash: bool,
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
    /// Level-one descriptions (same source as /provider: URL + auth state + protocol).
    pub provider_descs: Vec<String>,
    pub provider_selected: usize,
    /// The current provider's position in the level-one list (●; picker-model.md commit E).
    pub provider_current: Option<usize>,
    /// Level-two model list (None = still on level one).
    pub models: Option<ModelMenuModels>,
}

impl ModelMenu {
    /// Level-one list → the PickerModel core (shared row rendering / key dispatch; two-level + async stays in the shell).
    pub fn provider_picker(&self) -> crate::tui::picker::PickerModel {
        crate::tui::picker::PickerModel::new(
            self.providers
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    crate::tui::picker::PickerItem::new(
                        p.clone(),
                        p.clone(),
                        self.provider_descs.get(i).cloned().unwrap_or_default(),
                    )
                })
                .collect(),
            self.provider_selected,
            self.provider_current,
        )
    }
}

#[derive(Clone)]
pub struct ModelMenuModels {
    pub provider: String,
    /// Loaded models (filled in asynchronously; may be incomplete).
    pub models: Vec<String>,
    pub loading: bool,
    pub selected: usize,
    /// The currently active model's position in the list (● marker; computed on load).
    pub current: Option<usize>,
    /// The fetch failure reason (shown in the menu; None = success or not finished).
    pub failed: Option<String>,
}

impl ModelMenuModels {
    /// Level-two list → the PickerModel core (●/❯ dual markers, windowed rendering, number jump — the same
    /// conventions as the /provider selectors; the old hand-rolled rendering lacked these).
    pub fn picker(&self) -> crate::tui::picker::PickerModel {
        crate::tui::picker::PickerModel::new(
            self.models
                .iter()
                .map(|m| crate::tui::picker::PickerItem::new(m.clone(), m.clone(), String::new()))
                .collect(),
            self.selected,
            self.current,
        )
    }
}

/// `/think` single-level selector state (level table = off + [`crate::api::contract::THINKING_LEVELS`]).
///
/// Thin shell: state fields stay public (the test API is unchanged), interaction logic delegates to [`PickerModel`]
/// (picker-model.md: commit A, a pure refactor with zero behavior change).
#[derive(Clone)]
pub struct ThinkMenu {
    /// Browsed index (❯): moves with ↑↓/1-6, applied only on Enter/s.
    pub selected: usize,
    /// In-effect index at open time (●): fixed while browsing; the ● marker reads it.
    pub current: usize,
}

impl ThinkMenu {
    /// Thin shell → core: built from selected/current and THINK_LEVELS (shared by key dispatch and rendering).
    pub fn picker(&self) -> crate::tui::picker::PickerModel {
        crate::tui::picker::PickerModel::new(
            THINK_LEVELS
                .iter()
                .map(|(name, desc)| crate::tui::picker::PickerItem::new(*name, *name, *desc))
                .collect(),
            self.selected,
            Some(self.current),
        )
    }

    /// Scene key configuration (for the hint row).
    pub fn keys() -> crate::tui::picker::PickerKeys {
        crate::tui::picker::PickerKeys {
            session_only: true,
            number_jump: true,
        }
    }
}

/// `/theme` selector options (dark/light/auto; the ThemeSetting mapping lives in open_theme_menu).
pub const THEME_LEVELS: &[(&str, &str)] = &[
    ("dark", "dark theme"),
    ("light", "light theme"),
    ("auto", "follow the terminal background"),
];

/// `/theme` single-level selector state (thin shell, like ThinkMenu: fields public, logic delegated to PickerModel).
#[derive(Clone)]
pub struct ThemeMenu {
    /// Browsed index (❯): moves with ↑↓/1-3, applied only on Enter.
    pub selected: usize,
    /// In-effect index at open time (●).
    pub current: usize,
}

impl ThemeMenu {
    pub fn picker(&self) -> crate::tui::picker::PickerModel {
        crate::tui::picker::PickerModel::new(
            THEME_LEVELS
                .iter()
                .map(|(name, desc)| crate::tui::picker::PickerItem::new(*name, *name, *desc))
                .collect(),
            self.selected,
            Some(self.current),
        )
    }

    /// Scene key configuration: no s (theme persistence is by design), number jump 1-3.
    pub fn keys() -> crate::tui::picker::PickerKeys {
        crate::tui::picker::PickerKeys {
            session_only: false,
            number_jump: true,
        }
    }
}

/// /resume selector option cap (devex DX: sessions can be many; truncate to the latest N + a note row).
pub const RESUME_PICKER_MAX: usize = 20;

/// `/resume` session selector (picker-model.md commit C): dynamic single-level (disk snapshot),
/// Enter switches the session; label=display name, value=session name; confirmation takes the snapshot by the selected index.
#[derive(Clone)]
pub struct ResumeMenu {
    /// Browsed index (❯): moves with ↑↓/1-20, applied only on Enter.
    pub selected: usize,
    /// The current session's position in the list (●; None when absent or unset).
    pub current: Option<usize>,
    /// Session-list snapshot (same order as items; confirmation picks the Transcript by selected).
    pub transcripts: Vec<crate::transcript::Transcript>,
    /// The list was truncated (past RESUME_PICKER_MAX) → render a note row.
    pub truncated: bool,
}

impl ResumeMenu {
    pub fn picker(&self) -> crate::tui::picker::PickerModel {
        crate::tui::picker::PickerModel::new(
            self.transcripts
                .iter()
                .map(|t| {
                    let count = t.load_messages().unwrap_or_default().len();
                    crate::tui::picker::PickerItem::new(
                        t.name(),
                        t.name(),
                        format!("{count} messages"),
                    )
                })
                .collect(),
            self.selected,
            self.current,
        )
    }

    /// Scene key configuration: no s (switching sessions is the intent), number jump 1-20.
    pub fn keys() -> crate::tui::picker::PickerKeys {
        crate::tui::picker::PickerKeys {
            session_only: false,
            number_jump: true,
        }
    }
}

/// `/provider` selector (picker-model.md commit D): static single-level (default + a settings
/// providers snapshot), desc keeps the info column (URL + redacted key); Enter=switch+persist,
/// s=this session only (consistent with /think).
#[derive(Clone)]
pub struct ProviderMenu {
    /// Browsed index (❯): moves with ↑↓/1-N, applied only on Enter/s.
    pub selected: usize,
    /// The current provider's position in the list (●).
    pub current: Option<usize>,
    /// Option snapshot (name, desc): desc comes from provider_desc (url + the key's first 4 chars).
    pub options: Vec<(String, String)>,
}

impl ProviderMenu {
    pub fn picker(&self) -> crate::tui::picker::PickerModel {
        crate::tui::picker::PickerModel::new(
            self.options
                .iter()
                .map(|(name, desc)| {
                    crate::tui::picker::PickerItem::new(name.clone(), name.clone(), desc.clone())
                })
                .collect(),
            self.selected,
            self.current,
        )
    }

    /// Scene key configuration: s = this session only (switching does not write settings), number jump 1-9.
    pub fn keys() -> crate::tui::picker::PickerKeys {
        crate::tui::picker::PickerKeys {
            session_only: true,
            number_jump: true,
        }
    }
}

/// `/think` selector entries: level name + description (everything past off corresponds one-to-one with
/// THINKING_LEVELS, in the same order; consistency is guaranteed by a test).
pub const THINK_LEVELS: &[(&str, &str)] = &[
    (
        "off",
        "no thinking parameter (compatible with DeepSeek etc.)",
    ),
    ("low", "adaptive thinking · effort low"),
    ("medium", "adaptive thinking · effort medium"),
    ("high", "adaptive thinking · effort high (recommended)"),
    (
        "xhigh",
        "adaptive thinking · effort xhigh (recommended for coding/agentic work)",
    ),
    ("max", "adaptive thinking · effort max (deepest reasoning)"),
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

/// How long a requested interrupt may go unhonoured before Ctrl+C force-quits instead.
/// A live turn acknowledges cancellation well inside this; a turn whose task died never
/// will, and `busy` gates every other way out.
pub const INTERRUPT_GRACE: std::time::Duration = std::time::Duration::from_secs(3);
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
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp").then(|| s.to_string())
}

/// Path of a whole `![alt](path)` line (no spaces in path; unwraps `<path>`).
fn markdown_image_path(s: &str) -> Option<String> {
    let rest = s.strip_prefix("![")?;
    let close = rest.find("](")?;
    let rest = &rest[close + 2..];
    let end = rest.find(')')?;
    let p = &rest[..end];
    let p = p
        .strip_prefix('<')
        .and_then(|p| p.strip_suffix('>'))
        .unwrap_or(p);
    (!p.is_empty() && !p.contains(' ')).then(|| p.to_string())
}

/// Load timeout for a single image (a timeout counts as a load failure).
pub const IMAGE_LOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Lifetime of slash transient hints: they disappear from above the input after the timeout (never flushed).
pub const SLASH_OUTPUT_TTL: std::time::Duration = std::time::Duration::from_secs(2);
/// Error/usage slash rows live at least this long (G12 floor; they also clear on the
/// next input, so the user keeps a chance to act).
pub const SLASH_OUTPUT_ERROR_TTL: std::time::Duration = std::time::Duration::from_secs(8);

/// User message text entering the message flow when AskUserQuestion is declined
/// (Esc / empty Other submit) — an ordinary message, persistent with the flow.
pub const ASK_DECLINED_TEXT: &str = "User declined to answer questions";

/// Read/Search-style tool classification.
pub fn classify_tool(name: &str, input: &serde_json::Value) -> Option<CollapseKind> {
    match name {
        "Read" => input
            .get("file_path")
            .and_then(|p| p.as_str())
            .map(|p| CollapseKind::Read(Some(p.to_string()))),
        "Grep" | "Glob" => Some(CollapseKind::Search),
        // Managing subagents runs in streaks (check three, stop one), and every row used to be
        // its own two-line block that also closed whatever group was open. Fold the whole
        // streak, but count a stop apart from a look so the summary never reports a
        // deletion as a glance. An action-less call stays standalone (it is a malformed call).
        "AgentControl" => match input.get("action").and_then(|a| a.as_str()) {
            Some("stop") => Some(CollapseKind::AgentStop),
            Some("delete") => Some(CollapseKind::AgentDelete),
            Some(_) => Some(CollapseKind::AgentCheck),
            None => None,
        },
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
        "cat", "head", "tail", "less", "more", "wc", "stat", "file", "strings", "jq", "awk", "cut",
        "sort", "uniq", "tr",
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

/// Pure form of [`Chat::auth_error_hint`] (testable without a Session).
fn auth_hint_for(oauth: bool, provider: &str, code: &str, msg: String) -> String {
    match code {
        "AUTH_REQUIRED" if oauth && !msg.contains("/provider login") => {
            format!("{msg} (login expired? /provider login {provider} to sign in again)")
        }
        "PERMISSION_DENIED" if !msg.contains("/model") => {
            format!(
                "{msg} (the current subscription/permissions cannot use this model; switch with /model or check the apiKey)"
            )
        }
        _ => msg,
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
        g.read_paths
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
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
                if g.list == 1 {
                    "directory"
                } else {
                    "directories"
                }
            ),
        );
    }
    if g.agent_checks > 0 {
        push(
            "checked",
            "checking",
            format!(" {} {}", g.agent_checks, subagents(g.agent_checks)),
        );
    }
    // A stop and a delete are counted (and worded) apart from a look: folding them into
    // "checked 4 subagents" would report a run being killed as a glance.
    if g.agent_stops > 0 {
        push(
            "stopped",
            "stopping",
            format!(" {} {}", g.agent_stops, subagents(g.agent_stops)),
        );
    }
    if g.agent_deletes > 0 {
        push(
            "deleted",
            "deleting",
            format!(" {} {}", g.agent_deletes, subagents(g.agent_deletes)),
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
    if active { format!("{text}…") } else { text }
}

fn subagents(n: usize) -> &'static str {
    if n == 1 { "subagent" } else { "subagents" }
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
    if out
        .last()
        .is_some_and(|l| l.starts_with("[Exited with code"))
    {
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
    pub queued: Vec<QueuedInput>,
    /// Whether the `?` shortcut panel is expanded.
    pub help_visible: bool,
    /// Bottom transient notice (`Press ctrl-c again to exit` etc.).
    pub notice: Option<&'static str>,
    /// When the notice stops being true (the paired confirm window / a short
    /// TTL): the tick loop clears it — a stale "press again" line that outlived
    /// its window promised a behavior the next key no longer had.
    pub notice_until: Option<std::time::Instant>,
    /// Time of the most recent Ctrl+C on empty input (a second press within [`CTRL_C_WINDOW`] exits).
    ctrl_c_at: Option<std::time::Instant>,
    /// Time the running turn was first asked to stop; cleared when the next turn starts.
    /// Ctrl+C force-quits once it is older than [`INTERRUPT_GRACE`] and the turn is still busy.
    interrupt_at: Option<std::time::Instant>,
    /// Time of the most recent Esc (a second press within [`ESC_WINDOW`] clears the input).
    esc_at: Option<std::time::Instant>,
    /// Time of the last key press and the count of consecutive "fast" keys (paste-burst heuristic).
    last_key_at: Option<std::time::Instant>,
    burst_keys: usize,
    /// Collapsed paste blocks: placeholder `[Pasted text #N +M lines]` → real content.
    pastes: Vec<(String, String)>,
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
    /// Message opened by [`Chat::open_continuation_message`] to carry what the model says after a
    /// mid-turn answer. Recorded so a turn that ends without using it can drop it again —
    /// inferring that from "empty assistant message" would also catch messages nobody opened here.
    continuation_msg: Option<usize>,
    thinking_buf: String,
    /// Whether the current thinking segment is open for continuation: closed after ToolStart/TextDelta
    /// (segment boundaries); deltas in the same segment continue without paragraph breaks; new segments (fresh reasoning after a tool) are aggregated with \n\n.
    thinking_seg_open: bool,
    output_tokens: u64,
    output_round_tokens: u64,
    pub(super) token_rate: crate::token_rate::TokenRateSampler,
    pub(super) context_usage: crate::context_usage::ContextUsage,
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
    /// Task-list disk snapshot cache (refreshed each tick).
    tasks_cache: Vec<TodoItem>,
    processor: MarkdownProcessor,
    renderer: MarkdownRenderer,
    reply_cache: HashMap<String, Vec<Line>>,
    /// Terminal image capability (kitty protocol; probed for both hosts).
    pub image_cap: Option<ImageCap>,
    /// Portraits the transcript has put on screen. The transmit layer sends each
    /// one once and, after a store purge, sends exactly these again — recorded by
    /// the rows that drew them rather than rediscovered by scanning the document,
    /// which would put an O(messages × activities) sweep on the frame path.
    /// It only grows: a face whose message has already settled into scrollback
    /// still has cells out there referring to it.
    pub faces: HashSet<usize>,
    /// Portraits the blueprint pins to crew members, read once at startup — the
    /// answer is a committed file and the crew does not change while you look at
    /// it, so re-reading it per frame would be waste (the workspace learned the
    /// same thing in D49).
    faces_pinned: HashMap<String, usize>,
    /// Faces in the transcript at all (`experimental.chatAvatars`, off by default).
    /// Off means no sender band and no portrait on a watch row — the transcript the
    /// hub wrote before D50. The workspace views keep their portraits either way:
    /// there the face sits in a gutter the layout already spends, here it costs
    /// rows of its own, which is what the switch is for.
    chat_avatars: bool,
    /// Loaded image cache (url → PNG bytes + cell dimensions).
    pub images: HashMap<String, Arc<ImageMeta>>,
    /// Image urls currently being fetched (prevents duplicate loads).
    images_pending: HashSet<String>,
    /// Image urls whose load failed (rendered with a failure marker; a retry
    /// on a later message clears the mark).
    images_failed: HashSet<String>,
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
    /// Update banner (welcome card): the latest detected version (`vX.Y.Z`; None = no banner row).
    /// Data source: `crate::update::latest_cached` (24h TTL cache, warmed at startup).
    pub update_banner: Option<String>,
    /// Breathing animation start tick (window = [`UPDATE_BANNER_FRAMES`] frames; current frame = tick − start).
    update_banner_start: u64,
    /// Animation stopped (triggered by the first keypress in the window; the banner stays, it just stops breathing).
    update_banner_stopped: bool,
    /// motion off (settings `motion:"off"` or `BINGO_NO_MOTION=1`): breathing rests at the rest color
    /// and the banner stays (update-banner spec §2.5 "the indicator does not disappear, it just stops").
    motion_off: bool,
    /// Slash command output lines (/help /status etc.): rendered after messages, settled when idle.
    pub slash_lines: Vec<String>,
    /// When the slash output appeared (auto-dismissed by tick timeout).
    pub slash_at: Option<std::time::Instant>,
    /// Error/usage slash rows (G12/G13): longer TTL, clear on the next input, error color.
    pub slash_error_lines: Vec<String>,
    /// Informational output the user asked for (/help /status lists): stays
    /// until the next input or Esc — the old 2s TTL burned 18 lines of /help
    /// before anyone could read them.
    pub slash_info_lines: Vec<String>,
    /// Pinned panels (id → lines): persistent until their flow unpins them.
    /// OAuth device codes (valid 15 minutes) used to display for 2 seconds.
    pub pinned_panels: Vec<(String, Vec<String>)>,
    /// When the last error batch was pushed (longer TTL expiry base).
    pub slash_error_at: Option<std::time::Instant>,
    /// `/zzz` no-match flag (G9): the dropdown is empty but the input is a bare
    /// `/`-query — the suggestion area shows one dim hint row instead of a gap.
    pub slash_no_match: bool,
    /// /exit requested quitting (component layer consumes → system.exit).
    pub exit: bool,
    /// inline: segments of the document prefix already flushed to scrollback — 0 = none, 1 = welcome card,
    /// 1+k = welcome card + first k messages. The flush cursor counts by **message boundary**, not row number,
    /// so re-layout after a width change (all row numbers change) never reprints.
    pub flushed_segments: usize,
    /// inline: the number of already-flushed rows in the current doc (the canvas tail start); reset
    /// to zero on each build_rows — after a rebuild the flushed part is no longer in the document.
    pub tail_start: usize,
    /// Baseline that absorbs checkpoint accumulators: prevents double-counting when the
    /// flush cursor advances multiple times within one build (reset by build_rows).
    mark_base: usize,
    /// slash dropdown suggestions (non-empty when the input is `/` without arguments; rendered by the component layer).
    pub slash_suggestions: Vec<SlashSuggestion>,
    /// Selected index in the dropdown.
    pub slash_selected: usize,
    /// `/model` two-level selector (level-one endpoint → level-two model list; None = inactive).
    pub model_menu: Option<ModelMenu>,
    /// Last-used model per provider (session memory): switching back to a
    /// provider restores what you used there.
    provider_models: std::collections::HashMap<String, String>,
    /// Current provider was chosen session-only (`s` in the picker): model
    /// changes stay session-only too instead of half-persisting a pair.
    provider_session_only: bool,
    /// `/think` level selector (None = inactive).
    pub think_menu: Option<ThinkMenu>,
    /// `/theme` level selector (None = inactive).
    pub theme_menu: Option<ThemeMenu>,
    /// `/resume` session selector (None = inactive).
    pub resume_menu: Option<ResumeMenu>,
    /// `/provider` selector (None = inactive).
    pub provider_menu: Option<ProviderMenu>,
    /// The currently active theme setting (the ● marker's data source for /theme; updated by apply_theme).
    pub theme_setting: ThemeSetting,
    /// Menu-level model-list cache (provider → latest `/v1/models` result):
    /// validates `/model <name>` direct sets against the known list; avoids
    /// re-fetching when re-entering level two (P2-G cache, per-session).
    pub models_cache: std::collections::HashMap<String, Vec<String>>,
    /// Task-area expand signal (a Task tool call → shows the task list).
    pub tasks_visible: bool,
    /// Whether the task area was auto-opened by TaskCreate (not manually via ctrl+t): hides automatically when everything is done.
    pub tasks_auto: bool,
    /// Snapshot of the bottom entity area (agent instances + channels; refreshed on tick/WatchEvent).
    pub entities: Vec<EntityRow>,
    /// Entity selector focus (Some(i) = selection mode: ↑↓/Enter/Esc are captured).
    pub entity_focus: Option<usize>,
    /// Entity view pending open (app layer consumes → enters the fullscreen modal).
    pub open_entity: Option<EntityOpen>,
    /// Slack workspace view state. Lives here rather than in the modal so read
    /// cursors, the open conversation and collapsed sections survive leaving
    /// and re-entering the view.
    pub slack: crate::tui::slack::Workspace,
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
        self.warnings
            .retain(|(t, _)| t.elapsed() < Self::WARNING_TTL);
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
    #[allow(clippy::too_many_arguments)] // state-machine constructor: explicit args read better (same convention as tool/agent.rs)
    pub fn new(
        session: Arc<Session>,
        events: mpsc::UnboundedSender<UiEvent>,
        events_rx: mpsc::UnboundedReceiver<UiEvent>,
        asks: mpsc::UnboundedSender<AskRequest>,
        asks_rx: mpsc::UnboundedReceiver<AskRequest>,
        theme: Theme,
        theme_setting: ThemeSetting,
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
                            status: ev.state,
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
        let cwd = session.cwd().display().to_string();
        let history = crate::tui::history::History::new(crate::tui::history::load(
            &session.home,
            std::path::Path::new(&cwd),
        ));
        // The blueprint's pinned faces, read once: a committed file cannot answer
        // differently between frames, and the crew does not change while you look.
        let faces_pinned: HashMap<String, usize> =
            crate::team::load_team_tree(std::path::Path::new(&cwd))
                .ok()
                .flatten()
                .iter()
                .flat_map(|t| t.members())
                .filter_map(|(_, m)| {
                    Some((m.name.clone(), avatar::index_of_id(m.avatar.as_deref()?)?))
                })
                .collect();
        let permission_mode = session.permission_mode;
        // Update-banner (welcome card) data source + motion off: computed before the session moves into Self.
        // Store the bare version (rendering adds the `v` prefix in `banner_segments`).
        let update_banner = crate::update::latest_cached(&session.home).map(|v| v.to_string());
        let motion_off = session.settings.motion.as_deref() == Some("off")
            || std::env::var_os("BINGO_NO_MOTION").is_some();
        let chat_avatars = session.settings.experimental.chat_avatars;
        let context_window =
            crate::budget::context_window_for(&session.runtime.model.borrow().clone());
        let context_tokens = session
            .runtime
            .transcript
            .borrow()
            .clone()
            .and_then(|transcript| transcript.load_messages().ok())
            .map(|messages| crate::compact::estimate_tokens(&session.system, &messages))
            .unwrap_or(0);
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
            notice_until: None,
            ctrl_c_at: None,
            interrupt_at: None,
            esc_at: None,
            last_key_at: None,
            burst_keys: 0,
            pastes: Vec::new(),
            bash_history: Vec::new(),
            search: None,
            permission_mode,
            force_redraw: false,
            dump_transcript: false,
            bash_mode: false,
            busy: false,
            stream_msg: None,
            continuation_msg: None,
            thinking_buf: String::new(),
            thinking_seg_open: false,
            output_tokens: 0,
            output_round_tokens: 0,
            token_rate: crate::token_rate::TokenRateSampler::default(),
            context_usage: crate::context_usage::ContextUsage::new(context_tokens, context_window),
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
            faces: HashSet::new(),
            faces_pinned,
            chat_avatars,
            images: HashMap::new(),
            images_pending: HashSet::new(),
            images_failed: HashSet::new(),
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
            update_banner,
            update_banner_start: 0,
            update_banner_stopped: false,
            motion_off,
            slash_lines: Vec::new(),
            slash_at: None,
            slash_error_lines: Vec::new(),
            slash_info_lines: Vec::new(),
            pinned_panels: Vec::new(),
            slash_error_at: None,
            slash_no_match: false,
            exit: false,
            flushed_segments: 0,
            tail_start: 0,
            mark_base: 0,
            slash_suggestions: Vec::new(),
            slash_selected: 0,
            model_menu: None,
            provider_models: std::collections::HashMap::new(),
            provider_session_only: false,
            think_menu: None,
            theme_menu: None,
            resume_menu: None,
            provider_menu: None,
            theme_setting,
            models_cache: HashMap::new(),
            tasks_visible: false,
            tasks_auto: false,
            entities: Vec::new(),
            entity_focus: None,
            open_entity: None,
            slack: Default::default(),
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
            UiEvent::ModelsLoaded {
                provider,
                models,
                failed,
            } => {
                // Cache only successful fetches (/model <name> validation +
                // no re-fetch on re-entry) — a cached failure would poison
                // the advisory check and the re-entry fast path.
                if failed.is_none() && !models.is_empty() {
                    self.models_cache.insert(provider.clone(), models.clone());
                }
                if let Some(menu) = &mut self.model_menu
                    && let Some(m) = &mut menu.models
                    && m.provider == provider
                {
                    m.models = models;
                    m.loading = false;
                    m.failed = failed;
                    // P1-F: when the current provider and current model are in the list, preselect it —
                    // the counterpart of /think preselecting the current level; browsing must not switch.
                    let current_provider = self.session.runtime.provider.borrow().clone();
                    let current_model = self.session.runtime.model.borrow().clone();
                    let current = if m.provider == current_provider {
                        m.models.iter().position(|name| *name == current_model)
                    } else {
                        None
                    };
                    m.current = current;
                    m.selected = current.unwrap_or(0).min(m.models.len().saturating_sub(1));
                }
            }
            UiEvent::ImageReady { url, meta } => {
                self.images_pending.remove(&url);
                match meta {
                    Some(meta) => {
                        self.images_failed.remove(&url);
                        self.images.insert(url.clone(), Arc::new(meta));
                    }
                    None => {
                        self.images.remove(&url);
                        self.images_failed.insert(url.clone());
                        self.push_warning(format!("image load failed: {url}"));
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
                self.interrupt_at = None;
                let now = std::time::Instant::now();
                self.turn_started = Some(now);
                self.output_tokens = 0;
                self.output_round_tokens = 0;
                self.token_rate.start(now);
                self.messages.push(UiMessage {
                    role: Role::Assistant,
                    text: String::new(),
                    activities: Vec::new(),
                    insert_points: Vec::new(),
                    groups: Vec::new(),
                    group_of: Vec::new(),
                });
                self.stream_msg = Some(self.messages.len() - 1);
                self.continuation_msg = None;
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
                    self.close_running_thinking(i);
                }
            }
            UiEvent::ThinkingDelta(thinking) => {
                if let Some(i) = self.stream_msg {
                    let last_is_running_thinking =
                        self.messages[i].activities.last().is_some_and(|a| {
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
                                    a.content
                                        .first()
                                        .is_some_and(|l| l.plain_text() == thinking)
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
                                    t.duration_ms =
                                        self.tick.saturating_sub(t.start_tick).saturating_mul(33);
                                }
                                hint.set_content(content);
                            }
                            return;
                        }
                        self.thinking_buf = thinking.clone();
                        self.messages[i].activities.retain(|a| {
                            !(matches!(a.kind, ActivityKind::Thinking(_)) && a.content.is_empty())
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
            UiEvent::ContextUsage { used, window } => {
                self.context_usage = crate::context_usage::ContextUsage::new(used, window);
            }
            UiEvent::OutputTokens(tokens) => {
                self.output_tokens = self
                    .output_tokens
                    .saturating_sub(self.output_round_tokens)
                    .saturating_add(tokens);
                self.output_round_tokens = tokens;
                self.token_rate
                    .observe_round(tokens, std::time::Instant::now());
            }
            UiEvent::ToolStart { name } => {
                if is_hidden_tool(&name) {
                    return;
                }
                if let Some(i) = self.stream_msg {
                    self.close_running_thinking(i);
                }
                // Tool start = reasoning segment boundary: subsequent deltas aggregate into a new segment.
                self.thinking_seg_open = false;
                let name: &'static str = Box::leak(name.into_boxed_str());
                let mut hint = Activity::new(ActivityKind::Tool(ToolCall::running(name, "")));
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
                        active: true,
                        ..CollapseGroup::default()
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
                    CollapseKind::AgentCheck => self.messages[i].groups[g].agent_checks += 1,
                    CollapseKind::AgentStop => self.messages[i].groups[g].agent_stops += 1,
                    CollapseKind::AgentDelete => self.messages[i].groups[g].agent_deletes += 1,
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
                    m.activities
                        .iter_mut()
                        .find(|a| matches!(&a.kind, ActivityKind::Watch(w) if w.label == *label))
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
                    WatchState::Done | WatchState::Failed | WatchState::Cancelled
                );
                if terminal || signal.is_some() {
                    if let Some(sig) = &signal
                        && let Some(hint) = self.messages.iter_mut().find_map(|m| {
                            m.activities.iter_mut().find(
                                |a| matches!(&a.kind, ActivityKind::Watch(w) if w.label == *label),
                            )
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
                self.output_round_tokens = 0;
                self.token_rate.finish_round();
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
                        let in_group = group_of.get(hint_idx).copied().flatten().is_some();
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
                            let lines: Vec<String> =
                                done.output.lines().map(str::to_string).collect();
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
                self.output_round_tokens = 0;
                self.token_rate.stop();
                self.thinking_seg_open = false;
                self.drop_empty_stream_message();
                // AskUserQuestion answers are ordinary user messages (in the message flow,
                // settled/flushed with it) — nothing to clean at turn end, they persist with the session.
                // After a user interruption, background-task completion must not auto-start a new turn;
                // with queued messages, the user's message goes first (submitted together below).
                if (self.session.watch.has_wake_notifications(None)
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
                        self.messages[i].activities = keep
                            .iter()
                            .map(|&k| self.messages[i].activities[k].clone())
                            .collect();
                        self.messages[i].insert_points = keep
                            .iter()
                            .map(|&k| self.messages[i].insert_points[k])
                            .collect();
                        self.messages[i].group_of =
                            keep.iter().map(|&k| self.messages[i].group_of[k]).collect();
                    }
                    for hint in &mut self.messages[i].activities {
                        if let ActivityKind::Thinking(t) = &mut hint.kind
                            && t.state == ThinkingState::Running
                        {
                            t.state = ThinkingState::Done;
                            t.duration_ms =
                                self.tick.saturating_sub(t.start_tick).saturating_mul(33);
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
            UiEvent::SlashError(message) => {
                self.push_slash_error(message);
            }
            UiEvent::SlashInfo(message) => {
                self.push_slash_info(message);
            }
            UiEvent::PinPanel { id, lines } => {
                self.pin_panel(&id, lines);
            }
            UiEvent::Unpin { id } => {
                self.unpin_panel(&id);
            }
            UiEvent::Error {
                code,
                msg,
                level,
                context,
            } => {
                // Only a turn-level failure ends the running turn. Short sync
                // ops (model list fetch, token counts) can fail while a turn is
                // still streaming — resetting busy for them stopped the spinner
                // and re-armed the input while the turn kept running (violated
                // the v1.21 instant-command contract).
                if matches!(context, crate::error::ErrorContext::LongTurn) {
                    self.busy = false;
                    self.drop_empty_stream_message();
                    self.stream_msg = None;
                }
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
            self.images_failed.remove(&url);
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
            // Instant commands bypass the queue (CC semantics: settings knobs apply
            // before the next turn; read-only status commands run mid-turn). This is a
            // side-channel dispatch — it must not reset `busy`. run_slash's contract is
            // the line WITHOUT the leading slash, so strip it here.
            if let Some(rest) = text.strip_prefix('/') {
                let name = rest.split_whitespace().next().unwrap_or("");
                if INSTANT_SLASH_COMMANDS.contains(&name) {
                    self.run_slash(rest);
                    self.update_slash_suggestions();
                    return;
                }
            }
            let is_slash = text.starts_with('/');
            self.queued.push(QueuedInput { text, is_slash });
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
                self.clear_slash_suggestions();
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
            crate::tui::input::insert(
                &mut self.input,
                &mut self.cursor,
                &crate::api::image::marker(id),
            );
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
                    out.push(crate::api::image::marker(id));
                    continue;
                }
            }
            out.push(line.to_string());
        }
        out.join("\n")
    }

    /// Resolves `#[image N]` references in text → attachments (deduped, in order); unknown ids are ignored.
    fn resolve_images(&self, text: &str) -> Vec<crate::api::types::ImageAttachment> {
        self.session.attachments.resolve(text)
    }

    /// Raw image bytes → compress (within the API limit) → register the attachment → placeholder id.
    fn register_image(&mut self, bytes: &[u8]) -> Option<usize> {
        self.session.attachments.register(bytes)
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

    /// Queues slash error/usage rows (G12): they live longer than success hints
    /// ([`SLASH_OUTPUT_ERROR_TTL`] floor) and clear on the next input — the user needs
    /// time to read "what happened + what you can do" (feedback-states §3).
    fn push_slash_error(&mut self, text: String) {
        for line in text.lines() {
            self.slash_error_lines.push(line.to_string());
        }
        self.slash_error_at = Some(std::time::Instant::now());
        self.dirty = true;
    }

    /// Informational output tier: persists until the next input or Esc (no
    /// TTL) — for content the user explicitly asked to read.
    fn push_slash_info(&mut self, text: String) {
        for line in text.lines() {
            self.slash_info_lines.push(line.to_string());
        }
        self.dirty = true;
    }

    /// Startup note (invalid provider fallback etc.): info tier — persists
    /// until the first input, unlike stderr which the alt screen wipes.
    pub fn push_startup_note(&mut self, note: String) {
        self.push_slash_info(note);
    }

    /// Pin (or replace) a persistent panel: shown above the prompt until the
    /// owning flow unpins it. For anything that must outlive a TTL — device
    /// codes, long-operation progress.
    pub fn pin_panel(&mut self, id: &str, lines: Vec<String>) {
        if let Some(entry) = self.pinned_panels.iter_mut().find(|(pid, _)| pid == id) {
            entry.1 = lines;
        } else {
            self.pinned_panels.push((id.to_string(), lines));
        }
        self.dirty = true;
    }

    pub fn unpin_panel(&mut self, id: &str) {
        self.pinned_panels.retain(|(pid, _)| pid != id);
        self.dirty = true;
    }

    /// Whether any picker menu is open (dispatch and render read the same
    /// fact — they used to disagree on priority).
    fn menu_open(&self) -> bool {
        self.model_menu.is_some()
            || self.think_menu.is_some()
            || self.theme_menu.is_some()
            || self.resume_menu.is_some()
            || self.provider_menu.is_some()
    }

    /// The single mutual-exclusion point: every open_* goes through here —
    /// the old per-open hand-written clears formed an asymmetric triangle
    /// (newer menus closed older ones, never the reverse).
    fn close_menus(&mut self) {
        self.model_menu = None;
        self.think_menu = None;
        self.theme_menu = None;
        self.resume_menu = None;
        self.provider_menu = None;
        self.dirty = true;
    }

    /// Clears the slash dropdown and its no-match flag together (single lifecycle).
    fn clear_slash_suggestions(&mut self) {
        self.slash_suggestions.clear();
        self.slash_no_match = false;
    }

    /// Slash command dispatch. Returns true = consumed.
    fn run_slash(&mut self, line: &str) -> bool {
        // Any slash run closes the dropdown (Enter on a full input skips submit's clear-menu branch,
        // otherwise suggestion rows like `+ /model …` would linger below the input forever).
        self.clear_slash_suggestions();
        let (cmd, arg) = match line.split_once(char::is_whitespace) {
            Some((c, a)) => (c, a.trim()),
            None => (line, ""),
        };
        match cmd {
            "help" | "?" => self.slash_help(),
            "exit" | "quit" => self.exit = true,
            "clear" | "reset" | "new" => self.slash_clear(),
            "model" => self.slash_model(arg),
            "cd" => self.slash_cd(arg),
            "theme" => self.slash_theme(arg),
            "rename" => self.slash_rename(arg),
            "resume" => self.slash_resume(arg),
            "share" => self.slash_share(arg),
            "compact" => self.slash_compact(),
            "status" => self.slash_status(),
            "config" => self.slash_config(),
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
                self.push_slash_error(format!(
                    "[error] code={} msg=unknown command: /{other}. Type /help to see the available commands.",
                    crate::error::SLASH_ERROR_UNKNOWN_COMMAND
                ))
            }
        }
        true
    }

    fn slash_help(&mut self) {
        self.push_slash_info(crate::tui::slash::help_lines(SLASH_COMMANDS).join("\n"));
    }

    fn slash_cd(&mut self, arg: &str) {
        if self.busy {
            self.push_slash_error(format!(
                "[error] code={} msg=cannot switch working directory mid-turn (press Esc to interrupt, then retry)",
                crate::error::SLASH_ERROR_BAD_ARGUMENT
            ));
            return;
        }
        if arg.is_empty() {
            self.push_slash_error(format!(
                "[error] code={} msg=usage: /cd <dir>",
                crate::error::SLASH_ERROR_BAD_ARGUMENT
            ));
            return;
        }
        let requested = std::path::PathBuf::from(arg);
        let path = if requested.is_absolute() {
            requested
        } else {
            self.session.cwd().join(requested)
        };
        let path = match std::fs::canonicalize(&path) {
            Ok(path) if path.is_dir() => path,
            Ok(_) => {
                self.push_slash_error(format!(
                    "[error] code={} msg=not a directory: {arg}",
                    crate::error::SLASH_ERROR_BAD_ARGUMENT
                ));
                return;
            }
            Err(e) => {
                self.push_slash_error(format!(
                    "[error] code={} msg=cannot switch working directory to {arg}: {e}",
                    crate::error::SLASH_ERROR_BAD_ARGUMENT
                ));
                return;
            }
        };
        self.session.set_cwd(path.clone());
        self.cwd = path.display().to_string();
        self.history =
            crate::tui::history::History::new(crate::tui::history::load(&self.session.home, &path));
        self.faces_pinned = crate::team::load_team_tree(&path)
            .ok()
            .flatten()
            .iter()
            .flat_map(|tree| tree.members())
            .filter_map(|(_, member)| {
                Some((
                    member.name.clone(),
                    avatar::index_of_id(member.avatar.as_deref()?)?,
                ))
            })
            .collect();
        self.push_slash_output(format!("✓ working directory: {}", path.display()));
    }

    fn reset_context_usage(&mut self) {
        let model = self.session.runtime.model.borrow().clone();
        self.context_usage =
            crate::context_usage::ContextUsage::new(0, crate::budget::context_window_for(&model));
    }

    fn estimate_context_usage(&mut self, messages: &[crate::api::types::Message]) {
        let model = self.session.runtime.model.borrow().clone();
        self.context_usage = crate::context_usage::ContextUsage::new(
            crate::compact::estimate_tokens(&self.session.system, messages),
            crate::budget::context_window_for(&model),
        );
    }

    fn refresh_context_usage_from_transcript(&mut self) {
        let messages = self
            .session
            .runtime
            .transcript
            .borrow()
            .clone()
            .and_then(|transcript| transcript.load_messages().ok())
            .unwrap_or_default();
        self.estimate_context_usage(&messages);
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
        self.reset_context_usage();
        self.push_slash_output("✓ conversation cleared; starting a new session.".to_string());
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
        if self.busy {
            self.push_slash_error(
                "[error] code=BUSY msg=cannot switch models mid-turn (press Esc to interrupt, then retry)"
                    .to_string(),
            );
            return;
        }
        // P1-E: known-list check — when the current provider has a cache and the model is not in it, append a note
        // (advisory, non-blocking; the endpoint may have just shipped a new model or the cache may never have been pulled — typing it directly is still
        // a valid path). Merged into one line with the success note, to avoid the jarring "⚠ and ✓ together".
        let provider = self.session.runtime.provider.borrow().clone();
        let unknown = self
            .models_cache
            .get(&provider)
            .is_some_and(|known| !known.is_empty() && !known.contains(&model));
        let _ = self.session.runtime.model_tx.send(model.clone());
        self.refresh_context_usage_from_transcript();
        self.provider_models.insert(provider.clone(), model.clone());
        // Persistence follows the provider's scope: a session-only provider
        // (`s` in the picker) must not have its model half-persisted — that
        // wrote exactly the default-endpoint + foreign-model mismatch the
        // menu path guards against (audit A2).
        let scope = if self.provider_session_only {
            "(this session only — the provider is session-scoped)"
        } else {
            self.persist_selection(&model, &provider);
            ""
        };
        let out = if unknown {
            format!(
                "✓ model switched: {model}{scope} (⚠ not in {provider}'s known list; if the request fails, check with /model)"
            )
        } else {
            format!("✓ model switched: {model}{scope}")
        };
        self.push_slash_output(out);
    }

    /// Persist the provider+model pair. The two are one atomic selection:
    /// writing only one of them recreated the mismatch on restart (P0-A).
    fn persist_selection(&self, model: &str, provider: &str) {
        let cwd = std::path::PathBuf::from(&self.cwd);
        let _ = crate::settings::upsert_scoped_settings(
            &self.session.user_config_dir,
            &cwd,
            &serde_json::json!({ "model": model, "provider": provider }),
        );
    }

    /// Provider order (one source shared by /provider and /model's level one): default →
    /// built-in preset → user-defined. The two menus used to sort independently — "press 3" pointed at
    /// different endpoints in the two places (audit C3).
    fn provider_order(&self) -> Vec<String> {
        let mut names = vec!["default".to_string()];
        let mut user_names = Vec::new();
        for name in self.session.client.provider_names() {
            if self.session.client.is_preset(&name) {
                names.push(name);
            } else {
                user_names.push(name);
            }
        }
        names.extend(user_names);
        names
    }

    /// Enters the `/model` two-level selector: level one = current endpoint + configured providers
    /// (with the same endpoint/auth descriptions as /provider — it is the same list).
    fn open_model_menu(&mut self) {
        self.close_menus();
        let providers = self.provider_order();
        let provider_descs = providers.iter().map(|p| self.provider_desc(p)).collect();
        let current = self.session.runtime.provider.borrow().clone();
        let selected = providers.iter().position(|p| *p == current).unwrap_or(0);
        self.model_menu = Some(ModelMenu {
            providers,
            provider_descs,
            provider_selected: selected,
            provider_current: Some(selected),
            models: None,
        });
        self.clear_slash_suggestions();
    }

    /// Level-one Enter: asynchronously fetches the model list from that provider endpoint (forks the
    /// endpoint, without switching the current one); results arrive via the ModelsLoaded event. The
    /// level-one list (providers + provider_selected) is kept as-is: Esc back to level one doesn't lose it.
    fn open_model_models(
        &mut self,
        provider: String,
        providers: Vec<String>,
        provider_descs: Vec<String>,
        provider_selected: usize,
    ) {
        // P2-G cache: this session already fetched the list → reuse it
        // (the field's comment promised this; the fetch never did).
        if let Some(models) = self
            .models_cache
            .get(&provider)
            .filter(|m| !m.is_empty())
            .cloned()
        {
            let current_model = self.session.runtime.model.borrow().clone();
            let current_provider = self.session.runtime.provider.borrow().clone();
            let current = (provider == current_provider)
                .then(|| models.iter().position(|m| *m == current_model))
                .flatten();
            self.model_menu = Some(ModelMenu {
                providers,
                provider_descs,
                provider_selected,
                provider_current: None,
                models: Some(ModelMenuModels {
                    provider,
                    selected: current.unwrap_or(0).min(models.len().saturating_sub(1)),
                    models,
                    loading: false,
                    current,
                    failed: None,
                }),
            });
            return;
        }
        let session = self.session.clone();
        let events = self.events.clone();
        let provider_for_spawn = provider.clone();
        tokio::spawn(async move {
            // Unknown names must error — the old fallback silently listed the
            // CURRENT endpoint's models under the wrong provider label.
            let client = match session.client.with_provider(&provider_for_spawn) {
                Ok(c) => c,
                Err(e) => {
                    // Same visibility contract as a fetch failure: page-level
                    // error row + in-menu reason.
                    let _ = events.send(UiEvent::Error {
                        code: "GENERIC",
                        msg: e.clone(),
                        level: crate::error::ErrorLevel::Page,
                        context: crate::error::ErrorContext::ShortSync,
                    });
                    let _ = events.send(UiEvent::ModelsLoaded {
                        provider: provider_for_spawn,
                        models: Vec::new(),
                        failed: Some(e),
                    });
                    return;
                }
            };
            let (models, failed) = match client.list_models().await {
                Ok(m) => (m, None),
                Err(e) => {
                    let code = crate::error::map_error(&e);
                    // #18/main #91: short-op failures must be visible (page-level error row, error color),
                    // behavior keeps degrading gracefully — "degraded + visible".
                    let _ = events.send(UiEvent::Error {
                        code,
                        msg: e.to_string(),
                        level: crate::error::ErrorLevel::Page,
                        context: crate::error::ErrorContext::ShortSync,
                    });
                    // In-menu reason: a 401 is an auth problem, not "the
                    // endpoint returned no models".
                    let reason = if code == "AUTH_REQUIRED" {
                        format!(
                            "authentication failed: {} credentials invalid or not logged in (/provider login {})",
                            provider_for_spawn, provider_for_spawn
                        )
                    } else {
                        format!("fetch failed ({code})")
                    };
                    (Vec::new(), Some(reason))
                }
            };
            let _ = events.send(UiEvent::ModelsLoaded {
                provider: provider_for_spawn,
                models,
                failed,
            });
        });
        // The menu was taken out by the Enter branch — rebuild the level-two state here (level-one list kept).
        self.model_menu = Some(ModelMenu {
            providers,
            provider_descs,
            provider_selected,
            provider_current: None,
            models: Some(ModelMenuModels {
                provider,
                models: Vec::new(),
                loading: true,
                selected: 0,
                current: None,
                failed: None,
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
                    // Level two uses the same PickerModel core (windowed rendering follows selected).
                    let mut core = m.picker();
                    core.move_selection(1);
                    m.selected = core.selected;
                } else {
                    // Level one: delegates to the PickerModel core (picker-model.md commit E).
                    let mut core = menu.provider_picker();
                    core.move_selection(1);
                    menu.provider_selected = core.selected;
                }
                true
            }
            KeyCode::Up if !modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(m) = &mut menu.models {
                    let mut core = m.picker();
                    core.move_selection(-1);
                    m.selected = core.selected;
                } else {
                    let mut core = menu.provider_picker();
                    core.move_selection(-1);
                    menu.provider_selected = core.selected;
                }
                true
            }
            // Number jump: applies to both levels; out-of-range is swallowed (digits leaking into the input was once a half-modal boundary bug).
            KeyCode::Char(c)
                if c.is_ascii_digit() && !modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let n = c.to_digit(10).map(|n| n as usize).unwrap_or(0);
                if let Some(m) = &mut menu.models {
                    let mut core = m.picker();
                    if core.jump(n) {
                        m.selected = core.selected;
                    }
                } else {
                    let mut core = menu.provider_picker();
                    if core.jump(n) {
                        menu.provider_selected = core.selected;
                    }
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
                    self.open_model_models(
                        provider,
                        menu.providers,
                        menu.provider_descs,
                        menu.provider_selected,
                    );
                    return true;
                };
                // Level two: confirm the selected model. Keep the menu when the list is empty (fetch failed/none returned).
                let provider = m.provider.clone();
                let model = m.models.get(m.selected).cloned().unwrap_or_default();
                if model.is_empty() {
                    self.model_menu = Some(ModelMenu {
                        providers: menu.providers,
                        provider_descs: menu.provider_descs,
                        provider_selected: menu.provider_selected,
                        provider_current: menu.provider_current,
                        models: Some(m),
                    });
                    return true;
                }
                // provider+model is an atomic selection: confirming across endpoints goes through the same
                // switch_provider (login warnings, the busy guard, and paired persistence all live there),
                // the old bypass dropped every provider-side notice (audit A3).
                self.provider_models.insert(provider.clone(), model.clone());
                if provider != self.session.runtime.provider.borrow().clone() {
                    self.switch_provider(&provider, true);
                    if *self.session.runtime.provider.borrow() != provider {
                        // Switch refused (busy / unknown): keep the menu alive.
                        self.model_menu = Some(ModelMenu {
                            providers: menu.providers,
                            provider_descs: menu.provider_descs,
                            provider_selected: menu.provider_selected,
                            provider_current: menu.provider_current,
                            models: Some(m),
                        });
                    }
                } else {
                    self.set_model(model);
                }
                true
            }
            KeyCode::Esc => {
                // Level two → back to level one; level one → exit entirely (returns one level at a time).
                if self.model_menu.as_mut().is_some_and(|m| m.models.is_some()) {
                    self.model_menu.as_mut().expect("menu must exist").models = None;
                } else {
                    self.model_menu = None;
                }
                true
            }
            _ => false,
        }
    }

    /// `/theme [dark|light|auto]`: no argument opens the level selector (picker-model.md commit B);
    /// an argument takes the fast path (`/theme auto` keeps the explicit shortcut).
    fn slash_theme(&mut self, arg: &str) {
        if arg.is_empty() {
            self.open_theme_menu();
            return;
        }
        // A typo must not read as success: the old path silently parsed any
        // junk as auto and announced "✓ theme switched: auto".
        if !matches!(arg.trim(), "auto" | "dark" | "light") {
            self.push_slash_error(format!(
                "[error] code=BAD_ARGUMENT msg=unknown theme: {arg}. Choose from auto | dark | light"
            ));
            return;
        }
        self.apply_theme(ThemeSetting::parse(Some(arg)));
    }

    /// Apply the theme: rebuild the renderer/cache, persist, update theme_setting (the menu's ● data source).
    fn apply_theme(&mut self, setting: ThemeSetting) {
        let name = match setting {
            ThemeSetting::Dark => "dark",
            ThemeSetting::Light => "light",
            ThemeSetting::Auto => "auto",
        };
        self.theme_setting = setting;
        self.theme = Theme::for_terminal(setting, self.detected_background);
        // The renderer baked in theme styles and reply_cache holds old-theme rows — rebuild them in sync.
        self.renderer =
            crate::tui::markdown::MarkdownRenderer::with_theme(self.width, self.theme.clone());
        self.reply_cache.clear();
        self.dirty = true;
        let cwd = std::path::PathBuf::from(&self.cwd);
        let _ = crate::settings::upsert_scoped_settings(
            &self.session.user_config_dir,
            &cwd,
            &serde_json::json!({ "theme": name }),
        );
        self.push_slash_output(format!("✓ theme switched: {name}"));
    }

    /// Open the `/theme` selector: preselect the current level (theme_setting), close other menus exclusively.
    fn open_theme_menu(&mut self) {
        let current = match self.theme_setting {
            ThemeSetting::Dark => 0,
            ThemeSetting::Light => 1,
            ThemeSetting::Auto => 2,
        };
        let menu = ThemeMenu {
            selected: current,
            current,
        };
        // Empty-table guard (THEME_LEVELS is a non-empty const; this defensive branch is unreachable).
        if menu.picker().is_empty() {
            return;
        }
        self.close_menus();
        self.theme_menu = Some(menu);
        self.clear_slash_suggestions();
    }

    /// Theme menu keys: ↑↓/1-3 move (delegated to the PickerModel core),
    /// Enter applies + persists, Esc exits. Returns whether consumed.
    fn theme_menu_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let Some(menu) = &mut self.theme_menu else {
            return false;
        };
        match code {
            KeyCode::Down if !modifiers.contains(KeyModifiers::CONTROL) => {
                let mut core = menu.picker();
                core.move_selection(1);
                menu.selected = core.selected;
                true
            }
            KeyCode::Up if !modifiers.contains(KeyModifiers::CONTROL) => {
                let mut core = menu.picker();
                core.move_selection(-1);
                menu.selected = core.selected;
                true
            }
            // Direct jump: 1 = dark … 3 = auto.
            KeyCode::Char(c)
                if c.is_ascii_digit() && !modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let mut core = menu.picker();
                if let Some(n) = c.to_digit(10)
                    && core.jump(n as usize)
                {
                    menu.selected = core.selected;
                }
                // Swallow even out-of-range digits: a menu is a modal surface —
                // "4" on a 3-item picker used to type a literal 4 into the input.
                true
            }
            KeyCode::Enter => {
                let core = menu.picker();
                let value = core
                    .selected_item()
                    .map(|i| i.value.clone())
                    .unwrap_or_default();
                self.theme_menu = None;
                self.apply_theme(ThemeSetting::parse(Some(&value)));
                true
            }
            KeyCode::Esc => {
                self.theme_menu = None;
                true
            }
            _ => false,
        }
    }

    fn slash_rename(&mut self, arg: &str) {
        let Some(t) = self.session.runtime.transcript.borrow().clone() else {
            self.push_slash_error("this session has no transcript; cannot rename.".to_string());
            return;
        };
        match t.rename(arg) {
            Ok(new_t) => {
                let name = new_t.name();
                let _ = self.session.runtime.transcript_tx.send(Some(new_t));
                self.push_slash_output(format!("✓ session renamed: {name}"));
            }
            Err(e) => self.push_slash_error(format!("rename failed: {e}")),
        }
    }

    /// `/resume [name or keyword]`: no argument opens the session selector (picker-model.md commit C,
    /// the same picker as CC's /resume); an argument takes the fast path (name/keyword match, kept as-is).
    fn slash_resume(&mut self, arg: &str) {
        let home = self.session.home.clone();
        let transcripts = match crate::transcript::list(&home) {
            Ok(t) => t,
            Err(e) => {
                self.push_slash_error(format!("cannot read the session list: {e}"));
                return;
            }
        };
        if arg.is_empty() {
            if transcripts.is_empty() {
                self.push_slash_output("no past sessions.".to_string());
                return;
            }
            self.open_resume_menu(transcripts);
            return;
        }
        self.switch_transcript(transcripts.iter().find(|t| t.name().contains(arg)), arg);
    }

    /// Fast-path switch (argument /resume): a hit switches, a miss errors.
    fn switch_transcript(&mut self, found: Option<&crate::transcript::Transcript>, arg: &str) {
        let Some(found) = found else {
            self.push_slash_error(format!("no session contains '{arg}'."));
            return;
        };
        let count = found.load_messages().unwrap_or_default().len();
        let _ = self.session.runtime.transcript_tx.send(Some(found.clone()));
        self.messages.clear();
        self.slash_lines.clear();
        self.reset_flushed();
        self.refresh_context_usage_from_transcript();
        self.push_slash_output(format!(
            "✓ switched to session {} ({count} messages); the next reply uses its history.",
            found.name()
        ));
    }

    /// Open the `/resume` selector: truncate the disk snapshot to the latest RESUME_PICKER_MAX,
    /// ● marks the current session (when in the list), other menus close exclusively.
    fn open_resume_menu(&mut self, mut transcripts: Vec<crate::transcript::Transcript>) {
        let truncated = transcripts.len() > RESUME_PICKER_MAX;
        transcripts.truncate(RESUME_PICKER_MAX);
        let current = self.session.runtime.transcript.borrow().clone();
        let current = current
            .as_ref()
            .and_then(|cur| transcripts.iter().position(|t| t.path() == cur.path()));
        let menu = ResumeMenu {
            selected: current.unwrap_or(0),
            current,
            transcripts,
            truncated,
        };
        if menu.picker().is_empty() {
            return;
        }
        self.close_menus();
        self.resume_menu = Some(menu);
        self.clear_slash_suggestions();
    }

    /// Resume menu keys: ↑↓/1-N move (delegated to the PickerModel core),
    /// Enter switches the session (by selected index into the snapshot), Esc exits.
    fn resume_menu_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let Some(menu) = &mut self.resume_menu else {
            return false;
        };
        match code {
            KeyCode::Down if !modifiers.contains(KeyModifiers::CONTROL) => {
                let mut core = menu.picker();
                core.move_selection(1);
                menu.selected = core.selected;
                true
            }
            KeyCode::Up if !modifiers.contains(KeyModifiers::CONTROL) => {
                let mut core = menu.picker();
                core.move_selection(-1);
                menu.selected = core.selected;
                true
            }
            // Direct jump: 1..=min(len, 9) (past 9 items the number jump only covers the first 9).
            KeyCode::Char(c)
                if c.is_ascii_digit() && !modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let mut core = menu.picker();
                if let Some(n) = c.to_digit(10)
                    && core.jump(n as usize)
                {
                    menu.selected = core.selected;
                }
                // Swallow even out-of-range digits: a menu is a modal surface —
                // "4" on a 3-item picker used to type a literal 4 into the input.
                true
            }
            KeyCode::Enter => {
                // The confirm action takes the snapshot by the selected index (same order as items; the value≠label test anchor).
                let Some(t) = menu.transcripts.get(menu.selected).cloned() else {
                    return false;
                };
                let name = t.name();
                let count = t.load_messages().unwrap_or_default().len();
                self.resume_menu = None;
                let _ = self.session.runtime.transcript_tx.send(Some(t));
                self.messages.clear();
                self.slash_lines.clear();
                self.reset_flushed();
                self.refresh_context_usage_from_transcript();
                self.push_slash_output(format!(
                    "✓ switched to session {name} ({count} messages); the next reply uses its history."
                ));
                true
            }
            KeyCode::Esc => {
                self.resume_menu = None;
                true
            }
            _ => false,
        }
    }

    /// `/share` exports locally by default. Publishing a public link requires the
    /// explicit `--public` opt-in; the warning is presented before bytes leave the machine.
    fn slash_share(&mut self, arg: &str) {
        let public = parse_share_arg(arg, "--public");
        let open = parse_share_arg(arg, "--open");
        let Some(transcript) = self.session.runtime.transcript.borrow().clone() else {
            self.push_slash_output("no session to export yet (the new session has not been persisted; send a message first).".to_string());
            return;
        };
        let messages = match transcript.load_messages() {
            Ok(m) => m,
            Err(e) => {
                self.push_slash_error(format!("failed to read the session: {e}"));
                return;
            }
        };
        let stem = transcript.name();
        let share_path = crate::share::shares_dir(&self.session.home).join(format!("{stem}.json"));
        let doc = match crate::share::ShareStore::load_or_create(&share_path) {
            Ok(store) => store.snapshot(),
            Err(e) => {
                self.push_slash_error(format!(
                    "cannot read the share document ({e}); exporting the conversation view only."
                ));
                crate::share::ShareDoc::new(stem.clone())
            }
        };
        // Legacy-session fallback: without a share document, derive Team/DM/channel data from the main transcript.
        let doc = if doc.agents.is_empty() && doc.channels.is_empty() {
            crate::share::derive_share_doc(&stem, &messages)
        } else {
            doc
        };
        let html = crate::share_html::render(&doc, &messages);
        let out = std::path::PathBuf::from(&self.cwd).join(format!("{stem}.html"));

        // Local export is the safe default; `--open` only opens the generated file.
        if !public {
            let overwritten = out.exists();
            if let Err(e) = crate::share::write_html_atomic(&out, &html) {
                self.push_slash_error(format!("write failed: {e}"));
                return;
            }
            let mut lines = vec![format!(
                "✓ exported: {}{}",
                out.display(),
                if overwritten { " (overwritten)" } else { "" }
            )];
            if open {
                match crate::share::open_in_browser(&out.display().to_string()) {
                    Ok(_) => lines.push("opened in the browser.".to_string()),
                    Err(e) => lines.push(format!("cannot open the browser: {e}")),
                }
            }
            lines.push(
                "note: this file contains the full conversation and tool outputs (possibly sensitive); review it before sharing."
                    .to_string(),
            );
            self.push_slash_info(lines.join("\n"));
            return;
        }

        // Public publishing is asynchronous so the TUI event loop remains responsive.
        // The runtime settings snapshot is authoritative for the configured share service.
        let base = self
            .session
            .settings
            .share
            .base_url
            .clone()
            .unwrap_or_else(|| crate::share::DEFAULT_SHARE_BASE.to_string());
        let id = crate::share::share_id(&stem);
        let events = self.events.clone();
        self.pin_panel(
            "share",
            vec![
                "⚠ about to publish publicly: anyone can access the full conversation and tool outputs, which may contain sensitive information."
                    .to_string(),
                "⏳ publishing the share page…".to_string(),
            ],
        );
        tokio::spawn(async move {
            let unpin = || {
                let _ = events.send(UiEvent::Unpin {
                    id: "share".to_string(),
                });
            };
            match crate::share::upload_share(&base, &id, &html).await {
                Ok(url) => {
                    let mut lines = vec![format!("✓ published: {url}")];
                    if open {
                        match crate::share::open_in_browser(&url) {
                            Ok(_) => lines.push("opened in the browser.".to_string()),
                            Err(e) => lines.push(format!("cannot open the browser: {e}")),
                        }
                    }
                    unpin();
                    // The URL must survive long enough to copy — info tier.
                    let _ = events.send(UiEvent::SlashInfo(lines.join("\n")));
                }
                Err(e) => {
                    // Upload failure falls back to a local file + a notice (consistent with the bingo share subcommand).
                    let mut lines = vec![format!(
                        "upload failed ({e}); falling back to a local file."
                    )];
                    let overwritten = out.exists();
                    match crate::share::write_html_atomic(&out, &html) {
                        Ok(()) => lines.push(format!(
                            "✓ exported: {}{}",
                            out.display(),
                            if overwritten { " (overwritten)" } else { "" }
                        )),
                        Err(write_err) => lines.push(format!("write failed: {write_err}")),
                    }
                    if open && crate::share::open_in_browser(&out.display().to_string()).is_ok() {
                        lines.push("opened in the browser.".to_string());
                    }
                    lines.push(
                        "note: this file contains the full conversation and tool outputs (possibly sensitive); review it before sharing."
                            .to_string(),
                    );
                    unpin();
                    let _ = events.send(UiEvent::SlashError(lines.join("\n")));
                }
            }
        });
    }

    fn slash_compact(&mut self) {
        let session = self.session.clone();
        let events = self.events.clone();
        // Long operation (a full model call): pinned until the flow resolves —
        // a 2s hint left the rest of the wait silent.
        self.pin_panel("compact", vec!["⏳ compacting the context…".to_string()]);
        tokio::spawn(async move {
            let unpin = || {
                let _ = events.send(UiEvent::Unpin {
                    id: "compact".to_string(),
                });
            };
            let transcript = session.runtime.transcript.borrow().clone();
            let mut messages = match &transcript {
                Some(t) => t.load_messages().unwrap_or_default(),
                None => Vec::new(),
            };
            if messages.len() <= 8 {
                unpin();
                let _ = events.send(UiEvent::SlashOutput(
                    "the conversation is too short; no compaction needed.".to_string(),
                ));
                return;
            }
            let old_len = messages.len();
            let compacted = crate::compact::maybe_compact(&session, &mut messages, u64::MAX).await;
            if !compacted {
                unpin();
                let _ = events.send(UiEvent::SlashError(
                    "compaction failed (model call error).".to_string(),
                ));
                return;
            }
            let summary = messages
                .first()
                .map(|m| {
                    m.content
                        .iter()
                        .filter_map(|b| match b {
                            crate::api::types::ContentBlock::Text { text } => Some(text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            if let Some(t) = transcript {
                let _ = t.replace_messages(&messages);
            }
            let _ = events.send(UiEvent::ContextUsage {
                used: crate::compact::estimate_tokens(&session.system, &messages),
                window: crate::budget::context_window_for(&session.runtime.model.borrow().clone()),
            });
            unpin();
            let _ = events.send(UiEvent::SlashInfo(format!(
                "✓ compacted {old_len} messages → summary + the latest 8.\nSummary: {summary}"
            )));
        });
    }

    /// Async stats shared by /status and /context: message count + token count.
    fn slash_stats_async(&mut self, format: impl Fn(usize, u64) -> String + Send + 'static) {
        let session = self.session.clone();
        let events = self.events.clone();
        self.pin_panel("stats", vec!["⏳ gathering stats…".to_string()]);
        tokio::spawn(async move {
            let unpin = || {
                let _ = events.send(UiEvent::Unpin {
                    id: "stats".to_string(),
                });
            };
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
            unpin();
            let _ = events.send(UiEvent::SlashInfo(format(msgs.len(), tokens)));
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
            .unwrap_or_else(|| "none".to_string());
        let mode = session.permission_mode_str().to_string();
        self.slash_stats_async(move |msg_count, tokens| {
            // Window/percentage measured with the model actually in use — the
            // fixed 200k constant misread every non-Claude endpoint.
            let window = crate::budget::context_window_for(&model).max(1);
            format!(
                "Model: {model}\nProvider: {provider}\nThinking: {thinking_shown}\nPermission mode: {mode}\nSession: {transcript_name}\nMessages: {msg_count}\nContext: {tokens} tokens / {window} ({}%)",
                tokens * 100 / window
            )
        });
    }

    /// `/config`: the interpreter the five config sources never had — for
    /// every effective value, WHICH layer (or env var) won; plus endpoint,
    /// credentials location and unknown-key warnings.
    fn slash_config(&mut self) {
        let cwd = std::path::PathBuf::from(&self.cwd);
        let paths = crate::settings::layer_paths(&self.session.user_config_dir, &cwd);
        let layer_names = ["user", "project", "local"];
        let mut lines =
            vec!["config sources (user < project < local; later layers override):".to_string()];
        let mut layer_values: Vec<Option<serde_json::Value>> = Vec::new();
        for (path, name) in paths.iter().zip(layer_names) {
            let value = std::fs::read_to_string(path)
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok());
            let state = if value.is_some() {
                "✓"
            } else if path.exists() {
                "✗ parse failed"
            } else {
                "(does not exist)"
            };
            lines.push(format!("  {name:8} {} {state}", path.display()));
            layer_values.push(value);
        }
        let lookup = |key: &str| -> Option<(String, &'static str)> {
            for (i, value) in layer_values.iter().enumerate().rev() {
                if let Some(v) = value.as_ref().and_then(|v| v.get(key)) {
                    let shown = match v {
                        serde_json::Value::String(s) if key == "apiKey" => {
                            let mut masked: String = s.chars().take(4).collect();
                            masked.push('…');
                            masked
                        }
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    return Some((shown, layer_names[i]));
                }
            }
            None
        };
        lines.push("effective values and their sources:".to_string());
        for key in [
            "provider",
            "model",
            "thinkingLevel",
            "theme",
            "permissionMode",
            "apiKey",
            "apiBaseUrl",
            "shell",
            "motion",
        ] {
            let entry = match lookup(key) {
                Some((value, source)) => format!("  {key:18} = {value} ({source} layer)"),
                None => match key {
                    "apiKey" if std::env::var("ANTHROPIC_API_KEY").is_ok() => {
                        format!("  {key:18} = (env ANTHROPIC_API_KEY)")
                    }
                    "apiKey" if std::env::var("DEEPSEEK_API_KEY").is_ok() => {
                        format!("  {key:18} = (env DEEPSEEK_API_KEY)")
                    }
                    "apiBaseUrl" if std::env::var("ANTHROPIC_BASE_URL").is_ok() => {
                        format!("  {key:18} = (env ANTHROPIC_BASE_URL)")
                    }
                    _ => format!("  {key:18} = (default)"),
                },
            };
            lines.push(entry);
        }
        // Runtime identity: what this session is actually talking to.
        let provider = self.session.runtime.provider.borrow().clone();
        let model = self.session.runtime.model.borrow().clone();
        let (_, url) = self.session.client.current_endpoint();
        lines.push(format!(
            "current session: {provider} · {model} · {url}{}",
            if self.provider_session_only {
                " (provider is session-scoped)"
            } else {
                ""
            }
        ));
        lines.push(format!(
            "credential store: {} (/provider shows each endpoint's login state)",
            crate::auth::AuthStore::new(&self.session.home)
                .path()
                .display()
        ));
        // Unknown top-level keys: typos parse fine and silently do nothing.
        for (i, value) in layer_values.iter().enumerate() {
            if let Some(obj) = value.as_ref().and_then(|v| v.as_object()) {
                for key in obj.keys() {
                    if !crate::settings::KNOWN_KEYS.contains(&key.as_str()) {
                        lines.push(format!(
                            "⚠ unknown config key \"{key}\" in the {} layer (a typo? it will have no effect)",
                            layer_names[i]
                        ));
                    }
                }
            }
        }
        self.push_slash_info(lines.join("\n"));
    }

    fn slash_context(&mut self) {
        let model = self.session.runtime.model.borrow().clone();
        self.slash_stats_async(move |_msg_count, tokens| {
            let window = crate::budget::context_window_for(&model).max(1);
            let pct = tokens * 100 / window;
            let bar_len = 40usize;
            let filled = ((pct as usize * bar_len) / 100).min(bar_len);
            let bar = format!("{}·{}", "#".repeat(filled), "·".repeat(bar_len - filled));
            format!(
                "context: [{bar}] {pct}%\n{tokens} / {window} tokens used\nauto-compaction threshold: {}%",
                crate::budget::autocompact_threshold_for(&model) * 100 / window
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
            let mut lines = vec!["permission rules (.bingo/settings.json):".to_string()];
            for (name, list) in [
                ("allow", &rules.allow),
                ("deny", &rules.deny),
                ("ask", &rules.ask),
            ] {
                if list.is_empty() {
                    lines.push(format!("  {name}: (none)"));
                } else {
                    lines.push(format!("  {name}:"));
                    for rule in list {
                        lines.push(format!("    {rule}"));
                    }
                }
            }
            lines.push("usage: /permissions [allow|deny|ask] [rule, e.g. Skill(review:*)]".into());
            self.push_slash_info(lines.join("\n"));
            return;
        }
        let Some((kind, rule)) = arg.split_once(char::is_whitespace) else {
            self.push_slash_error("usage: /permissions [allow|deny|ask] [rule]".to_string());
            return;
        };
        if !["allow", "deny", "ask"].contains(&kind) || rule.is_empty() {
            self.push_slash_error("usage: /permissions [allow|deny|ask] [rule]".to_string());
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
                "✓ added {kind} rule: {rule} (active now + written to .bingo/settings.json)"
            )),
            Err(e) => self.push_slash_output(format!(
                "✓ added {kind} rule: {rule} (active now); persistence failed: {e}"
            )),
        }
    }

    fn slash_mcp(&mut self, arg: &str) {
        use crate::mcp::McpStatus;
        let session = self.session.clone();
        let cwd = std::path::PathBuf::from(&self.cwd);
        let user_config_dir = self.session.user_config_dir.clone();
        let events = self.events.clone();
        let parts: Vec<&str> = arg.split_whitespace().collect();
        match parts.first().copied() {
            None => {
                self.pin_panel("mcp", vec!["⏳ checking MCP servers…".to_string()]);
                tokio::spawn(async move {
                    let unpin = || {
                        let _ = events.send(UiEvent::Unpin {
                            id: "mcp".to_string(),
                        });
                    };
                    let mgr = session.runtime.mcp.lock().await;
                    let names = mgr.configured();
                    if names.is_empty() {
                        unpin();
                        let _ = events.send(UiEvent::SlashInfo(
                            "no MCP servers configured.\nAdd them under mcpServers in .bingo/settings.json or \
                             ~/.config/bingo/settings.json."
                                .to_string(),
                        ));
                        return;
                    }
                    let mut lines = vec![format!("MCP servers ({}):", names.len())];
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
                    lines.push(
                        "usage: /mcp enable|disable [name|all] · /mcp reconnect <name>".into(),
                    );
                    unpin();
                    let _ = events.send(UiEvent::SlashInfo(lines.join("\n")));
                });
            }
            Some(action @ ("enable" | "disable")) => {
                let target = parts.get(1).copied().unwrap_or("all").to_string();
                let enabled = action == "enable";
                self.push_slash_output(format!(
                    "⏳ {}{target}…",
                    if enabled { "enabling " } else { "disabling " }
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
                        let _ = events
                            .send(UiEvent::SlashError(format!("no MCP server \"{target}\".")));
                        return;
                    }
                    for name in &targets {
                        mgr.set_enabled(name, enabled);
                    }
                    if enabled {
                        // Union-merged key: the name must leave EVERY layer
                        // that lists it — writing only the project layer let
                        // a user-layer entry merge it right back next start.
                        for name in &targets {
                            let _ = crate::settings::remove_from_union_lists(
                                &user_config_dir,
                                &cwd,
                                "disabledMcpServers",
                                name,
                            );
                        }
                    } else {
                        let list = mgr.disabled();
                        let _ = crate::settings::upsert_project_settings(
                            &cwd,
                            &serde_json::json!({ "disabledMcpServers": list }),
                        );
                    }
                    let verb = if enabled { "enabled" } else { "disabled" };
                    let _ = events.send(UiEvent::SlashOutput(format!(
                        "{verb} {} MCP server(s): {}",
                        targets.len(),
                        targets.join(", ")
                    )));
                });
            }
            Some("reconnect") => {
                let Some(name) = parts.get(1).copied() else {
                    self.push_slash_error("usage: /mcp reconnect <server name>".to_string());
                    return;
                };
                let name = name.to_string();
                self.pin_panel("mcp", vec![format!("⏳ reconnecting {name}…")]);
                tokio::spawn(async move {
                    let unpin = || {
                        let _ = events.send(UiEvent::Unpin {
                            id: "mcp".to_string(),
                        });
                    };
                    let mut mgr = session.runtime.mcp.lock().await;
                    if !mgr.configured().contains(&name) {
                        unpin();
                        let _ =
                            events.send(UiEvent::SlashError(format!("no MCP server \"{name}\".")));
                        return;
                    }
                    if mgr.is_disabled(&name) {
                        unpin();
                        let _ = events.send(UiEvent::SlashError(format!(
                            "{name} is disabled; run /mcp enable {name} before reconnecting."
                        )));
                        return;
                    }
                    match mgr.reconnect(&name).await {
                        Ok(()) => {
                            let count = match mgr.status(&name) {
                                McpStatus::Connected { tool_count } => tool_count,
                                _ => 0,
                            };
                            unpin();
                            let _ = events.send(UiEvent::SlashOutput(format!(
                                "✓ {name} reconnected · {count} tools"
                            )));
                        }
                        Err(e) => {
                            unpin();
                            let _ = events.send(UiEvent::SlashError(format!("✗ {e}")));
                        }
                    }
                });
            }
            _ => self.push_slash_error(
                "usage: /mcp [enable|disable [name|all]] · /mcp reconnect <name>".to_string(),
            ),
        }
    }

    /// `/provider [name]`: no argument opens the selector (picker-model.md commit D); an argument takes the fast path.
    fn slash_provider(&mut self, arg: &str) {
        if let Some(rest) = arg.strip_prefix("login ") {
            return self.slash_provider_login(rest.trim());
        }
        if let Some(rest) = arg.strip_prefix("logout ") {
            return self.slash_provider_logout(rest.trim());
        }
        if arg.is_empty() {
            self.open_provider_menu();
            return;
        }
        self.switch_provider(arg, true);
    }

    /// Switch the provider: takes effect in the runtime immediately; persist=true writes settings (restored on restart).
    ///
    /// provider+model is an atomic selection: switching endpoints must resolve the model (this session's last-used →
    /// endpoint default → keep + warn) — the same rule as the subagent cross-provider check;
    /// the main session used to keep the old model silently, and the next message 404'd on the new endpoint.
    fn switch_provider(&mut self, name: &str, persist: bool) {
        // A mid-turn protocol swap would send this conversation's accumulated
        // thinking/reasoning blocks to the other protocol's endpoint — refuse
        // instead of corrupting the running turn.
        if self.busy {
            self.push_slash_error(
                "[error] code=BUSY msg=cannot switch providers mid-turn (press Esc to interrupt, then retry)".to_string(),
            );
            return;
        }
        let session = self.session.clone();
        let name = name.to_string();
        match session.client.set_provider(&name) {
            Ok(()) => {
                let (_, url) = session.client.current_endpoint();
                let prev_provider = session.runtime.provider.borrow().clone();
                let prev_model = session.runtime.model.borrow().clone();
                if prev_provider != name {
                    self.provider_models
                        .insert(prev_provider, prev_model.clone());
                }
                let _ = session.runtime.provider_tx.send(name.clone());
                let resolved = self
                    .provider_models
                    .get(&name)
                    .cloned()
                    .or_else(|| session.client.provider_default_model(&name));
                let model_note = match &resolved {
                    Some(model) if *model != prev_model => {
                        let _ = session.runtime.model_tx.send(model.clone());
                        format!(" · model {model}")
                    }
                    Some(_) => String::new(),
                    None => format!(
                        " · ⚠ model {prev_model} may not be available on this endpoint (pick with /model)"
                    ),
                };
                let model_now = session.runtime.model.borrow().clone();
                self.context_usage = crate::context_usage::ContextUsage::new(
                    self.context_usage.used,
                    crate::budget::context_window_for(&model_now),
                );
                self.provider_models.insert(name.clone(), model_now.clone());
                self.provider_session_only = !persist;
                if persist {
                    // Same path as the /model menu: provider+model persist as a pair.
                    self.persist_selection(&model_now, &name);
                    self.push_slash_output(format!(
                        "✓ provider switched: {name} ({url}){model_note}"
                    ));
                } else {
                    self.push_slash_output(format!(
                        "✓ provider switched: {name} ({url}){model_note} (this session only)"
                    ));
                }
                // Credentials unavailable: the switch succeeds but the first request would fail — guide early (the criterion is
                // "are credentials available", not just the auth kind — a keyless apiKey-style
                // preset used to pass silently).
                match session.client.auth_status(&name) {
                    Some(crate::api::contract::AuthStatus::OAuth { account: None }) => {
                        self.push_slash_output(format!(
                            "⚠ {name} not logged in: /provider login {name}"
                        ));
                    }
                    Some(crate::api::contract::AuthStatus::StoredKey { configured: false }) => {
                        self.push_slash_output(format!(
                            "⚠ {name} has no API key configured: /provider login {name} --manual <key>"
                        ));
                    }
                    Some(crate::api::contract::AuthStatus::Unconfigured) => {
                        self.push_slash_output(
                            "⚠ default has no credentials: write apiKey in settings or /provider login codex"
                                .to_string(),
                        );
                    }
                    _ => {}
                }
            }
            Err(e) => self.push_slash_error(e),
        }
    }

    /// Option description: URL + redacted key (first 4 chars; short keys get no ellipsis — following the existing info column).
    fn provider_desc(&self, name: &str) -> String {
        let (key, url) = self
            .session
            .client
            .provider_endpoint(name)
            .unwrap_or_else(|| (None, "?".to_string()));
        let protocol = self
            .session
            .client
            .provider_protocol(name)
            .unwrap_or_default();
        let auth = match self.session.client.auth_status(name) {
            Some(crate::api::contract::AuthStatus::ApiKey) => {
                let key = key.unwrap_or_default();
                if key.is_empty() {
                    format!("○ not configured (/provider login {name})")
                } else {
                    let mut key_shown: String = key.chars().take(4).collect();
                    if key.chars().count() > 4 {
                        key_shown.push('…');
                    }
                    format!("key {key_shown}")
                }
            }
            // The key in auth.json (--manual): configured reads live — logging in during this
            // session immediately flips it from "not configured" to "configured".
            Some(crate::api::contract::AuthStatus::StoredKey { configured: true }) => {
                "✓ key (auth.json)".to_string()
            }
            Some(crate::api::contract::AuthStatus::StoredKey { configured: false }) => {
                format!("○ not configured (/provider login {name} --manual <key>)")
            }
            Some(crate::api::contract::AuthStatus::OAuth { account: Some(acc) }) => {
                format!("✓ {acc}")
            }
            Some(crate::api::contract::AuthStatus::OAuth { account: None }) => {
                format!("○ not logged in (/provider login {name})")
            }
            Some(crate::api::contract::AuthStatus::Unconfigured) => {
                "○ not configured (write apiKey in settings or /provider login codex)".to_string()
            }
            None => "?".to_string(),
        };
        let badge = if self.session.client.is_preset(name) {
            " · built-in"
        } else {
            ""
        };
        format!("{url} ({auth} · {protocol}{badge})")
    }

    /// Open the `/provider` selector: default first (the top-level endpoint), then named providers;
    /// ● marks the current provider, other menus close exclusively.
    fn open_provider_menu(&mut self) {
        let current = self.session.runtime.provider.borrow().clone();
        // Order shares a source with /model's level one (provider_order): the number jump means the same in both places.
        let names = self.provider_order();
        let mut options = Vec::with_capacity(names.len());
        for name in names {
            options.push((name.clone(), self.provider_desc(&name)));
        }
        let current = options.iter().position(|(n, _)| *n == current);
        let menu = ProviderMenu {
            selected: current.unwrap_or(0),
            current,
            options,
        };
        if menu.picker().is_empty() {
            return;
        }
        self.close_menus();
        self.provider_menu = Some(menu);
        self.clear_slash_suggestions();
    }

    /// Provider menu keys: ↑↓/1-N move (delegated to the PickerModel core),
    /// Enter switches + persists, s = session-only (no settings write), Esc exits.
    fn provider_menu_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let Some(menu) = &mut self.provider_menu else {
            return false;
        };
        match code {
            KeyCode::Down if !modifiers.contains(KeyModifiers::CONTROL) => {
                let mut core = menu.picker();
                core.move_selection(1);
                menu.selected = core.selected;
                true
            }
            KeyCode::Up if !modifiers.contains(KeyModifiers::CONTROL) => {
                let mut core = menu.picker();
                core.move_selection(-1);
                menu.selected = core.selected;
                true
            }
            KeyCode::Char(c)
                if c.is_ascii_digit() && !modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let mut core = menu.picker();
                if let Some(n) = c.to_digit(10)
                    && core.jump(n as usize)
                {
                    menu.selected = core.selected;
                }
                // Swallow even out-of-range digits: a menu is a modal surface —
                // "4" on a 3-item picker used to type a literal 4 into the input.
                true
            }
            KeyCode::Char('s') if !modifiers.contains(KeyModifiers::CONTROL) => {
                let core = menu.picker();
                let value = core
                    .selected_item()
                    .map(|i| i.value.clone())
                    .unwrap_or_default();
                self.provider_menu = None;
                self.switch_provider(&value, false);
                true
            }
            KeyCode::Enter => {
                let core = menu.picker();
                let value = core
                    .selected_item()
                    .map(|i| i.value.clone())
                    .unwrap_or_default();
                self.provider_menu = None;
                self.switch_provider(&value, true);
                true
            }
            KeyCode::Esc => {
                self.provider_menu = None;
                true
            }
            _ => false,
        }
    }

    /// `/provider login <name> [--device-auth|--manual <token>]`: OAuth login
    /// for a provider with an `oauth` config (D33 §6). Default = loopback
    /// PKCE (opens the browser); `--device-auth` prints URL + code and polls
    /// (headless/SSH); `--manual` stores a pasted token (no refresh).
    fn slash_provider_login(&mut self, arg: &str) {
        let parts: Vec<&str> = arg.split_whitespace().collect();
        let Some(name) = parts.first() else {
            self.push_slash_output(
                "usage: /provider login <name> [--device-auth|--manual <token>]".to_string(),
            );
            return;
        };
        let manual = parts
            .iter()
            .position(|p| *p == "--manual")
            .and_then(|i| parts.get(i + 1).copied());
        let device_auth = parts.contains(&"--device-auth");

        let session = self.session.clone();
        // Effective config = user settings ⊕ built-in preset (D34 §6.5):
        // presets make official subscriptions loginable with zero config.
        let preset = crate::api::providers::presets::preset(name);
        let known = session.settings.providers.contains_key(*name) || preset.is_some();
        if !known {
            self.push_slash_error(format!(
                "provider \"{name}\" not found (see /provider for the list)"
            ));
            return;
        }
        let oauth_kind = session
            .settings
            .providers
            .get(*name)
            .and_then(|c| c.oauth.as_ref().map(|o| o.kind.clone()))
            .or_else(|| preset.and_then(|p| p.oauth_kind.map(str::to_string)));
        let name = name.to_string();
        let home = session.home.clone();
        let events = self.events.clone();
        let http = reqwest::Client::new();
        let config = crate::api::auth::OauthFlowConfig::codex();

        // --manual first: works for both apiKey presets (opencode-go, stores
        // auth.json `{type:"api"}`) and oauth presets (pasted access token).
        if let Some(token) = manual {
            let token = token.to_string();
            let api_preset = preset.map(|p| p.oauth_kind.is_none()).unwrap_or(false);
            // Share the session Client's TokenProvider: saving through the
            // same instance updates the adapter's cache + account mirror, so
            // the login takes effect in this session (no restart).
            let shared_tp = session.client.token_provider(&name);
            tokio::spawn(async move {
                if api_preset {
                    let store = crate::auth::AuthStore::new(&home);
                    match store.set(&name, crate::auth::AuthEntry::Api { key: token }) {
                        Ok(()) => {
                            let _ = events.send(UiEvent::SlashOutput(format!(
                                "✓ saved {name}'s API key (subscription key)"
                            )));
                        }
                        Err(e) => {
                            let _ = events.send(UiEvent::SlashError(format!("✗ save failed: {e}")));
                        }
                    }
                    return;
                }
                let tp = shared_tp.unwrap_or_else(|| {
                    std::sync::Arc::new(crate::api::auth::TokenProvider::new(&home, &name, config))
                });
                let tokens = crate::api::auth::TokenSet {
                    access_token: token,
                    refresh_token: String::new(),
                    id_token: None,
                    expires_at: None,
                    account_id: None,
                };
                match tp.save(&tokens).await {
                    Ok(()) => {
                        let _ = events.send(UiEvent::SlashOutput(format!(
                            "✓ saved {name}'s login info (a --manual token does not auto-refresh)"
                        )));
                    }
                    Err(e) => {
                        let _ = events.send(UiEvent::SlashError(format!("✗ save failed: {e}")));
                    }
                }
            });
            return;
        }

        // OAuth gate: codex only in v1; apiKey presets guide the key paste.
        let Some(oauth_kind) = oauth_kind else {
            self.push_slash_info(format!(
                "provider \"{name}\" requires an API key (subscription key):\n  1. get one at opencode.ai/auth\n  2. /provider login {name} --manual <key>"
            ));
            return;
        };
        if oauth_kind != "codex" {
            self.push_slash_error(format!(
                "unsupported oauth.kind \"{oauth_kind}\" (v1 supports only codex)"
            ));
            return;
        }

        if device_auth {
            // headless/SSH: print the URL + one-time code, poll for authorization.
            let shared_tp = session.client.token_provider(&name);
            tokio::spawn(async move {
                let flow = crate::api::auth::DeviceFlow::new(&http, &config);
                match flow.start().await {
                    Ok((prompt, device_auth_id, interval)) => {
                        // Pinned: the code is valid for 15 minutes — it must
                        // stay on screen for all of them (the 2s TTL burned
                        // it before anyone could type it).
                        let _ = events.send(UiEvent::PinPanel {
                            id: "login".to_string(),
                            lines: vec![
                                format!("sign in to {name} (device authorization)"),
                                format!("  1. open {}", prompt.verification_url),
                                format!("  2. enter code {} (valid for 15 minutes)", prompt.user_code),
                                "⏳ waiting for authorization… (Esc will not cancel; the panel disappears when done)".to_string(),
                            ],
                        });
                        let outcome = flow
                            .poll(&device_auth_id, &prompt.user_code, interval)
                            .await;
                        let _ = events.send(UiEvent::Unpin {
                            id: "login".to_string(),
                        });
                        match outcome {
                            Ok(tokens) => {
                                let tp = shared_tp.unwrap_or_else(|| {
                                    std::sync::Arc::new(crate::api::auth::TokenProvider::new(
                                        &home, &name, config,
                                    ))
                                });
                                match tp.save(&tokens).await {
                                    Ok(()) => {
                                        let _ = events.send(UiEvent::SlashOutput(format!(
                                            "✓ signed in to {name}"
                                        )));
                                    }
                                    Err(e) => {
                                        let _ = events.send(UiEvent::SlashOutput(format!(
                                            "✗ save failed: {e}"
                                        )));
                                    }
                                }
                            }
                            Err(e) => {
                                let _ = events
                                    .send(UiEvent::SlashOutput(format!("✗ sign-in failed: {e}")));
                            }
                        }
                    }
                    Err(e) => {
                        let _ = events.send(UiEvent::SlashError(format!("✗ sign-in failed: {e}")));
                    }
                }
            });
            return;
        }

        // Default: loopback PKCE (local callback + opening the browser).
        let shared_tp = session.client.token_provider(&name);
        tokio::spawn(async move {
            let flow = crate::api::auth::LoopbackPkce::new(&http, &config);
            match flow.authorize_url().await {
                Ok((url, _redirect, _verifier, handle)) => {
                    // Pinned, with the URL itself: on SSH/no-GUI hosts the
                    // browser never opens and this line is the only way
                    // through (it used to say "tried to open" and show nothing).
                    let _ = events.send(UiEvent::PinPanel {
                        id: "login".to_string(),
                        lines: vec![
                            format!("sign in to {name}: complete the authorization in the browser (tried to open it automatically)"),
                            format!("  {url}"),
                            format!("  browser did not open? /provider login {name} --device-auth"),
                        ],
                    });
                    let _ = crate::share::open_in_browser(&url);
                    let outcome = handle.await;
                    let _ = events.send(UiEvent::Unpin {
                        id: "login".to_string(),
                    });
                    match outcome {
                        Ok(Ok(tokens)) => {
                            let tp = shared_tp.unwrap_or_else(|| {
                                std::sync::Arc::new(crate::api::auth::TokenProvider::new(
                                    &home, &name, config,
                                ))
                            });
                            match tp.save(&tokens).await {
                                Ok(()) => {
                                    let _ = events.send(UiEvent::SlashOutput(format!(
                                        "✓ signed in to {name}"
                                    )));
                                }
                                Err(e) => {
                                    let _ = events
                                        .send(UiEvent::SlashOutput(format!("✗ save failed: {e}")));
                                }
                            }
                        }
                        Ok(Err(e)) => {
                            let _ =
                                events.send(UiEvent::SlashError(format!("✗ sign-in failed: {e}")));
                        }
                        Err(e) => {
                            let _ = events
                                .send(UiEvent::SlashError(format!("✗ sign-in interrupted: {e}")));
                        }
                    }
                }
                Err(e) => {
                    let _ = events.send(UiEvent::SlashError(format!("✗ sign-in failed: {e}")));
                }
            }
        });
    }

    fn slash_provider_logout(&mut self, arg: &str) {
        let name = arg.trim();
        if name.is_empty() {
            self.push_slash_error("usage: /provider logout <name>".to_string());
            return;
        }
        let session = self.session.clone();
        let preset = crate::api::providers::presets::preset(name);
        let known = session.settings.providers.contains_key(name) || preset.is_some();
        if !known {
            self.push_slash_error(format!(
                "provider \"{name}\" not found (see /provider for the list)"
            ));
            return;
        }
        let oauth_kind = session
            .settings
            .providers
            .get(name)
            .and_then(|c| c.oauth.as_ref().map(|o| o.kind.clone()))
            .or_else(|| preset.and_then(|p| p.oauth_kind.map(str::to_string)));
        let name = name.to_string();
        let home = session.home.clone();
        let events = self.events.clone();
        let shared_tp = session.client.token_provider(&name);
        tokio::spawn(async move {
            if oauth_kind.as_deref() != Some("codex") {
                // apiKey preset (opencode-go): only clears the auth.json entry.
                match crate::auth::AuthStore::new(&home).remove(&name) {
                    Ok(()) => {
                        let _ = events.send(UiEvent::SlashOutput(format!(
                            "✓ signed out of {name} (key cleared)"
                        )));
                    }
                    Err(e) => {
                        let _ = events.send(UiEvent::SlashError(format!("✗ sign-out failed: {e}")));
                    }
                }
                return;
            }
            // Same shared instance as login: the session's adapter loses its
            // cached token immediately (a stale cache kept requests working
            // after logout).
            let tp = shared_tp.unwrap_or_else(|| {
                std::sync::Arc::new(crate::api::auth::TokenProvider::new(
                    &home,
                    &name,
                    crate::api::auth::OauthFlowConfig::codex(),
                ))
            });
            match tp.logout().await {
                Ok(()) => {
                    let _ = events.send(UiEvent::SlashOutput(format!(
                        "✓ signed out of {name} (credentials cleared)"
                    )));
                }
                Err(e) => {
                    let _ = events.send(UiEvent::SlashError(format!("✗ sign-out failed: {e}")));
                }
            }
        });
    }

    fn slash_think(&mut self, arg: &str) {
        if arg.is_empty() {
            self.open_think_menu();
            return;
        }
        self.set_think_level(arg, true);
    }

    /// Sets the thinking level. Level table = off + THINKING_LEVELS: off sends no
    /// parameter; the rest send adaptive thinking + output_config.effort.
    /// `persist = false` applies to the current session only (no settings write).
    fn set_think_level(&mut self, arg: &str, persist: bool) {
        let level = if arg == "off" {
            None
        } else if crate::api::contract::THINKING_LEVELS.contains(&arg) {
            Some(arg.to_string())
        } else {
            self.push_slash_error(format!(
                "[error] code={} msg=usage: /think [off|low|medium|high|xhigh|max]",
                crate::error::SLASH_ERROR_BAD_ARGUMENT
            ));
            return;
        };
        let _ = self.session.runtime.thinking_tx.send(level.clone());
        let saved = level.as_deref().unwrap_or("off");
        // The wire gate (query.rs) skips thinking for models that reject it —
        // say so here, or the footer shows a level that never takes effect.
        let model = self.session.runtime.model.borrow().clone();
        let ignored = if level.is_some() && !crate::api::models::supports_thinking(&model) {
            format!(" (⚠ {model} does not support thinking; the level will be ignored)")
        } else {
            String::new()
        };
        if persist {
            let cwd = std::path::PathBuf::from(&self.cwd);
            let _ = crate::settings::upsert_scoped_settings(
                &self.session.user_config_dir,
                &cwd,
                &serde_json::json!({ "thinkingLevel": saved }),
            );
            self.push_slash_output(format!("✓ thinking level set: {saved}{ignored}"));
        } else {
            self.push_slash_output(format!(
                "✓ thinking level set: {saved} (this session only){ignored}"
            ));
        }
    }

    /// Enters the `/think` level selector: preselects the current level (off when unset).
    fn open_think_menu(&mut self) {
        let current = self.session.runtime.thinking.borrow().clone();
        let current = current.as_deref().unwrap_or("off");
        let current = THINK_LEVELS
            .iter()
            .position(|(name, _)| *name == current)
            .unwrap_or(0);
        let menu = ThinkMenu {
            selected: current,
            current,
        };
        // Empty-table guard (THINK_LEVELS is a non-empty const; this defensive branch is unreachable): the menu stays closed.
        if menu.picker().is_empty() {
            return;
        }
        self.close_menus();
        self.think_menu = Some(menu);
        self.clear_slash_suggestions();
    }

    /// Think level menu keys: ↑↓/1-6 move (wraps, delegated to the PickerModel core),
    /// Enter confirms + persists, s = session-only (no settings write), Esc exits.
    /// Returns whether consumed.
    fn think_menu_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let Some(menu) = &mut self.think_menu else {
            return false;
        };
        match code {
            KeyCode::Down if !modifiers.contains(KeyModifiers::CONTROL) => {
                let mut core = menu.picker();
                core.move_selection(1);
                menu.selected = core.selected;
                true
            }
            KeyCode::Up if !modifiers.contains(KeyModifiers::CONTROL) => {
                let mut core = menu.picker();
                core.move_selection(-1);
                menu.selected = core.selected;
                true
            }
            // Direct jump: 1 = off … 6 = max (fixed 6-item table, §G10).
            KeyCode::Char(c)
                if c.is_ascii_digit() && !modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let mut core = menu.picker();
                if let Some(n) = c.to_digit(10)
                    && core.jump(n as usize)
                {
                    menu.selected = core.selected;
                }
                // Swallow even out-of-range digits: a menu is a modal surface —
                // "4" on a 3-item picker used to type a literal 4 into the input.
                true
            }
            KeyCode::Char('s') if !modifiers.contains(KeyModifiers::CONTROL) => {
                let core = menu.picker();
                let value = core
                    .selected_item()
                    .map(|i| i.value.clone())
                    .unwrap_or_default();
                self.think_menu = None;
                self.set_think_level(&value, false);
                true
            }
            KeyCode::Enter => {
                let core = menu.picker();
                let value = core
                    .selected_item()
                    .map(|i| i.value.clone())
                    .unwrap_or_default();
                self.think_menu = None;
                self.set_think_level(&value, true);
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
                "no skills available.\nSkills live in .bingo/skills/<name>/SKILL.md or $XDG_CONFIG_HOME/bingo/skills/<name>/SKILL.md."
                    .to_string(),
            );
            return;
        }
        let listing = crate::skills::format_listing(&skills, crate::skills::DEFAULT_CHAR_BUDGET);
        self.push_slash_info(format!("available skills:\n{listing}"));
    }

    fn slash_tasks(&mut self) {
        self.refresh_tasks();
        // task_lines is gated by task-area visibility — /tasks explicitly asks for them, so bypass it temporarily.
        let was_visible = self.tasks_visible;
        self.tasks_visible = true;
        let lines = self.task_lines();
        self.tasks_visible = was_visible;
        if lines.is_empty() {
            self.push_slash_output("no background tasks right now.".to_string());
            return;
        }
        let text: Vec<String> = lines.iter().map(|l| l.plain_text()).collect();
        self.push_slash_info(text.join("\n"));
    }

    /// `/team <subcommand>` (D31 project-level formation): dispatched to team_cmd, multi-line output queued at once.
    fn slash_team(&mut self, arg: &str) {
        let lines = crate::team_cmd::run(&self.session, &std::path::PathBuf::from(&self.cwd), arg);
        self.push_slash_info(lines.join("\n"));
    }

    /// Rebuilds the slash dropdown from the registry and currently loaded skills.
    fn update_slash_suggestions(&mut self) {
        self.clear_slash_suggestions();
        let home = self.session.home.clone();
        let cwd = std::path::PathBuf::from(&self.cwd);
        let skills = crate::skills::load_skills(&home, &cwd)
            .into_iter()
            .map(|skill| {
                let mut description = skill.description;
                if description.chars().count() > crate::skills::MAX_LISTING_DESC_CHARS {
                    let cut: String = description
                        .chars()
                        .take(crate::skills::MAX_LISTING_DESC_CHARS - 1)
                        .collect();
                    description = format!("{cut}…");
                }
                SlashSuggestion {
                    name: skill.name,
                    hint: String::new(),
                    description,
                }
            });
        let result = crate::tui::slash::suggestions(
            &self.input,
            SLASH_COMMANDS,
            skills,
            // Full list: rendering windows around the selection (the old
            // hard cap made commands 6+ unreachable from a bare `/`).
            usize::MAX,
        );
        self.slash_suggestions = result.items;
        self.slash_selected = self
            .slash_selected
            .min(self.slash_suggestions.len().saturating_sub(1));
        self.slash_no_match = result.no_match;
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
                self.clear_slash_suggestions();
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
        self.clear_slash_suggestions();
    }

    /// Submits the next queued item after a turn (one at a time: a plain message starts
    /// the next turn; queued slash commands drain synchronously until one does).
    fn submit_queued(&mut self) {
        if self.busy || self.queued.is_empty() {
            return;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        // Drain queued slash commands synchronously; stop at the first plain message
        // (it starts a turn, which re-triggers submit_queued on TurnEnd).
        loop {
            let Some(first) = self.queued.first() else {
                return;
            };
            if !first.is_slash {
                break;
            }
            let item = self.queued.remove(0);
            self.run_slash(item.text.strip_prefix('/').unwrap_or(&item.text));
            if self.busy {
                return; // a skill command started a turn; the rest waits for TurnEnd
            }
        }
        let item = self.queued.remove(0);
        self.start_turn(item.text, true);
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
            let _ = events.send(UiEvent::Warning("turn interrupted".to_string()));
        } else {
            match outcome.end_reason {
                crate::query::QueryEndReason::EmptyResponseRetried => {
                    let _ = events.send(UiEvent::Warning(
                        "model returned an empty response and was retried".to_string(),
                    ));
                }
                crate::query::QueryEndReason::Completed => {}
            }
        }
        let _ = events.send(UiEvent::TurnEnd);
        let cwd = session.cwd();
        crate::memory::extract_memory(session, &outcome.messages, &session.home, &cwd).await;
    }

    /// A turn task that dies without reporting an outcome (a panic inside the spawn) leaves
    /// `busy` latched, and every interrupt and quit route is gated on `busy` — the session
    /// then answers only to `kill`. Watching the handle turns a lost turn back into the
    /// ordinary long-turn error state, which releases `busy` and offers retry / go back.
    fn supervise_turn(events: mpsc::UnboundedSender<UiEvent>, handle: tokio::task::JoinHandle<()>) {
        tokio::spawn(async move {
            if handle.await.is_err() {
                let _ = events.send(UiEvent::Error {
                    code: crate::error::TURN_LOST,
                    msg: "The turn ended unexpectedly; retry or go back.".to_string(),
                    level: crate::error::ErrorLevel::Full,
                    context: crate::error::ErrorContext::LongTurn,
                });
            }
        });
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
        let handle = tokio::spawn(async move {
            let _ = events.send(UiEvent::TurnStart);
            let mut ui = crate::ui::tui_hooks(events.clone(), asks);
            let history = Self::load_history(&session, &mut ui.on_warning);
            let result =
                run_query(&session, history, &text, &images, &mut ui, Some(cancel_rx)).await;
            match result {
                Ok(outcome) => {
                    Self::finish_turn(&events, &session, &outcome).await;
                }
                Err(e) => {
                    let code = crate::error::map_error(&e);
                    let _ = events.send(UiEvent::Error {
                        code,
                        msg: Self::auth_error_hint(&session, code, e.to_string()),
                        // Turn-level error = long-turn failure → full-flow full-screen state (AC-53).
                        level: crate::error::ErrorLevel::Full,
                        context: crate::error::ErrorContext::LongTurn,
                    });
                }
            }
        });
        Self::supervise_turn(self.events.clone(), handle);
    }

    /// Turn-level error message with auth guidance for the current provider:
    /// `AUTH_REQUIRED` on an oauth-configured provider appends a re-login
    /// hint (the raw API error body rarely tells the user what to do);
    /// `PERMISSION_DENIED` points at the model/subscription (D33 §6.4).
    fn auth_error_hint(session: &Session, code: &str, msg: String) -> String {
        let provider = session.runtime.provider.borrow().clone();
        // Merge built-in presets: zero-config codex (no settings entry) is the
        // main preset use case — without this, its expired token produced a
        // bare 401 with no re-login guidance.
        let oauth = session
            .settings
            .providers
            .get(&provider)
            .map(|c| c.oauth.is_some())
            .or_else(|| {
                crate::api::providers::presets::preset(&provider).map(|p| p.oauth_kind.is_some())
            })
            .unwrap_or(false);
        auth_hint_for(oauth, &provider, code, msg)
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
        // Same as start_turn: a fresh turn clears interrupt suppression —
        // without this, one interrupt followed by only `!` commands kept
        // background wake-ups suppressed for the rest of the session.
        self.interrupted = false;
        let session = self.session_for_turn();
        let events = self.events.clone();
        let asks = self.asks.clone();
        // Same as start_turn: subscribe first, then reset (send does not update with no receivers).
        let cancel_rx = self.cancel_tx.subscribe();
        self.cancel_tx.send_replace(false);
        let handle = tokio::spawn(async move {
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
                    let code = crate::error::map_error(&e);
                    let _ = events.send(UiEvent::Error {
                        code,
                        msg: Self::auth_error_hint(&session, code, e.to_string()),
                        // Turn-level error = long-turn failure → full-flow full-screen state (AC-53).
                        level: crate::error::ErrorLevel::Full,
                        context: crate::error::ErrorContext::LongTurn,
                    });
                }
            }
        });
        Self::supervise_turn(self.events.clone(), handle);
    }

    /// Dialog key input (Select semantics):
    /// digits/Enter confirm, ↑/↓ move the focus, Esc cancels; typing goes directly when the focus is on Other.
    /// Returns whether it was consumed.
    ///
    /// Modifier-carrying chars are NOT consumed: crossterm reports ctrl+c as
    /// `Char('c')` + CONTROL, so swallowing them here turned the interrupt (and
    /// every readline chord) into literal letters inside the Other input.
    pub fn ask_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let Some((request, _)) = &self.pending_ask else {
            return false;
        };
        if matches!(code, KeyCode::Char(_))
            && modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            return false;
        }
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

    /// AskUserQuestion answer message: header + one line of `· question → answer`. Treated as
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
        self.open_continuation_message();
    }

    /// The answer lands mid-turn and the model keeps going. Without a message of its own, that
    /// continuation streams into the assistant message *above* the answer (`stream_msg` still
    /// points there), so everything the model does next renders above what the user just said
    /// and the answer stays pinned to the bottom until the turn ends. Close the old message and
    /// open a fresh one, the way a turn boundary would: the transcript then reads in clock order.
    fn open_continuation_message(&mut self) {
        let Some(prev) = self.stream_msg else { return };
        // Tool rows registered before the answer index into `prev`'s activities
        // (`pending_tools` holds those indices), so a call still in flight pins the stream here.
        if !self.pending_tools.is_empty() {
            return;
        }
        // AskUserQuestion is a hidden tool: `ToolStart` returns before closing the running
        // thinking block, and a block left running would keep `prev` from ever settling
        // (`message_static_settled`) — with it the whole flush prefix, for the rest of the session.
        self.close_running_thinking(prev);
        // The buffer belongs to the block just closed; carried over, the next reasoning delta
        // would try to merge into a block the new message does not have, and be dropped.
        self.thinking_buf.clear();
        self.thinking_seg_open = false;
        self.messages.push(UiMessage {
            role: Role::Assistant,
            text: String::new(),
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        self.stream_msg = Some(self.messages.len() - 1);
        self.continuation_msg = self.stream_msg;
    }

    /// A continuation message the turn never filled (the answer was the last thing that happened):
    /// an empty assistant block renders as a stray gap. Only ever drops the message
    /// [`Chat::open_continuation_message`] opened. Call before clearing `stream_msg`.
    fn drop_empty_stream_message(&mut self) {
        let Some(i) = self.continuation_msg.take() else {
            return;
        };
        if self.stream_msg == Some(i)
            && i + 1 == self.messages.len()
            && self.messages[i].text.is_empty()
            && self.messages[i].activities.is_empty()
        {
            self.messages.pop();
            self.stream_msg = None;
        }
    }

    /// A tool call, message text, or a mid-turn answer all end the current reasoning segment.
    fn close_running_thinking(&mut self, i: usize) {
        let tick = self.tick;
        for hint in &mut self.messages[i].activities {
            if let ActivityKind::Thinking(t) = &mut hint.kind
                && t.state == ThinkingState::Running
            {
                t.state = ThinkingState::Done;
                t.duration_ms = tick.saturating_sub(t.start_tick).saturating_mul(33);
            }
        }
    }

    /// Submitting Other free text (CC SelectInputOption onSubmit: empty text = cancel).
    fn submit_ask_answer(&mut self, text: String) {
        if text.trim().is_empty() {
            let free_text = self.pending_ask.as_ref().is_some_and(|(r, _)| r.free_text);
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
        // Update-banner breathing (P1): the first keypress in the window stops it immediately (the user's attention has moved to the input;
        // the banner itself stays, it just stops breathing).
        if self.update_anim_active() {
            self.update_banner_stopped = true;
        }
        // #18 full-flow full-screen error state: primary actions Enter=retry / Esc=back, the rest ignored.
        if let Some(err) = &self.last_error
            && err.level == crate::error::ErrorLevel::Full
        {
            return self.error_screen_key(code, modifiers, now);
        }
        if self.ask_key(code, modifiers) {
            return true;
        }
        // A printable key that no menu claims (menus only take ↑↓/Enter/Esc/
        // digits/s) closes the menu first, then edits normally. Without this,
        // typing "/theme" over an open /think menu kept feeding a menu the
        // screen no longer showed — Enter landed on an invisible selection.
        if self.menu_open()
            && !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && matches!(code, KeyCode::Char(c) if !c.is_ascii_digit() && c != 's')
        {
            self.close_menus();
        }
        // `/model` `/think` selectors take priority over input (↑↓/Enter/Esc fully consumed).
        if self.model_menu_key(code, modifiers) {
            return true;
        }
        if self.think_menu_key(code, modifiers) {
            return true;
        }
        if self.theme_menu_key(code, modifiers) {
            return true;
        }
        if self.resume_menu_key(code, modifiers) {
            return true;
        }
        if self.provider_menu_key(code, modifiers) {
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
                if pasting || modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
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
            // The quit path below is gated on `busy`, so a turn that never clears it (its
            // task died) used to leave `kill` as the only way out. An interrupt the turn
            // has ignored for [`INTERRUPT_GRACE`] hands Ctrl+C back its exit meaning.
            if self
                .interrupt_at
                .is_some_and(|at| now.duration_since(at) >= INTERRUPT_GRACE)
            {
                self.exit = true;
                return true;
            }
            self.interrupt(now);
            self.notice = Some("Interrupting… press ctrl-c again to force quit");
            self.notice_until = Some(now + CTRL_C_WINDOW);
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
        self.notice_until = Some(now + CTRL_C_WINDOW);
        true
    }

    /// Esc: interrupts when busy; closes menus/suggestions; double-press with text while idle clears (into history).
    fn escape(&mut self, now: std::time::Instant) -> bool {
        if self.busy {
            self.interrupt(now);
            return true;
        }
        // A Page/Field error row is dismissable like every other overlay —
        // it used to sit above the prompt until the next turn started.
        if self
            .last_error
            .as_ref()
            .is_some_and(|e| e.level != crate::error::ErrorLevel::Full)
        {
            self.last_error = None;
            self.dirty = true;
            return true;
        }
        if !self.slash_suggestions.is_empty() {
            self.clear_slash_suggestions();
            // The dropdown only exists for a pure `/`-query — dismissing it
            // dismisses the query too (the leftover "/th" used to turn the
            // next command into "//model").
            if self.input.starts_with('/') {
                self.set_input("");
            }
            return true;
        }
        if !self.slash_info_lines.is_empty() {
            self.slash_info_lines.clear();
            self.dirty = true;
            return true;
        }
        if self.help_visible {
            self.help_visible = false;
            return true;
        }
        // The tasks panel opened with ctrl+t closes with Esc (it used to have
        // no exit at all — the ? panel closed, this one squatted).
        if self.tasks_visible && !self.tasks_auto {
            self.tasks_visible = false;
            self.dirty = true;
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
        self.notice_until = Some(now + ESC_WINDOW);
        true
    }

    /// Interrupts the current turn (Esc / Ctrl+C while busy). The first request is stamped
    /// so Ctrl+C can tell "the turn is stopping" from "the turn is never going to answer".
    fn interrupt(&mut self, now: std::time::Instant) {
        self.interrupted = true;
        self.interrupt_at.get_or_insert(now);
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
            if let Some(item) = self.queued.pop() {
                self.set_input(item.text);
            }
            return true;
        }
        let width = self.input_width();
        if let Some(cursor) = crate::tui::input::move_row(&self.input, self.cursor, width, down) {
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
        // G12: error/usage rows clear on the next input — the user has acted on them.
        if !self.slash_error_lines.is_empty() {
            self.slash_error_lines.clear();
            self.slash_error_at = None;
            self.dirty = true;
        }
        // Info output follows the same rule: reading time until the user acts.
        if !self.slash_info_lines.is_empty() {
            self.slash_info_lines.clear();
            self.dirty = true;
        }
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
        let coalesce =
            kind != EditKind::Bulk && self.last_edit == Some(kind) && !self.undo.is_empty();
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
                self.notice = Some("draft restored");
                self.notice_until = Some(std::time::Instant::now() + CTRL_C_WINDOW);
            }
            return;
        }
        let replaced = self.stash.is_some();
        self.stash = Some((std::mem::take(&mut self.input), self.cursor));
        self.cursor = 0;
        self.last_edit = None;
        self.update_slash_suggestions();
        self.notice = Some(if replaced {
            "draft saved (old draft overwritten) · ctrl+s on an empty input restores it"
        } else {
            "draft saved · ctrl+s on an empty input restores it"
        });
        self.notice_until = Some(std::time::Instant::now() + CTRL_C_WINDOW);
    }

    /// Shift+Tab: default → acceptEdits → plan → default.
    /// bypassPermissions / dontAsk stay in the cycle only when the session started in that mode
    /// (dangerous modes must not be reachable by one mispress).
    fn cycle_permission_mode(&mut self) {
        self.permission_mode = match self.permission_mode {
            PermissionMode::Default => PermissionMode::AcceptEdits,
            PermissionMode::AcceptEdits => PermissionMode::Plan,
            PermissionMode::Plan => PermissionMode::Default,
            // Started in bypass/dontAsk: toggle between it and default, never introducing a new dangerous mode.
            PermissionMode::BypassPermissions | PermissionMode::DontAsk => PermissionMode::Default,
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
            None | Some("off") => self
                .last_thinking
                .clone()
                .unwrap_or_else(|| "medium".into()),
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
        self.close_menus();
        let mut search = HistorySearch::default();
        if let Some((index, hit)) = self.history.search("", None) {
            search.index = Some(index);
            search.hit = Some(hit);
        }
        self.search = Some(search);
        self.clear_slash_suggestions();
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
                match search.hit {
                    Some(hit) => {
                        self.set_input(hit);
                        self.submit();
                    }
                    // No match: keep the search layer open (it used to close
                    // silently, eating the Enter).
                    None => self.search = Some(search),
                }
            }
            KeyCode::Tab => {
                if let Some(hit) = search.hit {
                    self.set_input(hit);
                }
            }
            // Esc = cancel, like every other layer (it used to ADOPT the hit —
            // the only place in the app where Esc committed something).
            KeyCode::Esc => {}
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
        // The bottom notice expires with the window it advertises.
        if let Some(until) = self.notice_until
            && std::time::Instant::now() >= until
        {
            self.notice = None;
            self.notice_until = None;
            self.dirty = true;
        }
        // Slash transient hints expire (operation confirmations leave no permanent placeholder);
        // error/usage rows live longer (G12) — they additionally clear on the next input.
        if let Some(at) = self.slash_at
            && at.elapsed() > SLASH_OUTPUT_TTL
        {
            self.slash_lines.clear();
            self.slash_at = None;
            self.dirty = true;
        }
        if let Some(at) = self.slash_error_at
            && at.elapsed() > SLASH_OUTPUT_ERROR_TTL
        {
            self.slash_error_lines.clear();
            self.slash_error_at = None;
            self.dirty = true;
        }
        for msg in &mut self.messages {
            for act in &mut msg.activities {
                if let ActivityKind::Thinking(t) = &mut act.kind
                    && t.state == ThinkingState::Running
                {
                    t.duration_ms = self.tick.saturating_sub(t.start_tick).saturating_mul(33);
                }
            }
        }
    }

    /// Frame number within the update-banner breathing window (animation running → Some; no banner / motion off /
    /// stopped by a keypress / window passed → None, resting). The 270-frame window = 9s = 3 breaths.
    fn update_banner_frame(&self) -> Option<u64> {
        if self.update_banner.is_none() || self.motion_off || self.update_banner_stopped {
            return None;
        }
        let frame = self.tick.saturating_sub(self.update_banner_start);
        (frame < UPDATE_BANNER_FRAMES).then_some(frame)
    }

    /// Whether the update-banner breathing is active (the frame loop keeps dirty set; outside the window it returns to idle).
    fn update_anim_active(&self) -> bool {
        self.update_banner_frame().is_some()
    }

    /// Whether any row changes with the tick (spinner frames / elapsed time / status rows).
    /// false when idle — the tick neither rebuilds the doc nor wakes the component.
    pub fn has_dynamic_rows(&self) -> bool {
        self.busy
            || self.messages.iter().any(|m| {
                m.groups.iter().any(|g| g.active) || m.activities.iter().any(|a| a.is_running())
            })
            || (self.tasks_visible
                && self
                    .tasks_cache
                    .iter()
                    .any(|t| t.status == TodoStatus::InProgress))
            || self.update_anim_active()
    }

    /// Whether the host's tick loop has work to do. Returns false when idle so the host skips the whole frame —
    /// with no animation and no pending events, not a single byte is written.
    pub fn needs_tick(&self) -> bool {
        self.has_dynamic_rows()
            || self.slash_at.is_some()
            || self.slash_error_at.is_some()
            || self.notice_until.is_some()
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
            && self
                .tasks_cache
                .iter()
                .all(|t| t.status == TodoStatus::Done)
        {
            self.tasks_visible = false;
            self.tasks_auto = false;
            let total = self.tasks_cache.len();
            self.push_slash_output(format!("✓ {total}/{total} tasks done · ctrl+t to view"));
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
        let active: Vec<&TodoItem> = t.iter().filter(|i| i.status != TodoStatus::Done).collect();
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
                    ActivityKind::Watch(w) if w.status == WatchState::Running => {
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

    pub fn token_rate_label(&self) -> Option<String> {
        if !self.busy {
            return None;
        }
        self.token_rate
            .label(std::time::Instant::now(), self.motion_off)
    }

    pub fn context_usage(&self) -> crate::context_usage::ContextUsage {
        self.context_usage
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
        fresh.extend(
            self.session
                .channels
                .list()
                .into_iter()
                .map(|c| EntityRow::Channel {
                    name: c.name,
                    seq: c.seq,
                    frozen: c.frozen,
                }),
        );
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
            EntityRow::Channel { name, seq, frozen } => {
                format!("◇ #{name}({seq}{})", if *frozen { "❄" } else { "" })
            }
        };
        let Some(selected) = self.entity_focus else {
            let summary = self
                .entities
                .iter()
                .map(brief)
                .collect::<Vec<_>>()
                .join(" · ");
            return vec![Line::styled(
                one_line(&format!("  {summary} — ctrl+g to view"), width),
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
                    "{} #{name} · {seq} msgs{}",
                    glyph(e),
                    if *frozen { " · frozen" } else { "" }
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
                format!("  … {} total", self.entities.len()),
                SegStyle::fg(self.theme.inactive),
            ));
        }
        rows.push(Line::styled(
            "  ↑↓ select · enter opens · esc closes".to_string(),
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
                self.notice = Some(
                    "no subagent instances or channels yet (they appear after the Agent tool spawns one)",
                );
                self.notice_until = Some(std::time::Instant::now() + CTRL_C_WINDOW);
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
                self.entity_focus = Some((i + 1).min(self.entities.len().saturating_sub(1)));
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
            .map(|item| format!("> {}", one_line(&item.text, self.width.saturating_sub(4))))
            .collect();
        if self.queued.len() > QUEUE_ROWS_MAX {
            out.push(format!(
                "… +{} more queued",
                self.queued.len() - QUEUE_ROWS_MAX
            ));
        }
        out
    }

    /// ctrl+r search hint line (`(reverse-i-search)`query': hit`).
    pub fn search_line(&self) -> Option<String> {
        let search = self.search.as_ref()?;
        let (prefix, hit) = match search.hit.as_deref() {
            Some(hit) => ("(reverse-i-search)", hit),
            // bash shows failure explicitly; silence read as "found nothing? or broken?".
            None if !search.query.is_empty() => ("(failed reverse-i-search)", ""),
            None => ("(reverse-i-search)", ""),
        };
        Some(one_line(
            &format!(
                "{prefix}`{}': {hit}   — enter submits · tab accepts · ctrl+r older · esc cancels",
                search.query
            ),
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
        !m.groups.iter().any(|g| g.active) && !m.activities.iter().any(|a| a.is_running())
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
    #[cfg_attr(not(test), allow(dead_code))]
    fn message_settled(&self, i: usize) -> bool {
        (i == 0 || self.message_settled(i - 1)) && self.message_static_settled(i)
    }

    /// Build the scrolling document: welcome card + messages (text and activities interleaved at their insert points) +
    /// permission-request blocks. The block list is laid out by [`crate::tui::statics::layout`]:
    /// `doc.settled` / checkpoints = the settled prefix (welcome card + all settled messages;
    /// permission-request blocks are never settled).
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
        let theme = self.theme.clone();
        // Segment numbering: 0 = welcome card, i+1 = messages[i]. The clamp is defensive: if the message set
        // is replaced wholesale (/clear, /resume) without the cursor resetting, better to re-render
        // than leave a blank screen.
        let skip = self.flushed_segments.min(self.messages.len() + 1);
        self.tail_start = 0;
        self.mark_base = 0;

        // Prefix-monotone settlement, precomputed in one pass (recursing per
        // message inside the loop would be quadratic on the hot path).
        let mut settled_flags = Vec::with_capacity(self.messages.len());
        let mut prefix_settled = true;
        for i in 0..self.messages.len() {
            prefix_settled = prefix_settled && self.message_static_settled(i);
            settled_flags.push(prefix_settled);
        }

        let mut blocks: Vec<Block> = Vec::new();
        if skip == 0 {
            blocks.push(Block::settled(self.welcome_el(width, &theme), true));
        }
        let pal = crate::tui::slack::Palette::new(&theme);
        for (i, &settled) in settled_flags
            .iter()
            .enumerate()
            .skip(skip.saturating_sub(1))
        {
            let role = self.messages[i].role;
            // The band is the experimental face (`experimental.chatAvatars`): switched
            // off, a message opens on its body, exactly as it did before D50.
            let band = self.chat_avatars.then(|| self.sender_band_el(role, &pal));
            let body = match role {
                Role::User => El::Rows(user_message_rows(&self.messages[i].text, width, &theme)),
                Role::Assistant => self.assistant_el(i, width, &theme, settled, &pal),
            };
            // Message block spacing (CC marginTop=1): one blank row after the welcome card and before each message.
            let mut stack = vec![El::Blank];
            stack.extend(band);
            stack.push(body);
            blocks.push(Block::settled(El::col(stack), settled));
        }
        if let Some(ask) = self.ask_el(&theme) {
            blocks.push(Block::live(ask));
        }
        // Slash command output (/help /status /compact etc.): transient hints — rendered after messages and
        // above the input, **never settled or flushed**, auto-dismissed after the tick timeout (SLASH_OUTPUT_TTL).
        if !self.slash_lines.is_empty() {
            blocks.push(Block::transient(El::Lines(
                self.slash_lines
                    .iter()
                    .map(|line| Line::styled(one_line(line, width), SegStyle::fg(theme.text)))
                    .collect(),
            )));
        }
        // Error/usage rows (G12/G13): longer TTL, error color, clear on the next input.
        if !self.slash_error_lines.is_empty() {
            blocks.push(Block::transient(El::Lines(
                self.slash_error_lines
                    .iter()
                    .map(|line| Line::styled(one_line(line, width), SegStyle::fg(theme.error)))
                    .collect(),
            )));
        }
        // Informational output (/help /status …): persists until the next
        // input/Esc; never settles into scrollback.
        if !self.slash_info_lines.is_empty() {
            blocks.push(Block::transient(El::Lines(
                self.slash_info_lines
                    .iter()
                    .map(|line| Line::styled(one_line(line, width), SegStyle::fg(theme.text)))
                    .collect(),
            )));
        }

        self.doc = crate::tui::statics::layout(blocks);
        &self.doc
    }

    /// Welcome-card block. It settles at birth but stays in the live doc
    /// (banner breathing, re-wrap on resize) until it crosses the window top.
    fn welcome_el(&self, width: usize, theme: &Theme) -> El {
        // New-version banner (update-banner): breathing color inside the window; outside / no banner → resting rest or None.
        let banner = self.update_banner.as_deref().map(|v| {
            let frame = self.update_banner_frame().unwrap_or(UPDATE_BANNER_FRAMES);
            (v, update_color(theme, frame, self.motion_off))
        });
        let provider = self.session.runtime.provider.borrow().clone();
        El::Rows(welcome_card_rows(
            theme,
            &self.session.runtime.model.borrow(),
            self.permission_mode_label(),
            &self.cwd,
            width,
            banner,
            !self.session.client.is_configured(&provider),
        ))
    }

    /// The band above a message: who is speaking, as a portrait and a name.
    ///
    /// The names are the room's own — `main` for the hub, and the human's own
    /// messages read `You` exactly as the workspace already writes them
    /// ([`crate::tui::slack::message_rows`]). So the name on the band is the name
    /// that addresses the speaker, and the two views agree without a display-name
    /// table to keep honest in both.
    ///
    /// Neither speaker is a blueprint member, so both faces come from the same
    /// name hash the workspace falls back to — pinning is for the crew.
    fn sender_band_el(&mut self, role: Role, pal: &crate::tui::slack::Palette) -> El {
        let (name, shown) = match role {
            Role::User => (crate::channels::USER_NAME, "You"),
            Role::Assistant => (crate::channels::HUB_NAME, crate::channels::HUB_NAME),
        };
        let index = crate::tui::avatar::index_of(name);
        self.faces.insert(index);
        El::Rows(
            crate::tui::slack::sender_band(index, name, shown, self.image_cap.is_some(), pal)
                .into_iter()
                .map(Row::new)
                .collect(),
        )
    }

    /// Assistant message: markdown text and activities interleaved in model
    /// output order; collapse groups fold runs of read/search tools. `settled`
    /// mirrors the old `message_settled(i)` (prefix-monotone flag).
    /// The portrait each of this message's activities wears, resolved in one pass
    /// before the rows are built (the row loop holds a read borrow of `messages`,
    /// and recording a face needs a write).
    ///
    /// Only a subagent watch row gets one, and only where the terminal can place
    /// images: the face is what buys the `⎿` connector's place, so a chip skin —
    /// which has no face to spend — keeps `◉` and the connector exactly as before.
    /// With `experimental.chatAvatars` off the transcript wears no faces at all,
    /// which lands in the same place as a terminal that cannot draw them.
    fn watch_portraits(
        &mut self,
        i: usize,
        pal: &crate::tui::slack::Palette,
    ) -> Vec<Option<Portrait>> {
        if !self.chat_avatars || self.image_cap.is_none() {
            return Vec::new();
        }
        let named: Vec<Option<String>> = self.messages[i]
            .activities
            .iter()
            .map(|act| match &act.kind {
                ActivityKind::Watch(w) if w.kind == crate::watch::WatchKind::Agent => {
                    // `{instance} · {description}` — the address is the prefix.
                    let name = w.label.split(" · ").next().unwrap_or_default().trim();
                    (!name.is_empty()).then(|| name.to_string())
                }
                _ => None,
            })
            .collect();
        named
            .into_iter()
            .map(|name| {
                let name = name?;
                let index = self
                    .faces_pinned
                    .get(&name)
                    .copied()
                    .unwrap_or_else(|| avatar::index_of(&name));
                self.faces.insert(index);
                Some(Portrait {
                    top: crate::tui::slack::gutter_cell(index, &name, 0, true, pal),
                    bottom: crate::tui::slack::gutter_cell(index, &name, 1, true, pal),
                })
            })
            .collect()
    }

    fn assistant_el(
        &mut self,
        i: usize,
        width: usize,
        theme: &Theme,
        settled: bool,
        pal: &crate::tui::slack::Palette,
    ) -> El {
        let portraits = self.watch_portraits(i, pal);
        // Thinking completion row (CC SystemTextMessage `✻ Churned for 40s`):
        // rendered at the end of the message (after text and all tools), from the last completed
        // real thinking block (empty placeholder blocks produce no completion row).
        // Only rendered after the turn ends: while running, `✻ Baked for 0.4s` would appear
        // while tools are still running, contradicting the bottom running-status row.
        let show_done_line = i == self.messages.len() - 1 && self.stream_msg.is_none() || settled;
        // Markdown render closure: borrows only disjoint fields to avoid conflicting with
        // the shared read borrow of `self.messages`.
        let mut render = {
            let processor = &mut self.processor;
            let renderer = &mut self.renderer;
            let cache = &mut self.reply_cache;
            let images = &self.images;
            let images_failed = &self.images_failed;
            let image_cap = self.image_cap;
            let images_version = self.images_version;
            move |reply: &str| -> Vec<Line> {
                if reply.is_empty() {
                    return Vec::new();
                }
                if let Some(lines) = cache.get(reply) {
                    return lines.clone();
                }
                renderer.set_width(width.saturating_sub(2));
                // Image cache version changed → sync the renderer (clears its per-block cache).
                if renderer.images_version() != images_version {
                    renderer.set_images(image_cap, images, images_failed, images_version);
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
        let mut parts: Vec<El> = Vec::new();
        for (idx, act) in msg.activities.iter().enumerate() {
            let pos_chars = msg
                .insert_points
                .get(idx)
                .copied()
                .unwrap_or(rendered_chars)
                .min(text.chars().count());
            if pos_chars > rendered_chars {
                let seg_end = char_bounds.get(pos_chars).copied().unwrap_or(text.len());
                let reply = render(&text[rendered_bytes..seg_end]);
                parts.push(text_el(theme, reply));
                rendered_chars = pos_chars;
                rendered_bytes = seg_end;
            }
            let group_idx = msg.group_of.get(idx).copied().flatten();
            let group_collapsed = group_idx.is_some_and(|g| !msg.groups[g].expanded);
            let is_group_head =
                group_idx.is_some_and(|g| msg.groups[g].activities.first() == Some(&idx));
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
                // A failure inside the fold is otherwise invisible: the summary counts the call
                // as if it had worked, and only ctrl+o shows the error row. Say so on the
                // summary line — it matters most for the calls that change something.
                let failed = msg.groups[g]
                    .activities
                    .iter()
                    .filter(|&&ai| {
                        matches!(
                            msg.activities.get(ai),
                            Some(a) if matches!(
                                &a.kind,
                                ActivityKind::Tool(t) if t.status == ToolStatus::Error
                            )
                        )
                    })
                    .count();
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
                if failed > 0 {
                    line.push_styled(format!(" · {failed} failed"), SegStyle::fg(theme.error));
                }
                line.push_styled(
                    " (ctrl+o to expand)".to_string(),
                    SegStyle::fg(theme.inactive),
                );
                parts.push(El::click(
                    ClickTarget::Group {
                        message: i,
                        group: g,
                    },
                    El::Line(line),
                ));
                // Below a running collapse group, show the most recent tool's input (the CC ⎿ row).
                // The hint may be a multi-line bash command: single-line it and truncate by width,
                // otherwise the row balloons into multiple lines and the row model drifts from the canvas.
                // It sits outside the Click wrapper — only the summary row toggles.
                if in_progress && let Some(hint) = &msg.groups[g].last_hint {
                    parts.push(El::Line(Line::styled(
                        one_line(&format!("  ⎿  {hint}"), width),
                        SegStyle::fg(theme.inactive),
                    )));
                }
                continue;
            }
            let (lines, local) = layout_activity(
                act,
                &[idx],
                0,
                theme,
                portraits.get(idx).and_then(|p| p.as_ref()),
                &mut |reply: &str| render(reply),
            );
            let activity = El::Annotated {
                rows: lines.into_iter().map(Row::new).collect(),
                clicks: local
                    .into_iter()
                    .map(|range| LocalClick {
                        start: range.start as usize,
                        end: range.end as usize,
                        target: ClickTarget::Activity {
                            message: i,
                            path: range.path,
                        },
                    })
                    .collect(),
            };
            // Expanded group: the group-head tool row doubles as the group summary row — the
            // enclosing Click is emitted first, so clicking it collapses the group back.
            parts.push(if let Some(g) = group_idx {
                El::click(
                    ClickTarget::Group {
                        message: i,
                        group: g,
                    },
                    activity,
                )
            } else {
                activity
            });
        }
        if rendered_bytes < text.len() {
            let reply = render(&text[rendered_bytes..]);
            parts.push(text_el(theme, reply));
        }
        if show_done_line
            && let Some(line) =
                self.messages[i]
                    .activities
                    .iter()
                    .rev()
                    .find_map(|a| match &a.kind {
                        ActivityKind::Thinking(t)
                            if t.state == ThinkingState::Done && !a.content.is_empty() =>
                        {
                            Some(crate::tui::activities::thinking_completion_line(t, theme))
                        }
                        _ => None,
                    })
        {
            parts.push(El::Line(line));
        }
        El::Col(parts)
    }

    /// Permission/ask block (PermissionDialog / AskUserQuestion):
    /// title (permission bold) + description (dim) + numbered options (Select:
    /// `❯ n. label` focus marker, desc sub-row dim, Other free input) + shortcut hints.
    fn ask_el(&self, theme: &Theme) -> Option<El> {
        let (request, _) = self.pending_ask.as_ref()?;
        let mut parts: Vec<El> = Vec::new();
        let mut title = Line::styled("⏺ ", SegStyle::fg(theme.text));
        title.push_styled(request.title.clone(), theme.permission());
        parts.push(El::Line(title));
        parts.push(El::Line(Line::styled(
            format!("  {}", request.question),
            SegStyle::fg(theme.text),
        )));
        // CC Select: one blank row between the question and the options.
        parts.push(El::Blank);
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
            // Only the option row itself confirms; the description sub-row stays inert.
            parts.push(El::click(ClickTarget::AskOption(opt_idx), El::Line(line)));
            if let Some(desc) = request
                .descriptions
                .get(opt_idx)
                .and_then(|d| d.as_deref())
                .filter(|d| !d.is_empty())
            {
                parts.push(El::Line(Line::styled(
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
            parts.push(El::click(ClickTarget::AskOption(other_idx), El::Line(line)));
            let placeholder = if focused {
                if self.ask_other.is_empty() {
                    "Type something.".to_string()
                } else {
                    format!("{}{}", self.ask_other, '▋')
                }
            } else {
                "Type something.".to_string()
            };
            parts.push(El::Line(Line::styled(
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
        parts.push(El::Line(Line::styled(
            format!("  {hint}"),
            SegStyle::fg(theme.inactive),
        )));
        Some(El::Col(parts))
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

/// Text segment: the first reply line carries the `⏺ ` marker (CC assistant
/// reply prefix); the rest map one line per row.
fn text_el(theme: &Theme, reply: Vec<Line>) -> El {
    let claude = theme.claude;
    El::Rows(
        reply
            .into_iter()
            .enumerate()
            .map(|(j, line)| {
                if j == 0 {
                    let mut styled = Line::styled("⏺ ", SegStyle::fg(claude));
                    styled.image = line.image.clone();
                    styled.segs.extend(line.segs);
                    Row::new(styled)
                } else {
                    Row::new(line)
                }
            })
            .collect(),
    )
}

/// Welcome card body (CC WelcomeBox): a starred greeting, the two commands
/// worth knowing, the cwd, and a dim identity line. `bingo` stays `bingo` —
/// this is homage, not impersonation.
///
/// The new-version banner row (update-banner spec v1.1): sits directly above the version-identity row, one blank
/// row from cwd; three segments (static inactive + version/command in breathing color, command bold),
/// breathing only affects the banner's two keyword segments; every other welcome-card element stays static.
fn welcome_rows(
    theme: &Theme,
    model: &str,
    mode: &str,
    cwd: &str,
    width: usize,
    banner: Option<(&str, Color)>,
    unconfigured: bool,
) -> Vec<Line> {
    let mut rows = Vec::new();
    let mut greeting = Line::styled(" ✻ ", SegStyle::fg(theme.claude));
    greeting.push_styled("Welcome back!", theme.text());
    rows.push(greeting);
    rows.push(Line::empty());
    rows.push(Line::styled(
        one_line("   /help for help · /status for your current setup", width),
        theme.dim(),
    ));
    rows.push(Line::empty());
    rows.push(Line::styled(
        one_line(&format!("   cwd: {cwd}"), width),
        theme.dim(),
    ));
    // Onboarding: with no usable credentials, the card says what to do next —
    // the login command lives in here, so the door must open before the key.
    if unconfigured {
        rows.push(Line::empty());
        rows.push(Line::styled(
            one_line(
                "   ⚠ no credentials configured: /provider login codex (ChatGPT subscription) or write apiKey in ~/.config/bingo/settings.json",
                width,
            ),
            SegStyle::fg(theme.warning),
        ));
    }
    // New-version banner row (update-banner spec §1.1): directly above the version-identity row, one blank row from cwd.
    if let Some((v, color)) = banner
        && let Some((pre, ver, mid, cmd)) = banner_segments(v, width)
    {
        rows.push(Line::empty());
        let no_color = std::env::var_os("NO_COLOR").is_some();
        let mut line = Line::styled(&pre, theme.dim());
        if no_color {
            // Monochrome / NO_COLOR fallback: a static bold row (spec §2.5).
            line.push_styled(ver, theme.dim());
            line.push_styled(mid, theme.dim());
            line.push_styled(cmd, theme.dim().bold());
        } else {
            line.push_styled(ver, SegStyle::fg(color));
            line.push_styled(mid, theme.dim());
            line.push_styled(cmd, SegStyle::fg(color).bold());
        }
        rows.push(line);
    }
    rows.push(Line::styled(
        one_line(
            &format!("   bingo v{} · {model} · {mode}", env!("CARGO_PKG_VERSION")),
            width,
        ),
        theme.dim(),
    ));
    rows
}

/// Welcome card rows (with the ╭╮ border), part of the scrollable content.
/// `banner` = the new-version hint (version + current breathing color); None = no banner row.
fn welcome_card_rows(
    theme: &Theme,
    model: &str,
    mode: &str,
    cwd: &str,
    width: usize,
    banner: Option<(&str, Color)>,
    unconfigured: bool,
) -> Vec<Row> {
    let gray = SegStyle::fg(theme.inactive);
    let inner_w = width.saturating_sub(2);
    let mut rows = vec![Row::new(Line::styled(
        format!("╭{}╮", "─".repeat(inner_w)),
        gray,
    ))];
    for line in welcome_rows(theme, model, mode, cwd, inner_w, banner, unconfigured) {
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

/// Update-banner breathing window: 270 frames = 9s (3 breaths; each cycle is 90 frames = 3.0s @30fps).
/// After the window it rests at the rest color and the banner stays (update-banner spec §2.3).
pub const UPDATE_BANNER_FRAMES: u64 = 270;
/// Breathing cycle in frames (one "in + out" every 3.0s at 30fps).
pub const UPDATE_BANNER_PERIOD: u64 = 90;

/// Banner truncation chain (update-banner spec §1.3, pure and testable): returns
/// (pre, ver, mid, cmd) — the static segment and the two breathing segments are separate so the render layer can color them.
///
/// | inner width | shown as |
/// |---|---|
/// | ≥50 (or the full line fits) | `   New version vX.Y.Z available — run bingo update` |
/// | ≥43 | `   New version vX.Y.Z — run bingo update` |
/// | ≥15 | `   bingo update` (the command alone, the minimal action entry) |
/// | <15 | None (banner hidden) |
///
/// At every tier `bingo update` stays visible, unwrapped, and inside the card.
pub fn banner_segments(v: &str, width: usize) -> Option<(String, String, String, String)> {
    const PRE: &str = "   New version ";
    const MID_FULL: &str = " available — run ";
    const MID_SHORT: &str = " — run ";
    const CMD: &str = "bingo update";
    let ver = format!("v{v}");
    let full_len = text_width(PRE) + text_width(&ver) + text_width(MID_FULL) + text_width(CMD);
    if width >= 50 || full_len <= width {
        return Some((PRE.to_string(), ver, MID_FULL.to_string(), CMD.to_string()));
    }
    if width >= 43 {
        return Some((PRE.to_string(), ver, MID_SHORT.to_string(), CMD.to_string()));
    }
    if width >= 15 {
        return Some((
            String::new(),
            String::new(),
            "   ".to_string(),
            CMD.to_string(),
        ));
    }
    None
}

/// The banner's full text (the string form of `banner_segments`; the pure function the spec names, used by test assertions).
#[cfg_attr(not(test), allow(dead_code))]
pub fn banner_line(v: &str, width: usize) -> Option<String> {
    banner_segments(v, width).map(|(pre, ver, mid, cmd)| format!("{pre}{ver}{mid}{cmd}"))
}

/// Update-banner breathing colors (update-banner spec §2, pure and testable):
/// - `motion_off` (settings `motion:"off"` or `BINGO_NO_MOTION`) → always rest (static, banner kept);
/// - no truecolor (theme downgraded to 256 colors) → discrete two-step: 60-frame cycle, peak for the first 12 frames (≥400ms);
/// - truecolor → sine breathing: `t = 0.5 − 0.5·cos(2π·phase/90)`, frame 0 = rest (trough), 45 = peak,
///   90 = back to rest; linear interpolation per sRGB channel.
///
/// Stops: dark `#D77757 ↔ #E8896B` (≥6.24:1 throughout); light `#B05227 ↔ #9A4A24` (≥4.72:1 throughout).
pub fn update_color(theme: &Theme, frame: u64, motion_off: bool) -> Color {
    let rest = if theme.is_dark {
        theme.claude
    } else {
        theme.claude_deep
    };
    if motion_off {
        return rest;
    }
    let peak = if theme.is_dark {
        theme.claude_strong
    } else {
        theme.claude_deep_strong
    };
    if !matches!(theme.claude_strong, Color::Rgb(..)) {
        // Discrete two-step (256-color terminal): 60-frame cycle, peak 400ms (12 frames) → rest 1600ms.
        return if frame % 60 < 12 { peak } else { rest };
    }
    let phase = (frame % UPDATE_BANNER_PERIOD) as f64;
    let t = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * phase / UPDATE_BANNER_PERIOD as f64).cos();
    lerp_color(rest, peak, t)
}

/// Per-channel sRGB linear interpolation (the two stops are close; no gamma correction — the spec notes it is out of scope).
fn lerp_color(a: Color, b: Color, t: f64) -> Color {
    let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (a, b) else {
        return b;
    };
    let l = |x: u8, y: u8| {
        (x as f64 + (y as f64 - x as f64) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color::Rgb(l(ar, br), l(ag, bg), l(ab, bb))
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

    /// Joined text of every slash output bucket (confirm + error + info).
    fn all_slash_text(chat: &Chat) -> String {
        chat.slash_lines
            .iter()
            .chain(&chat.slash_error_lines)
            .chain(&chat.slash_info_lines)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Segments covered by the latest settled checkpoint (checkpoint-equivalent read of the old aggregate field).
    fn settled_segments(chat: &Chat) -> usize {
        chat.doc.settled_marks.last().map_or(0, |m| m.segments)
    }

    /// A Chat with its own home (the unique dir for slash tests, so transcript/task storage is not
    /// shared with other tests). cwd points at the same home: the persistence paths of /model /think /theme etc.
    /// write into `{cwd}/.bingo` and must never pollute the repo's real config.
    fn test_chat_home(home: std::path::PathBuf) -> Chat {
        let _ = std::fs::create_dir_all(&home);
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
            cwd: Arc::new(std::sync::Mutex::new(home.clone())),
            home: home.clone(),
            // Hermetic: scoped writes must never touch the real user config.
            user_config_dir: home.join(".config"),
            quiet: true,
            compact_failures: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&home, "test")),
            expand_tasks: tokio::sync::watch::channel(false).0,
            agents: crate::agents::AgentRegistry::new(),
            channels: crate::channels::ChannelRegistry::new(Default::default()),
            instance: None,
            attachments: crate::api::image::Attachments::new(),
        });
        Chat::new(
            session,
            events_tx,
            events_rx,
            asks_tx,
            asks_rx,
            Theme::dark(),
            crate::tui::theme::ThemeSetting::Auto,
            None,
        )
    }

    #[test]
    fn slash_cd_updates_session_and_tool_context_cwd() {
        let root = std::env::temp_dir().join(format!(
            "bingo-slash-cd-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let start = root.join("start");
        let target = root.join("target");
        std::fs::create_dir_all(&start).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let target = std::fs::canonicalize(target).unwrap();
        let mut chat = test_chat_home(start.clone());

        assert_eq!(chat.session.cwd(), start);
        assert!(chat.run_slash(&format!("cd {}", target.display())));
        assert_eq!(chat.session.cwd(), target);
        assert_eq!(chat.cwd, target.display().to_string());
        let ctx =
            crate::query::tool_context(&chat.session, &crate::query::headless_hooks()).unwrap();
        assert_eq!(ctx.cwd, target);
        assert!(all_slash_text(&chat).contains("✓ working directory:"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn slash_cd_rejects_missing_directory_without_changing_cwd() {
        let root = std::env::temp_dir().join(format!(
            "bingo-slash-cd-missing-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut chat = test_chat_home(root.clone());

        assert!(chat.run_slash("cd missing"));
        assert_eq!(chat.session.cwd(), root);
        assert!(all_slash_text(&chat).contains("code=BAD_ARGUMENT"));

        std::fs::remove_dir_all(root).unwrap();
    }

    /// Banner truncation chain (update-banner spec §1.3): full / drop the available clause / command only / hidden.
    #[test]
    fn banner_segments_width_tiers() {
        let full = banner_segments("0.3.0", 50).unwrap();
        assert_eq!(full.0, "   New version ");
        assert_eq!(full.1, "v0.3.0");
        assert_eq!(full.2, " available — run ");
        assert_eq!(full.3, "bingo update");
        // The full line fits (width <50 but full_len ≤ width) → still full
        assert_eq!(banner_segments("0.3.0", 51).unwrap().2, " available — run ");
        // 43-49: the longest version v0.12.34 does not fit whole → drop the available clause
        let short = banner_segments("0.12.34", 49).unwrap();
        assert_eq!(short.2, " — run ");
        assert_eq!(short.3, "bingo update");
        // ≥15: command only
        let cmd_only = banner_segments("0.3.0", 15).unwrap();
        assert!(cmd_only.0.is_empty() && cmd_only.1.is_empty());
        assert_eq!(cmd_only.3, "bingo update");
        // <15: hidden
        assert!(banner_segments("0.3.0", 14).is_none());
    }

    #[test]
    fn banner_line_width_tiers() {
        assert_eq!(
            banner_line("0.3.0", 50).unwrap(),
            "   New version v0.3.0 available — run bingo update"
        );
        assert_eq!(
            banner_line("0.12.34", 49).unwrap(),
            "   New version v0.12.34 — run bingo update"
        );
        assert_eq!(banner_line("0.3.0", 15).unwrap(), "   bingo update");
        assert!(banner_line("0.3.0", 14).is_none());
    }

    /// Breathing-color pure function (update-banner spec §2.3/anchor 2): phase 0 = rest (trough), 45 = peak,
    /// 90 = back to rest; 0→45 strictly rises, 45→90 strictly falls; motion off → always rest; light theme takes the deep-orange stops.
    #[test]
    fn update_color_breathing_wave() {
        let dark = Theme::dark();
        let rest = Color::Rgb(215, 119, 87);
        let peak = Color::Rgb(232, 137, 107);
        assert_eq!(
            update_color(&dark, 0, false),
            rest,
            "starts at the trough (no jump)"
        );
        assert_eq!(
            update_color(&dark, 45, false),
            peak,
            "phase 45 = peak (exactly t=1)"
        );
        assert_eq!(update_color(&dark, 90, false), rest);
        assert_eq!(update_color(&dark, 135, false), peak, "cycle wraps around");
        assert_eq!(update_color(&dark, 180, false), rest);
        // Monotonicity (red channel)
        let r = |f: u64| -> u8 {
            match update_color(&dark, f, false) {
                Color::Rgb(r, _, _) => r,
                _ => panic!("a truecolor theme should return Rgb"),
            }
        };
        assert!(
            r(15) > r(0) && r(30) > r(15) && r(45) > r(30),
            "0→45 strictly rises"
        );
        assert!(
            r(60) < r(45) && r(75) < r(60) && r(90) < r(75),
            "45→90 strictly falls"
        );
        // motion off → always rest (the indicator stays, it just stops)
        assert_eq!(update_color(&dark, 45, true), rest);
        assert_eq!(update_color(&dark, 999, true), rest);
        // Light theme → deep-orange stops (rest #B05227 / peak #9A4A24); the bright orange is disabled
        let light = Theme::light();
        assert_eq!(update_color(&light, 0, false), Color::Rgb(176, 82, 39));
        assert_eq!(update_color(&light, 45, false), Color::Rgb(154, 74, 36));
        for f in [0u64, 22, 45, 67, 90] {
            assert_ne!(
                update_color(&light, f, false),
                Color::Rgb(215, 119, 87),
                "the light theme must not use the bright-orange stops (spec §2.2)"
            );
        }
    }

    /// 256-color discrete two-step (update-banner spec §2.4): 60-frame cycle, peak 400ms (12 frames) → rest.
    /// (`downgrade_to_256` is a private theme.rs method; the test hand-builds an Indexed theme to simulate the downgrade.)
    #[test]
    fn update_color_256_discrete_two_step() {
        let mut d256 = Theme::dark();
        d256.claude = Color::Indexed(167); // dark rest approximation
        d256.claude_strong = Color::Indexed(173); // dark peak approximation
        let f0 = update_color(&d256, 0, false);
        let f11 = update_color(&d256, 11, false);
        let f12 = update_color(&d256, 12, false);
        let f59 = update_color(&d256, 59, false);
        let f60 = update_color(&d256, 60, false);
        assert_eq!(f0, f11, "peak phase is contiguous (frames 0-11)");
        assert_eq!(f12, f59, "rest phase is contiguous (frames 12-59)");
        assert_ne!(f0, f12, "the two stops differ");
        assert_eq!(f60, f0, "the 60-frame cycle wraps");
        assert!(
            matches!(update_color(&d256, 5, false), Color::Indexed(_)),
            "the 256-color downgrade outputs Indexed"
        );
    }

    /// Welcome-card rendering (update-banner anchors 1/6/9): with a banner row (including the two-segment form); the no-banner layout regression is unchanged;
    /// narrow screens keep only `bingo update`.
    #[test]
    fn welcome_card_banner_rendering() {
        let theme = Theme::dark();
        let color = Color::Rgb(215, 119, 87);
        let with = welcome_card_rows(&theme, "m", "d", "/cwd", 80, Some(("0.3.0", color)), false);
        let texts: Vec<String> = with.iter().map(|r| r.line.plain_text()).collect();
        assert!(
            texts
                .iter()
                .any(|t| t.contains("New version v0.3.0 available — run bingo update")),
            "the full-tier banner must fit inside the card: {texts:?}"
        );
        // The banner sits directly above the version-identity row (adjacent, no blank line between)
        let banner_idx = texts
            .iter()
            .position(|t| t.contains("New version"))
            .unwrap();
        assert!(
            texts[banner_idx + 1].contains("bingo v"),
            "the banner must sit right below the identity row"
        );
        // No banner → the layout matches the current one (regression)
        let without = welcome_card_rows(&theme, "m", "d", "/cwd", 80, None, false);
        assert_eq!(
            with.len(),
            without.len() + 2,
            "banner + the blank row above = 2 rows"
        );
        // Narrow (inner 15): command only; "New version" must not appear
        let narrow = welcome_card_rows(&theme, "m", "d", "/c", 17, Some(("0.3.0", color)), false);
        let narrow_texts: Vec<String> = narrow.iter().map(|r| r.line.plain_text()).collect();
        assert!(narrow_texts.iter().any(|t| t.contains("bingo update")));
        assert!(!narrow_texts.iter().any(|t| t.contains("New version")));
        // Very narrow (inner <15): the banner is hidden, layout matches the no-banner one
        let tiny = welcome_card_rows(&theme, "m", "d", "/c", 16, Some(("0.3.0", color)), false);
        assert_eq!(
            tiny.len(),
            without.len(),
            "the banner is hidden when inner<15"
        );
    }

    /// Breathing window (update-banner anchors 3/5): active throughout the 270 frames (has_dynamic_rows),
    /// idle outside the window; a keypress in the window → stops immediately; motion off → never active.
    #[test]
    fn update_banner_animation_window_and_key_stop() {
        let mut chat = test_chat();
        chat.update_banner = Some("0.3.0".into());
        chat.update_banner_start = 0;
        chat.tick = 0;
        assert!(chat.update_anim_active());
        assert!(
            chat.has_dynamic_rows(),
            "the frame loop keeps dirty set inside the breathing window"
        );
        chat.tick = 269;
        assert!(
            chat.update_anim_active(),
            "still active on the window's last frame"
        );
        chat.tick = 270;
        assert!(!chat.update_anim_active(), "resting outside the window");
        assert!(
            !chat.has_dynamic_rows(),
            "back to idle outside the window (zero writes)"
        );
        // A keypress in the window → stops immediately (P1)
        chat.update_banner_start = 0;
        chat.tick = 50;
        assert!(chat.update_anim_active());
        let _ = chat.on_key(KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(
            !chat.update_anim_active(),
            "the first keypress in the window stops it immediately"
        );
        // motion off → never active (the banner stays as a static rest)
        let mut chat2 = test_chat();
        chat2.update_banner = Some("0.3.0".into());
        chat2.motion_off = true;
        chat2.tick = 10;
        assert!(!chat2.update_anim_active());
        assert!(!chat2.has_dynamic_rows());
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
            let _ = chat.events.send(UiEvent::ToolStart {
                name: "Read".into(),
            });
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
            let _ = chat
                .events
                .send(UiEvent::ToolDone(crate::query::ToolCallDone {
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

    /// No transcript row is wider than the width it was built for. The reply
    /// marker is prepended *after* the markdown is wrapped, so rendering the text
    /// at the full width made every filled first line `width + 2`: the viewport
    /// clipped the overhang (two characters gone with no sign they existed) and
    /// scrollback would have wrapped it onto a second physical row, breaking the
    /// one-document-row-per-terminal-row invariant the whole write-once design
    /// rests on.
    #[test]
    fn no_row_overflows_the_build_width() {
        for width in [40usize, 80, 100] {
            let mut chat = test_chat();
            // Long enough that some line must fill the width exactly.
            let long = "lockfile pins rewritten alongside the version bump ".repeat(8);
            chat.messages.push(msg(Role::User, &long));
            chat.messages.push(msg(Role::Assistant, &long));
            chat.build_rows(width);
            for (i, row) in chat.doc.rows.iter().enumerate() {
                let w = text_width(&row.line.plain_text());
                assert!(
                    w <= width,
                    "row {i} is {w} wide at width {width}: {:?}",
                    row.line.plain_text()
                );
            }
        }
    }

    /// A subagent's watch row is the one place in the transcript with many named
    /// speakers, so it wears their faces — the portrait spans the header and the
    /// result row, which is the height the block already had. The `⎿` connector is
    /// what it costs, and only where a face is actually drawn: a chip terminal has
    /// nothing to spend, so it keeps `◉` and the connector untouched.
    #[test]
    fn agent_watch_rows_wear_the_instance_face_only_where_images_place() {
        let watch = |chat: &mut Chat| {
            chat.messages.push(msg(Role::Assistant, ""));
            chat.apply_event(UiEvent::WatchEvent {
                label: "林夏 · UI review".into(),
                kind: crate::watch::WatchKind::Agent,
                status: WatchState::Running,
                detail: Some("produced 200 chars".into()),
                duration_ms: 0,
                payload: None,
                signal: None,
            });
            chat.build_rows(80);
            chat.doc
                .rows
                .iter()
                .map(|r| r.line.plain_text())
                .collect::<Vec<_>>()
        };

        let mut chip = test_chat();
        chip.chat_avatars = true;
        let rows = watch(&mut chip);
        assert!(
            rows.iter().any(|r| r.contains("◉ 林夏 · UI review")),
            "chip terminals keep the glyph: {rows:?}"
        );
        assert!(
            rows.iter().any(|r| r.contains("⎿")),
            "and keep the connector: {rows:?}"
        );
        assert!(
            chip.faces.len() <= 2,
            "no portrait was claimed for a terminal that cannot draw one"
        );

        let mut placed = test_chat();
        placed.chat_avatars = true;
        placed.image_cap = Some(ImageCap::default_cells());
        let rows = watch(&mut placed);
        let header = rows
            .iter()
            .find(|r| r.contains("林夏 · UI review"))
            .unwrap_or_else(|| panic!("watch row present: {rows:?}"));
        assert!(
            header.contains(gfx::PLACEHOLDER) && !header.contains('◉'),
            "the face replaces the glyph: {header:?}"
        );
        assert!(
            placed.faces.contains(&crate::tui::avatar::index_of("林夏")),
            "the instance's face is recorded for transmission"
        );
    }

    /// The band names the speaker with the name that addresses it: the hub is
    /// `main` in the room and on the band, and the human's own messages read
    /// `You`, exactly as the workspace already writes them. Both faces are
    /// recorded — the transmit layer sends what the rows drew, so a portrait no
    /// row asked for is never sent and one a row used is never missed.
    #[test]
    fn sender_band_names_the_speaker_and_records_its_face() {
        let mut chat = test_chat();
        chat.chat_avatars = true;
        chat.messages.push(msg(Role::User, "hi"));
        chat.messages.push(msg(Role::Assistant, "hello"));
        chat.build_rows(80);
        let rows: Vec<String> = chat
            .doc
            .rows
            .iter()
            .map(|r| r.line.plain_text().trim().to_string())
            .collect();
        assert!(
            rows.iter().any(|r| r.ends_with("You")),
            "the human's own band reads You: {rows:?}"
        );
        assert!(
            rows.iter().any(|r| r.ends_with(crate::channels::HUB_NAME)),
            "the hub's band reads its room name: {rows:?}"
        );
        let expected: HashSet<usize> = [
            crate::tui::avatar::index_of(crate::channels::USER_NAME),
            crate::tui::avatar::index_of(crate::channels::HUB_NAME),
        ]
        .into_iter()
        .collect();
        assert_eq!(chat.faces, expected, "both faces recorded for transmission");
    }

    /// The two skins differ in height here and only here: the portrait needs a
    /// second row, the chip does not. Nothing below the band depends on it —
    /// unlike the workspace gutter, where unequal heights would shear the body.
    #[test]
    fn sender_band_costs_a_second_row_only_where_portraits_place() {
        let build = |cap: Option<ImageCap>| -> (usize, bool) {
            let mut chat = test_chat();
            chat.chat_avatars = true;
            chat.image_cap = cap;
            chat.messages.push(msg(Role::User, "hi"));
            chat.build_rows(80);
            let placed = chat
                .doc
                .rows
                .iter()
                .any(|r| r.line.plain_text().contains(gfx::PLACEHOLDER));
            (chat.doc.rows.len(), placed)
        };
        let (chip_rows, chip_placed) = build(None);
        let (portrait_rows, portrait_placed) = build(Some(ImageCap::default_cells()));
        assert_eq!(
            portrait_rows,
            chip_rows + 1,
            "the portrait's second row is the whole difference"
        );
        assert!(portrait_placed, "image terminals get placeholder cells");
        assert!(!chip_placed, "the chip skin places no image");
    }

    /// The switch is what the transcript's faces hang on, and it is off unless a
    /// settings layer asks for them: no band over a message, no portrait on a watch
    /// row, and nothing recorded for transmission — a face nobody drew must not be
    /// sent. The workspace views are not governed by this and are not touched here.
    #[test]
    fn without_the_switch_the_transcript_wears_no_face() {
        let mut chat = test_chat();
        assert!(!chat.chat_avatars, "off unless a settings layer asks");
        chat.image_cap = Some(ImageCap::default_cells());
        chat.messages.push(msg(Role::User, "hi"));
        chat.messages.push(msg(Role::Assistant, ""));
        chat.apply_event(UiEvent::WatchEvent {
            label: "林夏 · UI review".into(),
            kind: crate::watch::WatchKind::Agent,
            status: WatchState::Running,
            detail: Some("produced 200 chars".into()),
            duration_ms: 0,
            payload: None,
            signal: None,
        });
        chat.build_rows(80);
        let rows: Vec<String> = chat
            .doc
            .rows
            .iter()
            .map(|r| r.line.plain_text().trim().to_string())
            .collect();
        assert!(
            !rows.iter().any(|r| r.ends_with("You")
                || r.ends_with(crate::channels::HUB_NAME)
                || r.contains(gfx::PLACEHOLDER)),
            "no band and no portrait: {rows:?}"
        );
        assert!(
            rows.iter().any(|r| r.contains("◉ 林夏 · UI review")),
            "the watch row keeps its glyph: {rows:?}"
        );
        assert!(chat.faces.is_empty(), "nothing is claimed for transmission");
    }

    /// Task-family / AskUserQuestion calls are not shown in the transcript
    /// (renderToolUseMessage = null; the task panel / dialog shows them).
    #[test]
    fn hidden_tools_produce_no_activities() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        for name in [
            "TaskCreate",
            "TaskUpdate",
            "TaskGet",
            "TaskList",
            "AskUserQuestion",
        ] {
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
        assert!(
            chat.pending_tools.is_empty(),
            "the pending FIFO stays matched"
        );
        // Visible tools still render normally.
        let _ = chat.events.send(UiEvent::ToolStart {
            name: "Bash".into(),
        });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Bash".into(),
            input: json!({"command": "ls"}),
            standalone: false,
        });
        chat.drain_events();
        assert_eq!(
            chat.messages[0].activities.len(),
            1,
            "Bash renders normally"
        );
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
        assert!(
            chat.tasks_visible,
            "the auto panel shows when there are active items"
        );
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
        assert!(
            !chat.tasks_visible,
            "the auto panel hides when everything completes"
        );
        assert!(!chat.tasks_auto);
        assert!(chat.task_lines().is_empty());
        assert!(
            chat.slash_lines
                .iter()
                .any(|l| l.contains("✓ 1/1 tasks done")),
            "a transient row is pushed at the hiding moment: {:?}",
            chat.slash_lines
        );
        assert!(
            !chat.has_dynamic_rows(),
            "after hiding, the task area does not drive the tick"
        );

        // Create another task (the expand signal reopens the panel) → reappears; all done again → hides again.
        let id2 = create_task(&chat, "t2").await;
        chat.tasks_visible = true;
        chat.tasks_auto = true;
        chat.refresh_tasks();
        assert!(
            chat.tasks_visible,
            "the auto panel reappears with a new task"
        );
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
        assert!(
            !chat.tasks_visible,
            "hides again once everything completes again"
        );
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
        assert!(chat.tasks_visible, "manual open shows it");
        assert!(!chat.tasks_auto, "manual open is not auto");
        chat.refresh_tasks();
        let lines = chat.task_lines();
        let joined: Vec<String> = lines.iter().map(|l| l.plain_text()).collect();
        assert!(joined[0].contains("todo · 1/1 tasks"), "{joined:?}");
        assert!(joined.iter().any(|l| l.starts_with("☒ ")), "{joined:?}");
        assert!(
            chat.slash_lines.is_empty(),
            "a manual panel is its own feedback; no transient row is pushed: {:?}",
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
        let joined = chat.slash_info_lines.join("\n");
        assert!(joined.contains("☒ t1"), "{joined:?}");
        assert!(!joined.contains("no background tasks"), "{joined:?}");
    }

    #[test]
    fn click_toggles_tool_activity() {
        let mut chat = test_chat();
        chat.messages.push(UiMessage {
            activities: vec![tool_activity()],
            ..msg(Role::Assistant, "reply")
        });
        chat.build_rows(100);
        assert!(
            !chat.doc.click_ranges.is_empty(),
            "build_rows populates ranges"
        );

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
        assert_eq!(chat.running_status(), None, "no status row when idle");

        chat.busy = true;
        chat.turn_started = Some(std::time::Instant::now());
        let verb = chat.running_status().expect("busy status").verb;
        assert_eq!(verb, "Working", "fallback when nothing is active");

        let mut tool = tool_activity();
        if let ActivityKind::Tool(t) = &mut tool.kind {
            t.summary = "$ cargo test".to_string();
        }
        chat.messages.push(UiMessage {
            activities: vec![tool],
            ..msg(Role::Assistant, "")
        });
        let verb = chat.running_status().expect("busy status").verb;
        assert_eq!(verb, "$ cargo test", "a running tool's summary wins");

        // A running Watch (subagent/background task) verb = its label (CC ActivityIndicator
        // shows the agent activeForm): after tools, before thinking.
        chat.messages[0].activities.clear();
        chat.messages[0]
            .activities
            .push(Activity::new(ActivityKind::Watch(WatchCall {
                label: "scout · listing desktop dir contents".into(),
                kind: crate::watch::WatchKind::Agent,
                status: WatchState::Running,
                detail: Some("produced 43 chars".into()),
                duration_ms: 0,
            })));
        let verb = chat.running_status().expect("busy status").verb;
        assert_eq!(
            verb, "scout · listing desktop dir contents",
            "a Running Watch's verb = its label"
        );

        // A Done Watch no longer claims the verb (falls through to thinking/Working).
        if let ActivityKind::Watch(w) = &mut chat.messages[0].activities[0].kind {
            w.status = WatchState::Done;
        }
        let verb = chat.running_status().expect("busy status").verb;
        assert_ne!(
            verb, "Agent: listing desktop dir contents",
            "a Done Watch does not occupy the verb"
        );

        chat.messages[0].activities.clear();
        chat.apply_turn_start();
        // TurnStart appends a new message (index 1): the placeholder thinking lives there.
        let stage = match &chat.messages[1].activities[0].kind {
            ActivityKind::Thinking(t) => t.stage,
            _ => unreachable!(),
        };
        let verb = chat.running_status().expect("busy status").verb;
        assert_eq!(verb, stage, "thinking quip words");
    }

    /// bash-mode toggle: `!` on empty input enters, `!` never enters the input,
    /// `!` inserts normally when the input is non-empty, backspace on empty input exits.
    #[test]
    fn bang_toggles_bash_mode() {
        let mut chat = test_chat();
        assert!(!chat.bash_mode);
        assert!(chat.on_key(KeyCode::Char('!'), KeyModifiers::empty()));
        assert!(chat.bash_mode, "! enters bash mode");
        assert!(chat.input.is_empty(), "! itself does not insert input");
        assert!(chat.on_key(KeyCode::Char('l'), KeyModifiers::empty()));
        assert_eq!(chat.input, "l");
        assert!(chat.on_key(KeyCode::Char('!'), KeyModifiers::empty()));
        assert_eq!(chat.input, "l!", "with non-empty input, ! inserts normally");
        assert!(chat.bash_mode, "non-empty input does not exit bash mode");
        assert!(chat.on_key(KeyCode::Backspace, KeyModifiers::empty()));
        assert!(chat.on_key(KeyCode::Backspace, KeyModifiers::empty()));
        assert!(chat.on_key(KeyCode::Backspace, KeyModifiers::empty()));
        assert!(
            !chat.bash_mode,
            "backspace on an empty input exits bash mode"
        );
    }

    /// `!` commands (standalone tool activity): not part of collapse groups, expanded by default when done,
    /// preview = the output itself (stripped of the `$ cmd` echo and the `[Exited with code N]` footnote).
    #[test]
    fn bash_preview_expands_with_output() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::ToolStart {
            name: "Bash".into(),
        });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Bash".into(),
            input: json!({"command": "ls"}),
            standalone: true,
        });
        chat.drain_events();
        assert!(
            chat.messages[0].groups.is_empty(),
            "standalone messages do not group"
        );
        let _ = chat
            .events
            .send(UiEvent::ToolDone(crate::query::ToolCallDone {
                name: "Bash".into(),
                summary: "$ ls".into(),
                output: "$ ls\nREADME.md\nsrc\n[Exited with code 0]".into(),
                is_error: false,
                duration_ms: 5,
                diff: None,
            }));
        chat.drain_events();
        let a = &chat.messages[0].activities[0];
        assert!(a.expanded, "the output preview is expanded by default");
        let text: Vec<String> = a.content.iter().map(|l| l.plain_text()).collect();
        assert_eq!(
            text,
            vec!["README.md", "src"],
            "the preview drops the echo and the exit code: {text:?}"
        );
    }

    /// Model-driven Bash (standalone=false) still folds into a group as before.
    #[test]
    fn model_bash_still_folds_into_group() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::ToolStart {
            name: "Bash".into(),
        });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Bash".into(),
            input: json!({"command": "cargo test"}),
            standalone: false,
        });
        chat.drain_events();
        assert_eq!(
            chat.messages[0].groups.len(),
            1,
            "model-driven messages still group"
        );
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
            cwd: Arc::new(std::sync::Mutex::new(std::env::temp_dir())),
            home: std::env::temp_dir(),
            user_config_dir: std::env::temp_dir().join(".config"),
            quiet: true,
            compact_failures: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            expand_tasks: tokio::sync::watch::channel(false).0,
            agents: crate::agents::AgentRegistry::new(),
            channels: crate::channels::ChannelRegistry::new(Default::default()),
            instance: None,
            attachments: crate::api::image::Attachments::new(),
        });
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (asks_tx, asks_rx) = mpsc::unbounded_channel();
        let mut chat = Chat::new(
            session,
            events_tx,
            events_rx,
            asks_tx,
            asks_rx,
            Theme::dark(),
            crate::tui::theme::ThemeSetting::Auto,
            None,
        );
        chat.bash_mode = true;
        chat.input = "echo hello".to_string();
        chat.submit();
        assert!(chat.bash_mode, "bash mode stays on after submit");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            chat.drain_all();
            if !chat.busy && !chat.messages.is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the turn did not end within the timeout"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(
            chat.messages[0].text, "!echo hello",
            "the user message carries the ! prefix"
        );
        let done_tool = chat.messages[1].activities.iter().any(|a| {
            matches!(&a.kind, ActivityKind::Tool(t)
                if t.name == "Bash" && t.status == ToolStatus::Done)
        });
        assert!(done_tool, "the Bash tool activity closes as Done");
        let preview = &chat.messages[1].activities[0];
        assert!(
            preview.expanded,
            "the ! command's output preview is expanded"
        );
        assert!(
            preview.content.iter().any(|l| l.plain_text() == "hello"),
            "the preview contains the command output: {:?}",
            preview
                .content
                .iter()
                .map(|l| l.plain_text())
                .collect::<Vec<_>>()
        );
        assert!(!chat.busy, "turn ended");
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
        chat.apply_event(UiEvent::ToolStart {
            name: "WebFetch".into(),
        });
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
        assert!(
            text.contains("got it, summarizing"),
            "merged segment: {text}"
        );
    }

    /// Thinking after text interrupts opens a new block, no longer merging.
    #[test]
    fn thinking_after_text_opens_new_block() {
        let mut chat = test_chat();
        chat.apply_turn_start();
        chat.apply_event(UiEvent::ThinkingDelta("plan".into()));
        chat.apply_event(UiEvent::TextDelta("body…".into()));
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
        chat.messages[0].text = "hello!".to_string();
        chat.apply_event(UiEvent::TurnEnd);
        chat.build_rows(100);
        let joined: Vec<String> = chat.doc.rows.iter().map(|r| r.line.plain_text()).collect();
        let lines: Vec<&str> = joined.iter().map(String::as_str).collect();
        let thinking = lines
            .iter()
            .position(|l| l.contains("✻ Thinking"))
            .expect("thinking block header");
        let reply = lines
            .iter()
            .position(|l| l.contains("hello"))
            .expect("reply text");
        let done_line = lines
            .iter()
            .position(|l| l.contains("✻ Baked for 3.3s"))
            .expect("completion line");
        assert!(
            thinking < reply && reply < done_line,
            "the completion line sits at the message end: {lines:?}"
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
        assert!(
            !joined2.contains("for 0.4s"),
            "an empty placeholder has no completion line: {joined2}"
        );
    }

    /// The completion row only appears after the turn ends: with thinking Done but tools still running,
    /// `✻ Baked for 0.4s` is not rendered, avoiding a contradiction with the bottom running-status row.
    #[test]
    fn thinking_completion_line_waits_for_turn_end() {
        let mut chat = test_chat();
        chat.apply_turn_start();
        chat.apply_event(UiEvent::ThinkingDelta("plan".into()));
        chat.apply_event(UiEvent::ToolStart {
            name: "Bash".into(),
        });
        chat.build_rows(100);
        let rows: Vec<String> = chat.doc.rows.iter().map(|r| r.line.plain_text()).collect();
        assert!(
            !rows
                .iter()
                .any(|l| l.starts_with("✻ ") && l.contains(" for ")),
            "no completion line mid-turn: {rows:?}"
        );
        chat.apply_event(UiEvent::TurnEnd);
        chat.build_rows(100);
        let rows: Vec<String> = chat.doc.rows.iter().map(|r| r.line.plain_text()).collect();
        assert!(
            rows.iter()
                .any(|l| l.starts_with("✻ ") && l.contains(" for ")),
            "a completion line must appear after the turn: {rows:?}"
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
        assert!(!chat.busy, "slash does not start a turn");
        let joined = chat.slash_info_lines.join("\n");
        for cmd in [
            "/clear", "/model", "/resume", "/rename", "/compact", "/exit",
        ] {
            assert!(joined.contains(cmd), "missing {cmd}: {joined}");
        }

        chat.input = "/nope".to_string();
        chat.submit();
        assert!(
            chat.slash_error_lines
                .iter()
                .any(|l| l.contains("unknown command")),
            "unknown commands land in the error rows: {:?}",
            chat.slash_error_lines
        );
    }

    /// picker-model.md commit E: /model's level one goes through the PickerModel core — ● marks the current provider,
    /// number jump, and level two keeps its logic (digits never reach the input).
    #[tokio::test]
    async fn model_menu_level_one_uses_picker_core() {
        let mut chat = test_chat();
        // Level one: default + one named provider.
        let settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            ..Default::default()
        };
        let mut s2 = settings.clone();
        s2.providers.insert(
            "deepseek".to_string(),
            crate::settings::ProviderConfig {
                api_key: Some("sk-ds".into()),
                api_base_url: "https://api.deepseek.com".into(),
                supports_images: None,
                protocol: None,
                oauth: None,
            },
        );
        Arc::get_mut(&mut chat.session).unwrap().client =
            crate::api::client::Client::from_settings(&s2).unwrap();

        chat.input = "/model".to_string();
        chat.submit();
        let menu = chat.model_menu.as_ref().expect("menu is open");
        assert_eq!(
            menu.provider_current,
            Some(0),
            "● marks the current provider default"
        );
        let core = menu.provider_picker();
        assert_eq!(
            core.items.len(),
            4,
            "default + built-in presets (codex/opencode-go) + deepseek"
        );

        // Number jump 2 = codex (unified order: default → built-in preset → user-defined);
        // level one consumes it, never reaching the input.
        assert!(chat.on_key(KeyCode::Char('2'), KeyModifiers::empty()));
        let menu = chat.model_menu.as_ref().expect("menu is open");
        assert_eq!(
            menu.provider_selected, 1,
            "2 jumps to codex (presets come before user providers)"
        );
        assert_eq!(chat.input, "", "level-one digits are consumed by the menu");

        // Enter into level two: digits are consumed too (out-of-range is swallowed; nothing leaks into the input).
        assert!(chat.on_key(KeyCode::Enter, KeyModifiers::empty()));
        let menu = chat.model_menu.as_ref().expect("menu is open");
        assert!(menu.models.is_some(), "enters level two");
        assert!(chat.on_key(KeyCode::Char('3'), KeyModifiers::empty()));
        assert_eq!(
            chat.input, "",
            "level-two digits are consumed by the menu; none leak into the input"
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
        assert_eq!(chat.context_usage.window, 128_000);
        assert_eq!(chat.context_usage.used, 0);
        assert!(chat.slash_lines.join("\n").contains("deepseek-v4"));
        // No layer defines `model` → the USER layer gets it; the cwd stays
        // untouched (no conjured .bingo/ in arbitrary directories).
        let saved: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".config/bingo/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            saved["model"], "deepseek-v4",
            "the selection writes back to user settings"
        );
        assert!(
            !home.join(".bingo").exists(),
            "must not create a project layer out of thin air"
        );
        chat.input = "/model".to_string();
        chat.submit();
        assert!(chat.model_menu.is_some(), "no argument enters the selector");
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
        chat.context_usage = crate::context_usage::ContextUsage::new(90_000, 200_000);
        chat.input = "/clear".to_string();
        chat.submit();
        assert!(chat.messages.is_empty(), "UI messages cleared");
        assert_eq!(chat.context_usage.used, 0);
        assert!(
            chat.session.runtime.transcript.borrow().is_some(),
            "new transcript"
        );
    }

    /// /theme: rebuilds the theme (dark → light render difference) + persists to .bingo/settings.json.
    #[test]
    fn slash_theme_switches_and_persists() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-theme", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mut chat = test_chat_home(tmp.join("home"));
        chat.cwd = tmp.display().to_string();
        let dark_text = chat.theme.text;
        chat.input = "/theme light".to_string();
        chat.submit();
        assert_ne!(chat.theme.text, dark_text, "theme switched");
        // theme is a user-level preference: with no layer defining it, write the user layer — never create .bingo in cwd.
        let saved = std::fs::read_to_string(tmp.join("home/.config/bingo/settings.json")).unwrap();
        assert!(saved.contains("\"theme\": \"light\""), "{saved}");
        assert!(
            !tmp.join(".bingo").exists(),
            "must not create a project layer out of thin air"
        );

        // When the project layer defines theme: write the effective layer (project), the user layer is no longer bypassed.
        std::fs::create_dir_all(tmp.join(".bingo")).unwrap();
        std::fs::write(
            tmp.join(".bingo/settings.json"),
            "{\n  \"theme\": \"light\"\n}\n",
        )
        .unwrap();
        chat.input = "/theme dark".to_string();
        chat.submit();
        let project = std::fs::read_to_string(tmp.join(".bingo/settings.json")).unwrap();
        assert!(project.contains("\"theme\": \"dark\""), "{project}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `/theme` with no argument → opens the level selector (picker-model.md commit B): preselects the current level,
    /// ↑↓/1-3 browses, Enter applies + persists, Esc cancels without changing state; the `/theme auto` shortcut stays.
    #[test]
    fn theme_picker_selects_and_applies() {
        let tmp =
            std::env::temp_dir().join(format!("bingo-slash-{}-theme-picker", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mut chat = test_chat_home(tmp.join("home"));
        chat.cwd = tmp.display().to_string();

        // No argument → the menu opens, preselecting the current level (default Auto = index 2).
        chat.input = "/theme".to_string();
        chat.submit();
        let menu = chat.theme_menu.as_ref().expect("menu is open");
        assert_eq!(
            THEME_LEVELS[menu.selected].0, "auto",
            "preselects the current level auto"
        );
        assert_eq!(
            THEME_LEVELS[menu.current].0, "auto",
            "● marks the current level"
        );
        // Esc cancels: state unchanged, menu closed.
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(chat.theme_menu.is_none(), "Esc closes the menu");
        assert_eq!(
            chat.theme_setting,
            crate::tui::theme::ThemeSetting::Auto,
            "Esc does not change the theme"
        );
        assert!(
            !tmp.join(".bingo/settings.json").exists(),
            "cancelling does not write settings"
        );

        // Number jump 2 = light; Enter applies + persists + closes.
        chat.input = "/theme".to_string();
        chat.submit();
        assert!(chat.on_key(KeyCode::Char('2'), KeyModifiers::empty()));
        let menu = chat.theme_menu.as_ref().expect("menu is open");
        assert_eq!(THEME_LEVELS[menu.selected].0, "light", "2 jumps to light");
        assert!(chat.on_key(KeyCode::Enter, KeyModifiers::empty()));
        assert!(chat.theme_menu.is_none(), "Enter closes the menu");
        assert_eq!(
            chat.theme_setting,
            crate::tui::theme::ThemeSetting::Light,
            "Enter applies the theme"
        );
        let saved = std::fs::read_to_string(tmp.join("home/.config/bingo/settings.json")).unwrap();
        assert!(saved.contains("\"theme\": \"light\""), "{saved}");

        // ↑↓ browsing and number jump share the core; reopening shows ● on the new level.
        chat.input = "/theme".to_string();
        chat.submit();
        let menu = chat.theme_menu.as_ref().expect("menu is open");
        assert_eq!(
            THEME_LEVELS[menu.current].0, "light",
            "● follows the active level"
        );
        assert!(chat.on_key(KeyCode::Up, KeyModifiers::empty()));
        let menu = chat.theme_menu.as_ref().expect("menu is open");
        assert_eq!(THEME_LEVELS[menu.selected].0, "dark", "up to dark");
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));

        // The shortcut path stays: /theme auto switches directly.
        chat.input = "/theme auto".to_string();
        chat.submit();
        assert_eq!(
            chat.theme_setting,
            crate::tui::theme::ThemeSetting::Auto,
            "the explicit shortcut stays"
        );
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
        let _ = t_a.append(&crate::api::types::Message::user_text("aaaa"));
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let t_b = crate::transcript::create(&home, &tmp).unwrap();
        let _ = t_b.append(&crate::api::types::Message::user_text("bbbb"));
        let mut chat = test_chat_home(home.clone());
        let _ = chat.session.runtime.transcript_tx.send(Some(t_a.clone()));
        let name_b = t_b.name();
        chat.input = "/resume".to_string();
        chat.submit();
        // No argument → opens the session selector (picker-model.md commit C): the list has b, ● marks the current a.
        let menu = chat.resume_menu.as_ref().expect("selector is open");
        let core = menu.picker();
        assert!(
            core.items.iter().any(|i| i.label == name_b),
            "the selector lists session b"
        );
        assert_eq!(
            menu.current,
            Some(1),
            "● marks the current session (t_a is older, at index 1)"
        );
        // Esc cancels: the current session stays.
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(chat.resume_menu.is_none());
        assert_eq!(
            chat.session
                .runtime
                .transcript
                .borrow()
                .clone()
                .unwrap()
                .name(),
            t_a.name(),
            "Esc does not switch"
        );

        // Enter confirms (snapshot taken by the selected index, the value≠label anchor): switches to b.
        chat.input = "/resume".to_string();
        chat.submit();
        chat.input = String::new();
        chat.on_key(KeyCode::Char('1'), KeyModifiers::empty());
        let menu = chat.resume_menu.as_ref().expect("selector is open");
        assert_eq!(menu.selected, 0, "1 jumps to the newest session");
        assert!(chat.on_key(KeyCode::Enter, KeyModifiers::empty()));
        assert!(chat.resume_menu.is_none(), "Enter closes the selector");
        let current = chat.session.runtime.transcript.borrow().clone().unwrap();
        assert_eq!(
            current.name(),
            name_b,
            "Enter switches the session (snapshot by index)"
        );
        assert!(chat.context_usage.used > 0, "resumed history is estimated");

        // The argument fast path stays.
        chat.input = format!("/resume {}", t_a.name());
        chat.submit();
        let current = chat.session.runtime.transcript.borrow().clone().unwrap();
        assert_eq!(current.name(), t_a.name(), "the argument fast switch stays");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// /share: by default exports the current session's HTML locally only
    /// (file exists, path echoed, overwrite hint).
    #[test]
    fn slash_share_exports_current_session_locally_by_default() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-share", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        let t = crate::transcript::create(&home, &tmp).unwrap_or_else(|e| panic!("{e}"));
        let _ = t.append(&crate::api::types::Message::user_text("hi"));
        let mut chat = test_chat_home(home.clone());
        let _ = chat.session.runtime.transcript_tx.send(Some(t));
        chat.input = "/share".to_string();
        chat.submit();
        let stem = chat
            .session
            .runtime
            .transcript
            .borrow()
            .clone()
            .unwrap()
            .name();
        let joined = chat.slash_info_lines.join("\n");
        assert!(joined.contains("exported"), "{joined}");
        assert!(
            joined.contains(&stem),
            "the path contains the stem: {joined}"
        );
        assert!(
            joined.contains("note: this file contains the full conversation"),
            "privacy warning"
        );
        // Output dir = chat.cwd (test_chat_home is set to home).
        let out = home.join(format!("{stem}.html"));
        assert!(out.exists(), "artifact exists: {}", out.display());
        let html = std::fs::read_to_string(&out).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            html.contains("hi"),
            "the artifact contains the message text"
        );
        assert!(
            html.contains("data-view=\"conv\""),
            "the artifact is a share page"
        );
        // Second export → overwrite notice.
        chat.input = "/share".to_string();
        chat.submit();
        assert!(
            chat.slash_info_lines.join("\n").contains("overwritten"),
            "overwrite notice: {}",
            chat.slash_info_lines.join("\n")
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// /share: without a transcript (the new session was never persisted), report that nothing can be exported.
    #[test]
    fn slash_share_without_transcript_hints() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-noshare", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mut chat = test_chat_home(tmp.join("home"));
        chat.input = "/share".to_string();
        chat.submit();
        assert!(
            chat.slash_lines
                .join("\n")
                .contains("no session to export yet"),
            "{}",
            chat.slash_lines.join("\n")
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// /share flag parsing (pure logic; no browser or network side effects).
    #[test]
    fn parse_share_arg_flags() {
        assert!(parse_share_arg("--open", "--open"));
        assert!(parse_share_arg("--public --open", "--open"));
        assert!(parse_share_arg("  --public  ", "--public"));
        assert!(!parse_share_arg("", "--public"));
        assert!(!parse_share_arg("--open", "--public"));
        assert!(!parse_share_arg("--output x", "--public"));
    }

    /// /share --public: the mock server receives a POST only when public
    /// publishing is explicitly chosen; the public-access and sensitive-content
    /// warning shows before the upload starts.
    #[tokio::test]
    async fn slash_share_public_opt_in_warns_before_upload() {
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
        let t = crate::transcript::create(&home, &tmp).unwrap_or_else(|e| panic!("{e}"));
        let _ = t.append(&crate::api::types::Message::user_text("hi"));
        let mut chat = test_chat_home(home.clone());
        // settings.share.baseUrl → a local mock server (runtime session config; no disk/XDG dependency).
        Arc::get_mut(&mut chat.session)
            .unwrap_or_else(|| panic!("single reference"))
            .settings
            .share
            .base_url = Some(format!("http://{addr}"));
        let _ = chat.session.runtime.transcript_tx.send(Some(t));
        chat.input = "/share --public".to_string();
        chat.submit();
        let preflight = chat
            .pinned_panels
            .iter()
            .flat_map(|(_, lines)| lines.iter().cloned())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            preflight.contains("anyone can access"),
            "the public scope must be visible before uploading: {preflight}"
        );
        assert!(
            preflight.contains("sensitive information"),
            "the sensitive-content note must be visible before uploading: {preflight}"
        );
        // Wait until the mock server thread finishes (it received the request and replied); stable under parallel load too.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while !handle.is_finished() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            handle.is_finished(),
            "the mock server never received the upload request"
        );
        let (request_line, body) = handle.join().unwrap();
        chat.drain_events();
        let joined = chat.slash_info_lines.join("\n");
        assert!(joined.contains("published"), "{joined}");
        assert!(
            joined.contains(&format!("http://{addr}/share/u/")),
            "{joined}"
        );
        assert!(
            chat.pinned_panels.is_empty(),
            "the progress panel clears after the upload completes"
        );
        assert!(request_line.starts_with("POST /share/u/"), "{request_line}");
        assert!(body.contains("hi"), "the uploaded body is the full HTML");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Legacy `--local` remains harmless and resolves to the local-by-default path.
    #[test]
    fn slash_share_legacy_local_flag_stays_local() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-locshare", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        let t = crate::transcript::create(&home, &tmp).unwrap_or_else(|e| panic!("{e}"));
        let _ = t.append(&crate::api::types::Message::user_text("hi"));
        let mut chat = test_chat_home(home.clone());
        let _ = chat.session.runtime.transcript_tx.send(Some(t));
        chat.input = "/share --local".to_string();
        chat.submit();
        let joined = chat.slash_info_lines.join("\n");
        assert!(joined.contains("exported"), "{joined}");
        assert!(!joined.contains("published"), "local mode does not upload");
        let stem = chat
            .session
            .runtime
            .transcript
            .borrow()
            .clone()
            .unwrap()
            .name();
        assert!(
            home.join(format!("{stem}.html")).exists(),
            "the local file exists"
        );
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
        assert!(chat.slash_info_lines.join("\n").contains("allow: (none)"));

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
            chat.slash_info_lines
                .join("\n")
                .contains("- pdf: Converts documents to PDF"),
            "{}",
            chat.slash_info_lines.join("\n")
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
        assert!(empty.contains("no background tasks"), "{empty}");

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
        let listed = chat.slash_info_lines.join("\n");
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
            "slash output is never settled (not flushed)"
        );
        let joined: Vec<String> = chat.doc.rows.iter().map(|r| r.line.plain_text()).collect();
        assert!(joined.iter().any(|l| l.contains("/model")), "{joined:?}");

        // After the TTL, a tick clears it: the transient hint disappears.
        chat.slash_at = Some(
            std::time::Instant::now() - SLASH_OUTPUT_TTL - std::time::Duration::from_millis(1),
        );
        chat.tick();
        assert!(
            chat.slash_lines.is_empty(),
            "slash output disappears after the timeout"
        );
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
            assert!(
                std::time::Instant::now() < deadline,
                "the skill turn did not end"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(
            chat.messages[0].text,
            "✦ guide",
            "only the ✦ marker is submitted: {}",
            &chat.messages[0].text[..chat.messages[0].text.len().min(80)]
        );
        assert!(
            !chat.messages[0].text.contains("Diagnostic guide"),
            "the full body no longer enters the context"
        );
    }

    /// Unknown slash commands still point to /help (no mis-consumption when the skill name does not match).
    #[test]
    fn slash_unknown_still_guides() {
        let mut chat = test_chat();
        chat.input = "/nope-skill".to_string();
        chat.submit();
        let joined = all_slash_text(&chat);
        assert!(
            joined.contains("unknown command: /nope-skill"),
            "the unknown-command guidance is kept: {joined}"
        );
        assert!(
            joined.contains("code=UNKNOWN_COMMAND"),
            "G13 stable error code: {joined}"
        );
        assert!(
            chat.messages.is_empty(),
            "unknown commands do not start a turn"
        );
    }

    /// G12 TTL grading: success hints expire after SLASH_OUTPUT_TTL, error/usage rows
    /// after SLASH_OUTPUT_ERROR_TTL, and error rows clear on the next input.
    #[test]
    fn slash_error_rows_have_longer_ttl_and_clear_on_input() {
        let mut chat = test_chat();
        chat.push_slash_output("✓ done".to_string());
        chat.push_slash_error("[error] code=BAD_ARGUMENT msg=usage: /think [...]".to_string());
        assert_eq!(chat.slash_lines.len(), 1);
        assert_eq!(chat.slash_error_lines.len(), 1);

        // Past the success TTL but inside the error TTL: only the success hint expires.
        chat.slash_at = Some(
            std::time::Instant::now() - SLASH_OUTPUT_TTL - std::time::Duration::from_millis(1),
        );
        chat.tick();
        assert!(chat.slash_lines.is_empty(), "success rows expire after 2s");
        assert_eq!(
            chat.slash_error_lines.len(),
            1,
            "error rows have not expired yet"
        );

        // Past the error TTL: both gone.
        chat.slash_error_at = Some(
            std::time::Instant::now()
                - SLASH_OUTPUT_ERROR_TTL
                - std::time::Duration::from_millis(1),
        );
        chat.tick();
        assert!(
            chat.slash_error_lines.is_empty(),
            "error rows expire after 8s"
        );

        // A fresh error clears on the next real input edit (after_edit path).
        chat.push_slash_error("usage: /think [...]".to_string());
        assert!(chat.on_key(KeyCode::Char('a'), KeyModifiers::empty()));
        assert!(
            chat.slash_error_lines.is_empty(),
            "the next input clears the error rows"
        );
    }

    /// G9 no-match hint: a bare `/`-query with zero matches flags the hint; any further
    /// keystroke (re-filter) or closing clears it.
    #[test]
    fn slash_no_match_flags_hint_row() {
        let mut chat = test_chat();
        chat.input = "/zzz".to_string();
        chat.update_slash_suggestions();
        assert!(chat.slash_suggestions.is_empty());
        assert!(chat.slash_no_match, "/zzz with no match shows the hint");
        // Typing further re-filters: still no match, hint stays.
        chat.input = "/zzzx".to_string();
        chat.update_slash_suggestions();
        assert!(chat.slash_no_match);
        // A matching prefix clears it.
        chat.input = "/th".to_string();
        chat.update_slash_suggestions();
        assert!(!chat.slash_suggestions.is_empty());
        assert!(!chat.slash_no_match, "a match suppresses the hint");
        // Clearing the input (any path that re-runs the filter) removes the hint.
        chat.input = "/zzz".to_string();
        chat.update_slash_suggestions();
        assert!(chat.slash_no_match);
        chat.input = String::new();
        chat.update_slash_suggestions();
        assert!(!chat.slash_no_match, "an empty input suppresses the hint");
    }

    /// P1-E: /provider list keys are redacted — short keys (≤4 chars) get no ellipsis.
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
        // No argument → opens the selector: the info column (URL + redacted key) goes into desc (picker-model.md commit D).
        let menu = chat.provider_menu.as_ref().expect("selector is open");
        let core = menu.picker();
        let desc = core
            .items
            .iter()
            .find(|i| i.label == "default")
            .map(|i| i.description.as_str())
            .expect("the default option");
        assert!(desc.contains("https://api.anthropic.com"), "{desc}");
        assert!(
            desc.contains("key main"),
            "short keys have no ellipsis: {desc}"
        );
        assert!(!desc.contains("main…"), "{desc}");
    }

    /// P5 (D34): with empty settings, /provider login codex takes the preset oauth path —
    /// it must not say "provider not found" nor "API key required" (codex is an oauth-type preset).
    /// (tokio: the device-auth branch spawns on the runtime.)
    #[tokio::test]
    async fn slash_provider_login_uses_preset_with_empty_settings() {
        let tmp = std::env::temp_dir().join(format!("bingo-preset-login-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mut chat = test_chat_home(tmp.join("home"));
        chat.input = "/provider login codex --device-auth".to_string();
        chat.submit();
        let out = all_slash_text(&chat);
        assert!(!out.contains("provider not found"), "{out}");
        assert!(
            !out.contains("requires an API key"),
            "codex is an oauth-type preset: {out}"
        );
        // opencode-go is an apiKey-type preset → without --manual it guides toward a key.
        let mut chat = test_chat_home(tmp.join("home2"));
        chat.input = "/provider login opencode-go".to_string();
        chat.submit();
        let out = all_slash_text(&chat);
        assert!(
            out.contains("requires an API key"),
            "an apiKey-type preset guides toward pasting a key: {out}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// P5: opencode-go --manual stores auth.json `{type:"api"}` (zero settings pollution).
    #[tokio::test]
    async fn slash_provider_login_opencode_go_manual_stores_api_key() {
        let tmp = std::env::temp_dir().join(format!("bingo-preset-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        let mut chat = test_chat_home(home.clone());
        chat.input = "/provider login opencode-go --manual sk-og".to_string();
        chat.submit();
        let store = crate::auth::AuthStore::new(&home);
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if let Ok(Some(entry)) = store.get("opencode-go") {
                match entry {
                    crate::auth::AuthEntry::Api { key } => {
                        assert_eq!(key, "sk-og", "opencode-go key storage");
                    }
                    other => panic!("expected an Api entry: {other:?}"),
                }
                let _ = std::fs::remove_dir_all(&tmp);
                return;
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
        panic!("opencode-go key was not stored");
    }

    /// P4: switching to a not-logged-in OAuth provider → the switch succeeds but carries a login hint; the list
    /// shows the protocol marker + the not-logged-in state, and a missing apiBaseUrl → the protocol default endpoint.
    #[test]
    fn slash_provider_switch_warns_on_oauth_not_logged_in() {
        let tmp = std::env::temp_dir().join(format!("bingo-preset-warn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mut chat = test_chat_home(tmp.join("home"));
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            ..Default::default()
        };
        settings.providers.insert(
            "codex".to_string(),
            crate::settings::ProviderConfig {
                api_key: None,
                api_base_url: String::new(),
                supports_images: None,
                protocol: Some("openai".into()),
                oauth: Some(crate::settings::OauthConfig {
                    kind: "codex".into(),
                    account: None,
                }),
            },
        );
        Arc::get_mut(&mut chat.session).unwrap().client =
            crate::api::client::Client::from_settings_at(
                &settings,
                |_| Err(std::env::VarError::NotPresent),
                &tmp.join("home"),
            )
            .unwrap();
        chat.input = "/provider codex".to_string();
        chat.submit();
        let out = chat.slash_lines.join("\n");
        assert!(out.contains("✓ provider switched: codex"), "{out}");
        assert!(
            out.contains("not logged in: /provider login codex"),
            "not-logged-in hint: {out}"
        );

        chat.input = "/provider".to_string();
        chat.submit();
        // No argument → selector: desc carries the preset endpoint + not-logged-in state + protocol + built-in badge.
        let menu = chat.provider_menu.as_ref().expect("selector is open");
        let core = menu.picker();
        let desc = core
            .items
            .iter()
            .find(|i| i.label == "codex")
            .map(|i| i.description.as_str())
            .expect("the codex option");
        assert!(
            desc.contains("https://chatgpt.com/backend-api"),
            "missing apiBaseUrl → preset endpoint: {desc}"
        );
        assert!(
            desc.contains("○ not logged in (/provider login codex) · openai · built-in"),
            "protocol marker + not-logged-in state + built-in: {desc}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// P4: turn-level error text gains auth hints — an oauth provider 401 → a login hint (with the name);
    /// non-oauth 401 gets none (apiKey scenarios have no login concept); 403 → a model/subscription hint;
    /// hints already present / unrelated codes pass through verbatim.
    #[test]
    fn auth_error_hint_appends_login_guidance() {
        let base = "API error: HTTP 401: invalid token".to_string();
        let hinted = auth_hint_for(true, "codex", "AUTH_REQUIRED", base.clone());
        assert!(
            hinted.contains("/provider login codex"),
            "the oauth 401 hint carries the provider name: {hinted}"
        );

        // A non-oauth provider's 401: no login hint (an expired apiKey just needs the key checked).
        let api_key_msg = auth_hint_for(false, "deepseek", "AUTH_REQUIRED", base.clone());
        assert_eq!(api_key_msg, base, "non-oauth 401 passes through verbatim");

        // An AuthError that already carries a login hint (permanent refresh failure) is not extended again.
        let already = "login has expired (refresh_token_expired): /provider login to sign in again"
            .to_string();
        assert_eq!(
            auth_hint_for(true, "codex", "AUTH_REQUIRED", already.clone()),
            already,
            "an existing hint is not repeated"
        );

        // 403 → a model/subscription hint.
        let denied = auth_hint_for(
            false,
            "deepseek",
            "PERMISSION_DENIED",
            "API error: HTTP 403: quota".into(),
        );
        assert!(
            denied.contains("/model"),
            "the 403 hints at /model: {denied}"
        );

        // Unrelated codes pass through verbatim.
        let rate = "API error: HTTP 429: rate limited".to_string();
        assert_eq!(
            auth_hint_for(true, "codex", "RATE_LIMITED", rate.clone()),
            rate
        );
    }

    #[test]
    fn slash_provider_lists_and_switches() {
        let s_tmp = std::env::temp_dir().join(format!("bingo-slash-{}-provs", std::process::id()));
        let _ = std::fs::remove_dir_all(&s_tmp);
        let mut chat = test_chat();
        chat.input = "/provider".to_string();
        chat.submit();
        // No argument → opens the selector: default first, ● marks the current.
        let menu = chat.provider_menu.as_ref().expect("selector is open");
        assert_eq!(menu.current, Some(0), "● marks the current default");
        let core = menu.picker();
        assert_eq!(
            core.items.first().map(|i| i.label.as_str()),
            Some("default"),
            "default comes first"
        );

        // Configure a named provider, then switch.
        let providers = std::collections::HashMap::from([(
            "deepseek".to_string(),
            crate::settings::ProviderConfig {
                api_key: Some("sk-ds".into()),
                api_base_url: "https://api.deepseek.com".into(),
                supports_images: None,
                protocol: None,
                oauth: None,
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
                api_key: Some("sk-ds".into()),
                api_base_url: "https://api.deepseek.com".into(),
                supports_images: None,
                protocol: None,
                oauth: None,
            },
        );
        Arc::get_mut(&mut chat.session).unwrap().client =
            crate::api::client::Client::from_settings(&settings).unwrap();

        // Reopen the selector: the list has deepseek; Esc does not change the current.
        chat.input = "/provider".to_string();
        chat.submit();
        let menu = chat.provider_menu.as_ref().expect("selector is open");
        let core = menu.picker();
        assert!(
            core.items.iter().any(|i| i.label == "deepseek"),
            "the selector lists deepseek"
        );
        assert_eq!(menu.current, Some(0), "● marks the current default");
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(chat.provider_menu.is_none(), "Esc closes the selector");
        assert_eq!(
            *chat.session.runtime.provider.borrow(),
            "default",
            "Esc does not change"
        );

        // Enter confirms: switch + persist (equivalent to the argument fast path).
        chat.input = "/provider".to_string();
        chat.submit();
        // Order: default(1) → codex(2) → opencode-go(3) → deepseek(4).
        assert!(chat.on_key(KeyCode::Char('4'), KeyModifiers::empty()));
        let menu = chat.provider_menu.as_ref().expect("selector is open");
        assert_eq!(menu.selected, 3, "4 jumps to deepseek");
        assert!(chat.on_key(KeyCode::Enter, KeyModifiers::empty()));
        assert!(chat.provider_menu.is_none(), "Enter closes the selector");
        assert_eq!(
            *chat.session.runtime.provider.borrow(),
            "deepseek",
            "the runtime provider is synced"
        );
        let out = chat.slash_lines.join("\n");
        assert!(out.contains("✓ provider switched: deepseek"), "{out}");

        // The argument fast path stays.
        chat.input = "/provider deepseek".to_string();
        chat.submit();
        assert_eq!(*chat.session.runtime.provider.borrow(), "deepseek");
        let _ = std::fs::remove_dir_all(&s_tmp);

        // s = this session only (a fresh chat, nothing persisted before): runtime switch without writing settings.
        let mut chat = test_chat_home(s_tmp.join("home"));
        chat.cwd = s_tmp.display().to_string();
        Arc::get_mut(&mut chat.session).unwrap().client =
            crate::api::client::Client::from_settings(&settings).unwrap();
        chat.input = "/provider".to_string();
        chat.submit();
        let menu = chat.provider_menu.as_ref().expect("selector is open");
        assert_eq!(menu.current, Some(0), "● marks the current default");
        assert!(chat.on_key(KeyCode::Char('4'), KeyModifiers::empty()));
        assert!(chat.on_key(KeyCode::Char('s'), KeyModifiers::empty()));
        assert_eq!(
            *chat.session.runtime.provider.borrow(),
            "deepseek",
            "s switches the runtime"
        );
        let out = chat.slash_lines.join("\n");
        assert!(
            out.contains("(this session only)"),
            "the s annotation: {out}"
        );
        assert!(
            !s_tmp.join(".bingo/settings.json").exists(),
            "s does not write settings"
        );

        // A miss errors (into the error bucket).
        chat.input = "/provider nope".to_string();
        chat.submit();
        let out = all_slash_text(&chat);
        assert!(out.contains("not found"), "{out}");
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
        let menu = chat.think_menu.as_ref().expect("menu is open");
        assert_eq!(
            THINK_LEVELS[menu.selected].0, "off",
            "preselects off when unset"
        );
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(chat.think_menu.is_none(), "Esc exits the menu");

        // New level xhigh: runtime effect + persistence.
        chat.input = "/think xhigh".to_string();
        chat.submit();
        let out = chat.slash_lines.join("\n");
        assert!(out.contains("✓ thinking level set: xhigh"), "{out}");
        assert_eq!(
            chat.session.runtime.thinking.borrow().as_deref(),
            Some("xhigh")
        );
        let saved: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.join("home/.config/bingo/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["thinkingLevel"], "xhigh");

        chat.input = "/think off".to_string();
        chat.submit();
        let out = chat.slash_lines.join("\n");
        assert!(out.contains("✓ thinking level set: off"), "{out}");
        assert_eq!(chat.session.runtime.thinking.borrow().as_deref(), None);

        chat.input = "/think bogus".to_string();
        chat.submit();
        let out = chat.slash_error_lines.join("\n");
        assert!(
            out.contains("usage: /think") && out.contains("code=BAD_ARGUMENT"),
            "the usage line carries a stable error code: {out}"
        );
        assert_eq!(
            chat.session.runtime.thinking.borrow().as_deref(),
            None,
            "an invalid argument does not change the state"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ------------------------------------------------------------------
    // /mcp: list / enable|disable (persisted list) / reconnect
    // ------------------------------------------------------------------

    async fn slash_mcp_wait(chat: &mut Chat) -> String {
        // Results land in whichever tier fits (confirm/info/error); only NEW
        // lines count — info/error persist across steps by design.
        let start = chat.slash_lines.len();
        let start_info = chat.slash_info_lines.len();
        let start_err = chat.slash_error_lines.len();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            chat.drain_all();
            let output: Vec<String> = chat.slash_lines[start.min(chat.slash_lines.len())..]
                .iter()
                .chain(&chat.slash_info_lines[start_info.min(chat.slash_info_lines.len())..])
                .chain(&chat.slash_error_lines[start_err.min(chat.slash_error_lines.len())..])
                .filter(|l| !l.starts_with('⏳'))
                .map(|l| l.to_string())
                .collect();
            if !output.is_empty() {
                return output.join("\n");
            }
            assert!(
                std::time::Instant::now() < deadline,
                "slash output timed out"
            );
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
        assert!(out.contains("no MCP servers configured"), "{out}");
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
            )));
        chat.input = "/mcp".to_string();
        chat.submit();
        let out = slash_mcp_wait(&mut chat).await;
        assert!(out.contains("MCP servers (1)"), "{out}");
        assert!(out.contains("files"), "{out}");

        chat.input = "/mcp disable files".to_string();
        chat.submit();
        let out = slash_mcp_wait(&mut chat).await;
        assert!(out.contains("disabled 1 MCP server(s): files"), "{out}");
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
        assert!(out.contains("enabled 1 MCP server(s): files"), "{out}");
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
            )));
        chat.input = "/mcp reconnect nope".to_string();
        chat.submit();
        let out = slash_mcp_wait(&mut chat).await;
        assert!(out.contains("no MCP server \"nope\""), "{out}");
        // Reconnect a failing server: the failure detail shows through
        chat.input = "/mcp reconnect files".to_string();
        chat.submit();
        let out = slash_mcp_wait(&mut chat).await;
        assert!(out.contains("files"), "{out}");
        assert!(
            out.contains("handshake failed") || out.contains("✗"),
            "{out}"
        );
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
        assert!(
            chat.slash_suggestions.len() >= SLASH_COMMANDS.len(),
            "everything lands in state (including skill expansion; the render layer windows around the selection, so commands 6+ are reachable again)"
        );
        assert!(chat.slash_suggestions.iter().any(|s| s.name == "model"));
        // Render-layer window: 5 visible + a "N more" indicator.
        let rows = crate::tui::el::render(crate::tui::chrome::chrome(&chat, 100, false)).rows;
        let joined: String = rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("more"),
            "the out-of-window count is visible: {joined}"
        );

        chat.input = "/model deepseek".to_string();
        chat.update_slash_suggestions();
        assert!(
            chat.slash_suggestions.is_empty(),
            "no suggestions with an argument"
        );

        chat.input = "hi".to_string();
        chat.update_slash_suggestions();
        assert!(
            chat.slash_suggestions.is_empty(),
            "no suggestions without a leading /"
        );
    }

    /// Dispatch completeness: every SLASH_COMMANDS entry has a `run_slash` arm, and every
    /// dispatch arm's primary name lives in the table (aliases normalize to a primary).
    /// The mirror list below must stay in sync with `run_slash`'s match arms — this test is the gate.
    #[test]
    fn slash_dispatch_covers_every_table_entry() {
        use std::collections::HashSet;
        // run_slash match arms as (arm, primary); aliases share the primary's handler.
        let dispatch: &[(&str, &str)] = &[
            ("help", "help"),
            ("?", "help"),
            ("exit", "exit"),
            ("quit", "exit"),
            ("clear", "clear"),
            ("reset", "clear"),
            ("new", "clear"),
            ("model", "model"),
            ("cd", "cd"),
            ("theme", "theme"),
            ("rename", "rename"),
            ("resume", "resume"),
            ("share", "share"),
            ("compact", "compact"),
            ("status", "status"),
            ("config", "config"),
            ("context", "context"),
            ("permissions", "permissions"),
            ("mcp", "mcp"),
            ("provider", "provider"),
            ("think", "think"),
            ("skills", "skills"),
            ("tasks", "tasks"),
            ("team", "team"),
        ];
        let table: HashSet<&str> = SLASH_COMMANDS.iter().map(|(n, _, _)| *n).collect();
        let arms: HashSet<&str> = dispatch.iter().map(|(a, _)| *a).collect();
        let primaries: HashSet<&str> = dispatch.iter().map(|(_, p)| *p).collect();
        for name in &table {
            assert!(
                arms.contains(name),
                "the registered command /{name} is missing a run_slash dispatch arm"
            );
        }
        for p in &primaries {
            assert!(
                table.contains(p),
                "the dispatch arm /{p} is not in the SLASH_COMMANDS table"
            );
        }
    }

    /// `/help` renders every SLASH_COMMANDS entry (title + one line each) with its hint,
    /// straight from the same table — the single source stays the only source.
    #[test]
    fn slash_help_lists_every_command_with_hint() {
        let mut chat = test_chat();
        chat.run_slash("help");
        let lines: Vec<&str> = chat.slash_info_lines.iter().map(String::as_str).collect();
        assert_eq!(
            lines.len(),
            SLASH_COMMANDS.len() + 3,
            "title + one row per command + sub-command line + key cross-link"
        );
        assert_eq!(lines[0], "available commands:");
        assert!(
            lines.iter().any(|l| l.contains("/provider login")),
            "sub-commands are discoverable"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("press ? on an empty input")),
            "cross-linked to the ? panel"
        );
        for ((name, hint, desc), line) in SLASH_COMMANDS.iter().zip(&lines[1..]) {
            let cmd = if hint.is_empty() {
                format!("/{name}")
            } else {
                format!("/{name} {hint}")
            };
            assert!(
                line.contains(&cmd),
                "the row carries the command and its argument hint: {line}"
            );
            assert!(
                line.ends_with(desc),
                "the row ends with the description: {line}"
            );
        }
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
            "skills merge into the suggestions"
        );

        chat.input = "/mo".to_string();
        chat.update_slash_suggestions();
        let names: Vec<&str> = chat
            .slash_suggestions
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(names, vec!["model"], "prefix filtering: {names:?}");

        // Overlong descriptions are truncated (MAX_LISTING_DESC_CHARS):
        // a NoWrap overlong row would push the canvas past the terminal width → stale diff residue.
        let long = "x".repeat(400);
        std::fs::write(&skill, format!("---\ndescription: {long}\n---\nbody\n")).unwrap();
        chat.input = "/p".to_string();
        chat.update_slash_suggestions();
        let desc = chat
            .slash_suggestions
            .iter()
            .find(|s| s.name == "pdf")
            .map(|s| s.description.clone())
            .expect("the pdf skill is among the suggestions");
        assert!(
            desc.chars().count() <= crate::skills::MAX_LISTING_DESC_CHARS,
            "description truncated: {} chars",
            desc.chars().count()
        );
        assert!(
            desc.ends_with('…'),
            "truncation carries an ellipsis: {desc}"
        );
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
            "wraps at the top"
        );

        // Tab applies the selection (/help) → `/help ` with suggestions cleared and nothing run.
        chat.input = "/".to_string();
        chat.update_slash_suggestions();
        chat.slash_selected = 0;
        assert!(chat.slash_menu_key(KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(chat.input, "/help ");
        assert!(chat.slash_suggestions.is_empty());
        assert!(chat.slash_lines.is_empty(), "Tab does not execute");

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
            "typing /model shows suggestions: {:?}",
            chat.slash_suggestions
        );
        chat.submit();
        assert!(
            chat.model_menu.is_some(),
            "/model enters the two-level selector (level one = the endpoint list)"
        );
        assert!(
            chat.slash_suggestions.is_empty(),
            "menu mode has no slash suggestions"
        );
        assert!(!chat.busy);
        // Esc exits the menu.
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(chat.model_menu.is_none(), "Esc exits the menu");

        // Partial prefix `/sta`: Enter applies the selection (status first) and runs it.
        chat.input = "/sta".to_string();
        chat.update_slash_suggestions();
        assert!(
            chat.slash_suggestions.iter().any(|s| s.name == "status"),
            "has suggestions: {:?}",
            chat.slash_suggestions
        );
        chat.submit();
        assert!(
            chat.pinned_panels
                .iter()
                .any(|(_, l)| l.join("").contains("⏳")),
            "status ran (async stats hint)"
        );
        assert!(
            chat.slash_suggestions.is_empty(),
            "the menu closes after a partial-prefix execution"
        );
    }

    /// `/model` two-level selector: Enter opens the menu (level-one endpoint list),
    /// move the selection → Enter goes to level two (loading) → Esc exits level by level.
    #[tokio::test]
    async fn model_menu_two_stage_navigation() {
        let mut chat = test_chat();
        chat.input = "/model".to_string();
        chat.submit();
        let Some(menu) = &chat.model_menu else {
            panic!("menu did not open");
        };
        assert_eq!(
            menu.providers,
            vec!["default"],
            "the level-one list contains the current endpoint"
        );
        assert!(menu.models.is_none(), "stops at level one");
        assert!(
            chat.on_key(KeyCode::Down, KeyModifiers::empty()),
            "↓ moves the selection"
        );
        assert_eq!(
            chat.model_menu.as_ref().unwrap().provider_selected,
            0,
            "a single-item list wraps back to 0"
        );
        // Enter goes to level two: async fetch in progress (loading).
        assert!(chat.on_key(KeyCode::Enter, KeyModifiers::empty()));
        let m = &chat.model_menu.as_ref().unwrap().models;
        assert!(m.is_some(), "entered level two");
        assert!(m.as_ref().unwrap().loading, "fetching");
        // Esc returns level by level: two → one → exit.
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(
            chat.model_menu.as_ref().is_some_and(|m| m.models.is_none()),
            "level-two Esc returns to level one"
        );
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(chat.model_menu.is_none(), "level-one Esc exits entirely");
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
            "the selected model takes effect"
        );
        assert!(
            chat.model_menu.is_none(),
            "closes the menu after confirming"
        );
        assert!(
            chat.slash_lines.join("\n").contains("model switched"),
            "confirmation hint"
        );
    }

    /// With multiple providers, level-two Esc returns to level one: the level-one provider list and selection must survive
    /// (regression: open_model_models used to rebuild providers as a single element, losing the list after Esc).
    #[tokio::test]
    async fn model_menu_esc_back_keeps_provider_list() {
        let home = std::env::temp_dir().join(format!("bingo-model-esc-{}", std::process::id()));
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
                    api_key: Some(key.into()),
                    api_base_url: url.into(),
                    supports_images: None,
                    protocol: None,
                    oauth: None,
                },
            );
        }
        Arc::get_mut(&mut chat.session).unwrap().client =
            crate::api::client::Client::from_settings(&settings).unwrap();

        chat.input = "/model".to_string();
        chat.submit();
        let providers = chat.model_menu.as_ref().unwrap().providers.clone();
        assert_eq!(
            providers,
            vec!["default", "codex", "opencode-go", "deepseek", "local"],
            "the same order as /provider: default → built-in preset → user-defined"
        );
        assert_eq!(chat.model_menu.as_ref().unwrap().provider_selected, 0);

        // ↓ twice selects local → Enter into level two (loading) → Esc back to level one.
        chat.on_key(KeyCode::Down, KeyModifiers::empty());
        chat.on_key(KeyCode::Down, KeyModifiers::empty());
        assert_eq!(chat.model_menu.as_ref().unwrap().provider_selected, 2);
        chat.on_key(KeyCode::Enter, KeyModifiers::empty());
        assert!(
            chat.model_menu.as_ref().unwrap().models.is_some(),
            "enters level two"
        );
        chat.on_key(KeyCode::Esc, KeyModifiers::empty());
        let menu = chat.model_menu.as_ref().expect("level one is still there");
        assert_eq!(
            menu.providers,
            vec!["default", "codex", "opencode-go", "deepseek", "local"],
            "the list is kept"
        );
        assert_eq!(menu.provider_selected, 2, "the selection is kept");
        assert!(menu.models.is_none(), "back at level one");

        let _ = std::fs::remove_dir_all(&home);
    }

    /// P0-A: /provider <name> persists the provider; confirming in the /model menu
    /// writes provider + model together into `.bingo/settings.json` (restart restores the same endpoint and model).
    #[tokio::test]
    async fn provider_switch_persists_provider_and_model_menu_persists_both() {
        let tmp =
            std::env::temp_dir().join(format!("bingo-slash-{}-provpersist", std::process::id()));
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
                api_key: Some("sk-ds".into()),
                api_base_url: "https://api.deepseek.com".into(),
                supports_images: None,
                protocol: None,
                oauth: None,
            },
        );
        Arc::get_mut(&mut chat.session).unwrap().client =
            crate::api::client::Client::from_settings(&settings).unwrap();

        // /provider deepseek: switch + persist.
        chat.input = "/provider deepseek".to_string();
        chat.submit();
        let saved: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.join("home/.config/bingo/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            saved["provider"], "deepseek",
            "the provider is persisted (user layer)"
        );
        assert_eq!(*chat.session.runtime.provider.borrow(), "deepseek");

        // /model menu: current provider=deepseek (preselected) → level-two confirms the model
        // → provider + model persist together.
        chat.input = "/model".to_string();
        chat.submit();
        assert_eq!(
            chat.model_menu.as_ref().unwrap().provider_selected,
            3,
            "level one preselects the current provider (unified order: default, codex, opencode-go, deepseek)"
        );
        chat.on_key(KeyCode::Enter, KeyModifiers::empty());
        if let Some(m) = &mut chat.model_menu.as_mut().unwrap().models {
            m.models = vec!["deepseek-v4".to_string()];
            m.loading = false;
        }
        chat.on_key(KeyCode::Enter, KeyModifiers::empty());
        assert!(
            chat.model_menu.is_none(),
            "closes the menu after confirming"
        );
        let saved: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.join("home/.config/bingo/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["model"], "deepseek-v4", "the model is persisted");
        assert_eq!(
            saved["provider"], "deepseek",
            "the provider persists with the model"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// P1-E: /model <name> sets directly — with a cache and no hit → a non-blocking hint;
    /// no cache / never fetched → switches directly with no hint.
    #[test]
    fn slash_model_validates_against_cached_list() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-modelval", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mut chat = test_chat_home(tmp.clone());

        // No cache: switch directly, no validation hint.
        chat.input = "/model custom-new".to_string();
        chat.submit();
        let out = chat.slash_lines.join("\n");
        assert!(out.contains("✓ model switched: custom-new"), "{out}");
        assert!(!out.contains("not in"), "{out}");

        // The current provider has a cache and the model is not in it: the success note carries a ⚠ hint,
        // one line (advisory, non-blocking; it still switches).
        chat.slash_lines.clear();
        chat.models_cache.insert(
            "default".to_string(),
            vec!["claude-sonnet-5".to_string(), "deepseek-v4".to_string()],
        );
        chat.input = "/model unknown-xyz".to_string();
        chat.submit();
        let out = chat.slash_lines.join("\n");
        assert!(
            out.contains("✓ model switched: unknown-xyz (⚠ not in default's known list"),
            "{out}"
        );
        assert_eq!(
            out.lines().count(),
            1,
            "one-line output; ⚠ and ✓ do not coexist"
        );
        assert_eq!(*chat.session.runtime.model.borrow(), "unknown-xyz");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// P1-F: after ModelsLoaded, the current provider's current model is preselected (the counterpart of /think
    /// preselecting the current level), so browsing cannot accidentally switch; a model missing from the list falls back to 0; the result
    /// is written into models_cache (used by /model <name> validation).
    #[tokio::test]
    async fn models_loaded_preselects_current_model_and_caches() {
        let mut chat = test_chat();
        chat.input = "/model".to_string();
        chat.submit();
        chat.on_key(KeyCode::Enter, KeyModifiers::empty());
        // Current provider=default, current model=test-model (test_chat's initial value).
        chat.handle(UiEvent::ModelsLoaded {
            provider: "default".into(),
            models: vec!["m0".into(), "test-model".into(), "m2".into()],
            failed: None,
        });
        let m = chat.model_menu.as_ref().unwrap().models.as_ref().unwrap();
        assert_eq!(m.selected, 1, "preselects the current model");
        assert_eq!(m.models[m.selected], "test-model");
        assert_eq!(
            chat.models_cache.get("default").map(Vec::as_slice),
            Some(&["m0".to_string(), "test-model".to_string(), "m2".to_string()][..]),
            "the load result lands in the cache"
        );

        // The current model is not in the list: the selection falls back to 0.
        chat.handle(UiEvent::ModelsLoaded {
            provider: "default".into(),
            models: vec!["m0".into(), "m1".into()],
            failed: None,
        });
        let m = chat.model_menu.as_ref().unwrap().models.as_ref().unwrap();
        assert_eq!(m.selected, 0, "a miss falls back to 0");
    }

    /// /think with no argument enters the level selector: preselects the current level, ↑↓ moves, Enter confirms, Esc exits.
    #[test]
    fn think_menu_navigates_and_confirms() {
        let home = std::env::temp_dir().join(format!("bingo-think-menu-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let mut chat = test_chat_home(home.clone());
        let _ = chat.session.runtime.thinking_tx.send(Some("high".into()));
        chat.input = "/think".to_string();
        chat.submit();
        let menu = chat.think_menu.as_ref().expect("menu is open");
        assert_eq!(
            THINK_LEVELS[menu.selected].0, "high",
            "preselects the current level"
        );
        // ↑ to medium, Enter confirms: runtime effect + persistence + menu closes.
        assert!(chat.on_key(KeyCode::Up, KeyModifiers::empty()));
        assert!(chat.on_key(KeyCode::Enter, KeyModifiers::empty()));
        assert!(
            chat.think_menu.is_none(),
            "closes the menu after confirming"
        );
        assert_eq!(
            chat.session.runtime.thinking.borrow().as_deref(),
            Some("medium")
        );
        let saved: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".config/bingo/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            saved["thinkingLevel"], "medium",
            "the selection is persisted"
        );
        // Reopen the menu: Esc exits directly; off clears the level.
        chat.input = "/think".to_string();
        chat.submit();
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(chat.think_menu.is_none(), "Esc exits");
        chat.input = "/think off".to_string();
        chat.submit();
        assert_eq!(
            chat.session.runtime.thinking.borrow().as_deref(),
            None,
            "off clears the level"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// THINK_LEVELS (selector) matches the API layer's THINKING_LEVELS: off + all levels, in the same order.
    #[test]
    fn think_levels_match_api_levels() {
        assert_eq!(THINK_LEVELS[0].0, "off");
        let menu: Vec<&str> = THINK_LEVELS[1..].iter().map(|(n, _)| *n).collect();
        assert_eq!(menu, crate::api::contract::THINKING_LEVELS.to_vec());
    }

    /// 1..6 direct jump selects the right row and the digits never reach the input;
    /// `s` applies session-only (runtime changes, settings.json not written).
    #[test]
    fn think_menu_direct_jump_and_session_only() {
        let home = std::env::temp_dir().join(format!("bingo-think-menu-s-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let mut chat = test_chat_home(home.clone());
        let _ = chat.session.runtime.thinking_tx.send(Some("off".into()));
        chat.input = "/think".to_string();
        chat.submit();
        let menu = chat.think_menu.as_ref().expect("menu is open");
        assert_eq!(menu.current, 0, "● records the active level at open time");
        // '3' jumps to medium (off=1, low=2, medium=3); digits are consumed, not typed.
        assert!(chat.on_key(KeyCode::Char('3'), KeyModifiers::empty()));
        let menu = chat.think_menu.as_ref().expect("menu is open");
        assert_eq!(THINK_LEVELS[menu.selected].0, "medium", "3 jumps to medium");
        assert_eq!(
            chat.input, "",
            "digit keys are consumed by the menu, never reaching the input"
        );
        // '6' wraps-jumps to max; Enter persists.
        assert!(chat.on_key(KeyCode::Char('6'), KeyModifiers::empty()));
        let menu = chat.think_menu.as_ref().expect("menu is open");
        assert_eq!(THINK_LEVELS[menu.selected].0, "max", "6 jumps to max");
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(
            chat.session.runtime.thinking.borrow().as_deref(),
            Some("off"),
            "Esc does not change the state"
        );

        // `s`: session-only — runtime switches, no settings write.
        chat.input = "/think".to_string();
        chat.submit();
        assert!(chat.on_key(KeyCode::Char('2'), KeyModifiers::empty()));
        assert!(chat.on_key(KeyCode::Char('s'), KeyModifiers::empty()));
        assert!(
            chat.think_menu.is_none(),
            "s closes the menu after confirming"
        );
        assert_eq!(
            chat.session.runtime.thinking.borrow().as_deref(),
            Some("low"),
            "s switches the runtime"
        );
        let out = chat.slash_lines.join("\n");
        assert!(
            out.contains("(this session only)"),
            "the s output is annotated this-session-only: {out}"
        );
        assert!(
            !home.join(".bingo/settings.json").exists(),
            "s does not write settings.json"
        );
        let _ = std::fs::remove_dir_all(&home);
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
    fn agent_control_classifier_counts_a_change_apart_from_a_look() {
        assert_eq!(
            classify_tool("AgentControl", &json!({"action": "list"})),
            Some(CollapseKind::AgentCheck)
        );
        assert_eq!(
            classify_tool(
                "AgentControl",
                &json!({"action": "messages", "agent": "scout"})
            ),
            Some(CollapseKind::AgentCheck)
        );
        assert_eq!(
            classify_tool("AgentControl", &json!({"action": "stop", "agent": "scout"})),
            Some(CollapseKind::AgentStop)
        );
        assert_eq!(
            classify_tool(
                "AgentControl",
                &json!({"action": "delete", "agent": "scout"})
            ),
            Some(CollapseKind::AgentDelete)
        );
        // No action at all is a malformed call: it stays a standalone row rather than
        // being counted as one of anything.
        assert_eq!(
            classify_tool("AgentControl", &json!({"agent": "scout"})),
            None
        );
    }

    #[test]
    fn summary_past_tense_counts() {
        let mut g = CollapseGroup {
            activities: vec![0, 1, 2],
            search: 1,
            read_paths: vec!["a.md".into(), "b.md".into(), "c.md".into()],
            ..CollapseGroup::default()
        };
        assert_eq!(
            collapse_summary(&g, false),
            "Searched for 1 pattern, read 3 files"
        );
        g.search = 2;
        assert_eq!(
            collapse_summary(&g, false),
            "Searched for 2 patterns, read 3 files"
        );
        g.active = true;
        assert_eq!(
            collapse_summary(&g, true),
            "Searching for 2 patterns, reading 3 files…"
        );
    }

    #[test]
    fn summary_never_reports_a_stopped_subagent_as_a_look() {
        let g = CollapseGroup {
            activities: vec![0, 1, 2, 3],
            agent_checks: 3,
            agent_stops: 1,
            ..CollapseGroup::default()
        };
        assert_eq!(
            collapse_summary(&g, false),
            "Checked 3 subagents, stopped 1 subagent"
        );
        let g = CollapseGroup {
            activities: vec![0],
            agent_deletes: 1,
            ..CollapseGroup::default()
        };
        assert_eq!(collapse_summary(&g, false), "Deleted 1 subagent");
        // Mixed with file work: one group, one line, every kind still named.
        let g = CollapseGroup {
            activities: vec![0, 1],
            read_paths: vec!["a.md".into()],
            agent_checks: 2,
            ..CollapseGroup::default()
        };
        assert_eq!(
            collapse_summary(&g, true),
            "Reading 1 file, checking 2 subagents…"
        );
    }

    #[test]
    fn summary_read_paths_dedupe_and_ops_fallback() {
        let g = CollapseGroup {
            activities: vec![0, 1],
            read_paths: vec!["a.md".into(), "a.md".into()],
            ..CollapseGroup::default()
        };
        assert_eq!(collapse_summary(&g, false), "Read 1 file");
        let g = CollapseGroup {
            activities: vec![0],
            read_ops: 2,
            list: 1,
            ..CollapseGroup::default()
        };
        assert_eq!(
            collapse_summary(&g, false),
            "Read 2 files, listed 1 directory"
        );
    }

    #[test]
    fn result_summaries() {
        assert_eq!(
            result_summary("Read", "line1\nline2\n\nline3"),
            Some("Read 3 lines".to_string())
        );
        assert_eq!(
            result_summary("Grep", "a:1:x\nb:2:y"),
            Some("Found 2 matches".to_string())
        );
        assert_eq!(
            result_summary("Glob", "a.rs\nb.rs"),
            Some("Found 2 files".to_string())
        );
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
            let _ = chat.events.send(UiEvent::ToolStart {
                name: "Read".into(),
            });
            chat.drain_events();
            let _ = chat.events.send(UiEvent::ToolReady {
                name: "Read".into(),
                input: json!({"file_path": path}),
                standalone: false,
            });
            chat.drain_events();
        }
        let joined = visible(&mut chat, 120, 20);
        assert!(
            joined.contains("Reading 2 files"),
            "active summary: {joined}"
        );
        assert!(joined.contains("ctrl+o to expand"), "fold hint: {joined}");
        assert!(
            !joined.contains("a.md"),
            "paths hidden when collapsed: {joined}"
        );
    }

    /// Managing subagents used to produce one two-line block per call, all reading
    /// `AgentControl(action="messages")` — the target was invisible (the k=v fallback takes the
    /// alphabetically first key). One fold, counts that keep a stop apart, and a ⎿ row naming
    /// the instance the latest call was aimed at.
    #[test]
    fn consecutive_agent_control_calls_fold_and_name_their_target() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        for input in [
            json!({"action": "messages", "agent": "scout"}),
            json!({"action": "messages", "agent": "reviewer"}),
            json!({"action": "stop", "agent": "scout"}),
        ] {
            let _ = chat.events.send(UiEvent::ToolStart {
                name: "AgentControl".into(),
            });
            chat.drain_events();
            let _ = chat.events.send(UiEvent::ToolReady {
                name: "AgentControl".into(),
                input,
                standalone: false,
            });
            chat.drain_events();
        }
        assert_eq!(
            chat.messages[0].groups.len(),
            1,
            "three calls, one group — not three blocks"
        );
        let joined = visible(&mut chat, 120, 20);
        assert!(
            joined.contains("Checking 2 subagents, stopping 1 subagent"),
            "the stop is not counted as a look: {joined}"
        );
        assert!(
            joined.contains("⎿  stop scout"),
            "the ⎿ row names the latest call and its target: {joined}"
        );
    }

    /// A tool the classifier did not know closed the open group, so a subagent check in the
    /// middle of file work split one fold into three blocks.
    #[test]
    fn an_agent_control_call_no_longer_breaks_a_file_group() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        for (name, input) in [
            ("Read", json!({"file_path": "a.md"})),
            ("AgentControl", json!({"action": "list"})),
            ("Read", json!({"file_path": "b.md"})),
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
        assert_eq!(chat.messages[0].groups.len(), 1, "still one group");
        let joined = visible(&mut chat, 120, 20);
        assert!(
            joined.contains("Reading 2 files, checking 1 subagent"),
            "both kinds counted on one line: {joined}"
        );
    }

    /// A failure inside a fold used to be invisible: the summary counted the call as if it had
    /// worked and only ctrl+o showed the error row. It matters most now that stopping a subagent
    /// folds too — "stopped 1 subagent" must not stand for a stop that was refused.
    #[test]
    fn a_failure_inside_the_fold_is_named_on_the_summary_row() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        for input in [
            json!({"action": "stop", "agent": "ghost"}),
            json!({"action": "list"}),
        ] {
            let _ = chat.events.send(UiEvent::ToolStart {
                name: "AgentControl".into(),
            });
            chat.drain_events();
            let _ = chat.events.send(UiEvent::ToolReady {
                name: "AgentControl".into(),
                input,
                standalone: false,
            });
            chat.drain_events();
        }
        let _ = chat
            .events
            .send(UiEvent::ToolDone(crate::query::ToolCallDone {
                name: "AgentControl".into(),
                summary: "stop ghost".into(),
                output: "no subagent named ghost".into(),
                is_error: true,
                duration_ms: 0,
                diff: None,
            }));
        chat.drain_events();
        let joined = visible(&mut chat, 120, 20);
        assert!(
            joined.contains("· 1 failed"),
            "the folded failure is named: {joined}"
        );
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
        let _ = chat
            .events
            .send(UiEvent::ToolDone(crate::query::ToolCallDone {
                name: "Read".into(),
                summary: "Read a.md".into(),
                output: "l1\nl2\nl3".into(),
                is_error: false,
                duration_ms: 0,
                diff: None,
            }));
        let _ = chat
            .events
            .send(UiEvent::ToolDone(crate::query::ToolCallDone {
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
        assert!(
            joined.contains("Read a.md"),
            "expanded first tool: {joined}"
        );
        assert!(
            joined.contains("Read b.md"),
            "expanded second tool: {joined}"
        );
        assert!(
            joined.contains("Read 3 lines"),
            "result summary row: {joined}"
        );
        assert!(
            !joined.contains("Reading 2 files"),
            "no collapse line: {joined}"
        );
    }

    #[test]
    fn non_collapsible_tool_breaks_group() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::ToolStart {
            name: "Read".into(),
        });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Read".into(),
            input: json!({"file_path": "a.md"}),
            standalone: false,
        });
        let _ = chat.events.send(UiEvent::ToolStart {
            name: "WebSearch".into(),
        });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "WebSearch".into(),
            input: json!({"query": "rust"}),
            standalone: false,
        });
        chat.drain_events();
        let joined = visible(&mut chat, 120, 20);
        assert!(joined.contains("Read 1 file"), "group rendered: {joined}");
        assert!(
            joined.contains("WebSearch"),
            "websearch independent: {joined}"
        );
        assert!(
            !joined.contains("Reading"),
            "group closed by websearch: {joined}"
        );
    }

    #[test]
    fn tool_after_thinking_placeholder_groups_without_panic() {
        // Regression: a tool right after the TurnStart placeholder thinking — group_of must stay in sync with activities.
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        chat.apply_turn_start();
        let _ = chat.events.send(UiEvent::ToolStart {
            name: "Read".into(),
        });
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
        let _ = chat.events.send(UiEvent::ToolStart {
            name: "Read".into(),
        });
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
        assert!(
            visible(&mut chat, 120, 40).contains("Read 2 files"),
            "collapsed first"
        );
        assert!(chat.toggle_transcript());
        let expanded = visible(&mut chat, 120, 40);
        assert!(expanded.contains("Read a.md"), "expanded: {expanded}");
        assert!(
            !expanded.contains("Read 2 files"),
            "no collapse line: {expanded}"
        );
        assert!(chat.toggle_transcript());
        let collapsed = visible(&mut chat, 120, 40);
        assert!(
            collapsed.contains("Read 2 files"),
            "collapsed again: {collapsed}"
        );
        assert!(
            !collapsed.contains("Read a.md"),
            "tools hidden: {collapsed}"
        );
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
        assert!(
            collapsed.contains("Read 2 files"),
            "ctrl+o collapsed: {collapsed}"
        );
    }

    #[test]
    fn running_tool_shows_input_summary_after_ready() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::ToolStart {
            name: "Skill".into(),
        });
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
        let _ = chat
            .events
            .send(UiEvent::ToolDone(crate::query::ToolCallDone {
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
        assert!(
            joined.contains("✦ Skill(pdf doc.md)"),
            "header row: {joined}"
        );
        assert!(
            joined.contains("✦ pdf"),
            "the result row shows only the ✦ skill name: {joined}"
        );
        assert!(
            !joined.contains("read /tmp/skills/SKILL.md"),
            "the pointer path never enters the TUI result row: {joined}"
        );
        assert!(
            joined.contains("Ran in 3.2s"),
            "the result row carries the duration: {joined}"
        );
        assert!(
            !joined.contains("3210ms"),
            "milliseconds no longer enter the header row: {joined}"
        );
    }

    /// Agent aligns with Task renderToolUseMessage=null: ToolStart creates no tool activity row,
    /// the message area is carried solely by the Watch progress row (the only display, updated in place).
    #[test]
    fn agent_tool_start_creates_no_tool_activity() {
        assert!(is_hidden_tool("Agent"), "Agent is a hidden tool");
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::ToolStart {
            name: "Agent".into(),
        });
        chat.drain_events();
        assert!(
            chat.messages[0]
                .activities
                .iter()
                .all(|a| !matches!(a.kind, ActivityKind::Tool(_))),
            "Agent creates no Tool activity: {:?}",
            chat.messages[0]
                .activities
                .iter()
                .map(|a| format!("{:?}", a.kind))
                .collect::<Vec<_>>()
        );

        // The Watch activity row is created normally (the only Agent display).
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "Agent: listing desktop dir contents".into(),
            kind: crate::watch::WatchKind::Agent,
            status: WatchState::Running,
            detail: Some("produced 0 chars".into()),
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
        assert_eq!(watch_rows, 1, "a single Watch row");

        // Later events with the same label update in place, creating no new row.
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "Agent: listing desktop dir contents".into(),
            kind: crate::watch::WatchKind::Agent,
            status: WatchState::Running,
            detail: Some("produced 43 chars".into()),
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
        assert_eq!(watch_rows, 1, "same-label events do not create new rows");
        let detail = chat.messages[0]
            .activities
            .iter()
            .find_map(|a| match &a.kind {
                ActivityKind::Watch(w) => w.detail.clone(),
                _ => None,
            });
        assert_eq!(
            detail.as_deref(),
            Some("produced 43 chars"),
            "the detail updates in place"
        );
    }

    #[tokio::test]
    async fn terminal_watch_event_triggers_auto_turn_when_idle() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "Agent: long task".into(),
            kind: crate::watch::WatchKind::Agent,
            status: WatchState::Running,
            detail: None,
            duration_ms: 0,
            payload: None,
            signal: None,
        });
        chat.drain_events();
        assert!(!chat.busy);
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "Agent: long task".into(),
            kind: crate::watch::WatchKind::Agent,
            status: WatchState::Done,
            detail: Some("done".into()),
            duration_ms: 30000,
            payload: Some(serde_json::json!("result")),
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
        chat.input = "still typing".to_string();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "tail -f app.log".into(),
            kind: crate::watch::WatchKind::Command,
            status: WatchState::Running,
            detail: None,
            duration_ms: 0,
            payload: None,
            signal: None,
        });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "tail -f app.log".into(),
            kind: crate::watch::WatchKind::Command,
            status: WatchState::Running,
            detail: Some("found 1 error".into()),
            duration_ms: 12000,
            payload: None,
            signal: Some("found error: ERROR boom".into()),
        });
        chat.drain_events();
        tokio::task::yield_now().await;
        chat.drain_events();
        assert!(chat.busy, "signal wakes despite typing");
        assert_eq!(chat.input, "still typing", "input preserved");
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
        let id = watch.register_with_conditions(Box::new(FakeWatchable), Vec::new(), None);
        watch.set_state(
            id,
            crate::watch::WatchState::Done,
            Some("done".into()),
            None,
        );
        assert!(watch.has_wake_notifications(None), "notification queued");
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
        let big = "the clippy baseline is running in the background (task 2). Here is the summary and optimization list.\n\n---\n\n## Project overview (subagent summary)\n\n**bingo** is a local agent CLI implemented in Rust.\n\n- **Two run modes**: interactive TUI and headless `--print`\n- **9 built-in tools** + MCP (stdio) adapters; five permission-gate modes\n- **Core layering**: `api/`, `tool/`, `query.rs`, `tui.rs`\n- **watch mechanism**: background command / subagent state machines\n";
        for chunk in big.chars().collect::<Vec<_>>().chunks(120) {
            let t: String = chunk.iter().collect();
            let _ = chat.events.send(UiEvent::TextDelta(t));
            chat.drain_events();
        }
        let _ = chat.events.send(UiEvent::ToolStart {
            name: "Bash".into(),
        });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Bash".into(),
            input: json!({"command": "cargo clippy"}),
            standalone: false,
        });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "Agent: review".into(),
            kind: crate::watch::WatchKind::Agent,
            status: WatchState::Running,
            detail: Some("produced 100 chars".into()),
            duration_ms: 5000,
            payload: None,
            signal: None,
        });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::TextDelta(
            "more body text, with more CJK, continuing.".into(),
        ));
        chat.drain_events();
        let _ = chat
            .events
            .send(UiEvent::ToolDone(crate::query::ToolCallDone {
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
            label: "Agent: review".into(),
            kind: crate::watch::WatchKind::Agent,
            status: WatchState::Done,
            detail: Some("done".into()),
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
            label: "Agent: explore".into(),
            kind: crate::watch::WatchKind::Agent,
            status: WatchState::Running,
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
            label: "Agent: explore".into(),
            kind: crate::watch::WatchKind::Agent,
            status: WatchState::Done,
            detail: Some("done".into()),
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
        assert_eq!(w.status, WatchState::Done, "in-place status change");
    }

    #[test]
    fn idle_round_notification_does_not_trigger_auto_turn() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "watch ls".into(),
            kind: crate::watch::WatchKind::Command,
            status: WatchState::Idle,
            detail: Some("round 1".into()),
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
            status: WatchState::Running,
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
            status: WatchState::Idle,
            detail: Some("round 2".into()),
            duration_ms: 4000,
            payload: None,
            signal: None,
        });
        let _ = chat.events.send(UiEvent::WatchEvent {
            label: "watch -n 2 ls".into(),
            kind: crate::watch::WatchKind::Command,
            status: WatchState::Done,
            detail: None,
            duration_ms: 9000,
            payload: Some(serde_json::json!("done output")),
            signal: None,
        });
        chat.drain_events();
        assert_eq!(chat.messages[0].activities.len(), 1, "updates in place");
        let joined = visible(&mut chat, 120, 30);
        assert!(joined.contains("⏺ watch -n 2 ls"), "header: {joined}");
        assert!(joined.contains("  ⎿  round 2"), "result row: {joined}");
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
        assert_eq!(
            collapse_summary(g, false),
            "Read 1 file, ran 2 bash commands"
        );
        assert_eq!(
            collapse_summary(g, true),
            "Reading 1 file, running 2 bash commands…"
        );
        for (summary, out) in [
            ("Bash $ cargo test", "ok"),
            ("Read a.md", "l1"),
            ("Bash $ npm run build", "done"),
        ] {
            let _ = chat
                .events
                .send(UiEvent::ToolDone(crate::query::ToolCallDone {
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
        let _ = chat.events.send(UiEvent::ToolStart {
            name: "Read".into(),
        });
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
        let _ = chat
            .events
            .send(UiEvent::ToolDone(crate::query::ToolCallDone {
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
        assert!(
            !joined.contains("⎿"),
            "hint hidden when group done: {joined}"
        );
    }

    /// Collapse groups are bounded by text: neither RoundEnd (model rounds) nor thinking splits them,
    /// tools across rounds merge into one group; only text (TextDelta) opens a new one.
    #[test]
    fn group_survives_rounds_and_thinking_until_text() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::ToolStart {
            name: "Grep".into(),
        });
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
        let _ = chat.events.send(UiEvent::ToolStart {
            name: "Grep".into(),
        });
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
        let _ = chat.events.send(UiEvent::ToolStart {
            name: "Read".into(),
        });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            name: "Read".into(),
            input: json!({"file_path": "a.md"}),
            standalone: false,
        });
        chat.drain_events();
        assert_eq!(
            chat.messages[0].groups.len(),
            1,
            "same-group Read joins group"
        );
        // Text appears: the group closes and later tools open a new one.
        let _ = chat.events.send(UiEvent::TextDelta("conclusion…".into()));
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolStart {
            name: "Grep".into(),
        });
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
        assert!(
            visible(&mut chat, 120, 40).contains("Reading 2 files"),
            "running fold"
        );
        assert!(chat.toggle_transcript());
        assert!(
            !visible(&mut chat, 120, 40).contains("Reading 2 files"),
            "expanded"
        );
        for (summary, out) in [("Read a.md", "l1\nl2\nl3"), ("Read b.md", "x\ny")] {
            let _ = chat
                .events
                .send(UiEvent::ToolDone(crate::query::ToolCallDone {
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
        assert!(
            collapsed.contains("Read 2 files"),
            "collapsed after turn: {collapsed}"
        );
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
            let _ = chat
                .events
                .send(UiEvent::ToolDone(crate::query::ToolCallDone {
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
            assert!(
                !visible(&mut chat, 120, 40).contains("Read 2 files"),
                "expanded state"
            );
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
        let row = chat
            .doc
            .rows
            .iter()
            .find(|r| r.line.plain_text().starts_with("❯"));
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
        assert_eq!(bubbles.len(), 3, "one bubble Row per line");
        for row in &bubbles {
            for seg in &row.line.segs {
                assert!(
                    !seg.text.contains(['\n', '\r']),
                    "a Row must be a single line: {:?}",
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
        assert!(bubbles.len() > 1, "a long message wraps into multiple rows");
        for row in bubbles {
            // 2 prefix columns + body ≤ width-1 (1 column of right padding inside the bubble).
            assert!(
                text_width(&row.line.plain_text()) <= 29,
                "row width exceeded: {:?}",
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
        let _ = chat.events.send(UiEvent::ToolStart {
            name: "Bash".into(),
        });
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
        assert!(
            !hint.line.plain_text().contains('\n'),
            "the hint is single-lined"
        );
        assert!(
            text_width(&hint.line.plain_text()) <= 60,
            "the hint truncates by width"
        );
    }

    /// The flush cursor counts by message boundary: re-layout after a width change (all row numbers change) never re-flushes.
    #[test]
    fn flush_cursor_survives_width_change() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::User, "first message"));
        chat.messages.push(msg(Role::Assistant, "reply body"));
        chat.build_rows(100);
        assert_eq!(
            chat.doc.settled,
            chat.doc.rows.len(),
            "everything settles when idle"
        );
        assert_eq!(
            settled_segments(&chat),
            3,
            "welcome card + 2 messages = 3 segments"
        );
        chat.advance_flushed();
        assert_eq!(chat.flushed_segments, 3);
        assert_eq!(chat.tail_start, chat.doc.rows.len());

        // Rebuild after a width change: already-flushed segments no longer appear in the doc.
        chat.build_rows(40);
        assert_eq!(
            chat.tail_start, 0,
            "the tail restarts from zero after a rebuild"
        );
        assert!(chat.doc.rows.is_empty(), "flushed content is not rebuilt");
        let text: String = chat.doc.rows.iter().map(|r| r.line.plain_text()).collect();
        assert!(!text.contains("first message"), "not printed again");

        // A new message only builds its own segment.
        chat.messages.push(msg(Role::User, "second message"));
        chat.build_rows(40);
        assert!(
            chat.doc
                .rows
                .iter()
                .any(|r| r.line.plain_text().contains("second message")),
            "the new message enters the document"
        );
        assert_eq!(settled_segments(&chat), 1, "only 1 new segment");
    }

    /// Streaming (unsettled) content is not flushed: a full markdown re-parse rewrites earlier rows,
    /// which would be frozen in scrollback as an unchangeable intermediate state.
    #[test]
    fn streaming_content_is_not_flushed_until_settled() {
        let mut chat = test_chat();
        chat.build_rows(80);
        chat.advance_flushed();
        let welcome_segments = chat.flushed_segments;
        assert_eq!(welcome_segments, 1, "the welcome card is segment 0");

        chat.handle(UiEvent::TurnStart);
        chat.handle(UiEvent::TextDelta("| a | b |".into()));
        chat.build_rows(80);
        assert_eq!(chat.doc.settled, 0, "streaming content is not settled");
        assert!(
            !chat.doc.rows.is_empty(),
            "but still renders in the dynamic tail"
        );
        chat.advance_flushed();
        assert_eq!(
            chat.flushed_segments, welcome_segments,
            "the cursor does not move"
        );

        chat.handle(UiEvent::TurnEnd);
        chat.build_rows(80);
        assert_eq!(
            chat.doc.settled,
            chat.doc.rows.len(),
            "everything settles after the turn ends"
        );
        chat.advance_flushed();
        assert_eq!(
            chat.flushed_segments,
            welcome_segments + 1,
            "the message is flushed"
        );
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
        assert_eq!(chat.flushed_segments, 0, "the cursor resets");
        assert!(chat.dirty, "a rebuild after the reset");
        chat.build_rows(80);
        assert!(
            chat.doc
                .rows
                .iter()
                .any(|r| r.line.plain_text().contains("bingo")),
            "the welcome card reappears"
        );
    }

    /// An AskUserQuestion answer is an ordinary user message: it enters the message flow, settles like a normal message,
    /// and flushes (the segment count advances) — no longer a transient block rendered above the input box.
    #[test]
    fn ask_answer_message_flushes_like_normal_message() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::User, "hi"));
        chat.build_rows(80);
        chat.advance_flushed();
        assert_eq!(chat.flushed_segments, 2, "welcome card + the user's input");

        // Answer one question (through the real event path).
        let (tx, _rx) = oneshot::channel();
        let mut request =
            PermissionRequest::new("Tech stack", "Which library?", vec!["A".into(), "B".into()]);
        request.free_text = true;
        chat.pending_ask = Some((request, tx));
        chat.ask_focus = 0;
        assert!(
            chat.ask_key(KeyCode::Enter, KeyModifiers::empty()),
            "Enter selects A"
        );
        assert!(chat.pending_ask.is_none(), "the dialog closed");

        // The answer enters the message flow as a user message.
        let answer = chat
            .messages
            .last()
            .expect("the answer message entered the flow");
        assert_eq!(answer.role, Role::User, "the answer is a user message");
        assert!(
            answer.text.contains("User answered the questions:"),
            "{}",
            answer.text
        );
        assert!(
            answer.text.contains("· Which library? → A"),
            "{}",
            answer.text
        );
        // Settles and flushes like a normal message: the cursor advances by message segment.
        chat.build_rows(80);
        assert_eq!(
            chat.doc.settled,
            chat.doc.rows.len(),
            "the answer message is settled"
        );
        chat.advance_flushed();
        assert_eq!(
            chat.flushed_segments, 3,
            "the welcome card + hi + the answer message all flush"
        );
    }

    /// Answer messages persist with the session: TurnEnd no longer clears them (they used to be in-turn transient blocks,
    /// vanishing at the turn end; now they are part of the message flow).
    #[test]
    fn ask_answer_message_persists_across_turn_end() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::User, "hi"));
        let (tx, _rx) = oneshot::channel();
        let mut request =
            PermissionRequest::new("Tech stack", "Which library?", vec!["A".into(), "B".into()]);
        request.free_text = true;
        chat.pending_ask = Some((request, tx));
        chat.ask_focus = 1;
        assert!(
            chat.ask_key(KeyCode::Enter, KeyModifiers::empty()),
            "Enter selects B"
        );

        chat.handle(UiEvent::TurnEnd);
        let answer = chat
            .messages
            .last()
            .expect("the answer message is still there");
        assert_eq!(
            answer.role,
            Role::User,
            "the turn end does not clear the answer message"
        );
        assert!(
            answer.text.contains("· Which library? → B"),
            "{}",
            answer.text
        );
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
            "the answer still renders in the message flow: {joined}"
        );
    }

    /// Answering mid-turn ends the assistant message and opens a fresh one: everything the model
    /// does next belongs *below* what the user just said. Before this, `stream_msg` kept pointing
    /// at the message above the answer, so the continuation rendered on top of it and the answer
    /// sat pinned at the bottom of the transcript until the turn ended (#28).
    #[test]
    fn answer_mid_turn_opens_a_new_message_for_the_continuation() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::User, "hi"));
        chat.handle(UiEvent::TurnStart);
        chat.handle(UiEvent::TextDelta("before".into()));
        answer_pending_ask(&mut chat);
        assert_eq!(
            chat.messages.len(),
            4,
            "hi + what the model said before asking + the answer + the continuation"
        );
        assert_eq!(
            chat.stream_msg,
            Some(3),
            "the stream moved below the answer"
        );

        chat.handle(UiEvent::TextDelta("after".into()));
        assert_eq!(chat.messages[1].text, "before");
        assert_eq!(chat.messages[2].role, Role::User, "the answer");
        assert_eq!(
            chat.messages[3].text, "after",
            "the continuation lands under the answer, not above it"
        );
    }

    /// The message the answer closed has to be able to settle. AskUserQuestion is a hidden tool,
    /// so `ToolStart` never closed the placeholder thinking block TurnStart opened — left running
    /// it would hold the settle prefix (and every flush after it) for the rest of the session.
    #[test]
    fn the_message_an_answer_closed_settles_without_waiting_for_the_turn() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::User, "hi"));
        chat.handle(UiEvent::TurnStart);
        chat.handle(UiEvent::ThinkingDelta("weighing it up".into()));
        answer_pending_ask(&mut chat);

        chat.build_rows(80);
        assert!(chat.message_settled(0), "the leading user message");
        assert!(
            chat.message_settled(1),
            "the pre-answer message is finished: nothing more can be added to it"
        );
        assert!(chat.message_settled(2), "the answer");
        assert!(
            !chat.message_settled(3),
            "the continuation is still streaming"
        );

        chat.handle(UiEvent::TurnEnd);
        chat.build_rows(80);
        assert_eq!(
            chat.doc.settled,
            chat.doc.rows.len(),
            "everything settles after the turn ends"
        );
    }

    /// The turn ends right after the answer: the continuation message never received anything,
    /// and an empty assistant block would render as a stray gap.
    #[test]
    fn an_unused_continuation_message_is_dropped_at_turn_end() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::User, "hi"));
        chat.handle(UiEvent::TurnStart);
        chat.handle(UiEvent::TextDelta("before".into()));
        answer_pending_ask(&mut chat);
        assert_eq!(chat.messages.len(), 4);

        chat.handle(UiEvent::TurnEnd);
        assert_eq!(
            chat.messages.len(),
            3,
            "the empty continuation is dropped: hi + the model's text + the answer"
        );
        assert_eq!(chat.messages[2].role, Role::User, "the answer is last");
        chat.build_rows(80);
        chat.advance_flushed();
        assert_eq!(
            chat.flushed_segments, 4,
            "welcome card + hi + the model's text + the answer all flush"
        );
    }

    /// A tool call still in flight owns activity indices in the current message
    /// (`pending_tools`), so its rows must keep landing there: the split waits.
    #[test]
    fn a_tool_in_flight_pins_the_stream_to_its_own_message() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::User, "hi"));
        chat.handle(UiEvent::TurnStart);
        chat.handle(UiEvent::ToolStart {
            name: "Read".into(),
        });
        answer_pending_ask(&mut chat);
        assert_eq!(chat.stream_msg, Some(1), "the stream stays with the tool");
        assert_eq!(chat.messages.len(), 3, "hi + assistant + answer");
    }

    /// Answers a pending free-text question by confirming its first option.
    fn answer_pending_ask(chat: &mut Chat) {
        let (tx, _rx) = oneshot::channel();
        let mut request = PermissionRequest::new("Tech stack", "Which library?", vec!["A".into()]);
        request.free_text = true;
        chat.pending_ask = Some((request, tx));
        chat.ask_focus = 0;
        assert!(
            chat.ask_key(KeyCode::Enter, KeyModifiers::empty()),
            "select A"
        );
    }

    /// The error path never reaches TurnEnd (start_turn's `Err(e)` only emits UiEvent::Error):
    /// the answer message stays in the message flow — the old transient block was never cleaned up on the error path and hung until
    /// /clear (the regression path of a pre-24ba4d9 bug); an ordinary message has no state to clear, so it is fixed by design.
    #[test]
    fn ask_answer_message_survives_error_path() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::User, "hi"));
        let (tx, _rx) = oneshot::channel();
        let mut request =
            PermissionRequest::new("Tech stack", "Which library?", vec!["A".into(), "B".into()]);
        request.free_text = true;
        chat.pending_ask = Some((request, tx));
        chat.ask_focus = 0;
        assert!(
            chat.ask_key(KeyCode::Enter, KeyModifiers::empty()),
            "select A"
        );

        chat.handle(UiEvent::Error {
            code: "SERVER_ERROR",
            msg: "turn failed".to_string(),
            level: crate::error::ErrorLevel::Full,
            context: crate::error::ErrorContext::LongTurn,
        });
        // The answer message stays in the flow and renders as usual.
        let answer = chat
            .messages
            .last()
            .expect("the answer message is still there");
        assert_eq!(answer.role, Role::User);
        assert!(
            answer.text.contains("· Which library? → A"),
            "{}",
            answer.text
        );
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
            "the answer still renders after the error: {joined}"
        );
    }

    /// The ordering guard must be linear: build_rows' settling decision under full settling (hundreds of messages)
    /// must not blow up exponentially (regression: a per-prefix recursive evaluation froze at ~40 messages).
    #[test]
    fn message_settled_guard_is_linear_for_large_settled_sessions() {
        let mut chat = test_chat();
        for _ in 0..400 {
            chat.messages.push(msg(Role::User, "hi"));
            chat.messages.push(msg(Role::Assistant, "ok"));
        }
        // Fully static settling: build_rows decides settling for every message.
        chat.build_rows(80);
        assert_eq!(chat.doc.settled, chat.doc.rows.len(), "everything settles");
        for i in 0..chat.messages.len() {
            assert!(chat.message_settled(i), "message {i} settles");
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
        assert!(welcome > 0, "the welcome card flushes");

        chat.messages
            .push(msg(Role::User, "please explain this code"));
        flush_frame(&mut chat, 100, &mut printed);
        chat.handle(UiEvent::TurnStart);
        for chunk in [
            "First paragraph.\n\n",
            "## Heading\n\n",
            "- item one\n",
            "- item two\n",
        ] {
            chat.handle(UiEvent::TextDelta(chunk.into()));
            flush_frame(&mut chat, 100, &mut printed);
        }
        // Mid-turn resize: all row numbers change after re-layout.
        flush_frame(&mut chat, 60, &mut printed);
        chat.handle(UiEvent::TextDelta("ending.".into()));
        chat.handle(UiEvent::TurnEnd);
        flush_frame(&mut chat, 60, &mut printed);
        // Idling a few frames must print nothing more.
        let after = printed.len();
        for _ in 0..3 {
            flush_frame(&mut chat, 60, &mut printed);
        }
        assert_eq!(printed.len(), after, "no new flushes");

        // The welcome card itself has repeated padded rows; deduping by content would false-positive — check only the message part.
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for line in &printed[welcome..] {
            if line.trim().is_empty() {
                continue;
            }
            *seen.entry(line.as_str()).or_default() += 1;
        }
        for (line, count) in &seen {
            assert_eq!(*count, 1, "row flushed {count} times: {line:?}");
        }
        let joined = printed.join("\n");
        assert!(
            joined.contains("please explain this code"),
            "the user message is flushed"
        );
        assert!(joined.contains("ending."), "settled body flushes");
        assert!(
            chat.doc.rows.is_empty(),
            "the tail is empty after everything flushes"
        );
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
        chat.messages.push(msg(Role::Assistant, "reply"));
        chat.build_rows(80);
        chat.advance_flushed();
        chat.build_rows(80);
        assert!(
            chat.doc.rows.is_empty(),
            "the tail is empty after everything flushes"
        );
        assert!(chat.expand_transcript());
        assert!(chat.dump_transcript);
        assert!(
            chat.force_redraw,
            "the replay frame first clears the visible screen"
        );
        chat.build_rows(80);
        let text: String = chat
            .doc
            .rows
            .iter()
            .map(|row| row.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("reply"),
            "the replay document includes flushed messages: {text}"
        );

        // Historical messages with collapse groups → everything expands before the replay.
        chat.dump_transcript = false;
        start_group(&mut chat);
        let _ = chat
            .events
            .send(UiEvent::ToolDone(crate::query::ToolCallDone {
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
            "all fold groups expanded"
        );

        // Fully expanded → the second press goes the collapse direction: back to aggregates (the app layer
        // handles the clear-redraw + rehydration to close it up).
        assert!(chat.transcript_fully_expanded());
        assert!(chat.collapse_transcript());
        assert!(
            chat.messages
                .iter()
                .flat_map(|m| &m.groups)
                .all(|g| !g.expanded),
            "all fold groups closed"
        );
        assert!(
            !chat.transcript_fully_expanded(),
            "after closing, it returns to the expand direction"
        );
        assert!(
            !chat.collapse_transcript(),
            "already fully closed; closing again changes nothing"
        );
    }

    /// The tick does not set dirty when idle (no doc rebuild); it does when dynamic elements exist.
    #[test]
    fn tick_marks_dirty_only_when_dynamic() {
        let mut chat = test_chat();
        chat.dirty = false;
        chat.tick();
        assert!(!chat.dirty, "no rebuild when idle");
        assert!(!chat.needs_tick(), "idle does not wake components");
        chat.busy = true;
        chat.tick();
        assert!(chat.dirty, "rebuilds while busy (spinner/duration row)");
        assert!(chat.needs_tick());
        // Pending events must also wake it up (otherwise they would never drain).
        chat.busy = false;
        chat.dirty = false;
        let _ = chat.events.send(UiEvent::Warning("w".into()));
        assert!(chat.needs_tick(), "pending events need a wake-up");

        let mut slash_error = test_chat();
        slash_error.push_slash_error("unknown command; type /help.".to_string());
        assert!(
            slash_error.needs_tick(),
            "even when idle, a slash error must drive the tick so the error TTL auto-expires it"
        );
        slash_error.slash_error_at = Some(
            std::time::Instant::now()
                - SLASH_OUTPUT_ERROR_TTL
                - std::time::Duration::from_millis(1),
        );
        slash_error.tick();
        assert!(
            slash_error.slash_error_lines.is_empty(),
            "cleared after the error TTL expires"
        );
        assert!(
            !slash_error.needs_tick(),
            "after clearing, the host returns to true idle"
        );
    }

    #[test]
    fn settled_tracks_streaming_message() {
        let mut chat = test_chat();
        chat.build_rows(100);
        let welcome = chat.doc.settled;
        assert!(welcome > 0, "welcome card rows are settled");
        assert_eq!(
            chat.doc.settled,
            chat.doc.rows.len(),
            "empty session fully settled"
        );
        // Turn start: streaming message + placeholder thinking → must not settle.
        chat.handle(UiEvent::TurnStart);
        chat.build_rows(100);
        assert_eq!(chat.doc.settled, welcome, "streaming message not settled");
        assert!(chat.doc.rows.len() > welcome, "streaming message rendered");
        // Turn end: the message settles, all rows enter settled.
        chat.handle(UiEvent::TurnEnd);
        chat.build_rows(100);
        assert_eq!(
            chat.doc.settled,
            chat.doc.rows.len(),
            "all rows settled after turn"
        );
        // Settling is one-way: a second message (streaming) does not move existing boundaries.
        let after_turn = chat.doc.settled;
        chat.handle(UiEvent::TurnStart);
        chat.build_rows(100);
        assert_eq!(
            chat.doc.settled, after_turn,
            "new turn keeps prior settled boundary"
        );
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
        assert_eq!(
            chat.doc.settled, welcome,
            "running tool keeps message dynamic"
        );
        // Tool done → settles.
        let a = &mut chat.messages[0].activities[0];
        match &mut a.kind {
            ActivityKind::Tool(t) => t.status = ToolStatus::Done,
            _ => panic!("tool activity expected"),
        }
        chat.build_rows(100);
        assert_eq!(
            chat.doc.settled,
            chat.doc.rows.len(),
            "settled after tool done"
        );
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
            PermissionRequest::new("Allow running Bash", "cargo build", vec!["Allow".into()]),
            tx,
        ));
        chat.build_rows(100);
        assert_eq!(chat.doc.settled, welcome, "ask block not settled");
        // Turn end + request resolved → everything settles.
        chat.pending_ask = None;
        chat.handle(UiEvent::TurnEnd);
        chat.build_rows(100);
        assert_eq!(
            chat.doc.settled,
            chat.doc.rows.len(),
            "all settled after ask done"
        );
    }

    #[test]
    fn permission_request_renders_with_clickable_options() {
        let mut chat = test_chat();
        let (tx, _rx) = oneshot::channel();
        chat.pending_ask = Some((
            PermissionRequest::new(
                "Allow running Bash",
                "cargo build",
                vec!["Allow".into(), "Deny".into()],
            ),
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
        assert!(joined.contains("Allow running Bash"), "title: {joined}");
        assert!(
            joined.contains("❯ 1. Allow"),
            "focused first option: {joined}"
        );
        assert!(joined.contains("2. Deny"), "option row: {joined}");
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
            PermissionRequest::new("Tech stack", "Which library?", vec!["A".into(), "B".into()]);
        request.free_text = true;
        request.descriptions = vec![None, Some("faster".to_string())];
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
        assert!(joined.contains("  faster"), "desc dim row: {joined}");
        assert!(joined.contains("3. Other"), "other option: {joined}");
        assert!(joined.contains("Type something."), "placeholder: {joined}");
        assert!(
            chat.ask_key(KeyCode::Char('3'), KeyModifiers::empty()),
            "digit 3 → Other"
        );
        chat.build_rows(100);
        let joined: String = chat
            .doc
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("❯ 3. Other"), "other focused: {joined}");
        assert!(
            joined.contains("enter to submit · esc to cancel"),
            "input hint: {joined}"
        );
        for c in ['s', 'e', 'r', 'd', 'e'] {
            assert!(
                chat.ask_key(KeyCode::Char(c), KeyModifiers::empty()),
                "type {c}"
            );
        }
        assert!(
            chat.ask_key(KeyCode::Enter, KeyModifiers::empty()),
            "submit"
        );
        assert!(chat.pending_ask.is_none(), "dialog closed");
        assert_eq!(rx.try_recv(), Ok(DialogAction::Answer("serde".to_string())));
        // The answer enters the message flow: an ordinary user message (Q&A echo).
        let answer = chat
            .messages
            .last()
            .expect("the answer message entered the flow");
        assert_eq!(answer.role, Role::User);
        assert_eq!(
            answer.text,
            "User answered the questions:\n  · Which library? → serde"
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
            joined.contains("· Which library? → serde"),
            "result line: {joined}"
        );
        assert!(
            joined.contains("❯ "),
            "the answer renders as a user bubble: {joined}"
        );
    }

    #[test]
    fn ask_other_empty_submit_cancels() {
        let mut chat = test_chat();
        let (tx, mut rx) = oneshot::channel();
        let mut request =
            PermissionRequest::new("Tech stack", "Which library?", vec!["A".into(), "B".into()]);
        request.free_text = true;
        chat.pending_ask = Some((request, tx));
        chat.ask_focus = 2;
        assert!(
            chat.ask_key(KeyCode::Enter, KeyModifiers::empty()),
            "empty Other submit"
        );
        assert!(chat.pending_ask.is_none());
        assert_eq!(rx.try_recv(), Ok(DialogAction::Cancel));
        // A decline also enters the message flow (an ordinary user message).
        let declined = chat
            .messages
            .last()
            .expect("the decline message entered the flow");
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
        assert!(
            joined.contains("User declined to answer questions"),
            "{joined}"
        );
    }

    #[test]
    fn ask_arrow_keys_move_focus() {
        let mut chat = test_chat();
        let (tx, mut rx) = oneshot::channel();
        let mut request =
            PermissionRequest::new("Tech stack", "Which library?", vec!["A".into(), "B".into()]);
        request.free_text = true;
        chat.pending_ask = Some((request, tx));
        assert!(chat.ask_key(KeyCode::Down, KeyModifiers::empty()), "↓ to B");
        assert_eq!(chat.ask_focus, 1);
        assert!(
            chat.ask_key(KeyCode::Down, KeyModifiers::empty()),
            "↓ to Other"
        );
        assert_eq!(chat.ask_focus, 2);
        assert!(
            chat.ask_key(KeyCode::Down, KeyModifiers::empty()),
            "↓ at the bottom stops moving"
        );
        assert_eq!(chat.ask_focus, 2);
        assert!(
            chat.ask_key(KeyCode::Up, KeyModifiers::empty()),
            "↑ back to B"
        );
        assert_eq!(chat.ask_focus, 1);
        assert!(
            chat.ask_key(KeyCode::Enter, KeyModifiers::empty()),
            "Enter selects B"
        );
        assert_eq!(rx.try_recv(), Ok(DialogAction::Confirm(1)));
        let answer = chat
            .messages
            .last()
            .expect("the answer message entered the flow");
        assert_eq!(answer.role, Role::User);
        assert!(
            answer.text.contains("· Which library? → B"),
            "the option text is the answer: {}",
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
            "busy Esc interrupts"
        );
        assert!(chat.interrupted, "Esc sets interrupted");
        assert!(
            *chat.cancel_tx.borrow(),
            "the interrupt signal was sent (send_replace applies unconditionally)"
        );
        chat.busy = false;
        chat.interrupted = false;
        chat.busy = true;
        let _ = chat.cancel_tx.send_replace(true);
        let cancel_rx = chat.cancel_tx.subscribe();
        chat.cancel_tx.send_replace(false);
        assert!(
            !*cancel_rx.borrow(),
            "reset before a new turn starts: the receiver reads false"
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
            "after all receivers drop, send_replace still resets (send would fail)"
        );
    }

    #[test]
    fn image_ready_updates_cache_and_invalidates_render_cache() {
        let mut chat = test_chat();
        chat.reply_cache
            .insert("x".to_string(), vec![Line::plain("old")]);
        let meta = ImageMeta {
            cols: 5,
            rows: 3,
            bytes: vec![1, 2, 3],
        };
        chat.handle(UiEvent::ImageReady {
            url: "a.png".to_string(),
            meta: Some(meta.clone()),
        });
        assert!(
            chat.images.contains_key("a.png"),
            "a successful load lands in the cache"
        );
        assert_eq!(chat.images["a.png"].cols, 5);
        assert_eq!(
            chat.images_version, 2,
            "the version increments (starts at 1)"
        );
        assert!(chat.reply_cache.is_empty(), "reply_cache invalidated");

        chat.handle(UiEvent::ImageReady {
            url: "a.png".to_string(),
            meta: None,
        });
        assert!(
            !chat.images.contains_key("a.png"),
            "a failure removes the cache entry"
        );
        assert!(
            chat.warnings.iter().any(|(_, w)| w.contains("a.png")),
            "warning hint"
        );
    }

    #[test]
    fn turn_end_without_capability_skips_image_loading() {
        let mut chat = test_chat();
        chat.apply_turn_start();
        chat.handle(UiEvent::TextDelta(
            "![img](https://example.com/i.png)".to_string(),
        ));
        chat.handle(UiEvent::TurnEnd);
        assert!(chat.images.is_empty(), "no capability → not loaded");
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
        chat.handle(UiEvent::TextDelta(format!("![img]({url})")));
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
        assert!(
            chat.images_pending.is_empty(),
            "the in-flight set is cleared"
        );
        chat.build_rows(100);
        let image_rows = chat
            .doc
            .rows
            .iter()
            .filter(|r| r.line.image.is_some())
            .count();
        assert!(image_rows > 0, "image-block rows appear in the document");
        let meta = &chat.images[&url];
        assert_eq!(image_rows, meta.rows, "block rows = meta.rows");
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
        chat.messages
            .push(msg(Role::Assistant, &format!("![img]({url})")));
        // Load in flight (the effect of load_message_images).
        chat.images_pending.insert(url.clone());
        chat.build_rows(100);
        assert_eq!(
            settled_segments(&chat),
            1,
            "only the welcome card settles; a message with in-flight images does not"
        );

        // Load succeeds → the message settles, and flushed rows carry an ImageRef (the block head emits the kitty sequence).
        let meta = ImageMeta {
            cols: 4,
            rows: 2,
            bytes: tiny_png(),
        };
        chat.handle(UiEvent::ImageReady {
            url: url.clone(),
            meta: Some(meta),
        });
        chat.build_rows(100);
        assert_eq!(
            settled_segments(&chat),
            2,
            "the message settles once the image is ready"
        );
        let image_rows: Vec<&Row> = chat
            .doc
            .rows
            .iter()
            .take(chat.doc.settled)
            .filter(|r| r.line.image.is_some())
            .collect();
        assert!(!image_rows.is_empty(), "settled rows contain image blocks");
    }

    /// A failed load (including None from a timeout) also releases the block:
    /// it settles with a failure-marked placeholder, distinguishable from a
    /// still-loading one.
    #[test]
    fn failed_image_load_settles_with_placeholder() {
        let mut chat = test_chat();
        chat.image_cap = Some(ImageCap::default_cells());
        chat.messages
            .push(msg(Role::Assistant, "![img](missing.png)"));
        chat.images_pending.insert("missing.png".to_string());
        chat.build_rows(100);
        assert_eq!(
            settled_segments(&chat),
            1,
            "does not settle while in flight"
        );
        chat.handle(UiEvent::ImageReady {
            url: "missing.png".to_string(),
            meta: None,
        });
        chat.build_rows(100);
        assert_eq!(
            settled_segments(&chat),
            2,
            "settles normally after a failure"
        );
        let text: String = chat
            .doc
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("#[image ✗ load failed]"),
            "the failure marker lands in the settled text: {text}"
        );
    }

    /// Without image capability, nothing enters the in-flight set and messages settle immediately (unchanged behavior).
    #[test]
    fn without_image_capability_messages_settle_immediately() {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, "![img](a.png)"));
        chat.build_rows(100);
        assert!(chat.images_pending.is_empty());
        assert_eq!(
            settled_segments(&chat),
            2,
            "no capability → does not wait for images"
        );
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
        assert_eq!(chat.cursor, 0, "ctrl+a to the line start");
        assert!(press(&mut chat, KeyCode::Right));
        press(&mut chat, KeyCode::Char('i'));
        assert_eq!(chat.input, "hiello world", "inserts at the cursor");
        assert!(ctrl(&mut chat, 'e'));
        assert_eq!(chat.cursor, chat.input.len(), "ctrl+e to the line end");
        assert!(alt(&mut chat, 'b'));
        assert_eq!(chat.cursor, "hiello ".len(), "alt+b back one word");
        assert!(alt(&mut chat, 'f'));
        assert_eq!(chat.cursor, chat.input.len(), "alt+f forward one word");
        // CJK moves by character and renders by display width.
        chat.set_input("ＡＢ");
        press(&mut chat, KeyCode::Left);
        assert_eq!(chat.cursor, 3, "one char back at a time (3 bytes)");
    }

    /// ctrl+k/u/w delete into the kill buffer, ctrl+y pastes back; ctrl+d deletes after the caret.
    #[test]
    fn kill_ring_round_trip() {
        let mut chat = chat_with_history("kill");
        type_text(&mut chat, "alpha beta");
        assert!(ctrl(&mut chat, 'w'));
        assert_eq!(chat.input, "alpha ");
        assert!(ctrl(&mut chat, 'y'));
        assert_eq!(chat.input, "alpha beta", "ctrl+y pastes back");
        assert!(ctrl(&mut chat, 'a'));
        assert!(ctrl(&mut chat, 'k'));
        assert_eq!(chat.input, "", "ctrl+k deletes to the line end");
        assert!(ctrl(&mut chat, 'y'));
        assert_eq!(chat.input, "alpha beta");
        assert!(ctrl(&mut chat, 'u'));
        assert_eq!(chat.input, "", "ctrl+u deletes to the line start");
        chat.set_input("abc");
        chat.cursor = 1;
        assert!(ctrl(&mut chat, 'd'));
        assert_eq!(chat.input, "ac", "ctrl+d deletes the char after the cursor");
    }

    /// History: submitted entries go into history and persist; ↑/↓ navigate, back at the bottom restores the draft;
    /// consecutive identical prompts are recorded once.
    #[test]
    fn prompt_history_persists_and_navigates() {
        let mut chat = chat_with_history("history");
        chat.record_history("first");
        chat.record_history("second");
        chat.record_history("second");
        assert_eq!(
            chat.history.entries(),
            ["first", "second"],
            "consecutive repeats record once"
        );
        // Persisted: a new session with the same home + cwd can read it.
        let reloaded =
            crate::tui::history::load(&chat.session.home, std::path::Path::new(&chat.cwd));
        assert_eq!(reloaded, vec!["first".to_string(), "second".to_string()]);

        chat.set_input("draft");
        press(&mut chat, KeyCode::Up);
        assert_eq!(chat.input, "second");
        press(&mut chat, KeyCode::Up);
        assert_eq!(chat.input, "first");
        press(&mut chat, KeyCode::Down);
        assert_eq!(chat.input, "second");
        press(&mut chat, KeyCode::Down);
        assert_eq!(chat.input, "draft", "back at the bottom, draft is restored");
        let _ = std::fs::remove_dir_all(&chat.session.home);
    }

    /// Multi-line input: `\`+Enter and ctrl+j insert newlines, Enter submits the whole;
    /// rendered as multiple rows (each height=1, not a single row stuffed with \n).
    #[test]
    fn multiline_input_renders_as_multiple_rows() {
        let mut chat = chat_with_history("multiline");
        chat.width = 80;
        type_text(&mut chat, "first\\");
        assert!(press(&mut chat, KeyCode::Enter), "\\+Enter newline");
        type_text(&mut chat, "second");
        assert!(ctrl(&mut chat, 'j'), "ctrl+j newline");
        type_text(&mut chat, "third");
        assert_eq!(chat.input, "first\nsecond\nthird");
        let rows = chat.prompt_lines();
        assert_eq!(rows.len(), 3, "three lines of input = three Rows");
        for row in &rows {
            assert!(!row.plain_text().contains('\n'), "rows contain no newline");
        }
        assert!(
            rows[2].plain_text().contains('▋'),
            "the caret is drawn on the last row"
        );
        // ↑ moves along visual rows within a multi-line input before switching history.
        chat.record_history("older");
        press(&mut chat, KeyCode::Up);
        assert_eq!(
            chat.input, "first\nsecond\nthird",
            "in-line movement does not touch the text"
        );
        press(&mut chat, KeyCode::Up);
        press(&mut chat, KeyCode::Up);
        assert_eq!(
            chat.input, "older",
            "history switches only at the first line"
        );
        let _ = std::fs::remove_dir_all(&chat.session.home);
    }

    /// The input area has a row cap: long input only shows the screen around the caret.
    #[test]
    fn prompt_rows_are_capped() {
        let mut chat = chat_with_history("caprows");
        chat.width = 40;
        chat.set_input(
            (0..30)
                .map(|i| format!("line{i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
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
        assert!(chat.interrupted, "busy → interrupt");
        assert!(!chat.exit);

        chat.busy = false;
        chat.set_input("draft");
        chat.on_key_at(KeyCode::Char('c'), KeyModifiers::CONTROL, t0);
        assert_eq!(chat.input, "", "non-empty input clears first");
        assert!(!chat.exit, "clearing does not exit");
        assert_eq!(
            chat.history.entries().last().map(String::as_str),
            Some("draft")
        );

        chat.on_key_at(KeyCode::Char('c'), KeyModifiers::CONTROL, t0);
        assert_eq!(chat.notice, Some("Press ctrl-c again to exit"));
        assert!(!chat.exit, "the first time only hints");
        chat.on_key_at(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            t0 + CTRL_C_WINDOW,
        );
        assert!(chat.exit, "the second press inside the window exits");

        // The counter restarts after the window expires.
        let mut chat = chat_with_history("ctrlc2");
        chat.on_key_at(KeyCode::Char('c'), KeyModifiers::CONTROL, t0);
        chat.on_key_at(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            t0 + CTRL_C_WINDOW + std::time::Duration::from_millis(1),
        );
        assert!(!chat.exit, "outside the window: no exit, just a new hint");
        assert_eq!(chat.notice, Some("Press ctrl-c again to exit"));
        let _ = std::fs::remove_dir_all(&chat.session.home);
    }

    /// A turn whose task died never clears `busy`, and every quit route is gated on `busy` —
    /// the session used to answer only to `kill`. An interrupt left unhonoured past
    /// [`INTERRUPT_GRACE`] gives Ctrl+C its exit meaning back.
    #[test]
    fn ctrl_c_force_quits_a_turn_that_never_stops() {
        let mut chat = chat_with_history("wedged");
        let t0 = std::time::Instant::now();
        chat.busy = true;

        chat.on_key_at(KeyCode::Char('c'), KeyModifiers::CONTROL, t0);
        assert!(chat.interrupted, "the first press asks the turn to stop");
        assert!(
            !chat.exit,
            "a turn that may still be stopping is not killed"
        );

        chat.on_key_at(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            t0 + INTERRUPT_GRACE - std::time::Duration::from_millis(1),
        );
        assert!(!chat.exit, "inside the grace it stays an interrupt");

        chat.on_key_at(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            t0 + INTERRUPT_GRACE,
        );
        assert!(chat.exit, "still busy past the grace → exit");
        let _ = std::fs::remove_dir_all(&chat.session.home);
    }

    /// The escape hatch must not fire on a healthy interrupt: the next turn clears the
    /// stamp, so Ctrl+C during it hints and interrupts exactly as before.
    #[test]
    fn a_new_turn_rearms_the_ordinary_interrupt() {
        let mut chat = chat_with_history("rearm");
        let t0 = std::time::Instant::now();
        chat.busy = true;
        chat.on_key_at(KeyCode::Char('c'), KeyModifiers::CONTROL, t0);
        assert!(chat.interrupt_at.is_some());

        chat.apply_event(UiEvent::TurnStart);
        assert_eq!(
            chat.interrupt_at, None,
            "a fresh turn is owed a fresh grace"
        );

        chat.on_key_at(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            t0 + INTERRUPT_GRACE * 10,
        );
        assert!(!chat.exit, "the new turn's first press only interrupts");
        let _ = std::fs::remove_dir_all(&chat.session.home);
    }

    /// A panic inside the turn task is swallowed by tokio (nothing joins the handle) and the
    /// terminal repaints over its message, so the only visible symptom was a session stuck
    /// on "Working…" forever. The supervisor turns it into the ordinary long-turn error.
    #[tokio::test]
    async fn a_lost_turn_reports_itself_instead_of_latching_busy() {
        let (events, mut rx) = mpsc::unbounded_channel();
        // The panic below prints its own backtrace line; that noise is the point — in the
        // TUI it lands on the alternate screen and is repainted away within a frame.
        let handle = tokio::spawn(async { panic!("turn task died") });

        Chat::supervise_turn(events, handle);

        let event = rx.recv().await;
        assert!(
            matches!(
                event,
                Some(UiEvent::Error { code, context, .. })
                    if code == crate::error::TURN_LOST
                        && context == crate::error::ErrorContext::LongTurn
            ),
            "a lost turn reports itself: {event:?}"
        );
    }

    /// Esc: interrupts when busy; closes suggestions/panels layer by layer; double-press with text clears and saves to history.
    #[test]
    fn esc_closes_layers_then_clears_input() {
        let mut chat = chat_with_history("esc");
        let t0 = std::time::Instant::now();
        chat.busy = true;
        chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0);
        assert!(chat.interrupted, "busy → interrupt");

        chat.busy = false;
        chat.set_input("/");
        assert!(!chat.slash_suggestions.is_empty());
        chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0);
        assert!(
            chat.slash_suggestions.is_empty(),
            "the dropdown closes first"
        );
        assert_eq!(
            chat.input, "",
            "the slash query clears with the dropdown (no more leftover //)"
        );

        chat.set_input("hello");
        chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0);
        assert_eq!(chat.input, "hello", "the first press only arms it");
        assert_eq!(chat.notice, Some("Press esc again to clear"));
        chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0);
        assert_eq!(chat.input, "", "double-press clears");
        assert_eq!(
            chat.history.entries().last().map(String::as_str),
            Some("hello")
        );
        let _ = std::fs::remove_dir_all(&chat.session.home);
    }

    /// Shift+Tab cycles the permission mode, and it really applies to the next turn's Session.
    #[test]
    fn shift_tab_cycles_permission_mode() {
        let mut chat = chat_with_history("mode");
        assert_eq!(chat.permission_mode, PermissionMode::Default);
        press(&mut chat, KeyCode::BackTab);
        assert_eq!(chat.permission_mode, PermissionMode::AcceptEdits);
        assert_eq!(
            chat.permission_mode_label(),
            "acceptEdits",
            "the footer badge shares a source"
        );
        press(&mut chat, KeyCode::BackTab);
        assert_eq!(chat.permission_mode, PermissionMode::Plan);
        press(&mut chat, KeyCode::BackTab);
        assert_eq!(
            chat.permission_mode,
            PermissionMode::Default,
            "cycles back to default"
        );
        // The turn's Session carries the current mode (Session is immutable in Arc → derive a copy).
        press(&mut chat, KeyCode::BackTab);
        assert_eq!(
            chat.session_for_turn().permission_mode,
            PermissionMode::AcceptEdits
        );
        assert_eq!(
            chat.session.permission_mode,
            PermissionMode::Default,
            "the original Session is unchanged"
        );

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
        assert_eq!(
            chat.queued,
            vec![QueuedInput {
                text: "first queued".into(),
                is_slash: false
            }]
        );
        assert_eq!(chat.input, "", "the input clears after enqueueing");
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

    /// Busy dispatch (contract §4.2): instant commands run immediately and never reset
    /// `busy`; other slash commands queue with the slash marker; plain messages queue.
    #[test]
    fn busy_dispatch_runs_instant_and_queues_the_rest() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-busy", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mut chat = test_chat_home(tmp.join("home"));
        chat.cwd = tmp.display().to_string();
        chat.busy = true;

        // Instant: /think xhigh applies now, not queued; busy stays true.
        chat.set_input("/think xhigh");
        chat.submit();
        assert_eq!(
            chat.session.runtime.thinking.borrow().as_deref(),
            Some("xhigh"),
            "whitelisted commands apply immediately while busy"
        );
        assert!(chat.busy, "the whitelist path does not reset busy");
        assert!(chat.queued.is_empty(), "whitelisted commands do not queue");
        let out = chat.slash_lines.join("\n");
        assert!(out.contains("✓ thinking level set: xhigh"), "{out}");

        chat.set_input("/model deepseek-v4");
        chat.submit();
        assert_eq!(*chat.session.runtime.model.borrow(), "test-model");
        assert!(
            chat.slash_error_lines
                .join("\n")
                .contains("cannot switch models mid-turn")
        );

        // Non-instant slash: queued with the slash marker (never sent as a prompt).
        chat.set_input("/clear");
        chat.submit();
        assert_eq!(
            chat.queued,
            vec![QueuedInput {
                text: "/clear".into(),
                is_slash: true
            }],
            "non-whitelisted slash commands queue with a marker"
        );

        // Plain message: queued without the marker.
        chat.set_input("hello");
        chat.submit();
        assert_eq!(chat.queued.len(), 2);
        assert!(!chat.queued[1].is_slash);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// After TurnEnd, queued slash commands drain through `run_slash` (not `start_turn`),
    /// in order, until a plain message starts the next turn.
    #[tokio::test]
    async fn queued_slashes_drain_through_run_slash() {
        let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-drain", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let mut chat = test_chat_home(tmp.join("home"));
        chat.cwd = tmp.display().to_string();
        chat.queued = vec![
            QueuedInput {
                text: "/think low".into(),
                is_slash: true,
            },
            QueuedInput {
                text: "/nope".into(),
                is_slash: true,
            },
            QueuedInput {
                text: "the message".into(),
                is_slash: false,
            },
        ];
        chat.submit_queued();
        // Both slash commands ran (think applied + unknown guidance), then the message started a turn.
        assert_eq!(
            chat.session.runtime.thinking.borrow().as_deref(),
            Some("low"),
            "queued slash commands run as commands"
        );
        let out = all_slash_text(&chat);
        assert!(
            out.contains("unknown command: /nope") && out.contains("code=UNKNOWN_COMMAND"),
            "unknown commands get guidance instead of the model: {out}"
        );
        assert!(chat.busy, "the last plain message starts a new turn");
        assert_eq!(chat.messages.last().map(|m| m.role), Some(Role::User));
        assert!(
            chat.messages
                .last()
                .is_some_and(|m| m.text == "the message"),
            "plain messages reach the model via start_turn"
        );
        let _ = std::fs::remove_dir_all(&tmp);
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
        assert!(chat.notice.is_some(), "empty-state hint");
        assert!(chat.entity_focus.is_none());
        // Create one agent instance + one channel.
        chat.session.agents.insert(
            "scout",
            crate::agents::AgentKind::Hire,
            None,
            "research".into(),
            chat.session.clone(),
        );
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
        assert!(
            joined
                .last()
                .unwrap_or(&String::new())
                .contains("enter opens")
        );
        // ↓ to the channel, Enter opens it.
        assert!(chat.on_key(KeyCode::Down, KeyModifiers::empty()));
        assert!(chat.on_key(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(
            chat.open_entity,
            Some(EntityOpen::Channel("table".into())),
            "channel selected"
        );
        assert!(chat.entity_focus.is_none(), "focus exits after opening");
        // After refocusing, Esc only closes the selector (does not trigger global Esc semantics).
        let _ = chat.on_key(KeyCode::Char('g'), KeyModifiers::CONTROL);
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(chat.entity_focus.is_none());
    }

    /// Queues beyond the cap fold into one row (row count feeds chrome, so it must be bounded).
    #[test]
    fn queue_lines_are_capped() {
        let mut chat = chat_with_history("queuecap");
        chat.queued = (0..10)
            .map(|i| QueuedInput {
                text: format!("m{i}"),
                is_slash: false,
            })
            .collect();
        assert_eq!(chat.queue_lines().len(), QUEUE_ROWS_MAX + 1);
        assert!(
            chat.queue_lines()
                .last()
                .is_some_and(|l| l.contains("more queued"))
        );
    }

    /// `?`: toggles the panel on empty input; an ordinary character otherwise.
    #[test]
    fn question_mark_toggles_help_panel() {
        let mut chat = chat_with_history("help");
        chat.width = 100;
        chat.height = 40;
        press(&mut chat, KeyCode::Char('?'));
        assert!(chat.help_visible);
        assert!(!chat.help_lines().is_empty(), "the panel has content");
        assert!(chat.input.is_empty(), "? does not enter the input");
        press(&mut chat, KeyCode::Char('?'));
        assert!(!chat.help_visible, "pressed again, closes");
        assert!(chat.help_lines().is_empty());
        type_text(&mut chat, "why");
        press(&mut chat, KeyCode::Char('?'));
        assert_eq!(chat.input, "why?", "with text present it is a plain char");
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
        assert!(
            short < tall,
            "short terminals get a shorter panel: {short} vs {tall}"
        );
        assert!(
            short + 9 <= 14,
            "the panel + remaining chrome fit within the terminal height"
        );
        chat.height = 6;
        assert!(
            chat.help_lines().is_empty(),
            "very short terminals show no panel"
        );
    }

    /// ctrl+s stash/restore (with the caret), ctrl+_ undo, ctrl+t task area, ctrl+l repaint.
    #[test]
    fn stash_undo_tasks_and_redraw() {
        let mut chat = chat_with_history("t2");
        type_text(&mut chat, "stashed");
        chat.cursor = 3;
        assert!(ctrl(&mut chat, 's'));
        assert_eq!(chat.input, "", "ctrl+s stashes and clears");
        assert!(ctrl(&mut chat, 's'));
        assert_eq!(
            (chat.input.as_str(), chat.cursor),
            ("stashed", 3),
            "the restore includes the caret"
        );

        // Undo: a bulk edit (kill) steps back one.
        chat.set_input("undo me");
        chat.cursor = chat.input.len();
        assert!(ctrl(&mut chat, 'w'));
        assert_eq!(chat.input, "undo ");
        assert!(ctrl(&mut chat, '7'), "ctrl+_ arrives as ctrl+7");
        assert_eq!(chat.input, "undo me", "undo returns to before the deletion");

        assert!(!chat.tasks_visible);
        assert!(ctrl(&mut chat, 't'));
        assert!(chat.tasks_visible, "ctrl+t shows the task area");
        assert!(ctrl(&mut chat, 't'));
        assert!(!chat.tasks_visible);

        assert!(ctrl(&mut chat, 'l'));
        assert!(chat.force_redraw, "ctrl+l requests a full-screen redraw");
    }

    /// bash mode: empty-input Esc/backspace/ctrl+u exit; Tab completes from this session's `!` history.
    #[test]
    fn bash_mode_exits_and_completes() {
        let mut chat = chat_with_history("bash");
        chat.bash_history.push("cargo test --all".to_string());
        press(&mut chat, KeyCode::Char('!'));
        assert!(chat.bash_mode);
        press(&mut chat, KeyCode::Esc);
        assert!(!chat.bash_mode, "empty input + Esc exits shell mode");
        press(&mut chat, KeyCode::Char('!'));
        assert!(ctrl(&mut chat, 'u'));
        assert!(!chat.bash_mode, "empty input + ctrl+u exits");
        press(&mut chat, KeyCode::Char('!'));
        type_text(&mut chat, "cargo");
        press(&mut chat, KeyCode::Tab);
        assert_eq!(chat.input, "cargo test --all", "Tab prefix completion");
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
        assert!(!chat.busy, "Enter during a paste does not send");
        assert!(
            chat.input.starts_with("[Pasted text #1 +"),
            "placeholder: {}",
            chat.input
        );
        assert_eq!(chat.pastes.len(), 1);
        assert!(
            chat.expand_pastes(&chat.input).contains("line11"),
            "the real content expands on submit"
        );

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
        assert_eq!(chat.input, "", "Enter submits instead of a newline");
        assert_eq!(
            chat.queued,
            vec![QueuedInput {
                text: "hi".into(),
                is_slash: false
            }]
        );
    }

    /// Bracketed paste: the whole chunk inserts at the caret as one undo step; ≥10 lines fold into a placeholder,
    /// with the real content expanded at submit time. CR newlines (what terminals paste) are normalized first.
    #[test]
    fn bracketed_paste_inserts_and_collapses() {
        let mut chat = chat_with_history("paste-real");
        chat.set_input("ab");
        chat.cursor = 1;
        chat.on_paste("X");
        assert_eq!(chat.input, "aXb", "inserts at the cursor");
        assert_eq!(chat.cursor, 2);
        chat.undo_edit();
        assert_eq!(chat.input, "ab", "one paste = one undo step");

        // Short chunks do not fold (below the threshold).
        let mut chat = chat_with_history("paste-short");
        chat.on_paste("line1\nline2");
        assert_eq!(chat.input, "line1\nline2");
        assert!(chat.pastes.is_empty(), "below the threshold, no folding");

        // ≥ PASTE_COLLAPSE_LINES lines fold; CR and CRLF both count as newlines.
        let mut chat = chat_with_history("paste-fold");
        let body: String = (0..PASTE_COLLAPSE_LINES)
            .map(|i| format!("line{i}\r"))
            .collect();
        chat.on_paste(&body);
        assert!(
            chat.input.starts_with("[Pasted text #1 +"),
            "placeholder: {}",
            chat.input
        );
        assert_eq!(chat.cursor, chat.input.len());
        assert!(
            chat.expand_pastes(&chat.input).contains("line9"),
            "the real content expands on submit"
        );
        assert!(
            !chat.expand_pastes(&chat.input).contains('\r'),
            "CR is normalized"
        );

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
        chat.set_input(format!("take a look at this image\n{}", png.display()));
        chat.busy = true; // take the queue path: no tokio runtime needed
        chat.submit();
        assert_eq!(chat.queued.len(), 1);
        assert_eq!(
            chat.queued[0].text,
            format!("take a look at this image\n#[image 1]"),
            "the path line becomes a placeholder: {}",
            chat.queued[0].text
        );
        assert_eq!(chat.session.attachments.len(), 1);
        assert_eq!(
            chat.session.attachments.get(1).unwrap().media_type,
            "image/png"
        );
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
        chat.set_input(format!("![img]({})\n{}", png.display(), txt.display()));
        chat.busy = true;
        chat.submit();
        assert_eq!(
            chat.queued[0].text,
            format!("#[image 1]\n{}", txt.display())
        );
        assert_eq!(chat.session.attachments.len(), 1, "txt is not registered");
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
        let text = format!(
            "look at #[image {id1}] and #[image {id2}], then #[image {id1}] and #[image 99]"
        );
        let imgs = chat.resolve_images(&text);
        assert_eq!(imgs.len(), 2, "dedup + out-of-range ignored");
        assert_eq!(
            imgs[0].data,
            chat.session.attachments.get(id1).unwrap().data
        );
        assert_eq!(
            imgs[1].data,
            chat.session.attachments.get(id2).unwrap().data
        );
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
        assert!(chat.search.is_some(), "enters search mode");
        let line = chat.search_line().expect("search row");
        assert!(
            line.starts_with("(reverse-i-search)`': cargo build"),
            "{line}"
        );
        assert!(line.contains("enter submit"), "key hints visible: {line}");
        type_text(&mut chat, "cargo");
        assert_eq!(
            chat.search.as_ref().and_then(|s| s.hit.clone()).as_deref(),
            Some("cargo build")
        );
        assert!(ctrl(&mut chat, 'r'), "pressing again finds an older match");
        assert_eq!(
            chat.search.as_ref().and_then(|s| s.hit.clone()).as_deref(),
            Some("cargo test")
        );
        // In search mode, the input row shows the hit.
        assert_eq!(chat.prompt_lines()[0].plain_text(), "cargo test");
        press(&mut chat, KeyCode::Tab);
        assert!(chat.search.is_none(), "Tab accepts and exits search");
        assert_eq!(chat.input, "cargo test");

        // ctrl+c cancels: the input restores to its pre-search content.
        chat.set_input("keep");
        ctrl(&mut chat, 'r');
        ctrl(&mut chat, 'c');
        assert!(chat.search.is_none(), "ctrl+c exits search");
        assert_eq!(chat.input, "keep", "cancelling does not change the input");
        let _ = std::fs::remove_dir_all(&chat.session.home);
    }

    /// Alt+T thinking toggle: off ↔ the previous level.
    #[test]
    fn alt_t_toggles_thinking() {
        let mut chat = chat_with_history("think");
        let _ = chat
            .session
            .runtime
            .thinking_tx
            .send(Some("high".to_string()));
        alt(&mut chat, 't');
        assert_eq!(
            *chat.session.runtime.thinking.borrow(),
            None,
            "thinking turned off"
        );
        alt(&mut chat, 't');
        assert_eq!(
            chat.session.runtime.thinking.borrow().as_deref(),
            Some("high"),
            "restores the last level"
        );
    }

    /// Task area (CC glyphs): `☐`/`☒`, completed items dimmed + strikethrough semantics.
    #[test]
    fn task_lines_use_checkbox_glyphs() {
        let mut chat = chat_with_history("todo");
        chat.tasks_visible = true;
        chat.tasks_cache = vec![
            TodoItem {
                text: "done one".into(),
                status: TodoStatus::Done,
            },
            TodoItem {
                text: "doing".into(),
                status: TodoStatus::InProgress,
            },
            TodoItem {
                text: "later".into(),
                status: TodoStatus::Pending,
            },
        ];
        let lines = chat.task_lines();
        let joined: Vec<String> = lines.iter().map(|l| l.plain_text()).collect();
        assert!(joined[0].contains("todo · 1/3 tasks"), "{joined:?}");
        assert!(joined.iter().any(|l| l == "☒ done one"), "{joined:?}");
        assert!(joined.iter().any(|l| l == "☐ doing"), "{joined:?}");
        assert!(joined.iter().any(|l| l == "☐ later"), "{joined:?}");
        assert!(
            !joined
                .iter()
                .any(|l| l.contains("[x]") || l.contains("[ ]"))
        );
        let done_text = lines
            .iter()
            .find(|l| l.plain_text() == "☒ done one")
            .and_then(|l| l.segs.last())
            .expect("done seg");
        assert!(
            done_text.style.strikethrough,
            "done items carry strikethrough semantics"
        );
        assert_eq!(
            done_text.style.fg,
            Some(chat.theme.inactive),
            "and render dimmed"
        );
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
        assert_eq!(text, "x▋", "with input there is no placeholder");
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
            .expect("FX-04 is in the fixture list");
        fx.inject(&chat.events);
        chat.drain_events();
        let err = chat
            .last_error
            .as_ref()
            .expect("the error state was recorded");
        assert_eq!(err.code, "AUTH_REQUIRED");
        assert_eq!(err.level, ErrorLevel::Full);
        let frame = Frame::assemble(&chat, Size::new(80, 24));
        let joined: String = frame
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("something went wrong"),
            "fullscreen error-state title: {joined}"
        );
        assert!(
            joined.contains("code=AUTH_REQUIRED"),
            "stable code visible: {joined}"
        );
        assert!(joined.contains("retries"), "primary-action hint: {joined}");
        assert!(
            frame.cursor.is_none(),
            "the fullscreen state hides the input caret"
        );
        // Esc returns: not a dead end.
        chat.on_key(KeyCode::Esc, KeyModifiers::empty());
        assert!(chat.last_error.is_none(), "Esc back clears the error state");
    }

    /// #18 page-level error-row highlight: inject a Page-level fixture → the `[error]` row uses the error color
    /// (A zone; theme.error = (255,107,128) color baseline).
    #[test]
    fn page_error_row_is_highlighted_with_error_color() {
        use crate::error::ErrorLevel;
        use crate::tui::app::Frame;
        use crate::tui::test_util::{ErrorContext, error_fixtures};
        use ratatui::layout::Size;
        use ratatui::style::Color;
        let mut chat = test_chat();
        let fx = error_fixtures()
            .into_iter()
            .find(|f| f.code == "TIMEOUT" && f.context == ErrorContext::ShortSync)
            .expect("FX-01 is in the fixture list");
        fx.inject(&chat.events);
        chat.drain_events();
        assert_eq!(chat.last_error.as_ref().unwrap().level, ErrorLevel::Page);
        let frame = Frame::assemble(&chat, Size::new(80, 24));
        let error_row = frame
            .rows
            .iter()
            .find(|r| r.line.plain_text().starts_with("[error]"))
            .expect("the error row exists");
        assert!(
            error_row
                .line
                .segs
                .iter()
                .any(|s| s.style.fg == Some(Color::Rgb(255, 107, 128))),
            "the error row highlights with the error color (255,107,128): {:?}",
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
        chat.last_prompt = "why is the sky blue".into();
        let fx = error_fixtures()
            .into_iter()
            .find(|f| f.code == "PERMISSION_DENIED")
            .expect("FX-05 is in the fixture list");
        fx.inject(&chat.events);
        chat.drain_events();
        assert_eq!(chat.last_error.as_ref().unwrap().level, ErrorLevel::Full);
        chat.on_key(KeyCode::Enter, KeyModifiers::empty());
        assert!(chat.last_error.is_none(), "Enter clears the error state");
        assert!(chat.busy, "Enter retries and starts a new turn");
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
            .expect("FX-11 is in the fixture list");
        fx.inject(&chat.events);
        chat.drain_events();
        let err = chat
            .last_error
            .as_ref()
            .expect("the error state was recorded");
        assert_eq!(err.code, "TIMEOUT");
        assert_eq!(
            err.level,
            ErrorLevel::Full,
            "a long-turn TIMEOUT escalates to the full-flow level (AC-53)"
        );
        let frame = Frame::assemble(&chat, Size::new(80, 24));
        let joined: String = frame
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("code=TIMEOUT"),
            "the fullscreen state carries the stable code: {joined}"
        );
        assert!(
            joined.contains("retry") || joined.contains("back"),
            "AC-53 includes the \"retry or back\" path: {joined}"
        );
        assert!(
            frame.cursor.is_none(),
            "the fullscreen state hides the input caret"
        );
        // Same code, short sync (FX-01) → page-level error row, not full-screen — the two TIMEOUT levels are told apart by context.
        let mut short = test_chat();
        let fx_short = error_fixtures()
            .into_iter()
            .find(|f| f.code == "TIMEOUT" && f.context == ErrorContext::ShortSync)
            .expect("FX-01 is in the fixture list");
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
            "short-sync TIMEOUT = a page-level error row: {joined_short}"
        );
        assert!(
            !joined_short.contains("something went wrong"),
            "short-sync does not go fullscreen: {joined_short}"
        );
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
            let err = chat
                .last_error
                .as_ref()
                .expect("the error state was recorded");
            assert_eq!(err.code, fx.code, "error code recorded: {}", fx.code);
            assert_eq!(
                err.level, fx.level,
                "the level is carried explicitly by the producer (no copied mapping table): {}",
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
                    assert!(
                        joined.contains("something went wrong"),
                        "full-flow fullscreen-state title: {} / {joined}",
                        fx.code
                    );
                    assert!(
                        joined.contains(&format!("code={}", fx.code)),
                        "the fullscreen state carries the stable code: {} / {joined}",
                        fx.code
                    );
                    assert!(
                        frame.cursor.is_none(),
                        "the fullscreen state hides the caret: {}",
                        fx.code
                    );
                }
                ErrorLevel::Page | ErrorLevel::Field => {
                    assert!(
                        joined.contains(&format!("[error] code={}", fx.code)),
                        "page/field-level error rows carry the stable code: {} / {joined}",
                        fx.code
                    );
                    assert!(
                        !joined.contains("something went wrong"),
                        "page/field-level does not go fullscreen: {} / {joined}",
                        fx.code
                    );
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
            .expect("FX-01 is in the fixture list");
        fx.inject(&chat.events);
        chat.drain_events();
        let frame = Frame::assemble(&chat, Size::new(80, 24));
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        let area = buf.area;
        crate::tui::view::render_rows(&frame.rows, Color::White, &mut buf, area);
        let err_color = Color::Rgb(255, 107, 128);
        let has_err_color =
            (0..buf.area.height).any(|y| (0..buf.area.width).any(|x| buf[(x, y)].fg == err_color));
        assert!(
            has_err_color,
            "the error row really renders the error color (255,107,128) into the cell"
        );
        // Text anchor (assertions only anchor on the code).
        let joined: String = frame
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("[error] code=TIMEOUT"),
            "the error-row text carries the stable code: {joined}"
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
        // Exercise the real production fetch path (fork "default" — unknown names now error honestly instead of
        // silently falling back to the current endpoint, so that is no longer this test's path).
        chat.open_model_models(
            "default".into(),
            vec!["default".into()],
            vec![String::new()],
            0,
        );
        // Let the spawned task start and register its timeout timer first (under start_paused it only advances when polled).
        tokio::task::yield_now().await;
        // The 10s read timeout fires → emits UiEvent::Error (page-level).
        tokio::time::advance(std::time::Duration::from_secs(11)).await;
        tokio::task::yield_now().await; // let the spawned task finish sending the event
        chat.drain_events();
        let err = chat
            .last_error
            .as_ref()
            .expect("the production emitter recorded the error state");
        assert_eq!(
            err.code, "TIMEOUT",
            "a list_models read timeout lands on TIMEOUT"
        );
        assert_eq!(
            err.level,
            ErrorLevel::Page,
            "short-sync = page level (the real path)"
        );
        assert_eq!(err.context, ErrorContext::ShortSync, "context = short-sync");
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
            "the real-path error row is visible: {joined}"
        );
        assert!(
            !joined.contains("something went wrong"),
            "page level does not go fullscreen: {joined}"
        );
    }
    /// Info tier: /help output persists (no TTL) until the next input or Esc
    /// — the old 2s TTL burned it before anyone could read.
    #[test]
    fn info_output_persists_until_input_or_escape() {
        let mut chat = test_chat();
        chat.input = "/help".to_string();
        chat.submit();
        assert!(
            !chat.slash_info_lines.is_empty(),
            "/help lands in the info bucket"
        );
        chat.tick();
        assert!(
            !chat.slash_info_lines.is_empty(),
            "the tick does not clear info (no TTL)"
        );
        // Typing clears it (read then act).
        chat.on_key(KeyCode::Char('h'), KeyModifiers::empty());
        assert!(chat.slash_info_lines.is_empty(), "input clears info");

        chat.input = "/help".to_string();
        chat.submit();
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(chat.slash_info_lines.is_empty(), "Esc clears info");
    }

    /// Pinned panels survive ticks and render in the chrome until unpinned.
    #[test]
    fn pinned_panel_lives_until_unpinned() {
        let mut chat = test_chat();
        chat.pin_panel(
            "login",
            vec![
                "sign in to codex (device authorization)".to_string(),
                "  enter code ABCD-EFGH".to_string(),
            ],
        );
        chat.tick();
        assert_eq!(
            chat.pinned_panels.len(),
            1,
            "the tick does not clear pinned"
        );
        let rows = crate::tui::el::render(crate::tui::chrome::chrome(&chat, 80, false)).rows;
        let joined: String = rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("ABCD-EFGH"),
            "the panel is visible: {joined}"
        );
        chat.handle(UiEvent::Unpin {
            id: "login".to_string(),
        });
        assert!(chat.pinned_panels.is_empty(), "unpin makes it disappear");
    }

    /// Batch-2 invariant: switching providers resolves the model atomically —
    /// last-used per provider wins, the provider default fills in, and
    /// switching back restores what you used there.
    #[test]
    fn switch_provider_resolves_model_atomically() {
        let mut chat = test_chat();
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            ..Default::default()
        };
        settings.providers.insert(
            "deepseek".to_string(),
            crate::settings::ProviderConfig {
                api_key: Some("sk-ds".into()),
                api_base_url: "https://api.deepseek.com".into(),
                supports_images: None,
                protocol: None,
                oauth: None,
            },
        );
        Arc::get_mut(&mut chat.session).unwrap().client =
            crate::api::client::Client::from_settings_with(&settings, |_| {
                Err(std::env::VarError::NotPresent)
            })
            .unwrap();
        let _ = chat.session.runtime.model_tx.send("claude-sonnet-5".into());

        // An anthropic-protocol endpoint: default model fallback (claude-sonnet-5 is already in use → unchanged).
        chat.input = "/provider deepseek".to_string();
        chat.submit();
        assert_eq!(*chat.session.runtime.provider.borrow(), "deepseek");
        // Switch models mid-session → away and back restores the model that endpoint last used.
        chat.input = "/model deepseek-v4".to_string();
        chat.submit();
        chat.input = "/provider default".to_string();
        chat.submit();
        assert_eq!(
            *chat.session.runtime.model.borrow(),
            "claude-sonnet-5",
            "back to default restores its last model"
        );
        chat.input = "/provider deepseek".to_string();
        chat.submit();
        assert_eq!(
            *chat.session.runtime.model.borrow(),
            "deepseek-v4",
            "back to deepseek restores its last model"
        );
    }

    /// Mid-turn provider switches are refused: a cross-protocol swap would
    /// send this conversation's thinking blocks to the wrong endpoint.
    #[test]
    fn switch_provider_refuses_while_busy() {
        let mut chat = test_chat();
        chat.busy = true;
        chat.input = "/provider codex".to_string();
        chat.submit();
        assert_eq!(
            *chat.session.runtime.provider.borrow(),
            "default",
            "not switched"
        );
        assert!(
            chat.slash_error_lines.join("\n").contains("BUSY"),
            "{:?}",
            chat.slash_error_lines
        );
    }

    /// P0-9 regression: modifier chords in the Other input stay chords —
    /// ctrl+c must reach the global interrupt, not become a literal letter.
    #[test]
    fn ask_other_input_does_not_swallow_modifier_chords() {
        let mut chat = test_chat();
        let (tx, _rx) = tokio::sync::oneshot::channel();
        chat.pending_ask = Some((
            crate::ui::PermissionRequest {
                title: "choose".into(),
                question: "pick one".into(),
                options: vec!["A".into()],
                descriptions: vec![None],
                free_text: true,
            },
            tx,
        ));
        chat.ask_focus = 1; // the Other input slot
        assert!(chat.ask_key(KeyCode::Char('h'), KeyModifiers::empty()));
        assert_eq!(chat.ask_other, "h");
        assert!(
            !chat.ask_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            "ctrl+c is not swallowed by the dialog"
        );
        assert_eq!(
            chat.ask_other, "h",
            "modified keys do not leak into the input"
        );
    }

    /// P0-7 regression: a short-sync (Page-level) failure must not end the
    /// running turn — busy/stream stay untouched; only LongTurn errors reset.
    #[test]
    fn short_sync_error_keeps_the_running_turn() {
        let mut chat = test_chat();
        chat.busy = true;
        chat.stream_msg = Some(0);
        chat.handle(UiEvent::Error {
            code: "TIMEOUT",
            msg: "list_models timeout".into(),
            level: crate::error::ErrorLevel::Page,
            context: crate::error::ErrorContext::ShortSync,
        });
        assert!(
            chat.busy,
            "a short-sync failure does not interrupt the turn"
        );
        assert_eq!(chat.stream_msg, Some(0));
        assert!(chat.last_error.is_some(), "the error row is still recorded");

        chat.handle(UiEvent::Error {
            code: "TIMEOUT",
            msg: "turn died".into(),
            level: crate::error::ErrorLevel::Full,
            context: crate::error::ErrorContext::LongTurn,
        });
        assert!(!chat.busy, "a turn-level failure resets as usual");
    }

    /// Page/Field error rows dismiss with Esc instead of squatting above the
    /// prompt until the next turn.
    #[test]
    fn escape_dismisses_page_level_errors() {
        let mut chat = test_chat();
        chat.handle(UiEvent::Error {
            code: "TIMEOUT",
            msg: "x".into(),
            level: crate::error::ErrorLevel::Page,
            context: crate::error::ErrorContext::ShortSync,
        });
        assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
        assert!(chat.last_error.is_none(), "Esc clears a page-level error");
    }

    /// `/theme` with junk reports BAD_ARGUMENT instead of silently switching
    /// to auto with a success message (slash-command-ux G13).
    #[test]
    fn slash_theme_rejects_unknown_names() {
        let mut chat = test_chat();
        chat.input = "/theme bogus".to_string();
        chat.submit();
        let joined = all_slash_text(&chat);
        assert!(joined.contains("BAD_ARGUMENT"), "{joined}");
        assert!(!joined.contains("✓"), "no success receipt shown: {joined}");
    }

    /// P0-16 regression: a bash-mode turn clears interrupt suppression the
    /// same way a model turn does.
    #[tokio::test]
    async fn bash_turn_resets_interrupted() {
        let mut chat = test_chat();
        chat.interrupted = true;
        chat.bash_mode = true;
        chat.set_input("echo hi");
        chat.submit();
        assert!(
            !chat.interrupted,
            "! suppresses the interrupt on turn reset"
        );
    }

    /// The bottom notice expires with its window: an expired "press again"
    /// promise disappears instead of lying.
    #[test]
    fn notice_expires_after_its_window() {
        let mut chat = test_chat();
        chat.notice = Some("Press ctrl-c again to exit");
        chat.notice_until = Some(std::time::Instant::now() - std::time::Duration::from_millis(1));
        assert!(
            chat.needs_tick(),
            "with an expiring notice, idling is not allowed"
        );
        chat.tick();
        assert!(chat.notice.is_none(), "cleared once expired");
        assert!(chat.notice_until.is_none());
    }
}
